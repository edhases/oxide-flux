//! Add Torrent Dialog modal for Oxide Flux.
//!
//! Provides a modern, modular sheet for adding torrents via `.torrent` files
//! or `magnet:` links, matching `docs/design/ui-spec.md` (§5.3) and `AGENTS.md`.

use std::path::{Path, PathBuf};

use iced::widget::{
    button, checkbox, column, container, mouse_area, opaque, pick_list, row, scrollable,
    space::Space, stack, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Fill, Font, Length, Theme, font};

use crate::i18n::{ResolvedLanguage, TextKey, tr};
use crate::icons::{self, icon};
use crate::player;
use crate::style;
use crate::theme::{T, type_scale};

/// Segmented mode for the add dialog: local file or magnet URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    File,
    Magnet,
}

/// Priority choice for a file within the torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PriorityChoice {
    Skip,
    #[default]
    Normal,
    High,
}

impl PriorityChoice {
    /// All selectable priority choices in order.
    pub const ALL: [Self; 3] = [Self::Skip, Self::Normal, Self::High];
}

impl std::fmt::Display for PriorityChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Skip => "Skip",
            Self::Normal => "Normal",
            Self::High => "High",
        })
    }
}

impl From<PriorityChoice> for flux_api::Priority {
    fn from(choice: PriorityChoice) -> Self {
        match choice {
            PriorityChoice::Skip => Self::Skip,
            PriorityChoice::Normal => Self::Normal,
            PriorityChoice::High => Self::High,
        }
    }
}

impl From<flux_api::Priority> for PriorityChoice {
    fn from(priority: flux_api::Priority) -> Self {
        match priority {
            flux_api::Priority::Skip => Self::Skip,
            flux_api::Priority::High => Self::High,
            flux_api::Priority::Low | flux_api::Priority::Normal => Self::Normal,
        }
    }
}

/// One entry in the torrent file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Index corresponding to the engine layout.
    pub index: usize,
    /// Path of the file relative to the torrent root folder.
    pub path: PathBuf,
    /// Total length in bytes.
    pub length: u64,
    /// Whether this file is currently selected for download.
    pub selected: bool,
    /// Desired download priority.
    pub priority: PriorityChoice,
}

impl FileEntry {
    /// Creates a new `FileEntry` with default `Normal` priority and `selected: true`.
    pub fn new(index: usize, path: PathBuf, length: u64) -> Self {
        Self {
            index,
            path,
            length,
            selected: true,
            priority: PriorityChoice::Normal,
        }
    }

    /// User-facing display name (file name or relative path string).
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| self.path.to_str().unwrap_or("—"))
            .to_string()
    }

    /// Whether this file is recognized as playable media.
    pub fn is_video(&self) -> bool {
        player::is_media(&self.path.to_string_lossy())
    }
}

/// Resulting action produced when dialog interaction yields an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddDialogAction {
    /// Confirm and submit a `.torrent` file download.
    ConfirmTorrent {
        bytes: Vec<u8>,
        save_path: PathBuf,
        file_priorities: Vec<(usize, PriorityChoice)>,
        start_immediately: bool,
        sequential: bool,
    },
    /// Confirm and submit a `magnet:` link download.
    ConfirmMagnet {
        uri: String,
        save_path: PathBuf,
        start_immediately: bool,
        sequential: bool,
    },
    /// Request the parent window to open a folder picker.
    BrowseFolder,
    /// Request the parent window to open a file picker for `.torrent`.
    BrowseTorrentFile,
    /// Request the engine/supervisor to fetch metadata for a magnet URI.
    FetchMagnet(String),
    /// Cancel or close the dialog.
    Cancel,
}

/// Messages emitted by the Add Torrent dialog widgets.
#[derive(Debug, Clone)]
pub enum AddDialogMessage {
    ModeChanged(Mode),
    SavePathChanged(String),
    BrowseFolder,
    FolderPicked(Option<PathBuf>),
    ToggleFile(usize),
    FilePriority(usize, PriorityChoice),
    SelectAllFiles,
    SelectNoneFiles,
    SelectVideoFiles,
    ToggleStartImmediately(bool),
    ToggleSequential(bool),
    Confirm,
    Cancel,
    MagnetInputChanged(String),
    FetchMagnet,
    BrowseTorrentFile,
    TorrentFilePicked(Option<Vec<u8>>),
}

/// State of the Add Torrent Dialog.
#[derive(Debug, Clone)]
pub struct AddDialogState {
    /// Active mode: File or Magnet.
    pub mode: Mode,

    // --- Mode::File state ---
    /// Torrent display name.
    pub name: String,
    /// Total payload size of the torrent in bytes.
    pub total_size: u64,
    /// Total number of pieces, if known.
    pub piece_count: Option<usize>,
    /// Piece length in bytes, if known.
    pub piece_length: Option<u32>,
    /// Selected destination folder.
    pub save_path: String,
    /// Available space in bytes on destination drive.
    pub free_space: Option<u64>,
    /// Error message string, if any.
    pub error: Option<String>,
    /// Raw .torrent file bytes, retained so the engine does not re-read from disk.
    pub bytes: Option<Vec<u8>>,
    /// List of declared files.
    pub files: Vec<FileEntry>,

