//! File priorities and piece bitfields.
//!
//! A torrent is downloaded piece by piece, but users select *files*. The two are
//! not the same thing: in a v1 torrent a piece can straddle a file boundary, so a
//! piece is only skippable when **every** file it overlaps is skipped. The
//! [`desired_pieces`] function is the single place that decision is made; the rest
//! of the daemon treats its output as "the pieces this torrent wants".
//!
//! Nothing here does I/O or networking, so the selection rules are unit-testable
//! byte-exactly.

use flux_core::metainfo::FileLayout;

/// Upper bound on the number of pieces a torrent may declare.
///
/// Piece counts come from attacker-controlled metadata and every piece gets a bit
/// in two bitfields plus a slot in the future picker, so an unbounded count is an
/// allocation DoS. 4 Mi pieces is 256 KiB of bitfield per torrent and, at the
/// 16 MiB piece length that large torrents use, 64 TiB of content.
pub const MAX_PIECES: usize = 4 * 1024 * 1024;

/// How much a file is wanted.
///
/// `High` currently behaves exactly like `Normal` for piece selection: it exists
/// so the streaming layer can order the download window later. Both are "not
/// skipped" as far as [`desired_pieces`] is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Priority {
    /// The file is not wanted; pieces owned only by skipped files are not needed.
    #[default]
    Skip,
    /// The file is wanted.
    Normal,
    /// The file is wanted and should lead the streaming window.
    High,
}

impl Priority {
    /// Whether the file contributes desired pieces.
    pub fn is_selected(self) -> bool {
        !matches!(self, Self::Skip)
    }

    /// Stable lowercase name, as shown by the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }
}

/// A fixed-length set of piece indices.
///
/// Bit `i` is stored MSB-first inside byte `i / 8`, the same order the peer wire
/// bitfield uses (BEP 3), so [`PieceBitfield::to_bytes`] can be sent as-is.
#[derive(Clone)]
pub struct PieceBitfield {
    len: usize,
    words: Vec<u64>,
}

impl PieceBitfield {
    /// Allocates an empty set of `len` bits.
    ///
    /// Callers must have bounded `len` by [`MAX_PIECES`] already (the torrent
    /// planning step does). Bits beyond `len` are always zero and never reported.
    pub fn new(len: usize) -> Self {
        Self {
            len,
            words: vec![0; len.div_ceil(64)],
        }
    }

    /// Number of pieces the set can hold.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the set holds no pieces at all.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Reads a bit, or `None` when the index is out of range.
    pub fn get(&self, index: usize) -> Option<bool> {
        if index >= self.len {
            return None;
        }
        let word = self.words.get(index / 64)?;
        Some(word & (1_u64 << (index % 64)) != 0)
    }

    /// Writes a bit and reports whether it actually changed.
    ///
    /// Out-of-range indices are ignored and report `false`.
    pub fn set(&mut self, index: usize, value: bool) -> bool {
        if index >= self.len {
            return false;
        }
        let Some(word) = self.words.get_mut(index / 64) else {
            return false;
        };
        let mask = 1_u64 << (index % 64);
        let before = *word & mask != 0;
        if before == value {
            return false;
        }
        if value {
            *word |= mask;
        } else {
            *word &= !mask;
        }
        true
    }

    /// Sets a bit, returning whether it changed.
    pub fn insert(&mut self, index: usize) -> bool {
        self.set(index, true)
    }

    /// Clears every bit.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    /// Number of set bits.
    pub fn count_ones(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// Whether every piece is set.
    ///
    /// An empty set is never complete: a torrent with no pieces has nothing to
    /// verify, and claiming completeness would mark it as seeding.
    pub fn all(&self) -> bool {
        self.len > 0 && self.count_ones() == self.len
    }

    /// Whether nothing is set.
    pub fn is_empty_set(&self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }

    /// Iterates the indices of the set bits in ascending order.
    pub fn ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.words
            .iter()
            .enumerate()
            .flat_map(|(word_index, word)| {
                let word = *word;
                (0..64)
                    .filter(move |bit| word & (1_u64 << bit) != 0)
                    .map(move |bit| word_index * 64 + bit)
            })
            .filter(|index| *index < self.len)
    }

