use std::cell::{Cell, RefCell};
use std::sync::Once;

use commun::format::{coeurs, duree, go};
use commun::modele::{EtatTicket, Fin, Statut, VueReserve};
use commun::planif::{NB_RESERVES, wsl_frais};
use commun::systeme::hhmm;
use windows_sys::Win32::Foundation::{COLORREF, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DT_END_ELLIPSIS, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FillRect, FrameRect, GetDC, GetDeviceCaps,
    HBRUSH, HDC, HFONT, InvalidateRect, LOGPIXELSY, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::System::ApplicationInstallationAndServicing::{ACTCTXW, ActivateActCtx, CreateActCtxW};
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

use crate::serveur::{Cloture, fin_du_journal, nom_etat};
use crate::ui::{self, partage};

const ID_LISTE: usize = 100;
const ID_ANNULER: usize = 101;
const ID_LIBERER: usize = 102;
const ID_GEL: usize = 103;
const ID_JOURNAL_BOUTON: usize = 104;
const ID_JOURNAL: usize = 105;
const ID_FORCER: usize = 106;
const MINUTERIE: usize = 1;
const LIGNES_JOURNAL: usize = 20;

const COULEUR_UTILISE: COLORREF = 0x00b0_7a4a;
const COULEUR_RESERVE: COLORREF = 0x0030_a0f0;
const COULEUR_MARGE: COLORREF = 0x00a8_a8e8;
const COULEUR_LIBRE: COLORREF = 0x00ee_eeee;
const COULEUR_CADRE: COLORREF = 0x0088_8888;

const COLONNES: [(&str, i32); 11] = [
    ("N°", 45),
    ("Session", 140),
    ("Dossier", 120),
    ("Branche / worktree", 190),
    ("État", 115),
    ("Côté", 60),
    ("RAM", 60),
    ("CPU", 70),
    ("Durée", 60),
    ("Depuis", 55),
    ("Raison", 900),
];

thread_local! {
    static PRINCIPALE: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static LISTE: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static BOUTONS: Cell<[HWND; 5]> = const { Cell::new([std::ptr::null_mut(); 5]) };
    static JOURNAL: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static POLICE: Cell<HFONT> = const { Cell::new(std::ptr::null_mut()) };
    static ECHELLE: Cell<f32> = const { Cell::new(1.0) };
    static BARRES: Cell<RECT> = const { Cell::new(RECT { left: 0, top: 0, right: 0, bottom: 0 }) };
    static STATUT: RefCell<Option<Statut>> = const { RefCell::new(None) };
    static TEXTE_JOURNAL: RefCell<String> = const { RefCell::new(String::new()) };
}

static CLASSE: Once = Once::new();
static STYLES: Once = Once::new();

fn large(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn s(v: i32) -> i32 {
    (v as f32 * ECHELLE.with(Cell::get)).round() as i32
}

/// Contrôles communs v6 (style Windows actuel) sans ressource embarquée : contexte d'activation depuis un manifeste écrit à côté de l'état.
fn activer_styles() {
    STYLES.call_once(|| {
        let Some(p) = partage() else { return };
        let chemin = p.dossier().join("controles-communs.manifest");
        let manifeste = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"><dependency><dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/></dependentAssembly></dependency></assembly>
"#;
        if std::fs::write(&chemin, manifeste).is_err() {
            return;
        }
        let source = large(&chemin.to_string_lossy());
        let mut ctx: ACTCTXW = unsafe { std::mem::zeroed() };
        ctx.cbSize = size_of::<ACTCTXW>() as u32;
        ctx.lpSource = source.as_ptr();
        unsafe {
            let h = CreateActCtxW(&ctx);
            if h != INVALID_HANDLE_VALUE {
                let mut cookie = 0usize;
                ActivateActCtx(h, &mut cookie);
            }
            let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_LISTVIEW_CLASSES };
            InitCommonControlsEx(&icc);
        }
    });
}

pub fn message_de_dialogue(msg: &MSG) -> bool {
    let h = PRINCIPALE.with(Cell::get);
    !h.is_null() && unsafe { IsDialogMessageW(h, msg) } != 0
}

/// Ouvre la fenêtre (ou la ramène au premier plan) ; `visible` faux en mode test : créée mais jamais affichée.
pub fn ouvrir(visible: bool) {
    let existante = PRINCIPALE.with(Cell::get);
    if !existante.is_null() {
        if visible {
            unsafe {
                ShowWindow(existante, SW_SHOWNORMAL);
                SetForegroundWindow(existante);
            }
        }
        return;
    }
    activer_styles();
    let classe = large("OrchestrateurFenetre");
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        CLASSE.call_once(|| {
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(procedure);
            wc.hInstance = instance;
            wc.lpszClassName = classe.as_ptr();
            wc.hbrBackground = (15 + 1) as HBRUSH;
            wc.hCursor = windows_sys::Win32::UI::WindowsAndMessaging::LoadCursorW(
                std::ptr::null_mut(),
                windows_sys::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            );
            RegisterClassW(&wc);
        });
        let ecran = GetDC(std::ptr::null_mut());
        ECHELLE.with(|e| e.set(GetDeviceCaps(ecran, LOGPIXELSY as i32) as f32 / 96.0));
        ReleaseDC(std::ptr::null_mut(), ecran);
        let titre = large("Orchestrateur de charge");
        // Mode test : composée hors de l'écran, sans barre des tâches ni activation, pour être capturable sans rien montrer.
        let (ex, x, y) = if visible {
            (WS_EX_CONTROLPARENT, CW_USEDEFAULT, CW_USEDEFAULT)
        } else {
            (WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, -20000, -20000)
        };
        let h = CreateWindowExW(
            ex,
            classe.as_ptr(),
            titre.as_ptr(),
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

pub fn fermer() {
    let h = PRINCIPALE.with(Cell::get);
    if !h.is_null() {
        unsafe { DestroyWindow(h) };
    }
}

unsafe fn enfant(parent: HWND, ex: u32, classe: &str, texte: &str, style: u32, id: usize) -> HWND {
    let c = large(classe);
    let t = large(texte);
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
        SendMessageW(h, WM_SETFONT, POLICE.with(Cell::get) as usize, 1);
        h
    }
}

unsafe fn creer_enfants(h: HWND) {
    unsafe {
        let nom = large("Segoe UI");
        let police = CreateFontW(-s(12), 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, nom.as_ptr());
        POLICE.with(|p| p.set(police));
        let liste = enfant(
            h,
            WS_EX_CLIENTEDGE,
            "SysListView32",
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            ID_LISTE,
        );
        SendMessageW(
            liste,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES | LVS_EX_DOUBLEBUFFER) as isize,
        );
        for (i, (titre, largeur)) in COLONNES.iter().enumerate() {
            let mut t = large(titre);
            let mut col: LVCOLUMNW = std::mem::zeroed();
            col.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM;
            col.cx = s(*largeur);
            col.pszText = t.as_mut_ptr();
            col.iSubItem = i as i32;
            SendMessageW(liste, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
        }
        LISTE.with(|l| l.set(liste));
        let bouton = |texte: &str, id: usize| enfant(h, 0, "BUTTON", texte, WS_TABSTOP | BS_PUSHBUTTON as u32, id);
        BOUTONS.with(|b| {
            b.set([
                bouton("Annuler la demande…", ID_ANNULER),
                bouton("Libérer le créneau…", ID_LIBERER),
                bouton("Forcer le passage…", ID_FORCER),
                bouton("Geler la file", ID_GEL),
                bouton("Ouvrir le journal", ID_JOURNAL_BOUTON),
            ])
        });
        let journal = enfant(
            h,
            WS_EX_CLIENTEDGE,
            "EDIT",
            "",
            WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
            ID_JOURNAL,
        );
        JOURNAL.with(|j| j.set(journal));
    }
}

unsafe fn disposer(h: HWND) {
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(h, &mut rc);
        let m = s(10);
        let largeur = rc.right - 2 * m;
        let hauteur_liste = ((rc.bottom - 2 * m) as f32 * 0.38).max(s(140) as f32) as i32;
        let mut y = m;
        MoveWindow(LISTE.with(Cell::get), m, y, largeur, hauteur_liste, 1);
        y += hauteur_liste + s(8);
        let mut x = m;
        for b in BOUTONS.with(Cell::get) {
            MoveWindow(b, x, y, s(170), s(28), 1);
            x += s(178);
        }
        y += s(28) + s(10);
        let barres = RECT { left: m, top: y, right: m + largeur, bottom: y + s(22) + NB_RESERVES as i32 * s(26) };
        BARRES.with(|b| b.set(barres));
        y = barres.bottom + s(8);
        MoveWindow(JOURNAL.with(Cell::get), m, y, largeur, (rc.bottom - m - y).max(s(60)), 1);
        InvalidateRect(h, std::ptr::null(), 1);
    }
}

fn selection() -> Option<u64> {
    let liste = LISTE.with(Cell::get);
    unsafe {
        let indice = SendMessageW(liste, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
        if indice < 0 {
            return None;
        }
        let mut item: LVITEMW = std::mem::zeroed();
        item.mask = LVIF_PARAM;
        item.iItem = indice as i32;
        SendMessageW(liste, LVM_GETITEMW, 0, &mut item as *mut _ as isize);
        Some(item.lParam as u64)
    }
}

fn etat_de(id: u64) -> Option<(EtatTicket, String)> {
    STATUT.with(|st| {
        st.borrow()
            .as_ref()
            .and_then(|s| s.tickets.iter().find(|t| t.id == id).map(|t| (t.etat, format!("{} — {}", t.session, t.description))))
    })
}

fn maj_boutons() {
    let [annuler, liberer, forcer, _, _] = BOUTONS.with(Cell::get);
    let etat = selection().and_then(etat_de).map(|(e, _)| e);
    unsafe {
        EnableWindow(annuler, (etat == Some(EtatTicket::Attente)) as i32);
        EnableWindow(forcer, (etat == Some(EtatTicket::Attente)) as i32);
        EnableWindow(liberer, matches!(etat, Some(EtatTicket::Actif | EtatTicket::Orphelin)) as i32);
    }
}

unsafe fn texte_cellule(liste: HWND, ligne: i32, colonne: i32, texte: &str) {
    let mut t = large(texte);
    unsafe {
        let mut item: LVITEMW = std::mem::zeroed();
        item.iSubItem = colonne;
        item.pszText = t.as_mut_ptr();
        SendMessageW(liste, LVM_SETITEMTEXTW, ligne as usize, &item as *const _ as isize);
    }
}

fn rafraichir(h: HWND) {
    let Some(p) = partage() else { return };
    let statut = p.instantane();
    let lignes = fin_du_journal(&p.dossier(), LIGNES_JOURNAL);
    let liste = LISTE.with(Cell::get);
    let choisi = selection();
    STATUT.with(|s| *s.borrow_mut() = Some(statut.clone()));
    unsafe {
        SendMessageW(liste, WM_SETREDRAW, 0, 0);
        SendMessageW(liste, LVM_DELETEALLITEMS, 0, 0);
        for (i, t) in statut.tickets.iter().enumerate() {
            let mut numero = large(&format!("#{}", t.id));
            let mut item: LVITEMW = std::mem::zeroed();
            item.mask = LVIF_TEXT | LVIF_PARAM;
            item.iItem = i as i32;
            item.pszText = numero.as_mut_ptr();
            item.lParam = t.id as isize;
            if choisi == Some(t.id) {
                item.mask |= LVIF_STATE;
                item.state = LVIS_SELECTED | LVIS_FOCUSED;
                item.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
            }
            let ligne = SendMessageW(liste, LVM_INSERTITEMW, 0, &item as *const _ as isize) as i32;
            let depuis = t.accorde.unwrap_or(t.cree);
            let etat = format!(
                "{}{}{}{}",
                nom_etat(t.etat),
                if t.bail_depasse { " (bail dépassé)" } else { "" },
                if t.force { " (forcé)" } else { "" },
                if t.manuel { " (manuel)" } else { "" }
            );
            let cellules = [
                t.session.clone(),
                t.projet.clone().unwrap_or_else(|| "—".into()),
                t.branche.clone().unwrap_or_else(|| "—".into()),
                etat,
                t.cote.nom().to_owned(),
                go(t.besoin.ram_mo as i64),
                coeurs(t.besoin.cpu as i64),
                duree(t.besoin.duree_s),
                hhmm(depuis),
                t.phrase.clone(),
            ];
            for (c, texte) in cellules.iter().enumerate() {
                texte_cellule(liste, ligne, c as i32 + 1, texte);
            }
        }
        SendMessageW(liste, WM_SETREDRAW, 1, 0);
        InvalidateRect(liste, std::ptr::null(), 1);

        let texte = lignes.join("\r\n");
        let change = TEXTE_JOURNAL.with(|j| {
            let mut j = j.borrow_mut();
            let change = *j != texte;
            *j = texte.clone();
            change
        });
        if change {
            let journal = JOURNAL.with(Cell::get);
            SetWindowTextW(journal, large(&texte).as_ptr());
            SendMessageW(journal, WM_VSCROLL, SB_BOTTOM as usize, 0);
        }
        let [_, _, _, gel, _] = BOUTONS.with(Cell::get);
        SetWindowTextW(gel, large(if statut.gel_manuel.is_some() { "Dégeler la file" } else { "Geler la file" }).as_ptr());
        let attente = statut.tickets.iter().filter(|t| t.etat == EtatTicket::Attente).count();
        let actifs = statut.tickets.len() - attente;
        let titre = format!(
            "Orchestrateur de charge — {actifs} actif(s), {attente} en attente{}",
            if statut.gel_manuel.is_some() { " — FILE GELÉE" } else { "" }
        );
        SetWindowTextW(h, large(&titre).as_ptr());
        let barres = BARRES.with(Cell::get);
        InvalidateRect(h, &barres, 1);
    }
    maj_boutons();
}

unsafe fn rectangle(hdc: HDC, r: &RECT, couleur: COLORREF) {
    unsafe {
        let pinceau = CreateSolidBrush(couleur);
        FillRect(hdc, r, pinceau);
        DeleteObject(pinceau as _);
    }
}

unsafe fn texte(hdc: HDC, r: &RECT, t: &str) {
    let mut w: Vec<u16> = t.encode_utf16().collect();
    let mut r = *r;
    unsafe { DrawTextW(hdc, w.as_mut_ptr(), w.len() as i32, &mut r, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS) };
}

/// (utilisé, réservé, marge, total) d'une réserve, et le texte qui l'accompagne.
fn valeurs(r: &VueReserve, statut: &Statut) -> (i64, i64, i64, i64, String) {
    if r.cle == "cpu" {
        let charge = wsl_frais(&statut.mesures, statut.maintenant).map(|w| w.charge1);
        let utilise = charge.map_or(0, |c| (c as f64).round() as i64).min(r.total);
        let mesure = charge.map_or("charge inconnue".to_owned(), |c| format!("charge {c:.1}"));
        let texte = format!(
            "{mesure} · réservé {} · marge {} · libre {} (sur {})",
            coeurs(r.reserve),
            coeurs(r.marge),
            r.libre_lisible(),
            coeurs(r.total)
        );
        return (utilise, r.reserve, r.marge, r.total, texte);
    }
    if r.cle == "cpu_windows" {
        let utilise = (statut.mesures.cpu_pct as f64 * r.total as f64 / 100.0).round() as i64;
        let texte = format!(
            "CPU {:.0} % · réservé {} · marge {} · libre {} (sur {})",
            statut.mesures.cpu_pct,
            coeurs(r.reserve),
            coeurs(r.marge),
            r.libre_lisible(),
            coeurs(r.total)
        );
        return (utilise, r.reserve, r.marge, r.total, texte);
    }
    if r.cle == "ram_wsl" && statut.mesures.vmmem.is_none() {
        return (0, r.reserve, r.marge, r.total, "WSL arrêté".into());
    }
    let source = match r.cle.as_str() {
        "ram_wsl" if wsl_frais(&statut.mesures, statut.maintenant).is_some() => " · selon MemAvailable",
        "ram_wsl" => " · selon 12 Go − vmmemWSL (pas de mesure WSL récente)",
        _ => "",
    };
    let utilise = (r.total - r.disponible).max(0);
    let texte = format!(
        "utilisé {} · réservé {} · marge {} · libre {} (sur {}){source}",
        go(utilise),
        go(r.reserve),
        go(r.marge),
        r.libre_lisible(),
        go(r.total)
    );
    (utilise, r.reserve, r.marge, r.total, texte)
}

unsafe fn dessiner_barres(hdc: HDC) {
    let Some(statut) = STATUT.with(|s| s.borrow().clone()) else { return };
    let zone = BARRES.with(Cell::get);
    unsafe {
        let ancienne = SelectObject(hdc, POLICE.with(Cell::get) as _);
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, 0x0020_2020);
        let mut x = zone.left + s(115);
        for (nom, couleur) in [("utilisé", COULEUR_UTILISE), ("réservé", COULEUR_RESERVE), ("marge", COULEUR_MARGE), ("libre", COULEUR_LIBRE)] {
            let carre = RECT { left: x, top: zone.top + s(5), right: x + s(12), bottom: zone.top + s(17) };
            rectangle(hdc, &carre, couleur);
            texte(hdc, &RECT { left: x + s(16), top: zone.top, right: x + s(90), bottom: zone.top + s(22) }, nom);
            x += s(95);
        }
        let largeur_barre = ((zone.right - zone.left - s(115)) as f32 * 0.42) as i32;
        for (i, r) in statut.reserves.iter().enumerate() {
            let y = zone.top + s(22) + i as i32 * s(26);
            texte(hdc, &RECT { left: zone.left, top: y, right: zone.left + s(110), bottom: y + s(26) }, &r.nom);
            let barre = RECT { left: zone.left + s(115), top: y + s(5), right: zone.left + s(115) + largeur_barre, bottom: y + s(21) };
            rectangle(hdc, &barre, COULEUR_LIBRE);
            let (utilise, reserve, marge, total, legende) = valeurs(r, &statut);
            let mut debut = barre.left;
            for (v, couleur) in [(utilise, COULEUR_UTILISE), (reserve, COULEUR_RESERVE), (marge, COULEUR_MARGE)] {
                let px = if total > 0 { (v.max(0) as f64 * largeur_barre as f64 / total as f64) as i32 } else { 0 };
                let fin = (debut + px).min(barre.right);
                if fin > debut {
                    rectangle(hdc, &RECT { left: debut, top: barre.top, right: fin, bottom: barre.bottom }, couleur);
                }
                debut = fin;
            }
            let cadre = CreateSolidBrush(COULEUR_CADRE);
            FrameRect(hdc, &barre, cadre);
            DeleteObject(cadre as _);
            texte(hdc, &RECT { left: barre.right + s(10), top: y, right: zone.right, bottom: y + s(26) }, &legende);
        }
        SelectObject(hdc, ancienne);
    }
}

fn confirmer(h: HWND, id: u64, liberer: bool) {
    let Some((etat, quoi)) = etat_de(id) else { return };
    let question = if liberer {
        format!(
            "Libérer le créneau du ticket #{id} ({}) ?\n\n{quoi}\n\nSa réservation disparaît : si la tâche tourne encore, la machine peut saturer.",
            nom_etat(etat)
        )
    } else {
        format!("Annuler la demande #{id} ?\n\n{quoi}")
    };
    let titre = large("Orchestrateur de charge");
    let reponse = unsafe { MessageBoxW(h, large(&question).as_ptr(), titre.as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) };
    if reponse == IDYES
        && let Some(p) = partage()
    {
        p.clore(id, &Fin::default(), Cloture::Fenetre);
    }
}

fn confirmer_forcage(h: HWND, id: u64) {
    let Some(p) = partage() else { return };
    let Some(question) = p.apercu_forcage(id) else { return };
    let question = format!("{question}\n\nLes autres tickets gardent leur place.");
    let titre = large("Forcer le passage");
    let reponse = unsafe {
        MessageBoxW(h, large(&question).as_ptr(), titre.as_ptr(), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2)
    };
    if reponse == IDYES
        && let Err(e) = p.forcer(id)
    {
        unsafe { MessageBoxW(h, large(&e).as_ptr(), titre.as_ptr(), MB_ICONWARNING) };
    }
}

unsafe extern "system" fn procedure(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                PRINCIPALE.with(|p| p.set(h));
                creer_enfants(h);
                disposer(h);
                rafraichir(h);
                SetTimer(h, MINUTERIE, 2000, None);
                0
            }
            WM_SIZE => {
                if !LISTE.with(Cell::get).is_null() {
                    disposer(h);
                }
                0
            }
            WM_TIMER => {
                rafraichir(h);
                0
            }
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(h, &mut ps);
                dessiner_barres(hdc);
                EndPaint(h, &ps);
                0
            }
            WM_PRINTCLIENT => {
                let hdc = wparam as HDC;
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(h, &mut rc);
                FillRect(hdc, &rc, (15 + 1) as HBRUSH);
                dessiner_barres(hdc);
                0
            }
            WM_NOTIFY => {
                let entete = &*(lparam as *const NMHDR);
                if entete.idFrom == ID_LISTE && entete.code == LVN_ITEMCHANGED {
                    maj_boutons();
                }
                0
            }
            WM_COMMAND => {
                match wparam & 0xffff {
                    ID_ANNULER => {
                        if let Some(id) = selection() {
                            confirmer(h, id, false);
                        }
                    }
                    ID_LIBERER => {
                        if let Some(id) = selection() {
                            confirmer(h, id, true);
                        }
                    }
                    ID_FORCER => {
                        if let Some(id) = selection() {
                            confirmer_forcage(h, id);
                        }
                    }
                    ID_GEL => {
                        let gele = STATUT.with(|s| s.borrow().as_ref().is_some_and(|s| s.gel_manuel.is_some()));
                        if let Some(p) = partage() {
                            p.geler(!gele, "fenêtre");
                        }
                    }
                    ID_JOURNAL_BOUTON => ui::ouvrir_journal(),
                    _ => return 0,
                }
                rafraichir(h);
                0
            }
            WM_CLOSE => {
                DestroyWindow(h);
                0
            }
            WM_DESTROY => {
                KillTimer(h, MINUTERIE);
                let police = POLICE.with(|p| p.replace(std::ptr::null_mut()));
                if !police.is_null() {
                    DeleteObject(police as _);
                }
                PRINCIPALE.with(|p| p.set(std::ptr::null_mut()));
                LISTE.with(|l| l.set(std::ptr::null_mut()));
                STATUT.with(|s| *s.borrow_mut() = None);
                TEXTE_JOURNAL.with(|j| *j.borrow_mut() = String::new());
                0
            }
            _ => DefWindowProcW(h, msg, wparam, lparam),
        }
    }
}
