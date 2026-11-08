//! The setup window: a header band, then pages in the same window (maintenance choice, looking, choosing, steps,
//! result, last steps). Controls of a page are created when it opens and destroyed when it closes.

use std::cell::RefCell;

use clearway::setup::{HookTrust, Installed, Sessions, old_sessions_list};
use clearway::tr;
use clearway::visual::{self as v, Canvas, inset, rect};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CreateFontW, DC_BRUSH, DEFAULT_CHARSET, DT_CENTER, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, EndPaint, FW_NORMAL, FW_SEMIBOLD, GetDC, GetDeviceCaps, GetStockObject, HDC,
    InvalidateRect, LOGPIXELSY, PAINTSTRUCT, ReleaseDC, SetBkColor, SetDCBrushColor, SetTextColor, WHITE_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, DRAWITEMSTRUCT, EM_REPLACESEL, EM_SETSEL, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, PBM_SETMARQUEE, PBM_SETPOS, PBM_SETRANGE32, PBS_MARQUEE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, IsWindowEnabled, SetFocus, VK_RETURN};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, BM_CLICK, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_OWNERDRAW,
    CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx, ES_AUTOVSCROLL,
    ES_MULTILINE, ES_READONLY, GWL_EXSTYLE, GWL_STYLE, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    HICON, IDC_ARROW, IMAGE_ICON, IsDialogMessageW, IsWindowVisible, KillTimer, LR_DEFAULTCOLOR, LWA_ALPHA,
    LoadCursorW, LoadIconW, LoadImageW, MSG, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW, SM_CXSCREEN,
    SM_CYSCREEN, SW_HIDE, SW_SHOW, SWP_NOZORDER, SendMessageW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_KEYDOWN, WM_LBUTTONUP, WM_PAINT, WM_SETFONT, WM_TIMER, WNDCLASSW,
    WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_EX_LAYERED, WS_GROUP, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::windows::{
    Action, Checks, Event, Options, Outcome, Row, Tool, check, daily_check_enabled, element, hint, install, installed,
    installed_version, plan, relaunch_from_temp, runs_from_install_dir, uninstall_plan,
};

const WIDTH: i32 = 760;
const HEADER: i32 = 104;
const MARGIN: i32 = 32;
const INNER: i32 = WIDTH - 2 * MARGIN;
const TOP: i32 = HEADER + 22;
const ROWS_TOP: i32 = TOP + 96;
const ROW_STEP: i32 = 50;
const STEP_LINE: i32 = 30;
const LOOKING_HEIGHT: i32 = 330;
const BUTTON_HEIGHT: i32 = 40;

const WM_PLANNED: u32 = WM_APP + 1;
const WM_EVENT: u32 = WM_APP + 2;
const WM_FINISHED: u32 = WM_APP + 3;
const WM_CHECKED: u32 = WM_APP + 4;
const FADE_TIMER: usize = 1;

const ID_OPTION: usize = 11;
/// Left button: Install, Continue, Next, Verify. 12 is also what the test scripts post to click "Install".
const ID_LEFT: usize = 12;
const ID_RIGHT: usize = 13;
const ID_DETAILS: usize = 15;
const ID_UPDATE: usize = 16;
const ID_REMOVE: usize = 17;
const ID_ROW: usize = 100;

const GREEN: COLORREF = v::GREEN_INK;
const ORANGE: COLORREF = v::AMBER;
const RED: COLORREF = v::RED;
const BLUE: COLORREF = v::TEAL;
const GREY: COLORREF = v::MUTED;
const INK: COLORREF = v::INK;

const BODY: usize = 0;
const BOLD: usize = 1;
const TITLE: usize = 2;
const GLYPH: usize = 4;
const SMALL: usize = 5;
const MARK: usize = 6;
const MONO: usize = 7;

const TODO: &str = "○";
const RUNNING: &str = "⏳";
const DONE: &str = "✔";
const FAILED: &str = "✖";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Looking,
    Maintenance,
    Choosing,
    Steps,
    Result,
    Last,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reopen {
    Claude,
    Codex,
    CodexGuard,
}

