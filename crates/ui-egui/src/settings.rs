//! Settings pages drawn from OrcaSlicer's layout: groups with an icon and a title, rows of
//! label and field. Used by the sidebar's Process panel and the settings windows.

use egui::{Color32, RichText, Sense, Ui, vec2};

use CosmicSlicer_config::{Entry, Group, Mode, OptionDef, OptionType, Page, TabKind};

use crate::icons;
use crate::state::{Level, Session};
use crate::theme::{Tokens, parse_hex, to_hex};

/// Which values a form edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub kind: TabKind,
    /// The project filament (0-based) for filament settings.
    pub filament: usize,
    /// An object's own overrides, in the sidebar's Objects mode.
    pub object: Option<u64>,
}

impl Target {
    pub fn new(kind: TabKind) -> Self {
        Self { kind, filament: 0, object: None }
    }
}

fn read(session: &Session, target: Target, key: &str) -> String {
    if let Some(id) = target.object
        && let Some((_, v)) = session.model.get(id).and_then(|o| o.overrides.iter().find(|(k, _)| k == key))
    {
        return v.clone();
    }
    session.presets.value(session.schema, target.kind, target.filament, key)
}

fn edited(session: &Session, target: Target, key: &str) -> bool {
    match target.object {
        Some(id) => session.model.get(id).is_some_and(|o| o.overrides.iter().any(|(k, _)| k == key)),
        None => session.presets.is_edited(target.kind, target.filament, key),
    }
}

fn write(session: &mut Session, target: Target, key: &str, value: String) {
    match target.object {
        Some(id) => {
            if let Some(o) = session.model.get_mut(id) {
                match o.overrides.iter_mut().find(|(k, _)| k == key) {
                    Some(slot) => slot.1 = value,
                    None => o.overrides.push((key.to_owned(), value)),
                }
            }
            session.dirty = true;
        }
        None => {
            let schema = session.schema;
            session.presets.set_value(schema, target.kind, target.filament, key, value);
            session.dirty = true;
        }
    }
    session.sliced = None;
}

fn revert(session: &mut Session, target: Target, key: &str) {
    match target.object {
        Some(id) => {
            if let Some(o) = session.model.get_mut(id) {
                o.overrides.retain(|(k, _)| k != key);
            }
        }
        None => session.presets.revert(target.kind, target.filament, key),
    }
    session.sliced = None;
}

/// Whether `def` shows at this Advanced setting.
pub fn visible(def: &OptionDef, advanced: bool) -> bool {
    advanced || def.mode == Mode::Simple
}

fn entry_visible(session: &Session, e: &Entry, advanced: bool) -> bool {
    e.keys().iter().any(|k| session.schema.option(k).is_some_and(|d| visible(d, advanced)))
}

pub fn group_visible(session: &Session, g: &Group, advanced: bool) -> bool {
    g.options.iter().any(|e| entry_visible(session, e, advanced))
}

/// The groups of one page.
pub fn page(ui: &mut Ui, session: &mut Session, t: &Tokens, page: &Page, target: Target, advanced: bool) {
    let mut any = false;
    for g in &page.groups {
        if group_visible(session, g, advanced) {
            any = true;
            group(ui, session, t, g, target, advanced);
        }
    }
    if !any {
        ui.add_space(12.0);
        ui.label(RichText::new("Turn on Advanced to see the settings of this page.").color(t.text_muted));
    }
}

fn group_header(ui: &mut Ui, t: &Tokens, icon: &str, title: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
        icons::paint(ui, r, icon, t, true);
        ui.label(RichText::new(title).strong().size(13.5).color(t.text));
        let rest = ui.available_rect_before_wrap();
        ui.painter().hline(rest.x_range().shrink(6.0), rest.center().y, egui::Stroke::new(1.0, t.border));
    });
    ui.add_space(2.0);
}

