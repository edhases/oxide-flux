//! Widget styles built only from [`crate::theme::T`].
//!
//! Selection is drawn as a child widget (a 2 px accent strip), not a border:
//! `iced::Border` has one width for all four sides, so a left-only bar cannot be
//! expressed as a widget border without custom drawing.

use flux_api::TorrentState;
use iced::widget::{button, container, pick_list, progress_bar, text_input};
use iced::{Background, Border, Color, Shadow, Theme};

use crate::theme::T;

fn border(color: Color, width: f32, radius: f32) -> Border {
    Border {
        color,
        width,
        radius: radius.into(),
    }
}

fn button_style(background: Color, text: Color, outline: Color, radius: f32) -> button::Style {
    button::Style {
        background: Some(Background::Color(background)),
        text_color: text,
        border: border(outline, 1.0, radius),
        shadow: Shadow::default(),
        snap: true,
    }
}

fn flat_button(background: Color, text: Color) -> button::Style {
    button::Style {
        background: Some(Background::Color(background)),
        text_color: text,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Filled blue CTA with a near-black label (FDM's dark pattern).
pub fn primary(_theme: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Active => {
            button_style(T.accent_fill, T.accent_on, Color::TRANSPARENT, 999.0)
        }
        button::Status::Hovered => {
            button_style(T.accent_fill_hover, T.accent_on, Color::TRANSPARENT, 999.0)
        }
        button::Status::Pressed => button_style(
            T.accent_fill_pressed,
            T.accent_on,
            Color::TRANSPARENT,
            999.0,
        ),
        button::Status::Disabled => {
            button_style(T.overlay, T.text_disabled, Color::TRANSPARENT, 999.0)
        }
    }
}

/// Outlined secondary action.
pub fn secondary(_theme: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Active => button_style(T.raised, T.text_primary, T.border_default, 999.0),
        button::Status::Hovered => button_style(T.overlay, T.text_primary, T.border_strong, 999.0),
        button::Status::Pressed => button_style(T.overlay, T.text_primary, T.border_strong, 999.0),
        button::Status::Disabled => {
            button_style(T.overlay, T.text_disabled, T.border_default, 999.0)
        }
    }
}

/// Text-only action for toolbars.
pub fn ghost(_theme: &Theme, status: button::Status) -> button::Style {
    let transparent = Color::TRANSPARENT;
    match status {
        button::Status::Active => button_style(transparent, T.text_secondary, transparent, 8.0),
        button::Status::Hovered => button_style(T.overlay, T.text_primary, transparent, 8.0),
        button::Status::Pressed => button_style(T.overlay, T.text_primary, transparent, 8.0),
        button::Status::Disabled => button_style(transparent, T.text_disabled, transparent, 8.0),
    }
}

/// Destructive text action.
pub fn danger_text(_theme: &Theme, status: button::Status) -> button::Style {
    let transparent = Color::TRANSPARENT;
    match status {
        button::Status::Active => button_style(transparent, T.danger, transparent, 8.0),
        button::Status::Hovered => button_style(T.danger_soft, T.danger, transparent, 8.0),
        button::Status::Pressed => button_style(T.danger_soft, T.danger, transparent, 8.0),
        button::Status::Disabled => button_style(transparent, T.text_disabled, transparent, 8.0),
    }
}

/// Square icon button, no background until hovered.
pub fn icon(_theme: &Theme, status: button::Status) -> button::Style {
    let transparent = Color::TRANSPARENT;
    match status {
        button::Status::Active => button_style(transparent, T.text_secondary, transparent, 8.0),
        button::Status::Hovered => button_style(T.overlay, T.text_primary, transparent, 8.0),
        button::Status::Pressed => button_style(T.overlay, T.text_primary, transparent, 8.0),
        button::Status::Disabled => button_style(transparent, T.text_disabled, transparent, 8.0),
    }
}

/// Sidebar row. `radius` is deliberately 0: the 2 px selection strip is a child
/// widget and iced does not clip it to a rounded parent background.
pub fn nav(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let transparent = Color::TRANSPARENT;
        if selected {
            return flat_button(T.selection_bg, T.text_primary);
        }
        match status {
            button::Status::Active => flat_button(transparent, T.text_primary),
            button::Status::Hovered => flat_button(T.overlay, T.text_primary),
            button::Status::Pressed => flat_button(T.overlay, T.text_primary),
            button::Status::Disabled => flat_button(transparent, T.text_disabled),
        }
    }
}

/// Table row, selected or not. Flush so the selection strip reaches the edge.
pub fn row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let transparent = Color::TRANSPARENT;
        if selected {
            return flat_button(T.selection_bg, T.text_primary);
        }
        match status {
            button::Status::Active => flat_button(transparent, T.text_primary),
            button::Status::Hovered => flat_button(T.raised, T.text_primary),
            button::Status::Pressed => flat_button(T.overlay, T.text_primary),
            button::Status::Disabled => flat_button(transparent, T.text_disabled),
        }
    }
}

/// Details tab; the 2 px accent underline is a sibling widget.
pub fn tab(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let transparent = Color::TRANSPARENT;
        if selected {
            return flat_button(transparent, T.text_primary);
        }
        match status {
            button::Status::Active => flat_button(transparent, T.text_secondary),
            button::Status::Hovered => flat_button(T.overlay, T.text_primary),
            button::Status::Pressed => flat_button(T.overlay, T.text_primary),
            button::Status::Disabled => flat_button(transparent, T.text_disabled),
        }
    }
}

