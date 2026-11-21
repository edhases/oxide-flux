//! Daemon configuration.
//!
//! The file is TOML. Every field is optional; a missing field takes its
//! [`Config::default`] value, so a minimal config only has to name what differs.
//!
//! ```toml
//! download_dir = "D:/media/downloads"
//! max_active_downloads = 4
//! max_active_seeds = 8
//! preallocate = "sparse"        # "none" | "sparse" | "zeroed"
//! listen_port = 6881
//! api_bind = "127.0.0.1:6800"
//! api_token = "..."             # never logged, never printed
//! worker_threads = 4
//! verifier_threads = 2
//! ```

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use flux_disk::Allocation;
use serde::{Deserialize, Serialize};

/// Default number of incomplete torrents that may hold a download slot.
pub const DEFAULT_MAX_ACTIVE_DOWNLOADS: usize = 3;
/// Default number of complete torrents that may hold a seed slot.
pub const DEFAULT_MAX_ACTIVE_SEEDS: usize = 5;
/// Default listen port. Stored only: this build opens no sockets.
pub const DEFAULT_LISTEN_PORT: u16 = 6881;
/// Default peer connections kept open per torrent.
pub const DEFAULT_MAX_PEERS: usize = 50;
/// Default number of peers we upload to at the same time.
pub const DEFAULT_MAX_UPLOAD_SLOTS: usize = 4;
/// Mainline DHT routers used for the first bootstrap.
pub const DEFAULT_DHT_BOOTSTRAP: [&str; 3] = [
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "router.utorrent.com:6881",
];
/// Default control-API bind address. Stored only: this build serves no API.
pub const DEFAULT_API_BIND: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 6800);

/// How newly added files are prepared on disk.
///
/// Mirrors [`flux_disk::Allocation`] but stays serialisable and independent of the
/// storage crate's public enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preallocate {
    /// Create the files but do not grow them. Existing bytes are left alone.
    ///
    /// Verification reports every piece that cannot be read as missing rather than
    /// corrupt, so this mode is honest but cannot pre-reserve space.
    None,
    /// Grow each file to its declared length with a sparse `set_len`. Fast, and the
    /// bytes read back as zeroes.
    #[default]
    Sparse,
    /// Grow and explicitly zero each file, committing the blocks to the filesystem.
    /// Slow, but a player can mmap a file immediately.
    Zeroed,
}

impl Preallocate {
    /// The storage-layer allocation mode this maps to.
    pub fn allocation(self) -> Allocation {
        match self {
            Self::None => Allocation::None,
            Self::Sparse => Allocation::Sparse,
            Self::Zeroed => Allocation::Zeroed,
        }
    }
}

impl fmt::Display for Preallocate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Sparse => "sparse",
            Self::Zeroed => "zeroed",
        })
    }
}

/// Daemon configuration.
///
/// The token is never part of the [`fmt::Debug`] output and never appears in
/// [`Config::summary`], so it cannot leak through a log line.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Root directory every torrent is written under.
    pub download_dir: PathBuf,
    /// Incomplete torrents that may hold a download slot at the same time.
    pub max_active_downloads: usize,
    /// Complete torrents that may hold a seed slot at the same time.
    pub max_active_seeds: usize,
    /// How files are prepared when a torrent is added.
    pub preallocate: Preallocate,
    /// TCP/uTP listen port for inbound peers.
    pub listen_port: u16,
    /// Peer connections kept open per torrent.
    pub max_peers_per_torrent: usize,
    /// Peers we upload to concurrently (tit-for-tat slots).
    pub max_upload_slots: usize,
    /// Whether trackerless peer discovery may run.
    pub enable_dht: bool,
    /// DHT bootstrap routers, `host:port`. Resolved at startup.
    pub dht_bootstrap: Vec<String>,
    /// Persist the session across restarts.
    pub persist: bool,
    /// Snapshot path. `None` means `<download_dir>/.fluxd-state.json`.
    pub state_file: Option<PathBuf>,
    /// Control API bind address. Stored for the future API layer; unused today.
    pub api_bind: SocketAddr,
    /// Bearer token for the control API. `None` means no authentication.
    pub api_token: Option<String>,
    /// Tokio worker threads used by the daemon runtime.
    pub worker_threads: usize,
    /// Hashing threads each verification run may use.
    pub verifier_threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: PathBuf::from("downloads"),
            max_active_downloads: DEFAULT_MAX_ACTIVE_DOWNLOADS,
            max_active_seeds: DEFAULT_MAX_ACTIVE_SEEDS,
            preallocate: Preallocate::Sparse,
            listen_port: DEFAULT_LISTEN_PORT,
            max_peers_per_torrent: DEFAULT_MAX_PEERS,
            max_upload_slots: DEFAULT_MAX_UPLOAD_SLOTS,
            enable_dht: true,
            dht_bootstrap: DEFAULT_DHT_BOOTSTRAP
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            persist: true,
            state_file: None,
            api_bind: DEFAULT_API_BIND,
            api_token: None,
            worker_threads: default_threads(2, 8),
            verifier_threads: default_threads(1, 8),
        }
    }
}

