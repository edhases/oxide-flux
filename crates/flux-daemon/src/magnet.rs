//! Magnet metadata bootstrap: turn an info hash into a real `.torrent`.
//!
//! A magnet link only carries hashes, so a client that joins a swarm with one
//! owns no info dictionary: no name, no file list, no piece hashes. BEP 9
//! (`ut_metadata`) fixes that — another peer carries the info dictionary and
//! hands it over in 16 KiB slices over the BEP 10 extension protocol. This
//! module is the client side of that exchange: [`fetch`] takes a [`FetchSpec`]
//! (already resolved from a link by [`FetchSpec::from_magnet`]), finds peers
//! through [`flux_net::tracker`] or the caller's seed list, asks one of them for
//! the metadata, verifies it against the info hash and returns a complete
//! `.torrent` ready for [`crate::verify::plan`].
//!
//! ## Flow
//!
//! ```text
//!  seeds? ---- yes -------------------------------------------> dial peers
//!    | no
//!    +--> announce Started (left: 0) --> tracker peers ---------> dial peers
//!
//!  per peer, in order:
//!    handshake (BEP 3, extension bit set)
//!    --> extended handshake (id 0) advertising ut_metadata: 1
//!    <-- extended handshake (id 0): their ut_metadata id, metadata_size?
//!    --> ut_metadata request piece 0
//!    <-- data piece 0 (tells total_size)
//!    --> requests for the remaining pieces, at most 4 in flight
//!    <-- data pieces ...                       (reject ends this peer's turn)
//!    assemble --> SHA-1 over the exact info bytes == info_hash?
//! ```
//!
//! Peers are tried **sequentially**. Each attempt gets `remaining budget /
//! remaining peers`, so one unresponsive peer can never eat the whole
//! [`FetchSpec::timeout`]; the last attempt gets whatever is left, and the whole
//! call is additionally wrapped in that timeout. Sequential attempts also mean
//! no detached tasks: every socket is closed before the next peer is dialled,
//! with [`PeerCommand::Close`] when the command queue has room and an immediate
//! [`PeerHandle::close`] otherwise, on success and on failure alike.
//!
//! ## The `pieces: 0` assumption
//!
//! [`flux_net::peer::PeerConfig::pieces`] is the torrent's piece count, which a
//! metadata fetch cannot know yet. This module passes `pieces: 0` and relies on
//! `flux_net::peer` treating zero as *unknown*: a bitfield from the peer is then
//! acknowledged without being decoded instead of being rejected as the wrong
//! length. A peer that sends a bitfield right after its handshake is therefore
//! harmless here. Every connection that knows the torrent shape must still set
//! the real count — this is the one caller allowed to leave it at zero.
//!
//! ## Trust
//!
//! The info hash is the only trust anchor. Metadata is never returned before
//! its SHA-1 matches: assembling goes through [`assemble_torrent`], which
//! splices the peer's raw info bytes under `4:info` verbatim, and
//! [`flux_core::metainfo::parse`] hashes *those* bytes — never a re-encode — so
//! the hash is preserved exactly. A mismatch is [`MagnetError::HashMismatch`]
//! and nothing is returned. A peer that claims more than
//! [`MAX_METADATA_SIZE`] is refused before any buffer of that size exists.
//!
//! The daemon has no `sha1` dependency on its library target, so verification
//! reuses the workspace parser rather than duplicating a hash implementation.
//! A consequence worth naming: metadata whose info dictionary is not a valid
//! torrent at all is reported as [`MagnetError::Protocol`] instead of
//! `HashMismatch`, because the hash is only observable through that parser.
//!
//! ## Events
//!
//! Events are best effort. They are delivered with `try_send` and dropped when
//! the channel is full or closed; a UI that stops reading must never stall or
//! fail a fetch.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use flux_core::bencode::{self, Value};
use flux_core::magnet::MagnetLink;
use flux_core::metainfo;
use flux_net::metadata::{
    self, Error as MetadataError, MAX_METADATA_SIZE, METADATA_PIECE_SIZE, MetadataMessage,
};
use flux_net::peer::{self, PeerCommand, PeerConfig, PeerEvent, PeerHandle};
use flux_net::tracker::{AnnounceRequest, Announcer, AnnouncerConfig, Event};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout, timeout_at};

