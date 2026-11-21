//! HTTP(S) tracker announces (BEP 3).
//!
//! Two details in this module exist because clients routinely get them wrong:
//!
//! 1. `info_hash` and `peer_id` are **20 raw bytes**. They are percent-encoded
//!    one byte at a time with the unreserved set of RFC 3986, so a hash byte of
//!    `0x2B` becomes `%2B` and a space byte becomes `%20` — never `+`, and
//!    never a UTF-8 sequence. [`percent_encode`] is the only encoder used, and
//!    the tests pin the exact query bytes.
//! 2. `peers` is a compact string of 6-byte big-endian `IPv4:port` entries and
//!    `peers6` is 18-byte `IPv6:port` entries. A length that is not a multiple
//!    of the entry size is rejected instead of being silently truncated. The
//!    original dictionary model (`peers` as a list of `ip`/`port` dictionaries)
//!    is accepted as a fallback.
//!
//! Responses are decoded with [`flux_core::bencode`], which is strict about
//! canonical keys; a tracker that sends a duplicate or unsorted key is treated
//! as malformed. The body is capped before a single byte of it is parsed.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use flux_core::bencode::{self, Value};

use super::{AnnounceRequest, AnnounceResponse, DEFAULT_INTERVAL, TrackerError};

/// Default time limit for one HTTP announce: 30 s.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default number of redirects a tracker URL may send us through: 5.
pub const DEFAULT_MAX_REDIRECTS: usize = 5;

/// Default response body cap: 1 MiB.
///
/// A tracker response that is legitimately bigger than this is a peer list of
/// roughly 170 000 addresses, far beyond what a client asks for.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Default `User-Agent`, taken from the crate version.
pub const DEFAULT_USER_AGENT: &str = concat!("OxideFlux/", env!("CARGO_PKG_VERSION"));

/// Upper bound on peers accepted from one response.
///
/// The body cap already bounds the bytes; this bounds the vector even when a
/// tracker sends the same addresses over and over.
pub const MAX_PEERS: usize = 10_000;

/// Bytes in one compact IPv4 peer entry.
pub const COMPACT_IPV4_PEER_LEN: usize = 6;

/// Bytes in one compact IPv6 (`peers6`) entry.
pub const COMPACT_IPV6_PEER_LEN: usize = 18;

/// HTTP transport settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    /// Total time allowed for one announce, redirects included.
    pub timeout: Duration,
    /// Number of redirects to follow before giving up.
    pub max_redirects: usize,
    /// Largest response body that will be parsed.
    pub max_body_bytes: usize,
    /// `User-Agent` sent with every announce.
    pub user_agent: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }
}

/// An HTTP(S) tracker client.
///
/// The underlying [`reqwest::Client`] is built once and reused: it owns the
/// connection pool, the timeout, the redirect policy and the TLS stack
/// (`rustls`, so no system OpenSSL is required). `https` is used only when the
/// announce URL says so; plain `http` is never upgraded or downgraded.
#[derive(Debug, Clone)]
pub struct HttpTracker {
    client: reqwest::Client,
    config: HttpConfig,
}

