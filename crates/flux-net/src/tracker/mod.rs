//! Tracker announces: HTTP(S) per BEP 3 and UDP per BEP 15.
//!
//! A tracker is the only part of a torrent session that talks to an HTTP or
//! UDP service rather than to a peer, so it gets its own small stack:
//!
//! - [`http`] builds the BEP 3 query string byte-exactly (the raw 20-byte info
//!   hash and peer id are percent-encoded per byte, never as UTF-8) and parses
//!   compact and dictionary peer lists;
//! - [`udp`] implements the BEP 15 connect/announce exchange with transaction-id
//!   validation and a doubling retry timeout;
//! - [`Announcer`] owns per-tracker scheduling state and walks the announce
//!   list tier by tier.
//!
//! ## Scope
//!
//! Announce only: no scrape, no WebSocket trackers, no DHT/PEX/LSD. A tracker
//! URL with any other scheme is rejected instead of silently ignored.
//!
//! ## Time belongs to the caller
//!
//! Nothing here reads a clock. [`Announcer::announce`] takes the caller's
//! monotonic `now` as a [`Duration`] and stores absolute deadlines in the same
//! units, so scheduling is exact in tests and one scheduler can drive several
//! torrents from one timestamp. Only the network I/O itself uses real time
//! (the configured request timeouts).
//!
//! ## Defaults
//!
//! | item | value | configured by |
//! | --- | --- | --- |
//! | HTTP timeout | 30 s | [`http::HttpConfig::timeout`] |
//! | HTTP redirects | 5 | [`http::HttpConfig::max_redirects`] |
//! | HTTP body cap | 1 MiB | [`http::HttpConfig::max_body_bytes`] |
//! | UDP first timeout | 15 s | [`udp::UdpConfig::timeout`] |
//! | UDP attempts | 3 | [`udp::UdpConfig::attempts`] |
//! | UDP receive buffer | 8 KiB | [`udp::UdpConfig::max_packet_bytes`] |
//! | announce interval fallback | 30 min | [`DEFAULT_INTERVAL`] |
//! | failure backoff | 30 s × 2ⁿ, capped at 30 min | [`AnnouncerConfig`] |
//! | interval cap | 24 h | [`AnnouncerConfig::max_interval`] |

use std::time::Duration;

use thiserror::Error;

use crate::handshake::{InfoHash, PeerId};

pub mod http;
pub mod udp;

/// The interval assumed when a tracker omits one: 30 minutes, the BEP 3
/// default recommendation.
pub const DEFAULT_INTERVAL: u32 = 30 * 60;

/// The announce events defined by BEP 3 / BEP 15.
///
/// `None` in [`AnnounceRequest::event`] is a regular periodic announce; this
/// enum only names the three transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    /// The download started; sent once, right after the first announce.
    Started,
    /// The client is shutting this torrent down.
    Stopped,
    /// The download finished.
    Completed,
}

impl Event {
    /// The spelling BEP 3 uses in the `event` query parameter.
    #[must_use]
    pub const fn http_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped => "stopped",
            Self::Completed => "completed",
        }
    }

    /// The numeric code BEP 15 uses in the announce packet (0 is "none", so no
    /// event is ever encoded as 0).
    #[must_use]
    pub const fn udp_code(self) -> u32 {
        match self {
            Self::Completed => 1,
            Self::Started => 2,
            Self::Stopped => 3,
        }
    }
}

/// One announce, independent of transport.
///
/// The info hash and peer id are raw bytes: they are opaque identifiers, not
/// text, and every transport treats them as bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnnounceRequest {
    /// The torrent's v1 info hash.
    pub info_hash: InfoHash,
    /// Our peer id.
    pub peer_id: PeerId,
    /// The TCP port peers should connect to.
    pub port: u16,
    /// Bytes uploaded so far.
    pub uploaded: u64,
    /// Bytes downloaded so far.
    pub downloaded: u64,
    /// Bytes still needed. Zero for a completed download.
    pub left: u64,
    /// The transition this announce reports, if any.
    pub event: Option<Event>,
    /// How many peers we would like back; `None` leaves the tracker's default.
    pub num_want: Option<i32>,
    /// A per-torrent key that survives IP changes (BEP 3 `key`).
    pub key: Option<u32>,
}

