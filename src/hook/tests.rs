use serde_json::json;

use super::*;
use crate::model::{Need, Side};

fn ticket(id: u64, state: TicketState, identity: Option<&str>, claude: Option<&str>) -> TicketView {
    TicketView {
        id,
        state,
        side: Side::Wsl,
        need: Need { ram_mb: 1024, cpu: 2, duration_s: 600 },
        description: "build".into(),
        session: "s".into(),
        created: 0,
        granted: None,
        manual: false,
        force: false,
        lease_overrun: false,
        project: None,
        branch: None,
        presence_age_s: 0,
        message: String::new(),
        identity: identity.map(str::to_owned),
        claude_session_id: claude.map(str::to_owned),
    }
}

#[test]
fn heavy_commands_are_found_behind_wrappers_and_chains() {
    let cases = [
        ("cargo build --release -j 4", Some("cargo build")),
        ("cargo +nightly test", Some("cargo test")),
        ("cd ~/dev/x && RUST_LOG=debug cargo t", Some("cargo t")),
        ("timeout 600 nice -n 10 make -j8", Some("make")),
        ("sudo -E env FOO=1 ./gradlew assemble", Some("gradlew")),
        ("npm run build:prod", Some("npm run")),
        ("pnpm -r test", Some("pnpm test")),
        ("yarn e2e", Some("yarn e2e")),
        ("npx vitest run", Some("vitest")),
        ("pnpm exec playwright test", Some("playwright")),
        ("uv run pytest -x", Some("pytest")),
        ("python3 -m pytest tests/", Some("python3 -m pytest")),
        ("docker compose up --build -d", Some("docker build")),
        ("docker build -t x .", Some("docker build")),
        ("cmake --build build", Some("cmake --build")),
        ("C:\\tools\\MSBuild.exe app.sln", Some("msbuild")),
        ("echo ok; go test ./...", Some("go test")),
        ("ls | grep x && git status", None),
        ("cargo fmt && cargo --version", None),
        ("npm install", None),
        ("docker ps", None),
        ("python3 script.py", None),
        ("clearway run \"build\" -- cargo build", None),
        ("echo cargo build", None),
    ];
    for (command, expected) in cases {
        assert_eq!(heavy(command).as_deref(), expected, "{command}");
    }
}

#[test]
fn only_shell_tools_are_judged() {
    let bash = json!({"session_id": "abc", "tool_name": "Bash", "tool_input": {"command": "cargo build"}});
    assert_eq!(shell_command(&bash), Some(("abc".into(), "cargo build".into())));
    let codex = json!({"session_id": "t1", "tool_name": "exec_command", "tool_input": {"command": ["cargo", "test"]}});
    assert_eq!(shell_command(&codex), Some(("t1".into(), "cargo test".into())));
    let mcp = json!({"session_id": "abc", "tool_name": "mcp__plugin_clearway_clearway__release", "tool_input": {}});
    assert_eq!(shell_command(&mcp), None);
}

#[test]
fn an_active_ticket_of_the_session_lets_the_command_through() {
    let tickets = [
        ticket(3, TicketState::Active, Some("claude:other"), Some("other")),
        ticket(4, TicketState::Active, Some("claude:abc"), Some("abc")),
    ];
    assert_eq!(decide("cargo build", "cargo build", "abc", &tickets), Verdict::Allow);
    let codex = [ticket(5, TicketState::Active, Some("codex:t1"), None)];
    assert_eq!(decide("cargo build", "cargo build", "t1", &codex), Verdict::Allow);
    let run = [ticket(6, TicketState::Active, None, Some("abc"))];
    assert_eq!(decide("cargo build", "cargo build", "abc", &run), Verdict::Allow);
}

#[test]
fn without_an_active_ticket_the_reason_says_what_to_do() {
    let others = [ticket(3, TicketState::Active, Some("claude:other"), Some("other"))];
    let Verdict::Deny(reason) = decide("cargo build", "cargo build", "abc", &others) else { panic!("must deny") };
    assert!(reason.contains("request_clearance") && reason.contains("clearway run"), "{reason}");
    let waiting = [ticket(7, TicketState::Waiting, Some("claude:abc"), Some("abc"))];
    let Verdict::Deny(reason) = decide("cargo build", "cargo build", "abc", &waiting) else { panic!("must deny") };
    assert!(reason.contains("#7") && reason.contains("await_clearance"), "{reason}");
    assert!(matches!(decide("cargo build", "cargo build", "", &others), Verdict::Deny(_)), "no session id, no owner");
}

#[test]
fn deny_output_follows_the_pre_tool_use_format() {
    let out = deny_output("why");
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(out["hookSpecificOutput"]["permissionDecisionReason"], "why");
}
