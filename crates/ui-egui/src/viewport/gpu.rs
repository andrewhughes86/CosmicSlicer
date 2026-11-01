//! wgpu backend for the 3D canvas.
//!
//! Renders into an offscreen multisampled target with its own depth buffer, resolves it into
//! a texture registered with egui, and the canvas paints that texture as an image. Objects
//! keep their own vertex buffers and take their colour from a per-object uniform, so hover
//! and selection never re-upload geometry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui_wgpu::RenderState;
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

/// Pipelines write through an sRGB view so shading and blending happen in linear space...
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// ...while egui samples native textures as gamma-encoded `Rgba8Unorm`.
const STORAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;
/// Uniform offsets must be multiples of this on every backend.
const OBJECT_STRIDE: u64 = 256;
/// Most vertices in one buffer (a multiple of 2 and 3).
const MAX_CHUNK_VERTICES: usize = 6 << 20;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameUniforms {
    view_proj: [[f32; 4]; 4],
    light_dir: [f32; 4],
    bg_top: [f32; 4],
    bg_bottom: [f32; 4],
    layers: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObjectUniforms {
    model: [[f32; 4]; 4],
    /// Inverse transpose of `model`, for normals.
    normal: [[f32; 4]; 4],
    color: [f32; 4],
    flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FlatVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// One toolpath segment: from `a` to `b` (top of the bead), `size` = (width, height).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SegmentInstance {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub size: [f32; 2],
}

/// Flat-coloured geometry: filled triangles and lines.
#[derive(Default)]
pub struct FlatGeometry {
    pub triangles: Vec<FlatVertex>,
    pub lines: Vec<FlatVertex>,
}

impl FlatGeometry {
    pub fn line(&mut self, a: Vec3, b: Vec3, color: [f32; 4]) {
        self.lines.push(FlatVertex { pos: a.to_array(), color });
        self.lines.push(FlatVertex { pos: b.to_array(), color });
    }

    pub fn triangle(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 4]) {
        for p in [a, b, c] {
            self.triangles.push(FlatVertex { pos: p.to_array(), color });
        }
    }
}

/// One object drawn this frame: which uploaded mesh, where, and how.
#[derive(Clone, Copy, Debug)]
pub struct ObjectDraw {
    pub mesh: usize,
    pub model: Mat4,
    pub color: [f32; 4],
    pub highlight: f32,
    pub outside: bool,
}

/// A mesh's triangles, three vertices each with the face normal, ready to upload.
pub fn mesh_vertices(mesh: &CosmicSlicer_model::Mesh) -> Vec<MeshVertex> {
    let mut out = Vec::with_capacity(mesh.triangles.len() * 3);
    for t in &mesh.triangles {
        let Some([a, b, c]) = mesh.corners(t) else { continue };
        let n = (b - a).cross(c - a).normalize_or_zero().to_array();
        for p in [a, b, c] {
            out.push(MeshVertex { pos: p.to_array(), normal: n });
        }
    }
    out
}

/// Per-frame inputs for [`Gpu::render`].
pub struct FrameParams {
    pub size_px: [u32; 2],
    pub view_proj: Mat4,
    pub light_dir: Vec3,
    pub bg_top: [f32; 4],
    pub bg_bottom: [f32; 4],
    /// (top, bottom, layer height) of the shown layers in Preview.
    pub layers: Option<[f32; 3]>,
}

fn chunks<T: bytemuck::Pod>(device: &wgpu::Device, label: &str, data: &[T]) -> Vec<(wgpu::Buffer, u32)> {
    let by_limit = device.limits().max_buffer_size / std::mem::size_of::<T>().max(1) as u64;
    let chunk = (usize::try_from(by_limit).unwrap_or(usize::MAX).min(MAX_CHUNK_VERTICES) / 6 * 6).max(6);
    data.chunks(chunk)
        .filter_map(|part| {
            let count = u32::try_from(part.len()).ok()?;
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(part),
                usage: wgpu::BufferUsages::VERTEX,
            });
            Some((buf, count))
        })
        .collect()
}

