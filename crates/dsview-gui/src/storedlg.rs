//! Saving and exporting with a range (`pv/dialogs/storeprogress.cpp`): the
//! file, start and end cursors (logic data), original or compressed data
//! for CSV, then the work in a thread with a progress window.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use dsview_core::Mode;
use dsview_formats::export::Format;
use dsview_session::Session;
use dsview_session::store::Progress;
use eframe::egui;

type StoreResult = Result<(), String>;

pub struct StoreDialog {
    path: PathBuf,
    export: bool,
    /// Cursor numbers, 0 for "-".
    start: usize,
    end: usize,
    original: bool,
    running: Option<(Receiver<StoreResult>, Arc<Progress>)>,
    pub message: Option<String>,
}

pub enum Outcome {
    Open,
    Done,
}

impl StoreDialog {
    pub fn new(path: PathBuf, export: bool, original: bool) -> StoreDialog {
        StoreDialog {
            path,
            export,
            start: 0,
            end: 0,
            original,
            running: None,
            message: None,
        }
    }

    /// "Original data" as chosen (kept in the options on export, as Qt).
    pub fn original(&self) -> bool {
        self.original
    }

    /// The work runs: the views wait (it holds the stores).
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        s: &Session,
        cursors: &[u64],
        version: &str,
    ) -> Outcome {
        if let Some((rx, prog)) = &self.running {
            match rx.try_recv() {
                Ok(r) => {
                    s.set_saving(false);
                    self.running = None;
                    if let Err(e) = r {
                        self.message = Some(e);
                    }
                    return Outcome::Done;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    egui::Window::new(if self.export {
                        "Exporting..."
                    } else {
                        "Saving..."
                    })
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .min_width(500.0)
                    .show(ctx, |ui| {
                        let p = prog.percent();
                        ui.add(egui::ProgressBar::new(p as f32 / 100.0).text(format!("{p}%")));
                        // closing the window while it runs cancels it
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            prog.cancel
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return Outcome::Open;
                }
                Err(_) => {
                    s.set_saving(false);
                    self.running = None;
                    return Outcome::Done;
                }
            }
        }
        let logic = s.mode() == Mode::Logic as i32;
        let csv = self.path.extension().is_some_and(|e| e == "csv");
        let mut out = Outcome::Open;
        let mut open = true;
        egui::Window::new(if self.export { "Export" } else { "Save" })
            .collapsible(false)
            .open(&mut open)
            .min_width(500.0)
            .show(ctx, |ui| {
                ui.label(self.path.display().to_string());
                if logic {
                    let names: Vec<String> = std::iter::once("-".to_string())
                        .chain((1..=cursors.len()).map(|i| format!("Cursor{i}")))
                        .collect();
                    egui::Grid::new("store-range")
                        .num_columns(2)
                        .show(ui, |ui| {
                            for (label, v) in [("Start", &mut self.start), ("End", &mut self.end)] {
                                let l = ui.label(label);
                                *v = (*v).min(cursors.len());
                                egui::ComboBox::from_id_salt(("store", label))
                                    .selected_text(names[*v].clone())
                                    .show_ui(ui, |ui| {
                                        for (i, n) in names.iter().enumerate() {
                                            ui.selectable_value(v, i, n);
                                        }
                                    })
                                    .response
                                    .labelled_by(l.id);
                                ui.end_row();
                            }
                        });
                    if self.export && csv {
                        ui.radio_value(&mut self.original, true, "Original data");
                        ui.radio_value(&mut self.original, false, "Compressed data");
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        out = self.accept(s, cursors, logic, version);
                    }
                    if ui.button("Cancel").clicked() {
                        out = Outcome::Done;
                    }
                });
            });
        if !open {
            out = Outcome::Done;
        }
        out
    }

    /// `accept()`: the range checks, then the work in a thread.
    fn accept(&mut self, s: &Session, cursors: &[u64], logic: bool, version: &str) -> Outcome {
        let mut range = None;
        if logic {
            let at = |n: usize| {
                if n > 0 {
                    cursors.get(n - 1).copied().unwrap_or(0)
                } else {
                    0
                }
            };
            let (mut a, mut b) = (at(self.start), at(self.end));
            if self.start > 0 && self.end > 0 {
                if self.start == self.end {
                    self.message = Some("Data range error".into());
                    return Outcome::Open;
                }
                let total = s
                    .data()
                    .logic
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .ring_sample_count();
                if a > total && b > total {
                    self.message = Some("No data in the data range".into());
                    return Outcome::Open;
                }
                if a > b {
                    std::mem::swap(&mut a, &mut b);
                }
            }
            range = Some((a, b));
        }
        let (tx, rx) = channel();
        let prog = Arc::new(Progress::default());
        let prog2 = prog.clone();
        let (s2, path, export, original, version) = (
            s.clone(),
            self.path.clone(),
            self.export,
            self.original,
            version.to_owned(),
        );
        s.set_saving(true);
        std::thread::Builder::new()
            .name("store".into())
            .spawn(move || {
                let r = if export {
                    let fmt = match path.extension().and_then(|e| e.to_str()) {
                        Some("vcd") => Format::Vcd,
                        _ => Format::Csv,
                    };
                    dsview_session::store::export_range_with(
                        &s2, &path, fmt, range, original, &prog2,
                    )
                } else {
                    dsview_session::store::save_dsl_range_with(&s2, &path, &version, range, &prog2)
                };
                let _ = tx.send(r);
            })
            .ok();
        self.running = Some((rx, prog));
        Outcome::Open
    }
}
