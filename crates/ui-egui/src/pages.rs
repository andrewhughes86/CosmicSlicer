//! The pages without a 3D canvas: Home, Device, Multi-device and Project.

use egui::{Align, Layout, RichText, Sense, Stroke, Ui, vec2};

use CosmicSlicer_connect::{Command, Heater, HostType, PhysicalPrinter, State, Storage, bambu};

use crate::Action;
use crate::devices::Job;
use crate::icons;
use crate::state::{Page, Session};
use crate::theme::Tokens;
use crate::ui_kit::{self, Surface};

/// The Device pages' and printer dialog's state.
pub struct DeviceState {
    pub devices: crate::devices::Devices,
    /// The printer in the printer dialog: its place in the list (`None` for a new one) and
    /// its settings as edited.
    pub edit: Option<(Option<usize>, PhysicalPrinter)>,
    pub test: Option<Job<Result<String, String>>>,
    pub test_result: Option<Result<String, String>>,
    pub storages: Option<Job<Result<Vec<Storage>, String>>>,
    pub storage_list: Vec<Storage>,
    pub discover: Option<Job<Result<Vec<bambu::Found>, String>>>,
    pub found: Result<Vec<bambu::Found>, String>,
    pub nozzle_target: f32,
    pub bed_target: f32,
    pub fan: f32,
    pub jog_step: usize,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            devices: Default::default(),
            edit: None,
            test: None,
            test_result: None,
            storages: None,
            storage_list: Vec::new(),
            discover: None,
            found: Ok(Vec::new()),
            nozzle_target: 0.0,
            bed_target: 0.0,
            fan: 0.0,
            jog_step: 1,
        }
    }
}

// ---- Home ----

