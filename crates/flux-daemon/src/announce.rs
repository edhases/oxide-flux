//! Tracker announce service for the session.
//!
//! One [`TrackerService`] owns the announce schedule of one torrent. It sits on
//! top of [`flux_net::tracker`] — the HTTP/HTTPS and UDP (BEP 15) clients, the
//! tiered failover and the exponential backoff all live there. This module adds
//! only what the session needs around it:
//!
//! - a task that wakes on the tracker's own interval (or the backoff deadline
//!   after failures) and keeps running until the torrent is shut down;
//! - `Started` / `Completed` / `Stopped` transitions: `started` and `completed`
//!   are delivered at most once (retried on the next cycle while the trackers
//!   are unreachable), `stopped` is best effort during shutdown;
//! - peer lists deduplicated across announces and capped, so a lying tracker
//!   cannot flood the peer layer;
//! - caller-supplied byte counters through a [`watch`] channel, so this task
//!   never guesses how much was transferred.
//!
//! The task reads its clock only through [`tokio::time::Instant`]: the network
//! calls use real time, everything else is a monotonic deadline handed to the
//! [`Announcer`]. There is no second scheduler and no busy loop — when every
//! tracker is failing, the sleep is the announcer's backoff.
//!
//! `private` is metadata only here: a private torrent still announces normally.
//! The flag is carried so the session can refuse DHT/PEX/LSD and cross-torrent
//! peer sharing for it once those exist.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use flux_core::metainfo::Metainfo;
use flux_net::tracker::{
    AnnounceOutcome, AnnounceRequest, AnnounceResponse, Announcer, AnnouncerConfig,
    DEFAULT_INTERVAL, Event, Scheme, TrackerError,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Default number of peers emitted per announce, matching common client
/// defaults. The tracker may return more; the surplus is dropped.
pub const DEFAULT_PEER_CAP: usize = 200;

/// Commands queued for the announce task. Small on purpose: the task is the
/// only writer and each command is completed before the method returns.
const COMMAND_CAPACITY: usize = 8;

/// How long [`TrackerService::shutdown`] waits for the best-effort `Stopped`
/// announce and the task to finish before aborting it.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Longest tracker URL accepted. Real announce URLs are well under this; the
/// cap only stops a hostile metainfo from causing large allocations.
const MAX_URL_LEN: usize = 2048;

/// Peers remembered for deduplication across announces. When the set grows
/// past this it is cleared: re-announcing an old peer is harmless, unbounded
/// growth is not.
const SEEN_PEER_LIMIT: usize = 65_536;

/// Everything one torrent needs to announce.
///
/// Build it with [`AnnounceSpec::from_metainfo`] unless the caller already
/// sanitised the tracker list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceSpec {
    /// The v1 info hash; tracker protocols carry the raw 20 bytes.
    pub info_hash: [u8; 20],
    /// Our peer id.
    pub peer_id: [u8; 20],
    /// The TCP port peers should connect back to.
    pub port: u16,
    /// Announce-list tiers. Empty strings and duplicates are filtered by
    /// [`Self::from_metainfo`]; the announcer itself only rejects empty lists.
    pub trackers: Vec<Vec<String>>,
    /// BEP 27 private flag. Metadata only: private torrents still announce,
    /// but the session must not share their peers or use DHT/PEX/LSD.
    pub private: bool,
    /// How many peers to request from the tracker.
    pub num_want: Option<i32>,
}

impl AnnounceSpec {
    /// Builds a spec from parsed metainfo.
    ///
    /// Tracker URLs are taken from `announce-list` in tier order, deduplicated
    /// across tiers, and filtered: wrong schemes (`ws://`, `webseed`), empty
    /// hosts and control characters are dropped instead of failing the torrent.
    /// When no tier survives, `announce` is used as a one-tracker tier.
    ///
    /// # Errors
    ///
    /// Returns [`AnnounceError::NoInfoHash`] for a v2-only torrent (there is no
    /// 20-byte v1 hash to announce) and [`AnnounceError::NoTrackers`] when
    /// neither field contains a usable HTTP/HTTPS/UDP URL.
    pub fn from_metainfo(
        metainfo: &Metainfo,
        port: u16,
        peer_id: [u8; 20],
    ) -> Result<Self, AnnounceError> {
        let info_hash = metainfo.info.hashes.v1.ok_or(AnnounceError::NoInfoHash)?;

        let mut seen = HashSet::new();
        let mut trackers = Vec::new();
        for tier in &metainfo.announce_list {
            let mut clean = Vec::new();
            for url in tier {
                let Some(url) = usable_tracker_url(url) else {
                    continue;
                };
                if seen.insert(url.clone()) {
                    clean.push(url);
                }
            }
            if !clean.is_empty() {
                trackers.push(clean);
            }
        }

        if trackers.is_empty()
            && let Some(url) = metainfo.announce.as_deref().and_then(usable_tracker_url)
        {
            trackers.push(vec![url]);
        }

        if trackers.is_empty() {
            return Err(AnnounceError::NoTrackers);
        }

        Ok(Self {
            info_hash,
            peer_id,
            port,
            trackers,
            private: metainfo.is_private(),
            num_want: None,
        })
    }
}

