use std::cell::{Cell, RefCell};
use std::sync::{Mutex, Once};

use clearway::format::{cores, duration, gb};
use clearway::model::{TicketState, Release, Status, ReserveView};
use clearway::scheduler::{RESERVE_COUNT, fresh_wsl};
use clearway::system::hhmm;
use clearway::tr;
use windows_sys::Win32::Foundation::{COLORREF, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DT_END_ELLIPSIS, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FillRect, FrameRect, GetDC, GetDeviceCaps,
    HBRUSH, HDC, HFONT, InvalidateRect, LOGPIXELSY, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
    ACTCTXW, ActivateActCtx, CreateActCtxW, DeactivateActCtx, ReleaseActCtx,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
    LVIF_PARAM, LVIF_STATE, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_GETITEMW,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW,
    LVN_ITEMCHANGED, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_GRIDLINES, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_PUSHBUTTON, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOVSCROLL, ES_MULTILINE,
    ES_READONLY, GetClientRect, IDYES, IsDialogMessageW, KillTimer, MB_DEFBUTTON2,
    MB_ICONQUESTION, MB_ICONWARNING, MB_YESNO, MSG, MessageBoxW, MoveWindow, RegisterClassW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowTextW, ShowWindow, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY,
    WM_NOTIFY, WM_PAINT, WM_PRINTCLIENT, WM_SETFONT, WM_SETREDRAW, WM_SIZE, WM_TIMER, WM_VSCROLL, SB_BOTTOM, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::server::{Closing, log_tail};
use crate::ui::{self, shared};

const GROUP_GAP: i32 = 10;
const ID_LIST: usize = 100;
const ID_CANCEL: usize = 101;
const ID_FREE: usize = 102;
const ID_FREEZE: usize = 103;
const ID_LOG_BUTTON: usize = 104;
const ID_LOG: usize = 105;
const ID_FORCE: usize = 106;
const TIMER: usize = 1;
const LOG_LINES: usize = 20;

const USED_COLOR: COLORREF = 0x00b0_7a4a;
const RESERVED_COLOR: COLORREF = 0x0030_a0f0;
const MARGIN_COLOR: COLORREF = 0x00a8_a8e8;
const FREE_COLOR: COLORREF = 0x00ee_eeee;
const FRAME_COLOR: COLORREF = 0x0088_8888;

fn columns() -> [(String, i32); 11] {
    [
        (tr!("No.", "N°"), 45),
        ("Session".into(), 140),
        (tr!("Directory", "Dossier"), 120),
        (tr!("Branch / worktree", "Branche / worktree"), 190),
        (tr!("State", "État"), 115),
        (tr!("Side", "Côté"), 60),
        ("RAM".into(), 60),
        ("CPU".into(), 70),
        (tr!("Duration", "Durée"), 60),
        (tr!("Since", "Depuis"), 55),
        (tr!("Reason", "Raison"), 900),
    ]
}

fn state_label(state: TicketState) -> String {
    match state {
        TicketState::Waiting => tr!("waiting", "attente"),
        TicketState::Active => tr!("active", "actif"),
        TicketState::Orphaned => tr!("orphaned", "orphelin"),
    }
}

thread_local! {
    static MAIN: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static LIST: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static BUTTONS: Cell<[HWND; 5]> = const { Cell::new([std::ptr::null_mut(); 5]) };
    static LOG: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static FONT: Cell<HFONT> = const { Cell::new(std::ptr::null_mut()) };
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static BARS: Cell<RECT> = const { Cell::new(RECT { left: 0, top: 0, right: 0, bottom: 0 }) };
    static STATUS: RefCell<Option<Status>> = const { RefCell::new(None) };
    static LOG_TEXT: RefCell<String> = const { RefCell::new(String::new()) };
}

static CLASS: Once = Once::new();
static STYLES: Once = Once::new();
/// Styles activation context (handle, activation cookie or 0), released on the UI thread at shutdown.
static CONTEXT: Mutex<Option<(usize, usize)>> = Mutex::new(None);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn s(v: i32) -> i32 {
    (v as f32 * SCALE.with(Cell::get)).round() as i32
}

/// Common Controls v6 (current Windows look) without an embedded resource: activation context from a manifest written next to the state.
fn enable_styles() {
    STYLES.call_once(|| {
        let Some(p) = shared() else { return };
        let path = p.dir().join("common-controls.manifest");
        let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"><dependency><dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/></dependentAssembly></dependency></assembly>
"#;
        if std::fs::write(&path, manifest).is_err() {
            return;
        }
        let source = wide(&path.to_string_lossy());
        let mut ctx: ACTCTXW = unsafe { std::mem::zeroed() };
        ctx.cbSize = size_of::<ACTCTXW>() as u32;
        ctx.lpSource = source.as_ptr();
        unsafe {
            let h = CreateActCtxW(&ctx);
            if h != INVALID_HANDLE_VALUE {
                let mut cookie = 0usize;
                if ActivateActCtx(h, &mut cookie) == 0 {
                    cookie = 0;
                }
                if let Ok(mut c) = CONTEXT.lock() {
                    *c = Some((h as usize, cookie));
                }
            }
            let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_LISTVIEW_CLASSES };
            InitCommonControlsEx(&icc);
        }
    });
}

