//! Peer wire messages and their length-prefixed framing (BEP 3).
//!
//! Every frame is a 4-byte big-endian body length followed by the body. A body
//! length of zero is a keep-alive and has no message id. Because TCP hands out
//! arbitrary byte counts, [`Decoder`] keeps the partial frame in a buffer and
//! is driven by [`Decoder::feed`] / [`Decoder::next_frame`]; it never blocks and
//! never reads a clock.
//!
//! Scope note: only the ten base messages are defined. Unknown ids — including
//! the fast-extension and extension-protocol ids, which are not implemented
//! yet — are a hard [`DecodeError::UnknownMessageId`] rather than a silent
//! "unknown" message with an opaque payload. A connection that speaks them is
//! dropped, which is honest: this crate cannot honour their semantics.
//!
//! Allocation policy: the decoder sizes its buffer from bytes actually
//! received, never from the declared length. A declared length is checked
//! against [`DEFAULT_MAX_FRAME_LEN`] (or [`Decoder::with_max_frame_len`]) before
//! a single byte is reserved for the body.

use std::fmt;

use thiserror::Error;

use crate::bitfield::{Bitfield, BitfieldError};

/// Default upper bound on a frame **body** (the value in the length prefix).
///
/// 128 KiB + 9 bytes = 131 081: enough for a 128 KiB `piece` block plus the
/// 9-byte `piece` header (id, index, begin). BEP 3 blocks are 16 KiB, so this is
/// generous headroom for non-conforming peers while staying far below what
/// could exhaust memory. It also fits the bitfield of any torrent with up to
/// 1 048 576 pieces; a session serving larger torrents must raise the limit
/// with [`Decoder::with_max_frame_len`] rather than hoping for a huge buffer.
pub const DEFAULT_MAX_FRAME_LEN: usize = 128 * 1024 + 9;

/// The standard BEP 3 block size: 16 KiB.
pub const BLOCK_SIZE: usize = 16 * 1024;

/// The largest block a single `piece` message should carry: 128 KiB.
///
/// [`DEFAULT_MAX_FRAME_LEN`] is sized to hold exactly one of these.
pub const MAX_BLOCK_SIZE: usize = 128 * 1024;

/// Wire bytes of a `piece` frame that are not payload: the 4-byte length
/// prefix, the message id, and the 8-byte index/begin header.
pub const PIECE_FRAME_OVERHEAD: usize = 4 + 1 + 4 + 4;

/// Message ids defined by BEP 3.
pub mod ids {
    /// Choke.
    pub const CHOKE: u8 = 0;
    /// Unchoke.
    pub const UNCHOKE: u8 = 1;
    /// Interested.
    pub const INTERESTED: u8 = 2;
    /// Not interested.
    pub const NOT_INTERESTED: u8 = 3;
    /// Have: one piece index.
    pub const HAVE: u8 = 4;
    /// Bitfield.
    pub const BITFIELD: u8 = 5;
    /// Request: index, begin, length.
    pub const REQUEST: u8 = 6;
    /// Piece: index, begin, block.
    pub const PIECE: u8 = 7;
    /// Cancel: index, begin, length.
    pub const CANCEL: u8 = 8;
    /// Port: DHT listen port (BEP 5).
    pub const PORT: u8 = 9;
}

/// A bitfield as it arrived on the wire.
///
/// The piece count lives in the metainfo, not the message, so an incoming
/// bitfield cannot be validated at decode time. It is held opaquely until the
/// session calls [`BitfieldMessage::into_bitfield`] with the torrent's piece
/// count; only then are length and trailing bits checked.
#[derive(Clone, PartialEq, Eq)]
pub struct BitfieldMessage {
    bytes: Vec<u8>,
}

