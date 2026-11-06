use super::*;
use crate::modele::Vmmem;

const H: u64 = 1_800_000_000;

/// Les cas historiques gardent la marge Windows de 4 Go de leurs chiffres ; `cas_reel_du_05_11` prend les défauts.
fn config() -> Config {
    Config { marge_ram_windows_mo: 4096, ..Config::default() }
}

fn mesures(ram_windows_dispo_go: f64, wsl_dispo_go: f64, vmmem_go: f64) -> Mesures {
    Mesures {
        ram_totale_mo: 32 * 1024,
        ram_dispo_mo: (ram_windows_dispo_go * 1024.0) as u64,
        commit_mo: 0,
        commit_limite_mo: 0,
        cpu_pct: 0.0,
        coeurs_windows: 16,
        swap_sortie_ps: 0.0,
        vmmem: Some(Vmmem { pid: 4242, rss_mo: (vmmem_go * 1024.0) as u64 }),
        wsl: Some(MesuresWsl {
            mem_dispo_mo: (wsl_dispo_go * 1024.0) as u64,
            mem_totale_mo: 11_960,
            charge1: 1.0,
            swap_total_mo: 4096,
            swap_libre_mo: 4096,
            pswpin: 0,
            pswpout: 0,
            quand: H,
        }),
    }
}

fn frais(mut m: Mesures, quand: u64) -> Mesures {
    if let Some(w) = &mut m.wsl {
        w.quand = quand;
    }
    m
}

fn large() -> Mesures {
    mesures(20.0, 8.0, 4.0)
}

fn ticket(id: u64, cote: Cote, ram_go: f64, cpu: u32, duree_min: u64) -> Ticket {
    Ticket {
        id,
        requete_id: format!("r{id}"),
        cote,
        besoin: Besoin { ram_mo: (ram_go * 1024.0) as u64, cpu, duree_s: duree_min * 60 },
        description: format!("tâche {id}"),
        session: "essai".into(),
        claude_session_id: None,
        claude_pid: None,
        dossier: "/".into(),
        signature: String::new(),
        etat: EtatTicket::Attente,
        cree: H,
        accorde: None,
        dispo_accord_mo: None,
        manuel: false,
        recu: false,
        bail_depasse: false,
        rappel: None,
        cause_orphelin: None,
        rattachement: String::new(),
        projet: None,
        branche: None,
        force: false,
        base_ram_windows_mo: None,
        base_ram_wsl_mo: None,
        base_vmmem_mo: None,
        doublages: 0,
        liaison: None,
        identite: None,
        perdu: None,
        presence: H,
        ecoutes: 1,
    }
}

fn actif(mut t: Ticket, accorde: u64, dispo_accord_go: f64) -> Ticket {
    t.etat = EtatTicket::Actif;
    t.accorde = Some(accorde);
    t.presence = accorde;
    t.dispo_accord_mo = Some((dispo_accord_go * 1024.0) as u64);
    t.recu = true;
    t
}

fn etat(tickets: Vec<Ticket>) -> Etat {
    Etat {
        epoque: 1,
        compteur: tickets.iter().map(|t| t.id).max().unwrap_or(0),
        vmmem_pid: Some(4242),
        gel_manuel: None,
        tickets,
        ..Etat::default()
    }
}

fn present(mut e: Etat, quand: u64) -> Etat {
    for t in &mut e.tickets {
        t.presence = quand;
    }
    e
}

fn etat_de(e: &Etat, id: u64) -> Option<EtatTicket> {
    e.ticket(id).map(|t| t.etat)
}

#[test]
fn file_vide_feu_vert_immediat() {
    let e = etat(vec![ticket(1, Cote::Wsl, 2.0, 2, 10)]);
    let p = planifier(&e, &large(), &config(), H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(1)]);
    assert_eq!(etat_de(&p.etat, 1), Some(EtatTicket::Actif));
    assert_eq!(p.raisons[&1].decision, TypeDecision::FeuVert);
    assert!(p.persister);
}

#[test]
fn plusieurs_feux_verts_dans_l_ordre_tant_que_ca_tient() {
    let e = etat(vec![ticket(1, Cote::Wsl, 2.0, 2, 10), ticket(2, Cote::Wsl, 2.0, 2, 10), ticket(3, Cote::Wsl, 2.0, 2, 10)]);
    let p = planifier(&e, &large(), &config(), H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(1), Evenement::FeuVert(2), Evenement::FeuVert(3)]);
    let e = etat(vec![
        ticket(1, Cote::Wsl, 3.0, 2, 10),
        ticket(2, Cote::Wsl, 3.0, 2, 10),
        ticket(3, Cote::Wsl, 3.0, 2, 10),
    ]);
    let p = planifier(&e, &large(), &config(), H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(1), Evenement::FeuVert(2)]);
    assert_eq!(p.raisons[&3].decision, TypeDecision::Attente);
    assert_eq!(p.raisons[&3].position, Some(1));
}

#[test]
fn remplissage_d_un_ticket_qui_finit_avant_t() {
    let e = etat(vec![
        actif(ticket(1, Cote::Wsl, 5.0, 2, 30), H - 60, 8.0),
        ticket(2, Cote::Wsl, 3.0, 2, 10),
        ticket(3, Cote::Wsl, 0.5, 1, 1),
    ]);
    let p = planifier(&e, &large(), &config(), H);
    // Le #3 utilise la RAM WSL qui manque au #2, mais finit (1 min) avant son démarrage estimé (fin du #1).
    assert_eq!(p.evenements, vec![Evenement::FeuVert(3)]);
    let r2 = &p.raisons[&2];
    assert_eq!(r2.decision, TypeDecision::Attente);
    assert_eq!(r2.position, Some(1));
    assert_eq!(r2.reserve.as_deref(), Some("ram_wsl"));
    assert_eq!(r2.bloquants, vec![1]);
    assert_eq!(r2.debloque_vers, Some(H - 60 + 30 * 60));
    assert!(r2.phrase.contains("RAM WSL insuffisante"), "{}", r2.phrase);
    assert!(r2.phrase.contains("Débloqué à la fin du #1"), "{}", r2.phrase);
    assert!(r2.phrase.contains("Doublé par #3 (1/3)"), "{}", r2.phrase);
    let r3 = &p.raisons[&3];
    assert_eq!(r3.decision, TypeDecision::FeuVert);
    assert!(r3.phrase.contains("passé devant #2 : finit avant son démarrage estimé"), "{}", r3.phrase);
    assert_eq!(p.etat.ticket(2).unwrap().doublages, 1);

    // Un suivant qui utilise la RAM WSL manquante et finit après T reste derrière, avec la raison.
    let e = etat(vec![
        actif(ticket(1, Cote::Wsl, 5.0, 2, 30), H - 60, 8.0),
        ticket(2, Cote::Wsl, 3.0, 2, 10),
        ticket(3, Cote::Wsl, 0.5, 1, 60),
    ]);
    let p = planifier(&e, &large(), &config(), H);
    assert!(p.evenements.is_empty());
    assert!(p.raisons[&3].phrase.contains("il utilise aussi la RAM WSL qui manque au #2"), "{}", p.raisons[&3].phrase);
}

