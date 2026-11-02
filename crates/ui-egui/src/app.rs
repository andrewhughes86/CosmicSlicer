//! The application: lays out the top bar, the page, and the dialogs; runs the panes'
//! actions, the keyboard shortcuts and launch automation; saves preferences and presets.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Event, KeyboardShortcut, Ui, UserData, ViewportCommand};
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

/// Preferences and presets as saved in `settings.json`.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    prefs: Prefs,
    presets: SavedPresets,
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
    /// The language the fonts and texts were set up for (the preference as it was).
    applied_language: Option<String>,
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
    /// Send the current plate to this printer once it is sliced: (printer, start printing).
    send_when_sliced: Option<(String, bool)>,
    /// Plates still to slice after the running one ("Slice all").
    slice_queue: Vec<usize>,
    /// Write every plate's G-code into this folder once "Slice all" is done.
    export_all_to: Option<PathBuf>,
    /// Write the G-code here when the slice finishes, without asking (automation).
    export_to: Option<PathBuf>,
    /// A G-code file being read for the Preview.
    gcode_load: Option<crate::slicer::GcodeLoad>,
    /// Where settings, autosaves and crash reports live; `None` in test runs.
    data_dir: Option<PathBuf>,
    autosaver: Option<crate::recovery::Autosaver>,
    /// The settings file as last written, so unchanged settings are not rewritten.
    saved_settings: String,
    /// The user's preset files, kept in step with the bundle.
    user_files: Option<crate::profiles::UserFiles>,
    user_presets_seen: u64,
}

