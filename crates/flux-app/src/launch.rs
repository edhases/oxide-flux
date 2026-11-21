//! Startup: argument parsing, settings, file associations and single-instance
//! handoff. Keep this synchronous so it can run before iced starts.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::platform;
use crate::single_instance::{self, OpenRequest, RequestQueue};

/// Help text for `--help`.
pub const USAGE: &str = "\
Oxide Flux - streaming BitTorrent client

USAGE:
    oxide-flux [OPTIONS] [MAGNET|TORRENT]

OPTIONS:
    --register      Register .torrent and magnet: associations (current user)
    --unregister    Remove the associations this client created
    --no-register   Do not register associations, now or on later runs
    -h, --help      Print this help
    -V, --version   Print the version

Without a target, the window opens normally. With one, the link opens in the
window that is already running (or starts one).
";

/// Parsed command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Options {
    /// A magnet URI or `.torrent` path.
    pub target: Option<String>,
    /// Explicitly register associations and exit.
    pub register: bool,
    /// Explicitly remove associations and exit.
    pub unregister: bool,
    /// Turn automatic registration off and remember it.
    pub no_register: bool,
    /// Print help and exit.
    pub help: bool,
    /// Print the version and exit.
    pub version: bool,
}

/// Which playback engine to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackBackend {
    /// `playback/mpv/mpv.exe` next to the application, falling back to `PATH`.
    #[default]
    Bundled,
    /// A system-installed mpv (`mpv.exe`/`mpv` on `PATH`, or `OXIDE_FLUX_MPV`).
    System,
    /// An explicit executable path from [`PlaybackSettings::mpv_path`].
    Custom,
    /// Do not play media at all.
    Disabled,
}

impl PlaybackBackend {
    /// Every option, in menu order.
    pub const ALL: [Self; 4] = [Self::Bundled, Self::System, Self::Custom, Self::Disabled];

    /// Display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Bundled => "Bundled mpv",
            Self::System => "System mpv",
            Self::Custom => "Custom path",
            Self::Disabled => "Disabled",
        }
    }
}

impl std::fmt::Display for PlaybackBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Playback-related settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackSettings {
    /// Selected backend.
    pub backend: PlaybackBackend,
    /// Explicit mpv path for [`PlaybackBackend::Custom`].
    pub mpv_path: Option<PathBuf>,
}

/// Persistent desktop settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Register `.torrent`/`magnet:` associations on startup.
    pub register_associations: bool,
    /// Playback engine choice.
    pub playback: PlaybackSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            register_associations: true,
            playback: PlaybackSettings::default(),
        }
    }
}

impl Settings {
    /// Reads settings, falling back to defaults when the file is missing or
    /// unreadable. A corrupt file is replaced on the next save.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes settings atomically enough for a small file.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O or serialization error.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        let payload = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&temporary, payload)?;
        fs::rename(temporary, path)
    }
}

/// What the UI should do at boot.
#[derive(Debug, Clone, Default)]
pub struct Initial {
    /// Links/files that must be opened after the window appears.
    pub requests: Vec<OpenRequest>,
    /// Problems to show in the error banner (associations, state directory).
    pub notices: Vec<String>,
    /// Queue for links received while the window is open.
    pub queue: Option<RequestQueue>,
    /// Persisted desktop settings.
    pub settings: Settings,
}

/// Result of [`prepare`].
#[derive(Debug)]
pub enum Plan {
    /// Start the GUI.
    Run(Initial),
    /// The process is done; exit with this status.
    Exit(i32),
}

