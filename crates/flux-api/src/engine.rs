//! The engine boundary.
//!
//! [`Engine`] is the only thing the control plane knows about the daemon. All
//! data crossing it is plain owned serde types, so `flux-api` never depends on
//! the torrent engine crates and tests can plug in [`MockEngine`].
//!
//! The engine is responsible for real torrent lifecycle; the transport does not
//! synthesize progress, speeds, or peers. [`TorrentSummary`] therefore contains
//! no rate fields — add them to the DTO only once the engine can report real
//! measurements.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Session information returned by `session.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// Human-readable session name.
    pub name: String,
    /// Engine build version.
    pub version: String,
    /// Control API version. Starts at `1`.
    pub api_version: u32,
}

impl Info {
    /// The current control API version.
    pub const API_VERSION: u32 = 1;

    /// Information for this build, for engines that do not override it.
    pub fn current() -> Self {
        Self {
            name: "oxide-flux".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            api_version: Self::API_VERSION,
        }
    }
}

/// Honest lifecycle state of a torrent.
///
/// This serializes as a lowercase string (`"queued"`, `"downloading"`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TorrentState {
    /// Accepted, waiting for a slot or metadata.
    Queued,
    /// Verifying existing data.
    Checking,
    /// Actively transferring pieces.
    Downloading,
    /// Complete and serving.
    Seeding,
    /// Data is complete but nothing is transferring: no peer layer exists in this
    /// build, or the torrent is waiting for a seed slot.
    Complete,
    /// Suspended by the user.
    Paused,
    /// Stopped on an engine error.
    Error,
}

/// Per-torrent download priority for a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// Do not download this file.
    Skip,
    /// Download after normal and high priority work.
    Low,
    /// Default priority.
    #[default]
    Normal,
    /// Download ahead of other files.
    High,
}

/// Piece-selection policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamingMode {
    /// Walk pieces from the playhead forward. Default, and what playback needs.
    #[default]
    Sequential,
    /// Serve the streaming window first, then prefer rare pieces.
    Rarest,
}

/// A torrent entry for the list view.
///
/// Speeds and peer counts are real measurements from the swarm; they read `0`
/// when no swarm is running. `progress` must be finite and within `0.0..=1.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentSummary {
    /// Opaque torrent id (the infohash for real engines).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Current lifecycle state.
    pub state: TorrentState,
    /// Whether the torrent is `private: 1`.
    pub private: bool,
    /// Completion fraction in `0.0..=1.0`.
    pub progress: f64,
    /// Verified pieces.
    pub pieces_have: u64,
    /// Total pieces in the torrent.
    pub pieces_total: u64,
    /// Total payload bytes, or `0` before metadata is known.
    pub total_bytes: u64,
    /// Connected peers, as reported by the swarm.
    pub peers: u32,
    /// Current download rate in bytes per second.
    pub download_rate: u64,
    /// Current upload rate in bytes per second.
    pub upload_rate: u64,
    /// Last engine failure, when the torrent is in an error state.
    pub last_error: Option<String>,
}

/// One file inside a torrent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    /// Position in the torrent's file list.
    pub index: usize,
    /// Path relative to the torrent root.
    pub path: String,
    /// File length in bytes.
    pub length: u64,
    /// Current download priority.
    pub priority: Priority,
    /// Verified bytes covering this file, rounded up to whole pieces.
    pub bytes_completed: u64,
    /// Whether every byte of the file is verified.
    pub completed: bool,
    /// Offset of the file in the torrent address space.
    pub offset: u64,
}

/// A torrent entry for the detail view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentDetail {
    /// Summary fields, flattened into the detail object.
    #[serde(flatten)]
    pub summary: TorrentSummary,
    /// Files in torrent order.
    pub files: Vec<FileView>,
    /// Torrent piece length in bytes.
    pub piece_length: u64,
    /// Download root configured for this torrent.
    pub download_dir: String,
}

