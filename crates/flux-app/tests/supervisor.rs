//! Supervisor lifecycle: startup, event snapshots and graceful shutdown.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use flux_app::supervisor::{self, Command, Event};
use flux_daemon::Config;
use tokio::time::timeout;

fn config(root: &std::path::Path) -> Config {
    Config {
        download_dir: root.to_path_buf(),
        listen_port: 0,
        persist: false,
        enable_dht: false,
        ..Config::default()
    }
}

#[tokio::test]
async fn shutdown_reports_closed_after_the_session_stops() {
    let dir = tempfile::tempdir().unwrap();
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(supervisor::run(config(dir.path()), command_rx, events));

    match timeout(Duration::from_secs(10), event_rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Event::Ready { .. } => {}
        other => panic!("expected Ready, got {other:?}"),
    }

    commands.send(Command::Shutdown).unwrap();

    // The supervisor must finish `session.shutdown()` (flush, fsync, tracker
    // stop) and only then report Closed.
    loop {
        let event = timeout(Duration::from_secs(10), event_rx.recv())
            .await
            .expect("shutdown must not hang")
            .expect("event channel closed");
        if matches!(event, Event::Closed) {
            break;
        }
    }
    assert!(event_rx.recv().await.is_none(), "the supervisor must end");
}
