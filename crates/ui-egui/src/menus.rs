//! OrcaSlicer's menus: the main menu (File, Edit, Window, View, Calibration, Help) and the
//! right-click menus of objects and the empty canvas.

use egui::Ui;

use crate::Action;
use crate::state::{Page, Session};
use crate::ui_kit::{menu_check, menu_item};
use crate::viewport::View;
use CosmicSlicer_config::TabKind;
use CosmicSlicer_model::primitives;

/// Ctrl on Windows and Linux, ⌘ on macOS, as shortcut text.
pub fn ctrl(key: &str) -> String {
    if cfg!(target_os = "macos") { format!("⌘{key}") } else { format!("Ctrl+{key}") }
}

pub fn file_menu(ui: &mut Ui, session: &mut Session, actions: &mut Vec<Action>) {
    if menu_item(ui, "New Project", &ctrl("N"), true) {
        actions.push(Action::NewProject);
    }
    if menu_item(ui, "Open Project", &ctrl("O"), true) {
        actions.push(Action::OpenProject);
    }
    ui.menu_button("Recent files", |ui| {
        let recent = session.prefs.recent.clone();
        if recent.is_empty() {
            ui.add_enabled(false, egui::Button::new("No recent files"));
        }
        for path in recent {
            let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_owned();
            if ui.button(label).on_hover_text(path.display().to_string()).clicked() {
                actions.push(Action::ImportPath(path));
                ui.close();
            }
        }
        ui.separator();
        if ui.add_enabled(!session.prefs.recent.is_empty(), egui::Button::new("Clear recent files")).clicked() {
            session.prefs.recent.clear();
            ui.close();
        }
    });
    if menu_item(ui, "Open G-code…", "", true) {
        actions.push(Action::OpenGcode(None));
    }
    if menu_item(ui, "Save Project", &ctrl("S"), true) {
        actions.push(Action::SaveProject);
    }
    if menu_item(ui, "Save Project as", &ctrl("Shift+S"), true) {
        actions.push(Action::SaveProject);
    }
    ui.separator();
    ui.menu_button("Import", |ui| {
        if menu_item(ui, "Import 3MF/STL/STEP/SVG/OBJ/AMF", &ctrl("I"), true) {
            actions.push(Action::Import);
        }
        menu_item(ui, "Import ZIP Archive", "", false);
        if menu_item(ui, "Import Configs", "", true) {
            actions.push(Action::ImportConfig);
        }
    });
    ui.menu_button("Export", |ui| {
        let any = !session.model.objects.is_empty();
        if menu_item(ui, "Export all objects as one STL", "", any) {
            actions.push(Action::ExportStl);
        }
        menu_item(ui, "Export Generic 3MF", "", false);
        if menu_item(ui, "Export plate sliced file", &ctrl("G"), true) {
            actions.push(Action::ExportGcode);
        }
        if menu_item(ui, "Export G-code", "", true) {
            actions.push(Action::ExportGcode);
        }
        menu_item(ui, "Export toolpaths as OBJ", "", false);
        if menu_item(ui, "Export printer preset", "", true) {
            actions.push(Action::ExportPreset(CosmicSlicer_config::TabKind::Printer, 0));
        }
        if menu_item(ui, "Export filament preset", "", true) {
            actions.push(Action::ExportPreset(CosmicSlicer_config::TabKind::Filament, 0));
        }
        if menu_item(ui, "Export process preset", "", true) {
            actions.push(Action::ExportPreset(CosmicSlicer_config::TabKind::Process, 0));
        }
    });
    ui.separator();
    if menu_item(ui, "Quit", &ctrl("Q"), true) {
        actions.push(Action::Quit);
    }
}

