//! Behavioural tests for the portable disk layer.
//!
//! Everything here goes through the public API on purpose: the writer, the layout and
//! the verifier must stay useful without reaching into private state. The one exception
//! is deliberate — the byte-budget unit test lives next to the cache it measures.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc as std_mpsc};

use flux_disk::{
    Block, BlockBackend, BlockRejected, DiskError, DiskWriter, FileHandle, FileLayout, FileSet,
    FileSpec, LayoutOptions, PieceHash, PieceOutcome, StdBackend, SubmitError, Verifier,
    VerifyConfig, WriterConfig,
};
use sha1::{Digest, Sha1};
use tokio::sync::mpsc::error::TryRecvError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn layout(files: &[(&str, u64, u64)]) -> Result<Arc<FileLayout>, DiskError> {
    let specs = files
        .iter()
        .map(|(path, offset, length)| FileSpec::new(*path, *offset, *length))
        .collect();
    Ok(Arc::new(FileLayout::new(specs)?))
}

fn patterns(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn sha1(data: &[u8]) -> PieceHash {
    let mut hasher = Sha1::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = [0_u8; 20];
    out.copy_from_slice(&digest);
    out
}

/// Forwards to `StdBackend` and counts positional writes so coalescing is observable.
struct CountingBackend {
    inner: StdBackend,
    writes: Arc<AtomicUsize>,
    written_bytes: Arc<AtomicUsize>,
}

impl CountingBackend {
    fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let writes = Arc::new(AtomicUsize::new(0));
        let written_bytes = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inner: StdBackend::default(),
                writes: Arc::clone(&writes),
                written_bytes: Arc::clone(&written_bytes),
            },
            writes,
            written_bytes,
        )
    }
}

impl BlockBackend for CountingBackend {
    fn open(&self, path: &Path) -> io::Result<FileHandle> {
        self.inner.open(path)
    }

    fn preallocate(&self, handle: &FileHandle, len: u64) -> io::Result<()> {
        self.inner.preallocate(handle, len)
    }

    fn allocate_and_zero(&self, handle: &FileHandle, len: u64) -> io::Result<()> {
        self.inner.allocate_and_zero(handle, len)
    }

    fn read_exact_at(&self, handle: &FileHandle, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact_at(handle, offset, buf)
    }

    fn write_all_at(&self, handle: &FileHandle, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.written_bytes.fetch_add(buf.len(), Ordering::SeqCst);
        self.inner.write_all_at(handle, offset, buf)
    }

    fn sync(&self, handle: &FileHandle) -> io::Result<()> {
        self.inner.sync(handle)
    }
}

/// Parks the writer inside its first backend write until the test opens the gate. That
/// makes "the queue is full" a fact instead of a timing guess.
struct GatedBackend {
    inner: StdBackend,
    reached_write: std_mpsc::SyncSender<()>,
    open: Mutex<bool>,
    gate: Condvar,
}

impl GatedBackend {
    fn new() -> (Self, std_mpsc::Receiver<()>) {
        let (reached_write, reached) = std_mpsc::sync_channel(1);
        (
            Self {
                inner: StdBackend::default(),
                reached_write,
                open: Mutex::new(false),
                gate: Condvar::new(),
            },
            reached,
        )
    }

    fn release(&self) {
        if let Ok(mut open) = self.open.lock() {
            *open = true;
            self.gate.notify_all();
        }
    }

    fn wait(&self) -> io::Result<()> {
        // Best effort: the test only needs the first notification.
        let _ = self.reached_write.send(());
        let mut open = self
            .open
            .lock()
            .map_err(|_| io::Error::other("gate mutex poisoned"))?;
        while !*open {
            open = self
                .gate
                .wait(open)
                .map_err(|_| io::Error::other("gate mutex poisoned"))?;
        }
        Ok(())
    }
}

impl BlockBackend for GatedBackend {
    fn open(&self, path: &Path) -> io::Result<FileHandle> {
        self.inner.open(path)
    }

    fn preallocate(&self, handle: &FileHandle, len: u64) -> io::Result<()> {
        self.inner.preallocate(handle, len)
    }

    fn allocate_and_zero(&self, handle: &FileHandle, len: u64) -> io::Result<()> {
        self.inner.allocate_and_zero(handle, len)
    }

    fn read_exact_at(&self, handle: &FileHandle, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact_at(handle, offset, buf)
    }

