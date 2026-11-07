use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use clearway::protocol::{RELAY_ABSENT, socket_path};
use serde_json::{Value, json};

/// Short delays for the tests; French messages, as the assertions are written against them.
const FAST_CONFIG: &str = "margin.ram_windows = \"4G\"\n\
    delay.silence = \"3s\"\ndelay.unattended = \"4s\"\ndelay.absence = \"8s\"\ndelay.recall = \"3s\"\ndelay.long_poll = \"2s\"\n\
    delay.resume = \"3s\"\nlanguage = \"fr\"\n";

struct Orchestra {
    child: Option<Child>,
    dir: PathBuf,
    pipe: String,
}

fn wait<F: FnMut() -> bool>(max: Duration, mut condition: F) -> bool {
    let end = Instant::now() + max;
    while Instant::now() < end {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

impl Orchestra {
    fn prepare(name: &str) -> Orchestra {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pipe = format!("essai-{name}-{}", std::process::id());
        std::fs::write(dir.join("config.toml"), format!("pipe = \"{pipe}\"\n{FAST_CONFIG}")).unwrap();
        Orchestra { child: None, dir, pipe }
    }

    fn start(name: &str) -> Orchestra {
        let mut o = Orchestra::prepare(name);
        o.launch();
        o
    }

    fn process(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_clearway"));
        c.arg("serve").arg("--dir").arg(&self.dir).arg("--test").stdout(Stdio::null()).stderr(Stdio::null());
        c
    }

    fn answers(&self) -> bool {
        UnixStream::connect(socket_path(&self.pipe)).is_ok()
    }

    fn launch(&mut self) {
        self.child = Some(self.process().spawn().unwrap());
        assert!(wait(Duration::from_secs(10), || self.answers()), "Clearway does not answer on {}", self.pipe);
    }

    fn kill(&mut self) {
        if let Some(mut e) = self.child.take() {
            let _ = e.kill();
            let _ = e.wait();
        }
    }

    fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        self.call_with(method, path, json!({}), body)
    }

    fn call_with(&self, method: &str, path: &str, params: Value, body: Value) -> (u16, Value) {
        let mut stream = UnixStream::connect(socket_path(&self.pipe)).unwrap();
        let req = json!({ "method": method, "path": path, "params": params, "body": body });
        stream.write_all(format!("{req}\n").as_bytes()).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let rep: Value = serde_json::from_str(&line).unwrap();
        (rep["code"].as_u64().unwrap() as u16, rep["body"].clone())
    }

    fn create(&self, req_text: &str, side: &str, ram_mb: u64) -> (u16, Value) {
        self.call(
            "POST",
            "/tickets",
            json!({
                "request_id": req_text, "side": side, "ram_mb": ram_mb, "cpu": 1, "duration_s": 600,
                "description": format!("essai {req_text}"), "session": "test", "claude_session_id": null,
                "claude_pid": null, "dir": "/", "signature": "", "mesures": null
            }),
        )
    }

    fn raw_wait(&self, id: u64, ack: bool) -> (u16, Value) {
        let mut params = json!({ "known": "", "timeout_s": "2" });
        if ack {
            params["ack"] = json!("1");
        }
        self.call_with("GET", &format!("/tickets/{id}/wait"), params, Value::Null)
    }

    fn state(&self) -> Value {
        self.call("GET", "/status", Value::Null).1
    }

    fn ticket(&self, id: u64) -> Option<Value> {
        self.state()["tickets"].as_array().unwrap().iter().find(|t| t["id"] == id).cloned()
    }

    fn ticket_state(&self, id: u64) -> Option<String> {
        self.ticket(id).map(|t| t["state"].as_str().unwrap().to_owned())
    }

    fn cli(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_clearway"));
        c.args(args)
            .env("CLEARWAY_EXE", env!("CARGO_BIN_EXE_clearway"))
            .env("CLEARWAY_PIPE", &self.pipe)
            .env("CLEARWAY_DIR", &self.dir)
            .env("CLEARWAY_LAUNCH_ARGS", "--test")
            .env("CLEARWAY_SESSION", "test")
            .env("LC_ALL", "fr_FR.UTF-8")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_PID");
        c
    }

    fn out(&self, args: &[&str]) -> Output {
        self.cli(args).output().unwrap()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("clearway.log")).unwrap_or_default()
    }
}

/// Connection kept open, like an MCP process's: several requests, possibly in parallel.
struct Line {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Line {
    fn open(o: &Orchestra) -> Line {
        let stream = UnixStream::connect(socket_path(&o.pipe)).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        Line { stream, reader }
    }

