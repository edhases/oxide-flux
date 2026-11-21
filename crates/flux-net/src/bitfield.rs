//! Piece-availability bitmap.
//!
//! BEP 3 defines a bitfield as one bit per piece, most significant bit first:
//! piece 0 is `0x80` of byte 0. The wire form must be exactly
//! `ceil(pieces / 8)` bytes, with every bit past the last piece zero. Peers
//! that set spare bits are rejected rather than silently accepted, because a
//! spare bit is indistinguishable from a real piece index once the piece count
//! is wrong by one.

use std::fmt;

use thiserror::Error;

/// Errors produced when constructing or indexing a [`Bitfield`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BitfieldError {
    /// The wire byte length is not exactly `ceil(pieces / 8)`.
    #[error("bitfield for {pieces} pieces needs exactly {expected} bytes, got {got}")]
    WrongByteLength {
        /// Piece count the bitfield was built for.
        pieces: usize,
        /// Required byte length.
        expected: usize,
        /// Supplied byte length.
        got: usize,
    },

    /// Bits past the last piece must be zero.
    #[error("bitfield for {pieces} pieces has non-zero trailing bits ({mask:#04x})")]
    InvalidTrailingBits {
        /// Piece count the bitfield was built for.
        pieces: usize,
        /// The offending bits, already masked to the spare positions.
        mask: u8,
    },

    /// The requested piece index does not exist in this bitfield.
    #[error("piece index {index} is out of range for {pieces} pieces")]
    IndexOutOfRange {
        /// Requested index.
        index: usize,
        /// Number of pieces the bitfield holds.
        pieces: usize,
    },
}

/// Number of bytes required to hold `pieces` bits.
#[must_use]
pub const fn byte_len(pieces: usize) -> usize {
    pieces.div_ceil(8)
}

/// A fixed-size set of piece indices.
///
/// The spare bits of the final byte are always zero, both after construction
/// and after every mutation, so [`to_bytes`](Bitfield::to_bytes) is always a
/// valid BEP 3 bitfield.
///
/// ```
/// use flux_net::Bitfield;
///
/// let mut have = Bitfield::new(10);
/// have.set(0).expect("index 0 exists");
/// have.set(9).expect("index 9 exists");
/// assert_eq!(have.to_bytes(), &[0x80, 0x40]);
/// assert_eq!(have.count(), 2);
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Bitfield {
    bits: Vec<u8>,
    pieces: usize,
    count: usize,
}

impl Bitfield {
    /// Creates a bitfield for `pieces` pieces with every bit clear.
    #[must_use]
    pub fn new(pieces: usize) -> Self {
        Self {
            bits: vec![0; byte_len(pieces)],
            pieces,
            count: 0,
        }
    }

    /// Creates a bitfield for `pieces` pieces with every bit set.
    #[must_use]
    pub fn all(pieces: usize) -> Self {
        let mut field = Self {
            bits: vec![0xFF; byte_len(pieces)],
            pieces,
            count: pieces,
        };
        field.mask_tail();
        field
    }

    /// Parses the wire form, validating byte length and trailing bits.
    ///
    /// ```
    /// use flux_net::{Bitfield, BitfieldError};
    ///
    /// // 10 pieces: the two spare bits of byte 1 must be clear.
    /// assert!(Bitfield::from_bytes(10, &[0x81, 0xC0]).is_ok());
    /// assert_eq!(
    ///     Bitfield::from_bytes(10, &[0x81, 0xC1]),
    ///     Err(BitfieldError::InvalidTrailingBits {
    ///         pieces: 10,
    ///         mask: 0x01,
    ///     }),
    /// );
    /// ```
    pub fn from_bytes(pieces: usize, bytes: &[u8]) -> Result<Self, BitfieldError> {
        let expected = byte_len(pieces);
        if bytes.len() != expected {
            return Err(BitfieldError::WrongByteLength {
                pieces,
                expected,
                got: bytes.len(),
            });
        }

        let used = pieces % 8;
        if used != 0 {
            let spare_mask = 0xFFu8 >> used;
            if let Some(&tail) = bytes.last()
                && tail & spare_mask != 0
            {
                return Err(BitfieldError::InvalidTrailingBits {
                    pieces,
                    mask: tail & spare_mask,
                });
            }
        }

        let count = bytes.iter().map(|byte| byte.count_ones() as usize).sum();
        Ok(Self {
            bits: bytes.to_vec(),
            pieces,
            count,
        })
    }

