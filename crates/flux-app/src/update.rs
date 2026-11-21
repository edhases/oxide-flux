//! Self-update from GitHub Releases.
//!
//! [`check`] only lists releases and compares versions; [`install`] downloads the
//! current platform's archive, verifies the published checksums and replaces the
//! running binary. Both are blocking (the crate's HTTP client is synchronous), so
//! the UI calls them from `spawn_blocking`.
//!
//! Release tags are `vX.Y.Z`; a pre-release tag never counts as an update.

use semver::Version;

/// GitHub owner of the release repository.
const REPO_OWNER: &str = "edhases";
/// GitHub repository that publishes the releases.
const REPO_NAME: &str = "oxide-flux";
/// The binary asset name inside every release archive.
const BIN_NAME: &str = "oxide-flux";
/// The asset that carries the per-file SHA-256 digests.
const CHECKSUMS_ASSET: &str = "SHA256SUMS";

/// A newer release found on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    /// The bare version (`0.2.0`, no leading `v`).
    pub version: String,
}

/// Why an update check or install did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// Listing or comparing releases failed.
    #[error("update check failed: {0}")]
    Check(String),
    /// Downloading, verifying or replacing the binary failed.
    #[error("update install failed: {0}")]
    Install(String),
}

/// Lists the GitHub releases of `oxide-flux` and returns the highest stable
/// version greater than `current`, without downloading anything.
///
/// A leading `v` on `current` is ignored. Errors are returned to the caller; the
/// UI treats them as "no update" unless the user asked to install.
///
/// # Errors
///
/// Returns [`UpdateError::Check`] when the release list cannot be fetched.
pub fn check(current: &str) -> Result<Option<AvailableUpdate>, UpdateError> {
    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .build()
        .map_err(|error| UpdateError::Check(error.to_string()))?
        .fetch()
        .map_err(|error| UpdateError::Check(error.to_string()))?
        .into_vec();

    let versions = releases
        .iter()
        .map(|release| release.version())
        .collect::<Vec<_>>();

    Ok(newest(current, &versions).map(|version| AvailableUpdate { version }))
}

/// Downloads the offered stable release, verifies its checksums and replaces the
/// running binary, returning the installed version.
///
/// The target is pinned to the release [`check`] would offer, so the installed
/// version always matches what the UI announced and a pre-release can never be
/// selected. The caller typically follows with a restart prompt: the process
/// keeps running the old image until it is restarted.
///
/// # Errors
///
/// Returns [`UpdateError::Install`] when no newer stable release exists, when the
/// release cannot be fetched, downloaded, verified or installed, and when the
/// backend reports that nothing newer was installed (it never claims success
/// without an installed update).
pub fn install(current: &str) -> Result<String, UpdateError> {
    let update = check(current).map_err(|error| UpdateError::Install(error.to_string()))?;
    let Some(update) = update else {
        return Err(UpdateError::Install(
            "no newer stable release is available".to_owned(),
        ));
    };

    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(bare_version(current))
        .release_tag(format!("v{}", update.version))
        .no_confirm(true)
        .show_download_progress(false)
        .checksum_from_asset(CHECKSUMS_ASSET)
        .verify_release_digest(true)
        .build()
        .map_err(|error| UpdateError::Install(error.to_string()))?
        .update()
        .map_err(|error| UpdateError::Install(error.to_string()))?;

    if !status.is_updated() {
        return Err(UpdateError::Install(format!(
            "no newer release was installed (still at {})",
            status.version()
        )));
    }

    Ok(status.version().to_owned())
}

/// The highest stable version in `versions` that is greater than `current`.
///
/// Tags may carry a leading `v`; malformed tags, pre-releases and anything not
/// strictly newer are skipped. An unparseable `current` yields `None` (no
/// update) rather than an invented comparison.
#[must_use]
pub fn newest(current: &str, versions: &[&str]) -> Option<String> {
    let current = Version::parse(bare_version(current)).ok()?;

    versions
        .iter()
        .filter_map(|tag| parse_version(tag))
        .filter(|version| *version > current)
        .max()
        .map(|version| version.to_string())
}

/// Strips a single leading `v` and surrounding whitespace.
fn bare_version(version: &str) -> &str {
    let trimmed = version.trim();
    trimmed.strip_prefix('v').unwrap_or(trimmed)
}

/// Parses a release tag, rejecting anything that is not stable semver.
fn parse_version(tag: &str) -> Option<Version> {
    let version = Version::parse(bare_version(tag)).ok()?;
    version.pre.is_empty().then_some(version)
}
