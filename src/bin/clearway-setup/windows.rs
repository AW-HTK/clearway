use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clearway::config::Config;
use clearway::model::Status;
use clearway::protocol::{DEFAULT_PIPE, Request, Response, read_line_limited};
use clearway::setup::{
    HookTrust, Installed, Probe, Sessions, WSL_PROBE, compare_installed, codex_hooks_request, expand, find_files,
    hook_trust_from_config, hook_trust_from_list, old_sessions_list, probe_result, sessions_of, sh_quote, wsl_names,
};
use clearway::tr;
use serde_json::Value;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MARKETPLACE: &str = "AW-HTK/clearway";
const PLUGIN: &str = "clearway@clearway";

const FILES: [(&str, &[u8]); 8] = [
    ("FONT-LICENSES.txt", include_bytes!("../../../assets/fonts/FONT-LICENSES.txt")),
    ("clearway.exe", include_bytes!(concat!(env!("CLEARWAY_BUNDLE_DIR"), "/clearway.exe"))),
    ("clearway-x86_64-linux", include_bytes!(concat!(env!("CLEARWAY_BUNDLE_DIR"), "/clearway-x86_64-linux"))),
    ("install.ps1", include_bytes!(concat!(env!("CLEARWAY_BUNDLE_DIR"), "/install.ps1"))),
    ("uninstall.ps1", include_bytes!(concat!(env!("CLEARWAY_BUNDLE_DIR"), "/uninstall.ps1"))),
    ("install.sh", include_bytes!(concat!(env!("CLEARWAY_BUNDLE_DIR"), "/install.sh"))),
    ("SHA256SUMS", include_bytes!(concat!(env!("CLEARWAY_BUNDLE_DIR"), "/SHA256SUMS"))),
    ("VERSION", include_bytes!(concat!(env!("CLEARWAY_BUNDLE_DIR"), "/VERSION"))),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

    fn program(self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Codex => "codex",
        }
    }

    pub fn app(self) -> &'static str {
        match self {
            Tool::Claude => "Claude",
            Tool::Codex => "Codex",
        }
    }

    /// Arguments of the plugin commands, and the index of the one that must succeed (install).
    fn install_commands(self) -> (Vec<String>, usize) {
        match self {
            Tool::Claude => (
                vec![
                    format!("plugin marketplace add {MARKETPLACE}"),
                    "plugin marketplace update clearway".into(),
                    format!("plugin install {PLUGIN} --scope user"),
                    format!("plugin update {PLUGIN}"),
                ],
                2,
            ),
            Tool::Codex => (
                vec![
                    format!("plugin marketplace add {MARKETPLACE}"),
                    "plugin marketplace upgrade clearway".into(),
                    format!("plugin add {PLUGIN}"),
                ],
                2,
            ),
        }
    }

    fn remove_commands(self) -> Vec<String> {
        match self {
            Tool::Claude => vec![format!("plugin uninstall {PLUGIN} --scope user"), "plugin marketplace remove clearway".into()],
            Tool::Codex => vec![format!("plugin remove {PLUGIN}"), "plugin marketplace remove clearway".into()],
        }
    }
}

/// Where a Windows command line tool is looked for beyond the PATH. `{VAR}` is an environment variable, `{DRIVE}`
/// each fixed disk, `*` one folder level: only these folders are read, never a whole disk.
const CLI_PATHS: &[(Tool, &str)] = &[
    (Tool::Claude, r"{USERPROFILE}\.local\bin\claude.exe"),
    (Tool::Claude, r"{APPDATA}\npm\claude.cmd"),
    (Tool::Claude, r"{LOCALAPPDATA}\Microsoft\WinGet\Links\claude.exe"),
    (Tool::Claude, r"{LOCALAPPDATA}\Microsoft\WinGet\Packages\Anthropic.ClaudeCode_*\claude.exe"),
    (Tool::Claude, r"{DRIVE}\Program Files\nodejs\claude.cmd"),
    (Tool::Claude, r"{DRIVE}\nodejs\claude.cmd"),
    (Tool::Codex, r"{CODEX_INSTALL_DIR}\codex.exe"),
    (Tool::Codex, r"{LOCALAPPDATA}\Programs\OpenAI\Codex\bin\codex.exe"),
    (Tool::Codex, r"{USERPROFILE}\.codex\packages\standalone\current\bin\codex.exe"),
    (Tool::Codex, r"{APPDATA}\npm\codex.cmd"),
    (Tool::Codex, r"{LOCALAPPDATA}\Microsoft\WinGet\Links\codex.exe"),
    (Tool::Codex, r"{DRIVE}\Program Files\nodejs\codex.cmd"),
    (Tool::Codex, r"{DRIVE}\nodejs\codex.cmd"),
];

