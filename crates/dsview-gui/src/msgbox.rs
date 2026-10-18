//! Message and confirmation windows (`pv/ui/msgbox.cpp`, `DsMessageBox`):
//! `MsgBox::Show` is a "Message" window with OK, `MsgBox::Confirm` a
//! "Confirm" window with Yes and No. Both stay in the middle of the window
//! and never block the frame.

use eframe::egui;

fn frame(ctx: &egui::Context, id: &str, title: &str, text: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Window::new(title)
        .id(egui::Id::NULL.with(("msgbox", id)))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(text);
            ui.add_space(6.0);
            ui.horizontal(add);
        });
}

/// `MsgBox::Show`: true once OK (or Enter/Esc) closed it.
pub fn show(ctx: &egui::Context, id: &str, text: &str) -> bool {
    let mut done = false;
    frame(ctx, id, "Message", text, |ui| {
        done = ui.button("OK").clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape));
    });
    done
}

/// `MsgBox::Confirm`: Some(true) for Yes, Some(false) for No or Esc.
pub fn confirm(ctx: &egui::Context, id: &str, text: &str) -> Option<bool> {
    let mut answer = None;
    frame(ctx, id, "Confirm", text, |ui| {
        if ui.button("Yes").clicked() {
            answer = Some(true);
        }
        if ui.button("No").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(false);
        }
    });
    answer
}
