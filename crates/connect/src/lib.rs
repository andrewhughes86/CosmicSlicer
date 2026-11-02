//! Printer connections: OrcaSlicer's print hosts (OctoPrint and Klipper's Moonraker,
//! PrusaLink, Duet, Repetier, AstroBox, ESP3D, MKS, FlashAir, …) and Bambu Lab printers in
//! LAN mode, without Bambu's closed network plugin.
//!
//! Everything here blocks, with a timeout on every read and write; the app runs it on a
//! worker thread per printer (`link`), so a printer that stops answering never holds up the
//! window. Replies are hostile input: sizes are capped and every field is optional.
#![allow(non_snake_case)]

pub mod bambu;
pub mod hosts;
pub mod http;
pub mod link;
pub mod md5;
pub mod mqtt;
pub mod net;

use serde::{Deserialize, Serialize};

/// OrcaSlicer's host types (its `host_type` values), and Bambu Lab printers in LAN mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostType {
    /// OctoPrint, or Klipper through Moonraker (found out when connecting).
    #[default]
    OctoPrint,
    PrusaLink,
    PrusaConnect,
    Duet,
    FlashAir,
    AstroBox,
    Repetier,
    Mks,
    Esp3d,
    CrealityPrint,
    Obico,
    Flashforge,
    SimplyPrint,
    ElegooLink,
    /// A Bambu Lab printer on the local network (MQTT, FTPS), with its access code.
    BambuLan,
}

impl HostType {
    pub const ALL: [HostType; 15] = [
        HostType::OctoPrint,
        HostType::BambuLan,
        HostType::PrusaLink,
        HostType::PrusaConnect,
        HostType::Duet,
        HostType::FlashAir,
        HostType::AstroBox,
        HostType::Repetier,
        HostType::Mks,
        HostType::Esp3d,
        HostType::CrealityPrint,
        HostType::Obico,
        HostType::Flashforge,
        HostType::SimplyPrint,
        HostType::ElegooLink,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HostType::OctoPrint => "Octo/Klipper",
            HostType::PrusaLink => "PrusaLink",
            HostType::PrusaConnect => "PrusaConnect",
            HostType::Duet => "Duet",
            HostType::FlashAir => "FlashAir",
            HostType::AstroBox => "AstroBox",
            HostType::Repetier => "Repetier",
            HostType::Mks => "MKS",
            HostType::Esp3d => "ESP3D",
            HostType::CrealityPrint => "CrealityPrint",
            HostType::Obico => "Obico",
            HostType::Flashforge => "Flashforge",
            HostType::SimplyPrint => "SimplyPrint",
            HostType::ElegooLink => "Elegoo Link",
            HostType::BambuLan => "Bambu Lab (LAN mode)",
        }
    }

    /// OrcaSlicer's `host_type` value.
    pub fn orca(self) -> &'static str {
        match self {
            HostType::OctoPrint => "octoprint",
            HostType::PrusaLink => "prusalink",
            HostType::PrusaConnect => "prusaconnect",
            HostType::Duet => "duet",
            HostType::FlashAir => "flashair",
            HostType::AstroBox => "astrobox",
            HostType::Repetier => "repetier",
            HostType::Mks => "mks",
            HostType::Esp3d => "esp3d",
            HostType::CrealityPrint => "crealityprint",
            HostType::Obico => "obico",
            HostType::Flashforge => "flashforge",
            HostType::SimplyPrint => "simplyprint",
            HostType::ElegooLink => "elegoolink",
            HostType::BambuLan => "bambu_lan",
        }
    }

    pub fn from_orca(s: &str) -> Option<HostType> {
        HostType::ALL.into_iter().find(|h| h.orca() == s.trim())
    }

    /// Why plates cannot be sent to this kind of host yet, if they cannot.
    pub fn unsupported(self) -> Option<&'static str> {
        match self {
            HostType::PrusaConnect | HostType::Obico | HostType::SimplyPrint => {
                Some("this is a cloud service; CosmicSlicer does not send to it yet")
            }
            HostType::CrealityPrint | HostType::Flashforge | HostType::ElegooLink => {
                Some("CosmicSlicer does not speak this printer's protocol yet")
            }
            _ => None,
        }
    }

    /// Whether the Device page can follow and control the printer.
    pub fn live(self) -> bool {
        matches!(self, HostType::OctoPrint | HostType::PrusaLink | HostType::BambuLan)
    }

    /// Whether the printer offers more than one place to store files.
    pub fn has_storage(self) -> bool {
        matches!(self, HostType::OctoPrint | HostType::PrusaLink)
    }
}

