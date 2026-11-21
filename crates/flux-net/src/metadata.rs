//! `ut_metadata` (BEP 9): metadata exchange message codec.
//!
//! A magnet link only carries hashes, so a peer that joins a swarm with one has
//! to fetch the info dictionary from another peer before it can do anything
//! else. BEP 9 does that with three messages — request a 16 KiB slice, return a
//! slice, or refuse — wrapped in the BEP 10 extension protocol. This module is
//! only the inner body: the extension handshake, the negotiated `ut_metadata`
//! message id and socket I/O all live elsewhere.
//!
//! ## How the swarm uses it
//!
//! After the extension handshake negotiates an `ut_metadata` id, the peer actor
//! sends [`MetadataMessage::Request`] for each piece of
//! `ceil(total_size / 16 KiB)` and reassembles the [`MetadataMessage::Data`]
//! payloads in piece order. Concretely, the bytes returned by
//! [`MetadataMessage::encode`] become the `payload` of
//! `PeerCommand::Extended { id: ut_metadata_id, payload }`, and an inbound
//! frame arrives as `PeerEvent::Extended`, whose body is handed back to
//! [`MetadataMessage::decode`]. Neither the command/event pair nor the peer
//! actor exists in this crate yet; this codec is the piece they will call, and
//! the hashes in [`MagnetLink`](flux_core::magnet::MagnetLink) are what the
//! reassembled dictionary is checked against.
//!
//! ## Wire format
//!
//! Every message is a bencode dictionary followed, for `data` only, by raw
//! bytes that are *not* length-prefixed:
//!
//! ```text
//! request  d1:msg_typei0e5:piecei<piece>ee
//! data     d1:msg_typei1e5:piecei<piece>10:total_sizei<total>ee<piece bytes>
//! reject   d1:msg_typei2e5:piecei<piece>ee
//! ```
//!
//! Because [`flux_core::bencode`] rejects trailing bytes, [`decode`] first
//! scans the leading dictionary to find its exact end, decodes that slice, and
//! only then treats the remainder as piece data for `data` and as garbage for
//! the other two message types.
//!
//! ## Limits
//!
//! | item | value |
//! | --- | --- |
//! | metadata piece | [`METADATA_PIECE_SIZE`] = 16 KiB |
//! | metadata size cap | [`MAX_METADATA_SIZE`] = 4 MiB |
//!
//! A `data` payload must be exactly as long as its piece implies: 16 KiB for
//! every piece except the last, which is `total_size % 16 KiB` (or 16 KiB when
//! the total divides evenly). Anything else is rejected rather than padded or
//! truncated, so a receiver never has to guess which bytes are real.
//!
//! [`decode`]: MetadataMessage::decode

use flux_core::bencode::{self, Value};

/// The metadata slice size mandated by BEP 9: 16 KiB.
pub const METADATA_PIECE_SIZE: u32 = 16 * 1024;

/// Hard cap on a torrent's metadata, in bytes: 4 MiB.
///
/// Real info dictionaries are orders of magnitude smaller; the cap keeps a
/// hostile peer from announcing a size that would make the receiver allocate
/// without bound while reassembling pieces.
pub const MAX_METADATA_SIZE: u32 = 4 * 1024 * 1024;

/// Largest nesting the structural scan will follow while locating the end of
/// the dictionary. `ut_metadata` dictionaries are one level deep.
const MAX_SCAN_DEPTH: u32 = 8;

/// A `ut_metadata` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataMessage {
    /// Ask the peer for one metadata piece (`msg_type` 0).
    Request {
        /// Zero-based piece index.
        piece: u32,
    },
    /// A metadata piece and the total size it belongs to (`msg_type` 1).
    Data {
        /// Zero-based piece index.
        piece: u32,
        /// Size of the complete metadata blob, 1..=[`MAX_METADATA_SIZE`].
        total_size: u32,
        /// The piece bytes, exactly as many as this piece implies.
        data: Vec<u8>,
    },
    /// Refuse a request, usually because the sender has no metadata
    /// (`msg_type` 2).
    Reject {
        /// Zero-based piece index being refused.
        piece: u32,
    },
}

impl MetadataMessage {
    /// The `msg_type` value this message is encoded with.
    #[must_use]
    pub const fn message_type(&self) -> u8 {
        match self {
            Self::Request { .. } => 0,
            Self::Data { .. } => 1,
            Self::Reject { .. } => 2,
        }
    }

