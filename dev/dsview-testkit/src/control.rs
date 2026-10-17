//! Blocking client for DSView's control socket (`DSView --mcp`) and the
//! scenario runner used to record and check control-API transcripts.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Same lookup as `ControlServer::default_socket_path()`.
pub fn default_socket_path() -> PathBuf {
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

pub struct Client {
    socket: PathBuf,
    next_id: u64,
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            next_id: 1,
        }
    }

    /// One request per connection, like dsview-mcp.
    pub fn call(
        &mut self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Result<Value, String>> {
        let mut stream = UnixStream::connect(&self.socket).with_context(|| {
            format!(
                "connect {} (is `DSView --mcp` running?)",
                self.socket.display()
            )
        })?;
        stream.set_read_timeout(Some(timeout))?;
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        stream.write_all(format!("{req}\n").as_bytes())?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let resp: Value =
            serde_json::from_str(&line).with_context(|| format!("bad response to {method}"))?;
        if resp["id"] != json!(id) {
            bail!("response id mismatch for {method}");
        }
        Ok(match resp.get("error") {
            Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
            None => Ok(resp["result"].clone()),
        })
    }
}

/// How a recorded response is compared on replay.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Compare {
    /// Identical after normalization.
    Exact,
    /// Same JSON shape (keys and value types); for timing-dependent data.
    Shape,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub method: String,
    pub params: Value,
    pub compare: Compare,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recorded {
    pub method: String,
    pub params: Value,
    pub compare: Compare,
    /// `{"ok": result}` or `{"err": message}`, normalized.
    pub response: Value,
}

fn step(method: &str, params: Value, compare: Compare) -> Step {
    Step {
        method: method.into(),
        params,
        compare,
    }
}

/// Exercises all 31 control methods on the demo devices. `{"$device": name}`
/// and `{"$last_decoder": true}` placeholders are resolved at run time, and
/// `$OUT` is the scratch directory for files.
pub fn demo_scenario() -> Vec<Step> {
    use Compare::{Exact, Shape};
    vec![
        step("version", json!({}), Exact),
        // Select first: the previous run leaves another device active.
        step("select_device", json!({"$device": "Demo Logic"}), Exact),
        step("list_devices", json!({}), Exact),
        step("set_mode", json!({"mode": "logic"}), Exact),
        step("set_trigger", json!({"type": "none"}), Exact),
        step("set_sampling", json!({"samples": 1000000}), Exact),
        step(
            "set_channels",
            json!({"channels": [{"index": 2, "enabled": false}]}),
            Exact,
        ),
        step(
            "set_channels",
            json!({"channels": [{"index": 2, "enabled": true}]}),
            Exact,
        ),
        step("set_threshold", json!({"volts": 1.5}), Exact),
        step("get_config", json!({}), Exact),
        step("start_capture", json!({"timeout_s": 60}), Exact),
        step("capture_status", json!({}), Exact),
        step("capture_info", json!({}), Exact),
        step("get_edges", json!({"channel": 1, "limit": 20}), Exact),
        step(
            "read_logic",
            json!({"channels": [0, 1], "max_transitions": 50}),
            Exact,
        ),
        step("measure_logic", json!({"channel": 1}), Exact),
        step("list_decoders", json!({"filter": "i2c"}), Exact),
        step("describe_decoder", json!({"id": "1:i2c"}), Exact),
        step("list_active_decoders", json!({}), Exact),
        step(
            "add_decoder",
            json!({"id": "1:i2c", "channels": {"scl": 1, "sda": 0}, "options": {"address_format": "shifted"}}),
            Exact,
        ),
        step(
            "get_annotations",
            json!({"decoder": {"$last_decoder": true}, "row": "addr-data", "limit": 50}),
            Exact,
        ),
        step("run_decoders", json!({}), Exact),
        step(
            "remove_decoder",
            json!({"index": {"$last_decoder": true}}),
            Exact,
        ),
        step(
            "save_session",
            json!({"path": "$OUT/transcript.dsl"}),
            Exact,
        ),
        step("export_data", json!({"path": "$OUT/transcript.csv"}), Exact),
        step("open_file", json!({"path": "$OUT/transcript.dsl"}), Exact),
        step("show_region", json!({"start": 40000, "end": 60000}), Exact),
        step("set_cursors", json!({"samples": [45147, 50223]}), Exact),
        step("set_cursors", json!({"samples": []}), Exact),
        step(
            "select_device",
            json!({"$device": "Demo Oscilloscope"}),
            Exact,
        ),
        step("set_mode", json!({"mode": "dso"}), Exact),
        step("set_dso", json!({"timebase_ns": 1000}), Shape),
        step("start_capture", json!({"timeout_s": 30}), Shape),
        step(
            "read_waveform",
            json!({"channel": 0, "max_points": 16}),
            Shape,
        ),
        step("measure_dso", json!({"channel": 0}), Shape),
        step("stop_capture", json!({}), Exact),
        step("no_such_method", json!({}), Exact),
        step("get_edges", json!({"channel": "x"}), Exact),
    ]
}

