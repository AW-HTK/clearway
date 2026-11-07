//! Command guard for the PreToolUse hook of Claude Code and Codex: a heavy shell command is refused until the
//! session holds an active ticket. Everything here is pure; `clearway hook` does the input and the server call.

use serde_json::{Value, json};

use crate::model::{TicketState, TicketView};
use crate::tr;

/// Tools whose input carries a shell command.
const SHELL_TOOLS: [&str; 5] = ["Bash", "PowerShell", "shell", "exec_command", "local_shell"];

/// Prefixes that run the real command after them.
const WRAPPERS: [&str; 10] = ["sudo", "time", "nice", "ionice", "nohup", "env", "exec", "command", "stdbuf", "chrt"];

/// Heavy whatever their arguments.
const ALWAYS: [&str; 26] = [
    "rustc", "make", "gmake", "nmake", "mingw32-make", "ninja", "msbuild", "bazel", "bazelisk", "mvn", "mvnw",
    "gradle", "gradlew", "pytest", "tox", "nox", "vitest", "jest", "playwright", "cypress", "webpack", "tsc",
    "turbo", "nx", "blender", "ffmpeg",
];

/// Heavy with one of these first arguments.
const BY_SUBCOMMAND: [(&str, &[&str]); 12] = [
    (
        "cargo",
        &[
            "build", "b", "test", "t", "bench", "clippy", "check", "c", "run", "r", "doc", "install", "zigbuild",
            "nextest", "llvm-cov", "tarpaulin", "xwin",
        ],
    ),
    ("go", &["build", "test", "install", "generate"]),
    ("dotnet", &["build", "test", "publish", "pack", "run"]),
    ("swift", &["build", "test"]),
    ("vite", &["build"]),
    ("next", &["build"]),
    ("nuxt", &["build", "generate"]),
    ("astro", &["build"]),
    ("ng", &["build", "test"]),
    ("ollama", &["run", "pull", "create", "serve"]),
    ("flutter", &["build", "test"]),
    ("xcodebuild", &["build", "test", "archive"]),
];

/// Runners whose next word is a command to judge again.
const RUNNERS: [(&str, &[&str]); 9] = [
    ("npx", &[]),
    ("bunx", &[]),
    ("pnpx", &[]),
    ("uvx", &[]),
    ("pnpm", &["exec", "dlx"]),
    ("yarn", &["dlx", "exec"]),
    ("uv", &["run"]),
    ("poetry", &["run"]),
    ("pipenv", &["run"]),
];

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

fn program(token: &str) -> String {
    let base = token.trim_matches(['"', '\'']).rsplit(['/', '\\']).next().unwrap_or(token).to_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_owned();
        }
    }
    base
}

fn is_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

fn first_word<'a>(words: &[&'a str]) -> Option<(usize, &'a str)> {
    words.iter().copied().enumerate().find(|(_, w)| !w.starts_with('-') && !w.starts_with('+'))
}

fn package_script(words: &[&str]) -> bool {
    let Some((i, sub)) = first_word(words) else { return false };
    let script = if sub == "run" || sub == "run-script" { first_word(&words[i + 1..]).map_or("", |(_, w)| w) } else { sub };
    matches!(script, "test" | "t" | "build" | "e2e")
        || ["build:", "test:", "e2e"].iter().any(|p| script.starts_with(p))
}

