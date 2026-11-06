use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use commun::config::Config;
use commun::format::{duree, go};
use commun::modele::{
    Besoin, CauseOrphelin, Demande, Etat, EtatTicket, Fin, Mesures, MesuresWsl, Presence, Prolonge, Raison, Statut,
    Ticket, TypeDecision, Vue, VueTicket,
};
use commun::planif::{self, Evenement, Liberation, Reserve};
use commun::protocole::{Reponse, Requete, lire_ligne};
use commun::systeme::{hhmm, horodatage, maintenant};
use serde_json::{Value, json};

use crate::ui;

const MEMOIRE_TERMINES: usize = 200;
const PRESSION_AVANT_BULLE_S: u64 = 120;
const INTERVALLE_BULLES_S: u64 = 30 * 60;
const RAM_CRITIQUE_MO: u64 = 1024;

/// Règle des bulles : au plus une par genre toutes les 30 min ; une pression n'en donne qu'après 2 min
/// continues, et seulement si un ticket attend. Le reste va au journal et dans l'infobulle.
#[derive(Default)]
pub struct Bulles {
    derniere: BTreeMap<&'static str, u64>,
    pression_depuis: BTreeMap<&'static str, u64>,
}

impl Bulles {
    pub fn autoriser(&mut self, genre: &'static str, maintenant: u64) -> bool {
        if self.derniere.get(genre).is_some_and(|&d| maintenant.saturating_sub(d) < INTERVALLE_BULLES_S) {
            return false;
        }
        self.derniere.insert(genre, maintenant);
        true
    }

    /// Suit une pression ; vrai quand elle mérite une bulle maintenant.
    pub fn pression(&mut self, genre: &'static str, active: bool, file_non_vide: bool, maintenant: u64) -> bool {
        if !active {
            self.pression_depuis.remove(genre);
            return false;
        }
        let depuis = *self.pression_depuis.entry(genre).or_insert(maintenant);
        maintenant.saturating_sub(depuis) >= PRESSION_AVANT_BULLE_S && file_non_vide && self.autoriser(genre, maintenant)
    }
}

pub struct Annonce {
    pub titre: String,
    pub texte: String,
    pub alerte: bool,
}

pub struct Serveur {
    pub etat: Etat,
    pub config: Config,
    dossier: PathBuf,
    mesures: Mesures,
    raisons: BTreeMap<u64, Raison>,
    gel: Vec<String>,
    pression: Vec<Reserve>,
    termines: VecDeque<(u64, &'static str, String)>,
    cle_tete: String,
    annonces: Vec<Annonce>,
    bulles: Bulles,
    historique_swap: VecDeque<(u64, u64)>,
    erreur_persistance: bool,
}

pub struct Partage {
    serveur: Mutex<Serveur>,
    signal: Condvar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Cloture {
    Fin,
    Annule,
    Fenetre,
}

const FENETRE_SWAP_S: u64 = 30;

/// Débit de sortie du swap en pages/s, sur ~30 s, à partir des relevés cumulés de `pswpout`.
pub fn debit_sortie_swap(historique: &mut VecDeque<(u64, u64)>, maintenant: u64, pswpout: u64) -> f32 {
    // 0 = client trop ancien pour lire /proc/vmstat : relevé inconnu, il ne doit pas créer un faux pic.
    if pswpout == 0 {
        return match (historique.front(), historique.back()) {
            (Some(&(t0, p0)), Some(&(t1, p1))) if t1 > t0 => p1.saturating_sub(p0) as f32 / (t1 - t0) as f32,
            _ => 0.0,
        };
    }
    if historique.back().is_some_and(|&(_, p)| p > pswpout) {
        historique.clear();
    }
    historique.push_back((maintenant, pswpout));
    while historique.len() > 2 && historique[1].0 + FENETRE_SWAP_S <= maintenant {
        historique.pop_front();
    }
    let (t0, p0) = historique[0];
    let duree = maintenant.saturating_sub(t0);
    if duree == 0 { 0.0 } else { pswpout.saturating_sub(p0) as f32 / duree as f32 }
}

pub fn journaliser(dossier: &Path, texte: &str) {
    let ligne = format!("{} {}\n", horodatage(maintenant()), texte);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dossier.join("journal.log")) {
        let _ = f.write_all(ligne.as_bytes());
    }
}

/// Dernières lignes du journal, lues depuis la fin du fichier.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn fin_du_journal(dossier: &Path, n: usize) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(dossier.join("journal.log")) else {
        return Vec::new();
    };
    let taille = f.metadata().map(|m| m.len()).unwrap_or(0);
    let debut = taille.saturating_sub(16 * 1024);
    let mut octets = Vec::new();
    if f.seek(SeekFrom::Start(debut)).is_err() || f.read_to_end(&mut octets).is_err() {
        return Vec::new();
    }
    let texte = String::from_utf8_lossy(&octets);
    let mut lignes: Vec<String> = texte.lines().map(str::to_owned).collect();
    if debut > 0 && !lignes.is_empty() {
        lignes.remove(0);
    }
    let depart = lignes.len().saturating_sub(n);
    lignes.split_off(depart)
}

