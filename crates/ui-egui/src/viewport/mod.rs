//! The 3D canvas of Prepare and Preview: the plates, the objects on them, the camera, picking
//! and the handles of the Move / Rotate / Scale tools. The 2D chrome drawn over it (toolbars,
//! plate buttons, tool panels, the Preview legend and sliders) is in `overlay`.

pub mod bed;
pub mod camera;
pub mod gizmo;
pub mod gpu;
mod overlay;

use std::sync::Arc;

use egui::{Pos2, Rect, Sense, Ui};
use egui_wgpu::RenderState;
use glam::{Quat, Vec2, Vec3};

use CosmicSlicer_config::{TabKind, first_number};
use CosmicSlicer_model::{Aabb, Transform};

use crate::Action;
use crate::state::{Page, Session};
use crate::theme::{Tokens, linear, parse_hex};
pub use camera::{Camera, View};
use gizmo::Handle;
pub use gizmo::Tool;
use gpu::{FlatGeometry, FrameParams, Gpu, ObjectDraw};

enum Drag {
    Orbit,
    Pan,
    /// Dragging objects across the plate, on the plane at `z`.
    Move {
        ids: Vec<u64>,
        last: Vec3,
        z: f32,
        moved: bool,
    },
    Axis {
        axis: usize,
        ids: Vec<u64>,
        origin: Vec3,
        last: f32,
    },
    Rotate {
        axis: usize,
        id: u64,
        centre: Pos2,
        start_angle: f32,
        start: Transform,
        sign: f32,
    },
    Scale {
        id: u64,
        centre: Pos2,
        start_len: f32,
        start: Transform,
    },
}

/// What the Preview sliders show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layers {
    /// Number of layers in the shown plate.
    pub count: usize,
    pub height: f32,
    pub first: f32,
    /// Top and bottom shown layer (1-based, inclusive).
    pub top: usize,
    pub bottom: usize,
    /// Heights of the sliced layers, when the plate is sliced.
    pub zs: Vec<f32>,
}

impl Layers {
    /// Height of the top of layer `n` (1-based).
    pub fn z_of(&self, n: usize) -> f32 {
        if n == 0 {
            return 0.0;
        }
        match self.zs.get(n - 1) {
            Some(z) => *z,
            None => self.first + (n.saturating_sub(1)) as f32 * self.height,
        }
    }
}

pub struct Viewport {
    pub camera: Camera,
    gpu: Option<Gpu>,
    bed_key: Option<String>,
    pub tool: Option<Tool>,
    hover: Option<u64>,
    hot: Option<Handle>,
    drag: Option<Drag>,
    /// Frame everything on the next frame (start-up, "Zoom to fit").
    pub fit_pending: bool,
    pub layers: Layers,
    /// Position along the top layer's moves (0 to 1).
    pub moves: f32,
    /// Index into the legend's view types.
    pub view_type: usize,
    /// Legend rows switched off.
    pub hidden_features: Vec<bool>,
    /// The object right-clicked, for its context menu.
    context_object: Option<u64>,
    size: Vec2,
    /// Uniform scaling in the Scale panel.
    pub uniform_scale: bool,
    /// The sliced result whose toolpaths are on the GPU, and how they are coloured.
    segments_key: Option<usize>,
    colours_key: Option<(usize, usize, Vec<bool>)>,
    /// The legend's gradient for the current view type.
    pub legend_range: Option<crate::preview::Range>,
    /// Toolpath segments the GPU holds (may be fewer than the result has).
    segments_loaded: usize,
    /// Preview options, as OrcaSlicer's legend has them.
    pub show_travel: bool,
    pub show_retractions: bool,
    pub show_seams: bool,
    /// The legend's "Close" on an opened G-code file was clicked.
    pub close_viewer: bool,
    /// A top layer to show once the plate is sliced (automation).
    pub pending_top: Option<usize>,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            gpu: None,
            bed_key: None,
            tool: None,
            hover: None,
            hot: None,
            drag: None,
            fit_pending: true,
            layers: Layers::default(),
            moves: 1.0,
            view_type: 0,
            hidden_features: Vec::new(),
            context_object: None,
            size: Vec2::new(1200.0, 800.0),
            uniform_scale: true,
            segments_key: None,
            colours_key: None,
            legend_range: None,
            segments_loaded: 0,
            show_travel: false,
            show_retractions: false,
            show_seams: false,
            close_viewer: false,
            pending_top: None,
        }
    }
}

