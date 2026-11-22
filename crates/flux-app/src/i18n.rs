//! Internationalization (i18n) foundation for Oxide Flux.
//!
//! Provides language selection, OS locale resolution, grammatical pluralization
//! rules for English and Ukrainian, and centralized translation dictionaries
//! for all user interface components.

use serde::{Deserialize, Serialize};

/// User-selectable language preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Follow the operating system's configured locale.
    #[default]
    System,
    /// English.
    English,
    /// Ukrainian (Українська).
    Ukrainian,
}

/// The effective resolved runtime language (never [`Language::System`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResolvedLanguage {
    /// English.
    #[default]
    English,
    /// Ukrainian.
    Ukrainian,
}

impl Language {
    /// Available options for settings UI.
    pub const ALL: [Self; 3] = [Self::System, Self::English, Self::Ukrainian];

    /// Resolves the effective language.
    ///
    /// When set to [`Language::System`], queries the system locale via [`sys_locale::get_locale()`].
    /// If the locale identifier begins with `"uk"` (e.g. `"uk"`, `"uk-UA"`, `"uk_UA"`), resolves
    /// to [`ResolvedLanguage::Ukrainian`], otherwise defaults to [`ResolvedLanguage::English`].
    #[must_use]
    pub fn resolve(&self) -> ResolvedLanguage {
        match self {
            Self::English => ResolvedLanguage::English,
            Self::Ukrainian => ResolvedLanguage::Ukrainian,
            Self::System => {
                if let Some(locale) = sys_locale::get_locale() {
                    resolve_locale_str(&locale)
                } else {
                    ResolvedLanguage::English
                }
            }
        }
    }

    /// Display label for selection dropdowns in settings.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::English => "English",
            Self::Ukrainian => "Українська",
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl From<Language> for ResolvedLanguage {
    fn from(lang: Language) -> Self {
        lang.resolve()
    }
}

impl ResolvedLanguage {
    /// All concrete languages supported.
    pub const ALL: [Self; 2] = [Self::English, Self::Ukrainian];

    /// Two-letter ISO 639-1 language code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Ukrainian => "uk",
        }
    }

    /// Native name (endonym) of the language.
    #[must_use]
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Ukrainian => "Українська",
        }
    }

    /// Translates a given [`TextKey`] into this language.
    #[must_use]
    pub fn t(self, key: TextKey) -> &'static str {
        tr(self, key)
    }

    /// Formats a count of torrents.
    #[must_use]
    pub fn torrents_count(self, n: usize) -> String {
        torrents_count(self, n)
    }

    /// Formats a count of files.
    #[must_use]
    pub fn files_count(self, n: usize) -> String {
        files_count(self, n)
    }

    /// Formats a selection count out of total files.
    #[must_use]
    pub fn files_selected(self, selected: usize, total: usize) -> String {
        files_selected(self, selected, total)
    }

    /// Formats a count of pieces.
    #[must_use]
    pub fn pieces_count(self, n: usize) -> String {
        pieces_count(self, n)
    }

    /// Formats a count of peers.
    #[must_use]
    pub fn peers_count(self, n: usize) -> String {
        peers_count(self, n)
    }

    /// Formats a count of seeds.
    #[must_use]
    pub fn seeds_count(self, n: usize) -> String {
        seeds_count(self, n)
    }
}

impl std::fmt::Display for ResolvedLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.native_name())
    }
}

/// Resolves a locale string (e.g. `"uk-UA"`, `"en_US"`) to [`ResolvedLanguage`].
#[must_use]
pub fn resolve_locale_str(locale: &str) -> ResolvedLanguage {
    let lower = locale.to_ascii_lowercase();
    if lower.starts_with("uk") {
        ResolvedLanguage::Ukrainian
    } else {
        ResolvedLanguage::English
    }
}

// ---------------------------------------------------------------------------
// Pluralization
// ---------------------------------------------------------------------------

/// Grammatical plural category for Ukrainian language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UkPlural {
    /// Form for 1, 21, 31, 101, etc. (`n % 10 == 1 && n % 100 != 11`).
    One,
    /// Form for 2-4, 22-24, 102-104, etc. (`2..=4 contains n % 10 && !(12..=14 contains n % 100)`).
    Few,
    /// Form for 0, 5-20, 25-30, 11-14, 111-114, etc.
    Many,
}

/// Computes the Ukrainian grammatical plural category for count `n`.
#[must_use]
pub fn uk_plural(n: usize) -> UkPlural {
    let rem10 = n % 10;
    let rem100 = n % 100;
    if rem10 == 1 && rem100 != 11 {
        UkPlural::One
    } else if (2..=4).contains(&rem10) && !(12..=14).contains(&rem100) {
        UkPlural::Few
    } else {
        UkPlural::Many
    }
}

/// Grammatical plural category for English language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnPlural {
    /// Form for 1.
    One,
    /// Form for any count other than 1.
    Other,
}

/// Computes the English grammatical plural category for count `n`.
#[must_use]
pub fn en_plural(n: usize) -> EnPlural {
    if n == 1 {
        EnPlural::One
    } else {
        EnPlural::Other
    }
}

// ---------------------------------------------------------------------------
// Plural string formatters
// ---------------------------------------------------------------------------

/// Formats a count of torrents according to grammatical rules of `lang`.
///
/// # Examples
/// ```
/// use flux_app::i18n::{torrents_count, ResolvedLanguage};
/// assert_eq!(torrents_count(ResolvedLanguage::English, 1), "1 torrent");
/// assert_eq!(torrents_count(ResolvedLanguage::English, 2), "2 torrents");
/// assert_eq!(torrents_count(ResolvedLanguage::Ukrainian, 1), "1 торент");
/// assert_eq!(torrents_count(ResolvedLanguage::Ukrainian, 2), "2 торенти");
/// assert_eq!(torrents_count(ResolvedLanguage::Ukrainian, 5), "5 торентів");
/// ```
#[must_use]
pub fn torrents_count(lang: ResolvedLanguage, n: usize) -> String {
    match lang {
        ResolvedLanguage::English => match en_plural(n) {
            EnPlural::One => "1 torrent".to_string(),
            EnPlural::Other => format!("{n} torrents"),
        },
        ResolvedLanguage::Ukrainian => match uk_plural(n) {
            UkPlural::One => format!("{n} торент"),
            UkPlural::Few => format!("{n} торенти"),
            UkPlural::Many => format!("{n} торентів"),
        },
    }
}

