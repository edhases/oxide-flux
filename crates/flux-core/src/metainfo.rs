//! BitTorrent metainfo (`.torrent`) parsing for v1, v2 and hybrid torrents.
//!
//! Hashes are always computed over the *original* info dictionary bytes that
//! were found in the input, never over a re-encoded structure; [`bencode`]
//! records those byte spans for us.
//!
//! Parsing is strict. Any structural surprise that could make two clients
//! compute different hashes or different file layouts is an error. Paths are
//! kept raw here; they must be passed through [`crate::path::sanitize`] before
//! they are joined with a download root.

use std::collections::{BTreeMap, BTreeSet};

use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::bencode::{self, Limits, Value};

/// Size of a leaf block in a BEP 52 Merkle tree (16 KiB).
pub const V2_LEAF_SIZE: u32 = 16 * 1024;

/// A v1 (SHA-1) info hash.
pub type InfoHashV1 = [u8; 20];

/// A v2 (SHA-256) info hash.
pub type InfoHashV2 = [u8; 32];

/// The SHA-1 and/or SHA-256 hashes of a parsed info dictionary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hashes {
    /// SHA-1 hash, present for v1 and hybrid torrents.
    pub v1: Option<InfoHashV1>,
    /// SHA-256 hash, present for v2 and hybrid torrents.
    pub v2: Option<InfoHashV2>,
}

/// The metainfo variants a torrent can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentKind {
    /// BEP 3 torrent, identified by a SHA-1 info hash.
    V1,
    /// BEP 52 torrent, identified by a SHA-256 info hash.
    V2,
    /// Both formats, with the same content described twice.
    Hybrid,
}

/// A parsed `.torrent` file.
#[derive(Debug, Clone)]
pub struct Metainfo {
    /// Primary tracker URL, if any.
    pub announce: Option<String>,
    /// Tiered tracker URLs from `announce-list`.
    pub announce_list: Vec<Vec<String>>,
    /// Free-form comment from the creator.
    pub comment: Option<String>,
    /// Name of the program that created the torrent.
    pub created_by: Option<String>,
    /// Unix timestamp of creation.
    pub creation_date: Option<i64>,
    /// The validated info dictionary.
    pub info: Info,
}

impl Metainfo {
    /// SHA-1 info hash, present for v1 and hybrid torrents.
    pub fn info_hash(&self) -> Option<InfoHashV1> {
        self.info.hashes.v1
    }

    /// SHA-256 info hash, present for v2 and hybrid torrents.
    pub fn info_hash_v2(&self) -> Option<InfoHashV2> {
        self.info.hashes.v2
    }

    /// Whether the torrent is marked private (BEP 27).
    ///
    /// When this is `true`, DHT, PEX, LSD and cross-torrent peer sharing must
    /// be disabled by the caller for this torrent.
    pub fn is_private(&self) -> bool {
        self.info.private
    }

    /// The metainfo variant of this torrent.
    pub fn kind(&self) -> TorrentKind {
        match (self.info.hashes.v1.is_some(), self.info.hashes.v2.is_some()) {
            (true, true) => TorrentKind::Hybrid,
            (true, false) => TorrentKind::V1,
            (false, true) => TorrentKind::V2,
            // Parsed torrents always carry at least one hash.
            (false, false) => TorrentKind::V1,
        }
    }
}

/// The validated contents of a torrent's `info` dictionary.
#[derive(Debug, Clone)]
pub struct Info {
    /// Display name. For v1 multi-file torrents this is the root directory.
    pub name: String,
    /// Piece length in bytes. Always non-zero.
    pub piece_length: u32,
    /// BEP 27 private flag.
    pub private: bool,
    /// File list and piece address space.
    pub layout: FileLayout,
    /// v1 SHA-1 piece hashes, absent for v2-only torrents.
    pub pieces: Option<Vec<InfoHashV1>>,
    /// v2 piece layers keyed by each file's Merkle root.
    ///
    /// Empty when the torrent has no multi-piece files or omitted the field.
    /// Verifying that these hashes actually match file contents is the
    /// verifier's job.
    pub piece_layers: BTreeMap<InfoHashV2, Vec<InfoHashV2>>,
    /// Info dictionary hashes.
    pub hashes: Hashes,
}

/// One file in a torrent, in torrent address-space order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Path components, already resolved against the torrent name.
    ///
    /// Every component still has to be validated by [`crate::path::sanitize`].
    pub path: Vec<String>,
    /// Offset of the first byte in the piece address space.
    pub offset: u64,
    /// File length in bytes. May be zero.
    pub length: u64,
}

/// A contiguous run of a piece that falls inside one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PieceRange {
    /// Index into [`FileLayout::files`].
    pub file_index: usize,
    /// Offset inside that file.
    pub offset: u64,
    /// Number of bytes covered by the piece.
    pub length: u64,
}

/// File list plus the mapping from piece indices to files.
///
/// In v1 layouts files are packed back-to-back, so a piece can span several
/// files. In v2 layouts every non-empty file starts on a piece boundary, so a
/// piece maps to a single file and the address space contains alignment gaps.
/// Both cases are handled here; callers must not assume one piece equals one
/// file.
#[derive(Debug, Clone)]
pub struct FileLayout {
    files: Vec<FileEntry>,
    total_length: u64,
    payload_length: u64,
    piece_length: u32,
    piece_count: usize,
    aligned: bool,
}

impl FileLayout {
    /// Build a v1-style layout where files are packed without gaps.
    ///
    /// Offsets are assigned in iteration order. The total length is the sum of
    /// all file lengths.
    pub fn contiguous(
        files: impl IntoIterator<Item = (Vec<String>, u64)>,
        piece_length: u32,
    ) -> Result<Self, Error> {
        Self::build(files, piece_length, false)
    }

    /// Build a v2-style layout where every non-empty file starts on a piece boundary.
    ///
    /// Alignment gaps are part of the address space but not of
    /// [`FileLayout::payload_length`].
    pub fn aligned(
        files: impl IntoIterator<Item = (Vec<String>, u64)>,
        piece_length: u32,
    ) -> Result<Self, Error> {
        Self::build(files, piece_length, true)
    }

