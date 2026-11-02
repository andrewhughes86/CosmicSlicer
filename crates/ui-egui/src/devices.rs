//! The printers on the network as the window sees them: a connection (worker thread) per
//! printer in use, what each last reported, its camera picture, and the background jobs the
//! printer dialog starts (test, storage list, discovery). Nothing here blocks.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use CosmicSlicer_connect::link::{Event, Link, Request};
use CosmicSlicer_connect::{Command, PhysicalPrinter, Status, Storage, bambu, hosts};

use crate::state::{Level, Session};

/// What the window knows of one printer.
#[derive(Default)]
pub struct DeviceInfo {
    pub status: Option<Status>,
    /// Why the printer cannot be reached, while it cannot.
    pub offline: Option<String>,
    pub frame: Option<egui::TextureHandle>,
    pub storages: Vec<Storage>,
    /// An upload under way.
    pub sending: bool,
}

/// A job on its own thread, for the dialog.
pub struct Job<T> {
    rx: Receiver<T>,
}

impl<T: Send + 'static> Job<T> {
    pub fn start(work: impl FnOnce() -> T + Send + 'static) -> Option<Job<T>> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("printer job".into())
            .spawn(move || {
                if let Ok(v) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
                    let _ = tx.send(v);
                }
            })
            .ok()?;
        Some(Job { rx })
    }

    /// The result once ready; `Some(None)` if the job died.
    pub fn poll(&self) -> Option<Option<T>> {
        match self.rx.try_recv() {
            Ok(v) => Some(Some(v)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}

#[derive(Default)]
pub struct Devices {
    links: Vec<Link>,
    pub info: std::collections::HashMap<String, DeviceInfo>,
    /// The printer whose camera is on.
    camera: Option<String>,
}

impl Devices {
    /// The connection to `p`, started (or restarted, if its settings changed) as needed.
    fn link(&mut self, p: &PhysicalPrinter) -> Option<&Link> {
        if let Some(i) = self.links.iter().position(|l| l.printer.name == p.name)
            && self.links.get(i).is_some_and(|l| l.printer != *p)
        {
            self.links.remove(i);
            self.info.remove(&p.name);
        }
        if !self.links.iter().any(|l| l.printer.name == p.name) {
            let link = Link::start(p.clone()).ok()?;
            self.links.push(link);
        }
        self.links.iter().find(|l| l.printer.name == p.name)
    }

    /// Follows `p` (the Device pages call this for the printers they show).
    pub fn watch(&mut self, p: &PhysicalPrinter) {
        let _ = self.link(p);
    }

    pub fn send(&mut self, p: &PhysicalPrinter, r: Request) {
        if let Request::Upload { .. } = r {
            self.info.entry(p.name.clone()).or_default().sending = true;
        }
        if let Some(l) = self.link(p) {
            l.send(r);
        }
    }

    pub fn control(&mut self, p: &PhysicalPrinter, cmd: Command) {
        self.send(p, Request::Control(cmd));
    }

    /// Shows `p`'s camera (turning off any other), or none.
    pub fn camera(&mut self, p: Option<&PhysicalPrinter>) {
        let want = p.map(|p| p.name.clone());
        if want == self.camera {
            return;
        }
        if let Some(old) = self.camera.take()
            && let Some(l) = self.links.iter().find(|l| l.printer.name == old)
        {
            l.send(Request::Camera(false));
        }
        if let Some(p) = p {
            self.send(p, Request::Camera(true));
            self.camera = Some(p.name.clone());
        }
    }

    pub fn camera_on(&self, name: &str) -> bool {
        self.camera.as_deref() == Some(name)
    }

    /// Drops connections to printers no longer set up.
    pub fn prune(&mut self, printers: &[PhysicalPrinter]) {
        self.links.retain(|l| printers.iter().any(|p| p == &l.printer));
        self.info.retain(|name, _| printers.iter().any(|p| &p.name == name));
        if self.camera.as_ref().is_some_and(|c| !printers.iter().any(|p| &p.name == c)) {
            self.camera = None;
        }
    }

    /// Picks up what the printers have said.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut Session) {
        for link in &self.links {
            let name = link.printer.name.clone();
            for event in link.poll() {
                let info = self.info.entry(name.clone()).or_default();
                match event {
                    Event::Status(s) => {
                        info.status = Some(s);
                        info.offline = None;
                    }
                    Event::Offline(why) => {
                        info.offline = Some(why);
                    }
                    Event::Done(Ok(note)) => {
                        info.sending = false;
                        session.notify(Level::Info, format!("{name}: {note}"));
                    }
                    Event::Done(Err(e)) => {
                        info.sending = false;
                        session.notify(Level::Error, format!("{name}: {e}"));
                    }
                    Event::Frame(f) => {
                        let image = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
                        match &mut info.frame {
                            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                            None => {
                                info.frame = Some(ctx.load_texture(
                                    format!("camera-{name}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ))
                            }
                        }
                    }
                    Event::Storages(list) => info.storages = list,
                }
            }
        }
        if !self.links.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

/// Tests `p` on a thread.
pub fn test(p: &PhysicalPrinter) -> Option<Job<Result<String, String>>> {
    let p = p.clone();
    Job::start(move || {
        if p.host == CosmicSlicer_connect::HostType::BambuLan {
            bambu::Session::connect(&p).map(|_| "Connected to the printer in LAN mode".to_owned())
        } else {
            hosts::test(&p)
        }
    })
}

/// Lists `p`'s storage on a thread.
pub fn storages(p: &PhysicalPrinter) -> Option<Job<Result<Vec<Storage>, String>>> {
    let p = p.clone();
    Job::start(move || hosts::storages(&p))
}

/// Listens for Bambu printers' announcements on a thread.
pub fn discover() -> Option<Job<Result<Vec<bambu::Found>, String>>> {
    Job::start(|| bambu::discover(std::time::Duration::from_secs(6)))
}
