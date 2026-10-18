//! `dsview`: DSView's GUI in Rust (docs/migration/PLAN.md, Phase 5).
//!
//! M6.0 state: a viewer of the active logic capture, painted from
//! `dsview-viewmodel`, to settle the rendering path. `--bench SECONDS`
//! redraws continuously while panning and logs frame times, then exits.
//! The renderer is glow (OpenGL) unless `DSVIEW_RENDERER=wgpu`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsview_core::channel_type;
use dsview_session::{Paths, Session};
use dsview_viewmodel::logic::{EdgeBuffers, logic_shape};
use dsview_viewmodel::viewport::{Timebase, Viewport};
use eframe::egui;

/// DSView's version (CMakeLists.txt `DS_VERSION_STRING`).
const DSVIEW_VERSION: &str = "1.6.1";
/// Trace rows: height and gap (logic mode default without the GUI's
/// layout rules, which come with M6a).
const ROW_HEIGHT: f32 = 30.0;
const ROW_GAP: f32 = 8.0;
const HEADER_WIDTH: f32 = 90.0;

struct Bench {
    until: Instant,
    frames: Vec<Duration>,
    cpu: Vec<Duration>,
    last: Option<Instant>,
    step: i64,
}

struct App {
    session: Session,
    vp: Viewport,
    bufs: EdgeBuffers,
    bench: Option<Bench>,
}

