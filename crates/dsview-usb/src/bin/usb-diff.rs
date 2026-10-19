//! Compares the DreamSourceLab vendor protocol in two usbmon captures, e.g.
//! the C and the Rust driver running the same scenario.
//!
//! usage: usb-diff <a.pcapng> <b.pcapng>    (exit status 1 when they differ)

use std::path::PathBuf;

use dsview_usb::{replay, trace};

fn main() {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    assert_eq!(args.len(), 2, "usage: usb-diff <a.pcapng> <b.pcapng>");
    let a = trace::read(&args[0]).unwrap_or_else(|e| panic!("{}: {e}", args[0].display()));
    let b = trace::read(&args[1]).unwrap_or_else(|e| panic!("{}: {e}", args[1].display()));
    let diffs = replay::compare(&a, &b);
    if diffs.is_empty() {
        println!("same protocol");
        return;
    }
    for d in &diffs {
        println!("{d}");
    }
    std::process::exit(1);
}