impl AnnounceRequest {
    /// Builds a plain periodic announce for a torrent.
    #[must_use]
    pub fn new(info_hash: InfoHash, peer_id: PeerId, port: u16) -> Self {
        Self {
            info_hash,
            peer_id,
            port,
            ..Self::default()
        }
    }
}

/// A tracker's answer, normalised across transports.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnnounceResponse {
    /// Seconds until the next regular announce.
    pub interval: u32,
    /// The tracker's minimum allowed interval, if it sent one.
    pub min_interval: Option<u32>,
    /// An opaque id to echo on later announces (BEP 3 `tracker id`).
    pub tracker_id: Option<Vec<u8>>,
    /// Number of seeders (`complete`).
    pub complete: u32,
    /// Number of leechers (`incomplete`).
    pub incomplete: u32,
    /// Peers the tracker knows about; may be empty.
    pub peers: Vec<std::net::SocketAddr>,
    /// A non-fatal diagnostic from the tracker (`warning message`).
    pub warning: Option<String>,
}

/// Everything that can go wrong during an announce.
///
/// Tracker data is untrusted: every malformed byte sequence becomes one of
/// these instead of a panic. The type is `Clone`/`PartialEq` because the
/// scheduling layer hands the last error back to its caller and tests compare
/// exact failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TrackerError {
    /// The URL's scheme is not `http`, `https` or `udp`.
    #[error("unsupported tracker scheme in `{url}`")]
    UnsupportedScheme {
        /// The URL that was rejected.
        url: String,
    },

    /// The URL itself is unusable (no host, bad port, empty list, …).
    #[error("invalid tracker URL: {reason}")]
    InvalidUrl {
        /// What is wrong with it.
        reason: String,
    },

    /// The request (or its retries) ran out of time.
    #[error("tracker request timed out")]
    Timeout,

    /// The socket or HTTP stack failed before a full response arrived.
    #[error("tracker transport error: {message}")]
    Transport {
        /// The underlying error, stringified so this type stays cheap to clone
        /// across the actor boundary.
        message: String,
    },

    /// An HTTP tracker answered with a non-2xx status.
    #[error("tracker answered HTTP {status}")]
    HttpStatus {
        /// The status code.
        status: u16,
    },

    /// The response body exceeded the configured cap and was not parsed.
    #[error("tracker response exceeds the {limit}-byte limit")]
    ResponseTooLarge {
        /// The cap that was exceeded.
        limit: usize,
    },

    /// The body was not valid canonical bencode.
    #[error("invalid bencode in tracker response: {0}")]
    Bencode(#[from] flux_core::bencode::Error),

    /// The tracker refused the announce with a `failure reason`.
    #[error("tracker failure: {reason}")]
    Failure {
        /// The tracker's message (lossily decoded).
        reason: String,
    },

    /// The response was bencode but not shaped like a tracker response.
    #[error("malformed tracker response: {reason}")]
    MalformedResponse {
        /// What was wrong.
        reason: String,
    },

    /// Peer data did not match the compact wire layout.
    #[error("malformed tracker peer data: {reason}")]
    MalformedPeers {
        /// What was wrong.
        reason: String,
    },

    /// A UDP packet carried a different transaction id than the one we sent.
    #[error("tracker transaction id mismatch: expected {expected:#010x}, got {got:#010x}")]
    TransactionMismatch {
        /// The id we generated for this attempt.
        expected: u32,
        /// The id the tracker echoed.
        got: u32,
    },

    /// A UDP packet was structurally valid but not the expected action.
    #[error("tracker protocol error: {reason}")]
    Protocol {
        /// What the packet contained.
        reason: String,
    },

    /// No tracker is due yet; the caller should come back later.
    #[error("no tracker is due for another {next_in:?}")]
    NotDue {
        /// Time until the earliest tracker becomes eligible.
        next_in: Duration,
    },
}

/// The schemes this module can announce over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS.
    Https,
    /// BEP 15 UDP.
    Udp,
}

