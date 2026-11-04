//! Parity with OrcaSlicer (session 19). `tools/parity.sh` slices each reference case with
//! OrcaSlicer's command line into `<case>/orca.gcode`; this slices the same model with the
//! settings Orca wrote into that file, reads both G-code files back with the same reader, and
//! writes `<case>/report.txt` and `<dir>/summary.txt`: layers, time, filament, each feature's
//! volume and time, and how much of each layer's toolpaths cover the same ground. It also lists
//! where our profile library resolves Orca's presets differently.
//!
//! `PARITY_DIR=<dir> cargo test --release -p CosmicSlicer-engine --lib parity -- --ignored --nocapture`
//! (`PARITY_CASE` picks cases by name).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use glam::{Mat4, Vec3};

use CosmicSlicer_config::{Config, Schema, TabKind, userfiles};

use crate::gcode::Preview;
use crate::toolpath::Role;
use crate::{Job, JobObject, Progress, Sliced, gcodeview, slice, view_gcode};

/// Over this, a difference is flagged in the report.
const FLAG: f64 = 0.03;
/// Toolpath map cell (mm).
const CELL: f64 = 0.25;

/// What Orca calls each plate type, and its temperature option.
const BEDS: [(&str, &str); 7] = [
    ("Cool Plate (SuperTack)", "supertack_plate_temp"),
    ("Cool Plate", "cool_plate_temp"),
    ("Engineering Plate", "eng_plate_temp"),
    ("High Temp Plate", "hot_plate_temp"),
    ("Smooth PEI Plate", "hot_plate_temp"),
    ("Textured PEI Plate", "textured_plate_temp"),
    ("Textured Cool Plate", "textured_cool_plate_temp"),
];

#[test]
#[ignore]
fn parity_with_orca() {
    let Ok(dir) = std::env::var("PARITY_DIR") else { return };
    let only = std::env::var("PARITY_CASE").unwrap_or_default();
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .map(|r| r.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join("orca.gcode").is_file()).collect())
        .unwrap_or_default();
    cases.sort();
    let mut summary = String::from(
        "case                           layers        time (s)          filament (mm)     map    flagged\n",
    );
    for case in cases {
        let name = case.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !only.is_empty() && !name.contains(&only) {
            continue;
        }
        let r = compare(&case);
        println!("==== {name}\n{}", r.text);
        let _ = std::fs::write(case.join("report.txt"), &r.text);
        let _ = writeln!(summary, "{name:<30} {}", r.line);
    }
    println!("{summary}");
    let _ = std::fs::write(Path::new(&dir).join("summary.txt"), summary);
}

struct Report {
    text: String,
    line: String,
}

