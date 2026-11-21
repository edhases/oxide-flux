//! The native desktop client: FDM-style shell over the in-process engine.
//!
//! Visual constants live in [`crate::theme`], widget styles in [`crate::style`];
//! this file only assembles them. Two rules are enforced here: a number the
//! engine cannot measure is rendered as `—`, and the layout is one flush surface
//! stack (56 / 44 / 232 / 232 / 28) instead of floating rounded cards.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use flux_api::{FileView, Priority, Stats, TorrentDetail, TorrentState, TorrentSummary};
use flux_daemon::Config;
use iced::widget::text::Wrapping;
use iced::widget::{
    Space, button, column, container, pick_list, progress_bar, row, scrollable, text, text_input,
};
#[cfg(windows)]
use iced::window::raw_window_handle::RawWindowHandle;
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Font, Length, Padding, Subscription, Task,
};
use iced::{font, keyboard, window};

use crate::icons::{self, icon};
use crate::launch::Initial;
use crate::player::{self, Bounds, Player};
use crate::single_instance::{OpenRequest, RequestQueue};
use crate::style;
use crate::supervisor::{self, Command, Event};
use crate::theme::{self, T, metrics, type_scale};

const BODY: f32 = type_scale::BODY;
const CAPTION: f32 = type_scale::CAPTION;
const MICRO: f32 = type_scale::MICRO;

/// Boots the desktop application with links handed over by the OS or another
/// instance.
///
/// # Errors
///
/// Returns an [`iced::Error`] if the window or renderer cannot start.
pub fn run(initial: Initial) -> iced::Result {
    iced::application(move || App::new(initial.clone()), App::update, App::view)
        .theme(|_: &App| theme::iced_theme())
        .subscription(App::subscription)
        .title("Oxide Flux")
        .window(window::Settings {
            size: iced::Size::new(1280.0, 800.0),
            min_size: Some(iced::Size::new(960.0, 600.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        })
        .font(include_bytes!("../assets/fonts/Inter-Variable.ttf").as_slice())
        .default_font(ui_font(400))
        .run()
}

fn ui_font(weight: u16) -> Font {
    Font {
        family: font::Family::Name("Inter"),
        weight: match weight {
            600..=u16::MAX => font::Weight::Semibold,
            500..=599 => font::Weight::Medium,
            _ => font::Weight::Normal,
        },
        ..Font::DEFAULT
    }
}

fn semibold() -> Font {
    ui_font(600)
}

fn medium() -> Font {
    ui_font(500)
}

fn search_id() -> iced::widget::Id {
    iced::widget::Id::new("library-search")
}

/// Which slice of the list the sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Downloading,
    Seeding,
    Completed,
    Paused,
    Checking,
    Error,
}

impl Filter {
    const ALL: [Self; 7] = [
        Self::All,
        Self::Downloading,
        Self::Seeding,
        Self::Completed,
        Self::Paused,
        Self::Checking,
        Self::Error,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Downloading => "Downloading",
            Self::Seeding => "Seeding",
            Self::Completed => "Completed",
            Self::Paused => "Paused",
            Self::Checking => "Checking",
            Self::Error => "Error",
        }
    }

    fn matches(self, torrent: &TorrentSummary) -> bool {
        match self {
            Self::All => true,
            Self::Downloading => matches!(
                torrent.state,
                TorrentState::Queued | TorrentState::Downloading
            ),
            Self::Seeding => torrent.state == TorrentState::Seeding,
            // §5.1: Completed includes both a finished seed and a completed
            // torrent that is not currently serving.
            Self::Completed => matches!(
                torrent.state,
                TorrentState::Seeding | TorrentState::Complete
            ),
            Self::Paused => torrent.state == TorrentState::Paused,
            Self::Checking => torrent.state == TorrentState::Checking,
            Self::Error => torrent.state == TorrentState::Error,
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            Self::All => icons::FILTER,
            Self::Downloading => icons::ARROW_DOWN,
            Self::Seeding => icons::ARROW_UP,
            Self::Completed => icons::CHECK_CIRCLE,
            Self::Paused => icons::PAUSE,
            Self::Checking => icons::ROTATE,
            Self::Error => icons::ALERT,
        }
    }
}

/// Sortable table columns, in the spec's display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    Size,
    Progress,
    State,
    Peers,
    Down,
    Up,
    Eta,
}

/// Details pane tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    General,
    Files,
}

/// File priority choice shown in the details pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityChoice {
    Skip,
    Normal,
    High,
}

impl PriorityChoice {
    const ALL: [Self; 3] = [Self::Skip, Self::Normal, Self::High];
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

impl From<PriorityChoice> for Priority {
    fn from(choice: PriorityChoice) -> Self {
        match choice {
            PriorityChoice::Skip => Self::Skip,
            PriorityChoice::Normal => Self::Normal,
            PriorityChoice::High => Self::High,
        }
    }
}

impl From<Priority> for PriorityChoice {
    fn from(priority: Priority) -> Self {
        match priority {
            Priority::Skip => Self::Skip,
            Priority::High => Self::High,
            // The API's `Low` has no engine equivalent (`Unsupported`); this
            // build never sets it, so reading it back as Normal is the only
            // non-invented option. It is not offered in the dropdown.
            Priority::Low | Priority::Normal => Self::Normal,
        }
    }
}

/// UI messages.
#[derive(Debug, Clone)]
pub enum Message {
    Pump,
    Filter(Filter),
    Select(String),
    SearchChanged(String),
    ClearSearch,
    FocusSearch,
    Escape,
    Sort(Column),
    Tab(DetailTab),
    AddFile,
    FilePicked(Option<Vec<u8>>),
    ToggleMagnet,
    MagnetChanged(String),
    AddMagnet,
    Pause,
    Resume,
    Remove,
    RemoveFiles,
    ToggleSelectedPause,
    DeleteSelected,
    Priority(usize, PriorityChoice),
    /// Ask the supervisor for a loopback stream of one file.
    PlayStream {
        id: String,
        file: usize,
        title: String,
    },
    /// The stream URL is ready; the window handle is still to be fetched.
    StreamReady {
        url: String,
        title: String,
    },
    WindowHandle(Option<isize>),
    WindowId(Option<window::Id>),
    WindowScale(f32),
    Resized(iced::Size),
    StopPlayback,
    Dropped(PathBuf),
    DragHover(bool),
    CloseRequested,
    ClearFilters,
    ClearError,
}

/// Application state.
pub struct App {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    events: tokio::sync::mpsc::UnboundedReceiver<Event>,
    _runtime: tokio::runtime::Runtime,
    download_dir: PathBuf,
    version: String,
    torrents: Vec<TorrentSummary>,
    stats: Stats,
    filter: Filter,
    search: String,
    sort: Option<(Column, bool)>,
    tab: DetailTab,
    selected: Option<String>,
    detail: Option<TorrentDetail>,
    magnet_open: bool,
    magnet_input: String,
    error: Option<String>,
    dismissed_error: Option<String>,
    player: Player,
    dht: bool,
    initial_requests: Vec<OpenRequest>,
    queue: Option<RequestQueue>,
    last_poll: Instant,
    drag_hover: bool,
    closing: bool,
    engine_closed: bool,
    hwnd: Option<isize>,
    window_id: Option<window::Id>,
    scale: Option<f32>,
    window_size: iced::Size,
    pending_stream: Option<(String, String)>,
    playing: Option<String>,
}

impl App {
    /// Builds the engine, the supervisor channels and the initial state.
    #[must_use]
    pub fn new(initial: Initial) -> Self {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("oxide-flux: cannot start the engine runtime: {error}");
                std::process::exit(1);
            }
        };