/// Deactivates and releases the styles context; on the UI thread, after the message loop.
pub fn release_resources() {
    let Some((h, cookie)) = CONTEXT.lock().ok().and_then(|mut c| c.take()) else { return };
    unsafe {
        if cookie != 0 {
            DeactivateActCtx(0, cookie);
        }
        ReleaseActCtx(h as _);
    }
}

pub fn dialog_message(msg: &MSG) -> bool {
    let h = MAIN.with(Cell::get);
    !h.is_null() && unsafe { IsDialogMessageW(h, msg) } != 0
}

/// Opens the window (or brings it to the front); `visible` false in test mode: created but never shown.
pub fn open(visible: bool) {
    let existing = MAIN.with(Cell::get);
    if !existing.is_null() {
        if visible {
            unsafe {
                ShowWindow(existing, SW_SHOWNORMAL);
                SetForegroundWindow(existing);
            }
        }
        return;
    }
    enable_styles();
    let class = wide("ClearwayWindow");
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        CLASS.call_once(|| {
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(procedure);
            wc.hInstance = instance;
            wc.lpszClassName = class.as_ptr();
            wc.hbrBackground = (15 + 1) as HBRUSH;
            // Resource 1: the icon linked by build.rs; null (default icon) if the exe was built without it.
            wc.hIcon = windows_sys::Win32::UI::WindowsAndMessaging::LoadIconW(instance, 1 as _);
            wc.hCursor = windows_sys::Win32::UI::WindowsAndMessaging::LoadCursorW(
                std::ptr::null_mut(),
                windows_sys::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            );
            RegisterClassW(&wc);
        });
        let screen = GetDC(std::ptr::null_mut());
        SCALE.with(|e| e.set(GetDeviceCaps(screen, LOGPIXELSY as i32) as f32 / 96.0));
        ReleaseDC(std::ptr::null_mut(), screen);
        let title = wide("Clearway");
        // Test mode: laid out off screen, without taskbar entry or activation, so it can be captured without showing anything.
        let (ex, x, y) = if visible {
            (WS_EX_CONTROLPARENT, CW_USEDEFAULT, CW_USEDEFAULT)
        } else {
            (WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, -20000, -20000)
        };
        let h = CreateWindowExW(
            ex,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            s(1080),
            s(700),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if h.is_null() {
            return;
        }
        if visible {
            ShowWindow(h, SW_SHOWNORMAL);
            SetForegroundWindow(h);
        } else {
            ShowWindow(h, SW_SHOWNOACTIVATE);
        }
    }
}

pub fn close() {
    let h = MAIN.with(Cell::get);
    if !h.is_null() {
        unsafe { DestroyWindow(h) };
    }
}

