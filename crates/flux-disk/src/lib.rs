//! Storage layer for Oxide Flux: preallocation, coalesced piece writes and piece
//! verification.
//!
//! # The blocking contract
//!
//! Nothing in this crate runs on the tokio runtime. [`DiskWriter`] and [`Verifier`]
//! own dedicated worker threads and are fed through **bounded** channels: a producer
//! that outruns the disk is slowed down (`write_block().await` waits for a slot,
//! `try_write_block` returns [`SubmitError::Full`]) instead of growing an unbounded
//! in-RAM queue.
//!
//! # Pieces are not files
//!
//! [`FileLayout`] owns the file boundary table and maps a torrent-address-space range
//! onto `(file, file_offset, len)` segments, so one 16 KiB block can land in two files
//! without the caller noticing, and the byte budget of a piece never depends on where
//! the file edges happen to fall.
//!
//! # Paths
//!
//! [`FileSpec::path`] is expected to be sanitized by the caller (see the workspace
//! `AGENTS.md`). As defence in depth this crate additionally refuses absolute paths and
//! `..` components before touching the filesystem.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use flux_disk::{
//!     Block, BlockBackend, DiskWriter, FileLayout, FileSet, FileSpec, PieceOutcome,
//!     StdBackend, Verifier, VerifyConfig, WriterConfig, verify_piece,
//! };
//!
//! # #[allow(dead_code)]
//! async fn example() -> Result<(), Box<dyn std::error::Error>> {
//!     // 16 KiB pieces, split over two files: piece 0 straddles the file edge.
//!     let layout = Arc::new(FileLayout::new(vec![
//!         FileSpec::new("a.bin", 0, 8_192),
//!         FileSpec::new("b.bin", 8_192, 8_192),
//!     ])?);
//!     let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());
//!     let root = std::env::temp_dir().join("oxide-flux-doc-example");
//!
//!     let (writer, mut events) = DiskWriter::start(
//!         WriterConfig::new(16_384),
//!         Arc::clone(&layout),
//!         Arc::clone(&backend),
//!         root.clone(),
//!     )?;
//!     writer.write_block(Block::new(0, 0, vec![0xAB; 16_384])).await?;
//!     writer.sync_all().await?;
//!     writer.shutdown().await?;
//!     while let Some(event) = events.recv().await {
//!         assert!(event.result.is_ok());
//!     }
//!
//!     // Read the piece back and hash it without spinning up the pool.
//!     let files = FileSet::open(layout, backend, &root)?;
//!     let mut piece = vec![0_u8; 16_384];
//!     let outcome = verify_piece(&files, 16_384, 0, [0_u8; 20], &mut piece);
//!     assert!(matches!(outcome, PieceOutcome::Corrupt { .. }));
//!
//!     // Or hand a whole queue of pieces to the bounded hashing pool.
//!     let (verifier, mut results) = Verifier::start(
//!         VerifyConfig::new(),
//!         Arc::new(FileLayout::new(vec![FileSpec::new("c.bin", 0, 16_384)])?),
//!         Arc::new(StdBackend::default()),
//!         root,
//!         16_384,
//!         vec![[0_u8; 20]],
//!     )?;
//!     verifier.verify(0).await?;
//!     let _first = results.recv().await;
//!     verifier.shutdown().await?;
//!     # Ok(())
//! }
//! ```

use std::io;
use std::path::PathBuf;

pub mod backend;
pub mod layout;
pub mod verify;
pub mod writer;

pub use backend::{Allocation, BlockBackend, FileHandle, FileSet, StdBackend};
pub use layout::{FileLayout, FileSpec, LayoutOptions, Segment};
pub use verify::{PieceHash, PieceOutcome, Verifier, VerifyConfig, VerifyEvent, verify_piece};
pub use writer::{Block, BlockRejected, DiskWriter, SubmitError, WriteEvent, WriterConfig};