        let config = default_config();
        let download_dir = config.download_dir.clone();
        let dht = config.enable_dht;
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, events) = tokio::sync::mpsc::unbounded_channel();
        runtime.spawn(supervisor::run(config, command_rx, event_tx));

        let error = if initial.notices.is_empty() {
            None
        } else {
            Some(initial.notices.join(" · "))
        };

        Self {
            commands,
            events,
            _runtime: runtime,
            download_dir,
            version: String::new(),
            torrents: Vec::new(),
            stats: Stats::default(),
            filter: Filter::All,
            search: String::new(),
            sort: None,
            tab: DetailTab::General,
            selected: None,
            detail: None,
            magnet_open: false,
            magnet_input: String::new(),
            error,
            dismissed_error: None,
            player: Player::new(),
            dht,
            initial_requests: initial.requests,
            queue: initial.queue,
            last_poll: Instant::now() - Duration::from_secs(1),
            drag_hover: false,
            closing: false,
            engine_closed: false,
            hwnd: None,
            window_id: None,
            scale: None,
            window_size: iced::Size::new(1280.0, 800.0),
            pending_stream: None,
            playing: None,
        }
    }

    /// Drains engine events ten times a second and polls for links handed over
    /// by other launches.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(100)).map(|_| Message::Pump),
            iced::event::listen_with(drag_event),
            iced::event::listen_with(close_event),
            // `listen_with` receives captured events too, which is what lets
            // `Esc` clear the search while the text field owns it. Other
            // shortcuts still require an ignored event, so Space/Delete do
            // nothing while a text field is editing.
            iced::event::listen_with(key_event),
            window::resize_events().map(|(_id, size)| Message::Resized(size)),
        ])
    }

    /// Handles one message.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Pump => {
                self.drain_events();
                if self.pending_stream.is_some() {
                    self.try_start_playback();
                    if self.pending_stream.is_some() {
                        return self.request_window();
                    }
                }
                if self.playing.is_some() && !self.player.is_running() {
                    self.player.stop();
                    self.playing = None;
                }
                if self.closing && self.engine_closed {
                    return iced::exit();
                }
            }
            Message::Filter(filter) => self.filter = filter,
            Message::Select(id) => {
                self.selected = Some(id.clone());
                self.detail = None;
                self.dismissed_error = None;
                let _ = self.commands.send(Command::GetDetail(id));
            }
            Message::SearchChanged(value) => self.search = value,
            Message::ClearSearch => self.search.clear(),
            Message::FocusSearch => {
                return iced::widget::operation::focus(search_id());
            }
            Message::Escape => {
                if self.playing.is_some() {
                    self.stop_playback();
                } else {
                    let had_search = !self.search.is_empty();
                    self.search.clear();
                    if !had_search {
                        self.selected = None;
                        self.detail = None;
                    }
                }
            }
            Message::Sort(column) => {
                self.sort = match self.sort {
                    Some((active, ascending)) if active == column => {
                        if ascending {
                            Some((column, false))
                        } else {
                            None
                        }
                    }
                    _ => Some((column, true)),
                };
            }
            Message::Tab(tab) => self.tab = tab,
            Message::AddFile => return Task::perform(pick_torrent(), Message::FilePicked),
            Message::FilePicked(bytes) => {
                if let Some(bytes) = bytes {
                    let _ = self.commands.send(Command::AddBytes(bytes));
                }
            }
            Message::ToggleMagnet => self.magnet_open = !self.magnet_open,
            Message::MagnetChanged(value) => self.magnet_input = value,
            Message::AddMagnet => {
                let uri = self.magnet_input.trim().to_owned();
                if !uri.is_empty() {
                    let _ = self.commands.send(Command::AddMagnet(uri));
                    self.magnet_input.clear();
                    self.magnet_open = false;
                }
            }
            Message::Pause => {
                if let Some(id) = self.selected.clone() {
                    let _ = self.commands.send(Command::Pause(id));
                }
            }
            Message::Resume => {
                if let Some(id) = self.selected.clone() {
                    let _ = self.commands.send(Command::Resume(id));
                }
            }
            Message::Remove => {
                if let Some(id) = self.selected.clone() {
                    let _ = self.commands.send(Command::Remove {
                        id,
                        delete_files: false,
                    });
                    self.selected = None;
                    self.detail = None;
                }
            }
            Message::RemoveFiles => {
                if let Some(id) = self.selected.clone() {
                    let _ = self.commands.send(Command::Remove {
                        id,
                        delete_files: true,
                    });
                    self.selected = None;
                    self.detail = None;
                }
            }
            Message::ToggleSelectedPause => {
                if let Some(summary) = self.selected_summary() {
                    let id = summary.id.clone();
                    let command = if summary.state == TorrentState::Paused {
                        Command::Resume(id)
                    } else {
                        Command::Pause(id)
                    };
                    let _ = self.commands.send(command);
                }
            }
            Message::DeleteSelected => {
                if let Some(id) = self.selected.clone() {
                    let _ = self.commands.send(Command::Remove {
                        id,
                        delete_files: false,
                    });
                    self.selected = None;
                    self.detail = None;
                }
            }
            Message::Priority(index, choice) => {
                if let Some(id) = self.selected.clone() {
                    let _ = self.commands.send(Command::SetPriority {
                        id,
                        index,
                        priority: choice.into(),
                    });
                }
            }
            Message::PlayStream { id, file, .. } => {
                self.error = None;
                let _ = self.commands.send(Command::Play { id, file });
            }
            Message::StreamReady { url, title } => {
                self.pending_stream = Some((url, title));
            }
            Message::WindowId(id) => {
                self.window_id = id;
            }
            Message::WindowHandle(handle) => {
                self.hwnd = handle;
                self.try_start_playback();
                if handle.is_none()
                    && let Some((url, title)) = self.pending_stream.take()
                {
                    match self.player.play(Path::new(&url), &title) {
                        Ok(()) => self.playing = Some(title),
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            Message::WindowScale(scale) => {
                self.scale = Some(scale);
                self.try_start_playback();
                if self.playing.is_some() {
                    self.player.set_bounds(self.video_bounds());
                }
            }
            Message::Resized(size) => {
                self.window_size = size;
                if self.playing.is_some() {
                    self.player.set_bounds(self.video_bounds());
                }
            }
            Message::StopPlayback => self.stop_playback(),
            Message::Dropped(path) => {
                self.drag_hover = false;
                if path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
                {
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            let _ = self.commands.send(Command::AddBytes(bytes));
                        }
                        Err(error) => {
                            self.error = Some(format!("cannot read {}: {error}", path.display()));
                        }
                    }
                } else {
                    self.error = Some(format!(
                        "{} is not a .torrent file (magnet links are pasted or opened from the OS)",
                        path.display()
                    ));
                }
            }
            Message::DragHover(hovered) => self.drag_hover = hovered,
            Message::CloseRequested => {
                if !self.closing {
                    self.closing = true;
                    self.player.stop();
                    // The supervisor shuts the session down (fsync, tracker
                    // `stopped`) and then reports `Event::Closed`.
                    let _ = self.commands.send(Command::Shutdown);
                }
            }
            Message::ClearFilters => {
                self.search.clear();
                self.filter = Filter::All;
            }
            Message::ClearError => {
                self.error = None;
                self.dismissed_error = self.torrent_error();
            }
        }
        Task::none()
    }

    /// Physical client-area rectangle the embedded player should cover:
    /// everything below the header/toolbar, above the status bar.
    fn video_bounds(&self) -> Bounds {
        let scale = self.scale.unwrap_or(1.0);
        let top = (metrics::HEADER + metrics::TOOLBAR) * scale;
        let bottom = metrics::STATUS * scale;
        let width = (self.window_size.width * scale).round().max(1.0) as u32;
        let height = ((self.window_size.height * scale) - top - bottom)
            .max(120.0 * scale)
            .round() as u32;
        Bounds {
            x: 0,
            y: top.round() as i32,
            width,
            height,
        }
    }

    fn try_start_playback(&mut self) {
        let (Some(hwnd), Some(_scale)) = (self.hwnd, self.scale) else {
            return;
        };
        let Some((url, title)) = self.pending_stream.clone() else {
            return;
        };
        self.pending_stream = None;
        let bounds = self.video_bounds();
        match self
            .player
            .play_embedded(hwnd, bounds, Path::new(&url), &title)
        {
            Ok(()) => self.playing = Some(title),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn stop_playback(&mut self) {
        self.player.stop();
        self.playing = None;
        self.pending_stream = None;
    }

    fn request_window(&self) -> Task<Message> {
        let Some(id) = self.window_id else {
            return window::latest().map(Message::WindowId);
        };
        let mut tasks = Vec::new();
        if self.hwnd.is_none() {
            tasks.push(window::run(id, raw_hwnd).map(Message::WindowHandle));
        }
        if self.scale.is_none() {
            tasks.push(window::scale_factor(id).map(Message::WindowScale));
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            Task::batch(tasks)
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Ready {
                    version,
                    download_dir: _,
                } => self.version = version,
                Event::Snapshot { torrents, stats } => {
                    // Forget a dismissal once the engine stops repeating that
                    // exact failure, so a recurring error surfaces again.
                    if let Some(dismissed) = self.dismissed_error.clone()
                        && !torrents.iter().any(|torrent| {
                            torrent.last_error.as_deref() == Some(dismissed.as_str())
                        })
                    {
                        self.dismissed_error = None;
                    }
                    // Drop a selection whose torrent left the session, so the
                    // details pane cannot show a ghost.
                    if let Some(id) = &self.selected
                        && !torrents.iter().any(|torrent| &torrent.id == id)
                    {
                        self.selected = None;
                        self.detail = None;
                    }
                    self.torrents = torrents;
                    self.stats = stats;
                    if let Some(id) = &self.selected {
                        let _ = self.commands.send(Command::GetDetail(id.clone()));
                    }
                }
                Event::Detail(detail) => self.detail = Some(detail),
                Event::StreamReady { url, title } => {
                    self.pending_stream = Some((url, title));
                }
                Event::Error(message) => self.error = Some(message),
                Event::Closed => {
                    self.engine_closed = true;
                    if !self.closing {
                        self.error =
                            Some("Engine stopped. Your torrents are safe on disk.".to_owned());
                    }
                }
            }
        }
        self.poll_open_requests();
    }

    fn poll_open_requests(&mut self) {
        let pending = !self.initial_requests.is_empty();
        if !pending && self.last_poll.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_poll = Instant::now();
        let mut requests = std::mem::take(&mut self.initial_requests);
        if let Some(queue) = &self.queue
            && let Ok(more) = queue.drain()
        {
            requests.extend(more);
        }
        for request in requests {
            self.open_request(request);
        }
    }

    fn open_request(&mut self, request: OpenRequest) {
        match request {
            OpenRequest::Magnet(uri) => {
                let _ = self.commands.send(Command::AddMagnet(uri));
            }
            OpenRequest::Torrent(path) => match std::fs::read(&path) {
                Ok(bytes) => {
                    let _ = self.commands.send(Command::AddBytes(bytes));
                }
                Err(error) => {
                    self.error = Some(format!("cannot read {}: {error}", path.display()));
                }
            },
        }
    }

    /// Renders the whole window as one flush stack: header, rules, toolbar,
    /// body, status bar.
    pub fn view(&self) -> Element<'_, Message> {
        let mut layout = column![self.header(), rule(), self.toolbar(), rule()]
            .width(Fill)
            .height(Fill);
        if self.magnet_open {
            layout = layout.push(self.magnet_row());
        }
        layout = layout.push(self.body());
        layout = layout.push(rule());
        layout = layout.push(self.status_bar());
        container(layout)
            .style(style::canvas)
            .width(Fill)
            .height(Fill)
            .padding(0)
            .into()
    }

    fn header(&self) -> Element<'_, Message> {
        let mut brand = row![icon(icons::DOWNLOAD, 24.0, T.text_primary)]
            .spacing(8)
            .align_y(Alignment::Center);
        brand = brand.push(
            text("Oxide Flux")
                .size(type_scale::HEADING)
                .font(semibold()),
        );
        if !self.version.is_empty() {
            brand = brand.push(
                text(format!("v{}", self.version))
                    .size(MICRO)
                    .color(T.text_muted),
            );
        }

        let clear: Element<'_, Message> = if self.search.is_empty() {
            Space::new().width(0).into()
        } else {
            button(icon(icons::X, 14.0, T.text_muted))
                .on_press(Message::ClearSearch)
                .style(style::icon)
                .padding(0)
                .width(20)
                .height(20)
                .into()
        };
        let search = container(
            row![
                icon(icons::SEARCH, 16.0, T.text_muted),
                text_input("Search torrents…", &self.search)
                    .id(search_id())
                    .on_input(Message::SearchChanged)
                    .style(style::search_field)
                    .padding([6, 0])
                    .size(BODY)
                    .width(Fill),
                clear,
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .height(32)
        .width(320)
        .padding([0, 10])
        .style(style::search_shell);

        let add = button(
            row![
                icon(icons::PLUS, 16.0, T.accent_on),
                text("Add .torrent").size(BODY).font(semibold()),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .on_press(Message::AddFile)
        .style(style::primary)
        .padding([0, 16])
        .height(36);

        let magnet = button(
            row![
                icon(icons::MAGNET, 16.0, T.text_primary),
                text("Add magnet").size(BODY).font(medium()),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .on_press(Message::ToggleMagnet)
        .style(style::secondary)
        .padding([0, 14])
        .height(36);

        container(
            row![brand, Space::new().width(Fill), search, add, magnet,]
                .spacing(8)
                .align_y(Alignment::Center),
        )
        .padding([0, 24])
        .height(metrics::HEADER)
        .width(Fill)
        .style(style::flush)
        .into()
    }

    fn magnet_row(&self) -> Element<'_, Message> {
        container(
            row![
                icon(icons::MAGNET, 16.0, T.text_muted),
                text_input("magnet:?xt=urn:btih:…", &self.magnet_input)
                    .on_input(Message::MagnetChanged)
                    .on_submit(Message::AddMagnet)
                    .style(style::input)
                    .padding([7, 10])
                    .size(BODY)
                    .width(Fill),
                button(text("Fetch metadata").size(BODY).font(medium()))
                    .on_press(Message::AddMagnet)
                    .style(style::primary)
                    .padding([0, 14])
                    .height(32),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([0, 24]),
        )
        .height(48)
        .width(Fill)
        .style(style::flush)
        .into()
    }

    fn toolbar(&self) -> Element<'_, Message> {
        let state = self.selected_summary().map(|summary| summary.state);
        let has_selection = state.is_some();
        let can_pause = state.is_some_and(|state| state != TorrentState::Paused);
        let can_resume = state == Some(TorrentState::Paused);

        container(
            row![
                toolbar_button(icons::PAUSE, "Pause", Message::Pause, can_pause),
                toolbar_button(icons::PLAY, "Resume", Message::Resume, can_resume),
                toolbar_button(icons::TRASH, "Remove", Message::Remove, has_selection),
                toolbar_button(
                    icons::X,
                    "Stop",
                    Message::StopPlayback,
                    self.playing.is_some()
                ),
                Space::new().width(Fill),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([0, 24])
        .height(metrics::TOOLBAR)
        .width(Fill)
        .style(style::flush)
        .into()
    }

    fn body(&self) -> Element<'_, Message> {
        if let Some(title) = &self.playing {
            return container(
                column![
                    icon(icons::PLAY, 32.0, T.accent),
                    text("Playing in the embedded player")
                        .size(18)
                        .font(semibold()),
                    text(elide_middle(title, 70))
                        .size(BODY)
                        .color(T.text_secondary),
                    text("Esc stops playback").size(MICRO).color(T.text_muted),
                ]
                .spacing(10)
                .align_x(Alignment::Center),
            )
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .style(style::surface)
            .into();
        }
        row![self.sidebar(), vertical_rule(), self.content()]
            .height(Fill)
            .width(Fill)
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut items = column![section("LIBRARY")].spacing(0).width(Fill);
        for filter in Filter::ALL {
            let count = self
                .torrents
                .iter()
                .filter(|torrent| filter.matches(torrent))
                .count();
            items = items.push(self.nav_item(filter, count));
        }

        let path = elide_middle(&self.download_dir.display().to_string(), 28);
        let footer = column![
            section("SAVE TO"),
            row![
                icon(icons::HARD_DRIVE, 14.0, T.text_muted),
                text(path).size(MICRO).color(T.text_muted),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([0, 12]),
        ]
        .spacing(0);

        container(
            column![items, Space::new().height(Fill), footer]
                .spacing(0)
                .height(Fill),
        )
        .width(metrics::NAV)
        .padding([12, 0])
        .height(Fill)
        .style(style::surface)
        .into()
    }

    fn nav_item(&self, filter: Filter, count: usize) -> Element<'_, Message> {
        let selected = self.filter == filter;
        let color = if selected { T.accent } else { T.text_secondary };
        let count_color = if selected {
            T.accent_text
        } else if count == 0 {
            T.text_muted
        } else {
            T.text_secondary
        };
        let count_background = if selected { T.accent_soft } else { T.overlay };

        button(
            row![
                selection_bar(selected, 34.0),
                container(
                    row![
                        icon(filter.icon(), 18.0, color),
                        text(filter.label()).size(BODY).font(if selected {
                            semibold()
                        } else {
                            medium()
                        }),
                        Space::new().width(Fill),
                        container(
                            text(count.to_string())
                                .size(MICRO)
                                .font(semibold())
                                .color(count_color)
                        )
                        .padding([1, 7])
                        .style(move |_| container::Style {
                            background: Some(Background::Color(count_background)),
                            border: Border {
                                color: Color::TRANSPARENT,
                                width: 0.0,
                                radius: 999.0.into(),
                            },
                            ..container::Style::default()
                        }),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .width(Fill),
                )
                .width(Fill)
                .height(34)
                .padding(Padding::new(0.0).left(10.0).right(10.0))
                .align_y(Alignment::Center),
            ]
            .width(Fill)
            .height(34),
        )
        .on_press(Message::Filter(filter))
        .style(style::nav(selected))
        .padding(0)
        .width(Fill)
        .height(34)
        .into()
    }

    fn content(&self) -> Element<'_, Message> {
        let visible = self.visible();
        let mut body = column![].spacing(0).width(Fill).height(Fill);

        if let Some(message) = self.banner_message() {
            body = body.push(self.error_banner(&message));
        }
        if self.torrents.is_empty() {
            body = body.push(container(self.empty_state()).width(Fill).height(Fill));
            return container(body)
                .style(style::surface)
                .width(Fill)
                .height(Fill)
                .into();
        }

        body = body.push(self.content_header(visible.len()));
        body = body.push(self.table_header());
        body = body.push(rule_strong());
        if visible.is_empty() {
            body = body.push(
                container(self.no_results())
                    .width(Fill)
                    .height(Fill)
                    .center_x(Fill)
                    .center_y(Fill),
            );
        } else {
            body = body.push(scrollable(self.table(&visible)).height(Fill).width(Fill));
            if self.selected.is_none() {
                body = body.push(self.drop_strip());
            }
        }
        if self.selected.is_some() {
            body = body.push(rule());
            body = body.push(self.details());
        }

        container(body)
            .style(style::surface)
            .width(Fill)
            .height(Fill)
            .into()
    }

    fn content_header(&self, count: usize) -> Element<'_, Message> {
        let total = self.torrents.len();
        let count_text = if self.search.trim().is_empty() && self.filter == Filter::All {
            let noun = if count == 1 { "torrent" } else { "torrents" };
            format!("{count} {noun}")
        } else {
            format!("{count} of {total}")
        };
        let down: u64 = self.torrents.iter().map(|t| t.download_rate).sum();

        container(
            row![
                text(self.filter.label())
                    .size(BODY)
                    .font(semibold())
                    .color(T.text_primary),
                text(count_text).size(CAPTION).color(T.text_muted),
                Space::new().width(Fill),
                text(format!("↓ {}/s", format_bytes(down)))
                    .size(CAPTION)
                    .color(T.text_secondary),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .height(metrics::CONTENT_HEADER)
        .width(Fill)
        .padding([0, 24])
        .style(style::flush)
        .into()
    }

    fn visible(&self) -> Vec<&TorrentSummary> {
        let needle = self.search.trim().to_lowercase();
        let mut visible: Vec<&TorrentSummary> = self
            .torrents
            .iter()
            .filter(|torrent| self.filter.matches(torrent))
            .filter(|torrent| needle.is_empty() || torrent.name.to_lowercase().contains(&needle))
            .collect();

        if let Some((column, ascending)) = self.sort {
            visible.sort_by(|left, right| {
                let order = match column {
                    Column::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
                    Column::Size => left.total_bytes.cmp(&right.total_bytes),
                    Column::Progress => left
                        .progress
                        .partial_cmp(&right.progress)
                        .unwrap_or(std::cmp::Ordering::Equal),
                    Column::Down => left.download_rate.cmp(&right.download_rate),
                    Column::Up => left.upload_rate.cmp(&right.upload_rate),
                    Column::Peers => left.peers.cmp(&right.peers),
                    Column::State => state_label(left.state).cmp(state_label(right.state)),
                    Column::Eta => eta_seconds(left).cmp(&eta_seconds(right)),
                };
                if ascending { order } else { order.reverse() }
            });
        }
        visible
    }

    fn table_header(&self) -> Element<'_, Message> {
        container(
            row![
                Space::new().width(2),
                container(
                    row![
                        self.header_cell("Name", Column::Name, Fill, Alignment::Start),
                        self.header_cell("Size", Column::Size, Length::Fixed(76.0), Alignment::End),
                        self.header_cell(
                            "Progress",
                            Column::Progress,
                            Length::Fixed(128.0),
                            Alignment::Start
                        ),
                        self.header_cell(
                            "Status",
                            Column::State,
                            Length::Fixed(96.0),
                            Alignment::Start
                        ),
                        self.header_cell(
                            "Peers",
                            Column::Peers,
                            Length::Fixed(48.0),
                            Alignment::End
                        ),
                        self.header_cell("Down", Column::Down, Length::Fixed(76.0), Alignment::End),
                        self.header_cell("Up", Column::Up, Length::Fixed(76.0), Alignment::End),
                        self.header_cell("ETA", Column::Eta, Length::Fixed(64.0), Alignment::End),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .width(Fill),
                )
                .width(Fill)
                .padding(Padding::new(0.0).left(10.0).right(12.0)),
            ]
            .height(metrics::TABLE_HEADER)
            .width(Fill),
        )
        .height(metrics::TABLE_HEADER)
        .width(Fill)
        .style(style::surface)
        .into()
    }

    fn header_cell(
        &self,
        label: &'static str,
        column: Column,
        width: Length,
        align: Alignment,
    ) -> Element<'_, Message> {
        let active = self.sort.is_some_and(|(active, _)| active == column);
        let arrow: Element<'_, Message> = if active {
            let arrow_icon = if self.sort.is_some_and(|(_, ascending)| ascending) {
                icons::CHEVRON_UP
            } else {
                icons::CHEVRON_DOWN
            };
            icon(arrow_icon, 12.0, T.accent).into()
        } else {
            Space::new().width(0).into()
        };
        button(
            container(
                row![
                    text(label).size(CAPTION).font(medium()).color(if active {
                        T.text_primary
                    } else {
                        T.text_muted
                    }),
                    arrow,
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            )
            .width(Fill)
            .align_x(align),
        )
        .on_press(Message::Sort(column))
        .style(style::ghost)
        .padding(0)
        .width(width)
        .height(metrics::TABLE_HEADER)
        .into()
    }

    fn table(&self, visible: &[&TorrentSummary]) -> Element<'_, Message> {
        let mut rows = column![].spacing(0).width(Fill);
        for torrent in visible {
            rows = rows.push(self.table_row(torrent));
        }
        rows.into()
    }

    fn table_row(&self, torrent: &TorrentSummary) -> Element<'_, Message> {
        let selected = self.selected.as_deref() == Some(torrent.id.as_str());
        let percent = torrent.progress.clamp(0.0, 1.0) as f32;
        let (state_icon, state_color) = state_visual(torrent.state);
        let cells = row![
            text(elide(&torrent.name, 48))
                .size(BODY)
                .font(if selected { semibold() } else { medium() })
                .color(T.text_primary)
                .wrapping(Wrapping::None)
                .width(Fill),
            numeric(size_label(torrent), 76.0, T.text_secondary),
            row![
                container(
                    progress_bar(0.0..=1.0, percent)
                        .girth(6)
                        .style(style::progress_state(torrent.state, percent))
                )
                .width(72),
                text(format!("{:.0}%", percent * 100.0))
                    .size(CAPTION)
                    .font(medium())
                    .color(percent_color(torrent.state, percent))
                    .width(40)
                    .align_x(Alignment::End),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .width(128),
            row![
                icon(state_icon, 16.0, state_color),
                text(state_label(torrent.state))
                    .size(CAPTION)
                    .font(medium())
                    .color(T.text_secondary),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .width(96),
            numeric(torrent.peers.to_string(), 48.0, T.text_secondary),
            numeric(rate_label(torrent.download_rate), 76.0, T.text_secondary),
            numeric(rate_label(torrent.upload_rate), 76.0, T.text_secondary),
            numeric(eta_label(torrent), 64.0, T.text_muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Fill);

        button(
            column![
                row![
                    selection_bar(selected, metrics::ROW - 1.0),
                    container(cells)
                        .width(Fill)
                        .height(metrics::ROW - 1.0)
                        .padding(Padding::new(0.0).left(10.0).right(12.0))
                        .align_y(Alignment::Center),
                ]
                .width(Fill)
                .height(metrics::ROW - 1.0),
                rule(),
            ]
            .width(Fill)
            .height(metrics::ROW),
        )
        .on_press(Message::Select(torrent.id.clone()))
        .style(style::row(selected))
        .padding(0)
        .width(Fill)
        .height(metrics::ROW)
        .into()
    }

    fn drop_strip(&self) -> Element<'_, Message> {
        column![
            rule(),
            container(
                row![
                    icon(icons::DOWNLOAD, 16.0, T.text_muted),
                    text("Drop .torrent files anywhere in this window, or use Add magnet")
                        .size(CAPTION)
                        .color(T.text_muted),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .height(metrics::DROP_STRIP)
            .width(Fill)
            .padding([0, 16])
            .style(style::flush),
        ]
        .into()
    }

    fn no_results(&self) -> Element<'_, Message> {
        column![
            text("No torrents match your filters")
                .size(type_scale::HEADING)
                .font(semibold())
                .color(T.text_primary),
            text("Try a different status or clear the search.")
                .size(BODY)
                .color(T.text_secondary),
            button(text("Clear filters").size(BODY).font(medium()))
                .on_press(Message::ClearFilters)
                .style(style::secondary)
                .padding([0, 14])
                .height(32),
        ]
        .spacing(12)
        .align_x(Alignment::Center)
        .into()
    }

    fn empty_state(&self) -> Element<'_, Message> {
        let hero = container(icon(icons::DOWNLOAD, 40.0, T.accent))
            .padding(16)
            .style(|_| container::Style {
                background: Some(Background::Color(T.accent_soft)),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 999.0.into(),
                },
                ..container::Style::default()
            });

        let hovered = self.drag_hover;
        let drop_zone = container(
            column![
                icon(
                    icons::DOWNLOAD,
                    32.0,
                    if hovered { T.accent } else { T.text_muted }
                ),
                text(if hovered {
                    "Release to add the torrent"
                } else {
                    "Drop .torrent files here"
                })
                .size(type_scale::HEADING)
                .font(semibold()),
                text("or paste a magnet link")
                    .size(CAPTION)
                    .color(if hovered {
                        T.accent_text
                    } else {
                        T.text_secondary
                    }),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .width(Fill)
        .height(180)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(style::drop_zone(hovered));

        container(
            column![
                hero,
                text("No torrents yet")
                    .size(type_scale::DISPLAY)
                    .font(semibold()),
                text(
                    "Add a .torrent file, paste a magnet link, or drop files anywhere in this window."
                )
                .size(BODY)
                .color(T.text_secondary),
                row![
                    button(
                        row![
                            icon(icons::PLUS, 16.0, T.accent_on),
                            text("Add .torrent").size(BODY).font(semibold())
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center)
                    )
                    .on_press(Message::AddFile)
                    .style(style::primary)
                    .padding([0, 16])
                    .height(36),
                    button(
                        row![
                            icon(icons::MAGNET, 16.0, T.text_primary),
                            text("Add magnet").size(BODY).font(medium())
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center)
                    )
                    .on_press(Message::ToggleMagnet)
                    .style(style::secondary)
                    .padding([0, 16])
                    .height(36),
                ]
                .spacing(12),
                drop_zone,
                text("Torrents stay on this device. Nothing is uploaded unless you seed.")
                    .size(MICRO)
                    .color(T.text_muted),
            ]
            .spacing(16)
            .align_x(Alignment::Center)
            .width(Fill)
            .max_width(560),
        )
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .into()
    }

    fn details(&self) -> Element<'_, Message> {
        let Some(summary) = self.selected_summary() else {
            return Space::new().height(0).into();
        };

        let content: Element<'_, Message> = match (&self.detail, self.tab) {
            (Some(detail), DetailTab::General) => self.general_tab(detail),
            (Some(detail), DetailTab::Files) => self.files_tab(detail),
            (None, _) => container(text("Loading details…").size(BODY).color(T.text_muted))
                .width(Fill)
                .height(Fill)
                .center_x(Fill)
                .center_y(Fill)
                .into(),
        };

        container(
            column![
                self.detail_identity(summary),
                self.detail_tabs(),
                container(scrollable(content).height(Fill).width(Fill))
                    .width(Fill)
                    .height(Fill)
                    .padding(Padding::new(0.0).left(16.0).right(16.0)),
            ]
            .width(Fill)
            .height(Fill),
        )
        .width(Fill)
        .height(metrics::DETAILS)
        .style(style::surface)
        .into()
    }

    fn detail_identity(&self, summary: &TorrentSummary) -> Element<'_, Message> {
        let percent = summary.progress.clamp(0.0, 1.0) as f32;
        let meta = if summary.download_rate > 0 {
            format!(
                "{}/s · ETA {}",
                format_bytes(summary.download_rate),
                eta_label(summary)
            )
        } else {
            "—".to_owned()
        };

        container(
            row![
                text(elide(&summary.name, 64))
                    .size(type_scale::HEADING)
                    .font(semibold())
                    .color(T.text_primary)
                    .wrapping(Wrapping::None)
                    .width(Fill),
                state_badge(summary.state),
                container(
                    progress_bar(0.0..=1.0, percent)
                        .girth(8)
                        .style(style::progress_state(summary.state, percent))
                )
                .width(240),
                text(format!("{:.0}%", percent * 100.0))
                    .size(CAPTION)
                    .font(medium())
                    .color(percent_color(summary.state, percent))
                    .width(44)
                    .align_x(Alignment::End),
                text(meta).size(MICRO).color(T.text_muted),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .height(metrics::DETAILS_IDENTITY)
        .padding(Padding::new(0.0).left(16.0).right(16.0))
        .into()
    }

    fn detail_tabs(&self) -> Element<'_, Message> {
        container(
            row![
                self.tab_button("General", DetailTab::General),
                self.tab_button("Files", DetailTab::Files),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .height(metrics::DETAILS_TABS)
        .padding(Padding::new(0.0).left(12.0).right(12.0))
        .into()
    }

    fn tab_button(&self, label: &'static str, tab: DetailTab) -> Element<'_, Message> {
        let selected = self.tab == tab;
        let width = match tab {
            DetailTab::General => 78.0,
            DetailTab::Files => 62.0,
        };
        column![
            button(
                text(label)
                    .size(CAPTION)
                    .font(if selected { semibold() } else { medium() })
            )
            .on_press(Message::Tab(tab))
            .style(style::tab(selected))
            .padding([0, 12])
            .height(34)
            .width(Fill),
            container(Space::new().width(Fill).height(2)).style(style::underline(selected)),
        ]
        .width(Length::Fixed(width))
        .into()
    }

    fn general_tab(&self, detail: &TorrentDetail) -> Element<'_, Message> {
        let summary = &detail.summary;
        let progress = if summary.pieces_total == 0 && summary.total_bytes == 0 {
            em().to_owned()
        } else {
            format!("{:.0}%", summary.progress.clamp(0.0, 1.0) * 100.0)
        };
        let pieces = if summary.pieces_total == 0 {
            em().to_owned()
        } else {
            format!("{} / {}", summary.pieces_have, summary.pieces_total)
        };
        let piece_length = if detail.piece_length == 0 {
            em().to_owned()
        } else {
            format_bytes(detail.piece_length)
        };
        let save_to = if detail.download_dir.is_empty() {
            em().to_owned()
        } else {
            elide_middle(&detail.download_dir, 72)
        };

        column![
            field("Size", size_label(summary)),
            field("Progress", progress),
            field("Pieces", pieces),
            field("Piece length", piece_length),
            field("Save path", save_to),
            field("State", state_label(summary.state).to_owned()),
            field("Peers", summary.peers.to_string()),
            field("Down", rate_label(summary.download_rate)),
            field("Up", rate_label(summary.upload_rate)),
            field("ETA", eta_label(summary)),
            field(
                "Private",
                if summary.private {
                    "yes".to_owned()
                } else {
                    "no".to_owned()
                },
            ),
            hash_field("Info hash", summary.id.clone()),
        ]
        .spacing(0)
        .into()
    }

    fn files_tab(&self, detail: &TorrentDetail) -> Element<'_, Message> {
        let mut files = column![].spacing(0);

        for file in &detail.files {
            let priority = PriorityChoice::from(file.priority);
            let index = file.index;
            let media = player::is_media(&file.path);
            let mut line = row![
                icon(icons::FILE, 14.0, T.text_muted),
                text(elide(&file.path, 64))
                    .size(CAPTION)
                    .color(T.text_primary)
                    .wrapping(Wrapping::None)
                    .width(Fill),
                text(format_bytes(file.length))
                    .size(MICRO)
                    .color(T.text_muted)
                    .width(76)
                    .align_x(Alignment::End),
                coverage_cell(file),
                pick_list(&PriorityChoice::ALL[..], Some(priority), move |choice| {
                    Message::Priority(index, choice)
                })
                .text_size(MICRO)
                .style(style::pick)
                .width(92),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .height(28)
            .width(Fill);
            if media {
                let title = file.path.clone();
                line = line.push(play_button(
                    detail.summary.id.clone(),
                    file.index,
                    title,
                    file.completed || file.bytes_completed > 0,
                ));
            } else {
                line = line.push(Space::new().width(64));
            }
            files = files.push(line);
        }

        files.into()
    }

    fn banner_message(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(error) = &self.error {
            parts.push(error.clone());
        }
        if let Some(torrent) = self.error_torrent()
            && let Some(error) = &torrent.last_error
            && self.dismissed_error.as_deref() != Some(error.as_str())
            && !parts.iter().any(|part| part == error)
        {
            parts.push(format!("{}: {error}", elide(&torrent.name, 40)));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }

    fn error_torrent(&self) -> Option<&TorrentSummary> {
        self.selected_summary()
            .filter(|torrent| torrent.last_error.is_some())
            .or_else(|| {
                self.torrents
                    .iter()
                    .find(|torrent| torrent.last_error.is_some())
            })
    }

    fn torrent_error(&self) -> Option<String> {
        self.error_torrent()
            .and_then(|torrent| torrent.last_error.clone())
    }

    fn selected_summary(&self) -> Option<&TorrentSummary> {
        let id = self.selected.as_deref()?;
        self.torrents.iter().find(|torrent| torrent.id == id)
    }

    fn error_banner(&self, message: &str) -> Element<'_, Message> {
        container(
            row![
                container(Space::new().width(3).height(40)).style(|_| {
                    container::Style {
                        background: Some(Background::Color(T.danger)),
                        ..container::Style::default()
                    }
                }),
                row![
                    icon(icons::ALERT, 16.0, T.danger),
                    text(message.to_owned())
                        .size(CAPTION)
                        .color(T.text_primary)
                        .wrapping(Wrapping::Word)
                        .width(Fill),
                    button(text("Dismiss").size(MICRO).font(semibold()))
                        .on_press(Message::ClearError)
                        .style(style::danger_text)
                        .padding([0, 8])
                        .height(24),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .width(Fill)
                .padding(Padding::new(0.0).left(12.0).right(12.0)),
            ]
            .height(40)
            .width(Fill),
        )
        .height(40)
        .width(Fill)
        .style(style::error_banner)
        .into()
    }

    fn status_bar(&self) -> Element<'_, Message> {
        let down: u64 = self.torrents.iter().map(|t| t.download_rate).sum();
        let up: u64 = self.torrents.iter().map(|t| t.upload_rate).sum();
        let selection = if self.selected.is_some() {
            " · 1 selected"
        } else {
            ""
        };
        let noun = if self.stats.torrents == 1 {
            "torrent"
        } else {
            "torrents"
        };

        container(
            row![
                text(format!(
                    "{} {noun} · {} active · {} seeding · {} paused{selection}",
                    self.stats.torrents, self.stats.active, self.stats.seeding, self.stats.paused
                ))
                .size(CAPTION)
                .color(T.text_muted),
                Space::new().width(20),
                text(if self.dht { "DHT on" } else { "DHT off" })
                    .size(CAPTION)
                    .color(T.text_muted),
                Space::new().width(Fill),
                text(format!("session {}", ratio_label(&self.stats)))
                    .size(CAPTION)
                    .color(T.text_muted),
                Space::new().width(20),
                text(format!(
                    "↓ {}/s   ↑ {}/s",
                    format_bytes(down),
                    format_bytes(up)
                ))
                .size(CAPTION)
                .color(T.text_secondary),
            ]
            .align_y(Alignment::Center),
        )
        .padding([0, 16])
        .height(metrics::STATUS)
        .width(Fill)
        .style(style::status_bar)
        .into()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new(Initial::default())
    }
}

/// Maps window drop events to messages.
fn drag_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(window::Event::FileDropped(path)) => Some(Message::Dropped(path)),
        iced::Event::Window(window::Event::FileHovered(_)) => Some(Message::DragHover(true)),
        iced::Event::Window(window::Event::FilesHoveredLeft) => Some(Message::DragHover(false)),
        _ => None,
    }
}

fn close_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(window::Event::CloseRequested) => Some(Message::CloseRequested),
        _ => None,
    }
}

fn key_event(
    event: iced::Event,
    status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key,
        modified_key,
        modifiers,
        text,
        ..
    }) = event
    else {
        return None;
    };

    if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
        return Some(Message::Escape);
    }
    if modifiers.command() && is_ctrl_f(&modified_key, text.as_deref()) {
        // Ctrl+F must also work from the magnet field, whose text input may
        // claim the event.
        return Some(Message::FocusSearch);
    }
    if status != iced::event::Status::Ignored {
        return None;
    }
    shortcut(key, modified_key, text.as_deref(), modifiers)
}

