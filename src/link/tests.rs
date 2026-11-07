use std::io::{BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use serde_json::{Value, json};

use super::*;

/// Fake Clearway on in-memory pipes: answers `/link` (resuming whatever the test sets), heartbeats and `/status`;
/// `/slow` answers after `/status`, out of order. Each connection can be cut by the test.
struct Fake {
    connections: AtomicUsize,
    requests: Mutex<Vec<(usize, Request)>>,
    writers: Mutex<Vec<Option<PipeWriter>>>,
    refuse: Mutex<bool>,
    ended: AtomicUsize,
}

impl Fake {
    fn new() -> Arc<Fake> {
        Arc::new(Fake {
            connections: AtomicUsize::new(0),
            requests: Mutex::default(),
            writers: Mutex::default(),
            refuse: Mutex::new(false),
            ended: AtomicUsize::new(0),
        })
    }

    fn paths(&self) -> Vec<(usize, String)> {
        self.requests.lock().unwrap().iter().map(|(c, r)| (*c, r.path.clone())).collect()
    }

    /// The server side of connection `n` closes.
    fn cut(&self, n: usize) {
        self.writers.lock().unwrap()[n] = None;
    }
}

fn answer(fake: &Fake, n: usize, id: Option<u64>, code: u16, body: Value) {
    if let Some(w) = fake.writers.lock().unwrap()[n].as_mut() {
        let _ = writeln!(w, "{}", serde_json::to_string(&Response { id, code, body }).unwrap());
    }
}

struct Pipes(Arc<Fake>);

impl Connector for Pipes {
    type Guard = usize;

    fn connect(&self) -> Result<Connection<usize>, String> {
        let fake = self.0.clone();
        let n = fake.connections.fetch_add(1, Ordering::SeqCst);
        let (to_server_r, to_server_w): (PipeReader, PipeWriter) = std::io::pipe().unwrap();
        let (to_client_r, to_client_w) = std::io::pipe().unwrap();
        fake.writers.lock().unwrap().push(Some(to_client_w));
        std::thread::spawn(move || {
            let mut slow: Option<Option<u64>> = None;
            for line in BufReader::new(to_server_r).lines() {
                let Ok(line) = line else { break };
                let r: Request = serde_json::from_str(&line).unwrap();
                fake.requests.lock().unwrap().push((n, r.clone()));
                match r.path.as_str() {
                    "/link" if *fake.refuse.lock().unwrap() => answer(&fake, n, r.id, 503, json!("busy")),
                    "/link" => answer(&fake, n, r.id, 200, json!({ "link": n + 1, "resumed": [] })),
                    "/slow" => slow = Some(r.id),
                    _ => {
                        answer(&fake, n, r.id, 200, json!({ "path": r.path, "connection": n }));
                        if let Some(id) = slow.take() {
                            answer(&fake, n, id, 200, json!({ "path": "/slow" }));
                        }
                    }
                }
            }
        });
        Ok(Connection { reader: Box::new(to_client_r), writer: Box::new(to_server_w), guard: n })
    }

    fn ended(&self, _guard: usize) {
        self.0.ended.fetch_add(1, Ordering::SeqCst);
    }

    fn measures(&self) -> Option<WslMeasures> {
        Some(WslMeasures {
            mem_avail_mb: 4096,
            mem_total_mb: 8192,
            load1: 1.5,
            swap_total_mb: 0,
            swap_free_mb: 0,
            pswpin: 0,
            pswpout: 0,
            at: 0,
        })
    }
}

fn until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

fn link() -> Link {
    Link { identity: "claude:abc".into(), session: "essai".into(), client: "test".into(), measures: None }
}

#[test]
fn links_first_then_multiplexes_out_of_order() {
    let fake = Fake::new();
    let channel = Arc::new(Linked::start(Pipes(fake.clone()), link(), |_| {}));
    assert!(until(Duration::from_secs(2), || channel.is_linked()), "linked at start, before any call");
    let first = fake.requests.lock().unwrap()[0].1.clone();
    assert_eq!(first.path, "/link");
    assert_eq!(first.body["identity"], "claude:abc");
    assert_eq!(first.body["measures"]["load1"], 1.5, "the link carries the WSL measures");

    let slow = {
        let c = channel.clone();
        std::thread::spawn(move || c.call(Request::new("GET", "/slow", Value::Null), Duration::from_secs(5)))
    };
    assert!(until(Duration::from_secs(2), || fake.paths().iter().any(|(_, p)| p == "/slow")));
    let quick = channel.call(Request::new("GET", "/status", Value::Null), Duration::from_secs(5)).unwrap();
    assert_eq!(quick.body["path"], "/status");
    assert_eq!(slow.join().unwrap().unwrap().body["path"], "/slow", "each response reaches its own caller");
    assert_eq!(fake.connections.load(Ordering::SeqCst), 1, "one connection for everything");
    channel.close();
}

#[test]
fn connection_lost_fails_the_calls_then_relinks() {
    let fake = Fake::new();
    let channel = Arc::new(Linked::start(Pipes(fake.clone()), link(), |_| {}));
    assert!(until(Duration::from_secs(2), || channel.is_linked()));
    let waiting = {
        let c = channel.clone();
        std::thread::spawn(move || c.call(Request::new("GET", "/slow", Value::Null), Duration::from_secs(10)))
    };
    assert!(until(Duration::from_secs(2), || fake.paths().iter().any(|(_, p)| p == "/slow")));
    let began = Instant::now();
    fake.cut(0);
    assert!(waiting.join().unwrap().is_err(), "a call in flight fails at once");
    assert!(began.elapsed() < Duration::from_secs(2));
    assert!(until(Duration::from_secs(2), || fake.ended.load(Ordering::SeqCst) == 1), "guard handed back");

    // The next call opens a new connection and links again before going through.
    let r = channel.call(Request::new("GET", "/status", Value::Null), Duration::from_secs(5)).unwrap();
    assert_eq!(r.body["connection"], 1);
    let on_second: Vec<String> = fake.paths().into_iter().filter(|(c, _)| *c == 1).map(|(_, p)| p).collect();
    assert_eq!(on_second.first().map(String::as_str), Some("/link"), "{on_second:?}");
    channel.close();
    assert!(until(Duration::from_secs(2), || fake.ended.load(Ordering::SeqCst) == 2));
}

#[test]
fn refused_link_is_an_error_and_nothing_else_is_sent() {
    let fake = Fake::new();
    *fake.refuse.lock().unwrap() = true;
    let channel = Linked::start(Pipes(fake.clone()), link(), |_| {});
    let e = channel.call(Request::new("GET", "/status", Value::Null), Duration::from_secs(5)).unwrap_err();
    assert!(e.contains("503"), "{e}");
    assert!(fake.paths().iter().all(|(_, p)| p == "/link"), "{:?}", fake.paths());
    *fake.refuse.lock().unwrap() = false;
    assert!(channel.call(Request::new("GET", "/status", Value::Null), Duration::from_secs(5)).is_ok());
    channel.close();
    assert!(channel.call(Request::new("GET", "/status", Value::Null), Duration::from_secs(1)).is_err(), "closed");
}
