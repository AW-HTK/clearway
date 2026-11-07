//! Who an MCP server works for: the Claude Code or Codex session that started it, found from its environment or its
//! parent processes. The key lets a new MCP process of the same session resume its tickets.

use std::path::Path;

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// Executable and arguments; on Windows only the executable name.
    pub command: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub key: String,
    pub name: String,
    pub agent: Agent,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<u32>,
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

fn session_file(sessions: &Path, pid: u32) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(sessions.join(format!("{pid}.json"))).ok()?).ok()
}

fn named(v: &Value) -> Option<String> {
    v["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned)
}

fn is_codex(p: &Process) -> bool {
    p.command.iter().take(3).any(|a| {
        let base = a.rsplit(['/', '\\']).next().unwrap_or(a).to_lowercase();
        base == "codex" || base == "codex.exe" || base == "codex.js"
    })
}

/// `env`: the MCP process environment; `ancestors`: its parents, nearest first; `sessions`: `~/.claude/sessions`.
pub fn identify(env: &dyn Fn(&str) -> Option<String>, ancestors: &[Process], sessions: &Path, dir: &str) -> Identity {
    let var = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    let forced_name = var("CLEARWAY_SESSION").or_else(|| var("CHARGE_SESSION"));
    let dir_name = Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.to_owned());

    let claude_pid = var("CLAUDE_PID").and_then(|p| p.parse().ok());
    let claude = match var("CLAUDE_CODE_SESSION_ID") {
        Some(id) => Some((id, claude_pid.and_then(|p| session_file(sessions, p)), claude_pid)),
        None => ancestors.iter().find_map(|p| {
            let v = session_file(sessions, p.pid)?;
            let id = v["sessionId"].as_str().filter(|s| !s.is_empty())?.to_owned();
            Some((id, Some(v), Some(p.pid)))
        }),
    };
    if let Some((id, file, pid)) = claude {
        let name = forced_name.or_else(|| file.as_ref().and_then(named)).unwrap_or_else(|| format!("Claude {}", short(&id)));
        return Identity { key: format!("claude:{id}"), name, agent: Agent::Claude, claude_session_id: Some(id), claude_pid: pid };
    }
    let codex = |key: String, name: String| Identity {
        key,
        name: forced_name.clone().unwrap_or(name),
        agent: Agent::Codex,
        claude_session_id: None,
        claude_pid: None,
    };
    if let Some(id) = var("CODEX_THREAD_ID") {
        return codex(format!("codex:{id}"), format!("Codex {}", short(&id)));
    }
    if let Some(p) = ancestors.iter().find(|p| is_codex(p)) {
        return codex(format!("codex-pid:{}", p.pid), format!("Codex {dir_name}"));
    }
    let parent = ancestors.first().map_or(0, |p| p.pid);
    Identity {
        key: format!("pid:{parent}:{dir}"),
        name: forced_name.unwrap_or(dir_name),
        agent: Agent::Other,
        claude_session_id: None,
        claude_pid: None,
    }
}

/// Longest blocking wait for Codex, under the `tool_timeout_sec` of `[mcp_servers.<server>]` (60 s by default).
pub fn codex_max_s(config: Option<&str>, server: &str) -> u64 {
    let header = format!("[mcp_servers.{server}]");
    let mut inside = false;
    let mut timeout = None;
    for line in config.unwrap_or("").lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == header;
        } else if inside
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == "tool_timeout_sec"
        {
            timeout = v.split('#').next().and_then(|v| v.trim().parse::<f64>().ok()).map(|t| t as u64);
        }
    }
    let t = timeout.unwrap_or(60);
    if t > 120 { t - 60 } else { t * 3 / 4 }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    fn process(pid: u32, command: &[&str]) -> Process {
        Process { pid, command: command.iter().map(|s| s.to_string()).collect() }
    }

    fn sessions() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("clearway-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("3063.json"), r#"{"pid":3063,"sessionId":"1dbd7a7a-cc41","name":"RDEV"}"#).unwrap();
        std::fs::write(d.join("4000.json"), r#"{"pid":4000,"sessionId":"77aa","name":"  "}"#).unwrap();
        d
    }

    #[test]
    fn claude_from_its_environment_or_its_parents() {
        let s = sessions();
        let id = identify(&env(&[("CLAUDE_CODE_SESSION_ID", "1dbd7a7a-cc41"), ("CLAUDE_PID", "3063")]), &[], &s, "/a/b");
        assert_eq!((id.key.as_str(), id.name.as_str(), id.agent), ("claude:1dbd7a7a-cc41", "RDEV", Agent::Claude));
        assert_eq!(id.claude_pid, Some(3063));

        let parents = [process(12, &["node", "mcp"]), process(4000, &["claude"])];
        let id = identify(&env(&[]), &parents, &s, "/a/b");
        assert_eq!((id.key.as_str(), id.name.as_str(), id.claude_pid), ("claude:77aa", "Claude 77aa", Some(4000)));

        let id = identify(&env(&[("CLAUDE_CODE_SESSION_ID", "x"), ("CLEARWAY_SESSION", "lead")]), &[], &s, "/");
        assert_eq!(id.name, "lead");
        let _ = std::fs::remove_dir_all(s);
    }

    #[test]
    fn codex_from_its_thread_or_its_parent_command() {
        let s = std::env::temp_dir().join("clearway-no-sessions");
        let id = identify(&env(&[("CODEX_THREAD_ID", "019a2b3c4d5e")]), &[], &s, "/home/adel/dev/nuages-v2");
        assert_eq!((id.key.as_str(), id.name.as_str(), id.agent), ("codex:019a2b3c4d5e", "Codex 019a2b3c", Agent::Codex));

        let parents = [process(50, &["/usr/bin/node", "/home/adel/.npm/bin/codex", "--yolo"]), process(1, &["init"])];
        let id = identify(&env(&[]), &parents, &s, "/home/adel/dev/nuages-v2");
        assert_eq!((id.key.as_str(), id.name.as_str()), ("codex-pid:50", "Codex nuages-v2"));
        let windows = [process(70, &["codex.exe"])];
        assert_eq!(identify(&env(&[]), &windows, &s, "C:\\dev\\x").key, "codex-pid:70");

        let id = identify(&env(&[]), &[process(9, &["bash"])], &s, "/home/adel/dev/x");
        assert_eq!((id.key.as_str(), id.name.as_str(), id.agent), ("pid:9:/home/adel/dev/x", "x", Agent::Other));
    }

    #[test]
    fn codex_wait_stays_under_its_tool_timeout() {
        assert_eq!(codex_max_s(None, "clearway"), 45, "Codex default: 60 s");
        let config = "[mcp_servers.other]\ntool_timeout_sec = 10\n[mcp_servers.clearway]\ncommand = \"clearway\"\ntool_timeout_sec = 3600 # set by the installer\n";
        assert_eq!(codex_max_s(Some(config), "clearway"), 3540);
        assert_eq!(codex_max_s(Some("[mcp_servers.clearway]\ntool_timeout_sec = 100.0\n"), "clearway"), 75);
    }
}
