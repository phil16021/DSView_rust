//! The logic trigger dock (`pv/dock/triggerdock.cpp`): simple or advanced
//! trigger, the position, and for the advanced trigger the stage tabs and
//! the serial trigger. The settings live in the session
//! (`State::adv_trigger`), which commits them when a capture starts.

use dsview_core::{Mode, key};
use dsview_session::Session;
use dsview_session::trigger::{AdvTrigger, normalise, serial_from_hex, serial_hex};
use eframe::egui::{self, Ui};

const NOTE: &str = "X: Don't care\n0: Low level\n1: High level\nR: Rising edge\nF: Falling edge\nC: Rising/Falling edge";

#[derive(Default)]
pub struct TriggerDock {
    pub open: bool,
    /// The stage tab shown.
    stage_tab: usize,
    /// The serial value's hex field, while "Input hex" is on.
    hex: Option<String>,
    pub message: Option<String>,
}

/// A value line edit: monospace, normalised when it loses focus.
fn value_edit(ui: &mut Ui, label: &str, v: &mut String, allowed: &str) -> bool {
    let l = ui.label(label);
    let r = ui.add(
        egui::TextEdit::singleline(v)
            .font(egui::TextStyle::Monospace)
            .desired_width(ui.available_width().min(290.0))
            .char_limit(31),
    );
    let r = r.labelled_by(l.id);
    if r.lost_focus() || (!r.has_focus() && v.len() != 31) {
        *v = normalise(v, allowed);
        return true;
    }
    r.changed()
}

