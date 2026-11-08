//! Native Clearway dashboard: ticket strips, a resource sidebar and a readable activity feed.
//! The list boxes retain scrolling, keyboard selection and accessible text; only their drawing is customized.
use std::cell::{Cell, RefCell};
use std::sync::{Mutex, Once};

use clearway::format::{cores, duration, gb};
use clearway::model::{Release, Status, TicketState, TicketView};
use clearway::scheduler::fresh_wsl;
use clearway::system::hhmm;
use clearway::tr;
use clearway::visual::{self as v, Canvas, Fonts, inset, rect, wide};
use windows_sys::Win32::Foundation::{HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
    ACTCTXW, ActivateActCtx, CreateActCtxW, DeactivateActCtx, ReleaseActCtx,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, MEASUREITEMSTRUCT, ODS_FOCUS,
    ODS_NOFOCUSRECT, ODS_SELECTED,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::server::{Closing, log_tail};
use crate::ui::{self, shared};

const ID_LIST: usize = 100;
const ID_CANCEL: usize = 101;
const ID_FREE: usize = 102;
const ID_FREEZE: usize = 103;
const ID_LOG_BUTTON: usize = 104;
const ID_LOG: usize = 105;
const ID_FORCE: usize = 106;
const ID_SESSIONS: usize = 107;
const TIMER: usize = 1;
const CARD_HEIGHT: i32 = 124;
const SINGLE: u32 = DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS;
const WRAP: u32 = DT_LEFT | DT_WORDBREAK | DT_END_ELLIPSIS;

#[derive(Clone, Copy, Default)]
struct Layout {
    left: i32,
    width: i32,
    queue_top: i32,
    queue_height: i32,
    activity_top: i32,
    side: i32,
    side_width: i32,
}

thread_local! {
    static MAIN: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static LIST: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static BUTTONS: Cell<[HWND; 5]> = const { Cell::new([std::ptr::null_mut(); 5]) };
    static LOG: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static SESSIONS: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static DETAIL: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static ALERT: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static STATUS: RefCell<Option<Status>> = const { RefCell::new(None) };
    static LOG_LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static LAYOUT: Cell<Layout> = const { Cell::new(Layout { left: 0, width: 0, queue_top: 0, queue_height: 0, activity_top: 0, side: 0, side_width: 0 }) };
    static DETAIL_TEXT: RefCell<String> = const { RefCell::new(String::new()) };
    static LOG_DETAIL: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn state_label(state: TicketState) -> String {
    match state {
        TicketState::Waiting => tr!("waiting", "en attente"),
        TicketState::Active => tr!("cleared", "en piste"),
        TicketState::Orphaned => tr!("no signal", "sans signal"),
    }
}

static CLASS: Once = Once::new();
static STYLES: Once = Once::new();
/// Styles activation context (handle, activation cookie or 0), released on the UI thread at shutdown.
static CONTEXT: Mutex<Option<(usize, usize)>> = Mutex::new(None);

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
    let Some((h, cookie)) = CONTEXT.lock().ok().and_then(|mut c| c.take()) else {
        return;
    };
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
            wc.hbrBackground = std::ptr::null_mut();
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
            (
                WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                -20000,
                -20000,
            )
        };
        let h = CreateWindowExW(
            ex,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            s(1280),
            s(860),
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

unsafe fn child(parent: HWND, class: &str, name: &str, style: u32, id: usize) -> HWND {
    unsafe {
        let h = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(name).as_ptr(),
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
        FONTS.with(|f| {
            if let Some(f) = f.borrow().as_ref() {
                SendMessageW(h, WM_SETFONT, f.body as usize, 1);
            }
        });
        h
    }
}

unsafe fn create_children(h: HWND) {
    FONTS.with(|f| *f.borrow_mut() = Some(Fonts::new(SCALE.with(Cell::get))));
    unsafe {
        let style =
            WS_TABSTOP | WS_VSCROLL | (LBS_NOTIFY | LBS_HASSTRINGS | LBS_OWNERDRAWFIXED | LBS_NOINTEGRALHEIGHT) as u32;
        LIST.with(|l| {
            l.set(child(
                h,
                "LISTBOX",
                &tr!("Task queue", "File des tâches"),
                style,
                ID_LIST,
            ))
        });
        LOG.with(|l| l.set(child(h, "LISTBOX", &tr!("Activity", "Activité"), style, ID_LOG)));
        SESSIONS.with(|l| {
            l.set(child(
                h,
                "LISTBOX",
                &tr!("Connected sessions", "Sessions reliées"),
                style,
                ID_SESSIONS,
            ))
        });
        DETAIL.with(|l| {
            l.set(child(
                h,
                "EDIT",
                "",
                WS_TABSTOP | WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
                0,
            ))
        });
        ALERT.with(|l| {
            l.set(child(
                h,
                "EDIT",
                "",
                WS_TABSTOP | WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
                0,
            ))
        });
        let button = |label: &str, id| child(h, "BUTTON", label, WS_TABSTOP | BS_OWNERDRAW as u32, id);
        BUTTONS.with(|b| {
            b.set([
                button(&tr!("Cancel request…", "Annuler…"), ID_CANCEL),
                button(&tr!("Release slot…", "Libérer…"), ID_FREE),
                button(&tr!("Force through…", "Forcer…"), ID_FORCE),
                button(&tr!("Freeze queue", "Geler la file"), ID_FREEZE),
                button(&tr!("Journal", "Journal"), ID_LOG_BUTTON),
            ])
        });
    }
}

unsafe fn lay_out(h: HWND) {
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(h, &mut rc);
        let m = s(20);
        let side_width = s(330);
        let side = rc.right - m - side_width;
        let width = side - 2 * m;
        let has_alerts = STATUS.with(|st| st.borrow().as_ref().is_some_and(|st| !st.alerts.is_empty()));
        let alert_height = if has_alerts { s(56) } else { 0 };
        let queue_top = s(124) + alert_height;
        let count = STATUS.with(|st| st.borrow().as_ref().map_or(1, |st| st.tickets.len().max(1))) as i32;
        let queue_height = (count * s(CARD_HEIGHT))
            .min((rc.bottom - s(450) - alert_height).max(s(140)))
            .max(s(CARD_HEIGHT));
        let actions_y = queue_top + queue_height + s(8);
        let activity_top = actions_y + s(162);
        LAYOUT.with(|l| {
            l.set(Layout {
                left: m,
                width,
                queue_top,
                queue_height,
                activity_top,
                side,
                side_width,
            })
        });
        MoveWindow(LIST.with(Cell::get), m, queue_top, width, queue_height, 1);
        MoveWindow(ALERT.with(Cell::get), m + s(8), s(124), width - s(16), s(46), 1);
        ShowWindow(ALERT.with(Cell::get), if has_alerts { SW_SHOW } else { SW_HIDE });
        let [cancel, release, force, freeze, log] = BUTTONS.with(Cell::get);
        for (i, b) in [cancel, release, force].into_iter().enumerate() {
            MoveWindow(b, m + i as i32 * s(140), actions_y, s(132), s(36), 1);
        }
        MoveWindow(freeze, side + side_width - s(248), s(18), s(142), s(36), 1);
        MoveWindow(log, side + side_width - s(98), s(18), s(98), s(36), 1);
        MoveWindow(
            DETAIL.with(Cell::get),
            m + s(12),
            actions_y + s(48),
            width - s(24),
            s(94),
            1,
        );
        MoveWindow(
            LOG.with(Cell::get),
            m + s(12),
            activity_top + s(42),
            width - s(24),
            (rc.bottom - activity_top - s(72)).max(s(32)),
            1,
        );
        MoveWindow(
            SESSIONS.with(Cell::get),
            side + s(14),
            s(580),
            side_width - s(28),
            (rc.bottom - s(608)).max(s(48)),
            1,
        );
        InvalidateRect(h, std::ptr::null(), 0);
    }
}

fn selection() -> Option<u64> {
    let index = unsafe { SendMessageW(LIST.with(Cell::get), LB_GETCURSEL, 0, 0) };
    STATUS.with(|s| {
        s.borrow()
            .as_ref()?
            .tickets
            .get(usize::try_from(index).ok()?)
            .map(|t| t.id)
    })
}

fn state_of(id: u64) -> Option<(TicketState, String)> {
    STATUS.with(|s| {
        s.borrow()
            .as_ref()?
            .tickets
            .iter()
            .find(|t| t.id == id)
            .map(|t| (t.state, format!("{} — {}", t.session, t.description)))
    })
}

fn ticket_text(t: &TicketView) -> String {
    let mut state = state_label(t.state);
    if t.lease_overrun {
        state += &tr!(" · overdue", " · bail dépassé");
    }
    if t.force {
        state += &tr!(" · forced", " · forcé");
    }
    if t.manual {
        state += &tr!(" · manual", " · manuel");
    }
    format!(
        "#{} · {} · {}\r\n{} · {} · {}\r\n{} · {} · {} · {}\r\n{}",
        t.id,
        state,
        t.description,
        t.session,
        t.project.as_deref().unwrap_or("—"),
        t.branch.as_deref().unwrap_or("—"),
        t.side.name(),
        gb(t.need.ram_mb as i64),
        cores(t.need.cpu as i64),
        duration(t.need.duration_s),
        t.message
    )
}

fn detail_text(value: String) {
    let changed = DETAIL_TEXT.with(|d| {
        if *d.borrow() == value {
            return false;
        }
        *d.borrow_mut() = value.clone();
        true
    });
    if changed {
        unsafe { SetWindowTextW(DETAIL.with(Cell::get), wide(&value).as_ptr()) };
    }
}

fn update_buttons() {
    let chosen = selection();
    let state = chosen.and_then(state_of).map(|(e, _)| e);
    let [cancel, release, force, _, _] = BUTTONS.with(Cell::get);
    unsafe {
        EnableWindow(cancel, (state == Some(TicketState::Waiting)) as i32);
        EnableWindow(force, (state == Some(TicketState::Waiting)) as i32);
        EnableWindow(
            release,
            matches!(state, Some(TicketState::Active | TicketState::Orphaned)) as i32,
        );
    }
    let value = STATUS
        .with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|st| st.tickets.iter().find(|t| Some(t.id) == chosen).map(ticket_text))
        })
        .unwrap_or_else(|| {
            tr!(
                "Select a ticket to see its details and available actions.",
                "Sélectionnez un ticket pour lire ses détails et accéder aux actions."
            )
        });
    detail_text(LOG_DETAIL.with(|l| l.borrow().clone()).unwrap_or(value));
}

