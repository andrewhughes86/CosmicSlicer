//! The application: lays out the top bar, the page, and the dialogs; runs the panes'
//! actions, the keyboard shortcuts and launch automation; saves preferences and presets.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Event, Key, KeyboardShortcut, Modifiers, Ui, UserData, ViewportCommand};
use egui_wgpu::RenderState;
use serde::{Deserialize, Serialize};

use CosmicSlicer_config::{Preset, TabKind};

use crate::Action;
use crate::automation::{self, Automation, Step};
use crate::dialogs::{Dialogs, Editor};
use crate::pages::{self, DeviceState};
use crate::sidebar::{self, Sidebar};
use crate::state::{Level, Page, Prefs, Session};
use crate::theme::{self, Theme, Tokens};
use crate::topbar::{self, SliceChoices};
use crate::ui_kit::{self, Surface};
use crate::viewport::{Tool, View, Viewport};

const PREFS_KEY: &str = "cosmicslicer.prefs";
const PRESETS_KEY: &str = "cosmicslicer.presets";

#[derive(Default)]
pub struct LaunchOptions {
    pub automation: Automation,
    /// A test run: nothing is saved.
    pub ephemeral: bool,
    /// Model files to import at start-up.
    pub files: Vec<PathBuf>,
}

/// The user's presets and selections, as saved between runs.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedPresets {
    printers: Vec<Preset>,
    filaments: Vec<Preset>,
    processes: Vec<Preset>,
    printer: String,
    process: String,
    project_filaments: Vec<String>,
}

pub struct CosmicSlicerApp {
    session: Session,
    viewport: Viewport,
    sidebar: Sidebar,
    sidebar_visible: bool,
    dialogs: Dialogs,
    device: DeviceState,
    choices: SliceChoices,
    render_state: Option<RenderState>,
    automation: Automation,
    ephemeral: bool,
    applied_theme: Option<Theme>,
    capture: Option<PathBuf>,
    capture_requested: bool,
    capture_wait: u32,
    startup_files: Vec<PathBuf>,
    window_title: String,
    /// Frames that panicked and were recovered.
    recoveries: u32,
    /// The slice running, if any.
    runner: Option<crate::slicer::Runner>,
    /// (plate, revision, fingerprint) of the last slice that failed, so Preview does not retry it.
    last_failed: Option<(usize, u64, u64)>,
    /// Export the G-code as soon as the running slice finishes.
    export_when_sliced: bool,
    /// Write the G-code here when the slice finishes, without asking (automation).
    export_to: Option<PathBuf>,
}

