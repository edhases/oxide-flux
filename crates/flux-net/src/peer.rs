//! TCP peer-wire session: handshake, bidirectional message pump, keep-alive.
//!
//! One [`PeerHandle`] plus one spawned task is one connection. The task owns
//! the socket and does exactly three things: it runs the BEP 3 handshake, it
//! turns wire frames into [`PeerEvent`]s, and it turns [`PeerCommand`]s into
//! wire frames. It makes no decisions: no piece picking, no choking, no
//! request scheduling. Those belong to the swarm layer, which drives this
//! transport through the bounded command channel and reads the bounded event
//! channel.
//!
//! ```text
//!   swarm -- PeerCommand --> [ command channel ] --> session task --> socket
//!   swarm <-- PeerEvent ---- [ event channel ]   <-- session task <-- socket
//! ```
//!
//! ## Handshake
//!
//! [`connect`] dials, sends our handshake, then reads the peer's. [`accept`]
//! reads first, verifies, and only then replies, so a peer that asks for the
//! wrong torrent never gets an answer. Both directions fail with
//! [`PeerError::Handshake`] (before any event is produced and with the socket
//! already closed) when the peer's info hash is not [`PeerConfig::info_hash`].
//! We always advertise the extension protocol (BEP 10) and advertise DHT
//! (BEP 5) only when [`PeerConfig::advertise_dht`] is set.
//!
//! ## Defaults and limits
//!
//! | item | default | configured by |
//! | --- | --- | --- |
//! | frame body cap | [`DEFAULT_MAX_FRAME_LEN`] | [`PeerConfig::max_frame_len`] |
//! | keep-alive interval | 120 s | [`PeerConfig::keepalive`] |
//! | idle timeout | 150 s | [`PeerConfig::idle_timeout`] |
//! | write timeout | 30 s | [`PeerConfig::write_timeout`] |
//! | command queue | 64 | [`PeerConfig::command_buffer`] |
//! | event queue | 256 | [`PeerConfig::event_buffer`] |
//! | write buffer cap | 256 KiB | [`MAX_WRITE_BUFFER`] |
//! | read chunk | 16 KiB | private |
//!
//! A `Duration::ZERO` on any of the three timers disables that timer. The
//! handshake itself is bounded by `idle_timeout`; a peer that does not finish
//! it in time is refused with [`PeerError::HandshakeTimeout`].
//!
//! [`PeerConfig::pieces`] has no default: [`PeerConfig::new`] leaves it at 0,
//! which means "this torrent has no pieces", so a caller that forgets it will
//! see every bitfield rejected. It is the one field the transport cannot infer.
//!
//! ## Backpressure (bounded, on purpose)
//!
//! Every queue is bounded, and the only things ever dropped are a command the
//! caller could not legally encode and (see below) the terminal event:
//!
//! - **Commands.** [`PeerHandle::send`] awaits capacity; [`PeerHandle::try_send`]
//!   hands the command back. When the socket cannot drain, the session stops
//!   dequeuing commands once its own write buffer reaches [`MAX_WRITE_BUFFER`],
//!   the command channel fills, and producers block. Resident write memory is
//!   therefore at most `MAX_WRITE_BUFFER` plus one frame (a command is always
//!   accepted once the buffer is below the cap, even if it then overshoots).
//! - **Events.** When the event channel is full the session keeps the one event
//!   it was about to deliver and *stops reading the socket*. TCP's receive
//!   window then closes on the peer, which is the only honest way to apply
//!   backpressure to a flooding peer. Memory stays bounded by the event queue
//!   plus one read chunk plus one frame. While reads are stalled this way the
//!   idle timer is suspended: a consumer that is not draining events must not
//!   get its peers dropped for "silence" they were never given the chance to
//!   speak into.
//! - **Deliberately unbounded input is never buffered.** Frames larger than the
//!   cap are rejected from the 4-byte prefix alone by [`Decoder`].
//!
//! The terminal [`PeerEvent::Closed`] is best effort: if the event queue is
//! already full when the connection ends, the event may be dropped. The
//! reliable signal is the same in both cases: the event channel closes
//! (`recv` yields `None`) when the session task ends.
//!
//! ## Message mapping
//!
//! All ten BEP 3 messages are decoded. Three cases deserve spelling out:
//!
//! - **`extended` (wire id 20).** The BEP 3 codec does not know this message,
//!   but the session does: every `extended` frame is forwarded verbatim as
//!   [`PeerEvent::Extended`] with its extension id, whatever that id is. The
//!   session never negotiates or interprets extension ids; the metadata layer
//!   above needs the raw id for BEP 9/10 handshakes. Outbound frames are built
//!   the same way from [`PeerCommand::Extended`]. Unknown ids that are *not*
//!   20 are still a codec error, and a codec error closes the connection with
//!   [`CloseReason::Protocol`] rather than looping.
//! - **`bitfield`.** The wire form carries no piece count, so it is validated
//!   against [`PeerConfig::pieces`] on the way in. A bitfield of the wrong
//!   length, or one with trailing bits set, is a protocol error, not an
//!   approximated event.
//! - **`port` (DHT listen port).** Ignored: there is no DHT in this client, so
//!   there is no event that could carry it honestly. It is the one BEP 3
//!   message with no [`PeerEvent`] counterpart.
//!
//! ## Time is the runtime's
//!
//! The session reads no system clock and does no blocking work: all waiting is
//! `tokio::time` and all I/O is non-blocking on the reactor. That is what lets
//! the timeout tests below drive the whole connection with
//! `tokio::time::pause` / `advance` instead of sleeping.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc};
use tokio::time::{Instant, sleep_until, timeout};

use crate::bitfield::{Bitfield, BitfieldError};
use crate::handshake::{HANDSHAKE_LEN, Handshake, HandshakeError, InfoHash, PeerId};
use crate::message::{
    BitfieldMessage, DEFAULT_MAX_FRAME_LEN, DecodeError, Decoder, EncodeError, Message, encode_into,
};

/// The BEP 10 extension-protocol message id (`extended`).
pub const EXTENDED_ID: u8 = 20;

/// Default keep-alive interval: 120 s.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(120);
/// Default idle timeout: 150 s, i.e. longer than one keep-alive interval.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(150);
/// Default write timeout: 30 s.
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Default command queue capacity.
pub const DEFAULT_COMMAND_BUFFER: usize = 64;
/// Default event queue capacity.
pub const DEFAULT_EVENT_BUFFER: usize = 256;

/// Hard cap on bytes buffered for writing before commands stop being dequeued.
///
/// The buffer may overshoot once, by at most one whole frame, because a command
/// is accepted as soon as the buffer is below the cap. This is the bound the
/// "memory stays flat" promise rests on.
pub const MAX_WRITE_BUFFER: usize = 256 * 1024;

/// Bytes read from the socket per reactor wake-up.
const READ_CHUNK: usize = 16 * 1024;

