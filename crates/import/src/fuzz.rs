//! A small mutation fuzzer for the readers' tests (cargo-fuzz needs a nightly toolchain;
//! this runs with the normal tests). Each case starts from a valid sample and is damaged at
//! random: bytes flipped, dropped, repeated or inserted, numbers made hostile, the end cut
//! off. A reader passes when it returns, whatever it returns; a panic is a failure, reported
//! with the seed and the input written to the temporary folder to replay.
//!
//! `COSMICSLICER_FUZZ_ITERS` sets the cases per sample (default 200); set it high for a long
//! run. `COSMICSLICER_FUZZ_SEED` starts from another seed. Run it through `tools/capped.sh`:
//! each case's input sits in `<tmp>/cosmicslicer-fuzz/` while it runs, so a case that is
//! killed for using too much memory is left there, and on Linux a case that takes the process
//! past `MAX_PEAK_MB` fails.

/// xorshift64*: fast, small, and the same everywhere for a seed.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0 to n-1 (0 when n is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    /// -1 to 1.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in.max(1)) == 0
    }
}

/// Numbers that break careless parsers.
const HOSTILE: &[&str] = &[
    "nan",
    "-nan",
    "inf",
    "-inf",
    "1e309",
    "-1e309",
    "1e-320",
    "0",
    "-0",
    "4294967296",
    "18446744073709551616",
    "-2147483649",
    "99999999999999999999",
    "1e30",
    "-1",
    "",
];

/// `input` damaged at random, one to four times.
pub fn mutate(input: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = input.to_vec();
    for _ in 0..1 + rng.below(4) {
        let n = b.len();
        match rng.below(9) {
            0 if n > 0 => {
                let i = rng.below(n);
                if let Some(x) = b.get_mut(i) {
                    *x ^= 1 << rng.below(8);
                }
            }
            1 if n > 0 => {
                let i = rng.below(n);
                let len = 1 + rng.below((n - i).min(64));
                b.drain(i..(i + len).min(n));
            }
            2 if n > 0 => {
                // Repeat a stretch (deep nesting, long lists).
                let i = rng.below(n);
                let len = 1 + rng.below((n - i).min(64));
                let piece: Vec<u8> = b.get(i..(i + len).min(n)).unwrap_or(&[]).to_vec();
                for _ in 0..1 + rng.below(8) {
                    b.splice(i..i, piece.iter().copied());
                }
            }
            3 => {
                let i = rng.below(n + 1);
                b.insert(i, (rng.next_u64() & 0xFF) as u8);
            }
            4 if n > 0 => b.truncate(rng.below(n)),
            5 => {
                // A number replaced by a hostile one.
                let starts: Vec<usize> = (0..n)
                    .filter(|&i| {
                        b.get(i).is_some_and(u8::is_ascii_digit)
                            && (i == 0 || b.get(i - 1).is_some_and(|c| !c.is_ascii_digit()))
                    })
                    .collect();
                if let Some(&i) = starts.get(rng.below(starts.len())) {
                    let end =
                        (i..n).find(|&k| b.get(k).is_some_and(|c| !c.is_ascii_digit() && *c != b'.')).unwrap_or(n);
                    let with = HOSTILE.get(rng.below(HOSTILE.len())).copied().unwrap_or("0");
                    b.splice(i..end, with.bytes());
                }
            }
            6 if n >= 4 => {
                // Four bytes (a binary count or offset) set to an extreme.
                let i = rng.below(n - 3);
                let v: u32 = [0, 1, 0xFFFF_FFFF, 0x7FFF_FFFF, 0x8000_0000][rng.below(5)];
                b.splice(i..i + 4, v.to_le_bytes());
            }
            7 if n > 0 => {
                // Two stretches swapped.
                let (i, j) = (rng.below(n), rng.below(n));
                b.swap(i, j);
            }
            _ => {
                let i = rng.below(n + 1);
                let junk = [b"\0", &b"\xff\xfe"[..], b"((((", b"]]]]", b"<a>", b"#", b"\n\n"][rng.below(7)];
                b.splice(i..i, junk.iter().copied());
            }
        }
        if b.len() > 1 << 22 {
            b.truncate(1 << 22);
        }
    }
    b
}

/// Cases per sample.
pub fn iterations() -> usize {
    std::env::var("COSMICSLICER_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(200).clamp(1, 10_000_000)
}

fn base_seed() -> u64 {
    std::env::var("COSMICSLICER_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5EED_CAFE)
}

/// Runs `read` on each sample and on `iterations()` damaged copies of it. Returns the failures
/// (name, seed, iteration, where the input was saved); empty when every case returned.
pub fn run(name: &str, samples: &[Vec<u8>], read: &dyn Fn(&[u8])) -> Vec<String> {
    let mut failures = Vec::new();
    for (k, sample) in samples.iter().enumerate() {
        let seed = base_seed() ^ (k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ fnv(name);
        let mut rng = Rng::new(seed);
        for i in 0..=iterations() {
            let input = if i == 0 { sample.clone() } else { mutate(sample, &mut rng) };
            // Left behind if the process is killed mid-case (out of memory): the culprit.
            let running = running_dir().join(format!("{name}-{k}-case{i}-seed{seed:#x}.bin"));
            let _ = std::fs::write(&running, &input);
            let before = peak_rss_mb();
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&input))).is_ok();
            let grew = match (before, peak_rss_mb()) {
                (Some(a), Some(b)) if b > a.max(MAX_PEAK_MB) => Some(b),
                _ => None,
            };
            let _ = std::fs::remove_file(&running);
            if !ok || grew.is_some() {
                let path = std::env::temp_dir().join(format!("cosmicslicer-fuzz-{name}-{k}-{i}.bin"));
                let _ = std::fs::write(&path, &input);
                let what = match grew {
                    Some(mb) => format!("took the process to {mb} MB"),
                    None => "panicked".to_owned(),
                };
                failures
                    .push(format!("{name}: sample {k}, seed {seed:#x}, case {i} {what}; input in {}", path.display()));
                if failures.len() > 5 {
                    break;
                }
            }
        }
    }
    failures
}

/// A case that takes the process's peak memory past this fails: no file of a few MB needs it.
const MAX_PEAK_MB: u64 = 512;

/// `<tmp>/cosmicslicer-fuzz/`, which holds the input of each case while it runs.
fn running_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("cosmicslicer-fuzz");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The process's peak resident memory (Linux; `None` elsewhere).
fn peak_rss_mb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_change_things_and_stay_bounded() {
        let mut rng = Rng::new(7);
        let sample = b"v 1.5 2 3\nf 1 2 3\n".to_vec();
        let changed = (0..100).filter(|_| mutate(&sample, &mut rng) != sample).count();
        assert!(changed > 80, "{changed}");
        assert!(mutate(&[], &mut rng).len() <= 64);
        let caught = run("selftest", &[b"x".to_vec()], &|b| {
            assert!(b.first() != Some(&b'!'), "planted");
        });
        // The planted panic may or may not be reached; the harness itself never panics.
        assert!(caught.len() <= 6);
    }
}