impl CosmicSlicerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, options: LaunchOptions) -> Self {
        let mut app = Self::headless(options);
        app.render_state = cc.wgpu_render_state.clone();
        if !app.ephemeral
            && let Some(storage) = cc.storage
        {
            let session = &mut app.session;
            if let Some(prefs) = storage.get_string(PREFS_KEY).and_then(|s| serde_json::from_str::<Prefs>(&s).ok()) {
                session.prefs = prefs;
            }
            if let Some(saved) =
                storage.get_string(PRESETS_KEY).and_then(|s| serde_json::from_str::<SavedPresets>(&s).ok())
            {
                restore_presets(session, saved);
            }
        }
        app
    }

    /// The app without a window: no GPU, nothing loaded from storage (tests).
    pub fn headless(options: LaunchOptions) -> Self {
        Self {
            session: Session::new(),
            viewport: Viewport::default(),
            sidebar: Sidebar::new(),
            sidebar_visible: true,
            dialogs: Dialogs::default(),
            device: DeviceState::default(),
            choices: SliceChoices::default(),
            render_state: None,
            automation: options.automation,
            ephemeral: options.ephemeral,
            applied_theme: None,
            capture: None,
            capture_requested: false,
            capture_wait: 0,
            startup_files: options.files,
            window_title: String::new(),
            recoveries: 0,
            runner: None,
            last_failed: None,
            export_when_sliced: false,
            export_to: None,
        }
    }

    /// Starts slicing the current plate, unless that slice is already running or done.
    fn start_slice(&mut self) {
        let s = &mut self.session;
        if !s.model.objects.iter().any(|o| o.plate == s.plate && o.visible) {
            s.notify(Level::Warning, "There is nothing on this plate to slice.");
            return;
        }
        if s.sliced.as_ref().is_some_and(|st| st.is_current(s)) {
            if self.export_when_sliced {
                self.export_when_sliced = false;
                self.export_gcode();
            }
            return;
        }
        let fingerprint = crate::slicer::fingerprint(s);
        if let Some(r) = &self.runner {
            if r.plate == s.plate && r.revision == s.revision && r.fingerprint == fingerprint {
                return;
            }
            r.progress.cancel();
        }
        match crate::slicer::Runner::start(s) {
            Ok(r) => {
                s.slicing = Some((0.0, "Slicing".into()));
                self.runner = Some(r);
            }
            Err(e) => s.notify(Level::Error, e),
        }
    }

    /// Checks on the running slice; picks up its result.
    fn poll_slice(&mut self, ctx: &egui::Context) {
        let s = &mut self.session;
        // In Preview, a stale or missing slice is redone, as OrcaSlicer's background slicing does.
        if s.page == Page::Preview
            && self.runner.is_none()
            && !s.sliced.as_ref().is_some_and(|st| st.is_current(s))
            && s.model.objects.iter().any(|o| o.plate == s.plate && o.visible)
            && self.last_failed != Some((s.plate, s.revision, crate::slicer::fingerprint(s)))
        {
            self.start_slice();
        }
        let s = &mut self.session;
        let Some(runner) = &self.runner else {
            s.slicing = None;
            return;
        };
        if std::mem::take(&mut s.cancel_slice) {
            runner.progress.cancel();
        }
        s.slicing = Some((runner.progress.fraction(), runner.progress.stage()));
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let Some(result) = runner.poll() else { return };
        let (plate, revision, fingerprint) = (runner.plate, runner.revision, runner.fingerprint);
        self.runner = None;
        s.slicing = None;
        match result {
            Ok(sliced) => {
                for w in &sliced.warnings {
                    s.notify(Level::Warning, w.clone());
                }
                if let Some(path) = self.export_to.take() {
                    match std::fs::write(&path, sliced.gcode.as_bytes()) {
                        Ok(()) => println!("G-code written to {}", path.display()),
                        Err(e) => eprintln!("could not write {}: {e}", path.display()),
                    }
                }
                s.sliced = Some(crate::slicer::SliceState { plate, revision, fingerprint, result: Arc::new(sliced) });
                if self.export_when_sliced {
                    self.export_when_sliced = false;
                    self.export_gcode();
                }
            }
            Err(CosmicSlicer_engine::SliceError::Cancelled) => {}
            Err(e) => {
                if self.export_to.take().is_some() {
                    eprintln!("slicing failed: {e}");
                }
                self.last_failed = Some((plate, revision, fingerprint));
                self.export_when_sliced = false;
                s.notify(Level::Error, e.to_string());
            }
        }
    }

    fn export_gcode(&mut self) {
        let s = &mut self.session;
        let Some(st) = s.sliced.as_ref().filter(|st| st.is_current(s)) else { return };
        let result = Arc::clone(&st.result);
        let name = format!("{}_plate_{}.gcode", s.project.name.replace(['/', '\\', ':'], "_"), s.plate + 1);
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export G-code")
            .add_filter("G-code", &["gcode", "gco", "g"])
            .set_file_name(name)
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, result.gcode.as_bytes()) {
            Ok(()) => s.notify(Level::Info, format!("Exported {}", path.display())),
            Err(e) => s.notify(Level::Error, format!("Could not write {}: {e}", path.display())),
        }
    }

    /// Keeps going after a bug in drawing a frame: closes what was open, says what happened,
    /// and falls back to a plain page if the same thing keeps failing.
    fn recover(&mut self, payload: Box<dyn std::any::Any + Send>) {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".into());
        log::error!("recovered from a panic while drawing: {message}");
        self.recoveries += 1;
        self.dialogs = Dialogs::default();
        self.viewport.tool = None;
        if self.recoveries > 3 {
            self.session.page = Page::Home;
        }
        self.session.notify(
            Level::Error,
            format!("CosmicSlicer hit a bug drawing the window and recovered; your project is unchanged. ({message})"),
        );
    }

    fn run_step(&mut self, step: Step, actions: &mut Vec<Action>) {
        match step {
            Step::Tab(name) => match Page::from_name(&name) {
                Some(Page::Preview) => actions.push(Action::Slice),
                Some(p) => self.session.page = p,
                None => eprintln!("--tab: there is no page called {name}"),
            },
            Step::Primitive(name) => self.session.add_primitive(&name),
            Step::Open(path) => actions.push(Action::ImportPath(path)),
            Step::Arrange => self.session.arrange(false),
            Step::SelectAll => self.session.select_all(),
            Step::Theme(name) => {
                self.session.prefs.theme = if name.eq_ignore_ascii_case("light") { Theme::Light } else { Theme::Dark };
            }
            Step::Dialog(name) => match name.to_ascii_lowercase().as_str() {
                "preferences" => actions.push(Action::Preferences),
                "printer" => actions.push(Action::Settings(TabKind::Printer)),
                "filament" => actions.push(Action::FilamentSettings(0)),
                "process" => actions.push(Action::Settings(TabKind::Process)),
                "about" => actions.push(Action::About),
                "shortcuts" => actions.push(Action::Shortcuts),
                "compare" => actions.push(Action::ComparePresets),
                other => eprintln!("--dialog: unknown dialog {other}"),
            },
            Step::Slice => actions.push(Action::Slice),
            Step::Advanced => self.session.prefs.advanced = true,
            Step::Tool(name) => {
                self.viewport.tool = Tool::GIZMOS.iter().find(|g| g.2.eq_ignore_ascii_case(&name)).map(|g| g.0);
            }
            Step::Export(path) => {
                self.export_to = Some(path);
                actions.push(Action::Slice);
            }
            Step::Layer(n) => self.viewport.pending_top = Some(n),
            Step::Set(kv) => match kv.split_once('=') {
                Some((k, v)) => {
                    let schema = self.session.schema;
                    self.session.presets.set_value(schema, TabKind::Process, 0, k.trim(), v.trim().to_owned());
                }
                None => eprintln!("--set needs key=value, not {kv}"),
            },
            Step::Page(name) => {
                let pages = self.session.schema.pages(TabKind::Process);
                if let Some(i) = pages.iter().position(|p| p.name.eq_ignore_ascii_case(&name)) {
                    self.sidebar.process_page = i;
                }
            }
        }
    }

    fn import_dialog(&mut self) {
        let files = rfd::FileDialog::new()
            .set_title("Import 3MF/STL/STEP/SVG/OBJ/AMF")
            .add_filter("Supported files", CosmicSlicer_model::IMPORT_EXTENSIONS)
            .add_filter("STL", &["stl"])
            .add_filter("OBJ", &["obj"])
            .pick_files();
        for path in files.unwrap_or_default() {
            self.session.import(&path);
        }
        self.viewport.fit_pending = true;
    }

    fn export_stl(&mut self) {
        if self.session.model.objects.is_empty() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export all objects as one STL")
            .add_filter("STL", &["stl"])
            .set_file_name(format!("{}.stl", self.session.project.name))
            .save_file()
        else {
            return;
        };
        let objects: Vec<_> = self.session.model.objects.iter().filter(|o| o.visible).collect();
        let meshes: Vec<(&CosmicSlicer_model::Mesh, glam::Mat4)> =
            objects.iter().map(|o| (o.mesh.as_ref(), o.matrix())).collect();
        match CosmicSlicer_model::stl::write_binary(&path, &meshes) {
            Ok(()) => self.session.notify(Level::Info, format!("Exported {}", path.display())),
            Err(e) => self.session.notify(Level::Error, e.to_string()),
        }
    }

    fn handle(&mut self, ctx: &egui::Context, action: Action) {
        let s = &mut self.session;
        match action {
            Action::NewProject => {
                s.new_project();
                self.viewport.fit_pending = true;
            }
            Action::OpenProject | Action::SaveProject => {
                s.notify(
                    Level::Warning,
                    "Project files (.3mf) arrive with the next step; use Import for STL and OBJ models.",
                );
            }
            Action::Import => self.import_dialog(),
            Action::ImportPath(path) => {
                s.import(&path);
                self.viewport.fit_pending = true;
            }
            Action::ExportStl => self.export_stl(),
            Action::ExportGcode => {
                if s.sliced.as_ref().is_some_and(|st| st.is_current(s)) {
                    self.export_gcode();
                } else if s.model.objects.iter().any(|o| o.plate == s.plate && o.visible) {
                    self.export_when_sliced = true;
                    self.start_slice();
                } else {
                    s.notify(Level::Warning, "There is nothing on this plate to slice.");
                }
            }
            Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Action::Slice => {
                s.page = Page::Preview;
                self.last_failed = None;
                self.start_slice();
            }
            Action::Print => self.dialogs.print = true,
            Action::Settings(kind) => {
                self.dialogs.editor = Some(Editor { kind, filament: 0, page: 0, search: String::new() });
            }
            Action::FilamentSettings(i) => {
                self.dialogs.editor =
                    Some(Editor { kind: TabKind::Filament, filament: i, page: 0, search: String::new() });
            }
            Action::SelectPreset(kind, filament, index) => self.dialogs.request_switch(s, kind, filament, index),
            Action::SavePreset(kind, filament) => self.dialogs.open_save(s, kind, filament),
            Action::ComparePresets => {
                let current = s.presets.process.preset;
                self.dialogs.compare = Some((TabKind::Process, current, current));
            }
            Action::PlateSettings(p) => self.dialogs.plate = Some(p),
            Action::PhysicalPrinter => self.dialogs.physical_printer = true,
            Action::FlushingVolumes => self.dialogs.flushing = true,
            Action::Preferences => self.dialogs.preferences = true,
            Action::About => self.dialogs.about = true,
            Action::Shortcuts => self.dialogs.shortcuts = true,
            Action::Calibration(name) => self.dialogs.calibration = Some(name),
            Action::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            Action::View(v) => self.viewport.set_view(v),
            Action::ZoomFit => self.viewport.fit_pending = true,
            Action::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Action::Rename(id) => {
                let name = s.model.get(id).map(|o| o.name.clone()).unwrap_or_default();
                self.dialogs.rename = Some((id, name));
            }
            Action::Unavailable(what) => s.notify(Level::Warning, format!("{what} arrives in a later step.")),
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let typing = ctx.egui_wants_keyboard_input();
        let ctrl = Modifiers::COMMAND;
        let shift = Modifiers::SHIFT;
        let page = self.session.page;
        let three_d = page.is_3d();
        let mut fired: Vec<&str> = Vec::new();
        ctx.input_mut(|i| {
            let mut take = |m: Modifiers, k: Key, name: &'static str| {
                if i.consume_shortcut(&KeyboardShortcut::new(m, k)) {
                    fired.push(name);
                }
            };
            // With Ctrl: always, even in a text box, except the editing keys.
            take(ctrl, Key::N, "new");
            take(ctrl, Key::O, "open");
            take(ctrl | shift, Key::S, "save");
            take(ctrl, Key::S, "save");
            take(ctrl, Key::I, "import");
            take(ctrl, Key::R, "slice");
            take(ctrl, Key::G, "export");
            take(ctrl, Key::P, "prefs");
            take(ctrl, Key::Q, "quit");
            for (k, n) in [
                (Key::Num1, "page1"),
                (Key::Num2, "page2"),
                (Key::Num3, "page3"),
                (Key::Num4, "page4"),
                (Key::Num5, "page5"),
            ] {
                take(ctrl, k, n);
            }
            if typing {
                return;
            }
            take(ctrl | shift, Key::Z, "redo");
            take(ctrl, Key::Z, "undo");
            take(ctrl, Key::Y, "redo");
            take(ctrl, Key::A, "select-all");
            take(ctrl, Key::K, "clone");
            take(ctrl, Key::C, "copy");
            take(ctrl, Key::X, "cut");
            take(ctrl, Key::V, "paste");
            take(ctrl, Key::Delete, "delete-all");
            take(Modifiers::NONE, Key::Delete, "delete");
            take(Modifiers::NONE, Key::Backspace, "delete");
            take(Modifiers::NONE, Key::Escape, "escape");
            take(shift, Key::Tab, "sidebar");
            take(Modifiers::NONE, Key::Tab, "tab");
            take(shift, Key::Slash, "help");
            take(Modifiers::NONE, Key::Questionmark, "help");
            if !three_d {
                return;
            }
            for (k, n) in [
                (Key::Num0, "view0"),
                (Key::Num1, "view1"),
                (Key::Num2, "view2"),
                (Key::Num3, "view3"),
                (Key::Num4, "view4"),
                (Key::Num5, "view5"),
                (Key::Num6, "view6"),
                (Key::Z, "fit"),
                (Key::K, "projection"),
                (Key::E, "labels"),
            ] {
                take(Modifiers::NONE, k, n);
            }
            if page == Page::Preview {
                take(Modifiers::NONE, Key::ArrowUp, "layer-up");
                take(Modifiers::NONE, Key::ArrowDown, "layer-down");
                return;
            }
            for (k, n) in [
                (Key::A, "arrange"),
                (Key::Q, "orient"),
                (Key::M, "tool-move"),
                (Key::R, "tool-rotate"),
                (Key::S, "tool-scale"),
                (Key::F, "tool-lay on face"),
                (Key::C, "tool-cut"),
                (Key::B, "tool-mesh boolean"),
                (Key::L, "tool-support painting"),
                (Key::P, "tool-seam painting"),
                (Key::H, "tool-fuzzy skin painting"),
                (Key::N, "tool-color painting"),
                (Key::T, "tool-text shape"),
                (Key::U, "tool-measure"),
                (Key::Y, "tool-assemble"),
            ] {
                take(Modifiers::NONE, k, n);
            }
            for (m, k, n) in [
                (Modifiers::NONE, Key::ArrowLeft, "nudge-left"),
                (Modifiers::NONE, Key::ArrowRight, "nudge-right"),
                (Modifiers::NONE, Key::ArrowUp, "nudge-up"),
                (Modifiers::NONE, Key::ArrowDown, "nudge-down"),
                (shift, Key::ArrowLeft, "nudge-left10"),
                (shift, Key::ArrowRight, "nudge-right10"),
                (shift, Key::ArrowUp, "nudge-up10"),
                (shift, Key::ArrowDown, "nudge-down10"),
            ] {
                take(m, k, n);
            }
        });
        let s = &mut self.session;
        for name in fired {
            match name {
                "new" => actions.push(Action::NewProject),
                "open" => actions.push(Action::OpenProject),
                "save" => actions.push(Action::SaveProject),
                "import" => actions.push(Action::Import),
                "slice" => actions.push(Action::Slice),
                "export" => actions.push(Action::ExportGcode),
                "prefs" => actions.push(Action::Preferences),
                "quit" => actions.push(Action::Quit),
                "page1" => s.page = Page::Home,
                "page2" => s.page = Page::Prepare,
                "page3" => actions.push(Action::Slice),
                "page4" => s.page = Page::Device,
                "page5" => s.page = Page::Project,
                "undo" => s.undo(),
                "redo" => s.redo(),
                "select-all" => s.select_all(),
                "clone" => s.clone_selected(),
                "copy" => s.copy(),
                "cut" => {
                    s.copy();
                    s.delete_selected();
                }
                "paste" => s.paste(),
                "delete-all" => s.delete_all(),
                "delete" => s.delete_selected(),
                "escape" => {
                    if !self.dialogs.close_one() {
                        if self.viewport.tool.is_some() {
                            self.viewport.tool = None;
                        } else {
                            s.selection.clear();
                        }
                    }
                }
                "sidebar" => self.sidebar_visible = !self.sidebar_visible,
                "tab" => match s.page {
                    Page::Prepare => actions.push(Action::Slice),
                    Page::Preview => s.page = Page::Prepare,
                    _ => {}
                },
                "help" => self.dialogs.shortcuts = true,
                "fit" => self.viewport.fit_pending = true,
                "projection" => s.prefs.perspective = !s.prefs.perspective,
                "labels" => s.prefs.show_labels = !s.prefs.show_labels,
                "arrange" => s.arrange(false),
                "orient" => s.auto_orient(),
                "layer-up" => self.viewport.layers.top = (self.viewport.layers.top + 1).min(self.viewport.layers.count),
                "layer-down" => {
                    self.viewport.layers.top =
                        self.viewport.layers.top.saturating_sub(1).max(self.viewport.layers.bottom)
                }
                view if view.starts_with("view") => {
                    let i: usize = view[4..].parse().unwrap_or(0);
                    if let Some((v, ..)) = View::ALL.get(i) {
                        self.viewport.set_view(*v);
                    }
                }
                tool if tool.starts_with("tool-") => {
                    let wanted = &tool[5..];
                    let found = Tool::GIZMOS.iter().find(|g| g.2.eq_ignore_ascii_case(wanted)).map(|g| g.0);
                    if !s.model.objects.is_empty() {
                        self.viewport.tool = if self.viewport.tool == found { None } else { found };
                    }
                }
                nudge if nudge.starts_with("nudge-") && !s.selection.is_empty() => {
                    let step = if nudge.ends_with("10") { 10.0 } else { 1.0 };
                    let d = match nudge.trim_end_matches("10") {
                        "nudge-left" => glam::Vec3::NEG_X,
                        "nudge-right" => glam::Vec3::X,
                        "nudge-up" => glam::Vec3::Y,
                        _ => glam::Vec3::NEG_Y,
                    } * step;
                    s.checkpoint();
                    let ids = s.selection.clone();
                    s.nudge(&ids, d);
                }
                _ => {}
            }
        }
    }

    /// Requests a capture while one is pending and saves it when the image arrives.
    fn service_capture(&mut self, ctx: &egui::Context) {
        let Some(path) = self.capture.clone() else { return };
        ctx.request_repaint();
        // A screenshot of a slice waits for it to finish.
        if self.runner.is_some() {
            return;
        }
        if !self.capture_requested {
            self.capture_wait += 1;
            if self.capture_wait < 3 {
                return;
            }
            self.capture_requested = true;
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(UserData::default()));
            return;
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                _ => None,
            })
        });
        let Some(image) = image else { return };
        match automation::save_png(&image, &path) {
            Ok(()) => println!("screenshot saved to {}", path.display()),
            Err(e) => eprintln!("could not save screenshot {}: {e}", path.display()),
        }
        self.capture = None;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

