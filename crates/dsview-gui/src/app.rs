//! The main window: menu, device line, the time view, the status bar.

use crate::i18n::tr;
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
    LoadProfile(PathBuf),
    StoreProfile(PathBuf),
    Screenshot(PathBuf),
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
    pub trigger: crate::triggerdock::TriggerDock,
    pub measure: crate::measuredock::MeasureDock,
    pub search: crate::searchdock::SearchDock,
    pub calibration: Option<crate::calibration::Calibration>,
    pub store: Option<crate::storedlg::StoreDialog>,
    /// A screenshot to save here once the frame is grabbed.
    screenshot: Option<PathBuf>,
    ctx_for_shot: bool,
    shot_result: Option<Receiver<Result<(), String>>>,
    /// The application options and their dialog's working copy.
    pub options: crate::options::AppOptions,
    options_edit: Option<crate::options::AppOptions>,
    pub fft: crate::fftview::FftState,
    pub shared: Shared,
    pub version: String,
    events: Receiver<SessionEvent>,
    files: Option<Receiver<Option<FileJob>>>,
    message: Option<String>,
    switch_prompt: Option<(u64, String)>,
    /// A decoder from an opened file whose program or file options wait
    /// for the user's yes.
    held: Option<dsview_session::store::HeldDecoder>,
    known_devices: usize,
    /// "Save captured data?" is asked; what follows either way.
    store_confirm: Option<AfterSave>,
    /// What follows the save that answer started (`_is_auto_switch_device`,
    /// `_next_switch_device`).
    after_save: Option<AfterSave>,
    /// The window title set last (`update_title_bar_text`).
    title: String,
    /// Close the window this frame.
    close_now: bool,
    /// The Log Options dialog while open.
    log_options: Option<crate::log::LogOptions>,
    /// The About window is open.
    about: bool,
    /// The shortcuts' modifier filter and debounce.
    keys: dsview_viewmodel::input::KeyGate,
    started: Instant,
    /// The work mode whose docks are shown.
    dock_mode: Option<i32>,
    /// The dialogs at start-up, while any is left (set by `main`).
    pub startup: Option<crate::startup::Startup>,
    /// The restored window was checked against the screen.
    window_checked: bool,
    pub bench: Option<Bench>,
    pub soak: Option<crate::soak::Soak>,
}

/// The directory a file dialog for `job` starts in: the one used last
/// for that kind of file (`UserHistory`), if it still exists.
fn history_dir(h: &crate::options::UserHistory, job: &FileJob) -> Option<PathBuf> {
    let dir = match job {
        FileJob::Open(_) => PathBuf::from(&h.open_dir),
        FileJob::Save(_) => PathBuf::from(&h.save_dir),
        FileJob::Export(_) => PathBuf::from(&h.export_dir),
        FileJob::LoadProfile(_) | FileJob::StoreProfile(_) => PathBuf::from(&h.session_dir),
        FileJob::Screenshot(_) => PathBuf::from(&h.screenshot_path).parent()?.to_owned(),
    };
    dir.is_dir().then_some(dir)
}

/// Remembers where the chosen file is (`userHistory.* = ...`); for
/// screenshots the file itself, as Qt.
fn remember_dir(h: &mut crate::options::UserHistory, job: &FileJob) {
    let dir = |p: &PathBuf| {
        p.parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match job {
        FileJob::Open(p) => h.open_dir = dir(p),
        FileJob::Save(p) => h.save_dir = dir(p),
        FileJob::Export(p) => h.export_dir = dir(p),
        FileJob::LoadProfile(p) | FileJob::StoreProfile(p) => h.session_dir = dir(p),
        FileJob::Screenshot(p) => h.screenshot_path = p.to_string_lossy().into_owned(),
    }
}

/// What happens once the captured data was saved or not.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AfterSave {
    /// The window was being closed: "No" closes it (`able_to_close`); after
    /// a save it stays open, as in Qt.
    Close,
    /// `set_default_device`
    DefaultDevice,
    Device(u64),
}

