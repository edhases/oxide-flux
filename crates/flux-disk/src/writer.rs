//! The coalescing piece writer.
//!
//! Peers hand over 16 KiB blocks in whatever order they arrive. Writing each block
//! with its own syscall is the classic way to turn a fast swarm into a slow disk, so
//! blocks are accumulated per piece in RAM and a **complete** piece is written as one
//! contiguous range (usually one syscall; two when the piece straddles a file edge).
//!
//! The cache is byte-bounded and lives next to a bounded command channel: a peer that
//! outruns the disk is slowed down by backpressure instead of growing the queue.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use crate::DiskError;
use crate::backend::{Allocation, BlockBackend, FileSet};
use crate::layout::FileLayout;

/// One block of a piece, addressed inside the piece.
#[derive(Debug, Clone)]
pub struct Block {
    /// Piece index in the torrent.
    pub piece: u32,
    /// Byte offset of `data` inside the piece.
    pub begin: u32,
    /// Payload. Length must fit inside the piece.
    pub data: Vec<u8>,
}

impl Block {
    /// Builds a block. Bounds are validated by the writer, not here.
    pub fn new(piece: u32, begin: u32, data: Vec<u8>) -> Self {
        Self { piece, begin, data }
    }
}

/// Tunables for [`DiskWriter::start`].
#[derive(Debug, Clone)]
pub struct WriterConfig {
    /// Piece length. A piece is the unit of coalesced writing.
    pub piece_length: u32,
    /// Commands that may be queued before producers block or [`DiskWriter::try_write_block`]
    /// reports [`SubmitError::Full`].
    pub queue_capacity: usize,
    /// Hard byte budget of the in-RAM cache. Never exceeded after a block is accepted.
    pub cache_bytes: usize,
    /// Capacity of the completion-event channel.
    pub event_capacity: usize,
    /// Flush an incomplete piece once its oldest buffered block is this old. `None`
    /// disables the timer (tests use it to stay deterministic); the byte budget still
    /// bounds the cache.
    pub flush_interval: Option<Duration>,
    /// How files are prepared when the writer opens them.
    pub allocation: Allocation,
}

impl WriterConfig {
    /// Sensible defaults for a streaming-first client: 32 MiB cache, 64 queued
    /// commands, 500 ms timeout flush, sparse preallocation.
    pub fn new(piece_length: u32) -> Self {
        Self {
            piece_length,
            queue_capacity: 64,
            cache_bytes: 32 * 1024 * 1024,
            event_capacity: 256,
            flush_interval: Some(Duration::from_millis(500)),
            allocation: Allocation::Sparse,
        }
    }
}

/// Why a block could not be queued.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    /// The writer is at capacity. The swarm must slow down; retry later.
    #[error("writer queue is full")]
    Full,
    /// The writer has shut down.
    #[error("writer has shut down")]
    Closed,
}

/// A block that was not queued, handed back so the caller can retry it instead of
/// dropping payload it already received from a peer.
#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub struct BlockRejected {
    /// The block that was rejected.
    pub block: Block,
    /// Why it was rejected.
    #[source]
    pub reason: SubmitError,
}

/// Completion of a whole piece.
#[derive(Debug)]
pub struct WriteEvent {
    /// Piece index.
    pub piece: u32,
    /// `Ok` once every byte of the piece was written, `Err` when it was rejected or
    /// the backend failed. Pieces that were only partially flushed (eviction,
    /// timeout, shutdown) produce no event: they are not complete.
    pub result: Result<(), DiskError>,
}

/// Handle to the writer thread. Cheap to keep behind the torrent actor; all methods
/// are safe to call from any thread.
pub struct DiskWriter {
    tx: mpsc::Sender<Command>,
    ticker_stop: Arc<AtomicBool>,
}

