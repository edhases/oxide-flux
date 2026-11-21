//! Strict JSON-RPC 2.0 envelope types and validation.
//!
//! The control plane accepts exactly one JSON-RPC request per HTTP `POST /rpc`.
//! Batch requests (JSON arrays) are **not supported in v1** and are rejected
//! with [`codes::INVALID_REQUEST`]; a future version may add batched responses.
//!
//! Validation is intentionally narrow:
//!
//! - `jsonrpc` must be the string `"2.0"`.
//! - `method` must be a non-empty string.
//! - `params`, if present, must be an object or an array (the spec's
//!   "structured value"). `null` and scalars are rejected.
//! - `id`, if present, must be a string, an integer, or `null`. Fractional
//!   numbers and other types are rejected. An absent `id` marks a
//!   notification; an explicit `null` id is a request whose response echoes
//!   `null`.
//! - Unknown top-level members are ignored, per JSON-RPC's extension model.
//!
//! Parse failures produce [`codes::PARSE_ERROR`]; every other validation
//! failure produces [`codes::INVALID_REQUEST`]. Both travel over HTTP with
//! status `200` (see the crate-level docs).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only JSON-RPC version this crate speaks.
pub const JSONRPC_VERSION: &str = "2.0";

/// Standard JSON-RPC 2.0 error codes plus the application range used by the
/// engine-facing methods.
pub mod codes {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON was valid but not a valid JSON-RPC request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The requested method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The method exists but the parameters are invalid.
    pub const INVALID_PARAMS: i64 = -32602;
    /// The server hit an unexpected internal failure.
    pub const INTERNAL_ERROR: i64 = -32603;

    /// Lowest implementation-defined server error code (inclusive).
    pub const SERVER_ERROR_MIN: i64 = -32099;
    /// Highest implementation-defined server error code (inclusive).
    pub const SERVER_ERROR_MAX: i64 = -32000;

    /// A torrent id was not present in the session.
    pub const APP_TORRENT_NOT_FOUND: i64 = -32001;
    /// The torrent is already in the session.
    pub const APP_DUPLICATE_TORRENT: i64 = -32002;
    /// The supplied metainfo or magnet URI is not usable.
    pub const APP_INVALID_TORRENT: i64 = -32003;
    /// The engine failed while touching storage.
    pub const APP_IO_ERROR: i64 = -32004;
    /// The engine does not support the requested operation.
    pub const APP_UNSUPPORTED: i64 = -32005;
    /// A file index in a priority update does not exist.
    pub const APP_INVALID_FILE_INDEX: i64 = -32006;
    /// Generic domain failure that does not fit a more specific code.
    pub const APP_DOMAIN_ERROR: i64 = -32000;
}

/// A JSON-RPC request id: string, integer, or `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// A string id.
    String(String),
    /// An integer id.
    Number(i64),
    /// An explicit `null` id. Responses echo it as `null`.
    Null,
}

/// Why a byte slice or JSON value is not a valid JSON-RPC request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The input was not JSON at all.
    #[error("parse error: {0}")]
    Parse(String),
    /// The input was JSON but violated the request shape.
    #[error("invalid request: {0}")]
    Invalid(String),
}

impl RequestError {
    /// The JSON-RPC error code for this failure.
    pub fn code(&self) -> i64 {
        match self {
            Self::Parse(_) => codes::PARSE_ERROR,
            Self::Invalid(_) => codes::INVALID_REQUEST,
        }
    }

    /// Convert this failure into a wire error object.
    pub fn into_rpc_error(self) -> RpcError {
        RpcError::new(self.code(), self.to_string())
    }
}

/// A validated JSON-RPC 2.0 request.
///
/// The `id` field is `None` for notifications and `Some(Id::Null)` when the
/// client explicitly sent `"id": null`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The method name.
    pub method: String,
    /// Structured parameters, if any.
    pub params: Option<Value>,
    /// Request id; `None` means the request is a notification.
    pub id: Option<Id>,
}

