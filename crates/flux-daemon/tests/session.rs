//! Integration tests for the daemon: add, select, preallocate, verify, queue.
//!
//! Every torrent is built in-test with `flux_core::bencode::encode` and real SHA-1
//! piece hashes, so verification has something true to compare against. Nothing
//! here sleeps: ordering is driven through commands and events.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use flux_core::bencode::{self, Value};
use flux_core::metainfo;
use flux_daemon::cli::{self, VerifySummary};
use flux_daemon::pieces::Priority;
use flux_daemon::verify::{self, AddError};
use flux_daemon::{Config, Event, SessionError, SessionHandle, Status, TorrentId, Verification};
use flux_disk::{Allocation, Block, BlockBackend, DiskWriter, StdBackend, WriterConfig};
use sha1::{Digest, Sha1};
use tokio::sync::broadcast;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Synthetic torrents
// ---------------------------------------------------------------------------

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
    let digest = hasher.finalize();
    let mut out = [0_u8; 20];
    out.copy_from_slice(digest.as_slice());
    out
}

/// A torrent built for a test, plus the payload it describes.
struct Synthetic {
    bytes: Vec<u8>,
    id: TorrentId,
    payload: Vec<u8>,
    piece_length: u32,
}

impl Synthetic {
    /// Multi-file v1 torrent. `files` are the path components below the name.
    fn multi(name: &str, files: &[(Vec<&str>, u64)], piece_length: u32, private: bool) -> Self {
        let mut payload = Vec::new();
        for (index, (_, length)) in files.iter().enumerate() {
            let byte = b'A' + u8::try_from(index).unwrap_or(0) % 26;
            payload.extend(std::iter::repeat_n(byte, usize::try_from(*length).unwrap()));
        }
        let pieces: Vec<u8> = payload
            .chunks(usize::try_from(piece_length).unwrap())
            .flat_map(sha1)
            .collect();
        let entries: Vec<Value> = files
            .iter()
            .map(|(path, length)| {
                dict(vec![
                    ("length", Value::Int(i64::try_from(*length).unwrap())),
                    (
                        "path",
                        Value::List(path.iter().map(|part| text(part)).collect()),
                    ),
                ])
            })
            .collect();
        let info = dict(vec![
            ("files", Value::List(entries)),
            ("name", text(name)),
            ("piece length", Value::Int(i64::from(piece_length))),
            ("pieces", Value::Bytes(pieces)),
            ("private", Value::Int(if private { 1 } else { 0 })),
        ]);
        Self::finish(info, payload, piece_length)
    }

    /// Single-file v1 torrent.
    fn single(name: &str, length: u64, piece_length: u32) -> Self {
        let payload: Vec<u8> =
            std::iter::repeat_n(b'Z', usize::try_from(length).unwrap()).collect();
        let pieces: Vec<u8> = payload
            .chunks(usize::try_from(piece_length).unwrap())
            .flat_map(sha1)
            .collect();
        let info = dict(vec![
            ("length", Value::Int(i64::try_from(length).unwrap())),
            ("name", text(name)),
            ("piece length", Value::Int(i64::from(piece_length))),
            ("pieces", Value::Bytes(pieces)),
        ]);
        Self::finish(info, payload, piece_length)
    }

    /// v2-only torrent, for the gap-tolerant layout bridge.
    fn v2(name: &str, files: &[(&str, u64)], piece_length: u32) -> Self {
        let tree: Vec<(Vec<u8>, Value)> = files
            .iter()
            .map(|(path, length)| {
                let properties = dict(vec![
                    ("length", Value::Int(i64::try_from(*length).unwrap())),
                    ("pieces root", Value::Bytes(vec![0x5a; 32])),
                ]);
                (path.as_bytes().to_vec(), dict(vec![("", properties)]))
            })
            .collect();
        let info = dict(vec![
            ("file tree", Value::Dict(tree)),
            ("meta version", Value::Int(2)),
            ("name", text(name)),
            ("piece length", Value::Int(i64::from(piece_length))),
        ]);
        Self::finish(info, Vec::new(), piece_length)
    }