fn saved_presets(session: &Session) -> SavedPresets {
    let b = &session.presets;
    let user = |list: &[Preset]| list.iter().filter(|p| !p.system).cloned().collect::<Vec<_>>();
    let name = |list: &[Preset], i: usize| list.get(i).map(|p| p.name.clone()).unwrap_or_default();
    SavedPresets {
        printers: user(&b.printers),
        filaments: user(&b.filaments),
        processes: user(&b.processes),
        printer: name(&b.printers, b.printer.preset),
        process: name(&b.processes, b.process.preset),
        project_filaments: b.project_filaments.iter().map(|s| name(&b.filaments, s.preset)).collect(),
    }
}

fn restore_presets(session: &mut Session, saved: SavedPresets) {
    let b = &mut session.presets;
    let add = |list: &mut Vec<Preset>, user: Vec<Preset>| {
        for p in user {
            if !list.iter().any(|q| q.name == p.name) {
                list.push(Preset { system: false, ..p });
            }
        }
    };
    add(&mut b.printers, saved.printers);
    add(&mut b.filaments, saved.filaments);
    add(&mut b.processes, saved.processes);
    let find = |list: &[Preset], name: &str| list.iter().position(|p| p.name == name);
    if let Some(i) = find(&b.printers, &saved.printer) {
        b.printer.preset = i;
    }
    if let Some(i) = find(&b.processes, &saved.process) {
        b.process.preset = i;
    }
    let filaments: Vec<usize> = saved.project_filaments.iter().filter_map(|n| find(&b.filaments, n)).collect();
    if !filaments.is_empty() {
        b.project_filaments = filaments
            .into_iter()
            .take(CosmicSlicer_config::presets::MAX_FILAMENTS)
            .map(|preset| CosmicSlicer_config::Slot { preset, ..Default::default() })
            .collect();
    }
}