/// Rebuild only when the accessible strings change; preserve the selection and scroll position.
unsafe fn replace_rows(h: HWND, rows: &[String], chosen: Option<usize>, top: usize) {
    unsafe {
        SendMessageW(h, WM_SETREDRAW, 0, 0);
        SendMessageW(h, LB_RESETCONTENT, 0, 0);
        for row in rows {
            SendMessageW(h, LB_ADDSTRING, 0, wide(row).as_ptr() as isize);
        }
        if let Some(i) = chosen {
            SendMessageW(h, LB_SETCURSEL, i, 0);
        }
        SendMessageW(h, LB_SETTOPINDEX, top.min(rows.len().saturating_sub(1)), 0);
        SendMessageW(h, WM_SETREDRAW, 1, 0);
        InvalidateRect(h, std::ptr::null(), 0);
    }
}

fn refresh(h: HWND) {
    let Some(p) = shared() else { return };
    let status = p.snapshot();
    let lines: Vec<_> = log_tail(&p.dir(), 100).into_iter().rev().collect();
    let chosen = selection();
    let list = LIST.with(Cell::get);
    let rows: Vec<_> = status.tickets.iter().map(ticket_text).collect();
    let sessions: Vec<_> = status
        .links
        .iter()
        .map(|l| format!("{} · {}", l.session, l.client))
        .collect();
    let (queue_changed, sessions_changed, count_changed) = STATUS.with(|s| {
        let s = s.borrow();
        (
            s.as_ref()
                .map(|s| s.tickets.iter().map(ticket_text).collect::<Vec<_>>())
                .as_ref()
                != Some(&rows),
            s.as_ref()
                .map(|s| {
                    s.links
                        .iter()
                        .map(|l| format!("{} · {}", l.session, l.client))
                        .collect::<Vec<_>>()
                })
                .as_ref()
                != Some(&sessions),
            s.as_ref().map(|s| (s.tickets.len(), s.alerts.is_empty()))
                != Some((status.tickets.len(), status.alerts.is_empty())),
        )
    });
    let old_top = unsafe { SendMessageW(list, LB_GETTOPINDEX, 0, 0).max(0) as usize };
    let index = status.tickets.iter().position(|t| Some(t.id) == chosen).or_else(|| {
        if chosen.is_none() && !status.tickets.is_empty() {
            Some(0)
        } else {
            None
        }
    });
    STATUS.with(|s| *s.borrow_mut() = Some(status.clone()));
    unsafe {
        let alerts = status.alerts.join("\r\n");
        SetWindowTextW(ALERT.with(Cell::get), wide(&alerts).as_ptr());
        if queue_changed {
            replace_rows(list, &rows, index, old_top);
        }
        if sessions_changed {
            let h = SESSIONS.with(Cell::get);
            replace_rows(
                h,
                &sessions,
                None,
                SendMessageW(h, LB_GETTOPINDEX, 0, 0).max(0) as usize,
            );
        }
        let log_changed = LOG_LINES.with(|l| *l.borrow() != lines);
        if log_changed {
            let log = LOG.with(Cell::get);
            let top = SendMessageW(log, LB_GETTOPINDEX, 0, 0).max(0) as usize;
            let selected = SendMessageW(log, LB_GETCURSEL, 0, 0);
            // Track the actual row, even as new activity is prepended.
            let (anchor, selected_text) = LOG_LINES.with(|l| {
                (
                    l.borrow().get(top).cloned(),
                    usize::try_from(selected).ok().and_then(|i| l.borrow().get(i).cloned()),
                )
            });
            let new_top = if top == 0 {
                0
            } else {
                anchor.and_then(|a| lines.iter().position(|l| *l == a)).unwrap_or(top)
            };
            let new_selected = selected_text.and_then(|a| lines.iter().position(|l| *l == a));
            LOG_LINES.with(|l| *l.borrow_mut() = lines.clone());
            replace_rows(log, &lines, new_selected, new_top);
        }
        // WM_SETREDRAW(TRUE) can restore WS_VISIBLE; apply empty-state visibility after rebuilding.
        ShowWindow(list, if rows.is_empty() { SW_HIDE } else { SW_SHOW });
        ShowWindow(
            SESSIONS.with(Cell::get),
            if sessions.is_empty() { SW_HIDE } else { SW_SHOW },
        );
        ShowWindow(LOG.with(Cell::get), if lines.is_empty() { SW_HIDE } else { SW_SHOW });
        let frozen = status.manual_freeze.is_some();
        SetWindowTextW(
            BUTTONS.with(Cell::get)[3],
            wide(&if frozen {
                tr!("Resume queue", "Reprendre la file")
            } else {
                tr!("Freeze queue", "Geler la file")
            })
            .as_ptr(),
        );
        if count_changed {
            lay_out(h);
        }
        InvalidateRect(h, std::ptr::null(), 0);
        InvalidateRect(list, std::ptr::null(), 0);
    }
    update_buttons();
}

