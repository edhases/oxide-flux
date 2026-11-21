//! Piece and block selection.
//!
//! The picker is pure state: it owns the local piece state, the per-peer
//! availability sets and the in-flight request table. It has no clock and does
//! no I/O, which makes the ordering and endgame behaviour byte-exactly testable.
//!
//! Two policies are supported. [`Mode::Sequential`] walks pieces from the
//! playhead forward and is the default for media playback. [`Mode::RarestFirst`]
//! still serves a lookahead window sequentially and only then prefers rare
//! pieces, so streaming is never starved by swarm health heuristics.
//!
//! A block is never lost: [`Picker::on_peer_gone`] and
//! [`Picker::on_peer_choked`] return every request that only that peer held to
//! the unrequested state.

use std::collections::{HashMap, HashSet, VecDeque};

/// Identifier the engine uses for a peer connection.
pub type PeerId = u64;

/// Default request size: 16 KiB, the de-facto standard.
pub const DEFAULT_BLOCK_SIZE: u32 = 16 * 1024;
/// Default number of pieces in the streaming priority window.
pub const DEFAULT_LOOKAHEAD_PIECES: usize = 8;
/// Default maximum number of in-flight requests per peer.
pub const DEFAULT_MAX_IN_FLIGHT_PER_PEER: usize = 16;
/// Default number of remaining blocks at which endgame starts.
pub const DEFAULT_ENDGAME_BLOCKS: usize = 8;
/// Default maximum number of queued cancels.
pub const DEFAULT_CANCEL_BATCH: usize = 32;

/// Piece selection policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Stream from the playhead forward, wrapping around to backfill.
    #[default]
    Sequential,
    /// Serve the streaming window in order, then pick the rarest piece.
    RarestFirst,
}

/// A request to send to a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRequest {
    /// Peer the request is for.
    pub peer: PeerId,
    /// Piece index.
    pub piece: usize,
    /// Byte offset inside the piece.
    pub offset: u32,
    /// Request length in bytes.
    pub length: u32,
}

/// A cancel to send to a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancel {
    /// Peer the cancel is for.
    pub peer: PeerId,
    /// Piece index.
    pub piece: usize,
    /// Byte offset inside the piece.
    pub offset: u32,
    /// Length of the original request.
    pub length: u32,
}

/// A block identity inside the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BlockKey {
    piece: usize,
    block: u32,
}

/// Per-peer state owned by the picker.
#[derive(Debug)]
struct PeerState {
    available: HashSet<usize>,
    in_flight: HashSet<BlockKey>,
}

/// Why a picker operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The piece sizes, block size or totals were invalid.
    #[error("invalid picker configuration: {0}")]
    InvalidConfig(&'static str),
    /// The peer has not been registered with [`Picker::add_peer_availability`].
    #[error("peer {0} is not registered")]
    UnknownPeer(PeerId),
    /// A piece index was outside the piece count.
    #[error("piece {piece} is out of range")]
    PieceOutOfRange {
        /// The offending index.
        piece: usize,
    },
    /// The peer never advertised the piece it sent data for.
    #[error("peer {peer} never advertised piece {piece}")]
    PeerDidNotHavePiece {
        /// Peer that sent the block.
        peer: PeerId,
        /// Piece index.
        piece: usize,
    },
    /// A block arrived that this peer was never asked for.
    #[error("peer {peer} sent unrequested block at piece {piece} offset {offset}")]
    BlockNotRequested {
        /// Peer that sent the block.
        peer: PeerId,
        /// Piece index.
        piece: usize,
        /// Byte offset.
        offset: u32,
    },
    /// The offset or length did not match a real block boundary.
    #[error("block at piece {piece} offset {offset} length {length} is not a valid block")]
    InvalidBlock {
        /// Piece index.
        piece: usize,
        /// Byte offset.
        offset: u32,
        /// Requested length.
        length: u32,
    },
    /// The block had already been received.
    #[error("block at piece {piece} offset {offset} was already received")]
    BlockAlreadyReceived {
        /// Piece index.
        piece: usize,
        /// Byte offset.
        offset: u32,
    },
    /// A wire bitfield had the wrong number of bytes.
    #[error("bitfield has {actual} bytes, expected {expected}")]
    BitfieldLengthMismatch {
        /// Expected length derived from the piece count.
        expected: usize,
        /// Observed length.
        actual: usize,
    },
}

/// Piece selection state machine.
#[derive(Debug)]
pub struct Picker {
    piece_sizes: Vec<u32>,
    block_size: u32,
    piece_blocks: Vec<u32>,
    piece_complete: Vec<bool>,
    received: Vec<Vec<u64>>,
    received_blocks: Vec<u32>,
    piece_count: usize,
    total_length: u64,
    remaining_blocks: usize,
    completed_pieces: usize,
    requests: HashMap<BlockKey, HashSet<PeerId>>,
    peers: HashMap<PeerId, PeerState>,
    availability: Vec<u32>,
    order: Vec<usize>,
    cancels: VecDeque<Cancel>,
    mode: Mode,
    playhead: usize,
    lookahead_pieces: usize,
    max_in_flight_per_peer: usize,
    endgame_blocks: usize,
    max_cancel_batch: usize,
}

