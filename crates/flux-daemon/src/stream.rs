//! Loopback HTTP server for streaming a torrent file while it downloads.
//!
//! Media players speak HTTP: they send `GET /stream/<index>`, ask for byte
//! ranges while scrubbing, and reconnect when a connection ends. This module
//! implements just enough HTTP/1.1 for that, on top of a [`StreamSource`] the
//! session implements over its verified piece store.
//!
//! # Behaviour
//!
//! * Only `GET|HEAD /stream/<index>` exists; every other path or method is
//!   `404`. Malformed or oversized request heads are `400`.
//! * `HEAD` sends the same headers as `GET` and no body.
//! * A request without `Range` gets `200` and the whole file.
//! * A single `Range` gets `206`. Open-ended (`bytes=start-`) and suffix
//!   (`bytes=-N`) forms work, and an end past EOF is clamped. Invalid,
//!   unsatisfiable or multi-range requests get `416` with
//!   `Content-Range: bytes */<total>`.
//! * Every response sets `Accept-Ranges: bytes` and `Connection: close`.
//! * Bodies are pulled from [`StreamSource::read`] in chunks of at most
//!   [`MAX_CHUNK`] bytes, waiting up to the read budget for pieces that have
//!   not been verified yet. Nothing larger than one chunk is buffered.
//! * A chunk that misses the budget aborts the connection instead of ending
//!   the body early: the correct `Content-Length` was already promised, so the
//!   player sees a truncated response and retries. These aborts are counted in
//!   [`StreamServer::aborted_reads`].
//!
//! # Reachability
//!
//! The server has no authentication and exposes file contents, so
//! [`StreamServer::serve`] refuses any non-loopback bind address. There is no
//! opt-in; binding this server off-box is never allowed.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

/// Largest slice requested from [`StreamSource::read`] at once.
///
/// The HTTP body is streamed in these chunks; the server never allocates a
/// whole `Content-Length` buffer.
pub const MAX_CHUNK: usize = 1 << 20;

/// Read budget used by [`StreamServer::serve`]: how long one chunk may wait for
/// its pieces to be verified before the connection is aborted.
pub const DEFAULT_READ_WAIT: Duration = Duration::from_secs(30);

/// Extra time granted past `wait` before a source that ignores its own deadline
/// has its read cancelled. This only exists so a broken source cannot pin a
/// connection forever; a compliant source returns [`StreamError::NotAvailable`]
/// within `wait`.
const READ_GRACE: Duration = Duration::from_secs(1);

/// Request line plus headers cap. Larger requests are rejected with `400`.
const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// How long [`StreamServer::shutdown`] waits for the accept loop before
/// aborting it. In-flight connection tasks are aborted by the accept loop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Delay before retrying a failed `accept`, so a transient resource error does
/// not spin the accept loop.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Why a [`StreamSource`] refused a read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// The file index is not part of the torrent.
    #[error("unknown file index")]
    UnknownFile,
    /// The requested start or length lies outside the file.
    #[error("range outside the file")]
    OutOfRange,
    /// The bytes are not verified yet and did not become available in time.
    #[error("bytes not available yet")]
    NotAvailable,
    /// The source itself failed; the string is for diagnostics only.
    #[error("{0}")]
    Io(String),
}

/// Verified byte access for the streaming server.
///
/// Implementors are the only thing between HTTP clients and the disk, so the
/// contract is strict: [`read`](StreamSource::read) must fill the whole buffer
/// with bytes whose pieces have been hash-verified, and must never return bytes
/// from in-flight or failed pieces. Waiting for them is part of the job; the
/// HTTP layer cannot verify anything itself.
#[async_trait::async_trait]
pub trait StreamSource: Send + Sync {
    /// Length of the file, or `None` when the index is unknown.
    fn file_len(&self, file: usize) -> Option<u64>;

    /// A short `Content-Type` for the path/extension (e.g. `video/x-matroska`).
    fn content_type(&self, file: usize) -> &'static str;

    /// Fills `buf` with verified bytes from `start`, waiting up to `wait` for
    /// the range to become available.
    ///
    /// Returns [`StreamError::NotAvailable`] when the wait expires. A source
    /// that cannot fill the whole buffer must return an error rather than
    /// partial or unverified data. Do not return `NotAvailable` before `wait`
    /// expires: the server treats it as a failed body and closes the player's
    /// connection.
    async fn read(
        &self,
        file: usize,
        start: u64,
        buf: &mut [u8],
        wait: Duration,
    ) -> Result<(), StreamError>;
}