impl Scheme {
    /// Parses the scheme of `url`, case-insensitively, or returns `None`.
    ///
    /// Only the prefix is inspected, so this cannot fail on a malformed host;
    /// the transport does the rest of the validation.
    #[must_use]
    pub fn parse(url: &str) -> Option<Self> {
        let (scheme, _) = url.trim().split_once("://")?;
        if scheme.eq_ignore_ascii_case("http") {
            Some(Self::Http)
        } else if scheme.eq_ignore_ascii_case("https") {
            Some(Self::Https)
        } else if scheme.eq_ignore_ascii_case("udp") {
            Some(Self::Udp)
        } else {
            None
        }
    }

    /// Whether this scheme speaks HTTP.
    #[must_use]
    pub const fn is_http(self) -> bool {
        matches!(self, Self::Http | Self::Https)
    }
}

/// One tracker's scheduling state.
#[derive(Debug, Clone)]
struct TrackerState {
    url: String,
    scheme: Scheme,
    /// Caller-clock deadline for the next attempt; zero means "due now".
    next_due: Duration,
    failures: u32,
    tracker_id: Option<Vec<u8>>,
}

/// One tier of the announce list (BEP 12).
///
/// Trackers inside a tier are alternatives: the facade tries them in order and
/// moves on to the next tier only when the whole tier has failed.
#[derive(Debug, Clone)]
struct Tier {
    trackers: Vec<TrackerState>,
}

/// Scheduling policy for [`Announcer`].
///
/// All durations are caller-clock values. Nothing in here is wall time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncerConfig {
    /// HTTP transport settings.
    pub http: http::HttpConfig,
    /// UDP transport settings.
    pub udp: udp::UdpConfig,
    /// Delay after the first consecutive failure; doubled per further failure.
    pub failure_backoff_base: Duration,
    /// Upper bound for the failure backoff.
    pub failure_backoff_max: Duration,
    /// Upper bound applied to a tracker's announced interval.
    ///
    /// A tracker may ask for hours; capping keeps a lying tracker from putting
    /// a torrent to sleep forever.
    pub max_interval: Duration,
}

impl Default for AnnouncerConfig {
    fn default() -> Self {
        Self {
            http: http::HttpConfig::default(),
            udp: udp::UdpConfig::default(),
            failure_backoff_base: Duration::from_secs(30),
            failure_backoff_max: Duration::from_secs(30 * 60),
            max_interval: Duration::from_secs(24 * 60 * 60),
        }
    }
}

impl AnnouncerConfig {
    /// Exponential backoff for `failures` consecutive failures.
    ///
    /// `base × 2^(failures - 1)`, capped at [`Self::failure_backoff_max`].
    /// Attempts beyond 32 saturate at `2^31 × base`; a zero base or cap yields
    /// zero. No jitter, so the schedule is reproducible in tests.
    #[must_use]
    pub fn backoff(&self, failures: u32) -> Duration {
        if self.failure_backoff_base.is_zero() || self.failure_backoff_max.is_zero() {
            return Duration::ZERO;
        }
        let exponent = failures.saturating_sub(1).min(31);
        let scale = 1u32 << exponent;
        self.failure_backoff_base
            .saturating_mul(scale)
            .min(self.failure_backoff_max)
    }
}

/// The result of a successful announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceOutcome {
    /// The tracker that answered.
    pub url: String,
    /// Index into the tier list passed to [`Announcer::new`].
    pub tier: usize,
    /// The parsed answer.
    pub response: AnnounceResponse,
}

/// Drives the announce list over time.
///
/// The caller owns the loop: it keeps a monotonic clock, calls
/// [`announce`](Self::announce) with the current value whenever
/// [`next_announce_in`](Self::next_announce_in) says something is due, and
/// feeds the peers into the torrent actor. There is deliberately no background
/// task and no timer inside this type.
///
/// ## Selection
///
/// Starting at the current tier, every tracker whose deadline has passed is
/// tried in list order. A success ends the call and makes that tier current.
/// If the tier has no due tracker, or all of its due trackers fail, the next
/// tier is considered, wrapping around.
///
/// While the active tier answers, the other tiers are parked on the same
/// deadline, so a torrent with five tiers still sends one announce per
/// interval rather than five; they only receive traffic when the active tier
/// cannot answer. A failing tracker additionally gets an exponential backoff,
/// so a dead tracker is tried once per backoff window, not once per caller
/// tick.
#[derive(Debug)]
pub struct Announcer {
    tiers: Vec<Tier>,
    current: usize,
    config: AnnouncerConfig,
    http: http::HttpTracker,
    udp: udp::UdpTracker,
}