impl BitfieldMessage {
    /// Wraps the wire bytes of a bitfield payload.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// The wire bytes, without their length prefix or message id.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Payload length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the payload is empty (only valid for a zero-piece torrent).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Validates against `pieces` and produces a usable [`Bitfield`].
    pub fn into_bitfield(self, pieces: usize) -> Result<Bitfield, BitfieldError> {
        Bitfield::from_bytes(pieces, &self.bytes)
    }
}

impl From<&Bitfield> for BitfieldMessage {
    fn from(field: &Bitfield) -> Self {
        Self::from_bytes(field.to_bytes().to_vec())
    }
}

impl From<Bitfield> for BitfieldMessage {
    fn from(field: Bitfield) -> Self {
        Self::from_bytes(field.into_bytes())
    }
}

impl fmt::Debug for BitfieldMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BitfieldMessage")
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// A peer wire message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Keep-alive: a zero-length frame.
    KeepAlive,
    /// Stop sending requests to this peer.
    Choke,
    /// Requests may be sent to this peer.
    Unchoke,
    /// This peer wants pieces from us.
    Interested,
    /// This peer no longer wants pieces from us.
    NotInterested,
    /// This peer now has `index`.
    Have {
        /// Piece index.
        index: u32,
    },
    /// Availability bitmap.
    Bitfield(BitfieldMessage),
    /// Ask for `length` bytes of `index` starting at `begin`.
    Request {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block length in bytes.
        length: u32,
    },
    /// A block of piece data.
    Piece {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block bytes; never empty.
        block: Vec<u8>,
    },
    /// Withdraw a previously sent [`Message::Request`].
    Cancel {
        /// Piece index.
        index: u32,
        /// Byte offset inside the piece.
        begin: u32,
        /// Block length in bytes.
        length: u32,
    },
    /// DHT listen port.
    Port(u16),
}

impl Message {
    /// The message id, or `None` for a keep-alive.
    #[must_use]
    pub fn id(&self) -> Option<u8> {
        match self {
            Self::KeepAlive => None,
            Self::Choke => Some(ids::CHOKE),
            Self::Unchoke => Some(ids::UNCHOKE),
            Self::Interested => Some(ids::INTERESTED),
            Self::NotInterested => Some(ids::NOT_INTERESTED),
            Self::Have { .. } => Some(ids::HAVE),
            Self::Bitfield(_) => Some(ids::BITFIELD),
            Self::Request { .. } => Some(ids::REQUEST),
            Self::Piece { .. } => Some(ids::PIECE),
            Self::Cancel { .. } => Some(ids::CANCEL),
            Self::Port(_) => Some(ids::PORT),
        }
    }

    /// Bytes this message contributes to a rate limiter: the block of a
    /// `piece` message, and nothing else.
    ///
    /// Length prefixes, ids, indexes and bitfields are protocol overhead and
    /// are not counted (see [`crate::limit`]). Index and length *bounds* are
    /// the caller's responsibility.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        match self {
            Self::Piece { block, .. } => block.len(),
            _ => 0,
        }
    }

    /// Length of the body (the value of the length prefix).
    fn body_len(&self) -> usize {
        match self {
            Self::KeepAlive => 0,
            Self::Choke | Self::Unchoke | Self::Interested | Self::NotInterested => 1,
            Self::Have { .. } => 5,
            Self::Bitfield(field) => 1 + field.len(),
            Self::Request { .. } | Self::Cancel { .. } => 13,
            Self::Piece { block, .. } => 1 + 8 + block.len(),
            Self::Port(_) => 3,
        }
    }

    /// Total encoded size, length prefix included.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        4 + self.body_len()
    }
}

/// Encodes one message with the default maximum frame length.
pub fn encode(message: &Message) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    encode_into(message, DEFAULT_MAX_FRAME_LEN, &mut out)?;
    Ok(out)
}

