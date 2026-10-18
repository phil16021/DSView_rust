//! The dialogs at start-up (`MainFrame`): the hardware driver note when no
//! udev rule grants USB access (`show_driver_hint_once`), then the document
//! hint with the user guide (`show_doc`). "Not Show Again" turns either off
//! for good (`UserHistory`).

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::options::AppOptions;

/// `showDoc<language>.png` from Qt's resources (`DSView.qrc`).
const SHOW_DOC_CN: &[u8] = include_bytes!("../../../DSView/icons/showDoc25.png");
const SHOW_DOC_EN: &[u8] = include_bytes!("../../../DSView/icons/showDoc31.png");

/// Where a package puts the udev rule (`rule_paths`).
pub const RULE_PATHS: [&str; 3] = [
    "/usr/lib/udev/rules.d/60-dreamsourcelab.rules",
    "/lib/udev/rules.d/60-dreamsourcelab.rules",
    "/etc/udev/rules.d/60-dreamsourcelab.rules",
];

/// `show_driver_hint_once`'s text, or None when a rule is installed. With a
/// copy of the rule in the data directory (packages, AppImage) the hint
/// gives the command that installs it.
pub fn driver_hint(installed: impl Fn(&Path) -> bool, bundled: Option<&Path>) -> Option<String> {
    if RULE_PATHS.iter().any(|p| installed(Path::new(p))) {
        return None;
    }
    Some(match bundled {
        Some(b) => format!(
            "DreamSourceLab hardware needs a udev rule granting USB access, which was not \
             found on this system (this is normal when running the AppImage). To install \
             it, run:\n\nsudo cp \"{}\" /etc/udev/rules.d/ && sudo udevadm control \
             --reload-rules",
            b.display()
        ),
        None => "DreamSourceLab hardware needs a udev rule granting USB access, which was \
                 not found on this system. Create /etc/udev/rules.d/60-dreamsourcelab.rules \
                 with the following line, then run 'sudo udevadm control --reload-rules':\n\n\
                 SUBSYSTEM==\"usb\", ATTRS{idVendor}==\"2a0e\", MODE=\"0660\", \
                 TAG+=\"uaccess\""
            .to_owned(),
    })
}

enum Stage {
    DriverHint(String),
    Document,
    Done,
}

/// The start-up dialogs, one after the other.
pub struct Startup {
    stage: Stage,
    image: Option<egui::TextureHandle>,
    data_dir: PathBuf,
}

/// What the document hint asks for.
pub enum Action {
    OpenManual,
}

impl Startup {
    pub fn new(o: &AppOptions, data_dir: &Path) -> Startup {
        let bundled = data_dir.join("DreamSourceLab.rules");
        let hint = o
            .history
            .show_driver_hint
            .then(|| driver_hint(|p| p.exists(), bundled.is_file().then_some(&*bundled)))
            .flatten();
        Startup {
            stage: match hint {
                Some(t) => Stage::DriverHint(t),
                None => Stage::Document,
            },
            image: None,
            data_dir: data_dir.to_owned(),
        }
    }

    pub fn done(&self) -> bool {
        matches!(self.stage, Stage::Done)
    }

    /// The current dialog; the options record "Not Show Again".
    pub fn show(&mut self, ctx: &egui::Context, o: &mut AppOptions) -> Option<Action> {
        match &self.stage {
            Stage::DriverHint(text) => {
                let text = text.clone();
                if let Some(not_again) = buttons_window(ctx, "Hardware driver note", |ui| {
                    ui.label(text);
                }) {
                    if not_again {
                        o.history.show_driver_hint = false;
                        o.save();
                    }
                    self.stage = Stage::Document;
                }
                None
            }
            Stage::Document if !o.history.show_documents => {
                self.stage = Stage::Done;
                None
            }
            Stage::Document => {
                let tex = self
                    .image
                    .get_or_insert_with(|| load_doc_image(ctx, &self.data_dir, o.frame.language));
                let size = tex.size_vec2();
                let id = tex.id();
                let mut action = None;
                egui::Window::new("Document")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.image((id, size));
                        ui.horizontal(|ui| {
                            if ui.button("Not Show Again").clicked() {
                                o.history.show_documents = false;
                                o.save();
                                self.stage = Stage::Done;
                            }
                            if ui.button("Ignore").clicked() || crate::msgbox::esc(ctx) {
                                self.stage = Stage::Done;
                            }
                            if ui.button("Open").clicked() {
                                action = Some(Action::OpenManual);
                                self.stage = Stage::Done;
                            }
                        });
                    });
                action
            }
            Stage::Done => None,
        }
    }
}

/// A note with "Not Show Again" and "Ignore": Some(true) for the first,
/// Some(false) for the second (or Esc).
fn buttons_window(
    ctx: &egui::Context,
    title: &str,
    body: impl FnOnce(&mut egui::Ui),
) -> Option<bool> {
    let mut answer = None;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            body(ui);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Not Show Again").clicked() {
                    answer = Some(true);
                }
                if ui.button("Ignore").clicked() || crate::msgbox::esc(ctx) {
                    answer = Some(false);
                }
            });
        });
    answer
}

/// `showDoc<language>.png` from the data directory, else the built-in one
/// (Chinese or English; German shows the English one).
fn load_doc_image(ctx: &egui::Context, data_dir: &Path, language: i64) -> egui::TextureHandle {
    let file = std::fs::read(data_dir.join(format!("showDoc{language}.png"))).ok();
    let bytes: &[u8] = match &file {
        Some(b) => b,
        None if language == 25 => SHOW_DOC_CN,
        None => SHOW_DOC_EN,
    };
    let img = image::load_from_memory(bytes)
        .map(|i| i.to_rgba8())
        .unwrap_or_else(|_| image::RgbaImage::new(1, 1));
    let (w, h) = img.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    ctx.load_texture("showDoc", color, egui::TextureOptions::LINEAR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_hint_texts() {
        assert_eq!(driver_hint(|p| p.starts_with("/etc"), None), None);
        let plain = driver_hint(|_| false, None).unwrap();
        assert!(plain.contains("SUBSYSTEM==\"usb\", ATTRS{idVendor}==\"2a0e\""));
        let b = PathBuf::from("/opt/dsview/share/DSView/DreamSourceLab.rules");
        let bundled = driver_hint(|_| false, Some(&b)).unwrap();
        assert!(bundled.contains("sudo cp \"/opt/dsview/share/DSView/DreamSourceLab.rules\""));
    }
}
