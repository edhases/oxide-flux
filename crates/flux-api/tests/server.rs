//! Server startup guard: unsafe binds are refused before a socket is opened.

use std::error::Error;
use std::sync::Arc;

use flux_api::{ConfigError, MockEngine, ServerConfig, ServerError, serve};

#[tokio::test]
async fn refuses_non_loopback_without_token() -> Result<(), Box<dyn Error>> {
    let config = ServerConfig {
        bind: "0.0.0.0:0".parse()?,
        ..ServerConfig::default()
    };

    match serve(config, Arc::new(MockEngine::new())).await {
        Err(ServerError::Config(ConfigError::NonLoopbackWithoutToken { .. })) => Ok(()),
        Err(other) => Err(format!("unexpected startup error: {other}").into()),
        Ok(server) => {
            let _ = server.shutdown().await;
            Err("a non-loopback bind without a token must be refused".into())
        }
    }
}

#[tokio::test]
async fn loopback_bind_succeeds_and_shuts_down() -> Result<(), Box<dyn Error>> {
    let server = serve(
        ServerConfig::default().with_token("secret"),
        Arc::new(MockEngine::new()),
    )
    .await?;
    assert!(server.local_addr().ip().is_loopback());
    server.shutdown().await?;
    Ok(())
}
