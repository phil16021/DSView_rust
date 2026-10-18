//! The main window: menu, device line, the time view, the status bar.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use dsview_session::Session;
use dsview_session::session::SessionEvent;
use eframe::egui;

use crate::hooks::Shared;
use crate::timeview::TimeView;

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
    pub shared: Shared,
    pub version: String,
    events: Receiver<SessionEvent>,
    files: Option<Receiver<Option<FileJob>>>,
    message: Option<String>,
    pub bench: Option<Bench>,
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
            shared,
            version: version.into(),
            events: rx,
            files: None,
            message: None,
            bench: None,
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
        while self.events.try_recv().is_ok() {}
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
                ui.separator();
                let dev = self.session.state().device.as_ref().map(|d| d.name.clone());
                ui.label(dev.unwrap_or_else(|| "no device".into()));
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| {
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
        egui::CentralPanel::default().show(ui, |ui| {
            self.view.show(ui, &self.session);
        });
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
