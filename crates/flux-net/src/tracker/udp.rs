//! UDP tracker announces (BEP 15).
//!
//! The exchange is two request/response pairs on one socket:
//!
//! ```text
//! connect  request  8  protocol id 0x41727101980
//!                   4  action 0
//!                   4  transaction id
//!          response 4  action 0
//!                   4  transaction id
//!                   8  connection id
//!
//! announce request  8  connection id
//!                   4  action 1
//!                   4  transaction id
//!                  20  info hash
//!                  20  peer id
//!                   8  downloaded
//!                   8  left
//!                   8  uploaded
//!                   4  event (0 none, 1 completed, 2 started, 3 stopped)
//!                   4  IP address (0 = the packet's source address)
//!                   4  key
//!                   4  num want (-1 = tracker default)
//!                   2  port
//!          response 4  action 1
//!                   4  transaction id
//!                   4  interval
//!                   4  leechers
//!                   4  seeders
//!                  6n  IPv4 peers, or 18n IPv6 peers when the tracker itself
//!                      is reached over IPv6
//! ```
//!
//! Every packet is length-checked before a field is read, and the transaction
//! id and action are verified on connect responses, announce responses and
//! error packets alike. An error packet (action 3) becomes
//! [`TrackerError::Failure`] carrying the tracker's message.
//!
//! Scrape (action 2) is not implemented: this pass only needs peer discovery,
//! and a scrape would add a second response layout with its own limits for no
//! gain to playback.
//!
//! ## Retries
//!
//! A timeout is retried with a doubling wait: `timeout × 2^attempt`, up to
//! [`UdpConfig::attempts`] total attempts (default: 15 s, 30 s, 60 s). A packet
//! that arrives but is malformed, carries the wrong transaction id or the wrong
//! action is *not* retried — it aborts the announce, because the tracker is
//! demonstrably broken or answering someone else and repeating the query would
//! not help. Only silence is retried.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::net::{UdpSocket, lookup_host};
use tokio::time::timeout as wait_for;

use super::{AnnounceRequest, AnnounceResponse, DEFAULT_INTERVAL, Event, TrackerError};

/// The BEP 15 connect magic: `0x41727101980`.
pub const PROTOCOL_ID: u64 = 0x0000_0417_2710_1980;

/// Action code for a connect request and response.
pub const ACTION_CONNECT: u32 = 0;
/// Action code for an announce request and response.
pub const ACTION_ANNOUNCE: u32 = 1;
/// Action code for an error response.
pub const ACTION_ERROR: u32 = 3;

/// Bytes in a connect request.
pub const CONNECT_REQUEST_LEN: usize = 16;
/// Bytes in a connect response.
pub const CONNECT_RESPONSE_LEN: usize = 16;
/// Bytes in an announce request.
pub const ANNOUNCE_REQUEST_LEN: usize = 98;
/// Bytes in an announce response before the peer list.
pub const ANNOUNCE_RESPONSE_HEADER_LEN: usize = 20;
/// Action + transaction id, the prefix every response shares.
pub const RESPONSE_HEADER_LEN: usize = 8;
/// Bytes in one IPv4 peer entry.
pub const IPV4_PEER_LEN: usize = 6;
/// Bytes in one IPv6 peer entry.
pub const IPV6_PEER_LEN: usize = 18;

/// Default wait before the first retry: 15 s, as recommended by BEP 15.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// Default total number of attempts (first try included): 3.
pub const DEFAULT_ATTEMPTS: u32 = 3;
/// Default receive buffer: 8 KiB, enough for 1362 IPv4 peers.
pub const DEFAULT_MAX_PACKET_BYTES: usize = 8 * 1024;

/// UDP transport settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpConfig {
    /// Wait before the first retry; doubled for every further attempt.
    pub timeout: Duration,
    /// Total attempts, first try included. Values below 1 are treated as 1.
    pub attempts: u32,
    /// Size of the receive buffer; larger datagrams are truncated by the OS.
    pub max_packet_bytes: usize,
    /// Local address to bind, or `None` for the wildcard address of the
    /// tracker's family.
    ///
    /// The daemon sets this to its configured interface address so tracker
    /// traffic obeys the same binding as every other socket (see `AGENTS.md`).
    pub bind: Option<SocketAddr>,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            attempts: DEFAULT_ATTEMPTS,
            max_packet_bytes: DEFAULT_MAX_PACKET_BYTES,
            bind: None,
        }
    }
}

