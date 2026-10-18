//! The config keys, status layout and other constants match libsigrok.h.

use std::collections::BTreeMap;

fn header() -> String {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../libsigrok4DSL/libsigrok.h"
    );
    std::fs::read_to_string(p).expect("libsigrok.h")
}

/// `NAME = value` pairs of the C enum `name`.
fn c_enum(src: &str, name: &str, prefix: &str) -> BTreeMap<String, i32> {
    let start = src.find(&format!("enum {name}")).expect("enum");
    let body = &src[start..start + src[start..].find("};").unwrap()];
    let mut out = BTreeMap::new();
    for line in body.lines() {
        let line = line.split("/*").next().unwrap().trim();
        if let Some((n, v)) = line.trim_end_matches(',').split_once('=')
            && let Some(n) = n.trim().strip_prefix(prefix)
        {
            out.insert(n.to_owned(), v.trim().parse().unwrap());
        }
    }
    out
}

#[test]
fn config_keys_match_header() {
    let c = c_enum(&header(), "sr_config_option_id", "SR_CONF_");
    let rust: BTreeMap<String, i32> = dsview_core::key::ALL
        .iter()
        .map(|&(n, v)| (n.to_owned(), v))
        .collect();
    assert_eq!(rust, c);
}

#[test]
fn status_layout_is_c() {
    // measured with gcc on libsigrok.h (x86-64); the C shim also asserts it
    assert_eq!(std::mem::size_of::<dsview_core::Status>(), 160);
    assert_eq!(std::mem::offset_of!(dsview_core::Status, ch0_max), 34);
    assert_eq!(std::mem::offset_of!(dsview_core::Status, ch1_max), 96);
    assert_eq!(
        std::mem::offset_of!(dsview_core::Status, ch1_acc_mean_p3),
        156
    );
}
