use crate::atlas::{ATLAS_SIZE, TextureAtlas};
use crate::camera::{CameraController, ViewMode};
use crate::frustum::Frustum;
use crate::mesher::{MeshBucket, PackedCubeQuad, SectionMeshData, TerrainVertex};
use crate::overlay::{OverlayMeshBuilder, OverlayVertex};
use ahash::AHashMap;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use mcedit_core::ChunkPos;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Lod0BatchKey {
    bx4: i32,
    bz4: i32,
}

impl Lod0BatchKey {
    #[inline]
    fn from_chunk(cp: ChunkPos) -> Self {
        Self {
            bx4: cp.cx().div_euclid(4),
            bz4: cp.cz().div_euclid(4),
        }
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    diff_params: [f32; 4],
    lod0_origin: [i32; 4],
    lod0_mask: [[[u32; 4]; 16]; 32],
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
struct OverlayUniform {
    view_proj: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
struct BlurUniform {
    step_uv: [f32; 4],
}

struct BackdropBlurResources {
    capture_tex: wgpu::Texture,
    ping_view: wgpu::TextureView,
    pong_view: wgpu::TextureView,
    h_bind_group: wgpu::BindGroup,
    v_bind_group: wgpu::BindGroup,
    blit_bind_group: wgpu::BindGroup,
    quarter_size: (u32, u32),
}

struct GpuBucket {
    vbuf: wgpu::Buffer,
    index_count: u32,
    quad_count: usize,
    bytes: usize,
}

impl GpuBucket {
    fn from_bucket(device: &wgpu::Device, bucket: &MeshBucket, label: &str) -> Option<Self> {
        if bucket.is_empty() {
            return None;
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(&bucket.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Some(Self {
            vbuf,
            index_count: bucket.index_count(),
            quad_count: bucket.quad_count(),
            bytes: bucket.byte_size(),
        })
    }
}

struct GpuPackedBucket {
    bind_group: wgpu::BindGroup,
    /// Per-face `(start_index, end_index)` ranges in `shared_quad_ibuf` for directional culling:
    /// `[East=0, West=1, Up=2, Down=3, South=4, North=5]`.
    face_ranges: [(u32, u32); 6],
    quad_count: usize,
    bytes: usize,
}

impl GpuPackedBucket {
    fn from_quads(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        origin_xz: [f32; 2],
        quads: &[PackedCubeQuad],
        label: &str,
    ) -> Option<Self> {
        Self::from_slices(device, layout, origin_xz, &[quads], label)
    }

    /// Counting-sorts packed quads directly from up to 16 chunk slices into a single
    /// GPU storage buffer — avoiding intermediate merged `Vec<PackedCubeQuad>` allocations.
    fn from_slices(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        origin_xz: [f32; 2],
        slices: &[&[PackedCubeQuad]],
        label: &str,
    ) -> Option<Self> {
        let mut counts = [0usize; 6];
        let mut total = 0usize;
        for &quads in slices {
            total += quads.len();
            for q in quads {
                let f = (q.face() as usize).min(5);
                counts[f] += 1;
            }
        }
        if total == 0 {
            return None;
        }
        let mut offsets = [0usize; 6];
        let mut face_ranges = [(0u32, 0u32); 6];
        let mut cur = 0usize;
        for f in 0..6 {
            offsets[f] = cur;
            face_ranges[f] = ((cur * 6) as u32, ((cur + counts[f]) * 6) as u32);
            cur += counts[f];
        }

        // Header: [origin_x: f32, origin_z: f32] (8 bytes) followed by face-grouped PackedCubeQuads
        let header = PackedCubeQuad {
            w0: origin_xz[0].to_bits(),
            w1: origin_xz[1].to_bits(),
        };
        let mut raw = vec![header; total + 1];
        let mut write_pos = offsets;
        for &quads in slices {
            for &q in quads {
                let f = (q.face() as usize).min(5);
                raw[1 + write_pos[f]] = q;
                write_pos[f] += 1;
            }
        }

        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        Some(Self {
            bind_group,
            face_ranges,
            quad_count: total,
            bytes: std::mem::size_of_val(raw.as_slice()),
        })
    }
}

/// Computes a 6-bit mask of potentially visible face directions for a batch AABB relative to `cam`.
/// Bit 0=East (+X), 1=West (-X), 2=Up (+Y), 3=Down (-Y), 4=South (+Z), 5=North (-Z).
#[inline]
fn batch_visible_face_mask(cam: Vec3, aabb_min: Vec3, aabb_max: Vec3) -> u8 {
    let mut mask = 0u8;
    if cam.x >= aabb_min.x {
        mask |= 1 << 0; // East (+X) faces can be seen
    }
    if cam.x <= aabb_max.x {
        mask |= 1 << 1; // West (-X) faces can be seen
    }
    if cam.y >= aabb_min.y {
        mask |= 1 << 2; // Up (+Y) faces can be seen
    }
    if cam.y <= aabb_max.y {
        mask |= 1 << 3; // Down (-Y) faces can be seen
    }
    if cam.z >= aabb_min.z {
        mask |= 1 << 4; // South (+Z) faces can be seen
    }
    if cam.z <= aabb_max.z {
        mask |= 1 << 5; // North (-Z) faces can be seen
    }
    mask
}

/// Draws only the visible face directions of a `GpuPackedBucket`, coalescing contiguous ranges.
#[inline]
fn draw_packed_visible_faces<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    b: &'a GpuPackedBucket,
    face_mask: u8,
) {
    let mut bound = false;
    let mut run_start = 0u32;
    let mut run_end = 0u32;
    for f in 0..6 {
        let (s, e) = b.face_ranges[f];
        if (face_mask & (1u8 << f)) != 0 && s < e {
            if run_start == run_end {
                run_start = s;
                run_end = e;
            } else if s == run_end {
                run_end = e;
            } else {
                if !bound {
                    pass.set_bind_group(1, &b.bind_group, &[]);
                    bound = true;
                }
                pass.draw_indexed(run_start..run_end, 0, 0..1);
                run_start = s;
                run_end = e;
            }
        }
    }
    if run_start < run_end {
        if !bound {
            pass.set_bind_group(1, &b.bind_group, &[]);
        }
        pass.draw_indexed(run_start..run_end, 0, 0..1);
    }
}

fn create_shared_quad_ibuf(device: &wgpu::Device, quad_capacity: usize) -> wgpu::Buffer {
    let mut indices = Vec::with_capacity(quad_capacity * 6);
    for q in 0..quad_capacity as u32 {
        let base = q * 4;
        indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("shared_quad_ibuf"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    })
}

fn mesh_y_bounds(data: &SectionMeshData) -> (f32, f32) {
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for bucket in [
        &data.opaque,
        &data.alpha_cutout,
        &data.translucent,
        &data.diff_red,
        &data.diff_gray_a,
        &data.diff_gray_b,
    ] {
        for v in &bucket.vertices {
            if v.pos[1] < min_y {
                min_y = v.pos[1];
            }
            if v.pos[1] > max_y {
                max_y = v.pos[1];
            }
        }
    }
    for quads in [
        &data.packed_opaque,
        &data.packed_cutout,
        &data.packed_translucent,
        &data.packed_diff_red,
        &data.packed_diff_gray_a,
        &data.packed_diff_gray_b,
    ] {
        for q in quads {
            let (qy0, qy1) = q.y_bounds();
            if qy0 < min_y {
                min_y = qy0;
            }
            if qy1 > max_y {
                max_y = qy1;
            }
        }
    }
    if min_y.is_finite() && max_y.is_finite() {
        (min_y, max_y)
    } else {
        (0.0, 256.0)
    }
}

fn mesh_xyz_bounds(
    data: &SectionMeshData,
    origin_x: f32,
    origin_z: f32,
    fallback_min: Vec3,
    fallback_max: Vec3,
) -> (Vec3, Vec3) {
    let mut min_v = Vec3::splat(f32::INFINITY);
    let mut max_v = Vec3::splat(f32::NEG_INFINITY);
    for bucket in [
        &data.opaque,
        &data.alpha_cutout,
        &data.translucent,
        &data.diff_red,
        &data.diff_gray_a,
        &data.diff_gray_b,
    ] {
        for v in &bucket.vertices {
            let p = Vec3::from_array(v.pos);
            min_v = min_v.min(p);
            max_v = max_v.max(p);
        }
    }
    for quads in [
        &data.packed_opaque,
        &data.packed_cutout,
        &data.packed_translucent,
        &data.packed_diff_red,
        &data.packed_diff_gray_a,
        &data.packed_diff_gray_b,
    ] {
        for q in quads {
            let lx = (q.w0 & 0x3F) as f32;
            let lz = ((q.w0 >> 6) & 0x3F) as f32;
            let (qy0, qy1) = q.y_bounds();
            let du = (((q.w0 >> 24) & 0xF) + 1) as f32;
            let dv = (((q.w0 >> 28) & 0xF) + 1) as f32;
            let face = q.face();
            let (sx, sz) = match face {
                0 | 1 => (1.0, du),
                2 | 3 => (du, dv),
                _ => (du, 1.0),
            };
            let p0 = Vec3::new(origin_x + lx, qy0, origin_z + lz);
            let p1 = Vec3::new(origin_x + lx + sx, qy1, origin_z + lz + sz);
            min_v = min_v.min(p0);
            max_v = max_v.max(p1);
        }
    }
    if min_v.is_finite() && max_v.is_finite() {
        (min_v, max_v.max(min_v + Vec3::ONE))
    } else {
        (fallback_min, fallback_max)
    }
}

struct GpuSectionMesh {
    aabb_min: Vec3,
    aabb_max: Vec3,
    opaque: Option<GpuBucket>,
    alpha_cutout: Option<GpuBucket>,
    translucent: Option<GpuBucket>,
    packed_opaque: Option<GpuPackedBucket>,
    packed_cutout: Option<GpuPackedBucket>,
    packed_translucent: Option<GpuPackedBucket>,
    packed_diff_red: Option<GpuPackedBucket>,
    packed_diff_gray_a: Option<GpuPackedBucket>,
    packed_diff_gray_b: Option<GpuPackedBucket>,
    diff_red: Option<GpuBucket>,
    diff_gray_a: Option<GpuBucket>,
    diff_gray_b: Option<GpuBucket>,
    total_bytes: usize,
}

impl GpuSectionMesh {
    fn from_data(
        device: &wgpu::Device,
        packed_layout: &wgpu::BindGroupLayout,
        key: SectionMeshKey,
        data: &SectionMeshData,
    ) -> Self {
        let bkey = Lod0BatchKey::from_chunk(key.chunk);
        let origin_xz = [(bkey.bx4 * 64) as f32, (bkey.bz4 * 64) as f32];
        let (min_y, max_y) = if key.section_y == i32::MIN || key.lod != 0 {
            mesh_y_bounds(data)
        } else {
            ((key.section_y * 16) as f32, ((key.section_y + 1) * 16) as f32)
        };
        let aabb_min = Vec3::new(
            (key.chunk.cx() * 16) as f32,
            min_y,
            (key.chunk.cz() * 16) as f32,
        );
        let aabb_max = Vec3::new(aabb_min.x + 16.0, max_y.max(min_y + 1.0), aabb_min.z + 16.0);
        let opaque = GpuBucket::from_bucket(device, &data.opaque, "section_opaque");
        let alpha_cutout = GpuBucket::from_bucket(device, &data.alpha_cutout, "section_cutout");
        let translucent = GpuBucket::from_bucket(device, &data.translucent, "section_trans");
        let packed_opaque = GpuPackedBucket::from_quads(
            device,
            packed_layout,
            origin_xz,
            &data.packed_opaque,
            "section_packed_opaque",
        );
        let packed_cutout = GpuPackedBucket::from_quads(
            device,
            packed_layout,
            origin_xz,
            &data.packed_cutout,
            "section_packed_cutout",
        );
        let packed_translucent = GpuPackedBucket::from_quads(
            device,
            packed_layout,
            origin_xz,
            &data.packed_translucent,
            "section_packed_trans",
        );
        let packed_diff_red = GpuPackedBucket::from_quads(
            device,
            packed_layout,
            origin_xz,
            &data.packed_diff_red,
            "section_packed_diff_red",
        );
        let packed_diff_gray_a = GpuPackedBucket::from_quads(
            device,
            packed_layout,
            origin_xz,
            &data.packed_diff_gray_a,
            "section_packed_diff_gray_a",
        );
        let packed_diff_gray_b = GpuPackedBucket::from_quads(
            device,
            packed_layout,
            origin_xz,
            &data.packed_diff_gray_b,
            "section_packed_diff_gray_b",
        );
        let diff_red = GpuBucket::from_bucket(device, &data.diff_red, "section_diff_red");
        let diff_gray_a = GpuBucket::from_bucket(device, &data.diff_gray_a, "section_diff_gray_a");
        let diff_gray_b = GpuBucket::from_bucket(device, &data.diff_gray_b, "section_diff_gray_b");
        let total_bytes = opaque.as_ref().map_or(0, |b| b.bytes)
            + alpha_cutout.as_ref().map_or(0, |b| b.bytes)
            + translucent.as_ref().map_or(0, |b| b.bytes)
            + packed_opaque.as_ref().map_or(0, |b| b.bytes)
            + packed_cutout.as_ref().map_or(0, |b| b.bytes)
            + packed_translucent.as_ref().map_or(0, |b| b.bytes)
            + packed_diff_red.as_ref().map_or(0, |b| b.bytes)
            + packed_diff_gray_a.as_ref().map_or(0, |b| b.bytes)
            + packed_diff_gray_b.as_ref().map_or(0, |b| b.bytes)
            + diff_red.as_ref().map_or(0, |b| b.bytes)
            + diff_gray_a.as_ref().map_or(0, |b| b.bytes)
            + diff_gray_b.as_ref().map_or(0, |b| b.bytes);

        Self {
            aabb_min,
            aabb_max,
            opaque,
            alpha_cutout,
            translucent,
            packed_opaque,
            packed_cutout,
            packed_translucent,
            packed_diff_red,
            packed_diff_gray_a,
            packed_diff_gray_b,
            diff_red,
            diff_gray_a,
            diff_gray_b,
            total_bytes,
        }
    }

    fn from_batch_sections(
        device: &wgpu::Device,
        packed_layout: &wgpu::BindGroupLayout,
        bkey: Lod0BatchKey,
        sections: &[&SectionMeshData],
    ) -> Self {
        let origin_xz = [(bkey.bx4 * 64) as f32, (bkey.bz4 * 64) as f32];
        let fb_min = Vec3::new(origin_xz[0], 0.0, origin_xz[1]);
        let fb_max = fb_min + Vec3::new(64.0, 256.0, 64.0);

        let mut min_v = Vec3::splat(f32::INFINITY);
        let mut max_v = Vec3::splat(f32::NEG_INFINITY);
        let mut ov = 0usize;
        let mut cv = 0usize;
        let mut tv = 0usize;
        let mut drv = 0usize;
        let mut dgav = 0usize;
        let mut dgbv = 0usize;
        for &sec in sections {
            ov += sec.opaque.vertices.len();
            cv += sec.alpha_cutout.vertices.len();
            tv += sec.translucent.vertices.len();
            drv += sec.diff_red.vertices.len();
            dgav += sec.diff_gray_a.vertices.len();
            dgbv += sec.diff_gray_b.vertices.len();
            let (smin, smax) = mesh_xyz_bounds(sec, origin_xz[0], origin_xz[1], fb_min, fb_max);
            if !sec.is_empty() {
                min_v = min_v.min(smin);
                max_v = max_v.max(smax);
            }
        }
        let (aabb_min, aabb_max) = if min_v.is_finite() && max_v.is_finite() {
            (min_v, max_v.max(min_v + Vec3::ONE))
        } else {
            (fb_min, fb_max)
        };

        let mut merged_o = MeshBucket::default();
        let mut merged_c = MeshBucket::default();
        let mut merged_t = MeshBucket::default();
        let mut merged_dr = MeshBucket::default();
        let mut merged_dga = MeshBucket::default();
        let mut merged_dgb = MeshBucket::default();
        merged_o.vertices.reserve(ov);
        merged_c.vertices.reserve(cv);
        merged_t.vertices.reserve(tv);
        merged_dr.vertices.reserve(drv);
        merged_dga.vertices.reserve(dgav);
        merged_dgb.vertices.reserve(dgbv);
        let mut po_slices: Vec<&[PackedCubeQuad]> = Vec::with_capacity(sections.len());
        let mut pc_slices: Vec<&[PackedCubeQuad]> = Vec::with_capacity(sections.len());
        let mut pt_slices: Vec<&[PackedCubeQuad]> = Vec::with_capacity(sections.len());
        let mut pdr_slices: Vec<&[PackedCubeQuad]> = Vec::new();
        let mut pdga_slices: Vec<&[PackedCubeQuad]> = Vec::new();
        let mut pdgb_slices: Vec<&[PackedCubeQuad]> = Vec::new();
        for &sec in sections {
            merged_o.append(&sec.opaque);
            merged_c.append(&sec.alpha_cutout);
            merged_t.append(&sec.translucent);
            merged_dr.append(&sec.diff_red);
            merged_dga.append(&sec.diff_gray_a);
            merged_dgb.append(&sec.diff_gray_b);
            if !sec.packed_opaque.is_empty() {
                po_slices.push(&sec.packed_opaque);
            }
            if !sec.packed_cutout.is_empty() {
                pc_slices.push(&sec.packed_cutout);
            }
            if !sec.packed_translucent.is_empty() {
                pt_slices.push(&sec.packed_translucent);
            }
            if !sec.packed_diff_red.is_empty() {
                pdr_slices.push(&sec.packed_diff_red);
            }
            if !sec.packed_diff_gray_a.is_empty() {
                pdga_slices.push(&sec.packed_diff_gray_a);
            }
            if !sec.packed_diff_gray_b.is_empty() {
                pdgb_slices.push(&sec.packed_diff_gray_b);
            }
        }

        let opaque = GpuBucket::from_bucket(device, &merged_o, "batch4x4_opaque");
        let alpha_cutout = GpuBucket::from_bucket(device, &merged_c, "batch4x4_cutout");
        let translucent = GpuBucket::from_bucket(device, &merged_t, "batch4x4_trans");
        let diff_red = GpuBucket::from_bucket(device, &merged_dr, "batch4x4_diff_red");
        let diff_gray_a = GpuBucket::from_bucket(device, &merged_dga, "batch4x4_diff_gray_a");
        let diff_gray_b = GpuBucket::from_bucket(device, &merged_dgb, "batch4x4_diff_gray_b");
        let packed_opaque = GpuPackedBucket::from_slices(
            device,
            packed_layout,
            origin_xz,
            &po_slices,
            "batch4x4_packed_opaque",
        );
        let packed_cutout = GpuPackedBucket::from_slices(
            device,
            packed_layout,
            origin_xz,
            &pc_slices,
            "batch4x4_packed_cutout",
        );
        let packed_translucent = GpuPackedBucket::from_slices(
            device,
            packed_layout,
            origin_xz,
            &pt_slices,
            "batch4x4_packed_trans",
        );
        let packed_diff_red = GpuPackedBucket::from_slices(
            device,
            packed_layout,
            origin_xz,
            &pdr_slices,
            "batch4x4_packed_diff_red",
        );
        let packed_diff_gray_a = GpuPackedBucket::from_slices(
            device,
            packed_layout,
            origin_xz,
            &pdga_slices,
            "batch4x4_packed_diff_gray_a",
        );
        let packed_diff_gray_b = GpuPackedBucket::from_slices(
            device,
            packed_layout,
            origin_xz,
            &pdgb_slices,
            "batch4x4_packed_diff_gray_b",
        );
        let total_bytes = opaque.as_ref().map_or(0, |b| b.bytes)
            + alpha_cutout.as_ref().map_or(0, |b| b.bytes)
            + translucent.as_ref().map_or(0, |b| b.bytes)
            + packed_opaque.as_ref().map_or(0, |b| b.bytes)
            + packed_cutout.as_ref().map_or(0, |b| b.bytes)
            + packed_translucent.as_ref().map_or(0, |b| b.bytes)
            + packed_diff_red.as_ref().map_or(0, |b| b.bytes)
            + packed_diff_gray_a.as_ref().map_or(0, |b| b.bytes)
            + packed_diff_gray_b.as_ref().map_or(0, |b| b.bytes)
            + diff_red.as_ref().map_or(0, |b| b.bytes)
            + diff_gray_a.as_ref().map_or(0, |b| b.bytes)
            + diff_gray_b.as_ref().map_or(0, |b| b.bytes);

        Self {
            aabb_min,
            aabb_max,
            opaque,
            alpha_cutout,
            translucent,
            packed_opaque,
            packed_cutout,
            packed_translucent,
            packed_diff_red,
            packed_diff_gray_a,
            packed_diff_gray_b,
            diff_red,
            diff_gray_a,
            diff_gray_b,
            total_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionMeshKey {
    pub chunk: ChunkPos,
    /// Specific section Y, or `i32::MIN` when the whole chunk is merged into one mesh.
    pub section_y: i32,
    pub lod: u8,
}

/// Key for an 8x8-chunk distant LOD super-region tile.
/// - `lod == 1`: true 3D 2x2x2 greedy-meshed voxel LOD (mid-range)
/// - `lod == 2`: true 3D 4x4x4 greedy-meshed voxel LOD (far horizon up to 128 chunks)
/// - `lod == 3`: elevation-shaded flat top-down tile for 2D Chunk View
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LodRegionKey {
    pub rx8: i32,
    pub rz8: i32,
    pub lod: u8,
}

struct GpuLodRegionMesh {
    aabb_min: Vec3,
    aabb_max: Vec3,
    opaque: Option<GpuBucket>,
    translucent: Option<GpuBucket>,
    total_bytes: usize,
    /// 64-bit mask of which chunks inside this 8x8 super-region contain geometry.
    /// Allows skipping the entire LOD draw call on the CPU when all present chunks are in LOD 0.
    present_mask: u64,
}

impl GpuLodRegionMesh {
    fn from_data(
        device: &wgpu::Device,
        key: LodRegionKey,
        data: &SectionMeshData,
    ) -> Option<Self> {
        let opaque = GpuBucket::from_bucket(device, &data.opaque, "lod_region_opaque");
        let translucent = GpuBucket::from_bucket(device, &data.translucent, "lod_region_trans");
        if opaque.is_none() && translucent.is_none() {
            return None;
        }
        let mut present_mask = 0u64;
        let base_cx = key.rx8 * 8;
        let base_cz = key.rz8 * 8;
        for bucket in [&data.opaque, &data.translucent] {
            for v in &bucket.vertices {
                let cx = ((v.pos[0] + v.uv[0]) / 16.0).floor() as i32 - base_cx;
                let cz = ((v.pos[2] + v.uv[1]) / 16.0).floor() as i32 - base_cz;
                if (0..8).contains(&cx) && (0..8).contains(&cz) {
                    present_mask |= 1u64 << ((cz * 8 + cx) as u32);
                }
            }
        }
        let (min_y, max_y) = mesh_y_bounds(data);
        let aabb_min = Vec3::new((key.rx8 * 128) as f32, min_y, (key.rz8 * 128) as f32);
        let aabb_max = Vec3::new(
            aabb_min.x + 128.0,
            max_y.max(min_y + 1.0),
            aabb_min.z + 128.0,
        );
        let total_bytes = opaque.as_ref().map_or(0, |b| b.bytes)
            + translucent.as_ref().map_or(0, |b| b.bytes);
        Some(Self {
            aabb_min,
            aabb_max,
            opaque,
            translucent,
            total_bytes,
            present_mask,
        })
    }
}

pub struct WorldRenderer {
    opaque_pipeline: wgpu::RenderPipeline,
    cutout_pipeline: wgpu::RenderPipeline,
    packed_opaque_pipeline: wgpu::RenderPipeline,
    packed_cutout_pipeline: wgpu::RenderPipeline,
    packed_translucent_pipeline: wgpu::RenderPipeline,
    // Compare diff overlay pipelines
    packed_depth_only_pipeline: wgpu::RenderPipeline,
    depth_only_pipeline: wgpu::RenderPipeline,
    packed_diff_red_pipeline: wgpu::RenderPipeline,
    diff_red_pipeline: wgpu::RenderPipeline,
    packed_diff_gray_a_pipeline: wgpu::RenderPipeline,
    diff_gray_a_pipeline: wgpu::RenderPipeline,
    packed_diff_gray_b_pipeline: wgpu::RenderPipeline,
    diff_gray_b_pipeline: wgpu::RenderPipeline,
    lod_pipeline: wgpu::RenderPipeline,
    lod_translucent_pipeline: wgpu::RenderPipeline,
    translucent_pipeline: wgpu::RenderPipeline,
    overlay_tri_pipeline: wgpu::RenderPipeline,
    overlay_line_pipeline: wgpu::RenderPipeline,

    terrain_bind_group: wgpu::BindGroup,
    terrain_bind_layout: wgpu::BindGroupLayout,
    cam_uniform_buf: wgpu::Buffer,

    packed_quad_bind_layout: wgpu::BindGroupLayout,

    shared_quad_ibuf: wgpu::Buffer,
    shared_quad_ibuf_quads: usize,

    overlay_bind_group: wgpu::BindGroup,
    overlay_uniform_buf: wgpu::Buffer,

    atlas_texture: wgpu::Texture,
    atlas_sampler: wgpu::Sampler,

    depth_view: wgpu::TextureView,
    depth_size: (u32, u32),
    surface_format: wgpu::TextureFormat,

    blur_pipeline: wgpu::RenderPipeline,
    blur_blit_pipeline: wgpu::RenderPipeline,
    blur_bind_layout: wgpu::BindGroupLayout,
    blur_sampler: wgpu::Sampler,
    blur_res: BackdropBlurResources,

    cpu_sections: AHashMap<SectionMeshKey, SectionMeshData>,
    pending_lod0: AHashMap<ChunkPos, GpuSectionMesh>,
    lod0_batches: AHashMap<Lod0BatchKey, GpuSectionMesh>,
    dirty_batches: AHashMap<Lod0BatchKey, Instant>,
    lod0_region_masks: AHashMap<(i32, i32), u64>,
    cached_lod0_origin: [i32; 4],
    cached_lod0_mask: Box<[[[u32; 4]; 16]; 32]>,
    lod0_mask_dirty: bool,
    lod_regions: AHashMap<LodRegionKey, GpuLodRegionMesh>,
    last_cam_chunk: ChunkPos,
    evicted_lod0: Vec<ChunkPos>,
    evicted_lod_regions: Vec<LodRegionKey>,
    vram_bytes: usize,
    pub vram_limit_bytes: usize,

    grid_tri_buf: Option<(wgpu::Buffer, u32)>,
    grid_line_buf: Option<(wgpu::Buffer, u32)>,

    preview_mesh: Option<GpuSectionMesh>,
    pub preview_origin: Vec3,
    pub diff_params: [f32; 4],
}

impl WorldRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        atlas: &TextureAtlas,
        vram_limit_mb: usize,
    ) -> Self {
        let terrain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/terrain.wgsl").into()),
        });
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/overlay.wgsl").into()),
        });

        let cam_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera_uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (atlas_texture, atlas_view) = create_atlas_texture(device, queue, atlas);
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas_sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let terrain_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("terrain_bind_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let terrain_bind_group = Self::make_terrain_bind_group(
            device,
            &terrain_bind_layout,
            &cam_uniform_buf,
            &atlas_view,
            &atlas_sampler,
        );

        let terrain_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("terrain_pipeline_layout"),
                bind_group_layouts: &[Some(&terrain_bind_layout)],
                immediate_size: 0,
            });