fn compare(case: &Path) -> Report {
    let fail = |why: String| Report { text: why.clone(), line: why };
    let Ok(orca_text) = std::fs::read_to_string(case.join("orca.gcode")) else {
        return fail("orca.gcode unreadable".into());
    };
    let cfg = gcodeview::embedded_config(&orca_text);
    if cfg.values.is_empty() {
        return fail("orca.gcode has no config block".into());
    }
    let mut text = String::new();
    presets(&cfg, &mut text);

    // The case's models in name order; with several, each prints with its own filament.
    let mut models: Vec<std::path::PathBuf> = std::fs::read_dir(case)
        .map(|r| {
            r.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "stl")).collect()
        })
        .unwrap_or_default();
    models.sort();
    if models.is_empty() {
        return fail("no model .stl".into());
    }
    let orca = view_gcode(orca_text, &cfg, &filaments_of(&cfg));
    let Some(job) = job_for(&models, &cfg, &orca.preview) else { return fail("model unreadable".into()) };
    let started = std::time::Instant::now();
    let ours = match slice(&job, &Progress::default()) {
        Ok(s) => s,
        Err(e) => return fail(format!("our slice failed: {e:?}")),
    };
    let secs = started.elapsed().as_secs_f64();
    let _ = std::fs::write(case.join("ours.gcode"), &ours.gcode);
    // Both through the same reader, so the comparison is of the toolpaths, not the estimators.
    let back = view_gcode(ours.gcode.clone(), &cfg, &filaments_of(&cfg));
    let _ = writeln!(text, "sliced in {secs:.2} s");

    let mut flagged = Vec::new();
    let mut row = |text: &mut String, what: &str, o: f64, u: f64| {
        let d = if o.abs() > 1e-9 {
            (u - o) / o
        } else if u.abs() > 1e-9 {
            1.0
        } else {
            0.0
        };
        let mark = if d.abs() > FLAG { " <<" } else { "" };
        if !mark.is_empty() {
            flagged.push(what.to_owned());
        }
        let _ = writeln!(text, "  {what:<28} orca {o:>12.1}  ours {u:>12.1}  {:>+7.1}%{mark}", d * 100.0);
    };
    let _ = writeln!(text, "totals (both files read by our reader):");
    row(&mut text, "layers", orca.preview.layers.len() as f64, back.preview.layers.len() as f64);
    row(&mut text, "time (s)", orca.stats.total_time, back.stats.total_time);
    let model = |s: &Sliced| s.stats.total_time - s.stats.prepare_time;
    row(&mut text, "from the first layer (s)", model(&orca), model(&back));
    row(&mut text, "travel time (s)", orca.stats.travel_time, back.stats.travel_time);
    let fil = |s: &Sliced| s.stats.filament_mm.iter().sum::<f64>();
    row(&mut text, "filament (mm)", fil(&orca), fil(&back));
    row(&mut text, "travels", orca.preview.travels.len() as f64, back.preview.travels.len() as f64);
    row(&mut text, "retractions", orca.preview.retractions.len() as f64, back.preview.retractions.len() as f64);
    let _ = writeln!(text, "estimates in the files' headers:");
    let est = |g: &str| header_time(g).unwrap_or(f64::NAN);
    row(&mut text, "header time (s)", est(&orca.gcode), est(&ours.gcode));
    let _ = writeln!(
        text,
        "  (our reader: {:.0} s on Orca's file, {:.0} s on ours; our slicer's own: {:.0} s)",
        orca.stats.total_time, back.stats.total_time, ours.stats.total_time
    );

    let _ = writeln!(text, "by feature: volume (mm³) and time (s):");
    // Filament each feature takes (from E, not the drawn widths).
    let (ov, uv) = (orca.stats.volume_by_role.to_vec(), back.stats.volume_by_role.to_vec());
    for role in Role::ALL {
        let i = role.index();
        let (o, u) = (ov.get(i).copied().unwrap_or(0.0), uv.get(i).copied().unwrap_or(0.0));
        let (ot, ut) = (
            orca.stats.time_by_role.get(i).copied().unwrap_or(0.0),
            back.stats.time_by_role.get(i).copied().unwrap_or(0.0),
        );
        if o + u + ot + ut < 1e-6 {
            continue;
        }
        row(&mut text, &format!("{} mm³", role.name()), o, u);
        row(&mut text, &format!("{} s", role.name()), ot, ut);
    }

    let _ = writeln!(text, "our slicer's estimate against our reader's, on our file (s):");
    row(&mut text, "travel", back.stats.travel_time, ours.stats.travel_time);
    for role in Role::ALL {
        let i = role.index();
        let (r, s) = (
            back.stats.time_by_role.get(i).copied().unwrap_or(0.0),
            ours.stats.time_by_role.get(i).copied().unwrap_or(0.0),
        );
        if r + s > 1e-6 {
            row(&mut text, role.name(), r, s);
        }
    }

    let (mean, worst) = maps(&orca.preview, &back.preview);
    let _ = writeln!(text, "toolpath map ({CELL} mm cells, same ground covered / either covers):");
    let _ = writeln!(text, "  mean {:.1}%", mean * 100.0);
    for (z, iou, o, u) in worst.iter().take(8) {
        let _ = writeln!(text, "  z {z:>7.2}: {:>5.1}%  (orca {o} cells, ours {u})", iou * 100.0);
    }
    // Pictures: the first layer, the middle one and the worst three (Orca red, ours blue).
    let mut zs: Vec<f64> = worst.iter().take(3).map(|w| w.0).collect();
    // And any asked for (PARITY_Z=29.6,10).
    zs.extend(std::env::var("PARITY_Z").unwrap_or_default().split(',').filter_map(|z| z.trim().parse::<f64>().ok()));
    let mut heights: Vec<f64> = worst.iter().map(|w| w.0).collect();
    heights.sort_by(f64::total_cmp);
    zs.extend(heights.first().copied());
    zs.extend(heights.get(heights.len() / 2).copied());
    for z in zs {
        if let Some(png) = picture(&orca.preview, &back.preview, z) {
            let _ = std::fs::write(case.join(format!("layer-{z:06.2}.png")), png);
        }
    }
    let line = format!(
        "{:>5}/{:<5}  {:>7.0}/{:<7.0}  {:>8.0}/{:<8.0}  {:>5.1}%  {}",
        orca.preview.layers.len(),
        back.preview.layers.len(),
        orca.stats.total_time,
        back.stats.total_time,
        fil(&orca),
        fil(&back),
        mean * 100.0,
        flagged.len()
    );
    Report { text, line }
}

