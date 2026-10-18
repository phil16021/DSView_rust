//! The log (`pv/log.cpp` over `common/log/xlog.c`): messages at or below
//! the level go to stderr and, once enabled, to `~/DSView.log` as
//! `DSView: <message>` lines. The Log Options dialog
//! (`LogoBar::on_action_setting_log`) sets level, file and append mode.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use eframe::egui;

/// `XLOG_LEVEL_*`: none, error, warning, info, debug, detail.
pub const LEVEL_DBG: i32 = 4;

struct LogState {
    level: AtomicI32,
    /// The log file once enabled (`b_logfile`), and whether it receives
    /// (`xlog_set_receiver_enable`).
    file: Mutex<Option<(File, bool)>>,
    path: Mutex<PathBuf>,
}

fn state() -> &'static LogState {
    static S: OnceLock<LogState> = OnceLock::new();
    S.get_or_init(|| LogState {
        level: AtomicI32::new(3),
        file: Mutex::new(None),
        path: Mutex::new(default_path()),
    })
}

/// `get_dsv_log_path`: `~/DSView.log` on Linux.
fn default_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("DSView.log")
}

pub fn path() -> PathBuf {
    state()
        .path
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Another log file (tests); an open one is closed.
pub fn set_path(p: PathBuf) {
    let s = state();
    *s.path.lock().unwrap_or_else(|e| e.into_inner()) = p;
    *s.file.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Installs the sink behind `dsview_drivers::sys::log`.
pub fn init(level: i32) {
    set_level(level);
    dsview_drivers::sys::set_log_sink(Box::new(|l, m| {
        let s = state();
        if l > s.level.load(Ordering::Relaxed) {
            return;
        }
        eprintln!("{m}");
        if let Some((f, true)) = s.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = writeln!(f, "DSView: {m}");
            let _ = f.flush();
        }
    }));
}

pub fn level() -> i32 {
    state().level.load(Ordering::Relaxed)
}

/// `dsv_log_level`: clamped to 0-5, as `xlog_set_level` refuses others.
pub fn set_level(l: i32) {
    if (0..=5).contains(&l) {
        state().level.store(l, Ordering::Relaxed);
        dsview_drivers::sys::log(3, &format!("Set log level: {l}"));
    }
}

/// `dsv_log_enalbe_logfile`: opens the file once (appending or
/// truncating); later calls do nothing.
pub fn enable_file(append: bool) {
    let s = state();
    let mut f = s.file.lock().unwrap_or_else(|e| e.into_inner());
    if f.is_some() {
        return;
    }
    let mut o = std::fs::OpenOptions::new();
    o.create(true).read(true);
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    let p = path();
    match o.open(&p) {
        Ok(file) => *f = Some((file, true)),
        Err(_) => {
            drop(f);
            dsview_drivers::sys::log(1, "Create log file error!");
            return;
        }
    }
    drop(f);
    dsview_drivers::sys::log(3, &format!("Store log to file: \"{}\"", p.display()));
}

/// `dsv_set_log_file_enable`
pub fn set_file_enabled(on: bool) {
    if let Some((_, e)) = state()
        .file
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        *e = on;
    }
}

/// `dsv_clear_log_file`: an open log file starts over; otherwise the file
/// is deleted.
pub fn clear() {
    let s = state();
    let mut f = s.file.lock().unwrap_or_else(|e| e.into_inner());
    match f.as_mut() {
        Some((file, _)) => {
            let _ = file.set_len(0);
            // appended writes go to the (new) end; truncated files keep
            // their position otherwise
            let _ = std::io::Seek::rewind(file);
        }
        None => {
            let _ = std::fs::remove_file(path());
        }
    }
}

/// The command line and the settings together (`main.cpp`): `-s` raises
/// the level to debug and logs to the file (appending); a saved "save to
/// file" opens it in the saved mode and its level wins when higher.
pub fn start(cli_level: i32, store_log: bool, o: &crate::options::AppOptions) {
    let mut level = cli_level;
    if store_log && level < LEVEL_DBG {
        level = LEVEL_DBG;
    }
    init(level);
    if store_log {
        enable_file(true);
    }
    if o.able_save_log {
        enable_file(o.append_log_mode);
        if o.log_level >= level {
            set_level(o.log_level);
        }
    }
}