        let packed_quad_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("packed_quad_bind_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let packed_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("packed_pipeline_layout"),
                bind_group_layouts: &[
                    Some(&terrain_bind_layout),
                    Some(&packed_quad_bind_layout),
                ],
                immediate_size: 0,
            });

        let terrain_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TerrainVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: 12,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 20,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Unorm8x4,
                },
            ],
        };

        let opaque_pipeline = make_terrain_pipeline(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_opaque",
            surface_format,
            Some(&terrain_vertex_layout),
            true,
            Some(wgpu::Face::Back),
            None,
            wgpu::DepthBiasState::default(),
        );
        let cutout_pipeline = make_terrain_pipeline(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_cutout",
            surface_format,
            Some(&terrain_vertex_layout),
            true,
            None,
            None,
            wgpu::DepthBiasState::default(),
        );
        let packed_opaque_pipeline = make_terrain_pipeline(
            device,
            &packed_pipeline_layout,
            &terrain_shader,
            "vs_packed_quad",
            "fs_packed_opaque",
            surface_format,
            None,
            true,
            Some(wgpu::Face::Back),
            None,
            wgpu::DepthBiasState::default(),
        );
        let packed_cutout_pipeline = make_terrain_pipeline(
            device,
            &packed_pipeline_layout,
            &terrain_shader,
            "vs_packed_quad",
            "fs_packed_cutout",
            surface_format,
            None,
            true,
            Some(wgpu::Face::Back),
            None,
            wgpu::DepthBiasState::default(),
        );
        let packed_translucent_pipeline = make_terrain_pipeline(
            device,
            &packed_pipeline_layout,
            &terrain_shader,
            "vs_packed_quad",
            "fs_packed_translucent",
            surface_format,
            None,
            false,
            Some(wgpu::Face::Back),
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::DepthBiasState::default(),
        );
        // Compare diff overlay pipelines
        let packed_depth_only_pipeline = make_terrain_pipeline_ext(
            device,
            &packed_pipeline_layout,
            &terrain_shader,
            "vs_packed_quad",
            "fs_packed_depth_only",
            surface_format,
            None,
            true,
            wgpu::CompareFunction::Less,
            Some(wgpu::Face::Back),
            None,
            wgpu::ColorWrites::empty(),
            wgpu::DepthBiasState::default(),
        );
        let depth_only_pipeline = make_terrain_pipeline_ext(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_depth_only",
            surface_format,
            Some(&terrain_vertex_layout),
            true,
            wgpu::CompareFunction::Less,
            None,
            None,
            wgpu::ColorWrites::empty(),
            wgpu::DepthBiasState::default(),
        );
        let packed_diff_red_pipeline = make_terrain_pipeline(
            device,
            &packed_pipeline_layout,
            &terrain_shader,
            "vs_packed_quad",
            "fs_packed_diff_red",
            surface_format,
            None,
            true,
            Some(wgpu::Face::Back),
            None,
            wgpu::DepthBiasState::default(),
        );
        let diff_red_pipeline = make_terrain_pipeline(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_diff_red",
            surface_format,
            Some(&terrain_vertex_layout),
            true,
            None,
            None,
            wgpu::DepthBiasState::default(),
        );
        let packed_diff_gray_a_pipeline = make_terrain_pipeline_ext(
            device,
            &packed_pipeline_layout,
            &terrain_shader,
            "vs_packed_quad",
            "fs_packed_diff_gray_a",
            surface_format,
            None,
            false,
            wgpu::CompareFunction::Equal,
            Some(wgpu::Face::Back),
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::ColorWrites::ALL,
            wgpu::DepthBiasState::default(),
        );
        let diff_gray_a_pipeline = make_terrain_pipeline_ext(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_diff_gray_a",
            surface_format,
            Some(&terrain_vertex_layout),
            false,
            wgpu::CompareFunction::Equal,
            None,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::ColorWrites::ALL,
            wgpu::DepthBiasState::default(),
        );
        let packed_diff_gray_b_pipeline = make_terrain_pipeline_ext(
            device,
            &packed_pipeline_layout,
            &terrain_shader,
            "vs_packed_quad",
            "fs_packed_diff_gray_b",
            surface_format,
            None,
            false,
            wgpu::CompareFunction::Equal,
            Some(wgpu::Face::Back),
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::ColorWrites::ALL,
            wgpu::DepthBiasState::default(),
        );
        let diff_gray_b_pipeline = make_terrain_pipeline_ext(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_diff_gray_b",
            surface_format,
            Some(&terrain_vertex_layout),
            false,
            wgpu::CompareFunction::Equal,
            None,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::ColorWrites::ALL,
            wgpu::DepthBiasState::default(),
        );
        let lod_pipeline = make_terrain_pipeline(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_lod",
            surface_format,
            Some(&terrain_vertex_layout),
            true,
            Some(wgpu::Face::Back),
            None,
            wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 1.0,
                clamp: 0.0,
            },
        );
        let lod_translucent_pipeline = make_terrain_pipeline(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_lod_translucent",
            surface_format,
            Some(&terrain_vertex_layout),
            false,
            None,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::DepthBiasState {
                constant: 1,
                slope_scale: 0.5,
                clamp: 0.0,
            },
        );
        let translucent_pipeline = make_terrain_pipeline(
            device,
            &terrain_pipeline_layout,
            &terrain_shader,
            "vs_main",
            "fs_translucent",
            surface_format,
            Some(&terrain_vertex_layout),
            false,
            None,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::DepthBiasState::default(),
        );

        let shared_quad_ibuf_quads = 65536usize;
        let shared_quad_ibuf = create_shared_quad_ibuf(device, shared_quad_ibuf_quads);

        let overlay_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay_uniform"),
            size: std::mem::size_of::<OverlayUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let overlay_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("overlay_bind_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let overlay_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay_bind_group"),
            layout: &overlay_bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: overlay_uniform_buf.as_entire_binding(),
            }],
        });
        let overlay_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay_pipeline_layout"),
                bind_group_layouts: &[Some(&overlay_bind_layout)],
                immediate_size: 0,
            });
        let overlay_v_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<OverlayVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: 12,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };
        let overlay_tri_pipeline = make_overlay_pipeline(
            device,
            &overlay_pipeline_layout,
            &overlay_shader,
            surface_format,
            &overlay_v_layout,
            wgpu::PrimitiveTopology::TriangleList,
        );
        let overlay_line_pipeline = make_overlay_pipeline(
            device,
            &overlay_pipeline_layout,
            &overlay_shader,
            surface_format,
            &overlay_v_layout,
            wgpu::PrimitiveTopology::LineList,
        );

        let depth_view = create_depth_view(device, width.max(1), height.max(1));

        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blur.wgsl").into()),
        });
        let blur_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blur_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let blur_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("blur_bind_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let blur_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("blur_pipeline_layout"),
                bind_group_layouts: &[Some(&blur_bind_layout)],
                immediate_size: 0,
            });
        let blur_pipeline =
            make_blur_pipeline(device, &blur_pipeline_layout, &blur_shader, "fs_blur", surface_format);
        let blur_blit_pipeline =
            make_blur_pipeline(device, &blur_pipeline_layout, &blur_shader, "fs_blit", surface_format);
        let blur_res = create_backdrop_blur_resources(
            device,
            &blur_bind_layout,
            &blur_sampler,
            surface_format,
            width.max(1),
            height.max(1),
        );

        Self {
            opaque_pipeline,
            cutout_pipeline,
            packed_opaque_pipeline,
            packed_cutout_pipeline,
            packed_translucent_pipeline,
            packed_depth_only_pipeline,
            depth_only_pipeline,
            packed_diff_red_pipeline,
            diff_red_pipeline,
            packed_diff_gray_a_pipeline,
            diff_gray_a_pipeline,
            packed_diff_gray_b_pipeline,
            diff_gray_b_pipeline,
            lod_pipeline,
            lod_translucent_pipeline,
            translucent_pipeline,
            overlay_tri_pipeline,
            overlay_line_pipeline,
            terrain_bind_group,
            terrain_bind_layout,
            cam_uniform_buf,
            packed_quad_bind_layout,
            shared_quad_ibuf,
            shared_quad_ibuf_quads,
            overlay_bind_group,
            overlay_uniform_buf,
            atlas_texture,
            atlas_sampler,
            depth_view,
            depth_size: (width.max(1), height.max(1)),
            surface_format,
            blur_pipeline,
            blur_blit_pipeline,
            blur_bind_layout,
            blur_sampler,
            blur_res,
            cpu_sections: AHashMap::new(),
            pending_lod0: AHashMap::new(),
            lod0_batches: AHashMap::new(),
            dirty_batches: AHashMap::new(),
            lod0_region_masks: AHashMap::new(),
            cached_lod0_origin: [0; 4],
            cached_lod0_mask: Box::new([[[0; 4]; 16]; 32]),
            lod0_mask_dirty: true,
            lod_regions: AHashMap::new(),
            last_cam_chunk: ChunkPos::new(0, 0),
            evicted_lod0: Vec::new(),
            evicted_lod_regions: Vec::new(),
            vram_bytes: 0,
            vram_limit_bytes: vram_limit_mb.max(64) * 1024 * 1024,
            grid_tri_buf: None,
            grid_line_buf: None,
            preview_mesh: None,
            preview_origin: Vec3::ZERO,
            diff_params: [0.55, 0.85, 0.62, 0.0],
        }
    }

    fn make_terrain_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        cam_buf: &wgpu::Buffer,
        atlas_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cam_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    pub fn update_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, atlas: &TextureAtlas) {
        let (tex, view) = create_atlas_texture(device, queue, atlas);
        self.atlas_texture = tex;
        self.terrain_bind_group = Self::make_terrain_bind_group(
            device,
            &self.terrain_bind_layout,
            &self.cam_uniform_buf,
            &view,
            &self.atlas_sampler,
        );
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let w = width.max(1);
        let h = height.max(1);
        if self.depth_size != (w, h) {
            self.depth_view = create_depth_view(device, w, h);
            self.blur_res = create_backdrop_blur_resources(
                device,
                &self.blur_bind_layout,
                &self.blur_sampler,
                self.surface_format,
                w,
                h,
            );
            self.depth_size = (w, h);
        }
    }

    /// Applies a 2-pass quarter-resolution separable Gaussian backdrop blur to the
    /// physical pixel rectangle `rect_px = [x, y, width, height]` on `target_view`.
    pub fn apply_panel_backdrop_blur(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        surface_tex: &wgpu::Texture,
        target_view: &wgpu::TextureView,
        rect_px: [u32; 4],
    ) {
        let (full_w, full_h) = self.depth_size;
        let x0 = rect_px[0].min(full_w);
        let y0 = rect_px[1].min(full_h);
        let rw = rect_px[2].min(full_w.saturating_sub(x0));
        let rh = rect_px[3].min(full_h.saturating_sub(y0));
        if rw == 0 || rh == 0 {
            return;
        }

        // Copy only the left panel rectangle (+16px margin for filter taps) from the rendered 3D surface
        let copy_w = (rw + 16).min(full_w.saturating_sub(x0));
        let copy_h = (rh + 16).min(full_h.saturating_sub(y0));
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: surface_tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.blur_res.capture_tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: copy_w,
                height: copy_h,
                depth_or_array_layers: 1,
            },
        );

        let (qw_total, qh_total) = self.blur_res.quarter_size;
        let qx0 = (x0 / 4).min(qw_total.saturating_sub(1));
        let qy0 = (y0 / 4).min(qh_total.saturating_sub(1));
        let qw = ((rw + 19) / 4).clamp(1, qw_total.saturating_sub(qx0));
        let qh = ((rh + 19) / 4).clamp(1, qh_total.saturating_sub(qy0));

        // Pass 1: Horizontal 9-tap Gaussian downsample + blur (capture_tex -> ping_view)
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop_blur_h"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.blur_res.ping_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blur_pipeline);
            pass.set_bind_group(0, &self.blur_res.h_bind_group, &[]);
            pass.set_scissor_rect(qx0, qy0, qw, qh);
            pass.draw(0..3, 0..1);
        }

        // Pass 2: Vertical 9-tap Gaussian blur (ping_view -> pong_view)
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop_blur_v"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.blur_res.pong_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blur_pipeline);
            pass.set_bind_group(0, &self.blur_res.v_bind_group, &[]);
            pass.set_scissor_rect(qx0, qy0, qw, qh);
            pass.draw(0..3, 0..1);
        }

        // Pass 3: Composite blurred backdrop strictly inside the left panel rectangle on target_view
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop_blur_composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blur_blit_pipeline);
            pass.set_bind_group(0, &self.blur_res.blit_bind_group, &[]);
            pass.set_scissor_rect(x0, y0, rw, rh);
            pass.draw(0..3, 0..1);
        }
    }

    pub fn upload_section(
        &mut self,
        device: &wgpu::Device,
        key: SectionMeshKey,
        data: SectionMeshData,
    ) {
        self.remove_section(&key);
        if data.is_empty() {
            return;
        }
        // Upload a lightweight single-chunk GPU buffer immediately (~40 us) so the chunk appears
        // on screen right away, and coalesce the 4x4 super-batch merge so we only merge ONCE
        // when the 4x4 batch fills up or settles (eliminating 37-112 ms main-thread freezes!).
        let gpu_chunk = GpuSectionMesh::from_data(device, &self.packed_quad_bind_layout, key, &data);
        self.vram_bytes += gpu_chunk.total_bytes;
        if let Some(old) = self.pending_lod0.insert(key.chunk, gpu_chunk) {
            self.vram_bytes = self.vram_bytes.saturating_sub(old.total_bytes);
        }
        self.cpu_sections.insert(key, data);
        let rx8 = key.chunk.cx().div_euclid(8);
        let rz8 = key.chunk.cz().div_euclid(8);
        let lx = key.chunk.cx().rem_euclid(8) as u32;
        let lz = key.chunk.cz().rem_euclid(8) as u32;
        *self.lod0_region_masks.entry((rx8, rz8)).or_insert(0) |= 1u64 << (lz * 8 + lx);
        self.dirty_batches
            .insert(Lod0BatchKey::from_chunk(key.chunk), Instant::now());
        self.lod0_mask_dirty = true;
        self.enforce_vram_budget();
    }

    pub fn flush_dirty_batches(&mut self, device: &wgpu::Device) {
        if self.dirty_batches.is_empty() {
            return;
        }
        let now = Instant::now();
        let coalesce_delay = Duration::from_millis(180);

        // Select batches that are ready: either all 16 chunks in the 4x4 are loaded,
        // or the batch has settled for >= 180 ms since its last chunk arrival (or was invalidated).
        let ready: Vec<Lod0BatchKey> = self
            .dirty_batches
            .iter()
            .filter_map(|(&bkey, &touched_at)| {
                if now.duration_since(touched_at) >= coalesce_delay {
                    return Some(bkey);
                }
                let base_cx = bkey.bx4 * 4;
                let base_cz = bkey.bz4 * 4;
                let mut count = 0usize;
                for dz in 0..4 {
                    for dx in 0..4 {
                        let k = SectionMeshKey {
                            chunk: ChunkPos::new(base_cx + dx, base_cz + dz),
                            section_y: i32::MIN,
                            lod: 0,
                        };
                        if self.cpu_sections.contains_key(&k) {
                            count += 1;
                        }
                    }
                }
                if count == 16 { Some(bkey) } else { None }
            })
            .collect();

        if ready.is_empty() {
            return;
        }

        let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::BatchMerge);
        let budget = Duration::from_micros(2500);
        let mut rebuilt = 0u64;

        for bkey in ready {
            if rebuilt > 0 && now.elapsed() >= budget {
                break;
            }
            self.dirty_batches.remove(&bkey);
            rebuilt += 1;

            if let Some(old) = self.lod0_batches.remove(&bkey) {
                self.vram_bytes = self.vram_bytes.saturating_sub(old.total_bytes);
            }
            let base_cx = bkey.bx4 * 4;
            let base_cz = bkey.bz4 * 4;
            let mut batch_sections: Vec<&SectionMeshData> = Vec::with_capacity(16);
            for dz in 0..4 {
                for dx in 0..4 {
                    let cp = ChunkPos::new(base_cx + dx, base_cz + dz);
                    if let Some(old_p) = self.pending_lod0.remove(&cp) {
                        self.vram_bytes = self.vram_bytes.saturating_sub(old_p.total_bytes);
                    }
                    let k = SectionMeshKey {
                        chunk: cp,
                        section_y: i32::MIN,
                        lod: 0,
                    };
                    if let Some(sec_data) = self.cpu_sections.get(&k)
                        && !sec_data.is_empty()
                    {
                        batch_sections.push(sec_data);
                    }
                }
            }
            if !batch_sections.is_empty() {
                let gpu_batch = GpuSectionMesh::from_batch_sections(
                    device,
                    &self.packed_quad_bind_layout,
                    bkey,
                    &batch_sections,
                );
                self.vram_bytes += gpu_batch.total_bytes;
                self.lod0_batches.insert(bkey, gpu_batch);
            }
        }
        mcedit_core::profiler::add(mcedit_core::profiler::Counter::BatchesRebuilt, rebuilt);
        self.enforce_vram_budget();
    }

    pub fn upload_lod_region(
        &mut self,
        device: &wgpu::Device,
        key: LodRegionKey,
        data: &SectionMeshData,
    ) {
        self.remove_lod_region(&key);
        let Some(mesh) = GpuLodRegionMesh::from_data(device, key, data) else {
            return;
        };
        self.vram_bytes += mesh.total_bytes;
        self.lod_regions.insert(key, mesh);
        self.enforce_vram_budget();
    }

    pub fn set_camera_chunk(&mut self, cam_cp: ChunkPos) {
        self.last_cam_chunk = cam_cp;
        if self.vram_bytes > self.vram_limit_bytes {
            self.enforce_vram_budget();
        }
    }

    pub fn take_evictions(&mut self) -> (Vec<ChunkPos>, Vec<LodRegionKey>) {
        (
            std::mem::take(&mut self.evicted_lod0),
            std::mem::take(&mut self.evicted_lod_regions),
        )
    }

    pub fn is_vram_near_limit(&self) -> bool {
        self.vram_bytes >= (self.vram_limit_bytes * 95) / 100
    }

    pub fn has_lod_regions(&self) -> bool {
        !self.lod_regions.is_empty()
    }

    pub fn furthest_lod_dist_sq(&self, is_2d: bool) -> Option<i64> {
        let cam_cx = self.last_cam_chunk.cx() as i64;
        let cam_cz = self.last_cam_chunk.cz() as i64;
        self.lod_regions
            .keys()
            .filter(|k| (k.lod == 3) == is_2d)
            .map(|k| {
                let dx = k.rx8 as i64 * 8 + 4 - cam_cx;
                let dz = k.rz8 as i64 * 8 + 4 - cam_cz;
                dx * dx + dz * dz
            })
            .max()
    }

    pub fn furthest_lod0_dist_sq(&self) -> Option<i64> {
        let cam_cx = self.last_cam_chunk.cx() as i64;
        let cam_cz = self.last_cam_chunk.cz() as i64;
        self.cpu_sections
            .keys()
            .map(|k| {
                let dx = k.chunk.cx() as i64 - cam_cx;
                let dz = k.chunk.cz() as i64 - cam_cz;
                dx * dx + dz * dz
            })
            .max()
    }

    fn enforce_vram_budget(&mut self) {
        let cam_cx = self.last_cam_chunk.cx() as i64;
        let cam_cz = self.last_cam_chunk.cz() as i64;
        while self.vram_bytes > self.vram_limit_bytes {
            // 1. Always evict the furthest LOD super-region from the camera first.
            let furthest_lod = self
                .lod_regions
                .keys()
                .copied()
                .max_by_key(|k| {
                    let dx = k.rx8 as i64 * 8 + 4 - cam_cx;
                    let dz = k.rz8 as i64 * 8 + 4 - cam_cz;
                    dx * dx + dz * dz
                });
            if let Some(evict) = furthest_lod {
                if let Some(m) = self.lod_regions.remove(&evict) {
                    self.vram_bytes = self.vram_bytes.saturating_sub(m.total_bytes);
                    self.evicted_lod_regions.push(evict);
                }
                continue;
            }

            // 2. Only if all LOD regions are already evicted, evict the furthest LOD 0 chunk.
            let furthest_lod0 = self
                .cpu_sections
                .keys()
                .copied()
                .max_by_key(|k| {
                    let dx = k.chunk.cx() as i64 - cam_cx;
                    let dz = k.chunk.cz() as i64 - cam_cz;
                    dx * dx + dz * dz
                });
            if let Some(evict) = furthest_lod0 {
                self.remove_section(&evict);
                let bkey = Lod0BatchKey::from_chunk(evict.chunk);
                if let Some(m) = self.lod0_batches.remove(&bkey) {
                    self.vram_bytes = self.vram_bytes.saturating_sub(m.total_bytes);
                }
                self.dirty_batches
                    .insert(bkey, Instant::now() - Duration::from_secs(1));
                self.evicted_lod0.push(evict.chunk);
                continue;
            }

            break;
        }
    }

    pub fn remove_section(&mut self, key: &SectionMeshKey) {
        if let Some(old_p) = self.pending_lod0.remove(&key.chunk) {
            self.vram_bytes = self.vram_bytes.saturating_sub(old_p.total_bytes);
        }
        if self.cpu_sections.remove(key).is_some() {
            let rx8 = key.chunk.cx().div_euclid(8);
            let rz8 = key.chunk.cz().div_euclid(8);
            let lx = key.chunk.cx().rem_euclid(8) as u32;
            let lz = key.chunk.cz().rem_euclid(8) as u32;
            if let Some(mask) = self.lod0_region_masks.get_mut(&(rx8, rz8)) {
                *mask &= !(1u64 << (lz * 8 + lx));
            }
            let bkey = Lod0BatchKey::from_chunk(key.chunk);
            if self.lod0_batches.contains_key(&bkey) {
                self.dirty_batches
                    .insert(bkey, Instant::now() - Duration::from_secs(1));
            } else {
                self.dirty_batches.insert(bkey, Instant::now());
            }
            self.lod0_mask_dirty = true;
        }
    }

    pub fn remove_lod_region(&mut self, key: &LodRegionKey) {
        if let Some(old) = self.lod_regions.remove(key) {
            self.vram_bytes = self.vram_bytes.saturating_sub(old.total_bytes);
        }
    }

    pub fn remove_chunk_lod0_only(&mut self, pos: ChunkPos) {
        let k = SectionMeshKey {
            chunk: pos,
            section_y: i32::MIN,
            lod: 0,
        };
        self.remove_section(&k);
    }

    pub fn invalidate_chunk(&mut self, pos: ChunkPos) {
        self.remove_chunk_lod0_only(pos);
        let rx8 = pos.cx().div_euclid(8);
        let rz8 = pos.cz().div_euclid(8);
        for lod in [1u8, 2u8, 3u8] {
            self.remove_lod_region(&LodRegionKey { rx8, rz8, lod });
        }
    }

    pub fn clear_all_meshes(&mut self) {
        self.cpu_sections.clear();
        self.pending_lod0.clear();
        self.lod0_batches.clear();
        self.dirty_batches.clear();
        self.lod0_region_masks.clear();
        self.lod0_mask_dirty = true;
        self.lod_regions.clear();
        self.evicted_lod0.clear();
        self.evicted_lod_regions.clear();
        self.vram_bytes = 0;
        self.grid_tri_buf = None;
        self.grid_line_buf = None;
    }

    pub fn set_chunk_grid(&mut self, device: &wgpu::Device, grid: Option<&OverlayMeshBuilder>) {
        let Some(grid) = grid else {
            self.grid_tri_buf = None;
            self.grid_line_buf = None;
            return;
        };
        self.grid_tri_buf = if !grid.tri_verts.is_empty() {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("static_chunk_grid_tris"),
                contents: bytemuck::cast_slice(&grid.tri_verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            Some((buf, grid.tri_verts.len() as u32))
        } else {
            None
        };
        self.grid_line_buf = if !grid.line_verts.is_empty() {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("static_chunk_grid_lines"),
                contents: bytemuck::cast_slice(&grid.line_verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            Some((buf, grid.line_verts.len() as u32))
        } else {
            None
        };
    }

    pub fn set_preview_mesh(
        &mut self,
        device: &wgpu::Device,
        origin: Vec3,
        data: Option<&SectionMeshData>,
    ) {
        self.preview_origin = origin;
        self.preview_mesh = data.filter(|d| !d.is_empty()).map(|d| {
            GpuSectionMesh::from_data(
                device,
                &self.packed_quad_bind_layout,
                SectionMeshKey {
                    chunk: ChunkPos::new(0, 0),
                    section_y: i32::MIN,
                    lod: 0,
                },
                d,
            )
        });
    }

    /// Returns `(lod0_chunk_meshes, lod_region_meshes, total_vram_bytes)`.
    pub fn stats(&self) -> (usize, usize, usize) {
        (self.cpu_sections.len(), self.lod_regions.len(), self.vram_bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        camera: &CameraController,
        viewport_size: (u32, u32),
        sky_color: [f32; 4],
        overlays: &OverlayMeshBuilder,
    ) {
        self.last_cam_chunk = ChunkPos::new(
            (camera.pos.x / 16.0).floor() as i32,
            (camera.pos.z / 16.0).floor() as i32,
        );

        let vp = camera.view_proj(viewport_size.0 as f32, viewport_size.1 as f32);
        let frustum = Frustum::from_view_proj(vp);
        let is_2d = camera.mode == ViewMode::Chunk2D;

        if is_2d || self.cpu_sections.is_empty() {
            self.cached_lod0_origin = [0i32; 4];
        } else {
            let origin_cx = (camera.pos.x / 16.0).floor() as i32 - 128;
            let origin_cz = (camera.pos.z / 16.0).floor() as i32 - 128;
            let desired_origin = [origin_cx, origin_cz, 1, 0];
            if self.lod0_mask_dirty || self.cached_lod0_origin != desired_origin {
                self.cached_lod0_origin = desired_origin;
                self.cached_lod0_mask.fill([[0u32; 4]; 16]);
                for k in self.cpu_sections.keys() {
                    let rx = k.chunk.cx() - origin_cx;
                    let rz = k.chunk.cz() - origin_cz;
                    if (0..256).contains(&rx) && (0..256).contains(&rz) {
                        let bit = ((rz as u32) << 8) | (rx as u32);
                        let word_idx = (bit >> 5) as usize;
                        let vec_idx = word_idx >> 2;
                        let comp_idx = word_idx & 3;
                        let bit_idx = bit & 31;
                        self.cached_lod0_mask[vec_idx >> 4][vec_idx & 15][comp_idx] |=
                            1u32 << bit_idx;
                    }
                }
                self.lod0_mask_dirty = false;
            }
        }

        let cam_u = CameraUniform {
            view_proj: vp.to_cols_array_2d(),
            cam_pos: [camera.pos.x, camera.pos.y, camera.pos.z, 1.0],
            diff_params: self.diff_params,
            lod0_origin: self.cached_lod0_origin,
            lod0_mask: *self.cached_lod0_mask,
        };
        queue.write_buffer(&self.cam_uniform_buf, 0, bytemuck::bytes_of(&cam_u));

        let ov_u = OverlayUniform {
            view_proj: vp.to_cols_array_2d(),
        };
        queue.write_buffer(&self.overlay_uniform_buf, 0, bytemuck::bytes_of(&ov_u));

        let preview_vp = vp * Mat4::from_translation(self.preview_origin);
        let mut preview_uniform_buf = None;
        let mut preview_bind_group = None;
        if self.preview_mesh.is_some() {
            let pu = CameraUniform {
                view_proj: preview_vp.to_cols_array_2d(),
                cam_pos: [camera.pos.x, camera.pos.y, camera.pos.z, 1.0],
                diff_params: self.diff_params,
                lod0_origin: [0; 4],
                lod0_mask: [[[0; 4]; 16]; 32],
            };
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("preview_cam_u"),
                contents: bytemuck::bytes_of(&pu),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let tex_view = self
                .atlas_texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let bg = Self::make_terrain_bind_group(
                device,
                &self.terrain_bind_layout,
                &buf,
                &tex_view,
                &self.atlas_sampler,
            );
            preview_uniform_buf = Some(buf);
            preview_bind_group = Some(bg);
        }
        let _ = preview_uniform_buf;

        let ov_tri_buf = if !overlays.tri_verts.is_empty() {
            Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay_tris"),
                contents: bytemuck::cast_slice(&overlays.tri_verts),
                usage: wgpu::BufferUsages::VERTEX,
            }))
        } else {
            None
        };
        let ov_line_buf = if !overlays.line_verts.is_empty() {
            Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay_lines"),
                contents: bytemuck::cast_slice(&overlays.line_verts),
                usage: wgpu::BufferUsages::VERTEX,
            }))
        } else {
            None
        };

        // Ensure shared static quad index buffer is large enough for the biggest vertex or packed bucket
        let mut max_quads = 0usize;
        for s in self.lod0_batches.values().chain(self.pending_lod0.values()) {
            for b in [
                &s.opaque,
                &s.alpha_cutout,
                &s.translucent,
                &s.diff_red,
                &s.diff_gray_a,
                &s.diff_gray_b,
            ]
            .into_iter()
            .flatten()
            {
                max_quads = max_quads.max(b.quad_count);
            }
            for b in [
                &s.packed_opaque,
                &s.packed_cutout,
                &s.packed_translucent,
                &s.packed_diff_red,
                &s.packed_diff_gray_a,
                &s.packed_diff_gray_b,
            ]
            .into_iter()
            .flatten()
            {
                max_quads = max_quads.max(b.quad_count);
            }
        }
        for r in self.lod_regions.values() {
            for b in [&r.opaque, &r.translucent].into_iter().flatten() {
                max_quads = max_quads.max(b.quad_count);
            }
        }
        if let Some(pm) = &self.preview_mesh
            && let Some(b) = &pm.translucent
        {
            max_quads = max_quads.max(b.quad_count);
        }
        if max_quads > self.shared_quad_ibuf_quads {
            let new_cap = max_quads.next_power_of_two();
            self.shared_quad_ibuf = create_shared_quad_ibuf(device, new_cap);
            self.shared_quad_ibuf_quads = new_cap;
        }

        let cam_pos = camera.pos;
        let mut visible_sections: Vec<(&GpuSectionMesh, f32, u8)> = if !is_2d {
            self.lod0_batches
                .values()
                .chain(self.pending_lod0.values())
                .filter(|s| frustum.intersects_aabb(s.aabb_min, s.aabb_max))
                .map(|s| {
                    let center = (s.aabb_min + s.aabb_max) * 0.5;
                    let mask = batch_visible_face_mask(cam_pos, s.aabb_min, s.aabb_max);
                    (s, center.distance_squared(cam_pos), mask)
                })
                .collect()
        } else {
            Vec::new()
        };
        // Front-to-back sort for hardware Early-Z rejection of occluded hills and underground terrain
        visible_sections.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));

        let mut visible_lods: Vec<(&GpuLodRegionMesh, f32)> = self
            .lod_regions
            .iter()
            .filter(|(k, r)| {
                let lod_matches = if is_2d {
                    k.lod == 3
                } else {
                    if r.present_mask != 0 {
                        let cov = self
                            .lod0_region_masks
                            .get(&(k.rx8, k.rz8))
                            .copied()
                            .unwrap_or(0);
                        if (cov & r.present_mask) == r.present_mask {
                            return false;
                        }
                    }
                    k.lod == 1 || k.lod == 2
                };
                lod_matches && frustum.intersects_aabb(r.aabb_min, r.aabb_max)
            })
            .map(|(_, r)| {
                let center = (r.aabb_min + r.aabb_max) * 0.5;
                (r, center.distance_squared(cam_pos))
            })
            .collect();
        if !is_2d {
            visible_lods.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
        }

        {
            let mut calls = 0u64;
            let mut indices = 0u64;
            for (s, _, face_mask) in &visible_sections {
                for b in [
                    &s.opaque,
                    &s.alpha_cutout,
                    &s.translucent,
                    &s.diff_red,
                    &s.diff_gray_a,
                    &s.diff_gray_b,
                ]
                .into_iter()
                .flatten()
                {
                    calls += 1;
                    indices += b.index_count as u64;
                }
                for b in [
                    &s.packed_opaque,
                    &s.packed_cutout,
                    &s.packed_translucent,
                    &s.packed_diff_red,
                    &s.packed_diff_gray_a,
                    &s.packed_diff_gray_b,
                ]
                .into_iter()
                .flatten()
                {
                    let mut vis_idx = 0u64;
                    for (f, &(st, en)) in b.face_ranges.iter().enumerate() {
                        if (face_mask & (1u8 << f)) != 0 {
                            vis_idx += (en - st) as u64;
                        }
                    }
                    if vis_idx > 0 {
                        calls += 1;
                        indices += vis_idx;
                    }
                }
            }
            for (r, _) in &visible_lods {
                for b in [&r.opaque, &r.translucent].into_iter().flatten() {
                    calls += 1;
                    indices += b.index_count as u64;
                }
            }
            mcedit_core::profiler::add(mcedit_core::profiler::Counter::DrawCalls, calls);
            mcedit_core::profiler::add(mcedit_core::profiler::Counter::Triangles, indices / 3);
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world_render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: sky_color[0] as f64,
                            g: sky_color[1] as f64,
                            b: sky_color[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_bind_group(0, &self.terrain_bind_group, &[]);
            pass.set_index_buffer(self.shared_quad_ibuf.slice(..), wgpu::IndexFormat::Uint32);

            if !is_2d {
                // 1a. LOD 0 Opaque (Sodium-style 8-byte packed cube quads, front-to-back + 6-face culled)
                pass.set_pipeline(&self.packed_opaque_pipeline);
                for (s, _, face_mask) in &visible_sections {
                    if let Some(b) = &s.packed_opaque {
                        draw_packed_visible_faces(&mut pass, b, *face_mask);
                    }
                }

                // 1b. LOD 0 Opaque (non-cube blocks: stairs, slabs, fences, etc.)
                pass.set_pipeline(&self.opaque_pipeline);
                for (s, _, _) in &visible_sections {
                    if let Some(b) = &s.opaque {
                        pass.set_vertex_buffer(0, b.vbuf.slice(..));
                        pass.draw_indexed(0..b.index_count, 0, 0..1);
                    }
                }

                // 2a. LOD 0 Alpha Cutout (packed cube quads: leaves, glass, backface + 6-face culled)
                pass.set_pipeline(&self.packed_cutout_pipeline);
                for (s, _, face_mask) in &visible_sections {
                    if let Some(b) = &s.packed_cutout {
                        draw_packed_visible_faces(&mut pass, b, *face_mask);
                    }
                }

                // 2b. LOD 0 Alpha Cutout (non-cube: grass, flowers, torches, etc.)
                pass.set_pipeline(&self.cutout_pipeline);
                for (s, _, _) in &visible_sections {
                    if let Some(b) = &s.alpha_cutout {
                        pass.set_vertex_buffer(0, b.vbuf.slice(..));
                        pass.draw_indexed(0..b.index_count, 0, 0..1);
                    }
                }

                // 2c. World Compare Mismatched Blocks (Semi-transparent Red tint over texture, depth-writing)
                let has_diff_red = visible_sections
                    .iter()
                    .any(|(s, _, _)| s.packed_diff_red.is_some() || s.diff_red.is_some());
                if has_diff_red {
                    pass.set_pipeline(&self.packed_diff_red_pipeline);
                    for (s, _, face_mask) in &visible_sections {
                        if let Some(b) = &s.packed_diff_red {
                            draw_packed_visible_faces(&mut pass, b, *face_mask);
                        }
                    }
                    pass.set_pipeline(&self.diff_red_pipeline);
                    for (s, _, _) in &visible_sections {
                        if let Some(b) = &s.diff_red {
                            pass.set_vertex_buffer(0, b.vbuf.slice(..));
                            pass.draw_indexed(0..b.index_count, 0, 0..1);
                        }
                    }
                }

                // 3. Distant True-3D LOD 1/2 Opaque (LOD 0 chunks masked cleanly in fs_lod)
                if !visible_lods.is_empty() {
                    pass.set_pipeline(&self.lod_pipeline);
                    for (r, _) in &visible_lods {
                        if let Some(b) = &r.opaque {
                            pass.set_vertex_buffer(0, b.vbuf.slice(..));
                            pass.draw_indexed(0..b.index_count, 0, 0..1);
                        }
                    }
                }

                // 3b. World Compare Only-in-A Blocks (2-pass depth prepass + light cool gray alpha blend)
                let has_diff_gray_a = visible_sections
                    .iter()
                    .any(|(s, _, _)| s.packed_diff_gray_a.is_some() || s.diff_gray_a.is_some());
                if has_diff_gray_a {
                    pass.set_pipeline(&self.packed_depth_only_pipeline);
                    for (s, _, face_mask) in &visible_sections {
                        if let Some(b) = &s.packed_diff_gray_a {
                            draw_packed_visible_faces(&mut pass, b, *face_mask);
                        }
                    }
                    pass.set_pipeline(&self.depth_only_pipeline);
                    for (s, _, _) in &visible_sections {
                        if let Some(b) = &s.diff_gray_a {
                            pass.set_vertex_buffer(0, b.vbuf.slice(..));
                            pass.draw_indexed(0..b.index_count, 0, 0..1);
                        }
                    }
                    pass.set_pipeline(&self.packed_diff_gray_a_pipeline);
                    for (s, _, face_mask) in &visible_sections {
                        if let Some(b) = &s.packed_diff_gray_a {
                            draw_packed_visible_faces(&mut pass, b, *face_mask);
                        }
                    }
                    pass.set_pipeline(&self.diff_gray_a_pipeline);
                    for (s, _, _) in &visible_sections {
                        if let Some(b) = &s.diff_gray_a {
                            pass.set_vertex_buffer(0, b.vbuf.slice(..));
                            pass.draw_indexed(0..b.index_count, 0, 0..1);
                        }
                    }
                }

                // 3c. World Compare Only-in-B Blocks (2-pass depth prepass + darker slate gray ghost blend)
                let has_diff_gray_b = visible_sections
                    .iter()
                    .any(|(s, _, _)| s.packed_diff_gray_b.is_some() || s.diff_gray_b.is_some());
                if has_diff_gray_b {
                    pass.set_pipeline(&self.packed_depth_only_pipeline);
                    for (s, _, face_mask) in &visible_sections {
                        if let Some(b) = &s.packed_diff_gray_b {
                            draw_packed_visible_faces(&mut pass, b, *face_mask);
                        }
                    }
                    pass.set_pipeline(&self.depth_only_pipeline);
                    for (s, _, _) in &visible_sections {
                        if let Some(b) = &s.diff_gray_b {
                            pass.set_vertex_buffer(0, b.vbuf.slice(..));
                            pass.draw_indexed(0..b.index_count, 0, 0..1);
                        }
                    }
                    pass.set_pipeline(&self.packed_diff_gray_b_pipeline);
                    for (s, _, face_mask) in &visible_sections {
                        if let Some(b) = &s.packed_diff_gray_b {
                            draw_packed_visible_faces(&mut pass, b, *face_mask);
                        }
                    }
                    pass.set_pipeline(&self.diff_gray_b_pipeline);
                    for (s, _, _) in &visible_sections {
                        if let Some(b) = &s.diff_gray_b {
                            pass.set_vertex_buffer(0, b.vbuf.slice(..));
                            pass.draw_indexed(0..b.index_count, 0, 0..1);
                        }
                    }
                }

                // 4. Distant True-3D LOD 1/2 Translucent (water, stained glass, ice, diff ghosts)
                if !visible_lods.is_empty() {
                    pass.set_pipeline(&self.lod_translucent_pipeline);
                    for (r, _) in &visible_lods {
                        if let Some(b) = &r.translucent {
                            pass.set_vertex_buffer(0, b.vbuf.slice(..));
                            pass.draw_indexed(0..b.index_count, 0, 0..1);
                        }
                    }
                }

                // 5a. LOD 0 Translucent (packed cube quads: stained glass, ice)
                pass.set_pipeline(&self.packed_translucent_pipeline);
                for (s, _, face_mask) in &visible_sections {
                    if let Some(b) = &s.packed_translucent {
                        draw_packed_visible_faces(&mut pass, b, *face_mask);
                    }
                }

                // 5b. LOD 0 Translucent (non-cube: water, panes, etc.)
                pass.set_pipeline(&self.translucent_pipeline);
                for (s, _, _) in &visible_sections {
                    if let Some(b) = &s.translucent {
                        pass.set_vertex_buffer(0, b.vbuf.slice(..));
                        pass.draw_indexed(0..b.index_count, 0, 0..1);
                    }
                }
            } else if !visible_lods.is_empty() {
                // 2D Chunk View overhead super-regions
                pass.set_pipeline(&self.lod_pipeline);
                for (r, _) in &visible_lods {
                    if let Some(b) = &r.opaque {
                        pass.set_vertex_buffer(0, b.vbuf.slice(..));
                        pass.draw_indexed(0..b.index_count, 0, 0..1);
                    }
                }
            }

            // 6. Optional Clone/Brush Preview Mesh
            if let (Some(pm), Some(pbg)) = (&self.preview_mesh, &preview_bind_group)
                && let Some(b) = &pm.translucent
            {
                pass.set_pipeline(&self.translucent_pipeline);
                pass.set_bind_group(0, pbg, &[]);
                pass.set_vertex_buffer(0, b.vbuf.slice(..));
                pass.draw_indexed(0..b.index_count, 0, 0..1);
            }

            // 7. Cached Static World Chunk Grid + Dynamic Overlays
            pass.set_bind_group(0, &self.overlay_bind_group, &[]);
            if self.grid_tri_buf.is_some() || ov_tri_buf.is_some() {
                pass.set_pipeline(&self.overlay_tri_pipeline);
                if let Some((gb, cnt)) = &self.grid_tri_buf {
                    pass.set_vertex_buffer(0, gb.slice(..));
                    pass.draw(0..*cnt, 0..1);
                }
                if let Some(tb) = &ov_tri_buf {
                    pass.set_vertex_buffer(0, tb.slice(..));
                    pass.draw(0..overlays.tri_verts.len() as u32, 0..1);
                }
            }
            if self.grid_line_buf.is_some() || ov_line_buf.is_some() {
                pass.set_pipeline(&self.overlay_line_pipeline);
                if let Some((gb, cnt)) = &self.grid_line_buf {
                    pass.set_vertex_buffer(0, gb.slice(..));
                    pass.draw(0..*cnt, 0..1);
                }
                if let Some(lb) = &ov_line_buf {
                    pass.set_vertex_buffer(0, lb.slice(..));
                    pass.draw(0..overlays.line_verts.len() as u32, 0..1);
                }
            }
        }
    }
}