/// Where our library's resolution of the presets Orca used differs from the values Orca wrote.
fn presets(cfg: &Config, text: &mut String) {
    let schema = Schema::get();
    for (kind, id) in [
        (TabKind::Printer, "printer_settings_id"),
        (TabKind::Process, "print_settings_id"),
        (TabKind::Filament, "filament_settings_id"),
    ] {
        let name = cfg.values.get(id).cloned().unwrap_or_default();
        let Some(ours) = userfiles::find_parent(kind, &name) else {
            let _ = writeln!(text, "preset {name:?}: not in our library");
            continue;
        };
        let differ: Vec<String> = ours
            .config
            .values
            .iter()
            .filter(|(k, _)| schema.option(k).is_some() && !k.ends_with("_settings_id"))
            .filter_map(|(k, v)| {
                let o = cfg.values.get(k)?;
                (!same(o, v)).then(|| format!("    {k}: orca {:?}, ours {:?}", short(o), short(v)))
            })
            .collect();
        let _ = writeln!(text, "preset {name:?}: {} values differ from Orca's", differ.len());
        for d in differ {
            let _ = writeln!(text, "{d}");
        }
    }
}

/// Equal as text, or item by item as numbers ("0.20" = "0.2", "50%" = "50 %"). Orca writes one
/// item of the lists kept per extruder variant (Direct Drive Standard / High Flow): the first.
fn same(orca: &str, ours: &str) -> bool {
    let (a, b) = (orca.trim(), ours.trim());
    if a == b {
        return true;
    }
    let items = |s: &str| s.split(',').map(|v| v.trim().trim_end_matches('%').trim().to_owned()).collect::<Vec<_>>();
    let (x, mut y) = (items(a), items(b));
    if x.len() == 1 && y.len() > 1 {
        y.truncate(1);
    }
    x.len() == y.len()
        && x.iter().zip(&y).all(|(p, q)| {
            p == q || matches!((p.parse::<f64>(), q.parse::<f64>()), (Ok(m), Ok(n)) if (m - n).abs() < 1e-9)
        })
}

fn short(v: &str) -> String {
    let one = v.replace('\n', "\\n");
    if one.chars().count() > 60 { format!("{}…", one.chars().take(60).collect::<String>()) } else { one }
}

/// Our job for `models` with Orca's settings, each placed where Orca printed it (model `k`
/// with filament `k` when there are several).
fn job_for(models: &[std::path::PathBuf], cfg: &Config, orca: &Preview) -> Option<Job> {
    let bed = cfg.values.get("curr_bed_type").map(String::as_str).unwrap_or("Textured PEI Plate");
    let bed_key = BEDS.iter().find(|b| b.0 == bed).map_or("textured_plate_temp", |b| b.1);
    let several = models.len() > 1;
    let mut objects = Vec::new();
    for (k, model) in models.iter().enumerate() {
        let file = CosmicSlicer_model::load(model).ok()?;
        let mesh = file.objects.into_iter().next()?.mesh.centred();
        let centre = object_centre(orca, several.then_some(k)).unwrap_or([110.0, 110.0]);
        objects.push(JobObject {
            name: model.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            mesh: Arc::new(mesh),
            matrix: Mat4::from_translation(Vec3::new(centre[0] as f32, centre[1] as f32, 0.0)),
            extruder: if several { k } else { 0 },
            overrides: Vec::new(),
            support_volumes: Vec::new(),
            support_paint: Vec::new(),
            seam_paint: Vec::new(),
            fuzzy_paint: Vec::new(),
            color_paint: Vec::new(),
            brim_ears: Vec::new(),
            volumes: Vec::new(),
            ranges: Vec::new(),
            layer_profile: Vec::new(),
        });
    }
    let n = models.len();
    Some(Job {
        printer: cfg.clone(),
        process: cfg.clone(),
        filaments: (0..n).map(|k| filament(cfg, k, n)).collect(),
        bed_key: bed_key.into(),
        bed_type_name: bed.into(),
        objects,
        plate_name: "Plate 1".into(),
        events: Vec::new(),
        calib: None,
    })
}

