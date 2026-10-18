//! `dsview-cli`: DSView without its GUI.
//!
//! `dsview-cli serve --mcp` runs the capture session behind the same control
//! socket the Qt GUI opens with `--mcp`, so `dsview-mcp` (and anything else
//! speaking that JSON-RPC) drives it the same way.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dsview_control::api::Api;
use dsview_control::server::Server;
use dsview_session::{Paths, Session};

mod cmds;
mod soak;

/// DSView's version (the workspace version; CMakeLists.txt reads it as
/// `DS_VERSION_STRING`).
const DSVIEW_VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
usage: dsview-cli serve --mcp [options]
       dsview-cli soak [--mode loop|repeat] [--minutes N] [--device demo|dslogic]
                       [--samples FILE] [options]
       dsview-cli devices [--json]
       dsview-cli decoders [--json]
       dsview-cli info [--device D]
       dsview-cli capture [--device D] [--mode logic|dso|analog] [--samplerate HZ]
                          [--samples N] [--channels 0-7,9] [--timeout S] [--json]
                          --out FILE.dsl|csv|vcd|srzip
       dsview-cli export FILE.dsl OUT.csv|vcd|srzip
       dsview-cli decode FILE.dsl --decoder ID[:channel=N,option=value,...]...
                         [--json]
       dsview-cli verify-counter [--device D] [--samples N] [--timeout S]
       dsview-cli selftest [--timeout S] --out DIR

Runs DSView's capture session without a GUI behind the control socket
(DSVIEW_MCP_SOCKET, else $XDG_RUNTIME_DIR/dsview/mcp.sock).

options:
  --socket PATH      control socket path
  --data-dir DIR     DSView's data directory (demo/, res/, decoders/);
                     default ../share/dsview or ../share/DSView next
                     to the binary, then /usr/local/share/DSView
  --decoders DIR     protocol decoder directory (default <data-dir>/decoders,
                     else <data-dir>/../libsigrokdecode4DSL/decoders)
  --log-level N      0 none .. 5 detail (default 3)

--device D is an index of `devices` or a part of a name; without it the
commands take the first hardware device, else the active one.

verify-counter: the DSLogic's internal test mode; every sample must count up.
selftest: for owners of devices the developers do not have (DSCope, MSO-E8):
a capture per hardware device, the settings and the full log in DIR.

soak: captures for N minutes (default 60) in loop mode (one endless capture
into the ring buffer; the demo's random pattern) or repeat mode (one capture
after another, each decoded; the demo's protocol pattern) and reports the
resident memory every minute. After a 5-minute warm-up it samples the resident
memory every 5 s (--samples FILE writes them) and passes when their trend over
the run is under 1 %.
";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn fail(msg: &str) -> ! {
    eprintln!("dsview-cli: {msg}");
    std::process::exit(2);
}

