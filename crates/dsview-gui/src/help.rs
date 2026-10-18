//! The Help menu's actions (`pv/toolbars/logobar.cpp`): About
//! (`pv/dialogs/about.cpp`), the user guide, the bug tracker and the
//! release page.

use crate::i18n::tr;
use std::path::{Path, PathBuf};

use eframe::egui;

/// `LogoBar::on_action_issue`, `on_action_update`.
pub const BUG_REPORT_URL: &str = "https://github.com/DreamSourceLab/DSView/issues";
pub const UPDATE_URL: &str = "https://github.com/Schildkroet/DSView/releases";
const WEBSITE: &str = "https://www.DreamSourceLab.com";
const FORK_URL: &str = "https://github.com/Schildkroet/DSView";
const KICKSTARTER_URL: &str = "https://www.kickstarter.com/projects/dreamsourcelab/dslogic-multifunction-instruments-for-everyone";
const SIGROK_URL: &str = "http://sigrok.org/";

/// `MainWindow::openDoc`: `ug<language>.pdf` in the data directory. Only
/// the Chinese (25) and English (31) guides exist, so another language
/// gets the English one (Qt opens a missing file, F30).
pub fn manual_path(data_dir: &Path, language: i64) -> Option<PathBuf> {
    [language, 31]
        .iter()
        .map(|l| data_dir.join(format!("ug{l}.pdf")))
        .find(|p| p.is_file())
}

/// Opens a file or URL with the desktop's handler (`QDesktopServices`).
pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    if let Err(e) = open::that_detached(target.as_ref()) {
        dsview_drivers::sys::log(1, &format!("Open {:?}: {e}", target.as_ref()));
    }
}

/// A link opened with [`open`] (egui's hyperlinks need eframe's `links`
/// feature, which is not built).
fn link(ui: &mut egui::Ui, text: &str, url: &str) {
    if ui.link(text).on_hover_text(url).clicked() {
        open(url);
    }
}

/// The CPU architecture as `about.cpp` names it.
fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "x86",
        "arm" => "arm",
        "aarch64" => "arm64",
        _ => "other",
    }
}

/// The About window; false once closed.
pub fn about(ctx: &egui::Context, version: &str) -> bool {
    let mut open = true;
    egui::Window::new(tr("About"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("DSView {version} ({})", arch())).size(24.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(tr("Website:"));
                    link(ui, WEBSITE, WEBSITE);
                });
                ui.label(tr("Copyright: © DreamSourceLab. All rights reserved."));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(tr("Fork by Schildkroet:"));
                    link(ui, FORK_URL, FORK_URL);
                });
                ui.add_space(16.0);
                ui.label(egui::RichText::new("Special Thanks").size(16.0));
                link(ui, "All backers on kickstarter", KICKSTARTER_URL);
                link(ui, "All members of Sigrok project", SIGROK_URL);
                ui.label(tr("All contributors of all open-source projects"));
                ui.add_space(8.0);
            });
        });
    open && !ctx.input(|i| i.key_pressed(egui::Key::Escape))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_falls_back_to_english() {
        let dir = std::env::temp_dir().join(format!("dsv-ug-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(manual_path(&dir, 25), None);
        std::fs::write(dir.join("ug31.pdf"), b"%PDF").unwrap();
        std::fs::write(dir.join("ug25.pdf"), b"%PDF").unwrap();
        assert_eq!(manual_path(&dir, 25), Some(dir.join("ug25.pdf")));
        assert_eq!(manual_path(&dir, 7), Some(dir.join("ug31.pdf")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
