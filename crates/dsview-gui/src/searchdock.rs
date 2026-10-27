//! The search dock (`pv/dock/searchdock.cpp`) and its pattern dialog
//! (`pv/dialogs/search.cpp`): a pattern of X/0/1/R/F/C per logic channel,
//! searched forwards or backwards from the search cursor in a thread, with
//! a cancel button while it runs.

use crate::i18n::tr;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use dsview_core::channel_type;
use dsview_session::Session;
use eframe::egui::{self, Ui};

use crate::timeview::TimeView;

const NOTE: &str = "X: Don't care\n0: Low level\n1: High level\nR: Rising edge\nF: Falling edge\nC: Rising/Falling edge";

struct Job {
    title: &'static str,
    rx: Receiver<(bool, i64)>,
    stop: Arc<AtomicBool>,
    cancelled: bool,
}

#[derive(Default)]
pub struct SearchDock {
    pub open: bool,
    /// Channel → `X01RFC` (`_pattern`).
    pub pattern: BTreeMap<u16, u8>,
    dialog: Option<Vec<(u16, String, String)>>,
    job: Option<Job>,
    pub message: Option<String>,
}

impl SearchDock {
    /// Starts a search as the dock's buttons do (tests).
    #[doc(hidden)]
    pub fn search(&mut self, s: &Session, view: &mut TimeView, next: bool) {
        self.start(s, view, next);
    }

    /// A search is running (the logic store is locked by it).
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// The bar: previous, the pattern (click to edit), next.
    pub fn show(&mut self, ui: &mut Ui, s: &Session, view: &mut TimeView) {
        if view.search.is_none() {
            view.search = Some((0, false));
        }
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() / 2.0 - 120.0).max(0.0));
            if ui.button("<").on_hover_text(tr("Previous")).clicked() {
                self.start(s, view, false);
            }
            let label: String = self.pattern.values().map(|&c| c as char).collect();
            let text = if label.is_empty() {
                "search".to_string()
            } else {
                label
            };
            if ui
                .button(format!("Search: {text}"))
                .on_hover_text(tr("Search options"))
                .clicked()
            {
                self.dialog = Some(
                    s.state()
                        .signals
                        .iter()
                        .filter(|x| x.ty == channel_type::LOGIC)
                        .map(|x| {
                            let v = self
                                .pattern
                                .get(&x.index)
                                .map_or("X".to_string(), |&c| (c as char).to_string());
                            (x.index, x.name.clone(), v)
                        })
                        .collect(),
                );
            }
            if ui.button(">").on_hover_text(tr("Next")).clicked() {
                self.start(s, view, true);
            }
        });
        self.dialog(ui.ctx(), view);
    }

    /// `on_next` / `on_previous`.
    fn start(&mut self, s: &Session, view: &mut TimeView, next: bool) {
        if self.job.is_some() {
            return;
        }
        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
        if store.empty() {
            self.message = Some("No Sample data!".into());
            return;
        }
        let end = store.sample_count() as i64 - 1;
        let stop = store.search_stop_flag();
        drop(store);
        let (pos, hit) = view.search.unwrap_or((0, false));
        let mut from = pos as i64;
        if next {
            from += i64::from(hit);
            if from >= end {
                self.message = Some("Search cursor at the end position!".into());
                return;
            }
        } else {
            if from == 0 {
                self.message = Some("Search cursor at the start position!".into());
                return;
            }
            from -= i64::from(hit);
        }
        let (tx, rx) = channel();
        let s2 = s.clone();
        let pattern = self.pattern.clone();
        std::thread::Builder::new()
            .name("pattern-search".into())
            .spawn(move || {
                let store = s2.data().logic.lock().unwrap_or_else(|e| e.into_inner());
                let mut i = from;
                let r = store.pattern_search(0, end, &mut i, &pattern, next);
                let _ = tx.send((r, i));
            })
            .ok();
        self.job = Some(Job {
            title: if next {
                "Search Next..."
            } else {
                "Search Previous..."
            },
            rx,
            stop,
            cancelled: false,
        });
    }

    /// While searching: the progress window with Cancel; afterwards the
    /// result (`set_search_pos`, or "Pattern not found!").
    pub fn poll(&mut self, ctx: &egui::Context, s: &Session, view: &mut TimeView) {
        let Some(job) = &mut self.job else { return };
        match job.rx.try_recv() {
            Ok((found, pos)) => {
                let cancelled = job.cancelled;
                self.job = None;
                if found {
                    view.search = Some((pos.max(0) as u64, true));
                    view.center_on(s, pos.max(0) as u64);
                } else if !cancelled {
                    self.message = Some("Pattern not found!".into());
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                egui::Window::new(job.title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.spinner();
                        if ui.button(tr("Cancel")).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            job.cancelled = true;
                            job.stop.store(true, Ordering::Relaxed);
                        }
                    });
                ctx.request_repaint();
            }
            Err(_) => self.job = None,
        }
    }

    /// The pattern dialog: one character per logic channel.
    fn dialog(&mut self, ctx: &egui::Context, view: &mut TimeView) {
        let Some(mut rows) = self.dialog.take() else {
            return;
        };
        let mut keep = true;
        egui::Window::new(tr("Search Options"))
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::Grid::new("search-pattern")
                        .num_columns(3)
                        .show(ui, |ui| {
                            for (index, name, v) in rows.iter_mut() {
                                let l = ui.label(format!("{name}:"));
                                ui.label(index.to_string());
                                let r = ui
                                    .add(
                                        egui::TextEdit::singleline(v)
                                            .char_limit(1)
                                            .desired_width(20.0)
                                            .font(egui::TextStyle::Monospace),
                                    )
                                    .labelled_by(l.id);
                                if r.changed() {
                                    *v = v.to_uppercase();
                                    if !matches!(v.as_str(), "X" | "0" | "1" | "R" | "F" | "C") {
                                        v.clear();
                                    }
                                }
                                ui.end_row();
                            }
                        });
                    ui.label(NOTE);
                });
                ui.horizontal(|ui| {
                    if ui.button(tr("OK")).clicked() {
                        let new: BTreeMap<u16, u8> = rows
                            .iter()
                            .map(|(i, _, v)| (*i, v.bytes().next().unwrap_or(b'X')))
                            .collect();
                        if new != self.pattern {
                            // a new pattern searches from the cursor, not past it
                            if let Some((p, _)) = view.search {
                                view.search = Some((p, false));
                            }
                            self.pattern = new;
                        }
                        keep = false;
                    }
                    if ui.button(tr("Cancel")).clicked() || crate::msgbox::esc(ui.ctx()) {
                        keep = false;
                    }
                });
            });
        if keep {
            self.dialog = Some(rows);
        }
    }
}