    fn build(
        files: impl IntoIterator<Item = (Vec<String>, u64)>,
        piece_length: u32,
        aligned: bool,
    ) -> Result<Self, Error> {
        if piece_length == 0 {
            return Err(Error::ZeroPieceLength);
        }
        let piece_length_u64 = u64::from(piece_length);
        let mut entries = Vec::new();
        let mut total_length = 0u64;
        let mut payload_length = 0u64;
        let mut aligned_pieces = 0u64;

        for (path, length) in files {
            if aligned && length > 0 {
                aligned_pieces = aligned_pieces
                    .checked_add(length.div_ceil(piece_length_u64))
                    .ok_or(Error::TotalLengthOverflow)?;
                total_length = total_length
                    .div_ceil(piece_length_u64)
                    .checked_mul(piece_length_u64)
                    .ok_or(Error::TotalLengthOverflow)?;
            }
            entries.push(FileEntry {
                path,
                offset: total_length,
                length,
            });
            total_length = total_length
                .checked_add(length)
                .ok_or(Error::TotalLengthOverflow)?;
            payload_length = payload_length
                .checked_add(length)
                .ok_or(Error::TotalLengthOverflow)?;
        }

        if entries.is_empty() {
            return Err(Error::EmptyFileList);
        }
        if payload_length == 0 {
            return Err(Error::EmptyTorrent);
        }

        // Two entries writing the same path would make the download root's
        // contents depend on scheduling. Reject rather than overwrite.
        let mut seen: BTreeSet<&[String]> = BTreeSet::new();
        for entry in &entries {
            if !seen.insert(entry.path.as_slice()) {
                return Err(Error::DuplicateFilePath {
                    path: entry.path.join("/"),
                });
            }
        }

        let piece_count = if aligned {
            aligned_pieces
        } else {
            total_length.div_ceil(piece_length_u64)
        };
        // Both encodings must describe the same number of pieces the rest of
        // the client derives from the address-space length.
        if piece_count != total_length.div_ceil(piece_length_u64) {
            return Err(Error::InvalidLayout);
        }
        let piece_count = usize::try_from(piece_count).map_err(|_| Error::TooManyPieces)?;

        Ok(Self {
            files: entries,
            total_length,
            payload_length,
            piece_length,
            piece_count,
            aligned,
        })
    }

    /// Files in torrent order.
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    /// Length of the piece address space, including v2 alignment gaps.
    pub fn total_length(&self) -> u64 {
        self.total_length
    }

    /// Sum of all file lengths, excluding address-space gaps.
    pub fn payload_length(&self) -> u64 {
        self.payload_length
    }

    /// Piece length in bytes.
    pub fn piece_length(&self) -> u32 {
        self.piece_length
    }

    /// Number of pieces.
    pub fn piece_count(&self) -> usize {
        self.piece_count
    }

    /// Whether files are aligned to piece boundaries (v2 layout).
    pub fn is_aligned(&self) -> bool {
        self.aligned
    }

    /// Size of `piece` in bytes, or `None` if the index is out of range.
    ///
    /// The final piece of a file can be shorter than the piece length. In a v1
    /// layout only the last piece of the torrent is short; in a v2 layout every
    /// file can end with a short piece.
    pub fn piece_size(&self, piece: usize) -> Option<u32> {
        if piece >= self.piece_count {
            return None;
        }
        let piece_length = u64::from(self.piece_length);
        let start = (piece as u64).checked_mul(piece_length)?;
        if self.aligned {
            let (_, file) = self.file_at_offset(start)?;
            let offset = start.checked_sub(file.offset)?;
            let remaining = file.length.checked_sub(offset)?;
            Some(remaining.min(piece_length) as u32)
        } else {
            let remaining = self.total_length.checked_sub(start)?;
            Some(remaining.min(piece_length) as u32)
        }
    }

    /// Map a piece to the file ranges it covers.
    ///
    /// Zero-length files never appear. In v1 layouts the returned vector has
    /// one entry per file the piece straddles; in v2 layouts it has one entry.
    /// Returns `None` if the piece index is out of range.
    pub fn map_piece(&self, piece: usize) -> Option<Vec<PieceRange>> {
        if piece >= self.piece_count {
            return None;
        }
        let piece_length = u64::from(self.piece_length);
        let start = (piece as u64).checked_mul(piece_length)?;

        if self.aligned {
            let (file_index, file) = self.file_at_offset(start)?;
            let offset = start.checked_sub(file.offset)?;
            let length = file.length.checked_sub(offset)?.min(piece_length);
            return Some(vec![PieceRange {
                file_index,
                offset,
                length,
            }]);
        }

        let end = start.saturating_add(piece_length).min(self.total_length);
        let mut ranges = Vec::new();
        let mut index = self
            .files
            .partition_point(|file| file.offset.saturating_add(file.length) <= start);
        let mut position = start;
        while position < end && index < self.files.len() {
            let file = &self.files[index];
            let file_end = file.offset.saturating_add(file.length);
            if file_end <= position {
                index += 1;
                continue;
            }
            let offset = position.saturating_sub(file.offset);
            let length = (file_end - position).min(end - position);
            ranges.push(PieceRange {
                file_index: index,
                offset,
                length,
            });
            position += length;
            index += 1;
        }
        Some(ranges)
    }

    /// Piece holding `offset`, or `None` for an out-of-range offset or a v2
    /// alignment gap.
    pub fn piece_index_at(&self, offset: u64) -> Option<usize> {
        if offset >= self.total_length {
            return None;
        }
        let piece_length = u64::from(self.piece_length);
        if self.aligned {
            let (_, file) = self.file_at_offset(offset)?;
            let first_piece = file.offset / piece_length;
            let within = (offset - file.offset) / piece_length;
            usize::try_from(first_piece.checked_add(within)?).ok()
        } else {
            usize::try_from(offset / piece_length).ok()
        }
    }

    /// Find the non-empty file containing `offset`.
    fn file_at_offset(&self, offset: u64) -> Option<(usize, &FileEntry)> {
        let mut index = self.files.partition_point(|file| file.offset <= offset);
        while index > 0 {
            index -= 1;
            let file = &self.files[index];
            if file.length == 0 {
                continue;
            }
            if offset >= file.offset && offset < file.offset.saturating_add(file.length) {
                return Some((index, file));
            }
            // Offsets are sorted; a non-empty file that ends before `offset`
            // means the offset falls in an alignment gap.
            return None;
        }
        None
    }
}

