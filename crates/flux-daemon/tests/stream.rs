//! Loopback HTTP streaming tests.
//!
//! The module under test is included directly from `src/stream.rs` so this
//! suite compiles before `lib.rs` declares `pub mod stream;`. Once the module is
//! wired into the crate, `use flux_daemon::stream::...` can replace the `mod`
//! line; the tests themselves are unchanged.
//!
//! Everything is in-memory and offline: the mock source exposes a byte prefix
//! and can withhold the rest until the test releases it.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

#[path = "../src/stream.rs"]
mod stream;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use stream::{MAX_CHUNK, StreamError, StreamServer, StreamSource};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Mock source
// ---------------------------------------------------------------------------

struct MockSource {
    data: Vec<u8>,
    content_type: &'static str,
    /// Bytes from the start of the file that may be read.
    available: AtomicU64,
    notify: Notify,
    /// Largest buffer the server ever handed to `read`.
    max_read: AtomicUsize,
}

impl MockSource {
    fn new(data: Vec<u8>) -> Self {
        Self {
            available: AtomicU64::new(data.len() as u64),
            data,
            content_type: "video/x-matroska",
            notify: Notify::new(),
            max_read: AtomicUsize::new(0),
        }
    }

    /// Makes only the first `available` bytes readable.
    fn withhold(&self, available: u64) {
        self.available.store(available, Ordering::SeqCst);
    }

    /// Makes the first `available` bytes readable and wakes waiting readers.
    fn release(&self, available: u64) {
        self.available.store(available, Ordering::SeqCst);
        self.notify.notify_one();
    }

