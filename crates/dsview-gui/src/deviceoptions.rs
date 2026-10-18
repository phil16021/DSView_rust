//! The device options dialog (`pv/dialogs/deviceoptions.cpp` with the
//! `prop::binding::DeviceOptions` and `ProbeOptions` bindings): the
//! driver's option list turned into controls, the logic channel mode and
//! channels, the analog channels' probe options, and the DSO calibration
//! entries. Values go to the device on OK, except "Operation Mode" and the
//! channel mode, which apply at once (they change what the dialog shows).

use dsview_core::{ConfValue, ListItem, Mode, key};
use dsview_formats::cfmt;
use dsview_session::Session;
use dsview_session::store::config_info;
use eframe::egui::{self, Ui};

/// One control.
#[derive(Clone, Debug)]
enum Kind {
    /// A choice among values (`bind_enum`).
    Enum(Vec<(ConfValue, String)>, usize),
    /// A choice among list items, stored as their id (`bind_list`).
    List(Vec<ListItem>, i16),
    Bool(bool),
    Double {
        v: f64,
        min: f64,
        max: f64,
        decimals: usize,
        step: f64,
        suffix: &'static str,
    },
    Int {
        v: i64,
        min: i64,
        max: i64,
        suffix: &'static str,
    },
    /// Sample rate as a number in a range (`samplerate-steps`).
    Rate {
        v: f64,
        min: f64,
        max: f64,
        step: f64,
    },
}

#[derive(Clone, Debug)]
struct Prop {
    key: i32,
    /// The channel position for probe options.
    ch: Option<usize>,
    label: String,
    kind: Kind,
    /// Applied at once (`get_widget(parent, true)`).
    auto: bool,
}

impl Prop {
    fn value(&self) -> Option<ConfValue> {
        Some(match &self.kind {
            Kind::Enum(items, i) => items.get(*i)?.0.clone(),
            Kind::List(_, id) => ConfValue::I16(*id),
            Kind::Bool(b) => ConfValue::Bool(*b),
            Kind::Double { v, .. } => ConfValue::F64(*v),
            Kind::Int { v, .. } => ConfValue::U64(*v as u64),
            Kind::Rate { v, .. } => ConfValue::U64(*v as u64),
        })
    }

    fn commit(&self, s: &Session) {
        if let Some(v) = self.value() {
            s.set_config(self.key, v, self.ch);
        }
    }

    /// The control; true when the value changed.
    fn ui(&mut self, ui: &mut Ui, id: usize) -> bool {
        let l = ui.label(&self.label);
        let r = match &mut self.kind {
            Kind::Enum(items, sel) => {
                let mut i = *sel;
                let r = egui::ComboBox::from_id_salt(("opt", id))
                    .selected_text(items.get(i).map_or("", |x| x.1.as_str()))
                    .show_ui(ui, |ui| {
                        for (k, (_, t)) in items.iter().enumerate() {
                            ui.selectable_value(&mut i, k, t);
                        }
                    })
                    .response;
                let ch = i != *sel;
                *sel = i;
                return ch | r.labelled_by(l.id).changed();
            }
            Kind::List(items, v) => {
                let mut x = *v;
                let cur = items
                    .iter()
                    .find(|it| it.id == x as i32)
                    .map_or("", |it| it.name.as_str());
                egui::ComboBox::from_id_salt(("opt", id))
                    .selected_text(cur)
                    .show_ui(ui, |ui| {
                        for it in items.iter() {
                            ui.selectable_value(&mut x, it.id as i16, &it.name);
                        }
                    })
                    .response
                    .labelled_by(l.id);
                let ch = x != *v;
                *v = x;
                return ch;
            }
            Kind::Bool(b) => ui.checkbox(b, ""),
            Kind::Double {
                v,
                min,
                max,
                decimals,
                step,
                suffix,
            } => ui.add(
                egui::DragValue::new(v)
                    .range(*min..=*max)
                    .speed(*step)
                    .fixed_decimals(*decimals)
                    .suffix(*suffix),
            ),
            Kind::Int {
                v,
                min,
                max,
                suffix,
            } => ui.add(egui::DragValue::new(v).range(*min..=*max).suffix(*suffix)),
            Kind::Rate { v, min, max, step } => ui.add(
                egui::DragValue::new(v)
                    .range(*min..=*max)
                    .speed(*step)
                    .suffix(" Hz"),
            ),
        };
        r.labelled_by(l.id).changed()
    }
}

