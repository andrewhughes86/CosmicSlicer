//! Autosave and crash recovery.
//!
//! While CosmicSlicer runs it holds an OS lock on `autosave/session-<id>.lock` and saves the
//! project to `autosave/session-<id>.cosmicsave` a minute after each change. A clean exit
//! deletes both. If the program dies instead (a crash, a GPU driver fault, a power cut), the OS
//! drops the lock but the files stay; the next start finds a lock it can take, which means its
//! owner is gone, and offers to restore that project. A lock it cannot take belongs to another
//! running CosmicSlicer and is left alone.
//!
//! Panics also leave a crash report in `crashes/`.
//!
//! The snapshot is CosmicSlicer's own format, until 3MF projects arrive: a header, a JSON
//! manifest, then the meshes as raw little-endian numbers. Reading it trusts nothing.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use glam::Vec3;
use serde::{Deserialize, Serialize};

use CosmicSlicer_config::PresetBundle;
use CosmicSlicer_model::{HeightRange, Mesh, Model, Object, PartKind, Transform, Volume};

use crate::state::{PlateSettings, ProjectInfo, Session};
use crate::store;

const MAGIC: &[u8; 8] = b"CSAVE01\n";
/// Seconds after a change before it is saved.
pub const AUTOSAVE_DELAY: f64 = 60.0;
/// Most crash reports kept.
const MAX_REPORTS: usize = 20;
/// Caps on what a snapshot may claim, so a damaged file cannot exhaust memory.
const MAX_MANIFEST: u64 = 256 << 20;
const MAX_MESH_ITEMS: u64 = 100_000_000;

#[derive(Serialize, Deserialize)]
struct ObjectEntry {
    name: String,
    mesh: usize,
    transform: Transform,
    filament: usize,
    plate: usize,
    visible: bool,
    overrides: Vec<(String, String)>,
    #[serde(default)]
    paint: Option<CosmicSlicer_model::Paint>,
    #[serde(default)]
    parts: Vec<PartEntry>,
    #[serde(default)]
    ranges: Vec<HeightRange>,
    #[serde(default)]
    brim_ears: Vec<CosmicSlicer_model::BrimEar>,
}

#[derive(Serialize, Deserialize)]
struct PartEntry {
    name: String,
    kind: PartKind,
    mesh: usize,
    transform: Transform,
    overrides: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    saved_at: u64,
    project: ProjectInfo,
    /// (vertices, triangles) per mesh, in the order they follow the manifest.
    meshes: Vec<(u64, u64)>,
    objects: Vec<ObjectEntry>,
    plates: usize,
    locked: Vec<bool>,
    plate_settings: Vec<PlateSettings>,
    bed_type: usize,
    plate: usize,
    presets: PresetBundle,
}

/// A project read back from a snapshot.
pub struct Restored {
    pub project: ProjectInfo,
    pub model: Model,
    pub locked: Vec<bool>,
    pub plate_settings: Vec<PlateSettings>,
    pub bed_type: usize,
    pub plate: usize,
    pub presets: PresetBundle,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where `m` is in the list of meshes to write, adding it the first time.
fn mesh_index<'a>(m: &'a Arc<Mesh>, index: &mut HashMap<usize, usize>, meshes: &mut Vec<&'a Mesh>) -> usize {
    *index.entry(Arc::as_ptr(m) as usize).or_insert_with(|| {
        meshes.push(m.as_ref());
        meshes.len() - 1
    })
}