/// Parses arguments without touching the OS.
///
/// # Errors
///
/// Returns a message for unknown flags, conflicting flags, or more than one
/// target.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let mut options = Options::default();
    for argument in args {
        let text = argument.to_string_lossy().into_owned();
        match text.as_str() {
            "--register" => options.register = true,
            "--unregister" => options.unregister = true,
            "--no-register" => options.no_register = true,
            "-h" | "--help" => options.help = true,
            "-V" | "--version" => options.version = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`"));
            }
            other => {
                if options.target.is_some() {
                    return Err("only one magnet link or torrent file at a time".to_owned());
                }
                options.target = Some(other.to_owned());
            }
        }
    }
    if options.register && options.unregister {
        return Err("`--register` and `--unregister` cannot be combined".to_owned());
    }
    Ok(options)
}

/// `%APPDATA%\OxideFlux` on Windows, `$XDG_CONFIG_HOME/oxide-flux` elsewhere.
#[must_use]
pub fn config_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join(if cfg!(windows) {
            "OxideFlux"
        } else {
            "oxide-flux"
        })
}

fn parse_target(target: &str) -> Result<OpenRequest, String> {
    if target.to_ascii_lowercase().starts_with("magnet:") {
        return Ok(OpenRequest::Magnet(target.to_owned()));
    }
    let path = PathBuf::from(target);
    if !path.is_file() {
        return Err(format!("`{}` is not a file", path.display()));
    }
    Ok(OpenRequest::Torrent(path))
}

/// Runs every pre-GUI step and returns either the GUI boot state or an exit code.
///
/// # Errors
///
/// Returns a message for an invalid command line or an unusable target.
pub fn prepare(args: impl IntoIterator<Item = OsString>) -> Result<Plan, String> {
    let options = parse(args)?;

    if options.help {
        print!("{USAGE}");
        return Ok(Plan::Exit(0));
    }
    if options.version {
        println!("oxide-flux {}", env!("CARGO_PKG_VERSION"));
        return Ok(Plan::Exit(0));
    }
    if options.register {
        return match platform::register() {
            Ok(report) => {
                println!("registered magnet: links for this user");
                if report.torrent {
                    println!("registered .torrent files for this user");
                } else {
                    println!(
                        ".torrent stays associated with {}; choose Oxide Flux in Windows Default apps",
                        report
                            .torrent_owner
                            .as_deref()
                            .unwrap_or("another application")
                    );
                }
                Ok(Plan::Exit(0))
            }
            Err(error) => {
                eprintln!("oxide-flux: cannot register associations: {error}");
                Ok(Plan::Exit(1))
            }
        };
    }
    if options.unregister {
        return match platform::unregister() {
            Ok(()) => {
                println!("removed the associations created by Oxide Flux");
                Ok(Plan::Exit(0))
            }
            Err(error) => {
                eprintln!("oxide-flux: cannot remove associations: {error}");
                Ok(Plan::Exit(1))
            }
        };
    }

    let mut notices = Vec::new();
    let settings_path = config_dir().join("settings.json");
    let mut settings = Settings::load(&settings_path);
    if options.no_register {
        settings.register_associations = false;
        if let Err(error) = settings.save(&settings_path) {
            notices.push(format!("cannot save settings: {error}"));
        }
    }
    if settings.register_associations {
        match platform::register() {
            Ok(report) => {
                if !report.torrent {
                    notices.push(format!(
                        ".torrent is associated with {}; pick Oxide Flux in Windows Default apps",
                        report
                            .torrent_owner
                            .as_deref()
                            .unwrap_or("another application")
                    ));
                }
            }
            Err(error) => notices.push(format!("file associations: {error}")),
        }
    }

    let state_root = config_dir().join("instance");
    let target = match options.target.as_deref() {
        Some(target) => Some(parse_target(target)?),
        None => None,
    };

    match single_instance::claim(&state_root) {
        Ok(None) => {
            // Another instance owns the window: hand the target over and leave.
            if let Some(request) = target {
                let queue = RequestQueue::new(state_root.join("requests"));
                queue.enqueue(&request).map_err(|error| {
                    format!("cannot hand the link to the running window: {error}")
                })?;
            }
            Ok(Plan::Exit(0))
        }
        Ok(Some(instance)) => {
            let mut initial = Initial {
                queue: Some(instance.queue().clone()),
                notices,
                settings: settings.clone(),
                ..Initial::default()
            };
            match instance.queue().drain() {
                Ok(requests) => initial.requests = requests,
                Err(_) => initial.requests = Vec::new(),
            }
            if let Some(request) = target {
                initial.requests.push(request);
            }
            Ok(Plan::Run(initial))
        }
        Err(error) => {
            notices.push(format!("single-instance coordination unavailable: {error}"));
            Ok(Plan::Run(Initial {
                requests: target.into_iter().collect(),
                notices,
                queue: None,
                settings: settings.clone(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn parse_str(args: &[&str]) -> Result<Options, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn flags_and_target_are_parsed() {
        let options = parse_str(&["--no-register", "magnet:?xt=urn:btih:abc"]).unwrap();
        assert!(options.no_register);
        assert_eq!(options.target.as_deref(), Some("magnet:?xt=urn:btih:abc"));

        let options = parse_str(&["--register"]).unwrap();
        assert!(options.register);
        assert!(options.target.is_none());
    }

    #[test]
    fn conflicting_or_unknown_flags_are_rejected() {
        assert!(parse_str(&["--register", "--unregister"]).is_err());
        assert!(parse_str(&["--wat"]).is_err());
        assert!(parse_str(&["a.torrent", "b.torrent"]).is_err());
    }

    #[test]
    fn settings_default_to_associations_on() {
        assert!(Settings::default().register_associations);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            register_associations: false,
            playback: PlaybackSettings::default(),
        };
        settings.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert!(!loaded.register_associations);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn magnet_targets_do_not_touch_the_filesystem() {
        assert!(matches!(
            parse_target("magnet:?xt=urn:btih:abc").unwrap(),
            OpenRequest::Magnet(_)
        ));
        assert!(parse_target("does-not-exist.torrent").is_err());
    }
}