/// Everything that can go wrong below the peer layer.
///
/// I/O failures carry the torrent-relative path of the file that failed, because
/// "the disk is full" is only actionable when you know which file it was.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DiskError {
    /// A filesystem operation failed.
    #[error("i/o error on `{}`: {}", .path.display(), .source)]
    Io {
        /// Torrent-relative path of the file.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The layout has no files, or only zero-length files.
    #[error("torrent layout is empty")]
    EmptyLayout,
    /// `offset + length` does not fit in a `u64`.
    #[error("file range at {offset} with length {length} overflows u64")]
    LengthOverflow {
        /// Declared start offset.
        offset: u64,
        /// Declared length.
        length: u64,
    },
    /// Two files claim the same torrent bytes.
    #[error("files {first} and {second} overlap at byte {offset}")]
    Overlap {
        /// Index of the first file.
        first: usize,
        /// Index of the second file.
        second: usize,
        /// Torrent offset where the overlap starts.
        offset: u64,
    },
    /// The layout skips bytes that no file owns.
    #[error("layout has a {gap}-byte gap before file {index}")]
    Gap {
        /// Index of the file after the hole.
        index: usize,
        /// Size of the hole.
        gap: u64,
    },
    /// A range lies outside the torrent, or crosses a hole in a gapped layout.
    #[error("range {pos}..{end} is outside the torrent length {total}")]
    OutOfRange {
        /// Start of the requested range.
        pos: u64,
        /// End of the requested range.
        end: u64,
        /// Torrent length.
        total: u64,
    },
    /// A range crosses a hole in a layout built with `allow_gaps`.
    #[error("range {pos}..{end} crosses a hole in the layout")]
    Hole {
        /// Start of the requested range.
        pos: u64,
        /// End of the requested range.
        end: u64,
    },
    /// The piece index does not exist.
    #[error("piece {piece} does not fit in {pieces} pieces of {piece_length} bytes")]
    PieceOutOfRange {
        /// Requested piece.
        piece: u32,
        /// Number of pieces in the torrent.
        pieces: u64,
        /// Piece length.
        piece_length: u32,
    },
    /// Piece length must be non-zero.
    #[error("piece length must be non-zero")]
    ZeroPieceLength,
    /// A range or a segment does not fit in `usize`.
    #[error("range of {len} bytes cannot be addressed in memory")]
    RangeTooLarge {
        /// Requested length.
        len: u64,
    },
    /// A block claims bytes outside its piece.
    #[error(
        "block for piece {piece} at {begin} with {len} bytes exceeds piece length {piece_length}"
    )]
    BlockOutOfBounds {
        /// Piece index.
        piece: u32,
        /// Offset inside the piece.
        begin: u32,
        /// Payload length.
        len: usize,
        /// Length of the piece.
        piece_length: u32,
    },
    /// An empty block cannot be stored.
    #[error("block for piece {piece} at {begin} is empty")]
    EmptyBlock {
        /// Piece index.
        piece: u32,
        /// Offset inside the piece.
        begin: u32,
    },
    /// The same piece bytes were buffered twice, which would fake completeness.
    #[error("block for piece {piece} at {begin} overlaps an already buffered block")]
    BlockOverlap {
        /// Piece index.
        piece: u32,
        /// Offset inside the piece.
        begin: u32,
    },
    /// The hash count does not match the layout.
    #[error("expected hashes for {expected} pieces, got {actual}")]
    HashCountMismatch {
        /// Pieces implied by the layout.
        expected: usize,
        /// Hashes supplied by the caller.
        actual: usize,
    },
    /// A caller-provided buffer is too small for the requested range.
    #[error("buffer of {available} bytes is too small for {needed} bytes")]
    BufferTooSmall {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        available: u64,
    },
    /// A configuration value would make backpressure meaningless.
    #[error("configuration value `{name}` must be greater than zero")]
    InvalidConfig {
        /// Name of the offending field.
        name: &'static str,
    },
    /// A path could escape the download root.
    #[error("refusing unsafe relative path `{}`", .path.display())]
    UnsafePath {
        /// The rejected path.
        path: PathBuf,
    },
    /// A backend returned a handle it did not create.
    #[error("backend returned an unexpected file handle")]
    HandleType,
    /// The queue is closed.
    #[error("disk worker has shut down")]
    Closed,
    /// A worker thread disappeared without reporting a result.
    #[error("disk worker thread panicked or was cancelled")]
    WorkerPanicked,
    /// A worker thread could not be spawned.
    #[error("failed to spawn {role} thread: {source}")]
    Spawn {
        /// Which worker failed.
        role: &'static str,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
}

impl DiskError {
    pub(crate) fn io(path: &std::path::Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn spawn(role: &'static str, source: io::Error) -> Self {
        Self::Spawn { role, source }
    }
}
