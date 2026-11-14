use std::collections::BTreeMap;

use crate::config::Config;
use crate::format::{cores, duration, gb, ram_option};
use crate::i18n::Lang;
use crate::model::{
    DecisionKind, Hysteresis, Measures, Need, OrphanCause, Reason, ReserveView, Side, State, Ticket, TicketState, WslMeasures,
};
use crate::system::hhmm;
use crate::tr;

pub const WSL_FRESHNESS_S: u64 = 30;
pub const SWAP_CALM_S: u64 = 60;
/// Cores an active ticket keeps reserved however idle it is measured.
pub const CPU_FLOOR: i64 = 1;

/// Full but calm swap: worth reporting (status, tooltip) without freezing.
pub fn calm_swap(e: &State, m: &Measures, c: &Config, now: u64) -> Option<String> {
    let w = fresh_wsl(m, now)?;
    (!e.swap_pressure && w.swap_pct() >= c.swap_threshold_pct)
        .then(|| tr!("WSL swap {}% full (calm)", "swap WSL plein à {} % (calme)", w.swap_pct()))
}

/// Measured use of a CPU reserve in percent of its budget; `None` without a fresh measure.
pub fn cpu_used_pct(r: Reserve, m: &Measures, c: &Config, now: u64) -> Option<f64> {
    match r {
        // The load average also counts tasks blocked on I/O (git on a /mnt drive): measured busy cores win.
        Reserve::Cpu => fresh_wsl(m, now).map(|w| w.busy_cores.unwrap_or(w.load1) as f64 * 100.0 / c.cpu_total.max(1) as f64),
        Reserve::CpuWindows => (m.cpu_pct > 0.0 || m.windows_cores > 0).then_some(m.cpu_pct as f64),
        _ => None,
    }
}

/// Saturation must last `cpu.pressure_delay` to be declared, and calm must last as long to be lifted.
fn update_cpu_pressure(e: &mut State, m: &Measures, c: &Config, now: u64) {
    for (r, h) in [(Reserve::CpuWindows, &mut e.cpu_pressure.windows), (Reserve::Cpu, &mut e.cpu_pressure.wsl)] {
        let Some(used) = cpu_used_pct(r, m, c, now) else {
            *h = Hysteresis::default();
            continue;
        };
        let crossing = if h.on { used < c.cpu_release_pct as f64 } else { used >= c.cpu_pressure_pct as f64 };
        if !crossing {
            h.since = None;
        } else if now.saturating_sub(*h.since.get_or_insert(now)) >= c.cpu_pressure_delay_s {
            h.on = !h.on;
            h.declared_at = h.on.then(|| h.since.unwrap_or(now));
            h.since = None;
        }
    }
}

/// A full swap alone says nothing (old pages): it must really be paging out, or memory must be short.
fn update_swap_pressure(e: &mut State, m: &Measures, c: &Config, now: u64) {
    let Some(w) = fresh_wsl(m, now) else {
        e.swap_pressure = false;
        e.swap_calm_since = None;
        return;
    };
    let active = m.swap_out_ps > c.swap_out_rate as f32
        || w.mem_avail_mb < c.margin_ram_wsl_mb + 1024;
    let pct = w.swap_pct();
    if !e.swap_pressure {
        e.swap_pressure = pct >= c.swap_threshold_pct && active;
        e.swap_calm_since = None;
        return;
    }
    if pct < c.swap_release_pct {
        e.swap_pressure = false;
        e.swap_calm_since = None;
    } else if active {
        e.swap_calm_since = None;
    } else {
        let since = *e.swap_calm_since.get_or_insert(now);
        if now.saturating_sub(since) >= SWAP_CALM_S {
            e.swap_pressure = false;
            e.swap_calm_since = None;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Grouped by side: Windows first, then WSL. The order is the display order everywhere.
pub enum Reserve {
    RamWindows,
    CpuWindows,
    RamWsl,
    Cpu,
}

pub const RESERVES: [Reserve; 4] = [Reserve::RamWindows, Reserve::CpuWindows, Reserve::RamWsl, Reserve::Cpu];
pub const RESERVE_COUNT: usize = RESERVES.len();

impl Reserve {
    pub fn key(self) -> &'static str {
        match self {
            Reserve::RamWindows => "ram_windows",
            Reserve::RamWsl => "ram_wsl",
            Reserve::Cpu => "cpu",
            Reserve::CpuWindows => "cpu_windows",
        }
    }

    pub fn name(self) -> &'static str {
        match (self, crate::i18n::current()) {
            (Reserve::RamWindows, Lang::En) => "Windows RAM",
            (Reserve::RamWsl, Lang::En) => "WSL RAM",
            (Reserve::Cpu, Lang::En) => "WSL CPU",
            (Reserve::CpuWindows, Lang::En) => "Windows CPU",
            (Reserve::RamWindows, Lang::Fr) => "RAM Windows",
            (Reserve::RamWsl, Lang::Fr) => "RAM WSL",
            (Reserve::Cpu, Lang::Fr) => "CPU WSL",
            (Reserve::CpuWindows, Lang::Fr) => "CPU Windows",
        }
    }

    fn insufficient(self) -> String {
        match self {
            Reserve::Cpu | Reserve::CpuWindows => tr!("insufficient", "insuffisant"),
            _ => tr!("insufficient", "insuffisante"),
        }
    }

    fn i(self) -> usize {
        self as usize
    }

    pub fn is_ram(self) -> bool {
        matches!(self, Reserve::RamWindows | Reserve::RamWsl)
    }

    /// Every task debits Windows RAM and CPU (the VM's vCPUs run on the host); only `wsl` tasks debit WSL RAM and CPU.
    pub fn need(self, side: Side, b: &Need) -> i64 {
        match self {
            Reserve::RamWindows => b.ram_mb as i64,
            Reserve::RamWsl if side == Side::Wsl => b.ram_mb as i64,
            Reserve::Cpu if side == Side::Wsl => b.cpu as i64,
            Reserve::RamWsl | Reserve::Cpu => 0,
            Reserve::CpuWindows => b.cpu as i64,
        }
    }

    pub fn amount(self, v: i64) -> String {
        if self.is_ram() { gb(v) } else { cores(v) }
    }
}

/// What a ticket really takes from a reserve. A WSL task takes from Windows only the VM's remaining growth up to its
/// cap: beyond that, it uses memory already counted in `vmmemWSL`.
pub fn debit(r: Reserve, side: Side, b: &Need, m: &Measures, c: &Config) -> i64 {
    let need = r.need(side, b);
    if r == Reserve::RamWindows && side == Side::Wsl {
        let growth = c.wsl_ram_max_mb.saturating_sub(m.vmmem.map_or(0, |v| v.rss_mb)) as i64;
        need.min(growth)
    } else {
        need
    }
}

/// Debits of a waiting ticket: on Windows RAM, a WSL ticket costs what it adds to the VM's growth, see [`WslShare`].
pub fn needs_of(t: &Ticket, lev: &[Level; RESERVE_COUNT], m: &Measures, c: &Config) -> [i64; RESERVE_COUNT] {
    RESERVES.map(|r| {
        if r == Reserve::RamWindows && t.side == Side::Wsl {
            let w = &lev[r.i()].wsl;
            w.cost(w.pending + t.need.ram_mb as i64) - w.cost(w.pending)
        } else {
            debit(r, t.side, &t.need, m, c)
        }
    })
}

/// Windows RAM taken by the WSL tickets together: what they will still consume in WSL comes first out of the memory
/// vmmemWSL holds unused, and the VM can only grow up to its cap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WslShare {
    /// What the non-waiting WSL tickets will still consume in WSL.
    pub pending: i64,
    /// [`wsl_reusable`].
    pub pool: i64,
    /// What vmmemWSL can still grow: cap − its size.
    pub growth: i64,
}

