//! Running the app headless against the fake Flipper.
//!
//! - [`Harness`] drives `ArgusApp::ui` frame by frame with input it makes up (pointer moves and clicks, keys, text), on
//!   a clock of its own, and finds widgets through the accessibility tree egui builds each frame (role, label, bounds,
//!   disabled), so a test clicks "Delete…" the way a person would, never by calling the app's methods.
//! - [`render`] rasterises egui's own output (the tessellated meshes and textures the GPU would get) in software, so
//!   key screens can be compared with pictures in `tests/snapshots/` without a GPU, a window or a compositor.
//! - [`Fake`] serves `flipper_fake::FakeFlipper` on a Unix socket, session after session, and records every request as
//!   it arrives, so a test can check that nothing changed the device before a dialog was confirmed.
//!
//! Nothing here opens a real device: the app is handed the fake's socket as its only device.
#![allow(dead_code)]

use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argus_app::discovery::{Discovery, Fixed, Found, Kind};
use argus_app::net::Net;
use argus_app::{theme, ArgusApp, Config};
use eframe::egui::{self, accesskit, pos2, vec2, Color32, ColorImage, Event, Key, Modifiers, PointerButton, Pos2, Rect, TextureId};
use flipper_fake::FakeFlipper;
use flipper_rpc::frame::FrameDecoder;
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::Main;

