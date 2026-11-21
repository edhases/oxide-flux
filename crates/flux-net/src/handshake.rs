//! The BitTorrent handshake (BEP 3) and the reserved-bit helpers both peers
//! rely on.
//!
//! Layout: `1` byte protocol length, the 19-byte protocol string, an 8-byte
//! reserved field, the 20-byte info hash, and the 20-byte peer id — 68 bytes in
//! total.
//!
//! Reserved bits follow the BEP convention of numbering from the least
//! significant bit of the 8-byte field, so bit 0 is `reserved[7] & 0x01` and
//! bit 20 is `reserved[5] & 0x10`. Implementations routinely get that mapping
//! wrong, so the constants here are asserted against the exact byte offsets in
//! the tests.

use thiserror::Error;

/// The protocol string's length byte: 19.
pub const PSTR_LEN: u8 = 19;
/// The protocol string: `BitTorrent protocol`.
pub const PSTR: &[u8] = b"BitTorrent protocol";
/// Reserved field length in bytes.
pub const RESERVED_LEN: usize = 8;
/// Info hash length in bytes.
pub const INFO_HASH_LEN: usize = 20;
/// Peer id length in bytes.
pub const PEER_ID_LEN: usize = 20;
/// Total encoded handshake length: 68 bytes.
pub const HANDSHAKE_LEN: usize = 1 + PSTR.len() + RESERVED_LEN + INFO_HASH_LEN + PEER_ID_LEN;

/// A v1 (SHA-1) info hash.
pub type InfoHash = [u8; INFO_HASH_LEN];
/// A peer id.
pub type PeerId = [u8; PEER_ID_LEN];

/// One bit of the reserved field.
///
/// The fields are the byte offset inside the reserved field and the mask within
/// that byte, which is exactly what the wire layout needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedBit {
    /// Byte offset within the 8-byte reserved field.
    pub byte: usize,
    /// Bit mask inside that byte.
    pub mask: u8,
}

impl ReservedBit {
    /// Builds a [`ReservedBit`] from a bit number counted from the right of the
    /// 8-byte reserved field.
    ///
    /// # Panics
    ///
    /// Panics if `bit >= 64`. This helper is only meant for compile-time
    /// constants; nothing derived from the wire is ever passed to it.
    #[must_use]
    pub const fn from_bit(bit: u8) -> Self {
        assert!(bit < 64, "reserved bit index must be below 64");
        Self {
            byte: 7 - (bit / 8) as usize,
            mask: 1u8 << (bit % 8),
        }
    }
}

/// DHT support (BEP 5): reserved bit 0, i.e. `reserved[7] & 0x01`.
pub const RESERVED_DHT: ReservedBit = ReservedBit::from_bit(0);
/// Fast extension (BEP 6): reserved bit 2, i.e. `reserved[7] & 0x04`.
pub const RESERVED_FAST: ReservedBit = ReservedBit::from_bit(2);
/// Extension protocol (BEP 10): reserved bit 20, i.e. `reserved[5] & 0x10`.
pub const RESERVED_EXTENDED: ReservedBit = ReservedBit::from_bit(20);

/// A decoded handshake — the first 68 bytes a peer sends, and the first 68
/// bytes we send back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    reserved: [u8; RESERVED_LEN],
    info_hash: InfoHash,
    peer_id: PeerId,
}

impl Handshake {
    /// Builds a handshake with a zeroed reserved field.
    #[must_use]
    pub fn new(info_hash: InfoHash, peer_id: PeerId) -> Self {
        Self {
            reserved: [0; RESERVED_LEN],
            info_hash,
            peer_id,
        }
    }

    /// Builds a handshake with an explicit reserved field.
    #[must_use]
    pub fn with_reserved(
        info_hash: InfoHash,
        peer_id: PeerId,
        reserved: [u8; RESERVED_LEN],
    ) -> Self {
        Self {
            reserved,
            info_hash,
            peer_id,
        }
    }

    /// The raw reserved field.
    #[must_use]
    pub fn reserved(&self) -> [u8; RESERVED_LEN] {
        self.reserved
    }

