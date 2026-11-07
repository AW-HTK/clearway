//! Spike: minimal stdio MCP server with a `sleep(seconds)` tool, logging everything it receives.
//! Log: `$SPIKE_DIR/spike-<pid>.log`, by default `~/tmp/orchestrateur/spike/` (`%USERPROFILE%` on Windows).

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clearway::system::{timestamp, unix_now};
use serde_json::{Value, json};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const PROGRESS: Duration = Duration::from_secs(30);

struct Log(Mutex<std::fs::File>);

impl Log {
    fn write(&self, text: &str) {
        let clock = format!("{}.{:03}", timestamp(unix_now()), milliseconds());
        if let Ok(mut f) = self.0.lock() {
            let _ = writeln!(f, "{clock} {text}");
            let _ = f.flush();
        }
    }
}

fn milliseconds() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis()).unwrap_or(0)
}

struct Output {
    log: Arc<Log>,
    stream: Mutex<io::Stdout>,
}

impl Output {
    fn send(&self, message: &Value) -> bool {
        let line = message.to_string();
        let ok = self
            .stream
            .lock()
            .map(|mut s| writeln!(s, "{line}").and_then(|_| s.flush()).is_ok())
            .unwrap_or(false);
        self.log.write(&format!("→ {line}{}", if ok { "" } else { "  [WRITE FAILED]" }));
        ok
    }
}

fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SPIKE_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
    PathBuf::from(home).join("tmp").join("orchestrateur").join("spike")
}

fn sensitive(name: &str) -> bool {
    let n = name.to_uppercase();
    ["TOKEN", "KEY", "SECRET", "PASSWORD", "AUTH", "COOKIE", "CREDENTIAL"].iter().any(|m| n.contains(m))
}

fn environment(log: &Log) {
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| {
            ["CLAUDE", "CODEX", "MCP", "ANTHROPIC", "OPENAI", "WSL", "TERM_PROGRAM"].iter().any(|p| k.starts_with(p))
                || matches!(k.as_str(), "PWD" | "SHELL" | "USERPROFILE" | "HOME" | "PATH")
        })
        .collect();
    vars.sort();
    for (k, v) in vars {
        let v = if sensitive(&k) { format!("<masked, {} characters>", v.len()) } else { v };
        log.write(&format!("ENV {k}={v}"));
    }
}

fn one_line(text: &str) -> String {
    let flat = text.trim().replace(['\r', '\n'], " ⏎ ");
    if flat.chars().count() > 400 { format!("{}…", flat.chars().take(400).collect::<String>()) } else { flat }
}

#[cfg(unix)]
fn ancestry(log: &Log) {
    let mut pid = std::process::id();
    for _ in 0..6 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let parent = stat.rsplit(')').next().and_then(|r| r.split_whitespace().nth(1)).and_then(|p| p.parse::<u32>().ok());
        let Some(parent) = parent.filter(|&p| p > 1) else { break };
        let line = std::fs::read(format!("/proc/{parent}/cmdline"))
            .map(|o| o.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        log.write(&format!("PARENT pid {parent} :: {}", one_line(&line)));
        pid = parent;
    }
}

#[cfg(windows)]
fn ancestry(log: &Log) {
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
                log.write(&format!("PARENT {}", one_line(l)));
            }
        }
        Err(e) => log.write(&format!("PARENT unknown: {e}")),
    }
}

fn sleep(out: Arc<Output>, id: Value, seconds: u64, token: Option<Value>, cancelled: Arc<AtomicBool>) {
    let start = Instant::now();
    let total = Duration::from_secs(seconds);
    let mut next = PROGRESS;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            out.log.write(&format!("SLEEP {id} cancelled after {} s (no response, as MCP requires)", start.elapsed().as_secs()));
            return;
        }
        let elapsed = start.elapsed();
        if elapsed >= total {
            break;
        }
        if elapsed >= next {
            next += PROGRESS;
            if let Some(j) = &token {
                out.send(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": j,
                        "progress": elapsed.as_secs(),
                        "total": seconds,
                        "message": format!("Sleeping for {} s of {seconds} s", elapsed.as_secs())
                    }
                }));
            } else {
                out.log.write(&format!("SLEEP {id}: {} s, no progressToken", elapsed.as_secs()));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let ok = out.send(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "content": [{ "type": "text", "text": format!("Woke up after {seconds} s.") }], "isError": false }
    }));
    out.log.write(&format!("SLEEP {id} done, response {}", if ok { "written" } else { "NOT written" }));
}

