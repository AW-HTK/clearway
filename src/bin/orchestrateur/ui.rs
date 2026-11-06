use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::serveur::Partage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Couleur {
    Vert,
    Bleu,
    Orange,
    Rouge,
}

static PARTAGE: OnceLock<Arc<Partage>> = OnceLock::new();
/// Mode test : rien à l'écran (ni bulle, ni boîte, fenêtre cachée), erreurs sur stderr et au journal.
static TEST: AtomicBool = AtomicBool::new(false);
static ICONE: AtomicBool = AtomicBool::new(true);

pub fn mode_test(avec_icone: bool) {
    TEST.store(true, Ordering::SeqCst);
    ICONE.store(avec_icone, Ordering::SeqCst);
}

pub fn installer(p: Arc<Partage>) {
    let _ = PARTAGE.set(p);
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn partage() -> Option<&'static Arc<Partage>> {
    PARTAGE.get()
}

#[cfg(not(windows))]
static QUITTER: AtomicBool = AtomicBool::new(false);

#[cfg(not(windows))]
pub fn demarrer(_journal: Option<std::path::PathBuf>) -> bool {
    true
}

#[cfg(not(windows))]
pub fn maj(_couleur: Couleur, _texte: &str) {}

#[cfg(not(windows))]
pub fn bulle(_titre: &str, _texte: &str, _alerte: bool) {}

#[cfg(not(windows))]
pub fn fenetre(_ouvrir: bool) {}

#[cfg(not(windows))]
pub fn quitter() {
    QUITTER.store(true, Ordering::SeqCst);
}

#[cfg(not(windows))]
pub fn boucle() {
    while !QUITTER.load(Ordering::SeqCst) {
        std::thread::park_timeout(std::time::Duration::from_millis(100));
    }
}

#[cfg(not(windows))]
pub fn avertir(titre: &str, texte: &str) {
    eprintln!("{titre} : {texte}");
}

#[cfg(windows)]
pub use natif::{avertir, boucle, bulle, demarrer, fenetre, maj, ouvrir_journal, quitter};

#[cfg(windows)]
mod natif {
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

    use super::{Couleur, ICONE, TEST, partage};
    use crate::fenetre;
    use crate::serveur::journaliser;

    const ID: u32 = 1;
    const WM_ICONE: u32 = WM_APP + 1;
    const WM_FENETRE: u32 = WM_APP + 2;
    const CMD_OUVRIR: usize = 1;
    const CMD_GEL: usize = 2;
    const CMD_JOURNAL: usize = 3;
    const CMD_QUITTER: usize = 4;

    static CACHEE: AtomicUsize = AtomicUsize::new(0);
    static BARRE_RECREEE: AtomicU32 = AtomicU32::new(0);
    static ICONES: OnceLock<[usize; 4]> = OnceLock::new();
    static JOURNAL: OnceLock<Option<PathBuf>> = OnceLock::new();
    static DERNIER: Mutex<Option<(Couleur, String)>> = Mutex::new(None);

