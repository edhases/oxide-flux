//! Playback.
//!
//! mpv is the player. On Windows it can draw into a `STATIC` child window of
//! the application ([`Player::play_embedded`]); everywhere else, and as a
//! fallback when the child window cannot be created, mpv opens its own window.
//! The UI never pretends that something is playing when it is not.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[cfg(any(windows, test))]
use std::ffi::OsString;

use thiserror::Error;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

/// Rectangle in physical pixels, relative to the parent client area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bounds {
    /// Left edge, in physical pixels.
    pub x: i32,
    /// Top edge, in physical pixels.
    pub y: i32,
    /// Width, in physical pixels.
    pub width: u32,
    /// Height, in physical pixels.
    pub height: u32,
}

impl Bounds {
    /// A rectangle at `x`,`y` with the given size.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The rectangle with a size the platform can use.
    ///
    /// Win32 takes `i32` dimensions and a zero-sized child window is invisible,
    /// so the width and height are clamped to `1..=i32::MAX`. The origin is
    /// left alone: negative coordinates are legal inside a client area.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            width: self.width.clamp(1, i32::MAX.unsigned_abs()),
            height: self.height.clamp(1, i32::MAX.unsigned_abs()),
            ..self
        }
    }
}

/// Why playback could not start.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlayerError {
    /// No usable mpv executable was found.
    #[error("mpv is not installed (set OXIDE_FLUX_MPV to its path)")]
    Unavailable,
    /// mpv was found but refused to start.
    #[error("cannot start mpv: {0}")]
    Spawn(String),
    /// The player could not be embedded and its external fallback failed too.
    #[error("cannot embed playback: {0}")]
    Embed(String),
}

/// One playback session.
#[derive(Debug, Default)]
pub struct Player {
    child: Option<Child>,
    title: Option<String>,
    /// The child window mpv draws into, when playback is embedded.
    #[cfg(windows)]
    embedding: Option<windows::Session>,
}