/// Why a tracker list could not be built from metainfo.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AnnounceError {
    /// The metainfo has no v1 info hash, which tracker announces require.
    #[error("metainfo carries no v1 info hash; tracker announces need a 20-byte info hash")]
    NoInfoHash,
    /// Neither `announce` nor `announce-list` held a usable URL.
    #[error("metainfo has no usable tracker URL in `announce` or `announce-list`")]
    NoTrackers,
}

/// Byte counters the caller supplies on every announce cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackerStats {
    /// Bytes uploaded so far.
    pub uploaded: u64,
    /// Bytes downloaded so far.
    pub downloaded: u64,
    /// Bytes still needed. Ignored once the torrent is complete.
    pub left: u64,
}

/// What the announce task reports to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerEvent {
    /// New peers from a tracker that answered. Already deduplicated and capped.
    Peers {
        /// Index into [`AnnounceSpec::trackers`] of the tier that answered.
        tier: usize,
        /// Peers not delivered before.
        addr: Vec<SocketAddr>,
    },
    /// A non-fatal `warning message` from the tracker.
    Warn(String),
    /// An announce cycle failed. `url` is the tracker named by the error, or
    /// the list's primary tracker when the error does not carry one (the
    /// announcer reports only the last error of a failed cycle).
    Failed {
        /// The tracker associated with the failure.
        url: String,
        /// Rendered tracker error.
        error: String,
    },
}

/// The per-torrent announce task.
///
/// Dropping the service signals the task to send a best-effort `Stopped`
/// announce and exit; the task is detached so dropping is never blocking. Use
/// [`shutdown`](Self::shutdown) when the caller wants to wait for that.
pub struct TrackerService {
    commands: mpsc::Sender<Command>,
    task: Option<JoinHandle<()>>,
}

enum Command {
    /// Run a cycle now (after a caller-supplied pause or an empty peer set).
    AnnounceNow { reply: oneshot::Sender<()> },
    /// Report `completed` once, then resume the normal schedule.
    Completed { reply: oneshot::Sender<()> },
    /// Send `stopped` best-effort and stop the task.
    Shutdown,
}

impl TrackerService {
    /// Starts the announce task with the default peer cap.
    ///
    /// `events` must be a bounded channel; the task waits on it like any other
    /// producer, which is the backpressure that keeps a fast tracker from
    /// growing memory without bound.
    pub fn spawn(
        spec: AnnounceSpec,
        stats: watch::Receiver<TrackerStats>,
        events: mpsc::Sender<TrackerEvent>,
    ) -> Self {
        Self::spawn_with_peer_cap(spec, stats, events, DEFAULT_PEER_CAP)
    }

    /// Starts the announce task with an explicit peer cap.
    ///
    /// A cap of zero emits no peers (the announce still counts for the tracker).
    pub fn spawn_with_peer_cap(
        spec: AnnounceSpec,
        stats: watch::Receiver<TrackerStats>,
        events: mpsc::Sender<TrackerEvent>,
        peer_cap: usize,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let task = tokio::spawn(run(spec, stats, events, receiver, peer_cap));
        Self {
            commands,
            task: Some(task),
        }
    }

    /// Runs one announce cycle immediately instead of waiting for the
    /// tracker's interval.
    ///
    /// The call returns once the cycle finished (success or failure). It is
    /// meant for "no peers yet" refreshes. The forced cycle restarts the
    /// announce schedule, so each explicit call sends exactly one announce;
    /// the periodic loop still waits on the announcer's own interval and
    /// backoff, so a failing tracker is never polled in a tight loop.
    pub async fn announce_now(&self) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::AnnounceNow { reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }

    /// Reports `completed` to the trackers, once.
    ///
    /// Calling it again after the event was delivered does nothing. If every
    /// tracker is unreachable the event stays pending and is retried on the
    /// next cycle; it is never sent twice.
    pub async fn completed(&self) {
        let (reply, done) = oneshot::channel();
        if self
            .commands
            .send(Command::Completed { reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }

    /// Sends a best-effort `stopped` announce, then joins the task.
    ///
    /// The wait is bounded by [`STOP_GRACE`]; an unresponsive tracker is
    /// abandoned and the task is aborted rather than blocking shutdown.
    pub async fn shutdown(mut self) {
        let signalled = self.commands.try_send(Command::Shutdown).is_ok();
        if let Some(mut task) = self.task.take() {
            if signalled {
                if tokio::time::timeout(STOP_GRACE, &mut task).await.is_err() {
                    task.abort();
                }
            } else {
                // A full or closed command queue means the task is stuck in a
                // tracker call or already gone; do not wait on either.
                task.abort();
            }
        }
    }
}

impl Drop for TrackerService {
    fn drop(&mut self) {
        // Best effort only: the task also exits when the command channel
        // closes, so a full queue cannot leak it.
        let _ = self.commands.try_send(Command::Shutdown);
    }
}

/// Accepts only URLs the tracker stack can actually announce to.
///
/// `String` already rules out non-UTF-8; this rejects the hostile remainder:
/// control characters, empty authorities and schemes without a client.
fn usable_tracker_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return None;
    }
    if url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return None;
    }
    Scheme::parse(url)?;
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    Some(url.to_owned())
}

/// State owned by the spawned task.
struct Worker {
    spec: AnnounceSpec,
    announcer: Announcer,
    config: AnnouncerConfig,
    stats: watch::Receiver<TrackerStats>,
    events: mpsc::Sender<TrackerEvent>,
    started_at: Instant,
    peer_cap: usize,
    seen: HashSet<SocketAddr>,
    started: bool,
    completed: bool,
    completed_pending: bool,
}

async fn run(
    spec: AnnounceSpec,
    stats: watch::Receiver<TrackerStats>,
    events: mpsc::Sender<TrackerEvent>,
    mut commands: mpsc::Receiver<Command>,
    peer_cap: usize,
) {
    let config = AnnouncerConfig::default();
    let announcer = match Announcer::new(spec.trackers.clone(), config.clone()) {
        Ok(announcer) => announcer,
        Err(error) => {
            let _ = events
                .send(TrackerEvent::Failed {
                    url: primary_url(&spec),
                    error: error.to_string(),
                })
                .await;
            return;
        }
    };

    let mut worker = Worker {
        spec,
        announcer,
        config,
        stats,
        events,
        started_at: Instant::now(),
        peer_cap,
        seen: HashSet::new(),
        started: false,
        completed: false,
        completed_pending: false,
    };

    let mut next = worker.announce(Some(Event::Started), false).await;
    loop {
        let wait = next.unwrap_or_else(|| Duration::from_secs(u64::from(DEFAULT_INTERVAL)));
        tokio::select! {
            () = tokio::time::sleep(wait) => {
                let event = worker.next_event();
                next = worker.announce(event, false).await;
            }
            command = commands.recv() => match command {
                Some(Command::AnnounceNow { reply }) => {
                    let event = worker.next_event();
                    next = worker.announce(event, true).await;
                    let _ = reply.send(());
                }
                Some(Command::Completed { reply }) => {
                    if !worker.completed {
                        worker.completed_pending = true;
                        let event = worker.next_event();
                        next = worker.announce(event, true).await;
                    }
                    let _ = reply.send(());
                }
                Some(Command::Shutdown) => {
                    let _ = worker.announce(Some(Event::Stopped), true).await;
                    return;
                }
                None => return,
            },
        }
    }
}

impl Worker {
    /// The transition that still has to reach a tracker, if any.
    fn next_event(&self) -> Option<Event> {
        if self.completed_pending && !self.completed {
            Some(Event::Completed)
        } else if !self.started {
            Some(Event::Started)
        } else {
            None
        }
    }

