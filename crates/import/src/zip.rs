//! Unpacking: gzip streams and zip archives (3MF, zipped AMF), stored or deflated; and
//! packing zip archives (3MF projects).
//!
//! Sizes and offsets come from the file, so every one is checked against the bytes held and
//! the unpacked size is capped at [`MAX_UNPACKED_BYTES`].

use miniz_oxide::inflate::decompress_to_vec_with_limit;

use crate::{ImportError, MAX_UNPACKED_BYTES};

fn bad(what: &str) -> ImportError {
    ImportError::Malformed(format!("damaged compressed data: {what}"))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn inflate(data: &[u8]) -> Result<Vec<u8>, ImportError> {
    decompress_to_vec_with_limit(data, MAX_UNPACKED_BYTES).map_err(|e| match e.status {
        miniz_oxide::inflate::TINFLStatus::HasMoreOutput => ImportError::TooLarge,
        _ => bad("deflate stream"),
    })
}

pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

/// The contents of a gzip file (its first member).
pub fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, ImportError> {
    if bytes.get(2) != Some(&8) {
        return Err(bad("gzip method"));
    }
    let flags = *bytes.get(3).ok_or_else(|| bad("gzip header"))?;
    let mut at = 10usize;
    if flags & 4 != 0 {
        at += 2 + usize::from(u16_at(bytes, at).ok_or_else(|| bad("gzip extra"))?);
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            let end = bytes.get(at..).and_then(|b| b.iter().position(|&c| c == 0)).ok_or_else(|| bad("gzip name"))?;
            at += end + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    inflate(bytes.get(at..).ok_or_else(|| bad("gzip header"))?)
}

/// One file in an archive.
struct Entry {
    name: String,
    method: u16,
    compressed: u64,
    size: u64,
    local: u64,
}

pub struct Zip<'a> {
    bytes: &'a [u8],
    entries: Vec<Entry>,
}

/// Most files listed in one archive.
const MAX_ENTRIES: usize = 100_000;

impl<'a> Zip<'a> {
    pub fn is_zip(bytes: &[u8]) -> bool {
        bytes.starts_with(b"PK\x03\x04")
    }

    pub fn open(bytes: &'a [u8]) -> Result<Self, ImportError> {
        // The end-of-central-directory record sits in the last 64 KiB + 22 bytes.
        let from = bytes.len().saturating_sub(65_557);
        let eocd = (from..bytes.len().saturating_sub(21))
            .rev()
            .find(|&i| u32_at(bytes, i) == Some(0x0605_4b50))
            .ok_or_else(|| bad("no zip directory"))?;
        let mut count = u64::from(u16_at(bytes, eocd + 10).ok_or_else(|| bad("zip directory"))?);
        let mut offset = u64::from(u32_at(bytes, eocd + 16).ok_or_else(|| bad("zip directory"))?);
        // Zip64: the locator just before points at the real record.
        if (offset == 0xffff_ffff || count == 0xffff)
            && eocd >= 20
            && u32_at(bytes, eocd - 20) == Some(0x0706_4b50)
            && let Some(rec) = u64_at(bytes, eocd - 12).and_then(|r| usize::try_from(r).ok())
            && u32_at(bytes, rec) == Some(0x0606_4b50)
        {
            count = u64_at(bytes, rec + 32).ok_or_else(|| bad("zip64 directory"))?;
            offset = u64_at(bytes, rec + 48).ok_or_else(|| bad("zip64 directory"))?;
        }
        if count as usize > MAX_ENTRIES {
            return Err(bad("too many files in the archive"));
        }
        let mut at = usize::try_from(offset).map_err(|_| bad("zip directory offset"))?;
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            if u32_at(bytes, at) != Some(0x0201_4b50) {
                return Err(bad("zip directory entry"));
            }
            let field = |o: usize| u32_at(bytes, at + o).ok_or_else(|| bad("zip directory entry"));
            let half = |o: usize| u16_at(bytes, at + o).map(usize::from).ok_or_else(|| bad("zip directory entry"));
            let flags = half(8)?;
            let method = half(10)? as u16;
            let (mut compressed, mut size, mut local) =
                (u64::from(field(20)?), u64::from(field(24)?), u64::from(field(42)?));
            let (name_len, extra_len, comment_len) = (half(28)?, half(30)?, half(32)?);
            let name_bytes = bytes.get(at + 46..at + 46 + name_len).ok_or_else(|| bad("zip file name"))?;
            let extra = bytes.get(at + 46 + name_len..at + 46 + name_len + extra_len).unwrap_or_default();
            // Zip64 extra field: the 64-bit values for the fields saturated above, in order.
            let mut e = 0usize;
            while e + 4 <= extra.len() {
                let (id, len) = (u16_at(extra, e).unwrap_or(0), usize::from(u16_at(extra, e + 2).unwrap_or(0)));
                if id == 1 {
                    let mut v = e + 4;
                    for slot in [&mut size, &mut compressed, &mut local] {
                        if *slot == 0xffff_ffff {
                            *slot = u64_at(extra, v).ok_or_else(|| bad("zip64 field"))?;
                            v += 8;
                        }
                    }
                }
                e += 4 + len;
            }
            if flags & 1 == 0 {
                entries.push(Entry {
                    name: String::from_utf8_lossy(name_bytes).into_owned(),
                    method,
                    compressed,
                    size,
                    local,
                });
            }
            at += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { bytes, entries })
    }

    /// Names of the files held.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// A file's contents, found by name ignoring case and a leading `/`.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, ImportError> {
        let want = name.trim_start_matches('/');
        let entry = self
            .entries
            .iter()
            .find(|e| e.name.trim_start_matches('/').eq_ignore_ascii_case(want))
            .ok_or_else(|| ImportError::Malformed(format!("the archive has no `{want}`")))?;
        if entry.size > MAX_UNPACKED_BYTES as u64 {
            return Err(ImportError::TooLarge);
        }
        let at = usize::try_from(entry.local).map_err(|_| bad("zip entry offset"))?;
        if u32_at(self.bytes, at) != Some(0x0403_4b50) {
            return Err(bad("zip entry header"));
        }
        let name_len = usize::from(u16_at(self.bytes, at + 26).ok_or_else(|| bad("zip entry header"))?);
        let extra_len = usize::from(u16_at(self.bytes, at + 28).ok_or_else(|| bad("zip entry header"))?);
        let start = at + 30 + name_len + extra_len;
        let len = usize::try_from(entry.compressed).map_err(|_| bad("zip entry size"))?;
        let data =
            self.bytes.get(start..start.saturating_add(len)).ok_or_else(|| bad("zip entry runs past the end"))?;
        match entry.method {
            0 => Ok(data.to_vec()),
            8 => inflate(data),
            m => Err(ImportError::Unsupported(format!("zip compression method {m}"))),
        }
    }
}