/// Formats a count of files according to grammatical rules of `lang`.
///
/// # Examples
/// ```
/// use flux_app::i18n::{files_count, ResolvedLanguage};
/// assert_eq!(files_count(ResolvedLanguage::English, 1), "1 file");
/// assert_eq!(files_count(ResolvedLanguage::English, 5), "5 files");
/// assert_eq!(files_count(ResolvedLanguage::Ukrainian, 1), "1 файл");
/// assert_eq!(files_count(ResolvedLanguage::Ukrainian, 2), "2 файли");
/// assert_eq!(files_count(ResolvedLanguage::Ukrainian, 5), "5 файлів");
/// ```
#[must_use]
pub fn files_count(lang: ResolvedLanguage, n: usize) -> String {
    match lang {
        ResolvedLanguage::English => match en_plural(n) {
            EnPlural::One => "1 file".to_string(),
            EnPlural::Other => format!("{n} files"),
        },
        ResolvedLanguage::Ukrainian => match uk_plural(n) {
            UkPlural::One => format!("{n} файл"),
            UkPlural::Few => format!("{n} файли"),
            UkPlural::Many => format!("{n} файлів"),
        },
    }
}

/// Formats a selection count out of total files according to grammatical rules of `lang`.
///
/// # Examples
/// ```
/// use flux_app::i18n::{files_selected, ResolvedLanguage};
/// assert_eq!(files_selected(ResolvedLanguage::English, 2, 5), "2 of 5 files selected");
/// assert_eq!(files_selected(ResolvedLanguage::Ukrainian, 2, 5), "обрано 2 з 5 файлів");
/// ```
#[must_use]
pub fn files_selected(lang: ResolvedLanguage, selected: usize, total: usize) -> String {
    match lang {
        ResolvedLanguage::English => {
            let noun = if total == 1 { "file" } else { "files" };
            format!("{selected} of {total} {noun} selected")
        }
        ResolvedLanguage::Ukrainian => {
            let noun = if total % 10 == 1 && total % 100 != 11 {
                "файлу"
            } else {
                "файлів"
            };
            format!("обрано {selected} з {total} {noun}")
        }
    }
}

/// Formats a count of pieces according to grammatical rules of `lang`.
///
/// # Examples
/// ```
/// use flux_app::i18n::{pieces_count, ResolvedLanguage};
/// assert_eq!(pieces_count(ResolvedLanguage::English, 1), "1 piece");
/// assert_eq!(pieces_count(ResolvedLanguage::English, 5), "5 pieces");
/// assert_eq!(pieces_count(ResolvedLanguage::Ukrainian, 1), "1 шматочок");
/// assert_eq!(pieces_count(ResolvedLanguage::Ukrainian, 2), "2 шматочки");
/// assert_eq!(pieces_count(ResolvedLanguage::Ukrainian, 5), "5 шматочків");
/// ```
#[must_use]
pub fn pieces_count(lang: ResolvedLanguage, n: usize) -> String {
    match lang {
        ResolvedLanguage::English => match en_plural(n) {
            EnPlural::One => "1 piece".to_string(),
            EnPlural::Other => format!("{n} pieces"),
        },
        ResolvedLanguage::Ukrainian => match uk_plural(n) {
            UkPlural::One => format!("{n} шматочок"),
            UkPlural::Few => format!("{n} шматочки"),
            UkPlural::Many => format!("{n} шматочків"),
        },
    }
}

/// Formats a count of peers according to grammatical rules of `lang`.
#[must_use]
pub fn peers_count(lang: ResolvedLanguage, n: usize) -> String {
    match lang {
        ResolvedLanguage::English => match en_plural(n) {
            EnPlural::One => "1 peer".to_string(),
            EnPlural::Other => format!("{n} peers"),
        },
        ResolvedLanguage::Ukrainian => match uk_plural(n) {
            UkPlural::One => format!("{n} пір"),
            UkPlural::Few => format!("{n} піри"),
            UkPlural::Many => format!("{n} пірів"),
        },
    }
}

/// Formats a count of seeds according to grammatical rules of `lang`.
#[must_use]
pub fn seeds_count(lang: ResolvedLanguage, n: usize) -> String {
    match lang {
        ResolvedLanguage::English => match en_plural(n) {
            EnPlural::One => "1 seed".to_string(),
            EnPlural::Other => format!("{n} seeds"),
        },
        ResolvedLanguage::Ukrainian => match uk_plural(n) {
            UkPlural::One => format!("{n} сід"),
            UkPlural::Few => format!("{n} сіди"),
            UkPlural::Many => format!("{n} сідів"),
        },
    }
}

// ---------------------------------------------------------------------------
// UI Text Keys & Translations
// ---------------------------------------------------------------------------

/// Keys representing all static user interface text elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextKey {
    // Library filters
    FilterAll,
    FilterDownloading,
    FilterSeeding,
    FilterCompleted,
    FilterPaused,
    FilterChecking,
    FilterError,
    SectionLibrary,

    // Actions & buttons
    AddTorrentFile,
    AddMagnet,
    FetchMetadata,
    Pause,
    Resume,
    Remove,
    Stop,
    Browse,
    Cancel,
    Save,
    Settings,
    RestoreDefaults,
    Clear,
    Close,
    Install,
    Uninstall,
    Copy,
    OpenFolder,
    CopyMagnetLink,
    DeleteTorrentAndData,

    // Add dialog
    AddTorrent,
    SaveTo,
    FolderToDownloadInto,
    FolderCreatedIfNotExist,
    NotEnoughSpace,
    StartImmediately,
    DownloadInOrder,
    Files,
    All,
    None,
    Video,
    Skip,
    Normal,
    High,
    Priority,

    // Status bar
    DhtOn,
    DhtOff,
    Active,
    Seeding,
    Paused,
    ShareRatio,
    Selected,

    // Settings tabs
    SettingsGeneral,
    SettingsDownloads,
    SettingsConnection,
    SettingsAppearance,
    SettingsAdvanced,
    SettingsAbout,
    SettingsPlayback,

    // Settings fields & labels
    LanguageLabel,
    ThemeLabel,
    DensityLabel,
    Engine,
    Bundled,
    SystemOption,
    Custom,
    Disabled,
    DefaultDownloadFolder,
    ListeningPort,
    EnableDht,
    MaxActiveDownloads,
    MaxUploadRate,
    MaxDownloadRate,
    ThemeDark,
    ThemeLight,
    ThemeAuto,
    DensityCompact,
    DensityComfortable,
    InstallPlaybackEngine,
    RemovePlaybackEngine,

    // Table columns
    ColName,
    ColSize,
    ColProgress,
    ColDown,
    ColUp,
    ColPeers,
    ColState,
    ColEta,

    // Details tabs
    TabGeneral,
    TabFiles,
    TabPeers,
    TabTrackers,
    TabPieces,
    TabSpeed,

    // Empty state & hints
    DropTorrentPrompt,
    NoTorrentsMatchFilter,
    NoResultsFound,
    SearchTorrentsPlaceholder,
    PlayingInEmbeddedPlayer,
    EscStopsPlayback,

    // Additional general & details keys
    FieldPieces,
    FieldPieceLength,
    FieldPrivate,
    FieldInfoHash,
    Yes,
    No,
    Session,
    Dismiss,
    DropTorrentRelease,
    DropTorrentOrPaste,
    NoTorrentsYet,
    EmptyStateDescription,
}

