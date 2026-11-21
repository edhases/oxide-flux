//! Embedded Lucide icons (ISC, see `assets/icons/LICENSE.txt`).

use iced::widget::Svg;
use iced::widget::svg;
use iced::{Color, Theme};

pub const SEARCH: &[u8] = include_bytes!("../assets/icons/search.svg");
pub const PLUS: &[u8] = include_bytes!("../assets/icons/plus.svg");
pub const MAGNET: &[u8] = include_bytes!("../assets/icons/magnet.svg");
pub const PAUSE: &[u8] = include_bytes!("../assets/icons/pause.svg");
pub const PLAY: &[u8] = include_bytes!("../assets/icons/play.svg");
pub const TRASH: &[u8] = include_bytes!("../assets/icons/trash-2.svg");
pub const FOLDER: &[u8] = include_bytes!("../assets/icons/folder-open.svg");
pub const DOWNLOAD: &[u8] = include_bytes!("../assets/icons/download.svg");
pub const ARROW_DOWN: &[u8] = include_bytes!("../assets/icons/arrow-down.svg");
pub const ARROW_UP: &[u8] = include_bytes!("../assets/icons/arrow-up.svg");
pub const CLOCK: &[u8] = include_bytes!("../assets/icons/clock.svg");
pub const ROTATE: &[u8] = include_bytes!("../assets/icons/rotate-cw.svg");
pub const CHECK_CIRCLE: &[u8] = include_bytes!("../assets/icons/circle-check.svg");
pub const ALERT: &[u8] = include_bytes!("../assets/icons/alert-triangle.svg");
pub const LOCK: &[u8] = include_bytes!("../assets/icons/lock.svg");
pub const CHEVRON_DOWN: &[u8] = include_bytes!("../assets/icons/chevron-down.svg");
pub const CHEVRON_UP: &[u8] = include_bytes!("../assets/icons/chevron-up.svg");
pub const X: &[u8] = include_bytes!("../assets/icons/x.svg");
pub const MORE: &[u8] = include_bytes!("../assets/icons/more-horizontal.svg");
pub const FILE: &[u8] = include_bytes!("../assets/icons/file.svg");
pub const HARD_DRIVE: &[u8] = include_bytes!("../assets/icons/hard-drive.svg");
pub const FILTER: &[u8] = include_bytes!("../assets/icons/list-filter.svg");
pub const SETTINGS: &[u8] = include_bytes!("../assets/icons/settings.svg");
pub const CIRCLE_X: &[u8] = include_bytes!("../assets/icons/circle-x.svg");
pub const CHECK: &[u8] = include_bytes!("../assets/icons/check.svg");
pub const UPLOAD: &[u8] = include_bytes!("../assets/icons/upload.svg");
pub const CIRCLE_PAUSE: &[u8] = include_bytes!("../assets/icons/circle-pause.svg");

/// One tinted icon at a fixed size.
pub fn icon(bytes: &'static [u8], size: f32, color: Color) -> Svg<'static, Theme> {
    Svg::new(svg::Handle::from_memory(bytes))
        .width(size)
        .height(size)
        .style(move |_theme, _status| svg::Style { color: Some(color) })
}
