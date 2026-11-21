//! Magnet link parsing (BEP 9 / BEP 53).
//!
//! A magnet link names a torrent by hash instead of carrying its metainfo. This
//! module only parses the link; it does not contact trackers, fetch metadata or
//! verify anything. It exists so that a magnet URI can be turned into the
//! hashes, trackers and names the rest of the stack already understands.
//!
//! Parsing is strict and total — hostile input yields an [`Error`], never a
//! guess:
//!
//! - The scheme must be the lowercase `magnet:?`. RFC 3986 says schemes are
//!   case-insensitive, but a magnet link is only ever recognised by that exact
//!   prefix here: anything else is [`Error::NotMagnet`] rather than a lenient
//!   guess about what the caller meant to hand us.
//! - The query is *not* form-encoded: `+` stays a literal plus sign, and only
//!   `%XX` escapes are decoded. An escape with a missing or non-hex digit
//!   (`%`, `%z1`, `%4`) is an error rather than being copied through.
//! - Values are decoded byte-wise and only then interpreted. `dn` must be valid
//!   UTF-8; `tr` and `ws` must be UTF-8 URLs but their scheme is not checked
//!   here (the tracker module owns that).
//! - `xt=urn:btih:<hash>` accepts 40 hex digits in either case or RFC 4648
//!   base32 (A-Z2-7, case-insensitive, `=` padding tolerated). `xt=urn:btmh:`
//!   accepts the BEP 52 multihash form only: 68 hex digits (or the base32 of
//!   those 34 bytes) starting with `0x12 0x20` (sha2-256, 32 bytes).
//! - Unknown `xt` URNs (and `btmh` values using a different multihash) are kept
//!   in [`MagnetLink::unsupported`] instead of failing the link, because a
//!   hybrid link may legitimately carry hashes this version cannot use.
//! - A link with neither a v1 nor a v2 hash is [`Error::NoInfoHash`]. A second
//!   `xt` that names a *different* hash in the same family is
//!   [`Error::ConflictingInfoHash`]; repeating the same hash is harmless.
//!
//! Unknown parameters (`so`, `x.pe`, `xs`, ...) are ignored. Parameter names
//! are matched literally and case-sensitively; only their values are decoded.
//! Repeated `tr`/`ws` values keep their first position and are de-duplicated,
//! while a repeated `xl` takes its last value.
//!
//! Allocations are bounded: the URI may be at most [`MAX_URI_LEN`] bytes and at
//! most [`MAX_TRACKERS`], [`MAX_WEB_SEEDS`] and [`MAX_UNSUPPORTED`] values are
//! kept per list.

/// Largest magnet URI accepted, in bytes.
///
/// Far above any real link — a magnet with dozens of trackers is under 1 KiB —
/// and small enough that a hostile clipboard or CLI argument cannot force a
/// large allocation.
pub const MAX_URI_LEN: usize = 8 * 1024;

/// Largest number of `tr` entries kept; order is preserved.
pub const MAX_TRACKERS: usize = 256;

/// Largest number of `ws` (web seed) entries kept; order is preserved.
pub const MAX_WEB_SEEDS: usize = 256;

/// Largest number of unrecognised `xt` values kept.
pub const MAX_UNSUPPORTED: usize = 64;

/// The torrent identity and hints a magnet URI can carry.
///
/// Every field is optional because a magnet link may omit almost everything:
/// only an info hash is mandatory, and this version requires at least one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MagnetLink {
    /// BEP 3 SHA-1 info hash from `xt=urn:btih:`.
    pub info_hash: Option<[u8; 20]>,
    /// BEP 52 SHA-256 info hash from `xt=urn:btmh:`.
    pub info_hash_v2: Option<[u8; 32]>,
    /// Percent-decoded, UTF-8 display name from `dn`.
    pub display_name: Option<String>,
    /// Percent-decoded tracker URLs from `tr`, in link order, duplicates removed.
    pub trackers: Vec<String>,
    /// Percent-decoded web seed URLs from `ws`, in link order, duplicates removed.
    pub web_seeds: Vec<String>,
    /// Total payload length in bytes from `xl`.
    ///
    /// A repeated `xl` takes the last value; unlike `xt` there is no conflict
    /// error, because the length is a hint that metadata will replace.
    pub exact_length: Option<u64>,
    /// `xt` values that are not hashes this version can use, in link order.
    ///
    /// Kept verbatim (after percent decoding) so a caller can log or forward
    /// them; nothing here is trusted.
    pub unsupported: Vec<String>,
}