/// The command bundled with a desktop app, which shares the configuration of the command line tool (~/.claude,
/// ~/.codex). `{APPX_<APP>}` is the folder of the app's package, on whatever disk it is installed.
const APP_PATHS: &[(Tool, &str)] = &[
    (Tool::Claude, r"{APPDATA}\Claude\claude-code\*\claude.exe"),
    (Tool::Claude, r"{APPDATA}\Claude\claude-code\*\*\claude.exe"),
    (Tool::Claude, r"{LOCALAPPDATA}\Packages\Claude_*\LocalCache\Roaming\Claude\claude-code\*\claude.exe"),
    (Tool::Claude, r"{LOCALAPPDATA}\Packages\Claude_*\LocalCache\Roaming\Claude\claude-code\*\*\claude.exe"),
    (Tool::Claude, r"{LOCALAPPDATA}\Packages\AnthropicPBC.Claude_*\LocalCache\Roaming\Claude\claude-code\*\claude.exe"),
    (Tool::Claude, r"{LOCALAPPDATA}\Packages\AnthropicPBC.Claude_*\LocalCache\Roaming\Claude\claude-code\*\*\claude.exe"),
    (Tool::Codex, r"{APPX_CODEX}\app\resources\codex.exe"),
    (Tool::Codex, r"{DRIVE}\WindowsApps\OpenAI.Codex_*\app\resources\codex.exe"),
    (Tool::Codex, r"{LOCALAPPDATA}\OpenAI\Codex\bin\*\codex.exe"),
];

/// Signs that a desktop app is installed, even before its bundled command exists.
const APP_SIGNS: &[(Tool, &str)] = &[
    (Tool::Claude, r"{APPX_CLAUDE}\AppxManifest.xml"),
    (Tool::Claude, r"{LOCALAPPDATA}\AnthropicClaude\Update.exe"),
    (Tool::Claude, r"{LOCALAPPDATA}\Microsoft\WindowsApps\Claude.exe"),
    (Tool::Codex, r"{APPX_CODEX}\AppxManifest.xml"),
    (Tool::Codex, r"{LOCALAPPDATA}\Microsoft\WindowsApps\Codex.exe"),
];

/// Packages of the desktop apps (Microsoft Store or MSIX), whose folder may be on any disk.
const APP_PACKAGES: &[(&str, &str)] = &[("Claude", "APPX_CLAUDE"), ("AnthropicPBC.Claude", "APPX_CLAUDE"), ("OpenAI.Codex", "APPX_CODEX")];

#[derive(Clone, Debug)]
pub enum Action {
    Windows,
    Wsl(String),
    Plugin { distro: Option<String>, tool: Tool, path: String, label: String },
    RemovePlugin { distro: Option<String>, tool: Tool, path: String, label: String },
    RemoveWsl { distro: String, path: String },
    RemoveWindows,
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

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub daily: bool,
    /// Uninstalling: also delete the settings and the log.
    pub purge: bool,
}

pub struct Outcome {
    pub action: Action,
    pub ok: bool,
}

/// What the "Verify" button found.
pub struct Checks {
    /// Sessions linked to Clearway right now (Claude Code, Codex); `None` when Clearway does not answer.
    pub sessions: Option<(Sessions, Sessions)>,
    /// Trust of the hook in each place where the Codex plugin is installed.
    pub trust: Vec<(String, HookTrust)>,
}

fn quiet(cmd: &mut Command) -> Option<(bool, Vec<u8>)> {
    let out = cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    Some((out.status.success(), out.stdout))
}

/// Windows PowerShell, without the module path inherited from PowerShell 7 (its modules do not load in 5.1).
fn powershell() -> Command {
    let mut cmd = Command::new("powershell.exe");
    cmd.env_remove("PSModulePath").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]);
    cmd
}

