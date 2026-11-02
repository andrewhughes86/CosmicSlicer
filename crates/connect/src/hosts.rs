//! OrcaSlicer's print hosts over HTTP: test the connection, list storage, upload (and start)
//! a print, read the status and send commands. Bambu printers are in `bambu`.

use std::time::Duration;

use serde_json::{Value, json};

use crate::http::{Auth, Multipart, Request, Response, Url, encode};
use crate::{Command, HostType, PhysicalPrinter, State, Status, Storage, Temp};

/// Uploads may take a while on a slow printer.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Which API an "Octo/Klipper" host turned out to speak.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Flavour {
    #[default]
    OctoPrint,
    Moonraker,
}

fn base(p: &PhysicalPrinter) -> Result<Url, String> {
    Url::parse(&p.address)
}

fn auth(p: &PhysicalPrinter) -> Auth {
    match p.host {
        HostType::PrusaLink if !p.user.is_empty() => Auth::User(p.user.clone(), p.password.clone()),
        _ if !p.api_key.is_empty() => Auth::ApiKey(p.api_key.clone()),
        _ => Auth::None,
    }
}

fn request(p: &PhysicalPrinter, method: &'static str, path: &str) -> Result<Request, String> {
    let mut r = Request::new(method, base(p)?.with_path(path)).auth(auth(p));
    r.trust = p.trust();
    Ok(r)
}

fn get(p: &PhysicalPrinter, path: &str) -> Result<Response, String> {
    request(p, "GET", path)?.send()
}

fn get_json(p: &PhysicalPrinter, path: &str) -> Result<Value, String> {
    get(p, path)?.json()
}

/// Whether an Octo/Klipper host is OctoPrint or Moonraker.
pub fn flavour(p: &PhysicalPrinter) -> Flavour {
    if p.host == HostType::OctoPrint && get_json(p, "/server/info").is_ok_and(|v| v.get("result").is_some()) {
        Flavour::Moonraker
    } else {
        Flavour::OctoPrint
    }
}

/// Checks the printer answers and the key is right; returns what it says it is.
pub fn test(p: &PhysicalPrinter) -> Result<String, String> {
    if let Some(why) = p.host.unsupported() {
        return Err(format!("{}: {why}", p.host.label()));
    }
    match p.host {
        HostType::OctoPrint | HostType::AstroBox => {
            if flavour(p) == Flavour::Moonraker {
                let v = get_json(p, "/server/info")?;
                let state = v["result"]["klippy_state"].as_str().unwrap_or("unknown");
                return Ok(format!("Klipper (Moonraker), Klipper is {state}"));
            }
            let v = get_json(p, "/api/version")?;
            Ok(v["text"].as_str().map_or_else(|| "OctoPrint".to_owned(), str::to_owned))
        }
        HostType::PrusaLink => {
            let v = get_json(p, "/api/version")?;
            Ok(format!("PrusaLink {}", v["server"].as_str().or(v["text"].as_str()).unwrap_or("")))
        }
        HostType::Duet => {
            let v = duet_connect(p)?;
            Ok(format!("Duet, board {}", v["boardType"].as_str().unwrap_or("connected")))
        }
        HostType::Repetier => {
            let v = get_json(p, &format!("/printer/info?apikey={}", encode(&p.api_key)))?;
            Ok(format!("Repetier-Server {}", v["version"].as_str().unwrap_or("")))
        }
        HostType::FlashAir => {
            let t = get(p, "/command.cgi?op=118")?.text();
            if t.trim() == "1" {
                Ok("FlashAir, uploads enabled".into())
            } else {
                Err("the FlashAir card does not allow uploads (set UPLOAD=1 in its CONFIG)".into())
            }
        }
        HostType::Esp3d => {
            get(p, "/command?plain=M115")?;
            Ok("ESP3D".into())
        }
        HostType::Mks => {
            get(p, "/")?;
            Ok("MKS WiFi".into())
        }
        _ => Err(format!("{}: not supported", p.host.label())),
    }
}