/// Fichier temporaire, flush disque, puis remplacement : un crash laisse l'ancienne ou la nouvelle version, jamais un mélange.
pub fn ecrire_atomique(chemin: &Path, contenu: &[u8]) -> io::Result<()> {
    let tmp = chemin.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contenu)?;
        f.sync_all()?;
    }
    let mut essais = 0;
    loop {
        match std::fs::rename(&tmp, chemin) {
            Ok(()) => return Ok(()),
            // Un antivirus ou un indexeur peut tenir le fichier quelques millisecondes.
            Err(_) if essais < 5 => {
                essais += 1;
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn nom_etat(e: EtatTicket) -> &'static str {
    match e {
        EtatTicket::Attente => "attente",
        EtatTicket::Actif => "actif",
        EtatTicket::Orphelin => "orphelin",
    }
}

fn resume(t: &Ticket) -> String {
    let lieu = match (&t.projet, &t.branche) {
        (Some(p), Some(b)) => format!(" {p} · {b}"),
        (Some(p), None) => format!(" {p}"),
        _ => String::new(),
    };
    format!(
        "[{}]{lieu} {} ({}, {}, {} cœur(s), {}{})",
        t.session,
        t.description,
        t.cote.nom(),
        go(t.besoin.ram_mo as i64),
        t.besoin.cpu,
        duree(t.besoin.duree_s),
        if t.manuel { ", manuel" } else { "" }
    )
}

fn tronquer(texte: &str, max: usize) -> String {
    if texte.chars().count() <= max {
        return texte.to_owned();
    }
    let mut s: String = texte.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

impl Serveur {
    pub fn demarrer(dossier: PathBuf, config: Config, mesures: Mesures) -> Result<Serveur, String> {
        let chemin = dossier.join("etat.json");
        let mut annonces = Vec::new();
        let mut etat = match std::fs::read(&chemin) {
            Ok(octets) => match serde_json::from_slice::<Etat>(&octets) {
                Ok(e) => e,
                Err(e) => {
                    let copie = dossier.join(format!("etat.json.illisible-{}", maintenant()));
                    let _ = std::fs::rename(&chemin, &copie);
                    let texte = format!("etat.json illisible ({e}), mis de côté dans {} ; départ à vide.", copie.display());
                    journaliser(&dossier, &format!("ERREUR {texte}"));
                    annonces.push(Annonce { titre: "État illisible".into(), texte, alerte: true });
                    Etat::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Etat::default(),
            Err(e) => return Err(format!("lecture de {} : {e}", chemin.display())),
        };
        etat.epoque += 1;
        let now = maintenant();
        for t in &mut etat.tickets {
            t.presence = now;
        }
        let mut s = Serveur {
            etat,
            config,
            dossier,
            mesures,
            raisons: BTreeMap::new(),
            gel: Vec::new(),
            pression: Vec::new(),
            termines: VecDeque::new(),
            cle_tete: String::new(),
            annonces,
            bulles: Bulles::default(),
            historique_swap: VecDeque::new(),
            erreur_persistance: false,
        };
        s.persister(&s.etat).map_err(|e| format!("écriture de etat.json : {e}"))?;
        let repris = s.etat.tickets.len();
        s.journal(&format!(
            "DÉMARRAGE époque {} sur le tube {} · {repris} ticket(s) repris",
            s.etat.epoque, s.config.tube
        ));
        let candidat = s.etat.clone();
        s.valider(candidat, false, None, None)?;
        Ok(s)
    }

    fn journal(&self, texte: &str) {
        journaliser(&self.dossier, texte);
    }

    fn persister(&self, e: &Etat) -> io::Result<()> {
        let octets = serde_json::to_vec_pretty(e).map_err(io::Error::other)?;
        ecrire_atomique(&self.dossier.join("etat.json"), &octets)
    }

    fn annoncer(&mut self, genre: &'static str, titre: String, texte: String) {
        if self.bulles.autoriser(genre, maintenant()) {
            self.annonces.push(Annonce { titre, texte, alerte: true });
        }
    }

    fn terminer(&mut self, id: u64, motif: &'static str, phrase: String) {
        self.termines.push_back((id, motif, phrase));
        while self.termines.len() > MEMOIRE_TERMINES {
            self.termines.pop_front();
        }
    }

    fn maj_wsl(&mut self, w: Option<MesuresWsl>, now: u64) {
        if let Some(mut w) = w {
            w.quand = now;
            self.mesures.wsl = Some(w);
            self.mesures.swap_sortie_ps = debit_sortie_swap(&mut self.historique_swap, now, w.pswpout);
        }
    }

    /// Planifie sur `candidat`, marque comme remis le feu vert de `remis` s'il en reçoit un, écrit sur disque si
    /// une décision change, et seulement ensuite rend le tout visible.
    fn valider(&mut self, candidat: Etat, forcer: bool, remis: Option<u64>, prelude: Option<String>) -> Result<(), String> {
        let now = maintenant();
        let mut plan = planif::planifier(&candidat, &self.mesures, &self.config, now);
        let mut forcer = forcer;
        if let Some(t) = remis.and_then(|id| plan.etat.ticket_mut(id))
            && t.etat == EtatTicket::Actif
            && !t.recu
        {
            t.recu = true;
            forcer = true;
        }
        if plan.persister || forcer {
            if let Err(e) = self.persister(&plan.etat) {
                let texte = format!("écriture de etat.json impossible : {e} ; décision annulée.");
                if !self.erreur_persistance {
                    self.journal(&format!("ERREUR {texte}"));
                    self.annoncer("erreur", "Persistance en échec".into(), texte.clone());
                }
                self.erreur_persistance = true;
                return Err(texte);
            }
            self.erreur_persistance = false;
        }
        let ancien = std::mem::replace(&mut self.etat, plan.etat);
        self.raisons = plan.raisons;
        if let Some(p) = prelude {
            self.journal(&p);
        }

        for ev in &plan.evenements {
            match *ev {
                Evenement::FeuVert(id) => {
                    let t = self.etat.ticket(id).expect("ticket accordé présent").clone();
                    let phrase = self.raisons.get(&id).map(|r| r.phrase.clone()).unwrap_or_default();
                    self.journal(&format!("FEU VERT #{id} {} · {phrase}", resume(&t)));
                }
                Evenement::Rappel(id) => {
                    let quoi = self.etat.ticket(id).map(resume).unwrap_or_default();
                    let phrase = format!(
                        "Ticket #{id} : feu vert repris, personne ne l'a pris en {} ; place gardée, « charge attend {id} » pour le récupérer.",
                        duree(self.config.delai_rappel_s)
                    );
                    self.journal(&format!("RAPPEL #{id} {quoi} · {phrase}"));
                }
                Evenement::Expire(id) => {
                    let quoi = ancien.ticket(id).map(resume).unwrap_or_default();
                    let phrase = format!(
                        "Ticket #{id} retiré : aucun client ne l'attendait depuis plus de {} (rien ne tournait encore).",
                        duree(self.config.delai_absence_s)
                    );
                    self.journal(&format!("EXPIRÉ #{id} {quoi} · {phrase}"));
                    self.terminer(id, "expire", phrase);
                }
                Evenement::Orphelin(id) | Evenement::Bail(id) => {
                    let quoi = self.etat.ticket(id).map(resume).unwrap_or_default();
                    let phrase = self.raisons.get(&id).map(|r| r.phrase.clone()).unwrap_or_default();
                    let titre = if matches!(ev, Evenement::Bail(_)) { "BAIL ÉCHU" } else { "ORPHELIN" };
                    self.journal(&format!("{titre} #{id} {quoi} · {phrase}"));
                    self.annoncer("orphelin", format!("Ticket #{id} orphelin"), tronquer(&phrase, 250));
                }
                Evenement::Libere(id, motif) => {
                    let quoi = ancien.ticket(id).map(resume).unwrap_or_default();
                    let phrase = match motif {
                        Liberation::WslArrete => format!("Ticket #{id} libéré : WSL s'est arrêté ou a redémarré."),
                    };
                    self.journal(&format!("LIBÉRÉ #{id} {quoi} · {phrase}"));
                    self.terminer(id, "libere", phrase);
                }
            }
        }

        let nouvelles: Vec<Reserve> = plan.pression.iter().copied().filter(|r| !self.pression.contains(r)).collect();
        for r in &nouvelles {
            let texte = plan
                .gel
                .iter()
                .find(|g| g.contains(r.nom()) || (*r == Reserve::RamWsl && g.contains("swap WSL")))
                .cloned()
                .unwrap_or_default();
            self.journal(&format!("PRESSION {} · {texte}", r.nom()));
        }
        let file_non_vide = self.etat.tickets.iter().any(|t| t.etat == EtatTicket::Attente);
        for r in [Reserve::RamWindows, Reserve::RamWsl] {
            let genre = if r == Reserve::RamWindows { "pression ram_windows" } else { "pression ram_wsl" };
            if self.bulles.pression(genre, plan.pression.contains(&r), file_non_vide, now) {
                let texte = plan.gel.iter().find(|g| g.contains(r.nom())).cloned().unwrap_or_default();
                self.annonces.push(Annonce {
                    titre: format!("Pression sur la {} depuis 2 min", r.nom()),
                    texte: tronquer(&texte, 250),
                    alerte: true,
                });
            }
        }
        let dispo = self.mesures.ram_dispo_mo;
        if dispo > 0 && dispo < RAM_CRITIQUE_MO {
            self.annoncer(
                "ram_critique",
                "RAM Windows critique".into(),
                format!("Plus que {} disponible sous Windows : ferme un programme lourd.", go(dispo as i64)),
            );
        }
        for r in self.pression.iter().filter(|r| !plan.pression.contains(r)) {
            self.journal(&format!("RETOUR NORMAL {} · admissions de nouveau possibles.", r.nom()));
        }
        self.pression = plan.pression;
        self.gel = plan.gel;

        let tete = self
            .etat
            .tickets
            .iter()
            .find(|t| t.etat == EtatTicket::Attente)
            .and_then(|t| self.raisons.get(&t.id));
        let cle = tete.map(|r| r.cle()).unwrap_or_default();
        if cle != self.cle_tete {
            if let Some(r) = tete {
                self.journal(&format!("ATTENTE {}", r.phrase));
            }
            self.cle_tete = cle;
        }
        Ok(())
    }

    fn vue(&self, id: u64) -> Option<Vue> {
        if let Some(t) = self.etat.ticket(id) {
            let raison = self.raisons.get(&id).cloned().unwrap_or_else(|| {
                Raison::simple(TypeDecision::Attente, format!("#{id} en attente de la prochaine décision."))
            });
            return Some(Vue { id, etat: nom_etat(t.etat).into(), epoque: self.etat.epoque, raison });
        }
        self.termines.iter().rev().find(|(i, _, _)| *i == id).map(|(_, motif, phrase)| Vue {
            id,
            etat: (*motif).into(),
            epoque: self.etat.epoque,
            raison: Raison::simple(TypeDecision::Termine, phrase.clone()),
        })
    }

    fn statut(&self) -> Statut {
        let now = maintenant();
        let mut alertes = self.gel.clone();
        if self.mesures.vmmem.is_none() {
            alertes.push("WSL arrêté : seules les réserves Windows comptent.".into());
        }
        if let Some(info) = planif::swap_calme(&self.etat, &self.mesures, &self.config, now) {
            alertes.push(format!("Information : {info}, pas de gel."));
        }
        Statut {
            epoque: self.etat.epoque,
            maintenant: now,
            mesures: self.mesures.clone(),
            reserves: planif::vue_reserves(&self.etat, &self.mesures, &self.config, now),
            tickets: self
                .etat
                .tickets
                .iter()
                .map(|t| VueTicket {
                    id: t.id,
                    etat: t.etat,
                    cote: t.cote,
                    besoin: t.besoin,
                    description: t.description.clone(),
                    session: t.session.clone(),
                    projet: t.projet.clone(),
                    branche: t.branche.clone(),
                    cree: t.cree,
                    accorde: t.accorde,
                    manuel: t.manuel,
                    force: t.force,
                    presence_il_y_a_s: now.saturating_sub(t.presence),
                    phrase: self.raisons.get(&t.id).map(|r| r.phrase.clone()).unwrap_or_default(),
                })
                .collect(),
            alertes,
            gel_manuel: self.etat.gel_manuel,
        }
    }

    fn icone(&self) -> (ui::Couleur, String) {
        let compte = |e| self.etat.tickets.iter().filter(|t| t.etat == e).count();
        let (attente, actifs, orphelins) = (compte(EtatTicket::Attente), compte(EtatTicket::Actif), compte(EtatTicket::Orphelin));
        let couleur = if !self.gel.is_empty() || orphelins > 0 {
            ui::Couleur::Rouge
        } else if attente > 0 {
            ui::Couleur::Orange
        } else if actifs > 0 {
            ui::Couleur::Bleu
        } else {
            ui::Couleur::Vert
        };
        let niv = planif::vue_reserves(&self.etat, &self.mesures, &self.config, maintenant());
        let mut texte = format!(
            "Charge : {actifs} actif(s), {attente} en attente{}\nLibre : Win {} · WSL {} · CPU {}/{}",
            if orphelins > 0 { format!(", {orphelins} orphelin(s)") } else { String::new() },
            niv[0].libre_lisible(),
            if self.mesures.vmmem.is_some() { niv[1].libre_lisible() } else { "arrêté".into() },
            niv[2].libre.max(0),
            niv[3].libre.max(0)
        );
        let tete = self
            .etat
            .tickets
            .iter()
            .find(|t| t.etat == EtatTicket::Attente)
            .and_then(|t| self.raisons.get(&t.id))
            .map(|r| r.phrase.clone())
            .or_else(|| self.gel.first().cloned());
        if let Some(info) = planif::swap_calme(&self.etat, &self.mesures, &self.config, maintenant()) {
            texte.push_str(&format!("\n{info}"));
        }
        if let Some(p) = tete {
            texte.push('\n');
            texte.push_str(&p);
        }
        (couleur, texte)
    }
}

fn reponse(code: u16, corps: Value) -> Reponse {
    Reponse { code, corps }
}

fn erreur(code: u16, texte: &str) -> Reponse {
    reponse(code, json!({ "erreur": texte }))
}

fn en_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or_else(|e| json!({ "erreur": e.to_string() }))
}

fn introuvable(s: &Serveur, id: u64) -> Reponse {
    match s.vue(id) {
        Some(v) => reponse(404, en_json(&v)),
        None => erreur(404, &format!("Ticket #{id} inconnu.")),
    }
}

fn wsl_depuis_parametres(req: &Requete) -> Option<MesuresWsl> {
    Some(MesuresWsl {
        mem_dispo_mo: req.parametre("mem_dispo_mo")?.parse().ok()?,
        mem_totale_mo: req.parametre("mem_totale_mo")?.parse().ok()?,
        charge1: req.parametre("charge1")?.parse().ok()?,
        swap_total_mo: req.parametre("swap_total_mo").and_then(|v| v.parse().ok()).unwrap_or(0),
        swap_libre_mo: req.parametre("swap_libre_mo").and_then(|v| v.parse().ok()).unwrap_or(0),
        pswpin: req.parametre("pswpin").and_then(|v| v.parse().ok()).unwrap_or(0),
        pswpout: req.parametre("pswpout").and_then(|v| v.parse().ok()).unwrap_or(0),
        quand: 0,
    })
}

impl Partage {
    pub fn nouveau(serveur: Serveur) -> Partage {
        Partage { serveur: Mutex::new(serveur), signal: Condvar::new() }
    }

    fn verrou(&self) -> MutexGuard<'_, Serveur> {
        self.serveur.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Réveille les attentes, puis met à jour l'icône et les bulles hors du verrou.
    fn publier(&self, mut s: MutexGuard<'_, Serveur>) {
        self.signal.notify_all();
        let annonces = std::mem::take(&mut s.annonces);
        let (couleur, texte) = s.icone();
        drop(s);
        ui::maj(couleur, &texte);
        for a in annonces {
            ui::bulle(&a.titre, &a.texte, a.alerte);
        }
    }

    pub fn tic(&self, mesures: Mesures) {
        let mut s = self.verrou();
        let (wsl, swap_sortie_ps) = (s.mesures.wsl, s.mesures.swap_sortie_ps);
        s.mesures = Mesures { wsl, swap_sortie_ps, ..mesures };
        let candidat = s.etat.clone();
        let _ = s.valider(candidat, false, None, None);
        self.publier(s);
    }

    pub fn dossier(&self) -> PathBuf {
        self.verrou().dossier.clone()
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn instantane(&self) -> Statut {
        self.verrou().statut()
    }

    /// État après prise en compte d'une mesure WSL fraîche, pour qu'un premier `charge statut` ne montre pas de gel périmé.
    fn etat_avec(&self, wsl: Option<MesuresWsl>) -> Statut {
        let mut s = self.verrou();
        if wsl.is_some() {
            s.maj_wsl(wsl, maintenant());
            let candidat = s.etat.clone();
            let _ = s.valider(candidat, false, None, None);
        }
        let statut = s.statut();
        self.publier(s);
        statut
    }

    pub fn journal(&self, texte: &str) {
        self.verrou().journal(texte);
    }

    /// Question de confirmation d'un forçage, avec le risque chiffré.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn apercu_forcage(&self, id: u64) -> Option<String> {
        let s = self.verrou();
        let mut texte = planif::apercu_forcage(&s.etat, id, &s.mesures, &s.config, maintenant())?;
        if !s.pression.is_empty() {
            texte += " Une pression est en cours : risque de ralentissement.";
        } else if !s.gel.is_empty() {
            texte += " Les admissions sont gelées : le forçage passe outre.";
        }
        Some(texte)
    }

    /// Feu vert donné à la main par Adel depuis la fenêtre ; aucune route du tube n'y mène.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn forcer(&self, id: u64) -> Result<(), String> {
        let mut s = self.verrou();
        let (candidat, detail) = planif::forcer(&s.etat, id, &s.mesures, &s.config, maintenant())?;
        let r = s.valider(candidat, true, None, Some(format!("FORÇAGE {detail} · par Adel (fenêtre)")));
        self.publier(s);
        r
    }

    pub fn geler(&self, gele: bool, origine: &str) -> Reponse {
        let mut s = self.verrou();
        if s.etat.gel_manuel.is_some() == gele {
            let etat = if gele { "déjà gelée" } else { "pas gelée" };
            return reponse(200, json!({ "gele": gele, "phrase": format!("La file est {etat}.") }));
        }
        let mut candidat = s.etat.clone();
        candidat.gel_manuel = gele.then(maintenant);
        let (titre, phrase) = if gele {
            ("GEL MANUEL", "File gelée : aucune nouvelle admission jusqu'au dégel.")
        } else {
            ("DÉGEL MANUEL", "File dégelée : les admissions reprennent.")
        };
        let rep = match s.valider(candidat, true, None, Some(format!("{titre} ({origine}) · {phrase}"))) {
            Ok(()) => reponse(200, json!({ "gele": gele, "phrase": phrase })),
            Err(e) => erreur(503, &e),
        };
        self.publier(s);
        rep
    }

    pub fn clore(&self, id: u64, f: &Fin, comment: Cloture) -> Reponse {
        let mut s = self.verrou();
        let Some(t) = s.etat.ticket(id).cloned() else {
            return introuvable(&s, id);
        };
        let mut candidat = s.etat.clone();
        candidat.tickets.retain(|x| x.id != id);
        let etait = nom_etat(t.etat);
        let (prelude, motif, phrase) = match comment {
            Cloture::Fin => {
                let mut details = Vec::new();
                if let Some(c) = f.code {
                    details.push(format!("code {c}"));
                }
                if let Some(pic) = f.pic_mo {
                    details.push(format!("pic RSS {}", go(pic as i64)));
                }
                if let Some(d) = f.duree_s {
                    details.push(format!("durée {}", duree(d)));
                }
                let suite = if details.is_empty() { String::new() } else { format!(" · {}", details.join(" · ")) };
                (format!("FIN #{id} {}{suite}", resume(&t)), "termine", format!("Ticket #{id} terminé, créneau libéré."))
            }
            Cloture::Annule => {
                (format!("ANNULÉ #{id} {} (était {etait})", resume(&t)), "annule", format!("Ticket #{id} annulé."))
            }
            Cloture::Fenetre => (
                format!("LIBÉRÉ À LA MAIN #{id} {} (était {etait}) depuis la fenêtre", resume(&t)),
                "libere",
                format!("Ticket #{id} libéré à la main depuis la fenêtre de l'orchestrateur."),
            ),
        };
        let rep = match s.valider(candidat, true, None, Some(prelude)) {
            Ok(()) => {
                s.terminer(id, motif, phrase.clone());
                reponse(200, json!({ "id": id, "etat": motif, "phrase": phrase }))
            }
            Err(e) => erreur(503, &e),
        };
        self.publier(s);
        rep
    }
}

pub fn traiter(p: &Partage, req: &Requete, vivant: &dyn Fn() -> bool) -> Reponse {
    let segments = req.segments();
    let id = |s: &str| s.parse::<u64>().ok();
    let invalide = || erreur(404, "ticket invalide");
    match (req.methode.as_str(), segments.as_slice()) {
        ("POST", ["tickets"]) => creer(p, req),
        ("GET", ["tickets", n, "attente"]) => id(n).map_or_else(invalide, |i| attendre(p, req, i, vivant)),
        ("POST", ["tickets", n, "presence"]) => id(n).map_or_else(invalide, |i| presence(p, req, i)),
        ("POST", ["tickets", n, "prolonge"]) => id(n).map_or_else(invalide, |i| prolonger(p, req, i)),
        ("POST", ["tickets", n, "fin"]) => id(n).map_or_else(invalide, |i| {
            let f: Fin = serde_json::from_value(req.corps.clone()).unwrap_or_default();
            p.clore(i, &f, Cloture::Fin)
        }),
        ("DELETE", ["tickets", n]) => id(n).map_or_else(invalide, |i| p.clore(i, &Fin::default(), Cloture::Annule)),
        ("GET", ["etat"]) => reponse(200, en_json(&p.etat_avec(wsl_depuis_parametres(req)))),
        ("POST", ["gel"]) => p.geler(req.corps["gele"].as_bool().unwrap_or(true), "commande"),
        ("POST", ["fenetre"]) => {
            ui::fenetre(req.corps["ouvrir"].as_bool().unwrap_or(true));
            reponse(200, json!({ "phrase": "Demande transmise à la fenêtre." }))
        }
        ("POST", ["quitter"]) => {
            p.journal("ARRÊT demandé par le tube.");
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(200));
                ui::quitter();
            });
            reponse(200, json!({ "phrase": "Arrêt de l'orchestrateur en cours." }))
        }
        _ => erreur(404, "route inconnue"),
    }
}

