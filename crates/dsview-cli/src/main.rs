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

mod soak;

/// DSView's version (CMakeLists.txt `DS_VERSION_STRING`).
const DSVIEW_VERSION: &str = "1.6.1";

const USAGE: &str = "\
usage: dsview-cli serve --mcp [options]
       dsview-cli soak [--mode loop|repeat] [--minutes N] [--device demo|dslogic]
                       [--samples FILE] [options]

Runs DSView's capture session without a GUI behind the control socket
(DSVIEW_MCP_SOCKET, else $XDG_RUNTIME_DIR/dsview/mcp.sock).

options:
  --socket PATH      control socket path
  --data-dir DIR     DSView's data directory (demo/, res/, decoders/);
                     default ../share/DSView next to the binary, then
                     /usr/local/share/DSView
  --decoders DIR     protocol decoder directory (default <data-dir>/decoders,
                     else <data-dir>/../libsigrokdecode4DSL/decoders)
  --log-level N      0 none .. 5 detail (default 3)

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

/// `GetAppDataDir()`
fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent()?.parent().map(|p| p.join("share/DSView")))
        .filter(|d| d.is_dir())
    {
        return d;
    }
    PathBuf::from("/usr/local/share/DSView")
}

fn fail(msg: &str) -> ! {
    eprintln!("dsview-cli: {msg}");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    if cmd != "serve" && cmd != "soak" {
        eprint!("{USAGE}");
        std::process::exit(if args.iter().any(|a| a == "-h" || a == "--help") {
            0
        } else {
            2
        });
    }
    let mut mcp = false;
    let (mut socket, mut data_dir, mut decoders) = (None, None, None);
    let (mut soak_mode, mut minutes, mut device) = ("loop".to_owned(), 60.0f64, "demo".to_owned());
    let mut samples_file = None;
    let mut level = 3;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .unwrap_or_else(|| fail(&format!("{a} needs a value")))
        };
        match a.as_str() {
            "--mcp" => mcp = true,
            "--socket" => socket = Some(PathBuf::from(val())),
            "--data-dir" => data_dir = Some(PathBuf::from(val())),
            "--decoders" => decoders = Some(PathBuf::from(val())),
            "--log-level" => level = val().parse().unwrap_or_else(|_| fail("bad --log-level")),
            "--mode" => soak_mode = val(),
            "--minutes" => minutes = val().parse().unwrap_or_else(|_| fail("bad --minutes")),
            "--device" => device = val(),
            "--samples" => samples_file = Some(val()),
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => fail(&format!("unknown option {other}")),
        }
    }
    if cmd == "serve" && !mcp {
        fail("only `serve --mcp` is supported");
    }
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
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

    dsview_drivers::sys::set_log_sink(Box::new(move |l, m| {
        if l <= level {
            eprintln!("{m}");
        }
    }));
    dsview_decode::log::set_level(match level {
        0 | 1 => dsview_decode::log::Level::Err,
        2 => dsview_decode::log::Level::Warn,
        3 => dsview_decode::log::Level::Info,
        4 => dsview_decode::log::Level::Dbg,
        _ => dsview_decode::log::Level::Detail,
    });
    dsview_decode::log::set_sink(move |_, m| {
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
    if cmd == "soak" {
        let ok = soak::run(
            &session,
            &soak_mode,
            minutes,
            &device,
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