fn ticket_colors(t: &TicketView) -> (u32, u32) {
    if t.state == TicketState::Orphaned {
        (v::RED_WASH, v::RED)
    } else if t.lease_overrun || t.state == TicketState::Waiting {
        (v::AMBER_WASH, v::AMBER)
    } else {
        (v::GREEN_WASH, v::GREEN_INK)
    }
}

unsafe fn draw_ticket(d: &DRAWITEMSTRUCT, f: &Fonts, t: &TicketView, now: u64) {
    let c = unsafe { Canvas::new(d.hDC) };
    c.fill(d.rcItem, v::BACKGROUND);
    let r = rect(
        d.rcItem.left + s(1),
        d.rcItem.top + s(1),
        d.rcItem.right - d.rcItem.left - s(3),
        s(CARD_HEIGHT - 10),
    );
    c.panel(
        r,
        v::PAPER,
        if d.itemState & ODS_SELECTED != 0 {
            v::GREEN
        } else {
            v::BORDER
        },
        s(10),
    );
    let (wash, ink) = ticket_colors(t);
    let badge = rect(r.left + s(10), r.top + s(12), s(76), s(88));
    c.panel(badge, wash, wash, s(6));
    c.text(
        rect(badge.left, badge.top + s(10), s(76), s(28)),
        &format!("#{}", t.id),
        f.strong,
        ink,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER,
    );
    c.text(
        rect(badge.left + s(3), badge.top + s(44), s(70), s(36)),
        &if t.lease_overrun {
            tr!("Overdue", "Bail dépassé")
        } else {
            state_label(t.state)
        },
        f.small,
        ink,
        DT_CENTER | DT_WORDBREAK,
    );
    let x = r.left + s(100);
    let meta_x = r.right - s(174);
    let w = (meta_x - x - s(16)).max(0);
    c.text(
        rect(x, r.top + s(12), w, s(24)),
        &t.description,
        f.strong,
        v::INK,
        SINGLE,
    );
    c.text(
        rect(x, r.top + s(40), w, s(20)),
        &format!("{} · {}", t.session, t.project.as_deref().unwrap_or("—")),
        f.small,
        v::MUTED,
        SINGLE,
    );
    c.text(
        rect(x, r.top + s(62), w, s(18)),
        t.branch.as_deref().unwrap_or("—"),
        f.mono,
        v::MUTED,
        SINGLE,
    );
    c.text(rect(x, r.top + s(84), w, s(18)), &t.message, f.small, ink, SINGLE);
    c.fill(rect(meta_x - s(10), r.top + s(14), 1, s(84)), v::BORDER);
    c.text(
        rect(meta_x, r.top + s(14), s(160), s(20)),
        &format!("{} · {}", t.side.name(), gb(t.need.ram_mb as i64)),
        f.mono,
        v::INK,
        SINGLE,
    );
    c.text(
        rect(meta_x, r.top + s(38), s(160), s(20)),
        &format!("{} · {}", cores(t.need.cpu as i64), duration(t.need.duration_s)),
        f.small,
        v::MUTED,
        SINGLE,
    );
    let timing = if let Some(start) = t.granted {
        if t.lease_overrun {
            tr!(
                "Overdue by {}",
                "Dépassé de {}",
                duration(now.saturating_sub(start.saturating_add(t.need.duration_s)))
            )
        } else {
            format!("{} → {}", hhmm(start), hhmm(start.saturating_add(t.need.duration_s)))
        }
    } else {
        tr!(
            "Waiting {}",
            "Attend depuis {}",
            duration(now.saturating_sub(t.created))
        )
    };
    c.text(
        rect(meta_x, r.top + s(64), s(160), s(20)),
        &timing,
        f.small,
        ink,
        SINGLE,
    );
    if let Some(start) = t.granted {
        let w = s(148);
        let track = rect(meta_x, r.top + s(94), w, s(4));
        c.fill(track, v::TRACK);
        let used = (now.saturating_sub(start) as f64 / t.need.duration_s.max(1) as f64).clamp(0.0, 1.0);
        c.fill(
            rect(track.left, track.top, (w as f64 * used) as i32, s(4)),
            if t.lease_overrun { v::AMBER } else { v::GREEN },
        );
    }
    if d.itemState & ODS_FOCUS != 0 && d.itemState & ODS_NOFOCUSRECT == 0 {
        c.focus(inset(r, s(4), s(4)));
    }
}