/// Everything one connection needs that the swarm layer owns.
///
/// [`PeerConfig::new`] fills in the defaults from the module table; set
/// [`PeerConfig::pieces`] too, or every inbound bitfield will be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerConfig {
    /// The torrent's v1 info hash: ours in the handshake, and the only hash we
    /// will talk to a peer about.
    pub info_hash: InfoHash,
    /// Our peer id.
    pub peer_id: PeerId,
    /// Set the DHT reserved bit (BEP 5) in our handshake.
    pub advertise_dht: bool,
    /// Largest frame body accepted from the peer; see
    /// [`Decoder::with_max_frame_len`].
    pub max_frame_len: usize,
    /// Send a keep-alive when no command has been written for this long.
    /// `Duration::ZERO` disables keep-alives.
    pub keepalive: Duration,
    /// Drop the peer after this long without receiving anything from it.
    /// `Duration::ZERO` disables the idle timeout.
    pub idle_timeout: Duration,
    /// Drop the peer when its socket accepts no bytes for this long while
    /// outbound data is still queued. `Duration::ZERO` disables the timeout.
    pub write_timeout: Duration,
    /// Command queue capacity; at least 1 is used.
    pub command_buffer: usize,
    /// Event queue capacity; at least 1 is used.
    pub event_buffer: usize,
    /// Number of pieces in the torrent.
    ///
    /// A `bitfield` message carries only bits, never the piece count, so the
    /// session cannot validate one without this. It is the torrent's piece
    /// count, not a policy knob: an inbound bitfield whose byte length is not
    /// `ceil(pieces / 8)`, or whose spare trailing bits are set, closes the
    /// connection with [`CloseReason::Protocol`].
    /// Number of pieces in the torrent.
    ///
    /// `0` means "unknown" and is only for metadata fetches: bitfields are then
    /// acknowledged but not decoded. Every real connection must set this from the
    /// metainfo.
    pub pieces: usize,
}

impl PeerConfig {
    /// Builds a config for one torrent, with every timeout and queue at its
    /// default and DHT advertisement off.
    ///
    /// [`PeerConfig::pieces`] is left at 0, so callers must set it from the
    /// metainfo before connecting.
    #[must_use]
    pub fn new(info_hash: InfoHash, peer_id: PeerId) -> Self {
        Self {
            info_hash,
            peer_id,
            advertise_dht: false,
            max_frame_len: DEFAULT_MAX_FRAME_LEN,
            keepalive: DEFAULT_KEEPALIVE,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
            command_buffer: DEFAULT_COMMAND_BUFFER,
            event_buffer: DEFAULT_EVENT_BUFFER,
            pieces: 0,
        }
    }
}

/// A frame the session should put on the wire.
///
/// `Close` is a control command, not a message: it ends the session instead of
/// being encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerCommand {
    /// Piece availability bitmap, sent as a BEP 3 `bitfield`.
    Bitfield(Bitfield),
    /// Announce one piece, sent as `have`.
    Have(u32),
    /// We want pieces from the peer.
    Interested,
    /// We no longer want pieces from the peer.
    Uninterested,
    /// Stop requesting.
    Choke,
    /// Requests may resume.
    Unchoke,
    /// Ask for a block.
    Request {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block length in bytes.
        length: u32,
    },
    /// Withdraw a request.
    Cancel {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block length in bytes.
        length: u32,
    },
    /// A block of piece data, sent as `piece`. The data must not be empty.
    Block {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block bytes.
        data: Vec<u8>,
    },
    /// A BEP 10 extension message: wire id 20, then `id`, then `payload`.
    ///
    /// The session does not negotiate or interpret extension ids; it passes
    /// them through so the metadata layer can speak BEP 9/10 itself.
    Extended {
        /// Extension message id.
        id: u8,
        /// Raw payload.
        payload: Vec<u8>,
    },
    /// Send a zero-length frame.
    KeepAlive,
    /// Close the connection once the commands already queued have been dealt
    /// with.
    ///
    /// This is an ordered close, not a guarantee that every byte reached the
    /// peer: whatever is still buffered gets one final write attempt. Use
    /// [`PeerHandle::close`] to drop the connection immediately instead.
    Close,
}

/// Something the peer did, already decoded.
///
/// One variant per BEP 3 message the transport can carry, plus the raw
/// extension form. `port` messages have no counterpart (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// The peer will not serve us.
    Choke,
    /// The peer will serve us.
    Unchoke,
    /// The peer wants pieces from us.
    Interested,
    /// The peer no longer wants pieces from us.
    Uninterested,
    /// The peer now has `index`.
    Have(u32),
    /// The peer's availability, validated against [`PeerConfig::pieces`].
    Bitfield(Bitfield),
    /// The peer asks for a block.
    Request {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block length in bytes.
        length: u32,
    },
    /// The peer sent a block.
    Block {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block bytes.
        data: Vec<u8>,
    },
    /// The peer withdrew a request.
    Cancel {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block length in bytes.
        length: u32,
    },
    /// A BEP 10 extension message, forwarded verbatim.
    Extended {
        /// Extension message id, exactly as it arrived.
        id: u8,
        /// Raw payload.
        payload: Vec<u8>,
    },
    /// The connection ended. Always the last event when it can be delivered.
    Closed(CloseReason),
}

/// Why a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The peer closed its end (a zero-length read).
    RemoteClosed,
    /// We closed: [`PeerCommand::Close`], [`PeerHandle::close`], or the last
    /// handle was dropped.
    LocalClose,
    /// Nothing arrived within [`PeerConfig::idle_timeout`].
    IdleTimeout,
    /// The peer broke the wire protocol; the socket is closed, not resynced.
    Protocol(ProtocolError),
    /// The transport failed, or outbound data stalled past
    /// [`PeerConfig::write_timeout`].
    Io(io::ErrorKind),
}