    // --- Mode::Magnet state ---
    /// Magnet link input string.
    pub magnet_input: String,
    /// Whether metadata fetch is currently active.
    pub fetching_metadata: bool,
    /// Connected peers during metadata fetch.
    pub fetch_peers: usize,
    /// Metadata fetch error message, if any.
    pub fetch_error: Option<String>,

    // --- Start options ---
    /// Whether to start downloading immediately upon adding.
    pub start_immediately: bool,
    /// Whether to download sequentially (stream ready).
    pub sequential: bool,

    // --- Localization ---
    /// Optional language override (English default).
    pub language: Option<ResolvedLanguage>,
}

impl AddDialogState {
    /// Creates a new default dialog state pointing to `default_download_dir`.
    pub fn new(default_download_dir: impl Into<String>) -> Self {
        let save_path = default_download_dir.into();
        let free = free_space(Path::new(save_path.trim()));
        Self {
            mode: Mode::File,
            name: String::new(),
            total_size: 0,
            piece_count: None,
            piece_length: None,
            save_path,
            free_space: free,
            error: None,
            bytes: None,
            files: Vec::new(),
            magnet_input: String::new(),
            fetching_metadata: false,
            fetch_peers: 0,
            fetch_error: None,
            start_immediately: true,
            sequential: false,
            language: None,
        }
    }

    /// Sets the language for localization.
    pub fn with_language(mut self, lang: ResolvedLanguage) -> Self {
        self.language = Some(lang);
        self
    }

    /// Initializes state from parsed `.torrent` bytes.
    pub fn from_metainfo(
        bytes: Vec<u8>,
        save_path: impl Into<String>,
        free_space: Option<u64>,
    ) -> Result<Self, String> {
        let mut state = Self::new(save_path);
        state.free_space = free_space;
        state.set_metainfo(bytes)?;
        Ok(state)
    }

    /// Parses `.torrent` bytes and populates metadata and file tree.
    pub fn set_metainfo(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let metainfo = flux_core::metainfo::parse(&bytes)
            .map_err(|err| format!("Invalid .torrent file: {err}"))?;

        let layout = &metainfo.info.layout;
        let files: Vec<FileEntry> = layout
            .files()
            .iter()
            .enumerate()
            .map(|(index, f)| {
                let path = PathBuf::from_iter(f.path.iter().map(String::as_str));
                FileEntry::new(index, path, f.length)
            })
            .collect();

        self.name = metainfo.info.name;
        self.total_size = files.iter().map(|f| f.length).sum();
        self.piece_count = metainfo.info.pieces.as_ref().map(Vec::len);
        self.piece_length = Some(metainfo.info.piece_length);
        self.bytes = Some(bytes);
        self.files = files;
        self.error = None;
        Ok(())
    }

    /// Selects all files for download and restores Normal priority if previously Skipped.
    pub fn select_all(&mut self) {
        for file in &mut self.files {
            file.selected = true;
            if file.priority == PriorityChoice::Skip {
                file.priority = PriorityChoice::Normal;
            }
        }
    }

    /// Deselects all files and sets priority to Skip.
    pub fn select_none(&mut self) {
        for file in &mut self.files {
            file.selected = false;
            file.priority = PriorityChoice::Skip;
        }
    }

    /// Selects only recognized video/media files, skipping all others.
    pub fn select_video(&mut self) {
        for file in &mut self.files {
            if file.is_video() {
                file.selected = true;
                if file.priority == PriorityChoice::Skip {
                    file.priority = PriorityChoice::Normal;
                }
            } else {
                file.selected = false;
                file.priority = PriorityChoice::Skip;
            }
        }
    }

    /// Toggles the selection of a specific file by index.
    pub fn toggle_file(&mut self, index: usize) {
        if let Some(file) = self.files.get_mut(index) {
            file.selected = !file.selected;
            if file.selected {
                if file.priority == PriorityChoice::Skip {
                    file.priority = PriorityChoice::Normal;
                }
            } else {
                file.priority = PriorityChoice::Skip;
            }
        }
    }

    /// Updates the priority of a file by index.
    pub fn set_file_priority(&mut self, index: usize, priority: PriorityChoice) {
        if let Some(file) = self.files.get_mut(index) {
            file.priority = priority;
            file.selected = priority != PriorityChoice::Skip;
        }
    }

    /// Total bytes of currently selected files.
    pub fn selected_size(&self) -> u64 {
        self.files
            .iter()
            .filter(|f| f.selected)
            .map(|f| f.length)
            .sum()
    }

    /// Total count of currently selected files.
    pub fn selected_count(&self) -> usize {
        self.files.iter().filter(|f| f.selected).count()
    }

    /// Calculates bytes still missing under `save_path` for selected files.
    ///
    /// Existing file data on disk is credited (up to declared length) and does not
    /// need to be fetched or budgeted again.
    pub fn missing_bytes(&self) -> u64 {
        let root = Path::new(self.save_path.trim());
        if self.files.is_empty() {
            return self.total_size;
        }

        let mut missing = 0u64;
        for file in &self.files {
            if !file.selected {
                continue;
            }
            let length = file.length;
            let path = safe_join(root, &file.path);
            let present = match path.and_then(|p| std::fs::metadata(p).ok()) {
                Some(meta) if meta.is_file() => meta.len().min(length),
                _ => 0,
            };
            missing = missing.saturating_add(length.saturating_sub(present));
        }
        missing
    }

