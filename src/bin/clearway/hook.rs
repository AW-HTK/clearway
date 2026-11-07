//! `clearway hook`: PreToolUse hook of Claude Code and Codex. Reads the event on stdin, refuses a heavy shell command
//! while the session holds no active ticket, and lets everything through when Clearway cannot be asked in time.

use std::io::{BufReader, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use clearway::hook::{self, Verdict};
use clearway::model::Status;
use clearway::protocol::{DEFAULT_PIPE, Request, Response, read_line_limited};
use clearway::tr;
use serde_json::Value;

const INPUT_MAX: u64 = 1024 * 1024;
const SERVER_TIMEOUT: Duration = Duration::from_secs(2);

pub fn main(_args: &[String]) -> i32 {
    if std::env::var("CLEARWAY_HOOK").is_ok_and(|v| v.eq_ignore_ascii_case("off")) {
        return 0;
    }
    let mut raw = String::new();
    if std::io::stdin().take(INPUT_MAX).read_to_string(&mut raw).is_err() {
        return 0;
    }
    let Ok(input) = serde_json::from_str::<Value>(&raw) else { return 0 };
    let Some((session_id, command)) = hook::shell_command(&input) else { return 0 };
    let Some(what) = hook::heavy(&command) else { return 0 };

    let pipe = std::env::var("CLEARWAY_PIPE").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| DEFAULT_PIPE.into());
    let status = match status(pipe) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", tr!("clearway hook: Clearway not reachable ({e}), command allowed", "clearway hook : Clearway injoignable ({e}), commande permise"));
            return 0;
        }
    };
    if let Verdict::Deny(reason) = hook::decide(&what, &command, &session_id, &status.tickets) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", hook::deny_output(&reason));
        let _ = out.flush();
    }
    0
}

/// `GET /status`, abandoned after `SERVER_TIMEOUT`: the hook must never hold the session up.
fn status(pipe: String) -> Result<Status, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ask_status(&pipe));
    });
    let response = rx.recv_timeout(SERVER_TIMEOUT).map_err(|_| tr!("no answer within 2 s", "pas de réponse en 2 s"))??;
    if response.code != 200 {
        return Err(format!("HTTP {}", response.code));
    }
    serde_json::from_value(response.body).map_err(|e| e.to_string())
}

fn exchange(reader: impl Read, mut writer: impl Write) -> Result<Response, String> {
    let request = serde_json::to_string(&Request::new("GET", "/status", Value::Null)).map_err(|e| e.to_string())?;
    writeln!(writer, "{request}").and_then(|()| writer.flush()).map_err(|e| e.to_string())?;
    let line = read_line_limited(&mut BufReader::new(reader))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| tr!("connection closed", "connexion fermée"))?;
    serde_json::from_str(&line).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn ask_status(pipe: &str) -> Result<Response, String> {
    use std::process::{Command, Stdio};
    let exe = crate::client::find_exe()?;
    let mut cmd = Command::new(&exe);
    cmd.args(["relay", "--pipe", pipe]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // A Windows exe started from a Linux directory would inherit a UNC path.
    if exe.starts_with("/mnt/") {
        cmd.current_dir("/mnt/c");
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let (Some(reader), Some(writer)) = (child.stdout.take(), child.stdin.take()) else {
        let _ = child.kill();
        return Err(tr!("relay without pipes", "relais sans tubes"));
    };
    let answer = exchange(reader, writer);
    let _ = child.kill();
    let _ = child.wait();
    answer
}

#[cfg(windows)]
fn ask_status(pipe: &str) -> Result<Response, String> {
    let (reader, writer) = crate::transport::connect(pipe).map_err(|e| e.to_string())?;
    exchange(reader, writer)
}
