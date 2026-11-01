//! Running the engine: a plate's job from the session, sliced on a background thread so the
//! window never stalls, with progress, cancel, and a fingerprint that tells when the result
//! no longer matches the plate.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use glam::{Mat4, Vec3};

use CosmicSlicer_config::TabKind;
use CosmicSlicer_engine::{Job, JobObject, Progress, SliceError, Sliced};

use crate::state::{BED_TYPES, Session};

/// A finished slice and what it was made from.
pub struct SliceState {
    pub plate: usize,
    pub revision: u64,
    pub fingerprint: u64,
    pub result: Arc<Sliced>,
}

impl SliceState {
    /// Whether the result still matches the plate and settings.
    pub fn is_current(&self, session: &Session) -> bool {
        self.plate == session.plate && self.revision == session.revision && self.fingerprint == fingerprint(session)
    }
}

/// Changes when any setting that affects slicing changes.
pub fn fingerprint(session: &Session) -> u64 {
    let p = &session.presets;
    let mut h = DefaultHasher::new();
    format!("{:?}{:?}{:?}", p.printer, p.process, p.project_filaments).hash(&mut h);
    for kind in [TabKind::Printer, TabKind::Process] {
        if let Some(preset) = p.preset(kind, 0) {
            format!("{:?}", preset.config).hash(&mut h);
        }
    }
    for i in 0..p.project_filaments.len() {
        if let Some(preset) = p.preset(TabKind::Filament, i) {
            format!("{:?}", preset.config).hash(&mut h);
        }
    }
    session.bed_type.hash(&mut h);
    session.plate_settings.get(session.plate).map(|s| (s.bed_type, s.sequence, s.spiral)).hash(&mut h);
    h.finish()
}

/// The job for plate `plate`: its visible objects in printer coordinates.
pub fn job(session: &Session, plate: usize) -> Job {
    let p = &session.presets;
    let origin = session.plate_origin(plate);
    let to_printer = Mat4::from_translation(-Vec3::new(origin.x, origin.y, 0.0));
    let filaments = (0..p.project_filaments.len()).map(|i| p.effective(TabKind::Filament, i)).collect();
    let bed = session.plate_settings.get(plate).and_then(|s| s.bed_type).unwrap_or(session.bed_type);
    let bed_key = BED_TYPES.get(bed).map_or("textured_plate_temp", |b| b.1).to_owned();
    let bed_type_name = BED_TYPES.get(bed).map_or("Textured PEI Plate", |b| b.0).to_owned();
    let mut process = p.effective(TabKind::Process, 0);
    if let Some(s) = session.plate_settings.get(plate) {
        match s.spiral {
            1 => process.set("spiral_mode", "1"),
            2 => process.set("spiral_mode", "0"),
            _ => {}
        }
    }
    Job {
        printer: p.effective(TabKind::Printer, 0),
        process,
        filaments,
        bed_key,
        bed_type_name,
        objects: session
            .model
            .objects
            .iter()
            .filter(|o| o.plate == plate && o.visible)
            .map(|o| JobObject {
                name: o.name.clone(),
                mesh: Arc::clone(&o.mesh),
                matrix: to_printer * o.matrix(),
                extruder: o.filament.saturating_sub(1),
                overrides: o.overrides.clone(),
            })
            .collect(),
        plate_name: session.plate_name(plate),
    }
}

/// A G-code file being read for the Preview.
pub struct GcodeLoad {
    pub name: String,
    rx: Receiver<Result<Sliced, String>>,
}

/// Largest G-code file opened.
const MAX_GCODE_BYTES: u64 = 2 << 30;

impl GcodeLoad {
    pub fn start(session: &Session, path: &std::path::Path) -> GcodeLoad {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("G-code").to_owned();
        let printer =
            session.presets.effective(TabKind::Printer, 0).overlaid(&session.presets.effective(TabKind::Process, 0));
        let filaments: Vec<_> = (0..session.presets.project_filaments.len())
            .map(|i| session.presets.effective(TabKind::Filament, i))
            .collect();
        let path = path.to_owned();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("gcode reader".into()).spawn(move || {
            let result = (|| {
                let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                if len > MAX_GCODE_BYTES {
                    return Err(format!("{} is larger than 2 GB", path.display()));
                }
                let bytes = std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
                let text = String::from_utf8_lossy(&bytes).into_owned();
                Ok(CosmicSlicer_engine::view_gcode(text, &printer, &filaments))
            })();
            let _ = tx.send(result);
        });
        if let Err(e) = spawned {
            let (tx, rx2) = mpsc::channel();
            let _ = tx.send(Err(format!("could not start reading: {e}")));
            return GcodeLoad { name, rx: rx2 };
        }
        GcodeLoad { name, rx }
    }

    pub fn poll(&self) -> Option<Result<Sliced, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("reading stopped unexpectedly".into())),
        }
    }
}

/// A slice in progress.
pub struct Runner {
    pub progress: Arc<Progress>,
    rx: Receiver<Result<Sliced, SliceError>>,
    pub plate: usize,
    pub revision: u64,
    pub fingerprint: u64,
}

