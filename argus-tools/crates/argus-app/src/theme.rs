//! qFlipper's look (application/imports/Theme/Theme.qml, the DefaultAmber style) with the user's accent where qFlipper
//! has orange: every orange of qFlipper's palette is made from the accent with the same saturation and brightness steps
//! qFlipper takes from its own orange (`#fe8a2c`), and qFlipper's greens, blues and reds stay as they are.
//!
//! qFlipper is dark only, and so is this: the accent comes from Yggdrasil's Settings > Appearance
//! (`~/.config/yggdrasil/appearance.conf`, read at most once a second, so a change shows without a restart), or is
//! Yggdrasil's purple; Yggdrasil's light style is not followed.

use std::path::PathBuf;

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

use crate::fonts;

/// Yggdrasil's fallback accent (yggdrasil-conf's `FALLBACK_ACCENT`), used when the desktop names none.
pub const FALLBACK_ACCENT: [u8; 3] = [124, 84, 214];

/// qFlipper's orange (`Theme.color.lightorange2`): every other orange is a step from it.
const QF_ORANGE: [u8; 3] = [0xfe, 0x8a, 0x2c];

/// qFlipper's colours, with the accent's in place of its oranges. The names are Theme.qml's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// `lightorange2`: the accent itself, qFlipper's text and outline colour.
    pub accent: Color32,
    /// `lightorange1`: hover.
    pub light1: Color32,
    /// `lightorange3`: values in the info panel.
    pub light3: Color32,
    /// `darkorange1`: button fill, the status bar, text on the accent, the LCD's lit pixels.
    pub dark1: Color32,
    /// `darkorange2`: a disabled button's fill.
    pub dark2: Color32,
    /// `mediumorange1`: disabled text and outlines, a small button's outline.
    pub medium1: Color32,
    /// `mediumorange2`: hover fill.
    pub medium2: Color32,
    /// `mediumorange3`: the window's border.
    pub medium3: Color32,
    /// `mediumorange4`: labels in the info panel.
    pub medium4: Color32,
    /// The content card's gradient (GridBackground.qml), its grid lines, the device body's gradient (flipper.svg).
    pub card_top: Color32,
    pub card_bottom: Color32,
    pub grid: Color32,
    pub body_top: Color32,
    pub body_middle: Color32,
    pub body_bottom: Color32,
    /// qFlipper's `lightgreen`, `mediumgreen1`, `mediumgreen2`, `darkgreen`.
    pub green: Color32,
    pub green_medium1: Color32,
    pub green_medium2: Color32,
    pub green_dark: Color32,
    /// `lightblue`, `darkblue1`, `darkblue2`.
    pub blue: Color32,
    pub blue_dark1: Color32,
    pub blue_dark2: Color32,
    /// `lightred2` (errors, warnings, the danger button), `lightred1` (its hover), `lightred3` (missing values),
    /// `lightred4` (the status bar's text on error), `mediumred1`, `mediumred2`, `darkred1`, `darkred2`.
    pub red: Color32,
    pub red1: Color32,
    pub red3: Color32,
    pub red4: Color32,
    pub red_medium1: Color32,
    pub red_medium2: Color32,
    pub red_dark1: Color32,
    pub red_dark2: Color32,
    /// The names the rest of the app uses for the same colours.
    pub bg: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub field: Color32,
    pub border: Color32,
    pub hover: Color32,
    pub select: Color32,
    pub sidebar: Color32,
    pub strip: Color32,
    pub folder: Color32,
    /// Warnings that are not errors: qFlipper's own orange.
    pub amber: Color32,
}

impl Palette {
    /// Text that sits on the accent colour: qFlipper's `darkorange1`.
    pub fn on_accent(&self) -> Color32 {
        self.dark1
    }
}

fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn hsv(c: [u8; 3]) -> (f32, f32, f32) {
    let [r, g, b] = c.map(|x| x as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn from_hsv(h: f32, s: f32, v: f32) -> Color32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let f = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(r), f(g), f(b))
}