/// Default overall budget for one fetch: 60 s.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Default number of peers tried in one fetch: 20.
pub const DEFAULT_MAX_PEERS: usize = 20;

/// How many `ut_metadata` requests may be outstanding on one connection.
///
/// Four keeps the round trip to a well-behaved peer short without flooding its
/// command queue; BEP 9 metadata is at most 256 pieces.
pub const MAX_PIECES_IN_FLIGHT: u32 = 4;

/// Our own `ut_metadata` extension id, advertised in the extended handshake.
///
/// The value only matters to a peer that asks *us* for metadata, which this
/// module never serves. It is still advertised because BEP 10 requires
/// `m.ut_metadata` to be present for the peer to know we speak BEP 9 at all.
pub const OUR_UT_METADATA_ID: u8 = 1;

/// The BEP 10 extended handshake id: extension message id 0.
const EXTENDED_HANDSHAKE_ID: u8 = 0;

/// Everything one metadata fetch needs.
///
/// Build it with [`FetchSpec::new`] (or [`FetchSpec::from_magnet`]) so the
/// defaults match [`DEFAULT_TIMEOUT`] and [`DEFAULT_MAX_PEERS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchSpec {
    /// The v1 info hash the metadata must hash to: the trust anchor.
    pub info_hash: [u8; 20],
    /// Tracker announce URLs from the magnet's `tr` parameters.
    pub trackers: Vec<String>,
    /// Extra peers to try (manual adds, tests).
    ///
    /// When this list is non-empty it replaces the tracker announce entirely:
    /// no network call is made beyond the peers themselves.
    pub seeds: Vec<SocketAddr>,
    /// Our peer id, used in every handshake.
    pub peer_id: [u8; 20],
    /// The TCP port announced to trackers.
    pub port: u16,
    /// Overall budget for the whole fetch, including tracker announces.
    pub timeout: Duration,
    /// Upper bound on distinct peers tried, seeds included.
    pub max_peers: usize,
}

impl Default for FetchSpec {
    /// Zero hash, zero peer id, default budgets.
    ///
    /// The hash and peer id have no meaningful default; callers must set them
    /// (prefer [`FetchSpec::new`]).
    fn default() -> Self {
        Self::new([0; 20], [0; 20], 0)
    }
}

impl FetchSpec {
    /// A fetch for `info_hash` with the default timeout and peer cap.
    #[must_use]
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20], port: u16) -> Self {
        Self {
            info_hash,
            trackers: Vec::new(),
            seeds: Vec::new(),
            peer_id,
            port,
            timeout: DEFAULT_TIMEOUT,
            max_peers: DEFAULT_MAX_PEERS,
        }
    }

    /// Builds a spec from a parsed magnet link.
    ///
    /// # Errors
    ///
    /// Returns [`MagnetError::NoInfoHash`] when the link carries only a v2
    /// (`urn:btmh:`) hash or no `xt` at all. Both the v1 info hash and
    /// `ut_metadata` are BEP 3-era machinery; a v2-only magnet needs the v2
    /// metadata path, which does not exist yet.
    pub fn from_magnet(
        link: &MagnetLink,
        peer_id: [u8; 20],
        port: u16,
    ) -> Result<Self, MagnetError> {
        let info_hash = link.info_hash.ok_or(MagnetError::NoInfoHash)?;
        Ok(Self {
            trackers: link.trackers.clone(),
            ..Self::new(info_hash, peer_id, port)
        })
    }
}

/// A verified torrent produced by [`fetch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// The raw info dictionary bytes, exactly as the peer sent them.
    pub info: Vec<u8>,
    /// A complete `.torrent` whose info bytes are `info`, so its hash matches.
    pub torrent: Vec<u8>,
}

/// Best-effort progress from a running fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchEvent {
    /// How many distinct peers will be tried.
    Peers(usize),
    /// One metadata piece was stored; `total` is the number of pieces.
    Progress {
        /// Zero-based index of the piece just stored.
        piece: u32,
        /// Number of pieces the metadata is split into.
        total: u32,
    },
    /// The metadata was verified; `bytes` is its size.
    Done {
        /// Length of the info dictionary.
        bytes: usize,
    },
}