    /// Whether the required download size exceeds measurable free space.
    pub fn lacks_space(&self) -> bool {
        self.free_space
            .is_some_and(|free| self.missing_bytes() > free)
    }

    /// Whether at least one playable media file is currently selected.
    pub fn has_playable_selected(&self) -> bool {
        self.files.iter().any(|f| f.selected && f.is_video())
    }

    /// Whether the dialog inputs are valid for submission.
    pub fn can_confirm(&self) -> bool {
        let path_ok = !self.save_path.trim().is_empty();
        match self.mode {
            Mode::File => {
                path_ok
                    && self.bytes.is_some()
                    && !self.files.is_empty()
                    && self.selected_count() > 0
                    && !self.lacks_space()
            }
            Mode::Magnet => {
                let magnet_ok = !self.magnet_input.trim().is_empty();
                if self.files.is_empty() {
                    path_ok && magnet_ok
                } else {
                    path_ok && magnet_ok && self.selected_count() > 0 && !self.lacks_space()
                }
            }
        }
    }

    /// Translates a key using dialog language or English fallback.
    fn text(&self, key: TextKey) -> &'static str {
        tr(self.language.unwrap_or(ResolvedLanguage::English), key)
    }

    /// Handles a dialog message, updating internal state and returning an action if triggered.
    pub fn update(&mut self, message: AddDialogMessage) -> Option<AddDialogAction> {
        match message {
            AddDialogMessage::ModeChanged(mode) => {
                self.mode = mode;
                None
            }
            AddDialogMessage::SavePathChanged(path) => {
                self.free_space = free_space(Path::new(path.trim()));
                self.save_path = path;
                self.error = None;
                None
            }
            AddDialogMessage::BrowseFolder => Some(AddDialogAction::BrowseFolder),
            AddDialogMessage::FolderPicked(maybe_path) => {
                if let Some(path) = maybe_path {
                    self.free_space = free_space(&path);
                    self.save_path = path.display().to_string();
                    self.error = None;
                }
                None
            }
            AddDialogMessage::ToggleFile(index) => {
                self.toggle_file(index);
                None
            }
            AddDialogMessage::FilePriority(index, priority) => {
                self.set_file_priority(index, priority);
                None
            }
            AddDialogMessage::SelectAllFiles => {
                self.select_all();
                None
            }
            AddDialogMessage::SelectNoneFiles => {
                self.select_none();
                None
            }
            AddDialogMessage::SelectVideoFiles => {
                self.select_video();
                None
            }
            AddDialogMessage::ToggleStartImmediately(enabled) => {
                self.start_immediately = enabled;
                None
            }
            AddDialogMessage::ToggleSequential(enabled) => {
                self.sequential = enabled;
                None
            }
            AddDialogMessage::MagnetInputChanged(val) => {
                self.magnet_input = val;
                self.fetch_error = None;
                None
            }
            AddDialogMessage::FetchMagnet => {
                let trimmed = self.magnet_input.trim();
                if trimmed.is_empty() {
                    self.fetch_error = Some("Enter a magnet URI to fetch.".to_string());
                    None
                } else {
                    self.fetching_metadata = true;
                    self.fetch_error = None;
                    Some(AddDialogAction::FetchMagnet(trimmed.to_string()))
                }
            }
            AddDialogMessage::BrowseTorrentFile => Some(AddDialogAction::BrowseTorrentFile),
            AddDialogMessage::TorrentFilePicked(maybe_bytes) => {
                if let Some(bytes) = maybe_bytes {
                    if let Err(err) = self.set_metainfo(bytes) {
                        self.error = Some(err);
                    } else {
                        self.error = None;
                    }
                }
                None
            }
            AddDialogMessage::Cancel => Some(AddDialogAction::Cancel),
            AddDialogMessage::Confirm => {
                let trimmed = self.save_path.trim();
                if trimmed.is_empty() {
                    self.error = Some("Choose a folder to save into.".to_string());
                    return None;
                }
                let root = PathBuf::from(trimmed);
                if root.exists() && !root.is_dir() {
                    self.error = Some(format!("`{}` is a file, not a folder.", root.display()));
                    return None;
                }

                match self.mode {
                    Mode::File => {
                        let Some(bytes) = &self.bytes else {
                            self.error = Some("Please select a torrent file first.".to_string());
                            return None;
                        };
                        if self.selected_count() == 0 {
                            self.error =
                                Some("Please select at least one file to download.".to_string());
                            return None;
                        }
                        if self.lacks_space() {
                            self.error = Some(format!(
                                "Not enough space: needs {}, only {} free.",
                                crate::format_bytes(self.missing_bytes()),
                                crate::format_bytes(self.free_space.unwrap_or(0)),
                            ));
                            return None;
                        }

                        let file_priorities =
                            self.files.iter().map(|f| (f.index, f.priority)).collect();

                        Some(AddDialogAction::ConfirmTorrent {
                            bytes: bytes.clone(),
                            save_path: root,
                            file_priorities,
                            start_immediately: self.start_immediately,
                            sequential: self.sequential,
                        })
                    }
                    Mode::Magnet => {
                        let uri = self.magnet_input.trim();
                        if uri.is_empty() {
                            self.fetch_error = Some("Enter a magnet URI.".to_string());
                            return None;
                        }
                        if !self.files.is_empty() {
                            if self.selected_count() == 0 {
                                self.fetch_error = Some(
                                    "Please select at least one file to download.".to_string(),
                                );
                                return None;
                            }
                            if self.lacks_space() {
                                self.fetch_error = Some(format!(
                                    "Not enough space: needs {}, only {} free.",
                                    crate::format_bytes(self.missing_bytes()),
                                    crate::format_bytes(self.free_space.unwrap_or(0)),
                                ));
                                return None;
                            }
                        }

                        Some(AddDialogAction::ConfirmMagnet {
                            uri: uri.to_string(),
                            save_path: root,
                            start_immediately: self.start_immediately,
                            sequential: self.sequential,
                        })
                    }
                }
            }
        }
    }

    /// Renders the complete Add Torrent dialog modal.
    pub fn view(&self) -> Element<'_, AddDialogMessage> {
        let title_str = self.text(TextKey::AddTorrent);
        let header = row![
            text(title_str)
                .size(type_scale::HEADING)
                .font(font_semibold())
                .color(T.text_primary),
            Space::new().width(Fill),
            button(icon(icons::X, 14.0, T.text_muted))
                .on_press(AddDialogMessage::Cancel)
                .style(style::icon)
                .padding(0)
                .width(24)
                .height(24),
        ]
        .align_y(Alignment::Center);

        let segmented_mode = container(
            row![
                button(text("Torrent file").size(type_scale::CAPTION).font(
                    if self.mode == Mode::File {
                        font_semibold()
                    } else {
                        font_medium()
                    }
                ))
                .on_press(AddDialogMessage::ModeChanged(Mode::File))
                .style(segment_button_style(self.mode == Mode::File))
                .padding([4, 16])
                .height(28),
                button(text("Magnet link").size(type_scale::CAPTION).font(
                    if self.mode == Mode::Magnet {
                        font_semibold()
                    } else {
                        font_medium()
                    }
                ))
                .on_press(AddDialogMessage::ModeChanged(Mode::Magnet))
                .style(segment_button_style(self.mode == Mode::Magnet))
                .padding([4, 16])
                .height(28),
            ]
            .spacing(2)
            .align_y(Alignment::Center),
        )
        .style(|_| container::Style {
            background: Some(Background::Color(T.overlay)),
            border: Border {
                color: T.border_subtle,
                width: 1.0,
                radius: 999.0.into(),
            },
            ..container::Style::default()
        })
        .padding(2);

        let mut body = column![header, segmented_mode].spacing(14).width(Fill);

        match self.mode {
            Mode::File => {
                if self.bytes.is_none() && self.name.is_empty() {
                    let drop_zone_content = container(
                        column![
                            icon(icons::DOWNLOAD, 28.0, T.accent),
                            text("Drop a .torrent file here")
                                .size(type_scale::BODY)
                                .font(font_medium())
                                .color(T.text_primary),
                            text("or click to browse")
                                .size(type_scale::CAPTION)
                                .color(T.text_muted),
                        ]
                        .spacing(6)
                        .align_x(Alignment::Center),
                    )
                    .width(Fill)
                    .height(130)
                    .center_x(Fill)
                    .center_y(Fill)
                    .style(style::drop_zone(false));

                    let drop_zone_button = button(drop_zone_content)
                        .on_press(AddDialogMessage::BrowseTorrentFile)
                        .style(style::ghost)
                        .padding(0)
                        .width(Fill);

                    body = body.push(drop_zone_button);
                } else {
                    body = body.push(self.view_torrent_identity());
                    body = body.push(self.view_save_path_section());
                    body = body.push(self.view_files_section());
                }
            }
            Mode::Magnet => {
                body = body.push(self.view_magnet_input());
                body = body.push(self.view_save_path_section());
                if !self.files.is_empty() {
                    body = body.push(self.view_files_section());
                }
            }
        }

        body = body.push(self.view_start_options());
        body = body.push(self.view_footer());

        let card = container(body)
            .width(620)
            .max_height(680)
            .padding(22)
            .style(|_| container::Style {
                background: Some(Background::Color(T.surface)),
                border: Border {
                    color: T.border_strong,
                    width: 1.0,
                    radius: 12.0.into(),
                },
                ..container::Style::default()
            });

        let scrim = mouse_area(container(Space::new().width(Fill).height(Fill)).style(|_| {
            container::Style {
                background: Some(Background::Color(Color {
                    a: 0.55,
                    ..Color::BLACK
                })),
                ..container::Style::default()
            }
        }))
        .on_press(AddDialogMessage::Cancel);

        stack![
            scrim,
            container(opaque(card))
                .width(Fill)
                .height(Fill)
                .center_x(Fill)
                .center_y(Fill),
        ]
        .into()
    }

    fn view_torrent_identity(&self) -> Element<'_, AddDialogMessage> {
        let size_str = crate::format_bytes(self.total_size);
        let subtitle = match (self.piece_count, self.piece_length) {
            (Some(count), Some(len)) => {
                format!(
                    "{} · {} pieces · {} pieces",
                    size_str,
                    count,
                    crate::format_bytes(len as u64)
                )
            }
            (Some(count), None) => format!("{size_str} · {count} pieces · ready to add"),
            _ => format!("{size_str} · ready to add"),
        };

        column![
            text(self.name.clone())
                .size(type_scale::BODY)
                .font(font_semibold())
                .color(T.text_primary)
                .wrapping(iced::widget::text::Wrapping::Word),
            text(subtitle).size(type_scale::CAPTION).color(T.text_muted),
        ]
        .spacing(3)
        .into()
    }

    fn view_magnet_input(&self) -> Element<'_, AddDialogMessage> {
        let label = text("MAGNET LINK")
            .size(type_scale::MICRO)
            .font(font_semibold())
            .color(T.text_muted);

        let input_field = text_input("magnet:?xt=urn:btih:...", &self.magnet_input)
            .on_input(AddDialogMessage::MagnetInputChanged)
            .on_submit(AddDialogMessage::FetchMagnet)
            .style(style::input)
            .padding([7, 10])
            .size(type_scale::BODY);

        let fetch_label = self.text(TextKey::FetchMetadata);
        let fetch_button = button(
            row![
                icon(icons::ROTATE, 14.0, T.text_primary),
                text(fetch_label).size(type_scale::BODY),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .on_press(AddDialogMessage::FetchMagnet)
        .style(style::secondary)
        .padding([0, 14])
        .height(34);

        let input_row = row![input_field, fetch_button]
            .spacing(8)
            .height(34)
            .align_y(Alignment::Center);

        let status_hint = if self.fetching_metadata {
            row![
                icon(icons::ROTATE, 14.0, T.accent),
                text(format!(
                    "Fetching metadata… {} peers connected",
                    self.fetch_peers
                ))
                .size(type_scale::CAPTION)
                .color(T.text_secondary),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
        } else if let Some(err) = &self.fetch_error {
            row![
                icon(icons::ALERT, 14.0, T.danger),
                text(err.clone()).size(type_scale::CAPTION).color(T.danger),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
        } else {
            row![
                text("Paste a magnet link to fetch its metadata and select files.")
                    .size(type_scale::CAPTION)
                    .color(T.text_muted),
            ]
            .align_y(Alignment::Center)
        };

        column![label, input_row, status_hint].spacing(6).into()
    }

    fn view_save_path_section(&self) -> Element<'_, AddDialogMessage> {
        let save_to_text = self.text(TextKey::SaveTo);
        let folder_placeholder = self.text(TextKey::FolderToDownloadInto);
        let browse_text = self.text(TextKey::Browse);

        let label = text(save_to_text)
            .size(type_scale::MICRO)
            .font(font_semibold())
            .color(T.text_muted);

        let path_field = text_input(folder_placeholder, &self.save_path)
            .on_input(AddDialogMessage::SavePathChanged)
            .on_submit(AddDialogMessage::Confirm)
            .style(style::input)
            .padding([7, 10])
            .size(type_scale::BODY);

        let browse_button = button(
            row![
                icon(icons::FOLDER, 14.0, T.text_primary),
                text(browse_text).size(type_scale::BODY),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .on_press(AddDialogMessage::BrowseFolder)
        .style(style::secondary)
        .padding([0, 14])
        .height(34);

        let path_row = row![path_field, browse_button]
            .spacing(8)
            .height(34)
            .align_y(Alignment::Center);

        let hint = match &self.error {
            Some(err) => text(err.clone())
                .size(type_scale::CAPTION)
                .color(T.danger)
                .width(Fill)
                .wrapping(iced::widget::text::Wrapping::Word),
            None if self.lacks_space() => text(format!(
                "Not enough space: needs {}, only {} free on this drive.",
                crate::format_bytes(self.missing_bytes()),
                crate::format_bytes(self.free_space.unwrap_or(0)),
            ))
            .size(type_scale::CAPTION)
            .color(T.danger)
            .width(Fill)
            .wrapping(iced::widget::text::Wrapping::Word),
            None if !self.files.is_empty() && self.missing_bytes() < self.total_size => {
                text(format!(
                    "{} of {} already in that folder; it will be verified, not downloaded again.",
                    crate::format_bytes(self.total_size.saturating_sub(self.missing_bytes())),
                    crate::format_bytes(self.total_size),
                ))
                .size(type_scale::CAPTION)
                .color(T.text_secondary)
                .width(Fill)
                .wrapping(iced::widget::text::Wrapping::Word)
            }
            None if self.free_space.is_some() => text(format!(
                "Free space: {} · Needs {}",
                crate::format_bytes(self.free_space.unwrap_or(0)),
                crate::format_bytes(self.missing_bytes()),
            ))
            .size(type_scale::CAPTION)
            .color(T.text_muted)
            .width(Fill)
            .wrapping(iced::widget::text::Wrapping::Word),
            None => text(self.text(TextKey::FolderCreatedIfNotExist))
                .size(type_scale::CAPTION)
                .color(T.text_muted)
                .width(Fill)
                .wrapping(iced::widget::text::Wrapping::Word),
        };

        column![label, path_row, hint].spacing(6).into()
    }

    fn view_files_section(&self) -> Element<'_, AddDialogMessage> {
        let files_label = self.text(TextKey::Files);
        let all_label = self.text(TextKey::All);
        let none_label = self.text(TextKey::None);
        let video_label = self.text(TextKey::Video);

        let header = row![
            text(files_label)
                .size(type_scale::CAPTION)
                .font(font_semibold())
                .color(T.text_primary),
            text(format!(
                "· {} of {} selected · {}",
                self.selected_count(),
                self.files.len(),
                crate::format_bytes(self.selected_size())
            ))
            .size(type_scale::CAPTION)
            .color(T.text_muted),
            Space::new().width(Fill),
            button(text(all_label).size(type_scale::MICRO))
                .on_press(AddDialogMessage::SelectAllFiles)
                .style(style::secondary)
                .padding([2, 8])
                .height(24),
            button(text(none_label).size(type_scale::MICRO))
                .on_press(AddDialogMessage::SelectNoneFiles)
                .style(style::secondary)
                .padding([2, 8])
                .height(24),
            button(text(video_label).size(type_scale::MICRO))
                .on_press(AddDialogMessage::SelectVideoFiles)
                .style(style::secondary)
                .padding([2, 8])
                .height(24),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .height(28);

        let mut file_rows = column![].spacing(1);
        for file in &self.files {
            let index = file.index;
            let selected = file.selected;
            let priority = file.priority;
            let name_color = if selected {
                T.text_primary
            } else {
                T.text_muted
            };

            let file_cb = checkbox(selected)
                .size(16.0)
                .style(checkbox_style)
                .on_toggle(move |_| AddDialogMessage::ToggleFile(index));

            let file_icon = icon(
                icons::FILE,
                14.0,
                if selected { T.accent } else { T.text_muted },
            );

            let name_text = text(elide(&file.display_name(), 44))
                .size(type_scale::CAPTION)
                .color(name_color)
                .wrapping(iced::widget::text::Wrapping::None)
                .width(Fill);

            let size_text = text(crate::format_bytes(file.length))
                .size(type_scale::MICRO)
                .color(T.text_muted)
                .width(68)
                .align_x(Alignment::End);

            let priority_pick =
                pick_list(&PriorityChoice::ALL[..], Some(priority), move |choice| {
                    AddDialogMessage::FilePriority(index, choice)
                })
                .text_size(type_scale::MICRO)
                .style(style::pick)
                .width(88);

            let row_item = row![file_cb, file_icon, name_text, size_text, priority_pick]
                .spacing(8)
                .padding([2, 8])
                .height(28)
                .align_y(Alignment::Center);

            file_rows = file_rows.push(row_item);
        }

        let scrollable_files = container(
            scrollable(file_rows)
                .height(Length::Fixed(200.0))
                .width(Fill),
        )
        .style(|_| container::Style {
            background: Some(Background::Color(T.overlay)),
            border: Border {
                color: T.border_subtle,
                width: 1.0,
                radius: 8.0.into(),
            },
            ..container::Style::default()
        })
        .padding(4);

        column![header, scrollable_files].spacing(6).into()
    }

    fn view_start_options(&self) -> Element<'_, AddDialogMessage> {
        let start_label = self.text(TextKey::StartImmediately);
        let download_in_order_label = self.text(TextKey::DownloadInOrder);

        let start_cb = checkbox(self.start_immediately)
            .label(start_label)
            .size(16.0)
            .style(checkbox_style)
            .on_toggle(AddDialogMessage::ToggleStartImmediately);

        let sequential_cb = if self.has_playable_selected() || self.files.is_empty() {
            checkbox(self.sequential)
                .label(download_in_order_label)
                .size(16.0)
                .style(checkbox_style)
                .on_toggle(AddDialogMessage::ToggleSequential)
        } else {
            checkbox(false)
                .label(download_in_order_label)
                .size(16.0)
                .style(checkbox_style)
        };

        row![start_cb, Space::new().width(18), sequential_cb]
            .align_y(Alignment::Center)
            .height(30)
            .into()
    }

    fn view_footer(&self) -> Element<'_, AddDialogMessage> {
        let cancel_text = self.text(TextKey::Cancel);
        let add_text = self.text(TextKey::AddTorrent);

        let summary_text = if self.files.is_empty() {
            String::new()
        } else {
            format!(
                "{} · {} of {} files",
                crate::format_bytes(self.selected_size()),
                self.selected_count(),
                self.files.len()
            )
        };

        let footer_summary = text(summary_text)
            .size(type_scale::CAPTION)
            .color(T.text_muted);

        let cancel_button = button(text(cancel_text).size(type_scale::BODY))
            .on_press(AddDialogMessage::Cancel)
            .style(style::secondary)
            .padding([0, 16])
            .height(36);

        let add_button = button(text(add_text).size(type_scale::BODY).font(font_semibold()))
            .on_press_maybe(self.can_confirm().then_some(AddDialogMessage::Confirm))
            .style(style::primary)
            .padding([0, 16])
            .height(36);

        row![
            footer_summary,
            Space::new().width(Fill),
            cancel_button,
            add_button,
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .height(44)
        .into()
    }
}

// ---------------------------------------------------------------------------
// Helpers & Styling
// ---------------------------------------------------------------------------

fn font_semibold() -> Font {
    Font {
        family: font::Family::Name("Inter"),
        weight: font::Weight::Semibold,
        ..Font::DEFAULT
    }
}

fn font_medium() -> Font {
    Font {
        family: font::Family::Name("Inter"),
        weight: font::Weight::Medium,
        ..Font::DEFAULT
    }
}

fn segment_button_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        if selected {
            button::Style {
                background: Some(Background::Color(T.raised)),
                text_color: T.text_primary,
                border: Border {
                    color: T.border_strong,
                    width: 1.0,
                    radius: 999.0.into(),
                },
                shadow: iced::Shadow::default(),
                snap: true,
            }
        } else {
            match status {
                button::Status::Active => button::Style {
                    background: Some(Background::Color(Color::TRANSPARENT)),
                    text_color: T.text_secondary,
                    border: Border::default(),
                    shadow: iced::Shadow::default(),
                    snap: true,
                },
                button::Status::Hovered | button::Status::Pressed => button::Style {
                    background: Some(Background::Color(T.overlay)),
                    text_color: T.text_primary,
                    border: Border::default(),
                    shadow: iced::Shadow::default(),
                    snap: true,
                },
                button::Status::Disabled => button::Style {
                    background: Some(Background::Color(Color::TRANSPARENT)),
                    text_color: T.text_disabled,
                    border: Border::default(),
                    shadow: iced::Shadow::default(),
                    snap: true,
                },
            }
        }
    }
}