fn create_atlas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &TextureAtlas,
) -> (wgpu::Texture, wgpu::TextureView) {
    // 5 mip levels: 512 -> 256 -> 128 -> 64 -> 32 (16x16 -> 8x8 -> 4x4 -> 2x2 -> 1x1 per tile).
    // Stopping at mip 4 guarantees neighboring atlas tiles NEVER bleed into each other!
    const MIP_LEVELS: u32 = 5;
    let size = wgpu::Extent3d {
        width: ATLAS_SIZE,
        height: ATLAS_SIZE,
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain_atlas"),
        size,
        mip_level_count: MIP_LEVELS,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let mut cur_w = ATLAS_SIZE;
    let mut cur_data: Vec<u8> = atlas.rgba.as_raw().to_vec();
    for mip in 0..MIP_LEVELS {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &cur_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * cur_w),
                rows_per_image: Some(cur_w),
            },
            wgpu::Extent3d {
                width: cur_w,
                height: cur_w,
                depth_or_array_layers: 1,
            },
        );
        if mip + 1 < MIP_LEVELS {
            let next_w = (cur_w / 2).max(1);
            let mut next_data = vec![0u8; (next_w * next_w * 4) as usize];
            for y in 0..next_w {
                for x in 0..next_w {
                    let sx = x * 2;
                    let sy = y * 2;
                    let mut r_acc = 0u32;
                    let mut g_acc = 0u32;
                    let mut b_acc = 0u32;
                    let mut a_acc = 0u32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let idx = (((sy + dy) * cur_w + (sx + dx)) * 4) as usize;
                            let a = cur_data[idx + 3] as u32;
                            // Alpha-weighted RGB prevents transparent cutout margins from darkening distant mips
                            r_acc += (cur_data[idx] as u32) * a;
                            g_acc += (cur_data[idx + 1] as u32) * a;
                            b_acc += (cur_data[idx + 2] as u32) * a;
                            a_acc += a;
                        }
                    }
                    let dst = ((y * next_w + x) * 4) as usize;
                    if let Some(a_nz) = std::num::NonZeroU32::new(a_acc) {
                        next_data[dst] = (r_acc / a_nz).min(255) as u8;
                        next_data[dst + 1] = (g_acc / a_nz).min(255) as u8;
                        next_data[dst + 2] = (b_acc / a_nz).min(255) as u8;
                        next_data[dst + 3] = ((a_acc + 2) / 4).min(255) as u8;
                    }
                }
            }
            cur_w = next_w;
            cur_data = next_data;
        }
    }

    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("world_depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn create_backdrop_blur_resources(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> BackdropBlurResources {
    let qw = (width / 4).max(1);
    let qh = (height / 4).max(1);

    let capture_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("backdrop_capture_tex"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let capture_view = capture_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let make_quarter_tex = |label: &str| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: qw,
                height: qh,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    };
    let ping_view = make_quarter_tex("backdrop_blur_ping");
    let pong_view = make_quarter_tex("backdrop_blur_pong");

    let h_u = BlurUniform {
        step_uv: [1.0 / (qw as f32), 0.0, 0.0, 0.0],
    };
    let v_u = BlurUniform {
        step_uv: [0.0, 1.0 / (qh as f32), 0.0, 0.0],
    };
    let h_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("blur_h_uniform"),
        contents: bytemuck::bytes_of(&h_u),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let v_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("blur_v_uniform"),
        contents: bytemuck::bytes_of(&v_u),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let make_bg = |label: &str, ubuf: &wgpu::Buffer, view: &wgpu::TextureView| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    };

    let h_bind_group = make_bg("blur_h_bg", &h_buf, &capture_view);
    let v_bind_group = make_bg("blur_v_bg", &v_buf, &ping_view);
    let blit_bind_group = make_bg("blur_blit_bg", &h_buf, &pong_view);

    BackdropBlurResources {
        capture_tex,
        ping_view,
        pong_view,
        h_bind_group,
        v_bind_group,
        blit_bind_group,
        quarter_size: (qw, qh),
    }
}