/// Encodes one message into `out`, refusing bodies above `max_frame_len`.
///
/// `out` is left untouched when the message is rejected.
pub fn encode_into(
    message: &Message,
    max_frame_len: usize,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    let body_len = message.body_len();
    let too_large = || EncodeError::FrameTooLarge {
        length: body_len,
        max: max_frame_len,
    };
    if body_len > max_frame_len {
        return Err(too_large());
    }
    let prefix = u32::try_from(body_len).map_err(|_| too_large())?;

    out.reserve(4 + body_len);
    out.extend_from_slice(&prefix.to_be_bytes());
    match message {
        Message::KeepAlive => {}
        Message::Choke => out.push(ids::CHOKE),
        Message::Unchoke => out.push(ids::UNCHOKE),
        Message::Interested => out.push(ids::INTERESTED),
        Message::NotInterested => out.push(ids::NOT_INTERESTED),
        Message::Have { index } => {
            out.push(ids::HAVE);
            out.extend_from_slice(&index.to_be_bytes());
        }
        Message::Bitfield(field) => {
            out.push(ids::BITFIELD);
            out.extend_from_slice(field.as_bytes());
        }
        Message::Request {
            index,
            begin,
            length,
        } => {
            out.push(ids::REQUEST);
            push_u32_triplet(out, *index, *begin, *length);
        }
        Message::Cancel {
            index,
            begin,
            length,
        } => {
            out.push(ids::CANCEL);
            push_u32_triplet(out, *index, *begin, *length);
        }
        Message::Piece {
            index,
            begin,
            block,
        } => {
            out.push(ids::PIECE);
            out.extend_from_slice(&index.to_be_bytes());
            out.extend_from_slice(&begin.to_be_bytes());
            out.extend_from_slice(block);
        }
        Message::Port(port) => {
            out.push(ids::PORT);
            out.extend_from_slice(&port.to_be_bytes());
        }
    }
    Ok(())
}

/// Appends three big-endian `u32`s (index, begin, length).
fn push_u32_triplet(out: &mut Vec<u8>, index: u32, begin: u32, length: u32) {
    out.extend_from_slice(&index.to_be_bytes());
    out.extend_from_slice(&begin.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
}

/// Errors from [`encode`] / [`encode_into`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EncodeError {
    /// The body does not fit the configured frame limit (or a `u32` prefix).
    #[error("message body of {length} bytes does not fit the {max}-byte frame limit")]
    FrameTooLarge {
        /// Body length that was requested.
        length: usize,
        /// Maximum body length allowed.
        max: usize,
    },
}

/// Errors from [`Decoder::next_frame`].
///
/// Any error is fatal for the connection: the decoder does not resynchronise
/// and will keep reporting it, because a length prefix that lies cannot be
/// resynchronised from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DecodeError {
    /// A declared body length exceeded the configured cap.
    #[error("declared frame body of {declared} bytes exceeds the {max}-byte limit")]
    FrameTooLarge {
        /// Length read from the prefix.
        declared: usize,
        /// Maximum body length the decoder accepts.
        max: usize,
    },

    /// A message with a fixed body length arrived with a different length.
    #[error("message id {id:#04x} has frame length {length}, expected {expected}")]
    UnexpectedLength {
        /// Message id.
        id: u8,
        /// Body length, id included.
        length: usize,
        /// Required body length.
        expected: usize,
    },

    /// A message is too short to contain the fields its id requires.
    #[error("message id {id:#04x} with frame length {length} has no room for its fields")]
    MissingFields {
        /// Message id.
        id: u8,
        /// Body length, id included.
        length: usize,
    },

    /// A `piece` message carried an empty block.
    #[error("piece frame of length {length} carries no block data")]
    EmptyBlock {
        /// Body length, id included.
        length: usize,
    },

    /// A non-empty frame had no room for a message id.
    #[error("frame carries a length but no message id")]
    MissingMessageId,

    /// The message id is not part of BEP 3.
    #[error("unknown message id {id:#04x}")]
    UnknownMessageId {
        /// The unrecognised id.
        id: u8,
    },
}