impl HttpTracker {
    /// Builds a client from `config`.
    ///
    /// Fails only if the TLS backend or the client configuration itself cannot
    /// be initialised.
    pub fn new(config: HttpConfig) -> Result<Self, TrackerError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::limited(config.max_redirects))
            .user_agent(config.user_agent.clone())
            .build()
            .map_err(transport_error)?;
        Ok(Self { client, config })
    }

    /// The effective configuration.
    #[must_use]
    pub fn config(&self) -> &HttpConfig {
        &self.config
    }

    /// Sends one announce and parses the answer.
    ///
    /// `tracker_id` is the opaque id from the previous response, echoed as the
    /// `trackerid` parameter when present.
    pub async fn announce(
        &self,
        base_url: &str,
        request: &AnnounceRequest,
        tracker_id: Option<&[u8]>,
    ) -> Result<AnnounceResponse, TrackerError> {
        let Some(scheme) = super::Scheme::parse(base_url) else {
            return Err(TrackerError::UnsupportedScheme {
                url: base_url.to_owned(),
            });
        };
        if !scheme.is_http() {
            return Err(TrackerError::UnsupportedScheme {
                url: base_url.to_owned(),
            });
        }

        let url = build_url(base_url, request, tracker_id)?;
        let mut response = self.client.get(url).send().await.map_err(transport_error)?;

        let status = response.status();
        if !status.is_success() {
            return Err(TrackerError::HttpStatus {
                status: status.as_u16(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.config.max_body_bytes as u64)
        {
            return Err(TrackerError::ResponseTooLarge {
                limit: self.config.max_body_bytes,
            });
        }

        // Accumulate chunk by chunk and stop at the cap: the declared length is
        // never used to size an allocation, and a chunked response without a
        // declared length is bounded too.
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if body.len().saturating_add(chunk.len()) > self.config.max_body_bytes {
                return Err(TrackerError::ResponseTooLarge {
                    limit: self.config.max_body_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }

        parse_response(&body)
    }
}

/// Maps a [`reqwest::Error`] onto [`TrackerError`].
fn transport_error(error: reqwest::Error) -> TrackerError {
    if error.is_timeout() {
        TrackerError::Timeout
    } else {
        TrackerError::Transport {
            message: error.to_string(),
        }
    }
}

/// Builds the full announce URL, appending the query to any the tracker URL
/// already carries (passkeys live there).
pub fn build_url(
    base_url: &str,
    request: &AnnounceRequest,
    tracker_id: Option<&[u8]>,
) -> Result<String, TrackerError> {
    let base = base_url.trim();
    if base.is_empty() {
        return Err(TrackerError::InvalidUrl {
            reason: "empty tracker URL".to_owned(),
        });
    }
    if base.contains('#') {
        return Err(TrackerError::InvalidUrl {
            reason: "tracker URLs with a fragment are not supported".to_owned(),
        });
    }
    if base
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(TrackerError::InvalidUrl {
            reason: "tracker URL contains whitespace or control characters".to_owned(),
        });
    }

    let separator = match base.rfind('?') {
        None => "?",
        Some(_) if base.ends_with('?') || base.ends_with('&') => "",
        Some(_) => "&",
    };
    let query = build_query(request, tracker_id);
    Ok(format!("{base}{separator}{query}"))
}

/// Builds the BEP 3 query string without the leading `?`.
///
/// Parameter order follows the BEP: identity, transfer counters, event, then
/// the optional hints. Every value is ASCII; only the two identity fields need
/// percent-encoding, because they are binary.
#[must_use]
pub fn build_query(request: &AnnounceRequest, tracker_id: Option<&[u8]>) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(11);
    parts.push(format!("info_hash={}", percent_encode(&request.info_hash)));
    parts.push(format!("peer_id={}", percent_encode(&request.peer_id)));
    parts.push(format!("port={}", request.port));
    parts.push(format!("uploaded={}", request.uploaded));
    parts.push(format!("downloaded={}", request.downloaded));
    parts.push(format!("left={}", request.left));
    if let Some(event) = request.event {
        parts.push(format!("event={}", event.http_str()));
    }
    // Compact peer lists are the only form worth asking for; `peers6` comes on
    // the same response whenever the tracker has IPv6 peers.
    parts.push("compact=1".to_owned());
    if let Some(num_want) = request.num_want {
        parts.push(format!("numwant={num_want}"));
    }
    if let Some(key) = request.key {
        parts.push(format!("key={key:08x}"));
    }
    if let Some(tracker_id) = tracker_id {
        parts.push(format!("trackerid={}", percent_encode(tracker_id)));
    }
    parts.join("&")
}

/// Percent-encodes every byte outside the RFC 3986 unreserved set.
///
/// Unreserved bytes (`A-Z a-z 0-9 - . _ ~`) pass through; everything else
/// becomes `%XX` with uppercase hex. This is byte-oriented on purpose: the
/// input may be invalid UTF-8, and a space must become `%20`, never `+`.
#[must_use]
pub fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut out = String::with_capacity(bytes.len() * 3);
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            // ASCII by construction, so a single byte is a single character.
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }
    out
}