impl Request {
    /// Parse and validate one JSON request document.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, RequestError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| RequestError::Parse(error.to_string()))?;
        Self::from_value(value)
    }

    /// Validate an already-parsed JSON value.
    pub fn from_value(value: Value) -> Result<Self, RequestError> {
        let mut object = match value {
            Value::Object(object) => object,
            Value::Array(_) => {
                return Err(RequestError::Invalid(
                    "batch requests are not supported in v1".to_owned(),
                ));
            }
            _ => {
                return Err(RequestError::Invalid(
                    "request must be a JSON object".to_owned(),
                ));
            }
        };

        match object.remove("jsonrpc") {
            Some(Value::String(version)) if version == JSONRPC_VERSION => {}
            Some(Value::String(_)) => {
                return Err(RequestError::Invalid(format!(
                    "jsonrpc must be the string \"{JSONRPC_VERSION}\""
                )));
            }
            Some(_) => {
                return Err(RequestError::Invalid("jsonrpc must be a string".to_owned()));
            }
            None => {
                return Err(RequestError::Invalid("missing jsonrpc member".to_owned()));
            }
        }

        let method = match object.remove("method") {
            Some(Value::String(method)) if !method.is_empty() => method,
            Some(Value::String(_)) => {
                return Err(RequestError::Invalid("method must not be empty".to_owned()));
            }
            Some(_) => {
                return Err(RequestError::Invalid("method must be a string".to_owned()));
            }
            None => {
                return Err(RequestError::Invalid("missing method member".to_owned()));
            }
        };

        let params = match object.remove("params") {
            None => None,
            Some(value @ Value::Object(_)) | Some(value @ Value::Array(_)) => Some(value),
            Some(_) => {
                return Err(RequestError::Invalid(
                    "params must be an object or an array".to_owned(),
                ));
            }
        };

        let id = match object.remove("id") {
            None => None,
            Some(value) => Some(serde_json::from_value::<Id>(value).map_err(|_| {
                RequestError::Invalid("id must be a string, an integer, or null".to_owned())
            })?),
        };

        Ok(Self { method, params, id })
    }

    /// Whether this request must be handled without a response.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    /// Numeric error code. Standard codes and the application range are in
    /// [`codes`].
    pub code: i64,
    /// Human-readable explanation.
    pub message: String,
    /// Optional structured detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Build an error with an arbitrary code.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach structured detail.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// `-32700`: the request body was not valid JSON.
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(codes::PARSE_ERROR, message)
    }

    /// `-32600`: the JSON was valid but not a JSON-RPC request.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_REQUEST, message)
    }

    /// `-32601`: no such method.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            codes::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )
    }

    /// `-32602`: the method exists but the params did not deserialize or failed
    /// a local structural check.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    /// `-32603`: an unexpected failure while handling a valid request.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL_ERROR, message)
    }

    /// An implementation-defined domain error. Prefer one of the
    /// [`codes`] `APP_*` constants; `code` should be in
    /// `codes::SERVER_ERROR_MIN..=codes::SERVER_ERROR_MAX`.
    pub fn application(code: i64, message: impl Into<String>) -> Self {
        Self::new(code, message)
    }
}

impl From<RequestError> for RpcError {
    fn from(error: RequestError) -> Self {
        error.into_rpc_error()
    }
}