unsafe fn child(parent: HWND, ex: u32, class: &str, text: &str, style: u32, id: usize) -> HWND {
    let c = wide(class);
    let t = wide(text);
    unsafe {
        let h = CreateWindowExW(
            ex,
            c.as_ptr(),
            t.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            parent,
            id as _,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        SendMessageW(h, WM_SETFONT, FONT.with(Cell::get) as usize, 1);
        h
    }
}

unsafe fn create_children(h: HWND) {
    unsafe {
        let name = wide("Segoe UI");
        let font = CreateFontW(-s(12), 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, name.as_ptr());
        FONT.with(|p| p.set(font));
        let list = child(
            h,
            WS_EX_CLIENTEDGE,
            "SysListView32",
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            ID_LIST,
        );
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES | LVS_EX_DOUBLEBUFFER) as isize,
        );
        for (i, (title, width)) in columns().iter().enumerate() {
            let mut t = wide(title);
            let mut col: LVCOLUMNW = std::mem::zeroed();
            col.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM;
            col.cx = s(*width);
            col.pszText = t.as_mut_ptr();
            col.iSubItem = i as i32;
            SendMessageW(list, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
        }
        LIST.with(|l| l.set(list));
        let button = |text: &str, id: usize| child(h, 0, "BUTTON", text, WS_TABSTOP | BS_PUSHBUTTON as u32, id);
        BUTTONS.with(|b| {
            b.set([
                button(&tr!("Cancel request…", "Annuler la demande…"), ID_CANCEL),
                button(&tr!("Release slot…", "Libérer le créneau…"), ID_FREE),
                button(&tr!("Force through…", "Forcer le passage…"), ID_FORCE),
                button(&tr!("Freeze the queue", "Geler la file"), ID_FREEZE),
                button(&tr!("Open the log", "Ouvrir le journal"), ID_LOG_BUTTON),
            ])
        });
        let log = child(
            h,
            WS_EX_CLIENTEDGE,
            "EDIT",
            "",
            WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
            ID_LOG,
        );
        LOG.with(|j| j.set(log));
    }
}

unsafe fn lay_out(h: HWND) {
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(h, &mut rc);
        let m = s(10);
        let width = rc.right - 2 * m;
        let list_height = ((rc.bottom - 2 * m) as f32 * 0.38).max(s(140) as f32) as i32;
        let mut y = m;
        MoveWindow(LIST.with(Cell::get), m, y, width, list_height, 1);
        y += list_height + s(8);
        let mut x = m;
        for b in BUTTONS.with(Cell::get) {
            MoveWindow(b, x, y, s(170), s(28), 1);
            x += s(178);
        }
        y += s(28) + s(10);
        let bars = RECT { left: m, top: y, right: m + width, bottom: y + s(22) + RESERVE_COUNT as i32 * s(26) + s(GROUP_GAP) };
        BARS.with(|b| b.set(bars));
        y = bars.bottom + s(8);
        MoveWindow(LOG.with(Cell::get), m, y, width, (rc.bottom - m - y).max(s(60)), 1);
        InvalidateRect(h, std::ptr::null(), 1);
    }
}

fn selection() -> Option<u64> {
    let list = LIST.with(Cell::get);
    unsafe {
        let index = SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
        if index < 0 {
            return None;
        }
        let mut item: LVITEMW = std::mem::zeroed();
        item.mask = LVIF_PARAM;
        item.iItem = index as i32;
        SendMessageW(list, LVM_GETITEMW, 0, &mut item as *mut _ as isize);
        Some(item.lParam as u64)
    }
}

fn state_of(id: u64) -> Option<(TicketState, String)> {
    STATUS.with(|st| {
        st.borrow()
            .as_ref()
            .and_then(|s| s.tickets.iter().find(|t| t.id == id).map(|t| (t.state, format!("{} — {}", t.session, t.description))))
    })
}

fn update_buttons() {
    let [cancel, release, force, _, _] = BUTTONS.with(Cell::get);
    let state = selection().and_then(state_of).map(|(e, _)| e);
    unsafe {
        EnableWindow(cancel, (state == Some(TicketState::Waiting)) as i32);
        EnableWindow(force, (state == Some(TicketState::Waiting)) as i32);
        EnableWindow(release, matches!(state, Some(TicketState::Active | TicketState::Orphaned)) as i32);
    }
}

unsafe fn cell_text(list: HWND, line: i32, column: i32, text: &str) {
    let mut t = wide(text);
    unsafe {
        let mut item: LVITEMW = std::mem::zeroed();
        item.iSubItem = column;
        item.pszText = t.as_mut_ptr();
        SendMessageW(list, LVM_SETITEMTEXTW, line as usize, &item as *const _ as isize);
    }
}