/// Each filament's settings from Orca's, as many as it names.
fn filaments_of(cfg: &Config) -> Vec<Config> {
    let n = cfg.values.get("filament_diameter").map_or(1, |v| v.split(',').count()).max(1);
    (0..n).map(|k| filament(cfg, k, n)).collect()
}

/// Filament `k` of `n`'s settings: Orca writes every per-filament number as a list of `n`.
fn filament(cfg: &Config, k: usize, n: usize) -> Config {
    if n < 2 {
        return cfg.clone();
    }
    let schema = Schema::get();
    let mut out = cfg.clone();
    for (key, v) in &cfg.values {
        let Some(def) = schema.option(key) else { continue };
        if !def.is_vector() || def.kind() == CosmicSlicer_config::OptionType::String {
            continue;
        }
        let items: Vec<&str> = v.split(',').collect();
        if items.len() == n
            && let Some(item) = items.get(k)
        {
            out.set(key.clone(), (*item).to_owned());
        }
    }
    out
}

fn is_object(r: Role) -> bool {
    !matches!(r, Role::Custom | Role::Skirt | Role::Brim | Role::PrimeTower)
}

/// The middle of what Orca printed of the object (start G-code, skirt and brim aside), or of
/// what filament `tool` printed.
fn object_centre(p: &Preview, tool: Option<usize>) -> Option<[f64; 2]> {
    let mut lo = [f64::MAX; 2];
    let mut hi = [f64::MIN; 2];
    // The outer walls' extent: symmetric about the part, unlike seams and wipes.
    let mine = |s: &&crate::gcode::Segment| tool.is_none_or(|t| usize::from(s.extruder) == t);
    let outer = p.segments.iter().filter(mine).any(|s| s.role == Role::OuterWall);
    for s in
        p.segments.iter().filter(mine).filter(|s| if outer { s.role == Role::OuterWall } else { is_object(s.role) })
    {
        for q in [s.a, s.b] {
            for k in 0..2 {
                let v = f64::from(q.get(k).copied().unwrap_or(0.0));
                lo[k] = lo[k].min(v);
                hi[k] = hi[k].max(v);
            }
        }
    }
    (lo[0] <= hi[0]).then(|| [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0])
}

/// Cells each layer's object toolpaths pass over, keyed by height (0.01 mm).
fn cells(p: &Preview) -> HashMap<i64, HashSet<(i32, i32)>> {
    let mut out: HashMap<i64, HashSet<(i32, i32)>> = HashMap::new();
    for l in &p.layers {
        let key = (f64::from(l.z) * 100.0).round() as i64;
        let set = out.entry(key).or_default();
        for s in p.segments.get(l.segments.clone()).unwrap_or(&[]).iter().filter(|s| is_object(s.role)) {
            let (a, b) = (Vec3::from(s.a), Vec3::from(s.b));
            let n = ((a.distance(b) as f64 / (CELL / 2.0)).ceil() as usize).clamp(1, 100_000);
            for k in 0..=n {
                let q = a.lerp(b, k as f32 / n as f32);
                set.insert(((f64::from(q.x) / CELL).floor() as i32, (f64::from(q.y) / CELL).floor() as i32));
            }
        }
    }
    out
}

/// Mean share of ground covered by both, per layer, and the worst layers (z, share, cells).
fn maps(orca: &Preview, ours: &Preview) -> (f64, Vec<(f64, f64, usize, usize)>) {
    let (a, b) = (cells(orca), cells(ours));
    let empty = HashSet::new();
    let mut keys: Vec<i64> = a.keys().chain(b.keys()).copied().collect::<HashSet<_>>().into_iter().collect();
    keys.sort_unstable();
    let mut per: Vec<(f64, f64, usize, usize)> = keys
        .iter()
        .filter_map(|k| {
            let (x, y) = (a.get(k).unwrap_or(&empty), b.get(k).unwrap_or(&empty));
            let union = x.union(y).count();
            (union > 0).then(|| (*k as f64 / 100.0, x.intersection(y).count() as f64 / union as f64, x.len(), y.len()))
        })
        .collect();
    let mean = if per.is_empty() { 0.0 } else { per.iter().map(|p| p.1).sum::<f64>() / per.len() as f64 };
    per.sort_by(|p, q| p.1.total_cmp(&q.1));
    (mean, per)
}

