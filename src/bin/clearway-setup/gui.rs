//! The setup window: a header band, then four pages in the same window (looking, choosing, installing, done).

use std::cell::RefCell;

use clearway::tr;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FW_SEMIBOLD, FillRect, GetDC, GetDeviceCaps, GetStockObject, HDC,
    InvalidateRect, LOGPIXELSY, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
    WHITE_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, EM_REPLACESEL, EM_SETSEL, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, PBM_GETPOS, PBM_SETMARQUEE, PBM_SETPOS, PBM_SETRANGE32, PBS_MARQUEE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW,
    DI_NORMAL, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY,
    GWL_EXSTYLE, GWL_STYLE, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HICON, IDC_ARROW, IMAGE_ICON,
    IsDialogMessageW, KillTimer, LR_DEFAULTCOLOR, LWA_ALPHA, LoadCursorW, LoadIconW, LoadImageW, MSG, MoveWindow,
    PostMessageW, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SWP_NOZORDER,
    SendMessageW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_PAINT,
    WM_SETFONT, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_EX_LAYERED,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::windows::{Action, Event, Row, daily_check_enabled, install, plan};

const WIDTH: i32 = 720;
const HEADER: i32 = 92;
const MARGIN: i32 = 28;
const TOP: i32 = HEADER + 22;
const ROWS_TOP: i32 = TOP + 96;
const ROW_STEP: i32 = 50;
const LOOKING_HEIGHT: i32 = 330;
const BUTTON_HEIGHT: i32 = 32;

const WM_PLANNED: u32 = WM_APP + 1;
const WM_EVENT: u32 = WM_APP + 2;
const WM_FINISHED: u32 = WM_APP + 3;
const FADE_TIMER: usize = 1;

const ID_DAILY: usize = 11;
const ID_INSTALL: usize = 12;
const ID_CANCEL: usize = 13;
const ID_CLOSE: usize = 14;
const ID_DETAILS: usize = 15;
const ID_ROW: usize = 100;

const NAVY: (u8, u8, u8) = (0x10, 0x2a, 0x43);
const TEAL: (u8, u8, u8) = (0x17, 0x6f, 0x6a);
const GREEN: COLORREF = rgb(0x1e, 0x8e, 0x4e);
const ORANGE: COLORREF = rgb(0xc8, 0x6a, 0x10);
const GREY: COLORREF = rgb(0x6b, 0x72, 0x80);
const INK: COLORREF = rgb(0x1f, 0x29, 0x37);

const BODY: usize = 0;
const BOLD: usize = 1;
const TITLE: usize = 2;
const SUBTITLE: usize = 3;
const GLYPH: usize = 4;
const SMALL: usize = 5;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