    /// One byte of the reserved field, or `None` for an out-of-range offset.
    #[must_use]
    pub fn reserved_byte(&self, index: usize) -> Option<u8> {
        self.reserved.get(index).copied()
    }

    /// Whether `bit` is set.
    #[must_use]
    pub fn supports(&self, bit: ReservedBit) -> bool {
        self.reserved
            .get(bit.byte)
            .is_some_and(|byte| byte & bit.mask != 0)
    }

    /// Sets or clears `bit`, ignoring an out-of-range byte offset.
    pub fn set_reserved(&mut self, bit: ReservedBit, enabled: bool) {
        if let Some(byte) = self.reserved.get_mut(bit.byte) {
            if enabled {
                *byte |= bit.mask;
            } else {
                *byte &= !bit.mask;
            }
        }
    }

    /// Whether this peer advertised DHT support (BEP 5).
    #[must_use]
    pub fn supports_dht(&self) -> bool {
        self.supports(RESERVED_DHT)
    }

    /// Advertises or withdraws DHT support (BEP 5).
    pub fn set_dht(&mut self, enabled: bool) {
        self.set_reserved(RESERVED_DHT, enabled);
    }

    /// Whether this peer advertised the extension protocol (BEP 10).
    #[must_use]
    pub fn supports_extended(&self) -> bool {
        self.supports(RESERVED_EXTENDED)
    }

    /// Advertises or withdraws the extension protocol (BEP 10).
    pub fn set_extended(&mut self, enabled: bool) {
        self.set_reserved(RESERVED_EXTENDED, enabled);
    }

    /// Whether this peer advertised the fast extension (BEP 6).
    #[must_use]
    pub fn supports_fast(&self) -> bool {
        self.supports(RESERVED_FAST)
    }

    /// Advertises or withdraws the fast extension (BEP 6).
    pub fn set_fast(&mut self, enabled: bool) {
        self.set_reserved(RESERVED_FAST, enabled);
    }

    /// The info hash the peer claims to serve.
    #[must_use]
    pub fn info_hash(&self) -> &InfoHash {
        &self.info_hash
    }

    /// The peer id the peer claims to use.
    #[must_use]
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    /// Encodes the handshake into its fixed 68-byte wire form.
    #[must_use]
    pub fn encode(&self) -> [u8; HANDSHAKE_LEN] {
        let mut out = [0u8; HANDSHAKE_LEN];
        out[0] = PSTR_LEN;
        out[1..1 + PSTR.len()].copy_from_slice(PSTR);
        out[1 + PSTR.len()..1 + PSTR.len() + RESERVED_LEN].copy_from_slice(&self.reserved);
        out[1 + PSTR.len() + RESERVED_LEN..1 + PSTR.len() + RESERVED_LEN + INFO_HASH_LEN]
            .copy_from_slice(&self.info_hash);
        out[HANDSHAKE_LEN - PEER_ID_LEN..].copy_from_slice(&self.peer_id);
        out
    }

    /// Appends the handshake to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.encode());
    }

    /// Decodes the leading 68 bytes of `input`.
    ///
    /// A longer buffer is accepted, so a peer's read buffer can be passed
    /// directly; anything after the handshake is left to the message decoder.
    pub fn decode(input: &[u8]) -> Result<Self, HandshakeError> {
        let Some(&pstr_len) = input.first() else {
            return Err(HandshakeError::Truncated {
                got: 0,
                need: HANDSHAKE_LEN,
            });
        };
        if pstr_len != PSTR_LEN {
            return Err(HandshakeError::WrongPstrLen { got: pstr_len });
        }
        if input.len() < HANDSHAKE_LEN {
            return Err(HandshakeError::Truncated {
                got: input.len(),
                need: HANDSHAKE_LEN,
            });
        }
        if input.get(1..1 + PSTR.len()) != Some(PSTR) {
            return Err(HandshakeError::WrongPstr);
        }

        Ok(Self {
            reserved: array_at(input, 1 + PSTR.len())?,
            info_hash: array_at(input, 1 + PSTR.len() + RESERVED_LEN)?,
            peer_id: array_at(input, 1 + PSTR.len() + RESERVED_LEN + INFO_HASH_LEN)?,
        })
    }

    /// Decodes and rejects a handshake for a different torrent.
    ///
    /// The info hash is the only thing tying a peer to a torrent, so this is
    /// the form the accept path should use.
    pub fn decode_expecting(input: &[u8], expected: InfoHash) -> Result<Self, HandshakeError> {
        let handshake = Self::decode(input)?;
        handshake.verify_info_hash(expected)?;
        Ok(handshake)
    }

    /// Checks the decoded info hash against the torrent we asked for.
    pub fn verify_info_hash(&self, expected: InfoHash) -> Result<(), HandshakeError> {
        if self.info_hash != expected {
            return Err(HandshakeError::InfoHashMismatch {
                expected,
                got: self.info_hash,
            });
        }
        Ok(())
    }
}

