//! The protocol dock (`pv/dock/protocoldock.cpp`), the decoder options
//! dialog (`dialogs/decoderoptionsdlg.cpp`) and the protocol export
//! (`dialogs/protocolexp.cpp`).

use crate::i18n::tr;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    /// The decode region's start and end cursor numbers (0: the capture's
    /// start / end).
    region: (usize, usize),
    /// The region the edited stack had, as samples.
    old_region: Option<(u64, u64)>,
    /// The show buttons: decoder id or row title -> shown (absent: shown).
    shown: HashMap<String, bool>,
}

/// The titles of decoder `d`'s rows (`Row::title`): one per annotation row,
/// or the decoder's name when it has none.
fn row_titles(d: &DecoderInfo) -> Vec<String> {
    if d.annotation_rows.is_empty() {
        vec![d.name.clone()]
    } else {
        d.annotation_rows
            .iter()
            .map(|r| format!("{}: {}", d.name, r.desc))
            .collect()
    }
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
            region: (0, 0),
            old_region: None,
            shown: HashMap::new(),
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
            shown: t
                .stack
                .decoders()
                .iter()
                .enumerate()
                .map(|(k, d)| (d.info.id.clone(), t.stack.decoder_shown(k)))
                .chain(
                    t.stack
                        .rows()
                        .iter()
                        .enumerate()
                        .map(|(i, r)| (r.title(), t.stack.row_shown(i))),
                )
                .collect(),
            region: (0, 0),
            old_region: Some(t.stack.region()),
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
        if !self.shown.is_empty() {
            let show: Map<String, Value> = self
                .shown
                .iter()
                .map(|(k, v)| (k.clone(), (*v).into()))
                .collect();
            o.insert("show".into(), show.into());
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
    export: Option<Export>,
    pub message: Option<String>,
    /// The annotation clicked: the view jumps to it.
    pub jump: Option<(u64, u64)>,
    /// The view's cursors (sample positions), for the decode region.
    pub cursors: Vec<u64>,
    /// Number formats by protocol id (`AppConfig::SetProtocolFormat`); a
    /// new stack of that protocol starts in it.
    pub formats: std::collections::BTreeMap<String, String>,
    pub formats_changed: bool,
    /// The protocol list options window (`ProtocolList`) is open.
    list_opts: bool,
    /// Map Zoom "Fixed": a jump keeps the scale (`get_map_zoom() == 1`).
    pub map_zoom_fixed: bool,
    /// Where the export dialog starts (`protocolExportPath`), and the file
    /// an export went to, for the app to remember.
    pub export_dir: Option<String>,
    pub exported_to: Option<PathBuf>,
    /// Decoder texts in the language (`transDecoderDlg`); a change made in
    /// the decoder dialog, for the app to save.
    pub translate: bool,
    pub translate_changed: Option<bool>,
}

/// A decoder's channel or option text: by its `idn` in the language when
/// translating, else as the decoder gives it.
fn dec_text(translate: bool, idn: Option<&str>, text: &str) -> String {
    match idn {
        Some(idn) if translate => crate::i18n::tr_dec(idn, text),
        _ => text.to_owned(),
    }
}

const EXPORT_FORMATS: [&str; 2] = ["Comma-Separated Values (*.csv)", "Text files (*.txt)"];

/// The protocol export's steps.
enum Export {
    /// The dialog: TXT instead of CSV; the listed rows (index, title, chosen).
    Options {
        txt: bool,
        rows: Vec<(usize, String, bool)>,
    },
    /// The file dialog is open.
    File {
        rx: Receiver<Option<PathBuf>>,
        rows: Vec<usize>,
    },
    Running {
        progress: Arc<AtomicU32>,
        cancel: Arc<AtomicBool>,
        done: Receiver<Result<(), String>>,
    },
}

/// `save_proc` in a thread.
fn start_export(s: &Session, t: Arc<DecodeTrace>, rows: Vec<usize>, path: PathBuf) -> Export {
    let sample_count = s
        .data()
        .logic
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .ring_sample_count();
    let progress = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, done) = channel();
    let (p, c) = (progress.clone(), cancel.clone());
    std::thread::spawn(move || {
        let r = dsview_session::store::export_protocol_with(
            &t,
            &rows,
            sample_count,
            &path,
            &mut |pct| {
                p.store(pct, Ordering::Relaxed);
                !c.load(Ordering::Relaxed)
            },
        );
        let _ = tx.send(r);
    });
    Export::Running {
        progress,
        cancel,
        done,
    }
}

