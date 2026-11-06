use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use commun::format::{coeurs, duree, go, lire_duree, lire_ram};
use commun::modele::{Cote, Demande, Fin, MesuresWsl, Presence, Prolonge, Statut, Vue};
use commun::protocole::{RELAIS_ABSENT, Reponse, Requete, TUBE_DEFAUT};
use commun::systeme::{aleatoire, hex, hhmm};
use serde_json::{Value, json};

const AIDE: &str = "\
charge : file des tâches lourdes, servie par orchestrateur.exe (Windows) à travers son relais

  charge run [--cote wsl|windows] [--ram 3G] [--cpu 4] [--duree 15m] [--max 9m] \"description\" -- commande…
      demande un créneau, attend le feu vert au premier plan, lance la commande, signale sa présence,
      libère à la fin ; relancer la même commande reprend le même ticket
  charge demande [options] [\"<session>\"] \"<description>\"
      crée un ticket manuel et affiche son numéro ; la durée déclarée sert de bail
  charge attend <ticket> [--max 9m]       attend le feu vert au premier plan, en écrivant chaque changement
  charge prolonge <ticket> <durée> \"<raison>\"   allonge la durée d'un ticket actif
  charge fin <ticket> [--code N]          libère le créneau
  charge annule <ticket>                  retire la demande ou libère le créneau
  charge statut                           file, réserves et mesures

Codes de sortie : 0 feu vert ou succès, 1 ticket sorti de la file, 2 erreur, 3 refus,
75 toujours en attente après --max (relancer la même commande garde la place), 130 interrompu.
Variables : CHARGE_EXE (orchestrateur.exe), CHARGE_TUBE, CHARGE_DOSSIER (état Windows vu de WSL),
CHARGE_SESSION (nom affiché).
";

const PATIENCE_S: u64 = 120;
/// Au-delà, un fichier `maintenance` est considéré comme oublié par une réinstallation qui a planté.
const MAINTENANCE_MAX: Duration = Duration::from_secs(5 * 60);
const ATTENTE_MAX_ENTRE_ESSAIS_S: u64 = 15;
const INTERVALLE_PRESENCE: Duration = Duration::from_secs(10);
const DELAI_COURT: Duration = Duration::from_secs(15);
const MARGE_ATTENTE: Duration = Duration::from_secs(30);
const MAX_DEFAUT_S: u64 = 9 * 60;
const CODE_ENCORE: i32 = 75;
const INTERROMPU: &str = "interrompu par un signal";

static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn noter_signal(sig: libc::c_int) {
    SIGNAL.store(sig, Ordering::SeqCst);
}

fn installer_signaux() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, noter_signal as *const () as libc::sighandler_t) };
    }
}

fn signal_recu() -> bool {
    SIGNAL.load(Ordering::SeqCst) != 0
}

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match executer(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("charge : {e}");
            2
        }
    };
    std::process::exit(code);
}

fn executer(args: &[String]) -> Result<i32, String> {
    let Some((cmd, reste)) = args.split_first() else {
        eprint!("{AIDE}");
        return Ok(2);
    };
    match cmd.as_str() {
        "run" => run(reste),
        "demande" => demande(reste),
        "attend" => attend(reste),
        "prolonge" => prolonge(reste),
        "fin" => fin(reste),
        "annule" => annule(numero(reste)?),
        "statut" => statut(),
        "aide" | "--help" | "-h" => {
            print!("{AIDE}");
            Ok(0)
        }
        _ => {
            eprint!("{AIDE}");
            Ok(2)
        }
    }
}

fn numero(args: &[String]) -> Result<u64, String> {
    args.first()
        .and_then(|a| a.trim_start_matches('#').parse().ok())
        .ok_or_else(|| "numéro de ticket attendu".to_owned())
}

enum Echec {
    Absent(String),
    Perdu(String),
    Delai,
    Interrompu,
}

/// Un relais `orchestrateur.exe relais` gardé ouvert : une requête par ligne sur son stdin, une réponse par ligne sur son stdout.
struct Connexion {
    enfant: Child,
    entree: Option<ChildStdin>,
    lignes: Receiver<String>,
}