/// A bound loopback streaming server.
///
/// [`serve`](StreamServer::serve) starts the accept loop; dropping the value
/// aborts it and every in-flight connection, but [`shutdown`](Self::shutdown)
/// is the graceful path that waits for the accept loop to stop first.
pub struct StreamServer {
    addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    aborted_reads: Arc<AtomicU64>,
}

impl StreamServer {
    /// Binds `bind` and starts serving with [`DEFAULT_READ_WAIT`].
    ///
    /// Fails with [`io::ErrorKind::InvalidInput`] when `bind` is not a
    /// loopback address. Passing port `0` asks the OS for an ephemeral port;
    /// [`local_addr`](Self::local_addr) reports the one that was bound.
    pub async fn serve(bind: SocketAddr, source: Arc<dyn StreamSource>) -> io::Result<Self> {
        Self::serve_with_wait(bind, source, DEFAULT_READ_WAIT).await
    }

    /// Like [`serve`](Self::serve), with an explicit per-chunk wait budget.
    ///
    /// A shorter budget makes a stalled stream give up sooner; longer budgets
    /// tolerate slower piece arrivals. The loopback restriction is identical.
    pub async fn serve_with_wait(
        bind: SocketAddr,
        source: Arc<dyn StreamSource>,
        wait: Duration,
    ) -> io::Result<Self> {
        if !bind.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to bind streaming server to non-loopback address {bind}"),
            ));
        }

        let listener = TcpListener::bind(bind).await?;
        let addr = listener.local_addr()?;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let aborted_reads = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn(accept_loop(
            listener,
            source,
            wait,
            shutdown_rx,
            Arc::clone(&aborted_reads),
        ));
        Ok(Self {
            addr,
            shutdown,
            task: Some(task),
            aborted_reads,
        })
    }

    /// The address the listener actually bound.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Number of response bodies aborted because a chunk could not be produced
    /// (source error or expired wait). A player retrying after such an abort is
    /// expected; a growing count points at unverified or unreachable pieces.
    pub fn aborted_reads(&self) -> u64 {
        self.aborted_reads.load(Ordering::Relaxed)
    }

    /// Stops accepting new connections and aborts every in-flight one.
    ///
    /// Returns after the accept loop has stopped. Connections blocked in
    /// [`StreamSource::read`] are cancelled, so shutdown does not wait for the
    /// read budget.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(SHUTDOWN_GRACE, &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        // Without an explicit shutdown, do not leave the listener task behind.
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    source: Arc<dyn StreamSource>,
    wait: Duration,
    mut shutdown: watch::Receiver<bool>,
    aborted_reads: Arc<AtomicU64>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _peer)) => {
                        connections.spawn(serve_connection(
                            stream,
                            Arc::clone(&source),
                            wait,
                            Arc::clone(&aborted_reads),
                        ));
                    }
                    Err(_) => tokio::time::sleep(ACCEPT_RETRY_DELAY).await,
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    // Dropping the listener closes the port before `shutdown` returns;
    // aborting the JoinSet cancels reads that are still waiting.
    connections.shutdown().await;
}

async fn serve_connection(
    mut stream: TcpStream,
    source: Arc<dyn StreamSource>,
    wait: Duration,
    aborted_reads: Arc<AtomicU64>,
) {
    let request = match read_request(&mut stream).await {
        Ok(request) => request,
        Err(RequestError::Malformed | RequestError::TooLarge) => {
            write_status(&mut stream, "400 Bad Request").await;
            return;
        }
        Err(RequestError::Io) => return,
    };

    if matches!(request.method, Method::Other) {
        write_status(&mut stream, "404 Not Found").await;
        return;
    }
    let Some(file) = request.file else {
        write_status(&mut stream, "404 Not Found").await;
        return;
    };
    let Some(total) = source.file_len(file) else {
        write_status(&mut stream, "404 Not Found").await;
        return;
    };

    let planned = match request.range.as_deref() {
        None => Plan::Whole,
        Some(value) => match parse_range(value, total) {
            Some((start, end)) => Plan::Range { start, end },
            None => Plan::Unsatisfiable,
        },
    };

    let (status, start, len, content_range) = match planned {
        Plan::Whole => ("200 OK", 0, total, None),
        Plan::Range { start, end } => (
            "206 Partial Content",
            start,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{total}")),
        ),
        Plan::Unsatisfiable => {
            let head = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\n\
                 Content-Type: {}\r\n\
                 Accept-Ranges: bytes\r\n\
                 Content-Range: bytes */{total}\r\n\
                 Content-Length: 0\r\n\
                 Connection: close\r\n\
                 \r\n",
                source.content_type(file)
            );
            let _ = stream.write_all(head.as_bytes()).await;
            return;
        }
    };

    let mut head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {}\r\n\
         Accept-Ranges: bytes\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n",
        source.content_type(file)
    );
    if let Some(range) = content_range.as_deref() {
        head.push_str("Content-Range: ");
        head.push_str(range);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }

    if matches!(request.method, Method::Head) || len == 0 {
        return;
    }

    // One chunk of scratch memory, sized to the smaller of the body and the
    // chunk cap, never to `Content-Length` of a large file.
    let capacity = usize::try_from(len.min(MAX_CHUNK as u64)).unwrap_or(MAX_CHUNK);
    let mut chunk = vec![0_u8; capacity.max(1)];
    let budget = wait.saturating_add(READ_GRACE);
    let mut offset = start;
    let mut remaining = len;

    while remaining > 0 {
        let want = usize::try_from(remaining.min(MAX_CHUNK as u64)).unwrap_or(MAX_CHUNK);
        let read =
            tokio::time::timeout(budget, source.read(file, offset, &mut chunk[..want], wait)).await;
        if !matches!(read, Ok(Ok(()))) {
            // The promised Content-Length can no longer be honoured: abort the
            // connection so the player sees a truncated body and retries.
            aborted_reads.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if stream.write_all(&chunk[..want]).await.is_err() {
            return;
        }
        offset += want as u64;
        remaining -= want as u64;
    }
}

