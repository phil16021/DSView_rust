//! The protocol dock (`pv/dock/protocoldock.cpp`), the decoder options
//! dialog (`dialogs/decoderoptionsdlg.cpp`) and the protocol export
//! (`dialogs/protocolexp.cpp`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use dsview_core::{Mode, channel_type};
use dsview_decode::{DecoderInfo, OptValue};
use dsview_session::Session;
use dsview_session::decode::Ann;
use dsview_session::session::DecodeTrace;
use eframe::egui::{self, Ui};
use egui_extras::{Column, TableBuilder};
use serde_json::{Map, Value, json};

/// `protocol_sort_callback`: names compared case-insensitively.
fn sorted_decoders() -> Vec<Arc<DecoderInfo>> {
    let mut v = dsview_decode::decoders();
    v.sort_by_key(|d| d.name.to_uppercase());
    v
}

/// The decoders a stacked decoder needs below it, down to one that takes
/// logic input (`get_output_protocol_by_id`): bottom first, or None when
/// the chain does not reach logic.
fn stack_for(d: &Arc<DecoderInfo>, list: &[Arc<DecoderInfo>]) -> Option<Vec<Arc<DecoderInfo>>> {
    let mut chain = vec![d.clone()];
    let mut input = d.inputs.first()?.clone();
    while input != "logic" {
        let base = list.iter().find(|b| {
            b.outputs
                .first()
                .is_some_and(|o| *o == input && (!b.id.starts_with("0:") || *o == b.id))
        })?;
        if chain.iter().any(|c| c.id == base.id) {
            return None;
        }
        chain.insert(0, base.clone());
        input = base.inputs.first()?.clone();
    }
    Some(chain)
}

fn opt_text(v: &OptValue) -> String {
    match v {
        OptValue::Str(s) => s.clone(),
        OptValue::Int(i) => i.to_string(),
        OptValue::Float(f) => f.to_string(),
    }
}

/// The options dialog of a new decoder stack.
struct AddDialog {
    stack: Vec<Arc<DecoderInfo>>,
    /// Bottom decoder's channel id -> logic channel (None: unbound).
    channels: Vec<(String, Option<u16>)>,
    /// Per decoder: option id -> text.
    options: Vec<Vec<(String, String)>>,
    error: Option<String>,
    /// Editing decoder trace `n` (its name and place are kept).
    replace: Option<(usize, String, i32)>,
}