pub fn home(ui: &mut Ui, session: &mut Session, t: &Tokens, actions: &mut Vec<Action>) {
    let full = ui.available_rect_before_wrap();
    let nav = egui::Rect::from_min_size(full.min, vec2(220.0, full.height()));
    ui.painter().rect_filled(nav, 0.0, t.panel_bg);
    ui.painter().vline(nav.right(), nav.y_range(), Stroke::new(1.0, t.border));
    let mut nav_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(nav.shrink(16.0)).layout(Layout::top_down(Align::Min)));
    nav_ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
        icons::paint_original(ui, r, "cosmic_logo", t);
        ui.label(RichText::new("CosmicSlicer").size(18.0).strong().color(t.text));
    });
    nav_ui.add_space(20.0);
    let item = |ui: &mut Ui, icon: &str, label: &str| -> bool {
        let (r, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(r, 6.0, t.hover_bg);
        }
        icons::paint(
            ui,
            egui::Rect::from_center_size(r.left_center() + vec2(18.0, 0.0), vec2(18.0, 18.0)),
            icon,
            t,
            true,
        );
        ui.painter().text(
            r.left_center() + vec2(36.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(14.0),
            t.text,
        );
        response.clicked()
    };
    if item(&mut nav_ui, "add", "New Project") {
        actions.push(Action::NewProject);
        session.page = Page::Prepare;
    }
    if item(&mut nav_ui, "open_project", "Open Project") {
        actions.push(Action::OpenProject);
    }
    if item(&mut nav_ui, "toolbar_open", "Import Model") {
        actions.push(Action::Import);
    }
    nav_ui.add_space(12.0);
    nav_ui.separator();
    nav_ui.add_space(8.0);
    if item(&mut nav_ui, "custom-gcode_note", "Manuals") {
        actions.push(Action::OpenUrl("https://github.com/SoftFever/OrcaSlicer/wiki"));
    }
    if item(&mut nav_ui, "open_in_browser", "Online Models") {
        actions.push(Action::OpenUrl("https://www.printables.com"));
    }

    let content = egui::Rect::from_min_max(nav.right_top() + vec2(28.0, 24.0), full.max - vec2(28.0, 24.0));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(content).layout(Layout::top_down(Align::Min)));
    c.label(RichText::new("Recently opened").size(20.0).strong());
    c.add_space(14.0);
    let recent = session.prefs.recent.clone();
    if recent.is_empty() {
        c.add_space(40.0);
        c.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(120.0, 120.0), Sense::hover());
            icons::paint_original(ui, r, "cosmic_logo", t);
            ui.add_space(12.0);
            ui.label(RichText::new("Welcome to CosmicSlicer").size(22.0).strong());
            ui.label(RichText::new("Start a project, or bring in a model to print.").color(t.text_muted));
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                let w = 320.0;
                ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                if ui_kit::primary_button(ui, t, "  New Project  ").clicked() {
                    actions.push(Action::NewProject);
                    session.page = Page::Prepare;
                }
                ui.add_space(10.0);
                if ui.button("  Import Model…  ").clicked() {
                    actions.push(Action::Import);
                }
            });
        });
        return;
    }
    egui::ScrollArea::vertical().show(&mut c, |ui| {
        let card_w = 220.0;
        let cols = ((ui.available_width() + 16.0) / (card_w + 16.0)).floor().max(1.0) as usize;
        egui::Grid::new("recent-grid").spacing([16.0, 16.0]).show(ui, |ui| {
            for (i, path) in recent.iter().enumerate() {
                let (r, response) = ui.allocate_exact_size(vec2(card_w, 190.0), Sense::click());
                let hovered = response.hovered();
                ui.painter().rect_filled(r, 8.0, t.panel_bg);
                ui.painter().rect_stroke(
                    r,
                    8.0,
                    Stroke::new(1.0, if hovered { t.accent } else { t.border }),
                    egui::StrokeKind::Inside,
                );
                let thumb = egui::Rect::from_min_size(r.min + vec2(10.0, 10.0), vec2(card_w - 20.0, 120.0));
                ui.painter().rect_filled(thumb, 6.0, t.raised_bg);
                match project_thumbnail(ui.ctx(), path) {
                    Some(tex) => {
                        // Fit the square image in the card, centred.
                        let side = thumb.height().min(thumb.width());
                        let img = egui::Rect::from_center_size(thumb.center(), vec2(side, side));
                        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                        ui.painter().image(tex.id(), img, uv, egui::Color32::WHITE);
                    }
                    None => icons::paint(
                        ui,
                        egui::Rect::from_center_size(thumb.center(), vec2(56.0, 56.0)),
                        "menu_obj_cube",
                        t,
                        true,
                    ),
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                ui.painter().text(
                    r.left_top() + vec2(12.0, 142.0),
                    egui::Align2::LEFT_TOP,
                    name,
                    egui::FontId::proportional(13.5),
                    t.text,
                );
                let dir = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                let dir = if dir.len() > 30 { format!("…{}", &dir[dir.len().saturating_sub(29)..]) } else { dir };
                ui.painter().text(
                    r.left_top() + vec2(12.0, 162.0),
                    egui::Align2::LEFT_TOP,
                    dir,
                    egui::FontId::proportional(11.5),
                    t.text_muted,
                );
                if response.on_hover_text(path.display().to_string()).clicked() {
                    actions.push(Action::OpenProjectPath(path.clone()));
                    session.page = Page::Prepare;
                }
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
    });
}

/// A project's plate thumbnail, read once per file and kept for the session.
fn project_thumbnail(ctx: &egui::Context, path: &std::path::Path) -> Option<egui::TextureHandle> {
    let ext_ok = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("3mf"));
    if !ext_ok {
        return None;
    }
    let id = egui::Id::new(("recent-thumbnail", path));
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        return cached;
    }
    let tex = crate::project::thumbnail(path)
        .and_then(|png| decode_png(&png))
        .map(|image| ctx.load_texture(format!("recent-{}", path.display()), image, egui::TextureOptions::LINEAR));
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

/// An 8-bit RGBA or RGB PNG as an image; `None` for anything else or a damaged file.
fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let (w, h) = (info.width as usize, info.height as usize);
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let data = buf.get(..frame.buffer_size())?;
    let rgba: Vec<u8> = match (frame.color_type, frame.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => data.to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            data.as_chunks::<3>().0.iter().flat_map(|[r, g, b]| [*r, *g, *b, 255]).collect()
        }
        _ => return None,
    };
    (rgba.len() == w * h * 4).then(|| egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

// ---- Device ----

fn card<R>(ui: &mut Ui, t: &Tokens, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui_kit::show(Surface::Card, ui, t, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong().size(14.5));
        ui.add_space(6.0);
        add(ui)
    })
    .inner
}