/// Why a metadata fetch failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MagnetError {
    /// The magnet link has no v1 info hash, so there is nothing to verify.
    #[error("magnet link carries no v1 info hash")]
    NoInfoHash,
    /// Neither seeds nor trackers produced a single usable peer.
    #[error("no usable peers for this torrent")]
    NoPeers,
    /// The assembled info dictionary does not hash to the expected value.
    #[error("metadata hash mismatch: expected {expected:02x?}..., got {got:02x?}...")]
    HashMismatch {
        /// The magnet's info hash.
        expected: [u8; 20],
        /// SHA-1 of the bytes the peer served.
        got: [u8; 20],
    },
    /// A peer or its handshake announced metadata above
    /// [`MAX_METADATA_SIZE`](flux_net::metadata::MAX_METADATA_SIZE).
    #[error("metadata is larger than the 4 MiB limit")]
    MetadataTooLarge,
    /// The tracker could not be used or answered with an error.
    #[error("tracker error: {0}")]
    Tracker(String),
    /// A peer failed in transport (connect, handshake, disconnect).
    #[error("peer error: {0}")]
    Peer(String),
    /// The overall budget expired.
    #[error("metadata fetch timed out")]
    Timeout,
    /// The peer refused to serve its metadata.
    #[error("peer rejected the metadata request")]
    Rejected,
    /// The peer sent metadata messages that cannot be interpreted coherently.
    #[error("metadata protocol error: {0}")]
    Protocol(String),
}

/// Fetches the info dictionary for `spec.info_hash` and returns it verified.
///
/// Seeds are used first; when they are empty the trackers are announced once
/// with `event=started` and `left=0` and their peers are used. Nothing is
/// invented: with no seeds and no trackers this fails with
/// [`MagnetError::NoPeers`] — there is no DHT in this build.
///
/// # Errors
///
/// See [`MagnetError`]. The returned metadata is always hash-verified; the
/// hash is never skipped.
pub async fn fetch(
    spec: FetchSpec,
    events: Option<mpsc::Sender<FetchEvent>>,
) -> Result<Metadata, MagnetError> {
    if spec.timeout.is_zero() {
        return Err(MagnetError::Timeout);
    }
    match timeout(spec.timeout, fetch_inner(spec, events)).await {
        Ok(result) => result,
        Err(_) => Err(MagnetError::Timeout),
    }
}

/// The body of [`fetch`], already bounded by the caller's timeout.
async fn fetch_inner(
    spec: FetchSpec,
    events: Option<mpsc::Sender<FetchEvent>>,
) -> Result<Metadata, MagnetError> {
    let peers = collect_peers(&spec).await?;
    emit(&events, FetchEvent::Peers(peers.len()));

    let deadline = Instant::now() + spec.timeout;
    let mut last = None;
    for (index, addr) in peers.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let left = peers.len() - index;
        let budget = remaining / u32::try_from(left).unwrap_or(u32::MAX);
        if budget.is_zero() {
            last = Some(AttemptFailure::Timeout);
            continue;
        }

        match attempt_peer(*addr, &spec, &events, budget).await {
            Ok(info) => {
                let metadata = verify(info, &spec)?;
                emit(
                    &events,
                    FetchEvent::Done {
                        bytes: metadata.info.len(),
                    },
                );
                return Ok(metadata);
            }
            // A rejected request, oversized metadata, inconsistent messages or
            // a hash mismatch say something about the swarm itself; trying
            // another peer would not make the answer true.
            Err(AttemptFailure::Fatal(error)) => return Err(error),
            Err(failure) => last = Some(failure),
        }
    }

    match last {
        Some(AttemptFailure::Peer(message)) => Err(MagnetError::Peer(message)),
        Some(AttemptFailure::Fatal(error)) => Err(error),
        _ => Err(MagnetError::Timeout),
    }
}

