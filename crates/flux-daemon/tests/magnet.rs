//! Offline `ut_metadata` tests: a fake provider over loopback TCP.
//!
//! Every provider is built on [`flux_net::peer::accept`] and
//! [`flux_net::metadata`], so the client side of the fetch is exercised against
//! the real handshake and extension framing. No tracker is needed: peers are
//! passed through [`FetchSpec::seeds`].

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use flux_core::bencode::{self, Value};
use flux_daemon::magnet::{FetchEvent, FetchSpec, MagnetError, assemble_torrent, fetch};
use flux_net::bitfield::Bitfield;
use flux_net::metadata::{METADATA_PIECE_SIZE, MetadataMessage};
use flux_net::peer::{self, PeerCommand, PeerConfig, PeerEvent};
use sha1::{Digest, Sha1};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// The client's peer id, the same for every test.
const CLIENT_ID: [u8; 20] = *b"-OF0100-000000000001";
/// The provider's peer id.
const PROVIDER_ID: [u8; 20] = *b"-PF0100-000000000002";
/// The extension id the provider advertises for `ut_metadata`.
const PROVIDER_EXT_ID: u8 = 3;

fn sha1(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = [0_u8; 20];
    out.copy_from_slice(digest.as_slice());
    out
}

/// A valid single-file v1 info dictionary whose piece hashes are synthetic.
///
/// `metainfo::parse` checks that the hash count matches the layout, not that
/// the hashes describe file contents, so fake hashes are enough and keep the
/// test data small.
fn info_dict(name: &str, length: usize, piece_length: u32) -> Vec<u8> {
    let count = length.div_ceil(piece_length as usize);
    let hashes: Vec<u8> = (0..count)
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

/// How the fake provider answers `ut_metadata` requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reply {
    /// Serve the real bytes.
    Data,
    /// Refuse every request.
    Reject,
    /// Claim a 5 MiB blob, above the 4 MiB cap.
    Oversized,
    /// Say nothing at all after the handshake.
    Silent,
}

