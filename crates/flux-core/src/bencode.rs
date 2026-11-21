//! Strict bencode decoding and canonical encoding.
//!
//! Bencode is the wire and metadata format of BitTorrent, and every byte of it
//! arrives from an untrusted peer or tracker. The decoder here is deliberately
//! strict and allocates only after a length has been validated against both the
//! configured limit and the bytes actually present in the input.
//!
//! Canonical form (sorted dictionary keys, minimal integer representation) is
//! enforced on input and produced on output. Because the info dictionary of a
//! torrent is hashed byte-for-byte, callers that need the original bytes should
//! use [`decode_with_spans`] and hash the slice rather than re-encoding.

use std::cmp::Ordering;

/// Decoder resource limits.
///
/// The defaults are generous for `.torrent` metadata and conservative for a
/// hostile network: 8 MiB per string, 32 nested containers, 64 MiB of input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest single byte string (or dictionary key) that may be decoded.
    pub max_string_len: usize,
    /// Largest number of nested lists/dictionaries below the root value.
    pub max_depth: usize,
    /// Largest input buffer that will be decoded at all.
    pub max_input_len: usize,
}

impl Limits {
    /// Build a custom set of limits.
    pub const fn new(max_string_len: usize, max_depth: usize, max_input_len: usize) -> Self {
        Self {
            max_string_len,
            max_depth,
            max_input_len,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_string_len: 8 * 1024 * 1024,
            max_depth: 32,
            max_input_len: 64 * 1024 * 1024,
        }
    }
}

/// A decoded bencode value.
///
/// Dictionary entries carry string keys. The decoder guarantees that the keys
/// are sorted and unique; the canonical encoder sorts keys again, so a
/// hand-built dictionary is encoded canonically regardless of insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A signed 64-bit integer.
    Int(i64),
    /// An arbitrary byte string.
    Bytes(Vec<u8>),
    /// A list of values.
    List(Vec<Value>),
    /// A dictionary whose keys are byte strings.
    Dict(Vec<(Vec<u8>, Value)>),
}

impl Value {
    /// Look up a dictionary entry by key.
    ///
    /// Returns `None` for non-dictionaries. The lookup is linear: torrent
    /// dictionaries are small, and the decoder already validated their order.
    pub fn get(&self, key: &[u8]) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries
                .iter()
                .find(|(candidate, _)| candidate.as_slice() == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// Borrow this value as an integer.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Borrow this value as a byte string.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(value) => Some(value),
            _ => None,
        }
    }

    /// Borrow this value as a list.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(values) => Some(values),
            _ => None,
        }
    }

    /// Borrow this value as a dictionary's entries.
    pub fn as_dict(&self) -> Option<&[(Vec<u8>, Value)]> {
        match self {
            Value::Dict(entries) => Some(entries),
            _ => None,
        }
    }
}

/// A byte range inside the decoded input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Inclusive start offset.
    pub start: usize,
    /// Exclusive end offset.
    pub end: usize,
}

impl Span {
    /// Build a span from its bounds.
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Slice the original input with this span.
    ///
    /// Returns an empty slice if the span is out of bounds, which cannot happen
    /// for spans produced by the decoder.
    pub fn get<'a>(&self, input: &'a [u8]) -> &'a [u8] {
        input.get(self.start..self.end).unwrap_or_default()
    }
}

/// A decoded value plus the byte ranges of the entries of a top-level dictionary.
///
/// This exists so that callers can hash the *original* info dictionary bytes.
/// Re-encoding a decoded-and-re-encoded structure happens to produce the same
/// bytes only if the input was fully canonical, and hashing must not depend on
/// that assumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The decoded value.
    pub value: Value,
    /// Top-level dictionary entries paired with their value spans.
    pub spans: Vec<(Vec<u8>, Span)>,
}

impl Decoded {
    /// Find the byte span of a value stored under `key` in the top-level dictionary.
    pub fn span_of(&self, key: &[u8]) -> Option<Span> {
        self.spans
            .iter()
            .find(|(candidate, _)| candidate.as_slice() == key)
            .map(|(_, span)| *span)
    }
}

