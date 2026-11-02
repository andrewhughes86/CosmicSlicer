//! Model import, from CosmicDMS (the Cosmic Design and Manufacturing Suite).
//!
//! Model import reads faceted geometry (STL, OBJ, PLY, OFF, 3MF, AMF, glTF, COLLADA, VRML,
//! X3D, 3DS, DXF) and exact CAD geometry (STEP, IGES, ACIS SAT), which is tessellated to the asked
//! tolerance, into a [`Mesh`]. Every reader treats its input as hostile: sizes and counts are
//! capped, numbers must be finite and indices are range-checked, so a bad file is an
//! [`ImportError`], never a panic.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
#![allow(non_snake_case)]

pub mod amf;
pub mod collada;
pub mod dxf;
pub mod geom;
pub mod gltf;
pub mod iges;
pub mod obj;
pub mod off;
pub mod ply;
pub mod sat;
pub mod step;
pub mod stl;
pub mod threeds;
pub mod threemf;
pub mod vrml;
pub mod x3d;

mod brep;
mod soup;
pub mod xml;
pub mod zip;

use std::io::Read as _;
use std::path::Path;

use crate::geom::{Mesh, MeshError};
use thiserror::Error;

pub(crate) use soup::Soup;

/// Largest model file read.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Largest file unpacked from a compressed one (gzip, zip).
pub const MAX_UNPACKED_BYTES: usize = 1024 * 1024 * 1024;
/// Most triangles in one imported model.
pub const MAX_TRIANGLES: usize = 50_000_000;
/// Default chord tolerance for tessellating curved CAD surfaces, mm.
pub const DEFAULT_TOLERANCE: f64 = 0.01;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ImportError {
    #[error("unsupported file type `{0}`; CosmicSlicer imports {list}", list = ModelFormat::extension_list())]
    UnsupportedFormat(String),
    #[error("could not read {path}: {message}")]
    Io { path: String, message: String },
    #[error("file is larger than the {} MiB limit", MAX_FILE_BYTES / (1024 * 1024))]
    TooLarge,
    #[error("model has more than {MAX_TRIANGLES} triangles")]
    TooManyTriangles,
    #[error("{format} line {line}: {message}")]
    Syntax { format: &'static str, line: usize, message: String },
    #[error("{0}")]
    Malformed(String),
    #[error("not supported yet: {0}")]
    Unsupported(String),
    #[error("the file contains no triangles")]
    Empty,
}

impl From<MeshError> for ImportError {
    fn from(e: MeshError) -> Self {
        Self::Malformed(e.to_string())
    }
}

/// Model file formats CosmicSlicer reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelFormat {
    Stl,
    Obj,
    Ply,
    Off,
    ThreeMf,
    Amf,
    Gltf,
    Collada,
    Vrml,
    X3d,
    ThreeDs,
    Dxf,
    Step,
    Iges,
    Sat,
}

