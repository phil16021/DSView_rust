//! Shipped decoders that never returned or failed on ordinary input
//! (DEVIATIONS F16; F35, F37 and F39 from the decoder soak; F36), each on the
//! smallest input that showed it. A decoder that loops holds the GIL in its
//! thread, so a hang cannot be recovered from: the watchdog aborts the test
//! process with the decoder's name.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use dsview_decode::{OptValue, OutputData, SendError, Session};

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/decoders");
        dsview_decode::init(&[dir.as_path()]).expect("init");
        dsview_decode::load_all();
    });
}

/// Aborts the process when `what` has not finished within 20 s.
struct Watchdog(Arc<AtomicBool>);

impl Watchdog {
    fn start(what: &'static str) -> Watchdog {
        let done = Arc::new(AtomicBool::new(false));
        let d = Arc::clone(&done);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            if !d.load(Ordering::SeqCst) {
                eprintln!("{what} did not return within 20 s: the decoder loops");
                std::process::abort();
            }
        });
        Watchdog(done)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Bit-packs per-sample levels of each channel.
fn pack(channels: &[Vec<bool>]) -> Vec<Vec<u8>> {
    channels
        .iter()
        .map(|levels| {
            let mut v = vec![0u8; levels.len().div_ceil(64) * 8];
            for (i, &l) in levels.iter().enumerate() {
                if l {
                    v[i / 8] |= 1 << (i % 8);
                }
            }
            v
        })
        .collect()
}

/// One I²C write (SCL, SDA): start, `bytes` each with an ACK, stop; 8
/// samples per half clock.
fn i2c_write(bytes: &[u8]) -> Vec<Vec<bool>> {
    let h = 8;
    let (mut scl, mut sda) = (vec![true; 40], vec![true; 40]);
    let mut put = |c: bool, d: bool, n: usize| {
        scl.extend(std::iter::repeat_n(c, n));
        sda.extend(std::iter::repeat_n(d, n));
    };
    put(true, false, h);
    for &byte in bytes {
        for i in (0..8).rev() {
            put(false, byte >> i & 1 == 1, h);
            put(true, byte >> i & 1 == 1, h);
        }
        put(false, false, h);
        put(true, false, h);
    }
    put(false, false, h);
    put(true, false, h);
    put(true, true, 4 * h);
    vec![scl, sda]
}

/// Decodes `levels` with `id` (stacked on `under` when given); returns the
/// annotation texts and the result of `send`.
fn decode(
    id: &str,
    under: Option<(&str, &[&str])>,
    channels: &[&str],
    opts: &[(&str, OptValue)],
    levels: &[Vec<bool>],
) -> (Vec<String>, Result<(), SendError>) {
    init();
    let session = Session::new();
    let opts: HashMap<String, OptValue> = opts
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let (bottom, top) = match under {
        Some((bid, bch)) => {
            let b = session
                .inst_new(bid, Some(HashMap::new()), None)
                .expect("bottom");
            let map: Vec<(String, i32)> = bch
                .iter()
                .enumerate()
                .map(|(i, c)| (c.to_string(), i as i32))
                .collect();
            session.channel_set_all(&b, &map).expect("channels");
            let t = session.inst_new(id, Some(opts), None).expect(id);
            session.stack(&b, &t);
            (b, t)
        }
        None => {
            let t = session.inst_new(id, Some(opts), None).expect(id);
            let map: Vec<(String, i32)> = channels
                .iter()
                .enumerate()
                .map(|(i, c)| (c.to_string(), i as i32))
                .collect();
            session.channel_set_all(&t, &map).expect("channels");
            (Arc::clone(&t), t)
        }
    };
    let _ = (&bottom, &top);
    session.metadata_set_samplerate(1_000_000);
    let texts = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&texts);
    let top_id = id.to_owned();
    session.callback_add(dsview_decode::module::OUTPUT_ANN, move |o| {
        if let OutputData::Annotation(a) = &o.data
            && o.inst.decoder.id == top_id
        {
            sink.lock().unwrap().extend(a.texts.first().cloned());
        }
    });
    session.start().expect("start");
    let bufs = pack(levels);
    let refs: Vec<Option<&[u8]>> = bufs.iter().map(|b| Some(b.as_slice())).collect();
    let n = levels[0].len() as u64;
    let consts = vec![0u8; levels.len()];
    let res = session.send(0, n, &refs, &consts);
    let _ = session.end();
    let t = texts.lock().unwrap().clone();
    (t, res)
}

