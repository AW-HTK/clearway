//! Minimal MCP server over stdio (JSON-RPC 2.0, one message per line): initialize, ping, tools/list, tools/call,
//! notifications/cancelled and notifications/progress. Stdout carries the protocol only.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// Known protocol versions, newest first.
pub const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// First version whose tool results carry `structuredContent`.
const STRUCTURED_SINCE: &str = "2025-06-18";

pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub instructions: String,
}

/// Result of a tool; `after` runs only once the response line has been written (clearance acknowledgement).
pub struct ToolResult {
    pub text: String,
    pub structured: Option<Value>,
    pub is_error: bool,
    pub after: Option<Box<dyn FnOnce() + Send>>,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> ToolResult {
        ToolResult { text: text.into(), structured: None, is_error: false, after: None }
    }

    pub fn error(text: impl Into<String>) -> ToolResult {
        ToolResult { text: text.into(), structured: None, is_error: true, after: None }
    }
}

pub trait Provider: Send + Sync + 'static {
    /// MCP tool definitions (`name`, `description`, `inputSchema`).
    fn tools(&self) -> Vec<Value>;
    /// `Err`: unknown tool or invalid arguments (protocol error); a failure of the tool itself is a `ToolResult::error`.
    fn call(&self, name: &str, arguments: &Value, call: &Call) -> Result<ToolResult, String>;
}

struct Output {
    stream: Mutex<Box<dyn Write + Send>>,
}

impl Output {
    fn send(&self, message: &Value) -> bool {
        let mut line = message.to_string();
        line.push('\n');
        let mut s = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        s.write_all(line.as_bytes()).and_then(|_| s.flush()).is_ok()
    }
}

/// A tool call in progress: cancellation and progress.
pub struct Call {
    cancelled: Arc<AtomicBool>,
    output: Arc<Output>,
    token: Option<Value>,
    steps: AtomicU64,
}

impl Call {
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Progress notification, when the client gave a `progressToken`; it also resets the client's idle timeout.
    pub fn progress(&self, message: &str) {
        let Some(token) = &self.token else { return };
        let step = self.steps.fetch_add(1, Ordering::SeqCst) + 1;
        self.output.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": { "progressToken": token, "progress": step, "message": message }
        }));
    }
}

fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

type InFlight = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

struct Session {
    provider: Arc<dyn Provider>,
    output: Arc<Output>,
    in_flight: InFlight,
    version: Arc<Mutex<String>>,
}

/// Serves the protocol until the end of `input`; calls still in progress are then cancelled.
pub fn serve(
    provider: Arc<dyn Provider>,
    info: &ServerInfo,
    input: impl BufRead,
    output: impl Write + Send + 'static,
    log: &(dyn Fn(&str) + Sync),
) {
    let session = Session {
        provider,
        output: Arc::new(Output { stream: Mutex::new(Box::new(output)) }),
        in_flight: Arc::default(),
        version: Arc::new(Mutex::new(STRUCTURED_SINCE.to_owned())),
    };
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let messages = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => batch,
            Ok(m) => vec![m],
            Err(e) => {
                session.output.send(&error(&Value::Null, -32700, &format!("unreadable JSON: {e}")));
                continue;
            }
        };
        for m in messages {
            dispatch(&session, info, m, log);
        }
    }
    for c in session.in_flight.lock().unwrap_or_else(|e| e.into_inner()).values() {
        c.store(true, Ordering::SeqCst);
    }
    log("end of stdin: calls in progress cancelled");
}

fn dispatch(session: &Session, info: &ServerInfo, m: Value, log: &(dyn Fn(&str) + Sync)) {
    let method = m["method"].as_str().unwrap_or("").to_owned();
    let params = m.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = m.get("id").cloned() else {
        if method == "notifications/cancelled" {
            let target = params["requestId"].to_string();
            if let Some(c) = session.in_flight.lock().unwrap_or_else(|e| e.into_inner()).get(&target) {
                c.store(true, Ordering::SeqCst);
                log(&format!("call {target} cancelled ({})", params["reason"]));
            }
        }
        return;
    };
    match method.as_str() {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let chosen = if VERSIONS.contains(&asked) { asked } else { VERSIONS[0] };
            *session.version.lock().unwrap_or_else(|e| e.into_inner()) = chosen.to_owned();
            log(&format!("initialize: client {}, version {asked:?} → {chosen}", params["clientInfo"]));
            session.output.send(&result(
                &id,
                json!({
                    "protocolVersion": chosen,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": info.name, "version": info.version },
                    "instructions": info.instructions
                }),
            ));
        }
        "ping" => {
            session.output.send(&result(&id, json!({})));
        }
        "tools/list" => {
            session.output.send(&result(&id, json!({ "tools": session.provider.tools() })));
        }
        "tools/call" => call_tool(session, id, params),
        other => {
            session.output.send(&error(&id, -32601, &format!("unknown method: {other}")));
        }
    }
}