    /// Wraps the info dictionary in a torrent and cross-checks the hash the id
    /// claims against an independently computed SHA-1 of the encoded info.
    fn finish(info: Value, payload: Vec<u8>, piece_length: u32) -> Self {
        let raw_info = bencode::encode(&info);
        let expected_hash = sha1(&raw_info);
        let expected_id: String = expected_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let bytes = bencode::encode(&dict(vec![("info", info)]));
        let metainfo = metainfo::parse(&bytes).expect("synthetic torrent must parse");
        let id = TorrentId::from_info(&metainfo.info).expect("synthetic torrent must have a hash");
        if metainfo.info.hashes.v1.is_some() {
            assert_eq!(
                metainfo.info.hashes.v1,
                Some(expected_hash),
                "the v1 info hash must be the SHA-1 of the encoded info dictionary"
            );
            assert_eq!(
                id.as_str(),
                expected_id,
                "the id must be the hex of that SHA-1"
            );
        }
        Self {
            bytes,
            id,
            payload,
            piece_length,
        }
    }

    fn piece_count(&self) -> usize {
        metainfo::parse(&self.bytes)
            .unwrap()
            .info
            .layout
            .piece_count()
    }
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

async fn start_session(root: &Path, tweak: impl FnOnce(&mut Config)) -> SessionHandle {
    let mut config = Config {
        download_dir: root.to_path_buf(),
        verifier_threads: 2,
        // Tests must never touch the network: no bootstrap resolution, no UDP.
        enable_dht: false,
        ..Config::default()
    };
    tweak(&mut config);
    SessionHandle::start(config).await.unwrap()
}

async fn next_event<F>(events: &mut broadcast::Receiver<Event>, mut predicate: F) -> Event
where
    F: FnMut(&Event) -> bool,
{
    let waiting = async {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if predicate(&event) {
                        return event;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("session event stream closed")
                }
            }
        }
    };
    timeout(WAIT, waiting)
        .await
        .expect("timed out waiting for a session event")
}

/// Waits for the `Finished` event of one torrent and returns `(have, missing)`.
async fn wait_finished(events: &mut broadcast::Receiver<Event>, id: &TorrentId) -> (usize, usize) {
    let event = next_event(
        events,
        |event| matches!(event, Event::Finished { id: other, .. } if other == id),
    )
    .await;
    match event {
        Event::Finished { have, missing, .. } => (have, missing),
        other => panic!("expected Finished, got {other:?}"),
    }
}

