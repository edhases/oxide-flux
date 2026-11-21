//! HTTP transport: `POST /rpc` and `GET /ws`.
//!
//! JSON-RPC responses always use HTTP `200`, including parse errors and
//! invalid requests. JSON-RPC treats those as protocol-level results, not
//! transport failures, and clients parse the error object from the body.
//! Transport-level conditions keep their HTTP meaning:
//!
//! - `401 Unauthorized` — missing or wrong bearer token.
//! - `413 Payload Too Large` — the body exceeds [`ServerConfig::max_body_bytes`].
//! - `204 No Content` — a notification was accepted (it has no response).
//!
//! Authentication is enabled whenever [`ServerConfig::token`] is `Some` and
//! disabled on loopback when it is `None`. Non-loopback binds require a token
//! (see [`ServerConfig::validate`]).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, watch};

use crate::engine::Engine;
use crate::methods::Dispatcher;
use crate::protocol::{Id, Request, Response as RpcResponse, RpcError};
use crate::ws::Event;

/// Default body cap: enough for large `.torrent` files, small enough to bound
/// memory per request.
pub const DEFAULT_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Per-connection event buffer. A WebSocket client that falls further behind
/// than this is disconnected rather than buffered without limit.
pub const EVENT_BUFFER: usize = 256;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind. Defaults to `127.0.0.1:0` (loopback, ephemeral port);
    /// the daemon should set a concrete port.
    pub bind: SocketAddr,
    /// Bearer token required from clients. Mandatory for non-loopback binds.
    pub token: Option<String>,
    /// Maximum request body size in bytes.
    pub max_body_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            token: None,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

impl ServerConfig {
    /// Default configuration: loopback, ephemeral port, no token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the bearer token.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Set the request body cap.
    #[must_use]
    pub fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }

    /// Reject unsafe combinations before binding.
    ///
    /// Binding beyond loopback without a token would expose the control plane
    /// to every host that can reach the socket, so it is refused outright.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_body_bytes == 0 {
            return Err(ConfigError::ZeroBodyLimit);
        }
        if self
            .token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty())
        {
            return Err(ConfigError::EmptyToken);
        }
        if !self.bind.ip().is_loopback() && self.token.is_none() {
            return Err(ConfigError::NonLoopbackWithoutToken { addr: self.bind });
        }
        Ok(())
    }
}

/// Configuration refused before any socket is bound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A wildcard or routable bind without authentication.
    #[error("refusing to bind non-loopback address {addr} without an explicit bearer token")]
    NonLoopbackWithoutToken {
        /// The rejected bind address.
        addr: SocketAddr,
    },
    /// An empty token can never authenticate a request.
    #[error("bearer token must not be empty")]
    EmptyToken,
    /// A zero body cap would reject every request.
    #[error("max_body_bytes must be greater than zero")]
    ZeroBodyLimit,
}

/// Shared transport state.
///
/// [`crate::serve`] constructs this with live event and shutdown channels. The
/// public [`AppState::new`] constructor is for embedding tests where no server
/// lifecycle is running.
pub struct AppState {
    dispatcher: Dispatcher,
    token: Option<String>,
    max_body_bytes: usize,
    bind: SocketAddr,
    events: broadcast::Sender<Event>,
    shutdown: watch::Receiver<bool>,
    _shutdown_keepalive: Option<watch::Sender<bool>>,
}

impl AppState {
    /// Build state with private, never-signalled channels.
    pub fn new(engine: Arc<dyn Engine>, config: &ServerConfig) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut state = Self::with_channels(engine, config, events, shutdown_rx);
        state._shutdown_keepalive = Some(shutdown_tx);
        state
    }

    pub(crate) fn with_channels(
        engine: Arc<dyn Engine>,
        config: &ServerConfig,
        events: broadcast::Sender<Event>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        Self {
            dispatcher: Dispatcher::new(engine),
            token: config.token.clone(),
            max_body_bytes: config.max_body_bytes,
            bind: config.bind,
            events,
            shutdown,
            _shutdown_keepalive: None,
        }
    }

    /// The broadcast sender for engine events.
    pub fn events(&self) -> &broadcast::Sender<Event> {
        &self.events
    }

    pub(crate) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The address the server bound, used for same-origin checks.
    pub(crate) fn bind(&self) -> SocketAddr {
        self.bind
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub(crate) fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown.clone()
    }
}

/// Build the Axum router. This is public so transports and tests can mount the
/// API without going through [`crate::serve`].
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/index.html", get(index_html))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/rpc", post(rpc))
        .route("/ws", get(crate::ws::handler))
        .with_state(state)
}

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

/// The embedded UI is public: it contains no data, and every data call it makes
/// still needs the bearer token.
fn asset(body: &'static str, content_type: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'self'; connect-src 'self' ws: wss:; \
                     img-src 'self' data:; style-src 'self'; script-src 'self'",
                ),
            ),
        ],
        body,
    )
}

