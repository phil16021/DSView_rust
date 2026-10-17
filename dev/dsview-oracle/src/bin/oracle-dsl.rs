//! Loads a capture through the original C session driver and prints a JSON
//! summary: header values, channels and a SHA-256 of every channel's samples
//! as DSView receives them. The Rust `.dsl` reader must produce the same.
//!
//! usage: oracle-dsl <capture.dsl|.demo> [--log LEVEL]

use std::ffi::{CStr, CString, c_int, c_void};

use anyhow::{Context, Result, bail};
use dsview_oracle::ffi;
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Default)]
struct Collected {
    logic_channels: Vec<usize>, // enabled logic probe indexes, in packet order
    logic: Vec<Vec<u64>>,       // per enabled logic channel: 64-sample words
    raw: Sha256,                // DSO/analog payload bytes
    raw_samples: u64,
    packets: [u64; 5],
    split_format: bool,
    dso_channels: usize,
}

extern "C" fn on_packet(
    user: *mut c_void,
    kind: c_int,
    format: c_int,
    data: *const u8,
    len: u64,
    num: c_int,
) {
    // Never unwind into C.
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        collect(user, kind, format, data, len, num)
    }));
    if r.is_err() {
        std::process::abort();
    }
}

fn collect(user: *mut c_void, kind: c_int, format: c_int, data: *const u8, len: u64, num: c_int) {
    let c = unsafe { &mut *(user as *mut Collected) };
    if (0..5).contains(&kind) {
        c.packets[kind as usize] += 1;
    }
    match kind {
        ffi::PKT_LOGIC => {
            if format != ffi::LOGIC_CROSS {
                c.split_format = true;
                return;
            }
            // The first (info-only) pass does not know the channels yet.
            if c.logic.is_empty() || data.is_null() {
                return;
            }
            let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
            let n = c.logic.len();
            // LA_CROSS_DATA: per 64 samples, one u64 per enabled channel.
            for (i, word) in bytes.as_chunks::<8>().0.iter().enumerate() {
                c.logic[i % n].push(u64::from_le_bytes(*word));
            }
        }
        ffi::PKT_DSO | ffi::PKT_ANALOG if !data.is_null() && num > 0 => {
            // DSO packets carry u8 samples interleaved over the enabled channels.
            let bytes =
                unsafe { std::slice::from_raw_parts(data, num as usize * c.dso_channels.max(1)) };
            c.raw.update(bytes);
            c.raw_samples += num as u64;
        }
        _ => {}
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .context("usage: oracle-dsl <capture> [--log LEVEL]")?;
    let mut log_level = 0;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--log" => log_level = args.next().context("--log LEVEL")?.parse()?,
            other => bail!("unexpected argument {other}"),
        }
    }

    // The C side reports channels only after opening, but packets need the
    // enabled-channel order up front: open once for the info, then capture.
    let cpath = CString::new(path.as_str())?;
    let mut info: Box<ffi::OracleDslInfo> = Box::new(unsafe { std::mem::zeroed() });
    let mut probe = Collected::default();
    let rc = unsafe {
        ffi::oracle_dsl_run(
            cpath.as_ptr(),
            log_level,
            info.as_mut(),
            on_packet,
            &mut probe as *mut _ as *mut c_void,
        )
    };
    if rc != 0 {
        bail!("C loader failed (rc {rc})");
    }

    let channels: Vec<ffi::OracleChannel> = info.channels[..info.n_channels as usize].to_vec();
    let mut collected = Collected::default();
    for ch in &channels {
        if ch.enabled != 0 && ch.kind == ffi::CH_LOGIC {
            collected.logic_channels.push(ch.index as usize);
        }
        if ch.enabled != 0 && (ch.kind == ffi::CH_DSO || ch.kind == ffi::CH_ANALOG) {
            collected.dso_channels += 1;
        }
    }
    collected.logic = vec![Vec::new(); collected.logic_channels.len()];
    let rc = unsafe {
        ffi::oracle_dsl_run(
            cpath.as_ptr(),
            log_level,
            info.as_mut(),
            on_packet,
            &mut collected as *mut _ as *mut c_void,
        )
    };
    if rc != 0 {
        bail!("C loader failed (rc {rc})");
    }

    let total = info.limit_samples;
    let channel_json: Vec<_> = channels
        .iter()
        .map(|ch| {
            let name = unsafe { CStr::from_ptr(ch.name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let kind = match ch.kind {
                ffi::CH_LOGIC => "logic",
                ffi::CH_DSO => "dso",
                ffi::CH_ANALOG => "analog",
                _ => "other",
            };
            let mut o =
                json!({"index": ch.index, "type": kind, "enabled": ch.enabled != 0, "name": name});
            if let Some(pos) = collected
                .logic_channels
                .iter()
                .position(|&p| p == ch.index as usize)
            {
                let words = &collected.logic[pos];
                // Hash exactly `total` samples: whole words, then the partial tail.
                let mut h = Sha256::new();
                let full = (total / 64) as usize;
                for w in words.iter().take(full) {
                    h.update(w.to_le_bytes());
                }
                let rem = total % 64;
                if rem > 0
                    && let Some(w) = words.get(full)
                {
                    let masked = w & ((1u64 << rem) - 1);
                    h.update(&masked.to_le_bytes()[..rem.div_ceil(8) as usize]);
                }
                o["samples"] = json!((words.len() as u64 * 64).min(total));
                o["sha256"] = json!(format!("{:x}", h.finalize()));
            }
            o
        })
        .collect();

    let mode = match info.mode {
        0 => "logic",
        1 => "dso",
        2 => "analog",
        _ => "unknown",
    };
    let mut summary = json!({
        "mode": mode,
        "samplerate": info.samplerate,
        "limit_samples": total,
        "channels": channel_json,
        "packets": {"logic": collected.packets[1], "dso": collected.packets[2], "analog": collected.packets[3], "end": collected.packets[4]},
    });
    if collected.raw_samples > 0 {
        summary["raw_samples"] = json!(collected.raw_samples);
        summary["raw_sha256"] = json!(format!("{:x}", collected.raw.clone().finalize()));
    }
    if collected.split_format {
        summary["warning"] = json!("split-format logic packets were not hashed");
    }
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
