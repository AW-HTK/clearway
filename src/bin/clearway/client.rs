use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use clearway::format::{cores, duration, gb, parse_duration, parse_ram};
use clearway::model::{Side, TicketRequest, Release, WslMeasures, Presence, Extension, Status, View};
use clearway::protocol::{RELAY_ABSENT, Response, Request, DEFAULT_PIPE};
use clearway::system::{random, hex, hhmm};
use clearway::tr;
use serde_json::{Value, json};

const LEGACY_HELP: &str = "\
charge : file des tâches lourdes, servie par clearway.exe (Windows) à travers son relais

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
Variables : CHARGE_EXE (clearway.exe), CHARGE_TUBE, CHARGE_DOSSIER (état Windows vu de WSL),
CHARGE_SESSION (nom affiché).
";

fn help() -> String {
    if legacy() {
        return LEGACY_HELP.to_owned();
    }
    tr!(
        "\
clearway: queue for heavy tasks, served by clearway.exe (Windows) through its relay

  clearway run [--side wsl|windows] [--ram 3G] [--cpu 4] [--duration 15m] [--max 9m] \"description\" -- command…
      requests a slot, waits for clearance in the foreground, runs the command, reports its presence,
      releases at the end; rerunning the same command resumes the same ticket
  clearway request [options] [\"<session>\"] \"<description>\"
      creates a manual ticket and prints its number; the declared duration is its lease
  clearway wait <ticket> [--max 9m]       waits for clearance in the foreground, printing each change
  clearway extend <ticket> <duration> \"<reason>\"   extends the duration of an active ticket
  clearway release <ticket> [--code N]    releases the slot
  clearway cancel <ticket>                withdraws the request or releases the slot
  clearway status                         queue, reserves and measures
  clearway mcp [--max-s <seconds>]        MCP server (stdio) for a Claude Code or Codex session

Exit codes: 0 cleared or success, 1 ticket left the queue, 2 error, 3 refused,
75 still waiting after --max (rerunning the same command keeps the place), 130 interrupted.
Variables: CLEARWAY_EXE (clearway.exe), CLEARWAY_PIPE, CLEARWAY_DIR (Windows state seen from WSL),
CLEARWAY_SESSION (displayed name).
",
        "\
clearway : file des tâches lourdes, servie par clearway.exe (Windows) à travers son relais

  clearway run [--side wsl|windows] [--ram 3G] [--cpu 4] [--duration 15m] [--max 9m] \"description\" -- commande…
      demande un créneau, attend le feu vert au premier plan, lance la commande, signale sa présence,
      libère à la fin ; relancer la même commande reprend le même ticket
  clearway request [options] [\"<session>\"] \"<description>\"
      crée un ticket manuel et affiche son numéro ; la durée déclarée sert de bail
  clearway wait <ticket> [--max 9m]       attend le feu vert au premier plan, en écrivant chaque changement
  clearway extend <ticket> <durée> \"<raison>\"   allonge la durée d'un ticket actif
  clearway release <ticket> [--code N]    libère le créneau
  clearway cancel <ticket>                retire la demande ou libère le créneau
  clearway status                         file, réserves et mesures
  clearway mcp [--max-s <secondes>]       serveur MCP (stdio) pour une session Claude Code ou Codex

Codes de sortie : 0 feu vert ou succès, 1 ticket sorti de la file, 2 erreur, 3 refus,
75 toujours en attente après --max (relancer la même commande garde la place), 130 interrompu.
Variables : CLEARWAY_EXE (clearway.exe), CLEARWAY_PIPE, CLEARWAY_DIR (état Windows vu de WSL),
CLEARWAY_SESSION (nom affiché).
"
    )
}

const PATIENCE_S: u64 = 120;
/// Past this age, a `maintenance` file is taken as left behind by a crashed reinstall.
const MAINTENANCE_MAX: Duration = Duration::from_secs(5 * 60);
const MAX_WAIT_BETWEEN_ATTEMPTS_S: u64 = 15;
const PRESENCE_INTERVAL: Duration = Duration::from_secs(10);
const SHORT_TIMEOUT: Duration = Duration::from_secs(15);
const WAIT_MARGIN: Duration = Duration::from_secs(30);
const DEFAULT_MAX_S: u64 = 9 * 60;
const CODE_STILL_WAITING: i32 = 75;

static SIGNAL: AtomicI32 = AtomicI32::new(0);
static LEGACY: AtomicBool = AtomicBool::new(false);

extern "C" fn note_signal(sig: libc::c_int) {
    SIGNAL.store(sig, Ordering::SeqCst);
}

fn install_signals() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, note_signal as *const () as libc::sighandler_t) };
    }
}

fn signal_received() -> bool {
    SIGNAL.load(Ordering::SeqCst) != 0
}

fn interrupted() -> String {
    tr!("interrupted by a signal", "interrompu par un signal")
}

fn legacy() -> bool {
    LEGACY.load(Ordering::SeqCst)
}

/// Name the user typed the client under: `charge` keeps the old French command names.
fn program() -> &'static str {
    if legacy() { "charge" } else { "clearway" }
}

/// Command to quote in hints, from its English subcommand: `clearway release`, or `charge fin` in legacy mode.
fn cmd(sub: &str) -> String {
    if !legacy() {
        return format!("clearway {sub}");
    }
    let french = match sub {
        "request" => "demande",
        "wait" => "attend",
        "extend" => "prolonge",
        "release" => "fin",
        "cancel" => "annule",
        "status" => "statut",
        other => other,
    };
    format!("charge {french}")
}

/// `CLEARWAY_*` variable, else its old `CHARGE_*` name.
fn env_os(name: &str, old: &str) -> Option<OsString> {
    std::env::var_os(name).or_else(|| std::env::var_os(old))
}

pub(crate) fn env(name: &str, old: &str) -> Option<String> {
    std::env::var(name).or_else(|_| std::env::var(old)).ok()
}

pub fn main(args: &[String], legacy: bool) -> i32 {
    if legacy {
        LEGACY.store(true, Ordering::SeqCst);
        clearway::i18n::set(clearway::i18n::Lang::Fr);
    }
    match execute(args) {
        Ok(c) => c,
        Err(e) => {
            let prog = program();
            eprintln!("{}", tr!("{prog}: {e}", "{prog} : {e}"));
            2
        }
    }
}

