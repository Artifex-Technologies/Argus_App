//! qFlipper's shapes, drawn with egui's painter: the device (application/assets/gfx/images/flipper.svg, its geometry
//! read from the SVG's path data, without the wordmark), the buttons of the DefaultAmber style (`Button.qml`,
//! `SmallButton.qml`, `SmallButtonRed.qml`, `MainButton.qml`, `LinkButton.qml`, `TabButton.qml`), the tab pane's
//! dotted outline (`TabPane.qml`), the gradients of `GridBackground.qml` and the device body, the USB plug
//! (`typec.svg`) and the spinner (`spinner.svg`). qFlipper is GPL-3.0, as this is.

use eframe::egui::{
    self, epaint, pos2, vec2, Align2, Color32, FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
    WidgetInfo, WidgetType,
};

use super::icons::Icon;
use crate::fonts;
use crate::theme::Palette;

// ------------------------------------------------------------------------------------------------- SVG path data

/// The subpaths of SVG path data (M, L, H, V, C, S, Z, absolute and relative), curves flattened into lines.
pub fn svg_path(d: &str) -> Vec<Vec<Pos2>> {
    let mut tokens: Vec<Result<char, f32>> = Vec::new();
    let mut chars = d.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            tokens.push(Ok(c));
        } else if c == '-' || c == '.' || c.is_ascii_digit() {
            let mut end = i + c.len_utf8();
            let mut dot = c == '.';
            while let Some(&(j, n)) = chars.peek() {
                let exponent = (n == 'e' || n == 'E') && !d[i..j].contains(['e', 'E']);
                let sign_after_e = (n == '-' || n == '+') && d[..j].ends_with(['e', 'E']);
                if n.is_ascii_digit() || (n == '.' && !dot) || exponent || sign_after_e {
                    dot |= n == '.';
                    end = j + n.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Err(d[i..end].parse().unwrap_or(0.0)));
        }
    }
    let mut out: Vec<Vec<Pos2>> = Vec::new();
    let (mut cur, mut start, mut last_ctrl) = (Pos2::ZERO, Pos2::ZERO, None::<Pos2>);
    let mut cmd = 'M';
    let mut i = 0;
    let num = |i: &mut usize| -> f32 {
        let v = match tokens.get(*i) {
            Some(Err(v)) => *v,
            _ => 0.0,
        };
        *i += 1;
        v
    };
    let cubic = |out: &mut Vec<Vec<Pos2>>, p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2| {
        let seg = out.last_mut().expect("a subpath");
        for k in 1..=12 {
            let t = k as f32 / 12.0;
            let u = 1.0 - t;
            seg.push(pos2(
                u * u * u * p0.x + 3.0 * u * u * t * p1.x + 3.0 * u * t * t * p2.x + t * t * t * p3.x,
                u * u * u * p0.y + 3.0 * u * u * t * p1.y + 3.0 * u * t * t * p2.y + t * t * t * p3.y,
            ));
        }
    };
    while i < tokens.len() {
        if let Ok(c) = tokens[i] {
            cmd = c;
            i += 1;
            if matches!(c, 'Z' | 'z') {
                if let Some(seg) = out.last_mut() {
                    seg.push(start);
                }
                cur = start;
                last_ctrl = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur.to_vec2() } else { Vec2::ZERO };
        match cmd.to_ascii_uppercase() {
            'M' => {
                cur = pos2(num(&mut i), num(&mut i)) + base;
                start = cur;
                out.push(vec![cur]);
                // further pairs after a move are lines
                cmd = if rel { 'l' } else { 'L' };
                last_ctrl = None;
            }
            'L' => {
                cur = pos2(num(&mut i), num(&mut i)) + base;
                out.last_mut().expect("a subpath").push(cur);
                last_ctrl = None;
            }
            'H' => {
                cur.x = num(&mut i) + base.x;
                out.last_mut().expect("a subpath").push(cur);
                last_ctrl = None;
            }
            'V' => {
                cur.y = num(&mut i) + base.y;
                out.last_mut().expect("a subpath").push(cur);
                last_ctrl = None;
            }
            'C' => {
                let p1 = pos2(num(&mut i), num(&mut i)) + base;
                let p2 = pos2(num(&mut i), num(&mut i)) + base;
                let p3 = pos2(num(&mut i), num(&mut i)) + base;
                cubic(&mut out, cur, p1, p2, p3);
                last_ctrl = Some(p2);
                cur = p3;
            }
            'S' => {
                let p1 = last_ctrl.map_or(cur, |c| cur + (cur - c));
                let p2 = pos2(num(&mut i), num(&mut i)) + base;
                let p3 = pos2(num(&mut i), num(&mut i)) + base;
                cubic(&mut out, cur, p1, p2, p3);
                last_ctrl = Some(p2);
                cur = p3;
            }
            _ => i += 1,
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------- fills

/// Fills a star-shaped polygon (every point seen from its centroid) with a vertical gradient through `stops`
/// (`(y, colour)`, top to bottom), as a fan of triangles whose corners take the gradient's colour at their height.
pub fn gradient_polygon(p: &Painter, pts: &[Pos2], stops: &[(f32, Color32)]) {
    if pts.len() < 3 {
        return;
    }
    let at = |y: f32| -> Color32 {
        let first = stops[0];
        if y <= first.0 {
            return first.1;
        }
        for w in stops.windows(2) {
            let ((y0, c0), (y1, c1)) = (w[0], w[1]);
            if y <= y1 {
                let t = ((y - y0) / (y1 - y0).max(1e-3)).clamp(0.0, 1.0);
                return c0.lerp_to_gamma(c1, t);
            }
        }
        stops[stops.len() - 1].1
    };
    let centre = pts.iter().fold(Vec2::ZERO, |a, p| a + p.to_vec2()) / pts.len() as f32;
    let mut mesh = epaint::Mesh::default();
    mesh.colored_vertex(centre.to_pos2(), at(centre.y));
    for q in pts {
        mesh.colored_vertex(*q, at(q.y));
    }
    let n = pts.len() as u32;
    for k in 0..n {
        mesh.add_triangle(0, 1 + k, 1 + (k + 1) % n);
    }
    p.add(Shape::mesh(mesh));
}

/// The corners of a rounded rectangle, as epaint draws it.
pub fn rounded(rect: Rect, radius: f32) -> Vec<Pos2> {
    let mut pts = Vec::new();
    epaint::tessellator::path::rounded_rectangle(&mut pts, rect, egui::epaint::CornerRadiusF32::same(radius));
    pts
}

/// A dotted line, as TabPane.qml's sides (dots of `dot`, a gap of `gap`).
pub fn dotted(p: &Painter, from: Pos2, to: Pos2, width: f32, dot: f32, gap: f32, colour: Color32) {
    let len = from.distance(to);
    if len <= 0.0 {
        return;
    }
    let dir = (to - from) / len;
    let mut s = 0.0;
    while s < len {
        let e = (s + dot).min(len);
        p.line_segment([from + dir * s, from + dir * e], Stroke::new(width, colour));
        s += dot + gap;
    }
}

// ------------------------------------------------------------------------------------------------- the device

/// What the device's screen shows.
pub enum Screen {
    /// A frame of the live screen (a 128 x 64 texture).
    Live(egui::TextureId),
    /// Nothing (the lit LCD): qFlipper shows its dolphin here, which is Flipper's art.
    Blank,
    Success,
    Warning,
    Error,
    Recovery,
}

/// flipper.svg's parts (its group is moved 2 px left, as the SVG does), without the wordmark.
const BODY: &str = "m 303.3,1.2 c 0,0 -219.9,0 -229.7,0 -9.8,0 -17,10.3 -17,10.3 0,0 -49.3,63.9 -51.4,66.8 -3,4 -2.1,14.1 0,17 \
    5.9,8 34.5,47.1 35.8,48.8 1.4,1.7 7.7,11.1 16.5,11.1 8.8,0 278.7,0 283.5,0 8.7,0 19.6,-20.2 19.6,-25.2 0,-5 -0.3,-58.9 \
    -0.3,-64.5 0,-3.7 -2.6,-8.4 -5.1,-11.5 C 352.9,51.2 319.9,10 317,6.5 314.5,3.6 308.8,1.2 303.3,1.2 Z";
const INNER: &str = "M 341.5,120 H 72.3 c -9.1,0 -12.5,-3.8 -17.7,-9.5 L 54,109.8 C 49.1,104.3 42.3,96.3 38.2,91 34.4,86.1 \
    35.9,83.7 38.8,79 L 39,78.7 C 40.6,76.2 50.1,62.7 59.3,49.6 68.5,36.6 78,23.1 79.8,20.3 84.5,13.1 90.5,10 100,10 h \
    199.6 c 3.6,0 8.8,1.7 12,5.5 l 0.8,0.9 c 20.6,24.6 32.3,38.7 34.9,41.9 4.2,5.3 4.8,7.7 4.8,11.4 v 39.7 c -0.1,5 \
    -4.4,10.6 -10.6,10.6 z";
const KEYPAD: &str = "M 329.5,98 H 277 c -22.4,0 -40,-17.1 -40,-39 0,-22 17.9,-40 40,-40 11.1,0 21.7,4.6 29.3,12.8 l \
    33.9,40.8 c 4,4.3 3.9,8.7 3.9,11.3 v 0.6 C 344,90.7 336.1,98 329.5,98 Z";
const ARROWS: [&str; 4] = [
    "m 281.2,40 h -9.3 c -0.4,0 -0.8,-0.2 -0.9,-0.5 -0.2,-0.3 -0.2,-0.7 0,-1.1 l 4.7,-7.9 c 0.4,-0.7 1.5,-0.7 1.9,0 l \
     4.7,7.9 c 0.2,0.3 0.2,0.7 0,1.1 -0.4,0.3 -0.8,0.5 -1.1,0.5 z",
    "m 276.5,87 c -0.4,0 -0.8,-0.2 -1,-0.5 l -4.7,-7.9 c -0.2,-0.3 -0.2,-0.7 0,-1.1 0.2,-0.3 0.6,-0.5 0.9,-0.5 h 9.3 c \
     0.4,0 0.8,0.2 0.9,0.5 0.2,0.3 0.2,0.7 0,1.1 l -4.7,7.9 c 0.1,0.3 -0.3,0.5 -0.7,0.5 z",
    "m 296.1,64.2 c -0.2,0 -0.4,0 -0.5,-0.1 -0.3,-0.2 -0.5,-0.6 -0.5,-0.9 v -9.3 c 0,-0.4 0.2,-0.8 0.5,-0.9 0.3,-0.2 \
     0.7,-0.2 1.1,0 l 7.9,4.7 c 0.3,0.2 0.5,0.6 0.5,1 0,0.4 -0.2,0.8 -0.5,1 l -7.9,4.7 c -0.3,-0.2 -0.4,-0.2 -0.6,-0.2 z",
    "m 256.9,64.2 c -0.2,0 -0.4,0 -0.5,-0.1 l -7.9,-4.7 c -0.3,-0.2 -0.5,-0.6 -0.5,-1 0,-0.4 0.2,-0.8 0.5,-1 l 7.9,-4.7 c \
     0.3,-0.2 0.7,-0.2 1.1,0 0.3,0.2 0.5,0.6 0.5,0.9 v 9.3 c 0,0.4 -0.2,0.8 -0.5,0.9 -0.2,0.4 -0.4,0.4 -0.6,0.4 z",
];
const BACK_ARROW: &str = "m 330.8,88 h -6.4 c -0.3,0 -0.3,-0.2 -0.3,-0.5 v -2 c 0,-0.3 0,-0.4 0.3,-0.4 h 5.9 c 0.6,0 \
    1.1,-0.3 1.4,-0.7 0.3,-0.4 0.5,-0.9 0.5,-1.4 0,-1 -0.5,-2 -1.6,-2 H 326 l -0.1,1.6 c 0,0.2 -0.1,0.4 -0.3,0.5 \
    -0.2,0.1 -0.4,0 -0.5,-0.1 l -2.9,-3 c -0.2,-0.2 -0.2,-0.5 0,-0.7 l 2.8,-3 c 0.1,-0.1 0.4,-0.2 0.5,-0.1 0.2,0.1 \
    0.3,0.3 0.3,0.5 V 78 h 4.9 c 2.8,0 4.2,2.5 4.2,5 0,1.3 -0.4,2.5 -1,3.4 -0.7,1 -1.8,1.6 -3.1,1.6 z";

/// The device's size (flipper.svg's view box).
pub const DEVICE: Vec2 = vec2(360.0, 156.0);
/// Where the screen sits in it (DeviceWidget.qml), at one screen pixel per device pixel, as qFlipper.
pub const SCREEN_AT: Vec2 = vec2(93.0, 26.0);

/// Draws the device at `origin` (its top left corner) and returns the screen's rectangle.
pub fn device(p: &Painter, origin: Pos2, pal: &Palette, screen: Screen, recovery: bool) -> Rect {
    let at = |q: Pos2| origin + vec2(q.x - 2.0, q.y);
    let first = |d: &str| svg_path(d).into_iter().next().unwrap_or_default().into_iter().map(at).collect::<Vec<Pos2>>();
    let line = Stroke::new(1.0, pal.accent);

    let body = first(BODY);
    gradient_polygon(
        p,
        &body,
        &[(origin.y, pal.body_top), (origin.y + DEVICE.y * 0.45, pal.body_middle), (origin.y + DEVICE.y, pal.body_bottom)],
    );
    p.add(Shape::closed_line(body, Stroke::new(3.0, pal.accent)));
    p.add(Shape::closed_line(first(INNER), line));
    // the screen's frame and window (g11, g23)
    let frame = Rect::from_min_max(at(pos2(88.5, 19.5)), at(pos2(229.5, 97.5)));
    p.rect_stroke(frame, 6.2, line, StrokeKind::Middle);
    let window = Rect::from_min_max(at(pos2(91.0, 22.0)), at(pos2(227.0, 95.0)));
    p.rect_filled(window, 4.3, pal.accent);
    // the keypad: its outline, the D-pad's ring, OK, the four arrows, back with its arrow, the light
    p.add(Shape::closed_line(first(KEYPAD), line));
    p.circle_stroke(at(pos2(276.5, 58.5)), 35.05, line);
    p.circle_stroke(at(pos2(276.5, 58.5)), 11.0, line);
    for a in ARROWS {
        p.add(Shape::closed_line(first(a), line));
    }
    p.circle_stroke(at(pos2(328.6, 82.5)), 11.05, line);
    p.add(Shape::closed_line(first(BACK_ARROW), line));
    p.circle_stroke(at(pos2(240.5, 94.5)), 3.0, line);
    if recovery {
        // DeviceWidget.qml's blue light
        p.circle_filled(origin + vec2(238.5, 94.5), 4.5, pal.blue);
    }
    // where qFlipper's device has Flipper's wordmark, Argus's name in the title font
    p.text(origin + vec2(193.0, 137.0), Align2::CENTER_CENTER, "ARGUS", fonts::sporty(30.0), pal.accent);

    let lcd = Rect::from_min_size(origin + SCREEN_AT, vec2(128.0, 64.0));
    let ink = pal.dark1;
    let c = lcd.center();
    match screen {
        Screen::Live(tex) => {
            p.image(tex, lcd, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        Screen::Blank => {}
        Screen::Success => {
            let s = Stroke::new(4.0, ink);
            p.line_segment([c + vec2(-18.0, 0.0), c + vec2(-6.0, 12.0)], s);
            p.line_segment([c + vec2(-6.0, 12.0), c + vec2(18.0, -12.0)], s);
        }
        Screen::Warning | Screen::Error => {
            p.text(c, Align2::CENTER_CENTER, "!", fonts::sporty(45.0), ink);
        }
        Screen::Recovery => {
            p.text(c, Align2::CENTER_CENTER, "RECOVERY", fonts::label(), ink);
        }
    }
    lcd
}

// ------------------------------------------------------------------------------------------------- buttons

/// DefaultAmber's button kinds.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// `Button.qml`: filled, bright outline.
    Normal,
    /// `SmallButton.qml`: no fill, dim outline; `highlighted` (the suggested one) is filled with a bright outline.
    Small {
        highlighted: bool,
    },
    /// `SmallButtonRed.qml`.
    Red,
    /// `MainButton.qml`'s green and blue.
    Green,
    Blue,
    /// `LinkButton.qml`: underlined text in a colour.
    Link(Color32),
    /// A footer button that opens something: drawn pressed while it is open.
    Toggle(bool),
}

#[derive(Clone, Copy)]
struct Set {
    normal: Color32,
    hover: Color32,
    down: Color32,
    disabled: Color32,
}

impl Set {
    fn pick(&self, enabled: bool, hovered: bool, down: bool) -> Color32 {
        if !enabled {
            self.disabled
        } else if down {
            self.down
        } else if hovered {
            self.hover
        } else {
            self.normal
        }
    }
}

/// (text, fill, outline) for a kind, as the QML's ColorSets.
fn sets(kind: Kind, p: &Palette) -> (Set, Set, Set) {
    let t = Color32::TRANSPARENT;
    let set = |normal, hover, down, disabled| Set { normal, hover, down, disabled };
    match kind {
        Kind::Normal | Kind::Toggle(_) => (
            set(p.accent, p.light1, p.dark1, p.medium1),
            set(p.dark1, p.medium2, p.accent, p.dark2),
            set(p.accent, p.light1, p.accent, p.medium1),
        ),
        Kind::Small { highlighted } => (
            set(p.accent, p.light1, p.dark1, p.medium1),
            set(if highlighted { p.dark1 } else { t }, p.medium2, p.accent, t),
            set(if highlighted { p.accent } else { p.medium1 }, p.light1, p.accent, p.medium1),
        ),
        Kind::Red => (
            set(p.red, p.red1, p.red_dark1, p.red_medium1),
            set(t, p.red_medium2, p.red, t),
            set(p.red_medium1, p.red1, p.red, p.red_medium1),
        ),
        Kind::Green => (
            set(p.green, p.green, p.green_dark, p.medium1),
            set(p.green_medium2, p.green_medium1, p.green, p.dark2),
            set(p.green, p.green, p.green, p.medium1),
        ),
        Kind::Blue => (
            set(p.blue, p.blue, p.blue_dark1, p.medium1),
            set(p.blue_dark2, p.blue_dark1, p.blue, p.dark2),
            set(p.blue, p.blue, p.blue, p.medium1),
        ),
        Kind::Link(c) => (set(c, c.lerp_to_gamma(Color32::WHITE, 0.2), c, p.medium1), set(t, t, t, t), set(t, t, t, t)),
    }
}

/// A qFlipper button: its label is what assistive technology (and the tests) read, its text is drawn in capitals
/// (links keep their case), with an optional icon before it.
pub struct QButton<'a> {
    label: &'a str,
    kind: Kind,
    pal: &'a Palette,
    size: Vec2,
    font: FontId,
    icon: Option<&'static Icon>,
    text: bool,
    radius: f32,
    border: f32,
}

impl<'a> QButton<'a> {
    pub fn new(label: &'a str, kind: Kind, pal: &'a Palette, size: Vec2) -> QButton<'a> {
        QButton { label, kind, pal, size, font: fonts::label(), icon: None, text: true, radius: 5.0, border: 2.0 }
    }

    /// As wide as its text and icon, with Button.qml's padding (8 px on each side).
    pub fn fit(label: &'a str, kind: Kind, pal: &'a Palette, height: f32) -> QButton<'a> {
        QButton::new(label, kind, pal, vec2(0.0, height))
    }

    /// `MainButton.qml`: 280 x 56, radius 7, a 3 px outline, the title font.
    pub fn main(label: &'a str, kind: Kind, pal: &'a Palette) -> QButton<'a> {
        QButton { font: fonts::sporty(fonts::TITLE), radius: 7.0, border: 3.0, ..QButton::new(label, kind, pal, vec2(280.0, 56.0)) }
    }

    pub fn icon(mut self, icon: &'static Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Only the icon is drawn; the label is still what assistive technology reads.
    pub fn icon_only(mut self) -> Self {
        self.text = false;
        self
    }

    pub fn radius(mut self, r: f32) -> Self {
        self.radius = r;
        self
    }
}

impl egui::Widget for QButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        // its id is its label's, not the next automatic one: overlays come and go around it, and a press and its
        // release must meet the same button
        let link = matches!(self.kind, Kind::Link(_));
        let text = match (self.text, link) {
            (false, _) => String::new(),
            (true, true) => self.label.to_string(),
            (true, false) => fonts::pixel_text(self.label),
        };
        let galley = ui.painter().layout_no_wrap(text, self.font.clone(), Color32::PLACEHOLDER);
        let gap = if self.text { 6.0 } else { 0.0 };
        let icon_w = self.icon.map_or(0.0, |i| i.w as f32 + gap);
        let width = icon_w + galley.size().x;
        let size = if self.size.x > 0.0 { self.size } else { vec2((width + 16.0).ceil(), self.size.y) };
        let (_, rect) = ui.allocate_space(size);
        let resp = ui.interact(rect, ui.id().with(("button", self.label)), Sense::click());
        let enabled = ui.is_enabled();
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, self.label));
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let hovered = resp.hovered();
        let down = resp.is_pointer_button_down_on() || self.kind == Kind::Toggle(true);
        let (fg, bg, stroke) = sets(self.kind, self.pal);
        let (fg, bg, stroke) = (fg.pick(enabled, hovered, down), bg.pick(enabled, hovered, down), stroke.pick(enabled, hovered, down));
        let p = ui.painter();
        if !link {
            p.rect_filled(rect, self.radius, bg);
            p.rect_stroke(rect, self.radius, Stroke::new(self.border, stroke), StrokeKind::Inside);
        }
        let left = (rect.center().x - width / 2.0).round();
        if let Some(i) = self.icon {
            i.draw(p, pos2(left + i.w as f32 / 2.0, rect.center().y), fg);
        }
        let pos = pos2(left + icon_w, (rect.center().y - galley.size().y / 2.0).round());
        let text_rect = Rect::from_min_size(pos, galley.size());
        p.galley(pos, galley, fg);
        if link {
            let y = text_rect.bottom() - 1.0;
            p.line_segment([pos2(text_rect.left(), y), pos2(text_rect.right(), y)], Stroke::new(2.0, fg));
        }
        resp
    }
}