impl App {
    fn timebase(&self) -> Timebase {
        let st = self.session.state();
        Timebase {
            snap_samplerate: st.cur_snap_samplerate as f64,
            samplerate: self.session.sample_rate() as f64,
            sample_limit: st.cur_samplelimits,
            dso_view_time: None,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let tb = self.timebase();
        let rect = ui.max_rect();
        let width = (rect.width() - HEADER_WIDTH).max(1.0) as i32;
        if self.vp.width != width {
            self.vp.width = width;
            self.vp.update_limits(&tb);
        }

        // input: wheel zooms at the pointer, drag pans
        let area =
            egui::Rect::from_min_max(egui::pos2(rect.left() + HEADER_WIDTH, rect.top()), rect.max);
        let resp = ui.interact(area, ui.make_persistent_id("traces"), egui::Sense::drag());
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && let Some(p) = resp.hover_pos()
        {
            self.vp
                .zoom(scroll as f64 / 50.0, (p.x - area.left()) as i32, &tb);
        }
        if resp.dragged() {
            let off = self.vp.x_offset - resp.drag_delta().x as i64;
            self.vp.set_scale_offset(self.vp.scale, off, &tb);
        }
        if let Some(b) = &mut self.bench {
            // pan back and forth across the capture
            let max = self.vp.max_offset(&tb);
            let mut off = self.vp.x_offset + b.step;
            if off <= 0 || off >= max {
                b.step = -b.step;
                off = off.clamp(0, max.max(0));
            }
            self.vp.set_scale_offset(self.vp.scale, off, &tb);
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(30));
        let mut signals = self.session.state().signals.clone();
        if std::env::var_os("DSVIEW_BENCH_EMPTY").is_some() {
            signals.clear();
        }
        let store = self
            .session
            .data()
            .logic
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let end_align = store.ring_sample_count().saturating_sub(1);
        let mut shapes = Vec::new();
        for (row, sig) in signals
            .iter()
            .filter(|s| s.ty == channel_type::LOGIC)
            .enumerate()
        {
            let top = rect.top() + ROW_GAP + row as f32 * (ROW_HEIGHT + ROW_GAP);
            if top > rect.bottom() {
                break;
            }
            painter.text(
                egui::pos2(rect.left() + 6.0, top + ROW_HEIGHT / 2.0),
                egui::Align2::LEFT_CENTER,
                &sig.name,
                egui::FontId::proportional(13.0),
                egui::Color32::from_gray(220),
            );
            let shape = logic_shape(
                &store,
                sig.index,
                &self.vp,
                &tb,
                width as u16,
                (top - rect.top() + ROW_HEIGHT) as i32,
                ROW_HEIGHT as i32,
                end_align,
                &mut self.bufs,
            );
            let colour = egui::Color32::from_rgb(0x6c, 0xa0, 0xdc);
            let ox = area.left();
            let oy = rect.top();
            // one mesh of 1 px rectangles per trace
            let mut mesh = egui::Mesh::default();
            for l in &shape.lines {
                let (x0, x1) = (l.x0.min(l.x1), l.x0.max(l.x1));
                let (y0, y1) = (l.y0.min(l.y1), l.y0.max(l.y1));
                mesh.add_colored_rect(
                    egui::Rect::from_min_max(
                        egui::pos2(ox + x0, oy + y0),
                        egui::pos2(ox + x1 + 1.0, oy + y1 + 1.0),
                    ),
                    colour,
                );
            }
            shapes.push(egui::Shape::mesh(mesh));
            for &(x, y) in &shape.dots {
                shapes.push(egui::Shape::circle_filled(
                    egui::pos2(ox + x, oy + y),
                    2.0,
                    colour,
                ));
            }
        }
        drop(store);
        painter.extend(shapes);

        let cpu = t0.elapsed();
        if let Some(b) = &mut self.bench {
            let now = Instant::now();
            if let Some(l) = b.last {
                b.frames.push(now - l);
                b.cpu.push(cpu);
            }
            b.last = Some(now);
            if now >= b.until {
                report(b, width);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ui.ctx().request_repaint();
        }
    }
}

fn report(b: &Bench, width: i32) {
    let mut f: Vec<f64> = b.frames.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    let mut c: Vec<f64> = b.cpu.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    f.sort_by(f64::total_cmp);
    c.sort_by(f64::total_cmp);
    let pct = |v: &[f64], p: f64| {
        v.get(((v.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let renderer = std::env::var("DSVIEW_RENDERER").unwrap_or_else(|_| "glow".into());
    println!(
        "bench {renderer}: {} frames, trace width {width} px; frame ms median {:.1} p95 {:.1} \
         ({:.1} fps); ui cpu ms median {:.2} p95 {:.2}",
        f.len(),
        pct(&f, 0.5),
        pct(&f, 0.95),
        1000.0 / pct(&f, 0.5).max(1e-9),
        pct(&c, 0.5),
        pct(&c, 0.95),
    );
}

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

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    let (mut data_dir, mut bench) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => data_dir = args.next().map(PathBuf::from),
            "--bench" => bench = args.next().and_then(|v| v.parse::<f64>().ok()),
            other => {
                eprintln!("dsview: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    let decoders = {
        let own = data_dir.join("decoders");
        let shared = data_dir.join("../libsigrokdecode4DSL/decoders");
        if !own.is_dir() && shared.is_dir() {
            shared
        } else {
            own
        }
    };
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
    // the demo logic analyzer with protocol.demo, captured once
    let (list, _) = session.devices();
    if let Some(e) = list.iter().find(|e| e.name == "Demo Logic") {
        session.set_device(e.handle);
    }
    session.start_capture(false);
    session.wait(Duration::from_secs(30), |st| {
        st.ended_seq > 0 && !st.working
    });

    let renderer = match std::env::var("DSVIEW_RENDERER").as_deref() {
        Ok("wgpu") => eframe::Renderer::Wgpu,
        _ => eframe::Renderer::Glow,
    };
    let options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("DSView (Rust preview)")
            .with_inner_size(
                std::env::var("DSVIEW_BENCH_SIZE")
                    .ok()
                    .and_then(|v| {
                        let (w, h) = v.split_once('x')?;
                        Some([w.parse().ok()?, h.parse().ok()?])
                    })
                    .unwrap_or([1920.0, 1080.0]),
            ),
        ..Default::default()
    };
    let s2 = session.clone();
    let r = eframe::run_native(
        "dsview",
        options,
        Box::new(move |cc| {
            if std::env::var_os("DSVIEW_FEATHER").is_none() {
                // logic traces are axis-aligned 1 px lines: no anti-aliasing
                // needed, and software GL rasterises far less
                cc.egui_ctx
                    .tessellation_options_mut(|o| o.feathering = false);
            }
            let tb = {
                let st = s2.state();
                Timebase {
                    snap_samplerate: st.cur_snap_samplerate as f64,
                    samplerate: s2.sample_rate() as f64,
                    sample_limit: st.cur_samplelimits,
                    dso_view_time: None,
                }
            };
            let mut vp = Viewport {
                width: 1800,
                ..Viewport::default()
            };
            vp.update_limits(&tb);
            // about 20 samples per pixel: dense but not the full overview
            vp.set_scale_offset(20.0 / tb.snap_samplerate, 1500, &tb);
            Ok(Box::new(App {
                session: s2,
                vp,
                bufs: EdgeBuffers::default(),
                bench: bench.map(|secs| Bench {
                    until: Instant::now() + Duration::from_secs_f64(secs),
                    frames: Vec::new(),
                    cpu: Vec::new(),
                    last: None,
                    step: 7,
                }),
            }))
        }),
    );
    session.exit();
    r
}