/// Parses an HTTP tracker response body.
///
/// `failure reason` is fatal (returned as [`TrackerError::Failure`]); `warning
/// message` is carried in [`AnnounceResponse::warning`]. Missing `complete` or
/// `incomplete` counters read as zero, and a missing or zero `interval` falls
/// back to [`DEFAULT_INTERVAL`].
pub fn parse_response(body: &[u8]) -> Result<AnnounceResponse, TrackerError> {
    let value = bencode::decode(body)?;
    if value.as_dict().is_none() {
        return Err(TrackerError::MalformedResponse {
            reason: "response is not a dictionary".to_owned(),
        });
    }
    if let Some(reason) = bytes_field(&value, b"failure reason")? {
        return Err(TrackerError::Failure {
            reason: String::from_utf8_lossy(reason).into_owned(),
        });
    }

    let interval = u32_field(&value, b"interval")?
        .filter(|interval| *interval != 0)
        .unwrap_or(DEFAULT_INTERVAL);
    let min_interval = u32_field(&value, b"min interval")?;
    let complete = u32_field(&value, b"complete")?.unwrap_or(0);
    let incomplete = u32_field(&value, b"incomplete")?.unwrap_or(0);
    let tracker_id = bytes_field(&value, b"tracker id")?.map(<[u8]>::to_vec);
    let warning = bytes_field(&value, b"warning message")?
        .map(|text| String::from_utf8_lossy(text).into_owned());

    let mut peers = Vec::new();
    let mut seen = HashSet::new();
    if let Some(peers_value) = value.get(b"peers") {
        match peers_value {
            Value::Bytes(compact) => {
                parse_compact_v4(compact, &mut peers, &mut seen)?;
            }
            Value::List(entries) => parse_dict_peers(entries, &mut peers, &mut seen),
            _ => {
                return Err(TrackerError::MalformedResponse {
                    reason: "`peers` is neither a byte string nor a list".to_owned(),
                });
            }
        }
    }
    if let Some(peers6) = bytes_field(&value, b"peers6")? {
        parse_compact_v6(peers6, &mut peers, &mut seen)?;
    }

    Ok(AnnounceResponse {
        interval,
        min_interval,
        tracker_id,
        complete,
        incomplete,
        peers,
        warning,
    })
}

/// Reads an optional byte-string field, rejecting a value of the wrong type.
fn bytes_field<'a>(value: &'a Value, key: &[u8]) -> Result<Option<&'a [u8]>, TrackerError> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::Bytes(bytes)) => Ok(Some(bytes)),
        Some(_) => Err(TrackerError::MalformedResponse {
            reason: format!("`{}` is not a byte string", String::from_utf8_lossy(key)),
        }),
    }
}

/// Reads an optional integer field, rejecting negatives and values above
/// `u32::MAX`.
fn u32_field(value: &Value, key: &[u8]) -> Result<Option<u32>, TrackerError> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::Int(number)) => {
            u32::try_from(*number)
                .map(Some)
                .map_err(|_| TrackerError::MalformedResponse {
                    reason: format!(
                        "`{}` is outside the u32 range: {number}",
                        String::from_utf8_lossy(key)
                    ),
                })
        }
        Some(_) => Err(TrackerError::MalformedResponse {
            reason: format!("`{}` is not an integer", String::from_utf8_lossy(key)),
        }),
    }
}

/// Adds `address` unless it is already present or the list is full.
fn push_peer(peers: &mut Vec<SocketAddr>, seen: &mut HashSet<SocketAddr>, address: SocketAddr) {
    if peers.len() >= MAX_PEERS {
        return;
    }
    if seen.insert(address) {
        peers.push(address);
    }
}

