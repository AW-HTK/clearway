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
    let out = "claude=/home/u/.local/bin/claude\ncodex=\nclearway=/home/u/.local/bin/clearway\n";
    let p = probe_result(out);
    assert_eq!(p.claude.as_deref(), Some("/home/u/.local/bin/claude"));
    assert_eq!((p.codex, p.clearway.as_deref()), (None, Some("/home/u/.local/bin/clearway")));
    let p = probe_result("claude=claude: not found\ncodex=/usr/bin/codex");
    assert_eq!((p.claude, p.codex.as_deref(), p.clearway), (None, Some("/usr/bin/codex"), None));
}

#[test]
fn patterns_expand_variables_and_drives() {
    let var = |name: &str| match name {
        "APPDATA" => Some(r"C:\Users\a\AppData\Roaming\".to_owned()),
        "USERNAME" => Some("a".to_owned()),
        _ => None,
    };
    let drives = ["C:\\".to_owned(), "D:".to_owned()];
    assert_eq!(expand(r"{APPDATA}\Claude\x.exe", &var, &drives), [r"C:\Users\a\AppData\Roaming\Claude\x.exe"]);
    assert_eq!(expand(r"{DRIVE}\Users\{USERNAME}\c.exe", &var, &drives), [r"C:\Users\a\c.exe", r"D:\Users\a\c.exe"]);
    assert!(expand(r"{NOPE}\c.exe", &var, &drives).is_empty());
}

#[test]
fn files_are_found_through_wildcard_folders_newest_first() {
    let root = std::env::temp_dir().join(format!("clearway-find-{}", std::process::id()));
    let old = root.join("claude-code/2.1.9/aa");
    let new = root.join("claude-code/2.1.10/bb");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    std::fs::write(old.join("claude.exe"), b"1").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(new.join("claude.exe"), b"2").unwrap();
    std::fs::write(new.join("other.txt"), b"3").unwrap();
    let pattern = root.join("claude-code/*/*/claude.exe").display().to_string();
    let found = find_files(&pattern);
    assert_eq!(found, [new.join("claude.exe"), old.join("claude.exe")]);
    assert!(find_files(&root.join("nothing/*/claude.exe").display().to_string()).is_empty());
    assert_eq!(find_files(&root.join("claude-code/2.1.*/aa/claude.exe").display().to_string()), [old.join("claude.exe")]);
    for f in [old.join("claude.exe"), new.join("claude.exe"), new.join("other.txt")] {
        std::fs::remove_file(f).unwrap();
    }
    for d in [&old, &new, &root.join("claude-code/2.1.9"), &root.join("claude-code/2.1.10"), &root.join("claude-code"), &root] {
        std::fs::remove_dir(d).unwrap();
    }
}

#[test]
fn codex_trust_of_the_clearway_hook() {
    let answer = |status: &str| {
        format!(
            "{{\"id\":1,\"result\":{{}}}}\n{{\"id\":2,\"result\":{{\"data\":[{{\"cwd\":\"/h\",\"errors\":[],\"warnings\":[],\"hooks\":[{{\"key\":\"clearway@clearway:hooks/hooks.json:pre_tool_use:0:0\",\"pluginId\":\"clearway@clearway\",\"eventName\":\"preToolUse\",\"trustStatus\":\"{status}\"}}]}}]}}}}\n"
        )
    };
    assert_eq!(hook_trust_from_list(&answer("trusted")), Some(HookTrust::Trusted));
    assert_eq!(hook_trust_from_list(&answer("untrusted")), Some(HookTrust::Untrusted));
    assert_eq!(hook_trust_from_list(&answer("modified")), Some(HookTrust::Modified));
    let none = "{\"id\":2,\"result\":{\"data\":[{\"hooks\":[]}]}}";
    assert_eq!(hook_trust_from_list(none), Some(HookTrust::Absent));
    assert_eq!(hook_trust_from_list("{\"id\":1,\"result\":{}}"), None);
    assert!(codex_hooks_request("/home/u").lines().count() == 3);

    let config = "[hooks.state.\"clearway@clearway:hooks/hooks.json:pre_tool_use:0:0\"]\ntrusted_hash = \"sha256:ab\"\n";
    assert_eq!(hook_trust_from_config(config), HookTrust::Trusted);
    let other = "[hooks.state.\"/home/u/x/.codex/hooks.json:pre_tool_use:0:0\"]\ntrusted_hash = \"sha256:ab\"\n";
    assert_eq!(hook_trust_from_config(other), HookTrust::Untrusted);
}

#[test]
fn quoting_survives_single_quotes() {
    assert_eq!(sh_quote("/home/o'neil/bin/claude"), r"'/home/o'\''neil/bin/claude'");
}