/// A link's size for its text (LinkButton.qml: no padding).
pub fn link_size(ui: &Ui, text: &str) -> Vec2 {
    ui.painter().layout_no_wrap(text.to_string(), fonts::label(), Color32::WHITE).size() + vec2(0.0, 4.0)
}

/// `TabButton.qml`: 47 x 41, the top corners rounded, an icon; filled while it is the current tab.
pub fn tab(ui: &mut Ui, rect: Rect, icon: &'static Icon, label: &str, current: bool, enabled: bool, pal: &Palette) -> Response {
    let resp = ui.interact(rect, ui.id().with(("tab", label)), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, current, label));
    let (fg, bg, stroke) = sets(Kind::Normal, pal);
    let down = current || resp.is_pointer_button_down_on();
    let hovered = resp.hovered();
    let (fg, bg, stroke) = (fg.pick(enabled, hovered, down), bg.pick(enabled, hovered, down), stroke.pick(enabled, hovered, down));
    let p = ui.painter();
    let r = egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 };
    p.rect_filled(rect, r, bg);
    p.rect_stroke(rect, r, Stroke::new(2.0, stroke), StrokeKind::Inside);
    icon.draw(p, rect.center(), fg);
    resp
}

/// `TabPane.qml`'s outline: solid at the top (square on the left, where the tabs are) and at the bottom, dotted at
/// half strength on the sides, filled with `fill`.
pub fn pane(p: &Painter, r: Rect, fill: Color32, pal: &Palette) {
    let radius = 7.0;
    let w = 2.0;
    p.rect_filled(r, egui::CornerRadius { nw: 0, ne: 7, sw: 7, se: 7 }, fill);
    let solid = Stroke::new(w, pal.accent);
    let cap = radius + w;
    // the top: a square corner on the left, a round one on the right
    let top = Rect::from_min_size(r.min, vec2(r.width(), cap));
    p.with_clip_rect(top).rect_stroke(
        Rect::from_min_size(r.min, vec2(r.width(), cap * 2.0)),
        egui::CornerRadius { nw: 0, ne: 7, sw: 0, se: 0 },
        solid,
        StrokeKind::Inside,
    );
    let bottom = Rect::from_min_max(pos2(r.left(), r.bottom() - cap), r.max);
    p.with_clip_rect(bottom).rect_stroke(
        Rect::from_min_max(pos2(r.left(), r.bottom() - cap * 2.0), r.max),
        radius,
        solid,
        StrokeKind::Inside,
    );
    let dim = pal.accent.gamma_multiply(0.5);
    for x in [r.left() + w / 2.0, r.right() - w / 2.0] {
        dotted(p, pos2(x, r.top() + cap), pos2(x, r.bottom() - cap), w, 2.0, 3.0, dim);
    }
}