impl MagnetLink {
    /// Whether the link carries at least one usable info hash.
    ///
    /// Always `true` for a value returned by [`parse`].
    #[must_use]
    pub fn has_info_hash(&self) -> bool {
        self.info_hash.is_some() || self.info_hash_v2.is_some()
    }
}

/// A magnet parsing failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The URI exceeded [`MAX_URI_LEN`].
    #[error("magnet URI is {len} bytes, over the {max}-byte limit")]
    TooLong {
        /// Observed length in bytes.
        len: usize,
        /// Configured cap.
        max: usize,
    },
    /// The URI did not start with the lowercase `magnet:?` prefix.
    #[error("not a magnet URI: expected a lowercase `magnet:?` prefix")]
    NotMagnet,
    /// A query component had no `=`.
    #[error("query parameter `{param}` has no `=`")]
    MissingValue {
        /// The raw component, bounded by [`MAX_URI_LEN`].
        param: String,
    },
    /// A `%XX` escape was incomplete or not hexadecimal.
    #[error("invalid percent escape in `{param}` at byte {offset}")]
    InvalidEscape {
        /// Parameter whose value contained the escape.
        param: &'static str,
        /// Byte offset of the `%` inside the raw (undecoded) value.
        offset: usize,
    },
    /// A value that must be text was not valid UTF-8 after decoding.
    #[error("`{param}` is not valid UTF-8 after percent decoding")]
    NotUtf8 {
        /// Parameter whose value was not UTF-8.
        param: &'static str,
    },
    /// An `xt` value was present but empty.
    #[error("`xt` is empty")]
    EmptyXt,
    /// An `xt` value named a known URN but its hash was malformed.
    #[error("`xt={value}` is not a valid {kind} info hash")]
    InvalidInfoHash {
        /// The hash family: `btih` or `btmh`.
        kind: &'static str,
        /// The offending value.
        value: String,
    },
    /// Two `xt` values named different hashes in the same family.
    #[error("magnet URI contains conflicting info hashes")]
    ConflictingInfoHash,
    /// No `xt` value produced a hash this version can use.
    #[error("magnet URI contains no usable info hash")]
    NoInfoHash,
    /// An `xl` value was not a non-negative decimal integer.
    #[error("`xl={value}` is not a non-negative decimal integer")]
    InvalidExactLength {
        /// The offending value.
        value: String,
    },
    /// A repeated parameter exceeded its list cap.
    #[error("too many `{param}` entries (limit {max})")]
    TooManyValues {
        /// Parameter name.
        param: &'static str,
        /// Configured cap.
        max: usize,
    },
}

/// Parse a magnet URI.
///
/// # Errors
///
/// Returns the [`Error`] that describes the first problem found: wrong scheme,
/// oversized input, malformed escape, non-UTF-8 text, malformed hash, conflicting
/// hashes, missing hash, malformed `xl`, too many list entries, or a component
/// without `=`.
pub fn parse(uri: &str) -> Result<MagnetLink, Error> {
    if uri.len() > MAX_URI_LEN {
        return Err(Error::TooLong {
            len: uri.len(),
            max: MAX_URI_LEN,
        });
    }
    let query = uri.strip_prefix("magnet:?").ok_or(Error::NotMagnet)?;

    let mut link = MagnetLink::default();
    for component in query.split('&') {
        if component.is_empty() {
            continue;
        }
        let (name, raw) = component
            .split_once('=')
            .ok_or_else(|| Error::MissingValue {
                param: component.to_owned(),
            })?;
        match name {
            "dn" => {
                link.display_name = Some(text(percent_decode(raw.as_bytes(), "dn")?, "dn")?);
            }
            "tr" => {
                let value = text(percent_decode(raw.as_bytes(), "tr")?, "tr")?;
                push_unique(&mut link.trackers, value, MAX_TRACKERS, "tr")?;
            }
            "ws" => {
                let value = text(percent_decode(raw.as_bytes(), "ws")?, "ws")?;
                push_unique(&mut link.web_seeds, value, MAX_WEB_SEEDS, "ws")?;
            }
            "xl" => {
                let value = text(percent_decode(raw.as_bytes(), "xl")?, "xl")?;
                let length = parse_u64(&value).ok_or(Error::InvalidExactLength { value })?;
                link.exact_length = Some(length);
            }
            "xt" => record_xt(&mut link, raw)?,
            // Unknown parameters (`so`, `x.pe`, `xs`, ...) carry nothing this
            // version uses; their values are not decoded, so a stray escape in
            // one cannot reject an otherwise usable link.
            _ => {}
        }
    }

    if !link.has_info_hash() {
        return Err(Error::NoInfoHash);
    }
    Ok(link)
}

