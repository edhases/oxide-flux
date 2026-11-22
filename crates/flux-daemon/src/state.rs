//! Session persistence: snapshot of torrents, priorities and paused state.
//!
//! The daemon writes one small JSON document next to the downloads, by default
//! `download_dir/.fluxd-state.json`. Each torrent stores the raw `.torrent`
//! bytes (hex in the JSON so the file stays text and diffable), the per-file
//! priorities, whether it is paused, and — only when it lives outside the
//! session directory — its own download root. There are no secrets in a
//! snapshot: no API token, no tracker credentials, no peer addresses.
//!
//! ## Guarantees
//!
//! * **Atomic writes.** The document is written to `<name>.tmp` in the same
//!   directory, flushed to disk, then renamed over the target. A failed save
//!   leaves the previous snapshot untouched and removes the temporary file.
//! * **Strict reads.** A missing file is a fresh session, not an error. Any
//!   other failure — unreadable file, corrupt JSON, bad hex, an unknown format
//!   version — is a typed [`StateError`] that names the path. Data is never
//!   silently discarded.
//! * **No blocking work on the runtime.** [`StateStore::load`] and
//!   [`StateStore::save`] are ordinary blocking functions (fine for the CLI);
//!   [`StateStore::load_async`] and [`StateStore::save_async`] move the file
//!   work to `spawn_blocking` for use inside the session actor.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Format version written by this build.
pub const STATE_VERSION: u32 = 1;

/// File name used when the store is rooted at a download directory.
pub const STATE_FILE_NAME: &str = ".fluxd-state.json";

/// Largest accepted `priorities` vector (one byte per file).
///
/// Empty is valid ("priorities unknown"), but a corrupt or hostile file must
/// not make the daemon allocate gigabytes while parsing.
pub const MAX_PRIORITIES: usize = 1_000_000;

/// The persisted form of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Format version, always [`STATE_VERSION`] for files this build writes.
    pub version: u32,
    /// Torrents to restore, in the order they were added.
    pub torrents: Vec<TorrentRecord>,
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            torrents: Vec::new(),
        }
    }
}

/// One torrent in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentRecord {
    /// Lowercase hex info hash, 40 characters (v1).
    pub info_hash: String,
    /// The raw `.torrent` bytes. Stored as lowercase hex.
    #[serde(with = "hex_bytes")]
    pub metainfo: Vec<u8>,
    /// One byte per file: `0` skip, `1` normal, `2` high. Empty means unknown.
    pub priorities: Vec<u8>,
    /// Whether the torrent is paused.
    pub paused: bool,
    /// The torrent's own download root, when it differs from the session
    /// directory; `None` means "use the session's configured directory".
    ///
    /// The snapshot is JSON, so the path is text. A root that is not valid UTF-8
    /// cannot be represented exactly and is recorded lossily (invalid sequences
    /// become `U+FFFD`) rather than dropped: on restore the torrent is re-rooted
    /// at that approximation, which is visible and fixable, instead of silently
    /// falling back to the session directory and stranding the bytes it already
    /// wrote. Roots chosen by the desktop UI always come from UTF-8 input, so the
    /// lossy case is a CLI-only edge.
    pub download_dir: Option<String>,
}

impl SessionSnapshot {
    /// An empty snapshot for a fresh session.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Something that went wrong while loading or saving a snapshot.
#[derive(Debug, Error)]
pub enum StateError {
    /// The state file could not be read.
    #[error("cannot read session state `{path}`: {source}")]
    Read {
        /// The file that was read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The state file could not be written, flushed or renamed.
    #[error("cannot write session state `{path}`: {source}")]
    Write {
        /// The target file.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The document is not valid JSON, has the wrong shape, or holds invalid
    /// field values.
    #[error("session state `{path}` is corrupt: {reason}")]
    Corrupt {
        /// The file that failed to load.
        path: PathBuf,
        /// What made it unusable.
        reason: String,
    },

    /// The document was written by a newer or unknown format version.
    #[error(
        "session state `{path}` uses unsupported version {version}; this build understands \
         version {supported}"
    )]
    UnsupportedVersion {
        /// The file that failed to load.
        path: PathBuf,
        /// The version found in the file.
        version: u64,
        /// The version this build writes.
        supported: u32,
    },