impl App {
    /// `confirm_to_store_data()`: offers to save a hardware capture once per
    /// capture (not with "don't ask to save" where `ask_option` says so).
    /// True when the question is asked; `then` follows its answer.
    fn confirm_store(&mut self, then: AfterSave, ask_option: bool) -> bool {
        if ask_option && self.options.dont_ask_save_on_exit {
            return false;
        }
        if self.session.have_hardware_data() && self.session.is_first_store_confirm() {
            self.store_confirm = Some(then);
            return true;
        }
        false
    }

    /// `MainWindow::switchLanguage`: saved, and every text in it from the
    /// next frame on.
    pub fn set_language(&mut self, lang: i64) {
        self.options.frame.language = lang;
        self.options.save();
        self.session.state().language = lang;
        crate::i18n::load(&crate::i18n::lang_dir(self.session.data_dir()), lang);
    }

    /// `MainWindow::openDoc`: the user guide in the language.
    fn open_manual(&mut self) {
        match crate::help::manual_path(self.session.data_dir(), self.options.frame.language) {
            Some(p) => crate::help::open(p),
            None => self.message = Some("File does not exist!".into()),
        }
    }

    /// One shortcut (`MainWindow::eventFilter`). T/D/M/R toggle only the
    /// docks the work mode offers (`TrigBar::*_clicked`).
    pub fn shortcut(&mut self, sc: dsview_viewmodel::input::Shortcut) {
        use dsview_viewmodel::input::{Shortcut as K, dock_offered};
        let mode = self.session.mode();
        match sc {
            K::RunStop => self.bar.run_stop(&self.session, false),
            K::Instant => self.bar.run_stop(&self.session, true),
            K::Trigger | K::Protocol | K::Measure | K::Search => {
                if dock_offered(mode, sc) {
                    let open = self.dock_flag(sc);
                    *open = !*open;
                }
            }
            K::DeviceOptions => {
                if self.bar.options.is_none() {
                    self.bar.options =
                        Some(crate::deviceoptions::DeviceOptions::new(&self.session));
                }
            }
            _ => {
                self.view.key(&self.session, sc);
            }
        }
    }

    /// The open flag of a dock.
    fn dock_flag(&mut self, dock: dsview_viewmodel::input::Shortcut) -> &mut bool {
        use dsview_viewmodel::input::Shortcut as K;
        match dock {
            K::Trigger => &mut self.dso_trigger.open,
            K::Protocol => &mut self.protocol.open,
            K::Measure => &mut self.measure.open,
            _ => &mut self.search.open,
        }
    }

    /// The saved docks of a work mode (0 logic, 1 DSO, 2 analog).
    fn mode_docks(&mut self, mode: i32) -> &mut crate::options::DockOptions {
        match mode {
            1 => &mut self.options.frame.dso_dock,
            2 => &mut self.options.frame.analog_dock,
            _ => &mut self.options.frame.logic_dock,
        }
    }

    const DOCKS: [dsview_viewmodel::input::Shortcut; 4] = [
        dsview_viewmodel::input::Shortcut::Trigger,
        dsview_viewmodel::input::Shortcut::Protocol,
        dsview_viewmodel::input::Shortcut::Measure,
        dsview_viewmodel::input::Shortcut::Search,
    ];

    /// `TrigBar::reload` on a mode change: a dock is open when the mode
    /// offers it and it was open when this mode was last left.
    fn load_docks(&mut self) {
        let mode = self.session.mode();
        if self.dock_mode == Some(mode) {
            return;
        }
        self.dock_mode = Some(mode);
        let mut saved = self.mode_docks(mode).clone();
        for d in Self::DOCKS {
            let want = dsview_viewmodel::input::dock_offered(mode, d) && *saved.flag(d);
            *self.dock_flag(d) = want;
        }
    }

    /// Keeps the docks opened or closed in this mode (Qt saves each toggle,
    /// `SaveFrame`); a dock the mode does not offer stays closed and keeps
    /// its saved state.
    fn store_docks(&mut self) {
        let Some(mode) = self.dock_mode else {
            return;
        };
        let mut docks = self.mode_docks(mode).clone();
        for d in Self::DOCKS {
            if dsview_viewmodel::input::dock_offered(mode, d) {
                *docks.flag(d) = *self.dock_flag(d);
            } else {
                *self.dock_flag(d) = false;
            }
        }
        if *self.mode_docks(mode) != docks {
            *self.mode_docks(mode) = docks;
            self.options.save();
        }
    }

