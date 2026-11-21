//! End-to-end transfer through the public session API.
//!
//! Two real sessions on loopback: one seeds a torrent it already has on disk, the
//! other starts empty, is pointed at the seeder's listen port, and must verify
//! every piece before reporting completion. No trackers, no DHT, no sleeps in the
//! scheduling path — only bounded polling of state that the engine owns.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use flux_core::bencode::{self, Value};
use flux_daemon::{Config, SessionHandle, Status, TorrentId, TorrentState};
use flux_net::metadata::{METADATA_PIECE_SIZE, MetadataMessage};
use flux_net::peer::{PeerCommand, PeerConfig, PeerEvent};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// A multi-file torrent plus the exact payload each file must contain.
struct Fixture {
    bytes: Vec<u8>,
    id: TorrentId,
    name: String,
    files: Vec<(String, Vec<u8>)>,
    info_bytes: Vec<u8>,
}

fn fixture(name: &str, file_names: &[&str], lengths: &[usize], piece_length: u32) -> Fixture {
    fixture_with_announce(name, file_names, lengths, piece_length, None)
}

fn fixture_with_announce(
    name: &str,
    file_names: &[&str],
    lengths: &[usize],
    piece_length: u32,
    announce: Option<&str>,
) -> Fixture {
    let mut payload = Vec::new();
    let mut files = Vec::new();
    for (index, (file_name, length)) in file_names.iter().zip(lengths).enumerate() {
        let byte = b'A' + u8::try_from(index).unwrap() % 26;
        let data = vec![byte; *length];
        payload.extend_from_slice(&data);
        files.push(((*file_name).to_owned(), data));
    }
    let pieces: Vec<u8> = payload
        .chunks(usize::try_from(piece_length).unwrap())
        .flat_map(sha1)
        .collect();
    let entries: Vec<Value> = files
        .iter()
        .map(|(file_name, data)| {
            dict(vec![
                ("length", Value::Int(i64::try_from(data.len()).unwrap())),
                ("path", Value::List(vec![text(file_name)])),
            ])
        })
        .collect();
    let info = dict(vec![
        ("files", Value::List(entries)),
        ("name", text(name)),
        ("piece length", Value::Int(i64::from(piece_length))),
        ("pieces", Value::Bytes(pieces)),
    ]);
    let info_bytes = bencode::encode(&info);
    let mut top = vec![("info", info)];
    if let Some(announce) = announce {
        top.push(("announce", text(announce)));
    }
    let bytes = bencode::encode(&dict(top));
    let parsed = flux_core::metainfo::parse(&bytes).unwrap();
    let id = TorrentId::from_info(&parsed.info).unwrap();
    Fixture {
        bytes,
        id,
        name: name.to_owned(),
        files,
        info_bytes,
    }
}