    fn write_all_at(&self, handle: &FileHandle, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.wait()?;
        self.inner.write_all_at(handle, offset, buf)
    }

    fn sync(&self, handle: &FileHandle) -> io::Result<()> {
        self.inner.sync(handle)
    }
}

fn test_config(piece_length: u32) -> WriterConfig {
    WriterConfig {
        piece_length,
        queue_capacity: 8,
        cache_bytes: 1 << 20,
        event_capacity: 32,
        flush_interval: None,
        ..WriterConfig::new(piece_length)
    }
}

#[tokio::test]
async fn piece_crossing_file_boundary_roundtrips() -> TestResult {
    const PIECE: u32 = 16_384;
    let root = tempfile::tempdir()?;
    let layout = layout(&[("a.bin", 0, 8_192), ("b.bin", 8_192, 8_192)])?;
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());

    // One block, one piece, two files.
    let data = patterns(PIECE as usize, 7);
    let (writer, mut events) = DiskWriter::start(
        test_config(PIECE),
        Arc::clone(&layout),
        Arc::clone(&backend),
        root.path().to_path_buf(),
    )?;
    writer.write_block(Block::new(0, 0, data.clone())).await?;
    writer.flush().await?;
    writer.sync_all().await?;
    writer.shutdown().await?;

    let mut completions = 0;
    while let Some(event) = events.recv().await {
        assert!(
            event.result.is_ok(),
            "piece {} failed: {:?}",
            event.piece,
            event.result
        );
        completions += 1;
    }
    assert_eq!(completions, 1, "one complete piece, one completion event");

    assert_eq!(std::fs::read(root.path().join("a.bin"))?, &data[..8_192]);
    assert_eq!(std::fs::read(root.path().join("b.bin"))?, &data[8_192..]);

    // The read path goes through the same table, including a range that starts and ends
    // in the middle of the two files.
    let files = FileSet::open(layout, backend, root.path())?;
    let mut whole = vec![0_u8; data.len()];
    files.read_exact_at(0, &mut whole)?;
    assert_eq!(whole, data);

    let mut straddling = vec![0_u8; 100];
    files.read_exact_at(8_000, &mut straddling)?;
    assert_eq!(straddling, &data[8_000..8_100]);
    Ok(())
}

#[tokio::test]
async fn short_final_piece_and_zero_length_file() -> TestResult {
    const PIECE: u32 = 512;
    let root = tempfile::tempdir()?;
    let layout = layout(&[
        ("empty.bin", 0, 0),
        ("data.bin", 0, 1_000),
        ("tail.bin", 1_000, 10),
    ])?;

    assert_eq!(layout.total_length(), 1_010);
    assert_eq!(layout.piece_count(PIECE)?, 2);
    assert_eq!(layout.piece_range(PIECE, 0)?, (0, 512));
    assert_eq!(layout.piece_range(PIECE, 1)?, (512, 498));
    assert!(matches!(
        layout.piece_range(PIECE, 2),
        Err(DiskError::PieceOutOfRange { .. })
    ));
    // The zero-length file occupies no bytes, so it can never be a mapping target.
    let segments = layout.map(0, 10)?;
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].file, 1);

    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());
    let data = patterns(1_010, 3);
    let (writer, mut events) = DiskWriter::start(
        test_config(PIECE),
        Arc::clone(&layout),
        Arc::clone(&backend),
        root.path().to_path_buf(),
    )?;

    for (piece, begin, len) in [
        (0_u32, 0_usize, 256_usize),
        (0, 256, 256),
        (1, 0, 256),
        (1, 256, 242),
    ] {
        let slice =
            data[begin + (piece as usize * 512)..begin + (piece as usize * 512) + len].to_vec();
        writer
            .write_block(Block::new(piece, begin as u32, slice))
            .await?;
    }
    writer.shutdown().await?;

    let mut completed = 0;
    while let Some(event) = events.recv().await {
        assert!(
            event.result.is_ok(),
            "piece {} failed: {:?}",
            event.piece,
            event.result
        );
        completed += 1;
    }
    assert_eq!(completed, 2);

    assert_eq!(std::fs::read(root.path().join("empty.bin"))?.len(), 0);
    assert_eq!(std::fs::read(root.path().join("data.bin"))?, &data[..1_000]);
    assert_eq!(std::fs::read(root.path().join("tail.bin"))?, &data[1_000..]);

    let files = FileSet::open(layout, backend, root.path())?;
    let mut roundtrip = vec![0_u8; 1_010];
    files.read_exact_at(0, &mut roundtrip)?;
    assert_eq!(roundtrip, data);
    Ok(())
}