impl Default for ProtocolDock {
    fn default() -> Self {
        ProtocolDock {
            open: true,
            search: String::new(),
            add: None,
            table: 0,
            filter_col: 0,
            cursors: Vec::new(),
            formats: Default::default(),
            formats_changed: false,
            list_opts: false,
            map_zoom_fixed: false,
            export_dir: None,
            exported_to: None,
            translate: true,
            translate_changed: None,
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
        ui.heading(tr("Decoders"));
        if !logic_mode {
            ui.label(tr("Protocol decoders need logic analyzer mode."));
            return;
        }
        // the searchable picker
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(tr("Search decoders"))
                    .desired_width(150.0),
            )
            .widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::TextInput, true, tr("Search decoders"))
            });
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
                // the number format, kept per protocol id
                let mut f = t.stack.format();
                egui::ComboBox::from_id_salt(("fmt", i))
                    .width(54.0)
                    .selected_text(f.name())
                    .show_ui(ui, |ui| {
                        for x in dsview_session::numfmt::Format::ALL {
                            ui.selectable_value(&mut f, x, x.name());
                        }
                    })
                    .response
                    .on_hover_text(tr("Number format"));
                if f != t.stack.format() {
                    t.stack.set_format(f);
                    if let Some(id) = t.stack.decoders().last().map(|d| d.info.id.clone()) {
                        self.formats.insert(id, f.name().to_owned());
                        self.formats_changed = true;
                    }
                }
                if ui
                    .small_button(tr("Decode"))
                    .on_hover_text(tr("Decode again"))
                    .clicked()
                {
                    s.start_decode(i);
                }
                if ui.small_button(tr("Settings")).clicked() {
                    self.add = Some(AddDialog::edit(i, t));
                }
                if ui
                    .small_button(tr("x"))
                    .on_hover_text(tr("Remove"))
                    .clicked()
                {
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
            // the annotation jumped to is marked on its trace
            // (`set_mark_index`), the others' marks cleared
            if let Some((a, b)) = self.jump {
                for x in &traces {
                    x.stack.set_mark(-1);
                }
                t.stack.set_mark(((a + b) / 2) as i64);
            }
        }
        self.add_dialog(ui.ctx(), s);
        self.list_options(ui.ctx(), &traces);
        self.export_job(ui.ctx(), s, &traces);
    }

    /// `ProtocolList`: the map zoom, the protocol the table shows and which
    /// of its rows are listed.
    fn list_options(&mut self, ctx: &egui::Context, traces: &[Arc<DecodeTrace>]) {
        if !self.list_opts {
            return;
        }
        let mut open = !crate::msgbox::esc(ctx);
        egui::Window::new(tr("Protocol List Viewer"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("list_opts").num_columns(2).show(ui, |ui| {
                    let l = ui.label(tr("Map Zoom: "));
                    egui::ComboBox::from_id_salt("map_zoom")
                        .selected_text(tr(if self.map_zoom_fixed {
                            "Fixed"
                        } else {
                            "Fit to Window"
                        }))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.map_zoom_fixed,
                                false,
                                tr("Fit to Window"),
                            );
                            ui.selectable_value(&mut self.map_zoom_fixed, true, tr("Fixed"));
                        })
                        .response
                        .labelled_by(l.id);
                    ui.end_row();
                    let l = ui.label(tr("Decoded Protocols: "));
                    egui::ComboBox::from_id_salt("list_protocol")
                        .selected_text(traces.get(self.table).map(|t| t.name()).unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, t) in traces.iter().enumerate() {
                                ui.selectable_value(&mut self.table, i, t.name());
                            }
                        })
                        .response
                        .labelled_by(l.id);
                    ui.end_row();
                    if let Some(t) = traces.get(self.table) {
                        for (i, r) in t.stack.rows().iter().enumerate() {
                            let l = ui.label(r.title());
                            let mut on = t.stack.row_list_shown(i);
                            if ui.checkbox(&mut on, "").labelled_by(l.id).changed() {
                                t.stack.set_row_list_shown(i, on);
                            }
                            ui.end_row();
                        }
                    }
                });
                if ui.button(tr("OK")).clicked() {
                    self.list_opts = false;
                }
            });
        if !open {
            self.list_opts = false;
        }
    }

    fn add_dialog(&mut self, ctx: &egui::Context, s: &Session) {
        let mut translate = self.translate;
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
            // `IDS_DLG_DECODER_IF_TRANS`: only where decoder texts exist
            if crate::i18n::language() == dsview_viewmodel::qsettings::LAN_CN {
                let mut t = translate;
                if ui.checkbox(&mut t, tr("Translate param names")).changed() {
                    translate = t;
                    self.translate_changed = Some(t);
                }
            }
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
                        // "name (description) *", the description in the
                        // language (`decoderoptionsdlg.cpp`)
                        let text = match info {
                            Some(c) => format!(
                                "{} ({})",
                                c.name,
                                dec_text(translate, c.idn.as_deref(), &c.desc)
                            ),
                            None => id.clone(),
                        };
                        let l = ui.label(format!("{text}{}", if required { " *" } else { "" }));
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
                            })
                            .response
                            .labelled_by(l.id);
                        ui.end_row();
                    }
                });
            // the edited trace takes the show buttons at once
            let live = dlg
                .replace
                .as_ref()
                .and_then(|r| s.state().decoders.get(r.0).cloned());
            for (k, d) in dlg.stack.iter().enumerate() {
                ui.separator();
                // `DecoderGroupBox`: the decoder's and its rows' show buttons
                let mut on = dlg.shown.get(&d.id).copied().unwrap_or(true);
                if ui
                    .checkbox(&mut on, egui::RichText::new(&d.name).strong())
                    .changed()
                {
                    dlg.shown.insert(d.id.clone(), on);
                    if let Some(t) = &live {
                        t.stack.set_decoder_shown(k, on);
                    }
                }
                ui.indent(("dec-rows", k), |ui| {
                    for title in row_titles(d) {
                        let mut on = dlg.shown.get(&title).copied().unwrap_or(true);
                        if ui.checkbox(&mut on, &title).changed() {
                            if let Some(t) = &live
                                && let Some(i) =
                                    t.stack.rows().iter().position(|r| r.title() == title)
                            {
                                t.stack.show_row(i, on);
                            }
                            dlg.shown.insert(title, on);
                        }
                    }
                });
                if d.options.is_empty() {
                    continue;
                }
                egui::Grid::new(("dec-opts", k))
                    .num_columns(2)
                    .show(ui, |ui| {
                        for (id, text) in dlg.options[k].iter_mut() {
                            let Some(o) = d.options.iter().find(|o| o.id == *id) else {
                                continue;
                            };
                            let desc = o.desc.clone().unwrap_or_else(|| id.clone());
                            let l = ui.label(dec_text(translate, o.idn.as_deref(), &desc));
                            if o.values.is_empty() {
                                ui.text_edit_singleline(text).labelled_by(l.id);
                            } else {
                                egui::ComboBox::from_id_salt(("opt", k, id.clone()))
                                    .selected_text(text.clone())
                                    .show_ui(ui, |ui| {
                                        for v in &o.values {
                                            let t = opt_text(v);
                                            ui.selectable_value(text, t.clone(), t);
                                        }
                                    })
                                    .response
                                    .labelled_by(l.id);
                            }
                            ui.end_row();
                        }
                    });
            }
            // the decode region (`update_decode_range`): cursors or the ends
            ui.separator();
            if let Some((a, b)) = dlg.old_region.take() {
                let find = |v: u64| {
                    self.cursors
                        .iter()
                        .position(|&c| c == v)
                        .map_or(0, |p| p + 1)
                };
                dlg.region = (find(a), if b == u64::MAX { 0 } else { find(b) });
            }
            let names: Vec<String> = (1..=self.cursors.len())
                .map(|i| format!("Cursor{i}"))
                .collect();
            egui::Grid::new("dec-region").num_columns(2).show(ui, |ui| {
                for (label, first, v) in [
                    ("Start", "Start", &mut dlg.region.0),
                    ("End", "End", &mut dlg.region.1),
                ] {
                    let l = ui.label(label);
                    *v = (*v).min(names.len());
                    let text = if *v == 0 {
                        first.to_string()
                    } else {
                        names[*v - 1].clone()
                    };
                    egui::ComboBox::from_id_salt(("dec-region", label))
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(v, 0, first);
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(v, i + 1, n);
                            }
                        })
                        .response
                        .labelled_by(l.id);
                    ui.end_row();
                }
            });
            if let Some(e) = &dlg.error {
                ui.colored_label(egui::Color32::from_rgb(0xEF, 0x29, 0x29), e);
            }
            ui.horizontal(|ui| {
                if ui.button(tr("OK")).clicked() {
                    ok = true;
                }
                if ui.button(tr("Cancel")).clicked() || crate::msgbox::esc(ui.ctx()) {
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
                        // the region in samples, inside the capture
                        let last = s.state().cur_samplelimits.saturating_sub(1);
                        let at =
                            |n: usize| (n > 0).then(|| self.cursors.get(n - 1).copied()).flatten();
                        let mut start = at(dlg.region.0).unwrap_or(0);
                        let mut end = at(dlg.region.1).unwrap_or(last);
                        if start > last {
                            start = 0;
                        }
                        end = end.min(last);
                        if start > end {
                            std::mem::swap(&mut start, &mut end);
                        }
                        if let Some(t) = s.state().decoders.get(index) {
                            t.stack
                                .set_region(start, if dlg.region.1 == 0 { u64::MAX } else { end });
                            let id = t.stack.decoders().last().map(|d| d.info.id.clone());
                            if let Some(f) = id.and_then(|id| self.formats.get(&id)) {
                                t.stack.set_format(dsview_session::numfmt::Format::parse(f));
                            }
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
        let fmt = t.stack.format();
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
            .filter(|(i, _)| t.stack.row_list_shown(*i))
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
                    .hint_text(tr("Search"))
                    .desired_width(120.0),
            )
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, tr("Search")));
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
                        .filter(|(_, a)| a.text_in(fmt).contains(&self.filter))
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
            if ui
                .button(tr("List…"))
                .on_hover_text(tr("Protocol list options"))
                .clicked()
            {
                self.list_opts = true;
            }
            if ui.button(tr("Export…")).clicked() && self.export.is_none() {
                // the rows listed; the first one chosen
                let rows = t
                    .stack
                    .rows()
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| t.stack.row_list_shown(*i))
                    .enumerate()
                    .map(|(k, (i, r))| (i, r.title(), k == 0))
                    .collect();
                self.export = Some(Export::Options { txt: false, rows });
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
                                        egui::Label::new(a.text_in(fmt))
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text(a.text_in(fmt))
                                    .clicked()
                            {
                                self.jump = Some((a.start, a.end));
                            }
                        });
                    }
                });
            });
    }

    /// `ProtocolExp`: the format and the rows to export, then the file,
    /// then the export with its progress and Cancel.
    fn export_job(&mut self, ctx: &egui::Context, s: &Session, traces: &[Arc<DecodeTrace>]) {
        let Some(job) = self.export.take() else {
            return;
        };
        let Some(t) = traces.get(self.table).cloned() else {
            return;
        };
        self.export = match job {
            Export::Options { mut txt, mut rows } => {
                let mut go = None;
                let mut open = !crate::msgbox::esc(ctx);
                egui::Window::new(tr("Protocol Export"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        egui::Grid::new("pexp").num_columns(2).show(ui, |ui| {
                            let l = ui.label(tr("Export Format: "));
                            egui::ComboBox::from_id_salt("pexp_fmt")
                                .selected_text(EXPORT_FORMATS[usize::from(txt)])
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut txt, false, EXPORT_FORMATS[0]);
                                    ui.selectable_value(&mut txt, true, EXPORT_FORMATS[1]);
                                })
                                .response
                                .labelled_by(l.id);
                            ui.end_row();
                            for (_, title, on) in rows.iter_mut() {
                                let l = ui.label(title.as_str());
                                ui.checkbox(on, "").labelled_by(l.id);
                                ui.end_row();
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button(tr("OK")).clicked() {
                                go = Some(true);
                            }
                            if ui.button(tr("Cancel")).clicked() || crate::msgbox::esc(ui.ctx()) {
                                go = Some(false);
                            }
                        });
                    });
                match go {
                    _ if !open => None,
                    Some(false) => None,
                    Some(true) if t.stack.result_count() == 0 || rows.is_empty() => {
                        self.message = Some("No data to export".into());
                        Some(Export::Options { txt, rows })
                    }
                    Some(true) => {
                        let chosen: Vec<usize> = rows
                            .iter()
                            .filter(|r| r.2)
                            .map(|r| r.0)
                            .take(20) // EXPORT_DEC_ROW_COUNT_MAX
                            .collect();
                        let (tx, rx) = channel();
                        let start = self
                            .export_dir
                            .as_ref()
                            .map(PathBuf::from)
                            .filter(|d| d.is_dir());
                        std::thread::spawn(move || {
                            // the chosen format first
                            let (a, b) = if txt { ("txt", "csv") } else { ("csv", "txt") };
                            let mut d = rfd::FileDialog::new();
                            if let Some(dir) = start {
                                d = d.set_directory(dir);
                            }
                            let p = d
                                .set_title("Export Data")
                                .add_filter(a.to_uppercase(), &[a])
                                .add_filter(b.to_uppercase(), &[b])
                                .save_file()
                                .map(|p| {
                                    // the format's suffix unless the name has it
                                    if p.extension().is_some_and(|e| e == a) {
                                        p
                                    } else {
                                        let mut n = p.into_os_string();
                                        n.push(format!(".{a}"));
                                        PathBuf::from(n)
                                    }
                                });
                            let _ = tx.send(p);
                        });
                        Some(Export::File { rx, rows: chosen })
                    }
                    None => Some(Export::Options { txt, rows }),
                }
            }
            Export::File { rx, rows } => match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => Some(Export::File { rx, rows }),
                Ok(Some(path)) => {
                    if rows.is_empty() {
                        dsview_drivers::sys::log(
                            3,
                            "ERROR: There have no decode data row to export.",
                        );
                        None
                    } else {
                        self.exported_to = Some(path.clone());
                        Some(start_export(s, t, rows, path))
                    }
                }
                _ => None,
            },
            Export::Running {
                progress,
                cancel,
                done,
            } => match done.try_recv() {
                Ok(r) => {
                    if let Err(e) = r {
                        self.message = Some(e);
                    }
                    None
                }
                Err(_) => {
                    egui::Window::new(tr("Export Protocol List Result... It can take a while."))
                        .collapsible(false)
                        .resizable(false)
                        .show(ctx, |ui| {
                            let p = progress.load(Ordering::Relaxed);
                            ui.add(
                                egui::ProgressBar::new(p as f32 / 100.0)
                                    .desired_width(260.0)
                                    .text(format!("{p}%")),
                            );
                            if ui.button(tr("Cancel")).clicked() || crate::msgbox::esc(ui.ctx()) {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        });
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    Some(Export::Running {
                        progress,
                        cancel,
                        done,
                    })
                }
            },
        };
        if matches!(self.export, Some(Export::File { .. })) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