/// Resolves the peer list: seeds when present, otherwise one announce.
async fn collect_peers(spec: &FetchSpec) -> Result<Vec<SocketAddr>, MagnetError> {
    let raw = if spec.seeds.is_empty() {
        if spec.trackers.is_empty() {
            return Err(MagnetError::NoPeers);
        }
        announce(spec).await?
    } else {
        // Seeds are explicit and local; announcing as well would add a network
        // dependency to an otherwise offline fetch.
        spec.seeds.clone()
    };

    let peers = dedupe_peers(raw, spec.max_peers);
    if peers.is_empty() {
        return Err(MagnetError::NoPeers);
    }
    Ok(peers)
}

/// Announces `Started` once and returns every peer the tracker named.
async fn announce(spec: &FetchSpec) -> Result<Vec<SocketAddr>, MagnetError> {
    let tiers: Vec<Vec<String>> = spec.trackers.iter().map(|url| vec![url.clone()]).collect();
    let mut announcer = Announcer::new(tiers, AnnouncerConfig::default())
        .map_err(|err| MagnetError::Tracker(err.to_string()))?;
    let request = AnnounceRequest {
        info_hash: spec.info_hash,
        peer_id: spec.peer_id,
        port: spec.port,
        left: 0,
        event: Some(Event::Started),
        num_want: Some(i32::try_from(spec.max_peers).unwrap_or(i32::MAX)),
        ..AnnounceRequest::default()
    };
    let outcome = announcer
        .announce(&request, Duration::ZERO)
        .await
        .map_err(|err| MagnetError::Tracker(err.to_string()))?;
    Ok(outcome.response.peers)
}

/// Keeps the first `max` distinct, dialable addresses in list order.
fn dedupe_peers(addrs: impl IntoIterator<Item = SocketAddr>, max: usize) -> Vec<SocketAddr> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for addr in addrs {
        if out.len() >= max {
            break;
        }
        if addr.port() == 0 || addr.ip().is_unspecified() {
            continue;
        }
        if seen.insert(addr) {
            out.push(addr);
        }
    }
    out
}

/// A failed attempt at one peer.
#[derive(Debug)]
enum AttemptFailure {
    /// This peer's share of the budget ran out.
    Timeout,
    /// This peer is unusable; the next one may work.
    Peer(String),
    /// The swarm, not the peer, gave an answer that ends the fetch.
    Fatal(MagnetError),
}

/// Dials one peer, asks for the metadata, and always closes the connection.
async fn attempt_peer(
    addr: SocketAddr,
    spec: &FetchSpec,
    events: &Option<mpsc::Sender<FetchEvent>>,
    budget: Duration,
) -> Result<Vec<u8>, AttemptFailure> {
    let deadline = Instant::now() + budget;
    let mut config = PeerConfig::new(spec.info_hash, spec.peer_id);
    // See the module docs: zero is "unknown" for a metadata fetch, so an
    // inbound bitfield is acknowledged instead of validated.
    config.pieces = 0;
    // Our own deadline governs the whole attempt, including the handshake, so
    // the transport's idle timer must not race it to the same instant.
    config.idle_timeout = Duration::ZERO;
    config.keepalive = Duration::ZERO;

    let (handle, mut from_peer) = match timeout_at(deadline, peer::connect(addr, config)).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(err)) => {
            return Err(AttemptFailure::Peer(format!(
                "cannot connect to {addr}: {err}"
            )));
        }
        Err(_) => return Err(AttemptFailure::Timeout),
    };

    let result = exchange(&handle, &mut from_peer, events, deadline).await;

    // Ordered close when the queue has room; dropping the handle closes the
    // session either way, so this can never block the fetch.
    if handle.try_send(PeerCommand::Close).is_err() {
        handle.close();
    }
    result
}

