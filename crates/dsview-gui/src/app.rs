//! The main window: menu, device line, the time view, the status bar.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use dsview_session::Session;
use dsview_session::session::SessionEvent;
use eframe::egui;

use crate::hooks::Shared;
use crate::protocol::ProtocolDock;
use crate::timeview::TimeView;
use crate::toolbar::SamplingBar;

/// A file chosen in a (blocking) native dialog on its own thread.
enum FileJob {
    Open(PathBuf),
    Save(PathBuf),
    Export(PathBuf),
    LoadProfile(PathBuf),
    StoreProfile(PathBuf),
    Screenshot(PathBuf),
}

pub struct Bench {
    pub until: Instant,
    pub frames: Vec<f64>,
    pub cpu: Vec<f64>,
    pub last: Option<Instant>,
    pub step: i64,
}

pub struct App {
    pub session: Session,
    pub view: TimeView,
    pub bar: SamplingBar,
    pub protocol: ProtocolDock,
    pub dso_trigger: crate::dsotrigger::DsoTriggerDock,
    pub trigger: crate::triggerdock::TriggerDock,
    pub measure: crate::measuredock::MeasureDock,
    pub search: crate::searchdock::SearchDock,
    pub calibration: Option<crate::calibration::Calibration>,
    pub store: Option<crate::storedlg::StoreDialog>,
    /// A screenshot to save here once the frame is grabbed.
    screenshot: Option<PathBuf>,
    ctx_for_shot: bool,
    shot_result: Option<Receiver<Result<(), String>>>,
    /// The application options and their dialog's working copy.
    pub options: crate::options::AppOptions,
    options_edit: Option<crate::options::AppOptions>,
    pub fft: crate::fftview::FftState,
    pub shared: Shared,
    pub version: String,
    events: Receiver<SessionEvent>,
    files: Option<Receiver<Option<FileJob>>>,
    message: Option<String>,
    switch_prompt: Option<(u64, String)>,
    known_devices: usize,
    pub bench: Option<Bench>,
    pub soak: Option<crate::soak::Soak>,
}

impl App {
    pub fn new(session: Session, shared: Shared, version: &str, ctx: &egui::Context) -> App {
        let _ = shared.ctx.set(ctx.clone());
        // repaint when the session changes (events arrive on other threads)
        let events = session.subscribe();
        let (tx, rx) = channel();
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("gui-events".into())
            .spawn(move || {
                for ev in events {
                    if tx.send(ev).is_err() {
                        break;
                    }
                    c.request_repaint();
                }
            })
            .expect("event thread");
        App {
            session,
            view: TimeView::default(),
            bar: SamplingBar::default(),
            protocol: ProtocolDock::default(),
            dso_trigger: crate::dsotrigger::DsoTriggerDock::default(),
            trigger: crate::triggerdock::TriggerDock::default(),
            measure: crate::measuredock::MeasureDock::default(),
            search: crate::searchdock::SearchDock::default(),
            calibration: None,
            store: None,
            screenshot: None,
            ctx_for_shot: false,
            shot_result: None,
            options: crate::options::AppOptions::default(),
            options_edit: None,
            fft: crate::fftview::FftState::default(),
            shared,
            version: version.into(),
            events: rx,
            files: None,
            message: None,
            switch_prompt: None,
            known_devices: 0,
            bench: None,
            soak: None,
        }
    }