/// A printer on the network, as OrcaSlicer's "physical printer".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PhysicalPrinter {
    pub name: String,
    pub host: HostType,
    /// Hostname, IP or URL (`print_host`).
    pub address: String,
    /// API key or password (`printhost_apikey`).
    pub api_key: String,
    /// User and password (PrusaLink's digest login).
    pub user: String,
    pub password: String,
    /// Repetier's printer slug (`printhost_port`).
    pub slug: String,
    /// The storage to upload to (PrusaLink `usb`, OctoPrint `local` or `sdcard`); empty for
    /// the printer's default.
    pub storage: String,
    /// Check HTTPS certificates against the public roots (printers mostly sign their own).
    pub verify_tls: bool,
    /// Bambu: the printer's serial number and the access code from its screen.
    pub serial: String,
    pub access_code: String,
    /// Bambu: whether to print with the AMS, and which AMS slot each filament uses.
    pub use_ams: bool,
    pub ams_mapping: Vec<i32>,
    /// Printer presets this printer is used for.
    pub presets: Vec<String>,
}

impl PhysicalPrinter {
    pub fn trust(&self) -> net::Trust {
        if self.verify_tls { net::Trust::Verified } else { net::Trust::Any }
    }
}

/// What a printer is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Unknown,
    Idle,
    Preparing,
    Printing,
    Paused,
    Finished,
    Failed,
    Error,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Unknown => "Connecting",
            State::Idle => "Idle",
            State::Preparing => "Preparing",
            State::Printing => "Printing",
            State::Paused => "Paused",
            State::Finished => "Finished",
            State::Failed => "Failed",
            State::Error => "Error",
        }
    }

    pub fn busy(self) -> bool {
        matches!(self, State::Preparing | State::Printing | State::Paused)
    }
}

/// A heater's reading.
#[derive(Clone, Debug, PartialEq)]
pub struct Temp {
    pub name: String,
    pub actual: f32,
    pub target: f32,
}

/// A printer's state as last read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub state: State,
    /// The printer's own word for it.
    pub detail: String,
    /// 0 to 1.
    pub progress: Option<f32>,
    pub file: Option<String>,
    /// (current, total).
    pub layer: Option<(u32, u32)>,
    pub remaining_s: Option<f64>,
    pub temps: Vec<Temp>,
    /// Part cooling fan, 0 to 1.
    pub fan: Option<f32>,
    /// Bambu's speed level, 1 (silent) to 4 (ludicrous).
    pub speed: Option<u8>,
    /// The printer's camera can be shown.
    pub camera: bool,
}

/// A heater to set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heater {
    Nozzle,
    Bed,
}

/// Something to ask a printer to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Pause,
    Resume,
    Stop,
    /// Lines of G-code.
    Gcode(String),
    /// Move one axis ('X', 'Y', 'Z' or 'E') by mm, relative.
    Jog(char, f32),
    Home,
    SetTemp(Heater, f32),
    /// Part cooling fan, 0 to 1.
    Fan(f32),
    /// Bambu's speed level, 1 to 4.
    Speed(u8),
}

impl Command {
    /// The G-code for commands that are G-code on any printer.
    pub fn gcode(&self) -> Option<String> {
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        Some(match self {
            Command::Gcode(g) => g.clone(),
            Command::Jog('E', mm) => format!("M83\nG1 E{:.2} F300", finite(*mm).clamp(-100.0, 100.0)),
            Command::Jog(axis, mm) => {
                let mm = finite(*mm).clamp(-500.0, 500.0);
                let feed = if *axis == 'Z' { 600 } else { 3000 };
                format!("G91\nG1 {axis}{mm:.2} F{feed}\nG90")
            }
            Command::Home => "G28".into(),
            Command::SetTemp(Heater::Nozzle, t) => format!("M104 S{:.0}", finite(*t).clamp(0.0, 500.0)),
            Command::SetTemp(Heater::Bed, t) => format!("M140 S{:.0}", finite(*t).clamp(0.0, 200.0)),
            Command::Fan(f) => format!("M106 S{:.0}", finite(*f).clamp(0.0, 1.0) * 255.0),
            _ => return None,
        })
    }
}

/// A place on the printer to store files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    pub id: String,
    pub name: String,
}

/// A file name the printer will accept: no path, no odd characters, a G-code extension.
pub fn safe_name(name: &str, ext: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut s: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.() ".contains(c) { c } else { '_' })
        .take(120)
        .collect();
    s = s.trim().trim_start_matches('.').to_owned();
    if s.is_empty() {
        s = "plate".into();
    }
    if !s.to_ascii_lowercase().ends_with(ext) {
        s.push_str(ext);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_gcode() {
        assert_eq!(safe_name("../a b/c?d", ".gcode"), "c_d.gcode");
        assert_eq!(safe_name("", ".gcode"), "plate.gcode");
        assert_eq!(safe_name("x.GCODE", ".gcode"), "x.GCODE");
        assert_eq!(Command::Jog('X', 10.0).gcode().as_deref(), Some("G91\nG1 X10.00 F3000\nG90"));
        assert_eq!(Command::Fan(0.5).gcode().as_deref(), Some("M106 S128"));
        assert_eq!(Command::SetTemp(Heater::Nozzle, f32::NAN).gcode().as_deref(), Some("M104 S0"));
        assert!(Command::Pause.gcode().is_none());
        for h in HostType::ALL {
            assert_eq!(HostType::from_orca(h.orca()), Some(h));
        }
    }
}
