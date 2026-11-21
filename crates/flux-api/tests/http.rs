//! HTTP transport tests: auth, body caps, and JSON-RPC status conventions.
//! No sockets are bound: the router is driven with `tower::ServiceExt`.

use std::error::Error;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request as HttpRequest, StatusCode, header};
use flux_api::{AppState, MockEngine, ServerConfig, codes, router};
use serde_json::Value;
use tower::ServiceExt;

const VALID: &str = r#"{"jsonrpc":"2.0","id":1,"method":"session.info"}"#;

fn build(token: Option<&str>, max_body_bytes: usize) -> Router {
    let config = ServerConfig {
        token: token.map(|token| token.to_owned()),
        max_body_bytes,
        ..ServerConfig::default()
    };
    let state = Arc::new(AppState::new(Arc::new(MockEngine::new()), &config));
    router(state)
}

fn rpc(body: &str, token: Option<&str>) -> Result<HttpRequest<Body>, Box<dyn Error>> {
    let mut builder = HttpRequest::builder()
        .method("POST")
        .uri("/rpc")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    Ok(builder.body(Body::from(body.to_owned()))?)
}

async fn json_body(response: axum::response::Response) -> Result<Value, Box<dyn Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn cross_site_and_non_json_requests_are_rejected() -> Result<(), Box<dyn Error>> {
    let app = build(None, 1024);

    // A browser "simple request" (text/plain) must not reach the dispatcher.
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/rpc")
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::HOST, "127.0.0.1:6800")
        .body(Body::from(VALID))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // A foreign Origin on an otherwise valid JSON request is CSRF.
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/rpc")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "127.0.0.1:6800")
        .header(header::ORIGIN, "http://evil.example")
        .body(Body::from(VALID))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // DNS rebinding: a non-loopback Host on a loopback listener.
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/rpc")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "evil.example")
        .body(Body::from(VALID))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // The UI's own origin still works.
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/rpc")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "127.0.0.1:6800")
        .header(header::ORIGIN, "http://127.0.0.1:6800")
        .body(Body::from(VALID))?;
    let response = app.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn missing_or_wrong_token_is_401() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 1024);
    for token in [None, Some("wrong"), Some("")] {
        let response = app.clone().oneshot(rpc(VALID, token)?).await?;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "token {token:?} should be rejected"
        );
        assert!(
            response.headers().contains_key(header::WWW_AUTHENTICATE),
            "401 must advertise the auth scheme"
        );
    }
    Ok(())
}

#[tokio::test]
async fn correct_token_is_200_and_returns_result() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 1024);
    let response = app.oneshot(rpc(VALID, Some("secret"))?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );

    let value = json_body(response).await?;
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], 1);
    assert_eq!(value["result"]["name"], "oxide-flux");
    assert!(value.get("error").is_none());
    Ok(())
}

#[tokio::test]
async fn loopback_without_token_allows_requests() -> Result<(), Box<dyn Error>> {
    let app = build(None, 1024);
    let response = app.oneshot(rpc(VALID, None)?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn oversized_body_is_413_before_json_parsing() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 32);
    // Deliberately malformed: the size check must win over JSON-RPC parsing.
    let body = "x".repeat(128);
    let response = app.oneshot(rpc(&body, Some("secret"))?).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn oversized_content_length_is_413_without_reading() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 32);
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/rpc")
        .header(header::AUTHORIZATION, "Bearer secret")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, "4096")
        .body(Body::from("{}"))?;
    let response = app.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn malformed_json_is_parse_error_with_http_200() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 1024);
    let response = app.oneshot(rpc("{", Some("secret"))?).await?;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "JSON-RPC parse errors travel with HTTP 200"
    );
    let value = json_body(response).await?;
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], Value::Null);
    assert_eq!(value["error"]["code"], codes::PARSE_ERROR);
    Ok(())
}

#[tokio::test]
async fn invalid_request_and_batch_are_32600_with_http_200() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 1024);
    for body in [
        r#"{"jsonrpc":"1.0","method":"session.info"}"#,
        r#"[{"jsonrpc":"2.0","method":"session.info"}]"#,
    ] {
        let response = app.clone().oneshot(rpc(body, Some("secret"))?).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await?;
        assert_eq!(value["id"], Value::Null);
        assert_eq!(value["error"]["code"], codes::INVALID_REQUEST);
    }
    Ok(())
}

#[tokio::test]
async fn domain_error_is_http_200_with_application_code() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 1024);
    let body = r#"{"jsonrpc":"2.0","id":"x","method":"torrent.get","params":{"id":"missing"}}"#;
    let response = app.oneshot(rpc(body, Some("secret"))?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let value = json_body(response).await?;
    assert_eq!(value["id"], "x");
    assert_eq!(value["error"]["code"], codes::APP_TORRENT_NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn notification_is_204_with_no_body() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"), 1024);
    let body = r#"{"jsonrpc":"2.0","method":"session.info"}"#;
    let response = app.oneshot(rpc(body, Some("secret"))?).await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(bytes.is_empty());
    Ok(())
}