impl Announcer {
    /// Builds an announcer from announce-list tiers.
    ///
    /// Empty tiers and empty URLs are dropped; every remaining URL must use a
    /// scheme this module implements. At least one tracker must survive.
    pub fn new(tiers: Vec<Vec<String>>, config: AnnouncerConfig) -> Result<Self, TrackerError> {
        let mut built = Vec::new();
        for tier in tiers {
            let mut trackers = Vec::new();
            for url in tier {
                let url = url.trim().to_owned();
                if url.is_empty() {
                    continue;
                }
                let Some(scheme) = Scheme::parse(&url) else {
                    return Err(TrackerError::UnsupportedScheme { url });
                };
                trackers.push(TrackerState {
                    url,
                    scheme,
                    next_due: Duration::ZERO,
                    failures: 0,
                    tracker_id: None,
                });
            }
            if !trackers.is_empty() {
                built.push(Tier { trackers });
            }
        }
        if built.is_empty() {
            return Err(TrackerError::InvalidUrl {
                reason: "announce list contains no usable tracker".to_owned(),
            });
        }

        let http = http::HttpTracker::new(config.http.clone())?;
        let udp = udp::UdpTracker::new(config.udp.clone());
        Ok(Self {
            tiers: built,
            current: 0,
            config,
            http,
            udp,
        })
    }

    /// Builds an announcer with one tracker per tier.
    pub fn from_urls(
        urls: impl IntoIterator<Item = String>,
        config: AnnouncerConfig,
    ) -> Result<Self, TrackerError> {
        Self::new(urls.into_iter().map(|url| vec![url]).collect(), config)
    }

    /// The effective configuration.
    #[must_use]
    pub fn config(&self) -> &AnnouncerConfig {
        &self.config
    }

    /// Index of the tier that most recently answered (or 0 before the first
    /// announce).
    #[must_use]
    pub fn current_tier(&self) -> usize {
        self.current
    }

    /// Total number of trackers across all tiers.
    #[must_use]
    pub fn tracker_count(&self) -> usize {
        self.tiers.iter().map(|tier| tier.trackers.len()).sum()
    }

    /// Time until the earliest tracker is eligible, or `None` if there are no
    /// trackers (which cannot happen after a successful [`new`](Self::new)).
    ///
    /// `Some(Duration::ZERO)` means something is due now. This is a scheduling
    /// hint: [`announce`](Self::announce) also returns [`TrackerError::NotDue`]
    /// rather than announcing early.
    #[must_use]
    pub fn next_announce_in(&self, now: Duration) -> Option<Duration> {
        self.tiers
            .iter()
            .flat_map(|tier| tier.trackers.iter())
            .map(|tracker| tracker.next_due.saturating_sub(now))
            .min()
    }

    /// Whether any tracker is eligible at `now`.
    #[must_use]
    pub fn is_due(&self, now: Duration) -> bool {
        self.next_announce_in(now) == Some(Duration::ZERO)
    }

