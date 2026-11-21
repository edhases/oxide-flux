//! # flux-net
//!
//! Peer networking for Oxide Flux. This pass covers the TCP peer wire —
//! piece-availability bitmaps ([`bitfield`]), the BEP 3 handshake
//! ([`handshake`]), the BEP 3 message codec ([`message`]), and payload-only
//! rate limiting ([`limit`]) — plus tracker announces ([`tracker`]): HTTP(S)
//! per BEP 3 and UDP per BEP 15.
//!
//! uTP, DHT, PEX, LSD and MSE/PE are deliberately absent. They will arrive as
//! their own modules rather than as stubs, so "it compiles" never implies
//! support that does not exist.
//!
//! ## Shape
//!
//! The peer-wire modules are synchronous and clock-free. The daemon owns the
//! socket in a tokio task and:
//!
//! 1. feeds whatever `read` returned into [`Decoder::feed`];
//! 2. drains [`Decoder::next_frame`] until it reports that more bytes are
//!    needed;
//! 3. charges [`Message::payload_len`] bytes to a [`RateLimiter`] with a
//!    monotonic `now`, passing the frame's overhead and LAN peers through
//!    [`TrafficClass`] instead.
//!
//! Keeping I/O and time outside the codec is what makes the framing and
//! limiter tests deterministic. The [`tracker`] modules are the exception to
//! the sync rule — they do real HTTP and UDP — but they follow the same clock
//! rule: [`Announcer::announce`] takes `now` from the caller and never reads a
//! system time.
//!
//! ## Limits the daemon should mirror
//!
//! | item | value |
//! | --- | --- |
//! | frame body cap (default) | [`DEFAULT_MAX_FRAME_LEN`] = 128 KiB + 9 = 131 081 |
//! | standard block | [`BLOCK_SIZE`] = 16 KiB |
//! | largest block | [`MAX_BLOCK_SIZE`] = 128 KiB |
//! | piece frame overhead | [`PIECE_FRAME_OVERHEAD`] = 13 bytes |
//! | burst | caller-configured, never exceeded; rate 0 = blocked |
//!
//! The frame cap is a *body* cap, so a legal `piece` frame of `1 + 8 +
//! block` bytes fits a 128 KiB block exactly. Torrents with more than
//! 1 048 576 pieces need a bigger cap for their bitfield message and should use
//! [`Decoder::with_max_frame_len`].

#![warn(missing_docs)]

pub mod bitfield;
pub mod dht;
pub mod handshake;
pub mod limit;
pub mod message;
pub mod metadata;
pub mod peer;
pub mod tracker;

pub use bitfield::{Bitfield, BitfieldError, SetBits};
pub use handshake::{
    HANDSHAKE_LEN, Handshake, HandshakeError, INFO_HASH_LEN, InfoHash, PEER_ID_LEN, PSTR, PSTR_LEN,
    PeerId, RESERVED_DHT, RESERVED_EXTENDED, RESERVED_FAST, ReservedBit,
};
pub use limit::{RateLimiter, TrafficClass};
pub use message::{
    BLOCK_SIZE, BitfieldMessage, DEFAULT_MAX_FRAME_LEN, DecodeError, Decoder, EncodeError,
    MAX_BLOCK_SIZE, Message, PIECE_FRAME_OVERHEAD, encode, encode_into, ids,
};
pub use tracker::{
    AnnounceOutcome, AnnounceRequest, AnnounceResponse, Announcer, AnnouncerConfig, Event, Scheme,
    TrackerError,
};
