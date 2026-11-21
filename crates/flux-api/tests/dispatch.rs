//! Dispatch tests: every method against `MockEngine`, plus protocol-level
//! failures surfaced as JSON-RPC errors.

use std::error::Error;
use std::sync::Arc;

use flux_api::{
    Dispatcher, Engine, FileView, MockEngine, Priority, Request, TorrentDetail, TorrentState,
    TorrentSummary, codes,
};
use serde_json::{Value, json};

fn detail() -> TorrentDetail {
    TorrentDetail {
        summary: TorrentSummary {
            id: "abc".to_owned(),
            name: "example".to_owned(),
            state: TorrentState::Downloading,
            private: false,
            progress: 0.5,
            pieces_have: 2,
            pieces_total: 4,
            total_bytes: 8192,
            peers: 0,
            download_rate: 0,
            upload_rate: 0,
            last_error: None,
        },
        files: vec![
            FileView {
                index: 0,
                path: "movie.mkv".to_owned(),
                length: 6144,
                priority: Priority::Normal,
                bytes_completed: 0,
                completed: false,
                offset: 0,
            },
            FileView {
                index: 1,
                path: "subs.srt".to_owned(),
                length: 2048,
                priority: Priority::Normal,
                bytes_completed: 0,
                completed: false,
                offset: 0,
            },
        ],
        piece_length: 16384,
        download_dir: "downloads".to_owned(),
    }
}

fn seeded() -> Result<Arc<MockEngine>, Box<dyn Error>> {
    let engine = Arc::new(MockEngine::new());
    engine.seed(detail())?;
    Ok(engine)
}

async fn error_code(
    dispatcher: &Dispatcher,
    method: &str,
    params: Option<Value>,
) -> Result<i64, Box<dyn Error>> {
    let error = dispatcher
        .dispatch(method, params)
        .await
        .err()
        .ok_or("expected an error")?;
    Ok(error.code)
}

#[tokio::test]
async fn session_and_torrent_list_and_get() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(seeded()?);

    let info = dispatcher.dispatch("session.info", None).await?;
    assert_eq!(info["name"], "oxide-flux");
    assert_eq!(info["apiVersion"], json!(1));

    let stats = dispatcher
        .dispatch("session.stats", Some(json!({})))
        .await?;
    assert_eq!(stats["torrents"], json!(1));
    assert_eq!(stats["downloading"], json!(1));
    assert_eq!(stats["active"], json!(1));

    let list = dispatcher.dispatch("torrent.list", Some(json!({}))).await?;
    let list = list.as_array().ok_or("list result should be an array")?;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], "abc");
    assert_eq!(list[0]["piecesHave"], json!(2));

    let detail = dispatcher
        .dispatch("torrent.get", Some(json!({"id": "abc"})))
        .await?;
    assert_eq!(detail["name"], "example");
    assert_eq!(detail["files"][1]["path"], "subs.srt");
    Ok(())
}

#[tokio::test]
async fn add_pause_resume_remove_round_trip() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(Arc::new(MockEngine::new()));
    let hash = "0123456789abcdef0123456789abcdef01234567";
    let added = dispatcher
        .dispatch(
            "torrent.add",
            Some(json!({"kind": "magnet", "uri": format!("magnet:?xt=urn:btih:{hash}&dn=Test")})),
        )
        .await?;
    assert_eq!(added["id"], hash);
    assert_eq!(added["name"], "Test");
    assert_eq!(added["state"], "queued");

    dispatcher
        .dispatch("torrent.pause", Some(json!({"id": hash})))
        .await?;
    let paused = dispatcher
        .dispatch("torrent.get", Some(json!({"id": hash})))
        .await?;
    assert_eq!(paused["state"], "paused");

    dispatcher
        .dispatch("torrent.resume", Some(json!({"id": hash})))
        .await?;
    assert_eq!(
        error_code(&dispatcher, "torrent.get", Some(json!({"id": "missing"}))).await?,
        codes::APP_TORRENT_NOT_FOUND
    );
    let resumed = dispatcher
        .dispatch("torrent.get", Some(json!({"id": hash})))
        .await?;
    assert_eq!(resumed["state"], "queued");

    dispatcher
        .dispatch(
            "torrent.remove",
            Some(json!({"id": hash, "deleteFiles": true})),
        )
        .await?;
    assert_eq!(
        error_code(&dispatcher, "torrent.get", Some(json!({"id": hash}))).await?,
        codes::APP_TORRENT_NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn metainfo_add_and_set_file_priorities() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(seeded()?);
    let detail = dispatcher
        .dispatch(
            "torrent.setFilePriorities",
            Some(json!({
                "id": "abc",
                "priorities": [
                    {"index": 1, "priority": "skip"},
                    {"index": 0, "priority": "high"}
                ]
            })),
        )
        .await?;
    assert_eq!(detail, Value::Null);

    let updated = dispatcher
        .dispatch("torrent.get", Some(json!({"id": "abc"})))
        .await?;
    assert_eq!(updated["files"][0]["priority"], "high");
    assert_eq!(updated["files"][1]["priority"], "skip");

    let added = dispatcher
        .dispatch(
            "torrent.add",
            Some(json!({"kind": "metainfo", "bytes": [100, 56, 58, 97, 110, 110, 111, 117, 110, 99, 101]})),
        )
        .await?;
    assert_eq!(added["state"], "queued");
    Ok(())
}

