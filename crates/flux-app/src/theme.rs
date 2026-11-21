//! Design tokens for Oxide Flux, transcribed from `docs/design/ui-spec.md`.
//!
//! One palette, one accent, no widget picks a color on its own: every style in
//! [`crate::style`] reads these fields.

use std::sync::LazyLock;

use iced::theme::Palette;
use iced::{Color, Theme};

/// Every color the UI may use.
#[allow(dead_code)]
pub struct Tokens {
    pub canvas: Color,
    pub surface: Color,
    pub raised: Color,
    pub overlay: Color,
    pub border_subtle: Color,
    pub border_default: Color,
    pub border_strong: Color,
    pub border_input: Color,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub text_disabled: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_pressed: Color,
    pub accent_fill: Color,
    pub accent_fill_hover: Color,
    pub accent_fill_pressed: Color,
    pub accent_on: Color,
    pub accent_text: Color,
    pub accent_soft: Color,
    pub selection_bg: Color,
    pub selection_bar: Color,
    pub focus_ring: Color,
    pub success: Color,
    pub success_soft: Color,
    pub warning: Color,
    pub warning_soft: Color,
    pub danger: Color,
    pub danger_fill: Color,
    pub danger_fill_hover: Color,
    pub danger_fill_pressed: Color,
    pub danger_on: Color,
    pub danger_soft: Color,
    pub info: Color,
    pub progress_track: Color,
}

/// Fixed heights and widths from the specification (§3.2).
pub mod metrics {
    /// Header strip.
    pub const HEADER: f32 = 56.0;
    /// Toolbar strip.
    pub const TOOLBAR: f32 = 44.0;
    /// Content heading above the table.
    pub const CONTENT_HEADER: f32 = 40.0;
    /// Table header row.
    pub const TABLE_HEADER: f32 = 32.0;
    /// One table row.
    pub const ROW: f32 = 32.0;
    /// Expanded details pane.
    pub const DETAILS: f32 = 232.0;
    /// Details identity header.
    pub const DETAILS_IDENTITY: f32 = 40.0;
    /// Details tab strip, including the 2 px underline.
    pub const DETAILS_TABS: f32 = 36.0;
    /// Status bar.
    pub const STATUS: f32 = 28.0;
    /// Sidebar width.
    pub const NAV: f32 = 232.0;
    /// Drop strip at the bottom of an unselected list.
    pub const DROP_STRIP: f32 = 56.0;
}

/// Font sizes in logical pixels (§2.4).
pub mod type_scale {
    /// Default UI text and table names.
    pub const BODY: f32 = 13.0;
    /// Table numbers, captions and the status bar.
    pub const CAPTION: f32 = 12.0;
    /// Micro labels, badges and footer hints.
    pub const MICRO: f32 = 11.0;
    /// Details identity heading.
    pub const HEADING: f32 = 15.0;
    /// Empty-state title.
    pub const DISPLAY: f32 = 24.0;
}

const fn rgb(hex: u32) -> Color {
    Color::from_rgb(
        ((hex >> 16) & 0xFF) as f32 / 255.0,
        ((hex >> 8) & 0xFF) as f32 / 255.0,
        (hex & 0xFF) as f32 / 255.0,
    )
}

/// The dark token set (default).
pub static T: LazyLock<Tokens> = LazyLock::new(|| Tokens {
    canvas: rgb(0x0F1115),
    surface: rgb(0x15181D),
    raised: rgb(0x1B1F26),
    overlay: rgb(0x232831),
    border_subtle: rgb(0x262C35),
    border_default: rgb(0x2E343E),
    border_strong: rgb(0x3C4450),
    border_input: rgb(0x667385),
    text_primary: rgb(0xE6E9EE),
    text_secondary: rgb(0xA7B0BC),
    text_muted: rgb(0x858F9D),
    text_disabled: rgb(0x5A6470),
    accent: rgb(0x3B82F6),
    accent_hover: rgb(0x5B9AF8),
    accent_pressed: rgb(0x2F6FD0),
    accent_fill: rgb(0x6BB6FF),
    accent_fill_hover: rgb(0x8CC8FF),
    accent_fill_pressed: rgb(0x4C9AFF),
    accent_on: rgb(0x08111F),
    accent_text: rgb(0x8AB8FF),
    accent_soft: rgb(0x1B2A44),
    selection_bg: rgb(0x20344F),
    selection_bar: rgb(0x3B82F6),
    focus_ring: rgb(0x60A5FA),
    success: rgb(0x34D399),
    success_soft: rgb(0x12352A),
    warning: rgb(0xFBBF24),
    warning_soft: rgb(0x3A2E12),
    danger: rgb(0xF87171),
    danger_fill: rgb(0xF87171),
    danger_fill_hover: rgb(0xFCA5A5),
    danger_fill_pressed: rgb(0xEF4444),
    danger_on: rgb(0x1A0A0A),
    danger_soft: rgb(0x3A1D1F),
    info: rgb(0x8AB8FF),
    progress_track: rgb(0x2A3039),
});

/// The iced theme handed to stock widgets and tooltips.
pub fn iced_theme() -> Theme {
    Theme::custom(
        "Oxide Flux",
        Palette {
            background: T.canvas,
            text: T.text_primary,
            primary: T.accent,
            success: T.success,
            warning: T.warning,
            danger: T.danger,
        },
    )
}