fn hms(secs: f64) -> String {
    let s = secs.clamp(0.0, 1e7) as u64;
    if s >= 3600 { format!("{}h {:02}m", s / 3600, s / 60 % 60) } else { format!("{}m {:02}s", s / 60, s % 60) }
}

/// A line saying what the printer is doing, for headers and the printer list.
fn state_line(d: &DeviceState, p: &PhysicalPrinter) -> (String, bool) {
    let info = d.devices.info.get(&p.name);
    if let Some(why) = info.and_then(|i| i.offline.as_ref()) {
        return (format!("Offline: {why}"), false);
    }
    match info.and_then(|i| i.status.as_ref()) {
        Some(s) => {
            let mut text = s.state.label().to_owned();
            if let Some(f) = s.progress.filter(|_| s.state.busy()) {
                text.push_str(&format!(" · {:.0}%", f * 100.0));
            }
            (text, true)
        }
        None if !p.host.live() => ("Takes uploads (no live status for this host type)".into(), true),
        None => ("Connecting…".into(), true),
    }
}

pub fn device(ui: &mut Ui, session: &mut Session, t: &Tokens, d: &mut DeviceState, actions: &mut Vec<Action>) {
    let Some(p) = session.active_printer().cloned() else {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
            icons::paint(ui, r, "monitor_none_printer", t, true);
            ui.label(RichText::new("No printer is set up").size(17.0).strong());
            ui.label(
                RichText::new(
                    "Add your printer's network connection (Klipper, OctoPrint, PrusaLink, a Bambu Lab printer in LAN mode, …) to send plates to it and follow its prints here.",
                )
                .color(t.text_muted),
            );
            ui.add_space(8.0);
            if ui_kit::primary_button(ui, t, "Add printer").clicked() {
                actions.push(Action::PhysicalPrinter);
            }
        });
        d.devices.camera(None);
        return;
    };
    if p.host.live() {
        d.devices.watch(&p);
    }
    let status = d.devices.info.get(&p.name).and_then(|i| i.status.clone());
    let online = d.devices.info.get(&p.name).is_some_and(|i| i.offline.is_none() && i.status.is_some());
    let (line, ok) = state_line(d, &p);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
        icons::paint(ui, r, "monitor_printer", t, true);
        let names: Vec<String> = session.prefs.printers.iter().map(|x| x.name.clone()).collect();
        egui::ComboBox::from_id_salt("device-printer").selected_text(RichText::new(&p.name).strong()).show_ui(
            ui,
            |ui| {
                for n in names {
                    if ui.selectable_label(n == p.name, &n).clicked() {
                        session.prefs.active_printer = n;
                    }
                }
            },
        );
        ui.label(RichText::new(format!("{} · {line}", p.host.label())).color(if ok {
            t.text_muted
        } else {
            t.warning
        }));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui_kit::primary_button(ui, t, "Add printer").clicked() {
                actions.push(Action::PhysicalPrinter);
            }
            if ui.button("Edit").clicked() {
                let i = session.prefs.printers.iter().position(|x| x.name == p.name);
                d.edit = Some((i, p.clone()));
                d.test_result = None;
                actions.push(Action::PhysicalPrinter);
            }
        });
    });
    ui.add_space(10.0);
    let width = ui.available_width();
    let left_w = (width * 0.58).max(300.0);
    let state = status.as_ref().map_or(State::Unknown, |s| s.state);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(left_w);
            card(ui, t, "Camera", |ui| {
                let on = d.devices.camera_on(&p.name);
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), (left_w * 0.56).min(420.0)), Sense::hover());
                ui.painter().rect_filled(r, 6.0, t.app_bg);
                match d.devices.info.get(&p.name).and_then(|i| i.frame.as_ref()).filter(|_| on) {
                    Some(tex) => {
                        // Fitted, keeping its shape.
                        let size = tex.size_vec2();
                        let k = (r.width() / size.x.max(1.0)).min(r.height() / size.y.max(1.0));
                        let img = egui::Rect::from_center_size(r.center(), size * k);
                        ui.painter().image(
                            tex.id(),
                            img,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    None => {
                        icons::paint(
                            ui,
                            egui::Rect::from_center_size(r.center() - vec2(0.0, 14.0), vec2(48.0, 48.0)),
                            "monitor_camera",
                            t,
                            false,
                        );
                        let text = if on {
                            "Waiting for the camera…"
                        } else if online {
                            "Camera off"
                        } else {
                            "No camera feed — the printer is not connected"
                        };
                        ui.painter().text(
                            r.center() + vec2(0.0, 26.0),
                            egui::Align2::CENTER_CENTER,
                            text,
                            egui::FontId::proportional(13.0),
                            t.text_muted,
                        );
                    }
                }
                ui.add_space(4.0);
                let label = if on { "Turn camera off" } else { "Show camera" };
                if ui.add_enabled(p.host.live(), egui::Button::new(label)).clicked() {
                    d.devices.camera(if on { None } else { Some(&p) });
                }
            });
            ui.add_space(10.0);
            card(ui, t, "Printing progress", |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(90.0, 90.0), Sense::hover());
                    ui.painter().rect_filled(r, 6.0, t.raised_bg);
                    icons::paint(ui, r.shrink(20.0), "monitor_item_print", t, false);
                    ui.vertical(|ui| {
                        let offline = d.devices.info.get(&p.name).is_some_and(|i| i.offline.is_some());
                        let title = match status.as_ref().and_then(|s| s.file.clone()) {
                            Some(f) => f,
                            None if offline => "Offline".to_owned(),
                            None => state.label().to_owned(),
                        };
                        ui.label(RichText::new(title).strong());
                        let f = status.as_ref().and_then(|s| s.progress).unwrap_or(0.0);
                        ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width() - 10.0).show_percentage());
                        ui.horizontal(|ui| {
                            let layer =
                                status.as_ref().and_then(|s| s.layer).map_or("—".into(), |(c, n)| format!("{c}/{n}"));
                            let left = status.as_ref().and_then(|s| s.remaining_s).map_or("—".into(), hms);
                            ui.label(RichText::new(format!("Layer: {layer}")).color(t.text_muted));
                            ui.add_space(16.0);
                            ui.label(RichText::new(format!("Remaining: {left}")).color(t.text_muted));
                        });
                        ui.horizontal(|ui| {
                            let (icon, tip, cmd) = if state == State::Paused {
                                ("print_control_resume", "Resume", Command::Resume)
                            } else {
                                ("print_control_pause", "Pause", Command::Pause)
                            };
                            let busy = state.busy();
                            if icons::button_enabled(ui, icon, t, 28.0, false, busy, tip).clicked() {
                                d.devices.control(&p, cmd);
                            }
                            if icons::button_enabled(ui, "print_control_stop", t, 28.0, false, busy, "Stop").clicked() {
                                d.devices.control(&p, Command::Stop);
                            }
                            if d.devices.info.get(&p.name).is_some_and(|i| i.sending) {
                                ui.spinner();
                                ui.label("Sending…");
                            }
                        });
                    });
                });
            });
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            card(ui, t, "Control", |ui| control(ui, t, d, &p, status.as_ref(), online));
        });
    });
}