/// The Log Options dialog's state.
pub struct LogOptions {
    level: i32,
    save: bool,
    append: bool,
    confirm_clear: bool,
    pub message: Option<String>,
}

impl LogOptions {
    pub fn new(o: &crate::options::AppOptions) -> LogOptions {
        LogOptions {
            level: o.log_level.clamp(0, 5),
            save: o.able_save_log,
            append: o.append_log_mode,
            confirm_clear: false,
            message: None,
        }
    }

    /// The dialog; Some(true) for OK (the options are then updated and
    /// applied), Some(false) for Cancel.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        o: &mut crate::options::AppOptions,
    ) -> Option<bool> {
        let mut done = None;
        let exists = path().is_file();
        egui::Window::new("Log Options")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("log-opts").num_columns(2).show(ui, |ui| {
                    let l = ui.label("Log Level");
                    egui::ComboBox::from_id_salt("log-level")
                        .selected_text(self.level.to_string())
                        .show_ui(ui, |ui| {
                            for i in 0..=5 {
                                ui.selectable_value(&mut self.level, i, i.to_string());
                            }
                        })
                        .response
                        .labelled_by(l.id);
                    ui.end_row();
                    let l = ui.label("Save To File");
                    ui.checkbox(&mut self.save, "").labelled_by(l.id);
                    ui.end_row();
                    let l = ui.label("Append mode");
                    ui.checkbox(&mut self.append, "").labelled_by(l.id);
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    if ui.add_enabled(exists, egui::Button::new("Open")).clicked() {
                        let _ = open::that_detached(path());
                    }
                    if ui.add_enabled(exists, egui::Button::new("Clear")).clicked() {
                        self.confirm_clear = true;
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        done = Some(false);
                    }
                });
            });
        if self.confirm_clear {
            egui::Window::new("Clear log")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Are you sure to clear log file?");
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            clear();
                            self.confirm_clear = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_clear = false;
                        }
                    });
                });
        }
        if done == Some(true)
            && (self.save, self.level, self.append)
                != (o.able_save_log, o.log_level, o.append_log_mode)
        {
            o.able_save_log = self.save;
            o.log_level = self.level;
            o.append_log_mode = self.append;
            o.save();
            set_level(self.level);
            // Qt opens a newly enabled file in overwrite mode here,
            // whatever "Append mode" says (F30)
            if self.save {
                enable_file(false);
            }
            set_file_enabled(self.save);
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test: the log state is global.
    #[test]
    fn file_levels_and_clear() {
        let dir = std::env::temp_dir().join(format!("dsv-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("DSView.log");
        std::fs::write(&p, "old\n").unwrap();
        set_path(p.clone());
        let read = || std::fs::read_to_string(&p).unwrap_or_default();
        // -s: debug level, appended to the old file
        let mut o = crate::options::AppOptions::default();
        start(3, true, &o);
        assert_eq!(level(), LEVEL_DBG);
        dsview_drivers::sys::log(4, "dbg line");
        dsview_drivers::sys::log(5, "detail line");
        let t = read();
        assert!(t.starts_with("old\n"), "{t}");
        assert!(t.contains("DSView: dbg line"));
        assert!(!t.contains("detail line"));
        // the file switched off keeps what it has
        set_file_enabled(false);
        dsview_drivers::sys::log(1, "not written");
        assert!(!read().contains("not written"));
        set_file_enabled(true);
        // clear: an open file starts over
        clear();
        dsview_drivers::sys::log(1, "after clear");
        assert_eq!(read(), "DSView: after clear\n");
        // saved options: overwrite mode, a higher saved level wins
        set_path(p.clone());
        o.able_save_log = true;
        o.append_log_mode = false;
        o.log_level = 5;
        start(3, false, &o);
        assert_eq!(level(), 5);
        let t = read();
        assert!(!t.contains("after clear"), "overwritten: {t}");
        assert!(t.contains("DSView: Set log level: 5"), "{t}");
        // a lower saved level does not lower the command line's
        o.log_level = 1;
        set_path(p.clone());
        start(4, false, &o);
        assert_eq!(level(), 4);
        // out of range: unchanged
        set_level(9);
        assert_eq!(level(), 4);
        // without an open file, clear deletes it
        set_path(p.clone());
        clear();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