    /// The wire form: exactly `ceil(pieces / 8)` bytes with spare bits clear.
    #[must_use]
    pub fn to_bytes(&self) -> &[u8] {
        &self.bits
    }

    /// Consumes the bitfield and returns its wire form.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bits
    }

    /// Number of pieces this bitfield describes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pieces
    }

    /// Whether the bitfield describes zero pieces.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pieces == 0
    }

    /// Number of set bits, i.e. pieces this peer claims to have.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }

    /// Whether every piece is set (true for a zero-piece bitfield).
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.count == self.pieces
    }

    /// Tests one piece index.
    pub fn get(&self, index: usize) -> Result<bool, BitfieldError> {
        self.locate(index)?;
        Ok(self.test(index))
    }

    /// Sets one piece index. Setting an already-set index is a no-op.
    pub fn set(&mut self, index: usize) -> Result<(), BitfieldError> {
        let (byte, mask) = self.locate(index)?;
        if let Some(slot) = self.bits.get_mut(byte)
            && *slot & mask == 0
        {
            *slot |= mask;
            self.count += 1;
        }
        Ok(())
    }

    /// Clears one piece index. Clearing an already-clear index is a no-op.
    pub fn clear(&mut self, index: usize) -> Result<(), BitfieldError> {
        let (byte, mask) = self.locate(index)?;
        if let Some(slot) = self.bits.get_mut(byte)
            && *slot & mask != 0
        {
            *slot &= !mask;
            self.count -= 1;
        }
        Ok(())
    }

    /// Iterates the set piece indices in ascending order.
    pub fn iter_set(&self) -> SetBits<'_> {
        SetBits {
            field: self,
            next: 0,
        }
    }

    /// Splits `index` into a byte offset and bit mask (MSB first).
    fn locate(&self, index: usize) -> Result<(usize, u8), BitfieldError> {
        if index >= self.pieces {
            return Err(BitfieldError::IndexOutOfRange {
                index,
                pieces: self.pieces,
            });
        }
        Ok((index / 8, 1u8 << (7 - index % 8)))
    }

    /// Bit test without the bounds check; callers must already have validated.
    fn test(&self, index: usize) -> bool {
        let byte = index / 8;
        let mask = 1u8 << (7 - index % 8);
        self.bits.get(byte).is_some_and(|slot| slot & mask != 0)
    }

    /// Clears the spare bits of the final byte after a bulk fill.
    fn mask_tail(&mut self) {
        let used = self.pieces % 8;
        if used == 0 {
            return;
        }
        if let Some(tail) = self.bits.last_mut() {
            *tail &= !(0xFFu8 >> used);
        }
    }
}

impl fmt::Debug for Bitfield {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bitfield")
            .field("pieces", &self.pieces)
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Bitfield {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{} pieces", self.count, self.pieces)
    }
}

/// Iterator over the set indices of a [`Bitfield`].
#[derive(Debug, Clone)]
pub struct SetBits<'a> {
    field: &'a Bitfield,
    next: usize,
}

