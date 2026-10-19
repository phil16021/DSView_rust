//! The config keys, status layout and other constants match libsigrok.h
//! (the C header is gone since M7a; its key numbers are frozen in
//! `config_keys.txt`, taken from tag `c-final`).

use std::collections::BTreeMap;

/// `NAME = value` lines of the frozen `enum sr_config_option_id`.
fn frozen_keys() -> BTreeMap<String, i32> {
    include_str!("config_keys.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (n, v) = l.split_once('=').expect("NAME = value");
            (n.trim().to_owned(), v.trim().parse().expect("number"))
        })
        .collect()
}

#[test]
fn config_keys_match_header() {
    let c = frozen_keys();
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

#[test]
fn trigger_layout_is_c() {
    // struct ds_trigger: 4 u16, 3 x 17 bytes, 2 x 17 x 32 chars, 2 x 17 u32
    assert_eq!(std::mem::size_of::<dsview_core::Trigger>(), 1284);
    assert_eq!(
        std::mem::offset_of!(dsview_core::Trigger, trigger0_count),
        1148
    );
}