impl eframe::App for CosmicSlicerApp {
    /// Test runs leave egui's layout memory alone too.
    fn persist_egui_memory(&self) -> bool {
        !self.ephemeral
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.ephemeral {
            return;
        }
        if let Ok(prefs) = serde_json::to_string(&self.session.prefs) {
            storage.set_string(PREFS_KEY, prefs);
        }
        if let Ok(presets) = serde_json::to_string(&saved_presets(&self.session)) {
            storage.set_string(PRESETS_KEY, presets);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        // A bug in one frame must never take the user's work down with it.
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.frame(ui))) {
            self.recover(payload);
            ui.ctx().request_repaint();
        }
    }
}

impl CosmicSlicerApp {
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let tokens: &'static Tokens = Tokens::for_theme(self.session.prefs.theme);
        if self.applied_theme != Some(self.session.prefs.theme) {
            theme::apply(&ctx, tokens);
            self.applied_theme = Some(self.session.prefs.theme);
        }
        self.session.now = ctx.input(|i| i.time);
        self.viewport.camera.perspective = self.session.prefs.perspective;
        let mut actions: Vec<Action> = Vec::new();
        for path in std::mem::take(&mut self.startup_files) {
            actions.push(Action::ImportPath(path));
        }
        let (steps, capture) = self.automation.tick(&ctx);
        for step in steps {
            self.run_step(step, &mut actions);
        }
        if let Some(path) = capture {
            self.capture = Some(path);
        }
        self.service_capture(&ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if !dropped.is_empty() {
            self.session.page = Page::Prepare;
            actions.extend(dropped.into_iter().map(Action::ImportPath));
        }
        self.shortcuts(&ctx, &mut actions);
        self.poll_slice(&ctx);
        topbar::resize_edges(&ctx);
        topbar::show(ui, &mut self.session, tokens, &mut self.choices, &mut actions);

        match self.session.page {
            Page::Prepare | Page::Preview => {
                if self.sidebar_visible {
                    egui::Panel::left("sidebar")
                        .resizable(true)
                        .default_size(430.0)
                        .size_range(360.0..=720.0)
                        .frame(ui_kit::frame(Surface::Sidebar, tokens))
                        .show(ui, |ui| {
                            sidebar::show(ui, &mut self.sidebar, &mut self.session, tokens, &mut actions);
                        });
                }
                egui::CentralPanel::default().frame(ui_kit::frame(Surface::Canvas, tokens)).show(ui, |ui| {
                    self.viewport.show(ui, &mut self.session, tokens, self.render_state.as_ref(), &mut actions);
                });
            }
            Page::Home => {
                egui::CentralPanel::default().frame(ui_kit::frame(Surface::Page, tokens).inner_margin(0)).show(
                    ui,
                    |ui| {
                        pages::home(ui, &mut self.session, tokens, &mut actions);
                    },
                );
            }
            Page::Device => {
                egui::CentralPanel::default().frame(ui_kit::frame(Surface::Page, tokens)).show(ui, |ui| {
                    pages::device(ui, &mut self.session, tokens, &mut self.device, &mut actions);
                });
            }
            Page::MultiDevice => {
                egui::CentralPanel::default().frame(ui_kit::frame(Surface::Page, tokens)).show(ui, |ui| {
                    pages::multi_device(ui, tokens, &mut actions);
                });
            }
            Page::Project => {
                egui::CentralPanel::default().frame(ui_kit::frame(Surface::Page, tokens)).show(ui, |ui| {
                    pages::project(ui, &mut self.session, tokens);
                });
            }
        }
        self.dialogs.show(&ctx, &mut self.session, tokens, &mut self.device);
        if std::mem::take(&mut self.dialogs.export_requested) {
            actions.push(Action::ExportGcode);
        }
        for action in actions {
            self.handle(&ctx, action);
        }
        let title = format!("{} — CosmicSlicer", self.session.title());
        if title != self.window_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

#[cfg(test)]
mod smoke {
    //! Draws every page, dialog, settings page and canvas tool headlessly, so a layout bug
    //! (an egui assertion, an index out of range) fails a test instead of a user's session.

    use super::*;
    use crate::dialogs::PendingSwitch;

    fn frames(app: &mut CosmicSlicerApp, ctx: &egui::Context, n: usize) {
        for _ in 0..n {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| app.frame(ui));
            // No renderer takes the textures in a test.
            out.textures_delta.clear();
        }
    }

    fn app_with_objects() -> CosmicSlicerApp {
        let mut app = CosmicSlicerApp::headless(LaunchOptions { ephemeral: true, ..Default::default() });
        for name in ["Cube", "Cylinder", "Sphere", "Cone", "Disc", "Torus"] {
            app.session.add_primitive(name);
        }
        app.session.presets.add_filament();
        app
    }

    #[test]
    fn every_page_draws() {
        let ctx = egui::Context::default();
        for theme in [Theme::Dark, Theme::Light] {
            let mut app = app_with_objects();
            app.session.prefs.theme = theme;
            app.session.prefs.multi_device = true;
            app.session.prefs.show_labels = true;
            for page in [Page::Home, Page::Prepare, Page::Preview, Page::Device, Page::MultiDevice, Page::Project] {
                app.session.page = page;
                frames(&mut app, &ctx, 3);
            }
            app.session.prefs.recent = vec![PathBuf::from("/tmp/a.stl"), PathBuf::from("/tmp/b.obj")];
            app.session.page = Page::Home;
            frames(&mut app, &ctx, 2);
            // An empty project too.
            app.session.delete_all();
            for page in [Page::Prepare, Page::Preview, Page::Project] {
                app.session.page = page;
                frames(&mut app, &ctx, 2);
            }
        }
    }

    #[test]
    fn every_settings_page_draws() {
        let ctx = egui::Context::default();
        let mut app = app_with_objects();
        for advanced in [false, true] {
            app.session.prefs.advanced = advanced;
            for kind in [TabKind::Process, TabKind::Filament, TabKind::Printer] {
                let pages = app.session.schema.pages(kind).len();
                for page in 0..pages {
                    app.dialogs.editor = Some(Editor { kind, filament: 1, page, search: String::new() });
                    frames(&mut app, &ctx, 2);
                }
                app.dialogs.editor = Some(Editor { kind, filament: 0, page: 0, search: "temp".into() });
                frames(&mut app, &ctx, 2);
            }
            // The sidebar's Process pages, globally and for an object, and its search.
            for scope in [0, 1] {
                app.sidebar.scope = scope;
                app.session.select_all();
                for page in 0..app.session.schema.pages(TabKind::Process).len() {
                    app.sidebar.process_page = page;
                    frames(&mut app, &ctx, 2);
                }
            }
            app.sidebar.search = "speed".into();
            frames(&mut app, &ctx, 2);
            app.sidebar.search.clear();
        }
    }

    #[test]
    fn every_dialog_and_tool_draws() {
        let ctx = egui::Context::default();
        let mut app = app_with_objects();
        app.session.select_all();
        let d = &mut app.dialogs;
        d.preferences = true;
        d.about = true;
        d.shortcuts = true;
        d.plate = Some(0);
        d.physical_printer = true;
        d.flushing = true;
        d.calibration = Some("Temperature");
        d.rename = Some((1, "Renamed".into()));
        d.bed_shape = Some([220.0, 220.0, 0.0, 0.0]);
        d.print = true;
        d.save_preset = Some((TabKind::Process, 0, "Mine".into()));
        d.switch = Some(PendingSwitch { kind: TabKind::Process, filament: 0, index: 1 });
        d.compare = Some((TabKind::Process, 0, 3));
        frames(&mut app, &ctx, 3);
        for (tool, ..) in Tool::GIZMOS {
            app.viewport.tool = Some(tool);
            frames(&mut app, &ctx, 2);
        }
        app.viewport.tool = Some(Tool::VariableLayerHeight);
        frames(&mut app, &ctx, 2);
        // Several plates, one locked; the plate chrome draws for each.
        app.session.add_plate();
        app.session.add_plate();
        app.session.toggle_lock(1);
        app.session.page = Page::Preview;
        frames(&mut app, &ctx, 2);
        app.session.page = Page::Prepare;
        frames(&mut app, &ctx, 2);
    }
}