    fn send(&mut self, req_text: Value) {
        self.stream.write_all(format!("{req_text}\n").as_bytes()).unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn call(&mut self, method: &str, path: &str, body: Value) -> Value {
        self.send(json!({ "method": method, "path": path, "body": body }));
        self.read()
    }

    fn open_link(&mut self, identity: &str) -> Value {
        self.call("POST", "/link", json!({ "identity": identity, "session": "essai MCP", "client": "test" }))
    }

    fn create(&mut self, req_text: &str, ram_mb: u64) -> Value {
        self.call(
            "POST",
            "/tickets",
            json!({
                "request_id": req_text, "side": "windows", "ram_mb": ram_mb, "cpu": 1, "duration_s": 600, "manual": true,
                "description": format!("essai {req_text}"), "session": "essai MCP", "claude_session_id": null,
                "claude_pid": null, "dir": "/", "signature": "", "mesures": null
            }),
        )
    }
}

impl Drop for Orchestra {
    fn drop(&mut self) {
        if self.child.is_some() && self.answers() {
            let _ = self.call("POST", "/quit", Value::Null);
            if let Some(e) = self.child.as_mut() {
                let _ = wait(Duration::from_secs(3), || matches!(e.try_wait(), Ok(Some(_))));
            }
        }
        self.kill();
    }
}

fn text(o: &[u8]) -> String {
    String::from_utf8_lossy(o).into_owned()
}

#[test]
fn request_wait_release_and_status() {
    let o = Orchestra::start("cycle");
    let a = o.out(&["request", "--side", "windows", "--ram", "10G", "tâche A"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert_eq!(text(&a.stdout).trim(), "1");
    assert!(text(&a.stderr).contains("feu vert"), "{}", text(&a.stderr));

    let b = o.out(&["request", "--side", "windows", "--ram", "8G", "session B", "tâche B"]);
    assert_eq!(text(&b.stdout).trim(), "2");
    assert!(text(&b.stderr).contains("RAM Windows insuffisante"), "{}", text(&b.stderr));
    assert!(text(&b.stderr).contains("Débloqué à la fin du #1"), "{}", text(&b.stderr));

    let waiting = o.cli(&["wait", "2"]).stdout(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let status_view = o.out(&["status"]);
    let s = text(&status_view.stdout);
    assert!(s.contains("#1") && s.contains("actif") && s.contains("(manuel)") && s.contains("#2"), "{s}");

    let f = o.out(&["release", "1"]);
    assert_eq!(f.status.code(), Some(0));
    assert!(text(&f.stdout).contains("terminé"), "{}", text(&f.stdout));
    let out = waiting.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let lines = text(&out.stdout);
    assert!(lines.contains("position 1"), "{lines}");
    assert!(lines.contains("FEU VERT pour le ticket #2"), "{lines}");

    assert_eq!(o.out(&["cancel", "2"]).status.code(), Some(0));
    assert_eq!(o.out(&["release", "2"]).status.code(), Some(1));
    let j = o.log();
    for expected in ["START époque 1", "REQUEST #1", "CLEARED #1", "WAITING #2", "RELEASED #1", "CLEARED #2", "CANCELLED #2"] {
        assert!(j.contains(expected), "journal sans « {expected} » :\n{j}");
    }
}

#[test]
fn immediate_refusal_and_exclusive_cpu() {
    let o = Orchestra::start("refus");
    let r = o.out(&["request", "--side", "windows", "--ram", "40G", "trop gros"]);
    assert_eq!(r.status.code(), Some(3));
    assert!(text(&r.stdout).is_empty());
    assert!(text(&r.stderr).contains("RAM Windows impossible"), "{}", text(&r.stderr));
    let r = o.out(&["run", "--cpu", "11", "trop de cœurs", "--", "true"]);
    assert_eq!(r.status.code(), Some(3));
    assert!(text(&r.stderr).contains("--cpu à 10 max"), "{}", text(&r.stderr));
    let r = o.out(&["run", "--cpu", "10", "--ram", "1G", "exclusif", "--", "true"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(o.state()["tickets"].as_array().unwrap().is_empty());
}

#[test]
fn run_launches_the_command_and_returns_its_code() {
    let o = Orchestra::start("run");
    let r = o.out(&["run", "--side", "windows", "--ram", "1G", "écho", "--", "sh", "-c", "echo bonjour; exit 7"]);
    assert_eq!(r.status.code(), Some(7), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout).trim(), "bonjour");
    assert!(text(&r.stderr).contains("feu vert #1"), "{}", text(&r.stderr));
    assert!(o.state()["tickets"].as_array().unwrap().is_empty());
    assert!(o.log().contains("code 7"), "{}", o.log());
}

#[test]
fn run_waits_its_turn() {
    let o = Orchestra::start("run-attente");
    let (code, a) = o.create("occupant", "windows", 12 * 1024);
    assert_eq!(code, 201);
    assert_eq!(a["state"], "active");
    let run = o
        .cli(&["run", "--side", "windows", "--ram", "8G", "second", "--", "sh", "-c", "echo parti"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(o.ticket_state(2).as_deref(), Some("waiting"));
    o.call("POST", "/tickets/1/release", json!({}));
    let out = run.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("en attente, position 1"), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), "parti");
    assert!(o.log().contains("ACK #2"), "{}", o.log());
}

#[test]
fn max_returns_75_and_the_same_command_resumes_its_ticket() {
    let o = Orchestra::start("rattachement");
    o.create("occupant", "windows", 12 * 1024);
    let args = ["run", "--side", "windows", "--ram", "8G", "--max", "2s", "patient", "--", "echo", "enfin"];
    let r = o.out(&args);
    assert_eq!(r.status.code(), Some(75), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("Relance la même commande"), "{}", text(&r.stderr));
    assert_eq!(o.ticket_state(2).as_deref(), Some("waiting"));

    let retry = o.cli(&args).stderr(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(o.state()["tickets"].as_array().unwrap().len(), 2, "pas de nouveau ticket");
    o.call("POST", "/tickets/1/release", json!({}));
    let out = retry.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("ticket #2 repris"), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim(), "enfin");

    // `wait --max`: code 75, and the place is still there.
    o.create("occupant-2", "windows", 12 * 1024);
    let d = o.out(&["request", "--side", "windows", "--ram", "8G", "manual"]);
    let id = text(&d.stdout).trim().to_owned();
    let a = o.out(&["wait", &id, "--max", "1s"]);
    assert_eq!(a.status.code(), Some(75));
    assert!(text(&a.stdout).contains(&format!("Relance « clearway wait {id} »")), "{}", text(&a.stdout));
    assert_eq!(o.ticket_state(id.parse().unwrap()).as_deref(), Some("waiting"));
}

#[test]
fn undelivered_clearance_recalled_then_reclaimed() {
    let o = Orchestra::start("rappel");
    o.create("occupant", "windows", 12 * 1024);
    let d = o.out(&["request", "--side", "windows", "--ram", "8G", "personne n'écoute"]);
    let id: u64 = text(&d.stdout).trim().parse().unwrap();
    o.call("POST", "/tickets/1/release", json!({}));
    assert_eq!(o.ticket_state(id).as_deref(), Some("active"));
    assert!(wait(Duration::from_secs(8), || o.ticket_state(id).as_deref() == Some("waiting")));
    assert!(o.log().contains(&format!("RECALLED #{id}")), "{}", o.log());
    let message = o.ticket(id).unwrap()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("feu vert rappelé"), "{message}");

    let a = o.out(&["wait", &id.to_string(), "--max", "10s"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stdout));
    assert!(text(&a.stdout).contains("FEU VERT"));
    assert!(o.log().contains(&format!("ACK #{id}")), "{}", o.log());
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(o.ticket_state(id).as_deref(), Some("active"), "remis : plus de rappel");
}

/// Clearance granted while no client listens: it waits to be claimed.
fn pending_clearance(o: &Orchestra) -> u64 {
    o.create("occupant", "windows", 12 * 1024);
    let (_, t) = o.create("suivant", "windows", 8 * 1024);
    assert_eq!(t["state"], "waiting", "{t}");
    o.call("POST", "/tickets/1/release", json!({}));
    let id = t["id"].as_u64().unwrap();
    assert_eq!(o.ticket_state(id).as_deref(), Some("active"));
    id
}

#[test]
fn clearance_acknowledgement() {
    let claim = |name: &str, ack: bool| {
        let o = Orchestra::start(name);
        let id = pending_clearance(&o);
        let (code, v) = o.raw_wait(id, ack);
        assert_eq!((code, v["state"].as_str()), (200, Some("active")), "{v}");
        (o, id)
    };
    let (old, id_old) = claim("accuse-ancien", false);
    let (without, id_without) = claim("accuse-sans", true);
    let (with, id_with) = claim("accuse-avec", true);
    let ids = [id_old, id_without, id_with];
    let (code, r) = with.call("POST", &format!("/tickets/{}/ack", ids[2]), Value::Null);
    assert_eq!(code, 200, "{r}");
    assert!(with.log().contains(&format!("ACK #{}", ids[2])), "{}", with.log());
    let (code, _) = with.call("POST", "/tickets/999/ack", Value::Null);
    assert_eq!(code, 404);

    // Clearance sent, never acknowledged within delay.recall (3 s): taken back, place kept.
    assert!(wait(Duration::from_secs(8), || without.ticket_state(ids[1]).as_deref() == Some("waiting")));
    assert!(without.log().contains(&format!("RECALLED #{}", ids[1])), "{}", without.log());
    let (code, r) = without.call("POST", &format!("/tickets/{}/ack", ids[1]), Value::Null);
    assert_eq!(code, 409, "{r}");
    assert!(r["error"].as_str().unwrap().contains("clearway wait"), "{r}");

    // The old client (response = delivery) and the acknowledged one keep their clearance; without presence they
    // end up orphaned, which implies a delivered clearance.
    std::thread::sleep(Duration::from_secs(2));
    for (o, id) in [(&old, ids[0]), (&with, ids[2])] {
        let state = o.ticket_state(id);
        assert!(matches!(state.as_deref(), Some("active" | "orphaned")), "{state:?}");
        assert!(!o.log().contains(&format!("RECALLED #{id}")), "{}", o.log());
    }
}

#[test]
fn manual_lease_then_extension() {
    let o = Orchestra::start("bail");
    let d = o.out(&["request", "--side", "windows", "--ram", "1G", "--duration", "2s", "court"]);
    let id = text(&d.stdout).trim().to_owned();
    let n: u64 = id.parse().unwrap();

    // Lease overrun: still active and reserved, the queue goes on.
    assert!(wait(Duration::from_secs(8), || o.log().contains(&format!("LEASE OVERRUN #{id}"))), "{}", o.log());
    let t = o.ticket(n).unwrap();
    assert_eq!((t["state"].as_str(), &t["lease_overrun"]), (Some("active"), &json!(true)), "{t}");
    assert!(t["message"].as_str().unwrap().contains(&format!("clearway extend {id}")), "{t}");
    let (_, during) = o.create("pendant", "windows", 1024);
    assert_eq!(during["state"], "active", "{during}");
    o.call("POST", &format!("/tickets/{}/release", during["id"]), json!({}));
    let s = text(&o.out(&["status"]).stdout);
    assert!(s.contains("(bail dépassé)"), "{s}");

    // No news for delay.absence (8 s) after the deadline: orphaned, admissions frozen.
    assert!(wait(Duration::from_secs(14), || o.ticket_state(n).as_deref() == Some("orphaned")));
    assert!(o.log().contains(&format!("LEASE EXPIRED #{id}")), "{}", o.log());
    let (_, b) = o.create("suivant", "windows", 1024);
    assert_eq!(b["reason"]["decision"], "frozen");
    assert!(b["reason"]["message"].as_str().unwrap().contains("bail échu"), "{b}");

    let p = o.out(&["extend", &id, "10m", "tests plus longs que prévu"]);
    assert_eq!(p.status.code(), Some(0), "{}", text(&p.stderr));
    assert!(text(&p.stdout).contains("prolongé de 10 min"), "{}", text(&p.stdout));
    assert_eq!(o.ticket_state(id.parse().unwrap()).as_deref(), Some("active"));
    assert!(o.log().contains("tests plus longs que prévu"));
    let (_, big) = o.create("gros", "windows", 15 * 1024);
    assert_eq!(big["state"], "waiting");
    let r = o.out(&["extend", &big["id"].to_string(), "5m", "pas encore actif"]);
    assert_eq!(r.status.code(), Some(2));
    assert!(text(&r.stderr).contains("rien à prolonger"), "{}", text(&r.stderr));
}

#[test]
fn manual_freeze() {
    let o = Orchestra::start("gel");
    let (code, g) = o.call("POST", "/freeze", json!({ "frozen": true }));
    assert_eq!(code, 200, "{g}");
    let (_, a) = o.create("a", "windows", 1024);
    assert_eq!(a["reason"]["decision"], "frozen");
    assert!(a["reason"]["message"].as_str().unwrap().contains("gel manuel"), "{a}");
    assert!(text(&o.out(&["status"]).stdout).contains("File gelée à la main"));
    o.call("POST", "/freeze", json!({ "frozen": false }));
    assert_eq!(o.ticket_state(1).as_deref(), Some("active"));
    assert!(o.log().contains("MANUAL FREEZE") && o.log().contains("MANUAL UNFREEZE"));
}

#[test]
fn orphan_freezes_then_comes_back_on_signal() {
    let o = Orchestra::start("orphelin");
    let (_, a) = o.create("a", "windows", 1024);
    assert_eq!(a["state"], "active");
    assert!(wait(Duration::from_secs(8), || o.ticket_state(1).as_deref() == Some("orphaned")));
    let (_, b) = o.create("b", "windows", 1024);
    assert_eq!(b["state"], "waiting");
    assert_eq!(b["reason"]["decision"], "frozen");
    assert!(b["reason"]["message"].as_str().unwrap().contains("ticket #1 orphelin"), "{b}");

    let (code, p) = o.call("POST", "/tickets/1/presence", json!({}));
    assert_eq!(code, 200);
    assert_eq!(p["state"], "active");
    assert_eq!(o.ticket_state(2).as_deref(), Some("active"));
    assert!(o.log().contains("ORPHANED #1") && o.log().contains("BACK #1"));
}

#[test]
fn supervisor_killed_during_task() {
    let o = Orchestra::start("superviseur");
    let mut run = o
        .cli(&["run", "--side", "windows", "--ram", "1G", "long", "--", "sleep", "30"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait(Duration::from_secs(5), || o.ticket_state(1).as_deref() == Some("active")));
    unsafe { libc::kill(run.id() as i32, libc::SIGKILL) };
    let _ = run.wait();
    assert!(wait(Duration::from_secs(8), || o.ticket_state(1).as_deref() == Some("orphaned")));
    let _ = Command::new("pkill").args(["-f", "^sleep 30$"]).status();
    assert_eq!(o.out(&["cancel", "1"]).status.code(), Some(0));
}

#[test]
fn signal_to_supervisor_stops_command_and_reports_release() {
    let o = Orchestra::start("signal");
    let mut run = o
        .cli(&["run", "--side", "windows", "--ram", "1G", "long", "--", "sleep", "40"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait(Duration::from_secs(5), || o.ticket_state(1).as_deref() == Some("active")));
    unsafe { libc::kill(run.id() as i32, libc::SIGTERM) };
    let status_view = run.wait().unwrap();
    assert_eq!(status_view.code(), Some(128 + libc::SIGTERM));
    assert!(o.state()["tickets"].as_array().unwrap().is_empty());
    assert!(o.log().contains("RELEASED #1"), "{}", o.log());
}

#[test]
fn unattended_wait_skipped_then_removed() {
    let o = Orchestra::start("expiration");
    o.create("gros", "windows", 15 * 1024);
    let (_, b) = o.create("petit", "windows", 4 * 1024);
    assert_eq!(b["state"], "waiting");
    assert!(wait(Duration::from_secs(8), || {
        o.ticket(2).is_some_and(|t| t["message"].as_str().unwrap().contains("aucun client ne l'attend"))
    }));
    assert!(wait(Duration::from_secs(10), || o.ticket_state(2).is_none()));
    assert!(o.log().contains("EXPIRED #2"));
}

#[test]
fn resume_after_hard_stop_and_epoch() {
    let mut o = Orchestra::start("reprise");
    let (_, a) = o.create("a", "windows", 12 * 1024);
    assert_eq!(a["epoch"], 1);
    let run = o
        .cli(&["run", "--side", "windows", "--ram", "8G", "après reprise", "--", "true"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(wait(Duration::from_secs(5), || o.ticket_state(2).as_deref() == Some("waiting")));
    o.kill();
    let on_disk: Value = serde_json::from_slice(&std::fs::read(o.dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(on_disk["tickets"].as_array().unwrap().len(), 2);
    o.launch();
    assert_eq!(o.state()["epoch"], 2);
    assert_eq!(o.ticket_state(1).as_deref(), Some("active"));
    // The epoch 1 client opens a new relay and gets its clearance from the new instance.
    o.call("POST", "/tickets/1/release", json!({"code": 0}));
    let out = run.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(o.log().contains("START époque 2"), "{}", o.log());
}

#[test]
fn second_instance_and_stop_through_the_pipe() {
    let mut o = Orchestra::start("instances");
    let status_view = o.process().status().unwrap();
    assert_eq!(status_view.code(), Some(3));
    assert!(o.log().contains("INFO Déjà lancé"), "{}", o.log());
    assert!(!o.log().contains("ERROR"));
    let (code, _) = o.call("POST", "/quit", Value::Null);
    assert_eq!(code, 200);
    let mut e = o.child.take().unwrap();
    assert!(wait(Duration::from_secs(5), || matches!(e.try_wait(), Ok(Some(s)) if s.success())));
    assert!(o.log().contains("STOP"));
    assert!(!o.answers());
}

#[test]
fn creation_idempotence() {
    let o = Orchestra::start("idempotence");
    let (c1, a) = o.create("meme-requete", "windows", 1024);
    let (c2, b) = o.create("meme-requete", "windows", 1024);
    assert_eq!((c1, c2), (201, 200));
    assert_eq!(a["id"], b["id"]);
    let (_, c) = o.create("autre-requete", "windows", 1024);
    assert_ne!(a["id"], c["id"]);
}

#[test]
fn wsl_stop_frees_wsl_tickets() {
    let o = Orchestra::start("wsl-arrete");
    let (_, a) = o.create("wsl", "wsl", 1024);
    assert_eq!(a["state"], "active");
    let (_, b) = o.create("win", "windows", 1024);
    assert_eq!(b["state"], "active");
    std::fs::write(
        o.dir.join("measures-test.json"),
        r#"{"ram_total_mb":32768,"ram_avail_mb":20480,"commit_mb":0,"commit_limit_mb":0,"cpu_pct":0.0,"vmmem":null}"#,
    )
    .unwrap();
    assert!(wait(Duration::from_secs(6), || o.ticket_state(1).is_none()));
    assert!(o.ticket_state(2).is_some());
    assert!(o.log().contains("FREED #1"));
    let s = text(&o.out(&["status"]).stdout);
    assert!(s.contains("WSL : arrêté"), "{s}");
}

#[test]
fn relay_absent_then_automatic_launch() {
    let o = Orchestra::prepare("lancement");
    let r = Command::new(env!("CARGO_BIN_EXE_clearway")).args(["relay", "--pipe", &o.pipe]).env("CLEARWAY_LANG", "en").output().unwrap();
    assert_eq!(r.status.code(), Some(RELAY_ABSENT));
    assert!(text(&r.stderr).contains("relay: Clearway is not running"), "{}", text(&r.stderr));

    let s = o.out(&["status"]);
    assert_eq!(s.status.code(), Some(0), "{}", text(&s.stderr));
    assert!(text(&s.stderr).contains("Clearway absent, lancement"), "{}", text(&s.stderr));
    assert!(text(&s.stdout).contains("File vide"));
    let (code, _) = o.call("POST", "/quit", Value::Null);
    assert_eq!(code, 200);
    assert!(wait(Duration::from_secs(5), || !o.answers()));
}

#[test]
fn clearway_unreachable() {
    let o = Orchestra::prepare("injoignable");
    let began = Instant::now();
    let r = o.cli(&["status"]).env("CLEARWAY_EXE", "/bin/false").env("CLEARWAY_PATIENCE", "6").output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    assert!(began.elapsed() >= Duration::from_secs(3));
    let e = text(&r.stderr);
    assert!(e.contains("nouvel essai dans 1 s") && e.contains("nouvel essai dans 2 s"), "délai croissant : {e}");
    assert!(e.contains("Clearway injoignable depuis"), "{e}");
}

/// Mimics `clearway.exe` when WSL interop stalls: the first `failures` launches fail with the real interop error.
fn fake_exe(o: &Orchestra, failures: u32) -> PathBuf {
    let path = o.dir.join("faux-exe.sh");
    let counter = o.dir.join("lancements");
    let _ = std::fs::remove_file(&counter);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nn=$(cat '{c}' 2>/dev/null || echo 0)\necho $((n + 1)) > '{c}'\nif [ \"$n\" -lt {failures} ]; then\n  echo 'WSL ERROR: UtilAcceptVsock:281: accept4 failed 110' >&2\n  exit 1\nfi\nexec '{real}' \"$@\"\n",
            c = counter.display(),
            real = env!("CARGO_BIN_EXE_clearway")
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&path).status().unwrap();
    path
}

#[test]
fn interop_stalls_then_comes_back() {
    let o = Orchestra::start("interop");
    let exe = fake_exe(&o, 2);
    let r = o.cli(&["status"]).env("CLEARWAY_EXE", &exe).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    let e = text(&r.stderr);
    assert!(e.contains("interop WSL indisponible (WSL ERROR: UtilAcceptVsock"), "{e}");
    assert!(e.contains("nouvel essai dans 1 s") && e.contains("nouvel essai dans 2 s"), "{e}");

    // While waiting, the outage does not drop the ticket: the client reattaches and gets its clearance.
    o.create("occupant", "windows", 12 * 1024);
    let d = o.out(&["request", "--side", "windows", "--ram", "8G", "patient"]);
    let id = text(&d.stdout).trim().to_owned();
    let exe = fake_exe(&o, 0);
    let waiting = o.cli(&["wait", &id, "--max", "1m"]).env("CLEARWAY_EXE", &exe).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    // Interop stalls while the client waits: its relay dies, the next 2 launches fail.
    std::fs::write(o.dir.join("lancements"), "-2").unwrap();
    let _ = Command::new("pkill").args(["-f", &format!("relay --pipe {}", o.pipe)]).status();
    std::thread::sleep(Duration::from_secs(1));
    o.call("POST", "/tickets/1/release", json!({}));
    let out = waiting.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}\n{}", text(&out.stdout), text(&out.stderr));
    assert!(text(&out.stdout).contains("FEU VERT"), "{}", text(&out.stdout));
    assert!(text(&out.stderr).contains("interop WSL indisponible"), "{}", text(&out.stderr));
}

#[test]
fn ticket_dir_and_branch() {
    let o = Orchestra::start("lieu");
    let repo = o.dir.join("depot-essai");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |inside: &std::path::Path, args: &[&str]| {
        let s = Command::new("git").current_dir(inside).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
        assert!(s.status.success(), "git {args:?} : {}", text(&s.stderr));
    };
    git(&repo, &["init", "-q", "-b", "principale"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "départ"]);
    git(&repo, &["worktree", "add", "-q", "-b", "feat/essai", "../arbre-essai"]);
    std::fs::create_dir_all(repo.join("sous/dossier")).unwrap();

    let d = o.cli(&["request", "--side", "windows", "--ram", "1G", "dans le dépôt"]).current_dir(repo.join("sous/dossier")).output().unwrap();
    assert_eq!(d.status.code(), Some(0), "{}", text(&d.stderr));
    let d = o.cli(&["request", "--side", "windows", "--ram", "1G", "dans le worktree"]).current_dir(o.dir.join("arbre-essai")).output().unwrap();
    assert_eq!(d.status.code(), Some(0), "{}", text(&d.stderr));
    let d = o.cli(&["request", "--side", "windows", "--ram", "1G", "hors git"]).current_dir("/").env_remove("CLEARWAY_SESSION").output().unwrap();
    assert_eq!(d.status.code(), Some(0), "{}", text(&d.stderr));

    let s = text(&o.out(&["status"]).stdout);
    assert!(s.contains("[test] depot-essai · principale : dans le dépôt"), "{s}");
    assert!(s.contains("[test] arbre-essai · arbre-essai (branche feat/essai) : dans le worktree"), "{s}");
    assert!(s.contains("[—] / : hors git"), "{s}");
}

#[test]
fn maintenance_in_progress_no_launch() {
    let o = Orchestra::prepare("maintenance");
    let file = o.dir.join("maintenance");
    std::fs::write(&file, "").unwrap();
    let r = o.cli(&["status"]).env("CLEARWAY_PATIENCE", "4").output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let e = text(&r.stderr);
    assert!(e.contains("Clearway en cours de mise à jour, nouvel essai dans 1 s"), "{e}");
    assert!(!e.contains("lancement"), "pas de lancement pendant la maintenance : {e}");
    assert!(!o.answers());

    // A file older than 5 min is ignored: the client launches Clearway.
    let ok = Command::new("touch").args(["-d", "-6 minutes"]).arg(&file).status().unwrap();
    assert!(ok.success());
    let r = o.out(&["status"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("Clearway absent, lancement"));
    let (code, _) = o.call("POST", "/quit", Value::Null);
    assert_eq!(code, 200);
    assert!(wait(Duration::from_secs(5), || !o.answers()));
}

#[test]
fn claude_or_codex_session_name() {
    let o = Orchestra::start("session");
    let home = o.dir.join("maison");
    std::fs::create_dir_all(home.join(".claude/sessions")).unwrap();
    std::fs::write(home.join(".claude/sessions/4242.json"), r#"{"pid":4242,"name":"RDEV APP","kind":"interactive"}"#).unwrap();
    std::fs::write(home.join(".claude/sessions/4343.json"), "pas du json").unwrap();
    let request_cmd = |pid: &str, codex: Option<&str>, what: &str| {
        let mut c = o.cli(&["request", "--side", "windows", "--ram", "1G", what]);
        c.env_remove("CLEARWAY_SESSION").env("HOME", &home).env("CLAUDE_PID", pid).env_remove("CODEX_THREAD_ID");
        if let Some(id) = codex {
            c.env("CODEX_THREAD_ID", id).env_remove("CLAUDE_PID");
        }
        let s = c.output().unwrap();
        assert_eq!(s.status.code(), Some(0), "{}", text(&s.stderr));
    };
    request_cmd("4242", None, "nom lu");
    request_cmd("4343", None, "fichier illisible");
    request_cmd("9999", None, "fichier absent");
    request_cmd("", Some("019a2b3c-4d5e-6f70"), "codex");
    let s = text(&o.out(&["status"]).stdout);
    assert!(s.contains("[RDEV APP]") && s.contains(": nom lu"), "{s}");
    assert!(s.lines().any(|l| l.contains("[—]") && l.contains("fichier illisible")), "{s}");
    assert!(s.lines().any(|l| l.contains("[—]") && l.contains("fichier absent")), "{s}");
    assert!(s.contains("[Codex 019a2b3c]"), "{s}");
}

#[test]
fn multiplexing_by_id_and_legacy_mode() {
    let o = Orchestra::start("multiplexage");
    o.create("occupant", "windows", 12 * 1024);
    let (_, t) = o.create("patient", "windows", 8 * 1024);
    let id = t["id"].as_u64().unwrap();
    let mut l = Line::open(&o);
    let began = Instant::now();
    let params = json!({ "known": key(&t["reason"]), "timeout_s": "2" });
    l.send(json!({ "id": 2, "method": "GET", "path": format!("/tickets/{id}/wait"), "params": params }));
    l.send(json!({ "id": 3, "method": "GET", "path": "/status" }));
    let a = l.read();
    assert_eq!(a["id"], 3, "l'état répond pendant l'attente : {a}");
    assert!(began.elapsed() < Duration::from_millis(1500));
    let b = l.read();
    assert_eq!((b["id"].as_u64(), b["code"].as_u64()), (Some(2), Some(200)), "{b}");
    assert!(began.elapsed() >= Duration::from_millis(1500), "l'attente a bien duré");
    // Without id: sequential, response without id.
    let c = l.call("GET", "/status", Value::Null);
    assert!(c.get("id").is_none() && c["code"] == 200, "{c}");
}

/// Stable key of a reason, as `Reason::key` computes it.
fn key(reason: &Value) -> String {
    let r: clearway::model::Reason = serde_json::from_value(reason.clone()).unwrap();
    r.key()
}

#[test]
fn bidirectional_relay() {
    let o = Orchestra::start("relais-bidir");
    o.create("occupant", "windows", 12 * 1024);
    let (_, t) = o.create("patient", "windows", 8 * 1024);
    let id = t["id"].as_u64().unwrap();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_clearway"))
        .args(["relay", "--pipe", &o.pipe])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = relay.stdin.take().unwrap();
    let mut out = BufReader::new(relay.stdout.take().unwrap());
    let known = key(&t["reason"]);
    let waiting = json!({ "id": 1, "method": "GET", "path": format!("/tickets/{id}/wait"), "params": { "known": known, "timeout_s": "2" } });
    writeln!(input, "{waiting}").unwrap();
    writeln!(input, "{}", json!({ "id": 2, "method": "GET", "path": "/status" })).unwrap();
    let mut read = || {
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    assert_eq!(read()["id"], 2, "la réponse rapide passe devant l'attente en cours");
    assert_eq!(read()["id"], 1);
    // End of stdin with an `id` request in flight: exit without waiting (a multiplexed client leaving).
    writeln!(input, "{waiting}").unwrap();
    drop(input);
    let began = Instant::now();
    assert!(wait(Duration::from_secs(1), || matches!(relay.try_wait(), Ok(Some(_)))), "fin de stdin : le relais sort");
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(relay.wait().unwrap().code(), Some(0));

    // `echo request | relay`: the response to a request without `id` is delivered before exiting.
    let out = Command::new(env!("CARGO_BIN_EXE_clearway"))
        .args(["relay", "--pipe", &o.pipe])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut r| {
            let waiting = json!({ "method": "GET", "path": format!("/tickets/{id}/wait"), "params": { "known": key(&t["reason"]), "timeout_s": "1" } });
            writeln!(r.stdin.take().unwrap(), "{waiting}\n{}", json!({ "method": "GET", "path": "/status" }))?;
            r.wait_with_output()
        })
        .unwrap();
    let lines: Vec<Value> = text(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn lost_link_then_resumed_by_the_same_session() {
    let o = Orchestra::start("liaison");
    let mut a = Line::open(&o);
    let link = a.open_link("session-A");
    assert_eq!((link["code"].as_u64(), link["body"]["link"].as_u64()), (Some(200), Some(1)), "{link}");
    assert_eq!(a.open_link("session-A")["code"], 409, "une seule liaison par connexion");
    assert_eq!(a.call("POST", "/link/heartbeat", json!({}))["code"], 200);
    let active = a.create("lie-actif", 4 * 1024);
    let waiting = a.create("lie-attente", 14 * 1024);
    assert_eq!((active["body"]["state"].as_str(), waiting["body"]["state"].as_str()), (Some("active"), Some("waiting")));
    let (n1, n2) = (active["body"]["id"].as_u64().unwrap(), waiting["body"]["id"].as_u64().unwrap());

    drop(a);
    assert!(wait(Duration::from_secs(3), || o.ticket_state(n1).as_deref() == Some("orphaned")), "{}", o.log());
    assert!(o.log().contains("LINK #1 fermée"), "{}", o.log());
    let message = o.ticket(n1).unwrap()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("liaison de sa session perdue"), "{message}");
    // No freeze: another client goes through.
    let (_, other) = o.create("autre", "windows", 1024);
    assert_eq!(other["state"], "active", "{other}");
    o.call("POST", &format!("/tickets/{}/release", other["id"]), json!({}));

    // Another session resumes nothing; the same one does.
    let mut b = Line::open(&o);
    assert_eq!(b.open_link("session-B")["body"]["resumed"], json!([]));
    let mut c = Line::open(&o);
    assert_eq!(c.open_link("session-A")["body"]["resumed"], json!([n1, n2]));
    assert_eq!(o.ticket_state(n1).as_deref(), Some("active"));
    assert!(o.log().contains(&format!("reprise de #{n1}, #{n2}")), "{}", o.log());

    // Without resume within delay.resume (3 s): removed.
    drop(c);
    assert!(wait(Duration::from_secs(8), || o.ticket(n1).is_none() && o.ticket(n2).is_none()), "{}", o.log());
    assert!(o.log().contains(&format!("Ticket #{n1} retiré : liaison de sa session perdue")), "{}", o.log());
    drop(b);
}

#[test]
fn link_resume_after_restart() {
    let mut o = Orchestra::start("liaison-redemarrage");
    let mut a = Line::open(&o);
    a.open_link("session-R");
    let n = a.create("survivant", 2048)["body"]["id"].as_u64().unwrap();
    o.kill();
    drop(a);
    o.launch();
    assert_eq!(o.ticket_state(n).as_deref(), Some("orphaned"));
    let mut b = Line::open(&o);
    assert_eq!(b.open_link("session-R")["body"]["resumed"], json!([n]));
    assert_eq!(o.ticket_state(n).as_deref(), Some("active"));
    assert_eq!(Line::open(&o).call("POST", "/link/heartbeat", json!({}))["code"], 409, "battement sans liaison");
}

#[test]
fn pre_clearway_clients_keep_working() {
    let o = Orchestra::start("ancien-client");
    // Raw French dialect of the old `charge`: French keys and routes in, French keys and values out.
    let mut l = Line::open(&o);
    l.send(json!({ "methode": "POST", "chemin": "/tickets", "corps": {
        "requete_id": "ancien-1", "cote": "windows", "ram_mo": 1024, "cpu": 1, "duree_s": 600, "manuel": true,
        "description": "ancien client", "session": "ancienne", "claude_session_id": null, "claude_pid": null,
        "dossier": "/", "signature": "", "mesures": null } }));
    let created = l.read();
    assert_eq!(created["code"], 201, "{created}");
    assert_eq!((created["corps"]["etat"].as_str(), created["corps"]["raison"]["decision"].as_str()), (Some("actif"), Some("feu_vert")), "{created}");
    assert!(created.get("body").is_none() && created["corps"]["raison"]["phrase"].is_string(), "{created}");
    l.send(json!({ "methode": "GET", "chemin": "/etat", "parametres": { "mem_dispo_mo": "4000", "mem_totale_mo": "11960", "charge1": "1" } }));
    let status = l.read();
    assert_eq!(status["corps"]["tickets"][0]["etat"], "actif", "{status}");
    assert!(status["corps"]["mesures"]["wsl"]["mem_dispo_mo"].is_u64(), "{status}");
    l.send(json!({ "methode": "POST", "chemin": "/tickets/1/fin", "corps": {} }));
    assert_eq!(l.read()["corps"]["etat"], "termine");

    // The old command name, through a link called `charge`: old subcommands, old variables, French texts.
    let dir = o.dir.join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let charge = dir.join("charge");
    let _ = std::fs::remove_file(&charge);
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_clearway"), &charge).unwrap();
    let old = |args: &[&str]| {
        Command::new(&charge)
            .args(args)
            .env("CHARGE_EXE", env!("CARGO_BIN_EXE_clearway"))
            .env("CHARGE_TUBE", &o.pipe)
            .env("CHARGE_DOSSIER", &o.dir)
            .env("CHARGE_ARGS_LANCEMENT", "--test")
            .env("CHARGE_SESSION", "ancienne")
            .env("LC_ALL", "C.UTF-8")
            .env_remove("CLEARWAY_LANG")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_PID")
            .output()
            .unwrap()
    };
    let d = old(&["demande", "--cote", "windows", "--ram", "1G", "tâche"]);
    let id = text(&d.stdout).trim().to_owned();
    assert!(text(&d.stderr).contains(&format!("Lance la tâche lourde, puis « charge fin {id} ».")), "{}", text(&d.stderr));
    let a = old(&["attend", &id, "--max", "5s"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert!(text(&a.stdout).contains(&format!("FEU VERT pour le ticket #{id} : lance ta tâche lourde maintenant, puis « charge fin {id} ».")), "{}", text(&a.stdout));
    let s = old(&["statut"]);
    assert!(text(&s.stdout).contains("actif"), "{}", text(&s.stdout));
    assert_eq!(old(&["fin", &id]).status.code(), Some(0));
}

#[test]
fn french_state_and_config_are_migrated() {
    let mut o = Orchestra::prepare("migration");
    std::fs::remove_file(o.dir.join("config.toml")).unwrap();
    std::fs::write(
        o.dir.join("config.toml"),
        format!("tube = \"{}\"\nmarge.ram_windows = \"4G\"\ndelai.actif = \"3s\"\ndelai.long_poll = \"2s\"\n", o.pipe),
    )
    .unwrap();
    std::fs::write(
        o.dir.join("etat.json"),
        r#"{"epoque": 10, "compteur": 44, "vmmem_pid": 4242, "gel_manuel": null, "tickets": [{
            "id": 44, "requete_id": "r44", "cote": "wsl", "besoin": {"ram_mo": 4096, "cpu": 4, "duree_s": 3600},
            "description": "GSTM", "session": "GSTM", "claude_session_id": null, "claude_pid": null, "dossier": "/",
            "signature": "", "etat": "actif", "cree": 1794000000, "accorde": 1794000000, "dispo_accord_mo": 6000,
            "manuel": true, "recu": true, "rappel": null, "cause_orphelin": null, "rattachement": "",
            "projet": "gstm", "branche": "main", "force": false, "doublages": 0}]}"#,
    )
    .unwrap();
    o.launch();
    let t = o.ticket(44).expect("ticket 44 taken over");
    assert_eq!((t["state"].as_str(), t["project"].as_str(), t["manual"].as_bool()), (Some("active"), Some("gstm"), Some(true)), "{t}");
    assert_eq!(o.state()["epoch"], 11);
    assert!(o.dir.join("state.json").exists() && o.dir.join("etat.json.before-clearway").exists());
    assert!(!o.dir.join("etat.json").exists());
    let config = std::fs::read_to_string(o.dir.join("config.toml")).unwrap();
    assert!(config.contains(&format!("pipe = \"{}\"", o.pipe)) && config.contains("delay.silence = \"3s\""), "{config}");
    assert!(o.dir.join("config.toml.before-clearway").exists());
    assert!(o.log().contains("etat.json"), "{}", o.log());
}

#[test]
fn english_messages() {
    let mut o = Orchestra::prepare("anglais");
    let config = std::fs::read_to_string(o.dir.join("config.toml")).unwrap().replace("language = \"fr\"", "language = \"en\"");
    std::fs::write(o.dir.join("config.toml"), config).unwrap();
    o.launch();
    let d = o.cli(&["request", "--side", "windows", "--ram", "10G", "first"]).env("LC_ALL", "C.UTF-8").output().unwrap();
    let id = text(&d.stdout).trim().to_owned();
    assert!(text(&d.stderr).contains(&format!("clearway release {id}")), "{}", text(&d.stderr));
    let (_, w) = o.create("second", "windows", 8 * 1024);
    let message = w["reason"]["message"].as_str().unwrap();
    assert!(message.contains("waiting, position 1") && message.contains("Windows RAM"), "{message}");
    assert!(o.log().contains("REQUEST #2"), "{}", o.log());
}

/// A real `clearway mcp` process for one Claude session, through the relay as under WSL.
struct Mcp {
    child: Child,
    lines: std::sync::mpsc::Receiver<Value>,
}

impl Mcp {
    fn start(o: &Orchestra, session_id: &str) -> Mcp {
        let mut child = o
            .cli(&["mcp"])
            .env("CLAUDE_CODE_SESSION_ID", session_id)
            .env_remove("CLEARWAY_SESSION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in out.lines() {
                let Ok(line) = line else { break };
                let _ = tx.send(serde_json::from_str(&line).expect("stdout carries JSON-RPC only"));
            }
        });
        let mut m = Mcp { child, lines };
        m.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }));
        assert!(m.response(1)["result"]["instructions"].as_str().unwrap().contains("request_clearance"));
        m
    }

    fn send(&mut self, v: Value) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{v}").unwrap();
    }

    fn response(&self, id: u64) -> Value {
        loop {
            let v = self.lines.recv_timeout(Duration::from_secs(20)).expect("MCP response");
            if v["id"] == id {
                return v;
            }
        }
    }

    fn tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments, "_meta": { "progressToken": id } } }));
        self.response(id)["result"].clone()
    }

