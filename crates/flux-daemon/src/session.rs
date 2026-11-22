//! The session actor: one task owns every torrent, everything else sends messages.
//!
//! # Why an actor
//!
//! Piece state, file priorities, queue slots and verification progress all have to
//! agree with each other. Modelling that as one task with a command channel means a
//! snapshot can never show a torrent as `Seeding` while another task is still
//! hashing it. The command channel is bounded: a caller that outruns the actor is
//! slowed down instead of growing a queue.
//!
//! # What transfers and what does not
//!
//! Each verified torrent that holds a queue slot gets a [`crate::swarm::Swarm`]:
//! that is what dials peers, requests blocks, verifies pieces and serves uploads.
//! States stay honest: `Active` means "running, not complete yet", `Ready`/`Seeding`
//! mean every **desired** piece is verified. Bytes and peer counts come from the
//! swarm's own events, never from an estimate.<br>
//! Still absent: uTP, MSE, PEX, LSD and v2 Merkle verification; a v2-only torrent
//! is storage-ready but cannot be downloaded.
//!
//! # Queueing
//!
//! * incomplete torrents compete for [`Config::max_active_downloads`] slots,
//! * complete torrents compete for [`Config::max_active_seeds`] slots,
//! * waiting torrents are promoted strictly in add order (the sequence number is
//!   taken when the add command is received, not when its files finish being
//!   prepared), and promotion is re-evaluated on completion, removal, pause and
//!   resume.
//!
//! Verification runs one torrent at a time, so a batch of adds cannot spawn a
//! hashing pool per torrent.
//!
//! # Where files live
//!
//! Every torrent owns a download root, resolved once when it is added: the
//! caller's choice, or [`Config::download_dir`] when the caller passes none.
//! Preallocation, verification, the swarm, streaming and file deletion all read
//! that root, so two torrents that happen to share a name never write into each
//! other's folders. The root is persisted per torrent only when it differs from
//! the session default, which keeps existing state files unchanged.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use flux_core::metainfo::TorrentKind;
use flux_core::picker::Mode;
use flux_net::dht::{self, DhtConfig, DhtHandle};
use flux_net::handshake::{HANDSHAKE_LEN, Handshake};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::announce::AnnounceSpec;
use crate::config::Config;
use crate::id::TorrentId;
use crate::pieces::{self, PieceBitfield, Priority};
use crate::state::{STATE_VERSION, SessionSnapshot, StateError, StateStore, TorrentRecord};
use crate::swarm::{self, SwarmConfig, SwarmEvent, SwarmHandle};
use crate::verify::{self, AddError, CheckJob, CheckOutcome, CheckRun, CheckUpdate, TorrentPlan};

/// Commands that may be queued before a caller blocks.
const COMMAND_CAPACITY: usize = 64;
/// Caller-visible events that may be buffered before the oldest is dropped.
const EVENT_CAPACITY: usize = 512;
/// Messages the preparation and verification tasks may queue for the actor.
const INTERNAL_CAPACITY: usize = 256;
/// Piece results a verification pass may queue before it blocks.
const CHECK_CAPACITY: usize = 256;
/// A pass reports progress at most this many times.
const PROGRESS_STEPS: usize = 100;
/// Swarm events buffered before the swarm blocks (backpressure, not loss).
const SWARM_EVENT_CAPACITY: usize = 256;
/// How long an inbound peer has to send its handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Ports tried in order when the configured listen port is taken.
const PORT_ATTEMPTS: u16 = 10;
/// How often trackerless peers are looked up.
const DHT_INTERVAL: Duration = Duration::from_secs(300);

type Reply<T> = oneshot::Sender<Result<T, SessionError>>;

/// Lifecycle state of one torrent.
///
/// Every variant is reachable in this build; none of them implies network
/// activity. The string form ([`Status::as_str`]) is the API value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// Verified incomplete and waiting for a download slot.
    Queued,
    /// Files have been created and preallocated; verification has not started.
    Preallocated,
    /// Existing data is being hashed.
    Checking,
    /// Paused by the user. A paused torrent holds no queue slot.
    Paused,
    /// Verified incomplete, holding a download slot. Nothing can arrive yet: this
    /// build has no peer networking.
    Active,
    /// Complete (every piece verified) and waiting for a seed slot.
    Ready,
    /// Complete and holding an active seed slot. The slot is reserved for the
    /// future peer layer; no peer is being served today.
    Seeding,
}

impl Status {
    /// Stable lowercase name used by the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preallocated => "preallocated",
            Self::Checking => "checking",
            Self::Paused => "paused",
            Self::Active => "active",
            Self::Ready => "ready",
            Self::Seeding => "seeding",
        }
    }

    /// Whether the torrent holds a download or seed slot.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active | Self::Seeding)
    }

    /// Whether the torrent is complete and can serve data.
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Ready | Self::Seeding)
    }

    /// Whether the torrent is waiting for a slot.
    pub fn is_waiting(self) -> bool {
        matches!(self, Self::Queued | Self::Ready)
    }
}

/// How far hash verification got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verification {
    /// Not started.
    Pending,
    /// Hashing now.
    Running,
    /// Every piece was hashed; the torrent's piece counts say what was found.
    Complete,
    /// The torrent only carries v2 Merkle hashes, which this build cannot check.
    Unsupported,
    /// The pass could not finish; see the torrent's `last_error`.
    Failed,
}

impl Verification {
    /// Stable lowercase name used by the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

/// One file as reported by the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    /// Index into the torrent's file list.
    pub index: usize,
    /// Sanitized path relative to the download root, with `/` separators.
    pub path: String,
    /// Declared length in bytes.
    pub length: u64,
    /// Torrent-space offset.
    pub offset: u64,
    /// Current priority.
    pub priority: Priority,
    /// Whether the priority is not [`Priority::Skip`].
    pub selected: bool,
    /// Whether every byte of the file is covered by verified pieces.
    pub completed: bool,
    /// Verified bytes that fall inside this file.
    pub bytes_completed: u64,
}

/// One file as the streaming layer needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFile {
    /// Index in torrent order.
    pub index: usize,
    /// Sanitized path relative to the download root.
    pub path: String,
    /// Start in the torrent address space.
    pub offset: u64,
    /// Declared length.
    pub length: u64,
    /// First piece that overlaps this file.
    pub piece_start: usize,
    /// One past the last piece that overlaps this file.
    pub piece_end: usize,
}

/// Everything a `StreamSource` needs for one torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLayout {
    /// Download root.
    pub root: PathBuf,
    /// Piece length in bytes.
    pub piece_length: u32,
    /// Files in torrent order.
    pub files: Vec<StreamFile>,
}

/// A torrent as reported by the session.
///
/// This is the JSON-visible shape the control API mirrors. Lengths are
/// address-space lengths: `total_length` includes v2 alignment gaps,
/// `payload_length` does not. There are deliberately no speed, peer or transfer
/// fields — nothing in this build measures them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentState {
    /// Info hash hex, v1 preferred, v2 for v2-only torrents.
    pub id: TorrentId,
    /// Display name.
    pub name: String,
    /// `private: 1`. The future DHT/PEX/LSD layer must disable itself when true.
    pub private: bool,
    /// v1, v2 or hybrid.
    pub kind: TorrentKind,
    /// Absolute root this torrent's files live under: either the folder it was
    /// added with or the session's configured download directory.
    pub root: PathBuf,
    /// Lifecycle state.
    pub status: Status,
    /// Hash verification progress.
    pub verification: Verification,
    /// Piece address-space length, including v2 alignment gaps.
    pub total_length: u64,
    /// Sum of the file lengths, excluding alignment gaps.
    pub payload_length: u64,
    /// Piece length in bytes.
    pub piece_length: u32,
    /// Number of pieces.
    pub piece_count: usize,
    /// Pieces whose bytes were verified.
    pub pieces_have: usize,
    /// Pieces that were readable but hashed wrong; they need re-downloading.
    pub pieces_corrupt: usize,
    /// Pieces needed by the current file selection.
    pub pieces_desired: usize,
    /// Verified bytes in the address space.
    pub bytes_have: u64,
    /// Bytes covered by the desired pieces.
    pub bytes_desired: u64,
    /// Connected peers, as reported by the swarm (0 when no swarm is running).
    pub peers: usize,
    /// Payload bytes downloaded by this swarm instance.
    pub downloaded_bytes: u64,
    /// Payload bytes uploaded by this swarm instance.
    pub uploaded_bytes: u64,
    /// Current download rate in bytes per second.
    pub download_rate: u64,
    /// Current upload rate in bytes per second.
    pub upload_rate: u64,
    /// Whether the torrent holds a download or seed slot.
    pub active: bool,
    /// Whether every desired piece is verified. Only then can the status be
    /// `Ready`/`Seeding`.
    pub complete: bool,
    /// 1-based position among waiting torrents, in add order.
    pub queue_position: Option<usize>,
    /// Last failure (allocation, verification, ...), if any.
    pub last_error: Option<String>,
    /// Per-file state, in torrent order.
    pub files: Vec<FileState>,
}