async fn index_html() -> impl IntoResponse {
    asset(INDEX_HTML, "text/html; charset=utf-8")
}

async fn app_js() -> impl IntoResponse {
    asset(APP_JS, "application/javascript; charset=utf-8")
}

async fn style_css() -> impl IntoResponse {
    asset(STYLE_CSS, "text/css; charset=utf-8")
}

async fn rpc(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Response {
    if !request_is_local(state.bind, &headers) {
        return forbidden("cross-site request refused");
    }
    if !is_authorized(state.token(), &headers, None) {
        return unauthorized();
    }
    if !is_json_content_type(&headers) {
        return unsupported_media_type();
    }
    if content_length_exceeds(&headers, state.max_body_bytes) {
        return payload_too_large();
    }
    let bytes = match to_bytes(body, state.max_body_bytes).await {
        Ok(bytes) => bytes,
        // `to_bytes` fails when the body crosses the limit (including chunked
        // bodies without a usable Content-Length), or when the stream errors.
        // Either way the request is not processable.
        Err(_) => return payload_too_large(),
    };

    let request = match Request::from_slice(&bytes) {
        Ok(request) => request,
        Err(error) => {
            let error = RpcError::from(error);
            return Json(RpcResponse::error(Id::Null, error)).into_response();
        }
    };

    match state.dispatcher.handle(request).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// True when the request is authorized. An unset token disables auth (only
/// reachable on loopback because of [`ServerConfig::validate`]).
pub(crate) fn is_authorized(
    expected: Option<&str>,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if query_token.is_some_and(|provided| tokens_match(expected, provided)) {
        return true;
    }
    bearer_token(headers).is_some_and(|provided| tokens_match(expected, provided))
}

fn tokens_match(expected: &str, provided: &str) -> bool {
    // Constant-time over the contents. Length is necessarily observable; token
    // length is not secret.
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(token.trim())
    } else {
        None
    }
}

/// Rejects browsers on another origin and DNS-rebinding host names.
///
/// Local apps are a CSRF target: a page on any site can POST `text/plain` to
/// `127.0.0.1` without a preflight. The defence is to require the request to
/// look same-origin (matching `Origin`/`Host`, both loopback names when the
/// server is loopback) and to require `application/json`, which forces a CORS
/// preflight that this server never grants.
pub(crate) fn request_is_local(bind: SocketAddr, headers: &HeaderMap) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(host_without_port);
    if let Some(host) = host {
        if bind.ip().is_loopback() && !is_loopback_name(host) {
            return false;
        }
        if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            let Some(origin_host) = origin_host(origin) else {
                return false;
            };
            if !origin_host.eq_ignore_ascii_case(host) {
                return false;
            }
        }
    } else if headers.contains_key(header::ORIGIN) {
        // Browsers always send Host; an Origin without one cannot be verified.
        return false;
    }
    true
}

fn host_without_port(value: &str) -> &str {
    if let Some(rest) = value.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        return &value[..end + 2];
    }
    value.split(':').next().unwrap_or(value)
}

fn origin_host(origin: &str) -> Option<&str> {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    Some(host_without_port(rest))
}

fn is_loopback_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "::1"
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
}

pub(crate) fn forbidden(reason: &'static str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "error": reason }))).into_response()
}

pub(crate) fn unsupported_media_type() -> Response {
    (
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        Json(json!({ "error": "Content-Type must be application/json" })),
    )
        .into_response()
}

fn content_length_exceeds(headers: &HeaderMap, limit: usize) -> bool {
    let limit = u64::try_from(limit).unwrap_or(u64::MAX);
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > limit)
}

pub(crate) fn unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response();
    let _ = response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn payload_too_large() -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({ "error": "request body exceeds the configured limit" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_exactly() {
        assert!(tokens_match("secret", "secret"));
        assert!(!tokens_match("secret", "secre"));
        assert!(!tokens_match("secret", "secreu"));
        assert!(!tokens_match("secret", "Secret"));
    }

    #[test]
    fn config_refuses_non_loopback_without_token() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig {
            bind: "0.0.0.0:0".parse()?,
            ..ServerConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(ConfigError::NonLoopbackWithoutToken { addr: config.bind })
        );

        let config = config.with_token("secret");
        assert_eq!(config.validate(), Ok(()));
        Ok(())
    }

    #[test]
    fn config_refuses_empty_token_and_zero_limit() {
        let config = ServerConfig::default().with_token("   ");
        assert_eq!(config.validate(), Err(ConfigError::EmptyToken));

        let config = ServerConfig::default().with_max_body_bytes(0);
        assert_eq!(config.validate(), Err(ConfigError::ZeroBodyLimit));
    }
}