/// Bug de surveillance n°5 (05/11) : #28 (Windows) bloqué « derrière #26 » qui attendait la RAM WSL tenue par #24.
#[test]
fn remplissage_cas_reel_du_05_11() {
    let c = Config::default();
    // D'après la surveillance : 5,7 Go et 8 cœurs libres côté Windows (8,2 disponibles − 1,5 de marge − 1 Go de
    // croissance possible de la VM pour le #24), RAM WSL épuisée.
    let m = mesures(8.2, 3.0, 11.0);
    let e = etat(vec![
        actif(ticket(24, Cote::Wsl, 4.0, 6, 60), H, 6.0),
        ticket(26, Cote::Wsl, 3.0, 4, 15),
        ticket(28, Cote::Windows, 4.0, 4, 5),
    ]);
    let p = planifier(&present(e, H + 120), &frais(m, H + 120), &c, H + 120);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(28)], "{}", p.raisons[&28].phrase);
    // #26 manque de RAM WSL (tenue par #24) et de CPU WSL (6 + 4 cœurs sur 10 − 2) : #28 n'utilise ni l'un ni l'autre.
    assert!(
        p.raisons[&28].phrase.contains("passé devant #26 : n'utilise ni la RAM WSL ni le CPU WSL, qui lui manquent"),
        "{}",
        p.raisons[&28].phrase
    );
    // Ressource différente : il passe sans compter dans l'anti-famine.
    assert!(p.raisons[&26].phrase.contains("Passés sur une ressource dont il n'a pas besoin : #28"), "{}", p.raisons[&26].phrase);
    assert_eq!(p.etat.ticket(26).unwrap().doublages, 0);
}

#[test]
fn anti_famine() {
    let c = config();
    let tete = |doublages: u32, cree: u64| {
        let mut t = ticket(2, Cote::Wsl, 3.0, 2, 10);
        t.doublages = doublages;
        t.cree = cree;
        t
    };
    // Utilise la RAM WSL qui manque à la tête, mais finit (1 min) avant T : passage compté.
    let conflit = || ticket(3, Cote::Wsl, 0.5, 1, 1);
    let occupant = || actif(ticket(1, Cote::Wsl, 5.0, 2, 30), H - 60, 8.0);
    let p = planifier(&etat(vec![occupant(), tete(0, H), conflit()]), &large(), &c, H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(3)]);
    assert_eq!(p.etat.ticket(2).unwrap().doublages, 1);
    // Déjà doublée 3 fois : plus de passage « fin avant T ».
    let p = planifier(&etat(vec![occupant(), tete(3, H), conflit()]), &large(), &c, H);
    assert!(p.evenements.is_empty());
    assert!(p.raisons[&3].phrase.contains("anti-famine : il utilise aussi la RAM WSL et #2 a déjà été doublé 3 fois"), "{}", p.raisons[&3].phrase);
    assert!(p.raisons[&2].phrase.contains("Anti-famine : plus aucun ticket ne passe devant"), "{}", p.raisons[&2].phrase);
    // Tête en attente depuis 20 min : idem.
    assert!(planifier(&etat(vec![occupant(), tete(0, H - 20 * 60), conflit()]), &large(), &c, H).evenements.is_empty());
    // Réglable : avec 5 dépassements permis, le 4e passe.
    let c5 = Config { max_doublages: 5, ..config() };
    assert_eq!(planifier(&etat(vec![occupant(), tete(3, H), conflit()]), &large(), &c5, H).evenements, vec![Evenement::FeuVert(3)]);
    // Le gel manuel arrête tout remplissage.
    let mut e = etat(vec![occupant(), tete(0, H), ticket(3, Cote::Windows, 1.0, 1, 5)]);
    e.gel_manuel = Some(H);
    assert!(planifier(&e, &large(), &c, H).evenements.is_empty());
}

/// Cas réel du 05/11 : #39 (Windows) bloqué « derrière #33 (anti-famine) » alors que #33 (WSL) n'attendait que la RAM WSL.
#[test]
fn anti_famine_ignore_les_ressources_differentes() {
    let c = Config::default();
    let mut t33 = ticket(33, Cote::Wsl, 4.0, 6, 30);
    t33.cree = H - 55 * 60;
    t33.doublages = 3;
    // #33 ne manque que de RAM WSL (3 Go disponibles − 1,5 de marge < 4 Go) ; Windows a 7,5 Go et 14 cœurs libres.
    let e = etat(vec![t33, ticket(39, Cote::Windows, 3.0, 4, 10)]);
    let p = planifier(&present(e, H), &frais(mesures(9.0, 3.0, 7.0), H), &c, H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(39)], "{}", p.raisons[&39].phrase);
    assert!(p.raisons[&39].phrase.contains("passé devant #33 : n'utilise pas la RAM WSL qui lui manque"), "{}", p.raisons[&39].phrase);
    assert_eq!(p.etat.ticket(33).unwrap().doublages, 3, "non compté");
}

#[test]
fn refus_immediat_des_besoins_impossibles() {
    let c = config();
    let m = large();
    let r = refus(Cote::Wsl, &Besoin { ram_mo: 11 * 1024, cpu: 2, duree_s: 60 }, &m, &c).unwrap();
    assert_eq!(r.decision, TypeDecision::Refus);
    assert_eq!(r.reserve.as_deref(), Some("ram_wsl"));
    assert!(r.phrase.contains("RAM WSL impossible"), "{}", r.phrase);
    assert!(r.phrase.contains("--ram à 10.5G max"), "{}", r.phrase);

    // WSL : au-delà de 10 − 2 cœurs, admise en exclusif ; refusée seulement au-delà de 10.
    assert!(refus(Cote::Wsl, &Besoin { ram_mo: 2048, cpu: 10, duree_s: 60 }, &m, &c).is_none());
    let r = refus(Cote::Wsl, &Besoin { ram_mo: 2048, cpu: 11, duree_s: 60 }, &m, &c).unwrap();
    assert_eq!(r.reserve.as_deref(), Some("cpu"));
    assert!(r.phrase.contains("--cpu à 10 max"), "{}", r.phrase);
    // Windows : son propre budget de 16 cœurs logiques.
    assert!(refus(Cote::Windows, &Besoin { ram_mo: 2048, cpu: 11, duree_s: 60 }, &m, &c).is_none());
    let r = refus(Cote::Windows, &Besoin { ram_mo: 2048, cpu: 17, duree_s: 60 }, &m, &c).unwrap();
    assert_eq!(r.reserve.as_deref(), Some("cpu_windows"));
    assert!(r.phrase.contains("--cpu à 16 max"), "{}", r.phrase);

    // 11 Go côté Windows tiennent dans 32 − 4 Go : la RAM WSL ne compte pas pour une tâche Windows.
    assert!(refus(Cote::Windows, &Besoin { ram_mo: 11 * 1024, cpu: 2, duree_s: 60 }, &m, &c).is_none());
    assert!(refus(Cote::Windows, &Besoin { ram_mo: 29 * 1024, cpu: 2, duree_s: 60 }, &m, &c).is_some());
}