/// Incremental frame decoder for one peer connection.
///
/// Feed it whatever the socket read produced, then drain with
/// [`next_frame`](Decoder::next_frame) until it returns `Ok(None)`.
///
/// ```
/// use flux_net::{Decoder, Message};
///
/// let mut decoder = Decoder::new();
/// for byte in [0u8, 0, 0, 1, 1] {
///     decoder.feed(&[byte]);
/// }
/// assert_eq!(decoder.next_frame().expect("valid"), Some(Message::Unchoke));
/// assert_eq!(decoder.next_frame().expect("valid"), None);
/// ```
#[derive(Clone)]
pub struct Decoder {
    buf: Vec<u8>,
    head: usize,
    max_frame_len: usize,
}

impl Decoder {
    /// Creates a decoder with the default frame limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_frame_len(DEFAULT_MAX_FRAME_LEN)
    }

    /// Creates a decoder with an explicit body-length cap.
    ///
    /// The cap is clamped to `u32::MAX`, the largest length a prefix can
    /// express.
    #[must_use]
    pub fn with_max_frame_len(max_frame_len: usize) -> Self {
        Self {
            buf: Vec::new(),
            head: 0,
            max_frame_len: max_frame_len.min(u32::MAX as usize),
        }
    }

    /// The configured body-length cap.
    #[must_use]
    pub fn max_frame_len(&self) -> usize {
        self.max_frame_len
    }

    /// Bytes buffered but not yet consumed as a complete frame.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buf.len() - self.head
    }

    /// Appends bytes read from the socket.
    ///
    /// The buffer only grows by what the caller feeds; declared lengths never
    /// size an allocation.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.head > 0 {
            self.buf.drain(..self.head);
            self.head = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Parses the next complete frame, or `Ok(None)` if more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Message>, DecodeError> {
        let available = self.buffered_len();
        if available < 4 {
            return Ok(None);
        }

        let Some(prefix) = self
            .buf
            .get(self.head..self.head + 4)
            .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        else {
            return Ok(None);
        };
        let declared = u32::from_be_bytes(prefix) as usize;

        if declared == 0 {
            self.head += 4;
            self.compact();
            return Ok(Some(Message::KeepAlive));
        }
        if declared > self.max_frame_len {
            return Err(DecodeError::FrameTooLarge {
                declared,
                max: self.max_frame_len,
            });
        }
        // `declared >= 1` and `available >= 4`, so this cannot underflow.
        if available - 4 < declared {
            return Ok(None);
        }

        let start = self.head + 4;
        let end = start + declared;
        let Some(body) = self.buf.get(start..end) else {
            return Ok(None);
        };
        let message = decode_body(body)?;
        self.head = end;
        self.compact();
        Ok(Some(message))
    }

    /// Drops consumed bytes once the whole buffer is drained.
    fn compact(&mut self) {
        if self.head == self.buf.len() {
            self.buf.clear();
            self.head = 0;
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoder")
            .field("buffered", &self.buffered_len())
            .field("max_frame_len", &self.max_frame_len)
            .finish_non_exhaustive()
    }
}

