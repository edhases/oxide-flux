//! The `fluxd` command line.
//!
//! Two entry points, no argument-parsing dependency:
//!
//! ```text
//! fluxd verify <torrent-file> [--dir PATH] [--json]
//! fluxd --config <path>
//! ```
//!
//! `verify` parses a torrent, sanitizes its paths, preallocates its files and
//! hashes every piece. Exit status is `0` when every piece matched, `1` when data
//! is missing or corrupt, and `2` for usage or I/O failures.
//!
//! `--config` validates the config, creates the download directory, starts the
//! session and serves the local control API until Ctrl-C. Peer networking is not
//! implemented, so the daemon exposes state and storage operations only; it never
//! pretends to be transferring.
//!
//! All logic lives in this library; `main` only forwards [`main`].

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use flux_disk::{Allocation, DiskError};

use crate::config::{Config, ConfigError};
use crate::engine;
use crate::id::TorrentId;
use crate::json::JsonOut;
use crate::pieces::{self, PieceBitfield};
use crate::session::{SessionError, SessionHandle};
use crate::verify::{self, AddError, CheckJob, VerifyReport};

/// Help text, also printed for `--help`.
pub const USAGE: &str = "\
fluxd - Oxide Flux engine

USAGE:
    fluxd verify <torrent-file> [--dir PATH] [--json]
    fluxd --config <path>
    fluxd --help
    fluxd --version

COMMANDS:
    verify          Parse a torrent, preallocate its files and hash every piece.

OPTIONS:
    --dir PATH      Download root for `verify` (default: current directory)
    --json          Machine-readable summary for `verify`
    --config PATH   TOML configuration for the daemon; serves the control API
    -h, --help      Print this help
    -V, --version   Print the version
";

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Verify one torrent's local data.
    Verify(VerifyArgs),
    /// Load the config, prepare the download dir, then exit.
    Daemon(DaemonArgs),
    /// Print the help text.
    Help,
    /// Print the version.
    Version,
}

/// Arguments for `fluxd verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyArgs {
    /// The `.torrent` file to check.
    pub torrent: PathBuf,
    /// Download root. Defaults to the current directory.
    pub dir: Option<PathBuf>,
    /// Print JSON instead of text.
    pub json: bool,
}

/// Arguments for `fluxd --config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonArgs {
    /// Config file. `None` means "use the defaults".
    pub config: Option<PathBuf>,
}

/// Result of a verification pass, as printed by the CLI.
#[derive(Debug, Clone)]
pub struct VerifySummary {
    /// Info hash hex.
    pub id: TorrentId,
    /// Torrent name.
    pub name: String,
    /// `private: 1`.
    pub private: bool,
    /// Root that was checked.
    pub dir: PathBuf,
    /// Piece length in bytes.
    pub piece_length: u32,
    /// Number of pieces.
    pub piece_count: usize,
    /// Address-space length.
    pub total_length: u64,
    /// Sum of the file lengths.
    pub payload_length: u64,
    /// Pieces that matched.
    pub have: usize,
    /// Pieces that were not verified. Includes `corrupt` and `unreadable`.
    pub missing: usize,
    /// Pieces that were readable but wrong.
    pub corrupt: usize,
    /// Pieces that could not be read.
    pub unreadable: usize,
    /// Whether every piece matched.
    pub clean: bool,
    /// First read error, if any.
    pub first_error: Option<String>,
    /// Per-file coverage.
    pub files: Vec<VerifyFileSummary>,
}

/// Per-file part of [`VerifySummary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyFileSummary {
    /// Path relative to the download root, with `/` separators.
    pub path: String,
    /// Declared length.
    pub length: u64,
    /// Verified bytes.
    pub bytes_completed: u64,
    /// Whether the file is fully covered by verified pieces.
    pub completed: bool,
}

impl VerifySummary {
    /// `0` when clean, `1` when data is missing or corrupt.
    pub fn exit_code(&self) -> u8 {
        if self.clean { 0 } else { 1 }
    }
}