#[test]
fn la_ram_wsl_debite_aussi_la_ram_windows() {
    // Windows : 9 Go disponibles − 4 Go de marge = 5 Go ; un actif WSL de 4 Go en réserve 4.
    let m = mesures(9.0, 8.0, 4.0);
    let e = etat(vec![actif(ticket(1, Cote::Wsl, 4.0, 2, 30), H - 60, 8.0), ticket(2, Cote::Windows, 3.0, 2, 10)]);
    let p = planifier(&e, &m, &config(), H);
    assert!(p.evenements.is_empty());
    let r = &p.raisons[&2];
    assert_eq!(r.reserve.as_deref(), Some("ram_windows"));
    assert_eq!(r.reserve_actifs, Some(4096));

    let niv = niveaux(&p.etat, &m, &config(), H);
    assert_eq!(niv[Reserve::RamWindows.i()].reserve, 4096);
    assert_eq!(niv[Reserve::RamWsl.i()].reserve, 4096);
    // Une tâche Windows ne débite pas la RAM WSL.
    let e = etat(vec![actif(ticket(1, Cote::Windows, 4.0, 2, 30), H - 60, 8.0)]);
    let niv = niveaux(&e, &m, &config(), H);
    assert_eq!(niv[Reserve::RamWsl.i()].reserve, 0);
    assert_eq!(niv[Reserve::RamWindows.i()].reserve, 4096);
}

#[test]
fn ram_wsl_suit_memavailable_et_se_replie_sur_vmmem() {
    let c = config();
    // Nuit du 05/11 : vmmemWSL à 9,5 Go (cache compris) mais MemAvailable à 5,5 Go : c'est MemAvailable qui compte.
    let m = mesures(20.0, 5.5, 9.5);
    assert_eq!(disponible(Reserve::RamWsl, &m, &c, H), 5632);
    assert_eq!(disponible(Reserve::RamWsl, &m, &c, H + FRAICHEUR_WSL_S), 5632);
    // Mesure WSL périmée (plus de 30 s) : repli sur 12 Go − vmmemWSL.
    assert_eq!(disponible(Reserve::RamWsl, &m, &c, H + FRAICHEUR_WSL_S + 1), 2560);
    // WSL arrêté et aucune mesure : le plafond entier.
    let mut arrete = m.clone();
    arrete.vmmem = None;
    arrete.wsl = None;
    assert_eq!(disponible(Reserve::RamWsl, &arrete, &c, H), 12288);
    // Un ticket WSL de 3 Go passe avec MemAvailable, alors que l'ancienne règle min(…) l'aurait fait attendre.
    let e = etat(vec![ticket(1, Cote::Wsl, 3.0, 2, 10)]);
    assert_eq!(planifier(&e, &m, &c, H).evenements, vec![Evenement::FeuVert(1)]);
}

#[test]
fn attente_sans_client_sautee_apres_90_s_puis_retiree_apres_15_min() {
    let mut t = ticket(1, Cote::Wsl, 9.0, 2, 10);
    t.ecoutes = 0;
    let mut e = etat(vec![actif(ticket(2, Cote::Wsl, 5.0, 2, 30), H, 8.0), t, ticket(3, Cote::Wsl, 1.0, 1, 5)]);
    e.ticket_mut(2).unwrap().presence = H + 89;
    let p = planifier(&e, &large(), &config(), H + 90);
    assert!(p.evenements.is_empty(), "le #1 est encore attendu, le #3 reste derrière");
    assert!(p.raisons[&3].phrase.contains("derrière #1"));

    let mut e2 = e.clone();
    e2.ticket_mut(2).unwrap().presence = H + 91;
    let p = planifier(&e2, &large(), &config(), H + 91);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(3)], "le #1 sans client est sauté, sa place est gardée");
    let r1 = &p.raisons[&1];
    assert_eq!(r1.reserve.as_deref(), Some("absent"));
    assert_eq!(r1.position, Some(1));
    assert!(r1.phrase.contains("Relance « charge attend 1 »"), "{}", r1.phrase);
    assert!(p.etat.ticket(1).is_some());

    let mut e3 = e.clone();
    e3.ticket_mut(2).unwrap().presence = H + 15 * 60;
    let p = planifier(&e3, &large(), &config(), H + 15 * 60 + 1);
    assert!(p.evenements.contains(&Evenement::Expire(1)));

    // Une connexion long-poll ouverte vaut présence.
    e.ticket_mut(1).unwrap().ecoutes = 1;
    let p = planifier(&e, &large(), &config(), H + 3000);
    assert!(!p.evenements.contains(&Evenement::Expire(1)));
}

#[test]
fn feu_vert_non_remis_rappele_apres_2_min_puis_saute() {
    let e = etat(vec![ticket(1, Cote::Wsl, 1.0, 1, 10)]);
    let p = planifier(&e, &large(), &config(), H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(1)]);
    let mut e = p.etat;
    e.ticket_mut(1).unwrap().ecoutes = 0;
    let p = planifier(&e, &large(), &config(), H + 119);
    assert!(p.evenements.is_empty());
    assert!(p.raisons[&1].phrase.contains("pas encore accusé"), "{}", p.raisons[&1].phrase);
    let p = planifier(&e, &large(), &config(), H + 120);
    assert_eq!(p.evenements, vec![Evenement::Rappel(1)]);
    let t = p.etat.ticket(1).unwrap();
    assert_eq!((t.etat, t.accorde, t.rappel), (EtatTicket::Attente, None, Some(H + 120)));
    assert!(p.raisons[&1].phrase.contains("feu vert rappelé"), "{}", p.raisons[&1].phrase);

    // Rappelé, il ne reprend pas un feu vert tout seul et laisse passer les suivants.
    let mut e = p.etat;
    e.tickets.push(ticket(2, Cote::Wsl, 1.0, 1, 10));
    e.ticket_mut(2).unwrap().presence = H + 130;
    let p = planifier(&e, &large(), &config(), H + 130);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(2)]);

    // Remis, il n'est jamais rappelé.
    let mut e = etat(vec![actif(ticket(1, Cote::Wsl, 1.0, 1, 10), H, 8.0)]);
    e.ticket_mut(1).unwrap().presence = H + 300;
    assert!(planifier(&e, &large(), &config(), H + 300).evenements.is_empty());
}

