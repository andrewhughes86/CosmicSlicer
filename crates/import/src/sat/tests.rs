//! SAT reading, on small files written here and the format's own example.

use std::collections::HashMap;

use super::*;

/// Signed volume and whether every edge is shared by exactly two triangles.
fn volume_closed(m: &crate::geom::Mesh) -> (f64, bool) {
    let mut vol = 0.0;
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for t in m.triangles() {
        let [a, b, c] = m.corners(*t).map(|p| DVec3::new(f64::from(p.x), f64::from(p.y), f64::from(p.z)));
        vol += a.dot(b.cross(c)) / 6.0;
        for k in 0..3 {
            let (i, j) = (t[k], t[(k + 1) % 3]);
            *edges.entry((i.min(j), i.max(j))).or_insert(0) += 1;
        }
    }
    (vol, edges.values().all(|&n| n == 2))
}

fn read_text(text: &str) -> Result<Imported, ImportError> {
    read(text.as_bytes(), Options::default())
}

/// The cylinder example of the SAT format document (release 4.0, inches, a transform that turns
/// the cylinder's axis to Y and moves it 10 along it).
pub(crate) const CYLINDER: &str = "400 0 1 0
11 Scheme AIDE 11 ACIS 4.0 NT 24 Mon Apr 12 13:59:03 1998
25.4 1e-06 1e-10
-0 body $1 $2 $-1 $3 #
-1 display_attribute-st-attrib $-1 $4 $-1 $0 1 #
-2 lump $-1 $-1 $5 $0 #
-3 transform $-1 1 0 0 0 0 -1 0 1 0 0 10 0 1 rotate no_reflect
   no_shear #
-4 rgb_color-st-attrib $-1 $6 $1 $0 0 1 0 #
-5 shell $-1 $-1 $-1 $7 $-1 $2 #
-6 id_attribute-st-attrib $-1 $-1 $4 $0 1 #
-7 face $-1 $8 $9 $5 $-1 $10 forward single #
-8 face $-1 $11 $12 $5 $-1 $13 forward single #
-9 loop $-1 $14 $15 $7 #
-10 cone-surface $-1 0 0 0 0 0 1 10 0 0 1 I I 0 1 forward I I I I #
-11 face $-1 $-1 $16 $5 $-1 $17 forward single #
-12 loop $-1 $-1 $18 $8 #
-13 plane-surface $-1 0 0 -10 0 0 -1 -1 0 0 forward_v I I I I #
-14 loop $-1 $-1 $19 $7 #
-15 coedge $-1 $15 $15 $18 $20 1 $9 $-1 #
-16 loop $-1 $-1 $21 $11 #
-17 plane-surface $-1 0 0 10 0 0 1 1 0 0 forward_v I I I I #
-18 coedge $-1 $18 $18 $15 $20 0 $12 $-1 #
-19 coedge $-1 $19 $19 $21 $22 1 $14 $-1 #
-20 edge $-1 $23 $23 $18 $24 forward #
-21 coedge $-1 $21 $21 $19 $22 0 $16 $-1 #
-22 edge $-1 $25 $25 $21 $26 forward #
-23 vertex $-1 $20 $27 #
-24 ellipse-curve $-1 0 0 -10 0 0 -1 10 0 0 1 I I #
-25 vertex $-1 $22 $28 #
-26 ellipse-curve $-1 0 0 10 0 0 1 10 0 0 1 I I #
-27 point $-1 10 0 -10 #
-28 point $-1 10 0 10 #
End-of-ACIS-data
";

#[test]
fn the_format_documents_cylinder_is_closed_and_placed() -> Result<(), ImportError> {
    let m = read_text(CYLINDER)?;
    assert_eq!(m.units, Some(Units::Inches));
    let (vol, closed) = volume_closed(&m.mesh);
    assert!(closed, "watertight");
    let exact = std::f64::consts::PI * 100.0 * 20.0;
    assert!((vol - exact).abs() / exact < 0.01, "{vol} vs {exact}");
    // The transform turns the axis from Z to Y and moves it 10 along: Y runs 0 to 20.
    let b = m.mesh.bounds();
    assert!((b.min.y - 0.0).abs() < 1e-3 && (b.max.y - 20.0).abs() < 1e-3, "{b:?}");
    assert!((b.max.x - 10.0).abs() < 0.02 && (b.max.z - 10.0).abs() < 0.02, "{b:?}");
    Ok(())
}

/// Writes release 7.0 records: each gets an entity id and a pattern pointer after its
/// attribute, as files of that release do.
struct W {
    recs: Vec<String>,
}