/// A metainfo parsing failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The underlying bencode was invalid.
    #[error("bencode: {0}")]
    Bencode(#[from] bencode::Error),
    /// The top-level value was not a dictionary.
    #[error("metainfo root is not a dictionary")]
    NotADictionary,
    /// A required key was missing.
    #[error("missing required key `{key}`")]
    MissingKey {
        /// Key name.
        key: &'static str,
    },
    /// A key had the wrong bencode type.
    #[error("key `{key}` must be {expected}")]
    WrongType {
        /// Key name.
        key: &'static str,
        /// Human-readable expected type.
        expected: &'static str,
    },
    /// A string field was not valid UTF-8.
    #[error("key `{key}` contains invalid UTF-8")]
    InvalidUtf8 {
        /// Key name.
        key: &'static str,
    },
    /// A string field was empty.
    #[error("key `{key}` is empty")]
    EmptyValue {
        /// Key name.
        key: &'static str,
    },
    /// A v2 torrent was missing `meta version`.
    #[error("`meta version` is required when `file tree` is present")]
    MissingMetaVersion,
    /// `meta version` named a version this parser does not support.
    #[error("`meta version` {version} is not supported")]
    UnsupportedMetaVersion {
        /// The version found.
        version: i64,
    },
    /// `piece length` was zero.
    #[error("`piece length` must be positive")]
    ZeroPieceLength,
    /// `piece length` did not fit in 32 bits.
    #[error("`piece length` {value} does not fit in 32 bits")]
    PieceLengthTooLarge {
        /// The value found.
        value: i64,
    },
    /// A v2 piece length was below 16 KiB.
    #[error("v2 `piece length` {value} is smaller than 16 KiB")]
    PieceLengthTooSmall {
        /// The value found.
        value: u32,
    },
    /// A v2 piece length was not a power of two.
    #[error("v2 `piece length` {value} is not a power of two")]
    PieceLengthNotPowerOfTwo {
        /// The value found.
        value: u32,
    },
    /// A file length was negative.
    #[error("file length must not be negative")]
    NegativeLength,
    /// File offsets or lengths overflowed `u64`.
    #[error("file lengths overflow")]
    TotalLengthOverflow,
    /// The layout would require more pieces than `usize` allows.
    #[error("too many pieces")]
    TooManyPieces,
    /// The file list was empty.
    #[error("torrent contains no files")]
    EmptyFileList,
    /// Two entries resolved to the same path.
    #[error("duplicate file path `{path}`")]
    DuplicateFilePath {
        /// The duplicated path.
        path: String,
    },
    /// All files had zero length.
    #[error("torrent contains no data")]
    EmptyTorrent,
    /// Neither `length` nor `files` was present.
    #[error("missing `length` or `files` in info dictionary")]
    MissingFileList,
    /// Both `length` and `files` were present.
    #[error("info dictionary contains both `length` and `files`")]
    ConflictingFileLists,
    /// A `files` entry had an empty `path` list.
    #[error("file path is empty")]
    EmptyFilePath,
    /// A v2 file tree contained an empty path element.
    #[error("file tree contains an empty path element")]
    EmptyPathElement,
    /// `pieces` was not a whole number of 20-byte hashes.
    #[error("`pieces` length {len} is not a multiple of 20")]
    InvalidPiecesLength {
        /// Observed byte length.
        len: usize,
    },
    /// The number of piece hashes disagreed with the layout.
    #[error("expected {expected} piece hashes but found {actual}")]
    PieceCountMismatch {
        /// Number implied by total length and piece length.
        expected: u64,
        /// Number actually present.
        actual: usize,
    },
    /// `private` was neither 0 nor 1.
    #[error("`private` must be 0 or 1, found {value}")]
    InvalidPrivateFlag {
        /// The value found.
        value: i64,
    },
    /// The file tree root described a file.
    #[error("file tree root must not be a file")]
    RootFileEntry,
    /// A file node had sibling entries.
    #[error("file tree file entry must not have sibling entries")]
    FileEntryWithSiblings,
    /// A `pieces root` was not 32 bytes.
    #[error("`pieces root` must be 32 bytes, found {len}")]
    InvalidPiecesRoot {
        /// Observed byte length.
        len: usize,
    },
    /// A non-empty v2 file had no `pieces root`.
    #[error("file `{path}` has no `pieces root`")]
    MissingPiecesRoot {
        /// File path.
        path: String,
    },
    /// A piece layer was not a whole number of 32-byte hashes.
    #[error("piece layer length {len} is not a multiple of 32")]
    InvalidPieceLayerLength {
        /// Observed byte length.
        len: usize,
    },
    /// A piece layer had the wrong number of hashes for its file.
    #[error("piece layer for `{path}` has {actual} hashes, expected {expected}")]
    PieceLayerCountMismatch {
        /// File path.
        path: String,
        /// Hashes implied by the file length.
        expected: u64,
        /// Hashes present in the layer.
        actual: u64,
    },
    /// A piece layer referenced a root that no file declared.
    #[error("piece layer refers to an unknown pieces root")]
    UnknownPieceLayer,
    /// Piece layers were present without a v2 file tree.
    #[error("`piece layers` present without a v2 `file tree`")]
    PieceLayersWithoutFileTree,
    /// The constructed layout was internally inconsistent.
    #[error("inconsistent file layout")]
    InvalidLayout,
    /// Neither v1 nor v2 torrent data was present.
    #[error("info dictionary contains no v1 or v2 torrent data")]
    MissingTorrentData,
}

/// Parse a torrent with the default bencode limits.
pub fn parse(input: &[u8]) -> Result<Metainfo, Error> {
    parse_with_limits(input, Limits::default())
}

/// Parse a torrent with explicit bencode limits.
pub fn parse_with_limits(input: &[u8], limits: Limits) -> Result<Metainfo, Error> {
    let decoded = bencode::decode_with_spans(input, &limits)?;
    let root = decoded.value.as_dict().ok_or(Error::NotADictionary)?;

    let info_value = dict_get(root, b"info").ok_or(Error::MissingKey { key: "info" })?;
    let info = info_value.as_dict().ok_or(Error::WrongType {
        key: "info",
        expected: "dictionary",
    })?;
    let info_span = decoded
        .span_of(b"info")
        .ok_or(Error::MissingKey { key: "info" })?;
    let raw_info = info_span.get(input);
    let hash_v1 = sha1_hash(raw_info);
    let hash_v2 = sha256_hash(raw_info);

    // BEP 52 requires implementations to reject a newer meta version before
    // performing other validations that might produce a more confusing error.
    let meta_version = match dict_get(info, b"meta version") {
        Some(value) => Some(int_field(value, "meta version")?),
        None => None,
    };
    if let Some(version) = meta_version
        && version != 2
    {
        return Err(Error::UnsupportedMetaVersion { version });
    }

    let announce = optional_string(root, "announce")?;
    let announce_list = match dict_get(root, b"announce-list") {
        Some(value) => parse_announce_list(value)?,
        None => Vec::new(),
    };
    let comment = optional_string(root, "comment")?;
    let created_by = optional_string(root, "created by")?;
    let creation_date = match dict_get(root, b"creation date") {
        Some(value) => Some(int_field(value, "creation date")?),
        None => None,
    };

    let name = required_string(info, "name")?;
    if name.is_empty() {
        return Err(Error::EmptyValue { key: "name" });
    }
    let piece_length = required_piece_length(info)?;
    let private = parse_private(info)?;

    let file_tree = dict_get(info, b"file tree");
    if file_tree.is_some() && meta_version != Some(2) {
        return Err(Error::MissingMetaVersion);
    }

    let v1 = match dict_get(info, b"pieces") {
        Some(value) => Some((
            parse_pieces(value)?,
            FileLayout::contiguous(parse_v1_files(info, &name)?, piece_length)?,
        )),
        None => {
            if dict_get(info, b"length").is_some() || dict_get(info, b"files").is_some() {
                return Err(Error::MissingKey { key: "pieces" });
            }
            None
        }
    };

    if let Some((pieces, layout)) = &v1 {
        let expected = layout.piece_count() as u64;
        if pieces.len() as u64 != expected {
            return Err(Error::PieceCountMismatch {
                expected,
                actual: pieces.len(),
            });
        }
    }

    let v2_files = match file_tree {
        Some(value) => {
            validate_v2_piece_length(piece_length)?;
            Some(parse_file_tree(value)?)
        }
        None => None,
    };

    if v1.is_none() && v2_files.is_none() {
        return Err(Error::MissingTorrentData);
    }

    let piece_layers = match dict_get(root, b"piece layers") {
        Some(value) => {
            let files = v2_files
                .as_deref()
                .ok_or(Error::PieceLayersWithoutFileTree)?;
            parse_piece_layers(value, files, piece_length)?
        }
        None => BTreeMap::new(),
    };

    let (pieces, layout, hashes) = match (v1, v2_files) {
        (Some((pieces, layout)), Some(_v2)) => (
            Some(pieces),
            layout,
            Hashes {
                v1: Some(hash_v1),
                v2: Some(hash_v2),
            },
        ),
        (Some((pieces, layout)), None) => (
            Some(pieces),
            layout,
            Hashes {
                v1: Some(hash_v1),
                v2: None,
            },
        ),
        (None, Some(files)) => {
            let layout = FileLayout::aligned(
                files.into_iter().map(|file| (file.path, file.length)),
                piece_length,
            )?;
            (
                None,
                layout,
                Hashes {
                    v1: None,
                    v2: Some(hash_v2),
                },
            )
        }
        (None, None) => return Err(Error::MissingTorrentData),
    };

    Ok(Metainfo {
        announce,
        announce_list,
        comment,
        created_by,
        creation_date,
        info: Info {
            name,
            piece_length,
            private,
            layout,
            pieces,
            piece_layers,
            hashes,
        },
    })
}