fn bytes_at(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

// ---------------------------------------------------------------------------
// Add
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adds_a_single_file_torrent_and_preallocates_it() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::single("file.bin", 8, 4);
    let session = start_session(dir.path(), |_| {}).await;
    let mut events = session.subscribe();

    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();

    assert_eq!(state.name, "file.bin");
    assert_eq!(state.id, torrent.id);
    assert!(!state.private);
    assert_eq!(state.payload_length, 8);
    assert_eq!(state.total_length, 8);
    assert_eq!(state.piece_length, 4);
    assert_eq!(state.piece_count, 2);
    assert_eq!(state.files.len(), 1);
    assert_eq!(state.files[0].path, "file.bin");
    assert_eq!(state.files[0].length, 8);
    // The add returns after preallocation and before the pass can finish.
    assert!(
        matches!(state.status, Status::Preallocated | Status::Checking),
        "unexpected status {:?}",
        state.status
    );
    assert!(matches!(
        state.verification,
        Verification::Pending | Verification::Running
    ));

    // Sparse preallocation gives the exact declared size, filled with zeroes.
    let path = dir.path().join("file.bin");
    assert_eq!(fs::metadata(&path).unwrap().len(), 8);
    assert_eq!(bytes_at(&path), vec![0_u8; 8]);

    // Zeroed files match no real piece hash, so everything is missing.
    let (have, missing) = wait_finished(&mut events, &state.id).await;
    assert_eq!((have, missing), (0, 2));

    let state = session.get(&state.id).await.unwrap();
    assert_eq!(state.verification, Verification::Complete);
    assert_eq!(state.pieces_have, 0);
    assert_eq!(state.pieces_desired, 2);
    assert_eq!(state.bytes_have, 0);
    assert_eq!(state.bytes_desired, 8);
    assert!(!state.complete);
    assert!(matches!(state.status, Status::Active | Status::Queued));
    assert!(!state.files[0].completed);

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn adds_a_multi_file_torrent_with_exact_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::multi(
        "root",
        &[(vec!["a.bin"], 3), (vec!["sub", "b.bin"], 9)],
        4,
        false,
    );
    let session = start_session(dir.path(), |_| {}).await;
    let mut events = session.subscribe();

    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    assert_eq!(state.files.len(), 2);
    assert_eq!(state.files[0].path, "root/a.bin");
    assert_eq!(state.files[1].path, "root/sub/b.bin");
    assert_eq!(state.total_length, 12);
    assert_eq!(state.piece_count, 3);

    assert_eq!(
        fs::metadata(dir.path().join("root/a.bin")).unwrap().len(),
        3
    );
    assert_eq!(
        fs::metadata(dir.path().join("root/sub/b.bin"))
            .unwrap()
            .len(),
        9
    );

    let (have, missing) = wait_finished(&mut events, &state.id).await;
    assert_eq!((have, missing), (0, 3));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn duplicate_adds_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::single("file.bin", 4, 4);
    let session = start_session(dir.path(), |_| {}).await;
    let mut events = session.subscribe();

    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_finished(&mut events, &state.id).await;

    let error = session
        .add_torrent(torrent.bytes.clone())
        .await
        .unwrap_err();
    assert!(matches!(error, SessionError::Duplicate { .. }), "{error:?}");
    session.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Selective download
// ---------------------------------------------------------------------------

#[tokio::test]
async fn straddling_pieces_follow_file_selection() {
    // a = [0,3), b = [3,8), c = [8,12); piece length 4.
    // piece 0 straddles a and b, piece 1 is inside b, piece 2 is inside c.
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::multi(
        "root",
        &[(vec!["a.bin"], 3), (vec!["b.bin"], 5), (vec!["c.bin"], 4)],
        4,
        false,
    );
    let session = start_session(dir.path(), |_| {}).await;
    let mut events = session.subscribe();
    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_finished(&mut events, &state.id).await;

    assert_eq!(state.pieces_desired, 3);

    // Skip the middle file: piece 0 stays because a.bin is still wanted.
    let state = session
        .set_file_priorities(
            &torrent.id,
            [
                (0, Priority::Normal),
                (1, Priority::Skip),
                (2, Priority::Normal),
            ],
        )
        .await
        .unwrap();
    assert_eq!(state.pieces_desired, 2);
    assert_eq!(state.bytes_desired, 8); // piece 0 (4 bytes) + piece 2 (4 bytes)
    let pieces = session.piece_state(&torrent.id).await.unwrap();
    assert_eq!(pieces.desired.ones().collect::<Vec<_>>(), vec![0, 2]);
    assert!(!state.files[1].selected);
    assert_eq!(state.files[1].priority, Priority::Skip);

    // Skip everything: the straddling piece is no longer wanted either.
    let state = session
        .set_file_priorities(
            &torrent.id,
            [
                (0, Priority::Skip),
                (1, Priority::Skip),
                (2, Priority::Skip),
            ],
        )
        .await
        .unwrap();
    assert_eq!(state.pieces_desired, 0);
    assert_eq!(state.bytes_desired, 0);
    assert!(
        session
            .piece_state(&torrent.id)
            .await
            .unwrap()
            .desired
            .is_empty_set()
    );

    // Skip only the first file: piece 0 survives because b.bin owns part of it.
    let state = session
        .set_file_priorities(
            &torrent.id,
            [
                (0, Priority::Skip),
                (1, Priority::Normal),
                (2, Priority::Normal),
            ],
        )
        .await
        .unwrap();
    assert_eq!(state.pieces_desired, 3);
    let pieces = session.piece_state(&torrent.id).await.unwrap();
    assert_eq!(pieces.desired.ones().collect::<Vec<_>>(), vec![0, 1, 2]);
    assert!(!state.files[0].selected);

    // A v1 single-file torrent has one file, so one priority.
    let bad = session
        .set_file_priorities(&torrent.id, [(9, Priority::High)])
        .await
        .unwrap_err();
    assert!(
        matches!(
            bad,
            SessionError::FileIndexOutOfRange { index: 9, files: 3 }
        ),
        "{bad:?}"
    );

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn zero_length_files_do_not_affect_selection() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::multi(
        "root",
        &[
            (vec!["a.bin"], 4),
            (vec!["empty.bin"], 0),
            (vec!["b.bin"], 4),
        ],
        4,
        false,
    );
    let session = start_session(dir.path(), |_| {}).await;
    let mut events = session.subscribe();
    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_finished(&mut events, &state.id).await;

    assert_eq!(state.piece_count, 2);
    assert_eq!(state.files[1].length, 0);
    assert_eq!(state.files[1].path, "root/empty.bin");

    let state = session
        .set_file_priorities(
            &torrent.id,
            [
                (0, Priority::Skip),
                (1, Priority::High),
                (2, Priority::Skip),
            ],
        )
        .await
        .unwrap();
    // The empty file owns no bytes, so selecting it selects nothing.
    assert_eq!(state.pieces_desired, 0);
    assert!(state.files[1].selected);

    let state = session
        .set_file_priorities(
            &torrent.id,
            [
                (0, Priority::Skip),
                (1, Priority::Skip),
                (2, Priority::Normal),
            ],
        )
        .await
        .unwrap();
    assert_eq!(state.pieces_desired, 1);
    session.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Verification against real bytes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn written_pieces_are_the_only_ones_verified() {
    let dir = tempfile::tempdir().unwrap();
    // Payload "AABBCCDD..." split over a 3-byte and a 5-byte file, 4-byte pieces.
    let torrent = Synthetic::multi("root", &[(vec!["a.bin"], 3), (vec!["b.bin"], 5)], 4, false);
    assert_eq!(torrent.piece_count(), 2);
    let session = start_session(dir.path(), |_| {}).await;
    let mut events = session.subscribe();
    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    let (have, missing) = wait_finished(&mut events, &state.id).await;
    assert_eq!((have, missing), (0, 2));

    // Write exactly piece 0 ("AAAB", which straddles both files) through the
    // real writer, then ask the session to re-hash the files.
    let plan = verify::plan(&torrent.bytes).unwrap();
    write_pieces(
        dir.path(),
        plan.disk_layout(),
        &[(0, torrent.payload[0..4].to_vec())],
        torrent.piece_length,
    )
    .await;

    let state = session.recheck(&torrent.id).await.unwrap();
    // The pass starts immediately, so the torrent is already checking.
    assert!(matches!(
        state.verification,
        Verification::Pending | Verification::Running
    ));
    let (have, missing) = wait_finished(&mut events, &state.id).await;
    assert_eq!((have, missing), (1, 1));

    let state = session.get(&torrent.id).await.unwrap();
    assert_eq!(state.pieces_have, 1);
    assert_eq!(state.pieces_corrupt, 1);
    assert_eq!(state.bytes_have, 4);
    assert!(!state.complete);
    let pieces = session.piece_state(&torrent.id).await.unwrap();
    assert_eq!(pieces.have.get(0), Some(true));
    assert_eq!(pieces.have.get(1), Some(false));
    // Piece 0 covers a.bin completely and one byte of b.bin.
    assert!(state.files[0].completed);
    assert_eq!(state.files[0].bytes_completed, 3);
    assert!(!state.files[1].completed);
    assert_eq!(state.files[1].bytes_completed, 1);

    // Write the second piece: now the torrent is genuinely complete.
    write_pieces(
        dir.path(),
        plan.disk_layout(),
        &[(1, torrent.payload[4..8].to_vec())],
        torrent.piece_length,
    )
    .await;
    session.recheck(&torrent.id).await.unwrap();
    let (have, missing) = wait_finished(&mut events, &state.id).await;
    assert_eq!((have, missing), (2, 0));

    let state = session.get(&torrent.id).await.unwrap();
    assert_eq!(state.verification, Verification::Complete);
    assert!(state.complete);
    assert_eq!(state.bytes_have, 8);
    assert!(state.files.iter().all(|file| file.completed));
    // Complete torrents claim a seed slot; the slot is reserved, not serving.
    assert_eq!(state.status, Status::Seeding);
    assert!(state.active);

    session.shutdown().await.unwrap();
}

async fn write_pieces(
    root: &Path,
    layout: Arc<flux_disk::FileLayout>,
    pieces: &[(u32, Vec<u8>)],
    piece_length: u32,
) {
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());
    let config = WriterConfig {
        allocation: Allocation::None,
        flush_interval: None,
        ..WriterConfig::new(piece_length)
    };
    let (writer, _events) = DiskWriter::start(config, layout, backend, root.to_path_buf()).unwrap();
    for (piece, data) in pieces {
        writer
            .write_block(Block::new(*piece, 0, data.clone()))
            .await
            .unwrap();
    }
    writer.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Hostile paths and private torrents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hostile_paths_are_rejected_and_nothing_escapes() {
    let outside = tempfile::tempdir().unwrap();
    let root = outside.path().join("media");
    let session = start_session(&root, |_| {}).await;

    let cases: Vec<Synthetic> = vec![
        Synthetic::multi("root", &[(vec!["..", "evil"], 4)], 4, false),
        Synthetic::multi("root", &[(vec!["/etc/passwd"], 4)], 4, false),
        Synthetic::multi("root", &[(vec!["CON"], 4)], 4, false),
        Synthetic::multi("root", &[(vec!["sub", "..", "evil"], 4)], 4, false),
    ];

    for torrent in cases {
        let error = session
            .add_torrent(torrent.bytes.clone())
            .await
            .expect_err("hostile path must be rejected");
        assert!(
            matches!(
                error,
                SessionError::Add(AddError::UnsafePath { .. })
                    | SessionError::Add(AddError::UnsafeName { .. })
            ),
            "unexpected error {error:?}"
        );
    }

    // Nothing was created, in the root or beside it.
    let mut entries: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    assert!(entries.is_empty(), "root is not empty: {entries:?}");
    let mut siblings: Vec<_> = fs::read_dir(outside.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    siblings.sort();
    assert_eq!(siblings, vec![std::ffi::OsString::from("media")]);
    assert!(!outside.path().join("evil").exists());

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn private_torrents_are_surfaced() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::multi("root", &[(vec!["a.bin"], 8)], 4, true);
    assert!(verify::plan(&torrent.bytes).unwrap().private());

    let session = start_session(dir.path(), |_| {}).await;
    let mut events = session.subscribe();
    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    assert!(
        state.private,
        "private flag must be visible to the peer layer"
    );
    wait_finished(&mut events, &state.id).await;

    let state = session.get(&torrent.id).await.unwrap();
    assert!(state.private);
    session.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Queue limits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn download_slots_promote_in_add_order() {
    let dir = tempfile::tempdir().unwrap();
    let session = start_session(dir.path(), |config| {
        config.max_active_downloads = 1;
    })
    .await;
    let mut events = session.subscribe();

    let first = Synthetic::single("first.bin", 4, 4);
    let second = Synthetic::single("second.bin", 4, 4);

    let first_state = session.add_torrent(first.bytes.clone()).await.unwrap();
    wait_finished(&mut events, &first_state.id).await;
    let first_state = session.get(&first.id).await.unwrap();
    assert_eq!(first_state.status, Status::Active);
    assert_eq!(first_state.queue_position, None);

    let second_state = session.add_torrent(second.bytes.clone()).await.unwrap();
    wait_finished(&mut events, &second_state.id).await;
    let queued = session.get(&second.id).await.unwrap();
    assert_eq!(queued.status, Status::Queued);
    assert_eq!(queued.queue_position, Some(1));
    assert!(!queued.active);

    // Pausing the active torrent releases its slot and promotes the waiters.
    session.pause(&first.id).await.unwrap();
    let promoted = session.get(&second.id).await.unwrap();
    assert_eq!(promoted.status, Status::Active);
    assert_eq!(promoted.queue_position, None);
    assert_eq!(session.get(&first.id).await.unwrap().status, Status::Paused);

    // Removing the active torrent frees the slot again...
    session.remove(&second.id, false).await.unwrap();
    assert!(matches!(
        session.get(&second.id).await.unwrap_err(),
        SessionError::UnknownTorrent { .. }
    ));

    // ...and resuming the paused one takes it, in add order.
    let resumed = session.resume(&first.id).await.unwrap();
    assert_eq!(resumed.status, Status::Active);

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn seed_slots_are_limited_and_queued_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let session = start_session(dir.path(), |config| {
        config.max_active_seeds = 1;
    })
    .await;
    let mut events = session.subscribe();

    let first = Synthetic::single("first.bin", 4, 4);
    let second = Synthetic::single("second.bin", 4, 4);

    for torrent in [&first, &second] {
        let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
        wait_finished(&mut events, &state.id).await;
        // Make it complete by storing exactly the bytes it claims.
        fs::write(dir.path().join(&state.name), &torrent.payload).unwrap();
        session.recheck(&torrent.id).await.unwrap();
        let (have, missing) = wait_finished(&mut events, &torrent.id).await;
        assert_eq!((have, missing), (1, 0));
    }

    let first_state = session.get(&first.id).await.unwrap();
    let second_state = session.get(&second.id).await.unwrap();
    assert_eq!(first_state.status, Status::Seeding);
    assert!(first_state.complete);
    assert_eq!(second_state.status, Status::Ready);
    assert!(second_state.complete);
    assert_eq!(second_state.queue_position, Some(1));
    assert!(!second_state.active);

    // Completing and removing the holder promotes the waiter.
    session.remove(&first.id, true).await.unwrap();
    assert!(!dir.path().join("first.bin").exists());
    let promoted = session.get(&second.id).await.unwrap();
    assert_eq!(promoted.status, Status::Seeding);

    session.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// v2 bridge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v2_only_torrents_map_gaps_onto_the_disk_layout() {
    let dir = tempfile::tempdir().unwrap();
    // a.bin holds pieces 0..2, b.bin starts at piece 2: the byte range 20000..32768
    // is an alignment gap that no file owns.
    let torrent = Synthetic::v2("root", &[("a.bin", 20000), ("b.bin", 1000)], 16384);

    let plan = verify::plan(&torrent.bytes).unwrap();
    assert_eq!(plan.total_length(), 33768);
    assert_eq!(plan.payload_length(), 21000);
    assert_eq!(plan.piece_count(), 3);
    assert_eq!(plan.files()[1].offset, 32768);
    assert_eq!(plan.disk_layout().total_length(), 33768);
    assert_eq!(plan.disk_layout().piece_count(16384).unwrap(), 3);
    assert!(!plan.verification_supported());

    let session = start_session(dir.path(), |_| {}).await;
    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    // Incomplete, so it takes a download slot; nothing can arrive yet.
    assert_eq!(state.status, Status::Active);
    assert_eq!(state.verification, Verification::Unsupported);
    assert!(!state.complete);
    assert_eq!(
        fs::metadata(dir.path().join("root/a.bin")).unwrap().len(),
        20000
    );
    assert_eq!(
        fs::metadata(dir.path().join("root/b.bin")).unwrap().len(),
        1000
    );
    assert_eq!(state.files[1].offset, 32768);
    assert_eq!(state.bytes_desired, 21000);

    session.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CLI verification, through the library
// ---------------------------------------------------------------------------

async fn write_torrent_and_payload(
    root: &Path,
    torrent: &Synthetic,
    payload: &[u8],
) -> std::path::PathBuf {
    let file = root.join("test.torrent");
    fs::write(&file, &torrent.bytes).unwrap();
    let media = root.join("media");
    fs::create_dir_all(&media).unwrap();
    // Single-file torrents land directly under the root under their name.
    let metainfo = metainfo::parse(&torrent.bytes).unwrap();
    let file_path = media.join(&metainfo.info.name);
    fs::write(file_path, payload).unwrap();
    file
}

#[tokio::test]
async fn verify_command_reports_a_clean_torrent() {
    let dir = tempfile::tempdir().unwrap();
    let single = Synthetic::single("film.bin", 9, 4);
    let torrent_file = write_torrent_and_payload(dir.path(), &single, &single.payload).await;
    let media = dir.path().join("media");

    let summary = cli::verify_torrent_file(&torrent_file, &media)
        .await
        .unwrap();
    assert_eq!(summary.exit_code(), 0);
    assert!(summary.clean);
    assert_eq!(summary.name, "film.bin");
    assert_eq!(summary.piece_count, 3);
    assert_eq!(summary.have, 3);
    assert_eq!(summary.missing, 0);
    assert_eq!(summary.corrupt, 0);
    assert_eq!(summary.unreadable, 0);
    assert_eq!(summary.payload_length, 9);
    assert!(summary.files[0].completed);
    assert_eq!(summary.files[0].bytes_completed, 9);
    assert_eq!(bytes_at(&media.join("film.bin")), single.payload);

    // The JSON form is well-formed and carries the same verdict.
    let json = cli::summary_json(&summary);
    assert!(json.starts_with('{') && json.ends_with('}'));
    assert!(json.contains("\"clean\":true"));
    assert!(json.contains("\"have\":3"));

    // A torrent whose payload was never written is not clean, but its files were
    // still created at their declared size.
    let absent = Synthetic::single("absent.bin", 8, 4);
    let absent_file = dir.path().join("absent.torrent");
    fs::write(&absent_file, &absent.bytes).unwrap();
    let missing = cli::verify_torrent_file(&absent_file, &media)
        .await
        .unwrap();
    assert_eq!(missing.exit_code(), 1);
    assert!(!missing.clean);
    assert_eq!(missing.have, 0);
    assert_eq!(missing.missing, 2);
    assert_eq!(fs::metadata(media.join("absent.bin")).unwrap().len(), 8);
}

#[tokio::test]
async fn verify_command_reports_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::single("film.bin", 8, 4);
    let torrent_file = dir.path().join("test.torrent");
    fs::write(&torrent_file, &torrent.bytes).unwrap();
    let media = dir.path().join("media");
    fs::create_dir_all(&media).unwrap();
    // Right length, wrong content.
    fs::write(media.join("film.bin"), vec![0_u8; 8]).unwrap();

    let summary: VerifySummary = cli::verify_torrent_file(&torrent_file, &media)
        .await
        .unwrap();
    assert_eq!(summary.exit_code(), 1);
    assert!(!summary.clean);
    assert_eq!(summary.have, 0);
    assert_eq!(summary.corrupt, 2);
    assert_eq!(summary.unreadable, 0);
}

#[tokio::test]
async fn verify_command_rejects_hostile_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::multi("root", &[(vec!["CON"], 4)], 4, false);
    let torrent_file = dir.path().join("hostile.torrent");
    fs::write(&torrent_file, &torrent.bytes).unwrap();
    let media = dir.path().join("media");
    fs::create_dir_all(&media).unwrap();

    let error = cli::verify_torrent_file(&torrent_file, &media)
        .await
        .unwrap_err();
    assert_eq!(error.exit_code(), 2);
    assert!(matches!(
        error,
        cli::CliError::Add(AddError::UnsafePath { .. })
    ));
}

#[tokio::test]
async fn preallocate_none_creates_unreadable_files_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let torrent = Synthetic::single("file.bin", 4, 4);
    let session = start_session(dir.path(), |config| {
        config.preallocate = flux_daemon::Preallocate::None;
    })
    .await;
    let mut events = session.subscribe();

    let state = session.add_torrent(torrent.bytes.clone()).await.unwrap();
    // `none` really does not grow the file, and the session does not pretend it did.
    assert_eq!(fs::metadata(dir.path().join("file.bin")).unwrap().len(), 0);
    let (have, missing) = wait_finished(&mut events, &state.id).await;
    assert_eq!((have, missing), (0, 1));

    let state = session.get(&torrent.id).await.unwrap();
    assert_eq!(state.verification, Verification::Complete);
    assert_eq!(state.pieces_have, 0);
    assert_eq!(state.pieces_corrupt, 0);
    // An unreadable piece is reported as an error, never as a corrupt hash.
    assert!(
        state
            .last_error
            .as_deref()
            .is_some_and(|error| !error.is_empty()),
        "the read failure must be surfaced: {:?}",
        state.last_error
    );
    session.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_cannot_start_with_a_bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        download_dir: dir.path().to_path_buf(),
        max_active_downloads: 0,
        ..Config::default()
    };
    let error = SessionHandle::start(config).await.unwrap_err();
    assert!(matches!(
        error,
        SessionError::Config(flux_daemon::ConfigError::ZeroLimit {
            field: "max_active_downloads"
        })
    ));

    // A file where the download dir should be is rejected, not silently used.
    let file = dir.path().join("not-a-dir");
    fs::write(&file, b"x").unwrap();
    let config = Config {
        download_dir: file,
        ..Config::default()
    };
    assert!(SessionHandle::start(config).await.is_err());
}