/// Session-wide counters returned by `session.stats`.
///
/// Counters, not rates: the engine owns when they advance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Number of torrents in the session.
    pub torrents: usize,
    /// Torrents doing work (checking, downloading, or seeding).
    pub active: usize,
    /// Torrents downloading.
    pub downloading: usize,
    /// Torrents seeding.
    pub seeding: usize,
    /// Torrents paused by the user.
    pub paused: usize,
    /// Payload bytes downloaded this session.
    pub bytes_downloaded: u64,
    /// Payload bytes uploaded this session.
    pub bytes_uploaded: u64,
}

/// What to add to the session.
///
/// Wire form is internally tagged:
///
/// ```json
/// {"kind":"metainfo","bytes":[100,56,58,97,110,110,111,117,110,99,101]}
/// {"kind":"magnet","uri":"magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum AddTorrent {
    /// Raw `.torrent` metainfo bytes.
    Metainfo {
        /// Bencoded metainfo bytes.
        bytes: Vec<u8>,
    },
    /// A magnet URI.
    Magnet {
        /// The URI, starting with `magnet:?`.
        uri: String,
    },
}

/// Domain failures an engine can report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// The id is not in the session.
    #[error("torrent not found: {0}")]
    NotFound(String),
    /// The same torrent is already present.
    #[error("torrent already exists: {0}")]
    Duplicate(String),
    /// The metainfo or magnet URI cannot be used.
    #[error("invalid torrent source: {0}")]
    InvalidSource(String),
    /// A priority update referenced a file that does not exist.
    #[error("invalid file index: {0}")]
    InvalidFileIndex(usize),
    /// The engine does not implement the operation.
    #[error("unsupported operation: {0}")]
    Unsupported(String),
    /// A storage or other I/O failure.
    #[error("i/o error: {0}")]
    Io(String),
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// The daemon-facing engine interface.
///
/// Implementations must be safe to share across request tasks. The transport
/// calls methods concurrently; an implementation that needs serialization owns
/// that internally (actor or lock), not the transport.
#[async_trait]
pub trait Engine: Send + Sync {
    /// Session identity and API version.
    async fn info(&self) -> Result<Info, EngineError>;

    /// All torrents, in engine-defined stable order.
    async fn list(&self) -> Result<Vec<TorrentSummary>, EngineError>;

    /// One torrent with files.
    async fn get(&self, id: &str) -> Result<TorrentDetail, EngineError>;

    /// Add metainfo bytes or a magnet URI.
    async fn add(&self, source: AddTorrent) -> Result<TorrentSummary, EngineError>;

    /// Pause a torrent.
    async fn pause(&self, id: &str) -> Result<(), EngineError>;

    /// Resume a paused torrent.
    async fn resume(&self, id: &str) -> Result<(), EngineError>;

    /// Remove a torrent, optionally deleting its files.
    async fn remove(&self, id: &str, delete_files: bool) -> Result<(), EngineError>;

    /// Replace priorities for the listed files.
    async fn set_file_priorities(
        &self,
        id: &str,
        priorities: Vec<(usize, Priority)>,
    ) -> Result<(), EngineError>;

    /// Move the streaming playhead, in pieces.
    async fn set_playhead(&self, id: &str, piece: u32) -> Result<(), EngineError>;

    /// Change the piece-selection policy.
    async fn set_streaming_mode(&self, id: &str, mode: StreamingMode) -> Result<(), EngineError>;

    /// Loopback URL that streams one file while it is still downloading.
    ///
    /// Engines without a streaming server return
    /// [`EngineError::Unsupported`].
    async fn stream_url(&self, id: &str, file: usize) -> Result<String, EngineError>;

    /// Session counters.
    async fn stats(&self) -> Result<Stats, EngineError>;
}

/// Deterministic in-memory engine for tests and transport development.
///
/// It does not parse metainfo or contact a swarm. Added torrents have no known
/// metadata (`total_bytes: 0`, zero pieces, no files) and start
/// [`TorrentState::Queued`]. Use [`MockEngine::seed`] to install a fully formed
/// [`TorrentDetail`] when a test needs files or progress.
#[derive(Debug, Default)]
pub struct MockEngine {
    torrents: Mutex<BTreeMap<String, TorrentDetail>>,
}

