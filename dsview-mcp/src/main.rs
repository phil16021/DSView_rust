//! dsview-mcp: an MCP server (stdio) that controls a running DSViewR.
//!
//! DSView must run with `--mcp`, which opens a per-user Unix socket; this
//! process translates MCP tool calls into requests on that socket and is
//! started by the MCP client (e.g. `claude mcp add dsview -- dsview-mcp`).

mod bridge;
mod paths;
mod tools;

use rmcp::ServiceExt;

const HELP: &str = "\
dsview-mcp - MCP server for DSViewR (stdio transport)

Start DSViewR with --mcp (or let this server launch it), then register:
  claude mcp add dsview -- /path/to/dsview-mcp

Environment:
  DSVIEW_MCP_ALLOWED_DIRS  colon-separated directories for open/save/export (default ~/DSView)
  DSVIEW_MCP_SOCKET        control socket path (default $XDG_RUNTIME_DIR/dsview/mcp.sock)
  DSVIEW_BIN               DSViewR executable to launch when it is not running (default DSViewR;
                           dsviewr-cli runs as `dsviewr-cli serve --mcp`, DSViewR without a GUI)
  DSVIEW_MCP_NO_LAUNCH=1   never launch DSViewR
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("dsview-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other:?} (see --help)"),
        }
    }

    let paths = paths::PathPolicy::from_env().map_err(anyhow::Error::msg)?;
    let bridge = bridge::Bridge::from_env();

    // stdout carries the MCP protocol; diagnostics go to stderr.
    eprintln!(
        "dsview-mcp {}: socket {}, allowed dirs {:?}",
        env!("CARGO_PKG_VERSION"),
        bridge.socket_path().display(),
        paths.roots()
    );

    let server = tools::DsviewServer::new(bridge, paths)
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}