/// `Ctrl+F` is either the logical key or the control character `U+0006`.
/// `text_with_all_modifiers` produces the control code even on keyboard layouts
/// where the logical key is not Latin, so search stays reachable there.
fn is_ctrl_f(modified_key: &keyboard::Key, text: Option<&str>) -> bool {
    matches!(
        modified_key.as_ref(),
        keyboard::Key::Character("f") | keyboard::Key::Character("F")
    ) || text == Some("\u{6}")
}

/// Key mapping shared with [`key_event`]. The window listener routes `Esc`
/// (and `Ctrl+F`) before checking whether a widget captured the event; the
/// remaining shortcuts only fire on an ignored event, so they stay inert while
/// a text field is editing.
fn shortcut(
    key: keyboard::Key,
    modified_key: keyboard::Key,
    text: Option<&str>,
    modifiers: keyboard::Modifiers,
) -> Option<Message> {
    match key {
        keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Escape),
        keyboard::Key::Named(keyboard::key::Named::Space) if modifiers.is_empty() => {
            Some(Message::ToggleSelectedPause)
        }
        keyboard::Key::Named(keyboard::key::Named::Delete) if modifiers.is_empty() => {
            Some(Message::DeleteSelected)
        }
        _ if modifiers.command() && is_ctrl_f(&modified_key, text) => Some(Message::FocusSearch),
        _ => None,
    }
}

