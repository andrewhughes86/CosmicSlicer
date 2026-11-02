//! Bambu Lab printers in LAN mode, without Bambu's network plugin: status and commands over
//! the printer's MQTT broker (TLS, port 8883, user `bblp`, the access code from the printer's
//! screen), files over implicit FTPS (port 990), prints started from a sliced 3MF
//! (`.gcode.3mf`), printers found by their SSDP announcements, and the P1/A1 camera (TLS,
//! port 6000). Newer firmware only allows this with "LAN only" mode (or developer mode) on.

use std::io::{self, Read, Write};
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::net::{self, Stream, Trust};
use crate::{Command, PhysicalPrinter, State, Status, Temp};

pub const MQTT_PORT: u16 = 8883;
pub const FTPS_PORT: u16 = 990;
pub const CAMERA_PORT: u16 = 6000;
/// The port printers announce themselves on.
pub const SSDP_PORT: u16 = 2021;

pub fn report_topic(serial: &str) -> String {
    format!("device/{serial}/report")
}

pub fn request_topic(serial: &str) -> String {
    format!("device/{serial}/request")
}

/// Asks for a full report (P1 and A1 printers otherwise send only changes).
pub fn pushall() -> Value {
    json!({"pushing": {"sequence_id": "0", "command": "pushall", "version": 1, "push_target": 1}})
}