    /// Runs one announce attempt and returns the next deadline.
    ///
    /// `force` restarts the announce schedule because the caller asked for an
    /// out-of-band event (a refresh, `completed` or `stopped`). The periodic
    /// loop never forces, so backoff is honoured there.
    async fn announce(&mut self, event: Option<Event>, force: bool) -> Option<Duration> {
        if force && !self.restart_schedule().await {
            return self.announcer.next_announce_in(self.now());
        }

        let stats = *self.stats.borrow();
        let left = if self.completed || self.completed_pending {
            0
        } else {
            stats.left
        };
        let request = AnnounceRequest {
            info_hash: self.spec.info_hash,
            peer_id: self.spec.peer_id,
            port: self.spec.port,
            uploaded: stats.uploaded,
            downloaded: stats.downloaded,
            left,
            event,
            num_want: self.spec.num_want,
            key: None,
        };

        let now = self.now();
        match self.announcer.announce(&request, now).await {
            Ok(outcome) => {
                self.record_success(event, &outcome).await;
            }
            Err(TrackerError::NotDue { .. }) => {
                // The automatic loop always sleeps exactly until the deadline;
                // reaching this means a forced cycle was not needed.
            }
            Err(error) => {
                let _ = self
                    .events
                    .send(TrackerEvent::Failed {
                        url: primary_url(&self.spec),
                        error: error.to_string(),
                    })
                    .await;
            }
        }

        self.announcer.next_announce_in(self.now())
    }

    /// Rebuilds the announcer so an explicit event is attempted immediately.
    async fn restart_schedule(&mut self) -> bool {
        match Announcer::new(self.spec.trackers.clone(), self.config.clone()) {
            Ok(announcer) => {
                self.announcer = announcer;
                true
            }
            Err(error) => {
                let _ = self
                    .events
                    .send(TrackerEvent::Failed {
                        url: primary_url(&self.spec),
                        error: error.to_string(),
                    })
                    .await;
                false
            }
        }
    }

    async fn record_success(&mut self, event: Option<Event>, outcome: &AnnounceOutcome) {
        let response: &AnnounceResponse = &outcome.response;
        if let Some(warning) = response.warning.clone() {
            let _ = self.events.send(TrackerEvent::Warn(warning)).await;
        }

        let peers = self.collect_peers(&response.peers);
        if !peers.is_empty() {
            let _ = self
                .events
                .send(TrackerEvent::Peers {
                    tier: outcome.tier,
                    addr: peers,
                })
                .await;
        }

        match event {
            Some(Event::Started) => self.started = true,
            Some(Event::Completed) => {
                self.completed = true;
                self.completed_pending = false;
            }
            Some(Event::Stopped) | None => {}
        }
    }

    /// Filters a tracker's peer list: no duplicates (including peers delivered
    /// earlier), at most `peer_cap` entries.
    fn collect_peers(&mut self, peers: &[SocketAddr]) -> Vec<SocketAddr> {
        if self.seen.len() > SEEN_PEER_LIMIT {
            self.seen.clear();
        }
        let mut out = Vec::new();
        for &addr in peers {
            if out.len() >= self.peer_cap {
                break;
            }
            if self.seen.insert(addr) {
                out.push(addr);
            }
        }
        out
    }

    fn now(&self) -> Duration {
        self.started_at.elapsed()
    }
}

