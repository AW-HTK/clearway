//! What `clearway-setup` reads from Windows and WSL; pure so that it is tested on Linux.

/// Distributions printed by `wsl.exe -l -q` (UTF-16LE, sometimes with a BOM), Docker Desktop's own excluded.
pub fn wsl_names(raw: &[u8]) -> Vec<String> {
    let text = if raw.len() >= 2 && raw.iter().skip(1).step_by(2).take(8).all(|b| *b == 0) || raw.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0').to_owned())
        .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
        .collect()
}

/// Shell script printing `claude=<path>`, `codex=<path>` and `clearway=<path>` (empty when absent) inside a
/// distribution; the client is looked for in `CLEARWAY_BIN_DIR` when set, as install.sh does.
pub const WSL_PROBE: &str = r#"for t in claude codex clearway; do
p=$(command -v "$t" 2>/dev/null || "${SHELL:-sh}" -lc "command -v $t" 2>/dev/null || true)
[ -z "$p" ] && [ -x "$HOME/.local/bin/$t" ] && p="$HOME/.local/bin/$t"
if [ "$t" = clearway ] && [ -n "${CLEARWAY_BIN_DIR:-}" ]; then
p=; [ -x "$CLEARWAY_BIN_DIR/clearway" ] && p="$CLEARWAY_BIN_DIR/clearway"
fi
echo "$t=$p"
done"#;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub claude: Option<String>,
    pub codex: Option<String>,
    pub clearway: Option<String>,
}

/// Paths from the output of `WSL_PROBE`.
pub fn probe_result(out: &str) -> Probe {
    let get = |name: &str| {
        out.lines()
            .filter_map(|l| l.trim().strip_prefix(name)?.strip_prefix('='))
            .map(str::trim)
            .find(|p| p.starts_with('/'))
            .map(str::to_owned)
    };
    Probe { claude: get("claude"), codex: get("codex"), clearway: get("clearway") }
}

/// Expands `{NAME}` from `var` and `{DRIVE}` (one result per drive, e.g. `D:`); a pattern naming an unknown variable
/// gives nothing.
pub fn expand(pattern: &str, var: &dyn Fn(&str) -> Option<String>, drives: &[String]) -> Vec<String> {
    let roots: Vec<String> = if pattern.contains("{DRIVE}") {
        drives.iter().map(|d| pattern.replace("{DRIVE}", d.trim_end_matches(['\\', '/']))).collect()
    } else {
        vec![pattern.to_owned()]
    };
    roots
        .into_iter()
        .filter_map(|mut p| {
            while let Some(start) = p.find('{') {
                let end = start + p[start..].find('}')?;
                let value = var(&p[start + 1..end]).filter(|v| !v.is_empty())?;
                p.replace_range(start..=end, value.trim_end_matches(['\\', '/']));
            }
            Some(p)
        })
        .collect()
}

fn wildcard(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.to_lowercase(), name.to_lowercase());
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => name.len() >= head.len() + tail.len() && name.starts_with(head) && wildcard_tail(tail, &name[head.len()..]),
    }
}

fn wildcard_tail(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => name.ends_with(pattern),
        Some((part, rest)) => (0..=name.len()).filter(|i| name.is_char_boundary(*i)).any(|i| {
            name[i..].starts_with(part) && wildcard_tail(rest, &name[i + part.len()..])
        }),
    }
}

/// Existing files matching `pattern`, where `*` stands within one path component: only the folders on the way are
/// listed, never a whole disk. The most recently modified comes first.
pub fn find_files(pattern: &str) -> Vec<std::path::PathBuf> {
    use std::path::{Component, PathBuf};
    let path = std::path::Path::new(pattern);
    let mut found: Vec<PathBuf> = vec![PathBuf::new()];
    for component in path.components() {
        let part = component.as_os_str().to_string_lossy().into_owned();
        let wild = matches!(component, Component::Normal(_)) && part.contains('*');
        found = found
            .into_iter()
            .flat_map(|base| {
                if !wild {
                    return vec![base.join(component.as_os_str())];
                }
                std::fs::read_dir(&base)
                    .map(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .filter(|e| wildcard(&part, &e.file_name().to_string_lossy()))
                            .map(|e| base.join(e.file_name()))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .filter(|p| p.exists())
            .collect();
        if found.is_empty() {
            break;
        }
    }
    let modified = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    found.retain(|p| p.is_file());
    found.sort_by_key(|p| std::cmp::Reverse(modified(p)));
    found
}

/// Trust of Clearway's hook in Codex: a hook must be approved in `/hooks` before Codex runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookTrust {
    Trusted,
    /// Never approved.
    Untrusted,
    /// Approved for another version of the hook: to approve again.
    Modified,
    /// Codex does not list the hook (plugin not loaded).
    Absent,
}

/// Lines sent to `codex app-server` on stdin to list the hooks seen from `cwd`.
pub fn codex_hooks_request(cwd: &str) -> String {
    let lines = [
        serde_json::json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "clearway-setup", "version": env!("CARGO_PKG_VERSION")}}}),
        serde_json::json!({"method": "initialized"}),
        serde_json::json!({"id": 2, "method": "hooks/list", "params": {"cwds": [cwd]}}),
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Trust of the clearway PreToolUse hook in the answer of `codex app-server` to `hooks/list`; `None` before the answer.
pub fn hook_trust_from_list(out: &str) -> Option<HookTrust> {
    let answer = out.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find(|m| m["id"] == 2)?;
    let hooks = answer["result"]["data"].as_array()?.iter().filter_map(|e| e["hooks"].as_array()).flatten();
    let ours: Vec<&serde_json::Value> = hooks
        .filter(|h| h["pluginId"] == "clearway@clearway" && h["eventName"].as_str().is_some_and(|e| e.eq_ignore_ascii_case("pre_tool_use") || e.eq_ignore_ascii_case("preToolUse")))
        .collect();
    let status = |s: &str| ours.iter().any(|h| h["trustStatus"] == s);
    Some(if ours.is_empty() {
        HookTrust::Absent
    } else if status("modified") {
        HookTrust::Modified
    } else if status("untrusted") {
        HookTrust::Untrusted
    } else {
        HookTrust::Trusted
    })
}

/// Fallback when `codex app-server` does not answer: an approval recorded in `config.toml`, perhaps for an older
/// version of the hook (the key carries no version).
pub fn hook_trust_from_config(config: &str) -> HookTrust {
    let mut ours = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            let key = line.trim_start_matches('[').trim_end_matches(']').trim_start_matches("hooks.state.").trim_matches('"');
            ours = line.starts_with("[hooks.state.") && key.starts_with("clearway@clearway:") && key.contains(":pre_tool_use:");
        } else if ours && line.starts_with("trusted_hash") && line.contains('=') {
            return HookTrust::Trusted;
        }
    }
    HookTrust::Untrusted
}

/// Quotes `s` for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests;