fn checkbox_style(
    _theme: &Theme,
    status: iced::widget::checkbox::Status,
) -> iced::widget::checkbox::Style {
    match status {
        iced::widget::checkbox::Status::Active { is_checked } => iced::widget::checkbox::Style {
            background: Background::Color(if is_checked { T.accent_fill } else { T.raised }),
            icon_color: T.accent_on,
            border: Border {
                color: if is_checked {
                    T.accent_fill
                } else {
                    T.border_input
                },
                width: 1.0,
                radius: 4.0.into(),
            },
            text_color: Some(T.text_primary),
        },
        iced::widget::checkbox::Status::Hovered { is_checked } => iced::widget::checkbox::Style {
            background: Background::Color(if is_checked {
                T.accent_fill_hover
            } else {
                T.overlay
            }),
            icon_color: T.accent_on,
            border: Border {
                color: if is_checked {
                    T.accent_fill_hover
                } else {
                    T.border_strong
                },
                width: 1.0,
                radius: 4.0.into(),
            },
            text_color: Some(T.text_primary),
        },
        iced::widget::checkbox::Status::Disabled { is_checked } => iced::widget::checkbox::Style {
            background: Background::Color(if is_checked { T.overlay } else { T.canvas }),
            icon_color: T.text_disabled,
            border: Border {
                color: T.border_default,
                width: 1.0,
                radius: 4.0.into(),
            },
            text_color: Some(T.text_disabled),
        },
    }
}