    /// Announces to the first tracker that accepts, updating the schedule.
    ///
    /// Tries due trackers tier by tier, in list order, at most once each per
    /// call. On success the tracker's `interval` (floored by its
    /// `min_interval`, capped at [`AnnouncerConfig::max_interval`]) becomes the
    /// next deadline, the tier becomes current, and the response is returned.
    /// On failure the tracker is backed off and the next candidate is tried.
    ///
    /// Returns [`TrackerError::NotDue`] when nothing is eligible, and the last
    /// transport error when every eligible tracker failed.
    pub async fn announce(
        &mut self,
        request: &AnnounceRequest,
        now: Duration,
    ) -> Result<AnnounceOutcome, TrackerError> {
        let plan = self.plan(now);
        if plan.is_empty() {
            let next_in = self.next_announce_in(now).unwrap_or(Duration::ZERO);
            return Err(TrackerError::NotDue { next_in });
        }

        let mut last_error = None;
        for (tier_index, tracker_index) in plan {
            let (url, scheme, tracker_id) = {
                let tracker = &self.tiers[tier_index].trackers[tracker_index];
                (
                    tracker.url.clone(),
                    tracker.scheme,
                    tracker.tracker_id.clone(),
                )
            };
            let result = if scheme.is_http() {
                self.http
                    .announce(&url, request, tracker_id.as_deref())
                    .await
            } else {
                self.udp.announce(&url, request).await
            };

            match result {
                Ok(response) => {
                    self.record_success(tier_index, tracker_index, &response, now);
                    return Ok(AnnounceOutcome {
                        url,
                        tier: tier_index,
                        response,
                    });
                }
                Err(error) => {
                    self.record_failure(tier_index, tracker_index, now);
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or(TrackerError::NotDue {
            next_in: Duration::ZERO,
        }))
    }

    /// Due trackers, ordered from the current tier and wrapping around.
    fn plan(&self, now: Duration) -> Vec<(usize, usize)> {
        let mut plan = Vec::new();
        let tiers = self.tiers.len();
        for step in 0..tiers {
            let tier_index = (self.current + step) % tiers;
            for (tracker_index, tracker) in self.tiers[tier_index].trackers.iter().enumerate() {
                if tracker.next_due <= now {
                    plan.push((tier_index, tracker_index));
                }
            }
        }
        plan
    }

    /// Records a successful announce and advances the schedule.
    fn record_success(
        &mut self,
        tier_index: usize,
        tracker_index: usize,
        response: &AnnounceResponse,
        now: Duration,
    ) {
        let interval = self.schedule_interval(response.interval, response.min_interval);
        let due = now.saturating_add(interval);

        self.current = tier_index;
        {
            let tracker = &mut self.tiers[tier_index].trackers[tracker_index];
            tracker.failures = 0;
            tracker.next_due = due;
            tracker.tracker_id = response.tracker_id.clone();
        }
        // Every other tracker -- in this tier and in the others -- waits for the
        // same deadline. That keeps the announce list a failover chain instead
        // of N announces per interval, while a tracker that is already backing
        // off further out keeps its later deadline. The successful tracker is
        // skipped by the comparison itself, since its deadline is already `due`.
        for tier in &mut self.tiers {
            for tracker in &mut tier.trackers {
                if tracker.next_due < due {
                    tracker.next_due = due;
                }
            }
        }
    }

    /// Records a failed announce and applies the exponential backoff.
    fn record_failure(&mut self, tier_index: usize, tracker_index: usize, now: Duration) {
        let delay = {
            let failures = self.tiers[tier_index].trackers[tracker_index]
                .failures
                .saturating_add(1);
            let delay = self.config.backoff(failures);
            self.tiers[tier_index].trackers[tracker_index].failures = failures;
            delay
        };
        self.tiers[tier_index].trackers[tracker_index].next_due = now.saturating_add(delay);
    }

    /// Converts a tracker's interval into a caller-clock duration.
    fn schedule_interval(&self, interval: u32, min_interval: Option<u32>) -> Duration {
        let seconds = u64::from(interval.max(min_interval.unwrap_or(0)));
        let seconds = if seconds == 0 {
            u64::from(DEFAULT_INTERVAL)
        } else {
            seconds
        };
        Duration::from_secs(seconds).min(self.config.max_interval)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Offline helpers shared by the tracker tests: a one-response HTTP server
    //! and a local UDP tracker.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UdpSocket};

    /// A local HTTP server that answers every request with the same bytes.
    pub(crate) struct TestServer {
        /// Base announce URL, without a query string.
        pub(crate) url: String,
        /// Requests received so far.
        pub(crate) hits: Arc<AtomicUsize>,
    }

    /// Wraps `body` in a minimal HTTP/1.1 200 response with a length.
    pub(crate) fn http_ok(body: &[u8]) -> Vec<u8> {
        http_response("200 OK", body)
    }

    /// Wraps `body` in an HTTP/1.1 response with an explicit status line.
    pub(crate) fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// Spawns a server on loopback that replies with `response` to every GET.
    pub(crate) async fn http_server(response: Vec<u8>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let address = listener.local_addr().expect("local address");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let response = response.clone();
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Read until the end of the request headers; a GET fits in
                    // one segment in practice, but be tolerant of fragmentation.
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let Ok(read) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..read]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                        if request.len() > 64 * 1024 {
                            return;
                        }
                    }
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        TestServer {
            url: format!("http://{address}/announce"),
            hits,
        }
    }

    /// What the local UDP tracker should do with each packet.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum UdpBehaviour {
        /// Answer connect and announce correctly.
        Happy,
        /// Connect answers with `transaction_id + 1`.
        WrongTransaction,
        /// Connect answers with action 2 instead of 0.
        WrongAction,
        /// Connect answers with an 8-byte packet (header only).
        ShortPacket,
        /// Never answer, and count the attempts.
        Silent,
    }