impl AddDialog {
    fn new(stack: Vec<Arc<DecoderInfo>>, logic: &[(u16, String)]) -> AddDialog {
        let root = &stack[0];
        // bind channels in order to the channels whose names match, else in turn
        let mut next = logic.iter();
        let channels = root
            .channels
            .iter()
            .chain(&root.opt_channels)
            .enumerate()
            .map(|(i, c)| {
                let by_name = logic.iter().find(|(_, n)| {
                    n.eq_ignore_ascii_case(&c.name) || n.eq_ignore_ascii_case(&c.id)
                });
                let ch = by_name.map(|x| x.0).or_else(|| {
                    if i < root.channels.len() {
                        next.next().map(|x| x.0)
                    } else {
                        None
                    }
                });
                (c.id.clone(), ch)
            })
            .collect();
        let options = stack
            .iter()
            .map(|d| {
                d.options
                    .iter()
                    .map(|o| {
                        (
                            o.id.clone(),
                            o.def.as_ref().map(opt_text).unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .collect();
        AddDialog {
            stack,
            channels,
            options,
            error: None,
            replace: None,
        }
    }

    /// The dialog for an existing stack (the decoder's "Settings").
    fn edit(i: usize, t: &DecodeTrace) -> AddDialog {
        let stack: Vec<Arc<DecoderInfo>> =
            t.stack.decoders().iter().map(|d| d.info.clone()).collect();
        let root = &t.stack.decoders()[0];
        let channels = root
            .info
            .channels
            .iter()
            .chain(&root.info.opt_channels)
            .map(|c| {
                let bound = root
                    .probes
                    .iter()
                    .find(|(id, _)| *id == c.id)
                    .map(|(_, ch)| *ch as u16);
                (c.id.clone(), bound)
            })
            .collect();
        let options = t
            .stack
            .decoders()
            .iter()
            .map(|d| {
                d.info
                    .options
                    .iter()
                    .map(|o| {
                        let v = d.options.get(&o.id).or(o.def.as_ref());
                        (o.id.clone(), v.map(opt_text).unwrap_or_default())
                    })
                    .collect()
            })
            .collect();
        AddDialog {
            stack,
            channels,
            options,
            error: None,
            replace: Some((i, t.name(), *t.view_index.lock().unwrap())),
        }
    }

    /// The saved-stack JSON `StoreSession::load_decoders` reads.
    fn to_json(&self) -> Result<Value, String> {
        let root = &self.stack[0];
        let mut chans = Vec::new();
        for (id, ch) in &self.channels {
            match ch {
                Some(i) => chans.push(json!({ id: i })),
                None if root.channels.iter().any(|c| c.id == *id) => {
                    return Err(format!("The required channel '{id}' is not bound."));
                }
                None => {}
            }
        }
        let opts = |k: usize| -> Map<String, Value> {
            let d = &self.stack[k];
            self.options[k]
                .iter()
                .map(|(id, text)| {
                    let def = d
                        .options
                        .iter()
                        .find(|o| o.id == *id)
                        .and_then(|o| o.def.clone());
                    let v = match def {
                        Some(OptValue::Int(_)) => text
                            .trim()
                            .parse::<i64>()
                            .map_or(Value::from(text.clone()), Value::from),
                        Some(OptValue::Float(_)) => text
                            .trim()
                            .parse::<f64>()
                            .map_or(Value::from(text.clone()), Value::from),
                        _ => Value::from(text.clone()),
                    };
                    (id.clone(), v)
                })
                .collect()
        };
        let stacked: Vec<Value> = (1..self.stack.len())
            .map(|k| json!({"id": self.stack[k].id, "options": opts(k)}))
            .collect();
        let mut o = Map::new();
        o.insert("id".into(), root.id.clone().into());
        o.insert("channel".into(), chans.into());
        o.insert("options".into(), opts(0).into());
        if !stacked.is_empty() {
            o.insert("stacked decoders".into(), stacked.into());
        }
        Ok(Value::Object(o))
    }
}

pub struct ProtocolDock {
    pub open: bool,
    search: String,
    add: Option<AddDialog>,
    /// The decoder trace the table shows.
    table: usize,
    /// The column searched.
    filter_col: usize,
    filter: String,
    matches: Vec<usize>,
    cur_match: usize,
    filtered_for: (usize, usize, String, u64),
    export: Option<Receiver<Option<PathBuf>>>,
    pub message: Option<String>,
    /// The annotation clicked: the view jumps to it.
    pub jump: Option<(u64, u64)>,
}

impl Default for ProtocolDock {
    fn default() -> Self {
        ProtocolDock {
            open: true,
            search: String::new(),
            add: None,
            table: 0,
            filter_col: 0,
            filter: String::new(),
            matches: Vec::new(),
            cur_match: 0,
            filtered_for: (usize::MAX, 0, String::new(), 0),
            export: None,
            message: None,
            jump: None,
        }
    }
}

impl ProtocolDock {
    pub fn show(&mut self, ui: &mut Ui, s: &Session) {
        let logic_mode = s.mode() == Mode::Logic as i32;
        ui.heading("Decoders");
        if !logic_mode {
            ui.label("Protocol decoders need logic analyzer mode.");
            return;
        }
        // the searchable picker
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search decoders")
                    .desired_width(150.0),
            );
        });
        if !self.search.is_empty() {
            let needle = self.search.to_lowercase();
            let all = sorted_decoders();
            egui::ScrollArea::vertical()
                .id_salt("picker")
                .max_height(160.0)
                .show(ui, |ui| {
                    for d in all.iter().filter(|d| {
                        d.name.to_lowercase().contains(&needle)
                            || d.id.to_lowercase().contains(&needle)
                    }) {
                        if ui
                            .selectable_label(false, format!("{} ({})", d.name, d.id))
                            .clicked()
                        {
                            match stack_for(d, &all) {
                                Some(stack) => {
                                    let logic: Vec<(u16, String)> = s
                                        .state()
                                        .signals
                                        .iter()
                                        .filter(|x| x.ty == channel_type::LOGIC)
                                        .map(|x| (x.index, x.name.clone()))
                                        .collect();
                                    self.add = Some(AddDialog::new(stack, &logic));
                                }
                                None => self.message = Some("find the base decoder error!".into()),
                            }
                            self.search.clear();
                        }
                    }
                });
        }
        ui.separator();

        // the decoder list
        let traces = s.state().decoders.clone();
        let decoding = s.is_decoding();
        let mut remove = None;
        for (i, t) in traces.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.selectable_label(self.table == i, t.name()).clicked() {
                    self.table = i;
                }
                let p = t.stack.progress();
                ui.add(
                    egui::ProgressBar::new(p as f32 / 100.0)
                        .desired_width(60.0)
                        .text(format!("{p}%")),
                );
                if ui
                    .small_button("Decode")
                    .on_hover_text("Decode again")
                    .clicked()
                {
                    s.start_decode(i);
                }
                if ui.small_button("Settings").clicked() {
                    self.add = Some(AddDialog::edit(i, t));
                }
                if ui.small_button("x").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
            });
            let err = t.stack.error_message();
            if !err.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(0xEF, 0x29, 0x29), err);
            }
        }
        if let Some(i) = remove {
            s.remove_decoder(i);
            if self.table >= i && self.table > 0 {
                self.table -= 1;
            }
        }
        if decoding {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        ui.separator();
        if let Some(t) = traces.get(self.table) {
            self.table_ui(ui, s, t);
        }
        self.add_dialog(ui.ctx(), s);
        self.export_job(s, &traces);
    }

    fn add_dialog(&mut self, ctx: &egui::Context, s: &Session) {
        let Some(dlg) = &mut self.add else { return };
        let mut keep = true;
        let mut ok = false;
        let logic: Vec<(u16, String)> = s
            .state()
            .signals
            .iter()
            .filter(|x| x.ty == channel_type::LOGIC)
            .map(|x| (x.index, x.name.clone()))
            .collect();
        egui::Window::new(format!(
            "{} options",
            dlg.stack.last().map_or("", |d| d.name.as_str())
        ))
        .collapsible(false)
        .show(ctx, |ui| {
            let root = dlg.stack[0].clone();
            egui::Grid::new("dec-channels")
                .num_columns(2)
                .show(ui, |ui| {
                    for (id, ch) in dlg.channels.iter_mut() {
                        let info = root
                            .channels
                            .iter()
                            .chain(&root.opt_channels)
                            .find(|c| c.id == *id);
                        let required = root.channels.iter().any(|c| c.id == *id);
                        ui.label(format!(
                            "{}{}",
                            info.map_or(id.as_str(), |c| c.name.as_str()),
                            if required { " *" } else { "" }
                        ));
                        let current = ch
                            .and_then(|c| logic.iter().find(|x| x.0 == c))
                            .map_or("-".to_owned(), |x| format!("{}: {}", x.0, x.1));
                        egui::ComboBox::from_id_salt(("ch", id.clone()))
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                if !required {
                                    ui.selectable_value(ch, None, "-");
                                }
                                for (i, n) in &logic {
                                    ui.selectable_value(ch, Some(*i), format!("{i}: {n}"));
                                }
                            });
                        ui.end_row();
                    }
                });
            for (k, d) in dlg.stack.iter().enumerate() {
                if d.options.is_empty() {
                    continue;
                }
                ui.separator();
                ui.strong(&d.name);
                egui::Grid::new(("dec-opts", k))
                    .num_columns(2)
                    .show(ui, |ui| {
                        for (id, text) in dlg.options[k].iter_mut() {
                            let Some(o) = d.options.iter().find(|o| o.id == *id) else {
                                continue;
                            };
                            ui.label(o.desc.clone().unwrap_or_else(|| id.clone()));
                            if o.values.is_empty() {
                                ui.text_edit_singleline(text);
                            } else {
                                egui::ComboBox::from_id_salt(("opt", k, id.clone()))
                                    .selected_text(text.clone())
                                    .show_ui(ui, |ui| {
                                        for v in &o.values {
                                            let t = opt_text(v);
                                            ui.selectable_value(text, t.clone(), t);
                                        }
                                    });
                            }
                            ui.end_row();
                        }
                    });
            }
            if let Some(e) = &dlg.error {
                ui.colored_label(egui::Color32::from_rgb(0xEF, 0x29, 0x29), e);
            }
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    ok = true;
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if ok {
            match dlg.to_json() {
                Ok(mut entry) => {
                    if let Some((_, name, vi)) = &dlg.replace {
                        entry["label"] = name.clone().into();
                        entry["view_index"] = (*vi).into();
                    }
                    if dsview_session::store::load_decoders(s, &[entry]) == 0 {
                        dlg.error = Some("DSView could not add the decoder".into());
                    } else {
                        let mut index = s.state().decoders.len() - 1;
                        if let Some((old, _, _)) = dlg.replace {
                            s.remove_decoder(old);
                            s.move_last_decoder_to(old);
                            index = old;
                        }
                        s.start_decode(index);
                        self.table = index;
                        keep = false;
                    }
                }
                Err(e) => dlg.error = Some(e),
            }
        }
        if !keep {
            self.add = None;
        }
    }

    /// The annotation table: one column per row shown in the list; table
    /// row n holds each column's n-th annotation (as Qt's model does).
    fn table_ui(&mut self, ui: &mut Ui, s: &Session, t: &Arc<DecodeTrace>) {
        let sample_count = s
            .data()
            .logic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ring_sample_count();
        let rows: Vec<(usize, String)> = t
            .stack
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.list_shown)
            .map(|(i, r)| (i, r.title()))
            .collect();
        if rows.is_empty() {
            return;
        }
        self.filter_col = self.filter_col.min(rows.len() - 1);
        let cols: Vec<Vec<Ann>> = rows
            .iter()
            .map(|(i, _)| t.stack.row_sorted(*i, sample_count))
            .collect();
        let n = cols.iter().map(Vec::len).max().unwrap_or(0);

        // search in the chosen column (case-sensitive, as Qt's filter)
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Search")
                    .desired_width(120.0),
            );
            let key = (
                self.table,
                self.filter_col,
                self.filter.clone(),
                t.stack.result_count(),
            );
            if key != self.filtered_for {
                self.matches = if self.filter.is_empty() {
                    Vec::new()
                } else {
                    cols[self.filter_col]
                        .iter()
                        .enumerate()
                        .filter(|(_, a)| a.text().contains(&self.filter))
                        .map(|(i, _)| i)
                        .collect()
                };
                self.cur_match = 0;
                self.filtered_for = key;
            }
            if !self.filter.is_empty() {
                ui.label(format!("{} matches", self.matches.len()));
                let jump = |m: &mut Self, k: usize| {
                    if let Some(&i) = m.matches.get(k)
                        && let Some(a) = cols[m.filter_col].get(i)
                    {
                        m.jump = Some((a.start, a.end));
                    }
                };
                if ui.small_button("<").clicked() && !self.matches.is_empty() {
                    self.cur_match = (self.cur_match + self.matches.len() - 1) % self.matches.len();
                    jump(self, self.cur_match);
                }
                if ui.small_button(">").clicked() && !self.matches.is_empty() {
                    self.cur_match = (self.cur_match + 1) % self.matches.len();
                    jump(self, self.cur_match);
                }
            }
            if ui.button("Export…").clicked() && self.export.is_none() {
                let (tx, rx) = channel();
                std::thread::spawn(move || {
                    let p = rfd::FileDialog::new()
                        .set_title("Export Protocol List")
                        .add_filter("CSV", &["csv"])
                        .add_filter("TXT", &["txt"])
                        .save_file();
                    let _ = tx.send(p);
                });
                self.export = Some(rx);
            }
        });
        let shown_rows: Vec<usize> = if self.filter.is_empty() {
            (0..n).collect()
        } else {
            self.matches.clone()
        };
        let width = 110.0;
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::exact(60.0))
            .columns(Column::initial(width).resizable(true), rows.len())
            .header(18.0, |mut h| {
                h.col(|ui| {
                    ui.strong("#");
                });
                for (k, (_, title)) in rows.iter().enumerate() {
                    h.col(|ui| {
                        let short = title.split(": ").last().unwrap_or(title);
                        if ui
                            .selectable_label(k == self.filter_col, short)
                            .on_hover_text(title)
                            .clicked()
                        {
                            self.filter_col = k;
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, shown_rows.len(), |mut row| {
                    let i = shown_rows[row.index()];
                    row.col(|ui| {
                        ui.label((i + 1).to_string());
                    });
                    for col in &cols {
                        row.col(|ui| {
                            if let Some(a) = col.get(i)
                                && ui
                                    .add(
                                        egui::Label::new(a.text())
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text(a.text())
                                    .clicked()
                            {
                                self.jump = Some((a.start, a.end));
                            }
                        });
                    }
                });
            });
    }

    fn export_job(&mut self, s: &Session, traces: &[Arc<DecodeTrace>]) {
        let Some(rx) = &self.export else { return };
        let Ok(path) = rx.try_recv() else { return };
        self.export = None;
        let (Some(path), Some(t)) = (path, traces.get(self.table)) else {
            return;
        };
        let sample_count = s
            .data()
            .logic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ring_sample_count();
        let rows: Vec<usize> = t
            .stack
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.list_shown)
            .map(|(i, _)| i)
            .take(1)
            .collect();
        if let Err(e) = dsview_session::store::export_protocol(t, &rows, sample_count, &path) {
            self.message = Some(e);
        }
    }
}
