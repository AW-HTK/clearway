use std::collections::BTreeMap;
use std::io::{self, BufRead, Read};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_PIPE: &str = "clearway";
/// Pipe of the pre-Clearway server, still served for clients installed before the rename.
pub const LEGACY_PIPE: &str = "orchestrateur";
pub const MAX_LINE: usize = 256 * 1024;

/// Exit codes of `clearway.exe relay`.
pub const RELAY_ABSENT: i32 = 69;
pub const RELAY_LOST: i32 = 74;

/// One JSON request per line: the logical routes, without HTTP.
/// With an `id` it runs alongside the others and its response carries the same `id`, in any order;
/// without one it completes before the next line is read (old clients).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Value,
}

impl Request {
    pub fn new(method: &str, path: &str, body: Value) -> Request {
        Request { id: None, method: method.into(), path: path.into(), params: BTreeMap::new(), body }
    }

    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').filter(|s| !s.is_empty()).collect()
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub code: u16,
    #[serde(default)]
    pub body: Value,
}

/// Reads a line of at most `MAX_LINE` bytes; `None` at end of stream.
pub fn read_line_limited(r: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut s = String::new();
    let n = r.take(MAX_LINE as u64 + 1).read_line(&mut s)?;
    if n == 0 {
        return Ok(None);
    }
    if n > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    Ok(Some(s.trim_end_matches(['\r', '\n']).to_owned()))
}

/// Outside Windows (tests), the named pipe is replaced by a Unix socket.
#[cfg(unix)]
pub fn socket_path(pipe: &str) -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("clearway-{pipe}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut r = Request::new("GET", "/tickets/12/wait", Value::Null);
        r.params.insert("known".into(), "waiting-1".into());
        let line = serde_json::to_string(&r).unwrap();
        let mut input = format!("{line}\n").into_bytes();
        input.extend_from_slice(b"{\"method\":\"GET\",\"path\":\"/status\"}\n");
        let mut reader = &input[..];
        let read: Request = serde_json::from_str(&read_line_limited(&mut reader).unwrap().unwrap()).unwrap();
        assert_eq!(read.segments(), vec!["tickets", "12", "wait"]);
        assert_eq!(read.param("known"), Some("waiting-1"));
        let two: Request = serde_json::from_str(&read_line_limited(&mut reader).unwrap().unwrap()).unwrap();
        assert_eq!(two.path, "/status");
        assert!(read_line_limited(&mut reader).unwrap().is_none());
        assert!(!line.contains("\"id\""), "without an id, nothing is added for old servers: {line}");
        let with: Request = serde_json::from_str("{\"id\":7,\"method\":\"GET\",\"path\":\"/status\"}").unwrap();
        assert_eq!(with.id, Some(7));
    }

    #[test]
    fn line_too_long() {
        let raw = vec![b'a'; MAX_LINE + 10];
        assert!(read_line_limited(&mut &raw[..]).is_err());
    }
}