/// A priority assignment for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilePriority {
    /// File index.
    pub index: usize,
    /// New priority.
    pub priority: Priority,
}

impl From<(usize, Priority)> for FilePriority {
    fn from((index, priority): (usize, Priority)) -> Self {
        Self { index, priority }
    }
}

/// The two bitfields that describe a torrent's pieces.
#[derive(Debug, Clone)]
pub struct PieceStateView {
    /// Pieces the current selection needs.
    pub desired: PieceBitfield,
    /// Pieces whose bytes were verified.
    pub have: PieceBitfield,
}

/// Something the session wants observers to know.
///
/// Delivered over a bounded [`broadcast`] channel; a slow observer sees
/// [`broadcast::error::RecvError::Lagged`] and should resynchronise with
/// [`SessionHandle::list`].
#[derive(Debug, Clone)]
pub enum Event {
    /// A torrent finished being prepared and is now part of the session.
    Added {
        /// Its state at registration. Boxed because the event channel is cloned
        /// per subscriber and a full snapshot is by far the largest thing it
        /// carries; every queued event would otherwise reserve room for it.
        torrent: Box<TorrentState>,
    },
    /// A torrent was removed.
    Removed {
        /// Its id.
        id: TorrentId,
    },
    /// The lifecycle state changed.
    StatusChanged {
        /// Its id.
        id: TorrentId,
        /// The new status.
        status: Status,
    },
    /// Verification made progress. Emitted at most ~100 times per pass.
    Checked {
        /// Its id.
        id: TorrentId,
        /// Pieces hashed so far.
        pieces_done: usize,
        /// Pieces in the pass.
        pieces_total: usize,
    },
    /// Verification finished.
    Finished {
        /// Its id.
        id: TorrentId,
        /// Pieces that matched their hash.
        have: usize,
        /// Pieces that did not.
        missing: usize,
    },
    /// File priorities changed, so the desired-piece set changed.
    Updated {
        /// Its id.
        id: TorrentId,
    },
    /// The session stopped.
    Shutdown,
    /// A non-fatal problem worth surfacing (listener port, persistence).
    Warning(String),
}

/// An owned handle to a running session.
///
/// Cheap to clone; every clone talks to the same actor. The actor stops when the
/// last handle is dropped or after [`SessionHandle::shutdown`].
#[derive(Debug, Clone)]
pub struct SessionHandle {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
    listen_port: u16,
    peer_id: [u8; 20],
    dht: Option<DhtHandle>,
}

impl SessionHandle {
    /// Validates the config, creates the download directory and spawns the actor.
    ///
    /// Must be called from within a tokio runtime; the actor lives on it.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Config`] for an invalid config and
    /// [`SessionError::Io`] if the download directory cannot be created.
    pub async fn start(config: Config) -> Result<Self, SessionError> {
        config.validate()?;
        let root = config.download_dir.clone();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&root))
            .await?
            .map_err(|source| SessionError::Io {
                path: config.download_dir.clone(),
                source,
            })?;

        let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (internal, internal_rx) = mpsc::channel(INTERNAL_CAPACITY);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);

        // A listener is what makes seeding and tracker announces possible. A busy
        // port is not fatal: try the next few, then an ephemeral one, and announce
        // the port that actually bound. `listen_port = 0` means "pick one".
        let (bound, port) =
            bind_listener(config.listen_port)
                .await
                .map_err(|source| SessionError::Io {
                    path: PathBuf::from(format!("listen:{}", config.listen_port)),
                    source,
                })?;
        let listen_port = port;
        let listener = Some(spawn_accept_loop(bound, internal.clone()));

        // Trackerless discovery is opt-in and never sees a private torrent.
        let dht = if config.enable_dht {
            start_dht(&config, internal.clone(), events.clone()).await
        } else {
            None
        };

        // Persisted torrents are restored through the normal add path, so paths,
        // preallocation and verification behave exactly as if they were added now.
        let store = if config.persist {
            Some(match &config.state_file {
                Some(path) => StateStore::new(path.clone()),
                None => StateStore::for_download_dir(&config.download_dir),
            })
        } else {
            None
        };
        let mut restore = HashMap::new();
        let mut restore_order = Vec::new();
        if let Some(store) = &store {
            let snapshot = store.load_async().await?;
            for record in snapshot.torrents {
                let id = TorrentId::new(record.info_hash.clone()).map_err(|_| {
                    SessionError::State(StateError::Corrupt {
                        path: store.path().to_path_buf(),
                        reason: format!("invalid info_hash `{}`", record.info_hash),
                    })
                })?;
                if restore.insert(id.clone(), record).is_none() {
                    restore_order.push(id);
                }
            }
        }

        let peer_id = generate_peer_id();
        let session = Session {
            config: Arc::new(config),
            torrents: HashMap::new(),
            next_seq: 0,
            command_rx,
            internal_rx,
            internal: internal.clone(),
            events: events.clone(),
            active_check: None,
            next_check_epoch: 0,
            peer_id,
            listen_port,
            listener,
            store,
            restore,
            restore_order,
            persist_pending: false,
            save_task: None,
            dht: dht.clone(),
            dht_tick: tokio::time::interval(DHT_INTERVAL),
        };
        tokio::spawn(session.run());
        Ok(Self {
            commands,
            events,
            listen_port,
            peer_id,
            dht,
        })
    }

    /// Fetches metadata for a magnet link and adds the resulting torrent.
    ///
    /// Uses the link's trackers; when it has none, the DHT is asked for peers
    /// first. A magnet with no trackers and no DHT peers fails with
    /// [`SessionError::Magnet`] — nothing is added and no state is invented.
    ///
    /// # Errors
    ///
    /// [`SessionError::MagnetLink`] for a malformed URI, [`SessionError::Magnet`]
    /// when metadata cannot be fetched or does not hash to the link's info hash,
    /// plus everything [`SessionHandle::add_torrent`] reports.
    pub async fn add_magnet(&self, uri: &str) -> Result<TorrentState, SessionError> {
        let link = flux_core::magnet::parse(uri)
            .map_err(|error| SessionError::MagnetLink(error.to_string()))?;
        let mut spec =
            crate::magnet::FetchSpec::from_magnet(&link, self.peer_id, self.listen_port)?;
        if spec.trackers.is_empty()
            && let Some(dht) = &self.dht
        {
            // A magnet without trackers has no other peer source. The privacy
            // flag is unknown until the metadata arrives; once it does, the
            // swarm refuses DHT for a private torrent.
            spec.seeds = dht.get_peers(spec.info_hash).await;
        }
        let metadata = crate::magnet::fetch(spec, None).await?;
        self.add_torrent(metadata.torrent).await
    }

    /// The port inbound peers can reach, or `0` when listening is disabled.
    pub fn listen_port(&self) -> u16 {
        self.listen_port
    }

    /// Dials one peer for a torrent.
    ///
    /// Returns `false` when the torrent has no running swarm (paused, waiting for
    /// a slot, unverifiable, or unknown) or when the address is already connected.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] when the actor is gone.
    pub async fn add_peer(
        &self,
        id: &TorrentId,
        addr: std::net::SocketAddr,
    ) -> Result<bool, SessionError> {
        let id = id.clone();
        self.request(|reply| Command::AddPeer { id, addr, reply })
            .await
    }

    /// Subscribes to the event stream. Subscribe before adding a torrent if you
    /// need its `Added` event.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Adds a torrent whose files live under `root` instead of the configured
    /// download directory.
    ///
    /// `None` means [`Config::download_dir`], which is what
    /// [`SessionHandle::add_torrent`] does. The root is resolved and created here
    /// once, so verification, the swarm, streaming and file deletion cannot
    /// disagree about where the bytes belong. A path that exists but is not a
    /// directory is rejected with [`SessionError::RootNotDirectory`].
    ///
    /// # Errors
    ///
    /// [`SessionError::Add`] for bad or untrustworthy metadata or a failed
    /// allocation, [`SessionError::Duplicate`] if the id is already present,
    /// [`SessionError::RootNotDirectory`] or [`SessionError::Io`] for an unusable
    /// root.
    pub async fn add_torrent_with_root(
        &self,
        bytes: impl Into<Vec<u8>>,
        root: Option<PathBuf>,
    ) -> Result<TorrentState, SessionError> {
        self.request(|reply| Command::AddTorrent {
            bytes: bytes.into(),
            root,
            reply,
        })
        .await
    }

    /// Adds a torrent from `.torrent` bytes into the configured download
    /// directory.
    ///
    /// Parses the metainfo, sanitizes every path, preallocates the files and
    /// queues hash verification. Returns once the files exist; the returned state
    /// is `Preallocated` or `Checking` (or already `Verified` for a v2-only
    /// torrent, whose Merkle hashes this build cannot check).
    ///
    /// # Errors
    ///
    /// [`SessionError::Add`] for bad or untrustworthy metadata or a failed
    /// allocation, [`SessionError::Duplicate`] if the id is already present.
    pub async fn add_torrent(
        &self,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<TorrentState, SessionError> {
        self.add_torrent_with_root(bytes, None).await
    }

    /// Reads a `.torrent` file from disk and adds it. Blocking I/O runs on a
    /// blocking thread.
    ///
    /// # Errors
    ///
    /// [`SessionError::ReadTorrent`] if the file cannot be read, plus everything
    /// [`SessionHandle::add_torrent`] reports.
    pub async fn add_torrent_file(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<TorrentState, SessionError> {
        let path = path.into();
        let read_path = path.clone();
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(read_path))
            .await?
            .map_err(|source| SessionError::ReadTorrent { path, source })?;
        self.add_torrent(bytes).await
    }

    /// Removes a torrent, optionally deleting its files.
    ///
    /// The queue is re-evaluated immediately; file deletion, when requested, runs
    /// on a blocking thread and this future resolves once it is done.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`], or [`SessionError::Add`] if deletion fails.
    pub async fn remove(&self, id: &TorrentId, delete_files: bool) -> Result<(), SessionError> {
        let id = id.clone();
        self.request(|reply| Command::Remove {
            id,
            delete_files,
            reply,
        })
        .await
    }

    /// Pauses a torrent and releases its queue slot.
    ///
    /// An in-flight verification pass is not interrupted; it just cannot promote
    /// the torrent until it is resumed.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`].
    pub async fn pause(&self, id: &TorrentId) -> Result<TorrentState, SessionError> {
        let id = id.clone();
        self.request(|reply| Command::Pause { id, reply }).await
    }

    /// Resumes a paused torrent, re-entering the queue in add order.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`].
    pub async fn resume(&self, id: &TorrentId) -> Result<TorrentState, SessionError> {
        let id = id.clone();
        self.request(|reply| Command::Resume { id, reply }).await
    }

    /// Applies per-file priorities and recomputes the desired pieces.
    ///
    /// All indices are validated before anything is applied, so a bad index
    /// cannot leave a half-updated selection.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`] or [`SessionError::FileIndexOutOfRange`].
    pub async fn set_file_priorities(
        &self,
        id: &TorrentId,
        changes: impl IntoIterator<Item = impl Into<FilePriority>>,
    ) -> Result<TorrentState, SessionError> {
        let id = id.clone();
        let changes: Vec<FilePriority> = changes.into_iter().map(Into::into).collect();
        self.request(|reply| Command::SetFilePriorities { id, changes, reply })
            .await
    }

    /// Lists every torrent in add order.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] if the actor is gone.
    pub async fn list(&self) -> Result<Vec<TorrentState>, SessionError> {
        self.request(|reply| Command::List { reply }).await
    }

    /// Returns one torrent.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`].
    pub async fn get(&self, id: &TorrentId) -> Result<TorrentState, SessionError> {
        let id = id.clone();
        self.request(|reply| Command::Get { id, reply }).await
    }

    /// Returns the desired and verified piece bitfields.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`].
    pub async fn piece_state(&self, id: &TorrentId) -> Result<PieceStateView, SessionError> {
        let id = id.clone();
        self.request(|reply| Command::GetPieceState { id, reply })
            .await
    }

    /// Re-runs verification for a torrent against the bytes now on disk.
    ///
    /// The verified-piece set is cleared first, so a reader never sees old results
    /// mixed with new ones.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`] or [`SessionError::AlreadyChecking`].
    pub async fn recheck(&self, id: &TorrentId) -> Result<TorrentState, SessionError> {
        let id = id.clone();
        self.request(|reply| Command::Recheck { id, reply }).await
    }

    /// Stops the actor and waits for it to finish. No files are deleted.
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] if the actor already stopped.
    pub async fn shutdown(self) -> Result<(), SessionError> {
        self.request(|reply| Command::Shutdown { reply }).await
    }

    /// Moves the streaming playhead, in pieces.
    ///
    /// It is remembered and re-applied when a swarm (re)starts.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`] or [`SessionError::Closed`].
    pub async fn set_playhead(&self, id: &TorrentId, piece: usize) -> Result<(), SessionError> {
        let id = id.clone();
        self.request(|reply| Command::SetPlayhead { id, piece, reply })
            .await
    }

    /// Changes the piece-selection policy for one torrent.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`] or [`SessionError::Closed`].
    pub async fn set_mode(&self, id: &TorrentId, mode: Mode) -> Result<(), SessionError> {
        let id = id.clone();
        self.request(|reply| Command::SetMode { id, mode, reply })
            .await
    }

    /// Returns the file/piece layout the streaming layer needs.
    ///
    /// # Errors
    ///
    /// [`SessionError::UnknownTorrent`] or [`SessionError::Closed`].
    pub async fn stream_layout(&self, id: &TorrentId) -> Result<StreamLayout, SessionError> {
        let id = id.clone();
        self.request(|reply| Command::GetStreamLayout { id, reply })
            .await
    }

    async fn request<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, SessionError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(make(reply))
            .await
            .map_err(|_| SessionError::Closed)?;
        response.await.map_err(|_| SessionError::ActorStopped)?
    }
}