/// Speaks BEP 9 over one connection until the metadata is complete.
async fn exchange(
    handle: &PeerHandle,
    from_peer: &mut mpsc::Receiver<PeerEvent>,
    events: &Option<mpsc::Sender<FetchEvent>>,
    deadline: Instant,
) -> Result<Vec<u8>, AttemptFailure> {
    send_extended(
        handle,
        EXTENDED_HANDSHAKE_ID,
        our_extended_handshake(),
        deadline,
    )
    .await?;

    let mut their_id = None;
    let mut declared = None;
    let mut assembly: Option<Assembly> = None;
    let mut next_request = 0u32;
    let mut in_flight = 0u32;

    loop {
        let event = match timeout_at(deadline, from_peer.recv()).await {
            Err(_) => return Err(AttemptFailure::Timeout),
            Ok(None) => {
                return Err(AttemptFailure::Peer(
                    "peer closed the connection without an answer".to_owned(),
                ));
            }
            Ok(Some(event)) => event,
        };

        match event {
            PeerEvent::Extended {
                id: EXTENDED_HANDSHAKE_ID,
                payload,
            } => {
                // A repeated handshake cannot retroactively change the id we
                // have already been requesting with.
                if their_id.is_some() {
                    continue;
                }
                let (id, size) = parse_extended_handshake(&payload)?;
                if let Some(size) = size {
                    if size > MAX_METADATA_SIZE {
                        return Err(AttemptFailure::Fatal(MagnetError::MetadataTooLarge));
                    }
                    declared = Some(size);
                }
                their_id = Some(id);
                // Ask for piece 0 only: its `total_size` tells us the shape.
                send_request(handle, id, 0, deadline).await?;
                in_flight = 1;
                next_request = 1;
            }
            PeerEvent::Extended { id, payload } => {
                let Some(their_id) = their_id else {
                    continue;
                };
                if id != their_id {
                    continue;
                }
                let message = match MetadataMessage::decode(&payload) {
                    Ok(message) => message,
                    Err(MetadataError::InvalidTotalSize { .. }) => {
                        return Err(AttemptFailure::Fatal(MagnetError::MetadataTooLarge));
                    }
                    Err(err) => {
                        return Err(AttemptFailure::Fatal(MagnetError::Protocol(format!(
                            "invalid ut_metadata message: {err}"
                        ))));
                    }
                };

                match message {
                    // We never ask the peer for anything but data; a request
                    // or handshake is ignored rather than answered.
                    MetadataMessage::Request { .. } => {}
                    MetadataMessage::Reject { .. } => {
                        return Err(AttemptFailure::Fatal(MagnetError::Rejected));
                    }
                    MetadataMessage::Data {
                        piece,
                        total_size,
                        data,
                    } => {
                        // Decoding already enforces the 4 MiB cap and the
                        // piece length; the check here is the explicit refusal
                        // the API promises, and it runs before any buffer of
                        // that size exists.
                        if total_size > MAX_METADATA_SIZE {
                            return Err(AttemptFailure::Fatal(MagnetError::MetadataTooLarge));
                        }
                        if assembly.is_none() {
                            if let Some(declared) = declared
                                && declared != total_size
                            {
                                return Err(AttemptFailure::Fatal(MagnetError::Protocol(format!(
                                    "handshake declared {declared} bytes, data claims {total_size}"
                                ))));
                            }
                            assembly = Some(Assembly::new(total_size));
                        }
                        let Some(state) = assembly.as_mut() else {
                            return Err(AttemptFailure::Fatal(MagnetError::Protocol(
                                "metadata assembly disappeared".to_owned(),
                            )));
                        };
                        state.put(piece, total_size, &data)?;
                        in_flight = in_flight.saturating_sub(1);
                        emit(
                            events,
                            FetchEvent::Progress {
                                piece,
                                total: state.piece_count(),
                            },
                        );
                        if state.is_complete() {
                            let Some(done) = assembly.take() else {
                                return Err(AttemptFailure::Fatal(MagnetError::Protocol(
                                    "metadata assembly disappeared".to_owned(),
                                )));
                            };
                            return Ok(done.into_bytes());
                        }
                        let total = state.piece_count();
                        while in_flight < MAX_PIECES_IN_FLIGHT && next_request < total {
                            send_request(handle, their_id, next_request, deadline).await?;
                            in_flight += 1;
                            next_request += 1;
                        }
                    }
                }
            }
            PeerEvent::Closed(reason) => {
                return Err(AttemptFailure::Peer(format!(
                    "peer closed the connection ({reason:?})"
                )));
            }
            // Bitfields, haves, choking and blocks carry nothing a metadata
            // fetch needs.
            _ => {}
        }
    }
}