/// The session as snapshot bytes. Meshes shared by copies are written once.
pub fn encode(session: &Session) -> Vec<u8> {
    let mut index: HashMap<usize, usize> = HashMap::new();
    let mut meshes: Vec<&Mesh> = Vec::new();
    let mut objects = Vec::new();
    for o in &session.model.objects {
        let mesh = mesh_index(&o.mesh, &mut index, &mut meshes);
        let parts = o
            .parts
            .iter()
            .map(|p| PartEntry {
                name: p.name.clone(),
                kind: p.kind,
                mesh: mesh_index(&p.mesh, &mut index, &mut meshes),
                transform: p.transform,
                overrides: p.overrides.clone(),
            })
            .collect();
        objects.push(ObjectEntry {
            name: o.name.clone(),
            mesh,
            transform: o.transform,
            filament: o.filament,
            plate: o.plate,
            visible: o.visible,
            overrides: o.overrides.clone(),
            paint: o.paint.as_deref().cloned(),
            parts,
            ranges: o.ranges.clone(),
            brim_ears: o.brim_ears.clone(),
        });
    }
    let manifest = Manifest {
        saved_at: now_secs(),
        project: session.project.clone(),
        meshes: meshes.iter().map(|m| (m.vertices.len() as u64, m.triangles.len() as u64)).collect(),
        objects,
        plates: session.model.plates,
        locked: session.locked.clone(),
        plate_settings: session.plate_settings.clone(),
        bed_type: session.bed_type,
        plate: session.plate,
        presets: session.presets.clone(),
    };
    let json = serde_json::to_vec(&manifest).unwrap_or_default();
    let mesh_bytes: usize = meshes.iter().map(|m| m.vertices.len() * 12 + m.triangles.len() * 12).sum();
    let mut out = Vec::with_capacity(MAGIC.len() + 8 + json.len() + mesh_bytes);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(json.len() as u64).to_le_bytes());
    out.extend_from_slice(&json);
    for m in meshes {
        for v in &m.vertices {
            for x in v.to_array() {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
        for t in &m.triangles {
            for i in t {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
    }
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("the snapshot is damaged")?;
        let s = self.bytes.get(self.at..end).ok_or("the snapshot is cut short")?;
        self.at = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
}

/// Reads snapshot bytes, checking every count and index.
pub fn decode(bytes: &[u8]) -> Result<Restored, String> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        return Err("not a CosmicSlicer autosave".into());
    }
    let len = r.u64()?;
    if len > MAX_MANIFEST {
        return Err("the snapshot is damaged".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(r.take(len as usize)?).map_err(|e| format!("the snapshot is damaged: {e}"))?;
    let mut meshes: Vec<Arc<Mesh>> = Vec::with_capacity(manifest.meshes.len());
    for &(nv, nt) in &manifest.meshes {
        if nv > MAX_MESH_ITEMS || nt > MAX_MESH_ITEMS {
            return Err("the snapshot is damaged".into());
        }
        let mut mesh = Mesh::default();
        // Each claimed item must be backed by bytes before anything is allocated for it.
        let needed = (nv + nt).saturating_mul(12);
        if needed > bytes.len().saturating_sub(r.at) as u64 {
            return Err("the snapshot is cut short".into());
        }
        mesh.vertices.reserve(nv as usize);
        for _ in 0..nv {
            let b = r.take(12)?;
            let f = |i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
            mesh.vertices.push(Vec3::new(f(0), f(4), f(8)));
        }
        mesh.triangles.reserve(nt as usize);
        for _ in 0..nt {
            mesh.triangles.push([r.u32()?, r.u32()?, r.u32()?]);
        }
        mesh.sanitise();
        meshes.push(Arc::new(mesh));
    }
    let plates = manifest.plates.clamp(1, 100);
    let mut model = Model::new();
    model.plates = plates;
    for o in manifest.objects {
        let Some(mesh) = meshes.get(o.mesh) else { continue };
        let t = o.transform;
        if !(t.offset.is_finite() && t.rotation.is_finite() && t.scale.is_finite()) {
            continue;
        }
        model.insert(Object {
            id: 0,
            name: o.name,
            mesh: Arc::clone(mesh),
            transform: t,
            filament: o.filament.max(1),
            plate: o.plate.min(plates - 1),
            visible: o.visible,
            overrides: o.overrides,
            paint: o.paint.map(Arc::new),
            parts: o
                .parts
                .into_iter()
                .filter(|p| {
                    p.transform.offset.is_finite() && p.transform.rotation.is_finite() && p.transform.scale.is_finite()
                })
                .filter_map(|p| {
                    Some(Volume {
                        name: p.name,
                        kind: p.kind,
                        mesh: Arc::clone(meshes.get(p.mesh)?),
                        transform: p.transform,
                        overrides: p.overrides,
                    })
                })
                .collect(),
            ranges: o.ranges.into_iter().filter(|r| r.lo.is_finite() && r.hi.is_finite()).collect(),
            brim_ears: o.brim_ears.into_iter().filter(|e| e.at.is_finite() && e.radius.is_finite()).collect(),
        });
    }
    let mut presets = manifest.presets;
    // Selections must point at presets that exist.
    let clamp = |slot: &mut CosmicSlicer_config::Slot, len: usize| slot.preset = slot.preset.min(len.saturating_sub(1));
    clamp(&mut presets.printer, presets.printers.len());
    clamp(&mut presets.process, presets.processes.len());
    let n = presets.filaments.len();
    presets.project_filaments.iter_mut().for_each(|s| clamp(s, n));
    presets.project_filaments.truncate(CosmicSlicer_config::presets::MAX_FILAMENTS);
    if presets.project_filaments.is_empty() || presets.printers.is_empty() || presets.processes.is_empty() || n == 0 {
        presets = PresetBundle::builtin();
    }
    Ok(Restored {
        project: manifest.project,
        model,
        locked: manifest.locked,
        plate_settings: manifest.plate_settings,
        bed_type: manifest.bed_type.min(crate::state::BED_TYPES.len() - 1),
        plate: manifest.plate.min(plates - 1),
        presets,
    })
}

/// Puts a restored project into the session (one undo step).
pub fn apply(session: &mut Session, r: Restored) {
    session.checkpoint();
    session.model = r.model;
    session.locked = r.locked;
    session.locked.resize(session.model.plates, false);
    session.plate_settings = r.plate_settings;
    session.bed_type = r.bed_type;
    session.plate = r.plate;
    session.presets = r.presets;
    session.project = r.project;
    session.selection.clear();
    session.clamp_filaments();
    session.dirty = true;
}

/// A project left behind by a session that did not exit cleanly.
#[derive(Clone, Debug)]
pub struct Orphan {
    pub snapshot: PathBuf,
    pub lock: PathBuf,
    pub name: String,
    pub objects: usize,
    /// Seconds since it was saved.
    pub age: u64,
    /// A crash report written after it, if any.
    pub report: Option<PathBuf>,
}

impl Orphan {
    pub fn read(&self) -> Result<Restored, String> {
        let bytes = std::fs::read(&self.snapshot).map_err(|e| format!("could not read the autosave: {e}"))?;
        decode(&bytes)
    }

    pub fn discard(&self) {
        let _ = std::fs::remove_file(&self.snapshot);
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// This session's autosave: its lock, held while the program runs, and its snapshot file.
pub struct Autosaver {
    lock_path: PathBuf,
    snapshot: PathBuf,
    /// Held to tell other instances this session is alive; the OS releases it if we die.
    _lock: File,
    /// What was last saved: (revision, settings fingerprint, project name).
    saved: Option<(u64, u64, String)>,
    /// When the current unsaved change was first seen.
    changed_at: Option<f64>,
}

fn session_id() -> String {
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{}-{n}", now_secs(), std::process::id())
}

impl Autosaver {
    /// Takes this session's lock in `dir/autosave` and returns it with any orphaned project,
    /// newest first.
    pub fn start(dir: &Path) -> Result<(Autosaver, Vec<Orphan>), String> {
        let folder = dir.join("autosave");
        std::fs::create_dir_all(&folder).map_err(|e| format!("could not create {}: {e}", folder.display()))?;
        let orphans = find_orphans(dir);
        let id = session_id();
        let lock_path = folder.join(format!("session-{id}.lock"));
        let lock = File::create(&lock_path).map_err(|e| format!("could not create {}: {e}", lock_path.display()))?;
        lock.try_lock().map_err(|e| format!("could not lock {}: {e}", lock_path.display()))?;
        let snapshot = folder.join(format!("session-{id}.cosmicsave"));
        Ok((Autosaver { lock_path, snapshot, _lock: lock, saved: None, changed_at: None }, orphans))
    }

    /// Called every frame; saves a minute after the last change, and returns an error to show.
    pub fn tick(&mut self, session: &Session, now: f64) -> Option<String> {
        let state = (session.revision, crate::slicer::fingerprint(session), session.project.name.clone());
        if self.saved.as_ref() == Some(&state) {
            self.changed_at = None;
            return None;
        }
        // An empty, untouched project is not worth saving.
        if self.saved.is_none() && session.model.objects.is_empty() && !session.dirty {
            return None;
        }
        let since = *self.changed_at.get_or_insert(now);
        if now - since < AUTOSAVE_DELAY {
            return None;
        }
        self.save(session).err()
    }

    /// Saves now.
    pub fn save(&mut self, session: &Session) -> Result<(), String> {
        let state = (session.revision, crate::slicer::fingerprint(session), session.project.name.clone());
        let result = store::write_atomic(&self.snapshot, &encode(session))
            .map_err(|e| format!("Autosave failed: could not write {}: {e}", self.snapshot.display()));
        // Do not retry every frame after a failure; try again after the delay.
        self.saved = Some(state);
        self.changed_at = None;
        result
    }

    /// A clean exit: nothing to recover.
    pub fn finish(self) {
        let _ = std::fs::remove_file(&self.snapshot);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Snapshots whose lock is free: their session ended without cleaning up.
fn find_orphans(dir: &Path) -> Vec<Orphan> {
    let folder = dir.join("autosave");
    let Ok(entries) = std::fs::read_dir(&folder) else { return Vec::new() };
    let mut out = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "lock") {
            continue;
        }
        let Ok(file) = File::options().read(true).write(true).open(&path) else { continue };
        // Locked: another CosmicSlicer is running and owns it.
        if file.try_lock().is_err() {
            continue;
        }
        drop(file);
        let snapshot = path.with_extension("cosmicsave");
        let Ok(meta) = std::fs::metadata(&snapshot) else {
            // Nothing was saved: just a stale lock.
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let saved = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        let (name, objects) = std::fs::read(&snapshot)
            .ok()
            .and_then(|b| decode(&b).ok())
            .map_or(("(unreadable)".to_owned(), 0), |r| (r.project.name, r.model.objects.len()));
        out.push(Orphan {
            snapshot,
            lock: path,
            name,
            objects,
            age: now_secs().saturating_sub(saved),
            report: newest_report(dir, saved),
        });
    }
    out.sort_by_key(|o| o.age);
    out
}

fn newest_report(dir: &Path, after: u64) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir.join("crashes")).ok()?;
    entries
        .flatten()
        .filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            (t + 5 >= after).then_some((t, e.path()))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Writes a report for every panic to `dir/crashes`, then runs the default hook. The hook
/// itself never panics: every step that can fail is allowed to.
pub fn install_crash_reports(dir: &Path) {
    let folder = dir.join("crashes");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown".into());
        let location =
            info.location().map_or_else(String::new, |l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let thread = std::thread::current().name().unwrap_or("unnamed").to_owned();
        let report = format!(
            "CosmicSlicer {} crash report\n\
             time (unix): {}\nos: {} {}\nthread: {thread}\n\
             message: {message}\nlocation: {location}\n\n\
             CosmicSlicer catches most of these and keeps running; this report helps fix the bug.\n\n\
             backtrace:\n{}\n",
            env!("CARGO_PKG_VERSION"),
            now_secs(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::backtrace::Backtrace::force_capture(),
        );
        if std::fs::create_dir_all(&folder).is_ok() {
            let name = format!("crash-{}-{}.txt", now_secs(), std::process::id());
            let _ = store::write_atomic(&folder.join(name), report.as_bytes());
            prune_reports(&folder);
        }
        previous(info);
    }));
}

fn prune_reports(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut files: Vec<(SystemTime, PathBuf)> =
        entries.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if files.len() <= MAX_REPORTS {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - MAX_REPORTS) {
        let _ = std::fs::remove_file(p);
    }
}

/// "3 minutes ago".
pub fn ago(secs: u64) -> String {
    let d = Duration::from_secs(secs).as_secs();
    match d {
        0..=59 => "less than a minute ago".into(),
        60..=3599 => format!("{} minute(s) ago", d / 60),
        3600..=86_399 => format!("{} hour(s) ago", d / 3600),
        _ => format!("{} day(s) ago", d / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::TempDir;

    fn session() -> Session {
        let mut s = Session::new();
        s.add_primitive("Cube");
        s.add_primitive("Torus");
        s.clone_selected();
        s.add_plate();
        s.add_primitive("Sphere");
        s.project.name = "Bracket".into();
        let schema = s.schema;
        s.presets.set_value(schema, CosmicSlicer_config::TabKind::Process, 0, "wall_loops", "4".into());
        s
    }

    #[test]
    fn a_snapshot_round_trips() {
        let s = session();
        let bytes = encode(&s);
        let r = decode(&bytes);
        assert!(r.is_ok(), "{:?}", r.as_ref().err());
        let Ok(r) = r else { return };
        assert_eq!(r.project.name, "Bracket");
        assert_eq!(r.model.objects.len(), s.model.objects.len());
        assert_eq!(r.model.plates, 2);
        for (a, b) in r.model.objects.iter().zip(&s.model.objects) {
            assert_eq!(a.transform, b.transform);
            assert_eq!(a.plate, b.plate);
            assert_eq!(a.mesh.triangles, b.mesh.triangles);
        }
        // The torus and its clone share one mesh, in the file and after reading it.
        assert!(Arc::ptr_eq(&r.model.objects[1].mesh, &r.model.objects[2].mesh));
        assert_eq!(r.presets.process.edits.values.get("wall_loops").map(String::as_str), Some("4"));
    }

    #[test]
    fn damaged_snapshots_are_errors() {
        let bytes = encode(&session());
        for cut in [0, 5, 16, bytes.len() / 2, bytes.len() - 1] {
            assert!(decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut lying = bytes.clone();
        // Claim an absurd manifest length.
        lying[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&lying).is_err());
        let mut flipped = bytes;
        let mid = flipped.len() - 20;
        flipped[mid] ^= 0xFF;
        // Bad mesh data must not crash; it may decode with odd numbers or fail.
        let _ = decode(&flipped);
    }

    #[test]
    fn a_dead_session_leaves_an_orphan_and_a_live_one_does_not() {
        let dir = TempDir::new("orphans");
        let first = Autosaver::start(&dir.0);
        assert!(first.is_ok(), "{:?}", first.as_ref().err());
        let Ok((mut live, orphans)) = first else { return };
        assert!(orphans.is_empty());
        assert!(live.save(&session()).is_ok());
        // Another instance starting now must not see the live session as orphaned.
        let second = Autosaver::start(&dir.0);
        assert!(second.as_ref().is_ok_and(|(_, o)| o.is_empty()));
        if let Ok((s, _)) = second {
            s.finish();
        }
        // The live one dies without cleaning up: its lock goes, its files stay.
        drop(live);
        let third = Autosaver::start(&dir.0);
        assert!(third.is_ok());
        let Ok((third, orphans)) = third else { return };
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].name, "Bracket");
        assert_eq!(orphans[0].objects, 4);
        assert!(orphans[0].read().is_ok());
        orphans[0].discard();
        third.finish();
        let fourth = Autosaver::start(&dir.0);
        assert!(fourth.as_ref().is_ok_and(|(_, o)| o.is_empty()));
    }

    #[test]
    fn autosave_waits_for_quiet() {
        let dir = TempDir::new("quiet");
        let Ok((mut a, _)) = Autosaver::start(&dir.0) else { return };
        let mut s = session();
        assert!(a.tick(&s, 0.0).is_none());
        assert!(!a.snapshot.exists(), "not saved straight away");
        assert!(a.tick(&s, AUTOSAVE_DELAY + 1.0).is_none());
        assert!(a.snapshot.exists());
        let before = std::fs::metadata(&a.snapshot).map(|m| m.len()).unwrap_or(0);
        s.add_primitive("Cone");
        a.tick(&s, 100.0);
        assert_eq!(std::fs::metadata(&a.snapshot).map(|m| m.len()).unwrap_or(0), before);
        a.tick(&s, 100.0 + AUTOSAVE_DELAY + 1.0);
        assert!(std::fs::metadata(&a.snapshot).map(|m| m.len()).unwrap_or(0) > before);
        a.finish();
    }
}