/// Each tool call runs on its own thread, so that ping, cancellation and other calls are served meanwhile.
fn call_tool(session: &Session, id: Value, params: Value) {
    let key = id.to_string();
    let cancelled = Arc::new(AtomicBool::new(false));
    session.in_flight.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), cancelled.clone());
    let call = Call {
        cancelled,
        output: session.output.clone(),
        token: params["_meta"].get("progressToken").cloned(),
        steps: AtomicU64::new(0),
    };
    let (provider, output, in_flight, version) =
        (session.provider.clone(), session.output.clone(), session.in_flight.clone(), session.version.clone());
    let fallback = output.clone();
    let id_copy = id.clone();
    let launched = std::thread::Builder::new().stack_size(512 * 1024).spawn(move || {
        let name = params["name"].as_str().unwrap_or("");
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let message = match provider.call(name, &arguments, &call) {
            Err(e) => Some((error(&id, -32602, &e), None)),
            // MCP asks not to answer a cancelled request; the after-write action does not run either.
            Ok(_) if call.cancelled() => None,
            Ok(r) => {
                let mut body = json!({ "content": [{ "type": "text", "text": r.text }], "isError": r.is_error });
                let structured = version.lock().unwrap_or_else(|e| e.into_inner()).as_str() >= STRUCTURED_SINCE;
                if let (Some(s), true) = (r.structured, structured) {
                    body["structuredContent"] = s;
                }
                Some((result(&id, body), r.after))
            }
        };
        in_flight.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        if let Some((message, after)) = message
            && output.send(&message)
            && let Some(after) = after
        {
            after();
        }
    });
    if launched.is_err() {
        fallback.send(&error(&id_copy, -32603, "could not start a thread for the call"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    use std::sync::mpsc;
    use std::time::Duration;

    struct Echo;

    impl Provider for Echo {
        fn tools(&self) -> Vec<Value> {
            vec![json!({ "name": "echo", "inputSchema": { "type": "object" } })]
        }

        fn call(&self, name: &str, arguments: &Value, _call: &Call) -> Result<ToolResult, String> {
            match name {
                "echo" => Ok(ToolResult { structured: Some(arguments.clone()), ..ToolResult::text("echo") }),
                _ => Err(format!("unknown tool: {name}")),
            }
        }
    }

    fn session() -> (std::io::PipeWriter, mpsc::Receiver<Value>, std::thread::JoinHandle<()>) {
        let (input_reader, input_writer) = std::io::pipe().unwrap();
        let (output_reader, output_writer) = std::io::pipe().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output_reader).lines().map_while(Result::ok) {
                let _ = tx.send(serde_json::from_str(&line).unwrap());
            }
        });
        let info = ServerInfo { name: "test".into(), version: "0".into(), instructions: "…".into() };
        let server = std::thread::spawn(move || serve(Arc::new(Echo), &info, BufReader::new(input_reader), output_writer, &|_| {}));
        (input_writer, rx, server)
    }

    fn send(w: &mut std::io::PipeWriter, m: Value) {
        writeln!(w, "{m}").unwrap();
    }

    #[test]
    fn protocol_errors_and_versions() {
        let (mut w, rx, server) = session();
        let next = || rx.recv_timeout(Duration::from_secs(5)).unwrap();
        writeln!(w, "{{not json").unwrap();
        assert_eq!(next()["error"]["code"], -32700);
        send(&mut w, json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" }));
        assert_eq!(next()["error"]["code"], -32601);
        send(&mut w, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "nope" } }));
        assert_eq!(next()["error"]["code"], -32602);
        send(&mut w, json!({ "jsonrpc": "2.0", "id": 3, "method": "initialize", "params": { "protocolVersion": "2024-11-05" } }));
        assert_eq!(next()["result"]["protocolVersion"], "2024-11-05");
        send(&mut w, json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "echo", "arguments": { "a": 1 } } }));
        let r = next();
        assert!(r["result"].get("structuredContent").is_none(), "no structured content before 2025-06-18: {r}");
        send(&mut w, json!([{ "jsonrpc": "2.0", "id": 5, "method": "ping" }, { "jsonrpc": "2.0", "method": "notifications/initialized" }]));
        assert_eq!(next()["id"], 5);
        drop(w);
        server.join().unwrap();
    }
}