impl ModelFormat {
    pub const ALL: [Self; 15] = [
        Self::Step,
        Self::Iges,
        Self::Sat,
        Self::Stl,
        Self::Obj,
        Self::ThreeMf,
        Self::Ply,
        Self::Off,
        Self::Amf,
        Self::Gltf,
        Self::Collada,
        Self::Vrml,
        Self::X3d,
        Self::ThreeDs,
        Self::Dxf,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Stl => "stl",
            Self::Obj => "obj",
            Self::Ply => "ply",
            Self::Off => "off",
            Self::ThreeMf => "3mf",
            Self::Amf => "amf",
            Self::Gltf => "gltf",
            Self::Collada => "dae",
            Self::Vrml => "wrl",
            Self::X3d => "x3d",
            Self::ThreeDs => "3ds",
            Self::Dxf => "dxf",
            Self::Step => "step",
            Self::Iges => "iges",
            Self::Sat => "sat",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stl => "STL",
            Self::Obj => "Wavefront OBJ",
            Self::Ply => "PLY",
            Self::Off => "OFF",
            Self::ThreeMf => "3MF",
            Self::Amf => "AMF",
            Self::Gltf => "glTF",
            Self::Collada => "COLLADA",
            Self::Vrml => "VRML",
            Self::X3d => "X3D",
            Self::ThreeDs => "3DS",
            Self::Dxf => "DXF",
            Self::Step => "STEP",
            Self::Iges => "IGES",
            Self::Sat => "ACIS SAT",
        }
    }

    /// File extensions, lower case, without the dot. Any of them may also end in `.gz`.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Stl => &["stl"],
            Self::Obj => &["obj"],
            Self::Ply => &["ply"],
            Self::Off => &["off"],
            Self::ThreeMf => &["3mf"],
            Self::Amf => &["amf"],
            Self::Gltf => &["gltf", "glb"],
            Self::Collada => &["dae"],
            Self::Vrml => &["wrl", "vrml", "wrz"],
            Self::X3d => &["x3d", "x3dz"],
            Self::ThreeDs => &["3ds"],
            Self::Dxf => &["dxf"],
            Self::Step => &["step", "stp", "p21", "stpz"],
            Self::Iges => &["iges", "igs"],
            Self::Sat => &["sat"],
        }
    }

    /// Exact CAD geometry, tessellated on import, rather than triangles.
    pub fn is_cad(self) -> bool {
        matches!(self, Self::Step | Self::Iges | Self::Sat)
    }

    /// Every extension, as `.a, .b, …` for messages.
    pub fn extension_list() -> String {
        Self::ALL.iter().flat_map(|f| f.extensions()).map(|e| format!(".{e}")).collect::<Vec<_>>().join(", ")
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }

    /// The format a file name's extension names, ignoring case and a trailing `.gz`.
    pub fn from_path(path: &Path) -> Result<Self, ImportError> {
        let ext_of = |p: &Path| p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let mut ext = ext_of(path);
        if ext == "gz" {
            ext = path.file_stem().map(|s| ext_of(Path::new(s))).unwrap_or_default();
        }
        Self::ALL
            .into_iter()
            .find(|f| f.extensions().contains(&ext.as_str()))
            .ok_or_else(|| ImportError::UnsupportedFormat(if ext.is_empty() { "(none)".into() } else { ext }))
    }
}

/// Unit a model file was authored in. Mesh formats mostly carry no units, so the caller says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Millimetres,
    Centimetres,
    Metres,
    Inches,
}

impl Units {
    pub const IDS: &'static [&'static str] = &["mm", "cm", "m", "in"];

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "mm" => Self::Millimetres,
            "cm" => Self::Centimetres,
            "m" => Self::Metres,
            "in" => Self::Inches,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Millimetres => "mm",
            Self::Centimetres => "cm",
            Self::Metres => "m",
            Self::Inches => "in",
        }
    }

    /// Millimetres per unit.
    pub fn to_mm(self) -> f32 {
        match self {
            Self::Millimetres => 1.0,
            Self::Centimetres => 10.0,
            Self::Metres => 1000.0,
            Self::Inches => 25.4,
        }
    }

    /// The unit `mm` millimetres long, when it is one of ours.
    pub fn from_mm(mm: f64) -> Option<Self> {
        [Self::Millimetres, Self::Centimetres, Self::Metres, Self::Inches]
            .into_iter()
            .find(|u| (f64::from(u.to_mm()) - mm).abs() <= mm * 1.0e-6)
    }
}

/// How a model is read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Most a tessellated CAD surface may stray from the exact one, mm.
    pub tolerance: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self { tolerance: DEFAULT_TOLERANCE }
    }
}

/// A model read from a file.
#[derive(Clone, Debug)]
pub struct Imported {
    /// In the file's units; [`import_mesh`] scales it to millimetres.
    pub mesh: Mesh,
    /// The unit the file says it uses, when it says. Units the enum lacks are scaled to
    /// millimetres by the reader, which then reports millimetres.
    pub units: Option<Units>,
    /// What was skipped or approximated, for the user.
    pub warnings: Vec<String>,
}

impl Imported {
    pub(crate) fn new(mesh: Mesh) -> Self {
        Self { mesh, units: None, warnings: Vec::new() }
    }
}

/// Fetches a file the model refers to (a glTF buffer, say) by its relative name.
pub type Sibling<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;

