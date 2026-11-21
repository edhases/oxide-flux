//! Oxide Flux engine: the headless daemon and its local data pipeline.
//!
//! This crate is the process that owns a session: it parses metainfo, validates
//! and preallocates files, tracks per-file priorities, applies queue limits and
//! verifies existing data. It is deliberately **not** a peer client yet — there are
//! no trackers, DHT, PEX, LSD, uTP or MSE in this build, so nothing can be
//! downloaded, and no state pretends otherwise. See [`session::Status`].
//!
//! The UI is a separate process and talks to the engine over the control API
//! (`flux-api`); it never links against this crate's internals.
//!
//! # Layout of the crate
//!
//! * [`config`] — TOML configuration, validation, token redaction.
//! * [`session`] — the actor that owns all torrent state.
//! * [`verify`] — the core↔disk layout bridge, preallocation and hash checking.
//! * [`pieces`] — file priorities and the desired/have piece bitfields.
//! * [`cli`] — `fluxd` argument parsing and commands.
//!
//! # Example
//!
//! ```no_run
//! # #[allow(dead_code)]
//! async fn example() -> Result<(), Box<dyn std::error::Error>> {
//!     use flux_daemon::{Config, SessionHandle};
//!     use flux_daemon::pieces::Priority;
//!
//!     let config = Config {
//!         download_dir: std::env::temp_dir().join("flux-demo"),
//!         ..Config::default()
//!     };
//!
//!     let session = SessionHandle::start(config).await?;
//!     let mut events = session.subscribe();
//!     let state = session.add_torrent(std::fs::read("example.torrent")?).await?;
//!     println!("{} is {:?}", state.name, state.status);
//!
//!     session
//!         .set_file_priorities(&state.id, [(0, Priority::Skip)])
//!         .await?;
//!     let _ = events.recv().await;
//!     session.shutdown().await?;
//!     Ok(())
//! }
//! ```

pub mod announce;
pub mod cli;
pub mod config;
pub mod engine;
pub mod id;
pub mod json;
pub mod magnet;
pub mod pieces;
pub mod session;
pub mod state;
pub mod stream;
pub mod stream_source;
pub mod swarm;
pub mod verify;

pub use config::{Config, ConfigError, Preallocate};
pub use engine::{DaemonEngine, spawn_event_bridge};
pub use id::{IdError, TorrentId};
pub use pieces::{PieceBitfield, Priority};
pub use session::{
    Event, FilePriority, FileState, SessionError, SessionHandle, Status, TorrentState, Verification,
};
pub use verify::{AddError, CheckRun, TorrentPlan, VerifyReport};

/// Version of the engine, as reported by `fluxd --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