fn tools() -> Value {
    json!({ "tools": [{
        "name": "sleep",
        "description": "Waits for the requested number of seconds, then answers. Used to test long tool calls.",
        "inputSchema": {
            "type": "object",
            "properties": { "seconds": { "type": "integer", "minimum": 0, "description": "How long to wait" } },
            "required": ["seconds"]
        }
    }]})
}

fn main() {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("spike-{}.log", std::process::id()));
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap_or_else(|e| {
        eprintln!("spike: cannot open log {}: {e}", path.display());
        std::process::exit(2)
    });
    let log = Arc::new(Log(Mutex::new(file)));
    log.write(&format!(
        "START pid {} · argv {:?} · current dir {:?}",
        std::process::id(),
        std::env::args().collect::<Vec<_>>(),
        std::env::current_dir().ok()
    ));
    environment(&log);
    {
        let log = log.clone();
        std::thread::spawn(move || ancestry(&log));
    }

    let out = Arc::new(Output { log: log.clone(), stream: Mutex::new(io::stdout()) });
    let mut in_flight: HashMap<String, Arc<AtomicBool>> = HashMap::new();
    let input = io::stdin();
    for line in input.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                log.write(&format!("ERROR reading stdin: {e}"));
                break;
            }
        };
        log.write(&format!("← {line}"));
        let Ok(m) = serde_json::from_str::<Value>(&line) else {
            log.write("unreadable line");
            continue;
        };
        let id = m.get("id").cloned();
        let method = m["method"].as_str().unwrap_or("");
        let params = &m["params"];
        match (method, id) {
            ("initialize", Some(id)) => {
                let requested = params["protocolVersion"].as_str().unwrap_or("");
                let version = if VERSIONS.contains(&requested) { requested } else { VERSIONS[1] };
                log.write(&format!(
                    "INITIALIZE client {} · requested version {requested:?}, chosen {version} · capabilities {}",
                    params["clientInfo"],
                    params["capabilities"]
                ));
                out.send(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "spike", "version": "0.1.0" },
                        "instructions": "Test server: the sleep(seconds) tool waits, then answers."
                    }
                }));
            }
            ("tools/list", Some(id)) => {
                out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": tools() }));
            }
            ("tools/call", Some(id)) if params["name"] == "sleep" => {
                let seconds = params["arguments"]["seconds"].as_u64().unwrap_or(0);
                let token = params["_meta"].get("progressToken").cloned();
                log.write(&format!("SLEEP {id}: {seconds} s, progressToken {token:?}, _meta {}", params["_meta"]));
                let cancelled = Arc::new(AtomicBool::new(false));
                in_flight.insert(id.to_string(), cancelled.clone());
                let out = out.clone();
                std::thread::spawn(move || sleep(out, id, seconds, token, cancelled));
            }
            ("tools/call", Some(id)) => {
                out.send(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": [{ "type": "text", "text": "Unknown tool." }], "isError": true }
                }));
            }
            ("ping", Some(id)) => {
                out.send(&json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
            }
            ("notifications/cancelled", None) => {
                let target = params["requestId"].to_string();
                log.write(&format!("CANCELLATION of {target} · reason {}", params["reason"]));
                if let Some(a) = in_flight.get(&target) {
                    a.store(true, Ordering::SeqCst);
                }
            }
            (_, None) => {}
            (other, Some(id)) => {
                out.send(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("unknown method: {other}") }
                }));
            }
        }
    }
    let still = in_flight.values().filter(|a| !a.load(Ordering::SeqCst)).count();
    log.write(&format!("END OF STDIN · {still} sleep call(s) started without cancellation; exiting in 2 s"));
    std::thread::sleep(Duration::from_secs(2));
    log.write("EXIT");
}