#[derive(Default)]
struct FlatBuffers {
    triangles: Vec<(wgpu::Buffer, u32)>,
    lines: Vec<(wgpu::Buffer, u32)>,
}

impl FlatBuffers {
    fn upload(device: &wgpu::Device, label: &str, g: &FlatGeometry) -> Self {
        Self { triangles: chunks(device, label, &g.triangles), lines: chunks(device, label, &g.lines) }
    }
}

struct Targets {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    resolve: wgpu::TextureView,
    depth: wgpu::TextureView,
}

pub struct Gpu {
    render_state: RenderState,
    /// The first GPU validation error reported for the device; wgpu would otherwise panic.
    error: Arc<Mutex<Option<String>>>,
    target_format: wgpu::TextureFormat,
    frame_uniforms: wgpu::Buffer,
    frame_group: wgpu::BindGroup,
    object_layout: wgpu::BindGroupLayout,
    object_uniforms: wgpu::Buffer,
    object_group: wgpu::BindGroup,
    object_capacity: u64,
    background: wgpu::RenderPipeline,
    mesh: wgpu::RenderPipeline,
    flat_triangles: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    overlay_lines: wgpu::RenderPipeline,
    /// Uploaded meshes by key (the address of the model's shared mesh).
    meshes: HashMap<usize, Vec<(wgpu::Buffer, u32)>>,
    draws: Vec<ObjectDraw>,
    bed: FlatBuffers,
    overlay: FlatBuffers,
    segments_pipeline: wgpu::RenderPipeline,
    /// Toolpath instances and their colours (RGBA8), and the range drawn.
    segments: Option<(wgpu::Buffer, u32)>,
    segment_colors: Option<wgpu::Buffer>,
    pub segment_range: std::ops::Range<u32>,
    targets: Option<Targets>,
    texture_id: Option<egui::TextureId>,
}

