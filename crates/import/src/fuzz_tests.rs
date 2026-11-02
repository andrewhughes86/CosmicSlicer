//! Every reader fed damaged copies of a valid file of its format: each must return (a model
//! or an error), never panic. See `fuzz` for running it longer.

use crate::fuzz;
use crate::{ModelFormat, Options};

fn stl_binary() -> Vec<u8> {
    let mut b = vec![0u8; 80];
    b.extend_from_slice(&2u32.to_le_bytes());
    for tri in
        [[[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], [[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]]]
    {
        b.extend_from_slice(&[0u8; 12]);
        for v in tri {
            for c in v {
                b.extend_from_slice(&c.to_le_bytes());
            }
        }
        b.extend_from_slice(&[0, 0]);
    }
    b
}

fn three_mf() -> Vec<u8> {
    let model = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources><object id="1" type="model"><mesh>
  <vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/><vertex x="0" y="0" z="10"/></vertices>
  <triangles><triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="0" v2="3" v3="2"/><triangle v1="1" v2="2" v3="3"/></triangles>
 </mesh></object></resources>
 <build><item objectid="1" transform="1 0 0 0 1 0 0 0 1 5 5 0"/></build>
</model>"#;
    crate::zip::write(&[("3D/3dmodel.model".into(), model.as_bytes().to_vec())]).unwrap_or_default()
}

/// A valid file of each format.
fn samples() -> Vec<(ModelFormat, Vec<u8>)> {
    let ascii_stl = "solid a\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid a\n";
    let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nvn 0 0 1\nvt 0 0\nf 1/1/1 2/1/1 3/1/1\nf 1 2 4\ng part\nusemtl x\nf -1 -2 -3\n";
    let ply = "ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n0 1 0\n3 0 1 2\n";
    let off = "OFF\n4 2 0\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n3 0 1 2\n4 0 1 3 2\n";
    let amf = r#"<?xml version="1.0"?><amf unit="inch"><object id="0"><mesh><vertices><vertex><coordinates><x>0</x><y>0</y><z>0</z></coordinates></vertex><vertex><coordinates><x>1</x><y>0</y><z>0</z></coordinates></vertex><vertex><coordinates><x>0</x><y>1</y><z>0</z></coordinates></vertex></vertices><volume><triangle><v1>0</v1><v2>1</v2><v3>2</v3></triangle></volume></mesh></object><constellation id="1"><instance objectid="0"><deltax>5</deltax><rz>30</rz></instance></constellation></amf>"#;
    let gltf = r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":36,"uri":"data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA"}],"bufferViews":[{"buffer":0,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],"nodes":[{"mesh":0,"translation":[1,2,3],"children":[]}],"scenes":[{"nodes":[0]}]}"#;
    let collada = r##"<?xml version="1.0"?><COLLADA><asset><unit meter="0.001"/><up_axis>Z_UP</up_axis></asset><library_geometries><geometry id="g"><mesh><source id="p"><float_array id="pa" count="9">0 0 0 1 0 0 0 1 0</float_array><technique_common><accessor source="#pa" count="3" stride="3"/></technique_common></source><vertices id="v"><input semantic="POSITION" source="#p"/></vertices><triangles count="1"><input semantic="VERTEX" source="#v" offset="0"/><p>0 1 2</p></triangles></mesh></geometry></library_geometries><library_visual_scenes><visual_scene id="s"><node><matrix>1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1</matrix><instance_geometry url="#g"/></node></visual_scene></library_visual_scenes><scene><instance_visual_scene url="#s"/></scene></COLLADA>"##;
    let vrml = "#VRML V2.0 utf8\nDEF A Transform { translation 1 0 0 children [ Shape { geometry IndexedFaceSet { coord Coordinate { point [ 0 0 0, 1 0 0, 0 1 0 ] } coordIndex [ 0 1 2 -1 ] } } ] }\nUSE A\n";
    let x3d = r#"<?xml version="1.0"?><X3D><Scene><Transform translation="1 0 0" scale="2 2 2"><Shape><IndexedFaceSet coordIndex="0 1 2 -1"><Coordinate point="0 0 0 1 0 0 0 1 0"/></IndexedFaceSet></Shape></Transform></Scene></X3D>"#;
    let dxf = "0\nSECTION\n2\nENTITIES\n0\n3DFACE\n8\n0\n10\n0\n20\n0\n30\n0\n11\n1\n21\n0\n31\n0\n12\n0\n22\n1\n32\n0\n13\n0\n23\n1\n33\n0\n0\nENDSEC\n0\nEOF\n";
    // 3DS: main chunk, editor, object "a", a triangle mesh with three points and one face.
    let mut ds = Vec::new();
    let points: Vec<u8> =
        [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut verts = 3u16.to_le_bytes().to_vec();
    verts.extend(points);
    let mut faces = 1u16.to_le_bytes().to_vec();
    for v in [0u16, 1, 2, 0] {
        faces.extend(v.to_le_bytes());
    }
    let chunk = |id: u16, body: &[u8]| {
        let mut c = id.to_le_bytes().to_vec();
        c.extend(((body.len() + 6) as u32).to_le_bytes());
        c.extend_from_slice(body);
        c
    };
    let mesh: Vec<u8> = [chunk(0x4110, &verts), chunk(0x4120, &faces)].concat();
    let mut obj_body = b"a\0".to_vec();
    obj_body.extend(chunk(0x4100, &mesh));
    ds.extend(chunk(0x4D4D, &chunk(0x3D3D, &chunk(0x4000, &obj_body))));
    let mut step = crate::step::tests::W::default();
    let solid = step.cuboid([0.0, 0.0, 0.0], [10.0, 20.0, 5.0]);
    let mm = step.mm();
    step.rep("ADVANCED_BREP_SHAPE_REPRESENTATION", &[solid], mm);
    let mut iges = crate::iges::tests::W::new();
    let plane = iges.add(108, "0.,0.,1.,0.,0,0.,0.,0.,0.", 0, 0, true);
    let arc = iges.add(100, "0.,5.,5.,7.,5.,7.,5.", 0, 0, true);
    let outer = iges.add(142, &format!("1,{plane},0,{arc},2"), 0, 0, true);
    iges.add(144, &format!("{plane},1,0,{outer}"), 0, 0, false);
    vec![
        (ModelFormat::Stl, ascii_stl.as_bytes().to_vec()),
        (ModelFormat::Stl, stl_binary()),
        (ModelFormat::Obj, obj.as_bytes().to_vec()),
        (ModelFormat::Ply, ply.as_bytes().to_vec()),
        (ModelFormat::Off, off.as_bytes().to_vec()),
        (ModelFormat::ThreeMf, three_mf()),
        (ModelFormat::Amf, amf.as_bytes().to_vec()),
        (ModelFormat::Gltf, gltf.as_bytes().to_vec()),
        (ModelFormat::Collada, collada.as_bytes().to_vec()),
        (ModelFormat::Vrml, vrml.as_bytes().to_vec()),
        (ModelFormat::X3d, x3d.as_bytes().to_vec()),
        (ModelFormat::Dxf, dxf.as_bytes().to_vec()),
        (ModelFormat::ThreeDs, ds),
        (ModelFormat::Step, step.text().into_bytes()),
        (ModelFormat::Iges, iges.text().into_bytes()),
        (ModelFormat::Sat, crate::sat::tests::CYLINDER.as_bytes().to_vec()),
    ]
}

#[test]
fn the_samples_read() {
    for (format, bytes) in samples() {
        let r = crate::read_model(&bytes, format, Options::default(), &|_| None);
        assert!(r.is_ok(), "{format:?}: {:?}", r.err());
    }
}

#[test]
fn every_reader_survives_damaged_files() {
    let mut failures = Vec::new();
    for (format, bytes) in samples() {
        let name = format!("{format:?}");
        failures.extend(fuzz::run(&name, &[bytes], &|b| {
            let _ = crate::read_model(b, format, Options::default(), &|_| None);
        }));
    }
    // The zip and gzip layers on their own.
    failures.extend(fuzz::run("zip", &[three_mf()], &|b| {
        if let Ok(z) = crate::zip::Zip::open(b) {
            for n in z.names().map(str::to_owned).collect::<Vec<_>>() {
                let _ = z.read(&n);
            }
        }
        let _ = crate::zip::gunzip(b);
    }));
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Replays one saved case: `COSMICSLICER_FUZZ_REPLAY=<tmp>/cosmicslicer-fuzz/Sat-0-case139-….bin`
/// (the file name starts with the format). Does nothing without it.
#[test]
fn replay() {
    let Some(path) = std::env::var_os("COSMICSLICER_FUZZ_REPLAY") else { return };
    let path = std::path::PathBuf::from(path);
    let bytes = std::fs::read(&path).unwrap_or_default();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let format = samples().into_iter().map(|(f, _)| f).find(|f| name.starts_with(&format!("{f:?}-")));
    let start = std::time::Instant::now();
    match format {
        Some(format) => {
            let r = crate::read_model(&bytes, format, Options::default(), &|_| None);
            let r = r.map(|m| format!("{} triangles, {:?}", m.mesh.triangles().len(), m.warnings));
            eprintln!("{format:?}: {r:?} in {:?}", start.elapsed());
        }
        None => eprintln!("no format for {name}"),
    }
}
