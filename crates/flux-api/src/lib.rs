//! Control-plane transport for Oxide Flux.
//!
//! This crate is the **only** path between the headless daemon and its UI. It
//! exposes JSON-RPC 2.0 over `POST /rpc` plus a server-push event stream over
//! `GET /ws`, and knows nothing about torrents beyond the [`Engine`] trait.
//!
//! # Wire format
//!
//! Request:
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"method":"torrent.get","params":{"id":"abc"}}
//! ```
//!
//! Success (`id` echoes the request id — string, integer, or `null`):
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"result":{"id":"abc","name":"example","state":"paused","private":false,"progress":0.5,"piecesHave":2,"piecesTotal":4,"totalBytes":4096,"files":[],"pieceLength":0,"downloadDir":""}}
//! ```
//!
//! Error:
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"torrent not found: abc"}}
//! ```
//!
//! Notification (no `id`, no response; sent only on the WebSocket):
//!
//! ```json
//! {"jsonrpc":"2.0","method":"torrent.updated","params":{"id":"abc","name":"example","state":"downloading","private":false,"progress":0.5,"piecesHave":2,"piecesTotal":4,"totalBytes":4096}}
//! ```
//!
//! # Methods
//!
//! | Method | Params | Result |
//! | --- | --- | --- |
//! | `session.info` | `{}` | [`Info`] |
//! | `session.stats` | `{}` | [`Stats`] |
//! | `torrent.list` | `{}` | [`TorrentSummary`] array |
//! | `torrent.get` | `{"id":"…"}` | [`TorrentDetail`] |
//! | `torrent.add` | `{"kind":"metainfo","bytes":[…]}` or `{"kind":"magnet","uri":"magnet:?…"}` | [`TorrentSummary`] |
//! | `torrent.pause` | `{"id":"…"}` | `null` |
//! | `torrent.resume` | `{"id":"…"}` | `null` |
//! | `torrent.remove` | `{"id":"…","deleteFiles":false}` | `null` |
//! | `torrent.setFilePriorities` | `{"id":"…","priorities":[{"index":0,"priority":"high"}]}` | `null` |
//!
//! Unknown methods return `-32601`; bad parameters (including unknown fields)
//! return `-32602`; engine failures use the application range.
//!
//! # Error codes
//!
//! | Code | Meaning |
//! | --- | --- |
//! | `-32700` | Parse error: body was not JSON. |
//! | `-32600` | Invalid request, including batch arrays (unsupported in v1). |
//! | `-32601` | Method not found. |
//! | `-32602` | Invalid params. |
//! | `-32603` | Internal error. |
//! | `-32001` | Torrent not found. |
//! | `-32002` | Duplicate torrent. |
//! | `-32003` | Invalid metainfo or magnet URI. |
//! | `-32004` | Engine I/O failure. |
//! | `-32005` | Unsupported operation. |
//! | `-32006` | Invalid file index. |
//! | `-32000` | Generic domain error. |
//!
//! # HTTP status mapping
//!
//! JSON-RPC results — including `-32700` and `-32600` — use HTTP `200`; the
//! error lives in the JSON body. `401` is reserved for authentication and
//! `413` for a body over [`ServerConfig::max_body_bytes`]. Notifications get
//! `204 No Content`.
//!
//! # WebSocket events
//!
//! [`Event::TorrentUpdated`], [`Event::TorrentRemoved`], and
//! [`Event::SessionShutdown`] become JSON-RPC notifications with methods
//! `torrent.updated`, `torrent.removed`, and `session.shutdown`. A client that
//! lags more than [`EVENT_BUFFER`] events is disconnected; it can resynchronize
//! with `torrent.list`.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use flux_api::{MockEngine, ServerConfig, serve};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let engine = Arc::new(MockEngine::new());
//! let server = serve(
//!     ServerConfig::default().with_token("change-me"),
//!     engine,
//! )
//! .await?;
//! println!("control API on {}", server.local_addr());
//! server.shutdown().await?;
//! # Ok(())
//! # }
//! ```

pub mod engine;
pub mod http;
pub mod methods;
pub mod protocol;
pub mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

pub use engine::{
    AddTorrent, Engine, EngineError, FileView, Info, MockEngine, Priority, Stats, StreamingMode,
    TorrentDetail, TorrentState, TorrentSummary,
};
pub use http::{AppState, ConfigError, DEFAULT_MAX_BODY_BYTES, EVENT_BUFFER, ServerConfig, router};
pub use methods::{
    Dispatcher, SESSION_INFO, SESSION_STATS, TORRENT_ADD, TORRENT_GET, TORRENT_LIST, TORRENT_PAUSE,
    TORRENT_REMOVE, TORRENT_RESUME, TORRENT_SET_FILE_PRIORITIES, TORRENT_SET_PLAYHEAD,
    TORRENT_SET_STREAMING_MODE, TORRENT_STREAM_URL,
};
pub use protocol::{
    Id, JSONRPC_VERSION, Notification, Request, RequestError, Response, RpcError, codes,
};
pub use ws::Event;

/// A running control API.
///
/// Dropping the value signals graceful shutdown but does not wait for it. Call
/// [`Server::shutdown`] when the caller must know the listener has stopped.
pub struct Server {
    local_addr: SocketAddr,
    events: broadcast::Sender<Event>,
    shutdown: watch::Sender<bool>,
    handle: Option<JoinHandle<std::io::Result<()>>>,
}

impl Server {
    /// The actual bound address (useful when configured with port `0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// A clone of the event sender, for components that need to subscribe.
    pub fn events(&self) -> broadcast::Sender<Event> {
        self.events.clone()
    }

    /// Publish an event to every connected WebSocket client.
    ///
    /// Events published with no connected client are dropped; the bounded
    /// broadcast buffer is the backpressure mechanism. Returns the number of
    /// receivers that will see the event (`0` if none).
    pub fn publish(&self, event: Event) -> usize {
        self.events.send(event).unwrap_or(0)
    }

    /// Stop accepting requests, close WebSocket connections, and wait for the
    /// listener task to finish.
    pub async fn shutdown(mut self) -> Result<(), ServerError> {
        let _ = self.shutdown.send(true);
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        match handle.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(source)) => Err(ServerError::Io(source)),
            Err(source) => Err(ServerError::Join(source)),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Failures while starting or stopping the server.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The configuration is unsafe or unusable.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The configured address could not be bound.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// Requested address.
        addr: SocketAddr,
        /// Underlying socket error.
        #[source]
        source: std::io::Error,
    },
    /// The server task failed after binding.
    #[error("server i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The listener task panicked or was cancelled.
    #[error("server task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Bind the control API and start serving in the background.
///
/// Configuration is validated **before** any socket is opened, so an unsafe
/// non-loopback bind without a token fails without exposing an interface.
pub async fn serve(config: ServerConfig, engine: Arc<dyn Engine>) -> Result<Server, ServerError> {
    config.validate()?;

    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|source| ServerError::Bind {
            addr: config.bind,
            source,
        })?;
    let local_addr = listener.local_addr()?;

    let (events, _) = broadcast::channel(EVENT_BUFFER);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(AppState::with_channels(
        engine,
        &config,
        events.clone(),
        shutdown_rx.clone(),
    ));
    let app = http::router(state);

    let mut graceful = shutdown_rx;
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = graceful.changed().await;
            })
            .await
    });

    Ok(Server {
        local_addr,
        events,
        shutdown: shutdown_tx,
        handle: Some(handle),
    })
}