impl CosmicSlicerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, options: LaunchOptions) -> Self {
        let mut app = Self::headless(options);
        app.render_state = cc.wgpu_render_state.clone();
        if app.ephemeral {
            return app;
        }
        app.data_dir = crate::store::data_dir();
        let Some(dir) = app.data_dir.clone() else {
            app.session.notify(Level::Warning, "No folder for settings was found; settings and autosaves are off.");
            return app;
        };
        crate::recovery::install_crash_reports(&dir);
        // Settings: our own file, else what older versions kept in eframe's storage.
        let mut saved = SavedPresets::default();
        match crate::store::read_json::<Persisted>(&dir.join("settings.json")) {
            Ok(Some(p)) => {
                app.session.prefs = p.prefs;
                saved = p.presets;
            }
            Ok(None) => {
                if let Some(storage) = cc.storage {
                    if let Some(prefs) =
                        storage.get_string(PREFS_KEY).and_then(|s| serde_json::from_str::<Prefs>(&s).ok())
                    {
                        app.session.prefs = prefs;
                    }
                    if let Some(s) =
                        storage.get_string(PRESETS_KEY).and_then(|s| serde_json::from_str::<SavedPresets>(&s).ok())
                    {
                        saved = s;
                    }
                }
            }
            Err(e) => app.session.notify(Level::Warning, e),
        }
        // Presets: the library for the installed printers, then the user's own files.
        app.session.presets = crate::profiles::build(&app.session.prefs);
        let (files, errors) = crate::profiles::UserFiles::load(&dir, &mut app.session.presets);
        for e in errors {
            app.session.notify(Level::Warning, format!("A user preset could not be read: {e}"));
        }
        app.user_files = Some(files);
        restore_presets(&mut app.session, saved);
        // Preferences that act at start.
        let prefs = &app.session.prefs;
        if app.startup_files.is_empty() {
            app.session.page = if prefs.default_page == Page::Prepare { Page::Prepare } else { Page::Home };
        }
        if prefs.remember_print_action {
            app.choices = SliceChoices { slice_all: prefs.slice_all, print: prefs.print_choice.min(3) };
        }
        let level = match prefs.log_level.as_str() {
            "error" => log::LevelFilter::Error,
            "info" => log::LevelFilter::Info,
            "debug" => log::LevelFilter::Debug,
            _ => log::LevelFilter::Warn,
        };
        log::set_max_level(level);
        app.user_presets_seen = user_presets_hash(&app.session);
        // First run: the setup wizard, as OrcaSlicer opens it.
        if !app.session.prefs.setup_done {
            app.dialogs.wizard =
                Some(crate::wizard::Wizard::new(&app.session.prefs, crate::wizard::WizardPage::Printers));
        }
        match crate::recovery::Autosaver::start(&dir) {
            Ok((saver, orphans)) => {
                app.autosaver = Some(saver);
                app.dialogs.restore = orphans.into_iter().next();
            }
            Err(e) => app.session.notify(Level::Warning, format!("Autosave is off: {e}")),
        }
        app
    }

    /// Writes preferences and presets if they changed.
    fn save_settings(&mut self) {
        let Some(dir) = &self.data_dir else { return };
        let persisted = Persisted { prefs: self.session.prefs.clone(), presets: saved_presets(&self.session) };
        let Ok(json) = serde_json::to_string_pretty(&persisted) else { return };
        if json == self.saved_settings {
            return;
        }
        match crate::store::write_atomic(&dir.join("settings.json"), json.as_bytes()) {
            Ok(()) => self.saved_settings = json,
            Err(e) => self.session.notify(Level::Warning, format!("Could not save settings: {e}")),
        }
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
            applied_language: None,
            capture: None,
            capture_requested: false,
            capture_wait: 0,
            startup_files: options.files,
            window_title: String::new(),
            recoveries: 0,
            runner: None,
            last_failed: None,
            export_when_sliced: false,
            send_when_sliced: None,
            slice_queue: Vec::new(),
            export_all_to: None,
            export_to: None,
            gcode_load: None,
            data_dir: None,
            autosaver: None,
            saved_settings: String::new(),
            user_files: None,
            user_presets_seen: 0,
        }
    }

    /// The answer to "restore the project from last time?".
    fn answer_restore(&mut self, restore: bool) {
        let Some(orphan) = self.dialogs.restore.take() else { return };
        if restore {
            match orphan.read() {
                Ok(r) => {
                    crate::recovery::apply(&mut self.session, r);
                    self.session.page = Page::Prepare;
                    self.viewport.fit_pending = true;
                    self.session.notify(Level::Info, format!("Restored \"{}\" from the autosave.", orphan.name));
                }
                Err(e) => {
                    self.session.notify(Level::Error, format!("The autosave could not be restored: {e}"));
                    return;
                }
            }
        }
        orphan.discard();
        // Save the restored project under this session at once.
        if restore && let Some(saver) = self.autosaver.as_mut() {
            let _ = saver.save(&self.session);
        }
    }

    /// Starts slicing the current plate, unless that slice is already running or done.
    fn start_slice(&mut self) {
        self.slice_queue.clear();
        let plate = self.session.plate;
        self.slice_plate(plate);
    }

    /// Slices every plate with something on it, the current one first ("Slice all").
    fn start_slice_all(&mut self) {
        let s = &self.session;
        let mut plates: Vec<usize> =
            (0..s.model.plates).filter(|p| s.model.objects.iter().any(|o| o.plate == *p && o.visible)).collect();
        if plates.is_empty() {
            self.session.notify(Level::Warning, "There is nothing on the plates to slice.");
            return;
        }
        if let Some(i) = plates.iter().position(|p| *p == s.plate) {
            plates.remove(i);
            plates.insert(0, s.plate);
        }
        let first = plates.remove(0);
        self.slice_queue = plates;
        self.slice_plate(first);
    }

    /// Starts the next plate waiting in the queue; at the end, writes "Export all" files.
    fn next_in_queue(&mut self) {
        while !self.slice_queue.is_empty() && self.runner.is_none() {
            let p = self.slice_queue.remove(0);
            self.slice_plate(p);
        }
        if self.runner.is_none()
            && self.slice_queue.is_empty()
            && let Some(dir) = self.export_all_to.take()
        {
            self.write_all_gcode(&dir);
        }
    }

    /// Starts slicing plate `plate`, unless that slice is already running or done.
    fn slice_plate(&mut self, plate: usize) {
        let s = &mut self.session;
        if !s.model.objects.iter().any(|o| o.plate == plate && o.visible) {
            if self.slice_queue.is_empty() && self.export_all_to.is_none() {
                s.notify(Level::Warning, "There is nothing on this plate to slice.");
            }
            return;
        }
        if s.result_for(plate).is_some() {
            if self.export_when_sliced && plate == s.plate {
                self.export_when_sliced = false;
                self.export_gcode();
            }
            return;
        }
        let fingerprint = crate::slicer::fingerprint_for(s, plate);
        if let Some(r) = &self.runner {
            if r.plate == plate && r.revision == s.revision && r.fingerprint == fingerprint {
                return;
            }
            r.progress.cancel();
        }
        // Slicing is when a weak driver or a huge model is most likely to bring things down.
        if let Some(saver) = self.autosaver.as_mut()
            && s.dirty
            && let Err(e) = saver.save(s)
        {
            s.notify(Level::Warning, e);
        }
        match crate::slicer::Runner::start(s, plate) {
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
        if let Some(load) = &self.gcode_load {
            s.slicing = Some((0.0, format!("Reading {}", load.name)));
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            if let Some(result) = load.poll() {
                let name = load.name.clone();
                self.gcode_load = None;
                s.slicing = None;
                match result {
                    Ok(v) => {
                        for w in &v.warnings {
                            s.notify(Level::Warning, w.clone());
                        }
                        s.viewer = Some((name, Arc::new(v)));
                        self.viewport.fit_pending = true;
                    }
                    Err(e) => s.notify(Level::Error, e),
                }
            }
            return;
        }
        if std::mem::take(&mut self.viewport.close_viewer) {
            s.viewer = None;
        }
        // In Preview, a stale or missing slice is redone, as OrcaSlicer's background slicing does
        // (not while a G-code file is open).
        if s.page == Page::Preview
            && s.viewer.is_none()
            && self.runner.is_none()
            && s.result_for(s.plate).is_none()
            && s.model.objects.iter().any(|o| o.plate == s.plate && o.visible)
            && self.last_failed != Some((s.plate, s.revision, crate::slicer::fingerprint(s)))
        {
            self.start_slice();
        }
        let s = &mut self.session;
        let Some(runner) = &self.runner else {
            s.slicing = None;
            if !self.slice_queue.is_empty() || self.export_all_to.is_some() {
                self.next_in_queue();
            }
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
                s.sliced.retain(|st| st.plate != plate && st.revision == revision);
                s.sliced.push(crate::slicer::SliceState { plate, revision, fingerprint, result: Arc::new(sliced) });
                if self.export_when_sliced && plate == s.plate {
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
                self.slice_queue.clear();
                self.export_all_to = None;
                s.notify(Level::Error, e.to_string());
            }
        }
    }

    /// Sends the current plate's slice to the printer asked for: G-code, or for a Bambu
    /// printer the sliced 3MF it prints from.
    fn send_to_printer(&mut self) {
        let Some((name, start)) = self.send_when_sliced.take() else { return };
        let s = &mut self.session;
        let Some(p) = s.prefs.printers.iter().find(|p| p.name == name).cloned() else {
            s.notify(Level::Error, format!("There is no printer called {name} any more."));
            return;
        };
        let Some(result) = s.result_for(s.plate) else { return };
        let base = format!("{}_plate_{}", s.project.name.replace(['/', '\\', ':'], "_"), s.plate + 1);
        let bambu = p.host == CosmicSlicer_connect::HostType::BambuLan;
        let (file, data) = if bambu {
            let thumb = crate::project::plate_image(s, s.plate, 256)
                .and_then(|rgba| CosmicSlicer_engine::thumbnail::png(&rgba, 256, 256));
            let stats = &result.stats;
            let filaments: Vec<CosmicSlicer_connect::bambu::Filament> = (0..s.presets.project_filaments.len())
                .map(|i| CosmicSlicer_connect::bambu::Filament {
                    kind: s.presets.value(s.schema, TabKind::Filament, i, "filament_type"),
                    colour: s.presets.filament_colour(s.schema, i),
                    used_m: stats.filament_mm.get(i).copied().unwrap_or(0.0) / 1000.0,
                    used_g: stats.weight_g.get(i).copied().unwrap_or(0.0),
                })
                .collect();
            match CosmicSlicer_connect::bambu::sliced_3mf(
                result.gcode.as_bytes(),
                thumb.as_deref(),
                stats.total_time,
                &filaments,
            ) {
                Ok(bytes) => (format!("{base}.gcode.3mf"), bytes),
                Err(e) => {
                    s.notify(Level::Error, format!("Could not pack the plate for the printer: {e}"));
                    return;
                }
            }
        } else {
            (format!("{base}.gcode"), result.gcode.as_bytes().to_vec())
        };
        s.notify(Level::Info, format!("Sending {file} to {}…", p.name));
        s.prefs.active_printer = p.name.clone();
        self.device.devices.send(&p, CosmicSlicer_connect::link::Request::Upload { name: file, data, start });
    }

    /// Asks for a folder, then slices every plate and writes each plate's G-code there.
    fn export_all_gcode(&mut self) {
        let Some(dir) = rfd::FileDialog::new().set_title("Export G-code of all plates").pick_folder() else {
            return;
        };
        self.export_all_to = Some(dir);
        self.start_slice_all();
        self.next_in_queue();
    }

    /// Writes each sliced plate's G-code into `dir`, named after the project and plate.
    fn write_all_gcode(&mut self, dir: &std::path::Path) {
        let s = &mut self.session;
        let base = s.project.name.replace(['/', '\\', ':'], "_");
        let mut written = 0;
        for p in 0..s.model.plates {
            let Some(result) = s.result_for(p) else { continue };
            let path = dir.join(format!("{base}_plate_{}.gcode", p + 1));
            match crate::store::write_atomic(&path, result.gcode.as_bytes()) {
                Ok(()) => written += 1,
                Err(e) => s.notify(Level::Error, format!("Could not write {}: {e}", path.display())),
            }
        }
        if written > 0 {
            s.notify(Level::Info, format!("Exported {written} plate(s) to {}", dir.display()));
        }
    }

    fn export_gcode(&mut self) {
        let s = &mut self.session;
        let Some(result) = s.shown_result() else { return };
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
            Step::Volume(spec) => {
                use CosmicSlicer_model::PartKind;
                let Some(id) = self.session.model.objects.last().map(|o| o.id) else { return };
                let (kind, shape) = spec.split_once(':').unwrap_or((spec.as_str(), "cube"));
                let kind = match kind.to_ascii_lowercase().as_str() {
                    "range" => {
                        self.session.add_range(id);
                        return;
                    }
                    "part" => PartKind::Normal,
                    "modifier" => PartKind::Modifier,
                    "negative" => PartKind::Negative,
                    "enforcer" => PartKind::SupportEnforcer,
                    "blocker" => PartKind::SupportBlocker,
                    other => {
                        eprintln!("--add-volume: unknown kind {other}");
                        return;
                    }
                };
                if self.session.add_volume(id, kind, shape).is_none() {
                    eprintln!("--add-volume: there is no primitive called {shape}");
                }
            }
            Step::Open(path) => actions.push(Action::ImportPath(path)),
            Step::Arrange => self.session.arrange(false),
            Step::Calibrate(name) => {
                let test = crate::calibration::Test::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(&name));
                match test {
                    Some(t) => {
                        let d = crate::calibration::Dialog::new(t, &self.session);
                        if let Err(e) = crate::calibration::set_up(&mut self.session, &d) {
                            eprintln!("--calibrate: {e}");
                        }
                        self.viewport.fit_pending = true;
                    }
                    None => eprintln!("--calibrate: there is no calibration called {name}"),
                }
            }
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
                "wizard" => actions.push(Action::Wizard(false)),
                "wizard-filaments" => actions.push(Action::Wizard(true)),
                other => eprintln!("--dialog: unknown dialog {other}"),
            },
            Step::Slice => actions.push(Action::Slice),
            Step::Advanced => self.session.prefs.advanced = true,
            Step::Tool(name) => {
                self.viewport.tool =
                    Tool::GIZMOS.iter().find(|g| g.2.eq_ignore_ascii_case(&name)).map(|g| g.0).or_else(|| {
                        name.eq_ignore_ascii_case("variable layer height").then_some(Tool::VariableLayerHeight)
                    });
            }
            Step::Export(path) => {
                self.export_to = Some(path);
                actions.push(Action::Slice);
            }
            Step::Layer(n) => self.viewport.pending_top = Some(n),
            Step::OpenGcode(path) => actions.push(Action::OpenGcode(Some(path))),
            Step::PreviewOptions => {
                self.viewport.show_travel = true;
                self.viewport.show_retractions = true;
                self.viewport.show_seams = true;
            }
            Step::Autosave => {
                if let Some(saver) = self.autosaver.as_mut()
                    && let Err(e) = saver.save(&self.session)
                {
                    eprintln!("{e}");
                }
            }
            Step::SimulateCrash => {
                // Dies like a driver fault would: no unwinding, no clean-up.
                eprintln!("simulating a crash");
                std::process::abort();
            }
            Step::Restore => self.dialogs.restore_choice = Some(true),
            Step::SimulatePanic => simulated_panic(),
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
            .set_title("Import 3MF/STL/STEP/OBJ/AMF")
            .add_filter("Supported files", &CosmicSlicer_model::import_extensions())
            .add_filter("3MF", &["3mf"])
            .add_filter("STL", &["stl"])
            .add_filter("STEP", &["step", "stp"])
            .add_filter("OBJ", &["obj"])
            .add_filter("ZIP archive", &["zip"])
            .pick_files();
        for path in files.unwrap_or_default() {
            self.session.import(&path);
        }
        self.viewport.fit_pending = true;
    }

    /// Opens a 3MF project, replacing the current one (undoably); a 3MF holding only a model is
    /// imported instead.
    fn open_project(&mut self, path: &std::path::Path) {
        if !crate::project::is_project_file(path) {
            self.session.import(path);
            self.viewport.fit_pending = true;
            return;
        }
        match crate::project::open(&self.session, path) {
            Ok((mut restored, warnings)) => {
                // Preferences › Project › Load behaviour.
                let differs = restored.presets.printer.preset != self.session.presets.printer.preset
                    || restored.presets.process.preset != self.session.presets.process.preset;
                let geometry_only = match self.session.prefs.load_behaviour {
                    3 => true,
                    1 if differs => self.ask_geometry_only(),
                    2 => self.ask_geometry_only(),
                    _ => false,
                };
                if geometry_only {
                    restored.presets = self.session.presets.clone();
                    restored.bed_type = self.session.bed_type;
                }
                crate::recovery::apply(&mut self.session, restored);
                self.session.dirty = false;
                self.session.page = Page::Prepare;
                self.session.remember(path);
                for w in warnings {
                    self.session.notify(Level::Warning, w);
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("project").to_owned();
                self.session.notify(Level::Info, format!("Opened {name}"));
                self.viewport.fit_pending = true;
            }
            Err(e) => self.session.notify(Level::Error, e),
        }
    }

    /// Asks whether to keep the current presets when opening a project (Preferences › Project
    /// › Load behaviour); true keeps them (geometry only).
    fn ask_geometry_only(&self) -> bool {
        if self.ephemeral {
            return false;
        }
        let answer = rfd::MessageDialog::new()
            .set_title("Open project")
            .set_description("Load the project's printer, filament and process settings too, or only its objects?")
            .set_buttons(rfd::MessageButtons::OkCancelCustom("Load all".into(), "Geometry only".into()))
            .show();
        matches!(answer, rfd::MessageDialogResult::Custom(ref s) if s == "Geometry only")
    }

    fn save_project(&mut self, path: &std::path::Path) {
        let s = &mut self.session;
        match crate::project::save(s, path) {
            Ok(()) => {
                s.project.path = Some(path.to_owned());
                if let Some(stem) = path.file_stem().and_then(|n| n.to_str()) {
                    s.project.name = stem.to_owned();
                }
                s.dirty = false;
                s.remember(path);
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("project").to_owned();
                s.notify(Level::Info, format!("Saved {name}"));
            }
            Err(e) => s.notify(Level::Error, e),
        }
    }

    fn save_project_as(&mut self) {
        let name = format!("{}.3mf", self.session.project.name.trim());
        let picked = rfd::FileDialog::new()
            .set_title("Save Project as")
            .add_filter("Project (.3mf)", &["3mf"])
            .set_file_name(name)
            .save_file();
        if let Some(mut path) = picked {
            if !is_3mf(&path) {
                path.set_extension("3mf");
            }
            self.save_project(&path);
        }
    }

    /// Imports OrcaSlicer or CosmicSlicer preset files as user presets.
    fn import_config(&mut self) {
        let files =
            rfd::FileDialog::new().set_title("Import Configs").add_filter("Preset (.json)", &["json"]).pick_files();
        let s = &mut self.session;
        for path in files.unwrap_or_default() {
            let result = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| CosmicSlicer_config::userfiles::from_json(&t, None));
            match result {
                Ok((kind, p)) => {
                    let name = p.name.clone();
                    let index = s.presets.add_user(kind, p);
                    if kind == TabKind::Printer {
                        s.presets.select(kind, 0, index);
                    }
                    s.notify(Level::Info, format!("Imported {} preset \"{name}\".", kind.title().to_lowercase()));
                }
                Err(e) => s.notify(Level::Error, format!("{}: {e}", path.display())),
            }
        }
    }

    /// Writes the selected preset of `kind` as an OrcaSlicer-style JSON file.
    fn export_preset(&mut self, kind: TabKind, filament: usize) {
        let s = &mut self.session;
        let Some(p) = s.presets.preset(kind, filament).cloned() else { return };
        let mut p = p;
        // Export what is on screen, edits included, as a preset based on the selected one.
        p.config = s.presets.effective(kind, filament);
        if p.system {
            p.inherits = p.name.clone();
            p.system = false;
        }
        let text = CosmicSlicer_config::userfiles::to_text(s.schema, kind, &p);
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export preset")
            .add_filter("Preset (.json)", &["json"])
            .set_file_name(CosmicSlicer_config::userfiles::file_name(&p.name))
            .save_file()
        else {
            return;
        };
        match crate::store::write_atomic(&path, text.as_bytes()) {
            Ok(()) => s.notify(Level::Info, format!("Exported {}", path.display())),
            Err(e) => s.notify(Level::Error, format!("Could not write {}: {e}", path.display())),
        }
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
        // Every printed part, where it stands.
        let meshes: Vec<(&CosmicSlicer_model::Mesh, glam::Mat4)> = objects.iter().flat_map(|o| o.solids()).collect();
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
            Action::OpenProject => {
                let picked =
                    rfd::FileDialog::new().set_title("Open Project").add_filter("Project (.3mf)", &["3mf"]).pick_file();
                if let Some(path) = picked {
                    self.open_project(&path);
                }
            }
            Action::OpenProjectPath(path) => self.open_project(&path),
            Action::PickFont => {
                let picked = rfd::FileDialog::new()
                    .set_title("Load font")
                    .add_filter("Font", &["ttf", "otf", "ttc"])
                    .pick_file();
                if let Some(path) = picked {
                    match std::fs::read(&path) {
                        Ok(bytes) if bytes.len() < 64 << 20 => {
                            let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("Font").to_owned();
                            let ts = &mut self.viewport.tools.text;
                            ts.custom = Some((name, Arc::new(bytes)));
                            ts.font = crate::emboss::FONTS.len();
                        }
                        Ok(_) => s.notify(Level::Warning, "That font file is too large."),
                        Err(e) => s.notify(Level::Error, format!("could not read {}: {e}", path.display())),
                    }
                }
            }
            Action::EmbossSvg => {
                let picked = rfd::FileDialog::new().set_title("Emboss an SVG").add_filter("SVG", &["svg"]).pick_file();
                if let Some(path) = picked {
                    let ts = self.viewport.tools.text.clone();
                    let first = s.selection.first().copied();
                    match std::fs::read(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|b| crate::emboss::svg(&b, ts.height))
                    {
                        Ok(loops) => {
                            let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("SVG").to_owned();
                            s.add_emboss(&name, &loops, ts.depth, ts.mode, first);
                        }
                        Err(e) => s.notify(Level::Error, e),
                    }
                }
            }
            Action::AddVolumeFile(id, kind) => {
                let picked = rfd::FileDialog::new()
                    .set_title(format!("Load {}", kind.label().to_lowercase()))
                    .add_filter("Supported files", &CosmicSlicer_model::import_extensions())
                    .pick_file();
                if let Some(path) = picked {
                    s.add_volume_file(id, kind, &path);
                }
            }
            Action::SaveProject => match s.project.path.clone().filter(|p| is_3mf(p)) {
                Some(path) => self.save_project(&path),
                None => self.save_project_as(),
            },
            Action::SaveProjectAs => self.save_project_as(),
            Action::Import => self.import_dialog(),
            Action::ImportPath(path) => {
                if is_3mf(&path) {
                    self.open_project(&path);
                } else {
                    s.import(&path);
                    self.viewport.fit_pending = true;
                }
            }
            Action::ExportStl => self.export_stl(),
            Action::ExportGcode => {
                if s.result_for(s.plate).is_some() {
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
                s.viewer = None;
                s.page = Page::Preview;
                self.last_failed = None;
                self.start_slice();
            }
            Action::SliceAll => {
                s.viewer = None;
                s.page = Page::Preview;
                self.last_failed = None;
                self.start_slice_all();
            }
            Action::ExportAllGcode => self.export_all_gcode(),
            Action::AddAttachment(kind) => {
                let mut dialog = rfd::FileDialog::new().set_title(format!("Add to {kind}"));
                if kind == "Model Pictures" {
                    dialog = dialog.add_filter("Pictures", &["png", "jpg", "jpeg"]);
                }
                for path in dialog.pick_files().unwrap_or_default() {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_owned();
                    match std::fs::metadata(&path) {
                        Ok(m) if m.len() > 64 << 20 => {
                            s.notify(Level::Warning, format!("{name} is over 64 MB; it was not added."))
                        }
                        Ok(_) => match std::fs::read(&path) {
                            Ok(bytes) => {
                                s.project.files.push((kind.to_owned(), name, Arc::new(bytes)));
                                s.dirty = true;
                            }
                            Err(e) => s.notify(Level::Error, format!("Could not read {name}: {e}")),
                        },
                        Err(e) => s.notify(Level::Error, format!("Could not read {name}: {e}")),
                    }
                }
            }
            Action::SaveAttachment(i) => {
                if let Some((_, name, bytes)) = s.project.files.get(i).cloned()
                    && let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file()
                    && let Err(e) = crate::store::write_atomic(&path, &bytes)
                {
                    s.notify(Level::Error, format!("Could not write {}: {e}", path.display()));
                }
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
            Action::Calibration(name) => {
                self.dialogs.calibration =
                    crate::calibration::Test::from_name(name).map(|t| crate::calibration::Dialog::new(t, s));
            }
            Action::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            Action::View(v) => self.viewport.set_view(v),
            Action::ZoomFit => self.viewport.fit_pending = true,
            Action::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Action::Rename(id) => {
                let name = s.model.get(id).map(|o| o.name.clone()).unwrap_or_default();
                self.dialogs.rename = Some((id, name));
            }
            Action::Wizard(filaments) => {
                let page =
                    if filaments { crate::wizard::WizardPage::Filaments } else { crate::wizard::WizardPage::Printers };
                self.dialogs.wizard = Some(crate::wizard::Wizard::new(&s.prefs, page));
            }
            Action::ImportConfig => self.import_config(),
            Action::OpenGcode(path) => {
                let path = path.or_else(|| {
                    rfd::FileDialog::new()
                        .set_title("Open G-code")
                        .add_filter("G-code", &["gcode", "gco", "g", "bgcode"])
                        .pick_file()
                });
                if let Some(path) = path {
                    self.gcode_load = Some(crate::slicer::GcodeLoad::start(s, &path));
                    s.page = Page::Preview;
                }
            }
            Action::ExportPreset(kind, filament) => self.export_preset(kind, filament),
            Action::Unavailable(what) => s.notify(Level::Warning, format!("{what} arrives in a later step.")),
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use crate::shortcuts::Scope;
        let typing = ctx.egui_wants_keyboard_input();
        let page = self.session.page;
        let painting = page == Page::Prepare && self.viewport.tool.and_then(Tool::paint_kind).is_some();
        let table = crate::shortcuts::by_priority();
        let mut fired: Vec<&str> = Vec::new();
        ctx.input_mut(|i| {
            for s in &table {
                let active = match s.scope {
                    Scope::Global => true,
                    Scope::App => !typing,
                    Scope::Canvas => !typing && page.is_3d(),
                    Scope::Prepare => {
                        // While painting, the brush keys take the keys they share with the tools.
                        let shadowed = painting
                            && table.iter().any(|p| p.scope == Scope::Painting && p.key == s.key && p.mods == s.mods);
                        !typing && page == Page::Prepare && !shadowed
                    }
                    Scope::Preview => !typing && page == Page::Preview,
                    Scope::Painting => !typing && painting,
                };
                if active && i.consume_shortcut(&KeyboardShortcut::new(s.mods, s.key)) {
                    fired.push(s.id);
                }
            }
        });
        let s = &mut self.session;
        for name in fired {
            match name {
                "new" => actions.push(Action::NewProject),
                "open" => actions.push(Action::OpenProject),
                "save" => actions.push(Action::SaveProject),
                "save-as" => actions.push(Action::SaveProjectAs),
                "import" => actions.push(Action::Import),
                "slice" => actions.push(Action::Slice),
                "export" => actions.push(Action::ExportGcode),
                "print" => actions.push(Action::Print),
                "prefs" => actions.push(Action::Preferences),
                "quit" => actions.push(Action::Quit),
                "undo" => s.undo(),
                "redo" => s.redo(),
                "select-all" => s.select_all(),
                "select-all-plates" => s.selection = s.model.objects.iter().map(|o| o.id).collect(),
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
                    if !self.dialogs.close_one() && !crate::dialogs::Dialogs::close_units(s) {
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
                "search" => {
                    s.page = if s.page.is_3d() { s.page } else { Page::Prepare };
                    self.sidebar.focus_search = true;
                }
                "reload-device" => {
                    self.device.devices = Default::default();
                }
                "fit" => self.viewport.fit_pending = true,
                "zoom-in" => self.viewport.camera.zoom(0.8, None),
                "zoom-out" => self.viewport.camera.zoom(1.25, None),
                "projection" => s.prefs.perspective = !s.prefs.perspective,
                "labels" => s.prefs.show_labels = !s.prefs.show_labels,
                "arrange" => s.arrange(true),
                "arrange-plate" => s.arrange(false),
                "add-instance" => s.add_instance(),
                "remove-instance" => s.remove_instance(),
                "toggle-printable" => {
                    for id in s.selection.clone() {
                        s.toggle_visible(id);
                    }
                }
                "orient" => s.auto_orient(),
                "orient-plate" => {
                    let kept = std::mem::take(&mut s.selection);
                    s.auto_orient();
                    s.selection = kept;
                }
                "rotate-left" | "rotate-right" if !s.selection.is_empty() => {
                    let angle = if name == "rotate-left" { 45f32 } else { -45f32 }.to_radians();
                    s.checkpoint();
                    let ids = s.selection.clone();
                    for id in &ids {
                        if let Some(o) = s.model.get_mut(*id) {
                            let c = o.bounds().centre().truncate();
                            o.turn_about(angle, c, c);
                        }
                    }
                    s.settle(&ids);
                }
                "layer-up" => self.viewport.layers.top = (self.viewport.layers.top + 1).min(self.viewport.layers.count),
                "layer-down" => {
                    self.viewport.layers.top =
                        self.viewport.layers.top.saturating_sub(1).max(self.viewport.layers.bottom)
                }
                "go-to-layer" => self.dialogs.go_to_layer = Some(self.viewport.layers.top.to_string()),
                "moves-left" => self.viewport.moves = (self.viewport.moves - 0.01).max(0.0),
                "moves-right" => self.viewport.moves = (self.viewport.moves + 0.01).min(1.0),
                "moves-start" => self.viewport.moves = 0.0,
                "moves-end" => self.viewport.moves = 1.0,
                "brush-circle" => self.viewport.tools.brush = crate::viewport::tools::Brush::Circle,
                "brush-sphere" => self.viewport.tools.brush = crate::viewport::tools::Brush::Sphere,
                "brush-fill" => self.viewport.tools.brush = crate::viewport::tools::Brush::Fill,
                view if view.starts_with("view") => {
                    let i: usize = view.get(4..).and_then(|v| v.parse().ok()).unwrap_or(0);
                    if let Some((v, ..)) = View::ALL.get(i) {
                        self.viewport.set_view(*v);
                    }
                }
                tool if tool.starts_with("tool-") => {
                    let wanted = tool.get(5..).unwrap_or("");
                    let found = Tool::GIZMOS.iter().find(|g| g.2.eq_ignore_ascii_case(wanted)).map(|g| g.0);
                    if !s.model.objects.is_empty() {
                        self.viewport.tool = if self.viewport.tool == found { None } else { found };
                    }
                }
                nudge if nudge.starts_with("nudge-") && !s.selection.is_empty() => {
                    let step = if nudge.ends_with("-fine") { 1.0 } else { 10.0 };
                    let d = match nudge.trim_end_matches("-fine") {
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
        if self.runner.is_some() || self.gcode_load.is_some() {
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

/// Fails on purpose (`--simulate-panic`), to check that a bug in a frame is recovered from and
/// reported: the one place a panic is wanted.
#[allow(clippy::panic)]
fn simulated_panic() -> ! {
    std::panic::panic_any("simulated failure (--simulate-panic)")
}

fn saved_presets(session: &Session) -> SavedPresets {
    let b = &session.presets;
    let name = |list: &[Preset], i: usize| list.get(i).map(|p| p.name.clone()).unwrap_or_default();
    // User presets live in their own files now; only the selections are kept here.
    SavedPresets {
        printers: Vec::new(),
        filaments: Vec::new(),
        processes: Vec::new(),
        printer: name(&b.printers, b.printer.preset),
        process: name(&b.processes, b.process.preset),
        project_filaments: b.project_filaments.iter().map(|s| name(&b.filaments, s.preset)).collect(),
    }
}

/// Puts back the selections, and user presets an older version kept in the settings file.
fn restore_presets(session: &mut Session, saved: SavedPresets) {
    let b = &mut session.presets;
    for (kind, list) in
        [(TabKind::Printer, saved.printers), (TabKind::Filament, saved.filaments), (TabKind::Process, saved.processes)]
    {
        for p in list {
            if !b.list(kind).iter().any(|q| q.name == p.name) {
                b.add_user(kind, Preset { system: false, ..p });
            }
        }
    }
    crate::profiles::select_by_name(b, &saved.printer, &saved.process, &saved.project_filaments);
}

/// Changes when any user preset is added, changed, renamed or deleted.
fn user_presets_hash(session: &Session) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for kind in [TabKind::Printer, TabKind::Filament, TabKind::Process] {
        for p in session.presets.list(kind).iter().filter(|p| !p.system) {
            format!("{}{:?}{:?}", p.name, p.inherits, p.config).hash(&mut h);
        }
    }
    h.finish()
}

impl eframe::App for CosmicSlicerApp {
    /// Test runs leave egui's layout memory alone too.
    fn persist_egui_memory(&self) -> bool {
        !self.ephemeral
    }

    /// eframe calls this every 30 s and on exit; settings go to our own file, written safely.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if !self.ephemeral {
            self.save_settings();
        }
    }

    /// A clean exit leaves nothing to recover.
    fn on_exit(&mut self) {
        if let Some(saver) = self.autosaver.take() {
            saver.finish();
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
        if self.applied_language.as_deref() != Some(self.session.prefs.language.as_str()) {
            let chosen = self.session.prefs.language.clone();
            let code = if chosen.is_empty() {
                crate::i18n::system_language().unwrap_or_else(|| "en".into())
            } else {
                chosen.clone()
            };
            crate::i18n::set_language(&code);
            if !crate::i18n::install_fonts(&ctx, &code) {
                self.session.notify(Level::Warning, "No font for this language was found on the system; install Noto Sans CJK (or Noto Sans Thai) to see its text.");
            }
            self.applied_language = Some(chosen);
        }
        if self.session.prefs.remember_print_action {
            self.session.prefs.slice_all = self.choices.slice_all;
            self.session.prefs.print_choice = self.choices.print;
        }
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
            // G-code opens in the Preview; models go on the plate.
            let (gcode, models): (Vec<PathBuf>, Vec<PathBuf>) = dropped.into_iter().partition(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "gcode" | "gco" | "g"))
            });
            if !models.is_empty() {
                self.session.page = Page::Prepare;
                actions.extend(models.into_iter().map(Action::ImportPath));
            }
            if let Some(g) = gcode.into_iter().next() {
                actions.push(Action::OpenGcode(Some(g)));
            }
        }
        self.shortcuts(&ctx, &mut actions);
        self.session.sync_instances();
        self.poll_slice(&ctx);
        if let Some(saver) = self.autosaver.as_mut()
            && let Some(e) = saver.tick(&self.session, self.session.now)
        {
            self.session.notify(Level::Warning, e);
        }
        // User presets are written as soon as they change.
        let seen = user_presets_hash(&self.session);
        if seen != self.user_presets_seen {
            self.user_presets_seen = seen;
            if let Some(files) = self.user_files.as_mut() {
                for e in files.sync(&self.session.presets) {
                    self.session.notify(Level::Warning, e);
                }
            }
        }
        if let Some(wizard) = self.dialogs.wizard.as_mut()
            && let Some(outcome) = wizard.show(&ctx, tokens)
        {
            self.dialogs.wizard = None;
            self.session.prefs.setup_done = true;
            if let crate::wizard::Outcome::Finished { printers, filaments } = outcome {
                self.session.prefs.installed_printers = printers;
                self.session.prefs.visible_filaments = filaments;
                self.session.presets = crate::profiles::rebuild(&self.session.presets, &self.session.prefs);
                self.session.sliced.clear();
                self.save_settings();
            }
        }
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
                    pages::multi_device(ui, &mut self.session, tokens, &mut self.device, &mut actions);
                });
            }
            Page::Project => {
                egui::CentralPanel::default().frame(ui_kit::frame(Surface::Page, tokens)).show(ui, |ui| {
                    pages::project(ui, &mut self.session, tokens, &mut actions);
                });
            }
        }
        self.dialogs.show(&ctx, &mut self.session, tokens, &mut self.device);
        if let Some(layer) = self.dialogs.jump_to_layer.take() {
            let l = &mut self.viewport.layers;
            l.top = layer.min(l.count).max(l.bottom.min(l.count));
        }
        if std::mem::take(&mut self.dialogs.calibration_ready) {
            self.session.page = Page::Prepare;
            self.viewport.fit_pending = true;
        }
        if std::mem::take(&mut self.dialogs.export_requested) {
            actions.push(Action::ExportGcode);
        }
        if let Some(request) = self.dialogs.send_requested.take() {
            self.send_when_sliced = Some(request);
            if self.session.result_for(self.session.plate).is_none() {
                self.start_slice();
                if self.runner.is_none() {
                    self.send_when_sliced = None;
                }
            }
        }
        if self.send_when_sliced.is_some() {
            if self.session.result_for(self.session.plate).is_some() {
                self.send_to_printer();
            } else if self.runner.is_none() {
                // The slice failed (its error is shown); nothing is sent.
                self.send_when_sliced = None;
            }
        }
        self.device.devices.poll(&ctx, &mut self.session);
        if let Some((kind, filament)) = self.dialogs.export_preset.take() {
            actions.push(Action::ExportPreset(kind, filament));
        }
        if let Some(restore) = self.dialogs.restore_choice.take() {
            self.answer_restore(restore);
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

fn is_3mf(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("3mf"))
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
        app.session.add_event(0, 1.0, crate::state::EventKind::ColorChange, String::new());
        app.session.add_event(0, 2.0, crate::state::EventKind::Custom, "; x".into());
        // One object with every kind of volume and a height range, for the object list.
        if let Some(id) = app.session.model.objects.first().map(|o| o.id) {
            for kind in CosmicSlicer_model::PartKind::ALL {
                app.session.add_volume(id, kind, "Cube");
            }
            app.session.add_range(id);
        }
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
            // A volume and a height range picked, in the Process panel's Objects mode.
            if let Some(id) = app.session.model.objects.first().map(|o| o.id) {
                app.session.page = Page::Prepare;
                for sub in [crate::state::Sub::Volume(1), crate::state::Sub::Range(0), crate::state::Sub::Volume(99)] {
                    app.session.selection = vec![id];
                    app.sidebar.sub = Some((id, sub));
                    app.sidebar.scope = 1;
                    frames(&mut app, &ctx, 2);
                }
                app.sidebar.folded.push(id);
                frames(&mut app, &ctx, 1);
            }
            // A project name too long for the title bar.
            app.session.project.name = "A very long project name ".repeat(12);
            app.session.page = Page::Prepare;
            frames(&mut app, &ctx, 2);
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
        d.calibration = Some(crate::calibration::Dialog::new(crate::calibration::Test::Temperature, &app.session));
        d.rename = Some((1, "Renamed".into()));
        d.bed_shape = Some([220.0, 220.0, 0.0, 0.0]);
        d.print = true;
        d.save_preset = Some((TabKind::Process, 0, "Mine".into()));
        d.switch = Some(PendingSwitch { kind: TabKind::Process, filament: 0, index: 1 });
        d.compare = Some((TabKind::Process, 0, 3));
        d.restore = Some(crate::recovery::Orphan {
            snapshot: PathBuf::from("/nonexistent/a.cosmicsave"),
            lock: PathBuf::from("/nonexistent/a.lock"),
            name: "Bracket".into(),
            objects: 3,
            age: 125,
            report: Some(PathBuf::from("/nonexistent/crash.txt")),
        });
        app.session.units_prompt = Some(app.session.selection.clone());
        frames(&mut app, &ctx, 3);
        for page in [crate::wizard::WizardPage::Printers, crate::wizard::WizardPage::Filaments] {
            app.dialogs.wizard = Some(crate::wizard::Wizard::new(&app.session.prefs, page));
            frames(&mut app, &ctx, 2);
        }
        app.dialogs.wizard = None;
        // A user preset to rename and delete.
        let schema = app.session.schema;
        app.session.presets.set_value(schema, TabKind::Process, 0, "wall_loops", "3".into());
        assert!(app.session.presets.save_as(TabKind::Process, 0, "Mine").is_ok());
        let mine = app.session.presets.process.preset;
        app.dialogs.editor = Some(Editor { kind: TabKind::Process, filament: 0, page: 0, search: String::new() });
        app.dialogs.rename_preset = Some((TabKind::Process, mine, "Mine 2".into()));
        app.dialogs.delete_preset = Some((TabKind::Process, mine));
        frames(&mut app, &ctx, 2);
        // Restoring a snapshot that is gone is an error notice, not a crash.
        app.dialogs.restore_choice = Some(true);
        frames(&mut app, &ctx, 2);
        assert!(app.dialogs.restore.is_none());
        // Paint, picks, a cut plane and connectors, so each tool has something to show.
        if let Some(id) = app.session.selection.first().copied() {
            use CosmicSlicer_model::threemf::PaintKind;
            for kind in [PaintKind::Support, PaintKind::Seam, PaintKind::Fuzzy, PaintKind::Color] {
                app.session.paint(id, kind, &[0, 1, 2], 1);
            }
            let at = crate::viewport::tools::SurfacePoint {
                id,
                tri: Some(0),
                point: glam::Vec3::ZERO,
                normal: glam::Vec3::Z,
            };
            app.viewport.tools.hover = Some(at);
            app.viewport.tools.measure = vec![at, at];
            app.session.add_brim_ear(id, glam::Vec3::new(100.0, 100.0, 0.0), 3.0);
            if let Some(o) = app.session.model.get(id) {
                app.viewport.tools.cut.follow(o);
            }
            app.viewport.tools.cut.opts.connectors.push(glam::Vec3::new(100.0, 100.0, 5.0));
        }
        for (tool, ..) in Tool::GIZMOS {
            app.viewport.tool = Some(tool);
            frames(&mut app, &ctx, 2);
        }
        // The text tool with a loaded font, in each mode.
        app.viewport.tools.text.custom = Some(("Mono".into(), Arc::new(crate::emboss::FONTS[1].1.to_vec())));
        app.viewport.tools.text.font = crate::emboss::FONTS.len();
        for mode in [crate::viewport::tools::EmbossMode::Engrave, crate::viewport::tools::EmbossMode::Object] {
            app.viewport.tools.text.mode = mode;
            app.viewport.tool = Some(Tool::Text);
            frames(&mut app, &ctx, 2);
        }
        app.viewport.tool = Some(Tool::VariableLayerHeight);
        frames(&mut app, &ctx, 2);
        // An object with instances in the list.
        app.session.add_instance();
        frames(&mut app, &ctx, 2);
        // The arrange settings, and a prime tower on the plate.
        app.viewport.tool = None;
        app.viewport.arrange_settings = true;
        app.session.presets.add_filament();
        let schema = app.session.schema;
        app.session.presets.set_value(schema, TabKind::Process, 0, "enable_prime_tower", "1".into());
        frames(&mut app, &ctx, 2);
        app.viewport.arrange_settings = false;
        // Several plates, one locked; the plate chrome draws for each.
        app.session.add_plate();
        app.session.add_plate();
        app.session.toggle_lock(1);
        app.session.page = Page::Preview;
        frames(&mut app, &ctx, 2);
        // A G-code file in the Preview, with every mark on.
        let text = "G90\nM83\nG1 Z0.2\nG1 X10 Y10\n;TYPE:Outer wall\nG1 X20 Y10 E1\nG1 E-0.8\nG1 X30 Y30\n".to_owned();
        let viewed = CosmicSlicer_engine::view_gcode(text, &CosmicSlicer_config::Config::default(), &[]);
        app.session.viewer = Some(("test.gcode".into(), Arc::new(viewed)));
        app.viewport.show_travel = true;
        app.viewport.show_retractions = true;
        app.viewport.show_seams = true;
        frames(&mut app, &ctx, 2);
        app.viewport.close_viewer = true;
        frames(&mut app, &ctx, 2);
        assert!(app.session.viewer.is_none());
        app.session.page = Page::Prepare;
        frames(&mut app, &ctx, 2);
    }

    #[test]
    fn the_device_pages_and_printer_dialogs_draw() {
        use CosmicSlicer_connect::{HostType, PhysicalPrinter};
        let ctx = egui::Context::default();
        let mut app = app_with_objects();
        // Nobody answers on port 9 of the loopback: the pages show the printers offline.
        app.session.prefs.printers = vec![
            PhysicalPrinter {
                name: "Voron".into(),
                host: HostType::OctoPrint,
                address: "127.0.0.1:9".into(),
                ..Default::default()
            },
            PhysicalPrinter {
                name: "P1S".into(),
                host: HostType::BambuLan,
                address: "127.0.0.1".into(),
                serial: "01P00A000000000".into(),
                access_code: "12345678".into(),
                ..Default::default()
            },
            PhysicalPrinter { name: "Cloud".into(), host: HostType::Obico, address: "x".into(), ..Default::default() },
        ];
        for active in ["Voron", "P1S", "Cloud"] {
            app.session.prefs.active_printer = active.into();
            for page in [Page::Device, Page::MultiDevice] {
                app.session.page = page;
                frames(&mut app, &ctx, 2);
            }
        }
        // The camera on, then off.
        if let Some(p) = app.session.prefs.printers.first().cloned() {
            app.device.devices.camera(Some(&p));
            app.session.page = Page::Device;
            frames(&mut app, &ctx, 2);
            app.device.devices.camera(None);
        }
        // The printer dialog for each kind of host, with search results and a test result.
        app.dialogs.physical_printer = true;
        for host in HostType::ALL {
            app.device.edit = Some((Some(1), PhysicalPrinter { name: "Edited".into(), host, ..Default::default() }));
            app.device.found = Ok(vec![CosmicSlicer_connect::bambu::Found {
                ip: "192.168.1.9".into(),
                serial: "01S".into(),
                name: "X1C".into(),
                model: "BL-P001".into(),
            }]);
            app.device.test_result = Some(Err("refused".into()));
            frames(&mut app, &ctx, 2);
        }
        // The Print dialog, with no printers and with some.
        app.dialogs.physical_printer = false;
        app.dialogs.print = true;
        frames(&mut app, &ctx, 2);
        let printers = std::mem::take(&mut app.session.prefs.printers);
        app.dialogs.print = true;
        frames(&mut app, &ctx, 2);
        app.session.prefs.printers = printers;
        // An empty list after all: the Device page invites adding one.
        app.session.prefs.printers.clear();
        app.session.page = Page::Device;
        frames(&mut app, &ctx, 2);
    }

    #[test]
    fn print_plate_slices_and_uploads_to_klipper() {
        use CosmicSlicer_connect::{HostType, PhysicalPrinter};
        use std::io::{BufRead, BufReader, Read, Write};
        // A fake Moonraker: answers /server/info, and takes the upload.
        let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") else { return };
        let Ok(addr) = listener.local_addr() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(20) {
                let Ok(mut s) = stream else { continue };
                let Ok(clone) = s.try_clone() else { continue };
                let mut r = BufReader::new(clone);
                let (mut head, mut len) = (String::new(), 0usize);
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                        break;
                    }
                    if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    head.push_str(&l);
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let reply = if head.starts_with("GET /server/info") {
                    r#"{"result":{"klippy_state":"ready"}}"#
                } else {
                    r#"{"result":{}}"#
                };
                let _ = s
                    .write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).as_bytes());
                if head.starts_with("POST /server/files/upload") {
                    let _ = tx.send(body);
                }
            }
        });
        let ctx = egui::Context::default();
        let mut app = CosmicSlicerApp::headless(LaunchOptions { ephemeral: true, ..Default::default() });
        app.session.add_primitive("Cube");
        app.session.prefs.printers = vec![PhysicalPrinter {
            name: "Voron".into(),
            host: HostType::OctoPrint,
            address: addr.to_string(),
            ..Default::default()
        }];
        app.dialogs.send_requested = Some(("Voron".into(), true));
        let start = std::time::Instant::now();
        let mut body = None;
        while start.elapsed() < std::time::Duration::from_secs(120) && body.is_none() {
            frames(&mut app, &ctx, 1);
            body = rx.try_recv().ok();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let text = body.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        assert!(text.contains("filename=\"Untitled_plate_1.gcode\""), "no upload arrived");
        assert!(text.contains("name=\"print\"\r\n\r\ntrue") && text.contains("G1 "), "{}", text.len());
    }

    #[test]
    fn every_calibration_dialog_draws() {
        use crate::calibration::{Dialog, Test};
        let ctx = egui::Context::default();
        let mut app = app_with_objects();
        for test in Test::ALL {
            for printed in [false, true] {
                let mut d = Dialog::new(test, &app.session);
                d.printed = printed;
                app.dialogs.calibration = Some(d);
                frames(&mut app, &ctx, 2);
            }
        }
        // The menu's names reach the dialog.
        app.handle(&ctx, Action::Calibration(Test::PaPattern.label()));
        assert!(app.dialogs.calibration.as_ref().is_some_and(|d| d.test == Test::PaPattern));
    }

    #[test]
    fn pages_draw_in_other_languages_and_at_any_scale() {
        let ctx = egui::Context::default();
        let mut app = app_with_objects();
        let png = CosmicSlicer_engine::thumbnail::png(&[200; 16 * 16 * 4], 16, 16).unwrap_or_default();
        app.session.project.files = vec![
            ("Model Pictures".into(), "front.png".into(), Arc::new(png)),
            ("Model Pictures".into(), "broken.jpg".into(), Arc::new(vec![1, 2, 3])),
            ("Bill of Materials".into(), "bom.csv".into(), Arc::new(b"a,b".to_vec())),
        ];
        for (lang, scale) in [("de", 1.0), ("ja", 1.5), ("th", 2.0), ("", 1.25)] {
            app.session.prefs.language = lang.into();
            // Moving to another monitor changes the scale between frames.
            ctx.set_pixels_per_point(scale);
            for page in [Page::Home, Page::Prepare, Page::Preview, Page::Device, Page::Project] {
                app.session.page = page;
                frames(&mut app, &ctx, 2);
            }
            app.dialogs.preferences = true;
            for p in 0..6 {
                app.dialogs.preferences_page = p;
                frames(&mut app, &ctx, 1);
            }
            app.dialogs.preferences = false;
            app.dialogs.shortcuts = true;
            app.dialogs.go_to_layer = Some("3".into());
            frames(&mut app, &ctx, 2);
            app.dialogs.shortcuts = false;
        }
        ctx.set_pixels_per_point(1.0);
        crate::i18n::set_language("en");
        app.session.prefs.language = "en".into();
        frames(&mut app, &ctx, 1);
    }

    #[test]
    fn shortcuts_from_the_table_act() {
        let ctx = egui::Context::default();
        let mut app = app_with_objects();
        let press = |app: &mut CosmicSlicerApp, key: egui::Key, modifiers: egui::Modifiers| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
                events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| app.frame(ui));
            out.textures_delta.clear();
        };
        app.session.page = Page::Prepare;
        frames(&mut app, &ctx, 1);
        // Ctrl+Shift+A: every object on every plate.
        press(&mut app, egui::Key::A, egui::Modifiers { shift: true, ..egui::Modifiers::COMMAND });
        assert_eq!(app.session.selection.len(), app.session.model.objects.len());
        // Page Up turns the selection 45°.
        let id = app.session.model.objects.first().map(|o| o.id).unwrap_or(0);
        app.session.selection = vec![id];
        let before = app.session.model.get(id).map(|o| o.transform.quat());
        press(&mut app, egui::Key::PageUp, egui::Modifiers::NONE);
        let after = app.session.model.get(id).map(|o| o.transform.quat());
        let turned = before.zip(after).map(|(a, b)| a.angle_between(b).to_degrees());
        assert!(turned.is_some_and(|d| (d - 45.0).abs() < 0.5), "{turned:?}");
        // Arrows move 10 mm, Shift+arrows 1 mm.
        let x0 = app.session.model.get(id).map_or(0.0, |o| o.transform.offset.x);
        press(&mut app, egui::Key::ArrowRight, egui::Modifiers::NONE);
        press(&mut app, egui::Key::ArrowRight, egui::Modifiers::SHIFT);
        let x1 = app.session.model.get(id).map_or(0.0, |o| o.transform.offset.x);
        assert!((x1 - x0 - 11.0).abs() < 1e-3, "{x0} {x1}");
        // A tool key, and the painting keys taking over while painting.
        press(&mut app, egui::Key::H, egui::Modifiers::NONE);
        assert_eq!(app.viewport.tool, Some(Tool::FuzzySkin));
        press(&mut app, egui::Key::S, egui::Modifiers::NONE);
        assert_eq!(app.viewport.tools.brush, crate::viewport::tools::Brush::Sphere);
        assert_eq!(app.viewport.tool, Some(Tool::FuzzySkin));
        // Ctrl+1 is the top view; ? opens the list.
        press(&mut app, egui::Key::Questionmark, egui::Modifiers::NONE);
        assert!(app.dialogs.shortcuts);
    }
}

