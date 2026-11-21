//! Torrent identity.
//!
//! A torrent is identified by its info hash. Hybrid torrents carry both a v1 and a
//! v2 hash; the v1 (SHA-1) hash is preferred because it is the one every tracker
//! and peer speaks today. A v2-only torrent falls back to the SHA-256 hash.

use std::fmt;
use std::str::FromStr;

use flux_core::metainfo::{Hashes, Info};

/// A torrent identity, rendered as lowercase hex.
///
/// 40 hex characters for a v1 hash, 64 for a v2 hash. The string form is what
/// the control API shows and what callers pass back in commands.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TorrentId(String);

impl TorrentId {
    /// Builds the identity of a parsed torrent, preferring the v1 hash.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::NoHash`] when the info dictionary carries neither hash,
    /// which a successful [`flux_core::metainfo::parse`] cannot produce.
    pub fn from_info(info: &Info) -> Result<Self, IdError> {
        Self::from_hashes(&info.hashes)
    }

    /// Builds the identity from a hash pair, preferring the v1 hash.
    pub fn from_hashes(hashes: &Hashes) -> Result<Self, IdError> {
        if let Some(hash) = hashes.v1 {
            return Ok(Self(to_hex(&hash)));
        }
        if let Some(hash) = hashes.v2 {
            return Ok(Self(to_hex(&hash)));
        }
        Err(IdError::NoHash)
    }

    /// Parses and validates a hex info hash.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Malformed`] unless the input is 40 or 64 hex
    /// characters. Both cases are normalised to lowercase.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        let valid_len = value.len() == 40 || value.len() == 64;
        if !valid_len || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(IdError::Malformed(value));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    /// The lowercase hex form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the id and returns the hex string.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for TorrentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TorrentId {
    type Err = IdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl AsRef<str> for TorrentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Why a torrent id could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The metainfo carried neither a v1 nor a v2 info hash.
    #[error("metainfo carries neither a v1 nor a v2 info hash")]
    NoHash,
    /// The string was not a 40- or 64-character hex digest.
    #[error("`{0}` is not a 40- or 64-character hex info hash")]
    Malformed(String),
}

/// Lowercase hex encoding.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn prefers_v1_and_falls_back_to_v2() {
        let both = Hashes {
            v1: Some([0xab; 20]),
            v2: Some([0xcd; 32]),
        };
        assert_eq!(TorrentId::from_hashes(&both).unwrap().as_str().len(), 40);

        let v2_only = Hashes {
            v1: None,
            v2: Some([0xcd; 32]),
        };
        assert_eq!(TorrentId::from_hashes(&v2_only).unwrap().as_str().len(), 64);

        let neither = Hashes { v1: None, v2: None };
        assert_eq!(
            TorrentId::from_hashes(&neither).unwrap_err(),
            IdError::NoHash
        );
    }

    #[test]
    fn round_trips_hex_and_normalises_case() {
        let id = TorrentId::new("AB".repeat(20)).unwrap();
        assert_eq!(id.as_str(), "ab".repeat(20));
        assert!(TorrentId::new("ab".repeat(19)).is_err());
        assert!(TorrentId::new("zz".repeat(20)).is_err());
        assert_eq!(id.to_string().parse::<TorrentId>().unwrap(), id);
    }
}