    /// Encode as a bencode dictionary, plus trailing piece bytes for
    /// [`MetadataMessage::Data`].
    ///
    /// No validation happens here: [`MetadataMessage`] values are meant to come
    /// from [`MetadataMessage::decode`] (and therefore already be valid) or
    /// from a caller that built them within the documented limits.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = match self {
            Self::Request { piece } => encoded_dict(*piece, 0, None),
            Self::Reject { piece } => encoded_dict(*piece, 2, None),
            Self::Data {
                piece, total_size, ..
            } => encoded_dict(*piece, 1, Some(*total_size)),
        };
        if let Self::Data { data, .. } = self {
            out.extend_from_slice(data);
        }
        out
    }

    /// Decode the body of an extended message.
    ///
    /// Extra dictionary keys are ignored: a peer may attach hints this codec
    /// does not use, and an unexpected `total_size` on a `request` is not an
    /// error.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for an empty or non-dictionary payload, malformed
    /// bencode, a missing or non-integer `msg_type`/`piece`/`total_size`, an
    /// unsupported message type, an out-of-range size or piece index, a piece
    /// whose length disagrees with `total_size`, or trailing bytes after a
    /// `request`/`reject`.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let end = dictionary_end(payload)?;
        let value = bencode::decode(&payload[..end])?;
        let trailing = &payload[end..];

        let msg_type = match value.get(b"msg_type") {
            Some(Value::Int(number)) => *number,
            Some(_) => {
                return Err(Error::WrongType {
                    key: "msg_type",
                    expected: "an integer",
                });
            }
            None => {
                return Err(Error::MissingKey { key: "msg_type" });
            }
        };

        match msg_type {
            0 => {
                reject_trailing(trailing)?;
                Ok(Self::Request {
                    piece: u32_field(&value, "piece")?,
                })
            }
            2 => {
                reject_trailing(trailing)?;
                Ok(Self::Reject {
                    piece: u32_field(&value, "piece")?,
                })
            }
            1 => {
                let total_size = u32_field(&value, "total_size")?;
                if total_size == 0 || total_size > MAX_METADATA_SIZE {
                    return Err(Error::InvalidTotalSize {
                        value: total_size,
                        max: MAX_METADATA_SIZE,
                    });
                }
                let piece = u32_field(&value, "piece")?;
                let pieces = piece_count(total_size);
                if piece >= pieces {
                    return Err(Error::PieceOutOfRange { piece, pieces });
                }
                let expected = piece_len(total_size, piece);
                if trailing.len() != expected {
                    return Err(Error::InvalidPieceLength {
                        piece,
                        len: trailing.len(),
                        expected,
                    });
                }
                Ok(Self::Data {
                    piece,
                    total_size,
                    data: trailing.to_vec(),
                })
            }
            other => Err(Error::UnknownMessageType { value: other }),
        }
    }
}

/// Number of 16 KiB pieces needed for `total_size` bytes.
///
/// Returns 0 for a zero size, which [`MetadataMessage::decode`] never produces
/// because it rejects `total_size == 0`.
#[must_use]
pub fn piece_count(total_size: u32) -> u32 {
    total_size.div_ceil(METADATA_PIECE_SIZE)
}

/// Number of bytes the piece at `piece` must carry.
///
/// `piece` must be below [`piece_count`] for `total_size`; the arithmetic is
/// done in `u64` so it cannot overflow or wrap.
fn piece_len(total_size: u32, piece: u32) -> usize {
    let offset = u64::from(piece) * u64::from(METADATA_PIECE_SIZE);
    let remaining = u64::from(total_size).saturating_sub(offset);
    remaining.min(u64::from(METADATA_PIECE_SIZE)) as usize
}

/// Reject any bytes left after a dictionary that carries no trailing data.
fn reject_trailing(trailing: &[u8]) -> Result<(), Error> {
    if trailing.is_empty() {
        Ok(())
    } else {
        Err(Error::TrailingData)
    }
}

/// Read `key` as a `u32`, rejecting negatives and values above `u32::MAX`.
fn u32_field(value: &Value, key: &'static str) -> Result<u32, Error> {
    let found = value.get(key.as_bytes()).ok_or(Error::MissingKey { key })?;
    let Value::Int(number) = found else {
        return Err(Error::WrongType {
            key,
            expected: "an integer",
        });
    };
    u32::try_from(*number).map_err(|_| Error::IntegerOutOfRange {
        key,
        value: *number,
    })
}