impl W {
    fn new() -> Self {
        Self { recs: Vec::new() }
    }

    /// Reserves a record number.
    fn next(&self) -> usize {
        self.recs.len()
    }

    fn add(&mut self, kind: &str, data: &str) -> usize {
        let n = self.recs.len();
        self.recs.push(format!("-{n} {kind} $-1 -1 $-1 {data} #"));
        n
    }

    fn set(&mut self, n: usize, kind: &str, data: &str) {
        if let Some(r) = self.recs.get_mut(n) {
            *r = format!("-{n} {kind} $-1 -1 $-1 {data} #");
        }
    }

    fn text(&self, mm_per_unit: f64) -> String {
        format!(
            "700 0 1 0\n@4 test @12 ACIS 7.0 NT @24 Mon Oct 26 12:00:00 2026\n{mm_per_unit} 9.9999999999999995e-07 1e-10\n{}\nEnd-of-ACIS-data\n",
            self.recs.join("\n")
        )
    }

    /// A box from `lo` to `hi`: six planes, twelve straight edges, eight vertices.
    fn cuboid(&mut self, lo: [f64; 3], hi: [f64; 3]) {
        let corner = |i: usize| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        // Each face's corners counter-clockwise about its outward normal.
        let faces: [([usize; 4], [f64; 3]); 6] = [
            ([0, 2, 3, 1], [0.0, 0.0, -1.0]),
            ([4, 5, 7, 6], [0.0, 0.0, 1.0]),
            ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
            ([3, 2, 6, 7], [0.0, 1.0, 0.0]),
            ([0, 4, 6, 2], [-1.0, 0.0, 0.0]),
            ([1, 3, 7, 5], [1.0, 0.0, 0.0]),
        ];
        let body = self.add("body", "$1 $-1 $-1");
        let lump = self.add("lump", "$-1 $2 $0");
        let shell = self.next();
        self.add("shell", "");
        // Records reserved now, filled in below.
        let reserve = |w: &mut W, n: usize| -> Vec<usize> { (0..n).map(|_| w.add("x", "")).collect() };
        let face_ids = reserve(self, 6);
        let loop_ids = reserve(self, 6);
        let coedges = reserve(self, 24);
        let mut edge_of: HashMap<(usize, usize), usize> = HashMap::new();
        let mut edge_coedges: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut vertex_ids = HashMap::new();
        for i in 0..8 {
            let p = corner(i);
            let point = self.add("point", &format!("{} {} {}", p[0], p[1], p[2]));
            let v = self.add("vertex", &format!("$-1 ${point}"));
            vertex_ids.insert(i, v);
        }
        for (f, (quad, n)) in faces.iter().enumerate() {
            let p = corner(quad[0]);
            let u = [n[1].abs(), n[2].abs(), n[0].abs()];
            let surface = self.add(
                "plane-surface",
                &format!(
                    "{} {} {} {} {} {} {} {} {} forward_v I I I I",
                    p[0], p[1], p[2], n[0], n[1], n[2], u[0], u[1], u[2]
                ),
            );
            let next_face = face_ids.get(f + 1).map_or(-1, |x| *x as i64);
            self.set(
                face_ids[f],
                "face",
                &format!("${next_face} ${} ${shell} $-1 ${surface} forward single", loop_ids[f]),
            );
            self.set(loop_ids[f], "loop", &format!("$-1 ${} ${}", coedges[f * 4], face_ids[f]));
            for k in 0..4 {
                let (a, b) = (quad[k], quad[(k + 1) % 4]);
                let key = (a.min(b), a.max(b));
                let edge = match edge_of.get(&key) {
                    Some(e) => *e,
                    None => {
                        let (pa, pb) = (corner(key.0), corner(key.1));
                        let d = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
                        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                        let curve = self.add(
                            "straight-curve",
                            &format!("{} {} {} {} {} {} I I", pa[0], pa[1], pa[2], d[0] / len, d[1] / len, d[2] / len),
                        );
                        let e = self.add("edge", "");
                        self.set(
                            e,
                            "edge",
                            &format!(
                                "${} 0 ${} {len} ${} ${curve} forward @7 unknown",
                                vertex_ids[&key.0],
                                vertex_ids[&key.1],
                                coedges[f * 4 + k]
                            ),
                        );
                        edge_of.insert(key, e);
                        e
                    }
                };
                edge_coedges.entry(edge).or_default().push(coedges[f * 4 + k]);
                let sense = if a < b { "forward" } else { "reversed" };
                let (next, prev) = (coedges[f * 4 + (k + 1) % 4], coedges[f * 4 + (k + 3) % 4]);
                self.set(
                    coedges[f * 4 + k],
                    "coedge",
                    &format!("${next} ${prev} $-1 ${edge} {sense} ${} $-1", loop_ids[f]),
                );
            }
        }
        self.set(shell, "shell", &format!("$-1 $-1 ${} $-1 ${lump}", face_ids[0]));
        let _ = body;
    }
}