#[derive(Default)]
struct Ui {
    hwnd: usize,
    scale: f32,
    fonts: [usize; 8],
    installed_label: String,
    footer_y: i32,
    maintenance_remove: bool,
    icon: usize,
    page_title: usize,
    text: usize,
    progress: usize,
    log: usize,
    option: usize,
    left: usize,
    right: usize,
    details: usize,
    page: Page,
    uninstall: bool,
    /// Render the real controls with sample version data; no machine probes or actions.
    preview: bool,
    /// Controls of the current page, destroyed when it closes.
    dynamic: Vec<usize>,
    colors: Vec<(usize, COLORREF)>,
    rows: Vec<Row>,
    checks: Vec<usize>,
    radios: (usize, usize),
    marks: Vec<usize>,
    current: usize,
    outcomes: Vec<(Action, bool)>,
    /// Last steps: what the line asks, its mark and its state label.
    last: Vec<(Reopen, usize, usize, usize)>,
    busy: bool,
    success: bool,
    details_shown: bool,
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

fn with_mut<T>(f: impl FnOnce(&mut Ui) -> T) -> T {
    UI.with(|u| f(&mut u.borrow_mut()))
}

unsafe fn make_font(size: i32, weight: u32, face: &str) -> usize {
    if face == "Segoe UI" {
        return v::font(size, weight, false, with(|u| u.scale)) as usize;
    }
    let name = wide(face);
    unsafe {
        CreateFontW(
            -s(size),
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            name.as_ptr(),
        ) as usize
    }
}

fn child(ex: u32, class: &str, text: &str, style: u32, id: usize, font: usize, rect: (i32, i32, i32, i32)) -> usize {
    let class = wide(class);
    let text = wide(text);
    let parent = h(with(|u| u.hwnd));
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

/// A label of the current page, in `color`.
fn label(text: &str, font: usize, rect: (i32, i32, i32, i32), color: COLORREF) -> usize {
    let c = child(0, "STATIC", text, WS_VISIBLE | 0x80, 0, font, rect);
    with_mut(|u| {
        u.dynamic.push(c);
        u.colors.push((c, color));
    });
    c
}

fn set_color(control: usize, color: COLORREF) {
    with_mut(|u| match u.colors.iter_mut().find(|(c, _)| *c == control) {
        Some(entry) => entry.1 = color,
        None => u.colors.push((control, color)),
    });
    unsafe { InvalidateRect(h(control), std::ptr::null(), 1) };
}

fn mark(control: usize, glyph: &str, color: COLORREF) {
    set_text(control, glyph);
    set_color(control, color);
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

fn enable(control: usize, on: bool) {
    unsafe { EnableWindow(h(control), i32::from(on)) };
}

fn is_checked(control: usize) -> bool {
    unsafe { SendMessageW(h(control), BM_GETCHECK, 0, 0) == BST_CHECKED as isize }
}

fn set_check(control: usize, on: bool) {
    unsafe {
        SendMessageW(
            h(control),
            BM_SETCHECK,
            if on { BST_CHECKED } else { BST_UNCHECKED } as usize,
            0,
        )
    };
}

/// Gives the window a client height of `height` (unscaled), centred on the screen.
fn resize(height: i32) {
    let hwnd = with(|u| u.hwnd);
    unsafe {
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: s(WIDTH),
            bottom: s(height),
        };
        AdjustWindowRectEx(&mut rc, style, 0, WS_EX_CONTROLPARENT);
        let (w, hgt) = (rc.right - rc.left, rc.bottom - rc.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
        let y = ((GetSystemMetrics(SM_CYSCREEN) - hgt) / 2).max(0);
        SetWindowPos(h(hwnd), std::ptr::null_mut(), x, y, w, hgt, SWP_NOZORDER);
        InvalidateRect(h(hwnd), std::ptr::null(), 1);
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

/// Closes the current page: its controls go, the shared ones hide.
fn leave_page(next: Page) {
    let (dynamic, shared) = with(|u| {
        (
            u.dynamic.clone(),
            [u.progress, u.log, u.option, u.left, u.right, u.details],
        )
    });
    for c in dynamic {
        unsafe { DestroyWindow(h(c)) };
    }
    show(&shared, false);
    with_mut(|u| {
        u.dynamic.clear();
        u.colors.clear();
        u.checks.clear();
        u.marks.clear();
        u.last.clear();
        u.radios = (0, 0);
        u.page = next;
        u.details_shown = false;
        u.footer_y = 0;
    });
    let (page_title, text) = with(|u| (u.page_title, u.text));
    place(page_title, (MARGIN, TOP, INNER, 30));
    place(text, (MARGIN, TOP + 36, INNER, 44));
    show(&[page_title, text], true);
}

/// The two bottom-right buttons (`None` hides one) and, at the left, "Show details" when asked.
fn buttons(y: i32, left: Option<&str>, right: Option<&str>, details: bool) {
    let (l, r, d) = with(|u| (u.left, u.right, u.details));
    with_mut(|u| u.footer_y = y - 16);
    // Primary action at the far right, secondary action immediately before it.
    let right_x = if left.is_some() {
        WIDTH - MARGIN - 130 - 10 - 110
    } else {
        WIDTH - MARGIN - 110
    };
    for (control, text, x, w) in [(r, right, right_x, 110), (l, left, WIDTH - MARGIN - 130, 130)] {
        match text {
            Some(t) => {
                set_text(control, t);
                place(control, (x, y, w, BUTTON_HEIGHT));
                enable(control, true);
                show(&[control], true);
            }
            None => show(&[control], false),
        }
    }
    if details {
        set_text(d, &tr!("Show details", "Afficher les détails"));
        place(d, (MARGIN, y, 190, BUTTON_HEIGHT));
        enable(d, true);
    }
    show(&[d], details);
}

fn progress_marquee(on: bool) {
    let progress = with(|u| u.progress);
    unsafe {
        let style = GetWindowLongPtrW(h(progress), GWL_STYLE);
        if on {
            SetWindowLongPtrW(h(progress), GWL_STYLE, style | PBS_MARQUEE as isize);
            SendMessageW(h(progress), PBM_SETMARQUEE, 1, 30);
        } else {
            SendMessageW(h(progress), PBM_SETMARQUEE, 0, 0);
            SetWindowLongPtrW(h(progress), GWL_STYLE, style & !(PBS_MARQUEE as isize));
        }
    }
}

fn show_maintenance() {
    leave_page(Page::Maintenance);
    let (page_title, text) = with(|u| (u.page_title, u.text));
    set_text(
        page_title,
        &tr!("Clearway is already installed", "Clearway est déjà installé"),
    );
    let new = env!("CARGO_PKG_VERSION");
    let version = if with(|u| u.preview) {
        Installed::Older("0.1.5".into())
    } else {
        installed_version()
    };
    let old_label = match &version {
        Installed::Older(old) | Installed::Newer(old) => old.clone(),
        Installed::Same => new.to_owned(),
        Installed::Unknown => String::new(),
    };
    with_mut(|u| {
        u.installed_label = old_label;
        u.maintenance_remove = false;
    });
    if matches!(version, Installed::Older(_)) {
        set_text(
            page_title,
            &tr!("A new version is ready", "Une nouvelle version est prête"),
        );
    }
    let (about, choice, update_detail) = match version {
        Installed::Older(old) => (
            tr!(
                "Installed version: {old}. This setup holds version {new}.",
                "Version installée : {old}. Cette installation contient la version {new}."
            ),
            tr!("Update to {new}", "Mettre à jour vers la {new}"),
            tr!(
                "Replaces version {old}, keeps the settings and the log, and adds the plugin to the tools found.",
                "Remplace la version {old}, garde les réglages et le journal, et ajoute le plugin aux outils trouvés."
            ),
        ),
        Installed::Same => (
            tr!("Version {new} is installed.", "La version {new} est installée."),
            tr!("Repair", "Réparer"),
            tr!(
                "Installs this version again and adds the plugin to the tools found.",
                "Réinstalle cette version et ajoute le plugin aux outils trouvés."
            ),
        ),
        Installed::Newer(installed) => (
            tr!(
                "Installed version: {installed}, newer than this setup's ({new}).",
                "Version installée : {installed}, plus récente que celle de cette installation ({new})."
            ),
            tr!("Go back to {new}", "Revenir à la {new}"),
            tr!(
                "Replaces version {installed} with this older one.",
                "Remplace la version {installed} par cette version plus ancienne."
            ),
        ),
        Installed::Unknown => (
            tr!(
                "This setup holds version {new}.",
                "Cette installation contient la version {new}."
            ),
            tr!("Update or repair", "Mettre à jour ou réparer"),
            tr!(
                "Installs this version again and adds the plugin to the tools found.",
                "Réinstalle cette version et ajoute le plugin aux outils trouvés."
            ),
        ),
    };
    set_text(
        text,
        &format!(
            "{about} {}",
            tr!(
                "Nothing is changed before the next page.",
                "Rien n'est modifié avant la page suivante."
            )
        ),
    );
    let update = child(
        0,
        "BUTTON",
        &choice,
        WS_VISIBLE | WS_TABSTOP | WS_GROUP | BS_AUTORADIOBUTTON as u32,
        ID_UPDATE,
        BODY,
        (MARGIN + 18, ROWS_TOP + 14, INNER - 36, 26),
    );
    let remove = child(
        0,
        "BUTTON",
        &tr!("Uninstall", "Désinstaller"),
        WS_VISIBLE | WS_TABSTOP | BS_AUTORADIOBUTTON as u32,
        ID_REMOVE,
        BODY,
        (MARGIN + 18, ROWS_TOP + 124, INNER - 36, 26),
    );
    let remove_detail = tr!(
        "Removes Clearway from Windows, from WSL and from Claude Code and Codex.",
        "Retire Clearway de Windows, de WSL, de Claude Code et de Codex."
    );
    with_mut(|u| {
        u.dynamic.extend([update, remove]);
        u.radios = (update, remove);
    });
    label(
        &update_detail,
        SMALL,
        (MARGIN + 39, ROWS_TOP + 46, INNER - 62, 42),
        GREY,
    );
    label(
        &remove_detail,
        SMALL,
        (MARGIN + 39, ROWS_TOP + 156, INNER - 62, 42),
        GREY,
    );
    set_check(update, true);
    label(
        &tr!("FOR YOUR ACCOUNT", "POUR VOTRE COMPTE"),
        SMALL,
        (MARGIN + 18, ROWS_TOP + 234, INNER - 36, 20),
        GREY,
    );
    label(
        &tr!(
            "Windows and WSL · no administrator rights required.",
            "Windows et WSL · aucun droit d’administrateur requis."
        ),
        BODY,
        (MARGIN + 18, ROWS_TOP + 262, INNER - 36, 26),
        INK,
    );
    let y = ROWS_TOP + 330;
    buttons(
        y,
        Some(&tr!("Continue", "Continuer")),
        Some(&tr!("Cancel", "Annuler")),
        false,
    );
    resize(y + BUTTON_HEIGHT + MARGIN);
    unsafe { SetFocus(h(with(|u| u.left))) };
}

fn start_looking() {
    if with(|u| u.preview) {
        return;
    }
    leave_page(Page::Looking);
    let (page_title, text, progress, hwnd, uninstall) =
        with(|u| (u.page_title, u.text, u.progress, u.hwnd, u.uninstall));
    if uninstall {
        set_text(
            page_title,
            &tr!("Looking for what is installed…", "Recherche de ce qui est installé…"),
        );
        set_text(
            text,
            &tr!(
                "Clearway looks in Windows and WSL. Nothing is changed yet.",
                "Clearway regarde dans Windows et dans WSL. Rien n'est encore modifié."
            ),
        );
    } else {
        set_text(
            page_title,
            &tr!("Looking at this computer…", "Recherche sur cet ordinateur…"),
        );
        set_text(
            text,
            &tr!(
                "Clearway looks for WSL, Claude Code and Codex, and their desktop apps. Nothing is changed yet.",
                "Clearway cherche WSL, Claude Code et Codex, et leurs applications de bureau. Rien n'est encore modifié."
            ),
        );
    }
    progress_marquee(true);
    place(progress, (MARGIN, TOP + 92, INNER, 8));
    show(&[progress], true);
    resize(LOOKING_HEIGHT);
    std::thread::spawn(move || {
        let rows = Box::new(if uninstall { uninstall_plan() } else { plan() });
        unsafe { PostMessageW(hwnd as HWND, WM_PLANNED, 0, Box::into_raw(rows) as isize) };
    });
}

/// One checkbox per action (its description below in grey), one grey label per information; the window fits them.
fn show_choosing(rows: Vec<Row>) {
    leave_page(Page::Choosing);
    let (page_title, text, option, uninstall) = with(|u| (u.page_title, u.text, u.option, u.uninstall));
    if uninstall {
        set_text(page_title, &tr!("What will be removed", "Ce qui va être retiré"));
        set_text(
            text,
            &tr!(
                "Clearway is removed from each ticked place. Open sessions lose it at once; their heavy tasks are no longer queued.",
                "Clearway est retiré de chaque endroit coché. Les sessions ouvertes le perdent aussitôt ; leurs tâches lourdes ne sont plus mises en file."
            ),
        );
    } else {
        set_text(page_title, &tr!("What will be installed", "Ce qui va être installé"));
        set_text(
            text,
            &tr!(
                "Clearway lets your AI sessions run their heavy tasks one after the other, so that the computer does not freeze. Everything is installed for your account only, without administrator rights.",
                "Clearway fait passer les tâches lourdes de vos sessions d'IA chacune à son tour, pour que l'ordinateur ne sature pas. Tout s'installe pour votre compte seulement, sans droits d'administrateur."
            ),
        );
    }
    let mut checks = Vec::new();
    let mut y = ROWS_TOP;
    for (i, row) in rows.iter().enumerate() {
        let check = if row.action.is_some() {
            let c = child(
                0,
                "BUTTON",
                &row.label,
                WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                ID_ROW + i,
                BODY,
                (MARGIN, y, INNER, 22),
            );
            with_mut(|u| u.dynamic.push(c));
            set_check(c, row.checked);
            if row.locked {
                enable(c, false);
            }
            c
        } else {
            label(&format!("ⓘ  {}", row.label), BODY, (MARGIN + 2, y + 2, INNER, 22), GREY)
        };
        checks.push(check);
        label(&row.detail, SMALL, (MARGIN + 21, y + 23, INNER - 21, 18), GREY);
        y += ROW_STEP;
    }
    let option_y = y + 6;
    if uninstall {
        set_text(
            option,
            &tr!(
                "Also delete the settings and the log (%LOCALAPPDATA%\\clearway)",
                "Supprimer aussi les réglages et le journal (%LOCALAPPDATA%\\clearway)"
            ),
        );
        set_check(option, false);
    } else {
        set_text(
            option,
            &tr!(
                "Look for new versions once a day (asks GitHub; nothing installs by itself)",
                "Chercher les nouvelles versions une fois par jour (interroge GitHub ; rien ne s'installe tout seul)"
            ),
        );
        set_check(option, daily_check_enabled());
    }
    place(option, (MARGIN, option_y, INNER, 24));
    show(&[option], true);
    let buttons_y = option_y + 24 + 22;
    let go = if uninstall {
        tr!("Uninstall", "Désinstaller")
    } else {
        tr!("Install", "Installer")
    };
    buttons(buttons_y, Some(&go), Some(&tr!("Cancel", "Annuler")), false);
    with_mut(|u| {
        u.rows = rows;
        u.checks = checks;
    });
    resize(buttons_y + BUTTON_HEIGHT + MARGIN);
    unsafe { SetFocus(h(with(|u| u.left))) };
}

/// A row ticked or unticked: what needs it follows (a plugin without its distribution's client cannot work).
fn row_clicked(index: usize) {
    let (checks, needing) = with(|u| {
        let needing: Vec<usize> = u
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.needs == Some(index))
            .map(|(i, _)| i)
            .collect();
        (u.checks.clone(), needing)
    });
    let Some(&control) = checks.get(index) else { return };
    let on = is_checked(control);
    with_mut(|u| u.rows[index].checked = on);
    for i in needing {
        set_check(checks[i], on);
        enable(checks[i], on);
        with_mut(|u| u.rows[i].checked = on);
    }
}

/// Log area of the steps and result pages, shown in place of the page's lines.
fn place_log(buttons_y: i32) {
    let log = with(|u| u.log);
    place(log, (MARGIN, TOP + 70, INNER, buttons_y - TOP - 84));
}

fn start_steps() {
    if with(|u| u.preview) {
        return;
    }
    let (hwnd, option, uninstall, actions) = with(|u| {
        let actions: Vec<Action> = u
            .rows
            .iter()
            .filter(|r| r.checked)
            .filter_map(|r| r.action.clone())
            .collect();
        (u.hwnd, u.option, u.uninstall, actions)
    });
    let options = if uninstall {
        Options {
            daily: false,
            purge: is_checked(option),
        }
    } else {
        Options {
            daily: is_checked(option),
            purge: false,
        }
    };
    leave_page(Page::Steps);
    let (page_title, text, progress) = with(|u| (u.page_title, u.text, u.progress));
    with_mut(|u| {
        u.busy = true;
        u.current = 0;
    });
    set_text(
        page_title,
        &if uninstall {
            tr!("Uninstalling…", "Désinstallation en cours…")
        } else {
            tr!("Installing…", "Installation en cours…")
        },
    );
    set_text(
        text,
        &tr!(
            "Please wait: this takes about a minute.",
            "Patientez : cela prend environ une minute."
        ),
    );
    let mut y = TOP + 76;
    let mut marks = Vec::new();
    for action in &actions {
        marks.push(label(TODO, MARK, (MARGIN, y, 24, 24), GREY));
        label(&element(action), BODY, (MARGIN + 30, y + 1, INNER - 30, 24), INK);
        y += STEP_LINE;
    }
    with_mut(|u| u.marks = marks);
    progress_marquee(false);
    unsafe {
        SendMessageW(h(progress), PBM_SETRANGE32, 0, (actions.len().max(1) * 100) as isize);
        SendMessageW(h(progress), PBM_SETPOS, 0, 0);
    }
    place(progress, (MARGIN, y + 10, INNER, 8));
    show(&[progress], true);
    let buttons_y = (y + 42).max(TOP + 300);
    place_log(buttons_y);
    buttons(buttons_y, None, None, true);
    resize(buttons_y + BUTTON_HEIGHT + MARGIN);
    std::thread::spawn(move || {
        let post = |e: Event| unsafe {
            PostMessageW(hwnd as HWND, WM_EVENT, 0, Box::into_raw(Box::new(e)) as isize);
        };
        let outcomes = Box::new(install(&actions, &options, &post));
        unsafe { PostMessageW(hwnd as HWND, WM_FINISHED, 0, Box::into_raw(outcomes) as isize) };
    });
}

fn on_event(e: Event) {
    let (page_title, progress, marks, current) = with(|u| (u.page_title, u.progress, u.marks.clone(), u.current));
    match e {
        Event::Step { index, total, title } => {
            with_mut(|u| u.current = index);
            set_text(page_title, &format!("{title}…"));
            append_log(&format!("── {title} ({}/{total})", index + 1));
            if let Some(&m) = marks.get(index) {
                mark(m, RUNNING, BLUE);
            }
            unsafe { SendMessageW(h(progress), PBM_SETPOS, index * 100 + 15, 0) };
        }
        Event::Line(line) => append_log(&line),
        Event::StepDone { ok } => {
            if let Some(&m) = marks.get(current) {
                if ok { mark(m, DONE, GREEN) } else { mark(m, FAILED, RED) }
            }
            unsafe { SendMessageW(h(progress), PBM_SETPOS, (current + 1) * 100, 0) };
            if !ok {
                append_log(&tr!("   failed", "   échec"));
            }
        }
    }
}

fn plugins_done(outcomes: &[(Action, bool)]) -> bool {
    outcomes.iter().any(|(a, ok)| *ok && matches!(a, Action::Plugin { .. }))
}

fn show_result(outcomes: Vec<Outcome>) {
    let outcomes: Vec<(Action, bool)> = outcomes.into_iter().map(|o| (o.action, o.ok)).collect();
    let success = outcomes.iter().all(|(_, ok)| *ok);
    let uninstall = with(|u| u.uninstall);
    with_mut(|u| {
        u.busy = false;
        u.success = success;
    });
    leave_page(Page::Result);
    let (page_title, text) = with(|u| (u.page_title, u.text));
    label(
        if success { DONE } else { "!" },
        GLYPH,
        (MARGIN, TOP - 6, 56, 60),
        if success { GREEN } else { ORANGE },
    );
    place(page_title, (MARGIN + 64, TOP + 4, INNER - 64, 30));
    place(text, (MARGIN + 64, TOP + 40, INNER - 64, 40));
    let (title, body) = match (uninstall, success) {
        (false, true) => (
            tr!("Clearway is installed", "Clearway est installé"),
            tr!(
                "Its icon is next to the clock. One last page tells what is left to do.",
                "Son icône est à côté de l'horloge. Une dernière page indique ce qu'il reste à faire."
            ),
        ),
        (false, false) => (
            tr!("Installation incomplete", "Installation incomplète"),
            tr!(
                "What failed is marked ✖, with what to do.",
                "Ce qui a échoué est marqué ✖, avec ce qu'il faut faire."
            ),
        ),
        (true, true) => (
            tr!("Clearway is uninstalled", "Clearway est désinstallé"),
            tr!(
                "Restart your Claude Code and Codex sessions so that they forget it.",
                "Relancez vos sessions Claude Code et Codex pour qu'elles l'oublient."
            ),
        ),
        (true, false) => (
            tr!("Uninstallation incomplete", "Désinstallation incomplète"),
            tr!(
                "What could not be removed is marked ✖, with what to do.",
                "Ce qui n'a pas pu être retiré est marqué ✖, avec ce qu'il faut faire."
            ),
        ),
    };
    set_text(page_title, &title);
    set_text(text, &body);
    let mut y = TOP + 96;
    for (action, ok) in &outcomes {
        label(
            if *ok { DONE } else { FAILED },
            MARK,
            (MARGIN + 64, y, 24, 24),
            if *ok { GREEN } else { RED },
        );
        label(&element(action), BODY, (MARGIN + 94, y + 1, INNER - 94, 24), INK);
        y += STEP_LINE;
        if !ok {
            label(&hint(action), SMALL, (MARGIN + 94, y - 4, INNER - 94, 34), GREY);
            y += 34;
        }
    }
    let buttons_y = (y + 16).max(TOP + 240);
    place_log(buttons_y);
    let next = !uninstall && plugins_done(&outcomes);
    let close = tr!("Close", "Fermer");
    if next {
        buttons(buttons_y, Some(&tr!("Next", "Suivant")), None, true);
    } else {
        buttons(buttons_y, None, Some(&close), true);
    }
    with_mut(|u| u.outcomes = outcomes);
    resize(buttons_y + BUTTON_HEIGHT + MARGIN);
    unsafe { SetFocus(h(with(|u| if next { u.left } else { u.right }))) };
}

fn show_last() {
    let outcomes = with(|u| u.outcomes.clone());
    leave_page(Page::Last);
    let (page_title, text) = with(|u| (u.page_title, u.text));
    set_text(page_title, &tr!("Last steps", "Dernières étapes"));
    set_text(
        text,
        &tr!(
            "For your sessions to use Clearway. \"Verify\" looks again at each line.",
            "Pour que vos sessions utilisent Clearway. « Vérifier » regarde de nouveau chaque ligne."
        ),
    );
    let installed = |t: Tool| {
        outcomes
            .iter()
            .any(|(a, ok)| *ok && matches!(a, Action::Plugin { tool, .. } if *tool == t))
    };
    let app = |t: Tool| {
        let cli_only = format!("{} (Windows)", t.name());
        outcomes.iter().any(|(a, ok)| {
            *ok && matches!(a, Action::Plugin { tool, distro: None, label, .. } if *tool == t && *label != cli_only)
        })
    };
    let mut lines: Vec<(Reopen, String, String)> = Vec::new();
    for (tool, kind) in [(Tool::Claude, Reopen::Claude), (Tool::Codex, Reopen::Codex)] {
        if installed(tool) {
            let title = if app(tool) {
                tr!(
                    "Restart your {} sessions and the {} app",
                    "Relancez vos sessions {} et l'application {}",
                    tool.name(),
                    tool.app()
                )
            } else {
                tr!("Restart your {} sessions", "Relancez vos sessions {}", tool.name())
            };
            lines.push((
                kind,
                title,
                tr!(
                    "A session opened before this installation or update must be restarted to use it.",
                    "Une session ouverte avant cette installation ou mise à jour doit être relancée pour l'utiliser."
                ),
            ));
        }
    }
    if installed(Tool::Codex) {
        lines.push((
            Reopen::CodexGuard,
            tr!("Approve Clearway's guard in Codex", "Approuvez la garde de Clearway dans Codex"),
            tr!(
                "In a NEW Codex session: /hooks, then PreToolUse, then approve \"Clearway\" (clearway hook). In the app: Settings > Hooks.",
                "Dans une NOUVELLE session Codex : /hooks, puis PreToolUse, puis approuvez « Clearway » (clearway hook). Dans l'application : Paramètres > Hooks."
            ),
        ));
    }
    let mut y = TOP + 92;
    let mut last = Vec::new();
    for (kind, title, detail) in lines {
        let m = label(TODO, MARK, (MARGIN, y, 24, 24), GREY);
        label(&title, BODY, (MARGIN + 30, y + 1, INNER - 30, 24), INK);
        let detail = label(&detail, SMALL, (MARGIN + 30, y + 26, INNER - 30, 34), GREY);
        let state = label("", SMALL, (MARGIN + 30, y + 60, INNER - 30, 18), GREY);
        last.push((kind, m, detail, state));
        y += 88;
    }
    with_mut(|u| u.last = last);
    let buttons_y = y + 8;
    buttons(
        buttons_y,
        Some(&tr!("Verify", "Vérifier")),
        Some(&tr!("Finish", "Terminer")),
        false,
    );
    resize(buttons_y + BUTTON_HEIGHT + MARGIN);
    unsafe { SetFocus(h(with(|u| u.right))) };
    start_check();
}

fn start_check() {
    if with(|u| u.preview) {
        return;
    }
    let (hwnd, left, last, outcomes) = with(|u| (u.hwnd, u.left, u.last.clone(), u.outcomes.clone()));
    enable(left, false);
    for (_, _, _, state) in &last {
        set_text(*state, &tr!("Verifying…", "Vérification…"));
        set_color(*state, GREY);
    }
    let actions: Vec<Action> = outcomes.into_iter().filter(|(_, ok)| *ok).map(|(a, _)| a).collect();
    std::thread::spawn(move || {
        let checks = Box::new(check(&actions));
        unsafe { PostMessageW(hwnd as HWND, WM_CHECKED, 0, Box::into_raw(checks) as isize) };
    });
}

/// Done once every linked session runs this version: (done, detail naming the sessions to restart, state).
fn sessions_state(s: &Sessions, tool: &str) -> (Option<bool>, String, String) {
    let version = env!("CARGO_PKG_VERSION");
    let up_to_date = |k: usize| {
        if k == 1 {
            tr!("1 session up to date ({version})", "1 session à jour ({version})")
        } else {
            tr!("{k} sessions up to date ({version})", "{k} sessions à jour ({version})")
        }
    };
    match (s.current, s.old.len()) {
        (0, 0) => (
            Some(false),
            tr!(
                "No {tool} session sees Clearway yet: open a new one.",
                "Aucune session {tool} ne voit encore Clearway : ouvrez-en une nouvelle."
            ),
            tr!("no session linked yet", "aucune session connectée pour l'instant"),
        ),
        (k, 0) => (
            Some(true),
            tr!(
                "Every linked session runs this version. A session opened before the installation is not listed: restart it if any is left.",
                "Toutes les sessions connectées utilisent cette version. Une session ouverte avant l'installation n'apparaît pas : relancez-la s'il en reste."
            ),
            up_to_date(k),
        ),
        (k, n) => (
            Some(false),
            tr!(
                "Still on an older version, to restart: {}",
                "Encore sur une ancienne version, à relancer : {}",
                old_sessions_list(&s.old, 3)
            ),
            format!(
                "{} · {}",
                up_to_date(k),
                if n == 1 {
                    tr!("1 to restart", "1 à relancer")
                } else {
                    tr!("{n} to restart", "{n} à relancer")
                }
            ),
        ),
    }
}

fn on_checked(c: Checks) {
    let (left, last, page) = with(|u| (u.left, u.last.clone(), u.page));
    if page != Page::Last {
        return;
    }
    enable(left, true);
    for (kind, m, detail, state) in last {
        let (done, text) = match kind {
            Reopen::Claude | Reopen::Codex => match &c.sessions {
                None => (
                    None,
                    tr!(
                        "Clearway does not answer: look at its icon next to the clock.",
                        "Clearway ne répond pas : regardez son icône près de l'horloge."
                    ),
                ),
                Some((claude, codex)) => {
                    let (s, tool) = if kind == Reopen::Claude {
                        (claude, Tool::Claude)
                    } else {
                        (codex, Tool::Codex)
                    };
                    let (done, what, text) = sessions_state(s, tool.name());
                    set_text(detail, &what);
                    (done, text)
                }
            },
            Reopen::CodexGuard => {
                let all = !c.trust.is_empty() && c.trust.iter().all(|(_, t)| *t == HookTrust::Trusted);
                let parts: Vec<String> = c
                    .trust
                    .iter()
                    .map(|(place, t)| {
                        let what = match t {
                            HookTrust::Trusted => tr!("approved", "approuvée"),
                            HookTrust::Untrusted => tr!("not approved yet", "pas encore approuvée"),
                            HookTrust::Modified => {
                                tr!("changed: approve it again", "modifiée : à approuver de nouveau")
                            }
                            HookTrust::Absent => tr!(
                                "not loaded yet (open a new session)",
                                "pas encore chargée (ouvrez une nouvelle session)"
                            ),
                        };
                        format!("{place} : {what}")
                    })
                    .collect();
                (Some(all), parts.join(" · "))
            }
        };
        match done {
            Some(true) => mark(m, DONE, GREEN),
            Some(false) => mark(m, TODO, GREY),
            None => mark(m, "!", ORANGE),
        }
        set_text(state, &text);
        set_color(
            state,
            match done {
                Some(true) => GREEN,
                Some(false) => GREY,
                None => ORANGE,
            },
        );
    }
}

fn toggle_details() {
    let (log, details, dynamic, shown, progress, page) =
        with(|u| (u.log, u.details, u.dynamic.clone(), u.details_shown, u.progress, u.page));
    let shown = !shown;
    with_mut(|u| u.details_shown = shown);
    show(&dynamic, !shown);
    if page == Page::Steps {
        show(&[progress], !shown);
    }
    show(&[log], shown);
    set_text(
        details,
        &if shown {
            tr!("Hide details", "Masquer les détails")
        } else {
            tr!("Show details", "Afficher les détails")
        },
    );
}

fn left_clicked() {
    if with(|u| u.preview) {
        return;
    }
    let (page, radios, hwnd) = with(|u| (u.page, u.radios, u.hwnd));
    match page {
        Page::Maintenance => {
            if is_checked(radios.1) {
                if runs_from_install_dir() {
                    if relaunch_from_temp(&["--uninstall".to_owned()]).is_ok() {
                        with_mut(|u| u.success = true);
                        unsafe { DestroyWindow(h(hwnd)) };
                    }
                    return;
                }
                with_mut(|u| u.uninstall = true);
            }
            start_looking();
        }
        Page::Choosing => start_steps(),
        Page::Result => show_last(),
        Page::Last => start_check(),
        Page::Looking | Page::Steps => {}
    }
}

unsafe fn create(hwnd: HWND) {
    unsafe {
        UI.with(|u| u.borrow_mut().hwnd = hwnd as usize);
        let fonts = [
            make_font(15, FW_NORMAL, "Segoe UI"),
            make_font(20, FW_SEMIBOLD, "Segoe UI"),
            make_font(24, FW_SEMIBOLD, "Segoe UI"),
            make_font(15, FW_NORMAL, "Segoe UI"),
            make_font(44, FW_NORMAL, "Segoe UI Symbol"),
            make_font(13, FW_NORMAL, "Segoe UI"),
            make_font(16, FW_NORMAL, "Segoe UI Symbol"),
            v::font(13, FW_NORMAL, true, with(|u| u.scale)) as usize,
        ];
        let icon = LoadImageW(
            GetModuleHandleW(std::ptr::null()),
            1 as _,
            IMAGE_ICON,
            s(48),
            s(48),
            LR_DEFAULTCOLOR,
        ) as usize;
        with_mut(|u| {
            u.fonts = fonts;
            u.icon = icon;
        });
        let page_title = child(0, "STATIC", "", WS_VISIBLE | 0x80, 0, BOLD, (MARGIN, TOP, INNER, 30));
        let text = child(
            0,
            "STATIC",
            "",
            WS_VISIBLE | 0x80,
            0,
            BODY,
            (MARGIN, TOP + 36, INNER, 44),
        );
        let progress = child(
            0,
            "msctls_progress32",
            "",
            PBS_MARQUEE,
            0,
            BODY,
            (MARGIN, TOP + 92, INNER, 8),
        );
        let option = child(
            0,
            "BUTTON",
            "",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            ID_OPTION,
            BODY,
            (MARGIN, 0, INNER, 24),
        );
        let right = child(
            0,
            "BUTTON",
            "",
            WS_TABSTOP | BS_OWNERDRAW as u32,
            ID_RIGHT,
            BODY,
            (0, 0, 110, BUTTON_HEIGHT),
        );
        let left = child(
            0,
            "BUTTON",
            "",
            WS_TABSTOP | BS_OWNERDRAW as u32,
            ID_LEFT,
            BODY,
            (0, 0, 130, BUTTON_HEIGHT),
        );
        let log = child(
            WS_EX_CLIENTEDGE,
            "EDIT",
            "",
            WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
            0,
            SMALL,
            (0, 0, 10, 10),
        );
        let details = child(
            0,
            "BUTTON",
            "",
            WS_TABSTOP | BS_OWNERDRAW as u32,
            ID_DETAILS,
            BODY,
            (0, 0, 190, BUTTON_HEIGHT),
        );
        with_mut(|u| {
            (u.page_title, u.text, u.progress, u.option, u.right, u.left) =
                (page_title, text, progress, option, right, left);
            (u.log, u.details) = (log, details);
        });
    }
}

/// Card background for native radio/label controls; the native radio keeps its keyboard behavior.
fn control_background(control: HWND) -> COLORREF {
    if with(|u| u.page) != Page::Maintenance {
        return v::PAPER;
    }
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetWindowRect(control, &mut rc);
        let mut point = windows_sys::Win32::Foundation::POINT { x: rc.left, y: rc.top };
        windows_sys::Win32::Graphics::Gdi::ScreenToClient(h(with(|u| u.hwnd)), &mut point);
        let remove = with(|u| u.maintenance_remove);
        let selected_top = ROWS_TOP + if remove { 110 } else { 0 };
        if point.y >= s(selected_top) && point.y < s(selected_top + 98) {
            v::rgb(0xf2faf5)
        } else if point.y >= s(ROWS_TOP + 220) && point.y < s(ROWS_TOP + 306) {
            v::rgb(0xf4f6f8)
        } else {
            v::PAPER
        }
    }
}

unsafe fn draw_page(_hwnd: HWND, dc: HDC) {
    unsafe {
        {
            let c = Canvas::new(dc);
            c.fill(rect(0, 0, s(WIDTH), s(HEADER)), v::NAVY);
            let (icon, fonts, uninstall, page, remove, old, footer) = with(|u| {
                (
                    u.icon,
                    u.fonts,
                    u.uninstall,
                    u.page,
                    u.maintenance_remove,
                    u.installed_label.clone(),
                    u.footer_y,
                )
            });
            DrawIconEx(
                dc,
                s(MARGIN),
                s(28),
                icon as HICON,
                s(48),
                s(48),
                0,
                std::ptr::null_mut(),
                DI_NORMAL,
            );
            c.text(
                rect(s(MARGIN + 64), s(24), s(280), s(32)),
                "Clearway",
                fonts[TITLE] as _,
                v::PAPER,
                DT_LEFT | DT_SINGLELINE,
            );
            c.text(
                rect(s(MARGIN + 64), s(62), s(280), s(24)),
                &if uninstall {
                    tr!("Uninstall", "Désinstallation")
                } else {
                    tr!("Setup", "Installation")
                },
                fonts[SMALL] as _,
                v::rgb(0xa9b8c6),
                DT_LEFT | DT_SINGLELINE,
            );
            let version = env!("CARGO_PKG_VERSION");
            let chip = rect(s(WIDTH - MARGIN - 90), s(38), s(90), s(30));
            c.panel(chip, v::GREEN_INK, v::GREEN_INK, s(6));
            c.text(
                chip,
                version,
                fonts[MONO] as _,
                v::PAPER,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER,
            );
            if !old.is_empty() && old != version {
                let chip = rect(s(WIDTH - MARGIN - 216), s(38), s(90), s(30));
                c.panel(chip, v::rgb(0x1c2d3e), v::rgb(0x1c2d3e), s(6));
                c.text(
                    chip,
                    &old,
                    fonts[MONO] as _,
                    v::rgb(0xc5d1dc),
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
                c.text(
                    rect(chip.right, chip.top, s(36), s(30)),
                    "→",
                    fonts[BODY] as _,
                    v::rgb(0x6fd39b),
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
            }
            if page == Page::Maintenance {
                for (offset, selected) in [(0, !remove), (110, remove)] {
                    let rc = rect(s(MARGIN), s(ROWS_TOP + offset), s(INNER), s(98));
                    c.panel(
                        rc,
                        if selected { v::rgb(0xf2faf5) } else { v::PAPER },
                        if selected { v::GREEN } else { v::BORDER },
                        s(10),
                    );
                    if selected {
                        c.panel(inset(rc, 1, 1), v::rgb(0xf2faf5), v::GREEN, s(9));
                    }
                }
                c.panel(
                    rect(s(MARGIN), s(ROWS_TOP + 220), s(INNER), s(86)),
                    v::rgb(0xf4f6f8),
                    v::rgb(0xf4f6f8),
                    s(10),
                );
            }
            if footer > 0 {
                c.fill(rect(s(MARGIN), s(footer), s(INNER), 1), v::BORDER);
            }
        }
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                draw_page(hwnd, dc);
                EndPaint(hwnd, &ps);
                0
            }
            windows_sys::Win32::UI::WindowsAndMessaging::WM_PRINTCLIENT => {
                let c = Canvas::new(wparam as HDC);
                let mut client: RECT = std::mem::zeroed();
                windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
                c.fill(client, v::PAPER);
                draw_page(hwnd, wparam as HDC);
                0
            }
            WM_DRAWITEM => {
                let d = &*(lparam as *const DRAWITEMSTRUCT);
                v::button(
                    d,
                    with(|u| u.fonts[BODY]) as _,
                    with(|u| u.scale),
                    d.CtlID as usize == ID_LEFT,
                    v::PAPER,
                );
                1
            }
            WM_LBUTTONUP if with(|u| u.page) == Page::Maintenance => {
                let x = (lparam as u16 as i16) as i32;
                let y = ((lparam >> 16) as u16 as i16) as i32;
                if x >= s(MARGIN) && x <= s(WIDTH - MARGIN) {
                    let selected = if y >= s(ROWS_TOP) && y < s(ROWS_TOP + 98) {
                        Some(false)
                    } else if y >= s(ROWS_TOP + 110) && y < s(ROWS_TOP + 208) {
                        Some(true)
                    } else {
                        None
                    };
                    if let Some(remove) = selected {
                        let radios = with(|u| u.radios);
                        set_check(radios.0, !remove);
                        set_check(radios.1, remove);
                        SendMessageW(hwnd, WM_COMMAND, if remove { ID_REMOVE } else { ID_UPDATE }, 0);
                        SetFocus(h(if remove { radios.1 } else { radios.0 }));
                    }
                }
                0
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let dc = wparam as HDC;
                let control = lparam as usize;
                let color = with(|u| {
                    u.colors
                        .iter()
                        .find(|(c, _)| *c == control)
                        .map_or(INK, |(_, color)| *color)
                });
                let background = control_background(lparam as HWND);
                SetBkColor(dc, background);
                SetTextColor(dc, color);
                SetDCBrushColor(dc, background);
                GetStockObject(DC_BRUSH) as LRESULT
            }
            WM_TIMER if wparam == FADE_TIMER => {
                let alpha = with_mut(|u| {
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
                show_choosing(*Box::from_raw(lparam as *mut Vec<Row>));
                0
            }
            WM_EVENT => {
                on_event(*Box::from_raw(lparam as *mut Event));
                0
            }
            WM_FINISHED => {
                show_result(*Box::from_raw(lparam as *mut Vec<Outcome>));
                0
            }
            WM_CHECKED => {
                on_checked(*Box::from_raw(lparam as *mut Checks));
                0
            }
            // Only clicks: a control's other notifications (focus, edit changes) carry the same identifier.
            WM_COMMAND if (wparam >> 16) & 0xffff != 0 => 0,
            WM_COMMAND => {
                match wparam & 0xffff {
                    ID_LEFT => left_clicked(),
                    ID_UPDATE | ID_REMOVE => {
                        with_mut(|u| u.maintenance_remove = wparam & 0xffff == ID_REMOVE);
                        InvalidateRect(hwnd, std::ptr::null(), 1);
                        for control in with(|u| u.dynamic.clone()) {
                            InvalidateRect(h(control), std::ptr::null(), 1);
                        }
                    }
                    ID_RIGHT => {
                        DestroyWindow(hwnd);
                    }
                    ID_DETAILS => toggle_details(),
                    id if id >= ID_ROW => row_clicked(id - ID_ROW),
                    _ => {}
                }
                0
            }
            // No closing in the middle of an installation.
            WM_CLOSE if with(|u| u.busy) => 0,
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

pub fn run(uninstall: bool, preview: bool) -> i32 {
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
        with_mut(|u| {
            u.scale = scale;
            u.uninstall = uninstall;
            u.preview = preview;
        });
        let title = wide(&if preview {
            tr!(
                "Clearway setup — preview only",
                "Installation Clearway — aperçu sans modification"
            )
        } else if uninstall {
            tr!("Uninstall Clearway", "Désinstallation de Clearway")
        } else {
            tr!("Clearway setup", "Installation de Clearway")
        });
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
        if preview || (!uninstall && installed()) {
            show_maintenance();
        } else {
            start_looking();
        }
        if preview {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED as isize));
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                -20000,
                -20000,
                0,
                0,
                SWP_NOZORDER
                    | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                    | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
            );
            ShowWindow(hwnd, windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE);
        } else {
            SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
            ShowWindow(hwnd, SW_SHOW);
            SetTimer(hwnd, FADE_TIMER, 15, None);
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            // Owner-drawn buttons retain Space and Tab natively; Enter activates the focused
            // action, or the primary action when focus is on a radio/check box.
            if msg.message == WM_KEYDOWN && msg.wParam == VK_RETURN as usize {
                let focus = GetFocus() as usize;
                let (left, right, details) = with(|u| (u.left, u.right, u.details));
                let target = if [left, right, details].contains(&focus) {
                    focus
                } else {
                    left
                };
                if IsWindowVisible(h(target)) != 0 && IsWindowEnabled(h(target)) != 0 {
                    SendMessageW(h(target), BM_CLICK, 0, 0);
                    continue;
                }
            }
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
