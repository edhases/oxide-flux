//! End-to-end tests for the control-API adapter: daemon session in, DTOs out.
//!
//! Response shapes are covered by `flux-api`; these tests pin the mapping that
//! only the daemon can get wrong: honest states, id translation, and the
//! operations this build refuses instead of approximating.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use flux_api::{AddTorrent, Engine, Event, Priority, TorrentState as ApiState};
use flux_core::bencode::{self, Value};
use flux_daemon::engine::DaemonEngine;
use flux_daemon::{Config, SessionHandle, spawn_event_bridge};
use sha1::{Digest, Sha1};
use tokio::sync::broadcast;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(30);

fn dict(entries: Vec<(&str, Value)>) -> Value {
    Value::Dict(
        entries
            .into_iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value))
            .collect(),
    )
}

fn text(value: &str) -> Value {
    Value::Bytes(value.as_bytes().to_vec())
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(data);
    let mut out = [0_u8; 20];
    out.copy_from_slice(hasher.finalize().as_slice());
    out
}

/// The daemon builds its own runtime; the control API cannot serve a request
/// without a reactor. This is a regression test for a runtime built without
/// `enable_io`.
#[test]
fn cli_runtime_has_io_enabled() {
    let runtime = flux_daemon::cli::build_runtime(Some(1)).unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    });
}

/// A single-file v1 torrent whose payload is `length` bytes of `Z`.
fn single_file(name: &str, length: u64, piece_length: u32, private: bool) -> Vec<u8> {
    let payload: Vec<u8> = std::iter::repeat_n(b'Z', usize::try_from(length).unwrap()).collect();
    let pieces: Vec<u8> = payload
        .chunks(usize::try_from(piece_length).unwrap())
        .flat_map(sha1)
        .collect();
    let info = dict(vec![
        ("length", Value::Int(i64::try_from(length).unwrap())),
        ("name", text(name)),
        ("piece length", Value::Int(i64::from(piece_length))),
        ("pieces", Value::Bytes(pieces)),
        ("private", Value::Int(if private { 1 } else { 0 })),
    ]);
    bencode::encode(&dict(vec![("info", info)]))
}

fn make_engine(session: &SessionHandle, dir: &std::path::Path) -> DaemonEngine {
    DaemonEngine::new(session.clone(), dir)
}