/// One of qFlipper's oranges made from the accent: the accent's hue, and the orange's saturation and brightness taken
/// relative to qFlipper's base orange and applied to the accent's.
fn tone(accent: [u8; 3], orange: u32) -> Color32 {
    let (h, s, v) = hsv(accent);
    let (_, bs, bv) = hsv(QF_ORANGE);
    let o = hex(orange);
    let (_, os, ov) = hsv([o.r(), o.g(), o.b()]);
    from_hsv(h, (s * os / bs).min(1.0), (v * ov / bv).min(1.0))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// An accent whose brightest channel is under 150 is scaled up until it is 176 (hue kept), so it reads on black.
fn readable_on_dark(c: [u8; 3]) -> [u8; 3] {
    let m = c[0].max(c[1]).max(c[2]);
    if m >= 150 || m == 0 {
        return c;
    }
    let k = 176.0 / m as f32;
    c.map(|x| (x as f32 * k).round().min(255.0) as u8)
}

/// The palette for an accent (None: Yggdrasil's purple).
pub fn palette(accent: Option<[u8; 3]>) -> Palette {
    let a = readable_on_dark(accent.unwrap_or(FALLBACK_ACCENT));
    let accent = Color32::from_rgb(a[0], a[1], a[2]);
    let t = |orange: u32| tone(a, orange);
    let dark1 = t(0x491d00);
    let medium4 = t(0x9b450b);
    Palette {
        accent,
        // qFlipper's hover orange is a little yellower; a hue shift means nothing for other accents, so it is lighter
        light1: mix(accent, Color32::WHITE, 0.2),
        light3: t(0xce5300),
        dark1,
        dark2: t(0x3a1f10),
        medium1: t(0x9e5823),
        medium2: t(0x76380b),
        medium3: t(0x662c00),
        medium4,
        card_top: t(0x090400),
        card_bottom: t(0x210f00),
        grid: t(0xaa5115),
        body_top: t(0x090400),
        body_middle: t(0x220f00),
        body_bottom: t(0x3c1a00),
        green: hex(0x2ed832),
        green_medium1: hex(0x285b12),
        green_medium2: hex(0x203812),
        green_dark: hex(0x0c160c),
        blue: hex(0x228cff),
        blue_dark1: hex(0x11355c),
        blue_dark2: hex(0x152b47),
        red: hex(0xff5924),
        red1: hex(0xff5b27),
        red3: hex(0xff1f00),
        red4: hex(0xff3c00),
        red_medium1: hex(0x953618),
        red_medium2: hex(0x672715),
        red_dark1: hex(0x451a0e),
        red_dark2: hex(0x331400),
        bg: Color32::BLACK,
        text: accent,
        dim: medium4,
        field: dark1,
        border: t(0x9e5823),
        hover: t(0x76380b),
        select: t(0x662c00),
        sidebar: t(0x090400),
        strip: t(0x3a1f10),
        folder: accent,
        amber: hex(0xfe8a2c),
    }
}

/// Applies the palette and qFlipper's fonts to egui's style (DefaultAmber's Button, ComboBox, Switch, ScrollBar,
/// ToolTip, Dialog), so the widgets egui draws itself look like qFlipper's.
pub fn apply(ctx: &egui::Context, p: &Palette) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(30.0, FontFamily::Name(fonts::SPORTY.into()))),
            (TextStyle::Body, FontId::proportional(fonts::BODY)),
            (TextStyle::Button, FontId::proportional(fonts::BODY)),
            (TextStyle::Small, FontId::proportional(fonts::BODY)),
            (TextStyle::Monospace, FontId::monospace(fonts::MONO)),
        ]
        .into();
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = Color32::BLACK;
        v.window_fill = Color32::BLACK;
        v.extreme_bg_color = Color32::BLACK;
        v.faint_bg_color = p.dark2;
        v.code_bg_color = p.dark1;
        v.window_stroke = Stroke::new(2.0, p.accent);
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.window_corner_radius = CornerRadius::same(7);
        v.menu_corner_radius = CornerRadius::same(4);
        v.selection.bg_fill = p.accent;
        v.selection.stroke = Stroke::new(1.0, p.dark1);
        v.hyperlink_color = p.accent;
        v.warn_fg_color = p.amber;
        v.error_fg_color = p.red;
        v.override_text_color = None;
        v.text_cursor.stroke = Stroke::new(2.0, p.accent);
        v.interact_cursor = Some(egui::CursorIcon::PointingHand);
        let r = CornerRadius::same(5);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = Color32::BLACK;
        w.noninteractive.weak_bg_fill = Color32::BLACK;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.medium1);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.accent);
        // a button at rest: DefaultAmber's SmallButton (no fill, a dim outline); hovered, pressed and open as Button
        w.inactive.bg_fill = p.dark1;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::new(2.0, p.medium1);
        w.inactive.fg_stroke = Stroke::new(1.0, p.accent);
        w.hovered.bg_fill = p.medium2;
        w.hovered.weak_bg_fill = p.medium2;
        w.hovered.bg_stroke = Stroke::new(2.0, p.light1);
        w.hovered.fg_stroke = Stroke::new(1.0, p.light1);
        w.active.bg_fill = p.accent;
        w.active.weak_bg_fill = p.accent;
        w.active.bg_stroke = Stroke::new(2.0, p.accent);
        w.active.fg_stroke = Stroke::new(1.0, p.dark1);
        w.open.bg_fill = p.dark1;
        w.open.weak_bg_fill = p.medium2;
        w.open.bg_stroke = Stroke::new(2.0, p.accent);
        w.open.fg_stroke = Stroke::new(1.0, p.accent);
        for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            s.corner_radius = r;
            s.expansion = 0.0;
        }
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size.y = 26.0;
        style.interaction.selectable_labels = false;
    });
}

