//! Mainline DHT (BEP 5): KRPC, routing table, peer discovery.
//!
//! This module is a self-contained IPv4 DHT node: it owns one UDP socket, one
//! routing table and one peer store, and it speaks the four mainline queries
//! (`ping`, `find_node`, `get_peers`, `announce_peer`). Everything is KRPC
//! dictionaries encoded and decoded with [`flux_core::bencode`]; there is no
//! second bencode parser here.
//!
//! ```text
//! query   { t, y: "q", q: <method>, a: { id, ... } }
//! reply   { t, y: "r", r: { id, nodes?: <26-byte entries>, values?: [<6-byte entries>], token?: <bytes> } }
//! error   { t, y: "e", e: [ code, message ] }
//! ```
//!
//! ## Scope
//!
//! - IPv4 only. [`DhtConfig::bind`] must be an IPv4 address; IPv6 bootstrap
//!   addresses and peers are skipped. BEP 32 `nodes6`/`values6` are absent.
//! - No BEP 42 node-id security extension, no persistence, no automatic
//!   routing-table refresh and no `sample_infohashes`. The node id is fresh
//!   random bytes for every [`start`].
//! - Private torrents must never reach this module. The daemon is responsible
//!   for not calling [`DhtHandle::get_peers`] or [`DhtHandle::announce_peer`]
//!   for a torrent whose metainfo says `private: 1`; there is no per-torrent
//!   flag here to get wrong.
//!
//! ## Shape
//!
//! One task owns the socket, the routing table and the peer store. Public
//! handles send commands over a fixed-capacity channel and wait on `oneshot`
//! replies; at most 64 lookups run at once, and reply/event channels are
//! bounded too. Events are dropped, never awaited, if the consumer falls
//! behind.
//!
//! ## Routing policy
//!
//! 160 buckets of shared prefix length (index = number of leading equal bits),
//! [`BUCKET_SIZE`] = 8 nodes each, plus a global [`DhtConfig::max_nodes`] cap.
//! Our own id is never inserted.
//!
//! 1. If the bucket already holds the id, refresh its address and timestamp.
//! 2. If the global cap is reached, the new node is dropped. No cross-bucket
//!    eviction is attempted, so a full table stays stable.
//! 3. If the bucket is full, the least-recently-seen occupant is pinged. It is
//!    evicted **only if that ping fails** (timeout, error reply or send error);
//!    if it answers, the new node is dropped. While one such verification is in
//!    flight for a bucket, further candidates for that bucket are dropped.
//!
//! ## Lookup policy
//!
//! `alpha` = 3 concurrent `get_peers` per wave, at most [`MAX_LOOKUP_ROUNDS`]
//! waves, candidates sorted by XOR distance to the target. A wave ends when
//! every query in it has answered, timed out or errored. The lookup stops when
//! no unqueried node remains among the closest [`LOOKUP_K`] known candidates,
//! or when the wave cap is hit. `values` are deduplicated and capped at
//! [`MAX_VALUES`] addresses.
//!
//! ## Tokens
//!
//! `announce_peer` needs a token from a preceding `get_peers`. Tokens are eight
//! bytes of a keyed 64-bit mix of a secret and the requester's IPv4 address,
//! rotating every [`TOKEN_ROTATE`] (5 min); the previous secret is still
//! accepted, so a token lives at most ten minutes. This is a replay/DoS barrier
//! only — not authentication, and deliberately not HMAC. A bad token is
//! ignored silently: no error reply and no stored peer.
//!
//! ## Peer store
//!
//! At most [`MAX_PEER_HASHES`] info hashes, [`MAX_PEERS_PER_HASH`] addresses per
//! hash, each entry living [`PEER_TTL`] (30 min). When the hash cap is reached
//! the oldest hash is dropped; a full per-hash list drops its oldest entry.
//! `get_peers` for an unknown or expired hash returns compact `nodes` instead.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use flux_core::bencode::{self, Limits, Value};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};

/// Bytes in a DHT node id (160 bits).
pub const NODE_ID_LEN: usize = 20;

/// A 160-bit DHT node id or torrent info hash.
pub type NodeId = [u8; NODE_ID_LEN];

/// Nodes per routing bucket (`k` in the Kademlia paper).
pub const BUCKET_SIZE: usize = 8;

/// Lookups run this many queries concurrently per wave.
pub const ALPHA: usize = 3;

/// Hard cap on lookup waves.
pub const MAX_LOOKUP_ROUNDS: usize = 8;

/// A lookup stops once the closest this many known nodes have been queried.
pub const LOOKUP_K: usize = 8;

/// Maximum number of peer addresses a single `get_peers` returns.
pub const MAX_VALUES: usize = 200;

/// Maximum number of info hashes in the peer store.
pub const MAX_PEER_HASHES: usize = 2000;

/// Maximum number of peer addresses stored per info hash.
pub const MAX_PEERS_PER_HASH: usize = 50;

/// How long an announced peer is stored.
pub const PEER_TTL: Duration = Duration::from_secs(30 * 60);

/// How often tokens rotate. The previous generation stays valid.
pub const TOKEN_ROTATE: Duration = Duration::from_secs(5 * 60);

/// Default per-query wait: 4 s, as in BEP 5 deployments.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(4);

/// Default routing-table cap.
pub const DEFAULT_MAX_NODES: usize = 1000;

/// Commands the handle may queue before it waits for the node task.
const COMMAND_BUFFER: usize = 32;

/// Events the node may queue before dropping new ones.
const EVENT_BUFFER: usize = 64;

/// Concurrent iterative lookups. Callers beyond this get an empty result.
const MAX_OPS: usize = 64;

/// Known nodes a lookup may remember (distance-sorted, bounded).
const MAX_CANDIDATES: usize = 256;

/// One UDP datagram buffer. `nodes` replies are at most 208 bytes, `values`
/// replies are capped on receipt, so anything larger is malformed or hostile.
const MAX_PACKET_BYTES: usize = 4096;

/// Strict bencode limits for KRPC. Real messages are a few hundred bytes.
const KRPC_LIMITS: Limits = Limits::new(4096, 8, MAX_PACKET_BYTES);

/// Bytes in a compact IPv4 node entry: address + port + id.
const COMPACT_NODE_LEN: usize = 4 + 2 + NODE_ID_LEN;

/// Bytes in a compact IPv4 peer entry: address + port.
const COMPACT_PEER_LEN: usize = 4 + 2;

/// Longest transaction id we echo or accept. Ours are four bytes.
const MAX_TID_LEN: usize = 32;

/// How long a wake-up with no outstanding queries waits before rechecking.
const IDLE_TICK: Duration = Duration::from_secs(60);

/// Configuration for [`start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtConfig {
    /// Local UDP address to bind. Must be IPv4; normally `0.0.0.0:0`.
    pub bind: SocketAddr,
    /// Mainline routers to contact at startup. Empty in tests.
    pub bootstrap: Vec<SocketAddr>,
    /// Per-query wait before a query counts as timed out.
    pub query_timeout: Duration,
    /// Routing-table cap, across all 160 buckets.
    pub max_nodes: usize,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            bootstrap: Vec::new(),
            query_timeout: DEFAULT_QUERY_TIMEOUT,
            max_nodes: DEFAULT_MAX_NODES,
        }
    }
}

/// A snapshot of node counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DhtStats {
    /// Nodes currently in the routing table.
    pub nodes: usize,
    /// KRPC queries successfully written to the socket.
    pub queries_sent: u64,
    /// Valid replies matched to an outstanding query.
    pub responses: u64,
    /// Outstanding queries that expired, mismatched or errored.
    pub timeouts: u64,
}

/// Notifications from the node task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DhtEvent {
    /// A `get_peers` lookup finished with at least one address.
    Peers {
        /// The info hash that was looked up.
        info_hash: NodeId,
        /// Discovered peer addresses, deduplicated.
        peers: Vec<SocketAddr>,
    },
    /// A new node entered the routing table (not a refresh).
    NodeAdded(SocketAddr),
    /// A node failed verification, a send failed, or another non-fatal fault.
    Failed(String),
}