fn refresh(h: HWND) {
    let Some(p) = shared() else { return };
    let status = p.snapshot();
    let lines = log_tail(&p.dir(), LOG_LINES);
    let list = LIST.with(Cell::get);
    let chosen = selection();
    STATUS.with(|s| *s.borrow_mut() = Some(status.clone()));
    unsafe {
        SendMessageW(list, WM_SETREDRAW, 0, 0);
        SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        for (i, t) in status.tickets.iter().enumerate() {
            let mut number = wide(&format!("#{}", t.id));
            let mut item: LVITEMW = std::mem::zeroed();
            item.mask = LVIF_TEXT | LVIF_PARAM;
            item.iItem = i as i32;
            item.pszText = number.as_mut_ptr();
            item.lParam = t.id as isize;
            if chosen == Some(t.id) {
                item.mask |= LVIF_STATE;
                item.state = LVIS_SELECTED | LVIS_FOCUSED;
                item.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
            }
            let line = SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as isize) as i32;
            let since = t.granted.unwrap_or(t.created);
            let mut state = state_label(t.state);
            if t.lease_overrun {
                state += &tr!(" (lease overrun)", " (bail dépassé)");
            }
            if t.force {
                state += &tr!(" (forced)", " (forcé)");
            }
            if t.manual {
                state += &tr!(" (manual)", " (manuel)");
            }
            let cells = [
                t.session.clone(),
                t.project.clone().unwrap_or_else(|| "—".into()),
                t.branch.clone().unwrap_or_else(|| "—".into()),
                state,
                t.side.name().to_owned(),
                gb(t.need.ram_mb as i64),
                cores(t.need.cpu as i64),
                duration(t.need.duration_s),
                hhmm(since),
                t.message.clone(),
            ];
            for (c, text) in cells.iter().enumerate() {
                cell_text(list, line, c as i32 + 1, text);
            }
        }
        SendMessageW(list, WM_SETREDRAW, 1, 0);
        InvalidateRect(list, std::ptr::null(), 1);

        let text = lines.join("\r\n");
        let change = LOG_TEXT.with(|j| {
            let mut j = j.borrow_mut();
            let change = *j != text;
            *j = text.clone();
            change
        });
        if change {
            let log = LOG.with(Cell::get);
            SetWindowTextW(log, wide(&text).as_ptr());
            SendMessageW(log, WM_VSCROLL, SB_BOTTOM as usize, 0);
        }
        let [_, _, _, freeze, _] = BUTTONS.with(Cell::get);
        let freeze_text = if status.manual_freeze.is_some() {
            tr!("Unfreeze the queue", "Dégeler la file")
        } else {
            tr!("Freeze the queue", "Geler la file")
        };
        SetWindowTextW(freeze, wide(&freeze_text).as_ptr());
        let waiting = status.tickets.iter().filter(|t| t.state == TicketState::Waiting).count();
        let active = status.tickets.len() - waiting;
        let frozen = if status.manual_freeze.is_some() { tr!(" — QUEUE FROZEN", " — FILE GELÉE") } else { String::new() };
        let title = tr!(
            "Clearway — {active} active, {waiting} waiting{frozen}",
            "Clearway — {active} actif(s), {waiting} en attente{frozen}"
        );
        SetWindowTextW(h, wide(&title).as_ptr());
        let bars = BARS.with(Cell::get);
        InvalidateRect(h, &bars, 1);
    }
    update_buttons();
}

unsafe fn rectangle(hdc: HDC, r: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(hdc, r, brush);
        DeleteObject(brush as _);
    }
}

unsafe fn text(hdc: HDC, r: &RECT, t: &str) {
    let mut w: Vec<u16> = t.encode_utf16().collect();
    let mut r = *r;
    unsafe { DrawTextW(hdc, w.as_mut_ptr(), w.len() as i32, &mut r, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS) };
}