    /// End of stdin, as when the session closes.
    fn close(mut self) {
        drop(self.child.stdin.take());
        assert!(wait(Duration::from_secs(5), || matches!(self.child.try_wait(), Ok(Some(_)))), "clearway mcp exits");
    }
}

#[test]
fn mcp_process_links_its_session_and_resumes_it() {
    let mut o = Orchestra::start("mcp-reel");
    let mut a = Mcp::start(&o, "sess-int-1");
    assert!(wait(Duration::from_secs(5), || o.log().contains("LINK #1 ouverte")), "{}", o.log());
    let (_, status) = o.call("GET", "/status", Value::Null);
    let links = status["links"].as_array().expect("links in /status");
    assert_eq!(links.len(), 1, "{status}");
    assert!(links[0]["client"].as_str().is_some_and(|c| c.contains(" mcp ")), "{status}");

    let r = a.tool(2, "request_clearance", json!({ "description": "essai MCP", "ram": "1G", "duration": "10m" }));
    assert_eq!(r["structuredContent"]["cleared"], true, "{r}");
    let id = r["structuredContent"]["ticket"].as_u64().unwrap();
    assert!(wait(Duration::from_secs(3), || o.log().contains(&format!("ACK #{id}"))), "{}", o.log());
    let t = o.ticket(id).unwrap();
    assert_eq!(t["session"], "Claude sess-int", "{t}");

    // Session closed: the ticket waits for its session; a new MCP process of the same session resumes it.
    a.close();
    assert!(wait(Duration::from_secs(3), || o.log().contains("LINK #1 fermée")), "{}", o.log());
    let mut b = Mcp::start(&o, "sess-int-1");
    assert!(wait(Duration::from_secs(5), || o.log().contains(&format!("reprise de #{id}"))), "{}", o.log());
    assert_eq!(o.ticket_state(id).as_deref(), Some("active"));

    // Clearway restarts: the MCP process links again by itself and keeps the ticket.
    o.kill();
    o.launch();
    let resumed = |log: String| log.matches(&format!("reprise de #{id}")).count() >= 2;
    assert!(wait(Duration::from_secs(30), || resumed(o.log())), "{}", o.log());
    let r = b.tool(3, "release", json!({ "ticket": id, "code": 0 }));
    assert_eq!(r["isError"], false, "{r}");
    assert!(o.ticket(id).is_none());
    b.close();
}

fn hook(o: &Orchestra, session_id: &str, command: &str) -> (Option<i32>, String) {
    let mut child = o.cli(&["hook"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let event = json!({ "session_id": session_id, "hook_event_name": "PreToolUse", "tool_name": "Bash",
        "tool_input": { "command": command } });
    writeln!(child.stdin.take().unwrap(), "{event}").unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code(), text(&out.stdout))
}

#[test]
fn hook_refuses_heavy_commands_until_the_session_is_cleared() {
    let mut o = Orchestra::start("hook");
    let (code, out) = hook(&o, "sess-hook", "cargo build --release");
    assert_eq!(code, Some(0));
    let refusal: Value = serde_json::from_str(out.trim()).expect("JSON refusal");
    assert_eq!(refusal["hookSpecificOutput"]["permissionDecision"], "deny", "{out}");
    assert!(out.contains("request_clearance"), "{out}");
    assert_eq!(hook(&o, "sess-hook", "git status && ls"), (Some(0), String::new()), "light command, no output");

    let mut m = Mcp::start(&o, "sess-hook");
    let r = m.tool(2, "request_clearance", json!({ "description": "build", "ram": "1G", "duration": "10m" }));
    let id = r["structuredContent"]["ticket"].as_u64().unwrap();
    assert_eq!(r["structuredContent"]["cleared"], true, "{r}");
    assert_eq!(hook(&o, "sess-hook", "cargo build --release"), (Some(0), String::new()), "cleared session");
    assert!(hook(&o, "other-session", "cargo build").1.contains("deny"), "another session is still refused");
    m.tool(3, "release", json!({ "ticket": id, "code": 0 }));
    m.close();

    o.kill();
    assert_eq!(hook(&o, "sess-hook", "cargo build"), (Some(0), String::new()), "Clearway absent: allowed");
}
