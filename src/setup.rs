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

/// Shell script printing `claude=<path>` and `codex=<path>` (empty when absent) inside a distribution.
pub const WSL_PROBE: &str = r#"for t in claude codex; do
p=$(command -v "$t" 2>/dev/null || "${SHELL:-sh}" -lc "command -v $t" 2>/dev/null || true)
[ -z "$p" ] && [ -x "$HOME/.local/bin/$t" ] && p="$HOME/.local/bin/$t"
echo "$t=$p"
done"#;

/// `(claude, codex)` paths from the output of `WSL_PROBE`.
pub fn probe_result(out: &str) -> (Option<String>, Option<String>) {
    let get = |name: &str| {
        out.lines()
            .filter_map(|l| l.trim().strip_prefix(name)?.strip_prefix('='))
            .map(str::trim)
            .find(|p| p.starts_with('/'))
            .map(str::to_owned)
    };
    (get("claude"), get("codex"))
}

/// Quotes `s` for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests;