impl Connexion {
    fn ouvrir(exe: &Path, tube: &str) -> Result<Connexion, Echec> {
        let mut cmd = Command::new(exe);
        cmd.args(["relais", "--tube", tube]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Un exe Windows lancé depuis un dossier Linux hériterait d'un chemin UNC.
        if exe.starts_with("/mnt/") {
            cmd.current_dir("/mnt/c");
        }
        let mut enfant = cmd.spawn().map_err(|e| Echec::Perdu(format!("lancement de {} : {e}", exe.display())))?;
        let sortie = enfant.stdout.take().expect("stdout du relais");
        let entree = enfant.stdin.take();
        let (tx, lignes) = mpsc::channel();
        std::thread::spawn(move || {
            for ligne in BufReader::new(sortie).lines() {
                let Ok(ligne) = ligne else { break };
                if tx.send(ligne).is_err() {
                    break;
                }
            }
        });
        Ok(Connexion { enfant, entree, lignes })
    }

    fn fin(&mut self) -> Echec {
        self.entree = None;
        let statut = self.enfant.wait();
        let mut erreur = String::new();
        if let Some(mut e) = self.enfant.stderr.take() {
            let _ = e.read_to_string(&mut erreur);
        }
        let erreur = erreur.trim().to_owned();
        match statut.ok().and_then(|s| s.code()) {
            Some(RELAIS_ABSENT) => Echec::Absent(erreur),
            Some(c) => Echec::Perdu(if erreur.is_empty() { format!("relais sorti avec le code {c}") } else { erreur }),
            None => Echec::Perdu("relais interrompu".into()),
        }
    }

    fn echanger(&mut self, req: &Requete, delai: Duration) -> Result<Reponse, Echec> {
        let mut ligne = serde_json::to_string(req).map_err(|e| Echec::Perdu(e.to_string()))?;
        ligne.push('\n');
        let ecrit = match self.entree.as_mut() {
            Some(e) => e.write_all(ligne.as_bytes()).and_then(|_| e.flush()).is_ok(),
            None => false,
        };
        if !ecrit {
            return Err(self.fin());
        }
        let limite = Instant::now() + delai;
        loop {
            match self.lignes.recv_timeout(Duration::from_millis(250)) {
                Ok(l) => {
                    return serde_json::from_str(&l).map_err(|e| Echec::Perdu(format!("réponse illisible ({e}) : {l}")));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.fin()),
                Err(RecvTimeoutError::Timeout) if signal_recu() => return Err(Echec::Interrompu),
                Err(RecvTimeoutError::Timeout) if Instant::now() > limite => return Err(Echec::Delai),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl Drop for Connexion {
    fn drop(&mut self) {
        self.entree = None;
        let _ = self.enfant.kill();
        let _ = self.enfant.wait();
    }
}

struct Client {
    exe: PathBuf,
    tube: String,
    connexion: Option<Connexion>,
    lance: bool,
    /// Échéance de `--max` : les nouveaux essais s'arrêtent là pour rester sous le délai d'une commande.
    limite: Option<Instant>,
}

/// Échec de lancement du relais par l'interop WSL (vsock, exécution impossible), par opposition à un orchestrateur coupé.
fn echec_interop(message: &str) -> bool {
    message.contains("WSL ERROR") || message.contains("vsock") || message.starts_with("lancement de ") || message.starts_with("relais sorti avec le code")
}

fn patience() -> Duration {
    Duration::from_secs(std::env::var("CHARGE_PATIENCE").ok().and_then(|p| p.parse().ok()).unwrap_or(PATIENCE_S))
}

fn trouver_exe() -> Result<PathBuf, String> {
    if let Some(e) = std::env::var_os("CHARGE_EXE") {
        return Ok(PathBuf::from(e));
    }
    let utilisateurs = std::fs::read_dir("/mnt/c/Users").map_err(|e| format!("/mnt/c/Users illisible : {e}"))?;
    for u in utilisateurs.flatten() {
        let exe = u.path().join("AppData/Local/Programs/orchestrateur/orchestrateur.exe");
        if exe.exists() {
            return Ok(exe);
        }
    }
    Err("orchestrateur.exe introuvable : installe-le dans %LOCALAPPDATA%\\Programs\\orchestrateur ou définis CHARGE_EXE".into())
}

fn wslpath(p: &Path) -> Option<String> {
    let sortie = Command::new("wslpath").arg("-w").arg(p).stderr(Stdio::null()).output().ok()?;
    let texte = String::from_utf8(sortie.stdout).ok()?.trim().to_owned();
    (sortie.status.success() && !texte.is_empty()).then_some(texte)
}

impl Client {
    fn nouveau() -> Result<Client, String> {
        Ok(Client {
            exe: trouver_exe()?,
            tube: std::env::var("CHARGE_TUBE").unwrap_or_else(|_| TUBE_DEFAUT.into()),
            connexion: None,
            lance: false,
            limite: None,
        })
    }

    /// Dossier d'état Windows : `CHARGE_DOSSIER`, sinon `%LOCALAPPDATA%\orchestrateur` déduit de l'emplacement de l'exe.
    fn dossier_etat(&self) -> Option<PathBuf> {
        std::env::var_os("CHARGE_DOSSIER")
            .map(PathBuf::from)
            .or_else(|| self.exe.parent()?.parent()?.parent().map(|local| local.join("orchestrateur")))
    }

    /// Réinstallation en cours (fichier posé par windows/reinstaller.ps1) : on attend au lieu de lancer l'exe.
    fn en_maintenance(&self) -> bool {
        self.dossier_etat()
            .and_then(|d| std::fs::metadata(d.join("maintenance")).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < MAINTENANCE_MAX)
    }

    fn copie(&self) -> Client {
        Client { exe: self.exe.clone(), tube: self.tube.clone(), connexion: None, lance: self.lance, limite: None }
    }

    /// Démarre l'orchestrateur, une seule fois, quand le relais le dit absent.
    fn lancer(&self) {
        let mut args: Vec<String> = Vec::new();
        if let Some(d) = std::env::var_os("CHARGE_DOSSIER").map(PathBuf::from) {
            let chemin = if self.exe.extension().is_some_and(|e| e == "exe") { wslpath(&d) } else { Some(d.display().to_string()) };
            if let Some(c) = chemin {
                args.extend(["--dossier".into(), c]);
            }
        }
        if let Ok(extra) = std::env::var("CHARGE_ARGS_LANCEMENT") {
            args.extend(extra.split_whitespace().map(str::to_owned));
        }
        eprintln!("charge : orchestrateur absent, lancement de {}…", self.exe.display());
        if self.exe.extension().is_some_and(|e| e == "exe") {
            let Some(exe_w) = wslpath(&self.exe) else {
                eprintln!("charge : wslpath a échoué, lancement impossible.");
                return;
            };
            let liste: Vec<String> = args.iter().map(|a| format!("'\"{}\"'", a.replace('\'', "''"))).collect();
            // `cmd /c start` ferait hériter à l'exe les tubes de l'interop : on attendrait sa fin.
            let mut commande = format!("Start-Process -FilePath '{}'", exe_w.replace('\'', "''"));
            if !liste.is_empty() {
                commande += &format!(" -ArgumentList {}", liste.join(","));
            }
            let _ = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &commande])
                .current_dir("/mnt/c")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        } else {
            let _ = Command::new(&self.exe).args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }

    fn appeler(&mut self, req: &Requete, delai: Duration) -> Result<Reponse, String> {
        let debut = Instant::now();
        let patience = patience();
        let mut derniere = String::new();
        let mut pause = Duration::from_secs(1);
        loop {
            let mut absent = false;
            if self.connexion.is_none() {
                match Connexion::ouvrir(&self.exe, &self.tube) {
                    Ok(c) => self.connexion = Some(c),
                    Err(Echec::Perdu(m)) => derniere = m,
                    Err(_) => {}
                }
            }
            if let Some(c) = self.connexion.as_mut() {
                match c.echanger(req, delai) {
                    Ok(r) => return Ok(r),
                    Err(Echec::Interrompu) => {
                        self.connexion = None;
                        return Err(INTERROMPU.into());
                    }
                    Err(Echec::Delai) => {
                        self.connexion = None;
                        return Err(format!("pas de réponse de l'orchestrateur en {} s", delai.as_secs()));
                    }
                    Err(Echec::Absent(m)) => {
                        self.connexion = None;
                        absent = true;
                        derniere = m;
                    }
                    Err(Echec::Perdu(m)) => {
                        self.connexion = None;
                        derniere = m;
                    }
                }
            }
            if signal_recu() {
                return Err(INTERROMPU.into());
            }
            let maintenance = absent && self.en_maintenance();
            if absent && !maintenance && !self.lance {
                self.lance = true;
                self.lancer();
            }
            let attente = if absent && !maintenance { Duration::from_secs(1) } else { pause };
            let echeance = self.limite.is_some_and(|l| Instant::now() + attente >= l);
            if debut.elapsed() + attente > patience || echeance {
                return Err(format!(
                    "orchestrateur injoignable depuis {} s ({}). Vérifie son icône dans la zone de notification Windows.",
                    debut.elapsed().as_secs(),
                    derniere.lines().next().unwrap_or("")
                ));
            }
            if maintenance {
                eprintln!("charge : orchestrateur en cours de mise à jour, nouvel essai dans {} s.", attente.as_secs());
                pause = (pause * 2).min(Duration::from_secs(ATTENTE_MAX_ENTRE_ESSAIS_S));
            } else if !absent {
                let quoi = if echec_interop(&derniere) { "interop WSL indisponible" } else { "orchestrateur coupé" };
                eprintln!(
                    "charge : {quoi} ({}), nouvel essai dans {} s.",
                    derniere.lines().next().unwrap_or("sans détail"),
                    attente.as_secs()
                );
                pause = (pause * 2).min(Duration::from_secs(ATTENTE_MAX_ENTRE_ESSAIS_S));
            }
            std::thread::sleep(attente);
        }
    }

    fn demander(&mut self, methode: &str, chemin: &str, corps: Value) -> Result<Reponse, String> {
        self.appeler(&Requete::nouvelle(methode, chemin, corps), DELAI_COURT)
    }

    fn attente(&mut self, id: u64, connu: &str, plafond: Duration) -> Result<Reponse, String> {
        let mut req = Requete::nouvelle("GET", &format!("/tickets/{id}/attente"), Value::Null);
        req.parametres = BTreeMap::from([
            ("connu".into(), connu.to_owned()),
            ("delai_s".into(), plafond.as_secs().max(1).to_string()),
        ]);
        ajouter_mesures(&mut req);
        self.appeler(&req, plafond + MARGE_ATTENTE)
    }
}

fn vmstat(nom: &str) -> u64 {
    std::fs::read_to_string("/proc/vmstat")
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix(nom)?.strip_prefix(' ')?.trim().parse().ok()))
        .unwrap_or(0)
}

fn mesures_wsl() -> Option<MesuresWsl> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let champ = |nom: &str| -> Option<u64> {
        meminfo
            .lines()
            .find(|l| l.starts_with(nom))?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()
            .map(|ko| ko / 1024)
    };
    let charge1 = std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()?;
    Some(MesuresWsl {
        mem_dispo_mo: champ("MemAvailable:")?,
        mem_totale_mo: champ("MemTotal:")?,
        charge1,
        swap_total_mo: champ("SwapTotal:").unwrap_or(0),
        swap_libre_mo: champ("SwapFree:").unwrap_or(0),
        pswpin: vmstat("pswpin"),
        pswpout: vmstat("pswpout"),
        quand: 0,
    })
}

fn ajouter_mesures(req: &mut Requete) {
    if let Some(m) = mesures_wsl() {
        req.parametres.insert("mem_dispo_mo".into(), m.mem_dispo_mo.to_string());
        req.parametres.insert("mem_totale_mo".into(), m.mem_totale_mo.to_string());
        req.parametres.insert("charge1".into(), format!("{:.2}", m.charge1));
        req.parametres.insert("swap_total_mo".into(), m.swap_total_mo.to_string());
        req.parametres.insert("swap_libre_mo".into(), m.swap_libre_mo.to_string());
        req.parametres.insert("pswpin".into(), m.pswpin.to_string());
        req.parametres.insert("pswpout".into(), m.pswpout.to_string());
    }
}

fn requete_id() -> String {
    let h = hex(&aleatoire(16).unwrap_or_else(|_| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{:032x}", t.as_nanos() ^ ((std::process::id() as u128) << 64)).into_bytes()[..16].to_vec()
    }));
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn fnv1a(texte: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for o in texte.bytes() {
        h ^= o as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

struct Options {
    cote: Cote,
    ram_mo: Option<u64>,
    cpu: Option<u32>,
    duree_s: Option<u64>,
    max_s: u64,
    code: Option<i32>,
    positionnels: Vec<String>,
    commande: Vec<String>,
    sans_orchestrateur: bool,
}

fn options(args: &[String], avec_commande: bool) -> Result<Options, String> {
    let mut o = Options {
        cote: Cote::Wsl,
        ram_mo: None,
        cpu: None,
        duree_s: None,
        max_s: MAX_DEFAUT_S,
        code: None,
        positionnels: Vec::new(),
        commande: Vec::new(),
        sans_orchestrateur: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" && avec_commande {
            o.commande = args[i + 1..].to_vec();
            break;
        }
        let (nom, valeur_jointe) = match a.split_once('=') {
            Some((n, v)) if a.starts_with("--") => (n, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        let mut valeur = || -> Result<String, String> {
            if let Some(v) = valeur_jointe.clone() {
                return Ok(v);
            }
            i += 1;
            args.get(i).cloned().ok_or_else(|| format!("valeur attendue après {nom}"))
        };
        match nom {
            "--cote" => {
                let v = valeur()?;
                o.cote = Cote::lire(&v).ok_or_else(|| format!("--cote : « wsl » ou « windows » attendu, pas « {v} »"))?;
            }
            "--ram" => {
                let v = valeur()?;
                o.ram_mo = Some(lire_ram(&v).ok_or_else(|| format!("--ram illisible : « {v} » (ex. 3G, 1.5G, 512M)"))?);
            }
            "--cpu" => {
                let v = valeur()?;
                o.cpu = Some(v.parse().ok().filter(|&n| n > 0).ok_or_else(|| format!("--cpu : entier positif attendu, pas « {v} »"))?);
            }
            "--duree" => {
                let v = valeur()?;
                o.duree_s = Some(lire_duree(&v).ok_or_else(|| format!("--duree illisible : « {v} » (ex. 15m, 1h30, 90s)"))?);
            }
            "--max" => {
                let v = valeur()?;
                o.max_s = lire_duree(&v).ok_or_else(|| format!("--max illisible : « {v} » (ex. 9m, 90s)"))?;
            }
            "--code" => {
                let v = valeur()?;
                o.code = Some(v.parse().map_err(|_| format!("--code : entier attendu, pas « {v} »"))?);
            }
            "--sans-orchestrateur" if avec_commande => o.sans_orchestrateur = true,
            n if n.starts_with("--") => return Err(format!("option inconnue : {n}")),
            _ => o.positionnels.push(a.clone()),
        }
        i += 1;
    }
    Ok(o)
}

/// `CHARGE_SESSION`, sinon le nom de la session Claude Code (fichier interne non documenté, lu sans exigence),
/// sinon Codex, sinon « — ».
fn session_par_defaut() -> String {
    if let Ok(s) = std::env::var("CHARGE_SESSION") {
        return s;
    }
    let nom_claude = std::env::var("CLAUDE_PID").ok().and_then(|pid| {
        let chemin = PathBuf::from(std::env::var_os("HOME")?).join(format!(".claude/sessions/{pid}.json"));
        let v: Value = serde_json::from_slice(&std::fs::read(chemin).ok()?).ok()?;
        v["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned)
    });
    if let Some(n) = nom_claude {
        return n;
    }
    match std::env::var("CODEX_THREAD_ID") {
        Ok(id) if !id.is_empty() => format!("Codex {}", id.chars().take(8).collect::<String>()),
        _ => "—".into(),
    }
}

fn nom_de(chemin: &Path) -> String {
    chemin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| chemin.display().to_string())
}

/// (dossier, branche / worktree) du dossier courant, en un appel git borné à 2 s : les dépôts sur /mnt/g sont lents.
fn lieu_git() -> (String, Option<String>) {
    let courant = std::env::current_dir().unwrap_or_default();
    let mut enfant = match Command::new("git")
        .args(["rev-parse", "--show-toplevel", "--git-dir", "--git-common-dir", "--abbrev-ref", "HEAD", "--short", "HEAD"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(e) => e,
        Err(_) => return (nom_de(&courant), None),
    };
    let limite = Instant::now() + Duration::from_secs(2);
    let statut = loop {
        match enfant.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < limite => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = enfant.kill();
                let _ = enfant.wait();
                break None;
            }
        }
    };
    let Some(statut) = statut else {
        return (nom_de(&courant), Some("?".into()));
    };
    let mut sortie = String::new();
    if let Some(mut s) = enfant.stdout.take() {
        let _ = s.read_to_string(&mut sortie);
    }
    let lignes: Vec<&str> = sortie.lines().collect();
    let [racine, git_dir, commun, branche, sha] = lignes.as_slice() else {
        return (nom_de(&courant), if statut.success() { Some("?".into()) } else { None });
    };
    let absolu = |p: &str| std::fs::canonicalize(courant.join(p)).unwrap_or_else(|_| courant.join(p));
    let branche = if *branche == "HEAD" { format!("HEAD@{sha}") } else { (*branche).to_owned() };
    let racine = PathBuf::from(racine);
    let texte = if absolu(git_dir) != absolu(commun) {
        format!("{} (branche {branche})", nom_de(&racine))
    } else {
        branche
    };
    (nom_de(&racine), Some(texte))
}

/// Signature indicative pour l'historique : programme et sous-commandes, sans options ni chemins.
fn signature(commande: &[String]) -> String {
    let mut mots = Vec::new();
    for (i, m) in commande.iter().enumerate() {
        if i == 0 {
            mots.push(Path::new(m).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| m.clone()));
        } else if !m.starts_with('-') && !m.contains('/') && !m.contains('=') && mots.len() < 3 {
            mots.push(m.clone());
        } else {
            break;
        }
    }
    mots.join(" ")
}

fn demande_de(o: &Options, session: String, description: String, manuel: bool) -> Demande {
    let dossier = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    let (projet, branche) = lieu_git();
    let session_claude = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let rattachement = if manuel {
        String::new()
    } else {
        fnv1a(&[
            session_claude.clone().unwrap_or_else(|| session.clone()),
            dossier.clone(),
            o.cote.nom().into(),
            format!("{:?} {:?} {:?}", o.ram_mo, o.cpu, o.duree_s),
            description.clone(),
            o.commande.join("\u{1f}"),
        ]
        .join("\u{1e}"))
    };
    Demande {
        requete_id: requete_id(),
        cote: o.cote,
        ram_mo: o.ram_mo,
        cpu: o.cpu,
        duree_s: o.duree_s,
        description,
        session,
        claude_session_id: session_claude,
        claude_pid: std::env::var("CLAUDE_PID").ok().and_then(|p| p.parse().ok()),
        dossier,
        signature: signature(&o.commande),
        mesures: mesures_wsl(),
        manuel,
        rattachement,
        projet: Some(projet),
        branche,
    }
}

fn lire_vue(corps: &Value) -> Result<Vue, String> {
    serde_json::from_value(corps.clone()).map_err(|e| format!("réponse illisible de l'orchestrateur ({e}) : {corps}"))
}

fn phrase(corps: &Value) -> String {
    corps["raison"]["phrase"]
        .as_str()
        .or_else(|| corps["phrase"].as_str())
        .or_else(|| corps["erreur"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| corps.to_string())
}

fn creer(c: &mut Client, d: &Demande) -> Result<Result<(u16, Vue), Vue>, String> {
    let r = c.demander("POST", "/tickets", serde_json::to_value(d).map_err(|e| e.to_string())?)?;
    match r.code {
        200 | 201 => Ok(Ok((r.code, lire_vue(&r.corps)?))),
        422 => Ok(Err(lire_vue(&r.corps)?)),
        code => Err(format!("création refusée ({code}) : {}", phrase(&r.corps))),
    }
}

fn demande(args: &[String]) -> Result<i32, String> {
    let o = options(args, false)?;
    let (session, description) = match o.positionnels.as_slice() {
        [d] => (session_par_defaut(), d.clone()),
        [s, d] => (s.clone(), d.clone()),
        _ => return Err("usage : charge demande [options] [\"<session>\"] \"<description>\"".into()),
    };
    let mut c = Client::nouveau()?;
    match creer(&mut c, &demande_de(&o, session, description, true))? {
        Ok((_, v)) => {
            println!("{}", v.id);
            eprintln!("{}", v.raison.phrase);
            if v.etat == "attente" {
                eprintln!("Lance « charge attend {} » au premier plan (il rend la main au feu vert, ou après --max avec le code 75).", v.id);
            } else {
                eprintln!("Lance la tâche lourde, puis « charge fin {} ».", v.id);
            }
            Ok(0)
        }
        Err(refus) => {
            eprintln!("{}", refus.raison.phrase);
            Ok(3)
        }
    }
}

enum FinAttente {
    FeuVert,
    Sorti(String),
    Encore(String),
}

/// Attend au premier plan jusqu'au feu vert, à la sortie du ticket, ou à l'échéance de `--max`.
fn attendre(c: &mut Client, id: u64, connu: &mut String, max_s: u64, ecrire: &dyn Fn(&str)) -> Result<FinAttente, String> {
    let limite = Instant::now() + Duration::from_secs(max_s);
    c.limite = Some(limite);
    let mut derniere = String::new();
    loop {
        let reste = limite.saturating_duration_since(Instant::now());
        if reste.is_zero() {
            c.limite = None;
            return Ok(FinAttente::Encore(derniere));
        }
        // Une coupure de l'interop ne fait jamais lâcher le ticket : on se rattache au retour, jusqu'à --max.
        let r = match c.attente(id, connu, reste.min(Duration::from_secs(60))) {
            Ok(r) => r,
            Err(e) if e == INTERROMPU => return Err(e),
            Err(e) => {
                eprintln!("charge : {e} Le ticket #{id} reste en file ; nouvel essai.");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        match r.code {
            200 => {
                let v = lire_vue(&r.corps)?;
                let cle = v.raison.cle();
                derniere = v.raison.phrase.clone();
                if cle != *connu {
                    ecrire(&v.raison.phrase);
                    *connu = cle;
                }
                if v.etat == "actif" || v.etat == "orphelin" {
                    c.limite = None;
                    return Ok(FinAttente::FeuVert);
                }
            }
            404 => {
                c.limite = None;
                return Ok(FinAttente::Sorti(phrase(&r.corps)));
            }
            code => return Err(format!("attente du #{id} ({code}) : {}", phrase(&r.corps))),
        }
    }
}

fn attend(args: &[String]) -> Result<i32, String> {
    let id = numero(args)?;
    let o = options(&args[1..], false)?;
    installer_signaux();
    let mut c = Client::nouveau()?;
    let mut connu = String::new();
    let ecrire = |p: &str| {
        println!("{p}");
        let _ = std::io::stdout().flush();
    };
    match attendre(&mut c, id, &mut connu, o.max_s, &ecrire) {
        Ok(FinAttente::FeuVert) => {
            println!("FEU VERT pour le ticket #{id} : lance ta tâche lourde maintenant, puis « charge fin {id} ».");
            Ok(0)
        }
        Ok(FinAttente::Sorti(p)) => {
            println!("Ticket #{id} hors de la file : {p}");
            Ok(1)
        }
        Ok(FinAttente::Encore(p)) => {
            println!(
                "Toujours en attente après {} : {p} Relance « charge attend {id} » pour garder ta place.",
                duree(o.max_s)
            );
            Ok(CODE_ENCORE)
        }
        Err(e) if e == INTERROMPU => Ok(130),
        Err(e) => Err(e),
    }
}

fn prolonge(args: &[String]) -> Result<i32, String> {
    let (Some(id), Some(d), Some(raison)) = (
        args.first().and_then(|a| a.trim_start_matches('#').parse::<u64>().ok()),
        args.get(1).and_then(|d| lire_duree(d)),
        args.get(2),
    ) else {
        return Err("usage : charge prolonge <ticket> <durée> \"<raison>\" (ex. charge prolonge 12 20m \"Infection plus longue que prévu\")".into());
    };
    let mut c = Client::nouveau()?;
    let corps = serde_json::to_value(Prolonge { duree_s: d, raison: raison.clone() }).map_err(|e| e.to_string())?;
    let r = c.demander("POST", &format!("/tickets/{id}/prolonge"), corps)?;
    match r.code {
        200 => {
            println!("{}", phrase(&r.corps));
            Ok(0)
        }
        404 => {
            eprintln!("Ticket #{id} inconnu ou déjà sorti de la file : {}", phrase(&r.corps));
            Ok(1)
        }
        code => Err(format!("prolongation du #{id} ({code}) : {}", phrase(&r.corps))),
    }
}

fn fin(args: &[String]) -> Result<i32, String> {
    let id = numero(args)?;
    let o = options(&args[1..], false)?;
    let mut c = Client::nouveau()?;
    clore(&mut c, id, &Fin { code: o.code, pic_mo: None, duree_s: None })
}

fn clore(c: &mut Client, id: u64, f: &Fin) -> Result<i32, String> {
    let r = c.demander("POST", &format!("/tickets/{id}/fin"), serde_json::to_value(f).map_err(|e| e.to_string())?)?;
    afficher_cloture(id, &r)
}

fn annule(id: u64) -> Result<i32, String> {
    let mut c = Client::nouveau()?;
    let r = c.demander("DELETE", &format!("/tickets/{id}"), Value::Null)?;
    afficher_cloture(id, &r)
}

fn afficher_cloture(id: u64, r: &Reponse) -> Result<i32, String> {
    match r.code {
        200 => {
            println!("{}", phrase(&r.corps));
            Ok(0)
        }
        404 => {
            eprintln!("Ticket #{id} inconnu ou déjà sorti de la file : {}", phrase(&r.corps));
            Ok(1)
        }
        code => Err(format!("ticket #{id} ({code}) : {}", phrase(&r.corps))),
    }
}

fn statut() -> Result<i32, String> {
    let mut c = Client::nouveau()?;
    let mut req = Requete::nouvelle("GET", "/etat", Value::Null);
    ajouter_mesures(&mut req);
    let r = c.appeler(&req, DELAI_COURT)?;
    if r.code != 200 {
        return Err(format!("statut ({}) : {}", r.code, phrase(&r.corps)));
    }
    let s: Statut = serde_json::from_value(r.corps).map_err(|e| format!("statut illisible : {e}"))?;
    let m = &s.mesures;
    println!(
        "Orchestrateur (époque {}) · RAM Windows {} disponibles sur {} · commit {} / {} · CPU {:.0} %",
        s.epoque,
        go(m.ram_dispo_mo as i64),
        go(m.ram_totale_mo as i64),
        go(m.commit_mo as i64),
        go(m.commit_limite_mo as i64),
        m.cpu_pct
    );
    match (m.vmmem, m.wsl) {
        (None, _) => println!("WSL : arrêté (vmmemWSL absent)"),
        (Some(v), Some(w)) => println!(
            "WSL : vmmemWSL {} · MemAvailable {} sur {} · charge {:.2} (mesure d'il y a {} s)",
            go(v.rss_mo as i64),
            go(w.mem_dispo_mo as i64),
            go(w.mem_totale_mo as i64),
            w.charge1,
            s.maintenant.saturating_sub(w.quand)
        ),
        (Some(v), None) => println!("WSL : vmmemWSL {} · pas encore de mesure du client", go(v.rss_mo as i64)),
    }
    if let Some(g) = s.gel_manuel {
        println!("! File gelée à la main depuis {}.", hhmm(g));
    }
    for r in &s.reserves {
        let q = |v: i64| if r.cle.starts_with("cpu") { coeurs(v) } else { go(v) };
        println!(
            "  {:<12} disponible {:>9} · réservé {:>9} · marge {:>8} · libre {}",
            r.nom,
            q(r.disponible),
            q(r.reserve),
            q(r.marge),
            r.libre_lisible()
        );
    }
    for a in s.alertes.iter().filter(|a| !a.contains("gel manuel")) {
        println!("! {a}");
    }
    if s.tickets.is_empty() {
        println!("File vide, aucun créneau occupé.");
    }
    for t in &s.tickets {
        let depuis = t.accorde.unwrap_or(t.cree);
        let lieu = match (&t.projet, &t.branche) {
            (Some(p), Some(b)) => format!(" {p} · {b}"),
            (Some(p), None) => format!(" {p}"),
            _ => String::new(),
        };
        println!(
            "#{:<4} {:<8} {:<7} {:>7} {:>9} {:>8}  depuis {}  [{}]{lieu} : {}{}",
            t.id,
            format!("{:?}", t.etat).to_lowercase(),
            t.cote.nom(),
            go(t.besoin.ram_mo as i64),
            coeurs(t.besoin.cpu as i64),
            duree(t.besoin.duree_s),
            hhmm(depuis),
            t.session,
            t.description,
            match (t.force, t.manuel) {
                (true, true) => " (forcé, manuel)",
                (true, false) => " (forcé)",
                (false, true) => " (manuel)",
                (false, false) => "",
            }
        );
        println!("      {}", t.phrase);
    }
    Ok(0)
}

fn run(args: &[String]) -> Result<i32, String> {
    let o = options(args, true)?;
    if o.commande.is_empty() {
        return Err("usage : charge run [options] \"description\" -- commande…".into());
    }
    if o.positionnels.len() > 1 {
        return Err("une seule description attendue avant « -- » (mets-la entre guillemets)".into());
    }
    if o.sans_orchestrateur {
        eprintln!("charge : --sans-orchestrateur : lancement direct, hors file.");
        let statut = Command::new(&o.commande[0])
            .args(&o.commande[1..])
            .status()
            .map_err(|e| format!("lancement de {} : {e}", o.commande[0]))?;
        return Ok(statut.code().unwrap_or_else(|| 128 + statut.signal().unwrap_or(0)));
    }
    installer_signaux();
    let description = o.positionnels.first().cloned().unwrap_or_else(|| o.commande.join(" "));
    let mut c = Client::nouveau()?;
    let d = demande_de(&o, session_par_defaut(), description, false);
    let (code, v) = match creer(&mut c, &d)? {
        Ok(r) => r,
        Err(refus) => {
            eprintln!("charge : {}", refus.raison.phrase);
            return Ok(3);
        }
    };
    let id = v.id;
    if code == 200 {
        eprintln!("charge : ticket #{id} repris (même commande) · {}", v.raison.phrase);
    } else {
        eprintln!("charge : ticket #{id} · {}", v.raison.phrase);
    }
    if v.etat == "attente" {
        let mut connu = v.raison.cle();
        let ecrire = |p: &str| eprintln!("charge : {p}");
        match attendre(&mut c, id, &mut connu, o.max_s, &ecrire) {
            Ok(FinAttente::FeuVert) => {}
            Ok(FinAttente::Sorti(p)) => {
                eprintln!("charge : ticket #{id} retiré : {p}");
                return Ok(1);
            }
            Ok(FinAttente::Encore(p)) => {
                eprintln!(
                    "charge : toujours en attente après {} : {p} Relance la même commande pour reprendre le ticket #{id} sans perdre ta place.",
                    duree(o.max_s)
                );
                return Ok(CODE_ENCORE);
            }
            Err(e) if e == INTERROMPU => {
                let _ = c.copie().demander("DELETE", &format!("/tickets/{id}"), Value::Null);
                eprintln!("charge : interrompu, ticket #{id} annulé.");
                return Ok(130);
            }
            Err(e) => return Err(e),
        }
    }
    executer_accorde(&mut c, id, &o.commande)
}

fn executer_accorde(c: &mut Client, id: u64, commande: &[String]) -> Result<i32, String> {
    eprintln!("charge : feu vert #{id}, lancement de : {}", commande.join(" "));
    let debut = Instant::now();
    let mut enfant = match Command::new(&commande[0]).args(&commande[1..]).spawn() {
        Ok(e) => e,
        Err(e) => {
            let _ = clore(c, id, &Fin { code: Some(127), pic_mo: None, duree_s: Some(0) });
            return Err(format!("lancement de {} : {e}", commande[0]));
        }
    };

    let fini = Arc::new(AtomicBool::new(false));
    let veilleur = {
        let fini = fini.clone();
        let mut c = c.copie();
        std::thread::spawn(move || {
            let chemin = format!("/tickets/{id}/presence");
            let mut prevenu = false;
            while !fini.load(Ordering::SeqCst) {
                let corps = serde_json::to_value(Presence { mesures: mesures_wsl() }).unwrap_or(json!({}));
                if let Ok(r) = c.demander("POST", &chemin, corps)
                    && r.code == 404
                    && !prevenu
                {
                    eprintln!("charge : le ticket #{id} n'est plus suivi par l'orchestrateur ; la commande continue.");
                    prevenu = true;
                }
                let reveil = Instant::now() + INTERVALLE_PRESENCE;
                while Instant::now() < reveil && !fini.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        })
    };

    let mut transmis: Option<Instant> = None;
    let mut tue = false;
    let statut = loop {
        match enfant.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => {
                eprintln!("charge : attente de la commande : {e}");
                break enfant.wait().map_err(|e| e.to_string())?;
            }
        }
        if signal_recu() && transmis.is_none() {
            unsafe { libc::kill(enfant.id() as i32, libc::SIGTERM) };
            transmis = Some(Instant::now());
        }
        if !tue && transmis.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            let _ = enfant.kill();
            tue = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    fini.store(true, Ordering::SeqCst);
    let _ = veilleur.join();

    let code = statut.code().unwrap_or_else(|| 128 + statut.signal().unwrap_or(0));
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let pic_mo =
        (unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) } == 0).then_some(usage.ru_maxrss as u64 / 1024);
    let duree_s = debut.elapsed().as_secs();
    let f = Fin { code: Some(code), pic_mo, duree_s: Some(duree_s) };
    // Le signal reçu ne doit pas empêcher de déclarer la fin.
    SIGNAL.store(0, Ordering::SeqCst);
    let mut c = c.copie();
    match c.demander("POST", &format!("/tickets/{id}/fin"), serde_json::to_value(&f).map_err(|e| e.to_string())?) {
        Ok(r) if r.code == 200 || r.code == 404 => {}
        Ok(r) => eprintln!("charge : fin du #{id} non enregistrée ({}) : {}", r.code, phrase(&r.corps)),
        Err(e) => eprintln!("charge : fin du #{id} non enregistrée : {e} ; « charge fin {id} » plus tard."),
    }
    eprintln!("charge : #{id} terminé (code {code}, {}).", duree(duree_s));
    Ok(code)
}