/// Layer `z` of both, drawn at 12 px/mm with each line's width: Orca red, ours blue, both
/// magenta; travels as thin grey lines.
fn picture(orca: &Preview, ours: &Preview, z: f64) -> Option<Vec<u8>> {
    const PX: f64 = 12.0;
    let layer = |p: &Preview| -> Vec<crate::gcode::Segment> {
        p.layers
            .iter()
            .filter(|l| (f64::from(l.z) - z).abs() < 0.005)
            .flat_map(|l| p.segments.get(l.segments.clone()).unwrap_or(&[]).iter().copied())
            .filter(|s| is_object(s.role))
            .collect()
    };
    let (a, b) = (layer(orca), layer(ours));
    let mut lo = [f64::MAX; 2];
    let mut hi = [f64::MIN; 2];
    for s in a.iter().chain(&b) {
        for q in [s.a, s.b] {
            lo = [lo[0].min(f64::from(q[0])), lo[1].min(f64::from(q[1]))];
            hi = [hi[0].max(f64::from(q[0])), hi[1].max(f64::from(q[1]))];
        }
    }
    if lo[0] > hi[0] {
        return None;
    }
    let (w, h) = (((hi[0] - lo[0] + 2.0) * PX).min(4000.0) as u32, ((hi[1] - lo[1] + 2.0) * PX).min(4000.0) as u32);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    let mut stamp = |x: f64, y: f64, r: f64, channel: usize| {
        let (cx, cy) = ((x - lo[0] + 1.0) * PX, (hi[1] - y + 1.0) * PX);
        let rr = (r * PX).max(0.5);
        for py in (cy - rr).floor() as i64..=(cy + rr).ceil() as i64 {
            for px in (cx - rr).floor() as i64..=(cx + rr).ceil() as i64 {
                let (dx, dy) = (px as f64 - cx, py as f64 - cy);
                if px < 0 || py < 0 || px >= i64::from(w) || py >= i64::from(h) || dx * dx + dy * dy > rr * rr {
                    continue;
                }
                let i = ((py as u32 * w + px as u32) * 4) as usize;
                // Orca takes the green and blue out (red), ours the red and green (blue).
                for c in [channel, 1] {
                    if let Some(v) = rgba.get_mut(i + c) {
                        *v = v.saturating_sub(110);
                    }
                }
            }
        }
    };
    for (segs, channel) in [(&a, 2usize), (&b, 0usize)] {
        for s in segs.iter() {
            let (p, q) = (Vec3::from(s.a), Vec3::from(s.b));
            let n = ((p.distance(q) as f64 * PX / 2.0).ceil() as usize).clamp(1, 20_000);
            for k in 0..=n {
                let m = p.lerp(q, k as f32 / n as f32);
                stamp(f64::from(m.x), f64::from(m.y), f64::from(s.width) * 0.45, channel);
            }
        }
    }
    crate::thumbnail::png(&rgba, w, h)
}

/// "; model printing time: 1h 2m 3s; …" (the start and end G-code aside; or "estimated printing
/// time (normal mode) = …").
fn header_time(g: &str) -> Option<f64> {
    let line =
        g.lines().take(400).find(|l| l.contains("model printing time") || l.contains("estimated printing time"))?;
    let t = line.split(';').find(|p| p.contains("model printing time")).unwrap_or(line).rsplit([':', '=']).next()?;
    let mut secs = 0.0;
    for part in t.split_whitespace() {
        let (n, unit) = part.split_at(part.find(|c: char| c.is_alphabetic())?);
        let n: f64 = n.parse().ok()?;
        secs += n * match unit {
            "d" => 86_400.0,
            "h" => 3_600.0,
            "m" => 60.0,
            _ => 1.0,
        };
    }
    Some(secs)
}

#[test]
fn header_times_read() {
    assert_eq!(header_time("; model printing time: 35m 21s; total estimated time: 1h 42m 1s\n"), Some(2121.0));
    assert_eq!(header_time("; estimated printing time (normal mode) = 2d 3s\n"), Some(172_803.0));
    assert!(same("0.20,50%", "0.2,50"));
    assert!(!same("0.2", "0.3"));
    assert!(same("300", "300,400") && !same("300,300", "300,400"));
}
