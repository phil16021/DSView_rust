//! `dsview`: DSView's GUI in Rust (docs/migration/PLAN.md, Phase 5).
//!
//! Milestone M6a, the viewer: open `.dsl`/`.demo` files, logic traces with
//! the ruler, header, zoom/pan, cursors and measurements; the control API
//! with `--mcp`.
//!
//! `dsview [options] [FILE]`, options as the Qt GUI's (`main.cpp`):
//! `-l N` log level, `-v` version, `--mcp` control socket; and
//! `--data-dir DIR`, `--bench SECONDS` (frame-time log, then exit). The
//! renderer is glow (OpenGL) unless `DSVIEW_RENDERER=wgpu`.

use dsview_gui::{app, hooks};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsview_control::api::Api;
use dsview_control::server::Server;
use dsview_session::{Paths, Session};
use eframe::egui;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

use dsview_gui::DSVIEW_VERSION;

fn usage() -> ! {
    eprintln!(
        "usage: DSViewR [-l N] [-s] [-v] [--mcp] [--data-dir DIR] [--bench SECONDS] [--soak MINUTES] [FILE]"
    );
    std::process::exit(2);
}

/// eframe's application, with the monitor layout read before its window
/// opens. Wayland gives no layout: clients cannot place their windows there.
struct MonitorProbe<'a> {
    inner: eframe::EframeWinitApplication<'a>,
    monitors: app::Monitors,
    wayland: bool,
    probed: bool,
}

impl ApplicationHandler<eframe::UserEvent> for MonitorProbe<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !std::mem::replace(&mut self.probed, true) && !self.wayland {
            let all: Vec<_> = event_loop.available_monitors().collect();
            let primary = event_loop.primary_monitor();
            let layout = (!all.is_empty()).then(|| app::MonitorLayout {
                monitors: all
                    .iter()
                    .map(|m| {
                        let (p, s) = (m.position(), m.size());
                        [p.x, p.y, s.width as i32, s.height as i32]
                    })
                    .collect(),
                // none known: the first one
                primary: primary
                    .and_then(|p| all.iter().position(|m| *m == p))
                    .unwrap_or(0),
            });
            if let Ok(mut m) = self.monitors.lock() {
                *m = layout;
            }
        }
        self.inner.resumed(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.inner.window_event(event_loop, id, event);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.inner.new_events(event_loop, cause);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
        self.inner.user_event(event_loop, event);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        self.inner.device_event(event_loop, id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.about_to_wait(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.memory_warning(event_loop);
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn is_wayland<T>(event_loop: &EventLoop<T>) -> bool {
    use winit::platform::wayland::EventLoopExtWayland as _;
    event_loop.is_wayland()
}

#[cfg(not(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
)))]
fn is_wayland<T>(_: &EventLoop<T>) -> bool {
    false
}