    /// Records the window state while it is normal (Qt keeps the normal
    /// rectangle while maximised).
    fn track_window(&mut self, ctx: &egui::Context) {
        let (max, outer, inner, monitor) = ctx.input(|i| {
            let v = i.viewport();
            (v.maximized, v.outer_rect, v.inner_rect, v.monitor_size)
        });
        // once, at start: a window left on a screen that is gone comes back
        if !self.window_checked
            && let (Some(o), Some(m)) = (outer, monitor)
        {
            self.window_checked = true;
            if max == Some(false)
                && let Some((x, y)) = dsview_viewmodel::qsettings::onto_screen(
                    (o.left(), o.top(), o.width(), o.height()),
                    m.x,
                    m.y,
                )
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
                return;
            }
        }
        if let Some(m) = max {
            self.options.frame.is_max = m;
        }
        if max == Some(false)
            && let (Some(o), Some(r)) = (outer, inner)
        {
            self.options.frame.window = Some([
                o.left() as i64,
                o.top() as i64,
                r.width() as i64,
                r.height() as i64,
            ]);
        }
    }

    /// `MainFrame::writeSettings` and the view state Qt keeps in
    /// `appOptions` (trace heights, DSO split).
    pub fn save_settings(&mut self) {
        self.options.trace_height_factor = self.view.height_factor;
        self.options.dso_split_channels = self.view.dso.split;
        self.options.save();
    }

    fn after_save_action(&mut self, a: AfterSave) {
        match a {
            AfterSave::Close => self.close_now = true,
            AfterSave::DefaultDevice => {
                if self.session.is_working() {
                    self.session.stop_capture();
                }
                self.session.set_default_device();
            }
            AfterSave::Device(h) => {
                self.session.set_device(h);
            }
        }
    }

    /// The save started from "Save captured data?" ended (saved, failed or
    /// cancelled: `DSV_MSG_SAVE_COMPLETE`).
    fn save_ended(&mut self) {
        self.session.clear_store_confirm_flag();
        if let Some(a) = self.after_save.take() {
            self.after_save_action(a);
        }
    }

    fn store_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(then) = self.store_confirm else {
            return;
        };
        match crate::msgbox::confirm(ctx, "store", "Save captured data?") {
            Some(true) => {
                self.store_confirm = None;
                if self.session.is_working() {
                    self.session.stop_capture();
                }
                // Qt saves and keeps the window open
                self.after_save = (then != AfterSave::Close).then_some(then);
                self.pick(
                    ctx,
                    "Save File",
                    FileJob::Save,
                    ("DSView data", &["dsl"]),
                    true,
                );
            }
            Some(false) => {
                self.store_confirm = None;
                self.after_save_action(then);
            }
            None => {}
        }
    }

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
            trigger: crate::triggerdock::TriggerDock::default(),
            measure: crate::measuredock::MeasureDock::default(),
            search: crate::searchdock::SearchDock::default(),
            calibration: None,
            store: None,
            screenshot: None,
            ctx_for_shot: false,
            shot_result: None,
            options: crate::options::AppOptions::default(),
            options_edit: None,
            fft: crate::fftview::FftState::default(),
            shared,
            version: version.into(),
            events: rx,
            files: None,
            message: None,
            switch_prompt: None,
            held: None,
            known_devices: 0,
            store_confirm: None,
            after_save: None,
            title: String::new(),
            close_now: false,
            log_options: None,
            about: false,
            keys: Default::default(),
            started: Instant::now(),
            dock_mode: None,
            startup: None,
            window_checked: false,
            bench: None,
            soak: None,
        }
    }

    fn pick(
        &mut self,
        ctx: &egui::Context,
        title: &'static str,
        make: fn(PathBuf) -> FileJob,
        filter: (&'static str, &'static [&'static str]),
        save: bool,
    ) {
        if self.files.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.shared.lock().dialog = Some(title.into());
        let c = ctx.clone();
        // the directory this kind of dialog used last (`UserHistory`)
        let start = history_dir(&self.options.history, &make(PathBuf::new()));
        std::thread::spawn(move || {
            let mut d = rfd::FileDialog::new().set_title(title);
            if let Some(dir) = start {
                d = d.set_directory(dir);
            }
            let d = d.add_filter(filter.0, filter.1);
            let path = match save {
                true => d.save_file(),
                false => d.pick_file(),
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
        if let Some(j) = &job {
            remember_dir(&mut self.options.history, j);
            self.options.save();
        }
        let result = match job {
            None => {
                if self.after_save.is_some() {
                    self.save_ended();
                }
                Ok(())
            }
            Some(FileJob::Open(p)) => self
                .session
                .set_file(&p.to_string_lossy())
                .map_err(|_| format!("Failed to load {}", p.display())),
            Some(FileJob::Save(mut p)) => {
                if p.extension().is_none_or(|e| e != "dsl") {
                    p.set_extension("dsl");
                }
                if self.session.have_view_data() {
                    self.store = Some(crate::storedlg::StoreDialog::new(
                        p,
                        false,
                        self.options.original_data,
                    ));
                    Ok(())
                } else {
                    Err("No data to save.".into())
                }
            }
            Some(FileJob::LoadProfile(p)) => self.session.load_profile(&p),
            Some(FileJob::StoreProfile(mut p)) => {
                if p.extension().is_none_or(|e| e != "dsc") {
                    p.set_extension("dsc");
                }
                self.session.store_profile(&p)
            }
            Some(FileJob::Screenshot(mut p)) => {
                if p.extension().is_none_or(|e| e != "png" && e != "jpeg") {
                    p.set_extension("png");
                }
                // the next frame's picture comes back as an event
                self.screenshot = Some(p);
                self.ctx_for_shot = true;
                Ok(())
            }
            Some(FileJob::Export(p)) => {
                if self.session.have_view_data() {
                    self.store = Some(crate::storedlg::StoreDialog::new(
                        p,
                        true,
                        self.options.original_data,
                    ));
                    Ok(())
                } else {
                    Err("No data to export.".into())
                }
            }
        };
        if let Err(e) = result {
            self.message = Some(e);
        }
    }

    /// `on_screenShot`: asks for the frame; the callback saves it and
    /// reports back.
    fn screenshots(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.ctx_for_shot)
            && let Some(path) = self.screenshot.take()
        {
            let (tx, rx) = channel();
            self.shot_result = Some(rx);
            let c = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                egui::ScreenshotCallback::new(move |img: std::sync::Arc<egui::ColorImage>| {
                    let [w, h] = img.size;
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                    let r = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                        .ok_or_else(|| "bad screenshot".to_string())
                        .and_then(|i| {
                            if path.extension().is_some_and(|e| e == "jpeg") {
                                image::DynamicImage::ImageRgba8(i)
                                    .to_rgb8()
                                    .save(&path)
                                    .map_err(|e| e.to_string())
                            } else {
                                i.save(&path).map_err(|e| e.to_string())
                            }
                        });
                    let _ = tx.send(r);
                    c.request_repaint();
                }),
            ));
        }
        if let Some(rx) = &self.shot_result
            && let Ok(r) = rx.try_recv()
        {
            self.shot_result = None;
            if let Err(e) = r {
                self.message = Some(e);
            }
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
            // DSV_MSG_CURRENT_DEVICE_DETACHED: offer to save its data, then
            // the default device
            self.switch_prompt = None;
            self.session.save_config();
            if self.session.state().saving {
                self.after_save = Some(AfterSave::DefaultDevice);
            } else if !self.confirm_store(AfterSave::DefaultDevice, true)
                && !self.session.is_working()
            {
                self.session.set_default_device();
            }
        } else if list.len() > self.known_devices && self.known_devices > 0 {
            // DSV_MSG_NEW_USB_DEVICE: not while a hardware capture runs; a
            // demo is left without asking
            let current = self.session.state().device.clone();
            let hardware = current.as_ref().is_some_and(|d| d.is_hardware());
            let demo = current.as_ref().is_some_and(|d| d.is_demo());
            if let Some(newest) = list.last().map(|e| (e.handle, e.name.clone())) {
                if demo {
                    self.switch_to_new(newest.0);
                } else if !(hardware && self.session.is_working()) {
                    self.switch_prompt = Some(newest);
                }
            }
        }
        self.known_devices = list.len();
    }

    /// "To switch the new device?" answered yes: offer to save the logic or
    /// analog data first.
    fn switch_to_new(&mut self, handle: u64) {
        let dso = self.session.mode() == dsview_core::Mode::Dso as i32;
        if self.session.state().saving {
            self.after_save = Some(AfterSave::Device(handle));
        } else if dso || !self.confirm_store(AfterSave::Device(handle), true) {
            if self.session.is_working() {
                self.session.stop_capture();
            }
            self.session.set_device(handle);
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
        self.frame(ui);
    }
}

impl App {
    /// One frame of the window (eframe's `App::ui`, without the frame
    /// handle it does not use).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
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
        if data && self.session.is_realtime_refresh() && self.options.auto_scroll_latest_data {
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
        // a failed switch (`SigSession::set_device`'s messages)
        let err = std::mem::take(&mut self.session.state().device_error);
        if let Some(t) = dsview_session::session::device_error_text(err) {
            self.message = Some(t.into());
        }
        // the docks of this work mode, as last left (`TrigBar::reload`)
        self.load_docks();
        // `MainWindow::eventFilter`: the shortcuts, outside text fields
        if !ctx.egui_wants_keyboard_input() {
            let keys: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => Some((*key, *modifiers)),
                        _ => None,
                    })
                    .collect()
            });
            for (k, m) in keys {
                let now = self.started.elapsed().as_millis() as u64;
                if !self
                    .keys
                    .accept(now, m.ctrl || m.command || m.shift || m.alt)
                {
                    continue;
                }
                if let Some(sc) = dsview_viewmodel::input::shortcut(k.name()) {
                    self.shortcut(sc);
                }
            }
        }
        self.file_jobs();
        self.screenshots(&ctx);
        self.control_ops();

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(tr("File"), |ui| {
                    if crate::icons::button(ui, "open", tr("Open…")).clicked() {
                        self.pick(
                            &ctx,
                            "Open File",
                            FileJob::Open,
                            ("DSView data", &["dsl", "demo"]),
                            false,
                        );
                        ui.close();
                    }
                    if crate::icons::button(ui, "save", tr("Save…")).clicked() {
                        self.pick(
                            &ctx,
                            "Save File",
                            FileJob::Save,
                            ("DSView data", &["dsl"]),
                            true,
                        );
                        ui.close();
                    }
                    if crate::icons::button(ui, "export", tr("Export…")).clicked() {
                        // `getSuportedExportFormats`: srzip holds logic data only
                        let formats: (&str, &[&str]) =
                            if self.session.mode() == dsview_core::Mode::Logic as i32 {
                                ("CSV, VCD or srzip", &["csv", "vcd", "srzip"])
                            } else {
                                ("CSV or VCD", &["csv", "vcd"])
                            };
                        self.pick(&ctx, "Export Data", FileJob::Export, formats, true);
                        ui.close();
                    }
                    ui.separator();
                    // the profile menu (FileBar's session menu)
                    if crate::icons::button(ui, "file", tr("Load Session…")).clicked() {
                        self.pick(
                            &ctx,
                            "Open Session",
                            FileJob::LoadProfile,
                            ("DSView Session", &["dsc"]),
                            false,
                        );
                        ui.close();
                    }
                    if crate::icons::button(ui, "file", tr("Save Session…")).clicked() {
                        self.pick(
                            &ctx,
                            "Save Session",
                            FileJob::StoreProfile,
                            ("DSView Session", &["dsc"]),
                            true,
                        );
                        ui.close();
                    }
                    if ui.button(tr("Load Default Session")).clicked() {
                        match self.session.default_profile_path() {
                            Some(p) => {
                                if let Err(e) = self.session.load_profile(&p) {
                                    self.message = Some(e);
                                }
                            }
                            None => self.message = Some("No device.".into()),
                        }
                        ui.close();
                    }
                    ui.separator();
                    if crate::icons::button(ui, "capture", tr("Capture…")).clicked() {
                        self.pick(
                            &ctx,
                            "Save As",
                            FileJob::Screenshot,
                            ("png file", &["png", "jpeg"]),
                            true,
                        );
                        ui.close();
                    }
                });
                // Qt's display menu: the themes and the application options
                ui.menu_button(tr("Display"), |ui| {
                    ui.menu_button(tr("Themes"), |ui| {
                        let now = crate::theme::Theme::from_style(&self.options.frame.style);
                        for t in crate::theme::Theme::ALL {
                            if ui.radio(now == t, tr(t.label())).clicked() {
                                self.options.frame.style = t.style().to_owned();
                                self.options.save();
                                self.options.apply(&mut self.view, ui.ctx());
                                ui.close();
                            }
                        }
                    });
                    if crate::icons::button(ui, "gear", tr("Options…")).clicked() {
                        self.options_edit = Some(self.options.clone());
                        ui.close();
                    }
                });
                ui.menu_button(tr("View"), |ui| {
                    // only the docks this mode offers (`TrigBar::reload`)
                    use dsview_viewmodel::input::{Shortcut as K, dock_offered};
                    let mode = self.session.mode();
                    for (d, label) in [
                        (K::Trigger, ("Trigger", 'T')),
                        (K::Protocol, ("Decode", 'D')),
                        (K::Measure, ("Measure", 'M')),
                        (K::Search, ("Search", 'R')),
                    ] {
                        if dock_offered(mode, d) {
                            // Qt's trig bar buttons, with their keys
                            let text = format!("{} ({})", tr(label.0), label.1);
                            ui.checkbox(self.dock_flag(d), text);
                        }
                    }
                    ui.checkbox(&mut self.view.dso.split, tr("Split DSO channels"));
                    ui.horizontal(|ui| {
                        let l = ui.label(tr("DSO persistence"));
                        ui.add(
                            egui::DragValue::new(&mut self.view.dso.persist.ms)
                                .range(0..=10000)
                                .suffix(" ms"),
                        )
                        .labelled_by(l.id);
                    });
                });
                let dso = self.session.mode() == dsview_core::Mode::Dso as i32;
                ui.add_enabled_ui(dso, |ui| {
                    ui.menu_button(tr("Function"), |ui| {
                        if crate::icons::button(ui, "fft", tr("FFT…")).clicked() {
                            self.fft.open_dialog();
                            ui.close();
                        }
                        if crate::icons::button(ui, "math", tr("Math…")).clicked() {
                            self.view.dso.math.open_dialog();
                            ui.close();
                        }
                        if crate::icons::button(ui, "lissajous", tr("Lissajous…")).clicked() {
                            self.view.dso.lissajous.open_dialog(&self.session);
                            ui.close();
                        }
                        if ui.button(tr("Reference…")).clicked() {
                            self.view.dso.refs.dialog = Some(0);
                            ui.close();
                        }
                        if ui.button(tr("Histogram…")).clicked() {
                            self.view.dso.analysis.histogram = Some((0, false));
                            ui.close();
                        }
                        if ui.button(tr("Channel measure…")).clicked() {
                            self.view.dso.analysis.chan_measure = Some((0, 1));
                            ui.close();
                        }
                    });
                });
                // Qt's logo bar
                ui.menu_button(tr("Help"), |ui| {
                    // `switchLanguage`: each language named in itself
                    ui.menu_button(tr("Language"), |ui| {
                        use dsview_viewmodel::qsettings::{LAN_CN, LAN_DE, LAN_EN};
                        for (lang, name) in
                            [(LAN_EN, "English"), (LAN_CN, "中文"), (LAN_DE, "Deutsch")]
                        {
                            let on = self.options.frame.language == lang;
                            if ui.radio(on, name).clicked() {
                                self.set_language(lang);
                                ui.close();
                            }
                        }
                    });
                    if crate::icons::button(ui, "about", tr("About…")).clicked() {
                        self.about = true;
                        ui.close();
                    }
                    if crate::icons::button(ui, "manual", tr("Manual…")).clicked() {
                        self.open_manual();
                        ui.close();
                    }
                    if crate::icons::button(ui, "bug", tr("Bug Report")).clicked() {
                        crate::help::open(crate::help::BUG_REPORT_URL);
                        ui.close();
                    }
                    if crate::icons::button(ui, "update", tr("Update")).clicked() {
                        crate::help::open(crate::help::UPDATE_URL);
                        ui.close();
                    }
                    if crate::icons::button(ui, "log", tr("Log Options")).clicked() {
                        self.log_options = Some(crate::log::LogOptions::new(&self.options));
                        ui.close();
                    }
                });
                ui.separator();
                let dev = self.session.state().device.as_ref().map(|d| d.name.clone());
                ui.label(dev.unwrap_or_else(|| "no device".into()));
            });
        });
        egui::Panel::top("sampling").show(ui, |ui| {
            self.bar.show(ui, &self.session);
        });
        // `SamplingBar::on_device_selected`: stop, offer to save (not with
        // "don't ask to save", which Qt ignored here, F29), switch
        if let Some(h) = self.bar.switch_to.take() {
            self.session.stop_capture();
            if !self.confirm_store(AfterSave::Device(h), true) {
                self.session.set_device(h);
            }
        }
        // `update_title_bar_text`: the version, and the hardware profile if
        // the option says so
        let mut title = format!("DSViewR v{}", self.version);
        if self.options.display_profile_in_bar
            && let Some(f) = self.session.state().profile_file.clone()
        {
            title += &format!(" [{}]", f.display());
        }
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        // the normal window's place and size (`saveNormalRegion`)
        self.track_window(&ctx);
        // `MainFrame::closeEvent` / `able_to_close`; `writeSettings`
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.confirm_store(AfterSave::Close, true) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if self.bench.is_none() && self.soak.is_none() {
                self.save_settings();
            }
        }
        self.store_confirm_window(&ctx);
        if let Some(mut d) = self.log_options.take()
            && d.show(&ctx, &mut self.options).is_none()
        {
            self.log_options = Some(d);
        }
        // "No" to the store question while closing
        if std::mem::take(&mut self.close_now) {
            if self.bench.is_none() && self.soak.is_none() {
                self.save_settings();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let dso_mode = self.session.mode() == dsview_core::Mode::Dso as i32;
        let logic_mode = self.session.mode() == dsview_core::Mode::Logic as i32;
        if logic_mode && self.search.open {
            egui::Panel::bottom("search").show(ui, |ui| {
                self.search.show(ui, &self.session, &mut self.view);
            });
        } else if !self.search.open {
            self.view.search = None;
        }
        self.search.poll(&ctx, &self.session, &mut self.view);
        if let Some(m) = self.search.message.take() {
            self.message = Some(m);
        }
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
                    self.protocol.cursors = self.view.cursors.samples.clone();
                    self.protocol.formats = self.options.protocol_formats.clone();
                    self.protocol.show(ui, &self.session);
                    if std::mem::take(&mut self.protocol.formats_changed) {
                        self.options.protocol_formats = self.protocol.formats.clone();
                        self.options.save();
                    }
                });
        }
        let dso = self.session.mode() == dsview_core::Mode::Dso as i32;
        if self.measure.open {
            egui::Panel::right("measure")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.measure.show(ui, &self.session, &mut self.view);
                    });
                });
        }
        let logic = self.session.mode() == dsview_core::Mode::Logic as i32;
        if logic && self.dso_trigger.open {
            egui::Panel::right("trigger")
                .resizable(true)
                .default_size(330.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.trigger.show(ui, &self.session);
                    });
                });
        }
        if let Some(m) = self.bar.message.take() {
            self.message = Some(m);
        }
        if let Some(m) = self.trigger.message.take() {
            self.message = Some(m);
        }
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
        self.view.map_zoom_fixed = self.protocol.map_zoom_fixed;
        if let Some(t) = self.protocol.translate_changed.take() {
            self.options.trans_decoder_dlg = t;
            self.options.save();
        }
        self.protocol.translate = self.options.trans_decoder_dlg;
        self.fft.palette = self.view.palette;
        // the protocol export's directory (`protocolExportPath`)
        self.protocol.export_dir = Some(self.options.history.protocol_export_path.clone());
        if let Some(p) = self.protocol.exported_to.take()
            && let Some(d) = p.parent()
        {
            self.options.history.protocol_export_path = d.to_string_lossy().into_owned();
            self.options.save();
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
            } else if self.search.busy() || self.store.as_ref().is_some_and(|d| d.busy()) {
                // the search holds the logic store; the view waits for it
                ui.centered_and_justified(|ui| ui.label(tr("Searching…")));
            } else {
                self.view.show(ui, &self.session);
            }
        });
        // check_calibration(): DSO hardware in calibration mode
        if std::mem::take(&mut self.bar.calibrate)
            && self.session.mode() == dsview_core::Mode::Dso as i32
            && self
                .session
                .config(dsview_core::key::CALI, None)
                .is_some_and(|v| v.get_bool())
        {
            self.calibration = Some(crate::calibration::Calibration::open(&self.session));
        }
        if let Some(c) = &mut self.calibration {
            c.show(&ctx, &self.session);
            if !c.open {
                self.calibration = None;
            }
        }
        if let Some(d) = &mut self.store {
            let cursors = self.view.cursors.samples.clone();
            let out = d.show(&ctx, &self.session, &cursors, &self.version);
            if let Some(m) = d.message.take() {
                self.message = Some(m);
            }
            if d.busy() && d.original() != self.options.original_data {
                self.options.original_data = d.original();
                self.options.save();
            }
            if matches!(out, crate::storedlg::Outcome::Done) {
                let saved = self.store.take().is_some_and(|d| !d.is_export());
                if saved {
                    self.save_ended();
                }
            }
        }
        if let Some(mut o) = self.options_edit.take() {
            let mut done = None;
            egui::Window::new(tr("Application Options"))
                .collapsible(false)
                .show(&ctx, |ui| done = o.dialog(ui));
            match done {
                Some(true) => {
                    o.apply(&mut self.view, &ctx);
                    self.session.state().swap_back_buffer_always = o.swap_back_buffer_always;
                    o.save();
                    self.options = o;
                }
                Some(false) => {}
                None => self.options_edit = Some(o),
            }
        }
        self.fft.dialog(&ctx, &self.session);
        let names: Vec<(u16, String)> = self
            .session
            .state()
            .signals
            .iter()
            .filter(|x| x.ty == dsview_core::channel_type::DSO)
            .map(|x| (x.index, x.name.clone()))
            .collect();
        self.view.dso.math.dialog(&ctx, &names);
        self.view.dso.lissajous.dialog(&ctx, &self.session, &names);
        // DSV_MSG_NEW_USB_DEVICE's question
        if let Some((handle, _)) = self.switch_prompt.clone() {
            match crate::msgbox::confirm(&ctx, "new-device", "To switch the new device?") {
                Some(true) => {
                    self.switch_prompt = None;
                    self.switch_to_new(handle);
                }
                Some(false) => self.switch_prompt = None,
                None => {}
            }
        }
        // the held decoder options, one decoder at a time
        if self.held.is_none() {
            let mut st = self.session.state();
            if !st.held_decoders.is_empty() {
                self.held = Some(st.held_decoders.remove(0));
            }
        }
        if let Some(h) = self.held.clone() {
            let text = format!(
                "The file sets decoder options that run a program or read a file:\n{}\nUse them?",
                h.settings.join("\n")
            );
            if let Some(yes) = crate::msgbox::confirm(&ctx, "held-options", &text) {
                if yes {
                    self.session.apply_held_decoder(&h);
                }
                self.held = None;
            }
        }
        if let Some(msg) = self.message.clone()
            && crate::msgbox::show(&ctx, "message", &msg)
        {
            self.message = None;
        }
        if self.about && !crate::help::about(&ctx, &self.version) {
            self.about = false;
        }
        if let Some(mut st) = self.startup.take() {
            if let Some(crate::startup::Action::OpenManual) = st.show(&ctx, &mut self.options) {
                self.open_manual();
            }
            if !st.done() {
                self.startup = Some(st);
            }
        }

        self.store_docks();
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