    /// The snapshot could not be encoded.
    #[error("cannot encode session state for `{path}`: {reason}")]
    Encode {
        /// The target file.
        path: PathBuf,
        /// The encoder error.
        reason: String,
    },

    /// The blocking task that performs the file work did not complete.
    #[error("session state task for `{path}` failed: {reason}")]
    Task {
        /// The file the task worked on.
        path: PathBuf,
        /// Why the task ended.
        reason: String,
    },
}

/// A snapshot file on disk.
///
/// The store is just a path: it holds no open handle, so several calls can
/// interleave as long as the caller serialises [`save`](Self::save).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// Uses an explicit file path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Uses the default path under a download directory:
    /// `<dir>/.fluxd-state.json`.
    pub fn for_download_dir(dir: impl AsRef<Path>) -> Self {
        Self::new(dir.as_ref().join(STATE_FILE_NAME))
    }

    /// The snapshot file this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the snapshot.
    ///
    /// A missing file yields an empty, current-version snapshot. Everything
    /// else is an error; see the module documentation.
    pub fn load(&self) -> Result<SessionSnapshot, StateError> {
        load(&self.path)
    }

    /// Writes the snapshot atomically.
    ///
    /// Creates the parent directory if needed, writes and flushes
    /// `<name>.tmp`, then renames it over the target.
    pub fn save(&self, snapshot: &SessionSnapshot) -> Result<(), StateError> {
        save(&self.path, snapshot)
    }

    /// [`load`](Self::load) on the blocking pool.
    pub async fn load_async(&self) -> Result<SessionSnapshot, StateError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || load(&path))
            .await
            .map_err(|error| StateError::Task {
                path: self.path.clone(),
                reason: error.to_string(),
            })?
    }

    /// [`save`](Self::save) on the blocking pool.
    pub async fn save_async(&self, snapshot: &SessionSnapshot) -> Result<(), StateError> {
        let path = self.path.clone();
        let snapshot = snapshot.clone();
        tokio::task::spawn_blocking(move || save(&path, &snapshot))
            .await
            .map_err(|error| StateError::Task {
                path: self.path.clone(),
                reason: error.to_string(),
            })?
    }
}

fn load(path: &Path) -> Result<SessionSnapshot, StateError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(SessionSnapshot::default());
        }
        Err(error) => {
            return Err(StateError::Read {
                path: path.to_owned(),
                source: error,
            });
        }
    };

    // Read the version before the full document so a file from a future build
    // reports `UnsupportedVersion` even when its shape changed.
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| StateError::Corrupt {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| StateError::Corrupt {
            path: path.to_owned(),
            reason: "missing or non-integer `version`".to_owned(),
        })?;
    if version != u64::from(STATE_VERSION) {
        return Err(StateError::UnsupportedVersion {
            path: path.to_owned(),
            version,
            supported: STATE_VERSION,
        });
    }

    let snapshot: SessionSnapshot =
        serde_json::from_value(value).map_err(|error| StateError::Corrupt {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    validate(path, &snapshot)?;
    Ok(snapshot)
}

fn save(path: &Path, snapshot: &SessionSnapshot) -> Result<(), StateError> {
    validate(path, snapshot)?;

    let mut bytes = serde_json::to_vec_pretty(snapshot).map_err(|error| StateError::Encode {
        path: path.to_owned(),
        reason: error.to_string(),
    })?;
    bytes.push(b'\n');

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| StateError::Write {
            path: path.to_owned(),
            source: error,
        })?;
    }

    let tmp = temporary_path(path);
    let result = write_and_replace(path, &tmp, &bytes);
    if result.is_err() {
        // Never leave a half-written file behind. If the removal itself fails
        // the original target is still intact, which is what matters.
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_and_replace(path: &Path, tmp: &Path, bytes: &[u8]) -> Result<(), StateError> {
    let write_error = |source: io::Error| StateError::Write {
        path: path.to_owned(),
        source,
    };

    let mut file = File::create(tmp).map_err(write_error)?;
    file.write_all(bytes).map_err(write_error)?;
    file.sync_all().map_err(write_error)?;
    drop(file);
    fs::rename(tmp, path).map_err(write_error)
}

/// `<name>.tmp` next to the target, so the rename stays on one filesystem.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| OsString::from(STATE_FILE_NAME), |name| name.to_owned());
    name.push(".tmp");
    path.with_file_name(name)
}

