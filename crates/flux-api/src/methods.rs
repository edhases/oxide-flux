//! JSON-RPC method table and parameter validation.
//!
//! Every method maps to one [`Engine`] call. Parameters are deserialized with
//! serde and reject unknown fields, so a typo in a UI request fails with
//! `-32602` instead of being silently ignored. Engine failures are translated
//! into the application error range (`-32000..=-32099`).

use std::collections::HashSet;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{AddTorrent, Engine, EngineError, Priority, StreamingMode};
use crate::protocol::{Request, Response, RpcError, codes};

/// Method name for [`Engine::info`].
pub const SESSION_INFO: &str = "session.info";
/// Method name for [`Engine::stats`].
pub const SESSION_STATS: &str = "session.stats";
/// Method name for [`Engine::list`].
pub const TORRENT_LIST: &str = "torrent.list";
/// Method name for [`Engine::get`].
pub const TORRENT_GET: &str = "torrent.get";
/// Method name for [`Engine::add`].
pub const TORRENT_ADD: &str = "torrent.add";
/// Method name for [`Engine::pause`].
pub const TORRENT_PAUSE: &str = "torrent.pause";
/// Method name for [`Engine::resume`].
pub const TORRENT_RESUME: &str = "torrent.resume";
/// Method name for [`Engine::remove`].
pub const TORRENT_REMOVE: &str = "torrent.remove";
/// Method name for [`Engine::set_file_priorities`].
pub const TORRENT_SET_FILE_PRIORITIES: &str = "torrent.setFilePriorities";
/// Method name for [`Engine::set_playhead`].
pub const TORRENT_SET_PLAYHEAD: &str = "torrent.setPlayhead";
/// Method name for [`Engine::set_streaming_mode`].
pub const TORRENT_SET_STREAMING_MODE: &str = "torrent.setStreamingMode";
/// Method name for [`Engine::stream_url`].
pub const TORRENT_STREAM_URL: &str = "torrent.streamUrl";

/// Dispatches validated JSON-RPC requests to an engine.
#[derive(Clone)]
pub struct Dispatcher {
    engine: Arc<dyn Engine>,
}

impl Dispatcher {
    /// Wrap an engine.
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self { engine }
    }

    /// Execute a method and return its JSON result.
    pub async fn dispatch(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            SESSION_INFO => {
                let _: EmptyParams = parse_params(params)?;
                serialize_result(self.engine.info().await.map_err(engine_error)?)
            }
            SESSION_STATS => {
                let _: EmptyParams = parse_params(params)?;
                serialize_result(self.engine.stats().await.map_err(engine_error)?)
            }
            TORRENT_LIST => {
                let _: EmptyParams = parse_params(params)?;
                serialize_result(self.engine.list().await.map_err(engine_error)?)
            }
            TORRENT_GET => {
                let params: IdParams = parse_params(params)?;
                let id = require_id(params.id)?;
                serialize_result(self.engine.get(&id).await.map_err(engine_error)?)
            }
            TORRENT_ADD => {
                let source: AddTorrent = parse_params(params)?;
                validate_add(&source)?;
                serialize_result(self.engine.add(source).await.map_err(engine_error)?)
            }
            TORRENT_PAUSE => {
                let params: IdParams = parse_params(params)?;
                let id = require_id(params.id)?;
                self.engine.pause(&id).await.map_err(engine_error)?;
                Ok(Value::Null)
            }
            TORRENT_RESUME => {
                let params: IdParams = parse_params(params)?;
                let id = require_id(params.id)?;
                self.engine.resume(&id).await.map_err(engine_error)?;
                Ok(Value::Null)
            }
            TORRENT_REMOVE => {
                let params: RemoveParams = parse_params(params)?;
                let id = require_id(params.id)?;
                self.engine
                    .remove(&id, params.delete_files)
                    .await
                    .map_err(engine_error)?;
                Ok(Value::Null)
            }
            TORRENT_SET_FILE_PRIORITIES => {
                let params: SetFilePrioritiesParams = parse_params(params)?;
                let id = require_id(params.id)?;
                let mut seen = HashSet::with_capacity(params.priorities.len());
                let mut updates = Vec::with_capacity(params.priorities.len());
                for update in params.priorities {
                    if !seen.insert(update.index) {
                        return Err(RpcError::invalid_params(format!(
                            "duplicate file index {}",
                            update.index
                        )));
                    }
                    updates.push((update.index, update.priority));
                }
                self.engine
                    .set_file_priorities(&id, updates)
                    .await
                    .map_err(engine_error)?;
                Ok(Value::Null)
            }
            TORRENT_SET_PLAYHEAD => {
                let params: SetPlayheadParams = parse_params(params)?;
                let id = require_id(params.id)?;
                self.engine
                    .set_playhead(&id, params.piece)
                    .await
                    .map_err(engine_error)?;
                Ok(Value::Null)
            }
            TORRENT_SET_STREAMING_MODE => {
                let params: SetStreamingModeParams = parse_params(params)?;
                let id = require_id(params.id)?;
                self.engine
                    .set_streaming_mode(&id, params.mode)
                    .await
                    .map_err(engine_error)?;
                Ok(Value::Null)
            }
            TORRENT_STREAM_URL => {
                let params: StreamUrlParams = parse_params(params)?;
                let id = require_id(params.id)?;
                let url = self
                    .engine
                    .stream_url(&id, params.file)
                    .await
                    .map_err(engine_error)?;
                Ok(Value::String(url))
            }
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    /// Handle one request, returning `None` for notifications.
    ///
    /// Notifications are still executed; only the response is suppressed.
    pub async fn handle(&self, request: Request) -> Option<Response> {
        let Request { method, params, id } = request;
        let result = self.dispatch(&method, params).await;
        let id = id?;
        Some(match result {
            Ok(value) => Response::success(id, value),
            Err(error) => Response::error(id, error),
        })
    }
}

