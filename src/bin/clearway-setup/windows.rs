use std::io::{BufRead, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clearway::setup::{WSL_PROBE, probe_result, sh_quote, wsl_names};
use clearway::tr;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MARKETPLACE: &str = "AW-HTK/clearway";
const PLUGIN: &str = "clearway@clearway";

const FILES: [(&str, &[u8]); 6] = [
    ("clearway.exe", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/clearway.exe"))),
    ("clearway-x86_64-linux", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/clearway-x86_64-linux"))),
    ("install.ps1", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/install.ps1"))),
    ("install.sh", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/install.sh"))),
    ("SHA256SUMS", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/SHA256SUMS"))),
    ("VERSION", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/VERSION"))),
];

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Claude,
    Codex,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Claude => "Claude Code",
            Tool::Codex => "Codex",
        }
    }

    /// Shell commands, `{t}` being the program: add the marketplace, refresh it, install or update the plugin.
    fn commands(self) -> [String; 4] {
        match self {
            Tool::Claude => [
                format!("{{t}} plugin marketplace add {MARKETPLACE}"),
                "{t} plugin marketplace update clearway".into(),
                format!("{{t}} plugin install {PLUGIN} --scope user"),
                format!("{{t}} plugin update {PLUGIN}"),
            ],
            Tool::Codex => [
                format!("{{t}} plugin marketplace add {MARKETPLACE}"),
                "{t} plugin marketplace upgrade clearway".into(),
                format!("{{t}} plugin add {PLUGIN}"),
                String::new(),
            ],
        }
    }
}

struct Found {
    /// `None`: Windows itself.
    distro: Option<String>,
    tools: Vec<(Tool, String)>,
}

fn quiet(cmd: &mut Command) -> Option<(bool, Vec<u8>)> {
    let out = cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    Some((out.status.success(), out.stdout))
}

fn windows_tool(program: &str) -> Option<String> {
    let (ok, _) = quiet(Command::new("cmd.exe").args(["/d", "/c", "where", program]))?;
    ok.then(|| program.to_owned())
}

fn distros(running_only: bool) -> Vec<String> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-l", "-q"]);
    if running_only {
        cmd.arg("--running");
    }
    quiet(&mut cmd).map(|(_, out)| wsl_names(&out)).unwrap_or_default()
}

/// Desktop apps (Microsoft Store packages) found, by display name.
fn desktop_apps() -> Vec<&'static str> {
    let script = "Get-AppxPackage | ForEach-Object Name";
    let out = quiet(Command::new("powershell.exe").args(["-NoProfile", "-Command", script])).map(|(_, o)| o).unwrap_or_default();
    let names = String::from_utf8_lossy(&out).to_lowercase();
    [("claude", "Claude"), ("openai.codex", "Codex")]
        .into_iter()
        .filter(|(package, _)| names.lines().any(|l| l.trim() == *package))
        .map(|(_, app)| app)
        .collect()
}

fn scan() -> (Vec<Found>, Vec<String>) {
    let mut found = vec![Found {
        distro: None,
        tools: [(Tool::Claude, "claude"), (Tool::Codex, "codex")]
            .into_iter()
            .filter_map(|(t, p)| windows_tool(p).map(|p| (t, p)))
            .collect(),
    }];
    let running = distros(true);
    for d in &running {
        let out = quiet(Command::new("wsl.exe").args(["-d", d, "--exec", "sh", "-c", WSL_PROBE])).map(|(_, o)| o).unwrap_or_default();
        let (claude, codex) = probe_result(&String::from_utf8_lossy(&out));
        let tools = [(Tool::Claude, claude), (Tool::Codex, codex)].into_iter().filter_map(|(t, p)| p.map(|p| (t, p))).collect();
        found.push(Found { distro: Some(d.clone()), tools });
    }
    let stopped = distros(false).into_iter().filter(|d| !running.contains(d)).collect();
    (found, stopped)
}

fn place(f: &Found) -> String {
    f.distro.as_ref().map_or_else(|| "Windows".to_owned(), |d| format!("WSL {d}"))
}

fn print_plan(found: &[Found], stopped: &[String], apps: &[&str]) {
    println!("{}", tr!("Clearway {} will be installed for this user:", "Clearway {} va être installé pour cet utilisateur :", env!("CARGO_PKG_VERSION")));
    println!(
        "{}",
        tr!(
            "  [x] Windows: clearway.exe in %LOCALAPPDATA%\\Programs\\clearway, on your PATH, started at logon (tray icon).",
            "  [x] Windows : clearway.exe dans %LOCALAPPDATA%\\Programs\\clearway, dans votre PATH, lancé à l'ouverture de session (icône)."
        )
    );
    for f in found.iter().filter(|f| f.distro.is_some()) {
        println!("{}", tr!("  [x] {}: clearway client in ~/.local/bin (distribution running).", "  [x] {} : client clearway dans ~/.local/bin (distribution démarrée).", place(f)));
    }
    for f in found {
        for (tool, path) in &f.tools {
            println!(
                "{}",
                tr!(
                    "  [x] {} on {} ({path} found): Clearway plugin (wait tool, heavy-task guidance, guard before heavy commands).",
                    "  [x] {} sur {} ({path} trouvé) : plugin Clearway (outil d'attente, consigne des tâches lourdes, garde avant les commandes lourdes).",
                    tool.name(),
                    place(f)
                )
            );
        }
    }
    if found.iter().all(|f| f.tools.is_empty()) {
        println!("{}", tr!("  [ ] No Claude Code or Codex found: install one, then run this setup again.", "  [ ] Aucun Claude Code ni Codex trouvé : installez-en un, puis relancez cette installation."));
    }
    let windows_cli = found.iter().find(|f| f.distro.is_none()).map_or(&[][..], |f| &f.tools[..]);
    for app in apps {
        let tool = if *app == "Claude" { Tool::Claude } else { Tool::Codex };
        if windows_cli.iter().all(|(t, _)| *t != tool) {
            println!(
                "{}",
                tr!(
                    "  [ ] {app} desktop app (Windows): no command line to install from; in the app, add the plugin marketplace {MARKETPLACE}, then install the clearway plugin.",
                    "  [ ] Application {app} (Windows) : pas de ligne de commande pour l'installer ; dans l'application, ajoutez le marketplace de plugins {MARKETPLACE}, puis installez le plugin clearway."
                )
            );
        }
    }
    for d in stopped {
        println!("{}", tr!("  [ ] WSL {d}: stopped, not touched. Start it and run this setup again to include it.", "  [ ] WSL {d} : arrêtée, non modifiée. Démarrez-la et relancez cette installation pour l'inclure."));
    }
}

fn confirm() -> bool {
    print!("{}", tr!("\nInstall? [Y/n] ", "\nInstaller ? [O/n] "));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    !matches!(line.trim().chars().next(), Some('n' | 'N'))
}

fn unpack() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("clearway-setup-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in FILES {
        std::fs::write(dir.join(name), bytes)?;
    }
    Ok(dir)
}

fn clean(dir: &Path) {
    for (name, _) in FILES {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let _ = std::fs::remove_dir(dir);
}

fn step(title: &str, cmd: &mut Command) -> bool {
    println!("\n== {title}");
    let ok = cmd.status().is_ok_and(|s| s.success());
    if !ok {
        println!("{}", tr!("   FAILED: {title}", "   ÉCHEC : {title}"));
    }
    ok
}

fn windows_part(dir: &Path) -> bool {
    let script = dir.join("install.ps1");
    step(
        &tr!("Windows: clearway.exe", "Windows : clearway.exe"),
        Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .env("CLEARWAY_SOURCE", dir)
            .env("CLEARWAY_QUIET", "1"),
    )
}

fn wsl_part(distro: &str, dir: &Path) -> bool {
    step(
        &tr!("WSL {distro}: clearway client", "WSL {distro} : client clearway"),
        Command::new("wsl.exe")
            .args(["-d", distro, "--exec", "sh", "-c", r#"src=$(wslpath -u "$1") && CLEARWAY_SOURCE="$src" sh "$src/install.sh""#, "clearway-setup"])
            .arg(dir),
    )
}

/// Runs the plugin commands of `tool`; only the install command must succeed.
fn plugin_part(f: &Found, tool: Tool, path: &str) -> bool {
    println!("\n== {}", tr!("{} on {}: Clearway plugin", "{} sur {} : plugin Clearway", tool.name(), place(f)));
    let [add, refresh, install, update] = tool.commands();
    let mut ok = true;
    for (i, line) in [add, refresh, install, update].iter().enumerate().filter(|(_, l)| !l.is_empty()) {
        let status = match &f.distro {
            None => Command::new("cmd.exe").args(["/d", "/c", &line.replace("{t}", path)]).status(),
            Some(d) => {
                let line = line.replace("{t}", &sh_quote(path));
                Command::new("wsl.exe").args(["-d", d, "--exec", "sh", "-c", r#"exec "${SHELL:-sh}" -lc "$1""#, "clearway-setup", &line]).status()
            }
        };
        if i == 2 && !status.is_ok_and(|s| s.success()) {
            println!("{}", tr!("   FAILED: plugin not installed", "   ÉCHEC : plugin non installé"));
            ok = false;
        }
    }
    ok
}

fn pause() {
    print!("{}", tr!("\nPress Enter to close.", "\nAppuyez sur Entrée pour fermer."));
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().lock().read_line(&mut String::new());
}

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let yes = args.iter().any(|a| a == "--yes");
    let dry = args.iter().any(|a| a == "--dry-run");
    let interactive = args.is_empty();
    let lang = match clearway::i18n::current() {
        clearway::i18n::Lang::Fr => "fr",
        clearway::i18n::Lang::En => "en",
    };
    let wslenv = std::env::var("WSLENV").map_or_else(|_| "CLEARWAY_LANG".to_owned(), |v| format!("{v}:CLEARWAY_LANG"));
    // Single-threaded here: no other thread reads the environment yet.
    unsafe {
        std::env::set_var("CLEARWAY_LANG", lang);
        std::env::set_var("WSLENV", wslenv);
    }

    println!("{}", tr!("Looking for WSL, Claude Code and Codex (nothing is changed yet)…\n", "Recherche de WSL, Claude Code et Codex (rien n'est encore modifié)…\n"));
    let (found, stopped) = scan();
    print_plan(&found, &stopped, &desktop_apps());
    if dry || !(yes || confirm()) {
        if !dry {
            println!("{}", tr!("Nothing installed.", "Rien n'a été installé."));
        }
        if interactive {
            pause();
        }
        return 0;
    }

    let dir = match unpack() {
        Ok(d) => d,
        Err(e) => {
            println!("{}", tr!("Cannot write the setup files: {e}", "Impossible d'écrire les fichiers d'installation : {e}"));
            if interactive {
                pause();
            }
            return 1;
        }
    };
    let mut failures = Vec::new();
    if !windows_part(&dir) {
        failures.push("Windows".to_owned());
    }
    for f in found.iter() {
        if let Some(d) = &f.distro
            && !wsl_part(d, &dir)
        {
            failures.push(place(f));
        }
    }
    for f in &found {
        for (tool, path) in &f.tools {
            if !plugin_part(f, *tool, path) {
                failures.push(format!("{} ({})", tool.name(), place(f)));
            }
        }
    }
    clean(&dir);

    println!();
    if failures.is_empty() {
        println!("{}", tr!("Clearway is installed.", "Clearway est installé."));
    } else {
        println!("{}", tr!("Done, with failures: {}.", "Terminé, avec des échecs : {}.", failures.join(", ")));
    }
    println!(
        "{}",
        tr!(
            "Open a new terminal, then restart your Claude Code and Codex sessions so that they load the plugin. In Codex, \
             type /hooks once and trust Clearway's hook.",
            "Ouvrez un nouveau terminal, puis relancez vos sessions Claude Code et Codex pour qu'elles chargent le plugin. \
             Dans Codex, tapez /hooks une fois et approuvez le hook de Clearway."
        )
    );
    if interactive {
        pause();
    }
    i32::from(!failures.is_empty())
}
