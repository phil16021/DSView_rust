//! Client for DSView's local control socket (`DSView --mcp`).
//!
//! Each call opens its own connection and sends one newline-delimited
//! JSON-RPC 2.0 request, so long-running calls (captures, decoding) never
//! block other tool calls.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

/// Largest response accepted from DSView.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
/// How long to wait for a freshly launched DSView to open its socket.
const LAUNCH_WAIT: Duration = Duration::from_secs(20);

pub struct Bridge {
    socket: PathBuf,
    launch: Option<String>,
    launch_lock: Mutex<()>,
    next_id: AtomicU64,
}

/// Same lookup order as `ControlServer::default_socket_path()` in DSView.
pub fn default_socket_path() -> PathBuf {
    if let Some(custom) = std::env::var_os("DSVIEW_MCP_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(custom);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| home_dir().join(".local/state"));
    base.join("dsview/mcp.sock")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl Bridge {
    pub fn from_env() -> Self {
        let no_launch = std::env::var("DSVIEW_MCP_NO_LAUNCH").is_ok_and(|v| v == "1");
        let launch =
            (!no_launch).then(|| std::env::var("DSVIEW_BIN").unwrap_or_else(|_| "DSView".into()));
        Self::new(default_socket_path(), launch)
    }

    pub fn new(socket: PathBuf, launch: Option<String>) -> Self {
        Self {
            socket,
            launch,
            launch_lock: Mutex::new(()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Sends one request and returns its `result`, or the error message.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let stream = self.connect().await?;
        let (read, mut write) = stream.into_split();

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        line.push(b'\n');
        write
            .write_all(&line)
            .await
            .map_err(|e| format!("sending to DSView failed: {e}"))?;

        let mut reader = BufReader::new(read).take(MAX_RESPONSE_BYTES);
        let mut response = String::new();
        match tokio::time::timeout(timeout, reader.read_line(&mut response)).await {
            Err(_) => {
                return Err(format!(
                    "DSView did not answer '{method}' within {}s",
                    timeout.as_secs()
                ));
            }
            Ok(Err(e)) => return Err(format!("reading from DSView failed: {e}")),
            Ok(Ok(0)) => return Err("DSView closed the connection (did it exit?)".into()),
            Ok(Ok(_)) => {}
        }
        parse_response(&response, id)
    }

    async fn connect(&self) -> Result<UnixStream, String> {
        if let Ok(stream) = UnixStream::connect(&self.socket).await {
            return Ok(stream);
        }
        let Some(bin) = &self.launch else {
            return Err(self.not_running());
        };

        // Only one tool call launches DSView; the others wait for its socket.
        let _guard = self.launch_lock.lock().await;
        if let Ok(stream) = UnixStream::connect(&self.socket).await {
            return Ok(stream);
        }

        launch(bin)?;
        let deadline = tokio::time::Instant::now() + LAUNCH_WAIT;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Ok(stream) = UnixStream::connect(&self.socket).await {
                return Ok(stream);
            }
        }
        Err(format!(
            "started '{bin} --mcp' but its control socket did not appear at {} within {}s",
            self.socket.display(),
            LAUNCH_WAIT.as_secs()
        ))
    }

    fn not_running(&self) -> String {
        format!(
            "DSView is not running with MCP control enabled (no socket at {}). \
             Ask the user to start it with: DSView --mcp",
            self.socket.display()
        )
    }
}

/// Starts DSView detached; it keeps running when this server exits.
fn launch(bin: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;

    let mut child = Command::new(bin)
        .arg("--mcp")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            format!("could not start '{bin} --mcp': {e} (set DSVIEW_BIN to the DSView executable)")
        })?;
    // Reap it if it exits while we are still running.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    eprintln!("dsview-mcp: launched {bin} --mcp");
    Ok(())
}

fn parse_response(line: &str, id: u64) -> Result<Value, String> {
    let mut response: Value = serde_json::from_str(line.trim())
        .map_err(|e| format!("invalid response from DSView: {e}"))?;
    if response.get("id").and_then(Value::as_u64) != Some(id) {
        return Err("DSView answered a different request".into());
    }
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(message.to_string());
    }
    Ok(response
        .get_mut("result")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[test]
    fn parses_result_and_errors() {
        let ok = parse_response(r#"{"jsonrpc":"2.0","id":7,"result":{"a":1}}"#, 7).unwrap();
        assert_eq!(ok, json!({"a": 1}));

        let err = parse_response(
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"no device"}}"#,
            7,
        );
        assert_eq!(err.unwrap_err(), "no device");

        assert!(parse_response(r#"{"jsonrpc":"2.0","id":8,"result":1}"#, 7).is_err());
        assert!(parse_response("not json", 7).is_err());
    }

    #[tokio::test]
    async fn round_trip_over_unix_socket() {
        let dir = std::env::temp_dir().join(format!("dsview-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();

        // Fake DSView: echo the method back as the result.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let resp = json!({"jsonrpc": "2.0", "id": req["id"], "result": {"method": req["method"], "params": req["params"]}});
            write
                .write_all(format!("{resp}\n").as_bytes())
                .await
                .unwrap();
        });

        let bridge = Bridge::new(path.clone(), None);
        let result = bridge
            .call("get_config", json!({"x": 1}), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result, json!({"method": "get_config", "params": {"x": 1}}));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn missing_socket_without_launch_is_an_error() {
        let bridge = Bridge::new(PathBuf::from("/nonexistent/dsview/mcp.sock"), None);
        let err = bridge
            .call("version", json!({}), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(err.contains("DSView --mcp"), "{err}");
    }
}
