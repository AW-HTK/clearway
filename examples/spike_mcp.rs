//! Spike : serveur MCP stdio minimal avec un outil `dormir(secondes)`, qui journalise tout ce qu'il reçoit.
//! Journal : `$SPIKE_DOSSIER/spike-<pid>.log`, par défaut `~/tmp/orchestrateur/spike/` (`%USERPROFILE%` sous Windows).

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use commun::systeme::{horodatage, maintenant};
use serde_json::{Value, json};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const PROGRESSION: Duration = Duration::from_secs(30);

struct Journal(Mutex<std::fs::File>);

impl Journal {
    fn ecrire(&self, texte: &str) {
        let horloge = format!("{}.{:03}", horodatage(maintenant()), millisecondes());
        if let Ok(mut f) = self.0.lock() {
            let _ = writeln!(f, "{horloge} {texte}");
            let _ = f.flush();
        }
    }
}

fn millisecondes() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis()).unwrap_or(0)
}

struct Sortie {
    journal: Arc<Journal>,
    flux: Mutex<io::Stdout>,
}

impl Sortie {
    fn envoyer(&self, message: &Value) -> bool {
        let ligne = message.to_string();
        let ok = self
            .flux
            .lock()
            .map(|mut s| writeln!(s, "{ligne}").and_then(|_| s.flush()).is_ok())
            .unwrap_or(false);
        self.journal.ecrire(&format!("→ {ligne}{}", if ok { "" } else { "  [ÉCHEC D'ÉCRITURE]" }));
        ok
    }
}

fn dossier() -> PathBuf {
    if let Some(d) = std::env::var_os("SPIKE_DOSSIER") {
        return PathBuf::from(d);
    }
    let maison = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
    PathBuf::from(maison).join("tmp").join("orchestrateur").join("spike")
}

fn sensible(nom: &str) -> bool {
    let n = nom.to_uppercase();
    ["TOKEN", "KEY", "SECRET", "PASSWORD", "AUTH", "COOKIE", "CREDENTIAL"].iter().any(|m| n.contains(m))
}

fn environnement(journal: &Journal) {
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| {
            ["CLAUDE", "CODEX", "MCP", "ANTHROPIC", "OPENAI", "WSL", "TERM_PROGRAM"].iter().any(|p| k.starts_with(p))
                || matches!(k.as_str(), "PWD" | "SHELL" | "USERPROFILE" | "HOME" | "PATH")
        })
        .collect();
    vars.sort();
    for (k, v) in vars {
        let v = if sensible(&k) { format!("<masqué, {} caractères>", v.len()) } else { v };
        journal.ecrire(&format!("ENV {k}={v}"));
    }
}

fn une_ligne(texte: &str) -> String {
    let plat = texte.trim().replace(['\r', '\n'], " ⏎ ");
    if plat.chars().count() > 400 { format!("{}…", plat.chars().take(400).collect::<String>()) } else { plat }
}

#[cfg(unix)]
fn ascendance(journal: &Journal) {
    let mut pid = std::process::id();
    for _ in 0..6 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let parent = stat.rsplit(')').next().and_then(|r| r.split_whitespace().nth(1)).and_then(|p| p.parse::<u32>().ok());
        let Some(parent) = parent.filter(|&p| p > 1) else { break };
        let ligne = std::fs::read(format!("/proc/{parent}/cmdline"))
            .map(|o| o.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        journal.ecrire(&format!("PARENT pid {parent} :: {}", une_ligne(&ligne)));
        pid = parent;
    }
}

#[cfg(windows)]
fn ascendance(journal: &Journal) {
    use std::os::windows::process::CommandExt;
    let script = format!(
        "$p={}; for($i=0;$i -lt 7 -and $p;$i++){{ $w=Get-CimInstance Win32_Process -Filter \"ProcessId=$p\"; \
         if(-not $w){{break}}; \"$($w.ProcessId) $($w.Name) :: $($w.CommandLine)\"; $p=$w.ParentProcessId }}",
        std::process::id()
    );
    match std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .creation_flags(0x0800_0000)
        .output()
    {
        Ok(o) => {
            for l in String::from_utf8_lossy(&o.stdout).lines().skip(1) {
                journal.ecrire(&format!("PARENT {}", une_ligne(l)));
            }
        }
        Err(e) => journal.ecrire(&format!("PARENT inconnu : {e}")),
    }
}

fn dormir(sortie: Arc<Sortie>, id: Value, secondes: u64, jeton: Option<Value>, annule: Arc<AtomicBool>) {
    let debut = Instant::now();
    let total = Duration::from_secs(secondes);
    let mut prochaine = PROGRESSION;
    loop {
        if annule.load(Ordering::SeqCst) {
            sortie.journal.ecrire(&format!("DORMIR {id} annulé après {} s (pas de réponse, conformément à MCP)", debut.elapsed().as_secs()));
            return;
        }
        let ecoule = debut.elapsed();
        if ecoule >= total {
            break;
        }
        if ecoule >= prochaine {
            prochaine += PROGRESSION;
            if let Some(j) = &jeton {
                sortie.envoyer(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": j,
                        "progress": ecoule.as_secs(),
                        "total": secondes,
                        "message": format!("Dort depuis {} s sur {secondes} s", ecoule.as_secs())
                    }
                }));
            } else {
                sortie.journal.ecrire(&format!("DORMIR {id} : {} s, pas de progressToken", ecoule.as_secs()));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let ok = sortie.envoyer(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "content": [{ "type": "text", "text": format!("Réveillé après {secondes} s.") }], "isError": false }
    }));
    sortie.journal.ecrire(&format!("DORMIR {id} terminé, réponse {}", if ok { "écrite" } else { "NON écrite" }));
}

