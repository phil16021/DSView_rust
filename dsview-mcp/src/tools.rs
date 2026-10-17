//! MCP tools. Each one maps to a method of DSView's control socket; this
//! layer adds schemas, descriptions, the file path policy and timeouts.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bridge::Bridge;
use crate::paths::{Access, PathPolicy};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Extra time on top of a call's own timeout, so DSView reports the timeout itself.
const TIMEOUT_MARGIN: u64 = 15;

#[derive(Clone)]
pub struct DsviewServer {
    bridge: Arc<Bridge>,
    paths: Arc<PathPolicy>,
    tool_router: ToolRouter<Self>,
}

fn ok(value: Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value.to_string())])
}

fn fail(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// Serializes tool parameters, dropping absent optional fields.
fn params<T: Serialize>(p: &T) -> Value {
    serde_json::to_value(p).unwrap_or_else(|_| json!({}))
}

fn wait_timeout(timeout_s: Option<f64>, default_s: u64) -> Duration {
    let s = timeout_s.unwrap_or(default_s as f64).clamp(1.0, 3600.0) as u64;
    Duration::from_secs(s + TIMEOUT_MARGIN)
}

// ---------------------------------------------------------------- parameters

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct IndexParam {
    /// Index as returned by the matching list tool.
    pub index: i64,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ModeParams {
    /// "logic" (logic analyzer), "dso" (oscilloscope) or "analog" (data acquisition).
    pub mode: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SamplingParams {
    /// Sample rate in Hz; must be one of `available_samplerates` from get_config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samplerate: Option<u64>,
    /// Number of samples to capture (logic mode). DSView rounds it to its depth steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ChannelSetting {
    /// Channel index (see get_config).
    pub index: i64,
    /// Enable or disable the channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// New display name (max 31 characters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ChannelsParams {
    /// Channels to change.
    pub channels: Vec<ChannelSetting>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TriggerParams {
    /// "none", "rising", "falling", "high", "low" or "edge" (any edge).
    #[serde(rename = "type")]
    pub kind: String,
    /// Logic channel index to trigger on (required unless type is "none"). Other channels' triggers are cleared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<i64>,
    /// Where the trigger sits in the capture, 0-100 percent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_percent: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ThresholdParams {
    /// Logic threshold voltage in volts (0-5), on devices that support it.
    pub volts: f64,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DsoParams {
    /// Horizontal timebase in nanoseconds per division.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timebase_ns: Option<u64>,
    /// Oscilloscope channel for vdiv_mv/coupling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<i64>,
    /// Vertical scale in millivolts per division; the nearest available step is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vdiv_mv: Option<u64>,
    /// "dc", "ac" or "gnd".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coupling: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CaptureParams {
    /// Single-shot capture. Defaults to true in dso mode (otherwise it runs until stopped) and false in logic mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instant: Option<bool>,
    /// Wait until the capture (and automatic re-decoding) finishes. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<bool>,
    /// Maximum seconds to wait (default 60).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EdgesParams {
    /// Logic channel index.
    pub channel: i64,
    /// First sample index (default 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// Last sample index, inclusive (default: end of capture).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    /// Maximum edges to return (default 1000, max 100000). Continue from `next_start` when truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadLogicParams {
    /// Logic channel indexes (default: all captured channels).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<Vec<i64>>,
    /// First sample index (default 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// Last sample index, inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    /// Maximum transitions per channel (default 2000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_transitions: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WaveformParams {
    /// Oscilloscope channel index.
    pub channel: i64,
    /// First sample index (default 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// Last sample index, inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    /// Maximum points (default 1000, max 20000). Larger ranges return min/max per point.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_points: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FilterParams {
    /// Case-insensitive text matched against id, name and tags, e.g. "i2c", "uart", "spi".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DecoderIdParams {
    /// Decoder id from list_decoders, e.g. "1:i2c".
    pub id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct StackedDecoder {
    /// Decoder id; its inputs must match the previous decoder's outputs.
    pub id: String,
    /// Option values by option id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AddDecoderParams {
    /// Decoder id taking logic input, e.g. "1:i2c" or "1:uart".
    pub id: String,
    /// Decoder channel id -> logic channel index, e.g. {"scl": 1, "sda": 0}. All required channels must be mapped.
    pub channels: BTreeMap<String, i64>,
    /// Option values by option id (see describe_decoder), e.g. {"baudrate": 115200}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, Value>,
    /// Higher-level decoders stacked on top, e.g. [{"id": "eeprom24xx"}].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<StackedDecoder>,
    /// Decode the current capture right away and wait for it (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RunDecodersParams {
    /// Only this decoder (index from list_active_decoders); default all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    /// Maximum seconds to wait (default 120).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnnotationsParams {
    /// Decoder index from list_active_decoders.
    pub decoder: i64,
    /// Only rows whose title contains this text or whose id equals it, e.g. "addr-data".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// First sample index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// Last sample index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    /// Skip this many annotations (paging; use `next_offset`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Maximum annotations (default 500, max 5000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PathParams {
    /// File path: absolute, "~/...", or relative to the first allowed directory.
    pub path: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExportParams {
    /// Output file path ending in .csv or .vcd (absolute, "~/...", or relative to the first allowed directory).
    pub path: String,
    /// "csv" or "vcd" (default: from the file extension). VCD needs logic data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MeasureLogicParams {
    /// Logic channel index.
    pub channel: i64,
    /// First sample index (default 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// Last sample index (default: end of capture).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ChannelParam {
    /// Channel index.
    pub channel: i64,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RegionParams {
    /// First sample index to show.
    pub start: u64,
    /// Last sample index to show (greater than start).
    pub end: u64,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CursorsParams {
    /// Sample indexes for cursors (max 16); an empty list removes all cursors.
    pub samples: Vec<u64>,
}

// ---------------------------------------------------------------- tools

impl DsviewServer {
    pub fn new(bridge: Bridge, paths: PathPolicy) -> Self {
        Self { bridge: Arc::new(bridge), paths: Arc::new(paths), tool_router: Self::tool_router() }
    }

    async fn call(&self, method: &str, params: Value, timeout: Duration) -> CallToolResult {
        match self.bridge.call(method, params, timeout).await {
            Ok(v) => ok(v),
            Err(e) => fail(e),
        }
    }

    fn instructions(&self) -> String {
        let roots: Vec<String> = self.paths.roots().iter().map(|r| r.display().to_string()).collect();
        format!(
            "Controls the DSView window the user is looking at (DreamSourceLab logic analyzers and \
             oscilloscopes, or the built-in demo devices).\n\
             Typical flow: get_config -> select_device/set_mode/set_sampling/set_channels/set_trigger -> \
             start_capture (waits for the data) -> get_edges, read_logic, measure_logic, or add_decoder + \
             get_annotations; in dso mode read_waveform and measure_dso.\n\
             Positions are sample indexes into the capture; time_ns = sample * 1e9 / samplerate.\n\
             Captures from real hardware act on the user's circuit; confirm with the user before changing \
             a setup they did not ask you to change.\n\
             Files can only be opened and written inside: {}.",
            roots.join(", ")
        )
    }
}

#[tool_router]
impl DsviewServer {
    #[tool(description = "List the devices DSView can use (hardware, demo devices, opened files). Use the index with select_device.")]
    async fn list_devices(&self) -> CallToolResult {
        self.call("list_devices", json!({}), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Make another device the active one in DSView.")]
    async fn select_device(&self, Parameters(p): Parameters<IndexParam>) -> CallToolResult {
        self.call("select_device", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Current device, mode, sample rate, sample count, channels, trigger and the allowed sample rates.")]
    async fn get_config(&self) -> CallToolResult {
        self.call("get_config", json!({}), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Switch the work mode: logic analyzer, oscilloscope (dso) or data acquisition (analog).")]
    async fn set_mode(&self, Parameters(p): Parameters<ModeParams>) -> CallToolResult {
        self.call("set_mode", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Set the sample rate and/or sample count. Returns the values actually applied.")]
    async fn set_sampling(&self, Parameters(p): Parameters<SamplingParams>) -> CallToolResult {
        self.call("set_sampling", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Enable/disable channels and rename them.")]
    async fn set_channels(&self, Parameters(p): Parameters<ChannelsParams>) -> CallToolResult {
        self.call("set_channels", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Set a simple logic trigger on one channel (or clear it with type none) and the trigger position.")]
    async fn set_trigger(&self, Parameters(p): Parameters<TriggerParams>) -> CallToolResult {
        self.call("set_trigger", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Set the logic input threshold voltage (devices with adjustable threshold only).")]
    async fn set_threshold(&self, Parameters(p): Parameters<ThresholdParams>) -> CallToolResult {
        self.call("set_threshold", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Oscilloscope settings: timebase, and per-channel V/div and coupling.")]
    async fn set_dso(&self, Parameters(p): Parameters<DsoParams>) -> CallToolResult {
        self.call("set_dso", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Start a capture with the current settings. By default waits until it is complete (including automatic protocol decoding) and returns a summary.")]
    async fn start_capture(&self, Parameters(p): Parameters<CaptureParams>) -> CallToolResult {
        let timeout = wait_timeout(p.timeout_s, 60);
        self.call("start_capture", params(&p), timeout).await
    }

    #[tool(description = "Stop a running capture.")]
    async fn stop_capture(&self) -> CallToolResult {
        self.call("stop_capture", json!({}), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Whether DSView is capturing, decoding or saving, any open dialog, and a summary of the current data.")]
    async fn status(&self) -> CallToolResult {
        self.call("capture_status", json!({}), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Summary of the captured data: sample count, sample rate, duration, trigger position, channels.")]
    async fn capture_info(&self) -> CallToolResult {
        self.call("capture_info", json!({}), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Edges (rising/falling) of one logic channel with sample index and time, paged.")]
    async fn get_edges(&self, Parameters(p): Parameters<EdgesParams>) -> CallToolResult {
        self.call("get_edges", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Several logic channels as run-length transitions: initial level plus the sample indexes where each channel toggles.")]
    async fn read_logic(&self, Parameters(p): Parameters<ReadLogicParams>) -> CallToolResult {
        self.call("read_logic", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Oscilloscope waveform of one channel in millivolts, decimated to max_points (min/max per point).")]
    async fn read_waveform(&self, Parameters(p): Parameters<WaveformParams>) -> CallToolResult {
        self.call("read_waveform", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Available protocol decoders (I2C, SPI, UART, CAN, ...), optionally filtered.")]
    async fn list_decoders(&self, Parameters(p): Parameters<FilterParams>) -> CallToolResult {
        self.call("list_decoders", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Channels, options (with defaults and allowed values) and annotation rows of a decoder.")]
    async fn describe_decoder(&self, Parameters(p): Parameters<DecoderIdParams>) -> CallToolResult {
        self.call("describe_decoder", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Add a protocol decoder (logic mode) with channel mapping, options and optional stacked decoders; by default decodes the current capture and waits.")]
    async fn add_decoder(&self, Parameters(p): Parameters<AddDecoderParams>) -> CallToolResult {
        self.call("add_decoder", params(&p), wait_timeout(None, 120)).await
    }

    #[tool(description = "Decoders currently configured in DSView, with progress, annotation counts and rows.")]
    async fn list_active_decoders(&self) -> CallToolResult {
        self.call("list_active_decoders", json!({}), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Remove a configured decoder by index.")]
    async fn remove_decoder(&self, Parameters(p): Parameters<IndexParam>) -> CallToolResult {
        self.call("remove_decoder", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Decode the current capture again (all decoders or one) and wait for the result.")]
    async fn run_decoders(&self, Parameters(p): Parameters<RunDecodersParams>) -> CallToolResult {
        let timeout = wait_timeout(p.timeout_s, 120);
        self.call("run_decoders", params(&p), timeout).await
    }

    #[tool(description = "Decoded annotations (e.g. I2C addresses and data bytes) of one decoder, sorted by time and paged.")]
    async fn get_annotations(&self, Parameters(p): Parameters<AnnotationsParams>) -> CallToolResult {
        self.call("get_annotations", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Open a saved .dsl capture in DSView (from the allowed directories) and wait until it is loaded.")]
    async fn open_file(&self, Parameters(p): Parameters<PathParams>) -> CallToolResult {
        let path = match self.paths.check(&p.path, Access::Read, &["dsl"]) {
            Ok(path) => path,
            Err(e) => return fail(e),
        };
        self.call("open_file", json!({"path": path}), wait_timeout(None, 120)).await
    }

    #[tool(description = "Save the current capture as a .dsl file (in the allowed directories).")]
    async fn save_session(&self, Parameters(p): Parameters<PathParams>) -> CallToolResult {
        let path = match self.paths.check(&p.path, Access::Write, &["dsl"]) {
            Ok(path) => path,
            Err(e) => return fail(e),
        };
        self.call("save_session", json!({"path": path}), wait_timeout(None, 300)).await
    }

    #[tool(description = "Export the current capture as CSV or VCD (in the allowed directories).")]
    async fn export_data(&self, Parameters(p): Parameters<ExportParams>) -> CallToolResult {
        let path = match self.paths.check(&p.path, Access::Write, &["csv", "vcd"]) {
            Ok(path) => path,
            Err(e) => return fail(e),
        };
        let mut args = json!({"path": path});
        if let Some(format) = p.format {
            args["format"] = json!(format);
        }
        self.call("export_data", args, wait_timeout(None, 300)).await
    }

    #[tool(description = "Frequency, period, duty cycle, pulse widths and edge counts of a logic channel over a sample range.")]
    async fn measure_logic(&self, Parameters(p): Parameters<MeasureLogicParams>) -> CallToolResult {
        self.call("measure_logic", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "DSView's oscilloscope measurements for a channel (frequency, Vpp, RMS, rise time, ...).")]
    async fn measure_dso(&self, Parameters(p): Parameters<ChannelParam>) -> CallToolResult {
        self.call("measure_dso", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Zoom the DSView window to a sample range, e.g. to show the user a finding.")]
    async fn show_region(&self, Parameters(p): Parameters<RegionParams>) -> CallToolResult {
        self.call("show_region", params(&p), DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Place cursors in the DSView window at sample indexes (empty list clears them).")]
    async fn set_cursors(&self, Parameters(p): Parameters<CursorsParams>) -> CallToolResult {
        self.call("set_cursors", params(&p), DEFAULT_TIMEOUT).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DsviewServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("dsview-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(self.instructions())
    }
}
