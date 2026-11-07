//! User-facing messages: English by default, French provided.
//! The language comes from `language` in config.toml (server), else from `CLEARWAY_LANG`, `LC_ALL`, `LC_MESSAGES`
//! or `LANG`, else from the Windows display language.

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Fr,
}

const UNSET: u8 = 0;

static GLOBAL: AtomicU8 = AtomicU8::new(UNSET);

thread_local! {
    static LOCAL: Cell<Option<Lang>> = const { Cell::new(None) };
}

fn code(lang: Lang) -> u8 {
    match lang {
        Lang::En => 1,
        Lang::Fr => 2,
    }
}

pub fn set(lang: Lang) {
    GLOBAL.store(code(lang), Ordering::SeqCst);
}

pub fn current() -> Lang {
    if let Some(l) = LOCAL.with(Cell::get) {
        return l;
    }
    match GLOBAL.load(Ordering::SeqCst) {
        1 => Lang::En,
        2 => Lang::Fr,
        _ => {
            let l = detect();
            set(l);
            l
        }
    }
}

/// Runs `f` with `lang` on this thread only (tests, or a reply in the caller's language).
pub fn with<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    let before = LOCAL.with(|c| c.replace(Some(lang)));
    let r = f();
    LOCAL.with(|c| c.set(before));
    r
}

/// `fr`, `fr_FR.UTF-8`, `French`, `français` → French; `en…` → English; anything else is unknown.
pub fn parse(s: &str) -> Option<Lang> {
    let s = s.trim().to_lowercase();
    if s.starts_with("fr") {
        Some(Lang::Fr)
    } else if s.starts_with("en") {
        Some(Lang::En)
    } else {
        None
    }
}

pub fn name(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        Lang::Fr => "fr",
    }
}

pub fn detect() -> Lang {
    for var in ["CLEARWAY_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(v) = std::env::var(var).ok().filter(|v| !v.is_empty()) {
            // The first variable set wins, as for gettext: `LANG=C.UTF-8` means English.
            return parse(&v).unwrap_or(Lang::En);
        }
    }
    system_lang().unwrap_or(Lang::En)
}

#[cfg(windows)]
fn system_lang() -> Option<Lang> {
    let langid = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    // Primary language identifier 0x0C is French, 0x09 English.
    match langid & 0x3ff {
        0x0c => Some(Lang::Fr),
        _ => Some(Lang::En),
    }
}

#[cfg(not(windows))]
fn system_lang() -> Option<Lang> {
    None
}

/// `tr!("English {x}", "Français {x}", …)`: formats the message in the current language.
#[macro_export]
macro_rules! tr {
    ($en:literal, $fr:literal $(, $arg:expr)* $(,)?) => {
        match $crate::i18n::current() {
            $crate::i18n::Lang::Fr => format!($fr $(, $arg)*),
            $crate::i18n::Lang::En => format!($en $(, $arg)*),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_names_and_override() {
        assert_eq!(parse("fr_FR.UTF-8"), Some(Lang::Fr));
        assert_eq!(parse("French"), Some(Lang::Fr));
        assert_eq!(parse("en_US"), Some(Lang::En));
        assert_eq!(parse("C.UTF-8"), None);
        let n = 3;
        assert_eq!(with(Lang::En, || tr!("{} tickets", "{} tickets", n + 1)), "4 tickets");
        assert_eq!(with(Lang::Fr, || tr!("ticket #{n} released", "ticket #{n} libéré")), "ticket #3 libéré");
        assert_eq!(with(Lang::En, || tr!("ticket #{n} released", "ticket #{n} libéré")), "ticket #3 released");
    }
}