impl Picker {
    /// Create a picker for a contiguous piece space (v1-style layout).
    ///
    /// `piece_count` must equal `ceil(total_length / piece_length)`. Blocks are
    /// 16 KiB by default; see [`Picker::with_piece_sizes`] for v2 layouts where
    /// individual files can end with short pieces.
    pub fn new(piece_count: usize, piece_length: u32, total_length: u64) -> Result<Self, Error> {
        if piece_count == 0 {
            return Err(Error::InvalidConfig("piece count must be positive"));
        }
        if piece_length == 0 {
            return Err(Error::InvalidConfig("piece length must be positive"));
        }
        if total_length == 0 {
            return Err(Error::InvalidConfig("total length must be positive"));
        }
        let expected = total_length.div_ceil(u64::from(piece_length));
        if expected != piece_count as u64 {
            return Err(Error::InvalidConfig(
                "piece count does not match total length and piece length",
            ));
        }
        let sizes = (0..piece_count)
            .map(|piece| {
                let start = (piece as u64) * u64::from(piece_length);
                let remaining = total_length - start;
                remaining.min(u64::from(piece_length)) as u32
            })
            .collect();
        Self::with_piece_sizes(sizes, DEFAULT_BLOCK_SIZE)
    }

    /// Create a picker with explicit per-piece sizes.
    ///
    /// This is the form needed for v2 torrents, where each file may end with a
    /// short piece, and is also convenient for tests.
    pub fn with_piece_sizes(piece_sizes: Vec<u32>, block_size: u32) -> Result<Self, Error> {
        if piece_sizes.is_empty() {
            return Err(Error::InvalidConfig("piece count must be positive"));
        }
        if block_size == 0 {
            return Err(Error::InvalidConfig("block size must be positive"));
        }
        if piece_sizes.contains(&0) {
            return Err(Error::InvalidConfig("piece sizes must be positive"));
        }

        let piece_count = piece_sizes.len();
        let mut total_length = 0u64;
        let mut piece_blocks = Vec::with_capacity(piece_count);
        let mut received = Vec::with_capacity(piece_count);
        let mut remaining_blocks = 0usize;
        for size in &piece_sizes {
            total_length = total_length
                .checked_add(u64::from(*size))
                .ok_or(Error::InvalidConfig("total length overflow"))?;
            let blocks = size.div_ceil(block_size);
            remaining_blocks = remaining_blocks
                .checked_add(blocks as usize)
                .ok_or(Error::InvalidConfig("too many blocks"))?;
            piece_blocks.push(blocks);
            received.push(vec![0u64; blocks.div_ceil(64) as usize]);
        }

        let mut picker = Self {
            piece_count,
            total_length,
            piece_sizes,
            block_size,
            piece_blocks,
            piece_complete: vec![false; piece_count],
            received,
            received_blocks: vec![0; piece_count],
            remaining_blocks,
            completed_pieces: 0,
            requests: HashMap::new(),
            peers: HashMap::new(),
            availability: vec![0; piece_count],
            order: Vec::with_capacity(piece_count),
            cancels: VecDeque::new(),
            mode: Mode::default(),
            playhead: 0,
            lookahead_pieces: DEFAULT_LOOKAHEAD_PIECES,
            max_in_flight_per_peer: DEFAULT_MAX_IN_FLIGHT_PER_PEER,
            endgame_blocks: DEFAULT_ENDGAME_BLOCKS,
            max_cancel_batch: DEFAULT_CANCEL_BATCH,
        };
        picker.endgame_blocks = picker.endgame_blocks.min(picker.remaining_blocks.max(1));
        Ok(picker)
    }

    /// Current selection policy.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Change the selection policy.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Current streaming playhead, as a piece index.
    pub fn playhead(&self) -> usize {
        self.playhead
    }

    /// Move the streaming playhead. The value is clamped to the last piece.
    pub fn set_playhead(&mut self, piece: usize) {
        self.playhead = piece.min(self.piece_count.saturating_sub(1));
    }

    /// Set the number of pieces in the streaming priority window.
    pub fn set_lookahead_pieces(&mut self, pieces: usize) {
        self.lookahead_pieces = pieces;
    }

    /// Set the maximum number of outstanding requests per peer.
    pub fn set_max_in_flight_per_peer(&mut self, requests: usize) {
        self.max_in_flight_per_peer = requests;
    }

    /// Set how many remaining blocks trigger endgame duplicate requests.
    ///
    /// Zero disables endgame. The initial value is clamped to the torrent size.
    pub fn set_endgame_blocks(&mut self, blocks: usize) {
        self.endgame_blocks = blocks;
    }

    /// Set the maximum number of queued cancels.
    pub fn set_max_cancel_batch(&mut self, cancels: usize) {
        self.max_cancel_batch = cancels;
        while self.cancels.len() > cancels {
            self.cancels.pop_front();
        }
    }

    /// Number of pieces.
    pub fn piece_count(&self) -> usize {
        self.piece_count
    }

    /// Sum of all piece sizes in bytes.
    pub fn total_length(&self) -> u64 {
        self.total_length
    }

