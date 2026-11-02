//! IGES reading, on small files written here.

use super::*;

/// Writes the fixed-column sections.
struct W {
    d: Vec<String>,
    p: Vec<String>,
    units: (i32, &'static str),
}

impl W {
    fn new() -> Self {
        Self { d: Vec::new(), p: Vec::new(), units: (2, "2HMM") }
    }

    /// Adds an entity; `dependent` marks it physically dependent. Returns its DE number.
    fn add(&mut self, kind: i32, params: &str, form: i32, transform: usize, dependent: bool) -> usize {
        let de = self.d.len() + 1;
        let first = self.p.len() + 1;
        let text = format!("{kind},{params};");
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(64) {
            let s: String = chunk.iter().collect();
            let n = self.p.len() + 1;
            self.p.push(format!("{s:<64} {de:>7}P{n:>7}"));
        }
        let lines = self.p.len() + 1 - first;
        let status = if dependent { "00010000" } else { "00000000" };
        let n = self.d.len() + 1;
        self.d.push(format!(
            "{kind:>8}{first:>8}{:>8}{:>8}{:>8}{:>8}{transform:>8}{:>8}{status:>8}D{n:>7}",
            0, 0, 0, 0, 0
        ));
        self.d.push(format!(
            "{kind:>8}{:>8}{:>8}{lines:>8}{form:>8}{:>8}{:>8}{:>8}{:>8}D{:>7}",
            0,
            0,
            "",
            "",
            "",
            0,
            n + 1
        ));
        de
    }