/// Decodes one frame body (id included, length prefix excluded).
fn decode_body(body: &[u8]) -> Result<Message, DecodeError> {
    let Some((&id, payload)) = body.split_first() else {
        return Err(DecodeError::MissingMessageId);
    };
    let length = body.len();
    match id {
        ids::CHOKE => {
            expect_empty(id, length, payload)?;
            Ok(Message::Choke)
        }
        ids::UNCHOKE => {
            expect_empty(id, length, payload)?;
            Ok(Message::Unchoke)
        }
        ids::INTERESTED => {
            expect_empty(id, length, payload)?;
            Ok(Message::Interested)
        }
        ids::NOT_INTERESTED => {
            expect_empty(id, length, payload)?;
            Ok(Message::NotInterested)
        }
        ids::HAVE => {
            expect_length(id, length, 5)?;
            Ok(Message::Have {
                index: take_u32(payload, id)?.0,
            })
        }
        ids::BITFIELD => Ok(Message::Bitfield(BitfieldMessage::from_bytes(
            payload.to_vec(),
        ))),
        ids::REQUEST | ids::CANCEL => {
            expect_length(id, length, 13)?;
            let (index, payload) = take_u32(payload, id)?;
            let (begin, payload) = take_u32(payload, id)?;
            let (request_length, _) = take_u32(payload, id)?;
            if id == ids::REQUEST {
                Ok(Message::Request {
                    index,
                    begin,
                    length: request_length,
                })
            } else {
                Ok(Message::Cancel {
                    index,
                    begin,
                    length: request_length,
                })
            }
        }
        ids::PIECE => {
            if length < 9 {
                return Err(DecodeError::MissingFields { id, length });
            }
            if length == 9 {
                return Err(DecodeError::EmptyBlock { length });
            }
            let (index, payload) = take_u32(payload, id)?;
            let (begin, block) = take_u32(payload, id)?;
            Ok(Message::Piece {
                index,
                begin,
                block: block.to_vec(),
            })
        }
        ids::PORT => {
            expect_length(id, length, 3)?;
            match payload
                .get(..2)
                .and_then(|slice| <[u8; 2]>::try_from(slice).ok())
            {
                Some(port) => Ok(Message::Port(u16::from_be_bytes(port))),
                None => Err(DecodeError::MissingFields { id, length }),
            }
        }
        _ => Err(DecodeError::UnknownMessageId { id }),
    }
}

fn expect_empty(id: u8, length: usize, payload: &[u8]) -> Result<(), DecodeError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(DecodeError::UnexpectedLength {
            id,
            length,
            expected: 1,
        })
    }
}

fn expect_length(id: u8, length: usize, expected: usize) -> Result<(), DecodeError> {
    if length == expected {
        Ok(())
    } else {
        Err(DecodeError::UnexpectedLength {
            id,
            length,
            expected,
        })
    }
}