fn validate(path: &Path, snapshot: &SessionSnapshot) -> Result<(), StateError> {
    let corrupt = |reason: String| StateError::Corrupt {
        path: path.to_owned(),
        reason,
    };

    for record in &snapshot.torrents {
        if record.info_hash.len() != 40
            || !record
                .info_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(corrupt(format!(
                "torrent info hash `{}` is not 40 hex characters",
                record.info_hash
            )));
        }
        if record.metainfo.is_empty() {
            return Err(corrupt(format!(
                "torrent `{}` has no metainfo bytes",
                record.info_hash
            )));
        }
        if record.priorities.len() > MAX_PRIORITIES {
            return Err(corrupt(format!(
                "torrent `{}` has {} priorities, more than the {MAX_PRIORITIES} limit",
                record.info_hash,
                record.priorities.len()
            )));
        }
    }
    Ok(())
}

mod hex_bytes {
    use serde::Deserialize;
    use serde::de::{self, Deserializer};
    use serde::ser::Serializer;

    use crate::id::to_hex;

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&to_hex(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        decode_hex(&text).map_err(de::Error::custom)
    }

    fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
        if !text.len().is_multiple_of(2) {
            return Err(format!(
                "hex string has an odd length ({} characters)",
                text.len()
            ));
        }
        let mut out = Vec::with_capacity(text.len() / 2);
        for (index, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let high = nibble(pair[0]).ok_or_else(|| {
                format!(
                    "invalid hex byte `{}` at offset {}",
                    text_lossy(pair),
                    index * 2
                )
            })?;
            let low = nibble(pair[1]).ok_or_else(|| {
                format!(
                    "invalid hex byte `{}` at offset {}",
                    text_lossy(pair),
                    index * 2 + 1
                )
            })?;
            out.push((high << 4) | low);
        }
        Ok(out)
    }

    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    fn text_lossy(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn record(hash: &str, metainfo: &[u8], priorities: &[u8]) -> TorrentRecord {
        TorrentRecord {
            info_hash: hash.to_owned(),
            metainfo: metainfo.to_vec(),
            priorities: priorities.to_vec(),
            paused: false,
            download_dir: None,
        }
    }

    fn two_torrents() -> SessionSnapshot {
        SessionSnapshot {
            version: STATE_VERSION,
            torrents: vec![
                TorrentRecord {
                    info_hash: "ab".repeat(20),
                    metainfo: vec![0x64, 0x38, 0x3a, 0x00, 0xff],
                    priorities: vec![0, 1, 2, 1],
                    paused: false,
                    download_dir: Some("D:/downloads/media".to_owned()),
                },
                TorrentRecord {
                    info_hash: "cd".repeat(20),
                    metainfo: (0..32).collect(),
                    priorities: Vec::new(),
                    paused: true,
                    download_dir: None,
                },
            ],
        }
    }

    #[test]
    fn the_default_path_is_beside_the_downloads() {
        let store = StateStore::for_download_dir("D:/downloads");
        assert_eq!(store.path(), Path::new("D:/downloads/.fluxd-state.json"));
    }

    #[test]
    fn round_trips_two_torrents_and_priorities() {
        let dir = tempdir().unwrap();
        let store = StateStore::for_download_dir(dir.path());
        let snapshot = two_torrents();

        store.save(&snapshot).unwrap();
        assert_eq!(store.load().unwrap(), snapshot);

        // Metainfo is hex in the JSON, and priorities are plain numbers.
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(
            text.contains("\"metainfo\": \"64383a00ff\""),
            "unexpected JSON: {text}"
        );
        assert!(
            text.contains("\"priorities\": ["),
            "unexpected JSON: {text}"
        );
        assert!(
            text.starts_with("{\n  \"version\": 1,"),
            "unexpected JSON: {text}"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_session() {
        let dir = tempdir().unwrap();
        let store = StateStore::for_download_dir(dir.path().join("nothing-here"));
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.version, STATE_VERSION);
        assert!(snapshot.torrents.is_empty());
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let dir = tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        fs::write(store.path(), br#"{"version":2,"torrents":[]}"#).unwrap();

        match store.load().unwrap_err() {
            StateError::UnsupportedVersion {
                path,
                version,
                supported,
            } => {
                assert_eq!(path, store.path());
                assert_eq!(version, 2);
                assert_eq!(supported, STATE_VERSION);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn truncated_json_names_the_path() {
        let dir = tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        fs::write(store.path(), br#"{"version":1,"torrents":["#).unwrap();

        let error = store.load().unwrap_err();
        assert!(matches!(error, StateError::Corrupt { .. }), "{error}");
        assert!(error.to_string().contains("state.json"), "{error}");
    }

    #[test]
    fn bad_hex_and_bad_hashes_are_refused() {
        let dir = tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));

        fs::write(
            store.path(),
            br#"{"version":1,"torrents":[{"info_hash":"abababababababababababababababababababab","metainfo":"zz","priorities":[],"paused":false,"download_dir":null}]}"#,
        )
        .unwrap();
        assert!(matches!(store.load(), Err(StateError::Corrupt { .. })));

        // A short info hash is corrupt even when the rest is fine.
        fs::write(
            store.path(),
            br#"{"version":1,"torrents":[{"info_hash":"abab","metainfo":"00","priorities":[],"paused":false,"download_dir":null}]}"#,
        )
        .unwrap();
        assert!(matches!(store.load(), Err(StateError::Corrupt { .. })));
    }

    #[test]
    fn absurd_priority_counts_are_refused() {
        let snapshot = SessionSnapshot {
            version: STATE_VERSION,
            torrents: vec![record(&"ab".repeat(20), &[1, 2, 3], &[])],
        };
        let path = Path::new("state.json");
        assert!(validate(path, &snapshot).is_ok());

        let oversized = SessionSnapshot {
            version: STATE_VERSION,
            torrents: vec![TorrentRecord {
                priorities: vec![1; MAX_PRIORITIES + 1],
                ..snapshot.torrents[0].clone()
            }],
        };
        assert!(matches!(
            validate(path, &oversized),
            Err(StateError::Corrupt { .. })
        ));
    }

    #[test]
    fn saving_twice_leaves_one_file_and_no_temporary() {
        let dir = tempdir().unwrap();
        let store = StateStore::for_download_dir(dir.path());
        let snapshot = two_torrents();

        store.save(&snapshot).unwrap();
        store.save(&snapshot).unwrap();

        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![STATE_FILE_NAME.to_owned()]);
        assert!(!dir.path().join(format!("{STATE_FILE_NAME}.tmp")).exists());
    }

    #[test]
    fn a_failed_replace_leaves_no_temporary_file() {
        let dir = tempdir().unwrap();
        // Renaming a file over an existing directory fails on every platform,
        // which exercises the cleanup path without touching permissions.
        let target = dir.path().join("state.json");
        fs::create_dir(&target).unwrap();
        let store = StateStore::new(&target);

        let error = store.save(&two_torrents()).unwrap_err();
        assert!(matches!(error, StateError::Write { .. }), "{error}");
        assert!(target.is_dir(), "the previous entry must survive");
        assert!(!dir.path().join("state.json.tmp").exists());
    }

    #[test]
    fn a_large_snapshot_round_trips() {
        let dir = tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        // 2 MiB of pseudo-random-looking bytes: the hex encoding doubles it.
        let metainfo: Vec<u8> = (0..2 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect();
        let snapshot = SessionSnapshot {
            version: STATE_VERSION,
            torrents: vec![record(&"ef".repeat(20), &metainfo, &[1, 1, 1])],
        };

        store.save(&snapshot).unwrap();
        assert_eq!(store.load().unwrap(), snapshot);

        let file_len = fs::metadata(store.path()).unwrap().len();
        assert!(
            file_len > 2 * metainfo.len() as u64,
            "hex encoding should roughly double the payload: {file_len}"
        );
    }

    #[test]
    fn json_keeps_metainfo_readable() {
        let dir = tempdir().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));
        store
            .save(&SessionSnapshot {
                version: STATE_VERSION,
                torrents: vec![record(&"aa".repeat(20), b"hello", &[2])],
            })
            .unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("68656c6c6f"), "{text}");
    }
}