/// (used, reserved, margin, total) of a reserve, and its caption.
fn values(r: &ReserveView, status: &Status) -> (i64, i64, i64, i64, String) {
    if r.key == "cpu" {
        let load = fresh_wsl(&status.measures, status.now).map(|w| w.load1);
        let used = load.map_or(0, |c| (c as f64).round() as i64).min(r.total);
        let measure = load.map_or_else(|| tr!("load unknown", "charge inconnue"), |c| tr!("load {c:.1}", "charge {c:.1}"));
        let text = tr!(
            "{measure} · reserved {} · margin {} · free {} (of {})",
            "{measure} · réservé {} · marge {} · libre {} (sur {})",
            cores(r.reserve),
            cores(r.margin),
            r.free_text(),
            cores(r.total)
        );
        return (used, r.reserve, r.margin, r.total, text);
    }
    if r.key == "cpu_windows" {
        let used = (status.measures.cpu_pct as f64 * r.total as f64 / 100.0).round() as i64;
        let text = tr!(
            "CPU {:.0}% · reserved {} · margin {} · free {} (of {})",
            "CPU {:.0} % · réservé {} · marge {} · libre {} (sur {})",
            status.measures.cpu_pct,
            cores(r.reserve),
            cores(r.margin),
            r.free_text(),
            cores(r.total)
        );
        return (used, r.reserve, r.margin, r.total, text);
    }
    if r.key == "ram_wsl" && status.measures.vmmem.is_none() {
        return (0, r.reserve, r.margin, r.total, tr!("WSL stopped", "WSL arrêté"));
    }
    let source = match r.key.as_str() {
        "ram_wsl" if fresh_wsl(&status.measures, status.now).is_some() => {
            tr!(" · from MemAvailable", " · selon MemAvailable")
        }
        "ram_wsl" => tr!(" · from 12 GB − vmmemWSL", " · selon 12 Go − vmmemWSL"),
        _ => String::new(),
    };
    let used = (r.total - r.available).max(0);
    let text = tr!(
        "used {} · reserved {} · margin {} · free {} (of {}){source}",
        "utilisé {} · réservé {} · marge {} · libre {} (sur {}){source}",
        gb(used),
        gb(r.reserve),
        gb(r.margin),
        r.free_text(),
        gb(r.total)
    );
    (used, r.reserve, r.margin, r.total, text)
}

unsafe fn draw_bars(hdc: HDC) {
    let Some(status) = STATUS.with(|s| s.borrow().clone()) else { return };
    let zone = BARS.with(Cell::get);
    unsafe {
        let old = SelectObject(hdc, FONT.with(Cell::get) as _);
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, 0x0020_2020);
        let mut x = zone.left + s(115);
        let swatches = [
            (tr!("used", "utilisé"), USED_COLOR),
            (tr!("reserved", "réservé"), RESERVED_COLOR),
            (tr!("margin", "marge"), MARGIN_COLOR),
            (tr!("free", "libre"), FREE_COLOR),
        ];
        for (name, color) in swatches {
            let square = RECT { left: x, top: zone.top + s(5), right: x + s(12), bottom: zone.top + s(17) };
            rectangle(hdc, &square, color);
            text(hdc, &RECT { left: x + s(16), top: zone.top, right: x + s(90), bottom: zone.top + s(22) }, &name);
            x += s(95);
        }
        let bar_width = ((zone.right - zone.left - s(115)) as f32 * 0.42) as i32;
        for (i, r) in status.reserves.iter().enumerate() {
            // Windows reserves first, then WSL ones, a little apart.
            let y = zone.top + s(22) + i as i32 * s(26) + if r.key.starts_with("ram_wsl") || r.key == "cpu" { s(GROUP_GAP) } else { 0 };
            text(hdc, &RECT { left: zone.left, top: y, right: zone.left + s(110), bottom: y + s(26) }, &r.name);
            let bar = RECT { left: zone.left + s(115), top: y + s(5), right: zone.left + s(115) + bar_width, bottom: y + s(21) };
            rectangle(hdc, &bar, FREE_COLOR);
            let (used, reserve, margin, total, legend) = values(r, &status);
            let mut start = bar.left;
            for (v, color) in [(used, USED_COLOR), (reserve, RESERVED_COLOR), (margin, MARGIN_COLOR)] {
                let px = if total > 0 { (v.max(0) as f64 * bar_width as f64 / total as f64) as i32 } else { 0 };
                let end = (start + px).min(bar.right);
                if end > start {
                    rectangle(hdc, &RECT { left: start, top: bar.top, right: end, bottom: bar.bottom }, color);
                }
                start = end;
            }
            let frame = CreateSolidBrush(FRAME_COLOR);
            FrameRect(hdc, &bar, frame);
            DeleteObject(frame as _);
            text(hdc, &RECT { left: bar.right + s(10), top: y, right: zone.right, bottom: y + s(26) }, &legend);
        }
        SelectObject(hdc, old);
    }
}