impl TextKey {
    /// List of all text keys for testing coverage.
    pub const ALL: [Self; 110] = [
        Self::FilterAll,
        Self::FilterDownloading,
        Self::FilterSeeding,
        Self::FilterCompleted,
        Self::FilterPaused,
        Self::FilterChecking,
        Self::FilterError,
        Self::SectionLibrary,
        Self::AddTorrentFile,
        Self::AddMagnet,
        Self::FetchMetadata,
        Self::Pause,
        Self::Resume,
        Self::Remove,
        Self::Stop,
        Self::Browse,
        Self::Cancel,
        Self::Save,
        Self::Settings,
        Self::RestoreDefaults,
        Self::Clear,
        Self::Close,
        Self::Install,
        Self::Uninstall,
        Self::Copy,
        Self::OpenFolder,
        Self::CopyMagnetLink,
        Self::DeleteTorrentAndData,
        Self::AddTorrent,
        Self::SaveTo,
        Self::FolderToDownloadInto,
        Self::FolderCreatedIfNotExist,
        Self::NotEnoughSpace,
        Self::StartImmediately,
        Self::DownloadInOrder,
        Self::Files,
        Self::All,
        Self::None,
        Self::Video,
        Self::Skip,
        Self::Normal,
        Self::High,
        Self::Priority,
        Self::DhtOn,
        Self::DhtOff,
        Self::Active,
        Self::Seeding,
        Self::Paused,
        Self::ShareRatio,
        Self::Selected,
        Self::SettingsGeneral,
        Self::SettingsDownloads,
        Self::SettingsConnection,
        Self::SettingsAppearance,
        Self::SettingsAdvanced,
        Self::SettingsAbout,
        Self::SettingsPlayback,
        Self::LanguageLabel,
        Self::ThemeLabel,
        Self::DensityLabel,
        Self::Engine,
        Self::Bundled,
        Self::SystemOption,
        Self::Custom,
        Self::Disabled,
        Self::DefaultDownloadFolder,
        Self::ListeningPort,
        Self::EnableDht,
        Self::MaxActiveDownloads,
        Self::MaxUploadRate,
        Self::MaxDownloadRate,
        Self::ThemeDark,
        Self::ThemeLight,
        Self::ThemeAuto,
        Self::DensityCompact,
        Self::DensityComfortable,
        Self::InstallPlaybackEngine,
        Self::RemovePlaybackEngine,
        Self::ColName,
        Self::ColSize,
        Self::ColProgress,
        Self::ColDown,
        Self::ColUp,
        Self::ColPeers,
        Self::ColState,
        Self::ColEta,
        Self::TabGeneral,
        Self::TabFiles,
        Self::TabPeers,
        Self::TabTrackers,
        Self::TabPieces,
        Self::TabSpeed,
        Self::DropTorrentPrompt,
        Self::NoTorrentsMatchFilter,
        Self::NoResultsFound,
        Self::SearchTorrentsPlaceholder,
        Self::PlayingInEmbeddedPlayer,
        Self::EscStopsPlayback,
        Self::FieldPieces,
        Self::FieldPieceLength,
        Self::FieldPrivate,
        Self::FieldInfoHash,
        Self::Yes,
        Self::No,
        Self::Session,
        Self::Dismiss,
        Self::DropTorrentRelease,
        Self::DropTorrentOrPaste,
        Self::NoTorrentsYet,
        Self::EmptyStateDescription,
    ];
}