// ------------------------------------------------------------------------------------------------- the rest

/// `typec.svg`: the USB-C plug and its cable, with the arrow pointing at the device; `at` is its top left corner.
pub fn plug(p: &Painter, at: Pos2, pal: &Palette) {
    let s = Stroke::new(2.0, pal.accent);
    let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(at + vec2(x, y), vec2(w, h));
    p.rect_stroke(r(1.0, 7.0, 4.0, 24.0), 2.0, s, StrokeKind::Middle);
    p.rect_stroke(r(1.0, 7.0, 20.0, 24.0), egui::CornerRadius { nw: 2, ne: 0, sw: 2, se: 0 }, s, StrokeKind::Middle);
    p.rect_stroke(r(22.0, 1.0, 50.0, 35.0), 2.0, s, StrokeKind::Middle);
    p.add(Shape::line(vec![at + vec2(159.0, 13.0), at + vec2(72.0, 13.0), at + vec2(72.0, 23.0), at + vec2(159.0, 23.0)], s));
    p.line_segment([at + vec2(35.0, 18.0), at + vec2(61.0, 18.0)], s);
    p.add(Shape::line(vec![at + vec2(41.4, 11.6), at + vec2(35.0, 18.0), at + vec2(41.4, 24.4)], s));
}

/// `spinner.svg`: a dim ring and a bright quarter of it, turned by `angle`.
pub fn spinner(p: &Painter, c: Pos2, angle: f32, pal: &Palette) {
    p.circle_stroke(c, 10.0, Stroke::new(4.0, pal.medium3));
    let arc: Vec<Pos2> = (0..=12)
        .map(|k| {
            let a = angle - std::f32::consts::FRAC_PI_2 + k as f32 / 12.0 * std::f32::consts::FRAC_PI_2;
            c + vec2(a.cos(), a.sin()) * 10.0
        })
        .collect();
    p.add(Shape::line(arc, Stroke::new(4.0, pal.accent)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_data_is_read_as_svg_reads_it() {
        let p = svg_path("M 1,2 h 3 v 4 L 0,0 z m 10,10 l 1-1 1.5.5");
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], vec![pos2(1.0, 2.0), pos2(4.0, 2.0), pos2(4.0, 6.0), pos2(0.0, 0.0), pos2(1.0, 2.0)]);
        assert_eq!(p[1], vec![pos2(11.0, 12.0), pos2(12.0, 11.0), pos2(13.5, 11.5)]);
        let c = svg_path("M0 0C0 10 10 10 10 0");
        assert_eq!(c[0].len(), 13);
        assert_eq!(*c[0].last().unwrap(), pos2(10.0, 0.0));
        assert!((c[0][6].y - 7.5).abs() < 1e-4, "the curve's middle");
    }

    #[test]
    fn the_device_outline_is_flipper_svgs() {
        // the body's outline spans the view box, the keypad's sits right of the screen
        let body = &svg_path(BODY)[0];
        let (minx, maxx) = body.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.x), b.max(p.x)));
        let (miny, maxy) = body.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.y), b.max(p.y)));
        assert!(minx > 3.0 && minx < 6.0 && maxx > 360.0 && maxx < 361.0, "{minx} {maxx}");
        assert!(miny > 1.0 && miny < 1.5 && maxy > 155.0 && maxy < 155.5, "{miny} {maxy}");
        for d in ARROWS.iter().chain([&INNER, &KEYPAD, &BACK_ARROW]) {
            let first = &svg_path(d)[0];
            assert!(first.len() > 3 && first[0].distance(*first.last().unwrap()) < 0.2, "closed: {d}");
        }
    }
}