pub fn tempdir(name: &str) -> PathBuf {
    // short and per test (socket paths are limited to 107 bytes)
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("app-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------- the fake on a socket

struct Recording<'a> {
    stream: std::os::unix::net::UnixStream,
    wire: &'a Mutex<Vec<u8>>,
}

impl std::io::Read for Recording<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.stream.read(buf)?;
        self.wire.lock().unwrap().extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

impl std::io::Write for Recording<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

/// What the app sent, one entry per session.
type Wires = Arc<Mutex<Vec<Arc<Mutex<Vec<u8>>>>>>;
/// The fake's storage (`FakeFlipper::dump`) and inputs after each session.
type Ends = Arc<Mutex<Vec<(String, Vec<(i32, i32)>)>>>;

pub struct Fake {
    pub sock: PathBuf,
    wires: Wires,
    ends: Ends,
}

impl Fake {
    pub fn start(dir: &Path, fake: FakeFlipper) -> Fake {
        Fake::start_with(dir, fake, |_| {})
    }

    /// As [`Fake::start`], with `ended` hearing each session's transcript as it ends (a test's device list follows a
    /// reboot into the bootloader with it).
    pub fn start_with(dir: &Path, fake: FakeFlipper, mut ended: impl FnMut(&flipper_fake::Transcript) + Send + 'static) -> Fake {
        let sock = dir.join("flipper.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let wires: Wires = Arc::default();
        let ends: Ends = Arc::default();
        let (w, e) = (wires.clone(), ends.clone());
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                let wire = Arc::new(Mutex::new(Vec::new()));
                w.lock().unwrap().push(wire.clone());
                let t = fake.serve(&mut Recording { stream, wire: &wire });
                e.lock().unwrap().push((fake.dump(), fake.inputs.borrow().clone()));
                if let Ok(t) = t {
                    ended(&t);
                }
            }
        });
        Fake { sock, wires, ends }
    }

    /// Every request the app sent so far, all sessions, in order.
    pub fn requests(&self) -> Vec<Main> {
        let mut out = Vec::new();
        for wire in self.wires.lock().unwrap().iter() {
            let bytes = wire.lock().unwrap().clone();
            let marker = b"start_rpc_session\r";
            let Some(start) = bytes.windows(marker.len()).position(|w| w == marker) else { continue };
            let mut d = FrameDecoder::new();
            for chunk in bytes[start + marker.len()..].chunks(4096) {
                d.push(chunk).unwrap();
                while let Ok(Some(m)) = d.next_frame() {
                    out.push(m);
                }
            }
        }
        out
    }

    /// The requests that change the device (storage writes, folders, deletes, renames, reboots, resets, updates).
    pub fn changes(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter(|m| {
                matches!(
                    m.content,
                    Some(
                        Content::StorageWriteRequest(_)
                            | Content::StorageMkdirRequest(_)
                            | Content::StorageDeleteRequest(_)
                            | Content::StorageRenameRequest(_)
                            | Content::SystemRebootRequest(_)
                            | Content::SystemFactoryResetRequest(_)
                            | Content::SystemUpdateRequest(_)
                    )
                )
            })
            .map(flipper_fake::describe)
            .collect()
    }

    pub fn sessions(&self) -> usize {
        self.wires.lock().unwrap().len()
    }

    /// The fake's storage after the last session that ended.
    pub fn last_dump(&self) -> Option<String> {
        self.ends.lock().unwrap().last().map(|e| e.0.clone())
    }

    pub fn wait_ended(&self, sessions: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.ends.lock().unwrap().len() < sessions {
            assert!(Instant::now() < deadline, "the session did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

// ---------------------------------------------------------------------------- the app, headless

pub struct Options {
    pub check_updates: bool,
    pub net: Option<Net>,
    pub dark: bool,
    pub settings: String,
}

impl Default for Options {
    fn default() -> Options {
        Options { check_updates: false, net: None, dark: true, settings: String::new() }
    }
}

pub struct Harness {
    pub app: ArgusApp,
    pub ctx: egui::Context,
    pub size: egui::Vec2,
    pub time: f64,
    pub dir: PathBuf,
    events: Vec<Event>,
    nodes: Vec<(accesskit::NodeId, accesskit::Node)>,
    textures: HashMap<TextureId, ColorImage>,
    shapes: Vec<egui::epaint::ClippedShape>,
    pub viewport_commands: Vec<egui::ViewportCommand>,
}

#[derive(Clone, Debug)]
pub struct Widget {
    pub role: accesskit::Role,
    pub label: String,
    pub value: String,
    pub rect: Rect,
    pub enabled: bool,
    pub selected: bool,
}

impl Harness {
    /// The app with `devices` as its only devices, its files in `dir`, a fixed dark style.
    pub fn new(dir: &Path, devices: Vec<Found>, opts: Options) -> Harness {
        Harness::with_discovery(dir, Box::new(Fixed(devices)), opts)
    }

    /// The app with a discovery of the test's own (devices that come and go).
    pub fn with_discovery(dir: &Path, discovery: Box<dyn Discovery>, opts: Options) -> Harness {
        let config = dir.join("config");
        std::fs::create_dir_all(&config).unwrap();
        let mut settings = format!("check_updates = {}\n", opts.check_updates);
        settings.push_str(&opts.settings);
        std::fs::write(config.join(argus_app::settings::FILE), settings).unwrap();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cfg = Config {
            discovery,
            config_dir: config,
            datastore: dir.join("data/chymaera.db"),
            library_dir: dir.join("data/captures"),
            // an address nothing listens on: a test that did not start an update server must not reach any
            net: opts.net.unwrap_or_else(|| Net::new("https://127.0.0.1:9".into(), None)),
            theme: Some(theme::Source::Fixed { dark: opts.dark, accent: None }),
            yggdrasil_config: None,
            timeout: Duration::from_secs(2),
            home,
            api_token: "test-token".into(),
            recovery_poll: Duration::from_millis(5),
            // Limbo is never reached: its socket here does not exist
            tools: argus_app::tools::Dirs {
                data: dir.join("data/argus"),
                pulls: dir.join("data/pulls"),
                limbo_socket: dir.join("limbo.sock"),
            },
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        ctx.all_styles_mut(|s| {
            s.animation_time = 0.0;
            s.visuals.text_cursor.blink = false;
        });
        Harness {
            app: ArgusApp::new(cfg),
            ctx,
            size: vec2(argus_app::app::WINDOW[0], argus_app::app::WINDOW[1]),
            time: 0.0,
            dir: dir.to_path_buf(),
            events: Vec::new(),
            nodes: Vec::new(),
            textures: HashMap::new(),
            shapes: Vec::new(),
            viewport_commands: Vec::new(),
        }
    }

    /// One frame, 1/60 s after the last.
    pub fn step(&mut self) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            predicted_dt: 1.0 / 60.0,
            focused: true,
            events: std::mem::take(&mut self.events),
            ..Default::default()
        };
        let app = &mut self.app;
        let mut out = self.ctx.run_ui(input, |ui| app.ui(ui));
        self.time += 1.0 / 60.0;
        let mut delta = std::mem::take(&mut out.textures_delta);
        for (id, deltas) in &delta.set {
            for delta in deltas {
                let egui::ImageData::Color(img) = &delta.image;
                match delta.pos {
                    None => {
                        self.textures.insert(*id, (**img).clone());
                    }
                    Some([x0, y0]) => {
                        let t = self.textures.get_mut(id).expect("a patch for a known texture");
                        for y in 0..img.size[1] {
                            for x in 0..img.size[0] {
                                t.pixels[(y0 + y) * t.size[0] + x0 + x] = img.pixels[y * img.size[0] + x];
                            }
                        }
                    }
                }
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
        delta.clear();
        if let Some(u) = out.platform_output.accesskit_update {
            self.nodes = u.nodes;
        }
        for (_, v) in out.viewport_output {
            self.viewport_commands.extend(v.commands);
        }
        self.shapes = out.shapes;
    }

    /// Runs frames (with a little real time between them, for the device worker) until `cond` holds.
    pub fn wait(&mut self, what: &str, cond: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            self.step();
            if cond(self) {
                return;
            }
            if Instant::now() > deadline {
                let labels: Vec<String> = self.widgets().iter().map(|w| format!("{:?} {:?}", w.role, w.label)).collect();
                panic!("waited in vain for {what}; on screen: {labels:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Lets every animation finish (the longest is the log drawer's 500 ms).
    pub fn settle(&mut self) {
        for _ in 0..45 {
            self.step();
        }
    }

    pub fn widgets(&self) -> Vec<Widget> {
        self.nodes
            .iter()
            .filter_map(|(_, n)| {
                let b = n.bounds()?;
                // egui puts a label's text in its value, a button's in its label
                let label = match n.role() {
                    accesskit::Role::Label => n.value(),
                    _ => n.label(),
                };
                Some(Widget {
                    role: n.role(),
                    label: label.unwrap_or_default().to_string(),
                    value: n.value().unwrap_or_default().to_string(),
                    rect: Rect::from_min_max(pos2(b.x0 as f32, b.y0 as f32), pos2(b.x1 as f32, b.y1 as f32)),
                    enabled: !n.is_disabled(),
                    selected: n.toggled() == Some(accesskit::Toggled::True),
                })
            })
            .collect()
    }

    /// The confirmation dialog's area while it is open: only what is inside it can be clicked then.
    fn modal(&self) -> Option<Rect> {
        if self.app.picker_open() {
            return self.ctx.memory(|m| m.area_rect(egui::Id::new("picker")));
        }
        self.app.dialog().and_then(|_| self.ctx.memory(|m| m.area_rect(egui::Id::new("confirm"))))
    }

    /// The enabled widget labelled exactly `label` that a click can reach (inside the open modal, if any).
    pub fn find(&self, label: &str) -> Option<Widget> {
        let modal = self.modal();
        self.widgets().into_iter().rfind(|w| w.label == label && w.enabled && modal.is_none_or(|m| m.contains_rect(w.rect)))
    }

    pub fn has(&self, label: &str) -> bool {
        self.find(label).is_some()
    }

    /// Whether any widget (enabled or not) is labelled `label`.
    pub fn shows(&self, label: &str) -> bool {
        self.widgets().iter().any(|w| w.label == label)
    }

    pub fn shows_text(&self, part: &str) -> bool {
        self.widgets().iter().any(|w| w.label.contains(part) || w.value.contains(part))
    }

    pub fn is_enabled(&self, label: &str) -> bool {
        let all: Vec<Widget> = self.widgets().into_iter().filter(|w| w.label == label).collect();
        assert!(!all.is_empty(), "no widget labelled {label:?}");
        all.iter().any(|w| w.enabled)
    }

    pub fn push_event(&mut self, e: Event) {
        self.events.push(e);
    }

    fn pointer(&mut self, p: Pos2, pressed: bool) {
        self.events.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }

    pub fn click_at(&mut self, p: Pos2) {
        self.events.push(Event::PointerMoved(p));
        self.step();
        self.pointer(p, true);
        self.step();
        self.pointer(p, false);
        self.step();
    }

    /// Two frames, so what opened last frame (a dialog lays itself out invisibly first) is where it will stay.
    fn layout(&mut self) {
        self.step();
        self.step();
    }

    /// Clicks the enabled widget labelled `label`; fails if there is none.
    pub fn click(&mut self, label: &str) {
        self.layout();
        let w = self.find(label).unwrap_or_else(|| {
            let shown = self.shows(label);
            panic!("no enabled {label:?} to click (shown but disabled: {shown})")
        });
        self.click_at(w.rect.center());
    }

    pub fn double_click(&mut self, label: &str) {
        // a second on the clock first, so an earlier click does not make this a triple click
        self.time += 1.0;
        self.layout();
        let w = self.find(label).unwrap_or_else(|| panic!("no enabled {label:?} to double-click"));
        let p = w.rect.center();
        self.click_at(p);
        self.pointer(p, true);
        self.step();
        self.pointer(p, false);
        self.step();
    }

    /// Presses the pointer on `label` and holds it.
    pub fn press(&mut self, label: &str) -> Pos2 {
        self.layout();
        let p = self.find(label).unwrap_or_else(|| panic!("no {label:?} to press")).rect.center();
        self.events.push(Event::PointerMoved(p));
        self.step();
        self.pointer(p, true);
        self.step();
        p
    }

    pub fn release(&mut self, p: Pos2) {
        self.pointer(p, false);
        self.step();
    }

    pub fn key(&mut self, key: Key, pressed: bool) {
        self.events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
        self.step();
    }

    /// The text fields, top to bottom (the accessibility tree's order is not the drawing order).
    fn fields(&self) -> Vec<Widget> {
        let mut fields: Vec<Widget> = self.widgets().into_iter().filter(|w| w.role == accesskit::Role::TextInput).collect();
        fields.sort_by(|a, b| a.rect.min.y.total_cmp(&b.rect.min.y));
        fields
    }

    /// Replaces the text of text field number `n`, counted from the top.
    pub fn set_field(&mut self, n: usize, text: &str) {
        self.layout();
        let fields = self.fields();
        let f = fields.get(n).unwrap_or_else(|| panic!("no text field {n} (there are {})", fields.len())).clone();
        self.click_at(f.rect.center());
        self.events.push(Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND });
        self.step();
        self.events.push(Event::Text(text.into()));
        self.step();
    }

    pub fn field_value(&self, n: usize) -> String {
        self.fields().into_iter().nth(n).map(|w| w.value).unwrap_or_default()
    }

    /// The window as the GPU would draw it.
    pub fn render(&self) -> ColorImage {
        let prims = self.ctx.tessellate(self.shapes.clone(), 1.0);
        render(&prims, &self.textures, self.size.x as usize, self.size.y as usize)
    }

    /// Compares the window with `tests/snapshots/NAME.png`. `ARGUS_UPDATE_SNAPSHOTS=1` writes it instead; a missing
    /// snapshot fails (so CI never passes on a picture it made itself). The picture of a failure is kept next to the
    /// test's files.
    pub fn snapshot(&self, name: &str) {
        let img = self.render();
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(format!("{name}.png"));
        if std::env::var_os("ARGUS_UPDATE_SNAPSHOTS").is_some() {
            write_png(&file, &img);
            return;
        }
        let want =
            read_png(&file).unwrap_or_else(|| panic!("no snapshot {} (run with ARGUS_UPDATE_SNAPSHOTS=1 to make it)", file.display()));
        assert_eq!(want.size, img.size, "{name}: size");
        let differ = want.pixels.iter().zip(&img.pixels).filter(|(a, b)| {
            let (a, b) = (a.to_array(), b.to_array());
            (0..3).any(|i| a[i].abs_diff(b[i]) > 24)
        });
        let n = differ.count();
        if n > 100 {
            let got = self.dir.join(format!("{name}-got.png"));
            write_png(&got, &img);
            panic!("{name}: {n} pixels differ from the snapshot; this run's picture is {}", got.display());
        }
    }
}

pub fn socket_device(sock: &Path) -> Found {
    Found { kind: Kind::Socket(sock.to_path_buf()) }
}

// ---------------------------------------------------------------------------- software rendering

/// Draws egui's meshes the way its GPU back ends do (premultiplied alpha, blended in sRGB), onto a black window.
pub fn render(prims: &[egui::ClippedPrimitive], textures: &HashMap<TextureId, ColorImage>, w: usize, h: usize) -> ColorImage {
    let mut px = vec![[0f32, 0.0, 0.0, 1.0]; w * h];
    for p in prims {
        let egui::epaint::Primitive::Mesh(mesh) = &p.primitive else { continue };
        let Some(tex) = textures.get(&mesh.texture_id) else { continue };
        let clip = p.clip_rect.intersect(Rect::from_min_size(Pos2::ZERO, vec2(w as f32, h as f32)));
        if clip.width() <= 0.0 || clip.height() <= 0.0 {
            continue;
        }
        let (cx0, cy0) = (clip.min.x.round().max(0.0) as usize, clip.min.y.round().max(0.0) as usize);
        let (cx1, cy1) = ((clip.max.x.round() as usize).min(w), (clip.max.y.round() as usize).min(h));
        for tri in mesh.indices.as_chunks::<3>().0 {
            let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
            let area = edge(v[0].pos, v[1].pos, v[2].pos);
            if area.abs() < 1e-9 {
                continue;
            }
            let minx = v.iter().map(|v| v.pos.x).fold(f32::INFINITY, f32::min).floor().max(cx0 as f32) as usize;
            let maxx = (v.iter().map(|v| v.pos.x).fold(f32::NEG_INFINITY, f32::max).ceil() as usize).min(cx1);
            let miny = v.iter().map(|v| v.pos.y).fold(f32::INFINITY, f32::min).floor().max(cy0 as f32) as usize;
            let maxy = (v.iter().map(|v| v.pos.y).fold(f32::NEG_INFINITY, f32::max).ceil() as usize).min(cy1);
            for y in miny..maxy {
                for x in minx..maxx {
                    let p = pos2(x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = edge(v[1].pos, v[2].pos, p) / area;
                    let w1 = edge(v[2].pos, v[0].pos, p) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let lerp = |f: &dyn Fn(usize) -> f32| w0 * f(0) + w1 * f(1) + w2 * f(2);
                    let u = lerp(&|i| v[i].uv.x);
                    let vv = lerp(&|i| v[i].uv.y);
                    let tx = ((u * tex.size[0] as f32) as usize).min(tex.size[0] - 1);
                    let ty = ((vv * tex.size[1] as f32) as usize).min(tex.size[1] - 1);
                    let t = tex.pixels[ty * tex.size[0] + tx].to_array();
                    let mut src = [0f32; 4];
                    for (c, s) in src.iter_mut().enumerate() {
                        *s = lerp(&|i| v[i].color.to_array()[c] as f32 / 255.0) * t[c] as f32 / 255.0;
                    }
                    let d = &mut px[y * w + x];
                    for c in 0..4 {
                        d[c] = src[c] + d[c] * (1.0 - src[3]);
                    }
                }
            }
        }
    }
    let pixels = px
        .iter()
        .map(|p| Color32::from_rgb((p[0] * 255.0).round() as u8, (p[1] * 255.0).round() as u8, (p[2] * 255.0).round() as u8))
        .collect();
    ColorImage::new([w, h], pixels)
}

fn edge(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

pub fn write_png(path: &Path, img: &ColorImage) {
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    let data: Vec<u8> = img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]).collect();
    w.write_image_data(&data).unwrap();
}

pub fn read_png(path: &Path) -> Option<ColorImage> {
    let f = std::fs::File::open(path).ok()?;
    let mut r = png::Decoder::new(std::io::BufReader::new(f)).read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let data = &buf[..info.buffer_size()];
    let pixels = data.as_chunks::<3>().0.iter().map(|c| Color32::from_rgb(c[0], c[1], c[2])).collect();
    Some(ColorImage::new([info.width as usize, info.height as usize], pixels))
}

/// Devices the test changes while the app runs (a chip that leaves its bootloader and comes back as its firmware).
#[derive(Clone, Default)]
pub struct Shared(pub Arc<Mutex<Vec<Found>>>);

impl Shared {
    pub fn set(&self, list: Vec<Found>) {
        *self.0.lock().unwrap() = list;
    }
}

impl Discovery for Shared {
    fn scan(&self) -> Vec<Found> {
        self.0.lock().unwrap().clone()
    }
}
