struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    lod0_origin: vec4<i32>, // x = origin_cx, y = origin_cz, z = 0/1 mask enabled
    lod0_mask: array<vec4<u32>, 512>, // 256x256 chunk bitmask (radius 128 chunks) of active LOD 0 chunks
};

@group(0) @binding(0)
var<uniform> u_cam: CameraUniform;

@group(0) @binding(1)
var u_atlas_tex: texture_2d<f32>;

@group(0) @binding(2)
var u_atlas_samp: sampler;

struct PackedQuadBuffer {
    origin_x: f32,
    origin_z: f32,
    quads: array<vec2<u32>>,
};

@group(1) @binding(0)
var<storage, read> u_packed: PackedQuadBuffer;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world_pos: vec3<f32>,
};

struct VsOutPacked {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) color_packed: u32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = u_cam.view_proj * vec4<f32>(in.pos, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.world_pos = in.pos;
    return out;
}

@vertex
fn vs_packed_quad(@builtin(vertex_index) vid: u32) -> VsOutPacked {
    // Indexed via shared_quad_ibuf (4 vertices per quad -> full post-transform vertex cache
    // reuse and 100% 32-wide SIMD occupancy on Apple Silicon / Vulkan / DX12!)
    let quad_idx = vid >> 2u;
    let corner = vid & 3u;
    let q = u_packed.quads[quad_idx];
    let w0 = q.x;
    let w1 = q.y;

    let lx = f32(w0 & 0x3Fu);
    let lz = f32((w0 >> 6u) & 0x3Fu);
    let y0 = f32(i32((w0 >> 12u) & 0xFFFu) - 1024);
    let du = f32(((w0 >> 24u) & 0xFu) + 1u);
    let dv = f32(((w0 >> 28u) & 0xFu) + 1u);

    let face = w1 & 0x7u;
    let tile_col = f32((w1 >> 3u) & 0x1Fu);
    let tile_row = f32((w1 >> 8u) & 0x1Fu);
    let is_liquid = (w1 & 0x80000000u) != 0u;

    let x0 = u_packed.origin_x + lx;
    let z0 = u_packed.origin_z + lz;

    // Corner bitflags: TL=0, TR=1, BR=2, BL=3
    let is_right = (corner == 1u || corner == 2u);
    let is_bottom = (corner == 2u || corner == 3u);

    var pos: vec3<f32>;
    switch face {
        case 0u: {
            // East (+X)
            pos = vec3<f32>(x0 + 1.0, select(y0 + dv, y0, is_bottom), select(z0 + du, z0, is_right));
        }
        case 1u: {
            // West (-X)
            pos = vec3<f32>(x0, select(y0 + dv, y0, is_bottom), select(z0, z0 + du, is_right));
        }
        case 2u: {
            // Up (+Y): liquid top surface sits at y0 + 0.875
            let up_y = select(y0 + 1.0, y0 + 0.875, is_liquid);
            pos = vec3<f32>(select(x0, x0 + du, is_right), up_y, select(z0, z0 + dv, is_bottom));
        }
        case 3u: {
            // Down (-Y)
            pos = vec3<f32>(select(x0, x0 + du, is_right), y0, select(z0 + dv, z0, is_bottom));
        }
        case 4u: {
            // South (+Z)
            pos = vec3<f32>(select(x0, x0 + du, is_right), select(y0 + dv, y0, is_bottom), z0 + 1.0);
        }
        default: {
            // North (-Z)
            pos = vec3<f32>(select(x0 + du, x0, is_right), select(y0 + dv, y0, is_bottom), z0);
        }
    }

    let u_span = select(0.001, du - 0.001, is_right);
    let v_span = select(0.001, dv - 0.001, is_bottom);
    let uv = vec2<f32>(tile_col * 32.0 + 2.0 + u_span, tile_row * 32.0 + 2.0 + v_span);

    var out: VsOutPacked;
    out.clip_pos = u_cam.view_proj * vec4<f32>(pos, 1.0);
    out.uv = uv;
    // Pass packed 6-bit RGB + bit 18 (is_liquid) flat per triangle (3.85x less TBDR varying bandwidth!)
    out.color_packed = w1 >> 13u;
    return out;
}

fn unpack_flat_color(cp: u32) -> vec4<f32> {
    let r = f32(cp & 0x3Fu) * (1.0 / 63.0);
    let g = f32((cp >> 6u) & 0x3Fu) * (1.0 / 63.0);
    let b = f32((cp >> 12u) & 0x3Fu) * (1.0 / 63.0);
    let a = select(1.0, 200.0 / 255.0, (cp & 0x40000u) != 0u);
    return vec4<f32>(r, g, b, a);
}