/// Add an `xt` value to the link, or record why it is unusable.
fn record_xt(link: &mut MagnetLink, raw: &str) -> Result<(), Error> {
    let value = text(percent_decode(raw.as_bytes(), "xt")?, "xt")?;
    if value.is_empty() {
        return Err(Error::EmptyXt);
    }

    if let Some(body) = strip_prefix_ascii_case(&value, "urn:btih:") {
        let hash = decode_btih(body).ok_or_else(|| Error::InvalidInfoHash {
            kind: "btih",
            value: value.clone(),
        })?;
        if link.info_hash.is_some_and(|existing| existing != hash) {
            return Err(Error::ConflictingInfoHash);
        }
        link.info_hash = Some(hash);
        return Ok(());
    }

    if let Some(body) = strip_prefix_ascii_case(&value, "urn:btmh:") {
        if let Some(hash) = decode_btmh(body) {
            if link.info_hash_v2.is_some_and(|existing| existing != hash) {
                return Err(Error::ConflictingInfoHash);
            }
            link.info_hash_v2 = Some(hash);
            return Ok(());
        }
        // A well-formed URN with a multihash this version does not implement
        // is not an error: another client on the swarm may understand it.
        return push_unique(&mut link.unsupported, value, MAX_UNSUPPORTED, "xt");
    }

    push_unique(&mut link.unsupported, value, MAX_UNSUPPORTED, "xt")
}

/// Push `value` unless an identical one is already present, enforcing `max`.
fn push_unique(
    list: &mut Vec<String>,
    value: String,
    max: usize,
    param: &'static str,
) -> Result<(), Error> {
    if list.contains(&value) {
        return Ok(());
    }
    if list.len() >= max {
        return Err(Error::TooManyValues { param, max });
    }
    list.push(value);
    Ok(())
}

/// Interpret decoded bytes as UTF-8 text, naming the parameter on failure.
fn text(bytes: Vec<u8>, param: &'static str) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| Error::NotUtf8 { param })
}

/// Match an ASCII prefix case-insensitively, returning the remainder.
///
/// `prefix` must be ASCII. The returned slice starts on a `char` boundary
/// because every matched byte is an ASCII byte.
fn strip_prefix_ascii_case<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    let head = input.as_bytes().get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix.as_bytes()) {
        return None;
    }
    input.get(prefix.len()..)
}

/// Decode `%XX` escapes byte-wise, leaving every other byte — including `+` —
/// untouched.
fn percent_decode(raw: &[u8], param: &'static str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(raw.len());
    let mut index = 0;
    while let Some(&byte) = raw.get(index) {
        if byte != b'%' {
            out.push(byte);
            index += 1;
            continue;
        }
        let high = raw
            .get(index + 1)
            .copied()
            .and_then(hex_digit)
            .ok_or(Error::InvalidEscape {
                param,
                offset: index,
            })?;
        let low = raw
            .get(index + 2)
            .copied()
            .and_then(hex_digit)
            .ok_or(Error::InvalidEscape {
                param,
                offset: index,
            })?;
        out.push((high << 4) | low);
        index += 3;
    }
    Ok(out)
}

/// Decode a single hexadecimal digit.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode a string of hex digits; `None` if any byte is not a digit or the
/// length is odd.
fn decode_hex(value: &[u8]) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    for pair in value.as_chunks::<2>().0 {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

/// Decode RFC 4648 base32 (A-Z2-7, either case, `=` padding tolerated).
///
/// Padding is accepted only at the end, the data bits must be a whole number of
/// bytes, and unused trailing bits must be zero. This is deliberately not
/// lenient: a hash is either exactly the bytes it spells or it is rejected.
fn decode_base32(value: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len() * 5 / 8);
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    let mut padded = false;

    for &byte in value {
        if byte == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return None;
        }
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => return None,
        };
        accumulator = (accumulator << 5) | u32::from(digit);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }

    // Leftover bits are the encoder's zero padding; anything else means the
    // string is not a valid base32 encoding of `out`.
    if accumulator != 0 {
        return None;
    }
    Some(out)
}

