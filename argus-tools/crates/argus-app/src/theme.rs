//! The look of Yggdrasil's own apps, so Argus sits on the desktop like Minos and Settings do: the palette, the egui
//! style, light or dark and the accent colour from Settings > Appearance (`~/.config/yggdrasil/appearance.conf`, read
//! at most once a second, so a change shows without a restart).
//!
//! This mirrors Yggdrasil's `yggdrasil-ui` crate (packages/yggdrasil-ui/src/theme.rs) rather than depending on it:
//! Argus is built and tested on its own (its CI has no Yggdrasil checkout), and the palette is small. When that crate's
//! colours change, change them here too.

use std::path::PathBuf;

use eframe::egui::{self, Color32, CornerRadius};

/// Yggdrasil's fallback accent (yggdrasil-conf's `FALLBACK_ACCENT`), used when the desktop names none.
pub const FALLBACK_ACCENT: [u8; 3] = [124, 84, 214];

/// The Flipper's screen as the Qt app drew it: lit pixels in the dark orange, the rest in the LCD orange.
pub const LCD_BACKGROUND: Color32 = Color32::from_rgb(0xfe, 0x8a, 0x2c);
pub const LCD_PIXEL: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
pub const LCD_PIXEL_SMALL: Color32 = Color32::from_rgb(0x49, 0x1d, 0x00);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub strip: Color32,
    pub field: Color32,
    pub border: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub hover: Color32,
    pub select: Color32,
    pub row_alt: Color32,
    pub folder: Color32,
    pub green: Color32,
    pub amber: Color32,
    pub red: Color32,
    pub blue: Color32,
}

impl Palette {
    /// Text that sits on the accent colour: white, or near-black on a light accent.
    pub fn on_accent(&self) -> Color32 {
        let l = |c: u8| {
            let v = c as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let lum = 0.2126 * l(self.accent.r()) + 0.7152 * l(self.accent.g()) + 0.0722 * l(self.accent.b());
        if lum > 0.45 {
            Color32::from_rgb(24, 20, 36)
        } else {
            Color32::WHITE
        }
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// A colour whose brightest channel is under 150 is scaled up until it is 176 (hue kept), so it reads on the dark style.
fn readable_on_dark(c: Color32) -> Color32 {
    let m = c.r().max(c.g()).max(c.b());
    if m >= 150 || m == 0 {
        return c;
    }
    let k = 176.0 / m as f32;
    let f = |x: u8| (x as f32 * k).round().min(255.0) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// The palette for the style and accent (None: Yggdrasil's fallback accent).
pub fn palette(dark: bool, accent: Option<[u8; 3]>) -> Palette {
    let mut p = if dark {
        Palette {
            bg: Color32::from_rgb(16, 17, 22),
            sidebar: Color32::from_rgb(14, 15, 20),
            strip: Color32::from_rgb(11, 12, 16),
            field: Color32::from_rgb(24, 25, 32),
            border: Color32::from_rgb(38, 40, 52),
            text: Color32::from_rgb(232, 233, 238),
            dim: Color32::from_rgb(140, 143, 157),
            accent: Color32::from_rgb(FALLBACK_ACCENT[0], FALLBACK_ACCENT[1], FALLBACK_ACCENT[2]),
            hover: Color32::from_rgb(32, 34, 44),
            select: Color32::from_rgb(52, 40, 92),
            row_alt: Color32::from_rgb(19, 20, 26),
            folder: Color32::from_rgb(240, 196, 64),
            green: Color32::from_rgb(58, 158, 94),
            amber: Color32::from_rgb(201, 146, 58),
            red: Color32::from_rgb(180, 58, 69),
            blue: Color32::from_rgb(64, 140, 230),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(250, 250, 252),
            sidebar: Color32::from_rgb(243, 243, 247),
            strip: Color32::from_rgb(232, 232, 238),
            field: Color32::from_rgb(255, 255, 255),
            border: Color32::from_rgb(214, 214, 224),
            text: Color32::from_rgb(28, 29, 36),
            dim: Color32::from_rgb(110, 113, 128),
            accent: Color32::from_rgb(110, 70, 200),
            hover: Color32::from_rgb(232, 232, 240),
            select: Color32::from_rgb(214, 200, 244),
            row_alt: Color32::from_rgb(244, 244, 248),
            folder: Color32::from_rgb(232, 178, 40),
            green: Color32::from_rgb(46, 140, 80),
            amber: Color32::from_rgb(196, 130, 30),
            red: Color32::from_rgb(190, 50, 60),
            blue: Color32::from_rgb(30, 110, 210),
        }
    };
    if let Some([r, g, b]) = accent {
        let a = Color32::from_rgb(r, g, b);
        p.accent = if dark { readable_on_dark(a) } else { a };
        p.select = mix(p.bg, a, if dark { 0.28 } else { 0.30 });
    }
    p
}

/// Applies the palette to egui's style, as yggdrasil-ui's `theme::apply` does.
pub fn apply(ctx: &egui::Context, p: &Palette, dark: bool) {
    ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        v.panel_fill = p.bg;
        v.window_fill = p.field;
        v.extreme_bg_color = p.field;
        v.faint_bg_color = p.row_alt;
        v.window_stroke = egui::Stroke::new(1.0, p.border);
        v.selection.bg_fill = p.select;
        v.selection.stroke = egui::Stroke::new(1.0, p.accent);
        v.override_text_color = Some(p.text);
        let r = CornerRadius::same(7);
        for w in
            [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open]
        {
            w.corner_radius = r;
        }
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border);
        v.widgets.inactive.weak_bg_fill = p.field;
        v.widgets.inactive.bg_fill = p.field;
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.hovered.bg_fill = p.hover;
        v.widgets.active.weak_bg_fill = p.select;
        v.widgets.active.bg_fill = p.select;
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
        style.interaction.selectable_labels = false;
    });
}

/// Where the style comes from: the desktop's choice, or a fixed one (Settings, and the tests).
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// `appearance.conf` under this config folder (Yggdrasil's), read at most once a second.
    System(PathBuf),
    Fixed {
        dark: bool,
        accent: Option<[u8; 3]>,
    },
}