fn sha1_hash(bytes: &[u8]) -> InfoHashV1 {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let output = hasher.finalize();
    let mut hash = [0u8; 20];
    hash.copy_from_slice(output.as_slice());
    hash
}

fn sha256_hash(bytes: &[u8]) -> InfoHashV2 {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let output = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(output.as_slice());
    hash
}

fn dict_get<'a>(dict: &'a [(Vec<u8>, Value)], key: &[u8]) -> Option<&'a Value> {
    dict.iter()
        .find(|(candidate, _)| candidate.as_slice() == key)
        .map(|(_, value)| value)
}

fn optional_string(dict: &[(Vec<u8>, Value)], key: &'static str) -> Result<Option<String>, Error> {
    match dict_get(dict, key.as_bytes()) {
        Some(value) => Ok(Some(string_field(value, key)?)),
        None => Ok(None),
    }
}

fn required_string(dict: &[(Vec<u8>, Value)], key: &'static str) -> Result<String, Error> {
    let value = dict_get(dict, key.as_bytes()).ok_or(Error::MissingKey { key })?;
    string_field(value, key)
}

fn string_field(value: &Value, key: &'static str) -> Result<String, Error> {
    let bytes = value.as_bytes().ok_or(Error::WrongType {
        key,
        expected: "string",
    })?;
    utf8_field(bytes, key)
}

fn utf8_field(bytes: &[u8], key: &'static str) -> Result<String, Error> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| Error::InvalidUtf8 { key })
}

fn int_field(value: &Value, key: &'static str) -> Result<i64, Error> {
    value.as_int().ok_or(Error::WrongType {
        key,
        expected: "integer",
    })
}

fn non_negative_length(value: &Value, key: &'static str) -> Result<u64, Error> {
    let number = int_field(value, key)?;
    u64::try_from(number).map_err(|_| Error::NegativeLength)
}

fn required_piece_length(info: &[(Vec<u8>, Value)]) -> Result<u32, Error> {
    let value = dict_get(info, b"piece length").ok_or(Error::MissingKey {
        key: "piece length",
    })?;
    let number = int_field(value, "piece length")?;
    if number <= 0 {
        return Err(Error::ZeroPieceLength);
    }
    u32::try_from(number).map_err(|_| Error::PieceLengthTooLarge { value: number })
}

fn validate_v2_piece_length(piece_length: u32) -> Result<(), Error> {
    if piece_length < V2_LEAF_SIZE {
        return Err(Error::PieceLengthTooSmall {
            value: piece_length,
        });
    }
    if !piece_length.is_power_of_two() {
        return Err(Error::PieceLengthNotPowerOfTwo {
            value: piece_length,
        });
    }
    Ok(())
}

fn parse_private(info: &[(Vec<u8>, Value)]) -> Result<bool, Error> {
    match dict_get(info, b"private") {
        Some(value) => match int_field(value, "private")? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(Error::InvalidPrivateFlag { value }),
        },
        None => Ok(false),
    }
}

fn parse_pieces(value: &Value) -> Result<Vec<InfoHashV1>, Error> {
    let bytes = value.as_bytes().ok_or(Error::WrongType {
        key: "pieces",
        expected: "string",
    })?;
    let (chunks, remainder) = bytes.as_chunks::<20>();
    if !remainder.is_empty() {
        return Err(Error::InvalidPiecesLength { len: bytes.len() });
    }
    Ok(chunks.to_vec())
}

fn parse_announce_list(value: &Value) -> Result<Vec<Vec<String>>, Error> {
    let tiers = value.as_list().ok_or(Error::WrongType {
        key: "announce-list",
        expected: "list",
    })?;
    let mut parsed = Vec::with_capacity(tiers.len());
    for tier in tiers {
        let entries = tier.as_list().ok_or(Error::WrongType {
            key: "announce-list",
            expected: "list of lists",
        })?;
        let mut urls = Vec::with_capacity(entries.len());
        for entry in entries {
            let bytes = entry.as_bytes().ok_or(Error::WrongType {
                key: "announce-list",
                expected: "list of strings",
            })?;
            urls.push(utf8_field(bytes, "announce-list")?);
        }
        parsed.push(urls);
    }
    Ok(parsed)
}

fn parse_v1_files(info: &[(Vec<u8>, Value)], name: &str) -> Result<Vec<(Vec<String>, u64)>, Error> {
    let length = dict_get(info, b"length");
    let files = dict_get(info, b"files");
    match (length, files) {
        (Some(_), Some(_)) => Err(Error::ConflictingFileLists),
        (None, None) => Err(Error::MissingFileList),
        (Some(value), None) => {
            let length = non_negative_length(value, "length")?;
            Ok(vec![(vec![name.to_owned()], length)])
        }
        (None, Some(value)) => {
            let entries = value.as_list().ok_or(Error::WrongType {
                key: "files",
                expected: "list",
            })?;
            if entries.is_empty() {
                return Err(Error::EmptyFileList);
            }
            let mut paths = Vec::with_capacity(entries.len());
            for entry in entries {
                let file = entry.as_dict().ok_or(Error::WrongType {
                    key: "files",
                    expected: "list of dictionaries",
                })?;
                let length_value =
                    dict_get(file, b"length").ok_or(Error::MissingKey { key: "length" })?;
                let length = non_negative_length(length_value, "length")?;
                let path_value =
                    dict_get(file, b"path").ok_or(Error::MissingKey { key: "path" })?;
                let path_list = path_value.as_list().ok_or(Error::WrongType {
                    key: "path",
                    expected: "list",
                })?;
                if path_list.is_empty() {
                    return Err(Error::EmptyFilePath);
                }
                let mut path = Vec::with_capacity(path_list.len() + 1);
                path.push(name.to_owned());
                for component in path_list {
                    let bytes = component.as_bytes().ok_or(Error::WrongType {
                        key: "path",
                        expected: "list of strings",
                    })?;
                    path.push(utf8_field(bytes, "path")?);
                }
                paths.push((path, length));
            }
            Ok(paths)
        }
    }
}