#[derive(Default)]
struct Ui {
    hwnd: usize,
    scale: f32,
    fonts: [usize; 6],
    icon: usize,
    page_title: usize,
    text: usize,
    daily: usize,
    install: usize,
    cancel: usize,
    progress: usize,
    log: usize,
    glyph: usize,
    details: usize,
    close: usize,
    /// One control per row: a checkbox, or a grey label for an information row.
    checks: Vec<usize>,
    descriptions: Vec<usize>,
    rows: Vec<Row>,
    buttons_y: i32,
    installing: bool,
    success: bool,
    alpha: u8,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn s(v: i32) -> i32 {
    UI.with(|u| (v as f32 * u.borrow().scale).round() as i32)
}

fn h(x: usize) -> HWND {
    x as HWND
}

fn with<T>(f: impl FnOnce(&Ui) -> T) -> T {
    UI.with(|u| f(&u.borrow()))
}

unsafe fn make_font(size: i32, weight: u32, face: &str) -> usize {
    let name = wide(face);
    unsafe { CreateFontW(-s(size), 0, 0, 0, weight as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, name.as_ptr()) as usize }
}

#[allow(clippy::too_many_arguments)]
unsafe fn child(parent: HWND, ex: u32, class: &str, text: &str, style: u32, id: usize, font: usize, rect: (i32, i32, i32, i32)) -> usize {
    let class = wide(class);
    let text = wide(text);
    unsafe {
        let c = CreateWindowExW(
            ex,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | style,
            s(rect.0),
            s(rect.1),
            s(rect.2),
            s(rect.3),
            parent,
            id as _,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        SendMessageW(c, WM_SETFONT, with(|u| u.fonts[font]), 1);
        c as usize
    }
}

fn place(control: usize, rect: (i32, i32, i32, i32)) {
    unsafe { MoveWindow(h(control), s(rect.0), s(rect.1), s(rect.2), s(rect.3), 1) };
}

fn set_text(control: usize, text: &str) {
    let t = wide(text);
    unsafe { SetWindowTextW(h(control), t.as_ptr()) };
}

fn show(controls: &[usize], visible: bool) {
    for c in controls {
        unsafe { ShowWindow(h(*c), if visible { SW_SHOW } else { SW_HIDE }) };
    }
}

/// Gives the window a client height of `height` (unscaled), centred on the screen.
fn resize(height: i32) {
    let hwnd = with(|u| u.hwnd);
    unsafe {
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rc = RECT { left: 0, top: 0, right: s(WIDTH), bottom: s(height) };
        AdjustWindowRectEx(&mut rc, style, 0, WS_EX_CONTROLPARENT);
        let (w, hgt) = (rc.right - rc.left, rc.bottom - rc.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
        let y = ((GetSystemMetrics(SM_CYSCREEN) - hgt) / 2).max(0);
        SetWindowPos(h(hwnd), std::ptr::null_mut(), x, y, w, hgt, SWP_NOZORDER);
    }
}

fn append_log(line: &str) {
    let log = with(|u| u.log);
    let text = wide(&format!("{line}\r\n"));
    unsafe {
        SendMessageW(h(log), EM_SETSEL, usize::MAX >> 1, (usize::MAX >> 1) as isize);
        SendMessageW(h(log), EM_REPLACESEL, 0, text.as_ptr() as isize);
    }
}

fn is_checked(control: usize) -> bool {
    unsafe { SendMessageW(h(control), BM_GETCHECK, 0, 0) == BST_CHECKED as isize }
}

/// One checkbox per action (its description below in grey), one grey label per information; the window fits them.
fn create_rows(rows: Vec<Row>) {
    let hwnd = h(with(|u| u.hwnd));
    let inner = WIDTH - 2 * MARGIN;
    let mut checks = Vec::new();
    let mut descriptions = Vec::new();
    let mut y = ROWS_TOP;
    for (i, row) in rows.iter().enumerate() {
        unsafe {
            let check = if row.action.is_some() {
                let c = child(hwnd, 0, "BUTTON", &row.label, WS_TABSTOP | BS_AUTOCHECKBOX as u32, ID_ROW + i, BODY, (MARGIN, y, inner, 22));
                SendMessageW(h(c), BM_SETCHECK, if row.checked { BST_CHECKED } else { BST_UNCHECKED } as usize, 0);
                if row.locked {
                    EnableWindow(h(c), 0);
                }
                c
            } else {
                child(hwnd, 0, "STATIC", &format!("ⓘ  {}", row.label), 0x80, 0, BODY, (MARGIN + 2, y + 2, inner, 22))
            };
            checks.push(check);
            descriptions.push(child(hwnd, 0, "STATIC", &row.detail, 0x80, 0, SMALL, (MARGIN + 21, y + 23, inner - 21, 18)));
        }
        y += ROW_STEP;
    }
    let daily_y = y + 6;
    let buttons_y = daily_y + 24 + 22;
    let (daily, install, cancel) = with(|u| (u.daily, u.install, u.cancel));
    place(daily, (MARGIN, daily_y, inner, 24));
    place(cancel, (WIDTH - MARGIN - 100, buttons_y, 100, BUTTON_HEIGHT));
    place(install, (WIDTH - MARGIN - 100 - 10 - 120, buttons_y, 120, BUTTON_HEIGHT));
    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.rows = rows;
        u.checks = checks;
        u.descriptions = descriptions;
        u.buttons_y = buttons_y;
    });
    resize(buttons_y + BUTTON_HEIGHT + MARGIN);
}

/// A row ticked or unticked: what needs it follows (a plugin without its distribution's client cannot work).
fn row_clicked(index: usize) {
    let (checks, needing) = with(|u| {
        let needing: Vec<usize> = u.rows.iter().enumerate().filter(|(_, r)| r.needs == Some(index)).map(|(i, _)| i).collect();
        (u.checks.clone(), needing)
    });
    let Some(&control) = checks.get(index) else { return };
    let on = is_checked(control);
    UI.with(|u| u.borrow_mut().rows[index].checked = on);
    for i in needing {
        unsafe {
            SendMessageW(h(checks[i]), BM_SETCHECK, if on { BST_CHECKED } else { BST_UNCHECKED } as usize, 0);
            EnableWindow(h(checks[i]), i32::from(on));
        }
        UI.with(|u| u.borrow_mut().rows[i].checked = on);
    }
}

fn show_choosing() {
    let (page_title, text, daily, install, cancel, progress, checks, descriptions) =
        with(|u| (u.page_title, u.text, u.daily, u.install, u.cancel, u.progress, u.checks.clone(), u.descriptions.clone()));
    show(&[progress], false);
    set_text(page_title, &tr!("What will be installed", "Ce qui va être installé"));
    set_text(text, &tr!(
        "Clearway lets your AI sessions run their heavy tasks one after the other, so that the computer does not freeze. Everything is installed for your account only, without administrator rights.",
        "Clearway fait passer les tâches lourdes de vos sessions d'IA chacune à son tour, pour que l'ordinateur ne sature pas. Tout s'installe pour votre compte seulement, sans droits d'administrateur."
    ));
    show(&[daily, install, cancel], true);
    show(&checks, true);
    show(&descriptions, true);
    unsafe { SetFocus(h(install)) };
}

fn start_install() {
    let (hwnd, daily, actions, progress, log, buttons_y) = with(|u| {
        let actions: Vec<Action> = u.rows.iter().filter(|r| r.checked).filter_map(|r| r.action.clone()).collect();
        (u.hwnd, is_checked(u.daily), actions, u.progress, u.log, u.buttons_y)
    });
    let (page_title, text, daily_box, install_button, cancel, checks, descriptions) =
        with(|u| (u.page_title, u.text, u.daily, u.install, u.cancel, u.checks.clone(), u.descriptions.clone()));
    UI.with(|u| u.borrow_mut().installing = true);
    show(&[daily_box, install_button, cancel], false);
    show(&checks, false);
    show(&descriptions, false);
    set_text(page_title, &tr!("Installing…", "Installation en cours…"));
    set_text(text, &tr!("Please wait: this takes about a minute.", "Patientez : cela prend environ une minute."));
    unsafe {
        SendMessageW(h(progress), PBM_SETMARQUEE, 0, 0);
        let style = GetWindowLongPtrW(h(progress), GWL_STYLE);
        SetWindowLongPtrW(h(progress), GWL_STYLE, style & !(PBS_MARQUEE as isize));
        SendMessageW(h(progress), PBM_SETRANGE32, 0, (actions.len() * 100) as isize);
        SendMessageW(h(progress), PBM_SETPOS, 0, 0);
    }
    place(progress, (MARGIN, TOP + 92, WIDTH - 2 * MARGIN, 8));
    place(log, (MARGIN, TOP + 116, WIDTH - 2 * MARGIN, buttons_y + BUTTON_HEIGHT - TOP - 116));
    show(&[progress, log], true);
    std::thread::spawn(move || {
        let post = |e: Event| unsafe {
            PostMessageW(hwnd as HWND, WM_EVENT, 0, Box::into_raw(Box::new(e)) as isize);
        };
        let failures = Box::new(install(&actions, daily, &post));
        unsafe { PostMessageW(hwnd as HWND, WM_FINISHED, 0, Box::into_raw(failures) as isize) };
    });
}

fn on_event(e: Event) {
    let (page_title, progress) = with(|u| (u.page_title, u.progress));
    match e {
        Event::Step { index, total, title } => {
            set_text(page_title, &format!("{title}…"));
            append_log(&format!("── {title} ({}/{total})", index + 1));
            unsafe { SendMessageW(h(progress), PBM_SETPOS, index * 100 + 15, 0) };
        }
        Event::Line(line) => append_log(&line),
        Event::StepDone { ok } => {
            let pos = unsafe { SendMessageW(h(progress), PBM_GETPOS, 0, 0) } as usize;
            unsafe { SendMessageW(h(progress), PBM_SETPOS, (pos / 100 + 1) * 100, 0) };
            if !ok {
                append_log(&tr!("   failed", "   échec"));
            }
        }
    }
}

fn on_finished(failures: Vec<String>) {
    let (page_title, text, glyph, log, progress, details, close, buttons_y) =
        with(|u| (u.page_title, u.text, u.glyph, u.log, u.progress, u.details, u.close, u.buttons_y));
    let success = failures.is_empty();
    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.success = success;
        u.installing = false;
    });
    show(&[log, progress], false);
    let inner = WIDTH - 2 * MARGIN;
    place(glyph, (MARGIN, TOP - 6, 56, 60));
    place(page_title, (MARGIN + 64, TOP + 4, inner - 64, 30));
    place(text, (MARGIN + 64, TOP + 42, inner - 64, buttons_y - TOP - 56));
    place(details, (MARGIN, buttons_y, 170, BUTTON_HEIGHT));
    place(close, (WIDTH - MARGIN - 100, buttons_y, 100, BUTTON_HEIGHT));
    place(log, (MARGIN, TOP + 70, inner, buttons_y - TOP - 84));
    set_text(glyph, if success { "✔" } else { "!" });
    if success {
        set_text(page_title, &tr!("Clearway is installed", "Clearway est installé"));
        set_text(text, &tr!(
            "Its icon is next to the clock. Restart your Claude Code and Codex sessions so that they load the plugin.\r\n\r\nIn Codex, type /hooks once and trust Clearway's hook.\r\n\r\nTo look for a new version later: right-click the icon, then \"Check for updates\".",
            "Son icône est à côté de l'horloge. Relancez vos sessions Claude Code et Codex pour qu'elles chargent le plugin.\r\n\r\nDans Codex, tapez /hooks une fois et approuvez le hook de Clearway.\r\n\r\nPour chercher une nouvelle version plus tard : clic droit sur l'icône, puis « Rechercher les mises à jour »."
        ));
    } else {
        set_text(page_title, &tr!("Installation incomplete", "Installation incomplète"));
        set_text(text, &tr!(
            "These steps failed: {}.\r\n\r\nThe details show why. You can run this setup again once the cause is fixed.",
            "Ces étapes ont échoué : {}.\r\n\r\nLes détails indiquent pourquoi. Vous pouvez relancer cette installation une fois la cause corrigée.",
            failures.join(", ")
        ));
    }
    show(&[glyph, details, close], true);
    unsafe { SetFocus(h(close)) };
}

