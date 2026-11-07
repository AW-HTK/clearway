//! Channel of an MCP server to Clearway: one connection multiplexed by `id`, with the session's link opened on it,
//! a heartbeat every 20 s carrying the WSL measures, and a new connection (and link) when it drops.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::model::{Heartbeat, Link, WslMeasures};
use crate::protocol::{Request, Response, read_line_limited};
use crate::tools::Channel;
use crate::tr;

pub const HEARTBEAT: Duration = Duration::from_secs(20);
const LINK_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Connection<G> {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Kept while the connection lives (e.g. the relay process), handed back to `ended`.
    pub guard: G,
}

pub trait Connector: Send + Sync + 'static {
    type Guard: Send + 'static;
    fn connect(&self) -> Result<Connection<Self::Guard>, String>;
    /// The connection is over (closed by either side): its guard comes back, writer already dropped.
    fn ended(&self, _guard: Self::Guard) {}
    fn measures(&self) -> Option<WslMeasures> {
        None
    }
}

struct Live<G> {
    generation: u64,
    writer: Box<dyn Write + Send>,
    guard: G,
    /// `POST /link` answered: other requests may use the connection.
    linked: bool,
}

struct State<G> {
    live: Option<Live<G>>,
    generation: u64,
    /// Request `id` → (generation of its connection, where its response goes).
    pending: HashMap<u64, (u64, mpsc::Sender<Response>)>,
    closed: bool,
    /// The connection dropped: the heartbeat reconnects at once instead of at its next beat.
    reconnect: bool,
}

struct Inner<K: Connector> {
    connector: K,
    link: Link,
    state: Mutex<State<K::Guard>>,
    /// Only one thread connects and links; the others wait for it.
    connecting: Mutex<()>,
    next_id: AtomicU64,
    stop: Condvar,
    log: Box<dyn Fn(&str) + Send + Sync>,
}

/// A [`Channel`] whose connection stands for the session as long as it stays open.
pub struct Linked<K: Connector> {
    inner: Arc<Inner<K>>,
}

impl<K: Connector> Linked<K> {
    /// Connects in the background and keeps the link alive with a heartbeat until [`Linked::close`].
    pub fn start(connector: K, link: Link, log: impl Fn(&str) + Send + Sync + 'static) -> Linked<K> {
        let inner = Arc::new(Inner {
            connector,
            link,
            state: Mutex::new(State { live: None, generation: 0, pending: HashMap::new(), closed: false, reconnect: false }),
            connecting: Mutex::new(()),
            next_id: AtomicU64::new(1),
            stop: Condvar::new(),
            log: Box::new(log),
        });
        let beat = inner.clone();
        std::thread::spawn(move || beat.heartbeat_loop());
        Linked { inner }
    }

    /// Closes the connection: the server then keeps the session's tickets for its resume delay.
    pub fn close(&self) {
        let live = {
            let mut s = self.inner.lock();
            s.closed = true;
            s.pending.clear();
            s.live.take()
        };
        self.inner.stop.notify_all();
        if let Some(l) = live {
            self.inner.finish(l);
        }
    }

    pub fn is_linked(&self) -> bool {
        self.inner.lock().live.as_ref().is_some_and(|l| l.linked)
    }
}

impl<K: Connector> Channel for Linked<K> {
    fn call(&self, request: Request, timeout: Duration) -> Result<Response, String> {
        let generation = self.inner.ensure()?;
        self.inner.send(generation, request, timeout)
    }
}