impl DiskWriter {
    /// Opens the torrent's files and spawns the writer thread.
    ///
    /// Returns the writer and the receiver for piece completions. The receiver must be
    /// drained: the event channel is bounded, so a stalled consumer eventually stops
    /// the writer (that is the intended backpressure, not a bug).
    pub fn start(
        config: WriterConfig,
        layout: Arc<FileLayout>,
        backend: Arc<dyn BlockBackend>,
        root: PathBuf,
    ) -> Result<(Self, mpsc::Receiver<WriteEvent>), DiskError> {
        if config.piece_length == 0 {
            return Err(DiskError::ZeroPieceLength);
        }
        if config.queue_capacity == 0 {
            return Err(DiskError::InvalidConfig {
                name: "queue_capacity",
            });
        }
        if config.cache_bytes == 0 {
            return Err(DiskError::InvalidConfig {
                name: "cache_bytes",
            });
        }
        if config.event_capacity == 0 {
            return Err(DiskError::InvalidConfig {
                name: "event_capacity",
            });
        }
        // Fail fast on a piece length the layout cannot support.
        let _ = layout.piece_count(config.piece_length)?;

        let files = FileSet::open_with(layout.clone(), backend, &root, config.allocation)?;
        let (tx, mut rx) = mpsc::channel(config.queue_capacity);
        let (event_tx, event_rx) = mpsc::channel(config.event_capacity);

        let worker = Worker {
            layout,
            files,
            piece_length: config.piece_length,
            cache: PieceCache::new(config.cache_bytes),
            events: event_tx,
            flush_after: config.flush_interval,
            last_expire: Instant::now(),
        };
        thread::Builder::new()
            .name("flux-disk-writer".to_owned())
            .spawn(move || worker.run(&mut rx))
            .map_err(|err| DiskError::spawn("writer", err))?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        if let Some(interval) = config.flush_interval
            && !interval.is_zero()
        {
            spawn_ticker(tx.clone(), Arc::clone(&ticker_stop), interval)?;
        }

        Ok((Self { tx, ticker_stop }, event_rx))
    }

    /// Queues a block, waiting for capacity. This is the path the peer actor uses;
    /// awaiting here is what makes a fast swarm apply backpressure to itself.
    pub async fn write_block(&self, block: Block) -> Result<(), DiskError> {
        self.tx
            .send(Command::Block(block))
            .await
            .map_err(|_| DiskError::Closed)
    }

    /// Queues a block without waiting. On [`SubmitError::Full`] the block is returned
    /// inside [`BlockRejected`] so the caller can retry it instead of dropping payload
    /// it already paid for.
    pub fn try_write_block(&self, block: Block) -> Result<(), BlockRejected> {
        // Reserve capacity first: a permit is taken without consuming the block, so a
        // full queue hands the payload straight back to the caller.
        match self.tx.try_reserve() {
            Ok(permit) => {
                permit.send(Command::Block(block));
                Ok(())
            }
            Err(TrySendError::Full(())) => Err(BlockRejected {
                block,
                reason: SubmitError::Full,
            }),
            Err(TrySendError::Closed(())) => Err(BlockRejected {
                block,
                reason: SubmitError::Closed,
            }),
        }
    }

    /// Free command slots right now.
    pub fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    /// Writes every buffered (possibly incomplete) piece and returns once the cache is
    /// empty. Does not fsync; use [`Self::sync_all`] for that.
    ///
    /// Keep receiving completion events while this future is pending: the event channel
    /// is bounded and the worker will wait on it before acknowledging.
    pub async fn flush(&self) -> Result<(), DiskError> {
        self.round_trip(Command::Flush).await
    }

    /// Flushes and fsyncs every file.
    ///
    /// Same draining requirement as [`Self::flush`].
    pub async fn sync_all(&self) -> Result<(), DiskError> {
        self.round_trip(Command::SyncAll).await
    }

    /// Flushes, fsyncs and stops the writer thread.
    pub async fn shutdown(self) -> Result<(), DiskError> {
        self.ticker_stop.store(true, Ordering::Relaxed);
        let (ack, done) = oneshot::channel();
        self.tx
            .send(Command::Shutdown(ack))
            .await
            .map_err(|_| DiskError::Closed)?;
        done.await.map_err(|_| DiskError::WorkerPanicked)?
    }