/// Replaces machine-specific values so transcripts compare across machines.
fn normalize(v: &mut Value, out: &str) {
    match v {
        Value::String(s) => {
            if !out.is_empty() && s.contains(out) {
                *s = s.replace(out, "$OUT");
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| normalize(x, out)),
        Value::Object(o) => {
            o.remove("version");
            // Hardware that happens to be plugged in must not change demo transcripts.
            if let Some(Value::Array(devs)) = o.get_mut("devices") {
                devs.retain(|d| d["name"].as_str().is_some_and(|n| n.starts_with("Demo")));
                for (i, d) in devs.iter_mut().enumerate() {
                    d["index"] = json!(i);
                }
            }
            o.values_mut().for_each(|x| normalize(x, out));
        }
        _ => {}
    }
}

fn substitute(v: &Value, out: &str, client: &mut Client, last_decoder: i64) -> Result<Value> {
    Ok(match v {
        Value::String(s) => Value::String(s.replace("$OUT", out)),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| substitute(x, out, client, last_decoder))
                .collect::<Result<_>>()?,
        ),
        Value::Object(o) if o.contains_key("$last_decoder") => json!(last_decoder),
        Value::Object(o) if o.contains_key("$device") => {
            let name = o["$device"].as_str().unwrap_or_default();
            let list = client
                .call("list_devices", &json!({}), Duration::from_secs(10))?
                .map_err(anyhow::Error::msg)?;
            let index = list["devices"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|d| d["name"] == json!(name))
                .with_context(|| format!("device {name:?} not found"))?["index"]
                .clone();
            return Ok(json!({"index": index}));
        }
        Value::Object(o) => {
            let mut m = serde_json::Map::new();
            for (k, x) in o {
                m.insert(k.clone(), substitute(x, out, client, last_decoder)?);
            }
            Value::Object(m)
        }
        other => other.clone(),
    })
}

/// Runs `steps` and returns the normalized transcript.
pub fn run_scenario(client: &mut Client, steps: &[Step], out_dir: &Path) -> Result<Vec<Recorded>> {
    let out = out_dir.to_str().context("non-UTF-8 out dir")?;
    let mut recorded = Vec::new();
    let mut last_decoder = -1i64;
    for s in steps {
        let params = substitute(&s.params, out, client, last_decoder)?;
        let resp = client.call(&s.method, &params, Duration::from_secs(120))?;
        if s.method == "add_decoder"
            && let Ok(r) = &resp
            && let Some(d) = r["decoders"].as_array().and_then(|d| d.last())
        {
            last_decoder = d["index"].as_i64().unwrap_or(-1);
        }
        let mut response = match resp {
            Ok(r) => json!({"ok": r}),
            Err(e) => json!({"err": e}),
        };
        normalize(&mut response, out);
        let mut params_norm = params.clone();
        normalize(&mut params_norm, out);
        recorded.push(Recorded {
            method: s.method.clone(),
            params: params_norm,
            compare: s.compare,
            response,
        });
    }
    Ok(recorded)
}

/// Shape of a JSON value: same keys, same value types, arrays by element type.
pub fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(a) => json!([a.first().map(shape).unwrap_or(json!("empty"))]),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), shape(x))).collect()),
    }
}

/// Compares a fresh run against a stored transcript; returns mismatch descriptions.
pub fn compare(expected: &[Recorded], actual: &[Recorded]) -> Vec<String> {
    let mut problems = Vec::new();
    if expected.len() != actual.len() {
        problems.push(format!(
            "{} steps recorded, {} run",
            expected.len(),
            actual.len()
        ));
    }
    for (i, (e, a)) in expected.iter().zip(actual).enumerate() {
        let same = match e.compare {
            Compare::Exact => e.response == a.response,
            Compare::Shape => shape(&e.response) == shape(&a.response),
        };
        if !same {
            problems.push(format!(
                "step {} {} {}:\n  expected {}\n  actual   {}",
                i + 1,
                e.method,
                e.params,
                truncate(&e.response.to_string()),
                truncate(&a.response.to_string())
            ));
        }
    }
    problems
}

fn truncate(s: &str) -> String {
    if s.chars().count() > 400 {
        format!("{}…", s.chars().take(400).collect::<String>())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_ignores_values() {
        assert_eq!(
            shape(&json!({"a": 1, "b": ["x"]})),
            shape(&json!({"a": 2, "b": ["y", "z"]}))
        );
        assert_ne!(shape(&json!({"a": 1})), shape(&json!({"a": "1"})));
    }

    #[test]
    fn normalize_hides_out_dir_and_hardware() {
        let mut v = json!({"path": "/tmp/x/a.dsl", "version": "1.6.1",
                           "devices": [{"index": 0, "name": "DSLogic Plus"}, {"index": 1, "name": "Demo Logic"}]});
        normalize(&mut v, "/tmp/x");
        assert_eq!(
            v,
            json!({"path": "$OUT/a.dsl", "devices": [{"index": 0, "name": "Demo Logic"}]})
        );
    }

    #[test]
    fn scenario_covers_every_control_method() {
        let methods: std::collections::BTreeSet<_> =
            demo_scenario().into_iter().map(|s| s.method).collect();
        // 31 methods of DSView/pv/mcp/controlapi.cpp plus one unknown method.
        assert_eq!(methods.len(), 32, "{methods:?}");
    }
}
