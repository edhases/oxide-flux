//! End-to-end transfer tests: two real swarms over loopback TCP.
//!
//! Every torrent is built in-test with `flux_core::bencode::encode` and real
//! SHA-1 piece hashes. The payloads are ~1 MiB with 32 KiB pieces so a whole
//! suite runs in seconds; nothing here sleeps to order events.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use flux_core::bencode::{self, Value};
use flux_daemon::TorrentId;
use flux_daemon::pieces::{PieceBitfield, Priority, desired_pieces};
use flux_daemon::swarm::{self, SwarmConfig, SwarmError, SwarmEvent, SwarmHandle};
use flux_daemon::verify::{self, TorrentPlan};
use sha1::{Digest, Sha1};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// A generous per-test bound; real transfers finish in well under a second.
const WAIT: Duration = Duration::from_secs(30);
/// Small pieces keep the loopback transfer fast and the piece map readable.
const PIECE_LENGTH: u32 = 32 * 1024;

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

/// A deterministic payload byte, so the same torrent can be written into two
/// different roots and compared byte for byte.
fn payload_byte(offset: usize) -> u8 {
    (offset.wrapping_mul(37).wrapping_add(11) % 251) as u8
}

struct Synthetic {
    bytes: Vec<u8>,
    payload: Vec<u8>,
}

