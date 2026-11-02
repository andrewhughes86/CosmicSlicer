//! Performance budgets on a reference set: OrcaSlicer's calibration towers (in
//! `assets/calib`) and primitives with supports, sliced one after another with the time each
//! takes checked against its budget, and on Linux the process's peak memory. CI runs it on
//! its own (`--ignored --test-threads=1`; other tests running alongside would skew the
//! times). Budgets are about three times what a 4-core laptop takes in this test build (at
//! least 5 s), so a failure means a real slowdown, not a busy runner. `BUDGET_ONLY=<part of a
//! name>` runs just those cases.

use super::*;
use CosmicSlicer_model::{Mesh, primitives};
use glam::Vec3;

const MAX_PEAK_MB: u64 = 1500;

/// A name, the model, the settings changed and the budget in seconds.
type Case<'a> = (&'a str, Mesh, &'a [(&'a str, &'a str)], f64);

fn calib(name: &str) -> Mesh {
    let z: &[u8] = match name {
        "temperature_tower" => include_bytes!("../../../assets/calib/temperature_tower.stl.z"),
        "retraction_tower" => include_bytes!("../../../assets/calib/retraction_tower.stl.z"),
        "SpeedTestStructure" => include_bytes!("../../../assets/calib/SpeedTestStructure.stl.z"),
        "ringing_tower" => include_bytes!("../../../assets/calib/ringing_tower.stl.z"),
        "fast_tower_test" => include_bytes!("../../../assets/calib/fast_tower_test.stl.z"),
        _ => &[],
    };
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(z, 64 << 20)
        .ok()
        .and_then(|b| CosmicSlicer_model::stl::parse(&b).ok())
        .unwrap_or_default()
}

/// The bottom `height` mm of a tower, as the calibration dialog cuts it to the range asked for.
fn tower_part(mesh: Mesh, height: f32) -> Mesh {
    let b = mesh.bounds();
    CosmicSlicer_model::cut::cut(&mesh, Vec3::new(0.0, 0.0, b.min.z + height), Vec3::Z).below
}

fn job(mesh: Mesh, settings: &[(&str, &str)]) -> Job {
    let mut printer = Config::default();
    printer.set("printable_area", "0x0,250x0,250x250,0x250");
    printer.set("printable_height", "300");
    let mut process = Config::default();
    for (k, v) in settings {
        process.set(*k, *v);
    }
    let mut filament = Config::default();
    filament.set("filament_density", "1.24");
    Job {
        printer,
        process,
        filaments: vec![filament],
        bed_key: "textured_plate_temp".into(),
        bed_type_name: "Textured PEI Plate".into(),
        objects: vec![JobObject {
            name: "reference".into(),
            mesh: Arc::new(mesh.centred()),
            matrix: Mat4::from_translation(Vec3::new(125.0, 125.0, 0.0)),
            extruder: 0,
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
        }],
        plate_name: "Plate 1".into(),
        events: Vec::new(),
        calib: None,
    }
}

fn peak_rss_mb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb / 1024)
}

#[test]
#[ignore = "timing: run alone with --ignored --test-threads=1 (CI does)"]
fn the_reference_set_slices_within_budget() {
    let tree = [("enable_support", "1"), ("support_type", "tree(auto)")];
    let grid = [("enable_support", "1"), ("support_type", "normal(auto)")];
    let gyroid = [("sparse_infill_pattern", "gyroid"), ("sparse_infill_density", "20%")];
    let only = std::env::var("BUDGET_ONLY").unwrap_or_default();
    let cases: Vec<Case<'_>> = vec![
        ("temperature tower", tower_part(calib("temperature_tower"), 50.0), &[], 5.0),
        ("retraction tower", calib("retraction_tower"), &[], 5.0),
        ("speed test structure", calib("SpeedTestStructure"), &[], 90.0),
        ("ringing tower", calib("ringing_tower"), &[], 5.0),
        ("fast tower", calib("fast_tower_test"), &[], 5.0),
        ("sphere, tree support", primitives::sphere(25.0, 96), &tree, 12.0),
        ("sphere, grid support", primitives::sphere(25.0, 96), &grid, 10.0),
        ("torus, gyroid", primitives::torus(30.0, 10.0, 96, 48), &gyroid, 15.0),
    ];
    let mut over = Vec::new();
    for (name, mesh, settings, budget) in cases.into_iter().filter(|c| c.0.contains(only.as_str())) {
        assert!(!mesh.triangles.is_empty(), "{name}: no mesh");
        let j = job(mesh, settings);
        let progress = Progress::default();
        let started = std::time::Instant::now();
        let out = slice(&j, &progress);
        let secs = started.elapsed().as_secs_f64();
        let Ok(out) = out else {
            over.push(format!("{name}: {:?}", out.err()));
            continue;
        };
        let stages: Vec<String> =
            progress.timings().iter().filter(|(_, t)| *t >= 0.05).map(|(s, t)| format!("{s} {t:.1}")).collect();
        println!(
            "{name}: {secs:.2} s of {budget:.0} ({} layers, {} segments; {}), peak {} MB",
            out.stats.layers,
            out.preview.segments.len(),
            stages.join(", "),
            peak_rss_mb().map_or("?".into(), |m| m.to_string())
        );
        if secs > budget {
            over.push(format!("{name}: {secs:.1} s, budget {budget:.0} s ({})", stages.join(", ")));
        }
    }
    if let Some(mb) = peak_rss_mb().filter(|mb| *mb > MAX_PEAK_MB) {
        over.push(format!("peak memory {mb} MB, budget {MAX_PEAK_MB} MB"));
    }
    assert!(over.is_empty(), "{over:#?}");
}