fn section(label: &'static str) -> Element<'static, Message> {
    container(text(label).size(MICRO).font(semibold()).color(T.text_muted))
        .padding(Padding::new(0.0).top(16.0).bottom(8.0).left(12.0))
        .into()
}

fn rule() -> Element<'static, Message> {
    container(Space::new().height(1))
        .width(Fill)
        .height(1)
        .style(style::rule)
        .into()
}

fn rule_strong() -> Element<'static, Message> {
    container(Space::new().height(1))
        .width(Fill)
        .height(1)
        .style(style::rule_strong)
        .into()
}

fn vertical_rule() -> Element<'static, Message> {
    container(Space::new().width(1))
        .width(1)
        .height(Fill)
        .style(style::rule)
        .into()
}

fn selection_bar(selected: bool, height: f32) -> Element<'static, Message> {
    container(Space::new().width(2).height(height))
        .width(2)
        .height(height)
        .style(style::selection_bar(selected))
        .into()
}

fn toolbar_button(
    bytes: &'static [u8],
    label: &'static str,
    message: Message,
    enabled: bool,
) -> Element<'static, Message> {
    button(
        row![
            icon(
                bytes,
                18.0,
                if enabled {
                    T.text_secondary
                } else {
                    T.text_disabled
                }
            ),
            text(label).size(BODY).font(medium()),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .on_press_maybe(enabled.then_some(message))
    .style(style::ghost)
    .padding([0, 12])
    .height(32)
    .into()
}

fn numeric(value: String, width: f32, color: Color) -> Element<'static, Message> {
    text(value)
        .size(CAPTION)
        .font(medium())
        .color(color)
        .width(Length::Fixed(width))
        .align_x(Alignment::End)
        .into()
}