fn execute(args: &[String]) -> Result<i32, String> {
    let Some((cmd, left)) = args.split_first() else {
        eprint!("{}", help());
        return Ok(2);
    };
    match cmd.as_str() {
        "run" => run(left),
        "request" | "demande" => request_cmd(left),
        "wait" | "attend" => wait_cmd(left),
        "extend" | "prolonge" => extend_cmd(left),
        "release" | "fin" => end(left),
        "cancel" | "annule" => cancel_cmd(number(left)?),
        "status" | "statut" => status_view(),
        "help" | "aide" | "--help" | "-h" => {
            print!("{}", help());
            Ok(0)
        }
        _ => {
            eprint!("{}", help());
            Ok(2)
        }
    }
}

fn number(args: &[String]) -> Result<u64, String> {
    args.first()
        .and_then(|a| a.trim_start_matches('#').parse().ok())
        .ok_or_else(|| tr!("ticket number expected", "numéro de ticket attendu"))
}

enum Failure {
    Absent(String),
    Lost(String),
    Timeout,
    Interrupted,
}

/// A `clearway relay` process kept open: one request per line on its stdin, one response per line on its stdout.
struct Connection {
    child: Child,
    input: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Connection {
    fn open(exe: &Path, pipe: &str) -> Result<Connection, Failure> {
        let mut cmd = Command::new(exe);
        cmd.args(["relay", "--pipe", pipe]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // A Windows exe started from a Linux directory would inherit a UNC path.
        if exe.starts_with("/mnt/") {
            cmd.current_dir("/mnt/c");
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| Failure::Lost(tr!("launching {}: {e}", "lancement de {} : {e}", exe.display())))?;
        let out = child.stdout.take().expect("relay stdout");
        let input = child.stdin.take();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Connection { child, input, lines })
    }

    fn end(&mut self) -> Failure {
        self.input = None;
        let status = self.child.wait();
        let mut error = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut error);
        }
        let error = error.trim().to_owned();
        match status.ok().and_then(|s| s.code()) {
            Some(RELAY_ABSENT) => Failure::Absent(error),
            Some(c) => Failure::Lost(if error.is_empty() {
                tr!("relay exited with code {c}", "relais sorti avec le code {c}")
            } else {
                error
            }),
            None => Failure::Lost(tr!("relay interrupted", "relais interrompu")),
        }
    }

    fn exchange(&mut self, req: &Request, delay: Duration) -> Result<Response, Failure> {
        let mut line = serde_json::to_string(req).map_err(|e| Failure::Lost(e.to_string()))?;
        line.push('\n');
        let written = match self.input.as_mut() {
            Some(e) => e.write_all(line.as_bytes()).and_then(|_| e.flush()).is_ok(),
            None => false,
        };
        if !written {
            return Err(self.end());
        }
        let limit = Instant::now() + delay;
        loop {
            match self.lines.recv_timeout(Duration::from_millis(250)) {
                Ok(l) => {
                    return serde_json::from_str(&l)
                        .map_err(|e| Failure::Lost(tr!("unreadable response ({e}): {l}", "réponse illisible ({e}) : {l}")));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.end()),
                Err(RecvTimeoutError::Timeout) if signal_received() => return Err(Failure::Interrupted),
                Err(RecvTimeoutError::Timeout) if Instant::now() > limit => return Err(Failure::Timeout),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.input = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    exe: PathBuf,
    pipe: String,
    connection: Option<Connection>,
    launched: bool,
    /// `--max` deadline: new attempts stop there to stay under a command's timeout.
    limit: Option<Instant>,
}

/// The relay could not be started through WSL interop (vsock, exec failure), as opposed to Clearway being down.
/// The WSL interop socket refused, as in the Codex sandbox: retrying cannot help.
fn sandboxed(message: &str) -> bool {
    message.contains("UtilBindVsockAnyPort")
}

fn interop_failure(message: &str) -> bool {
    message.contains("WSL ERROR")
        || message.contains("vsock")
        || ["lancement de ", "launching ", "relais sorti avec le code", "relay exited with code"]
            .iter()
            .any(|p| message.starts_with(p))
}

fn patience() -> Duration {
    Duration::from_secs(env("CLEARWAY_PATIENCE", "CHARGE_PATIENCE").and_then(|p| p.parse().ok()).unwrap_or(PATIENCE_S))
}

pub(crate) fn find_exe() -> Result<PathBuf, String> {
    if let Some(e) = env_os("CLEARWAY_EXE", "CHARGE_EXE") {
        return Ok(PathBuf::from(e));
    }
    let users: Vec<PathBuf> = std::fs::read_dir("/mnt/c/Users")
        .map_err(|e| tr!("/mnt/c/Users unreadable: {e}", "/mnt/c/Users illisible : {e}"))?
        .flatten()
        .map(|u| u.path())
        .collect();
    for relative in ["AppData/Local/Programs/clearway/clearway.exe", "AppData/Local/Programs/orchestrateur/orchestrateur.exe"] {
        if let Some(exe) = users.iter().map(|u| u.join(relative)).find(|exe| exe.exists()) {
            return Ok(exe);
        }
    }
    Err(tr!(
        "clearway.exe not found: install it in %LOCALAPPDATA%\\Programs\\clearway or set CLEARWAY_EXE",
        "clearway.exe introuvable : installe-le dans %LOCALAPPDATA%\\Programs\\clearway ou définis CLEARWAY_EXE"
    ))
}

/// Starts Clearway as `run` would, unless an update is in progress.
pub(crate) fn launch_server() {
    if let Ok(c) = Client::new()
        && !c.in_maintenance()
    {
        c.launch();
    }
}

fn wslpath(p: &Path) -> Option<String> {
    let out = Command::new("wslpath").arg("-w").arg(p).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

impl Client {
    fn new() -> Result<Client, String> {
        Ok(Client {
            exe: find_exe()?,
            pipe: env("CLEARWAY_PIPE", "CHARGE_TUBE").unwrap_or_else(|| DEFAULT_PIPE.into()),
            connection: None,
            launched: false,
            limit: None,
        })
    }

    /// Windows state directories: `CLEARWAY_DIR`, else `%LOCALAPPDATA%\clearway` and the old
    /// `%LOCALAPPDATA%\orchestrateur`, found from the exe location.
    fn state_dirs(&self) -> Vec<PathBuf> {
        if let Some(d) = env_os("CLEARWAY_DIR", "CHARGE_DOSSIER") {
            return vec![PathBuf::from(d)];
        }
        let Some(local) = self.exe.parent().and_then(Path::parent).and_then(Path::parent) else {
            return Vec::new();
        };
        vec![local.join("clearway"), local.join("orchestrateur")]
    }

    /// Reinstall in progress (file written by windows/reinstaller.ps1): wait instead of launching the exe.
    fn in_maintenance(&self) -> bool {
        self.state_dirs().iter().any(|d| {
            std::fs::metadata(d.join("maintenance"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < MAINTENANCE_MAX)
        })
    }

    fn copy(&self) -> Client {
        Client { exe: self.exe.clone(), pipe: self.pipe.clone(), connection: None, launched: self.launched, limit: None }
    }

    /// Starts Clearway, once, when the relay reports it absent.
    fn launch(&self) {
        let prog = program();
        let mut args: Vec<String> = vec!["serve".into()];
        if let Some(d) = env_os("CLEARWAY_DIR", "CHARGE_DOSSIER").map(PathBuf::from) {
            let path = if self.exe.extension().is_some_and(|e| e == "exe") { wslpath(&d) } else { Some(d.display().to_string()) };
            if let Some(c) = path {
                args.extend(["--dir".into(), c]);
            }
        }
        if let Some(extra) = env("CLEARWAY_LAUNCH_ARGS", "CHARGE_ARGS_LANCEMENT") {
            args.extend(extra.split_whitespace().map(str::to_owned));
        }
        eprintln!(
            "{}",
            tr!("{prog}: Clearway is not running, launching {}…", "{prog} : Clearway absent, lancement de {}…", self.exe.display())
        );
        if self.exe.extension().is_some_and(|e| e == "exe") {
            let Some(exe_w) = wslpath(&self.exe) else {
                eprintln!("{}", tr!("{prog}: wslpath failed, cannot launch.", "{prog} : wslpath a échoué, lancement impossible."));
                return;
            };
            let list: Vec<String> = args.iter().map(|a| format!("'\"{}\"'", a.replace('\'', "''"))).collect();
            // `cmd /c start` would make the exe inherit the interop pipes, and we would wait for it to exit.
            let mut command = format!("Start-Process -FilePath '{}'", exe_w.replace('\'', "''"));
            if !list.is_empty() {
                command += &format!(" -ArgumentList {}", list.join(","));
            }
            let _ = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &command])
                .current_dir("/mnt/c")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        } else {
            let _ = Command::new(&self.exe).args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }

    fn call(&mut self, req: &Request, delay: Duration) -> Result<Response, String> {
        let prog = program();
        let began = Instant::now();
        let patience = patience();
        let mut last = String::new();
        let mut pause = Duration::from_secs(1);
        loop {
            let mut absent = false;
            if self.connection.is_none() {
                match Connection::open(&self.exe, &self.pipe) {
                    Ok(c) => self.connection = Some(c),
                    Err(Failure::Lost(m)) => last = m,
                    Err(_) => {}
                }
            }
            if let Some(c) = self.connection.as_mut() {
                match c.exchange(req, delay) {
                    Ok(r) => return Ok(r),
                    Err(Failure::Interrupted) => {
                        self.connection = None;
                        return Err(interrupted());
                    }
                    Err(Failure::Timeout) => {
                        self.connection = None;
                        return Err(tr!("no response from Clearway within {} s", "pas de réponse de Clearway en {} s", delay.as_secs()));
                    }
                    Err(Failure::Absent(m)) => {
                        self.connection = None;
                        absent = true;
                        last = m;
                    }
                    Err(Failure::Lost(m)) => {
                        self.connection = None;
                        last = m;
                    }
                }
            }
            if signal_received() {
                return Err(interrupted());
            }
            if sandboxed(&last) {
                return Err(tr!(
                    "this sandbox cannot start Windows programs (WSL interop refused), so it cannot reach Clearway: \
                     use the clearway MCP tools (request_clearance, then release) instead of this command.",
                    "ce bac à sable ne peut pas lancer de programme Windows (interop WSL refusée), donc pas joindre \
                     Clearway : utilise les outils MCP clearway (request_clearance, puis release) au lieu de cette commande."
                ));
            }
            let maintenance = absent && self.in_maintenance();
            if absent && !maintenance && !self.launched {
                self.launched = true;
                self.launch();
            }
            let waiting = if absent && !maintenance { Duration::from_secs(1) } else { pause };
            let deadline = self.limit.is_some_and(|l| Instant::now() + waiting >= l);
            if began.elapsed() + waiting > patience || deadline {
                return Err(tr!(
                    "Clearway unreachable for {} s ({}). Check its icon in the Windows notification area.",
                    "Clearway injoignable depuis {} s ({}). Vérifie son icône dans la zone de notification Windows.",
                    began.elapsed().as_secs(),
                    last.lines().next().unwrap_or("")
                ));
            }
            if maintenance {
                eprintln!(
                    "{}",
                    tr!(
                        "{prog}: Clearway is being updated, retrying in {} s.",
                        "{prog} : Clearway en cours de mise à jour, nouvel essai dans {} s.",
                        waiting.as_secs()
                    )
                );
                pause = (pause * 2).min(Duration::from_secs(MAX_WAIT_BETWEEN_ATTEMPTS_S));
            } else if !absent {
                let what = if interop_failure(&last) {
                    tr!("WSL interop unavailable", "interop WSL indisponible")
                } else {
                    tr!("Clearway down", "Clearway coupé")
                };
                let detail = last.lines().next().map(str::to_owned).unwrap_or_else(|| tr!("no detail", "sans détail"));
                eprintln!(
                    "{}",
                    tr!(
                        "{prog}: {what} ({detail}), retrying in {} s.",
                        "{prog} : {what} ({detail}), nouvel essai dans {} s.",
                        waiting.as_secs()
                    )
                );
                pause = (pause * 2).min(Duration::from_secs(MAX_WAIT_BETWEEN_ATTEMPTS_S));
            }
            std::thread::sleep(waiting);
        }
    }

    fn request(&mut self, method: &str, path: &str, body: Value) -> Result<Response, String> {
        self.call(&Request::new(method, path, body), SHORT_TIMEOUT)
    }

    fn waiting(&mut self, id: u64, known: &str, cap: Duration) -> Result<Response, String> {
        let mut req = Request::new("GET", &format!("/tickets/{id}/wait"), Value::Null);
        req.params = BTreeMap::from([
            ("known".into(), known.to_owned()),
            ("timeout_s".into(), cap.as_secs().max(1).to_string()),
            ("ack".into(), "1".into()),
        ]);
        add_measures(&mut req);
        self.call(&req, cap + WAIT_MARGIN)
    }
}

fn vmstat(name: &str) -> u64 {
    std::fs::read_to_string("/proc/vmstat")
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.trim().parse().ok()))
        .unwrap_or(0)
}

pub(crate) fn measures_wsl() -> Option<WslMeasures> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| -> Option<u64> {
        meminfo
            .lines()
            .find(|l| l.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()
            .map(|kb| kb / 1024)
    };
    let load1 = std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()?;
    let (cpu_busy, cpu_jiffies, cpus) =
        std::fs::read_to_string("/proc/stat").ok().as_deref().and_then(clearway::model::cpu_counters).unwrap_or_default();
    Some(WslMeasures {
        mem_avail_mb: field("MemAvailable:")?,
        mem_total_mb: field("MemTotal:")?,
        load1,
        swap_total_mb: field("SwapTotal:").unwrap_or(0),
        swap_free_mb: field("SwapFree:").unwrap_or(0),
        pswpin: vmstat("pswpin"),
        pswpout: vmstat("pswpout"),
        cpu_busy,
        cpu_jiffies,
        cpus,
        busy_cores: None,
        at: 0,
    })
}

fn add_measures(req: &mut Request) {
    if let Some(m) = measures_wsl() {
        req.params.insert("mem_avail_mb".into(), m.mem_avail_mb.to_string());
        req.params.insert("mem_total_mb".into(), m.mem_total_mb.to_string());
        req.params.insert("load1".into(), format!("{:.2}", m.load1));
        req.params.insert("swap_total_mb".into(), m.swap_total_mb.to_string());
        req.params.insert("swap_free_mb".into(), m.swap_free_mb.to_string());
        req.params.insert("pswpin".into(), m.pswpin.to_string());
        req.params.insert("pswpout".into(), m.pswpout.to_string());
        req.params.insert("cpu_busy".into(), m.cpu_busy.to_string());
        req.params.insert("cpu_jiffies".into(), m.cpu_jiffies.to_string());
        req.params.insert("cpus".into(), m.cpus.to_string());
    }
}

fn request_id() -> String {
    let h = hex(&random(16).unwrap_or_else(|_| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{:032x}", t.as_nanos() ^ ((std::process::id() as u128) << 64)).into_bytes()[..16].to_vec()
    }));
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn fnv1a(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for o in text.bytes() {
        h ^= o as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

struct Options {
    side: Side,
    ram_mb: Option<u64>,
    cpu: Option<u32>,
    duration_s: Option<u64>,
    max_s: u64,
    code: Option<i32>,
    positionals: Vec<String>,
    command: Vec<String>,
    no_clearway: bool,
}

fn options(args: &[String], with_command: bool) -> Result<Options, String> {
    let mut o = Options {
        side: Side::Wsl,
        ram_mb: None,
        cpu: None,
        duration_s: None,
        max_s: DEFAULT_MAX_S,
        code: None,
        positionals: Vec::new(),
        command: Vec::new(),
        no_clearway: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" && with_command {
            o.command = args[i + 1..].to_vec();
            break;
        }
        let (name, value_joined) = match a.split_once('=') {
            Some((n, v)) if a.starts_with("--") => (n, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(v) = value_joined.clone() {
                return Ok(v);
            }
            i += 1;
            args.get(i).cloned().ok_or_else(|| tr!("value expected after {name}", "valeur attendue après {name}"))
        };
        match name {
            "--side" | "--cote" => {
                let v = value()?;
                o.side = Side::read(&v).ok_or_else(|| {
                    tr!("{name}: `wsl` or `windows` expected, not `{v}`", "{name} : « wsl » ou « windows » attendu, pas « {v} »")
                })?;
            }
            "--ram" => {
                let v = value()?;
                o.ram_mb = Some(parse_ram(&v).ok_or_else(|| {
                    tr!("unreadable --ram: `{v}` (e.g. 3G, 1.5G, 512M)", "--ram illisible : « {v} » (ex. 3G, 1.5G, 512M)")
                })?);
            }
            "--cpu" => {
                let v = value()?;
                o.cpu = Some(v.parse().ok().filter(|&n| n > 0).ok_or_else(|| {
                    tr!("--cpu: positive integer expected, not `{v}`", "--cpu : entier positif attendu, pas « {v} »")
                })?);
            }
            "--duration" | "--duree" => {
                let v = value()?;
                o.duration_s = Some(parse_duration(&v).ok_or_else(|| {
                    tr!("unreadable {name}: `{v}` (e.g. 15m, 1h30, 90s)", "{name} illisible : « {v} » (ex. 15m, 1h30, 90s)")
                })?);
            }
            "--max" => {
                let v = value()?;
                o.max_s = parse_duration(&v)
                    .ok_or_else(|| tr!("unreadable --max: `{v}` (e.g. 9m, 90s)", "--max illisible : « {v} » (ex. 9m, 90s)"))?;
            }
            "--code" => {
                let v = value()?;
                o.code =
                    Some(v.parse().map_err(|_| tr!("--code: integer expected, not `{v}`", "--code : entier attendu, pas « {v} »"))?);
            }
            "--no-clearway" | "--sans-orchestrateur" if with_command => o.no_clearway = true,
            n if n.starts_with("--") => return Err(tr!("unknown option: {n}", "option inconnue : {n}")),
            _ => o.positionals.push(a.clone()),
        }
        i += 1;
    }
    Ok(o)
}

/// `CLEARWAY_SESSION`, else the Claude Code session name (undocumented internal file, read leniently), else Codex,
/// else `—`.
fn default_session() -> String {
    if let Some(s) = env("CLEARWAY_SESSION", "CHARGE_SESSION") {
        return s;
    }
    let claude_name = std::env::var("CLAUDE_PID").ok().and_then(|pid| {
        let path = PathBuf::from(std::env::var_os("HOME")?).join(format!(".claude/sessions/{pid}.json"));
        let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        v["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned)
    });
    if let Some(n) = claude_name {
        return n;
    }
    match std::env::var("CODEX_THREAD_ID") {
        Ok(id) if !id.is_empty() => format!("Codex {}", id.chars().take(8).collect::<String>()),
        _ => "—".into(),
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// (directory, branch / worktree) of the current directory, from one git call capped at 2 s: repos on /mnt/g are slow.
pub(crate) fn git_location() -> (String, Option<String>) {
    git_location_in(&std::env::current_dir().unwrap_or_default())
}

/// Project folder and branch text of the git repository holding `dir`.
pub(crate) fn git_location_in(dir: &Path) -> (String, Option<String>) {
    let current = dir.to_path_buf();
    let mut child = match Command::new("git")
        .current_dir(&current)
        .args(["rev-parse", "--show-toplevel", "--git-dir", "--git-common-dir", "--abbrev-ref", "HEAD", "--short", "HEAD"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(e) => e,
        Err(_) => return (name_of(&current), None),
    };
    let limit = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(status) = status else {
        return (name_of(&current), Some("?".into()));
    };
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    let lines: Vec<&str> = out.lines().collect();
    let [root, git_dir, common_dir, branch, sha] = lines.as_slice() else {
        return (name_of(&current), if status.success() { Some("?".into()) } else { None });
    };
    let absolute = |p: &str| std::fs::canonicalize(current.join(p)).unwrap_or_else(|_| current.join(p));
    let branch = if *branch == "HEAD" { format!("HEAD@{sha}") } else { (*branch).to_owned() };
    let root = PathBuf::from(root);
    let text = if absolute(git_dir) != absolute(common_dir) {
        tr!("{} (branch {branch})", "{} (branche {branch})", name_of(&root))
    } else {
        branch
    };
    (name_of(&root), Some(text))
}

/// Rough signature for the history: program and subcommands, without options or paths.
fn signature(command: &[String]) -> String {
    let mut words = Vec::new();
    for (i, m) in command.iter().enumerate() {
        if i == 0 {
            words.push(Path::new(m).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| m.clone()));
        } else if !m.starts_with('-') && !m.contains('/') && !m.contains('=') && words.len() < 3 {
            words.push(m.clone());
        } else {
            break;
        }
    }
    words.join(" ")
}

fn request_from(o: &Options, session: String, description: String, manual: bool) -> TicketRequest {
    let dir = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    let (project, branch) = git_location();
    let session_claude = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let reattach_key = if manual {
        String::new()
    } else {
        fnv1a(&[
            session_claude.clone().unwrap_or_else(|| session.clone()),
            dir.clone(),
            o.side.name().into(),
            format!("{:?} {:?} {:?}", o.ram_mb, o.cpu, o.duration_s),
            description.clone(),
            o.command.join("\u{1f}"),
        ]
        .join("\u{1e}"))
    };
    TicketRequest {
        request_id: request_id(),
        side: o.side,
        ram_mb: o.ram_mb,
        cpu: o.cpu,
        duration_s: o.duration_s,
        description,
        session,
        claude_session_id: session_claude,
        claude_pid: std::env::var("CLAUDE_PID").ok().and_then(|p| p.parse().ok()),
        dir,
        signature: signature(&o.command),
        measures: measures_wsl(),
        manual,
        reattach_key,
        project: Some(project),
        branch,
        ack: !manual,
    }
}

fn read_view(body: &Value) -> Result<View, String> {
    serde_json::from_value(body.clone())
        .map_err(|e| tr!("unreadable response from Clearway ({e}): {body}", "réponse illisible de Clearway ({e}) : {body}"))
}

fn message(body: &Value) -> String {
    body["reason"]["message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .or_else(|| body["error"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| body.to_string())
}

fn create(c: &mut Client, d: &TicketRequest) -> Result<Result<(u16, View), View>, String> {
    let r = c.request("POST", "/tickets", serde_json::to_value(d).map_err(|e| e.to_string())?)?;
    match r.code {
        200 | 201 => Ok(Ok((r.code, read_view(&r.body)?))),
        422 => Ok(Err(read_view(&r.body)?)),
        code => Err(tr!("ticket creation refused ({code}): {}", "création refusée ({code}) : {}", message(&r.body))),
    }
}

fn request_cmd(args: &[String]) -> Result<i32, String> {
    let o = options(args, false)?;
    let (session, description) = match o.positionals.as_slice() {
        [d] => (default_session(), d.clone()),
        [s, d] => (s.clone(), d.clone()),
        _ => {
            let c = cmd("request");
            return Err(tr!(
                "usage: {c} [options] [\"<session>\"] \"<description>\"",
                "usage : {c} [options] [\"<session>\"] \"<description>\""
            ));
        }
    };
    let mut c = Client::new()?;
    match create(&mut c, &request_from(&o, session, description, true))? {
        Ok((_, v)) => {
            let id = v.id;
            println!("{id}");
            eprintln!("{}", v.reason.message);
            if v.state == "waiting" {
                let c = cmd("wait");
                eprintln!(
                    "{}",
                    tr!(
                        "Run `{c} {id}` in the foreground (it returns on clearance, or after --max with code 75).",
                        "Lance « {c} {id} » au premier plan (il rend la main au feu vert, ou après --max avec le code 75)."
                    )
                );
            } else {
                let c = cmd("release");
                eprintln!("{}", tr!("Start the heavy task, then `{c} {id}`.", "Lance la tâche lourde, puis « {c} {id} »."));
            }
            Ok(0)
        }
        Err(refusal) => {
            eprintln!("{}", refusal.reason.message);
            Ok(3)
        }
    }
}

enum WaitEnd {
    Cleared,
    Out(String),
    Still(String),
}

/// Waits in the foreground for clearance, for the ticket to leave the queue, or for `--max` to run out.
fn wait(c: &mut Client, id: u64, known: &mut String, max_s: u64, write: &dyn Fn(&str)) -> Result<WaitEnd, String> {
    let prog = program();
    let limit = Instant::now() + Duration::from_secs(max_s);
    c.limit = Some(limit);
    let mut last = String::new();
    loop {
        let left = limit.saturating_duration_since(Instant::now());
        if left.is_zero() {
            c.limit = None;
            return Ok(WaitEnd::Still(last));
        }
        // An interop outage never drops the ticket: reattach when it comes back, up to --max.
        let r = match c.waiting(id, known, left.min(Duration::from_secs(60))) {
            Ok(r) => r,
            Err(e) if e == interrupted() => return Err(e),
            Err(e) => {
                eprintln!(
                    "{}",
                    tr!(
                        "{prog}: {e} Ticket #{id} stays in the queue; retrying.",
                        "{prog} : {e} Le ticket #{id} reste en file ; nouvel essai."
                    )
                );
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        match r.code {
            200 => {
                let v = read_view(&r.body)?;
                let key = v.reason.key();
                last = v.reason.message.clone();
                if key != *known {
                    write(&v.reason.message);
                    *known = key;
                }
                if v.state == "active" || v.state == "orphaned" {
                    c.limit = None;
                    return Ok(WaitEnd::Cleared);
                }
            }
            404 => {
                c.limit = None;
                return Ok(WaitEnd::Out(message(&r.body)));
            }
            code => return Err(tr!("waiting for #{id} ({code}): {}", "attente du #{id} ({code}) : {}", message(&r.body))),
        }
    }
}

/// Confirms the clearance was delivered; `Ok(false)` if Clearway took it back in the meantime.
fn acknowledge(c: &mut Client, id: u64) -> Result<bool, String> {
    let r = c.request("POST", &format!("/tickets/{id}/ack"), Value::Null)?;
    match r.code {
        200 => Ok(true),
        409 => Ok(false),
        // Server older than the ack: the response it sent already counted as delivery.
        404 if r.body["error"] == "unknown route" || r.body["error"] == "route inconnue" || r.body["erreur"] == "route inconnue" => {
            Ok(true)
        }
        code => Err(tr!("ack of #{id} ({code}): {}", "accusé du #{id} ({code}) : {}", message(&r.body))),
    }
}

fn wait_cmd(args: &[String]) -> Result<i32, String> {
    let id = number(args)?;
    let o = options(&args[1..], false)?;
    install_signals();
    let mut c = Client::new()?;
    let mut known = String::new();
    let write = |p: &str| {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{p}").and_then(|_| out.flush());
    };
    let wait_again = cmd("wait");
    match wait(&mut c, id, &mut known, o.max_s, &write) {
        Ok(WaitEnd::Cleared) => {
            let release = cmd("release");
            let mut out = std::io::stdout();
            writeln!(
                out,
                "{}",
                tr!(
                    "CLEARED: ticket #{id} — start your heavy task now, then `{release} {id}`.",
                    "FEU VERT pour le ticket #{id} : lance ta tâche lourde maintenant, puis « {release} {id} »."
                )
            )
            .and_then(|_| out.flush())
            .map_err(|e| {
                tr!(
                    "clearance of #{id} not delivered ({e}): without an ack, Clearway will take it back.",
                    "feu vert du #{id} non transmis ({e}) : sans accusé, Clearway le reprendra."
                )
            })?;
            match acknowledge(&mut c, id) {
                Ok(true) => Ok(0),
                Ok(false) => {
                    println!(
                        "{}",
                        tr!(
                            "Clearance of #{id} taken back by Clearway before the ack: start nothing, rerun `{wait_again} {id}`.",
                            "Feu vert du #{id} repris par Clearway avant l'accusé : ne lance rien, relance « {wait_again} {id} »."
                        )
                    );
                    Ok(CODE_STILL_WAITING)
                }
                Err(e) if e == interrupted() => Ok(130),
                Err(e) => Err(tr!(
                    "{e}; clearance not confirmed: rerun `{wait_again} {id}` before starting the heavy task.",
                    "{e} ; feu vert non confirmé : relance « {wait_again} {id} » avant de lancer la tâche lourde."
                )),
            }
        }
        Ok(WaitEnd::Out(p)) => {
            println!("{}", tr!("Ticket #{id} out of the queue: {p}", "Ticket #{id} hors de la file : {p}"));
            Ok(1)
        }
        Ok(WaitEnd::Still(p)) => {
            println!(
                "{}",
                tr!(
                    "Still waiting after {}: {p} Rerun `{wait_again} {id}` to keep your place.",
                    "Toujours en attente après {} : {p} Relance « {wait_again} {id} » pour garder ta place.",
                    duration(o.max_s)
                )
            );
            Ok(CODE_STILL_WAITING)
        }
        Err(e) if e == interrupted() => Ok(130),
        Err(e) => Err(e),
    }
}

fn extend_cmd(args: &[String]) -> Result<i32, String> {
    let (Some(id), Some(d), Some(reason)) = (
        args.first().and_then(|a| a.trim_start_matches('#').parse::<u64>().ok()),
        args.get(1).and_then(|d| parse_duration(d)),
        args.get(2),
    ) else {
        let c = cmd("extend");
        return Err(tr!(
            "usage: {c} <ticket> <duration> \"<reason>\" (e.g. {c} 12 20m \"Infection longer than expected\")",
            "usage : {c} <ticket> <durée> \"<raison>\" (ex. {c} 12 20m \"Infection plus longue que prévu\")"
        ));
    };
    let mut c = Client::new()?;
    let body = serde_json::to_value(Extension { duration_s: d, reason: reason.clone() }).map_err(|e| e.to_string())?;
    let r = c.request("POST", &format!("/tickets/{id}/extend"), body)?;
    match r.code {
        200 => {
            println!("{}", message(&r.body));
            Ok(0)
        }
        404 => {
            eprintln!(
                "{}",
                tr!(
                    "Ticket #{id} unknown or already out of the queue: {}",
                    "Ticket #{id} inconnu ou déjà sorti de la file : {}",
                    message(&r.body)
                )
            );
            Ok(1)
        }
        code => Err(tr!("extension of #{id} ({code}): {}", "prolongation du #{id} ({code}) : {}", message(&r.body))),
    }
}

fn end(args: &[String]) -> Result<i32, String> {
    let id = number(args)?;
    let o = options(&args[1..], false)?;
    let mut c = Client::new()?;
    close_ticket(&mut c, id, &Release { code: o.code, peak_mb: None, duration_s: None })
}

fn close_ticket(c: &mut Client, id: u64, f: &Release) -> Result<i32, String> {
    let r = c.request("POST", &format!("/tickets/{id}/release"), serde_json::to_value(f).map_err(|e| e.to_string())?)?;
    show_closing(id, &r)
}

fn cancel_cmd(id: u64) -> Result<i32, String> {
    let mut c = Client::new()?;
    let r = c.request("DELETE", &format!("/tickets/{id}"), Value::Null)?;
    show_closing(id, &r)
}

fn show_closing(id: u64, r: &Response) -> Result<i32, String> {
    match r.code {
        200 => {
            println!("{}", message(&r.body));
            Ok(0)
        }
        404 => {
            eprintln!(
                "{}",
                tr!(
                    "Ticket #{id} unknown or already out of the queue: {}",
                    "Ticket #{id} inconnu ou déjà sorti de la file : {}",
                    message(&r.body)
                )
            );
            Ok(1)
        }
        code => Err(tr!("ticket #{id} ({code}): {}", "ticket #{id} ({code}) : {}", message(&r.body))),
    }
}

fn status_view() -> Result<i32, String> {
    let mut c = Client::new()?;
    let mut req = Request::new("GET", "/status", Value::Null);
    add_measures(&mut req);
    let r = c.call(&req, SHORT_TIMEOUT)?;
    if r.code != 200 {
        return Err(tr!("status ({}): {}", "statut ({}) : {}", r.code, message(&r.body)));
    }
    let s: Status = serde_json::from_value(r.body).map_err(|e| tr!("unreadable status: {e}", "statut illisible : {e}"))?;
    let m = &s.measures;
    println!(
        "{}",
        tr!(
            "Clearway (epoch {}) · Windows RAM {} available of {} · commit {} / {} · CPU {:.0} %",
            "Clearway (époque {}) · RAM Windows {} disponibles sur {} · commit {} / {} · CPU {:.0} %",
            s.epoch,
            gb(m.ram_avail_mb as i64),
            gb(m.ram_total_mb as i64),
            gb(m.commit_mb as i64),
            gb(m.commit_limit_mb as i64),
            m.cpu_pct
        )
    );
    match (m.vmmem, m.wsl) {
        (None, _) => println!("{}", tr!("WSL: stopped (no vmmemWSL)", "WSL : arrêté (vmmemWSL absent)")),
        (Some(v), Some(w)) => println!(
            "{}",
            tr!(
                "WSL: vmmemWSL {} · MemAvailable {} of {} · load {:.2}{} (measured {} s ago)",
                "WSL : vmmemWSL {} · MemAvailable {} sur {} · charge {:.2}{} (mesure d'il y a {} s)",
                gb(v.rss_mb as i64),
                gb(w.mem_avail_mb as i64),
                gb(w.mem_total_mb as i64),
                w.load1,
                w.busy_cores.map(|b| tr!(" · {b:.1} cores busy", " · {b:.1} cœurs occupés")).unwrap_or_default(),
                s.now.saturating_sub(w.at)
            )
        ),
        (Some(v), None) => println!(
            "{}",
            tr!(
                "WSL: vmmemWSL {} · no measure from the client yet",
                "WSL : vmmemWSL {} · pas encore de mesure du client",
                gb(v.rss_mb as i64)
            )
        ),
    }
    if let Some(g) = s.manual_freeze {
        println!("{}", tr!("! Queue frozen by hand since {}.", "! File gelée à la main depuis {}.", hhmm(g)));
    }
    for r in &s.reserves {
        let q = |v: i64| if r.key.starts_with("cpu") { cores(v) } else { gb(v) };
        println!(
            "{}",
            tr!(
                "  {:<12} available {:>9} · reserved {:>9} · margin {:>8} · free {}",
                "  {:<12} disponible {:>9} · réservé {:>9} · marge {:>8} · libre {}",
                r.name,
                q(r.available),
                q(r.reserve),
                q(r.margin),
                r.free_text()
            ) + &r.used_pct.map_or(String::new(), |p| tr!(" · measured {p} %", " · mesuré {p} %"))
        );
    }
    // The manual freeze already has its own line above.
    for a in s.alerts.iter().filter(|a| !a.contains("gel manuel") && !a.to_lowercase().contains("manual freeze")) {
        println!("! {a}");
    }
    if s.tickets.is_empty() {
        println!("{}", tr!("Queue empty, no slot in use.", "File vide, aucun créneau occupé."));
    }
    for t in &s.tickets {
        let since = t.granted.unwrap_or(t.created);
        let location = match (&t.project, &t.branch) {
            (Some(p), Some(b)) => format!(" {p} · {b}"),
            (Some(p), None) => format!(" {p}"),
            _ => String::new(),
        };
        let flags = match (t.force, t.manual) {
            (true, true) => tr!(" (forced, manual)", " (forcé, manuel)"),
            (true, false) => tr!(" (forced)", " (forcé)"),
            (false, true) => tr!(" (manual)", " (manuel)"),
            (false, false) => String::new(),
        };
        let overrun = if t.lease_overrun { tr!(" (lease overrun)", " (bail dépassé)") } else { String::new() };
        println!(
            "{}",
            tr!(
                "#{:<4} {:<8} {:<7} {:>7} {:>9} {:>8}  since {}  [{}]{location}: {}{flags}{overrun}",
                "#{:<4} {:<8} {:<7} {:>7} {:>9} {:>8}  depuis {}  [{}]{location} : {}{flags}{overrun}",
                t.id,
                match t.state {
                    clearway::model::TicketState::Waiting => tr!("waiting", "attente"),
                    clearway::model::TicketState::Active => tr!("active", "actif"),
                    clearway::model::TicketState::Orphaned => tr!("orphaned", "orphelin"),
                },
                t.side.name(),
                gb(t.need.ram_mb as i64),
                cores(t.need.cpu as i64),
                duration(t.need.duration_s),
                hhmm(since),
                t.session,
                t.description
            )
        );
        println!("      {}", t.message);
    }
    Ok(0)
}

fn run(args: &[String]) -> Result<i32, String> {
    let prog = program();
    let o = options(args, true)?;
    if o.command.is_empty() {
        let c = cmd("run");
        return Err(tr!("usage: {c} [options] \"description\" -- command…", "usage : {c} [options] \"description\" -- commande…"));
    }
    if o.positionals.len() > 1 {
        return Err(tr!(
            "only one description expected before `--` (put it in quotes)",
            "une seule description attendue avant « -- » (mets-la entre guillemets)"
        ));
    }
    if o.no_clearway {
        let flag = if legacy() { "--sans-orchestrateur" } else { "--no-clearway" };
        eprintln!("{}", tr!("{prog}: {flag}: direct launch, outside the queue.", "{prog} : {flag} : lancement direct, hors file."));
        let status = Command::new(&o.command[0])
            .args(&o.command[1..])
            .status()
            .map_err(|e| tr!("launching {}: {e}", "lancement de {} : {e}", o.command[0]))?;
        return Ok(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0)));
    }
    install_signals();
    let description = o.positionals.first().cloned().unwrap_or_else(|| o.command.join(" "));
    let mut c = Client::new()?;
    let d = request_from(&o, default_session(), description, false);
    let (code, v) = match create(&mut c, &d)? {
        Ok(r) => r,
        Err(refusal) => {
            eprintln!("{}", tr!("{prog}: {}", "{prog} : {}", refusal.reason.message));
            return Ok(3);
        }
    };
    let id = v.id;
    if code == 200 {
        eprintln!(
            "{}",
            tr!("{prog}: ticket #{id} resumed (same command) · {}", "{prog} : ticket #{id} repris (même commande) · {}", v.reason.message)
        );
    } else {
        eprintln!("{}", tr!("{prog}: ticket #{id} · {}", "{prog} : ticket #{id} · {}", v.reason.message));
    }
    let mut waiting = v.state == "waiting";
    let mut known = v.reason.key();
    loop {
        if waiting {
            let write = |p: &str| eprintln!("{}", tr!("{prog}: {p}", "{prog} : {p}"));
            match wait(&mut c, id, &mut known, o.max_s, &write) {
                Ok(WaitEnd::Cleared) => {}
                Ok(WaitEnd::Out(p)) => {
                    eprintln!("{}", tr!("{prog}: ticket #{id} withdrawn: {p}", "{prog} : ticket #{id} retiré : {p}"));
                    return Ok(1);
                }
                Ok(WaitEnd::Still(p)) => {
                    eprintln!(
                        "{}",
                        tr!(
                            "{prog}: still waiting after {}: {p} Rerun the same command to resume ticket #{id} without losing your place.",
                            "{prog} : toujours en attente après {} : {p} Relance la même commande pour reprendre le ticket #{id} sans perdre ta place.",
                            duration(o.max_s)
                        )
                    );
                    return Ok(CODE_STILL_WAITING);
                }
                Err(e) if e == interrupted() => {
                    let _ = c.copy().request("DELETE", &format!("/tickets/{id}"), Value::Null);
                    eprintln!("{}", tr!("{prog}: interrupted, ticket #{id} cancelled.", "{prog} : interrompu, ticket #{id} annulé."));
                    return Ok(130);
                }
                Err(e) => return Err(e),
            }
        }
        match acknowledge(&mut c, id) {
            Ok(true) => break,
            Ok(false) => {
                eprintln!(
                    "{}",
                    tr!(
                        "{prog}: clearance #{id} taken back before the ack; waiting again.",
                        "{prog} : feu vert #{id} repris avant l'accusé ; attente reprise."
                    )
                );
                waiting = true;
            }
            Err(e) if e == interrupted() => {
                let _ = c.copy().request("DELETE", &format!("/tickets/{id}"), Value::Null);
                eprintln!("{}", tr!("{prog}: interrupted, ticket #{id} cancelled.", "{prog} : interrompu, ticket #{id} annulé."));
                return Ok(130);
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    tr!("{prog}: {e}; the presence signal will confirm it.", "{prog} : {e} ; le signal de présence le confirmera.")
                );
                break;
            }
        }
    }
    run_granted(&mut c, id, &o.command)
}

fn run_granted(c: &mut Client, id: u64, command: &[String]) -> Result<i32, String> {
    let prog = program();
    eprintln!(
        "{}",
        tr!("{prog}: clearance #{id}, launching: {}", "{prog} : feu vert #{id}, lancement de : {}", command.join(" "))
    );
    let began = Instant::now();
    let mut child = match Command::new(&command[0]).args(&command[1..]).spawn() {
        Ok(e) => e,
        Err(e) => {
            let _ = close_ticket(c, id, &Release { code: Some(127), peak_mb: None, duration_s: Some(0) });
            return Err(tr!("launching {}: {e}", "lancement de {} : {e}", command[0]));
        }
    };

    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let done = done.clone();
        // The connection that just carried the acknowledgement is already open: the first presence leaves at once,
        // without a new relay through WSL interop.
        let mut c = std::mem::replace(c, c.copy());
        std::thread::spawn(move || {
            let path = format!("/tickets/{id}/presence");
            let mut warned = false;
            while !done.load(Ordering::SeqCst) {
                let body = serde_json::to_value(Presence { measures: measures_wsl() }).unwrap_or(json!({}));
                if let Ok(r) = c.request("POST", &path, body)
                    && r.code == 404
                    && !warned
                {
                    eprintln!(
                        "{}",
                        tr!(
                            "{prog}: ticket #{id} is no longer tracked by Clearway; the command goes on.",
                            "{prog} : le ticket #{id} n'est plus suivi par Clearway ; la commande continue."
                        )
                    );
                    warned = true;
                }
                let wakeup = Instant::now() + PRESENCE_INTERVAL;
                while Instant::now() < wakeup && !done.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            c
        })
    };

    let mut forwarded: Option<Instant> = None;
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => {
                eprintln!("{}", tr!("{prog}: waiting for the command: {e}", "{prog} : attente de la commande : {e}"));
                break child.wait().map_err(|e| e.to_string())?;
            }
        }
        if signal_received() && forwarded.is_none() {
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            forwarded = Some(Instant::now());
        }
        if !killed && forwarded.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            let _ = child.kill();
            killed = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    done.store(true, Ordering::SeqCst);
    if let Ok(connection) = watcher.join() {
        *c = connection;
    }

    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let peak_mb =
        (unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) } == 0).then_some(usage.ru_maxrss as u64 / 1024);
    let duration_s = began.elapsed().as_secs();
    let f = Release { code: Some(code), peak_mb, duration_s: Some(duration_s) };
    // A signal received must not prevent reporting the release.
    SIGNAL.store(0, Ordering::SeqCst);
    match c.request("POST", &format!("/tickets/{id}/release"), serde_json::to_value(&f).map_err(|e| e.to_string())?) {
        Ok(r) if r.code == 200 || r.code == 404 => {}
        Ok(r) => eprintln!(
            "{}",
            tr!(
                "{prog}: release of #{id} not recorded ({}): {}",
                "{prog} : fin du #{id} non enregistrée ({}) : {}",
                r.code,
                message(&r.body)
            )
        ),
        Err(e) => {
            let release = cmd("release");
            eprintln!(
                "{}",
                tr!(
                    "{prog}: release of #{id} not recorded: {e}; run `{release} {id}` later.",
                    "{prog} : fin du #{id} non enregistrée : {e} ; « {release} {id} » plus tard."
                )
            )
        }
    }
    eprintln!(
        "{}",
        tr!("{prog}: #{id} finished (code {code}, {}).", "{prog} : #{id} terminé (code {code}, {}).", duration(duration_s))
    );
    Ok(code)
}
