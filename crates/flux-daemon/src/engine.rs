//! Adapter from the daemon's [`SessionHandle`] to the control-API [`Engine`].
//!
//! The transport crate knows nothing about metainfo, disks or queues; this module
//! is the only place where the two vocabularies meet. Two rules shape it:
//!
//! * **No invented state.** This build has no peer layer, so a torrent that is
//!   merely holding a download slot is reported as [`ApiState::Queued`] and a
//!   complete one as [`ApiState::Complete`]; neither claims to be transferring.
//! * **Unsupported means rejected.** Magnet URIs need `ut_metadata`, and file
//!   priority `low` has no engine equivalent, so both return
//!   [`EngineError::Unsupported`] instead of being silently approximated.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use flux_api::{
    AddTorrent, Engine, EngineError, FileView, Info, Priority as ApiPriority, Stats, StreamingMode,
    TorrentDetail, TorrentState as ApiState, TorrentSummary,
};
use flux_core::metainfo::TorrentKind;
use flux_core::picker::Mode as CoreMode;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::session::{FilePriority, Status};
use crate::stream::StreamServer;
use crate::stream_source::TorrentStreamSource;
use crate::{SessionError, SessionHandle, TorrentState};

/// Maximum torrents the API will report, mirroring the session's own limits.
const SUMMARY_LIMIT: usize = 100_000;

/// A [`flux_api::Engine`] backed by a running daemon session.
#[derive(Clone)]
pub struct DaemonEngine {
    session: SessionHandle,
    streams: Arc<Mutex<HashMap<String, StreamServer>>>,
}

