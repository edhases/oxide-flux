//! WebSocket tests: authentication, upgrade, event forwarding, and ping/pong.
//! Each test binds to port 0 on loopback.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use flux_api::{Event, MockEngine, ServerConfig, TorrentState, TorrentSummary, serve};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const TIMEOUT: Duration = Duration::from_secs(5);

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
        peers: 3,
        download_rate: 1024,
        upload_rate: 512,
        last_error: None,
    }
}

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<Value, Box<dyn Error>> {
    let message = timeout(TIMEOUT, socket.next())
        .await?
        .ok_or("socket closed before a message arrived")??;
    match message {
        Message::Text(text) => Ok(serde_json::from_str(text.as_str())?),
        other => Err(format!("expected a text frame, got {other:?}").into()),
    }
}

#[tokio::test]
async fn rejects_handshake_without_token() -> Result<(), Box<dyn Error>> {
    let server = serve(
        ServerConfig::default().with_token("secret"),
        Arc::new(MockEngine::new()),
    )
    .await?;
    let url = format!("ws://{}/ws", server.local_addr());

    match connect_async(url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 401);
        }
        Err(error) => return Err(error.into()),
        Ok(_) => return Err("handshake without a token must fail".into()),
    }

    timeout(TIMEOUT, server.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn accepts_query_token_and_forwards_events() -> Result<(), Box<dyn Error>> {
    let server = serve(
        ServerConfig::default().with_token("secret"),
        Arc::new(MockEngine::new()),
    )
    .await?;
    let url = format!("ws://{}/ws?token=secret", server.local_addr());
    let (mut socket, response) = connect_async(url).await?;
    assert_eq!(response.status().as_u16(), 101);

    let expected = summary();
    assert_eq!(
        server.publish(Event::TorrentUpdated(expected.clone())),
        1,
        "the upgraded client should be subscribed"
    );
    let value = next_json(&mut socket).await?;
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["method"], "torrent.updated");
    assert!(
        value.get("id").is_none(),
        "notifications must not carry an id"
    );
    assert_eq!(value["params"]["id"], "abc");
    assert_eq!(value["params"]["state"], "downloading");
    assert_eq!(value["params"]["progress"], 0.5);

    assert_eq!(
        server.publish(Event::TorrentRemoved {
            id: "abc".to_owned()
        }),
        1
    );
    let value = next_json(&mut socket).await?;
    assert_eq!(value["method"], "torrent.removed");
    assert_eq!(value["params"], serde_json::json!({"id": "abc"}));

    assert_eq!(server.publish(Event::SessionShutdown), 1);
    let value = next_json(&mut socket).await?;
    assert_eq!(value["method"], "session.shutdown");
    assert_eq!(value["params"], serde_json::json!({}));

    socket.close(None).await?;
    timeout(TIMEOUT, server.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn answers_pings() -> Result<(), Box<dyn Error>> {
    let server = serve(
        ServerConfig::default().with_token("secret"),
        Arc::new(MockEngine::new()),
    )
    .await?;
    let url = format!("ws://{}/ws?token=secret", server.local_addr());
    let (mut socket, response) = connect_async(url).await?;
    assert_eq!(response.status().as_u16(), 101);

    socket.send(Message::Ping(b"ping".to_vec().into())).await?;
    let message = timeout(TIMEOUT, socket.next())
        .await?
        .ok_or("socket closed before pong")??;
    match message {
        Message::Pong(payload) => assert_eq!(payload.as_ref(), b"ping"),
        other => return Err(format!("expected pong, got {other:?}").into()),
    }

    socket.close(None).await?;
    timeout(TIMEOUT, server.shutdown()).await??;
    Ok(())
}