/// Folds a report into the state kept so far (reports carry only what changed).
pub fn merge(state: &mut Value, delta: &Value) {
    match (state, delta) {
        (Value::Object(s), Value::Object(d)) => {
            for (k, v) in d {
                match s.get_mut(k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        s.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (s, d) => *s = d.clone(),
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x| x.is_finite())
}

/// The printer's state from its accumulated report.
pub fn status(state: &Value) -> Status {
    let p = &state["print"];
    let detail = p["gcode_state"].as_str().unwrap_or("").to_owned();
    let mut s = Status {
        state: match detail.as_str() {
            "IDLE" => State::Idle,
            "PREPARE" | "SLICING" => State::Preparing,
            "RUNNING" => State::Printing,
            "PAUSE" => State::Paused,
            "FINISH" => State::Finished,
            "FAILED" => State::Failed,
            _ => State::Unknown,
        },
        detail,
        camera: true,
        ..Default::default()
    };
    if num(&p["print_error"]).is_some_and(|e| e != 0.0) && s.state != State::Finished {
        s.state = if s.state == State::Paused { State::Paused } else { State::Error };
    }
    s.progress = num(&p["mc_percent"]).map(|x| (x as f32 / 100.0).clamp(0.0, 1.0));
    s.remaining_s = num(&p["mc_remaining_time"]).filter(|m| *m >= 0.0).map(|m| m * 60.0);
    s.file = p["subtask_name"].as_str().filter(|f| !f.is_empty()).map(str::to_owned);
    if let (Some(c), Some(t)) = (num(&p["layer_num"]), num(&p["total_layer_num"])) {
        s.layer = Some((c.clamp(0.0, 1e6) as u32, t.clamp(0.0, 1e6) as u32));
    }
    for (a, t, name) in [
        ("nozzle_temper", "nozzle_target_temper", "Nozzle"),
        ("bed_temper", "bed_target_temper", "Bed"),
        ("chamber_temper", "", "Chamber"),
    ] {
        if let Some(actual) = num(&p[a]) {
            let target = num(&p[t]).unwrap_or(0.0);
            s.temps.push(Temp { name: name.into(), actual: actual as f32, target: target as f32 });
        }
    }
    // The fan speed is 0 to 15.
    s.fan = num(&p["cooling_fan_speed"]).map(|f| (f as f32 / 15.0).clamp(0.0, 1.0));
    s.speed = num(&p["spd_lvl"]).map(|l| l.clamp(1.0, 4.0) as u8);
    s
}

/// The request for `cmd`.
pub fn command(cmd: &Command) -> Option<Value> {
    let print = |command: &str, param: Option<String>| {
        let mut v = json!({"sequence_id": "0", "command": command});
        if let (Some(p), Some(o)) = (param, v.as_object_mut()) {
            o.insert("param".into(), Value::String(p));
        }
        json!({ "print": v })
    };
    Some(match cmd {
        Command::Pause => print("pause", None),
        Command::Resume => print("resume", None),
        Command::Stop => print("stop", None),
        Command::Speed(l) => print("print_speed", Some((*l).clamp(1, 4).to_string())),
        // Bambu's part fan is P1.
        Command::Fan(f) => {
            let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
            print("gcode_line", Some(format!("M106 P1 S{:.0}\n", f * 255.0)))
        }
        other => print("gcode_line", Some(format!("{}\n", other.gcode()?))),
    })
}

/// Starts printing `file` (already on the printer's storage) from plate `plate`.
pub fn start_print(file: &str, plate: usize, md5: &str, use_ams: bool, ams_mapping: &[i32]) -> Value {
    let title = file.trim_end_matches(".gcode.3mf").trim_end_matches(".3mf");
    json!({"print": {
        "sequence_id": "0",
        "command": "project_file",
        "param": format!("Metadata/plate_{}.gcode", plate.max(1)),
        "project_id": "0", "profile_id": "0", "task_id": "0", "subtask_id": "0",
        "subtask_name": title,
        "url": format!("ftp://{file}"),
        "md5": md5,
        "timelapse": false,
        "bed_type": "auto",
        "bed_levelling": true,
        "flow_cali": false,
        "vibration_cali": false,
        "layer_inspect": false,
        "use_ams": use_ams,
        "ams_mapping": ams_mapping,
    }})
}

/// A filament as the sliced 3MF lists it.
#[derive(Clone, Debug, Default)]
pub struct Filament {
    pub kind: String,
    /// `#RRGGBB`.
    pub colour: String,
    pub used_m: f64,
    pub used_g: f64,
}

/// One plate's G-code packed as Bambu's sliced 3MF, which the printers print from.
pub fn sliced_3mf(
    gcode: &[u8],
    thumbnail_png: Option<&[u8]>,
    seconds: f64,
    filaments: &[Filament],
) -> Result<Vec<u8>, String> {
    let md5 = crate::md5::hex(gcode).to_ascii_uppercase();
    let weight: f64 = filaments.iter().map(|f| f.used_g).filter(|g| g.is_finite()).sum();
    let mut info = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <header>\n    <header_item key=\"X-BBL-Client-Type\" value=\"slicer\"/>\n    <header_item key=\"X-BBL-Client-Version\" value=\"01.10.00.00\"/>\n  </header>\n  <plate>\n    <metadata key=\"index\" value=\"1\"/>\n",
    );
    info.push_str(&format!("    <metadata key=\"prediction\" value=\"{:.0}\"/>\n", seconds.max(0.0)));
    info.push_str(&format!("    <metadata key=\"weight\" value=\"{weight:.2}\"/>\n"));
    for (i, f) in filaments.iter().enumerate() {
        let esc = |s: &str| s.replace(['"', '<', '>', '&'], "");
        info.push_str(&format!(
            "    <filament id=\"{}\" type=\"{}\" color=\"{}\" used_m=\"{:.2}\" used_g=\"{:.2}\"/>\n",
            i + 1,
            esc(&f.kind),
            esc(&f.colour),
            f.used_m,
            f.used_g
        ));
    }
    info.push_str("  </plate>\n</config>\n");
    let mut files = vec![
        (
            "[Content_Types].xml".to_owned(),
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
 <Default Extension="png" ContentType="image/png"/>
 <Default Extension="gcode" ContentType="text/x.gcode"/>
</Types>
"#
            .to_vec(),
        ),
        (
            "_rels/.rels".to_owned(),
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/3dmodel.model" Id="rel-1" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"#
            .to_vec(),
        ),
        (
            "3D/3dmodel.model".to_owned(),
            br#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <metadata name="Application">CosmicSlicer</metadata>
 <resources/>
 <build/>
</model>
"#
            .to_vec(),
        ),
        ("Metadata/plate_1.gcode".to_owned(), gcode.to_vec()),
        ("Metadata/plate_1.gcode.md5".to_owned(), md5.into_bytes()),
        ("Metadata/slice_info.config".to_owned(), info.into_bytes()),
    ];
    if let Some(png) = thumbnail_png {
        files.push(("Metadata/plate_1.png".to_owned(), png.to_vec()));
    }
    CosmicSlicer_import::zip::write(&files).map_err(|e| e.to_string())
}

// ---- FTPS ----

/// A reply from the FTP server: code and text (multi-line replies joined).
fn ftp_reply(s: &mut Stream) -> io::Result<(u16, String)> {
    let mut text = String::new();
    let mut first_code: Option<String> = None;
    loop {
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while line.len() < 4096 {
            if s.read(&mut b)? == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the printer closed the file connection"));
            }
            if b[0] == b'\n' {
                break;
            }
            line.push(b[0]);
        }
        let line = String::from_utf8_lossy(&line).trim_end().to_owned();
        text.push_str(&line);
        text.push('\n');
        let code = line.get(..3).unwrap_or("").to_owned();
        let more = line.as_bytes().get(3) == Some(&b'-');
        match &first_code {
            None if more => first_code = Some(code),
            None => return Ok((code.parse().unwrap_or(0), text)),
            Some(c) if *c == code && !more => return Ok((code.parse().unwrap_or(0), text)),
            Some(_) => {}
        }
    }
}

fn ftp(s: &mut Stream, cmd: &str, want: &[u16]) -> Result<String, String> {
    net::send(s, format!("{cmd}\r\n").as_bytes()).map_err(|e| e.to_string())?;
    let (code, text) = ftp_reply(s).map_err(|e| e.to_string())?;
    if want.contains(&code) {
        Ok(text)
    } else {
        let shown = if cmd.starts_with("PASS") { "PASS" } else { cmd };
        Err(match code {
            530 => "the printer refused the access code".to_owned(),
            _ => format!("the printer answered {shown} with: {}", text.trim()),
        })
    }
}

/// The data port from a `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)` reply.
pub fn passive_port(reply: &str) -> Option<u16> {
    let inner = reply.split('(').nth(1)?.split(')').next()?;
    let n: Vec<u16> = inner.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    match n.as_slice() {
        [_, _, _, _, p1, p2] if *p1 < 256 && *p2 < 256 => Some(p1 * 256 + p2),
        _ => None,
    }
}

/// Writes `bytes` to the printer's storage as `name`, over implicit FTPS.
pub fn upload(p: &PhysicalPrinter, name: &str, bytes: &[u8]) -> Result<(), String> {
    let host = host(p)?;
    let timeout = Duration::from_secs(60);
    let mut c = net::connect(&host, FTPS_PORT, Some(Trust::Any), timeout)
        .map_err(|e| format!("could not reach the printer's storage (port {FTPS_PORT}): {e}"))?;
    let (code, text) = ftp_reply(&mut c).map_err(|e| e.to_string())?;
    if code != 220 {
        return Err(format!("the printer's storage is not ready: {}", text.trim()));
    }
    ftp(&mut c, "USER bblp", &[331, 230])?;
    ftp(&mut c, &format!("PASS {}", p.access_code.trim()), &[230])?;
    ftp(&mut c, "PBSZ 0", &[200])?;
    ftp(&mut c, "PROT P", &[200])?;
    ftp(&mut c, "TYPE I", &[200])?;
    let pasv = ftp(&mut c, "PASV", &[227])?;
    let port = passive_port(&pasv).ok_or("the printer gave no data port")?;
    let tcp = net::connect(&host, port, None, timeout).map_err(|e| e.to_string())?;
    net::send(&mut c, format!("STOR {name}\r\n").as_bytes()).map_err(|e| e.to_string())?;
    // The data connection is TLS too, resuming the control connection's session.
    let data = match tcp {
        Stream::Plain(t) => Stream::tls(t, &host, Trust::Any).map_err(|e| format!("file connection: {e}"))?,
        tls => tls,
    };
    let (code, text) = ftp_reply(&mut c).map_err(|e| e.to_string())?;
    if code != 150 && code != 125 {
        return Err(format!("the printer would not take the file: {}", text.trim()));
    }
    let mut data = data;
    data.write_all(bytes).map_err(|e| format!("sending the file: {e}"))?;
    if let Stream::Tls(s) = &mut data {
        s.conn.send_close_notify();
        let _ = s.flush();
    }
    drop(data);
    let (code, text) = ftp_reply(&mut c).map_err(|e| e.to_string())?;
    let _ = net::send(&mut c, b"QUIT\r\n");
    if code == 226 || code == 250 { Ok(()) } else { Err(format!("the file did not arrive whole: {}", text.trim())) }
}

fn host(p: &PhysicalPrinter) -> Result<String, String> {
    let a = p.address.trim().trim_start_matches("http://").trim_start_matches("https://");
    let a = a.split('/').next().unwrap_or("");
    let h = a.rsplit_once(':').map_or(a, |(h, _)| h).trim_matches(['[', ']']);
    if h.is_empty() { Err("the printer's IP address is empty".into()) } else { Ok(h.to_owned()) }
}

// ---- MQTT ----

/// A live connection to a printer's broker.
pub struct Session {
    client: crate::mqtt::Client<Stream>,
    serial: String,
    /// Everything reported so far.
    pub state: Value,
    last_ping: Instant,
}

impl Session {
    pub fn connect(p: &PhysicalPrinter) -> Result<Session, String> {
        if p.serial.trim().is_empty() || p.access_code.trim().is_empty() {
            return Err("a Bambu printer needs its serial number and access code".into());
        }
        let host = host(p)?;
        let stream = net::connect(&host, MQTT_PORT, Some(Trust::Any), Duration::from_secs(5))
            .map_err(|e| format!("could not reach the printer (port {MQTT_PORT}; is LAN mode on?): {e}"))?;
        let id = format!("cosmicslicer-{}", std::process::id());
        let mut client =
            crate::mqtt::Client::connect(stream, &id, "bblp", p.access_code.trim(), 30).map_err(|e| e.to_string())?;
        let serial = p.serial.trim().to_owned();
        client.subscribe(&report_topic(&serial)).map_err(|e| e.to_string())?;
        client.publish(&request_topic(&serial), pushall().to_string().as_bytes()).map_err(|e| e.to_string())?;
        // Short reads from here on, so the worker can check for commands between messages.
        let _ = client.stream().set_timeout(Duration::from_millis(250));
        Ok(Session { client, serial, state: json!({}), last_ping: Instant::now() })
    }

    /// Reads what the printer has sent; returns whether anything changed.
    pub fn pump(&mut self) -> Result<bool, String> {
        if self.last_ping.elapsed() > Duration::from_secs(15) {
            self.client.ping().map_err(|e| e.to_string())?;
            self.last_ping = Instant::now();
        }
        let mut changed = false;
        for _ in 0..64 {
            match self.client.poll().map_err(|e| e.to_string())? {
                Some((_, payload)) => {
                    if let Ok(v) = serde_json::from_slice::<Value>(&payload) {
                        merge(&mut self.state, &v);
                        changed = true;
                    }
                }
                None => break,
            }
        }
        Ok(changed)
    }

    pub fn send(&mut self, v: &Value) -> Result<(), String> {
        self.client.publish(&request_topic(&self.serial), v.to_string().as_bytes()).map_err(|e| e.to_string())
    }

    pub fn refresh(&mut self) -> Result<(), String> {
        self.send(&pushall())
    }
}

/// Uploads a sliced 3MF and starts it.
pub fn print(session: &mut Session, p: &PhysicalPrinter, name: &str, package: &[u8]) -> Result<(), String> {
    let name = crate::safe_name(name.trim_end_matches(".gcode.3mf").trim_end_matches(".gcode"), ".gcode.3mf");
    upload(p, &name, package)?;
    let md5 = crate::md5::hex(package);
    // Without a mapping set, filament n prints from AMS slot n.
    let mapping: Vec<i32> = if p.ams_mapping.is_empty() {
        (0..filament_count(package).max(1) as i32).collect()
    } else {
        p.ams_mapping.clone()
    };
    session.send(&start_print(&name, 1, &md5, p.use_ams, &mapping))
}

/// How many filaments a sliced 3MF lists.
fn filament_count(package: &[u8]) -> usize {
    CosmicSlicer_import::zip::Zip::open(package)
        .and_then(|z| z.read("Metadata/slice_info.config"))
        .map(|info| String::from_utf8_lossy(&info).matches("<filament ").count().min(16))
        .unwrap_or(1)
}

// ---- discovery ----

/// A printer that announced itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub ip: String,
    pub serial: String,
    pub name: String,
    pub model: String,
}

