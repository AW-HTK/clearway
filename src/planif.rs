use std::collections::BTreeMap;

use crate::config::Config;
use crate::format::{coeurs, duree, go, option_ram};
use crate::modele::{
    Besoin, CauseOrphelin, Cote, Etat, EtatTicket, Mesures, MesuresWsl, Raison, Ticket, TypeDecision, VueReserve,
};
use crate::systeme::hhmm;

pub const FRAICHEUR_WSL_S: u64 = 30;
pub const SWAP_CALME_S: u64 = 60;

/// Swap plein mais calme : à signaler (statut, infobulle) sans geler.
pub fn swap_calme(e: &Etat, m: &Mesures, c: &Config, maintenant: u64) -> Option<String> {
    let w = wsl_frais(m, maintenant)?;
    (!e.pression_swap && w.swap_pct() >= c.swap_seuil_pct).then(|| format!("swap WSL plein à {} % (calme)", w.swap_pct()))
}

/// Un swap plein ne dit rien seul (pages anciennes) : il faut qu'il sorte vraiment ou que la mémoire manque.
fn mettre_a_jour_pression_swap(e: &mut Etat, m: &Mesures, c: &Config, maintenant: u64) {
    let Some(w) = wsl_frais(m, maintenant) else {
        e.pression_swap = false;
        e.swap_calme_depuis = None;
        return;
    };
    let actif = m.swap_sortie_ps > c.swap_debit_sortie as f32
        || w.mem_dispo_mo < c.marge_ram_wsl_mo + 1024;
    let pct = w.swap_pct();
    if !e.pression_swap {
        e.pression_swap = pct >= c.swap_seuil_pct && actif;
        e.swap_calme_depuis = None;
        return;
    }
    if pct < c.swap_seuil_levee_pct {
        e.pression_swap = false;
        e.swap_calme_depuis = None;
    } else if actif {
        e.swap_calme_depuis = None;
    } else {
        let depuis = *e.swap_calme_depuis.get_or_insert(maintenant);
        if maintenant.saturating_sub(depuis) >= SWAP_CALME_S {
            e.pression_swap = false;
            e.swap_calme_depuis = None;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reserve {
    RamWindows,
    RamWsl,
    Cpu,
    CpuWindows,
}

pub const RESERVES: [Reserve; 4] = [Reserve::RamWindows, Reserve::RamWsl, Reserve::Cpu, Reserve::CpuWindows];
pub const NB_RESERVES: usize = RESERVES.len();

impl Reserve {
    pub fn cle(self) -> &'static str {
        match self {
            Reserve::RamWindows => "ram_windows",
            Reserve::RamWsl => "ram_wsl",
            Reserve::Cpu => "cpu",
            Reserve::CpuWindows => "cpu_windows",
        }
    }

    pub fn nom(self) -> &'static str {
        match self {
            Reserve::RamWindows => "RAM Windows",
            Reserve::RamWsl => "RAM WSL",
            Reserve::Cpu => "CPU WSL",
            Reserve::CpuWindows => "CPU Windows",
        }
    }

    fn insuffisante(self) -> &'static str {
        match self {
            Reserve::Cpu | Reserve::CpuWindows => "insuffisant",
            _ => "insuffisante",
        }
    }

    fn i(self) -> usize {
        self as usize
    }

    pub fn est_ram(self) -> bool {
        matches!(self, Reserve::RamWindows | Reserve::RamWsl)
    }

    /// Toute tâche débite la RAM et le CPU de Windows (les vCPU de la VM tournent sur l'hôte) ;
    /// seules les tâches `wsl` débitent la RAM et le CPU de WSL.
    pub fn besoin(self, cote: Cote, b: &Besoin) -> i64 {
        match self {
            Reserve::RamWindows => b.ram_mo as i64,
            Reserve::RamWsl if cote == Cote::Wsl => b.ram_mo as i64,
            Reserve::Cpu if cote == Cote::Wsl => b.cpu as i64,
            Reserve::RamWsl | Reserve::Cpu => 0,
            Reserve::CpuWindows => b.cpu as i64,
        }
    }

    pub fn quantite(self, v: i64) -> String {
        if self.est_ram() { go(v) } else { coeurs(v) }
    }
}