unsafe fn draw_activity(d: &DRAWITEMSTRUCT, f: &Fonts, line: &str) {
    let c = unsafe { Canvas::new(d.hDC) };
    c.fill(
        d.rcItem,
        if d.itemState & ODS_SELECTED != 0 {
            v::BACKGROUND
        } else {
            v::PAPER
        },
    );
    let mut parts = line.splitn(4, ' ');
    let _date = parts.next().unwrap_or("");
    let time = parts.next().unwrap_or("");
    let kind = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or(line);
    let (label, color) = match kind {
        "RELEASED" => (tr!("RELEASED", "LIBÉRÉ"), v::GREEN_INK),
        "CLEARED" => (tr!("CLEARED", "EN PISTE"), v::GREEN_INK),
        "REQUEST" => (tr!("REQUEST", "DEMANDE"), v::MUTED),
        "WAITING" => (tr!("WAITING", "ATTENTE"), v::AMBER),
        "ERROR" => (tr!("ERROR", "ERREUR"), v::RED),
        _ => (kind.to_owned(), v::MUTED),
    };
    let r = d.rcItem;
    c.text(rect(r.left, r.top, s(74), s(32)), time, f.mono, v::MUTED, SINGLE);
    c.text(
        rect(r.left + s(82), r.top, s(90), s(32)),
        &label,
        f.small,
        color,
        SINGLE,
    );
    c.text(
        rect(r.left + s(180), r.top, r.right - r.left - s(180), s(32)),
        body,
        f.small,
        v::INK,
        SINGLE,
    );
    if d.itemState & ODS_FOCUS != 0 {
        c.focus(inset(r, 1, 1));
    }
}