/// Failures from [`start`].
#[derive(Debug, Error)]
pub enum DhtError {
    /// The UDP socket could not be bound.
    #[error("dht could not bind {addr}: {source}")]
    Bind {
        /// Address that was requested.
        addr: SocketAddr,
        /// Underlying socket error.
        #[source]
        source: io::Error,
    },
    /// Only IPv4 is supported.
    #[error("dht supports IPv4 only, got {0}")]
    UnsupportedFamily(SocketAddr),
    /// The bound socket did not report a local address.
    #[error("dht could not read its local address: {0}")]
    LocalAddress(#[source] io::Error),
}

/// The public handle to a running node.
///
/// Cloning is cheap; every clone shares the same bounded command channel.
#[derive(Debug, Clone)]
pub struct DhtHandle {
    commands: mpsc::Sender<Command>,
    local_addr: SocketAddr,
}

impl DhtHandle {
    /// The UDP address this node actually bound.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Adds a seed node and waits until it has answered or timed out.
    ///
    /// The probe is a `find_node` for our own id, so a reply inserts the seed
    /// and up to eight of its neighbours. IPv6 addresses are ignored.
    pub async fn add_node(&self, addr: SocketAddr) {
        let (reply, wait) = oneshot::channel();
        if self
            .commands
            .send(Command::AddNode { addr, reply })
            .await
            .is_err()
        {
            return;
        }
        let _ = wait.await;
    }

    /// Pings one address and reports whether a valid reply arrived.
    ///
    /// This is an added convenience over the BEP 5 client API; it is also what
    /// the routing table uses for liveness checks internally.
    pub async fn ping(&self, addr: SocketAddr) -> bool {
        let (reply, wait) = oneshot::channel();
        if self
            .commands
            .send(Command::Ping { addr, reply })
            .await
            .is_err()
        {
            return false;
        }
        wait.await.unwrap_or(false)
    }

    /// Runs an iterative `get_peers` lookup for `info_hash`.
    ///
    /// Returns deduplicated addresses, capped at [`MAX_VALUES`]. An empty
    /// result means no peer was found (or the node could not answer).
    pub async fn get_peers(&self, info_hash: NodeId) -> Vec<SocketAddr> {
        let (reply, wait) = oneshot::channel();
        if self
            .commands
            .send(Command::GetPeers { info_hash, reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        wait.await.unwrap_or_default()
    }

    /// Announces a TCP port for `info_hash` and returns how many nodes stored it.
    ///
    /// The lookup first obtains a write token with `get_peers`, then sends
    /// `announce_peer` with `implied_port = 0` to the closest token holders.
    /// Only valid `id` replies are counted.
    pub async fn announce_peer(&self, info_hash: NodeId, port: u16) -> usize {
        let (reply, wait) = oneshot::channel();
        if self
            .commands
            .send(Command::AnnouncePeer {
                info_hash,
                port,
                reply,
            })
            .await
            .is_err()
        {
            return 0;
        }
        wait.await.unwrap_or(0)
    }

    /// Returns a counter snapshot.
    pub async fn stats(&self) -> DhtStats {
        let (reply, wait) = oneshot::channel();
        if self.commands.send(Command::Stats { reply }).await.is_err() {
            return DhtStats::default();
        }
        wait.await.unwrap_or_default()
    }
}

/// Starts a node: binds the socket, spawns the owning task and returns a handle
/// plus the event receiver.
///
/// The event channel is bounded; a slow consumer loses events rather than
/// blocking the node.
pub async fn start(config: DhtConfig) -> Result<(DhtHandle, mpsc::Receiver<DhtEvent>), DhtError> {
    if !config.bind.is_ipv4() {
        return Err(DhtError::UnsupportedFamily(config.bind));
    }
    let socket = UdpSocket::bind(config.bind)
        .await
        .map_err(|source| DhtError::Bind {
            addr: config.bind,
            source,
        })?;
    let local_addr = socket.local_addr().map_err(DhtError::LocalAddress)?;

    let (command_tx, command_rx) = mpsc::channel(COMMAND_BUFFER);
    let (event_tx, event_rx) = mpsc::channel(EVENT_BUFFER);

    let our_id = random_id();
    let now = Instant::now();
    let table = RoutingTable::new(our_id, config.max_nodes);
    let node = Node {
        our_id,
        config,
        table,
        peers: PeerStore::new(PEER_TTL),
        tokens: TokenManager::new(now),
        queries: HashMap::new(),
        seeds_inflight: HashSet::new(),
        evictions: HashMap::new(),
        ops: HashMap::new(),
        next_op: 0,
        next_tid: 0,
        events: event_tx,
        stats: DhtStats::default(),
    };

    let handle = DhtHandle {
        commands: command_tx,
        local_addr,
    };
    tokio::spawn(node.run(socket, command_rx));
    Ok((handle, event_rx))
}

/// Commands accepted by the node task.
enum Command {
    AddNode {
        addr: SocketAddr,
        reply: oneshot::Sender<()>,
    },
    Ping {
        addr: SocketAddr,
        reply: oneshot::Sender<bool>,
    },
    GetPeers {
        info_hash: NodeId,
        reply: oneshot::Sender<Vec<SocketAddr>>,
    },
    AnnouncePeer {
        info_hash: NodeId,
        port: u16,
        reply: oneshot::Sender<usize>,
    },
    Stats {
        reply: oneshot::Sender<DhtStats>,
    },
}

/// Why an outstanding query was sent.
enum QueryPurpose {
    Seed {
        reply: Option<oneshot::Sender<()>>,
    },
    Ping {
        reply: Option<oneshot::Sender<bool>>,
        eviction: Option<usize>,
    },
    Lookup {
        op: u64,
    },
    Announce {
        op: u64,
    },
}

/// A query awaiting a reply.
struct Query {
    addr: SocketAddr,
    expected_id: Option<NodeId>,
    purpose: QueryPurpose,
    deadline: Instant,
}

/// A candidate a lookup has learned about. Ids are unknown only for seed
/// addresses handed to us by the caller.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    id: Option<NodeId>,
    addr: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    GetPeers,
    Announce { port: u16 },
}

enum OpReply {
    Peers(oneshot::Sender<Vec<SocketAddr>>),
    Announced(oneshot::Sender<usize>),
}

enum OpPhase {
    Lookup,
    Announcing { pending: u32, ok: usize },
}

/// One iterative lookup or announce.
struct Op {
    kind: OpKind,
    target: NodeId,
    candidates: Vec<Candidate>,
    queried: HashSet<SocketAddr>,
    active: u32,
    round: usize,
    values: Vec<SocketAddr>,
    tokens: Vec<(NodeId, SocketAddr, Vec<u8>)>,
    phase: OpPhase,
    reply: OpReply,
}

impl Op {
    fn new(kind: OpKind, target: NodeId, reply: OpReply) -> Self {
        Self {
            kind,
            target,
            candidates: Vec::new(),
            queried: HashSet::new(),
            active: 0,
            round: 0,
            values: Vec::new(),
            tokens: Vec::new(),
            phase: OpPhase::Lookup,
            reply,
        }
    }