/// Truncates string to `max_chars` adding a trailing ellipsis.
pub fn elide(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_owned();
    }
    let mut truncated: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

/// Truncates string from the middle with an ellipsis.
pub fn elide_middle(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 1 {
        return "…".to_owned();
    }
    let keep = max_chars - 1;
    let head = keep / 2;
    let tail = keep - head;
    let mut shortened: String = value.chars().take(head).collect();
    shortened.push('…');
    shortened.extend(value.chars().skip(count - tail));
    shortened
}

/// Joins torrent-relative components onto `root`, refusing anything that could
/// escape it (`..` or root components).
pub fn safe_join(root: &Path, relative: &Path) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => path.push(part),
            _ => return None,
        }
    }
    Some(path)
}

/// Measures free bytes on the volume holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return None;
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let wide: Vec<u16> = probe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut available = 0u64;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path and the out-pointer is
        // a live `u64`; the total-size arguments may be null.
        #[allow(unsafe_code)]
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { None } else { Some(available) }
    }
    #[cfg(not(windows))]
    {
        let _ = probe;
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn safe_join_prevents_directory_traversal() {
        let root = Path::new("C:/downloads");
        assert_eq!(
            safe_join(root, Path::new("Show/e01.mkv")),
            Some(PathBuf::from("C:/downloads/Show/e01.mkv"))
        );
        assert_eq!(safe_join(root, Path::new("../secret")), None);
        assert_eq!(safe_join(root, Path::new("foo/../../bar")), None);
        assert_eq!(safe_join(root, Path::new("/absolute")), None);
    }

    #[test]
    fn data_already_on_disk_reduces_missing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let save_dir = dir.path().join("Media");
        std::fs::create_dir_all(&save_dir).unwrap();
        std::fs::write(save_dir.join("video.mkv"), vec![0u8; 40]).unwrap();

        let mut state = AddDialogState::new(dir.path().display().to_string());
        state.files = vec![
            FileEntry {
                index: 0,
                path: PathBuf::from("Media/video.mkv"),
                length: 100,
                selected: true,
                priority: PriorityChoice::Normal,
            },
            FileEntry {
                index: 1,
                path: PathBuf::from("Media/sub.srt"),
                length: 50,
                selected: true,
                priority: PriorityChoice::Normal,
            },
        ];
        state.total_size = 150;
        state.free_space = Some(200);

        // 40 of 100 bytes are on disk for video.mkv, so missing is (100 - 40) + 50 = 110.
        assert_eq!(state.missing_bytes(), 110);
        assert!(!state.lacks_space());

        // Deselect sub.srt -> only video.mkv missing bytes counted: 60.
        state.files[1].selected = false;
        assert_eq!(state.missing_bytes(), 60);

        // Tight space check
        state.free_space = Some(59);
        assert!(state.lacks_space());
    }

    #[test]
    fn selection_presets_work_correctly() {
        let mut state = AddDialogState::new("D:/Downloads");
        state.files = vec![
            FileEntry::new(0, PathBuf::from("movie.mkv"), 1000),
            FileEntry::new(1, PathBuf::from("readme.txt"), 100),
            FileEntry::new(2, PathBuf::from("trailer.mp4"), 500),
        ];
        state.total_size = 1600;

        // Default all selected
        assert_eq!(state.selected_count(), 3);
        assert_eq!(state.selected_size(), 1600);

        // Select none
        state.select_none();
        assert_eq!(state.selected_count(), 0);
        assert_eq!(state.selected_size(), 0);
        assert!(
            state
                .files
                .iter()
                .all(|f| f.priority == PriorityChoice::Skip)
        );

        // Select video
        state.select_video();
        assert_eq!(state.selected_count(), 2);
        assert_eq!(state.selected_size(), 1500);
        assert!(state.files[0].selected);
        assert!(!state.files[1].selected);
        assert!(state.files[2].selected);
        assert_eq!(state.files[0].priority, PriorityChoice::Normal);
        assert_eq!(state.files[1].priority, PriorityChoice::Skip);
        assert_eq!(state.files[2].priority, PriorityChoice::Normal);

        // Select all
        state.select_all();
        assert_eq!(state.selected_count(), 3);
        assert_eq!(state.selected_size(), 1600);
        assert!(
            state
                .files
                .iter()
                .all(|f| f.priority == PriorityChoice::Normal)
        );
    }

    #[test]
    fn file_priority_and_toggle_interaction() {
        let mut state = AddDialogState::new("D:/Downloads");
        state.files = vec![FileEntry::new(0, PathBuf::from("a.txt"), 100)];

        // Setting priority to Skip sets selected to false
        state.set_file_priority(0, PriorityChoice::Skip);
        assert!(!state.files[0].selected);
        assert_eq!(state.files[0].priority, PriorityChoice::Skip);

        // Setting priority to High sets selected to true
        state.set_file_priority(0, PriorityChoice::High);
        assert!(state.files[0].selected);
        assert_eq!(state.files[0].priority, PriorityChoice::High);

        // Toggling from true to false sets priority to Skip
        state.toggle_file(0);
        assert!(!state.files[0].selected);
        assert_eq!(state.files[0].priority, PriorityChoice::Skip);

        // Toggling back to true restores Normal priority
        state.toggle_file(0);
        assert!(state.files[0].selected);
        assert_eq!(state.files[0].priority, PriorityChoice::Normal);
    }

    #[test]
    fn priority_choice_conversions() {
        assert_eq!(
            flux_api::Priority::from(PriorityChoice::Skip),
            flux_api::Priority::Skip
        );
        assert_eq!(
            flux_api::Priority::from(PriorityChoice::Normal),
            flux_api::Priority::Normal
        );
        assert_eq!(
            flux_api::Priority::from(PriorityChoice::High),
            flux_api::Priority::High
        );

        assert_eq!(
            PriorityChoice::from(flux_api::Priority::Skip),
            PriorityChoice::Skip
        );
        assert_eq!(
            PriorityChoice::from(flux_api::Priority::Normal),
            PriorityChoice::Normal
        );
        assert_eq!(
            PriorityChoice::from(flux_api::Priority::Low),
            PriorityChoice::Normal
        );
        assert_eq!(
            PriorityChoice::from(flux_api::Priority::High),
            PriorityChoice::High
        );
    }

    #[test]
    fn elide_helpers() {
        assert_eq!(elide("hello world", 5), "hell…");
        assert_eq!(elide("hello", 10), "hello");
        assert_eq!(elide_middle("abcdefghij", 6), "ab…hij");
    }

    #[test]
    fn localization_support() {
        let mut state = AddDialogState::new("D:/Downloads");
        assert_eq!(state.text(TextKey::AddTorrent), "Add torrent");

        state.language = Some(ResolvedLanguage::Ukrainian);
        assert_eq!(state.text(TextKey::AddTorrent), "Додати торент");
        assert_eq!(state.text(TextKey::SaveTo), "ЗБЕРЕГТИ В");
        assert_eq!(state.text(TextKey::Cancel), "Скасувати");
    }
}
