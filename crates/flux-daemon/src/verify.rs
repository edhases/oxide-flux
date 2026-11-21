//! Turning metainfo into something the disk layer can act on, then checking it.
//!
//! # The two layouts
//!
//! [`flux_core::metainfo::FileLayout`] is the authority on torrent semantics. It
//! is **piece-aligned** for v2 torrents: every non-empty file starts on a piece
//! boundary and the address space therefore contains alignment gaps that no file
//! owns.
//!
//! [`flux_disk::FileLayout`] is a byte-mapping table that refuses gaps unless
//! [`flux_disk::LayoutOptions::allow_gaps`] is set, because a silent gap is how a
//! writer corrupts the next file.
//!
//! [`plan`] bridges the two by copying core's `offset`/`length` for every file
//! verbatim into [`flux_disk::FileSpec`] and enabling gaps exactly when core's
//! layout is aligned (v2). Torrent-space offsets are then identical on both
//! sides, and `piece_size`/`piece_range` agree for every piece. Hybrid torrents
//! parse to the v1 contiguous layout because v1 data is present, so they take the
//! gap-free path.
//!
//! # Paths
//!
//! Every path component goes through [`flux_core::path::sanitize`] before it is
//! joined with the download root, and after the files exist their canonical
//! parents are checked against the canonical root. Metadata never gets to name a
//! path outside the download directory.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flux_core::metainfo::{self, Metainfo};
use flux_core::path;
use flux_disk::{
    Allocation, BlockBackend, DiskError, DiskWriter, FileLayout as DiskLayout, FileSpec,
    LayoutOptions, PieceHash, PieceOutcome, StdBackend, SubmitError, Verifier, VerifyConfig,
    WriterConfig,
};
use tokio::sync::mpsc;

use crate::id::{IdError, TorrentId};
use crate::pieces::{MAX_PIECES, PieceBitfield};

/// One file of a torrent, resolved to a safe path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// Index into the torrent's file list.
    pub index: usize,
    /// Sanitized path relative to the download root, including the torrent name.
    pub relative: PathBuf,
    /// Torrent-space offset, identical in the core and disk layouts.
    pub offset: u64,
    /// Declared length in bytes.
    pub length: u64,
}

/// A torrent that has been parsed, path-checked and mapped onto the disk layer.
#[derive(Debug)]
pub struct TorrentPlan {
    metainfo: Metainfo,
    id: TorrentId,
    files: Vec<PlannedFile>,
    disk_layout: Arc<DiskLayout>,
    v1_hashes: Option<Vec<PieceHash>>,
    raw: Vec<u8>,
}

impl TorrentPlan {
    /// The parsed metainfo.
    pub fn metainfo(&self) -> &Metainfo {
        &self.metainfo
    }

    /// The original `.torrent` bytes, for persistence and re-announces.
    ///
    /// Kept verbatim: the info hash is defined over these bytes, so they must
    /// never be re-encoded.
    pub fn raw_metainfo(&self) -> &[u8] {
        &self.raw
    }

    /// The torrent identity (v1 hash preferred, v2 otherwise).
    pub fn id(&self) -> &TorrentId {
        &self.id
    }

    /// Display name.
    pub fn name(&self) -> &str {
        &self.metainfo.info.name
    }

    /// Whether the torrent is `private: 1`.
    ///
    /// Surfaced so the future DHT/PEX/LSD layer can hard-disable itself for this
    /// torrent; leaking a private torrent's peers gets the user banned.
    pub fn private(&self) -> bool {
        self.metainfo.info.private
    }

    /// Piece length in bytes.
    pub fn piece_length(&self) -> u32 {
        self.metainfo.info.piece_length
    }

    /// Number of pieces.
    pub fn piece_count(&self) -> usize {
        self.metainfo.info.layout.piece_count()
    }

    /// Address-space length, including v2 alignment gaps.
    pub fn total_length(&self) -> u64 {
        self.metainfo.info.layout.total_length()
    }

    /// Sum of the file lengths, excluding alignment gaps.
    pub fn payload_length(&self) -> u64 {
        self.metainfo.info.layout.payload_length()
    }