/// A Windows tool run with `args`: an exe directly, a .cmd through cmd.exe.
fn windows_command(path: &str, args: &str) -> Command {
    if path.to_lowercase().ends_with(".exe") {
        let mut cmd = Command::new(path);
        cmd.args(args.split_whitespace());
        cmd
    } else {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/d", "/s", "/c"]).raw_arg(format!("\"\"{path}\" {args}\""));
        cmd
    }
}

/// A WSL tool run with `args` in a login shell, as in a terminal.
fn wsl_command(distro: &str, path: &str, args: &str) -> Command {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-d", distro, "--exec", "sh", "-c", r#"exec "${SHELL:-sh}" -lc "$1""#, "clearway-setup"])
        .arg(format!("{} {args}", sh_quote(path)));
    cmd
}

fn tool_command(distro: Option<&str>, path: &str, args: &str) -> Command {
    match distro {
        None => windows_command(path, args),
        Some(d) => wsl_command(d, path, args),
    }
}

fn distros(running_only: bool) -> Vec<String> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-l", "-q"]);
    if running_only {
        cmd.arg("--running");
    }
    quiet(&mut cmd).map(|(_, out)| wsl_names(&out)).unwrap_or_default()
}

fn fixed_drives() -> Vec<String> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:", (b'A' + i) as char))
        .filter(|d| {
            let root: Vec<u16> = format!("{d}\\").encode_utf16().chain([0]).collect();
            unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED }
        })
        .collect()
}

/// Folders of the desktop app packages, as `APPX_CLAUDE` / `APPX_CODEX`.
fn app_packages() -> Vec<(&'static str, String)> {
    let names: Vec<String> = APP_PACKAGES.iter().map(|(n, _)| format!("'{n}'")).collect();
    let script = format!(
        "Get-AppxPackage -ErrorAction SilentlyContinue | Where-Object {{ $_.Name -in {} }} | ForEach-Object {{ $_.Name + '=' + $_.InstallLocation }}",
        names.join(",")
    );
    let out = quiet(powershell().arg(script)).map(|(_, o)| o).unwrap_or_default();
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.trim().split_once('='))
        .filter_map(|(name, dir)| APP_PACKAGES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, var)| (*var, dir.to_owned())))
        .filter(|(_, dir)| !dir.is_empty())
        .collect()
}

struct Finder {
    drives: Vec<String>,
    packages: Vec<(&'static str, String)>,
}

impl Finder {
    fn new() -> Finder {
        Finder { drives: fixed_drives(), packages: app_packages() }
    }

    fn var(&self, name: &str) -> Option<String> {
        self.packages.iter().find(|(v, _)| *v == name).map(|(_, d)| d.clone()).or_else(|| std::env::var(name).ok())
    }

    /// The newest file matching one of the patterns of `tool`, tried in order.
    fn first(&self, catalog: &[(Tool, &str)], tool: Tool) -> Option<String> {
        catalog.iter().filter(|(t, _)| *t == tool).find_map(|(_, pattern)| {
            expand(pattern, &|name| self.var(name), &self.drives)
                .iter()
                .find_map(|p| find_files(p).into_iter().next())
                .map(|p| p.display().to_string())
        })
    }