/// Decodes `4-byte IPv4 + 2-byte port` entries.
fn parse_compact_v4(
    bytes: &[u8],
    peers: &mut Vec<SocketAddr>,
    seen: &mut HashSet<SocketAddr>,
) -> Result<(), TrackerError> {
    let (entries, remainder) = bytes.as_chunks::<COMPACT_IPV4_PEER_LEN>();
    if !remainder.is_empty() {
        return Err(TrackerError::MalformedPeers {
            reason: format!(
                "compact `peers` has {} bytes, not a multiple of {COMPACT_IPV4_PEER_LEN}",
                bytes.len()
            ),
        });
    }
    for entry in entries {
        let ip = Ipv4Addr::new(entry[0], entry[1], entry[2], entry[3]);
        let port = u16::from_be_bytes([entry[4], entry[5]]);
        if port == 0 {
            continue;
        }
        push_peer(peers, seen, SocketAddr::new(IpAddr::V4(ip), port));
    }
    Ok(())
}

/// Decodes `16-byte IPv6 + 2-byte port` entries.
fn parse_compact_v6(
    bytes: &[u8],
    peers: &mut Vec<SocketAddr>,
    seen: &mut HashSet<SocketAddr>,
) -> Result<(), TrackerError> {
    let (entries, remainder) = bytes.as_chunks::<COMPACT_IPV6_PEER_LEN>();
    if !remainder.is_empty() {
        return Err(TrackerError::MalformedPeers {
            reason: format!(
                "compact `peers6` has {} bytes, not a multiple of {COMPACT_IPV6_PEER_LEN}",
                bytes.len()
            ),
        });
    }
    for entry in entries {
        let octets: [u8; 16] =
            entry[..16]
                .try_into()
                .map_err(|_| TrackerError::MalformedPeers {
                    reason: "truncated `peers6` entry".to_owned(),
                })?;
        let ip = Ipv6Addr::from(octets);
        let port = u16::from_be_bytes([entry[16], entry[17]]);
        if port == 0 {
            continue;
        }
        push_peer(peers, seen, SocketAddr::new(IpAddr::V6(ip), port));
    }
    Ok(())
}

