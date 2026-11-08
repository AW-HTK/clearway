//! Shared native drawing primitives for the application and setup, from the Clearway mockups.
//! Colors are RGB here (converted to Win32 COLORREF), never separate palettes per executable.

use std::sync::Once;
use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW;

pub const fn rgb(value: u32) -> COLORREF {
    ((value & 0xff) << 16) | (value & 0xff00) | ((value >> 16) & 0xff)
}

pub const NAVY: COLORREF = rgb(0x0d1722);
pub const BACKGROUND: COLORREF = rgb(0xeef2f5);
pub const PAPER: COLORREF = rgb(0xffffff);
pub const INK: COLORREF = rgb(0x17202b);
pub const MUTED: COLORREF = rgb(0x4d5967);
pub const BORDER: COLORREF = rgb(0xd6dde4);
pub const GREEN: COLORREF = rgb(0x16a35a);
pub const GREEN_INK: COLORREF = rgb(0x116236);
pub const GREEN_WASH: COLORREF = rgb(0xdff3e7);
pub const AMBER: COLORREF = rgb(0x7a4a00);
pub const AMBER_WASH: COLORREF = rgb(0xfbefd9);
pub const RED: COLORREF = rgb(0xa82e35);
pub const RED_WASH: COLORREF = rgb(0xfbe8e9);
pub const BLUE: COLORREF = rgb(0x4b7fbf);
pub const TEAL: COLORREF = rgb(0x2f8f97);
pub const MARGIN: COLORREF = rgb(0xb9c4cf);
pub const TRACK: COLORREF = rgb(0xe3e9ee);

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub fn rect(x: i32, y: i32, width: i32, height: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + width.max(0),
        bottom: y + height.max(0),
    }
}

pub fn inset(r: RECT, x: i32, y: i32) -> RECT {
    rect(
        r.left + x,
        r.top + y,
        r.right - r.left - 2 * x,
        r.bottom - r.top - 2 * y,
    )
}

/// Owns GDI fonts; callers must deselect them from their DCs before dropping them.
pub struct Fonts {
    pub body: HFONT,
    pub small: HFONT,
    pub strong: HFONT,
    pub title: HFONT,
    pub mono: HFONT,
}

impl Fonts {
    pub fn new(scale: f32) -> Self {
        Self {
            body: font(14, FW_NORMAL, false, scale),
            small: font(12, FW_NORMAL, false, scale),
            strong: font(15, FW_SEMIBOLD, false, scale),
            title: font(22, FW_SEMIBOLD, false, scale),
            mono: font(12, FW_NORMAL, true, scale),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.body, self.small, self.strong, self.title, self.mono] {
            unsafe { DeleteObject(f as _) };
        }
    }
}

/// Loads bundled fonts privately, without installing anything in Windows. GDI releases these resources
/// with the process; keeping them alive also covers windows closed and reopened from the tray.
fn register_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let files: [&[u8]; 3] = [
            include_bytes!("../assets/fonts/Inter-Regular.ttf"),
            include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
            include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
        ];
        for bytes in files {
            let mut count = 0u32;
            unsafe {
                AddFontMemResourceEx(
                    bytes.as_ptr().cast(),
                    bytes.len() as u32,
                    std::ptr::null(),
                    &raw mut count,
                )
            };
        }
    });
}

/// Bundled Inter/JetBrains Mono, with Windows fallbacks if GDI cannot load them.
pub fn font(size: i32, weight: u32, mono: bool, scale: f32) -> HFONT {
    register_fonts();
    unsafe {
        let make = |face: &str| {
            CreateFontW(
                -(size as f32 * scale).round() as i32,
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
                wide(face).as_ptr(),
            )
        };
        let wanted = if mono {
            "JetBrains Mono"
        } else if weight >= FW_SEMIBOLD {
            "Inter SemiBold"
        } else {
            "Inter"
        };
        let f = make(wanted);
        let dc = GetDC(std::ptr::null_mut());
        let old = SelectObject(dc, f as _);
        let mut face = [0u16; 128];
        let n = GetTextFaceW(dc, face.len() as i32, face.as_mut_ptr());
        SelectObject(dc, old);
        ReleaseDC(std::ptr::null_mut(), dc);
        let actual = String::from_utf16_lossy(&face[..n.max(0) as usize])
            .trim_end_matches('\0')
            .to_owned();
        if actual == wanted {
            f
        } else {
            DeleteObject(f as _);
            make(if mono { "Consolas" } else { "Segoe UI" })
        }
    }
}