unsafe fn draw(h: HWND, dc: HDC) {
    let c = unsafe { Canvas::new(dc) };
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(h, &mut rc) };
    c.fill(rc, v::BACKGROUND);
    c.fill(rect(0, 0, rc.right, s(72)), v::NAVY);
    FONTS.with(|fonts| {
        let fonts = fonts.borrow();
        let Some(f) = fonts.as_ref() else { return };
        unsafe {
            DrawIconEx(
                dc,
                s(20),
                s(20),
                LoadIconW(GetModuleHandleW(std::ptr::null()), 1 as _),
                s(32),
                s(32),
                0,
                std::ptr::null_mut(),
                DI_NORMAL,
            );
        }
        c.text(rect(s(64), s(14), s(210), s(30)), "Clearway", f.title, v::PAPER, SINGLE);
        c.text(
            rect(s(66), s(44), s(260), s(18)),
            &format!("{} · Windows / WSL", env!("CARGO_PKG_VERSION")),
            f.small,
            v::rgb(0xa9b8c6),
            SINGLE,
        );
        let l = LAYOUT.with(Cell::get);
        c.text(
            rect(l.left, s(86), s(78), s(28)),
            &tr!("Queue", "File"),
            f.title,
            v::INK,
            SINGLE,
        );
        c.panel(
            rect(l.left, l.activity_top, l.width, rc.bottom - l.activity_top - s(20)),
            v::PAPER,
            v::BORDER,
            s(10),
        );
        c.text(
            rect(l.left + s(16), l.activity_top + s(10), l.width - s(32), s(24)),
            &tr!("Activity", "Activité"),
            f.strong,
            v::INK,
            SINGLE,
        );
        c.panel(
            rect(l.left, l.queue_top + l.queue_height + s(50), l.width, s(108)),
            v::PAPER,
            v::BORDER,
            s(10),
        );
        c.panel(rect(l.side, s(88), l.side_width, s(436)), v::PAPER, v::BORDER, s(10));
        c.text(
            rect(l.side + s(16), s(102), l.side_width - s(32), s(24)),
            &tr!("Resources", "Ressources"),
            f.strong,
            v::INK,
            SINGLE,
        );
        c.panel(
            rect(l.side, s(538), l.side_width, rc.bottom - s(558)),
            v::PAPER,
            v::BORDER,
            s(10),
        );
        c.text(
            rect(l.side + s(16), s(548), l.side_width - s(32), s(24)),
            &tr!("Connected sessions", "Sessions reliées"),
            f.strong,
            v::INK,
            SINGLE,
        );
        STATUS.with(|st| {
            let st = st.borrow();
            let Some(st) = st.as_ref() else { return };
            if !st.alerts.is_empty() {
                c.panel(rect(l.left, s(120), l.width, s(54)), v::AMBER_WASH, v::AMBER_WASH, s(8));
            }
            let waiting = st.tickets.iter().filter(|t| t.state == TicketState::Waiting).count();
            let active = st.tickets.iter().filter(|t| t.state == TicketState::Active).count();
            let summary = if st.manual_freeze.is_some() {
                tr!(
                    "Queue paused · {active} active · {waiting} waiting",
                    "File gelée · {active} en piste · {waiting} en attente"
                )
            } else {
                tr!(
                    "{active} active · {waiting} waiting",
                    "{active} en piste · {waiting} en attente"
                )
            };
            c.text(
                rect(l.left + s(86), s(88), l.width - s(86), s(26)),
                &summary,
                f.body,
                if st.manual_freeze.is_some() { v::AMBER } else { v::MUTED },
                SINGLE,
            );
            if st.tickets.is_empty() {
                c.panel(
                    rect(l.left, l.queue_top, l.width, l.queue_height - s(10)),
                    v::PAPER,
                    v::BORDER,
                    s(10),
                );
                c.text(
                    rect(l.left + s(24), l.queue_top + s(22), l.width - s(48), s(28)),
                    &tr!("The queue is clear", "La file est libre"),
                    f.strong,
                    v::INK,
                    SINGLE,
                );
                c.text(
                    rect(l.left + s(24), l.queue_top + s(58), l.width - s(48), s(36)),
                    &tr!(
                        "Requests from your sessions will appear here.",
                        "Les demandes de vos sessions apparaîtront ici."
                    ),
                    f.body,
                    v::MUTED,
                    WRAP,
                );
            }
            for (i, r) in st.reserves.iter().enumerate() {
                let y = s(146 + i as i32 * 84);
                let x = l.side + s(16);
                let width = l.side_width - s(32);
                c.text(rect(x, y, width, s(20)), &r.name, f.body, v::INK, SINGLE);
                let free = if r.key.starts_with("cpu") {
                    cores(r.free.max(0))
                } else {
                    gb(r.free.max(0))
                };
                c.text(
                    rect(x, y, width, s(20)),
                    &tr!("{free} free", "{free} libres"),
                    f.mono,
                    v::INK,
                    DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
                );
                let used = match r.key.as_str() {
                    "cpu_windows" => (st.measures.cpu_pct as f64 * r.total as f64 / 100.0).round() as i64,
                    "cpu" => fresh_wsl(&st.measures, st.now).map_or(0, |w| w.load1.round() as i64),
                    _ => (r.total - r.available).max(0),
                };
                let bar = rect(x, y + s(28), width, s(10));
                c.fill(bar, v::TRACK);
                let mut start = x;
                for (amount, color) in [
                    (
                        used,
                        if r.key == "cpu" || r.key == "ram_wsl" {
                            v::TEAL
                        } else {
                            v::BLUE
                        },
                    ),
                    (r.reserve, v::GREEN),
                    (r.margin, v::MARGIN),
                ] {
                    let pixels = (width as f64 * amount.max(0) as f64 / r.total.max(1) as f64) as i32;
                    let end = (start + pixels).min(bar.right);
                    c.fill(rect(start, bar.top, end - start, s(10)), color);
                    start = end;
                }
                let unit = |n| if r.key.starts_with("cpu") { cores(n) } else { gb(n) };
                let legend = if r.key == "ram_wsl" && st.measures.vmmem.is_none() {
                    tr!("WSL stopped", "WSL arrêté")
                } else if r.key == "cpu" && fresh_wsl(&st.measures, st.now).is_none() {
                    tr!(
                        "WSL load unavailable · {} reserved",
                        "Charge WSL indisponible · {} réservés",
                        unit(r.reserve)
                    )
                } else if r.estimate.is_some() {
                    r.free_text()
                } else {
                    tr!(
                        "{} used · {} reserved · margin {} · of {}",
                        "{} utilisés · {} réservés · marge {} · sur {}",
                        unit(used),
                        unit(r.reserve),
                        unit(r.margin),
                        unit(r.total)
                    )
                };
                c.text(rect(x, y + s(46), width, s(30)), &legend, f.small, v::MUTED, WRAP);
            }
            c.text(
                rect(l.side + s(16), s(490), l.side_width - s(32), s(22)),
                &tr!(
                    "Used / reserved / margin · updated every 2 s",
                    "Utilisé / réservé / marge · actualisé toutes les 2 s"
                ),
                f.small,
                v::MUTED,
                SINGLE,
            );
            if st.links.is_empty() {
                c.text(
                    rect(l.side + s(16), s(590), l.side_width - s(32), s(46)),
                    &tr!("No MCP session connected.", "Aucune session MCP reliée."),
                    f.body,
                    v::MUTED,
                    WRAP,
                );
            }
        });
    });
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
        tr!(
            "Cancel request #{id}?\n\n{what}",
            "Annuler la demande #{id} ?\n\n{what}"
        )
    };
    let title = wide("Clearway");
    let response = unsafe {
        MessageBoxW(
            h,
            wide(&question).as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        )
    };
    if response == IDYES
        && let Some(p) = shared()
    {
        p.close_ticket(id, &Release::default(), Closing::Window);
    }
}

