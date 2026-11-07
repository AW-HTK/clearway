//! New releases (Windows server): looks for one when asked from the tray menu, or once a day if `update.check =
//! "daily"`, and offers it. Installing is always the user's click, then the setup's own confirmation.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clearway::tr;
use clearway::update::{self, Release};

use crate::server::log_line;
use crate::ui;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FIRST_DAILY_CHECK: Duration = Duration::from_secs(5 * 60);
const DAY: Duration = Duration::from_secs(24 * 3600);

static AVAILABLE: Mutex<Option<Release>> = Mutex::new(None);
static NOTIFIED: Mutex<Option<String>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn available() -> Option<Release> {
    AVAILABLE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn fetch(url: &str, json: bool) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("curl.exe");
    cmd.args(["-fsSL", "--max-time", "60", "-A", concat!("clearway/", env!("CARGO_PKG_VERSION"))]);
    if json {
        cmd.args(["-H", "Accept: application/vnd.github+json"]);
    }
    let out = cmd
        .arg(url)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| tr!("curl.exe: {e}", "curl.exe : {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(out.stdout)
}

fn latest() -> Result<Option<Release>, String> {
    let answer: serde_json::Value = serde_json::from_slice(&fetch(update::LATEST_RELEASE_API, true)?).map_err(|e| e.to_string())?;
    Ok(update::release_from_api(&answer).filter(|r| update::is_newer(&r.version, env!("CARGO_PKG_VERSION"))))
}

/// Asks GitHub for the latest release. `manual`: also says when there is nothing new or the check failed.
pub fn check(dir: &Path, manual: bool) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        let result = latest();
        BUSY.store(false, Ordering::SeqCst);
        match result {
            Ok(Some(release)) => {
                let version = release.version.clone();
                *AVAILABLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(release);
                let mut notified = NOTIFIED.lock().unwrap_or_else(|e| e.into_inner());
                if manual || notified.as_deref() != Some(version.as_str()) {
                    *notified = Some(version.clone());
                    log_line(&dir, &tr!("UPDATE Clearway {version} is available.", "MISE À JOUR Clearway {version} est disponible."));
                    ui::update_balloon(
                        &tr!("Clearway {version} is available", "Clearway {version} est disponible"),
                        &tr!(
                            "Click here, or use the tray menu, to install it. The setup shows what it changes and asks first.",
                            "Cliquez ici, ou passez par le menu de l'icône, pour l'installer. L'installation montre ce qu'elle change et demande d'abord."
                        ),
                    );
                }
            }
            Ok(None) if manual => ui::balloon(
                &tr!("Clearway is up to date", "Clearway est à jour"),
                &tr!("Version {} is the latest.", "La version {} est la dernière.", env!("CARGO_PKG_VERSION")),
                false,
            ),
            Ok(None) => {}
            Err(e) => {
                log_line(&dir, &tr!("UPDATE check failed: {e}", "MISE À JOUR recherche impossible : {e}"));
                if manual {
                    ui::balloon(&tr!("Update check failed", "Recherche de mise à jour impossible"), &e, true);
                }
            }
        }
    });
}

/// Once a day, a few minutes after start; only offers, never installs.
pub fn daily(dir: PathBuf) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_DAILY_CHECK);
        loop {
            check(&dir, false);
            std::thread::sleep(DAY);
        }
    });
}

fn download(release: &Release) -> Result<PathBuf, String> {
    let sums = fetch(&release.sums_url, false)?;
    let signature = String::from_utf8(fetch(&release.signature_url, false)?).map_err(|e| e.to_string())?;
    if !update::signed_by(update::RELEASE_PUBLIC_KEY, &sums, &signature) {
        return Err(tr!("SHA256SUMS is not signed by Clearway's key", "SHA256SUMS n'est pas signé par la clé de Clearway"));
    }
    let expected = update::expected_sum(&String::from_utf8_lossy(&sums), update::SETUP_ASSET)
        .ok_or_else(|| tr!("SHA256SUMS has no line for the setup", "SHA256SUMS n'a pas de ligne pour l'installateur"))?;
    let setup = fetch(&release.setup_url, false)?;
    let actual = update::sha256_hex(&setup);
    if actual != expected {
        return Err(tr!("setup SHA-256 {actual}, expected {expected}", "SHA-256 de l'installateur {actual}, attendu {expected}"));
    }
    let folder = std::env::temp_dir().join(format!("clearway-update-{}", release.version));
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join(update::SETUP_ASSET);
    std::fs::write(&path, setup).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Downloads the offered setup, checks its signature and SHA-256, then opens it in its own console window.
pub fn install(dir: &Path) {
    let Some(release) = available() else { return };
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        let result = download(&release);
        BUSY.store(false, Ordering::SeqCst);
        match result {
            Ok(setup) => {
                log_line(&dir, &tr!("UPDATE setup {} checked, opened.", "MISE À JOUR installateur {} vérifié, ouvert.", release.version));
                if !ui::open(&setup) {
                    ui::balloon(&tr!("Cannot open the setup", "Impossible d'ouvrir l'installateur"), &setup.display().to_string(), true);
                }
            }
            Err(e) => {
                log_line(&dir, &tr!("UPDATE refused: {e}", "MISE À JOUR refusée : {e}"));
                ui::balloon(&tr!("Update not installed", "Mise à jour non installée"), &e, true);
            }
        }
    });
}