/// Sends one BEP 10 message, bounded by the attempt deadline.
async fn send_extended(
    handle: &PeerHandle,
    id: u8,
    payload: Vec<u8>,
    deadline: Instant,
) -> Result<(), AttemptFailure> {
    match timeout_at(deadline, handle.send(PeerCommand::Extended { id, payload })).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(AttemptFailure::Peer(format!("send failed: {err}"))),
        Err(_) => Err(AttemptFailure::Timeout),
    }
}

/// Sends a `ut_metadata` request for one piece.
async fn send_request(
    handle: &PeerHandle,
    id: u8,
    piece: u32,
    deadline: Instant,
) -> Result<(), AttemptFailure> {
    let payload = MetadataMessage::Request { piece }.encode();
    send_extended(handle, id, payload, deadline).await
}

/// Our BEP 10 handshake: `m.ut_metadata` and a client version string.
fn our_extended_handshake() -> Vec<u8> {
    bencode::encode(&Value::Dict(vec![
        (
            b"m".to_vec(),
            Value::Dict(vec![(
                b"ut_metadata".to_vec(),
                Value::Int(i64::from(OUR_UT_METADATA_ID)),
            )]),
        ),
        (
            b"v".to_vec(),
            Value::Bytes(
                concat!("Oxide Flux ", env!("CARGO_PKG_VERSION"))
                    .as_bytes()
                    .to_vec(),
            ),
        ),
    ]))
}

/// Reads the peer's `ut_metadata` id and optional `metadata_size`.
fn parse_extended_handshake(payload: &[u8]) -> Result<(u8, Option<u32>), AttemptFailure> {
    let value = bencode::decode(payload)
        .map_err(|err| AttemptFailure::Peer(format!("malformed extended handshake: {err}")))?;
    let Some(extension_map) = value.get(b"m").and_then(Value::as_dict) else {
        return Err(AttemptFailure::Peer(
            "extended handshake has no `m` dictionary".to_owned(),
        ));
    };
    let id = extension_map
        .iter()
        .find(|(key, _)| key.as_slice() == b"ut_metadata")
        .and_then(|(_, value)| value.as_int())
        .and_then(|value| u8::try_from(value).ok())
        .filter(|id| *id != EXTENDED_HANDSHAKE_ID)
        .ok_or_else(|| AttemptFailure::Peer("peer does not support ut_metadata".to_owned()))?;

    let size = match value.get(b"metadata_size") {
        None => None,
        Some(Value::Int(number)) => {
            let size = u32::try_from(*number)
                .ok()
                .filter(|size| *size > 0)
                .ok_or_else(|| {
                    AttemptFailure::Peer("peer declared an invalid metadata_size".to_owned())
                })?;
            Some(size)
        }
        Some(_) => {
            return Err(AttemptFailure::Peer(
                "metadata_size is not an integer".to_owned(),
            ));
        }
    };
    Ok((id, size))
}

/// Reassembles metadata pieces into one contiguous blob.
#[derive(Debug)]
struct Assembly {
    total: u32,
    bytes: Vec<u8>,
    have: Vec<bool>,
    received: u32,
}

impl Assembly {
    /// A buffer for `total` bytes. The caller validated `1..=MAX_METADATA_SIZE`.
    fn new(total: u32) -> Self {
        let pieces = metadata::piece_count(total);
        Self {
            total,
            bytes: vec![0; total as usize],
            have: vec![false; pieces as usize],
            received: 0,
        }
    }

    fn piece_count(&self) -> u32 {
        metadata::piece_count(self.total)
    }

    fn is_complete(&self) -> bool {
        self.received == self.piece_count()
    }