/// A session failure, as reported to a caller.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// No torrent with that id.
    #[error("unknown torrent `{id}`")]
    UnknownTorrent {
        /// The requested id.
        id: String,
    },
    /// The torrent is already in the session.
    #[error("torrent `{id}` is already in this session")]
    Duplicate {
        /// The duplicated id.
        id: String,
    },
    /// A verification pass is already running for that torrent.
    #[error("torrent `{id}` is already being checked")]
    AlreadyChecking {
        /// The requested id.
        id: String,
    },
    /// A file index did not exist.
    #[error("file index {index} is out of range for a {files}-file torrent")]
    FileIndexOutOfRange {
        /// The offending index.
        index: usize,
        /// Files in the torrent.
        files: usize,
    },
    /// The command channel is closed (the actor is gone).
    #[error("session has shut down")]
    Closed,
    /// The torrent's download root names something that is not a directory.
    #[error("download root `{}` exists but is not a directory", .path.display())]
    RootNotDirectory {
        /// The rejected root.
        path: PathBuf,
    },
    /// The actor stopped before answering.
    #[error("session actor stopped before replying")]
    ActorStopped,
    /// A filesystem operation failed.
    #[error("i/o error on `{}`: {source}", .path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The torrent file could not be read.
    #[error("cannot read torrent `{}`: {source}", .path.display())]
    ReadTorrent {
        /// The path that was read.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// A torrent could not be added.
    #[error(transparent)]
    Add(#[from] AddError),
    /// The configuration was rejected.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    /// Session state could not be loaded or saved.
    #[error(transparent)]
    State(#[from] StateError),
    /// A magnet URI was malformed.
    #[error("invalid magnet link: {0}")]
    MagnetLink(String),
    /// Metadata could not be fetched for a magnet link.
    #[error(transparent)]
    Magnet(#[from] crate::magnet::MagnetError),
    /// A blocking task was cancelled or panicked.
    #[error("session task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    /// An invariant that should be impossible was violated.
    #[error("internal session error: {0}")]
    Internal(&'static str),
}

/// Which queue slot a torrent holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// No slot: paused, still being prepared, or waiting.
    None,
    /// Holds one of `max_active_downloads`.
    Download,
    /// Holds one of `max_active_seeds`.
    Seed,
}

/// Where a torrent is in its storage lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Files exist; verification has not run.
    Preallocated,
    /// Hashing now.
    Hashing,
    /// Hashing finished (or was not possible).
    Verified,
}

struct Torrent {
    seq: u64,
    /// Resolved download root: the only place this torrent's bytes may live.
    root: PathBuf,
    plan: Arc<TorrentPlan>,
    priorities: Vec<Priority>,
    desired: PieceBitfield,
    have: PieceBitfield,
    corrupt: PieceBitfield,
    phase: Phase,
    verification: Verification,
    paused: bool,
    slot: Slot,
    status: Status,
    checked: usize,
    last_error: Option<String>,
    swarm: Option<SwarmHandle>,
    swarm_epoch: u64,
    peers: usize,
    downloaded_bytes: u64,
    uploaded_bytes: u64,
    down_rate: f64,
    up_rate: f64,
    last_stats: Option<(std::time::Instant, u64, u64)>,
    playhead: usize,
    mode: Mode,
}

impl Torrent {
    fn new(seq: u64, plan: Arc<TorrentPlan>, root: PathBuf) -> Self {
        let priorities = vec![Priority::Normal; plan.files().len()];
        // The priority count is built to match, and the piece limit was checked
        // while planning; an empty set is the safe fallback either way.
        let desired = pieces::desired_pieces(plan.layout(), &priorities)
            .unwrap_or_else(|_| PieceBitfield::new(plan.piece_count()));
        let have = PieceBitfield::new(plan.piece_count());
        let corrupt = PieceBitfield::new(plan.piece_count());
        let mut torrent = Self {
            seq,
            root,
            plan,
            priorities,
            desired,
            have,
            corrupt,
            phase: Phase::Preallocated,
            verification: Verification::Pending,
            paused: false,
            slot: Slot::None,
            status: Status::Preallocated,
            checked: 0,
            last_error: None,
            swarm: None,
            swarm_epoch: 0,
            peers: 0,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            down_rate: 0.0,
            up_rate: 0.0,
            last_stats: None,
            playhead: 0,
            mode: Mode::Sequential,
        };
        torrent.status = torrent.status();
        torrent
    }

    fn complete(&self) -> bool {
        self.phase == Phase::Verified
            && (0..self.desired.len()).all(|piece| {
                !self.desired.get(piece).unwrap_or(false) || self.have.get(piece).unwrap_or(false)
            })
    }

    fn status(&self) -> Status {
        if self.paused {
            return Status::Paused;
        }
        match self.phase {
            Phase::Preallocated => Status::Preallocated,
            Phase::Hashing => Status::Checking,
            Phase::Verified => {
                if self.complete() {
                    if self.slot == Slot::Seed {
                        Status::Seeding
                    } else {
                        Status::Ready
                    }
                } else if self.slot == Slot::Download {
                    Status::Active
                } else {
                    Status::Queued
                }
            }
        }
    }

    fn recompute_desired(&mut self) {
        match pieces::desired_pieces(self.plan.layout(), &self.priorities) {
            Ok(bits) => self.desired = bits,
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn snapshot(&self, queue_position: Option<usize>) -> TorrentState {
        let layout = self.plan.layout();
        let coverage = pieces::coverage_per_file(layout, &self.have);
        let files = self
            .plan
            .files()
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let priority = self
                    .priorities
                    .get(index)
                    .copied()
                    .unwrap_or(Priority::Skip);
                let bytes_completed = coverage.get(index).copied().unwrap_or(0);
                FileState {
                    index,
                    path: display_path(&file.relative),
                    length: file.length,
                    offset: file.offset,
                    priority,
                    selected: priority.is_selected(),
                    completed: bytes_completed >= file.length,
                    bytes_completed: bytes_completed.min(file.length),
                }
            })
            .collect();

        TorrentState {
            id: self.plan.id().clone(),
            name: self.plan.name().to_owned(),
            private: self.plan.private(),
            kind: self.plan.metainfo().kind(),
            root: self.root.clone(),
            status: self.status(),
            verification: self.verification,
            total_length: self.plan.total_length(),
            payload_length: self.plan.payload_length(),
            piece_length: self.plan.piece_length(),
            piece_count: self.plan.piece_count(),
            pieces_have: self.have.count_ones(),
            pieces_corrupt: self.corrupt.count_ones(),
            pieces_desired: self.desired.count_ones(),
            bytes_have: pieces::bytes_in(layout, &self.have),
            bytes_desired: pieces::bytes_in(layout, &self.desired),
            peers: self.peers,
            downloaded_bytes: self.downloaded_bytes,
            uploaded_bytes: self.uploaded_bytes,
            download_rate: self.down_rate as u64,
            upload_rate: self.up_rate as u64,
            active: self.slot != Slot::None,
            complete: self.complete(),
            queue_position,
            last_error: self.last_error.clone(),
            files,
        }
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Exponential smoothing for the rate shown in the UI.
fn smooth(previous: f64, sample: f64) -> f64 {
    if previous <= 0.0 {
        sample
    } else {
        previous * 0.6 + sample * 0.4
    }
}

#[derive(Debug, Clone)]
struct ActiveCheck {
    id: TorrentId,
    epoch: u64,
}

/// The command surface. Every variant carries its own reply channel, so the actor
/// never has to guess who is waiting.
enum Command {
    AddTorrent {
        bytes: Vec<u8>,
        /// `None` keeps the session's configured download directory.
        root: Option<PathBuf>,
        reply: Reply<TorrentState>,
    },
    Remove {
        id: TorrentId,
        delete_files: bool,
        reply: Reply<()>,
    },
    Pause {
        id: TorrentId,
        reply: Reply<TorrentState>,
    },
    Resume {
        id: TorrentId,
        reply: Reply<TorrentState>,
    },
    SetFilePriorities {
        id: TorrentId,
        changes: Vec<FilePriority>,
        reply: Reply<TorrentState>,
    },
    List {
        reply: Reply<Vec<TorrentState>>,
    },
    Get {
        id: TorrentId,
        reply: Reply<TorrentState>,
    },
    GetPieceState {
        id: TorrentId,
        reply: Reply<PieceStateView>,
    },
    Recheck {
        id: TorrentId,
        reply: Reply<TorrentState>,
    },
    /// Dial a peer for one torrent (manual peers, DHT results, tests).
    AddPeer {
        id: TorrentId,
        addr: std::net::SocketAddr,
        reply: Reply<bool>,
    },
    SetPlayhead {
        id: TorrentId,
        piece: usize,
        reply: Reply<()>,
    },
    SetMode {
        id: TorrentId,
        mode: Mode,
        reply: Reply<()>,
    },
    GetStreamLayout {
        id: TorrentId,
        reply: Reply<StreamLayout>,
    },
    Shutdown {
        reply: Reply<()>,
    },
}

/// Messages the session sends to itself from the preparation and verification
/// tasks. Boxed payloads keep the variant sizes close together.
enum Internal {
    Prepared {
        seq: u64,
        /// The root preparation actually used, so registration cannot drift from
        /// the directory the files were created in.
        root: PathBuf,
        prepared: Box<Result<TorrentPlan, SessionError>>,
        reply: Reply<TorrentState>,
    },
    CheckUpdate {
        id: TorrentId,
        epoch: u64,
        update: CheckUpdate,
    },
    CheckDone {
        id: TorrentId,
        epoch: u64,
        result: Box<Result<CheckRun, flux_disk::DiskError>>,
    },
    /// One event from a torrent's swarm. The epoch makes events from a replaced
    /// swarm harmless.
    Swarm {
        id: TorrentId,
        epoch: u64,
        event: SwarmEvent,
    },
    /// An accepted connection whose handshake named `hash`. `prefix` holds the
    /// 68 handshake bytes so the transport does not read them twice.
    Inbound {
        hash: [u8; 20],
        prefix: Vec<u8>,
        stream: TcpStream,
    },
    /// Peers discovered through the DHT.
    DhtPeers {
        hash: [u8; 20],
        peers: Vec<std::net::SocketAddr>,
    },
}

struct Session {
    config: Arc<Config>,
    torrents: HashMap<TorrentId, Torrent>,
    next_seq: u64,
    command_rx: mpsc::Receiver<Command>,
    internal_rx: mpsc::Receiver<Internal>,
    internal: mpsc::Sender<Internal>,
    events: broadcast::Sender<Event>,
    active_check: Option<ActiveCheck>,
    next_check_epoch: u64,
    peer_id: [u8; 20],
    listen_port: u16,
    listener: Option<JoinHandle<()>>,
    store: Option<StateStore>,
    restore: HashMap<TorrentId, TorrentRecord>,
    restore_order: Vec<TorrentId>,
    persist_pending: bool,
    save_task: Option<JoinHandle<()>>,
    dht: Option<DhtHandle>,
    dht_tick: tokio::time::Interval,
}

/// Binds the first free port at or after `preferred`, falling back to an
/// ephemeral one. Returns the listener and the port it actually used.
async fn bind_listener(preferred: u16) -> io::Result<(TcpListener, u16)> {
    for offset in 0..PORT_ATTEMPTS {
        let port = preferred.saturating_add(offset);
        if port == 0 {
            break;
        }
        if let Ok(listener) = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await {
            let bound = listener.local_addr()?.port();
            return Ok((listener, bound));
        }
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).await?;
    let bound = listener.local_addr()?.port();
    Ok((listener, bound))
}

/// Accepts connections, reads the handshake to learn the torrent, then hands the
/// stream to the actor for routing.
fn spawn_accept_loop(listener: TcpListener, internal: mpsc::Sender<Internal>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let accepted = listener.accept().await;
            let (stream, _addr) = match accepted {
                Ok(pair) => pair,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let internal = internal.clone();
            tokio::spawn(async move {
                let mut stream = stream;
                let mut prefix = [0u8; HANDSHAKE_LEN];
                let read = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.read_exact(&mut prefix));
                if !matches!(read.await, Ok(Ok(_))) {
                    return;
                }
                let Ok(handshake) = Handshake::decode(&prefix) else {
                    return;
                };
                let hash = *handshake.info_hash();
                let _ = internal
                    .send(Internal::Inbound {
                        hash,
                        prefix: prefix.to_vec(),
                        stream,
                    })
                    .await;
            });
        }
    })
}

/// A `-OF0100-` peer id: 8 constant bytes plus 12 random ones.
fn generate_peer_id() -> [u8; 20] {
    let mut id = [0u8; 20];
    id[..8].copy_from_slice(b"-OF0100-");
    for byte in &mut id[8..] {
        *byte = fastrand::u8(..);
    }
    id
}

/// Starts the mainline DHT, or returns `None` when it cannot bind or has no
/// usable bootstrap routers.
///
/// Bootstrap hostnames are resolved with a short timeout; a failure is not fatal
/// because the tracker path still works.
async fn start_dht(
    config: &Config,
    internal: mpsc::Sender<Internal>,
    events: broadcast::Sender<Event>,
) -> Option<DhtHandle> {
    let mut bootstrap = Vec::new();
    for host in &config.dht_bootstrap {
        let resolved = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host(host.as_str()),
        )
        .await;
        match resolved {
            Ok(Ok(mut addrs)) => {
                if let Some(addr) = addrs.find(std::net::SocketAddr::is_ipv4) {
                    bootstrap.push(addr);
                }
            }
            _ => continue,
        }
    }

    let dht_config = DhtConfig {
        bind: SocketAddr::from(([0, 0, 0, 0], config.listen_port)),
        bootstrap,
        ..DhtConfig::default()
    };
    match dht::start(dht_config).await {
        Ok((handle, mut dht_events)) => {
            tokio::spawn(async move {
                while let Some(event) = dht_events.recv().await {
                    match event {
                        dht::DhtEvent::Peers { info_hash, peers } => {
                            let message = Internal::DhtPeers {
                                hash: info_hash,
                                peers,
                            };
                            if internal.send(message).await.is_err() {
                                break;
                            }
                        }
                        dht::DhtEvent::Failed(reason) => {
                            let _ = events.send(Event::Warning(format!("dht: {reason}")));
                        }
                        dht::DhtEvent::NodeAdded(_) => {}
                    }
                }
            });
            Some(handle)
        }
        Err(error) => {
            let _ = events.send(Event::Warning(format!("dht unavailable: {error}")));
            None
        }
    }
}