fn outils() -> Value {
    json!({ "tools": [{
        "name": "dormir",
        "description": "Attend le nombre de secondes demandé puis répond. Sert à tester les longs appels d'outils.",
        "inputSchema": {
            "type": "object",
            "properties": { "secondes": { "type": "integer", "minimum": 0, "description": "Durée de l'attente" } },
            "required": ["secondes"]
        }
    }]})
}

fn main() {
    let dossier = dossier();
    let _ = std::fs::create_dir_all(&dossier);
    let chemin = dossier.join(format!("spike-{}.log", std::process::id()));
    let fichier = std::fs::OpenOptions::new().create(true).append(true).open(&chemin).unwrap_or_else(|e| {
        eprintln!("spike : journal {} impossible : {e}", chemin.display());
        std::process::exit(2)
    });
    let journal = Arc::new(Journal(Mutex::new(fichier)));
    journal.ecrire(&format!(
        "DÉBUT pid {} · argv {:?} · dossier courant {:?}",
        std::process::id(),
        std::env::args().collect::<Vec<_>>(),
        std::env::current_dir().ok()
    ));
    environnement(&journal);
    {
        let journal = journal.clone();
        std::thread::spawn(move || ascendance(&journal));
    }

    let sortie = Arc::new(Sortie { journal: journal.clone(), flux: Mutex::new(io::stdout()) });
    let mut en_cours: HashMap<String, Arc<AtomicBool>> = HashMap::new();
    let entree = io::stdin();
    for ligne in entree.lock().lines() {
        let ligne = match ligne {
            Ok(l) => l,
            Err(e) => {
                journal.ecrire(&format!("ERREUR de lecture de stdin : {e}"));
                break;
            }
        };
        journal.ecrire(&format!("← {ligne}"));
        let Ok(m) = serde_json::from_str::<Value>(&ligne) else {
            journal.ecrire("ligne illisible");
            continue;
        };
        let id = m.get("id").cloned();
        let methode = m["method"].as_str().unwrap_or("");
        let params = &m["params"];
        match (methode, id) {
            ("initialize", Some(id)) => {
                let demandee = params["protocolVersion"].as_str().unwrap_or("");
                let version = if VERSIONS.contains(&demandee) { demandee } else { VERSIONS[1] };
                journal.ecrire(&format!(
                    "INITIALIZE client {} · version demandée {demandee:?}, retenue {version} · capacités {}",
                    params["clientInfo"],
                    params["capabilities"]
                ));
                sortie.envoyer(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "spike", "version": "0.1.0" },
                        "instructions": "Serveur d'essai : l'outil dormir(secondes) attend puis répond."
                    }
                }));
            }
            ("tools/list", Some(id)) => {
                sortie.envoyer(&json!({ "jsonrpc": "2.0", "id": id, "result": outils() }));
            }
            ("tools/call", Some(id)) if params["name"] == "dormir" => {
                let secondes = params["arguments"]["secondes"].as_u64().unwrap_or(0);
                let jeton = params["_meta"].get("progressToken").cloned();
                journal.ecrire(&format!("DORMIR {id} : {secondes} s, progressToken {jeton:?}, _meta {}", params["_meta"]));
                let annule = Arc::new(AtomicBool::new(false));
                en_cours.insert(id.to_string(), annule.clone());
                let sortie = sortie.clone();
                std::thread::spawn(move || dormir(sortie, id, secondes, jeton, annule));
            }
            ("tools/call", Some(id)) => {
                sortie.envoyer(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": [{ "type": "text", "text": "Outil inconnu." }], "isError": true }
                }));
            }
            ("ping", Some(id)) => {
                sortie.envoyer(&json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
            }
            ("notifications/cancelled", None) => {
                let cible = params["requestId"].to_string();
                journal.ecrire(&format!("ANNULATION de {cible} · raison {}", params["reason"]));
                if let Some(a) = en_cours.get(&cible) {
                    a.store(true, Ordering::SeqCst);
                }
            }
            (_, None) => {}
            (autre, Some(id)) => {
                sortie.envoyer(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("méthode inconnue : {autre}") }
                }));
            }
        }
    }
    let encore = en_cours.values().filter(|a| !a.load(Ordering::SeqCst)).count();
    journal.ecrire(&format!("FIN DE STDIN · {encore} appel(s) dormir lancé(s) sans annulation ; sortie dans 2 s"));
    std::thread::sleep(Duration::from_secs(2));
    journal.ecrire("SORTIE");
}