    async fn round_trip(
        &self,
        make: fn(oneshot::Sender<Result<(), DiskError>>) -> Command,
    ) -> Result<(), DiskError> {
        let (ack, done) = oneshot::channel();
        self.tx
            .send(make(ack))
            .await
            .map_err(|_| DiskError::Closed)?;
        done.await.map_err(|_| DiskError::WorkerPanicked)?
    }
}

impl Drop for DiskWriter {
    fn drop(&mut self) {
        // Without (or even with) an explicit shutdown, stopping the timer lets the
        // channel close so the worker can flush and exit instead of leaking a thread.
        self.ticker_stop.store(true, Ordering::Relaxed);
    }
}

enum Command {
    Block(Block),
    Flush(oneshot::Sender<Result<(), DiskError>>),
    SyncAll(oneshot::Sender<Result<(), DiskError>>),
    Shutdown(oneshot::Sender<Result<(), DiskError>>),
    Tick,
}

fn spawn_ticker(
    tx: mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
    interval: Duration,
) -> Result<(), DiskError> {
    thread::Builder::new()
        .name("flux-disk-writer-timer".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(interval);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                match tx.try_send(Command::Tick) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Closed(_)) => return,
                }
            }
        })
        .map(|_| ())
        .map_err(|err| DiskError::spawn("writer timer", err))
}

struct Worker {
    layout: Arc<FileLayout>,
    files: FileSet,
    piece_length: u32,
    cache: PieceCache,
    events: mpsc::Sender<WriteEvent>,
    flush_after: Option<Duration>,
    last_expire: Instant,
}

impl Worker {
    fn run(mut self, rx: &mut mpsc::Receiver<Command>) {
        loop {
            match rx.blocking_recv() {
                Some(Command::Block(block)) => self.accept(block),
                Some(Command::Flush(ack)) => {
                    let result = self.flush_pending();
                    let _ = ack.send(result);
                }
                Some(Command::SyncAll(ack)) => {
                    let result = self.flush_pending().and(self.files.sync_all());
                    let _ = ack.send(result);
                }
                Some(Command::Tick) => self.expire(),
                Some(Command::Shutdown(ack)) => {
                    let result = self.finish();
                    let _ = ack.send(result);
                    return;
                }
                None => {
                    let _ = self.finish();
                    return;
                }
            }
        }
    }

    fn emit(&self, piece: u32, result: Result<(), DiskError>) {
        // A closed event channel means the daemon stopped listening; the data is on
        // disk, so there is nothing to recover.
        let _ = self.events.blocking_send(WriteEvent { piece, result });
    }

    fn accept(&mut self, block: Block) {
        let piece = block.piece;
        if let Err(err) = self.try_accept(block) {
            self.emit(piece, Err(err));
        }
    }

    fn try_accept(&mut self, block: Block) -> Result<(), DiskError> {
        if block.data.is_empty() {
            return Err(DiskError::EmptyBlock {
                piece: block.piece,
                begin: block.begin,
            });
        }
        let (piece_offset, piece_len) = self.layout.piece_range(self.piece_length, block.piece)?;
        let end = u64::from(block.begin) + block.data.len() as u64;
        if end > u64::from(piece_len) {
            return Err(DiskError::BlockOutOfBounds {
                piece: block.piece,
                begin: block.begin,
                len: block.data.len(),
                piece_length: piece_len,
            });
        }

        // Timeout-based flushing happens on the way in, so an idle torrent is not the
        // only case that gets its partial pieces written out.
        self.expire();

        let piece = block.piece;
        if self.cache.insert(piece, piece_len, block)? {
            if let Some(buffered) = self.cache.take(piece) {
                self.write_complete(piece, piece_offset, piece_len, buffered);
            }
        } else {
            for (index, buffered) in self.cache.evict_over_budget() {
                if let Err(err) = self.write_partial_at(index, buffered) {
                    self.emit(index, Err(err));
                }
            }
        }
        Ok(())
    }

