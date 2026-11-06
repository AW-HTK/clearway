use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use commun::protocole::{RELAIS_ABSENT, chemin_socket};
use serde_json::{Value, json};

const CONFIG_RAPIDE: &str = "marge.ram_windows = \"4G\"\n\
    delai.actif = \"3s\"\ndelai.attente = \"4s\"\ndelai.absence = \"8s\"\ndelai.rappel = \"3s\"\ndelai.long_poll = \"2s\"\n\
    delai.reprise = \"3s\"\n";

struct Orchestre {
    enfant: Option<Child>,
    dossier: PathBuf,
    tube: String,
}

fn attendre<F: FnMut() -> bool>(max: Duration, mut condition: F) -> bool {
    let fin = Instant::now() + max;
    while Instant::now() < fin {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

impl Orchestre {
    fn preparer(nom: &str) -> Orchestre {
        let dossier = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(nom);
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        let tube = format!("essai-{nom}-{}", std::process::id());
        std::fs::write(dossier.join("config.toml"), format!("tube = \"{tube}\"\n{CONFIG_RAPIDE}")).unwrap();
        Orchestre { enfant: None, dossier, tube }
    }

    fn demarrer(nom: &str) -> Orchestre {
        let mut o = Orchestre::preparer(nom);
        o.lancer();
        o
    }

    fn processus(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_orchestrateur"));
        c.arg("--dossier").arg(&self.dossier).arg("--test").stdout(Stdio::null()).stderr(Stdio::null());
        c
    }

    fn repond(&self) -> bool {
        UnixStream::connect(chemin_socket(&self.tube)).is_ok()
    }

    fn lancer(&mut self) {
        self.enfant = Some(self.processus().spawn().unwrap());
        assert!(attendre(Duration::from_secs(10), || self.repond()), "l'orchestrateur ne répond pas sur {}", self.tube);
    }

    fn tuer(&mut self) {
        if let Some(mut e) = self.enfant.take() {
            let _ = e.kill();
            let _ = e.wait();
        }
    }

    fn appel(&self, methode: &str, chemin: &str, corps: Value) -> (u16, Value) {
        self.appel_avec(methode, chemin, json!({}), corps)
    }

    fn appel_avec(&self, methode: &str, chemin: &str, parametres: Value, corps: Value) -> (u16, Value) {
        let mut flux = UnixStream::connect(chemin_socket(&self.tube)).unwrap();
        let req = json!({ "methode": methode, "chemin": chemin, "parametres": parametres, "corps": corps });
        flux.write_all(format!("{req}\n").as_bytes()).unwrap();
        let mut ligne = String::new();
        BufReader::new(&flux).read_line(&mut ligne).unwrap();
        let rep: Value = serde_json::from_str(&ligne).unwrap();
        (rep["code"].as_u64().unwrap() as u16, rep["corps"].clone())
    }

    fn creer(&self, requete: &str, cote: &str, ram_mo: u64) -> (u16, Value) {
        self.appel(
            "POST",
            "/tickets",
            json!({
                "requete_id": requete, "cote": cote, "ram_mo": ram_mo, "cpu": 1, "duree_s": 600,
                "description": format!("essai {requete}"), "session": "test", "claude_session_id": null,
                "claude_pid": null, "dossier": "/", "signature": "", "mesures": null
            }),
        )
    }

    fn attente_brute(&self, id: u64, accuse: bool) -> (u16, Value) {
        let mut parametres = json!({ "connu": "", "delai_s": "2" });
        if accuse {
            parametres["accuse"] = json!("1");
        }
        self.appel_avec("GET", &format!("/tickets/{id}/attente"), parametres, Value::Null)
    }

    fn etat(&self) -> Value {
        self.appel("GET", "/etat", Value::Null).1
    }

    fn ticket(&self, id: u64) -> Option<Value> {
        self.etat()["tickets"].as_array().unwrap().iter().find(|t| t["id"] == id).cloned()
    }

    fn etat_ticket(&self, id: u64) -> Option<String> {
        self.ticket(id).map(|t| t["etat"].as_str().unwrap().to_owned())
    }

    fn charge(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_charge"));
        c.args(args)
            .env("CHARGE_EXE", env!("CARGO_BIN_EXE_orchestrateur"))
            .env("CHARGE_TUBE", &self.tube)
            .env("CHARGE_DOSSIER", &self.dossier)
            .env("CHARGE_ARGS_LANCEMENT", "--test")
            .env("CHARGE_SESSION", "test")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_PID");
        c
    }

    fn sortie(&self, args: &[&str]) -> Output {
        self.charge(args).output().unwrap()
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.dossier.join("journal.log")).unwrap_or_default()
    }
}

/// Connexion gardée ouverte, comme celle d'un processus MCP : plusieurs requêtes, éventuellement en parallèle.
struct Ligne {
    flux: UnixStream,
    lecteur: BufReader<UnixStream>,
}

impl Ligne {
    fn ouvrir(o: &Orchestre) -> Ligne {
        let flux = UnixStream::connect(chemin_socket(&o.tube)).unwrap();
        let lecteur = BufReader::new(flux.try_clone().unwrap());
        Ligne { flux, lecteur }
    }

    fn envoyer(&mut self, requete: Value) {
        self.flux.write_all(format!("{requete}\n").as_bytes()).unwrap();
    }

    fn lire(&mut self) -> Value {
        let mut ligne = String::new();
        self.lecteur.read_line(&mut ligne).unwrap();
        serde_json::from_str(&ligne).unwrap()
    }

    fn appel(&mut self, methode: &str, chemin: &str, corps: Value) -> Value {
        self.envoyer(json!({ "methode": methode, "chemin": chemin, "corps": corps }));
        self.lire()
    }

    fn lier(&mut self, identite: &str) -> Value {
        self.appel("POST", "/liaison", json!({ "identite": identite, "session": "essai MCP", "client": "test" }))
    }

    fn creer(&mut self, requete: &str, ram_mo: u64) -> Value {
        self.appel(
            "POST",
            "/tickets",
            json!({
                "requete_id": requete, "cote": "windows", "ram_mo": ram_mo, "cpu": 1, "duree_s": 600, "manuel": true,
                "description": format!("essai {requete}"), "session": "essai MCP", "claude_session_id": null,
                "claude_pid": null, "dossier": "/", "signature": "", "mesures": null
            }),
        )
    }
}

impl Drop for Orchestre {
    fn drop(&mut self) {
        if self.enfant.is_some() && self.repond() {
            let _ = self.appel("POST", "/quitter", Value::Null);
            if let Some(e) = self.enfant.as_mut() {
                let _ = attendre(Duration::from_secs(3), || matches!(e.try_wait(), Ok(Some(_))));
            }
        }
        self.tuer();
    }
}

fn texte(o: &[u8]) -> String {
    String::from_utf8_lossy(o).into_owned()
}

#[test]
fn demande_attente_fin_et_statut() {
    let o = Orchestre::demarrer("cycle");
    let a = o.sortie(&["demande", "--cote", "windows", "--ram", "10G", "tâche A"]);
    assert_eq!(a.status.code(), Some(0), "{}", texte(&a.stderr));
    assert_eq!(texte(&a.stdout).trim(), "1");
    assert!(texte(&a.stderr).contains("feu vert"), "{}", texte(&a.stderr));

    let b = o.sortie(&["demande", "--cote", "windows", "--ram", "8G", "session B", "tâche B"]);
    assert_eq!(texte(&b.stdout).trim(), "2");
    assert!(texte(&b.stderr).contains("RAM Windows insuffisante"), "{}", texte(&b.stderr));
    assert!(texte(&b.stderr).contains("Débloqué à la fin du #1"), "{}", texte(&b.stderr));

    let attente = o.charge(&["attend", "2"]).stdout(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let statut = o.sortie(&["statut"]);
    let s = texte(&statut.stdout);
    assert!(s.contains("#1") && s.contains("actif") && s.contains("(manuel)") && s.contains("#2"), "{s}");

    let f = o.sortie(&["fin", "1"]);
    assert_eq!(f.status.code(), Some(0));
    assert!(texte(&f.stdout).contains("terminé"), "{}", texte(&f.stdout));
    let sortie = attente.wait_with_output().unwrap();
    assert_eq!(sortie.status.code(), Some(0));
    let lignes = texte(&sortie.stdout);
    assert!(lignes.contains("position 1"), "{lignes}");
    assert!(lignes.contains("FEU VERT pour le ticket #2"), "{lignes}");

    assert_eq!(o.sortie(&["annule", "2"]).status.code(), Some(0));
    assert_eq!(o.sortie(&["fin", "2"]).status.code(), Some(1));
    let j = o.journal();
    for attendu in ["DÉMARRAGE époque 1", "DEMANDE #1", "FEU VERT #1", "ATTENTE #2", "FIN #1", "FEU VERT #2", "ANNULÉ #2"] {
        assert!(j.contains(attendu), "journal sans « {attendu} » :\n{j}");
    }
}

#[test]
fn refus_immediat_et_cpu_exclusif() {
    let o = Orchestre::demarrer("refus");
    let r = o.sortie(&["demande", "--cote", "windows", "--ram", "40G", "trop gros"]);
    assert_eq!(r.status.code(), Some(3));
    assert!(texte(&r.stdout).is_empty());
    assert!(texte(&r.stderr).contains("RAM Windows impossible"), "{}", texte(&r.stderr));
    let r = o.sortie(&["run", "--cpu", "11", "trop de cœurs", "--", "true"]);
    assert_eq!(r.status.code(), Some(3));
    assert!(texte(&r.stderr).contains("--cpu à 10 max"), "{}", texte(&r.stderr));
    let r = o.sortie(&["run", "--cpu", "10", "--ram", "1G", "exclusif", "--", "true"]);
    assert_eq!(r.status.code(), Some(0), "{}", texte(&r.stderr));
    assert!(o.etat()["tickets"].as_array().unwrap().is_empty());
}

#[test]
fn run_lance_la_commande_et_rend_son_code() {
    let o = Orchestre::demarrer("run");
    let r = o.sortie(&["run", "--cote", "windows", "--ram", "1G", "écho", "--", "sh", "-c", "echo bonjour; exit 7"]);
    assert_eq!(r.status.code(), Some(7), "{}", texte(&r.stderr));
    assert_eq!(texte(&r.stdout).trim(), "bonjour");
    assert!(texte(&r.stderr).contains("feu vert #1"), "{}", texte(&r.stderr));
    assert!(o.etat()["tickets"].as_array().unwrap().is_empty());
    assert!(o.journal().contains("code 7"), "{}", o.journal());
}

#[test]
fn run_attend_son_tour() {
    let o = Orchestre::demarrer("run-attente");
    let (code, a) = o.creer("occupant", "windows", 12 * 1024);
    assert_eq!(code, 201);
    assert_eq!(a["etat"], "actif");
    let run = o
        .charge(&["run", "--cote", "windows", "--ram", "8G", "second", "--", "sh", "-c", "echo parti"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(o.etat_ticket(2).as_deref(), Some("attente"));
    o.appel("POST", "/tickets/1/fin", json!({}));
    let sortie = run.wait_with_output().unwrap();
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie.stderr));
    assert!(texte(&sortie.stderr).contains("en attente, position 1"), "{}", texte(&sortie.stderr));
    assert_eq!(texte(&sortie.stdout).trim(), "parti");
    assert!(o.journal().contains("ACCUSÉ #2"), "{}", o.journal());
}

#[test]
fn max_rend_75_et_la_meme_commande_reprend_son_ticket() {
    let o = Orchestre::demarrer("rattachement");
    o.creer("occupant", "windows", 12 * 1024);
    let args = ["run", "--cote", "windows", "--ram", "8G", "--max", "2s", "patient", "--", "echo", "enfin"];
    let r = o.sortie(&args);
    assert_eq!(r.status.code(), Some(75), "{}", texte(&r.stderr));
    assert!(texte(&r.stderr).contains("Relance la même commande"), "{}", texte(&r.stderr));
    assert_eq!(o.etat_ticket(2).as_deref(), Some("attente"));

    let relance = o.charge(&args).stderr(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(o.etat()["tickets"].as_array().unwrap().len(), 2, "pas de nouveau ticket");
    o.appel("POST", "/tickets/1/fin", json!({}));
    let sortie = relance.wait_with_output().unwrap();
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie.stderr));
    assert!(texte(&sortie.stderr).contains("ticket #2 repris"), "{}", texte(&sortie.stderr));
    assert_eq!(texte(&sortie.stdout).trim(), "enfin");

    // `attend --max` : code 75, puis la place est toujours là.
    o.creer("occupant-2", "windows", 12 * 1024);
    let d = o.sortie(&["demande", "--cote", "windows", "--ram", "8G", "manuel"]);
    let id = texte(&d.stdout).trim().to_owned();
    let a = o.sortie(&["attend", &id, "--max", "1s"]);
    assert_eq!(a.status.code(), Some(75));
    assert!(texte(&a.stdout).contains(&format!("Relance « charge attend {id} »")), "{}", texte(&a.stdout));
    assert_eq!(o.etat_ticket(id.parse().unwrap()).as_deref(), Some("attente"));
}

#[test]
fn feu_vert_non_remis_rappele_puis_recupere() {
    let o = Orchestre::demarrer("rappel");
    o.creer("occupant", "windows", 12 * 1024);
    let d = o.sortie(&["demande", "--cote", "windows", "--ram", "8G", "personne n'écoute"]);
    let id: u64 = texte(&d.stdout).trim().parse().unwrap();
    o.appel("POST", "/tickets/1/fin", json!({}));
    assert_eq!(o.etat_ticket(id).as_deref(), Some("actif"));
    assert!(attendre(Duration::from_secs(8), || o.etat_ticket(id).as_deref() == Some("attente")));
    assert!(o.journal().contains(&format!("RAPPEL #{id}")), "{}", o.journal());
    let phrase = o.ticket(id).unwrap()["phrase"].as_str().unwrap().to_owned();
    assert!(phrase.contains("feu vert rappelé"), "{phrase}");

    let a = o.sortie(&["attend", &id.to_string(), "--max", "10s"]);
    assert_eq!(a.status.code(), Some(0), "{}", texte(&a.stdout));
    assert!(texte(&a.stdout).contains("FEU VERT"));
    assert!(o.journal().contains(&format!("ACCUSÉ #{id}")), "{}", o.journal());
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(o.etat_ticket(id).as_deref(), Some("actif"), "remis : plus de rappel");
}

/// Feu vert accordé alors qu'aucun client n'écoute : il attend d'être réclamé.
fn feu_vert_en_suspens(o: &Orchestre) -> u64 {
    o.creer("occupant", "windows", 12 * 1024);
    let (_, t) = o.creer("suivant", "windows", 8 * 1024);
    assert_eq!(t["etat"], "attente", "{t}");
    o.appel("POST", "/tickets/1/fin", json!({}));
    let id = t["id"].as_u64().unwrap();
    assert_eq!(o.etat_ticket(id).as_deref(), Some("actif"));
    id
}

#[test]
fn accuse_du_feu_vert() {
    let reclamer = |nom: &str, accuse: bool| {
        let o = Orchestre::demarrer(nom);
        let id = feu_vert_en_suspens(&o);
        let (code, v) = o.attente_brute(id, accuse);
        assert_eq!((code, v["etat"].as_str()), (200, Some("actif")), "{v}");
        (o, id)
    };
    let (ancien, id_ancien) = reclamer("accuse-ancien", false);
    let (sans, id_sans) = reclamer("accuse-sans", true);
    let (avec, id_avec) = reclamer("accuse-avec", true);
    let ids = [id_ancien, id_sans, id_avec];
    let (code, r) = avec.appel("POST", &format!("/tickets/{}/recu", ids[2]), Value::Null);
    assert_eq!(code, 200, "{r}");
    assert!(avec.journal().contains(&format!("ACCUSÉ #{}", ids[2])), "{}", avec.journal());
    let (code, _) = avec.appel("POST", "/tickets/999/recu", Value::Null);
    assert_eq!(code, 404);

    // Feu vert envoyé, jamais accusé dans delai.rappel (3 s) : repris, place gardée.
    assert!(attendre(Duration::from_secs(8), || sans.etat_ticket(ids[1]).as_deref() == Some("attente")));
    assert!(sans.journal().contains(&format!("RAPPEL #{}", ids[1])), "{}", sans.journal());
    let (code, r) = sans.appel("POST", &format!("/tickets/{}/recu", ids[1]), Value::Null);
    assert_eq!(code, 409, "{r}");
    assert!(r["erreur"].as_str().unwrap().contains("charge attend"), "{r}");

    // L'ancien client (réponse = remise) et l'accusé gardent leur feu vert ; sans signal de présence, ils
    // finissent orphelins, ce qui suppose un feu vert remis.
    std::thread::sleep(Duration::from_secs(2));
    for (o, id) in [(&ancien, ids[0]), (&avec, ids[2])] {
        let etat = o.etat_ticket(id);
        assert!(matches!(etat.as_deref(), Some("actif" | "orphelin")), "{etat:?}");
        assert!(!o.journal().contains(&format!("RAPPEL #{id}")), "{}", o.journal());
    }
}

#[test]
fn bail_manuel_puis_prolongation() {
    let o = Orchestre::demarrer("bail");
    let d = o.sortie(&["demande", "--cote", "windows", "--ram", "1G", "--duree", "2s", "court"]);
    let id = texte(&d.stdout).trim().to_owned();
    let n: u64 = id.parse().unwrap();

    // Bail dépassé : toujours actif et réservé, la file continue.
    assert!(attendre(Duration::from_secs(8), || o.journal().contains(&format!("BAIL DÉPASSÉ #{id}"))), "{}", o.journal());
    let t = o.ticket(n).unwrap();
    assert_eq!((t["etat"].as_str(), &t["bail_depasse"]), (Some("actif"), &json!(true)), "{t}");
    assert!(t["phrase"].as_str().unwrap().contains(&format!("charge prolonge {id}")), "{t}");
    let (_, pendant) = o.creer("pendant", "windows", 1024);
    assert_eq!(pendant["etat"], "actif", "{pendant}");
    o.appel("POST", &format!("/tickets/{}/fin", pendant["id"]), json!({}));
    let s = texte(&o.sortie(&["statut"]).stdout);
    assert!(s.contains("(bail dépassé)"), "{s}");

    // Sans nouvelle pendant delai.absence (8 s) après l'échéance : orphelin, admissions gelées.
    assert!(attendre(Duration::from_secs(14), || o.etat_ticket(n).as_deref() == Some("orphelin")));
    assert!(o.journal().contains(&format!("BAIL ÉCHU #{id}")), "{}", o.journal());
    let (_, b) = o.creer("suivant", "windows", 1024);
    assert_eq!(b["raison"]["decision"], "gel");
    assert!(b["raison"]["phrase"].as_str().unwrap().contains("bail échu"), "{b}");

    let p = o.sortie(&["prolonge", &id, "10m", "tests plus longs que prévu"]);
    assert_eq!(p.status.code(), Some(0), "{}", texte(&p.stderr));
    assert!(texte(&p.stdout).contains("prolongé de 10 min"), "{}", texte(&p.stdout));
    assert_eq!(o.etat_ticket(id.parse().unwrap()).as_deref(), Some("actif"));
    assert!(o.journal().contains("tests plus longs que prévu"));
    let (_, gros) = o.creer("gros", "windows", 15 * 1024);
    assert_eq!(gros["etat"], "attente");
    let r = o.sortie(&["prolonge", &gros["id"].to_string(), "5m", "pas encore actif"]);
    assert_eq!(r.status.code(), Some(2));
    assert!(texte(&r.stderr).contains("rien à prolonger"), "{}", texte(&r.stderr));
}

#[test]
fn gel_manuel() {
    let o = Orchestre::demarrer("gel");
    let (code, g) = o.appel("POST", "/gel", json!({ "gele": true }));
    assert_eq!(code, 200, "{g}");
    let (_, a) = o.creer("a", "windows", 1024);
    assert_eq!(a["raison"]["decision"], "gel");
    assert!(a["raison"]["phrase"].as_str().unwrap().contains("gel manuel"), "{a}");
    assert!(texte(&o.sortie(&["statut"]).stdout).contains("File gelée à la main"));
    o.appel("POST", "/gel", json!({ "gele": false }));
    assert_eq!(o.etat_ticket(1).as_deref(), Some("actif"));
    assert!(o.journal().contains("GEL MANUEL") && o.journal().contains("DÉGEL MANUEL"));
}

#[test]
fn orphelin_gele_puis_revient_au_signal() {
    let o = Orchestre::demarrer("orphelin");
    let (_, a) = o.creer("a", "windows", 1024);
    assert_eq!(a["etat"], "actif");
    assert!(attendre(Duration::from_secs(8), || o.etat_ticket(1).as_deref() == Some("orphelin")));
    let (_, b) = o.creer("b", "windows", 1024);
    assert_eq!(b["etat"], "attente");
    assert_eq!(b["raison"]["decision"], "gel");
    assert!(b["raison"]["phrase"].as_str().unwrap().contains("ticket #1 orphelin"), "{b}");

    let (code, p) = o.appel("POST", "/tickets/1/presence", json!({}));
    assert_eq!(code, 200);
    assert_eq!(p["etat"], "actif");
    assert_eq!(o.etat_ticket(2).as_deref(), Some("actif"));
    assert!(o.journal().contains("ORPHELIN #1") && o.journal().contains("RETOUR #1"));
}

#[test]
fn superviseur_tue_pendant_la_tache() {
    let o = Orchestre::demarrer("superviseur");
    let mut run = o
        .charge(&["run", "--cote", "windows", "--ram", "1G", "long", "--", "sleep", "30"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(attendre(Duration::from_secs(5), || o.etat_ticket(1).as_deref() == Some("actif")));
    unsafe { libc::kill(run.id() as i32, libc::SIGKILL) };
    let _ = run.wait();
    assert!(attendre(Duration::from_secs(8), || o.etat_ticket(1).as_deref() == Some("orphelin")));
    let _ = Command::new("pkill").args(["-f", "^sleep 30$"]).status();
    assert_eq!(o.sortie(&["annule", "1"]).status.code(), Some(0));
}

#[test]
fn signal_au_superviseur_arrete_la_commande_et_declare_la_fin() {
    let o = Orchestre::demarrer("signal");
    let mut run = o
        .charge(&["run", "--cote", "windows", "--ram", "1G", "long", "--", "sleep", "40"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(attendre(Duration::from_secs(5), || o.etat_ticket(1).as_deref() == Some("actif")));
    unsafe { libc::kill(run.id() as i32, libc::SIGTERM) };
    let statut = run.wait().unwrap();
    assert_eq!(statut.code(), Some(128 + libc::SIGTERM));
    assert!(o.etat()["tickets"].as_array().unwrap().is_empty());
    assert!(o.journal().contains("FIN #1"), "{}", o.journal());
}

#[test]
fn attente_sans_client_sautee_puis_retiree() {
    let o = Orchestre::demarrer("expiration");
    o.creer("gros", "windows", 15 * 1024);
    let (_, b) = o.creer("petit", "windows", 4 * 1024);
    assert_eq!(b["etat"], "attente");
    assert!(attendre(Duration::from_secs(8), || {
        o.ticket(2).is_some_and(|t| t["phrase"].as_str().unwrap().contains("aucun client ne l'attend"))
    }));
    assert!(attendre(Duration::from_secs(10), || o.etat_ticket(2).is_none()));
    assert!(o.journal().contains("EXPIRÉ #2"));
}

#[test]
fn reprise_apres_arret_brutal_et_epoque() {
    let mut o = Orchestre::demarrer("reprise");
    let (_, a) = o.creer("a", "windows", 12 * 1024);
    assert_eq!(a["epoque"], 1);
    let run = o
        .charge(&["run", "--cote", "windows", "--ram", "8G", "après reprise", "--", "true"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(attendre(Duration::from_secs(5), || o.etat_ticket(2).as_deref() == Some("attente")));
    o.tuer();
    let sur_disque: Value = serde_json::from_slice(&std::fs::read(o.dossier.join("etat.json")).unwrap()).unwrap();
    assert_eq!(sur_disque["tickets"].as_array().unwrap().len(), 2);
    o.lancer();
    assert_eq!(o.etat()["epoque"], 2);
    assert_eq!(o.etat_ticket(1).as_deref(), Some("actif"));
    // Le client de l'époque 1 rouvre un relais et obtient le feu vert sur la nouvelle instance.
    o.appel("POST", "/tickets/1/fin", json!({"code": 0}));
    let sortie = run.wait_with_output().unwrap();
    assert_eq!(sortie.status.code(), Some(0), "{}", texte(&sortie.stderr));
    assert!(o.journal().contains("DÉMARRAGE époque 2"), "{}", o.journal());
}

#[test]
fn deuxieme_instance_et_arret_par_le_tube() {
    let mut o = Orchestre::demarrer("instances");
    let statut = o.processus().status().unwrap();
    assert_eq!(statut.code(), Some(3));
    assert!(o.journal().contains("INFO Déjà lancé"), "{}", o.journal());
    assert!(!o.journal().contains("ERREUR Déjà lancé"));
    let (code, _) = o.appel("POST", "/quitter", Value::Null);
    assert_eq!(code, 200);
    let mut e = o.enfant.take().unwrap();
    assert!(attendre(Duration::from_secs(5), || matches!(e.try_wait(), Ok(Some(s)) if s.success())));
    assert!(o.journal().contains("ARRÊT"));
    assert!(!o.repond());
}

#[test]
fn idempotence_de_la_creation() {
    let o = Orchestre::demarrer("idempotence");
    let (c1, a) = o.creer("meme-requete", "windows", 1024);
    let (c2, b) = o.creer("meme-requete", "windows", 1024);
    assert_eq!((c1, c2), (201, 200));
    assert_eq!(a["id"], b["id"]);
    let (_, c) = o.creer("autre-requete", "windows", 1024);
    assert_ne!(a["id"], c["id"]);
}

#[test]
fn arret_de_wsl_libere_les_tickets_wsl() {
    let o = Orchestre::demarrer("wsl-arrete");
    let (_, a) = o.creer("wsl", "wsl", 1024);
    assert_eq!(a["etat"], "actif");
    let (_, b) = o.creer("win", "windows", 1024);
    assert_eq!(b["etat"], "actif");
    std::fs::write(
        o.dossier.join("mesures-test.json"),
        r#"{"ram_totale_mo":32768,"ram_dispo_mo":20480,"commit_mo":0,"commit_limite_mo":0,"cpu_pct":0.0,"vmmem":null}"#,
    )
    .unwrap();
    assert!(attendre(Duration::from_secs(6), || o.etat_ticket(1).is_none()));
    assert!(o.etat_ticket(2).is_some());
    assert!(o.journal().contains("LIBÉRÉ #1"));
    let s = texte(&o.sortie(&["statut"]).stdout);
    assert!(s.contains("WSL : arrêté"), "{s}");
}

#[test]
fn relais_absent_puis_lancement_automatique() {
    let o = Orchestre::preparer("lancement");
    let r = Command::new(env!("CARGO_BIN_EXE_orchestrateur")).args(["relais", "--tube", &o.tube]).output().unwrap();
    assert_eq!(r.status.code(), Some(RELAIS_ABSENT));
    assert!(texte(&r.stderr).contains("orchestrateur absent"), "{}", texte(&r.stderr));

    let s = o.sortie(&["statut"]);
    assert_eq!(s.status.code(), Some(0), "{}", texte(&s.stderr));
    assert!(texte(&s.stderr).contains("orchestrateur absent, lancement"), "{}", texte(&s.stderr));
    assert!(texte(&s.stdout).contains("File vide"));
    let (code, _) = o.appel("POST", "/quitter", Value::Null);
    assert_eq!(code, 200);
    assert!(attendre(Duration::from_secs(5), || !o.repond()));
}

#[test]
fn orchestrateur_injoignable() {
    let o = Orchestre::preparer("injoignable");
    let debut = Instant::now();
    let r = o.charge(&["statut"]).env("CHARGE_EXE", "/bin/false").env("CHARGE_PATIENCE", "6").output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    assert!(debut.elapsed() >= Duration::from_secs(3));
    let e = texte(&r.stderr);
    assert!(e.contains("nouvel essai dans 1 s") && e.contains("nouvel essai dans 2 s"), "délai croissant : {e}");
    assert!(e.contains("orchestrateur injoignable depuis"), "{e}");
}

/// Imite `orchestrateur.exe` quand l'interop WSL cale : les `echecs` premiers lancements échouent comme le 05/11 à 21:45.
fn faux_exe(o: &Orchestre, echecs: u32) -> PathBuf {
    let chemin = o.dossier.join("faux-exe.sh");
    let compteur = o.dossier.join("lancements");
    let _ = std::fs::remove_file(&compteur);
    std::fs::write(
        &chemin,
        format!(
            "#!/bin/sh\nn=$(cat '{c}' 2>/dev/null || echo 0)\necho $((n + 1)) > '{c}'\nif [ \"$n\" -lt {echecs} ]; then\n  echo 'WSL ERROR: UtilAcceptVsock:281: accept4 failed 110' >&2\n  exit 1\nfi\nexec '{vrai}' \"$@\"\n",
            c = compteur.display(),
            vrai = env!("CARGO_BIN_EXE_orchestrateur")
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&chemin).status().unwrap();
    chemin
}

#[test]
fn interop_qui_cale_puis_revient() {
    let o = Orchestre::demarrer("interop");
    let exe = faux_exe(&o, 2);
    let r = o.charge(&["statut"]).env("CHARGE_EXE", &exe).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", texte(&r.stderr));
    let e = texte(&r.stderr);
    assert!(e.contains("interop WSL indisponible (WSL ERROR: UtilAcceptVsock"), "{e}");
    assert!(e.contains("nouvel essai dans 1 s") && e.contains("nouvel essai dans 2 s"), "{e}");

    // En attente, la coupure ne fait pas lâcher le ticket : le client se rattache et reçoit son feu vert.
    o.creer("occupant", "windows", 12 * 1024);
    let d = o.sortie(&["demande", "--cote", "windows", "--ram", "8G", "patient"]);
    let id = texte(&d.stdout).trim().to_owned();
    let exe = faux_exe(&o, 0);
    let attente = o.charge(&["attend", &id, "--max", "1m"]).env("CHARGE_EXE", &exe).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    // L'interop cale pendant que le client attend : son relais meurt, les 2 relances suivantes échouent.
    std::fs::write(o.dossier.join("lancements"), "-2").unwrap();
    let _ = Command::new("pkill").args(["-f", &format!("relais --tube {}", o.tube)]).status();
    std::thread::sleep(Duration::from_secs(1));
    o.appel("POST", "/tickets/1/fin", json!({}));
    let sortie = attente.wait_with_output().unwrap();
    assert_eq!(sortie.status.code(), Some(0), "{}\n{}", texte(&sortie.stdout), texte(&sortie.stderr));
    assert!(texte(&sortie.stdout).contains("FEU VERT"), "{}", texte(&sortie.stdout));
    assert!(texte(&sortie.stderr).contains("interop WSL indisponible"), "{}", texte(&sortie.stderr));
}

#[test]
fn dossier_et_branche_du_ticket() {
    let o = Orchestre::demarrer("lieu");
    let depot = o.dossier.join("depot-essai");
    std::fs::create_dir_all(&depot).unwrap();
    let git = |dans: &std::path::Path, args: &[&str]| {
        let s = Command::new("git").current_dir(dans).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
        assert!(s.status.success(), "git {args:?} : {}", texte(&s.stderr));
    };
    git(&depot, &["init", "-q", "-b", "principale"]);
    git(&depot, &["commit", "-q", "--allow-empty", "-m", "départ"]);
    git(&depot, &["worktree", "add", "-q", "-b", "feat/essai", "../arbre-essai"]);
    std::fs::create_dir_all(depot.join("sous/dossier")).unwrap();

    let d = o.charge(&["demande", "--cote", "windows", "--ram", "1G", "dans le dépôt"]).current_dir(depot.join("sous/dossier")).output().unwrap();
    assert_eq!(d.status.code(), Some(0), "{}", texte(&d.stderr));
    let d = o.charge(&["demande", "--cote", "windows", "--ram", "1G", "dans le worktree"]).current_dir(o.dossier.join("arbre-essai")).output().unwrap();
    assert_eq!(d.status.code(), Some(0), "{}", texte(&d.stderr));
    let d = o.charge(&["demande", "--cote", "windows", "--ram", "1G", "hors git"]).current_dir("/").env_remove("CHARGE_SESSION").output().unwrap();
    assert_eq!(d.status.code(), Some(0), "{}", texte(&d.stderr));

    let s = texte(&o.sortie(&["statut"]).stdout);
    assert!(s.contains("[test] depot-essai · principale : dans le dépôt"), "{s}");
    assert!(s.contains("[test] arbre-essai · arbre-essai (branche feat/essai) : dans le worktree"), "{s}");
    assert!(s.contains("[—] / : hors git"), "{s}");
}

#[test]
fn maintenance_en_cours_pas_de_lancement() {
    let o = Orchestre::preparer("maintenance");
    let fichier = o.dossier.join("maintenance");
    std::fs::write(&fichier, "").unwrap();
    let r = o.charge(&["statut"]).env("CHARGE_PATIENCE", "4").output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let e = texte(&r.stderr);
    assert!(e.contains("orchestrateur en cours de mise à jour, nouvel essai dans 1 s"), "{e}");
    assert!(!e.contains("lancement"), "pas de lancement pendant la maintenance : {e}");
    assert!(!o.repond());

    // Un fichier vieux de plus de 5 min est ignoré : le client lance l'orchestrateur.
    let ok = Command::new("touch").args(["-d", "-6 minutes"]).arg(&fichier).status().unwrap();
    assert!(ok.success());
    let r = o.sortie(&["statut"]);
    assert_eq!(r.status.code(), Some(0), "{}", texte(&r.stderr));
    assert!(texte(&r.stderr).contains("orchestrateur absent, lancement"));
    let (code, _) = o.appel("POST", "/quitter", Value::Null);
    assert_eq!(code, 200);
    assert!(attendre(Duration::from_secs(5), || !o.repond()));
}

#[test]
fn nom_de_session_claude_ou_codex() {
    let o = Orchestre::demarrer("session");
    let maison = o.dossier.join("maison");
    std::fs::create_dir_all(maison.join(".claude/sessions")).unwrap();
    std::fs::write(maison.join(".claude/sessions/4242.json"), r#"{"pid":4242,"name":"RDEV APP","kind":"interactive"}"#).unwrap();
    std::fs::write(maison.join(".claude/sessions/4343.json"), "pas du json").unwrap();
    let demande = |pid: &str, codex: Option<&str>, quoi: &str| {
        let mut c = o.charge(&["demande", "--cote", "windows", "--ram", "1G", quoi]);
        c.env_remove("CHARGE_SESSION").env("HOME", &maison).env("CLAUDE_PID", pid).env_remove("CODEX_THREAD_ID");
        if let Some(id) = codex {
            c.env("CODEX_THREAD_ID", id).env_remove("CLAUDE_PID");
        }
        let s = c.output().unwrap();
        assert_eq!(s.status.code(), Some(0), "{}", texte(&s.stderr));
    };
    demande("4242", None, "nom lu");
    demande("4343", None, "fichier illisible");
    demande("9999", None, "fichier absent");
    demande("", Some("019a2b3c-4d5e-6f70"), "codex");
    let s = texte(&o.sortie(&["statut"]).stdout);
    assert!(s.contains("[RDEV APP]") && s.contains(": nom lu"), "{s}");
    assert!(s.lines().any(|l| l.contains("[—]") && l.contains("fichier illisible")), "{s}");
    assert!(s.lines().any(|l| l.contains("[—]") && l.contains("fichier absent")), "{s}");
    assert!(s.contains("[Codex 019a2b3c]"), "{s}");
}

#[test]
fn multiplexage_par_id_et_ancien_mode() {
    let o = Orchestre::demarrer("multiplexage");
    o.creer("occupant", "windows", 12 * 1024);
    let (_, t) = o.creer("patient", "windows", 8 * 1024);
    let id = t["id"].as_u64().unwrap();
    let mut l = Ligne::ouvrir(&o);
    let debut = Instant::now();
    let parametres = json!({ "connu": cle(&t["raison"]), "delai_s": "2" });
    l.envoyer(json!({ "id": 2, "methode": "GET", "chemin": format!("/tickets/{id}/attente"), "parametres": parametres }));
    l.envoyer(json!({ "id": 3, "methode": "GET", "chemin": "/etat" }));
    let a = l.lire();
    assert_eq!(a["id"], 3, "l'état répond pendant l'attente : {a}");
    assert!(debut.elapsed() < Duration::from_millis(1500));
    let b = l.lire();
    assert_eq!((b["id"].as_u64(), b["code"].as_u64()), (Some(2), Some(200)), "{b}");
    assert!(debut.elapsed() >= Duration::from_millis(1500), "l'attente a bien duré");
    // Sans id : séquentiel, réponse sans id.
    let c = l.appel("GET", "/etat", Value::Null);
    assert!(c.get("id").is_none() && c["code"] == 200, "{c}");
}

/// Clé stable d'une raison telle que la calcule `Raison::cle`.
fn cle(raison: &Value) -> String {
    let r: commun::modele::Raison = serde_json::from_value(raison.clone()).unwrap();
    r.cle()
}

#[test]
fn relais_bidirectionnel() {
    let o = Orchestre::demarrer("relais-bidir");
    o.creer("occupant", "windows", 12 * 1024);
    let (_, t) = o.creer("patient", "windows", 8 * 1024);
    let id = t["id"].as_u64().unwrap();
    let mut relais = Command::new(env!("CARGO_BIN_EXE_orchestrateur"))
        .args(["relais", "--tube", &o.tube])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut entree = relais.stdin.take().unwrap();
    let mut sortie = BufReader::new(relais.stdout.take().unwrap());
    let connu = cle(&t["raison"]);
    let attente = json!({ "id": 1, "methode": "GET", "chemin": format!("/tickets/{id}/attente"), "parametres": { "connu": connu, "delai_s": "2" } });
    writeln!(entree, "{attente}").unwrap();
    writeln!(entree, "{}", json!({ "id": 2, "methode": "GET", "chemin": "/etat" })).unwrap();
    let mut lire = || {
        let mut ligne = String::new();
        sortie.read_line(&mut ligne).unwrap();
        serde_json::from_str::<Value>(&ligne).unwrap()
    };
    assert_eq!(lire()["id"], 2, "la réponse rapide passe devant l'attente en cours");
    assert_eq!(lire()["id"], 1);
    // Fin de stdin avec une requête `id` en cours : sortie sans l'attendre (un client multiplexé qui part).
    writeln!(entree, "{attente}").unwrap();
    drop(entree);
    let debut = Instant::now();
    assert!(attendre(Duration::from_secs(1), || matches!(relais.try_wait(), Ok(Some(_)))), "fin de stdin : le relais sort");
    assert!(debut.elapsed() < Duration::from_secs(1));
    assert_eq!(relais.wait().unwrap().code(), Some(0));

    // `echo requête | relais` : la réponse d'une requête sans `id` est rendue avant la sortie.
    let sortie = Command::new(env!("CARGO_BIN_EXE_orchestrateur"))
        .args(["relais", "--tube", &o.tube])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut r| {
            let attente = json!({ "methode": "GET", "chemin": format!("/tickets/{id}/attente"), "parametres": { "connu": cle(&t["raison"]), "delai_s": "1" } });
            writeln!(r.stdin.take().unwrap(), "{attente}\n{}", json!({ "methode": "GET", "chemin": "/etat" }))?;
            r.wait_with_output()
        })
        .unwrap();
    let lignes: Vec<Value> = texte(&sortie.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lignes.len(), 2, "{lignes:?}");
    assert_eq!(sortie.status.code(), Some(0));
}

#[test]
fn liaison_perdue_puis_reprise_par_la_meme_session() {
    let o = Orchestre::demarrer("liaison");
    let mut a = Ligne::ouvrir(&o);
    let lien = a.lier("session-A");
    assert_eq!((lien["code"].as_u64(), lien["corps"]["liaison"].as_u64()), (Some(200), Some(1)), "{lien}");
    assert_eq!(a.lier("session-A")["code"], 409, "une seule liaison par connexion");
    assert_eq!(a.appel("POST", "/liaison/battement", json!({}))["code"], 200);
    let actif = a.creer("lie-actif", 4 * 1024);
    let attente = a.creer("lie-attente", 14 * 1024);
    assert_eq!((actif["corps"]["etat"].as_str(), attente["corps"]["etat"].as_str()), (Some("actif"), Some("attente")));
    let (n1, n2) = (actif["corps"]["id"].as_u64().unwrap(), attente["corps"]["id"].as_u64().unwrap());

    drop(a);
    assert!(attendre(Duration::from_secs(3), || o.etat_ticket(n1).as_deref() == Some("orphelin")), "{}", o.journal());
    assert!(o.journal().contains("LIAISON #1 fermée"), "{}", o.journal());
    let phrase = o.ticket(n1).unwrap()["phrase"].as_str().unwrap().to_owned();
    assert!(phrase.contains("liaison de sa session perdue"), "{phrase}");
    // Pas de gel : un autre client passe.
    let (_, autre) = o.creer("autre", "windows", 1024);
    assert_eq!(autre["etat"], "actif", "{autre}");
    o.appel("POST", &format!("/tickets/{}/fin", autre["id"]), json!({}));

    // Une autre session ne reprend rien ; la même, si.
    let mut b = Ligne::ouvrir(&o);
    assert_eq!(b.lier("session-B")["corps"]["repris"], json!([]));
    let mut c = Ligne::ouvrir(&o);
    assert_eq!(c.lier("session-A")["corps"]["repris"], json!([n1, n2]));
    assert_eq!(o.etat_ticket(n1).as_deref(), Some("actif"));
    assert!(o.journal().contains(&format!("reprise de #{n1}, #{n2}")), "{}", o.journal());

    // Sans reprise dans delai.reprise (3 s) : retirés.
    drop(c);
    assert!(attendre(Duration::from_secs(8), || o.ticket(n1).is_none() && o.ticket(n2).is_none()), "{}", o.journal());
    assert!(o.journal().contains(&format!("Ticket #{n1} retiré : liaison de sa session perdue")), "{}", o.journal());
    drop(b);
}

#[test]
fn reprise_de_liaison_apres_redemarrage() {
    let mut o = Orchestre::demarrer("liaison-redemarrage");
    let mut a = Ligne::ouvrir(&o);
    a.lier("session-R");
    let n = a.creer("survivant", 2048)["corps"]["id"].as_u64().unwrap();
    o.tuer();
    drop(a);
    o.lancer();
    assert_eq!(o.etat_ticket(n).as_deref(), Some("orphelin"));
    let mut b = Ligne::ouvrir(&o);
    assert_eq!(b.lier("session-R")["corps"]["repris"], json!([n]));
    assert_eq!(o.etat_ticket(n).as_deref(), Some("actif"));
    assert_eq!(Ligne::ouvrir(&o).appel("POST", "/liaison/battement", json!({}))["code"], 409, "battement sans liaison");
}
