//! The four themes (`MainWindow::switchTheme`, the `.qss` files): egui
//! visuals for the widgets, and the view colours (`AppConfig::GetStyleColor`
//! and the palettes of `dark.qss`, `light.qss`, `latte.qss`, `frappe.qss`).

use eframe::egui::{self, Color32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    Latte,
    Frappe,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::Dark, Theme::Light, Theme::Latte, Theme::Frappe];

    /// The settings' `style` value.
    pub fn from_style(s: &str) -> Theme {
        match s {
            "light" => Theme::Light,
            "latte" => Theme::Latte,
            "frappe" => Theme::Frappe,
            _ => Theme::Dark,
        }
    }

    pub fn style(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Latte => "latte",
            Theme::Frappe => "frappe",
        }
    }

    /// The menu entry (`IDS_TOOLBAR_DISPLAY_THEMES_*`).
    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
            Theme::Latte => "Latte",
            Theme::Frappe => "Frappé",
        }
    }

    /// `AppConfig::IsDarkStyle`: dark and frappé, which also take the
    /// light-on-dark icons.
    pub fn is_dark(self) -> bool {
        matches!(self, Theme::Dark | Theme::Frappe)
    }

    pub fn palette(self) -> Palette {
        let rgb = |v: u32| Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
        match self {
            // the Rust GUI's dark colours since M6a (Qt's trace area is
            // rgb(38,38,38); DEVIATIONS)
            Theme::Dark => Palette {
                bg: Color32::from_gray(24),
                header: Color32::from_gray(34),
                ruler: Color32::from_gray(30),
                deep: Color32::from_gray(12),
                text: Color32::from_gray(220),
                text2: Color32::from_gray(200),
                tick: Color32::from_gray(170),
                dim: Color32::from_gray(140),
                muted: Color32::from_gray(160),
                faint: Color32::from_gray(90),
                divider: Color32::from_gray(48),
                accent: rgb(0x1185D1),
            },
            Theme::Light => Palette {
                bg: rgb(0xF8F8F8),
                header: rgb(0xEFEFEF),
                ruler: rgb(0xF2F2F2),
                deep: rgb(0xFFFFFF),
                text: rgb(0x2A2A2A),
                text2: rgb(0x3A3A3A),
                tick: rgb(0x606060),
                dim: rgb(0x808080),
                muted: rgb(0x707070),
                faint: rgb(0xB4B4B4),
                divider: rgb(0xD0D0D0),
                accent: rgb(0x1185D1),
            },
            Theme::Latte => Palette {
                bg: rgb(0xEFF1F5),
                header: rgb(0xE6E9EF),
                ruler: rgb(0xE6E9EF),
                deep: rgb(0xDCE0E8),
                text: rgb(0x4C4F69),
                text2: rgb(0x5C5F77),
                tick: rgb(0x6C6F85),
                dim: rgb(0x8C8FA1),
                muted: rgb(0x7C7F93),
                faint: rgb(0xACB0BE),
                divider: rgb(0xCCD0DA),
                accent: rgb(0x1E66F5),
            },
            Theme::Frappe => Palette {
                bg: rgb(0x303446),
                header: rgb(0x292C3C),
                ruler: rgb(0x292C3C),
                deep: rgb(0x232634),
                text: rgb(0xC6D0F5),
                text2: rgb(0xB5BFE2),
                tick: rgb(0xA5ADCE),
                dim: rgb(0x838BA7),
                muted: rgb(0x949CBB),
                faint: rgb(0x737994),
                divider: rgb(0x414559),
                accent: rgb(0x8CAAEE),
            },
        }
    }

    /// The widgets' look: egui's dark or light visuals in the theme's
    /// colours.
    pub fn visuals(self) -> egui::Visuals {
        let p = self.palette();
        let mut v = if self.is_dark() {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.panel_fill = p.header;
        v.window_fill = p.header;
        v.extreme_bg_color = p.deep;
        v.faint_bg_color = p.ruler;
        v.hyperlink_color = p.accent;
        v.selection.bg_fill = p.accent.gamma_multiply(0.6);
        v.widgets.noninteractive.fg_stroke.color = p.text;
        v.widgets.noninteractive.bg_fill = p.header;
        v.widgets.noninteractive.bg_stroke.color = p.divider;
        v.widgets.inactive.bg_fill = p.ruler;
        v.widgets.inactive.weak_bg_fill = p.ruler;
        v.widgets.inactive.fg_stroke.color = p.text2;
        v
    }
}

/// The view colours by role.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// The trace area (`GetStyleColor`).
    pub bg: Color32,
    pub header: Color32,
    pub ruler: Color32,
    /// Behind plots (FFT, Lissajous).
    pub deep: Color32,
    pub text: Color32,
    pub text2: Color32,
    /// Ruler ticks and their labels.
    pub tick: Color32,
    pub dim: Color32,
    /// Unassigned slots (the measurement bar).
    pub muted: Color32,
    pub faint: Color32,
    /// Channel dividers.
    pub divider: Color32,
    pub accent: Color32,
}

impl Default for Palette {
    fn default() -> Self {
        Theme::Dark.palette()
    }
}

/// `c` with alpha `a` (Qt's `setAlpha` on a palette colour).
pub fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// Sets the theme's visuals (`switchTheme`).
pub fn apply(ctx: &egui::Context, theme: Theme) {
    let v = theme.visuals();
    ctx.all_styles_mut(|s| s.visuals = v.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_round_trip() {
        for t in Theme::ALL {
            assert_eq!(Theme::from_style(t.style()), t);
        }
        assert_eq!(Theme::from_style("nonsense"), Theme::Dark);
        assert!(Theme::Frappe.is_dark() && !Theme::Latte.is_dark());
        assert_eq!(
            Theme::Frappe.palette().bg,
            Color32::from_rgb(0x30, 0x34, 0x46)
        );
    }
}