    /// Request size in bytes.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Blocks in `piece`, or `None` if the index is out of range.
    pub fn piece_blocks(&self, piece: usize) -> Option<u32> {
        self.piece_blocks.get(piece).copied()
    }

    /// Size of `piece`, or `None` if the index is out of range.
    pub fn piece_size(&self, piece: usize) -> Option<u32> {
        self.piece_sizes.get(piece).copied()
    }

    /// Number of pieces still missing at least one block.
    pub fn completed_pieces(&self) -> usize {
        self.completed_pieces
    }

    /// Number of blocks not yet received.
    pub fn remaining_blocks(&self) -> usize {
        self.remaining_blocks
    }

    /// Number of advertised peers for `piece`.
    pub fn piece_availability(&self, piece: usize) -> Option<u32> {
        self.availability.get(piece).copied()
    }

    /// Number of requests currently held by `peer`.
    pub fn in_flight(&self, peer: PeerId) -> usize {
        self.peers
            .get(&peer)
            .map_or(0, |state| state.in_flight.len())
    }

    /// Whether every block of `piece` has been received.
    pub fn is_piece_complete(&self, piece: usize) -> bool {
        self.piece_complete.get(piece).copied().unwrap_or(false)
    }

    /// Register or replace a peer's availability from a list of piece indices.
    ///
    /// Re-adding an existing peer discards its previous state; any requests it
    /// held are requeued rather than lost.
    pub fn add_peer_availability(
        &mut self,
        peer: PeerId,
        available: impl IntoIterator<Item = usize>,
    ) -> Result<(), Error> {
        let mut pieces = HashSet::new();
        for piece in available {
            if piece >= self.piece_count {
                return Err(Error::PieceOutOfRange { piece });
            }
            pieces.insert(piece);
        }
        if self.peers.contains_key(&peer) {
            self.on_peer_gone(peer);
        }
        for piece in &pieces {
            self.availability[*piece] = self.availability[*piece].saturating_add(1);
        }
        self.peers.insert(
            peer,
            PeerState {
                available: pieces,
                in_flight: HashSet::new(),
            },
        );
        Ok(())
    }

    /// Register a peer from a wire bitfield (MSB-first, spare bits ignored).
    pub fn add_peer_bitfield(&mut self, peer: PeerId, bitfield: &[u8]) -> Result<(), Error> {
        let expected = self.piece_count.div_ceil(8);
        if bitfield.len() != expected {
            return Err(Error::BitfieldLengthMismatch {
                expected,
                actual: bitfield.len(),
            });
        }
        let pieces = (0..self.piece_count)
            .filter(|piece| bitfield[piece / 8] & (0x80 >> (piece % 8)) != 0)
            .collect::<Vec<_>>();
        self.add_peer_availability(peer, pieces)
    }

    /// Record that `peer` just advertised `piece` with a `have` message.
    pub fn add_peer_have(&mut self, peer: PeerId, piece: usize) -> Result<(), Error> {
        if piece >= self.piece_count {
            return Err(Error::PieceOutOfRange { piece });
        }
        let state = self.peers.get_mut(&peer).ok_or(Error::UnknownPeer(peer))?;
        if state.available.insert(piece) {
            self.availability[piece] = self.availability[piece].saturating_add(1);
        }
        Ok(())
    }

    /// Drop a peer and requeue every request that only it held.
    ///
    /// Returns the number of blocks that are no longer in flight anywhere and
    /// are therefore immediately selectable again.
    pub fn on_peer_gone(&mut self, peer: PeerId) -> usize {
        let Some(state) = self.peers.remove(&peer) else {
            return 0;
        };
        for piece in &state.available {
            self.availability[*piece] = self.availability[*piece].saturating_sub(1);
        }
        self.cancels.retain(|cancel| cancel.peer != peer);

        let mut requeued = 0;
        for key in state.in_flight {
            let empty = match self.requests.get_mut(&key) {
                Some(requesters) => {
                    requesters.remove(&peer);
                    requesters.is_empty()
                }
                None => false,
            };
            if empty {
                self.requests.remove(&key);
                requeued += 1;
            }
        }
        requeued
    }

    /// Requeue everything `peer` had outstanding while keeping it registered.
    ///
    /// This is what a choke (or a connection reset that will be retried) needs:
    /// the peer stays in the swarm, but its requests stop blocking other peers.
    pub fn on_peer_choked(&mut self, peer: PeerId) -> usize {
        let Some(state) = self.peers.get_mut(&peer) else {
            return 0;
        };
        let keys: Vec<BlockKey> = state.in_flight.drain().collect();
        let mut requeued = 0;
        for key in keys {
            let empty = match self.requests.get_mut(&key) {
                Some(requesters) => {
                    requesters.remove(&peer);
                    requesters.is_empty()
                }
                None => false,
            };
            if empty {
                self.requests.remove(&key);
                requeued += 1;
            }
        }
        requeued
    }

