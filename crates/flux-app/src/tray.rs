//! Windows System Tray Icon implementation for Oxide Flux.

#![allow(unsafe_code)]

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
        DestroyIcon, DestroyMenu, DestroyWindow, GetCursorPos, HICON, IsIconic, MF_SEPARATOR,
        MF_STRING, RegisterClassW, SetForegroundWindow, ShowWindow, TPM_BOTTOMALIGN,
        TPM_RIGHTBUTTON, TrackPopupMenu, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
        WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_OVERLAPPED,
    };

    const WM_TRAY_CALLBACK: u32 = WM_USER + 100;
    const MENU_ID_OPEN: usize = 1001;
    const MENU_ID_PAUSE_ALL: usize = 1002;
    const MENU_ID_RESUME_ALL: usize = 1003;
    const MENU_ID_EXIT: usize = 1004;

    /// Events dispatched from tray interaction.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TrayEvent {
        OpenWindow,
        PauseAll,
        ResumeAll,
        Exit,
    }

    static EVENT_SENDER: std::sync::Mutex<Option<Sender<TrayEvent>>> = std::sync::Mutex::new(None);
    static MAIN_WINDOW_HWND: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "system" fn tray_window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAY_CALLBACK => {
                let event_type = (lparam & 0xffff) as u32;
                match event_type {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                        let main_hwnd = MAIN_WINDOW_HWND.load(Ordering::Relaxed);
                        if main_hwnd != 0 {
                            show_and_focus(main_hwnd);
                        }
                        if let Ok(guard) = EVENT_SENDER.lock()
                            && let Some(sender) = guard.as_ref()
                        {
                            let _ = sender.send(TrayEvent::OpenWindow);
                        }
                    }
                    WM_RBUTTONUP => unsafe {
                        show_context_menu(hwnd);
                    },
                    _ => {}
                }
                0
            }
            WM_COMMAND => {
                let cmd = wparam;
                let event = match cmd {
                    MENU_ID_OPEN => {
                        let main_hwnd = MAIN_WINDOW_HWND.load(Ordering::Relaxed);
                        if main_hwnd != 0 {
                            show_and_focus(main_hwnd);
                        }
                        Some(TrayEvent::OpenWindow)
                    }
                    MENU_ID_PAUSE_ALL => Some(TrayEvent::PauseAll),
                    MENU_ID_RESUME_ALL => Some(TrayEvent::ResumeAll),
                    MENU_ID_EXIT => Some(TrayEvent::Exit),
                    _ => None,
                };
                if let Some(event) = event
                    && let Ok(guard) = EVENT_SENDER.lock()
                    && let Some(sender) = guard.as_ref()
                {
                    let _ = sender.send(event);
                }
                0
            }
            WM_DESTROY => 0,
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    unsafe fn show_context_menu(hwnd: HWND) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }

            let open_str = to_wide("Відкрити Oxide Flux\0");
            let pause_str = to_wide("Призупинити всі\0");
            let resume_str = to_wide("Відновити всі\0");
            let exit_str = to_wide("Вихід\0");

            AppendMenuW(menu, MF_STRING, MENU_ID_OPEN, open_str.as_ptr());
            AppendMenuW(menu, MF_STRING, MENU_ID_PAUSE_ALL, pause_str.as_ptr());
            AppendMenuW(menu, MF_STRING, MENU_ID_RESUME_ALL, resume_str.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(menu, MF_STRING, MENU_ID_EXIT, exit_str.as_ptr());

            let mut pt: POINT = std::mem::zeroed();
            GetCursorPos(&mut pt);

            // Required so menu disappears when clicking outside
            SetForegroundWindow(hwnd);

            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                pt.x,
                pt.y,
                0,
                hwnd,
                std::ptr::null(),
            );

            DestroyMenu(menu);
        }
    }

    pub fn show_and_focus(hwnd_isize: isize) {
        if hwnd_isize == 0 {
            return;
        }
        unsafe {
            let hwnd = hwnd_isize as HWND;
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, 9); // SW_RESTORE
            } else {
                ShowWindow(hwnd, 5); // SW_SHOW
            }
            SetForegroundWindow(hwnd);
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    pub struct TrayIcon {
        hwnd: HWND,
        hicon: HICON,
        events: Receiver<TrayEvent>,
    }

    impl TrayIcon {
        pub fn new() -> Option<Self> {
            let (tx, rx) = channel();
            if let Ok(mut guard) = EVENT_SENDER.lock() {
                *guard = Some(tx);
            }

            unsafe {
                // Register helper message window class
                let class_name = to_wide("OxideFluxTrayMessageWindow\0");
                let mut wc: WNDCLASSW = std::mem::zeroed();
                wc.lpfnWndProc = Some(tray_window_proc);
                wc.lpszClassName = class_name.as_ptr();

                RegisterClassW(&wc);

                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    class_name.as_ptr(),
                    WS_OVERLAPPED,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );

                if hwnd.is_null() {
                    return None;
                }

                // Create HICON from embedded PNG bytes
                let png_bytes = include_bytes!("../assets/icon-32.png");
                let hicon = CreateIconFromResourceEx(
                    png_bytes.as_ptr(),
                    png_bytes.len() as u32,
                    1, // 1 = icon
                    0x00030000,
                    32,
                    32,
                    0,
                );

                if hicon.is_null() {
                    DestroyWindow(hwnd);
                    return None;
                }

                let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = hwnd;
                nid.uID = 1;
                nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
                nid.uCallbackMessage = WM_TRAY_CALLBACK;
                nid.hIcon = hicon;

                let tip_chars = to_wide("Oxide Flux\0");
                let copy_len = tip_chars.len().min(nid.szTip.len());
                nid.szTip[..copy_len].copy_from_slice(&tip_chars[..copy_len]);

                if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
                    DestroyIcon(hicon);
                    DestroyWindow(hwnd);
                    return None;
                }

                Some(Self {
                    hwnd,
                    hicon,
                    events: rx,
                })
            }
        }

        pub fn set_main_hwnd(&self, hwnd: isize) {
            MAIN_WINDOW_HWND.store(hwnd, Ordering::Relaxed);
        }

        pub fn poll_event(&self) -> Option<TrayEvent> {
            self.events.try_recv().ok()
        }
    }

    impl Drop for TrayIcon {
        fn drop(&mut self) {
            unsafe {
                let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = self.hwnd;
                nid.uID = 1;
                Shell_NotifyIconW(NIM_DELETE, &nid);

                if !self.hicon.is_null() {
                    DestroyIcon(self.hicon);
                }
                if !self.hwnd.is_null() {
                    DestroyWindow(self.hwnd);
                }
            }
            if let Ok(mut guard) = EVENT_SENDER.lock() {
                *guard = None;
            }
        }
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TrayEvent {
        OpenWindow,
        PauseAll,
        ResumeAll,
        Exit,
    }

    pub struct TrayIcon;

    impl TrayIcon {
        pub fn new() -> Option<Self> {
            None
        }
        pub fn set_main_hwnd(&self, _hwnd: isize) {}
        pub fn poll_event(&self) -> Option<TrayEvent> {
            None
        }
    }

    /// There is no tray on other platforms, so there is nothing to bring
    /// forward; the iced window keeps its own focus handling.
    pub fn show_and_focus(_hwnd_isize: isize) {}
}

#[cfg(not(windows))]
pub use imp::*;