/// `g_variant_print` of a list value, or the string itself.
fn print(v: &ConfValue) -> String {
    match v {
        ConfValue::Str(s) => s.clone(),
        ConfValue::Bool(b) => b.to_string(),
        ConfValue::Byte(x) => x.to_string(),
        ConfValue::I16(x) => x.to_string(),
        ConfValue::U16(x) => x.to_string(),
        ConfValue::I32(x) => x.to_string(),
        ConfValue::U32(x) => x.to_string(),
        ConfValue::I64(x) => x.to_string(),
        ConfValue::U64(x) => x.to_string(),
        ConfValue::F64(x) => x.to_string(),
        other => format!("{other:?}"),
    }
}

/// The values of a list as single values.
fn items(list: Option<ConfValue>) -> Vec<ConfValue> {
    match list {
        Some(ConfValue::StrV(v)) => v.into_iter().map(ConfValue::Str).collect(),
        Some(ConfValue::U64s(v)) => v.into_iter().map(ConfValue::U64).collect(),
        Some(ConfValue::I32s(v)) => v.into_iter().map(ConfValue::I32).collect(),
        Some(ConfValue::Bytes(v)) => v.into_iter().map(ConfValue::Byte).collect(),
        _ => Vec::new(),
    }
}

/// A value of a dictionary list (`g_variant_lookup_value`).
fn lookup(list: Option<ConfValue>, name: &str) -> Option<ConfValue> {
    match list {
        Some(ConfValue::Dict(d)) => d.into_iter().find(|(k, _)| k == name).map(|(_, v)| v),
        _ => None,
    }
}

/// `sr_voltage_string` for `print_vdiv`.
fn vdiv_string(mv: u64) -> String {
    if mv < 1000 {
        format!("{mv}mV")
    } else {
        format!("{}V", mv / 1000)
    }
}

fn enum_prop(
    key: i32,
    ch: Option<usize>,
    label: &str,
    values: Vec<ConfValue>,
    cur: Option<ConfValue>,
    printer: fn(&ConfValue) -> String,
) -> Prop {
    let items: Vec<(ConfValue, String)> = values
        .into_iter()
        .map(|v| {
            let t = printer(&v);
            (v, t)
        })
        .collect();
    let sel = cur
        .and_then(|c| items.iter().position(|(v, _)| *v == c))
        .unwrap_or(0);
    Prop {
        key,
        ch,
        label: label.into(),
        kind: Kind::Enum(items, sel),
        auto: false,
    }
}