fn parse_params<P: DeserializeOwned>(params: Option<Value>) -> Result<P, RpcError> {
    let value = params.unwrap_or_else(|| json!({}));
    serde_json::from_value(value).map_err(|error| RpcError::invalid_params(error.to_string()))
}

fn serialize_result<T: Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::internal(format!("failed to serialize result: {error}")))
}

fn require_id(id: String) -> Result<String, RpcError> {
    if id.is_empty() {
        Err(RpcError::invalid_params("id must not be empty"))
    } else {
        Ok(id)
    }
}

fn validate_add(source: &AddTorrent) -> Result<(), RpcError> {
    match source {
        AddTorrent::Metainfo { bytes, .. } if bytes.is_empty() => {
            Err(RpcError::invalid_params("metainfo must not be empty"))
        }
        AddTorrent::Magnet { uri } if !uri.starts_with("magnet:?") => Err(
            RpcError::invalid_params("magnet uri must start with \"magnet:?\""),
        ),
        AddTorrent::Metainfo { .. } | AddTorrent::Magnet { .. } => Ok(()),
    }
}

fn engine_error(error: EngineError) -> RpcError {
    match error {
        EngineError::NotFound(id) => RpcError::application(
            codes::APP_TORRENT_NOT_FOUND,
            format!("torrent not found: {id}"),
        ),
        EngineError::Duplicate(id) => RpcError::application(
            codes::APP_DUPLICATE_TORRENT,
            format!("torrent already exists: {id}"),
        ),
        EngineError::InvalidSource(message) => RpcError::application(
            codes::APP_INVALID_TORRENT,
            format!("invalid torrent source: {message}"),
        ),
        EngineError::InvalidFileIndex(index) => RpcError::application(
            codes::APP_INVALID_FILE_INDEX,
            format!("invalid file index: {index}"),
        ),
        EngineError::Unsupported(message) => RpcError::application(
            codes::APP_UNSUPPORTED,
            format!("unsupported operation: {message}"),
        ),
        EngineError::Io(message) => {
            RpcError::application(codes::APP_IO_ERROR, format!("i/o error: {message}"))
        }
        EngineError::Other(message) => RpcError::application(codes::APP_DOMAIN_ERROR, message),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdParams {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RemoveParams {
    id: String,
    #[serde(default)]
    delete_files: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetFilePrioritiesParams {
    id: String,
    priorities: Vec<FilePriorityUpdate>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetPlayheadParams {
    id: String,
    piece: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetStreamingModeParams {
    id: String,
    mode: StreamingMode,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StreamUrlParams {
    id: String,
    file: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FilePriorityUpdate {
    index: usize,
    priority: Priority,
}