impl DaemonEngine {
    /// Wraps a session.
    ///
    /// `download_dir` is the session's configured directory, accepted so callers
    /// can construct the engine before the session reports anything. It is not
    /// stored: every torrent knows its own root, and the detail view reports
    /// that one, so a torrent added to a different folder is never mislabelled
    /// with the session default.
    #[must_use]
    pub fn new(session: SessionHandle, _download_dir: impl Into<PathBuf>) -> Self {
        Self {
            session,
            streams: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The underlying session, for callers that need features the control API
    /// does not expose yet (piece bitfields, recheck).
    #[must_use]
    pub fn session(&self) -> &SessionHandle {
        &self.session
    }

    fn detail(&self, state: &TorrentState) -> TorrentDetail {
        TorrentDetail {
            summary: summary_of(state),
            files: state
                .files
                .iter()
                .map(|file| FileView {
                    index: file.index,
                    path: file.path.clone(),
                    length: file.length,
                    priority: api_priority(file.priority),
                    bytes_completed: file.bytes_completed.min(file.length),
                    completed: file.completed,
                    offset: file.offset,
                })
                .collect(),
            piece_length: u64::from(state.piece_length),
            download_dir: state.root.display().to_string(),
        }
    }
}

#[async_trait]
impl Engine for DaemonEngine {
    async fn info(&self) -> Result<Info, EngineError> {
        Ok(Info {
            name: "oxide-flux".to_owned(),
            version: crate::VERSION.to_owned(),
            api_version: Info::API_VERSION,
        })
    }

    async fn list(&self) -> Result<Vec<TorrentSummary>, EngineError> {
        let states = self.session.list().await.map_err(map_session_error)?;
        if states.len() > SUMMARY_LIMIT {
            return Err(EngineError::Other(format!(
                "session holds more than {SUMMARY_LIMIT} torrents"
            )));
        }
        Ok(states.iter().map(summary_of).collect())
    }

    async fn get(&self, id: &str) -> Result<TorrentDetail, EngineError> {
        let id = parse_id(id)?;
        let state = self.session.get(&id).await.map_err(map_session_error)?;
        Ok(self.detail(&state))
    }

    async fn add(&self, source: AddTorrent) -> Result<TorrentSummary, EngineError> {
        let state = match source {
            AddTorrent::Metainfo { bytes, save_path } => self
                .session
                .add_torrent_with_root(bytes, save_path)
                .await
                .map_err(map_session_error)?,
            AddTorrent::Magnet { uri } => self
                .session
                .add_magnet(&uri)
                .await
                .map_err(map_session_error)?,
        };
        Ok(summary_of(&state))
    }

    async fn pause(&self, id: &str) -> Result<(), EngineError> {
        let id = parse_id(id)?;
        self.session.pause(&id).await.map_err(map_session_error)?;
        Ok(())
    }

    async fn resume(&self, id: &str) -> Result<(), EngineError> {
        let id = parse_id(id)?;
        self.session.resume(&id).await.map_err(map_session_error)?;
        Ok(())
    }

    async fn remove(&self, id: &str, delete_files: bool) -> Result<(), EngineError> {
        let id = parse_id(id)?;
        self.session
            .remove(&id, delete_files)
            .await
            .map_err(map_session_error)
    }

    async fn set_file_priorities(
        &self,
        id: &str,
        priorities: Vec<(usize, ApiPriority)>,
    ) -> Result<(), EngineError> {
        let id = parse_id(id)?;
        let mut changes = Vec::with_capacity(priorities.len());
        for (index, priority) in priorities {
            let priority = match priority {
                ApiPriority::Skip => crate::Priority::Skip,
                ApiPriority::Normal => crate::Priority::Normal,
                ApiPriority::High => crate::Priority::High,
                ApiPriority::Low => {
                    return Err(EngineError::Unsupported(
                        "file priority `low` is not modelled; use `normal` or `skip`".to_owned(),
                    ));
                }
            };
            changes.push(FilePriority { index, priority });
        }
        self.session
            .set_file_priorities(&id, changes)
            .await
            .map_err(map_session_error)?;
        Ok(())
    }

    async fn set_playhead(&self, id: &str, piece: u32) -> Result<(), EngineError> {
        let id = parse_id(id)?;
        self.session
            .set_playhead(&id, piece as usize)
            .await
            .map_err(map_session_error)
    }

    async fn set_streaming_mode(&self, id: &str, mode: StreamingMode) -> Result<(), EngineError> {
        let id = parse_id(id)?;
        let mode = match mode {
            StreamingMode::Sequential => CoreMode::Sequential,
            StreamingMode::Rarest => CoreMode::RarestFirst,
        };
        self.session
            .set_mode(&id, mode)
            .await
            .map_err(map_session_error)
    }

    async fn stream_url(&self, id: &str, file: usize) -> Result<String, EngineError> {
        let parsed = parse_id(id)?;
        let layout = self
            .session
            .stream_layout(&parsed)
            .await
            .map_err(map_session_error)?;
        if file >= layout.files.len() {
            return Err(EngineError::InvalidFileIndex(file));
        }

        let mut streams = self.streams.lock().await;
        if let Some(server) = streams.get(id) {
            return Ok(format!("http://{}/stream/{}", server.local_addr(), file));
        }

        let source = TorrentStreamSource::new(self.session.clone(), parsed, layout)
            .map_err(|error| EngineError::Io(error.to_string()))?;
        let server = StreamServer::serve(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(source),
        )
        .await
        .map_err(|error| EngineError::Io(error.to_string()))?;
        let url = format!("http://{}/stream/{}", server.local_addr(), file);
        streams.insert(id.to_owned(), server);
        Ok(url)
    }

    async fn stats(&self) -> Result<Stats, EngineError> {
        let states = self.session.list().await.map_err(map_session_error)?;
        let mut stats = Stats {
            torrents: states.len(),
            ..Stats::default()
        };
        for state in &states {
            // Counters, not rates. They come from real swarm events; a torrent
            // with no swarm contributes zero.
            stats.bytes_downloaded = stats
                .bytes_downloaded
                .saturating_add(state.downloaded_bytes);
            stats.bytes_uploaded = stats.bytes_uploaded.saturating_add(state.uploaded_bytes);
            match state.status {
                Status::Checking => stats.active += 1,
                Status::Active => {
                    stats.active += 1;
                    stats.downloading += 1;
                }
                Status::Seeding => {
                    stats.active += 1;
                    stats.seeding += 1;
                }
                Status::Paused => stats.paused += 1,
                Status::Queued | Status::Preallocated | Status::Ready => {}
            }
        }
        Ok(stats)
    }
}

/// Translates one engine torrent into the API summary.
fn summary_of(state: &TorrentState) -> TorrentSummary {
    let total = state.piece_count as u64;
    let have = u64::try_from(state.pieces_have).unwrap_or(total);
    TorrentSummary {
        id: state.id.as_str().to_owned(),
        name: state.name.clone(),
        state: api_state(state.status),
        private: state.private,
        progress: if total == 0 {
            0.0
        } else {
            have as f64 / total as f64
        },
        pieces_have: have,
        pieces_total: total,
        total_bytes: state.payload_length,
        peers: u32::try_from(state.peers).unwrap_or(u32::MAX),
        download_rate: state.download_rate,
        upload_rate: state.upload_rate,
        last_error: state.last_error.clone(),
    }
}

/// Lifecycle mapping. `Active` and `Ready` are queue-slot states with no peer
/// layer behind them, so they surface as `Queued`/`Complete`, never as
/// `Downloading` or `Seeding`.
fn api_state(status: Status) -> ApiState {
    match status {
        Status::Queued | Status::Preallocated => ApiState::Queued,
        Status::Checking => ApiState::Checking,
        Status::Paused => ApiState::Paused,
        // The swarm is running; whether data arrives depends on peers, but this
        // is the honest "downloading" state now that the peer layer exists.
        Status::Active => ApiState::Downloading,
        Status::Ready => ApiState::Complete,
        Status::Seeding => ApiState::Seeding,
    }
}

/// The API's `Priority` has one level the engine does not model (`low`), but
/// every engine level has an API counterpart.
fn api_priority(priority: crate::Priority) -> ApiPriority {
    match priority {
        crate::Priority::Skip => ApiPriority::Skip,
        crate::Priority::Normal => ApiPriority::Normal,
        crate::Priority::High => ApiPriority::High,
    }
}

fn parse_id(id: &str) -> Result<crate::TorrentId, EngineError> {
    id.parse::<crate::TorrentId>()
        .map_err(|error| EngineError::InvalidSource(error.to_string()))
}

fn map_session_error(error: SessionError) -> EngineError {
    match error {
        SessionError::UnknownTorrent { id } => EngineError::NotFound(id),
        SessionError::Duplicate { id } => EngineError::Duplicate(id),
        SessionError::FileIndexOutOfRange { index, .. } => EngineError::InvalidFileIndex(index),
        SessionError::MagnetLink(reason) => EngineError::InvalidSource(reason),
        SessionError::Magnet(crate::magnet::MagnetError::NoInfoHash) => EngineError::Unsupported(
            "v2-only magnets need Merkle metadata verification, which this build does not implement"
                .to_owned(),
        ),
        SessionError::Magnet(reason) => EngineError::InvalidSource(reason.to_string()),
        SessionError::Add(inner) => match inner {
            crate::AddError::Disk(_) | crate::AddError::Io { .. } | crate::AddError::Join(_) => {
                EngineError::Io(inner.to_string())
            }
            _ => EngineError::InvalidSource(inner.to_string()),
        },
        SessionError::Io { .. } | SessionError::ReadTorrent { .. } => {
            EngineError::Io(error.to_string())
        }
        SessionError::Closed | SessionError::ActorStopped => {
            EngineError::Other("session has shut down".to_owned())
        }
        other => EngineError::Other(other.to_string()),
    }
}

/// Forwards session events to the control API's notification stream.
///
/// Progress ticks (`Checked`) are filtered out: they can fire ~100 times per
/// pass, and clients get the same information by re-reading `torrent.get`.
/// A lagged observer is ignored here because the WebSocket layer resynchronises
/// from `torrent.list` on its own.
///
/// The task ends when the session shuts down or closes its channel.
pub fn spawn_event_bridge(
    session: SessionHandle,
    sink: broadcast::Sender<flux_api::Event>,
) -> JoinHandle<()> {
    // Subscribe before spawning so no event emitted after this call can be missed.
    let mut events = session.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => match event {
                    crate::Event::Added { torrent } => {
                        let _ = sink.send(flux_api::Event::TorrentUpdated(summary_of(&torrent)));
                    }
                    crate::Event::StatusChanged { id, .. }
                    | crate::Event::Updated { id }
                    | crate::Event::Finished { id, .. } => {
                        if let Ok(state) = session.get(&id).await {
                            let _ = sink.send(flux_api::Event::TorrentUpdated(summary_of(&state)));
                        }
                    }
                    crate::Event::Removed { id } => {
                        let _ = sink.send(flux_api::Event::TorrentRemoved {
                            id: id.into_string(),
                        });
                    }
                    crate::Event::Checked { .. } => {}
                    crate::Event::Warning(_) => {}
                    crate::Event::Shutdown => {
                        let _ = sink.send(flux_api::Event::SessionShutdown);
                        break;
                    }
                },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// The engine's view of a torrent kind, for callers that need it without
/// depending on `flux-core` directly.
#[must_use]
pub fn kind_name(kind: TorrentKind) -> &'static str {
    match kind {
        TorrentKind::V1 => "v1",
        TorrentKind::V2 => "v2",
        TorrentKind::Hybrid => "hybrid",
    }
}

/// Convenience constructor for the daemon binary.
#[must_use]
pub fn shared(session: SessionHandle, download_dir: impl Into<PathBuf>) -> Arc<DaemonEngine> {
    Arc::new(DaemonEngine::new(session, download_dir))
}
