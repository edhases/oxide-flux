//! Windows child-window embedding for mpv.
//!
//! mpv renders into any `HWND` passed with `--wid`, so a `STATIC` child of the
//! application window is all the platform glue needs: no window class of our
//! own, no message pump, and no callback into Rust. The child is owned by
//! [`Session`] and destroyed when the session is dropped, which happens after
//! mpv has been reaped — `Player::stop` and `Player::drop` therefore leave
//! neither a process nor a window behind.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{GetLastError, HWND};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, WS_CHILD,
    WS_CLIPSIBLINGS, WS_VISIBLE,
};

use super::{Bounds, mpv_args};

/// `"STATIC"` as null-terminated UTF-16.
///
/// `STATIC` is a system class, so no `RegisterClassW` is required and
/// `hInstance` must be null when creating it.
const STATIC_CLASS: [u16; 7] = [
    b'S' as u16,
    b'T' as u16,
    b'A' as u16,
    b'T' as u16,
    b'I' as u16,
    b'C' as u16,
    0,
];

/// One live playback session: the window mpv draws into.
///
/// The mpv process itself stays in `Player`; this type only owns platform
/// resources that must be released in a fixed order (process first, window
/// second).
#[derive(Debug)]
pub(super) struct Session {
    window: ChildWindow,
    /// The IPC pipe mpv listens on. Reserved for the future controller that
    /// will seek/stop playback; kept so the name can be handed out later.
    _ipc: String,
}

impl Session {
    /// Creates a `STATIC` child of `parent` and spawns mpv into it.
    ///
    /// A failure before the spawn destroys the window again, so the caller sees
    /// either a complete session or no window at all.
    pub(super) fn start(
        parent: isize,
        bounds: Bounds,
        executable: &Path,
        path: &Path,
    ) -> Result<(Self, Child), String> {
        let window = ChildWindow::create(parent, bounds)?;
        let ipc = ipc_pipe_path();
        let child = Command::new(executable)
            .args(mpv_args(window.hwnd(), path, &ipc))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start mpv in the embedded window: {error}"))?;
        Ok((Self { window, _ipc: ipc }, child))
    }

    /// Moves and resizes the child window, in physical pixels.
    pub(super) fn set_bounds(&self, bounds: Bounds) {
        self.window.set_bounds(bounds);
    }
}

/// A `STATIC` child window that mpv draws into.
///
/// Owns the handle and destroys it on drop. The handle is stored as `isize`
/// rather than `HWND` because `windows-sys` models `HWND` as a raw pointer,
/// which would otherwise make the whole `Player` non-`Send`.
#[derive(Debug)]
struct ChildWindow {
    hwnd: isize,
}

impl ChildWindow {
    /// Creates a visible child of `parent` at `bounds`.
    fn create(parent: isize, bounds: Bounds) -> Result<Self, String> {
        if parent == 0 {
            return Err("the parent window handle is null".to_owned());
        }
        let bounds = bounds.clamped();
        // SAFETY: `STATIC` is a system class, so `hinstance` may be null and no
        // registration is needed. The only non-null pointer is
        // `STATIC_CLASS.as_ptr()`, which points at a `'static` constant with a
        // terminating NUL. The returned handle is checked before it is stored.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                STATIC_CLASS.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                bounds.x,
                bounds.y,
                bounds.width as i32,
                bounds.height as i32,
                parent as HWND,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            // SAFETY: `GetLastError` only reads the calling thread's error
            // slot; no pointer is involved.
            let code = unsafe { GetLastError() };
            return Err(format!("CreateWindowExW(STATIC) failed with error {code}"));
        }
        Ok(Self {
            hwnd: hwnd as isize,
        })
    }

    /// The raw handle as `--wid` wants it (decimal `isize`).
    fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// Moves and resizes the window; failures are ignored because the window
    /// may be on its way out (mpv already exited, parent closing).
    fn set_bounds(&self, bounds: Bounds) {
        let bounds = bounds.clamped();
        // SAFETY: the handle belongs to this window and is destroyed only in
        // `Drop`, so it is valid for the duration of the call. `SWP_NOZORDER`
        // makes `hwndinsertafter` irrelevant (hence the null) and
        // `SWP_NOACTIVATE` keeps keyboard focus in the application.
        unsafe {
            SetWindowPos(
                self.hwnd as HWND,
                std::ptr::null_mut(),
                bounds.x,
                bounds.y,
                bounds.width as i32,
                bounds.height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

impl Drop for ChildWindow {
    fn drop(&mut self) {
        // SAFETY: `hwnd` was returned by `CreateWindowExW` in `create` and is
        // destroyed exactly once, here. `DestroyWindow` returns `FALSE` for an
        // already-destroyed handle instead of faulting.
        unsafe {
            DestroyWindow(self.hwnd as HWND);
        }
    }
}

/// A unique named-pipe path for mpv's JSON IPC.
///
/// mpv creates a server named pipe, so no filesystem entry is involved; the
/// process id plus a counter is enough to keep two sessions apart. A future
/// controller can connect to `\\.\pipe\oxide-flux-<pid>-<n>`.
fn ipc_pipe_path() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    format!(r"\\.\pipe\oxide-flux-{}-{sequence}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_parent_is_a_typed_failure() {
        match ChildWindow::create(0, Bounds::new(0, 0, 10, 10)) {
            Ok(_) => panic!("a null parent cannot host a child window"),
            Err(error) => assert!(error.contains("parent"), "{error}"),
        }
    }

    #[test]
    fn ipc_pipe_paths_are_unique_and_windows_shaped() {
        let first = ipc_pipe_path();
        let second = ipc_pipe_path();
        assert!(first.starts_with(r"\\.\pipe\oxide-flux-"), "{first}");
        assert!(
            first.contains(&format!("-{}-", std::process::id())),
            "{first}"
        );
        assert_ne!(first, second);
    }

    /// Opt-in smoke test: needs a real mpv and a desktop session.
    ///
    /// Run with `OXIDE_FLUX_TEST_MPV=1`; the default test run never launches a
    /// process or opens a window.
    #[test]
    fn embeds_mpv_into_a_real_child_window_when_enabled() {
        if std::env::var_os("OXIDE_FLUX_TEST_MPV").is_none() {
            return;
        }
        let Some(executable) = super::super::mpv_executable() else {
            return;
        };

        use windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

        // SAFETY: `GetDesktopWindow` returns a borrowed window handle owned by
        // the system; it is valid in the caller's desktop and is not destroyed
        // here. It is only used as the parent of our child window.
        let parent = unsafe { GetDesktopWindow() } as isize;

        let media = std::env::temp_dir().join("oxide-flux-embed-smoke.mkv");
        // An empty file is enough: the point is that mpv starts against the
        // child window, not that a frame decodes.
        if std::fs::write(&media, []).is_err() {
            return;
        }

        match Session::start(parent, Bounds::new(0, 0, 320, 240), &executable, &media) {
            Ok((session, mut child)) => {
                session.set_bounds(Bounds::new(0, 0, 640, 360));
                let _ = child.kill();
                let _ = child.wait();
            }
            Err(error) => panic!("{error}"),
        }
        let _ = std::fs::remove_file(&media);
    }
}