    fn pick(
        &mut self,
        ctx: &egui::Context,
        title: &'static str,
        make: fn(PathBuf) -> FileJob,
        filter: (&'static str, &'static [&'static str]),
        save: bool,
    ) {
        if self.files.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.shared.lock().dialog = Some(title.into());
        let c = ctx.clone();
        std::thread::spawn(move || {
            let d = rfd::FileDialog::new().set_title(title);
            let d = d.add_filter(filter.0, filter.1);
            let path = match save {
                true => d.save_file(),
                false => d.pick_file(),
            };
            let _ = tx.send(path.map(make));
            c.request_repaint();
        });
        self.files = Some(rx);
    }

    fn file_jobs(&mut self) {
        let Some(rx) = &self.files else { return };
        let Ok(job) = rx.try_recv() else { return };
        self.files = None;
        self.shared.lock().dialog = None;
        let result = match job {
            None => Ok(()),
            Some(FileJob::Open(p)) => self
                .session
                .set_file(&p.to_string_lossy())
                .map_err(|_| format!("Failed to load {}", p.display())),
            Some(FileJob::Save(mut p)) => {
                if p.extension().is_none_or(|e| e != "dsl") {
                    p.set_extension("dsl");
                }
                if self.session.have_view_data() {
                    self.store = Some(crate::storedlg::StoreDialog::new(
                        p,
                        false,
                        self.options.original_data,
                    ));
                    Ok(())
                } else {
                    Err("No data to save.".into())
                }
            }
            Some(FileJob::LoadProfile(p)) => self.session.load_profile(&p),
            Some(FileJob::StoreProfile(mut p)) => {
                if p.extension().is_none_or(|e| e != "dsc") {
                    p.set_extension("dsc");
                }
                self.session.store_profile(&p)
            }
            Some(FileJob::Screenshot(mut p)) => {
                if p.extension().is_none_or(|e| e != "png" && e != "jpeg") {
                    p.set_extension("png");
                }
                // the next frame's picture comes back as an event
                self.screenshot = Some(p);
                self.ctx_for_shot = true;
                Ok(())
            }
            Some(FileJob::Export(p)) => {
                if self.session.have_view_data() {
                    self.store = Some(crate::storedlg::StoreDialog::new(
                        p,
                        true,
                        self.options.original_data,
                    ));
                    Ok(())
                } else {
                    Err("No data to export.".into())
                }
            }
        };
        if let Err(e) = result {
            self.message = Some(e);
        }
    }

    /// `on_screenShot`: asks for the frame; the callback saves it and
    /// reports back.
    fn screenshots(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.ctx_for_shot)
            && let Some(path) = self.screenshot.take()
        {
            let (tx, rx) = channel();
            self.shot_result = Some(rx);
            let c = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                egui::ScreenshotCallback::new(move |img: std::sync::Arc<egui::ColorImage>| {
                    let [w, h] = img.size;
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                    let r = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                        .ok_or_else(|| "bad screenshot".to_string())
                        .and_then(|i| {
                            if path.extension().is_some_and(|e| e == "jpeg") {
                                image::DynamicImage::ImageRgba8(i)
                                    .to_rgb8()
                                    .save(&path)
                                    .map_err(|e| e.to_string())
                            } else {
                                i.save(&path).map_err(|e| e.to_string())
                            }
                        });
                    let _ = tx.send(r);
                    c.request_repaint();
                }),
            ));
        }
        if let Some(rx) = &self.shot_result
            && let Ok(r) = rx.try_recv()
        {
            self.shot_result = None;
            if let Err(e) = r {
                self.message = Some(e);
            }
        }
    }

    /// Devices came or went (`DSV_MSG_CURRENT_DEVICE_DETACHED`,
    /// `DSV_MSG_NEW_USB_DEVICE`).
    fn hotplug(&mut self) {
        let (list, _) = self.session.devices();
        let active = self.session.state().device.as_ref().map(|d| d.handle);
        if let Some(h) = active
            && !list.iter().any(|e| e.handle == h)
        {
            self.message = Some("The current device was disconnected.".into());
            if !self.session.is_working() {
                self.session.set_default_device();
            }
        } else if list.len() > self.known_devices && self.known_devices > 0 {
            self.switch_prompt = list.last().map(|e| (e.handle, e.name.clone()));
        }
        self.known_devices = list.len();
    }

    fn control_ops(&mut self) {
        let (region, cursors) = {
            let mut p = self.shared.lock();
            (p.region.take(), p.cursors.take())
        };
        if let Some((a, b)) = region {
            self.view.show_region(&self.session, a, b);
        }
        if let Some(c) = cursors {
            self.view.cursors.samples = c;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        let mut data = false;
        let mut devices = false;
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                SessionEvent::DataAppended => data = true,
                SessionEvent::DeviceListChanged => devices = true,
                _ => {}
            }
        }
        // the view follows a stream or loop capture (`set_receive_len`)
        if data && self.session.is_realtime_refresh() && self.options.auto_scroll_latest_data {
            let tb = crate::timeview::timebase(&self.session);
            let ring = self
                .session
                .data()
                .logic
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .ring_sample_count();
            self.view.vp.update_limits(&tb);
            self.view.vp.scroll_to_last(ring, &tb);
        }
        if devices {
            self.hotplug();
        }
        // S and I: run/stop and single; T, D, M, R: the trigger, protocol,
        // measure and search docks (MainWindow's shortcuts)
        let (key_s, key_i, key_t, key_d, key_m, key_r) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::I),
                i.key_pressed(egui::Key::T),
                i.key_pressed(egui::Key::D),
                i.key_pressed(egui::Key::M),
                i.key_pressed(egui::Key::R),
            )
        });
        if !ctx.egui_wants_keyboard_input() {
            if key_t {
                self.dso_trigger.open = !self.dso_trigger.open;
            }
            if key_d {
                self.protocol.open = !self.protocol.open;
            }
            if key_m {
                self.measure.open = !self.measure.open;
            }
            if key_r {
                self.search.open = !self.search.open;
            }
            if key_s {
                self.bar.run_stop(&self.session, false);
            }
            if key_i {
                self.bar.run_stop(&self.session, true);
            }
        }
        self.file_jobs();
        self.screenshots(&ctx);
        self.control_ops();

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        self.pick(
                            &ctx,
                            "Open File",
                            FileJob::Open,
                            ("DSView data", &["dsl", "demo"]),
                            false,
                        );
                        ui.close();
                    }
                    if ui.button("Save…").clicked() {
                        self.pick(
                            &ctx,
                            "Save File",
                            FileJob::Save,
                            ("DSView data", &["dsl"]),
                            true,
                        );
                        ui.close();
                    }
                    if ui.button("Export…").clicked() {
                        self.pick(
                            &ctx,
                            "Export Data",
                            FileJob::Export,
                            ("CSV or VCD", &["csv", "vcd"]),
                            true,
                        );
                        ui.close();
                    }
                    ui.separator();
                    // the profile menu (FileBar's session menu)
                    if ui.button("Load profile…").clicked() {
                        self.pick(
                            &ctx,
                            "Open Session",
                            FileJob::LoadProfile,
                            ("DSView Session", &["dsc"]),
                            false,
                        );
                        ui.close();
                    }
                    if ui.button("Store profile…").clicked() {
                        self.pick(
                            &ctx,
                            "Save Session",
                            FileJob::StoreProfile,
                            ("DSView Session", &["dsc"]),
                            true,
                        );
                        ui.close();
                    }
                    if ui.button("Default profile").clicked() {
                        match self.session.default_profile_path() {
                            Some(p) => {
                                if let Err(e) = self.session.load_profile(&p) {
                                    self.message = Some(e);
                                }
                            }
                            None => self.message = Some("No device.".into()),
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Screenshot…").clicked() {
                        self.pick(
                            &ctx,
                            "Save As",
                            FileJob::Screenshot,
                            ("png file", &["png", "jpeg"]),
                            true,
                        );
                        ui.close();
                    }
                });
                ui.menu_button("Options", |ui| {
                    if ui.button("Application options…").clicked() {
                        self.options_edit = Some(self.options.clone());
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.dso_trigger.open, "Trigger (T)");
                    ui.checkbox(&mut self.protocol.open, "Protocol (D)");
                    ui.checkbox(&mut self.measure.open, "Measure (M)");
                    ui.checkbox(&mut self.search.open, "Search (R)");
                    ui.checkbox(&mut self.view.dso.split, "Split DSO channels");
                    ui.horizontal(|ui| {
                        let l = ui.label("DSO persistence");
                        ui.add(
                            egui::DragValue::new(&mut self.view.dso.persist.ms)
                                .range(0..=10000)
                                .suffix(" ms"),
                        )
                        .labelled_by(l.id);
                    });
                });
                let dso = self.session.mode() == dsview_core::Mode::Dso as i32;
                ui.add_enabled_ui(dso, |ui| {
                    ui.menu_button("Functions", |ui| {
                        if ui.button("FFT…").clicked() {
                            self.fft.open_dialog();
                            ui.close();
                        }
                        if ui.button("Math…").clicked() {
                            self.view.dso.math.open_dialog();
                            ui.close();
                        }
                        if ui.button("Lissajous…").clicked() {
                            self.view.dso.lissajous.open_dialog(&self.session);
                            ui.close();
                        }
                        if ui.button("Reference…").clicked() {
                            self.view.dso.refs.dialog = Some(0);
                            ui.close();
                        }
                        if ui.button("Histogram…").clicked() {
                            self.view.dso.analysis.histogram = Some((0, false));
                            ui.close();
                        }
                        if ui.button("Channel measure…").clicked() {
                            self.view.dso.analysis.chan_measure = Some((0, 1));
                            ui.close();
                        }
                    });
                });
                ui.separator();
                let dev = self.session.state().device.as_ref().map(|d| d.name.clone());
                ui.label(dev.unwrap_or_else(|| "no device".into()));
            });
        });
        egui::Panel::top("sampling").show(ui, |ui| {
            self.bar.show(ui, &self.session);
        });
        let dso_mode = self.session.mode() == dsview_core::Mode::Dso as i32;
        let logic_mode = self.session.mode() == dsview_core::Mode::Logic as i32;
        if logic_mode && self.search.open {
            egui::Panel::bottom("search").show(ui, |ui| {
                self.search.show(ui, &self.session, &mut self.view);
            });
        } else if !self.search.open {
            self.view.search = None;
        }
        self.search.poll(&ctx, &self.session, &mut self.view);
        if let Some(m) = self.search.message.take() {
            self.message = Some(m);
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            if dso_mode {
                self.view.dso_measure_bar(ui, &self.session);
                return;
            }
            ui.horizontal(|ui| {
                if let Some(m) = &self.view.measure {
                    ui.label(format!(
                        "Width: {} ({})   Period: {} ({})   Frequency: {}   Duty: {}",
                        m.width_time,
                        m.width_samples,
                        m.period_time,
                        m.period_samples,
                        m.frequency,
                        m.duty
                    ));
                } else if let Some((r, f)) = self.view.edges {
                    ui.label(format!("Rising: {r}   Falling: {f}   Edges: {}", r + f));
                } else {
                    ui.label(" ");
                }
            });
        });
        if self.protocol.open {
            egui::Panel::right("protocol")
                .resizable(true)
                .default_size(360.0)
                .show(ui, |ui| {
                    self.protocol.cursors = self.view.cursors.samples.clone();
                    self.protocol.formats = self.options.protocol_formats.clone();
                    self.protocol.show(ui, &self.session);
                    if std::mem::take(&mut self.protocol.formats_changed) {
                        self.options.protocol_formats = self.protocol.formats.clone();
                        self.options.save();
                    }
                });
        }
        let dso = self.session.mode() == dsview_core::Mode::Dso as i32;
        if self.measure.open {
            egui::Panel::right("measure")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.measure.show(ui, &self.session, &mut self.view);
                    });
                });
        }
        let logic = self.session.mode() == dsview_core::Mode::Logic as i32;
        if logic && self.dso_trigger.open {
            egui::Panel::right("trigger")
                .resizable(true)
                .default_size(330.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.trigger.show(ui, &self.session);
                    });
                });
        }
        if let Some(m) = self.bar.message.take() {
            self.message = Some(m);
        }
        if let Some(m) = self.trigger.message.take() {
            self.message = Some(m);
        }
        if dso && self.dso_trigger.open {
            egui::Panel::right("dso-trigger")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| {
                    self.dso_trigger.show(ui, &self.session);
                });
        }
        if let Some(m) = self.dso_trigger.message.take() {
            self.message = Some(m);
        }
        self.view.map_zoom_fixed = self.protocol.map_zoom_fixed;
        if let Some((a, b)) = self.protocol.jump.take() {
            self.view.show_region(&self.session, a, b.max(a + 1));
        }
        if let Some(m) = self.protocol.message.take() {
            self.message = Some(m);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if dso && self.fft.active().is_some() {
                // the FFT pane under the time view (Qt's second viewport)
                let r = ui.max_rect();
                let split = r.top() + r.height() * 0.6;
                let top = egui::Rect::from_min_max(r.min, egui::pos2(r.right(), split - 2.0));
                let bottom = egui::Rect::from_min_max(egui::pos2(r.left(), split + 2.0), r.max);
                ui.scope_builder(egui::UiBuilder::new().max_rect(top), |ui| {
                    self.view.show(ui, &self.session);
                });
                ui.scope_builder(egui::UiBuilder::new().max_rect(bottom), |ui| {
                    self.fft.show(ui, &self.session);
                });
            } else if self.search.busy() || self.store.as_ref().is_some_and(|d| d.busy()) {
                // the search holds the logic store; the view waits for it
                ui.centered_and_justified(|ui| ui.label("Searching…"));
            } else {
                self.view.show(ui, &self.session);
            }
        });
        // check_calibration(): DSO hardware in calibration mode
        if std::mem::take(&mut self.bar.calibrate)
            && self.session.mode() == dsview_core::Mode::Dso as i32
            && self
                .session
                .config(dsview_core::key::CALI, None)
                .is_some_and(|v| v.get_bool())
        {
            self.calibration = Some(crate::calibration::Calibration::open(&self.session));
        }
        if let Some(c) = &mut self.calibration {
            c.show(&ctx, &self.session);
            if !c.open {
                self.calibration = None;
            }
        }
        if let Some(d) = &mut self.store {
            let cursors = self.view.cursors.samples.clone();
            let out = d.show(&ctx, &self.session, &cursors, &self.version);
            if let Some(m) = d.message.take() {
                self.message = Some(m);
            }
            if d.busy() && d.original() != self.options.original_data {
                self.options.original_data = d.original();
                self.options.save();
            }
            if matches!(out, crate::storedlg::Outcome::Done) {
                self.store = None;
            }
        }
        if let Some(mut o) = self.options_edit.take() {
            let mut done = None;
            egui::Window::new("Application Options")
                .collapsible(false)
                .show(&ctx, |ui| done = o.dialog(ui));
            match done {
                Some(true) => {
                    o.apply(&mut self.view, &ctx);
                    o.save();
                    self.options = o;
                }
                Some(false) => {}
                None => self.options_edit = Some(o),
            }
        }
        self.fft.dialog(&ctx, &self.session);
        let names: Vec<(u16, String)> = self
            .session
            .state()
            .signals
            .iter()
            .filter(|x| x.ty == dsview_core::channel_type::DSO)
            .map(|x| (x.index, x.name.clone()))
            .collect();
        self.view.dso.math.dialog(&ctx, &names);
        self.view.dso.lissajous.dialog(&ctx, &self.session, &names);
        if let Some((handle, name)) = self.switch_prompt.clone() {
            egui::Window::new("New device")
                .collapsible(false)
                .show(&ctx, |ui| {
                    ui.label(format!("\"{name}\" was connected. Switch to it?"));
                    ui.horizontal(|ui| {
                        if ui.button("Switch").clicked() {
                            if !self.session.is_working() {
                                self.session.set_device(handle);
                            }
                            self.switch_prompt = None;
                        }
                        if ui.button("Keep").clicked() {
                            self.switch_prompt = None;
                        }
                    });
                });
        }
        if let Some(msg) = self.message.clone() {
            let mut open = true;
            egui::Window::new("DSView")
                .open(&mut open)
                .collapsible(false)
                .show(&ctx, |ui| {
                    ui.label(msg);
                });
            if !open {
                self.message = None;
            }
        }

        if let Some(soak) = &mut self.soak {
            if !self.session.is_working() {
                println!("soak: the capture stopped");
                std::process::exit(1);
            }
            if soak.frame() {
                self.session.stop_capture();
                let ok = soak.report(self.session.state().captures);
                self.session.exit();
                std::process::exit(if ok { 0 } else { 1 });
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        if let Some(b) = &mut self.bench {
            let tb = crate::timeview::timebase(&self.session);
            let max = self.view.vp.max_offset(&tb);
            let mut off = self.view.vp.x_offset + b.step;
            if off <= 0 || off >= max {
                b.step = -b.step;
                off = off.clamp(0, max.max(0));
            }
            self.view.vp.set_scale_offset(self.view.vp.scale, off, &tb);
            let now = Instant::now();
            if let Some(l) = b.last {
                b.frames.push((now - l).as_secs_f64() * 1000.0);
                b.cpu.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            b.last = Some(now);
            if now >= b.until {
                crate::report(b);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        }
    }
}
