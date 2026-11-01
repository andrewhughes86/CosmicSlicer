struct Frame {
    view_proj: mat4x4<f32>,
    // xyz: light direction; w = 1 when the target has no sRGB view (WebGL), so the
    // shader encodes sRGB itself.
    light_dir: vec4<f32>,
    bg_top: vec4<f32>,
    bg_bottom: vec4<f32>,
    // x: top of the shown layers, y: bottom, z: layer height, w: 1 in Preview.
    layers: vec4<f32>,
};

struct Object {
    model: mat4x4<f32>,
    normal: mat4x4<f32>,
    color: vec4<f32>,
    // x: highlight (0..1), y: 1 when outside the printable volume, z: 1 when mirrored.
    flags: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Frame;
@group(1) @binding(0) var<uniform> obj: Object;

/// Linear to sRGB when the target cannot do it (see `light_dir.w`).
fn encode(c: vec4<f32>) -> vec4<f32> {
    if (u.light_dir.w < 0.5) {
        return c;
    }
    let rgb = clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = rgb * 12.92;
    let hi = 1.055 * pow(rgb, vec3<f32>(1.0 / 2.4)) - 0.055;
    return vec4<f32>(select(hi, lo, rgb <= vec3<f32>(0.0031308)), c.a);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

// ---- objects ----

struct MeshIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) world: vec3<f32>,
};

@vertex
fn vs_mesh(v: MeshIn) -> MeshOut {
    var o: MeshOut;
    let world = obj.model * vec4<f32>(v.pos, 1.0);
    o.clip = u.view_proj * world;
    o.normal = (obj.normal * vec4<f32>(v.normal, 0.0)).xyz;
    o.world = world.xyz;
    return o;
}

fn shade(n: vec3<f32>, base: vec3<f32>) -> vec3<f32> {
    let key = max(dot(n, normalize(u.light_dir.xyz)), 0.0);
    let fill = max(dot(n, normalize(vec3<f32>(-0.4, 0.6, 0.3))), 0.0);
    let sky = 0.5 + 0.5 * n.z;
    return base * (0.22 + 0.62 * key + 0.18 * fill + 0.12 * sky);
}

@fragment
fn fs_mesh(i: MeshOut, @builtin(front_facing) facing: bool) -> @location(0) vec4<f32> {
    var n = normalize(i.normal);
    // A mirrored object's triangles wind the other way.
    let front = facing != (obj.flags.z > 0.5);
    let preview = u.layers.w > 0.5;
    if (preview) {
        // Layers above the top slider or below the bottom one are not shown.
        if (i.world.z > u.layers.x + 1e-4 || i.world.z < u.layers.y - 1e-4) {
            discard;
        }
    }
    if (!front) {
        n = -n;
    }
    var base = obj.color.rgb;
    if (preview) {
        // The feature colours of OrcaSlicer's legend, by the way a face points.
        if (!front) {
            base = srgb_to_linear(vec3<f32>(176.0, 48.0, 41.0) / 255.0);   // sparse infill
        } else if (n.z > 0.7) {
            base = srgb_to_linear(vec3<f32>(240.0, 64.0, 64.0) / 255.0);   // top surface
        } else if (n.z < -0.7) {
            base = srgb_to_linear(vec3<f32>(102.0, 92.0, 199.0) / 255.0);  // bottom surface
        } else {
            base = srgb_to_linear(vec3<f32>(255.0, 125.0, 56.0) / 255.0);  // outer wall
        }
        let h = max(u.layers.z, 0.01);
        let layer = floor(i.world.z / h);
        if (layer - 2.0 * floor(layer / 2.0) > 0.5) {
            base = base * 0.86;
        }
        // The cut face at the top slider reads as the layer being printed.
        if (!front) {
            return encode(vec4<f32>(base * 0.9, 1.0));
        }
    }
    var c = shade(n, base);
    if (obj.flags.y > 0.5) {
        c = mix(c, srgb_to_linear(vec3<f32>(0.97, 0.44, 0.44)), 0.55);
    }
    c = mix(c, c * 0.6 + vec3<f32>(0.25, 0.75, 0.9) * 0.55, obj.flags.x);
    return encode(vec4<f32>(c, obj.color.a));
}

// ---- flat geometry: the bed, its grid, gizmos ----

struct FlatIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct FlatOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_flat(v: FlatIn) -> FlatOut {
    var o: FlatOut;
    o.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    o.color = v.color;
    return o;
}

@fragment
fn fs_flat(i: FlatOut) -> @location(0) vec4<f32> {
    return encode(i.color);
}

// ---- background ----

struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) t: f32,
};

@vertex
fn vs_background(@builtin(vertex_index) index: u32) -> BgOut {
    // One triangle covering the screen.
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(index & 2u) * 2.0 - 1.0;
    var o: BgOut;
    o.clip = vec4<f32>(x, y, 0.0, 1.0);
    o.t = y * 0.5 + 0.5;
    return o;
}

@fragment
fn fs_background(i: BgOut) -> @location(0) vec4<f32> {
    return encode(mix(u.bg_bottom, u.bg_top, clamp(i.t, 0.0, 1.0)));
}

// ---- toolpaths: each extrusion a box from a to b, built here from its instance ----

struct SegOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_segment(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    @location(2) size: vec2<f32>,
    @location(3) color: vec4<f32>,
) -> SegOut {
    var d = b.xy - a.xy;
    let len = length(d);
    if (len < 1e-6) {
        d = vec2<f32>(1.0, 0.0);
    } else {
        d = d / len;
    }
    let side = vec3<f32>(-d.y, d.x, 0.0);
    let along = vec3<f32>(d, 0.0);
    let half = size.x * 0.5;
    // Four faces (top, right, bottom, left), two triangles each.
    let face = vi / 6u;
    let corner = vi % 6u;
    // Which end of the segment, and which edge of the face's cross-section.
    var ends = array<u32, 6>(0u, 0u, 1u, 0u, 1u, 1u);
    var edges = array<u32, 6>(0u, 1u, 1u, 0u, 1u, 0u);
    let e = ends[corner];
    let k = edges[corner];
    // Cross-section corners: (side, up) for left-top, right-top, right-bottom, left-bottom.
    var sx = array<f32, 4>(-1.0, 1.0, 1.0, -1.0);
    var sz = array<f32, 4>(0.0, 0.0, -1.0, -1.0);
    let c = (face + k) % 4u;
    // The top is a little narrower than the bottom: beads are rounded.
    let squeeze = select(1.0, 0.85, sz[c] == 0.0);
    let p0 = select(a, b, e == 1u);
    // Ends reach half a width further, so corners close.
    let reach = select(-half, half, e == 1u);
    let pos = p0 + along * reach + side * (sx[c] * half * squeeze) + vec3<f32>(0.0, 0.0, sz[c] * size.y);
    var normals = array<vec3<f32>, 4>(vec3<f32>(0.0, 0.0, 1.0), side, vec3<f32>(0.0, 0.0, -1.0), -side);
    var o: SegOut;
    o.clip = u.view_proj * vec4<f32>(pos, 1.0);
    o.normal = normals[face];
    o.color = color;
    return o;
}

@fragment
fn fs_segment(i: SegOut) -> @location(0) vec4<f32> {
    if (i.color.a < 0.01) {
        discard;
    }
    let base = srgb_to_linear(i.color.rgb);
    let n = normalize(i.normal);
    return encode(vec4<f32>(shade(n, base) * 1.05, 1.0));
}