impl Runner {
    pub fn start(session: &Session) -> Result<Runner, String> {
        let job = job(session, session.plate);
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let worker = Arc::clone(&progress);
        std::thread::Builder::new()
            .name("slicer".into())
            .spawn(move || {
                // `slice` catches its own panics; the send fails only if the window closed.
                let _ = tx.send(CosmicSlicer_engine::slice(&job, &worker));
            })
            .map_err(|e| format!("could not start the slicer: {e}"))?;
        Ok(Runner { progress, rx, plate: session.plate, revision: session.revision, fingerprint: fingerprint(session) })
    }

    /// The result, once it is ready.
    pub fn poll(&self) -> Option<Result<Sliced, SliceError>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(SliceError::Bug("the slicer stopped unexpectedly".into()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview;

    #[test]
    fn a_plate_slices_in_the_background_and_colours_every_way() {
        let mut session = Session::new();
        session.add_primitive("Cube");
        session.add_primitive("Torus");
        session.presets.add_filament();
        if let Some(id) = session.model.objects.last().map(|o| o.id) {
            session.set_filament(id, 2);
        }
        let runner = Runner::start(&session);
        assert!(runner.is_ok());
        let Ok(runner) = runner else { return };
        let started = std::time::Instant::now();
        let result = loop {
            if let Some(r) = runner.poll() {
                break r;
            }
            assert!(started.elapsed().as_secs() < 120, "slicing took too long");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(result.is_ok(), "{:?}", result.as_ref().err());
        let Ok(sliced) = result else { return };
        assert!(sliced.gcode.contains("T1"), "two filaments change tools");
        let n = sliced.preview.segments.len();
        assert!(n > 1000);
        for view in 0..preview::VIEW_TYPES.len() {
            let (colours, range) = preview::colours(&sliced, view, &[false; 17]);
            assert_eq!(colours.len(), n);
            if (1..=8).contains(&view) {
                assert!(range.is_some_and(|r| r.min <= r.max), "view {view}");
            }
        }
        // Hidden features are transparent.
        let mut hidden = [false; 17];
        hidden[CosmicSlicer_engine::Role::OuterWall.index()] = true;
        let (colours, _) = preview::colours(&sliced, 0, &hidden);
        let outer = sliced.preview.segments.iter().position(|s| s.role == CosmicSlicer_engine::Role::OuterWall);
        assert!(outer.is_some_and(|i| colours[i][3] == 0));
        // The slice matches the session until something changes.
        let st = SliceState {
            plate: 0,
            revision: runner.revision,
            fingerprint: runner.fingerprint,
            result: Arc::new(sliced),
        };
        assert!(st.is_current(&session));
        let schema = session.schema;
        session.presets.set_value(schema, TabKind::Process, 0, "wall_loops", "5".into());
        assert!(!st.is_current(&session));
    }

    #[test]
    fn a_plate_job_is_in_printer_coordinates() {
        let mut session = Session::new();
        session.add_plate();
        session.add_primitive("Cube");
        let j = job(&session, 1);
        assert_eq!(j.objects.len(), 1);
        let b = j.objects[0].mesh.bounds_transformed(&j.objects[0].matrix);
        let (lo, size) = session.bed();
        assert!(b.min.x >= lo.x && b.max.x <= lo.x + size.x, "{b:?}");
        assert!(job(&session, 0).objects.is_empty());
    }
}

#[cfg(test)]
mod library_printers {
    use super::*;
    use crate::state::Prefs;

    fn slice_on(printer: &str) -> Result<CosmicSlicer_engine::Sliced, String> {
        let prefs = Prefs { installed_printers: vec![printer.to_owned()], ..Prefs::default() };
        let mut session = Session::new();
        session.presets = crate::profiles::build(&prefs);
        if session.presets.current_printer().is_none_or(|p| p.name != printer) {
            return Err(format!("{printer} is not in the library"));
        }
        session.add_primitive("Cube");
        let runner = Runner::start(&session)?;
        let started = std::time::Instant::now();
        loop {
            if let Some(r) = runner.poll() {
                return r.map_err(|e| e.to_string());
            }
            if started.elapsed().as_secs() > 120 {
                return Err("timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn real_printers_slice_with_their_own_gcode() {
        for (printer, expect) in [
            ("Bambu Lab X1 Carbon 0.4 nozzle", "M1002"),
            ("Prusa MK4S 0.4 nozzle", "M555"),
            ("Creality K1C 0.4 nozzle", "START_PRINT"),
            ("Voron 2.4 350 0.4 nozzle", "PRINT_START"),
            ("MyKlipper 0.4 nozzle", "PRINT_START"),
        ] {
            let out = slice_on(printer);
            assert!(out.is_ok(), "{printer}: {:?}", out.as_ref().err());
            let Ok(out) = out else { continue };
            let start: String =
                out.gcode.lines().take_while(|l| !l.starts_with(";LAYER_CHANGE")).collect::<Vec<_>>().join("\n");
            assert!(
                start.contains(expect),
                "{printer}: start G-code lacks {expect}:\n{}",
                &start[..start.len().min(1500)]
            );
            let leftover =
                out.gcode.lines().map(|l| l.split(';').next().unwrap_or("")).find(|c| c.contains(['{', '}']));
            assert!(leftover.is_none(), "{printer}: template text in G-code: {leftover:?}");
            assert!(out.gcode.contains(";TYPE:Outer wall"), "{printer}");
        }
    }
}