impl Gpu {
    pub fn new(render_state: &RenderState) -> Self {
        let device = &render_state.device;
        let error: Arc<Mutex<Option<String>>> = Arc::default();
        let sink = Arc::clone(&error);
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            log::error!("wgpu: {e}");
            if let Ok(mut slot) = sink.lock() {
                slot.get_or_insert_with(|| e.to_string().lines().take(3).collect::<Vec<_>>().join(" "));
            }
        }));
        let view_formats =
            render_state.adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::VIEW_FORMATS);
        let target_format = if view_formats { COLOR_FORMAT } else { STORAGE_FORMAT };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
        });
        let uniform_entry = |dynamic: bool| wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: None,
            },
            count: None,
        };
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas frame"),
            entries: &[uniform_entry(false)],
        });
        let object_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas object"),
            entries: &[uniform_entry(true)],
        });
        let frame_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas frame"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas frame"),
            layout: &frame_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: frame_uniforms.as_entire_binding() }],
        });
        let (object_uniforms, object_group) = Self::object_buffer(device, &object_layout, 64);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas"),
            bind_group_layouts: &[Some(&frame_layout), Some(&object_layout)],
            immediate_size: 0,
        });

        let mesh_layout = Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        });
        let flat_layout = Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<FlatVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        });
        let blend_target = [Some(wgpu::ColorTargetState {
            format: target_format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let multisample = wgpu::MultisampleState { count: SAMPLES, mask: !0, alpha_to_coverage_enabled: false };
        let depth = |write: bool, compare: wgpu::CompareFunction, bias: wgpu::DepthBiasState| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(compare),
            stencil: wgpu::StencilState::default(),
            bias,
        };
        let pipeline = |label: &str,
                        entry: (&str, &str),
                        buffer: &Option<wgpu::VertexBufferLayout>,
                        topology,
                        depth_state: wgpu::DepthStencilState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(entry.0),
                    compilation_options: Default::default(),
                    buffers: std::slice::from_ref(buffer),
                },
                primitive: wgpu::PrimitiveState { topology, cull_mode: None, ..Default::default() },
                depth_stencil: Some(depth_state),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry.1),
                    compilation_options: Default::default(),
                    targets: &blend_target,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let less = wgpu::CompareFunction::LessEqual;
        let no_bias = wgpu::DepthBiasState::default();
        let mesh = pipeline(
            "canvas mesh",
            ("vs_mesh", "fs_mesh"),
            &mesh_layout,
            wgpu::PrimitiveTopology::TriangleList,
            depth(true, less, wgpu::DepthBiasState { constant: 2, slope_scale: 1.5, clamp: 0.0 }),
        );
        // The bed sits a little behind the objects standing on it.
        let flat_triangles = pipeline(
            "canvas flat",
            ("vs_flat", "fs_flat"),
            &flat_layout,
            wgpu::PrimitiveTopology::TriangleList,
            depth(true, less, wgpu::DepthBiasState { constant: 4, slope_scale: 2.0, clamp: 0.0 }),
        );
        let lines = pipeline(
            "canvas lines",
            ("vs_flat", "fs_flat"),
            &flat_layout,
            wgpu::PrimitiveTopology::LineList,
            depth(true, less, no_bias),
        );
        let overlay_lines = pipeline(
            "canvas overlay",
            ("vs_flat", "fs_flat"),
            &flat_layout,
            wgpu::PrimitiveTopology::LineList,
            depth(false, wgpu::CompareFunction::Always, no_bias),
        );
        let background = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas background"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_background"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth(false, wgpu::CompareFunction::Always, no_bias)),
            multisample,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_background"),
                compilation_options: Default::default(),
                targets: &blend_target,
            }),
            multiview_mask: None,
            cache: None,
        });

        let segment_layouts = [
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<SegmentInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: 4,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![3 => Unorm8x4],
            }),
        ];
        let segments_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas toolpaths"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_segment"),
                compilation_options: Default::default(),
                buffers: &segment_layouts,
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(depth(true, less, no_bias)),
            multisample,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_segment"),
                compilation_options: Default::default(),
                targets: &blend_target,
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            render_state: render_state.clone(),
            error,
            target_format,
            frame_uniforms,
            frame_group,
            object_layout,
            object_uniforms,
            object_group,
            object_capacity: 64,
            background,
            mesh,
            flat_triangles,
            lines,
            overlay_lines,
            meshes: HashMap::new(),
            draws: Vec::new(),
            bed: FlatBuffers::default(),
            overlay: FlatBuffers::default(),
            segments_pipeline,
            segments: None,
            segment_colors: None,
            segment_range: 0..0,
            targets: None,
            texture_id: None,
        }
    }

    fn object_buffer(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        capacity: u64,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas objects"),
            size: capacity.max(1) * OBJECT_STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas objects"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<ObjectUniforms>() as u64),
                }),
            }],
        });
        (buffer, group)
    }

    /// The first GPU error reported, if any; the canvas stops drawing when there is one.
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    /// Uploads the meshes not yet on the GPU and frees those no longer used.
    pub fn sync_meshes(&mut self, meshes: &[(usize, &CosmicSlicer_model::Mesh)]) {
        let device = &self.render_state.device;
        self.meshes.retain(|k, _| meshes.iter().any(|(m, _)| m == k));
        for (key, mesh) in meshes {
            if !self.meshes.contains_key(key) {
                self.meshes.insert(*key, chunks(device, "object", &mesh_vertices(mesh)));
            }
        }
    }

    pub fn set_draws(&mut self, draws: Vec<ObjectDraw>) {
        let needed = (draws.len() as u64).max(1);
        if needed > self.object_capacity {
            let capacity = needed.next_power_of_two();
            let (buffer, group) = Self::object_buffer(&self.render_state.device, &self.object_layout, capacity);
            self.object_uniforms = buffer;
            self.object_group = group;
            self.object_capacity = capacity;
        }
        self.draws = draws;
    }

    /// Most toolpath segments drawn, whatever the device allows (one buffer each).
    fn max_segments(&self) -> usize {
        let limit = self.render_state.device.limits().max_buffer_size / std::mem::size_of::<SegmentInstance>() as u64;
        usize::try_from(limit).unwrap_or(usize::MAX).min(16_000_000)
    }

    /// Replaces the toolpaths; returns how many fit.
    pub fn set_segments(&mut self, segments: &[SegmentInstance]) -> usize {
        let n = segments.len().min(self.max_segments());
        self.segments = (n > 0).then(|| {
            let buf = self.render_state.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("toolpaths"),
                contents: bytemuck::cast_slice(segments.get(..n).unwrap_or_default()),
                usage: wgpu::BufferUsages::VERTEX,
            });
            (buf, n as u32)
        });
        self.segment_colors = None;
        self.segment_range = 0..0;
        n
    }

    /// Colours for the toolpaths, one RGBA per segment; alpha 0 hides one.
    pub fn set_segment_colors(&mut self, colors: &[[u8; 4]]) {
        let n = self.segments.as_ref().map_or(0, |s| s.1 as usize).min(colors.len());
        self.segment_colors = (n > 0).then(|| {
            self.render_state.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("toolpath colours"),
                contents: bytemuck::cast_slice(colors.get(..n).unwrap_or_default()),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
    }

    pub fn clear_segments(&mut self) {
        self.segments = None;
        self.segment_colors = None;
        self.segment_range = 0..0;
    }

    pub fn set_bed(&mut self, g: &FlatGeometry) {
        self.bed = FlatBuffers::upload(&self.render_state.device, "bed", g);
    }

    pub fn set_overlay(&mut self, g: &FlatGeometry) {
        self.overlay = FlatBuffers::upload(&self.render_state.device, "overlay", g);
    }

    fn ensure_targets(&mut self, size: [u32; 2]) -> Option<egui::TextureId> {
        let device = &self.render_state.device;
        let target_format = self.target_format;
        let srgb_views = target_format == COLOR_FORMAT;
        let max = device.limits().max_texture_dimension_2d.max(1);
        let size = [size[0].clamp(1, max), size[1].clamp(1, max)];
        if let (Some(t), Some(id)) = (&self.targets, self.texture_id)
            && t.size == size
        {
            return Some(id);
        }
        let texture = |label: &str, format: wgpu::TextureFormat, samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: if format == STORAGE_FORMAT && srgb_views { &[COLOR_FORMAT] } else { &[] },
            })
        };
        let srgb_view = |t: &wgpu::Texture| {
            t.create_view(&wgpu::TextureViewDescriptor { format: Some(target_format), ..Default::default() })
        };
        let msaa = texture("canvas msaa", STORAGE_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let color = texture(
            "canvas color",
            STORAGE_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = texture("canvas depth", DEPTH_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let display = color.create_view(&wgpu::TextureViewDescriptor::default());
        let targets = Targets {
            size,
            msaa: srgb_view(&msaa),
            resolve: srgb_view(&color),
            depth: depth.create_view(&wgpu::TextureViewDescriptor::default()),
        };
        let mut renderer = self.render_state.renderer.write();
        let id = match self.texture_id {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(device, &display, wgpu::FilterMode::Linear, id);
                id
            }
            None => renderer.register_native_texture(device, &display, wgpu::FilterMode::Linear),
        };
        drop(renderer);
        self.targets = Some(targets);
        self.texture_id = Some(id);
        Some(id)
    }

    /// Draws one frame and returns the egui texture holding it.
    pub fn render(&mut self, p: &FrameParams) -> Option<egui::TextureId> {
        let id = self.ensure_targets(p.size_px)?;
        let targets = self.targets.as_ref()?;
        let layers = p.layers.map_or([0.0; 4], |[top, bottom, h]| [top, bottom, h, 1.0]);
        let frame = FrameUniforms {
            view_proj: p.view_proj.to_cols_array_2d(),
            light_dir: p
                .light_dir
                .normalize_or(Vec3::Z)
                .extend(if self.target_format == COLOR_FORMAT { 0.0 } else { 1.0 })
                .to_array(),
            bg_top: p.bg_top,
            bg_bottom: p.bg_bottom,
            layers,
        };
        let queue = &self.render_state.queue;
        queue.write_buffer(&self.frame_uniforms, 0, bytemuck::bytes_of(&frame));
        let count = self.draws.len().min(self.object_capacity as usize);
        let mut staging = vec![0u8; count * OBJECT_STRIDE as usize];
        for (i, d) in self.draws.iter().enumerate().take(count) {
            let normal = d.model.inverse().transpose();
            let mirrored = d.model.determinant() < 0.0;
            let u = ObjectUniforms {
                model: d.model.to_cols_array_2d(),
                normal: if normal.is_finite() { normal.to_cols_array_2d() } else { Mat4::IDENTITY.to_cols_array_2d() },
                color: d.color,
                flags: [d.highlight, if d.outside { 1.0 } else { 0.0 }, if mirrored { 1.0 } else { 0.0 }, 0.0],
            };
            let start = i * OBJECT_STRIDE as usize;
            if let Some(slot) = staging.get_mut(start..start + std::mem::size_of::<ObjectUniforms>()) {
                slot.copy_from_slice(bytemuck::bytes_of(&u));
            }
        }
        if !staging.is_empty() {
            queue.write_buffer(&self.object_uniforms, 0, &staging);
        }

        let mut encoder =
            self.render_state.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("canvas") });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa,
                    depth_slice: None,
                    resolve_target: Some(&targets.resolve),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_bind_group(0, &self.frame_group, &[]);
            rp.set_bind_group(1, &self.object_group, &[0]);
            rp.set_pipeline(&self.background);
            rp.draw(0..3, 0..1);
            let draw =
                |rp: &mut wgpu::RenderPass<'_>, pipeline: &wgpu::RenderPipeline, list: &[(wgpu::Buffer, u32)]| {
                    if list.is_empty() {
                        return;
                    }
                    rp.set_pipeline(pipeline);
                    for (buf, n) in list {
                        rp.set_vertex_buffer(0, buf.slice(..));
                        rp.draw(0..*n, 0..1);
                    }
                };
            draw(&mut rp, &self.flat_triangles, &self.bed.triangles);
            draw(&mut rp, &self.lines, &self.bed.lines);
            for (i, d) in self.draws.iter().enumerate().take(count) {
                let Some(chunks) = self.meshes.get(&d.mesh) else { continue };
                rp.set_bind_group(1, &self.object_group, &[(i as u64 * OBJECT_STRIDE) as u32]);
                draw(&mut rp, &self.mesh, chunks);
            }
            rp.set_bind_group(1, &self.object_group, &[0]);
            if let (Some((buf, n)), Some(colors)) = (&self.segments, &self.segment_colors) {
                let end = self.segment_range.end.min(*n);
                let start = self.segment_range.start.min(end);
                if end > start {
                    rp.set_pipeline(&self.segments_pipeline);
                    rp.set_vertex_buffer(0, buf.slice(..));
                    rp.set_vertex_buffer(1, colors.slice(..));
                    rp.draw(0..24, start..end);
                }
            }
            draw(&mut rp, &self.flat_triangles, &self.overlay.triangles);
            draw(&mut rp, &self.overlay_lines, &self.overlay.lines);
        }
        queue.submit([encoder.finish()]);
        Some(id)
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Some(id) = self.texture_id.take() {
            self.render_state.renderer.write().free_texture(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    /// The shader parses and validates without a GPU, so a mistake fails CI on every platform
    /// rather than the first time the 3D view opens.
    #[test]
    fn the_scene_shader_is_valid() {
        use wgpu::naga;
        let source = include_str!("scene.wgsl");
        let module = naga::front::wgsl::parse_str(source);
        assert!(module.is_ok(), "{}", module.as_ref().err().map(|e| e.emit_to_string(source)).unwrap_or_default());
        let Ok(module) = module else { return };
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty());
        let info = validator.validate(&module);
        assert!(info.is_ok(), "{:?}", info.err());
        for entry in
            ["vs_mesh", "fs_mesh", "vs_flat", "fs_flat", "vs_background", "fs_background", "vs_segment", "fs_segment"]
        {
            assert!(module.entry_points.iter().any(|e| e.name == entry), "{entry}");
        }
    }
}
