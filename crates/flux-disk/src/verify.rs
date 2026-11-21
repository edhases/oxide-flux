//! Piece verification on a dedicated hashing pool.
//!
//! Hashing a 16 MiB piece is a CPU-bound operation on top of a full read-back; it
//! never touches the tokio runtime. A configurable pool of blocking threads consumes
//! a bounded job queue and reports results on a bounded event channel, so the torrent
//! actor can keep peer state moving while pieces are checked.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use sha1::{Digest, Sha1};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use crate::backend::{BlockBackend, FileSet};
use crate::layout::FileLayout;
use crate::{DiskError, SubmitError};

/// A v1 piece hash (SHA-1). v2/merkle verification needs its own pipeline.
pub type PieceHash = [u8; 20];

/// Result of checking one piece.
#[derive(Debug)]
pub enum PieceOutcome {
    /// The bytes on disk match the expected SHA-1.
    Good,
    /// The bytes on disk do not match. The torrent actor should re-request the piece.
    Corrupt {
        /// Hash from the metainfo.
        expected: PieceHash,
        /// Hash of what is actually stored.
        actual: PieceHash,
    },
    /// Verification was cancelled before the piece was hashed (shutdown).
    Cancelled,
    /// The piece could not be read or addressed.
    Failed(DiskError),
}

/// One verification result.
#[derive(Debug)]
pub struct VerifyEvent {
    /// Piece index.
    pub piece: u32,
    /// What the hash check concluded.
    pub outcome: PieceOutcome,
}

/// Tunables for [`Verifier::start`].
#[derive(Debug, Clone)]
pub struct VerifyConfig {
    /// Hashing threads. Defaults to the available cores, capped at 8 — enough to keep
    /// a 500 Mbit/s stream fed without starving the rest of the process.
    ///
    /// Each thread owns one piece-sized read buffer for its whole lifetime, so
    /// `threads * piece_length` is the pool's memory floor.
    pub threads: usize,
    /// Jobs that may be queued before producers block or `try_verify` reports
    /// [`SubmitError::Full`].
    pub queue_capacity: usize,
    /// Capacity of the result channel.
    pub event_capacity: usize,
}