impl<K: Connector> Inner<K> {
    fn lock(&self) -> std::sync::MutexGuard<'_, State<K::Guard>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lost() -> String {
        tr!("connection to Clearway lost", "connexion à Clearway perdue")
    }

    fn linked_generation(&self) -> Option<u64> {
        self.lock().live.as_ref().filter(|l| l.linked).map(|l| l.generation)
    }

    /// A linked connection, opened (and linked) if needed.
    fn ensure(self: &Arc<Self>) -> Result<u64, String> {
        if let Some(g) = self.linked_generation() {
            return Ok(g);
        }
        let _one = self.connecting.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(g) = self.linked_generation() {
            return Ok(g);
        }
        if self.lock().closed {
            return Err(tr!("MCP server closing", "serveur MCP en cours d'arrêt"));
        }
        let c = self.connector.connect()?;
        let generation = {
            let mut s = self.lock();
            s.generation += 1;
            let generation = s.generation;
            if let Some(old) = s.live.replace(Live { generation, writer: c.writer, guard: c.guard, linked: false }) {
                drop(s);
                self.finish(old);
            }
            generation
        };
        let reader = self.clone();
        std::thread::spawn(move || reader.read_loop(c.reader, generation));

        let mut link = self.link.clone();
        link.measures = self.connector.measures();
        let body = serde_json::to_value(&link).map_err(|e| e.to_string())?;
        let answer = self.send(generation, Request::new("POST", "/link", body), LINK_TIMEOUT);
        match answer {
            Ok(r) if r.code == 200 => {
                let mut s = self.lock();
                match s.live.as_mut().filter(|l| l.generation == generation) {
                    Some(l) => l.linked = true,
                    None => return Err(Self::lost()),
                }
                drop(s);
                (self.log)(&format!("link #{} open, resumed {}", r.body["link"], r.body["resumed"]));
                Ok(generation)
            }
            other => {
                self.drop_live(generation);
                Err(match other {
                    Ok(r) => tr!("link refused ({}): {}", "liaison refusée ({}) : {}", r.code, r.body),
                    Err(e) => e,
                })
            }
        }
    }

    fn send(&self, generation: u64, mut request: Request, timeout: Duration) -> Result<Response, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        request.id = Some(id);
        let mut line = serde_json::to_string(&request).map_err(|e| e.to_string())?;
        line.push('\n');
        let (tx, rx) = mpsc::channel();
        {
            let mut s = self.lock();
            let s = &mut *s;
            let Some(live) = s.live.as_mut().filter(|l| l.generation == generation) else {
                return Err(Self::lost());
            };
            s.pending.insert(id, (generation, tx));
            if live.writer.write_all(line.as_bytes()).and_then(|_| live.writer.flush()).is_err() {
                s.pending.remove(&id);
                drop(rx);
                let _ = s;
                self.drop_live(generation);
                return Err(Self::lost());
            }
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Disconnected) => Err(Self::lost()),
            Err(RecvTimeoutError::Timeout) => {
                self.lock().pending.remove(&id);
                Err(tr!("no response from Clearway within {} s", "pas de réponse de Clearway en {} s", timeout.as_secs()))
            }
        }
    }

    fn read_loop(&self, reader: Box<dyn Read + Send>, generation: u64) {
        let mut reader = BufReader::new(reader);
        while let Ok(Some(line)) = read_line_limited(&mut reader) {
            let Ok(r) = serde_json::from_str::<Response>(&line) else {
                (self.log)(&format!("unreadable response: {line}"));
                continue;
            };
            let Some(id) = r.id else { continue };
            if let Some((_, tx)) = self.lock().pending.remove(&id) {
                let _ = tx.send(r);
            }
        }
        if self.drop_live(generation) {
            (self.log)("connection to Clearway closed");
            self.lock().reconnect = true;
            self.stop.notify_all();
        }
    }

    /// Ends this generation's connection and fails its requests in flight; `false` if it was already over.
    fn drop_live(&self, generation: u64) -> bool {
        let live = {
            let mut s = self.lock();
            s.pending.retain(|_, (g, _)| *g != generation);
            match s.live.as_ref() {
                Some(l) if l.generation == generation => s.live.take(),
                _ => None,
            }
        };
        let ended = live.is_some();
        if let Some(l) = live {
            self.finish(l);
        }
        ended
    }

    fn finish(&self, live: Live<K::Guard>) {
        drop(live.writer);
        self.connector.ended(live.guard);
    }

    fn heartbeat_loop(self: Arc<Self>) {
        let mut reported = String::new();
        let mut wait = Duration::ZERO;
        loop {
            {
                let s = self.lock();
                let (mut s, _) =
                    self.stop.wait_timeout_while(s, wait, |s| !s.closed && !s.reconnect).unwrap_or_else(|e| e.into_inner());
                if s.closed {
                    return;
                }
                s.reconnect = false;
            }
            wait = HEARTBEAT;
            let outcome = self.ensure().and_then(|generation| {
                let body = serde_json::to_value(Heartbeat { measures: self.connector.measures() }).map_err(|e| e.to_string())?;
                self.send(generation, Request::new("POST", "/link/heartbeat", body), LINK_TIMEOUT)
            });
            let now = match outcome {
                Ok(r) if r.code == 200 => String::new(),
                Ok(r) => format!("heartbeat refused ({}): {}", r.code, r.body),
                Err(e) => format!("heartbeat failed: {e}"),
            };
            if now != reported {
                (self.log)(if now.is_empty() { "heartbeat ok" } else { &now });
                reported = now;
            }
            if !reported.is_empty() {
                // Retry sooner while Clearway is unreachable, so that the link comes back within the resume delay.
                wait = Duration::from_secs(5);
            }
        }
    }
}

#[cfg(test)]
mod tests;
