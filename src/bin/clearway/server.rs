use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use clearway::config::Config;
use clearway::format::{duration, gb};
use clearway::model::{
    Heartbeat, Need, OrphanCause, TicketRequest, Link, State, TicketState, Release, Measures, WslMeasures, Presence, Extension, Reason, Status,
    Ticket, DecisionKind, View, TicketView,
};
use clearway::scheduler::{self, Event, FreeReason, Reserve};
use clearway::protocol::{Response, Request, read_line_limited};
use clearway::system::{hhmm, timestamp, unix_now};
use serde_json::{Value, json};

use crate::ui;

const FINISHED_MEMORY: usize = 200;
const PRESSURE_BEFORE_BALLOON_S: u64 = 120;
const BALLOON_INTERVAL_S: u64 = 30 * 60;
const CRITICAL_RAM_MB: u64 = 1024;
const MAX_IN_FLIGHT_PER_CONNECTION: usize = 32;

/// Open link: a session's MCP process, as long as its connection stays open.
struct LiveLink {
    identity: String,
    session: String,
}

/// Balloon rule: at most one per kind every 30 min; a pressure gives one only after 2 continuous min, and only if a
/// ticket is waiting. The rest goes to the log and the tooltip.
#[derive(Default)]
pub struct Balloons {
    last: BTreeMap<&'static str, u64>,
    pressure_since: BTreeMap<&'static str, u64>,
}

impl Balloons {
    pub fn allow(&mut self, kind: &'static str, now: u64) -> bool {
        if self.last.get(kind).is_some_and(|&d| now.saturating_sub(d) < BALLOON_INTERVAL_S) {
            return false;
        }
        self.last.insert(kind, now);
        true
    }

    /// Tracks a pressure; true when it deserves a balloon now.
    pub fn pressure(&mut self, kind: &'static str, active: bool, queue_not_empty: bool, now: u64) -> bool {
        if !active {
            self.pressure_since.remove(kind);
            return false;
        }
        let since = *self.pressure_since.entry(kind).or_insert(now);
        now.saturating_sub(since) >= PRESSURE_BEFORE_BALLOON_S && queue_not_empty && self.allow(kind, now)
    }
}

pub struct Announcement {
    pub title: String,
    pub text: String,
    pub alert: bool,
}

