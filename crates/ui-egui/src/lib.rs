//! CosmicSlicer's user interface: a Rust rebuild of OrcaSlicer's, in egui and wgpu.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
#![allow(non_snake_case)]

mod app;
pub mod automation;
mod devices;
mod dialogs;
mod emboss;
pub mod icons;
mod menus;
mod pages;
mod preview;
mod profiles;
mod project;
mod recovery;
mod settings;
mod sidebar;
mod slicer;
pub mod state;
mod store;
pub mod theme;
mod topbar;
pub mod ui_kit;
mod viewport;
mod wizard;

use std::path::PathBuf;

use CosmicSlicer_config::TabKind;

pub use app::{CosmicSlicerApp, LaunchOptions};
pub use automation::{Automation, Step};

pub const APP_ID: &str = "CosmicSlicer";

pub const APP_ICON_SVG: &str = include_str!("../../../assets/icons/app/cosmicslicer.svg");

/// What the panes ask the app to do; things that need a file dialog, a window or the camera.
#[derive(Clone, Debug)]
pub enum Action {
    NewProject,
    OpenProject,
    /// Opens a project file, or imports it when it is only a model.
    OpenProjectPath(PathBuf),
    SaveProject,
    SaveProjectAs,
    Import,
    ImportPath(PathBuf),
    /// Pick a model file to add to an object as a volume of the kind given.
    AddVolumeFile(u64, CosmicSlicer_model::PartKind),
    /// Pick an SVG file to emboss, as the Text shape tool is set.
    EmbossSvg,
    /// Pick a font file for the Text shape tool.
    PickFont,
    ExportStl,
    ExportGcode,
    /// Slice every plate and write each one's G-code into a folder.
    ExportAllGcode,
    Quit,
    Slice,
    SliceAll,
    Print,
    Settings(TabKind),
    FilamentSettings(usize),
    SelectPreset(TabKind, usize, usize),
    SavePreset(TabKind, usize),
    ComparePresets,
    PlateSettings(usize),
    PhysicalPrinter,
    FlushingVolumes,
    Preferences,
    About,
    Shortcuts,
    Calibration(&'static str),
    OpenUrl(&'static str),
    View(viewport::View),
    ZoomFit,
    ToggleSidebar,
    Rename(u64),
    /// Open the setup wizard (on its filament page when true).
    Wizard(bool),
    ImportConfig,
    /// Open a G-code file in the Preview (asks for it when `None`).
    OpenGcode(Option<PathBuf>),
    ExportPreset(TabKind, usize),
    /// A feature not built yet, by name.
    Unavailable(&'static str),
}

pub fn app_icon(px: u32) -> Option<egui::IconData> {
    use resvg::{tiny_skia, usvg};
    let px = px.clamp(16, 1024);
    std::panic::catch_unwind(|| {
        let tree = usvg::Tree::from_str(APP_ICON_SVG, &usvg::Options::default()).ok()?;
        let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
        let scale = px as f32 / 256.0;
        resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        // The window system wants straight (not premultiplied) alpha.
        let rgba = pixmap.pixels().iter().flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        });
        Some(egui::IconData { rgba: rgba.collect(), width: px, height: px })
    })
    .ok()
    .flatten()
}

pub fn native_options() -> eframe::NativeOptions {
    let viewport = egui::ViewportBuilder::default();
    let viewport = match app_icon(256) {
        Some(icon) => viewport.with_icon(std::sync::Arc::new(icon)),
        None => viewport,
    };
    eframe::NativeOptions {
        viewport: viewport
            .with_title("CosmicSlicer")
            // The top bar is the title bar, with its own window controls, as in OrcaSlicer.
            .with_decorations(false)
            .with_app_id(APP_ID)
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1024.0, 640.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        // The canvas renders offscreen with its own depth and MSAA targets.
        depth_buffer: 0,
        multisampling: 0,
        centered: true,
        ..Default::default()
    }
}

pub fn run(options: LaunchOptions) -> eframe::Result {
    eframe::run_native(APP_ID, native_options(), Box::new(move |cc| Ok(Box::new(CosmicSlicerApp::new(cc, options)))))
}

#[cfg(test)]
mod app_icon_tests {
    #[test]
    fn the_app_icon_renders() {
        let icon = super::app_icon(64);
        assert!(icon.as_ref().is_some_and(|i| i.width == 64 && i.rgba.len() == 64 * 64 * 4));
        let px = |x: usize, y: usize| icon.as_ref().map_or(0, |i| i.rgba[(y * 64 + x) * 4 + 3]);
        assert_eq!(px(32, 32), 255);
        assert_eq!(px(0, 0), 0);
    }
}
