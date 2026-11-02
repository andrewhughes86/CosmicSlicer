//! Launch-time automation: open a page, add objects, then optionally save a screenshot
//! and quit. Used for review evidence and smoke tests.

use std::path::{Path, PathBuf};

use egui::ColorImage;

/// Frames to let layout settle before running steps.
const SETTLE_FRAMES: u32 = 4;
/// Frames between the last step and the screenshot.
const CAPTURE_DELAY_FRAMES: u32 = 30;

/// Something automation asks the app to do.
#[derive(Clone, Debug)]
pub enum Step {
    /// Show this page (Home, Prepare, Preview, Device, Multi-device, Project).
    Tab(String),
    /// Add a primitive (Cube, Cylinder, ...).
    Primitive(String),
    /// Add a volume to the last object: "modifier:cube", "part:sphere", "negative:cylinder",
    /// "enforcer:cube", "blocker:cube", or "range" for a height range.
    Volume(String),
    /// Import a model file.
    Open(PathBuf),
    /// Arrange the current plate.
    Arrange,
    /// Select every object.
    SelectAll,
    /// Use the light or dark theme.
    Theme(String),
    /// Open a dialog: preferences, printer, filament, process, about.
    Dialog(String),
    /// Run "Slice plate".
    Slice,
    /// Switch Advanced settings on.
    Advanced,
    /// Activate a canvas tool by name (move, rotate, scale).
    Tool(String),
    /// Show this process settings page by name.
    Page(String),
    /// Slice the plate and write its G-code here.
    Export(PathBuf),
    /// Set a process setting: `key=value`.
    Set(String),
    /// Show the Preview up to this layer.
    Layer(usize),
    /// Open a G-code file in the Preview.
    OpenGcode(PathBuf),
    /// Turn on the Preview's travel, retraction and seam marks.
    PreviewOptions,
    /// Autosave now.
    Autosave,
    /// Abort the process without cleaning up, as a crash would (recovery tests).
    SimulateCrash,
    /// Answer "Restore" to the restore prompt.
    Restore,
    /// Fail inside a frame, to check the app recovers and writes a crash report.
    SimulatePanic,
}

#[derive(Default)]
pub struct Automation {
    pub steps: Vec<Step>,
    pub screenshot: Option<PathBuf>,
    frame: u32,
    ran: bool,
}

impl Automation {
    pub fn new(steps: Vec<Step>, screenshot: Option<PathBuf>) -> Self {
        Self { steps, screenshot, ..Default::default() }
    }

    /// Called once per frame: the steps to run now, and where to capture to.
    pub fn tick(&mut self, ctx: &egui::Context) -> (Vec<Step>, Option<PathBuf>) {
        if self.ran && self.screenshot.is_none() {
            return (Vec::new(), None);
        }
        self.frame = self.frame.saturating_add(1);
        ctx.request_repaint();
        let mut steps = Vec::new();
        if !self.ran && self.frame >= SETTLE_FRAMES {
            self.ran = true;
            steps = std::mem::take(&mut self.steps);
        }
        let capture = if self.frame >= SETTLE_FRAMES + CAPTURE_DELAY_FRAMES { self.screenshot.take() } else { None };
        (steps, capture)
    }
}

/// Writes `image` as an RGBA PNG.
pub fn save_png(image: &ColorImage, path: &Path) -> Result<(), String> {
    let [w, h] = image.size;
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    writer.write_image_data(&bytes).map_err(|e| e.to_string())
}
