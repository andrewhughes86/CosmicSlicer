//! Keyboard shortcuts: OrcaSlicer's (its `Shortcuts.cpp`) for what CosmicSlicer has, plus a
//! few of its own; the handler and the Keyboard Shortcuts window both read this table.

use egui::{Key, Modifiers};

/// Where a shortcut works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Anywhere, even while typing (they all hold Ctrl).
    Global,
    /// Anywhere but in a text box.
    App,
    /// On the Prepare and Preview canvases.
    Canvas,
    Prepare,
    Preview,
    /// While a painting tool is open (replacing the tool keys).
    Painting,
}

#[derive(Clone, Copy, Debug)]
pub struct Shortcut {
    /// What the handler does.
    pub id: &'static str,
    /// OrcaSlicer's description.
    pub label: &'static str,
    pub section: &'static str,
    pub mods: Modifiers,
    pub key: Key,
    pub scope: Scope,
}

const N: Modifiers = Modifiers::NONE;
const C: Modifiers = Modifiers::COMMAND;
const S: Modifiers = Modifiers::SHIFT;
const CS: Modifiers = Modifiers { shift: true, ..Modifiers::COMMAND };

const fn sc(
    id: &'static str,
    label: &'static str,
    section: &'static str,
    mods: Modifiers,
    key: Key,
    scope: Scope,
) -> Shortcut {
    Shortcut { id, label, section, mods, key, scope }
}

