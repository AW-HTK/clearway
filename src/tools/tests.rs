use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;

use super::*;
use crate::mcp;
use crate::model::{DecisionKind, Reason};

/// Fake Clearway: one ticket store, a ticket is cleared after `polls_before_clearance` waits; every request is recorded
/// with what had been written on the MCP output at that moment.
struct Fake {
    polls_before_clearance: u32,
    poll_delay: Duration,
    log: Mutex<Vec<(Request, String)>>,
    written: Arc<Mutex<String>>,
    polls: Mutex<u32>,
    next_id: Mutex<u64>,
    state: Mutex<String>,
}

impl Fake {
    fn new(polls_before_clearance: u32, poll_delay: Duration, written: Arc<Mutex<String>>) -> Arc<Fake> {
        Arc::new(Fake {
            polls_before_clearance,
            poll_delay,
            log: Mutex::default(),
            written,
            polls: Mutex::new(0),
            next_id: Mutex::new(41),
            state: Mutex::new("waiting".into()),
        })
    }

    fn view(&self, id: u64, position: usize) -> Value {
        let state = self.state.lock().unwrap().clone();
        let mut reason = Reason::simple(
            if state == "active" { DecisionKind::Cleared } else { DecisionKind::Waiting },
            format!("#{id} {state}, position {position}"),
        );
        reason.position = (state == "waiting").then_some(position);
        serde_json::to_value(View { id, state, epoch: 1, reason }).unwrap()
    }

    fn requests(&self, method: &str, path_end: &str) -> Vec<(Request, String)> {
        self.log.lock().unwrap().iter().filter(|(r, _)| r.method == method && r.path.ends_with(path_end)).cloned().collect()
    }
}

impl Channel for Fake {
    fn call(&self, request: Request, _timeout: Duration) -> Result<Response, String> {
        let written = self.written.lock().unwrap().clone();
        self.log.lock().unwrap().push((request.clone(), written));
        let ok = |body: Value| Ok(Response { id: None, code: 200, body });
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/tickets") => {
                let ram = request.body["ram_mb"].as_u64().unwrap_or(0);
                if ram > 100 * 1024 {
                    let reason = Reason::simple(DecisionKind::Refused, "Refused: impossible.".into());
                    let body = serde_json::to_value(View { id: 0, state: "refused".into(), epoch: 1, reason }).unwrap();
                    return Ok(Response { id: None, code: 422, body });
                }
                let mut next = self.next_id.lock().unwrap();
                *next += 1;
                if self.polls_before_clearance == 0 {
                    *self.state.lock().unwrap() = "active".into();
                }
                Ok(Response { id: None, code: 201, body: self.view(*next, 3) })
            }
            ("GET", p) if p.ends_with("/wait") => {
                std::thread::sleep(self.poll_delay);
                let mut polls = self.polls.lock().unwrap();
                *polls += 1;
                if *polls >= self.polls_before_clearance {
                    *self.state.lock().unwrap() = "active".into();
                }
                let id = p.split('/').nth(2).unwrap().parse().unwrap();
                ok(self.view(id, 3usize.saturating_sub(*polls as usize).max(1)))
            }
            ("GET", "/status") => ok(json!({
                "epoch": 1, "now": 0, "measures": { "ram_total_mb": 1, "ram_avail_mb": 1, "commit_mb": 0, "commit_limit_mb": 0, "cpu_pct": 0.0, "vmmem": null },
                "reserves": [{ "key": "ram_windows", "name": "Windows RAM", "total": 32768, "available": 9000, "reserve": 0, "margin": 1536, "free": 7464 }],
                "tickets": [], "alerts": []
            })),
            _ => ok(json!({ "message": format!("{} {} done", request.method, request.path) })),
        }
    }
}

/// MCP output: a real pipe, with a copy of everything written (to check what was written before an acknowledgement).
struct Recorder {
    pipe: std::io::PipeWriter,
    copy: Arc<Mutex<String>>,
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.pipe.write(buf)?;
        self.copy.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.pipe.flush()
    }
}

struct Client {
    input: Option<std::io::PipeWriter>,
    lines: mpsc::Receiver<Value>,
    server: Option<std::thread::JoinHandle<()>>,
    fake: Arc<Fake>,
}