/// The heavy command found in one simple command (no `&&`, `|`…), as `program subcommand`.
fn heavy_words(words: &[&str], depth: u8) -> Option<String> {
    let mut i = 0;
    while i < words.len() && is_assignment(words[i]) {
        i += 1;
    }
    while i < words.len() {
        let p = program(words[i]);
        if !WRAPPERS.contains(&p.as_str()) && p != "timeout" {
            break;
        }
        i += 1;
        let digit = |w: &str| w.starts_with(|c: char| c.is_ascii_digit());
        while i < words.len() && (words[i].starts_with('-') || is_assignment(words[i]) || digit(words[i])) {
            i += 1;
        }
    }
    let (head, rest) = words[i..].split_first()?;
    let p = program(head);
    let label = |sub: Option<&str>| sub.map_or_else(|| p.clone(), |s| format!("{p} {s}"));
    if p == "clearway" || p == "charge" {
        return None;
    }
    if let Some(script) = inner_script(&p, rest) {
        return if depth < 3 { heavy_at(&script, depth + 1) } else { None };
    }
    if ALWAYS.contains(&p.as_str()) {
        return Some(label(None));
    }
    if let Some((_, subs)) = BY_SUBCOMMAND.iter().find(|(name, _)| *name == p) {
        let (_, sub) = first_word(rest)?;
        return subs.contains(&sub).then(|| label(Some(sub)));
    }
    if matches!(p.as_str(), "npm" | "pnpm" | "yarn" | "bun") && package_script(rest) {
        return Some(label(first_word(rest).map(|(_, w)| w)));
    }
    if p.starts_with("python") || p == "py" {
        let m = rest.iter().position(|w| *w == "-m")?;
        let module = *rest.get(m + 1)?;
        return matches!(module, "pytest" | "tox" | "nox" | "build" | "unittest").then(|| format!("{p} -m {module}"));
    }
    if p == "cmake" {
        return rest.contains(&"--build").then(|| label(Some("--build")));
    }
    if matches!(p.as_str(), "docker" | "podman" | "docker-compose") {
        let build = rest.iter().any(|w| *w == "build" || *w == "--build");
        return build.then(|| label(Some("build")));
    }
    if depth < 3
        && let Some((_, subs)) = RUNNERS.iter().find(|(name, _)| *name == p)
    {
        let start = if subs.is_empty() {
            0
        } else {
            let (k, sub) = first_word(rest)?;
            if !subs.contains(&sub) {
                return None;
            }
            k + 1
        };
        let after = &rest[start..];
        let (k, _) = first_word(after)?;
        return heavy_words(&after[k..], depth + 1);
    }
    None
}