#[test]
fn bail_d_un_ticket_manuel() {
    let mut t = actif(ticket(1, Cote::Wsl, 2.0, 2, 10), H, 8.0);
    t.manuel = true;
    let e = etat(vec![t, ticket(2, Cote::Wsl, 1.0, 1, 5)]);
    // Sans superviseur, pas d'orphelin par silence avant l'échéance.
    let mut avant = e.clone();
    avant.ticket_mut(2).unwrap().presence = H + 599;
    let p = planifier(&avant, &large(), &config(), H + 599);
    assert!(!p.evenements.iter().any(|ev| matches!(
        ev,
        Evenement::Orphelin(1) | Evenement::Bail(1) | Evenement::BailDepasse(1)
    )));
    assert!(p.raisons[&1].phrase.contains("Bail jusqu'à"), "{}", p.raisons[&1].phrase);

    // À l'échéance : bail dépassé, ticket toujours actif et réservé, la file continue.
    let mut echu = e.clone();
    echu.ticket_mut(2).unwrap().presence = H + 600;
    let p = planifier(&echu, &large(), &config(), H + 600);
    assert_eq!(p.evenements, vec![Evenement::BailDepasse(1), Evenement::FeuVert(2)]);
    let t = p.etat.ticket(1).unwrap();
    assert_eq!((t.etat, t.bail_depasse), (EtatTicket::Actif, true));
    assert!(p.gel.is_empty(), "{:?}", p.gel);
    assert!(niveaux(&p.etat, &large(), &config(), H + 600)[Reserve::RamWsl.i()].par.contains(&1));
    let phrase = &p.raisons[&1].phrase;
    assert!(phrase.contains("Bail dépassé depuis 0 s"), "{phrase}");
    assert!(phrase.contains("charge prolonge 1") && phrase.contains("charge fin 1"), "{phrase}");
    assert!(phrase.contains(&format!("orphelin à {}", hhmm(H + 600 + 900))), "{phrase}");

    // L'événement n'est donné qu'une fois.
    let mut e = p.etat;
    e.tickets.retain(|t| t.id != 2);
    assert!(planifier(&e, &large(), &config(), H + 1000).evenements.is_empty());

    // 15 min après l'échéance sans aucune nouvelle : orphelin, admissions gelées comme avant.
    let mut attente = ticket(3, Cote::Wsl, 1.0, 1, 5);
    attente.presence = H + 1500;
    e.tickets.push(attente);
    let p = planifier(&e, &large(), &config(), H + 1500);
    assert_eq!(p.evenements, vec![Evenement::Bail(1)]);
    let t = p.etat.ticket(1).unwrap();
    assert_eq!((t.etat, t.cause_orphelin), (EtatTicket::Orphelin, Some(CauseOrphelin::Bail)));
    assert_eq!(p.raisons[&3].decision, TypeDecision::Gel);
    assert!(p.raisons[&1].phrase.contains("bail échu depuis 15 min"), "{}", p.raisons[&1].phrase);
    assert!(p.raisons[&1].phrase.contains("charge prolonge 1"), "{}", p.raisons[&1].phrase);

    // Jamais de libération sur les mesures, même mémoire revenue depuis longtemps.
    let mut e = p.etat;
    for k in 0..10 {
        let quand = H + 1600 + k * 30;
        e.ticket_mut(3).unwrap().presence = quand;
        let p = planifier(&e, &frais(large(), quand), &config(), quand);
        assert!(!p.evenements.iter().any(|ev| matches!(ev, Evenement::Libere(1, _))));
        e = p.etat;
    }
}

#[test]
fn cas_reel_65_bail_echu_session_vivante_sans_gel() {
    // Cas réel : #65 (manuel, wsl, 2 Go, 2 cœurs, 15 min) accordé à 13:33:05, bail échu à 13:48:05 alors que la
    // session travaillait encore ; prolongé de 30 min à 14:13:33, fini à 14:28:45. La file était gelée entre-temps.
    let (c, m) = (config(), large());
    let accord = H;
    let echeance = accord + 15 * 60;
    let mut t65 = actif(ticket(65, Cote::Wsl, 2.0, 2, 15), accord, 8.0);
    t65.manuel = true;

    let p = planifier(&etat(vec![t65]), &m, &c, echeance + 1);
    assert_eq!(p.evenements, vec![Evenement::BailDepasse(65)]);
    assert!(p.gel.is_empty(), "{:?}", p.gel);
    assert_eq!(p.raisons[&65].decision, TypeDecision::Actif);

    // Une autre session demande un créneau WSL pendant le dépassement : elle passe sur ce qui reste.
    let mut e = p.etat;
    let mut t66 = ticket(66, Cote::Wsl, 3.0, 4, 10);
    t66.presence = echeance + 60;
    e.tickets.push(t66);
    let p = planifier(&e, &m, &c, echeance + 60);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(66)]);
    let mut e = p.etat;
    e.tickets.retain(|t| t.id != 66);

    // La session se manifeste à 14:00:05 (« charge attend 65 ») : l'orphelin recule d'autant.
    e.ticket_mut(65).unwrap().presence = accord + 27 * 60;
    let p = planifier(&e, &m, &c, accord + 40 * 60);
    assert!(p.evenements.is_empty(), "{:?}", p.evenements);
    assert_eq!(etat_de(&p.etat, 65), Some(EtatTicket::Actif));
    assert!(p.gel.is_empty(), "{:?}", p.gel);
    let phrase = &p.raisons[&65].phrase;
    assert!(phrase.contains("Bail dépassé depuis 25 min"), "{phrase}");
    assert!(phrase.contains(&format!("orphelin à {}", hhmm(accord + 42 * 60))), "{phrase}");

    // Prolongation de 30 min à 14:13:33 : bail de nouveau en cours.
    let mut e = p.etat;
    let prolonge = e.ticket_mut(65).unwrap();
    prolonge.besoin.duree_s = 40 * 60 + 28 + 30 * 60;
    prolonge.bail_depasse = false;
    prolonge.presence = accord + 40 * 60 + 28;
    let p = planifier(&e, &m, &c, accord + 41 * 60);
    assert!(p.evenements.is_empty(), "{:?}", p.evenements);
    assert!(p.raisons[&65].phrase.contains("Bail jusqu'à"), "{}", p.raisons[&65].phrase);
}

#[test]
fn gel_manuel() {
    let mut e = etat(vec![ticket(1, Cote::Windows, 1.0, 1, 5), ticket(2, Cote::Windows, 1.0, 1, 5)]);
    e.gel_manuel = Some(H - 60);
    let p = planifier(&e, &large(), &config(), H);
    assert!(p.evenements.is_empty());
    let r = &p.raisons[&1];
    assert_eq!(r.decision, TypeDecision::Gel);
    assert!(r.phrase.contains("gel manuel depuis"), "{}", r.phrase);
    assert!(p.raisons[&2].phrase.contains("derrière #1"));
    assert!(p.gel.iter().any(|g| g.contains("gel manuel")));
    e.gel_manuel = None;
    assert_eq!(planifier(&e, &large(), &config(), H).evenements.len(), 2);
}

#[test]
fn actif_silencieux_devient_orphelin_et_gele_les_admissions() {
    let e = etat(vec![actif(ticket(1, Cote::Wsl, 1.0, 1, 30), H, 8.0), ticket(2, Cote::Wsl, 1.0, 1, 5)]);
    let p = planifier(&e, &large(), &config(), H + 61);
    assert_eq!(p.evenements, vec![Evenement::Orphelin(1)]);
    assert_eq!(etat_de(&p.etat, 1), Some(EtatTicket::Orphelin));
    let r2 = &p.raisons[&2];
    assert_eq!(r2.decision, TypeDecision::Gel);
    assert_eq!(r2.bloquants, vec![1]);
    assert!(r2.phrase.contains("ticket #1 orphelin"), "{}", r2.phrase);
    assert!(r2.phrase.contains("« charge annule 1 »"), "{}", r2.phrase);
    assert_eq!(p.raisons[&1].decision, TypeDecision::Orphelin);
    assert!(!p.gel.is_empty());

    // La réservation de l'orphelin reste comptée.
    let niv = niveaux(&p.etat, &large(), &config(), H + 61);
    assert_eq!(niv[Reserve::RamWsl.i()].reserve, 1024);
}

