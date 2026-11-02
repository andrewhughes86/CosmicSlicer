//! The pages without a 3D canvas: Home, Device, Multi-device and Project.

use egui::{Align, Layout, RichText, Sense, Stroke, Ui, vec2};

use crate::Action;
use crate::icons;
use crate::state::{Page, Session};
use crate::theme::Tokens;
use crate::ui_kit::{self, Surface};

/// Device page state: the printer connection being set up.
#[derive(Clone, Debug)]
pub struct DeviceState {
    pub host_type: usize,
    pub host: String,
    pub api_key: String,
    pub nozzle_target: f32,
    pub bed_target: f32,
    pub fan: f32,
    pub speed: usize,
    pub jog_step: usize,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            host_type: 0,
            host: String::new(),
            api_key: String::new(),
            nozzle_target: 0.0,
            bed_target: 0.0,
            fan: 0.0,
            speed: 1,
            jog_step: 1,
        }
    }
}

/// OrcaSlicer's printer host types.
pub const HOST_TYPES: &[&str] = &[
    "Octo/Klipper",
    "PrusaLink",
    "PrusaConnect",
    "Duet",
    "FlashAir",
    "AstroBox",
    "Repetier",
    "MKS",
    "ESP3D",
    "CrealityPrint",
    "Obico",
    "Flashforge",
    "SimplyPrint",
    "Elegoo Link",
];

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

pub fn device(ui: &mut Ui, session: &mut Session, t: &Tokens, d: &mut DeviceState, actions: &mut Vec<Action>) {
    let printer =
        session.presets.preset(CosmicSlicer_config::TabKind::Printer, 0).map(|p| p.name.clone()).unwrap_or_default();
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
        icons::paint(ui, r, "monitor_printer", t, true);
        ui.label(RichText::new(&printer).size(17.0).strong());
        ui.label(RichText::new("·  Not connected").color(t.text_muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui_kit::primary_button(ui, t, "Connect").clicked() {
                actions.push(Action::PhysicalPrinter);
            }
        });
    });
    ui.add_space(10.0);
    let width = ui.available_width();
    let left_w = (width * 0.58).max(300.0);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(left_w);
            card(ui, t, "Camera", |ui| {
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), (left_w * 0.56).min(420.0)), Sense::hover());
                ui.painter().rect_filled(r, 6.0, t.app_bg);
                icons::paint(
                    ui,
                    egui::Rect::from_center_size(r.center() - vec2(0.0, 14.0), vec2(48.0, 48.0)),
                    "monitor_camera",
                    t,
                    false,
                );
                ui.painter().text(
                    r.center() + vec2(0.0, 26.0),
                    egui::Align2::CENTER_CENTER,
                    "No camera feed — the printer is not connected",
                    egui::FontId::proportional(13.0),
                    t.text_muted,
                );
            });
            ui.add_space(10.0);
            card(ui, t, "Printing progress", |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(90.0, 90.0), Sense::hover());
                    ui.painter().rect_filled(r, 6.0, t.raised_bg);
                    icons::paint(ui, r.shrink(20.0), "monitor_item_print", t, false);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Idle").strong());
                        ui.add(
                            egui::ProgressBar::new(0.0).desired_width(ui.available_width() - 10.0).show_percentage(),
                        );
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Layer: —").color(t.text_muted));
                            ui.add_space(16.0);
                            ui.label(RichText::new("Remaining: —").color(t.text_muted));
                        });
                        ui.horizontal(|ui| {
                            for icon in ["print_control_pause", "print_control_stop"] {
                                icons::button_enabled(ui, icon, t, 28.0, false, false, "Available when printing");
                            }
                        });
                    });
                });
            });
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            card(ui, t, "Control", |ui| {
                ui.add_enabled_ui(false, |ui| {
                    egui::Grid::new("device-temps").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
                        for (icon, name, value) in [
                            ("monitor_nozzle_temp", "Nozzle", &mut d.nozzle_target),
                            ("monitor_bed_temp", "Bed", &mut d.bed_target),
                        ] {
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                                icons::paint(ui, r, icon, t, true);
                                ui.label(name);
                            });
                            ui.label(RichText::new("— °C").color(t.text_muted));
                            ui.add(egui::DragValue::new(value).range(0.0..=350.0).suffix(" °C"));
                            ui.end_row();
                        }
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                            icons::paint(ui, r, "monitor_fan", t, true);
                            ui.label("Part fan");
                        });
                        ui.label(RichText::new("—").color(t.text_muted));
                        ui.add(egui::Slider::new(&mut d.fan, 0.0..=100.0).suffix("%"));
                        ui.end_row();
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("Speed");
                        for (i, name) in ["Silent", "Standard", "Sport", "Ludicrous"].iter().enumerate() {
                            ui.selectable_value(&mut d.speed, i, *name);
                        }
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Jog").strong());
                    ui.horizontal(|ui| {
                        for (i, step) in ["0.1", "1", "10", "100"].iter().enumerate() {
                            ui.selectable_value(&mut d.jog_step, i, format!("{step} mm"));
                        }
                    });
                    egui::Grid::new("jog").spacing([4.0, 4.0]).show(ui, |ui| {
                        ui.label("");
                        let _ = ui.button(" Y+ ");
                        ui.label("");
                        ui.label("   ");
                        let _ = ui.button(" Z+ ");
                        ui.end_row();
                        let _ = ui.button(" X− ");
                        let (r, _) = ui.allocate_exact_size(vec2(36.0, 24.0), Sense::hover());
                        icons::paint(ui, r, "monitor_axis_home", t, false);
                        let _ = ui.button(" X+ ");
                        ui.label("   ");
                        let _ = ui.button(" Z− ");
                        ui.end_row();
                        ui.label("");
                        let _ = ui.button(" Y− ");
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let _ = ui.button("Extrude");
                        let _ = ui.button("Retract");
                        let _ = ui.button("Load");
                        let _ = ui.button("Unload");
                    });
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Connect a printer to control it from here.").small().color(t.text_muted));
            });
        });
    });
}

// ---- Multi-device ----

pub fn multi_device(ui: &mut Ui, t: &Tokens, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Multi-device").size(18.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui_kit::primary_button(ui, t, "Add printer").clicked() {
                actions.push(Action::PhysicalPrinter);
            }
        });
    });
    ui.add_space(10.0);
    card(ui, t, "Printers", |ui| {
        egui::Grid::new("multi-device").num_columns(5).striped(true).spacing([30.0, 6.0]).show(ui, |ui| {
            for h in ["Printer", "Status", "Progress", "Task", "Actions"] {
                ui.label(RichText::new(h).color(t.text_muted));
            }
            ui.end_row();
        });
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
            icons::paint(ui, r, "monitor_none_printer", t, true);
            ui.label(
                RichText::new("No printers yet. Add printers to send plates to several at once.").color(t.text_muted),
            );
        });
        ui.add_space(30.0);
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
