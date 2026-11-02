//! 3Dconnexion mice. On Linux and BSD through spacenavd's socket, read on its own thread;
//! the canvas takes the motion summed since the last frame. Windows and macOS need
//! 3Dconnexion's own driver interface, which CosmicSlicer does not speak yet.

use std::sync::{Arc, Mutex, OnceLock};

/// Motion summed since it was last taken: translation X, Y, Z and rotation X, Y, Z, in the
/// device's units (about ±350 at full deflection per event).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    pub t: [f32; 3],
    pub r: [f32; 3],
    /// Events summed.
    pub events: u32,
}

#[derive(Default)]
struct Shared {
    motion: Motion,
    connected: bool,
}

fn shared() -> &'static Arc<Mutex<Shared>> {
    static S: OnceLock<Arc<Mutex<Shared>>> = OnceLock::new();
    S.get_or_init(|| Arc::new(Mutex::new(Shared::default())))
}

/// What the Preferences page says about 3D mice here.
pub fn support_note() -> &'static str {
    if cfg!(unix) && !cfg!(target_os = "macos") {
        "3Dconnexion mice work through spacenavd (install and start it)."
    } else {
        "3Dconnexion mice are supported on Linux (spacenavd) for now."
    }
}

/// Starts listening for a device (once); harmless when there is none.
pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let state = Arc::clone(shared());
            let _ = std::thread::Builder::new().name("3d mouse".into()).spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listen(&state)));
            });
        }
    });
}

/// Whether a device is connected.
pub fn connected() -> bool {
    shared().lock().map(|s| s.connected).unwrap_or(false)
}

/// The motion since the last call.
pub fn take() -> Motion {
    shared().lock().map(|mut s| std::mem::take(&mut s.motion)).unwrap_or_default()
}

/// Adds one spacenavd event (eight native-endian i32: type, then x y z rx ry rz for motion).
pub fn feed(event: &[i32; 8]) {
    if event[0] != 0 {
        return;
    }
    if let Ok(mut s) = shared().lock() {
        let m = &mut s.motion;
        for k in 0..3 {
            m.t[k] += event[1 + k] as f32;
            m.r[k] += event[4 + k] as f32;
        }
        m.events = m.events.saturating_add(1);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn listen(state: &Arc<Mutex<Shared>>) {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    loop {
        if let Ok(mut s) =
            UnixStream::connect("/var/run/spnav.sock").or_else(|_| UnixStream::connect("/run/spnav.sock"))
        {
            if let Ok(mut g) = state.lock() {
                g.connected = true;
            }
            let mut buf = [0u8; 32];
            while s.read_exact(&mut buf).is_ok() {
                let mut e = [0i32; 8];
                for (slot, chunk) in e.iter_mut().zip(buf.as_chunks::<4>().0) {
                    *slot = i32::from_ne_bytes(*chunk);
                }
                feed(&e);
            }
            if let Ok(mut g) = state.lock() {
                g.connected = false;
            }
        }
        // No daemon, or it went away: look again now and then.
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_sums_and_is_taken() {
        let _ = take();
        feed(&[0, 10, 0, -5, 1, 2, 3, 16]);
        feed(&[0, 10, 0, 0, 0, 0, 0, 16]);
        feed(&[1, 0, 0, 0, 0, 0, 0, 0]);
        let m = take();
        assert_eq!((m.t, m.r, m.events), ([20.0, 0.0, -5.0], [1.0, 2.0, 3.0], 2));
        assert_eq!(take().events, 0);
        assert!(!support_note().is_empty());
    }
}