    /// Copies one piece in, rejecting duplicates and inconsistencies.
    fn put(&mut self, piece: u32, total_size: u32, data: &[u8]) -> Result<(), AttemptFailure> {
        if total_size != self.total {
            return Err(AttemptFailure::Fatal(MagnetError::Protocol(format!(
                "metadata size changed from {} to {total_size}",
                self.total
            ))));
        }
        let pieces = self.piece_count();
        if piece >= pieces {
            return Err(AttemptFailure::Fatal(MagnetError::Protocol(format!(
                "metadata piece {piece} is outside 0..{pieces}"
            ))));
        }
        let start = (piece as usize).saturating_mul(METADATA_PIECE_SIZE as usize);
        let expected = (self.total as usize)
            .saturating_sub(start)
            .min(METADATA_PIECE_SIZE as usize);
        if data.len() != expected {
            return Err(AttemptFailure::Fatal(MagnetError::Protocol(format!(
                "metadata piece {piece} carries {} bytes, expected {expected}",
                data.len()
            ))));
        }
        let index = piece as usize;
        if self.have.get(index).copied().unwrap_or(true) {
            return Err(AttemptFailure::Fatal(MagnetError::Protocol(format!(
                "metadata piece {piece} was sent twice"
            ))));
        }
        let Some(target) = self.bytes.get_mut(start..start + expected) else {
            return Err(AttemptFailure::Fatal(MagnetError::Protocol(
                "metadata piece outside the assembly buffer".to_owned(),
            )));
        };
        target.copy_from_slice(data);
        if let Some(slot) = self.have.get_mut(index) {
            *slot = true;
        }
        self.received = self.received.saturating_add(1);
        Ok(())
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Verifies the info bytes and wraps them in a complete torrent.
fn verify(info: Vec<u8>, spec: &FetchSpec) -> Result<Metadata, MagnetError> {
    let torrent = assemble_torrent(&info, &spec.trackers);
    let parsed = metainfo::parse(&torrent)
        .map_err(|err| MagnetError::Protocol(format!("metadata is not a valid torrent: {err}")))?;
    let got = parsed
        .info_hash()
        .ok_or_else(|| MagnetError::Protocol("metadata carries no v1 info hash".to_owned()))?;
    if got != spec.info_hash {
        return Err(MagnetError::HashMismatch {
            expected: spec.info_hash,
            got,
        });
    }
    Ok(Metadata { info, torrent })
}

/// Delivers an event if somebody is still listening.
fn emit(events: &Option<mpsc::Sender<FetchEvent>>, event: FetchEvent) {
    if let Some(sender) = events {
        let _ = sender.try_send(event);
    }
}

/// Builds the `.torrent` bytes from the raw info dict and the magnet's trackers.
///
/// The result is a canonical bencode dictionary with the keys in order
/// (`announce` < `announce-list` < `info`). The info bytes are appended
/// verbatim — never re-encoded — so the info hash of the result is exactly the
/// SHA-1 of `info`. Each tracker becomes its own one-entry tier in
/// `announce-list`; empty tracker strings are skipped, and when nothing is left
/// both announce keys are omitted.
#[must_use]
pub fn assemble_torrent(info: &[u8], trackers: &[String]) -> Vec<u8> {
    let trackers: Vec<&str> = trackers
        .iter()
        .map(String::as_str)
        .filter(|tracker| !tracker.trim().is_empty())
        .collect();

    let trackers_len: usize = trackers.iter().map(|tracker| tracker.len() + 8).sum();
    let mut out = Vec::with_capacity(info.len() + trackers_len + 64);
    out.push(b'd');

    if let Some(first) = trackers.first() {
        out.extend_from_slice(b"8:announce");
        push_bencode_bytes(first.as_bytes(), &mut out);
        out.extend_from_slice(b"13:announce-listl");
        for tracker in &trackers {
            out.push(b'l');
            push_bencode_bytes(tracker.as_bytes(), &mut out);
            out.push(b'e');
        }
        out.push(b'e');
    }

    out.extend_from_slice(b"4:info");
    out.extend_from_slice(info);
    out.push(b'e');
    out
}

/// Appends one length-prefixed bencode byte string.
fn push_bencode_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use sha1::{Digest, Sha1};

    use super::*;

    fn sha1(bytes: &[u8]) -> [u8; 20] {
        let mut hasher = Sha1::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut out = [0_u8; 20];
        out.copy_from_slice(digest.as_slice());
        out
    }

    /// A small, valid v1 info dictionary.
    fn info_dict(name: &str) -> Vec<u8> {
        let length = 256usize;
        let piece_length = 32u32;
        let hashes: Vec<u8> = (0..length.div_ceil(piece_length as usize))
            .flat_map(|index| sha1(&(index as u64).to_le_bytes()))
            .collect();
        bencode::encode(&Value::Dict(vec![
            (b"length".to_vec(), Value::Int(length as i64)),
            (b"name".to_vec(), Value::Bytes(name.as_bytes().to_vec())),
            (
                b"piece length".to_vec(),
                Value::Int(i64::from(piece_length)),
            ),
            (b"pieces".to_vec(), Value::Bytes(hashes)),
        ]))
    }

    fn assert_key_order(torrent: &[u8]) {
        let announce = torrent
            .windows(b"8:announce".len())
            .position(|window| window == b"8:announce")
            .expect("announce key");
        let list = torrent
            .windows(b"13:announce-list".len())
            .position(|window| window == b"13:announce-list")
            .expect("announce-list key");
        let info = torrent
            .windows(b"4:info".len())
            .position(|window| window == b"4:info")
            .expect("info key");
        assert!(announce < list, "announce must come before announce-list");
        assert!(list < info, "announce-list must come before info");
    }

    #[test]
    fn assemble_without_trackers_omits_announce_keys() {
        let info = info_dict("no-trackers");
        let torrent = assemble_torrent(&info, &[]);

        let parsed = metainfo::parse(&torrent).expect("assembled torrent must parse");
        assert_eq!(parsed.info_hash(), Some(sha1(&info)));
        assert!(parsed.announce.is_none());
        assert!(parsed.announce_list.is_empty());

        assert!(!torrent.windows(8).any(|window| window == b"announce"));
        // The whole body is `d4:info<info>e`.
        assert_eq!(torrent.first(), Some(&b'd'));
        assert_eq!(torrent.last(), Some(&b'e'));
        assert_eq!(&torrent[1..7], b"4:info");
        assert_eq!(&torrent[7..7 + info.len()], info.as_slice());
    }

    #[test]
    fn assemble_with_many_trackers_sorts_keys_and_preserves_hash() {
        let info = info_dict("multi");
        let trackers = vec![
            "http://tracker.example/announce".to_owned(),
            "udp://tracker.example:6969/announce".to_owned(),
        ];
        let torrent = assemble_torrent(&info, &trackers);

        assert_key_order(&torrent);
        let parsed = metainfo::parse(&torrent).expect("assembled torrent must parse");
        assert_eq!(parsed.info_hash(), Some(sha1(&info)));
        assert_eq!(parsed.announce.as_deref(), Some(trackers[0].as_str()));
        assert_eq!(parsed.announce_list.len(), 2);
        assert_eq!(parsed.announce_list[0], vec![trackers[0].clone()]);
        assert_eq!(parsed.announce_list[1], vec![trackers[1].clone()]);

        // The decoder only accepts sorted, unique keys, so a successful parse
        // already proves order; check the dictionary explicitly as well.
        let value = bencode::decode(&torrent).expect("canonical bencode");
        let entries = value.as_dict().expect("dictionary");
        let keys: Vec<&[u8]> = entries.iter().map(|(key, _)| key.as_slice()).collect();
        let expected: Vec<&[u8]> = vec![b"announce", b"announce-list", b"info"];
        assert_eq!(keys, expected);
    }

    #[test]
    fn assemble_skips_empty_trackers() {
        let info = info_dict("blank");
        let torrent = assemble_torrent(&info, &["".to_owned(), "   ".to_owned()]);
        let parsed = metainfo::parse(&torrent).expect("assembled torrent must parse");
        assert!(parsed.announce.is_none());
        assert!(parsed.announce_list.is_empty());
    }

    #[test]
    fn dedupe_keeps_order_and_respects_the_cap() {
        let a: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:2".parse().unwrap();
        let dead: SocketAddr = "0.0.0.0:3".parse().unwrap();
        let zero: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let peers = dedupe_peers([a, b, a, dead, zero], 4);
        assert_eq!(peers, vec![a, b]);
        assert_eq!(dedupe_peers([a, b], 1), vec![a]);
    }
}
