//! Clearway's MCP tools: `request_clearance` blocks until the clearance, `await_clearance` resumes a wait,
//! `release`, `extend`, `cancel` and `status` answer at once. Generic over the channel to the server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::format::{parse_duration, parse_ram};
use crate::mcp::{Call, Provider, ServerInfo, ToolResult};
use crate::model::{Extension, Release, Side, Status, TicketRequest, View};
use crate::protocol::{Request, Response};
use crate::tr;

/// Requests to the Clearway server; several may be in flight at once (multiplexed connection).
pub trait Channel: Send + Sync + 'static {
    fn call(&self, request: Request, timeout: Duration) -> Result<Response, String>;
}

/// Who asks: shown in the queue and used to find the session's tickets again.
#[derive(Clone, Debug, Default)]
pub struct SessionInfo {
    pub name: String,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<u32>,
    pub dir: String,
    pub project: Option<String>,
    pub branch: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// Longest blocking wait of one call; `None`: until the clearance (Claude). Codex: under its `tool_timeout_sec`.
    pub default_max_s: Option<u64>,
    /// One long poll lasts at most this long, so that a cancellation is seen within it.
    pub slice_s: u64,
    /// Progress is sent on every change of reason and at least this often.
    pub progress_every_s: u64,
    pub retry_pause: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { default_max_s: None, slice_s: 15, progress_every_s: 60, retry_pause: Duration::from_secs(2) }
    }
}

const SHORT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Tools<C: Channel> {
    channel: Arc<C>,
    session: SessionInfo,
    settings: Settings,
    /// Last ticket requested through this server: `await_clearance` without a number resumes it.
    last_ticket: Mutex<Option<u64>>,
}

enum WaitEnd {
    Cleared(View),
    Out(String),
    Still(View),
    Cancelled,
}

fn ticket_arg(arguments: &Value) -> Result<u64, String> {
    arguments["ticket"]
        .as_u64()
        .or_else(|| arguments["ticket"].as_str().and_then(|s| s.trim_start_matches('#').parse().ok()))
        .ok_or_else(|| tr!("`ticket` (number) expected", "`ticket` (numéro) attendu"))
}