/// A JSON-RPC response. Exactly one of `result` or `error` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Echo of the request id.
    pub id: Id,
    /// Successful result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Build a success response.
    pub fn success(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response.
    pub fn error(id: Id, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// Whether this response carries an error.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// A server-initiated JSON-RPC notification (no `id`, no response).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Event method name, for example `torrent.updated`.
    pub method: String,
    /// Event payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Build a notification.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn invalid_code(input: &str) -> Option<i64> {
        Request::from_slice(input.as_bytes())
            .err()
            .map(|error| error.code())
    }

    #[test]
    fn rejects_malformed_json_as_parse_error() {
        for input in ["", "{", "not json", "{\"jsonrpc\":}"] {
            assert_eq!(
                invalid_code(input),
                Some(codes::PARSE_ERROR),
                "input {input:?} should be a parse error"
            );
        }
    }

    #[test]
    fn rejects_invalid_request_shapes() {
        let cases = [
            "{}",
            "[]",
            "[{}]",
            "null",
            "42",
            r#"{"jsonrpc":"1.0","method":"session.info"}"#,
            r#"{"jsonrpc":2.0,"method":"session.info"}"#,
            r#"{"jsonrpc":"2.0"}"#,
            r#"{"jsonrpc":"2.0","method":42}"#,
            r#"{"jsonrpc":"2.0","method":""}"#,
            r#"{"jsonrpc":"2.0","method":"session.info","params":null}"#,
            r#"{"jsonrpc":"2.0","method":"session.info","params":"x"}"#,
            r#"{"jsonrpc":"2.0","method":"session.info","params":7}"#,
            r#"{"jsonrpc":"2.0","method":"session.info","id":1.5}"#,
            r#"{"jsonrpc":"2.0","method":"session.info","id":true}"#,
            r#"{"jsonrpc":"2.0","method":"session.info","id":{}}"#,
            r#"{"jsonrpc":"2.0","method":"session.info","id":[1]}"#,
        ];
        for input in cases {
            assert_eq!(
                invalid_code(input),
                Some(codes::INVALID_REQUEST),
                "input {input} should be invalid"
            );
        }
    }

    #[test]
    fn batch_rejection_names_batching() {
        let error = Request::from_slice(b"[]").err();
        let message = error.map(|error| error.to_string()).unwrap_or_default();
        assert!(message.contains("batch"), "message was {message:?}");
    }

    #[test]
    fn accepts_structural_params_and_ignores_unknown_members()
    -> Result<(), Box<dyn std::error::Error>> {
        let object = Request::from_slice(
            br#"{"jsonrpc":"2.0","method":"m","params":{"a":1},"extra":"ignored"}"#,
        )?;
        assert_eq!(object.params, Some(json!({"a": 1})));
        assert!(object.is_notification());

        let array = Request::from_slice(br#"{"jsonrpc":"2.0","method":"m","params":[1,2]}"#)?;
        assert_eq!(array.params, Some(json!([1, 2])));
        Ok(())
    }

    #[test]
    fn echoes_string_number_and_null_ids() -> Result<(), Box<dyn std::error::Error>> {
        let cases = [
            (
                r#"{"jsonrpc":"2.0","method":"m","id":"abc"}"#,
                Id::String("abc".into()),
            ),
            (r#"{"jsonrpc":"2.0","method":"m","id":42}"#, Id::Number(42)),
            (r#"{"jsonrpc":"2.0","method":"m","id":null}"#, Id::Null),
        ];
        for (input, expected) in cases {
            let request = Request::from_slice(input.as_bytes())?;
            let id = request.id.ok_or("request should have an id")?;
            assert_eq!(id, expected);
            let response = Response::success(id.clone(), json!("ok"));
            let value = serde_json::to_value(&response)?;
            assert_eq!(value["jsonrpc"], "2.0");
            assert_eq!(value["id"], serde_json::to_value(&expected)?);
            assert_eq!(value["result"], "ok");
            assert!(value.get("error").is_none());
        }
        Ok(())
    }

    #[test]
    fn absent_id_is_a_notification() -> Result<(), Box<dyn std::error::Error>> {
        let request = Request::from_slice(br#"{"jsonrpc":"2.0","method":"m"}"#)?;
        assert!(request.is_notification());
        assert_eq!(request.id, None);
        Ok(())
    }

    #[test]
    fn explicit_null_id_is_not_a_notification() -> Result<(), Box<dyn std::error::Error>> {
        let request = Request::from_slice(br#"{"jsonrpc":"2.0","method":"m","id":null}"#)?;
        assert!(!request.is_notification());
        assert_eq!(request.id, Some(Id::Null));
        Ok(())
    }

    #[test]
    fn error_response_omits_result_and_optional_data() -> Result<(), Box<dyn std::error::Error>> {
        let response = Response::error(Id::Number(7), RpcError::invalid_params("bad id"));
        let value = serde_json::to_value(&response)?;
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["error"]["code"], codes::INVALID_PARAMS);
        assert_eq!(value["error"]["message"], "bad id");
        assert!(value["error"].get("data").is_none());
        assert!(value.get("result").is_none());
        assert!(response.is_error());
        Ok(())
    }

    #[test]
    fn notification_wire_shape() -> Result<(), Box<dyn std::error::Error>> {
        let notification = Notification::new("torrent.removed", Some(json!({"id": "abc"})));
        let value = serde_json::to_value(&notification)?;
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "torrent.removed");
        assert_eq!(value["params"]["id"], "abc");
        assert!(value.get("id").is_none());
        Ok(())
    }

    #[test]
    fn request_error_codes() {
        assert_eq!(
            RequestError::Parse("bad".into()).into_rpc_error().code,
            codes::PARSE_ERROR
        );
        assert_eq!(
            RequestError::Invalid("bad".into()).into_rpc_error().code,
            codes::INVALID_REQUEST
        );
    }
}