#[tokio::test]
async fn writer_preallocates_declared_lengths() -> TestResult {
    let root = tempfile::tempdir()?;
    let layout = layout(&[("one.bin", 0, 4_096), ("two.bin", 4_096, 8_192)])?;
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::new(true));

    let mut config = test_config(16_384);
    config.allocation = flux_disk::Allocation::Sparse;
    let (writer, _events) = DiskWriter::start(
        config,
        layout,
        Arc::clone(&backend),
        root.path().to_path_buf(),
    )?;
    writer.shutdown().await?;

    let one = std::fs::metadata(root.path().join("one.bin"))?;
    let two = std::fs::metadata(root.path().join("two.bin"))?;
    assert_eq!(one.len(), 4_096);
    assert_eq!(two.len(), 8_192);
    Ok(())
}

#[test]
fn allocate_and_zero_commits_zeroed_bytes() -> TestResult {
    let root = tempfile::tempdir()?;
    let path = root.path().join("zeroed.bin");
    let backend = StdBackend::new(true);
    let handle = backend.open(&path)?;
    backend.allocate_and_zero(&handle, 256 * 1024)?;
    backend.sync(&handle)?;
    drop(handle);

    let bytes = std::fs::read(&path)?;
    assert_eq!(bytes.len(), 256 * 1024);
    assert!(bytes.iter().all(|byte| *byte == 0));
    Ok(())
}

#[tokio::test]
async fn small_blocks_coalesce_into_one_write_per_piece() -> TestResult {
    const PIECE: u32 = 16_384;
    let root = tempfile::tempdir()?;
    let layout = layout(&[("video.bin", 0, 2 * u64::from(PIECE))])?;
    let (backend, writes, written_bytes) = CountingBackend::new();
    let backend: Arc<dyn BlockBackend> = Arc::new(backend);

    let mut config = test_config(PIECE);
    config.allocation = flux_disk::Allocation::None;
    let (writer, mut events) = DiskWriter::start(
        config,
        Arc::clone(&layout),
        Arc::clone(&backend),
        root.path().to_path_buf(),
    )?;

    let data = patterns(2 * PIECE as usize, 11);
    for piece in 0..2_u32 {
        for block in 0..4_u32 {
            let start = (piece as usize * PIECE as usize) + (block as usize * 4_096);
            let payload = data[start..start + 4_096].to_vec();
            writer
                .write_block(Block::new(piece, block * 4_096, payload))
                .await?;
        }
    }
    writer.shutdown().await?;

    let mut completed = 0;
    while let Some(event) = events.recv().await {
        assert!(
            event.result.is_ok(),
            "piece {} failed: {:?}",
            event.piece,
            event.result
        );
        completed += 1;
    }
    assert_eq!(completed, 2);

    // Eight 4 KiB blocks, two 16 KiB pieces, two backend writes.
    assert_eq!(
        writes.load(Ordering::SeqCst),
        2,
        "expected one coalesced write per piece"
    );
    assert_eq!(written_bytes.load(Ordering::SeqCst), 2 * PIECE as usize);
    assert_eq!(std::fs::read(root.path().join("video.bin"))?, data);
    Ok(())
}

