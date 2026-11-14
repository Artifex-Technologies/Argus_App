//! A small file picker of the app's own, for the dialogs' path fields and the tools' package field: folders and files
//! of one folder at a time, read with `read_dir` only (nothing is opened), dot files hidden. "Choose" takes the
//! selected file; for a field that names a new file, "Save here" keeps the name and takes the folder shown. It is drawn
//! as qFlipper's dialogs and file manager are, so it works the same on any desktop and in the headless tests.
//! (Yggdrasil's own chooser, Minos, is reached through the FileChooser portal; that needs a D-Bus client the app does
//! not carry yet.)

use std::path::{Path, PathBuf};

use eframe::egui::{self, vec2, Color32, Id, RichText, Sense, WidgetInfo, WidgetType};

use super::draw::{Kind, QButton};
use super::icons;

use super::{ArgusApp, FieldKind};
use crate::fonts;
use crate::theme::Palette;

/// Where a picked path goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickFor {
    /// Field `n` of the open confirmation dialog.
    Field(usize),
    /// The tools' package field.
    Package,
}

pub struct Picker {
    pub to: PickFor,
    pub save: bool,
    pub dir: PathBuf,
    /// The file name a save keeps.
    pub name: String,
    pub entries: Vec<(String, bool)>,
    pub selected: Option<String>,
    pub error: Option<String>,
}

/// Folders first, then files, each by name without case; dot files left out.
fn read(dir: &Path) -> Result<Vec<(String, bool)>, String> {
    let mut out: Vec<(String, bool)> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if name.starts_with('.') {
                return None;
            }
            // a link to a folder counts as a folder (metadata follows it)
            let dir = std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false);
            Some((name, dir))
        })
        .collect();
    out.sort_by_key(|a| (!a.1, a.0.to_lowercase()));
    Ok(out)
}

impl Picker {
    fn new(to: PickFor, save: bool, current: &str, home: &Path) -> Picker {
        let p = PathBuf::from(current.trim());
        let (dir, name) = if p.is_dir() {
            (p, String::new())
        } else {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (p.parent().filter(|d| d.is_dir()).map(Path::to_path_buf).unwrap_or_else(|| home.to_path_buf()), name)
        };
        let mut picker = Picker { to, save, dir: PathBuf::new(), name, entries: Vec::new(), selected: None, error: None };
        picker.go(dir);
        picker
    }