    /// Choose the next block for `peer`, or `None` if it cannot take one.
    ///
    /// The returned request is recorded as in flight. During endgame the same
    /// block may be requested from several peers; receiving it queues cancels
    /// for the other copies, drained with [`Picker::take_cancels`].
    pub fn pick(&mut self, peer: PeerId) -> Option<BlockRequest> {
        if self.remaining_blocks == 0 {
            return None;
        }
        let state = self.peers.get(&peer)?;
        if state.in_flight.len() >= self.max_in_flight_per_peer {
            return None;
        }
        let endgame = self.endgame_blocks > 0 && self.remaining_blocks <= self.endgame_blocks;
        let key = self.select_block(peer, endgame)?;
        let request = self.request_for(peer, key)?;
        self.record_request(peer, key);
        Some(request)
    }

    /// Requeues one block a peer no longer has to answer.
    ///
    /// Used by timeouts: the request is forgotten so a late block is rejected as
    /// unrequested, while other healthy requests on the same peer stay intact.
    pub fn requeue_block(&mut self, peer: PeerId, piece: usize, offset: u32) -> bool {
        if self.block_size == 0 || !offset.is_multiple_of(self.block_size) {
            return false;
        }
        let key = BlockKey {
            piece,
            block: offset / self.block_size,
        };
        let Some(state) = self.peers.get_mut(&peer) else {
            return false;
        };
        if !state.in_flight.remove(&key) {
            return false;
        }
        if let Some(requesters) = self.requests.get_mut(&key) {
            requesters.remove(&peer);
            if requesters.is_empty() {
                self.requests.remove(&key);
            }
        }
        true
    }

    /// Mark a block as received from `peer`.
    ///
    /// Clears its in-flight record and, if duplicate requests existed, queues a
    /// cancel for each other peer. Blocks from peers that never advertised the
    /// piece, blocks that were already received and blocks that do not align to
    /// the piece's block layout are rejected without changing state.
    pub fn on_block_received(
        &mut self,
        peer: PeerId,
        piece: usize,
        offset: u32,
        length: u32,
    ) -> Result<(), Error> {
        let key = self.validate_block(peer, piece, offset, length)?;
        if self.block_received(key) {
            return Err(Error::BlockAlreadyReceived { piece, offset });
        }
        // Provenance: only the peers we actually asked may deliver this block.
        // Without this, any connection could inject bytes for a piece another
        // peer is fetching, and a corrupt piece would have no author.
        let requested = self
            .requests
            .get(&key)
            .is_some_and(|requesters| requesters.contains(&peer));
        if !requested {
            return Err(Error::BlockNotRequested {
                peer,
                piece,
                offset,
            });
        }

        self.set_block_received(key);
        self.received_blocks[piece] += 1;
        self.remaining_blocks = self.remaining_blocks.saturating_sub(1);
        if self.received_blocks[piece] == self.piece_blocks[piece] {
            self.piece_complete[piece] = true;
            self.completed_pieces += 1;
        }

        if let Some(requesters) = self.requests.remove(&key) {
            for requester in requesters {
                if let Some(state) = self.peers.get_mut(&requester) {
                    state.in_flight.remove(&key);
                }
                if requester != peer {
                    self.queue_cancel(requester, key);
                }
            }
        }
        Ok(())
    }

    /// Mark a whole piece as already present, for example after disk verification.
    ///
    /// Any requests for its blocks are cleared and cancelled. Repeating the call
    /// is a no-op.
    pub fn mark_complete(&mut self, piece: usize) -> Result<(), Error> {
        if piece >= self.piece_count {
            return Err(Error::PieceOutOfRange { piece });
        }
        if self.piece_complete[piece] {
            return Ok(());
        }
        let blocks = self.piece_blocks[piece];
        for block in 0..blocks {
            let key = BlockKey { piece, block };
            if self.block_received(key) {
                continue;
            }
            self.set_block_received(key);
            self.received_blocks[piece] += 1;
            self.remaining_blocks = self.remaining_blocks.saturating_sub(1);
            if let Some(requesters) = self.requests.remove(&key) {
                for requester in requesters {
                    if let Some(state) = self.peers.get_mut(&requester) {
                        state.in_flight.remove(&key);
                    }
                    self.queue_cancel(requester, key);
                }
            }
        }
        self.piece_complete[piece] = true;
        self.completed_pieces += 1;
        Ok(())
    }

    /// Drop a piece's received blocks after a failed hash verification.
    ///
    /// Every block of the piece becomes selectable again. In-flight requests
    /// for blocks that had not arrived are left alone.
    pub fn reset_piece(&mut self, piece: usize) -> Result<(), Error> {
        if piece >= self.piece_count {
            return Err(Error::PieceOutOfRange { piece });
        }
        let blocks = self.piece_blocks[piece];
        let mut received = 0usize;
        for block in 0..blocks {
            let key = BlockKey { piece, block };
            if self.block_received(key) {
                self.clear_block_received(key);
                received += 1;
            }
        }
        self.received_blocks[piece] = 0;
        self.remaining_blocks += received;
        if self.piece_complete[piece] {
            self.piece_complete[piece] = false;
            self.completed_pieces = self.completed_pieces.saturating_sub(1);
        }
        Ok(())
    }

    /// Drain queued cancels.
    pub fn take_cancels(&mut self) -> Vec<Cancel> {
        self.cancels.drain(..).collect()
    }

