//! Pure BitTorrent data model: bencode, metainfo, path validation, piece selection.
//! No I/O, no CLI, no async runtime here.

pub mod bencode;
pub mod magnet;
pub mod metainfo;
pub mod path;
pub mod picker;