/// `ltc26x7`: an I²C address below the chip's range made the address
/// conversion loop forever with a growing list.
#[test]
fn ltc26x7_low_address_returns() {
    let _w = Watchdog::start("ltc26x7");
    // address 0x02, write; command 0x30, two data bytes
    let (texts, res) = decode(
        "ltc26x7",
        Some(("1:i2c", &["scl", "sda"])),
        &[],
        &[],
        &i2c_write(&[0x02 << 1, 0x30, 0x12, 0x34]),
    );
    res.expect("decodes");
    assert!(
        texts.iter().any(|t| t.contains("Unknown")),
        "the address is reported as unknown: {texts:?}"
    );
}

/// `gpib`: with `sample_total` set, the skip count reached 0 and `skip: 0`
/// returns the same sample again, forever.
#[test]
fn gpib_sample_total_returns() {
    let _w = Watchdog::start("gpib");
    let names = [
        "dio1", "dio2", "dio3", "dio4", "dio5", "dio6", "dio7", "dio8", "eoi", "dav", "nrfd",
        "ndac", "ifc", "srq", "atn", "ren",
    ];
    // idle bus, DAV pulsing low once
    let mut levels = vec![vec![true; 400]; 16];
    for s in &mut levels[9][100..120] {
        *s = false;
    }
    let (_, res) = decode(
        "gpib",
        None,
        &names,
        &[("sample_total", OptValue::Int(50))],
        &levels,
    );
    res.expect("decodes");
}

/// `lfast`: the timeout check used an undefined name, so the second edge
/// raised a NameError.
#[test]
fn lfast_runs_past_its_first_edges() {
    let _w = Watchdog::start("lfast");
    // a clock of 20-sample bits
    let levels = vec![(0..2000).map(|i| (i / 20) % 2 == 1).collect::<Vec<bool>>()];
    let (texts, res) = decode("lfast", None, &["data"], &[], &levels);
    res.expect("no NameError");
    assert!(!texts.is_empty(), "annotations");
}

/// `ir_irmp` needs the IRMP C library, which DSViewR does not ship: decoding
/// fails with a message that says so.
#[test]
fn ir_irmp_without_its_library_says_so() {
    let _w = Watchdog::start("ir_irmp");
    let levels = vec![(0..2000).map(|i| (i / 50) % 2 == 1).collect::<Vec<bool>>()];
    let (_, res) = decode("ir_irmp", None, &["ir"], &[], &levels);
    match res {
        Err(SendError::Decoder(msg)) => {
            assert!(msg.contains("IRMP library"), "{msg}");
            assert!(msg.contains("libirmp.so"), "{msg}");
        }
        other => panic!("expected the library error, got {other:?}"),
    }
}

/// A failed library load must not leave `__del__` raising AttributeError
/// when the half-made object is collected.
#[test]
fn irmp_library_del_after_failed_load_is_quiet() {
    let Ok(py) = which("python3") else {
        eprintln!("no python3 on PATH; skipped");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/decoders/ir_irmp");
    let out = std::process::Command::new(py)
        .arg("-c")
        .arg(
            "import importlib.util, sys\n\
             spec = importlib.util.spec_from_file_location('irmp_library', sys.argv[1])\n\
             m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)\n\
             try:\n    m.IrmpLibrary()\nexcept OSError:\n    pass\n",
        )
        .arg(dir.join("irmp_library.py"))
        .env("LD_LIBRARY_PATH", "/nonexistent")
        .output()
        .expect("python3 runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!err.contains("AttributeError"), "{err}");
}

fn which(name: &str) -> Result<PathBuf, ()> {
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(name))
                .find(|f| f.is_file())
        })
        .ok_or(())
}

