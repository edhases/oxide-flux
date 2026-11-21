//! Per-torrent swarm: peers, request pipeline, choking, uploads.
//!
//! One [`spawn`] call starts one tokio task that owns every peer connection of
//! one torrent: the piece picker, the request schedule, the choking policy, the
//! upload slots, the disk writer, the hash verifier and (optionally) the tracker
//! announce service. The task is the only place those pieces of state meet.
//!
//! ```text
//!  session ── SwarmHandle methods ──> [ command channel ] ──> swarm task
//!  session <── SwarmEvent ─────────── [ event channel   ] <──┘
//! ```
//!
//! Everything below the swarm is already tested and is reused unchanged:
//! [`flux_net::peer`] moves bytes, [`flux_core::picker`] decides which block to
//! ask for, [`flux_disk`] writes and hashes, and [`crate::announce`] talks to
//! trackers. This module only makes policy decisions and never re-implements
//! framing, name lookup or piece geometry.
//!
//! # Refuse what cannot work
//!
//! Only v1 SHA-1 piece hashes can be verified today. A v2-only torrent has
//! Merkle roots instead (the piece layers are parsed but never checked), so
//! [`spawn`] returns [`SwarmError::Unverifiable`] rather than download bytes that
//! could never be trusted. A hybrid torrent carries v1 hashes and is accepted.
//!
//! # Selective download
//!
//! [`Picker`] has no notion of file priorities: it queues every piece it does
//! not have. The swarm therefore applies [`SwarmConfig::desired`] by marking
//! every undesired piece complete in the picker. From the picker's point of view
//! a skipped piece is already on disk, so it is never requested; a later
//! [`SwarmHandle::set_desired`] calls [`Picker::reset_piece`] for pieces that
//! became desired and [`Picker::mark_complete`] for pieces that became
//! undesired. `desired` is produced by [`crate::pieces::desired_pieces`], which
//! already marks a piece desired when **any** owning file is selected.
//!
//! # Verify, then announce
//!
//! A peer's bitfield is a hint, never evidence. Blocks go to the disk writer,
//! and a piece is only added to the local `have` set after the verifier read it
//! back from disk and the SHA-1 matched. Only then is `Have` broadcast and
//! [`SwarmEvent::PieceVerified`] emitted. A corrupt piece is reset in the picker
//! (so it is fetched again) and reported as [`SwarmEvent::PieceCorrupt`].
//!
//! Because the writer may flush an incomplete piece on its timer, a piece can be
//! complete on disk without the writer emitting a `WriteEvent`. The swarm
//! therefore coalesces completed pieces and asks the writer for an explicit
//! `flush()` before hashing them: a flush command is ordered after every block
//! the swarm already queued, so what the verifier reads is exactly what the
//! peers sent.
//!
//! # Choking
//!
//! Every 10 s the swarm unchokes up to [`SwarmConfig::max_upload_slots`]
//! interested peers with the highest download rate since the previous tick.
//! Every third tick one slot is given to the next peer in a rotating order (the
//! optimistic unchoke). A peer that becomes interested while a slot is free is
//! unchoked immediately instead of waiting for the tick. Blocks are never served
//! to a choked peer, and a piece that completed leaves at least two upload slots
//! open.
//!
//! # Per-peer writer tasks
//!
//! Each connection has its own writer task so the swarm's scheduling loop never
//! awaits the transport, and so commands reach a peer in the order they were
//! queued. Frames are *not* rate-limited here: the transport drains every frame
//! buffered by a read before waiting for more bytes, so a burst of requests or
//! haves is delivered immediately.
//!
//! # Trackers and private torrents
//!
//! When [`SwarmConfig::announce`] is `Some`, a [`TrackerService`] is spawned and
//! its peers are fed into the dialler; real byte counters are published to it
//! after every tick, `completed` is reported once the desired set is verified,
//! and `stopped` is sent during shutdown. A private torrent still announces
//! normally; BEP 27 only forbids DHT/PEX/LSD, and none of those exist in this
//! build, so the flag changes nothing here yet.
//!
//! # What the session layer must honour
//!
//! * [`SwarmConfig::root`] must be the download root the plan's files are
//!   relative to, and the files must already exist (the session preallocates
//!   them). The swarm opens them with [`Allocation::None`] and never truncates.
//! * [`SwarmConfig::initial_have`] must be the set the startup verification
//!   proved, not a guess.
//! * The [`SwarmEvent`] channel must be drained. Events are sent with
//!   `send().await`; a stalled consumer applies backpressure to the whole swarm
//!   instead of losing state.
//! * [`SwarmHandle::add_inbound`] receives an accepted socket; the swarm runs the
//!   peer handshake itself with the plan's v1 info hash.
//! * [`SwarmHandle::shutdown`] must be awaited (or the handle dropped) so the
//!   writer is flushed and fsynced and the tracker gets its `stopped` announce.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use flux_core::picker::{Cancel, DEFAULT_BLOCK_SIZE, Error as PickerError, Mode, Picker};
use flux_disk::{
    Allocation, Block, BlockBackend, DiskError, DiskWriter, FileSet, PieceOutcome, StdBackend,
    SubmitError, Verifier, VerifyConfig, VerifyEvent, WriteEvent, WriterConfig,
};
use flux_net::peer::{PeerCommand, PeerConfig, PeerEvent, PeerHandle};
use flux_net::{Bitfield, DEFAULT_MAX_FRAME_LEN, MAX_BLOCK_SIZE};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval_at};

use crate::announce::{AnnounceSpec, TrackerEvent, TrackerService, TrackerStats};
use crate::pieces::PieceBitfield;
use crate::verify::TorrentPlan;

/// Commands queued between a [`SwarmHandle`] and the swarm task.
const COMMAND_CAPACITY: usize = 64;
/// Events queued between peer pumps, uploads and the swarm task.
const INPUT_CAPACITY: usize = 512;
/// Peer addresses delivered by the tracker forwarder.
const TRACKER_CAPACITY: usize = 32;
/// Peer count used when [`SwarmConfig::max_peers`] is zero.
pub const DEFAULT_MAX_PEERS: usize = 50;
/// Upload slot count used when [`SwarmConfig::max_upload_slots`] is zero.
pub const DEFAULT_MAX_UPLOAD_SLOTS: usize = 4;
/// Blocks kept in the swarm while the writer queue is full.
const PENDING_WRITE_BLOCKS: usize = 128;
/// Commands that may sit in one peer's outbound queue.
const PEER_OUT_CAPACITY: usize = 256;
/// Capacity of the per-peer writer channel.
const PEER_CHANNEL_CAPACITY: usize = 256;
/// Concurrent upload reads per peer.
const MAX_UPLOAD_IN_FLIGHT: usize = 8;
/// Extra inbound handshakes allowed above the peer cap before eviction.
const ACCEPT_HEADROOM: usize = 8;
/// Upload requests accepted per peer while its read cap is full. The picker's
/// per-peer in-flight limit is 16, so this never overflows in practice.
const MAX_UPLOAD_QUEUE: usize = 64;
/// A request with no block for this long is requeued as if the peer choked us.
const STALE_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How often stale requests are checked.
const STALE_CHECK_INTERVAL: Duration = Duration::from_secs(2);
/// Rechoke period (BEP 3's de-facto 10 s).
const RECHOKE_INTERVAL: Duration = Duration::from_secs(10);
/// Every Nth rechoke hands one slot to the optimistic peer.
const OPTIMISTIC_ROUNDS: u32 = 3;
/// Byte-counter publication period.
const STATS_INTERVAL: Duration = Duration::from_secs(1);
/// Upper bound on a dial/accept handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Writer cache budget handed to [`flux_disk`].
const WRITER_CACHE_BYTES: usize = 16 * 1024 * 1024;
/// Flush interval handed to [`flux_disk`]; incomplete pieces are still written
/// for streaming, which is why the verifier path uses an explicit flush.
const WRITER_FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Everything one swarm needs to start.
///
/// The plan and the two bitfields are the only inputs the swarm trusts; every
/// byte it reports later comes from a hash it computed itself.
pub struct SwarmConfig {
    /// The parsed, path-checked torrent.
    pub plan: Arc<TorrentPlan>,
    /// Download root the plan's files are relative to. The files must already
    /// exist and be preallocated.
    pub root: PathBuf,
    /// Our BEP 3 peer id.
    pub peer_id: [u8; 20],
    /// TCP port to advertise to trackers.
    pub listen_port: u16,
    /// Pieces the startup verification proved.
    pub initial_have: PieceBitfield,
    /// Pieces the current file selection needs (see [`crate::pieces`]).
    pub desired: PieceBitfield,
    /// Maximum live peer connections; `0` means [`DEFAULT_MAX_PEERS`].
    pub max_peers: usize,
    /// Maximum simultaneously unchoked peers; `0` means
    /// [`DEFAULT_MAX_UPLOAD_SLOTS`].
    pub max_upload_slots: usize,
    /// Tracker announce specification; `None` starts no tracker traffic at all.
    pub announce: Option<AnnounceSpec>,
    /// Hashing threads for the verifier pool.
    pub verifier_threads: usize,
}