/// CRC-32 (IEEE), as zip entries carry it.
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    let mut crc = !0u32;
    for b in data {
        let k = ((crc ^ u32::from(*b)) & 0xff) as usize;
        crc = table.get(k).copied().unwrap_or(0) ^ (crc >> 8);
    }
    !crc
}

/// A zip archive of `files` (name, contents), deflated where that is smaller. Archives too
/// big for plain zip (4 GiB, 65 535 files) are refused.
pub fn write(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, ImportError> {
    let mut out = Vec::new();
    let mut dir = Vec::new();
    let too_big = || ImportError::Unsupported("archives over 4 GiB".into());
    let count = u16::try_from(files.len()).map_err(|_| ImportError::Unsupported("over 65 535 files".into()))?;
    for (name, data) in files {
        let crc = crc32(data);
        let packed = miniz_oxide::deflate::compress_to_vec(data, 6);
        let (method, body): (u16, &[u8]) = if packed.len() < data.len() { (8, &packed) } else { (0, data) };
        let local = u32::try_from(out.len()).map_err(|_| too_big())?;
        let (csize, usize_) =
            (u32::try_from(body.len()).map_err(|_| too_big())?, u32::try_from(data.len()).map_err(|_| too_big())?);
        let name_len = u16::try_from(name.len()).map_err(|_| ImportError::Unsupported("long file names".into()))?;
        // Local header: version 2.0, UTF-8 names, method, no time, crc and sizes.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&csize.to_le_bytes());
        out.extend_from_slice(&usize_.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(body);
        dir.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        dir.extend_from_slice(&20u16.to_le_bytes());
        dir.extend_from_slice(&20u16.to_le_bytes());
        dir.extend_from_slice(&0x0800u16.to_le_bytes());
        dir.extend_from_slice(&method.to_le_bytes());
        dir.extend_from_slice(&[0, 0, 0x21, 0]);
        dir.extend_from_slice(&crc.to_le_bytes());
        dir.extend_from_slice(&csize.to_le_bytes());
        dir.extend_from_slice(&usize_.to_le_bytes());
        dir.extend_from_slice(&name_len.to_le_bytes());
        dir.extend_from_slice(&[0; 12]);
        dir.extend_from_slice(&local.to_le_bytes());
        dir.extend_from_slice(name.as_bytes());
    }
    let offset = u32::try_from(out.len()).map_err(|_| too_big())?;
    let dir_len = u32::try_from(dir.len()).map_err(|_| too_big())?;
    out.extend_from_slice(&dir);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&dir_len.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A zip archive holding `files` stored (method 0), for tests.
    pub fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (name, data) in files {
            let local = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            dir.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            dir.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
            dir.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            dir.extend_from_slice(&local.to_le_bytes());
            dir.extend_from_slice(name.as_bytes());
        }
        let offset = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn stored_and_deflated_entries_read_back() -> Result<(), ImportError> {
        let z = stored_zip(&[("a/b.txt", b"hello"), ("c.bin", b"")]);
        let zip = Zip::open(&z)?;
        assert_eq!(zip.names().collect::<Vec<_>>(), ["a/b.txt", "c.bin"]);
        assert_eq!(zip.read("/A/B.TXT")?, b"hello");
        assert!(zip.read("missing").is_err());
        let deflated = miniz_oxide::deflate::compress_to_vec(b"abcabcabc", 6);
        assert_eq!(inflate(&deflated)?, b"abcabcabc");
        let mut gz = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3, b'x', 0];
        gz.extend_from_slice(&deflated);
        assert_eq!(gunzip(&gz)?, b"abcabcabc");
        Ok(())
    }

    #[test]
    fn written_archives_read_back() -> Result<(), ImportError> {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let big = vec![b'a'; 10_000];
        let z = write(&[("3D/3dmodel.model".into(), big.clone()), ("x.png".into(), vec![1, 2, 3])])?;
        assert!(z.len() < 2_000);
        let zip = Zip::open(&z)?;
        assert_eq!(zip.read("3D/3dmodel.model")?, big);
        assert_eq!(zip.read("x.png")?, [1, 2, 3]);
        Ok(())
    }

    #[test]
    fn damaged_archives_are_errors() {
        let z = stored_zip(&[("a.txt", b"hello")]);
        for cut in 0..z.len() {
            if let Ok(zip) = Zip::open(&z[..cut]) {
                let _ = zip.read("a.txt");
            }
        }
        let mut bad = z.clone();
        if let Some(b) = bad.get_mut(z.len() - 6) {
            *b = 0xff;
        }
        assert!(Zip::open(&bad).is_err());
    }
}
