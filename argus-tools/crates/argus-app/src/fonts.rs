//! qFlipper's fonts, or the closest freely licensed ones (assets/fonts/NOTICE says which and why):
//!
//! - [`PIXEL`]: m5x7 at 32 px, where qFlipper's `TextLabel` and `Button` use HaxrCorp 4089 at 32 px (both 7 px tall
//!   capitals, drawn two screen pixels per font pixel);
//! - [`SPORTY`]: Jersey 15 at 45 px, where qFlipper's names, titles and main button use Born2bSportyV2 at 48 px;
//! - [`PROGGY`]: ProggySquare at 16 px (the version, the connection badges), as qFlipper;
//! - [`SHARE`]: Share Tech (file names), and Share Tech Mono as egui's monospace (the log, the console), as qFlipper;
//! - egui's proportional text (the widgets egui lays out itself) is Pixel Operator at 16 px.
//!
//! Each pixel font is used only at a whole multiple of its pixel grid (m5x7 and Pixel Operator: 16 px, Jersey 15:
//! 15 px), without hinting and with glyphs on whole pixels, so its pixels stay square and sharp. egui's own fonts stay
//! behind them for any character they lack; [`pixel_text`] spells the ellipsis, which m5x7 lacks, as three dots.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, FontTweak};

pub const PIXEL: &str = "pixel";
pub const SPORTY: &str = "sporty";
pub const PROGGY: &str = "proggy";
pub const SHARE: &str = "share";

/// The size of egui's body text (Pixel Operator, one screen pixel per font pixel).
pub const BODY: f32 = 16.0;
/// The size of [`sporty`] titles (three screen pixels per font pixel).
pub const TITLE: f32 = 45.0;
/// Share Tech Mono's size in the log and the console.
pub const MONO: f32 = 15.0;

/// qFlipper's `TextLabel` and `Button` font: m5x7 at 32 px.
pub fn label() -> FontId {
    FontId::new(32.0, FontFamily::Name(PIXEL.into()))
}

/// Text for the [`PIXEL`] font in capitals, as qFlipper's labels and buttons are, with the ellipsis spelled out.
pub fn pixel_text(text: &str) -> String {
    text.to_uppercase().replace('…', "...")
}

pub fn sporty(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SPORTY.into()))
}

pub fn proggy() -> FontId {
    FontId::new(16.0, FontFamily::Name(PROGGY.into()))
}

pub fn share(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SHARE.into()))
}

fn data(bytes: &'static [u8], pixel_font: bool) -> Arc<FontData> {
    let tweak = if pixel_font {
        FontTweak { hinting: Some(false), subpixel_binning: Some(false), ..FontTweak::default() }
    } else {
        FontTweak::default()
    };
    Arc::new(FontData::from_static(bytes).tweak(tweak))
}

/// Installs the fonts; once, before the first frame that draws text.
pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let fallback = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let fonts: [(&str, &'static [u8], bool); 6] = [
        ("m5x7", include_bytes!("../assets/fonts/m5x7.ttf"), true),
        ("PixelOperator", include_bytes!("../assets/fonts/PixelOperator.ttf"), true),
        ("Jersey15", include_bytes!("../assets/fonts/Jersey15-Regular.ttf"), true),
        ("ProggySquare", include_bytes!("../../../../application/assets/fonts/ProggySquare.ttf"), true),
        ("ShareTech", include_bytes!("../../../../application/assets/fonts/ShareTech-Regular.ttf"), false),
        ("ShareTechMono", include_bytes!("../../../../application/assets/fonts/ShareTechMono-Regular.ttf"), false),
    ];
    for (name, bytes, pixel_font) in fonts {
        defs.font_data.insert(name.into(), data(bytes, pixel_font));
    }
    let with_fallback = |first: &str| std::iter::once(first.to_string()).chain(fallback.iter().cloned()).collect::<Vec<_>>();
    defs.families.insert(FontFamily::Name(PIXEL.into()), with_fallback("m5x7"));
    defs.families.insert(FontFamily::Name(SPORTY.into()), with_fallback("Jersey15"));
    defs.families.insert(FontFamily::Name(PROGGY.into()), with_fallback("ProggySquare"));
    defs.families.insert(FontFamily::Name(SHARE.into()), with_fallback("ShareTech"));
    defs.families.entry(FontFamily::Proportional).or_default().insert(0, "PixelOperator".into());
    defs.families.entry(FontFamily::Monospace).or_default().insert(0, "ShareTechMono".into());
    ctx.set_fonts(defs);
}