fn control(
    ui: &mut Ui,
    t: &Tokens,
    d: &mut DeviceState,
    p: &PhysicalPrinter,
    status: Option<&CosmicSlicer_connect::Status>,
    online: bool,
) {
    // Moving the head mid-print is refused, as on the printer's own screen.
    let printing = status.is_some_and(|s| s.state.busy());
    let prusalink = p.host == HostType::PrusaLink;
    ui.add_enabled_ui(online && !prusalink, |ui| {
        egui::Grid::new("device-temps").num_columns(4).spacing([12.0, 8.0]).show(ui, |ui| {
            for (icon, name, heater) in
                [("monitor_nozzle_temp", "Nozzle", Heater::Nozzle), ("monitor_bed_temp", "Bed", Heater::Bed)]
            {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                    icons::paint(ui, r, icon, t, true);
                    ui.label(name);
                });
                let reading = status.and_then(|s| s.temps.iter().find(|x| x.name == name));
                ui.label(reading.map_or("— °C".into(), |r| format!("{:.0} / {:.0} °C", r.actual, r.target)));
                let value = if heater == Heater::Nozzle { &mut d.nozzle_target } else { &mut d.bed_target };
                ui.add(egui::DragValue::new(value).range(0.0..=350.0).suffix(" °C"));
                if ui.button("Set").clicked() {
                    d.devices.control(p, Command::SetTemp(heater, *value));
                }
                ui.end_row();
            }
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                icons::paint(ui, r, "monitor_fan", t, true);
                ui.label("Part fan");
            });
            ui.label(status.and_then(|s| s.fan).map_or("—".into(), |f| format!("{:.0}%", f * 100.0)));
            let fan = ui.add(egui::Slider::new(&mut d.fan, 0.0..=100.0).suffix("%"));
            if fan.drag_stopped() || (fan.changed() && !fan.dragged()) {
                d.devices.control(p, Command::Fan(d.fan / 100.0));
            }
            ui.end_row();
        });
        if p.host == HostType::BambuLan {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Speed");
                let level = status.and_then(|s| s.speed).unwrap_or(2);
                for (i, name) in ["Silent", "Standard", "Sport", "Ludicrous"].iter().enumerate() {
                    if ui.selectable_label(level as usize == i + 1, *name).clicked() {
                        d.devices.control(p, Command::Speed(i as u8 + 1));
                    }
                }
            });
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Jog").strong());
        ui.add_enabled_ui(!printing, |ui| {
            let steps = [0.1f32, 1.0, 10.0, 100.0];
            ui.horizontal(|ui| {
                for (i, step) in steps.iter().enumerate() {
                    ui.selectable_value(&mut d.jog_step, i, format!("{step} mm"));
                }
            });
            let step = steps.get(d.jog_step).copied().unwrap_or(1.0);
            let mut jog = None;
            egui::Grid::new("jog").spacing([4.0, 4.0]).show(ui, |ui| {
                ui.label("");
                if ui.button(" Y+ ").clicked() {
                    jog = Some(Command::Jog('Y', step));
                }
                ui.label("");
                ui.label("   ");
                if ui.button(" Z+ ").clicked() {
                    jog = Some(Command::Jog('Z', step.min(10.0)));
                }
                ui.end_row();
                if ui.button(" X− ").clicked() {
                    jog = Some(Command::Jog('X', -step));
                }
                let (r, home) = ui.allocate_exact_size(vec2(36.0, 24.0), Sense::click());
                icons::paint(ui, r, "monitor_axis_home", t, home.hovered());
                if home.on_hover_text("Home all axes").clicked() {
                    jog = Some(Command::Home);
                }
                if ui.button(" X+ ").clicked() {
                    jog = Some(Command::Jog('X', step));
                }
                ui.label("   ");
                if ui.button(" Z− ").clicked() {
                    jog = Some(Command::Jog('Z', -step.min(10.0)));
                }
                ui.end_row();
                ui.label("");
                if ui.button(" Y− ").clicked() {
                    jog = Some(Command::Jog('Y', -step));
                }
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Extrude 10 mm").clicked() {
                    jog = Some(Command::Jog('E', 10.0));
                }
                if ui.button("Retract 10 mm").clicked() {
                    jog = Some(Command::Jog('E', -10.0));
                }
            });
            if let Some(cmd) = jog {
                d.devices.control(p, cmd);
            }
        });
    });
    ui.add_space(6.0);
    let note = if !online {
        "Shown once the printer answers."
    } else if prusalink {
        "PrusaLink lets CosmicSlicer pause, resume and stop prints; use the printer for the rest."
    } else if printing {
        "Jogging is off while printing."
    } else {
        ""
    };
    if !note.is_empty() {
        ui.label(RichText::new(note).small().color(t.text_muted));
    }
}

