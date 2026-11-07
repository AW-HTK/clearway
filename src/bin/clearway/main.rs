#![windows_subsystem = "windows"]

#[cfg(unix)]
mod client;
mod hook;
mod mcp_server;
mod measures;
mod server;
mod transport;
mod ui;
#[cfg(windows)]
mod updater;
#[cfg(windows)]
mod window;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clearway::config::Config;
use clearway::protocol::{DEFAULT_PIPE, LEGACY_PIPE};

use server::{Shared, Server, log_line};

const MEASURE_INTERVAL: Duration = Duration::from_secs(2);

const USAGE: &str =
    "clearway serve [--dir <path>] [--test | --test-icon]  |  clearway relay [--pipe <name>]  |  clearway mcp [--max-s <s>]";

fn state_root() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"),
    }
}

fn default_dir() -> PathBuf {
    state_root().join("clearway")
}

/// First start after the rename: copies the pre-Clearway state folder into the new one (the files are then
/// translated where they are read) and leaves a note in the old folder, which stays in place for a rollback.
fn migrate_old_dir(old: &Path, new: &Path) -> Option<String> {
    if new.join("state.json").exists() || new.join("etat.json").exists() || !old.join("etat.json").exists() {
        return None;
    }
    std::fs::create_dir_all(new).ok()?;
    for (from, to) in [("etat.json", "etat.json"), ("config.toml", "config.toml"), ("journal.log", "journal-orchestrateur.log")] {
        let source = old.join(from);
        if source.exists() && !new.join(to).exists() {
            std::fs::copy(&source, new.join(to)).ok()?;
        }
    }
    let note = clearway::tr!(
        "This folder was migrated to {} on {}. Clearway no longer reads it; it is kept to go back to the previous version.\n",
        "Ce dossier a été migré vers {} le {}. Clearway ne le lit plus ; il est gardé pour revenir à la version précédente.\n",
        new.display(),
        clearway::system::timestamp(clearway::system::unix_now())
    );
    let _ = std::fs::write(old.join("MIGRATED-TO-CLEARWAY.txt"), note);
    Some(clearway::tr!(
        "state folder migrated from {} (old log kept as journal-orchestrateur.log).",
        "dossier d'état migré depuis {} (ancien journal gardé sous journal-orchestrateur.log).",
        old.display()
    ))
}

fn io_busy() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::AddrInUse, clearway::tr!("previous version running", "version précédente en service"))
}

fn failure(dir: &Path, title: &str, text: &str, code: i32) -> i32 {
    log_line(dir, &format!("ERROR {}", clearway::tr!("{title}: {text}", "{title} : {text}")));
    ui::warn(title, text);
    code
}

fn already_running(pipe: &str) -> String {
    format!(
        "INFO {}",
        clearway::tr!(
            "Already running: Clearway is running on pipe {pipe}; this copy stops.",
            "Déjà lancé : Clearway tourne sur le tube {pipe} ; cette copie s'arrête."
        )
    )
}

fn relayer(args: &[String]) -> i32 {
    let pipe = match args {
        [] => DEFAULT_PIPE,
        [option, name] if option == "--pipe" || option == "--tube" => name.as_str(),
        _ => {
            eprintln!("{}", clearway::tr!("usage: {USAGE}", "usage : {USAGE}"));
            return 2;
        }
    };
    transport::relay(pipe)
}