fn creer(p: &Partage, req: &Requete) -> Reponse {
    let d: Demande = match serde_json::from_value(req.corps.clone()) {
        Ok(d) => d,
        Err(e) => return erreur(400, &format!("demande illisible : {e}")),
    };
    let now = maintenant();
    let mut s = p.verrou();
    s.maj_wsl(d.mesures, now);
    if let Some(id) = s.etat.tickets.iter().find(|t| t.requete_id == d.requete_id).map(|t| t.id) {
        return reponse(200, en_json(&s.vue(id)));
    }
    // Une commande relancée à l'identique reprend son ticket, à condition que personne d'autre ne l'attende.
    let repris = (!d.rattachement.is_empty())
        .then(|| {
            s.etat.tickets.iter().find(|t| {
                t.rattachement == d.rattachement
                    && ((t.etat == EtatTicket::Attente && t.ecoutes == 0) || (t.etat == EtatTicket::Actif && !t.recu))
            })
        })
        .flatten()
        .map(|t| t.id);
    if let Some(id) = repris {
        let mut candidat = s.etat.clone();
        if let Some(t) = candidat.ticket_mut(id) {
            t.rappel = None;
            t.presence = now;
        }
        let rep = match s.valider(candidat, true, Some(id), Some(format!("RATTACHÉ #{id} : la même commande reprend son ticket."))) {
            Ok(()) => reponse(200, en_json(&s.vue(id))),
            Err(e) => erreur(503, &e),
        };
        p.publier(s);
        return rep;
    }
    let defaut = s.config.defaut(d.cote);
    let besoin = Besoin {
        ram_mo: d.ram_mo.unwrap_or(defaut.ram_mo),
        cpu: d.cpu.unwrap_or(defaut.cpu),
        duree_s: d.duree_s.unwrap_or(defaut.duree_s),
    };
    if let Some(raison) = planif::refus(d.cote, &besoin, &s.mesures, &s.config) {
        s.journal(&format!("REFUS [{}] {} · {}", d.session, d.description, raison.phrase));
        s.annoncer("refus", format!("Demande refusée : {}", d.session), tronquer(&raison.phrase, 250));
        let v = Vue { id: 0, etat: "refuse".into(), epoque: s.etat.epoque, raison };
        let rep = reponse(422, en_json(&v));
        p.publier(s);
        return rep;
    }
    let mut candidat = s.etat.clone();
    candidat.compteur += 1;
    let id = candidat.compteur;
    let t = Ticket {
        id,
        requete_id: d.requete_id,
        cote: d.cote,
        besoin,
        description: d.description,
        session: d.session,
        claude_session_id: d.claude_session_id,
        claude_pid: d.claude_pid,
        dossier: d.dossier,
        signature: d.signature,
        etat: EtatTicket::Attente,
        cree: now,
        accorde: None,
        dispo_accord_mo: None,
        manuel: d.manuel,
        recu: false,
        rappel: None,
        cause_orphelin: None,
        rattachement: d.rattachement,
        projet: d.projet,
        branche: d.branche,
        force: false,
        base_ram_windows_mo: None,
        base_ram_wsl_mo: None,
        base_vmmem_mo: None,
        doublages: 0,
        presence: now,
        ecoutes: 0,
    };
    let prelude = format!("DEMANDE #{id} {}", resume(&t));
    candidat.tickets.push(t);
    let rep = match s.valider(candidat, true, Some(id), Some(prelude)) {
        Ok(()) => reponse(201, en_json(&s.vue(id))),
        Err(e) => erreur(503, &e),
    };
    p.publier(s);
    rep
}