pub const ALL: &[Shortcut] = &[
    sc("new", "New Project", "Project", C, Key::N, Scope::Global),
    sc("open", "Open Project", "Project", C, Key::O, Scope::Global),
    sc("save", "Save Project", "Project", C, Key::S, Scope::Global),
    sc("save-as", "Save Project as", "Project", CS, Key::S, Scope::Global),
    sc("import", "Import geometry data from STL/STEP/3MF/OBJ/AMF files", "Project", C, Key::I, Scope::Global),
    sc("slice", "Slice plate", "Slicing and printing", C, Key::R, Scope::Global),
    sc("export", "Export plate sliced file", "Slicing and printing", C, Key::G, Scope::Global),
    sc("print", "Print plate", "Slicing and printing", CS, Key::G, Scope::Global),
    sc("select-all", "Select all objects", "Selection", C, Key::A, Scope::App),
    sc("select-all-plates", "Select all objects on all plates", "Selection", CS, Key::A, Scope::App),
    sc("undo", "Undo", "Editing", C, Key::Z, Scope::App),
    sc("redo", "Redo", "Editing", C, Key::Y, Scope::App),
    sc("redo", "Redo", "Editing", CS, Key::Z, Scope::App),
    sc("cut", "Cut", "Editing", C, Key::X, Scope::App),
    sc("copy", "Copy to clipboard", "Editing", C, Key::C, Scope::App),
    sc("paste", "Paste from clipboard", "Editing", C, Key::V, Scope::App),
    sc("delete", "Delete Selected", "Editing", N, Key::Delete, Scope::App),
    sc("delete", "Delete Selected", "Editing", N, Key::Backspace, Scope::App),
    sc("delete-all", "Delete All", "Editing", C, Key::D, Scope::App),
    sc("delete-all", "Delete All", "Editing", C, Key::Delete, Scope::App),
    sc("clone", "Clone Selected", "Editing", C, Key::K, Scope::App),
    sc("add-instance", "Add instance", "Objects", N, Key::Plus, Scope::Prepare),
    sc("add-instance", "Add instance", "Objects", N, Key::Equals, Scope::Prepare),
    sc("remove-instance", "Remove instance", "Objects", N, Key::Minus, Scope::Prepare),
    sc("toggle-printable", "Toggle printable for object/part", "Objects", N, Key::V, Scope::Prepare),
    sc("arrange", "Arrange all objects", "Placement", N, Key::A, Scope::Prepare),
    sc("arrange-plate", "Arrange objects on selected plates", "Placement", S, Key::A, Scope::Prepare),
    sc("orient", "Auto orient all/selected objects", "Placement", N, Key::Q, Scope::Prepare),
    sc("orient-plate", "Auto orient all objects on current plate", "Placement", S, Key::Q, Scope::Prepare),
    sc("nudge-left", "Move selection 10mm in negative X direction", "Placement", N, Key::ArrowLeft, Scope::Prepare),
    sc("nudge-right", "Move selection 10mm in positive X direction", "Placement", N, Key::ArrowRight, Scope::Prepare),
    sc("nudge-up", "Move selection 10mm in positive Y direction", "Placement", N, Key::ArrowUp, Scope::Prepare),
    sc("nudge-down", "Move selection 10mm in negative Y direction", "Placement", N, Key::ArrowDown, Scope::Prepare),
    sc("nudge-left-fine", "Move selection 1 mm (with Shift)", "Placement", S, Key::ArrowLeft, Scope::Prepare),
    sc("nudge-right-fine", "Move selection 1 mm (with Shift)", "Placement", S, Key::ArrowRight, Scope::Prepare),
    sc("nudge-up-fine", "Move selection 1 mm (with Shift)", "Placement", S, Key::ArrowUp, Scope::Prepare),
    sc("nudge-down-fine", "Move selection 1 mm (with Shift)", "Placement", S, Key::ArrowDown, Scope::Prepare),
    sc("rotate-left", "Rotate selection 45 degrees counterclockwise", "Placement", N, Key::PageUp, Scope::Prepare),
    sc("rotate-right", "Rotate selection 45 degrees clockwise", "Placement", N, Key::PageDown, Scope::Prepare),
    sc("tool-move", "Gizmo move", "Gizmos", N, Key::M, Scope::Prepare),
    sc("tool-rotate", "Gizmo rotate", "Gizmos", N, Key::R, Scope::Prepare),
    sc("tool-scale", "Gizmo scale", "Gizmos", N, Key::S, Scope::Prepare),
    sc("tool-lay on face", "Gizmo place face on bed", "Gizmos", N, Key::F, Scope::Prepare),
    sc("tool-cut", "Gizmo cut", "Gizmos", N, Key::C, Scope::Prepare),
    sc("tool-mesh boolean", "Gizmo mesh boolean", "Gizmos", N, Key::B, Scope::Prepare),
    sc("tool-support painting", "Gizmo FDM paint-on supports", "Gizmos", N, Key::L, Scope::Prepare),
    sc("tool-seam painting", "Gizmo FDM paint-on seam", "Gizmos", N, Key::P, Scope::Prepare),
    sc("tool-fuzzy skin painting", "Gizmo FDM paint-on fuzzy skin", "Gizmos", N, Key::H, Scope::Prepare),
    sc("tool-color painting", "Gizmo multi-material painting", "Gizmos", N, Key::N, Scope::Prepare),
    sc("tool-text shape", "Gizmo text emboss/engrave", "Gizmos", N, Key::T, Scope::Prepare),
    sc("tool-measure", "Gizmo measure", "Gizmos", N, Key::U, Scope::Prepare),
    sc("tool-assemble", "Gizmo assemble", "Gizmos", N, Key::Y, Scope::Prepare),
    sc("tool-brim ears", "Gizmo brim ears", "Gizmos", N, Key::E, Scope::Prepare),
    sc("go-to-layer", "Jump to layer", "Sliders", S, Key::G, Scope::Preview),
    sc("layer-up", "Vertical slider - Move active thumb Up", "Sliders", N, Key::ArrowUp, Scope::Preview),
    sc("layer-down", "Vertical slider - Move active thumb Down", "Sliders", N, Key::ArrowDown, Scope::Preview),
    sc("moves-left", "Horizontal slider - Move active thumb Left", "Sliders", N, Key::ArrowLeft, Scope::Preview),
    sc("moves-right", "Horizontal slider - Move active thumb Right", "Sliders", N, Key::ArrowRight, Scope::Preview),
    sc("moves-start", "Horizontal slider - Move to start position", "Sliders", N, Key::Home, Scope::Preview),
    sc("moves-end", "Horizontal slider - Move to last position", "Sliders", N, Key::End, Scope::Preview),
    sc("brush-circle", "Circle", "Painting tools", N, Key::C, Scope::Painting),
    sc("brush-sphere", "Sphere", "Painting tools", N, Key::S, Scope::Painting),
    sc("brush-fill", "Fill", "Painting tools", N, Key::F, Scope::Painting),
    sc("view0", "Camera view - Default", "Camera", C, Key::Num0, Scope::Global),
    sc("view1", "Camera view - Top", "Camera", C, Key::Num1, Scope::Global),
    sc("view2", "Camera view - Bottom", "Camera", C, Key::Num2, Scope::Global),
    sc("view3", "Camera view - Front", "Camera", C, Key::Num3, Scope::Global),
    sc("view4", "Camera view - Behind", "Camera", C, Key::Num4, Scope::Global),
    sc("view5", "Camera Angle - Left side", "Camera", C, Key::Num5, Scope::Global),
    sc("view6", "Camera Angle - Right side", "Camera", C, Key::Num6, Scope::Global),
    sc("fit", "Camera view - Current plate", "Camera", C, Key::Num7, Scope::Global),
    sc("fit", "Zoom to fit", "Camera", N, Key::Z, Scope::Canvas),
    sc("zoom-in", "Zoom in", "Camera", N, Key::I, Scope::Canvas),
    sc("zoom-out", "Zoom out", "Camera", N, Key::O, Scope::Canvas),
    sc("projection", "Perspective / orthographic view", "Camera", N, Key::K, Scope::Canvas),
    sc("labels", "Show object labels in 3D scene.", "Display", C, Key::E, Scope::Global),
    sc("prefs", "Preferences", "Application", C, Key::P, Scope::Global),
    sc("search", "Search", "Application", C, Key::F, Scope::Global),
    sc("tab", "Switch between Prepare/Preview", "Application", N, Key::Tab, Scope::Canvas),
    sc("sidebar", "Collapse/Expand the sidebar", "Application", S, Key::Tab, Scope::Canvas),
    sc("reload-device", "Reload the device page", "Application", N, Key::F5, Scope::App),
    sc("help", "Show keyboard shortcuts list", "Application", N, Key::Questionmark, Scope::App),
    sc("help", "Show keyboard shortcuts list", "Application", S, Key::Slash, Scope::App),
    sc("escape", "Deselect all / close the tool", "Application", N, Key::Escape, Scope::App),
    sc("quit", "Quit", "Application", C, Key::Q, Scope::Global),
];

