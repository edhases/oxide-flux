//! The embedded WebUI: public static assets, authenticated data.

use std::error::Error;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request as HttpRequest, StatusCode, header};
use flux_api::{AppState, MockEngine, ServerConfig, router};
use tower::ServiceExt;

fn build(token: Option<&str>) -> axum::Router {
    let config = ServerConfig {
        token: token.map(str::to_owned),
        ..ServerConfig::default()
    };
    let state = Arc::new(AppState::new(Arc::new(MockEngine::new()), &config));
    router(state)
}

async fn get(path: &str) -> Result<(StatusCode, axum::http::HeaderMap, String), Box<dyn Error>> {
    let request = HttpRequest::builder().uri(path).body(Body::empty())?;
    let response = build(Some("secret")).oneshot(request).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, headers, String::from_utf8(bytes.to_vec())?))
}

#[tokio::test]
async fn the_ui_is_served_without_a_token() -> Result<(), Box<dyn Error>> {
    let (status, headers, body) = get("/").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert!(
        headers.contains_key(header::CONTENT_SECURITY_POLICY),
        "the page must ship a CSP"
    );
    assert!(body.contains("Oxide Flux"));
    assert!(body.contains("/app.js"));

    let (status, _, body) = get("/index.html").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Oxide Flux"));
    Ok(())
}

#[tokio::test]
async fn assets_have_the_right_content_types() -> Result<(), Box<dyn Error>> {
    let (status, headers, script) = get("/app.js").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert!(script.contains("torrent.add"));

    let (status, headers, css) = get("/style.css").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    assert!(css.contains(".torrent"));
    Ok(())
}

#[tokio::test]
async fn ui_assets_do_not_open_the_data_endpoints() -> Result<(), Box<dyn Error>> {
    let app = build(Some("secret"));
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/rpc")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"session.info"}"#,
        ))?;
    let response = app.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}