pub struct Canvas {
    dc: HDC,
    saved: i32,
}

impl Canvas {
    /// # Safety
    /// `dc` must be a valid device context for the lifetime of this canvas, on the calling UI thread.
    pub unsafe fn new(dc: HDC) -> Self {
        let saved = unsafe { SaveDC(dc) };
        unsafe { SetBkMode(dc, TRANSPARENT as i32) };
        Self { dc, saved }
    }

    pub fn fill(&self, r: RECT, color: COLORREF) {
        unsafe {
            let b = CreateSolidBrush(color);
            FillRect(self.dc, &r, b);
            DeleteObject(b as _);
        }
    }

    pub fn panel(&self, r: RECT, fill: COLORREF, border: COLORREF, radius: i32) {
        unsafe {
            let b = CreateSolidBrush(fill);
            let p = CreatePen(PS_SOLID, 1, border);
            let old_b = SelectObject(self.dc, b as _);
            let old_p = SelectObject(self.dc, p as _);
            RoundRect(self.dc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);
            SelectObject(self.dc, old_p);
            SelectObject(self.dc, old_b);
            DeleteObject(p as _);
            DeleteObject(b as _);
        }
    }

    // HFONT is an opaque GDI handle, never dereferenced by Rust.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn text(&self, r: RECT, value: &str, font: HFONT, color: COLORREF, flags: u32) {
        unsafe {
            let old = SelectObject(self.dc, font as _);
            SetTextColor(self.dc, color);
            let mut r = r;
            let value = wide(value);
            DrawTextW(self.dc, value.as_ptr(), -1, &mut r, flags | DT_NOPREFIX);
            SelectObject(self.dc, old);
        }
    }

    pub fn focus(&self, r: RECT) {
        unsafe { DrawFocusRect(self.dc, &r) };
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe { RestoreDC(self.dc, self.saved) };
    }
}

/// Draw a real Win32 button, keeping native keyboard, focus and accessibility behavior.
/// # Safety
/// `item` must belong to the current WM_DRAWITEM call; `font` must be a live GDI font.
pub unsafe fn button(item: &DRAWITEMSTRUCT, font: HFONT, scale: f32, primary: bool, background: COLORREF) {
    let c = unsafe { Canvas::new(item.hDC) };
    let disabled = item.itemState & ODS_DISABLED != 0;
    let pressed = item.itemState & ODS_SELECTED != 0;
    let fill = if disabled {
        BACKGROUND
    } else if primary {
        if pressed { GREEN_INK } else { rgb(0x127a41) }
    } else if pressed {
        TRACK
    } else {
        PAPER
    };
    c.fill(item.rcItem, background);
    c.panel(
        inset(item.rcItem, 1, 1),
        fill,
        if primary && !disabled { fill } else { BORDER },
        (8.0 * scale) as i32,
    );
    let mut label = [0u16; 512];
    let n = unsafe { GetWindowTextW(item.hwndItem, label.as_mut_ptr(), label.len() as i32) };
    c.text(
        inset(item.rcItem, (8.0 * scale) as i32, 0),
        &String::from_utf16_lossy(&label[..n.max(0) as usize]),
        font,
        if disabled {
            MUTED
        } else if primary {
            PAPER
        } else {
            INK
        },
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
        c.focus(inset(item.rcItem, (4.0 * scale) as i32, (4.0 * scale) as i32));
    }
}