    fn go(&mut self, dir: PathBuf) {
        match read(&dir) {
            Ok(e) => {
                self.entries = e;
                self.dir = dir;
                self.selected = None;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl ArgusApp {
    pub fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    pub(super) fn open_picker(&mut self, to: PickFor) {
        let (current, save) = match to {
            PickFor::Field(i) => match self.dialog.as_ref().and_then(|d| d.fields.get(i)) {
                Some(f) => (f.value.clone(), f.kind == FieldKind::Save),
                None => return,
            },
            PickFor::Package => (self.tools.package.clone(), false),
        };
        self.picker = Some(Picker::new(to, save, &current, &self.cfg.home));
    }

    fn picked(&mut self, to: PickFor, path: PathBuf) {
        let text = path.display().to_string();
        match to {
            PickFor::Field(i) => {
                if let Some(f) = self.dialog.as_mut().and_then(|d| d.fields.get_mut(i)) {
                    f.value = text;
                }
            }
            PickFor::Package => self.tools.package = text,
        }
    }

    pub(super) fn picker_ui(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(mut p) = self.picker.take() else { return };
        let mut done: Option<Option<PathBuf>> = None;
        let resp = super::modal(Id::new("picker"), pal).show(ctx, |ui| {
            ui.set_width(520.0);
            super::dialog_header(ui, if p.save { "Choose where to save" } else { "Choose a file" }, pal);
            egui::Frame::NONE.inner_margin(16).show(ui, |ui| {
                ui.set_width(520.0 - 32.0);
                let small = Kind::Small { highlighted: false };
                ui.horizontal(|ui| {
                    if ui.add_enabled(p.dir.parent().is_some(), QButton::fit("Up", small, pal, 28.0)).clicked() {
                        if let Some(up) = p.dir.parent().map(Path::to_path_buf) {
                            p.go(up);
                        }
                    }
                    if ui.add(QButton::fit("Home", small, pal, 28.0)).clicked() {
                        p.go(self.cfg.home.clone());
                    }
                    // under the home folder as ~/...
                    let shown = match p.dir.strip_prefix(&self.cfg.home) {
                        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
                        Ok(rest) => format!("~/{}", rest.display()),
                        Err(_) => p.dir.display().to_string(),
                    };
                    ui.label(RichText::new(shown).font(fonts::share(16.0)).color(pal.accent));
                });
                let mut open = None;
                egui::ScrollArea::vertical().id_salt("picker-list").max_height(220.0).auto_shrink([false, false]).show(ui, |ui| {
                    for (name, dir) in &p.entries {
                        let sel = p.selected.as_deref() == Some(name.as_str());
                        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                        r.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, sel, name));
                        // as the file manager's rows: the selected one filled with the accent
                        let (fill, ink) = if sel {
                            (Some(pal.accent), pal.dark1)
                        } else if r.hovered() {
                            (Some(pal.medium2), pal.light1)
                        } else {
                            (None::<Color32>, pal.accent)
                        };
                        if let Some(f) = fill {
                            ui.painter().rect_filled(rect, 3.0, f);
                        }
                        let icon = if *dir { &icons::FOLDER } else { &icons::FILE_SMALL };
                        icon.draw(ui.painter(), rect.left_center() + vec2(14.0, 0.0), ink);
                        ui.painter().text(rect.left_center() + vec2(30.0, 0.0), egui::Align2::LEFT_CENTER, name, fonts::share(16.0), ink);
                        if r.clicked() {
                            p.selected = Some(name.clone());
                            if !*dir && p.save {
                                p.name = name.clone();
                            }
                        }
                        if r.double_clicked() {
                            open = Some((name.clone(), *dir));
                        }
                    }
                    if p.entries.is_empty() {
                        ui.label(RichText::new("Empty").color(pal.medium4));
                    }
                });
                if let Some((name, dir)) = open {
                    if dir {
                        p.go(p.dir.join(name));
                    } else if !p.save {
                        done = Some(Some(p.dir.join(name)));
                    }
                }
                if p.save {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut p.name).id(Id::new("picker-name")).desired_width(400.0));
                    });
                }
                if let Some(e) = &p.error {
                    ui.label(RichText::new(e).color(pal.red));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let sel_dir = p.selected.as_ref().and_then(|s| p.entries.iter().find(|(n, _)| n == s)).map(|(_, d)| *d);
                    let main = Kind::Small { highlighted: true };
                    if p.save {
                        let name_ok = !p.name.trim().is_empty() && !p.name.contains('/');
                        if ui.add_enabled(name_ok, QButton::fit("Save here", main, pal, 34.0)).clicked() {
                            done = Some(Some(p.dir.join(p.name.trim())));
                        }
                    } else if ui.add_enabled(sel_dir == Some(false), QButton::fit("Choose", main, pal, 34.0)).clicked() {
                        done = p.selected.clone().map(|n| Some(p.dir.join(n)));
                    }
                    if ui.add_enabled(sel_dir == Some(true), QButton::fit("Open folder", small, pal, 34.0)).clicked() {
                        if let Some(n) = p.selected.clone() {
                            p.go(p.dir.join(n));
                        }
                    }
                    if ui.add(QButton::fit("Cancel", small, pal, 34.0)).clicked() {
                        done = Some(None);
                    }
                });
            });
        });
        if resp.should_close() && done.is_none() {
            done = Some(None);
        }
        match done {
            Some(Some(path)) => self.picked(p.to, path),
            Some(None) => {}
            None => self.picker = Some(p),
        }
    }
}