fn message_of(body: &Value) -> String {
    body["reason"]["message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .or_else(|| body["error"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| body.to_string())
}

impl<C: Channel> Tools<C> {
    pub fn new(channel: Arc<C>, session: SessionInfo, settings: Settings) -> Tools<C> {
        Tools { channel, session, settings, last_ticket: Mutex::new(None) }
    }

    pub fn info() -> ServerInfo {
        ServerInfo {
            name: "clearway".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            instructions: tr!(
                "Clearway shares the machine's RAM and CPU between sessions. Before any heavy task (build, full test \
                 suite, render, local AI, docker build…): call request_clearance, which waits for your turn and may \
                 take minutes; then run the heavy command (Bash); then call release with the ticket number and the exit \
                 code. If the call is interrupted, the ticket keeps its place: resume with await_clearance.",
                "Clearway partage la RAM et le CPU de la machine entre les sessions. Avant toute tâche lourde (build, \
                 suite de tests complète, rendu, IA locale, docker build…) : appelle request_clearance, qui attend ton \
                 tour et peut durer des minutes ; lance ensuite la commande lourde (Bash) ; puis appelle release avec \
                 le numéro du ticket et le code de sortie. Si l'appel est interrompu, le ticket garde sa place : \
                 reprends avec await_clearance."
            ),
        }
    }

    fn call_server(&self, method: &str, path: &str, body: Value) -> Result<Response, String> {
        self.channel.call(Request::new(method, path, body), SHORT_TIMEOUT)
    }

    fn result(&self, view: Option<&View>, ticket: Option<u64>, cleared: bool, state: &str, next: String) -> ToolResult {
        let message = view.map(|v| v.reason.message.clone()).unwrap_or_default();
        let structured = json!({
            "ticket": ticket,
            "state": state,
            "cleared": cleared,
            "message": message,
            "position": view.and_then(|v| v.reason.position),
            "next_step": next,
        });
        let text = if message.is_empty() { next } else { format!("{message}\n{next}") };
        ToolResult { text, structured: Some(structured), is_error: false, after: None }
    }

    fn cleared(&self, view: &View) -> ToolResult {
        let id = view.id;
        let next = tr!(
            "Clearance for ticket #{id}: run the heavy command now, then call release(ticket={id}, code=<exit code>).",
            "Feu vert pour le ticket #{id} : lance la commande lourde maintenant, puis appelle release(ticket={id}, code=<code de sortie>)."
        );
        let mut r = self.result(Some(view), Some(id), true, &view.state, next);
        let channel = self.channel.clone();
        r.after = Some(Box::new(move || {
            let _ = channel.call(Request::new("POST", &format!("/tickets/{id}/ack"), Value::Null), SHORT_TIMEOUT);
        }));
        r
    }

    fn after_wait(&self, id: u64, end: WaitEnd, max_s: Option<u64>) -> ToolResult {
        match end {
            WaitEnd::Cleared(v) => self.cleared(&v),
            WaitEnd::Out(message) => {
                let next = tr!(
                    "Ticket #{id} is no longer in the queue: request a new one if the task is still to do.",
                    "Le ticket #{id} n'est plus dans la file : demandes-en un nouveau si la tâche reste à faire."
                );
                let mut r = self.result(None, Some(id), false, "out", next);
                r.text = format!("{message}\n{}", r.text);
                r
            }
            WaitEnd::Still(v) => {
                let after = crate::format::duration(max_s.unwrap_or(0));
                let next = tr!(
                    "Still waiting after {after}; the ticket keeps its place: call await_clearance(ticket={id}).",
                    "Toujours en attente après {after} ; le ticket garde sa place : appelle await_clearance(ticket={id})."
                );
                self.result(Some(&v), Some(id), false, &v.state, next)
            }
            WaitEnd::Cancelled => ToolResult::text(""),
        }
    }

    /// Waits in the foreground: long polls of `slice_s`, progress on each change and at least every `progress_every_s`.
    fn wait(&self, id: u64, max_s: Option<u64>, call: &Call) -> WaitEnd {
        let deadline = max_s.map(|s| Instant::now() + Duration::from_secs(s));
        let mut known = String::new();
        let mut last_view: Option<View> = None;
        let mut last_progress = Instant::now();
        loop {
            if call.cancelled() {
                return WaitEnd::Cancelled;
            }
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if left.is_some_and(|l| l.is_zero()) {
                return WaitEnd::Still(last_view.unwrap_or_else(|| placeholder(id)));
            }
            let slice = left.map_or(self.settings.slice_s, |l| l.as_secs().clamp(1, self.settings.slice_s.max(1)));
            let mut request = Request::new("GET", &format!("/tickets/{id}/wait"), Value::Null);
            request.params = BTreeMap::from([
                ("known".into(), known.clone()),
                ("timeout_s".into(), slice.to_string()),
                ("ack".into(), "1".into()),
            ]);
            let response = match self.channel.call(request, Duration::from_secs(slice + 30)) {
                Ok(r) => r,
                Err(e) => {
                    call.progress(&tr!(
                        "Clearway unreachable ({e}); ticket #{id} keeps its place, retrying.",
                        "Clearway injoignable ({e}) ; le ticket #{id} garde sa place, nouvel essai."
                    ));
                    std::thread::sleep(self.settings.retry_pause);
                    continue;
                }
            };
            match response.code {
                200 => {
                    let Ok(view) = serde_json::from_value::<View>(response.body.clone()) else {
                        return WaitEnd::Out(message_of(&response.body));
                    };
                    let key = view.reason.key();
                    if key != known || last_progress.elapsed() >= Duration::from_secs(self.settings.progress_every_s) {
                        call.progress(&view.reason.message);
                        last_progress = Instant::now();
                        known = key;
                    }
                    if view.state == "active" || view.state == "orphaned" {
                        return WaitEnd::Cleared(view);
                    }
                    last_view = Some(view);
                }
                404 => return WaitEnd::Out(message_of(&response.body)),
                code => {
                    call.progress(&tr!(
                        "Clearway answered {code} ({}); retrying.",
                        "Clearway a répondu {code} ({}) ; nouvel essai.",
                        message_of(&response.body)
                    ));
                    std::thread::sleep(self.settings.retry_pause);
                }
            }
        }
    }

    fn request_clearance(&self, arguments: &Value, call: &Call) -> Result<ToolResult, String> {
        let description = arguments["description"]
            .as_str()
            .filter(|d| !d.trim().is_empty())
            .ok_or_else(|| tr!("`description` expected", "`description` attendue"))?;
        let side = match arguments["side"].as_str() {
            None => Side::Wsl,
            Some(s) => Side::read(s).ok_or_else(|| tr!("`side`: wsl or windows", "`side` : wsl ou windows"))?,
        };
        let ram_mb = match &arguments["ram"] {
            Value::Null => None,
            Value::Number(n) => n.as_f64().map(|g| (g * 1024.0).round() as u64),
            v => Some(parse_ram(v.as_str().unwrap_or("")).ok_or_else(|| tr!("unreadable `ram` (e.g. \"6G\")", "`ram` illisible (ex. « 6G »)"))?),
        };
        let duration_s = match &arguments["duration"] {
            Value::Null => None,
            Value::Number(n) => n.as_u64().map(|m| m * 60),
            v => Some(
                parse_duration(v.as_str().unwrap_or(""))
                    .ok_or_else(|| tr!("unreadable `duration` (e.g. \"30m\")", "`duration` illisible (ex. « 30m »)"))?,
            ),
        };
        let request_id = crate::system::random(8).map(|b| crate::system::hex(&b)).unwrap_or_default();
        let body = TicketRequest {
            request_id,
            side,
            ram_mb,
            cpu: arguments["cpu"].as_u64().map(|c| c as u32),
            duration_s,
            description: description.to_owned(),
            session: self.session.name.clone(),
            claude_session_id: self.session.claude_session_id.clone(),
            claude_pid: self.session.claude_pid,
            dir: self.session.dir.clone(),
            signature: "mcp".into(),
            measures: None,
            manual: true,
            reattach_key: String::new(),
            project: self.session.project.clone(),
            branch: self.session.branch.clone(),
            ack: true,
        };
        let response = match self.call_server("POST", "/tickets", serde_json::to_value(&body).map_err(|e| e.to_string())?) {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::error(tr!("Clearway unreachable: {e}", "Clearway injoignable : {e}"))),
        };
        let view = match response.code {
            200 | 201 => serde_json::from_value::<View>(response.body.clone()).map_err(|e| e.to_string())?,
            422 => {
                let view = serde_json::from_value::<View>(response.body.clone()).ok();
                let next = tr!(
                    "Refused: lower the request as the message says, then call request_clearance again.",
                    "Refusé : baisse la demande comme l'indique le message, puis rappelle request_clearance."
                );
                return Ok(self.result(view.as_ref(), None, false, "refused", next));
            }
            code => return Ok(ToolResult::error(format!("{code}: {}", message_of(&response.body)))),
        };
        *self.last_ticket.lock().unwrap_or_else(|e| e.into_inner()) = Some(view.id);
        call.progress(&view.reason.message);
        if view.state == "active" || view.state == "orphaned" {
            return Ok(self.cleared(&view));
        }
        let max_s = arguments["max_s"].as_u64().or(self.settings.default_max_s);
        Ok(self.after_wait(view.id, self.wait(view.id, max_s, call), max_s))
    }

    fn await_clearance(&self, arguments: &Value, call: &Call) -> Result<ToolResult, String> {
        let id = match ticket_arg(arguments) {
            Ok(id) => id,
            Err(e) => (*self.last_ticket.lock().unwrap_or_else(|e| e.into_inner())).ok_or(e)?,
        };
        let max_s = arguments["max_s"].as_u64().or(self.settings.default_max_s);
        Ok(self.after_wait(id, self.wait(id, max_s, call), max_s))
    }

    fn simple(&self, method: &str, path: String, body: Value) -> ToolResult {
        match self.call_server(method, &path, body) {
            Ok(r) if r.code == 200 => ToolResult::text(message_of(&r.body)),
            Ok(r) => ToolResult::error(format!("{}: {}", r.code, message_of(&r.body))),
            Err(e) => ToolResult::error(tr!("Clearway unreachable: {e}", "Clearway injoignable : {e}")),
        }
    }

    fn status(&self) -> ToolResult {
        let response = match self.call_server("GET", "/status", Value::Null) {
            Ok(r) if r.code == 200 => r,
            Ok(r) => return ToolResult::error(format!("{}: {}", r.code, message_of(&r.body))),
            Err(e) => return ToolResult::error(tr!("Clearway unreachable: {e}", "Clearway injoignable : {e}")),
        };
        let Ok(status) = serde_json::from_value::<Status>(response.body) else {
            return ToolResult::error(tr!("unreadable status", "statut illisible"));
        };
        let mut lines: Vec<String> = status.reserves.iter().map(|r| format!("{}: {}", r.name, r.free_text())).collect();
        lines.extend(status.alerts.iter().map(|a| format!("! {a}")));
        for t in &status.tickets {
            lines.push(format!("#{} {:?} [{}] {}: {}", t.id, t.state, t.session, t.description, t.message));
        }
        if status.tickets.is_empty() {
            lines.push(tr!("Queue empty.", "File vide."));
        }
        ToolResult::text(lines.join("\n"))
    }
}