impl MockEngine {
    /// An empty session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a prepared torrent detail, primarily for tests.
    ///
    /// Rejects duplicate ids and summary values that violate the DTO contract
    /// (non-finite or out-of-range progress, more pieces than the total).
    pub fn seed(&self, detail: TorrentDetail) -> Result<(), EngineError> {
        validate_summary(&detail.summary)?;
        let mut torrents = self.lock()?;
        if torrents.contains_key(&detail.summary.id) {
            return Err(EngineError::Duplicate(detail.summary.id));
        }
        torrents.insert(detail.summary.id.clone(), detail);
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<String, TorrentDetail>>, EngineError> {
        self.torrents
            .lock()
            .map_err(|_| EngineError::Other("mock engine lock poisoned".to_owned()))
    }
}

#[async_trait]
impl Engine for MockEngine {
    async fn info(&self) -> Result<Info, EngineError> {
        Ok(Info::current())
    }

    async fn list(&self) -> Result<Vec<TorrentSummary>, EngineError> {
        let torrents = self.lock()?;
        Ok(torrents
            .values()
            .map(|detail| detail.summary.clone())
            .collect())
    }

    async fn get(&self, id: &str) -> Result<TorrentDetail, EngineError> {
        let torrents = self.lock()?;
        torrents
            .get(id)
            .cloned()
            .ok_or_else(|| EngineError::NotFound(id.to_owned()))
    }

    async fn add(&self, source: AddTorrent) -> Result<TorrentSummary, EngineError> {
        let id = source_id(&source)?;
        let name = source_name(&source, &id);
        let summary = TorrentSummary {
            id: id.clone(),
            name,
            state: TorrentState::Queued,
            private: false,
            progress: 0.0,
            pieces_have: 0,
            pieces_total: 0,
            total_bytes: 0,
            peers: 0,
            download_rate: 0,
            upload_rate: 0,
            last_error: None,
        };
        let detail = TorrentDetail {
            summary: summary.clone(),
            files: Vec::new(),
            piece_length: 0,
            download_dir: String::new(),
        };
        let mut torrents = self.lock()?;
        if torrents.contains_key(&id) {
            return Err(EngineError::Duplicate(id));
        }
        torrents.insert(id, detail);
        Ok(summary)
    }

    async fn pause(&self, id: &str) -> Result<(), EngineError> {
        let mut torrents = self.lock()?;
        let detail = torrents
            .get_mut(id)
            .ok_or_else(|| EngineError::NotFound(id.to_owned()))?;
        if detail.summary.state != TorrentState::Error {
            detail.summary.state = TorrentState::Paused;
        }
        Ok(())
    }

    async fn resume(&self, id: &str) -> Result<(), EngineError> {
        let mut torrents = self.lock()?;
        let detail = torrents
            .get_mut(id)
            .ok_or_else(|| EngineError::NotFound(id.to_owned()))?;
        if detail.summary.state == TorrentState::Paused {
            detail.summary.state = TorrentState::Queued;
        }
        Ok(())
    }

    async fn remove(&self, id: &str, _delete_files: bool) -> Result<(), EngineError> {
        // The mock owns no files, so `delete_files` is intentionally ignored.
        let mut torrents = self.lock()?;
        torrents
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| EngineError::NotFound(id.to_owned()))
    }

    async fn set_file_priorities(
        &self,
        id: &str,
        priorities: Vec<(usize, Priority)>,
    ) -> Result<(), EngineError> {
        let mut torrents = self.lock()?;
        let detail = torrents
            .get_mut(id)
            .ok_or_else(|| EngineError::NotFound(id.to_owned()))?;
        for (index, priority) in priorities {
            let file = detail
                .files
                .iter_mut()
                .find(|file| file.index == index)
                .ok_or(EngineError::InvalidFileIndex(index))?;
            file.priority = priority;
        }
        Ok(())
    }

    async fn set_playhead(&self, id: &str, _piece: u32) -> Result<(), EngineError> {
        let torrents = self.lock()?;
        torrents
            .get(id)
            .map(|_| ())
            .ok_or_else(|| EngineError::NotFound(id.to_owned()))
    }

    async fn set_streaming_mode(&self, id: &str, _mode: StreamingMode) -> Result<(), EngineError> {
        let torrents = self.lock()?;
        torrents
            .get(id)
            .map(|_| ())
            .ok_or_else(|| EngineError::NotFound(id.to_owned()))
    }