/// A command-line failure. Every variant exits with status 2.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The arguments were not understood.
    #[error("{0}\n\n{USAGE}")]
    Usage(String),
    /// The torrent file could not be read.
    #[error("cannot read torrent `{}`: {source}", .path.display())]
    ReadTorrent {
        /// The path that was read.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// A filesystem operation failed.
    #[error("i/o error on `{}`: {source}", .path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The config was invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The torrent could not be prepared.
    #[error(transparent)]
    Add(#[from] AddError),
    /// The storage layer refused a job.
    #[error(transparent)]
    Disk(#[from] DiskError),
    /// A blocking task was cancelled or panicked.
    #[error("blocking task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    /// The async runtime could not be started.
    #[error("cannot start the async runtime: {0}")]
    Runtime(#[source] io::Error),
    /// The session failed to start or stop.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The control API failed to start or stop.
    #[error(transparent)]
    Api(#[from] flux_api::ServerError),
}

impl CliError {
    /// Process exit status for this failure.
    pub fn exit_code(&self) -> u8 {
        2
    }
}

/// Runs the CLI and returns the process exit status.
///
/// This is the only function `main` needs.
pub fn main(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let invocation = match parse_args(args) {
        Ok(invocation) => invocation,
        Err(error) => return report_failure(&error),
    };

    match invocation {
        Invocation::Help => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Invocation::Version => {
            println!("fluxd {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Invocation::Verify(args) => {
            let runtime = match build_runtime(None) {
                Ok(runtime) => runtime,
                Err(error) => return report_failure(&error),
            };
            match runtime.block_on(verify_command(&args)) {
                Ok(summary) => {
                    print_summary(&summary, args.json);
                    ExitCode::from(summary.exit_code())
                }
                Err(error) => report_failure(&error),
            }
        }
        Invocation::Daemon(args) => {
            let config = match &args.config {
                Some(path) => Config::load(path),
                // An empty document is a valid config: everything takes its default.
                None => Config::parse(""),
            };
            let config = match config {
                Ok(config) => config,
                Err(error) => return report_failure(&CliError::Config(error)),
            };
            let runtime = match build_runtime(Some(config.worker_threads)) {
                Ok(runtime) => runtime,
                Err(error) => return report_failure(&error),
            };
            match runtime.block_on(daemon_command(&config)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => report_failure(&error),
            }
        }
    }
}

fn report_failure(error: &CliError) -> ExitCode {
    eprintln!("fluxd: {error}");
    ExitCode::from(error.exit_code())
}

pub fn build_runtime(worker_threads: Option<usize>) -> Result<tokio::runtime::Runtime, CliError> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(threads) = worker_threads {
        builder.worker_threads(threads);
    }
    // The control API needs a reactor: `session.info` is served over TCP.
    builder.enable_all().build().map_err(CliError::Runtime)
}

/// Parses the process arguments (program name first).
///
/// # Errors
///
/// Returns [`CliError::Usage`] for an unknown command, a missing value after
/// `--dir`/`--config`, or more than one torrent path.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, CliError> {
    let mut args = args.into_iter();
    let _program = args.next();
    let rest: Vec<OsString> = args.collect();
    let Some(first) = rest.first() else {
        return Ok(Invocation::Help);
    };
    let command = first.to_string_lossy().into_owned();
    match command.as_str() {
        "-h" | "--help" | "help" => Ok(Invocation::Help),
        "-V" | "--version" | "version" => Ok(Invocation::Version),
        "verify" => parse_verify(&rest[1..]),
        "--config" | "-c" => parse_daemon(&rest),
        other => Err(CliError::Usage(format!("unknown command `{other}`"))),
    }
}

fn parse_verify(args: &[OsString]) -> Result<Invocation, CliError> {
    let mut torrent = None;
    let mut dir = None;
    let mut json = false;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        let text = argument.to_string_lossy();
        match text.as_ref() {
            "--json" => json = true,
            "--dir" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| CliError::Usage("`--dir` needs a path after it".to_owned()))?;
                dir = Some(PathBuf::from(value));
            }
            "-h" | "--help" => return Ok(Invocation::Help),
            other if other.starts_with("--dir=") => {
                let value = other.trim_start_matches("--dir=");
                if value.is_empty() {
                    return Err(CliError::Usage("`--dir` needs a path after it".to_owned()));
                }
                dir = Some(PathBuf::from(value));
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown option `{other}`")));
            }
            other => {
                if torrent.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument `{other}`"
                    )));
                }
                torrent = Some(PathBuf::from(other));
            }
        }
        index += 1;
    }
    let Some(torrent) = torrent else {
        return Err(CliError::Usage(
            "`verify` needs a `.torrent` file".to_owned(),
        ));
    };
    Ok(Invocation::Verify(VerifyArgs { torrent, dir, json }))
}