fn attendre(p: &Partage, req: &Requete, id: u64, vivant: &dyn Fn() -> bool) -> Reponse {
    let connu = req.parametre("connu").unwrap_or("").to_owned();
    let mut s = p.verrou();
    s.maj_wsl(wsl_depuis_parametres(req), maintenant());
    let plafond = req.parametre("delai_s").and_then(|d| d.parse::<u64>().ok()).unwrap_or(u64::MAX);
    let delai = Duration::from_secs(s.config.long_poll_s.min(plafond));
    let Some(t) = s.etat.ticket(id) else {
        return introuvable(&s, id);
    };
    if t.rappel.is_some() {
        let mut candidat = s.etat.clone();
        if let Some(c) = candidat.ticket_mut(id) {
            c.rappel = None;
            c.presence = maintenant();
        }
        let _ = s.valider(candidat, true, None, Some(format!("RETOUR #{id} : un client attend de nouveau ce ticket.")));
    }
    if let Some(t) = s.etat.ticket_mut(id) {
        t.ecoutes += 1;
        t.presence = maintenant();
    }
    let limite = Instant::now() + delai;
    let vue = loop {
        let v = s.vue(id);
        let change = v.as_ref().is_none_or(|v| v.etat != "attente" || v.raison.cle() != connu);
        let reste = limite.saturating_duration_since(Instant::now());
        if change || reste.is_zero() {
            break v;
        }
        s = p.signal.wait_timeout(s, reste).unwrap_or_else(|e| e.into_inner()).0;
        // Client parti pendant l'attente : on cesse de le compter comme présent.
        if !vivant() {
            break None;
        }
    };
    if let Some(t) = s.etat.ticket_mut(id) {
        t.ecoutes = t.ecoutes.saturating_sub(1);
        t.presence = maintenant();
    }
    let rep = match vue {
        Some(v) if v.raison.decision == TypeDecision::Termine => reponse(404, en_json(&v)),
        Some(v) => {
            if v.etat == "actif" {
                let candidat = s.etat.clone();
                let _ = s.valider(candidat, false, Some(id), None);
            }
            reponse(200, en_json(&v))
        }
        None => erreur(404, &format!("Ticket #{id} inconnu ou client parti.")),
    };
    p.publier(s);
    rep
}

