//! Where CosmicSlicer keeps its files, and writing them so a crash or power cut mid-write
//! never leaves a damaged file: write a temporary file, flush it to disk, then rename it over
//! the old one.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Overrides the data folder (tests and scripted runs).
pub const DATA_DIR_ENV: &str = "COSMICSLICER_DATA_DIR";

/// The folder for settings, presets, autosaves and crash reports:
/// `~/.local/share/cosmicslicer`, `~/Library/Application Support/CosmicSlicer` or
/// `%APPDATA%\CosmicSlicer\data`.
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var_os(DATA_DIR_ENV).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => Some(p),
        None => eframe::storage_dir(crate::APP_ID),
    }
}

/// Writes `bytes` to `path` so readers see the old file or the new one, never half of one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Reads a JSON file; a damaged one is set aside as `<name>.damaged` and `None` comes back, so
/// the program starts with defaults instead of failing.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            let aside = path.with_extension("json.damaged");
            let _ = std::fs::rename(path, &aside);
            Err(format!(
                "{} was damaged ({e}); it was set aside as {} and defaults are used",
                path.display(),
                aside.display()
            ))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh folder under the system temp folder, removed when dropped.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> TempDir {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!("cosmicslicer-test-{name}-{}-{n}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_writes_replace_and_leave_no_temp_files() {
        let dir = TempDir::new("atomic");
        let path = dir.0.join("sub").join("settings.json");
        assert!(write_atomic(&path, b"{\"a\":1}").is_ok());
        assert!(write_atomic(&path, b"{\"a\":2}").is_ok());
        assert_eq!(std::fs::read(&path).ok().as_deref(), Some(&b"{\"a\":2}"[..]));
        let leftovers = std::fs::read_dir(dir.0.join("sub")).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn damaged_json_is_set_aside() {
        let dir = TempDir::new("damaged");
        let path = dir.0.join("prefs.json");
        let _ = std::fs::write(&path, b"{not json");
        let r: Result<Option<serde_json::Value>, String> = read_json(&path);
        assert!(r.is_err());
        assert!(!path.exists());
        assert!(dir.0.join("prefs.json.damaged").exists());
        let missing: Result<Option<serde_json::Value>, String> = read_json(&dir.0.join("none.json"));
        assert!(matches!(missing, Ok(None)));
    }
}