/// Copies `N` bytes starting at `offset`, or reports truncation.
fn array_at<const N: usize>(input: &[u8], offset: usize) -> Result<[u8; N], HandshakeError> {
    input
        .get(offset..offset + N)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(HandshakeError::Truncated {
            got: input.len(),
            need: offset + N,
        })
}

/// Handshake failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HandshakeError {
    /// The input ended before a whole handshake arrived.
    #[error("truncated handshake: got {got} bytes, need {need}")]
    Truncated {
        /// Bytes available.
        got: usize,
        /// Bytes required.
        need: usize,
    },

    /// The protocol string length byte was not 19.
    #[error("bad protocol length {got}, expected 19")]
    WrongPstrLen {
        /// The length byte that was read.
        got: u8,
    },

    /// The protocol string was not `BitTorrent protocol`.
    #[error("bad protocol string, expected `BitTorrent protocol`")]
    WrongPstr,

    /// The peer answered for a different torrent.
    #[error("info hash mismatch")]
    InfoHashMismatch {
        /// Hash of the torrent we are downloading.
        expected: InfoHash,
        /// Hash the peer advertised.
        got: InfoHash,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn sample() -> Handshake {
        Handshake::new([0x11; INFO_HASH_LEN], [0x22; PEER_ID_LEN])
    }

    #[test]
    fn round_trip() {
        let handshake = sample();
        let bytes = handshake.encode();
        assert_eq!(bytes.len(), 68);
        let decoded = Handshake::decode(&bytes).unwrap();
        assert_eq!(decoded, handshake);
        assert_eq!(decoded.info_hash(), &[0x11; 20]);
        assert_eq!(decoded.peer_id(), &[0x22; 20]);
    }

    #[test]
    fn exact_wire_layout() {
        let handshake = sample();
        let bytes = handshake.encode();
        assert_eq!(bytes[0], 19);
        assert_eq!(&bytes[1..20], b"BitTorrent protocol");
        assert_eq!(&bytes[20..28], &[0u8; 8]);
        assert_eq!(&bytes[28..48], &[0x11u8; 20]);
        assert_eq!(&bytes[48..68], &[0x22u8; 20]);
    }

    #[test]
    fn decode_accepts_a_stream_buffer() {
        let bytes = sample().encode();
        let mut buffer = bytes.to_vec();
        buffer.extend_from_slice(&[0, 0, 0, 1, 2]); // first message follows
        let decoded = Handshake::decode(&buffer).unwrap();
        assert_eq!(decoded, sample());
    }

    #[test]
    fn truncated_input_rejected() {
        let bytes = sample().encode();
        for len in [0usize, 1, 19, 20, 27, 28, 47, 48, 67] {
            assert_eq!(
                Handshake::decode(&bytes[..len]),
                Err(HandshakeError::Truncated {
                    got: len,
                    need: HANDSHAKE_LEN,
                }),
                "len {len}"
            );
        }
    }

    #[test]
    fn wrong_pstrlen_rejected() {
        let mut bytes = sample().encode();
        bytes[0] = 18;
        assert_eq!(
            Handshake::decode(&bytes),
            Err(HandshakeError::WrongPstrLen { got: 18 })
        );

        bytes[0] = 0;
        assert_eq!(
            Handshake::decode(&bytes),
            Err(HandshakeError::WrongPstrLen { got: 0 })
        );
        // A zero byte is a message prefix, not a handshake.
        assert!(Handshake::decode(&[0, 0, 0, 0]).is_err());
    }

    #[test]
    fn wrong_pstr_rejected() {
        let mut bytes = sample().encode();
        bytes[1] = b'b';
        assert_eq!(Handshake::decode(&bytes), Err(HandshakeError::WrongPstr));
    }

    #[test]
    fn info_hash_mismatch_rejected() {
        let bytes = sample().encode();
        assert_eq!(
            Handshake::decode_expecting(&bytes, [0xFF; 20]),
            Err(HandshakeError::InfoHashMismatch {
                expected: [0xFF; 20],
                got: [0x11; 20],
            })
        );
        assert!(Handshake::decode_expecting(&bytes, [0x11; 20]).is_ok());

        let handshake = sample();
        assert!(handshake.verify_info_hash([0x11; 20]).is_ok());
        assert!(handshake.verify_info_hash([0x12; 20]).is_err());
    }

    #[test]
    fn reserved_bit_positions_match_the_beps() {
        // Geometry: bit numbers count from the right of the 8-byte field.
        assert_eq!((RESERVED_DHT.byte, RESERVED_DHT.mask), (7, 0x01));
        assert_eq!((RESERVED_FAST.byte, RESERVED_FAST.mask), (7, 0x04));
        assert_eq!((RESERVED_EXTENDED.byte, RESERVED_EXTENDED.mask), (5, 0x10));

        assert_eq!(
            ReservedBit::from_bit(7),
            ReservedBit {
                byte: 7,
                mask: 0x80
            }
        );
        assert_eq!(
            ReservedBit::from_bit(8),
            ReservedBit {
                byte: 6,
                mask: 0x01
            }
        );
        assert_eq!(
            ReservedBit::from_bit(63),
            ReservedBit {
                byte: 0,
                mask: 0x80
            }
        );

        // And on the wire: reserved starts at byte 20 (1 pstrlen + 19 pstr).
        let mut handshake = sample();
        handshake.set_dht(true);
        let bytes = handshake.encode();
        assert_eq!(bytes[20 + 7], 0x01);
        assert_eq!(&bytes[20..28], &[0, 0, 0, 0, 0, 0, 0, 0x01]);

        handshake.set_extended(true);
        let bytes = handshake.encode();
        assert_eq!(bytes[20 + 5], 0x10);
        assert_eq!(bytes[20 + 7], 0x01);

        handshake.set_fast(true);
        let bytes = handshake.encode();
        assert_eq!(bytes[20 + 7], 0x05);
        assert_eq!(bytes[20 + 5], 0x10);
        assert!(handshake.supports_dht());
        assert!(handshake.supports_extended());
        assert!(handshake.supports_fast());
    }

    #[test]
    fn reserved_flags_decode_and_clear() {
        let mut handshake = sample();
        handshake.set_dht(true);
        handshake.set_fast(true);
        handshake.set_extended(true);
        let bytes = handshake.encode();

        let mut decoded = Handshake::decode(&bytes).unwrap();
        assert_eq!(decoded.reserved(), [0, 0, 0, 0, 0, 0x10, 0, 0x05]);
        assert_eq!(decoded.reserved_byte(5), Some(0x10));
        assert_eq!(decoded.reserved_byte(8), None);

        decoded.set_dht(false);
        assert!(!decoded.supports_dht());
        assert!(decoded.supports_fast());
        assert!(decoded.supports_extended());
        assert_eq!(decoded.reserved()[7], 0x04);

        // Withdrawing a bit that was never set is a no-op.
        decoded.set_extended(false);
        assert_eq!(decoded.reserved(), [0, 0, 0, 0, 0, 0, 0, 0x04]);
    }
}
