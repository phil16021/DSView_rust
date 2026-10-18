//! Prints a usbmon capture of a DreamSourceLab device as protocol steps:
//! vendor requests 0xB0 (CTL_WR), 0xB1 (CTL_RD_PRE), 0xB2 (CTL_RD) decoded
//! by destination, bulk OUT (the DSL_setting block, FPGA bitstream), and bulk
//! IN runs summarised.
//!
//! usage: usb-dump <capture.pcapng> [--all | --dev ADDRESS]
//!
//! Without options it shows the first device that receives a vendor
//! request; `--dev` picks another one (a replugged device gets a new address).

use std::collections::HashMap;
use std::path::PathBuf;

use dsview_usb::trace::{self, Kind, Urb, Xfer};

const DESTS: &[&str] = &[
    "FW_VERSION",
    "REVID_VERSION",
    "HW_STATUS",
    "PROG_B",
    "SYS",
    "LED",
    "INTRDY",
    "WORDWIDE",
    "START",
    "STOP",
    "BULK_WR",
    "REG",
    "NVM",
    "I2C_DSO",
    "I2C_REG",
    "I2C_STATUS",
    "DSO_EN0",
    "DSO_DC0",
    "DSO_ATT0",
    "DSO_EN1",
    "DSO_DC1",
    "DSO_ATT1",
    "AWG_WR",
    "I2C_PROBE",
    "I2C_EXT",
];

fn hex(d: &[u8], max: usize) -> String {
    let mut s: String = d.iter().take(max).map(|b| format!("{b:02x}")).collect();
    if d.len() > max {
        s += &format!("…(+{})", d.len() - max);
    }
    s
}

fn main() {
    const USAGE: &str = "usage: usb-dump <capture.pcapng> [--all | --dev ADDRESS]";
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect(USAGE));
    let mut all = false;
    let mut want: Option<u8> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--all" => all = true,
            "--dev" => want = Some(args.next().and_then(|v| v.parse().ok()).expect(USAGE)),
            _ => panic!("{USAGE}"),
        }
    }
    let urbs = trace::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let dev = urbs
        .iter()
        .find(|u| {
            u.xfer == Xfer::Control
                && u.setup.is_some_and(|s| s[0] & 0x60 == 0x40)
                && want.is_none_or(|d| d == u.dev)
        })
        .map(|u| (u.bus, u.dev));
    let submits: HashMap<u64, &Urb> = HashMap::new();
    let mut submits = submits;
    let t0 = urbs.first().map_or(0, |u| u.ts_us);
    let mut bulk_in: Option<(u8, usize, u64)> = None;
    for u in &urbs {
        if !all && Some((u.bus, u.dev)) != dev {
            continue;
        }
        if u.kind == Kind::Submit {
            submits.insert(u.id, u);
            if !(u.xfer == Xfer::Bulk && u.is_in()) {
                continue;
            }
        }
        let t = (u.ts_us - t0) as f64 / 1e6;
        let s = submits.get(&u.id).copied();
        match u.xfer {
            Xfer::Control if u.kind != Kind::Submit => {
                let Some(s) = s else { continue };
                let setup = s.setup.unwrap_or_default();
                let (rt, req) = (setup[0], setup[1]);
                let wvalue = u16::from_le_bytes([setup[2], setup[3]]);
                let windex = u16::from_le_bytes([setup[4], setup[5]]);
                let wlen = u16::from_le_bytes([setup[6], setup[7]]);
                let data = if rt & 0x80 != 0 { &u.data } else { &s.data };
                let what = match req {
                    0xb0 | 0xb1 if data.len() >= 4 => {
                        let dest = data[0] as usize;
                        format!(
                            "{} dest={} off={:#x} size={} data={}",
                            if req == 0xb0 {
                                "CTL_WR    "
                            } else {
                                "CTL_RD_PRE"
                            },
                            DESTS.get(dest).copied().unwrap_or("?"),
                            u16::from_le_bytes([data[1], data[2]]),
                            data[3],
                            hex(&data[4..], 48)
                        )
                    }
                    0xb2 => format!("CTL_RD     -> {}", hex(data, 48)),
                    0xa0 => format!("FX2 RAM    addr={wvalue:#06x} len={wlen}"),
                    _ => format!(
                        "req {rt:#04x}/{req:#04x} value={wvalue:#06x} index={windex:#06x} len={wlen} data={}",
                        hex(data, 32)
                    ),
                };
                println!("{t:10.6} ctrl {what} status={}", u.status);
            }
            Xfer::Bulk if !u.is_in() && u.kind != Kind::Submit => {
                let data = s.map_or(&u.data, |s| &s.data);
                println!(
                    "{t:10.6} bulk OUT ep{:#04x} len={} status={} data={}",
                    u.ep,
                    u.length,
                    u.status,
                    hex(data, 64)
                );
            }
            Xfer::Bulk if u.is_in() => {
                if u.kind == Kind::Submit {
                    continue;
                }
                // summarise runs of completions on the same endpoint
                let total = bulk_in.map_or(0, |b| b.1) + u.length as usize;
                let n = bulk_in.map_or(0, |b| b.2) + 1;
                if n == 1 || u.length < 512 || u.status != 0 {
                    println!(
                        "{t:10.6} bulk IN  ep{:#04x} #{n} len={} status={} total={total} data={}",
                        u.ep,
                        u.length,
                        u.status,
                        hex(&u.data, 32)
                    );
                }
                bulk_in = Some((u.ep, total, n));
            }
            _ => {}
        }
    }
    if let Some((ep, total, n)) = bulk_in {
        println!("bulk IN ep{ep:#04x}: {n} completions, {total} bytes");
    }
}
