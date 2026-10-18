//! Qt's SVG icons (`DSView/icons/{dark,light}`, `GetIconPath`): the
//! light-on-dark set for the dark and frappé themes, the other for light
//! and latte. They are built in and rasterised with resvg at the size and
//! pixel density they are drawn at; textures are kept per size.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;

macro_rules! icon_set {
    ($($name:literal),* $(,)?) => {
        const DARK: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../../../DSView/icons/dark/", $name, ".svg")))),*
        ];
        const LIGHT: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../../../DSView/icons/light/", $name, ".svg")))),*
        ];
    };
}

icon_set!(
    "about",
    "bug",
    "capture",
    "dark",
    "display",
    "export",
    "fft",
    "file",
    "function",
    "gear",
    "light",
    "lissajous",
    "log",
    "logo_color",
    "logo_noColor",
    "manual",
    "math",
    "measure",
    "open",
    "protocol",
    "save",
    "search",
    "single",
    "start",
    "stop",
    "trigger",
    "update",
);

/// The icon set follows the theme (`theme::apply`).
static DARK_ICONS: AtomicBool = AtomicBool::new(true);

pub fn set_dark(dark: bool) {
    DARK_ICONS.store(dark, Ordering::Relaxed);
}

/// The SVG of an icon in the current set.
pub fn svg(name: &str) -> Option<&'static [u8]> {
    let set = if DARK_ICONS.load(Ordering::Relaxed) {
        DARK
    } else {
        LIGHT
    };
    set.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// `size` × `size` pixels of an SVG (premultiplied RGBA), or None if it
/// does not parse.
pub fn rasterise(svg: &[u8], size: u32) -> Option<egui::ColorImage> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let s = tree.size();
    let k = size as f32 / s.width().max(s.height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(k, k),
        &mut pixmap.as_mut(),
    );
    Some(egui::ColorImage::from_rgba_premultiplied(
        [size as usize, size as usize],
        pixmap.data(),
    ))
}

#[derive(Clone, Default)]
struct Cache(HashMap<(&'static str, bool, u32), egui::TextureHandle>);

/// An icon `points` big, from the current set; None for an unknown name.
pub fn image(ctx: &egui::Context, name: &'static str, points: f32) -> Option<egui::Image<'static>> {
    let dark = DARK_ICONS.load(Ordering::Relaxed);
    let px = (points * ctx.pixels_per_point()).round().max(1.0) as u32;
    let id = egui::Id::NULL.with("dsview-icons");
    let cached = ctx.data(|d| {
        d.get_temp::<Cache>(id)
            .and_then(|c| c.0.get(&(name, dark, px)).cloned())
    });
    let tex = match cached {
        Some(t) => t,
        None => {
            let img = rasterise(svg(name)?, px)?;
            let t = ctx.load_texture(
                format!("icon-{name}-{dark}-{px}"),
                img,
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|d| {
                d.get_temp_mut_or_default::<Cache>(id)
                    .0
                    .insert((name, dark, px), t.clone())
            });
            t
        }
    };
    Some(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(points, points)))
}

/// A button with an icon and text, or the text alone when the icon is
/// missing.
pub fn button(
    ui: &mut egui::Ui,
    name: &'static str,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let h = ui.text_style_height(&egui::TextStyle::Button);
    match image(ui.ctx(), name, h) {
        Some(img) => ui.add(egui::Button::image_and_text(img, text)),
        None => ui.button(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_rasterises_in_both_sets() {
        for set in [DARK, LIGHT] {
            for (name, svg) in set {
                let img = rasterise(svg, 32).unwrap_or_else(|| panic!("{name}"));
                assert_eq!(img.size, [32, 32]);
                // something is drawn
                assert!(img.pixels.iter().any(|p| p.a() > 0), "{name} is empty");
            }
        }
    }
}