/// Build the dictionary of a `ut_metadata` message, canonically encoded.
fn encoded_dict(piece: u32, msg_type: i64, total_size: Option<u32>) -> Vec<u8> {
    let mut entries = vec![
        (b"msg_type".to_vec(), Value::Int(msg_type)),
        (b"piece".to_vec(), Value::Int(i64::from(piece))),
    ];
    if let Some(total) = total_size {
        entries.push((b"total_size".to_vec(), Value::Int(i64::from(total))));
    }
    bencode::encode(&Value::Dict(entries))
}

/// Return the offset just past the leading bencode dictionary.
///
/// This is a structural scan, not a parser: it only has to find where the
/// dictionary ends so the trailing piece bytes can be separated. The slice it
/// returns is decoded — and fully validated — by [`flux_core::bencode`], which
/// is the only bencode parser in the workspace.
fn dictionary_end(payload: &[u8]) -> Result<usize, Error> {
    match payload.first() {
        None => Err(Error::EmptyPayload),
        Some(b'd') => skip_value(payload, 0, 0),
        Some(_) => Err(Error::NotADictionary),
    }
}

/// Skip one bencode value starting at `pos`, returning the offset after it.
fn skip_value(input: &[u8], pos: usize, depth: u32) -> Result<usize, Error> {
    if depth > MAX_SCAN_DEPTH {
        return Err(Error::Malformed);
    }
    let tag = *input.get(pos).ok_or(Error::Malformed)?;
    match tag {
        b'i' => {
            let mut cursor = pos + 1;
            while let Some(&byte) = input.get(cursor) {
                if byte == b'e' {
                    return Ok(cursor + 1);
                }
                cursor += 1;
            }
            Err(Error::Malformed)
        }
        b'l' => {
            let mut cursor = pos + 1;
            loop {
                match input.get(cursor) {
                    Some(b'e') => return Ok(cursor + 1),
                    Some(_) => cursor = skip_value(input, cursor, depth + 1)?,
                    None => return Err(Error::Malformed),
                }
            }
        }
        b'd' => {
            let mut cursor = pos + 1;
            loop {
                match input.get(cursor) {
                    Some(b'e') => return Ok(cursor + 1),
                    Some(b'0'..=b'9') => {
                        cursor = skip_string(input, cursor)?;
                        cursor = skip_value(input, cursor, depth + 1)?;
                    }
                    _ => return Err(Error::Malformed),
                }
            }
        }
        b'0'..=b'9' => skip_string(input, pos),
        _ => Err(Error::Malformed),
    }
}

/// Skip one length-prefixed bencode byte string starting at `pos`.
fn skip_string(input: &[u8], pos: usize) -> Result<usize, Error> {
    let mut cursor = pos;
    let mut len: usize = 0;
    loop {
        match input.get(cursor) {
            Some(b':') => break,
            Some(byte @ b'0'..=b'9') => {
                len = len
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(usize::from(byte - b'0')))
                    .ok_or(Error::Malformed)?;
                cursor += 1;
            }
            _ => return Err(Error::Malformed),
        }
    }
    let start = cursor + 1;
    let end = start.checked_add(len).ok_or(Error::Malformed)?;
    if end > input.len() {
        return Err(Error::Malformed);
    }
    Ok(end)
}