/// Rejects a download root that exists but cannot hold torrent files.
///
/// A missing path is accepted because preparation creates it. Only a path that
/// already names a file (or a device) is a caller error, and it is reported as
/// such instead of surfacing later as a preallocation failure on a path that
/// looks like the culprit.
async fn check_root(root: &Path) -> Result<(), SessionError> {
    let path = root.to_path_buf();
    tokio::task::spawn_blocking(move || match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(SessionError::RootNotDirectory { path }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SessionError::Io { path, source }),
    })
    .await?
}

impl Session {
    async fn run(mut self) {
        let order = self.restore_order.clone();
        for id in order {
            if let Some(record) = self.restore.get(&id) {
                let bytes = record.metainfo.clone();
                // A record without a root was added into the session directory;
                // one with a root must replay into exactly that directory, or the
                // bytes it already owns would be orphaned.
                let root = record.download_dir.as_ref().map(PathBuf::from);
                let (reply, _dropped) = oneshot::channel();
                self.begin_add(bytes, root, reply);
            }
        }

        loop {
            let message = tokio::select! {
                command = self.command_rx.recv() => Message::Command(command),
                internal = self.internal_rx.recv() => Message::Internal(internal),
                _ = self.dht_tick.tick(), if self.dht.is_some() => Message::Dht,
            };
            let stop = match message {
                Message::Command(Some(command)) => self.handle_command(command).await,
                Message::Internal(Some(internal)) => {
                    self.handle_internal(internal);
                    false
                }
                Message::Dht => {
                    self.poll_dht();
                    false
                }
                // Every handle dropped, or no internal sender is left.
                Message::Command(None) | Message::Internal(None) => true,
            };
            self.flush_persist();
            if stop {
                break;
            }
        }
        if let Some(listener) = self.listener.take() {
            listener.abort();
        }
        let handles: Vec<SwarmHandle> = self
            .torrents
            .values_mut()
            .filter_map(|torrent| torrent.swarm.take())
            .collect();
        for handle in handles {
            handle.shutdown().await;
        }
        let _ = self.events.send(Event::Shutdown);
    }