    /// Writes out pieces whose buffered blocks are older than the flush interval.
    ///
    /// The scan is throttled to one pass per interval: it is called on every accepted
    /// block, and scanning the cache thousands of times a second would be pure
    /// overhead. The byte budget, not the timer, is what actually bounds memory.
    fn expire(&mut self) {
        let Some(max_age) = self.flush_after else {
            return;
        };
        let now = Instant::now();
        if now.saturating_duration_since(self.last_expire) < max_age {
            return;
        }
        self.last_expire = now;
        for (index, buffered) in self.cache.expire(max_age) {
            if let Err(err) = self.write_partial_at(index, buffered) {
                self.emit(index, Err(err));
            }
        }
    }

    /// Writes a complete piece as one contiguous range and reports the outcome.
    fn write_complete(
        &mut self,
        piece: u32,
        piece_offset: u64,
        piece_len: u32,
        mut buffered: PieceBuffers,
    ) {
        let mut buffer = vec![0_u8; piece_len as usize];
        for block in buffered.blocks.drain(..) {
            let begin = block.begin as usize;
            let end = begin.saturating_add(block.data.len());
            match buffer.get_mut(begin..end) {
                Some(target) => target.copy_from_slice(&block.data),
                None => {
                    self.emit(
                        piece,
                        Err(DiskError::BlockOutOfBounds {
                            piece,
                            begin: block.begin,
                            len: block.data.len(),
                            piece_length: piece_len,
                        }),
                    );
                    return;
                }
            }
        }
        let result = self.files.write_all_at(piece_offset, &buffer);
        self.emit(piece, result);
    }

    fn write_partial_at(&mut self, piece: u32, buffered: PieceBuffers) -> Result<(), DiskError> {
        let (offset, _len) = self.layout.piece_range(self.piece_length, piece)?;
        self.write_partial(offset, buffered)
    }