impl Client {
    fn start(polls_before_clearance: u32, poll_delay: Duration) -> Client {
        let (input_reader, input) = std::io::pipe().unwrap();
        let (output_reader, output_writer) = std::io::pipe().unwrap();
        let written = Arc::new(Mutex::new(String::new()));
        let fake = Fake::new(polls_before_clearance, poll_delay, written.clone());
        let settings = Settings { default_max_s: None, slice_s: 1, progress_every_s: 60, retry_pause: Duration::from_millis(10), default_side: Side::Wsl, locate: Some(|d| (Some(format!("project of {d}")), Some("feat/x".into()))) };
        let session = SessionInfo { name: "test".into(), dir: "/".into(), ..SessionInfo::default() };
        let tools: Arc<dyn mcp::Provider> = Arc::new(Tools::new(fake.clone(), session, settings));
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output_reader).lines().map_while(Result::ok) {
                let _ = tx.send(serde_json::from_str(&line).unwrap());
            }
        });
        let server = std::thread::spawn(move || {
            let recorder = Recorder { pipe: output_writer, copy: written };
            mcp::serve(tools, &Tools::<Fake>::info(), BufReader::new(input_reader), recorder, &|_| {});
        });
        let mut c = Client { input: Some(input), lines, server: Some(server), fake };
        c.send(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {} } }));
        assert_eq!(c.next()["result"]["protocolVersion"], "2025-06-18");
        c
    }

    fn send(&mut self, m: Value) {
        writeln!(self.input.as_mut().unwrap(), "{m}").unwrap();
    }

    fn next(&self) -> Value {
        self.lines.recv_timeout(Duration::from_secs(10)).expect("an MCP message")
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments, "_meta": { "progressToken": format!("p{id}") } } }));
    }

    /// Messages up to the response to `id`; progress notifications are returned with it.
    fn until_response(&self, id: u64) -> (Vec<Value>, Value) {
        let mut progress = Vec::new();
        loop {
            let m = self.next();
            if m["id"] == id {
                return (progress, m);
            }
            if m["method"] == "notifications/progress" {
                progress.push(m);
            }
        }
    }
}

#[test]
fn tool_list_and_instructions() {
    let mut c = Client::start(1, Duration::ZERO);
    c.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }));
    let names: Vec<String> =
        c.next()["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, ["request_clearance", "await_clearance", "release", "extend", "cancel", "status"]);
    assert!(Tools::<Fake>::info().instructions.contains("request_clearance"));
}

#[test]
fn request_blocks_with_progress_then_acknowledges_after_the_response() {
    let mut c = Client::start(3, Duration::from_millis(20));
    c.call(7, "request_clearance", json!({ "description": "build", "ram": "6G", "duration": "30m" }));
    let (progress, response) = c.until_response(7);
    let s = &response["result"]["structuredContent"];
    assert_eq!((s["cleared"].as_bool(), s["ticket"].as_u64(), s["state"].as_str()), (Some(true), Some(42), Some("active")), "{response}");
    assert!(s["next_step"].as_str().unwrap().contains("release(ticket=42"), "{s}");
    assert!(progress.len() >= 3, "a progress per change of reason: {progress:?}");
    assert!(progress.iter().all(|p| p["params"]["progressToken"] == "p7"));

    let created = c.fake.requests("POST", "/tickets");
    assert_eq!((created[0].0.body["manual"].as_bool(), created[0].0.body["ack"].as_bool()), (Some(true), Some(true)));
    assert_eq!(created[0].0.body["ram_mb"], 6 * 1024);
    assert_eq!(created[0].0.body["dir"], "/", "without `dir`, the session's folder");
    let waits = c.fake.requests("GET", "/wait");
    assert!(waits.iter().all(|(r, _)| r.param("ack") == Some("1") && r.param("timeout_s") == Some("1")));
    // The acknowledgement leaves only once the response line has been written.
    let mut acks = Vec::new();
    for _ in 0..100 {
        acks = c.fake.requests("POST", "/42/ack");
        if !acks.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(acks.len(), 1);
    assert!(acks[0].1.contains("\"id\":7"), "response written before the ack");
}

#[test]
fn ping_is_answered_during_a_wait() {
    let mut c = Client::start(4, Duration::from_millis(150));
    c.call(3, "request_clearance", json!({ "description": "tests" }));
    std::thread::sleep(Duration::from_millis(100));
    c.send(json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }));
    let (_, pong) = c.until_response(4);
    assert_eq!(pong["result"], json!({}));
    let (_, response) = c.until_response(3);
    assert_eq!(response["result"]["structuredContent"]["cleared"], true);
}