fn presence(p: &Partage, req: &Requete, id: u64) -> Reponse {
    let corps: Presence = serde_json::from_value(req.corps.clone()).unwrap_or_default();
    let now = maintenant();
    let mut s = p.verrou();
    s.maj_wsl(corps.mesures, now);
    let Some(t) = s.etat.ticket_mut(id) else {
        return introuvable(&s, id);
    };
    t.presence = now;
    let (etat, cause, recu) = (t.etat, t.cause_orphelin, t.recu);
    if etat == EtatTicket::Orphelin && cause != Some(CauseOrphelin::Bail) {
        let mut candidat = s.etat.clone();
        if let Some(c) = candidat.ticket_mut(id) {
            c.etat = EtatTicket::Actif;
            c.cause_orphelin = None;
            c.recu = true;
        }
        let prelude = format!("RETOUR #{id} : signal reçu, ticket de nouveau actif.");
        let _ = s.valider(candidat, true, None, Some(prelude));
    } else if etat == EtatTicket::Actif && !recu {
        let candidat = s.etat.clone();
        let _ = s.valider(candidat, false, Some(id), None);
    }
    let etat = s.etat.ticket(id).map(|t| nom_etat(t.etat)).unwrap_or("inconnu");
    let rep = reponse(200, json!({ "etat": etat, "epoque": s.etat.epoque, "ordre": null }));
    p.publier(s);
    rep
}

