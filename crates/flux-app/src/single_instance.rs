//! Single-instance coordination without a network socket.
//!
//! The first process locks `instance.lock`; every later process writes a small
//! request file and exits. The primary drains that directory on a timer, so
//! `magnet:` links and `.torrent` paths open in the window that is already open.
//! Files are written to a temporary name and renamed, so the reader never sees a
//! partial request.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// A thing the UI should open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenRequest {
    /// A magnet URI exactly as received.
    Magnet(String),
    /// A `.torrent` file on disk.
    Torrent(PathBuf),
}

/// The directory later launches write into.
#[derive(Debug, Clone)]
pub struct RequestQueue {
    root: PathBuf,
}

impl RequestQueue {
    /// Queue rooted at `root` (usually `<state>/requests`).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory this queue reads and writes.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes one request atomically.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O or serialization error.
    pub fn enqueue(&self, request: &OpenRequest) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!("{stamp:013}-{:05}-{sequence:04}.json", std::process::id());
        let temporary = self.root.join(format!("{name}.tmp"));
        let target = self.root.join(name);
        let payload = serde_json::to_vec(request).map_err(io::Error::other)?;
        {
            let mut file = File::create(&temporary)?;
            file.write_all(&payload)?;
            file.sync_all()?;
        }
        fs::rename(temporary, target)
    }

    /// Reads and removes every queued request, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error only when the directory itself cannot be read; a single
    /// corrupt file is removed and skipped.
    pub fn drain(&self) -> io::Result<Vec<OpenRequest>> {
        let mut requests = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(requests);
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        for path in paths {
            let parsed = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            if let Some(request) = parsed {
                requests.push(request);
            }
            let _ = fs::remove_file(path);
        }
        Ok(requests)
    }
}

/// The primary process's claim on the state directory.
#[derive(Debug)]
pub struct Instance {
    queue: RequestQueue,
    _lock: File,
}

impl Instance {
    /// The request queue the UI polls.
    pub fn queue(&self) -> &RequestQueue {
        &self.queue
    }
}

/// Tries to become the primary instance.
///
/// Returns `Ok(None)` when another process holds the lock; that process is
/// responsible for draining the queue.
///
/// # Errors
///
/// Returns an error when the state directory or lock file cannot be used.
pub fn claim(root: &Path) -> io::Result<Option<Instance>> {
    fs::create_dir_all(root)?;
    let lock_path = root.join("instance.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(Instance {
            queue: RequestQueue::new(root.join("requests")),
            _lock: file,
        })),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn requests_round_trip_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let queue = RequestQueue::new(dir.path().join("requests"));
        queue
            .enqueue(&OpenRequest::Magnet(
                "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".to_owned(),
            ))
            .unwrap();
        queue
            .enqueue(&OpenRequest::Torrent(PathBuf::from("C:/x/a.torrent")))
            .unwrap();
        let drained = queue.drain().unwrap();
        assert_eq!(drained.len(), 2);
        assert!(matches!(drained[0], OpenRequest::Magnet(_)));
        assert!(queue.drain().unwrap().is_empty());
    }

    #[test]
    fn corrupt_requests_are_dropped_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("requests");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("0001-bad.json"), b"{not json").unwrap();
        let queue = RequestQueue::new(&root);
        assert!(queue.drain().unwrap().is_empty());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn a_queued_request_is_drained_by_the_primary() {
        let dir = tempfile::tempdir().unwrap();
        let instance = claim(dir.path()).unwrap().expect("first claim wins");
        let queue = instance.queue().clone();
        queue
            .enqueue(&OpenRequest::Torrent(PathBuf::from("C:/x/a.torrent")))
            .unwrap();
        assert_eq!(queue.drain().unwrap().len(), 1);
    }
}