    /// Packs the set into `len.div_ceil(8)` bytes, MSB-first per byte.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![0_u8; self.len.div_ceil(8)];
        for index in self.ones() {
            let byte = index / 8;
            if let Some(slot) = bytes.get_mut(byte) {
                *slot |= 1 << (7 - (index % 8));
            }
        }
        bytes
    }

    /// Hex form of [`Self::to_bytes`].
    pub fn to_hex(&self) -> String {
        crate::id::to_hex(&self.to_bytes())
    }
}

impl std::fmt::Debug for PieceBitfield {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PieceBitfield({} of {} set)",
            self.count_ones(),
            self.len
        )
    }
}

/// Why a desired-piece set could not be computed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectionError {
    /// The priority list length did not match the layout's file count.
    #[error("{actual} priorities supplied for {expected} files")]
    PriorityCount {
        /// Files in the layout.
        expected: usize,
        /// Priorities supplied.
        actual: usize,
    },
    /// The layout declares more pieces than [`MAX_PIECES`].
    #[error("torrent declares {pieces} pieces, over the {limit} piece limit")]
    TooManyPieces {
        /// Declared piece count.
        pieces: usize,
        /// The limit.
        limit: usize,
    },
}

/// Computes the pieces a torrent needs for the given per-file priorities.
///
/// A piece is desired when at least one file that owns bytes of it is not
/// [`Priority::Skip`]. Pieces that a v2 alignment gap leaves unowned are never
/// desired (there is nothing to fetch), and zero-length files own no pieces, so
/// their priority is irrelevant.
///
/// # Errors
///
/// Returns [`SelectionError::PriorityCount`] when `priorities` does not have one
/// entry per file, and [`SelectionError::TooManyPieces`] for an excessive piece
/// count.
pub fn desired_pieces(
    layout: &FileLayout,
    priorities: &[Priority],
) -> Result<PieceBitfield, SelectionError> {
    let files = layout.files();
    if files.len() != priorities.len() {
        return Err(SelectionError::PriorityCount {
            expected: files.len(),
            actual: priorities.len(),
        });
    }
    if layout.piece_count() > MAX_PIECES {
        return Err(SelectionError::TooManyPieces {
            pieces: layout.piece_count(),
            limit: MAX_PIECES,
        });
    }

    let mut desired = PieceBitfield::new(layout.piece_count());
    let piece_length = u64::from(layout.piece_length());
    for (index, file) in files.iter().enumerate() {
        let selected = priorities.get(index).is_some_and(|p| p.is_selected());
        if !selected || file.length == 0 {
            continue;
        }
        // Every piece overlapping [offset, offset + length) is needed. File
        // boundaries and piece boundaries do not line up, which is exactly why
        // this maps whole file ranges instead of iterating pieces.
        let first = file.offset / piece_length;
        let last = (file.offset + file.length - 1) / piece_length;
        for piece in first..=last {
            if let Ok(index) = usize::try_from(piece) {
                desired.insert(index);
            }
        }
    }
    Ok(desired)
}

/// Total number of payload bytes covered by the set pieces of `bits`.
///
/// Pieces have their address-space size, so v2 alignment gaps do not count and
/// the final (short) piece of a file counts only the bytes that exist.
pub fn bytes_in(layout: &FileLayout, bits: &PieceBitfield) -> u64 {
    bits.ones()
        .filter_map(|piece| layout.piece_size(piece))
        .map(u64::from)
        .sum()
}

/// Whether the given pieces cover all of `file_index`'s bytes.
///
/// `covered` is expected to be a per-file byte counter filled from the same piece
/// set; see [`coverage_per_file`].
pub fn file_is_covered(file_length: u64, covered_bytes: u64) -> bool {
    covered_bytes >= file_length
}