pub fn edit_menu(ui: &mut Ui, session: &mut Session, actions: &mut Vec<Action>) {
    let selected = !session.selection.is_empty();
    if menu_item(ui, "Undo", &ctrl("Z"), session.can_undo()) {
        session.undo();
    }
    if menu_item(ui, "Redo", &ctrl("Y"), session.can_redo()) {
        session.redo();
    }
    ui.separator();
    if menu_item(ui, "Cut", &ctrl("X"), selected) {
        session.copy();
        session.delete_selected();
    }
    if menu_item(ui, "Copy", &ctrl("C"), selected) {
        session.copy();
    }
    if menu_item(ui, "Paste", &ctrl("V"), session.can_paste()) {
        session.paste();
    }
    if menu_item(ui, "Delete Selected", "Del", selected) {
        session.delete_selected();
    }
    if menu_item(ui, "Delete All", &ctrl("Del"), !session.model.objects.is_empty()) {
        session.delete_all();
    }
    ui.separator();
    if menu_item(ui, "Clone Selected", &ctrl("K"), selected) {
        session.clone_selected();
    }
    ui.separator();
    if menu_item(ui, "Select All", &ctrl("A"), !session.model.objects.is_empty()) {
        session.select_all();
    }
    if menu_item(ui, "Deselect All", "Esc", selected) {
        session.selection.clear();
    }
    let _ = actions;
}

pub fn window_menu(ui: &mut Ui, session: &mut Session, actions: &mut Vec<Action>) {
    for (page, label, key) in [
        (Page::Home, "Home", "1"),
        (Page::Prepare, "Prepare", "2"),
        (Page::Preview, "Preview", "3"),
        (Page::Device, "Device", "4"),
        (Page::Project, "Project", "5"),
    ] {
        if menu_check(ui, label, &ctrl(key), session.page == page) {
            session.page = page;
        }
    }
    ui.separator();
    if menu_item(ui, "Printer settings", "", true) {
        actions.push(Action::Settings(TabKind::Printer));
    }
    if menu_item(ui, "Filament settings", "", true) {
        actions.push(Action::FilamentSettings(0));
    }
    if menu_item(ui, "Process settings", "", true) {
        actions.push(Action::Settings(TabKind::Process));
    }
    ui.separator();
    if menu_item(ui, "Setup Wizard", "", true) {
        actions.push(Action::Wizard(false));
    }
    if menu_item(ui, "Compare Presets", "", true) {
        actions.push(Action::ComparePresets);
    }
    if menu_check(ui, "Show multi-device tab", "", session.prefs.multi_device) {
        session.prefs.multi_device = !session.prefs.multi_device;
    }
}

pub fn view_menu(ui: &mut Ui, session: &mut Session, actions: &mut Vec<Action>) {
    for (view, label, key) in View::ALL {
        if menu_item(ui, label, key, true) {
            actions.push(Action::View(view));
        }
    }
    if menu_item(ui, "Zoom to fit", "Z", true) {
        actions.push(Action::ZoomFit);
    }
    ui.separator();
    if menu_check(ui, "Use Perspective View", "K", session.prefs.perspective) {
        session.prefs.perspective = true;
    }
    if menu_check(ui, "Use Orthogonal View", "K", !session.prefs.perspective) {
        session.prefs.perspective = false;
    }
    ui.separator();
    if menu_check(ui, "Show Gridlines", "", session.prefs.show_grid) {
        session.prefs.show_grid = !session.prefs.show_grid;
    }
    if menu_check(ui, "Show Labels", "E", session.prefs.show_labels) {
        session.prefs.show_labels = !session.prefs.show_labels;
    }
    if menu_item(ui, "Collapse sidebar", "Shift+Tab", true) {
        actions.push(Action::ToggleSidebar);
    }
    ui.separator();
    if menu_item(ui, "Preferences", &ctrl("P"), true) {
        actions.push(Action::Preferences);
    }
}

pub fn calibration_menu(ui: &mut Ui, actions: &mut Vec<Action>) {
    for name in ["Temperature", "Max flowrate", "Pressure advance", "Flow ratio", "Retraction", "Cornering"] {
        if menu_item(ui, name, "", true) {
            actions.push(Action::Calibration(name));
        }
    }
    ui.menu_button("Input Shaping", |ui| {
        for name in ["Input Shaping Frequency", "Input Shaping Damping/zeta factor"] {
            if menu_item(ui, name, "", true) {
                actions.push(Action::Calibration(name));
            }
        }
    });
    if menu_item(ui, "VFA", "", true) {
        actions.push(Action::Calibration("VFA"));
    }
    ui.separator();
    if menu_item(ui, "Calibration Guide", "", true) {
        actions.push(Action::OpenUrl("https://github.com/SoftFever/OrcaSlicer/wiki/Calibration"));
    }
}