/// Splits one big-endian `u32` off the front of `payload`.
fn take_u32(payload: &[u8], id: u8) -> Result<(u32, &[u8]), DecodeError> {
    let (head, rest) = payload
        .split_at_checked(4)
        .ok_or(DecodeError::MissingFields {
            id,
            length: payload.len() + 1,
        })?;
    let bytes: [u8; 4] = head.try_into().map_err(|_| DecodeError::MissingFields {
        id,
        length: payload.len() + 1,
    })?;
    Ok((u32::from_be_bytes(bytes), rest))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn decode_one(bytes: &[u8]) -> Result<Message, DecodeError> {
        let mut decoder = Decoder::new();
        decoder.feed(bytes);
        decoder.next_frame()?.ok_or(DecodeError::MissingMessageId)
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    fn round_trip(message: Message) {
        let bytes = encode(&message).expect("message encodes");
        assert_eq!(bytes.len(), message.encoded_len());
        assert_eq!(decode_one(&bytes).expect("message decodes"), message);
    }

    #[test]
    fn every_message_round_trips() {
        round_trip(Message::KeepAlive);
        round_trip(Message::Choke);
        round_trip(Message::Unchoke);
        round_trip(Message::Interested);
        round_trip(Message::NotInterested);
        round_trip(Message::Have { index: 0 });
        round_trip(Message::Have { index: 123_456 });
        round_trip(Message::Bitfield(BitfieldMessage::from_bytes(vec![
            0xFF, 0x80,
        ])));
        round_trip(Message::Request {
            index: 1,
            begin: 16_384,
            length: 16_384,
        });
        round_trip(Message::Piece {
            index: 7,
            begin: 0,
            block: vec![0xAB; BLOCK_SIZE],
        });
        round_trip(Message::Cancel {
            index: 9,
            begin: 32_768,
            length: 16_384,
        });
        round_trip(Message::Port(68_81));
    }

    #[test]
    fn keep_alive_is_a_zero_length_frame() {
        assert_eq!(encode(&Message::KeepAlive).unwrap(), vec![0, 0, 0, 0]);
        assert_eq!(decode_one(&[0, 0, 0, 0]).unwrap(), Message::KeepAlive);
        // Back-to-back keep-alives.
        let mut decoder = Decoder::new();
        decoder.feed(&[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(decoder.next_frame().unwrap(), Some(Message::KeepAlive));
        assert_eq!(decoder.next_frame().unwrap(), Some(Message::KeepAlive));
        assert_eq!(decoder.next_frame().unwrap(), None);
    }

    #[test]
    fn split_feed_survives_one_byte_at_a_time() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&encode(&Message::Have { index: 42 }).unwrap());
        stream.extend_from_slice(
            &encode(&Message::Piece {
                index: 3,
                begin: 16_384,
                block: vec![0x5A; 1024],
            })
            .unwrap(),
        );
        stream.extend_from_slice(
            &encode(&Message::Cancel {
                index: 3,
                begin: 16_384,
                length: 1024,
            })
            .unwrap(),
        );
        stream.extend_from_slice(&encode(&Message::KeepAlive).unwrap());

        let mut decoder = Decoder::new();
        let mut decoded = Vec::new();
        for byte in stream {
            decoder.feed(&[byte]);
            while let Some(message) = decoder.next_frame().expect("valid frame") {
                decoded.push(message);
            }
        }

        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded[0], Message::Have { index: 42 });
        assert_eq!(
            decoded[1],
            Message::Piece {
                index: 3,
                begin: 16_384,
                block: vec![0x5A; 1024],
            }
        );
        assert_eq!(
            decoded[2],
            Message::Cancel {
                index: 3,
                begin: 16_384,
                length: 1024,
            }
        );
        assert_eq!(decoded[3], Message::KeepAlive);
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn partial_frame_waits_for_more_bytes() {
        let bytes = encode(&Message::Request {
            index: 1,
            begin: 2,
            length: 3,
        })
        .unwrap();
        let mut decoder = Decoder::new();
        for byte in &bytes[..bytes.len() - 1] {
            decoder.feed(&[*byte]);
            assert_eq!(decoder.next_frame().unwrap(), None);
        }
        decoder.feed(&bytes[bytes.len() - 1..]);
        assert_eq!(
            decoder.next_frame().unwrap(),
            Some(Message::Request {
                index: 1,
                begin: 2,
                length: 3,
            })
        );
    }

    #[test]
    fn oversized_length_rejected_before_allocating() {
        let mut decoder = Decoder::with_max_frame_len(16);
        decoder.feed(&17u32.to_be_bytes());
        assert_eq!(
            decoder.next_frame(),
            Err(DecodeError::FrameTooLarge {
                declared: 17,
                max: 16,
            })
        );
        // Only the four prefix bytes are buffered: the body was never sized.
        assert_eq!(decoder.buffered_len(), 4);

        let mut decoder = Decoder::new();
        let declared = u32::try_from(DEFAULT_MAX_FRAME_LEN + 1).unwrap();
        decoder.feed(&declared.to_be_bytes());
        assert_eq!(
            decoder.next_frame(),
            Err(DecodeError::FrameTooLarge {
                declared: DEFAULT_MAX_FRAME_LEN + 1,
                max: DEFAULT_MAX_FRAME_LEN,
            })
        );

        // A hostile u32::MAX declaration is rejected with four bytes buffered.
        let mut decoder = Decoder::new();
        decoder.feed(&u32::MAX.to_be_bytes());
        assert!(matches!(
            decoder.next_frame(),
            Err(DecodeError::FrameTooLarge { .. })
        ));
        assert_eq!(decoder.buffered_len(), 4);
    }

    #[test]
    fn max_frame_len_is_configurable_both_ways() {
        let block = vec![0u8; 32];
        let message = Message::Piece {
            index: 0,
            begin: 0,
            block: block.clone(),
        };
        let mut out = Vec::new();
        assert_eq!(
            encode_into(&message, 16, &mut out),
            Err(EncodeError::FrameTooLarge {
                length: 41,
                max: 16,
            })
        );
        assert!(out.is_empty(), "rejected messages must not write a prefix");

        let mut out = Vec::new();
        encode_into(&message, 41, &mut out).unwrap();
        let mut decoder = Decoder::with_max_frame_len(41);
        decoder.feed(&out);
        assert_eq!(decoder.next_frame().unwrap(), Some(message));
    }

    #[test]
    fn unknown_message_id_is_an_error() {
        // 20 is the extension protocol (BEP 10), not implemented here.
        let bytes = frame(&[20]);
        assert_eq!(
            decode_one(&bytes),
            Err(DecodeError::UnknownMessageId { id: 20 })
        );
        assert_eq!(
            decode_one(&frame(&[0xFE])),
            Err(DecodeError::UnknownMessageId { id: 0xFE })
        );
    }

    #[test]
    fn zero_length_non_keepalive_rejected() {
        // Length 0 *is* keep-alive, so no non-keepalive message can encode to
        // zero bytes. A one-byte body with an id that needs fields is the
        // shortest thing that looks like a message and is still rejected.
        assert_eq!(
            decode_one(&[0, 0, 0, 1, 4]).unwrap_err().to_string(),
            "message id 0x04 has frame length 1, expected 5"
        );
        assert_eq!(
            decode_one(&frame(&[ids::PIECE])),
            Err(DecodeError::MissingFields { id: 7, length: 1 })
        );
        assert_eq!(
            decode_one(&frame(&[ids::PORT])),
            Err(DecodeError::UnexpectedLength {
                id: 9,
                length: 1,
                expected: 3,
            })
        );
    }

    #[test]
    fn fixed_length_messages_reject_extra_and_missing_bytes() {
        assert_eq!(
            decode_one(&frame(&[ids::CHOKE, 0x00])),
            Err(DecodeError::UnexpectedLength {
                id: 0,
                length: 2,
                expected: 1,
            })
        );
        // Have with a trailing byte.
        assert_eq!(
            decode_one(&frame(&[ids::HAVE, 0, 0, 0, 7, 0xFF])),
            Err(DecodeError::UnexpectedLength {
                id: 4,
                length: 6,
                expected: 5,
            })
        );
        // Request one byte short.
        let mut body = vec![ids::REQUEST];
        body.extend_from_slice(&[0; 11]);
        assert_eq!(
            decode_one(&frame(&body)),
            Err(DecodeError::UnexpectedLength {
                id: 6,
                length: 12,
                expected: 13,
            })
        );
        // Port with a trailing byte.
        assert_eq!(
            decode_one(&frame(&[ids::PORT, 0x1A, 0xE1, 0x00])),
            Err(DecodeError::UnexpectedLength {
                id: 9,
                length: 4,
                expected: 3,
            })
        );
    }

    #[test]
    fn a_full_128_kib_block_fits_the_default_cap() {
        let message = Message::Piece {
            index: 0,
            begin: 0,
            block: vec![0x5A; MAX_BLOCK_SIZE],
        };
        let encoded = encode(&message).expect("a 128 KiB block fits the default cap");
        assert_eq!(encoded.len() - 4, DEFAULT_MAX_FRAME_LEN);
        assert_eq!(message.encoded_len(), DEFAULT_MAX_FRAME_LEN + 4);

        let mut decoder = Decoder::new();
        decoder.feed(&encoded);
        assert_eq!(decoder.next_frame().unwrap(), Some(message));
    }

    #[test]
    fn piece_without_block_rejected() {
        let mut body = vec![ids::PIECE];
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&2u32.to_be_bytes());
        assert_eq!(
            decode_one(&frame(&body)),
            Err(DecodeError::EmptyBlock { length: 9 })
        );
    }

    #[test]
    fn piece_payload_is_the_block_only() {
        let message = Message::Piece {
            index: 1,
            begin: 0,
            block: vec![0; 1234],
        };
        assert_eq!(message.payload_len(), 1234);
        assert_eq!(message.encoded_len(), 1234 + PIECE_FRAME_OVERHEAD);
        assert_eq!(Message::Have { index: 1 }.payload_len(), 0);
        assert_eq!(
            Message::Request {
                index: 0,
                begin: 0,
                length: 16
            }
            .payload_len(),
            0
        );
    }

    #[test]
    fn bitfield_is_validated_against_the_torrent() {
        let field = Bitfield::all(10);
        let message = Message::Bitfield(BitfieldMessage::from(&field));
        let encoded = encode(&message).unwrap();
        let decoded = decode_one(&encoded).unwrap();
        let Message::Bitfield(raw) = decoded else {
            panic!("expected a bitfield");
        };
        assert_eq!(raw.len(), 2);
        assert_eq!(raw.clone().into_bitfield(10).unwrap(), field);

        let junk = BitfieldMessage::from_bytes(vec![0xFF, 0xFF]);
        assert!(junk.into_bitfield(10).is_err());

        let empty = Message::Bitfield(BitfieldMessage::from_bytes(Vec::new()));
        round_trip(empty);
        assert!(BitfieldMessage::from_bytes(Vec::new()).is_empty());
    }

    #[test]
    fn message_ids_match_bep_3() {
        assert_eq!(Message::Choke.id(), Some(0));
        assert_eq!(Message::Unchoke.id(), Some(1));
        assert_eq!(Message::Interested.id(), Some(2));
        assert_eq!(Message::NotInterested.id(), Some(3));
        assert_eq!(Message::Have { index: 0 }.id(), Some(4));
        assert_eq!(
            Message::Bitfield(BitfieldMessage::from_bytes(vec![])).id(),
            Some(5)
        );
        assert_eq!(
            Message::Request {
                index: 0,
                begin: 0,
                length: 1
            }
            .id(),
            Some(6)
        );
        assert_eq!(
            Message::Piece {
                index: 0,
                begin: 0,
                block: vec![1]
            }
            .id(),
            Some(7)
        );
        assert_eq!(
            Message::Cancel {
                index: 0,
                begin: 0,
                length: 1
            }
            .id(),
            Some(8)
        );
        assert_eq!(Message::Port(1).id(), Some(9));
        assert_eq!(Message::KeepAlive.id(), None);
    }

    #[test]
    fn exact_wire_bytes() {
        assert_eq!(
            encode(&Message::Port(0x1AE1)).unwrap(),
            vec![0, 0, 0, 3, 9, 0x1A, 0xE1]
        );
        assert_eq!(
            encode(&Message::Request {
                index: 1,
                begin: 2,
                length: 3,
            })
            .unwrap(),
            vec![0, 0, 0, 13, 6, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3]
        );
        assert_eq!(
            encode(&Message::Piece {
                index: 1,
                begin: 2,
                block: vec![0xAA, 0xBB],
            })
            .unwrap(),
            vec![0, 0, 0, 11, 7, 0, 0, 0, 1, 0, 0, 0, 2, 0xAA, 0xBB]
        );
    }

    #[test]
    fn buffered_bytes_are_released_once_drained() {
        let mut decoder = Decoder::new();
        decoder.feed(&encode(&Message::KeepAlive).unwrap());
        decoder.feed(&[0, 0]);
        assert_eq!(decoder.buffered_len(), 6);
        assert_eq!(decoder.next_frame().unwrap(), Some(Message::KeepAlive));
        assert_eq!(decoder.buffered_len(), 2);
        assert_eq!(decoder.next_frame().unwrap(), None);
        assert_eq!(decoder.next_frame().unwrap(), None);
    }
}