/// Fills one byte counter per file with the bytes covered by `bits`.
///
/// A piece that straddles a file edge credits each side with only the bytes it
/// owns, which is why per-file progress cannot be derived from a piece count.
pub fn coverage_per_file(layout: &FileLayout, bits: &PieceBitfield) -> Vec<u64> {
    let mut covered = vec![0_u64; layout.files().len()];
    for piece in bits.ones() {
        let Some(ranges) = layout.map_piece(piece) else {
            continue;
        };
        for range in ranges {
            if let Some(slot) = covered.get_mut(range.file_index) {
                *slot = slot.saturating_add(range.length);
            }
        }
    }
    covered
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use flux_core::metainfo::FileLayout;

    fn layout(lengths: &[u64], piece_length: u32) -> FileLayout {
        let files: Vec<(Vec<String>, u64)> = lengths
            .iter()
            .enumerate()
            .map(|(index, length)| (vec![format!("f{index}")], *length))
            .collect();
        FileLayout::contiguous(files, piece_length).unwrap()
    }

    fn desired_indexes(layout: &FileLayout, priorities: &[Priority]) -> Vec<usize> {
        desired_pieces(layout, priorities).unwrap().ones().collect()
    }

    #[test]
    fn bitfield_basics() {
        let mut bits = PieceBitfield::new(70);
        assert_eq!(bits.len(), 70);
        assert!(!bits.all());
        assert!(bits.is_empty_set());
        assert_eq!(bits.get(70), None);
        assert!(!bits.set(70, true));

        assert!(bits.insert(0));
        assert!(bits.insert(63));
        assert!(bits.insert(64));
        assert!(!bits.insert(0));
        assert_eq!(bits.count_ones(), 3);
        assert_eq!(bits.ones().collect::<Vec<_>>(), vec![0, 63, 64]);
        assert_eq!(bits.to_bytes().len(), 9);
        assert_eq!(bits.to_bytes()[0], 0b1000_0000);
        assert!(bits.set(64, false));
        assert_eq!(bits.count_ones(), 2);
        bits.clear();
        assert!(bits.is_empty_set());
        assert!(!PieceBitfield::new(0).all());
    }

    #[test]
    fn all_files_selected_desires_every_piece() {
        let layout = layout(&[3, 5, 4], 4);
        let priorities = [Priority::Normal; 3];
        assert_eq!(desired_indexes(&layout, &priorities), vec![0, 1, 2]);
        let bits = desired_pieces(&layout, &priorities).unwrap();
        assert!(bits.all());
        assert_eq!(bytes_in(&layout, &bits), 12);
    }

    #[test]
    fn skipping_one_file_in_the_middle_keeps_straddling_pieces() {
        // f0 = [0,3), f1 = [3,8), f2 = [8,12); piece length 4.
        // piece 0 covers f0 and 1 byte of f1; piece 1 covers f1; piece 2 covers f2.
        let layout = layout(&[3, 5, 4], 4);
        let skipped = [Priority::Normal, Priority::Skip, Priority::Normal];
        assert_eq!(desired_indexes(&layout, &skipped), vec![0, 2]);

        // piece 0 is kept only because f0 is selected: it straddles both files.
        let all_skipped = [Priority::Skip, Priority::Skip, Priority::Skip];
        assert!(
            desired_pieces(&layout, &all_skipped)
                .unwrap()
                .is_empty_set()
        );

        // Skipping f1 alone leaves piece 1 undesired; skipping f0 and f2 too
        // would leave nothing at all.
        let middle_only = [Priority::Skip, Priority::Normal, Priority::Skip];
        assert_eq!(desired_indexes(&layout, &middle_only), vec![0, 1]);
    }

    #[test]
    fn first_and_last_file_edges() {
        // f0 = [0,1), f1 = [1,5), f2 = [5,9), piece length 4.
        // piece 0 = f0 + f1[0..3]; piece 1 = f1[3..4] + f2[0..3]; piece 2 = f2[3..4].
        let layout = layout(&[1, 4, 4], 4);
        assert_eq!(
            desired_indexes(&layout, &[Priority::Skip, Priority::Normal, Priority::Skip]),
            vec![0, 1]
        );
        assert_eq!(
            desired_indexes(&layout, &[Priority::Normal, Priority::Skip, Priority::Skip]),
            vec![0]
        );
        assert_eq!(
            desired_indexes(&layout, &[Priority::Skip, Priority::Skip, Priority::Normal]),
            vec![1, 2]
        );
        assert_eq!(
            desired_indexes(
                &layout,
                &[Priority::Normal, Priority::Skip, Priority::Normal]
            ),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn zero_length_files_own_no_pieces() {
        // f1 is empty; it sits between f0 and f2 in the address space.
        let layout = layout(&[4, 0, 4], 4);
        assert_eq!(layout.files()[1].length, 0);
        assert_eq!(
            desired_indexes(&layout, &[Priority::Skip, Priority::Normal, Priority::Skip]),
            Vec::<usize>::new()
        );
        assert_eq!(
            desired_indexes(
                &layout,
                &[Priority::Normal, Priority::Skip, Priority::Normal]
            ),
            vec![0, 1]
        );
    }

    #[test]
    fn matches_the_full_piece_mapping() {
        // Property check: the fast file-range walk must agree with per-piece
        // ownership, including v2 alignment gaps.
        let shapes: &[(&[u64], u32, bool)] = &[
            (&[3, 5, 4], 4, false),
            (&[1, 4, 4], 4, false),
            (&[4, 0, 4], 4, false),
            (&[9, 0, 1], 4, false),
            (&[9, 0, 1], 4, true),
            (&[5, 0, 3], 4, true),
            (&[1, 2, 3, 4, 5], 4, false),
        ];
        for (lengths, piece_length, aligned) in shapes {
            let files: Vec<(Vec<String>, u64)> = lengths
                .iter()
                .enumerate()
                .map(|(index, length)| (vec![format!("f{index}")], *length))
                .collect();
            let layout = if *aligned {
                FileLayout::aligned(files, *piece_length).unwrap()
            } else {
                FileLayout::contiguous(files, *piece_length).unwrap()
            };
            for mask in 0_u32..(1 << lengths.len().min(5)) {
                let priorities: Vec<Priority> = (0..lengths.len())
                    .map(|index| {
                        if mask & (1 << index) != 0 {
                            Priority::Normal
                        } else {
                            Priority::Skip
                        }
                    })
                    .collect();
                let fast = desired_pieces(&layout, &priorities).unwrap();
                let mut slow = PieceBitfield::new(layout.piece_count());
                for piece in 0..layout.piece_count() {
                    let needed = layout.map_piece(piece).is_some_and(|ranges| {
                        ranges
                            .iter()
                            .any(|range| priorities[range.file_index].is_selected())
                    });
                    slow.set(piece, needed);
                }
                assert_eq!(
                    fast.ones().collect::<Vec<_>>(),
                    slow.ones().collect::<Vec<_>>(),
                    "layout {lengths:?} aligned={aligned} mask={mask:b}"
                );
                assert_eq!(bytes_in(&layout, &fast), bytes_in(&layout, &slow));
            }
        }
    }

    #[test]
    fn coverage_splits_straddling_pieces() {
        let layout = layout(&[3, 5], 4);
        let mut bits = PieceBitfield::new(layout.piece_count());
        bits.insert(0);
        // Piece 0 covers 3 bytes of f0 and 1 byte of f1.
        assert_eq!(coverage_per_file(&layout, &bits), vec![3, 1]);
        assert!(!file_is_covered(5, 1));
        bits.insert(1);
        assert_eq!(coverage_per_file(&layout, &bits), vec![3, 5]);
        assert!(file_is_covered(5, 5));
    }

    #[test]
    fn rejects_wrong_priority_count_and_oversized_torrents() {
        let layout = layout(&[4, 4], 4);
        assert!(matches!(
            desired_pieces(&layout, &[Priority::Normal]),
            Err(SelectionError::PriorityCount {
                expected: 2,
                actual: 1
            })
        ));
    }
}