impl VerifyConfig {
    /// Cores capped at 8, 32 queued jobs, 256 result slots.
    pub fn new() -> Self {
        let cores = thread::available_parallelism().map_or(1, |count| count.get());
        Self {
            threads: cores.clamp(1, 8),
            queue_capacity: 32,
            event_capacity: 256,
        }
    }
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle to the hashing pool.
pub struct Verifier {
    tx: mpsc::Sender<u32>,
    /// Kept alive so the result channel closes only after the workers are done.
    _events: mpsc::Sender<VerifyEvent>,
    threads: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Verifier {
    /// Opens the torrent's files and spawns the pool.
    ///
    /// `hashes` must cover exactly the pieces computed from the layout: the verifier
    /// derives piece offsets and lengths from [`FileLayout`] and never trusts a
    /// declared length, so a short final piece needs no special case.
    pub fn start(
        config: VerifyConfig,
        layout: Arc<FileLayout>,
        backend: Arc<dyn BlockBackend>,
        root: std::path::PathBuf,
        piece_length: u32,
        hashes: Vec<PieceHash>,
    ) -> Result<(Self, mpsc::Receiver<VerifyEvent>), DiskError> {
        if config.threads == 0 {
            return Err(DiskError::InvalidConfig { name: "threads" });
        }
        if config.queue_capacity == 0 {
            return Err(DiskError::InvalidConfig {
                name: "queue_capacity",
            });
        }
        if config.event_capacity == 0 {
            return Err(DiskError::InvalidConfig {
                name: "event_capacity",
            });
        }
        if piece_length == 0 {
            return Err(DiskError::ZeroPieceLength);
        }

        let piece_count = layout.piece_count(piece_length)?;
        if piece_count != hashes.len() as u64 {
            return Err(DiskError::HashCountMismatch {
                expected: usize::try_from(piece_count).unwrap_or(usize::MAX),
                actual: hashes.len(),
            });
        }

        let files = Arc::new(FileSet::open(layout, backend, &root)?);
        let context = Arc::new(HashContext {
            files,
            piece_length,
            hashes: hashes.into(),
        });

        let (tx, rx) = mpsc::channel(config.queue_capacity);
        let (event_tx, event_rx) = mpsc::channel(config.event_capacity);
        let shared = Arc::new(Mutex::new(rx));
        let stop = Arc::new(AtomicBool::new(false));

        let mut threads = Vec::with_capacity(config.threads);
        for index in 0..config.threads {
            let worker_shared = Arc::clone(&shared);
            let worker_context = Arc::clone(&context);
            let worker_events = event_tx.clone();
            let worker_stop = Arc::clone(&stop);
            let spawned = thread::Builder::new()
                .name(format!("flux-disk-verify-{index}"))
                .spawn(move || {
                    run_worker(
                        &worker_shared,
                        &worker_context,
                        &worker_stop,
                        &worker_events,
                    );
                });
            match spawned {
                Ok(handle) => threads.push(handle),
                Err(err) => {
                    // Never leave half a pool behind: close the queue and wait for the
                    // workers that already started.
                    stop.store(true, Ordering::SeqCst);
                    drop(tx);
                    for handle in threads {
                        let _ = handle.join();
                    }
                    return Err(DiskError::spawn("verifier", err));
                }
            }
        }

        Ok((
            Self {
                tx,
                _events: event_tx,
                threads,
                stop,
            },
            event_rx,
        ))
    }

    /// Queues a piece, waiting for capacity.
    pub async fn verify(&self, piece: u32) -> Result<(), DiskError> {
        self.tx.send(piece).await.map_err(|_| DiskError::Closed)
    }

    /// Queues a piece without waiting.
    pub fn try_verify(&self, piece: u32) -> Result<(), SubmitError> {
        match self.tx.try_reserve() {
            Ok(permit) => {
                permit.send(piece);
                Ok(())
            }
            Err(TrySendError::Full(())) => Err(SubmitError::Full),
            Err(TrySendError::Closed(())) => Err(SubmitError::Closed),
        }
    }

    /// Free job slots right now.
    pub fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    /// Stops accepting work, lets in-flight pieces finish and joins the pool.
    ///
    /// After the stop flag is set, queued jobs are drained and reported as
    /// [`PieceOutcome::Cancelled`] (best effort) instead of being silently dropped, so
    /// shutdown is fast and nothing is forgotten. Keep receiving from the event channel
    /// while this future is pending: the workers deliver their last results before
    /// they exit.
    pub async fn shutdown(self) -> Result<(), DiskError> {
        self.stop.store(true, Ordering::SeqCst);
        drop(self.tx);

        let Self { threads, .. } = self;

        let (done_tx, done) = oneshot::channel();
        thread::Builder::new()
            .name("flux-disk-verify-join".to_owned())
            .spawn(move || {
                for handle in threads {
                    let _ = handle.join();
                }
                let _ = done_tx.send(());
            })
            .map_err(|err| DiskError::spawn("verifier join", err))?;

        done.await.map_err(|_| DiskError::WorkerPanicked)
    }
}

struct HashContext {
    files: Arc<FileSet>,
    piece_length: u32,
    hashes: Arc<[PieceHash]>,
}

fn run_worker(
    shared: &Mutex<mpsc::Receiver<u32>>,
    context: &HashContext,
    stop: &AtomicBool,
    events: &mpsc::Sender<VerifyEvent>,
) {
    let mut buffer = vec![0_u8; context.piece_length as usize];
    loop {
        let received = match shared.lock() {
            Ok(mut queue) => queue.blocking_recv(),
            // A poisoned queue means another worker panicked; stop quietly instead of
            // panicking on top of it.
            Err(_) => return,
        };
        let Some(piece) = received else {
            return;
        };

        let outcome = if stop.load(Ordering::Relaxed) {
            PieceOutcome::Cancelled
        } else {
            match context.hashes.get(piece as usize).copied() {
                Some(expected) => verify_piece(
                    &context.files,
                    context.piece_length,
                    piece,
                    expected,
                    &mut buffer,
                ),
                // Unreachable: the pool validates the hash count against the layout,
                // but a bad index is reported instead of panicking.
                None => PieceOutcome::Failed(DiskError::PieceOutOfRange {
                    piece,
                    pieces: context.hashes.len() as u64,
                    piece_length: context.piece_length,
                }),
            }
        };

        let event = VerifyEvent { piece, outcome };
        if stop.load(Ordering::Relaxed) {
            // Cancelling: never block on a consumer that may already be gone.
            let _ = events.try_send(event);
        } else {
            let _ = events.blocking_send(event);
        }
    }
}

/// Reads a whole piece back into `buf` and compares it with `expected`.
///
/// Piece offset and length come from the layout, never from a wire-reported length.
/// `buf` must be at least `piece_length` bytes long; a shorter buffer is reported as
/// [`DiskError::BufferTooSmall`] instead of panicking.
pub fn verify_piece(
    files: &FileSet,
    piece_length: u32,
    piece: u32,
    expected: PieceHash,
    buf: &mut [u8],
) -> PieceOutcome {
    let (offset, len) = match files.layout().piece_range(piece_length, piece) {
        Ok(range) => range,
        Err(err) => return PieceOutcome::Failed(err),
    };
    let len = len as usize;
    let Some(target) = buf.get_mut(..len) else {
        return PieceOutcome::Failed(DiskError::BufferTooSmall {
            needed: len as u64,
            available: buf.len() as u64,
        });
    };
    if let Err(err) = files.read_exact_at(offset, target) {
        return PieceOutcome::Failed(err);
    }

    let mut hasher = Sha1::new();
    hasher.update(&target[..]);
    let digest = hasher.finalize();
    let mut actual = [0_u8; 20];
    actual.copy_from_slice(&digest);

    if actual == expected {
        PieceOutcome::Good
    } else {
        PieceOutcome::Corrupt { expected, actual }
    }
}