fn sample_atlas_mip(raw_uv: vec2<f32>) -> vec4<f32> {
    // Fast scalar mip LOD via textureSampleLevel (avoids expensive textureSampleGrad path
    // on Apple Silicon while keeping 5-level per-tile mip L1 cache hits and zero tile seams).
    let is_tiled = raw_uv.x >= 1.5;
    let scale = select(512.0, 16.0, is_tiled);
    let ddx = dpdx(raw_uv) * scale;
    let ddy = dpdy(raw_uv) * scale;
    let rho_sq = max(dot(ddx, ddx), dot(ddy, ddy));
    let lod = clamp(0.5 * log2(max(rho_sq, 1.0)), 0.0, 4.0);
    let uv = resolve_atlas_uv(raw_uv);
    return textureSampleLevel(u_atlas_tex, u_atlas_samp, uv, lod);
}

fn resolve_atlas_uv(raw_uv: vec2<f32>) -> vec2<f32> {
    if raw_uv.x >= 1.5 {
        let col = floor(raw_uv.x * 0.03125);
        let row = floor(raw_uv.y * 0.03125);
        let lu = clamp(fract(raw_uv.x), 0.0032, 0.9968);
        let lv = clamp(fract(raw_uv.y), 0.0032, 0.9968);
        return vec2<f32>((col + lu) * 0.03125, (row + lv) * 0.03125);
    }
    return raw_uv;
}

@fragment
fn fs_packed_opaque(in: VsOutPacked) -> @location(0) vec4<f32> {
    let tex = sample_atlas_mip(in.uv);
    let col = unpack_flat_color(in.color_packed);
    return vec4<f32>(tex.rgb * col.rgb, 1.0);
}

@fragment
fn fs_packed_cutout(in: VsOutPacked) -> @location(0) vec4<f32> {
    let tex = sample_atlas_mip(in.uv);
    if tex.a < 0.5 {
        discard;
    }
    let col = unpack_flat_color(in.color_packed);
    return vec4<f32>(tex.rgb * col.rgb, tex.a * col.a);
}

@fragment
fn fs_packed_translucent(in: VsOutPacked) -> @location(0) vec4<f32> {
    let tex = sample_atlas_mip(in.uv);
    if tex.a < 0.01 {
        discard;
    }
    let col = unpack_flat_color(in.color_packed);
    return vec4<f32>(tex.rgb * col.rgb, tex.a * col.a);
}

@fragment
fn fs_opaque(in: VsOut) -> @location(0) vec4<f32> {
    let tex = sample_atlas_mip(in.uv);
    return vec4<f32>(tex.rgb * in.color.rgb, 1.0);
}

@fragment
fn fs_cutout(in: VsOut) -> @location(0) vec4<f32> {
    let tex = sample_atlas_mip(in.uv);
    if tex.a < 0.5 {
        discard;
    }
    return vec4<f32>(tex.rgb * in.color.rgb, tex.a * in.color.a);
}

fn is_chunk_lod0(cx: i32, cz: i32) -> bool {
    let rx = cx - u_cam.lod0_origin.x;
    let rz = cz - u_cam.lod0_origin.y;
    if rx < 0 || rx >= 256 || rz < 0 || rz >= 256 {
        return false;
    }
    let bit = u32((rz << 8) | rx);
    let word_idx = bit >> 5u;
    let vec_idx = word_idx >> 2u;
    let comp_idx = word_idx & 3u;
    let bit_idx = bit & 31u;
    let v = u_cam.lod0_mask[vec_idx];
    var word = v.x;
    if comp_idx == 1u {
        word = v.y;
    } else if comp_idx == 2u {
        word = v.z;
    } else if comp_idx == 3u {
        word = v.w;
    }
    return ((word >> bit_idx) & 1u) != 0u;
}

@fragment
fn fs_lod(in: VsOut) -> @location(0) vec4<f32> {
    if u_cam.lod0_origin.z != 0 {
        let sample_x = in.world_pos.x + in.uv.x;
        let sample_z = in.world_pos.z + in.uv.y;
        let cx = i32(floor(sample_x / 16.0));
        let cz = i32(floor(sample_z / 16.0));
        if is_chunk_lod0(cx, cz) {
            discard;
        }
    }
    return vec4<f32>(in.color.rgb, 1.0);
}

@fragment
fn fs_lod_translucent(in: VsOut) -> @location(0) vec4<f32> {
    if u_cam.lod0_origin.z != 0 {
        let sample_x = in.world_pos.x + in.uv.x;
        let sample_z = in.world_pos.z + in.uv.y;
        let cx = i32(floor(sample_x / 16.0));
        let cz = i32(floor(sample_z / 16.0));
        if is_chunk_lod0(cx, cz) {
            discard;
        }
    }
    return in.color;
}

@fragment
fn fs_translucent(in: VsOut) -> @location(0) vec4<f32> {
    let tex = sample_atlas_mip(in.uv);
    if tex.a < 0.01 {
        discard;
    }
    return vec4<f32>(tex.rgb * in.color.rgb, tex.a * in.color.a);
}