/// The tracker named by an error, or the list's primary tracker when the
/// announcer's error has no URL in it.
fn primary_url(spec: &AnnounceSpec) -> String {
    spec.trackers
        .iter()
        .flatten()
        .next()
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use flux_core::metainfo::{FileLayout, Hashes, Info};

    use super::*;

    const PEER_A: [u8; 6] = [127, 0, 0, 1, 0x1a, 0xe1]; // 127.0.0.1:6881
    const PEER_B: [u8; 6] = [10, 0, 0, 5, 0xc8, 0xd5]; // 10.0.0.5:51413
    const PEER_C: [u8; 6] = [192, 0, 2, 7, 0x69, 0x8d]; // 192.0.2.7:27021

    /// A tracker response with a compact IPv4 peer list.
    fn announce_body(peers: &[[u8; 6]], interval: u32) -> Vec<u8> {
        let mut body = format!(
            "d8:completei0e10:incompletei0e8:intervali{interval}e5:peers{}:",
            peers.len() * 6
        )
        .into_bytes();
        for peer in peers {
            body.extend_from_slice(peer);
        }
        body.push(b'e');
        body
    }

    /// One-response-per-request HTTP tracker on loopback.
    struct Server {
        url: String,
        hits: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        fn request_lines(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn http_server(status: &str, body: &[u8]) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);

        let hits = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let hits_thread = Arc::clone(&hits);
        let requests_thread = Arc::clone(&requests);

        // Detached on purpose: the test process ends the thread, and tests must
        // not sleep waiting for a shutdown handshake.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else {
                    continue;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match socket.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&chunk[..read]),
                    }
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                    if request.len() > 64 * 1024 {
                        break;
                    }
                }
                let line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                requests_thread.lock().unwrap().push(line);
                hits_thread.fetch_add(1, Ordering::SeqCst);
                let _ = socket.write_all(&response);
                let _ = socket.flush();
            }
        });

        Server {
            url: format!("http://{address}/announce"),
            hits,
            requests,
        }
    }

    fn spec(url: &str) -> AnnounceSpec {
        AnnounceSpec {
            info_hash: [0xab; 20],
            peer_id: [0xcd; 20],
            port: 6881,
            trackers: vec![vec![url.to_owned()]],
            private: false,
            num_want: Some(50),
        }
    }

    fn metainfo(announce: Option<&str>, tiers: &[&[&str]]) -> Metainfo {
        Metainfo {
            announce: announce.map(str::to_owned),
            announce_list: tiers
                .iter()
                .map(|tier| tier.iter().map(|url| (*url).to_owned()).collect())
                .collect(),
            comment: None,
            created_by: None,
            creation_date: None,
            info: Info {
                name: "synthetic".to_owned(),
                piece_length: 16_384,
                private: false,
                layout: FileLayout::contiguous([(vec!["synthetic.bin".to_owned()], 1u64)], 16_384)
                    .unwrap(),
                pieces: None,
                piece_layers: BTreeMap::new(),
                hashes: Hashes {
                    v1: Some([0x11; 20]),
                    v2: None,
                },
            },
        }
    }

    fn addr(peer: [u8; 6]) -> SocketAddr {
        SocketAddr::from((
            [peer[0], peer[1], peer[2], peer[3]],
            u16::from_be_bytes([peer[4], peer[5]]),
        ))
    }

    fn stats_watch() -> (watch::Sender<TrackerStats>, watch::Receiver<TrackerStats>) {
        watch::channel(TrackerStats {
            uploaded: 10,
            downloaded: 20,
            left: 30,
        })
    }

    async fn next_event(events: &mut mpsc::Receiver<TrackerEvent>) -> TrackerEvent {
        tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("event within the test timeout")
            .expect("the service keeps its event channel open")
    }

    #[tokio::test]
    async fn delivers_deduped_peers_from_a_compact_list() {
        let server = http_server("200 OK", &announce_body(&[PEER_A, PEER_A, PEER_B], 600));
        let (stats, stats_rx) = stats_watch();
        let (events, mut events_rx) = mpsc::channel(16);
        let service = TrackerService::spawn(spec(&server.url), stats_rx, events);

        match next_event(&mut events_rx).await {
            TrackerEvent::Peers { tier, addr: peers } => {
                assert_eq!(tier, 0);
                assert_eq!(peers, vec![addr(PEER_A), addr(PEER_B)]);
            }
            other => panic!("expected peers, got {other:?}"),
        }
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        drop(stats);
        service.shutdown().await;
    }

    #[tokio::test]
    async fn caps_the_peer_list() {
        let server = http_server("200 OK", &announce_body(&[PEER_A, PEER_B, PEER_C], 600));
        let (_stats, stats_rx) = stats_watch();
        let (events, mut events_rx) = mpsc::channel(16);
        let service = TrackerService::spawn_with_peer_cap(spec(&server.url), stats_rx, events, 2);

        match next_event(&mut events_rx).await {
            TrackerEvent::Peers { addr: peers, .. } => {
                assert_eq!(peers, vec![addr(PEER_A), addr(PEER_B)]);
            }
            other => panic!("expected peers, got {other:?}"),
        }
        service.shutdown().await;
    }

    #[tokio::test]
    async fn a_failure_reason_becomes_a_failed_event() {
        let server = http_server("200 OK", b"d14:failure reason7:privatee");
        let (_stats, stats_rx) = stats_watch();
        let (events, mut events_rx) = mpsc::channel(16);
        let service = TrackerService::spawn(spec(&server.url), stats_rx, events);

        match next_event(&mut events_rx).await {
            TrackerEvent::Failed { url, error } => {
                assert_eq!(url, server.url);
                assert!(error.contains("private"), "unexpected error: {error}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        service.shutdown().await;
    }

    #[test]
    fn metainfo_without_trackers_is_a_typed_error() {
        let meta = metainfo(None, &[]);
        assert_eq!(
            AnnounceSpec::from_metainfo(&meta, 6881, [0; 20]),
            Err(AnnounceError::NoTrackers)
        );

        // Hostile or unsupported URLs are dropped, and an empty result is still
        // the same typed error rather than a panic.
        let meta = metainfo(
            Some("not a url"),
            &[&["wss://tracker.example/announce", "http://", "  "]],
        );
        assert_eq!(
            AnnounceSpec::from_metainfo(&meta, 6881, [0; 20]),
            Err(AnnounceError::NoTrackers)
        );
    }

    #[test]
    fn v2_only_metainfo_has_no_announce_hash() {
        let mut meta = metainfo(Some("http://tracker.example/announce"), &[]);
        meta.info.hashes = Hashes {
            v1: None,
            v2: Some([0x22; 32]),
        };
        assert_eq!(
            AnnounceSpec::from_metainfo(&meta, 6881, [0; 20]),
            Err(AnnounceError::NoInfoHash)
        );
    }

    #[test]
    fn tiers_are_preserved_and_deduplicated() {
        let mut meta = metainfo(
            Some("http://fallback.example/announce"),
            &[
                &[
                    "http://one.example/announce",
                    "wss://ignored.example/announce",
                    "http://two.example/announce",
                ],
                &[],
                &["http://two.example/announce", "udp://three.example:6969"],
            ],
        );
        meta.info.private = true;
        let spec = AnnounceSpec::from_metainfo(&meta, 6881, [0xcd; 20]).unwrap();
        assert_eq!(
            spec.trackers,
            vec![
                vec![
                    "http://one.example/announce".to_owned(),
                    "http://two.example/announce".to_owned()
                ],
                vec!["udp://three.example:6969".to_owned()],
            ]
        );
        assert_eq!(spec.info_hash, [0x11; 20]);
        assert_eq!(spec.peer_id, [0xcd; 20]);
        assert_eq!(spec.port, 6881);
        assert!(spec.private, "the BEP 27 flag must survive the conversion");
    }

    #[test]
    fn announce_falls_back_to_the_primary_url() {
        let meta = metainfo(Some("http://only.example/announce"), &[]);
        let spec = AnnounceSpec::from_metainfo(&meta, 51413, [0xcd; 20]).unwrap();
        assert_eq!(
            spec.trackers,
            vec![vec!["http://only.example/announce".to_owned()]]
        );
    }

    #[tokio::test]
    async fn completed_is_sent_once_and_reports_left_zero() {
        let server = http_server("200 OK", &announce_body(&[PEER_A], 600));
        let (_stats, stats_rx) = stats_watch();
        let (events, mut events_rx) = mpsc::channel(16);
        let service = TrackerService::spawn(spec(&server.url), stats_rx, events);

        assert!(matches!(
            next_event(&mut events_rx).await,
            TrackerEvent::Peers { .. }
        ));

        service.completed().await;
        service.completed().await;

        let requests = server.request_lines();
        let completed: Vec<_> = requests
            .iter()
            .filter(|line| line.contains("event=completed"))
            .collect();
        assert_eq!(completed.len(), 1, "requests: {requests:?}");
        assert!(
            completed[0].contains("left=0"),
            "completed announce must report nothing left: {}",
            completed[0]
        );

        service.shutdown().await;
    }

    #[tokio::test]
    async fn announce_now_refreshes_without_waiting_for_the_interval() {
        let server = http_server("200 OK", &announce_body(&[PEER_A], 600));
        let (_stats, stats_rx) = stats_watch();
        let (events, mut events_rx) = mpsc::channel(16);
        let service = TrackerService::spawn(spec(&server.url), stats_rx, events);

        assert!(matches!(
            next_event(&mut events_rx).await,
            TrackerEvent::Peers { .. }
        ));
        // The interval is 600 s, so a second hit can only come from the
        // explicit refresh.
        service.announce_now().await;
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        service.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_is_bounded_and_stops_the_task() {
        let server = http_server("200 OK", &announce_body(&[], 600));
        let (_stats, stats_rx) = stats_watch();
        let (events, mut events_rx) = mpsc::channel(16);
        let service = TrackerService::spawn(spec(&server.url), stats_rx, events);

        // The spawned task sends `Started` before it reads commands, so the
        // extra hit here is the explicit refresh, not a race.
        service.announce_now().await;
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);

        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .expect("shutdown must not hang");

        let requests = server.request_lines();
        assert_eq!(
            requests
                .iter()
                .filter(|line| line.contains("event=stopped"))
                .count(),
            1,
            "requests: {requests:?}"
        );
        assert!(events_rx.recv().await.is_none());
    }
}