    /// A local UDP tracker and the number of datagrams it received.
    pub(crate) struct TestUdpTracker {
        /// Announce URL pointing at the tracker.
        pub(crate) url: String,
        /// Datagrams received so far.
        pub(crate) hits: Arc<AtomicUsize>,
    }

    /// The connection id the test tracker hands out.
    pub(crate) const TEST_CONNECTION_ID: u64 = 0x1122_3344_5566_7788;

    /// Spawns a BEP 15 tracker on loopback for `behaviour`.
    pub(crate) async fn udp_tracker(behaviour: UdpBehaviour) -> TestUdpTracker {
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind loopback");
        let address = socket.local_addr().expect("local address");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);

        tokio::spawn(async move {
            let mut buffer = [0u8; 2048];
            loop {
                let Ok((len, from)) = socket.recv_from(&mut buffer).await else {
                    break;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let Some(packet) = buffer.get(..len) else {
                    continue;
                };
                let Some(action) = packet.get(8..12).and_then(|bytes| bytes.try_into().ok()) else {
                    continue;
                };
                let Some(transaction) = packet.get(12..16).and_then(|bytes| bytes.try_into().ok())
                else {
                    continue;
                };
                let action = u32::from_be_bytes(action);
                let transaction = u32::from_be_bytes(transaction);

                let reply = match behaviour {
                    UdpBehaviour::Silent => continue,
                    UdpBehaviour::WrongTransaction => {
                        connect_response(transaction.wrapping_add(1), TEST_CONNECTION_ID)
                    }
                    UdpBehaviour::WrongAction => {
                        let mut reply = 2u32.to_be_bytes().to_vec();
                        reply.extend_from_slice(&transaction.to_be_bytes());
                        reply.extend_from_slice(&TEST_CONNECTION_ID.to_be_bytes());
                        reply
                    }
                    UdpBehaviour::ShortPacket => {
                        let mut reply = 0u32.to_be_bytes().to_vec();
                        reply.extend_from_slice(&transaction.to_be_bytes());
                        reply
                    }
                    UdpBehaviour::Happy => {
                        if action == 0 {
                            connect_response(transaction, TEST_CONNECTION_ID)
                        } else {
                            announce_response(transaction)
                        }
                    }
                };
                let _ = socket.send_to(&reply, from).await;
            }
        });

        TestUdpTracker {
            url: format!("udp://{address}/announce"),
            hits,
        }
    }

    fn connect_response(transaction: u32, connection_id: u64) -> Vec<u8> {
        let mut reply = 0u32.to_be_bytes().to_vec();
        reply.extend_from_slice(&transaction.to_be_bytes());
        reply.extend_from_slice(&connection_id.to_be_bytes());
        reply
    }