/// How the peer list of an announce response is laid out.
///
/// BEP 15 has no `peers6` field: the tracker's own address family decides,
/// because a tracker reached over IPv6 returns 18-byte entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerFormat {
    /// 4-byte address + 2-byte port.
    Ipv4,
    /// 16-byte address + 2-byte port.
    Ipv6,
}

impl PeerFormat {
    /// Bytes per entry in this layout.
    #[must_use]
    pub const fn entry_len(self) -> usize {
        match self {
            Self::Ipv4 => IPV4_PEER_LEN,
            Self::Ipv6 => IPV6_PEER_LEN,
        }
    }

    /// The layout a tracker at `address` uses.
    #[must_use]
    pub fn for_addr(address: SocketAddr) -> Self {
        if address.is_ipv4() {
            Self::Ipv4
        } else {
            Self::Ipv6
        }
    }
}

/// A BEP 15 tracker client.
///
/// Stateless: each [`announce`](Self::announce) binds its own socket, so two
/// torrents never share a connection id or a receive buffer. The configuration
/// is the only state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpTracker {
    config: UdpConfig,
}

impl UdpTracker {
    /// Wraps a configuration. Nothing is opened until the first announce.
    #[must_use]
    pub fn new(config: UdpConfig) -> Self {
        Self { config }
    }

    /// The effective configuration.
    #[must_use]
    pub fn config(&self) -> &UdpConfig {
        &self.config
    }

    /// Announces to `url` (`udp://host:port/...`; the path is ignored).
    pub async fn announce(
        &self,
        url: &str,
        request: &AnnounceRequest,
    ) -> Result<AnnounceResponse, TrackerError> {
        let (host, port) = parse_url(url)?;
        let addresses = lookup_host((host.as_str(), port))
            .await
            .map_err(|error| TrackerError::Transport {
                message: format!("resolving `{host}`: {error}"),
            })?
            .collect::<Vec<_>>();
        let Some(remote) = addresses.first().copied() else {
            return Err(TrackerError::InvalidUrl {
                reason: format!("`{url}` did not resolve to an address"),
            });
        };

        let local = self.config.bind.unwrap_or_else(|| {
            if remote.is_ipv4() {
                SocketAddr::from(([0, 0, 0, 0], 0))
            } else {
                SocketAddr::from(([0u16; 8], 0))
            }
        });
        let socket = UdpSocket::bind(local).await.map_err(io_transport)?;
        // Connecting filters datagrams from anyone but the tracker, so a stray
        // packet from a port scanner can never be mistaken for a response.
        socket.connect(remote).await.map_err(io_transport)?;

        let connection_id = self.connect(&socket).await?;
        self.send_announce(
            &socket,
            connection_id,
            request,
            PeerFormat::for_addr(remote),
        )
        .await
    }