fn field(label: &'static str, value: String) -> Element<'static, Message> {
    row![
        text(label).size(CAPTION).color(T.text_muted).width(96),
        text(value)
            .size(BODY)
            .color(T.text_primary)
            .wrapping(Wrapping::None),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .height(28)
    .into()
}

fn hash_field(label: &'static str, value: String) -> Element<'static, Message> {
    row![
        text(label).size(CAPTION).color(T.text_muted).width(96),
        text(value)
            .size(CAPTION)
            .font(Font {
                family: font::Family::Monospace,
                ..Font::DEFAULT
            })
            .color(T.text_secondary),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .height(28)
    .into()
}

fn state_badge(state: TorrentState) -> Element<'static, Message> {
    let (bytes, color) = state_visual(state);
    let soft = state_soft(state);
    container(
        row![
            icon(bytes, 12.0, color),
            text(state_label(state))
                .size(MICRO)
                .font(semibold())
                .color(color),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    )
    .height(22)
    .padding([0, 8])
    .style(move |_| container::Style {
        background: Some(Background::Color(soft)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 999.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

fn coverage_cell(file: &FileView) -> Element<'static, Message> {
    if file.completed {
        container(
            row![
                icon(icons::CHECK, 12.0, T.success),
                text("Verified").size(MICRO).color(T.success),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .width(140)
        .align_x(Alignment::End)
        .into()
    } else {
        text(format!(
            "{} of {} verified",
            format_bytes(file.bytes_completed),
            format_bytes(file.length)
        ))
        .size(MICRO)
        .color(T.text_muted)
        .wrapping(Wrapping::None)
        .width(140)
        .align_x(Alignment::End)
        .into()
    }
}

fn play_button(id: String, file: usize, title: String, enabled: bool) -> Element<'static, Message> {
    button(
        row![
            icon(
                icons::PLAY,
                12.0,
                if enabled { T.accent } else { T.text_disabled }
            ),
            text("Play").size(MICRO).font(medium()).color(if enabled {
                T.text_primary
            } else {
                T.text_disabled
            }),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    )
    .on_press_maybe(enabled.then_some(Message::PlayStream { id, file, title }))
    .style(style::ghost)
    .padding([0, 8])
    .height(24)
    .width(64)
    .into()
}

fn raw_hwnd(window: &dyn window::Window) -> Option<isize> {
    #[cfg(windows)]
    {
        let handle = window.window_handle().ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(win) => Some(win.hwnd.get()),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        None
    }
}

fn state_visual(state: TorrentState) -> (&'static [u8], Color) {
    match state {
        TorrentState::Queued => (icons::CLOCK, T.info),
        TorrentState::Checking => (icons::ROTATE, T.warning),
        TorrentState::Downloading => (icons::ARROW_DOWN, T.accent),
        TorrentState::Seeding => (icons::ARROW_UP, T.success),
        TorrentState::Complete => (icons::CHECK_CIRCLE, T.success),
        TorrentState::Paused => (icons::PAUSE, T.text_muted),
        TorrentState::Error => (icons::ALERT, T.danger),
    }
}

fn state_soft(state: TorrentState) -> Color {
    match state {
        TorrentState::Queued | TorrentState::Paused => T.overlay,
        TorrentState::Checking => T.warning_soft,
        TorrentState::Downloading => T.accent_soft,
        TorrentState::Seeding | TorrentState::Complete => T.success_soft,
        TorrentState::Error => T.danger_soft,
    }
}

fn state_label(state: TorrentState) -> &'static str {
    match state {
        TorrentState::Queued => "Queued",
        TorrentState::Checking => "Checking",
        TorrentState::Downloading => "Downloading",
        TorrentState::Seeding => "Seeding",
        TorrentState::Complete => "Completed",
        TorrentState::Paused => "Paused",
        TorrentState::Error => "Error",
    }
}

/// `—` is the only honest value for a size the engine has not reported yet.
fn size_label(torrent: &TorrentSummary) -> String {
    if torrent.total_bytes == 0 {
        em().to_owned()
    } else {
        format_bytes(torrent.total_bytes)
    }
}

fn rate_label(rate: u64) -> String {
    format!("{}/s", format_bytes(rate))
}

fn ratio_label(stats: &Stats) -> String {
    if stats.bytes_downloaded == 0 {
        em().to_owned()
    } else {
        format!(
            "{:.2}",
            stats.bytes_uploaded as f64 / stats.bytes_downloaded as f64
        )
    }
}

fn percent_color(state: TorrentState, fraction: f32) -> Color {
    match state {
        TorrentState::Error => T.danger,
        TorrentState::Paused => T.text_muted,
        TorrentState::Checking => T.warning,
        _ if fraction >= 0.999 => T.success,
        _ => T.text_secondary,
    }
}

fn em() -> &'static str {
    "—"
}

fn eta_seconds(torrent: &TorrentSummary) -> u64 {
    if torrent.total_bytes == 0 || torrent.download_rate == 0 || torrent.progress >= 1.0 {
        return u64::MAX;
    }
    let remaining = torrent
        .total_bytes
        .saturating_sub((torrent.progress.clamp(0.0, 1.0) * torrent.total_bytes as f64) as u64);
    remaining / torrent.download_rate.max(1)
}

fn eta_label(torrent: &TorrentSummary) -> String {
    let seconds = eta_seconds(torrent);
    if seconds == u64::MAX {
        em().to_owned()
    } else {
        format_duration(seconds)
    }
}

/// Truncates with a trailing ellipsis; the cell still clips via
/// `Wrapping::None`, so a very narrow window never grows a row.
fn elide(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_owned();
    }
    let mut truncated: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

/// Middle ellipsis for paths: keep the drive/prefix and the final folder.
fn elide_middle(value: &str, max_chars: usize) -> String {
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

/// Human-readable byte count.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Human-readable duration for an ETA.
#[must_use]
pub fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    }
}

fn default_config() -> Config {
    if let Some(path) = std::env::var_os("OXIDE_FLUX_DOWNLOAD_DIR") {
        return Config {
            download_dir: PathBuf::from(path),
            ..Config::default()
        };
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    Config {
        download_dir: home.join("Downloads").join("OxideFlux"),
        ..Config::default()
    }
}

async fn pick_torrent() -> Option<Vec<u8>> {
    let file = rfd::AsyncFileDialog::new()
        .add_filter("Torrent files", &["torrent"])
        .pick_file()
        .await?;
    Some(file.read().await)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn summary(total_bytes: u64, progress: f64, download_rate: u64) -> TorrentSummary {
        TorrentSummary {
            id: "abc".to_owned(),
            name: "Example".to_owned(),
            state: TorrentState::Downloading,
            private: false,
            progress,
            pieces_have: 0,
            pieces_total: 0,
            total_bytes,
            peers: 0,
            download_rate,
            upload_rate: 0,
            last_error: None,
        }
    }

    #[test]
    fn unknown_values_render_as_an_em_dash() {
        assert_eq!(size_label(&summary(0, 0.0, 0)), "—");
        assert_eq!(eta_label(&summary(0, 0.0, 1024)), "—");
        assert_eq!(eta_label(&summary(1024, 0.5, 0)), "—");
        assert_eq!(ratio_label(&Stats::default()), "—");
        assert_eq!(
            ratio_label(&Stats {
                bytes_downloaded: 0,
                bytes_uploaded: 9,
                ..Stats::default()
            }),
            "—"
        );
    }

    #[test]
    fn measured_values_stay_visible() {
        assert_eq!(size_label(&summary(1024, 0.5, 0)), "1.0 KiB");
        assert_eq!(rate_label(0), "0 B/s");
        assert_eq!(eta_label(&summary(1024, 0.5, 512)), "1s");
        assert_eq!(
            ratio_label(&Stats {
                bytes_downloaded: 100,
                bytes_uploaded: 50,
                ..Stats::default()
            }),
            "0.50"
        );
    }

    #[test]
    fn completed_filter_includes_seeding_and_complete() {
        let mut torrent = summary(1024, 1.0, 0);
        torrent.state = TorrentState::Seeding;
        assert!(Filter::Completed.matches(&torrent));
        torrent.state = TorrentState::Complete;
        assert!(Filter::Completed.matches(&torrent));
        torrent.state = TorrentState::Paused;
        assert!(!Filter::Completed.matches(&torrent));
    }

    #[test]
    fn long_names_keep_a_trailing_ellipsis() {
        assert_eq!(elide("short", 8), "short");
        assert_eq!(elide("abcdefghij", 8), "abcdefg…");
        assert_eq!(elide_middle("abcdefghijklmnop", 8), "abc…mnop");
        assert_eq!(elide_middle("short", 8), "short");
    }

    #[test]
    fn shortcuts_match_the_documented_keys() {
        assert!(matches!(
            shortcut(
                keyboard::Key::Named(keyboard::key::Named::Escape),
                keyboard::Key::Named(keyboard::key::Named::Escape),
                None,
                keyboard::Modifiers::default(),
            ),
            Some(Message::Escape)
        ));
        assert!(matches!(
            shortcut(
                keyboard::Key::Named(keyboard::key::Named::Space),
                keyboard::Key::Named(keyboard::key::Named::Space),
                None,
                keyboard::Modifiers::default(),
            ),
            Some(Message::ToggleSelectedPause)
        ));
        assert!(matches!(
            shortcut(
                keyboard::Key::Named(keyboard::key::Named::Delete),
                keyboard::Key::Named(keyboard::key::Named::Delete),
                None,
                keyboard::Modifiers::default(),
            ),
            Some(Message::DeleteSelected)
        ));
        assert!(matches!(
            shortcut(
                keyboard::Key::Character("f".into()),
                keyboard::Key::Character("f".into()),
                None,
                keyboard::Modifiers::CTRL,
            ),
            Some(Message::FocusSearch)
        ));
        assert!(
            shortcut(
                keyboard::Key::Named(keyboard::key::Named::Space),
                keyboard::Key::Named(keyboard::key::Named::Space),
                None,
                keyboard::Modifiers::SHIFT,
            )
            .is_none()
        );
    }

    #[test]
    fn ctrl_f_falls_back_to_the_control_character() {
        // A non-Latin layout produces U+0006 as the text even though the
        // logical key is not `f`.
        assert!(matches!(
            shortcut(
                keyboard::Key::Character("ф".into()),
                keyboard::Key::Character("ф".into()),
                Some("\u{6}"),
                keyboard::Modifiers::CTRL,
            ),
            Some(Message::FocusSearch)
        ));
    }

    fn key_pressed(key: keyboard::Key) -> iced::Event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::default(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn escape_survives_a_capturing_text_field_but_space_does_not() {
        let window = window::Id::unique();
        assert!(matches!(
            key_event(
                key_pressed(keyboard::Key::Named(keyboard::key::Named::Escape)),
                iced::event::Status::Captured,
                window,
            ),
            Some(Message::Escape)
        ));
        assert!(
            key_event(
                key_pressed(keyboard::Key::Named(keyboard::key::Named::Space)),
                iced::event::Status::Captured,
                window,
            )
            .is_none()
        );
    }
}