    /// The authoritative torrent layout from `flux-core`.
    pub fn layout(&self) -> &flux_core::metainfo::FileLayout {
        &self.metainfo.info.layout
    }

    /// Files with sanitized download-relative paths.
    pub fn files(&self) -> &[PlannedFile] {
        &self.files
    }

    /// The same file set expressed for `flux-disk`.
    pub fn disk_layout(&self) -> Arc<DiskLayout> {
        Arc::clone(&self.disk_layout)
    }

    /// v1 SHA-1 piece hashes, absent for v2-only torrents.
    pub fn piece_hashes(&self) -> Option<&[PieceHash]> {
        self.v1_hashes.as_deref()
    }

    /// Whether the storage layer can verify this torrent today.
    ///
    /// Only v1 SHA-1 piece hashes are supported. A v2-only torrent can be
    /// preallocated and prioritised, but its Merkle hashes are not checked yet, so
    /// its data must not be reported as verified.
    pub fn verification_supported(&self) -> bool {
        self.v1_hashes.is_some()
    }
}

/// Options for [`prepare`].
#[derive(Debug, Clone)]
pub struct PrepareOptions {
    /// Download root. Created by the caller.
    pub root: PathBuf,
    /// How files are prepared.
    pub allocation: Allocation,
}

impl PrepareOptions {
    /// Options for `root` with sparse preallocation.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            allocation: Allocation::Sparse,
        }
    }
}

/// Parses metainfo and resolves it against the filesystem rules of this crate.
///
/// Pure: no directories are created and no bytes are written. Use [`prepare`] to
/// also create and preallocate the files.
///
/// # Errors
///
/// Returns [`AddError`] for malformed metainfo, an unusable torrent name, an
/// unsafe path, an excessive piece count, or a core/disk layout disagreement.
pub fn plan(bytes: &[u8]) -> Result<TorrentPlan, AddError> {
    let metainfo = metainfo::parse(bytes)?;
    let id = TorrentId::from_info(&metainfo.info)?;
    let info = &metainfo.info;
    let layout = &info.layout;

    if layout.piece_count() > MAX_PIECES {
        return Err(AddError::TooManyPieces {
            pieces: layout.piece_count(),
            limit: MAX_PIECES,
        });
    }

    // The torrent name becomes a directory (or, for a single-file torrent, the
    // file itself), so it must be a legal component before anything is joined.
    let name = path::sanitize_component(info.name.as_bytes(), 0).map_err(|source| {
        AddError::UnsafeName {
            name: info.name.clone(),
            source,
        }
    })?;

    // v1 paths already start with the torrent name (core prepends it while
    // parsing). v2 file trees are rooted at the name instead, except for a
    // single-file v2 torrent whose only tree key *is* the name.
    let v2_tree = info.pieces.is_none();
    let single_named_file = layout.files().len() == 1
        && layout.files()[0].path.len() == 1
        && layout.files()[0].path[0] == info.name;
    let prefix_name = v2_tree && !single_named_file;

    let mut files = Vec::with_capacity(layout.files().len());
    let mut specs = Vec::with_capacity(layout.files().len());
    for (index, entry) in layout.files().iter().enumerate() {
        let mut components: Vec<&[u8]> = Vec::with_capacity(entry.path.len() + 1);
        if prefix_name {
            components.push(name.as_bytes());
        }
        components.extend(entry.path.iter().map(|part| part.as_bytes()));
        let relative = path::sanitize(&components).map_err(|source| AddError::UnsafePath {
            path: entry.path.join("/"),
            source,
        })?;
        specs.push(FileSpec::new(relative.clone(), entry.offset, entry.length));
        files.push(PlannedFile {
            index,
            relative,
            offset: entry.offset,
            length: entry.length,
        });
    }

    // v1 is contiguous and must stay gap-free; a v2 layout is piece-aligned and
    // its gaps are intentional. Gaps are never tolerated by accident.
    let disk_layout = DiskLayout::with_options(
        specs,
        LayoutOptions {
            allow_gaps: layout.is_aligned(),
        },
    )?;
    check_layouts_agree(layout, &disk_layout)?;

    Ok(TorrentPlan {
        v1_hashes: info.pieces.clone(),
        metainfo,
        id,
        files,
        disk_layout: Arc::new(disk_layout),
        raw: bytes.to_vec(),
    })
}

