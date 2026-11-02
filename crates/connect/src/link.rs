//! A printer's connection on its own thread: the window sends requests and picks up events
//! each frame, so nothing the printer does (or fails to do) ever holds up the window.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::hosts::{self, Flavour};
use crate::{Command, HostType, PhysicalPrinter, Status, Storage, bambu};

/// How often an HTTP printer is asked for its state.
const POLL: Duration = Duration::from_secs(2);
/// How long to wait before trying a printer that did not answer again.
const RETRY: Duration = Duration::from_secs(5);
/// Most events kept waiting for the window.
const MAX_EVENTS: usize = 256;

/// What the window asks of a printer.
#[derive(Clone, Debug)]
pub enum Request {
    /// Send `data` as `name`; with `start`, print it. For a Bambu printer `data` is a
    /// sliced 3MF.
    Upload {
        name: String,
        data: Vec<u8>,
        start: bool,
    },
    Control(Command),
    /// Show (or stop showing) the camera.
    Camera(bool),
    /// List the printer's storage.
    Storages,
}

/// A picture from the camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// What a printer tells the window.
#[derive(Clone, Debug)]
pub enum Event {
    Status(Status),
    /// The printer cannot be reached (or stopped answering).
    Offline(String),
    /// An upload or command finished: Ok with a note, or the error.
    Done(Result<String, String>),
    Frame(Frame),
    Storages(Vec<Storage>),
}

pub struct Link {
    pub printer: PhysicalPrinter,
    tx: Sender<Request>,
    rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
}

impl Link {
    pub fn start(printer: PhysicalPrinter) -> Result<Link, String> {
        let (tx, worker_rx) = mpsc::channel();
        let (worker_tx, rx) = mpsc::sync_channel(MAX_EVENTS);
        let stop = Arc::new(AtomicBool::new(false));
        let (p, flag) = (printer.clone(), Arc::clone(&stop));
        std::thread::Builder::new()
            .name(format!("printer {}", printer.name))
            .spawn(move || {
                // A bug in here must not take the window down: the link just ends.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Worker::new(p, worker_rx, worker_tx, flag).run();
                }));
            })
            .map_err(|e| format!("could not start the printer's connection: {e}"))?;
        Ok(Link { printer, tx, rx, stop })
    }

    pub fn send(&self, r: Request) {
        let _ = self.tx.send(r);
    }

    /// Events since the last call.
    pub fn poll(&self) -> Vec<Event> {
        self.rx.try_iter().take(MAX_EVENTS).collect()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Worker {
    p: PhysicalPrinter,
    rx: Receiver<Request>,
    tx: mpsc::SyncSender<Event>,
    stop: Arc<AtomicBool>,
    flavour: Option<Flavour>,
    bambu: Option<bambu::Session>,
    last_try: Option<Instant>,
    last_poll: Option<Instant>,
    offline: Option<String>,
    camera: Option<Arc<AtomicBool>>,
}

impl Worker {
    fn new(p: PhysicalPrinter, rx: Receiver<Request>, tx: mpsc::SyncSender<Event>, stop: Arc<AtomicBool>) -> Worker {
        Worker {
            p,
            rx,
            tx,
            stop,
            flavour: None,
            bambu: None,
            last_try: None,
            last_poll: None,
            offline: None,
            camera: None,
        }
    }

    fn emit(&self, e: Event) {
        // A full queue (the window is not reading) drops the event rather than block.
        let _ = self.tx.try_send(e);
    }

    fn run(mut self) {
        while !self.stop.load(Ordering::Relaxed) {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => self.handle(r),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        if let Some(c) = &self.camera {
            c.store(true, Ordering::Relaxed);
        }
    }

    fn offline(&mut self, why: String) {
        if self.offline.as_ref() != Some(&why) {
            self.emit(Event::Offline(why.clone()));
            self.offline = Some(why);
        }
    }

    fn tick(&mut self) {
        if !self.p.host.live() {
            return;
        }
        if self.p.host == HostType::BambuLan {
            if self.bambu.is_none() {
                if self.last_try.is_some_and(|t| t.elapsed() < RETRY) {
                    return;
                }
                self.last_try = Some(Instant::now());
                match bambu::Session::connect(&self.p) {
                    Ok(s) => {
                        self.bambu = Some(s);
                        self.offline = None;
                    }
                    Err(e) => return self.offline(e),
                }
            }
            let pumped = self.bambu.as_mut().map(bambu::Session::pump);
            match pumped {
                Some(Ok(true)) => {
                    if let Some(s) = &self.bambu {
                        let status = bambu::status(&s.state);
                        self.emit(Event::Status(status));
                    }
                }
                Some(Err(e)) => {
                    self.bambu = None;
                    self.offline(e);
                }
                _ => {}
            }
            return;
        }
        if self.last_poll.is_some_and(|t| t.elapsed() < POLL) {
            return;
        }
        self.last_poll = Some(Instant::now());
        let flavour = *self.flavour.get_or_insert_with(|| hosts::flavour(&self.p));
        match hosts::status(&self.p, flavour) {
            Ok(s) => {
                self.offline = None;
                self.emit(Event::Status(s));
            }
            Err(e) => {
                // Found out afresh next time: the printer may have been swapped or restarted.
                self.flavour = None;
                self.offline(e);
            }
        }
    }

    fn handle(&mut self, r: Request) {
        match r {
            Request::Upload { name, data, start } => {
                let result = if self.p.host == HostType::BambuLan {
                    self.bambu_print(&name, &data, start)
                } else {
                    hosts::upload(&self.p, &name, &data, start)
                };
                let note = if start { "Sent to the printer; printing starts." } else { "Sent to the printer." };
                self.emit(Event::Done(result.map(|()| note.to_owned())));
                self.last_poll = None;
            }
            Request::Control(cmd) => {
                let result = if self.p.host == HostType::BambuLan {
                    match (self.bambu.as_mut(), bambu::command(&cmd)) {
                        (Some(s), Some(v)) => s.send(&v).and_then(|()| s.refresh()),
                        (None, _) => Err("the printer is not connected".into()),
                        (_, None) => Err("the printer has no such command".into()),
                    }
                } else {
                    let flavour = *self.flavour.get_or_insert_with(|| hosts::flavour(&self.p));
                    hosts::control(&self.p, flavour, &cmd)
                };
                if let Err(e) = result {
                    self.emit(Event::Done(Err(e)));
                }
                self.last_poll = None;
            }
            Request::Camera(on) => {
                if let Some(c) = self.camera.take() {
                    c.store(true, Ordering::Relaxed);
                }
                if on {
                    let stop = Arc::new(AtomicBool::new(false));
                    self.camera = Some(Arc::clone(&stop));
                    let (p, tx, flavour) = (self.p.clone(), self.tx.clone(), self.flavour);
                    let _ = std::thread::Builder::new().name("camera".into()).spawn(move || {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| camera(p, flavour, tx, stop)));
                    });
                }
            }
            Request::Storages => match hosts::storages(&self.p) {
                Ok(list) => self.emit(Event::Storages(list)),
                Err(e) => self.emit(Event::Done(Err(e))),
            },
        }
    }

    fn bambu_print(&mut self, name: &str, data: &[u8], start: bool) -> Result<(), String> {
        if !start {
            let name = crate::safe_name(name.trim_end_matches(".gcode.3mf"), ".gcode.3mf");
            return bambu::upload(&self.p, &name, data);
        }
        if self.bambu.is_none() {
            self.bambu = Some(bambu::Session::connect(&self.p)?);
        }
        match self.bambu.as_mut() {
            Some(s) => bambu::print(s, &self.p, name, data),
            None => Err("the printer is not connected".into()),
        }
    }
}