fn prolonger(p: &Partage, req: &Requete, id: u64) -> Reponse {
    let d: Prolonge = match serde_json::from_value(req.corps.clone()) {
        Ok(d) => d,
        Err(e) => return erreur(400, &format!("prolongation illisible : {e}")),
    };
    let now = maintenant();
    let mut s = p.verrou();
    let Some(t) = s.etat.ticket(id).cloned() else {
        return introuvable(&s, id);
    };
    let (Some(accorde), true) = (t.accorde, t.etat != EtatTicket::Attente) else {
        return erreur(409, &format!("Ticket #{id} en attente : rien à prolonger, sa durée compte à partir du feu vert."));
    };
    let fin = t.fin_prevue().unwrap_or(now).max(now) + d.duree_s;
    let mut candidat = s.etat.clone();
    if let Some(c) = candidat.ticket_mut(id) {
        c.besoin.duree_s = fin - accorde;
        c.etat = EtatTicket::Actif;
        c.cause_orphelin = None;
        c.recu = true;
        c.presence = now;
    }
    let phrase = format!("Ticket #{id} prolongé de {} : fin prévue vers {}.", duree(d.duree_s), hhmm(fin));
    let prelude = format!("PROLONGÉ #{id} {} · {phrase} Raison : {}", resume(&t), d.raison);
    let rep = match s.valider(candidat, true, None, Some(prelude)) {
        Ok(()) => reponse(200, json!({ "id": id, "etat": "actif", "phrase": phrase })),
        Err(e) => erreur(503, &e),
    };
    p.publier(s);
    rep
}