impl Synthetic {
    /// A v1 multi-file torrent with the given file lengths.
    fn multi(name: &str, files: &[(&[&str], usize)]) -> Self {
        let total: usize = files.iter().map(|(_, length)| *length).sum();
        let payload: Vec<u8> = (0..total).map(payload_byte).collect();
        let hashes: Vec<u8> = payload
            .chunks(PIECE_LENGTH as usize)
            .flat_map(sha1)
            .collect();
        let entries: Vec<Value> = files
            .iter()
            .map(|(path, length)| {
                dict(vec![
                    ("length", Value::Int(*length as i64)),
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
            ("piece length", Value::Int(i64::from(PIECE_LENGTH))),
            ("pieces", Value::Bytes(hashes)),
        ]);
        Self::finish(info, payload)
    }

    /// A v1 single-file torrent.
    fn single(name: &str, length: usize) -> Self {
        let payload: Vec<u8> = (0..length).map(payload_byte).collect();
        let hashes: Vec<u8> = payload
            .chunks(PIECE_LENGTH as usize)
            .flat_map(sha1)
            .collect();
        let info = dict(vec![
            ("length", Value::Int(length as i64)),
            ("name", text(name)),
            ("piece length", Value::Int(i64::from(PIECE_LENGTH))),
            ("pieces", Value::Bytes(hashes)),
        ]);
        Self::finish(info, payload)
    }

    /// A v2-only torrent. Its piece layers are parsed but never verified, which
    /// is exactly the case a swarm must refuse.
    fn v2(name: &str, files: &[(&str, usize)]) -> Self {
        let tree: Vec<(Vec<u8>, Value)> = files
            .iter()
            .map(|(path, length)| {
                let properties = dict(vec![
                    ("length", Value::Int(*length as i64)),
                    ("pieces root", Value::Bytes(vec![0x5a; 32])),
                ]);
                (path.as_bytes().to_vec(), dict(vec![("", properties)]))
            })
            .collect();
        let info = dict(vec![
            ("file tree", Value::Dict(tree)),
            ("meta version", Value::Int(2)),
            ("name", text(name)),
            ("piece length", Value::Int(i64::from(PIECE_LENGTH))),
        ]);
        Self::finish(info, Vec::new())
    }

    /// Wraps the info dictionary in a torrent and checks the id it produces.
    fn finish(info: Value, payload: Vec<u8>) -> Self {
        let bytes = bencode::encode(&dict(vec![("info", info)]));
        let parsed = verify::plan(&bytes).expect("synthetic torrent must plan");
        let id: TorrentId = TorrentId::from_info(&parsed.metainfo().info)
            .expect("synthetic torrent must have an id");
        assert!(!id.as_str().is_empty());
        Self { bytes, payload }
    }

    fn plan(&self) -> Arc<TorrentPlan> {
        Arc::new(verify::plan(&self.bytes).expect("synthetic torrent must plan"))
    }
}

// ---------------------------------------------------------------------------
// Roots and on-disk payloads
// ---------------------------------------------------------------------------

/// Mirrors what the session does before starting a swarm: every file exists at
/// its declared length, zero-filled. The swarm itself uses `Allocation::None`.
fn preallocate(plan: &TorrentPlan, root: &Path) {
    for file in plan.files() {
        let path = root.join(&file.relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, vec![0_u8; file.length as usize]).unwrap();
    }
}

/// Writes a full torrent address space into the plan's files.
fn write_payload(plan: &TorrentPlan, root: &Path, payload: &[u8]) {
    for file in plan.files() {
        let start = file.offset as usize;
        let end = start + file.length as usize;
        let path = root.join(&file.relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, &payload[start..end]).unwrap();
    }
}

/// Concatenates the plan's files in torrent order. A file that was never
/// written is padded with its declared zeros, so a skipped file cannot hide
/// behind a short read.
fn read_root(plan: &TorrentPlan, root: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for file in plan.files() {
        let mut bytes = fs::read(root.join(&file.relative)).unwrap_or_default();
        let length = file.length as usize;
        if bytes.len() < length {
            bytes.resize(length, 0);
        }
        out.extend_from_slice(&bytes[..length]);
    }
    out
}

fn all_bits(count: usize) -> PieceBitfield {
    let mut bits = PieceBitfield::new(count);
    for piece in 0..count {
        bits.insert(piece);
    }
    bits
}

fn empty_bits(count: usize) -> PieceBitfield {
    PieceBitfield::new(count)
}

// ---------------------------------------------------------------------------
// Swarms
// ---------------------------------------------------------------------------

fn config(
    plan: Arc<TorrentPlan>,
    root: &Path,
    peer_id: [u8; 20],
    have: PieceBitfield,
    desired: PieceBitfield,
) -> SwarmConfig {
    SwarmConfig {
        plan,
        root: root.to_path_buf(),
        peer_id,
        listen_port: 0,
        initial_have: have,
        desired,
        max_peers: 8,
        max_upload_slots: 4,
        announce: None,
        verifier_threads: 2,
    }
}

fn spawn_swarm(config: SwarmConfig) -> (SwarmHandle, mpsc::Receiver<SwarmEvent>) {
    let (events, rx) = mpsc::channel(256);
    let handle = swarm::spawn(config, events).expect("swarm must start");
    (handle, rx)
}

/// A seeder: everything verified, everything desired.
fn start_seeder(
    plan: Arc<TorrentPlan>,
    root: &Path,
    peer_id: [u8; 20],
) -> (SwarmHandle, mpsc::Receiver<SwarmEvent>) {
    let count = plan.piece_count();
    spawn_swarm(config(
        plan,
        root,
        peer_id,
        all_bits(count),
        all_bits(count),
    ))
}

/// Drains a swarm's events so a full channel never backpressures the swarm.
fn drain_events(mut rx: mpsc::Receiver<SwarmEvent>) {
    drop(tokio::spawn(
        async move { while rx.recv().await.is_some() {} },
    ));
}

/// Connects a leecher to a seeder through a real listener: the leecher dials,
/// the test accepts, the seeder accepts the handshake.
async fn connect(leecher: &SwarmHandle, seeder: &SwarmHandle) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert!(leecher.add_peer(addr).await, "the leecher must dial");
    let (stream, _) = listener.accept().await.unwrap();
    assert!(seeder.add_inbound(stream).await, "the seeder must accept");
}

/// Waits for `Completed`, returning every event it saw first.
async fn wait_for_completion(rx: &mut mpsc::Receiver<SwarmEvent>) -> Vec<SwarmEvent> {
    let collect = async {
        let mut seen = Vec::new();
        loop {
            match rx.recv().await {
                Some(SwarmEvent::Completed) => return seen,
                Some(SwarmEvent::Failed(error)) => panic!("swarm failed: {error}"),
                Some(event) => seen.push(event),
                None => panic!("swarm event channel closed before completion"),
            }
        }
    };
    timeout(WAIT, collect)
        .await
        .expect("the transfer must complete")
}

fn verified_in(events: &[SwarmEvent]) -> Vec<u32> {
    events
        .iter()
        .filter_map(|event| match event {
            SwarmEvent::PieceVerified { piece } => Some(*piece),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_transfer_moves_every_piece() {
    let synthetic = Synthetic::multi(
        "payload",
        &[(&["a.bin"], 512 * 1024), (&["b.bin"], 512 * 1024)],
    );
    let plan = synthetic.plan();
    let pieces = plan.piece_count();
    assert_eq!(pieces, 32);

    let seeder_dir = TempDir::new().unwrap();
    preallocate(&plan, seeder_dir.path());
    write_payload(&plan, seeder_dir.path(), &synthetic.payload);

    let leecher_dir = TempDir::new().unwrap();
    preallocate(&plan, leecher_dir.path());

    let (seeder, seeder_events) = start_seeder(plan.clone(), seeder_dir.path(), [0x51; 20]);
    drain_events(seeder_events);
    let (leecher, mut leecher_events) = spawn_swarm(config(
        plan.clone(),
        leecher_dir.path(),
        [0x52; 20],
        empty_bits(pieces),
        all_bits(pieces),
    ));

    connect(&leecher, &seeder).await;
    let events = wait_for_completion(&mut leecher_events).await;

    let mut verified = verified_in(&events);
    verified.sort_unstable();
    verified.dedup();
    assert_eq!(
        verified,
        (0..pieces as u32).collect::<Vec<_>>(),
        "every piece must verify exactly once"
    );

    let snapshot = leecher.snapshot().await.expect("leecher is alive");
    assert!(snapshot.complete);
    assert_eq!(snapshot.pieces_have, pieces);
    assert_eq!(
        snapshot.downloaded_bytes,
        synthetic.payload.len() as u64,
        "events: {events:?}"
    );

    leecher.shutdown().await;

    // The bytes the verifier hashed are the bytes on disk.
    assert_eq!(
        read_root(&plan, leecher_dir.path()),
        synthetic.payload,
        "the leecher's files must match the payload byte for byte"
    );

    let seeder_snapshot = seeder.snapshot().await.expect("seeder is alive");
    assert!(
        seeder_snapshot.uploaded_bytes > 0,
        "the seeder must count real bytes, got {seeder_snapshot:?}"
    );
    seeder.shutdown().await;
}

#[tokio::test]
async fn selective_download_leaves_the_skipped_file_untouched() {
    let synthetic = Synthetic::multi(
        "selective",
        &[(&["one.bin"], 512 * 1024), (&["two.bin"], 512 * 1024)],
    );
    let plan = synthetic.plan();
    assert_eq!(plan.piece_count(), 32);

    // File 1 is wanted, file 2 is skipped. Both are piece-aligned here, so the
    // selection is exactly the first half of the pieces.
    let desired = desired_pieces(plan.layout(), &[Priority::Normal, Priority::Skip]).unwrap();
    assert_eq!(
        desired.ones().collect::<Vec<_>>(),
        (0..16).collect::<Vec<_>>()
    );

    let seeder_dir = TempDir::new().unwrap();
    preallocate(&plan, seeder_dir.path());
    write_payload(&plan, seeder_dir.path(), &synthetic.payload);

    let leecher_dir = TempDir::new().unwrap();
    preallocate(&plan, leecher_dir.path());

    let (seeder, seeder_events) = start_seeder(plan.clone(), seeder_dir.path(), [0x61; 20]);
    drain_events(seeder_events);
    let (leecher, mut leecher_events) = spawn_swarm(config(
        plan.clone(),
        leecher_dir.path(),
        [0x62; 20],
        empty_bits(plan.piece_count()),
        desired,
    ));

    connect(&leecher, &seeder).await;
    let events = wait_for_completion(&mut leecher_events).await;

    let mut verified = verified_in(&events);
    verified.sort_unstable();
    assert_eq!(
        verified,
        (0..16).collect::<Vec<_>>(),
        "only desired pieces may be fetched and verified"
    );

    let snapshot = leecher.snapshot().await.expect("leecher is alive");
    assert_eq!(snapshot.pieces_have, 16);
    assert!(snapshot.complete, "the selection is complete");
    leecher.shutdown().await;

    let on_disk = read_root(&plan, leecher_dir.path());
    let half = 512 * 1024;
    assert_eq!(&on_disk[..half], &synthetic.payload[..half]);
    assert!(
        on_disk[half..].iter().all(|byte| *byte == 0),
        "the skipped file must stay untouched"
    );

    seeder.shutdown().await;
}

#[tokio::test]
async fn corrupted_pieces_are_rejected_and_refetched() {
    let synthetic = Synthetic::single("corrupt.bin", 1024 * 1024);
    let plan = synthetic.plan();
    let pieces = plan.piece_count();

    // The malicious seeder has one flipped byte in piece 0 on disk but the
    // honest metainfo hashes: it serves bytes that cannot verify.
    let malicious_dir = TempDir::new().unwrap();
    preallocate(&plan, malicious_dir.path());
    let mut corrupt = synthetic.payload.clone();
    corrupt[0] ^= 0xFF;
    write_payload(&plan, malicious_dir.path(), &corrupt);

    let honest_dir = TempDir::new().unwrap();
    preallocate(&plan, honest_dir.path());
    write_payload(&plan, honest_dir.path(), &synthetic.payload);

    let leecher_dir = TempDir::new().unwrap();
    preallocate(&plan, leecher_dir.path());

    let (malicious, malicious_events) =
        start_seeder(plan.clone(), malicious_dir.path(), [0x71; 20]);
    drain_events(malicious_events);
    let (honest, honest_events) = start_seeder(plan.clone(), honest_dir.path(), [0x72; 20]);
    drain_events(honest_events);
    let (leecher, mut leecher_events) = spawn_swarm(config(
        plan.clone(),
        leecher_dir.path(),
        [0x73; 20],
        empty_bits(pieces),
        all_bits(pieces),
    ));

    connect(&leecher, &malicious).await;

    // Phase 1: the only peer lies about piece 0, so verification must catch it.
    let mut seen = Vec::new();
    let corrupt_piece = timeout(WAIT, async {
        loop {
            match leecher_events.recv().await {
                Some(event @ SwarmEvent::PieceCorrupt { piece }) => {
                    seen.push(event);
                    return piece;
                }
                Some(event) => seen.push(event),
                None => panic!("event channel closed"),
            }
        }
    })
    .await
    .expect("a corrupt piece must be reported");
    assert_eq!(corrupt_piece, 0, "the flipped byte is in piece 0");
    malicious.shutdown().await;

    // Phase 2: an honest peer serves the same piece again.
    connect(&leecher, &honest).await;
    seen.extend(wait_for_completion(&mut leecher_events).await);

    // No piece may be claimed verified before the corrupt one is reported, and
    // no piece may be verified twice in the whole run.
    let mut verified: HashMap<u32, usize> = HashMap::new();
    let mut first_corrupt = None;
    let mut first_verified = None;
    for (index, event) in seen.iter().enumerate() {
        match event {
            SwarmEvent::PieceCorrupt { piece } if *piece == corrupt_piece => {
                first_corrupt.get_or_insert(index);
            }
            SwarmEvent::PieceVerified { piece } => {
                *verified.entry(*piece).or_default() += 1;
                if *piece == corrupt_piece {
                    first_verified.get_or_insert(index);
                }
            }
            _ => {}
        }
    }
    assert!(first_corrupt.is_some(), "the corrupt piece was reported");
    assert!(
        first_verified > first_corrupt,
        "piece 0 must only verify after the corrupt copy was rejected"
    );
    assert_eq!(verified.len(), pieces, "every piece must eventually verify");
    assert!(
        verified.values().all(|count| *count == 1),
        "a verified piece must not be reported twice: {verified:?}"
    );

    leecher.shutdown().await;
    assert_eq!(
        read_root(&plan, leecher_dir.path()),
        synthetic.payload,
        "the refetched piece must be the honest bytes"
    );

    honest.shutdown().await;
}

#[tokio::test]
async fn a_disconnect_requeues_in_flight_pieces() {
    let synthetic = Synthetic::single("disconnect.bin", 1024 * 1024);
    let plan = synthetic.plan();
    let pieces = plan.piece_count();

    let first_dir = TempDir::new().unwrap();
    preallocate(&plan, first_dir.path());
    write_payload(&plan, first_dir.path(), &synthetic.payload);

    let second_dir = TempDir::new().unwrap();
    preallocate(&plan, second_dir.path());
    write_payload(&plan, second_dir.path(), &synthetic.payload);

    let leecher_dir = TempDir::new().unwrap();
    preallocate(&plan, leecher_dir.path());

    let (first, first_events) = start_seeder(plan.clone(), first_dir.path(), [0x81; 20]);
    drain_events(first_events);
    let (second, second_events) = start_seeder(plan.clone(), second_dir.path(), [0x82; 20]);
    drain_events(second_events);
    let (leecher, mut leecher_events) = spawn_swarm(config(
        plan.clone(),
        leecher_dir.path(),
        [0x83; 20],
        empty_bits(pieces),
        all_bits(pieces),
    ));

    connect(&leecher, &first).await;

    // Wait until the leecher has actually accepted a piece, then yank the peer
    // while its other requests are still outstanding.
    timeout(WAIT, async {
        loop {
            match leecher_events.recv().await {
                Some(SwarmEvent::PieceVerified { .. }) => return,
                Some(SwarmEvent::Failed(error)) => panic!("swarm failed: {error}"),
                Some(_) => {}
                None => panic!("event channel closed"),
            }
        }
    })
    .await
    .expect("the first piece must arrive before the peer is dropped");

    first.shutdown().await;

    // A second, honest peer must be able to finish what the first left behind.
    connect(&leecher, &second).await;
    wait_for_completion(&mut leecher_events).await;

    leecher.shutdown().await;
    assert_eq!(
        read_root(&plan, leecher_dir.path()),
        synthetic.payload,
        "the requeued pieces must be fetched in full"
    );

    second.shutdown().await;
}

#[tokio::test]
async fn v2_only_torrents_are_refused() {
    let synthetic = Synthetic::v2("v2-only", &[("data.bin", 64 * 1024)]);
    let plan = synthetic.plan();
    assert!(
        plan.piece_hashes().is_none(),
        "a v2-only plan has no v1 hashes to verify against"
    );

    let root = TempDir::new().unwrap();
    preallocate(&plan, root.path());
    let pieces = plan.piece_count();
    let (events, _rx) = mpsc::channel(4);

    let result = swarm::spawn(
        config(
            plan.clone(),
            root.path(),
            [0x91; 20],
            empty_bits(pieces),
            all_bits(pieces),
        ),
        events,
    );
    assert!(
        matches!(result, Err(SwarmError::Unverifiable)),
        "a v2-only torrent must be refused, not downloaded unverifiably"
    );
}