impl Iterator for SetBits<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while self.next < self.field.pieces {
            let index = self.next;
            self.next += 1;
            if self.field.test(index) {
                return Some(index);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.field.pieces - self.next))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn byte_len_rounds_up() {
        assert_eq!(byte_len(0), 0);
        assert_eq!(byte_len(1), 1);
        assert_eq!(byte_len(7), 1);
        assert_eq!(byte_len(8), 1);
        assert_eq!(byte_len(9), 2);
        assert_eq!(byte_len(1024), 128);
    }

    #[test]
    fn zero_pieces() {
        let mut field = Bitfield::new(0);
        assert_eq!(field.len(), 0);
        assert!(field.is_empty());
        assert_eq!(field.count(), 0);
        assert_eq!(field.to_bytes(), &[] as &[u8]);
        assert_eq!(field.iter_set().count(), 0);
        assert!(field.get(0).is_err());
        assert!(field.set(0).is_err());
        assert!(field.clear(0).is_err());

        assert!(Bitfield::from_bytes(0, &[]).is_ok());
        // One byte of "nothing" is still the wrong length for zero pieces.
        assert_eq!(
            Bitfield::from_bytes(0, &[0x00]),
            Err(BitfieldError::WrongByteLength {
                pieces: 0,
                expected: 0,
                got: 1,
            })
        );
    }

    #[test]
    fn one_piece() {
        let mut field = Bitfield::new(1);
        assert_eq!(field.to_bytes(), &[0x00]);
        assert!(!field.is_full());
        field.set(0).unwrap();
        assert_eq!(field.to_bytes(), &[0x80]);
        assert_eq!(field.count(), 1);
        assert!(field.is_full());
        assert_eq!(field.iter_set().collect::<Vec<_>>(), vec![0]);
        field.clear(0).unwrap();
        assert_eq!(field.to_bytes(), &[0x00]);
        assert_eq!(field.count(), 0);
    }

    #[test]
    fn non_multiple_of_eight_positions_are_msb_first() {
        let mut field = Bitfield::new(10);
        for index in [0, 7, 8, 9] {
            field.set(index).unwrap();
        }
        // byte 0: bit 7 and bit 0 -> 0x81; byte 1: bits 7 and 6 -> 0xC0.
        assert_eq!(field.to_bytes(), &[0x81, 0xC0]);
        assert_eq!(field.iter_set().collect::<Vec<_>>(), vec![0, 7, 8, 9]);

        field.set(8).unwrap(); // idempotent
        assert_eq!(field.count(), 4);
        field.clear(8).unwrap();
        field.clear(8).unwrap();
        assert_eq!(field.count(), 3);
        assert_eq!(field.to_bytes(), &[0x81, 0x40]);
    }

    #[test]
    fn from_bytes_round_trips() {
        let bytes = [0xDE, 0xAD, 0xBE, 0xEF, 0xF0];
        let field = Bitfield::from_bytes(37, &bytes).unwrap();
        assert_eq!(field.count(), 6 + 5 + 6 + 7 + 4);
        assert_eq!(field.to_bytes(), &bytes);
        assert_eq!(field.iter_set().count(), field.count());
    }

    #[test]
    fn bad_trailing_bits_rejected() {
        // 10 pieces leaves 6 spare bits: 0x3F is the whole spare region.
        assert_eq!(
            Bitfield::from_bytes(10, &[0xFF, 0xFF]),
            Err(BitfieldError::InvalidTrailingBits {
                pieces: 10,
                mask: 0x3F,
            })
        );
        // 13 pieces leaves 3 spare bits: only 0xF8 is a valid final byte.
        assert!(Bitfield::from_bytes(13, &[0xFF, 0xF8]).is_ok());
        assert_eq!(
            Bitfield::from_bytes(13, &[0xFF, 0xF9]),
            Err(BitfieldError::InvalidTrailingBits {
                pieces: 13,
                mask: 0x01,
            })
        );
        assert_eq!(
            Bitfield::from_bytes(13, &[0x00, 0x07]),
            Err(BitfieldError::InvalidTrailingBits {
                pieces: 13,
                mask: 0x07,
            })
        );
    }

    #[test]
    fn wrong_length_rejected() {
        assert_eq!(
            Bitfield::from_bytes(10, &[0x00]),
            Err(BitfieldError::WrongByteLength {
                pieces: 10,
                expected: 2,
                got: 1,
            })
        );
        assert_eq!(
            Bitfield::from_bytes(10, &[0x00, 0x00, 0x00]),
            Err(BitfieldError::WrongByteLength {
                pieces: 10,
                expected: 2,
                got: 3,
            })
        );
        assert!(Bitfield::from_bytes(8, &[]).is_err());
    }

    #[test]
    fn all_masks_the_tail() {
        let field = Bitfield::all(13);
        assert_eq!(field.to_bytes(), &[0xFF, 0xF8]);
        assert_eq!(field.count(), 13);
        assert!(field.is_full());
        assert_eq!(field.iter_set().count(), 13);

        let empty = Bitfield::all(0);
        assert_eq!(empty.to_bytes(), &[] as &[u8]);
        assert!(empty.is_full());
    }

    #[test]
    fn index_bounds() {
        let mut field = Bitfield::new(8);
        assert!(field.set(8).is_err());
        assert!(field.get(8).is_err());
        assert!(field.clear(usize::MAX).is_err());
        assert_eq!(
            field.set(8),
            Err(BitfieldError::IndexOutOfRange {
                index: 8,
                pieces: 8,
            })
        );
        assert!(field.set(7).is_ok());
    }

    #[test]
    fn into_bytes_preserves_wire_form() {
        let field = Bitfield::all(9);
        assert_eq!(field.clone().into_bytes(), vec![0xFF, 0x80]);
        assert_eq!(field.to_bytes(), &[0xFF, 0x80]);
    }
}