#[tokio::test]
async fn unknown_method_is_32601() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(Arc::new(MockEngine::new()));
    assert_eq!(
        error_code(&dispatcher, "torrent.nope", None).await?,
        codes::METHOD_NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn invalid_params_are_32602() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(seeded()?);
    let cases = [
        ("torrent.get", json!({})),
        ("torrent.get", json!({"id": "abc", "extra": true})),
        ("torrent.pause", json!({"id": ""})),
        ("torrent.resume", json!([1])),
        ("session.info", json!({"unexpected": 1})),
        ("torrent.add", json!({"kind": "magnet"})),
        (
            "torrent.add",
            json!({"kind": "magnet", "uri": "https://example.com/x"}),
        ),
        ("torrent.add", json!({"kind": "metainfo", "bytes": []})),
        (
            "torrent.add",
            json!({"kind": "magnet", "uri": "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567", "bogus": 1}),
        ),
        (
            "torrent.setFilePriorities",
            json!({"id": "abc", "priorities": [{"index": 0, "priority": "high"}, {"index": 0, "priority": "low"}]}),
        ),
        (
            "torrent.setFilePriorities",
            json!({"id": "abc", "priorities": [{"index": 0, "priority": "urgent"}]}),
        ),
    ];
    for (method, params) in cases {
        assert_eq!(
            error_code(&dispatcher, method, Some(params.clone())).await?,
            codes::INVALID_PARAMS,
            "method {method} params {params}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn domain_errors_use_application_codes() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(seeded()?);
    assert_eq!(
        error_code(&dispatcher, "torrent.get", Some(json!({"id": "missing"}))).await?,
        codes::APP_TORRENT_NOT_FOUND
    );
    assert_eq!(
        error_code(
            &dispatcher,
            "torrent.setFilePriorities",
            Some(json!({"id": "abc", "priorities": [{"index": 9, "priority": "low"}]}))
        )
        .await?,
        codes::APP_INVALID_FILE_INDEX
    );
    assert_eq!(
        error_code(
            &dispatcher,
            "torrent.add",
            Some(json!({"kind": "magnet", "uri": "magnet:?dn=no-hash"}))
        )
        .await?,
        codes::APP_INVALID_TORRENT
    );
    let copied = seeded()?;
    copied
        .add(flux_api::AddTorrent::Magnet {
            uri: "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        })
        .await?;
    let dispatcher = Dispatcher::new(copied);
    assert_eq!(
        error_code(
            &dispatcher,
            "torrent.add",
            Some(json!({"kind": "magnet", "uri": "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}))
        )
        .await?,
        codes::APP_DUPLICATE_TORRENT
    );
    Ok(())
}

#[tokio::test]
async fn notification_executes_without_response() -> Result<(), Box<dyn Error>> {
    let engine = seeded()?;
    let dispatcher = Dispatcher::new(engine.clone());
    let request = Request::from_slice(
        br#"{"jsonrpc":"2.0","method":"torrent.pause","params":{"id":"abc"}}"#,
    )?;
    assert!(request.is_notification());
    let response = dispatcher.handle(request).await;
    assert!(response.is_none());

    let paused = engine.get("abc").await?;
    assert_eq!(paused.summary.state, TorrentState::Paused);
    Ok(())
}

#[tokio::test]
async fn response_echoes_request_id() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(Arc::new(MockEngine::new()));
    for raw in [
        r#"{"jsonrpc":"2.0","id":"string-id","method":"session.info"}"#,
        r#"{"jsonrpc":"2.0","id":99,"method":"session.info"}"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"session.info"}"#,
    ] {
        let request = Request::from_slice(raw.as_bytes())?;
        let expected = request.id.clone();
        let response = dispatcher
            .handle(request)
            .await
            .ok_or("request should get a response")?;
        assert_eq!(Some(response.id.clone()), expected);
        assert!(!response.is_error());
    }
    Ok(())
}

#[tokio::test]
async fn streaming_controls_are_dispatched() -> Result<(), Box<dyn Error>> {
    let dispatcher = Dispatcher::new(seeded()?);

    dispatcher
        .dispatch(
            "torrent.setPlayhead",
            Some(json!({"id": "abc", "piece": 3})),
        )
        .await?;
    dispatcher
        .dispatch(
            "torrent.setStreamingMode",
            Some(json!({"id": "abc", "mode": "rarest"})),
        )
        .await?;

    assert_eq!(
        error_code(
            &dispatcher,
            "torrent.setStreamingMode",
            Some(json!({"id": "abc", "mode": "warp"})),
        )
        .await?,
        codes::INVALID_PARAMS
    );
    assert_eq!(
        error_code(
            &dispatcher,
            "torrent.setPlayhead",
            Some(json!({"id": "abc", "piece": -1})),
        )
        .await?,
        codes::INVALID_PARAMS
    );
    Ok(())
}