    fn max_read(&self) -> usize {
        self.max_read.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl StreamSource for MockSource {
    fn file_len(&self, file: usize) -> Option<u64> {
        (file == 0).then_some(self.data.len() as u64)
    }

    fn content_type(&self, file: usize) -> &'static str {
        if file == 0 {
            self.content_type
        } else {
            "application/octet-stream"
        }
    }

    async fn read(
        &self,
        file: usize,
        start: u64,
        buf: &mut [u8],
        wait: Duration,
    ) -> Result<(), StreamError> {
        if file != 0 {
            return Err(StreamError::UnknownFile);
        }
        let end = start
            .checked_add(buf.len() as u64)
            .ok_or(StreamError::OutOfRange)?;
        if end > self.data.len() as u64 {
            return Err(StreamError::OutOfRange);
        }
        self.max_read.fetch_max(buf.len(), Ordering::SeqCst);

        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if self.available.load(Ordering::SeqCst) >= end {
                let from = usize::try_from(start).expect("mock offset fits usize");
                let to = usize::try_from(end).expect("mock end fits usize");
                buf.copy_from_slice(&self.data[from..to]);
                return Ok(());
            }
            let notified = self.notify.notified();
            // Recheck after creating the waiter so a release between the first
            // check and the await is not lost.
            if self.available.load(Ordering::SeqCst) >= end {
                continue;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(StreamError::NotAvailable);
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                return Err(StreamError::NotAvailable);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Raw HTTP helpers
// ---------------------------------------------------------------------------

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn parse_response(raw: &[u8]) -> Response {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response head terminator");
    let head = std::str::from_utf8(&raw[..split]).expect("utf-8 response head");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status");
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').expect("header line");
            (name.trim().to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    Response {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

async fn send_raw(addr: SocketAddr, request: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut response = Vec::new();
    timeout(IO_TIMEOUT, stream.read_to_end(&mut response))
        .await
        .expect("response timed out")
        .expect("read response");
    response
}

async fn get(addr: SocketAddr, target: &str, headers: &str) -> Response {
    let request = format!("GET {target} HTTP/1.1\r\nHost: test\r\n{headers}\r\n");
    parse_response(&send_raw(addr, &request).await)
}

async fn head(addr: SocketAddr, target: &str, headers: &str) -> Response {
    let request = format!("HEAD {target} HTTP/1.1\r\nHost: test\r\n{headers}\r\n");
    parse_response(&send_raw(addr, &request).await)
}

async fn start(source: Arc<MockSource>) -> StreamServer {
    StreamServer::serve(SocketAddr::from(([127, 0, 0, 1], 0)), source)
        .await
        .expect("bind streaming server")
}

async fn start_with_wait(source: Arc<MockSource>, wait: Duration) -> StreamServer {
    StreamServer::serve_with_wait(SocketAddr::from(([127, 0, 0, 1], 0)), source, wait)
        .await
        .expect("bind streaming server")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_file_streams_in_bounded_chunks() {
    let length = 2 * MAX_CHUNK + 37;
    let data: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
    let source = Arc::new(MockSource::new(data.clone()));
    let server = start(Arc::clone(&source)).await;

    let response = get(server.local_addr(), "/stream/0", "").await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-length"),
        Some(length.to_string().as_str())
    );
    assert_eq!(response.header("content-type"), Some("video/x-matroska"));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert_eq!(response.header("connection"), Some("close"));
    assert_eq!(response.header("content-range"), None);
    assert_eq!(response.body, data);
    assert_eq!(source.max_read(), MAX_CHUNK);

    server.shutdown().await;
}

#[tokio::test]
async fn ranges_are_exact_and_clamped() {
    let data = b"0123456789abcdefghij".to_vec();
    let source = Arc::new(MockSource::new(data.clone()));
    let server = start(source).await;
    let addr = server.local_addr();

    let response = get(addr, "/stream/0", "Range: bytes=10-19\r\n").await;
    assert_eq!(response.status, 206);
    assert_eq!(response.body, b"abcdefghij");
    assert_eq!(response.header("content-length"), Some("10"));
    assert_eq!(response.header("content-range"), Some("bytes 10-19/20"));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert_eq!(response.header("content-type"), Some("video/x-matroska"));

    let response = get(addr, "/stream/0", "Range: bytes=15-\r\n").await;
    assert_eq!(response.status, 206);
    assert_eq!(response.body, b"fghij");
    assert_eq!(response.header("content-length"), Some("5"));
    assert_eq!(response.header("content-range"), Some("bytes 15-19/20"));

    let response = get(addr, "/stream/0", "Range: bytes=-5\r\n").await;
    assert_eq!(response.status, 206);
    assert_eq!(response.body, b"fghij");
    assert_eq!(response.header("content-range"), Some("bytes 15-19/20"));

    let response = get(addr, "/stream/0", "Range: bytes=-50\r\n").await;
    assert_eq!(response.status, 206);
    assert_eq!(response.body, data);
    assert_eq!(response.header("content-range"), Some("bytes 0-19/20"));

    let response = get(addr, "/stream/0", "Range: bytes=15-999\r\n").await;
    assert_eq!(response.status, 206);
    assert_eq!(response.body, b"fghij");
    assert_eq!(response.header("content-range"), Some("bytes 15-19/20"));

    server.shutdown().await;
}

#[tokio::test]
async fn head_has_the_same_headers_and_no_body() {
    let data = b"0123456789abcdefghij".to_vec();
    let source = Arc::new(MockSource::new(data));
    let server = start(source).await;
    let addr = server.local_addr();

    let response = head(addr, "/stream/0", "").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-length"), Some("20"));
    assert_eq!(response.header("content-type"), Some("video/x-matroska"));
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert_eq!(response.header("connection"), Some("close"));
    assert!(response.body.is_empty());

    let response = head(addr, "/stream/0", "Range: bytes=5-9\r\n").await;
    assert_eq!(response.status, 206);
    assert_eq!(response.header("content-length"), Some("5"));
    assert_eq!(response.header("content-range"), Some("bytes 5-9/20"));
    assert!(response.body.is_empty());

    server.shutdown().await;
}

#[tokio::test]
async fn unknown_files_and_other_routes_are_404() {
    let source = Arc::new(MockSource::new(b"0123456789".to_vec()));
    let server = start(source).await;
    let addr = server.local_addr();

    for target in ["/stream/1", "/stream/abc", "/stream/", "/nope"] {
        let response = get(addr, target, "").await;
        assert_eq!(response.status, 404, "{target}");
        assert_eq!(response.header("content-length"), Some("0"), "{target}");
        assert!(response.body.is_empty(), "{target}");
    }

    let raw = send_raw(
        addr,
        "POST /stream/0 HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    assert_eq!(parse_response(&raw).status, 404);

    server.shutdown().await;
}

#[tokio::test]
async fn invalid_and_unsatisfiable_ranges_are_416() {
    let source = Arc::new(MockSource::new(b"0123456789abcdefghij".to_vec()));
    let server = start(source).await;
    let addr = server.local_addr();

    for range in [
        "bytes=999999-",
        "bytes=20-",
        "bytes=0-1,3-4",
        "bytes=5-3",
        "bytes=abc-",
        "bytes=-0",
        "bytes=-",
        "items=0-1",
    ] {
        let response = get(addr, "/stream/0", &format!("Range: {range}\r\n")).await;
        assert_eq!(response.status, 416, "{range}");
        assert_eq!(
            response.header("content-range"),
            Some("bytes */20"),
            "{range}"
        );
        assert_eq!(response.header("content-length"), Some("0"), "{range}");
        assert!(response.body.is_empty(), "{range}");
    }

    server.shutdown().await;
}

#[tokio::test]
async fn zero_length_file_has_no_body_and_no_satisfiable_range() {
    let source = Arc::new(MockSource::new(Vec::new()));
    let server = start(source).await;
    let addr = server.local_addr();

    let response = get(addr, "/stream/0", "").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-length"), Some("0"));
    assert!(response.body.is_empty());

    let response = get(addr, "/stream/0", "Range: bytes=0-\r\n").await;
    assert_eq!(response.status, 416);
    assert_eq!(response.header("content-range"), Some("bytes */0"));

    server.shutdown().await;
}

#[tokio::test]
async fn waits_for_bytes_that_become_available() {
    let length = 2 * MAX_CHUNK + 11;
    let data: Vec<u8> = (0..length).map(|index| (index % 253) as u8).collect();
    let source = Arc::new(MockSource::new(data.clone()));
    // The first chunk is readable; the second one must wait.
    source.withhold(MAX_CHUNK as u64);
    let server = start_with_wait(Arc::clone(&source), Duration::from_secs(5)).await;
    let addr = server.local_addr();

    let client = tokio::spawn(async move { get(addr, "/stream/0", "").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    source.release(length as u64);

    let response = timeout(Duration::from_secs(5), client)
        .await
        .expect("client finished")
        .expect("client task");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, data);
    assert_eq!(source.max_read(), MAX_CHUNK);

    server.shutdown().await;
}

#[tokio::test]
async fn permanently_unavailable_bytes_abort_within_the_budget() {
    let source = Arc::new(MockSource::new(vec![7_u8; 64]));
    source.withhold(8);
    let server = start_with_wait(Arc::clone(&source), Duration::from_millis(200)).await;

    let started = tokio::time::Instant::now();
    let response = get(server.local_addr(), "/stream/0", "").await;
    let elapsed = started.elapsed();

    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-length"), Some("64"));
    assert!(
        response.body.len() < 64,
        "server must not send a short but complete body"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "aborted after {elapsed:?}"
    );
    assert_eq!(server.aborted_reads(), 1);

    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_stops_accepting_and_aborts_in_flight_connections() {
    let source = Arc::new(MockSource::new(vec![7_u8; 64]));
    source.withhold(0);
    let server = start_with_wait(Arc::clone(&source), Duration::from_secs(30)).await;
    let addr = server.local_addr();

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(b"GET /stream/0 HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .expect("write request");
        ready_tx.send(()).expect("signal ready");
        let mut response = Vec::new();
        // The server aborts the connection, which may surface as a reset.
        let _ = stream.read_to_end(&mut response).await;
        response
    });
    ready_rx.await.expect("client started");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = tokio::time::Instant::now();
    timeout(Duration::from_secs(2), server.shutdown())
        .await
        .expect("shutdown finished");
    assert!(started.elapsed() < Duration::from_secs(2));

    let raw = timeout(Duration::from_secs(2), client)
        .await
        .expect("client finished")
        .expect("client task");
    assert!(!raw.is_empty(), "headers should precede the abort");
    let response = parse_response(&raw);
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-length"), Some("64"));
    assert!(response.body.len() < 64);

    let connect = timeout(Duration::from_secs(1), TcpStream::connect(addr)).await;
    assert!(
        !matches!(connect, Ok(Ok(_))),
        "listener still accepting after shutdown"
    );
}

#[tokio::test]
async fn non_loopback_binds_are_refused() {
    let source = Arc::new(MockSource::new(vec![0_u8; 4]));
    let result = StreamServer::serve(SocketAddr::from(([0, 0, 0, 0], 0)), source).await;
    assert!(result.is_err());
}