unsafe fn create(hwnd: HWND) {
    unsafe {
        UI.with(|u| u.borrow_mut().hwnd = hwnd as usize);
        let fonts = [
            make_font(15, FW_NORMAL, "Segoe UI"),
            make_font(20, FW_SEMIBOLD, "Segoe UI"),
            make_font(28, FW_SEMIBOLD, "Segoe UI"),
            make_font(15, FW_NORMAL, "Segoe UI"),
            make_font(44, FW_NORMAL, "Segoe UI Symbol"),
            make_font(13, FW_NORMAL, "Segoe UI"),
        ];
        let icon = LoadImageW(GetModuleHandleW(std::ptr::null()), 1 as _, IMAGE_ICON, s(48), s(48), LR_DEFAULTCOLOR) as usize;
        UI.with(|u| {
            let mut u = u.borrow_mut();
            u.fonts = fonts;
            u.icon = icon;
        });
        let inner = WIDTH - 2 * MARGIN;
        let page_title = child(hwnd, 0, "STATIC", &tr!("Looking at this computer…", "Recherche sur cet ordinateur…"), WS_VISIBLE | 0x80, 0, BOLD, (MARGIN, TOP, inner, 30));
        let text = child(
            hwnd,
            0,
            "STATIC",
            &tr!("Clearway looks for WSL, Claude Code and Codex. Nothing is changed yet.", "Clearway cherche WSL, Claude Code et Codex. Rien n'est encore modifié."),
            WS_VISIBLE | 0x80,
            0,
            BODY,
            (MARGIN, TOP + 36, inner, 44),
        );
        let progress = child(hwnd, 0, "msctls_progress32", "", WS_VISIBLE | PBS_MARQUEE, 0, BODY, (MARGIN, TOP + 92, inner, 8));
        SendMessageW(h(progress), PBM_SETMARQUEE, 1, 30);
        let daily = child(
            hwnd,
            0,
            "BUTTON",
            &tr!("Look for new versions once a day (asks GitHub; nothing installs by itself)", "Chercher les nouvelles versions une fois par jour (interroge GitHub ; rien ne s'installe tout seul)"),
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            ID_DAILY,
            BODY,
            (MARGIN, 0, inner, 24),
        );
        if daily_check_enabled() {
            SendMessageW(h(daily), BM_SETCHECK, BST_CHECKED as usize, 0);
        }
        let cancel = child(hwnd, 0, "BUTTON", &tr!("Cancel", "Annuler"), WS_TABSTOP | BS_PUSHBUTTON as u32, ID_CANCEL, BODY, (0, 0, 100, BUTTON_HEIGHT));
        let install = child(hwnd, 0, "BUTTON", &tr!("Install", "Installer"), WS_TABSTOP | BS_DEFPUSHBUTTON as u32, ID_INSTALL, BODY, (0, 0, 120, BUTTON_HEIGHT));
        let log = child(hwnd, WS_EX_CLIENTEDGE, "EDIT", "", WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32, 0, SMALL, (0, 0, 10, 10));
        let glyph = child(hwnd, 0, "STATIC", "", 0x80, 0, GLYPH, (0, 0, 10, 10));
        let details = child(hwnd, 0, "BUTTON", &tr!("Show details", "Afficher les détails"), WS_TABSTOP | BS_PUSHBUTTON as u32, ID_DETAILS, BODY, (0, 0, 170, BUTTON_HEIGHT));
        let close = child(hwnd, 0, "BUTTON", &tr!("Close", "Fermer"), WS_TABSTOP | BS_DEFPUSHBUTTON as u32, ID_CLOSE, BODY, (0, 0, 100, BUTTON_HEIGHT));
        UI.with(|u| {
            let mut u = u.borrow_mut();
            (u.page_title, u.text, u.progress, u.daily, u.cancel, u.install) = (page_title, text, progress, daily, cancel, install);
            (u.log, u.glyph, u.details, u.close) = (log, glyph, details, close);
        });
    }
}