/// The places the printer can store files, for hosts that have more than one.
pub fn storages(p: &PhysicalPrinter) -> Result<Vec<Storage>, String> {
    match p.host {
        HostType::PrusaLink => {
            let v = get_json(p, "/api/v1/storage")?;
            let list = v["storage_list"].as_array().cloned().unwrap_or_default();
            Ok(list
                .iter()
                .filter(|s| s["available"].as_bool().unwrap_or(true) && !s["read_only"].as_bool().unwrap_or(false))
                .filter_map(|s| {
                    let path = s["path"].as_str()?.trim_matches('/').to_owned();
                    let name = s["name"].as_str().unwrap_or(&path).to_owned();
                    Some(Storage { id: path, name })
                })
                .take(16)
                .collect())
        }
        HostType::OctoPrint if flavour(p) == Flavour::OctoPrint => Ok(vec![
            Storage { id: "local".into(), name: "OctoPrint".into() },
            Storage { id: "sdcard".into(), name: "Printer's SD card".into() },
        ]),
        _ => Ok(Vec::new()),
    }
}

/// Sends `gcode` as `name`; with `start`, the printer prints it at once.
pub fn upload(p: &PhysicalPrinter, name: &str, gcode: &[u8], start: bool) -> Result<(), String> {
    if let Some(why) = p.host.unsupported() {
        return Err(format!("{}: {why}", p.host.label()));
    }
    let name = crate::safe_name(name, ".gcode");
    let send = |r: Request| r.timeout(UPLOAD_TIMEOUT).send().map(|_| ());
    match p.host {
        HostType::OctoPrint | HostType::AstroBox => {
            if p.host == HostType::OctoPrint && flavour(p) == Flavour::Moonraker {
                let form =
                    Multipart::new().field("root", "gcodes").field("print", if start { "true" } else { "false" });
                return send(request(p, "POST", "/server/files/upload")?.multipart(form.file("file", &name, gcode)));
            }
            let storage = if p.storage == "sdcard" { "sdcard" } else { "local" };
            let form = Multipart::new()
                .field("select", if start { "true" } else { "false" })
                .field("print", if start { "true" } else { "false" })
                .file("file", &name, gcode);
            send(request(p, "POST", &format!("/api/files/{storage}"))?.multipart(form))
        }
        HostType::PrusaLink => {
            let storage = if p.storage.is_empty() { "usb".to_owned() } else { p.storage.clone() };
            let r = request(p, "PUT", &format!("/api/v1/files/{}/{}", encode(&storage), encode(&name)))?
                .header("Print-After-Upload", if start { "?1" } else { "?0" })
                .header("Overwrite", "?1")
                .body("text/x.gcode", gcode.to_vec());
            send(r)
        }
        HostType::Duet => {
            duet_connect(p)?;
            let path = format!("0:/gcodes/{name}");
            send(
                request(p, "POST", &format!("/rr_upload?name={}", encode(&path)))?
                    .body("application/octet-stream", gcode.to_vec()),
            )?;
            if start {
                get(p, &format!("/rr_gcode?gcode={}", encode(&format!("M32 \"{path}\""))))?;
            }
            Ok(())
        }
        HostType::Repetier => {
            if p.slug.trim().is_empty() {
                return Err("Repetier needs the printer's name (slug) on the server".into());
            }
            let what = if start { "job" } else { "model" };
            let path = format!("/printer/{what}/{}?a=upload&apikey={}", encode(p.slug.trim()), encode(&p.api_key));
            send(
                request(p, "POST", &path)?
                    .multipart(Multipart::new().field("name", &name).file("filename", &name, gcode)),
            )
        }
        HostType::FlashAir => {
            get(p, "/upload.cgi?WRITEPROTECT=ON&UPDIR=/")?;
            send(request(p, "POST", "/upload.cgi")?.multipart(Multipart::new().file("file", &name, gcode)))?;
            if start {
                return Err("uploaded; FlashAir cannot start a print".into());
            }
            Ok(())
        }
        HostType::Esp3d => {
            send(
                request(p, "POST", "/upload")?
                    .multipart(Multipart::new().field("path", "/").file("myfile[]", &name, gcode)),
            )?;
            if start {
                get(p, &format!("/command?commandText={}", encode(&format!("M23 {name}\nM24"))))?;
            }
            Ok(())
        }
        HostType::Mks => {
            send(
                request(p, "POST", &format!("/upload?X-Filename={}", encode(&name)))?
                    .body("application/octet-stream", gcode.to_vec()),
            )?;
            if start {
                mks_command(p, &format!("M23 {name}\r\nM24\r\n"))?;
            }
            Ok(())
        }
        _ => Err(format!("{}: not supported", p.host.label())),
    }
}

