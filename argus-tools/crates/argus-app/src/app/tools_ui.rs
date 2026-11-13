//! The Argus tools window (TOOLS in the footer, the Qt app's ARGUS button), drawn in the card as the console is: tabs
//! Apps, Check-up, Rebuild, Snapshots and Captures, as `ArgusToolsWindow.qml` has them. The work runs in `tools.rs`,
//! on the device worker when it needs the card, else on a thread of its own; the window only asks and shows.

use std::path::PathBuf;

use eframe::egui::{self, Id, RichText, UiBuilder};

use super::{Action, ArgusApp, Dialog, Field};
use crate::theme::Palette;
use crate::tools::{self, Dirs, Reply, Sess, Tab};

impl ArgusApp {
    /// The firmware API the tools judge apps against: the device's, else the Qt app's default.
    pub(super) fn tools_api(&self) -> argus_tools::api::FirmwareApi {
        self.info
            .as_ref()
            .and_then(|i| tools::firmware_api(&i.pairs))
            .or_else(|| argus_tools::api::FirmwareApi::parse(tools::DEFAULT_API))
            .expect("the default API parses")
    }

    fn tools_dirs(&self) -> Dirs {
        self.cfg.tools.clone()
    }

    /// Runs `f` on a thread of its own (no card needed).
    pub(super) fn tools_local(&mut self, what: &str, f: impl FnOnce(&Dirs, &mut dyn FnMut(String)) -> Reply + Send + 'static) {
        let (Some(waker), dirs, tx) = (self.waker.clone(), self.tools_dirs(), self.tools_tx.clone()) else { return };
        self.tools.busy = Some(what.to_string());
        std::thread::spawn(move || {
            let mut status = |m: String| {
                let _ = tx.send(Reply::Status(m));
                waker();
            };
            let r = f(&dirs, &mut status);
            let _ = tx.send(r);
            waker();
        });
    }

    /// Runs `f` on the device worker, between the UI's own requests.
    pub(super) fn tools_device(&mut self, what: &str, f: impl FnOnce(&mut Sess, &Dirs, &mut dyn FnMut(String)) -> Reply + Send + 'static) {
        let (Some(waker), dirs, tx) = (self.waker.clone(), self.tools_dirs(), self.tools_tx.clone()) else { return };
        let Some(w) = &self.worker else {
            self.tools.message = Some(Err("no Flipper connected".into()));
            return;
        };
        self.tools.busy = Some(what.to_string());
        w.send(crate::device::Cmd::Run(Box::new(move |s: &mut Sess| {
            let mut status = |m: String| {
                let _ = tx.send(Reply::Status(m));
                waker();
            };
            let r = f(s, &dirs, &mut status);
            let _ = tx.send(r);
            waker();
        })));
    }

    pub(super) fn refresh_snapshots(&mut self) {
        self.tools_local("Reading the snapshots…", |d, _| Reply::Snapshots(tools::snapshots(d)));
    }

    pub(super) fn search_captures(&mut self) {
        let q = self.tools.query.clone();
        self.tools_local("Searching…", move |d, _| Reply::Captures(tools::search(d, &q)));
    }