#[tokio::test]
async fn verifier_flags_flipped_byte() -> TestResult {
    const PIECE: u32 = 4_096;
    let root = tempfile::tempdir()?;
    let layout = layout(&[("payload.bin", 0, 2 * u64::from(PIECE))])?;
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());

    let data = patterns(2 * PIECE as usize, 5);
    let hashes = vec![sha1(&data[..PIECE as usize]), sha1(&data[PIECE as usize..])];

    let (writer, mut completions) = DiskWriter::start(
        test_config(PIECE),
        Arc::clone(&layout),
        Arc::clone(&backend),
        root.path().to_path_buf(),
    )?;
    for piece in 0..2_u32 {
        let start = piece as usize * PIECE as usize;
        writer
            .write_block(Block::new(
                piece,
                0,
                data[start..start + PIECE as usize].to_vec(),
            ))
            .await?;
    }
    writer.shutdown().await?;
    while let Some(event) = completions.recv().await {
        assert!(event.result.is_ok());
    }

    let (verifier, mut results) = Verifier::start(
        VerifyConfig::new(),
        Arc::clone(&layout),
        Arc::clone(&backend),
        root.path().to_path_buf(),
        PIECE,
        hashes.clone(),
    )?;
    verifier.verify(0).await?;
    verifier.verify(1).await?;
    let mut good = 0;
    for _ in 0..2 {
        let event = results.recv().await.ok_or("verifier closed early")?;
        assert!(
            matches!(&event.outcome, PieceOutcome::Good),
            "piece {} was {:?}",
            event.piece,
            event.outcome
        );
        good += 1;
    }
    verifier.shutdown().await?;
    assert_eq!(good, 2);

    // Flip one byte of piece 0 behind the writer's back.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("payload.bin"))?;
        file.seek(SeekFrom::Start(10))?;
        file.write_all(&[data[10] ^ 0xff])?;
        file.sync_all()?;
    }

    let (verifier, mut results) = Verifier::start(
        VerifyConfig::new(),
        layout,
        backend,
        root.path().to_path_buf(),
        PIECE,
        hashes.clone(),
    )?;
    verifier.verify(0).await?;
    let event = results.recv().await.ok_or("verifier closed early")?;
    verifier.shutdown().await?;

    match event.outcome {
        PieceOutcome::Corrupt { expected, actual } => {
            assert_eq!(expected, hashes[0]);
            assert_ne!(actual, expected);
        }
        other => panic!("expected corruption, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn full_queue_returns_the_block_instead_of_growing() -> TestResult {
    const PIECE: u32 = 16_384;
    let root = tempfile::tempdir()?;
    let layout = layout(&[("queue.bin", 0, 2 * u64::from(PIECE))])?;
    let (gated, reached) = GatedBackend::new();
    let gated = Arc::new(gated);
    let backend: Arc<dyn BlockBackend> = Arc::clone(&gated) as Arc<dyn BlockBackend>;

    let mut config = test_config(PIECE);
    config.queue_capacity = 2;
    config.allocation = flux_disk::Allocation::None;
    let (writer, mut events) =
        DiskWriter::start(config, layout, backend, root.path().to_path_buf())?;

    // Piece 0 is complete after this one block, so the worker tries to write it and
    // parks inside the gated backend.
    writer.try_write_block(Block::new(0, 0, patterns(PIECE as usize, 1)))?;
    reached.recv()?;
    assert_eq!(
        writer.capacity(),
        2,
        "the worker consumed the first command before blocking"
    );

    // Two slots, two blocks: the queue is now full and the worker cannot drain it.
    writer.try_write_block(Block::new(1, 0, patterns(8_192, 2)))?;
    writer.try_write_block(Block::new(1, 8_192, patterns(8_192, 3)))?;
    assert_eq!(writer.capacity(), 0);

    match writer.try_write_block(Block::new(1, 0, patterns(4_096, 4))) {
        Err(BlockRejected {
            block,
            reason: SubmitError::Full,
        }) => {
            assert_eq!(block.piece, 1);
            assert_eq!(block.data.len(), 4_096, "the payload came back intact");
        }
        Err(BlockRejected { reason, .. }) => panic!("expected Full, got {reason}"),
        Ok(()) => panic!("queue must reject when full"),
    }

    gated.release();
    writer.flush().await?;
    writer.shutdown().await?;

    let mut completed = 0;
    while let Some(event) = events.recv().await {
        assert!(
            event.result.is_ok(),
            "piece {} failed: {:?}",
            event.piece,
            event.result
        );
        completed += 1;
    }
    assert_eq!(completed, 2);
    assert_eq!(
        std::fs::metadata(root.path().join("queue.bin"))?.len(),
        2 * u64::from(PIECE)
    );
    Ok(())
}

#[test]
fn layout_rejects_overlap_gap_and_overflow() {
    assert!(matches!(
        FileLayout::new(vec![
            FileSpec::new("a.bin", 0, 10),
            FileSpec::new("b.bin", 5, 10),
        ]),
        Err(DiskError::Overlap { .. })
    ));
    assert!(matches!(
        FileLayout::new(vec![
            FileSpec::new("a.bin", 0, 10),
            FileSpec::new("b.bin", 20, 10),
        ]),
        Err(DiskError::Gap { .. })
    ));
    assert!(matches!(
        FileLayout::new(vec![FileSpec::new("a.bin", u64::MAX - 4, 10)]),
        Err(DiskError::LengthOverflow { .. })
    ));
    assert!(matches!(
        FileLayout::new(vec![FileSpec::new("a.bin", 0, 0)]),
        Err(DiskError::EmptyLayout)
    ));

    let gapped = FileLayout::with_options(
        vec![
            FileSpec::new("a.bin", 0, 10),
            FileSpec::new("b.bin", 20, 10),
        ],
        LayoutOptions { allow_gaps: true },
    );
    assert!(gapped.is_ok(), "gaps are allowed when asked for");
}

#[test]
fn layout_rejects_ranges_outside_the_torrent() -> TestResult {
    let layout = FileLayout::new(vec![FileSpec::new("a.bin", 0, 100)])?;
    assert!(matches!(
        layout.map(90, 20),
        Err(DiskError::OutOfRange { .. })
    ));
    assert!(matches!(
        layout.map(u64::MAX, 2),
        Err(DiskError::OutOfRange { .. })
    ));
    assert!(layout.map(100, 0)?.is_empty());
    assert_eq!(layout.map(0, 100)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn partial_piece_is_flushed_without_a_completion_event() -> TestResult {
    const PIECE: u32 = 1_024;
    let root = tempfile::tempdir()?;
    let layout = layout(&[("partial.bin", 0, 2 * u64::from(PIECE))])?;
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());

    let (writer, mut events) = DiskWriter::start(
        test_config(PIECE),
        Arc::clone(&layout),
        Arc::clone(&backend),
        root.path().to_path_buf(),
    )?;

    let data = patterns(256, 9);
    writer.write_block(Block::new(0, 0, data.clone())).await?;
    writer.flush().await?;

    // The 256 buffered bytes must be on disk even though the piece is incomplete.
    let files = FileSet::open(layout, backend, root.path())?;
    let mut head = vec![0_u8; 256];
    files.read_exact_at(0, &mut head)?;
    assert_eq!(head, data);
    assert_eq!(
        std::fs::metadata(root.path().join("partial.bin"))?.len(),
        2 * u64::from(PIECE)
    );

    // ... and an incomplete piece must not report success.
    assert!(
        matches!(events.try_recv(), Err(TryRecvError::Empty)),
        "partial flush must not emit a completion event"
    );

    writer.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn writer_reports_invalid_blocks_instead_of_indexing_out_of_bounds() -> TestResult {
    const PIECE: u32 = 512;
    let root = tempfile::tempdir()?;
    let layout = layout(&[("guarded.bin", 0, 1_024)])?;
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());

    let (writer, mut events) = DiskWriter::start(
        test_config(PIECE),
        layout,
        backend,
        root.path().to_path_buf(),
    )?;

    // Piece 0 is 512 bytes: a block starting at 256 with 512 bytes runs past the end.
    writer
        .write_block(Block::new(0, 256, vec![0_u8; 512]))
        .await?;
    // There is no piece 7.
    writer
        .write_block(Block::new(7, 0, vec![0_u8; 512]))
        .await?;
    // Empty payloads are rejected too.
    writer.write_block(Block::new(0, 0, Vec::new())).await?;
    writer.shutdown().await?;

    let mut failures = Vec::new();
    while let Some(event) = events.recv().await {
        match event.result {
            Ok(()) => panic!("piece {} must not complete", event.piece),
            Err(err) => failures.push((event.piece, err)),
        }
    }
    assert_eq!(failures.len(), 3);
    assert!(
        failures
            .iter()
            .any(|(piece, err)| *piece == 7 && matches!(err, DiskError::PieceOutOfRange { .. }))
    );
    assert!(
        failures
            .iter()
            .any(|(_, err)| matches!(err, DiskError::BlockOutOfBounds { .. }))
    );
    assert!(
        failures
            .iter()
            .any(|(_, err)| matches!(err, DiskError::EmptyBlock { .. }))
    );
    Ok(())
}

#[test]
fn refuses_paths_that_escape_the_download_root() -> TestResult {
    let root = tempfile::tempdir()?;
    for bad in ["../escape.bin", "..", "/absolute.bin"] {
        let layout = Arc::new(FileLayout::new(vec![FileSpec::new(bad, 0, 16)])?);
        let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());
        assert!(
            matches!(
                FileSet::open(layout, backend, root.path()),
                Err(DiskError::UnsafePath { .. })
            ),
            "`{bad}` must be refused before touching the filesystem"
        );
    }
    Ok(())
}
