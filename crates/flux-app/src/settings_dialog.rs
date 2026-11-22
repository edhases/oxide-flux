//! Settings dialog / panel for Oxide Flux.
//!
//! Follows `docs/design/ui-spec.md` (§5.5):
//! - 900×640 modal sheet with 220 px left navigation and scrollable content pane.
//! - 56 px setting rows with title, description, and right-aligned controls.
//! - "restart ●" warning badge for engine settings requiring restart.
//! - Bottom action bar: "Restore defaults" on left, "Cancel" and "Save" on right.

use std::path::PathBuf;

use flux_daemon::Config as DaemonConfig;
use flux_daemon::Preallocate;
use iced::widget::{
    Space, button, column, container, mouse_area, opaque, pick_list, row, scrollable, stack, text,
    text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Fill, Font, Padding, Task, font};
use serde::{Deserialize, Serialize};

pub use crate::i18n::Language;
use crate::i18n::{ResolvedLanguage, TextKey, tr};
use crate::icons::{self, icon};
use crate::launch::{PlaybackBackend, PlaybackSettings, Settings as DesktopSettings};
use crate::playback;
use crate::style;
use crate::theme::T;
use crate::theme::type_scale::{BODY, CAPTION, MICRO};

/// Active tab in the Settings navigation sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SettingsTab {
    #[default]
    General,
    Downloads,
    Connection,
    Appearance,
    Playback,
    Advanced,
    About,
}

impl SettingsTab {
    /// All available tabs in display order.
    pub const ALL: [Self; 7] = [
        Self::General,
        Self::Downloads,
        Self::Connection,
        Self::Appearance,
        Self::Playback,
        Self::Advanced,
        Self::About,
    ];

    /// Human-readable tab title.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Downloads => "Downloads",
            Self::Connection => "Connection",
            Self::Appearance => "Appearance",
            Self::Playback => "Playback",
            Self::Advanced => "Advanced",
            Self::About => "About",
        }
    }

    /// Localized tab title.
    #[must_use]
    pub fn label_tr(self, lang: ResolvedLanguage) -> &'static str {
        match self {
            Self::General => tr(lang, TextKey::SettingsGeneral),
            Self::Downloads => tr(lang, TextKey::SettingsDownloads),
            Self::Connection => tr(lang, TextKey::SettingsConnection),
            Self::Appearance => tr(lang, TextKey::SettingsAppearance),
            Self::Playback => tr(lang, TextKey::SettingsPlayback),
            Self::Advanced => tr(lang, TextKey::SettingsAdvanced),
            Self::About => tr(lang, TextKey::SettingsAbout),
        }
    }

    /// Tab icon SVG bytes.
    #[must_use]
    pub fn icon(self) -> &'static [u8] {
        match self {
            Self::General => icons::SETTINGS,
            Self::Downloads => icons::DOWNLOAD,
            Self::Connection => icons::ROTATE,
            Self::Appearance => icons::FILTER,
            Self::Playback => icons::PLAY,
            Self::Advanced => icons::HARD_DRIVE,
            Self::About => icons::CHECK_CIRCLE,
        }
    }
}

impl std::fmt::Display for SettingsTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Color theme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeMode {
    /// Available theme options.
    pub const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    /// Display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

impl std::fmt::Display for ThemeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Table and UI layout density.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

impl Density {
    /// Available density options.
    pub const ALL: [Self; 2] = [Self::Comfortable, Self::Compact];

    /// Display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Comfortable => "Comfortable (32 px)",
            Self::Compact => "Compact (28 px)",
        }
    }
}

impl std::fmt::Display for Density {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Disk preallocation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PreallocateMode {
    #[default]
    Sparse,
    Full,
}

impl PreallocateMode {
    /// Available options.
    pub const ALL: [Self; 2] = [Self::Sparse, Self::Full];

    /// Display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Sparse => "Sparse (Fast)",
            Self::Full => "Full (Zeroed)",
        }
    }
}

impl std::fmt::Display for PreallocateMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl From<Preallocate> for PreallocateMode {
    fn from(p: Preallocate) -> Self {
        match p {
            Preallocate::Zeroed => Self::Full,
            _ => Self::Sparse,
        }
    }
}

impl From<PreallocateMode> for Preallocate {
    fn from(m: PreallocateMode) -> Self {
        match m {
            PreallocateMode::Sparse => Self::Sparse,
            PreallocateMode::Full => Self::Zeroed,
        }
    }
}

/// Complete configuration and UI state for the settings dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsState {
    /// Currently selected tab.
    pub active_tab: SettingsTab,

    // Appearance
    /// Color theme.
    pub theme: ThemeMode,
    /// Row and layout density.
    pub density: Density,
    /// Interface language.
    pub language: Language,
    /// Whether decorative motions/animations are disabled.
    pub reduce_motion: bool,
    /// Whether idle speeds render as "—" rather than "0 B/s".
    pub hide_zero_speeds: bool,

    // Downloads
    /// Default download folder.
    pub default_download_path: String,
    /// Disk file allocation mode.
    pub preallocate: PreallocateMode,
    /// Max simultaneous active downloads.
    pub max_active_downloads: usize,
    /// Max simultaneous active seeds.
    pub max_active_seeds: usize,

    // Connection
    /// Peer listen port (0 = ephemeral random).
    pub listen_port: u16,
    /// Max peers per torrent.
    pub max_peers: usize,
    /// Whether BEP 5 Mainline DHT is enabled.
    pub dht_enabled: bool,

    // Playback
    /// Selected playback engine backend.
    pub backend: PlaybackBackend,
    /// Custom path to mpv executable.
    pub custom_mpv_path: String,
    /// Installed plugin status description.
    pub plugin_status: Option<String>,
    /// Whether a plugin install or removal task is currently in flight.
    #[serde(skip)]
    pub plugin_busy: bool,

    // Advanced
    /// Whether session state is persisted across application restarts.
    pub persist_session: bool,
    /// Number of worker threads for async I/O.
    pub worker_threads: usize,
    /// Number of threads for hashing/verifying pieces.
    pub verifier_threads: usize,
    /// Read-only path to session state file.
    pub state_file_path: String,

    // About
    /// Application package version.
    pub app_version: String,
    /// Engine/daemon version.
    pub engine_version: String,
    /// License overview.
    pub licenses_info: String,
    /// Project GitHub repository URL.
    pub repo_url: String,

    // General / System integration
    /// Register file associations on startup.
    pub register_associations: bool,

    // Text input buffers for numeric fields to preserve transient typing state
    #[serde(skip)]
    pub port_input: String,
    #[serde(skip)]
    pub max_peers_input: String,
    #[serde(skip)]
    pub max_downloads_input: String,
    #[serde(skip)]
    pub max_seeds_input: String,
    #[serde(skip)]
    pub worker_threads_input: String,
    #[serde(skip)]
    pub verifier_threads_input: String,

    // Transient UI status indicators
    #[serde(skip)]
    pub is_dirty: bool,
    #[serde(skip)]
    pub notice: Option<String>,
    #[serde(skip)]
    pub error: Option<String>,
}