/// Cross-checks the disk boundary table against core's piece address space.
fn check_layouts_agree(
    core: &flux_core::metainfo::FileLayout,
    disk: &DiskLayout,
) -> Result<(), AddError> {
    if core.total_length() != disk.total_length() {
        return Err(AddError::LayoutMismatch {
            detail: format!(
                "core address space is {} bytes, disk layout is {} bytes",
                core.total_length(),
                disk.total_length()
            ),
        });
    }
    if core.piece_count() != usize::try_from(disk.piece_count(core.piece_length())?).unwrap_or(0) {
        return Err(AddError::LayoutMismatch {
            detail: "piece counts disagree".to_owned(),
        });
    }
    let piece_length = u64::from(core.piece_length());
    for (index, spec) in disk.files().iter().enumerate() {
        let Some(entry) = core.files().get(index) else {
            return Err(AddError::LayoutMismatch {
                detail: format!("disk layout has an extra file at index {index}"),
            });
        };
        if spec.offset != entry.offset || spec.length != entry.length {
            return Err(AddError::LayoutMismatch {
                detail: format!(
                    "file {index} is at {}..{} on disk but {}..{} in core",
                    spec.offset,
                    spec.offset.saturating_add(spec.length),
                    entry.offset,
                    entry.offset.saturating_add(entry.length)
                ),
            });
        }
        // A piece-aligned layout whose file offsets are not piece-aligned would
        // silently shift every piece boundary relative to core's mapping.
        if core.is_aligned() && spec.length > 0 && spec.offset % piece_length != 0 {
            return Err(AddError::LayoutMismatch {
                detail: format!(
                    "v2 file {index} starts at byte {} which is not a piece boundary",
                    spec.offset
                ),
            });
        }
    }
    Ok(())
}

/// Parses, creates/preallocates the files and proves they stay under the root.
///
/// # Errors
///
/// Returns [`AddError`] for anything that would make the torrent unusable: bad
/// metadata, an unsafe path, a failed allocation, or a path whose canonical
/// location escapes the download root.
pub async fn prepare(bytes: Vec<u8>, options: PrepareOptions) -> Result<TorrentPlan, AddError> {
    let torrent = tokio::task::spawn_blocking(move || plan(&bytes)).await??;
    preallocate(&torrent, &options).await?;
    confirm_under_root(&torrent, options.root).await?;
    Ok(torrent)
}

/// Creates the torrent's files at their declared lengths through `StdBackend`.
///
/// The disk writer owns the only code path that opens torrent files, so it is
/// reused with no blocks queued: opening applies the allocation mode, and
/// `shutdown` flushes and fsyncs without inventing progress.
///
/// # Errors
///
/// Returns [`AddError::Disk`] if a file cannot be created or grown.
pub async fn preallocate(plan: &TorrentPlan, options: &PrepareOptions) -> Result<(), AddError> {
    let layout = plan.disk_layout();
    let root = options.root.clone();
    let piece_length = plan.piece_length();
    let allocation = options.allocation;

    // Reject escaping links *before* any file is opened or resized. The writer
    // opens files with normal semantics, so a junction planted in the tree would
    // otherwise be grown before `confirm_under_root` noticed it.
    let files: Vec<PathBuf> = plan
        .files()
        .iter()
        .map(|file| file.relative.clone())
        .collect();
    let paths_root = options.root.clone();
    tokio::task::spawn_blocking(move || prepare_paths(&paths_root, &files)).await??;

    let (writer, mut events) = tokio::task::spawn_blocking(move || {
        let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());
        let config = WriterConfig {
            allocation,
            flush_interval: None,
            ..WriterConfig::new(piece_length)
        };
        DiskWriter::start(config, layout, backend, root)
    })
    .await??;
    writer.shutdown().await?;
    // No blocks were ever queued, so only worker failures can appear here. Drain
    // instead of leaving the bounded channel to fill.
    while let Ok(event) = events.try_recv() {
        if let Err(error) = event.result {
            return Err(AddError::Disk(error));
        }
    }
    Ok(())
}