pub fn group(ui: &mut Ui, session: &mut Session, t: &Tokens, g: &Group, target: Target, advanced: bool) {
    group_header(ui, t, &g.icon, &g.name);
    for e in &g.options {
        if entry_visible(session, e, advanced) {
            entry(ui, session, t, e, target, advanced);
        }
    }
}

/// Width of the label column.
fn label_width(ui: &Ui) -> f32 {
    (ui.available_width() * 0.46).clamp(120.0, 260.0)
}

fn tooltip(def: &OptionDef, key: &str) -> String {
    let mut s = def.tooltip.clone();
    if !s.is_empty() {
        s.push_str("\n\n");
    }
    s.push_str(&format!("parameter name: {key}"));
    s
}

fn label_cell(ui: &mut Ui, session: &mut Session, t: &Tokens, text: &str, hint: &str, target: Target, keys: &[&str]) {
    let modified = keys.iter().any(|k| edited(session, target, k));
    let width = label_width(ui);
    ui.allocate_ui_with_layout(vec2(width, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(width);
        if modified {
            let r = icons::button(ui, "undo", t, 18.0, false, "Revert to the preset's value");
            if r.clicked() {
                for k in keys {
                    revert(session, target, k);
                }
            }
        } else {
            ui.add_space(22.0);
        }
        let colour = if modified { t.modified } else { t.text };
        ui.add(egui::Label::new(RichText::new(text).color(colour)).truncate()).on_hover_text(hint);
    });
}

fn entry(ui: &mut Ui, session: &mut Session, t: &Tokens, e: &Entry, target: Target, advanced: bool) {
    let schema = session.schema;
    match e {
        Entry::Single { key, .. } => {
            let Some(def) = schema.option(key) else { return };
            // Multi-line text (G-code) takes the full width under its label.
            if def.multiline || def.kind() == OptionType::String && def.height.is_some_and(|h| h > 1) {
                ui.horizontal(|ui| {
                    label_cell(ui, session, t, def.display_label(), &tooltip(def, key), target, &[key]);
                });
                let mut text = read(session, target, key);
                let rows = def.height.unwrap_or(8).clamp(3, 20) as usize;
                let response = ui.add(
                    egui::TextEdit::multiline(&mut text).code_editor().desired_rows(rows).desired_width(f32::INFINITY),
                );
                if response.changed() {
                    write(session, target, key, text);
                }
                return;
            }
            ui.horizontal(|ui| {
                label_cell(ui, session, t, def.display_label(), &tooltip(def, key), target, &[key]);
                field(ui, session, t, key, def, target);
            });
        }
        Entry::Line { line, keys } => {
            let shown: Vec<&String> =
                keys.iter().filter(|k| schema.option(k).is_some_and(|d| visible(d, advanced))).collect();
            let first_tip = shown.first().and_then(|k| schema.option(k)).map(|d| d.tooltip.clone()).unwrap_or_default();
            let key_refs: Vec<&str> = shown.iter().map(|k| k.as_str()).collect();
            ui.horizontal(|ui| {
                label_cell(ui, session, t, line, &first_tip, target, &key_refs);
                ui.horizontal_wrapped(|ui| {
                    for key in shown {
                        let Some(def) = schema.option(key) else { continue };
                        ui.label(RichText::new(def.display_label()).color(t.text_muted))
                            .on_hover_text(tooltip(def, key));
                        field(ui, session, t, key, def, target);
                        ui.add_space(6.0);
                    }
                });
            });
        }
    }
}

/// The first element of a list value, which the fields edit.
fn first(value: &str) -> &str {
    value.split(',').next().unwrap_or("").trim()
}

/// `value` with its first element replaced by `new`.
fn replace_first(value: &str, new: &str) -> String {
    match value.split_once(',') {
        Some((_, rest)) => format!("{new},{rest}"),
        None => new.to_owned(),
    }
}