struct V2File {
    path: Vec<String>,
    length: u64,
    pieces_root: Option<InfoHashV2>,
}

fn parse_file_tree(value: &Value) -> Result<Vec<V2File>, Error> {
    let root = value.as_dict().ok_or(Error::WrongType {
        key: "file tree",
        expected: "dictionary",
    })?;
    let mut files = Vec::new();
    let mut path = Vec::new();
    walk_file_tree(root, &mut path, true, &mut files)?;
    if files.is_empty() {
        return Err(Error::EmptyFileList);
    }
    Ok(files)
}

fn walk_file_tree(
    node: &[(Vec<u8>, Value)],
    path: &mut Vec<String>,
    is_root: bool,
    files: &mut Vec<V2File>,
) -> Result<(), Error> {
    if let Some((_, file_value)) = node.iter().find(|(key, _)| key.is_empty()) {
        if is_root {
            return Err(Error::RootFileEntry);
        }
        if node.len() != 1 {
            return Err(Error::FileEntryWithSiblings);
        }
        let file = file_value.as_dict().ok_or(Error::WrongType {
            key: "file tree",
            expected: "dictionary",
        })?;
        let length_value = dict_get(file, b"length").ok_or(Error::MissingKey { key: "length" })?;
        let length = non_negative_length(length_value, "length")?;
        let pieces_root = match dict_get(file, b"pieces root") {
            Some(value) => {
                let bytes = value.as_bytes().ok_or(Error::WrongType {
                    key: "pieces root",
                    expected: "string",
                })?;
                if bytes.len() != 32 {
                    return Err(Error::InvalidPiecesRoot { len: bytes.len() });
                }
                let mut root = [0u8; 32];
                root.copy_from_slice(bytes);
                Some(root)
            }
            None => None,
        };
        if length > 0 && pieces_root.is_none() {
            return Err(Error::MissingPiecesRoot {
                path: path.join("/"),
            });
        }
        files.push(V2File {
            path: path.clone(),
            length,
            pieces_root,
        });
        return Ok(());
    }

    for (key, value) in node {
        let name = utf8_field(key, "file tree")?;
        if name.is_empty() {
            return Err(Error::EmptyPathElement);
        }
        let child = value.as_dict().ok_or(Error::WrongType {
            key: "file tree",
            expected: "dictionary",
        })?;
        path.push(name);
        walk_file_tree(child, path, false, files)?;
        path.pop();
    }
    Ok(())
}