fn parse_daemon(args: &[OsString]) -> Result<Invocation, CliError> {
    let mut config = None;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        let text = argument.to_string_lossy();
        match text.as_ref() {
            "--config" | "-c" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    CliError::Usage("`--config` needs a path after it".to_owned())
                })?;
                config = Some(PathBuf::from(value));
            }
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            other if other.starts_with("--config=") => {
                let value = other.trim_start_matches("--config=");
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "`--config` needs a path after it".to_owned(),
                    ));
                }
                config = Some(PathBuf::from(value));
            }
            other => {
                return Err(CliError::Usage(format!("unexpected argument `{other}`")));
            }
        }
        index += 1;
    }
    Ok(Invocation::Daemon(DaemonArgs { config }))
}

/// Runs `fluxd verify`.
///
/// # Errors
///
/// [`CliError`] for an unreadable torrent file, metadata that must be rejected,
/// or a storage failure.
pub async fn verify_command(args: &VerifyArgs) -> Result<VerifySummary, CliError> {
    let dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    verify_torrent_file(&args.torrent, &dir).await
}

/// Checks every piece of `torrent` against the bytes stored under `dir`.
///
/// Creates the missing files at their declared length (sparse) before hashing, so
/// `verify` is safe to run on a fresh directory: every piece then reports as
/// missing rather than as an I/O error. Existing data is never zeroed or
/// overwritten.
///
/// # Errors
///
/// [`CliError`] for an unreadable torrent file, metadata that must be rejected,
/// or a storage failure.
pub async fn verify_torrent_file(torrent: &Path, dir: &Path) -> Result<VerifySummary, CliError> {
    let read_path = torrent.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(read_path))
        .await?
        .map_err(|source| CliError::ReadTorrent {
            path: torrent.to_path_buf(),
            source,
        })?;

    // The root must exist before sanitized paths are resolved against it.
    let root = dir.to_path_buf();
    let create_path = dir.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::create_dir_all(&root))
        .await?
        .map_err(|source| CliError::Io {
            path: create_path,
            source,
        })?;

    let options = verify::PrepareOptions {
        root: dir.to_path_buf(),
        allocation: Allocation::Sparse,
    };
    let plan = verify::prepare(bytes, options).await?;

    let (report, have) = match plan.piece_hashes().map(|hashes| hashes.to_vec()) {
        Some(hashes) => {
            let job = CheckJob {
                layout: plan.disk_layout(),
                root: dir.to_path_buf(),
                piece_length: plan.piece_length(),
                hashes,
                threads: Config::default().verifier_threads,
            };
            let run = verify::run_check(job, None).await?;
            (run.report, run.have)
        }
        None => (
            VerifyReport {
                pieces_total: plan.piece_count(),
                ..VerifyReport::default()
            },
            PieceBitfield::new(plan.piece_count()),
        ),
    };

    let coverage = pieces::coverage_per_file(plan.layout(), &have);
    let files = plan
        .files()
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let bytes_completed = coverage.get(index).copied().unwrap_or(0);
            VerifyFileSummary {
                path: file.relative.to_string_lossy().replace('\\', "/"),
                length: file.length,
                bytes_completed: bytes_completed.min(file.length),
                completed: bytes_completed >= file.length,
            }
        })
        .collect();

    Ok(VerifySummary {
        id: plan.id().clone(),
        name: plan.name().to_owned(),
        private: plan.private(),
        dir: dir.to_path_buf(),
        piece_length: plan.piece_length(),
        piece_count: plan.piece_count(),
        total_length: plan.total_length(),
        payload_length: plan.payload_length(),
        have: report.have,
        missing: report.missing(),
        corrupt: report.corrupt,
        unreadable: report.unreadable,
        clean: report.is_clean(),
        first_error: report.first_error,
        files,
    })
}

