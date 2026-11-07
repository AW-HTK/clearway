use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Wsl,
    Windows,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Wsl => "wsl",
            Side::Windows => "windows",
        }
    }

    pub fn read(s: &str) -> Option<Side> {
        match s {
            "wsl" => Some(Side::Wsl),
            "windows" => Some(Side::Windows),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketState {
    Waiting,
    Active,
    Orphaned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrphanCause {
    Silence,
    Lease,
    /// The session's link (MCP process) closed: reservation kept for `resume_delay_s`, without freezing.
    Connection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Need {
    pub ram_mb: u64,
    pub cpu: u32,
    pub duration_s: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ticket {
    pub id: u64,
    pub request_id: String,
    pub side: Side,
    pub need: Need,
    pub description: String,
    pub session: String,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<u32>,
    pub dir: String,
    pub signature: String,
    pub state: TicketState,
    pub created: u64,
    pub granted: Option<u64>,
    pub avail_at_grant_mb: Option<u64>,
    /// Ticket from the `request` command: no supervisor, the declared duration is the lease.
    #[serde(default)]
    pub manual: bool,
    /// The clearance reached a client: ack, presence signal, or reply sent to an old client without ack.
    #[serde(default)]
    pub acked: bool,
    /// Manual ticket past its declared duration: still active and reserved, without freezing the queue.
    #[serde(default)]
    pub lease_overrun: bool,
    /// Clearance recalled for lack of use: the place is kept, the ticket is skipped until the client comes back.
    #[serde(default)]
    pub recalled: Option<u64>,
    #[serde(default)]
    pub orphan_cause: Option<OrphanCause>,
    #[serde(default)]
    pub reattach_key: String,
    /// Git root (or current directory) and branch / worktree, computed by the client at creation.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// Taken at clearance to measure what the ticket added: available RAM on both sides and vmmemWSL.
    #[serde(default)]
    pub base_ram_windows_mb: Option<i64>,
    #[serde(default)]
    pub base_ram_wsl_mb: Option<i64>,
    #[serde(default)]
    pub base_vmmem_mb: Option<u64>,
    /// Clearance given by hand from the window, outside the queue order.
    #[serde(default)]
    pub force: bool,
    /// Tickets that went ahead of this one while it was the blocked head of the queue (anti-starvation).
    #[serde(default)]
    pub overtakes: u32,
    /// Link (connection of an MCP process) that created the ticket, and its session identity for resuming.
    #[serde(default)]
    pub link: Option<u64>,
    #[serde(default)]
    pub identity: Option<String>,
    /// Link lost at this time: a new link with the same identity resumes the ticket within `resume_delay_s`.
    #[serde(default)]
    pub lost: Option<u64>,
    #[serde(skip)]
    pub presence: u64,
    #[serde(skip)]
    pub listeners: u32,
}

impl Ticket {
    pub fn planned_end(&self) -> Option<u64> {
        self.granted.map(|a| a + self.need.duration_s)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub epoch: u64,
    pub counter: u64,
    pub vmmem_pid: Option<u32>,
    #[serde(default)]
    pub manual_freeze: Option<u64>,
    pub tickets: Vec<Ticket>,
    /// Swap pressure hysteresis: declared, and calm since when.
    #[serde(skip)]
    pub swap_pressure: bool,
    #[serde(skip)]
    pub swap_calm_since: Option<u64>,
    /// Measured CPU pressure per side, with hysteresis: declared, and since when the opposite condition holds.
    #[serde(skip)]
    pub cpu_pressure: CpuPressure,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hysteresis {
    pub on: bool,
    pub since: Option<u64>,
    pub declared_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuPressure {
    pub windows: Hysteresis,
    pub wsl: Hysteresis,
}

impl State {
    pub fn ticket(&self, id: u64) -> Option<&Ticket> {
        self.tickets.iter().find(|t| t.id == id)
    }

    pub fn ticket_mut(&mut self, id: u64) -> Option<&mut Ticket> {
        self.tickets.iter_mut().find(|t| t.id == id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vmmem {
    pub pid: u32,
    pub rss_mb: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WslMeasures {
    pub mem_avail_mb: u64,
    pub mem_total_mb: u64,
    pub load1: f32,
    #[serde(default)]
    pub swap_total_mb: u64,
    #[serde(default)]
    pub swap_free_mb: u64,
    /// Cumulative /proc/vmstat counters (pages).
    #[serde(default)]
    pub pswpin: u64,
    #[serde(default)]
    pub pswpout: u64,
    #[serde(default)]
    pub at: u64,
}

impl WslMeasures {
    /// Swap used, in percent; 0 without swap.
    pub fn swap_pct(&self) -> u64 {
        if self.swap_total_mb == 0 {
            return 0;
        }
        self.swap_total_mb.saturating_sub(self.swap_free_mb) * 100 / self.swap_total_mb
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Measures {
    pub ram_total_mb: u64,
    pub ram_avail_mb: u64,
    pub commit_mb: u64,
    pub commit_limit_mb: u64,
    pub cpu_pct: f32,
    #[serde(default)]
    pub windows_cores: u32,
    /// WSL swap-out rate (pages/s over ~30 s), computed by the server from the client's counters.
    #[serde(default)]
    pub swap_out_ps: f32,
    pub vmmem: Option<Vmmem>,
    #[serde(default)]
    pub wsl: Option<WslMeasures>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TicketRequest {
    pub request_id: String,
    pub side: Side,
    pub ram_mb: Option<u64>,
    pub cpu: Option<u32>,
    pub duration_s: Option<u64>,
    pub description: String,
    pub session: String,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<u32>,
    pub dir: String,
    pub signature: String,
    pub measures: Option<WslMeasures>,
    #[serde(default)]
    pub manual: bool,
    /// Same value for an identical rerun of a command: it takes its ticket back instead of creating one.
    #[serde(default)]
    pub reattach_key: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// The client will confirm receipt of an immediate clearance with `/tickets/{n}/ack`.
    #[serde(default)]
    pub ack: bool,
}

/// Opening a link: the connection carrying it stands for the session while it stays open.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Link {
    /// Stable session key (Claude or Codex id, else PID or directory), used for resuming.
    pub identity: String,
    pub session: String,
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub measures: Option<WslMeasures>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Heartbeat {
    #[serde(default)]
    pub measures: Option<WslMeasures>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Extension {
    pub duration_s: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Presence {
    pub measures: Option<WslMeasures>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Release {
    pub code: Option<i32>,
    pub peak_mb: Option<u64>,
    pub duration_s: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Refused,
    Waiting,
    Frozen,
    Cleared,
    Active,
    Orphaned,
    Finished,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reason {
    pub decision: DecisionKind,
    #[serde(default)]
    pub position: Option<usize>,
    #[serde(default)]
    pub reserve: Option<String>,
    #[serde(default)]
    pub need: Option<i64>,
    #[serde(default)]
    pub available: Option<i64>,
    #[serde(default)]
    pub reserved_by_active: Option<i64>,
    #[serde(default)]
    pub margin: Option<i64>,
    #[serde(default)]
    pub blockers: Vec<u64>,
    #[serde(default)]
    pub unblocked_at: Option<u64>,
    pub message: String,
}

impl Reason {
    pub fn simple(decision: DecisionKind, message: String) -> Reason {
        Reason {
            decision,
            position: None,
            reserve: None,
            need: None,
            available: None,
            reserved_by_active: None,
            margin: None,
            blockers: Vec::new(),
            unblocked_at: None,
            message,
        }
    }

    /// Stable key of a reason: ignores measured values that change with every reading.
    pub fn key(&self) -> String {
        let decision = serde_json::to_value(self.decision)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let blockers: Vec<String> = self.blockers.iter().map(u64::to_string).collect();
        format!(
            "{}-{}-{}-{}-{}",
            decision,
            self.position.unwrap_or(0),
            self.reserve.as_deref().unwrap_or("x"),
            blockers.join("_"),
            self.unblocked_at.unwrap_or(0)
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct View {
    pub id: u64,
    pub state: String,
    pub epoch: u64,
    pub reason: Reason,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReserveView {
    pub key: String,
    pub name: String,
    pub total: i64,
    pub available: i64,
    pub reserve: i64,
    pub margin: i64,
    pub free: i64,
    /// Running exclusive WSL ticket, which explains a negative free WSL CPU.
    #[serde(default)]
    pub exclusive: Option<u64>,
    /// WSL RAM estimated as 12 GB − vmmemWSL for lack of a recent measure: time of the last measure (0: none).
    #[serde(default)]
    pub estimate: Option<u64>,
    /// Measured use of a CPU reserve, in percent of its budget (WSL: 1-min load average; Windows: CPU usage).
    #[serde(default)]
    pub used_pct: Option<u32>,
}

impl ReserveView {
    /// Free amount as displayed: never below zero, with the exclusive ticket that explains it.
    pub fn free_text(&self) -> String {
        let free = self.free.max(0);
        let text = if self.key.starts_with("cpu") { crate::format::cores(free) } else { crate::format::gb(free) };
        if let Some(at) = self.estimate {
            if at == 0 {
                return crate::tr!(
                    "estimate {text}, cache included, no WSL measure",
                    "estimation {text}, cache compris, aucune mesure WSL"
                );
            }
            let hhmm = crate::system::hhmm(at);
            return crate::tr!(
                "estimate {text}, cache included, no WSL measure since {hhmm}",
                "estimation {text}, cache compris, pas de mesure WSL depuis {hhmm}"
            );
        }
        match self.exclusive {
            Some(id) => crate::tr!("{text} (exclusive #{id})", "{text} (exclusif #{id})"),
            None => text,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TicketView {
    pub id: u64,
    pub state: TicketState,
    pub side: Side,
    pub need: Need,
    pub description: String,
    pub session: String,
    pub created: u64,
    pub granted: Option<u64>,
    pub manual: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub lease_overrun: bool,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    pub presence_age_s: u64,
    pub message: String,
    /// Key of the session that owns the ticket (`claude:<id>`, `codex:<id>`…), for the command guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    /// Version of the running server, checked by the installer after an update.
    #[serde(default)]
    pub version: String,
    pub epoch: u64,
    pub now: u64,
    pub measures: Measures,
    pub reserves: Vec<ReserveView>,
    pub tickets: Vec<TicketView>,
    pub alerts: Vec<String>,
    #[serde(default)]
    pub manual_freeze: Option<u64>,
}