impl Player {
    /// An idle player.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The file currently being played, if any.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Whether a player process is still running.
    pub fn is_running(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            _ => {
                self.child = None;
                self.title = None;
                #[cfg(windows)]
                {
                    self.embedding = None;
                }
                false
            }
        }
    }

    /// Whether the current playback is embedded in the application window.
    #[must_use]
    pub fn is_embedded(&self) -> bool {
        #[cfg(windows)]
        {
            self.embedding.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Starts playback in an external mpv window.
    ///
    /// # Errors
    ///
    /// [`PlayerError::Unavailable`] when mpv cannot be found,
    /// [`PlayerError::Spawn`] when it exists but fails to start.
    pub fn play(&mut self, path: &Path, title: &str) -> Result<(), PlayerError> {
        self.stop();
        let executable = require_mpv(mpv_executable)?;
        self.spawn_external(&executable, path, title)
    }

    /// Plays `path` inside `parent` at `bounds`.
    ///
    /// `parent` is a Win32 `HWND` as `isize`, and `bounds` are physical pixels
    /// relative to its client area; converting iced's logical layout (and its
    /// scale factor) is the caller's job.
    ///
    /// On Windows this creates a `STATIC` child window and spawns mpv with
    /// `--wid`, so the video is drawn inside the application. Embedding is
    /// best-effort: when the child window cannot be created, or mpv fails to
    /// start against it, playback falls back to an external mpv window and
    /// [`is_embedded`](Self::is_embedded) reports `false`. Outside Windows
    /// there is no embedding and this simply calls [`play`](Self::play).
    ///
    /// Call this first, then [`set_bounds`](Self::set_bounds) on every resize;
    /// `set_bounds` is a no-op while nothing is embedded.
    ///
    /// # Errors
    ///
    /// [`PlayerError::Unavailable`] when mpv cannot be found, and
    /// [`PlayerError::Embed`] when both embedding and its fallback fail.
    pub fn play_embedded(
        &mut self,
        parent: isize,
        bounds: Bounds,
        path: &Path,
        title: &str,
    ) -> Result<(), PlayerError> {
        #[cfg(windows)]
        {
            self.play_in_child_window(parent, bounds, path, title)
        }
        #[cfg(not(windows))]
        {
            let _ = (parent, bounds);
            self.play(path, title)
        }
    }

    /// Repositions/resizes the embedded player window.
    ///
    /// A no-op when playback is external or stopped. `bounds` are physical
    /// pixels relative to the parent's client area.
    pub fn set_bounds(&self, bounds: Bounds) {
        #[cfg(windows)]
        {
            if let Some(session) = &self.embedding {
                session.set_bounds(bounds);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = bounds;
        }
    }

    /// Stops playback, if running, and destroys the embedded window.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            reap(&mut child);
        }
        self.title = None;
        #[cfg(windows)]
        {
            // Dropping the session destroys the child window. This happens
            // after mpv is gone so nothing can paint into a dead HWND.
            self.embedding = None;
        }
    }

    fn spawn_external(
        &mut self,
        executable: &Path,
        path: &Path,
        title: &str,
    ) -> Result<(), PlayerError> {
        let child = Command::new(executable)
            .arg("--force-window=yes")
            .arg("--keep-open=no")
            .arg("--")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| PlayerError::Spawn(error.to_string()))?;
        self.child = Some(child);
        self.title = Some(title.to_owned());
        Ok(())
    }

    #[cfg(windows)]
    fn play_in_child_window(
        &mut self,
        parent: isize,
        bounds: Bounds,
        path: &Path,
        title: &str,
    ) -> Result<(), PlayerError> {
        self.stop();
        let executable = require_mpv(mpv_executable)?;
        match windows::Session::start(parent, bounds, &executable, path) {
            Ok((session, child)) => {
                self.child = Some(child);
                self.embedding = Some(session);
                self.title = Some(title.to_owned());
                Ok(())
            }
            Err(error) => {
                // Embedding is best-effort: a remote session, a missing desktop
                // or an mpv build without `--wid` should still play the file in
                // an ordinary window rather than fail outright. The original
                // reason is kept only when the fallback fails as well.
                match self.play(path, title) {
                    Ok(()) => Ok(()),
                    Err(fallback) => Err(PlayerError::Embed(format!(
                        "{error}; external fallback failed: {fallback}"
                    ))),
                }
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Resolves the mpv executable, or reports that nothing can play.
///
/// There is deliberately no separate fallback for embedding: with no executable
/// there is no player at all, so [`PlayerError::Unavailable`] is the only
/// honest answer. The lookup is injected so tests can prove that without
/// touching the machine's `PATH` or environment.
fn require_mpv(lookup: impl FnOnce() -> Option<PathBuf>) -> Result<PathBuf, PlayerError> {
    lookup().ok_or(PlayerError::Unavailable)
}

/// Terminates `child` and reaps it without hanging the UI thread.
///
/// `kill` is `TerminateProcess` on Windows and `SIGKILL` elsewhere, so the
/// process normally exits at once; the bounded poll only keeps a wedged process
/// from stalling the event loop for long.
fn reap(child: &mut Child) {
    // The process may already have exited; the status is collected below.
    let _ = child.kill();
    for _ in 0..50 {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    let _ = child.wait();
}

fn mpv_executable() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OXIDE_FLUX_MPV") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let names = if cfg!(windows) {
        ["mpv.exe", "mpv"]
    } else {
        ["mpv", "mpv.exe"]
    };
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for name in names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The exact argument list for an embedded mpv launch.
///
/// `--wid` makes mpv render into the given window, `--force-window=no` stops it
/// from creating a window of its own, `--keep-open=no` makes it exit when the
/// file ends, and `--no-terminal` keeps a console from flashing up.
/// `--input-ipc-server` opens a named pipe a future controller can seek and
/// stop through, and `--` ends option parsing so a path starting with `-` is
/// never read as an option.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn mpv_args(hwnd: isize, path: &Path, ipc: &str) -> Vec<OsString> {
    vec![
        OsString::from(format!("--wid={hwnd}")),
        OsString::from("--force-window=no"),
        OsString::from("--keep-open=no"),
        OsString::from("--no-terminal"),
        OsString::from(format!("--input-ipc-server={ipc}")),
        OsString::from("--"),
        path.as_os_str().to_owned(),
    ]
}

/// Whether a file extension is something mpv can usually play.
#[must_use]
pub fn is_media(path: &str) -> bool {
    let Some(extension) = Path::new(path).extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "mkv"
            | "mp4"
            | "m4v"
            | "avi"
            | "mov"
            | "webm"
            | "ts"
            | "m2ts"
            | "mpg"
            | "mpeg"
            | "flv"
            | "wmv"
            | "mp3"
            | "flac"
            | "m4a"
            | "aac"
            | "ogg"
            | "opus"
            | "wav"
            | "wv"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[OsString]) -> Vec<&str> {
        args.iter()
            .filter_map(|arg| arg.to_str())
            .filter(|arg| arg.starts_with("--"))
            .collect()
    }

    #[test]
    fn embedded_arguments_are_exact() {
        let args = mpv_args(
            0x1_2345,
            Path::new(r"D:\media\clip - 01.mkv"),
            r"\\.\pipe\oxide-flux-1",
        );
        let expected: Vec<OsString> = vec![
            OsString::from("--wid=74565"),
            OsString::from("--force-window=no"),
            OsString::from("--keep-open=no"),
            OsString::from("--no-terminal"),
            OsString::from(r"--input-ipc-server=\\.\pipe\oxide-flux-1"),
            OsString::from("--"),
            OsString::from(r"D:\media\clip - 01.mkv"),
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn the_path_always_comes_after_the_option_terminator() {
        let args = mpv_args(1, Path::new("--script=evil.lua"), "pipe");
        assert_eq!(args.len(), 7);
        assert_eq!(args[5], OsString::from("--"));
        assert_eq!(args[6], OsString::from("--script=evil.lua"));
        assert_eq!(
            options(&args[..5]),
            [
                "--wid=1",
                "--force-window=no",
                "--keep-open=no",
                "--no-terminal",
                "--input-ipc-server=pipe",
            ],
            "only our own flags may precede `--`"
        );
    }

    #[test]
    fn bounds_compare_and_clamp() {
        let bounds = Bounds::new(10, 20, 640, 480);
        assert_eq!(bounds, Bounds::new(10, 20, 640, 480));
        assert_ne!(bounds, Bounds::new(10, 20, 640, 481));

        assert_eq!(Bounds::default().clamped(), Bounds::new(0, 0, 1, 1));
        assert_eq!(
            Bounds::new(-5, -6, 0, 0).clamped(),
            Bounds::new(-5, -6, 1, 1),
            "negative origins stay, zero sizes grow to one pixel"
        );
        assert_eq!(
            Bounds::new(0, 0, u32::MAX, u32::MAX).clamped(),
            Bounds::new(0, 0, i32::MAX.unsigned_abs(), i32::MAX.unsigned_abs()),
            "Win32 sizes are i32"
        );
    }

    #[test]
    fn absent_mpv_is_unavailable_with_nothing_to_fall_back_to() {
        assert_eq!(require_mpv(|| None), Err(PlayerError::Unavailable));
        assert_eq!(
            require_mpv(|| Some(PathBuf::from("mpv.exe"))),
            Ok(PathBuf::from("mpv.exe"))
        );
    }

    #[test]
    fn a_fresh_player_is_idle_and_external() {
        let mut player = Player::new();
        assert!(player.title().is_none());
        assert!(!player.is_running());
        assert!(!player.is_embedded());
        player.set_bounds(Bounds::new(0, 0, 100, 100));
        assert!(!player.is_embedded());
    }

    #[test]
    fn media_detection_is_unchanged() {
        assert!(is_media("movie.MKV"));
        assert!(is_media("song.flac"));
        assert!(!is_media("readme.txt"));
        assert!(!is_media("no-extension"));
    }
}
