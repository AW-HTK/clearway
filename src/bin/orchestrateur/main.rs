#![windows_subsystem = "windows"]

#[cfg(windows)]
mod fenetre;
mod mesures;
mod serveur;
mod transport;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use commun::config::Config;
use commun::protocole::TUBE_DEFAUT;

use serveur::{Partage, Serveur, journaliser};

const INTERVALLE_MESURE: Duration = Duration::from_secs(2);

const USAGE: &str = "orchestrateur [--dossier <chemin>] [--test | --test-icone]  |  orchestrateur relais [--tube <nom>]";

fn dossier_defaut() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(d) => PathBuf::from(d).join("orchestrateur"),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/orchestrateur"),
    }
}

fn echec(dossier: &Path, titre: &str, texte: &str, code: i32) -> i32 {
    journaliser(dossier, &format!("ERREUR {titre} : {texte}"));
    ui::avertir(titre, texte);
    code
}

fn relayer(args: &[String]) -> i32 {
    let tube = match args {
        [] => TUBE_DEFAUT,
        [option, nom] if option == "--tube" => nom.as_str(),
        _ => {
            eprintln!("usage : {USAGE}");
            return 2;
        }
    };
    transport::relais(tube)
}

fn lancer() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "relais") {
        return relayer(&args[1..]);
    }
    let mut dossier = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--dossier" => dossier = iter.next().map(PathBuf::from),
            "--test" => ui::mode_test(false),
            "--test-icone" => ui::mode_test(true),
            "--version" => {
                println!("orchestrateur {}", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            _ => {
                ui::avertir("Argument inconnu", &format!("{a} ; usage : {USAGE}"));
                return 2;
            }
        }
    }
    let dossier = dossier.unwrap_or_else(dossier_defaut);
    if let Err(e) = std::fs::create_dir_all(&dossier) {
        ui::avertir("Dossier d'état inaccessible", &format!("{} : {e}", dossier.display()));
        return 2;
    }
    let config = match Config::charger(&dossier, true) {
        Ok(c) => c,
        Err(e) => return echec(&dossier, "Configuration invalide", &e, 2),
    };
    // Copie en trop (tâche planifiée et client qui relancent en même temps) : sans gravité, journal seulement.
    let Ok(_instance) = transport::instance_unique(&config.tube) else {
        journaliser(
            &dossier,
            &format!("INFO Déjà lancé : un orchestrateur tourne sur le tube {} ; cette copie s'arrête.", config.tube),
        );
        return 3;
    };
    let ecoute = match transport::ecouter(&config.tube) {
        Ok(e) => e,
        Err(e) if cfg!(unix) && e.kind() == std::io::ErrorKind::AddrInUse => {
            journaliser(
                &dossier,
                &format!("INFO Déjà lancé : un orchestrateur tourne sur le tube {} ; cette copie s'arrête.", config.tube),
            );
            return 3;
        }
        Err(e) => {
            let texte = format!(
                "Tube {} indisponible ({e}). Change « tube » dans {}.",
                config.tube,
                dossier.join("config.toml").display()
            );
            return echec(&dossier, "Tube déjà pris", &texte, 4);
        }
    };
    let (mut capteur, mesures) = mesures::premieres(&dossier);
    if !ui::demarrer(Some(dossier.join("journal.log"))) {
        journaliser(&dossier, "ERREUR icône : création de l'icône de notification impossible, on continue sans.");
    }
    let serveur = match Serveur::demarrer(dossier.clone(), config, mesures) {
        Ok(s) => s,
        Err(e) => return echec(&dossier, "Démarrage impossible", &e, 2),
    };
    let partage = Arc::new(Partage::nouveau(serveur));
    ui::installer(partage.clone());

    let p = partage.clone();
    std::thread::spawn(move || transport::servir(ecoute, p));
    let p = partage.clone();
    std::thread::spawn(move || {
        loop {
            p.tic(capteur.lire());
            std::thread::sleep(INTERVALLE_MESURE);
        }
    });

    ui::boucle();
    journaliser(&partage.dossier(), "ARRÊT : icône retirée, fin du processus.");
    0
}

fn main() {
    std::process::exit(lancer());
}