    fn sort_candidates(&mut self) {
        let target = self.target;
        self.candidates
            .sort_by(|left, right| match (left.id, right.id) {
                (Some(left_id), Some(right_id)) => {
                    xor_distance(&target, &left_id).cmp(&xor_distance(&target, &right_id))
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
    }

    fn should_continue(&self) -> bool {
        self.candidates
            .iter()
            .take(LOOKUP_K)
            .any(|candidate| !self.queried.contains(&candidate.addr))
    }

    fn remember_node(&mut self, id: NodeId, addr: SocketAddr) {
        if self.candidates.len() >= MAX_CANDIDATES {
            return;
        }
        if self.queried.contains(&addr) || self.candidates.iter().any(|known| known.addr == addr) {
            return;
        }
        self.candidates.push(Candidate { id: Some(id), addr });
    }

    fn remember_values(&mut self, values: impl IntoIterator<Item = SocketAddr>) {
        for addr in values {
            if self.values.len() >= MAX_VALUES {
                return;
            }
            if !self.values.contains(&addr) {
                self.values.push(addr);
            }
        }
    }

    fn remember_token(&mut self, id: NodeId, addr: SocketAddr, token: Vec<u8>) {
        if self.tokens.iter().any(|(_, known, _)| *known == addr) {
            return;
        }
        self.tokens.push((id, addr, token));
    }
}

enum PumpDecision {
    Finish,
    Send(Vec<(SocketAddr, Option<NodeId>)>),
}

/// Verification ping for a full bucket.
struct PendingEviction {
    old_id: NodeId,
    old_addr: SocketAddr,
    candidate_id: NodeId,
    candidate_addr: SocketAddr,
}

/// The task that owns the socket and all mutable DHT state.
struct Node {
    our_id: NodeId,
    config: DhtConfig,
    table: RoutingTable,
    peers: PeerStore,
    tokens: TokenManager,
    queries: HashMap<[u8; 4], Query>,
    seeds_inflight: HashSet<SocketAddr>,
    evictions: HashMap<usize, PendingEviction>,
    ops: HashMap<u64, Op>,
    next_op: u64,
    next_tid: u32,
    events: mpsc::Sender<DhtEvent>,
    stats: DhtStats,
}

impl Node {
    async fn run(mut self, socket: UdpSocket, mut commands: mpsc::Receiver<Command>) {
        let mut buffer = vec![0u8; MAX_PACKET_BYTES];
        let now = Instant::now();
        for addr in self.config.bootstrap.clone() {
            self.add_seed(&socket, addr, None, now).await;
        }

        loop {
            let now = Instant::now();
            let deadline = self.next_deadline(now);
            tokio::select! {
                biased;
                command = commands.recv() => {
                    match command {
                        Some(command) => self.on_command(&socket, command, Instant::now()).await,
                        None => break,
                    }
                }
                received = socket.recv_from(&mut buffer) => {
                    if let Ok((len, src)) = received {
                        self.on_packet(&socket, &buffer[..len], src, Instant::now()).await;
                    }
                }
                _ = sleep_until(deadline) => {
                    self.on_timer(&socket, Instant::now()).await;
                }
            }
        }
    }

    fn next_deadline(&self, now: Instant) -> Instant {
        self.queries
            .values()
            .map(|query| query.deadline)
            .min()
            .unwrap_or(now + IDLE_TICK)
    }

    fn query_timeout(&self) -> Duration {
        self.config.query_timeout.max(Duration::from_millis(1))
    }

    fn emit(&self, event: DhtEvent) {
        let _ = self.events.try_send(event);
    }

    async fn on_command(&mut self, socket: &UdpSocket, command: Command, now: Instant) {
        match command {
            Command::AddNode { addr, reply } => {
                self.add_seed(socket, addr, Some(reply), now).await;
            }
            Command::Ping { addr, reply } => {
                if !addr.is_ipv4() {
                    let _ = reply.send(false);
                    return;
                }
                let expected = self.table.id_for_addr(addr);
                let tid = self.allocate_tid();
                let message = krpc_query(&tid, &self.our_id, b"ping", Vec::new());
                let purpose = QueryPurpose::Ping {
                    reply: Some(reply),
                    eviction: None,
                };
                let _ = self
                    .send_query(socket, addr, tid, message, expected, purpose, now)
                    .await;
            }
            Command::GetPeers { info_hash, reply } => {
                if self.ops.len() >= MAX_OPS {
                    self.emit(DhtEvent::Failed(format!(
                        "dht lookup capacity reached, dropping lookup for {}",
                        hex(&info_hash)
                    )));
                    let _ = reply.send(Vec::new());
                    return;
                }
                self.start_op(
                    socket,
                    OpKind::GetPeers,
                    info_hash,
                    OpReply::Peers(reply),
                    now,
                )
                .await;
            }
            Command::AnnouncePeer {
                info_hash,
                port,
                reply,
            } => {
                if self.ops.len() >= MAX_OPS {
                    self.emit(DhtEvent::Failed(format!(
                        "dht lookup capacity reached, dropping announce for {}",
                        hex(&info_hash)
                    )));
                    let _ = reply.send(0);
                    return;
                }
                self.start_op(
                    socket,
                    OpKind::Announce { port },
                    info_hash,
                    OpReply::Announced(reply),
                    now,
                )
                .await;
            }
            Command::Stats { reply } => {
                let mut stats = self.stats;
                stats.nodes = self.table.len();
                let _ = reply.send(stats);
            }
        }
    }

    async fn add_seed(
        &mut self,
        socket: &UdpSocket,
        addr: SocketAddr,
        reply: Option<oneshot::Sender<()>>,
        now: Instant,
    ) {
        if !addr.is_ipv4() {
            self.emit(DhtEvent::Failed(format!(
                "ignoring IPv6 dht seed {addr}: IPv4 only"
            )));
            if let Some(reply) = reply {
                let _ = reply.send(());
            }
            return;
        }
        if !self.seeds_inflight.insert(addr) {
            if let Some(reply) = reply {
                let _ = reply.send(());
            }
            return;
        }
        let tid = self.allocate_tid();
        let message = krpc_query(
            &tid,
            &self.our_id,
            b"find_node",
            vec![(b"target".to_vec(), Value::Bytes(self.our_id.to_vec()))],
        );
        let sent = self
            .send_query(
                socket,
                addr,
                tid,
                message,
                None,
                QueryPurpose::Seed { reply },
                now,
            )
            .await;
        if !sent {
            self.seeds_inflight.remove(&addr);
            // Dropping `QueryPurpose::Seed` closes its reply channel; the
            // waiting `add_node` then returns.
            self.emit(DhtEvent::Failed(format!(
                "could not send find_node to {addr}"
            )));
        }
    }

    fn allocate_tid(&mut self) -> [u8; 4] {
        loop {
            self.next_tid = self.next_tid.wrapping_add(1);
            let tid = self.next_tid.to_be_bytes();
            if !self.queries.contains_key(&tid) {
                return tid;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_query(
        &mut self,
        socket: &UdpSocket,
        addr: SocketAddr,
        tid: [u8; 4],
        message: Value,
        expected_id: Option<NodeId>,
        purpose: QueryPurpose,
        now: Instant,
    ) -> bool {
        let bytes = bencode::encode(&message);
        if socket.send_to(&bytes, addr).await.is_err() {
            return false;
        }
        self.stats.queries_sent += 1;
        self.queries.insert(
            tid,
            Query {
                addr,
                expected_id,
                purpose,
                deadline: now + self.query_timeout(),
            },
        );
        true
    }

    async fn on_packet(&mut self, socket: &UdpSocket, data: &[u8], src: SocketAddr, now: Instant) {
        if !src.is_ipv4() {
            return;
        }
        let Ok(value) = bencode::decode_with_limits(data, &KRPC_LIMITS) else {
            return;
        };
        let Value::Dict(_) = &value else {
            return;
        };
        let Some(tid) = value.get(b"t").and_then(Value::as_bytes) else {
            return;
        };
        if tid.is_empty() || tid.len() > MAX_TID_LEN {
            return;
        }
        let Some(kind) = value.get(b"y").and_then(Value::as_bytes) else {
            return;
        };
        match kind {
            b"r" => self.on_response(socket, tid, src, &value, now).await,
            b"q" => self.on_query(socket, tid, src, &value, now).await,
            b"e" => self.on_error(socket, tid, src, now).await,
            _ => {}
        }
    }

    async fn on_response(
        &mut self,
        socket: &UdpSocket,
        tid: &[u8],
        src: SocketAddr,
        value: &Value,
        now: Instant,
    ) {
        let Ok(tid) = <[u8; 4]>::try_from(tid) else {
            return;
        };
        let Some(query) = self.queries.get(&tid) else {
            return;
        };
        let Some(response) = value.get(b"r") else {
            return;
        };
        let Some(response_id) = response.get(b"id").and_then(|id| node_id(id.as_bytes()?)) else {
            return;
        };
        if !response_is_valid(query, src, &response_id) {
            return;
        }
        let Some(query) = self.queries.remove(&tid) else {
            return;
        };
        if matches!(query.purpose, QueryPurpose::Seed { .. }) {
            self.seeds_inflight.remove(&query.addr);
        }
        self.stats.responses += 1;

        self.consider_node(socket, response_id, src, now).await;

        match query.purpose {
            QueryPurpose::Seed { reply } => {
                for (id, addr) in response
                    .get(b"nodes")
                    .and_then(Value::as_bytes)
                    .map_or_else(Vec::new, parse_compact_nodes)
                {
                    self.consider_node(socket, id, addr, now).await;
                }
                if let Some(reply) = reply {
                    let _ = reply.send(());
                }
            }
            QueryPurpose::Ping { reply, eviction } => {
                if let Some(bucket) = eviction {
                    self.eviction_alive(bucket, now);
                }
                if let Some(reply) = reply {
                    let _ = reply.send(true);
                }
            }
            QueryPurpose::Lookup { op } => {
                let nodes = response
                    .get(b"nodes")
                    .and_then(Value::as_bytes)
                    .map_or_else(Vec::new, parse_compact_nodes);
                let values = response
                    .get(b"values")
                    .map(parse_compact_peers)
                    .unwrap_or_default();
                let token = response
                    .get(b"token")
                    .and_then(Value::as_bytes)
                    .filter(|token| !token.is_empty() && token.len() <= MAX_TID_LEN)
                    .map(<[u8]>::to_vec);
                self.on_lookup_response(socket, op, response_id, src, nodes, values, token, now)
                    .await;
            }
            QueryPurpose::Announce { op } => {
                self.on_announce_response(op);
            }
        }
    }

    async fn on_error(&mut self, socket: &UdpSocket, tid: &[u8], src: SocketAddr, now: Instant) {
        let Ok(tid) = <[u8; 4]>::try_from(tid) else {
            return;
        };
        let Some(query) = self.queries.get(&tid) else {
            return;
        };
        if query.addr != src {
            return;
        }
        let Some(query) = self.queries.remove(&tid) else {
            return;
        };
        if matches!(query.purpose, QueryPurpose::Seed { .. }) {
            self.seeds_inflight.remove(&query.addr);
        }
        self.stats.timeouts += 1;
        self.on_query_failure(socket, query, now).await;
    }

    async fn on_query(
        &mut self,
        socket: &UdpSocket,
        tid: &[u8],
        src: SocketAddr,
        value: &Value,
        now: Instant,
    ) {
        let Some(args) = value.get(b"a") else {
            return;
        };
        let Some(id) = args.get(b"id").and_then(|id| node_id(id.as_bytes()?)) else {
            return;
        };
        let Some(method) = value.get(b"q").and_then(Value::as_bytes) else {
            return;
        };

        let mut extra: Vec<(Vec<u8>, Value)> = Vec::new();
        match method {
            b"ping" => {}
            b"find_node" => {
                let Some(target) = args.get(b"target").and_then(|value| {
                    let bytes = value.as_bytes()?;
                    <[u8; NODE_ID_LEN]>::try_from(bytes).ok()
                }) else {
                    return;
                };
                let nodes = self.table.closest(&target, BUCKET_SIZE);
                extra.push((b"nodes".to_vec(), Value::Bytes(compact_nodes(&nodes))));
            }
            b"get_peers" => {
                let Some(info_hash) = args.get(b"info_hash").and_then(|value| {
                    let bytes = value.as_bytes()?;
                    <[u8; NODE_ID_LEN]>::try_from(bytes).ok()
                }) else {
                    return;
                };
                let token = self.tokens.token_for(src.ip(), now);
                extra.push((b"token".to_vec(), Value::Bytes(token.to_vec())));
                let peers = self.peers.get(&info_hash, now);
                if peers.is_empty() {
                    let nodes = self.table.closest(&info_hash, BUCKET_SIZE);
                    extra.push((b"nodes".to_vec(), Value::Bytes(compact_nodes(&nodes))));
                } else {
                    extra.push((
                        b"values".to_vec(),
                        Value::List(
                            peers
                                .into_iter()
                                .map(|addr| Value::Bytes(compact_peer(addr)))
                                .collect(),
                        ),
                    ));
                }
            }
            b"announce_peer" => {
                let Some(info_hash) = args.get(b"info_hash").and_then(|value| {
                    let bytes = value.as_bytes()?;
                    <[u8; NODE_ID_LEN]>::try_from(bytes).ok()
                }) else {
                    return;
                };
                let Some(token) = args.get(b"token").and_then(Value::as_bytes) else {
                    return;
                };
                let Some(port) = args.get(b"port").and_then(Value::as_int) else {
                    return;
                };
                let Ok(port) = u16::try_from(port) else {
                    return;
                };
                if !self.tokens.valid(src.ip(), token, now) {
                    // Deliberately silent: a bad token is a DoS attempt, not a
                    // protocol error worth an amplified reply.
                    return;
                }
                let implied = args
                    .get(b"implied_port")
                    .and_then(Value::as_int)
                    .is_some_and(|implied| implied != 0);
                let peer_port = if implied { src.port() } else { port };
                if peer_port == 0 {
                    return;
                }
                self.peers
                    .insert(info_hash, SocketAddr::new(src.ip(), peer_port), now);
            }
            _ => return,
        }

        let response = krpc_response(tid, &self.our_id, extra);
        let _ = socket.send_to(&bencode::encode(&response), src).await;
        self.consider_node(socket, id, src, now).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn on_lookup_response(
        &mut self,
        socket: &UdpSocket,
        op_id: u64,
        responder_id: NodeId,
        src: SocketAddr,
        nodes: Vec<(NodeId, SocketAddr)>,
        values: Vec<SocketAddr>,
        token: Option<Vec<u8>>,
        now: Instant,
    ) {
        let mut pump = false;
        let mut finish_announce = false;
        {
            let Some(op) = self.ops.get_mut(&op_id) else {
                return;
            };
            match &mut op.phase {
                OpPhase::Lookup => {
                    op.active = op.active.saturating_sub(1);
                    for (id, addr) in &nodes {
                        if *id == self.our_id {
                            continue;
                        }
                        op.remember_node(*id, *addr);
                    }
                    op.remember_values(values);
                    if let Some(token) = token {
                        op.remember_token(responder_id, src, token);
                    }
                    pump = true;
                }
                OpPhase::Announcing { pending, ok } => {
                    *pending = pending.saturating_sub(1);
                    *ok += 1;
                    finish_announce = *pending == 0;
                }
            }
        }
        for (id, addr) in nodes {
            self.consider_node(socket, id, addr, now).await;
        }
        if pump {
            self.pump(socket, op_id, now).await;
        }
        if finish_announce {
            self.finish_announce(op_id);
        }
    }

    async fn on_timer(&mut self, socket: &UdpSocket, now: Instant) {
        let expired: Vec<[u8; 4]> = self
            .queries
            .iter()
            .filter(|(_, query)| query.deadline <= now)
            .map(|(tid, _)| *tid)
            .collect();
        for tid in expired {
            let Some(query) = self.queries.remove(&tid) else {
                continue;
            };
            if matches!(query.purpose, QueryPurpose::Seed { .. }) {
                self.seeds_inflight.remove(&query.addr);
            }
            self.stats.timeouts += 1;
            self.on_query_failure(socket, query, now).await;
        }
    }

    async fn on_query_failure(&mut self, socket: &UdpSocket, query: Query, now: Instant) {
        match query.purpose {
            QueryPurpose::Seed { reply } => {
                if let Some(reply) = reply {
                    let _ = reply.send(());
                }
            }
            QueryPurpose::Ping { reply, eviction } => {
                if let Some(reply) = reply {
                    let _ = reply.send(false);
                }
                if let Some(bucket) = eviction {
                    self.eviction_failed(bucket, now);
                }
            }
            QueryPurpose::Lookup { op } => {
                if let Some(state) = self.ops.get_mut(&op) {
                    state.active = state.active.saturating_sub(1);
                }
                self.pump(socket, op, now).await;
            }
            QueryPurpose::Announce { op } => {
                if decrement_announce(&mut self.ops, op) {
                    self.finish_announce(op);
                }
            }
        }
    }

    async fn consider_node(
        &mut self,
        socket: &UdpSocket,
        id: NodeId,
        addr: SocketAddr,
        now: Instant,
    ) {
        if id == self.our_id || !addr.is_ipv4() {
            return;
        }
        match self.table.insert(id, addr, now) {
            InsertOutcome::Added => self.emit(DhtEvent::NodeAdded(addr)),
            InsertOutcome::Refreshed => {}
            InsertOutcome::OwnId | InsertOutcome::AtCapacity => {}
            InsertOutcome::BucketFull {
                bucket,
                oldest_id,
                oldest_addr,
            } => {
                if self.evictions.contains_key(&bucket) {
                    return;
                }
                let pending = PendingEviction {
                    old_id: oldest_id,
                    old_addr: oldest_addr,
                    candidate_id: id,
                    candidate_addr: addr,
                };
                self.evictions.insert(bucket, pending);
                let tid = self.allocate_tid();
                let message = krpc_query(&tid, &self.our_id, b"ping", Vec::new());
                let sent = self
                    .send_query(
                        socket,
                        oldest_addr,
                        tid,
                        message,
                        Some(oldest_id),
                        QueryPurpose::Ping {
                            reply: None,
                            eviction: Some(bucket),
                        },
                        now,
                    )
                    .await;
                if !sent {
                    self.eviction_failed(bucket, now);
                }
            }
        }
    }

    fn eviction_alive(&mut self, bucket: usize, _now: Instant) {
        // The ping succeeded, so the occupant stays. `consider_node` refreshed
        // its timestamp before this runs.
        self.evictions.remove(&bucket);
    }

    fn eviction_failed(&mut self, bucket: usize, now: Instant) {
        let Some(pending) = self.evictions.remove(&bucket) else {
            return;
        };
        self.emit(DhtEvent::Failed(format!(
            "dht node {} failed verification and was evicted",
            pending.old_addr
        )));
        if self.table.replace(
            bucket,
            pending.old_id,
            pending.candidate_id,
            pending.candidate_addr,
            now,
        ) {
            self.emit(DhtEvent::NodeAdded(pending.candidate_addr));
        }
    }

    async fn start_op(
        &mut self,
        socket: &UdpSocket,
        kind: OpKind,
        target: NodeId,
        reply: OpReply,
        now: Instant,
    ) {
        let op_id = self.next_op;
        self.next_op = self.next_op.wrapping_add(1);
        let mut op = Op::new(kind, target, reply);
        for (id, addr) in self.table.closest(&target, BUCKET_SIZE) {
            op.candidates.push(Candidate { id: Some(id), addr });
        }
        // Bootstrap addresses are queried whenever the table is too small to
        // fill a wave on its own.
        if op.candidates.len() < ALPHA {
            for addr in self.config.bootstrap.clone() {
                if !addr.is_ipv4() || op.candidates.iter().any(|candidate| candidate.addr == addr) {
                    continue;
                }
                op.candidates.push(Candidate { id: None, addr });
            }
        }
        if op.candidates.is_empty() {
            Self::finish_empty(op);
            return;
        }
        self.ops.insert(op_id, op);
        self.pump(socket, op_id, now).await;
    }

    fn finish_empty(op: Op) {
        match op.reply {
            OpReply::Peers(reply) => {
                let _ = reply.send(Vec::new());
            }
            OpReply::Announced(reply) => {
                let _ = reply.send(0);
            }
        }
    }

    async fn pump(&mut self, socket: &UdpSocket, op_id: u64, now: Instant) {
        loop {
            let decision = match self.ops.get_mut(&op_id) {
                None => return,
                Some(op) if !matches!(op.phase, OpPhase::Lookup) || op.active > 0 => return,
                Some(op) => {
                    if op.round >= MAX_LOOKUP_ROUNDS || !op.should_continue() {
                        PumpDecision::Finish
                    } else {
                        op.round += 1;
                        op.sort_candidates();
                        let mut chosen = Vec::new();
                        for candidate in &op.candidates {
                            if chosen.len() >= ALPHA {
                                break;
                            }
                            if op.queried.insert(candidate.addr) {
                                chosen.push((candidate.addr, candidate.id));
                            }
                        }
                        if chosen.is_empty() {
                            PumpDecision::Finish
                        } else {
                            op.active = chosen.len() as u32;
                            PumpDecision::Send(chosen)
                        }
                    }
                }
            };

            match decision {
                PumpDecision::Finish => {
                    self.finish_lookup(socket, op_id, now).await;
                    return;
                }
                PumpDecision::Send(chosen) => {
                    let mut failed = 0u32;
                    for (addr, expected) in chosen {
                        if !self.send_lookup(socket, op_id, addr, expected, now).await {
                            failed += 1;
                        }
                    }
                    if failed == 0 {
                        return;
                    }
                    let Some(op) = self.ops.get_mut(&op_id) else {
                        return;
                    };
                    op.active = op.active.saturating_sub(failed);
                    if op.active > 0 {
                        return;
                    }
                    // Every send failed; try the next wave instead of stalling.
                }
            }
        }
    }

    async fn send_lookup(
        &mut self,
        socket: &UdpSocket,
        op_id: u64,
        addr: SocketAddr,
        expected: Option<NodeId>,
        now: Instant,
    ) -> bool {
        let Some(op) = self.ops.get(&op_id) else {
            return false;
        };
        let target = op.target;
        let tid = self.allocate_tid();
        let message = krpc_query(
            &tid,
            &self.our_id,
            b"get_peers",
            vec![(b"info_hash".to_vec(), Value::Bytes(target.to_vec()))],
        );
        self.send_query(
            socket,
            addr,
            tid,
            message,
            expected,
            QueryPurpose::Lookup { op: op_id },
            now,
        )
        .await
    }

    async fn finish_lookup(&mut self, socket: &UdpSocket, op_id: u64, now: Instant) {
        let Some(op) = self.ops.remove(&op_id) else {
            return;
        };
        let Op {
            kind,
            target,
            values,
            mut tokens,
            reply,
            ..
        } = op;
        match reply {
            OpReply::Peers(reply) => {
                if !values.is_empty() {
                    self.emit(DhtEvent::Peers {
                        info_hash: target,
                        peers: values.clone(),
                    });
                }
                let _ = reply.send(values);
            }
            OpReply::Announced(reply) => {
                tokens.sort_by(|(left, _, _), (right, _, _)| {
                    xor_distance(&target, left).cmp(&xor_distance(&target, right))
                });
                let tokens: Vec<(NodeId, SocketAddr, Vec<u8>)> =
                    tokens.into_iter().take(BUCKET_SIZE).collect();
                if tokens.is_empty() {
                    let _ = reply.send(0);
                    return;
                }
                let total = tokens.len() as u32;
                let op = Op {
                    kind,
                    target,
                    candidates: Vec::new(),
                    queried: HashSet::new(),
                    active: 0,
                    round: 0,
                    values: Vec::new(),
                    tokens: Vec::new(),
                    phase: OpPhase::Announcing {
                        pending: total,
                        ok: 0,
                    },
                    reply: OpReply::Announced(reply),
                };
                self.ops.insert(op_id, op);

                let mut failed = 0u32;
                for (id, addr, token) in tokens {
                    if !self
                        .send_announce(socket, op_id, id, addr, token, now)
                        .await
                    {
                        failed += 1;
                    }
                }
                let finished = match self.ops.get_mut(&op_id) {
                    Some(op) => match &mut op.phase {
                        OpPhase::Announcing { pending, .. } => {
                            *pending = pending.saturating_sub(failed);
                            *pending == 0
                        }
                        OpPhase::Lookup => false,
                    },
                    None => true,
                };
                if finished {
                    self.finish_announce(op_id);
                }
            }
        }
    }

    async fn send_announce(
        &mut self,
        socket: &UdpSocket,
        op_id: u64,
        id: NodeId,
        addr: SocketAddr,
        token: Vec<u8>,
        now: Instant,
    ) -> bool {
        let (port, info_hash) = {
            let Some(op) = self.ops.get(&op_id) else {
                return false;
            };
            let OpKind::Announce { port } = op.kind else {
                return false;
            };
            (port, op.target)
        };
        let tid = self.allocate_tid();
        let message = krpc_query(
            &tid,
            &self.our_id,
            b"announce_peer",
            vec![
                (b"implied_port".to_vec(), Value::Int(0)),
                (b"info_hash".to_vec(), Value::Bytes(info_hash.to_vec())),
                (b"port".to_vec(), Value::Int(i64::from(port))),
                (b"token".to_vec(), Value::Bytes(token)),
            ],
        );
        self.send_query(
            socket,
            addr,
            tid,
            message,
            Some(id),
            QueryPurpose::Announce { op: op_id },
            now,
        )
        .await
    }

    fn on_announce_response(&mut self, op_id: u64) {
        let finish = match self.ops.get_mut(&op_id) {
            Some(op) => match &mut op.phase {
                OpPhase::Announcing { pending, ok } => {
                    *pending = pending.saturating_sub(1);
                    *ok += 1;
                    *pending == 0
                }
                OpPhase::Lookup => false,
            },
            None => false,
        };
        if finish {
            self.finish_announce(op_id);
        }
    }

    fn finish_announce(&mut self, op_id: u64) {
        let Some(op) = self.ops.remove(&op_id) else {
            return;
        };
        let OpReply::Announced(reply) = op.reply else {
            return;
        };
        let ok = match op.phase {
            OpPhase::Announcing { ok, .. } => ok,
            OpPhase::Lookup => 0,
        };
        let _ = reply.send(ok);
    }
}

/// Validates a reply against the query it claims to answer.
fn response_is_valid(query: &Query, src: SocketAddr, id: &NodeId) -> bool {
    query.addr == src && query.expected_id.is_none_or(|expected| expected == *id)
}

fn decrement_announce(ops: &mut HashMap<u64, Op>, op_id: u64) -> bool {
    match ops.get_mut(&op_id) {
        Some(op) => match &mut op.phase {
            OpPhase::Announcing { pending, .. } => {
                *pending = pending.saturating_sub(1);
                *pending == 0
            }
            OpPhase::Lookup => false,
        },
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Routing table
// ---------------------------------------------------------------------------

/// A routing-table entry.
#[derive(Debug, Clone, Copy)]
struct TableNode {
    id: NodeId,
    addr: SocketAddr,
    last_seen: Instant,
}

/// What [`RoutingTable::insert`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsertOutcome {
    Added,
    Refreshed,
    OwnId,
    AtCapacity,
    BucketFull {
        bucket: usize,
        oldest_id: NodeId,
        oldest_addr: SocketAddr,
    },
}

/// 160 prefix buckets, each holding at most [`BUCKET_SIZE`] nodes.
struct RoutingTable {
    our_id: NodeId,
    buckets: Box<[Vec<TableNode>; 160]>,
    total: usize,
    max_nodes: usize,
}

impl RoutingTable {
    fn new(our_id: NodeId, max_nodes: usize) -> Self {
        Self {
            our_id,
            buckets: Box::new(std::array::from_fn(|_| Vec::new())),
            total: 0,
            max_nodes,
        }
    }

    fn len(&self) -> usize {
        self.total
    }

    fn insert(&mut self, id: NodeId, addr: SocketAddr, now: Instant) -> InsertOutcome {
        if id == self.our_id {
            return InsertOutcome::OwnId;
        }
        let Some(bucket) = bucket_index(&self.our_id, &id) else {
            return InsertOutcome::OwnId;
        };
        if let Some(node) = self.buckets[bucket].iter_mut().find(|node| node.id == id) {
            node.addr = addr;
            node.last_seen = now;
            self.buckets[bucket].sort_by_key(|node| node.last_seen);
            return InsertOutcome::Refreshed;
        }
        if self.total >= self.max_nodes {
            return InsertOutcome::AtCapacity;
        }
        if self.buckets[bucket].len() >= BUCKET_SIZE {
            let oldest = self.buckets[bucket]
                .iter()
                .min_by_key(|node| node.last_seen)
                .copied();
            if let Some(oldest) = oldest {
                return InsertOutcome::BucketFull {
                    bucket,
                    oldest_id: oldest.id,
                    oldest_addr: oldest.addr,
                };
            }
        }
        self.buckets[bucket].push(TableNode {
            id,
            addr,
            last_seen: now,
        });
        self.buckets[bucket].sort_by_key(|node| node.last_seen);
        self.total += 1;
        InsertOutcome::Added
    }

    /// Removes `old_id` from `bucket` and inserts the candidate that replaced
    /// it after a failed liveness ping. Returns whether the swap happened.
    fn replace(
        &mut self,
        bucket: usize,
        old_id: NodeId,
        id: NodeId,
        addr: SocketAddr,
        now: Instant,
    ) -> bool {
        if id == self.our_id {
            return false;
        }
        let Some(bucket_nodes) = self.buckets.get_mut(bucket) else {
            return false;
        };
        let Some(position) = bucket_nodes.iter().position(|node| node.id == old_id) else {
            return false;
        };
        let old = bucket_nodes.remove(position);
        self.total = self.total.saturating_sub(1);
        if self.total >= self.max_nodes {
            // Restore the old occupant; nothing may be inserted.
            bucket_nodes.push(old);
            bucket_nodes.sort_by_key(|node| node.last_seen);
            self.total += 1;
            return false;
        }
        self.buckets[bucket].push(TableNode {
            id,
            addr,
            last_seen: now,
        });
        self.buckets[bucket].sort_by_key(|node| node.last_seen);
        self.total += 1;
        true
    }

    fn id_for_addr(&self, addr: SocketAddr) -> Option<NodeId> {
        self.buckets
            .iter()
            .flatten()
            .find(|node| node.addr == addr)
            .map(|node| node.id)
    }

    /// Up to `count` nodes closest to `target` by XOR distance.
    fn closest(&self, target: &NodeId, count: usize) -> Vec<(NodeId, SocketAddr)> {
        let mut nodes: Vec<&TableNode> = self.buckets.iter().flatten().collect();
        nodes.sort_by_key(|node| xor_distance(target, &node.id));
        nodes
            .into_iter()
            .take(count)
            .map(|node| (node.id, node.addr))
            .collect()
    }
}

/// The bucket index for `id`: the bit position of the first differing bit.
fn bucket_index(ours: &NodeId, id: &NodeId) -> Option<usize> {
    let mut zeros = 0usize;
    for (left, right) in ours.iter().zip(id.iter()) {
        let xor = left ^ right;
        if xor != 0 {
            return Some(zeros + xor.leading_zeros() as usize);
        }
        zeros += 8;
    }
    None
}

fn xor_distance(left: &NodeId, right: &NodeId) -> NodeId {
    let mut distance = [0u8; NODE_ID_LEN];
    for (index, byte) in distance.iter_mut().enumerate() {
        *byte = left[index] ^ right[index];
    }
    distance
}

// ---------------------------------------------------------------------------
// Peer store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct PeerEntry {
    addr: SocketAddr,
    expires: Instant,
}

/// Bounded, expiring announced-peer table.
struct PeerStore {
    ttl: Duration,
    hashes: HashMap<NodeId, VecDeque<PeerEntry>>,
    order: VecDeque<NodeId>,
}

impl PeerStore {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            hashes: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    #[cfg(test)]
    fn hash_count(&self) -> usize {
        self.hashes.len()
    }

    fn insert(&mut self, info_hash: NodeId, addr: SocketAddr, now: Instant) {
        if !self.hashes.contains_key(&info_hash) {
            while self.hashes.len() >= MAX_PEER_HASHES {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                self.hashes.remove(&oldest);
            }
            self.hashes.insert(info_hash, VecDeque::new());
            self.order.push_back(info_hash);
        }
        let Some(peers) = self.hashes.get_mut(&info_hash) else {
            return;
        };
        peers.retain(|peer| peer.expires > now);
        if let Some(existing) = peers.iter_mut().find(|peer| peer.addr == addr) {
            existing.expires = now + self.ttl;
            return;
        }
        while peers.len() >= MAX_PEERS_PER_HASH {
            peers.pop_front();
        }
        peers.push_back(PeerEntry {
            addr,
            expires: now + self.ttl,
        });
    }

    fn get(&mut self, info_hash: &NodeId, now: Instant) -> Vec<SocketAddr> {
        let Some(peers) = self.hashes.get_mut(info_hash) else {
            return Vec::new();
        };
        peers.retain(|peer| peer.expires > now);
        peers.iter().map(|peer| peer.addr).collect()
    }
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Rotating token secret. A token is a keyed mix of the secret and the peer's
/// IPv4 address; it is a replay barrier, not authentication.
struct TokenManager {
    current: [u8; 8],
    previous: [u8; 8],
    rotated: Instant,
}

impl TokenManager {
    fn new(now: Instant) -> Self {
        Self {
            current: random_secret(),
            previous: random_secret(),
            rotated: now,
        }
    }

    fn rotate(&mut self, now: Instant) {
        let mut elapsed = now.checked_duration_since(self.rotated).unwrap_or_default();
        while elapsed >= TOKEN_ROTATE {
            self.previous = self.current;
            self.current = random_secret();
            self.rotated += TOKEN_ROTATE;
            elapsed = now.checked_duration_since(self.rotated).unwrap_or_default();
        }
    }

    fn token_for(&mut self, ip: IpAddr, now: Instant) -> [u8; 8] {
        self.rotate(now);
        token_bytes(&self.current, ip)
    }

    fn valid(&mut self, ip: IpAddr, token: &[u8], now: Instant) -> bool {
        self.rotate(now);
        token == token_bytes(&self.current, ip) || token == token_bytes(&self.previous, ip)
    }
}

fn token_bytes(secret: &[u8; 8], ip: IpAddr) -> [u8; 8] {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let octets = match ip {
        IpAddr::V4(ip) => ip.octets().to_vec(),
        IpAddr::V6(ip) => ip.octets().to_vec(),
    };
    for byte in secret.iter().chain(octets.iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // A final avalanche so neighboring addresses do not produce neighboring
    // tokens. Still not a MAC: it only needs to be unguessable enough to make
    // blind announce spam expensive.
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    hash.to_be_bytes()
}

// ---------------------------------------------------------------------------
// KRPC helpers
// ---------------------------------------------------------------------------

fn krpc_query(tid: &[u8], our_id: &NodeId, method: &[u8], args: Vec<(Vec<u8>, Value)>) -> Value {
    let mut fields = Vec::with_capacity(args.len() + 1);
    fields.push((b"id".to_vec(), Value::Bytes(our_id.to_vec())));
    fields.extend(args);
    Value::Dict(vec![
        (b"a".to_vec(), Value::Dict(fields)),
        (b"q".to_vec(), Value::Bytes(method.to_vec())),
        (b"t".to_vec(), Value::Bytes(tid.to_vec())),
        (b"y".to_vec(), Value::Bytes(b"q".to_vec())),
    ])
}

fn krpc_response(tid: &[u8], our_id: &NodeId, extra: Vec<(Vec<u8>, Value)>) -> Value {
    let mut fields = Vec::with_capacity(extra.len() + 1);
    fields.push((b"id".to_vec(), Value::Bytes(our_id.to_vec())));
    fields.extend(extra);
    Value::Dict(vec![
        (b"r".to_vec(), Value::Dict(fields)),
        (b"t".to_vec(), Value::Bytes(tid.to_vec())),
        (b"y".to_vec(), Value::Bytes(b"r".to_vec())),
    ])
}

fn node_id(bytes: &[u8]) -> Option<NodeId> {
    <[u8; NODE_ID_LEN]>::try_from(bytes).ok()
}

fn parse_compact_nodes(bytes: &[u8]) -> Vec<(NodeId, SocketAddr)> {
    bytes
        .as_chunks::<COMPACT_NODE_LEN>()
        .0
        .iter()
        .filter_map(|entry| {
            let ip = Ipv4Addr::new(entry[0], entry[1], entry[2], entry[3]);
            let port = u16::from_be_bytes([entry[4], entry[5]]);
            let id = node_id(&entry[6..])?;
            if port == 0 || ip.is_unspecified() {
                return None;
            }
            Some((id, SocketAddr::from((ip, port))))
        })
        .collect()
}

fn parse_compact_peers(value: &Value) -> Vec<SocketAddr> {
    let mut peers = Vec::new();
    match value {
        Value::List(values) => {
            for value in values {
                if let Some(bytes) = value.as_bytes() {
                    parse_compact_peer_bytes(bytes, &mut peers);
                }
            }
        }
        // Some implementations put one binary blob in `values`.
        Value::Bytes(bytes) => parse_compact_peer_bytes(bytes, &mut peers),
        _ => {}
    }
    peers
}

fn parse_compact_peer_bytes(bytes: &[u8], peers: &mut Vec<SocketAddr>) {
    for entry in bytes.as_chunks::<COMPACT_PEER_LEN>().0 {
        let ip = Ipv4Addr::new(entry[0], entry[1], entry[2], entry[3]);
        let port = u16::from_be_bytes([entry[4], entry[5]]);
        if port == 0 || ip.is_unspecified() {
            continue;
        }
        peers.push(SocketAddr::from((ip, port)));
    }
}

fn compact_nodes(nodes: &[(NodeId, SocketAddr)]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(nodes.len() * COMPACT_NODE_LEN);
    for (id, addr) in nodes {
        let SocketAddr::V4(addr) = addr else {
            continue;
        };
        bytes.extend_from_slice(&addr.ip().octets());
        bytes.extend_from_slice(&addr.port().to_be_bytes());
        bytes.extend_from_slice(id);
    }
    bytes
}

fn compact_peer(addr: SocketAddr) -> Vec<u8> {
    match addr {
        SocketAddr::V4(addr) => {
            let mut bytes = Vec::with_capacity(COMPACT_PEER_LEN);
            bytes.extend_from_slice(&addr.ip().octets());
            bytes.extend_from_slice(&addr.port().to_be_bytes());
            bytes
        }
        // The node is IPv4 only; an IPv6 address cannot be encoded.
        SocketAddr::V6(_) => Vec::new(),
    }
}

fn random_id() -> NodeId {
    let mut id = [0u8; NODE_ID_LEN];
    for chunk in id.chunks_mut(4) {
        let bytes = fastrand::u32(..).to_be_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    id
}

fn random_secret() -> [u8; 8] {
    let mut secret = [0u8; 8];
    secret[..4].copy_from_slice(&fastrand::u32(..).to_be_bytes());
    secret[4..].copy_from_slice(&fastrand::u32(..).to_be_bytes());
    secret
}

fn hex(id: &NodeId) -> String {
    let mut text = String::with_capacity(NODE_ID_LEN * 2);
    for byte in id {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn node_id_with(first: u8) -> NodeId {
        let mut id = [0u8; NODE_ID_LEN];
        id[0] = first;
        id
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn config(timeout_ms: u64) -> DhtConfig {
        DhtConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            bootstrap: Vec::new(),
            query_timeout: Duration::from_millis(timeout_ms),
            max_nodes: 1000,
        }
    }

    #[test]
    fn buckets_follow_shared_prefix_length() {
        let ours = [0u8; NODE_ID_LEN];
        assert_eq!(bucket_index(&ours, &node_id_with(0x80)), Some(0));
        assert_eq!(bucket_index(&ours, &node_id_with(0x40)), Some(1));
        assert_eq!(bucket_index(&ours, &node_id_with(0x01)), Some(7));
        let mut id = [0u8; NODE_ID_LEN];
        id[0] = 0x00;
        id[1] = 0x80;
        assert_eq!(bucket_index(&ours, &id), Some(8));
        assert_eq!(bucket_index(&ours, &ours), None);
    }

    #[test]
    fn routing_table_enforces_bucket_cap_and_evicts_only_on_failure() {
        let now = Instant::now();
        let ours = [0u8; NODE_ID_LEN];
        let mut table = RoutingTable::new(ours, 100);
        let mut ids = Vec::new();
        for index in 0..BUCKET_SIZE {
            let id = node_id_with(0x80 + u8::try_from(index).unwrap());
            ids.push(id);
            let outcome = table.insert(id, addr(1000 + index as u16), now);
            assert_eq!(outcome, InsertOutcome::Added);
        }
        assert_eq!(table.len(), BUCKET_SIZE);

        let newest = node_id_with(0x8f);
        let outcome = table.insert(newest, addr(2000), now);
        match outcome {
            InsertOutcome::BucketFull {
                bucket,
                oldest_id,
                oldest_addr,
            } => {
                assert_eq!(bucket, 0);
                assert_eq!(oldest_id, ids[0]);
                assert_eq!(oldest_addr, addr(1000));
            }
            other => panic!("expected a full bucket, got {other:?}"),
        }
        // A successful ping keeps the incumbent: no replacement is attempted.
        assert_eq!(table.len(), BUCKET_SIZE);
        assert!(
            table
                .closest(&newest, BUCKET_SIZE)
                .iter()
                .all(|(id, _)| *id != newest)
        );

        // A failed ping replaces the least-recently-seen occupant.
        assert!(table.replace(0, ids[0], newest, addr(2000), now));
        assert_eq!(table.len(), BUCKET_SIZE);
        let ids_now: Vec<NodeId> = table
            .closest(&ours, BUCKET_SIZE)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(ids_now.contains(&newest));
        assert!(!ids_now.contains(&ids[0]));
    }

    #[test]
    fn routing_table_respects_global_cap_and_own_id() {
        let now = Instant::now();
        let ours = [0u8; NODE_ID_LEN];
        let mut table = RoutingTable::new(ours, 3);
        assert_eq!(
            table.insert(ours, addr(1), now),
            InsertOutcome::OwnId,
            "our own id is never inserted"
        );
        for (index, first) in [0x80u8, 0x40, 0x20].into_iter().enumerate() {
            let id = node_id_with(first);
            assert_eq!(
                table.insert(id, addr(100 + index as u16), now),
                InsertOutcome::Added
            );
        }
        assert_eq!(table.len(), 3);
        assert_eq!(
            table.insert(node_id_with(0x10), addr(200), now),
            InsertOutcome::AtCapacity
        );
        // Refreshing an existing node is allowed at the cap.
        assert_eq!(
            table.insert(node_id_with(0x80), addr(201), now),
            InsertOutcome::Refreshed
        );
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn peer_store_bounds_and_expires() {
        let now = Instant::now();
        let mut store = PeerStore::new(Duration::from_millis(50));
        let hash = [7u8; NODE_ID_LEN];
        store.insert(hash, addr(6881), now);
        assert_eq!(store.get(&hash, now), vec![addr(6881)]);
        assert!(store.get(&hash, now + Duration::from_millis(60)).is_empty());

        for index in 0..MAX_PEERS_PER_HASH + 10 {
            store.insert(hash, addr(1000 + index as u16), now);
        }
        assert_eq!(store.get(&hash, now).len(), MAX_PEERS_PER_HASH);

        for index in 0..MAX_PEER_HASHES + 5 {
            let mut other = [0u8; NODE_ID_LEN];
            other[..8].copy_from_slice(&(index as u64).to_be_bytes());
            store.insert(other, addr(7000), now);
        }
        assert_eq!(store.hash_count(), MAX_PEER_HASHES);
        let mut first = [0u8; NODE_ID_LEN];
        first[..8].copy_from_slice(&0u64.to_be_bytes());
        assert!(store.get(&first, now).is_empty());
    }

    #[test]
    fn tokens_rotate_and_are_ip_bound() {
        let start = Instant::now();
        let mut tokens = TokenManager::new(start);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let token = tokens.token_for(ip, start);
        assert!(tokens.valid(ip, &token, start));
        assert!(!tokens.valid(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), &token, start));
        assert!(tokens.valid(ip, &token, start + TOKEN_ROTATE));
        assert!(!tokens.valid(ip, &token, start + TOKEN_ROTATE * 2));
    }

    #[test]
    fn response_validation_checks_source_and_expected_id() {
        let query = Query {
            addr: addr(1000),
            expected_id: Some(node_id_with(0x80)),
            purpose: QueryPurpose::Seed { reply: None },
            deadline: Instant::now(),
        };
        assert!(response_is_valid(&query, addr(1000), &node_id_with(0x80)));
        assert!(!response_is_valid(&query, addr(1001), &node_id_with(0x80)));
        assert!(!response_is_valid(&query, addr(1000), &node_id_with(0x81)));
    }

    #[test]
    fn compact_nodes_round_trip() {
        let entries = vec![
            (node_id_with(0x11), addr(6881)),
            (node_id_with(0x22), addr(51413)),
        ];
        let encoded = compact_nodes(&entries);
        assert_eq!(encoded.len(), 2 * COMPACT_NODE_LEN);
        assert_eq!(parse_compact_nodes(&encoded), entries);
        // A truncated tail is ignored, not a panic.
        assert_eq!(
            parse_compact_nodes(&encoded[..COMPACT_NODE_LEN + 3]).len(),
            1
        );
        assert_eq!(parse_compact_nodes(&[]), Vec::new());
    }

    #[tokio::test]
    async fn two_nodes_add_each_other_and_answer_ping() {
        let (a, _events_a) = start(config(150)).await.unwrap();
        let (b, _events_b) = start(config(150)).await.unwrap();
        a.add_node(b.local_addr()).await;
        b.add_node(a.local_addr()).await;

        assert!(a.stats().await.nodes >= 1, "a learned b");
        assert!(b.stats().await.nodes >= 1, "b learned a");
        assert!(a.ping(b.local_addr()).await);
        assert!(b.ping(a.local_addr()).await);
        assert_eq!(a.stats().await.nodes, 1);
    }

    #[tokio::test]
    async fn announced_peer_is_found_through_a_third_node() {
        let (a, _events_a) = start(config(150)).await.unwrap();
        let (b, _events_b) = start(config(150)).await.unwrap();
        let (c, _events_c) = start(config(150)).await.unwrap();
        a.add_node(b.local_addr()).await;
        b.add_node(a.local_addr()).await;
        b.add_node(c.local_addr()).await;
        c.add_node(b.local_addr()).await;
        assert!(b.stats().await.nodes >= 2);
        assert!(c.stats().await.nodes >= 1);

        let hash = [0x5au8; NODE_ID_LEN];
        let stored = c.announce_peer(hash, 6881).await;
        assert!(stored >= 1, "c stored the peer on at least one node");

        let expected = SocketAddr::new(c.local_addr().ip(), 6881);
        let found = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let peers = a.get_peers(hash).await;
                if peers.contains(&expected) {
                    return peers;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("get_peers found the announced peer within the bounded wait");
        assert!(found.contains(&expected));
    }

    #[tokio::test]
    async fn forged_tokens_are_rejected_silently() {
        let (node, _events) = start(config(150)).await.unwrap();
        let raw = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = node.local_addr();
        let info_hash = [0x33u8; NODE_ID_LEN];

        let forged = krpc_query(
            b"t-1",
            &[0x44u8; NODE_ID_LEN],
            b"announce_peer",
            vec![
                (b"implied_port".to_vec(), Value::Int(0)),
                (b"info_hash".to_vec(), Value::Bytes(info_hash.to_vec())),
                (b"port".to_vec(), Value::Int(6881)),
                (b"token".to_vec(), Value::Bytes(vec![0u8; 8])),
            ],
        );
        raw.send_to(&bencode::encode(&forged), target)
            .await
            .unwrap();
        let mut buffer = [0u8; MAX_PACKET_BYTES];
        assert!(
            tokio::time::timeout(Duration::from_millis(300), raw.recv_from(&mut buffer))
                .await
                .is_err(),
            "a bad token must not produce a reply"
        );

        // The node did not store the forged announce: get_peers returns nodes,
        // not values.
        let lookup = krpc_query(
            b"t-2",
            &[0x55u8; NODE_ID_LEN],
            b"get_peers",
            vec![(b"info_hash".to_vec(), Value::Bytes(info_hash.to_vec()))],
        );
        raw.send_to(&bencode::encode(&lookup), target)
            .await
            .unwrap();
        let (len, _) = tokio::time::timeout(Duration::from_secs(1), raw.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        let reply = bencode::decode(&buffer[..len]).unwrap();
        let response = reply.get(b"r").unwrap();
        assert!(response.get(b"values").is_none());
        assert!(response.get(b"nodes").is_some());
        assert!(response.get(b"token").is_some());
    }

    #[tokio::test]
    async fn hostile_packets_do_not_kill_the_node() {
        let (a, _events_a) = start(config(150)).await.unwrap();
        let (b, _events_b) = start(config(150)).await.unwrap();
        let raw = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = b.local_addr();

        for payload in [
            b"".as_slice(),
            b"x".as_slice(),
            b"i42e".as_slice(),
            b"d1:z".as_slice(),
            b"d1:yi1ee".as_slice(),
            b"d1:y1:r1:t99:short".as_slice(),
            b"l".as_slice(),
            b"d1:t0:1:y1:qe".as_slice(),
        ] {
            raw.send_to(payload, target).await.unwrap();
        }
        // A syntactically valid reply with a transaction id nobody asked for.
        let stray = krpc_response(b"zzzz", &[0x77u8; NODE_ID_LEN], Vec::new());
        raw.send_to(&bencode::encode(&stray), target).await.unwrap();
        // A reply from the wrong source is covered by the unit test above;
        // the node must still be alive here.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(a.ping(target).await);
    }

    #[tokio::test]
    async fn wrong_source_reply_is_ignored() {
        let (a, _events_a) = start(config(150)).await.unwrap();
        let a_addr = a.local_addr();
        let (b, _events_b) = start(config(150)).await.unwrap();

        let fake = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let fake_addr = fake.local_addr().unwrap();
        let pinger = a.clone();
        let ping = tokio::spawn(async move { pinger.ping(fake_addr).await });

        let mut buffer = [0u8; MAX_PACKET_BYTES];
        let (len, _) = tokio::time::timeout(Duration::from_secs(1), fake.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        let query = bencode::decode(&buffer[..len]).unwrap();
        let tid = query.get(b"t").unwrap().as_bytes().unwrap().to_vec();

        // Answer with the right transaction id, but from another address.
        let attacker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let reply = krpc_response(&tid, &[0x66u8; NODE_ID_LEN], Vec::new());
        attacker
            .send_to(&bencode::encode(&reply), a_addr)
            .await
            .unwrap();

        assert!(
            !ping.await.unwrap(),
            "a reply from the wrong source is ignored"
        );
        assert!(a.ping(b.local_addr()).await, "the node still works");
    }

    #[tokio::test]
    async fn unknown_hash_lookup_terminates_within_the_round_cap() {
        let (a, _events_a) = start(config(100)).await.unwrap();
        let (b, _events_b) = start(config(100)).await.unwrap();
        a.add_node(b.local_addr()).await;
        b.add_node(a.local_addr()).await;
        assert!(a.stats().await.nodes >= 1);

        let hash = [0xabu8; NODE_ID_LEN];
        let started = Instant::now();
        let peers = tokio::time::timeout(Duration::from_secs(5), a.get_peers(hash))
            .await
            .expect("lookup finished");
        assert!(peers.is_empty());
        let bound = Duration::from_millis(100) * (MAX_LOOKUP_ROUNDS as u32 + 4);
        assert!(
            started.elapsed() <= bound,
            "lookup took {:?}, expected at most {bound:?}",
            started.elapsed()
        );
    }
}
