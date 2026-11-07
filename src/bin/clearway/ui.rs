use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::server::Shared;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Green,
    Blue,
    Orange,
    Red,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
/// Test mode: nothing on screen (no balloon, no message box, hidden window), errors to stderr and the log.
static TEST: AtomicBool = AtomicBool::new(false);
static ICON: AtomicBool = AtomicBool::new(true);

pub fn test_mode(with_icon: bool) {
    TEST.store(true, Ordering::SeqCst);
    ICON.store(with_icon, Ordering::SeqCst);
}

pub fn install(p: Arc<Shared>) {
    let _ = SHARED.set(p);
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn shared() -> Option<&'static Arc<Shared>> {
    SHARED.get()
}

#[cfg(not(windows))]
static QUIT: AtomicBool = AtomicBool::new(false);

#[cfg(not(windows))]
pub fn start(_log: Option<std::path::PathBuf>) -> bool {
    true
}

#[cfg(not(windows))]
pub fn update(_color: Color, _text: &str) {}

#[cfg(not(windows))]
pub fn balloon(_title: &str, _text: &str, _alert: bool) {}

#[cfg(not(windows))]
pub fn window(_open: bool) {}

#[cfg(not(windows))]
pub fn quit() {
    QUIT.store(true, Ordering::SeqCst);
}

#[cfg(not(windows))]
pub fn run_loop() {
    while !QUIT.load(Ordering::SeqCst) {
        std::thread::park_timeout(std::time::Duration::from_millis(100));
    }
}

#[cfg(not(windows))]
pub fn warn(title: &str, text: &str) {
    eprintln!("{}", clearway::tr!("{title}: {text}", "{title} : {text}"));
}

#[cfg(windows)]
pub use native::{warn, run_loop, balloon, start, window, update, open_log, quit};