    async fn handle_command(&mut self, command: Command) -> bool {
        match command {
            Command::AddTorrent { bytes, root, reply } => {
                self.begin_add(bytes, root, reply);
                false
            }
            Command::Remove {
                id,
                delete_files,
                reply,
            } => {
                self.remove(id, delete_files, reply);
                false
            }
            Command::Pause { id, reply } => {
                let result = self.set_paused(&id, true);
                let _ = reply.send(result);
                false
            }
            Command::Resume { id, reply } => {
                let result = self.set_paused(&id, false);
                let _ = reply.send(result);
                false
            }
            Command::SetFilePriorities { id, changes, reply } => {
                let result = self.set_priorities(&id, &changes);
                let _ = reply.send(result);
                false
            }
            Command::List { reply } => {
                // One ordering pass for the whole list, so `List` stays linear in
                // the number of torrents instead of quadratic.
                let ids = self.ids_by_seq();
                let waiting: Vec<&TorrentId> = ids
                    .iter()
                    .filter(|id| {
                        self.torrents
                            .get(*id)
                            .is_some_and(|torrent| torrent.status().is_waiting())
                    })
                    .collect();
                let list = ids
                    .iter()
                    .filter_map(|id| {
                        let position = waiting
                            .iter()
                            .position(|other| *other == id)
                            .map(|index| index + 1);
                        self.torrents
                            .get(id)
                            .map(|torrent| torrent.snapshot(position))
                    })
                    .collect();
                let _ = reply.send(Ok(list));
                false
            }
            Command::Get { id, reply } => {
                let result = self
                    .state_of(&id)
                    .ok_or_else(|| SessionError::UnknownTorrent { id: id.to_string() });
                let _ = reply.send(result);
                false
            }
            Command::GetPieceState { id, reply } => {
                let result = match self.torrents.get(&id) {
                    Some(torrent) => Ok(PieceStateView {
                        desired: torrent.desired.clone(),
                        have: torrent.have.clone(),
                    }),
                    None => Err(SessionError::UnknownTorrent { id: id.to_string() }),
                };
                let _ = reply.send(result);
                false
            }
            Command::Recheck { id, reply } => {
                let result = self.recheck(&id);
                let _ = reply.send(result);
                false
            }
            Command::AddPeer { id, addr, reply } => {
                let handle = self
                    .torrents
                    .get(&id)
                    .and_then(|torrent| torrent.swarm.clone());
                let Some(handle) = handle else {
                    let _ = reply.send(Ok(false));
                    return false;
                };
                tokio::spawn(async move {
                    let added = handle.add_peer(addr).await;
                    let _ = reply.send(Ok(added));
                });
                false
            }
            Command::SetPlayhead { id, piece, reply } => {
                let result = self.set_playhead(&id, piece);
                let _ = reply.send(result);
                false
            }
            Command::SetMode { id, mode, reply } => {
                let result = self.set_mode(&id, mode);
                let _ = reply.send(result);
                false
            }
            Command::GetStreamLayout { id, reply } => {
                let result = self.stream_layout(&id);
                let _ = reply.send(result);
                false
            }
            Command::Shutdown { reply } => {
                let handles: Vec<SwarmHandle> = self
                    .torrents
                    .values_mut()
                    .filter_map(|torrent| torrent.swarm.take())
                    .collect();
                for handle in handles {
                    handle.shutdown().await;
                }
                // Always write on shutdown, never only when something is
                // pending: a save started by the latest tick may still be
                // writing an older snapshot, and `persist_now` is what waits
                // for it before the final state lands.
                self.persist_now().await;
                let _ = reply.send(Ok(()));
                true
            }
        }
    }

    fn handle_internal(&mut self, internal: Internal) {
        match internal {
            Internal::Prepared {
                seq,
                root,
                prepared,
                reply,
            } => self.finish_add(seq, root, *prepared, reply),
            Internal::CheckUpdate { id, epoch, update } => {
                self.on_check_update(&id, epoch, update);
            }
            Internal::CheckDone { id, epoch, result } => {
                self.on_check_done(&id, epoch, *result);
            }
            Internal::Swarm { id, epoch, event } => {
                self.on_swarm(&id, epoch, event);
            }
            Internal::Inbound {
                hash,
                prefix,
                stream,
            } => {
                self.on_inbound(hash, prefix, stream);
            }
            Internal::DhtPeers { hash, peers } => {
                self.on_dht_peers(hash, peers);
            }
        }
    }

