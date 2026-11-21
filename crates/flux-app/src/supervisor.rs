//! The engine supervisor: owns the session on a tokio runtime and talks to the
//! UI over two bounded channels.
//!
//! The UI thread must stay synchronous, so every session call happens here and
//! arrives as an [`Event`]. Snapshots are pushed once per second and after every
//! command, which is what keeps the table's rates and peer counts honest.

use std::sync::Arc;
use std::time::Duration;

use flux_api::{AddTorrent, Engine, EngineError, Priority, Stats, TorrentDetail, TorrentSummary};
use flux_daemon::{Config, DaemonEngine, SessionHandle};
use tokio::sync::mpsc;

/// Everything the UI can ask the engine to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// Force a snapshot (also happens on ticks and session events).
    Refresh,
    /// Add a `.torrent` file's bytes.
    AddBytes(Vec<u8>),
    /// Add a magnet URI (metadata is fetched from the swarm).
    AddMagnet(String),
    Pause(String),
    Resume(String),
    Remove {
        id: String,
        delete_files: bool,
    },
    SetPriority {
        id: String,
        index: usize,
        priority: Priority,
    },
    GetDetail(String),
    /// Stream one file through the loopback server (works while downloading).
    Play {
        id: String,
        file: usize,
    },
    Shutdown,
}

/// Everything the UI renders.
#[derive(Debug, Clone)]
pub enum Event {
    Ready {
        version: String,
        download_dir: String,
    },
    Snapshot {
        torrents: Vec<TorrentSummary>,
        stats: Stats,
    },
    Detail(TorrentDetail),
    /// A loopback URL ready for the embedded player.
    StreamReady {
        url: String,
        title: String,
    },
    Error(String),
    Closed,
}

/// Runs the session until `Command::Shutdown` or until the UI drops its sender.
pub async fn run(
    config: Config,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Event>,
) {
    let session = match SessionHandle::start(config.clone()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Error(format!("cannot start the session: {error}")));
            let _ = events.send(Event::Closed);
            return;
        }
    };
    let engine: Arc<dyn Engine> =
        Arc::new(DaemonEngine::new(session.clone(), &config.download_dir));

    let _ = events.send(Event::Ready {
        version: flux_daemon::VERSION.to_owned(),
        download_dir: config.download_dir.display().to_string(),
    });

    let mut session_events = session.subscribe();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                if !handle(engine.as_ref(), command, &events).await {
                    break;
                }
                dirty = true;
            }
            event = session_events.recv() => {
                // Lagged or closed: a snapshot resynchronises either way.
                if event.is_ok() {
                    dirty = true;
                }
            }
            _ = ticker.tick() => {
                dirty = true;
            }
        }

        if dirty {
            push_snapshot(engine.as_ref(), &events).await;
            dirty = false;
        }
    }

    let _ = session.shutdown().await;
    let _ = events.send(Event::Closed);
}

/// Returns `false` when the loop must stop.
async fn handle(
    engine: &dyn Engine,
    command: Command,
    events: &mpsc::UnboundedSender<Event>,
) -> bool {
    match command {
        Command::Refresh => {}
        Command::AddBytes(bytes) => report(
            events,
            engine.add(AddTorrent::Metainfo { bytes }).await.map(|_| ()),
        ),
        Command::AddMagnet(uri) => report(
            events,
            engine.add(AddTorrent::Magnet { uri }).await.map(|_| ()),
        ),
        Command::Pause(id) => report(events, engine.pause(&id).await),
        Command::Resume(id) => report(events, engine.resume(&id).await),
        Command::Remove { id, delete_files } => {
            report(events, engine.remove(&id, delete_files).await);
        }
        Command::SetPriority {
            id,
            index,
            priority,
        } => report(
            events,
            engine
                .set_file_priorities(&id, vec![(index, priority)])
                .await,
        ),
        Command::GetDetail(id) => match engine.get(&id).await {
            Ok(detail) => {
                let _ = events.send(Event::Detail(detail));
            }
            Err(error) => {
                let _ = events.send(Event::Error(error.to_string()));
            }
        },
        Command::Play { id, file } => match start_stream(engine, &id, file).await {
            Ok((url, title)) => {
                let _ = events.send(Event::StreamReady { url, title });
            }
            Err(error) => {
                let _ = events.send(Event::Error(error.to_string()));
            }
        },
        Command::Shutdown => return false,
    }
    true
}

fn report(events: &mpsc::UnboundedSender<Event>, result: Result<(), EngineError>) {
    if let Err(error) = result {
        let _ = events.send(Event::Error(error.to_string()));
    }
}

/// Points the picker at the file being played, then asks the engine for a
/// loopback stream URL.
async fn start_stream(
    engine: &dyn Engine,
    id: &str,
    file: usize,
) -> Result<(String, String), EngineError> {
    let detail = engine.get(id).await?;
    let entry = detail
        .files
        .iter()
        .find(|entry| entry.index == file)
        .ok_or(EngineError::InvalidFileIndex(file))?;
    let piece_length = detail.piece_length.max(1);
    let start_piece = u32::try_from(entry.offset / piece_length).unwrap_or(0);
    engine
        .set_streaming_mode(id, flux_api::StreamingMode::Sequential)
        .await?;
    engine.set_playhead(id, start_piece).await?;
    let url = engine.stream_url(id, file).await?;
    Ok((url, entry.path.clone()))
}

async fn push_snapshot(engine: &dyn Engine, events: &mpsc::UnboundedSender<Event>) {
    let torrents: Vec<TorrentSummary> = match engine.list().await {
        Ok(torrents) => torrents,
        Err(error) => {
            let _ = events.send(Event::Error(error.to_string()));
            return;
        }
    };
    let stats = match engine.stats().await {
        Ok(stats) => stats,
        Err(error) => {
            let _ = events.send(Event::Error(error.to_string()));
            return;
        }
    };
    let _ = events.send(Event::Snapshot { torrents, stats });
}