/// Translates a [`TextKey`] into the specified [`ResolvedLanguage`].
#[must_use]
pub fn tr(lang: ResolvedLanguage, key: TextKey) -> &'static str {
    match lang {
        ResolvedLanguage::English => match key {
            // Filters
            TextKey::FilterAll => "All",
            TextKey::FilterDownloading => "Downloading",
            TextKey::FilterSeeding => "Seeding",
            TextKey::FilterCompleted => "Completed",
            TextKey::FilterPaused => "Paused",
            TextKey::FilterChecking => "Checking",
            TextKey::FilterError => "Error",
            TextKey::SectionLibrary => "LIBRARY",

            // Actions & buttons
            TextKey::AddTorrentFile => "Add .torrent",
            TextKey::AddMagnet => "Add magnet",
            TextKey::FetchMetadata => "Fetch metadata",
            TextKey::Pause => "Pause",
            TextKey::Resume => "Resume",
            TextKey::Remove => "Remove",
            TextKey::Stop => "Stop",
            TextKey::Browse => "Browse",
            TextKey::Cancel => "Cancel",
            TextKey::Save => "Save",
            TextKey::Settings => "Settings",
            TextKey::RestoreDefaults => "Restore defaults",
            TextKey::Clear => "Clear",
            TextKey::Close => "Close",
            TextKey::Install => "Install",
            TextKey::Uninstall => "Uninstall",
            TextKey::Copy => "Copy",
            TextKey::OpenFolder => "Open folder",
            TextKey::CopyMagnetLink => "Copy magnet link",
            TextKey::DeleteTorrentAndData => "Delete torrent and data",

            // Add dialog
            TextKey::AddTorrent => "Add torrent",
            TextKey::SaveTo => "SAVE TO",
            TextKey::FolderToDownloadInto => "Folder to download into",
            TextKey::FolderCreatedIfNotExist => "The folder is created if it does not exist.",
            TextKey::NotEnoughSpace => "Not enough space",
            TextKey::StartImmediately => "Start immediately",
            TextKey::DownloadInOrder => "Download in order (stream)",
            TextKey::Files => "Files",
            TextKey::All => "All",
            TextKey::None => "None",
            TextKey::Video => "Video",
            TextKey::Skip => "Skip",
            TextKey::Normal => "Normal",
            TextKey::High => "High",
            TextKey::Priority => "Priority",

            // Status bar
            TextKey::DhtOn => "DHT on",
            TextKey::DhtOff => "DHT off",
            TextKey::Active => "active",
            TextKey::Seeding => "seeding",
            TextKey::Paused => "paused",
            TextKey::ShareRatio => "share ratio",
            TextKey::Selected => "selected",

            // Settings tabs
            TextKey::SettingsGeneral => "General",
            TextKey::SettingsDownloads => "Downloads",
            TextKey::SettingsConnection => "Connection",
            TextKey::SettingsAppearance => "Appearance",
            TextKey::SettingsAdvanced => "Advanced",
            TextKey::SettingsAbout => "About",
            TextKey::SettingsPlayback => "Playback",

            // Settings fields & labels
            TextKey::LanguageLabel => "Language",
            TextKey::ThemeLabel => "Theme",
            TextKey::DensityLabel => "Density",
            TextKey::Engine => "Engine",
            TextKey::Bundled => "Bundled",
            TextKey::SystemOption => "System",
            TextKey::Custom => "Custom",
            TextKey::Disabled => "Disabled",
            TextKey::DefaultDownloadFolder => "Default download folder",
            TextKey::ListeningPort => "Listening port",
            TextKey::EnableDht => "Enable DHT",
            TextKey::MaxActiveDownloads => "Maximum active downloads",
            TextKey::MaxUploadRate => "Maximum upload rate",
            TextKey::MaxDownloadRate => "Maximum download rate",
            TextKey::ThemeDark => "Dark",
            TextKey::ThemeLight => "Light",
            TextKey::ThemeAuto => "Auto",
            TextKey::DensityCompact => "Compact",
            TextKey::DensityComfortable => "Comfortable",
            TextKey::InstallPlaybackEngine => "Install playback engine",
            TextKey::RemovePlaybackEngine => "Remove playback engine",

            // Table columns
            TextKey::ColName => "Name",
            TextKey::ColSize => "Size",
            TextKey::ColProgress => "Progress",
            TextKey::ColDown => "Down",
            TextKey::ColUp => "Up",
            TextKey::ColPeers => "Peers",
            TextKey::ColState => "State",
            TextKey::ColEta => "ETA",

            // Details tabs
            TextKey::TabGeneral => "General",
            TextKey::TabFiles => "Files",
            TextKey::TabPeers => "Peers",
            TextKey::TabTrackers => "Trackers",
            TextKey::TabPieces => "Pieces",
            TextKey::TabSpeed => "Speed",

            // Empty & hints
            TextKey::DropTorrentPrompt => "Drop .torrent files here",
            TextKey::NoTorrentsMatchFilter => "No torrents match the current filter",
            TextKey::NoResultsFound => "No results found",
            TextKey::SearchTorrentsPlaceholder => "Search torrents…",
            TextKey::PlayingInEmbeddedPlayer => "Playing in the embedded player",
            TextKey::EscStopsPlayback => "Esc stops playback",
            TextKey::FieldPieces => "Pieces",
            TextKey::FieldPieceLength => "Piece length",
            TextKey::FieldPrivate => "Private",
            TextKey::FieldInfoHash => "Info hash",
            TextKey::Yes => "yes",
            TextKey::No => "no",
            TextKey::Session => "Session",
            TextKey::Dismiss => "Dismiss",
            TextKey::DropTorrentRelease => "Release to add the torrent",
            TextKey::DropTorrentOrPaste => "or paste a magnet link",
            TextKey::NoTorrentsYet => "No torrents yet",
            TextKey::EmptyStateDescription => {
                "Add a .torrent file, paste a magnet link, or drop files anywhere in this window."
            }
        },
        ResolvedLanguage::Ukrainian => match key {
            // Filters
            TextKey::FilterAll => "Усі",
            TextKey::FilterDownloading => "Завантажуються",
            TextKey::FilterSeeding => "Роздаються",
            TextKey::FilterCompleted => "Завершені",
            TextKey::FilterPaused => "Призупинені",
            TextKey::FilterChecking => "Перевірка",
            TextKey::FilterError => "Помилка",
            TextKey::SectionLibrary => "БІБЛІОТЕКА",

            // Actions & buttons
            TextKey::AddTorrentFile => "Додати .torrent",
            TextKey::AddMagnet => "Додати magnet",
            TextKey::FetchMetadata => "Отримати метадані",
            TextKey::Pause => "Призупинити",
            TextKey::Resume => "Продовжити",
            TextKey::Remove => "Видалити",
            TextKey::Stop => "Зупинити",
            TextKey::Browse => "Огляд",
            TextKey::Cancel => "Скасувати",
            TextKey::Save => "Зберегти",
            TextKey::Settings => "Налаштування",
            TextKey::RestoreDefaults => "Відновити замовчування",
            TextKey::Clear => "Очистити",
            TextKey::Close => "Закрити",
            TextKey::Install => "Встановити",
            TextKey::Uninstall => "Видалити",
            TextKey::Copy => "Копіювати",
            TextKey::OpenFolder => "Відкрити папку",
            TextKey::CopyMagnetLink => "Скопіювати magnet-посилання",
            TextKey::DeleteTorrentAndData => "Видалити торент і дані",

            // Add dialog
            TextKey::AddTorrent => "Додати торент",
            TextKey::SaveTo => "ЗБЕРЕГТИ В",
            TextKey::FolderToDownloadInto => "Папка для завантаження",
            TextKey::FolderCreatedIfNotExist => "Папка буде створена, якщо вона не існує.",
            TextKey::NotEnoughSpace => "Недостатньо місця",
            TextKey::StartImmediately => "Запустити негайно",
            TextKey::DownloadInOrder => "Завантажувати послідовно (стрімінг)",
            TextKey::Files => "Файли",
            TextKey::All => "Усі",
            TextKey::None => "Жодного",
            TextKey::Video => "Відео",
            TextKey::Skip => "Пропустити",
            TextKey::Normal => "Звичайний",
            TextKey::High => "Високий",
            TextKey::Priority => "Пріоритет",

            // Status bar
            TextKey::DhtOn => "DHT увімк.",
            TextKey::DhtOff => "DHT вимк.",
            TextKey::Active => "активні",
            TextKey::Seeding => "роздаються",
            TextKey::Paused => "призупинені",
            TextKey::ShareRatio => "коефіцієнт роздачі",
            TextKey::Selected => "обрано",

            // Settings tabs
            TextKey::SettingsGeneral => "Загальні",
            TextKey::SettingsDownloads => "Завантаження",
            TextKey::SettingsConnection => "З'єднання",
            TextKey::SettingsAppearance => "Вигляд",
            TextKey::SettingsAdvanced => "Розширені",
            TextKey::SettingsAbout => "Про програму",
            TextKey::SettingsPlayback => "Відтворення",

            // Settings fields & labels
            TextKey::LanguageLabel => "Мова",
            TextKey::ThemeLabel => "Тема",
            TextKey::DensityLabel => "Щільність",
            TextKey::Engine => "Движок",
            TextKey::Bundled => "Вбудований",
            TextKey::SystemOption => "Системний",
            TextKey::Custom => "Власний",
            TextKey::Disabled => "Вимкнено",
            TextKey::DefaultDownloadFolder => "Стандартна папка завантажень",
            TextKey::ListeningPort => "Порт прослуховування",
            TextKey::EnableDht => "Увімкнути DHT",
            TextKey::MaxActiveDownloads => "Максимум активних завантажень",
            TextKey::MaxUploadRate => "Максимальна швидкість віддачі",
            TextKey::MaxDownloadRate => "Максимальна швидкість завантаження",
            TextKey::ThemeDark => "Темна",
            TextKey::ThemeLight => "Світла",
            TextKey::ThemeAuto => "Авто",
            TextKey::DensityCompact => "Компактна",
            TextKey::DensityComfortable => "Зручна",
            TextKey::InstallPlaybackEngine => "Встановити рушій відтворення",
            TextKey::RemovePlaybackEngine => "Видалити рушій відтворення",

            // Table columns
            TextKey::ColName => "Назва",
            TextKey::ColSize => "Розмір",
            TextKey::ColProgress => "Прогрес",
            TextKey::ColDown => "Завантаження",
            TextKey::ColUp => "Віддача",
            TextKey::ColPeers => "Піри",
            TextKey::ColState => "Стан",
            TextKey::ColEta => "Залишилося",

            // Details tabs
            TextKey::TabGeneral => "Загальні",
            TextKey::TabFiles => "Файли",
            TextKey::TabPeers => "Піри",
            TextKey::TabTrackers => "Трекери",
            TextKey::TabPieces => "Шматочки",
            TextKey::TabSpeed => "Швидкість",

            // Empty & hints
            TextKey::DropTorrentPrompt => "Перетягніть .torrent-файли сюди",
            TextKey::NoTorrentsMatchFilter => "Немає торентів, які відповідають поточному фільтру",
            TextKey::NoResultsFound => "Нічого не знайдено",
            TextKey::SearchTorrentsPlaceholder => "Пошук торентів…",
            TextKey::PlayingInEmbeddedPlayer => "Відтворюється у вбудованому плеєрі",
            TextKey::EscStopsPlayback => "Esc зупиняє відтворення",
            TextKey::FieldPieces => "Шматочки",
            TextKey::FieldPieceLength => "Розмір шматочка",
            TextKey::FieldPrivate => "Приватний",
            TextKey::FieldInfoHash => "Хеш роздачі",
            TextKey::Yes => "так",
            TextKey::No => "ні",
            TextKey::Session => "Сесія",
            TextKey::Dismiss => "Закрити",
            TextKey::DropTorrentRelease => "Відпустіть, щоб додати торент",
            TextKey::DropTorrentOrPaste => "або вставте magnet-посилання",
            TextKey::NoTorrentsYet => "Ще немає торентів",
            TextKey::EmptyStateDescription => {
                "Додайте .torrent-файл, вставте magnet-посилання або перетягніть файли у це вікно."
            }
        },
    }
}