/// Simple commands of a shell line (split on `&&`, `|`, `;`…) as unquoted words; quotes protect separators.
fn simple_commands(line: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut documents: Vec<(String, bool)> = Vec::new();
    let mut chars = line.chars().peekable();
    let end_word = |words: &mut Vec<String>, word: &mut String, started: &mut bool| {
        if *started {
            words.push(std::mem::take(word));
            *started = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                started = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if chars.peek().is_some_and(|n| matches!(n, '"' | '\\' | '$' | '`')) => {
                            word.extend(chars.next());
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' if chars.peek().is_some_and(|n| n.is_whitespace() || matches!(n, '\'' | '"' | '\\')) => {
                started = true;
                word.extend(chars.next());
            }
            '$' if chars.peek() == Some(&'(') => {}
            '<' if chars.peek() == Some(&'<') => {
                chars.next();
                if chars.peek() == Some(&'<') {
                    chars.next();
                    continue;
                }
                let tabs = chars.next_if_eq(&'-').is_some();
                while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
                let mut delimiter = String::new();
                while let Some(c) = chars.next_if(|c| !c.is_whitespace() && !";|&<>()".contains(*c)) {
                    if !matches!(c, '\'' | '"' | '\\') {
                        delimiter.push(c);
                    }
                }
                if !delimiter.is_empty() {
                    documents.push((delimiter, tabs));
                }
            }
            '\n' if !documents.is_empty() => {
                end_word(&mut words, &mut word, &mut started);
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
                for (delimiter, tabs) in documents.drain(..) {
                    loop {
                        let line: String = chars.by_ref().take_while(|c| *c != '\n').collect();
                        let line = if tabs { line.trim_start_matches('\t') } else { line.as_str() };
                        if line == delimiter || chars.peek().is_none() {
                            break;
                        }
                    }
                }
            }
            '\n' | ';' | '|' | '&' | '(' | ')' | '{' | '}' | '`' => {
                end_word(&mut words, &mut word, &mut started);
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            c if c.is_whitespace() => end_word(&mut words, &mut word, &mut started),
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    end_word(&mut words, &mut word, &mut started);
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

/// The script run by `sh -c '…'`, `pwsh -Command …` or `cmd /c …`, when `p` is such a shell.
fn inner_script(p: &str, rest: &[&str]) -> Option<String> {
    match p {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "fish" => {
            let mut i = 0;
            while let Some(w) = rest.get(i) {
                if !(w.starts_with('-') || w.starts_with('+')) || *w == "-" || *w == "--" {
                    return None;
                }
                if !w.starts_with("--") && w.contains('c') {
                    return rest.get(i + 1).map(|s| (*s).to_owned());
                }
                i += if matches!(*w, "-o" | "+o" | "-O" | "+O") { 2 } else { 1 };
            }
            None
        }
        "pwsh" | "powershell" => {
            let k = rest.iter().position(|w| {
                let w = w.to_lowercase();
                let flag = w.trim_start_matches(['-', '/']);
                w.len() > flag.len() && !flag.is_empty() && "command".starts_with(flag)
            })?;
            Some(rest[k + 1..].join(" ")).filter(|s| !s.is_empty())
        }
        "cmd" => {
            let k = rest.iter().position(|w| w.eq_ignore_ascii_case("/c") || w.eq_ignore_ascii_case("/k"))?;
            Some(rest[k + 1..].join(" ")).filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

fn heavy_at(line: &str, depth: u8) -> Option<String> {
    simple_commands(line).iter().find_map(|words| {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        heavy_words(&words, depth)
    })
}

/// The first heavy command of a shell line, e.g. `Some("cargo build")`.
pub fn heavy(command: &str) -> Option<String> {
    heavy_at(command, 0)
}

/// `(session_id, command)` of a PreToolUse event whose tool runs a shell command.
pub fn shell_command(input: &Value) -> Option<(String, String)> {
    let tool = input["tool_name"].as_str()?;
    if !SHELL_TOOLS.contains(&tool) {
        return None;
    }
    let command = match &input["tool_input"]["command"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(Value::as_str).map(quote).collect::<Vec<_>>().join(" "),
        _ => return None,
    };
    Some((input["session_id"].as_str().unwrap_or_default().to_owned(), command))
}

fn quote(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./:=+,@%".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn owned_by(t: &TicketView, session_id: &str, ancestors: &[u32]) -> bool {
    let same_session = !session_id.is_empty()
        && (t.claude_session_id.as_deref() == Some(session_id)
            || t.identity.as_deref().and_then(|k| k.split_once(':')).is_some_and(|(_, id)| id == session_id));
    // Codex gives its MCP servers no thread id: their tickets carry the Codex process, which also starts this hook.
    let same_codex = t
        .identity
        .as_deref()
        .and_then(|k| k.strip_prefix("codex-pid:"))
        .and_then(|p| p.parse::<u32>().ok())
        .is_some_and(|pid| ancestors.contains(&pid));
    same_session || same_codex
}

/// Allows the heavy command `what` when the session holds an active ticket; `ancestors`: pids of the hook's parents.
pub fn decide(what: &str, command: &str, session_id: &str, ancestors: &[u32], tickets: &[TicketView]) -> Verdict {
    let mine: Vec<&TicketView> = tickets.iter().filter(|t| owned_by(t, session_id, ancestors)).collect();
    if mine.iter().any(|t| t.state == TicketState::Active) {
        return Verdict::Allow;
    }
    let short: String = command.chars().take(120).collect();
    let wrap = tr!(
        "Or wrap it, which waits and releases by itself: clearway run \"<what>\" -- {short}",
        "Ou enveloppe-la, ce qui attend et libère tout seul : clearway run \"<quoi>\" -- {short}"
    );
    if let Some(t) = mine.iter().find(|t| t.state != TicketState::Active) {
        let id = t.id;
        return Verdict::Deny(tr!(
            "Clearway: `{what}` is a heavy command and ticket #{id} of this session is not cleared yet. Wait with \
             await_clearance(ticket={id}), run the command once cleared, then release(ticket={id}, code). {wrap}",
            "Clearway : `{what}` est une commande lourde et le ticket #{id} de cette session n'a pas encore le feu \
             vert. Attends avec await_clearance(ticket={id}), lance la commande au feu vert, puis release(ticket={id}, \
             code). {wrap}"
        ));
    }
    Verdict::Deny(tr!(
        "Clearway: `{what}` is a heavy command. First ask for a slot with request_clearance (clearway MCP server, \
         heavy-tasks skill), run the command once cleared, then call release. {wrap}",
        "Clearway : `{what}` est une commande lourde. Demande d'abord un créneau avec request_clearance (serveur MCP \
         clearway, skill heavy-tasks), lance la commande au feu vert, puis appelle release. {wrap}"
    ))
}

/// Hook output refusing the tool call, understood by Claude Code and Codex.
pub fn deny_output(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

#[cfg(test)]
mod tests;
