//! The oscilloscope trigger dock (`pv/dock/dsotriggerdock.cpp`): position,
//! source, slope, hold-off and sensitivity, written to the device as they
//! change.

use crate::i18n::tr;
use dsview_core::{ConfValue, channel_type, key};
use dsview_session::Session;
use eframe::egui::{self, Ui};

/// `DSO_TRIGGER_AUTO` .. `DSO_TRIGGER_CH0O1`
const SOURCES: [(u8, &str); 5] = [
    (0, "Auto"),
    (1, "Channel 0"),
    (2, "Channel 1"),
    (3, "Channel 0 && 1"),
    (4, "Channel 0 | 1"),
];
/// The hold-off units: S, mS, uS (ns per unit).
const HOLD_UNITS: [(f64, &str); 3] = [(1e9, "S"), (1e6, "mS"), (1e3, "uS")];

#[derive(Default)]
pub struct DsoTriggerDock {
    pub open: bool,
    /// The hold-off unit shown (`_holdoff_comboBox`).
    hold_unit: Option<usize>,
    pub message: Option<String>,
}

fn byte(s: &Session, k: i32) -> Option<u8> {
    s.config(k, None).map(|v| v.get_byte())
}

impl DsoTriggerDock {
    pub fn show(&mut self, ui: &mut Ui, s: &Session) {
        ui.heading(tr("Trigger"));
        let dev = s.state().device.clone();
        let is_file = dev.as_ref().is_some_and(|d| d.is_file());
        let is_demo = dev.as_ref().is_some_and(|d| d.is_demo());
        if is_file {
            ui.label(tr("A file has no trigger settings."));
            return;
        }
        // update_view(): the demo only moves the position
        let locked = is_demo;

        // position
        let cur = byte(s, key::HORIZ_TRIGGERPOS).unwrap_or(50) as i32;
        let mut pos = cur;
        let l = ui
            .horizontal(|ui| {
                let l = ui.label(tr("Trigger Position: "));
                ui.add(egui::DragValue::new(&mut pos).range(1..=99).suffix("%"))
                    .labelled_by(l.id);
                l.id
            })
            .inner;
        ui.add(egui::Slider::new(&mut pos, 1..=99).show_value(false))
            .labelled_by(l);
        if pos != cur {
            self.pos_changed(s, pos);
        }
        ui.add_space(8.0);

        ui.add_enabled_ui(!locked, |ui| {
            // source
            let src_label = ui.label(tr("Trigger Source: ")).id;
            let src = byte(s, key::TRIGGER_SOURCE).unwrap_or(0);
            let enabled = |i: u16| {
                s.channels()
                    .iter()
                    .any(|c| c.ty == channel_type::DSO && c.index == i && c.enabled)
            };
            let mut new_src = None;
            for (id, name) in SOURCES {
                if ui.radio(src == id, name).clicked() {
                    new_src = Some(id);
                }
                if id == 0 {
                    // the auto mode's channel
                    let ch = byte(s, key::TRIGGER_CHANNEL).unwrap_or(0);
                    let names: Vec<(u16, String)> = s
                        .state()
                        .signals
                        .iter()
                        .filter(|x| x.ty == channel_type::DSO)
                        .map(|x| (x.index, x.name.clone()))
                        .collect();
                    let cur = names
                        .iter()
                        .find(|n| n.0 as u8 == ch)
                        .map_or(String::new(), |n| n.1.clone());
                    egui::ComboBox::from_id_salt("dso-trig-channel")
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            for (i, n) in &names {
                                if ui.selectable_label(*i as u8 == ch, n).clicked()
                                    && !s.set_config(
                                        key::TRIGGER_CHANNEL,
                                        ConfValue::Byte(*i as u8),
                                        None,
                                    )
                                {
                                    self.message = Some("Change trigger channel failed!".into());
                                }
                            }
                        })
                        .response
                        .labelled_by(src_label);
                }
            }
            if let Some(id) = new_src {
                // check_trig_channel(): disabled channels cannot trigger
                let ok = match id {
                    1 => enabled(0),
                    2 => enabled(1),
                    3 => enabled(0) && enabled(1),
                    _ => true,
                };
                let id = if ok {
                    id
                } else {
                    self.message = Some("Disabled channels cannot be used for triggering!".into());
                    0
                };
                if !s.set_config(key::TRIGGER_SOURCE, ConfValue::Byte(id), None) {
                    self.message = Some("Change trigger source failed!".into());
                }
            }
            ui.add_space(8.0);

            // slope
            ui.label(tr("Trigger Type: "));
            let slope = byte(s, key::TRIGGER_SLOPE).unwrap_or(0);
            for (id, name) in [(0u8, "Rising Edge"), (1, "Falling Edge")] {
                if ui.radio(slope == id, name).clicked()
                    && !s.set_config(key::TRIGGER_SLOPE, ConfValue::Byte(id), None)
                {
                    self.message = Some("Change trigger type failed!".into());
                }
            }
            ui.add_space(8.0);

            // hold-off: the value × unit / 10
            let holdoff = s
                .config(key::TRIGGER_HOLDOFF, None)
                .map_or(0, |v| v.get_u64());
            let unit = *self.hold_unit.get_or_insert_with(|| {
                let v = holdoff as f64 * 10.0;
                HOLD_UNITS.iter().position(|u| v >= u.0).unwrap_or(0)
            });
            let max = if HOLD_UNITS[unit].0 == 1e9 { 10 } else { 999 };
            let mut value = ((holdoff as f64 * 10.0 / HOLD_UNITS[unit].0) as i64).clamp(0, max);
            let mut new_unit = unit;
            let hl = ui
                .horizontal(|ui| {
                    let l = ui.label(tr("Hold Off Time: "));
                    ui.add(egui::DragValue::new(&mut value).range(0..=max))
                        .labelled_by(l.id);
                    egui::ComboBox::from_id_salt("dso-holdoff-unit")
                        .selected_text(HOLD_UNITS[unit].1)
                        .show_ui(ui, |ui| {
                            for (i, u) in HOLD_UNITS.iter().enumerate() {
                                ui.selectable_value(&mut new_unit, i, u.1);
                            }
                        })
                        .response
                        .labelled_by(l.id);
                    l.id
                })
                .inner;
            ui.add(egui::Slider::new(&mut value, 0..=max).show_value(false))
                .labelled_by(hl);
            let old = ((holdoff as f64 * 10.0 / HOLD_UNITS[unit].0) as i64).clamp(0, max);
            if value != old || new_unit != unit {
                let new_max = if HOLD_UNITS[new_unit].0 == 1e9 {
                    10
                } else {
                    999
                };
                self.hold_unit = Some(new_unit);
                let v = (value.min(new_max) as f64 * HOLD_UNITS[new_unit].0 / 10.0) as u64;
                if !s.set_config(key::TRIGGER_HOLDOFF, ConfValue::U64(v), None) {
                    self.message = Some("Change trigger hold off time failed!".into());
                }
            }
            ui.add_space(8.0);

            // sensitivity
            let ml = ui.label(tr("Noise Sensitivity: ")).id;
            let margin = byte(s, key::TRIGGER_MARGIN).unwrap_or(0);
            let mut m = margin as i32;
            ui.add(egui::Slider::new(&mut m, 0..=15).show_value(false))
                .labelled_by(ml);
            if m != margin as i32
                && !s.set_config(key::TRIGGER_MARGIN, ConfValue::Byte(m as u8), None)
            {
                self.message = Some("Change trigger value sensitivity failed!".into());
            }
        });
    }

    /// `pos_changed`, then `View::set_trig_pos`: the trigger cursor moves
    /// while there is no data to show or a capture runs.
    fn pos_changed(&mut self, s: &Session, pos: i32) {
        if !s.set_config(key::HORIZ_TRIGGERPOS, ConfValue::Byte(pos as u8), None) {
            self.message = Some("Change horiz trigger position failed!".into());
            return;
        }
        if !s.have_view_data() || s.is_working() {
            let limit = s.state().cur_samplelimits;
            s.set_trigger_cursor(limit * pos as u64 / 100);
        }
    }
}