/// `style` and `accent` lines of Yggdrasil's `appearance.conf` (dark and no accent when absent), as yggdrasil-conf reads them.
pub fn parse_appearance(text: &str) -> (bool, Option<[u8; 3]>) {
    let mut dark = true;
    let mut accent = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "style" => dark = v.trim() != "light",
            "accent" => {
                let parts: Vec<u8> = v.trim().trim_matches('"').split(',').filter_map(|x| x.trim().parse().ok()).collect();
                accent = (parts.len() == 3).then(|| [parts[0], parts[1], parts[2]]);
            }
            _ => {}
        }
    }
    (dark, accent)
}

/// Yggdrasil's config folder: `$XDG_CONFIG_HOME/yggdrasil`, else `~/.config/yggdrasil`.
pub fn yggdrasil_config() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("yggdrasil"))
}

/// The style in use, refreshed from its source at most once a second (by the app's clock).
pub struct Theme {
    pub source: Source,
    pub dark: bool,
    pub accent: Option<[u8; 3]>,
    checked: f64,
    applied: Option<(bool, Option<[u8; 3]>)>,
}

impl Theme {
    pub fn new(source: Source) -> Theme {
        let mut t = Theme { source, dark: true, accent: None, checked: f64::NEG_INFINITY, applied: None };
        t.refresh(0.0);
        t
    }

    fn refresh(&mut self, now: f64) {
        if now - self.checked < 1.0 {
            return;
        }
        self.checked = now;
        (self.dark, self.accent) = match &self.source {
            Source::Fixed { dark, accent } => (*dark, *accent),
            Source::System(dir) => {
                std::fs::read_to_string(dir.join("appearance.conf")).map(|t| parse_appearance(&t)).unwrap_or((true, None))
            }
        };
    }

    /// Call at the top of every frame: re-reads the source when due and restyles egui when it changed.
    pub fn frame(&mut self, ctx: &egui::Context) -> Palette {
        self.refresh(ctx.input(|i| i.time));
        let p = palette(self.dark, self.accent);
        if self.applied != Some((self.dark, self.accent)) {
            apply(ctx, &p, self.dark);
            self.applied = Some((self.dark, self.accent));
        }
        p
    }

    /// A new source (Settings changed the style): takes effect on the next frame.
    pub fn set_source(&mut self, source: Source) {
        self.source = source;
        self.checked = f64::NEG_INFINITY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_lines_are_read_as_yggdrasil_reads_them() {
        assert_eq!(parse_appearance(""), (true, None));
        assert_eq!(parse_appearance("style = light\naccent = 40, 160, 90 # green\n"), (false, Some([40, 160, 90])));
        assert_eq!(parse_appearance("style=dark\naccent=1,2\n"), (true, None));
    }

    #[test]
    fn an_accent_drives_the_selection_and_a_dark_one_is_lifted() {
        let p = palette(true, Some([101, 0, 11]));
        assert_eq!(p.accent, Color32::from_rgb(176, 0, 19));
        assert_eq!(palette(false, Some([101, 0, 11])).accent, Color32::from_rgb(101, 0, 11));
        assert_eq!(palette(true, None).bg, palette(true, Some([1, 2, 3])).bg);
    }

    #[test]
    fn the_system_source_follows_the_file() {
        let dir = std::env::temp_dir().join(format!("argus-app-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("appearance.conf"), "style = light\n").unwrap();
        let mut t = Theme::new(Source::System(dir.clone()));
        assert!(!t.dark);
        std::fs::write(dir.join("appearance.conf"), "style = dark\naccent = 10,200,10\n").unwrap();
        t.refresh(0.5);
        assert!(!t.dark, "not re-read within a second");
        t.refresh(1.5);
        assert!(t.dark && t.accent == Some([10, 200, 10]));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