#[test]
fn orphelin_jamais_libere_sur_les_mesures() {
    let c = config();
    let mut e = etat(vec![actif(ticket(1, Cote::Wsl, 3.0, 2, 30), H, 6.0)]);
    e.ticket_mut(1).unwrap().etat = EtatTicket::Orphelin;
    e.ticket_mut(1).unwrap().cause_orphelin = Some(CauseOrphelin::Silence);
    for k in 0..20 {
        let quand = H + 100 + k * 30;
        let p = planifier(&e, &frais(mesures(20.0, 8.0, 4.0), quand), &c, quand);
        assert!(p.evenements.is_empty(), "{:?}", p.evenements);
        e = p.etat;
    }
    assert_eq!(e.ticket(1).unwrap().etat, EtatTicket::Orphelin);
}

#[test]
fn arret_de_wsl_libere_les_tickets_wsl_seulement() {
    let e = etat(vec![
        actif(ticket(1, Cote::Wsl, 2.0, 2, 30), H, 8.0),
        actif(ticket(2, Cote::Windows, 2.0, 2, 30), H, 20.0),
        ticket(3, Cote::Wsl, 1.0, 1, 5),
    ]);
    let mut m = large();
    m.vmmem = None;
    m.wsl = None;
    let p = planifier(&e, &m, &config(), H + 5);
    assert_eq!(
        p.evenements,
        vec![Evenement::Libere(1, Liberation::WslArrete), Evenement::Libere(3, Liberation::WslArrete)]
    );
    assert_eq!(etat_de(&p.etat, 2), Some(EtatTicket::Actif));
    assert_eq!(p.etat.vmmem_pid, None);

    // Redémarrage de WSL : nouveau vmmemWSL, aucun ticket WSL n'y survit.
    let mut m2 = large();
    m2.vmmem = Some(Vmmem { pid: 999, rss_mo: 1024 });
    let p = planifier(&e, &m2, &config(), H + 5);
    assert!(p.etat.tickets.iter().all(|t| t.cote == Cote::Windows));
}

#[test]
fn pression_ram_windows_gele_et_s_explique() {
    let e = etat(vec![ticket(1, Cote::Windows, 1.0, 1, 5)]);
    let p = planifier(&e, &mesures(3.0, 8.0, 4.0), &config(), H);
    assert!(p.evenements.is_empty());
    assert_eq!(p.pression, vec![Reserve::RamWindows]);
    let r = &p.raisons[&1];
    assert_eq!(r.decision, TypeDecision::Gel);
    assert!(r.phrase.contains("pression : RAM Windows disponible 3 Go sous la marge de 4 Go"), "{}", r.phrase);
}

#[test]
fn pression_wsl_ne_gele_pas_une_tache_windows() {
    let e = etat(vec![ticket(1, Cote::Windows, 1.0, 1, 5)]);
    let p = planifier(&e, &mesures(20.0, 1.0, 4.0), &config(), H);
    assert_eq!(p.pression, vec![Reserve::RamWsl]);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(1)]);
}

#[test]
fn place_prise_hors_file_et_suggestion() {
    // Aucun actif : seule la mémoire occupée ailleurs bloque ; 3 Go − 1,5 Go de marge = 1,5 Go libres.
    let e = etat(vec![ticket(1, Cote::Wsl, 2.0, 2, 10)]);
    let p = planifier(&e, &mesures(20.0, 3.0, 4.0), &config(), H);
    let r = &p.raisons[&1];
    assert_eq!(r.debloque_vers, None);
    assert!(r.phrase.contains("la place est prise hors file"), "{}", r.phrase);
    assert!(r.phrase.contains("--ram 1.5G ou moins"), "{}", r.phrase);
}

#[test]
fn la_cle_ignore_les_valeurs_mesurees() {
    let e = etat(vec![actif(ticket(1, Cote::Wsl, 5.0, 2, 30), H, 8.0), ticket(2, Cote::Wsl, 3.0, 2, 10)]);
    let a = planifier(&e, &mesures(20.0, 8.0, 4.0), &config(), H);
    let b = planifier(&e, &mesures(19.0, 7.5, 4.2), &config(), H + 2);
    assert_ne!(a.raisons[&2].phrase, b.raisons[&2].phrase);
    assert_eq!(a.raisons[&2].cle(), b.raisons[&2].cle());
}

#[test]
fn rien_a_persister_sans_decision() {
    let e = etat(vec![actif(ticket(1, Cote::Wsl, 2.0, 2, 30), H, 8.0)]);
    let p = planifier(&e, &large(), &config(), H + 10);
    assert!(!p.persister);
}

/// Essai de bascule du 05/11 : refusé à tort avant la correction du débit Windows.
#[test]
fn cas_reel_du_05_11_ticket_wsl_de_3_go() {
    let c = Config::default();
    let m = mesures(4.7, 6.0, 10.8);
    let e = etat(vec![ticket(1, Cote::Wsl, 3.0, 2, 5)]);
    let p = planifier(&e, &m, &c, H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(1)], "{}", p.raisons[&1].phrase);
    // La VM ne peut plus grandir que de 1,2 Go : c'est tout ce que le ticket réserve sur la RAM Windows.
    let niv = niveaux(&p.etat, &m, &c, H);
    assert_eq!(niv[Reserve::RamWindows.i()].reserve, 1229);
    assert_eq!(niv[Reserve::RamWsl.i()].reserve, 3072);
    // Un ticket Windows de même taille débite tout son besoin : 4,7 − 1,2 − 1,5 = 2 Go libres, il attend.
    let e = etat(vec![actif(ticket(1, Cote::Wsl, 3.0, 2, 5), H, 6.0), ticket(2, Cote::Windows, 3.0, 2, 5)]);
    let p = planifier(&e, &m, &c, H);
    assert!(p.evenements.is_empty());
    assert_eq!(p.raisons[&2].reserve.as_deref(), Some("ram_windows"));
}

#[test]
fn debit_windows_d_un_ticket_wsl() {
    let c = Config::default();
    let b = Besoin { ram_mo: 3072, cpu: 2, duree_s: 60 };
    assert_eq!(debit(Reserve::RamWindows, Cote::Wsl, &b, &mesures(20.0, 8.0, 4.0), &c), 3072);
    assert_eq!(debit(Reserve::RamWindows, Cote::Wsl, &b, &mesures(20.0, 8.0, 11.0), &c), 1024);
    assert_eq!(debit(Reserve::RamWindows, Cote::Wsl, &b, &mesures(20.0, 8.0, 12.5), &c), 0);
    assert_eq!(debit(Reserve::RamWindows, Cote::Windows, &b, &mesures(20.0, 8.0, 12.5), &c), 3072);
    let mut arretee = mesures(20.0, 8.0, 4.0);
    arretee.vmmem = None;
    assert_eq!(debit(Reserve::RamWindows, Cote::Wsl, &b, &arretee, &c), 3072);
}