/// The colour objects print in: their filament's.
fn object_colour(session: &Session, filament: usize) -> egui::Color32 {
    parse_hex(&session.presets.filament_colour(session.schema, filament.saturating_sub(1)))
        .unwrap_or(egui::Color32::from_rgb(0x22, 0xD3, 0xEE))
}

impl Viewport {
    pub fn set_view(&mut self, view: View) {
        self.camera.set_view(view);
        self.fit_pending = true;
    }

    /// Frames the current plate and what is on it.
    fn fit(&mut self, session: &Session) {
        let mut b = session.plate_bounds(session.plate);
        b.max.z = b.min.z;
        for o in session.model.objects.iter().filter(|o| o.plate == session.plate) {
            b = b.union(&o.bounds());
        }
        self.camera.fit(&b, self.size.x / self.size.y.max(1.0));
    }

    fn aspect(&self) -> f32 {
        self.size.x / self.size.y.max(1.0)
    }

    /// The objects the canvas shows on `page`.
    fn shown<'a>(&self, session: &'a Session, page: Page) -> impl Iterator<Item = &'a CosmicSlicer_model::Object> {
        let plate = session.plate;
        session.model.objects.iter().filter(move |o| o.visible && (page == Page::Prepare || o.plate == plate))
    }

    /// The object under the mouse: (id, distance, triangle).
    fn pick(&self, session: &Session, page: Page, mouse: Vec2) -> Option<(u64, f32, usize)> {
        let ndc = Vec2::new(mouse.x / self.size.x * 2.0 - 1.0, 1.0 - mouse.y / self.size.y * 2.0);
        let (o, d) = self.camera.ray(ndc, self.aspect());
        let mut best: Option<(u64, f32, usize)> = None;
        for obj in self.shown(session, page) {
            let Some(near) = obj.bounds().ray_hit(o, d) else { continue };
            if best.is_some_and(|b| near > b.1) {
                continue;
            }
            if let Some((dist, tri)) = obj.mesh.ray_hit(&obj.matrix(), o, d)
                && best.is_none_or(|b| dist < b.1)
            {
                best = Some((obj.id, dist, tri));
            }
            // Its other parts select it too (with no face of the main mesh to lay down).
            for p in obj.parts.iter().filter(|p| p.kind.is_solid()) {
                if let Some((dist, _)) = p.mesh.ray_hit(&(obj.matrix() * p.matrix()), o, d)
                    && best.is_none_or(|b| dist < b.1)
                {
                    best = Some((obj.id, dist, usize::MAX));
                }
            }
        }
        best
    }

    fn ray(&self, mouse: Vec2) -> (Vec3, Vec3) {
        let ndc = Vec2::new(mouse.x / self.size.x * 2.0 - 1.0, 1.0 - mouse.y / self.size.y * 2.0);
        self.camera.ray(ndc, self.aspect())
    }

    fn selection_bounds(session: &Session) -> Option<Aabb> {
        let mut b = Aabb::EMPTY;
        for id in &session.selection {
            if let Some(o) = session.model.get(*id) {
                b = b.union(&o.bounds());
            }
        }
        b.is_valid().then_some(b)
    }

    /// Recomputes the layer range Preview shows.
    fn update_layers(&mut self, session: &Session) {
        if let Some(result) = session.shown_result() {
            let zs: Vec<f32> = result.preview.layers.iter().map(|l| l.z).collect();
            if zs != self.layers.zs {
                let count = zs.len();
                let height = result.preview.layers.last().map_or(0.2, |l| l.height);
                let first = zs.first().copied().unwrap_or(0.2);
                self.layers = Layers { count, height, first, top: count, bottom: 1.min(count), zs };
                if let Some(n) = self.pending_top.take() {
                    self.layers.top = n.clamp(1.min(count), count);
                }
            }
            return;
        }
        if !self.layers.zs.is_empty() {
            self.layers.zs.clear();
            self.layers.count = 0;
        }
        let schema = session.schema;
        let h = first_number(&session.presets.value(schema, TabKind::Process, 0, "layer_height")).unwrap_or(0.2) as f32;
        let first = first_number(&session.presets.value(schema, TabKind::Process, 0, "initial_layer_print_height"))
            .unwrap_or(0.2) as f32;
        let h = if h.is_finite() && h > 0.01 { h } else { 0.2 };
        let first = if first.is_finite() && first > 0.01 { first } else { h };
        let top_z = session
            .model
            .objects
            .iter()
            .filter(|o| o.plate == session.plate && o.visible)
            .map(|o| o.bounds().max.z)
            .fold(0.0f32, f32::max);
        let count = if top_z <= 0.0 { 0 } else { (((top_z - first) / h).ceil().max(0.0) as usize + 1).min(100_000) };
        if count != self.layers.count || h != self.layers.height || first != self.layers.first {
            self.layers = Layers { count, height: h, first, top: count, bottom: 1.min(count), zs: Vec::new() };
        }
        // Not `clamp`: its bounds may cross here (no layers), and it panics when they do.
        self.layers.top = self.layers.top.min(count).max(self.layers.bottom.min(count));
        self.layers.bottom = self.layers.bottom.min(self.layers.top).max(1.min(count));
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        session: &mut Session,
        t: &Tokens,
        render_state: Option<&RenderState>,
        actions: &mut Vec<Action>,
    ) {
        let page = session.page;
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        self.size = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
        if page == Page::Preview {
            self.update_layers(session);
        }
        if self.fit_pending && rect.width() > 10.0 {
            self.fit(session);
            self.fit_pending = false;
        }
        self.handle_input(ui, &response, rect, session, page, actions);
        self.context_menu(&response, session, t, actions);

        // ---- render ----
        let Some(rs) = render_state else {
            ui.painter().rect_filled(rect, 0.0, t.viewport_bottom);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "3D view needs a GPU (wgpu) renderer",
                egui::FontId::proportional(16.0),
                t.text_muted,
            );
            overlay::show(self, ui, rect, session, t, actions);
            return;
        };
        let mut gpu = self.gpu.take().unwrap_or_else(|| Gpu::new(rs));
        let bed_key = format!(
            "{:?}|{}|{}|{}|{}|{}",
            session.bed_outline(),
            session.model.plates,
            session.plate,
            page == Page::Preview,
            session.prefs.show_grid,
            t.dark
        );
        if self.bed_key.as_deref() != Some(&bed_key) {
            gpu.set_bed(&bed::build(session, t, page == Page::Preview));
            self.bed_key = Some(bed_key);
        }
        // A sliced plate shows its toolpaths instead of the models.
        let result = if page == Page::Preview { session.shown_result() } else { None };
        match &result {
            Some(r) => self.upload_toolpaths(&mut gpu, r),
            None => {
                if self.segments_key.take().is_some() {
                    gpu.clear_segments();
                    self.colours_key = None;
                    self.legend_range = None;
                }
            }
        }
        let shown: Vec<&CosmicSlicer_model::Object> =
            if result.is_some() { Vec::new() } else { self.shown(session, page).collect() };
        let meshes: Vec<(usize, &CosmicSlicer_model::Mesh)> = shown
            .iter()
            .flat_map(|o| {
                std::iter::once(&o.mesh)
                    .chain(o.parts.iter().map(|p| &p.mesh))
                    .map(|m| (Arc::as_ptr(m) as usize, m.as_ref()))
            })
            .collect();
        gpu.sync_meshes(&meshes);
        let mut draws: Vec<ObjectDraw> = shown
            .iter()
            .map(|o| {
                let selected = session.selection.contains(&o.id);
                let hovered = self.hover == Some(o.id);
                ObjectDraw {
                    mesh: Arc::as_ptr(&o.mesh) as usize,
                    model: o.matrix(),
                    color: linear(object_colour(session, o.filament)),
                    highlight: if page == Page::Preview {
                        0.0
                    } else if selected {
                        0.55
                    } else if hovered {
                        0.25
                    } else {
                        0.0
                    },
                    outside: page == Page::Prepare && !session.fits(o.id),
                }
            })
            .collect();
        // The other volumes: parts in the object's colour, the rest see-through, drawn last.
        let mut glass = Vec::new();
        for o in &shown {
            let selected = session.selection.contains(&o.id);
            for p in &o.parts {
                let colour = match p.kind {
                    CosmicSlicer_model::PartKind::Normal => {
                        let mut c = linear(object_colour(session, o.filament));
                        c[3] = 1.0;
                        c
                    }
                    CosmicSlicer_model::PartKind::Modifier => [0.55, 0.55, 0.6, 0.35],
                    CosmicSlicer_model::PartKind::Negative => [0.15, 0.15, 0.18, 0.45],
                    CosmicSlicer_model::PartKind::SupportEnforcer => [0.2, 0.45, 0.95, 0.4],
                    CosmicSlicer_model::PartKind::SupportBlocker => [0.95, 0.25, 0.2, 0.4],
                };
                let draw = ObjectDraw {
                    mesh: Arc::as_ptr(&p.mesh) as usize,
                    model: o.matrix() * p.matrix(),
                    color: colour,
                    highlight: if page == Page::Prepare && selected { 0.3 } else { 0.0 },
                    outside: false,
                };
                if p.kind.is_solid() {
                    draws.push(draw);
                } else if page == Page::Prepare {
                    glass.push(draw);
                }
            }
        }
        draws.extend(glass);
        gpu.set_draws(draws);
        let mut overlay_geometry = FlatGeometry::default();
        if let Some(r) = &result {
            self.preview_marks(r, t, &mut overlay_geometry);
        }
        if page == Page::Prepare
            && let Some(b) = Self::selection_bounds(session)
        {
            let tool = self.tool.filter(|t| t.has_handles()).unwrap_or(Tool::LayOnFace);
            gizmo::build(tool, &b, t, self.hot, &mut overlay_geometry);
        }
        gpu.set_overlay(&overlay_geometry);
        let ppp = ui.ctx().pixels_per_point();
        let size_px = [(rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32];
        let layers = (page == Page::Preview).then(|| {
            let l = &self.layers;
            [l.z_of(l.top), if l.bottom <= 1 { -1.0 } else { l.z_of(l.bottom - 1) }, l.height]
        });
        let eye = self.camera.back();
        let frame = FrameParams {
            size_px,
            view_proj: self.camera.view_proj(self.aspect()),
            light_dir: (eye + Vec3::new(0.3, -0.2, 0.6)).normalize_or(Vec3::Z),
            bg_top: linear(t.viewport_top),
            bg_bottom: linear(t.viewport_bottom),
            layers,
        };
        match gpu.error() {
            Some(e) => {
                ui.painter().rect_filled(rect, 0.0, t.viewport_bottom);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("The 3D view stopped: {e}"),
                    egui::FontId::proportional(14.0),
                    t.error,
                );
            }
            None => {
                if let Some(id) = gpu.render(&frame) {
                    ui.painter().image(
                        id,
                        rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
            }
        }
        self.gpu = Some(gpu);
        overlay::show(self, ui, rect, session, t, actions);
    }

    /// Travel, retraction and seam marks for the shown layers, and the tool at the end of the
    /// shown moves.
    fn preview_marks(&self, r: &CosmicSlicer_engine::Sliced, t: &Tokens, g: &mut FlatGeometry) {
        const MAX_MARKS: usize = 200_000;
        let layers = &r.preview.layers;
        let (lo, hi) = (self.layers.bottom.saturating_sub(1), self.layers.top.saturating_sub(1));
        let shown = layers.get(lo..=hi.min(layers.len().saturating_sub(1))).unwrap_or_default();
        let v = |p: [f32; 3]| Vec3::new(p[0], p[1], p[2]);
        let mut budget = MAX_MARKS;
        let cross = |g: &mut FlatGeometry, p: Vec3, size: f32, colour: [f32; 4]| {
            g.line(p - Vec3::X * size, p + Vec3::X * size, colour);
            g.line(p - Vec3::Y * size, p + Vec3::Y * size, colour);
            g.line(p, p + Vec3::Z * size * 1.5, colour);
        };
        for l in shown {
            if self.show_travel {
                for (a, b) in r.preview.travels.get(l.travels.clone()).unwrap_or_default().iter().take(budget) {
                    g.line(v(*a), v(*b), linear(t.accent_strong));
                    budget = budget.saturating_sub(1);
                }
            }
            if self.show_retractions {
                for p in r.preview.retractions.get(l.retractions.clone()).unwrap_or_default().iter().take(budget) {
                    cross(g, v(*p), 0.4, linear(egui::Color32::from_rgb(205, 34, 214)));
                    budget = budget.saturating_sub(1);
                }
            }
            if self.show_seams {
                for p in r.preview.seams.get(l.seams.clone()).unwrap_or_default().iter().take(budget) {
                    cross(g, v(*p), 0.4, linear(egui::Color32::from_rgb(230, 230, 230)));
                    budget = budget.saturating_sub(1);
                }
            }
        }
        // The nozzle where the shown moves end.
        let cap = self.segments_loaded;
        let end = layers
            .get(hi)
            .map_or(0, |x| x.segments.start + (x.segments.len() as f32 * self.moves.clamp(0.0, 1.0)).round() as usize);
        if let Some(s) = end.checked_sub(1).filter(|e| *e < cap).and_then(|e| r.preview.segments.get(e)) {
            let tip = v(s.b);
            let c = linear(t.warning);
            for k in 0..8 {
                let a = k as f32 / 8.0 * std::f32::consts::TAU;
                g.line(tip, tip + Vec3::new(a.cos() * 1.2, a.sin() * 1.2, 4.0), c);
            }
        }
    }

    /// Puts `r`'s toolpaths on the GPU (once), colours them for the legend, and sets the
    /// range the sliders show.
    fn upload_toolpaths(&mut self, gpu: &mut Gpu, r: &Arc<CosmicSlicer_engine::Sliced>) {
        let key = Arc::as_ptr(r) as usize;
        if self.segments_key != Some(key) {
            let instances: Vec<gpu::SegmentInstance> = r
                .preview
                .segments
                .iter()
                .map(|s| gpu::SegmentInstance { a: s.a, b: s.b, size: [s.width, s.height] })
                .collect();
            self.segments_loaded = gpu.set_segments(&instances);
            self.segments_key = Some(key);
            self.colours_key = None;
        }
        if self.hidden_features.len() != crate::preview::FEATURES.len() {
            self.hidden_features = vec![false; crate::preview::FEATURES.len()];
        }
        let ckey = (key, self.view_type, self.hidden_features.clone());
        if self.colours_key.as_ref() != Some(&ckey) {
            let (colours, range) = crate::preview::colours(r, self.view_type, &self.hidden_features);
            gpu.set_segment_colors(&colours);
            self.legend_range = range;
            self.colours_key = Some(ckey);
        }
        let layers = &r.preview.layers;
        let l = &self.layers;
        let start = layers.get(l.bottom.saturating_sub(1)).map_or(0, |x| x.segments.start);
        let end = layers.get(l.top.saturating_sub(1)).map_or(0, |x| {
            let n = x.segments.len() as f32;
            x.segments.start + (n * self.moves.clamp(0.0, 1.0)).round() as usize
        });
        let cap = self.segments_loaded;
        gpu.segment_range = (start.min(cap) as u32)..(end.min(cap) as u32);
    }

    fn handle_input(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        rect: Rect,
        session: &mut Session,
        page: Page,
        actions: &mut Vec<Action>,
    ) {
        let mouse = response
            .hover_pos()
            .or(response.interact_pointer_pos())
            .map(|p| Vec2::new(p.x - rect.left(), p.y - rect.top()));
        let selection_box = Self::selection_bounds(session);
        // Hover: handles first, then objects.
        if self.drag.is_none() {
            self.hot = None;
            self.hover = None;
            if let Some(m) = mouse.filter(|_| response.hovered()) {
                if page == Page::Prepare
                    && let (Some(tool), Some(b)) = (self.tool.filter(|t| t.has_handles()), selection_box)
                {
                    self.hot = gizmo::pick(tool, &b, &self.camera, self.size, m);
                }
                if self.hot.is_none() {
                    self.hover = self.pick(session, page, m).map(|h| h.0);
                }
            }
        }

        // Zoom with the wheel, towards the point under the mouse.
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.zoom_delta().ln() * 300.0);
            if scroll.abs() > 0.01 {
                let towards = mouse.filter(|_| session.prefs.zoom_to_mouse).and_then(|m| {
                    let (o, d) = self.ray(m);
                    self.pick(session, page, m)
                        .map(|(_, dist, _)| o + d * dist)
                        .or_else(|| camera::hit_plane(o, d, 0.0))
                });
                self.camera.zoom((-scroll * 0.0015).exp(), towards);
            }
        }

        let modifiers = ui.input(|i| i.modifiers);
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(m) = mouse
        {
            self.drag = Some(self.start_drag(session, page, m, rect));
        } else if response.drag_started_by(egui::PointerButton::Secondary)
            || response.drag_started_by(egui::PointerButton::Middle)
        {
            self.drag = Some(Drag::Pan);
        }

        if let Some(m) = mouse
            && self.drag.is_some()
            && response.dragged()
        {
            let delta = response.drag_delta();
            self.continue_drag(session, m, Vec2::new(delta.x, delta.y), rect, modifiers.shift);
        }
        if response.drag_stopped() {
            match self.drag.take() {
                Some(Drag::Move { ids, moved: true, .. } | Drag::Axis { ids, .. }) => session.settle(&ids),
                Some(Drag::Rotate { id, .. } | Drag::Scale { id, .. }) => session.settle(&[id]),
                _ => {}
            }
        }

        // Clicks select; Lay on face lays the clicked face down.
        if response.clicked_by(egui::PointerButton::Primary)
            && let Some(m) = mouse
        {
            let hit = self.pick(session, page, m);
            if page == Page::Prepare && self.tool == Some(Tool::LayOnFace) {
                if let Some((id, _, tri)) = hit {
                    lay_on_face(session, id, tri);
                }
                return;
            }
            match hit {
                Some((id, ..)) if modifiers.ctrl || modifiers.command || modifiers.shift => {
                    if let Some(i) = session.selection.iter().position(|s| *s == id) {
                        session.selection.remove(i);
                    } else {
                        session.selection.push(id);
                    }
                }
                Some((id, ..)) => {
                    session.selection = vec![id];
                    if let Some(o) = session.model.get(id) {
                        session.plate = o.plate;
                    }
                }
                None => {
                    session.selection.clear();
                    let (o, d) = self.ray(m);
                    if page == Page::Prepare
                        && let Some(p) = camera::hit_plane(o, d, 0.0).and_then(|p| session.plate_at(p.x, p.y))
                    {
                        session.plate = p;
                    }
                }
            }
        }
        if response.double_clicked() && self.hover.is_some() {
            actions.push(Action::Settings(TabKind::Process));
        }
        if response.secondary_clicked() {
            self.context_object = mouse.and_then(|m| self.pick(session, page, m)).map(|h| h.0);
            if let Some(id) = self.context_object
                && !session.selection.contains(&id)
            {
                session.selection = vec![id];
            }
        }
    }

    fn start_drag(&mut self, session: &mut Session, page: Page, m: Vec2, rect: Rect) -> Drag {
        if page != Page::Prepare {
            return Drag::Orbit;
        }
        let selection = session.selection.clone();
        if let (Some(handle), Some(b)) = (self.hot, Self::selection_bounds(session)) {
            let centre = b.centre();
            let screen_centre = self
                .camera
                .project(centre, self.size)
                .map_or(rect.center(), |p| Pos2::new(p.x + rect.left(), p.y + rect.top()));
            let mouse = Pos2::new(m.x + rect.left(), m.y + rect.top());
            let first = selection.first().copied();
            match (handle, first) {
                (Handle::Axis(axis), _) => {
                    let (o, d) = self.ray(m);
                    let last = gizmo::axis_param(centre, axis, o, d).unwrap_or(0.0);
                    session.checkpoint();
                    return Drag::Axis { axis, ids: selection, origin: centre, last };
                }
                (Handle::Ring(axis), Some(id)) => {
                    let start = session.model.get(id).map(|o| o.transform).unwrap_or_default();
                    let v = mouse - screen_centre;
                    let a = [Vec3::X, Vec3::Y, Vec3::Z][axis];
                    let sign = if a.dot(self.camera.back()) >= 0.0 { -1.0 } else { 1.0 };
                    session.checkpoint();
                    return Drag::Rotate { axis, id, centre: screen_centre, start_angle: v.y.atan2(v.x), start, sign };
                }
                (Handle::Corner, Some(id)) => {
                    let start = session.model.get(id).map(|o| o.transform).unwrap_or_default();
                    session.checkpoint();
                    return Drag::Scale {
                        id,
                        centre: screen_centre,
                        start_len: (mouse - screen_centre).length().max(1.0),
                        start,
                    };
                }
                _ => {}
            }
        }
        if self.tool == Some(Tool::LayOnFace) {
            return Drag::Orbit;
        }
        match self.pick(session, page, m) {
            Some((id, dist, _)) => {
                if !session.selection.contains(&id) {
                    session.selection = vec![id];
                }
                let (o, d) = self.ray(m);
                let z = (o + d * dist).z;
                let last = camera::hit_plane(o, d, z).unwrap_or(o + d * dist);
                Drag::Move { ids: session.selection.clone(), last, z, moved: false }
            }
            None => Drag::Orbit,
        }
    }

    /// `snap` (Shift held) turns in 15° steps.
    fn continue_drag(&mut self, session: &mut Session, m: Vec2, delta: Vec2, rect: Rect, snap: bool) {
        let ray = self.ray(m);
        let mouse = Pos2::new(m.x + rect.left(), m.y + rect.top());
        match &mut self.drag {
            Some(Drag::Orbit) => self.camera.orbit(delta),
            Some(Drag::Pan) => self.camera.pan(delta, self.size.y),
            Some(Drag::Move { ids, last, z, moved }) => {
                if let Some(p) = camera::hit_plane(ray.0, ray.1, *z) {
                    let step = Vec3::new(p.x - last.x, p.y - last.y, 0.0);
                    // Rays near the horizon throw the point far away; ignore wild jumps.
                    if step.length() < 2000.0 && step.length() > 0.0 {
                        if !*moved {
                            session.checkpoint();
                            *moved = true;
                        }
                        session.nudge(ids, step);
                        *last = p;
                    }
                }
            }
            Some(Drag::Axis { axis, ids, origin, last }) => {
                if let Some(tp) = gizmo::axis_param(*origin, *axis, ray.0, ray.1) {
                    let step = tp - *last;
                    if step.abs() < 2000.0 {
                        let dir = [Vec3::X, Vec3::Y, Vec3::Z][*axis];
                        session.nudge(ids, dir * step);
                        *last = tp;
                    }
                }
            }
            Some(Drag::Rotate { axis, id, centre, start_angle, start, sign }) => {
                let v = mouse - *centre;
                let mut angle = (v.y.atan2(v.x) - *start_angle) * *sign;
                if snap {
                    let step = 15f32.to_radians();
                    angle = (angle / step).round() * step;
                }
                let a = [Vec3::X, Vec3::Y, Vec3::Z][*axis];
                let mut t = *start;
                t.set_quat(Quat::from_axis_angle(a, angle) * start.quat());
                if let Some(o) = session.model.get_mut(*id) {
                    o.transform = t;
                    o.drop_to_bed();
                }
                session.revision = session.revision.wrapping_add(1);
            }
            Some(Drag::Scale { id, centre, start_len, start }) => {
                let factor = ((mouse - *centre).length() / *start_len).clamp(0.01, 100.0);
                if let Some(o) = session.model.get_mut(*id) {
                    o.transform.scale = start.scale * factor;
                    o.drop_to_bed();
                }
                session.revision = session.revision.wrapping_add(1);
            }
            None => {}
        }
    }

    fn context_menu(
        &mut self,
        response: &egui::Response,
        session: &mut Session,
        t: &Tokens,
        actions: &mut Vec<Action>,
    ) {
        if session.page != Page::Prepare {
            return;
        }
        let object = self.context_object;
        crate::ui_kit::context_menu(response, t, |ui| {
            ui.set_min_width(220.0);
            match object {
                Some(id) => crate::menus::object_menu(ui, session, id, actions),
                None => crate::menus::canvas_menu(ui, session, actions),
            }
        });
    }
}

/// Turns object `id` so triangle `tri` faces down, resting on the plate.
pub fn lay_on_face(session: &mut Session, id: u64, tri: usize) {
    let Some(o) = session.model.get(id) else { return };
    let Some(corners) = o.mesh.triangles.get(tri).and_then(|t| o.mesh.corners(t)) else { return };
    let m = o.matrix();
    let [a, b, c] = corners.map(|p| m.transform_point3(p));
    let n = (b - a).cross(c - a).normalize_or_zero();
    if n == Vec3::ZERO {
        return;
    }
    let mut t = o.transform;
    t.set_quat(Quat::from_rotation_arc(n, Vec3::NEG_Z) * t.quat());
    session.set_transform(id, t);
    if let Some(o) = session.model.get_mut(id) {
        o.drop_to_bed();
    }
}