/// Runs `fluxd --config <path>`: validate, create the download dir, then stop.
///
/// # Errors
///
/// [`CliError::Config`] if the download directory cannot be created.
/// Starts the session, serves the control API and waits for Ctrl-C.
///
/// The daemon owns exactly what this build can do: storage preparation,
/// verification, file selection and state reporting. No socket is opened for
/// peers, so nothing here can download or upload.
///
/// # Errors
///
/// [`CliError`] when the download directory cannot be prepared, the session fails
/// to start, or the API cannot bind (a non-loopback `api_bind` without
/// `api_token` is refused before any socket is opened).
pub async fn daemon_command(config: &Config) -> Result<(), CliError> {
    let ready = config.clone();
    let dir =
        tokio::task::spawn_blocking(move || ready.ensure_download_dir().map(Path::to_path_buf))
            .await??;

    let session = SessionHandle::start(config.clone()).await?;
    let engine = engine::shared(session.clone(), &dir);
    let server_config = flux_api::ServerConfig {
        bind: config.api_bind,
        token: config.api_token.clone(),
        ..flux_api::ServerConfig::new()
    };
    let server = flux_api::serve(server_config, engine).await?;
    let _bridge = engine::spawn_event_bridge(session.clone(), server.events());

    println!("fluxd {}", env!("CARGO_PKG_VERSION"));
    println!("config ok: {}", config.summary());
    println!("download dir ready: {}", dir.display());
    println!(
        "control API: http://{}/rpc  (UI: http://{}/, events: ws://{}/ws)",
        server.local_addr(),
        server.local_addr(),
        server.local_addr()
    );
    println!();
    println!("note: peer networking: TCP peer wire, HTTP/UDP trackers, DHT and ut_metadata");
    println!("      magnets. MSE, uTP, PEX and LSD are not implemented; v2-only torrents are");
    println!("      stored but never downloaded because Merkle verification is missing.");
    println!("      Press Ctrl-C to stop.");

    if let Err(source) = tokio::signal::ctrl_c().await {
        eprintln!("fluxd: cannot listen for Ctrl-C: {source}");
    }

    let _ = server.publish(flux_api::Event::SessionShutdown);
    session.shutdown().await?;
    server.shutdown().await?;
    println!("fluxd: stopped");
    Ok(())
}

fn print_summary(summary: &VerifySummary, json: bool) {
    if json {
        println!("{}", summary_json(summary));
        return;
    }

    println!("fluxd verify: `{}`", summary.name);
    println!("  id:     {}", summary.id);
    println!("  dir:    {}", summary.dir.display());
    println!(
        "  layout: {} file(s), {} bytes of payload ({} in the piece space), piece length {}, {} pieces",
        summary.files.len(),
        summary.payload_length,
        summary.total_length,
        summary.piece_length,
        summary.piece_count
    );
    println!(
        "  check:  {} of {} pieces verified ({} corrupt, {} unreadable)",
        summary.have, summary.piece_count, summary.corrupt, summary.unreadable
    );
    if summary.private {
        println!("  private: yes (DHT/PEX/LSD must stay off for this torrent)");
    }
    if let Some(error) = &summary.first_error {
        println!("  first read error: {error}");
    }
    if summary.clean {
        println!("  result: clean, every piece matched");
    } else {
        println!(
            "  result: NOT clean, {} piece(s) are not verified",
            summary.missing
        );
    }
}