/// The category of a decoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ErrorKind {
    /// Input ended in the middle of a value.
    #[error("unexpected end of input")]
    UnexpectedEof,
    /// A byte appeared where the grammar did not allow it.
    #[error("invalid token")]
    InvalidToken,
    /// A string length or integer had a leading zero.
    #[error("leading zero")]
    LeadingZero,
    /// The integer `-0` was encoded.
    #[error("negative zero is not a valid integer")]
    NegativeZero,
    /// An integer did not fit in `i64`.
    #[error("integer overflow")]
    IntegerOverflow,
    /// A string length was negative.
    #[error("negative string length")]
    NegativeStringLength,
    /// A string or dictionary key exceeded the configured limit.
    #[error("string exceeds the configured length limit")]
    OversizedString,
    /// Bytes followed the top-level value.
    #[error("trailing bytes after the top-level value")]
    TrailingBytes,
    /// A dictionary contained the same key twice.
    #[error("duplicate dictionary key")]
    DuplicateDictKey,
    /// Dictionary keys were not in ascending byte order.
    #[error("dictionary keys are not sorted")]
    UnsortedDictKey,
    /// A dictionary key was not a byte string.
    #[error("dictionary key is not a string")]
    DictKeyNotString,
    /// Nesting exceeded the configured limit.
    #[error("nesting exceeds the configured depth limit")]
    DepthExceeded,
    /// The input buffer itself exceeded the configured limit.
    #[error("input exceeds the configured size limit")]
    InputTooLarge,
}

/// A decoding failure with the byte offset at which it was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{kind} at byte {offset}")]
pub struct Error {
    /// What went wrong.
    pub kind: ErrorKind,
    /// Offset into the input at which the problem was detected.
    pub offset: usize,
}

impl Error {
    /// Build an error.
    pub const fn new(kind: ErrorKind, offset: usize) -> Self {
        Self { kind, offset }
    }
}

/// Decode a single value using the default [`Limits`].
pub fn decode(input: &[u8]) -> Result<Value, Error> {
    decode_with_limits(input, &Limits::default())
}

/// Decode a single value with explicit limits.
pub fn decode_with_limits(input: &[u8], limits: &Limits) -> Result<Value, Error> {
    Ok(decode_with_spans(input, limits)?.value)
}

/// Decode a single value and record the byte spans of top-level dictionary entries.
///
/// The spans refer to `input`; they are what callers must hash, never a
/// re-encoding of the returned [`Value`].
pub fn decode_with_spans(input: &[u8], limits: &Limits) -> Result<Decoded, Error> {
    Decoder::new(input, limits).run()
}

/// Encode a value in canonical form.
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

/// Append a value to `out` in canonical form.
///
/// Dictionary keys are emitted in ascending byte order; integers use their
/// minimal decimal representation.
pub fn encode_into(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Int(number) => {
            out.push(b'i');
            out.extend_from_slice(number.to_string().as_bytes());
            out.push(b'e');
        }
        Value::Bytes(bytes) => encode_bytes(bytes, out),
        Value::List(values) => {
            out.push(b'l');
            for value in values {
                encode_into(value, out);
            }
            out.push(b'e');
        }
        Value::Dict(entries) => {
            out.push(b'd');
            let mut order: Vec<usize> = (0..entries.len()).collect();
            order.sort_by(|&left, &right| entries[left].0.cmp(&entries[right].0));
            for index in order {
                encode_bytes(&entries[index].0, out);
                encode_into(&entries[index].1, out);
            }
            out.push(b'e');
        }
    }
}