impl Fixture {
    fn write_to(&self, root: &Path) {
        for (file_name, data) in &self.files {
            let path = root.join(&self.name).join(file_name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
    }

    fn assert_on_disk(&self, root: &Path) {
        for (file_name, data) in &self.files {
            let path = root.join(&self.name).join(file_name);
            let actual = std::fs::read(&path).unwrap();
            assert_eq!(actual.len(), data.len(), "length of {file_name}");
            assert!(actual == *data, "bytes of {file_name} differ");
        }
    }

    fn payload_len(&self) -> u64 {
        self.files.iter().map(|(_, data)| data.len() as u64).sum()
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn config(root: &Path, listen_port: u16) -> Config {
    Config {
        download_dir: root.to_path_buf(),
        listen_port,
        api_token: None,
        persist: false,
        // No DNS, no UDP: the tests below drive peers explicitly.
        enable_dht: false,
        ..Config::default()
    }
}

async fn wait_for(
    session: &SessionHandle,
    id: &TorrentId,
    what: &str,
    mut ready: impl FnMut(&TorrentState) -> bool,
) -> TorrentState {
    let deadline = Instant::now() + WAIT;
    loop {
        match session.get(id).await {
            Ok(state) if ready(&state) => return state,
            Ok(state) => assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last state: {state:?}"
            ),
            // A restored torrent is not registered until its files are prepared.
            Err(flux_daemon::SessionError::UnknownTorrent { .. }) => assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; torrent was never restored"
            ),
            Err(error) => panic!("waiting for {what}: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn two_sessions_transfer_a_multi_file_torrent() {
    let seeder_root = tempfile::tempdir().unwrap();
    let leecher_root = tempfile::tempdir().unwrap();
    let torrent = fixture(
        "movie",
        &["part1.bin", "part2.bin"],
        &[40_000, 24_000],
        16_384,
    );
    torrent.write_to(seeder_root.path());

    let port = free_port();
    let seeder = SessionHandle::start(config(seeder_root.path(), port))
        .await
        .unwrap();
    let seeder_state = seeder.add_torrent(torrent.bytes.clone()).await.unwrap();
    assert_eq!(seeder_state.id, torrent.id);
    let seeded = wait_for(&seeder, &torrent.id, "seeder to verify its data", |state| {
        state.complete
    })
    .await;
    assert_eq!(seeded.pieces_have, seeded.piece_count);
    assert!(matches!(seeded.status, Status::Seeding | Status::Ready));

    let leecher = SessionHandle::start(config(leecher_root.path(), 0))
        .await
        .unwrap();
    let leecher_state = leecher.add_torrent(torrent.bytes.clone()).await.unwrap();
    assert_eq!(leecher_state.pieces_have, 0);
    // The swarm starts as soon as verification says "nothing here yet".
    wait_for(&leecher, &torrent.id, "leecher to become active", |state| {
        state.active
    })
    .await;

    let addr = SocketAddr::from(([127, 0, 0, 1], seeder.listen_port()));
    assert!(
        leecher.add_peer(&torrent.id, addr).await.unwrap(),
        "the leecher must accept the seeder address"
    );

    let done = wait_for(&leecher, &torrent.id, "the transfer to complete", |state| {
        state.complete
    })
    .await;
    assert_eq!(done.pieces_have, done.piece_count);
    assert!(done.peers >= 1, "peer count must be real: {done:?}");
    // Counters are published on their own tick; wait for the honest value.
    let counted = wait_for(&leecher, &torrent.id, "download counters", |state| {
        state.downloaded_bytes >= torrent.payload_len()
    })
    .await;
    assert_eq!(counted.downloaded_bytes, torrent.payload_len());

    // Bytes on disk are the proof, not the counters.
    torrent.assert_on_disk(leecher_root.path());
    assert_eq!(
        std::fs::metadata(leecher_root.path().join("movie").join("part1.bin"))
            .unwrap()
            .len(),
        40_000
    );

    let seeder_after = wait_for(&seeder, &torrent.id, "upload counters", |state| {
        state.uploaded_bytes >= torrent.payload_len()
    })
    .await;
    assert!(
        seeder_after.uploaded_bytes >= torrent.payload_len(),
        "seeder uploaded {} bytes",
        seeder_after.uploaded_bytes
    );

    leecher.shutdown().await.unwrap();
    seeder.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_paused_torrent_does_not_swarm_and_resumes() {
    let root = tempfile::tempdir().unwrap();
    let torrent = fixture("movie", &["one.bin"], &[1_000], 1_000);
    torrent.write_to(root.path());

    let session = SessionHandle::start(config(root.path(), 0)).await.unwrap();
    session.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_for(&session, &torrent.id, "verification", |state| {
        state.complete
    })
    .await;

    session.pause(&torrent.id).await.unwrap();
    let paused = session.get(&torrent.id).await.unwrap();
    assert_eq!(paused.status, Status::Paused);
    assert!(!paused.active);

    session.resume(&torrent.id).await.unwrap();
    let resumed = session.get(&torrent.id).await.unwrap();
    assert!(resumed.active);
    assert!(matches!(resumed.status, Status::Seeding | Status::Ready));

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_selective_download_waits_for_exactly_the_selected_pieces() {
    let seeder_root = tempfile::tempdir().unwrap();
    let leecher_root = tempfile::tempdir().unwrap();
    let torrent = fixture(
        "show",
        &["episode.bin", "extras.bin"],
        &[40_000, 40_000],
        16_384,
    );
    torrent.write_to(seeder_root.path());

    let seeder = SessionHandle::start(config(seeder_root.path(), free_port()))
        .await
        .unwrap();
    seeder.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_for(&seeder, &torrent.id, "seeder verification", |state| {
        state.complete
    })
    .await;

    let leecher = SessionHandle::start(config(leecher_root.path(), 0))
        .await
        .unwrap();
    leecher.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_for(&leecher, &torrent.id, "leecher active", |state| {
        state.active
    })
    .await;

    // Skip the second file (index 1); only the first file's pieces are wanted.
    leecher
        .set_file_priorities(&torrent.id, [(1, flux_daemon::Priority::Skip)])
        .await
        .unwrap();
    let state = leecher.get(&torrent.id).await.unwrap();
    assert_eq!(state.files[1].priority, flux_daemon::Priority::Skip);
    assert!(state.pieces_desired < state.piece_count);

    let addr = SocketAddr::from(([127, 0, 0, 1], seeder.listen_port()));
    leecher.add_peer(&torrent.id, addr).await.unwrap();

    let done = wait_for(&leecher, &torrent.id, "selected pieces", |state| {
        state.complete
    })
    .await;
    // Complete means "every desired piece is here", not "everything".
    assert!(done.pieces_have < done.piece_count);
    assert!(done.complete);

    let first = std::fs::read(leecher_root.path().join("show").join("episode.bin")).unwrap();
    assert_eq!(first.len(), 40_000);
    assert!(first.iter().all(|byte| *byte == b'A'));

    let extras = std::fs::read(leecher_root.path().join("show").join("extras.bin")).unwrap();
    // Piece 2 straddles both files (32 768..49 152), so fetching the selected
    // file necessarily brings its first 9 152 bytes. Everything after the shared
    // piece must still be untouched.
    let shared = 49_152 - 40_000;
    assert!(
        extras[shared..].iter().all(|byte| *byte == 0),
        "bytes owned only by the skipped file must not be downloaded"
    );

    leecher.shutdown().await.unwrap();
    seeder.shutdown().await.unwrap();
}

/// A one-endpoint HTTP tracker that answers every announce with `peer`.
async fn start_tracker(peer: SocketAddr) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                let mut body = Vec::new();
                body.extend_from_slice(b"d8:intervali2e5:peers6:");
                match peer.ip() {
                    std::net::IpAddr::V4(ip) => body.extend_from_slice(&ip.octets()),
                    std::net::IpAddr::V6(_) => body.extend_from_slice(&[127, 0, 0, 1]),
                }
                body.extend_from_slice(&peer.port().to_be_bytes());
                body.push(b'e');
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_leecher_finds_its_peer_through_an_http_tracker() {
    let seeder_root = tempfile::tempdir().unwrap();
    let leecher_root = tempfile::tempdir().unwrap();

    let port = free_port();
    let announce = format!(
        "http://127.0.0.1:{}/announce",
        start_tracker(SocketAddr::from(([127, 0, 0, 1], port))).await
    );
    let torrent =
        fixture_with_announce("tracked", &["data.bin"], &[48_000], 16_384, Some(&announce));
    torrent.write_to(seeder_root.path());

    let seeder = SessionHandle::start(config(seeder_root.path(), port))
        .await
        .unwrap();
    seeder.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_for(&seeder, &torrent.id, "seeder verification", |state| {
        state.complete
    })
    .await;

    let leecher = SessionHandle::start(config(leecher_root.path(), 0))
        .await
        .unwrap();
    leecher.add_torrent(torrent.bytes.clone()).await.unwrap();

    // No add_peer: the only route to the seeder is the tracker announce.
    let done = wait_for(&leecher, &torrent.id, "tracker discovery", |state| {
        state.complete
    })
    .await;
    assert_eq!(done.pieces_have, done.piece_count);
    torrent.assert_on_disk(leecher_root.path());

    leecher.shutdown().await.unwrap();
    seeder.shutdown().await.unwrap();
}

/// A metadata-only peer: answers `ut_metadata` requests for `info`.
async fn spawn_metadata_provider(info_hash: [u8; 20], info: Vec<u8>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let info = info.clone();
            tokio::spawn(async move {
                let mut config = PeerConfig::new(info_hash, *b"-PF0100-000000000002");
                config.pieces = 0;
                let Ok((handle, mut events)) = flux_net::peer::accept(stream, config).await else {
                    return;
                };
                let handshake = bencode::encode(&Value::Dict(vec![
                    (
                        b"m".to_vec(),
                        Value::Dict(vec![(b"ut_metadata".to_vec(), Value::Int(3))]),
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
                    let PeerEvent::Extended { id, payload } = event else {
                        continue;
                    };
                    if id != 3 {
                        continue;
                    }
                    let Ok(MetadataMessage::Request { piece }) = MetadataMessage::decode(&payload)
                    else {
                        continue;
                    };
                    let size = METADATA_PIECE_SIZE as usize;
                    let start = piece as usize * size;
                    let end = (start + size).min(info.len());
                    let message = MetadataMessage::Data {
                        piece,
                        total_size: info.len() as u32,
                        data: info[start..end].to_vec(),
                    };
                    if handle
                        .send(PeerCommand::Extended {
                            id: 3,
                            payload: message.encode(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_magnet_link_fetches_metadata_and_adds_the_torrent() {
    let root = tempfile::tempdir().unwrap();
    let torrent = fixture("magnet", &["film.bin"], &[32_768], 16_384);
    let hash = sha1(&torrent.info_bytes);

    let provider = spawn_metadata_provider(hash, torrent.info_bytes.clone()).await;
    let tracker_port = start_tracker(provider).await;
    let tracker = format!("http://127.0.0.1:{tracker_port}/announce");
    let uri = format!(
        "magnet:?xt=urn:btih:{}&dn=magnet&tr={}",
        torrent.id.as_str(),
        tracker
    );

    let session = SessionHandle::start(config(root.path(), 0)).await.unwrap();
    let state = session.add_magnet(&uri).await.unwrap();
    assert_eq!(state.id, torrent.id);
    assert_eq!(state.name, "magnet");
    assert_eq!(state.piece_count, torrent.files[0].1.len().div_ceil(16_384));

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_session_survives_a_restart_with_priorities_and_pause() {
    let root = tempfile::tempdir().unwrap();
    let torrent = fixture(
        "movie",
        &["part1.bin", "part2.bin"],
        &[40_000, 24_000],
        16_384,
    );
    torrent.write_to(root.path());

    // This test is about persistence; enable it explicitly.
    let mut persistent = config(root.path(), 0);
    persistent.persist = true;
    let first = SessionHandle::start(persistent.clone()).await.unwrap();
    first.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_for(&first, &torrent.id, "first verification", |state| {
        state.complete
    })
    .await;
    first
        .set_file_priorities(&torrent.id, [(1, flux_daemon::Priority::Skip)])
        .await
        .unwrap();
    first.pause(&torrent.id).await.unwrap();
    first.shutdown().await.unwrap();

    let state_path = root.path().join(".fluxd-state.json");
    assert!(state_path.exists(), "the snapshot must be written");

    let second = SessionHandle::start(persistent).await.unwrap();
    let restored = wait_for(&second, &torrent.id, "restore", |state| {
        state.verification != flux_daemon::Verification::Pending || state.status == Status::Paused
    })
    .await;
    assert_eq!(restored.id, torrent.id);
    assert_eq!(restored.name, "movie");
    assert_eq!(restored.status, Status::Paused);
    assert_eq!(restored.files[1].priority, flux_daemon::Priority::Skip);
    assert_eq!(restored.pieces_desired, 3);
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_v2_only_torrent_is_storage_ready_but_never_pretends_to_download() {
    // Build a v2-only torrent: no SHA-1 pieces, so the engine cannot verify it.
    let tree = dict(vec![(
        "file.bin",
        dict(vec![(
            "",
            dict(vec![
                ("length", Value::Int(1_000)),
                ("pieces root", Value::Bytes(vec![0x5a; 32])),
            ]),
        )]),
    )]);
    let info = dict(vec![
        ("file tree", tree),
        ("meta version", Value::Int(2)),
        ("name", text("v2only")),
        ("piece length", Value::Int(16_384)),
    ]);
    let bytes = bencode::encode(&dict(vec![("info", info)]));

    let root = tempfile::tempdir().unwrap();
    let session = SessionHandle::start(config(root.path(), 0)).await.unwrap();
    let state = session.add_torrent(bytes).await.unwrap();
    assert_eq!(
        state.verification,
        flux_daemon::Verification::Unsupported,
        "v2-only has no verifier yet"
    );
    assert!(!state.complete);

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn streaming_layout_maps_files_onto_pieces() {
    let root = tempfile::tempdir().unwrap();
    let torrent = fixture("show", &["a.bin", "b.bin"], &[40_000, 24_000], 16_384);
    let session = SessionHandle::start(config(root.path(), 0)).await.unwrap();
    session.add_torrent(torrent.bytes.clone()).await.unwrap();

    let layout = session.stream_layout(&torrent.id).await.unwrap();
    assert_eq!(layout.piece_length, 16_384);
    assert_eq!(layout.root, root.path());
    assert_eq!(layout.files.len(), 2);
    assert_eq!(layout.files[0].path, "show/a.bin");
    assert_eq!(layout.files[0].offset, 0);
    assert_eq!(layout.files[0].piece_start, 0);
    assert_eq!(layout.files[0].piece_end, 3);
    assert_eq!(layout.files[1].offset, 40_000);
    assert_eq!(layout.files[1].piece_start, 2);
    assert_eq!(layout.files[1].piece_end, 4);

    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_completed_torrent_streams_over_loopback_http() {
    use flux_api::Engine as _;
    use flux_daemon::engine::DaemonEngine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let root = tempfile::tempdir().unwrap();
    let torrent = fixture("media", &["film.bin"], &[40_000], 16_384);
    torrent.write_to(root.path());

    let session = SessionHandle::start(config(root.path(), 0)).await.unwrap();
    session.add_torrent(torrent.bytes.clone()).await.unwrap();
    wait_for(&session, &torrent.id, "verification", |state| {
        state.complete
    })
    .await;

    let engine = DaemonEngine::new(session.clone(), root.path());
    let url = engine.stream_url(torrent.id.as_str(), 0).await.unwrap();
    let addr = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_owned();

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /stream/0 HTTP/1.1\r\nHost: localhost\r\nRange: bytes=0-1023\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("headers end");
    let head = String::from_utf8_lossy(&response[..split]);
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
    assert!(head.contains("Content-Range: bytes 0-1023/40000"), "{head}");
    assert_eq!(&response[split + 4..], &torrent.files[0].1[..1024]);

    session.shutdown().await.unwrap();
}
