//! The control socket (`pv/mcp/controlserver.cpp`): newline-delimited
//! JSON-RPC 2.0 over a Unix socket. A connection may send any number of
//! requests; each line is answered on its own (requests on one connection
//! one after another, connections in parallel).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::ControlBackend;
use crate::json::obj;

/// `ControlServer::MaxLineBytes`
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const SERVER_ERROR: i64 = -32000;

/// A listening control socket; dropping it closes the socket.
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// `ControlServer::start()` on `path`: refuses a path too long for
    /// `sockaddr_un`, creates the directory (owner-only), and never takes
    /// over a socket another instance still listens on.
    pub fn start(path: &Path, backend: Arc<dyn ControlBackend>) -> Result<Server, String> {
        let shown = path.display();
        // sockaddr_un.sun_path holds 108 bytes including the terminator.
        if path.as_os_str().len() > 107 {
            return Err(format!(
                "MCP: socket path is too long (max 107 bytes): {shown}"
            ));
        }
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|_| format!("MCP: cannot create socket directory {}", dir.display()))?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        if probe(path) {
            return Err(format!(
                "MCP: another DSView instance already listens on {shown}"
            ));
        }
        let _ = std::fs::remove_file(path);
        let listener =
            UnixListener::bind(path).map_err(|e| format!("MCP: cannot listen on {shown}: {e}"))?;
        // QLocalServer::UserAccessOption
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let thread = std::thread::Builder::new()
            .name("control".into())
            .spawn(move || accept_loop(listener, backend, st))
            .map_err(|e| e.to_string())?;
        Ok(Server {
            path: path.to_owned(),
            stop,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the accept loop
        let _ = UnixStream::connect(&self.path);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether something answers on `path` (QLocalSocket::waitForConnected(300)).
fn probe(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

fn accept_loop(listener: UnixListener, backend: Arc<dyn ControlBackend>, stop: Arc<AtomicBool>) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(conn) = conn else { continue };
        let backend = backend.clone();
        let _ = std::thread::Builder::new()
            .name("control-conn".into())
            .spawn(move || serve(conn, &*backend));
    }
}

/// Reads one line of at most [`MAX_LINE_BYTES`] (plus the newline):
/// `Ok(None)` at the end of the stream, `Err(())` when the line is too long.
fn read_line(r: &mut impl BufRead) -> std::io::Result<Option<Result<Vec<u8>, ()>>> {
    let mut line = Vec::new();
    let n = r
        .by_ref()
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if line.len() > MAX_LINE_BYTES {
        return Ok(Some(Err(())));
    }
    // a final line without its newline: Qt never reads it (canReadLine)
    else {
        return Ok(None);
    }
    Ok(Some(Ok(line)))
}

fn serve(conn: UnixStream, backend: &dyn ControlBackend) {
    let _ = conn.set_write_timeout(Some(Duration::from_secs(30)));
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    let mut r = BufReader::new(conn);
    loop {
        match read_line(&mut r) {
            Ok(Some(Ok(line))) => {
                let line = line.trim_ascii();
                if line.is_empty() {
                    continue;
                }
                if let Some(reply) = handle_line(line, backend)
                    && send(&mut out, &reply).is_err()
                {
                    return;
                }
            }
            Ok(Some(Err(()))) => {
                let _ = send(
                    &mut out,
                    &error(Value::Null, INVALID_REQUEST, "request too large"),
                );
                return;
            }
            Ok(None) | Err(_) => return,
        }
    }
}

fn send(out: &mut UnixStream, msg: &Value) -> std::io::Result<()> {
    let mut data = serde_json::to_vec(msg).unwrap_or_default();
    data.push(b'\n');
    out.write_all(&data)?;
    out.flush()
}

fn error(id: Value, code: i64, message: &str) -> Value {
    obj([
        (
            "error",
            obj([("code", code.into()), ("message", message.into())]),
        ),
        ("id", id),
        ("jsonrpc", "2.0".into()),
    ])
}

/// One request line: the reply, or `None` for a notification.
pub fn handle_line(line: &[u8], backend: &dyn ControlBackend) -> Option<Value> {
    let req = match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(o)) => o,
        _ => return Some(error(Value::Null, PARSE_ERROR, "invalid JSON")),
    };
    // a missing id is answered as null (QJsonValue Undefined → null)
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = match req.get("params") {
        None => Some(Map::new()),
        Some(Value::Object(p)) => Some(p.clone()),
        Some(_) => None,
    };
    let (Some(params), true, false) = (
        params,
        req.get("jsonrpc").and_then(Value::as_str) == Some("2.0"),
        method.is_empty(),
    ) else {
        return Some(error(
            id,
            INVALID_REQUEST,
            "expected a JSON-RPC 2.0 request with object params",
        ));
    };
    let result = backend.call(method, &params);
    if !req.contains_key("id") {
        return None;
    }
    Some(match result {
        Ok(v) => obj([
            ("id", id),
            ("jsonrpc", "2.0".into()),
            ("result", crate::json::qt(v)),
        ]),
        Err(e) => error(id, SERVER_ERROR, &e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Echo;
    impl ControlBackend for Echo {
        fn call(&self, method: &str, params: &Map<String, Value>) -> Result<Value, String> {
            match method {
                "echo" => Ok(Value::Object(params.clone())),
                m => Err(format!("unknown method: {m}")),
            }
        }
    }

    fn line(s: &str) -> Option<Value> {
        handle_line(s.as_bytes(), &Echo)
    }

    #[test]
    fn requests_and_errors() {
        assert_eq!(
            line(r#"{"jsonrpc":"2.0","id":1,"method":"echo","params":{"x":2.0}}"#),
            Some(json!({"jsonrpc":"2.0","id":1,"result":{"x":2}}))
        );
        assert_eq!(
            line("{nope"),
            Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"invalid JSON"}})
            )
        );
        assert_eq!(
            line(r#"{"jsonrpc":"2.0","id":"a","method":"echo","params":[1]}"#),
            Some(json!({"jsonrpc":"2.0","id":"a","error":{"code":-32600,
                "message":"expected a JSON-RPC 2.0 request with object params"}}))
        );
        assert_eq!(
            line(r#"{"jsonrpc":"2.0","id":3,"method":"x"}"#),
            Some(
                json!({"jsonrpc":"2.0","id":3,"error":{"code":-32000,"message":"unknown method: x"}})
            )
        );
        assert_eq!(line(r#"{"jsonrpc":"2.0","method":"echo"}"#), None);
    }

    #[test]
    fn socket_round_trip_and_no_takeover() {
        let dir = std::env::temp_dir().join(format!("dsvctl-{}", std::process::id()));
        let path = dir.join("s.sock");
        let srv = Server::start(&path, Arc::new(Echo)).unwrap();
        assert!(Server::start(&path, Arc::new(Echo)).is_err());
        let mut c = UnixStream::connect(&path).unwrap();
        c.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"echo\"}\n\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"echo\"}\n").unwrap();
        let mut r = BufReader::new(c.try_clone().unwrap());
        for want in [1, 2] {
            let mut s = String::new();
            r.read_line(&mut s).unwrap();
            let v: Value = serde_json::from_str(&s).unwrap();
            assert_eq!(v["id"], want);
        }
        // too long: an error, then the connection closes
        let big = vec![b' '; MAX_LINE_BYTES + 10];
        let _ = c.write_all(&big);
        let mut s = String::new();
        r.read_line(&mut s).unwrap();
        assert!(s.contains("request too large"), "{s}");
        drop(srv);
        assert!(!path.exists());
        let _ = std::fs::remove_dir(&dir);
    }
}
