//! Embeds every icon under `assets/icons/` (OrcaSlicer's artwork in `orca/`, CosmicSlicer's
//! own in `cosmic/`) into the binary.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
    let mut names: Vec<(String, String)> = Vec::new();
    for sub in ["orca", "cosmic"] {
        let dir = root.join(sub);
        println!("cargo:rerun-if-changed={}", dir.display());
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().is_none_or(|e| e != "svg") {
                continue;
            }
            let (Some(name), Some(abs)) = (
                path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
                path.canonicalize().ok().and_then(|p| p.to_str().map(str::to_owned)),
            ) else {
                continue;
            };
            // CosmicSlicer's own icons replace OrcaSlicer's of the same name.
            names.retain(|(n, _)| *n != name);
            names.push((name, abs));
        }
    }
    names.sort();
    let mut out = String::from(
        "/// (name, SVG source) for every icon, sorted by name.\npub static ICONS: &[(&str, &str)] = &[\n",
    );
    for (name, abs) in &names {
        println!("cargo:rerun-if-changed={abs}");
        let _ = writeln!(out, "    ({name:?}, include_str!({abs:?})),");
    }
    out.push_str("];\n");
    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into());
    if let Err(e) = std::fs::write(Path::new(&out_dir).join("icons.rs"), out) {
        println!("cargo:warning=could not write icons.rs: {e}");
    }
}