/// A `ut_metadata` decoding failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The extended-message body was empty.
    #[error("empty `ut_metadata` payload")]
    EmptyPayload,
    /// The payload did not start with a dictionary.
    #[error("`ut_metadata` payload is not a bencode dictionary")]
    NotADictionary,
    /// The bencode framing was truncated or otherwise unscannable.
    #[error("malformed bencode framing")]
    Malformed,
    /// The dictionary itself was invalid bencode.
    #[error("bencode: {0}")]
    Bencode(#[from] bencode::Error),
    /// A required key was absent.
    #[error("dictionary is missing `{key}`")]
    MissingKey {
        /// Key name.
        key: &'static str,
    },
    /// A key had the wrong bencode type.
    #[error("`{key}` must be {expected}")]
    WrongType {
        /// Key name.
        key: &'static str,
        /// Human-readable expected type.
        expected: &'static str,
    },
    /// An integer field was negative or too large for its type.
    #[error("`{key}` = {value} is out of range")]
    IntegerOutOfRange {
        /// Key name.
        key: &'static str,
        /// The value found.
        value: i64,
    },
    /// `msg_type` was not 0, 1 or 2.
    #[error("unsupported `msg_type` {value}")]
    UnknownMessageType {
        /// The value found.
        value: i64,
    },
    /// `total_size` was zero or above [`MAX_METADATA_SIZE`].
    #[error("`total_size` {value} is outside 1..={max}")]
    InvalidTotalSize {
        /// The value found.
        value: u32,
        /// Configured cap.
        max: u32,
    },
    /// The piece index does not exist in a blob of that size.
    #[error("piece {piece} is out of range for {pieces} piece(s)")]
    PieceOutOfRange {
        /// The requested index.
        piece: u32,
        /// Number of pieces the declared size needs.
        pieces: u32,
    },
    /// The trailing piece length disagreed with `total_size`.
    #[error("piece {piece} carries {len} bytes, expected {expected}")]
    InvalidPieceLength {
        /// Piece index.
        piece: u32,
        /// Bytes actually present.
        len: usize,
        /// Bytes the declared `total_size` implies.
        expected: usize,
    },
    /// Bytes followed a dictionary that must end the payload.
    #[error("trailing bytes after the `ut_metadata` dictionary")]
    TrailingData,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use flux_core::bencode::ErrorKind;

    use super::*;

    fn dict(entries: Vec<(&str, Value)>) -> Vec<u8> {
        bencode::encode(&Value::Dict(
            entries
                .into_iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value))
                .collect(),
        ))
    }

    fn request_dict(piece: i64) -> Vec<u8> {
        dict(vec![
            ("msg_type", Value::Int(0)),
            ("piece", Value::Int(piece)),
        ])
    }

    fn data_dict(piece: i64, total_size: i64) -> Vec<u8> {
        dict(vec![
            ("msg_type", Value::Int(1)),
            ("piece", Value::Int(piece)),
            ("total_size", Value::Int(total_size)),
        ])
    }

    fn payload_dict(piece: u32, total_size: u32) -> Vec<u8> {
        data_dict(i64::from(piece), i64::from(total_size))
    }

    #[test]
    fn request_round_trips() {
        let message = MetadataMessage::Request { piece: 7 };
        assert_eq!(message.message_type(), 0);
        let bytes = message.encode();
        assert_eq!(bytes, request_dict(7));
        assert_eq!(MetadataMessage::decode(&bytes).unwrap(), message);
    }

    #[test]
    fn reject_round_trips() {
        let message = MetadataMessage::Reject { piece: 0 };
        assert_eq!(message.message_type(), 2);
        let bytes = message.encode();
        assert_eq!(
            bytes,
            dict(vec![("msg_type", Value::Int(2)), ("piece", Value::Int(0))])
        );
        assert_eq!(MetadataMessage::decode(&bytes).unwrap(), message);
    }

    #[test]
    fn data_round_trips_for_a_final_short_piece() {
        let message = MetadataMessage::Data {
            piece: 1,
            total_size: METADATA_PIECE_SIZE + 5,
            data: vec![0xab; 5],
        };
        assert_eq!(message.message_type(), 1);
        let bytes = message.encode();
        assert_eq!(
            &bytes[..bytes.len() - 5],
            payload_dict(1, METADATA_PIECE_SIZE + 5).as_slice()
        );
        assert_eq!(&bytes[bytes.len() - 5..], &[0xab; 5]);
        assert_eq!(MetadataMessage::decode(&bytes).unwrap(), message);
    }

    #[test]
    fn data_round_trips_for_a_full_piece() {
        let message = MetadataMessage::Data {
            piece: 0,
            total_size: METADATA_PIECE_SIZE,
            data: vec![0x5a; METADATA_PIECE_SIZE as usize],
        };
        let bytes = message.encode();
        assert_eq!(MetadataMessage::decode(&bytes).unwrap(), message);
    }

    #[test]
    fn full_piece_encoding_is_dict_plus_sixteen_kib() {
        let message = MetadataMessage::Data {
            piece: 0,
            total_size: 2 * METADATA_PIECE_SIZE,
            data: vec![0x11; METADATA_PIECE_SIZE as usize],
        };
        let dict_len = payload_dict(0, 2 * METADATA_PIECE_SIZE).len();
        assert_eq!(message.encode().len(), dict_len + 16 * 1024);
    }

    #[test]
    fn piece_count_uses_ceiling_division() {
        assert_eq!(piece_count(0), 0);
        assert_eq!(piece_count(1), 1);
        assert_eq!(piece_count(METADATA_PIECE_SIZE), 1);
        assert_eq!(piece_count(METADATA_PIECE_SIZE + 1), 2);
        assert_eq!(piece_count(MAX_METADATA_SIZE), 256);
    }

    #[test]
    fn max_metadata_size_round_trips() {
        let total = MAX_METADATA_SIZE;
        let last = piece_count(total) - 1;
        let message = MetadataMessage::Data {
            piece: last,
            total_size: total,
            data: vec![0u8; METADATA_PIECE_SIZE as usize],
        };
        assert_eq!(MetadataMessage::decode(&message.encode()).unwrap(), message);
    }

    #[test]
    fn unsupported_message_types_are_rejected() {
        for msg_type in [3i64, 4, 255, -1] {
            let payload = dict(vec![
                ("msg_type", Value::Int(msg_type)),
                ("piece", Value::Int(0)),
            ]);
            assert!(
                matches!(
                    MetadataMessage::decode(&payload).unwrap_err(),
                    Error::UnknownMessageType { .. }
                ),
                "msg_type {msg_type} should be rejected"
            );
        }
    }

    #[test]
    fn missing_keys_are_rejected() {
        let no_type = dict(vec![("piece", Value::Int(0))]);
        assert!(matches!(
            MetadataMessage::decode(&no_type).unwrap_err(),
            Error::MissingKey { key: "msg_type" }
        ));

        let no_piece = dict(vec![("msg_type", Value::Int(0))]);
        assert!(matches!(
            MetadataMessage::decode(&no_piece).unwrap_err(),
            Error::MissingKey { key: "piece" }
        ));

        let no_size = dict(vec![("msg_type", Value::Int(1)), ("piece", Value::Int(0))]);
        assert!(matches!(
            MetadataMessage::decode(&no_size).unwrap_err(),
            Error::MissingKey { key: "total_size" }
        ));
    }

    #[test]
    fn wrong_key_types_are_rejected() {
        let text_type = dict(vec![
            ("msg_type", Value::Bytes(b"1".to_vec())),
            ("piece", Value::Int(0)),
        ]);
        assert!(matches!(
            MetadataMessage::decode(&text_type).unwrap_err(),
            Error::WrongType {
                key: "msg_type",
                ..
            }
        ));

        let text_piece = dict(vec![
            ("msg_type", Value::Int(0)),
            ("piece", Value::Bytes(b"0".to_vec())),
        ]);
        assert!(matches!(
            MetadataMessage::decode(&text_piece).unwrap_err(),
            Error::WrongType { key: "piece", .. }
        ));
    }

    #[test]
    fn total_size_out_of_range_is_rejected() {
        for total in [0i64, i64::from(MAX_METADATA_SIZE) + 1] {
            let payload = data_dict(0, total);
            assert!(matches!(
                MetadataMessage::decode(&payload).unwrap_err(),
                Error::InvalidTotalSize { .. }
            ));
        }

        let negative = data_dict(0, -1);
        assert!(matches!(
            MetadataMessage::decode(&negative).unwrap_err(),
            Error::IntegerOutOfRange {
                key: "total_size",
                ..
            }
        ));
    }

    #[test]
    fn piece_out_of_range_is_rejected() {
        // One byte of metadata has exactly one piece.
        let payload = data_dict(1, 1);
        assert!(matches!(
            MetadataMessage::decode(&payload).unwrap_err(),
            Error::PieceOutOfRange {
                piece: 1,
                pieces: 1
            }
        ));

        let negative = data_dict(-1, 1);
        assert!(matches!(
            MetadataMessage::decode(&negative).unwrap_err(),
            Error::IntegerOutOfRange { key: "piece", .. }
        ));
    }

    #[test]
    fn non_final_short_piece_is_rejected() {
        let mut payload = data_dict(0, 2 * i64::from(METADATA_PIECE_SIZE));
        payload.extend_from_slice(&[0u8; 100]);
        assert!(matches!(
            MetadataMessage::decode(&payload).unwrap_err(),
            Error::InvalidPieceLength {
                piece: 0,
                len: 100,
                expected: 16_384
            }
        ));
    }

    #[test]
    fn missing_piece_bytes_are_rejected() {
        // Complete dictionary, no trailing data at all.
        let payload = data_dict(0, i64::from(METADATA_PIECE_SIZE));
        assert!(matches!(
            MetadataMessage::decode(&payload).unwrap_err(),
            Error::InvalidPieceLength { len: 0, .. }
        ));
    }

    #[test]
    fn final_piece_must_be_exactly_remaining_bytes() {
        let total = i64::from(METADATA_PIECE_SIZE) + 5;

        let mut short = data_dict(1, total);
        short.extend_from_slice(&[0u8; 4]);
        assert!(matches!(
            MetadataMessage::decode(&short).unwrap_err(),
            Error::InvalidPieceLength {
                len: 4,
                expected: 5,
                ..
            }
        ));

        let mut long = data_dict(1, total);
        long.extend_from_slice(&[0u8; 6]);
        assert!(matches!(
            MetadataMessage::decode(&long).unwrap_err(),
            Error::InvalidPieceLength {
                len: 6,
                expected: 5,
                ..
            }
        ));

        let mut exact = data_dict(1, total);
        exact.extend_from_slice(&[0u8; 5]);
        assert!(MetadataMessage::decode(&exact).is_ok());
    }

    #[test]
    fn trailing_garbage_after_request_and_reject_is_rejected() {
        let mut request = request_dict(0);
        request.extend_from_slice(b"junk");
        assert_eq!(
            MetadataMessage::decode(&request).unwrap_err(),
            Error::TrailingData
        );

        let mut reject = dict(vec![("msg_type", Value::Int(2)), ("piece", Value::Int(0))]);
        reject.push(b'x');
        assert_eq!(
            MetadataMessage::decode(&reject).unwrap_err(),
            Error::TrailingData
        );
    }

    #[test]
    fn empty_payload_is_rejected() {
        assert_eq!(
            MetadataMessage::decode(&[]).unwrap_err(),
            Error::EmptyPayload
        );
    }

    #[test]
    fn non_dictionary_payload_is_rejected() {
        let list = bencode::encode(&Value::List(vec![Value::Int(0)]));
        assert!(matches!(
            MetadataMessage::decode(&list).unwrap_err(),
            Error::NotADictionary
        ));
        assert!(matches!(
            MetadataMessage::decode(b"i3e").unwrap_err(),
            Error::NotADictionary
        ));
    }

    #[test]
    fn truncated_payload_is_rejected() {
        let payload = request_dict(3);
        for cut in 1..payload.len() {
            let err = MetadataMessage::decode(&payload[..cut]).unwrap_err();
            assert!(
                matches!(err, Error::Malformed | Error::Bencode(_)),
                "cut at {cut} gave {err:?}"
            );
        }
    }

    #[test]
    fn oversized_integer_is_rejected() {
        let payload = b"d8:msg_typei1e5:piecei0e10:total_sizei99999999999999999999ee";
        let err = MetadataMessage::decode(payload).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Bencode(bencode::Error {
                    kind: ErrorKind::IntegerOverflow,
                    ..
                })
            ),
            "{err:?}"
        );
    }

    #[test]
    fn unsorted_dictionary_is_rejected() {
        // `msg_type` after `piece`: invalid canonical bencode.
        let payload = b"d5:piecei0e8:msg_typei0ee";
        assert!(matches!(
            MetadataMessage::decode(payload).unwrap_err(),
            Error::Bencode(bencode::Error {
                kind: ErrorKind::UnsortedDictKey,
                ..
            })
        ));
    }

    #[test]
    fn decode_rejects_integer_valued_trailing_but_keeps_data_bytes() {
        // The structural scan must stop at the dictionary's own `e`, not at the
        // first `e` in the integer or the piece data.
        let mut payload = data_dict(0, 4);
        payload.extend_from_slice(b"eeee");
        let decoded = MetadataMessage::decode(&payload).unwrap();
        assert_eq!(
            decoded,
            MetadataMessage::Data {
                piece: 0,
                total_size: 4,
                data: b"eeee".to_vec()
            }
        );
    }
}
