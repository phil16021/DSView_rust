//! The measure dock (`pv/dock/measuredock.cpp`): the mouse measurement,
//! time between two cursors, edges between two cursors on a channel, and
//! the cursor list (go to, delete). Rows are kept per work mode.

use dsview_core::channel_type;
use dsview_session::Session;
use dsview_viewmodel::measure;
use dsview_viewmodel::ruler::format_real_time;
use eframe::egui::{self, Color32, Ui};

use crate::timeview::{TimeView, cursor_colour};

/// A row measuring between cursors (numbers from 1; None: not chosen).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Row {
    c1: Option<usize>,
    c2: Option<usize>,
    /// Edge rows: the logic channel.
    channel: u16,
}

#[derive(Default, Clone)]
struct ModeRows {
    dist: Vec<Row>,
    edge: Vec<Row>,
}

#[derive(Default)]
pub struct MeasureDock {
    pub open: bool,
    /// Logic, DSO, analog (`_mode_rows`).
    rows: [ModeRows; 3],
}

fn mode_index(mode: i32) -> usize {
    match mode {
        m if m == dsview_core::Mode::Dso as i32 => 1,
        m if m == dsview_core::Mode::Analog as i32 => 2,
        _ => 0,
    }
}

/// A cursor's number button in its colour; a click opens the chooser.
fn cursor_button(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    sel: &mut Option<usize>,
    cursors: usize,
) {
    let text = sel.map_or(String::new(), |n| n.to_string());
    let fill = sel.map_or(ui.visuals().widgets.inactive.bg_fill, cursor_colour);
    let b = egui::Button::new(egui::RichText::new(text).color(Color32::WHITE))
        .fill(fill)
        .min_size(egui::vec2(24.0, 18.0));
    let resp = ui.add(b);
    let popup = ui.make_persistent_id(id);
    egui::Popup::menu(&resp).id(popup).show(|ui| {
        // `popup_all_coursors`: a grid of the cursors
        ui.horizontal_wrapped(|ui| {
            for n in 1..=cursors {
                let b = egui::Button::new(egui::RichText::new(n.to_string()).color(Color32::WHITE))
                    .fill(cursor_colour(n));
                if ui.add(b).clicked() {
                    *sel = Some(n);
                    ui.close();
                }
            }
        });
        if cursors == 0 {
            ui.label("No cursors");
        }
    });
}

impl MeasureDock {
    /// Adds a distance row and an edge row in logic mode (tests).
    #[doc(hidden)]
    pub fn add_logic_rows(&mut self, c1: usize, c2: usize, channel: u16) {
        let r = Row {
            c1: Some(c1),
            c2: Some(c2),
            channel,
        };
        self.rows[0].dist.push(r);
        self.rows[0].edge.push(r);
    }