    fn large(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn copier(dest: &mut [u16], s: &str) {
        let mut n = 0;
        for (i, c) in s.encode_utf16().take(dest.len() - 1).enumerate() {
            dest[i] = c;
            n = i + 1;
        }
        dest[n] = 0;
    }

    fn cachee() -> HWND {
        CACHEE.load(Ordering::SeqCst) as HWND
    }

    fn avec_icone() -> bool {
        ICONE.load(Ordering::SeqCst) && !cachee().is_null()
    }

    fn donnees() -> NOTIFYICONDATAW {
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = cachee();
        d.uID = ID;
        d
    }

    fn icone(c: Couleur) -> HICON {
        ICONES.get().map_or(std::ptr::null_mut(), |i| i[c as usize] as HICON)
    }

    fn creer_icone(c: Couleur) -> HICON {
        let taille = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize;
        let (r, g, b) = match c {
            Couleur::Vert => (0x2e, 0x9d, 0x4f),
            Couleur::Bleu => (0x2f, 0x6f, 0xd6),
            Couleur::Orange => (0xf0, 0x8c, 0x1a),
            Couleur::Rouge => (0xd6, 0x2f, 0x2f),
        };
        let centre = taille as f32 / 2.0;
        let rayon = taille as f32 * 0.45;
        let mut couleurs = vec![0u8; taille * taille * 4];
        for y in 0..taille {
            for x in 0..taille {
                let dx = x as f32 + 0.5 - centre;
                let dy = y as f32 + 0.5 - centre;
                let d = (dx * dx + dy * dy).sqrt();
                let couverture = (rayon + 0.5 - d).clamp(0.0, 1.0);
                let bord = if d > rayon - 1.5 { 0.65 } else { 1.0 };
                let p = (y * taille + x) * 4;
                couleurs[p] = (b as f32 * bord) as u8;
                couleurs[p + 1] = (g as f32 * bord) as u8;
                couleurs[p + 2] = (r as f32 * bord) as u8;
                couleurs[p + 3] = (couverture * 255.0) as u8;
            }
        }
        let masque = vec![0u8; taille.div_ceil(16) * 2 * taille];
        unsafe {
            CreateIcon(
                GetModuleHandleW(std::ptr::null()),
                taille as i32,
                taille as i32,
                1,
                32,
                masque.as_ptr(),
                couleurs.as_ptr(),
            )
        }
    }

    fn ajouter() -> bool {
        if !avec_icone() {
            return true;
        }
        let (couleur, texte) = DERNIER
            .lock()
            .ok()
            .and_then(|d| d.clone())
            .unwrap_or((Couleur::Vert, "Orchestrateur de charge".into()));
        let mut d = donnees();
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = WM_ICONE;
        d.hIcon = icone(couleur);
        copier(&mut d.szTip, &texte);
        unsafe {
            let ok = Shell_NotifyIconW(NIM_ADD, &d) != 0;
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &d);
            ok
        }
    }

    fn supprimer() {
        if avec_icone() {
            let d = donnees();
            unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
        }
    }

    pub fn maj(couleur: Couleur, texte: &str) {
        if !avec_icone() {
            return;
        }
        {
            let Ok(mut dernier) = DERNIER.lock() else { return };
            if dernier.as_ref().is_some_and(|(c, t)| *c == couleur && t == texte) {
                return;
            }
            *dernier = Some((couleur, texte.to_owned()));
        }
        let mut d = donnees();
        d.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.hIcon = icone(couleur);
        copier(&mut d.szTip, texte);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn bulle(titre: &str, texte: &str, alerte: bool) {
        if TEST.load(Ordering::SeqCst) || !avec_icone() {
            return;
        }
        let mut d = donnees();
        d.uFlags = NIF_INFO;
        copier(&mut d.szInfoTitle, titre);
        copier(&mut d.szInfo, texte);
        d.dwInfoFlags = if alerte { NIIF_WARNING } else { NIIF_INFO | NIIF_NOSOUND };
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn fenetre(ouvrir: bool) {
        unsafe { PostMessageW(cachee(), WM_FENETRE, ouvrir as usize, 0) };
    }

    pub fn quitter() {
        unsafe { PostMessageW(cachee(), WM_CLOSE, 0, 0) };
    }

    pub fn ouvrir_journal() {
        if let Some(Some(chemin)) = JOURNAL.get() {
            let fichier = large(&chemin.to_string_lossy());
            let verbe = large("open");
            unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verbe.as_ptr(),
                    fichier.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                )
            };
        }
    }

    fn file_gelee() -> bool {
        partage().is_some_and(|p| p.instantane().gel_manuel.is_some())
    }

