//! The file boundary table: torrent offset → (file, file offset, length).
//!
//! A piece never lines up with a file boundary. `FileLayout` owns the mapping so
//! the read and write paths can treat the torrent as one linear address space while
//! the backend sees ordinary, file-local offsets.

use std::path::PathBuf;

use crate::DiskError;

/// One file of a torrent, positioned in the torrent address space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    /// Path relative to the download root. Must already be sanitized by the caller
    /// (no absolute paths, no `..`, no Windows reserved names).
    pub path: PathBuf,
    /// Start of the file in the torrent address space.
    pub offset: u64,
    /// Length of the file in bytes.
    pub length: u64,
}

impl FileSpec {
    /// Creates a file entry. `offset` is the torrent-space start, not a file-local
    /// offset.
    pub fn new(path: impl Into<PathBuf>, offset: u64, length: u64) -> Self {
        Self {
            path: path.into(),
            offset,
            length,
        }
    }
}

/// Validation knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutOptions {
    /// Accept holes in the address space. Off by default: a gap in a torrent is a
    /// bug in the caller's metadata handling, and silently writing past it would
    /// corrupt the next file.
    pub allow_gaps: bool,
}

/// One contiguous run of bytes inside a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Index into [`FileLayout::files`].
    pub file: usize,
    /// Offset inside that file.
    pub file_offset: u64,
    /// Number of bytes in this run. Fits in memory by construction.
    pub len: usize,
}

#[derive(Debug, Clone, Copy)]
struct Span {
    file: usize,
    offset: u64,
    end: u64,
}

/// Validated mapping from torrent offsets to files.
///
/// Zero-length files are accepted and skipped by the mapping table (they occupy no
/// bytes, so they can never be the target of a read or write). Files may be supplied
/// in any order; the boundary table is sorted by offset, while `files()` preserves
/// the caller's order so file indices stay stable.
#[derive(Debug, Clone)]
pub struct FileLayout {
    files: Vec<FileSpec>,
    spans: Vec<Span>,
    total_length: u64,
}

impl FileLayout {
    /// Builds a layout, rejecting overlaps, gaps, empty torrents and u64 overflow.
    pub fn new(files: Vec<FileSpec>) -> Result<Self, DiskError> {
        Self::with_options(files, LayoutOptions::default())
    }

    /// Builds a layout with explicit validation options.
    pub fn with_options(files: Vec<FileSpec>, options: LayoutOptions) -> Result<Self, DiskError> {
        if files.is_empty() {
            return Err(DiskError::EmptyLayout);
        }

        let mut spans = Vec::new();
        for (index, spec) in files.iter().enumerate() {
            let end = spec
                .offset
                .checked_add(spec.length)
                .ok_or(DiskError::LengthOverflow {
                    offset: spec.offset,
                    length: spec.length,
                })?;
            if spec.length > 0 {
                spans.push(Span {
                    file: index,
                    offset: spec.offset,
                    end,
                });
            }
        }

        if spans.is_empty() {
            return Err(DiskError::EmptyLayout);
        }

        spans.sort_by_key(|span| span.offset);

        let first = spans[0];
        if first.offset != 0 && !options.allow_gaps {
            return Err(DiskError::Gap {
                index: first.file,
                gap: first.offset,
            });
        }

        for window in spans.windows(2) {
            let [previous, current] = window else {
                continue;
            };
            if current.offset < previous.end {
                return Err(DiskError::Overlap {
                    first: previous.file,
                    second: current.file,
                    offset: current.offset,
                });
            }
            if current.offset > previous.end && !options.allow_gaps {
                return Err(DiskError::Gap {
                    index: current.file,
                    gap: current.offset - previous.end,
                });
            }
        }

        let total_length = spans.last().map_or(0, |span| span.end);
        Ok(Self {
            files,
            spans,
            total_length,
        })
    }

    /// The files exactly as supplied, so indices in [`Segment::file`] stay stable.
    pub fn files(&self) -> &[FileSpec] {
        &self.files
    }

    /// Declared length of the torrent, in bytes.
    pub fn total_length(&self) -> u64 {
        self.total_length
    }

    /// Number of pieces of `piece_length` bytes needed to cover the torrent.
    pub fn piece_count(&self, piece_length: u32) -> Result<u64, DiskError> {
        if piece_length == 0 {
            return Err(DiskError::ZeroPieceLength);
        }
        Ok(self.total_length.div_ceil(u64::from(piece_length)))
    }

    /// Offset and length of `index`, computed from the layout — never from metadata
    /// that could claim a piece is longer than the torrent.
    pub fn piece_range(&self, piece_length: u32, index: u32) -> Result<(u64, u32), DiskError> {
        let count = self.piece_count(piece_length)?;
        if u64::from(index) >= count {
            return Err(DiskError::PieceOutOfRange {
                piece: index,
                pieces: count,
                piece_length,
            });
        }
        let offset = u64::from(index) * u64::from(piece_length);
        let len = u64::from(piece_length).min(self.total_length - offset);
        let len = u32::try_from(len).map_err(|_| DiskError::RangeTooLarge { len })?;
        Ok((offset, len))
    }

    /// Splits `pos..pos + len` into per-file runs.
    ///
    /// Borrows nothing up front: the returned vector is empty for a zero-length
    /// range. A range that leaves the torrent, crosses a hole in a gapped layout, or
    /// is longer than memory can address is an error, never a partial result.
    pub fn map(&self, pos: u64, len: u64) -> Result<Vec<Segment>, DiskError> {
        let mut segments = Vec::new();
        self.map_into(pos, len, &mut segments)?;
        Ok(segments)
    }

    /// Same as [`Self::map`] but reuses `out`, which the writer and verifier do on
    /// every block to stay allocation-free on the hot path.
    pub fn map_into(&self, pos: u64, len: u64, out: &mut Vec<Segment>) -> Result<(), DiskError> {
        out.clear();
        let end = pos.checked_add(len).ok_or(DiskError::OutOfRange {
            pos,
            end: u64::MAX,
            total: self.total_length,
        })?;
        if end > self.total_length {
            return Err(DiskError::OutOfRange {
                pos,
                end,
                total: self.total_length,
            });
        }
        if len == 0 {
            return Ok(());
        }

        let mut cursor = pos;
        let mut span_index = self.spans.partition_point(|span| span.end <= cursor);
        while cursor < end {
            let span = self
                .spans
                .get(span_index)
                .ok_or(DiskError::Hole { pos: cursor, end })?;
            let chunk = (span.end - cursor).min(end - cursor);
            let chunk_len =
                usize::try_from(chunk).map_err(|_| DiskError::RangeTooLarge { len: chunk })?;
            out.push(Segment {
                file: span.file,
                file_offset: cursor - span.offset,
                len: chunk_len,
            });
            cursor += chunk;
            span_index += 1;
        }
        Ok(())
    }
}