    pub fn show(&mut self, ui: &mut Ui, s: &Session, view: &mut TimeView) {
        ui.heading("Measure");
        let rate = s.state().cur_snap_samplerate.max(1);
        let trig = s.state().trigger_pos;
        let cursors = view.cursors.samples.clone();
        let n = cursors.len();
        let rows = &mut self.rows[mode_index(s.mode())];
        // cursors past the end are forgotten (`update_dist`)
        for r in rows.dist.iter_mut().chain(rows.edge.iter_mut()) {
            r.c1 = r.c1.filter(|&c| c <= n);
            r.c2 = r.c2.filter(|&c| c <= n);
        }

        egui::CollapsingHeader::new("Mouse measurement")
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(&mut view.measure_enabled, "Enable floating measurement");
                let m = view.measure.clone();
                let get = |f: fn(&measure::MouseMeasure) -> &String| {
                    m.as_ref().map_or("########".to_string(), |m| f(m).clone())
                };
                egui::Grid::new("mm").num_columns(4).show(ui, |ui| {
                    ui.label("");
                    ui.label("Time");
                    ui.label("");
                    ui.label("Samples");
                    ui.end_row();
                    ui.label("W: ");
                    ui.label(get(|m| &m.width_time));
                    ui.label("/");
                    ui.label(get(|m| &m.width_samples));
                    ui.end_row();
                    ui.label("P: ");
                    ui.label(get(|m| &m.period_time));
                    ui.label("/");
                    ui.label(get(|m| &m.period_samples));
                    ui.end_row();
                    ui.label("D: ");
                    ui.label(get(|m| &m.duty));
                    ui.end_row();
                    ui.label("F: ");
                    ui.label(get(|m| &m.frequency));
                    ui.end_row();
                });
            });

        egui::CollapsingHeader::new("Cursor Distance")
            .default_open(true)
            .show(ui, |ui| {
                if ui.button("+").on_hover_text("Add").clicked() {
                    rows.dist.push(Row::default());
                }
                let mut del = None;
                for (i, r) in rows.dist.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("x").on_hover_text("Delete").clicked() {
                            del = Some(i);
                        }
                        cursor_button(ui, ("dist1", i), &mut r.c1, n);
                        ui.label("-");
                        cursor_button(ui, ("dist2", i), &mut r.c2, n);
                        ui.label("=");
                        if let (Some(a), Some(b)) = (r.c1, r.c2) {
                            let (sa, sb) = (cursors[a - 1], cursors[b - 1]);
                            let delta = sa as i64 - sb as i64;
                            // get_cm_delta: the unsigned distance; its sign follows
                            let mut t = if a == b {
                                "0".to_string()
                            } else {
                                format_real_time(sa.abs_diff(sb) as i64, rate)
                            };
                            t = format!("{t}/{delta}");
                            if delta < 0 {
                                t = t.replace('+', "-");
                            }
                            ui.label(t);
                        }
                    });
                }
                if let Some(i) = del {
                    rows.dist.remove(i);
                }
            });

        egui::CollapsingHeader::new("Edges")
            .default_open(true)
            .show(ui, |ui| {
                if ui.button("+").on_hover_text("Add").clicked() {
                    rows.edge.push(Row::default());
                }
                let channels: Vec<u16> = s
                    .state()
                    .signals
                    .iter()
                    .filter(|x| x.ty == channel_type::LOGIC)
                    .map(|x| x.index)
                    .collect();
                let mut del = None;
                for (i, r) in rows.edge.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("x").on_hover_text("Delete").clicked() {
                            del = Some(i);
                        }
                        cursor_button(ui, ("edge1", i), &mut r.c1, n);
                        ui.label("-");
                        cursor_button(ui, ("edge2", i), &mut r.c2, n);
                        let at = ui.label("@").on_hover_text("Channel").id;
                        if !channels.contains(&r.channel) {
                            r.channel = channels.first().copied().unwrap_or(0);
                        }
                        egui::ComboBox::from_id_salt(("edge-ch", i))
                            .width(40.0)
                            .selected_text(r.channel.to_string())
                            .show_ui(ui, |ui| {
                                for c in &channels {
                                    ui.selectable_value(&mut r.channel, *c, c.to_string());
                                }
                            })
                            .response
                            .labelled_by(at);
                        ui.label("=");
                        let text = match (r.c1, r.c2) {
                            (Some(a), Some(b)) => {
                                let store =
                                    s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
                                measure::edge_counts(
                                    &store,
                                    r.channel,
                                    cursors[b - 1],
                                    cursors[a - 1],
                                )
                                .map_or("-/-/-".to_string(), |(rise, fall)| {
                                    format!("{rise}/{fall}/{}", rise + fall)
                                })
                            }
                            _ => "-/-/-".to_string(),
                        };
                        ui.label(text);
                    });
                }
                if let Some(i) = del {
                    rows.edge.remove(i);
                }
                ui.label("Channel / Rising/Falling/Edges");
            });

        egui::CollapsingHeader::new("Cursors")
            .default_open(true)
            .show(ui, |ui| {
                let mut del = None;
                let mut goto = None;
                for (i, &c) in cursors.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("x").on_hover_text("Delete").clicked() {
                            del = Some(i + 1);
                        }
                        let b = egui::Button::new(
                            egui::RichText::new((i + 1).to_string()).color(Color32::WHITE),
                        )
                        .fill(cursor_colour(i + 1))
                        .min_size(egui::vec2(24.0, 18.0));
                        if ui.add(b).on_hover_text("Go to").clicked() {
                            goto = Some(c);
                        }
                        // trigger-relative, as update_cursor_info shows them
                        let rel = c.wrapping_sub(trig) as i64;
                        ui.label(format!("{}/{}", format_real_time(rel, rate), rel));
                    });
                }
                if let Some(nr) = del {
                    view.cursors.remove(nr);
                }
                if let Some(c) = goto {
                    view.center_on(s, c);
                }
            });
    }
}