// ---- Multi-device ----

pub fn multi_device(ui: &mut Ui, session: &mut Session, t: &Tokens, d: &mut DeviceState, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Multi-device").size(18.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui_kit::primary_button(ui, t, "Add printer").clicked() {
                d.edit = None;
                actions.push(Action::PhysicalPrinter);
            }
        });
    });
    ui.add_space(10.0);
    let printers = session.prefs.printers.clone();
    card(ui, t, "Printers", |ui| {
        if printers.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
                icons::paint(ui, r, "monitor_none_printer", t, true);
                ui.label(RichText::new("No printers yet. Add printers to follow them all here.").color(t.text_muted));
            });
            ui.add_space(30.0);
            return;
        }
        egui::Grid::new("multi-device").num_columns(5).striped(true).spacing([30.0, 6.0]).show(ui, |ui| {
            for h in ["Printer", "Status", "Progress", "Task", "Actions"] {
                ui.label(RichText::new(h).color(t.text_muted));
            }
            ui.end_row();
            for p in &printers {
                if p.host.live() {
                    d.devices.watch(p);
                }
                let status = d.devices.info.get(&p.name).and_then(|i| i.status.clone());
                ui.label(RichText::new(&p.name).strong());
                let (line, ok) = state_line(d, p);
                ui.label(RichText::new(line).color(if ok { t.text } else { t.warning }));
                let f = status.as_ref().and_then(|s| s.progress).unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(f).desired_width(140.0).show_percentage());
                ui.label(status.as_ref().and_then(|s| s.file.clone()).unwrap_or_else(|| "—".into()));
                ui.horizontal(|ui| {
                    let busy = status.as_ref().is_some_and(|s| s.state.busy());
                    let paused = status.as_ref().is_some_and(|s| s.state == State::Paused);
                    if ui.add_enabled(busy, egui::Button::new(if paused { "Resume" } else { "Pause" })).clicked() {
                        d.devices.control(p, if paused { Command::Resume } else { Command::Pause });
                    }
                    if ui.add_enabled(busy, egui::Button::new("Stop")).clicked() {
                        d.devices.control(p, Command::Stop);
                    }
                    if ui.button("Open").clicked() {
                        session.prefs.active_printer = p.name.clone();
                        session.page = Page::Device;
                    }
                });
                ui.end_row();
            }
        });
    });
}