    unsafe fn menu(h: HWND) {
        unsafe {
            let m = CreatePopupMenu();
            let ouvrir = large("Ouvrir");
            let gel = large(if file_gelee() { "Dégeler la file" } else { "Geler la file" });
            let journal = large("Ouvrir le journal");
            let quitter = large("Quitter");
            AppendMenuW(m, MF_STRING, CMD_OUVRIR, ouvrir.as_ptr());
            AppendMenuW(m, MF_STRING, CMD_GEL, gel.as_ptr());
            AppendMenuW(m, MF_STRING, CMD_JOURNAL, journal.as_ptr());
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(m, MF_STRING, CMD_QUITTER, quitter.as_ptr());
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            // Sans premier plan, le menu ne se referme pas quand on clique ailleurs.
            SetForegroundWindow(h);
            TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, 0, h, std::ptr::null());
            PostMessageW(h, WM_NULL, 0, 0);
            DestroyMenu(m);
        }
    }

    fn journaliser_ici(texte: &str) {
        if let Some(Some(chemin)) = JOURNAL.get()
            && let Some(dossier) = chemin.parent()
        {
            journaliser(dossier, texte);
        }
    }

    unsafe extern "system" fn procedure(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_ICONE => {
                    match (lparam & 0xffff) as u32 {
                        WM_CONTEXTMENU | WM_RBUTTONUP => menu(h),
                        WM_LBUTTONDBLCLK | NIN_SELECT => fenetre::ouvrir(true),
                        _ => {}
                    }
                    0
                }
                WM_FENETRE => {
                    if wparam != 0 {
                        fenetre::ouvrir(!TEST.load(Ordering::SeqCst));
                    } else {
                        fenetre::fermer();
                    }
                    0
                }
                WM_COMMAND => {
                    match wparam & 0xffff {
                        CMD_OUVRIR => fenetre::ouvrir(true),
                        CMD_GEL => {
                            if let Some(p) = partage() {
                                p.geler(!file_gelee(), "menu de l'icône");
                            }
                        }
                        CMD_JOURNAL => ouvrir_journal(),
                        CMD_QUITTER => {
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
                        supprimer();
                        journaliser_ici("ARRÊT : fin de session Windows.");
                    }
                    0
                }
                WM_DESTROY => {
                    fenetre::fermer();
                    supprimer();
                    CACHEE.store(0, Ordering::SeqCst);
                    PostQuitMessage(0);
                    0
                }
                m if m != 0 && m == BARRE_RECREEE.load(Ordering::SeqCst) => {
                    ajouter();
                    0
                }
                _ => DefWindowProcW(h, msg, wparam, lparam),
            }
        }
    }

    /// Crée la fenêtre cachée (messages, arrêt propre) et l'icône hors mode test ; sur le fil de `boucle`.
    pub fn demarrer(journal: Option<PathBuf>) -> bool {
        let _ = JOURNAL.set(journal);
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware() };
        ICONES.get_or_init(|| {
            [Couleur::Vert, Couleur::Bleu, Couleur::Orange, Couleur::Rouge].map(|c| creer_icone(c) as usize)
        });
        let classe = large("OrchestrateurCharge");
        let titre = large("Orchestrateur de charge (service)");
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(procedure);
            wc.hInstance = instance;
            wc.lpszClassName = classe.as_ptr();
            RegisterClassW(&wc);
            BARRE_RECREEE.store(RegisterWindowMessageW(large("TaskbarCreated").as_ptr()), Ordering::SeqCst);
            // Fenêtre de premier niveau jamais affichée : `taskkill` sans /F lui envoie WM_CLOSE.
            let h = CreateWindowExW(
                0,
                classe.as_ptr(),
                titre.as_ptr(),
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
            CACHEE.store(h as usize, Ordering::SeqCst);
        }
        ajouter()
    }

    pub fn boucle() {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if !fenetre::message_de_dialogue(&msg) {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            // WM_DESTROY a déjà retiré l'icône de notification : ses images ne servent plus.
            for &h in ICONES.get().into_iter().flatten() {
                if h != 0 {
                    DestroyIcon(h as HICON);
                }
            }
        }
        fenetre::liberer();
    }

    /// Erreur de démarrage : en mode test, stderr seulement ; sinon icône rouge et bulle quelques secondes.
    pub fn avertir(titre: &str, texte: &str) {
        if TEST.load(Ordering::SeqCst) {
            eprintln!("{titre} : {texte}");
            return;
        }
        if let Ok(mut d) = DERNIER.lock() {
            *d = Some((Couleur::Rouge, format!("Orchestrateur : {titre}")));
        }
        if cachee().is_null() && !demarrer(None) {
            return;
        }
        bulle(titre, texte, true);
        std::thread::sleep(Duration::from_secs(8));
        supprimer();
    }
}
