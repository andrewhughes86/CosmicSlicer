//! STEP reading, on small models written here (analytic shapes with known volumes).

use super::*;
use std::fmt::Write as _;

/// Writes Part 21 instances, numbering them.
#[derive(Default)]
pub(crate) struct W {
    body: String,
    n: u64,
}

impl W {
    fn add(&mut self, s: &str) -> u64 {
        self.n += 1;
        let _ = writeln!(self.body, "#{}={s};", self.n);
        self.n
    }

    fn pt(&mut self, p: [f64; 3]) -> u64 {
        self.add(&format!("CARTESIAN_POINT('',({:?},{:?},{:?}))", p[0], p[1], p[2]))
    }

    fn dir(&mut self, d: [f64; 3]) -> u64 {
        self.add(&format!("DIRECTION('',({:?},{:?},{:?}))", d[0], d[1], d[2]))
    }

    fn axis(&mut self, o: [f64; 3], z: [f64; 3], x: [f64; 3]) -> u64 {
        let (o, z, x) = (self.pt(o), self.dir(z), self.dir(x));
        self.add(&format!("AXIS2_PLACEMENT_3D('',#{o},#{z},#{x})"))
    }

    fn vertex(&mut self, p: [f64; 3]) -> u64 {
        let p = self.pt(p);
        self.add(&format!("VERTEX_POINT('',#{p})"))
    }

    fn line_edge(&mut self, a: u64, pa: [f64; 3], b: u64, pb: [f64; 3]) -> u64 {
        let o = self.pt(pa);
        let d = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let dir = self.dir([d[0] / len, d[1] / len, d[2] / len]);
        let v = self.add(&format!("VECTOR('',#{dir},{len:?})"));
        let l = self.add(&format!("LINE('',#{o},#{v})"));
        self.add(&format!("EDGE_CURVE('',#{a},#{b},#{l},.T.)"))
    }

    fn face(&mut self, surface: u64, bounds: &[(Vec<(u64, bool)>, bool)], same: bool) -> u64 {
        let mut refs = Vec::new();
        for (k, (edges, orient)) in bounds.iter().enumerate() {
            let oes: Vec<String> = edges
                .iter()
                .map(|(e, fwd)| {
                    format!("#{}", self.add(&format!("ORIENTED_EDGE('',*,*,#{e},.{}.)", if *fwd { "T" } else { "F" })))
                })
                .collect();
            let lp = self.add(&format!("EDGE_LOOP('',({}))", oes.join(",")));
            let kind = if k == 0 { "FACE_OUTER_BOUND" } else { "FACE_BOUND" };
            refs.push(format!("#{}", self.add(&format!("{kind}('',#{lp},.{}.)", if *orient { "T" } else { "F" }))));
        }
        self.add(&format!("ADVANCED_FACE('',({}),#{surface},.{}.)", refs.join(","), if same { "T" } else { "F" }))
    }

    /// Units (`unit` is a length unit instance) and a representation of `items`.
    pub(crate) fn rep(&mut self, kind: &str, items: &[u64], unit: u64) -> u64 {
        let angle = self.add("( NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.) )");
        let ctx = self.add(&format!(
            "( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNIT_ASSIGNED_CONTEXT((#{unit},#{angle})) REPRESENTATION_CONTEXT('','') )"
        ));
        let list: Vec<String> = items.iter().map(|i| format!("#{i}")).collect();
        self.add(&format!("{kind}('',({}),#{ctx})", list.join(",")))
    }

    pub(crate) fn mm(&mut self) -> u64 {
        self.add("( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) )")
    }