fn main() {
    dsview_session::crash::install("dsview-cli", DSVIEW_VERSION);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    const ONESHOT: &[&str] = &[
        "devices",
        "decoders",
        "info",
        "capture",
        "export",
        "decode",
        "verify-counter",
        "selftest",
    ];
    if cmd != "serve" && cmd != "soak" && !ONESHOT.contains(&cmd) {
        eprint!("{USAGE}");
        std::process::exit(if args.iter().any(|a| a == "-h" || a == "--help") {
            0
        } else {
            2
        });
    }
    let mut mcp = false;
    let (mut socket, mut data_dir, mut decoders) = (None, None, None);
    let (mut mode, mut minutes, mut device) = (None, 60.0f64, None);
    let mut samples_file = None;
    let mut level = 3;
    let mut o = cmds::Opts {
        timeout_s: 60.0,
        ..Default::default()
    };
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .unwrap_or_else(|| fail(&format!("{a} needs a value")))
        };
        let num = |v: String| -> u64 { v.parse().unwrap_or_else(|_| fail(&format!("bad {a}"))) };
        match a.as_str() {
            "--mcp" => mcp = true,
            "--socket" => socket = Some(PathBuf::from(val())),
            "--data-dir" => data_dir = Some(PathBuf::from(val())),
            "--decoders" => decoders = Some(PathBuf::from(val())),
            "--log-level" => level = val().parse().unwrap_or_else(|_| fail("bad --log-level")),
            "--mode" => mode = Some(val()),
            "--minutes" => minutes = val().parse().unwrap_or_else(|_| fail("bad --minutes")),
            "--device" => device = Some(val()),
            // soak: the memory samples file; the others: the sample count
            "--samples" if cmd == "soak" => samples_file = Some(val()),
            "--samples" => o.samples = Some(num(val())),
            "--samplerate" => o.samplerate = Some(num(val())),
            "--channels" => o.channels = Some(val()),
            "--timeout" => o.timeout_s = num(val()) as f64,
            "--out" => o.out = Some(val()),
            "--decoder" => o.decoders.push(val()),
            "--json" => o.json = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other if !other.starts_with('-') && ONESHOT.contains(&cmd) => {
                o.args.push(other.to_owned())
            }
            other => fail(&format!("unknown option {other}")),
        }
    }
    o.device = device.clone();
    if cmd != "soak" {
        o.mode = mode.clone();
    }
    if cmd == "serve" && !mcp {
        fail("only `serve --mcp` is supported");
    }
    let data_dir = data_dir.unwrap_or_else(dsview_session::default_data_dir);
    // GetDecodeScriptDir(): <data>/decoders, else ../share/libsigrokdecode4DSL/decoders
    let decoders = decoders.unwrap_or_else(|| {
        let own = data_dir.join("decoders");
        let shared = data_dir.join("../libsigrokdecode4DSL/decoders");
        if !own.is_dir() && shared.is_dir() {
            shared
        } else {
            own
        }
    });

    // selftest keeps the whole log next to its report
    let log_file = (cmd == "selftest")
        .then_some(o.out.as_ref())
        .flatten()
        .and_then(|d| {
            std::fs::create_dir_all(d).ok()?;
            std::fs::File::create(Path::new(d).join("dsview.log")).ok()
        })
        .map(|f| Arc::new(std::sync::Mutex::new(f)));
    let full = log_file.is_some();
    let decode_log = log_file.clone();
    dsview_drivers::sys::set_log_sink(Box::new(move |l, m| {
        if let Some(f) = &log_file {
            use std::io::Write;
            let _ = writeln!(f.lock().unwrap_or_else(|e| e.into_inner()), "{m}");
        }
        if l <= level {
            eprintln!("{m}");
        }
    }));
    dsview_decode::log::set_level(match if full { 5 } else { level } {
        0 | 1 => dsview_decode::log::Level::Err,
        2 => dsview_decode::log::Level::Warn,
        3 => dsview_decode::log::Level::Info,
        4 => dsview_decode::log::Level::Dbg,
        _ => dsview_decode::log::Level::Detail,
    });
    dsview_decode::log::set_sink(move |_, m| {
        if let Some(f) = &decode_log {
            use std::io::Write;
            let _ = writeln!(f.lock().unwrap_or_else(|e| e.into_inner()), "srd: {m}");
            return;
        }
        if level > 0 {
            eprintln!("srd: {m}");
        }
    });
    if let Err(e) = dsview_decode::init(&[Path::new(&decoders)]) {
        fail(&format!("decoder engine: {e}"));
    }
    dsview_decode::load_all();

    let session = Session::new(
        Paths {
            data_dir,
            profile_dir: Paths::default_profile_dir(),
            version: DSVIEW_VERSION.into(),
        },
        Arc::new(dsview_usb::nusb_backend::NusbBackend),
    );
    session.set_default_device();
    if ONESHOT.contains(&cmd) {
        let code = cmds::run(cmd, &session, DSVIEW_VERSION, &o);
        session.exit();
        std::process::exit(code);
    }
    if cmd == "soak" {
        let ok = soak::run(
            &session,
            mode.as_deref().unwrap_or("loop"),
            minutes,
            device.as_deref().unwrap_or("demo"),
            samples_file.as_deref(),
        );
        session.exit();
        std::process::exit(if ok { 0 } else { 1 });
    }

    let path = socket.unwrap_or_else(dsview_control::default_socket_path);
    let api = Arc::new(Api::new(session.clone(), DSVIEW_VERSION));
    let server = match Server::start(&path, api) {
        Ok(s) => s,
        Err(e) => fail(&e),
    };
    eprintln!("MCP: control socket ready at {}", server.path().display());

    // SAFETY: the handler only stores to an atomic
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    drop(server);
    session.exit();
}