/// Creates parent directories only after proving every existing component is a
/// real directory inside the canonical root.
fn prepare_paths(root: &Path, files: &[PathBuf]) -> Result<(), AddError> {
    std::fs::create_dir_all(root).map_err(|source| AddError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let canonical_root = std::fs::canonicalize(root).map_err(|source| AddError::Io {
        path: root.to_path_buf(),
        source,
    })?;

    for relative in files {
        let components: Vec<_> = relative.components().collect();
        let mut current = root.to_path_buf();
        for component in &components[..components.len().saturating_sub(1)] {
            current.push(component.as_os_str());
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if is_link(&metadata) {
                        return Err(AddError::EscapedPath {
                            path: relative.display().to_string(),
                        });
                    }
                    let canonical =
                        std::fs::canonicalize(&current).map_err(|source| AddError::Io {
                            path: relative.clone(),
                            source,
                        })?;
                    if !canonical.starts_with(&canonical_root) {
                        return Err(AddError::EscapedPath {
                            path: relative.display().to_string(),
                        });
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    std::fs::create_dir(&current).map_err(|source| AddError::Io {
                        path: relative.clone(),
                        source,
                    })?;
                }
                Err(source) => {
                    return Err(AddError::Io {
                        path: relative.clone(),
                        source,
                    });
                }
            }
        }
        let full = root.join(relative);
        match std::fs::symlink_metadata(&full) {
            Ok(metadata) => {
                if is_link(&metadata) {
                    return Err(AddError::EscapedPath {
                        path: relative.display().to_string(),
                    });
                }
                let canonical = std::fs::canonicalize(&full).map_err(|source| AddError::Io {
                    path: relative.clone(),
                    source,
                })?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(AddError::EscapedPath {
                        path: relative.display().to_string(),
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(AddError::Io {
                    path: relative.clone(),
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Symfony links, junctions and other reparse points are all rejected: a torrent
/// may only write real files under its root.
#[cfg(not(windows))]
fn is_link(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Confirms every created file canonically resolves under the download root.
///
/// Sanitization already forbids `..` and separators, so this only catches what a
/// lexical check cannot: symlinks and junctions inside the tree.
async fn confirm_under_root(plan: &TorrentPlan, root: PathBuf) -> Result<(), AddError> {
    let files: Vec<PathBuf> = plan.files().iter().map(|f| f.relative.clone()).collect();
    tokio::task::spawn_blocking(move || {
        let canonical_root = std::fs::canonicalize(&root).map_err(|source| AddError::Io {
            path: root.clone(),
            source,
        })?;
        for relative in files {
            let full = root.join(&relative);
            let canonical = std::fs::canonicalize(&full).map_err(|source| AddError::Io {
                path: relative.clone(),
                source,
            })?;
            if !canonical.starts_with(&canonical_root) {
                return Err(AddError::EscapedPath {
                    path: relative.display().to_string(),
                });
            }
        }
        Ok(())
    })
    .await?
}

/// Deletes a torrent's files and the directories that become empty.
///
/// Blocking. Only paths that were already sanitized into `plan` are touched, and
/// empty-directory removal stops at the download root.
///
/// # Errors
///
/// Returns [`AddError::Io`] for the first file that exists but cannot be removed.
pub fn delete_files(plan: &TorrentPlan, root: &Path) -> Result<(), AddError> {
    let canonical_root = match std::fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(AddError::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };

    for file in plan.files() {
        let full = root.join(&file.relative);
        match std::fs::symlink_metadata(&full) {
            Ok(metadata) => {
                if is_link(&metadata) {
                    return Err(AddError::EscapedPath {
                        path: file.relative.display().to_string(),
                    });
                }
                let canonical = std::fs::canonicalize(&full).map_err(|source| AddError::Io {
                    path: file.relative.clone(),
                    source,
                })?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(AddError::EscapedPath {
                        path: file.relative.display().to_string(),
                    });
                }
                std::fs::remove_file(&full).map_err(|source| AddError::Io {
                    path: file.relative.clone(),
                    source,
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(AddError::Io {
                    path: file.relative.clone(),
                    source,
                });
            }
        }

        // Walk up while directories are real and empty, never above the torrent
        // name and never through a junction or symlink.
        let Some(first) = file.relative.components().next() else {
            continue;
        };
        let floor = root.join(first.as_os_str());
        let mut parent = full.parent().map(Path::to_path_buf);
        while let Some(directory) = parent {
            if !directory.starts_with(&floor) {
                break;
            }
            let removable = std::fs::symlink_metadata(&directory)
                .map(|metadata| metadata.is_dir() && !is_link(&metadata))
                .unwrap_or(false);
            if !removable || std::fs::remove_dir(&directory).is_err() {
                break;
            }
            parent = directory.parent().map(Path::to_path_buf);
        }
    }
    Ok(())
}

/// One piece verification job.
#[derive(Debug, Clone)]
pub struct CheckJob {
    /// Disk boundary table for the torrent.
    pub layout: Arc<DiskLayout>,
    /// Download root.
    pub root: PathBuf,
    /// Piece length.
    pub piece_length: u32,
    /// Expected v1 piece hashes, one per piece.
    pub hashes: Vec<PieceHash>,
    /// Hashing threads.
    pub threads: usize,
}

/// How one piece fared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Stored bytes match the expected hash.
    Good,
    /// Stored bytes were readable but hashed differently.
    Corrupt,
    /// The bytes could not be read at all.
    Unreadable,
}

/// A single piece result, as streamed to an observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckUpdate {
    /// Piece index.
    pub piece: u32,
    /// Result for that piece.
    pub outcome: CheckOutcome,
}

/// Aggregate result of a verification pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// Pieces the layout implied.
    pub pieces_total: usize,
    /// Pieces that matched their hash.
    pub have: usize,
    /// Pieces that were readable but wrong.
    pub corrupt: usize,
    /// Pieces that could not be read.
    pub unreadable: usize,
    /// First read error, if any.
    pub first_error: Option<String>,
}

impl VerifyReport {
    /// Pieces that were not verified.
    ///
    /// This is `pieces_total - have`, so it includes pieces that were readable
    /// but wrong (`corrupt`) and pieces that could not be read (`unreadable`).
    /// The three counts partition the pass exactly once.
    pub fn missing(&self) -> usize {
        self.pieces_total.saturating_sub(self.have)
    }

    /// Whether every piece matched.
    pub fn is_clean(&self) -> bool {
        self.pieces_total > 0 && self.have == self.pieces_total
    }
}

/// A completed verification pass: the report plus the verified-piece set.
#[derive(Debug)]
pub struct CheckRun {
    /// Aggregate counts.
    pub report: VerifyReport,
    /// Exactly the pieces that hashed correctly.
    pub have: PieceBitfield,
    /// Pieces that were readable but wrong, so the caller can re-download them.
    pub corrupt: PieceBitfield,
}

/// Hashes every piece of a torrent through the bounded verifier pool.
///
/// The pool and its queue are bounded, so a slow observer applies backpressure
/// instead of letting results pile up in memory. Every queued piece produces
/// exactly one result.
///
/// # Errors
///
/// Returns [`DiskError`] if the pool cannot be started or a worker disappears.
pub async fn run_check(
    job: CheckJob,
    sink: Option<&mpsc::Sender<CheckUpdate>>,
) -> Result<CheckRun, DiskError> {
    let total = job.hashes.len();
    if job.threads == 0 {
        return Err(DiskError::InvalidConfig { name: "threads" });
    }
    let piece_length = job.piece_length;
    let config = VerifyConfig {
        threads: job.threads,
        ..VerifyConfig::new()
    };
    let backend: Arc<dyn BlockBackend> = Arc::new(StdBackend::default());
    let (verifier, mut results) = tokio::task::spawn_blocking(move || {
        Verifier::start(
            config,
            job.layout,
            backend,
            job.root,
            piece_length,
            job.hashes,
        )
    })
    .await
    .map_err(|_| DiskError::WorkerPanicked)??;

    let mut report = VerifyReport {
        pieces_total: total,
        ..VerifyReport::default()
    };
    let mut have = PieceBitfield::new(total);
    let mut corrupt = PieceBitfield::new(total);
    let mut submitted = 0_usize;
    let mut done = 0_usize;
    let mut cancelled = false;

    while done < total {
        // Fill the bounded queue, then wait for one result. Interleaving the two
        // is what keeps a full result channel from blocking the workers.
        while submitted < total {
            let Ok(piece) = u32::try_from(submitted) else {
                break;
            };
            match verifier.try_verify(piece) {
                Ok(()) => submitted += 1,
                Err(SubmitError::Full) => break,
                Err(SubmitError::Closed) => {
                    cancelled = true;
                    break;
                }
            }
        }

        let Some(event) = results.recv().await else {
            cancelled = true;
            break;
        };
        done += 1;
        let outcome = match event.outcome {
            PieceOutcome::Good => {
                report.have += 1;
                have.insert(event.piece as usize);
                CheckOutcome::Good
            }
            PieceOutcome::Corrupt { .. } => {
                report.corrupt += 1;
                corrupt.insert(event.piece as usize);
                CheckOutcome::Corrupt
            }
            PieceOutcome::Failed(error) => {
                report.unreadable += 1;
                if report.first_error.is_none() {
                    report.first_error = Some(error.to_string());
                }
                CheckOutcome::Unreadable
            }
            PieceOutcome::Cancelled => {
                cancelled = true;
                break;
            }
        };
        if let Some(sink) = sink
            && sink
                .send(CheckUpdate {
                    piece: event.piece,
                    outcome,
                })
                .await
                .is_err()
        {
            // The observer is gone; finish the pass for the report instead of
            // abandoning queued work.
            cancelled = true;
            break;
        }
    }

    verifier.shutdown().await?;
    if cancelled {
        return Err(DiskError::Closed);
    }
    Ok(CheckRun {
        report,
        have,
        corrupt,
    })
}

/// A torrent cannot be added.
#[derive(Debug, thiserror::Error)]
pub enum AddError {
    /// The metainfo was malformed.
    #[error("invalid torrent: {0}")]
    Metainfo(#[from] metainfo::Error),
    /// The info hashes were unusable.
    #[error(transparent)]
    Id(#[from] IdError),
    /// The torrent name was not a legal path component.
    #[error("unsafe torrent name `{name}`: {source}")]
    UnsafeName {
        /// The rejected name.
        name: String,
        /// Why it was rejected.
        #[source]
        source: path::Error,
    },
    /// A metadata path was not a legal download-relative path.
    #[error("unsafe file path `{path}`: {source}")]
    UnsafePath {
        /// The rejected path, joined for display.
        path: String,
        /// Why it was rejected.
        #[source]
        source: path::Error,
    },
    /// A sanitized path still resolved outside the download root.
    #[error("file path `{path}` escapes the download root")]
    EscapedPath {
        /// The offending path.
        path: String,
    },
    /// The torrent declares more pieces than the engine will track.
    #[error("torrent declares {pieces} pieces, over the {limit} piece limit")]
    TooManyPieces {
        /// Declared piece count.
        pieces: usize,
        /// The limit.
        limit: usize,
    },
    /// The core and disk views of the torrent disagreed.
    #[error("inconsistent torrent layout: {detail}")]
    LayoutMismatch {
        /// What disagreed.
        detail: String,
    },
    /// A filesystem operation failed.
    #[error("i/o error on `{}`: {source}", .path.display())]
    Io {
        /// Torrent-relative path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The storage layer rejected the layout or failed to write.
    #[error(transparent)]
    Disk(#[from] DiskError),
    /// A blocking worker was cancelled or panicked.
    #[error("storage task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use flux_core::bencode::{self, Value};

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

    fn v1_multi(
        files: Vec<(&str, i64)>,
        piece_length: i64,
        pieces: usize,
        private: i64,
    ) -> Vec<u8> {
        let entries: Vec<Value> = files
            .into_iter()
            .map(|(path, length)| {
                dict(vec![
                    ("length", Value::Int(length)),
                    ("path", Value::List(vec![text(path)])),
                ])
            })
            .collect();
        let info = dict(vec![
            ("files", Value::List(entries)),
            ("name", text("root")),
            ("piece length", Value::Int(piece_length)),
            ("pieces", Value::Bytes(vec![0; pieces * 20])),
            ("private", Value::Int(private)),
        ]);
        bencode::encode(&dict(vec![("info", info)]))
    }

    #[test]
    fn bridges_a_v1_layout_without_gaps() {
        let bytes = v1_multi(vec![("a.bin", 3), ("b.bin", 5)], 4, 2, 0);
        let plan = plan(&bytes).unwrap();
        assert!(!plan.private());
        assert_eq!(plan.piece_count(), 2);
        assert_eq!(plan.total_length(), 8);
        assert_eq!(plan.disk_layout().total_length(), 8);
        assert_eq!(plan.disk_layout().piece_count(4).unwrap(), 2);
        let paths: Vec<&Path> = plan.files().iter().map(|f| f.relative.as_path()).collect();
        assert_eq!(
            paths,
            vec![Path::new("root/a.bin"), Path::new("root/b.bin")]
        );
        assert_eq!(plan.files()[1].offset, 3);
    }

    #[test]
    fn rejects_traversal_and_reserved_names() {
        for path in ["..", "CON", "/etc/passwd"] {
            let bytes = v1_multi(vec![(path, 4)], 4, 1, 0);
            let error = plan(&bytes).unwrap_err();
            assert!(
                matches!(
                    error,
                    AddError::UnsafePath { .. } | AddError::UnsafeName { .. }
                ),
                "{path} produced {error:?}"
            );
        }
    }

    #[test]
    fn traversal_in_a_later_component_is_rejected() {
        // `path` is a list in the real format; build one with two elements.
        let file = dict(vec![
            ("length", Value::Int(4)),
            (
                "path",
                Value::List(vec![text("sub"), text(".."), text("evil")]),
            ),
        ]);
        let info = dict(vec![
            ("files", Value::List(vec![file])),
            ("name", text("root")),
            ("piece length", Value::Int(4)),
            ("pieces", Value::Bytes(vec![0; 20])),
        ]);
        let bytes = bencode::encode(&dict(vec![("info", info)]));
        assert!(matches!(
            plan(&bytes).unwrap_err(),
            AddError::UnsafePath { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_rejected_before_any_write() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"outside").unwrap();
        std::fs::create_dir_all(root.path().join("movie")).unwrap();
        symlink(
            outside.path().join("secret"),
            root.path().join("movie/file.bin"),
        )
        .unwrap();

        let error = prepare_paths(root.path(), &[PathBuf::from("movie/file.bin")]).unwrap_err();
        assert!(matches!(error, AddError::EscapedPath { .. }));
        // The target keeps its bytes: nothing was opened or resized.
        assert_eq!(
            std::fs::read(outside.path().join("secret")).unwrap(),
            b"outside"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_directory_is_rejected_before_any_write() {
        use std::process::Command;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"outside").unwrap();
        let junction = root.path().join("movie");
        let created = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(outside.path())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !created {
            // Some CI images cannot create junctions; the Unix test covers the
            // same code path with a symlink.
            return;
        }

        let error = prepare_paths(root.path(), &[PathBuf::from("movie/file.bin")]).unwrap_err();
        assert!(matches!(error, AddError::EscapedPath { .. }));
        assert_eq!(
            std::fs::read(outside.path().join("secret")).unwrap(),
            b"outside"
        );
    }
}