#[test]
fn cancellation_keeps_the_ticket_and_await_resumes_it() {
    let mut c = Client::start(6, Duration::from_millis(50));
    c.call(5, "request_clearance", json!({ "description": "render" }));
    std::thread::sleep(Duration::from_millis(120));
    c.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 5, "reason": "AbortError: user-cancel" } }));
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(m) = c.lines.recv_timeout(Duration::from_millis(100)) {
        assert_ne!(m["id"], 5, "no response to a cancelled call: {m}");
    }
    assert!(c.fake.requests("DELETE", "/42").is_empty(), "the ticket stays in the queue");
    assert!(c.fake.requests("POST", "/ack").is_empty());

    c.call(6, "await_clearance", json!({}));
    let (_, response) = c.until_response(6);
    let s = &response["result"]["structuredContent"];
    assert_eq!((s["cleared"].as_bool(), s["ticket"].as_u64()), (Some(true), Some(42)), "{response}");
}

#[test]
fn max_s_returns_still_waiting_with_the_next_step() {
    let mut c = Client::start(1000, Duration::from_millis(20));
    c.call(8, "request_clearance", json!({ "description": "long", "max_s": 1 }));
    let (_, response) = c.until_response(8);
    let s = &response["result"]["structuredContent"];
    assert_eq!((s["cleared"].as_bool(), s["state"].as_str()), (Some(false), Some("waiting")), "{response}");
    assert!(s["next_step"].as_str().unwrap().contains("await_clearance(ticket=42)"), "{s}");
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn refusal_is_not_an_error() {
    let mut c = Client::start(1, Duration::ZERO);
    c.call(9, "request_clearance", json!({ "description": "too big", "ram": "200G" }));
    let (_, response) = c.until_response(9);
    let s = &response["result"]["structuredContent"];
    assert_eq!((s["cleared"].as_bool(), s["state"].as_str()), (Some(false), Some("refused")), "{response}");
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn immediate_clearance_and_simple_tools() {
    let mut c = Client::start(0, Duration::ZERO);
    c.call(1, "request_clearance", json!({ "description": "quick", "side": "windows", "cpu": 2 }));
    let (_, r) = c.until_response(1);
    assert_eq!(r["result"]["structuredContent"]["cleared"], true);
    c.call(2, "release", json!({ "ticket": 42, "code": 0 }));
    assert!(c.until_response(2).1["result"]["content"][0]["text"].as_str().unwrap().contains("/tickets/42/release"));
    c.call(3, "extend", json!({ "ticket": "#42", "duration": "20m", "reason": "slower" }));
    c.until_response(3);
    let extend = c.fake.requests("POST", "/42/extend");
    assert_eq!((extend[0].0.body["duration_s"].as_u64(), extend[0].0.body["reason"].as_str()), (Some(1200), Some("slower")));
    c.call(4, "cancel", json!({ "ticket": 42 }));
    c.until_response(4);
    assert_eq!(c.fake.requests("DELETE", "/tickets/42").len(), 1);
    c.call(5, "status", json!({}));
    let text = c.until_response(5).1["result"]["content"][0]["text"].as_str().unwrap().to_owned();
    assert!(text.contains("Windows RAM"), "{text}");
    c.call(6, "release", json!({}));
    assert_eq!(c.until_response(6).1["error"]["code"], -32602, "missing ticket: protocol error");
}

#[test]
fn end_of_stdin_stops_the_server_during_a_wait() {
    let mut c = Client::start(1000, Duration::from_millis(30));
    c.call(1, "request_clearance", json!({ "description": "abandoned" }));
    std::thread::sleep(Duration::from_millis(100));
    drop(c.input.take());
    let server = c.server.take().unwrap();
    let began = std::time::Instant::now();
    server.join().unwrap();
    assert!(began.elapsed() < Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(200));
    assert!(c.fake.requests("DELETE", "").is_empty() && c.fake.requests("POST", "/ack").is_empty());
}

#[test]
fn a_request_names_the_worktree_it_runs_in() {
    let mut c = Client::start(1, Duration::ZERO);
    c.call(3, "request_clearance", json!({ "description": "tests", "dir": "/w/nuages-v2" }));
    c.until_response(3);
    let created = c.fake.requests("POST", "/tickets");
    let body = &created[0].0.body;
    assert_eq!((body["dir"].as_str(), body["project"].as_str(), body["branch"].as_str()), (Some("/w/nuages-v2"), Some("project of /w/nuages-v2"), Some("feat/x")));
}