/// Ce qu'un ticket prend vraiment à une réserve. Une tâche WSL ne prend à Windows que la croissance encore
/// possible de la VM jusqu'à son plafond : au-delà, elle se sert dans la mémoire déjà comptée dans `vmmemWSL`.
pub fn debit(r: Reserve, cote: Cote, b: &Besoin, m: &Mesures, c: &Config) -> i64 {
    let besoin = r.besoin(cote, b);
    if r == Reserve::RamWindows && cote == Cote::Wsl {
        let croissance = c.wsl_ram_max_mo.saturating_sub(m.vmmem.map_or(0, |v| v.rss_mo)) as i64;
        besoin.min(croissance)
    } else {
        besoin
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Niveau {
    pub disponible: i64,
    pub reserve: i64,
    pub par: Vec<u64>,
    pub marge: i64,
}

impl Niveau {
    pub fn libre(&self) -> i64 {
        self.disponible - self.reserve - self.marge
    }
}

pub fn wsl_frais(m: &Mesures, maintenant: u64) -> Option<&MesuresWsl> {
    m.wsl
        .as_ref()
        .filter(|w| maintenant.saturating_sub(w.quand) <= FRAICHEUR_WSL_S)
}

/// Mesure en direct d'une réserve, sans les réservations.
pub fn disponible(r: Reserve, m: &Mesures, c: &Config, maintenant: u64) -> i64 {
    match r {
        Reserve::RamWindows => m.ram_dispo_mo as i64,
        // vmmemWSL garde le cache de pages récupérable : MemAvailable récent fait foi, 12 Go − vmmemWSL n'est qu'un repli.
        // La croissance de la VM reste comptée côté Windows par la RAM Windows mesurée.
        Reserve::RamWsl => match (wsl_frais(m, maintenant), m.vmmem) {
            (Some(w), _) => w.mem_dispo_mo as i64,
            (None, Some(v)) => c.wsl_ram_max_mo.saturating_sub(v.rss_mo) as i64,
            (None, None) => c.wsl_ram_max_mo as i64,
        },
        Reserve::Cpu => c.cpu_total as i64,
        Reserve::CpuWindows => coeurs_windows(m, c),
    }
}

/// Budget CPU Windows : `cpu.total_windows` s'il est réglé, sinon les cœurs logiques mesurés.
pub fn coeurs_windows(m: &Mesures, c: &Config) -> i64 {
    c.cpu_total_windows.map(i64::from).unwrap_or(if m.coeurs_windows > 0 { m.coeurs_windows as i64 } else { 16 })
}

fn marge(r: Reserve, c: &Config) -> i64 {
    match r {
        Reserve::RamWindows => c.marge_ram_windows_mo as i64,
        Reserve::RamWsl => c.marge_ram_wsl_mo as i64,
        Reserve::Cpu | Reserve::CpuWindows => c.marge_cpu as i64,
    }
}

/// Les tickets actifs et orphelins réservent leur besoin déclaré en plus des mesures : avant l'étape 2,
/// on ne sait pas s'ils ont atteint leur pic.
/// Ce qu'un ticket actif réserve encore sur une réserve. Pendant `delai.montee` après le feu vert, tout son débit ;
/// ensuite, seulement ce qu'il n'a pas encore fait monter, car le reste est déjà dans les mesures. Le CPU n'est pas
/// mesuré par réserve : sa réservation reste entière. Un orphelin garde la sienne entière.
pub fn reservation(t: &Ticket, r: Reserve, m: &Mesures, c: &Config, maintenant: u64) -> i64 {
    let b = debit(r, t.cote, &t.besoin, m, c);
    if b == 0 || !r.est_ram() || t.etat != EtatTicket::Actif {
        return b;
    }
    if t.accorde.is_none_or(|a| maintenant.saturating_sub(a) < c.delai_montee_s) {
        return b;
    }
    // Tickets accordés avant ces relevés : seule la RAM disponible de leur réserve principale a été notée.
    let principale = t.dispo_accord_mo.map(|d| d as i64);
    let base_wsl = t.base_ram_wsl_mo.or(principale.filter(|_| t.cote == Cote::Wsl));
    let base_windows = t.base_ram_windows_mo.or(principale.filter(|_| t.cote == Cote::Windows));
    let baisse_wsl = base_wsl.map(|base| base - disponible(Reserve::RamWsl, m, c, maintenant));
    let monte = match (r, t.cote) {
        (Reserve::RamWindows, Cote::Wsl) => t
            .base_vmmem_mo
            .zip(m.vmmem)
            .map(|(base, v)| v.rss_mo.saturating_sub(base) as i64)
            .or(baisse_wsl),
        (Reserve::RamWindows, _) => base_windows.map(|base| base - m.ram_dispo_mo as i64),
        (Reserve::RamWsl, _) => baisse_wsl,
        _ => None,
    };
    match monte {
        Some(monte) => (b - monte.clamp(0, b)).max(0),
        None => b,
    }
}

pub fn niveaux(e: &Etat, m: &Mesures, c: &Config, maintenant: u64) -> [Niveau; NB_RESERVES] {
    RESERVES.map(|r| {
        let mut n = Niveau { disponible: disponible(r, m, c, maintenant), marge: marge(r, c), ..Niveau::default() };
        for t in e.tickets.iter().filter(|t| t.etat != EtatTicket::Attente) {
            let b = reservation(t, r, m, c, maintenant);
            if b > 0 {
                n.reserve += b;
                n.par.push(t.id);
            }
        }
        n
    })
}

pub fn vue_reserves(e: &Etat, m: &Mesures, c: &Config, maintenant: u64) -> Vec<VueReserve> {
    let niv = niveaux(e, m, c, maintenant);
    RESERVES
        .iter()
        .map(|&r| {
            let n = &niv[r.i()];
            VueReserve {
                cle: r.cle().into(),
                nom: r.nom().into(),
                total: total(r, m, c),
                disponible: n.disponible,
                reserve: n.reserve,
                marge: n.marge,
                libre: n.libre(),
                exclusif: (r == Reserve::Cpu)
                    .then(|| {
                        e.tickets.iter().find(|t| {
                            t.etat != EtatTicket::Attente
                                && t.cote == Cote::Wsl
                                && t.besoin.cpu as i64 > c.cpu_total as i64 - c.marge_cpu as i64
                        })
                    })
                    .flatten()
                    .map(|t| t.id),
            }
        })
        .collect()
}

fn total(r: Reserve, m: &Mesures, c: &Config) -> i64 {
    match r {
        Reserve::RamWindows => m.ram_totale_mo as i64,
        Reserve::RamWsl => c.wsl_ram_max_mo as i64,
        Reserve::Cpu => c.cpu_total as i64,
        Reserve::CpuWindows => coeurs_windows(m, c),
    }
}

/// Refus à la création d'un besoin qu'aucune attente ne pourra satisfaire.
pub fn refus(cote: Cote, b: &Besoin, m: &Mesures, c: &Config) -> Option<Raison> {
    for r in RESERVES {
        let besoin = r.besoin(cote, b);
        let tot = total(r, m, c);
        if besoin == 0 || tot == 0 {
            continue;
        }
        // Au-delà de capacité − marge, une demande CPU reste admise, mais seule (voir `exclusif`).
        let capacite = if r.est_ram() { tot - marge(r, c) } else { tot };
        if besoin > capacite {
            let suggestion = if r.est_ram() {
                format!("Baisse --ram à {} max.", option_ram(capacite))
            } else {
                format!("Baisse --cpu à {} max.", capacite.max(1))
            };
            let mut raison = Raison::simple(
                TypeDecision::Refus,
                if r.est_ram() {
                    format!(
                        "Refusé : {} impossible (besoin {}, capacité maximale {} = {} − marge {}). {suggestion}",
                        r.nom(),
                        r.quantite(besoin),
                        r.quantite(capacite),
                        r.quantite(tot),
                        r.quantite(marge(r, c))
                    )
                } else {
                    format!(
                        "Refusé : CPU impossible (besoin {}, la machine compte {}). {suggestion}",
                        r.quantite(besoin),
                        r.quantite(tot)
                    )
                },
            );
            raison.reserve = Some(r.cle().into());
            raison.besoin = Some(besoin);
            raison.disponible = Some(capacite);
            raison.marge = Some(marge(r, c));
            return Some(raison);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liberation {
    WslArrete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evenement {
    FeuVert(u64),
    Expire(u64),
    Orphelin(u64),
    BailDepasse(u64),
    Bail(u64),
    Rappel(u64),
    Libere(u64, Liberation),
}

#[derive(Clone, Debug)]
struct Gel {
    cause: String,
    levee: String,
    orphelin: Option<u64>,
}

pub struct Plan {
    pub etat: Etat,
    pub evenements: Vec<Evenement>,
    pub persister: bool,
    pub raisons: BTreeMap<u64, Raison>,
    pub gel: Vec<String>,
    pub pression: Vec<Reserve>,
}

enum Blocage {
    Aucun,
    GelManuel,
    Gel(Reserve),
    Exclusif(Option<u64>),
    Manque(Vec<Reserve>),
}

/// Tête de file bloquée : ce qui lui manque, son démarrage estimé T et les tickets qui l'ont doublée.
struct Tete {
    id: u64,
    manque: Vec<Reserve>,
    vers: Option<u64>,
    autorise: bool,
    doublages: u32,
    cree: u64,
    passes: Vec<u64>,
    libres: Vec<u64>,
}

fn avec_article(r: Reserve) -> String {
    format!("{} {}", if r.est_ram() { "la" } else { "le" }, r.nom())
}

/// « la RAM WSL », « la RAM WSL et le CPU WSL ».
fn enumeration(reserves: &[Reserve]) -> String {
    let n: Vec<String> = reserves.iter().map(|&r| avec_article(r)).collect();
    match n.len() {
        0 => String::new(),
        1 => n[0].clone(),
        k => format!("{} et {}", n[..k - 1].join(", "), n[k - 1]),
    }
}

fn noms(reserves: &[Reserve]) -> String {
    reserves.iter().map(|r| r.nom()).collect::<Vec<_>>().join(", ")
}

/// Passe un ticket en actif et débite ses réserves ; renvoie le libre au moment de l'accord.
fn accorder(
    e: &mut Etat,
    niv: &mut [Niveau; NB_RESERVES],
    t: &Ticket,
    besoins: &[i64; NB_RESERVES],
    m: &Mesures,
    maintenant: u64,
) -> String {
    let libres: Vec<String> = RESERVES
        .iter()
        .filter(|r| besoins[r.i()] > 0)
        .map(|&r| format!("{} {}", r.nom(), r.quantite(niv[r.i()].libre())))
        .collect();
    let principale = if t.cote == Cote::Wsl { Reserve::RamWsl } else { Reserve::RamWindows };
    let dispo_principale = niv[principale.i()].disponible.max(0) as u64;
    for r in RESERVES {
        if besoins[r.i()] > 0 {
            niv[r.i()].reserve += besoins[r.i()];
            niv[r.i()].par.push(t.id);
        }
    }
    let tm = e.ticket_mut(t.id).expect("ticket en attente présent");
    tm.etat = EtatTicket::Actif;
    tm.accorde = Some(maintenant);
    tm.presence = maintenant;
    tm.dispo_accord_mo = Some(dispo_principale);
    tm.base_ram_windows_mo = Some(niv[Reserve::RamWindows.i()].disponible);
    tm.base_ram_wsl_mo = Some(niv[Reserve::RamWsl.i()].disponible);
    tm.base_vmmem_mo = m.vmmem.map(|v| v.rss_mo);
    tm.recu = false;
    tm.bail_depasse = false;
    tm.cause_orphelin = None;
    libres.join(", ")
}

fn liste(ids: &[u64]) -> String {
    let noms: Vec<String> = ids.iter().map(|i| format!("#{i}")).collect();
    match noms.len() {
        0 => String::new(),
        1 => noms[0].clone(),
        n => format!("{} et {}", noms[..n - 1].join(", "), noms[n - 1]),
    }
}

fn levee_orphelin(t: &Ticket) -> String {
    let id = t.id;
    match t.cause_orphelin {
        Some(CauseOrphelin::Bail) => format!(
            "« charge fin {id} », « charge prolonge {id} <durée> \"<raison>\" », « charge annule {id} » ou « Libérer » dans la fenêtre"
        ),
        _ => format!("signal du #{id}, « charge fin {id} », « charge annule {id} » ou « Libérer » dans la fenêtre"),
    }
}

fn cause_orphelin(t: &Ticket, maintenant: u64) -> String {
    match t.cause_orphelin {
        Some(CauseOrphelin::Bail) => format!(
            "bail échu depuis {} (durée déclarée de {} dépassée), sans nouvelle de la session depuis {}",
            duree(t.fin_prevue().map_or(0, |f| maintenant.saturating_sub(f))),
            duree(t.besoin.duree_s),
            duree(maintenant.saturating_sub(derniere_nouvelle(t)))
        ),
        _ => format!("sans signal depuis {}", duree(maintenant.saturating_sub(t.presence))),
    }
}

/// Dernier signe d'un ticket manuel au-delà de son bail : l'échéance elle-même, ou un signal plus récent.
fn derniere_nouvelle(t: &Ticket) -> u64 {
    t.fin_prevue().unwrap_or(0).max(t.presence)
}

/// Moment depuis lequel un ticket en attente n'a plus de client : dernière présence ou rappel du feu vert.
fn vu(t: &Ticket) -> u64 {
    t.presence.max(t.rappel.unwrap_or(0))
}

/// Décide des accords, rappels, expirations, orphelins et libérations, et explique la situation de chaque ticket.
pub fn planifier(etat: &Etat, m: &Mesures, c: &Config, maintenant: u64) -> Plan {
    let mut e = etat.clone();
    let mut evenements = Vec::new();

    let pid = m.vmmem.map(|v| v.pid);
    if e.vmmem_pid.is_some() && e.vmmem_pid != pid {
        for t in e.tickets.iter().filter(|t| t.cote == Cote::Wsl) {
            evenements.push(Evenement::Libere(t.id, Liberation::WslArrete));
        }
        e.tickets.retain(|t| t.cote != Cote::Wsl);
    }
    let mut persister = e.vmmem_pid != pid;
    e.vmmem_pid = pid;

    for t in &mut e.tickets {
        let silence = maintenant.saturating_sub(vu(t));
        let rappel_du = !t.recu && t.accorde.is_some_and(|a| maintenant.saturating_sub(a) >= c.delai_rappel_s);
        match t.etat {
            EtatTicket::Attente if t.ecoutes == 0 && silence > c.delai_absence_s => {
                evenements.push(Evenement::Expire(t.id));
            }
            EtatTicket::Actif if rappel_du => {
                t.etat = EtatTicket::Attente;
                t.accorde = None;
                t.dispo_accord_mo = None;
                t.rappel = Some(maintenant);
                evenements.push(Evenement::Rappel(t.id));
            }
            EtatTicket::Actif if t.manuel && t.fin_prevue().is_some_and(|f| maintenant >= f) => {
                if !t.bail_depasse {
                    t.bail_depasse = true;
                    evenements.push(Evenement::BailDepasse(t.id));
                }
                if maintenant.saturating_sub(derniere_nouvelle(t)) >= c.delai_absence_s {
                    t.etat = EtatTicket::Orphelin;
                    t.cause_orphelin = Some(CauseOrphelin::Bail);
                    evenements.push(Evenement::Bail(t.id));
                }
            }
            EtatTicket::Actif if !t.manuel && t.recu && silence > c.delai_actif_s => {
                t.etat = EtatTicket::Orphelin;
                t.cause_orphelin = Some(CauseOrphelin::Silence);
                evenements.push(Evenement::Orphelin(t.id));
            }
            _ => {}
        }
    }
    e.tickets.retain(|t| !evenements.contains(&Evenement::Expire(t.id)));

    mettre_a_jour_pression_swap(&mut e, m, c, maintenant);
    let mut niv = niveaux(&e, m, c, maintenant);
    let mut gels: [Option<Gel>; NB_RESERVES] = [None, None, None, None];
    let mut pression = Vec::new();
    for r in [Reserve::RamWindows, Reserve::RamWsl] {
        // Le repli 12 Go − vmmemWSL compte le cache de pages : il ne suffit pas à déclarer une pression.
        if r == Reserve::RamWsl && wsl_frais(m, maintenant).is_none() {
            continue;
        }
        let n = &niv[r.i()];
        if let Some(w) = wsl_frais(m, maintenant).filter(|_| r == Reserve::RamWsl && e.pression_swap) {
            pression.push(r);
            gels[r.i()] = Some(Gel {
                cause: format!(
                    "pression : swap WSL plein à {} % et actif (sortie {:.0} pages/s, MemAvailable {})",
                    w.swap_pct(),
                    m.swap_sortie_ps,
                    go(w.mem_dispo_mo as i64)
                ),
                levee: format!("swap WSL sous {} % ou calme pendant 1 min", c.swap_seuil_levee_pct),
                orphelin: None,
            });
        } else if n.disponible < n.marge {
            pression.push(r);
            gels[r.i()] = Some(Gel {
                cause: format!("pression : {} disponible {} sous la marge de {}", r.nom(), go(n.disponible), go(n.marge)),
                levee: format!("{} de nouveau au-dessus de {}", r.nom(), go(n.marge)),
                orphelin: None,
            });
        }
    }
    for t in e.tickets.iter().filter(|t| t.etat == EtatTicket::Orphelin) {
        for r in RESERVES {
            if r.besoin(t.cote, &t.besoin) > 0 && gels[r.i()].is_none() {
                gels[r.i()] = Some(Gel {
                    cause: format!("ticket #{} orphelin ({})", t.id, cause_orphelin(t, maintenant)),
                    levee: levee_orphelin(t),
                    orphelin: Some(t.id),
                });
            }
        }
    }
    let gel_manuel = e.gel_manuel.map(|depuis| Gel {
        cause: format!("gel manuel depuis {}", hhmm(depuis)),
        levee: "« Dégeler la file » dans la fenêtre ou le menu de l'icône".into(),
        orphelin: None,
    });

    let mut raisons = BTreeMap::new();
    let en_attente: Vec<u64> = e.tickets.iter().filter(|t| t.etat == EtatTicket::Attente).map(|t| t.id).collect();
    let mut tete: Option<Tete> = None;
    let mut position = 0;
    for id in en_attente {
        let t = e.ticket(id).expect("ticket en attente présent").clone();
        let silence = maintenant.saturating_sub(vu(&t));
        if t.rappel.is_some() || (t.ecoutes == 0 && silence > c.delai_attente_s) {
            position += 1;
            let pourquoi = match t.rappel {
                Some(r) => format!("feu vert rappelé à {} faute d'avoir été utilisé", hhmm(r)),
                None => format!("aucun client ne l'attend depuis {}", duree(silence)),
            };
            let mut r = Raison::simple(
                TypeDecision::Attente,
                format!(
                    "#{id} en attente, position {position} : {pourquoi} ; place gardée jusqu'à {}, les suivants passent. Relance « charge attend {id} ».",
                    hhmm(vu(&t) + c.delai_absence_s)
                ),
            );
            r.position = Some(position);
            r.reserve = Some("absent".into());
            raisons.insert(id, r);
            continue;
        }
        let besoins = RESERVES.map(|r| debit(r, t.cote, &t.besoin, m, c));
        let exclusif = besoins[Reserve::Cpu.i()] > c.cpu_total as i64 - c.marge_cpu as i64;
        let autres_wsl: Vec<&Ticket> =
            e.tickets.iter().filter(|a| a.etat != EtatTicket::Attente && a.cote == Cote::Wsl).collect();
        let blocage = if gel_manuel.is_some() {
            Blocage::GelManuel
        } else if let Some(r) = RESERVES.into_iter().find(|r| besoins[r.i()] > 0 && gels[r.i()].is_some()) {
            Blocage::Gel(r)
        } else if exclusif && !autres_wsl.is_empty() {
            let fins: Option<Vec<u64>> = autres_wsl
                .iter()
                .map(|a| if a.etat == EtatTicket::Orphelin { None } else { a.fin_prevue() })
                .collect();
            Blocage::Exclusif(fins.and_then(|f| f.into_iter().max()))
        } else {
            let manquantes: Vec<Reserve> = RESERVES
                .into_iter()
                .filter(|&r| !(exclusif && r == Reserve::Cpu))
                .filter(|r| besoins[r.i()] > 0 && besoins[r.i()] > niv[r.i()].libre())
                .collect();
            if manquantes.is_empty() { Blocage::Aucun } else { Blocage::Manque(manquantes) }
        };

        if let Some(h) = tete.as_mut() {
            let fin = maintenant + t.besoin.duree_s;
            let utilise: Vec<Reserve> = h.manque.iter().copied().filter(|r| besoins[r.i()] > 0).collect();
            let avant_t = h.vers.is_some_and(|v| fin <= v);
            let anti_famine = h.doublages >= c.max_doublages || maintenant.saturating_sub(h.cree) >= c.attente_max_tete_s;
            // Un ticket qui ne touche à rien de ce qui manque à la tête ne peut pas la retarder : l'anti-famine ne le
            // limite pas et ne le compte pas ; seuls les passages « fin avant T » partagent une ressource avec elle.
            let sans_conflit = utilise.is_empty();
            if matches!(blocage, Blocage::Aucun) && h.autorise && (sans_conflit || (avant_t && !anti_famine)) {
                let pourquoi = if utilise.is_empty() {
                    match h.manque.as_slice() {
                        [r] => format!("n'utilise pas {} qui lui manque", avec_article(*r)),
                        tous => format!(
                            "n'utilise ni {}, qui lui manquent",
                            tous.iter().map(|&r| avec_article(r)).collect::<Vec<_>>().join(" ni ")
                        ),
                    }
                } else {
                    format!("finit avant son démarrage estimé (vers {})", hhmm(h.vers.unwrap_or(maintenant)))
                };
                let libres = accorder(&mut e, &mut niv, &t, &besoins, m, maintenant);
                evenements.push(Evenement::FeuVert(id));
                raisons.insert(
                    id,
                    Raison::simple(
                        TypeDecision::FeuVert,
                        format!("#{id} : feu vert, passé devant #{} : {pourquoi}. Libre à l'accord : {libres}.", h.id),
                    ),
                );
                if sans_conflit {
                    h.libres.push(id);
                } else {
                    h.doublages += 1;
                    h.passes.push(id);
                    if let Some(ht) = e.ticket_mut(h.id) {
                        ht.doublages = h.doublages;
                    }
                }
                continue;
            }
            position += 1;
            let detail = if let Blocage::Manque(propres) = &blocage {
                format!("il manque lui-même de place : {}", noms(propres))
            } else if !h.autorise || !matches!(blocage, Blocage::Aucun) {
                "file stricte, ordre d'arrivée".to_owned()
            } else if anti_famine && avant_t {
                let pourquoi = if h.doublages >= c.max_doublages {
                    format!("#{} a déjà été doublé {} fois", h.id, h.doublages)
                } else {
                    format!("#{} attend depuis plus de {}", h.id, duree(c.attente_max_tete_s))
                };
                format!("anti-famine : il utilise aussi {} et {pourquoi}", enumeration(&utilise))
            } else {
                let quand = h.vers.map_or("une heure inconnue".to_owned(), |v| format!("vers {}", hhmm(v)));
                format!(
                    "il utilise aussi {} qui {} au #{} et finirait après son démarrage estimé ({quand})",
                    enumeration(&utilise),
                    if utilise.len() > 1 { "manquent" } else { "manque" },
                    h.id
                )
            };
            let mut r = Raison::simple(
                TypeDecision::Attente,
                format!("#{id} en attente, position {position} : derrière #{} ({detail}).", h.id),
            );
            r.position = Some(position);
            r.bloquants = vec![h.id];
            raisons.insert(id, r);
            continue;
        }

        let nouvelle_tete = |manque: Vec<Reserve>, vers: Option<u64>, autorise: bool| Tete {
            id,
            manque,
            vers,
            autorise,
            doublages: t.doublages,
            cree: t.cree,
            passes: Vec::new(),
            libres: Vec::new(),
        };
        match blocage {
            Blocage::Aucun => {
                let libres = accorder(&mut e, &mut niv, &t, &besoins, m, maintenant);
                evenements.push(Evenement::FeuVert(id));
                raisons.insert(
                    id,
                    Raison::simple(TypeDecision::FeuVert, format!("#{id} : feu vert. Libre à l'accord : {libres}.")),
                );
            }
            Blocage::GelManuel => {
                let g = gel_manuel.as_ref().expect("gel manuel présent");
                position += 1;
                let mut raison = Raison::simple(
                    TypeDecision::Gel,
                    format!("#{id} en attente, position {position} : admissions gelées, {}. Levée : {}.", g.cause, g.levee),
                );
                raison.position = Some(position);
                raison.reserve = Some("manuel".into());
                raisons.insert(id, raison);
                tete = Some(nouvelle_tete(Vec::new(), None, false));
            }
            Blocage::Gel(r) => {
                let g = gels[r.i()].as_ref().expect("gel présent");
                position += 1;
                let mut raison = Raison::simple(
                    TypeDecision::Gel,
                    format!(
                        "#{id} en attente, position {position} : admissions gelées sur {}, {}. Levée : {}.",
                        r.nom(),
                        g.cause,
                        g.levee
                    ),
                );
                raison.position = Some(position);
                raison.reserve = Some(r.cle().into());
                raison.bloquants = g.orphelin.into_iter().collect();
                raisons.insert(id, raison);
                tete = Some(nouvelle_tete(vec![r], None, true));
            }
            Blocage::Exclusif(vers) => {
                position += 1;
                raisons.insert(id, raison_exclusive(&t, position, &autres_wsl, c, maintenant));
                tete = Some(nouvelle_tete(vec![Reserve::Cpu], vers, true));
            }
            Blocage::Manque(manquantes) => {
                position += 1;
                let vers = estimation(&t, &niv, &e, m, c).0;
                raisons.insert(id, raison_attente(&t, position, &manquantes, &niv, &e, m, c, maintenant));
                tete = Some(nouvelle_tete(manquantes, vers, true));
            }
        }
    }
    if let Some(h) = &tete
        && let Some(r) = raisons.get_mut(&h.id)
    {
        if !h.passes.is_empty() {
            r.phrase += &format!(" Doublé par {} ({}/{}).", liste(&h.passes), h.doublages, c.max_doublages);
        }
        if !h.libres.is_empty() {
            r.phrase += &format!(" Passés sur une ressource dont il n'a pas besoin : {}.", liste(&h.libres));
        }
        if h.autorise
            && (h.doublages >= c.max_doublages || maintenant.saturating_sub(h.cree) >= c.attente_max_tete_s)
        {
            r.phrase += " Anti-famine : plus aucun ticket ne passe devant en prenant ce qui lui manque.";
        }
    }

    for t in &e.tickets {
        match t.etat {
            EtatTicket::Actif if !raisons.contains_key(&t.id) => {
                let accorde = t.accorde.unwrap_or(maintenant);
                let fin = t.fin_prevue().unwrap_or(maintenant);
                let suite = if !t.recu {
                    format!(
                        " Feu vert pas encore accusé par le client : repris à {} sans accusé.",
                        hhmm(accorde + c.delai_rappel_s)
                    )
                } else if t.manuel && maintenant >= fin {
                    format!(
                        " Bail dépassé depuis {} (durée déclarée de {}) : réservation maintenue, la file continue sur les \
                         ressources restantes. Travail en cours : « charge prolonge {id} <durée> \"<raison>\" » ; terminé : \
                         « charge fin {id} ». Sans nouvelle, orphelin à {} et admissions gelées.",
                        duree(maintenant - fin),
                        duree(t.besoin.duree_s),
                        hhmm(derniere_nouvelle(t) + c.delai_absence_s),
                        id = t.id
                    )
                } else if t.manuel {
                    format!(
                        " Bail jusqu'à {} : au-delà, « charge prolonge {id} » ou « charge fin {id} », sinon orphelin à {}.",
                        hhmm(fin),
                        hhmm(derniere_nouvelle(t) + c.delai_absence_s),
                        id = t.id
                    )
                } else {
                    String::new()
                };
                let montee = if maintenant.saturating_sub(accorde) < c.delai_montee_s {
                    format!(" Montée en charge jusqu'à {} : réservation entière.", hhmm(accorde + c.delai_montee_s))
                } else {
                    let restes: Vec<String> = [Reserve::RamWindows, Reserve::RamWsl]
                        .into_iter()
                        .filter(|&r| debit(r, t.cote, &t.besoin, m, c) > 0)
                        .map(|r| format!("{} {}", r.nom(), r.quantite(reservation(t, r, m, c, maintenant))))
                        .collect();
                    format!(" Montée en charge terminée, réservé encore : {}.", restes.join(", "))
                };
                let force = if t.force { " (forcé par Adel)" } else { "" };
                raisons.insert(
                    t.id,
                    Raison::simple(
                        TypeDecision::Actif,
                        format!("#{} actif{force} depuis {}, fin prévue vers {}.{montee}{suite}", t.id, hhmm(accorde), hhmm(fin)),
                    ),
                );
            }
            EtatTicket::Orphelin => {
                let gelees: Vec<&str> = RESERVES
                    .iter()
                    .filter(|r| r.besoin(t.cote, &t.besoin) > 0)
                    .map(|r| r.nom())
                    .collect();
                let mut raison = Raison::simple(
                    TypeDecision::Orphelin,
                    format!(
                        "#{} orphelin : {}. Sa réservation reste comptée et les admissions sont gelées sur {}. Levée : {}.",
                        t.id,
                        cause_orphelin(t, maintenant),
                        gelees.join(", "),
                        levee_orphelin(t)
                    ),
                );
                raison.bloquants = vec![t.id];
                raisons.insert(t.id, raison);
            }
            _ => {}
        }
    }

    let mut gel: Vec<String> = Vec::new();
    for g in gel_manuel.iter().chain(gels.iter().flatten()) {
        let texte = format!("Admissions gelées : {}. Levée : {}.", g.cause, g.levee);
        if !gel.contains(&texte) {
            gel.push(texte);
        }
    }

    persister |= !evenements.is_empty();
    Plan { etat: e, evenements, persister, raisons, gel, pression }
}

fn resume_besoin(t: &Ticket) -> String {
    format!("{}, {}, {}", t.cote.nom(), go(t.besoin.ram_mo as i64), coeurs(t.besoin.cpu as i64))
}

/// Texte de confirmation d'un forçage : ce qui resterait libre sur chaque réserve que le ticket débite.
pub fn apercu_forcage(e: &Etat, id: u64, m: &Mesures, c: &Config, maintenant: u64) -> Option<String> {
    let t = e.ticket(id).filter(|t| t.etat == EtatTicket::Attente)?;
    let niv = niveaux(e, m, c, maintenant);
    let apres: Vec<String> = RESERVES
        .iter()
        .filter_map(|&r| {
            let b = debit(r, t.cote, &t.besoin, m, c);
            (b > 0).then(|| {
                format!(
                    "{} libre après : {} (marge {})",
                    r.nom(),
                    r.quantite(niv[r.i()].libre() - b),
                    r.quantite(niv[r.i()].marge)
                )
            })
        })
        .collect();
    Some(format!("Forcer #{id} ({}) ? {}.", resume_besoin(t), apres.join(", ")))
}

/// Feu vert immédiat donné par Adel, hors de l'ordre de la file, des réserves et des gels.
/// Les autres gardent leur place ; l'anti-famine ne compte pas un forçage comme un dépassement.
pub fn forcer(e: &Etat, id: u64, m: &Mesures, c: &Config, maintenant: u64) -> Result<(Etat, String), String> {
    let t = e
        .ticket(id)
        .filter(|t| t.etat == EtatTicket::Attente)
        .cloned()
        .ok_or_else(|| format!("Ticket #{id} absent ou pas en attente : rien à forcer."))?;
    let mut niv = niveaux(e, m, c, maintenant);
    let besoins = RESERVES.map(|r| debit(r, t.cote, &t.besoin, m, c));
    let mut nouvel = e.clone();
    let libres = accorder(&mut nouvel, &mut niv, &t, &besoins, m, maintenant);
    if let Some(f) = nouvel.ticket_mut(id) {
        f.force = true;
        f.rappel = None;
    }
    Ok((nouvel, format!("#{id} ({}) · réserves au moment du forçage : {libres}", resume_besoin(&t))))
}

fn raison_exclusive(t: &Ticket, position: usize, autres: &[&Ticket], c: &Config, maintenant: u64) -> Raison {
    let ids: Vec<u64> = autres.iter().map(|a| a.id).collect();
    let fin = autres.iter().map(|a| a.fin_prevue()).collect::<Option<Vec<u64>>>().and_then(|f| f.into_iter().max());
    let quand = match (fin, autres.iter().any(|a| a.etat == EtatTicket::Orphelin)) {
        (_, true) => " Un orphelin en fait partie : il faut sa fin, son annulation ou « Libérer ».".to_owned(),
        (Some(f), false) if f < maintenant => format!(" Débloqué à la fin de {}, la dernière prévue vers {} (dépassée).", liste(&ids), hhmm(f)),
        (Some(f), false) => format!(" Débloqué à la fin de {}, la dernière prévue vers {}.", liste(&ids), hhmm(f)),
        (None, false) => String::new(),
    };
    let mut r = Raison::simple(
        TypeDecision::Attente,
        format!(
            "#{} en attente, position {position} : demande exclusive ({} sur {}, au-delà de {} − marge {}), elle passe quand aucun autre ticket WSL n'est actif ; actifs : {}.{quand}",
            t.id,
            coeurs(t.besoin.cpu as i64),
            c.cpu_total,
            c.cpu_total,
            c.marge_cpu,
            liste(&ids)
        ),
    );
    r.position = Some(position);
    r.reserve = Some("exclusif".into());
    r.bloquants = ids;
    r.debloque_vers = fin;
    r
}

#[allow(clippy::too_many_arguments)]
fn raison_attente(
    t: &Ticket,
    position: usize,
    manquantes: &[Reserve],
    niv: &[Niveau; NB_RESERVES],
    e: &Etat,
    m: &Mesures,
    c: &Config,
    maintenant: u64,
) -> Raison {
    let r = manquantes[0];
    let n = &niv[r.i()];
    let besoin = debit(r, t.cote, &t.besoin, m, c);
    let dont = if n.reserve > 0 {
        format!(" dont {} réservés pour {}", r.quantite(n.reserve), liste(&n.par))
    } else {
        String::new()
    };
    let mut phrase = format!(
        "#{} en attente, position {position} : {} {} (besoin {}, disponible {}{dont}, marge {}).",
        t.id,
        r.nom(),
        r.insuffisante(),
        r.quantite(besoin),
        r.quantite(n.disponible),
        r.quantite(n.marge)
    );
    for autre in &manquantes[1..] {
        phrase += &format!(" {} {} aussi.", autre.nom(), autre.insuffisante());
    }

    let (vers, attendus) = estimation(t, niv, e, m, c);
    match vers {
        Some(f) => {
            let quand = if f < maintenant {
                format!("prévue vers {} (dépassée)", hhmm(f))
            } else {
                format!("prévue vers {}", hhmm(f))
            };
            if attendus.len() == 1 {
                phrase += &format!(" Débloqué à la fin du #{}, {quand}.", attendus[0]);
            } else {
                phrase += &format!(" Débloqué à la fin des {}, la dernière {quand}.", liste(&attendus));
            }
        }
        None => {
            phrase += " Les fins prévues des actifs ne libèrent pas assez : la place est prise hors file (autres programmes, Docker, cache).";
        }
    }

    if manquantes.iter().all(|r| r.est_ram()) {
        let max = RESERVES
            .iter()
            .filter(|r| r.est_ram() && r.besoin(t.cote, &t.besoin) > 0)
            .map(|r| niv[r.i()].libre())
            .min()
            .unwrap_or(0);
        if max >= 256 {
            phrase += &format!(" Une nouvelle demande avec --ram {} ou moins passerait maintenant.", option_ram(max));
        }
    } else if manquantes.iter().all(|r| !r.est_ram()) {
        let max = manquantes.iter().map(|r| niv[r.i()].libre()).min().unwrap_or(0);
        if max >= 1 {
            phrase += &format!(" Une nouvelle demande avec --cpu {max} passerait maintenant.");
        }
    }

    let mut raison = Raison::simple(TypeDecision::Attente, phrase);
    raison.position = Some(position);
    raison.reserve = Some(r.cle().into());
    raison.besoin = Some(besoin);
    raison.disponible = Some(n.disponible);
    raison.reserve_actifs = Some(n.reserve);
    raison.marge = Some(n.marge);
    raison.bloquants = n.par.clone();
    raison.debloque_vers = vers;
    raison
}

/// Heure T à laquelle la tête tiendrait si les actifs finissaient comme prévu ; les orphelins ne comptent pas.
fn estimation(t: &Ticket, niv: &[Niveau; NB_RESERVES], e: &Etat, m: &Mesures, c: &Config) -> (Option<u64>, Vec<u64>) {
    let mut libres = niv.each_ref().map(Niveau::libre);
    let mut actifs: Vec<&Ticket> = e.tickets.iter().filter(|a| a.etat == EtatTicket::Actif).collect();
    actifs.sort_by_key(|a| (a.fin_prevue(), a.id));
    let mut attendus = Vec::new();
    for a in actifs {
        let mut utile = false;
        for r in RESERVES {
            let b = debit(r, a.cote, &a.besoin, m, c);
            libres[r.i()] += b;
            utile |= b > 0 && debit(r, t.cote, &t.besoin, m, c) > 0;
        }
        if utile {
            attendus.push(a.id);
        }
        if RESERVES.iter().all(|&r| {
            let b = debit(r, t.cote, &t.besoin, m, c);
            b == 0 || b <= libres[r.i()]
        }) {
            return (a.fin_prevue(), attendus);
        }
    }
    (None, Vec::new())
}

#[cfg(test)]
mod tests;