/// Reads one SSDP announcement from a Bambu printer.
pub fn parse_announcement(text: &str) -> Option<Found> {
    if !text.contains("bambulab") && !text.contains("bambu.com") {
        return None;
    }
    let mut f = Found::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().to_owned();
        match k.trim().to_ascii_lowercase().as_str() {
            "location" => f.ip = v,
            "usn" => f.serial = v,
            "devname.bambu.com" => f.name = v,
            "devmodel.bambu.com" => f.model = v,
            _ => {}
        }
    }
    (!f.ip.is_empty() && !f.serial.is_empty()).then_some(f)
}

/// Listens for printers' announcements for `wait`.
pub fn discover(wait: Duration) -> Result<Vec<Found>, String> {
    let sock = UdpSocket::bind(("0.0.0.0", SSDP_PORT))
        .map_err(|e| format!("could not listen for printers on port {SSDP_PORT}: {e}"))?;
    let _ = sock.join_multicast_v4(&std::net::Ipv4Addr::new(239, 255, 255, 250), &std::net::Ipv4Addr::UNSPECIFIED);
    let _ = sock.set_broadcast(true);
    sock.set_read_timeout(Some(Duration::from_millis(300))).map_err(|e| e.to_string())?;
    // Ask too, for printers that only answer searches.
    let search = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1990\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:bambulab-com:device:3dprinter:1\r\n\r\n";
    let _ = sock.send_to(search.as_bytes(), ("239.255.255.250", 1990));
    let _ = sock.send_to(search.as_bytes(), ("255.255.255.255", SSDP_PORT));
    let start = Instant::now();
    let mut found: Vec<Found> = Vec::new();
    let mut buf = [0u8; 2048];
    while start.elapsed() < wait && found.len() < 64 {
        if let Ok((n, _)) = sock.recv_from(&mut buf)
            && let Some(f) = parse_announcement(&String::from_utf8_lossy(buf.get(..n).unwrap_or(&[])))
            && !found.iter().any(|x| x.serial == f.serial)
        {
            found.push(f);
        }
    }
    Ok(found)
}