#[cfg(test)]
mod soak {
    //! A long session of random use through the session's own methods, with frames drawn
    //! between: add, select, move, scale, mirror, cut, split, copy, delete, undo and redo,
    //! plates, pages, any setting, arrange and slice. Nothing may panic, and memory must level
    //! off once the undo history is full. Run with `--ignored`; `COSMICSLICER_SOAK_STEPS` (default
    //! 600) and `COSMICSLICER_SOAK_SEED` set its length and seed. A failure names the step, the
    //! seed and the actions before it.

    use super::*;
    use CosmicSlicer_import::fuzz::Rng;
    use glam::Vec3;

    fn frames(app: &mut CosmicSlicerApp, ctx: &egui::Context, n: usize) {
        for _ in 0..n {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| app.frame(ui));
            out.textures_delta.clear();
        }
    }

    fn rss_mb() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb / 1024)
    }

    fn env(name: &str, default: u64) -> u64 {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    /// One random action; returns what it did.
    fn act(app: &mut CosmicSlicerApp, rng: &mut Rng, keys: &[&String]) -> String {
        let s = &mut app.session;
        let ids: Vec<u64> = s.model.objects.iter().map(|o| o.id).collect();
        let any = |rng: &mut Rng| ids.get(rng.below(ids.len())).copied();
        match rng.below(22) {
            0 | 1 => {
                let name = ["Cube", "Cylinder", "Sphere", "Cone", "Disc", "Torus"][rng.below(6)];
                s.add_primitive(name);
                format!("add {name}")
            }
            2 => {
                s.selection = any(rng).into_iter().collect();
                format!("select {:?}", s.selection)
            }
            3 => {
                s.select_all();
                "select all".into()
            }
            4 => {
                let d = Vec3::new(rng.unit() as f32, rng.unit() as f32, rng.unit() as f32) * 150.0;
                let t = s.targets();
                s.nudge(&t, d);
                format!("nudge {d}")
            }
            5 => {
                let f = [0.5f32, 0.8, 1.25, 2.0][rng.below(4)];
                let t = s.targets();
                s.scale_objects(&t, f);
                format!("scale {f}")
            }
            6 => {
                let axis = rng.below(3);
                s.mirror(axis);
                format!("mirror {axis}")
            }
            7 | 8 => {
                let n = 1 + rng.below(5);
                for _ in 0..n {
                    s.undo();
                }
                format!("undo {n}")
            }
            9 => {
                let n = 1 + rng.below(5);
                for _ in 0..n {
                    s.redo();
                }
                format!("redo {n}")
            }
            10 => {
                s.delete_selected();
                "delete".into()
            }
            11 => {
                s.copy();
                s.paste();
                "copy and paste".into()
            }
            12 => {
                s.clone_selected();
                "clone".into()
            }
            13 => {
                s.arrange(rng.chance(3));
                "arrange".into()
            }
            14 => {
                let page = [Page::Home, Page::Prepare, Page::Preview, Page::Device, Page::Project][rng.below(5)];
                s.page = page;
                format!("page {page:?}")
            }
            15 => {
                let Some(key) = keys.get(rng.below(keys.len())) else { return "no keys".into() };
                let def = s.schema.option(key);
                let value = match def.map(|d| d.enum_values.as_slice()) {
                    Some(e) if !e.is_empty() => e.get(rng.below(e.len())).map(|v| v.0.clone()).unwrap_or_default(),
                    _ => ["0", "1", "2", "5", "0.2", "50%", "-1", "1000", ""][rng.below(9)].to_owned(),
                };
                let kind = [TabKind::Process, TabKind::Filament, TabKind::Printer][rng.below(3)];
                s.presets.set_value(s.schema, kind, 0, key, value.clone());
                format!("set {kind:?} {key} = {value:?}")
            }
            16 => match rng.below(3) {
                0 => {
                    s.add_plate();
                    "add plate".into()
                }
                1 => {
                    let p = rng.below(s.model.plates.max(1));
                    s.plate = p;
                    format!("plate {p}")
                }
                _ => {
                    let p = rng.below(s.model.plates.max(1));
                    s.delete_plate(p);
                    format!("delete plate {p}")
                }
            },
            17 => {
                let Some(id) = any(rng) else { return "cut: nothing".into() };
                let z = s.model.get(id).map_or(0.0, |o| {
                    let b = o.bounds();
                    b.min.z + (b.max.z - b.min.z) * (0.2 + 0.6 * rng.unit().abs() as f32)
                });
                let opts = crate::state::CutOptions::default();
                s.cut(id, Vec3::new(0.0, 0.0, z), Vec3::Z, &opts);
                format!("cut {id} at {z}")
            }
            18 => match rng.below(4) {
                0 => {
                    s.split_selected();
                    "split".into()
                }
                1 => {
                    s.drop_to_bed();
                    "drop to bed".into()
                }
                2 => {
                    s.auto_orient();
                    "auto orient".into()
                }
                _ => {
                    s.add_instance();
                    "add instance".into()
                }
            },
            19 => {
                if rng.chance(20) {
                    s.new_project();
                    "new project".into()
                } else {
                    s.selection.clear();
                    "select none".into()
                }
            }
            _ => {
                app.start_slice();
                "slice".into()
            }
        }
    }

    #[test]
    #[ignore = "long: run with --ignored (CI runs it on its own)"]
    fn a_long_session_of_random_use() {
        let steps = env("COSMICSLICER_SOAK_STEPS", 600) as usize;
        let seed = env("COSMICSLICER_SOAK_SEED", 1);
        let ctx = egui::Context::default();
        let mut app = CosmicSlicerApp::headless(LaunchOptions { ephemeral: true, ..Default::default() });
        let schema = app.session.schema;
        let mut keys: Vec<&String> = schema.options.keys().collect();
        keys.sort();
        let mut rng = Rng::new(seed);
        let mut recent: std::collections::VecDeque<String> = Default::default();
        let mut slices = 0;
        let mut warm = None;
        let started = std::time::Instant::now();
        for step in 0..steps {
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let what = act(&mut app, &mut rng, &keys);
                let slicing = what == "slice";
                recent.push_back(what);
                if recent.len() > 20 {
                    recent.pop_front();
                }
                frames(&mut app, &ctx, 1);
                // Let a slice finish (or fail) through the frames, as the app does.
                let mut waited = 0;
                while slicing && app.runner.is_some() && waited < 6000 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    frames(&mut app, &ctx, 1);
                    waited += 1;
                }
                slicing
            }));
            assert!(done.is_ok(), "step {step} (seed {seed}) panicked; the actions before it: {recent:#?}");
            slices += usize::from(done.unwrap_or(false));
            if step == steps / 2 {
                warm = rss_mb();
            }
        }
        let end = rss_mb();
        println!(
            "{steps} steps, {slices} slices in {:.0} s; {} objects; memory {warm:?} MB at half way, {end:?} MB at the end",
            started.elapsed().as_secs_f64(),
            app.session.model.objects.len()
        );
        // The undo history is full by half way: from there memory must level off.
        if let (Some(a), Some(b)) = (warm, end) {
            assert!(b < a + 300, "memory grew from {a} MB to {b} MB");
        }
    }
}