/// Decode the body of `urn:btih:` into a 20-byte SHA-1 hash.
fn decode_btih(body: &str) -> Option<[u8; 20]> {
    if body.len() == 40
        && let Some(bytes) = decode_hex(body.as_bytes())
    {
        return bytes.try_into().ok();
    }
    decode_base32(body.as_bytes())?.try_into().ok()
}

/// Decode the body of `urn:btmh:` into a 32-byte SHA-256 hash.
///
/// Only the BEP 52 multihash `0x12 0x20 || sha256` is understood; any other
/// code or length yields `None` (the caller records it as unsupported).
fn decode_btmh(body: &str) -> Option<[u8; 32]> {
    let bytes = if body.len() == 68 {
        decode_hex(body.as_bytes())
    } else {
        None
    }
    .or_else(|| decode_base32(body.as_bytes()))?;
    if bytes.len() != 34 || bytes[0] != 0x12 || bytes[1] != 0x20 {
        return None;
    }
    bytes[2..].try_into().ok()
}

/// Parse a non-empty run of ASCII decimal digits, rejecting signs and spaces.
fn parse_u64(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    for &byte in value.as_bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        total = total.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(total)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Independent base32 encoder for building test inputs.
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

    fn base32_encode(bytes: &[u8]) -> String {
        let mut out = String::new();
        let mut accumulator: u32 = 0;
        let mut bits: u32 = 0;
        for &byte in bytes {
            accumulator = (accumulator << 8) | u32::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(ALPHABET[((accumulator >> bits) & 31) as usize] as char);
            }
        }
        if bits > 0 {
            out.push(ALPHABET[((accumulator << (5 - bits)) & 31) as usize] as char);
        }
        out
    }

    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            out.push(DIGITS[usize::from(byte >> 4)] as char);
            out.push(DIGITS[usize::from(byte & 0x0f)] as char);
        }
        out
    }

    const HASH_V1: [u8; 20] = [
        0x9f, 0x4a, 0x5b, 0x2c, 0x11, 0x03, 0xd7, 0x88, 0x64, 0x0e, 0xaa, 0x1c, 0x35, 0x70, 0x02,
        0x4b, 0x8d, 0xe6, 0x91, 0x3f,
    ];
    const HASH_V2: [u8; 32] = [
        0x2a, 0x11, 0x7c, 0x33, 0xd0, 0x4e, 0x95, 0x68, 0x07, 0x1b, 0xee, 0x42, 0x87, 0x5c, 0x9a,
        0x64, 0x3d, 0xf8, 0x21, 0x0c, 0xb5, 0x76, 0x9e, 0x4a, 0x03, 0x88, 0xcf, 0x12, 0x65, 0xab,
        0xd4, 0x70,
    ];

    fn btih_hex() -> String {
        format!("xt=urn:btih:{}", hex(&HASH_V1))
    }

    fn btmh_multihash() -> Vec<u8> {
        let mut bytes = vec![0x12, 0x20];
        bytes.extend_from_slice(&HASH_V2);
        bytes
    }

    #[test]
    fn btih_hex_lowercase() {
        let link = parse(&format!("magnet:?{}", btih_hex())).unwrap();
        assert_eq!(link.info_hash, Some(HASH_V1));
        assert_eq!(link.info_hash_v2, None);
        assert!(link.has_info_hash());
    }

    #[test]
    fn btih_hex_uppercase() {
        let uri = format!("magnet:?xt=urn:btih:{}", hex(&HASH_V1).to_uppercase());
        assert_eq!(parse(&uri).unwrap().info_hash, Some(HASH_V1));
    }

    #[test]
    fn btih_base32_without_padding() {
        let body = base32_encode(&HASH_V1);
        assert_eq!(body.len(), 32);
        let link = parse(&format!("magnet:?xt=urn:btih:{body}")).unwrap();
        assert_eq!(link.info_hash, Some(HASH_V1));
    }

    #[test]
    fn btih_base32_with_padding() {
        // 32 characters for 20 bytes is already a whole number of symbols; pad
        // it out to the 8-symbol block to prove extra `=` is tolerated.
        let body = format!("{}====", base32_encode(&HASH_V1));
        let link = parse(&format!("magnet:?xt=urn:btih:{body}")).unwrap();
        assert_eq!(link.info_hash, Some(HASH_V1));
    }

    #[test]
    fn btih_base32_lowercase_is_accepted() {
        let body = base32_encode(&HASH_V1).to_lowercase();
        let link = parse(&format!("magnet:?xt=urn:btih:{body}")).unwrap();
        assert_eq!(link.info_hash, Some(HASH_V1));
    }

    #[test]
    fn btih_base32_nonzero_trailing_bits_are_rejected() {
        // 33 symbols carry 165 bits: 20 bytes plus 5 stray bits, which must be
        // zero for a valid encoding.
        let body = format!("{}Z", base32_encode(&HASH_V1));
        let err = parse(&format!("magnet:?xt=urn:btih:{body}")).unwrap_err();
        assert!(matches!(err, Error::InvalidInfoHash { kind: "btih", .. }));
    }

    #[test]
    fn btmh_hex() {
        let uri = format!("magnet:?xt=urn:btmh:{}", hex(&btmh_multihash()));
        let link = parse(&uri).unwrap();
        assert_eq!(link.info_hash_v2, Some(HASH_V2));
        assert_eq!(link.info_hash, None);
    }

    #[test]
    fn btmh_base32() {
        let body = base32_encode(&btmh_multihash());
        let link = parse(&format!("magnet:?xt=urn:btmh:{body}")).unwrap();
        assert_eq!(link.info_hash_v2, Some(HASH_V2));
    }

    #[test]
    fn btmh_other_multihash_is_unsupported_not_fatal() {
        // 0x16 0x20 is a sha3-256 multihash: a valid URN this version cannot use.
        let mut bytes = vec![0x16, 0x20];
        bytes.extend_from_slice(&HASH_V2);
        let uri = format!("magnet:?xt=urn:btmh:{}&{}", hex(&bytes), btih_hex());
        let link = parse(&uri).unwrap();
        assert_eq!(link.info_hash, Some(HASH_V1));
        assert_eq!(link.info_hash_v2, None);
        assert_eq!(link.unsupported.len(), 1);
        assert!(link.unsupported[0].starts_with("urn:btmh:"));
    }

    #[test]
    fn hybrid_link_keeps_both_hashes() {
        let uri = format!(
            "magnet:?xt=urn:btih:{}&xt=urn:btmh:{}",
            hex(&HASH_V1),
            hex(&btmh_multihash())
        );
        let link = parse(&uri).unwrap();
        assert_eq!(link.info_hash, Some(HASH_V1));
        assert_eq!(link.info_hash_v2, Some(HASH_V2));
    }

    #[test]
    fn repeated_identical_xt_is_harmless() {
        let uri = format!("magnet:?{}&{}", btih_hex(), btih_hex());
        assert_eq!(parse(&uri).unwrap().info_hash, Some(HASH_V1));
    }

    #[test]
    fn percent_encoded_xt_is_decoded_before_matching() {
        let uri = format!("magnet:?xt=urn%3Abtih%3A{}", hex(&HASH_V1).to_uppercase());
        assert_eq!(parse(&uri).unwrap().info_hash, Some(HASH_V1));
    }

    #[test]
    fn multiple_trackers_keep_order_and_drop_duplicates() {
        let uri = concat!(
            "magnet:?xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f",
            "&tr=udp%3A%2F%2Ftracker.example%3A6969%2Fannounce",
            "&tr=http%3A%2F%2Fother.example%2Fa",
            "&tr=udp%3A%2F%2Ftracker.example%3A6969%2Fannounce",
            "&ws=http%3A%2F%2Fseed.example%2Ffile",
        );
        let link = parse(uri).unwrap();
        assert_eq!(
            link.trackers,
            vec![
                "udp://tracker.example:6969/announce".to_owned(),
                "http://other.example/a".to_owned(),
            ]
        );
        assert_eq!(link.web_seeds, vec!["http://seed.example/file".to_owned()]);
    }

    #[test]
    fn display_name_with_utf8() {
        let uri = "magnet:?dn=%D0%A2%D0%BE%D1%80%D1%80%D0%B5%D0%BD%D1%82%20%E2%9C%93\
                   &xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f";
        let link = parse(uri).unwrap();
        assert_eq!(link.display_name.as_deref(), Some("Торрент ✓"));
    }

    #[test]
    fn display_name_must_be_utf8() {
        let uri = "magnet:?dn=%FF%FE&xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f";
        assert!(matches!(
            parse(uri).unwrap_err(),
            Error::NotUtf8 { param: "dn" }
        ));
    }

    #[test]
    fn invalid_percent_escapes_are_rejected() {
        for bad in ["%zz", "%", "%4", "%4g"] {
            let uri = format!(
                "magnet:?tr=http%3A%2F%2Fx%2F{bad}&xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f"
            );
            match parse(&uri).unwrap_err() {
                Error::InvalidEscape { param: "tr", .. } => {}
                other => panic!("unexpected error for {bad}: {other:?}"),
            }
        }
    }

    #[test]
    fn plus_sign_is_literal_in_a_magnet_query() {
        let uri = "magnet:?dn=a+b%20c&xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f";
        let link = parse(uri).unwrap();
        assert_eq!(link.display_name.as_deref(), Some("a+b c"));

        let uri =
            "magnet:?tr=http%3A%2F%2Fx%2Fa+b&xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f";
        assert_eq!(parse(uri).unwrap().trackers, vec!["http://x/a+b"]);
    }

    #[test]
    fn missing_xt_is_an_error() {
        let err = parse("magnet:?dn=name&tr=udp%3A%2F%2Fx").unwrap_err();
        assert!(matches!(err, Error::NoInfoHash));
    }

    #[test]
    fn bare_string_is_not_a_magnet() {
        assert!(matches!(
            parse("9f4a5b2c1103d788640eaa1c3570024b8de6913f").unwrap_err(),
            Error::NotMagnet
        ));
        assert!(matches!(
            parse("magnet:9f4a").unwrap_err(),
            Error::NotMagnet
        ));
    }

    #[test]
    fn scheme_is_required_lowercase() {
        let uri = format!("MAGNET:?xt=urn:btih:{}", hex(&HASH_V1));
        assert!(matches!(parse(&uri).unwrap_err(), Error::NotMagnet));
        let uri = format!("Magnet:?xt=urn:btih:{}", hex(&HASH_V1));
        assert!(matches!(parse(&uri).unwrap_err(), Error::NotMagnet));
    }

    #[test]
    fn conflicting_btih_is_rejected() {
        let mut other = HASH_V1;
        other[0] ^= 0xff;
        let uri = format!("magnet:?{}&xt=urn:btih:{}", btih_hex(), hex(&other));
        assert!(matches!(
            parse(&uri).unwrap_err(),
            Error::ConflictingInfoHash
        ));
    }

    #[test]
    fn conflicting_btmh_is_rejected() {
        let mut other = HASH_V2;
        other[31] ^= 0xff;
        let mut multihash = vec![0x12, 0x20];
        multihash.extend_from_slice(&other);
        let uri = format!(
            "magnet:?xt=urn:btmh:{}&xt=urn:btmh:{}",
            hex(&btmh_multihash()),
            hex(&multihash)
        );
        assert!(matches!(
            parse(&uri).unwrap_err(),
            Error::ConflictingInfoHash
        ));
    }

    #[test]
    fn oversized_uri_is_rejected_before_parsing() {
        let uri = format!(
            "magnet:?xt=urn:btih:{}&tr={}",
            hex(&HASH_V1),
            "a".repeat(MAX_URI_LEN)
        );
        assert!(matches!(
            parse(&uri).unwrap_err(),
            Error::TooLong {
                max: MAX_URI_LEN,
                ..
            }
        ));
    }

    #[test]
    fn unknown_xt_is_collected() {
        let uri = format!(
            "magnet:?xt=urn:ed2k:31D6CFE0D16AE931B73C59D7E0C089C0&{}&xt=urn:sha1:ABCDEF",
            btih_hex()
        );
        let link = parse(&uri).unwrap();
        assert_eq!(link.info_hash, Some(HASH_V1));
        assert_eq!(
            link.unsupported,
            vec![
                "urn:ed2k:31D6CFE0D16AE931B73C59D7E0C089C0".to_owned(),
                "urn:sha1:ABCDEF".to_owned(),
            ]
        );
    }

    #[test]
    fn unknown_xt_alone_is_no_info_hash() {
        let err = parse("magnet:?xt=urn:sha1:ABCDEF").unwrap_err();
        assert!(matches!(err, Error::NoInfoHash));
    }

    #[test]
    fn empty_xt_is_rejected() {
        assert!(matches!(parse("magnet:?xt=").unwrap_err(), Error::EmptyXt));
    }

    #[test]
    fn query_order_does_not_matter() {
        let a = "magnet:?dn=n&xl=42&tr=http%3A%2F%2Fa%2Fannounce\
                 &xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f&ws=http%3A%2F%2Fw";
        let b = "magnet:?ws=http%3A%2F%2Fw&xt=urn:btih:9f4a5b2c1103d788640eaa1c3570024b8de6913f\
                 &tr=http%3A%2F%2Fa%2Fannounce&xl=42&dn=n";
        let left = parse(a).unwrap();
        let right = parse(b).unwrap();
        assert_eq!(left, right);
        assert_eq!(left.exact_length, Some(42));
    }

    #[test]
    fn exact_length_is_parsed_and_validated() {
        let link = parse(&format!("magnet:?{}&xl=0", btih_hex())).unwrap();
        assert_eq!(link.exact_length, Some(0));

        let link = parse(&format!("magnet:?{}&xl=18446744073709551615", btih_hex())).unwrap();
        assert_eq!(link.exact_length, Some(u64::MAX));

        for bad in ["18446744073709551616", "-1", "+5", "1 ", "0x10", ""] {
            let uri = format!("magnet:?{}&xl={bad}", btih_hex());
            assert!(
                matches!(parse(&uri).unwrap_err(), Error::InvalidExactLength { .. }),
                "`xl={bad}` should be rejected"
            );
        }
    }

    #[test]
    fn repeated_xl_takes_the_last_value() {
        let link = parse(&format!("magnet:?{}&xl=1&xl=2", btih_hex())).unwrap();
        assert_eq!(link.exact_length, Some(2));
    }

    #[test]
    fn parameter_without_equals_is_rejected() {
        let uri = format!("magnet:?{}&dn", btih_hex());
        assert!(matches!(
            parse(&uri).unwrap_err(),
            Error::MissingValue { .. }
        ));
    }

    #[test]
    fn empty_components_are_skipped() {
        let uri = format!("magnet:?&{}&", btih_hex());
        assert_eq!(parse(&uri).unwrap().info_hash, Some(HASH_V1));
    }

    #[test]
    fn malformed_btih_is_rejected() {
        for bad in ["", "zz", "9f4a5b2c1103d788640eaa1c3570024b8de6913", "0"] {
            let uri = format!("magnet:?xt=urn:btih:{bad}");
            assert!(
                matches!(
                    parse(&uri).unwrap_err(),
                    Error::InvalidInfoHash { kind: "btih", .. }
                ),
                "`urn:btih:{bad}` should be rejected"
            );
        }
    }

    #[test]
    fn malformed_btmh_is_unsupported() {
        // Right length, wrong multihash prefix and therefore not usable.
        let body = format!("00{}", hex(&HASH_V2));
        let uri = format!("magnet:?xt=urn:btmh:{body}&{}", btih_hex());
        let link = parse(&uri).unwrap();
        assert_eq!(link.info_hash_v2, None);
        assert_eq!(link.unsupported.len(), 1);
    }

    #[test]
    fn tracker_cap_is_enforced() {
        let mut uri = format!("magnet:?{}", btih_hex());
        for index in 0..=MAX_TRACKERS {
            uri.push_str(&format!("&tr=udp%3A%2F%2Ft{index}"));
        }
        assert!(matches!(
            parse(&uri).unwrap_err(),
            Error::TooManyValues {
                param: "tr",
                max: MAX_TRACKERS
            }
        ));
    }

    #[test]
    fn tracker_cap_counts_unique_values_only() {
        let mut uri = format!("magnet:?{}", btih_hex());
        for _ in 0..=MAX_TRACKERS {
            uri.push_str("&tr=udp%3A%2F%2Fsame");
        }
        let link = parse(&uri).unwrap();
        assert_eq!(link.trackers, vec!["udp://same".to_owned()]);
    }

    #[test]
    fn unsupported_cap_is_enforced() {
        let mut uri = format!("magnet:?{}", btih_hex());
        for index in 0..=MAX_UNSUPPORTED {
            uri.push_str(&format!("&xt=urn:sha1:{index}"));
        }
        assert!(matches!(
            parse(&uri).unwrap_err(),
            Error::TooManyValues {
                param: "xt",
                max: MAX_UNSUPPORTED
            }
        ));
    }
}
