# dsview-mcp

An [MCP](https://modelcontextprotocol.io) server that lets an AI agent (Claude Code, or any other MCP
client) drive a **running DSView**. It can configure and start captures, read edges and waveforms,
run protocol decoders and read their annotations, take measurements, save and export files, and
move the DSView view to show the user what it found.

```
MCP client ──stdio──> dsview-mcp ──JSON-RPC over a Unix socket──> DSView --mcp
```

DSView does the actual work; this binary only translates MCP tool calls, checks file paths and
applies timeouts. Everything the agent does appears live in the DSView window.

## Build

```sh
# DSView itself (from the repository root); the MCP endpoint needs Qt Network
/usr/bin/cmake -S . -B build && /usr/bin/cmake --build build -j

# the MCP server
cd dsview-mcp && cargo build --release
```

## Use with Claude Code

1. Start DSView with the control socket enabled: `DSView --mcp`. If DSView isn't running,
   dsview-mcp starts `DSView --mcp` itself on the first tool call (see `DSVIEW_BIN`).
2. Register the server:

   ```sh
   claude mcp add dsview -- /path/to/DSView/dsview-mcp/target/release/dsview-mcp
   ```

3. Ask for things like "capture 1M samples on the demo device, decode I²C on channels 0/1 and list
   the addresses that were written".

## Tools

| Group | Tools |
|---|---|
| Device / capture | `list_devices`, `select_device`, `get_config`, `set_mode`, `set_sampling`, `set_channels`, `set_trigger`, `set_threshold`, `set_dso`, `start_capture`, `stop_capture`, `status` |
| Readout | `capture_info`, `get_edges`, `read_logic`, `read_waveform` |
| Decoders | `list_decoders`, `describe_decoder`, `add_decoder`, `list_active_decoders`, `remove_decoder`, `run_decoders`, `get_annotations` |
| Files, measurements, view | `open_file`, `save_session`, `export_data`, `measure_logic`, `measure_dso`, `show_region`, `set_cursors` |

- Positions are sample indexes into the capture; results also give `time_ns` where useful.
- `start_capture`, `add_decoder` and `run_decoders` wait for completion by default.
- Large results are paged or decimated (`limit`/`next_start`, `offset`/`next_offset`, `max_points`).

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `DSVIEW_MCP_ALLOWED_DIRS` | `~/DSView` | Colon-separated directories that `open_file`, `save_session` and `export_data` may use. Relative paths resolve against the first one. |
| `DSVIEW_MCP_SOCKET` | `$XDG_RUNTIME_DIR/dsview/mcp.sock` | Control socket path. It must be the same for DSView and dsview-mcp. |
| `DSVIEW_BIN` | `DSView` | Executable to launch when DSView isn't running. |
| `DSVIEW_MCP_NO_LAUNCH` | unset | Set to `1` to never launch DSView. |

## Security model

- **Off by default.** DSView only listens when started with `--mcp`.
- **No network.** The control channel is a Unix socket in a `0700` directory, with the socket
  itself owner-only. Only processes of the same user can connect. No TCP port is ever opened.
- **File access is confined.** Every path is canonicalized and must lie inside an allowed
  directory, so `..` and symlinked directories can't escape. An existing symlink is never written
  through, and extensions are restricted (`.dsl` for open/save, `.csv`/`.vcd` for export).
- **No surprise dialogs.** State-changing calls are refused while DSView shows a modal dialog. The
  error names the dialog, so the agent can ask the user to close it.
- **Real hardware is affected.** A capture on a real device acts on the user's circuit. The server
  tells the agent to confirm setup changes the user didn't ask for.
- **The socket itself is trusted.** Any process running as the same user can talk to the socket
  directly and bypass the path checks here. This is the same trust level as that user running
  DSView themselves.

## Tests

```sh
cargo test        # path policy and socket framing
```