    pub(crate) fn text(&self) -> String {
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n{}ENDSEC;\nEND-ISO-10303-21;\n",
            self.body
        )
    }

    /// A box from `lo` to `hi` as a solid; returns the MANIFOLD_SOLID_BREP.
    pub(crate) fn cuboid(&mut self, lo: [f64; 3], hi: [f64; 3]) -> u64 {
        let corner = |i: usize| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        let v: Vec<u64> = (0..8).map(|i| self.vertex(corner(i))).collect();
        // Faces as corner rings, counter-clockwise seen from outside, with their normals.
        let faces: [([usize; 4], [f64; 3]); 6] = [
            ([0, 2, 3, 1], [0.0, 0.0, -1.0]),
            ([4, 5, 7, 6], [0.0, 0.0, 1.0]),
            ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
            ([2, 6, 7, 3], [0.0, 1.0, 0.0]),
            ([0, 4, 6, 2], [-1.0, 0.0, 0.0]),
            ([1, 3, 7, 5], [1.0, 0.0, 0.0]),
        ];
        let mut edges: HashMap<(usize, usize), u64> = HashMap::new();
        let mut fids = Vec::new();
        for (ring, n) in faces {
            let mut loop_edges = Vec::new();
            for k in 0..4 {
                let (a, b) = (ring[k], ring[(k + 1) % 4]);
                let key = (a.min(b), a.max(b));
                let e = match edges.get(&key) {
                    Some(e) => *e,
                    None => {
                        let e = self.line_edge(v[key.0], corner(key.0), v[key.1], corner(key.1));
                        edges.insert(key, e);
                        e
                    }
                };
                loop_edges.push((e, a < b));
            }
            let x = if n[0] != 0.0 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
            let ax = self.axis(corner(ring[0]), n, x);
            let plane = self.add(&format!("PLANE('',#{ax})"));
            fids.push(self.face(plane, &[(loop_edges, true)], true));
        }
        let list: Vec<String> = fids.iter().map(|f| format!("#{f}")).collect();
        let shell = self.add(&format!("CLOSED_SHELL('',({}))", list.join(",")));
        self.add(&format!("MANIFOLD_SOLID_BREP('',#{shell})"))
    }

    /// A cylinder of radius `r` from z = 0 to `h`; `seam` adds a seam edge (one loop on the
    /// side face) instead of two circle loops.
    pub(crate) fn cylinder(&mut self, r: f64, h: f64, seam: bool) -> u64 {
        let (v1, v2) = (self.vertex([r, 0.0, 0.0]), self.vertex([r, 0.0, h]));
        let a1 = self.axis([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let a2 = self.axis([0.0, 0.0, h], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let c1 = self.add(&format!("CIRCLE('',#{a1},{r:?})"));
        let c2 = self.add(&format!("CIRCLE('',#{a2},{r:?})"));
        let e1 = self.add(&format!("EDGE_CURVE('',#{v1},#{v1},#{c1},.T.)"));
        let e2 = self.add(&format!("EDGE_CURVE('',#{v2},#{v2},#{c2},.T.)"));
        let side_axis = self.axis([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let cyl = self.add(&format!("CYLINDRICAL_SURFACE('',#{side_axis},{r:?})"));
        let side = if seam {
            let s = self.line_edge(v1, [r, 0.0, 0.0], v2, [r, 0.0, h]);
            self.face(cyl, &[(vec![(e1, true), (s, true), (e2, false), (s, false)], true)], true)
        } else {
            self.face(cyl, &[(vec![(e1, true)], true), (vec![(e2, false)], true)], true)
        };
        let bottom_axis = self.axis([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]);
        let bottom_plane = self.add(&format!("PLANE('',#{bottom_axis})"));
        let bottom = self.face(bottom_plane, &[(vec![(e1, false)], true)], true);
        let top_axis = self.axis([0.0, 0.0, h], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let top_plane = self.add(&format!("PLANE('',#{top_axis})"));
        let top = self.face(top_plane, &[(vec![(e2, true)], true)], true);
        let shell = self.add(&format!("CLOSED_SHELL('',(#{side},#{bottom},#{top}))"));
        self.add(&format!("MANIFOLD_SOLID_BREP('',#{shell})"))
    }

    /// A hemisphere of radius `r` on the XY plane.
    fn dome(&mut self, r: f64) -> u64 {
        let v = self.vertex([r, 0.0, 0.0]);
        let a = self.axis([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let c = self.add(&format!("CIRCLE('',#{a},{r:?})"));
        let e = self.add(&format!("EDGE_CURVE('',#{v},#{v},#{c},.T.)"));
        let sa = self.axis([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let sphere = self.add(&format!("SPHERICAL_SURFACE('',#{sa},{r:?})"));
        let cap = self.face(sphere, &[(vec![(e, true)], true)], true);
        let ba = self.axis([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]);
        let plane = self.add(&format!("PLANE('',#{ba})"));
        let base = self.face(plane, &[(vec![(e, false)], true)], true);
        let shell = self.add(&format!("CLOSED_SHELL('',(#{cap},#{base}))"));
        self.add(&format!("MANIFOLD_SOLID_BREP('',#{shell})"))
    }
}

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

#[test]
fn a_box_is_closed_with_its_volume() -> Result<(), ImportError> {
    let mut w = W::default();
    let solid = w.cuboid([0.0, 0.0, 0.0], [10.0, 20.0, 5.0]);
    let mm = w.mm();
    w.rep("ADVANCED_BREP_SHAPE_REPRESENTATION", &[solid], mm);
    let m = read_text(&w.text())?;
    assert_eq!(m.units, Some(Units::Millimetres));
    assert_eq!(m.mesh.triangles().len(), 12);
    let (vol, closed) = volume_closed(&m.mesh);
    assert!(closed);
    assert!((vol - 1000.0).abs() < 1e-3, "{vol}");
    Ok(())
}

#[test]
fn cylinders_with_and_without_seams_are_closed() -> Result<(), ImportError> {
    for seam in [false, true] {
        let mut w = W::default();
        let solid = w.cylinder(5.0, 10.0, seam);
        let mm = w.mm();
        w.rep("ADVANCED_BREP_SHAPE_REPRESENTATION", &[solid], mm);
        let m = read_text(&w.text())?;
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let (vol, closed) = volume_closed(&m.mesh);
        assert!(closed, "seam {seam}");
        let exact = std::f64::consts::PI * 25.0 * 10.0;
        // The tessellation is inscribed, within 0.01 mm of the surface.
        assert!(vol < exact && vol > exact * (1.0 - 2.0 * 0.01 / 5.0), "seam {seam}: {vol} vs {exact}");
    }
    Ok(())
}

#[test]
fn a_dome_meets_the_tolerance() -> Result<(), ImportError> {
    let mut w = W::default();
    let solid = w.dome(20.0);
    let mm = w.mm();
    w.rep("ADVANCED_BREP_SHAPE_REPRESENTATION", &[solid], mm);
    let m = read_text(&w.text())?;
    let (vol, closed) = volume_closed(&m.mesh);
    assert!(closed);
    let exact = 2.0 / 3.0 * std::f64::consts::PI * 8000.0;
    assert!((vol - exact).abs() < exact * 3.0 * 0.01 / 20.0, "{vol} vs {exact}");
    for p in m.mesh.positions() {
        let r = p.length();
        assert!(p.z < 1e-4 || (r - 20.0).abs() < 1e-3, "{p}");
    }
    Ok(())
}

#[test]
fn assemblies_place_components_and_inches_are_kept() -> Result<(), ImportError> {
    let mut w = W::default();
    let inch_mm = w.mm();
    let measure = w.add(&format!("LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),#{inch_mm})"));
    let inch = w.add(&format!("( CONVERSION_BASED_UNIT('INCH',#{measure}) LENGTH_UNIT() NAMED_UNIT(*) )"));
    let solid = w.cuboid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
    let part = w.rep("ADVANCED_BREP_SHAPE_REPRESENTATION", &[solid], inch);
    let origin = w.axis([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    let place_a = w.axis([10.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    let place_b = w.axis([0.0, 0.0, 5.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
    let assembly = w.rep("SHAPE_REPRESENTATION", &[origin, place_a, place_b], inch);
    for place in [place_a, place_b] {
        let t = w.add(&format!("ITEM_DEFINED_TRANSFORMATION('','',#{origin},#{place})"));
        w.add(&format!(
            "( REPRESENTATION_RELATIONSHIP('','',#{part},#{assembly}) REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{t}) SHAPE_REPRESENTATION_RELATIONSHIP() )"
        ));
    }
    let m = read_text(&w.text())?;
    assert_eq!(m.units, Some(Units::Inches));
    assert_eq!(m.mesh.triangles().len(), 24);
    let b = m.mesh.bounds();
    // Component B is turned 90° about Z: x runs from -1 to 0.
    assert!((b.min - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5, "{b:?}");
    assert!((b.max - Vec3::new(11.0, 1.0, 6.0)).length() < 1e-5, "{b:?}");
    let (vol, closed) = volume_closed(&m.mesh);
    assert!(closed && (vol - 2.0).abs() < 1e-5);
    Ok(())
}

#[test]
fn hostile_files_never_panic() {
    let mut w = W::default();
    let solid = w.cylinder(5.0, 10.0, true);
    let mm = w.mm();
    w.rep("ADVANCED_BREP_SHAPE_REPRESENTATION", &[solid], mm);
    let good = w.text();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let swaps = [
        ("5.0", "-5.0"),
        ("5.0", "0.0"),
        ("10.0", "1e308"),
        ("#1,", "#9999,"),
        (".T.", ".F."),
        ("CIRCLE", "ELLIPSE"),
        ("(", "(("),
    ];
    for _ in 0..300 {
        let mut t = good.clone();
        let (from, to) = swaps[(next() as usize) % swaps.len()];
        if let Some(at) = t.match_indices(from).map(|(i, _)| i).nth((next() as usize) % 5) {
            t.replace_range(at..at + from.len(), to);
        }
        let cut = (next() as usize) % (t.len() + 1);
        let _ = read_text(&t);
        let _ = read_text(t.get(..cut).unwrap_or_default());
    }
}