    async fn stream_url(&self, _id: &str, _file: usize) -> Result<String, EngineError> {
        Err(EngineError::Unsupported(
            "the mock engine has no streaming server".to_owned(),
        ))
    }

    async fn stats(&self) -> Result<Stats, EngineError> {
        let torrents = self.lock()?;
        let mut stats = Stats {
            torrents: torrents.len(),
            ..Stats::default()
        };
        for detail in torrents.values() {
            match detail.summary.state {
                TorrentState::Checking => {
                    stats.active += 1;
                }
                TorrentState::Downloading => {
                    stats.active += 1;
                    stats.downloading += 1;
                }
                TorrentState::Seeding => {
                    stats.active += 1;
                    stats.seeding += 1;
                }
                TorrentState::Paused => stats.paused += 1,
                TorrentState::Complete | TorrentState::Queued | TorrentState::Error => {}
            }
        }
        Ok(stats)
    }
}

fn validate_summary(summary: &TorrentSummary) -> Result<(), EngineError> {
    if !summary.progress.is_finite() || !(0.0..=1.0).contains(&summary.progress) {
        return Err(EngineError::InvalidSource(
            "progress must be finite and within 0.0..=1.0".to_owned(),
        ));
    }
    if summary.pieces_have > summary.pieces_total {
        return Err(EngineError::InvalidSource(
            "pieces_have must not exceed pieces_total".to_owned(),
        ));
    }
    Ok(())
}

fn source_id(source: &AddTorrent) -> Result<String, EngineError> {
    match source {
        AddTorrent::Metainfo { bytes } => {
            if bytes.is_empty() {
                return Err(EngineError::InvalidSource(
                    "metainfo must not be empty".to_owned(),
                ));
            }
            Ok(format!("{:016x}", fnv1a_64(bytes)))
        }
        AddTorrent::Magnet { uri } => magnet_id(uri),
    }
}

fn source_name(source: &AddTorrent, id: &str) -> String {
    match source {
        AddTorrent::Metainfo { .. } => format!("metainfo-{id}"),
        AddTorrent::Magnet { uri } => magnet_field(uri, "dn")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("magnet-{id}")),
    }
}

fn magnet_id(uri: &str) -> Result<String, EngineError> {
    let xt = magnet_field(uri, "xt")
        .ok_or_else(|| EngineError::InvalidSource("magnet URI has no xt parameter".to_owned()))?;
    if let Some(hash) = xt.strip_prefix("urn:btih:") {
        if hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
        return Err(EngineError::InvalidSource(
            "urn:btih infohash must be 40 hexadecimal characters".to_owned(),
        ));
    }
    if let Some(hash) = xt.strip_prefix("urn:btmh:") {
        let hash = hash.strip_prefix("1220").unwrap_or(hash);
        if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
        return Err(EngineError::InvalidSource(
            "urn:btmh infohash must be a sha2-256 multihash".to_owned(),
        ));
    }
    Err(EngineError::InvalidSource(
        "magnet xt must be urn:btih or urn:btmh".to_owned(),
    ))
}