fn field(ui: &mut Ui, session: &mut Session, t: &Tokens, key: &str, def: &OptionDef, target: Target) {
    let raw = read(session, target, key);
    let is_vec = def.is_vector();
    let shown = if is_vec { first(&raw).to_owned() } else { raw.clone() };
    let id = ui.id().with(("field", key, target.filament, target.object));
    let colour_option = def.gui_type.as_deref() == Some("color") || key.ends_with("_colour");
    let commit = |session: &mut Session, v: String| {
        let v = if is_vec { replace_first(&raw, &v) } else { v };
        write(session, target, key, v);
    };
    match def.kind() {
        OptionType::Bool => {
            let mut on = CosmicSlicer_config::is_true(&shown);
            if ui.checkbox(&mut on, "").changed() {
                commit(session, if on { "1" } else { "0" }.into());
            }
        }
        OptionType::Enum => {
            let label = def.enum_label(&shown).unwrap_or(&shown).to_owned();
            let mut chosen = None;
            let width = 180.0f32.min((ui.available_width() - 8.0).max(60.0));
            egui::ComboBox::from_id_salt(id).selected_text(label).width(width).show_ui(ui, |ui| {
                for (v, l) in &def.enum_values {
                    if ui.selectable_label(*v == shown, l.as_str()).clicked() {
                        chosen = Some(v.clone());
                    }
                }
            });
            if let Some(v) = chosen {
                commit(session, v);
            }
        }
        _ if colour_option => {
            let mut c = parse_hex(&shown).unwrap_or(Color32::GRAY);
            if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                commit(session, to_hex(c));
            }
        }
        kind => {
            let numeric =
                matches!(kind, OptionType::Float | OptionType::Int | OptionType::Percent | OptionType::FloatOrPercent);
            // Leave room for the units so a narrow sidebar never grows to fit a field.
            let width = if numeric { 96.0 } else { 200.0f32 }.min((ui.available_width() - 44.0).max(48.0));
            if let Some(v) = text_field(ui, id, &shown, width) {
                match validate(def, kind, &v) {
                    Ok(v) => commit(session, v),
                    Err(e) => session.notify(Level::Warning, format!("{}: {e}", def.long_label())),
                }
            }
            if !def.sidetext.is_empty() {
                ui.label(RichText::new(&def.sidetext).color(t.text_muted).small());
            }
            if key == "printable_area" && ui.small_button("Set…").on_hover_text("Bed shape").clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("bed-shape-open"), true));
            }
        }
    }
}

/// A single-line text box that hands back its text when the edit is committed (Enter or
/// leaving the box), never while typing.
pub fn text_field(ui: &mut Ui, id: egui::Id, value: &str, width: f32) -> Option<String> {
    let mut buffer: String = ui.ctx().data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| value.to_owned());
    let response = ui.add(egui::TextEdit::singleline(&mut buffer).id(id).desired_width(width));
    if response.has_focus() {
        ui.ctx().data_mut(|d| d.insert_temp(id, buffer.clone()));
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.ctx().data_mut(|d| d.remove::<String>(id));
            response.surrender_focus();
        }
        return None;
    }
    ui.ctx().data_mut(|d| d.remove::<String>(id));
    (response.lost_focus() && buffer != value).then_some(buffer)
}

/// Checks a typed value against the option's type and range; returns it tidied.
pub fn validate(def: &OptionDef, kind: OptionType, v: &str) -> Result<String, String> {
    let v = v.trim();
    let range = |x: f64| -> Result<(), String> {
        if let Some(min) = def.min
            && x < min
        {
            return Err(format!("{x} is below the minimum {min}"));
        }
        if let Some(max) = def.max
            && x > max
        {
            return Err(format!("{x} is above the maximum {max}"));
        }
        Ok(())
    };
    let number = |s: &str| s.parse::<f64>().ok().filter(|x| x.is_finite());
    match kind {
        OptionType::Int => {
            let x = v.parse::<i64>().map_err(|_| format!("\"{v}\" is not a whole number"))?;
            range(x as f64)?;
            Ok(x.to_string())
        }
        OptionType::Float => {
            let x = number(v).ok_or_else(|| format!("\"{v}\" is not a number"))?;
            range(x)?;
            Ok(v.to_owned())
        }
        OptionType::Percent => {
            let x = number(v.trim_end_matches('%').trim()).ok_or_else(|| format!("\"{v}\" is not a percentage"))?;
            range(x)?;
            Ok(format!("{x}%"))
        }
        OptionType::FloatOrPercent => {
            let pct = v.ends_with('%');
            let x = number(v.trim_end_matches('%').trim())
                .ok_or_else(|| format!("\"{v}\" is not a number or percentage"))?;
            if x < 0.0 {
                return Err("negative values are not allowed".into());
            }
            Ok(if pct { format!("{x}%") } else { v.to_owned() })
        }
        _ => Ok(v.to_owned()),
    }
}