fn default_threads(minimum: usize, maximum: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(minimum, |count| count.get())
        .clamp(minimum, maximum)
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("download_dir", &self.download_dir)
            .field("max_active_downloads", &self.max_active_downloads)
            .field("max_active_seeds", &self.max_active_seeds)
            .field("preallocate", &self.preallocate)
            .field("listen_port", &self.listen_port)
            .field("max_peers_per_torrent", &self.max_peers_per_torrent)
            .field("max_upload_slots", &self.max_upload_slots)
            .field("dht", &self.enable_dht)
            .field("api_bind", &self.api_bind)
            // Presence is useful; the value is not, and must never reach a log.
            .field(
                "api_token",
                &self
                    .api_token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("none"),
            )
            .field("worker_threads", &self.worker_threads)
            .field("verifier_threads", &self.verifier_threads)
            .finish()
    }
}

impl Config {
    /// Reads and validates a TOML config file.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Read`] for an unreadable file, [`ConfigError::Parse`]
    /// for invalid TOML or an unknown field, and any [`Config::validate`] error.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|error| match error {
            ConfigError::ParseText { source, .. } => ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })
    }

    /// Parses and validates TOML text.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ParseText`] for malformed TOML and any
    /// [`Config::validate`] error.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self =
            toml::from_str(text).map_err(|source| ConfigError::ParseText { source })?;
        config.validate()?;
        Ok(config)
    }

    /// Checks the values that cannot be repaired at runtime.
    ///
    /// # Errors
    ///
    /// * [`ConfigError::EmptyDownloadDir`] if `download_dir` is empty.
    /// * [`ConfigError::DownloadDirNotADirectory`] if it exists and is a file.
    /// * [`ConfigError::ZeroLimit`] for a zero queue limit or thread count.
    /// * [`ConfigError::EmptyToken`] for `api_token = ""`, which would silently
    ///   mean "no authentication".
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.download_dir.as_os_str().is_empty() {
            return Err(ConfigError::EmptyDownloadDir);
        }
        if self.download_dir.exists() && !self.download_dir.is_dir() {
            return Err(ConfigError::DownloadDirNotADirectory {
                path: self.download_dir.clone(),
            });
        }
        if self.max_active_downloads == 0 {
            return Err(ConfigError::ZeroLimit {
                field: "max_active_downloads",
            });
        }
        if self.max_active_seeds == 0 {
            return Err(ConfigError::ZeroLimit {
                field: "max_active_seeds",
            });
        }
        if self.worker_threads == 0 {
            return Err(ConfigError::ZeroLimit {
                field: "worker_threads",
            });
        }
        if self.verifier_threads == 0 {
            return Err(ConfigError::ZeroLimit {
                field: "verifier_threads",
            });
        }
        if self.max_peers_per_torrent == 0 {
            return Err(ConfigError::ZeroLimit {
                field: "max_peers_per_torrent",
            });
        }
        if self.max_peers_per_torrent > 500 {
            return Err(ConfigError::LimitTooLarge {
                field: "max_peers_per_torrent",
                max: 500,
                value: self.max_peers_per_torrent,
            });
        }
        if self.max_upload_slots == 0 || self.max_upload_slots > 64 {
            return Err(ConfigError::LimitTooLarge {
                field: "max_upload_slots",
                max: 64,
                value: self.max_upload_slots,
            });
        }
        if self.persist
            && self
                .state_file
                .as_deref()
                .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigError::EmptyDownloadDir);
        }
        if self.api_token.as_deref().is_some_and(str::is_empty) {
            return Err(ConfigError::EmptyToken);
        }
        Ok(())
    }

    /// Creates `download_dir` if it is missing. Blocking: call from a blocking
    /// context, not from the reactor.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::CreateDownloadDir`] if the directory cannot be made.
    pub fn ensure_download_dir(&self) -> Result<&Path, ConfigError> {
        std::fs::create_dir_all(&self.download_dir).map_err(|source| {
            ConfigError::CreateDownloadDir {
                path: self.download_dir.clone(),
                source,
            }
        })?;
        Ok(&self.download_dir)
    }

    /// A one-line, token-free description for logs and CLI output.
    pub fn summary(&self) -> String {
        format!(
            "download_dir={} max_active_downloads={} max_active_seeds={} preallocate={} listen_port={} max_peers={} upload_slots={} dht={} persist={} api_bind={} auth={}",
            self.download_dir.display(),
            self.max_active_downloads,
            self.max_active_seeds,
            self.preallocate,
            self.listen_port,
            self.max_peers_per_torrent,
            self.max_upload_slots,
            self.enable_dht,
            self.persist,
            self.api_bind,
            if self.api_token.is_some() {
                "token"
            } else {
                "none"
            }
        )
    }
}