    pub(super) fn tools_ui(&mut self, ui: &mut egui::Ui, card: egui::Rect, pal: &Palette) {
        ui.painter().rect_filled(card, 12.0, pal.sidebar);
        if !self.tools.loaded && self.tools.busy.is_none() {
            self.tools.loaded = true;
            self.refresh_snapshots();
        }
        let connected = self.worker.is_some() && self.info.is_some() && !self.pulling() && matches!(self.stage, super::Stage::Ready);
        let idle = self.tools.busy.is_none();
        let inner = card.shrink(16.0);
        ui.scope_builder(UiBuilder::new().id(Id::new(("scope", 20))).max_rect(inner), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Argus tools").size(20.0));
                for (t, label) in [
                    (Tab::Apps, "Apps"),
                    (Tab::Checkup, "Check-up"),
                    (Tab::Rebuild, "Rebuild"),
                    (Tab::Snapshots, "Snapshots"),
                    (Tab::Captures, "Captures"),
                ] {
                    let on = self.tools.tab == t;
                    let b = egui::Button::new(RichText::new(label).color(if on { pal.on_accent() } else { pal.text })).fill(if on {
                        pal.accent
                    } else {
                        pal.field
                    });
                    if ui.add(b).clicked() {
                        self.tools.tab = t;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let api = self.tools_api().text();
                    let from = if self.info.as_ref().and_then(|i| tools::firmware_api(&i.pairs)).is_some() { "device" } else { "default" };
                    ui.label(RichText::new(format!("API {api} ({from})")).color(pal.dim).size(12.0));
                });
            });
            // the status line: what runs, else how the last job ended
            let (text, colour) = match (&self.tools.busy, &self.tools.message) {
                (Some(b), _) => (b.clone(), pal.dim),
                (None, Some(Ok(m))) => (m.clone(), pal.green),
                (None, Some(Err(e))) => (e.clone(), pal.red),
                (None, None) => {
                    (if connected { "Ready.".into() } else { "No Flipper connected: the card's tools wait for one.".into() }, pal.dim)
                }
            };
            ui.horizontal(|ui| {
                if !idle {
                    ui.add(egui::Spinner::new().size(12.0));
                }
                ui.label(RichText::new(text).color(colour).size(12.0));
            });
            ui.separator();
            let can_card = idle && connected;
            match self.tools.tab {
                Tab::Apps => self.tools_apps(ui, pal, idle, can_card),
                Tab::Checkup => self.tools_checkup(ui, pal, can_card),
                Tab::Rebuild => self.tools_rebuild(ui, pal, idle),
                Tab::Snapshots => self.tools_snapshots(ui, pal, idle, can_card),
                Tab::Captures => self.tools_captures(ui, pal, idle, can_card),
            }
        });
        if !idle {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String, width: f32) {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(value).id(Id::new(("tools-field", label))).desired_width(width));
    }

    fn tools_apps(&mut self, ui: &mut egui::Ui, pal: &Palette, idle: bool, can_card: bool) {
        ui.horizontal(|ui| {
            Self::path_row(ui, "Package", &mut self.tools.package, 420.0);
            if ui.add_enabled(idle, egui::Button::new("Browse…")).clicked() {
                self.open_picker(super::PickFor::Package);
            }
            if ui.add_enabled(idle && !self.tools.package.trim().is_empty(), egui::Button::new("Open")).clicked() {
                let path = PathBuf::from(self.tools.package.trim());
                let fw = self.tools_api();
                self.tools_local("Opening the package…", move |d, _| {
                    Reply::Package(tools::check_package(&path).and_then(|()| tools::scan_package(d, &path, fw)))
                });
            }
        });
        let Some(report) = self.tools.report.clone() else {
            ui.label(RichText::new("Open a firmware package (.tgz, .zip, .tar, resources.ths) or a .fap to list its apps.").color(pal.dim));
            return;
        };
        egui::ScrollArea::vertical().id_salt("tools-apps").max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
            for (i, a) in report.apps.iter().enumerate() {
                ui.horizontal(|ui| {
                    let name = a.manifest.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| a.file_name.clone());
                    if let Some(p) = self.tools.picked.get_mut(i) {
                        ui.checkbox(p, name);
                    }
                    ui.label(RichText::new(format!("{}/{}", a.category, a.file_name)).color(pal.dim).size(12.0));
                    let (status, colour) = match a.compat.as_ref().map(|c| c.status) {
                        Some(argus_tools::api::Status::Compatible) => ("compatible", pal.green),
                        Some(argus_tools::api::Status::Incompatible) => ("will not load", pal.red),
                        _ => ("unknown", pal.amber),
                    };
                    let r = ui.label(RichText::new(status).color(colour).size(12.0));
                    if let Some(c) = &a.compat {
                        r.on_hover_text(&c.reason);
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.tools.force, "Force (install apps that may not load)");
            let picked: Vec<_> = report.apps.iter().zip(&self.tools.picked).filter(|(_, p)| **p).map(|(a, _)| a.clone()).collect();
            if ui.add_enabled(can_card && !picked.is_empty(), egui::Button::new("Check the card")).clicked() {
                let (force, fw) = (self.tools.force, self.tools_api().text());
                self.tools_device("Checking the card…", move |s, _, _| Reply::Plan(Ok(tools::plan_install(s, &picked, force, &fw))));
            }
            let ready = self.tools.plan.iter().any(|p| !p.refused && (!p.exists || p.overwrite));
            if ui.add_enabled(can_card && ready, egui::Button::new("Install…")).clicked() {
                self.ask_tools(Action::ToolsInstall);
            }
        });
        if !self.tools.plan.is_empty() {
            egui::ScrollArea::vertical().id_salt("tools-plan").max_height(70.0).auto_shrink([false, true]).show(ui, |ui| {
                for p in self.tools.plan.iter_mut() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&p.remote).size(12.0));
                        if p.refused {
                            ui.label(RichText::new(&p.reason).color(pal.red).size(12.0));
                        } else if p.exists {
                            ui.checkbox(&mut p.overwrite, RichText::new("on the card: overwrite").size(12.0));
                        } else {
                            ui.label(RichText::new("new").color(pal.green).size(12.0));
                        }
                    });
                }
            });
        }
    }

    fn tools_checkup(&mut self, ui: &mut egui::Ui, pal: &Palette, can_card: bool) {
        ui.horizontal(|ui| {
            Self::path_row(ui, "Packages to look in (optional, separated by ;)", &mut self.tools.checkup_packages, 300.0);
        });
        if ui.add_enabled(can_card, egui::Button::new("Check the apps on the card")).clicked() {
            let packages: Vec<String> =
                self.tools.checkup_packages.split(';').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect();
            let fw = self.tools_api();
            self.tools_device("Listing the apps on the card…", move |s, d, st| Reply::Checkup(tools::checkup(s, d, fw, &packages, st)));
        }
        let Some(v) = self.tools.checkup.clone() else { return };
        egui::ScrollArea::vertical().id_salt("tools-checkup").auto_shrink([false, false]).show(ui, |ui| {
            for a in v["apps"].as_array().cloned().unwrap_or_default() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(a["path"].as_str().unwrap_or_default()).size(12.0));
                    let status = a["compat"]["status"].as_str().unwrap_or_default().to_string();
                    let colour = match status.as_str() {
                        "compatible" => pal.green,
                        "incompatible" => pal.red,
                        _ => pal.amber,
                    };
                    ui.label(RichText::new(&status).color(colour).size(12.0))
                        .on_hover_text(a["compat"]["reason"].as_str().unwrap_or_default());
                    if let Some(name) = a["replacement"]["file_name"].as_str() {
                        ui.label(RichText::new(format!("a compatible copy: {name}")).color(pal.accent).size(12.0));
                    }
                });
            }
        });
    }

    fn tools_rebuild(&mut self, ui: &mut egui::Ui, pal: &Palette, idle: bool) {
        ui.label(
            RichText::new("Builds an app's source with ufbt in a sealed Limbo run (a git URL is fetched in a run of its own).")
                .color(pal.dim),
        );
        ui.horizontal(|ui| Self::path_row(ui, "Source (folder or https:// git URL)", &mut self.tools.source, 330.0));
        ui.horizontal(|ui| {
            Self::path_row(ui, "Ref", &mut self.tools.git_ref, 140.0);
            Self::path_row(ui, "Folder in the repository", &mut self.tools.subdir, 160.0);
        });
        if ui.add_enabled(idle && !self.tools.source.trim().is_empty(), egui::Button::new("Build in Limbo")).clicked() {
            let (src, r, sub, fw) = (
                self.tools.source.trim().to_string(),
                self.tools.git_ref.trim().to_string(),
                self.tools.subdir.trim().to_string(),
                self.tools_api(),
            );
            self.tools_local("Starting the Limbo build…", move |d, st| Reply::Rebuild(tools::rebuild(d, &src, &r, &sub, fw, st)));
        }
        if let Some(b) = &self.tools.built {
            for f in &b.faps {
                ui.label(RichText::new(format!("{} ({} bytes): {:?}", f.file_name, f.size, f.compat.status)).size(12.0));
            }
            egui::ScrollArea::vertical().id_salt("tools-log").auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new(&b.log_tail).monospace().size(11.0).color(pal.dim));
            });
        }
    }

    fn tools_snapshots(&mut self, ui: &mut egui::Ui, pal: &Palette, idle: bool, can_card: bool) {
        ui.horizontal(|ui| {
            Self::path_row(ui, "Label", &mut self.tools.label, 220.0);
            if ui.add_enabled(can_card, egui::Button::new("Take a snapshot")).clicked() {
                let (label, fw) = (self.tools.label.trim().to_string(), self.tools_api().text());
                self.tools_device("Listing the card…", move |s, d, st| Reply::Done(tools::take_snapshot(s, d, &label, &fw, st)));
            }
            if ui.add_enabled(idle, egui::Button::new("Refresh")).clicked() {
                self.refresh_snapshots();
            }
        });
        let list = self.tools.snapshots.clone();
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical().id_salt("tools-snaps").max_height(190.0).auto_shrink([false, false]).show(&mut cols[0], |ui| {
                for s in &list {
                    let sel = self.tools.selected.as_deref() == Some(s.id.as_str());
                    let text = format!(
                        "{}  {}  {} files{}",
                        s.created,
                        s.id,
                        s.file_count,
                        if s.label.is_empty() { String::new() } else { format!("  {}", s.label) }
                    );
                    if ui.selectable_label(sel, RichText::new(text).size(12.0)).clicked() {
                        self.tools.selected = Some(s.id.clone());
                        self.tools.diff = None;
                        self.tools.restore = None;
                    }
                }
                if list.is_empty() {
                    ui.label(RichText::new("No snapshots yet").color(pal.dim));
                }
            });
            let ui = &mut cols[1];
            let Some(id) = self.tools.selected.clone() else {
                ui.label(RichText::new("Select a snapshot to compare, restore or delete it.").color(pal.dim));
                return;
            };
            ui.horizontal(|ui| {
                if ui.add_enabled(can_card, egui::Button::new("Compare with the card")).clicked() {
                    let id = id.clone();
                    self.tools_device("Comparing with the card…", move |s, d, st| Reply::Diff(tools::diff_with_card(s, d, &id, st)));
                }
                let others: Vec<String> = list.iter().filter(|s| s.id != id).map(|s| s.id.clone()).collect();
                let mut other = self.tools.other.clone().filter(|o| others.contains(o));
                egui::ComboBox::from_id_salt("tools-other").selected_text(other.clone().unwrap_or_else(|| "another…".into())).show_ui(
                    ui,
                    |ui| {
                        for o in &others {
                            ui.selectable_value(&mut other, Some(o.clone()), o);
                        }
                    },
                );
                self.tools.other = other.clone();
                if ui.add_enabled(idle && other.is_some(), egui::Button::new("Compare")).clicked() {
                    let (a, b) = (id.clone(), other.unwrap_or_default());
                    self.tools_local("Comparing snapshots…", move |d, _| Reply::Diff(tools::diff_snapshots(d, &a, &b)));
                }
            });
            ui.horizontal(|ui| {
                Self::path_row(ui, "Path", &mut self.tools.restore_path, 150.0);
                if ui.add_enabled(can_card, egui::Button::new("Plan a restore")).clicked() {
                    let (id, path) = (id.clone(), self.tools.restore_path.trim().to_string());
                    self.tools_device("Comparing the snapshot with the card…", move |s, d, st| {
                        Reply::RestorePlan(tools::plan_restore(s, d, &id, &path, st))
                    });
                }
            });
            ui.horizontal(|ui| {
                let ready = self.tools.restore.as_ref().is_some_and(|p| p.snapshot == id && p.creates + p.overwrites > 0);
                if ui.add_enabled(can_card && ready, egui::Button::new("Restore…")).clicked() {
                    self.ask_tools(Action::ToolsRestore);
                }
                if ui.add_enabled(idle, egui::Button::new(RichText::new("Delete…").color(pal.red))).clicked() {
                    self.ask_tools(Action::ToolsDeleteSnapshot { id: id.clone() });
                }
            });
            if let Some(d) = &self.tools.diff {
                egui::ScrollArea::vertical().id_salt("tools-diff").auto_shrink([false, false]).show(ui, |ui| {
                    for (what, list, colour) in
                        [("added", &d.added, pal.green), ("removed", &d.removed, pal.red), ("changed", &d.changed, pal.amber)]
                    {
                        for c in list {
                            ui.label(RichText::new(format!("{what}: {}", c.path)).color(colour).size(12.0));
                        }
                    }
                });
            }
        });
    }

    fn tools_captures(&mut self, ui: &mut egui::Ui, pal: &Palette, idle: bool, can_card: bool) {
        ui.horizontal(|ui| {
            if ui.add_enabled(can_card, egui::Button::new("Read from the card")).clicked() {
                self.tools_device("Listing the capture folders…", |s, d, st| Reply::Done(tools::read_captures(s, d, st)));
            }
            if ui.add_enabled(idle, egui::Button::new("Rebuild the library")).clicked() {
                self.tools_local("Indexing captures…", |d, _| Reply::Done(tools::index(d)));
            }
            if ui.add_enabled(idle, egui::Button::new("Duplicates")).clicked() {
                self.tools_local("Looking for duplicates…", |d, _| Reply::Dupes(tools::dupes(d)));
            }
        });
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.tools.query)
                    .id(Id::new("tools-query"))
                    .hint_text("Search: kind, name, a field, a tag…")
                    .desired_width(360.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(idle, egui::Button::new("Search")).clicked() || enter) && idle {
                self.search_captures();
            }
            ui.label(RichText::new(format!("{} of {}", self.tools.captures.len(), self.tools.total)).color(pal.dim).size(12.0));
        });
        let list = self.tools.captures.clone();
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical().id_salt("tools-captures").max_height(200.0).auto_shrink([false, false]).show(&mut cols[0], |ui| {
                for c in &list {
                    let sel = self.tools.capture.as_deref() == Some(c.id.as_str());
                    let text = RichText::new(format!(
                        "{}  {}  {}",
                        c.kind,
                        c.name,
                        if c.tags.is_empty() { String::new() } else { format!("[{}]", c.tags.join(", ")) }
                    ))
                    .size(12.0);
                    if ui.selectable_label(sel, text).clicked() && idle {
                        self.tools.capture = Some(c.id.clone());
                        let id = c.id.clone();
                        self.tools_local("Reading the capture…", move |d, _| Reply::Explain(tools::explain(d, &id)));
                    }
                }
            });
            let ui = &mut cols[1];
            if let Some(groups) = &self.tools.dupes {
                for g in groups.iter().take(20) {
                    ui.label(RichText::new(format!("{}: {}", g.kind, g.paths.join(", "))).size(11.0).color(pal.dim));
                }
            }
            let Some((c, text)) = self.tools.detail.clone() else { return };
            ui.label(RichText::new(&c.name).strong());
            egui::ScrollArea::vertical().id_salt("tools-explain").max_height(130.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new(text).size(12.0));
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.tools.tags).id(Id::new("tools-tags")).hint_text("tags").desired_width(150.0));
                for (label, add) in [("Add tags", true), ("Remove", false)] {
                    if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                        match tools::parse_tags(&self.tools.tags) {
                            Ok(t) => {
                                let id = c.id.clone();
                                let q = self.tools.query.clone();
                                self.tools_local("Tagging…", move |d, _| {
                                    let (add, remove) = if add { (t, Vec::new()) } else { (Vec::new(), t) };
                                    match tools::tag(d, &id, &add, &remove) {
                                        Ok(_) => Reply::Captures(tools::search(d, &q)),
                                        Err(e) => Reply::Done(Err(e)),
                                    }
                                });
                                self.tools.tags.clear();
                            }
                            Err(e) => self.tools.message = Some(Err(e)),
                        }
                    }
                }
            });
        });
    }

    /// The tools' confirmations, through the same dialog as everything that changes the device.
    pub(super) fn ask_tools(&mut self, action: Action) {
        let d = match &action {
            Action::ToolsInstall => {
                let todo: Vec<&tools::PlanItem> = self.tools.plan.iter().filter(|p| !p.refused && (!p.exists || p.overwrite)).collect();
                let overwrite = todo.iter().filter(|p| p.exists).count();
                Dialog {
                    title: "Install apps?".into(),
                    message: format!(
                        "{} app(s) will be written to the card's /ext/apps, {overwrite} of them over an app already there.",
                        todo.len()
                    ),
                    warning: (overwrite > 0)
                        .then(|| todo.iter().filter(|p| p.exists).map(|p| p.remote.clone()).collect::<Vec<_>>().join(", ")),
                    confirm: "Install".into(),
                    danger: overwrite > 0,
                    fields: Vec::new(),
                    action,
                    error: None,
                }
            }
            Action::ToolsRestore => {
                let Some(p) = &self.tools.restore else { return };
                Dialog {
                    title: "Restore from the snapshot?".into(),
                    message: format!(
                        "{} file(s) will be created and {} overwritten on the card from snapshot {}; {} already match.",
                        p.creates, p.overwrites, p.snapshot, p.same
                    ),
                    warning: (p.overwrites > 0).then(|| "Files on the card that differ from the snapshot are replaced.".to_string()),
                    confirm: "Restore".into(),
                    danger: p.overwrites > 0,
                    fields: Vec::new(),
                    action,
                    error: None,
                }
            }
            Action::ToolsDeleteSnapshot { id } => Dialog {
                title: format!("Delete snapshot {id}?"),
                message: "Its files stay only if another snapshot holds them. This action cannot be undone.".into(),
                warning: None,
                confirm: "Delete".into(),
                danger: true,
                fields: Vec::new(),
                action,
                error: None,
            },
            _ => return,
        };
        self.dialog = Some(d);
    }

    /// A confirmed tools action.
    pub(super) fn execute_tools(&mut self, action: Action, _fields: &[Field]) -> Result<(), String> {
        match action {
            Action::ToolsInstall => {
                let plan = self.tools.plan.clone();
                self.tools.plan.clear();
                self.tools_device("Installing…", move |s, _, st| Reply::Done(tools::install(s, &plan, st)));
            }
            Action::ToolsRestore => {
                let plan = self.tools.restore.take().ok_or("Plan a restore first.")?;
                self.tools_device("Restoring…", move |s, _, st| Reply::Done(tools::restore(s, &plan, st)));
            }
            Action::ToolsDeleteSnapshot { id } => {
                self.tools.selected = None;
                self.tools.diff = None;
                self.tools_local("Deleting…", move |d, _| Reply::Done(tools::delete_snapshot(d, &id)));
            }
            _ => {}
        }
        Ok(())
    }
}