pub struct Server {
    pub state: State,
    pub config: Config,
    dir: PathBuf,
    measures: Measures,
    reasons: BTreeMap<u64, Reason>,
    freeze_texts: Vec<String>,
    pressure: Vec<Reserve>,
    finished: VecDeque<(u64, &'static str, String)>,
    head_key: String,
    announcements: Vec<Announcement>,
    balloons: Balloons,
    swap_history: VecDeque<(u64, u64)>,
    error_persistence: bool,
    links: BTreeMap<u64, LiveLink>,
    link_counter: u64,
}

pub struct Shared {
    server: Mutex<Server>,
    signal: Condvar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Closing {
    Release,
    Cancelled,
    Window,
}

const SWAP_WINDOW_S: u64 = 30;

/// WSL swap-out rate in pages/s over ~30 s, from the cumulative `pswpout` readings.
pub fn swap_out_rate_of(history: &mut VecDeque<(u64, u64)>, now: u64, pswpout: u64) -> f32 {
    // 0 = client too old to read /proc/vmstat: unknown reading, it must not create a false spike.
    if pswpout == 0 {
        return match (history.front(), history.back()) {
            (Some(&(t0, p0)), Some(&(t1, p1))) if t1 > t0 => p1.saturating_sub(p0) as f32 / (t1 - t0) as f32,
            _ => 0.0,
        };
    }
    if history.back().is_some_and(|&(_, p)| p > pswpout) {
        history.clear();
    }
    history.push_back((now, pswpout));
    while history.len() > 2 && history[1].0 + SWAP_WINDOW_S <= now {
        history.pop_front();
    }
    let (t0, p0) = history[0];
    let duration = now.saturating_sub(t0);
    if duration == 0 { 0.0 } else { pswpout.saturating_sub(p0) as f32 / duration as f32 }
}

pub fn log_line(dir: &Path, text: &str) {
    let line = format!("{} {}\n", timestamp(unix_now()), text);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("clearway.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Last lines of the log, read from the end of the file.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn log_tail(dir: &Path, n: usize) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(dir.join("clearway.log")) else {
        return Vec::new();
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let began = size.saturating_sub(16 * 1024);
    let mut bytes = Vec::new();
    if f.seek(SeekFrom::Start(began)).is_err() || f.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if began > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let first_kept = lines.len().saturating_sub(n);
    lines.split_off(first_kept)
}

/// Temporary file, disk flush, then rename: a crash leaves the old or the new version, never a mix.
pub fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
    }
    let mut attempts = 0;
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            // An antivirus or an indexer may hold the file for a few milliseconds.
            Err(_) if attempts < 5 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn state_name(e: TicketState) -> &'static str {
    match e {
        TicketState::Waiting => "waiting",
        TicketState::Active => "active",
        TicketState::Orphaned => "orphaned",
    }
}

fn ticket_summary(t: &Ticket) -> String {
    let location = match (&t.project, &t.branch) {
        (Some(p), Some(b)) => format!(" {p} · {b}"),
        (Some(p), None) => format!(" {p}"),
        _ => String::new(),
    };
    clearway::tr!(
        "[{}]{location} {} ({}, {}, {} core(s), {}{})",
        "[{}]{location} {} ({}, {}, {} cœur(s), {}{})",
        t.session,
        t.description,
        t.side.name(),
        gb(t.need.ram_mb as i64),
        t.need.cpu,
        duration(t.need.duration_s),
        if t.manual { clearway::tr!(", manual", ", manuel") } else { String::new() }
    )
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// Pre-Clearway `etat.json` (French keys) → `state.json`, once; the old file is kept as `etat.json.before-clearway`.
fn migrate_legacy_state(dir: &Path) -> Option<String> {
    let old = dir.join("etat.json");
    let new = dir.join("state.json");
    if new.exists() || !old.exists() {
        return None;
    }
    let bytes = std::fs::read(&old).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let english = clearway::compat::state_from_legacy(value);
    let text = serde_json::to_vec_pretty(&english).ok()?;
    write_atomic(&new, &text).ok()?;
    let _ = std::fs::rename(&old, dir.join("etat.json.before-clearway"));
    Some(clearway::tr!(
        "etat.json of the previous version migrated to state.json (kept as etat.json.before-clearway).",
        "etat.json de la version précédente migré vers state.json (gardé sous etat.json.before-clearway)."
    ))
}

impl Server {
    pub fn start(dir: PathBuf, config: Config, measures: Measures) -> Result<Server, String> {
        let path = dir.join("state.json");
        let mut announcements = Vec::new();
        if let Some(text) = migrate_legacy_state(&dir) {
            log_line(&dir, &format!("INFO {text}"));
        }
        let mut state = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(e) => e,
                Err(e) => {
                    let copy = dir.join(format!("state.json.unreadable-{}", unix_now()));
                    let _ = std::fs::rename(&path, &copy);
                    let text = clearway::tr!(
                        "state.json unreadable ({e}), moved aside to {}; starting empty.",
                        "state.json illisible ({e}), mis de côté dans {} ; départ à vide.",
                        copy.display()
                    );
                    log_line(&dir, &format!("ERROR {text}"));
                    announcements.push(Announcement {
                        title: clearway::tr!("Unreadable state", "État illisible"),
                        text,
                        alert: true,
                    });
                    State::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(clearway::tr!("reading {}: {e}", "lecture de {} : {e}", path.display())),
        };
        state.epoch += 1;
        let now = unix_now();
        for t in &mut state.tickets {
            t.presence = now;
            // Connections do not survive a restart: the session has the resume delay to link again.
            if t.link.is_some() && t.lost.is_none() {
                t.lost = Some(now);
            }
        }
        let mut s = Server {
            state,
            config,
            dir,
            measures,
            reasons: BTreeMap::new(),
            freeze_texts: Vec::new(),
            pressure: Vec::new(),
            finished: VecDeque::new(),
            head_key: String::new(),
            announcements,
            balloons: Balloons::default(),
            swap_history: VecDeque::new(),
            error_persistence: false,
            links: BTreeMap::new(),
            link_counter: 0,
        };
        s.persist(&s.state)
            .map_err(|e| clearway::tr!("writing state.json: {e}", "écriture de state.json : {e}"))?;
        let resumed = s.state.tickets.len();
        s.log(&format!(
            "START {}",
            clearway::tr!(
                "epoch {} on pipe {} · {resumed} ticket(s) resumed",
                "époque {} sur le tube {} · {resumed} ticket(s) repris",
                s.state.epoch,
                s.config.pipe
            )
        ));
        let candidate = s.state.clone();
        s.commit(candidate, false, None, None)?;
        Ok(s)
    }

    fn log(&self, text: &str) {
        log_line(&self.dir, text);
    }

    fn persist(&self, e: &State) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(e).map_err(io::Error::other)?;
        write_atomic(&self.dir.join("state.json"), &bytes)
    }

    fn announce(&mut self, kind: &'static str, title: String, text: String) {
        if self.balloons.allow(kind, unix_now()) {
            self.announcements.push(Announcement { title, text, alert: true });
        }
    }

    fn finish(&mut self, id: u64, motive: &'static str, message: String) {
        self.finished.push_back((id, motive, message));
        while self.finished.len() > FINISHED_MEMORY {
            self.finished.pop_front();
        }
    }

    fn update_wsl(&mut self, w: Option<WslMeasures>, now: u64) {
        if let Some(mut w) = w {
            w.at = now;
            self.measures.wsl = Some(w);
            self.measures.swap_out_ps = swap_out_rate_of(&mut self.swap_history, now, w.pswpout);
        }
    }

    /// Schedules on `candidate`, marks the clearance of `delivered` as delivered if it gets one, writes to disk if a
    /// decision changes, and only then makes it all visible.
    fn commit(&mut self, candidate: State, force_through: bool, delivered: Option<u64>, prelude: Option<String>) -> Result<(), String> {
        let now = unix_now();
        let mut plan = scheduler::schedule(&candidate, &self.measures, &self.config, now);
        let mut force_through = force_through;
        if let Some(t) = delivered.and_then(|id| plan.state.ticket_mut(id))
            && t.state == TicketState::Active
            && !t.acked
        {
            t.acked = true;
            force_through = true;
        }
        if plan.persist || force_through {
            if let Err(e) = self.persist(&plan.state) {
                let text = clearway::tr!(
                    "cannot write state.json: {e}; decision cancelled.",
                    "écriture de state.json impossible : {e} ; décision annulée."
                );
                if !self.error_persistence {
                    self.log(&format!("ERROR {text}"));
                    self.announce("error", clearway::tr!("Persistence failing", "Persistance en échec"), text.clone());
                }
                self.error_persistence = true;
                return Err(text);
            }
            self.error_persistence = false;
        }
        let old = std::mem::replace(&mut self.state, plan.state);
        self.reasons = plan.reasons;
        if let Some(p) = prelude {
            self.log(&p);
        }

        for ev in &plan.events {
            match *ev {
                Event::Cleared(id) => {
                    let t = self.state.ticket(id).expect("granted ticket present").clone();
                    let message = self.reasons.get(&id).map(|r| r.message.clone()).unwrap_or_default();
                    self.log(&format!("CLEARED #{id} {} · {message}", ticket_summary(&t)));
                }
                Event::Recalled(id) => {
                    let what = self.state.ticket(id).map(ticket_summary).unwrap_or_default();
                    let message = clearway::tr!(
                        "Ticket #{id}: clearance withdrawn, nobody took it within {}; place kept, `clearway wait {id}` to get it back.",
                        "Ticket #{id} : feu vert repris, personne ne l'a pris en {} ; place gardée, « clearway wait {id} » pour le récupérer.",
                        duration(self.config.recall_delay_s)
                    );
                    self.log(&format!("RECALLED #{id} {what} · {message}"));
                }
                Event::Expired(id) => {
                    let what = old.ticket(id).map(ticket_summary).unwrap_or_default();
                    let message = clearway::tr!(
                        "Ticket #{id} removed: no client had been waiting for it for more than {} (nothing was running yet).",
                        "Ticket #{id} retiré : aucun client ne l'attendait depuis plus de {} (rien ne tournait encore).",
                        duration(self.config.absence_delay_s)
                    );
                    self.log(&format!("EXPIRED #{id} {what} · {message}"));
                    self.finish(id, "expired", message);
                }
                Event::LeaseOverrun(id) => {
                    let what = self.state.ticket(id).map(ticket_summary).unwrap_or_default();
                    let message = self.reasons.get(&id).map(|r| r.message.clone()).unwrap_or_default();
                    self.log(&format!("LEASE OVERRUN #{id} {what} · {message}"));
                }
                Event::Orphaned(id) | Event::Lease(id) => {
                    let what = self.state.ticket(id).map(ticket_summary).unwrap_or_default();
                    let message = self.reasons.get(&id).map(|r| r.message.clone()).unwrap_or_default();
                    let tag = if matches!(ev, Event::Lease(_)) { "LEASE EXPIRED" } else { "ORPHANED" };
                    self.log(&format!("{tag} #{id} {what} · {message}"));
                    self.announce(
                        "orphaned",
                        clearway::tr!("Ticket #{id} orphaned", "Ticket #{id} orphelin"),
                        truncate(&message, 250),
                    );
                }
                Event::Freed(id, motive) => {
                    let what = old.ticket(id).map(ticket_summary).unwrap_or_default();
                    let message = match motive {
                        FreeReason::WslStopped => clearway::tr!(
                            "Ticket #{id} freed: WSL stopped or restarted.",
                            "Ticket #{id} libéré : WSL s'est arrêté ou a redémarré."
                        ),
                        FreeReason::SessionClosed => clearway::tr!(
                            "Ticket #{id} removed: its session's link was lost, with no resume within {}.",
                            "Ticket #{id} retiré : liaison de sa session perdue, sans reprise en {}.",
                            duration(self.config.resume_delay_s)
                        ),
                    };
                    self.log(&format!("FREED #{id} {what} · {message}"));
                    self.finish(id, "freed", message);
                }
            }
        }

        let new: Vec<Reserve> = plan.pressure.iter().copied().filter(|r| !self.pressure.contains(r)).collect();
        for r in &new {
            let text = plan
                .freeze_texts
                .iter()
                .find(|g| {
                    g.contains(r.name()) || (*r == Reserve::RamWsl && (g.contains("swap WSL") || g.contains("WSL swap")))
                })
                .cloned()
                .unwrap_or_default();
            self.log(&format!("PRESSURE {} · {text}", r.name()));
        }
        let queue_not_empty = self.state.tickets.iter().any(|t| t.state == TicketState::Waiting);
        for r in [Reserve::RamWindows, Reserve::RamWsl] {
            let kind = if r == Reserve::RamWindows { "pressure ram_windows" } else { "pressure ram_wsl" };
            if self.balloons.pressure(kind, plan.pressure.contains(&r), queue_not_empty, now) {
                let text = plan.freeze_texts.iter().find(|g| g.contains(r.name())).cloned().unwrap_or_default();
                self.announcements.push(Announcement {
                    title: clearway::tr!("Pressure on {} for 2 min", "Pression sur la {} depuis 2 min", r.name()),
                    text: truncate(&text, 250),
                    alert: true,
                });
            }
        }
        let avail = self.measures.ram_avail_mb;
        if avail > 0 && avail < CRITICAL_RAM_MB {
            self.announce(
                "critical_ram",
                clearway::tr!("Windows RAM critical", "RAM Windows critique"),
                clearway::tr!(
                    "Only {} left available on Windows: close a heavy program.",
                    "Plus que {} disponible sous Windows : ferme un programme lourd.",
                    gb(avail as i64)
                ),
            );
        }
        for r in self.pressure.iter().filter(|r| !plan.pressure.contains(r)) {
            self.log(&format!(
                "BACK TO NORMAL {} · {}",
                r.name(),
                clearway::tr!("admissions possible again.", "admissions de nouveau possibles.")
            ));
        }
        self.pressure = plan.pressure;
        self.freeze_texts = plan.freeze_texts;

        let head = self
            .state
            .tickets
            .iter()
            .find(|t| t.state == TicketState::Waiting)
            .and_then(|t| self.reasons.get(&t.id));
        let key = head.map(|r| r.key()).unwrap_or_default();
        if key != self.head_key {
            if let Some(r) = head {
                self.log(&format!("WAITING {}", r.message));
            }
            self.head_key = key;
        }
        Ok(())
    }

    fn view(&self, id: u64) -> Option<View> {
        if let Some(t) = self.state.ticket(id) {
            let reason = self.reasons.get(&id).cloned().unwrap_or_else(|| {
                Reason::simple(
                    DecisionKind::Waiting,
                    clearway::tr!("#{id} waiting for the next decision.", "#{id} en attente de la prochaine décision."),
                )
            });
            return Some(View { id, state: state_name(t.state).into(), epoch: self.state.epoch, reason });
        }
        self.finished.iter().rev().find(|(i, _, _)| *i == id).map(|(_, motive, message)| View {
            id,
            state: (*motive).into(),
            epoch: self.state.epoch,
            reason: Reason::simple(DecisionKind::Finished, message.clone()),
        })
    }

    fn status_view(&self) -> Status {
        let now = unix_now();
        let mut alerts = self.freeze_texts.clone();
        if self.measures.vmmem.is_none() {
            alerts.push(clearway::tr!(
                "WSL stopped: only the Windows reserves count.",
                "WSL arrêté : seules les réserves Windows comptent."
            ));
        }
        if let Some(info) = scheduler::calm_swap(&self.state, &self.measures, &self.config, now) {
            alerts.push(clearway::tr!("Information: {info}, no freeze.", "Information : {info}, pas de gel."));
        }
        Status {
            epoch: self.state.epoch,
            now,
            measures: self.measures.clone(),
            reserves: scheduler::reserve_views(&self.state, &self.measures, &self.config, now),
            tickets: self
                .state
                .tickets
                .iter()
                .map(|t| TicketView {
                    id: t.id,
                    state: t.state,
                    side: t.side,
                    need: t.need,
                    description: t.description.clone(),
                    session: t.session.clone(),
                    project: t.project.clone(),
                    branch: t.branch.clone(),
                    created: t.created,
                    granted: t.granted,
                    manual: t.manual,
                    force: t.force,
                    lease_overrun: t.lease_overrun && t.state == TicketState::Active,
                    presence_age_s: now.saturating_sub(t.presence),
                    message: self.reasons.get(&t.id).map(|r| r.message.clone()).unwrap_or_default(),
                })
                .collect(),
            alerts,
            manual_freeze: self.state.manual_freeze,
        }
    }

    fn icon(&self) -> (ui::Color, String) {
        let count = |e| self.state.tickets.iter().filter(|t| t.state == e).count();
        let (waiting, actives, orphans) = (count(TicketState::Waiting), count(TicketState::Active), count(TicketState::Orphaned));
        let color = if !self.freeze_texts.is_empty() || orphans > 0 {
            ui::Color::Red
        } else if waiting > 0 {
            ui::Color::Orange
        } else if actives > 0 {
            ui::Color::Blue
        } else {
            ui::Color::Green
        };
        let lev = scheduler::reserve_views(&self.state, &self.measures, &self.config, unix_now());
        let by_key = |k: &str| lev.iter().find(|r| r.key == k).expect("every reserve has a view");
        let (ram_windows, cpu_windows, ram_wsl, cpu_wsl) =
            (by_key("ram_windows"), by_key("cpu_windows"), by_key("ram_wsl"), by_key("cpu"));
        let wsl = match (self.measures.vmmem, ram_wsl.estimate) {
            (None, _) => clearway::tr!("stopped", "arrêté"),
            (Some(_), Some(_)) => clearway::tr!(
                "≈ {} (estimated) · CPU {}",
                "≈ {} (estimé) · CPU {}",
                gb(ram_wsl.free.max(0)),
                cpu_wsl.free.max(0)
            ),
            (Some(_), None) => format!("{} · CPU {}", ram_wsl.free_text(), cpu_wsl.free.max(0)),
        };
        let mut text = clearway::tr!(
            "Load: {actives} active, {waiting} waiting{}\nFree: Win {} · CPU {} | WSL {}",
            "Charge : {actives} actif(s), {waiting} en attente{}\nLibre : Win {} · CPU {} | WSL {}",
            if orphans > 0 { clearway::tr!(", {orphans} orphaned", ", {orphans} orphelin(s)") } else { String::new() },
            ram_windows.free_text(),
            cpu_windows.free.max(0),
            wsl
        );
        let head = self
            .state
            .tickets
            .iter()
            .find(|t| t.state == TicketState::Waiting)
            .and_then(|t| self.reasons.get(&t.id))
            .map(|r| r.message.clone())
            .or_else(|| self.freeze_texts.first().cloned());
        if let Some(info) = scheduler::calm_swap(&self.state, &self.measures, &self.config, unix_now()) {
            text.push_str(&format!("\n{info}"));
        }
        if let Some(p) = head {
            text.push('\n');
            text.push_str(&p);
        }
        (color, text)
    }
}

fn response(code: u16, body: Value) -> Response {
    Response { id: None, code, body }
}

fn error(code: u16, text: &str) -> Response {
    response(code, json!({ "error": text }))
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or_else(|e| json!({ "error": e.to_string() }))
}

fn not_found(s: &Server, id: u64) -> Response {
    match s.view(id) {
        Some(v) => response(404, to_json(&v)),
        None => error(404, &clearway::tr!("Ticket #{id} unknown.", "Ticket #{id} inconnu.")),
    }
}

fn wsl_since_params(req: &Request) -> Option<WslMeasures> {
    Some(WslMeasures {
        mem_avail_mb: req.param("mem_avail_mb")?.parse().ok()?,
        mem_total_mb: req.param("mem_total_mb")?.parse().ok()?,
        load1: req.param("load1")?.parse().ok()?,
        swap_total_mb: req.param("swap_total_mb").and_then(|v| v.parse().ok()).unwrap_or(0),
        swap_free_mb: req.param("swap_free_mb").and_then(|v| v.parse().ok()).unwrap_or(0),
        pswpin: req.param("pswpin").and_then(|v| v.parse().ok()).unwrap_or(0),
        pswpout: req.param("pswpout").and_then(|v| v.parse().ok()).unwrap_or(0),
        at: 0,
    })
}

impl Shared {
    pub fn new(server: Server) -> Shared {
        Shared { server: Mutex::new(server), signal: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, Server> {
        self.server.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wakes the waiters, then updates the tray icon and the balloons outside the lock.
    fn publish(&self, mut s: MutexGuard<'_, Server>) {
        self.signal.notify_all();
        let announcements = std::mem::take(&mut s.announcements);
        let (color, text) = s.icon();
        drop(s);
        ui::update(color, &text);
        for a in announcements {
            ui::balloon(&a.title, &a.text, a.alert);
        }
    }

    pub fn tick(&self, measures: Measures) {
        let mut s = self.lock();
        let (wsl, swap_out_ps) = (s.measures.wsl, s.measures.swap_out_ps);
        s.measures = Measures { wsl, swap_out_ps, ..measures };
        let candidate = s.state.clone();
        let _ = s.commit(candidate, false, None, None);
        self.publish(s);
    }

    pub fn dir(&self) -> PathBuf {
        self.lock().dir.clone()
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn snapshot(&self) -> Status {
        self.lock().status_view()
    }

    /// Status after taking a fresh WSL measure into account, so that a first `clearway status` shows no stale freeze.
    fn state_with(&self, wsl: Option<WslMeasures>) -> Status {
        let mut s = self.lock();
        if wsl.is_some() {
            s.update_wsl(wsl, unix_now());
            let candidate = s.state.clone();
            let _ = s.commit(candidate, false, None, None);
        }
        let status_view = s.status_view();
        self.publish(s);
        status_view
    }

    pub fn log(&self, text: &str) {
        self.lock().log(text);
    }

    /// Confirmation question for forcing a ticket through, with the quantified risk.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn force_preview(&self, id: u64) -> Option<String> {
        let s = self.lock();
        let mut text = scheduler::force_preview(&s.state, id, &s.measures, &s.config, unix_now())?;
        if !s.pressure.is_empty() {
            text += &clearway::tr!(
                " A pressure is ongoing: risk of slowdown.",
                " Une pression est en cours : risque de ralentissement."
            );
        } else if !s.freeze_texts.is_empty() {
            text += &clearway::tr!(
                " Admissions are frozen: forcing overrides the freeze.",
                " Les admissions sont gelées : le forçage passe outre."
            );
        }
        Some(text)
    }

    /// Clearance given by hand from the window; no pipe route leads here.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn force_through(&self, id: u64) -> Result<(), String> {
        let mut s = self.lock();
        let (candidate, detail) = scheduler::force_through(&s.state, id, &s.measures, &s.config, unix_now())?;
        let prelude = format!("FORCED {detail} · {}", clearway::tr!("by hand (window)", "à la main (fenêtre)"));
        let r = s.commit(candidate, true, None, Some(prelude));
        self.publish(s);
        r
    }

    pub fn freeze(&self, frozen: bool, origin: &str) -> Response {
        let mut s = self.lock();
        if s.state.manual_freeze.is_some() == frozen {
            let message = if frozen {
                clearway::tr!("The queue is already frozen.", "La file est déjà gelée.")
            } else {
                clearway::tr!("The queue is not frozen.", "La file est pas gelée.")
            };
            return response(200, json!({ "frozen": frozen, "message": message }));
        }
        let mut candidate = s.state.clone();
        candidate.manual_freeze = frozen.then(unix_now);
        let (tag, message) = if frozen {
            (
                "MANUAL FREEZE",
                clearway::tr!(
                    "Queue frozen: no new admission until it is unfrozen.",
                    "File gelée : aucune nouvelle admission jusqu'au dégel."
                ),
            )
        } else {
            ("MANUAL UNFREEZE", clearway::tr!("Queue unfrozen: admissions resume.", "File dégelée : les admissions reprennent."))
        };
        let rep = match s.commit(candidate, true, None, Some(format!("{tag} ({origin}) · {message}"))) {
            Ok(()) => response(200, json!({ "frozen": frozen, "message": message })),
            Err(e) => error(503, &e),
        };
        self.publish(s);
        rep
    }

    pub fn close_ticket(&self, id: u64, f: &Release, comment: Closing) -> Response {
        let mut s = self.lock();
        let Some(t) = s.state.ticket(id).cloned() else {
            return not_found(&s, id);
        };
        let mut candidate = s.state.clone();
        candidate.tickets.retain(|x| x.id != id);
        let was = state_name(t.state);
        let (prelude, motive, message) = match comment {
            Closing::Release => {
                let mut details = Vec::new();
                if let Some(c) = f.code {
                    details.push(format!("code {c}"));
                }
                if let Some(peak) = f.peak_mb {
                    details.push(clearway::tr!("RSS peak {}", "pic RSS {}", gb(peak as i64)));
                }
                if let Some(d) = f.duration_s {
                    details.push(clearway::tr!("duration {}", "durée {}", duration(d)));
                }
                let rest = if details.is_empty() { String::new() } else { format!(" · {}", details.join(" · ")) };
                (
                    format!("RELEASED #{id} {}{rest}", ticket_summary(&t)),
                    "finished",
                    clearway::tr!("Ticket #{id} finished, slot freed.", "Ticket #{id} terminé, créneau libéré."),
                )
            }
            Closing::Cancelled => (
                format!("CANCELLED #{id} {} {}", ticket_summary(&t), clearway::tr!("(was {was})", "(était {was})")),
                "cancelled",
                clearway::tr!("Ticket #{id} cancelled.", "Ticket #{id} annulé."),
            ),
            Closing::Window => (
                format!(
                    "FREED BY HAND #{id} {} {}",
                    ticket_summary(&t),
                    clearway::tr!("(was {was}) from the window", "(était {was}) depuis la fenêtre")
                ),
                "freed",
                clearway::tr!(
                    "Ticket #{id} freed by hand from the Clearway window.",
                    "Ticket #{id} libéré à la main depuis la fenêtre de Clearway."
                ),
            ),
        };
        let rep = match s.commit(candidate, true, None, Some(prelude)) {
            Ok(()) => {
                s.finish(id, motive, message.clone());
                response(200, json!({ "id": id, "state": motive, "message": message }))
            }
            Err(e) => error(503, &e),
        };
        self.publish(s);
        rep
    }
}

/// What the server knows about the connection carrying a request.
pub struct Context {
    probe: Box<dyn Fn() -> bool + Send + Sync>,
    closed: AtomicBool,
    link: Mutex<Option<u64>>,
}

impl Context {
    /// `probe` tells whether the client is still there while the reader thread is not reading (requests without `id`).
    pub fn new(probe: impl Fn() -> bool + Send + Sync + 'static) -> Arc<Context> {
        Arc::new(Context { probe: Box::new(probe), closed: AtomicBool::new(false), link: Mutex::new(None) })
    }

    fn alive(&self) -> bool {
        !self.closed.load(Ordering::SeqCst) && (self.probe)()
    }

    fn link(&self) -> Option<u64> {
        *self.link.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn handle(p: &Shared, req: &Request, ctx: &Context) -> Response {
    let alive = &|| ctx.alive();
    let segments = req.segments();
    let id = |s: &str| s.parse::<u64>().ok();
    let invalid = || error(404, &clearway::tr!("invalid ticket", "ticket invalide"));
    match (req.method.as_str(), segments.as_slice()) {
        ("POST", ["tickets"]) => create(p, req, ctx),
        ("POST", ["link"]) => open_link(p, req, ctx),
        ("POST", ["link", "heartbeat"]) => heartbeat(p, req, ctx),
        ("GET", ["tickets", n, "wait"]) => id(n).map_or_else(invalid, |i| wait(p, req, i, alive)),
        ("POST", ["tickets", n, "ack"]) => id(n).map_or_else(invalid, |i| acknowledge(p, i)),
        ("POST", ["tickets", n, "presence"]) => id(n).map_or_else(invalid, |i| presence(p, req, i)),
        ("POST", ["tickets", n, "extend"]) => id(n).map_or_else(invalid, |i| extend(p, req, i)),
        ("POST", ["tickets", n, "release"]) => id(n).map_or_else(invalid, |i| {
            let f: Release = serde_json::from_value(req.body.clone()).unwrap_or_default();
            p.close_ticket(i, &f, Closing::Release)
        }),
        ("DELETE", ["tickets", n]) => id(n).map_or_else(invalid, |i| p.close_ticket(i, &Release::default(), Closing::Cancelled)),
        ("GET", ["status"]) => response(200, to_json(&p.state_with(wsl_since_params(req)))),
        ("POST", ["freeze"]) => p.freeze(req.body["frozen"].as_bool().unwrap_or(true), &clearway::tr!("command", "commande")),
        ("POST", ["window"]) => {
            ui::window(req.body["open"].as_bool().unwrap_or(true));
            response(200, json!({ "message": clearway::tr!("Request passed on to the window.", "Demande transmise à la fenêtre.") }))
        }
        ("POST", ["quit"]) => {
            p.log(&format!("STOP {}", clearway::tr!("requested through the pipe.", "demandé par le tube.")));
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(200));
                ui::quit();
            });
            response(200, json!({ "message": clearway::tr!("Clearway is stopping.", "Arrêt de Clearway en cours.") }))
        }
        _ => error(404, &clearway::tr!("unknown route", "route inconnue")),
    }
}

fn create(p: &Shared, req: &Request, ctx: &Context) -> Response {
    let d: TicketRequest = match serde_json::from_value(req.body.clone()) {
        Ok(d) => d,
        Err(e) => return error(400, &clearway::tr!("unreadable request: {e}", "demande illisible : {e}")),
    };
    let now = unix_now();
    let mut s = p.lock();
    s.update_wsl(d.measures, now);
    let delivered = |id: u64| (!d.ack).then_some(id);
    if let Some(id) = s.state.tickets.iter().find(|t| t.request_id == d.request_id).map(|t| t.id) {
        return response(200, to_json(&s.view(id)));
    }
    // The same command run again takes back its ticket, provided nobody else is waiting on it.
    let resumed = (!d.reattach_key.is_empty())
        .then(|| {
            s.state.tickets.iter().find(|t| {
                t.reattach_key == d.reattach_key
                    && ((t.state == TicketState::Waiting && t.listeners == 0) || (t.state == TicketState::Active && !t.acked))
            })
        })
        .flatten()
        .map(|t| t.id);
    if let Some(id) = resumed {
        let mut candidate = s.state.clone();
        if let Some(t) = candidate.ticket_mut(id) {
            t.recalled = None;
            t.presence = now;
        }
        let prelude = format!(
            "REATTACHED {}",
            clearway::tr!("#{id}: the same command takes back its ticket.", "#{id} : la même commande reprend son ticket.")
        );
        let rep = match s.commit(candidate, true, delivered(id), Some(prelude)) {
            Ok(()) => response(200, to_json(&s.view(id))),
            Err(e) => error(503, &e),
        };
        p.publish(s);
        return rep;
    }
    let default_need = s.config.default_need(d.side);
    let need = Need {
        ram_mb: d.ram_mb.unwrap_or(default_need.ram_mb),
        cpu: d.cpu.unwrap_or(default_need.cpu),
        duration_s: d.duration_s.unwrap_or(default_need.duration_s),
    };
    if let Some(reason) = scheduler::refusal(d.side, &need, &s.measures, &s.config) {
        s.log(&format!("REFUSED [{}] {} · {}", d.session, d.description, reason.message));
        s.announce(
            "refused",
            clearway::tr!("Request refused: {}", "Demande refusée : {}", d.session),
            truncate(&reason.message, 250),
        );
        let v = View { id: 0, state: "refused".into(), epoch: s.state.epoch, reason };
        let rep = response(422, to_json(&v));
        p.publish(s);
        return rep;
    }
    let link = ctx.link();
    let identity = link.and_then(|l| s.links.get(&l)).map(|l| l.identity.clone());
    let mut candidate = s.state.clone();
    candidate.counter += 1;
    let id = candidate.counter;
    let t = Ticket {
        id,
        request_id: d.request_id,
        side: d.side,
        need,
        description: d.description,
        session: d.session,
        claude_session_id: d.claude_session_id,
        claude_pid: d.claude_pid,
        dir: d.dir,
        signature: d.signature,
        state: TicketState::Waiting,
        created: now,
        granted: None,
        avail_at_grant_mb: None,
        manual: d.manual,
        acked: false,
        lease_overrun: false,
        recalled: None,
        orphan_cause: None,
        reattach_key: d.reattach_key,
        project: d.project,
        branch: d.branch,
        force: false,
        base_ram_windows_mb: None,
        base_ram_wsl_mb: None,
        base_vmmem_mb: None,
        overtakes: 0,
        link,
        identity,
        lost: None,
        presence: now,
        listeners: 0,
    };
    let prelude = format!("REQUEST #{id} {}", ticket_summary(&t));
    candidate.tickets.push(t);
    let rep = match s.commit(candidate, true, delivered(id), Some(prelude)) {
        Ok(()) => response(201, to_json(&s.view(id))),
        Err(e) => error(503, &e),
    };
    p.publish(s);
    rep
}

fn wait(p: &Shared, req: &Request, id: u64, alive: &dyn Fn() -> bool) -> Response {
    let known = req.param("known").unwrap_or("").to_owned();
    // A client that acknowledges confirms through `/ack`; otherwise, sending the response counts as delivery.
    let ack = req.param("ack") == Some("1");
    let mut s = p.lock();
    s.update_wsl(wsl_since_params(req), unix_now());
    let cap = req.param("timeout_s").and_then(|d| d.parse::<u64>().ok()).unwrap_or(u64::MAX);
    let delay = Duration::from_secs(s.config.long_poll_s.min(cap));
    let Some(t) = s.state.ticket(id) else {
        return not_found(&s, id);
    };
    if t.recalled.is_some() {
        let mut candidate = s.state.clone();
        if let Some(c) = candidate.ticket_mut(id) {
            c.recalled = None;
            c.presence = unix_now();
        }
        let prelude = format!(
            "BACK {}",
            clearway::tr!("#{id}: a client is waiting for this ticket again.", "#{id} : un client attend de nouveau ce ticket.")
        );
        let _ = s.commit(candidate, true, None, Some(prelude));
    }
    if let Some(t) = s.state.ticket_mut(id) {
        t.listeners += 1;
        t.presence = unix_now();
    }
    let limit = Instant::now() + delay;
    let view = loop {
        let v = s.view(id);
        let change = v.as_ref().is_none_or(|v| v.state != "waiting" || v.reason.key() != known);
        let left = limit.saturating_duration_since(Instant::now());
        if change || left.is_zero() {
            break v;
        }
        s = p.signal.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        // Client gone during the wait: it no longer counts as present.
        if !alive() {
            break None;
        }
    };
    if let Some(t) = s.state.ticket_mut(id) {
        t.listeners = t.listeners.saturating_sub(1);
        t.presence = unix_now();
    }
    let rep = match view {
        Some(v) if v.reason.decision == DecisionKind::Finished => response(404, to_json(&v)),
        Some(v) => {
            if v.state == "active" && !ack {
                let candidate = s.state.clone();
                let _ = s.commit(candidate, false, Some(id), None);
            }
            response(200, to_json(&v))
        }
        None => error(404, &clearway::tr!("Ticket #{id} unknown or client gone.", "Ticket #{id} inconnu ou client parti.")),
    };
    p.publish(s);
    rep
}

fn presence(p: &Shared, req: &Request, id: u64) -> Response {
    let body: Presence = serde_json::from_value(req.body.clone()).unwrap_or_default();
    let now = unix_now();
    let mut s = p.lock();
    s.update_wsl(body.measures, now);
    let Some(t) = s.state.ticket_mut(id) else {
        return not_found(&s, id);
    };
    t.presence = now;
    let (state, cause, acked) = (t.state, t.orphan_cause, t.acked);
    if state == TicketState::Orphaned && !matches!(cause, Some(OrphanCause::Lease | OrphanCause::Connection)) {
        let mut candidate = s.state.clone();
        if let Some(c) = candidate.ticket_mut(id) {
            c.state = TicketState::Active;
            c.orphan_cause = None;
            c.acked = true;
        }
        let prelude = format!(
            "BACK {}",
            clearway::tr!("#{id}: signal received, ticket active again.", "#{id} : signal reçu, ticket de nouveau actif.")
        );
        let _ = s.commit(candidate, true, None, Some(prelude));
    } else if state == TicketState::Active && !acked {
        let candidate = s.state.clone();
        let _ = s.commit(candidate, false, Some(id), None);
    }
    let state = s.state.ticket(id).map(|t| state_name(t.state)).unwrap_or("unknown");
    let rep = response(200, json!({ "state": state, "epoch": s.state.epoch, "order": null }));
    p.publish(s);
    rep
}

fn acknowledge(p: &Shared, id: u64) -> Response {
    let mut s = p.lock();
    let Some(t) = s.state.ticket_mut(id) else {
        return not_found(&s, id);
    };
    t.presence = unix_now();
    let (state, acked) = (t.state, t.acked);
    let rep = match state {
        TicketState::Waiting => {
            let message = clearway::tr!(
                "Ticket #{id}: clearance withdrawn before the acknowledgement, the ticket is waiting again. Run `clearway wait {id}` again.",
                "Ticket #{id} : feu vert repris avant l'accusé, le ticket attend de nouveau. Relance « clearway wait {id} »."
            );
            error(409, &message)
        }
        TicketState::Active if !acked => {
            let mut candidate = s.state.clone();
            if let Some(c) = candidate.ticket_mut(id) {
                c.acked = true;
            }
            let prelude = format!(
                "ACK {}",
                clearway::tr!("#{id}: clearance received by the client.", "#{id} : feu vert reçu par le client.")
            );
            match s.commit(candidate, true, None, Some(prelude)) {
                Ok(()) => response(
                    200,
                    json!({
                        "id": id,
                        "state": "active",
                        "message": clearway::tr!("Clearance of #{id} acknowledged.", "Feu vert du #{id} accusé.")
                    }),
                ),
                Err(e) => error(503, &e),
            }
        }
        _ => response(
            200,
            json!({
                "id": id,
                "state": state_name(state),
                "message": clearway::tr!("Clearance of #{id} already acknowledged.", "Feu vert du #{id} déjà accusé.")
            }),
        ),
    };
    p.publish(s);
    rep
}

fn extend(p: &Shared, req: &Request, id: u64) -> Response {
    let d: Extension = match serde_json::from_value(req.body.clone()) {
        Ok(d) => d,
        Err(e) => return error(400, &clearway::tr!("unreadable extension: {e}", "prolongation illisible : {e}")),
    };
    let now = unix_now();
    let mut s = p.lock();
    let Some(t) = s.state.ticket(id).cloned() else {
        return not_found(&s, id);
    };
    let (Some(granted), true) = (t.granted, t.state != TicketState::Waiting) else {
        return error(
            409,
            &clearway::tr!(
                "Ticket #{id} waiting: nothing to extend, its duration counts from the clearance.",
                "Ticket #{id} en attente : rien à prolonger, sa durée compte à partir du feu vert."
            ),
        );
    };
    let end = t.planned_end().unwrap_or(now).max(now) + d.duration_s;
    let mut candidate = s.state.clone();
    if let Some(c) = candidate.ticket_mut(id) {
        c.need.duration_s = end - granted;
        c.state = TicketState::Active;
        c.orphan_cause = None;
        c.acked = true;
        c.lease_overrun = false;
        c.presence = now;
    }
    let message = clearway::tr!(
        "Ticket #{id} extended by {}: planned end around {}.",
        "Ticket #{id} prolongé de {} : fin prévue vers {}.",
        duration(d.duration_s),
        hhmm(end)
    );
    let prelude = format!(
        "EXTENDED #{id} {} · {message} {}",
        ticket_summary(&t),
        clearway::tr!("Reason: {}", "Raison : {}", d.reason)
    );
    let rep = match s.commit(candidate, true, None, Some(prelude)) {
        Ok(()) => response(200, json!({ "id": id, "state": "active", "message": message })),
        Err(e) => error(503, &e),
    };
    p.publish(s);
    rep
}

fn open_link(p: &Shared, req: &Request, ctx: &Context) -> Response {
    let l: Link = match serde_json::from_value(req.body.clone()) {
        Ok(l) => l,
        Err(e) => return error(400, &clearway::tr!("unreadable link: {e}", "liaison illisible : {e}")),
    };
    if l.identity.trim().is_empty() {
        return error(400, &clearway::tr!("link without a session identity", "liaison sans identité de session"));
    }
    let mut guard = ctx.link.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(already) = *guard {
        return error(
            409,
            &clearway::tr!(
                "Link #{already} already open on this connection.",
                "Liaison #{already} déjà ouverte sur cette connexion."
            ),
        );
    }
    let now = unix_now();
    let mut s = p.lock();
    s.update_wsl(l.measures, now);
    s.link_counter += 1;
    let id = s.link_counter;
    s.links.insert(id, LiveLink { identity: l.identity.clone(), session: l.session.clone() });
    let mut candidate = s.state.clone();
    let mut resumed = Vec::new();
    for t in candidate.tickets.iter_mut().filter(|t| t.lost.is_some() && t.identity.as_deref() == Some(l.identity.as_str())) {
        t.link = Some(id);
        t.lost = None;
        t.presence = now;
        if t.state == TicketState::Orphaned && t.orphan_cause == Some(OrphanCause::Connection) {
            t.state = TicketState::Active;
            t.orphan_cause = None;
        }
        resumed.push(t.id);
    }
    *guard = Some(id);
    let rest = if resumed.is_empty() {
        clearway::tr!("no ticket to resume", "aucun ticket à reprendre")
    } else {
        let ids: Vec<String> = resumed.iter().map(|i| format!("#{i}")).collect();
        clearway::tr!("resuming {}", "reprise de {}", ids.join(", "))
    };
    let prelude = format!("LINK #{id} {} [{}] {} · {rest}", clearway::tr!("opened", "ouverte"), l.session, l.client);
    let rep = match s.commit(candidate, !resumed.is_empty(), None, Some(prelude)) {
        Ok(()) => response(200, json!({ "link": id, "epoch": s.state.epoch, "resumed": resumed })),
        Err(e) => {
            s.links.remove(&id);
            *guard = None;
            error(503, &e)
        }
    };
    p.publish(s);
    rep
}

fn heartbeat(p: &Shared, req: &Request, ctx: &Context) -> Response {
    let Some(l) = ctx.link() else {
        return error(
            409,
            &clearway::tr!(
                "No link on this connection: POST /link first.",
                "Aucune liaison sur cette connexion : POST /link d'abord."
            ),
        );
    };
    let b: Heartbeat = serde_json::from_value(req.body.clone()).unwrap_or_default();
    let now = unix_now();
    let mut s = p.lock();
    s.update_wsl(b.measures, now);
    // Waiting tickets count only real waits: a live session does not keep its place without listening.
    for t in s.state.tickets.iter_mut().filter(|t| t.link == Some(l) && t.state != TicketState::Waiting) {
        t.presence = now;
    }
    let candidate = s.state.clone();
    let _ = s.commit(candidate, false, None, None);
    let rep = response(200, json!({ "link": l, "epoch": s.state.epoch }));
    p.publish(s);
    rep
}

impl Shared {
    fn wake(&self) {
        let _s = self.lock();
        self.signal.notify_all();
    }