fn main() -> eframe::Result {
    dsview_session::crash::install("dsview", dsview_gui::DSVIEW_VERSION);
    let mut args = std::env::args().skip(1);
    let (mut data_dir, mut bench, mut file, mut mcp) = (None, None, None, false);
    let mut soak = None;
    let mut level = 3;
    let mut store_log = false;
    while let Some(a) = args.next() {
        // getopt's attached forms: -l3, --loglevel=3
        if let Some(v) = a
            .strip_prefix("--loglevel=")
            .or_else(|| a.strip_prefix("-l").filter(|v| !v.is_empty()))
        {
            level = v.parse().unwrap_or(level);
            continue;
        }
        match a.as_str() {
            "--data-dir" => data_dir = args.next().map(PathBuf::from),
            "--bench" => bench = args.next().and_then(|v| v.parse::<f64>().ok()),
            "--mcp" => mcp = true,
            "--soak" => soak = args.next().and_then(|v| v.parse::<f64>().ok()),
            "-l" | "--loglevel" => level = args.next().and_then(|v| v.parse().ok()).unwrap_or(3),
            "-v" | "-V" | "--version" => {
                println!("DSViewR {DSVIEW_VERSION}");
                return Ok(());
            }
            "-s" | "--storelog" => store_log = true,
            "-h" | "-?" | "--help" => usage(),
            f if !f.starts_with('-') && file.is_none() => file = Some(f.to_owned()),
            f if !f.starts_with('-') => {
                println!("Only one file can be openened.");
                std::process::exit(1);
            }
            _ => usage(),
        }
    }
    dsview_gui::log::init(level);
    let data_dir = data_dir.unwrap_or_else(dsview_session::default_data_dir);
    let decoders = data_dir.join("decoders");
    if let Err(e) = dsview_decode::init(&[decoders.as_path()]) {
        eprintln!("dsview: decoder engine: {e}");
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
    match &file {
        Some(f) if std::path::Path::new(f).is_file() => {
            if session.set_file(f).is_err() {
                eprintln!("dsview: failed to load {f}");
                session.set_default_device();
            }
        }
        Some(f) => {
            eprintln!("dsview: file not found: {f}");
            session.set_default_device();
        }
        None if bench.is_some() => {
            // the benchmark pans the demo capture
            let (list, _) = session.devices();
            if let Some(e) = list.iter().find(|e| e.name == "Demo Logic") {
                session.set_device(e.handle);
            }
            session.start_capture(false);
            session.wait(Duration::from_secs(30), |st| {
                st.ended_seq > 0 && !st.working
            });
        }
        None if soak.is_some() => {
            // a loop capture on the demo's random pattern
            let (list, _) = session.devices();
            if let Some(e) = list.iter().find(|e| e.name == "Demo Logic") {
                session.set_device(e.handle);
            }
            session.set_demo_pattern("random");
            session.set_collect_mode(dsview_session::session::CollectMode::Loop, 0.0);
            session.start_capture(false);
        }
        None => {
            session.set_default_device();
        }
    }

    let shared = hooks::Shared::default();
    let server = if mcp {
        let api = Api::with_view(
            session.clone(),
            DSVIEW_VERSION,
            Box::new(hooks::GuiHooks(shared.clone())),
        );
        match Server::start(&dsview_control::default_socket_path(), Arc::new(api)) {
            Ok(s) => {
                eprintln!("MCP: control socket ready at {}", s.path().display());
                Some(s)
            }
            Err(e) => {
                eprintln!("{e}");
                None
            }
        }
    } else {
        None
    };

    let renderer = match std::env::var("DSVIEW_RENDERER").as_deref() {
        Ok("wgpu") => eframe::Renderer::Wgpu,
        _ => eframe::Renderer::Glow,
    };
    // the saved settings; benchmarks and soaks run on the defaults
    let saved = (bench.is_none() && soak.is_none()).then(dsview_gui::options::AppOptions::load);
    // `-s` and the saved log options (main.cpp)
    dsview_gui::log::start(level, store_log, &saved.clone().unwrap_or_default());
    let bench_size = std::env::var("DSVIEW_BENCH_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some([w.parse().ok()?, h.parse().ok()?])
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("DSViewR v{DSVIEW_VERSION}"))
        // Wayland app id and X11 class: the installed desktop file's name,
        // `dsviewr`
        .with_app_id("dsviewr")
        .with_inner_size(bench_size.unwrap_or([1280.0, 800.0]));
    // `MainFrame::ReadSettings`: the last normal window, maximised again if
    // it was
    if bench_size.is_none()
        && let Some(o) = &saved
    {
        if let Some([x, y, w, h]) = o.frame.window {
            viewport = viewport
                .with_position([x as f32, y as f32])
                .with_inner_size([w as f32, h as f32]);
        }
        viewport = viewport.with_maximized(o.frame.is_max);
    }
    let options = eframe::NativeOptions {
        renderer,
        viewport,
        ..Default::default()
    };
    let s2 = session.clone();
    // eframe's event loop, run here to read the monitor layout winit only
    // gives inside it (`MainFrame::ReadSettings`' screen check)
    let event_loop = EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    let monitors = app::Monitors::default();
    let m2 = monitors.clone();
    let inner = eframe::create_native(
        "dsview",
        options,
        Box::new(move |cc| {
            // logic traces are axis-aligned 1 px lines: no anti-aliasing
            // needed, and software GL rasterises far less (PLAN.md §5.1)
            cc.egui_ctx
                .tessellation_options_mut(|o| o.feathering = false);
            let mut app = app::App::new(s2, shared, DSVIEW_VERSION, &cc.egui_ctx);
            // benchmarks and soaks run without anti-aliasing as measured in
            // PLAN.md §5.1 (trace meshes are never feathered, so the option
            // touches lines and text only)
            // CJK glyphs for Chinese (and the language menu's 中文)
            if let Some(p) = dsview_gui::i18n::add_cjk_font(&cc.egui_ctx) {
                dsview_gui::log::info(&format!("CJK font: {}", p.display()));
            }
            match saved {
                Some(o) => {
                    dsview_gui::i18n::load(
                        &dsview_gui::i18n::lang_dir(app.session.data_dir()),
                        o.frame.language,
                    );
                    app.session.state().language = o.frame.language;
                    app.startup = Some(dsview_gui::startup::Startup::new(
                        &o,
                        app.session.data_dir(),
                    ));
                    app.options = o;
                }
                None => app.options.antialias = false,
            }
            app.options.apply(&mut app.view, &cc.egui_ctx);
            app.session.state().swap_back_buffer_always = app.options.swap_back_buffer_always;
            app.monitors = m2;
            app.soak = soak.map(dsview_gui::soak::Soak::new);
            app.bench = bench.map(|secs| app::Bench {
                until: Instant::now() + Duration::from_secs_f64(secs),
                frames: Vec::new(),
                cpu: Vec::new(),
                last: None,
                step: 7,
            });
            Ok(Box::new(app))
        }),
        &event_loop,
    );
    let mut probe = MonitorProbe {
        inner,
        monitors,
        wayland: is_wayland(&event_loop),
        probed: false,
    };
    let r = event_loop.run_app(&mut probe).map_err(eframe::Error::from);
    drop(server);
    session.exit();
    r
}