fn serve(args: &[String]) -> i32 {
    let mut dir = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--dir" | "--dossier" => dir = iter.next().map(PathBuf::from),
            "--test" => ui::test_mode(false),
            "--test-icon" | "--test-icone" => ui::test_mode(true),
            "--version" => {
                println!("clearway {}", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            _ => {
                ui::warn(
                    &clearway::tr!("Unknown argument", "Argument inconnu"),
                    &clearway::tr!("{a}; usage: {USAGE}", "{a} ; usage : {USAGE}"),
                );
                return 2;
            }
        }
    }
    let migrated = if dir.is_none() { migrate_old_dir(&state_root().join("orchestrateur"), &default_dir()) } else { None };
    let dir = dir.unwrap_or_else(default_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        ui::warn(
            &clearway::tr!("State folder unreachable", "Dossier d'état inaccessible"),
            &clearway::tr!("{}: {e}", "{} : {e}", dir.display()),
        );
        return 2;
    }
    let config = match Config::load(&dir, true) {
        Ok(c) => c,
        Err(e) => return failure(&dir, &clearway::tr!("Invalid configuration", "Configuration invalide"), &e, 2),
    };
    if let Some(lang) = config.language {
        clearway::i18n::set(lang);
    }
    if let Some(text) = migrated {
        log_line(&dir, &format!("INFO {text}"));
    }
    // An extra copy (scheduled task and client relaunching at once) is harmless: log only.
    let Ok(_instance) = transport::single_instance(&config.pipe) else {
        log_line(&dir, &already_running(&config.pipe));
        return 3;
    };
    let listening = match transport::listen(&config.pipe) {
        Ok(e) => e,
        Err(e) if cfg!(unix) && e.kind() == std::io::ErrorKind::AddrInUse => {
            log_line(&dir, &already_running(&config.pipe));
            return 3;
        }
        Err(e) => {
            let text = clearway::tr!(
                "Pipe {} unavailable ({e}). Change `tube` in {}.",
                "Tube {} indisponible ({e}). Change « tube » dans {}.",
                config.pipe,
                dir.join("config.toml").display()
            );
            return failure(&dir, &clearway::tr!("Pipe already taken", "Tube déjà pris"), &text, 4);
        }
    };
    let (mut sensor, measures) = measures::first_measures(&dir);
    if !ui::start(Some(dir.join("clearway.log"))) {
        log_line(
            &dir,
            &format!(
                "ERROR {}",
                clearway::tr!(
                    "tray icon: could not create the notification icon, carrying on without it.",
                    "icône : création de l'icône de notification impossible, on continue sans."
                )
            ),
        );
    }
    let config_legacy_pipe = config.legacy_pipe && config.pipe == DEFAULT_PIPE;
    #[cfg(windows)]
    if config.update_check_daily && !ui::in_test() {
        updater::daily(dir.clone());
    }
    let server = match Server::start(dir.clone(), config, measures) {
        Ok(s) => s,
        Err(e) => return failure(&dir, &clearway::tr!("Cannot start", "Démarrage impossible"), &e, 2),
    };
    let shared = Arc::new(Shared::new(server));
    ui::install(shared.clone());

    let p = shared.clone();
    std::thread::spawn(move || transport::serve(listening, p));
    // Clients installed before the rename still use the old pipe: served as long as `legacy_pipe` is on.
    let mut _legacy_guard = None;
    if config_legacy_pipe {
        let served = transport::legacy_instance()
            .map_err(|()| io_busy())
            .and_then(|guard| Ok((guard, transport::listen(LEGACY_PIPE)?)));
        match served {
            Ok((guard, legacy)) => {
                _legacy_guard = Some(guard);
                let p = shared.clone();
                std::thread::spawn(move || transport::serve(legacy, p));
            }
            Err(e) => log_line(
                &dir,
                &format!(
                    "INFO {}",
                    clearway::tr!(
                        "old pipe {LEGACY_PIPE} not served ({e}): the previous version may still be running.",
                        "ancien tube {LEGACY_PIPE} non servi ({e}) : la version précédente tourne peut-être encore."
                    )
                ),
            ),
        }
    }
    let p = shared.clone();
    std::thread::spawn(move || {
        loop {
            p.tick(sensor.read());
            std::thread::sleep(MEASURE_INTERVAL);
        }
    });

    ui::run_loop();
    log_line(
        &shared.dir(),
        &format!("STOP{}", clearway::tr!(": tray icon removed, process ending.", " : icône retirée, fin du processus.")),
    );
    0
}

/// One binary: server and relay (Windows), client (WSL). Called as `charge` or `orchestrateur`, it keeps the old
/// command names and the French messages so that running sessions notice nothing.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let program = std::path::Path::new(args.first().map_or("", String::as_str))
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let rest = args.get(1..).unwrap_or_default();
    let code = match rest.first().map(String::as_str) {
        Some("serve") => serve(&rest[1..]),
        Some("relay" | "relais") => relayer(&rest[1..]),
        Some("mcp") => mcp_server::main(&rest[1..]),
        Some("hook") => hook::main(&rest[1..]),
        _ if cfg!(windows) || program == "orchestrateur" => serve(rest),
        #[cfg(unix)]
        _ => client::main(rest, program == "charge"),
        #[cfg(not(unix))]
        _ => serve(rest),
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_state_folder_is_copied_once_with_a_note() {
        let base = std::env::temp_dir().join(format!("clearway-migration-{}", std::process::id()));
        let (old, new) = (base.join("orchestrateur"), base.join("clearway"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("etat.json"), r#"{"epoque":3,"compteur":7,"vmmem_pid":null,"tickets":[]}"#).unwrap();
        std::fs::write(old.join("config.toml"), "tube = \"orchestrateur\"\n").unwrap();
        std::fs::write(old.join("journal.log"), "2026-11-06 23:17:30 DÉMARRAGE époque 12\n").unwrap();
        assert!(migrate_old_dir(&old, &new).is_some());
        assert!(new.join("etat.json").exists() && new.join("config.toml").exists());
        assert!(std::fs::read_to_string(new.join("journal-orchestrateur.log")).unwrap().contains("DÉMARRAGE"));
        assert!(old.join("MIGRATED-TO-CLEARWAY.txt").exists() && old.join("etat.json").exists(), "old folder kept");
        assert!(migrate_old_dir(&old, &new).is_none(), "only once");
        let config = Config::load(&new, false).unwrap();
        assert_eq!(config.pipe, DEFAULT_PIPE);
        assert!(std::fs::read_to_string(new.join("config.toml")).unwrap().contains("pipe = \"clearway\""));
        assert!(new.join("config.toml.before-clearway").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