/// `binding::DeviceOptions`: the device's options.
fn device_props(s: &Session) -> Vec<Prop> {
    let Some(ConfValue::I32s(keys)) = s.config_list(key::DEVICE_OPTIONS) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for k in keys {
        let Some((_, name)) = config_info(k) else {
            continue;
        };
        let list = s.config_list(k);
        let cur = s.config(k, None);
        let p = match k {
            key::SAMPLERATE => {
                if let Some(ConfValue::U64s(st)) = lookup(list.clone(), "samplerate-steps")
                    && st.len() == 3
                {
                    let v = cur.map_or(0.0, |c| c.get_u64() as f64);
                    Some(Prop {
                        key: k,
                        ch: None,
                        label: name.into(),
                        kind: Kind::Rate {
                            v,
                            min: st[0] as f64,
                            max: st[1] as f64,
                            step: st[2] as f64,
                        },
                        auto: false,
                    })
                } else {
                    lookup(list, "samplerates").map(|l| {
                        enum_prop(k, None, name, items(Some(l)), cur, |v| {
                            cfmt::samplerate_string(v.get_u64())
                        })
                    })
                }
            }
            key::CAPTURE_RATIO => Some(Prop {
                key: k,
                ch: None,
                label: name.into(),
                kind: Kind::Int {
                    v: cur.map_or(0, |c| c.get_u64() as i64),
                    min: 0,
                    max: 100,
                    suffix: "%",
                },
                auto: false,
            }),
            key::PATTERN_MODE
            | key::BUFFERSIZE
            | key::TRIGGER_SOURCE
            | key::MAX_HEIGHT
            | key::MAX_HEIGHT_VALUE
            | key::PROBE_COUPLING
            | key::PROBE_EN
            | key::ZERO
            | key::STREAM
            | key::TEST
            | key::STATUS
            | key::PROBE_FACTOR => Some(enum_prop(k, None, name, items(list), cur, print)),
            key::OPERATION_MODE | key::BUFFER_OPTIONS | key::THRESHOLD | key::FILTER => {
                match list {
                    Some(ConfValue::List(items)) => Some(Prop {
                        key: k,
                        ch: None,
                        label: name.into(),
                        kind: Kind::List(items, cur.map_or(0, |c| c.get_i16())),
                        auto: false,
                    }),
                    _ => None,
                }
            }
            key::VTH => Some(Prop {
                key: k,
                ch: None,
                label: name.into(),
                kind: Kind::Double {
                    v: cur.map_or(0.0, |c| c.get_f64()),
                    min: 0.0,
                    max: 5.0,
                    decimals: 1,
                    step: 0.1,
                    suffix: "V",
                },
                auto: false,
            }),
            key::RLE | key::RLE_SUPPORT | key::CLOCK_TYPE | key::CLOCK_EDGE | key::INSTANT => {
                Some(Prop {
                    key: k,
                    ch: None,
                    label: name.into(),
                    kind: Kind::Bool(cur.is_some_and(|c| c.get_bool())),
                    auto: false,
                })
            }
            key::EXT_SAMPLERATE => Some(Prop {
                key: k,
                ch: None,
                label: name.into(),
                kind: Kind::Int {
                    v: cur.map_or(0, |c| c.get_u64() as i64),
                    min: 10_000,
                    max: 1_000_000_000,
                    suffix: "",
                },
                auto: false,
            }),
            key::TIMEBASE => Some(enum_prop(k, None, name, items(list), cur, |v| {
                cfmt::period_string(v.get_u64())
            })),
            key::PROBE_VDIV => Some(enum_prop(k, None, name, items(list), cur, |v| {
                vdiv_string(v.get_u64())
            })),
            _ => None,
        };
        if let Some(mut p) = p {
            // "Operation Mode" applies at once
            p.auto = name == "Operation Mode";
            out.push(p);
        }
    }
    out
}

/// `binding::ProbeOptions` for the channel at position `pos`.
fn probe_props(s: &Session, pos: usize) -> Vec<Prop> {
    let Some(ConfValue::I32s(keys)) = s.config_list(key::PROBE_CONFIGS) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for k in keys {
        let Some((_, name)) = config_info(k) else {
            continue;
        };
        let list = s.config_list(k);
        let cur = s.config(k, Some(pos));
        let p = match k {
            key::PROBE_VDIV => lookup(list, "vdivs").map(|l| {
                enum_prop(k, Some(pos), name, items(Some(l)), cur, |v| {
                    vdiv_string(v.get_u64())
                })
            }),
            key::PROBE_MAP_MIN | key::PROBE_MAP_MAX => Some(Prop {
                key: k,
                ch: Some(pos),
                label: name.into(),
                kind: Kind::Double {
                    v: cur.map_or(0.0, |c| c.get_f64()),
                    min: -999_999.99,
                    max: 999_999.99,
                    decimals: 2,
                    step: 0.01,
                    suffix: "",
                },
                auto: false,
            }),
            key::PROBE_COUPLING => lookup(list, "coupling").map(|l| {
                enum_prop(k, Some(pos), name, items(Some(l)), cur, |v| {
                    match v.get_byte() {
                        0 => "DC",
                        1 => "AC",
                        2 => "GND",
                        _ => "Undefined",
                    }
                    .into()
                })
            }),
            key::PROBE_MAP_UNIT => Some(enum_prop(k, Some(pos), name, items(list), cur, print)),
            key::PROBE_MAP_DEFAULT => Some(Prop {
                key: k,
                ch: Some(pos),
                label: name.into(),
                kind: Kind::Bool(cur.is_some_and(|c| c.get_bool())),
                auto: false,
            }),
            _ => None,
        };
        out.extend(p);
    }
    out
}

/// The dialog's state while open.
pub struct DeviceOptions {
    props: Vec<Prop>,
    /// Channel enables, by channel position.
    enables: Vec<bool>,
    /// Analog: per channel position, its probe options.
    probes: Vec<Vec<Prop>>,
    tab: usize,
    /// What the panels were built for (`mode_check_timeout`).
    built_for: (i32, Option<ConfValue>, Option<ConfValue>),
    pub message: Option<String>,
}

