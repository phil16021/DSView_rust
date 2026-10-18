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
        save: Option<(&'static str, &'static [&'static str])>,
    ) {
        if self.files.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.shared.lock().dialog = Some(title.into());
        let c = ctx.clone();
        std::thread::spawn(move || {
            let d = rfd::FileDialog::new().set_title(title);
            let path = match save {
                Some((name, ext)) => d.add_filter(name, ext).save_file(),
                None => d.add_filter("DSView data", &["dsl", "demo"]).pick_file(),
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
                dsview_session::store::save_dsl(&self.session, &p, &self.version)
            }
            Some(FileJob::Export(p)) => {
                let fmt = match p.extension().and_then(|e| e.to_str()) {
                    Some("vcd") => dsview_formats::export::Format::Vcd,
                    _ => dsview_formats::export::Format::Csv,
                };
                dsview_session::store::export(&self.session, &p, fmt)
            }
        };
        if let Err(e) = result {
            self.message = Some(e);
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
        if data && self.session.is_realtime_refresh() {
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
        // S and I: run/stop and single, T: the trigger dock (MainWindow's
        // shortcuts)
        let (key_s, key_i, key_t) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::I),
                i.key_pressed(egui::Key::T),
            )
        });
        if !ctx.egui_wants_keyboard_input() {
            if key_t {
                self.dso_trigger.open = !self.dso_trigger.open;
            }
            if key_s {
                self.bar.run_stop(&self.session, false);
            }
            if key_i {
                self.bar.run_stop(&self.session, true);
            }
        }
        self.file_jobs();
        self.control_ops();

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        self.pick(&ctx, "Open File", FileJob::Open, None);
                        ui.close();
                    }
                    if ui.button("Save…").clicked() {
                        self.pick(
                            &ctx,
                            "Save File",
                            FileJob::Save,
                            Some(("DSView data", &["dsl"])),
                        );
                        ui.close();
                    }
                    if ui.button("Export…").clicked() {
                        self.pick(
                            &ctx,
                            "Export Data",
                            FileJob::Export,
                            Some(("CSV or VCD", &["csv", "vcd"])),
                        );
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.dso_trigger.open, "Trigger (T)");
                    ui.checkbox(&mut self.protocol.open, "Protocol (D)");
                    ui.checkbox(&mut self.view.dso.split, "Split DSO channels");
                });
                let dso = self.session.mode() == dsview_core::Mode::Dso as i32;
                ui.add_enabled_ui(dso, |ui| {
                    ui.menu_button("Functions", |ui| {
                        if ui.button("FFT…").clicked() {
                            self.fft.open_dialog();
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
                    self.protocol.show(ui, &self.session);
                });
        }
        let dso = self.session.mode() == dsview_core::Mode::Dso as i32;
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
            } else {
                self.view.show(ui, &self.session);
            }
        });
        self.fft.dialog(&ctx, &self.session);
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