/// Window canvas background.
pub fn canvas(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.canvas)),
        ..container::Style::default()
    }
}

/// A full-bleed pane surface: no outline, no radius, no gutter.
pub fn surface(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.surface)),
        ..container::Style::default()
    }
}

/// A borderless panel, for toolbars inside a surface.
pub fn flush(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.surface)),
        ..container::Style::default()
    }
}

/// 1 px structural rule (`border.subtle`).
pub fn rule(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.border_subtle)),
        ..container::Style::default()
    }
}

/// 1 px stronger rule, used below the table header.
pub fn rule_strong(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.border_strong)),
        ..container::Style::default()
    }
}

/// Status bar fill; the top rule is a sibling widget.
pub fn status_bar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.surface)),
        ..container::Style::default()
    }
}

/// Error banner fill; the 3 px danger edge is a sibling widget.
pub fn error_banner(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.danger_soft)),
        ..container::Style::default()
    }
}

/// The empty-state drop zone. iced cannot draw a dashed border, so the resting
/// outline is a solid 1 px `border.strong`; drag-over is a solid 2 px accent.
pub fn drop_zone(hovered: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(if hovered {
            T.accent_soft
        } else {
            T.raised
        })),
        border: border(
            if hovered { T.accent } else { T.border_strong },
            if hovered { 2.0 } else { 1.0 },
            12.0,
        ),
        ..container::Style::default()
    }
}

/// Selection strip child.
pub fn selection_bar(selected: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(if selected {
            T.selection_bar
        } else {
            Color::TRANSPARENT
        })),
        ..container::Style::default()
    }
}

/// 2 px tab underline.
pub fn underline(selected: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(if selected {
            T.accent
        } else {
            Color::TRANSPARENT
        })),
        ..container::Style::default()
    }
}

/// Search shell: the outer field with the leading icon and clear action.
pub fn search_shell(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(T.overlay)),
        border: border(T.border_input, 1.0, 8.0),
        ..container::Style::default()
    }
}

/// The text input inside [`search_shell`]: transparent until it owns focus, then
/// the 2 px focus ring (§4.8).
pub fn search_field(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let (width, color) = match status {
        text_input::Status::Focused { .. } => (2.0, T.focus_ring),
        text_input::Status::Hovered => (1.0, T.border_strong),
        text_input::Status::Active => (0.0, Color::TRANSPARENT),
        text_input::Status::Disabled => (0.0, Color::TRANSPARENT),
    };
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: border(color, width, 6.0),
        icon: T.text_muted,
        placeholder: T.text_muted,
        value: T.text_primary,
        selection: T.accent_soft,
    }
}

/// Search / form input.
pub fn input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let (width, border_color) = match status {
        text_input::Status::Focused { .. } => (2.0, T.focus_ring),
        text_input::Status::Hovered => (1.0, T.border_strong),
        text_input::Status::Active => (1.0, T.border_input),
        text_input::Status::Disabled => (1.0, T.border_default),
    };
    text_input::Style {
        background: Background::Color(T.overlay),
        border: border(border_color, width, 8.0),
        icon: T.text_muted,
        placeholder: T.text_muted,
        value: T.text_primary,
        selection: T.accent_soft,
    }
}

/// Priority dropdown.
pub fn pick(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let border_color = match status {
        pick_list::Status::Hovered => T.border_strong,
        pick_list::Status::Opened { .. } => T.focus_ring,
        _ => T.border_input,
    };
    pick_list::Style {
        text_color: T.text_primary,
        placeholder_color: T.text_muted,
        handle_color: T.text_muted,
        background: Background::Color(T.raised),
        border: border(border_color, 1.0, 8.0),
    }
}

fn progress_style(fill: Color) -> progress_bar::Style {
    progress_bar::Style {
        background: Background::Color(T.progress_track),
        bar: Background::Color(fill),
        border: border(Color::TRANSPARENT, 0.0, 999.0),
    }
}

/// Thin table progress bar in the data accent.
pub fn progress(_theme: &Theme) -> progress_bar::Style {
    progress_style(T.accent)
}

/// Progress bar coloured by lifecycle state (§4.6): success at 100 %, warning
/// while checking, danger on error, muted when paused, accent otherwise.
pub fn progress_state(
    state: TorrentState,
    fraction: f32,
) -> impl Fn(&Theme) -> progress_bar::Style {
    let fill = match state {
        TorrentState::Error => T.danger,
        TorrentState::Paused => T.text_muted,
        TorrentState::Checking => T.warning,
        _ if fraction >= 0.999 => T.success,
        _ => T.accent,
    };
    move |_theme| progress_style(fill)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn fill(state: TorrentState, fraction: f32) -> Background {
        progress_state(state, fraction)(&crate::theme::iced_theme()).bar
    }

    #[test]
    fn progress_fill_follows_state() {
        assert_eq!(
            fill(TorrentState::Paused, 0.4),
            Background::Color(crate::theme::T.text_muted)
        );
        assert_eq!(
            fill(TorrentState::Error, 0.4),
            Background::Color(crate::theme::T.danger)
        );
        assert_eq!(
            fill(TorrentState::Checking, 0.4),
            Background::Color(crate::theme::T.warning)
        );
        assert_eq!(
            fill(TorrentState::Downloading, 1.0),
            Background::Color(crate::theme::T.success)
        );
        assert_eq!(
            fill(TorrentState::Downloading, 0.4),
            Background::Color(crate::theme::T.accent)
        );
    }
}
