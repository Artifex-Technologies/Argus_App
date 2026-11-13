//! A small file picker of the app's own, for the dialogs' path fields and the tools' package field: folders and files
//! of one folder at a time, read with `read_dir` only (nothing is opened), dot files hidden. "Choose" takes the
//! selected file; for a field that names a new file, "Save here" keeps the name and takes the folder shown. It is drawn
//! in Yggdrasil's style like the rest of the window, so it works the same on any desktop and in the headless tests.
//! (Yggdrasil's own chooser, Minos, is reached through the FileChooser portal; that needs a D-Bus client the app does
//! not carry yet.)

use std::path::{Path, PathBuf};

use eframe::egui::{self, vec2, Id, RichText, Sense, WidgetInfo, WidgetType};

use super::{ArgusApp, FieldKind};
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
        let resp = egui::Modal::new(Id::new("picker")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new(if p.save { "Choose where to save" } else { "Choose a file" }).size(18.0).strong());
            ui.horizontal(|ui| {
                if ui.add_enabled(p.dir.parent().is_some(), egui::Button::new("Up")).clicked() {
                    if let Some(up) = p.dir.parent().map(Path::to_path_buf) {
                        p.go(up);
                    }
                }
                if ui.button("Home").clicked() {
                    p.go(self.cfg.home.clone());
                }
                // under the home folder as ~/...
                let shown = match p.dir.strip_prefix(&self.cfg.home) {
                    Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
                    Ok(rest) => format!("~/{}", rest.display()),
                    Err(_) => p.dir.display().to_string(),
                };
                ui.label(RichText::new(shown).color(pal.dim).size(12.0));
            });
            let mut open = None;
            egui::ScrollArea::vertical().id_salt("picker-list").max_height(240.0).auto_shrink([false, false]).show(ui, |ui| {
                for (name, dir) in &p.entries {
                    let sel = p.selected.as_deref() == Some(name.as_str());
                    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                    r.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, sel, name));
                    if sel {
                        ui.painter().rect_filled(rect, 6.0, pal.select);
                    } else if r.hovered() {
                        ui.painter().rect_filled(rect, 6.0, pal.hover);
                    }
                    let icon = egui::Rect::from_center_size(rect.left_center() + vec2(12.0, 0.0), vec2(12.0, 10.0));
                    if *dir {
                        ui.painter().rect_filled(icon, 2.0, pal.folder);
                    } else {
                        ui.painter().rect_stroke(
                            icon.shrink2(vec2(2.0, 0.0)),
                            1.0,
                            egui::Stroke::new(1.2, pal.dim),
                            egui::StrokeKind::Inside,
                        );
                    }
                    ui.painter().text(
                        rect.left_center() + vec2(26.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        name,
                        egui::FontId::proportional(13.0),
                        pal.text,
                    );
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
                    ui.label(RichText::new("Empty").color(pal.dim));
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
                    ui.add(egui::TextEdit::singleline(&mut p.name).id(Id::new("picker-name")).desired_width(300.0));
                });
            }
            if let Some(e) = &p.error {
                ui.label(RichText::new(e).color(pal.red));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let sel_dir = p.selected.as_ref().and_then(|s| p.entries.iter().find(|(n, _)| n == s)).map(|(_, d)| *d);
                if p.save {
                    let name_ok = !p.name.trim().is_empty() && !p.name.contains('/');
                    if ui
                        .add_enabled(name_ok, egui::Button::new(RichText::new("Save here").color(pal.on_accent())).fill(pal.accent))
                        .clicked()
                    {
                        done = Some(Some(p.dir.join(p.name.trim())));
                    }
                } else if ui
                    .add_enabled(sel_dir == Some(false), egui::Button::new(RichText::new("Choose").color(pal.on_accent())).fill(pal.accent))
                    .clicked()
                {
                    done = p.selected.clone().map(|n| Some(p.dir.join(n)));
                }
                if ui.add_enabled(sel_dir == Some(true), egui::Button::new("Open folder")).clicked() {
                    if let Some(n) = p.selected.clone() {
                        p.go(p.dir.join(n));
                    }
                }
                if ui.button("Cancel").clicked() {
                    done = Some(None);
                }
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