    fn begin_add(&mut self, bytes: Vec<u8>, root: Option<PathBuf>, reply: Reply<TorrentState>) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let root = root.unwrap_or_else(|| self.config.download_dir.clone());
        let options = verify::PrepareOptions {
            root: root.clone(),
            allocation: self.config.preallocate.allocation(),
        };
        let internal = self.internal.clone();
        tokio::spawn(async move {
            // A missing root is fine because preparation creates it; one that
            // already names a file would only fail deep inside preallocation, where
            // the error no longer points at the argument the caller passed.
            let prepared = match check_root(&root).await {
                Ok(()) => verify::prepare(bytes, options)
                    .await
                    .map_err(SessionError::Add),
                Err(error) => Err(error),
            };
            let _ = internal
                .send(Internal::Prepared {
                    seq,
                    root,
                    prepared: Box::new(prepared),
                    reply,
                })
                .await;
        });
    }

    fn finish_add(
        &mut self,
        seq: u64,
        root: PathBuf,
        prepared: Result<TorrentPlan, SessionError>,
        reply: Reply<TorrentState>,
    ) {
        let plan = match prepared {
            Ok(plan) => Arc::new(plan),
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let id = plan.id().clone();
        if self.torrents.contains_key(&id) {
            let _ = reply.send(Err(SessionError::Duplicate {
                id: id.into_string(),
            }));
            return;
        }

        let mut torrent = Torrent::new(seq, plan, root);
        if !torrent.plan.verification_supported() {
            // v2-only: there are no SHA-1 hashes to check, so the torrent is
            // storage-ready but unverified. `Verification::Unsupported` says so.
            torrent.phase = Phase::Verified;
            torrent.verification = Verification::Unsupported;
        }
        self.torrents.insert(id.clone(), torrent);
        if let Some(record) = self.restore.remove(&id)
            && let Some(torrent) = self.torrents.get_mut(&id)
        {
            for (index, byte) in record.priorities.iter().enumerate() {
                if let Some(slot) = torrent.priorities.get_mut(index) {
                    *slot = match byte {
                        0 => Priority::Skip,
                        2 => Priority::High,
                        _ => Priority::Normal,
                    };
                }
            }
            torrent.paused = record.paused;
            torrent.recompute_desired();
        }
        self.mark_dirty();
        self.refresh();

        match self.state_of(&id) {
            Some(state) => {
                self.emit(Event::Added {
                    torrent: Box::new(state.clone()),
                });
                let _ = reply.send(Ok(state));
            }
            None => {
                let _ = reply.send(Err(SessionError::Internal(
                    "torrent vanished during registration",
                )));
            }
        }
    }

    fn remove(&mut self, id: TorrentId, delete_files: bool, reply: Reply<()>) {
        if !self.torrents.contains_key(&id) {
            let _ = reply.send(Err(SessionError::UnknownTorrent {
                id: id.into_string(),
            }));
            return;
        }
        self.stop_swarm(&id);
        let Some(torrent) = self.torrents.remove(&id) else {
            let _ = reply.send(Err(SessionError::UnknownTorrent {
                id: id.into_string(),
            }));
            return;
        };
        // The verification task cannot be cancelled, but its messages carry an
        // epoch and a torrent id that is no longer registered, so they are
        // ignored and a new pass may start immediately.
        if self
            .active_check
            .as_ref()
            .is_some_and(|check| check.id == id)
        {
            self.active_check = None;
        }
        self.refresh();
        self.emit(Event::Removed { id: id.clone() });
        self.mark_dirty();

        if !delete_files {
            let _ = reply.send(Ok(()));
            return;
        }
        let root = torrent.root.clone();
        let plan = Arc::clone(&torrent.plan);
        tokio::spawn(async move {
            let result =
                tokio::task::spawn_blocking(move || verify::delete_files(plan.as_ref(), &root))
                    .await;
            let outcome = match result {
                Ok(inner) => inner.map_err(SessionError::Add),
                Err(error) => Err(SessionError::Add(AddError::Join(error))),
            };
            let _ = reply.send(outcome);
        });
    }

    fn set_paused(&mut self, id: &TorrentId, paused: bool) -> Result<TorrentState, SessionError> {
        let Some(torrent) = self.torrents.get_mut(id) else {
            return Err(SessionError::UnknownTorrent { id: id.to_string() });
        };
        if torrent.paused != paused {
            torrent.paused = paused;
            if paused {
                torrent.slot = Slot::None;
            }
        }
        self.refresh();
        self.mark_dirty();
        self.state_of(id)
            .ok_or(SessionError::Internal("torrent vanished"))
    }

    fn set_priorities(
        &mut self,
        id: &TorrentId,
        changes: &[FilePriority],
    ) -> Result<TorrentState, SessionError> {
        let files = match self.torrents.get(id) {
            Some(torrent) => torrent.priorities.len(),
            None => return Err(SessionError::UnknownTorrent { id: id.to_string() }),
        };
        // Validate everything first so a bad index cannot leave a partial update.
        if let Some(bad) = changes.iter().find(|change| change.index >= files) {
            return Err(SessionError::FileIndexOutOfRange {
                index: bad.index,
                files,
            });
        }
        let Some(torrent) = self.torrents.get_mut(id) else {
            return Err(SessionError::Internal("torrent vanished"));
        };
        for change in changes {
            if let Some(slot) = torrent.priorities.get_mut(change.index) {
                *slot = change.priority;
            }
        }
        torrent.recompute_desired();
        let desired = torrent.desired.clone();
        let swarm = torrent.swarm.clone();
        if let Some(handle) = swarm {
            tokio::spawn(async move {
                let _ = handle.set_desired(desired).await;
            });
        }
        self.refresh();
        self.mark_dirty();
        self.emit(Event::Updated { id: id.clone() });
        self.state_of(id)
            .ok_or(SessionError::Internal("torrent vanished"))
    }

    fn recheck(&mut self, id: &TorrentId) -> Result<TorrentState, SessionError> {
        if self
            .active_check
            .as_ref()
            .is_some_and(|check| check.id == *id)
        {
            return Err(SessionError::AlreadyChecking { id: id.to_string() });
        }
        self.stop_swarm(id);
        let Some(torrent) = self.torrents.get_mut(id) else {
            return Err(SessionError::UnknownTorrent { id: id.to_string() });
        };
        torrent.have.clear();
        torrent.corrupt.clear();
        torrent.checked = 0;
        torrent.last_error = None;
        torrent.slot = Slot::None;
        if torrent.plan.verification_supported() {
            torrent.phase = Phase::Preallocated;
            torrent.verification = Verification::Pending;
        } else {
            torrent.phase = Phase::Verified;
            torrent.verification = Verification::Unsupported;
        }
        self.refresh();
        self.state_of(id)
            .ok_or(SessionError::Internal("torrent vanished"))
    }

    /// Marks the snapshot as needing a rewrite.
    fn mark_dirty(&mut self) {
        if self.store.is_some() {
            self.persist_pending = true;
        }
    }

    /// Writes the snapshot once in the background, coalescing bursts.
    fn flush_persist(&mut self) {
        // A finished write is observed through its handle, so the actor never
        // has to be interrupted by a completion message. A write still in
        // flight keeps its slot; `persist_pending` survives until the next tick.
        if self
            .save_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        self.save_task = None;
        if !self.persist_pending {
            return;
        }
        let Some(store) = self.store.clone() else {
            self.persist_pending = false;
            return;
        };
        let snapshot = self.to_snapshot();
        self.persist_pending = false;
        let events = self.events.clone();
        self.save_task = Some(tokio::spawn(async move {
            if let Err(error) = store.save_async(&snapshot).await {
                let _ = events.send(Event::Warning(format!(
                    "session state could not be saved: {error}"
                )));
            }
        }));
    }

    /// Writes the snapshot synchronously (shutdown path).
    async fn persist_now(&mut self) {
        // A background save may still hold an older snapshot. Await it first,
        // otherwise it could land after this write and resurrect stale state
        // (for example "paused" flipping back to "running" across a restart).
        // The task only talks to the broadcast event channel, which never
        // blocks, so awaiting it here cannot deadlock the actor.
        if let Some(task) = self.save_task.take() {
            let _ = task.await;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        let snapshot = self.to_snapshot();
        self.persist_pending = false;
        if let Err(error) = store.save_async(&snapshot).await {
            let _ = self.events.send(Event::Warning(format!(
                "session state could not be saved: {error}"
            )));
        }
    }

    fn to_snapshot(&self) -> SessionSnapshot {
        let mut torrents = Vec::new();
        for id in self.ids_by_seq() {
            let Some(torrent) = self.torrents.get(&id) else {
                continue;
            };
            let priorities = torrent
                .priorities
                .iter()
                .map(|priority| match priority {
                    Priority::Skip => 0,
                    Priority::Normal => 1,
                    Priority::High => 2,
                })
                .collect();
            torrents.push(TorrentRecord {
                info_hash: torrent.plan.id().as_str().to_owned(),
                metainfo: torrent.plan.raw_metainfo().to_vec(),
                priorities,
                paused: torrent.paused,
                // The session directory is implied by where the snapshot lives, so
                // only a torrent that lives elsewhere has to name its root. This
                // keeps the state file of a default session byte-identical to what
                // earlier builds wrote.
                download_dir: (torrent.root != self.config.download_dir)
                    .then(|| torrent.root.to_string_lossy().into_owned()),
            });
        }
        SessionSnapshot {
            version: STATE_VERSION,
            torrents,
        }
    }

    /// Re-evaluates slots, statuses, the verification queue and the swarms.
    fn refresh(&mut self) {
        self.assign_slots();
        self.sync_status();
        self.pump_check();
        self.pump_swarms();
    }

    /// Starts swarms for verified torrents that hold a slot and stops the rest.
    fn pump_swarms(&mut self) {
        let ids = self.ids_by_seq();
        for id in ids {
            let wanted = self.torrents.get(&id).is_some_and(|torrent| {
                torrent.phase == Phase::Verified
                    && !torrent.paused
                    && torrent.slot != Slot::None
                    && torrent.plan.verification_supported()
            });
            let running = self
                .torrents
                .get(&id)
                .is_some_and(|torrent| torrent.swarm.is_some());
            if wanted && !running {
                self.start_swarm(&id);
            } else if !wanted && running {
                self.stop_swarm(&id);
            }
        }
    }

    fn start_swarm(&mut self, id: &TorrentId) {
        let internal = self.internal.clone();
        let peer_id = self.peer_id;
        let listen_port = self.listen_port;
        let max_peers = self.config.max_peers_per_torrent;
        let max_upload_slots = self.config.max_upload_slots;
        let verifier_threads = self.config.verifier_threads;

        let Some(torrent) = self.torrents.get_mut(id) else {
            return;
        };
        if torrent.swarm.is_some() || !torrent.plan.verification_supported() {
            return;
        }
        // With an ephemeral port there is still a real port to announce.
        let announce =
            AnnounceSpec::from_metainfo(torrent.plan.metainfo(), listen_port, peer_id).ok();
        let config = SwarmConfig {
            plan: Arc::clone(&torrent.plan),
            root: torrent.root.clone(),
            peer_id,
            listen_port,
            initial_have: torrent.have.clone(),
            desired: torrent.desired.clone(),
            max_peers,
            max_upload_slots,
            announce,
            verifier_threads,
        };
        let (events, mut event_rx) = mpsc::channel(SWARM_EVENT_CAPACITY);
        match swarm::spawn(config, events) {
            Ok(handle) => {
                torrent.swarm = Some(handle);
                torrent.swarm_epoch = torrent.swarm_epoch.wrapping_add(1);
                torrent.peers = 0;
                torrent.downloaded_bytes = 0;
                torrent.uploaded_bytes = 0;
                torrent.down_rate = 0.0;
                torrent.up_rate = 0.0;
                torrent.last_stats = None;
                let epoch = torrent.swarm_epoch;
                let playhead = torrent.playhead;
                let mode = torrent.mode;
                if let Some(handle) = torrent.swarm.clone() {
                    tokio::spawn(async move {
                        handle.set_playhead(playhead).await;
                        handle.set_mode(mode).await;
                    });
                }
                let id = id.clone();
                tokio::spawn(async move {
                    while let Some(event) = event_rx.recv().await {
                        let message = Internal::Swarm {
                            id: id.clone(),
                            epoch,
                            event,
                        };
                        if internal.send(message).await.is_err() {
                            break;
                        }
                    }
                });
            }
            Err(error) => {
                torrent.last_error = Some(error.to_string());
            }
        }
    }

    fn set_playhead(&mut self, id: &TorrentId, piece: usize) -> Result<(), SessionError> {
        let Some(torrent) = self.torrents.get_mut(id) else {
            return Err(SessionError::UnknownTorrent { id: id.to_string() });
        };
        torrent.playhead = piece.min(torrent.plan.piece_count().saturating_sub(1));
        let playhead = torrent.playhead;
        if let Some(handle) = torrent.swarm.clone() {
            tokio::spawn(async move {
                handle.set_playhead(playhead).await;
            });
        }
        Ok(())
    }

    fn set_mode(&mut self, id: &TorrentId, mode: Mode) -> Result<(), SessionError> {
        let Some(torrent) = self.torrents.get_mut(id) else {
            return Err(SessionError::UnknownTorrent { id: id.to_string() });
        };
        torrent.mode = mode;
        if let Some(handle) = torrent.swarm.clone() {
            tokio::spawn(async move {
                handle.set_mode(mode).await;
            });
        }
        Ok(())
    }

    fn stream_layout(&self, id: &TorrentId) -> Result<StreamLayout, SessionError> {
        let Some(torrent) = self.torrents.get(id) else {
            return Err(SessionError::UnknownTorrent { id: id.to_string() });
        };
        let piece_length = torrent.plan.piece_length();
        let piece_count = torrent.plan.piece_count();
        let piece_length_u64 = u64::from(piece_length).max(1);
        let files = torrent
            .plan
            .files()
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let start = usize::try_from(file.offset / piece_length_u64).unwrap_or(0);
                let end = if file.length == 0 {
                    start
                } else {
                    usize::try_from((file.offset + file.length - 1) / piece_length_u64)
                        .unwrap_or(start)
                        .saturating_add(1)
                };
                StreamFile {
                    index,
                    path: display_path(&file.relative),
                    offset: file.offset,
                    length: file.length,
                    piece_start: start.min(piece_count),
                    piece_end: end.min(piece_count),
                }
            })
            .collect();
        Ok(StreamLayout {
            root: torrent.root.clone(),
            piece_length,
            files,
        })
    }

    fn stop_swarm(&mut self, id: &TorrentId) {
        let Some(torrent) = self.torrents.get_mut(id) else {
            return;
        };
        let Some(handle) = torrent.swarm.take() else {
            return;
        };
        torrent.swarm_epoch = torrent.swarm_epoch.wrapping_add(1);
        torrent.peers = 0;
        torrent.down_rate = 0.0;
        torrent.up_rate = 0.0;
        torrent.last_stats = None;
        tokio::spawn(async move {
            handle.shutdown().await;
        });
    }

    fn on_swarm(&mut self, id: &TorrentId, epoch: u64, event: SwarmEvent) {
        let Some(torrent) = self.torrents.get_mut(id) else {
            return;
        };
        if torrent.swarm_epoch != epoch {
            return;
        }
        let mut failed = false;
        match event {
            SwarmEvent::PieceVerified { piece } => {
                if let Ok(piece) = usize::try_from(piece) {
                    torrent.have.insert(piece);
                }
            }
            SwarmEvent::PieceCorrupt { piece } => {
                if let Ok(piece) = usize::try_from(piece) {
                    torrent.corrupt.insert(piece);
                }
            }
            SwarmEvent::PeerCount(peers) => {
                torrent.peers = peers;
            }
            SwarmEvent::Stats {
                downloaded_bytes,
                uploaded_bytes,
            } => {
                let now = std::time::Instant::now();
                if let Some((at, previous_down, previous_up)) = torrent.last_stats {
                    let elapsed = now.duration_since(at).as_secs_f64();
                    if elapsed > 0.05 {
                        let down = downloaded_bytes.saturating_sub(previous_down) as f64 / elapsed;
                        let up = uploaded_bytes.saturating_sub(previous_up) as f64 / elapsed;
                        torrent.down_rate = smooth(torrent.down_rate, down);
                        torrent.up_rate = smooth(torrent.up_rate, up);
                    }
                }
                torrent.last_stats = Some((now, downloaded_bytes, uploaded_bytes));
                torrent.downloaded_bytes = downloaded_bytes;
                torrent.uploaded_bytes = uploaded_bytes;
            }
            SwarmEvent::Completed => {}
            SwarmEvent::Failed(message) => {
                torrent.last_error = Some(message);
                failed = true;
            }
        }
        self.refresh();
        if failed {
            self.emit(Event::Updated { id: id.clone() });
        }
    }

    /// Routes an accepted connection to the torrent whose v1 hash it named.
    fn on_inbound(&mut self, hash: [u8; 20], prefix: Vec<u8>, stream: TcpStream) {
        let id = self
            .torrents
            .iter()
            .find(|(_, torrent)| torrent.plan.metainfo().info.hashes.v1 == Some(hash))
            .map(|(id, _)| id.clone());
        let Some(id) = id else {
            return;
        };
        let handle = self
            .torrents
            .get(&id)
            .and_then(|torrent| torrent.swarm.clone());
        let Some(handle) = handle else {
            return;
        };
        tokio::spawn(async move {
            let _ = handle.add_inbound_prefixed(stream, prefix).await;
        });
    }

    /// Announces our port and looks up peers for every active, non-private
    /// torrent. Runs off the actor so a slow lookup cannot stall commands.
    fn poll_dht(&mut self) {
        let Some(dht) = self.dht.clone() else {
            return;
        };
        let port = self.listen_port;
        for id in self.ids_by_seq() {
            let Some(torrent) = self.torrents.get(&id) else {
                continue;
            };
            if torrent.plan.private() || torrent.slot == Slot::None {
                continue;
            }
            let Some(hash) = torrent.plan.metainfo().info.hashes.v1 else {
                continue;
            };
            let dht = dht.clone();
            let internal = self.internal.clone();
            tokio::spawn(async move {
                if port != 0 {
                    let _ = dht.announce_peer(hash, port).await;
                }
                let peers = dht.get_peers(hash).await;
                if !peers.is_empty() {
                    let _ = internal.send(Internal::DhtPeers { hash, peers }).await;
                }
            });
        }
    }

    /// Feeds DHT results to the matching swarm. Private torrents are refused
    /// here as well as in [`Session::poll_dht`].
    fn on_dht_peers(&mut self, hash: [u8; 20], peers: Vec<std::net::SocketAddr>) {
        let found = self
            .torrents
            .iter()
            .find(|(_, torrent)| {
                !torrent.plan.private() && torrent.plan.metainfo().info.hashes.v1 == Some(hash)
            })
            .map(|(id, torrent)| (id.clone(), torrent.swarm.clone()));
        let Some((_, Some(handle))) = found else {
            return;
        };
        for addr in peers.into_iter().take(50) {
            let handle = handle.clone();
            tokio::spawn(async move {
                let _ = handle.add_peer(addr).await;
            });
        }
    }

    /// Hands out download and seed slots in add order.
    fn assign_slots(&mut self) {
        let ids = self.ids_by_seq();

        // Count what is currently held by torrents still entitled to it.
        let mut downloads = 0_usize;
        let mut seeds = 0_usize;
        for id in &ids {
            let Some(torrent) = self.torrents.get(id) else {
                continue;
            };
            match torrent.slot {
                Slot::Download if eligible_download(torrent) => downloads += 1,
                Slot::Seed if eligible_seed(torrent) => seeds += 1,
                _ => {}
            }
        }

        let max_downloads = self.config.max_active_downloads;
        let max_seeds = self.config.max_active_seeds;
        for id in &ids {
            let Some(torrent) = self.torrents.get_mut(id) else {
                continue;
            };

            if eligible_seed(torrent) {
                if torrent.slot == Slot::Seed {
                    // already counted
                } else if seeds < max_seeds {
                    torrent.slot = Slot::Seed;
                    seeds += 1;
                } else if torrent.slot == Slot::Download {
                    torrent.slot = Slot::None;
                }
            } else if eligible_download(torrent) {
                if torrent.slot == Slot::Download {
                    // already counted
                } else if downloads < max_downloads {
                    torrent.slot = Slot::Download;
                    downloads += 1;
                } else if torrent.slot == Slot::Seed {
                    torrent.slot = Slot::None;
                }
            } else {
                torrent.slot = Slot::None;
            }
        }
    }

    fn sync_status(&mut self) {
        let ids = self.ids_by_seq();
        let mut changes = Vec::new();
        for id in &ids {
            let Some(torrent) = self.torrents.get_mut(id) else {
                continue;
            };
            let status = torrent.status();
            if status != torrent.status {
                torrent.status = status;
                changes.push((id.clone(), status));
            }
        }
        for (id, status) in changes {
            self.emit(Event::StatusChanged { id, status });
        }
    }

    /// Starts the next verification pass in add order, if none is running.
    fn pump_check(&mut self) {
        if self.active_check.is_some() {
            return;
        }
        let mut candidates: Vec<(u64, TorrentId)> = self
            .torrents
            .iter()
            .filter(|(_, torrent)| {
                torrent.phase == Phase::Preallocated
                    && !torrent.paused
                    && torrent.plan.verification_supported()
            })
            .map(|(id, torrent)| (torrent.seq, id.clone()))
            .collect();
        candidates.sort_by_key(|(seq, _)| *seq);
        if let Some((_, id)) = candidates.into_iter().next() {
            self.start_check(id);
        }
    }

    fn start_check(&mut self, id: TorrentId) {
        let epoch = self.next_check_epoch;
        self.next_check_epoch = self.next_check_epoch.wrapping_add(1);
        let threads = self.config.verifier_threads;

        // Everything needed for the job is collected here so the mutable borrow
        // of the torrent ends before the actor fields are touched again.
        let mut unsupported = false;
        let job = {
            let Some(torrent) = self.torrents.get_mut(&id) else {
                return;
            };
            torrent.phase = Phase::Hashing;
            torrent.verification = Verification::Running;
            torrent.have.clear();
            torrent.corrupt.clear();
            torrent.checked = 0;
            torrent.last_error = None;
            match torrent.plan.piece_hashes() {
                Some(hashes) => Some(CheckJob {
                    layout: torrent.plan.disk_layout(),
                    root: torrent.root.clone(),
                    piece_length: torrent.plan.piece_length(),
                    hashes: hashes.to_vec(),
                    threads,
                }),
                None => {
                    torrent.phase = Phase::Verified;
                    torrent.verification = Verification::Unsupported;
                    unsupported = true;
                    None
                }
            }
        };
        if unsupported {
            self.sync_status();
            return;
        }
        let Some(job) = job else {
            return;
        };

        self.active_check = Some(ActiveCheck {
            id: id.clone(),
            epoch,
        });

        let (updates, mut update_rx) = mpsc::channel(CHECK_CAPACITY);
        let forward = self.internal.clone();
        let forward_id = id.clone();
        tokio::spawn(async move {
            while let Some(update) = update_rx.recv().await {
                let sent = forward
                    .send(Internal::CheckUpdate {
                        id: forward_id.clone(),
                        epoch,
                        update,
                    })
                    .await;
                if sent.is_err() {
                    break;
                }
            }
        });

        let done = self.internal.clone();
        tokio::spawn(async move {
            let result = verify::run_check(job, Some(&updates)).await;
            // Closing the update channel lets the forwarder finish, then the
            // completion message closes the pass.
            drop(updates);
            let _ = done
                .send(Internal::CheckDone {
                    id,
                    epoch,
                    result: Box::new(result),
                })
                .await;
        });

        self.sync_status();
    }

    fn on_check_update(&mut self, id: &TorrentId, epoch: u64, update: CheckUpdate) {
        if !self.check_is_current(id, epoch) {
            return;
        }
        let progress = {
            let Some(torrent) = self.torrents.get_mut(id) else {
                return;
            };
            if torrent.phase != Phase::Hashing {
                return;
            }
            if update.outcome == CheckOutcome::Good
                && let Ok(piece) = usize::try_from(update.piece)
            {
                torrent.have.insert(piece);
            }
            torrent.checked = torrent.checked.saturating_add(1);
            let total = torrent.plan.piece_count();
            let stride = total.div_ceil(PROGRESS_STEPS).max(1);
            if torrent.checked % stride == 0 || torrent.checked >= total {
                Some((torrent.checked.min(total), total))
            } else {
                None
            }
        };
        if let Some((pieces_done, pieces_total)) = progress {
            self.emit(Event::Checked {
                id: id.clone(),
                pieces_done,
                pieces_total,
            });
        }
    }

    fn on_check_done(
        &mut self,
        id: &TorrentId,
        epoch: u64,
        result: Result<CheckRun, flux_disk::DiskError>,
    ) {
        if !self.check_is_current(id, epoch) {
            return;
        }
        self.active_check = None;

        let outcome = {
            let Some(torrent) = self.torrents.get_mut(id) else {
                self.refresh();
                return;
            };
            let total = torrent.plan.piece_count();
            match result {
                Ok(run) => {
                    torrent.have = run.have;
                    torrent.corrupt = run.corrupt;
                    torrent.checked = total;
                    torrent.verification = Verification::Complete;
                    torrent.phase = Phase::Verified;
                    torrent.last_error = run.report.first_error.clone();
                    (run.report.have, run.report.missing())
                }
                Err(error) => {
                    torrent.have.clear();
                    torrent.corrupt.clear();
                    torrent.checked = 0;
                    torrent.verification = Verification::Failed;
                    torrent.phase = Phase::Verified;
                    torrent.last_error = Some(error.to_string());
                    (0, total)
                }
            }
        };

        self.emit(Event::Finished {
            id: id.clone(),
            have: outcome.0,
            missing: outcome.1,
        });
        self.refresh();
    }

    fn check_is_current(&self, id: &TorrentId, epoch: u64) -> bool {
        self.active_check
            .as_ref()
            .is_some_and(|check| check.id == *id && check.epoch == epoch)
    }

    fn ids_by_seq(&self) -> Vec<TorrentId> {
        let mut ids: Vec<TorrentId> = self.torrents.keys().cloned().collect();
        ids.sort_by_key(|id| {
            self.torrents
                .get(id)
                .map_or(u64::MAX, |torrent| torrent.seq)
        });
        ids
    }

    fn state_of(&self, id: &TorrentId) -> Option<TorrentState> {
        let torrent = self.torrents.get(id)?;
        let position = self
            .ids_by_seq()
            .into_iter()
            .filter_map(|other| self.torrents.get(&other))
            .filter(|other| other.status().is_waiting())
            .position(|other| other.plan.id() == id)
            .map(|index| index + 1);
        Some(torrent.snapshot(position))
    }

    fn emit(&self, event: Event) {
        // No subscribers is a normal state for a headless run.
        let _ = self.events.send(event);
    }
}

fn eligible_download(torrent: &Torrent) -> bool {
    !torrent.paused && torrent.phase == Phase::Verified && !torrent.complete()
}

fn eligible_seed(torrent: &Torrent) -> bool {
    !torrent.paused && torrent.phase == Phase::Verified && torrent.complete()
}

enum Message {
    Command(Option<Command>),
    Internal(Option<Internal>),
    /// The periodic trackerless lookup tick.
    Dht,
}