/// What OK leads to (`SamplingBar::on_configure`).
pub enum Outcome {
    Open,
    Cancelled,
    Accepted,
    /// DSO: OK with "Auto Calibration" chosen.
    AutoCalibration,
    /// DSO: "Manual Calibration".
    ManualCalibration,
}

impl DeviceOptions {
    pub fn new(s: &Session) -> DeviceOptions {
        let mut d = DeviceOptions {
            props: device_props(s),
            enables: Vec::new(),
            probes: Vec::new(),
            tab: 0,
            built_for: (s.mode(), None, None),
            message: None,
        };
        d.build_channels(s);
        d
    }

    /// `build_dynamic_panel`
    fn build_channels(&mut self, s: &Session) {
        let chans = s.channels();
        self.enables = chans.iter().map(|c| c.enabled).collect();
        self.probes = if s.mode() == Mode::Analog as i32 {
            (0..chans.len()).map(|p| probe_props(s, p)).collect()
        } else {
            Vec::new()
        };
        self.built_for = (
            s.mode(),
            s.config(key::OPERATION_MODE, None),
            s.config(key::PATTERN_MODE, None),
        );
    }

    pub fn show(&mut self, ctx: &egui::Context, s: &Session) -> Outcome {
        let mut out = Outcome::Open;
        // the operation mode changed: the channel panel follows
        let now = (
            s.mode(),
            s.config(key::OPERATION_MODE, None),
            s.config(key::PATTERN_MODE, None),
        );
        if now != self.built_for {
            self.build_channels(s);
        }
        let mut open = !crate::msgbox::esc(ctx);
        egui::Window::new("Device Options")
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(600.0)
                    .show(ui, |ui| {
                        ui.strong("Mode");
                        egui::Grid::new("dev-props").num_columns(2).show(ui, |ui| {
                            for (i, p) in self.props.iter_mut().enumerate() {
                                if p.ui(ui, i) && p.auto {
                                    p.commit(s);
                                }
                                ui.end_row();
                            }
                        });
                        ui.separator();
                        match s.mode() {
                            m if m == Mode::Logic as i32 => self.logic_channels(ui, s),
                            m if m == Mode::Analog as i32 => self.analog_channels(ui, s),
                            _ => {
                                if s.config(key::HAVE_ZERO, None).is_some_and(|v| v.get_bool()) {
                                    ui.strong("Calibration");
                                    if ui.button("Auto Calibration").clicked() {
                                        out = Outcome::AutoCalibration;
                                    }
                                    if ui.button("Manual Calibration").clicked() {
                                        out = Outcome::ManualCalibration;
                                    }
                                }
                            }
                        }
                    });
                ui.separator();
                if ui.button("OK").clicked() {
                    out = self.accept(s);
                }
            });
        if !open {
            return Outcome::Cancelled;
        }
        if matches!(out, Outcome::ManualCalibration | Outcome::AutoCalibration) {
            // `zero_adj` / `on_calibration` accept the dialog first
            self.commit(s);
        }
        out
    }

    fn logic_channels(&mut self, ui: &mut Ui, s: &Session) {
        ui.strong("Channel");
        // channel mode, applied at once
        if let Some(ConfValue::List(modes)) = s.config_list(key::CHANNEL_MODE) {
            let cur = s.config(key::CHANNEL_MODE, None).map_or(0, |v| v.get_i16());
            for m in modes {
                if ui.radio(cur == m.id as i16, &m.name).clicked() && cur != m.id as i16 {
                    s.set_config(key::CHANNEL_MODE, ConfValue::I16(m.id as i16), None);
                    self.build_channels(s);
                }
            }
        }
        let vld = s
            .config(key::VLD_CH_NUM, None)
            .map(|v| v.get_i16() as usize);
        let total = s
            .config(key::TOTAL_CH_NUM, None)
            .map(|v| v.get_i16() as usize)
            .or(vld)
            .unwrap_or(self.enables.len());
        let stream = s.config(key::STREAM, None).is_some_and(|v| v.get_bool());
        let test = s.config(key::TEST, None).is_some_and(|v| v.get_bool());
        // more enabled than the mode allows: the rest off (`logic_probes`)
        if let Some(v) = vld {
            let mut on = 0;
            for e in self.enables.iter_mut() {
                if *e {
                    on += 1;
                    if on > v {
                        *e = false;
                    }
                }
            }
        }
        egui::Grid::new("dev-chans").num_columns(8).show(ui, |ui| {
            for i in 0..total {
                let mut msg = None;
                ui.add_enabled_ui(i < self.enables.len() && !test, |ui| {
                    let mut on = self.enables.get(i).copied().unwrap_or(false) || test;
                    if ui.checkbox(&mut on, i.to_string()).changed() && i < self.enables.len() {
                        // stream mode: at most the valid channel count
                        let count = self.enables.iter().filter(|x| **x).count() + 1;
                        if on && stream && vld.is_some_and(|v| count > v) {
                            msg = Some(format!("max count of channels! ({})", vld.unwrap_or(0)));
                        } else {
                            self.enables[i] = on;
                        }
                    }
                });
                if msg.is_some() {
                    self.message = msg;
                }
                if i % 8 == 7 {
                    ui.end_row();
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Enable All").clicked() {
                if stream && let Some(v) = vld {
                    // enable_max_probes
                    let mut on = self.enables.iter().filter(|x| **x).count();
                    for e in self.enables.iter_mut() {
                        if on >= v {
                            break;
                        }
                        if !*e {
                            *e = true;
                            on += 1;
                        }
                    }
                } else {
                    self.enables.iter_mut().for_each(|e| *e = true);
                }
            }
            if ui.button("Disable All").clicked() {
                self.enables.iter_mut().for_each(|e| *e = false);
            }
        });
    }

    fn analog_channels(&mut self, ui: &mut Ui, s: &Session) {
        ui.strong("Channel");
        let chans = s.channels();
        ui.horizontal(|ui| {
            for (i, c) in chans.iter().enumerate() {
                ui.selectable_value(&mut self.tab, i, format!("{} ", c.name));
            }
        });
        let i = self.tab.min(chans.len().saturating_sub(1));
        if let Some(on) = self.enables.get_mut(i) {
            ui.checkbox(on, "Enable: ");
            let enabled = *on;
            let map_default = self
                .probes
                .get(i)
                .and_then(|p| p.iter().find(|x| x.key == key::PROBE_MAP_DEFAULT))
                .is_some_and(|p| matches!(p.kind, Kind::Bool(true)));
            if let Some(props) = self.probes.get_mut(i) {
                egui::Grid::new(("probe", i)).num_columns(2).show(ui, |ui| {
                    for (k, p) in props.iter_mut().enumerate() {
                        let is_map = matches!(
                            p.key,
                            key::PROBE_MAP_UNIT | key::PROBE_MAP_MIN | key::PROBE_MAP_MAX
                        );
                        ui.add_enabled_ui(enabled && !(is_map && map_default), |ui| {
                            // `analog_channel_check`: "Map Default" applies at once
                            if p.ui(ui, 1000 + i * 100 + k) && p.key == key::PROBE_MAP_DEFAULT {
                                p.commit(s);
                            }
                        });
                        ui.end_row();
                    }
                });
                if map_default {
                    // the default map follows V/div: re-read it
                    let fresh = probe_props(s, i);
                    for p in props.iter_mut().filter(|p| {
                        matches!(
                            p.key,
                            key::PROBE_MAP_UNIT | key::PROBE_MAP_MIN | key::PROBE_MAP_MAX
                        )
                    }) {
                        if let Some(f) = fresh.iter().find(|f| f.key == p.key) {
                            p.kind = f.kind.clone();
                        }
                    }
                }
            }
        }
    }

    /// `accept()` without closing: the properties, the channel enables, the
    /// probe options.
    fn commit(&self, s: &Session) {
        for p in &self.props {
            p.commit(s);
        }
        let mode = s.mode();
        if mode == Mode::Logic as i32 || mode == Mode::Analog as i32 {
            let chans = s.channels();
            for (c, &on) in chans.iter().zip(&self.enables) {
                if c.enabled != on {
                    s.update_channel(c.index, c.ty, |x| x.enabled = on);
                }
            }
        }
        for props in &self.probes {
            for p in props {
                p.commit(s);
            }
        }
    }

    fn accept(&mut self, s: &Session) -> Outcome {
        let mode = s.mode();
        if (mode == Mode::Logic as i32 || mode == Mode::Analog as i32)
            && !self.enables.iter().any(|x| *x)
        {
            self.message = Some("All channel disabled! Please enable at least one channel.".into());
            return Outcome::Open;
        }
        self.commit(s);
        Outcome::Accepted
    }
}