/// The protocol violations that end a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProtocolError {
    /// The frame codec rejected the peer's bytes.
    #[error(transparent)]
    Decode(#[from] DecodeError),
    /// The peer's bitfield does not describe [`PeerConfig::pieces`] pieces.
    #[error(transparent)]
    Bitfield(#[from] BitfieldError),
}

/// Failures from [`connect`], [`accept`], and [`PeerHandle::send`].
#[derive(Debug, Error)]
pub enum PeerError {
    /// The socket failed, or the handshake write timed out.
    #[error("peer io error: {0}")]
    Io(#[from] io::Error),

    /// The peer's handshake was not ours to accept.
    #[error("peer handshake rejected: {0}")]
    Handshake(#[from] HandshakeError),

    /// The peer did not finish its handshake within the idle timeout.
    #[error("peer handshake did not complete within {timeout:?}")]
    HandshakeTimeout {
        /// The bound that expired.
        timeout: Duration,
    },

    /// The session is gone: the peer disconnected, we closed, or the task
    /// ended. No further command can be delivered.
    #[error("peer session is closed")]
    Closed,

    /// The command cannot be encoded, so it was not sent.
    #[error("command cannot be encoded: {0}")]
    Encode(#[from] EncodeError),

    /// A `piece` frame with no block bytes cannot be encoded: the codec rejects
    /// such a frame on receipt.
    #[error("a piece block cannot be empty")]
    EmptyBlock,
}

/// The sending half of a session: one handle per connection.
///
/// Dropping the handle closes the connection (the session task sees its
/// command channel end).
#[derive(Debug)]
pub struct PeerHandle {
    commands: mpsc::Sender<PeerCommand>,
    notify: Arc<Notify>,
    addr: SocketAddr,
    peer_id: PeerId,
    reserved: [u8; 8],
    max_frame_len: usize,
}

impl PeerHandle {
    /// The remote address: the dialled address for [`connect`], the accepted
    /// peer for [`accept`].
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The remote peer id, as it appeared in the peer's handshake.
    #[must_use]
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// The remote reserved field, as it appeared in the peer's handshake.
    #[must_use]
    pub fn reserved(&self) -> [u8; 8] {
        self.reserved
    }

    /// Queues a command, waiting for capacity.
    ///
    /// Waits while the queue is full and returns [`PeerError::Closed`] once the
    /// session has ended. Commands that cannot be encoded (an empty block, a
    /// block over the frame cap) are refused here, before they reach the queue.
    pub async fn send(&self, command: PeerCommand) -> Result<(), PeerError> {
        validate_command(&command, self.max_frame_len)?;
        self.commands
            .send(command)
            .await
            .map_err(|_| PeerError::Closed)
    }

    /// Queues a command only if there is room right now.
    ///
    /// The command comes back untouched when it was not queued: the queue is
    /// full, the session is closed, or the command cannot be encoded.
    pub fn try_send(&self, command: PeerCommand) -> Result<(), PeerCommand> {
        if validate_command(&command, self.max_frame_len).is_err() {
            return Err(command);
        }
        self.commands.try_send(command).map_err(|err| match err {
            mpsc::error::TrySendError::Full(command)
            | mpsc::error::TrySendError::Closed(command) => command,
        })
    }

    /// Closes the connection now, without waiting for queued commands.
    ///
    /// Idempotent. Use [`PeerCommand::Close`] for an ordered close.
    pub fn close(&self) {
        self.notify.notify_one();
    }
}

/// Dials `addr`, runs the outbound handshake, and starts the session.
///
/// Fails (with no event and with the socket closed) when the peer answers
/// for a different torrent.
pub async fn connect(
    addr: SocketAddr,
    config: PeerConfig,
) -> Result<(PeerHandle, mpsc::Receiver<PeerEvent>), PeerError> {
    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
    let ours = local_handshake(&config);
    write_handshake(&stream, &ours, config.write_timeout).await?;
    let remote = read_handshake(&stream, config.info_hash, config.idle_timeout).await?;
    Ok(start(stream, config, &remote, addr))
}

/// Runs the inbound handshake on an accepted socket and starts the session.
///
/// The peer's info hash is verified before we reply, so a peer asking for
/// another torrent gets a closed socket and no answer.
pub async fn accept(
    stream: TcpStream,
    config: PeerConfig,
) -> Result<(PeerHandle, mpsc::Receiver<PeerEvent>), PeerError> {
    stream.set_nodelay(true)?;
    let addr = stream.peer_addr()?;
    let remote = read_handshake(&stream, config.info_hash, config.idle_timeout).await?;
    let ours = local_handshake(&config);
    write_handshake(&stream, &ours, config.write_timeout).await?;
    Ok(start(stream, config, &remote, addr))
}

/// Like [`accept`], but the caller already read the handshake bytes.
///
/// A listener needs the info hash to decide which torrent a connection belongs
/// to, so it reads the 68 handshake bytes once and routes on them. `prefix` must
/// be exactly that handshake; anything the peer pipelined after it must still be
/// in the socket, not in `prefix`.
pub async fn accept_prefixed(
    stream: TcpStream,
    prefix: &[u8],
    config: PeerConfig,
) -> Result<(PeerHandle, mpsc::Receiver<PeerEvent>), PeerError> {
    if prefix.len() != HANDSHAKE_LEN {
        return Err(PeerError::Handshake(HandshakeError::Truncated {
            got: prefix.len(),
            need: HANDSHAKE_LEN,
        }));
    }
    stream.set_nodelay(true)?;
    let addr = stream.peer_addr()?;
    let remote = Handshake::decode_expecting(prefix, config.info_hash).map_err(PeerError::from)?;
    let ours = local_handshake(&config);
    write_handshake(&stream, &ours, config.write_timeout).await?;
    Ok(start(stream, config, &remote, addr))
}

/// Builds the handshake we send: extension protocol always, DHT on request.
fn local_handshake(config: &PeerConfig) -> Handshake {
    let mut handshake = Handshake::new(config.info_hash, config.peer_id);
    handshake.set_extended(true);
    handshake.set_dht(config.advertise_dht);
    handshake
}

/// Creates the channels, the handle, and the session task.
fn start(
    stream: TcpStream,
    config: PeerConfig,
    remote: &Handshake,
    addr: SocketAddr,
) -> (PeerHandle, mpsc::Receiver<PeerEvent>) {
    let (command_tx, command_rx) = mpsc::channel(config.command_buffer.max(1));
    let (event_tx, event_rx) = mpsc::channel(config.event_buffer.max(1));
    let notify = Arc::new(Notify::new());
    let max_frame_len = config.max_frame_len.min(u32::MAX as usize);

    let handle = PeerHandle {
        commands: command_tx,
        notify: Arc::clone(&notify),
        addr,
        peer_id: *remote.peer_id(),
        reserved: remote.reserved(),
        max_frame_len,
    };

    let session = Session {
        config,
        stream,
        frames: FrameReader::new(max_frame_len),
        commands: command_rx,
        events: event_tx,
        notify,
    };
    tokio::spawn(session.run());

    (handle, event_rx)
}

/// Reads exactly one handshake and checks it against `expected`.
async fn read_handshake(
    stream: &TcpStream,
    expected: InfoHash,
    idle_timeout: Duration,
) -> Result<Handshake, PeerError> {
    let read = async {
        let mut bytes = [0u8; HANDSHAKE_LEN];
        let mut filled = 0;
        while filled < HANDSHAKE_LEN {
            stream.readable().await?;
            match stream.try_read(&mut bytes[filled..]) {
                Ok(0) => {
                    return Err(PeerError::Io(io::Error::from(io::ErrorKind::UnexpectedEof)));
                }
                Ok(n) => filled += n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(PeerError::Io(err)),
            }
        }
        Handshake::decode_expecting(&bytes, expected).map_err(PeerError::from)
    };

    if idle_timeout.is_zero() {
        read.await
    } else {
        match timeout(idle_timeout, read).await {
            Ok(result) => result,
            Err(_) => Err(PeerError::HandshakeTimeout {
                timeout: idle_timeout,
            }),
        }
    }
}

/// Writes our handshake, bounded by the write timeout.
async fn write_handshake(
    stream: &TcpStream,
    handshake: &Handshake,
    write_timeout: Duration,
) -> Result<(), PeerError> {
    let bytes = handshake.encode();
    let write = write_all(stream, &bytes);
    if write_timeout.is_zero() {
        write.await
    } else {
        timeout(write_timeout, write)
            .await
            .map_err(|_| PeerError::Io(io::Error::from(io::ErrorKind::TimedOut)))?
    }
}

/// Writes every byte, waiting for the socket to be writable between chunks.
async fn write_all(stream: &TcpStream, bytes: &[u8]) -> Result<(), PeerError> {
    let mut written = 0;
    while written < bytes.len() {
        stream.writable().await?;
        match stream.try_write(&bytes[written..]) {
            Ok(0) => return Err(PeerError::Io(io::Error::from(io::ErrorKind::WriteZero))),
            Ok(n) => written += n,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(PeerError::Io(err)),
        }
    }
    Ok(())
}

/// The task that owns the socket for the life of one connection.
struct Session {
    config: PeerConfig,
    stream: TcpStream,
    frames: FrameReader,
    commands: mpsc::Receiver<PeerCommand>,
    events: mpsc::Sender<PeerEvent>,
    notify: Arc<Notify>,
}

impl Session {
    /// Pumps until the connection ends, then publishes the reason and closes.
    async fn run(self) {
        let Self {
            config,
            stream,
            mut frames,
            mut commands,
            events,
            notify,
        } = self;

        let pieces = config.pieces;
        let max_frame_len = config.max_frame_len.min(u32::MAX as usize);
        let mut out: Vec<u8> = Vec::new();
        let mut out_head = 0usize;
        let mut pending: Option<PeerEvent> = None;
        let mut scratch = vec![0u8; READ_CHUNK];
        let started = Instant::now();
        let mut last_read = started;
        let mut last_write = started;

        let reason = 'session: loop {
            // Drain every frame already buffered before waiting on the socket.
            // One event per turn is not enough: a burst of requests/haves can
            // arrive in a single read, and the socket may stay silent afterwards
            // (that used to stall the swarm until unrelated traffic arrived).
            if pending.is_none() {
                loop {
                    match next_event(&mut frames, pieces) {
                        Ok(Some(event)) => match events.try_send(event) {
                            Ok(()) => continue,
                            Err(mpsc::error::TrySendError::Full(event)) => {
                                pending = Some(event);
                                break;
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                break 'session CloseReason::LocalClose;
                            }
                        },
                        Ok(None) => break,
                        Err(err) => break 'session CloseReason::Protocol(err),
                    }
                }
            }

            // Hand it over if there is room right now, otherwise keep it and
            // let the `reserve` branch wait for the consumer.
            if let Some(event) = pending.take() {
                match events.try_send(event) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(event)) => pending = Some(event),
                    Err(mpsc::error::TrySendError::Closed(_)) => break CloseReason::LocalClose,
                }
            }

            // Write whatever the socket will take without waiting.
            if out_head < out.len() {
                match flush(&stream, &mut out, &mut out_head) {
                    Ok(written) => {
                        if written > 0 {
                            last_write = Instant::now();
                        }
                    }
                    Err(kind) => break CloseReason::Io(kind),
                }
            }

            let writing = out_head < out.len();
            let blocked = pending.is_some();
            let keepalive_at = last_write + config.keepalive;
            let idle_at = last_read + config.idle_timeout;
            let write_at = last_write + config.write_timeout;
            let accept_commands = !writing || (out.len() - out_head) < MAX_WRITE_BUFFER;

            tokio::select! {
                // Any close request wins immediately.
                _ = notify.notified() => break CloseReason::LocalClose,

                command = commands.recv(), if accept_commands => match command {
                    None => break CloseReason::LocalClose,
                    Some(PeerCommand::Close) => {
                        // One last attempt at what is already buffered.
                        let _ = flush(&stream, &mut out, &mut out_head);
                        break CloseReason::LocalClose;
                    }
                    Some(command) => {
                        // A command that cannot be encoded is a caller bug,
                        // not a peer fault: drop it and keep the link.
                        if queue_command(command, max_frame_len, &mut out).is_ok() {
                            last_write = Instant::now();
                        }
                    }
                },

                // The event channel is full: wait for room instead of reading
                // more from the peer. This is the backpressure that stops a
                // flooding peer at the TCP window.
                permit = events.reserve(), if blocked => match permit {
                    Ok(permit) => {
                        if let Some(event) = pending.take() {
                            permit.send(event);
                        }
                    }
                    Err(_) => break CloseReason::LocalClose,
                },

                // Reads are suspended while an event is waiting for the
                // consumer: that is the backpressure.
                ready = stream.readable(), if !blocked => match ready {
                    Ok(()) => match stream.try_read(&mut scratch) {
                        Ok(0) => break CloseReason::RemoteClosed,
                        Ok(n) => {
                            frames.feed(&scratch[..n]);
                            last_read = Instant::now();
                        }
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                        Err(err) => break CloseReason::Io(err.kind()),
                    },
                    Err(err) => break CloseReason::Io(err.kind()),
                },

                ready = stream.writable(), if writing => match ready {
                    Ok(()) => match flush(&stream, &mut out, &mut out_head) {
                        Ok(written) => {
                            if written > 0 {
                                last_write = Instant::now();
                            }
                        }
                        Err(kind) => break CloseReason::Io(kind),
                    },
                    Err(err) => break CloseReason::Io(err.kind()),
                },

                () = sleep_until(write_at), if writing && !config.write_timeout.is_zero() => {
                    break CloseReason::Io(io::ErrorKind::TimedOut);
                }

                // Only when nothing is queued: a peer with data on the way
                // does not need a keep-alive.
                () = sleep_until(keepalive_at), if !writing && !config.keepalive.is_zero() => {
                    if queue_command(PeerCommand::KeepAlive, max_frame_len, &mut out).is_ok() {
                        last_write = Instant::now();
                    }
                }

                () = sleep_until(idle_at), if !blocked && !config.idle_timeout.is_zero() => {
                    break CloseReason::IdleTimeout;
                }
            }
        };

        // Close the socket first so the peer sees EOF promptly, then make
        // every future send fail (which is what makes "closed" observable),
        // then publish the reason.
        drop(stream);
        commands.close();
        let _ = events.try_send(PeerEvent::Closed(reason));
    }
}

/// The incremental frame splitter.
///
/// The BEP 3 codec decodes frames but has no way to pass an id-20 frame
/// through, because `extended` is not a BEP 3 message. So this reader owns the
/// 4-byte framing the codec would otherwise own: it strips a complete frame
/// out of the stream, hands `extended` bodies to the peer layer, and hands
/// every other frame (length prefix included) to [`Decoder`], which remains
/// the only place BEP 3 bodies are parsed. The cap check happens on the prefix,
/// before any body is buffered, exactly as in the codec.
struct FrameReader {
    buf: Vec<u8>,
    head: usize,
    decoder: Decoder,
    max_frame_len: usize,
}

impl FrameReader {
    fn new(max_frame_len: usize) -> Self {
        Self {
            buf: Vec::new(),
            head: 0,
            decoder: Decoder::with_max_frame_len(max_frame_len),
            max_frame_len,
        }
    }

    /// Appends bytes read from the socket.
    fn feed(&mut self, bytes: &[u8]) {
        if self.head > 0 {
            self.buf.drain(..self.head);
            self.head = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete frame, or `Ok(None)` if more bytes are needed.
    fn next_frame(&mut self) -> Result<Option<Inbound>, DecodeError> {
        let Some(prefix) = self.buf.get(self.head..self.head + 4) else {
            return Ok(None);
        };
        let Ok(prefix) = <[u8; 4]>::try_from(prefix) else {
            return Ok(None);
        };
        let declared = u32::from_be_bytes(prefix) as usize;

        if declared > self.max_frame_len {
            return Err(DecodeError::FrameTooLarge {
                declared,
                max: self.max_frame_len,
            });
        }
        let end = self.head + 4 + declared;
        if end > self.buf.len() {
            return Ok(None);
        }

        let start = self.head + 4;
        let Some(body) = self.buf.get(start..end) else {
            return Ok(None);
        };

        let inbound = if body.first() == Some(&EXTENDED_ID) {
            // Wire id 20: strip the extension id and forward the payload.
            let Some((&id, payload)) = body.get(1..).and_then(|rest| rest.split_first()) else {
                return Err(DecodeError::MissingFields {
                    id: EXTENDED_ID,
                    length: body.len(),
                });
            };
            Inbound::Extended {
                id,
                payload: payload.to_vec(),
            }
        } else {
            // Everything else is the codec's job, prefix included.
            if let Some(frame) = self.buf.get(self.head..end) {
                self.decoder.feed(frame);
            }
            // A complete frame was just fed, so this cannot need more bytes.
            let Some(message) = self.decoder.next_frame()? else {
                return Err(DecodeError::MissingMessageId);
            };
            Inbound::Message(message)
        };

        self.head = end;
        if self.head == self.buf.len() {
            self.buf.clear();
            self.head = 0;
        }
        Ok(Some(inbound))
    }
}

/// One frame as the peer layer sees it.
enum Inbound {
    /// A BEP 3 message, decoded by the codec.
    Message(Message),
    /// A BEP 10 message: extension id plus untouched payload.
    Extended { id: u8, payload: Vec<u8> },
}

/// Pulls frames until one turns into a real event.
///
/// `keep-alive` and `port` frames produce no event; everything else produces
/// exactly one.
fn next_event(frames: &mut FrameReader, pieces: usize) -> Result<Option<PeerEvent>, ProtocolError> {
    loop {
        let Some(frame) = frames.next_frame()? else {
            return Ok(None);
        };
        let event = match frame {
            Inbound::Extended { id, payload } => Some(PeerEvent::Extended { id, payload }),
            Inbound::Message(Message::KeepAlive | Message::Port(_)) => None,
            Inbound::Message(Message::Choke) => Some(PeerEvent::Choke),
            Inbound::Message(Message::Unchoke) => Some(PeerEvent::Unchoke),
            Inbound::Message(Message::Interested) => Some(PeerEvent::Interested),
            Inbound::Message(Message::NotInterested) => Some(PeerEvent::Uninterested),
            Inbound::Message(Message::Have { index }) => Some(PeerEvent::Have(index)),
            Inbound::Message(Message::Bitfield(raw)) => {
                if pieces == 0 {
                    // The caller does not know the torrent shape yet (metadata
                    // fetch). Acknowledge the bitfield without decoding it.
                    Some(PeerEvent::Bitfield(Bitfield::new(0)))
                } else {
                    Some(PeerEvent::Bitfield(raw.into_bitfield(pieces)?))
                }
            }
            Inbound::Message(Message::Request {
                index,
                begin,
                length,
            }) => Some(PeerEvent::Request {
                index,
                begin,
                length,
            }),
            Inbound::Message(Message::Piece {
                index,
                begin,
                block,
            }) => Some(PeerEvent::Block {
                index,
                begin,
                data: block,
            }),
            Inbound::Message(Message::Cancel {
                index,
                begin,
                length,
            }) => Some(PeerEvent::Cancel {
                index,
                begin,
                length,
            }),
        };
        if let Some(event) = event {
            return Ok(Some(event));
        }
    }
}

/// Writes one frame into `out`; leaves `out` untouched on failure.
fn queue_command(
    command: PeerCommand,
    max_frame_len: usize,
    out: &mut Vec<u8>,
) -> Result<(), PeerError> {
    validate_command(&command, max_frame_len)?;
    match command {
        // Handled by the session loop before it gets here.
        PeerCommand::Close => {}
        PeerCommand::KeepAlive => encode_into(&Message::KeepAlive, max_frame_len, out)?,
        PeerCommand::Choke => encode_into(&Message::Choke, max_frame_len, out)?,
        PeerCommand::Unchoke => encode_into(&Message::Unchoke, max_frame_len, out)?,
        PeerCommand::Interested => encode_into(&Message::Interested, max_frame_len, out)?,
        PeerCommand::Uninterested => encode_into(&Message::NotInterested, max_frame_len, out)?,
        PeerCommand::Have(index) => encode_into(&Message::Have { index }, max_frame_len, out)?,
        PeerCommand::Bitfield(field) => encode_into(
            &Message::Bitfield(BitfieldMessage::from(field)),
            max_frame_len,
            out,
        )?,
        PeerCommand::Request {
            index,
            begin,
            length,
        } => encode_into(
            &Message::Request {
                index,
                begin,
                length,
            },
            max_frame_len,
            out,
        )?,
        PeerCommand::Cancel {
            index,
            begin,
            length,
        } => encode_into(
            &Message::Cancel {
                index,
                begin,
                length,
            },
            max_frame_len,
            out,
        )?,
        PeerCommand::Block { index, begin, data } => encode_into(
            &Message::Piece {
                index,
                begin,
                block: data,
            },
            max_frame_len,
            out,
        )?,
        PeerCommand::Extended { id, payload } => {
            push_extended(id, &payload, max_frame_len, out)?;
        }
    }
    Ok(())
}

/// Encodes `wire id 20 + extension id + payload` by hand; the BEP 3 codec has
/// no id-20 message.
fn push_extended(
    id: u8,
    payload: &[u8],
    max_frame_len: usize,
    out: &mut Vec<u8>,
) -> Result<(), PeerError> {
    let body_len = 2 + payload.len();
    let too_large = || EncodeError::FrameTooLarge {
        length: body_len,
        max: max_frame_len,
    };
    if body_len > max_frame_len {
        return Err(too_large().into());
    }
    let prefix = u32::try_from(body_len).map_err(|_| too_large())?;
    out.reserve(4 + body_len);
    out.extend_from_slice(&prefix.to_be_bytes());
    out.push(EXTENDED_ID);
    out.push(id);
    out.extend_from_slice(payload);
    Ok(())
}

/// The body length of the frame a command would produce (`None` for `Close`).
fn command_body_len(command: &PeerCommand) -> Option<usize> {
    match command {
        PeerCommand::Close => None,
        PeerCommand::KeepAlive => Some(0),
        PeerCommand::Choke
        | PeerCommand::Unchoke
        | PeerCommand::Interested
        | PeerCommand::Uninterested => Some(1),
        PeerCommand::Have(_) => Some(5),
        PeerCommand::Bitfield(field) => Some(1 + field.to_bytes().len()),
        PeerCommand::Request { .. } | PeerCommand::Cancel { .. } => Some(13),
        PeerCommand::Block { data, .. } => Some(9 + data.len()),
        PeerCommand::Extended { payload, .. } => Some(2 + payload.len()),
    }
}

/// Refuses commands that cannot become a legal frame.
fn validate_command(
    command: &PeerCommand,
    max_frame_len: usize,
) -> Result<Option<usize>, PeerError> {
    let Some(body_len) = command_body_len(command) else {
        return Ok(None);
    };
    if matches!(command, PeerCommand::Block { data, .. } if data.is_empty()) {
        return Err(PeerError::EmptyBlock);
    }
    if body_len > max_frame_len {
        return Err(EncodeError::FrameTooLarge {
            length: body_len,
            max: max_frame_len,
        }
        .into());
    }
    Ok(Some(body_len))
}

/// Writes as much of `out` as the socket takes right now; `Ok(0)` means the
/// socket would block. The returned count is the bytes actually written.
fn flush(stream: &TcpStream, out: &mut Vec<u8>, head: &mut usize) -> Result<usize, io::ErrorKind> {
    if *head >= out.len() {
        out.clear();
        *head = 0;
        return Ok(0);
    }
    match stream.try_write(&out[*head..]) {
        Ok(0) => Err(io::ErrorKind::WriteZero),
        Ok(n) => {
            *head += n;
            if *head == out.len() {
                out.clear();
                *head = 0;
            }
            Ok(n)
        }
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(0),
        Err(err) => Err(err.kind()),
    }
}

impl fmt::Debug for FrameReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameReader")
            .field("buffered", &(self.buf.len() - self.head))
            .field("max_frame_len", &self.max_frame_len)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::net::Ipv4Addr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::{advance, pause};

    use super::*;
    use crate::handshake::{RESERVED_DHT, RESERVED_EXTENDED, ReservedBit};
    use crate::message::{BLOCK_SIZE, MAX_BLOCK_SIZE, encode};

    const HASH: InfoHash = [0x11; 20];
    const OTHER_HASH: InfoHash = [0x77; 20];
    const SERVER_ID: PeerId = [0x51; 20];
    const CLIENT_ID: PeerId = [0x52; 20];

    /// A config for the shared test torrent: 10 pieces, so a bitfield is two
    /// bytes and can be checked against the piece count.
    fn config(peer_id: PeerId) -> PeerConfig {
        let mut config = PeerConfig::new(HASH, peer_id);
        config.pieces = 10;
        config
    }

    async fn listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        (listener, addr)
    }

    /// Two real sessions, fully handshaken.
    async fn pair() -> (
        PeerHandle,
        mpsc::Receiver<PeerEvent>,
        PeerHandle,
        mpsc::Receiver<PeerEvent>,
    ) {
        let (listener, addr) = listener().await;
        let server_config = config(SERVER_ID);
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept socket");
            accept(stream, server_config)
                .await
                .expect("server handshake")
        });
        let (client, client_events) = connect(addr, config(CLIENT_ID))
            .await
            .expect("client handshake");
        let (server, server_events) = task.await.expect("server task");
        (client, client_events, server, server_events)
    }

    /// One real session plus a raw socket driven by the test, after handshake.
    async fn raw_actor(config: PeerConfig) -> (PeerHandle, mpsc::Receiver<PeerEvent>, TcpStream) {
        let (listener, addr) = listener().await;
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept socket");
            accept(stream, config).await.expect("server handshake")
        });
        let mut raw = TcpStream::connect(addr).await.expect("raw connect");
        write_raw_handshake(&mut raw, HASH, CLIENT_ID).await;
        let ours = read_raw_handshake(&mut raw).await;
        assert_eq!(ours.info_hash(), &HASH);
        let (handle, events) = task.await.expect("server task");
        (handle, events, raw)
    }

    async fn write_raw_handshake(stream: &mut TcpStream, info_hash: InfoHash, peer_id: PeerId) {
        let handshake = Handshake::new(info_hash, peer_id);
        stream
            .write_all(&handshake.encode())
            .await
            .expect("write handshake");
    }

    async fn read_raw_handshake(stream: &mut TcpStream) -> Handshake {
        let mut bytes = [0u8; HANDSHAKE_LEN];
        stream.read_exact(&mut bytes).await.expect("read handshake");
        Handshake::decode(&bytes).expect("decode handshake")
    }

    /// The next event, with a timeout guard so a broken session fails the test
    /// instead of hanging it. Under `pause()` the guard is virtual time, which
    /// still fails the test rather than deadlocking it.
    async fn recv(events: &mut mpsc::Receiver<PeerEvent>) -> PeerEvent {
        timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("an event within 5 s")
            .expect("the event channel is open")
    }

    fn set(reserved: [u8; 8], bit: ReservedBit) -> bool {
        reserved
            .get(bit.byte)
            .is_some_and(|byte| byte & bit.mask != 0)
    }

    #[tokio::test]
    async fn handshake_pair_reports_peer_ids_and_reserved_bits() {
        let (listener, addr) = listener().await;
        let mut server_config = config(SERVER_ID);
        server_config.advertise_dht = false;
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept socket");
            accept(stream, server_config)
                .await
                .expect("server handshake")
        });

        let mut client_config = config(CLIENT_ID);
        client_config.advertise_dht = true;
        let (client, _client_events) = connect(addr, client_config)
            .await
            .expect("client handshake");
        let (server, _server_events) = task.await.expect("server task");

        assert_eq!(client.addr(), addr);
        assert_eq!(
            server.addr().ip(),
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(client.peer_id(), SERVER_ID);
        assert_eq!(server.peer_id(), CLIENT_ID);

        // The client advertised DHT, the server did not; both always advertise
        // the extension protocol.
        assert!(set(server.reserved(), RESERVED_DHT));
        assert!(set(server.reserved(), RESERVED_EXTENDED));
        assert!(!set(client.reserved(), RESERVED_DHT));
        assert!(set(client.reserved(), RESERVED_EXTENDED));
    }

    #[tokio::test]
    async fn connect_rejects_a_peer_for_another_torrent() {
        let (listener, addr) = listener().await;
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept socket");
            let _ = read_raw_handshake(&mut stream).await;
            write_raw_handshake(&mut stream, OTHER_HASH, SERVER_ID).await;
            // The dialler must hang up without saying anything more.
            let mut byte = [0u8; 1];
            stream.read(&mut byte).await.expect("read after handshake")
        });

        let result = connect(addr, config(CLIENT_ID)).await;
        let Err(err) = result else {
            panic!("a peer for another torrent must be refused");
        };
        assert!(
            matches!(
                err,
                PeerError::Handshake(HandshakeError::InfoHashMismatch { got, .. }) if got == OTHER_HASH
            ),
            "unexpected error: {err:?}"
        );
        assert_eq!(
            task.await.expect("raw task"),
            0,
            "the socket must be closed"
        );
    }

    #[tokio::test]
    async fn accept_rejects_a_peer_for_another_torrent() {
        let (listener, addr) = listener().await;
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept socket");
            accept(stream, config(SERVER_ID)).await
        });

        let mut raw = TcpStream::connect(addr).await.expect("raw connect");
        write_raw_handshake(&mut raw, OTHER_HASH, CLIENT_ID).await;

        let result = task.await.expect("accept task");
        let Err(err) = result else {
            panic!("a peer for another torrent must be refused");
        };
        assert!(
            matches!(
                err,
                PeerError::Handshake(HandshakeError::InfoHashMismatch { .. })
            ),
            "unexpected error: {err:?}"
        );

        // No reply was written: the first byte is an EOF.
        let mut byte = [0u8; 1];
        assert_eq!(raw.read(&mut byte).await.expect("read"), 0);
    }

    #[tokio::test]
    async fn messages_round_trip_in_both_directions() {
        let (client, mut client_events, server, mut server_events) = pair().await;

        let field = Bitfield::all(10);
        client
            .send(PeerCommand::Bitfield(field.clone()))
            .await
            .expect("send bitfield");
        assert_eq!(recv(&mut server_events).await, PeerEvent::Bitfield(field));

        client.send(PeerCommand::Have(7)).await.expect("send have");
        assert_eq!(recv(&mut server_events).await, PeerEvent::Have(7));

        client
            .send(PeerCommand::Interested)
            .await
            .expect("send interested");
        assert_eq!(recv(&mut server_events).await, PeerEvent::Interested);

        client
            .send(PeerCommand::Uninterested)
            .await
            .expect("send uninterested");
        assert_eq!(recv(&mut server_events).await, PeerEvent::Uninterested);

        client.send(PeerCommand::Choke).await.expect("send choke");
        assert_eq!(recv(&mut server_events).await, PeerEvent::Choke);

        client
            .send(PeerCommand::Unchoke)
            .await
            .expect("send unchoke");
        assert_eq!(recv(&mut server_events).await, PeerEvent::Unchoke);

        let request = PeerCommand::Request {
            index: 3,
            begin: 16_384,
            length: 16_384,
        };
        client.send(request).await.expect("send request");
        assert_eq!(
            recv(&mut server_events).await,
            PeerEvent::Request {
                index: 3,
                begin: 16_384,
                length: 16_384,
            }
        );

        client
            .send(PeerCommand::Cancel {
                index: 3,
                begin: 16_384,
                length: 16_384,
            })
            .await
            .expect("send cancel");
        assert_eq!(
            recv(&mut server_events).await,
            PeerEvent::Cancel {
                index: 3,
                begin: 16_384,
                length: 16_384,
            }
        );

        let block = vec![0xAB; BLOCK_SIZE];
        server
            .send(PeerCommand::Block {
                index: 9,
                begin: 32_768,
                data: block.clone(),
            })
            .await
            .expect("send block");
        assert_eq!(
            recv(&mut client_events).await,
            PeerEvent::Block {
                index: 9,
                begin: 32_768,
                data: block,
            }
        );

        // And the other direction for the plain messages.
        server
            .send(PeerCommand::Bitfield(Bitfield::all(10)))
            .await
            .expect("send bitfield back");
        assert_eq!(
            recv(&mut client_events).await,
            PeerEvent::Bitfield(Bitfield::all(10))
        );
        server.send(PeerCommand::Have(0)).await.expect("send have");
        assert_eq!(recv(&mut client_events).await, PeerEvent::Have(0));
        server
            .send(PeerCommand::Unchoke)
            .await
            .expect("send unchoke");
        assert_eq!(recv(&mut client_events).await, PeerEvent::Unchoke);
        server
            .send(PeerCommand::Request {
                index: 1,
                begin: 0,
                length: 16_384,
            })
            .await
            .expect("send request");
        assert_eq!(
            recv(&mut client_events).await,
            PeerEvent::Request {
                index: 1,
                begin: 0,
                length: 16_384,
            }
        );
    }

    #[tokio::test]
    async fn extended_messages_pass_through_with_any_id() {
        let (client, mut client_events, server, mut server_events) = pair().await;

        client
            .send(PeerCommand::Extended {
                id: 0x2A,
                payload: vec![1, 2, 3, 4],
            })
            .await
            .expect("send extended");
        assert_eq!(
            recv(&mut server_events).await,
            PeerEvent::Extended {
                id: 0x2A,
                payload: vec![1, 2, 3, 4],
            }
        );

        // An empty payload and id 0 (a BEP 10 handshake) are just as valid.
        server
            .send(PeerCommand::Extended {
                id: 0,
                payload: Vec::new(),
            })
            .await
            .expect("send extended handshake");
        assert_eq!(
            recv(&mut client_events).await,
            PeerEvent::Extended {
                id: 0,
                payload: Vec::new(),
            }
        );
    }

    #[tokio::test]
    async fn frames_sent_with_the_handshake_are_not_lost() {
        let (listener, addr) = listener().await;
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept socket");
            accept(stream, config(SERVER_ID))
                .await
                .expect("server handshake")
        });

        // One write, handshake and frame together: the handshake read must
        // consume exactly 68 bytes and leave the frame for the pump.
        let mut bytes = Handshake::new(HASH, CLIENT_ID).encode().to_vec();
        bytes.extend_from_slice(&encode(&Message::Interested).expect("encode interested"));
        let mut raw = TcpStream::connect(addr).await.expect("raw connect");
        raw.write_all(&bytes).await.expect("write handshake+frame");

        let (_handle, mut events) = task.await.expect("server task");
        assert_eq!(recv(&mut events).await, PeerEvent::Interested);
    }

    #[tokio::test]
    async fn a_smaller_frame_cap_is_enforced() {
        let mut config = config(SERVER_ID);
        config.max_frame_len = 32;
        let (_handle, mut events, mut raw) = raw_actor(config).await;

        // 33 bytes: legal BEP 3, over this connection's cap.
        raw.write_all(&33u32.to_be_bytes())
            .await
            .expect("write prefix");

        assert_eq!(
            recv(&mut events).await,
            PeerEvent::Closed(CloseReason::Protocol(ProtocolError::Decode(
                DecodeError::FrameTooLarge {
                    declared: 33,
                    max: 32,
                }
            )))
        );
    }

    #[tokio::test]
    async fn an_oversized_frame_is_a_protocol_error() {
        let (handle, mut events, mut raw) = raw_actor(config(SERVER_ID)).await;

        let declared = u32::try_from(DEFAULT_MAX_FRAME_LEN + 1).expect("fits u32");
        raw.write_all(&declared.to_be_bytes())
            .await
            .expect("write prefix");

        assert_eq!(
            recv(&mut events).await,
            PeerEvent::Closed(CloseReason::Protocol(ProtocolError::Decode(
                DecodeError::FrameTooLarge {
                    declared: DEFAULT_MAX_FRAME_LEN + 1,
                    max: DEFAULT_MAX_FRAME_LEN,
                }
            )))
        );
        assert!(
            matches!(
                handle.send(PeerCommand::KeepAlive).await,
                Err(PeerError::Closed)
            ),
            "the handle must report the session closed"
        );

        let mut byte = [0u8; 1];
        assert_eq!(raw.read(&mut byte).await.expect("read"), 0);
    }

    #[tokio::test]
    async fn an_unknown_message_id_is_a_protocol_error() {
        let (handle, mut events, mut raw) = raw_actor(config(SERVER_ID)).await;

        raw.write_all(&[0, 0, 0, 1, 0xFE])
            .await
            .expect("write garbage");

        assert_eq!(
            recv(&mut events).await,
            PeerEvent::Closed(CloseReason::Protocol(ProtocolError::Decode(
                DecodeError::UnknownMessageId { id: 0xFE }
            )))
        );
        assert!(matches!(
            handle.try_send(PeerCommand::KeepAlive),
            Err(PeerCommand::KeepAlive)
        ));
    }

    #[tokio::test]
    async fn a_bitfield_for_another_piece_count_is_a_protocol_error() {
        let (_handle, mut events, mut raw) = raw_actor(config(SERVER_ID)).await;

        // 10 pieces need two bytes; one byte is a mismatch.
        let frame = encode(&Message::Bitfield(BitfieldMessage::from_bytes(vec![0xFF])))
            .expect("encode bitfield");
        raw.write_all(&frame).await.expect("write bitfield");

        assert_eq!(
            recv(&mut events).await,
            PeerEvent::Closed(CloseReason::Protocol(ProtocolError::Bitfield(
                BitfieldError::WrongByteLength {
                    pieces: 10,
                    expected: 2,
                    got: 1,
                }
            )))
        );
    }

    #[tokio::test]
    async fn idle_timeout_drops_a_silent_peer() {
        let mut config = config(SERVER_ID);
        config.keepalive = Duration::ZERO;
        config.idle_timeout = Duration::from_secs(30);
        let (_handle, mut events, _raw) = raw_actor(config).await;

        pause();
        advance(Duration::from_secs(31)).await;

        assert_eq!(
            recv(&mut events).await,
            PeerEvent::Closed(CloseReason::IdleTimeout)
        );
    }

    #[tokio::test]
    async fn keepalive_is_sent_before_the_idle_timeout() {
        let mut config = config(SERVER_ID);
        config.keepalive = Duration::from_secs(120);
        config.idle_timeout = Duration::from_secs(150);
        let (_handle, mut events, mut raw) = raw_actor(config).await;

        pause();
        advance(Duration::from_secs(120)).await;

        let mut frame = [0u8; 4];
        raw.read_exact(&mut frame)
            .await
            .expect("a keep-alive frame arrives");
        assert_eq!(frame, [0, 0, 0, 0]);
        assert!(
            matches!(events.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "the peer is still alive at the keep-alive"
        );
    }

    #[tokio::test]
    async fn a_remote_keepalive_resets_the_idle_timeout() {
        let mut config = config(SERVER_ID);
        config.keepalive = Duration::ZERO;
        config.idle_timeout = Duration::from_secs(150);
        let (_handle, mut events, mut raw) = raw_actor(config).await;

        pause();
        advance(Duration::from_secs(100)).await;
        assert!(matches!(
            events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        // The keep-alive, then a message that proves both were read.
        raw.write_all(&[0, 0, 0, 0])
            .await
            .expect("write keep-alive");
        raw.write_all(&encode(&Message::Have { index: 4 }).expect("encode have"))
            .await
            .expect("write have");
        assert_eq!(recv(&mut events).await, PeerEvent::Have(4));

        // Past the original deadline (150 s from the start), but not past the
        // keep-alive's own deadline (100 s + 150 s).
        advance(Duration::from_secs(100)).await;
        assert!(
            matches!(events.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "the keep-alive must have reset the idle timer"
        );

        advance(Duration::from_secs(60)).await;
        assert_eq!(
            recv(&mut events).await,
            PeerEvent::Closed(CloseReason::IdleTimeout)
        );
    }

    #[tokio::test]
    async fn try_send_returns_the_command_when_the_queue_is_full() {
        let mut config = config(SERVER_ID);
        config.command_buffer = 2;
        config.keepalive = Duration::ZERO;
        config.idle_timeout = Duration::ZERO;
        let (handle, mut events, mut raw) = raw_actor(config).await;
        let handle = Arc::new(handle);

        // The raw side never reads, so the session's write buffer and the
        // kernel's socket buffers fill and the command queue backs up.
        let mut rejected = None;
        for _ in 0..1024 {
            tokio::task::yield_now().await;
            let command = PeerCommand::Block {
                index: 0,
                begin: 0,
                data: vec![0x5A; 32 * 1024],
            };
            if let Err(command) = handle.try_send(command) {
                rejected = Some(command);
                break;
            }
        }
        let rejected = rejected.expect("the queue must fill while the peer is not reading");
        assert!(matches!(rejected, PeerCommand::Block { .. }));

        // `send` must wait for capacity, not drop the command.
        let sender = {
            let handle = Arc::clone(&handle);
            tokio::spawn(async move {
                handle
                    .send(PeerCommand::Have(9))
                    .await
                    .expect("the blocked send completes once there is room")
            })
        };
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(
            !sender.is_finished(),
            "send must await capacity instead of dropping the command"
        );
        assert!(
            matches!(events.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "the session must stay open while commands are backed up"
        );

        // Drain the peer: the session flushes, frees a queue slot, and the
        // blocked send goes through.
        let drain = tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match raw.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        timeout(Duration::from_secs(10), sender)
            .await
            .expect("the blocked send completes")
            .expect("send task");

        drop(handle);
        drain.await.expect("drain task");
    }

    #[tokio::test]
    async fn a_peer_that_never_reads_hits_the_write_timeout() {
        let mut config = config(SERVER_ID);
        config.command_buffer = 2;
        config.keepalive = Duration::ZERO;
        config.idle_timeout = Duration::ZERO;
        let (handle, mut events, _raw) = raw_actor(config).await;
        let handle = Arc::new(handle);

        // Fill the kernel buffers and the command queue: nothing is moving.
        let mut full = false;
        for _ in 0..1024 {
            tokio::task::yield_now().await;
            let command = PeerCommand::Block {
                index: 0,
                begin: 0,
                data: vec![0x5A; 32 * 1024],
            };
            if handle.try_send(command).is_err() {
                full = true;
                break;
            }
        }
        assert!(
            full,
            "the write path must back up while the peer is not reading"
        );

        pause();
        advance(Duration::from_secs(31)).await;

        assert_eq!(
            recv(&mut events).await,
            PeerEvent::Closed(CloseReason::Io(io::ErrorKind::TimedOut))
        );
    }

    #[tokio::test]
    async fn dropping_the_handle_closes_the_connection() {
        let (client, mut client_events, server, mut server_events) = pair().await;

        drop(client);

        assert_eq!(
            recv(&mut server_events).await,
            PeerEvent::Closed(CloseReason::RemoteClosed)
        );
        assert_eq!(
            recv(&mut client_events).await,
            PeerEvent::Closed(CloseReason::LocalClose)
        );
        assert!(matches!(
            server.send(PeerCommand::KeepAlive).await,
            Err(PeerError::Closed)
        ));
    }

    #[tokio::test]
    async fn close_command_closes_both_sides() {
        let (client, mut client_events, _server, mut server_events) = pair().await;

        client.send(PeerCommand::Close).await.expect("send close");

        assert_eq!(
            recv(&mut client_events).await,
            PeerEvent::Closed(CloseReason::LocalClose)
        );
        assert_eq!(
            recv(&mut server_events).await,
            PeerEvent::Closed(CloseReason::RemoteClosed)
        );
    }

    #[tokio::test]
    async fn unencodable_commands_are_refused_locally() {
        let (client, _client_events, _server, mut server_events) = pair().await;

        let empty = client
            .send(PeerCommand::Block {
                index: 0,
                begin: 0,
                data: Vec::new(),
            })
            .await;
        assert!(
            matches!(empty, Err(PeerError::EmptyBlock)),
            "unexpected: {empty:?}"
        );

        let oversized = client
            .send(PeerCommand::Block {
                index: 0,
                begin: 0,
                data: vec![0u8; MAX_BLOCK_SIZE + 1],
            })
            .await;
        assert!(
            matches!(
                oversized,
                Err(PeerError::Encode(EncodeError::FrameTooLarge { .. }))
            ),
            "unexpected: {oversized:?}"
        );

        // Neither refusal may have touched the connection.
        client
            .send(PeerCommand::Interested)
            .await
            .expect("send interested");
        assert_eq!(recv(&mut server_events).await, PeerEvent::Interested);
    }

    #[tokio::test]
    async fn close_is_immediate_and_idempotent() {
        let (client, mut client_events, _server, mut server_events) = pair().await;

        client.close();
        client.close();

        assert_eq!(
            recv(&mut client_events).await,
            PeerEvent::Closed(CloseReason::LocalClose)
        );
        assert_eq!(
            recv(&mut server_events).await,
            PeerEvent::Closed(CloseReason::RemoteClosed)
        );
    }
}