#[test]
fn pas_de_pression_wsl_sans_mesure_recente() {
    let c = Config::default();
    // vmmemWSL à 10,9 Go : le repli donne 1,1 Go, sous la marge, mais ce n'est que du cache supposé.
    let m = mesures(20.0, 6.0, 10.9);
    assert!(planifier(&etat(vec![]), &m, &c, H + FRAICHEUR_WSL_S + 1).pression.is_empty());
    assert_eq!(planifier(&etat(vec![]), &frais(mesures(20.0, 1.0, 10.9), H), &c, H).pression, vec![Reserve::RamWsl]);
}

#[test]
fn demande_cpu_exclusive_passe_seule() {
    let c = config();
    let e = etat(vec![actif(ticket(1, Cote::Wsl, 1.0, 1, 10), H, 8.0), ticket(2, Cote::Wsl, 3.0, 10, 30)]);
    let p = planifier(&e, &large(), &c, H + 5);
    assert!(p.evenements.is_empty());
    let r = &p.raisons[&2];
    assert_eq!(r.reserve.as_deref(), Some("exclusif"));
    assert_eq!(r.bloquants, vec![1]);
    assert_eq!(r.debloque_vers, Some(H + 600));
    assert!(r.phrase.contains("demande exclusive"), "{}", r.phrase);

    // Un actif Windows ne bloque pas l'exclusivité WSL ; ensuite, l'exclusif bloque les autres tickets WSL seulement.
    let e = etat(vec![
        actif(ticket(1, Cote::Windows, 1.0, 0, 10), H, 20.0),
        ticket(2, Cote::Wsl, 3.0, 10, 30),
        ticket(3, Cote::Wsl, 1.0, 1, 5),
    ]);
    let p = planifier(&e, &large(), &c, H + 5);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(2)]);
    assert_eq!(p.raisons[&3].reserve.as_deref(), Some("cpu"));
}

/// Surveillance du 05/11 : le #13 (Windows, 4 cœurs) attendait « CPU insuffisant » derrière le #12 (WSL, 10 cœurs).
#[test]
fn budget_cpu_windows_separe() {
    let c = config();
    let e = etat(vec![
        actif(ticket(12, Cote::Wsl, 3.0, 10, 30), H, 8.0),
        ticket(13, Cote::Windows, 2.0, 4, 10),
        ticket(14, Cote::Windows, 1.0, 1, 10),
    ]);
    let p = planifier(&e, &large(), &c, H + 5);
    // Windows : 16 − 10 (vCPU du #12) − 2 = 4 libres, juste assez pour le #13 ; le #14 attend ensuite.
    assert_eq!(p.evenements, vec![Evenement::FeuVert(13)]);
    assert_eq!(p.raisons[&14].reserve.as_deref(), Some("cpu_windows"));
    let niv = niveaux(&p.etat, &large(), &c, H + 5);
    assert_eq!(niv[Reserve::Cpu.i()].reserve, 10, "le budget WSL ne voit que le #12");
    assert_eq!(niv[Reserve::CpuWindows.i()].reserve, 14);

    // `cpu.total_windows` remplace la mesure.
    let c8 = Config { cpu_total_windows: Some(8), ..config() };
    let p = planifier(&e, &large(), &c8, H + 5);
    assert!(p.evenements.is_empty());
    assert_eq!(p.raisons[&13].reserve.as_deref(), Some("cpu_windows"));
}

/// Soirée du 04/11 (journal du prototype). Besoins, durées et mesures SUPPOSÉS d'après les descriptions :
/// le journal ne donne ni les pics ni les réserves, il ne sert pas d'oracle.
#[test]
fn soiree_du_04_11() {
    let c = config();
    // Supposé : 14 Go libres sous Windows, vmmemWSL à 6 Go, MemAvailable 6 Go.
    let m = mesures(14.0, 6.0, 6.0);
    let t23_35 = H - 11 * 60;
    let t23_46 = H;
    let t23_56 = H + 10 * 60;
    let t00_00 = H + 14 * 60;

    // #3 (Infection ciblée, 1 Go, 1 cœur) occupe WSL ; #4 (back.sh puis Infection --threads=max, 10 cœurs)
    // arrive à 23:35 : admis, mais seulement quand il est seul côté WSL.
    assert!(refus(Cote::Wsl, &Besoin { ram_mo: 3072, cpu: 10, duree_s: 45 * 60 }, &m, &c).is_none());
    let t3 = actif(ticket(3, Cote::Wsl, 1.0, 1, 15), t23_35 - 2 * 60, 6.0);
    let mut t4 = ticket(4, Cote::Wsl, 3.0, 10, 45);
    t4.cree = t23_35;
    let p = planifier(&present(etat(vec![t3, t4.clone()]), t23_35), &m, &c, t23_35);
    assert!(p.evenements.is_empty());
    assert_eq!(p.raisons[&4].reserve.as_deref(), Some("exclusif"));
    // Fin du #3 à 23:46 : le #4 passe seul.
    let mut e = p.etat;
    e.tickets.retain(|t| t.id != 3);
    let p = planifier(&present(e, t23_46), &m, &c, t23_46);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(4)]);

    // Pendant le #4 (10 cœurs réservés), #5 (Infection ciblée) attend le CPU.
    let mut e = p.etat;
    e.ticket_mut(4).unwrap().recu = true;
    e.tickets.push(ticket(5, Cote::Wsl, 1.0, 1, 5));
    let p = planifier(&present(e, t23_56), &m, &c, t23_56);
    assert!(p.evenements.is_empty());
    assert_eq!(p.raisons[&5].reserve.as_deref(), Some("cpu"));
    assert_eq!(p.raisons[&5].debloque_vers, Some(t23_46 + 45 * 60));

    // #6 (Godot, 3 Go WSL) reste derrière ; #7 (Blender, Windows) passe devant le #5 comme le prototype l'avait fait à la main.
    let mut e = p.etat;
    e.tickets.push(ticket(6, Cote::Wsl, 3.0, 2, 15));
    e.tickets.push(ticket(7, Cote::Windows, 3.0, 2, 6));
    let p = planifier(&present(e, t00_00), &m, &c, t00_00);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(7)]);
    assert!(p.raisons[&7].phrase.contains("passé devant #5 : n'utilise pas le CPU WSL qui lui manque"), "{}", p.raisons[&7].phrase);
    assert_eq!(p.raisons[&6].position, Some(2));
    assert!(p.raisons[&6].phrase.contains("derrière #5"));

    // Fin du #4 : #5 et #6 démarrent au même passage.
    let mut e = p.etat;
    e.tickets.retain(|t| t.id != 4);
    let p = planifier(&present(e, t00_00 + 60), &m, &c, t00_00 + 60);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(5), Evenement::FeuVert(6)]);

    // #23 (même demande que le #4, 05/11 01:28) : seul côté WSL, il passe aussitôt.
    let mut t23 = ticket(23, Cote::Wsl, 3.0, 10, 45);
    t23.cree = H;
    let p = planifier(&etat(vec![actif(ticket(22, Cote::Windows, 5.0, 4, 20), H, 14.0), t23]), &m, &c, H + 1);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(23)]);
}