/// View used when the deadline passes before any answer from the server.
fn placeholder(id: u64) -> View {
    View {
        id,
        state: "waiting".into(),
        epoch: 0,
        reason: crate::model::Reason::simple(crate::model::DecisionKind::Waiting, String::new()),
    }
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

impl<C: Channel> Provider for Tools<C> {
    fn tools(&self) -> Vec<Value> {
        let ticket = json!({ "type": "integer", "description": tr!("Ticket number", "Numéro du ticket") });
        let max_s = json!({ "type": "integer", "description": tr!(
            "Longest wait of this call in seconds; then the ticket keeps its place (resume with await_clearance)",
            "Attente maximale de cet appel en secondes ; le ticket garde ensuite sa place (reprise par await_clearance)"
        ) });
        vec![
            json!({
                "name": "request_clearance",
                "description": tr!(
                    "Asks for a slot before a heavy task and WAITS until it is your turn (can take minutes; progress is \
                     reported). Returns the ticket number and `cleared`. Interrupting the call leaves the ticket in the \
                     queue: resume with await_clearance. A refusal is not an error: lower the request.",
                    "Demande un créneau avant une tâche lourde et ATTEND ton tour (peut durer des minutes ; la progression \
                     est signalée). Rend le numéro du ticket et `cleared`. Interrompre l'appel laisse le ticket dans la \
                     file : reprends avec await_clearance. Un refus n'est pas une erreur : baisse la demande."
                ),
                "inputSchema": schema(json!({
                    "description": { "type": "string", "description": tr!("What the heavy task is", "Ce qu'est la tâche lourde") },
                    "side": { "type": "string", "enum": ["wsl", "windows"], "description": tr!("Where it runs (default wsl)", "Où elle tourne (wsl par défaut)") },
                    "ram": { "type": "string", "description": tr!("Peak RAM, e.g. \"6G\"", "RAM au pic, ex. « 6G »") },
                    "cpu": { "type": "integer", "description": tr!("Cores", "Cœurs") },
                    "duration": { "type": "string", "description": tr!("Expected duration, e.g. \"30m\" (the lease)", "Durée prévue, ex. « 30m » (le bail)") },
                    "max_s": max_s
                }), &["description"]),
            }),
            json!({
                "name": "await_clearance",
                "description": tr!(
                    "Resumes waiting for a ticket whose request was interrupted (Escape) or timed out: the ticket kept \
                     its place in the queue. Without `ticket`, resumes the last ticket requested in this session.",
                    "Reprend l'attente d'un ticket dont la demande a été interrompue (Échap) ou a expiré : le ticket a \
                     gardé sa place dans la file. Sans `ticket`, reprend le dernier ticket demandé dans cette session."
                ),
                "inputSchema": schema(json!({ "ticket": ticket, "max_s": max_s }), &[]),
            }),
            json!({
                "name": "release",
                "description": tr!("Releases the slot once the heavy task is done.", "Libère le créneau une fois la tâche lourde finie."),
                "inputSchema": schema(json!({ "ticket": ticket, "code": { "type": "integer", "description": tr!("Exit code of the task", "Code de sortie de la tâche") } }), &["ticket"]),
            }),
            json!({
                "name": "extend",
                "description": tr!(
                    "Extends an active ticket whose task takes longer than declared (the lease).",
                    "Prolonge un ticket actif dont la tâche dure plus que prévu (le bail)."
                ),
                "inputSchema": schema(json!({
                    "ticket": ticket,
                    "duration": { "type": "string", "description": tr!("Extra time, e.g. \"20m\"", "Temps en plus, ex. « 20m »") },
                    "reason": { "type": "string" }
                }), &["ticket", "duration", "reason"]),
            }),
            json!({
                "name": "cancel",
                "description": tr!("Withdraws a waiting ticket or releases an active one.", "Retire un ticket en attente ou libère un ticket actif."),
                "inputSchema": schema(json!({ "ticket": ticket }), &["ticket"]),
            }),
            json!({
                "name": "status",
                "description": tr!("Queue, free resources and the reason of each ticket.", "File, ressources libres et raison de chaque ticket."),
                "inputSchema": schema(json!({}), &[]),
            }),
        ]
    }

    fn call(&self, name: &str, arguments: &Value, call: &Call) -> Result<ToolResult, String> {
        match name {
            "request_clearance" => self.request_clearance(arguments, call),
            "await_clearance" => self.await_clearance(arguments, call),
            "release" => {
                let id = ticket_arg(arguments)?;
                let body = Release { code: arguments["code"].as_i64().map(|c| c as i32), peak_mb: None, duration_s: None };
                Ok(self.simple("POST", format!("/tickets/{id}/release"), serde_json::to_value(body).map_err(|e| e.to_string())?))
            }
            "extend" => {
                let id = ticket_arg(arguments)?;
                let duration_s = parse_duration(arguments["duration"].as_str().unwrap_or(""))
                    .ok_or_else(|| tr!("unreadable `duration` (e.g. \"20m\")", "`duration` illisible (ex. « 20m »)"))?;
                let reason = arguments["reason"].as_str().unwrap_or("").to_owned();
                let body = Extension { duration_s, reason };
                Ok(self.simple("POST", format!("/tickets/{id}/extend"), serde_json::to_value(body).map_err(|e| e.to_string())?))
            }
            "cancel" => {
                let id = ticket_arg(arguments)?;
                Ok(self.simple("DELETE", format!("/tickets/{id}"), Value::Null))
            }
            "status" => Ok(self.status()),
            other => Err(tr!("unknown tool: {other}", "outil inconnu : {other}")),
        }
    }
}

#[cfg(test)]
mod tests;
