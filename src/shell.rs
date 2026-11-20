//! The Windows shell side of the app: a tray icon that keeps the service resident, start-at-login,
//! opening the browser, and error dialogs. Everything here is optional; other platforms get stubs.

#[cfg(not(windows))]
pub use other::*;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod other {
    use anyhow::{Result, bail};
    pub const SUPPORTED: bool = false;
    pub fn attach_console() {}
    pub fn open_url(_: &str) {}
    pub fn fatal(message: &str) {
        eprintln!("{message}");
    }
    pub fn autostart_enabled() -> bool {
        false
    }
    pub fn set_autostart(_: bool) -> Result<()> {
        bail!("此平台不支持开机自启")
    }
    pub fn run_tray(
        _: impl Fn() + Send + Sync + 'static,
        _: impl Fn() + Send + Sync + 'static,
    ) -> Result<()> {
        bail!("此平台没有托盘")
    }
    pub fn set_tooltip(_: &str) {}
    pub fn close_tray() {}
}

#[cfg(windows)]
mod win {
    use anyhow::{Result, bail};
    use std::{
        ptr::{null, null_mut},
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicU32, AtomicUsize, Ordering},
        },
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_SUCCESS, GENERIC_WRITE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Storage::FileSystem::{CreateFileW, FILE_SHARE_WRITE, OPEN_EXISTING},
        System::{
            Console::{AttachConsole, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
            LibraryLoader::GetModuleHandleW,
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
                REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW,
                RegSetValueExW,
            },
        },
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
                Shell_NotifyIconW, ShellExecuteW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW,
                MB_ICONERROR, MB_OK, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW,
                PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
                SetForegroundWindow, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
                TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY,
                WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
            },
        },
    };

    pub const SUPPORTED: bool = true;
    const WM_TRAY: u32 = WM_APP + 1;
    const WM_TOOLTIP: u32 = WM_APP + 2;
    const ID_OPEN: usize = 1;
    const ID_AUTOSTART: usize = 2;
    const ID_QUIT: usize = 3;
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE: &str = "RecallPort";

    struct Hooks {
        open: Box<dyn Fn() + Send + Sync>,
        quit: Box<dyn Fn() + Send + Sync>,
        tooltip: Mutex<String>,
        restarted: AtomicU32,
    }
    static HOOKS: OnceLock<Hooks> = OnceLock::new();
    static WINDOW: AtomicUsize = AtomicUsize::new(0);

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// When started from a terminal the process has no console of its own; borrow the parent's so
    /// `--help` and error text show up there.
    pub fn attach_console() {
        unsafe {
            if AttachConsole(0xFFFF_FFFF) == 0 {
                return;
            }
            let name = wide("CONOUT$");
            let handle = CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if !handle.is_null() && handle as isize != -1 {
                SetStdHandle(STD_OUTPUT_HANDLE, handle);
                SetStdHandle(STD_ERROR_HANDLE, handle);
            }
        }
    }

    pub fn open_url(url: &str) {
        unsafe {
            ShellExecuteW(
                null_mut(),
                wide("open").as_ptr(),
                wide(url).as_ptr(),
                null(),
                null(),
                1,
            );
        }
    }

    pub fn fatal(message: &str) {
        unsafe {
            MessageBoxW(
                null_mut(),
                wide(message).as_ptr(),
                wide("RecallPort").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn open_run_key(access: u32) -> Option<HKEY> {
        unsafe {
            let mut key: HKEY = null_mut();
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                access,
                null(),
                &mut key,
                null_mut(),
            );
            (status == ERROR_SUCCESS).then_some(key)
        }
    }

    fn command() -> Result<String> {
        Ok(format!(
            "\"{}\" --background",
            std::env::current_exe()?.display()
        ))
    }

    pub fn autostart_enabled() -> bool {
        let Some(key) = open_run_key(KEY_QUERY_VALUE) else {
            return false;
        };
        let wanted = command().unwrap_or_default();
        unsafe {
            let mut size = 0u32;
            let name = wide(VALUE);
            let probe = RegQueryValueExW(
                key,
                name.as_ptr(),
                null(),
                null_mut(),
                null_mut(),
                &mut size,
            );
            let mut matches = false;
            if probe == ERROR_SUCCESS && size > 0 {
                let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
                let status = RegQueryValueExW(
                    key,
                    name.as_ptr(),
                    null(),
                    null_mut(),
                    buffer.as_mut_ptr() as *mut u8,
                    &mut size,
                );
                if status == ERROR_SUCCESS {
                    let text = String::from_utf16_lossy(&buffer);
                    matches = text.trim_end_matches('\0') == wanted;
                }
            }
            RegCloseKey(key);
            matches
        }
    }

    pub fn set_autostart(on: bool) -> Result<()> {
        let Some(key) = open_run_key(KEY_SET_VALUE) else {
            bail!("无法写入当前用户的启动项");
        };
        let name = wide(VALUE);
        let status = unsafe {
            if on {
                let data = wide(&command()?);
                RegSetValueExW(
                    key,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr() as *const u8,
                    (data.len() * 2) as u32,
                )
            } else {
                let status = RegDeleteValueW(key, name.as_ptr());
                if status == 2 { ERROR_SUCCESS } else { status }
            }
        };
        unsafe { RegCloseKey(key) };
        if status != ERROR_SUCCESS {
            bail!("系统拒绝了这项设置（错误码 {status}）");
        }
        Ok(())
    }

    /// The icon embedded in the executable (resource id 1, see `build.rs`); falls back to a drawn one.
    fn icon() -> *mut core::ffi::c_void {
        let embedded = unsafe {
            LoadIconW(
                GetModuleHandleW(null()),
                std::ptr::without_provenance::<u16>(1),
            )
        };
        if embedded.is_null() {
            drawn_icon()
        } else {
            embedded
        }
    }

    /// A 32x32 teal tile with a ring and a dot, used only if the embedded icon cannot be loaded.
    fn drawn_icon() -> *mut core::ffi::c_void {
        const N: usize = 32;
        let mut pixels = vec![0u8; N * N * 4];
        for y in 0..N {
            for x in 0..N {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let dx = ((fx - 16.0).abs() - 9.0).max(0.0);
                let dy = ((fy - 16.0).abs() - 9.0).max(0.0);
                if dx.hypot(dy) > 7.0 {
                    continue;
                }
                let r = (fx - 16.0).hypot(fy - 16.0);
                let white = (8.0..10.6).contains(&r) || r < 3.4;
                let (b, g, red) = if white {
                    (255, 255, 255)
                } else {
                    (110, 118, 15)
                };
                let at = (y * N + x) * 4;
                pixels[at..at + 4].copy_from_slice(&[b, g, red, 255]);
            }
        }
        let mask = [0u8; N * N / 8];
        unsafe {
            CreateIcon(
                GetModuleHandleW(null()),
                N as i32,
                N as i32,
                1,
                32,
                mask.as_ptr(),
                pixels.as_ptr(),
            )
        }
    }

    fn data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = hwnd;
        d.uID = 1;
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = icon();
        if let Some(h) = HOOKS.get() {
            let tip = h.tooltip.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for (slot, unit) in d.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
                *slot = unit;
            }
        }
        d
    }

    unsafe fn menu(hwnd: HWND) {
        unsafe {
            let menu = CreatePopupMenu();
            let autostart = if autostart_enabled() { MF_CHECKED } else { 0 };
            AppendMenuW(menu, MF_STRING, ID_OPEN, wide("打开管理页").as_ptr());
            AppendMenuW(
                menu,
                MF_STRING | autostart,
                ID_AUTOSTART,
                wide("登录 Windows 时自动启动").as_ptr(),
            );
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(menu, MF_STRING, ID_QUIT, wide("退出 RecallPort").as_ptr());
            let mut at = POINT { x: 0, y: 0 };
            GetCursorPos(&mut at);
            SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                at.x,
                at.y,
                0,
                hwnd,
                null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            let Some(hooks) = HOOKS.get() else { return };
            match chosen as usize {
                ID_OPEN => (hooks.open)(),
                ID_AUTOSTART => {
                    if let Err(error) = set_autostart(!autostart_enabled()) {
                        fatal(&format!("{error}"));
                    }
                }
                ID_QUIT => (hooks.quit)(),
                _ => {}
            }
        }
    }

    unsafe extern "system" fn procedure(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe {
            match message {
                WM_TRAY => {
                    match (lparam as u32) & 0xFFFF {
                        WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                            if let Some(h) = HOOKS.get() {
                                (h.open)();
                            }
                        }
                        WM_RBUTTONUP | WM_CONTEXTMENU => menu(hwnd),
                        _ => {}
                    }
                    0
                }
                WM_TOOLTIP => {
                    Shell_NotifyIconW(NIM_MODIFY, &data(hwnd));
                    0
                }
                WM_CLOSE => {
                    Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
                    DestroyWindow(hwnd);
                    0
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    0
                }
                m if HOOKS
                    .get()
                    .is_some_and(|h| h.restarted.load(Ordering::Relaxed) == m) =>
                {
                    // Explorer restarted: put the icon back.
                    Shell_NotifyIconW(NIM_ADD, &data(hwnd));
                    0
                }
                _ => DefWindowProcW(hwnd, message, wparam, lparam),
            }
        }
    }

    pub fn set_tooltip(text: &str) {
        if let Some(h) = HOOKS.get() {
            *h.tooltip.lock().unwrap_or_else(|e| e.into_inner()) = text.to_owned();
        }
        let hwnd = WINDOW.load(Ordering::SeqCst);
        if hwnd != 0 {
            unsafe { PostMessageW(hwnd as HWND, WM_TOOLTIP, 0, 0) };
        }
    }

    /// Ask the tray loop to finish (from any thread).
    pub fn close_tray() {
        let hwnd = WINDOW.load(Ordering::SeqCst);
        if hwnd != 0 {
            unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
        }
    }

    /// Show the tray icon and run its message loop on this thread until `close_tray` or quit.
    pub fn run_tray(
        open: impl Fn() + Send + Sync + 'static,
        quit: impl Fn() + Send + Sync + 'static,
    ) -> Result<()> {
        unsafe {
            let hooks = Hooks {
                open: Box::new(open),
                quit: Box::new(quit),
                tooltip: Mutex::new("RecallPort · 正在启动…".into()),
                restarted: AtomicU32::new(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr())),
            };
            if HOOKS.set(hooks).is_err() {
                bail!("托盘已经在运行");
            }
            let instance = GetModuleHandleW(null());
            let class = wide("RecallPortTray");
            let mut window: WNDCLASSW = std::mem::zeroed();
            window.lpfnWndProc = Some(procedure);
            window.hInstance = instance;
            window.lpszClassName = class.as_ptr();
            if RegisterClassW(&window) == 0 {
                bail!("无法注册托盘窗口");
            }
            // A message-only window: it never appears anywhere.
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("RecallPort").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                (-3isize) as HWND,
                null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                bail!("无法创建托盘窗口");
            }
            WINDOW.store(hwnd as usize, Ordering::SeqCst);
            if Shell_NotifyIconW(NIM_ADD, &data(hwnd)) == 0 {
                bail!("无法显示托盘图标");
            }
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}