#[test]
fn affichage_du_libre_jamais_negatif() {
    let c = config();
    let e = etat(vec![actif(ticket(18, Cote::Wsl, 3.0, 10, 30), H, 8.0)]);
    let vues = vue_reserves(&e, &large(), &c, H);
    let cpu = vues.iter().find(|v| v.cle == "cpu").unwrap();
    assert_eq!(cpu.libre, -2, "le calcul reste exact");
    assert_eq!(cpu.exclusif, Some(18));
    assert_eq!(cpu.libre_lisible(), "0 cœur (exclusif #18)");
    let win = vues.iter().find(|v| v.cle == "cpu_windows").unwrap();
    assert_eq!((win.exclusif, win.libre_lisible()), (None, "4 cœurs".to_owned()));
    // RAM réservée au-delà du disponible : affichée à 0, sans mention d'exclusif.
    let e = etat(vec![actif(ticket(1, Cote::Windows, 18.0, 1, 30), H, 20.0)]);
    let ram = vue_reserves(&e, &large(), &c, H).into_iter().find(|v| v.cle == "ram_windows").unwrap();
    assert!(ram.libre < 0);
    assert_eq!(ram.libre_lisible(), "0 Go");
}

/// Swap à 91 % : pression seulement s'il sort vraiment ou si la mémoire manque, avec hystérésis.
#[test]
fn swap_plein_mais_calme_ne_gele_pas() {
    let c = Config::default();
    let e = etat(vec![ticket(1, Cote::Wsl, 1.0, 1, 5)]);
    let swap = |dispo_go: f64, libre_mo: u64, sortie: f32, t: u64| {
        let mut m = frais(mesures(20.0, dispo_go, 6.0), t);
        m.wsl.as_mut().unwrap().swap_libre_mo = libre_mo;
        m.swap_sortie_ps = sortie;
        m
    };
    // Cas réel du 05/11 à 23:30 : swap plein, quasi aucune sortie, MemAvailable 5,2 Go : ni pression ni gel, une information.
    let calme = swap(5.2, 377, 2.0, H);
    let p = planifier(&e, &calme, &c, H);
    assert!(p.pression.is_empty());
    assert_eq!(p.evenements, vec![Evenement::FeuVert(1)]);
    assert_eq!(swap_calme(&p.etat, &calme, &c, H).as_deref(), Some("swap WSL plein à 90 % (calme)"));

    // Swap plein et sortie à 300 pages/s : pression, gel des tickets WSL, raison explicite.
    let p = planifier(&e, &swap(5.2, 377, 300.0, H), &c, H);
    assert_eq!(p.pression, vec![Reserve::RamWsl]);
    assert!(p.raisons[&1].phrase.contains("swap WSL plein à 90 % et actif (sortie 300 pages/s"), "{}", p.raisons[&1].phrase);
    // Calme ensuite : la pression tient 59 s puis se lève à 60 s.
    let mut e2 = p.etat;
    for k in 0..=59 {
        e2 = planifier(&e2, &swap(5.2, 377, 0.0, H + 1 + k), &c, H + 1 + k).etat;
        assert!(e2.pression_swap, "levée trop tôt après {} s de calme", k);
    }
    assert!(!planifier(&e2, &swap(5.2, 377, 0.0, H + 61), &c, H + 61).etat.pression_swap);
    // Sous 85 % : levée immédiate.
    let p = planifier(&e, &swap(5.2, 377, 300.0, H), &c, H);
    assert!(!planifier(&p.etat, &swap(5.2, 700, 300.0, H + 2), &c, H + 2).etat.pression_swap);
    // Swap plein et MemAvailable sous marge + 1 Go (2,5 Go) : pression même sans sortie.
    assert_eq!(planifier(&e, &swap(2.2, 377, 0.0, H), &c, H).pression, vec![Reserve::RamWsl]);
}

#[test]
fn forcage_hors_file_reserves_et_gel() {
    let c = Config::default();
    let m = mesures(5.2, 3.0, 9.0);
    let mut e = etat(vec![
        actif(ticket(24, Cote::Wsl, 4.0, 6, 60), H, 6.0),
        ticket(26, Cote::Wsl, 3.0, 4, 15),
        ticket(28, Cote::Windows, 4.0, 4, 5),
        ticket(29, Cote::Windows, 1.0, 1, 5),
    ]);
    e.gel_manuel = Some(H);
    e.ticket_mut(26).unwrap().doublages = 1;
    let apercu = apercu_forcage(&e, 28, &m, &c, H).unwrap();
    assert!(apercu.starts_with("Forcer #28 (windows, 4 Go, 4 cœurs) ?"), "{apercu}");
    // RAM Windows : 5,2 − 3 (croissance possible de la VM pour le #24) − 1,5 = 0,7 Go libre, − 4 Go = −3,3 Go.
    assert!(apercu.contains("RAM Windows libre après : -3,3 Go (marge 1,5 Go)"), "{apercu}");
    assert!(apercu.contains("CPU Windows libre après : 4 cœurs (marge 2 cœurs)"), "{apercu}");
    assert!(!apercu.contains("RAM WSL"), "un ticket Windows ne débite pas WSL : {apercu}");

    let (forcee, journal) = forcer(&e, 28, &m, &c, H).unwrap();
    assert!(journal.contains("#28 (windows, 4 Go, 4 cœurs) · réserves au moment du forçage"), "{journal}");
    let t = forcee.ticket(28).unwrap();
    assert_eq!((t.etat, t.force), (EtatTicket::Actif, true));
    assert_eq!(forcee.ticket(26).unwrap().doublages, 1, "un forçage ne compte pas comme un dépassement");
    // Il compte dans les réservations ; les autres gardent leur place et le gel reste en vigueur pour eux.
    let p = planifier(&forcee, &m, &c, H);
    assert!(p.evenements.is_empty());
    assert_eq!(niveaux(&p.etat, &m, &c, H)[Reserve::RamWindows.i()].reserve, 3072 + 4096);
    assert!(p.raisons[&28].phrase.contains("actif (forcé par Adel)"), "{}", p.raisons[&28].phrase);
    assert_eq!(p.raisons[&26].position, Some(1));
    assert_eq!(p.raisons[&29].position, Some(2));
    // Rien à forcer sur un ticket actif ou inconnu.
    assert!(forcer(&forcee, 28, &m, &c, H).is_err());
    assert!(apercu_forcage(&e, 99, &m, &c, H).is_none());
}

