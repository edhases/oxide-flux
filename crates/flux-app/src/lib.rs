//! Oxide Flux desktop client.
//!
//! Native (iced) shell over an in-process engine, streaming-first layout.

pub mod add_dialog;
pub mod app;
pub mod i18n;
pub mod icons;
pub mod launch;
pub mod platform;
pub mod playback;
pub mod player;
pub mod settings_dialog;
pub mod single_instance;
pub mod style;
pub mod supervisor;
pub mod theme;
pub mod tray;
pub mod update;

pub use app::{App, Filter, Message, format_bytes, format_duration, run};
pub use i18n::{Language, ResolvedLanguage};
pub use launch::{Initial, Plan};
pub use player::is_media;
