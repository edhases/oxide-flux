//! Oxide Flux desktop client.
//!
//! Native (iced) shell over an in-process engine, streaming-first layout.

pub mod app;
pub mod icons;
pub mod launch;
pub mod platform;
pub mod player;
pub mod single_instance;
pub mod style;
pub mod supervisor;
pub mod theme;

pub use app::{App, Filter, Message, format_bytes, format_duration, run};
pub use launch::{Initial, Plan};
pub use player::is_media;