/// Settings of `kind` whose label, key or tooltip contain `query`: (page, group, entry).
pub fn search<'a>(
    session: &'a Session,
    kind: TabKind,
    query: &str,
    advanced: bool,
) -> Vec<(&'a str, &'a Group, &'a Entry)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let schema = session.schema;
    let mut out = Vec::new();
    for p in schema.pages(kind) {
        for g in &p.groups {
            for e in &g.options {
                let hit = e.keys().iter().any(|k| {
                    schema.option(k).is_some_and(|d| {
                        visible(d, advanced)
                            && (k.contains(&q)
                                || d.label.to_lowercase().contains(&q)
                                || d.full_label.to_lowercase().contains(&q)
                                || g.name.to_lowercase().contains(&q))
                    })
                }) || matches!(e, Entry::Line { line, .. } if line.to_lowercase().contains(&q));
                if hit {
                    out.push((p.name.as_str(), g, e));
                }
            }
        }
    }
    out
}

/// Search results drawn as rows under their page and group.
pub fn search_results(
    ui: &mut Ui,
    session: &mut Session,
    t: &Tokens,
    kind: TabKind,
    query: &str,
    target: Target,
    advanced: bool,
) {
    let results: Vec<(String, Group, Entry)> = search(session, kind, query, advanced)
        .into_iter()
        .map(|(p, g, e)| (p.to_owned(), g.clone(), e.clone()))
        .collect();
    if results.is_empty() {
        ui.add_space(10.0);
        ui.label(RichText::new(format!("No settings match \"{query}\".")).color(t.text_muted));
        return;
    }
    let mut last = String::new();
    for (page, g, e) in results {
        let heading = format!("{page} › {}", g.name);
        if heading != last {
            group_header(ui, t, &g.icon, &heading);
            last = heading;
        }
        entry(ui, session, t, &e, target, advanced);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CosmicSlicer_config::Schema;

    #[test]
    fn validation_respects_types_and_ranges() {
        let s = Schema::get();
        let Some(walls) = s.option("wall_loops") else { return };
        assert_eq!(validate(walls, OptionType::Int, " 3 ").as_deref(), Ok("3"));
        assert!(validate(walls, OptionType::Int, "2.5").is_err());
        assert!(validate(walls, OptionType::Int, "-1").is_err());
        let Some(density) = s.option("sparse_infill_density") else { return };
        assert_eq!(validate(density, OptionType::Percent, "15").as_deref(), Ok("15%"));
        assert!(validate(density, OptionType::Percent, "150%").is_err());
        let Some(lh) = s.option("layer_height") else { return };
        assert!(validate(lh, OptionType::Float, "NaN").is_err());
        assert!(validate(lh, OptionType::Float, "inf").is_err());
    }

    #[test]
    fn list_values_edit_their_first_element() {
        assert_eq!(first("60,70"), "60");
        assert_eq!(replace_first("60,70", "80"), "80,70");
        assert_eq!(replace_first("60", "80"), "80");
    }

    #[test]
    fn search_finds_settings() {
        let session = Session::new();
        assert!(!search(&session, TabKind::Process, "seam", true).is_empty());
        assert!(search(&session, TabKind::Process, "zzzz-no-such", true).is_empty());
    }
}