/// Decodes the original dictionary peer model.
///
/// Entries that are not dictionaries, lack `ip`/`port`, or name a host instead
/// of an IP literal are skipped: parsing must never trigger DNS. This is a
/// fallback for trackers without compact support, so being lenient here cannot
/// hide a protocol error the way a silent compact-truncation would.
fn parse_dict_peers(
    entries: &[Value],
    peers: &mut Vec<SocketAddr>,
    seen: &mut HashSet<SocketAddr>,
) {
    for entry in entries {
        let (Some(ip), Some(port)) = (
            entry.get(b"ip").and_then(Value::as_bytes),
            entry.get(b"port").and_then(Value::as_int),
        ) else {
            continue;
        };
        let Ok(ip) = std::str::from_utf8(ip) else {
            continue;
        };
        let Ok(ip) = ip.parse::<IpAddr>() else {
            continue;
        };
        let Ok(port) = u16::try_from(port) else {
            continue;
        };
        if port == 0 {
            continue;
        }
        push_peer(peers, seen, SocketAddr::new(ip, port));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::super::testing::{http_ok, http_response, http_server};
    use super::*;
    use crate::tracker::Event;

    /// A 20-byte info hash that is not valid UTF-8 and contains every byte the
    /// query builder has to treat specially.
    const HASH: [u8; 20] = [
        0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x00, 0x01, 0x20, 0x2B, 0x25, 0x2F, 0x3F,
        0x23, 0x7E, 0xFF, 0x80, 0x0A,
    ];

    fn request() -> AnnounceRequest {
        AnnounceRequest {
            info_hash: HASH,
            peer_id: *b"-OF0001-abcdefgh1234",
            port: 6881,
            uploaded: 0,
            downloaded: 12,
            left: 34,
            event: Some(Event::Completed),
            num_want: Some(-1),
            key: Some(0xDEAD_BEEF),
        }
    }

    #[test]
    fn percent_encoding_is_byte_exact() {
        assert_eq!(
            percent_encode(&HASH),
            "%124Vx%9A%BC%DE%F0%00%01%20%2B%25%2F%3F%23~%FF%80%0A"
        );
        assert_eq!(percent_encode(b"a b+c"), "a%20b%2Bc");
        assert_eq!(percent_encode(b"-._~AZaz09"), "-._~AZaz09");
        assert_eq!(percent_encode(&[0x00]), "%00");
        assert_eq!(percent_encode(&[0x7F, 0x80]), "%7F%80");
        // No UTF-8 expansion and no form-encoding `+` anywhere.
        assert!(percent_encode(&HASH).is_ascii());
        assert!(!percent_encode(b" ").contains('+'));
    }

    #[test]
    fn query_is_byte_exact_and_never_uses_plus() {
        let query = build_query(&request(), None);
        assert_eq!(
            query,
            "info_hash=%124Vx%9A%BC%DE%F0%00%01%20%2B%25%2F%3F%23~%FF%80%0A\
             &peer_id=-OF0001-abcdefgh1234\
             &port=6881\
             &uploaded=0\
             &downloaded=12\
             &left=34\
             &event=completed\
             &compact=1\
             &numwant=-1\
             &key=deadbeef"
        );
        assert!(!query.contains('+'));
    }

    #[test]
    fn tracker_id_is_echoed_percent_encoded() {
        let query = build_query(&request(), Some(b"a b+c\xFF"));
        assert!(query.ends_with("&trackerid=a%20b%2Bc%FF"));
    }

    #[test]
    fn query_includes_event_only_when_present() {
        let mut request = request();
        request.event = None;
        request.num_want = None;
        request.key = None;
        let query = build_query(&request, None);
        assert!(!query.contains("event="));
        assert!(!query.contains("numwant="));
        assert!(!query.contains("key="));
        assert!(query.contains("compact=1"));
    }

    #[test]
    fn url_appends_to_existing_queries() {
        let url = build_url("http://t/announce?passkey=abc", &request(), None).unwrap();
        assert!(url.starts_with("http://t/announce?passkey=abc&info_hash="));

        let url = build_url("http://t/announce?", &request(), None).unwrap();
        assert!(url.starts_with("http://t/announce?info_hash="));

        let url = build_url("http://t/announce", &request(), None).unwrap();
        assert!(url.starts_with("http://t/announce?info_hash="));

        assert!(matches!(
            build_url("http://t/announce#frag", &request(), None),
            Err(TrackerError::InvalidUrl { .. })
        ));
        assert!(matches!(
            build_url("   ", &request(), None),
            Err(TrackerError::InvalidUrl { .. })
        ));
    }

    /// Builds a response from `complete`, `incomplete`, `interval` and a raw
    /// compact `peers` payload.
    fn compact_body(complete: i64, incomplete: i64, interval: i64, peers: &[u8]) -> Vec<u8> {
        let mut body = format!(
            "d8:completei{complete}e10:incompletei{incomplete}e8:intervali{interval}e5:peers{}:",
            peers.len()
        )
        .into_bytes();
        body.extend_from_slice(peers);
        body.push(b'e');
        body
    }

    #[test]
    fn parses_compact_ipv4_peers() {
        let body = compact_body(
            5,
            7,
            1800,
            &[127, 0, 0, 1, 0x1A, 0xE1, 10, 0, 0, 5, 0xC8, 0xD5],
        );
        let response = parse_response(&body).unwrap();
        assert_eq!(response.interval, 1800);
        assert_eq!(response.min_interval, None);
        assert_eq!(response.complete, 5);
        assert_eq!(response.incomplete, 7);
        assert_eq!(
            response.peers,
            vec![
                "127.0.0.1:6881".parse().unwrap(),
                "10.0.0.5:51413".parse().unwrap()
            ]
        );
        assert_eq!(response.warning, None);
    }

    #[test]
    fn compact_peers_are_deduplicated_and_zero_ports_dropped() {
        let body = compact_body(
            0,
            0,
            60,
            &[
                127, 0, 0, 1, 0x1A, 0xE1, // duplicate
                127, 0, 0, 1, 0x1A, 0xE1, // port 0
                5, 6, 7, 8, 0, 0,
            ],
        );
        let response = parse_response(&body).unwrap();
        assert_eq!(response.peers, vec!["127.0.0.1:6881".parse().unwrap()]);
    }

    #[test]
    fn parses_compact_ipv6_peers() {
        let mut entry = [0u8; 18];
        entry[0] = 0x20;
        entry[1] = 0x01;
        entry[15] = 0x01;
        entry[16] = 0x1A;
        entry[17] = 0xE1;
        let mut body =
            b"d8:completei0e10:incompletei0e8:intervali900e5:peers0:6:peers618:".to_vec();
        body.extend_from_slice(&entry);
        body.push(b'e');

        let response = parse_response(&body).unwrap();
        assert_eq!(response.interval, 900);
        assert_eq!(response.peers, vec!["[2001::1]:6881".parse().unwrap()]);
    }

    #[test]
    fn rejects_peer_payloads_with_wrong_lengths() {
        let body = compact_body(0, 0, 60, &[1, 2, 3, 4, 5, 6, 7]);
        assert!(matches!(
            parse_response(&body),
            Err(TrackerError::MalformedPeers { .. })
        ));

        let mut body = b"d8:completei0e10:incompletei0e8:intervali60e5:peers0:6:peers617:".to_vec();
        body.extend_from_slice(&[0u8; 17]);
        body.push(b'e');
        assert!(matches!(
            parse_response(&body),
            Err(TrackerError::MalformedPeers { .. })
        ));
    }

    #[test]
    fn parses_dictionary_model_peers() {
        let peer = |ip: &str, port: i64| {
            Value::Dict(vec![
                (b"ip".to_vec(), Value::Bytes(ip.as_bytes().to_vec())),
                (b"port".to_vec(), Value::Int(port)),
            ])
        };
        let body = bencode::encode(&Value::Dict(vec![
            (b"complete".to_vec(), Value::Int(1)),
            (b"incomplete".to_vec(), Value::Int(2)),
            (b"interval".to_vec(), Value::Int(1800)),
            (
                b"peers".to_vec(),
                Value::List(vec![
                    peer("1.2.3.4", 6881),
                    peer("::1", 7000),
                    // A hostname must never trigger DNS from the parser.
                    peer("tracker.example", 80),
                    // Out-of-range ports are skipped, not panicked on.
                    peer("5.6.7.8", 70_000),
                    peer("5.6.7.9", 0),
                    Value::Int(7),
                ]),
            ),
        ]));
        let response = parse_response(&body).unwrap();
        assert_eq!(
            response.peers,
            vec![
                "1.2.3.4:6881".parse().unwrap(),
                "[::1]:7000".parse().unwrap()
            ]
        );
    }

    #[test]
    fn failure_reason_is_fatal_and_warning_is_not() {
        let error = parse_response(b"d14:failure reason7:privatee").unwrap_err();
        assert_eq!(
            error,
            TrackerError::Failure {
                reason: "private".to_owned()
            }
        );

        let mut body = compact_body(1, 2, 1800, &[127, 0, 0, 1, 0x1A, 0xE1]);
        body.pop();
        body.extend_from_slice(b"15:warning message9:slow downe");
        let response = parse_response(&body).unwrap();
        assert_eq!(response.warning.as_deref(), Some("slow down"));
        assert_eq!(response.peers.len(), 1);
    }

    #[test]
    fn missing_counters_and_interval_have_documented_defaults() {
        let response = parse_response(b"de").unwrap();
        assert_eq!(response.interval, DEFAULT_INTERVAL);
        assert_eq!(response.complete, 0);
        assert_eq!(response.incomplete, 0);
        assert!(response.peers.is_empty());

        let response = parse_response(b"d8:intervali0ee").unwrap();
        assert_eq!(response.interval, DEFAULT_INTERVAL);

        let response = parse_response(b"d8:intervali60e12:min intervali120ee").unwrap();
        assert_eq!(response.min_interval, Some(120));
    }

    #[test]
    fn rejects_non_dictionary_and_non_canonical_bodies() {
        assert!(matches!(
            parse_response(b"le"),
            Err(TrackerError::MalformedResponse { .. })
        ));
        assert!(matches!(
            parse_response(b"d1:ai1e1:ai2ee"),
            Err(TrackerError::Bencode(_))
        ));
        // An unknown-key dictionary is valid bencode but not a response.
        assert!(parse_response(b"d1:ai1ee").is_ok());
        assert!(matches!(
            parse_response(b"d8:interval2:noe"),
            Err(TrackerError::MalformedResponse { .. })
        ));
    }

    #[tokio::test]
    async fn announces_over_http() {
        let body = compact_body(4, 9, 1800, &[127, 0, 0, 1, 0x1A, 0xE1]);
        let server = http_server(http_ok(&body)).await;
        let tracker = HttpTracker::new(HttpConfig::default()).unwrap();

        let response = tracker
            .announce(&server.url, &request(), None)
            .await
            .unwrap();
        assert_eq!(response.interval, 1800);
        assert_eq!(response.complete, 4);
        assert_eq!(response.incomplete, 9);
        assert_eq!(response.peers.len(), 1);
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn http_failure_reason_surfaces_as_a_failure() {
        let server = http_server(http_ok(b"d14:failure reason11:bad passkeye")).await;
        let tracker = HttpTracker::new(HttpConfig::default()).unwrap();
        assert_eq!(
            tracker.announce(&server.url, &request(), None).await,
            Err(TrackerError::Failure {
                reason: "bad passkey".to_owned()
            })
        );
    }

    #[tokio::test]
    async fn http_error_status_is_reported() {
        let server = http_server(http_response("500 Internal Server Error", b"")).await;
        let tracker = HttpTracker::new(HttpConfig::default()).unwrap();
        assert_eq!(
            tracker.announce(&server.url, &request(), None).await,
            Err(TrackerError::HttpStatus { status: 500 })
        );
    }

    #[tokio::test]
    async fn truncated_bodies_are_a_transport_error() {
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(b"d8:completei1e");
        let server = http_server(response).await;
        let tracker = HttpTracker::new(HttpConfig::default()).unwrap();
        assert!(matches!(
            tracker.announce(&server.url, &request(), None).await,
            Err(TrackerError::Transport { .. })
        ));
    }

    #[tokio::test]
    async fn oversized_bodies_are_rejected_before_parsing() {
        let server = http_server(http_ok(&compact_body(
            0,
            0,
            60,
            &[7u8; COMPACT_IPV4_PEER_LEN * 100],
        )))
        .await;
        let tracker = HttpTracker::new(HttpConfig {
            max_body_bytes: 64,
            ..HttpConfig::default()
        })
        .unwrap();
        assert_eq!(
            tracker.announce(&server.url, &request(), None).await,
            Err(TrackerError::ResponseTooLarge { limit: 64 })
        );
    }

    #[tokio::test]
    async fn oversized_chunked_bodies_are_rejected_while_streaming() {
        let payload = compact_body(0, 0, 60, &[7u8; COMPACT_IPV4_PEER_LEN * 100]);
        let mut response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(format!("{:X}\r\n", payload.len()).as_bytes());
        response.extend_from_slice(&payload);
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        let server = http_server(response).await;
        let tracker = HttpTracker::new(HttpConfig {
            max_body_bytes: 64,
            ..HttpConfig::default()
        })
        .unwrap();
        assert_eq!(
            tracker.announce(&server.url, &request(), None).await,
            Err(TrackerError::ResponseTooLarge { limit: 64 })
        );
    }

    #[tokio::test]
    async fn redirect_loops_are_bounded() {
        let response = b"HTTP/1.1 302 Found\r\nLocation: /announce\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec();
        let server = http_server(response).await;
        let tracker = HttpTracker::new(HttpConfig {
            max_redirects: 2,
            ..HttpConfig::default()
        })
        .unwrap();
        assert!(matches!(
            tracker.announce(&server.url, &request(), None).await,
            Err(TrackerError::Transport { .. })
        ));
        assert!(server.hits.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn udp_urls_are_rejected_by_the_http_client() {
        let tracker = HttpTracker::new(HttpConfig {
            timeout: Duration::from_millis(10),
            ..HttpConfig::default()
        })
        .unwrap();
        assert!(matches!(
            tracker
                .announce("udp://127.0.0.1:1/announce", &request(), None)
                .await,
            Err(TrackerError::UnsupportedScheme { .. })
        ));
    }
}