/// Convenience alias for [`tr`].
#[must_use]
pub fn t(lang: ResolvedLanguage, key: TextKey) -> &'static str {
    tr(lang, key)
}

// ---------------------------------------------------------------------------
// Contextual Formatted Messages
// ---------------------------------------------------------------------------

/// Formats the "not enough disk space" warning hint in the Add Torrent dialog.
#[must_use]
pub fn not_enough_space_hint(lang: ResolvedLanguage, needed: &str, free: &str) -> String {
    match lang {
        ResolvedLanguage::English => {
            format!(
                "Not enough space: needs {needed}, only {free} free there. Pick another folder."
            )
        }
        ResolvedLanguage::Ukrainian => {
            format!(
                "Недостатньо місця: потрібно {needed}, доступно лише {free}. Оберіть іншу папку."
            )
        }
    }
}

/// Formats the "already in folder" notice in the Add Torrent dialog.
#[must_use]
pub fn already_in_folder_hint(lang: ResolvedLanguage, existing: &str, total: &str) -> String {
    match lang {
        ResolvedLanguage::English => {
            format!(
                "{existing} of {total} already in that folder; it will be verified, not downloaded again."
            )
        }
        ResolvedLanguage::Ukrainian => {
            format!(
                "{existing} з {total} вже у цій папці; файли будуть перевірені, а не завантажені повторно."
            )
        }
    }
}