/// Parses a model file's bytes. `sibling` fetches files the model refers to.
pub fn read_model(
    bytes: &[u8],
    format: ModelFormat,
    options: Options,
    sibling: Sibling<'_>,
) -> Result<Imported, ImportError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge);
    }
    let unpacked;
    let bytes = if format != ModelFormat::ThreeMf && zip::is_gzip(bytes) {
        unpacked = zip::gunzip(bytes)?;
        unpacked.as_slice()
    } else {
        bytes
    };
    let imported = match format {
        ModelFormat::Stl => Imported::new(stl::read(bytes)?),
        ModelFormat::Obj => Imported::new(obj::read(bytes)?),
        ModelFormat::Ply => Imported::new(ply::read(bytes)?),
        ModelFormat::Off => Imported::new(off::read(bytes)?),
        ModelFormat::ThreeMf => threemf::read(bytes)?,
        ModelFormat::Amf => amf::read(bytes)?,
        ModelFormat::Gltf => gltf::read(bytes, sibling)?,
        ModelFormat::Collada => collada::read(bytes)?,
        ModelFormat::Vrml => vrml::read(bytes)?,
        ModelFormat::X3d => x3d::read(bytes)?,
        ModelFormat::ThreeDs => Imported::new(threeds::read(bytes)?),
        ModelFormat::Dxf => dxf::read(bytes)?,
        ModelFormat::Step => step::read(bytes, options)?,
        ModelFormat::Iges => iges::read(bytes, options)?,
        ModelFormat::Sat => sat::read(bytes, options)?,
    };
    if imported.mesh.is_empty() {
        return Err(ImportError::Empty);
    }
    Ok(imported)
}

/// Parses a model file's bytes, without files it refers to.
pub fn read_mesh(bytes: &[u8], format: ModelFormat) -> Result<Mesh, ImportError> {
    read_model(bytes, format, Options::default(), &|_| None).map(|m| m.mesh)
}

/// Scales `imported` to millimetres: by `units` when given, else by the file's own unit.
fn to_millimetres(mut imported: Imported, units: Option<Units>) -> Result<Imported, ImportError> {
    let unit = units.or(imported.units).unwrap_or(Units::Millimetres);
    if unit != Units::Millimetres {
        imported.mesh = imported.mesh.scaled(unit.to_mm())?;
    }
    imported.units = Some(unit);
    Ok(imported)
}

/// Reads a whole file of at most [`MAX_FILE_BYTES`].
fn read_capped(path: &Path) -> Result<Vec<u8>, ImportError> {
    let io = |e: std::io::Error| ImportError::Io { path: path.display().to_string(), message: e.to_string() };
    let meta = std::fs::metadata(path).map_err(io)?;
    if !meta.is_file() {
        return Err(ImportError::Io { path: path.display().to_string(), message: "not a regular file".into() });
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge);
    }
    // Read through a cap in case the file grows while it is read.
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path).and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)).map_err(io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge);
    }
    Ok(bytes)
}

/// Reads and parses a model file, choosing the format by extension, and scales it to
/// millimetres (`units` overrides the unit the file states; mm when it states none).
/// Files the model refers to are looked up next to it, never outside its folder.
pub fn import_mesh(
    path: &Path,
    units: Option<Units>,
    options: Options,
) -> Result<(Imported, ModelFormat), ImportError> {
    let format = ModelFormat::from_path(path)?;
    let bytes = read_capped(path)?;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let sibling = move |name: &str| -> Option<Vec<u8>> {
        let rel = Path::new(name);
        let plain = rel.components().all(|c| matches!(c, std::path::Component::Normal(_)));
        if !plain || name.is_empty() {
            return None;
        }
        read_capped(&dir.join(rel)).ok()
    };
    let imported = read_model(&bytes, format, options, &sibling)?;
    Ok((to_millimetres(imported, units)?, format))
}

/// Parses a model file's bytes, choosing the format by `name`'s extension. For files that
/// arrive without a path (a browser upload); files the model refers to are not available.
pub fn import_mesh_bytes(
    name: &str,
    bytes: &[u8],
    units: Option<Units>,
    options: Options,
) -> Result<(Imported, ModelFormat), ImportError> {
    let format = ModelFormat::from_path(Path::new(name))?;
    let imported = read_model(bytes, format, options, &|_| None)?;
    Ok((to_millimetres(imported, units)?, format))
}

