//! `clearway mcp`: MCP server over stdio for one Claude Code or Codex session. Its connection to Clearway carries the
//! session's link: closing the session (end of stdin) hands its tickets over to the resume delay.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clearway::link::{Connection, Connector, Linked};
use clearway::model::{Link, Side};
use clearway::protocol::DEFAULT_PIPE;
use clearway::session::{self, Agent, Process};
use clearway::tools::{SessionInfo, Settings, Tools};
use clearway::tr;

/// Claude Code moves a long MCP call to the background and wakes the session at its end: one call may last.
const CLAUDE_MAX_S: u64 = 6 * 3600;
const RELAUNCH_AFTER: Duration = Duration::from_secs(60);
const ANCESTORS_MAX: usize = 16;

fn log(message: &str) {
    eprintln!("clearway mcp: {message}");
}

fn home() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    PathBuf::from(std::env::var_os(var).unwrap_or_default())
}

/// Starts Clearway at most once a minute while it stays unreachable.
fn may_launch(last: &Mutex<Option<Instant>>) -> bool {
    let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|t| t.elapsed() < RELAUNCH_AFTER) {
        return false;
    }
    *last = Some(Instant::now());
    true
}

pub fn main(args: &[String]) -> i32 {
    let mut max_s: Option<u64> = None;
    let mut pipe = std::env::var("CLEARWAY_PIPE").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| DEFAULT_PIPE.into());
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match (a.as_str(), iter.next()) {
            ("--max-s", Some(v)) if v.parse::<u64>().is_ok() => max_s = v.parse().ok(),
            ("--pipe", Some(v)) => pipe = v.clone(),
            _ => {
                eprintln!("{}", tr!("usage: clearway mcp [--max-s <seconds>] [--pipe <name>]", "usage : clearway mcp [--max-s <secondes>] [--pipe <nom>]"));
                return 2;
            }
        }
    }

    let dir = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    let identity = session::identify(&|k| std::env::var(k).ok(), &ancestors(), &home().join(".claude").join("sessions"), &dir);
    let max_s = max_s.or(match identity.agent {
        Agent::Codex => {
            let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"));
            let config = std::fs::read_to_string(codex_home.join("config.toml")).ok();
            Some(session::codex_max_s(config.as_deref(), "clearway"))
        }
        _ => Some(CLAUDE_MAX_S),
    });
    let (project, branch) = location(&dir);
    log(&format!("session {:?} ({}), longest wait {max_s:?} s", identity.name, identity.key));

    let link = Link {
        identity: identity.key.clone(),
        session: identity.name.clone(),
        client: format!("{:?} mcp {}", identity.agent, env!("CARGO_PKG_VERSION")).to_lowercase(),
        measures: None,
    };
    let channel = Arc::new(Linked::start(connector(pipe), link, log));
    let info = SessionInfo {
        name: identity.name,
        claude_session_id: identity.claude_session_id,
        claude_pid: identity.claude_pid,
        dir,
        project,
        branch,
    };
    let default_side = if cfg!(windows) { Side::Windows } else { Side::Wsl };
    let settings = Settings { default_max_s: max_s, default_side, ..Settings::default() };
    let tools = Arc::new(Tools::new(channel.clone(), info, settings));
    let server_info = Tools::<Linked<Transport>>::info();
    clearway::mcp::serve(tools, &server_info, std::io::stdin().lock(), std::io::stdout(), &log);
    channel.close();
    0
}

#[cfg(unix)]
type Transport = Relay;
#[cfg(windows)]
type Transport = Pipe;

#[cfg(unix)]
fn location(_dir: &str) -> (Option<String>, Option<String>) {
    let (project, branch) = crate::client::git_location();
    (Some(project), branch)
}

#[cfg(windows)]
fn location(dir: &str) -> (Option<String>, Option<String>) {
    (std::path::Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned()), None)
}

/// WSL: one `clearway.exe relay` process per connection, through WSL interop.
#[cfg(unix)]
pub struct Relay {
    exe: Result<PathBuf, String>,
    pipe: String,
    launched: Mutex<Option<Instant>>,
}

#[cfg(unix)]
fn connector(pipe: String) -> Relay {
    Relay { exe: crate::client::find_exe(), pipe, launched: Mutex::new(None) }
}

#[cfg(unix)]
impl Connector for Relay {
    type Guard = std::process::Child;

    fn connect(&self) -> Result<Connection<Self::Guard>, String> {
        use std::process::{Command, Stdio};
        let exe = self.exe.as_ref().map_err(Clone::clone)?;
        let mut cmd = Command::new(exe);
        cmd.args(["relay", "--pipe", &self.pipe]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // A Windows exe started from a Linux directory would inherit a UNC path.
        if exe.starts_with("/mnt/") {
            cmd.current_dir("/mnt/c");
        }
        let mut child = cmd.spawn().map_err(|e| tr!("launching {}: {e}", "lancement de {} : {e}", exe.display()))?;
        let (Some(reader), Some(writer)) = (child.stdout.take(), child.stdin.take()) else {
            let _ = child.kill();
            return Err(tr!("relay without pipes", "relais sans tubes"));
        };
        Ok(Connection { reader: Box::new(reader), writer: Box::new(writer), guard: child })
    }

    fn ended(&self, mut child: Self::Guard) {
        let _ = child.kill();
        let absent = child.wait().ok().and_then(|s| s.code()) == Some(clearway::protocol::RELAY_ABSENT);
        if absent && may_launch(&self.launched) {
            log("Clearway is not running, launching it");
            crate::client::launch_server();
        }
    }

    fn measures(&self) -> Option<clearway::model::WslMeasures> {
        crate::client::measures_wsl()
    }
}

#[cfg(unix)]
fn ancestors() -> Vec<Process> {
    let parent_of = |pid: u32| -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(") ")?.1.split_whitespace().nth(1)?.parse().ok()
    };
    let mut list = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && list.len() < ANCESTORS_MAX {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let command = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        list.push(Process { pid, command });
        match parent_of(pid) {
            Some(p) => pid = p,
            None => break,
        }
    }
    list
}

/// Windows: straight on the named pipe.
#[cfg(windows)]
pub struct Pipe {
    pipe: String,
    launched: Mutex<Option<Instant>>,
}

#[cfg(windows)]
fn connector(pipe: String) -> Pipe {
    Pipe { pipe, launched: Mutex::new(None) }
}

#[cfg(windows)]
impl Connector for Pipe {
    type Guard = ();

    fn connect(&self) -> Result<Connection<()>, String> {
        match crate::transport::connect(&self.pipe) {
            Ok((reader, writer)) => Ok(Connection { reader, writer, guard: () }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if may_launch(&self.launched) {
                    log("Clearway is not running, launching it");
                    launch_self();
                }
                Err(tr!("Clearway is not running", "Clearway absent"))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(windows)]
fn launch_self() {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = Command::new(exe)
        .arg("serve")
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(windows)]
fn ancestors() -> Vec<Process> {
    use std::collections::HashMap;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    let mut table: HashMap<u32, (u32, String)> = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            table.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    let mut list = Vec::new();
    let mut pid = table.get(&std::process::id()).map_or(0, |(p, _)| *p);
    while pid != 0 && list.len() < ANCESTORS_MAX {
        let Some((parent, exe)) = table.get(&pid) else { break };
        list.push(Process { pid, command: vec![exe.clone()] });
        pid = *parent;
    }
    list
}