/// Starts a listener that accepts exactly one peer and runs `reply`.
///
/// Returns the address to put in [`FetchSpec::seeds`] and the task handle.
async fn spawn_provider(
    info_hash: [u8; 20],
    info: Vec<u8>,
    reply: Reply,
    send_bitfield: bool,
) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();

    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut config = PeerConfig::new(info_hash, PROVIDER_ID);
        config.pieces = 0;
        let (handle, mut events) = peer::accept(stream, config).await.unwrap();

        if send_bitfield {
            // Sent immediately after the handshake, before any extension
            // traffic: the client must acknowledge it without knowing the piece
            // count (PeerConfig::pieces == 0).
            if handle
                .send(PeerCommand::Bitfield(Bitfield::all(64)))
                .await
                .is_err()
            {
                return;
            }
        }

        if reply == Reply::Silent {
            while let Some(event) = events.recv().await {
                if matches!(event, PeerEvent::Closed(_)) {
                    break;
                }
            }
            return;
        }

        let handshake = bencode::encode(&Value::Dict(vec![
            (
                b"m".to_vec(),
                Value::Dict(vec![(
                    b"ut_metadata".to_vec(),
                    Value::Int(i64::from(PROVIDER_EXT_ID)),
                )]),
            ),
            (b"metadata_size".to_vec(), Value::Int(info.len() as i64)),
        ]));
        if handle
            .send(PeerCommand::Extended {
                id: 0,
                payload: handshake,
            })
            .await
            .is_err()
        {
            return;
        }

        while let Some(event) = events.recv().await {
            match event {
                PeerEvent::Extended { id, payload } => {
                    // The client's own extended handshake arrives as id 0.
                    if id == 0 {
                        continue;
                    }
                    // The client must address the provider with the id the
                    // provider advertised, never a hard-coded one.
                    assert_eq!(id, PROVIDER_EXT_ID);
                    let Ok(MetadataMessage::Request { piece }) = MetadataMessage::decode(&payload)
                    else {
                        continue;
                    };
                    let message = match reply {
                        Reply::Reject => MetadataMessage::Reject { piece },
                        Reply::Oversized => MetadataMessage::Data {
                            piece,
                            total_size: 5 * 1024 * 1024,
                            data: vec![0u8; METADATA_PIECE_SIZE as usize],
                        },
                        Reply::Data | Reply::Silent => {
                            let start = piece as usize * METADATA_PIECE_SIZE as usize;
                            let end = (start + METADATA_PIECE_SIZE as usize).min(info.len());
                            MetadataMessage::Data {
                                piece,
                                total_size: info.len() as u32,
                                data: info[start..end].to_vec(),
                            }
                        }
                    };
                    if handle
                        .send(PeerCommand::Extended {
                            id: PROVIDER_EXT_ID,
                            payload: message.encode(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                PeerEvent::Closed(_) => break,
                _ => {}
            }
        }
    });

    (addr, task)
}

fn spec_for(info_hash: [u8; 20], seeds: Vec<SocketAddr>) -> FetchSpec {
    let mut spec = FetchSpec::new(info_hash, CLIENT_ID, 6881);
    spec.seeds = seeds;
    spec
}

/// Awaits a provider task that is expected to end on its own.
async fn join_provider(task: JoinHandle<()>) {
    timeout(Duration::from_secs(5), task)
        .await
        .expect("provider must finish")
        .expect("provider must not panic");
}

#[tokio::test]
async fn fetches_and_verifies_metadata() {
    // ~82 KiB of info dictionary, i.e. five 16 KiB metadata pieces: enough to
    // exercise the pipeline, not enough to slow the test down.
    let info = info_dict("happy", 64 * 1024 * 1024, 16 * 1024);
    assert!(info.len() > 4 * METADATA_PIECE_SIZE as usize);
    let info_hash = sha1(&info);
    let (addr, provider) = spawn_provider(info_hash, info.clone(), Reply::Data, false).await;

    let mut spec = spec_for(info_hash, vec![addr]);
    spec.trackers = vec!["http://127.0.0.1:9/announce".to_owned()];
    let (tx, mut events) = mpsc::channel(64);

    let metadata = timeout(Duration::from_secs(10), fetch(spec, Some(tx)))
        .await
        .expect("fetch must finish")
        .expect("fetch must succeed");

    assert_eq!(metadata.info, info);
    assert_eq!(sha1(&metadata.info), info_hash);

    let parsed = flux_core::metainfo::parse(&metadata.torrent).expect("torrent must parse");
    assert_eq!(parsed.info_hash(), Some(info_hash));
    assert_eq!(
        parsed.announce.as_deref(),
        Some("http://127.0.0.1:9/announce")
    );
    assert_eq!(parsed.info.name, "happy");

    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert_eq!(seen.first(), Some(&FetchEvent::Peers(1)));
    assert_eq!(seen.last(), Some(&FetchEvent::Done { bytes: info.len() }));
    let pieces = info.len().div_ceil(METADATA_PIECE_SIZE as usize) as u32;
    assert!(
        pieces > 1,
        "the test must exercise multiple metadata pieces"
    );
    assert!(seen.iter().any(|event| matches!(
        event,
        FetchEvent::Progress { piece, total } if *piece + 1 == pieces && *total == pieces
    )));

    join_provider(provider).await;
}

#[tokio::test]
async fn hash_mismatch_is_refused() {
    // The magnet claims `expected`, the provider serves other valid bytes.
    let expected_info = info_dict("claimed", 1024, 256);
    let served_info = info_dict("something-else", 1024, 256);
    let expected = sha1(&expected_info);
    let served_hash = sha1(&served_info);
    assert_ne!(expected, served_hash);

    let (addr, provider) = spawn_provider(expected, served_info.clone(), Reply::Data, false).await;
    let spec = spec_for(expected, vec![addr]);

    let error = timeout(Duration::from_secs(10), fetch(spec, None))
        .await
        .expect("fetch must finish")
        .expect_err("mismatched metadata must not be returned");

    match error {
        MagnetError::HashMismatch {
            expected: got_expected,
            got,
        } => {
            assert_eq!(got_expected, expected);
            assert_eq!(got, served_hash);
        }
        other => panic!("expected HashMismatch, got {other:?}"),
    }

    join_provider(provider).await;
}

#[tokio::test]
async fn rejection_is_reported() {
    let info = info_dict("rejected", 4096, 1024);
    let info_hash = sha1(&info);
    let (addr, provider) = spawn_provider(info_hash, info, Reply::Reject, false).await;
    let spec = spec_for(info_hash, vec![addr]);

    let error = timeout(Duration::from_secs(10), fetch(spec, None))
        .await
        .expect("fetch must finish")
        .expect_err("a rejection is not metadata");

    assert_eq!(error, MagnetError::Rejected);
    join_provider(provider).await;
}

#[tokio::test]
async fn oversized_metadata_is_refused_without_allocating_it() {
    let info = info_dict("oversized", 4096, 1024);
    let info_hash = sha1(&info);
    let (addr, provider) = spawn_provider(info_hash, info, Reply::Oversized, false).await;
    let spec = spec_for(info_hash, vec![addr]);

    let error = timeout(Duration::from_secs(10), fetch(spec, None))
        .await
        .expect("fetch must finish")
        .expect_err("5 MiB is over the cap");

    assert_eq!(error, MagnetError::MetadataTooLarge);
    join_provider(provider).await;
}

#[tokio::test]
async fn silent_peer_times_out_without_eating_the_budget() {
    let info = info_dict("silent", 4096, 1024);
    let info_hash = sha1(&info);
    let (addr, provider) = spawn_provider(info_hash, info, Reply::Silent, false).await;

    let mut spec = spec_for(info_hash, vec![addr]);
    spec.timeout = Duration::from_secs(1);

    let started = Instant::now();
    let error = fetch(spec, None).await.expect_err("nobody answers");
    let elapsed = started.elapsed();

    assert_eq!(error, MagnetError::Timeout);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");

    // The provider sees its socket close and stops on its own.
    join_provider(provider).await;
}

#[tokio::test]
async fn a_silent_peer_does_not_consume_the_next_peers_budget() {
    let info = info_dict("fallback", 64 * 1024 * 1024, 16 * 1024);
    let info_hash = sha1(&info);

    let (silent, silent_task) = spawn_provider(info_hash, info.clone(), Reply::Silent, false).await;
    let (good, good_task) = spawn_provider(info_hash, info.clone(), Reply::Data, false).await;

    let mut spec = spec_for(info_hash, vec![silent, good]);
    spec.timeout = Duration::from_secs(4);

    let metadata = timeout(Duration::from_secs(10), fetch(spec, None))
        .await
        .expect("fetch must finish")
        .expect("the second peer must serve the metadata");

    assert_eq!(metadata.info, info);
    join_provider(silent_task).await;
    join_provider(good_task).await;
}

#[tokio::test]
async fn a_bitfield_after_the_handshake_does_not_break_the_fetch() {
    let info = info_dict("bitfield", 4096, 1024);
    let info_hash = sha1(&info);
    let (addr, provider) = spawn_provider(info_hash, info.clone(), Reply::Data, true).await;
    let spec = spec_for(info_hash, vec![addr]);

    let metadata = timeout(Duration::from_secs(10), fetch(spec, None))
        .await
        .expect("fetch must finish")
        .expect("a bitfield is not an error");

    assert_eq!(metadata.info, info);
    join_provider(provider).await;
}

#[tokio::test]
async fn no_seeds_and_no_trackers_is_no_peers() {
    let info_hash = sha1(b"nowhere");
    let spec = spec_for(info_hash, Vec::new());

    let error = fetch(spec, None)
        .await
        .expect_err("there is no swarm to ask");
    assert_eq!(error, MagnetError::NoPeers);
}

#[tokio::test]
async fn assemble_torrent_round_trips_through_metainfo() {
    let info = info_dict("assembled", 1024, 256);
    let hash = sha1(&info);

    for trackers in [
        Vec::new(),
        vec!["http://tracker.example/announce".to_owned()],
        vec![
            "http://tracker.example/announce".to_owned(),
            "udp://tracker.example:6969/announce".to_owned(),
        ],
    ] {
        let torrent = assemble_torrent(&info, &trackers);
        let parsed = flux_core::metainfo::parse(&torrent).expect("torrent must parse");
        assert_eq!(parsed.info_hash(), Some(hash));
    }
}