/// Parses an ASCII float, accepting only finite values.
pub(crate) fn finite(token: &str) -> Option<f32> {
    token.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Parses an ASCII double, accepting only finite values.
pub(crate) fn finite64(token: &str) -> Option<f64> {
    token.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_comes_from_the_extension() {
        assert_eq!(ModelFormat::from_path(Path::new("a/b/Part.STL")), Ok(ModelFormat::Stl));
        assert_eq!(ModelFormat::from_path(Path::new("part.obj")), Ok(ModelFormat::Obj));
        assert_eq!(ModelFormat::from_path(Path::new("Part.STP")), Ok(ModelFormat::Step));
        assert_eq!(ModelFormat::from_path(Path::new("part.igs")), Ok(ModelFormat::Iges));
        assert_eq!(ModelFormat::from_path(Path::new("Part.SAT")), Ok(ModelFormat::Sat));
        assert_eq!(ModelFormat::from_path(Path::new("part.ply.gz")), Ok(ModelFormat::Ply));
        assert_eq!(ModelFormat::from_path(Path::new("scene.glb")), Ok(ModelFormat::Gltf));
        assert!(
            matches!(ModelFormat::from_path(Path::new("part.xyz")), Err(ImportError::UnsupportedFormat(e)) if e == "xyz")
        );
        assert!(matches!(ModelFormat::from_path(Path::new("part")), Err(ImportError::UnsupportedFormat(_))));
    }

    #[test]
    fn empty_files_are_refused() {
        for f in ModelFormat::ALL {
            assert!(read_mesh(b"", f).is_err(), "{f:?}");
        }
        assert_eq!(read_mesh(b"solid x\nendsolid x\n", ModelFormat::Stl), Err(ImportError::Empty));
        assert_eq!(read_mesh(b"# nothing\nv 0 0 0\n", ModelFormat::Obj), Err(ImportError::Empty));
    }

    #[test]
    fn units_scale_to_millimetres() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("cosmicdms-io-units-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("tri.obj");
        std::fs::write(&path, "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n")?;
        let (model, format) = import_mesh(&path, Some(Units::Inches), Options::default())?;
        std::fs::remove_dir_all(&dir)?;
        assert_eq!(format, ModelFormat::Obj);
        assert_eq!(model.mesh.bounds().max.x, 25.4);
        assert_eq!(model.units, Some(Units::Inches));
        Ok(())
    }

    #[test]
    fn missing_files_and_directories_are_io_errors() {
        assert!(matches!(
            import_mesh(Path::new("/no/such/dir/part.stl"), None, Options::default()),
            Err(ImportError::Io { .. })
        ));
        let dir = std::env::temp_dir().join(format!("cosmicdms-io-dir-{}.stl", std::process::id()));
        let made = std::fs::create_dir_all(&dir).is_ok();
        let r = import_mesh(&dir, None, Options::default());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !made || matches!(r, Err(ImportError::Io { ref message, .. }) if message == "not a regular file"),
            "{r:?}"
        );
    }

    /// Random and mangled bytes must come back as errors or meshes, never panics.
    #[test]
    fn garbage_never_panics() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let samples: [&[u8]; 6] = [
            b"solid a\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid a\n",
            b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nf 1 2 4 3\nf -1 -2 -3\n",
            b"f 1 2 3\nv nan 0 0\nv 1e39 0 0\nf 0 0 0\nf 99999999999 1 2\n",
            b"solid\nvertex 1 2\nfacet\nendloop\n",
            &[0u8; 84],
            &[0xffu8; 200],
        ];
        for sample in samples {
            for f in ModelFormat::ALL {
                let _ = read_mesh(sample, f);
                for _ in 0..200 {
                    let mut bytes = sample.to_vec();
                    for _ in 0..4 {
                        let len = bytes.len().max(1);
                        let i = (next() as usize) % len;
                        if let Some(b) = bytes.get_mut(i) {
                            *b = next() as u8;
                        }
                    }
                    bytes.truncate((next() as usize) % (bytes.len() + 1));
                    let _ = read_mesh(&bytes, f);
                }
            }
        }
    }
}