// ---- Project ----

pub fn project(ui: &mut Ui, session: &mut Session, t: &Tokens) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let w = ui.available_width().min(900.0);
        ui.vertical(|ui| {
            ui.set_width(w);
            ui.label(RichText::new("Project").size(18.0).strong());
            ui.add_space(10.0);
            card(ui, t, "Model information", |ui| {
                egui::Grid::new("project-info").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                    ui.label("Model name");
                    ui.add(egui::TextEdit::singleline(&mut session.project.name).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label("Author");
                    ui.add(egui::TextEdit::singleline(&mut session.project.author).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label("License");
                    egui::ComboBox::from_id_salt("license")
                        .selected_text(if session.project.license.is_empty() {
                            "None"
                        } else {
                            &session.project.license
                        })
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for l in ["", "CC0", "CC BY", "CC BY-SA", "CC BY-NC", "CC BY-NC-SA", "GPL-3.0", "MIT"] {
                                ui.selectable_value(
                                    &mut session.project.license,
                                    l.to_owned(),
                                    if l.is_empty() { "None" } else { l },
                                );
                            }
                        });
                    ui.end_row();
                    ui.label("Description");
                    ui.add(
                        egui::TextEdit::multiline(&mut session.project.description)
                            .desired_rows(6)
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                });
            });
            ui.add_space(12.0);
            card(ui, t, "Bill of materials", |ui| {
                egui::Grid::new("bom").num_columns(4).striped(true).spacing([24.0, 6.0]).show(ui, |ui| {
                    for h in ["Object", "Plate", "Filament", "Volume"] {
                        ui.label(RichText::new(h).color(t.text_muted));
                    }
                    ui.end_row();
                    for o in &session.model.objects {
                        ui.label(&o.name);
                        ui.label(format!("{}", o.plate + 1));
                        ui.label(format!("{}", o.filament));
                        let s = o.transform.scale;
                        let v = o.mesh.volume() * (s.x * s.y * s.z).abs() / 1000.0;
                        ui.label(format!("{v:.2} cm³"));
                        ui.end_row();
                    }
                });
                if session.model.objects.is_empty() {
                    ui.label(RichText::new("No objects in the project.").color(t.text_muted));
                }
            });
            ui.add_space(12.0);
            card(ui, t, "Pictures and assembly guide", |ui| {
                ui.label(
                    RichText::new("Add pictures and an assembly guide to share with the project file.")
                        .color(t.text_muted),
                );
                ui.add_enabled(false, egui::Button::new("Add picture…"));
            });
        });
    });
}