#[test]
fn a_release_7_box_is_closed_with_its_volume() -> Result<(), ImportError> {
    let mut w = W::new();
    w.cuboid([0.0, 0.0, 0.0], [10.0, 20.0, 5.0]);
    // An attribute carrying a string with a terminator in it, and a curve the reader skips.
    w.add("name_attrib-gen-attrib", "$-1 $-1 $0 @9 part #1 a");
    let m = read_text(&w.text(1.0))?;
    assert_eq!(m.units, Some(Units::Millimetres));
    let (vol, closed) = volume_closed(&m.mesh);
    assert!(closed);
    assert!((vol - 1000.0).abs() < 1e-3, "{vol}");
    Ok(())
}

#[test]
fn a_whole_sphere_needs_no_loops() -> Result<(), ImportError> {
    let mut w = W::new();
    w.add("body", "$1 $-1 $-1");
    w.add("lump", "$-1 $2 $0");
    w.add("shell", "$-1 $-1 $3 $-1 $1");
    w.add("face", "$-1 $-1 $2 $-1 $4 forward single");
    w.add("sphere-surface", "1 2 3 5 1 0 0 0 0 1 forward_v I I I I");
    let m = read_text(&w.text(1.0))?;
    let (vol, closed) = volume_closed(&m.mesh);
    let exact = 4.0 / 3.0 * std::f64::consts::PI * 125.0;
    assert!(closed && (vol - exact).abs() / exact < 0.01, "{vol} vs {exact}");
    let b = m.mesh.bounds();
    assert!((b.min.z - -2.0).abs() < 0.01 && (b.max.z - 8.0).abs() < 0.01, "{b:?}");
    Ok(())
}

#[test]
fn units_and_hostile_files() {
    // Centimetres are scaled to millimetres; units the enum lacks too.
    let mut w = W::new();
    w.cuboid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
    if let Ok(m) = read_text(&w.text(10.0)) {
        assert_eq!(m.units, Some(Units::Centimetres));
    }
    let m = read_text(&w.text(2.0));
    assert!(
        m.as_ref().is_ok_and(|m| (volume_closed(&m.mesh).0 - 8.0).abs() < 1e-3 && m.units == Some(Units::Millimetres))
    );
    for text in [
        "",
        "hello",
        "700 0 1 0\n@4 test @3 abc @3 now\n1 1e-6 1e-10\n",
        "700 0 1 0\n@4 test @3 abc @3 now\n1 1e-6 1e-10\nface $-1 -1 $-1 $9 $9 $9 $-1 $9 forward single #\n",
        "700 0 1 0\n@4 test @3 abc @3 now\n1 1e-6 1e-10\n-0 face $-1 -1 $-1 $-1 $1 $-1 $-1 $2 forward single #\n-1 loop $-1 -1 $-1 $1 $3 $0 #\n-2 spline-surface $-1 -1 $-1 forward { exactsur nubs 3 3 open open none none 99999999 2 } I I I I #\n-3 coedge $-1 -1 $-1 $3 $3 $-1 $4 forward $1 $-1 #\n-4 edge $-1 -1 $-1 $5 0 $5 1 $3 $-1 forward #\n-5 vertex $-1 -1 $-1 $4 $6 #\n-6 point $-1 -1 $-1 0 0 0 #\n",
        "ACIS BinaryFile",
        CYLINDER.get(..CYLINDER.len() / 2).unwrap_or_default(),
    ] {
        assert!(read_text(text).is_err(), "{text}");
    }
}

/// Damaged pointers that make coedge or loop rings never come back to their start (found by
/// the fuzzer: the walk once went round until it had taken 4 GB) are each followed once.
#[test]
fn rings_that_never_close_stop() {
    let open_ring = CYLINDER.replace("-14 loop $-1", "-14 loop0$-1");
    let mut lines: Vec<&str> = CYLINDER.lines().collect();
    for _ in 0..8 {
        lines.insert(13, "-9 loop $-1 $ $12 $5 $-1 $13 forward single #");
    }
    for text in [open_ring, lines.join("\n")] {
        let start = std::time::Instant::now();
        let _ = read_text(&text);
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
    }
}