fn confirm(h: HWND, id: u64, release: bool) {
    let Some((state, what)) = state_of(id) else { return };
    let question = if release {
        let state = state_label(state);
        tr!(
            "Release the slot of ticket #{id} ({state})?\n\n{what}\n\nIts reservation goes away: if the task is still running, the machine may saturate.",
            "Libérer le créneau du ticket #{id} ({state}) ?\n\n{what}\n\nSa réservation disparaît : si la tâche tourne encore, la machine peut saturer."
        )
    } else {
        tr!("Cancel request #{id}?\n\n{what}", "Annuler la demande #{id} ?\n\n{what}")
    };
    let title = wide("Clearway");
    let response = unsafe { MessageBoxW(h, wide(&question).as_ptr(), title.as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) };
    if response == IDYES
        && let Some(p) = shared()
    {
        p.close_ticket(id, &Release::default(), Closing::Window);
    }
}

fn confirm_force(h: HWND, id: u64) {
    let Some(p) = shared() else { return };
    let Some(question) = p.force_preview(id) else { return };
    let question = tr!("{question}\n\nThe other tickets keep their place.", "{question}\n\nLes autres tickets gardent leur place.");
    let title = wide(&tr!("Force through", "Forcer le passage"));
    let response = unsafe {
        MessageBoxW(h, wide(&question).as_ptr(), title.as_ptr(), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2)
    };
    if response == IDYES
        && let Err(e) = p.force_through(id)
    {
        unsafe { MessageBoxW(h, wide(&e).as_ptr(), title.as_ptr(), MB_ICONWARNING) };
    }
}

unsafe extern "system" fn procedure(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                MAIN.with(|p| p.set(h));
                create_children(h);
                lay_out(h);
                refresh(h);
                SetTimer(h, TIMER, 2000, None);
                0
            }
            WM_SIZE => {
                if !LIST.with(Cell::get).is_null() {
                    lay_out(h);
                }
                0
            }
            WM_TIMER => {
                refresh(h);
                0
            }
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(h, &mut ps);
                draw_bars(hdc);
                EndPaint(h, &ps);
                0
            }
            WM_PRINTCLIENT => {
                let hdc = wparam as HDC;
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(h, &mut rc);
                FillRect(hdc, &rc, (15 + 1) as HBRUSH);
                draw_bars(hdc);
                0
            }
            WM_NOTIFY => {
                if wparam == ID_LIST && lparam != 0 {
                    let header = &*(lparam as *const NMHDR);
                    if header.idFrom == ID_LIST && header.code == LVN_ITEMCHANGED {
                        update_buttons();
                    }
                }
                0
            }
            WM_COMMAND => {
                match wparam & 0xffff {
                    ID_CANCEL => {
                        if let Some(id) = selection() {
                            confirm(h, id, false);
                        }
                    }
                    ID_FREE => {
                        if let Some(id) = selection() {
                            confirm(h, id, true);
                        }
                    }
                    ID_FORCE => {
                        if let Some(id) = selection() {
                            confirm_force(h, id);
                        }
                    }
                    ID_FREEZE => {
                        let frozen = STATUS.with(|s| s.borrow().as_ref().is_some_and(|s| s.manual_freeze.is_some()));
                        if let Some(p) = shared() {
                            p.freeze(!frozen, &tr!("window", "fenêtre"));
                        }
                    }
                    ID_LOG_BUTTON => ui::open_log(),
                    _ => return 0,
                }
                refresh(h);
                0
            }
            WM_CLOSE => {
                DestroyWindow(h);
                0
            }
            WM_DESTROY => {
                KillTimer(h, TIMER);
                let font = FONT.with(|p| p.replace(std::ptr::null_mut()));
                if !font.is_null() {
                    DeleteObject(font as _);
                }
                MAIN.with(|p| p.set(std::ptr::null_mut()));
                LIST.with(|l| l.set(std::ptr::null_mut()));
                STATUS.with(|s| *s.borrow_mut() = None);
                LOG_TEXT.with(|j| *j.borrow_mut() = String::new());
                0
            }
            _ => DefWindowProcW(h, msg, wparam, lparam),
        }
    }
}