    fn text(&self) -> String {
        let g = format!(
            "1H,,1H;,4Htest,8Htest.igs,4Htest,3H1.0,32,38,6,308,15,4Htest,1.,{},{},1,1.,15H20261025.120000,0.001,1000.,4Hauth,3Horg,11,0;",
            self.units.0, self.units.1
        );
        let mut out = format!("{:<72}S{:>7}\n", "CosmicDMS test", 1);
        for (i, c) in g.chars().collect::<Vec<_>>().chunks(72).enumerate() {
            out.push_str(&format!("{:<72}G{:>7}\n", c.iter().collect::<String>(), i + 1));
        }
        for (i, l) in self.d.iter().enumerate() {
            let _ = i;
            out.push_str(l);
            out.push('\n');
        }
        for l in &self.p {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&format!(
            "{:<72}T{:>7}\n",
            format!("S      1G      1D{:>7}P{:>7}", self.d.len(), self.p.len()),
            1
        ));
        out
    }
}

fn area(m: &crate::geom::Mesh) -> f64 {
    m.triangles()
        .iter()
        .map(|t| {
            let [a, b, c] = m.corners(*t);
            f64::from((b - a).cross(c - a).length()) / 2.0
        })
        .sum()
}

#[test]
fn a_trimmed_plane_with_a_hole() -> Result<(), ImportError> {
    let mut w = W::new();
    let plane = w.add(108, "0.,0.,1.,0.,0,0.,0.,0.,0.", 0, 0, true);
    let corners = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
    let lines: Vec<usize> = (0..4)
        .map(|i| {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            w.add(110, &format!("{:?},{:?},0.,{:?},{:?},0.", a[0], a[1], b[0], b[1]), 0, 0, true)
        })
        .collect();
    let comp = w.add(102, &format!("4,{},{},{},{}", lines[0], lines[1], lines[2], lines[3]), 0, 0, true);
    let outer = w.add(142, &format!("1,{plane},0,{comp},2"), 0, 0, true);
    let arc = w.add(100, "0.,5.,5.,7.,5.,7.,5.", 0, 0, true);
    let hole = w.add(142, &format!("1,{plane},0,{arc},2"), 0, 0, true);
    w.add(144, &format!("{plane},1,1,{outer},{hole}"), 0, 0, false);
    let m = read(w.text().as_bytes(), Options::default())?;
    assert_eq!(m.units, Some(Units::Millimetres));
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let a = area(&m.mesh);
    let expect = 100.0 - std::f64::consts::PI * 4.0;
    // The hole is an inscribed polygon within 0.01 mm of the circle.
    assert!(a > expect && a - expect < 0.01 * 2.0 * std::f64::consts::PI * 2.0, "{a} vs {expect}");
    assert!(m.mesh.positions().iter().all(|p| p.z.abs() < 1e-6));
    Ok(())
}

#[test]
fn an_untrimmed_spline_patch_moved_by_a_matrix_in_inches() -> Result<(), ImportError> {
    let mut w = W::new();
    w.units = (1, "2HIN");
    let shift = w.add(124, "1.,0.,0.,10.,0.,1.,0.,0.,0.,0.,1.,0.", 0, 0, true);
    w.add(
        128,
        "1,1,1,1,0,0,1,0,0,0.,0.,1.,1.,0.,0.,1.,1.,1.,1.,1.,1.,0.,0.,0.,2.,0.,0.,0.,2.,0.,2.,2.,1.,0.,1.,0.,1.",
        0,
        shift,
        false,
    );
    let m = read(w.text().as_bytes(), Options::default())?;
    assert_eq!(m.units, Some(Units::Inches));
    let b = m.mesh.bounds();
    assert!(
        (b.min - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-5 && (b.max - Vec3::new(12.0, 2.0, 1.0)).length() < 1e-5,
        "{b:?}"
    );
    // A saddle: refined beyond its two corner triangles, within the tolerance (in inches).
    assert!(m.mesh.triangles().len() > 2);
    Ok(())
}

#[test]
fn hollerith_strings_keep_their_delimiters() {
    assert_eq!(tokens("3Ha,b,12,1.5D2;", ',', ';'), ["a,b", "12", "1.5D2"]);
    assert_eq!(real(Some(&"1.5D2".to_owned())), Some(150.0));
}

#[test]
fn hostile_files_never_panic() {
    let mut w = W::new();
    let plane = w.add(108, "0.,0.,1.,0.,0,0.,0.,0.,0.", 0, 0, true);
    let arc = w.add(100, "0.,5.,5.,7.,5.,7.,5.", 0, 0, true);
    let outer = w.add(142, &format!("1,{plane},0,{arc},2"), 0, 0, true);
    w.add(144, &format!("{plane},1,0,{outer}"), 0, 0, false);
    w.add(128, "99999999,1,1,1,0,0,1,0,0,0.,0.,1.", 0, 0, false);
    w.add(144, "1,1,5,999,-3", 0, 0, false);
    w.add(408, "9,0.,0.,0.,1.", 0, 0, false);
    let good = w.text();
    assert!(read(good.as_bytes(), Options::default()).is_ok());
    for cut in (0..good.len()).step_by(37) {
        let _ = read(good.get(..cut).unwrap_or_default().as_bytes(), Options::default());
    }
    let _ = read(good.replace("100,", "126,").as_bytes(), Options::default());
}

#[test]
fn faces_without_a_side_are_turned_outward() -> Result<(), ImportError> {
    // A 10 mm cube of six flat spline patches, half of them parameterised inside out.
    let c = |i: usize| {
        [if i & 1 == 0 { 0.0 } else { 10.0 }, if i & 2 == 0 { 0.0 } else { 10.0 }, if i & 4 == 0 { 0.0 } else { 10.0 }]
    };
    // Corners as (u0v0, u1v0, u0v1, u1v1); the first three face outward, the rest inward.
    let faces = [[0, 2, 1, 3], [4, 5, 6, 7], [0, 1, 4, 5], [2, 3, 6, 7], [0, 2, 4, 6], [1, 3, 5, 7]];
    let mut w = W::new();
    for f in faces {
        let pts: Vec<String> = f.iter().flat_map(|&i| c(i)).map(|v| format!("{v:?}")).collect();
        w.add(
            128,
            &format!("1,1,1,1,0,0,1,0,0,0.,0.,1.,1.,0.,0.,1.,1.,1.,1.,1.,1.,{},0.,1.,0.,1.", pts.join(",")),
            0,
            0,
            false,
        );
    }
    let m = read(w.text().as_bytes(), Options::default())?;
    let vol: f64 = m
        .mesh
        .triangles()
        .iter()
        .map(|t| {
            let [a, b, c] = m.mesh.corners(*t).map(|p| DVec3::new(f64::from(p.x), f64::from(p.y), f64::from(p.z)));
            a.dot(b.cross(c)) / 6.0
        })
        .sum();
    assert!((vol - 1000.0).abs() < 1e-3, "{vol}");
    Ok(())
}