impl TriggerDock {
    pub fn show(&mut self, ui: &mut Ui, s: &Session) {
        ui.heading("Trigger");
        if s.mode() != Mode::Logic as i32 {
            ui.label("The logic trigger applies in logic mode.");
            return;
        }
        let dev = s.state().device.clone();
        let hardware = dev.as_ref().is_some_and(|d| d.is_hardware());
        let stream = s.config(key::STREAM, None).is_some_and(|v| v.get_bool());
        let looping = s.state().collect_mode == dsview_session::session::CollectMode::Loop;
        let ch_num = s
            .config(key::TOTAL_CH_NUM, None)
            .map_or(16, |v| v.get_i16() as usize);
        let mut a: AdvTrigger = s.state().adv_trigger.clone();
        let before = a.clone();
        ui.add_enabled_ui(!looping, |ui| {
            // simple / advanced (adv_trigger(): DSLogic hardware, not streaming)
            if ui.radio(!a.advanced, "Simple Trigger").clicked() {
                a.advanced = false;
            }
            if ui
                .add_enabled(
                    !stream,
                    egui::RadioButton::new(a.advanced, "Advanced Trigger"),
                )
                .clicked()
            {
                if !hardware && !dev.as_ref().is_some_and(|d| d.is_file()) {
                    self.message = Some("Advanced Trigger need DSLogic Hardware Support!".into());
                } else if stream {
                    self.message = Some("Stream Mode Don't Support Advanced Trigger!".into());
                } else {
                    a.advanced = true;
                }
            }
            // position
            let (lo, hi) = s.trigger_range();
            let mut pos = s.trigger_position();
            let cur = pos;
            ui.add_enabled_ui(!stream, |ui| {
                let l = ui
                    .horizontal(|ui| {
                        let l = ui.label("Trigger Position: ");
                        ui.add(egui::DragValue::new(&mut pos).range(lo..=hi).suffix("%"))
                            .labelled_by(l.id);
                        l.id
                    })
                    .inner;
                ui.add(egui::Slider::new(&mut pos, lo..=hi).show_value(false))
                    .labelled_by(l);
            });
            if pos != cur {
                s.set_trigger_position(pos);
            }
            // stages
            ui.add_enabled_ui(a.advanced, |ui| {
                ui.horizontal(|ui| {
                    let l = ui.label("Total Trigger Stages: ");
                    egui::ComboBox::from_id_salt("trig-stages")
                        .selected_text((a.stages + 1).to_string())
                        .show_ui(ui, |ui| {
                            for i in 0..16 {
                                ui.selectable_value(&mut a.stages, i, (i + 1).to_string());
                            }
                        })
                        .response
                        .labelled_by(l.id);
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut a.tab, 0, "Stage Trigger");
                    ui.selectable_value(&mut a.tab, 1, "Serial Trigger");
                });
                if a.tab == 0 {
                    self.stage_tab = self.stage_tab.min(a.stages);
                    ui.horizontal_wrapped(|ui| {
                        for i in 0..=a.stages {
                            ui.selectable_value(&mut self.stage_tab, i, i.to_string());
                        }
                    });
                    self.stage_ui(ui, &mut a, ch_num);
                } else {
                    self.serial_ui(ui, &mut a, ch_num);
                }
            });
        });
        if a != before {
            s.state().adv_trigger = a;
        }
    }

    fn stage_ui(&mut self, ui: &mut Ui, a: &mut AdvTrigger, ch_num: usize) {
        let st = &mut a.stage[self.stage_tab];
        ui.vertical(|ui| {
            let combo = |ui: &mut Ui, label: &str, id: &str, v: &mut u8, items: [&str; 2]| {
                ui.horizontal(|ui| {
                    let l = ui.label(label);
                    egui::ComboBox::from_id_salt(id)
                        .selected_text(items[*v as usize])
                        .show_ui(ui, |ui| {
                            for (k, t) in items.iter().enumerate() {
                                ui.selectable_value(v, k as u8, *t);
                            }
                        })
                        .response
                        .labelled_by(l.id);
                });
            };
            combo(ui, "Logic", "trig-logic", &mut st.logic, ["Or", "And"]);

            if ch_num == 32 {
                value_edit(
                    ui,
                    "31 --------- 24 23 ---------- 16",
                    &mut st.value0_ext32,
                    "X01RFC",
                );
            }
            value_edit(
                ui,
                "15 ---------- 8 7 ----------- 0",
                &mut st.value0,
                "X01RFC",
            );

            combo(ui, "Inv", "trig-inv0", &mut st.inv0, ["==", "!="]);

            if ch_num == 32 {
                value_edit(
                    ui,
                    "31 --------- 24 23 ---------- 16",
                    &mut st.value1_ext32,
                    "X01RFC",
                );
            }
            value_edit(
                ui,
                "15 ---------- 8 7 ----------- 0",
                &mut st.value1,
                "X01RFC",
            );

            combo(ui, "Inv", "trig-inv1", &mut st.inv1, ["==", "!="]);

            ui.checkbox(&mut st.contiguous, "Contiguous");

            let l = ui.label("Counter");
            let mut c = st.count as i64;
            ui.add(egui::DragValue::new(&mut c).range(1..=i32::MAX as i64))
                .labelled_by(l.id);
            st.count = c as u32;
        });
        ui.label(egui::RichText::new(NOTE).monospace().small());
    }

    fn serial_ui(&mut self, ui: &mut Ui, a: &mut AdvTrigger, ch_num: usize) {
        let sr = &mut a.serial;
        egui::Grid::new("trig-serial")
            .num_columns(2)
            .show(ui, |ui| {
                for (label, v, v32) in [
                    ("Start Flag: ", &mut sr.start, &mut sr.start_ext32),
                    ("Stop Flag: ", &mut sr.stop, &mut sr.stop_ext32),
                    ("Clock Flag: ", &mut sr.clock, &mut sr.clock_ext32),
                ] {
                    if ch_num == 32 {
                        value_edit(ui, label, v32, "X01RFC");
                    }
                    value_edit(ui, if ch_num == 32 { "" } else { label }, v, "X01RFC");
                }
                let l = ui.label("Data Channel: ");
                egui::ComboBox::from_id_salt("trig-serial-ch")
                    .selected_text(sr.channel.to_string())
                    .show_ui(ui, |ui| {
                        for i in 0..ch_num {
                            ui.selectable_value(&mut sr.channel, i, i.to_string());
                        }
                    })
                    .response
                    .labelled_by(l.id);

                ui.label("Data Bits");
                egui::ComboBox::from_id_salt("trig-serial-bits")
                    .selected_text((sr.bits + 1).to_string())
                    .show_ui(ui, |ui| {
                        for i in 0..16 {
                            ui.selectable_value(&mut sr.bits, i, (i + 1).to_string());
                        }
                    });

                value_edit(ui, "Data Value: ", &mut sr.data, "X01");

                // the value as hex, typed when "Input hex" is on
                let l = ui.label("Hex: ");
                let mut on = self.hex.is_some();
                ui.horizontal(|ui| match &mut self.hex {
                    Some(h) => {
                        let r = ui
                            .add(
                                egui::TextEdit::singleline(h)
                                    .desired_width(60.0)
                                    .char_limit(4),
                            )
                            .labelled_by(l.id);
                        if r.changed() {
                            h.retain(|c| c.is_ascii_hexdigit());
                            *h = h.to_uppercase();
                            if let Some(v) = serial_from_hex(h) {
                                sr.data = v;
                            }
                        }
                    }
                    None => {
                        ui.label(serial_hex(&sr.data)).labelled_by(l.id);
                    }
                });

                if ui.checkbox(&mut on, "Input hex").changed() {
                    self.hex = on.then(|| serial_hex(&sr.data));
                }
            });
        ui.label(egui::RichText::new(NOTE).monospace().small());
    }
}
