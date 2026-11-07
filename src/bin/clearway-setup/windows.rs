use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use clearway::config::Config;
use clearway::setup::{WSL_PROBE, probe_result, sh_quote, wsl_names};
use clearway::tr;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MARKETPLACE: &str = "AW-HTK/clearway";
const PLUGIN: &str = "clearway@clearway";
/// Among a tool's plugin commands, the one that must succeed (install).
const INSTALL_COMMAND: usize = 2;

const FILES: [(&str, &[u8]); 6] = [
    ("clearway.exe", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/clearway.exe"))),
    ("clearway-x86_64-linux", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/clearway-x86_64-linux"))),
    ("install.ps1", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/install.ps1"))),
    ("install.sh", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/install.sh"))),
    ("SHA256SUMS", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/SHA256SUMS"))),
    ("VERSION", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/VERSION"))),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    Claude,
    Codex,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Claude => "Claude Code",
            Tool::Codex => "Codex",
        }
    }

    /// Shell commands, `{t}` being the program: add the marketplace, refresh it, install or update the plugin.
    fn commands(self) -> Vec<String> {
        match self {
            Tool::Claude => vec![
                format!("{{t}} plugin marketplace add {MARKETPLACE}"),
                "{t} plugin marketplace update clearway".into(),
                format!("{{t}} plugin install {PLUGIN} --scope user"),
                format!("{{t}} plugin update {PLUGIN}"),
            ],
            Tool::Codex => vec![
                format!("{{t}} plugin marketplace add {MARKETPLACE}"),
                "{t} plugin marketplace upgrade clearway".into(),
                format!("{{t}} plugin add {PLUGIN}"),
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Windows,
    Wsl(String),
    Plugin { distro: Option<String>, tool: Tool, path: String },
}

/// One line of the plan: an action the user may untick, or an information without action.
#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    pub detail: String,
    pub action: Option<Action>,
    pub checked: bool,
    /// Cannot be unticked (Clearway itself).
    pub locked: bool,
    /// Index of the row this one needs (a plugin needs its distribution's client).
    pub needs: Option<usize>,
}

pub enum Event {
    Step { index: usize, total: usize, title: String },
    Line(String),
    StepDone { ok: bool },
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

fn place(distro: Option<&str>) -> String {
    distro.map_or_else(|| "Windows".to_owned(), |d| format!("WSL {d}"))
}

fn info(label: String, detail: String) -> Row {
    Row { label, detail, action: None, checked: false, locked: true, needs: None }
}

/// Looks at the machine without changing anything and lists what the setup proposes.
pub fn plan() -> Vec<Row> {
    let mut rows = vec![Row {
        label: tr!("Clearway for Windows", "Clearway pour Windows"),
        detail: tr!(
            "The queue itself, with its icon next to the clock. Required.",
            "La file elle-même, avec son icône près de l'horloge. Indispensable."
        ),
        action: Some(Action::Windows),
        checked: true,
        locked: true,
        needs: None,
    }];
    let plugin_detail = || {
        tr!(
            "Waits for its turn before a heavy task; a guard stops the ones started without waiting.",
            "Attend son tour avant une tâche lourde ; une garde bloque celles lancées sans attendre."
        )
    };
    for (tool, program) in [(Tool::Claude, "claude"), (Tool::Codex, "codex")] {
        if let Some(path) = windows_tool(program) {
            rows.push(Row {
                label: format!("{} (Windows)", tool.name()),
                detail: plugin_detail(),
                action: Some(Action::Plugin { distro: None, tool, path }),
                checked: true,
                locked: false,
                needs: Some(0),
            });
        }
    }
    let running = distros(true);
    for d in &running {
        let out = quiet(Command::new("wsl.exe").args(["-d", d, "--exec", "sh", "-c", WSL_PROBE])).map(|(_, o)| o).unwrap_or_default();
        let (claude, codex) = probe_result(&String::from_utf8_lossy(&out));
        let client = rows.len();
        rows.push(Row {
            label: tr!("Clearway in WSL {d}", "Clearway dans WSL {d}"),
            detail: tr!(
                "The clearway command, used by the sessions of this distribution.",
                "La commande clearway, utilisée par les sessions de cette distribution."
            ),
            action: Some(Action::Wsl(d.clone())),
            checked: true,
            locked: false,
            needs: None,
        });
        for (tool, path) in [(Tool::Claude, claude), (Tool::Codex, codex)] {
            if let Some(path) = path {
                rows.push(Row {
                    label: format!("{} ({})", tool.name(), place(Some(d))),
                    detail: plugin_detail(),
                    action: Some(Action::Plugin { distro: Some(d.clone()), tool, path }),
                    checked: true,
                    locked: false,
                    needs: Some(client),
                });
            }
        }
    }
    let has_windows_cli = |t: Tool| rows.iter().any(|r| matches!(&r.action, Some(Action::Plugin { distro: None, tool, .. }) if *tool == t));
    let apps: Vec<&str> = desktop_apps()
        .into_iter()
        .filter(|app| !has_windows_cli(if *app == "Claude" { Tool::Claude } else { Tool::Codex }))
        .collect();
    for app in apps {
        rows.push(info(
            tr!("{app} app (Windows)", "Application {app} (Windows)"),
            tr!(
                "In the app, add the plugin marketplace {MARKETPLACE}, then the clearway plugin.",
                "Dans l'application, ajoutez le marketplace {MARKETPLACE}, puis le plugin clearway."
            ),
        ));
    }
    for d in distros(false).into_iter().filter(|d| !running.contains(d)) {
        rows.push(info(
            tr!("WSL {d} (stopped)", "WSL {d} (arrêtée)"),
            tr!("Not changed: start it, then run this setup again.", "Non modifiée : démarrez-la, puis relancez l'installation."),
        ));
    }
    if !rows.iter().any(|r| matches!(r.action, Some(Action::Plugin { .. }))) {
        rows.push(info(
            tr!("No Claude Code or Codex found", "Aucun Claude Code ni Codex trouvé"),
            tr!("Install one, then run this setup again.", "Installez-en un, puis relancez l'installation."),
        ));
    }
    rows
}

fn state_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("clearway")
}

/// Current `update.check` of the installed Clearway (off when not installed yet).
pub fn daily_check_enabled() -> bool {
    Config::load(&state_dir(), false).is_ok_and(|c| c.update_check_daily)
}

fn save_daily_check(daily: bool) -> Result<(), String> {
    let dir = state_dir();
    if !daily && !dir.join("config.toml").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut config = Config::load(&dir, true)?;
    if config.update_check_daily != daily {
        config.update_check_daily = daily;
        std::fs::write(dir.join("config.toml"), config.render()).map_err(|e| e.to_string())?;
    }
    Ok(())
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

/// Drops terminal colour codes and carriage returns from a child's output line.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Runs `cmd` without a console window, passing each output line to `emit`.
fn run(cmd: &mut Command, emit: &dyn Fn(Event)) -> bool {
    let spawned = cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            emit(Event::Line(e.to_string()));
            return false;
        }
    };
    let (tx, rx) = mpsc::channel::<String>();
    let streams: Vec<Box<dyn Read + Send>> = [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let readers: Vec<_> = streams
        .into_iter()
        .map(|stream| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                    let _ = tx.send(plain(&String::from_utf8_lossy(&line)));
                }
            })
        })
        .collect();
    drop(tx);
    for line in rx {
        if !line.trim().is_empty() {
            emit(Event::Line(line));
        }
    }
    for r in readers {
        let _ = r.join();
    }
    child.wait().is_ok_and(|s| s.success())
}

fn title(action: &Action) -> String {
    match action {
        Action::Windows => tr!("Installing Clearway for Windows", "Installation de Clearway pour Windows"),
        Action::Wsl(d) => tr!("Installing Clearway in WSL {d}", "Installation de Clearway dans WSL {d}"),
        Action::Plugin { distro, tool, .. } => {
            tr!("Adding the plugin to {} ({})", "Ajout du plugin à {} ({})", tool.name(), place(distro.as_deref()))
        }
    }
}

fn apply(action: &Action, dir: &Path, emit: &dyn Fn(Event)) -> bool {
    match action {
        Action::Windows => {
            let script = dir.join("install.ps1").display().to_string().replace('\'', "''");
            run(
                Command::new("powershell.exe")
                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
                    .arg(format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; & '{script}'"))
                    .env("CLEARWAY_SOURCE", dir)
                    .env("CLEARWAY_QUIET", "1"),
                emit,
            )
        }
        Action::Wsl(d) => run(
            Command::new("wsl.exe")
                .args(["-d", d, "--exec", "sh", "-c", r#"src=$(wslpath -u "$1") && CLEARWAY_SOURCE="$src" sh "$src/install.sh""#, "clearway-setup"])
                .arg(dir),
            emit,
        ),
        Action::Plugin { distro, tool, path } => {
            let mut ok = true;
            for (i, line) in tool.commands().iter().enumerate() {
                let done = match distro {
                    None => run(Command::new("cmd.exe").args(["/d", "/c", &line.replace("{t}", path)]), emit),
                    Some(d) => {
                        let line = line.replace("{t}", &sh_quote(path));
                        run(Command::new("wsl.exe").args(["-d", d, "--exec", "sh", "-c", r#"exec "${SHELL:-sh}" -lc "$1""#, "clearway-setup", &line]), emit)
                    }
                };
                if i == INSTALL_COMMAND && !done {
                    ok = false;
                }
            }
            ok
        }
    }
}

/// Installs the ticked actions in order; returns the titles of the steps that failed.
pub fn install(actions: &[Action], daily: bool, emit: &dyn Fn(Event)) -> Vec<String> {
    if let Err(e) = save_daily_check(daily) {
        emit(Event::Line(tr!("config.toml: {e}", "config.toml : {e}")));
    }
    let dir = match unpack() {
        Ok(d) => d,
        Err(e) => {
            emit(Event::Line(tr!("Cannot write the setup files: {e}", "Impossible d'écrire les fichiers d'installation : {e}")));
            return vec![tr!("setup files", "fichiers d'installation")];
        }
    };
    let mut failures = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        let t = title(action);
        emit(Event::Step { index, total: actions.len(), title: t.clone() });
        let ok = apply(action, &dir, emit);
        emit(Event::StepDone { ok });
        if !ok {
            failures.push(t);
        }
    }
    clean(&dir);
    failures
}

/// The language reaches install.sh in WSL through WSLENV.
fn share_language() {
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
}

fn text_mode(yes: bool) -> i32 {
    println!("{}", tr!("Looking for WSL, Claude Code and Codex (nothing is changed yet)…\n", "Recherche de WSL, Claude Code et Codex (rien n'est encore modifié)…\n"));
    let rows = plan();
    for r in &rows {
        let mark = if r.action.is_some() && r.checked { "x" } else { " " };
        println!("  [{mark}] {} : {}", r.label, r.detail);
    }
    if !yes {
        return 0;
    }
    let actions: Vec<Action> = rows.into_iter().filter(|r| r.checked).filter_map(|r| r.action).collect();
    let failures = install(&actions, daily_check_enabled(), &|e| match e {
        Event::Step { title, .. } => println!("\n== {title}"),
        Event::Line(l) => println!("{l}"),
        Event::StepDone { ok: false } => println!("{}", tr!("   FAILED", "   ÉCHEC")),
        Event::StepDone { ok: true } => {}
    });
    println!();
    if failures.is_empty() {
        println!("{}", tr!("Clearway is installed.", "Clearway est installé."));
        0
    } else {
        println!("{}", tr!("Done, with failures: {}.", "Terminé, avec des échecs : {}.", failures.join(", ")));
        1
    }
}

pub fn main() -> i32 {
    share_language();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let yes = args.iter().any(|a| a == "--yes");
    if yes || args.iter().any(|a| a == "--dry-run") {
        // A GUI program: write to the console of the terminal that started it, if any.
        unsafe { windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS) };
        let code = text_mode(yes);
        let _ = std::io::stdout().flush();
        return code;
    }
    crate::gui::run()
}