/// Formats the status bar summary line.
#[must_use]
pub fn status_summary(
    lang: ResolvedLanguage,
    torrents: usize,
    active: usize,
    seeding: usize,
    paused: usize,
    selected: bool,
) -> String {
    let t_str = torrents_count(lang, torrents);
    match lang {
        ResolvedLanguage::English => {
            let sel = if selected { " · 1 selected" } else { "" };
            format!("{t_str} · {active} active · {seeding} seeding · {paused} paused{sel}")
        }
        ResolvedLanguage::Ukrainian => {
            let sel = if selected { " · 1 обрано" } else { "" };
            format!("{t_str} · {active} активні · {seeding} роздаються · {paused} призупинені{sel}")
        }
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ukrainian_pluralization_categories() {
        // One: 1, 21, 31, 101... (n % 10 == 1 && n % 100 != 11)
        assert_eq!(uk_plural(1), UkPlural::One);
        assert_eq!(uk_plural(21), UkPlural::One);
        assert_eq!(uk_plural(31), UkPlural::One);
        assert_eq!(uk_plural(101), UkPlural::One);
        assert_eq!(uk_plural(121), UkPlural::One);
        assert_eq!(uk_plural(1001), UkPlural::One);

        // Few: 2-4, 22-24, 32-34... (2..=4 contains n % 10 && !(12..=14 contains n % 100))
        assert_eq!(uk_plural(2), UkPlural::Few);
        assert_eq!(uk_plural(3), UkPlural::Few);
        assert_eq!(uk_plural(4), UkPlural::Few);
        assert_eq!(uk_plural(22), UkPlural::Few);
        assert_eq!(uk_plural(23), UkPlural::Few);
        assert_eq!(uk_plural(24), UkPlural::Few);
        assert_eq!(uk_plural(32), UkPlural::Few);
        assert_eq!(uk_plural(102), UkPlural::Few);
        assert_eq!(uk_plural(104), UkPlural::Few);

        // Many: 0, 5-20, 25-30, 11-14...
        assert_eq!(uk_plural(0), UkPlural::Many);
        assert_eq!(uk_plural(5), UkPlural::Many);
        assert_eq!(uk_plural(6), UkPlural::Many);
        assert_eq!(uk_plural(10), UkPlural::Many);
        assert_eq!(uk_plural(11), UkPlural::Many);
        assert_eq!(uk_plural(12), UkPlural::Many);
        assert_eq!(uk_plural(13), UkPlural::Many);
        assert_eq!(uk_plural(14), UkPlural::Many);
        assert_eq!(uk_plural(15), UkPlural::Many);
        assert_eq!(uk_plural(20), UkPlural::Many);
        assert_eq!(uk_plural(25), UkPlural::Many);
        assert_eq!(uk_plural(30), UkPlural::Many);
        assert_eq!(uk_plural(111), UkPlural::Many);
        assert_eq!(uk_plural(112), UkPlural::Many);
        assert_eq!(uk_plural(114), UkPlural::Many);
    }

    #[test]
    fn test_english_pluralization_categories() {
        assert_eq!(en_plural(1), EnPlural::One);
        assert_eq!(en_plural(0), EnPlural::Other);
        assert_eq!(en_plural(2), EnPlural::Other);
        assert_eq!(en_plural(5), EnPlural::Other);
        assert_eq!(en_plural(11), EnPlural::Other);
        assert_eq!(en_plural(21), EnPlural::Other);
        assert_eq!(en_plural(101), EnPlural::Other);
    }

    #[test]
    fn test_torrents_count_plural_strings() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        // English
        assert_eq!(torrents_count(en, 0), "0 torrents");
        assert_eq!(torrents_count(en, 1), "1 torrent");
        assert_eq!(torrents_count(en, 2), "2 torrents");
        assert_eq!(torrents_count(en, 5), "5 torrents");
        assert_eq!(torrents_count(en, 21), "21 torrents");

        // Ukrainian
        assert_eq!(torrents_count(uk, 0), "0 торентів");
        assert_eq!(torrents_count(uk, 1), "1 торент");
        assert_eq!(torrents_count(uk, 2), "2 торенти");
        assert_eq!(torrents_count(uk, 4), "4 торенти");
        assert_eq!(torrents_count(uk, 5), "5 торентів");
        assert_eq!(torrents_count(uk, 11), "11 торентів");
        assert_eq!(torrents_count(uk, 12), "12 торентів");
        assert_eq!(torrents_count(uk, 14), "14 торентів");
        assert_eq!(torrents_count(uk, 20), "20 торентів");
        assert_eq!(torrents_count(uk, 21), "21 торент");
        assert_eq!(torrents_count(uk, 22), "22 торенти");
        assert_eq!(torrents_count(uk, 25), "25 торентів");
        assert_eq!(torrents_count(uk, 101), "101 торент");
        assert_eq!(torrents_count(uk, 111), "111 торентів");
        assert_eq!(torrents_count(uk, 112), "112 торентів");
        assert_eq!(torrents_count(uk, 114), "114 торентів");
    }

    #[test]
    fn test_files_count_plural_strings() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        // English
        assert_eq!(files_count(en, 1), "1 file");
        assert_eq!(files_count(en, 2), "2 files");
        assert_eq!(files_count(en, 5), "5 files");

        // Ukrainian
        assert_eq!(files_count(uk, 0), "0 файлів");
        assert_eq!(files_count(uk, 1), "1 файл");
        assert_eq!(files_count(uk, 2), "2 файли");
        assert_eq!(files_count(uk, 4), "4 файли");
        assert_eq!(files_count(uk, 5), "5 файлів");
        assert_eq!(files_count(uk, 11), "11 файлів");
        assert_eq!(files_count(uk, 14), "14 файлів");
        assert_eq!(files_count(uk, 21), "21 файл");
        assert_eq!(files_count(uk, 22), "22 файли");
        assert_eq!(files_count(uk, 25), "25 файлів");
        assert_eq!(files_count(uk, 101), "101 файл");
    }

    #[test]
    fn test_files_selected_plural_strings() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(files_selected(en, 2, 5), "2 of 5 files selected");
        assert_eq!(files_selected(en, 1, 1), "1 of 1 file selected");
        assert_eq!(files_selected(uk, 2, 5), "обрано 2 з 5 файлів");
        assert_eq!(files_selected(uk, 1, 1), "обрано 1 з 1 файлу");
        assert_eq!(files_selected(uk, 3, 21), "обрано 3 з 21 файлу");
        assert_eq!(files_selected(uk, 5, 22), "обрано 5 з 22 файлів");
    }

    #[test]
    fn test_pieces_count_plural_strings() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(pieces_count(en, 1), "1 piece");
        assert_eq!(pieces_count(en, 5), "5 pieces");

        assert_eq!(pieces_count(uk, 0), "0 шматочків");
        assert_eq!(pieces_count(uk, 1), "1 шматочок");
        assert_eq!(pieces_count(uk, 2), "2 шматочки");
        assert_eq!(pieces_count(uk, 4), "4 шматочки");
        assert_eq!(pieces_count(uk, 5), "5 шматочків");
        assert_eq!(pieces_count(uk, 11), "11 шматочків");
        assert_eq!(pieces_count(uk, 21), "21 шматочок");
        assert_eq!(pieces_count(uk, 22), "22 шматочки");
        assert_eq!(pieces_count(uk, 25), "25 шматочків");
        assert_eq!(pieces_count(uk, 101), "101 шматочок");
    }

    #[test]
    fn test_peers_and_seeds_count() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(peers_count(en, 1), "1 peer");
        assert_eq!(peers_count(en, 4), "4 peers");
        assert_eq!(peers_count(uk, 1), "1 пір");
        assert_eq!(peers_count(uk, 2), "2 піри");
        assert_eq!(peers_count(uk, 5), "5 пірів");

        assert_eq!(seeds_count(en, 1), "1 seed");
        assert_eq!(seeds_count(en, 10), "10 seeds");
        assert_eq!(seeds_count(uk, 1), "1 сід");
        assert_eq!(seeds_count(uk, 3), "3 сіди");
        assert_eq!(seeds_count(uk, 20), "20 сідів");
    }

    #[test]
    fn test_locale_string_resolution() {
        assert_eq!(resolve_locale_str("uk"), ResolvedLanguage::Ukrainian);
        assert_eq!(resolve_locale_str("uk-UA"), ResolvedLanguage::Ukrainian);
        assert_eq!(
            resolve_locale_str("uk_UA.UTF-8"),
            ResolvedLanguage::Ukrainian
        );
        assert_eq!(resolve_locale_str("UK"), ResolvedLanguage::Ukrainian);
        assert_eq!(resolve_locale_str("en"), ResolvedLanguage::English);
        assert_eq!(resolve_locale_str("en-US"), ResolvedLanguage::English);
        assert_eq!(resolve_locale_str("de-DE"), ResolvedLanguage::English);
        assert_eq!(resolve_locale_str("fr"), ResolvedLanguage::English);
        assert_eq!(resolve_locale_str(""), ResolvedLanguage::English);
    }

    #[test]
    fn test_language_resolve() {
        assert_eq!(Language::English.resolve(), ResolvedLanguage::English);
        assert_eq!(Language::Ukrainian.resolve(), ResolvedLanguage::Ukrainian);
        // Language::System returns a valid ResolvedLanguage without panic
        let resolved = Language::System.resolve();
        assert!(matches!(
            resolved,
            ResolvedLanguage::English | ResolvedLanguage::Ukrainian
        ));
    }

    #[test]
    fn test_library_filter_translations() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(tr(en, TextKey::FilterAll), "All");
        assert_eq!(tr(uk, TextKey::FilterAll), "Усі");

        assert_eq!(tr(en, TextKey::FilterDownloading), "Downloading");
        assert_eq!(tr(uk, TextKey::FilterDownloading), "Завантажуються");

        assert_eq!(tr(en, TextKey::FilterSeeding), "Seeding");
        assert_eq!(tr(uk, TextKey::FilterSeeding), "Роздаються");

        assert_eq!(tr(en, TextKey::FilterCompleted), "Completed");
        assert_eq!(tr(uk, TextKey::FilterCompleted), "Завершені");

        assert_eq!(tr(en, TextKey::FilterPaused), "Paused");
        assert_eq!(tr(uk, TextKey::FilterPaused), "Призупинені");

        assert_eq!(tr(en, TextKey::FilterChecking), "Checking");
        assert_eq!(tr(uk, TextKey::FilterChecking), "Перевірка");

        assert_eq!(tr(en, TextKey::FilterError), "Error");
        assert_eq!(tr(uk, TextKey::FilterError), "Помилка");
    }

    #[test]
    fn test_action_and_button_translations() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(tr(en, TextKey::AddTorrentFile), "Add .torrent");
        assert_eq!(tr(uk, TextKey::AddTorrentFile), "Додати .torrent");

        assert_eq!(tr(en, TextKey::AddMagnet), "Add magnet");
        assert_eq!(tr(uk, TextKey::AddMagnet), "Додати magnet");

        assert_eq!(tr(en, TextKey::FetchMetadata), "Fetch metadata");
        assert_eq!(tr(uk, TextKey::FetchMetadata), "Отримати метадані");

        assert_eq!(tr(en, TextKey::Pause), "Pause");
        assert_eq!(tr(uk, TextKey::Pause), "Призупинити");

        assert_eq!(tr(en, TextKey::Resume), "Resume");
        assert_eq!(tr(uk, TextKey::Resume), "Продовжити");

        assert_eq!(tr(en, TextKey::Remove), "Remove");
        assert_eq!(tr(uk, TextKey::Remove), "Видалити");

        assert_eq!(tr(en, TextKey::Stop), "Stop");
        assert_eq!(tr(uk, TextKey::Stop), "Зупинити");

        assert_eq!(tr(en, TextKey::Browse), "Browse");
        assert_eq!(tr(uk, TextKey::Browse), "Огляд");

        assert_eq!(tr(en, TextKey::Cancel), "Cancel");
        assert_eq!(tr(uk, TextKey::Cancel), "Скасувати");

        assert_eq!(tr(en, TextKey::Save), "Save");
        assert_eq!(tr(uk, TextKey::Save), "Зберегти");

        assert_eq!(tr(en, TextKey::Settings), "Settings");
        assert_eq!(tr(uk, TextKey::Settings), "Налаштування");

        assert_eq!(tr(en, TextKey::RestoreDefaults), "Restore defaults");
        assert_eq!(tr(uk, TextKey::RestoreDefaults), "Відновити замовчування");
    }

    #[test]
    fn test_add_dialog_translations() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(tr(en, TextKey::AddTorrent), "Add torrent");
        assert_eq!(tr(uk, TextKey::AddTorrent), "Додати торент");

        assert_eq!(tr(en, TextKey::SaveTo), "SAVE TO");
        assert_eq!(tr(uk, TextKey::SaveTo), "ЗБЕРЕГТИ В");

        assert_eq!(
            tr(en, TextKey::FolderToDownloadInto),
            "Folder to download into"
        );
        assert_eq!(
            tr(uk, TextKey::FolderToDownloadInto),
            "Папка для завантаження"
        );

        assert_eq!(
            tr(en, TextKey::FolderCreatedIfNotExist),
            "The folder is created if it does not exist."
        );
        assert_eq!(
            tr(uk, TextKey::FolderCreatedIfNotExist),
            "Папка буде створена, якщо вона не існує."
        );

        assert_eq!(tr(en, TextKey::NotEnoughSpace), "Not enough space");
        assert_eq!(tr(uk, TextKey::NotEnoughSpace), "Недостатньо місця");

        assert_eq!(tr(en, TextKey::StartImmediately), "Start immediately");
        assert_eq!(tr(uk, TextKey::StartImmediately), "Запустити негайно");

        assert_eq!(
            tr(en, TextKey::DownloadInOrder),
            "Download in order (stream)"
        );
        assert_eq!(
            tr(uk, TextKey::DownloadInOrder),
            "Завантажувати послідовно (стрімінг)"
        );

        assert_eq!(tr(en, TextKey::Files), "Files");
        assert_eq!(tr(uk, TextKey::Files), "Файли");

        assert_eq!(tr(en, TextKey::All), "All");
        assert_eq!(tr(uk, TextKey::All), "Усі");

        assert_eq!(tr(en, TextKey::None), "None");
        assert_eq!(tr(uk, TextKey::None), "Жодного");

        assert_eq!(tr(en, TextKey::Video), "Video");
        assert_eq!(tr(uk, TextKey::Video), "Відео");

        assert_eq!(tr(en, TextKey::Skip), "Skip");
        assert_eq!(tr(uk, TextKey::Skip), "Пропустити");

        assert_eq!(tr(en, TextKey::Normal), "Normal");
        assert_eq!(tr(uk, TextKey::Normal), "Звичайний");

        assert_eq!(tr(en, TextKey::High), "High");
        assert_eq!(tr(uk, TextKey::High), "Високий");

        assert_eq!(tr(en, TextKey::Priority), "Priority");
        assert_eq!(tr(uk, TextKey::Priority), "Пріоритет");
    }

    #[test]
    fn test_status_bar_translations() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(tr(en, TextKey::DhtOn), "DHT on");
        assert_eq!(tr(uk, TextKey::DhtOn), "DHT увімк.");

        assert_eq!(tr(en, TextKey::DhtOff), "DHT off");
        assert_eq!(tr(uk, TextKey::DhtOff), "DHT вимк.");

        assert_eq!(tr(en, TextKey::Active), "active");
        assert_eq!(tr(uk, TextKey::Active), "активні");

        assert_eq!(tr(en, TextKey::Seeding), "seeding");
        assert_eq!(tr(uk, TextKey::Seeding), "роздаються");

        assert_eq!(tr(en, TextKey::Paused), "paused");
        assert_eq!(tr(uk, TextKey::Paused), "призупинені");

        assert_eq!(tr(en, TextKey::ShareRatio), "share ratio");
        assert_eq!(tr(uk, TextKey::ShareRatio), "коефіцієнт роздачі");
    }

    #[test]
    fn test_settings_tabs_and_labels() {
        let en = ResolvedLanguage::English;
        let uk = ResolvedLanguage::Ukrainian;

        assert_eq!(tr(en, TextKey::SettingsGeneral), "General");
        assert_eq!(tr(uk, TextKey::SettingsGeneral), "Загальні");

        assert_eq!(tr(en, TextKey::SettingsDownloads), "Downloads");
        assert_eq!(tr(uk, TextKey::SettingsDownloads), "Завантаження");

        assert_eq!(tr(en, TextKey::SettingsConnection), "Connection");
        assert_eq!(tr(uk, TextKey::SettingsConnection), "З'єднання");

        assert_eq!(tr(en, TextKey::SettingsAppearance), "Appearance");
        assert_eq!(tr(uk, TextKey::SettingsAppearance), "Вигляд");

        assert_eq!(tr(en, TextKey::SettingsAdvanced), "Advanced");
        assert_eq!(tr(uk, TextKey::SettingsAdvanced), "Розширені");

        assert_eq!(tr(en, TextKey::SettingsAbout), "About");
        assert_eq!(tr(uk, TextKey::SettingsAbout), "Про програму");

        assert_eq!(tr(en, TextKey::SettingsPlayback), "Playback");
        assert_eq!(tr(uk, TextKey::SettingsPlayback), "Відтворення");

        assert_eq!(tr(en, TextKey::LanguageLabel), "Language");
        assert_eq!(tr(uk, TextKey::LanguageLabel), "Мова");

        assert_eq!(tr(en, TextKey::ThemeLabel), "Theme");
        assert_eq!(tr(uk, TextKey::ThemeLabel), "Тема");

        assert_eq!(tr(en, TextKey::DensityLabel), "Density");
        assert_eq!(tr(uk, TextKey::DensityLabel), "Щільність");
    }

    #[test]
    fn test_all_keys_non_empty_in_both_languages() {
        for key in TextKey::ALL {
            let en_str = tr(ResolvedLanguage::English, key);
            let uk_str = tr(ResolvedLanguage::Ukrainian, key);
            assert!(!en_str.is_empty(), "Empty English string for {:?}", key);
            assert!(!uk_str.is_empty(), "Empty Ukrainian string for {:?}", key);
        }
    }

    #[test]
    fn test_hint_formatters() {
        assert_eq!(
            not_enough_space_hint(ResolvedLanguage::English, "10 GB", "2 GB"),
            "Not enough space: needs 10 GB, only 2 GB free there. Pick another folder."
        );
        assert_eq!(
            not_enough_space_hint(ResolvedLanguage::Ukrainian, "10 GB", "2 GB"),
            "Недостатньо місця: потрібно 10 GB, доступно лише 2 GB. Оберіть іншу папку."
        );

        assert_eq!(
            already_in_folder_hint(ResolvedLanguage::English, "500 MB", "1 GB"),
            "500 MB of 1 GB already in that folder; it will be verified, not downloaded again."
        );
        assert_eq!(
            already_in_folder_hint(ResolvedLanguage::Ukrainian, "500 MB", "1 GB"),
            "500 MB з 1 GB вже у цій папці; файли будуть перевірені, а не завантажені повторно."
        );
    }

    #[test]
    fn test_status_summary_formatter() {
        assert_eq!(
            status_summary(ResolvedLanguage::English, 1, 1, 0, 0, false),
            "1 torrent · 1 active · 0 seeding · 0 paused"
        );
        assert_eq!(
            status_summary(ResolvedLanguage::English, 3, 2, 1, 0, true),
            "3 torrents · 2 active · 1 seeding · 0 paused · 1 selected"
        );
        assert_eq!(
            status_summary(ResolvedLanguage::Ukrainian, 1, 1, 0, 0, false),
            "1 торент · 1 активні · 0 роздаються · 0 призупинені"
        );
        assert_eq!(
            status_summary(ResolvedLanguage::Ukrainian, 3, 2, 1, 0, true),
            "3 торенти · 2 активні · 1 роздаються · 0 призупинені · 1 обрано"
        );
    }

    #[test]
    fn test_language_serialization() -> Result<(), serde_json::Error> {
        let json_sys = serde_json::to_string(&Language::System)?;
        assert_eq!(json_sys, "\"system\"");
        let de_sys: Language = serde_json::from_str(&json_sys)?;
        assert_eq!(de_sys, Language::System);

        let json_en = serde_json::to_string(&Language::English)?;
        assert_eq!(json_en, "\"english\"");
        let de_en: Language = serde_json::from_str(&json_en)?;
        assert_eq!(de_en, Language::English);

        let json_uk = serde_json::to_string(&Language::Ukrainian)?;
        assert_eq!(json_uk, "\"ukrainian\"");
        let de_uk: Language = serde_json::from_str(&json_uk)?;
        assert_eq!(de_uk, Language::Ukrainian);

        Ok(())
    }
}