/// Sert une connexion : une requête JSON par ligne, une réponse par ligne, jusqu'à la fin du flux.
pub fn servir_connexion(p: &Partage, lecture: impl Read, mut ecriture: impl Write, vivant: &dyn Fn() -> bool) {
    let mut lecteur = BufReader::new(lecture);
    while let Ok(Some(ligne)) = lire_ligne(&mut lecteur) {
        if ligne.trim().is_empty() {
            continue;
        }
        let rep = match serde_json::from_str::<Requete>(&ligne) {
            Ok(r) => traiter(p, &r, vivant),
            Err(e) => erreur(400, &format!("requête illisible : {e}")),
        };
        let mut texte = serde_json::to_string(&rep).unwrap_or_else(|_| "{\"code\":500}".into());
        texte.push('\n');
        if ecriture.write_all(texte.as_bytes()).and_then(|_| ecriture.flush()).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_800_000_000;

    #[test]
    fn pression_qui_oscille_ne_donne_pas_de_bulle() {
        let mut b = Bulles::default();
        for cycle in 0..20 {
            let debut = T + cycle * 90;
            for k in 0..30 {
                assert!(!b.pression("pression ram_windows", true, true, debut + k * 2));
            }
            assert!(!b.pression("pression ram_windows", false, true, debut + 60));
        }
    }

    #[test]
    fn pression_sans_file_journal_seulement() {
        let mut b = Bulles::default();
        for k in 0..300 {
            assert!(!b.pression("pression ram_windows", true, false, T + k * 2));
        }
    }

    #[test]
    fn pression_durable_avec_file_une_bulle_par_demi_heure() {
        let mut b = Bulles::default();
        let mut bulles = Vec::new();
        for k in 0..(3600 / 2) {
            let t = T + k * 2;
            if b.pression("pression ram_windows", true, true, t) {
                bulles.push(t - T);
            }
        }
        assert_eq!(bulles, vec![120, 120 + 1800]);
    }

    #[test]
    fn debit_de_sortie_du_swap_sur_30_s() {
        let mut h = VecDeque::new();
        assert_eq!(debit_sortie_swap(&mut h, T, 1000), 0.0);
        assert_eq!(debit_sortie_swap(&mut h, T + 10, 1000 + 3000), 300.0);
        for k in 1..=20 {
            debit_sortie_swap(&mut h, T + 10 + k * 2, 4000);
        }
        // 30 s plus tard sans nouvelle sortie : le débit retombe à 0.
        assert_eq!(debit_sortie_swap(&mut h, T + 52, 4000), 0.0);
        // Compteurs remis à zéro (WSL redémarré) : pas de débit négatif.
        assert_eq!(debit_sortie_swap(&mut h, T + 54, 10), 0.0);

        // Ancien client (pswpout absent, donc 0) mêlé à un nouveau : pas de faux pic (incident du 05/11 à 23:44).
        let mut h = VecDeque::new();
        assert_eq!(debit_sortie_swap(&mut h, T, 0), 0.0);
        assert_eq!(debit_sortie_swap(&mut h, T + 2, 4_200_000), 0.0);
        assert_eq!(debit_sortie_swap(&mut h, T + 4, 0), 0.0);
        assert_eq!(debit_sortie_swap(&mut h, T + 6, 4_200_040), 10.0);
    }

    #[test]
    fn une_bulle_par_genre_toutes_les_30_min() {
        let mut b = Bulles::default();
        assert!(b.autoriser("orphelin", T));
        assert!(!b.autoriser("orphelin", T + 60));
        assert!(b.autoriser("refus", T + 60), "les genres sont indépendants");
        assert!(!b.autoriser("orphelin", T + 1799));
        assert!(b.autoriser("orphelin", T + 1800));
    }
}