/// Bits of `bytes`, MSB first.
fn bits_msb(bytes: &[u8]) -> Vec<bool> {
    bytes
        .iter()
        .flat_map(|&b| (0..8).rev().map(move |i| b >> i & 1 == 1))
        .collect()
}

/// A clock (first channel) and `lines` clocked with it: each bit is set up
/// while the clock is low and held over its rising edge, 4 samples per half
/// clock; the clock ends low.
fn clocked(lines: &[Vec<bool>]) -> Vec<Vec<bool>> {
    let h = 4;
    let mut out = vec![vec![false; 8]; lines.len() + 1];
    for k in 0..lines[0].len() {
        for c in [false, true] {
            out[0].extend(std::iter::repeat_n(c, h));
            for (o, l) in out[1..].iter_mut().zip(lines) {
                o.extend(std::iter::repeat_n(l[k], h));
            }
        }
    }
    // a last falling edge, then idle
    out[0].extend(std::iter::repeat_n(false, 8));
    for o in &mut out[1..] {
        let last = *o.last().unwrap();
        o.extend(std::iter::repeat_n(last, 8));
    }
    out
}

/// `st7735`: a command byte missing from its table raised KeyError instead of
/// taking its "Unknown command" branch (F35).
#[test]
fn st7735_unknown_command_is_annotated() {
    let _w = Watchdog::start("st7735");
    // command 0x42 (not in the table), then NOP so 0x42 is described
    let mosi = bits_msb(&[0x42, 0x00]);
    let n = mosi.len();
    let mut l = clocked(&[vec![false; n], mosi, vec![false; n]]);
    // the helper puts the clock first; st7735 wants CS#, CLK, MOSI, DC
    l.swap(0, 1);
    let (texts, res) = decode("st7735", None, &["cs", "clk", "mosi", "dc"], &[], &l);
    res.expect("no KeyError");
    assert!(
        texts.iter().any(|t| t.starts_with("Unknown command: 42")),
        "{texts:?}"
    );
}

/// `i2s`: words wider than 32 bits overflowed the WAV sample packing (F35).
#[test]
fn i2s_wide_words_decode() {
    let _w = Watchdog::start("i2s");
    // three 64-bit words of ones; WS flips every 64 bits
    let n = 64 * 3 + 2;
    let ws: Vec<bool> = (0..n).map(|k| (k / 64) % 2 == 1).collect();
    let l = clocked(&[ws, vec![true; n]]);
    let (texts, res) = decode(
        "i2s",
        None,
        &["sck", "ws", "sd"],
        &[("wordsize", OptValue::Int(64))],
        &l,
    );
    res.expect("no struct.error");
    assert!(
        texts.iter().any(|t| t.ends_with(": ffffffffffffffff")),
        "{texts:?}"
    );
}

/// `seven_segment`: common-anode without a DP channel cut the pins to seven,
/// then read the eighth (IndexError, F35).
#[test]
fn seven_segment_common_anode_without_dp() {
    let _w = Watchdog::start("seven_segment");
    // segments of "1" (b, c) driven low (common anode), then all off
    let mut levels = vec![vec![true; 300]; 7];
    for seg in [1, 2] {
        for s in &mut levels[seg][50..150] {
            *s = false;
        }
    }
    let (texts, res) = decode(
        "seven_segment",
        None,
        &["a", "b", "c", "d", "e", "f", "g"],
        &[("polarity", OptValue::Str("common-anode".into()))],
        &levels,
    );
    res.expect("no IndexError");
    assert!(texts.iter().any(|t| t == "1"), "{texts:?}");
}