/// Fetches camera pictures until `stop`.
fn camera(p: PhysicalPrinter, flavour: Option<Flavour>, tx: mpsc::SyncSender<Event>, stop: Arc<AtomicBool>) {
    let mut bambu: Option<bambu::Camera> = None;
    let mut failures = 0;
    while !stop.load(Ordering::Relaxed) && failures < 5 {
        let jpeg = if p.host == HostType::BambuLan {
            if bambu.is_none() {
                match bambu::Camera::connect(&p) {
                    Ok(c) => bambu = Some(c),
                    Err(e) => {
                        let _ = tx.try_send(Event::Done(Err(e)));
                        return;
                    }
                }
            }
            match bambu.as_mut().map(bambu::Camera::frame) {
                Some(Ok(j)) => Ok(j),
                Some(Err(e)) => {
                    bambu = None;
                    Err(e)
                }
                None => Err("camera".into()),
            }
        } else {
            let f = flavour.unwrap_or_else(|| hosts::flavour(&p));
            let r = hosts::snapshot(&p, f);
            std::thread::sleep(Duration::from_millis(800));
            r
        };
        match jpeg.and_then(|j| decode(&j)) {
            Ok(frame) => {
                failures = 0;
                let _ = tx.try_send(Event::Frame(frame));
            }
            Err(e) => {
                failures += 1;
                if failures == 5 {
                    let _ = tx.try_send(Event::Done(Err(format!("camera: {e}"))));
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// A JPEG as RGBA.
pub fn decode(jpeg: &[u8]) -> Result<Frame, String> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let opts =
        DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA).set_max_width(4096).set_max_height(4096);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    let rgba = d.decode().map_err(|e| format!("not a picture: {e:?}"))?;
    let (width, height) = d.dimensions().ok_or("not a picture")?;
    if rgba.len() != width * height * 4 {
        return Err("a damaged picture".into());
    }
    Ok(Frame { width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::{ok_json, serve};

    #[test]
    fn a_link_reports_and_survives_a_dead_printer() {
        let (addr, _rx) = serve(8, |head, _| {
            if head.starts_with("GET /server/info") {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()
            } else if head.starts_with("GET /api/printer") {
                ok_json(r#"{"state":{"flags":{"ready":true}},"temperature":{"tool0":{"actual":25,"target":0}}}"#)
            } else {
                ok_json(r#"{"state":"Operational","progress":{}}"#)
            }
        });
        let link = Link::start(PhysicalPrinter { name: "a".into(), address: addr, ..Default::default() });
        let Ok(link) = link else { return };
        let start = Instant::now();
        let mut got = None;
        while start.elapsed() < Duration::from_secs(10) && got.is_none() {
            got = link.poll().into_iter().find_map(|e| match e {
                Event::Status(s) => Some(s),
                _ => None,
            });
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(got.is_some_and(|s| s.state == crate::State::Idle && s.temps.len() == 1));
        // Nothing listens here: an Offline event, and the window is never held up.
        let dead =
            Link::start(PhysicalPrinter { name: "b".into(), address: "127.0.0.1:9".into(), ..Default::default() });
        let Ok(dead) = dead else { return };
        let start = Instant::now();
        let mut offline = false;
        while start.elapsed() < Duration::from_secs(10) && !offline {
            let t = Instant::now();
            offline = dead.poll().iter().any(|e| matches!(e, Event::Offline(_)));
            assert!(t.elapsed() < Duration::from_millis(50));
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(offline);
    }

    #[test]
    fn jpeg_frames_decode_and_junk_does_not() {
        assert!(decode(b"not a jpeg").is_err());
        assert!(decode(&[]).is_err());
    }
}
