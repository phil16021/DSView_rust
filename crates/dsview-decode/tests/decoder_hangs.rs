//! Shipped decoders that never returned or failed on ordinary input
//! (DEVIATIONS F16), each on the smallest input that showed it. A decoder
//! that loops holds the GIL in its thread, so a hang cannot be recovered
//! from: the watchdog aborts the test process with the decoder's name.

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