/// `smart_qspi`: a quad step (here the address of Quad I/O Fast Read) with
/// IO1 not mapped indexed an empty list (IndexError, F35).
#[test]
fn smart_qspi_quad_step_without_io1() {
    let _w = Watchdog::start("smart_qspi");
    // command 0xEB on IO0, then one byte time of address; IO2/IO3 carry
    // bytes that are neither 0x00 nor 0xFF so the quad mode is chosen
    let mut l = clocked(&[
        bits_msb(&[0xEB, 0x12]),
        bits_msb(&[0x5A, 0x5A]),
        bits_msb(&[0x3C, 0x3C]),
    ]);
    // buffers go by decoder channel: an idle slot for the unmapped IO1
    l.insert(2, vec![false; l[0].len()]);
    let (texts, res) = decode("smart_qspi", None, &["clk", "io0", "io2", "io3"], &[], &l);
    res.expect("no IndexError");
    assert!(
        texts.iter().any(|t| t.contains("needs IO1-IO3")),
        "{texts:?}"
    );
}

/// `fsi`: an E_POLL carries no address and so no direction bit, but its
/// response reads the direction (AttributeError, F35). The frames were found
/// by a search over random bits; one level per half clock.
#[test]
fn fsi_poll_without_direction() {
    let _w = Watchdog::start("fsi");
    let bits = "00101111001011011001000010100110100110100101101111010110110100111010110000001111101001011011111";
    let (mut data, mut clk) = (vec![true; 2], vec![false; 2]);
    for (k, b) in bits.bytes().enumerate() {
        data.extend([b == b'1'; 2]);
        clk.extend([k % 2 == 1; 2]);
    }
    let (texts, res) = decode("fsi", None, &["data", "clock"], &[], &[data, clk]);
    res.expect("no AttributeError");
    assert!(
        texts.iter().any(|t| t.starts_with("Command: E_POLL")),
        "{texts:?}"
    );
}

/// `spdif`: a preamble-length pulse inside a subframe went into its bits as
/// a `2`, and the value conversion raised ValueError (F35).
#[test]
fn spdif_long_pulse_in_subframe() {
    let _w = Watchdog::start("spdif");
    // biphase mark, pulse widths in unit intervals: preambles B, W, M, W,
    // then 28 bits each ('1' = two short pulses, '0' = one of two UI)
    let bits = [1, 0, 1, 1, 0, 0, 1, 0].repeat(3);
    let mut widths = Vec::new();
    for k in 0..6 {
        widths.extend(match k % 4 {
            0 => [3, 1, 1, 3],
            2 => [3, 3, 1, 1],
            _ => [3, 2, 1, 2],
        });
        for &b in bits.iter().chain(&[0, 1, 0, 0]) {
            widths.extend(if b == 1 { &[1, 1][..] } else { &[2][..] });
        }
    }
    // subframe 3: its second bit, a '0', becomes a 3-UI pulse
    let per = widths.len() / 6;
    assert_eq!(widths[3 * per + 6], 2);
    widths[3 * per + 6] = 3;
    let mut data = vec![false; 8];
    let mut level = false;
    for w in widths {
        level = !level;
        data.extend(std::iter::repeat_n(level, w * 4));
    }
    data.extend([!level; 8]);
    let (texts, res) = decode("spdif", None, &["data"], &[], &[data]);
    res.expect("no ValueError");
    assert!(texts.iter().any(|t| t == "Sample ?"), "{texts:?}");
    assert!(
        texts.iter().any(|t| t == "Sample 0xb2b2b2"),
        "valid subframes keep their values: {texts:?}"
    );
}