/// A configuration failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("cannot read config `{}`: {source}", .path.display())]
    Read {
        /// Path that was read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The config file could not be parsed.
    #[error("cannot parse config `{}`: {source}", .path.display())]
    Parse {
        /// Path that was parsed.
        path: PathBuf,
        /// TOML syntax or type error.
        #[source]
        source: toml::de::Error,
    },
    /// Text was not valid TOML for [`Config`].
    #[error("cannot parse config: {source}")]
    ParseText {
        /// TOML syntax or type error.
        #[source]
        source: toml::de::Error,
    },
    /// `download_dir` was empty.
    #[error("`download_dir` must not be empty")]
    EmptyDownloadDir,
    /// `download_dir` names something that is not a directory.
    #[error("`download_dir` `{}` exists but is not a directory", .path.display())]
    DownloadDirNotADirectory {
        /// The offending path.
        path: PathBuf,
    },
    /// `download_dir` could not be created.
    #[error("cannot create `download_dir` `{}`: {source}", .path.display())]
    CreateDownloadDir {
        /// The offending path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A limit was zero, which would stall every torrent forever.
    #[error("`{field}` must be greater than zero")]
    ZeroLimit {
        /// The offending field.
        field: &'static str,
    },
    /// A limit was above the supported maximum.
    #[error("`{field}` is {value}, above the supported maximum {max}")]
    LimitTooLarge {
        /// The offending field.
        field: &'static str,
        /// Maximum accepted value.
        max: usize,
        /// Value that was supplied.
        value: usize,
    },
    /// An empty token would look like authentication but mean none.
    #[error("`api_token` must not be empty when set")]
    EmptyToken,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let config = Config::default();
        config.validate().unwrap();
        assert_eq!(config.preallocate, Preallocate::Sparse);
        assert_eq!(config.preallocate.allocation(), Allocation::Sparse);
        assert!(config.download_dir.is_relative());
    }

    #[test]
    fn partial_toml_keeps_defaults() {
        let config = Config::parse(
            r#"
            download_dir = "media"
            max_active_downloads = 1
            preallocate = "zeroed"
            "#,
        )
        .unwrap();
        assert_eq!(config.download_dir, PathBuf::from("media"));
        assert_eq!(config.max_active_downloads, 1);
        assert_eq!(config.max_active_seeds, DEFAULT_MAX_ACTIVE_SEEDS);
        assert_eq!(config.preallocate, Preallocate::Zeroed);
        assert_eq!(config.api_bind, DEFAULT_API_BIND);

        // An empty document is a valid config: everything takes its default.
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn rejects_bad_values_and_unknown_keys() {
        for text in [
            r#"max_active_downloads = 0"#,
            r#"max_active_seeds = 0"#,
            r#"worker_threads = 0"#,
            r#"verifier_threads = 0"#,
            r#"download_dir = """#,
            r#"api_token = """#,
            r#"api_token = 7"#,
            r#"listen_port = "6881""#,
            r#"api_bind = "not-an-address""#,
            r#"nonsense = 1"#,
            r#"preallocate = "sometimes""#,
        ] {
            assert!(Config::parse(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn token_never_appears_in_debug_or_summary() {
        let config = Config {
            api_token: Some("hunter2".to_owned()),
            ..Config::default()
        };
        assert!(!format!("{config:?}").contains("hunter2"));
        assert!(!config.summary().contains("hunter2"));
        assert!(format!("{config:?}").contains("redacted"));
        assert!(config.summary().contains("auth=token"));
    }
}