#[cfg(windows)]
mod native {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_NOSOUND, NIIF_WARNING, NIM_ADD,
        NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, ShellExecuteW,
        Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CW_USEDEFAULT, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, MF_SEPARATOR, MF_STRING,
        MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SW_SHOWNORMAL,
        SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE,
        WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_NULL, WM_QUERYENDSESSION,
        WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
    };

    use super::{Color, ICON, TEST, shared};
    use crate::window;
    use crate::server::log_line;

    const ID: u32 = 1;
    const WM_ICON: u32 = WM_APP + 1;
    const WM_WINDOW: u32 = WM_APP + 2;
    const CMD_OPEN: usize = 1;
    const CMD_FREEZE: usize = 2;
    const CMD_LOG: usize = 3;
    const CMD_QUIT: usize = 4;

    static HIDDEN: AtomicUsize = AtomicUsize::new(0);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
    static ICONS: OnceLock<[usize; 4]> = OnceLock::new();
    static LOG: OnceLock<Option<PathBuf>> = OnceLock::new();
    static LAST: Mutex<Option<(Color, String)>> = Mutex::new(None);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn copy(dest: &mut [u16], s: &str) {
        let mut n = 0;
        for (i, c) in s.encode_utf16().take(dest.len() - 1).enumerate() {
            dest[i] = c;
            n = i + 1;
        }
        dest[n] = 0;
    }

    fn hidden() -> HWND {
        HIDDEN.load(Ordering::SeqCst) as HWND
    }

    fn with_icon() -> bool {
        ICON.load(Ordering::SeqCst) && !hidden().is_null()
    }

    fn data() -> NOTIFYICONDATAW {
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = hidden();
        d.uID = ID;
        d
    }

    fn icon(c: Color) -> HICON {
        ICONS.get().map_or(std::ptr::null_mut(), |i| i[c as usize] as HICON)
    }

    fn create_icon(c: Color) -> HICON {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize;
        let (r, g, b) = match c {
            Color::Green => (0x2e, 0x9d, 0x4f),
            Color::Blue => (0x2f, 0x6f, 0xd6),
            Color::Orange => (0xf0, 0x8c, 0x1a),
            Color::Red => (0xd6, 0x2f, 0x2f),
        };
        let center = size as f32 / 2.0;
        let radius = size as f32 * 0.45;
        let mut colors = vec![0u8; size * size * 4];
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 + 0.5 - center;
                let dy = y as f32 + 0.5 - center;
                let d = (dx * dx + dy * dy).sqrt();
                let coverage = (radius + 0.5 - d).clamp(0.0, 1.0);
                let edge = if d > radius - 1.5 { 0.65 } else { 1.0 };
                let p = (y * size + x) * 4;
                colors[p] = (b as f32 * edge) as u8;
                colors[p + 1] = (g as f32 * edge) as u8;
                colors[p + 2] = (r as f32 * edge) as u8;
                colors[p + 3] = (coverage * 255.0) as u8;
            }
        }
        let masked = vec![0u8; size.div_ceil(16) * 2 * size];
        unsafe {
            CreateIcon(
                GetModuleHandleW(std::ptr::null()),
                size as i32,
                size as i32,
                1,
                32,
                masked.as_ptr(),
                colors.as_ptr(),
            )
        }
    }

    fn add() -> bool {
        if !with_icon() {
            return true;
        }
        let (color, text) = LAST
            .lock()
            .ok()
            .and_then(|d| d.clone())
            .unwrap_or((Color::Green, "Clearway".into()));
        let mut d = data();
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = WM_ICON;
        d.hIcon = icon(color);
        copy(&mut d.szTip, &text);
        unsafe {
            let ok = Shell_NotifyIconW(NIM_ADD, &d) != 0;
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &d);
            ok
        }
    }

    fn remove() {
        if with_icon() {
            let d = data();
            unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
        }
    }

    pub fn update(color: Color, text: &str) {
        if !with_icon() {
            return;
        }
        {
            let Ok(mut last) = LAST.lock() else { return };
            if last.as_ref().is_some_and(|(c, t)| *c == color && t == text) {
                return;
            }
            *last = Some((color, text.to_owned()));
        }
        let mut d = data();
        d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.hIcon = icon(color);
        copy(&mut d.szTip, text);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn balloon(title: &str, text: &str, alert: bool) {
        if TEST.load(Ordering::SeqCst) || !with_icon() {
            return;
        }
        let mut d = data();
        d.uFlags = NIF_INFO;
        copy(&mut d.szInfoTitle, title);
        copy(&mut d.szInfo, text);
        d.dwInfoFlags = if alert { NIIF_WARNING } else { NIIF_INFO | NIIF_NOSOUND };
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn window(open: bool) {
        unsafe { PostMessageW(hidden(), WM_WINDOW, open as usize, 0) };
    }

    pub fn quit() {
        unsafe { PostMessageW(hidden(), WM_CLOSE, 0, 0) };
    }

    pub fn open_log() {
        if let Some(Some(path)) = LOG.get() {
            let file = wide(&path.to_string_lossy());
            let verb = wide("open");
            unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    file.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                )
            };
        }
    }

    fn queue_frozen() -> bool {
        shared().is_some_and(|p| p.snapshot().manual_freeze.is_some())
    }

    unsafe fn menu(h: HWND) {
        unsafe {
            let m = CreatePopupMenu();
            let open = wide(&clearway::tr!("Open", "Ouvrir"));
            let freeze = wide(&if queue_frozen() {
                clearway::tr!("Unfreeze the queue", "Dégeler la file")
            } else {
                clearway::tr!("Freeze the queue", "Geler la file")
            });
            let log = wide(&clearway::tr!("Open the log", "Ouvrir le journal"));
            let quit = wide(&clearway::tr!("Quit", "Quitter"));
            AppendMenuW(m, MF_STRING, CMD_OPEN, open.as_ptr());
            AppendMenuW(m, MF_STRING, CMD_FREEZE, freeze.as_ptr());
            AppendMenuW(m, MF_STRING, CMD_LOG, log.as_ptr());
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(m, MF_STRING, CMD_QUIT, quit.as_ptr());
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            // Without the foreground, the menu does not close when clicking elsewhere.
            SetForegroundWindow(h);
            TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, 0, h, std::ptr::null());
            PostMessageW(h, WM_NULL, 0, 0);
            DestroyMenu(m);
        }
    }

    fn log_here(text: &str) {
        if let Some(Some(path)) = LOG.get()
            && let Some(dir) = path.parent()
        {
            log_line(dir, text);
        }
    }

    unsafe extern "system" fn procedure(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_ICON => {
                    match (lparam & 0xffff) as u32 {
                        WM_CONTEXTMENU | WM_RBUTTONUP => menu(h),
                        WM_LBUTTONDBLCLK | NIN_SELECT => window::open(true),
                        _ => {}
                    }
                    0
                }
                WM_WINDOW => {
                    if wparam != 0 {
                        window::open(!TEST.load(Ordering::SeqCst));
                    } else {
                        window::close();
                    }
                    0
                }
                WM_COMMAND => {
                    match wparam & 0xffff {
                        CMD_OPEN => window::open(true),
                        CMD_FREEZE => {
                            if let Some(p) = shared() {
                                p.freeze(!queue_frozen(), &clearway::tr!("tray icon menu", "menu de l'icône"));
                            }
                        }
                        CMD_LOG => open_log(),
                        CMD_QUIT => {
                            DestroyWindow(h);
                        }
                        _ => {}
                    }
                    0
                }
                WM_CLOSE => {
                    DestroyWindow(h);
                    0
                }
                WM_QUERYENDSESSION => 1,
                WM_ENDSESSION => {
                    if wparam != 0 {
                        remove();
                        log_here(&clearway::tr!("STOP: Windows session ending.", "STOP : fin de session Windows."));
                    }
                    0
                }
                WM_DESTROY => {
                    window::close();
                    remove();
                    HIDDEN.store(0, Ordering::SeqCst);
                    PostQuitMessage(0);
                    0
                }
                m if m != 0 && m == TASKBAR_CREATED.load(Ordering::SeqCst) => {
                    add();
                    0
                }
                _ => DefWindowProcW(h, msg, wparam, lparam),
            }
        }
    }

    /// Creates the hidden window (messages, clean shutdown) and, outside test mode, the tray icon; on the `run_loop` thread.
    pub fn start(log: Option<PathBuf>) -> bool {
        let _ = LOG.set(log);
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware() };
        ICONS.get_or_init(|| {
            [Color::Green, Color::Blue, Color::Orange, Color::Red].map(|c| create_icon(c) as usize)
        });
        let class = wide("ClearwayService");
        let title = wide("Clearway (service)");
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(procedure);
            wc.hInstance = instance;
            wc.lpszClassName = class.as_ptr();
            RegisterClassW(&wc);
            TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::SeqCst);
            // Top-level window never shown: `taskkill` without /F sends it WM_CLOSE.
            let h = CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if h.is_null() {
                return false;
            }
            HIDDEN.store(h as usize, Ordering::SeqCst);
        }
        add()
    }

    pub fn run_loop() {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if !window::dialog_message(&msg) {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            // WM_DESTROY already removed the tray icon: its images are no longer used.
            for &h in ICONS.get().into_iter().flatten() {
                if h != 0 {
                    DestroyIcon(h as HICON);
                }
            }
        }
        window::release_resources();
    }

    /// Startup error: stderr only in test mode; otherwise a red tray icon and a balloon for a few seconds.
    pub fn warn(title: &str, text: &str) {
        if TEST.load(Ordering::SeqCst) {
            eprintln!("{}", clearway::tr!("{title}: {text}", "{title} : {text}"));
            return;
        }
        if let Ok(mut d) = LAST.lock() {
            *d = Some((Color::Red, clearway::tr!("Clearway: {title}", "Clearway : {title}")));
        }
        if hidden().is_null() && !start(None) {
            return;
        }
        balloon(title, text, true);
        std::thread::sleep(Duration::from_secs(8));
        remove();
    }
}
