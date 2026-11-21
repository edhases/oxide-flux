//! Optional playback-engine plugin.
//!
//! The application itself does not decode media: mpv does. This module installs
//! a pinned, checksum-verified GPL mpv build into the per-user config directory
//! on request from Settings, so the installer can stay small and users who
//! already have mpv can keep using it.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// The bundled upstream build we install.
pub const MPV_VERSION: &str = "20261111";
/// Expected download size (shown in the UI, not enforced).
pub const MPV_ARCHIVE_BYTES: u64 = 34_181_485;

const MPV_URL: &str = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261111/mpv-x86_64-20261111-git-bcde6b93cc.7z";
/// SHA-256 of that exact archive, as published by the GitHub release digest.
const MPV_SHA256: &str = "f259b5fd4b5c395617c948a4fadcdf55bba1c5282fb7c13e88626b3f71072baf";

/// Something went wrong installing the playback plugin.
#[derive(Debug, Error)]
pub enum PluginError {
    /// The download failed.
    #[error("cannot download mpv: {0}")]
    Download(String),
    /// The archive did not match the pinned checksum.
    #[error("mpv archive checksum mismatch: expected {expected}, got {actual}")]
    Checksum {
        /// Pinned digest.
        expected: String,
        /// Digest of the downloaded bytes.
        actual: String,
    },
    /// The archive could not be unpacked.
    #[error("cannot unpack mpv: {0}")]
    Extract(String),
    /// A filesystem operation failed.
    #[error("i/o error: {0}")]
    Io(String),
    /// The archive unpacked but did not contain `mpv.exe`.
    #[error("the mpv archive did not contain mpv.exe")]
    MissingExecutable,
}

/// Where the in-app plugin installs mpv.
#[must_use]
pub fn plugin_dir() -> PathBuf {
    crate::launch::config_dir().join("playback").join("mpv")
}

/// Bundled mpv locations, in priority order.
///
/// The NSIS installer may place mpv next to the executable as an optional
/// component; the in-app plugin installs under the user's config directory.
#[must_use]
pub fn bundled_mpv() -> Option<PathBuf> {
    let names: [&str; 2] = if cfg!(windows) {
        ["mpv.exe", "mpv"]
    } else {
        ["mpv", "mpv.exe"]
    };
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        for name in names {
            candidates.push(directory.join("playback").join("mpv").join(name));
        }
    }
    for name in names {
        candidates.push(plugin_dir().join(name));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// The installed plugin version, if the VERSION marker exists.
#[must_use]
pub fn installed_version() -> Option<String> {
    let version = fs::read_to_string(plugin_dir().join("VERSION")).ok()?;
    let version = version.trim();
    (!version.is_empty()).then(|| version.to_owned())
}

/// Downloads, verifies and installs the pinned mpv build.
///
/// Blocking: call from `spawn_blocking`.
///
/// # Errors
///
/// [`PluginError`] for a failed download, checksum mismatch, unpack failure or
/// a missing executable.
pub fn install() -> Result<PathBuf, PluginError> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("OxideFlux/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| PluginError::Download(error.to_string()))?;
    let bytes = client
        .get(MPV_URL)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| PluginError::Download(error.to_string()))?
        .bytes()
        .map_err(|error| PluginError::Download(error.to_string()))?;

    let digest = Sha256::digest(&bytes);
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != MPV_SHA256 {
        return Err(PluginError::Checksum {
            expected: MPV_SHA256.to_owned(),
            actual,
        });
    }

    let archive = std::env::temp_dir().join(format!("oxide-flux-mpv-{}.7z", std::process::id()));
    fs::write(&archive, &bytes).map_err(|error| PluginError::Io(error.to_string()))?;

    let target = plugin_dir();
    let staging = target.with_file_name("mpv.tmp");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|error| PluginError::Io(error.to_string()))?;
    let extracted = sevenz_rust2::decompress_file(&archive, &staging);
    let _ = fs::remove_file(&archive);
    extracted.map_err(|error| PluginError::Extract(error.to_string()))?;

    if !staging.join(executable_name()).is_file() {
        let _ = fs::remove_dir_all(&staging);
        return Err(PluginError::MissingExecutable);
    }

    let _ = fs::remove_dir_all(&target);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| PluginError::Io(error.to_string()))?;
    }
    fs::rename(&staging, &target).map_err(|error| PluginError::Io(error.to_string()))?;
    fs::write(target.join("VERSION"), MPV_VERSION)
        .map_err(|error| PluginError::Io(error.to_string()))?;

    Ok(target.join(executable_name()))
}

/// Removes the in-app plugin installation.
///
/// # Errors
///
/// [`PluginError::Io`] when the directory exists but cannot be removed.
pub fn remove() -> Result<(), PluginError> {
    match fs::remove_dir_all(plugin_dir()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PluginError::Io(error.to_string())),
    }
}

fn executable_name() -> &'static str {
    if cfg!(windows) { "mpv.exe" } else { "mpv" }
}

/// Human-readable size of the pinned archive.
#[must_use]
pub fn download_size() -> String {
    format!("{:.0} MB", MPV_ARCHIVE_BYTES as f64 / (1024.0 * 1024.0))
}

/// Whether `path` looks like an mpv executable.
#[must_use]
pub fn is_mpv(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("mpv.exe") || name == "mpv")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn download_size_is_human_readable() {
        assert!(download_size().contains("MB"));
    }

    #[test]
    fn mpv_detection_accepts_only_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("mpv.exe");
        assert!(!is_mpv(&exe));
        std::fs::write(&exe, b"x").unwrap();
        assert!(is_mpv(&exe));
    }

    /// Set `OXIDE_FLUX_MPV_ARCHIVE` to a locally downloaded pinned 7z and run
    /// with `--ignored` to prove the extractor handles the real archive.
    #[test]
    #[ignore = "needs OXIDE_FLUX_MPV_ARCHIVE pointing at the pinned 7z"]
    fn local_pinned_archive_extracts_mpv() {
        let archive = std::env::var_os("OXIDE_FLUX_MPV_ARCHIVE")
            .expect("set OXIDE_FLUX_MPV_ARCHIVE to the pinned 7z");
        let dir = tempfile::tempdir().unwrap();
        sevenz_rust2::decompress_file(archive, dir.path()).unwrap();
        assert!(dir.path().join("mpv.exe").is_file());
    }
}