    /// Writes the contiguous runs of an incomplete piece. No completion event: the
    /// piece still has holes.
    fn write_partial(
        &mut self,
        piece_offset: u64,
        mut buffered: PieceBuffers,
    ) -> Result<(), DiskError> {
        buffered.blocks.sort_by_key(|block| block.begin);
        let mut run_begin = 0_u32;
        let mut run: Vec<u8> = Vec::new();
        for block in buffered.blocks {
            if !run.is_empty() && u64::from(block.begin) != u64::from(run_begin) + run.len() as u64
            {
                self.files
                    .write_all_at(piece_offset + u64::from(run_begin), &run)?;
                run.clear();
            }
            if run.is_empty() {
                run_begin = block.begin;
            }
            run.extend_from_slice(&block.data);
        }
        if !run.is_empty() {
            self.files
                .write_all_at(piece_offset + u64::from(run_begin), &run)?;
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<(), DiskError> {
        let mut first_error = None;
        for (piece, buffered) in self.cache.take_all() {
            if let Err(err) = self.write_partial_at(piece, buffered)
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn finish(&mut self) -> Result<(), DiskError> {
        self.flush_pending().and(self.files.sync_all())
    }
}

/// Blocks buffered for one piece, in arrival order.
#[derive(Debug)]
struct PieceBuffers {
    blocks: Vec<Block>,
    bytes: usize,
    seq: u64,
    touched: Instant,
}

impl PieceBuffers {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            bytes: 0,
            seq: 0,
            touched: Instant::now(),
        }
    }
}

/// Byte-bounded LRU of incomplete pieces.
struct PieceCache {
    budget: usize,
    used: usize,
    clock: u64,
    pieces: HashMap<u32, PieceBuffers>,
    /// `seq -> piece`, oldest first, so eviction is `pop_first` instead of a scan.
    order: BTreeMap<u64, u32>,
}

impl PieceCache {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            clock: 0,
            pieces: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    fn used(&self) -> usize {
        self.used
    }

    /// Adds a block. Returns `true` when the piece is now complete. Rejects blocks
    /// that overlap something already buffered, which would otherwise double-count
    /// bytes and make a piece look complete before it is.
    fn insert(&mut self, piece: u32, expected_len: u32, block: Block) -> Result<bool, DiskError> {
        let begin = u64::from(block.begin);
        let end = begin + block.data.len() as u64;
        if let Some(existing) = self.pieces.get(&piece) {
            for buffered in &existing.blocks {
                let buffered_begin = u64::from(buffered.begin);
                let buffered_end = buffered_begin + buffered.data.len() as u64;
                if begin < buffered_end && buffered_begin < end {
                    return Err(DiskError::BlockOverlap {
                        piece,
                        begin: block.begin,
                    });
                }
            }
        }

        self.clock += 1;
        let seq = self.clock;
        let len = block.data.len();
        self.used += len;

        let (previous, complete) = {
            let entry = self.pieces.entry(piece).or_insert_with(PieceBuffers::new);
            entry.blocks.push(block);
            entry.bytes += len;
            let previous = entry.seq;
            entry.seq = seq;
            entry.touched = Instant::now();
            (previous, entry.bytes == expected_len as usize)
        };

        if previous != 0 {
            self.order.remove(&previous);
        }
        self.order.insert(seq, piece);
        Ok(complete)
    }

    fn take(&mut self, piece: u32) -> Option<PieceBuffers> {
        let entry = self.pieces.remove(&piece)?;
        if entry.seq != 0 {
            self.order.remove(&entry.seq);
        }
        self.used = self.used.saturating_sub(entry.bytes);
        Some(entry)
    }

    /// Drops least-recently-touched pieces until the cache fits the budget again.
    fn evict_over_budget(&mut self) -> Vec<(u32, PieceBuffers)> {
        let mut evicted = Vec::new();
        while self.used() > self.budget {
            let Some((&seq, &piece)) = self.order.iter().next() else {
                break;
            };
            self.order.remove(&seq);
            if let Some(entry) = self.pieces.remove(&piece) {
                self.used = self.used.saturating_sub(entry.bytes);
                evicted.push((piece, entry));
            }
        }
        evicted
    }

    fn expire(&mut self, max_age: Duration) -> Vec<(u32, PieceBuffers)> {
        let now = Instant::now();
        let stale: Vec<u32> = self
            .pieces
            .iter()
            .filter(|(_, entry)| now.saturating_duration_since(entry.touched) >= max_age)
            .map(|(&piece, _)| piece)
            .collect();
        let mut expired = Vec::new();
        for piece in stale {
            if let Some(entry) = self.take(piece) {
                expired.push((piece, entry));
            }
        }
        expired
    }

    fn take_all(&mut self) -> Vec<(u32, PieceBuffers)> {
        self.order.clear();
        self.used = 0;
        let mut all: Vec<(u32, PieceBuffers)> = self.pieces.drain().collect();
        all.sort_by_key(|(_, entry)| entry.seq);
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_never_exceeds_byte_budget() -> Result<(), DiskError> {
        let mut cache = PieceCache::new(1024);
        let mut evicted_pieces = 0;
        for piece in 0..4_u32 {
            for block in 0..8_u32 {
                let complete =
                    cache.insert(piece, 4096, Block::new(piece, block * 256, vec![0_u8; 256]))?;
                assert!(!complete, "piece must not look complete at 2 KiB of 4 KiB");
                // The writer evicts right after accepting a block; the bound is enforced
                // by that pair, not by `insert` alone.
                evicted_pieces += cache.evict_over_budget().len();
                assert!(
                    cache.used() <= 1024,
                    "cache grew to {} bytes over a 1024 byte budget",
                    cache.used()
                );
            }
        }
        assert!(evicted_pieces > 0, "the budget must force evictions");
        assert!(cache.used() <= 1024);
        Ok(())
    }

    #[test]
    fn cache_rejects_overlapping_blocks() -> Result<(), DiskError> {
        let mut cache = PieceCache::new(1 << 20);
        assert!(!cache.insert(0, 512, Block::new(0, 0, vec![1; 256]))?);
        assert!(matches!(
            cache.insert(0, 512, Block::new(0, 128, vec![2; 256])),
            Err(DiskError::BlockOverlap { .. })
        ));
        assert!(cache.insert(0, 512, Block::new(0, 256, vec![3; 256]))?);
        Ok(())
    }
}