impl Default for SettingsState {
    fn default() -> Self {
        let app_version = env!("CARGO_PKG_VERSION").to_owned();
        let engine_version = format!("{app_version} (fluxd)");
        let licenses_info = "Lucide (ISC) · Inter (OFL 1.1) · iced (MIT)".to_owned();
        let repo_url = "https://github.com/edhases/oxide-flux".to_owned();
        let state_file_path = crate::launch::config_dir()
            .join(".fluxd-state.json")
            .display()
            .to_string();

        let default_download_path = DaemonConfig::default().download_dir.display().to_string();

        let installed = playback::installed_version();
        let bundled = playback::bundled_mpv();
        let plugin_status = match (&installed, &bundled) {
            (Some(version), Some(path)) => {
                Some(format!("Bundled mpv {version} at {}", path.display()))
            }
            (Some(version), None) => Some(format!("Bundled mpv {version} installed")),
            (None, Some(path)) => Some(format!("Local mpv found at {}", path.display())),
            (None, None) => None,
        };

        Self {
            active_tab: SettingsTab::General,
            theme: ThemeMode::System,
            density: Density::Comfortable,
            language: Language::System,
            reduce_motion: false,
            hide_zero_speeds: false,

            default_download_path,
            preallocate: PreallocateMode::Sparse,
            max_active_downloads: 3,
            max_active_seeds: 5,

            listen_port: 0,
            max_peers: 50,
            dht_enabled: true,

            backend: PlaybackBackend::Bundled,
            custom_mpv_path: String::new(),
            plugin_status,
            plugin_busy: false,

            persist_session: true,
            worker_threads: 4,
            verifier_threads: 2,
            state_file_path,

            app_version,
            engine_version,
            licenses_info,
            repo_url,

            register_associations: true,

            port_input: "0".to_owned(),
            max_peers_input: "50".to_owned(),
            max_downloads_input: "3".to_owned(),
            max_seeds_input: "5".to_owned(),
            worker_threads_input: "4".to_owned(),
            verifier_threads_input: "2".to_owned(),

            is_dirty: false,
            notice: None,
            error: None,
        }
    }
}

impl SettingsState {
    /// Creates a settings state initialized from the existing desktop and daemon configs.
    #[must_use]
    pub fn from_configs(desktop: &DesktopSettings, daemon: &DaemonConfig) -> Self {
        let custom_mpv_path = desktop
            .playback
            .mpv_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let state_file_path = daemon
            .state_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| {
                crate::launch::config_dir()
                    .join(".fluxd-state.json")
                    .display()
                    .to_string()
            });