impl WslShare {
    pub fn cost(&self, pending: i64) -> i64 {
        self.growth.min((pending - self.pool).max(0))
    }

    /// Largest WSL need whose Windows cost fits in `free`.
    pub fn max_need(&self, free: i64) -> i64 {
        let room = self.cost(self.pending) + free;
        if room >= self.growth { i64::MAX } else { room + self.pool - self.pending }
    }
}

/// Memory held by `vmmemWSL` that WSL does not use (page cache, freed pages): a WSL task takes it before the VM grows.
/// Zero without a fresh WSL measure.
pub fn wsl_reusable(m: &Measures, now: u64) -> i64 {
    match (fresh_wsl(m, now), m.vmmem) {
        (Some(w), Some(v)) => (v.rss_mb as i64 - w.mem_total_mb.saturating_sub(w.mem_avail_mb) as i64).max(0),
        _ => 0,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Level {
    pub available: i64,
    pub reserve: i64,
    pub holders: Vec<u64>,
    pub margin: i64,
    /// Windows RAM only.
    pub wsl: WslShare,
}

impl Level {
    pub fn free(&self) -> i64 {
        self.available - self.reserve - self.margin
    }
}

pub fn fresh_wsl(m: &Measures, now: u64) -> Option<&WslMeasures> {
    m.wsl
        .as_ref()
        .filter(|w| now.saturating_sub(w.at) <= WSL_FRESHNESS_S)
}

/// Live measure of a reserve, without reservations.
pub fn available(r: Reserve, m: &Measures, c: &Config, now: u64) -> i64 {
    match r {
        Reserve::RamWindows => m.ram_avail_mb as i64,
        // vmmemWSL keeps the reclaimable page cache: a recent MemAvailable wins, 12 GB − vmmemWSL is only a fallback.
        // The VM's growth stays counted on the Windows side through the measured Windows RAM.
        Reserve::RamWsl => match (fresh_wsl(m, now), m.vmmem) {
            (Some(w), _) => w.mem_avail_mb as i64,
            (None, Some(v)) => c.wsl_ram_max_mb.saturating_sub(v.rss_mb) as i64,
            (None, None) => c.wsl_ram_max_mb as i64,
        },
        Reserve::Cpu => c.cpu_total as i64,
        Reserve::CpuWindows => windows_cores(m, c),
    }
}

/// Windows CPU budget: `cpu.total_windows` if set, else the measured logical cores.
pub fn windows_cores(m: &Measures, c: &Config) -> i64 {
    c.cpu_total_windows.map(i64::from).unwrap_or(if m.windows_cores > 0 { m.windows_cores as i64 } else { 16 })
}

fn margin(r: Reserve, c: &Config) -> i64 {
    match r {
        Reserve::RamWindows => c.margin_ram_windows_mb as i64,
        Reserve::RamWsl => c.margin_ram_wsl_mb as i64,
        Reserve::Cpu | Reserve::CpuWindows => c.margin_cpu as i64,
    }
}

fn ramping_up(t: &Ticket, c: &Config, now: u64) -> bool {
    t.granted.is_none_or(|a| now.saturating_sub(a) < c.rampup_delay_s)
}

/// Cores measured busy on a CPU reserve; `None` without a fresh measure.
fn cpu_used_cores(r: Reserve, m: &Measures, c: &Config, now: u64) -> Option<f64> {
    let pct = cpu_used_pct(r, m, c, now)?;
    Some(pct * total(r, m, c) as f64 / 100.0)
}

/// What a ticket still reserves on a reserve: its whole debit during the ramp-up after clearance, then only what it
/// has not yet raised (the rest is already in the measures). An orphan keeps its whole debit.
/// CPU after the ramp-up: the measured load shared between the settled tickets in proportion to their declaration,
/// within [`CPU_FLOOR`, declared].
pub fn reservation(e: &State, t: &Ticket, r: Reserve, m: &Measures, c: &Config, now: u64) -> i64 {
    reservation_of(e, t, r, debit(r, t.side, &t.need, m, c), m, c, now)
}

/// [`reservation`] of the debit `b`.
fn reservation_of(e: &State, t: &Ticket, r: Reserve, b: i64, m: &Measures, c: &Config, now: u64) -> i64 {
    if b == 0|| t.state != TicketState::Active || ramping_up(t, c, now) {
        return b;
    }
    if !r.is_ram() {
        let Some(used) = cpu_used_cores(r, m, c, now) else {
            return b;
        };
        let declared: i64 = e
            .tickets
            .iter()
            .filter(|a| a.state == TicketState::Active && !ramping_up(a, c, now))
            .map(|a| debit(r, a.side, &a.need, m, c))
            .sum();
        let share = (used * b as f64 / declared.max(b) as f64).ceil() as i64;
        return share.max(CPU_FLOOR).min(b);
    }
    // Tickets granted before these baselines existed: only the available RAM of their main reserve was recorded.
    let main_reserve = t.avail_at_grant_mb.map(|d| d as i64);
    let base_wsl = t.base_ram_wsl_mb.or(main_reserve.filter(|_| t.side == Side::Wsl));
    let base_windows = t.base_ram_windows_mb.or(main_reserve.filter(|_| t.side == Side::Windows));
    let wsl_drop = base_wsl.map(|base| base - available(Reserve::RamWsl, m, c, now));
    let risen = match (r, t.side) {
        // Consumed inside WSL counts as well: part of it came from memory vmmemWSL already held.
        (Reserve::RamWindows, Side::Wsl) => {
            let vm_rise = t.base_vmmem_mb.zip(m.vmmem).map(|(base, v)| v.rss_mb.saturating_sub(base) as i64);
            match (vm_rise, wsl_drop) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            }
        }
        (Reserve::RamWindows, _) => base_windows.map(|base| base - m.ram_avail_mb as i64),
        (Reserve::RamWsl, _) => wsl_drop,
        _ => None,
    };
    match risen {
        Some(risen) => (b - risen.clamp(0, b)).max(0),
        None => b,
    }
}

pub fn levels(e: &State, m: &Measures, c: &Config, now: u64) -> [Level; RESERVE_COUNT] {
    RESERVES.map(|r| {
        let mut n = Level { available: available(r, m, c, now), margin: margin(r, c), ..Level::default() };
        let shared = r == Reserve::RamWindows;
        let mut wsl_holders = Vec::new();
        for t in e.tickets.iter().filter(|t| t.state != TicketState::Waiting) {
            if shared && t.side == Side::Wsl {
                let p = reservation_of(e, t, r, t.need.ram_mb as i64, m, c, now);
                n.wsl.pending += p;
                if p > 0 {
                    wsl_holders.push(t.id);
                }
                continue;
            }
            let b = reservation(e, t, r, m, c, now);
            if b > 0 {
                n.reserve += b;
                n.holders.push(t.id);
            }
        }
        if shared {
            n.wsl.pool = wsl_reusable(m, now);
            n.wsl.growth = c.wsl_ram_max_mb.saturating_sub(m.vmmem.map_or(0, |v| v.rss_mb)) as i64;
            let cost = n.wsl.cost(n.wsl.pending);
            if cost > 0 {
                n.reserve += cost;
                n.holders.extend(wsl_holders);
            }
        }
        n
    })
}

pub fn reserve_views(e: &State, m: &Measures, c: &Config, now: u64) -> Vec<ReserveView> {
    let lev = levels(e, m, c, now);
    RESERVES
        .iter()
        .map(|&r| {
            let n = &lev[r.i()];
            ReserveView {
                key: r.key().into(),
                name: r.name().into(),
                total: total(r, m, c),
                available: n.available,
                reserve: n.reserve,
                margin: n.margin,
                free: n.free(),
                used_pct: cpu_used_pct(r, m, c, now).map(|p| p.round() as u32),
                exclusive: (r == Reserve::Cpu)
                    .then(|| {
                        e.tickets.iter().find(|t| {
                            t.state != TicketState::Waiting
                                && t.side == Side::Wsl
                                && t.need.cpu as i64 > c.cpu_total as i64 - c.margin_cpu as i64
                        })
                    })
                    .flatten()
                    .map(|t| t.id),
                estimate: (r == Reserve::RamWsl && m.vmmem.is_some() && fresh_wsl(m, now).is_none())
                    .then(|| m.wsl.map_or(0, |w| w.at)),
            }
        })
        .collect()
}

fn total(r: Reserve, m: &Measures, c: &Config) -> i64 {
    match r {
        Reserve::RamWindows => m.ram_total_mb as i64,
        Reserve::RamWsl => c.wsl_ram_max_mb as i64,
        Reserve::Cpu => c.cpu_total as i64,
        Reserve::CpuWindows => windows_cores(m, c),
    }
}

/// Refusal at creation of a need that no amount of waiting could satisfy.
pub fn refusal(side: Side, b: &Need, m: &Measures, c: &Config) -> Option<Reason> {
    for r in RESERVES {
        let need = r.need(side, b);
        let tot = total(r, m, c);
        if need == 0 || tot == 0 {
            continue;
        }
        // Beyond capacity − margin, a CPU request is still admitted, but alone (see `exclusive`).
        let capacity = if r.is_ram() { tot - margin(r, c) } else { tot };
        if need > capacity {
            let suggestion = if r.is_ram() {
                tr!("Lower --ram to {} max.", "Baisse --ram à {} max.", ram_option(capacity))
            } else {
                tr!("Lower --cpu to {} max.", "Baisse --cpu à {} max.", capacity.max(1))
            };
            let mut reason = Reason::simple(
                DecisionKind::Refused,
                if r.is_ram() {
                    tr!(
                        "Refused: {} cannot fit (need {}, maximum capacity {} = {} − margin {}). {suggestion}",
                        "Refusé : {} impossible (besoin {}, capacité maximale {} = {} − marge {}). {suggestion}",
                        r.name(),
                        r.amount(need),
                        r.amount(capacity),
                        r.amount(tot),
                        r.amount(margin(r, c))
                    )
                } else {
                    tr!(
                        "Refused: CPU cannot fit (need {}, the machine has {}). {suggestion}",
                        "Refusé : CPU impossible (besoin {}, la machine compte {}). {suggestion}",
                        r.amount(need),
                        r.amount(tot)
                    )
                },
            );
            reason.reserve = Some(r.key().into());
            reason.need = Some(need);
            reason.available = Some(capacity);
            reason.margin = Some(margin(r, c));
            return Some(reason);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeReason {
    WslStopped,
    SessionClosed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Cleared(u64),
    Expired(u64),
    Orphaned(u64),
    LeaseOverrun(u64),
    Lease(u64),
    Recalled(u64),
    Freed(u64, FreeReason),
}

#[derive(Clone, Debug)]
struct Frozen {
    cause: String,
    lifting: String,
    orphan: Option<u64>,
}

pub struct Plan {
    pub state: State,
    pub events: Vec<Event>,
    pub persist: bool,
    pub reasons: BTreeMap<u64, Reason>,
    pub freeze_texts: Vec<String>,
    pub pressure: Vec<Reserve>,
}

enum Block {
    Nothing,
    ManualFreeze,
    Frozen(Reserve),
    Exclusive(Option<u64>),
    Missing(Vec<Reserve>),
}

/// Blocked head of the queue: what it lacks, its estimated start T and the tickets that overtook it.
struct Head {
    id: u64,
    missing: Vec<Reserve>,
    start_at: Option<u64>,
    allowed: bool,
    overtakes: u32,
    created: u64,
    passes: Vec<u64>,
    unrelated_passes: Vec<u64>,
}

fn with_article(r: Reserve) -> String {
    let article = if r.is_ram() { tr!("the", "la") } else { tr!("the", "le") };
    format!("{article} {}", r.name())
}

/// "the WSL RAM", "the WSL RAM and the WSL CPU".
fn enumeration(reserves: &[Reserve]) -> String {
    let n: Vec<String> = reserves.iter().map(|&r| with_article(r)).collect();
    match n.len() {
        0 => String::new(),
        1 => n[0].clone(),
        k => tr!("{} and {}", "{} et {}", n[..k - 1].join(", "), n[k - 1]),
    }
}

fn names(reserves: &[Reserve]) -> String {
    reserves.iter().map(|r| r.name()).collect::<Vec<_>>().join(", ")
}

/// Makes a ticket active and debits its reserves; returns what was free at the time of the grant.
fn grant(
    e: &mut State,
    lev: &mut [Level; RESERVE_COUNT],
    t: &Ticket,
    needs: &[i64; RESERVE_COUNT],
    m: &Measures,
    now: u64,
) -> String {
    let free_now: Vec<String> = RESERVES
        .iter()
        .filter(|r| needs[r.i()] > 0)
        .map(|&r| format!("{} {}", r.name(), r.amount(lev[r.i()].free())))
        .collect();
    if t.side == Side::Wsl {
        lev[Reserve::RamWindows.i()].wsl.pending += t.need.ram_mb as i64;
    }
    let main_reserve = if t.side == Side::Wsl { Reserve::RamWsl } else { Reserve::RamWindows };
    let main_avail = lev[main_reserve.i()].available.max(0) as u64;
    for r in RESERVES {
        if needs[r.i()] > 0 {
            lev[r.i()].reserve += needs[r.i()];
            lev[r.i()].holders.push(t.id);
        }
    }
    let tm = e.ticket_mut(t.id).expect("waiting ticket present");
    tm.state = TicketState::Active;
    tm.granted = Some(now);
    tm.presence = now;
    tm.avail_at_grant_mb = Some(main_avail);
    tm.base_ram_windows_mb = Some(lev[Reserve::RamWindows.i()].available);
    tm.base_ram_wsl_mb = Some(lev[Reserve::RamWsl.i()].available);
    tm.base_vmmem_mb = m.vmmem.map(|v| v.rss_mb);
    tm.acked = false;
    tm.lease_overrun = false;
    tm.orphan_cause = None;
    free_now.join(", ")
}

fn list(ids: &[u64]) -> String {
    let names: Vec<String> = ids.iter().map(|i| format!("#{i}")).collect();
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        n => tr!("{} and {}", "{} et {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

fn orphan_lifting(t: &Ticket) -> String {
    let id = t.id;
    match t.orphan_cause {
        Some(OrphanCause::Connection) => tr!(
            "resume by the same session, `clearway release {id}`, `clearway cancel {id}` or \"Release\" in the window",
            "reprise par la même session, « clearway release {id} », « clearway cancel {id} » ou « Libérer » dans la fenêtre"
        ),
        Some(OrphanCause::Lease) => tr!(
            "`clearway release {id}`, `clearway extend {id} <duration> \"<reason>\"`, `clearway cancel {id}` or \"Release\" in the window",
            "« clearway release {id} », « clearway extend {id} <durée> \"<raison>\" », « clearway cancel {id} » ou « Libérer » dans la fenêtre"
        ),
        _ => tr!(
            "signal from #{id}, `clearway release {id}`, `clearway cancel {id}` or \"Release\" in the window",
            "signal du #{id}, « clearway release {id} », « clearway cancel {id} » ou « Libérer » dans la fenêtre"
        ),
    }
}

fn orphan_cause(t: &Ticket, now: u64) -> String {
    match t.orphan_cause {
        Some(OrphanCause::Connection) => tr!(
            "link to its session lost for {} (session closed or MCP server restarted)",
            "liaison de sa session perdue depuis {} (session fermée ou serveur MCP relancé)",
            duration(now.saturating_sub(t.lost.unwrap_or(now)))
        ),
        Some(OrphanCause::Lease) => tr!(
            "lease expired {} ago (declared duration of {} exceeded), no news from the session for {}",
            "bail échu depuis {} (durée déclarée de {} dépassée), sans nouvelle de la session depuis {}",
            duration(t.planned_end().map_or(0, |f| now.saturating_sub(f))),
            duration(t.need.duration_s),
            duration(now.saturating_sub(last_news(t)))
        ),
        _ => tr!("no signal for {}", "sans signal depuis {}", duration(now.saturating_sub(t.presence))),
    }
}

/// Last sign of life of a manual ticket past its lease: the deadline itself, or a more recent signal.
fn last_news(t: &Ticket) -> u64 {
    t.planned_end().unwrap_or(0).max(t.presence)
}

/// Since when a waiting ticket has had no client: last presence or clearance recall.
fn seen(t: &Ticket) -> u64 {
    t.presence.max(t.recalled.unwrap_or(0))
}

/// Decides grants, recalls, expirations, orphans and releases, and explains each ticket's situation.
pub fn schedule(state: &State, m: &Measures, c: &Config, now: u64) -> Plan {
    let mut e = state.clone();
    let mut events = Vec::new();

    let pid = m.vmmem.map(|v| v.pid);
    if e.vmmem_pid.is_some() && e.vmmem_pid != pid {
        for t in e.tickets.iter().filter(|t| t.side == Side::Wsl) {
            events.push(Event::Freed(t.id, FreeReason::WslStopped));
        }
        e.tickets.retain(|t| t.side != Side::Wsl);
    }
    let mut persist = e.vmmem_pid != pid;
    e.vmmem_pid = pid;

    for t in &mut e.tickets {
        if let Some(since) = t.lost {
            if now.saturating_sub(since) >= c.resume_delay_s {
                events.push(Event::Freed(t.id, FreeReason::SessionClosed));
            } else if t.state == TicketState::Active {
                t.state = TicketState::Orphaned;
                t.orphan_cause = Some(OrphanCause::Connection);
            }
            continue;
        }
        let silence = now.saturating_sub(seen(t));
        let recall_due = !t.acked && t.granted.is_some_and(|a| now.saturating_sub(a) >= c.recall_delay_s);
        match t.state {
            TicketState::Waiting if t.listeners == 0 && silence > c.absence_delay_s => {
                events.push(Event::Expired(t.id));
            }
            TicketState::Active if recall_due => {
                t.state = TicketState::Waiting;
                t.granted = None;
                t.avail_at_grant_mb = None;
                t.recalled = Some(now);
                events.push(Event::Recalled(t.id));
            }
            TicketState::Active if t.manual && t.planned_end().is_some_and(|f| now >= f) => {
                if !t.lease_overrun {
                    t.lease_overrun = true;
                    events.push(Event::LeaseOverrun(t.id));
                }
                if now.saturating_sub(last_news(t)) >= c.absence_delay_s {
                    t.state = TicketState::Orphaned;
                    t.orphan_cause = Some(OrphanCause::Lease);
                    events.push(Event::Lease(t.id));
                }
            }
            TicketState::Active if !t.manual && t.acked && silence > c.silence_delay_s => {
                t.state = TicketState::Orphaned;
                t.orphan_cause = Some(OrphanCause::Silence);
                events.push(Event::Orphaned(t.id));
            }
            _ => {}
        }
    }
    e.tickets.retain(|t| {
        !events.contains(&Event::Expired(t.id))
            && !events.contains(&Event::Freed(t.id, FreeReason::SessionClosed))
    });

    update_swap_pressure(&mut e, m, c, now);
    update_cpu_pressure(&mut e, m, c, now);
    let mut lev = levels(&e, m, c, now);
    let mut freezes: [Option<Frozen>; RESERVE_COUNT] = [None, None, None, None];
    let mut pressure = Vec::new();
    for r in [Reserve::RamWindows, Reserve::RamWsl] {
        // The 12 GB − vmmemWSL fallback counts the page cache: not enough to declare pressure.
        if r == Reserve::RamWsl && fresh_wsl(m, now).is_none() {
            continue;
        }
        let n = &lev[r.i()];
        if let Some(w) = fresh_wsl(m, now).filter(|_| r == Reserve::RamWsl && e.swap_pressure) {
            pressure.push(r);
            freezes[r.i()] = Some(Frozen {
                cause: tr!(
                    "pressure: WSL swap {}% full and active (out {:.0} pages/s, MemAvailable {})",
                    "pression : swap WSL plein à {} % et actif (sortie {:.0} pages/s, MemAvailable {})",
                    w.swap_pct(),
                    m.swap_out_ps,
                    gb(w.mem_avail_mb as i64)
                ),
                lifting: tr!(
                    "WSL swap below {}% or calm for 1 min",
                    "swap WSL sous {} % ou calme pendant 1 min",
                    c.swap_release_pct
                ),
                orphan: None,
            });
        } else if n.available < n.margin {
            pressure.push(r);
            freezes[r.i()] = Some(Frozen {
                cause: tr!(
                    "pressure: {} available {} below the margin of {}",
                    "pression : {} disponible {} sous la marge de {}",
                    r.name(),
                    gb(n.available),
                    gb(n.margin)
                ),
                lifting: tr!("{} back above {}", "{} de nouveau au-dessus de {}", r.name(), gb(n.margin)),
                orphan: None,
            });
        }
    }
    for (r, h) in [(Reserve::CpuWindows, e.cpu_pressure.windows), (Reserve::Cpu, e.cpu_pressure.wsl)] {
        if !h.on {
            continue;
        }
        let used = cpu_used_pct(r, m, c, now).unwrap_or(0.0);
        let measured = if r == Reserve::Cpu && fresh_wsl(m, now).is_some_and(|w| w.busy_cores.is_none()) {
            let load = format!("{:.1}", fresh_wsl(m, now).map_or(0.0, |w| w.load1));
            let load = if crate::i18n::current() == crate::i18n::Lang::Fr { load.replace('.', ",") } else { load };
            tr!("load {load} / {}", "load {load} / {}", cores(c.cpu_total as i64))
        } else {
            format!("{used:.0} %")
        };
        let since = duration(now.saturating_sub(h.declared_at.unwrap_or(now)));
        pressure.push(r);
        freezes[r.i()] = Some(Frozen {
            cause: tr!("{} saturated ({measured}) for {since}", "{} saturé ({measured}) depuis {since}", r.name()),
            lifting: tr!(
                "{} under {} % for {}",
                "{} sous {} % pendant {}",
                r.name(),
                c.cpu_release_pct,
                duration(c.cpu_pressure_delay_s)
            ),
            orphan: None,
        });
    }
    // A lost link is not an unattended command: reservation kept, without freezing, for the resume delay.
    for t in e
        .tickets
        .iter()
        .filter(|t| t.state == TicketState::Orphaned && t.orphan_cause != Some(OrphanCause::Connection))
    {
        for r in RESERVES {
            if r.need(t.side, &t.need) > 0 && freezes[r.i()].is_none() {
                freezes[r.i()] = Some(Frozen {
                    cause: tr!("ticket #{} orphaned ({})", "ticket #{} orphelin ({})", t.id, orphan_cause(t, now)),
                    lifting: orphan_lifting(t),
                    orphan: Some(t.id),
                });
            }
        }
    }
    let manual_freeze = e.manual_freeze.map(|since| Frozen {
        cause: tr!("manual freeze since {}", "gel manuel depuis {}", hhmm(since)),
        lifting: tr!(
            "\"Unfreeze the queue\" in the window or the tray icon menu",
            "« Dégeler la file » dans la fenêtre ou le menu de l'icône"
        ),
        orphan: None,
    });

    let mut reasons = BTreeMap::new();
    let waiting_ids: Vec<u64> = e.tickets.iter().filter(|t| t.state == TicketState::Waiting).map(|t| t.id).collect();
    let mut head: Option<Head> = None;
    let mut position = 0;
    for id in waiting_ids {
        let t = e.ticket(id).expect("waiting ticket present").clone();
        let silence = now.saturating_sub(seen(&t));
        if t.lost.is_some() || t.recalled.is_some() || (t.listeners == 0 && silence > c.unattended_delay_s) {
            position += 1;
            let why = match (t.lost, t.recalled) {
                (Some(p), _) => tr!(
                    "link to its session lost at {}, waiting for a resume",
                    "liaison de sa session perdue à {}, en attente de reprise",
                    hhmm(p)
                ),
                (None, Some(r)) => tr!(
                    "clearance recalled at {} because it went unused",
                    "feu vert rappelé à {} faute d'avoir été utilisé",
                    hhmm(r)
                ),
                (None, None) => tr!(
                    "no client has been waiting for it for {}",
                    "aucun client ne l'attend depuis {}",
                    duration(silence)
                ),
            };
            let mut r = Reason::simple(
                DecisionKind::Waiting,
                tr!(
                    "#{id} waiting, position {position}: {why}; place kept until {}, the next ones go ahead. Resume with `clearway wait {id}`.",
                    "#{id} en attente, position {position} : {why} ; place gardée jusqu'à {}, les suivants passent. Relance « clearway wait {id} ».",
                    hhmm(t.lost.map_or(seen(&t) + c.absence_delay_s, |p| p + c.resume_delay_s))
                ),
            );
            r.position = Some(position);
            r.reserve = Some("absent".into());
            reasons.insert(id, r);
            continue;
        }
        let needs = needs_of(&t, &lev, m, c);
        let exclusive = needs[Reserve::Cpu.i()] > c.cpu_total as i64 - c.margin_cpu as i64;
        let other_wsl: Vec<&Ticket> =
            e.tickets.iter().filter(|a| a.state != TicketState::Waiting && a.side == Side::Wsl).collect();
        let block = if manual_freeze.is_some() {
            Block::ManualFreeze
        } else if let Some(r) = RESERVES.into_iter().find(|r| needs[r.i()] > 0 && freezes[r.i()].is_some()) {
            Block::Frozen(r)
        } else if exclusive
            && !other_wsl.is_empty()
            && needs[Reserve::Cpu.i()] > lev[Reserve::Cpu.i()].available - lev[Reserve::Cpu.i()].reserve
        {
            // Beyond the margin, an exclusive request still goes when the others' measured load leaves its cores.
            let ends: Option<Vec<u64>> = other_wsl
                .iter()
                .map(|a| if a.state == TicketState::Orphaned { None } else { a.planned_end() })
                .collect();
            Block::Exclusive(ends.and_then(|f| f.into_iter().max()))
        } else {
            let lacking: Vec<Reserve> = RESERVES
                .into_iter()
                .filter(|&r| !(exclusive && r == Reserve::Cpu))
                .filter(|r| needs[r.i()] > 0 && needs[r.i()] > lev[r.i()].free())
                .collect();
            if lacking.is_empty() { Block::Nothing } else { Block::Missing(lacking) }
        };

        if let Some(h) = head.as_mut() {
            let end = now + t.need.duration_s;
            let uses: Vec<Reserve> = h.missing.iter().copied().filter(|r| needs[r.i()] > 0).collect();
            let before_t = h.start_at.is_some_and(|v| end <= v);
            let anti_starvation = h.overtakes >= c.max_overtakes || now.saturating_sub(h.created) >= c.head_max_wait_s;
            // A ticket touching nothing the head lacks cannot delay it: anti-starvation neither limits nor counts it;
            // only "ends before T" passes share a resource with the head.
            let no_conflict = uses.is_empty();
            if matches!(block, Block::Nothing) && h.allowed && (no_conflict || (before_t && !anti_starvation)) {
                let why = if uses.is_empty() {
                    match h.missing.as_slice() {
                        [r] => tr!("does not use {}, which it lacks", "n'utilise pas {} qui lui manque", with_article(*r)),
                        all => tr!(
                            "uses neither {}, which it lacks",
                            "n'utilise ni {}, qui lui manquent",
                            all.iter().map(|&r| with_article(r)).collect::<Vec<_>>().join(tr!(" nor ", " ni ").as_str())
                        ),
                    }
                } else {
                    tr!(
                        "ends before its estimated start (around {})",
                        "finit avant son démarrage estimé (vers {})",
                        hhmm(h.start_at.unwrap_or(now))
                    )
                };
                let free_at_grant = grant(&mut e, &mut lev, &t, &needs, m, now);
                events.push(Event::Cleared(id));
                reasons.insert(
                    id,
                    Reason::simple(
                        DecisionKind::Cleared,
                        tr!(
                            "#{id}: cleared, went ahead of #{}: {why}. Free at clearance: {free_at_grant}.",
                            "#{id} : feu vert, passé devant #{} : {why}. Libre à l'accord : {free_at_grant}.",
                            h.id
                        ),
                    ),
                );
                if no_conflict {
                    h.unrelated_passes.push(id);
                } else {
                    h.overtakes += 1;
                    h.passes.push(id);
                    if let Some(ht) = e.ticket_mut(h.id) {
                        ht.overtakes = h.overtakes;
                    }
                }
                continue;
            }
            position += 1;
            let detail = if let Block::Missing(own) = &block {
                tr!("it lacks room itself: {}", "il manque lui-même de place : {}", names(own))
            } else if !h.allowed || !matches!(block, Block::Nothing) {
                tr!("strict queue, order of arrival", "file stricte, ordre d'arrivée")
            } else if anti_starvation && before_t {
                let why = if h.overtakes >= c.max_overtakes {
                    tr!("#{} has already been overtaken {} times", "#{} a déjà été doublé {} fois", h.id, h.overtakes)
                } else {
                    tr!(
                        "#{} has been waiting for more than {}",
                        "#{} attend depuis plus de {}",
                        h.id,
                        duration(c.head_max_wait_s)
                    )
                };
                tr!(
                    "anti-starvation: it also uses {} and {why}",
                    "anti-famine : il utilise aussi {} et {why}",
                    enumeration(&uses)
                )
            } else {
                let at = h
                    .start_at
                    .map_or(tr!("an unknown time", "une heure inconnue"), |v| tr!("around {}", "vers {}", hhmm(v)));
                let lacks = if uses.len() > 1 { tr!("lacks", "manquent") } else { tr!("lacks", "manque") };
                tr!(
                    "it also uses {0}, which #{2} {1}, and would end after its estimated start ({at})",
                    "il utilise aussi {} qui {} au #{} et finirait après son démarrage estimé ({at})",
                    enumeration(&uses),
                    lacks,
                    h.id
                )
            };
            let mut r = Reason::simple(
                DecisionKind::Waiting,
                tr!(
                    "#{id} waiting, position {position}: behind #{} ({detail}).",
                    "#{id} en attente, position {position} : derrière #{} ({detail}).",
                    h.id
                ),
            );
            r.position = Some(position);
            r.blockers = vec![h.id];
            reasons.insert(id, r);
            continue;
        }

        let new_head = |missing: Vec<Reserve>, start_at: Option<u64>, allowed: bool| Head {
            id,
            missing,
            start_at,
            allowed,
            overtakes: t.overtakes,
            created: t.created,
            passes: Vec::new(),
            unrelated_passes: Vec::new(),
        };
        match block {
            Block::Nothing => {
                let free_at_grant = grant(&mut e, &mut lev, &t, &needs, m, now);
                events.push(Event::Cleared(id));
                reasons.insert(
                    id,
                    Reason::simple(
                        DecisionKind::Cleared,
                        tr!(
                            "#{id}: cleared. Free at clearance: {free_at_grant}.",
                            "#{id} : feu vert. Libre à l'accord : {free_at_grant}."
                        ),
                    ),
                );
            }
            Block::ManualFreeze => {
                let g = manual_freeze.as_ref().expect("manual freeze present");
                position += 1;
                let mut reason = Reason::simple(
                    DecisionKind::Frozen,
                    tr!(
                        "#{id} waiting, position {position}: admissions frozen, {}. Lifted by: {}.",
                        "#{id} en attente, position {position} : admissions gelées, {}. Levée : {}.",
                        g.cause,
                        g.lifting
                    ),
                );
                reason.position = Some(position);
                reason.reserve = Some("manual".into());
                reasons.insert(id, reason);
                head = Some(new_head(Vec::new(), None, false));
            }
            Block::Frozen(r) => {
                let g = freezes[r.i()].as_ref().expect("freeze present");
                position += 1;
                let mut reason = Reason::simple(
                    DecisionKind::Frozen,
                    tr!(
                        "#{id} waiting, position {position}: admissions frozen on {}, {}. Lifted by: {}.",
                        "#{id} en attente, position {position} : admissions gelées sur {}, {}. Levée : {}.",
                        r.name(),
                        g.cause,
                        g.lifting
                    ),
                );
                reason.position = Some(position);
                reason.reserve = Some(r.key().into());
                reason.blockers = g.orphan.into_iter().collect();
                reasons.insert(id, reason);
                head = Some(new_head(vec![r], None, true));
            }
            Block::Exclusive(start_at) => {
                position += 1;
                reasons.insert(id, exclusive_reason(&t, position, &other_wsl, c, now));
                head = Some(new_head(vec![Reserve::Cpu], start_at, true));
            }
            Block::Missing(lacking) => {
                position += 1;
                let start_at = estimate(&t, &lev, &e, m, c).0;
                reasons.insert(id, wait_reason(&t, position, &lacking, &lev, &e, m, c, now));
                head = Some(new_head(lacking, start_at, true));
            }
        }
    }
    if let Some(h) = &head
        && let Some(r) = reasons.get_mut(&h.id)
    {
        if !h.passes.is_empty() {
            r.message += &tr!(
                " Overtaken by {} ({}/{}).",
                " Doublé par {} ({}/{}).",
                list(&h.passes),
                h.overtakes,
                c.max_overtakes
            );
        }
        if !h.unrelated_passes.is_empty() {
            r.message += &tr!(
                " Went ahead on a resource it does not need: {}.",
                " Passés sur une ressource dont il n'a pas besoin : {}.",
                list(&h.unrelated_passes)
            );
        }
        if h.allowed
            && (h.overtakes >= c.max_overtakes || now.saturating_sub(h.created) >= c.head_max_wait_s)
        {
            r.message += &tr!(
                " Anti-starvation: no ticket goes ahead of it any more by taking what it lacks.",
                " Anti-famine : plus aucun ticket ne passe devant en prenant ce qui lui manque."
            );
        }
    }

    for t in &e.tickets {
        let id = t.id;
        match t.state {
            TicketState::Active if !reasons.contains_key(&id) => {
                let granted = t.granted.unwrap_or(now);
                let end = t.planned_end().unwrap_or(now);
                let rest = if !t.acked {
                    tr!(
                        " Clearance not yet acknowledged by the client: taken back at {} without an ack.",
                        " Feu vert pas encore accusé par le client : repris à {} sans accusé.",
                        hhmm(granted + c.recall_delay_s)
                    )
                } else if t.manual && now >= end {
                    tr!(
                        " Lease overrun by {} (declared duration {}): reservation kept, the queue goes on with the \
                         remaining resources. Work in progress: `clearway extend {id} <duration> \"<reason>\"`; done: \
                         `clearway release {id}`. Without news, orphaned at {} and admissions frozen.",
                        " Bail dépassé depuis {} (durée déclarée de {}) : réservation maintenue, la file continue sur les \
                         ressources restantes. Travail en cours : « clearway extend {id} <durée> \"<raison>\" » ; terminé : \
                         « clearway release {id} ». Sans nouvelle, orphelin à {} et admissions gelées.",
                        duration(now - end),
                        duration(t.need.duration_s),
                        hhmm(last_news(t) + c.absence_delay_s)
                    )
                } else if t.manual {
                    tr!(
                        " Lease until {}: after that, `clearway extend {id}` or `clearway release {id}`, otherwise orphaned at {}.",
                        " Bail jusqu'à {} : au-delà, « clearway extend {id} » ou « clearway release {id} », sinon orphelin à {}.",
                        hhmm(end),
                        hhmm(last_news(t) + c.absence_delay_s)
                    )
                } else {
                    String::new()
                };
                let rampup = if now.saturating_sub(granted) < c.rampup_delay_s {
                    tr!(
                        " Ramp-up until {}: whole reservation.",
                        " Montée en charge jusqu'à {} : réservation entière.",
                        hhmm(granted + c.rampup_delay_s)
                    )
                } else {
                    let remaining: Vec<String> = RESERVES
                        .into_iter()
                        .filter(|&r| debit(r, t.side, &t.need, m, c) > 0)
                        .map(|r| format!("{} {}", r.name(), r.amount(reservation(&e, t, r, m, c, now))))
                        .collect();
                    tr!(
                        " Ramp-up over, still reserved: {}.",
                        " Montée en charge terminée, réservé encore : {}.",
                        remaining.join(", ")
                    )
                };
                let force = if t.force { tr!(" (forced by hand)", " (forcé à la main)") } else { String::new() };
                reasons.insert(
                    id,
                    Reason::simple(
                        DecisionKind::Active,
                        tr!(
                            "#{id} active{force} since {}, expected end around {}.{rampup}{rest}",
                            "#{id} actif{force} depuis {}, fin prévue vers {}.{rampup}{rest}",
                            hhmm(granted),
                            hhmm(end)
                        ),
                    ),
                );
            }
            TicketState::Orphaned if t.orphan_cause == Some(OrphanCause::Connection) => {
                let mut reason = Reason::simple(
                    DecisionKind::Orphaned,
                    tr!(
                        "#{id} orphaned: {}. Reservation kept without freezing the queue; freed at {} without a resume. Lifted by: {}.",
                        "#{id} orphelin : {}. Réservation gardée sans geler la file ; libéré à {} sans reprise. Levée : {}.",
                        orphan_cause(t, now),
                        hhmm(t.lost.unwrap_or(now) + c.resume_delay_s),
                        orphan_lifting(t)
                    ),
                );
                reason.blockers = vec![id];
                reasons.insert(id, reason);
            }
            TicketState::Orphaned => {
                let frozen_names: Vec<&str> = RESERVES
                    .iter()
                    .filter(|r| r.need(t.side, &t.need) > 0)
                    .map(|r| r.name())
                    .collect();
                let mut reason = Reason::simple(
                    DecisionKind::Orphaned,
                    tr!(
                        "#{id} orphaned: {}. Its reservation still counts and admissions are frozen on {}. Lifted by: {}.",
                        "#{id} orphelin : {}. Sa réservation reste comptée et les admissions sont gelées sur {}. Levée : {}.",
                        orphan_cause(t, now),
                        frozen_names.join(", "),
                        orphan_lifting(t)
                    ),
                );
                reason.blockers = vec![id];
                reasons.insert(id, reason);
            }
            _ => {}
        }
    }

    let mut freeze_texts: Vec<String> = Vec::new();
    for g in manual_freeze.iter().chain(freezes.iter().flatten()) {
        let text = tr!(
            "Admissions frozen: {}. Lifted by: {}.",
            "Admissions gelées : {}. Levée : {}.",
            g.cause,
            g.lifting
        );
        if !freeze_texts.contains(&text) {
            freeze_texts.push(text);
        }
    }

    persist |= !events.is_empty();
    Plan { state: e, events, persist, reasons, freeze_texts, pressure }
}

fn need_summary(t: &Ticket) -> String {
    format!("{}, {}, {}", t.side.name(), gb(t.need.ram_mb as i64), cores(t.need.cpu as i64))
}

/// Confirmation text for a forced clearance: what would stay free on each reserve the ticket debits.
pub fn force_preview(e: &State, id: u64, m: &Measures, c: &Config, now: u64) -> Option<String> {
    let t = e.ticket(id).filter(|t| t.state == TicketState::Waiting)?;
    let lev = levels(e, m, c, now);
    let after: Vec<String> = RESERVES
        .iter()
        .filter_map(|&r| {
            let b = needs_of(t, &lev, m, c)[r.i()];
            (b > 0).then(|| {
                tr!(
                    "{} free after: {} (margin {})",
                    "{} libre après : {} (marge {})",
                    r.name(),
                    r.amount(lev[r.i()].free() - b),
                    r.amount(lev[r.i()].margin)
                )
            })
        })
        .collect();
    Some(tr!("Force #{id} ({})? {}.", "Forcer #{id} ({}) ? {}.", need_summary(t), after.join(", ")))
}

/// Immediate clearance given by hand, outside the queue order, the reserves and the freezes.
/// The others keep their place; anti-starvation does not count a forced clearance as an overtake.
pub fn force_through(e: &State, id: u64, m: &Measures, c: &Config, now: u64) -> Result<(State, String), String> {
    let t = e
        .ticket(id)
        .filter(|t| t.state == TicketState::Waiting)
        .cloned()
        .ok_or_else(|| {
            tr!(
                "Ticket #{id} missing or not waiting: nothing to force.",
                "Ticket #{id} absent ou pas en attente : rien à forcer."
            )
        })?;
    let mut lev = levels(e, m, c, now);
    let needs = needs_of(&t, &lev, m, c);
    let mut new = e.clone();
    let free_at_grant = grant(&mut new, &mut lev, &t, &needs, m, now);
    if let Some(f) = new.ticket_mut(id) {
        f.force = true;
        f.recalled = None;
    }
    Ok((
        new,
        tr!(
            "#{id} ({}) · reserves at the time of forcing: {free_at_grant}",
            "#{id} ({}) · réserves au moment du forçage : {free_at_grant}",
            need_summary(&t)
        ),
    ))
}

fn exclusive_reason(t: &Ticket, position: usize, others: &[&Ticket], c: &Config, now: u64) -> Reason {
    let ids: Vec<u64> = others.iter().map(|a| a.id).collect();
    let end = others.iter().map(|a| a.planned_end()).collect::<Option<Vec<u64>>>().and_then(|f| f.into_iter().max());
    let at = match (end, others.iter().any(|a| a.state == TicketState::Orphaned)) {
        (_, true) => tr!(
            " An orphan is among them: it takes its end, its cancellation or \"Release\".",
            " Un orphelin en fait partie : il faut sa fin, son annulation ou « Libérer »."
        ),
        (Some(f), false) if f < now => tr!(
            " Unblocked when {} end, the last one expected around {} (overdue).",
            " Débloqué à la fin de {}, la dernière prévue vers {} (dépassée).",
            list(&ids),
            hhmm(f)
        ),
        (Some(f), false) => tr!(
            " Unblocked when {} end, the last one expected around {}.",
            " Débloqué à la fin de {}, la dernière prévue vers {}.",
            list(&ids),
            hhmm(f)
        ),
        (None, false) => String::new(),
    };
    let mut r = Reason::simple(
        DecisionKind::Waiting,
        tr!(
            "#{} waiting, position {position}: exclusive request ({} out of {}, beyond {} − margin {}), it goes when no other WSL ticket is active; active: {}.{at}",
            "#{} en attente, position {position} : demande exclusive ({} sur {}, au-delà de {} − marge {}), elle passe quand aucun autre ticket WSL n'est actif ; actifs : {}.{at}",
            t.id,
            cores(t.need.cpu as i64),
            c.cpu_total,
            c.cpu_total,
            c.margin_cpu,
            list(&ids)
        ),
    );
    r.position = Some(position);
    r.reserve = Some("exclusive".into());
    r.blockers = ids;
    r.unblocked_at = end;
    r
}

#[allow(clippy::too_many_arguments)]
fn wait_reason(
    t: &Ticket,
    position: usize,
    lacking: &[Reserve],
    lev: &[Level; RESERVE_COUNT],
    e: &State,
    m: &Measures,
    c: &Config,
    now: u64,
) -> Reason {
    let r = lacking[0];
    let n = &lev[r.i()];
    let need = needs_of(t, lev, m, c)[r.i()];
    let of_which = if n.reserve > 0 {
        tr!(" of which {} reserved for {}", " dont {} réservés pour {}", r.amount(n.reserve), list(&n.holders))
    } else {
        String::new()
    };
    let mut message = tr!(
        "#{} waiting, position {position}: {} {} (need {}, available {}{of_which}, margin {}).",
        "#{} en attente, position {position} : {} {} (besoin {}, disponible {}{of_which}, marge {}).",
        t.id,
        r.name(),
        r.insufficient(),
        r.amount(need),
        r.amount(n.available),
        r.amount(n.margin)
    );
    for other in &lacking[1..] {
        message += &tr!(" {} {} too.", " {} {} aussi.", other.name(), other.insufficient());
    }

    let (start_at, expected) = estimate(t, lev, e, m, c);
    match start_at {
        Some(f) => {
            let at = if f < now {
                tr!("expected around {} (overdue)", "prévue vers {} (dépassée)", hhmm(f))
            } else {
                tr!("expected around {}", "prévue vers {}", hhmm(f))
            };
            if expected.len() == 1 {
                message += &tr!(" Unblocked when #{} ends, {at}.", " Débloqué à la fin du #{}, {at}.", expected[0]);
            } else {
                message += &tr!(
                    " Unblocked when {} end, the last one {at}.",
                    " Débloqué à la fin des {}, la dernière {at}.",
                    list(&expected)
                );
            }
        }
        None => {
            message += &tr!(
                " The expected ends of the active tickets do not free enough: the room is taken outside the queue (other programs, Docker, cache).",
                " Les fins prévues des actifs ne libèrent pas assez : la place est prise hors file (autres programmes, Docker, cache)."
            );
        }
    }

    if lacking.iter().all(|r| r.is_ram()) {
        let max = RESERVES
            .iter()
            .filter(|r| r.is_ram() && r.need(t.side, &t.need) > 0)
            .map(|&r| if r == Reserve::RamWindows && t.side == Side::Wsl { lev[r.i()].wsl.max_need(lev[r.i()].free()) } else { lev[r.i()].free() })
            .min()
            .unwrap_or(0);
        if max >= 256 {
            message += &tr!(
                " A new request with --ram {} or less would pass now.",
                " Une nouvelle demande avec --ram {} ou moins passerait maintenant.",
                ram_option(max)
            );
        }
    } else if lacking.iter().all(|r| !r.is_ram()) {
        let max = lacking.iter().map(|r| lev[r.i()].free()).min().unwrap_or(0);
        if max >= 1 {
            message += &tr!(
                " A new request with --cpu {max} would pass now.",
                " Une nouvelle demande avec --cpu {max} passerait maintenant."
            );
        }
    }

    let mut reason = Reason::simple(DecisionKind::Waiting, message);
    reason.position = Some(position);
    reason.reserve = Some(r.key().into());
    reason.need = Some(need);
    reason.available = Some(n.available);
    reason.reserved_by_active = Some(n.reserve);
    reason.margin = Some(n.margin);
    reason.blockers = n.holders.clone();
    reason.unblocked_at = start_at;
    reason
}

/// Time T at which the head would fit if the active tickets ended as planned; orphans do not count.
fn estimate(t: &Ticket, lev: &[Level; RESERVE_COUNT], e: &State, m: &Measures, c: &Config) -> (Option<u64>, Vec<u64>) {
    let mut free = lev.each_ref().map(Level::free);
    let mut actives: Vec<&Ticket> = e.tickets.iter().filter(|a| a.state == TicketState::Active).collect();
    actives.sort_by_key(|a| (a.planned_end(), a.id));
    let needs = needs_of(t, lev, m, c);
    let mut expected = Vec::new();
    for a in actives {
        let mut useful = false;
        for r in RESERVES {
            let b = debit(r, a.side, &a.need, m, c);
            free[r.i()] += b;
            useful |= b > 0 && debit(r, t.side, &t.need, m, c) > 0;
        }
        if useful {
            expected.push(a.id);
        }
        if RESERVES.iter().all(|&r| needs[r.i()] == 0 || needs[r.i()] <= free[r.i()]) {
            return (a.planned_end(), expected);
        }
    }
    (None, Vec::new())
}

#[cfg(test)]
mod tests;