/// The `--json` document for a verification pass.
pub fn summary_json(summary: &VerifySummary) -> String {
    let mut json = JsonOut::new();
    json.object(|out| {
        out.key("id");
        out.string(summary.id.as_str());
        out.key("name");
        out.string(&summary.name);
        out.key("private");
        out.boolean(summary.private);
        out.key("dir");
        out.string(&summary.dir.to_string_lossy());
        out.key("piece_length");
        out.number(summary.piece_length);
        out.key("piece_count");
        out.number(summary.piece_count);
        out.key("total_length");
        out.number(summary.total_length);
        out.key("payload_length");
        out.number(summary.payload_length);
        out.key("have");
        out.number(summary.have);
        out.key("missing");
        out.number(summary.missing);
        out.key("corrupt");
        out.number(summary.corrupt);
        out.key("unreadable");
        out.number(summary.unreadable);
        out.key("clean");
        out.boolean(summary.clean);
        out.key("first_error");
        match &summary.first_error {
            Some(error) => out.string(error),
            None => out.null(),
        }
        out.key("files");
        out.array(|out| {
            for file in &summary.files {
                out.object(|out| {
                    out.key("path");
                    out.string(&file.path);
                    out.key("length");
                    out.number(file.length);
                    out.key("bytes_completed");
                    out.number(file.bytes_completed);
                    out.key("completed");
                    out.boolean(file.completed);
                });
            }
        });
    });
    json.finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_verify_arguments() {
        let args = vec![
            OsString::from("fluxd"),
            OsString::from("verify"),
            OsString::from("a.torrent"),
            OsString::from("--dir"),
            OsString::from("media"),
            OsString::from("--json"),
        ];
        assert_eq!(
            parse_args(args).unwrap(),
            Invocation::Verify(VerifyArgs {
                torrent: PathBuf::from("a.torrent"),
                dir: Some(PathBuf::from("media")),
                json: true,
            })
        );

        let args = vec![
            OsString::from("fluxd"),
            OsString::from("verify"),
            OsString::from("a.torrent"),
            OsString::from("--dir=media"),
        ];
        assert_eq!(
            parse_args(args).unwrap(),
            Invocation::Verify(VerifyArgs {
                torrent: PathBuf::from("a.torrent"),
                dir: Some(PathBuf::from("media")),
                json: false,
            })
        );
    }

    #[test]
    fn parses_daemon_arguments_and_rejects_nonsense() {
        assert_eq!(
            parse_args(vec![OsString::from("fluxd"), OsString::from("--config")])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            parse_args(vec![
                OsString::from("fluxd"),
                OsString::from("--config"),
                OsString::from("fluxd.toml"),
            ])
            .unwrap(),
            Invocation::Daemon(DaemonArgs {
                config: Some(PathBuf::from("fluxd.toml")),
            })
        );
        assert!(parse_args(vec![OsString::from("fluxd"), OsString::from("nope")]).is_err());
        assert!(parse_args(vec![OsString::from("fluxd"), OsString::from("verify")]).is_err());
        assert_eq!(
            parse_args(vec![OsString::from("fluxd"), OsString::from("--help")]).unwrap(),
            Invocation::Help
        );
        assert_eq!(
            parse_args(Vec::<OsString>::new()).unwrap(),
            Invocation::Help
        );
    }

    #[test]
    fn rejects_extra_positional_arguments() {
        let args = vec![
            OsString::from("fluxd"),
            OsString::from("verify"),
            OsString::from("a.torrent"),
            OsString::from("b.torrent"),
        ];
        assert!(parse_args(args).is_err());
    }
}