    /// The command line tool on the PATH, else in the usual folders.
    fn cli(&self, tool: Tool) -> Option<String> {
        let (_, out) = quiet(Command::new("cmd.exe").args(["/d", "/c", "where", tool.program()]))?;
        String::from_utf8_lossy(&out)
            .lines()
            .map(str::trim)
            .find(|p| [".exe", ".cmd", ".bat"].iter().any(|e| p.to_lowercase().ends_with(e)))
            .map(str::to_owned)
            .or_else(|| self.first(CLI_PATHS, tool))
    }
}

struct WindowsTool {
    tool: Tool,
    /// Command used for the plugin: the command line tool, else the one bundled with the app.
    path: Option<String>,
    cli: bool,
    app: bool,
}

impl WindowsTool {
    fn label(&self) -> String {
        match (self.cli, self.app) {
            (true, true) => tr!("{} (Windows) and the {} app", "{} (Windows) et l'application {}", self.tool.name(), self.tool.app()),
            (false, true) => tr!("{} app (Windows)", "Application {} (Windows)", self.tool.app()),
            _ => format!("{} (Windows)", self.tool.name()),
        }
    }
}

struct Machine {
    windows: Vec<WindowsTool>,
    running: Vec<(String, Probe)>,
    stopped: Vec<String>,
}

fn look() -> Machine {
    let finder = Finder::new();
    let windows = [Tool::Claude, Tool::Codex]
        .into_iter()
        .map(|tool| {
            let cli = finder.cli(tool);
            let bundled = finder.first(APP_PATHS, tool);
            let app = bundled.is_some() || finder.first(APP_SIGNS, tool).is_some();
            WindowsTool { tool, cli: cli.is_some(), app, path: cli.or(bundled) }
        })
        .filter(|t| t.cli || t.app)
        .collect();
    let running: Vec<(String, Probe)> = distros(true)
        .into_iter()
        .map(|d| {
            let out = quiet(Command::new("wsl.exe").args(["-d", &d, "--exec", "sh", "-c", WSL_PROBE])).map(|(_, o)| o).unwrap_or_default();
            let probe = probe_result(&String::from_utf8_lossy(&out));
            (d, probe)
        })
        .collect();
    let stopped = distros(false).into_iter().filter(|d| !running.iter().any(|(r, _)| r == d)).collect();
    Machine { windows, running, stopped }
}

fn place(distro: Option<&str>) -> String {
    distro.map_or_else(|| "Windows".to_owned(), |d| format!("WSL {d}"))
}

fn info(label: String, detail: String) -> Row {
    Row { label, detail, action: None, checked: false, locked: true, needs: None }
}

fn row(label: String, detail: String, action: Action, needs: Option<usize>) -> Row {
    Row { label, detail, action: Some(action), checked: true, locked: false, needs }
}

fn app_without_command(t: &WindowsTool) -> Row {
    info(
        t.label(),
        tr!(
            "Its bundled command is not there yet: open the app once, then run this setup again.",
            "Sa commande intégrée n'est pas encore là : ouvrez l'application une fois, puis relancez l'installation."
        ),
    )
}

/// Looks at the machine without changing anything and lists what the setup proposes.
pub fn plan() -> Vec<Row> {
    let m = look();
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
    for t in &m.windows {
        match &t.path {
            Some(path) => {
                let detail = if t.app {
                    tr!(
                        "Waits for its turn before a heavy task, in the desktop app too.",
                        "Attend son tour avant une tâche lourde, dans l'application de bureau aussi."
                    )
                } else {
                    plugin_detail()
                };
                let label = t.label();
                rows.push(row(label.clone(), detail, Action::Plugin { distro: None, tool: t.tool, path: path.clone(), label }, Some(0)));
            }
            None => rows.push(app_without_command(t)),
        }
    }
    for (d, probe) in &m.running {
        let client = rows.len();
        rows.push(row(
            tr!("Clearway in WSL {d}", "Clearway dans WSL {d}"),
            tr!(
                "The clearway command, used by the sessions of this distribution.",
                "La commande clearway, utilisée par les sessions de cette distribution."
            ),
            Action::Wsl(d.clone()),
            None,
        ));
        for (tool, path) in [(Tool::Claude, &probe.claude), (Tool::Codex, &probe.codex)] {
            if let Some(path) = path {
                let label = format!("{} ({})", tool.name(), place(Some(d)));
                let action = Action::Plugin { distro: Some(d.clone()), tool, path: path.clone(), label: label.clone() };
                rows.push(row(label, plugin_detail(), action, Some(client)));
            }
        }
    }
    for d in &m.stopped {
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

/// What the uninstallation removes, in order: the plugins, the WSL clients, then Clearway for Windows.
pub fn uninstall_plan() -> Vec<Row> {
    let m = look();
    let mut rows = Vec::new();
    let plugin_detail = |place: &str| {
        tr!("Removes the clearway plugin and its marketplace ({place}).", "Retire le plugin clearway et son marketplace ({place}).")
    };
    for t in &m.windows {
        match &t.path {
            Some(path) => {
                let label = t.label();
                let action = Action::RemovePlugin { distro: None, tool: t.tool, path: path.clone(), label: label.clone() };
                rows.push(row(label, plugin_detail("Windows"), action, None));
            }
            None => rows.push(app_without_command(t)),
        }
    }
    for (d, probe) in &m.running {
        for (tool, path) in [(Tool::Claude, &probe.claude), (Tool::Codex, &probe.codex)] {
            if let Some(path) = path {
                let label = format!("{} ({})", tool.name(), place(Some(d)));
                let action = Action::RemovePlugin { distro: Some(d.clone()), tool, path: path.clone(), label: label.clone() };
                rows.push(row(label, plugin_detail(&place(Some(d))), action, None));
            }
        }
        if let Some(path) = &probe.clearway {
            rows.push(row(
                tr!("Clearway in WSL {d}", "Clearway dans WSL {d}"),
                tr!("Deletes the clearway command ({path}).", "Supprime la commande clearway ({path})."),
                Action::RemoveWsl { distro: d.clone(), path: path.clone() },
                None,
            ));
        }
    }
    for d in &m.stopped {
        rows.push(info(
            tr!("WSL {d} (stopped)", "WSL {d} (arrêtée)"),
            tr!(
                "Not changed: start it, then run the uninstallation again to clean it too.",
                "Non modifiée : démarrez-la, puis relancez la désinstallation pour la nettoyer aussi."
            ),
        ));
    }
    rows.push(Row {
        label: tr!("Clearway for Windows", "Clearway pour Windows"),
        detail: tr!(
            "Stops the queue, removes its task, its PATH entry, its folder and its entry in Installed apps.",
            "Arrête la file, retire sa tâche, son entrée du PATH, son dossier et son entrée des Applications installées."
        ),
        action: Some(Action::RemoveWindows),
        checked: true,
        locked: true,
        needs: None,
    });
    rows
}

fn state_dir() -> PathBuf {
    std::env::var_os("CLEARWAY_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("clearway"))
}

pub fn install_dir() -> PathBuf {
    std::env::var_os("CLEARWAY_INSTALL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join(r"Programs\clearway"))
}

/// Clearway for Windows is already installed: the setup offers to update or to uninstall.
pub fn installed() -> bool {
    install_dir().join("clearway.exe").is_file()
}

pub fn installed_version() -> Installed {
    let out = quiet(Command::new(install_dir().join("clearway.exe")).arg("--version")).filter(|(ok, _)| *ok);
    let text = out.and_then(|(_, stdout)| String::from_utf8(stdout).ok());
    compare_installed(text.as_deref(), env!("CARGO_PKG_VERSION"))
}

/// This setup is the copy kept next to clearway.exe, which cannot delete its own folder.
pub fn runs_from_install_dir() -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).ok();
    let exe = std::env::current_exe().ok();
    exe.as_deref().and_then(Path::parent).and_then(canonical).is_some_and(|d| Some(d) == canonical(&install_dir()))
}

/// Starts a copy of this setup from the temporary folder with `args`, so that the installed folder can be emptied.
pub fn relaunch_from_temp(args: &[String]) -> std::io::Result<Child> {
    let copy = std::env::temp_dir().join(format!("clearway-setup-{}.exe", std::process::id()));
    std::fs::copy(std::env::current_exe()?, &copy)?;
    Command::new(copy).args(args).arg("--relaunched").current_dir(std::env::temp_dir()).spawn()
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

/// Runs `cmd` without a console window, passing each output line to `emit`; returns success and the output.
fn run(cmd: &mut Command, emit: &dyn Fn(Event)) -> (bool, String) {
    let spawned = cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            emit(Event::Line(e.to_string()));
            return (false, String::new());
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
    let mut all = String::new();
    for line in rx {
        if !line.trim().is_empty() {
            all.push_str(&line);
            all.push('\n');
            emit(Event::Line(line));
        }
    }
    for r in readers {
        let _ = r.join();
    }
    (child.wait().is_ok_and(|s| s.success()), all)
}

/// Short name of what an action installs or removes, for the step list and the result.
pub fn element(action: &Action) -> String {
    match action {
        Action::Windows | Action::RemoveWindows => tr!("Clearway for Windows", "Clearway pour Windows"),
        Action::Wsl(distro) | Action::RemoveWsl { distro, .. } => tr!("Clearway in WSL {distro}", "Clearway dans WSL {distro}"),
        Action::Plugin { label, .. } | Action::RemovePlugin { label, .. } => tr!("{label}: clearway plugin", "{label} : plugin clearway"),
    }
}

/// What to do when the action failed.
pub fn hint(action: &Action) -> String {
    match action {
        Action::Windows => tr!(
            "The details give the cause; fix it, then run this setup again.",
            "Les détails donnent la cause ; corrigez-la, puis relancez l'installation."
        ),
        Action::Wsl(d) => tr!(
            "Check that \"wsl -d {d}\" opens a terminal, then run this setup again.",
            "Vérifiez que « wsl -d {d} » ouvre un terminal, puis relancez l'installation."
        ),
        Action::Plugin { distro, tool, .. } => tr!(
            "Check that \"{} --version\" answers in {} and that github.com can be reached, then run this setup again.",
            "Vérifiez que « {} --version » répond dans {} et que github.com est joignable, puis relancez l'installation.",
            tool.program(),
            place(distro.as_deref())
        ),
        Action::RemovePlugin { distro, tool, .. } => tr!(
            "In {}, run \"{} plugin\" by hand to remove clearway (see the details).",
            "Dans {}, retirez clearway à la main avec « {} plugin » (voir les détails).",
            place(distro.as_deref()),
            tool.program()
        ),
        Action::RemoveWsl { distro, path } => {
            tr!("Delete {path} by hand in WSL {distro}.", "Supprimez {path} à la main dans WSL {distro}.")
        }
        Action::RemoveWindows => tr!(
            "Close the programs still using Clearway, then run the uninstallation again.",
            "Fermez les programmes qui utilisent encore Clearway, puis relancez la désinstallation."
        ),
    }
}

/// Deletes the WSL client given as $1 when it lies in the user's home, with the language saved by install.sh.
const REMOVE_WSL_CLIENT: &str = r#"case "$1" in
    "$HOME"/*) ;;
    *) echo "clearway: $1 is outside $HOME, not deleted"; exit 1 ;;
esac
rm -f "$1" "$(dirname "$1")/.clearway.new"
conf=${XDG_CONFIG_HOME:-$HOME/.config}/clearway
rm -f "$conf/language"
rmdir "$conf" 2>/dev/null || true
[ ! -e "$1" ] && echo "$1: deleted""#;

fn script(dir: &Path, name: &str) -> String {
    dir.join(name).display().to_string().replace('\'', "''")
}

fn apply(action: &Action, dir: &Path, options: &Options, emit: &dyn Fn(Event)) -> bool {
    match action {
        Action::Windows => {
            let mut cmd = powershell();
            cmd.arg(format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; & '{}'", script(dir, "install.ps1")))
                .env("CLEARWAY_SOURCE", dir)
                .env("CLEARWAY_QUIET", "1");
            if let Ok(exe) = std::env::current_exe() {
                cmd.env("CLEARWAY_SETUP_EXE", exe);
            }
            run(&mut cmd, emit).0
        }
        Action::RemoveWindows => {
            let mut cmd = powershell();
            cmd.arg(format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; & '{}'", script(dir, "uninstall.ps1")));
            if options.purge {
                cmd.env("CLEARWAY_PURGE", "1");
            }
            run(&mut cmd, emit).0
        }
        Action::Wsl(d) => {
            run(
                Command::new("wsl.exe")
                    .args(["-d", d, "--exec", "sh", "-c", r#"src=$(wslpath -u "$1") && CLEARWAY_SOURCE="$src" sh "$src/install.sh""#, "clearway-setup"])
                    .arg(dir),
                emit,
            )
            .0
        }
        Action::RemoveWsl { distro, path } => {
            run(Command::new("wsl.exe").args(["-d", distro, "--exec", "sh", "-c", REMOVE_WSL_CLIENT, "clearway-setup", path]), emit).0
        }
        Action::Plugin { distro, tool, path, .. } => {
            let (commands, required) = tool.install_commands();
            let mut ok = true;
            for (i, args) in commands.iter().enumerate() {
                let (done, _) = run(&mut tool_command(distro.as_deref(), path, args), emit);
                if i == required && !done {
                    ok = false;
                }
            }
            ok
        }
        Action::RemovePlugin { distro, tool, path, .. } => {
            let mut removed = false;
            for (i, args) in tool.remove_commands().iter().enumerate() {
                let (done, _) = run(&mut tool_command(distro.as_deref(), path, args), emit);
                if i == 0 {
                    removed = done;
                }
            }
            // Already absent counts as removed: the list must no longer show the plugin.
            match run(&mut tool_command(distro.as_deref(), path, "plugin list"), &|_| {}) {
                (true, list) => !list.contains(PLUGIN),
                (false, _) => removed,
            }
        }
    }
}

/// Runs the ticked actions in order and reports each one.
pub fn install(actions: &[Action], options: &Options, emit: &dyn Fn(Event)) -> Vec<Outcome> {
    if actions.iter().any(|a| matches!(a, Action::Windows))
        && let Err(e) = save_daily_check(options.daily)
    {
        emit(Event::Line(tr!("config.toml: {e}", "config.toml : {e}")));
    }
    let dir = match unpack() {
        Ok(d) => d,
        Err(e) => {
            emit(Event::Line(tr!("Cannot write the setup files: {e}", "Impossible d'écrire les fichiers d'installation : {e}")));
            return actions.iter().map(|a| Outcome { action: a.clone(), ok: false }).collect();
        }
    };
    let mut outcomes = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        emit(Event::Step { index, total: actions.len(), title: element(action) });
        let ok = apply(action, &dir, options, emit);
        emit(Event::StepDone { ok });
        outcomes.push(Outcome { action: action.clone(), ok });
    }
    clean(&dir);
    outcomes
}

/// `GET /status` straight on the pipe of Clearway for Windows.
fn server_status() -> Option<Status> {
    let name = std::env::var("CLEARWAY_PIPE").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| DEFAULT_PIPE.into());
    let path = format!(r"\\.\pipe\{name}");
    let began = Instant::now();
    let mut pipe = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(p) => break p,
            // All instances busy: another client is being served.
            Err(e) if e.raw_os_error() == Some(231) && began.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    };
    let request = serde_json::to_string(&Request::new("GET", "/status", Value::Null)).ok()?;
    writeln!(pipe, "{request}").ok()?;
    let line = read_line_limited(&mut BufReader::new(pipe)).ok()??;
    let response: Response = serde_json::from_str(&line).ok()?;
    (response.code == 200).then(|| serde_json::from_value(response.body).ok())?
}

/// Writes `input` to the command and collects its output until `done` says so or `timeout` passes.
fn converse(cmd: &mut Command, input: &str, done: impl Fn(&str) -> bool, timeout: Duration) -> String {
    let spawned = cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = spawned else { return String::new() };
    // Kept open until the end: the server would stop at the end of its input, perhaps before answering.
    let mut stdin = child.stdin.take();
    if let Some(s) = stdin.as_mut() {
        let _ = s.write_all(input.as_bytes()).and_then(|()| s.flush());
    }
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    let end = Instant::now() + timeout;
    let mut out = String::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(line) => {
                out.push_str(&line);
                out.push('\n');
                if done(&out) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(stdin);
    out
}

fn codex_trust(distro: Option<&str>, path: &str) -> HookTrust {
    let cwd = match distro {
        None => std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\".into()),
        Some(_) => "/".into(),
    };
    let mut cmd = tool_command(distro, path, "app-server");
    if distro.is_none() {
        cmd.current_dir(&cwd);
    }
    let out = converse(&mut cmd, &codex_hooks_request(&cwd), |o| hook_trust_from_list(o).is_some(), Duration::from_secs(20));
    if let Some(trust) = hook_trust_from_list(&out) {
        return trust;
    }
    let config = match distro {
        None => {
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".codex"));
            std::fs::read_to_string(home.join("config.toml")).unwrap_or_default()
        }
        Some(d) => {
            let cat = Command::new("wsl.exe").args(["-d", d, "--exec", "sh", "-c", r#"cat "${CODEX_HOME:-$HOME/.codex}/config.toml""#]).output();
            cat.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
        }
    };
    hook_trust_from_config(&config)
}

/// Checks behind the "Verify" button, for the plugins installed by `actions`.
pub fn check(actions: &[Action]) -> Checks {
    let version = env!("CARGO_PKG_VERSION");
    let sessions = server_status().map(|s| (sessions_of(&s.links, "claude", version), sessions_of(&s.links, "codex", version)));
    let trust = actions
        .iter()
        .filter_map(|a| match a {
            Action::Plugin { distro, tool: Tool::Codex, path, .. } => {
                Some((place(distro.as_deref()), codex_trust(distro.as_deref(), path)))
            }
            _ => None,
        })
        .collect();
    Checks { sessions, trust }
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

fn trust_text(t: HookTrust) -> String {
    match t {
        HookTrust::Trusted => tr!("approved", "approuvée"),
        HookTrust::Untrusted => tr!("not approved yet", "pas encore approuvée"),
        HookTrust::Modified => tr!("changed since its approval: approve it again", "modifiée depuis son approbation : à approuver de nouveau"),
        HookTrust::Absent => tr!("not loaded by Codex yet", "pas encore chargée par Codex"),
    }
}

fn text_mode(uninstall: bool, yes: bool, purge: bool, verify: bool) -> i32 {
    println!("{}", tr!("Looking for WSL, Claude Code and Codex (nothing is changed yet)…\n", "Recherche de WSL, Claude Code et Codex (rien n'est encore modifié)…\n"));
    let rows = if uninstall { uninstall_plan() } else { plan() };
    for r in &rows {
        let mark = if r.action.is_some() && r.checked { "x" } else { " " };
        println!("  [{mark}] {} : {}", r.label, r.detail);
    }
    let actions: Vec<Action> = rows.into_iter().filter(|r| r.checked).filter_map(|r| r.action).collect();
    if verify {
        let c = check(&actions);
        match c.sessions {
            Some((claude, codex)) => {
                println!();
                for (tool, s) in [("Claude Code", claude), ("Codex", codex)] {
                    println!("{}", tr!("{tool} sessions up to date: {}", "Sessions {tool} à jour : {}", s.current));
                    if !s.old.is_empty() {
                        println!("{}", tr!("  to restart (older version): {}", "  à relancer (ancienne version) : {}", old_sessions_list(&s.old, usize::MAX)));
                    }
                }
            }
            None => println!("\n{}", tr!("Clearway does not answer.", "Clearway ne répond pas.")),
        }
        for (place, t) in c.trust {
            println!("{}", tr!("Codex guard ({place}): {}", "Garde Codex ({place}) : {}", trust_text(t)));
        }
    }
    if !yes {
        return 0;
    }
    let outcomes = install(&actions, &Options { daily: daily_check_enabled(), purge }, &|e| match e {
        Event::Step { title, .. } => println!("\n== {title}"),
        Event::Line(l) => println!("{l}"),
        Event::StepDone { ok: false } => println!("{}", tr!("   FAILED", "   ÉCHEC")),
        Event::StepDone { ok: true } => {}
    });
    println!();
    for o in &outcomes {
        println!("{} {}", if o.ok { "✔" } else { "✖" }, element(&o.action));
        if !o.ok {
            println!("    {}", hint(&o.action));
        }
    }
    i32::from(outcomes.iter().any(|o| !o.ok))
}

pub fn main() -> i32 {
    share_language();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    // Preview has priority over every mutating option, including --yes and --uninstall.
    if has("--preview") {
        return crate::gui::run(false, true);
    }
    let (yes, uninstall) = (has("--yes"), has("--uninstall"));
    let text = yes || has("--dry-run") || has("--check");
    if text {
        // A GUI program: write to the console of the terminal that started it, if any.
        unsafe { windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS) };
    }
    if uninstall && !has("--relaunched") && runs_from_install_dir() {
        let child = relaunch_from_temp(&args);
        if !text {
            return i32::from(child.is_err());
        }
        return match child.and_then(|mut c| c.wait()) {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    if text {
        let code = text_mode(uninstall, yes, has("--purge"), has("--check"));
        let _ = std::io::stdout().flush();
        return code;
    }
    crate::gui::run(uninstall, false)
}