/// Bug de surveillance n°11 (05/11, 23:58) : #43 (Windows, 4 Go) attendait derrière la réservation du #42 (WSL),
/// dont la consommation était pourtant déjà dans les mesures.
#[test]
fn reservation_limitee_a_la_montee_en_charge() {
    let c = Config::default();
    let accorde = H - 10 * 60;
    let mut t42 = actif(ticket(42, Cote::Wsl, 4.0, 10, 60), accorde, 7.0);
    t42.base_vmmem_mo = Some(7 * 1024);
    t42.base_ram_wsl_mo = Some(7 * 1024);
    t42.base_ram_windows_mo = Some(10 * 1024);
    // Depuis son feu vert, vmmemWSL est passé de 7 à 10,1 Go et MemAvailable de 7 à 3 Go : il a pris ses 4 Go.
    let m = frais(mesures(6.4, 3.0, 10.1), H);
    let e = present(etat(vec![t42.clone(), ticket(43, Cote::Windows, 4.0, 4, 10)]), H);
    let p = planifier(&e, &m, &c, H);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(43)], "{}", p.raisons[&43].phrase);
    assert_eq!(reservation(&t42, Reserve::RamWindows, &m, &c, H), 0);
    assert_eq!(reservation(&t42, Reserve::RamWsl, &m, &c, H), 0);
    assert_eq!(reservation(&t42, Reserve::Cpu, &m, &c, H), 10, "le CPU n'est pas mesuré : réservation entière");
    assert!(
        p.raisons[&42].phrase.contains("Montée en charge terminée, réservé encore : RAM Windows 0 Go, RAM WSL 0 Go"),
        "{}",
        p.raisons[&42].phrase
    );

    // Pendant les 3 premières minutes : réservation entière (débit Windows 1,9 Go = 12 − 10,1), #43 attend.
    let mut jeune = t42.clone();
    jeune.accorde = Some(H - 60);
    let e = present(etat(vec![jeune.clone(), ticket(43, Cote::Windows, 4.0, 4, 10)]), H);
    let p = planifier(&e, &m, &c, H);
    assert!(p.evenements.is_empty());
    assert!(p.raisons[&43].phrase.contains("dont 1,9 Go réservés pour #42"), "{}", p.raisons[&43].phrase);
    assert!(p.raisons[&42].phrase.contains("Montée en charge jusqu'à"), "{}", p.raisons[&42].phrase);

    // Montée partielle : 1,5 Go pris sur 4 Go déclarés → 2,5 Go encore réservés côté WSL.
    let m2 = frais(mesures(9.0, 5.5, 8.5), H);
    assert_eq!(reservation(&t42, Reserve::RamWsl, &m2, &c, H), 2560);

    // Ticket accordé par un ancien exe (sans relevés) : repli sur la RAM disponible notée à l'accord.
    let mut ancien = t42.clone();
    ancien.base_vmmem_mo = None;
    ancien.base_ram_wsl_mo = None;
    ancien.base_ram_windows_mo = None;
    ancien.dispo_accord_mo = Some(7 * 1024);
    assert_eq!(reservation(&ancien, Reserve::RamWsl, &m, &c, H), 0);
    assert_eq!(reservation(&ancien, Reserve::RamWindows, &m, &c, H), 0);

    // Un orphelin garde sa réservation entière.
    let mut orphelin = t42.clone();
    orphelin.etat = EtatTicket::Orphelin;
    assert_eq!(reservation(&orphelin, Reserve::RamWsl, &m, &c, H), 4096);
}

#[test]
fn repli_ram_wsl_affiche_comme_estimation() {
    let e = etat(Vec::new());
    let c = config();
    let ram_wsl = |m: &Mesures, quand: u64| vue_reserves(&e, m, &c, quand).into_iter().find(|r| r.cle == "ram_wsl").unwrap();

    let frais = ram_wsl(&large(), H + 10);
    assert_eq!(frais.estimation, None);
    assert!(!frais.libre_lisible().contains("estimation"), "{}", frais.libre_lisible());

    // Mesure WSL périmée : repli 12 Go − vmmemWSL, cache compris, jamais présenté comme un libre mesuré.
    let mut plein = large();
    plein.vmmem = Some(Vmmem { pid: 4242, rss_mo: 12 * 1024 });
    let r = ram_wsl(&plein, H + 120);
    assert_eq!(r.estimation, Some(H));
    let texte = r.libre_lisible();
    assert!(texte.starts_with("estimation 0 Go, cache compris"), "{texte}");
    assert!(texte.contains(&format!("pas de mesure WSL depuis {}", hhmm(H))), "{texte}");

    plein.wsl = None;
    assert!(ram_wsl(&plein, H).libre_lisible().ends_with("aucune mesure WSL"));
    plein.vmmem = None;
    assert_eq!(ram_wsl(&plein, H).estimation, None, "WSL arrêté : pas d'estimation");
}

#[test]
fn liaison_perdue_reprise_ou_liberation() {
    let c = config();
    let mut actif_lie = actif(ticket(1, Cote::Wsl, 2.0, 2, 30), H, 8.0);
    actif_lie.manuel = true;
    actif_lie.liaison = Some(5);
    actif_lie.perdu = Some(H + 100);
    let mut attente_lie = ticket(2, Cote::Wsl, 1.0, 1, 10);
    attente_lie.liaison = Some(5);
    attente_lie.perdu = Some(H + 100);
    let mut autre = ticket(3, Cote::Wsl, 1.0, 1, 10);
    autre.presence = H + 110;
    let e = etat(vec![actif_lie, attente_lie, autre]);

    // Pendant delai.reprise : l'actif devient orphelin « connexion » sans geler, l'attente est sautée, les autres passent.
    let p = planifier(&e, &large(), &c, H + 110);
    assert_eq!(p.evenements, vec![Evenement::FeuVert(3)]);
    let t = p.etat.ticket(1).unwrap();
    assert_eq!((t.etat, t.cause_orphelin), (EtatTicket::Orphelin, Some(CauseOrphelin::Connexion)));
    assert!(p.gel.is_empty(), "{:?}", p.gel);
    assert!(niveaux(&p.etat, &large(), &c, H + 110)[Reserve::RamWsl.i()].par.contains(&1), "réservation gardée");
    let phrase = &p.raisons[&1].phrase;
    assert!(phrase.contains("liaison de sa session perdue") && phrase.contains("sans geler"), "{phrase}");
    assert!(phrase.contains(&format!("libéré à {}", hhmm(H + 220))), "{phrase}");
    let phrase = &p.raisons[&2].phrase;
    assert_eq!(p.raisons[&2].reserve.as_deref(), Some("absent"));
    assert!(phrase.contains(&format!("place gardée jusqu'à {}", hhmm(H + 220))), "{phrase}");

    // Au-delà de delai.reprise sans nouvelle liaison : tous deux sortent de la file.
    let p = planifier(&p.etat, &large(), &c, H + 220);
    assert_eq!(
        p.evenements,
        vec![Evenement::Libere(1, Liberation::SessionFermee), Evenement::Libere(2, Liberation::SessionFermee)]
    );
    assert!(p.etat.ticket(1).is_none() && p.etat.ticket(2).is_none() && p.etat.ticket(3).is_some());
}
