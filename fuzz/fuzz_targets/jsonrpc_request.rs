#![no_main]
//! The control socket's request handling: any line gets a JSON-RPC reply
//! (or none for a notification), never a panic.

use libfuzzer_sys::fuzz_target;
use serde_json::{Map, Value};

struct Echo;

impl dsview_control::ControlBackend for Echo {
    fn call(&self, method: &str, params: &Map<String, Value>) -> Result<Value, String> {
        if method.len() % 2 == 0 {
            Ok(Value::Object(params.clone()))
        } else {
            Err(method.to_owned())
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if let Some(reply) = dsview_control::server::handle_line(data, &Echo) {
        assert!(reply.get("jsonrpc").is_some(), "{reply}");
    }
});