    /// Number of cancels waiting to be sent.
    pub fn pending_cancels(&self) -> usize {
        self.cancels.len()
    }

    fn select_block(&mut self, peer: PeerId, endgame: bool) -> Option<BlockKey> {
        self.build_order();
        let count = self.order.len();

        for index in 0..count {
            let piece = self.order[index];
            if self.piece_complete[piece] || !self.peer_has(peer, piece) {
                continue;
            }
            let blocks = self.piece_blocks[piece];
            for block in 0..blocks {
                let key = BlockKey { piece, block };
                if self.block_received(key) || self.requests.contains_key(&key) {
                    continue;
                }
                return Some(key);
            }
        }

        if !endgame {
            return None;
        }

        for index in 0..count {
            let piece = self.order[index];
            if self.piece_complete[piece] || !self.peer_has(peer, piece) {
                continue;
            }
            let blocks = self.piece_blocks[piece];
            for block in 0..blocks {
                let key = BlockKey { piece, block };
                if self.block_received(key) {
                    continue;
                }
                if let Some(requesters) = self.requests.get(&key)
                    && !requesters.contains(&peer)
                {
                    return Some(key);
                }
            }
        }
        None
    }

    /// Fill the scratch order used by selection.
    fn build_order(&mut self) {
        let piece_count = self.piece_count;
        let start = self.playhead.min(piece_count.saturating_sub(1));
        self.order.clear();
        match self.mode {
            Mode::Sequential => {
                self.order.extend(start..piece_count);
                self.order.extend(0..start);
            }
            Mode::RarestFirst => {
                let window_end = start.saturating_add(self.lookahead_pieces).min(piece_count);
                self.order.extend(start..window_end);
                self.order.extend(0..start);
                self.order.extend(window_end..piece_count);
                let window_len = window_end - start;
                let availability = &self.availability;
                self.order[window_len..]
                    .sort_unstable_by_key(|&piece| (availability[piece], piece));
            }
        }
    }