#[tokio::test]
async fn adds_metainfo_and_reports_honest_state() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        enable_dht: false,
        ..Config::default()
    };
    let session = SessionHandle::start(config).await.unwrap();
    let engine = make_engine(&session, dir.path());

    let bytes = single_file("media", 1024, 1024, true);
    let summary = engine.add(AddTorrent::Metainfo { bytes }).await.unwrap();

    assert_eq!(summary.name, "media");
    assert!(summary.private);
    assert_eq!(summary.pieces_total, 1);
    assert_eq!(summary.pieces_have, 0);
    assert_eq!(summary.progress, 0.0);
    // Preallocation is done, hashing may still be starting: both are honest.
    assert!(matches!(
        summary.state,
        ApiState::Queued | ApiState::Checking
    ));
    assert_eq!(summary.total_bytes, 1024);

    let detail = engine.get(&summary.id).await.unwrap();
    assert_eq!(detail.summary.id, summary.id);
    assert_eq!(detail.piece_length, 1024);
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].path, "media");
    assert_eq!(detail.files[0].length, 1024);
    assert_eq!(detail.files[0].priority, Priority::Normal);

    let listed = engine.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, summary.id);

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn magnet_without_peers_is_reported_not_faked() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        enable_dht: false,
        ..Config::default()
    };
    let session = SessionHandle::start(config).await.unwrap();
    let engine = make_engine(&session, dir.path());

    // No trackers and no DHT: there is no peer source, and the engine says so.
    let error = engine
        .add(AddTorrent::Magnet {
            uri: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(matches!(error, flux_api::EngineError::InvalidSource(_)));
    assert!(engine.list().await.unwrap().is_empty());

    // A v2-only magnet needs Merkle metadata verification, which is absent.
    let error = engine
        .add(AddTorrent::Magnet {
            uri: format!("magnet:?xt=urn:btmh:1220{}", "ab".repeat(32)),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        flux_api::EngineError::Unsupported(message) if message.contains("Merkle")
    ));

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn low_priority_is_refused_and_skip_applies() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        enable_dht: false,
        ..Config::default()
    };
    let session = SessionHandle::start(config).await.unwrap();
    let engine = make_engine(&session, dir.path());

    let summary = engine
        .add(AddTorrent::Metainfo {
            bytes: single_file("media", 1024, 1024, false),
        })
        .await
        .unwrap();

    let error = engine
        .set_file_priorities(&summary.id, vec![(0, Priority::Low)])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        flux_api::EngineError::Unsupported(message) if message.contains("low")
    ));

    engine
        .set_file_priorities(&summary.id, vec![(0, Priority::Skip)])
        .await
        .unwrap();
    let detail = engine.get(&summary.id).await.unwrap();
    assert_eq!(detail.files[0].priority, Priority::Skip);

    let out_of_range = engine
        .set_file_priorities(&summary.id, vec![(9, Priority::Normal)])
        .await
        .unwrap_err();
    assert!(matches!(
        out_of_range,
        flux_api::EngineError::InvalidFileIndex(9)
    ));

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_ids_map_to_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        enable_dht: false,
        ..Config::default()
    };
    let session = SessionHandle::start(config).await.unwrap();
    let engine = make_engine(&session, dir.path());

    let missing = "0123456789abcdef0123456789abcdef01234567";
    assert!(matches!(
        engine.get(missing).await.unwrap_err(),
        flux_api::EngineError::NotFound(id) if id == missing
    ));
    assert!(matches!(
        engine.pause(missing).await.unwrap_err(),
        flux_api::EngineError::NotFound(_)
    ));

    // A malformed id is a client error, not a missing torrent.
    assert!(matches!(
        engine.get("not-a-hash").await.unwrap_err(),
        flux_api::EngineError::InvalidSource(_)
    ));

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn stats_are_counters_not_rates() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        enable_dht: false,
        ..Config::default()
    };
    let session = SessionHandle::start(config).await.unwrap();
    let engine = make_engine(&session, dir.path());

    engine
        .add(AddTorrent::Metainfo {
            bytes: single_file("media", 1024, 1024, false),
        })
        .await
        .unwrap();

    let stats = engine.stats().await.unwrap();
    assert_eq!(stats.torrents, 1);
    assert_eq!(stats.downloading, 0);
    assert_eq!(stats.seeding, 0);
    assert_eq!(stats.bytes_downloaded, 0);
    assert_eq!(stats.bytes_uploaded, 0);

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn events_reach_the_notification_stream() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        enable_dht: false,
        ..Config::default()
    };
    let session = SessionHandle::start(config).await.unwrap();
    let engine = make_engine(&session, dir.path());

    let (sink, mut notifications) = broadcast::channel(16);
    let bridge = spawn_event_bridge(session.clone(), sink);

    let summary = engine
        .add(AddTorrent::Metainfo {
            bytes: single_file("media", 1024, 1024, false),
        })
        .await
        .unwrap();

    let event = timeout(WAIT, notifications.recv()).await.unwrap().unwrap();
    match event {
        Event::TorrentUpdated(updated) => assert_eq!(updated.id, summary.id),
        other => panic!("expected torrent.updated, got {other:?}"),
    }

    session.shutdown().await.unwrap();
    bridge.await.unwrap();
}

#[tokio::test]
async fn detail_reports_verified_file_progress_and_downloading_state() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        enable_dht: false,
        persist: false,
        ..Config::default()
    };
    let session = SessionHandle::start(config).await.unwrap();
    let engine = make_engine(&session, dir.path());

    // The payload the synthetic torrent describes is 1024 bytes of `Z`.
    std::fs::write(dir.path().join("media"), vec![b'Z'; 1024]).unwrap();

    let summary = engine
        .add(AddTorrent::Metainfo {
            bytes: single_file("media", 1024, 1024, false),
        })
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let state = session.get(&summary.id.parse().unwrap()).await.unwrap();
        if state.complete {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "verification timed out"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let detail = engine.get(&summary.id).await.unwrap();
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].bytes_completed, 1024);
    assert!(detail.files[0].completed);
    assert_eq!(detail.files[0].offset, 0);
    assert!(matches!(
        detail.summary.state,
        flux_api::TorrentState::Seeding | flux_api::TorrentState::Complete
    ));

    session.shutdown().await.unwrap();
}