    /// Runs the connect exchange, retrying only timeouts.
    async fn connect(&self, socket: &UdpSocket) -> Result<u64, TrackerError> {
        let mut last_error = None;
        for attempt in 0..self.config.attempts.max(1) {
            let transaction_id = fastrand::u32(..);
            let packet = encode_connect_request(transaction_id);
            socket.send(&packet).await.map_err(io_transport)?;

            match self.receive(socket, attempt).await {
                Ok(response) => return parse_connect_response(&response, transaction_id),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or(TrackerError::Timeout))
    }

    /// Runs the announce exchange, retrying only timeouts.
    async fn send_announce(
        &self,
        socket: &UdpSocket,
        connection_id: u64,
        request: &AnnounceRequest,
        format: PeerFormat,
    ) -> Result<AnnounceResponse, TrackerError> {
        let mut last_error = None;
        for attempt in 0..self.config.attempts.max(1) {
            let transaction_id = fastrand::u32(..);
            let packet = encode_announce_request(connection_id, transaction_id, request);
            socket.send(&packet).await.map_err(io_transport)?;

            match self.receive(socket, attempt).await {
                Ok(response) => return parse_announce_response(&response, transaction_id, format),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or(TrackerError::Timeout))
    }

    /// Waits for one datagram, with the attempt's timeout.
    async fn receive(&self, socket: &UdpSocket, attempt: u32) -> Result<Vec<u8>, TrackerError> {
        let wait = self.config.timeout.saturating_mul(1u32 << attempt.min(16));
        let mut buffer = vec![0u8; self.config.max_packet_bytes];
        match wait_for(wait, socket.recv(&mut buffer)).await {
            Err(_elapsed) => Err(TrackerError::Timeout),
            Ok(Err(error)) => Err(io_transport(error)),
            Ok(Ok(len)) => {
                buffer.truncate(len);
                Ok(buffer)
            }
        }
    }
}

/// Maps a socket error onto [`TrackerError`].
fn io_transport(error: std::io::Error) -> TrackerError {
    TrackerError::Transport {
        message: error.to_string(),
    }
}

/// Extracts `(action, transaction id)` from any response packet.
fn packet_header(packet: &[u8]) -> Result<(u32, u32), TrackerError> {
    if packet.len() < RESPONSE_HEADER_LEN {
        return Err(TrackerError::MalformedResponse {
            reason: format!(
                "tracker packet has {} bytes, need at least {RESPONSE_HEADER_LEN}",
                packet.len()
            ),
        });
    }
    Ok((
        u32::from_be_bytes([packet[0], packet[1], packet[2], packet[3]]),
        u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
    ))
}

/// Extracts the connection id from a connect response.
pub fn parse_connect_response(packet: &[u8], transaction_id: u32) -> Result<u64, TrackerError> {
    let (action, got) = packet_header(packet)?;
    if got != transaction_id {
        return Err(TrackerError::TransactionMismatch {
            expected: transaction_id,
            got,
        });
    }
    match action {
        ACTION_ERROR => Err(error_response(packet)),
        ACTION_CONNECT => {
            if packet.len() < CONNECT_RESPONSE_LEN {
                return Err(TrackerError::MalformedResponse {
                    reason: format!(
                        "connect response has {} bytes, need {CONNECT_RESPONSE_LEN}",
                        packet.len()
                    ),
                });
            }
            let mut id = [0u8; 8];
            id.copy_from_slice(&packet[8..16]);
            Ok(u64::from_be_bytes(id))
        }
        other => Err(TrackerError::Protocol {
            reason: format!("unexpected action {other} in connect response"),
        }),
    }
}

/// Parses an announce response into the shared [`AnnounceResponse`].
///
/// `format` selects the peer entry size; see [`PeerFormat`].
pub fn parse_announce_response(
    packet: &[u8],
    transaction_id: u32,
    format: PeerFormat,
) -> Result<AnnounceResponse, TrackerError> {
    let (action, got) = packet_header(packet)?;
    if got != transaction_id {
        return Err(TrackerError::TransactionMismatch {
            expected: transaction_id,
            got,
        });
    }
    match action {
        ACTION_ERROR => Err(error_response(packet)),
        ACTION_ANNOUNCE => {
            if packet.len() < ANNOUNCE_RESPONSE_HEADER_LEN {
                return Err(TrackerError::MalformedResponse {
                    reason: format!(
                        "announce response has {} bytes, need {ANNOUNCE_RESPONSE_HEADER_LEN}",
                        packet.len()
                    ),
                });
            }
            let interval = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
            let leechers = u32::from_be_bytes([packet[12], packet[13], packet[14], packet[15]]);
            let seeders = u32::from_be_bytes([packet[16], packet[17], packet[18], packet[19]]);
            let peers = parse_peer_entries(packet.get(20..).unwrap_or_default(), format)?;
            Ok(AnnounceResponse {
                interval: if interval == 0 {
                    DEFAULT_INTERVAL
                } else {
                    interval
                },
                min_interval: None,
                tracker_id: None,
                complete: seeders,
                incomplete: leechers,
                peers,
                warning: None,
            })
        }
        other => Err(TrackerError::Protocol {
            reason: format!("unexpected action {other} in announce response"),
        }),
    }
}

/// Turns an action-3 packet into a [`TrackerError::Failure`].
fn error_response(packet: &[u8]) -> TrackerError {
    let message = packet.get(RESPONSE_HEADER_LEN..).unwrap_or_default();
    TrackerError::Failure {
        reason: String::from_utf8_lossy(message).into_owned(),
    }
}

/// Decodes the compact peer list of an announce response.
///
/// A datagram is at most 65 507 bytes, so the peer count is inherently bounded
/// by the transport; only the entry alignment needs checking.
fn parse_peer_entries(bytes: &[u8], format: PeerFormat) -> Result<Vec<SocketAddr>, TrackerError> {
    let mut peers = Vec::new();
    match format {
        PeerFormat::Ipv4 => {
            let (entries, remainder) = bytes.as_chunks::<IPV4_PEER_LEN>();
            if !remainder.is_empty() {
                return Err(ragged_peer_data(bytes.len(), IPV4_PEER_LEN));
            }
            for entry in entries {
                let port = u16::from_be_bytes([entry[4], entry[5]]);
                if port == 0 {
                    continue;
                }
                let ip = Ipv4Addr::new(entry[0], entry[1], entry[2], entry[3]);
                peers.push(SocketAddr::from((ip, port)));
            }
        }
        PeerFormat::Ipv6 => {
            let (entries, remainder) = bytes.as_chunks::<IPV6_PEER_LEN>();
            if !remainder.is_empty() {
                return Err(ragged_peer_data(bytes.len(), IPV6_PEER_LEN));
            }
            for entry in entries {
                let port = u16::from_be_bytes([entry[16], entry[17]]);
                if port == 0 {
                    continue;
                }
                let octets = <[u8; 16]>::try_from(&entry[..16]).map_err(|_| {
                    TrackerError::MalformedPeers {
                        reason: "truncated IPv6 peer entry".to_owned(),
                    }
                })?;
                peers.push(SocketAddr::from((Ipv6Addr::from(octets), port)));
            }
        }
    }
    Ok(peers)
}

/// Builds the error for a peer payload that is not a whole number of entries.
fn ragged_peer_data(len: usize, entry_len: usize) -> TrackerError {
    TrackerError::MalformedPeers {
        reason: format!("announce peer data has {len} bytes, not a multiple of {entry_len}"),
    }
}

/// Encodes the 16-byte connect request.
#[must_use]
pub fn encode_connect_request(transaction_id: u32) -> [u8; CONNECT_REQUEST_LEN] {
    let mut out = [0u8; CONNECT_REQUEST_LEN];
    out[0..8].copy_from_slice(&PROTOCOL_ID.to_be_bytes());
    out[8..12].copy_from_slice(&ACTION_CONNECT.to_be_bytes());
    out[12..16].copy_from_slice(&transaction_id.to_be_bytes());
    out
}

/// Encodes the 98-byte announce request.
#[must_use]
pub fn encode_announce_request(
    connection_id: u64,
    transaction_id: u32,
    request: &AnnounceRequest,
) -> [u8; ANNOUNCE_REQUEST_LEN] {
    let mut out = [0u8; ANNOUNCE_REQUEST_LEN];
    out[0..8].copy_from_slice(&connection_id.to_be_bytes());
    out[8..12].copy_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
    out[12..16].copy_from_slice(&transaction_id.to_be_bytes());
    out[16..36].copy_from_slice(&request.info_hash);
    out[36..56].copy_from_slice(&request.peer_id);
    out[56..64].copy_from_slice(&request.downloaded.to_be_bytes());
    out[64..72].copy_from_slice(&request.left.to_be_bytes());
    out[72..80].copy_from_slice(&request.uploaded.to_be_bytes());
    out[80..84].copy_from_slice(&request.event.map_or(0, Event::udp_code).to_be_bytes());
    // 0 means "use the source address of this packet".
    out[84..88].copy_from_slice(&0u32.to_be_bytes());
    out[88..92].copy_from_slice(&request.key.unwrap_or_default().to_be_bytes());
    out[92..96].copy_from_slice(&request.num_want.unwrap_or(-1).to_be_bytes());
    out[96..98].copy_from_slice(&request.port.to_be_bytes());
    out
}

/// Splits `udp://host:port[/path]` into its host and port.
///
/// A missing port means 80, matching HTTP's convention for announce URLs.
fn parse_url(url: &str) -> Result<(String, u16), TrackerError> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or_else(|| TrackerError::InvalidUrl {
            reason: format!("`{url}` has no scheme"),
        })?;
    if !scheme.eq_ignore_ascii_case("udp") {
        return Err(TrackerError::UnsupportedScheme {
            url: url.to_owned(),
        });
    }

    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@') // strip any user info; UDP trackers do not use it
        .next()
        .unwrap_or_default();

    let (host, port) = if let Some(inside) = authority.strip_prefix('[') {
        let (host, after) = inside
            .split_once(']')
            .ok_or_else(|| TrackerError::InvalidUrl {
                reason: format!("unterminated IPv6 literal in `{url}`"),
            })?;
        let port = match after {
            "" => 80,
            _ => match after.strip_prefix(':') {
                Some(port) => parse_port(port)?,
                None => {
                    return Err(TrackerError::InvalidUrl {
                        reason: format!("junk after IPv6 literal in `{url}`"),
                    });
                }
            },
        };
        (host.to_owned(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), parse_port(port)?),
            None => (authority.to_owned(), 80),
        }
    };