fn start_planning(hwnd: HWND) {
    let target = hwnd as usize;
    std::thread::spawn(move || {
        let rows = Box::new(plan());
        unsafe { PostMessageW(target as HWND, WM_PLANNED, 0, Box::into_raw(rows) as isize) };
    });
}

unsafe fn paint_header(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let dc: HDC = BeginPaint(hwnd, &mut ps);
        let width = s(WIDTH);
        let height = s(HEADER);
        let steps = 64;
        for i in 0..steps {
            let t = i as f32 / (steps - 1) as f32;
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
            let brush = CreateSolidBrush(rgb(mix(NAVY.0, TEAL.0), mix(NAVY.1, TEAL.1), mix(NAVY.2, TEAL.2)));
            let rc = RECT { left: width * i / steps, top: 0, right: width * (i + 1) / steps + 1, bottom: height };
            FillRect(dc, &rc, brush);
            DeleteObject(brush as _);
        }
        let (icon, title_font, subtitle_font) = with(|u| (u.icon, u.fonts[TITLE], u.fonts[SUBTITLE]));
        DrawIconEx(dc, s(MARGIN), (height - s(48)) / 2, icon as HICON, s(48), s(48), 0, std::ptr::null_mut(), DI_NORMAL);
        SetBkMode(dc, TRANSPARENT as i32);
        let text = |content: &str, f: usize, color: COLORREF, top: i32| {
            SelectObject(dc, f as _);
            SetTextColor(dc, color);
            let mut rc = RECT { left: s(MARGIN + 64), top: s(top), right: width - s(MARGIN), bottom: s(top + 40) };
            let w = wide(content);
            DrawTextW(dc, w.as_ptr(), -1, &mut rc, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);
        };
        text("Clearway", title_font, rgb(0xff, 0xff, 0xff), 18);
        text(&tr!("Setup · version {}", "Installation · version {}", env!("CARGO_PKG_VERSION")), subtitle_font, rgb(0xc9, 0xe6, 0xe2), 54);
        EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint_header(hwnd);
                0
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let dc = wparam as HDC;
                let control = lparam as usize;
                let color = with(|u| {
                    if control == u.glyph {
                        if u.success { GREEN } else { ORANGE }
                    } else if u.descriptions.contains(&control)
                        || u.checks.iter().zip(&u.rows).any(|(c, r)| *c == control && r.action.is_none())
                    {
                        GREY
                    } else {
                        INK
                    }
                });
                SetBkColor(dc, rgb(0xff, 0xff, 0xff));
                SetTextColor(dc, color);
                GetStockObject(WHITE_BRUSH) as LRESULT
            }
            WM_TIMER if wparam == FADE_TIMER => {
                let alpha = UI.with(|u| {
                    let mut u = u.borrow_mut();
                    u.alpha = u.alpha.saturating_add(32);
                    u.alpha
                });
                SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
                if alpha == 255 {
                    KillTimer(hwnd, FADE_TIMER);
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED as isize));
                    InvalidateRect(hwnd, std::ptr::null(), 1);
                }
                0
            }
            WM_PLANNED => {
                create_rows(*Box::from_raw(lparam as *mut Vec<Row>));
                show_choosing();
                InvalidateRect(hwnd, std::ptr::null(), 1);
                0
            }
            WM_EVENT => {
                on_event(*Box::from_raw(lparam as *mut Event));
                0
            }
            WM_FINISHED => {
                on_finished(*Box::from_raw(lparam as *mut Vec<String>));
                0
            }
            WM_COMMAND => {
                match wparam & 0xffff {
                    ID_INSTALL => start_install(),
                    ID_CANCEL | ID_CLOSE => {
                        DestroyWindow(hwnd);
                    }
                    ID_DETAILS => {
                        let (log, text, details) = with(|u| (u.log, u.text, u.details));
                        show(&[text], false);
                        show(&[log], true);
                        EnableWindow(h(details), 0);
                    }
                    id if id >= ID_ROW => row_clicked(id - ID_ROW),
                    _ => {}
                }
                0
            }
            // No closing in the middle of an installation.
            WM_CLOSE if with(|u| u.installing) => 0,
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

pub fn run() -> i32 {
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc);
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("ClearwaySetup");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(procedure);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        wc.hIcon = LoadIconW(instance, 1 as _);
        wc.hbrBackground = GetStockObject(WHITE_BRUSH) as _;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        RegisterClassW(&wc);

        let dc = GetDC(std::ptr::null_mut());
        let scale = GetDeviceCaps(dc, LOGPIXELSY as i32) as f32 / 96.0;
        ReleaseDC(std::ptr::null_mut(), dc);
        UI.with(|u| u.borrow_mut().scale = scale);
        let title = wide(&tr!("Clearway setup", "Installation de Clearway"));
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT | WS_EX_LAYERED,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
            0,
            s(WIDTH),
            s(LOOKING_HEIGHT),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return 1;
        }
        create(hwnd);
        resize(LOOKING_HEIGHT);
        SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
        ShowWindow(hwnd, SW_SHOW);
        SetTimer(hwnd, FADE_TIMER, 15, None);
        start_planning(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        for f in with(|u| u.fonts) {
            DeleteObject(f as _);
        }
        i32::from(!with(|u| u.success))
    }
}