#[derive(Debug, Clone, Copy)]
enum Method {
    Get,
    Head,
    Other,
}

enum Plan {
    Whole,
    Range { start: u64, end: u64 },
    Unsatisfiable,
}

struct Request {
    method: Method,
    file: Option<usize>,
    range: Option<String>,
}

enum RequestError {
    Io,
    Malformed,
    TooLarge,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, RequestError> {
    let mut buffer = Vec::with_capacity(512);
    let mut scratch = [0_u8; 512];
    loop {
        if let Some(end) = header_end(&buffer) {
            let text = std::str::from_utf8(&buffer[..end]).map_err(|_| RequestError::Malformed)?;
            return parse_request(text);
        }
        if buffer.len() >= MAX_REQUEST_HEAD {
            return Err(RequestError::TooLarge);
        }
        let room = MAX_REQUEST_HEAD - buffer.len();
        let want = room.min(scratch.len());
        let read = stream
            .read(&mut scratch[..want])
            .await
            .map_err(|_| RequestError::Io)?;
        if read == 0 {
            return Err(RequestError::Malformed);
        }
        buffer.extend_from_slice(&scratch[..read]);
    }
}

/// Position just past the blank line ending the request head.
fn header_end(buffer: &[u8]) -> Option<usize> {
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4);
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|at| at + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(end), None) => Some(end),
        (None, Some(end)) => Some(end),
        (None, None) => None,
    }
}

fn parse_request(text: &str) -> Result<Request, RequestError> {
    let mut lines = text.lines();
    let request_line = lines.next().ok_or(RequestError::Malformed)?;
    let mut parts = request_line.split_whitespace();
    let method = match parts.next() {
        Some("GET") => Method::Get,
        Some("HEAD") => Method::Head,
        Some(_) => Method::Other,
        None => return Err(RequestError::Malformed),
    };
    let target = parts.next().ok_or(RequestError::Malformed)?;
    let version = parts.next().ok_or(RequestError::Malformed)?;
    if !version.starts_with("HTTP/") || parts.next().is_some() {
        return Err(RequestError::Malformed);
    }

    let mut range = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(RequestError::Malformed);
        };
        if range.is_none() && name.eq_ignore_ascii_case("range") {
            range = Some(value.trim().to_owned());
        }
    }

    let file = target
        .strip_prefix("/stream/")
        .and_then(|rest| rest.parse::<usize>().ok());
    Ok(Request {
        method,
        file,
        range,
    })
}

/// Parses a single byte range into inclusive endpoints, or `None` when the
/// header is invalid, multi-range or unsatisfiable for `total`.
fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let (unit, spec) = value.split_once('=')?;
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return None;
    }
    let spec = spec.trim();
    if spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let first = first.trim();
    let last = last.trim();

    if first.is_empty() {
        // Suffix form: the last N bytes. `-0` selects nothing, so it is not a
        // satisfiable range.
        let suffix: u64 = last.parse().ok()?;
        if suffix == 0 || total == 0 {
            return None;
        }
        return Some((total.saturating_sub(suffix), total - 1));
    }

    let start: u64 = first.parse().ok()?;
    if total == 0 || start >= total {
        return None;
    }
    let end = if last.is_empty() {
        total - 1
    } else {
        let end: u64 = last.parse().ok()?;
        if end < start {
            return None;
        }
        end.min(total - 1)
    };
    Some((start, end))
}

async fn write_status(stream: &mut TcpStream, status: &str) {
    let head = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
}