fn encode_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    limits: &'a Limits,
    spans: Vec<(Vec<u8>, Span)>,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8], limits: &'a Limits) -> Self {
        Self {
            input,
            pos: 0,
            limits,
            spans: Vec::new(),
        }
    }

    fn run(mut self) -> Result<Decoded, Error> {
        if self.input.len() > self.limits.max_input_len {
            return Err(Error::new(ErrorKind::InputTooLarge, 0));
        }
        let value = self.value(0)?;
        if self.pos != self.input.len() {
            return Err(Error::new(ErrorKind::TrailingBytes, self.pos));
        }
        Ok(Decoded {
            value,
            spans: self.spans,
        })
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn err(&self, kind: ErrorKind) -> Error {
        Error::new(kind, self.pos)
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        match self.peek() {
            Some(b'i') => Ok(Value::Int(self.integer()?)),
            Some(b'l') => self.list(depth),
            Some(b'd') => self.dict(depth),
            Some(b'-' | b'0'..=b'9') => Ok(Value::Bytes(self.bytes()?.to_vec())),
            Some(_) => Err(self.err(ErrorKind::InvalidToken)),
            None => Err(self.err(ErrorKind::UnexpectedEof)),
        }
    }

    fn integer(&mut self) -> Result<i64, Error> {
        let start = self.pos;
        self.pos += 1; // 'i'
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
        }
        let digits_start = self.pos;
        let mut magnitude: u64 = 0;
        let mut digit_count = 0usize;

        loop {
            match self.peek() {
                Some(byte @ b'0'..=b'9') => {
                    let digit = u64::from(byte - b'0');
                    if digit_count == 0 && digit == 0 {
                        match self.input.get(self.pos + 1) {
                            Some(b'0'..=b'9') => return Err(self.err(ErrorKind::LeadingZero)),
                            Some(b'e') if negative => {
                                return Err(Error::new(ErrorKind::NegativeZero, start));
                            }
                            _ => {}
                        }
                    }
                    magnitude = magnitude
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or_else(|| Error::new(ErrorKind::IntegerOverflow, start))?;
                    digit_count += 1;
                    self.pos += 1;
                }
                Some(b'e') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => return Err(self.err(ErrorKind::InvalidToken)),
                None => return Err(self.err(ErrorKind::UnexpectedEof)),
            }
        }

        if digit_count == 0 {
            return Err(Error::new(ErrorKind::InvalidToken, digits_start));
        }

        if negative {
            if magnitude == 0 {
                return Err(Error::new(ErrorKind::NegativeZero, start));
            }
            const I64_MIN_MAGNITUDE: u64 = (i64::MAX as u64) + 1;
            if magnitude > I64_MIN_MAGNITUDE {
                return Err(Error::new(ErrorKind::IntegerOverflow, start));
            }
            if magnitude == I64_MIN_MAGNITUDE {
                Ok(i64::MIN)
            } else {
                Ok(-(magnitude as i64))
            }
        } else {
            if magnitude > i64::MAX as u64 {
                return Err(Error::new(ErrorKind::IntegerOverflow, start));
            }
            Ok(magnitude as i64)
        }
    }

    /// Decode a length-prefixed byte string.
    ///
    /// The declared length is validated against `max_string_len` and against the
    /// bytes remaining in the input before any allocation or slicing happens.
    fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let start = self.pos;
        let mut len: usize = 0;
        let mut digit_count = 0usize;

        loop {
            match self.peek() {
                Some(b':') => break,
                Some(b'-') => {
                    return Err(Error::new(ErrorKind::NegativeStringLength, start));
                }
                Some(byte @ b'0'..=b'9') => {
                    let digit = usize::from(byte - b'0');
                    if digit_count == 0 && digit == 0 {
                        match self.input.get(self.pos + 1) {
                            Some(b':') | None => {}
                            Some(_) => return Err(self.err(ErrorKind::LeadingZero)),
                        }
                    }
                    len = len
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or_else(|| Error::new(ErrorKind::OversizedString, start))?;
                    digit_count += 1;
                    self.pos += 1;
                }
                Some(_) => return Err(self.err(ErrorKind::InvalidToken)),
                None => return Err(self.err(ErrorKind::UnexpectedEof)),
            }
        }

        self.pos += 1; // ':'
        if digit_count == 0 {
            return Err(Error::new(ErrorKind::InvalidToken, self.pos - 1));
        }
        if len > self.limits.max_string_len {
            return Err(Error::new(ErrorKind::OversizedString, start));
        }
        // `len` is bounded by `max_string_len` and `pos` by the input length, so
        // this cannot overflow in practice; keep the check for 32-bit targets.
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| Error::new(ErrorKind::OversizedString, start))?;
        if end > self.input.len() {
            return Err(self.err(ErrorKind::UnexpectedEof));
        }
        let bytes = &self.input[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn list(&mut self, depth: usize) -> Result<Value, Error> {
        if depth >= self.limits.max_depth {
            return Err(self.err(ErrorKind::DepthExceeded));
        }
        self.pos += 1; // 'l'
        let mut values = Vec::new();
        loop {
            match self.peek() {
                Some(b'e') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => values.push(self.value(depth + 1)?),
                None => return Err(self.err(ErrorKind::UnexpectedEof)),
            }
        }
        Ok(Value::List(values))
    }

    fn dict(&mut self, depth: usize) -> Result<Value, Error> {
        if depth >= self.limits.max_depth {
            return Err(self.err(ErrorKind::DepthExceeded));
        }
        self.pos += 1; // 'd'
        let mut entries: Vec<(Vec<u8>, Value)> = Vec::new();
        loop {
            match self.peek() {
                Some(b'e') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    if !matches!(self.peek(), Some(b'0'..=b'9')) {
                        return Err(self.err(ErrorKind::DictKeyNotString));
                    }
                    let key_start = self.pos;
                    let key = self.bytes()?.to_vec();
                    if let Some((previous, _)) = entries.last() {
                        match key.as_slice().cmp(previous.as_slice()) {
                            Ordering::Equal => {
                                return Err(Error::new(ErrorKind::DuplicateDictKey, key_start));
                            }
                            Ordering::Less => {
                                return Err(Error::new(ErrorKind::UnsortedDictKey, key_start));
                            }
                            Ordering::Greater => {}
                        }
                    }
                    let value_start = self.pos;
                    let value = self.value(depth + 1)?;
                    let value_end = self.pos;
                    if depth == 0 {
                        self.spans
                            .push((key.clone(), Span::new(value_start, value_end)));
                    }
                    entries.push((key, value));
                }
                None => return Err(self.err(ErrorKind::UnexpectedEof)),
            }
        }
        Ok(Value::Dict(entries))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn decode_err(input: &[u8]) -> Error {
        decode(input).unwrap_err()
    }

    fn decode_with(input: &[u8], limits: Limits) -> Result<Value, Error> {
        decode_with_limits(input, &limits)
    }

    #[test]
    fn decodes_golden_vectors() {
        let cases: &[(&[u8], Value)] = &[
            (b"i42e", Value::Int(42)),
            (b"i-42e", Value::Int(-42)),
            (b"i0e", Value::Int(0)),
            (b"i-9223372036854775808e", Value::Int(i64::MIN)),
            (b"i9223372036854775807e", Value::Int(i64::MAX)),
            (b"4:spam", Value::Bytes(b"spam".to_vec())),
            (b"0:", Value::Bytes(Vec::new())),
            (b"le", Value::List(Vec::new())),
            (
                b"l4:spami42ee",
                Value::List(vec![Value::Bytes(b"spam".to_vec()), Value::Int(42)]),
            ),
            (b"de", Value::Dict(Vec::new())),
            (
                b"d3:bar4:spam3:fooi42ee",
                Value::Dict(vec![
                    (b"bar".to_vec(), Value::Bytes(b"spam".to_vec())),
                    (b"foo".to_vec(), Value::Int(42)),
                ]),
            ),
            (
                b"d4:spaml1:a1:bee",
                Value::Dict(vec![(
                    b"spam".to_vec(),
                    Value::List(vec![
                        Value::Bytes(b"a".to_vec()),
                        Value::Bytes(b"b".to_vec()),
                    ]),
                )]),
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(&decode(input).unwrap(), expected, "input {:?}", input);
        }
    }

    #[test]
    fn binary_strings_are_preserved() {
        let input = b"3:\x00\xff\x01";
        assert_eq!(decode(input).unwrap(), Value::Bytes(vec![0x00, 0xff, 0x01]));
    }

    #[test]
    fn encodes_canonically() {
        let value = Value::Dict(vec![
            (b"z".to_vec(), Value::Int(1)),
            (b"a".to_vec(), Value::Int(-2)),
            (b"m".to_vec(), Value::Bytes(b"x".to_vec())),
        ]);
        assert_eq!(encode(&value), b"d1:ai-2e1:m1:x1:zi1ee");
    }

    #[test]
    fn encoding_round_trips() {
        let value = Value::Dict(vec![
            (b"announce".to_vec(), Value::Bytes(b"http://t/".to_vec())),
            (
                b"list".to_vec(),
                Value::List(vec![Value::Int(0), Value::List(vec![])]),
            ),
        ]);
        let encoded = encode(&value);
        assert_eq!(decode(&encoded).unwrap(), value);
        assert_eq!(encode(&decode(&encoded).unwrap()), encoded);
    }

    #[test]
    fn rejects_duplicate_dict_keys() {
        let error = decode_err(b"d1:ai1e1:ai2ee");
        assert_eq!(error.kind, ErrorKind::DuplicateDictKey);
        assert_eq!(error.offset, 7);
    }

    #[test]
    fn rejects_unsorted_dict_keys() {
        let error = decode_err(b"d1:bi1e1:ai2ee");
        assert_eq!(error.kind, ErrorKind::UnsortedDictKey);
        assert_eq!(error.offset, 7);
    }

    #[test]
    fn rejects_non_string_dict_keys() {
        assert_eq!(decode_err(b"di1ei2ee").kind, ErrorKind::DictKeyNotString);
    }

    #[test]
    fn rejects_leading_zeros() {
        assert_eq!(decode_err(b"i03e").kind, ErrorKind::LeadingZero);
        assert_eq!(decode_err(b"i00e").kind, ErrorKind::LeadingZero);
        assert_eq!(decode_err(b"03:abc").kind, ErrorKind::LeadingZero);
    }

    #[test]
    fn rejects_negative_zero() {
        assert_eq!(decode_err(b"i-0e").kind, ErrorKind::NegativeZero);
    }

    #[test]
    fn rejects_integer_overflow() {
        assert_eq!(
            decode_err(b"i9223372036854775808e").kind,
            ErrorKind::IntegerOverflow
        );
        assert_eq!(
            decode_err(b"i-9223372036854775809e").kind,
            ErrorKind::IntegerOverflow
        );
        assert_eq!(
            decode_err(b"i999999999999999999999999999999e").kind,
            ErrorKind::IntegerOverflow
        );
    }

    #[test]
    fn rejects_truncated_input() {
        for input in [
            b"4:sp".as_slice(),
            b"l4:spam".as_slice(),
            b"d1:a".as_slice(),
            b"i42".as_slice(),
            b"i".as_slice(),
            b"".as_slice(),
        ] {
            assert_eq!(
                decode_err(input).kind,
                ErrorKind::UnexpectedEof,
                "input {:?}",
                input
            );
        }
    }

    #[test]
    fn rejects_trailing_bytes() {
        let error = decode_err(b"i1ei2e");
        assert_eq!(error.kind, ErrorKind::TrailingBytes);
        assert_eq!(error.offset, 3);
        assert_eq!(decode_err(b"0:x").kind, ErrorKind::TrailingBytes);
    }

    #[test]
    fn rejects_negative_string_length() {
        assert_eq!(decode_err(b"-1:a").kind, ErrorKind::NegativeStringLength);
    }

    #[test]
    fn rejects_oversized_strings_without_allocating() {
        let limits = Limits::new(2, 8, 1024);
        assert_eq!(
            decode_with(b"3:abc", limits).unwrap_err().kind,
            ErrorKind::OversizedString
        );
        assert_eq!(
            decode_err(b"18446744073709551616:").kind,
            ErrorKind::OversizedString
        );
        assert_eq!(
            decode_err(b"18446744073709551616:abc").kind,
            ErrorKind::OversizedString
        );
    }

    #[test]
    fn string_length_beyond_input_is_truncation() {
        assert_eq!(decode_err(b"100:abc").kind, ErrorKind::UnexpectedEof);
    }

    #[test]
    fn rejects_excessive_depth() {
        let limits = Limits::new(1024, 2, 1024);
        assert!(decode_with(b"llleee", limits).is_err());
        assert_eq!(
            decode_with(b"llleee", limits).unwrap_err().kind,
            ErrorKind::DepthExceeded
        );
        assert!(decode_with(b"llelee", limits).is_ok());
    }

    #[test]
    fn rejects_oversized_input() {
        let limits = Limits::new(1024, 8, 2);
        let error = decode_with(b"i1e", limits).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InputTooLarge);
        assert_eq!(error.offset, 0);
    }

    #[test]
    fn rejects_invalid_tokens() {
        for input in [
            b"x".as_slice(),
            b"i1xe".as_slice(),
            b"l!e".as_slice(),
            b"d!ei1ee".as_slice(),
            b"ie".as_slice(),
            b"i-e".as_slice(),
            b"+1:a".as_slice(),
        ] {
            assert!(
                matches!(
                    decode_err(input).kind,
                    ErrorKind::InvalidToken | ErrorKind::DictKeyNotString
                ),
                "input {:?}",
                input
            );
        }
    }

    #[test]
    fn spans_point_at_original_bytes() {
        let input = b"d4:infod4:name4:testee";
        let decoded = decode_with_spans(input, &Limits::default()).unwrap();
        let span = decoded.span_of(b"info").unwrap();
        assert_eq!(span.get(input), b"d4:name4:teste");
        assert_eq!(decoded.span_of(b"missing"), None);
    }

    #[test]
    fn span_lookup_works_for_non_canonical_key_text() {
        let input = b"d1:zi1ee";
        let decoded = decode_with_spans(input, &Limits::default()).unwrap();
        assert_eq!(decoded.span_of(b"z").unwrap().get(input), b"i1e");
    }
}