    /// A link's connection closed: its tickets wait for the same session to resume them during the resume delay.
    fn link_closed(&self, l: u64) {
        let now = unix_now();
        let mut s = self.lock();
        let session = s.links.remove(&l).map(|v| v.session).unwrap_or_default();
        let mut candidate = s.state.clone();
        let mut kept = Vec::new();
        for t in candidate.tickets.iter_mut().filter(|t| t.link == Some(l) && t.lost.is_none()) {
            t.lost = Some(now);
            kept.push(format!("#{}", t.id));
        }
        let rest = if kept.is_empty() {
            clearway::tr!("no ticket", "aucun ticket")
        } else {
            clearway::tr!(
                "{} kept until {} for a resume",
                "{} gardé(s) jusqu'à {} pour une reprise",
                kept.join(", "),
                hhmm(now + s.config.resume_delay_s)
            )
        };
        let prelude = format!("LINK #{l} {} [{session}] · {rest}", clearway::tr!("closed", "fermée"));
        let _ = s.commit(candidate, !kept.is_empty(), None, Some(prelude));
        self.publish(s);
    }
}

/// Writes one response line, in the French dialect when the request came from a pre-Clearway client.
fn send(writer: &Mutex<Box<dyn Write + Send>>, rep: &Response, legacy: bool) -> bool {
    let value = serde_json::to_value(rep).unwrap_or_else(|_| serde_json::json!({ "code": 500 }));
    let value = if legacy { clearway::compat::response_to_legacy(value) } else { value };
    let mut text = value.to_string();
    text.push('\n');
    let mut e = writer.lock().unwrap_or_else(|e| e.into_inner());
    e.write_all(text.as_bytes()).and_then(|_| e.flush()).is_ok()
}

/// Serves a connection until the end of the stream: one JSON request per line, one response per line. Requests with
/// an `id` each run on their own thread and may answer out of order; the others run one after the other.
pub fn serve_connection(p: &Arc<Shared>, read_half: impl Read, writer: Box<dyn Write + Send>, ctx: Arc<Context>) {
    let writer = Arc::new(Mutex::new(writer));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let mut reader = BufReader::new(read_half);
    while let Ok(Some(line)) = read_line_limited(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<Value>(&line).map(|v| {
            let legacy = clearway::compat::is_legacy_request(&v);
            (if legacy { clearway::compat::request_from_legacy(v) } else { v }, legacy)
        });
        let (req, legacy) = match parsed.and_then(|(v, legacy)| serde_json::from_value::<Request>(v).map(|r| (r, legacy))) {
            Ok(r) => r,
            Err(e) => {
                if !send(&writer, &error(400, &clearway::tr!("unreadable request: {e}", "requête illisible : {e}")), false) {
                    break;
                }
                continue;
            }
        };
        let Some(id) = req.id else {
            if !send(&writer, &handle(p, &req, &ctx), legacy) {
                break;
            }
            continue;
        };
        if in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT_PER_CONNECTION {
            in_flight.fetch_sub(1, Ordering::SeqCst);
            let mut rep = error(
                429,
                &clearway::tr!(
                    "too many requests in flight on this connection",
                    "trop de requêtes en cours sur cette connexion"
                ),
            );
            rep.id = Some(id);
            send(&writer, &rep, legacy);
            continue;
        }
        let (p2, ctx2, writer2, in_flight2) = (p.clone(), ctx.clone(), writer.clone(), in_flight.clone());
        let launched = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            let mut rep = handle(&p2, &req, &ctx2);
            rep.id = Some(id);
            send(&writer2, &rep, legacy);
            in_flight2.fetch_sub(1, Ordering::SeqCst);
        });
        if launched.is_err() {
            in_flight.fetch_sub(1, Ordering::SeqCst);
            let mut rep = error(
                503,
                &clearway::tr!("could not start a handler thread", "fil de traitement impossible à lancer"),
            );
            rep.id = Some(id);
            send(&writer, &rep, legacy);
        }
    }
    ctx.closed.store(true, Ordering::SeqCst);
    p.wake();
    if let Some(l) = ctx.link() {
        p.link_closed(l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_800_000_000;

    #[test]
    fn oscillating_pressure_gives_no_balloon() {
        let mut b = Balloons::default();
        for cycle in 0..20 {
            let began = T + cycle * 90;
            for k in 0..30 {
                assert!(!b.pressure("pressure ram_windows", true, true, began + k * 2));
            }
            assert!(!b.pressure("pressure ram_windows", false, true, began + 60));
        }
    }

    #[test]
    fn pressure_without_queue_logs_only() {
        let mut b = Balloons::default();
        for k in 0..300 {
            assert!(!b.pressure("pressure ram_windows", true, false, T + k * 2));
        }
    }

    #[test]
    fn lasting_pressure_with_queue_one_balloon_per_half_hour() {
        let mut b = Balloons::default();
        let mut balloons = Vec::new();
        for k in 0..(3600 / 2) {
            let t = T + k * 2;
            if b.pressure("pressure ram_windows", true, true, t) {
                balloons.push(t - T);
            }
        }
        assert_eq!(balloons, vec![120, 120 + 1800]);
    }

    #[test]
    fn swap_out_rate_over_30_s() {
        let mut h = VecDeque::new();
        assert_eq!(swap_out_rate_of(&mut h, T, 1000), 0.0);
        assert_eq!(swap_out_rate_of(&mut h, T + 10, 1000 + 3000), 300.0);
        for k in 1..=20 {
            swap_out_rate_of(&mut h, T + 10 + k * 2, 4000);
        }
        // 30 s later with no new swap-out: the rate falls back to 0.
        assert_eq!(swap_out_rate_of(&mut h, T + 52, 4000), 0.0);
        // Counters reset (WSL restarted): no negative rate.
        assert_eq!(swap_out_rate_of(&mut h, T + 54, 10), 0.0);

        // Old client (pswpout missing, hence 0) mixed with a new one: no false spike.
        let mut h = VecDeque::new();
        assert_eq!(swap_out_rate_of(&mut h, T, 0), 0.0);
        assert_eq!(swap_out_rate_of(&mut h, T + 2, 4_200_000), 0.0);
        assert_eq!(swap_out_rate_of(&mut h, T + 4, 0), 0.0);
        assert_eq!(swap_out_rate_of(&mut h, T + 6, 4_200_040), 10.0);
    }

    #[test]
    fn one_balloon_per_kind_every_30_min() {
        let mut b = Balloons::default();
        assert!(b.allow("orphaned", T));
        assert!(!b.allow("orphaned", T + 60));
        assert!(b.allow("refused", T + 60), "kinds are independent");
        assert!(!b.allow("orphaned", T + 1799));
        assert!(b.allow("orphaned", T + 1800));
    }
}