/// "Ctrl+Shift+S", as the platform names the keys.
pub fn chord(s: &Shortcut) -> String {
    let mut out = String::new();
    if s.mods.command || s.mods.ctrl {
        out.push_str(if cfg!(target_os = "macos") { "⌘+" } else { "Ctrl+" });
    }
    if s.mods.alt {
        out.push_str("Alt+");
    }
    if s.mods.shift {
        out.push_str("Shift+");
    }
    out.push_str(match s.key {
        Key::Plus => "+",
        Key::Minus => "-",
        Key::Equals => "=",
        Key::Questionmark => "?",
        Key::Slash => "/",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        k => k.name(),
    });
    out
}

/// The table in the order the handler tries it: more modifiers first, so Ctrl+Shift+S is not
/// taken for Ctrl+S.
pub fn by_priority() -> Vec<&'static Shortcut> {
    let mut v: Vec<&Shortcut> = ALL.iter().collect();
    let count = |m: Modifiers| u8::from(m.command || m.ctrl) + u8::from(m.shift) + u8::from(m.alt);
    v.sort_by_key(|s| std::cmp::Reverse(count(s.mods)));
    v
}

/// (section, rows of (keys, description)) for the Keyboard Shortcuts window; a description
/// with several chords lists them together.
pub fn sections() -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let mut out: Vec<(&'static str, Vec<(String, &'static str)>)> = Vec::new();
    for s in ALL {
        let idx = match out.iter().position(|(name, _)| *name == s.section) {
            Some(i) => i,
            None => {
                out.push((s.section, Vec::new()));
                out.len() - 1
            }
        };
        let Some((_, rows)) = out.get_mut(idx) else { continue };
        match rows.iter_mut().find(|(_, l)| *l == s.label) {
            Some((keys, _)) => {
                keys.push_str(" / ");
                keys.push_str(&chord(s));
            }
            None => rows.push((chord(s), s.label)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_shortcuts_clash() {
        // The same chord twice in one place would fire both.
        let overlaps = |a: Scope, b: Scope| {
            use Scope::*;
            a == b
                || matches!((a, b), (Global, _) | (_, Global) | (App, _) | (_, App))
                || matches!((a, b), (Canvas, Prepare | Preview) | (Prepare | Preview, Canvas))
        };
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                // Painting keys replace the tool keys while painting; that is deliberate.
                let painting = (a.scope == Scope::Painting) != (b.scope == Scope::Painting);
                if a.key == b.key && a.mods == b.mods && overlaps(a.scope, b.scope) && !painting {
                    assert_eq!(a.id, b.id, "{} clashes with {}", a.label, b.label);
                }
            }
        }
        assert_eq!(chord(&ALL[3]), if cfg!(target_os = "macos") { "⌘+Shift+S" } else { "Ctrl+Shift+S" });
        assert!(sections().iter().any(|(s, rows)| *s == "Camera" && rows.len() >= 8));
    }
}