    if host.is_empty() {
        return Err(TrackerError::InvalidUrl {
            reason: format!("`{url}` has no host"),
        });
    }
    Ok((host, port))
}

/// Parses a decimal port, rejecting anything outside `u16`.
fn parse_port(port: &str) -> Result<u16, TrackerError> {
    port.parse::<u16>().map_err(|_| TrackerError::InvalidUrl {
        reason: format!("invalid port `{port}`"),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::super::testing::{TEST_CONNECTION_ID, UdpBehaviour, udp_tracker};
    use super::*;

    fn request() -> AnnounceRequest {
        AnnounceRequest {
            info_hash: [0xAB; 20],
            peer_id: [0xCD; 20],
            port: 6881,
            uploaded: 10,
            downloaded: 20,
            left: 30,
            event: Some(Event::Started),
            num_want: Some(50),
            key: Some(0xDEAD_BEEF),
        }
    }

    fn quick_config(attempts: u32) -> UdpConfig {
        UdpConfig {
            timeout: Duration::from_millis(20),
            attempts,
            ..UdpConfig::default()
        }
    }

    #[test]
    fn connect_request_layout_is_bep_15() {
        let packet = encode_connect_request(0x0102_0304);
        assert_eq!(packet.len(), CONNECT_REQUEST_LEN);
        assert_eq!(&packet[0..8], &PROTOCOL_ID.to_be_bytes());
        assert_eq!(&packet[8..12], &[0, 0, 0, 0]);
        assert_eq!(&packet[12..16], &[1, 2, 3, 4]);
    }

    #[test]
    fn announce_request_layout_is_bep_15() {
        let packet = encode_announce_request(0x1122_3344_5566_7788, 0xAABB_CCDD, &request());
        assert_eq!(packet.len(), ANNOUNCE_REQUEST_LEN);
        assert_eq!(
            &packet[0..8],
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
        );
        assert_eq!(&packet[8..12], &[0, 0, 0, 1]);
        assert_eq!(&packet[12..16], &[0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(&packet[16..36], &[0xAB; 20]);
        assert_eq!(&packet[36..56], &[0xCD; 20]);
        assert_eq!(&packet[56..64], &20u64.to_be_bytes()); // downloaded
        assert_eq!(&packet[64..72], &30u64.to_be_bytes()); // left
        assert_eq!(&packet[72..80], &10u64.to_be_bytes()); // uploaded
        assert_eq!(&packet[80..84], &2u32.to_be_bytes()); // started
        assert_eq!(&packet[84..88], &[0, 0, 0, 0]); // use source address
        assert_eq!(&packet[88..92], &0xDEAD_BEEFu32.to_be_bytes());
        assert_eq!(&packet[92..96], &50i32.to_be_bytes());
        assert_eq!(&packet[96..98], &6881u16.to_be_bytes());
    }

    #[test]
    fn announce_request_omits_event_and_uses_defaults() {
        let mut request = request();
        request.event = None;
        request.num_want = None;
        request.key = None;
        let packet = encode_announce_request(0, 0, &request);
        assert_eq!(&packet[80..84], &[0, 0, 0, 0]);
        assert_eq!(&packet[88..92], &[0, 0, 0, 0]);
        assert_eq!(&packet[92..96], &(-1i32).to_be_bytes());
    }

    #[test]
    fn connect_response_parsing_checks_length_transaction_and_action() {
        let mut packet = 0u32.to_be_bytes().to_vec();
        packet.extend_from_slice(&7u32.to_be_bytes());
        packet.extend_from_slice(&TEST_CONNECTION_ID.to_be_bytes());
        assert_eq!(
            parse_connect_response(&packet, 7).unwrap(),
            TEST_CONNECTION_ID
        );

        assert!(matches!(
            parse_connect_response(&packet, 8),
            Err(TrackerError::TransactionMismatch {
                expected: 8,
                got: 7
            })
        ));
        assert!(matches!(
            parse_connect_response(&packet[..8], 7),
            Err(TrackerError::MalformedResponse { .. })
        ));
        assert!(matches!(
            parse_connect_response(&[0, 0, 0], 7),
            Err(TrackerError::MalformedResponse { .. })
        ));

        let mut wrong_action = 2u32.to_be_bytes().to_vec();
        wrong_action.extend_from_slice(&7u32.to_be_bytes());
        wrong_action.extend_from_slice(&TEST_CONNECTION_ID.to_be_bytes());
        assert!(matches!(
            parse_connect_response(&wrong_action, 7),
            Err(TrackerError::Protocol { .. })
        ));

        let mut error = 3u32.to_be_bytes().to_vec();
        error.extend_from_slice(&7u32.to_be_bytes());
        error.extend_from_slice(b"no such torrent");
        assert_eq!(
            parse_connect_response(&error, 7),
            Err(TrackerError::Failure {
                reason: "no such torrent".to_owned()
            })
        );
    }

    #[test]
    fn announce_response_parsing_handles_ipv4_and_ipv6_entries() {
        let mut packet = vec![0, 0, 0, 1];
        packet.extend_from_slice(&42u32.to_be_bytes());
        packet.extend_from_slice(&1800u32.to_be_bytes()); // interval
        packet.extend_from_slice(&7u32.to_be_bytes()); // leechers
        packet.extend_from_slice(&3u32.to_be_bytes()); // seeders
        packet.extend_from_slice(&[127, 0, 0, 1, 0x1A, 0xE1]);

        let response = parse_announce_response(&packet, 42, PeerFormat::Ipv4).unwrap();
        assert_eq!(response.interval, 1800);
        assert_eq!(response.incomplete, 7);
        assert_eq!(response.complete, 3);
        assert_eq!(response.peers, vec!["127.0.0.1:6881".parse().unwrap()]);

        let mut packet6 = packet[..20].to_vec();
        let mut octets = [0u8; 16];
        octets[0] = 0x20;
        octets[1] = 0x01;
        octets[15] = 0x01;
        packet6.extend_from_slice(&octets);
        packet6.extend_from_slice(&7000u16.to_be_bytes());
        let response = parse_announce_response(&packet6, 42, PeerFormat::Ipv6).unwrap();
        assert_eq!(response.peers, vec!["[2001::1]:7000".parse().unwrap()]);

        // A peer payload that is not a whole number of entries is rejected
        // rather than silently dropped, whichever layout is in use.
        let mut ragged = packet[..20].to_vec();
        ragged.extend_from_slice(&[0u8; 7]);
        assert!(matches!(
            parse_announce_response(&ragged, 42, PeerFormat::Ipv4),
            Err(TrackerError::MalformedPeers { .. })
        ));
        assert!(matches!(
            parse_announce_response(&ragged, 42, PeerFormat::Ipv6),
            Err(TrackerError::MalformedPeers { .. })
        ));
    }

    #[test]
    fn announce_response_parsing_checks_transaction_action_and_length() {
        let packet = [0u8, 0, 0, 1, 0, 0, 0, 9];
        assert!(matches!(
            parse_announce_response(&packet, 8, PeerFormat::Ipv4),
            Err(TrackerError::TransactionMismatch { .. })
        ));
        // Correct transaction id, but only the 8-byte header.
        assert!(matches!(
            parse_announce_response(&packet, 9, PeerFormat::Ipv4),
            Err(TrackerError::MalformedResponse { .. })
        ));

        let mut wrong_action = 2u32.to_be_bytes().to_vec();
        wrong_action.extend_from_slice(&9u32.to_be_bytes());
        wrong_action.extend_from_slice(&[0u8; 12]);
        assert!(matches!(
            parse_announce_response(&wrong_action, 9, PeerFormat::Ipv4),
            Err(TrackerError::Protocol { .. })
        ));

        // A zero interval falls back to the documented default.
        let mut zero_interval = 1u32.to_be_bytes().to_vec();
        zero_interval.extend_from_slice(&9u32.to_be_bytes());
        zero_interval.extend_from_slice(&[0u8; 12]);
        let response = parse_announce_response(&zero_interval, 9, PeerFormat::Ipv4).unwrap();
        assert_eq!(response.interval, DEFAULT_INTERVAL);
        assert!(response.peers.is_empty());
    }

    #[test]
    fn udp_urls_are_parsed_strictly() {
        assert_eq!(
            parse_url("udp://tracker.example:1337/announce").unwrap(),
            ("tracker.example".to_owned(), 1337)
        );
        assert_eq!(
            parse_url("UDP://tracker.example/announce").unwrap(),
            ("tracker.example".to_owned(), 80)
        );
        assert_eq!(
            parse_url("udp://[2001:db8::1]:6969").unwrap(),
            ("2001:db8::1".to_owned(), 6969)
        );
        assert_eq!(
            parse_url("udp://user:pass@tracker.example:6969").unwrap(),
            ("tracker.example".to_owned(), 6969)
        );

        assert!(matches!(
            parse_url("http://tracker.example/announce"),
            Err(TrackerError::UnsupportedScheme { .. })
        ));
        assert!(matches!(
            parse_url("tracker.example"),
            Err(TrackerError::InvalidUrl { .. })
        ));
        assert!(matches!(
            parse_url("udp://tracker.example:not-a-port"),
            Err(TrackerError::InvalidUrl { .. })
        ));
        assert!(matches!(
            parse_url("udp://:6969"),
            Err(TrackerError::InvalidUrl { .. })
        ));
    }

    #[tokio::test]
    async fn connect_then_announce_against_a_local_tracker() {
        let tracker = udp_tracker(UdpBehaviour::Happy).await;
        let client = UdpTracker::new(quick_config(3));
        let response = client.announce(&tracker.url, &request()).await.unwrap();
        assert_eq!(response.interval, 1800);
        assert_eq!(response.complete, 3);
        assert_eq!(response.incomplete, 7);
        assert_eq!(
            response.peers,
            vec![
                "127.0.0.1:6881".parse().unwrap(),
                "10.0.0.5:51413".parse().unwrap()
            ]
        );
        // Two datagrams: connect, then announce.
        assert_eq!(tracker.hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn wrong_transaction_id_is_rejected() {
        let tracker = udp_tracker(UdpBehaviour::WrongTransaction).await;
        let client = UdpTracker::new(quick_config(3));
        let error = client.announce(&tracker.url, &request()).await.unwrap_err();
        match error {
            TrackerError::TransactionMismatch { expected, got } => {
                assert_eq!(got, expected.wrapping_add(1));
            }
            other => panic!("expected a transaction mismatch, got {other:?}"),
        }
        // The mismatch aborts immediately; it is not retried.
        assert_eq!(tracker.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn wrong_action_is_rejected() {
        let tracker = udp_tracker(UdpBehaviour::WrongAction).await;
        let client = UdpTracker::new(quick_config(3));
        assert!(matches!(
            client.announce(&tracker.url, &request()).await,
            Err(TrackerError::Protocol { .. })
        ));
    }

    #[tokio::test]
    async fn short_packets_are_rejected() {
        let tracker = udp_tracker(UdpBehaviour::ShortPacket).await;
        let client = UdpTracker::new(quick_config(3));
        assert!(matches!(
            client.announce(&tracker.url, &request()).await,
            Err(TrackerError::MalformedResponse { .. })
        ));
    }

    #[tokio::test]
    async fn silence_is_retried_exactly_attempts_times() {
        let tracker = udp_tracker(UdpBehaviour::Silent).await;
        let config = quick_config(3);
        let client = UdpTracker::new(config);
        assert_eq!(
            client.announce(&tracker.url, &request()).await,
            Err(TrackerError::Timeout)
        );

        // The third datagram may still be in flight when the last timeout
        // fires; wait briefly for the tracker task to count it.
        for _ in 0..100 {
            if tracker.hits.load(Ordering::SeqCst) >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(tracker.hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_single_attempt_is_configurable() {
        let tracker = udp_tracker(UdpBehaviour::Silent).await;
        let client = UdpTracker::new(quick_config(1));
        assert_eq!(
            client.announce(&tracker.url, &request()).await,
            Err(TrackerError::Timeout)
        );
        for _ in 0..100 {
            if tracker.hits.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(tracker.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn http_urls_are_rejected_by_the_udp_client() {
        let client = UdpTracker::new(quick_config(1));
        assert!(matches!(
            client
                .announce("http://127.0.0.1:1/announce", &request())
                .await,
            Err(TrackerError::UnsupportedScheme { .. })
        ));
    }
}