/// Where the accent comes from: the desktop's choice, or a fixed one (Settings, and the tests).
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// `appearance.conf` under this config folder (Yggdrasil's), read at most once a second.
    System(PathBuf),
    Fixed {
        accent: Option<[u8; 3]>,
    },
}

/// The `accent` line of Yggdrasil's `appearance.conf` (None when absent), as yggdrasil-conf reads it.
pub fn parse_appearance(text: &str) -> Option<[u8; 3]> {
    let mut accent = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        if k.trim() == "accent" {
            let parts: Vec<u8> = v.trim().trim_matches('"').split(',').filter_map(|x| x.trim().parse().ok()).collect();
            accent = (parts.len() == 3).then(|| [parts[0], parts[1], parts[2]]);
        }
    }
    accent
}

/// Yggdrasil's config folder: `$XDG_CONFIG_HOME/yggdrasil`, else `~/.config/yggdrasil`.
pub fn yggdrasil_config() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("yggdrasil"))
}

/// The accent in use, refreshed from its source at most once a second (by the app's clock).
pub struct Theme {
    pub source: Source,
    pub accent: Option<[u8; 3]>,
    checked: f64,
    /// The accent egui was last styled with; `None` before the first frame (the fonts are installed then).
    applied: Option<Option<[u8; 3]>>,
}

impl Theme {
    pub fn new(source: Source) -> Theme {
        let mut t = Theme { source, accent: None, checked: f64::NEG_INFINITY, applied: None };
        t.refresh(0.0);
        t
    }

    fn refresh(&mut self, now: f64) {
        if now - self.checked < 1.0 {
            return;
        }
        self.checked = now;
        self.accent = match &self.source {
            Source::Fixed { accent } => *accent,
            Source::System(dir) => std::fs::read_to_string(dir.join("appearance.conf")).ok().and_then(|t| parse_appearance(&t)),
        };
    }

    /// Call at the top of every frame: re-reads the source when due and restyles egui when it changed. `None` on the
    /// first frame: the fonts are installed then and egui takes them from the next frame on, so nothing is drawn yet.
    pub fn frame(&mut self, ctx: &egui::Context) -> Option<Palette> {
        self.refresh(ctx.input(|i| i.time));
        let p = palette(self.accent);
        let first = self.applied.is_none();
        if first {
            fonts::install(ctx);
        }
        if self.applied != Some(self.accent) {
            apply(ctx, &p);
            self.applied = Some(self.accent);
        }
        if first {
            ctx.request_repaint();
            return None;
        }
        Some(p)
    }

    /// A new source (Settings changed the accent): takes effect on the next frame.
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
        assert_eq!(parse_appearance(""), None);
        assert_eq!(parse_appearance("style = light\naccent = 40, 160, 90 # green\n"), Some([40, 160, 90]));
        assert_eq!(parse_appearance("style=dark\naccent=1,2\n"), None);
    }

    #[test]
    fn qflippers_orange_gives_qflippers_palette_back() {
        // with qFlipper's own orange as the accent, the steps land on Theme.qml's colours, give or take the few degrees
        // of hue qFlipper's oranges differ by (the accent's hue is kept for every step)
        let p = palette(Some(QF_ORANGE));
        let near = |c: Color32, v: u32| {
            let w = hex(v);
            assert!(
                c.r().abs_diff(w.r()) <= 12 && c.g().abs_diff(w.g()) <= 12 && c.b().abs_diff(w.b()) <= 12,
                "{c:?} is not near #{v:06x}"
            );
        };
        near(p.accent, 0xfe8a2c);
        near(p.dark1, 0x491d00);
        near(p.medium1, 0x9e5823);
        near(p.medium4, 0x9b450b);
        near(p.light3, 0xce5300);
        near(p.card_bottom, 0x210f00);
        assert_eq!(p.green, hex(0x2ed832), "the greens stay qFlipper's");
    }

    #[test]
    fn a_dark_accent_is_lifted_and_the_steps_keep_its_hue() {
        assert_eq!(palette(Some([101, 0, 11])).accent, Color32::from_rgb(176, 0, 19));
        let p = palette(None);
        assert_eq!(p.accent, Color32::from_rgb(124, 84, 214));
        let (h, _, v) = hsv([p.dark1.r(), p.dark1.g(), p.dark1.b()]);
        assert!((h - hsv(FALLBACK_ACCENT).0).abs() < 3.0 && v < 0.3, "a dark purple");
    }

    #[test]
    fn the_system_source_follows_the_file() {
        let dir = std::env::temp_dir().join(format!("argus-app-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("appearance.conf"), "style = light\n").unwrap();
        let mut t = Theme::new(Source::System(dir.clone()));
        assert_eq!(t.accent, None);
        std::fs::write(dir.join("appearance.conf"), "accent = 10,200,10\n").unwrap();
        t.refresh(0.5);
        assert_eq!(t.accent, None, "not re-read within a second");
        t.refresh(1.5);
        assert_eq!(t.accent, Some([10, 200, 10]));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