/// Why a swarm could not be started.
#[derive(Debug, Error)]
pub enum SwarmError {
    /// The torrent has no v1 piece hashes, so nothing it downloads could be
    /// verified. Merkle verification for v2 is not implemented.
    #[error(
        "torrent has no v1 piece hashes; v2 Merkle verification is not implemented, \
         so its data cannot be verified"
    )]
    Unverifiable,
    /// The torrent has no pieces, so there is nothing to transfer.
    #[error("torrent has no pieces")]
    EmptyTorrent,
    /// A bitfield does not describe this torrent's pieces.
    #[error("{field} has {actual} bits but the torrent has {pieces} pieces")]
    BitfieldLength {
        /// Which bitfield was wrong (`initial_have` or `desired`).
        field: &'static str,
        /// Pieces in the torrent.
        pieces: usize,
        /// Bits supplied.
        actual: usize,
    },
    /// The piece geometry cannot be represented.
    #[error("unsupported piece layout: {0}")]
    Layout(&'static str),
    /// The picker rejected the piece count, piece sizes or block size.
    #[error(transparent)]
    Picker(#[from] flux_core::picker::Error),
    /// The storage layer refused the layout or a file.
    #[error(transparent)]
    Disk(#[from] DiskError),
    /// A configuration value makes no sense.
    #[error("configuration value `{0}` is not usable")]
    Config(&'static str),
    /// [`spawn`] was called without a tokio runtime.
    #[error("no tokio runtime is active, so the swarm task cannot be spawned")]
    NoRuntime,
}

/// A consistent view of one torrent, taken by the swarm task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwarmSnapshot {
    /// Live peer connections.
    pub peers: usize,
    /// Pieces verified on disk.
    pub pieces_have: usize,
    /// Pieces in the torrent.
    pub pieces_total: usize,
    /// Payload bytes received from peers. Corrupt bytes count here too;
    /// verification is reported separately.
    pub downloaded_bytes: u64,
    /// Payload bytes read from disk and queued for upload to peers.
    pub uploaded_bytes: u64,
    /// Whether every desired piece is verified.
    pub complete: bool,
}

/// Something the session needs to know about a running swarm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmEvent {
    /// A piece was read back and matched its v1 hash.
    PieceVerified {
        /// Piece index.
        piece: u32,
    },
    /// A piece failed verification (or its write failed) and was requeued.
    PieceCorrupt {
        /// Piece index.
        piece: u32,
    },
    /// The live peer count changed.
    PeerCount(usize),
    /// Byte counters changed.
    Stats {
        /// Payload bytes received.
        downloaded_bytes: u64,
        /// Payload bytes sent.
        uploaded_bytes: u64,
    },
    /// Every desired piece is verified. Emitted at most once.
    Completed,
    /// The swarm stopped because of an error. Emitted at most once, last.
    Failed(String),
}

/// Control surface of a running swarm.
///
/// All methods are cheap: they hand a command to the swarm task and (where the
/// answer matters) wait for it. They return `false`/`None` once the task has
/// stopped instead of panicking.
#[derive(Debug, Clone)]
pub struct SwarmHandle {
    commands: mpsc::Sender<Command>,
}

impl SwarmHandle {
    /// Dials `addr` and adds the connection when the handshake succeeds.
    ///
    /// Returns `false` when the address is already being dialled or connected,
    /// when the swarm is paused or shutting down, or when the peer limit cannot
    /// be honoured. The dial itself runs on its own task; failure is silent
    /// (this is an optimization hint, not a delivery guarantee).
    pub async fn add_peer(&self, addr: SocketAddr) -> bool {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::AddPeer { addr, reply })
            .await
            .is_err()
        {
            return false;
        }
        done.await.unwrap_or(false)
    }

    /// Runs the inbound handshake on an accepted socket and adds the peer.
    ///
    /// The stream must be freshly accepted by the session listener. Returns
    /// `false` when the swarm is paused, shutting down, or at capacity.
    pub async fn add_inbound(&self, stream: TcpStream) -> bool {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::AddInbound {
                stream,
                handshake: None,
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        done.await.unwrap_or(false)
    }

    /// Like [`SwarmHandle::add_inbound`], but the caller already read the 68-byte
    /// handshake (a listener needs it to route by info hash).
    pub async fn add_inbound_prefixed(&self, stream: TcpStream, handshake: Vec<u8>) -> bool {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::AddInbound {
                stream,
                handshake: Some(handshake),
                reply,
            })
            .await
            .is_err()
        {
            return false;
        }
        done.await.unwrap_or(false)
    }

    /// Replaces the desired-piece set.
    ///
    /// Newly desired pieces are re-armed in the picker; pieces that are no
    /// longer desired are marked complete so they stop being requested.
    pub async fn set_desired(&self, desired: PieceBitfield) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::SetDesired { desired, reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }

    /// Moves the streaming playhead (clamped to the last piece).
    pub async fn set_playhead(&self, piece: usize) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::SetPlayhead { piece, reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }

    /// Changes the piece-selection policy and re-schedules immediately.
    pub async fn set_mode(&self, mode: Mode) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::SetMode { mode, reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }

    /// Stops requesting and connecting and chokes every peer.
    ///
    /// Verified state is kept. Returns `false` when the swarm was already
    /// paused or has stopped.
    pub async fn pause(&self) -> bool {
        let (reply, done) = oneshot::channel();
        if self.commands.send(Command::Pause { reply }).await.is_err() {
            return false;
        }
        done.await.unwrap_or(false)
    }

    /// Resumes requesting, connecting and uploading.
    pub async fn resume(&self) -> bool {
        let (reply, done) = oneshot::channel();
        if self.commands.send(Command::Resume { reply }).await.is_err() {
            return false;
        }
        done.await.unwrap_or(false)
    }

    /// A consistent snapshot, or `None` once the swarm task has stopped.
    pub async fn snapshot(&self) -> Option<SwarmSnapshot> {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::Snapshot { reply })
            .await
            .is_err()
        {
            return None;
        }
        done.await.ok().flatten()
    }

    /// Flushes the writer, closes every peer, stops the tracker and joins the
    /// background work.
    pub async fn shutdown(self) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::Shutdown { reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

/// Commands a handle can send to the swarm task.
enum Command {
    AddPeer {
        addr: SocketAddr,
        reply: oneshot::Sender<bool>,
    },
    AddInbound {
        stream: TcpStream,
        handshake: Option<Vec<u8>>,
        reply: oneshot::Sender<bool>,
    },
    SetDesired {
        desired: PieceBitfield,
        reply: oneshot::Sender<()>,
    },
    SetPlayhead {
        piece: usize,
        reply: oneshot::Sender<()>,
    },
    SetMode {
        mode: Mode,
        reply: oneshot::Sender<()>,
    },
    Pause {
        reply: oneshot::Sender<bool>,
    },
    Resume {
        reply: oneshot::Sender<bool>,
    },
    Snapshot {
        reply: oneshot::Sender<Option<SwarmSnapshot>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// Everything that funnels into the swarm task from the outside.
enum Input {
    Connected {
        addr: SocketAddr,
        handle: PeerHandle,
        events: mpsc::Receiver<PeerEvent>,
    },
    AcceptFailed {
        addr: SocketAddr,
    },
    DialFailed {
        addr: SocketAddr,
    },
    PeerEvent {
        peer: u64,
        event: PeerEvent,
    },
    PeerGone {
        peer: u64,
    },
    Upload {
        peer: u64,
        index: u32,
        begin: u32,
        data: Option<Vec<u8>>,
    },
    TrackerPeers(Vec<SocketAddr>),
}

/// One wake-up of the swarm loop.
enum Wake {
    Command(Option<Command>),
    Input(Option<Input>),
    Write(Option<WriteEvent>),
    Verify(Option<VerifyEvent>),
    Flush(Option<Option<String>>),
    Stale,
    Choke,
    Stats,
}

/// One live peer connection.
struct PeerState {
    addr: SocketAddr,
    /// Outbound frames for this peer's writer task.
    out: mpsc::Sender<PeerCommand>,
    /// The task that owns the transport handle and paces frames.
    writer: JoinHandle<()>,
    /// Commands not yet accepted by the writer channel.
    pending_out: VecDeque<PeerCommand>,
    /// We asked the peer for pieces.
    am_interested: bool,
    /// The peer is choking us.
    peer_choked: bool,
    /// The peer asked us for pieces.
    peer_interested: bool,
    /// We are choking the peer.
    am_choking: bool,
    /// We received a bitfield or a have, so the picker knows its availability.
    announced: bool,
    /// Payload bytes received since the last rechoke.
    downloaded_since_tick: u64,
    /// Last valid block arrival.
    last_block: Instant,
    /// When the oldest in-flight request was issued.
    outstanding_since: Option<Instant>,
    /// Upload reads in flight for this peer.
    uploads: usize,
    /// Requests accepted while the read cap was full, served in order.
    pending_uploads: VecDeque<(u32, u32, u32)>,
    /// Issue time, piece and begin of every request this peer still owes.
    requested: VecDeque<(Instant, u32, u32)>,
    /// Corrupt pieces this peer contributed to.
    corrupt_strikes: u32,
}

/// State of the spawned task.
struct Swarm {
    layout: Arc<flux_disk::FileLayout>,
    piece_length: u32,
    piece_count: usize,
    piece_sizes: Vec<u32>,
    peer_config: PeerConfig,
    max_peers: usize,
    max_upload_slots: usize,

    picker: Picker,
    writer: Arc<DiskWriter>,
    verifier: Option<Verifier>,
    files: Arc<FileSet>,

    peers: HashMap<u64, PeerState>,
    by_addr: HashMap<SocketAddr, u64>,
    dialing: HashSet<SocketAddr>,
    accepting: HashSet<SocketAddr>,
    banned: HashSet<SocketAddr>,
    next_peer_id: u64,
    /// Peers that contributed blocks to a piece that is not verified yet.
    piece_sources: HashMap<u32, HashSet<u64>>,

    have: PieceBitfield,
    own_bitfield: Bitfield,
    desired: PieceBitfield,
    desired_remaining: usize,
    desired_left_bytes: u64,
    /// Bytes of every piece that is not verified yet, independent of selection.
    left_bytes: u64,
    tracker_completed: bool,

    downloaded: u64,
    uploaded: u64,
    paused: bool,
    completed: bool,
    shutting_down: bool,
    fatal: Option<String>,

    pending_write: VecDeque<Block>,
    dirty: BTreeSet<u32>,
    verifying: HashSet<usize>,
    pending_verify: VecDeque<u32>,
    flush_in_flight: bool,
    flush_task: Option<JoinHandle<()>>,
    /// Pieces covered by the flush currently in flight. Their blocks were all
    /// queued before the flush command, so hashing them afterwards is safe;
    /// pieces completed later wait for the next flush.
    flush_batch: Vec<u32>,
    /// Kept alive so the flush channel never closes and `recv` cannot busy-loop.
    flush_keep: mpsc::Sender<Option<String>>,
    /// Stop picking until the writer cache is flushed and stale partial blocks
    /// are gone (a re-armed piece must not collide with its cached copies).
    holding: bool,

    tracker: Option<TrackerService>,
    tracker_stats: Option<watch::Sender<TrackerStats>>,

    rechoke_round: u32,
    optimistic_cursor: usize,

    events: mpsc::Sender<SwarmEvent>,
    input_tx: mpsc::Sender<Input>,
    last_peer_count: usize,
}

/// Starts one torrent's swarm task.
///
/// # Errors
///
/// Returns [`SwarmError::Unverifiable`] for a torrent without v1 piece hashes
/// (v2-only), plus typed errors for an empty torrent, mismatched bitfields,
/// unusable piece geometry, and any layout/file failure from [`flux_disk`].
pub fn spawn(
    config: SwarmConfig,
    events: mpsc::Sender<SwarmEvent>,
) -> Result<SwarmHandle, SwarmError> {
    // Without a reactor `tokio::spawn` would panic; refuse instead.
    tokio::runtime::Handle::try_current().map_err(|_| SwarmError::NoRuntime)?;

    let plan = Arc::clone(&config.plan);
    let hashes = plan
        .piece_hashes()
        .ok_or(SwarmError::Unverifiable)?
        .to_vec();
    let piece_count = plan.piece_count();
    if piece_count == 0 {
        return Err(SwarmError::EmptyTorrent);
    }
    if config.initial_have.len() != piece_count {
        return Err(SwarmError::BitfieldLength {
            field: "initial_have",
            pieces: piece_count,
            actual: config.initial_have.len(),
        });
    }
    if config.desired.len() != piece_count {
        return Err(SwarmError::BitfieldLength {
            field: "desired",
            pieces: piece_count,
            actual: config.desired.len(),
        });
    }
    if hashes.len() != piece_count {
        return Err(SwarmError::Layout(
            "v1 hash count does not match the piece count",
        ));
    }
    let piece_length = plan.piece_length();
    if piece_length == 0 {
        return Err(SwarmError::Config("piece_length"));
    }
    let info_hash = plan
        .metainfo()
        .info
        .hashes
        .v1
        .ok_or(SwarmError::Unverifiable)?;

    let mut piece_sizes = Vec::with_capacity(piece_count);
    for piece in 0..piece_count {
        let size = plan
            .layout()
            .piece_size(piece)
            .ok_or(SwarmError::Layout("piece index outside the layout"))?;
        if size == 0 {
            return Err(SwarmError::Layout("a piece has zero length"));
        }
        piece_sizes.push(size);
    }

    let mut picker = Picker::with_piece_sizes(piece_sizes.clone(), DEFAULT_BLOCK_SIZE)?;
    picker.set_mode(Mode::Sequential);

    let mut have = PieceBitfield::new(piece_count);
    for piece in config.initial_have.ones() {
        if have.insert(piece) {
            // Verified before the swarm started; nothing to fetch or check.
            let _ = picker.mark_complete(piece);
        }
    }
    let own_bitfield = Bitfield::from_bytes(piece_count, &have.to_bytes())
        .map_err(|_| SwarmError::Layout("have bitfield does not describe the pieces"))?;

    // Selective download: the picker queues every piece, so the undesired ones
    // are marked complete. They are then indistinguishable from pieces already
    // on disk and are never requested.
    let desired = config.desired;
    for piece in 0..piece_count {
        if !desired.get(piece).unwrap_or(false) {
            let _ = picker.mark_complete(piece);
        }
    }

    let layout = plan.disk_layout();
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());
    let root = config.root.clone(); // borrowed below for the verifier
    let writer_config = WriterConfig {
        allocation: Allocation::None,
        flush_interval: Some(WRITER_FLUSH_INTERVAL),
        cache_bytes: WRITER_CACHE_BYTES,
        ..WriterConfig::new(piece_length)
    };
    let (writer, write_events) = DiskWriter::start(
        writer_config,
        Arc::clone(&layout),
        Arc::clone(&backend),
        config.root.clone(),
    )?;
    let (verifier, verify_events) = Verifier::start(
        VerifyConfig {
            threads: config.verifier_threads.max(1),
            ..VerifyConfig::new()
        },
        Arc::clone(&layout),
        Arc::clone(&backend),
        root.clone(),
        piece_length,
        hashes,
    )?;
    let files = Arc::new(FileSet::open(Arc::clone(&layout), backend, &root)?);

    let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (input_tx, input_rx) = mpsc::channel(INPUT_CAPACITY);
    let (flush_tx, flush_rx) = mpsc::channel(4);

    let (tracker, tracker_stats) = match config.announce.clone() {
        Some(mut spec) => {
            // The peer id and port the swarm actually uses win over whatever the
            // caller put in the spec, so an announce can never lie about them.
            spec.peer_id = config.peer_id;
            spec.port = config.listen_port;
            let left = remaining_bytes(&have, &piece_sizes);
            let (stats_tx, stats_rx) = watch::channel(TrackerStats {
                uploaded: 0,
                downloaded: 0,
                left,
            });
            let (tracker_tx, tracker_rx) = mpsc::channel(TRACKER_CAPACITY);
            let service = TrackerService::spawn(spec, stats_rx, tracker_tx);
            let forward_tx = input_tx.clone();
            drop(tokio::spawn(forward_tracker(tracker_rx, forward_tx)));
            (Some(service), Some(stats_tx))
        }
        None => (None, None),
    };

    let mut peer_config = PeerConfig::new(info_hash, config.peer_id);
    peer_config.pieces = piece_count;
    // A bitfield frame is `1 + ceil(pieces / 8)` bytes; the default cap only
    // covers 1 048 576 pieces.
    peer_config.max_frame_len = DEFAULT_MAX_FRAME_LEN.max(piece_count.div_ceil(8) + 1);
    // No DHT exists in this build, so the reserved bit stays clear whether or
    // not the torrent is private. The peer layer never claims a capability the
    // client does not have.
    peer_config.advertise_dht = false;

    let desired_remaining = desired
        .ones()
        .filter(|piece| !have.get(*piece).unwrap_or(false))
        .count();
    let desired_left = desired_left_bytes(&desired, &have, &piece_sizes);
    let left_bytes = remaining_bytes(&have, &piece_sizes);

    let swarm = Swarm {
        layout,
        piece_length,
        piece_count,
        piece_sizes,
        peer_config,
        max_peers: if config.max_peers == 0 {
            DEFAULT_MAX_PEERS
        } else {
            config.max_peers
        },
        max_upload_slots: if config.max_upload_slots == 0 {
            DEFAULT_MAX_UPLOAD_SLOTS
        } else {
            config.max_upload_slots
        },
        picker,
        writer: Arc::new(writer),
        verifier: Some(verifier),
        files,
        peers: HashMap::new(),
        by_addr: HashMap::new(),
        dialing: HashSet::new(),
        accepting: HashSet::new(),
        banned: HashSet::new(),
        next_peer_id: 1,
        piece_sources: HashMap::new(),
        have,
        own_bitfield,
        desired,
        desired_remaining,
        desired_left_bytes: desired_left,
        left_bytes,
        tracker_completed: false,
        downloaded: 0,
        uploaded: 0,
        paused: false,
        completed: false,
        shutting_down: false,
        fatal: None,
        pending_write: VecDeque::new(),
        dirty: BTreeSet::new(),
        verifying: HashSet::new(),
        pending_verify: VecDeque::new(),
        flush_in_flight: false,
        flush_task: None,
        flush_batch: Vec::new(),
        flush_keep: flush_tx,
        holding: false,
        tracker,
        tracker_stats,
        rechoke_round: 0,
        optimistic_cursor: 0,
        events,
        input_tx,
        last_peer_count: 0,
    };

    drop(tokio::spawn(swarm.run(
        command_rx,
        input_rx,
        write_events,
        verify_events,
        flush_rx,
    )));

    Ok(SwarmHandle {
        commands: command_tx,
    })
}

/// Sum of the sizes of the desired pieces that are not verified yet.
fn desired_left_bytes(desired: &PieceBitfield, have: &PieceBitfield, piece_sizes: &[u32]) -> u64 {
    desired
        .ones()
        .filter(|piece| !have.get(*piece).unwrap_or(false))
        .filter_map(|piece| piece_sizes.get(piece))
        .map(|size| u64::from(*size))
        .sum()
}

/// Bytes of every piece that has not been verified, ignoring file selection.
fn remaining_bytes(have: &PieceBitfield, piece_sizes: &[u32]) -> u64 {
    (0..piece_sizes.len())
        .filter(|piece| !have.get(*piece).unwrap_or(false))
        .filter_map(|piece| piece_sizes.get(piece))
        .map(|size| u64::from(*size))
        .sum()
}

/// Payload bytes carried by a block frame, zero for every other command.
fn block_len(command: &PeerCommand) -> u64 {
    match command {
        PeerCommand::Block { data, .. } => data.len() as u64,
        _ => 0,
    }
}

/// Forwards tracker results into the swarm's single input channel.
async fn forward_tracker(mut events: mpsc::Receiver<TrackerEvent>, tx: mpsc::Sender<Input>) {
    while let Some(event) = events.recv().await {
        let input = match event {
            TrackerEvent::Peers { addr, .. } => Input::TrackerPeers(addr),
            // Tracker warnings and failures are not fatal for the swarm: the
            // tracker service keeps its own backoff schedule.
            TrackerEvent::Warn(_) | TrackerEvent::Failed { .. } => continue,
        };
        if tx.send(input).await.is_err() {
            return;
        }
    }
}

/// Forwards one connection's inbound events into the swarm's input channel.
async fn pump_peer(peer: u64, mut events: mpsc::Receiver<PeerEvent>, tx: mpsc::Sender<Input>) {
    while let Some(event) = events.recv().await {
        if tx.send(Input::PeerEvent { peer, event }).await.is_err() {
            return;
        }
    }
    // The terminal `Closed` event is best effort; the channel closing is not.
    let _ = tx.send(Input::PeerGone { peer }).await;
}

/// Owns one connection's transport handle and forwards its outbound frames.
///
/// The task ends (and the connection closes) when the command channel closes or
/// the transport fails.
async fn peer_writer(handle: PeerHandle, mut commands: mpsc::Receiver<PeerCommand>) {
    while let Some(command) = commands.recv().await {
        let closing = matches!(command, PeerCommand::Close);
        if handle.send(command).await.is_err() {
            break;
        }
        if closing {
            break;
        }
    }
    handle.close();
}

impl Swarm {
    /// The main loop: one wake-up per command, peer event, disk event or timer.
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut inputs: mpsc::Receiver<Input>,
        mut writes: mpsc::Receiver<WriteEvent>,
        mut verifies: mpsc::Receiver<VerifyEvent>,
        mut flushes: mpsc::Receiver<Option<String>>,
    ) {
        let now = Instant::now();
        let mut stale = interval_at(now + STALE_CHECK_INTERVAL, STALE_CHECK_INTERVAL);
        let mut choke = interval_at(now + RECHOKE_INTERVAL, RECHOKE_INTERVAL);
        let mut stats = interval_at(now + STATS_INTERVAL, STATS_INTERVAL);
        stale.set_missed_tick_behavior(MissedTickBehavior::Delay);
        choke.set_missed_tick_behavior(MissedTickBehavior::Delay);
        stats.set_missed_tick_behavior(MissedTickBehavior::Delay);

        if self.is_complete() {
            // A fully verified torrent starts as a seed: the `started` announce
            // already reports `left = 0`, so there is no `completed` transition
            // to report for this run.
            self.finish_once(false).await;
        }

        let mut shutdown_reply: Option<oneshot::Sender<()>> = None;
        loop {
            let wake = tokio::select! {
                command = commands.recv() => Wake::Command(command),
                input = inputs.recv(), if self.pending_write.len() < PENDING_WRITE_BLOCKS => {
                    Wake::Input(input)
                }
                event = writes.recv() => Wake::Write(event),
                event = verifies.recv() => Wake::Verify(event),
                event = flushes.recv() => Wake::Flush(event),
                _ = stale.tick() => Wake::Stale,
                _ = choke.tick() => Wake::Choke,
                _ = stats.tick() => Wake::Stats,
            };

            match wake {
                Wake::Command(None) => break,
                Wake::Command(Some(Command::AddPeer { addr, reply })) => {
                    let accepted = self.start_dial(addr, true);
                    let _ = reply.send(accepted);
                }
                Wake::Command(Some(Command::AddInbound {
                    stream,
                    handshake,
                    reply,
                })) => {
                    let accepted = self.start_accept(stream, handshake);
                    let _ = reply.send(accepted);
                }
                Wake::Command(Some(Command::SetDesired { desired, reply })) => {
                    self.apply_desired(desired);
                    let _ = reply.send(());
                }
                Wake::Command(Some(Command::SetPlayhead { piece, reply })) => {
                    self.picker.set_playhead(piece);
                    let _ = reply.send(());
                }
                Wake::Command(Some(Command::SetMode { mode, reply })) => {
                    self.picker.set_mode(mode);
                    self.schedule_all();
                    let _ = reply.send(());
                }
                Wake::Command(Some(Command::Pause { reply })) => {
                    let _ = reply.send(self.pause());
                }
                Wake::Command(Some(Command::Resume { reply })) => {
                    let _ = reply.send(self.resume());
                }
                Wake::Command(Some(Command::Snapshot { reply })) => {
                    let _ = reply.send(Some(self.snapshot()));
                }
                Wake::Command(Some(Command::Shutdown { reply })) => {
                    self.shutting_down = true;
                    shutdown_reply = Some(reply);
                    break;
                }

                Wake::Input(None) => break,
                Wake::Input(Some(input)) => self.handle_input(input).await,

                Wake::Write(None) => {
                    self.fatal = Some("disk writer stopped unexpectedly".to_owned());
                    break;
                }
                Wake::Write(Some(event)) => self.on_write(event).await,
                Wake::Verify(None) => {
                    self.fatal = Some("verifier stopped unexpectedly".to_owned());
                    break;
                }
                Wake::Verify(Some(event)) => self.on_verified(event).await,
                // The keep-alive sender in `flush_keep` means this never closes
                // while the loop runs; `None` would mean the task is unwinding.
                Wake::Flush(Some(error)) => self.on_flush_done(error).await,
                Wake::Flush(None) => {}
                Wake::Stale => self.requeue_stale_requests(),
                Wake::Choke => self.rechoke(),
                Wake::Stats => {
                    self.publish_stats();
                    self.emit(SwarmEvent::Stats {
                        downloaded_bytes: self.downloaded,
                        uploaded_bytes: self.uploaded,
                    })
                    .await;
                }
            }

            if self.fatal.is_some() {
                break;
            }
            self.after_wake().await;
        }

        // Keep both bounded result channels drained while the workers stop, or
        // the writer can block on an event nobody is reading.
        let write_drain = tokio::spawn(async move { while writes.recv().await.is_some() {} });
        let verify_drain = tokio::spawn(async move { while verifies.recv().await.is_some() {} });
        self.teardown(shutdown_reply, write_drain, verify_drain)
            .await;
    }

    /// Scheduling, cancel routing, backpressure checks and peer-count reporting.
    async fn after_wake(&mut self) {
        self.drain_writes();
        self.pump_all();
        self.schedule_all();
        for cancel in self.picker.take_cancels() {
            self.route_cancel(cancel);
        }
        self.pump_all();
        self.maybe_flush();
        let count = self.peers.len();
        if count != self.last_peer_count {
            self.last_peer_count = count;
            self.emit(SwarmEvent::PeerCount(count)).await;
        }
    }

    async fn emit(&mut self, event: SwarmEvent) {
        // A dropped receiver means the session stopped; the swarm still finishes
        // its disk work on shutdown, so there is nothing else to do here.
        let _ = self.events.send(event).await;
    }

    fn snapshot(&self) -> SwarmSnapshot {
        SwarmSnapshot {
            peers: self.peers.len(),
            pieces_have: self.have.count_ones(),
            pieces_total: self.piece_count,
            downloaded_bytes: self.downloaded,
            uploaded_bytes: self.uploaded,
            complete: self.is_complete(),
        }
    }

    fn is_complete(&self) -> bool {
        self.desired_remaining == 0 && self.desired.count_ones() > 0
    }

    // -----------------------------------------------------------------------
    // Peers and connections
    // -----------------------------------------------------------------------

    fn start_dial(&mut self, addr: SocketAddr, allow_evict: bool) -> bool {
        if self.paused || self.shutting_down || self.banned.contains(&addr) {
            return false;
        }
        if self.by_addr.contains_key(&addr) || self.dialing.contains(&addr) {
            return false;
        }
        let total = self.peers.len() + self.dialing.len();
        if total >= self.max_peers {
            if !allow_evict || !self.evict_one() {
                return false;
            }
            if self.peers.len() + self.dialing.len() >= self.max_peers {
                return false;
            }
        }
        self.dialing.insert(addr);
        let config = self.peer_config.clone();
        let tx = self.input_tx.clone();
        drop(tokio::spawn(async move {
            match tokio::time::timeout(CONNECT_TIMEOUT, flux_net::peer::connect(addr, config)).await
            {
                Ok(Ok((handle, events))) => {
                    let _ = tx
                        .send(Input::Connected {
                            addr,
                            handle,
                            events,
                        })
                        .await;
                }
                _ => {
                    let _ = tx.send(Input::DialFailed { addr }).await;
                }
            }
        }));
        true
    }

    fn start_accept(&mut self, stream: TcpStream, handshake: Option<Vec<u8>>) -> bool {
        if self.paused || self.shutting_down {
            return false;
        }
        let Ok(addr) = stream.peer_addr() else {
            return false;
        };
        if self.banned.contains(&addr) || self.by_addr.contains_key(&addr) {
            return false;
        }
        // Do not evict an established peer for a connection that has not even
        // completed its handshake. Allow a small handshake window and decide in
        // `on_connected`, where the newcomer has proved it belongs to us.
        if self.peers.len() + self.accepting.len() >= self.max_peers + ACCEPT_HEADROOM {
            return false;
        }
        self.accepting.insert(addr);
        let config = self.peer_config.clone();
        let tx = self.input_tx.clone();
        drop(tokio::spawn(async move {
            let accepted = tokio::time::timeout(CONNECT_TIMEOUT, async {
                match handshake {
                    Some(prefix) => flux_net::peer::accept_prefixed(stream, &prefix, config).await,
                    None => flux_net::peer::accept(stream, config).await,
                }
            })
            .await;
            match accepted {
                Ok(Ok((handle, events))) => {
                    let _ = tx
                        .send(Input::Connected {
                            addr,
                            handle,
                            events,
                        })
                        .await;
                }
                _ => {
                    let _ = tx.send(Input::AcceptFailed { addr }).await;
                }
            }
        }));
        true
    }

    async fn on_connected(
        &mut self,
        addr: SocketAddr,
        handle: PeerHandle,
        events: mpsc::Receiver<PeerEvent>,
    ) {
        self.dialing.remove(&addr);
        self.accepting.remove(&addr);
        if self.paused || self.shutting_down || self.banned.contains(&addr) {
            handle.close();
            return;
        }
        // A dial to our own listener would otherwise connect us to ourselves.
        if handle.peer_id() == self.peer_config.peer_id {
            handle.close();
            return;
        }
        if self.by_addr.contains_key(&addr) {
            handle.close();
            return;
        }
        if self.peers.len() >= self.max_peers && !self.evict_one() {
            handle.close();
            return;
        }

        let id = self.next_peer_id;
        self.next_peer_id = self.next_peer_id.wrapping_add(1);
        let bitfield = Bitfield::from_bytes(self.piece_count, &self.have.to_bytes())
            .unwrap_or_else(|_| Bitfield::new(self.piece_count));

        let (out, out_rx) = mpsc::channel(PEER_CHANNEL_CAPACITY);
        // The writer task owns the transport handle. Dropping or aborting it
        // closes the connection, which is what `remove_peer` relies on.
        let writer = tokio::spawn(peer_writer(handle, out_rx));

        let mut pending_out = VecDeque::new();
        pending_out.push_back(PeerCommand::Bitfield(bitfield));

        self.peers.insert(
            id,
            PeerState {
                addr,
                out,
                writer,
                pending_out,
                am_interested: false,
                peer_choked: true,
                peer_interested: false,
                am_choking: true,
                announced: false,
                downloaded_since_tick: 0,
                last_block: Instant::now(),
                outstanding_since: None,
                uploads: 0,
                pending_uploads: VecDeque::new(),
                requested: VecDeque::new(),
                corrupt_strikes: 0,
            },
        );
        self.by_addr.insert(addr, id);
        if self
            .picker
            .add_peer_availability(id, std::iter::empty::<usize>())
            .is_err()
        {
            // Cannot happen for `id`s we allocated, but never leave a peer the
            // picker does not know about.
            self.remove_peer(id);
            return;
        }

        let tx = self.input_tx.clone();
        drop(tokio::spawn(pump_peer(id, events, tx)));
    }

    fn remove_peer(&mut self, id: u64) {
        let Some(mut peer) = self.peers.remove(&id) else {
            return;
        };
        self.by_addr.remove(&peer.addr);
        self.picker.on_peer_gone(id);
        let dropped = Self::discard_pending_blocks(&mut peer);
        self.uploaded = self.uploaded.saturating_sub(dropped);
        // Dropping the task drops the transport handle and its socket.
        peer.writer.abort();
    }

    /// Drops queued block frames and returns how many payload bytes never left.
    ///
    /// `uploaded` is credited when a block is queued, so every path that discards
    /// queued blocks must take the credit back or the ratio lies.
    fn discard_pending_blocks(peer: &mut PeerState) -> u64 {
        let mut dropped = 0_u64;
        peer.pending_out.retain(|command| {
            if let PeerCommand::Block { data, .. } = command {
                dropped = dropped.saturating_add(data.len() as u64);
                false
            } else {
                true
            }
        });
        dropped
    }

    /// Drops the least productive peer. Returns whether one was removed.
    fn evict_one(&mut self) -> bool {
        let victim = self
            .peers
            .iter()
            .min_by_key(|(_, peer)| (peer.downloaded_since_tick, peer.last_block))
            .map(|(&id, _)| id);
        match victim {
            Some(id) => {
                self.remove_peer(id);
                true
            }
            None => false,
        }
    }

    // -----------------------------------------------------------------------
    // Peer messages
    // -----------------------------------------------------------------------

    async fn handle_input(&mut self, input: Input) {
        match input {
            Input::Connected {
                addr,
                handle,
                events,
            } => self.on_connected(addr, handle, events).await,
            Input::DialFailed { addr } => {
                self.dialing.remove(&addr);
            }
            Input::AcceptFailed { addr } => {
                self.accepting.remove(&addr);
            }
            Input::PeerEvent { peer, event } => self.on_peer_event(peer, event).await,
            Input::PeerGone { peer } => self.remove_peer(peer),
            Input::Upload {
                peer,
                index,
                begin,
                data,
            } => self.on_upload(peer, index, begin, data).await,
            Input::TrackerPeers(addrs) => {
                for addr in addrs {
                    // Tracker peers never evict a peer we already have.
                    let _ = self.start_dial(addr, false);
                }
            }
        }
    }

    async fn on_peer_event(&mut self, id: u64, event: PeerEvent) {
        match event {
            PeerEvent::Choke => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_choked = true;
                    peer.outstanding_since = None;
                    peer.requested.clear();
                    peer.pending_out
                        .retain(|command| !matches!(command, PeerCommand::Request { .. }));
                }
                self.picker.on_peer_choked(id);
            }
            PeerEvent::Unchoke => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_choked = false;
                }
            }
            PeerEvent::Interested => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_interested = true;
                }
                self.maybe_unchoke(id);
            }
            PeerEvent::Uninterested => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.peer_interested = false;
                }
                self.choke(id);
            }
            PeerEvent::Have(index) => {
                if let Ok(piece) = usize::try_from(index)
                    && self.picker.add_peer_have(id, piece).is_ok()
                    && let Some(peer) = self.peers.get_mut(&id)
                {
                    peer.announced = true;
                }
            }
            PeerEvent::Bitfield(field) => {
                let result = self.picker.add_peer_bitfield(id, field.to_bytes());
                if result.is_ok()
                    && let Some(peer) = self.peers.get_mut(&id)
                {
                    peer.announced = true;
                }
            }
            PeerEvent::Request {
                index,
                begin,
                length,
            } => self.start_upload(id, index, begin, length),
            PeerEvent::Block { index, begin, data } => self.on_block(id, index, begin, data),
            PeerEvent::Cancel { index, begin, .. } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.pending_out.retain(|command| {
                        !matches!(
                            command,
                            PeerCommand::Block {
                                index: queued,
                                begin: queued_begin,
                                ..
                            } if *queued == index && *queued_begin == begin
                        )
                    });
                }
            }
            // There is no metadata layer in this build; extension messages are
            // carried by the transport but not interpreted here.
            PeerEvent::Extended { .. } => {}
            PeerEvent::Closed(_) => self.remove_peer(id),
        }
    }

    fn on_block(&mut self, id: u64, index: u32, begin: u32, data: Vec<u8>) {
        let Ok(piece) = usize::try_from(index) else {
            return;
        };
        let Ok(length) = u32::try_from(data.len()) else {
            return;
        };
        match self.picker.on_block_received(id, piece, begin, length) {
            Ok(()) => {
                self.downloaded = self.downloaded.saturating_add(u64::from(length));
                self.piece_sources.entry(index).or_default().insert(id);
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.downloaded_since_tick =
                        peer.downloaded_since_tick.saturating_add(u64::from(length));
                    peer.last_block = Instant::now();
                    peer.requested
                        .retain(|(_, piece, offset)| !(*piece == index && *offset == begin));
                    if self.picker.in_flight(id) == 0 {
                        peer.outstanding_since = None;
                    } else {
                        peer.outstanding_since =
                            peer.requested.front().map(|(issued, _, _)| *issued);
                    }
                }
                self.queue_write(Block::new(index, begin, data));
                if self.picker.is_piece_complete(piece) && !self.have.get(piece).unwrap_or(false) {
                    self.dirty.insert(index);
                }
            }
            Err(
                PickerError::BlockNotRequested { .. } | PickerError::BlockAlreadyReceived { .. },
            ) => {
                // A late duplicate or a block we already re-queued: never write
                // it, but do not punish the rest of the peer's requests either.
            }
            Err(_error) => {
                // A malformed block (or one for a piece the peer never had) is
                // never written. Requeue its outstanding requests so a broken
                // peer cannot stall the piece.
                self.picker.on_peer_choked(id);
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.outstanding_since = None;
                    peer.requested.clear();
                    peer.pending_out
                        .retain(|command| !matches!(command, PeerCommand::Request { .. }));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Downloads: writer, verification, scheduling
    // -----------------------------------------------------------------------

    fn queue_write(&mut self, block: Block) {
        if self.pending_write.is_empty() {
            match self.writer.try_write_block(block) {
                Ok(()) => {}
                Err(rejected) => self.pending_write.push_back(rejected.block),
            }
        } else {
            self.pending_write.push_back(block);
        }
    }

    fn drain_writes(&mut self) {
        while let Some(block) = self.pending_write.pop_front() {
            match self.writer.try_write_block(block) {
                Ok(()) => {}
                Err(rejected) => {
                    // The payload comes back intact; never drop it.
                    self.pending_write.push_front(rejected.block);
                    break;
                }
            }
        }
    }

    fn drain_blocked(&self) -> bool {
        !self.pending_write.is_empty()
    }

    async fn on_write(&mut self, event: WriteEvent) {
        let piece = event.piece as usize;
        match event.result {
            Ok(()) => {
                if !self.have.get(piece).unwrap_or(true) {
                    self.dirty.insert(event.piece);
                }
            }
            Err(error) => {
                if matches!(
                    &error,
                    DiskError::Closed | DiskError::WorkerPanicked | DiskError::Spawn { .. }
                ) {
                    self.fatal = Some(format!("disk writer failed: {error}"));
                    return;
                }
                let _ = self.picker.reset_piece(piece);
                if matches!(&error, DiskError::BlockOverlap { .. }) {
                    // An older copy of this block is still in the writer's cache
                    // from before the piece was re-armed. Reset, flush the cache
                    // and only then request the piece again.
                    self.holding = true;
                    return;
                }
                // The bytes did not land as a piece. Requeue it; verification
                // (or the hash check after the re-download) is the authority.
                self.dirty.insert(event.piece);
                self.emit(SwarmEvent::PieceCorrupt { piece: event.piece })
                    .await;
            }
        }
        self.drain_writes();
    }

    async fn on_verified(&mut self, event: VerifyEvent) {
        let piece = event.piece as usize;
        self.verifying.remove(&piece);
        match event.outcome {
            PieceOutcome::Good => {
                let newly = self.have.insert(piece);
                self.piece_sources.remove(&event.piece);
                let _ = self.picker.mark_complete(piece);
                let _ = self.own_bitfield.set(piece);
                if newly {
                    if let Some(size) = self.piece_sizes.get(piece) {
                        self.left_bytes = self.left_bytes.saturating_sub(u64::from(*size));
                    }
                    if self.desired.get(piece).unwrap_or(false) {
                        self.desired_remaining = self.desired_remaining.saturating_sub(1);
                        if let Some(size) = self.piece_sizes.get(piece) {
                            self.desired_left_bytes =
                                self.desired_left_bytes.saturating_sub(u64::from(*size));
                        }
                    }
                    self.broadcast(PeerCommand::Have(event.piece));
                    self.emit(SwarmEvent::PieceVerified { piece: event.piece })
                        .await;
                }
                // A tracker must hear `completed` only for the whole torrent,
                // never for "every selected file".
                if self.have.all()
                    && !self.tracker_completed
                    && let Some(tracker) = self.tracker.as_ref()
                {
                    self.tracker_completed = true;
                    tracker.completed().await;
                }
                if self.is_complete() {
                    self.finish_once(true).await;
                }
            }
            PieceOutcome::Corrupt { .. } => {
                self.dirty.remove(&event.piece);
                let _ = self.picker.reset_piece(piece);
                let sources = self.piece_sources.remove(&event.piece).unwrap_or_default();
                let mut ban = Vec::new();
                for id in &sources {
                    if let Some(peer) = self.peers.get_mut(id) {
                        peer.corrupt_strikes = peer.corrupt_strikes.saturating_add(1);
                        // A sole contributor is the only possible poisoner; a
                        // shared piece needs a pattern before anyone is banned.
                        if sources.len() == 1 || peer.corrupt_strikes >= 3 {
                            ban.push((*id, peer.addr));
                        }
                    }
                }
                for (_, addr) in &ban {
                    self.banned.insert(*addr);
                }
                for (id, _) in ban {
                    self.remove_peer(id);
                }
                self.emit(SwarmEvent::PieceCorrupt { piece: event.piece })
                    .await;
            }
            PieceOutcome::Cancelled => {
                self.dirty.remove(&event.piece);
                self.piece_sources.remove(&event.piece);
            }
            PieceOutcome::Failed(error) => {
                self.fatal = Some(format!("could not verify piece {}: {error}", event.piece));
            }
        }
        self.pump_verify();
    }

    /// Emits `Completed` exactly once; optionally reports it to the tracker.
    async fn finish_once(&mut self, notify_tracker: bool) {
        if self.completed {
            return;
        }
        self.completed = true;
        self.publish_stats();
        self.emit(SwarmEvent::Completed).await;
        // Seeding should not leave the swarm choking everyone; make sure at
        // least the minimum slots are offered at the next opportunity.
        self.rechoke();
        if notify_tracker && let Some(tracker) = self.tracker.as_ref() {
            tracker.completed().await;
        }
    }

    /// Adds every verified-and-written piece to the verifier queue, coalescing
    /// bursts into a single writer flush.
    fn maybe_flush(&mut self) {
        if (self.dirty.is_empty() && !self.holding) || self.flush_in_flight || self.shutting_down {
            return;
        }
        // A flush is only ordered after blocks that already reached the writer.
        // Blocks still parked in `pending_write` would land after it, so wait.
        if self.drain_blocked() {
            return;
        }
        self.flush_in_flight = true;
        // Everything dirty right now has all of its blocks already queued
        // ahead of the flush command. Pieces that complete while the flush is
        // in flight must wait for the next one, or they could be hashed before
        // their last block reached the disk.
        self.flush_batch = self.dirty.iter().copied().collect();
        self.dirty.clear();
        let writer = Arc::clone(&self.writer);
        let tx = self.flush_keep.clone();
        self.flush_task = Some(tokio::spawn(async move {
            let result = writer.flush().await;
            let error = result.err().map(|err| err.to_string());
            let _ = tx.send(error).await;
        }));
    }

    async fn on_flush_done(&mut self, error: Option<String>) {
        self.flush_in_flight = false;
        if let Some(task) = self.flush_task.take() {
            let _ = task.await;
        }
        if let Some(error) = error {
            self.fatal = Some(format!("disk flush failed: {error}"));
            return;
        }
        // The cache is empty now, so a piece that was waiting for its stale
        // partial blocks to be evicted may be requested again.
        self.holding = false;
        let batch = std::mem::take(&mut self.flush_batch);
        for piece in batch {
            let index = piece as usize;
            if self.have.get(index).unwrap_or(true) {
                continue;
            }
            if !self.picker.is_piece_complete(index) {
                continue;
            }
            if self.verifying.contains(&index) {
                continue;
            }
            self.pending_verify.push_back(piece);
        }
        self.pump_verify();
    }

    fn pump_verify(&mut self) {
        loop {
            let Some(&piece) = self.pending_verify.front() else {
                return;
            };
            let index = piece as usize;
            if self.have.get(index).unwrap_or(true) || self.verifying.contains(&index) {
                self.pending_verify.pop_front();
                continue;
            }
            let Some(verifier) = self.verifier.as_ref() else {
                return;
            };
            match verifier.try_verify(piece) {
                Ok(()) => {
                    self.pending_verify.pop_front();
                    self.verifying.insert(index);
                }
                Err(SubmitError::Full) => return,
                Err(SubmitError::Closed) => {
                    self.fatal = Some("verifier stopped unexpectedly".to_owned());
                    return;
                }
            }
        }
    }

    fn schedule_all(&mut self) {
        let ids: Vec<u64> = self.peers.keys().copied().collect();
        for id in ids {
            self.schedule_peer(id);
        }
    }

    fn schedule_peer(&mut self, id: u64) {
        if self.paused || self.shutting_down {
            return;
        }
        let Some(peer) = self.peers.get(&id) else {
            return;
        };
        let announced = peer.announced;
        let am_interested = peer.am_interested;
        let peer_choked = peer.peer_choked;
        let mut out_len = peer.pending_out.len();

        if !announced {
            return;
        }

        let needs = self.desired_remaining > 0;
        if !needs {
            // Nothing left to ask for; stop asking the peer to keep us fed.
            if am_interested {
                self.queue_control(id, PeerCommand::Uninterested);
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.am_interested = false;
                }
            }
            return;
        }

        if !am_interested {
            self.queue_control(id, PeerCommand::Interested);
            if let Some(peer) = self.peers.get_mut(&id) {
                peer.am_interested = true;
            }
        }
        if peer_choked || self.drain_blocked() || self.holding {
            return;
        }

        while out_len < PEER_OUT_CAPACITY {
            let request = self.picker.pick(id);
            let Some(request) = request else {
                break;
            };
            let index = u32::try_from(request.piece).unwrap_or(u32::MAX);
            let begin = request.offset;
            self.queue_control(
                id,
                PeerCommand::Request {
                    index,
                    begin,
                    length: request.length,
                },
            );
            out_len += 1;
            let now = Instant::now();
            if let Some(peer) = self.peers.get_mut(&id) {
                peer.requested.push_back((now, index, begin));
                if peer.outstanding_since.is_none() {
                    peer.outstanding_since = Some(now);
                }
            }
        }
        if self.picker.in_flight(id) == 0
            && let Some(peer) = self.peers.get_mut(&id)
        {
            peer.outstanding_since = None;
            peer.requested.clear();
        }
    }

    /// Queues a control frame for one peer.
    ///
    /// Each peer owns an outbound writer task that preserves order and keeps the
    /// swarm loop from awaiting the socket.
    fn queue_control(&mut self, id: u64, command: PeerCommand) {
        let mut dropped = 0_u64;
        {
            let Some(peer) = self.peers.get_mut(&id) else {
                return;
            };
            if peer.pending_out.is_empty() {
                match peer.out.try_send(command) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(command)) => {
                        peer.pending_out.push_back(command);
                    }
                    Err(mpsc::error::TrySendError::Closed(command)) => {
                        dropped = block_len(&command);
                    }
                }
            } else if peer.pending_out.len() < PEER_OUT_CAPACITY {
                peer.pending_out.push_back(command);
            } else {
                dropped = block_len(&command);
            }
        }
        self.uploaded = self.uploaded.saturating_sub(dropped);
    }

    fn pump_all(&mut self) {
        let ids: Vec<u64> = self.peers.keys().copied().collect();
        for id in ids {
            self.pump_peer(id);
        }
    }

    fn pump_peer(&mut self, id: u64) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        while let Some(command) = peer.pending_out.pop_front() {
            match peer.out.try_send(command) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(command)) => {
                    peer.pending_out.push_front(command);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => break,
            }
        }
    }

    fn broadcast(&mut self, command: PeerCommand) {
        let ids: Vec<u64> = self.peers.keys().copied().collect();
        for id in ids {
            self.queue_control(id, command.clone());
        }
    }

    fn route_cancel(&mut self, cancel: Cancel) {
        let Ok(index) = u32::try_from(cancel.piece) else {
            return;
        };
        if let Some(peer) = self.peers.get_mut(&cancel.peer)
            && peer.pending_out.len() < PEER_OUT_CAPACITY
        {
            peer.pending_out.push_back(PeerCommand::Cancel {
                index,
                begin: cancel.offset,
                length: cancel.length,
            });
        }
    }

    fn requeue_stale_requests(&mut self) {
        let now = Instant::now();
        for id in self.peers.keys().copied().collect::<Vec<_>>() {
            let mut stale = Vec::new();
            if let Some(peer) = self.peers.get_mut(&id) {
                while let Some(&(issued, index, begin)) = peer.requested.front() {
                    if now.duration_since(issued) < STALE_REQUEST_TIMEOUT {
                        break;
                    }
                    peer.requested.pop_front();
                    stale.push((index, begin));
                }
            }
            for (index, begin) in stale {
                if let Ok(piece) = usize::try_from(index) {
                    let _ = self.picker.requeue_block(id, piece, begin);
                }
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.pending_out.retain(|command| {
                        !matches!(
                            command,
                            PeerCommand::Request { index: i, begin: b, .. }
                                if *i == index && *b == begin
                        )
                    });
                }
            }
            if let Some(peer) = self.peers.get_mut(&id) {
                peer.outstanding_since = peer.requested.front().map(|(issued, _, _)| *issued);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Desired pieces
    // -----------------------------------------------------------------------

    fn apply_desired(&mut self, desired: PieceBitfield) {
        if desired.len() != self.piece_count {
            return;
        }
        let previous = std::mem::replace(&mut self.desired, desired);
        let mut rearmed = false;
        for piece in 0..self.piece_count {
            let was = previous.get(piece).unwrap_or(false);
            let now = self.desired.get(piece).unwrap_or(false);
            if was == now {
                continue;
            }
            let have = self.have.get(piece).unwrap_or(false);
            if now && !have {
                let _ = self.picker.reset_piece(piece);
                // The writer may still hold some of this piece's blocks from
                // before it was skipped. Flush them out before re-requesting,
                // or the writer will reject the new copies as overlaps.
                rearmed = true;
            } else if !now {
                let _ = self.picker.mark_complete(piece);
            }
        }
        if rearmed {
            self.holding = true;
        }
        self.recompute_desired();
        for cancel in self.picker.take_cancels() {
            self.route_cancel(cancel);
        }
        self.schedule_all();
    }

    fn recompute_desired(&mut self) {
        let mut remaining = 0_usize;
        let mut left = 0_u64;
        for piece in self.desired.ones() {
            if self.have.get(piece).unwrap_or(false) {
                continue;
            }
            remaining += 1;
            if let Some(size) = self.piece_sizes.get(piece) {
                left = left.saturating_add(u64::from(*size));
            }
        }
        self.desired_remaining = remaining;
        self.desired_left_bytes = left;
        self.publish_stats();
    }

    // -----------------------------------------------------------------------
    // Uploads and choking
    // -----------------------------------------------------------------------

    fn start_upload(&mut self, id: u64, index: u32, begin: u32, length: u32) {
        let Some(peer) = self.peers.get(&id) else {
            return;
        };
        if peer.am_choking {
            return;
        }
        if length == 0 || length > MAX_BLOCK_SIZE as u32 {
            return;
        }
        let Ok(piece) = usize::try_from(index) else {
            return;
        };
        if !self.have.get(piece).unwrap_or(false) {
            return;
        }
        let Ok((piece_offset, piece_len)) = self.layout.piece_range(self.piece_length, index)
        else {
            return;
        };
        let Some(end) = begin.checked_add(length) else {
            return;
        };
        if end > piece_len {
            return;
        }
        if peer.uploads >= MAX_UPLOAD_IN_FLIGHT {
            // Every request must eventually be answered or the peer stalls
            // until its own timeout, so queue instead of dropping.
            if let Some(peer) = self.peers.get_mut(&id)
                && peer.pending_uploads.len() < MAX_UPLOAD_QUEUE
            {
                peer.pending_uploads.push_back((index, begin, length));
            }
            return;
        }
        if let Some(peer) = self.peers.get_mut(&id) {
            peer.uploads += 1;
        }
        self.spawn_upload(
            id,
            index,
            begin,
            length,
            piece_offset.saturating_add(u64::from(begin)),
        );
    }

    /// Starts one upload read and reports back through the input channel.
    fn spawn_upload(&self, id: u64, index: u32, begin: u32, length: u32, position: u64) {
        let files = Arc::clone(&self.files);
        let tx = self.input_tx.clone();
        drop(tokio::task::spawn_blocking(move || {
            let mut buffer = vec![0_u8; length as usize];
            let data = match files.read_exact_at(position, &mut buffer) {
                Ok(()) => Some(buffer),
                Err(_) => None,
            };
            let _ = tx.blocking_send(Input::Upload {
                peer: id,
                index,
                begin,
                data,
            });
        }));
    }

    async fn on_upload(&mut self, id: u64, index: u32, begin: u32, data: Option<Vec<u8>>) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        peer.uploads = peer.uploads.saturating_sub(1);
        let Some(data) = data else {
            // A read failure is that peer's problem: close it.
            self.remove_peer(id);
            return;
        };
        if !peer.am_choking && peer.pending_out.len() < PEER_OUT_CAPACITY {
            let length = data.len() as u64;
            peer.pending_out
                .push_back(PeerCommand::Block { index, begin, data });
            self.uploaded = self.uploaded.saturating_add(length);
        }
        // A choked (or backed up) peer gets no block; it will ask again. Either
        // way, refill the read slot from the queue.
        self.next_upload(id);
    }

    /// Starts queued upload reads up to the per-peer cap.
    fn next_upload(&mut self, id: u64) {
        loop {
            let Some(peer) = self.peers.get(&id) else {
                return;
            };
            if peer.uploads >= MAX_UPLOAD_IN_FLIGHT || peer.am_choking {
                return;
            }
            let Some((index, begin, length)) = peer.pending_uploads.front().copied() else {
                return;
            };
            let Ok(piece) = usize::try_from(index) else {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.pending_uploads.pop_front();
                }
                continue;
            };
            let Ok((piece_offset, _)) = self.layout.piece_range(self.piece_length, index) else {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.pending_uploads.pop_front();
                }
                continue;
            };
            if !self.have.get(piece).unwrap_or(false) {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.pending_uploads.pop_front();
                }
                continue;
            }
            if let Some(peer) = self.peers.get_mut(&id) {
                peer.pending_uploads.pop_front();
                peer.uploads += 1;
            }
            let position = piece_offset.saturating_add(u64::from(begin));
            self.spawn_upload(id, index, begin, length, position);
        }
    }

    fn rechoke(&mut self) {
        self.rechoke_round = self.rechoke_round.wrapping_add(1);
        let mut interested: Vec<(u64, u64)> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.peer_interested)
            .map(|(&id, peer)| (id, peer.downloaded_since_tick))
            .collect();
        interested.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        let slots = if self.completed {
            self.max_upload_slots.max(2)
        } else {
            self.max_upload_slots
        };
        let mut chosen: Vec<u64> = interested.iter().take(slots).map(|(id, _)| *id).collect();

        // Every third tick one slot goes to a rotating peer, so a peer that is
        // never the fastest still gets a chance.
        if self.rechoke_round.is_multiple_of(OPTIMISTIC_ROUNDS)
            && !interested.is_empty()
            && slots > 0
        {
            let candidates: Vec<u64> = interested
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| !chosen.contains(id))
                .collect();
            if !candidates.is_empty() {
                let pick = candidates[self.optimistic_cursor % candidates.len()];
                self.optimistic_cursor = self.optimistic_cursor.wrapping_add(1);
                chosen.truncate(slots.saturating_sub(1));
                chosen.push(pick);
            }
        }

        for (id, _) in &interested {
            if chosen.contains(id) {
                self.unchoke(*id);
            } else {
                self.choke(*id);
            }
        }
        for peer in self.peers.values_mut() {
            peer.downloaded_since_tick = 0;
        }
    }

    fn maybe_unchoke(&mut self, id: u64) {
        if self.paused {
            return;
        }
        let slots = if self.completed {
            self.max_upload_slots.max(2)
        } else {
            self.max_upload_slots
        };
        let unchoked = self.peers.values().filter(|peer| !peer.am_choking).count();
        if unchoked < slots {
            self.unchoke(id);
        }
    }

    fn unchoke(&mut self, id: u64) {
        if let Some(peer) = self.peers.get_mut(&id)
            && peer.am_choking
        {
            peer.am_choking = false;
            peer.pending_out.push_back(PeerCommand::Unchoke);
        }
    }

    fn choke(&mut self, id: u64) {
        let mut dropped = 0;
        if let Some(peer) = self.peers.get_mut(&id) {
            if !peer.am_choking {
                peer.am_choking = true;
                peer.pending_out.push_back(PeerCommand::Choke);
            }
            // Never send a block to a peer we just choked.
            dropped = Self::discard_pending_blocks(peer);
            peer.pending_uploads.clear();
        }
        self.uploaded = self.uploaded.saturating_sub(dropped);
    }

    // -----------------------------------------------------------------------
    // Pause, stats and shutdown
    // -----------------------------------------------------------------------

    fn pause(&mut self) -> bool {
        if self.paused || self.shutting_down {
            return false;
        }
        self.paused = true;
        let ids: Vec<u64> = self.peers.keys().copied().collect();
        for id in ids {
            self.picker.on_peer_choked(id);
            if let Some(peer) = self.peers.get_mut(&id) {
                peer.outstanding_since = None;
                peer.requested.clear();
                peer.pending_out
                    .retain(|command| !matches!(command, PeerCommand::Request { .. }));
                if peer.am_interested {
                    peer.am_interested = false;
                    peer.pending_out.push_back(PeerCommand::Uninterested);
                }
            }
            self.choke(id);
        }
        true
    }

    fn resume(&mut self) -> bool {
        if !self.paused || self.shutting_down {
            return false;
        }
        self.paused = false;
        self.rechoke();
        self.schedule_all();
        true
    }

    fn publish_stats(&self) {
        if let Some(stats) = self.tracker_stats.as_ref() {
            stats.send_replace(TrackerStats {
                uploaded: self.uploaded,
                downloaded: self.downloaded,
                left: self.left_bytes,
            });
        }
    }

    async fn teardown(
        mut self,
        reply: Option<oneshot::Sender<()>>,
        write_drain: JoinHandle<()>,
        verify_drain: JoinHandle<()>,
    ) {
        let peers: Vec<PeerState> = self.peers.drain().map(|(_, peer)| peer).collect();
        for peer in &peers {
            peer.writer.abort();
        }
        drop(peers);

        if let Some(task) = self.flush_task.take() {
            let _ = task.await;
        }
        // Blocks already accepted from peers but parked behind a full writer
        // queue are real data: hand them over before shutting the writer down.
        let parked: Vec<Block> = self.pending_write.drain(..).collect();
        for block in parked {
            if self.writer.write_block(block).await.is_err() {
                break;
            }
        }
        if let Some(verifier) = self.verifier.take() {
            let _ = verifier.shutdown().await;
        }
        if let Some(writer) = Arc::into_inner(self.writer) {
            let _ = writer.shutdown().await;
        }
        if let Some(tracker) = self.tracker.take() {
            tracker.shutdown().await;
        }

        // Both workers have dropped their senders by now.
        let _ = write_drain.await;
        let _ = verify_drain.await;

        if let Some(error) = self.fatal.take() {
            let _ = self.events.send(SwarmEvent::Failed(error)).await;
        }
        if let Some(reply) = reply {
            let _ = reply.send(());
        }
    }
}