        Self {
            register_associations: desktop.register_associations,
            backend: desktop.playback.backend,
            custom_mpv_path,
            default_download_path: daemon.download_dir.display().to_string(),
            preallocate: daemon.preallocate.into(),
            max_active_downloads: daemon.max_active_downloads,
            max_downloads_input: daemon.max_active_downloads.to_string(),
            max_active_seeds: daemon.max_active_seeds,
            max_seeds_input: daemon.max_active_seeds.to_string(),
            listen_port: daemon.listen_port,
            port_input: daemon.listen_port.to_string(),
            max_peers: daemon.max_peers_per_torrent,
            max_peers_input: daemon.max_peers_per_torrent.to_string(),
            dht_enabled: daemon.enable_dht,
            persist_session: daemon.persist,
            state_file_path,
            worker_threads: daemon.worker_threads,
            worker_threads_input: daemon.worker_threads.to_string(),
            verifier_threads: daemon.verifier_threads,
            verifier_threads_input: daemon.verifier_threads.to_string(),
            ..Self::default()
        }
    }

    /// Refreshes facts that are derived from the running build and the live
    /// configs, keeping the saved preferences untouched.
    ///
    /// The dialog persists every choice in `settings.json`, so opening it must
    /// not reset fields from defaults; only versions, paths and plugin status
    /// are recomputed here.
    pub fn refresh_runtime(&mut self, desktop: &DesktopSettings, daemon: &DaemonConfig) {
        let fresh = Self::from_configs(desktop, daemon);
        self.app_version = fresh.app_version;
        self.engine_version = fresh.engine_version;
        self.state_file_path = fresh.state_file_path;
        self.plugin_status = fresh.plugin_status;
        self.licenses_info = fresh.licenses_info;
        self.repo_url = fresh.repo_url;
        self.port_input = self.listen_port.to_string();
        self.max_peers_input = self.max_peers.to_string();
        self.max_downloads_input = self.max_active_downloads.to_string();
        self.max_seeds_input = self.max_active_seeds.to_string();
        self.worker_threads_input = self.worker_threads.to_string();
        self.verifier_threads_input = self.verifier_threads.to_string();
        self.is_dirty = false;
        self.error = None;
        self.notice = None;
    }

    /// Applies edited values into desktop and daemon config instances.
    pub fn apply_to_configs(&self, desktop: &mut DesktopSettings, daemon: &mut DaemonConfig) {
        desktop.register_associations = self.register_associations;
        desktop.playback = PlaybackSettings {
            backend: self.backend,
            mpv_path: if self.backend == PlaybackBackend::Custom
                && !self.custom_mpv_path.trim().is_empty()
            {
                Some(PathBuf::from(self.custom_mpv_path.trim()))
            } else {
                None
            },
        };

        daemon.download_dir = PathBuf::from(self.default_download_path.trim());
        daemon.preallocate = self.preallocate.into();
        daemon.max_active_downloads = self.max_active_downloads;
        daemon.max_active_seeds = self.max_active_seeds;
        daemon.listen_port = self.listen_port;
        daemon.max_peers_per_torrent = self.max_peers;
        daemon.enable_dht = self.dht_enabled;
        daemon.persist = self.persist_session;
        daemon.worker_threads = self.worker_threads;
        daemon.verifier_threads = self.verifier_threads;
    }

    /// Checks whether any modified setting requires an in-process engine restart.
    #[must_use]
    pub fn requires_restart(&self, daemon: &DaemonConfig) -> bool {
        daemon.download_dir.as_os_str() != self.default_download_path.trim()
            || daemon.max_active_downloads != self.max_active_downloads
            || daemon.max_active_seeds != self.max_active_seeds
            || daemon.preallocate != Preallocate::from(self.preallocate)
            || daemon.listen_port != self.listen_port
            || daemon.max_peers_per_torrent != self.max_peers
            || daemon.enable_dht != self.dht_enabled
            || daemon.persist != self.persist_session
            || daemon.worker_threads != self.worker_threads
            || daemon.verifier_threads != self.verifier_threads
    }

    /// Updates state from a user interaction message.
    pub fn update(&mut self, message: SettingsMessage) -> Task<SettingsMessage> {
        match message {
            SettingsMessage::TabChanged(tab) => {
                self.active_tab = tab;
                Task::none()
            }
            SettingsMessage::LanguageChanged(lang) => {
                self.language = lang;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::ThemeChanged(theme) => {
                self.theme = theme;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::DensityChanged(density) => {
                self.density = density;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::ReduceMotionToggled(val) => {
                self.reduce_motion = val;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::HideZeroSpeedsToggled(val) => {
                self.hide_zero_speeds = val;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::DownloadDirChanged(dir) => {
                self.default_download_path = dir;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::BrowseDownloadDir => {
                let start = PathBuf::from(self.default_download_path.trim());
                let start = if start.is_dir() { Some(start) } else { None };
                Task::perform(pick_folder(start), SettingsMessage::DownloadDirPicked)
            }
            SettingsMessage::DownloadDirPicked(picked) => {
                if let Some(path) = picked {
                    self.default_download_path = path.display().to_string();
                    self.is_dirty = true;
                }
                Task::none()
            }
            SettingsMessage::PreallocateChanged(mode) => {
                self.preallocate = mode;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::MaxDownloadsChanged(input) => {
                self.max_downloads_input = input.clone();
                if let Ok(val) = input.trim().parse::<usize>()
                    && val > 0
                {
                    self.max_active_downloads = val;
                    self.is_dirty = true;
                }
                Task::none()
            }
            SettingsMessage::MaxSeedsChanged(input) => {
                self.max_seeds_input = input.clone();
                if let Ok(val) = input.trim().parse::<usize>() {
                    self.max_active_seeds = val;
                    self.is_dirty = true;
                }
                Task::none()
            }
            SettingsMessage::PortChanged(input) => {
                self.port_input = input.clone();
                if let Ok(port) = input.trim().parse::<u16>() {
                    self.listen_port = port;
                    self.is_dirty = true;
                }
                Task::none()
            }
            SettingsMessage::MaxPeersChanged(input) => {
                self.max_peers_input = input.clone();
                if let Ok(peers) = input.trim().parse::<usize>()
                    && peers > 0
                {
                    self.max_peers = peers;
                    self.is_dirty = true;
                }
                Task::none()
            }
            SettingsMessage::DhtToggled(val) => {
                self.dht_enabled = val;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::BackendChanged(backend) => {
                self.backend = backend;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::MpvPathChanged(path) => {
                self.custom_mpv_path = path;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::InstallPlugin => {
                self.plugin_busy = true;
                self.error = None;
                Task::perform(
                    async {
                        tokio::task::spawn_blocking(playback::install)
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|res| res.map_err(|e| e.to_string()))
                    },
                    SettingsMessage::PluginInstalled,
                )
            }
            SettingsMessage::RemovePlugin => {
                self.plugin_busy = true;
                self.error = None;
                Task::perform(
                    async {
                        tokio::task::spawn_blocking(playback::remove)
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|res| res.map_err(|e| e.to_string()))
                    },
                    SettingsMessage::PluginRemoved,
                )
            }
            SettingsMessage::PluginInstalled(result) => {
                self.plugin_busy = false;
                match result {
                    Ok(path) => {
                        self.backend = PlaybackBackend::Bundled;
                        self.plugin_status = Some(format!("Installed at {}", path.display()));
                        self.notice = Some("Playback engine installed successfully.".to_owned());
                    }
                    Err(err) => {
                        self.error = Some(format!("Cannot install playback engine: {err}"));
                    }
                }
                Task::none()
            }
            SettingsMessage::PluginRemoved(result) => {
                self.plugin_busy = false;
                match result {
                    Ok(()) => {
                        self.plugin_status = None;
                        self.notice = Some("Playback engine removed.".to_owned());
                    }
                    Err(err) => {
                        self.error = Some(format!("Cannot remove playback engine: {err}"));
                    }
                }
                Task::none()
            }
            SettingsMessage::PersistSessionToggled(val) => {
                self.persist_session = val;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::WorkerThreadsChanged(input) => {
                self.worker_threads_input = input.clone();
                if let Ok(threads) = input.trim().parse::<usize>()
                    && threads > 0
                {
                    self.worker_threads = threads;
                    self.is_dirty = true;
                }
                Task::none()
            }
            SettingsMessage::VerifierThreadsChanged(input) => {
                self.verifier_threads_input = input.clone();
                if let Ok(threads) = input.trim().parse::<usize>()
                    && threads > 0
                {
                    self.verifier_threads = threads;
                    self.is_dirty = true;
                }
                Task::none()
            }
            SettingsMessage::AssociationsToggled(val) => {
                self.register_associations = val;
                self.is_dirty = true;
                Task::none()
            }
            SettingsMessage::ResetDefaults => {
                let def = Self::default();
                self.theme = def.theme;
                self.density = def.density;
                self.language = def.language;
                self.reduce_motion = def.reduce_motion;
                self.hide_zero_speeds = def.hide_zero_speeds;
                self.default_download_path = def.default_download_path;
                self.preallocate = def.preallocate;
                self.max_active_downloads = def.max_active_downloads;
                self.max_downloads_input = def.max_downloads_input;
                self.max_active_seeds = def.max_active_seeds;
                self.max_seeds_input = def.max_seeds_input;
                self.listen_port = def.listen_port;
                self.port_input = def.port_input;
                self.max_peers = def.max_peers;
                self.max_peers_input = def.max_peers_input;
                self.dht_enabled = def.dht_enabled;
                self.backend = def.backend;
                self.custom_mpv_path = def.custom_mpv_path;
                self.persist_session = def.persist_session;
                self.worker_threads = def.worker_threads;
                self.worker_threads_input = def.worker_threads_input;
                self.verifier_threads = def.verifier_threads;
                self.verifier_threads_input = def.verifier_threads_input;
                self.register_associations = def.register_associations;

                self.is_dirty = true;
                self.notice = Some("Defaults restored. Click Save to apply.".to_owned());
                Task::none()
            }
            SettingsMessage::OpenConfigFolder => {
                open_config_folder();
                Task::none()
            }
            SettingsMessage::Save | SettingsMessage::Cancel => Task::none(),
        }
    }

    /// Renders the complete modal dialog overlay with backdrop scrim.
    pub fn view(&self) -> Element<'_, SettingsMessage> {
        self.view_modal()
    }

    /// Centered modal card with scrim backdrop.
    pub fn view_modal(&self) -> Element<'_, SettingsMessage> {
        let card = self.view_card();
        let scrim = mouse_area(container(Space::new().width(Fill).height(Fill)).style(|_| {
            container::Style {
                background: Some(Background::Color(Color {
                    a: 0.55,
                    ..Color::BLACK
                })),
                ..container::Style::default()
            }
        }))
        .on_press(SettingsMessage::Cancel);

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

    /// Renders the 900×640 settings card itself (without outer scrim).
    pub fn view_card(&self) -> Element<'_, SettingsMessage> {
        let lang = self.language.resolve();
        let header = row![
            text(tr(lang, TextKey::Settings))
                .size(18.0)
                .font(font_semibold()),
            Space::new().width(Fill),
            button(icon(icons::X, 14.0, T.text_muted))
                .on_press(SettingsMessage::Cancel)
                .style(style::icon)
                .padding(0)
                .width(28)
                .height(28),
        ]
        .padding(Padding::new(0.0).left(24.0).right(24.0))
        .height(56)
        .align_y(Alignment::Center);

        let mut nav_col = column![].spacing(2).width(Fill);
        for tab in SettingsTab::ALL {
            nav_col = nav_col.push(self.nav_item(tab));
        }

        let nav_pane = container(nav_col)
            .width(220)
            .height(Fill)
            .padding(Padding::new(12.0).top(16.0))
            .style(style::surface);

        let vertical_divider = container(Space::new().width(1).height(Fill))
            .width(1)
            .height(Fill)
            .style(style::rule);

        let content_pane = container(
            scrollable(self.content_for_tab(self.active_tab))
                .width(Fill)
                .height(Fill),
        )
        .width(Fill)
        .height(Fill)
        .padding(
            Padding::new(0.0)
                .left(32.0)
                .right(32.0)
                .top(12.0)
                .bottom(12.0),
        );

        let body = row![nav_pane, vertical_divider, content_pane]
            .width(Fill)
            .height(Fill);

        let footer = self.view_footer();

        let card_layout = column![header, rule(), body, rule(), footer]
            .width(Fill)
            .height(Fill);

        container(card_layout)
            .width(900)
            .height(640)
            .style(|_| container::Style {
                background: Some(Background::Color(T.surface)),
                border: Border {
                    color: T.border_default,
                    width: 1.0,
                    radius: 12.0.into(),
                },
                shadow: iced::Shadow {
                    color: Color {
                        a: 0.60,
                        ..Color::BLACK
                    },
                    offset: iced::Vector::new(0.0, 24.0),
                    blur_radius: 64.0,
                },
                ..container::Style::default()
            })
            .into()
    }

    fn nav_item(&self, tab: SettingsTab) -> Element<'_, SettingsMessage> {
        let selected = self.active_tab == tab;
        let icon_color = if selected { T.accent } else { T.text_secondary };
        let text_color = if selected {
            T.text_primary
        } else {
            T.text_secondary
        };
        let weight = if selected {
            font_semibold()
        } else {
            font_medium()
        };

        button(
            row![
                container(Space::new().width(2).height(Fill)).style(style::selection_bar(selected)),
                Space::new().width(10),
                icon(tab.icon(), 18.0, icon_color),
                Space::new().width(10),
                text(tab.label_tr(self.language.resolve()))
                    .size(BODY)
                    .font(weight)
                    .color(text_color),
            ]
            .align_y(Alignment::Center),
        )
        .on_press(SettingsMessage::TabChanged(tab))
        .style(style::nav(selected))
        .padding(Padding::new(0.0).right(12.0))
        .height(38)
        .width(Fill)
        .into()
    }

    fn content_for_tab(&self, tab: SettingsTab) -> Element<'_, SettingsMessage> {
        match tab {
            SettingsTab::General => self.tab_general(),
            SettingsTab::Downloads => self.tab_downloads(),
            SettingsTab::Connection => self.tab_connection(),
            SettingsTab::Appearance => self.tab_appearance(),
            SettingsTab::Playback => self.tab_playback(),
            SettingsTab::Advanced => self.tab_advanced(),
            SettingsTab::About => self.tab_about(),
        }
    }

    fn tab_general(&self) -> Element<'_, SettingsMessage> {
        column![
            section_header("SYSTEM INTEGRATION"),
            setting_row(
                "File associations",
                Some("Register .torrent and magnet: associations on startup"),
                false,
                toggle_switch(
                    self.register_associations,
                    SettingsMessage::AssociationsToggled
                ),
            ),
            setting_row(
                "Config folder",
                Some("Directory storing application settings and session state"),
                false,
                button(
                    row![
                        icon(icons::FOLDER, 14.0, T.text_primary),
                        text("Open folder").size(BODY)
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .on_press(SettingsMessage::OpenConfigFolder)
                .style(style::secondary)
                .padding([0, 14])
                .height(32)
                .into(),
            ),
            group_divider(),
            section_header("LANGUAGE"),
            setting_row(
                "Language",
                Some("Interface display language"),
                false,
                pick_list(
                    &Language::ALL[..],
                    Some(self.language),
                    SettingsMessage::LanguageChanged,
                )
                .style(style::pick)
                .text_size(BODY)
                .width(200)
                .into(),
            ),
        ]
        .spacing(4)
        .into()
    }

    fn tab_downloads(&self) -> Element<'_, SettingsMessage> {
        let path_input = text_input("Default download folder", &self.default_download_path)
            .on_input(SettingsMessage::DownloadDirChanged)
            .style(style::input)
            .width(260)
            .padding([6, 10])
            .size(BODY);

        let browse_btn = button(
            row![
                icon(icons::FOLDER, 14.0, T.text_primary),
                text("Browse").size(BODY)
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .on_press(SettingsMessage::BrowseDownloadDir)
        .style(style::secondary)
        .padding([0, 12])
        .height(32);

        let path_control = row![path_input, browse_btn]
            .spacing(8)
            .align_y(Alignment::Center);

        column![
            section_header("STORAGE"),
            setting_row(
                "Default download path",
                Some("Base directory where torrent payloads will be saved"),
                true,
                path_control.into(),
            ),
            setting_row(
                "Preallocate disk space",
                Some("Sparse files (fast) or Full zeroes (reserved storage)"),
                true,
                pick_list(
                    &PreallocateMode::ALL[..],
                    Some(self.preallocate),
                    SettingsMessage::PreallocateChanged,
                )
                .style(style::pick)
                .text_size(BODY)
                .width(180)
                .into(),
            ),
            group_divider(),
            section_header("TRANSFER QUEUE"),
            setting_row(
                "Max active downloads",
                Some("Maximum concurrent downloading torrent slots"),
                true,
                text_input("3", &self.max_downloads_input)
                    .on_input(SettingsMessage::MaxDownloadsChanged)
                    .style(style::input)
                    .width(80)
                    .padding([6, 10])
                    .size(BODY)
                    .into(),
            ),
            setting_row(
                "Max active seeds",
                Some("Maximum concurrent seeding torrent slots"),
                true,
                text_input("5", &self.max_seeds_input)
                    .on_input(SettingsMessage::MaxSeedsChanged)
                    .style(style::input)
                    .width(80)
                    .padding([6, 10])
                    .size(BODY)
                    .into(),
            ),
        ]
        .spacing(4)
        .into()
    }

    fn tab_connection(&self) -> Element<'_, SettingsMessage> {
        column![
            section_header("INBOUND LISTENER"),
            setting_row(
                "Listen port",
                Some("TCP port for incoming peer connections (0 = random ephemeral)"),
                true,
                text_input("0", &self.port_input)
                    .on_input(SettingsMessage::PortChanged)
                    .style(style::input)
                    .width(100)
                    .padding([6, 10])
                    .size(BODY)
                    .into(),
            ),
            setting_row(
                "Max peers per torrent",
                Some("Maximum connected peer limit per torrent swarm"),
                true,
                text_input("50", &self.max_peers_input)
                    .on_input(SettingsMessage::MaxPeersChanged)
                    .style(style::input)
                    .width(100)
                    .padding([6, 10])
                    .size(BODY)
                    .into(),
            ),
            group_divider(),
            section_header("PEER DISCOVERY"),
            setting_row(
                "Mainline DHT",
                Some("Distributed Hash Table peer discovery for public swarms (BEP 5)"),
                true,
                toggle_switch(self.dht_enabled, SettingsMessage::DhtToggled),
            ),
        ]
        .spacing(4)
        .into()
    }

    fn tab_appearance(&self) -> Element<'_, SettingsMessage> {
        column![
            section_header("APPEARANCE"),
            setting_row(
                "Theme",
                Some("Color palette for surfaces, cards, and text"),
                false,
                segmented_theme(self.theme),
            ),
            setting_row(
                "Density",
                Some("Comfortable (32 px rows) or Compact (28 px rows for dense tables)"),
                false,
                pick_list(
                    &Density::ALL[..],
                    Some(self.density),
                    SettingsMessage::DensityChanged,
                )
                .style(style::pick)
                .text_size(BODY)
                .width(200)
                .into(),
            ),
            setting_row(
                "Language",
                Some("Interface display language"),
                false,
                pick_list(
                    &Language::ALL[..],
                    Some(self.language),
                    SettingsMessage::LanguageChanged,
                )
                .style(style::pick)
                .text_size(BODY)
                .width(200)
                .into(),
            ),
            group_divider(),
            section_header("BEHAVIOR & ACCESSIBILITY"),
            setting_row(
                "Reduce motion",
                Some("Disable transitions and decorative animations"),
                false,
                toggle_switch(self.reduce_motion, SettingsMessage::ReduceMotionToggled),
            ),
            setting_row(
                "Hide zero speeds",
                Some("Display '—' instead of '0 B/s' for idle transfers in table"),
                false,
                toggle_switch(
                    self.hide_zero_speeds,
                    SettingsMessage::HideZeroSpeedsToggled
                ),
            ),
        ]
        .spacing(4)
        .into()
    }

    fn tab_playback(&self) -> Element<'_, SettingsMessage> {
        let engine_pick = pick_list(
            &PlaybackBackend::ALL[..],
            Some(self.backend),
            SettingsMessage::BackendChanged,
        )
        .style(style::pick)
        .text_size(BODY)
        .width(220);

        let installed = playback::installed_version();
        let bundled = playback::bundled_mpv();
        let status = match (&installed, &bundled) {
            (Some(version), Some(path)) => {
                format!("Bundled mpv {version} at {}", path.display())
            }
            (Some(version), None) => format!("Bundled mpv {version} installed"),
            (None, Some(path)) => format!("Local mpv found at {}", path.display()),
            (None, None) => format!(
                "No bundled mpv. Optional plugin downloads GPL mpv ({})",
                playback::download_size()
            ),
        };

        let present = installed.is_some() || bundled.is_some();
        let plugin_btn = if present {
            button("Remove playback engine")
                .on_press_maybe((!self.plugin_busy).then_some(SettingsMessage::RemovePlugin))
                .style(style::secondary)
                .padding([0, 14])
                .height(32)
        } else {
            button(text(format!(
                "Install playback engine ({})",
                playback::download_size()
            )))
            .on_press_maybe((!self.plugin_busy).then_some(SettingsMessage::InstallPlugin))
            .style(style::secondary)
            .padding([0, 14])
            .height(32)
        };

        let mut col = column![
            section_header("PLAYBACK ENGINE"),
            setting_row(
                "Engine backend",
                Some("Video/audio player backend for preview and streaming"),
                false,
                engine_pick.into(),
            ),
        ];

        if self.backend == PlaybackBackend::Custom {
            col = col.push(setting_row(
                "Custom mpv path",
                Some("Executable path to custom mpv.exe binary"),
                false,
                text_input("C:\\path\\to\\mpv.exe", &self.custom_mpv_path)
                    .on_input(SettingsMessage::MpvPathChanged)
                    .style(style::input)
                    .width(260)
                    .padding([6, 10])
                    .size(BODY)
                    .into(),
            ));
        }

        col = col.push(group_divider());
        col = col.push(section_header("OPTIONAL PLUGIN"));
        col = col.push(setting_row(
            "Playback plugin",
            Some(Box::leak(status.into_boxed_str())),
            false,
            plugin_btn.into(),
        ));

        col.spacing(4).into()
    }

    fn tab_advanced(&self) -> Element<'_, SettingsMessage> {
        let state_label = container(
            text(&self.state_file_path)
                .size(CAPTION)
                .color(T.text_muted),
        )
        .padding([6, 10])
        .style(|_| container::Style {
            background: Some(Background::Color(T.overlay)),
            border: Border {
                color: T.border_default,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        });

        column![
            section_header("PERSISTENCE"),
            setting_row(
                "Persist session",
                Some("Save torrent resume data and transfer state across restarts"),
                true,
                toggle_switch(self.persist_session, SettingsMessage::PersistSessionToggled),
            ),
            setting_row(
                "State file path",
                Some("Location of session persistence state file"),
                false,
                state_label.into(),
            ),
            group_divider(),
            section_header("CONCURRENCY & THREADS"),
            setting_row(
                "Worker threads",
                Some("Tokio async reactor threads for network and disk I/O"),
                true,
                text_input("4", &self.worker_threads_input)
                    .on_input(SettingsMessage::WorkerThreadsChanged)
                    .style(style::input)
                    .width(80)
                    .padding([6, 10])
                    .size(BODY)
                    .into(),
            ),
            setting_row(
                "Verifier threads",
                Some("Dedicated thread pool for piece SHA-1 hash checking"),
                true,
                text_input("2", &self.verifier_threads_input)
                    .on_input(SettingsMessage::VerifierThreadsChanged)
                    .style(style::input)
                    .width(80)
                    .padding([6, 10])
                    .size(BODY)
                    .into(),
            ),
            group_divider(),
            section_header("DATA DIRECTORIES"),
            setting_row(
                "Config folder",
                Some("Open local AppData configuration and plugin directory"),
                false,
                button(
                    row![
                        icon(icons::FOLDER, 14.0, T.text_primary),
                        text("Open folder").size(BODY)
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .on_press(SettingsMessage::OpenConfigFolder)
                .style(style::secondary)
                .padding([0, 14])
                .height(32)
                .into(),
            ),
        ]
        .spacing(4)
        .into()
    }

    fn tab_about(&self) -> Element<'_, SettingsMessage> {
        let app_sub = format!(
            "Version {} · Streaming-first BitTorrent client in Rust",
            self.app_version
        );
        let engine_sub = format!("fluxd / flux-core version {}", self.engine_version);

        column![
            section_header("ABOUT OXIDE FLUX"),
            setting_row(
                "Oxide Flux",
                Some(Box::leak(app_sub.into_boxed_str())),
                false,
                text(&self.app_version)
                    .size(BODY)
                    .font(font_semibold())
                    .color(T.accent_text)
                    .into(),
            ),
            setting_row(
                "Engine core",
                Some(Box::leak(engine_sub.into_boxed_str())),
                false,
                text(&self.engine_version)
                    .size(CAPTION)
                    .color(T.text_muted)
                    .into(),
            ),
            setting_row(
                "Repository",
                Some("Source code, issue tracking, and contributions on GitHub"),
                false,
                text(&self.repo_url)
                    .size(CAPTION)
                    .color(T.accent_text)
                    .into(),
            ),
            group_divider(),
            section_header("THIRD-PARTY LICENSES"),
            setting_row(
                "Lucide Icons",
                Some("ISC License · Copyright (c) Lucide Contributors"),
                false,
                text("ISC").size(CAPTION).color(T.text_muted).into(),
            ),
            setting_row(
                "Inter Typeface",
                Some("SIL Open Font License 1.1 · Rasmus Andersson"),
                false,
                text("OFL 1.1").size(CAPTION).color(T.text_muted).into(),
            ),
            setting_row(
                "iced GUI Library",
                Some("MIT License · Copyright (c) Héctor Ramón Jiménez"),
                false,
                text("MIT").size(CAPTION).color(T.text_muted).into(),
            ),
            group_divider(),
            section_header("CORE INTEGRITY"),
            container(
                text("Never invent bytes, speeds or peer counts. Strict SHA-1/SHA-256 hash-verified transfers.")
                    .size(CAPTION)
                    .color(T.text_muted)
            )
            .padding([8, 0]),
        ]
        .spacing(4)
        .into()
    }

    fn view_footer(&self) -> Element<'_, SettingsMessage> {
        let lang = self.language.resolve();
        let mut left_controls = row![
            button(text(tr(lang, TextKey::RestoreDefaults)).size(BODY))
                .on_press(SettingsMessage::ResetDefaults)
                .style(style::secondary)
                .padding([0, 16])
                .height(36),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        if let Some(error) = &self.error {
            left_controls = left_controls.push(text(error).size(CAPTION).color(T.danger));
        } else if let Some(notice) = &self.notice {
            left_controls = left_controls.push(text(notice).size(CAPTION).color(T.accent_text));
        }

        let right_controls = row![
            button(text(tr(lang, TextKey::Cancel)).size(BODY))
                .on_press(SettingsMessage::Cancel)
                .style(style::secondary)
                .padding([0, 18])
                .height(36),
            button(
                text(tr(lang, TextKey::Save))
                    .size(BODY)
                    .font(font_semibold())
            )
            .on_press(SettingsMessage::Save)
            .style(style::primary)
            .padding([0, 22])
            .height(36),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        row![left_controls, Space::new().width(Fill), right_controls]
            .padding(Padding::new(0.0).left(24.0).right(24.0))
            .height(64)
            .align_y(Alignment::Center)
            .into()
    }
}

/// All interactive messages handled by the settings dialog.
#[derive(Debug, Clone)]
pub enum SettingsMessage {
    TabChanged(SettingsTab),
    LanguageChanged(Language),
    ThemeChanged(ThemeMode),
    DensityChanged(Density),
    ReduceMotionToggled(bool),
    HideZeroSpeedsToggled(bool),
    DownloadDirChanged(String),
    BrowseDownloadDir,
    DownloadDirPicked(Option<PathBuf>),
    PreallocateChanged(PreallocateMode),
    MaxDownloadsChanged(String),
    MaxSeedsChanged(String),
    PortChanged(String),
    MaxPeersChanged(String),
    DhtToggled(bool),
    BackendChanged(PlaybackBackend),
    MpvPathChanged(String),
    InstallPlugin,
    RemovePlugin,
    PersistSessionToggled(bool),
    WorkerThreadsChanged(String),
    VerifierThreadsChanged(String),
    AssociationsToggled(bool),
    ResetDefaults,
    Save,
    Cancel,
    OpenConfigFolder,
    PluginInstalled(Result<PathBuf, String>),
    PluginRemoved(Result<(), String>),
}

// UI helper components

fn font_semibold() -> Font {
    Font {
        family: font::Family::Name("Inter"),
        weight: font::Weight::Semibold,
        stretch: font::Stretch::Normal,
        style: font::Style::Normal,
    }
}

fn font_medium() -> Font {
    Font {
        family: font::Family::Name("Inter"),
        weight: font::Weight::Medium,
        stretch: font::Stretch::Normal,
        style: font::Style::Normal,
    }
}

fn section_header<'a, Message: 'a>(label: &'static str) -> Element<'a, Message> {
    container(
        text(label)
            .size(MICRO)
            .font(font_semibold())
            .color(T.text_muted),
    )
    .padding(Padding::new(0.0).top(14.0).bottom(6.0))
    .into()
}

fn group_divider<'a, Message: 'a>() -> Element<'a, Message> {
    container(Space::new().height(1))
        .width(Fill)
        .height(1)
        .padding(Padding::new(0.0).top(8.0).bottom(8.0))
        .style(style::rule)
        .into()
}

fn rule<'a, Message: 'a>() -> Element<'a, Message> {
    container(Space::new().height(1))
        .width(Fill)
        .height(1)
        .style(style::rule)
        .into()
}

fn restart_badge<'a, Message: 'a>() -> Element<'a, Message> {
    container(
        row![
            text("restart")
                .size(MICRO)
                .font(font_medium())
                .color(T.warning),
            text("●").size(MICRO).color(T.warning),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    )
    .padding(Padding::new(2.0).left(6.0).right(6.0))
    .style(|_| container::Style {
        background: Some(Background::Color(T.warning_soft)),
        border: Border {
            color: T.warning,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

fn setting_row<'a, Message: 'a>(
    title: &'static str,
    description: Option<&'static str>,
    restart_required: bool,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut title_row = row![
        text(title)
            .size(BODY)
            .font(font_medium())
            .color(T.text_primary),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if restart_required {
        title_row = title_row.push(restart_badge());
    }

    let left = if let Some(desc) = description {
        column![title_row, text(desc).size(CAPTION).color(T.text_muted)]
            .spacing(2)
            .width(Fill)
    } else {
        column![title_row].width(Fill)
    };

    container(row![left, control].spacing(16).align_y(Alignment::Center))
        .height(56)
        .align_y(Alignment::Center)
        .into()
}

fn toggle_switch<'a, Message: Clone + 'a>(
    active: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    let knob_offset = if active { 16.0 } else { 0.0 };
    let knob = container(Space::new().width(16).height(16)).style(move |_| container::Style {
        background: Some(Background::Color(if active {
            T.accent_on
        } else {
            T.text_secondary
        })),
        border: Border {
            radius: 999.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    });

    let track = container(
        row![
            Space::new().width(knob_offset),
            knob,
            Space::new().width(16.0 - knob_offset),
        ]
        .align_y(Alignment::Center),
    )
    .width(36)
    .height(20)
    .padding(2)
    .style(move |_| container::Style {
        background: Some(Background::Color(if active {
            T.accent_fill
        } else {
            T.overlay
        })),
        border: Border {
            color: if active {
                Color::TRANSPARENT
            } else {
                T.border_input
            },
            width: 1.0,
            radius: 999.0.into(),
        },
        ..container::Style::default()
    });

    button(track)
        .on_press(on_toggle(!active))
        .style(|_, _| button::Style {
            background: None,
            border: Border::default(),
            snap: true,
            ..button::Style::default()
        })
        .padding(0)
        .into()
}

fn segmented_theme(current: ThemeMode) -> Element<'static, SettingsMessage> {
    let mut items = row![].spacing(2);
    for mode in ThemeMode::ALL {
        let selected = current == mode;
        let btn = button(
            text(mode.label())
                .size(CAPTION)
                .font(if selected {
                    font_semibold()
                } else {
                    font_medium()
                })
                .color(if selected {
                    T.accent_on
                } else {
                    T.text_secondary
                }),
        )
        .on_press(SettingsMessage::ThemeChanged(mode))
        .padding(Padding::new(4.0).left(12.0).right(12.0))
        .height(28)
        .style(move |_, status| {
            if selected {
                button::Style {
                    background: Some(Background::Color(T.accent_fill)),
                    text_color: T.accent_on,
                    border: Border {
                        radius: 999.0.into(),
                        ..Border::default()
                    },
                    snap: true,
                    ..button::Style::default()
                }
            } else {
                let bg = match status {
                    button::Status::Hovered | button::Status::Pressed => {
                        Some(Background::Color(T.raised))
                    }
                    _ => None,
                };
                button::Style {
                    background: bg,
                    text_color: T.text_secondary,
                    border: Border {
                        radius: 999.0.into(),
                        ..Border::default()
                    },
                    snap: true,
                    ..button::Style::default()
                }
            }
        });
        items = items.push(btn);
    }

    container(items)
        .padding(2)
        .height(32)
        .style(|_| container::Style {
            background: Some(Background::Color(T.overlay)),
            border: Border {
                color: T.border_default,
                width: 1.0,
                radius: 999.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

async fn pick_folder(start: Option<PathBuf>) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new();
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog
        .pick_folder()
        .await
        .map(|folder| folder.path().to_path_buf())
}

fn open_config_folder() {
    let dir = crate::launch::config_dir();
    let _ = std::fs::create_dir_all(&dir);

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&dir).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_state_is_valid() {
        let state = SettingsState::default();
        assert_eq!(state.active_tab, SettingsTab::General);
        assert_eq!(state.theme, ThemeMode::System);
        assert_eq!(state.density, Density::Comfortable);
        assert_eq!(state.language, Language::System);
        assert_eq!(state.max_active_downloads, 3);
        assert_eq!(state.max_active_seeds, 5);
        assert_eq!(state.listen_port, 0);
        assert_eq!(state.max_peers, 50);
        assert!(state.dht_enabled);
        assert!(state.persist_session);
        assert_eq!(state.worker_threads, 4);
        assert_eq!(state.verifier_threads, 2);
    }

    #[test]
    fn tab_switching() {
        let mut state = SettingsState::default();
        let _ = state.update(SettingsMessage::TabChanged(SettingsTab::Downloads));
        assert_eq!(state.active_tab, SettingsTab::Downloads);
        let _ = state.update(SettingsMessage::TabChanged(SettingsTab::Appearance));
        assert_eq!(state.active_tab, SettingsTab::Appearance);
    }

    #[test]
    fn appearance_changes() {
        let mut state = SettingsState::default();
        let _ = state.update(SettingsMessage::ThemeChanged(ThemeMode::Dark));
        assert_eq!(state.theme, ThemeMode::Dark);
        assert!(state.is_dirty);

        let _ = state.update(SettingsMessage::DensityChanged(Density::Compact));
        assert_eq!(state.density, Density::Compact);

        let _ = state.update(SettingsMessage::LanguageChanged(Language::Ukrainian));
        assert_eq!(state.language, Language::Ukrainian);

        let _ = state.update(SettingsMessage::ReduceMotionToggled(true));
        assert!(state.reduce_motion);

        let _ = state.update(SettingsMessage::HideZeroSpeedsToggled(true));
        assert!(state.hide_zero_speeds);
    }

    #[test]
    fn connection_and_downloads_changes() {
        let mut state = SettingsState::default();
        let _ = state.update(SettingsMessage::PortChanged("6881".to_owned()));
        assert_eq!(state.listen_port, 6881);
        assert_eq!(state.port_input, "6881");

        let _ = state.update(SettingsMessage::MaxPeersChanged("80".to_owned()));
        assert_eq!(state.max_peers, 80);

        let _ = state.update(SettingsMessage::MaxDownloadsChanged("6".to_owned()));
        assert_eq!(state.max_active_downloads, 6);

        let _ = state.update(SettingsMessage::MaxSeedsChanged("10".to_owned()));
        assert_eq!(state.max_active_seeds, 10);

        let _ = state.update(SettingsMessage::DhtToggled(false));
        assert!(!state.dht_enabled);

        let _ = state.update(SettingsMessage::PreallocateChanged(PreallocateMode::Full));
        assert_eq!(state.preallocate, PreallocateMode::Full);
    }

    #[test]
    fn config_sync_and_restart_detection() {
        let desktop = DesktopSettings::default();
        let daemon = DaemonConfig::default();
        let mut state = SettingsState::from_configs(&desktop, &daemon);
        assert!(!state.requires_restart(&daemon));

        let _ = state.update(SettingsMessage::PortChanged("9999".to_owned()));
        assert!(state.requires_restart(&daemon));

        let mut updated_desktop = desktop.clone();
        let mut updated_daemon = daemon.clone();
        state.apply_to_configs(&mut updated_desktop, &mut updated_daemon);
        assert_eq!(updated_daemon.listen_port, 9999);
        assert!(!state.requires_restart(&updated_daemon));
    }
}