/// `smart_qspi`: resolving a command's address mode (24 or 32 bits) wrote it
/// into the shared command table, so the first mode stuck: after a 4-byte
/// read, a 3-byte one waited for a fourth address byte (F36).
#[test]
fn smart_qspi_address_mode_follows_each_command() {
    let _w = Watchdog::start("smart_qspi");
    // one CS# low frame per command: Enter 4-byte mode, READ at 0x12345678,
    // Exit 4-byte mode, READ at 0xABCDEF. IO1 idle low selects the
    // configured two-line mode.
    let frames: [&[u8]; 4] = [
        &[0xB7],
        &[0x03, 0x12, 0x34, 0x56, 0x78],
        &[0xE9],
        &[0x03, 0xAB, 0xCD, 0xEF],
    ];
    // buffers by decoder channel: CLK, IO0, IO1, IO2, IO3, CS#
    let mut l = vec![Vec::new(); 6];
    for bytes in frames {
        let bits = bits_msb(bytes);
        let n = bits.len();
        let f = clocked(&[bits, vec![false; n]]);
        let len = f[0].len();
        for (o, c) in l.iter_mut().zip(f) {
            o.extend(c);
        }
        l[3].extend(vec![false; len]);
        l[4].extend(vec![false; len]);
        l[5].extend(vec![false; len]);
        // CS# high between frames
        for (k, o) in l.iter_mut().enumerate() {
            o.extend([k == 5; 16]);
        }
    }
    let (texts, res) = decode(
        "smart_qspi",
        None,
        &["clk", "io0", "io1", "cs"],
        &[("twolinesmode", OptValue::Str("dspi".into()))],
        &l,
    );
    res.expect("decodes");
    assert!(
        texts.iter().any(|t| t == "32-Bit Address : 0x12345678"),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "24-Bit Address : 0xabcdef"),
        "{texts:?}"
    );
}

/// `swd`: in the REQ state every sampled bit is kept, and each clock edge
/// searches all of them for a request: quadratic in the edges since the
/// last request or line reset (F37). With SWDIO high (no request; a line
/// reset is only reported when it ends), 40 000 clocks ran past the
/// watchdog's 20 s before.
#[test]
fn swd_long_idle_clock_is_linear() {
    let _w = Watchdog::start("swd");
    let n = 40_000;
    let clk: Vec<bool> = (0..4 * n).map(|i| (i / 2) % 2 == 1).collect();
    let dio = vec![true; clk.len()];
    let t = std::time::Instant::now();
    let (_, res) = decode("swd", None, &["swclk", "swdio"], &[], &[clk, dio]);
    res.expect("decodes");
    let took = t.elapsed();
    assert!(took < Duration::from_secs(3), "took {took:?}");
}

/// `ir_rc6`: a pulse after a decoded package, then the timeout, left one
/// bit, and the package check read the second (IndexError, F39).
#[test]
fn ir_rc6_stray_pulse_after_a_package() {
    let _w = Watchdog::start("ir_rc6");
    // 1 MHz: half a bit is 444 samples. IR marks are low, idle high.
    let t = 444;
    let mut ir = vec![true; 2000];
    let level = |l: bool, n: usize, ir: &mut Vec<bool>| ir.extend(std::iter::repeat_n(l, n));
    // leader, then start bit 1, mode 000, toggle 0 (double), address and data
    level(false, 6 * t, &mut ir);
    level(true, 2 * t, &mut ir);
    let bit = |b: bool, len: usize, ir: &mut Vec<bool>| {
        level(!b, len, ir);
        level(b, len, ir);
    };
    bit(true, t, &mut ir);
    for _ in 0..3 {
        bit(false, t, &mut ir);
    }
    bit(false, 2 * t, &mut ir);
    for b in bits_msb(&[0x2A, 0x5C]) {
        bit(b, t, &mut ir);
    }
    level(true, 10 * t, &mut ir);
    // stray pulses (found with a search over random ones), then idle
    for (low, high) in [(3, 1), (2, 1)] {
        level(false, low * t, &mut ir);
        level(true, high * t, &mut ir);
    }
    level(true, 10 * t, &mut ir);
    let (texts, res) = decode("ir_rc6", None, &["ir"], &[], &[ir]);
    res.expect("no IndexError");
    assert!(texts.iter().any(|t| t.starts_with("Data: ")), "{texts:?}");
}