fn magnet_field(uri: &str, key: &str) -> Option<String> {
    let query = uri.strip_prefix("magnet:?")?;
    for pair in query.split('&') {
        if let Some((name, value)) = pair.split_once('=')
            && name == key
        {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
            continue;
        }
        if bytes[index] == b'+' {
            decoded.push(b' ');
        } else {
            decoded.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn magnet(uri: &str) -> AddTorrent {
        AddTorrent::Magnet {
            uri: uri.to_owned(),
        }
    }

    #[tokio::test]
    async fn add_is_deterministic_and_rejects_duplicates() -> Result<(), EngineError> {
        let engine = MockEngine::new();
        let source =
            magnet("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=My+Movie");
        let first = engine.add(source.clone()).await?;
        assert_eq!(first.id, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(first.name, "My Movie");
        assert_eq!(first.state, TorrentState::Queued);
        assert!(!first.private);
        assert!((first.progress - 0.0).abs() < f64::EPSILON);

        assert!(matches!(
            engine.add(source).await,
            Err(EngineError::Duplicate(id)) if id == first.id
        ));
        Ok(())
    }

    #[tokio::test]
    async fn metainfo_ids_are_stable() -> Result<(), EngineError> {
        let engine = MockEngine::new();
        let source = AddTorrent::Metainfo {
            bytes: b"d4:infod4:name4:testee".to_vec(),
        };
        let first = engine.add(source.clone()).await?;
        assert_eq!(first.id.len(), 16);
        assert!(first.id.bytes().all(|byte| byte.is_ascii_hexdigit()));

        let other = MockEngine::new();
        assert_eq!(other.add(source).await?.id, first.id);
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_transitions_and_stats() -> Result<(), EngineError> {
        let engine = MockEngine::new();
        let id = engine
            .add(magnet(
                "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ))
            .await?
            .id;

        engine.pause(&id).await?;
        assert_eq!(engine.get(&id).await?.summary.state, TorrentState::Paused);
        assert_eq!(engine.stats().await?.paused, 1);

        engine.resume(&id).await?;
        assert_eq!(engine.get(&id).await?.summary.state, TorrentState::Queued);
        assert_eq!(engine.stats().await?.active, 0);

        engine.remove(&id, false).await?;
        assert!(matches!(
            engine.get(&id).await,
            Err(EngineError::NotFound(_))
        ));
        assert_eq!(engine.list().await?.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn seeded_torrents_accept_file_priorities() -> Result<(), EngineError> {
        let engine = MockEngine::new();
        engine.seed(TorrentDetail {
            summary: TorrentSummary {
                id: "abc".to_owned(),
                name: "seed".to_owned(),
                state: TorrentState::Downloading,
                private: true,
                progress: 0.25,
                pieces_have: 1,
                pieces_total: 4,
                total_bytes: 4096,
                peers: 0,
                download_rate: 0,
                upload_rate: 0,
                last_error: None,
            },
            files: vec![
                FileView {
                    index: 0,
                    path: "a.mkv".to_owned(),
                    length: 2048,
                    priority: Priority::Normal,
                    bytes_completed: 0,
                    completed: false,
                    offset: 0,
                },
                FileView {
                    index: 1,
                    path: "b.srt".to_owned(),
                    length: 2048,
                    priority: Priority::Normal,
                    bytes_completed: 0,
                    completed: false,
                    offset: 0,
                },
            ],
            piece_length: 16384,
            download_dir: "downloads".to_owned(),
        })?;

        engine
            .set_file_priorities("abc", vec![(1, Priority::Skip), (0, Priority::High)])
            .await?;
        let detail = engine.get("abc").await?;
        assert_eq!(detail.files[0].priority, Priority::High);
        assert_eq!(detail.files[1].priority, Priority::Skip);
        assert!(detail.summary.private);

        assert!(matches!(
            engine
                .set_file_priorities("abc", vec![(9, Priority::Low)])
                .await,
            Err(EngineError::InvalidFileIndex(9))
        ));
        Ok(())
    }

    #[test]
    fn seed_rejects_invalid_summaries() {
        let engine = MockEngine::new();
        let detail = TorrentDetail {
            summary: TorrentSummary {
                id: "bad".to_owned(),
                name: "bad".to_owned(),
                state: TorrentState::Queued,
                private: false,
                progress: 1.5,
                pieces_have: 0,
                pieces_total: 0,
                total_bytes: 0,
                peers: 0,
                download_rate: 0,
                upload_rate: 0,
                last_error: None,
            },
            files: Vec::new(),
            piece_length: 0,
            download_dir: String::new(),
        };
        assert!(matches!(
            engine.seed(detail),
            Err(EngineError::InvalidSource(_))
        ));
    }

    #[test]
    fn rejects_bad_magnets() {
        for uri in [
            "http://example.com/x.torrent",
            "magnet:?dn=no-infohash",
            "magnet:?xt=urn:btih:xyz",
            "magnet:?xt=urn:sha1:0123456789abcdef0123456789abcdef01234567",
        ] {
            assert!(matches!(magnet_id(uri), Err(EngineError::InvalidSource(_))));
        }
    }
}