fn confirm_force(h: HWND, id: u64) {
    let Some(p) = shared() else { return };
    let Some(question) = p.force_preview(id) else { return };
    let question = tr!(
        "{question}\n\nThe other tickets keep their place.",
        "{question}\n\nLes autres tickets gardent leur place."
    );
    let title = wide(&tr!("Force through", "Forcer le passage"));
    let response = unsafe {
        MessageBoxW(
            h,
            wide(&question).as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
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
            WM_GETMINMAXINFO => {
                let m = &mut *(lparam as *mut MINMAXINFO);
                m.ptMinTrackSize.x = s(1080);
                m.ptMinTrackSize.y = s(760);
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
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(h, &mut ps);
                draw(h, dc);
                EndPaint(h, &ps);
                0
            }
            WM_PRINTCLIENT => {
                draw(h, wparam as HDC);
                0
            }
            WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
                let dc = wparam as HDC;
                let alert = lparam as HWND == ALERT.with(Cell::get);
                SetTextColor(dc, if alert { v::AMBER } else { v::MUTED });
                // Stock DC brush needs no allocation and is valid until Windows has painted the control.
                let color = if alert {
                    v::AMBER_WASH
                } else if lparam as HWND == LIST.with(Cell::get) {
                    v::BACKGROUND
                } else {
                    v::PAPER
                };
                SetBkColor(dc, color);
                SetDCBrushColor(dc, color);
                GetStockObject(DC_BRUSH) as LRESULT
            }
            WM_MEASUREITEM => {
                let m = &mut *(lparam as *mut MEASUREITEMSTRUCT);
                m.itemHeight = s(match m.CtlID as usize {
                    ID_LIST => CARD_HEIGHT,
                    ID_SESSIONS => 56,
                    _ => 32,
                }) as u32;
                1
            }
            WM_DRAWITEM => {
                let d = &*(lparam as *const DRAWITEMSTRUCT);
                FONTS.with(|fonts| {
                    let fonts = fonts.borrow();
                    let Some(f) = fonts.as_ref() else { return };
                    match d.CtlID as usize {
                        ID_LIST => STATUS.with(|st| {
                            if let Some(st) = st.borrow().as_ref()
                                && let Some(t) = st.tickets.get(d.itemID as usize)
                            {
                                draw_ticket(d, f, t, st.now);
                            }
                        }),
                        ID_LOG => LOG_LINES.with(|l| {
                            if let Some(line) = l.borrow().get(d.itemID as usize) {
                                draw_activity(d, f, line);
                            }
                        }),
                        ID_SESSIONS => STATUS.with(|st| {
                            if let Some(st) = st.borrow().as_ref()
                                && let Some(link) = st.links.get(d.itemID as usize)
                            {
                                let c = Canvas::new(d.hDC);
                                c.fill(d.rcItem, v::PAPER);
                                c.text(
                                    rect(d.rcItem.left, d.rcItem.top, d.rcItem.right - d.rcItem.left, s(26)),
                                    &link.session,
                                    f.body,
                                    v::INK,
                                    SINGLE,
                                );
                                let old = link.client.split_whitespace().last().is_some_and(|version| {
                                    clearway::update::parse_version(version).is_some() && version != st.version
                                });
                                let client = if old {
                                    tr!("{} · restart", "{} · à relancer", link.client)
                                } else {
                                    link.client.clone()
                                };
                                c.text(
                                    rect(
                                        d.rcItem.left,
                                        d.rcItem.top + s(26),
                                        d.rcItem.right - d.rcItem.left,
                                        s(24),
                                    ),
                                    &client,
                                    f.mono,
                                    if old { v::AMBER } else { v::GREEN_INK },
                                    SINGLE,
                                );
                                if d.itemState & ODS_FOCUS != 0 {
                                    c.focus(inset(d.rcItem, 1, 1));
                                }
                            }
                        }),
                        _ => v::button(
                            d,
                            f.body,
                            SCALE.with(Cell::get),
                            d.CtlID as usize == ID_FREE,
                            if matches!(d.CtlID as usize, ID_FREEZE | ID_LOG_BUTTON) {
                                v::NAVY
                            } else {
                                v::BACKGROUND
                            },
                        ),
                    }
                });
                1
            }
            WM_COMMAND => {
                let id = wparam & 0xffff;
                let notification = (wparam >> 16) & 0xffff;
                if id == ID_LIST && notification == LBN_SELCHANGE as usize {
                    LOG_DETAIL.with(|l| *l.borrow_mut() = None);
                    update_buttons();
                    return 0;
                }
                if id == ID_LOG && notification == LBN_SELCHANGE as usize {
                    let i = SendMessageW(LOG.with(Cell::get), LB_GETCURSEL, 0, 0);
                    if let Some(line) =
                        LOG_LINES.with(|l| usize::try_from(i).ok().and_then(|i| l.borrow().get(i).cloned()))
                    {
                        LOG_DETAIL.with(|l| *l.borrow_mut() = Some(line.clone()));
                        detail_text(line);
                    }
                    return 0;
                }
                if notification != 0 {
                    return 0;
                }
                LOG_DETAIL.with(|l| *l.borrow_mut() = None);
                match id {
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
                MAIN.with(|p| p.set(std::ptr::null_mut()));
                LIST.with(|p| p.set(std::ptr::null_mut()));
                FONTS.with(|f| *f.borrow_mut() = None);
                STATUS.with(|s| *s.borrow_mut() = None);
                LOG_LINES.with(|l| l.borrow_mut().clear());
                DETAIL_TEXT.with(|l| l.borrow_mut().clear());
                LOG_DETAIL.with(|l| *l.borrow_mut() = None);
                0
            }
            _ => DefWindowProcW(h, msg, wparam, lparam),
        }
    }
}