    fn announce_response(transaction: u32) -> Vec<u8> {
        let mut reply = 1u32.to_be_bytes().to_vec();
        reply.extend_from_slice(&transaction.to_be_bytes());
        reply.extend_from_slice(&1800u32.to_be_bytes()); // interval
        reply.extend_from_slice(&7u32.to_be_bytes()); // leechers
        reply.extend_from_slice(&3u32.to_be_bytes()); // seeders
        reply.extend_from_slice(&[127, 0, 0, 1, 0x1A, 0xE1]); // 127.0.0.1:6881
        reply.extend_from_slice(&[10, 0, 0, 5, 0xC8, 0xD5]); // 10.0.0.5:51413
        reply
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::Ordering;

    use super::testing::{UdpBehaviour, http_ok, http_response, http_server, udp_tracker};
    use super::*;
    use crate::tracker::http::HttpConfig;
    use crate::tracker::udp::UdpConfig;

    fn request() -> AnnounceRequest {
        AnnounceRequest {
            info_hash: [0xAB; 20],
            peer_id: [0xCD; 20],
            port: 6881,
            uploaded: 10,
            downloaded: 20,
            left: 30,
            event: Some(Event::Started),
            num_want: Some(40),
            key: None,
        }
    }

    fn announce_body() -> Vec<u8> {
        http_ok(b"d8:completei3e10:incompletei7e8:intervali600e5:peers6:\x7f\x00\x00\x01\x1a\xe1e")
    }

    fn config() -> AnnouncerConfig {
        AnnouncerConfig {
            failure_backoff_base: Duration::from_secs(30),
            failure_backoff_max: Duration::from_secs(120),
            ..AnnouncerConfig::default()
        }
    }

    #[test]
    fn scheme_parsing_is_case_insensitive() {
        assert_eq!(Scheme::parse("http://t/announce"), Some(Scheme::Http));
        assert_eq!(Scheme::parse("HTTPS://t/a"), Some(Scheme::Https));
        assert_eq!(Scheme::parse("  udp://t:1/announce"), Some(Scheme::Udp));
        assert_eq!(Scheme::parse("ws://t/announce"), None);
        assert_eq!(Scheme::parse("tracker.example"), None);
        assert!(Scheme::Http.is_http());
        assert!(Scheme::Https.is_http());
        assert!(!Scheme::Udp.is_http());
    }

    #[test]
    fn event_codes_match_the_specs() {
        assert_eq!(Event::Started.http_str(), "started");
        assert_eq!(Event::Completed.udp_code(), 1);
        assert_eq!(Event::Started.udp_code(), 2);
        assert_eq!(Event::Stopped.udp_code(), 3);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let config = config();
        assert_eq!(config.backoff(1), Duration::from_secs(30));
        assert_eq!(config.backoff(2), Duration::from_secs(60));
        assert_eq!(config.backoff(3), Duration::from_secs(120));
        assert_eq!(config.backoff(4), Duration::from_secs(120));
        assert_eq!(config.backoff(u32::MAX), Duration::from_secs(120));
    }

    #[test]
    fn announcer_rejects_unsupported_schemes_and_empty_lists() {
        let error = Announcer::new(
            vec![vec!["wss://tracker.example/announce".to_owned()]],
            config(),
        )
        .unwrap_err();
        assert!(matches!(error, TrackerError::UnsupportedScheme { .. }));

        let error = Announcer::new(vec![vec![String::new()]], config()).unwrap_err();
        assert!(matches!(error, TrackerError::InvalidUrl { .. }));
    }

    #[test]
    fn next_announce_is_due_immediately_for_a_new_announcer() {
        let announcer = Announcer::new(
            vec![vec!["http://127.0.0.1:1/announce".to_owned()]],
            config(),
        )
        .unwrap();
        assert_eq!(announcer.tracker_count(), 1);
        assert_eq!(
            announcer.next_announce_in(Duration::ZERO),
            Some(Duration::ZERO)
        );
        assert!(announcer.is_due(Duration::ZERO));
        assert_eq!(
            announcer.next_announce_in(Duration::from_secs(1)),
            Some(Duration::ZERO)
        );
    }

    #[tokio::test]
    async fn announces_and_schedules_the_next_attempt() {
        let server = http_server(announce_body()).await;
        let mut announcer = Announcer::new(vec![vec![server.url.clone()]], config()).unwrap();
        let now = Duration::from_secs(100);

        let outcome = announcer.announce(&request(), now).await.unwrap();
        assert_eq!(outcome.tier, 0);
        assert_eq!(outcome.url, server.url);
        assert_eq!(outcome.response.interval, 600);
        assert_eq!(outcome.response.peers.len(), 1);
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);

        // 600 s from the announce, not from zero.
        assert_eq!(
            announcer.next_announce_in(now),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            announcer
                .announce(&request(), now + Duration::from_secs(1))
                .await,
            Err(TrackerError::NotDue {
                next_in: Duration::from_secs(599)
            })
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);

        // And it comes back exactly on time.
        let outcome = announcer
            .announce(&request(), now + Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(outcome.response.interval, 600);
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failing_tracker_is_backed_off_exponentially() {
        let server = http_server(http_response("500 Internal Server Error", b"")).await;
        let mut announcer = Announcer::new(vec![vec![server.url.clone()]], config()).unwrap();

        assert!(matches!(
            announcer.announce(&request(), Duration::ZERO).await,
            Err(TrackerError::HttpStatus { status: 500 })
        ));
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            announcer.next_announce_in(Duration::from_secs(1)),
            Some(Duration::from_secs(29))
        );

        // Before the backoff expires the tracker is not contacted again.
        assert_eq!(
            announcer
                .announce(&request(), Duration::from_secs(29))
                .await,
            Err(TrackerError::NotDue {
                next_in: Duration::from_secs(1)
            })
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);

        // Exactly at the deadline it is tried again; the second failure doubles
        // the delay to 60 s.
        assert!(
            announcer
                .announce(&request(), Duration::from_secs(30))
                .await
                .is_err()
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        assert_eq!(
            announcer.next_announce_in(Duration::from_secs(30)),
            Some(Duration::from_secs(60))
        );

        // Third failure is capped at 120 s.
        assert!(
            announcer
                .announce(&request(), Duration::from_secs(90))
                .await
                .is_err()
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 3);
        assert_eq!(
            announcer.next_announce_in(Duration::from_secs(90)),
            Some(Duration::from_secs(120))
        );
    }

    #[tokio::test]
    async fn fails_over_to_the_next_tier_and_keeps_it() {
        let failing = http_server(http_response("503 Service Unavailable", b"")).await;
        let healthy = http_server(announce_body()).await;
        let mut announcer = Announcer::new(
            vec![vec![failing.url.clone()], vec![healthy.url.clone()]],
            config(),
        )
        .unwrap();

        let outcome = announcer
            .announce(&request(), Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(outcome.tier, 1);
        assert_eq!(outcome.url, healthy.url);
        assert_eq!(announcer.current_tier(), 1);
        assert_eq!(failing.hits.load(Ordering::SeqCst), 1);
        assert_eq!(healthy.hits.load(Ordering::SeqCst), 1);

        // The whole list now waits for the healthy tracker's interval, so the
        // failed tier is not hammered while another tier answers.
        assert_eq!(
            announcer.next_announce_in(Duration::ZERO),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            announcer
                .announce(&request(), Duration::from_secs(599))
                .await,
            Err(TrackerError::NotDue {
                next_in: Duration::from_secs(1)
            })
        );
        assert_eq!(failing.hits.load(Ordering::SeqCst), 1);

        // On the deadline the current tier is tried first and answers again;
        // the failing one stays untouched.
        let outcome = announcer
            .announce(&request(), Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(outcome.tier, 1);
        assert_eq!(failing.hits.load(Ordering::SeqCst), 1);
        assert_eq!(healthy.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_tracker_that_refuses_with_a_failure_reason_is_a_failure() {
        let server = http_server(http_ok(b"d14:failure reason7:privatee")).await;
        let mut announcer = Announcer::new(vec![vec![server.url]], config()).unwrap();
        assert_eq!(
            announcer.announce(&request(), Duration::ZERO).await,
            Err(TrackerError::Failure {
                reason: "private".to_owned()
            })
        );
    }

    #[tokio::test]
    async fn udp_trackers_are_routed_to_bep_15() {
        let tracker = udp_tracker(UdpBehaviour::Happy).await;
        let mut announcer = Announcer::new(vec![vec![tracker.url]], config()).unwrap();
        let outcome = announcer
            .announce(&request(), Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(outcome.response.complete, 3);
        assert_eq!(outcome.response.incomplete, 7);
        assert_eq!(outcome.response.peers.len(), 2);
    }

    #[tokio::test]
    async fn an_unreachable_http_tracker_is_an_error_not_a_panic() {
        // Nothing listens on port 1 in a test environment; depending on the
        // platform the connection is refused (transport) or silently blackholed
        // until the timeout fires.
        let mut announcer = Announcer::new(
            vec![vec!["http://127.0.0.1:1/announce".to_owned()]],
            AnnouncerConfig {
                http: HttpConfig {
                    timeout: Duration::from_millis(50),
                    ..HttpConfig::default()
                },
                udp: UdpConfig::default(),
                ..config()
            },
        )
        .unwrap();
        let error = announcer
            .announce(&request(), Duration::ZERO)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            TrackerError::Transport { .. } | TrackerError::Timeout
        ));
    }
}