fn duet_connect(p: &PhysicalPrinter) -> Result<Value, String> {
    let v = get_json(p, &format!("/rr_connect?password={}", encode(&p.api_key)))?;
    match v["err"].as_i64() {
        Some(0) | None => Ok(v),
        Some(1) => Err("the Duet refused the password".into()),
        Some(e) => Err(format!("the Duet answered error {e}")),
    }
}

/// MKS boards take commands on TCP port 8080.
fn mks_command(p: &PhysicalPrinter, text: &str) -> Result<(), String> {
    let url = base(p)?;
    let mut s = crate::net::connect(&url.host, 8080, None, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    crate::net::send(&mut s, text.as_bytes()).map_err(|e| e.to_string())
}

fn f32_of(v: &Value) -> Option<f32> {
    v.as_f64().map(|x| x as f32).filter(|x| x.is_finite())
}

/// The printer's state, for hosts the Device page follows.
pub fn status(p: &PhysicalPrinter, flavour: Flavour) -> Result<Status, String> {
    match (p.host, flavour) {
        (HostType::OctoPrint, Flavour::Moonraker) => moonraker_status(p),
        (HostType::OctoPrint, Flavour::OctoPrint) => octoprint_status(p),
        (HostType::PrusaLink, _) => prusalink_status(p),
        _ => Err(format!("{} cannot report its state to CosmicSlicer", p.host.label())),
    }
}

fn moonraker_status(p: &PhysicalPrinter) -> Result<Status, String> {
    let v = get_json(
        p,
        "/printer/objects/query?extruder=temperature,target&heater_bed=temperature,target&print_stats&display_status=progress&virtual_sdcard=progress&fan=speed",
    )?;
    parse_moonraker(&v["result"]["status"])
}

/// Reads Moonraker's `printer.objects.query` status.
pub fn parse_moonraker(s: &Value) -> Result<Status, String> {
    if !s.is_object() {
        return Err("Klipper did not report its state (is it running?)".into());
    }
    let ps = &s["print_stats"];
    let detail = ps["state"].as_str().unwrap_or("").to_owned();
    let state = match detail.as_str() {
        "standby" => State::Idle,
        "printing" => State::Printing,
        "paused" => State::Paused,
        "complete" => State::Finished,
        "cancelled" => State::Failed,
        "error" => State::Error,
        _ => State::Unknown,
    };
    let progress = f32_of(&s["display_status"]["progress"]).or(f32_of(&s["virtual_sdcard"]["progress"]));
    let mut temps = Vec::new();
    for (key, name) in [("extruder", "Nozzle"), ("heater_bed", "Bed")] {
        if let (Some(a), Some(t)) = (f32_of(&s[key]["temperature"]), f32_of(&s[key]["target"])) {
            temps.push(Temp { name: name.into(), actual: a, target: t });
        }
    }
    let duration = ps["print_duration"].as_f64().filter(|d| d.is_finite());
    let remaining = match (duration, progress) {
        (Some(d), Some(f)) if f > 0.01 && f < 1.0 => Some(d / f as f64 * (1.0 - f as f64)),
        _ => None,
    };
    let info = &ps["info"];
    let layer = match (info["current_layer"].as_u64(), info["total_layer"].as_u64()) {
        (Some(c), Some(t)) => Some((c.min(u32::MAX as u64) as u32, t.min(u32::MAX as u64) as u32)),
        _ => None,
    };
    Ok(Status {
        state,
        detail,
        progress: progress.map(|f| f.clamp(0.0, 1.0)),
        file: ps["filename"].as_str().filter(|f| !f.is_empty()).map(str::to_owned),
        layer,
        remaining_s: remaining,
        temps,
        fan: f32_of(&s["fan"]["speed"]).map(|f| f.clamp(0.0, 1.0)),
        speed: None,
        camera: true,
    })
}

fn octoprint_status(p: &PhysicalPrinter) -> Result<Status, String> {
    let printer = get(p, "/api/printer")?;
    // 409: OctoPrint is up but the printer is not connected to it.
    let printer = if printer.status == 409 { Value::Null } else { printer.json()? };
    let job = get_json(p, "/api/job")?;
    Ok(parse_octoprint(&printer, &job))
}

/// Reads OctoPrint's `/api/printer` and `/api/job`.
pub fn parse_octoprint(printer: &Value, job: &Value) -> Status {
    let detail = job["state"].as_str().unwrap_or("Offline").to_owned();
    let flags = &printer["state"]["flags"];
    let state = if flags["error"].as_bool() == Some(true) {
        State::Error
    } else if flags["paused"].as_bool() == Some(true) || flags["pausing"].as_bool() == Some(true) {
        State::Paused
    } else if flags["printing"].as_bool() == Some(true) {
        State::Printing
    } else if flags["ready"].as_bool() == Some(true) || flags["operational"].as_bool() == Some(true) {
        State::Idle
    } else {
        State::Unknown
    };
    let mut temps = Vec::new();
    for (key, name) in [("tool0", "Nozzle"), ("bed", "Bed")] {
        let t = &printer["temperature"][key];
        if let (Some(a), Some(g)) = (f32_of(&t["actual"]), f32_of(&t["target"])) {
            temps.push(Temp { name: name.into(), actual: a, target: g });
        }
    }
    Status {
        state,
        detail,
        progress: f32_of(&job["progress"]["completion"]).map(|c| (c / 100.0).clamp(0.0, 1.0)),
        file: job["job"]["file"]["name"].as_str().map(str::to_owned),
        layer: None,
        remaining_s: job["progress"]["printTimeLeft"].as_f64().filter(|t| t.is_finite() && *t >= 0.0),
        temps,
        fan: None,
        speed: None,
        camera: true,
    }
}

fn prusalink_status(p: &PhysicalPrinter) -> Result<Status, String> {
    Ok(parse_prusalink(&get_json(p, "/api/v1/status")?))
}

/// Reads PrusaLink's `/api/v1/status`.
pub fn parse_prusalink(v: &Value) -> Status {
    let pr = &v["printer"];
    let detail = pr["state"].as_str().unwrap_or("").to_owned();
    let state = match detail.as_str() {
        "IDLE" | "READY" => State::Idle,
        "BUSY" => State::Preparing,
        "PRINTING" => State::Printing,
        "PAUSED" => State::Paused,
        "FINISHED" => State::Finished,
        "STOPPED" => State::Failed,
        "ERROR" | "ATTENTION" => State::Error,
        _ => State::Unknown,
    };
    let mut temps = Vec::new();
    for (a, t, name) in [("temp_nozzle", "target_nozzle", "Nozzle"), ("temp_bed", "target_bed", "Bed")] {
        if let (Some(a), Some(t)) = (f32_of(&pr[a]), f32_of(&pr[t])) {
            temps.push(Temp { name: name.into(), actual: a, target: t });
        }
    }
    let job = &v["job"];
    Status {
        state,
        detail,
        progress: f32_of(&job["progress"]).map(|c| (c / 100.0).clamp(0.0, 1.0)),
        file: None,
        layer: None,
        remaining_s: job["time_remaining"].as_f64().filter(|t| t.is_finite() && *t >= 0.0),
        temps,
        fan: f32_of(&pr["fan_print"]).map(|f| (f / 100.0).clamp(0.0, 1.0)),
        speed: None,
        camera: true,
    }
}

/// Sends `cmd` to a printer the Device page follows.
pub fn control(p: &PhysicalPrinter, flavour: Flavour, cmd: &Command) -> Result<(), String> {
    match (p.host, flavour) {
        (HostType::OctoPrint, Flavour::Moonraker) => {
            let path = match cmd {
                Command::Pause => "/printer/print/pause".to_owned(),
                Command::Resume => "/printer/print/resume".to_owned(),
                Command::Stop => "/printer/print/cancel".to_owned(),
                other => {
                    let g = other.gcode().ok_or("Klipper has no such command")?;
                    format!("/printer/gcode/script?script={}", encode(&g))
                }
            };
            request(p, "POST", &path)?.send().map(|_| ())
        }
        (HostType::OctoPrint, Flavour::OctoPrint) => {
            let (path, body) = match cmd {
                Command::Pause => ("/api/job", json!({"command": "pause", "action": "pause"})),
                Command::Resume => ("/api/job", json!({"command": "pause", "action": "resume"})),
                Command::Stop => ("/api/job", json!({"command": "cancel"})),
                other => {
                    let g = other.gcode().ok_or("OctoPrint has no such command")?;
                    let lines: Vec<&str> = g.lines().collect();
                    ("/api/printer/command", json!({ "commands": lines }))
                }
            };
            request(p, "POST", path)?.json(&body).send().map(|_| ())
        }
        (HostType::PrusaLink, _) => {
            let v = get_json(p, "/api/v1/status")?;
            let id = v["job"]["id"].as_u64().ok_or("PrusaLink has no print running")?;
            let (method, path) = match cmd {
                Command::Pause => ("PUT", format!("/api/v1/job/{id}/pause")),
                Command::Resume => ("PUT", format!("/api/v1/job/{id}/resume")),
                Command::Stop => ("DELETE", format!("/api/v1/job/{id}")),
                _ => return Err("PrusaLink only pauses, resumes and stops prints from here".into()),
            };
            request(p, method, &path)?.send().map(|_| ())
        }
        _ => Err(format!("{} cannot be controlled from CosmicSlicer", p.host.label())),
    }
}

/// A still from the printer's camera (JPEG or PNG bytes).
pub fn snapshot(p: &PhysicalPrinter, flavour: Flavour) -> Result<Vec<u8>, String> {
    let path = match (p.host, flavour) {
        (HostType::OctoPrint, Flavour::Moonraker) => {
            // The first webcam Moonraker knows, else the usual crowsnest address.
            let listed = get_json(p, "/server/webcams/list").ok().and_then(|v| {
                v["result"]["webcams"].as_array()?.iter().find_map(|w| w["snapshot_url"].as_str().map(str::to_owned))
            });
            listed.unwrap_or_else(|| "/webcam/?action=snapshot".into())
        }
        (HostType::OctoPrint, Flavour::OctoPrint) => "/webcam/?action=snapshot".into(),
        (HostType::PrusaLink, _) => "/api/v1/cameras/snap".into(),
        _ => return Err("no camera".into()),
    };
    let r = if path.starts_with("http://") || path.starts_with("https://") {
        let mut r = Request::new("GET", Url::parse(&path)?);
        r.trust = p.trust();
        r.send()?
    } else {
        get(p, &path)?
    };
    if r.status == 204 {
        return Err("the camera has no picture yet".into());
    }
    Ok(r.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::{ok_json, serve};

    #[test]
    fn moonraker_uploads_and_reports() -> Result<(), String> {
        let (addr, rx) = serve(4, |head, _| {
            if head.starts_with("GET /server/info") {
                ok_json(r#"{"result":{"klippy_state":"ready"}}"#)
            } else if head.starts_with("POST /server/files/upload") {
                ok_json(r#"{"result":{"item":{"path":"a.gcode"}}}"#)
            } else if head.starts_with("GET /printer/objects/query") {
                ok_json(
                    r#"{"result":{"status":{"extruder":{"temperature":210.5,"target":215},"heater_bed":{"temperature":60,"target":60},
                    "print_stats":{"state":"printing","filename":"a.gcode","print_duration":600,"info":{"current_layer":12,"total_layer":100}},
                    "display_status":{"progress":0.25},"fan":{"speed":0.5}}}}"#,
                )
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()
            }
        });
        let p = PhysicalPrinter { host: HostType::OctoPrint, address: addr, ..Default::default() };
        assert_eq!(flavour(&p), Flavour::Moonraker);
        upload(&p, "My Part.gcode", b"G28\n", true)?;
        let s = status(&p, Flavour::Moonraker)?;
        assert_eq!(s.state, State::Printing);
        assert_eq!(s.layer, Some((12, 100)));
        assert!(s.remaining_s.is_some_and(|r| (r - 1800.0).abs() < 1.0));
        assert_eq!(s.temps.first().map(|t| t.target), Some(215.0));
        let seen: Vec<(String, Vec<u8>)> = rx.try_iter().collect();
        let up = seen.iter().find(|(h, _)| h.starts_with("POST"));
        assert!(up.is_some_and(|(_, body)| {
            let b = String::from_utf8_lossy(body);
            b.contains("filename=\"My Part.gcode\"") && b.contains("G28") && b.contains("name=\"print\"\r\n\r\ntrue")
        }));
        Ok(())
    }

    #[test]
    fn octoprint_and_prusalink_states_read() {
        let printer = json!({"state":{"flags":{"printing":true}},"temperature":{"tool0":{"actual":200.0,"target":205.0},"bed":{"actual":55.0,"target":55.0}}});
        let job = json!({"state":"Printing","job":{"file":{"name":"x.gcode"}},"progress":{"completion":42.0,"printTimeLeft":300}});
        let s = parse_octoprint(&printer, &job);
        assert_eq!((s.state, s.file.as_deref(), s.temps.len()), (State::Printing, Some("x.gcode"), 2));
        assert!(s.progress.is_some_and(|p| (p - 0.42).abs() < 1e-4));
        let s = parse_prusalink(
            &json!({"printer":{"state":"PAUSED","temp_nozzle":215,"target_nozzle":215},"job":{"progress":50,"time_remaining":60}}),
        );
        assert_eq!((s.state, s.remaining_s), (State::Paused, Some(60.0)));
        // Garbage is an unknown state, not a crash.
        assert_eq!(parse_prusalink(&json!("x")).state, State::Unknown);
        assert!(parse_moonraker(&json!(null)).is_err());
    }

    #[test]
    fn prusalink_puts_the_file_on_its_storage() -> Result<(), String> {
        let (addr, rx) = serve(1, |_, _| b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n".to_vec());
        let p = PhysicalPrinter { host: HostType::PrusaLink, address: addr, api_key: "k".into(), ..Default::default() };
        upload(&p, "plate 1", b"G28\n", true)?;
        let (head, body) = rx.recv_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        assert!(head.starts_with("PUT /api/v1/files/usb/plate%201.gcode"), "{head}");
        assert!(head.contains("Print-After-Upload: ?1") && head.contains("X-Api-Key: k"));
        assert_eq!(body, b"G28\n");
        Ok(())
    }

    #[test]
    fn a_printer_that_is_not_there_is_an_error() {
        // Nothing listens on port 9 of the loopback.
        let p = PhysicalPrinter { host: HostType::OctoPrint, address: "127.0.0.1:9".into(), ..Default::default() };
        assert!(test(&p).is_err());
        let p = PhysicalPrinter { host: HostType::Obico, address: "x".into(), ..Default::default() };
        assert!(upload(&p, "a", b"", false).is_err());
    }
}
