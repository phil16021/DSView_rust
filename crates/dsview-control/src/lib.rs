//! DSView's control API without Qt: the local socket server of
//! `pv/mcp/controlserver.cpp` ([`server`]) and the JSON conventions of its
//! answers ([`json`]). What answers the calls is a [`ControlBackend`]; with
//! the `api` feature (default), [`api::Api`] answers them on the Rust capture
//! session.

#[cfg(feature = "api")]
pub mod api;
pub mod json;
pub mod server;

use serde_json::{Map, Value};

/// Answers control calls: `Ok(result)` or `Err(message)`.
pub trait ControlBackend: Send + Sync {
    fn call(&self, method: &str, params: &Map<String, Value>) -> Result<Value, String>;
}

/// `ControlServer::default_socket_path()`: `DSVIEW_MCP_SOCKET`, else
/// `$XDG_RUNTIME_DIR/dsview/mcp.sock` (when that directory exists), else
/// `~/.local/state/dsview/mcp.sock`.
pub fn default_socket_path() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(p) = std::env::var_os("DSVIEW_MCP_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("dsview/mcp.sock")
}