    fn peer_has(&self, peer: PeerId, piece: usize) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|state| state.available.contains(&piece))
    }

    fn record_request(&mut self, peer: PeerId, key: BlockKey) {
        self.requests.entry(key).or_default().insert(peer);
        if let Some(state) = self.peers.get_mut(&peer) {
            state.in_flight.insert(key);
        }
    }

    fn request_for(&self, peer: PeerId, key: BlockKey) -> Option<BlockRequest> {
        let (offset, length) = self.block_geometry(key)?;
        Some(BlockRequest {
            peer,
            piece: key.piece,
            offset,
            length,
        })
    }

    fn block_geometry(&self, key: BlockKey) -> Option<(u32, u32)> {
        let piece_size = self.piece_sizes.get(key.piece)?;
        let offset = u32::try_from(u64::from(key.block) * u64::from(self.block_size)).ok()?;
        let remaining = piece_size.checked_sub(offset)?;
        Some((offset, remaining.min(self.block_size)))
    }

    fn validate_block(
        &self,
        peer: PeerId,
        piece: usize,
        offset: u32,
        length: u32,
    ) -> Result<BlockKey, Error> {
        let state = self.peers.get(&peer).ok_or(Error::UnknownPeer(peer))?;
        if piece >= self.piece_count {
            return Err(Error::PieceOutOfRange { piece });
        }
        if !state.available.contains(&piece) {
            return Err(Error::PeerDidNotHavePiece { peer, piece });
        }
        if self.block_size == 0 || !offset.is_multiple_of(self.block_size) {
            return Err(Error::InvalidBlock {
                piece,
                offset,
                length,
            });
        }
        let block = offset / self.block_size;
        let expected = self
            .block_geometry(BlockKey { piece, block })
            .map(|(_, length)| length)
            .ok_or(Error::InvalidBlock {
                piece,
                offset,
                length,
            })?;
        if length != expected {
            return Err(Error::InvalidBlock {
                piece,
                offset,
                length,
            });
        }
        Ok(BlockKey { piece, block })
    }

    fn block_received(&self, key: BlockKey) -> bool {
        let word = key.block as usize / 64;
        let bit = key.block as usize % 64;
        self.received[key.piece]
            .get(word)
            .is_some_and(|word| word & (1u64 << bit) != 0)
    }

    fn set_block_received(&mut self, key: BlockKey) {
        let word = key.block as usize / 64;
        let bit = key.block as usize % 64;
        if let Some(word) = self.received[key.piece].get_mut(word) {
            *word |= 1u64 << bit;
        }
    }

    fn clear_block_received(&mut self, key: BlockKey) {
        let word = key.block as usize / 64;
        let bit = key.block as usize % 64;
        if let Some(word) = self.received[key.piece].get_mut(word) {
            *word &= !(1u64 << bit);
        }
    }

    fn queue_cancel(&mut self, peer: PeerId, key: BlockKey) {
        if self.cancels.len() >= self.max_cancel_batch {
            return;
        }
        if let Some((offset, length)) = self.block_geometry(key) {
            self.cancels.push_back(Cancel {
                peer,
                piece: key.piece,
                offset,
                length,
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn picker(sizes: &[u32], block_size: u32) -> Picker {
        Picker::with_piece_sizes(sizes.to_vec(), block_size).unwrap()
    }

    fn drain(picker: &mut Picker, peer: PeerId, max: usize) -> Vec<BlockRequest> {
        let mut requests = Vec::new();
        while requests.len() < max {
            match picker.pick(peer) {
                Some(request) => requests.push(request),
                None => break,
            }
        }
        requests
    }

    fn offsets(requests: &[BlockRequest]) -> Vec<(usize, u32)> {
        requests
            .iter()
            .map(|request| (request.piece, request.offset))
            .collect()
    }

    #[test]
    fn sequential_order_is_piece_then_offset() {
        let mut picker = picker(&[8, 8, 8, 8], 4);
        picker.add_peer_availability(1, 0..4).unwrap();

        let requests = drain(&mut picker, 1, 16);
        assert_eq!(
            offsets(&requests),
            vec![
                (0, 0),
                (0, 4),
                (1, 0),
                (1, 4),
                (2, 0),
                (2, 4),
                (3, 0),
                (3, 4)
            ]
        );
        assert!(picker.pick(1).is_none());
    }

    #[test]
    fn sequential_order_follows_the_playhead() {
        let mut picker = picker(&[4, 4, 4, 4], 4);
        picker.add_peer_availability(1, 0..4).unwrap();
        picker.set_playhead(2);

        let first = drain(&mut picker, 1, 2);
        assert_eq!(
            first
                .iter()
                .map(|request| request.piece)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        let second = drain(&mut picker, 1, 2);
        assert_eq!(
            second
                .iter()
                .map(|request| request.piece)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );

        picker.set_playhead(99);
        assert_eq!(picker.playhead(), 3);
    }

    #[test]
    fn rarest_first_prefers_rare_pieces_outside_the_window() {
        let mut picker = picker(&[4, 4, 4], 4);
        picker.set_mode(Mode::RarestFirst);
        picker.set_lookahead_pieces(0);
        picker.add_peer_availability(1, [0, 1]).unwrap();
        picker.add_peer_availability(2, [0, 2]).unwrap();
        picker.add_peer_availability(3, [0]).unwrap();
        picker.add_peer_availability(9, [0, 1, 2]).unwrap();

        assert_eq!(picker.piece_availability(0), Some(4));
        assert_eq!(picker.piece_availability(1), Some(2));
        assert_eq!(picker.piece_availability(2), Some(2));

        let request = picker.pick(9).unwrap();
        assert_eq!(request.piece, 1);
        picker
            .on_block_received(9, request.piece, request.offset, request.length)
            .unwrap();

        let request = picker.pick(9).unwrap();
        assert_eq!(request.piece, 2);
        picker
            .on_block_received(9, request.piece, request.offset, request.length)
            .unwrap();

        let request = picker.pick(9).unwrap();
        assert_eq!(request.piece, 0);
    }

    #[test]
    fn streaming_window_is_not_starved_by_rarest_first() {
        let mut picker = picker(&[4, 4, 4], 4);
        picker.set_mode(Mode::RarestFirst);
        picker.set_lookahead_pieces(2);
        picker.add_peer_availability(1, [0, 1]).unwrap();
        picker.add_peer_availability(2, [0, 2]).unwrap();
        picker.add_peer_availability(9, [0, 1, 2]).unwrap();

        // Piece 0 and 1 are in the window and are picked in order even though
        // piece 2 is equally rare.
        assert_eq!(picker.pick(9).unwrap().piece, 0);
        assert_eq!(picker.pick(9).unwrap().piece, 1);
        assert_eq!(picker.pick(9).unwrap().piece, 2);
    }

    #[test]
    fn honors_per_peer_in_flight_limit() {
        let mut picker = picker(&[8, 8], 4);
        picker.set_max_in_flight_per_peer(2);
        picker.add_peer_availability(1, 0..2).unwrap();

        let first = picker.pick(1).unwrap();
        let second = picker.pick(1).unwrap();
        assert_eq!(picker.in_flight(1), 2);
        assert!(picker.pick(1).is_none());

        picker
            .on_block_received(1, first.piece, first.offset, first.length)
            .unwrap();
        assert_eq!(picker.in_flight(1), 1);
        let third = picker.pick(1).unwrap();
        assert_ne!((third.piece, third.offset), (first.piece, first.offset));
        assert_ne!((third.piece, third.offset), (second.piece, second.offset));
    }

    #[test]
    fn endgame_duplicates_and_cancels() {
        let mut picker = picker(&[4], 4);
        picker.add_peer_availability(1, [0]).unwrap();
        picker.add_peer_availability(2, [0]).unwrap();

        let first = picker.pick(1).unwrap();
        let duplicate = picker.pick(2).unwrap();
        assert_eq!(
            (first.piece, first.offset, first.length),
            (duplicate.piece, duplicate.offset, duplicate.length)
        );

        picker
            .on_block_received(1, first.piece, first.offset, first.length)
            .unwrap();

        let cancels = picker.take_cancels();
        assert_eq!(
            cancels,
            vec![Cancel {
                peer: 2,
                piece: 0,
                offset: 0,
                length: 4
            }]
        );
        assert_eq!(picker.in_flight(2), 0);
        assert!(picker.is_piece_complete(0));
        assert_eq!(picker.remaining_blocks(), 0);
        assert!(picker.take_cancels().is_empty());
    }

    #[test]
    fn endgame_cancels_are_batched_and_bounded() {
        let mut picker = picker(&[4, 4, 4], 4);
        picker.set_max_cancel_batch(2);
        picker.add_peer_availability(1, 0..3).unwrap();
        picker.add_peer_availability(2, 0..3).unwrap();

        for _ in 0..3 {
            picker.pick(1).unwrap();
        }
        for _ in 0..3 {
            picker.pick(2).unwrap();
        }
        for piece in 0..3 {
            picker.on_block_received(1, piece, 0, 4).unwrap();
        }

        assert_eq!(picker.pending_cancels(), 2);
        assert_eq!(picker.take_cancels().len(), 2);
    }

    #[test]
    fn peer_gone_requeues_every_block() {
        let mut picker = picker(&[4, 4], 4);
        picker.add_peer_availability(1, 0..2).unwrap();
        picker.add_peer_availability(2, 0..2).unwrap();

        let request = picker.pick(1).unwrap();
        assert_eq!(picker.in_flight(1), 1);
        assert_eq!(picker.on_peer_gone(1), 1);
        assert_eq!(picker.in_flight(1), 0);

        let replacement = picker.pick(2).unwrap();
        assert_eq!(
            (replacement.piece, replacement.offset),
            (request.piece, request.offset)
        );
        assert_eq!(picker.on_peer_gone(99), 0);
    }

    #[test]
    fn choke_requeues_without_dropping_the_peer() {
        let mut picker = picker(&[8, 8], 4);
        picker.add_peer_availability(1, 0..2).unwrap();

        let first = picker.pick(1).unwrap();
        let second = picker.pick(1).unwrap();
        assert_eq!(picker.on_peer_choked(1), 2);
        assert_eq!(picker.in_flight(1), 0);

        // Availability is untouched, so the peer can immediately requeue.
        let again = picker.pick(1).unwrap();
        assert_eq!((again.piece, again.offset), (first.piece, first.offset));
        assert_ne!((again.piece, again.offset), (second.piece, second.offset));
    }

    #[test]
    fn final_short_piece_uses_its_real_length() {
        let mut picker = Picker::new(3, 4, 10).unwrap();
        assert_eq!(picker.piece_size(2), Some(2));
        picker.add_peer_availability(1, 0..3).unwrap();

        let requests = drain(&mut picker, 1, 3);
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[2].piece, 2);
        assert_eq!(requests[2].offset, 0);
        assert_eq!(requests[2].length, 2);
    }

    #[test]
    fn one_block_torrent() {
        let mut picker = Picker::new(1, 3, 3).unwrap();
        picker.add_peer_availability(1, [0]).unwrap();

        let request = picker.pick(1).unwrap();
        assert_eq!(request.piece, 0);
        assert_eq!(request.offset, 0);
        assert_eq!(request.length, 3);
        // The same peer must never be asked for a block twice.
        assert!(picker.pick(1).is_none());

        picker
            .on_block_received(1, request.piece, request.offset, request.length)
            .unwrap();
        assert_eq!(picker.completed_pieces(), 1);
        assert_eq!(picker.remaining_blocks(), 0);
    }

    #[test]
    fn rejects_blocks_from_unknown_or_unqualified_peers() {
        let mut picker = picker(&[4], 4);
        picker.add_peer_availability(1, [0]).unwrap();
        picker.add_peer_availability(2, []).unwrap();

        assert!(matches!(
            picker.on_block_received(9, 0, 0, 4),
            Err(Error::UnknownPeer(9))
        ));
        assert!(matches!(
            picker.on_block_received(2, 0, 0, 4),
            Err(Error::PeerDidNotHavePiece { peer: 2, piece: 0 })
        ));
        assert!(matches!(
            picker.on_block_received(1, 9, 0, 4),
            Err(Error::PieceOutOfRange { piece: 9 })
        ));
        assert!(matches!(
            picker.on_block_received(1, 0, 1, 4),
            Err(Error::InvalidBlock { .. })
        ));
        assert!(matches!(
            picker.on_block_received(1, 0, 0, 3),
            Err(Error::InvalidBlock { .. })
        ));

        // None of the rejections changed state.
        assert_eq!(picker.remaining_blocks(), 1);

        let request = picker.pick(1).unwrap();
        picker
            .on_block_received(1, request.piece, request.offset, request.length)
            .unwrap();
        assert!(matches!(
            picker.on_block_received(1, 0, 0, 4),
            Err(Error::BlockAlreadyReceived {
                piece: 0,
                offset: 0
            })
        ));
    }

    #[test]
    fn parses_wire_bitfields() {
        let mut picker = picker(&[4, 4, 4, 4, 4, 4, 4, 4, 4, 4], 4);
        picker
            .add_peer_bitfield(1, &[0b1010_0000, 0b0100_0000])
            .unwrap();

        assert_eq!(picker.piece_availability(0), Some(1));
        assert_eq!(picker.piece_availability(1), Some(0));
        assert_eq!(picker.piece_availability(2), Some(1));
        assert_eq!(picker.piece_availability(9), Some(1));

        assert_eq!(picker.pick(1).unwrap().piece, 0);
        assert_eq!(picker.pick(1).unwrap().piece, 2);
        assert_eq!(picker.pick(1).unwrap().piece, 9);

        assert!(matches!(
            picker.add_peer_bitfield(2, &[0u8]),
            Err(Error::BitfieldLengthMismatch {
                expected: 2,
                actual: 1
            })
        ));
    }

    #[test]
    fn have_messages_extend_availability() {
        let mut picker = picker(&[4, 4], 4);
        picker.add_peer_availability(1, [0]).unwrap();
        picker.add_peer_have(1, 1).unwrap();
        assert_eq!(picker.piece_availability(1), Some(1));

        assert!(matches!(
            picker.add_peer_have(1, 2),
            Err(Error::PieceOutOfRange { piece: 2 })
        ));
        assert!(matches!(
            picker.add_peer_have(9, 0),
            Err(Error::UnknownPeer(9))
        ));
    }

    #[test]
    fn mark_complete_skips_known_pieces() {
        let mut picker = picker(&[4, 4], 4);
        picker.add_peer_availability(1, 0..2).unwrap();
        picker.mark_complete(0).unwrap();
        picker.mark_complete(0).unwrap();

        assert!(picker.is_piece_complete(0));
        assert_eq!(picker.completed_pieces(), 1);
        assert_eq!(picker.remaining_blocks(), 1);
        assert_eq!(picker.pick(1).unwrap().piece, 1);
        assert!(matches!(
            picker.mark_complete(2),
            Err(Error::PieceOutOfRange { piece: 2 })
        ));
    }

    #[test]
    fn failed_verification_makes_a_piece_selectable_again() {
        let mut picker = picker(&[4, 4], 4);
        picker.add_peer_availability(1, 0..2).unwrap();
        let request = picker.pick(1).unwrap();
        picker
            .on_block_received(1, request.piece, request.offset, request.length)
            .unwrap();
        assert!(picker.is_piece_complete(0));
        assert_eq!(picker.completed_pieces(), 1);
        assert_eq!(picker.remaining_blocks(), 1);

        picker.reset_piece(0).unwrap();
        assert!(!picker.is_piece_complete(0));
        assert_eq!(picker.completed_pieces(), 0);
        assert_eq!(picker.remaining_blocks(), 2);
        assert_eq!(picker.pick(1).unwrap().piece, 0);
        assert!(matches!(
            picker.reset_piece(2),
            Err(Error::PieceOutOfRange { piece: 2 })
        ));
    }

    #[test]
    fn validates_constructor_input() {
        assert!(matches!(Picker::new(0, 4, 4), Err(Error::InvalidConfig(_))));
        assert!(matches!(Picker::new(2, 4, 4), Err(Error::InvalidConfig(_))));
        assert!(matches!(Picker::new(1, 0, 4), Err(Error::InvalidConfig(_))));
        assert!(matches!(Picker::new(1, 4, 0), Err(Error::InvalidConfig(_))));
        assert!(matches!(
            Picker::with_piece_sizes(Vec::new(), 4),
            Err(Error::InvalidConfig(_))
        ));
        assert!(matches!(
            Picker::with_piece_sizes(vec![4, 0], 4),
            Err(Error::InvalidConfig(_))
        ));
        assert!(matches!(
            Picker::with_piece_sizes(vec![4], 0),
            Err(Error::InvalidConfig(_))
        ));
    }

    #[test]
    fn unsolicited_blocks_are_rejected() {
        let mut picker = picker(&[4], 4);
        picker.add_peer_availability(1, [0]).unwrap();
        let error = picker.on_block_received(1, 0, 0, 4).unwrap_err();
        assert!(matches!(error, Error::BlockNotRequested { .. }));
    }

    #[test]
    fn a_peer_cannot_answer_another_peers_request() {
        let mut picker = picker(&[4], 4);
        picker.add_peer_availability(1, [0]).unwrap();
        picker.add_peer_availability(2, [0]).unwrap();
        assert_eq!(drain(&mut picker, 1, 1).len(), 1);

        let error = picker.on_block_received(2, 0, 0, 4).unwrap_err();
        assert!(matches!(error, Error::BlockNotRequested { peer: 2, .. }));
        assert!(picker.on_block_received(1, 0, 0, 4).is_ok());
    }

    #[test]
    fn a_stale_request_is_requeued_without_touching_the_rest() {
        let mut picker = picker(&[8], 4);
        picker.add_peer_availability(1, [0]).unwrap();
        let requests = drain(&mut picker, 1, 2);
        assert_eq!(offsets(&requests), vec![(0, 0), (0, 4)]);

        assert!(picker.requeue_block(1, 0, 0));
        assert!(!picker.requeue_block(1, 0, 0), "second requeue is a no-op");
        let again = drain(&mut picker, 1, 1);
        assert_eq!(offsets(&again), vec![(0, 0)]);
    }
}