fn make_blur_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    fs_entry: &str,
    surface_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs_entry),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn make_terrain_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs_entry: &str,
    fs_entry: &str,
    surface_format: wgpu::TextureFormat,
    v_layout: Option<&wgpu::VertexBufferLayout<'_>>,
    depth_write: bool,
    cull_mode: Option<wgpu::Face>,
    blend: Option<wgpu::BlendState>,
    bias: wgpu::DepthBiasState,
) -> wgpu::RenderPipeline {
    make_terrain_pipeline_ext(
        device,
        layout,
        shader,
        vs_entry,
        fs_entry,
        surface_format,
        v_layout,
        depth_write,
        wgpu::CompareFunction::Less,
        cull_mode,
        blend,
        wgpu::ColorWrites::ALL,
        bias,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_terrain_pipeline_ext(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs_entry: &str,
    fs_entry: &str,
    surface_format: wgpu::TextureFormat,
    v_layout: Option<&wgpu::VertexBufferLayout<'_>>,
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
    cull_mode: Option<wgpu::Face>,
    blend: Option<wgpu::BlendState>,
    write_mask: wgpu::ColorWrites,
    bias: wgpu::DepthBiasState,
) -> wgpu::RenderPipeline {
    let buffers: [Option<wgpu::VertexBufferLayout<'_>>; 1] = [v_layout.cloned()];
    let buffer_slice: &[Option<wgpu::VertexBufferLayout<'_>>] = if v_layout.is_some() {
        &buffers
    } else {
        &[]
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs_entry),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vs_entry),
            buffers: buffer_slice,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend,
                write_mask,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode,
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(depth_write),
            depth_compare: Some(depth_compare),
            stencil: Default::default(),
            bias,
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn make_overlay_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    surface_format: wgpu::TextureFormat,
    v_layout: &wgpu::VertexBufferLayout<'_>,
    topology: wgpu::PrimitiveTopology,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("overlay_pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(v_layout.clone())],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: Default::default(),
            bias: if topology == wgpu::PrimitiveTopology::TriangleList {
                wgpu::DepthBiasState {
                    constant: -2,
                    slope_scale: -1.0,
                    clamp: 0.0,
                }
            } else {
                wgpu::DepthBiasState::default()
            },
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}