// ---- camera ----

/// The P1 and A1 camera: JPEG frames over TLS on port 6000. (The X1's camera is RTSP, which
/// CosmicSlicer does not show yet.)
pub struct Camera {
    stream: Stream,
}

impl Camera {
    pub fn connect(p: &PhysicalPrinter) -> Result<Camera, String> {
        let host = host(p)?;
        let mut stream = net::connect(&host, CAMERA_PORT, Some(Trust::Any), Duration::from_secs(5))
            .map_err(|e| format!("no camera on port {CAMERA_PORT} (X1 printers' cameras are not shown yet): {e}"))?;
        let mut auth = vec![0x40, 0, 0, 0, 0x00, 0x30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        for text in ["bblp", p.access_code.trim()] {
            let mut field = [0u8; 32];
            for (slot, b) in field.iter_mut().zip(text.bytes()) {
                *slot = b;
            }
            auth.extend_from_slice(&field);
        }
        net::send(&mut stream, &auth).map_err(|e| e.to_string())?;
        Ok(Camera { stream })
    }

    /// The next frame's JPEG bytes.
    pub fn frame(&mut self) -> Result<Vec<u8>, String> {
        let mut head = [0u8; 16];
        self.stream.read_exact(&mut head).map_err(|e| format!("camera: {e}"))?;
        let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        if len == 0 || len > 4 << 20 {
            return Err("camera: a bad frame".into());
        }
        let mut jpeg = vec![0u8; len];
        self.stream.read_exact(&mut jpeg).map_err(|e| format!("camera: {e}"))?;
        if jpeg.get(..2) != Some(&[0xFF, 0xD8]) {
            return Err("camera: not a picture (is the access code right?)".into());
        }
        Ok(jpeg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_merge_and_read() {
        let mut state = json!({});
        merge(
            &mut state,
            &json!({"print": {"gcode_state": "RUNNING", "mc_percent": 10, "nozzle_temper": 219.5, "nozzle_target_temper": 220, "bed_temper": 55.0, "bed_target_temper": 55, "subtask_name": "Benchy", "layer_num": 5, "total_layer_num": 120, "mc_remaining_time": 42, "cooling_fan_speed": "15", "spd_lvl": 2}}),
        );
        // A later report carries only what changed.
        merge(&mut state, &json!({"print": {"mc_percent": 11}}));
        let s = status(&state);
        assert_eq!(
            (s.state, s.file.as_deref(), s.layer, s.speed),
            (State::Printing, Some("Benchy"), Some((5, 120)), Some(2))
        );
        assert!(s.progress.is_some_and(|p| (p - 0.11).abs() < 1e-6));
        assert_eq!(s.remaining_s, Some(42.0 * 60.0));
        assert_eq!(s.fan, Some(1.0));
        assert_eq!(s.temps.first().map(|t| t.target), Some(220.0));
        assert_eq!(status(&json!("junk")).state, State::Unknown);
    }

    #[test]
    fn commands_and_print_requests() {
        assert_eq!(command(&Command::Pause), Some(json!({"print": {"sequence_id": "0", "command": "pause"}})));
        let g = command(&Command::Jog('Z', 1.0));
        assert_eq!(g.as_ref().and_then(|v| v["print"]["param"].as_str()), Some("G91\nG1 Z1.00 F600\nG90\n"));
        let s = start_print("Benchy.gcode.3mf", 1, "abc", true, &[0, 2]);
        assert_eq!(s["print"]["url"], "ftp://Benchy.gcode.3mf");
        assert_eq!(s["print"]["param"], "Metadata/plate_1.gcode");
        assert_eq!(s["print"]["ams_mapping"], json!([0, 2]));
    }

    #[test]
    fn the_sliced_3mf_holds_the_gcode_and_its_checksum() -> Result<(), String> {
        let gcode = b"G28\nG1 X10\n";
        let f = Filament { kind: "PLA".into(), colour: "#FF0000".into(), used_m: 1.5, used_g: 4.5 };
        let bytes = sliced_3mf(gcode, None, 600.0, &[f])?;
        let zip = CosmicSlicer_import::zip::Zip::open(&bytes).map_err(|e| e.to_string())?;
        assert_eq!(zip.read("Metadata/plate_1.gcode").map_err(|e| e.to_string())?, gcode);
        let md5 = zip.read("Metadata/plate_1.gcode.md5").map_err(|e| e.to_string())?;
        assert_eq!(String::from_utf8_lossy(&md5), crate::md5::hex(gcode).to_ascii_uppercase());
        let info =
            String::from_utf8_lossy(&zip.read("Metadata/slice_info.config").map_err(|e| e.to_string())?).into_owned();
        assert!(info.contains("prediction\" value=\"600\"") && info.contains("type=\"PLA\""));
        assert_eq!(filament_count(&bytes), 1);
        Ok(())
    }

    #[test]
    fn announcements_and_passive_ports() {
        let text = "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nLocation: 192.168.1.20\r\nNT: urn:bambulab-com:device:3dprinter:1\r\nUSN: 01S00A123456789\r\nDevModel.bambu.com: C11\r\nDevName.bambu.com: P1P\r\n\r\n";
        let f = parse_announcement(text);
        assert_eq!(
            f.map(|f| (f.ip, f.serial, f.name)),
            Some(("192.168.1.20".into(), "01S00A123456789".into(), "P1P".into()))
        );
        assert!(parse_announcement("NOTIFY * HTTP/1.1\r\nLocation: 1.2.3.4\r\n").is_none());
        assert_eq!(passive_port("227 Entering Passive Mode (192,168,1,20,195,80)."), Some(195 * 256 + 80));
        assert_eq!(passive_port("227 nonsense"), None);
    }

    #[test]
    fn a_bambu_printer_needs_its_code() {
        let p = PhysicalPrinter { host: crate::HostType::BambuLan, address: "127.0.0.1".into(), ..Default::default() };
        assert!(Session::connect(&p).is_err());
    }
}