fn parse_piece_layers(
    value: &Value,
    files: &[V2File],
    piece_length: u32,
) -> Result<BTreeMap<InfoHashV2, Vec<InfoHashV2>>, Error> {
    let layers = value.as_dict().ok_or(Error::WrongType {
        key: "piece layers",
        expected: "dictionary",
    })?;

    let mut roots: BTreeMap<InfoHashV2, &V2File> = BTreeMap::new();
    for file in files {
        if file.length > 0
            && let Some(root) = file.pieces_root
        {
            roots.insert(root, file);
        }
    }

    let mut parsed = BTreeMap::new();
    for (key, value) in layers {
        if key.len() != 32 {
            return Err(Error::InvalidPiecesRoot { len: key.len() });
        }
        let mut root = [0u8; 32];
        root.copy_from_slice(key);

        let bytes = value.as_bytes().ok_or(Error::WrongType {
            key: "piece layers",
            expected: "string",
        })?;
        let (chunks, remainder) = bytes.as_chunks::<32>();
        if !remainder.is_empty() {
            return Err(Error::InvalidPieceLayerLength { len: bytes.len() });
        }
        let file = roots.get(&root).ok_or(Error::UnknownPieceLayer)?;
        let expected = file.length.div_ceil(u64::from(piece_length));
        let actual = chunks.len() as u64;
        if actual != expected {
            return Err(Error::PieceLayerCountMismatch {
                path: file.path.join("/"),
                expected,
                actual,
            });
        }
        let hashes = chunks.to_vec();
        parsed.insert(root, hashes);
    }
    Ok(parsed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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

    fn bytes(value: &[u8]) -> Value {
        Value::Bytes(value.to_vec())
    }

    fn list(values: Vec<Value>) -> Value {
        Value::List(values)
    }

    fn piece_blob(count: usize) -> Vec<u8> {
        (0..count).flat_map(|index| [index as u8; 20]).collect()
    }

    fn tiered() -> Value {
        list(vec![
            list(vec![text("udp://one/announce")]),
            list(vec![
                text("http://two/announce"),
                text("http://three/announce"),
            ]),
        ])
    }

    /// A v1 single-file torrent plus the exact class-generated info bytes.
    fn v1_single(piece_length: i64, length: i64, piece_count: usize) -> (Vec<u8>, Vec<u8>) {
        let info = dict(vec![
            ("length", Value::Int(length)),
            ("name", text("file.bin")),
            ("piece length", Value::Int(piece_length)),
            ("pieces", bytes(&piece_blob(piece_count))),
        ]);
        let raw_info = bencode::encode(&info);
        let torrent = dict(vec![
            ("announce", text("http://tracker.example/announce")),
            ("comment", text("golden")),
            ("created by", text("flux-test")),
            ("creation date", Value::Int(1_700_000_000)),
            ("announce-list", tiered()),
            ("info", info),
        ]);
        (bencode::encode(&torrent), raw_info)
    }

    fn v2_info(
        name: &str,
        piece_length: i64,
        file_tree: Value,
        extra: Vec<(&str, Value)>,
    ) -> Value {
        let mut entries = vec![
            ("file tree", file_tree),
            ("meta version", Value::Int(2)),
            ("name", text(name)),
            ("piece length", Value::Int(piece_length)),
        ];
        entries.extend(extra);
        dict(entries)
    }

    fn v2_props(length: i64, pieces_root: Option<[u8; 32]>) -> Value {
        let mut entries = vec![("length", Value::Int(length))];
        if let Some(root) = pieces_root {
            entries.push(("pieces root", bytes(&root)));
        }
        dict(entries)
    }

    fn v2_file(length: i64, pieces_root: Option<[u8; 32]>) -> Value {
        dict(vec![("", v2_props(length, pieces_root))])
    }

    fn single_file_tree(name: &str, length: i64, root: Option<[u8; 32]>) -> Value {
        dict(vec![(name, v2_file(length, root))])
    }

    #[test]
    fn parses_v1_single_file_and_hashes_raw_info() {
        let (torrent, raw_info) = v1_single(4, 8, 2);
        let metainfo = parse(&torrent).unwrap();

        assert_eq!(metainfo.kind(), TorrentKind::V1);
        assert_eq!(metainfo.info_hash(), Some(sha1_hash(&raw_info)));
        assert_eq!(metainfo.info_hash_v2(), None);
        assert_eq!(
            metainfo.announce.as_deref(),
            Some("http://tracker.example/announce")
        );
        assert_eq!(metainfo.announce_list.len(), 2);
        assert_eq!(metainfo.announce_list[1].len(), 2);
        assert_eq!(metainfo.comment.as_deref(), Some("golden"));
        assert_eq!(metainfo.created_by.as_deref(), Some("flux-test"));
        assert_eq!(metainfo.creation_date, Some(1_700_000_000));
        assert!(!metainfo.is_private());

        let info = &metainfo.info;
        assert_eq!(info.name, "file.bin");
        assert_eq!(info.piece_length, 4);
        assert_eq!(info.pieces.as_ref().map(Vec::len), Some(2));
        assert_eq!(info.layout.files().len(), 1);
        assert_eq!(info.layout.files()[0].path, vec!["file.bin".to_owned()]);
        assert_eq!(info.layout.files()[0].offset, 0);
        assert_eq!(info.layout.total_length(), 8);
        assert_eq!(info.layout.piece_count(), 2);
        assert_eq!(info.piece_layers.len(), 0);
    }

    #[test]
    fn parses_v1_multi_file_paths_and_pieces() {
        let info = dict(vec![
            (
                "files",
                list(vec![
                    dict(vec![
                        ("length", Value::Int(3)),
                        ("path", list(vec![text("a.txt")])),
                    ]),
                    dict(vec![
                        ("length", Value::Int(5)),
                        ("path", list(vec![text("sub"), text("b.txt")])),
                    ]),
                ]),
            ),
            ("name", text("root")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(2))),
            ("private", Value::Int(1)),
        ]);
        let torrent = bencode::encode(&dict(vec![("info", info)]));
        let metainfo = parse(&torrent).unwrap();

        assert!(metainfo.is_private());
        assert_eq!(metainfo.info.layout.total_length(), 8);
        assert_eq!(metainfo.info.layout.piece_count(), 2);
        assert_eq!(
            metainfo.info.layout.files()[0].path,
            vec!["root".to_owned(), "a.txt".to_owned()]
        );
        assert_eq!(
            metainfo.info.layout.files()[1].path,
            vec!["root".to_owned(), "sub".to_owned(), "b.txt".to_owned()]
        );
        assert_eq!(
            metainfo.info.layout.map_piece(0).unwrap(),
            vec![
                PieceRange {
                    file_index: 0,
                    offset: 0,
                    length: 3
                },
                PieceRange {
                    file_index: 1,
                    offset: 0,
                    length: 1
                }
            ]
        );
    }

    #[test]
    fn private_flag_accepts_zero_and_one() {
        for (flag, expected) in [(0, false), (1, true)] {
            let info = dict(vec![
                ("length", Value::Int(4)),
                ("name", text("file.bin")),
                ("piece length", Value::Int(4)),
                ("pieces", bytes(&piece_blob(1))),
                ("private", Value::Int(flag)),
            ]);
            let torrent = bencode::encode(&dict(vec![("info", info)]));
            assert_eq!(parse(&torrent).unwrap().is_private(), expected);
        }

        let info = dict(vec![
            ("length", Value::Int(4)),
            ("name", text("file.bin")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(1))),
            ("private", Value::Int(2)),
        ]);
        let torrent = bencode::encode(&dict(vec![("info", info)]));
        assert!(matches!(
            parse(&torrent).unwrap_err(),
            Error::InvalidPrivateFlag { value: 2 }
        ));
    }

    #[test]
    fn parses_v2_single_file_with_piece_layers() {
        let root = [0xAB; 32];
        let layer: Vec<u8> = [0x01u8; 32].into_iter().chain([0x02u8; 32]).collect();
        let info = v2_info(
            "movie.mkv",
            16384,
            single_file_tree("movie.mkv", 20000, Some(root)),
            vec![],
        );
        let torrent = Value::Dict(vec![
            (b"info".to_vec(), info),
            (
                b"piece layers".to_vec(),
                Value::Dict(vec![(root.to_vec(), bytes(&layer))]),
            ),
        ]);
        let encoded = bencode::encode(&torrent);
        let metainfo = parse(&encoded).unwrap();

        assert_eq!(metainfo.kind(), TorrentKind::V2);
        assert_eq!(metainfo.info_hash(), None);
        assert_eq!(
            metainfo.info_hash_v2(),
            Some(sha256_hash(&bencoded_info(&torrent)))
        );
        assert_eq!(metainfo.info.piece_length, 16384);
        assert_eq!(metainfo.info.pieces, None);
        assert_eq!(metainfo.info.layout.piece_count(), 2);
        assert_eq!(metainfo.info.layout.piece_size(1), Some(20000 - 16384));
        assert_eq!(metainfo.info.piece_layers.len(), 1);
        assert_eq!(metainfo.info.piece_layers[&root].len(), 2);
        assert_eq!(
            metainfo.info.layout.files()[0].path,
            vec!["movie.mkv".to_owned()]
        );
    }

    fn bencoded_info(torrent: &Value) -> Vec<u8> {
        bencode::encode(torrent.get(b"info").unwrap())
    }

    #[test]
    fn v2_files_are_piece_aligned() {
        let file_tree = dict(vec![
            ("a.bin", v2_file(20000, Some([0x11; 32]))),
            ("empty.bin", v2_file(0, None)),
            ("z.bin", v2_file(1000, Some([0x22; 32]))),
        ]);
        let info = v2_info("root", 16384, file_tree, vec![]);
        let torrent = bencode::encode(&dict(vec![("info", info)]));
        let metainfo = parse(&torrent).unwrap();
        let layout = &metainfo.info.layout;

        assert_eq!(layout.files().len(), 3);
        assert_eq!(layout.files()[1].length, 0);
        assert_eq!(layout.files()[2].offset, 32768);
        assert_eq!(layout.total_length(), 33768);
        assert_eq!(layout.payload_length(), 21000);
        assert_eq!(layout.piece_count(), 3);
        assert_eq!(layout.piece_size(1), Some(3616));
        assert_eq!(layout.piece_size(2), Some(1000));
        assert_eq!(
            layout.map_piece(1).unwrap(),
            vec![PieceRange {
                file_index: 0,
                offset: 16384,
                length: 3616
            }]
        );
        assert_eq!(layout.piece_index_at(21000), None);
        assert_eq!(layout.piece_index_at(19000), Some(1));
        assert_eq!(layout.piece_index_at(32768), Some(2));
    }

    #[test]
    fn parses_hybrid_torrent() {
        let root = [0x33; 32];
        let layer: Vec<u8> = [0x44u8; 32].into_iter().chain([0x55u8; 32]).collect();
        let info = v2_info(
            "file.bin",
            16384,
            single_file_tree("file.bin", 20000, Some(root)),
            vec![
                ("length", Value::Int(20000)),
                ("pieces", bytes(&piece_blob(2))),
            ],
        );
        let raw_info = bencode::encode(&info);
        let torrent = Value::Dict(vec![
            (b"info".to_vec(), info),
            (
                b"piece layers".to_vec(),
                Value::Dict(vec![(root.to_vec(), bytes(&layer))]),
            ),
        ]);
        let encoded = bencode::encode(&torrent);
        let metainfo = parse(&encoded).unwrap();

        assert_eq!(metainfo.kind(), TorrentKind::Hybrid);
        assert_eq!(metainfo.info_hash(), Some(sha1_hash(&raw_info)));
        assert_eq!(metainfo.info_hash_v2(), Some(sha256_hash(&raw_info)));
        assert_eq!(metainfo.info.pieces.as_ref().map(Vec::len), Some(2));
        assert_eq!(metainfo.info.layout.total_length(), 20000);
        assert_eq!(metainfo.info.piece_layers[&root].len(), 2);
    }

    #[test]
    fn layout_maps_pieces_across_file_boundaries() {
        let layout = FileLayout::contiguous(
            vec![
                (vec!["t".to_owned(), "a".to_owned()], 3),
                (vec!["t".to_owned(), "b".to_owned()], 0),
                (vec!["t".to_owned(), "c".to_owned()], 5),
                (vec!["t".to_owned(), "d".to_owned()], 5),
            ],
            4,
        )
        .unwrap();

        assert_eq!(layout.total_length(), 13);
        assert_eq!(layout.payload_length(), 13);
        assert_eq!(layout.piece_count(), 4);
        assert_eq!(layout.piece_size(0), Some(4));
        assert_eq!(layout.piece_size(3), Some(1));
        assert_eq!(
            layout.map_piece(0).unwrap(),
            vec![
                PieceRange {
                    file_index: 0,
                    offset: 0,
                    length: 3
                },
                PieceRange {
                    file_index: 2,
                    offset: 0,
                    length: 1
                }
            ]
        );
        assert_eq!(
            layout.map_piece(1).unwrap(),
            vec![PieceRange {
                file_index: 2,
                offset: 1,
                length: 4
            }]
        );
        assert_eq!(
            layout.map_piece(2).unwrap(),
            vec![PieceRange {
                file_index: 3,
                offset: 0,
                length: 4
            }]
        );
        assert_eq!(
            layout.map_piece(3).unwrap(),
            vec![PieceRange {
                file_index: 3,
                offset: 4,
                length: 1
            }]
        );
        assert_eq!(layout.map_piece(4), None);
        assert_eq!(layout.piece_index_at(12), Some(3));
        assert_eq!(layout.piece_index_at(13), None);
    }

    #[test]
    fn aligned_layout_handles_empty_files_and_gaps() {
        let layout = FileLayout::aligned(
            vec![
                (vec!["a".to_owned()], 20000),
                (vec!["empty".to_owned()], 0),
                (vec!["c".to_owned()], 1000),
            ],
            16384,
        )
        .unwrap();

        assert!(layout.is_aligned());
        assert_eq!(layout.files()[1].offset, 20000);
        assert_eq!(layout.files()[2].offset, 32768);
        assert_eq!(layout.piece_count(), 3);
        assert_eq!(layout.piece_size(2), Some(1000));
        assert_eq!(layout.map_piece(0).unwrap()[0].length, 16384);
        assert_eq!(layout.piece_index_at(20000), None);
    }

    #[test]
    fn aligned_layout_consistent_across_shapes() {
        let piece_length = 4u32;
        let shapes: &[&[u64]] = &[
            &[0, 0],
            &[1, 0],
            &[0, 1],
            &[1, 1],
            &[3, 0],
            &[0, 3],
            &[3, 0, 5],
            &[5, 0, 3],
            &[5, 0, 0, 1],
            &[9, 0, 1],
            &[1, 2, 3, 4, 5],
        ];
        for lengths in shapes {
            if lengths.iter().all(|length| *length == 0) {
                continue;
            }
            let files: Vec<(Vec<String>, u64)> = lengths
                .iter()
                .enumerate()
                .map(|(index, length)| (vec![format!("f{index}")], *length))
                .collect();

            let aligned = FileLayout::aligned(files.clone(), piece_length).unwrap();
            assert_eq!(
                aligned.piece_count() as u64,
                aligned.total_length().div_ceil(u64::from(piece_length))
            );
            let mut covered = 0u64;
            for piece in 0..aligned.piece_count() {
                let ranges = aligned.map_piece(piece).unwrap();
                assert_eq!(ranges.len(), 1);
                covered += ranges[0].length;
                assert_eq!(aligned.piece_size(piece), Some(ranges[0].length as u32));
                let start = (piece as u64) * u64::from(piece_length);
                assert_eq!(aligned.piece_index_at(start), Some(piece));
            }
            assert_eq!(covered, aligned.payload_length());

            let contiguous = FileLayout::contiguous(files, piece_length).unwrap();
            assert_eq!(
                contiguous.piece_count() as u64,
                contiguous.total_length().div_ceil(u64::from(piece_length))
            );
            let mut covered = 0u64;
            for piece in 0..contiguous.piece_count() {
                covered += contiguous
                    .map_piece(piece)
                    .unwrap()
                    .iter()
                    .map(|range| range.length)
                    .sum::<u64>();
                let start = (piece as u64) * u64::from(piece_length);
                assert_eq!(contiguous.piece_index_at(start), Some(piece));
            }
            assert_eq!(covered, contiguous.total_length());
        }
    }

    #[test]
    fn rejects_duplicate_file_paths() {
        let files = vec![
            (vec!["root".to_owned(), "a.bin".to_owned()], 4),
            (vec!["root".to_owned(), "a.bin".to_owned()], 4),
        ];
        assert!(matches!(
            FileLayout::contiguous(files, 4),
            Err(Error::DuplicateFilePath { .. })
        ));
    }

    #[test]
    fn truncations_and_mutations_never_panic() {
        let (torrent, _) = v1_single(4, 8, 2);
        for end in 0..=torrent.len() {
            let _ = parse(&torrent[..end]);
        }

        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for index in 0..torrent.len() {
            for _ in 0..16 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let mut mutated = torrent.clone();
                mutated[index] = state as u8;
                let _ = parse(&mutated);
            }
        }
    }

    #[test]
    fn rejects_structurally_invalid_torrents() {
        assert!(matches!(parse(b"i1e").unwrap_err(), Error::NotADictionary));
        assert!(matches!(
            parse(b"de").unwrap_err(),
            Error::MissingKey { key: "info" }
        ));

        let no_name = dict(vec![
            ("length", Value::Int(4)),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(1))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", no_name)]))).unwrap_err(),
            Error::MissingKey { key: "name" }
        ));

        let empty_name = dict(vec![
            ("length", Value::Int(4)),
            ("name", text("")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(1))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", empty_name)]))).unwrap_err(),
            Error::EmptyValue { key: "name" }
        ));
    }

    #[test]
    fn rejects_bad_piece_metadata() {
        // Zero piece length.
        let info = dict(vec![
            ("length", Value::Int(4)),
            ("name", text("x")),
            ("piece length", Value::Int(0)),
            ("pieces", bytes(&piece_blob(0))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::ZeroPieceLength
        ));

        // pieces not a multiple of 20.
        let info = dict(vec![
            ("length", Value::Int(4)),
            ("name", text("x")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&[0u8; 21])),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::InvalidPiecesLength { len: 21 }
        ));

        // `piece length` larger than a u32.
        let info = dict(vec![
            ("length", Value::Int(4)),
            ("name", text("x")),
            ("piece length", Value::Int(i64::from(u32::MAX) + 1)),
            ("pieces", bytes(&piece_blob(1))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::PieceLengthTooLarge { .. }
        ));

        // Hash count disagrees with the layout.
        let info = dict(vec![
            ("length", Value::Int(8)),
            ("name", text("x")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(1))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::PieceCountMismatch {
                expected: 2,
                actual: 1
            }
        ));
    }

    #[test]
    fn rejects_bad_v1_file_lists() {
        let conflicting = dict(vec![
            ("length", Value::Int(4)),
            ("files", list(vec![])),
            ("name", text("x")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(1))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", conflicting)]))).unwrap_err(),
            Error::ConflictingFileLists
        ));

        let empty = dict(vec![
            ("files", list(vec![])),
            ("name", text("x")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(0))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", empty)]))).unwrap_err(),
            Error::EmptyFileList
        ));

        let zero = dict(vec![
            ("length", Value::Int(0)),
            ("name", text("x")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(0))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", zero)]))).unwrap_err(),
            Error::EmptyTorrent
        ));

        let negative = dict(vec![
            ("length", Value::Int(-1)),
            ("name", text("x")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(0))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", negative)]))).unwrap_err(),
            Error::NegativeLength
        ));
    }

    #[test]
    fn rejects_total_length_overflow() {
        let file = |length: i64| {
            dict(vec![
                ("length", Value::Int(length)),
                ("path", list(vec![text("f")])),
            ])
        };
        let info = dict(vec![
            (
                "files",
                list(vec![file(i64::MAX), file(i64::MAX), file(i64::MAX)]),
            ),
            ("name", text("x")),
            ("piece length", Value::Int(16384)),
            ("pieces", bytes(&piece_blob(0))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::TotalLengthOverflow
        ));
    }

    #[test]
    fn rejects_invalid_utf8_names() {
        let info = dict(vec![
            ("length", Value::Int(4)),
            ("name", bytes(b"\xff\xfe")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(1))),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::InvalidUtf8 { key: "name" }
        ));
    }

    #[test]
    fn rejects_bad_v2_headers() {
        // Missing meta version.
        let info = dict(vec![
            (
                "file tree",
                single_file_tree("x.bin", 1000, Some([0x11; 32])),
            ),
            ("name", text("x")),
            ("piece length", Value::Int(16384)),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::MissingMetaVersion
        ));

        // Unsupported meta version.
        let info = dict(vec![
            (
                "file tree",
                single_file_tree("x.bin", 1000, Some([0x11; 32])),
            ),
            ("meta version", Value::Int(3)),
            ("name", text("x")),
            ("piece length", Value::Int(16384)),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::UnsupportedMetaVersion { version: 3 }
        ));

        // Piece length below 16 KiB.
        let info = v2_info(
            "x",
            8192,
            single_file_tree("x.bin", 1000, Some([0x11; 32])),
            vec![],
        );
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::PieceLengthTooSmall { value: 8192 }
        ));

        // Non-power-of-two piece length.
        let info = v2_info(
            "x",
            24576,
            single_file_tree("x.bin", 1000, Some([0x11; 32])),
            vec![],
        );
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::PieceLengthNotPowerOfTwo { value: 24576 }
        ));
    }

    #[test]
    fn rejects_malformed_v2_file_trees() {
        // Root cannot be a file.
        let info = dict(vec![
            ("file tree", dict(vec![("", v2_props(1, Some([0x11; 32])))])),
            ("meta version", Value::Int(2)),
            ("name", text("x")),
            ("piece length", Value::Int(16384)),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::RootFileEntry
        ));

        // A file node with a sibling directory entry.
        let info = v2_info(
            "x",
            16384,
            dict(vec![(
                "dir",
                Value::Dict(vec![
                    (b"".to_vec(), v2_props(1, Some([0x11; 32]))),
                    (b"child".to_vec(), v2_file(1, Some([0x22; 32]))),
                ]),
            )]),
            vec![],
        );
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::FileEntryWithSiblings
        ));

        // Non-empty file without a pieces root.
        let info = v2_info("x", 16384, single_file_tree("x.bin", 1000, None), vec![]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::MissingPiecesRoot { .. }
        ));

        // pieces root of the wrong size.
        let info = v2_info(
            "x",
            16384,
            dict(vec![(
                "x.bin",
                dict(vec![(
                    "",
                    dict(vec![
                        ("length", Value::Int(1000)),
                        ("pieces root", bytes(&[0u8; 31])),
                    ]),
                )]),
            )]),
            vec![],
        );
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::InvalidPiecesRoot { len: 31 }
        ));
    }

    #[test]
    fn rejects_bad_piece_layers() {
        let root = [0xAB; 32];
        let info = v2_info(
            "x",
            16384,
            single_file_tree("x.bin", 40000, Some(root)),
            vec![],
        );

        // Wrong count: 40000 bytes at 16384 needs 3 hashes.
        let layer = bytes(&[0u8; 64]);
        let torrent = Value::Dict(vec![
            (b"info".to_vec(), info),
            (
                b"piece layers".to_vec(),
                Value::Dict(vec![(root.to_vec(), layer)]),
            ),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&torrent)).unwrap_err(),
            Error::PieceLayerCountMismatch {
                expected: 3,
                actual: 2,
                ..
            }
        ));

        // Not a multiple of 32.
        let info = v2_info(
            "x",
            16384,
            single_file_tree("x.bin", 40000, Some(root)),
            vec![],
        );
        let torrent = Value::Dict(vec![
            (b"info".to_vec(), info),
            (
                b"piece layers".to_vec(),
                Value::Dict(vec![(root.to_vec(), bytes(&[0u8; 33]))]),
            ),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&torrent)).unwrap_err(),
            Error::InvalidPieceLayerLength { len: 33 }
        ));

        // Unknown root.
        let info = v2_info(
            "x",
            16384,
            single_file_tree("x.bin", 40000, Some(root)),
            vec![],
        );
        let torrent = Value::Dict(vec![
            (b"info".to_vec(), info),
            (
                b"piece layers".to_vec(),
                Value::Dict(vec![([0xCD; 32].to_vec(), bytes(&[0u8; 96]))]),
            ),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&torrent)).unwrap_err(),
            Error::UnknownPieceLayer
        ));

        // Piece layers without a v2 file tree.
        let info = dict(vec![
            ("length", Value::Int(4)),
            ("name", text("x")),
            ("piece length", Value::Int(4)),
            ("pieces", bytes(&piece_blob(1))),
        ]);
        let torrent = Value::Dict(vec![
            (b"info".to_vec(), info),
            (
                b"piece layers".to_vec(),
                Value::Dict(vec![(root.to_vec(), bytes(&[0u8; 32]))]),
            ),
        ]);
        assert!(matches!(
            parse(&bencode::encode(&torrent)).unwrap_err(),
            Error::PieceLayersWithoutFileTree
        ));
    }

    #[test]
    fn rejects_empty_v2_tree() {
        let info = v2_info("x", 16384, dict(vec![]), vec![]);
        assert!(matches!(
            parse(&bencode::encode(&dict(vec![("info", info)]))).unwrap_err(),
            Error::EmptyFileList
        ));
    }
}