pub fn help_menu(ui: &mut Ui, actions: &mut Vec<Action>) {
    if menu_item(ui, "Keyboard Shortcuts", "?", true) {
        actions.push(Action::Shortcuts);
    }
    if menu_item(ui, "OrcaSlicer Wiki", "", true) {
        actions.push(Action::OpenUrl("https://github.com/SoftFever/OrcaSlicer/wiki"));
    }
    ui.separator();
    if menu_item(ui, "About CosmicSlicer", "", true) {
        actions.push(Action::About);
    }
}

/// Right-click on an object.
pub fn object_menu(ui: &mut Ui, session: &mut Session, id: u64, actions: &mut Vec<Action>) {
    if menu_item(ui, "Delete", "Del", true) {
        session.delete_selected();
    }
    if menu_item(ui, "Clone", &ctrl("K"), true) {
        session.clone_selected();
    }
    if menu_item(ui, "Rename", "", true) {
        actions.push(Action::Rename(id));
    }
    ui.separator();
    if menu_item(ui, "Drop to bed", "", true) {
        session.drop_to_bed();
    }
    if menu_item(ui, "Auto orient", "", true) {
        session.auto_orient();
    }
    ui.menu_button("Mirror", |ui| {
        for (axis, name) in ["Along X axis", "Along Y axis", "Along Z axis"].iter().enumerate() {
            if menu_item(ui, name, "", true) {
                session.mirror(axis);
            }
        }
    });
    if menu_item(ui, "Split to objects", "", true) {
        session.split_selected();
    }
    ui.menu_button("Change filament", |ui| {
        let n = session.presets.project_filaments.len();
        for f in 1..=n {
            let current = session.model.get(id).is_some_and(|o| o.filament == f);
            if menu_check(ui, &format!("Filament {f}"), "", current) {
                for sel in session.selection.clone() {
                    session.set_filament(sel, f);
                }
            }
        }
    });
    ui.separator();
    ui.menu_button("Add part", |ui| {
        for (name, _) in primitives::ALL {
            menu_item(ui, name, "", false);
        }
    });
    ui.menu_button("Add modifier", |ui| {
        for (name, _) in primitives::ALL {
            menu_item(ui, name, "", false);
        }
    });
    if menu_item(ui, "Export as one STL", "", true) {
        actions.push(Action::ExportStl);
    }
    ui.separator();
    if menu_item(ui, "Edit process settings", "", true) {
        actions.push(Action::Settings(TabKind::Process));
    }
}

/// Right-click on the empty canvas.
pub fn canvas_menu(ui: &mut Ui, session: &mut Session, actions: &mut Vec<Action>) {
    ui.menu_button("Add Primitive", |ui| {
        for (name, _) in primitives::ALL {
            if menu_item(ui, name, "", true) {
                session.add_primitive(name);
            }
        }
    });
    if menu_item(ui, "Import", &ctrl("I"), true) {
        actions.push(Action::Import);
    }
    ui.separator();
    let any = !session.model.objects.is_empty();
    if menu_item(ui, "Arrange", "A", any) {
        session.arrange(false);
    }
    if menu_item(ui, "Select All", &ctrl("A"), any) {
        session.select_all();
    }
    if menu_item(ui, "Delete All", &ctrl("Del"), any) {
        session.delete_all();
    }
    if menu_item(ui, "Paste", &ctrl("V"), session.can_paste()) {
        session.paste();
    }
}

/// A shortcut as written in the shortcut list, with ⌘ for Ctrl on macOS.
pub fn ctrl_text(s: &str) -> String {
    if cfg!(target_os = "macos") { s.replace("Ctrl+", "⌘") } else { s.to_owned() }
}
