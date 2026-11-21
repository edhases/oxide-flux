//! WebSocket event stream at `GET /ws`.
//!
//! Authentication is identical to `POST /rpc`: a bearer token in the
//! `Authorization` header or a `token` query parameter. Query tokens are
//! supported for browser clients that cannot set headers on a WebSocket; they
//! can end up in access logs, so native clients should prefer the header.
//!
//! After the upgrade the server only sends JSON-RPC **notifications**. Incoming
//! text frames are ignored in v1 (mutations go through `POST /rpc`); ping frames
//! are answered with pong. If a client falls more than [`crate::http::EVENT_BUFFER`]
//! events behind, its connection is closed instead of buffering without bound.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::{broadcast, watch};

use crate::engine::TorrentSummary;
use crate::http::{AppState, is_authorized, unauthorized};
use crate::protocol::Notification;

/// Largest inbound WebSocket message accepted from a client. Clients do not
/// send RPC over this socket in v1, so the cap only needs to cover control
/// frames and a little slack.
const MAX_CLIENT_MESSAGE_BYTES: usize = 64 * 1024;

/// An event published by the daemon and forwarded to WebSocket clients.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A torrent's summary changed.
    TorrentUpdated(TorrentSummary),
    /// A torrent was removed.
    TorrentRemoved {
        /// Id of the removed torrent.
        id: String,
    },
    /// The daemon is shutting down.
    SessionShutdown,
}

impl Event {
    /// The JSON-RPC notification method for this event.
    pub fn method(&self) -> &'static str {
        match self {
            Self::TorrentUpdated(_) => "torrent.updated",
            Self::TorrentRemoved { .. } => "torrent.removed",
            Self::SessionShutdown => "session.shutdown",
        }
    }

    /// Convert to a notification. Returns `None` if the payload cannot be
    /// represented as JSON (for example a non-finite progress value), so one
    /// malformed event never tears down a client connection.
    pub fn to_notification(&self) -> Option<Notification> {
        let params = match self {
            Self::TorrentUpdated(summary) => {
                // serde_json would turn NaN/infinity into `null`, which would
                // silently corrupt the event. Drop malformed summaries instead.
                if !summary.progress.is_finite()
                    || !(0.0..=1.0).contains(&summary.progress)
                    || summary.pieces_have > summary.pieces_total
                {
                    return None;
                }
                serde_json::to_value(summary).ok()?
            }
            Self::TorrentRemoved { id } => {
                let mut params = Map::new();
                params.insert("id".to_owned(), Value::String(id.clone()));
                Value::Object(params)
            }
            Self::SessionShutdown => Value::Object(Map::new()),
        };
        Some(Notification::new(self.method(), Some(params)))
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WsQuery {
    token: Option<String>,
}

pub(crate) async fn handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !crate::http::request_is_local(state.bind(), &headers) {
        return crate::http::forbidden("cross-site websocket refused");
    }
    if !is_authorized(state.token(), &headers, query.token.as_deref()) {
        return unauthorized();
    }

    let events = state.subscribe();
    let shutdown = state.shutdown_receiver();
    ws.max_message_size(MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| client_loop(socket, events, shutdown))
}

async fn client_loop(
    mut socket: WebSocket,
    mut events: broadcast::Receiver<Event>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            return;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Text(_) | Message::Binary(_))) => {
                        // Client-to-server RPC is not part of the v1 event
                        // stream; mutations use POST /rpc.
                    }
                    Some(Err(_)) => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        let Some(notification) = event.to_notification() else {
                            continue;
                        };
                        let Ok(text) = serde_json::to_string(&notification) else {
                            continue;
                        };
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // The client cannot keep up. Drop it rather than queue
                        // events in memory for it; reconnect resynchronizes via
                        // torrent.list.
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TorrentState;
    use serde_json::json;

    fn summary() -> TorrentSummary {
        TorrentSummary {
            id: "abc".to_owned(),
            name: "example".to_owned(),
            state: TorrentState::Downloading,
            private: true,
            progress: 0.5,
            pieces_have: 2,
            pieces_total: 4,
            total_bytes: 4096,
            peers: 0,
            download_rate: 0,
            upload_rate: 0,
            last_error: None,
        }
    }

    #[test]
    fn event_notification_shapes() -> Result<(), Box<dyn std::error::Error>> {
        let updated = Event::TorrentUpdated(summary())
            .to_notification()
            .ok_or("updated event should serialize")?;
        let value = serde_json::to_value(updated)?;
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "torrent.updated");
        assert_eq!(value["params"]["id"], "abc");
        assert_eq!(value["params"]["private"], true);
        assert!(value.get("id").is_none());

        let removed = Event::TorrentRemoved {
            id: "abc".to_owned(),
        }
        .to_notification()
        .ok_or("removed event should serialize")?;
        assert_eq!(
            serde_json::to_value(removed)?["params"],
            json!({"id": "abc"})
        );

        let shutdown = Event::SessionShutdown
            .to_notification()
            .ok_or("shutdown event should serialize")?;
        assert_eq!(serde_json::to_value(shutdown)?["params"], json!({}));
        Ok(())
    }

    #[test]
    fn non_finite_progress_is_not_sent() {
        let mut bad = summary();
        bad.progress = f64::NAN;
        assert!(Event::TorrentUpdated(bad).to_notification().is_none());
    }
}
