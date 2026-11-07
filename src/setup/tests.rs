use super::*;

fn utf16(s: &str, bom: bool) -> Vec<u8> {
    let mut v = if bom { vec![0xff, 0xfe] } else { vec![] };
    v.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
    v
}

#[test]
fn wsl_list_in_utf16_without_docker_desktop() {
    let raw = utf16("Ubuntu\r\ndocker-desktop\r\nDebian\r\n\r\n", false);
    assert_eq!(wsl_names(&raw), ["Ubuntu", "Debian"]);
    assert_eq!(wsl_names(&utf16("Ubuntu-24.04\r\n", true)), ["Ubuntu-24.04"]);
    assert_eq!(wsl_names(b"Ubuntu\nFedora\n"), ["Ubuntu", "Fedora"]);
    assert!(wsl_names(&[]).is_empty());
}

#[test]
fn probe_keeps_absolute_paths_only() {
    let out = "claude=/home/u/.local/bin/claude\ncodex=\n";
    assert_eq!(probe_result(out), (Some("/home/u/.local/bin/claude".into()), None));
    assert_eq!(probe_result("claude=claude: not found\ncodex=/usr/bin/codex"), (None, Some("/usr/bin/codex".into())));
}

#[test]
fn quoting_survives_single_quotes() {
    assert_eq!(sh_quote("/home/o'neil/bin/claude"), r"'/home/o'\''neil/bin/claude'");
}
