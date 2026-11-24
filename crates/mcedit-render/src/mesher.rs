use crate::atlas::{LodColorTable, TextureAtlas, should_tint_face};
use bytemuck::{Pod, Zeroable};
use mcedit_core::{
    BlockMeta, BlockRegistry, BlockShape, BlockStateId, Chunk, ChunkSection, Face, LodChunkSurface,
    LodChunkVoxels, SchematicVolume,
};
use std::collections::BTreeMap;
use std::sync::Arc;

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct TerrainVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

/// Sodium-style 8-byte packed greedy cube quad for LOD 0 terrain (15x smaller than 4 verts + 6 indices).
/// `lx` and `lz` are 4x4-chunk-batch-local (`0..63`), so merging 16 chunks into a 4x4 super-batch
/// is a zero-fixup `extend_from_slice`.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct PackedCubeQuad {
    /// bits 0..6:   lx (0..63, batch-local X)
    /// bits 6..12:  lz (0..63, batch-local Z)
    /// bits 12..24: y + 1024 (supports world Y in -1024..=3071)
    /// bits 24..28: du - 1 (0..15 -> span 1..=16)
    /// bits 28..32: dv - 1 (0..15 -> span 1..=16)
    pub w0: u32,
    /// bits 0..3:   face (0=East, 1=West, 2=Up, 3=Down, 4=South, 5=North)
    /// bits 3..8:   atlas tile col (0..31)
    /// bits 8..13:  atlas tile row (0..31)
    /// bits 13..19: r >> 2 (6-bit red)
    /// bits 19..25: g >> 2 (6-bit green)
    /// bits 25..31: b >> 2 (6-bit blue)
    /// bit  31:     is_liquid (1 = liquid alpha 200/255 & Up face lowered by 0.125)
    pub w1: u32,
}

impl PackedCubeQuad {
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn pack(
        lx: u32,
        lz: u32,
        y: i32,
        du: u32,
        dv: u32,
        face: Face,
        tile: [u8; 2],
        color: [u8; 4],
        is_liquid: bool,
    ) -> Self {
        let y_biased = ((y + 1024).clamp(0, 4095)) as u32;
        let w0 = (lx & 0x3F)
            | ((lz & 0x3F) << 6)
            | ((y_biased & 0xFFF) << 12)
            | (((du.saturating_sub(1)) & 0xF) << 24)
            | (((dv.saturating_sub(1)) & 0xF) << 28);
        let r6 = ((color[0] as u32) >> 2) & 0x3F;
        let g6 = ((color[1] as u32) >> 2) & 0x3F;
        let b6 = ((color[2] as u32) >> 2) & 0x3F;
        let w1 = ((face.index() as u32) & 0x7)
            | (((tile[0] as u32) & 0x1F) << 3)
            | (((tile[1] as u32) & 0x1F) << 8)
            | (r6 << 13)
            | (g6 << 19)
            | (b6 << 25)
            | (if is_liquid { 1u32 << 31 } else { 0 });
        Self { w0, w1 }
    }

    #[inline]
    pub fn y_bounds(&self) -> (f32, f32) {
        let y0 = (((self.w0 >> 12) & 0xFFF) as i32 - 1024) as f32;
        let face = self.w1 & 0x7;
        let height = if face == 2 || face == 3 {
            1.0
        } else {
            (((self.w0 >> 28) & 0xF) + 1) as f32
        };
        (y0, y0 + height)
    }

    #[inline]
    pub fn face(&self) -> u32 {
        self.w1 & 0x7
    }

    /// Unpacks the 4 corners `[TL, TR, BR, BL]` in world space given the 4x4 batch origin.
    pub fn unpack_corners(&self, origin_x: f32, origin_z: f32) -> [[f32; 3]; 4] {
        let lx = (self.w0 & 0x3F) as f32;
        let lz = ((self.w0 >> 6) & 0x3F) as f32;
        let y0 = (((self.w0 >> 12) & 0xFFF) as i32 - 1024) as f32;
        let du = (((self.w0 >> 24) & 0xF) + 1) as f32;
        let dv = (((self.w0 >> 28) & 0xF) + 1) as f32;
        let x0 = origin_x + lx;
        let z0 = origin_z + lz;
        match self.w1 & 0x7 {
            0 => unit_face_corners(Face::East, [x0, y0, z0], [x0 + 1.0, y0 + dv, z0 + du]),
            1 => unit_face_corners(Face::West, [x0, y0, z0], [x0 + 1.0, y0 + dv, z0 + du]),
            2 => unit_face_corners(Face::Up, [x0, y0, z0], [x0 + du, y0 + 1.0, z0 + dv]),
            3 => unit_face_corners(Face::Down, [x0, y0, z0], [x0 + du, y0 + 1.0, z0 + dv]),
            4 => unit_face_corners(Face::South, [x0, y0, z0], [x0 + du, y0 + dv, z0 + 1.0]),
            _ => unit_face_corners(Face::North, [x0, y0, z0], [x0 + du, y0 + dv, z0 + 1.0]),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MeshBucket {
    pub vertices: Vec<TerrainVertex>,
}

impl MeshBucket {
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    #[inline]
    pub fn quad_count(&self) -> usize {
        self.vertices.len() / 4
    }

    #[inline]
    pub fn index_count(&self) -> u32 {
        ((self.vertices.len() / 4) * 6) as u32
    }

    pub fn byte_size(&self) -> usize {
        self.vertices.len() * std::mem::size_of::<TerrainVertex>()
    }

    pub fn append(&mut self, other: &MeshBucket) {
        if other.is_empty() {
            return;
        }
        self.vertices.extend_from_slice(&other.vertices);
    }

    pub fn push_quad(&mut self, corners: [[f32; 3]; 4], uv: [f32; 4], color: [u8; 4]) {
        let [u0, v0, u1, v1] = uv;
        let uvs = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
        for i in 0..4 {
            self.vertices.push(TerrainVertex {
                pos: corners[i],
                uv: uvs[i],
                color,
            });
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SectionMeshData {
    pub opaque: MeshBucket,
    pub alpha_cutout: MeshBucket,
    pub translucent: MeshBucket,
    pub packed_opaque: Vec<PackedCubeQuad>,
    pub packed_cutout: Vec<PackedCubeQuad>,
    pub packed_translucent: Vec<PackedCubeQuad>,
    // World-Compare diff buckets (empty = 0 bytes, 0 draw calls when compare is off)
    pub packed_diff_red: Vec<PackedCubeQuad>,
    pub packed_diff_gray_a: Vec<PackedCubeQuad>,
    pub packed_diff_gray_b: Vec<PackedCubeQuad>,
    pub diff_red: MeshBucket,
    pub diff_gray_a: MeshBucket,
    pub diff_gray_b: MeshBucket,
}

impl SectionMeshData {
    pub fn is_empty(&self) -> bool {
        self.opaque.is_empty()
            && self.alpha_cutout.is_empty()
            && self.translucent.is_empty()
            && self.packed_opaque.is_empty()
            && self.packed_cutout.is_empty()
            && self.packed_translucent.is_empty()
            && self.packed_diff_red.is_empty()
            && self.packed_diff_gray_a.is_empty()
            && self.packed_diff_gray_b.is_empty()
            && self.diff_red.is_empty()
            && self.diff_gray_a.is_empty()
            && self.diff_gray_b.is_empty()
    }

    pub fn byte_size(&self) -> usize {
        self.opaque.byte_size()
            + self.alpha_cutout.byte_size()
            + self.translucent.byte_size()
            + self.diff_red.byte_size()
            + self.diff_gray_a.byte_size()
            + self.diff_gray_b.byte_size()
            + (self.packed_opaque.len()
                + self.packed_cutout.len()
                + self.packed_translucent.len()
                + self.packed_diff_red.len()
                + self.packed_diff_gray_a.len()
                + self.packed_diff_gray_b.len())
                * std::mem::size_of::<PackedCubeQuad>()
    }

    pub fn append(&mut self, other: &SectionMeshData) {
        self.opaque.append(&other.opaque);
        self.alpha_cutout.append(&other.alpha_cutout);
        self.translucent.append(&other.translucent);
        self.packed_opaque.extend_from_slice(&other.packed_opaque);
        self.packed_cutout.extend_from_slice(&other.packed_cutout);
        self.packed_translucent
            .extend_from_slice(&other.packed_translucent);
        self.packed_diff_red
            .extend_from_slice(&other.packed_diff_red);
        self.packed_diff_gray_a
            .extend_from_slice(&other.packed_diff_gray_a);
        self.packed_diff_gray_b
            .extend_from_slice(&other.packed_diff_gray_b);
        self.diff_red.append(&other.diff_red);
        self.diff_gray_a.append(&other.diff_gray_a);
        self.diff_gray_b.append(&other.diff_gray_b);
    }

    fn bucket_for(&mut self, meta: &BlockMeta, fast_leaves: bool) -> &mut MeshBucket {
        if meta.is_translucent {
            &mut self.translucent
        } else if meta.shape == BlockShape::Leaves && fast_leaves {
            &mut self.opaque
        } else if meta.is_alpha_cutout {
            &mut self.alpha_cutout
        } else {
            &mut self.opaque
        }
    }
}

/// Snapshot of a chunk and its 4 cardinal neighbors for lock-free parallel `rayon` meshing.
#[derive(Clone)]
pub struct ChunkNeighborSnapshot {
    pub center: Arc<Chunk>,
    pub east: Option<Arc<Chunk>>,
    pub west: Option<Arc<Chunk>>,
    pub south: Option<Arc<Chunk>>,
    pub north: Option<Arc<Chunk>>,
}

impl ChunkNeighborSnapshot {
    pub fn get_block(&self, wx_local: i32, wy: i32, wz_local: i32) -> BlockStateId {
        if wx_local >= 16 {
            self.east
                .as_ref()
                .map(|c| c.get_block((wx_local - 16) as usize, wy, wz_local as usize))
                .unwrap_or(BlockStateId::AIR)
        } else if wx_local < 0 {
            self.west
                .as_ref()
                .map(|c| c.get_block((wx_local + 16) as usize, wy, wz_local as usize))
                .unwrap_or(BlockStateId::AIR)
        } else if wz_local >= 16 {
            self.south
                .as_ref()
                .map(|c| c.get_block(wx_local as usize, wy, (wz_local - 16) as usize))
                .unwrap_or(BlockStateId::AIR)
        } else if wz_local < 0 {
            self.north
                .as_ref()
                .map(|c| c.get_block(wx_local as usize, wy, (wz_local + 16) as usize))
                .unwrap_or(BlockStateId::AIR)
        } else {
            self.center
                .get_block(wx_local as usize, wy, wz_local as usize)
        }
    }

    pub fn get_light(&self, wx_local: i32, wy: i32, wz_local: i32) -> u8 {
        let min_y = self.center.min_y();
        let max_y = self.center.max_y();
        if wy >= max_y {
            return 15;
        }
        if wy < min_y {
            return 4;
        }
        let chunk = if wx_local >= 16 {
            self.east.as_ref()
        } else if wx_local < 0 {
            self.west.as_ref()
        } else if wz_local >= 16 {
            self.south.as_ref()
        } else if wz_local < 0 {
            self.north.as_ref()
        } else {
            Some(&self.center)
        };
        let Some(c) = chunk else {
            return 15;
        };
        let lx = wx_local.rem_euclid(16) as usize;
        let lz = wz_local.rem_euclid(16) as usize;
        let sl = c.get_sky_light(lx, wy, lz);
        let bl = c.get_block_light(lx, wy, lz);
        sl.max(bl).clamp(4, 15)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MeshOptions {
    pub fast_leaves: bool,
    pub rough_graphics: bool,
    pub show_hidden_ores: bool,
    pub hide_barriers: bool,
    /// When true (for distant LOD 0 chunks while the camera is above ground), skips
    /// meshing sections strictly buried below the chunk neighborhood's minimum surface height.
    pub cull_underground: bool,
}

/// Meshes all non-empty sections of a chunk into one merged `SectionMeshData`.
/// Significantly reduces GPU draw calls for LOD 0 rendering compared to per-section draws.
pub fn mesh_chunk_full(
    snap: &ChunkNeighborSnapshot,
    reg: &BlockRegistry,
    opts: MeshOptions,
) -> SectionMeshData {
    let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::Lod0MeshFull);
    let mut out = SectionMeshData::default();
    let min_underground_cutoff = if opts.cull_underground {
        let chunk_min_y = snap.center.min_y();
        // Ignore empty/void columns (heightmap == min_y) and take the 4th-lowest populated
        // surface height (~1.5th percentile) so a single stray cave hole or void edge does not
        // disable underground cave culling for the whole distant chunk, while preserving oceans,
        // rivers, valleys, and ravines.
        let mut valid_heights = [0i32; 256];
        let mut valid_len = 0usize;
        for &h in &snap.center.heightmap {
            if h > chunk_min_y {
                valid_heights[valid_len] = h;
                valid_len += 1;
            }
        }
        if valid_len > 0 {
            let pick = 3.min(valid_len - 1);
            let (_, &mut nth, _) = valid_heights[..valid_len].select_nth_unstable(pick);
            let mut min_surf = nth;
            for nb in [&snap.east, &snap.west, &snap.south, &snap.north]
                .into_iter()
                .flatten()
            {
                let nb_min_y = nb.min_y();
                let mut nb_lowest = i32::MAX;
                for &h in &nb.heightmap {
                    if h > nb_min_y && h < nb_lowest {
                        nb_lowest = h;
                    }
                }
                if nb_lowest < min_surf {
                    min_surf = nb_lowest;
                }
            }
            Some(min_surf.saturating_sub(4))
        } else {
            None
        }
    } else {
        None
    };
    for &sy in snap.center.sections.keys() {
        if let Some(cutoff) = min_underground_cutoff
            && sy * 16 + 15 < cutoff
        {
            continue;
        }
        mesh_section_into(snap, sy, reg, opts, &mut out);
    }
    let packed_quads =
        out.packed_opaque.len() + out.packed_cutout.len() + out.packed_translucent.len();
    mcedit_core::profiler::add(mcedit_core::profiler::Counter::Lod0Chunks, 1);
    mcedit_core::profiler::add(
        mcedit_core::profiler::Counter::Lod0Vertices,
        (out.opaque.vertices.len()
            + out.alpha_cutout.vertices.len()
            + out.translucent.vertices.len()
            + packed_quads * 4) as u64,
    );
    mcedit_core::profiler::add(
        mcedit_core::profiler::Counter::Lod0Indices,
        (out.opaque.index_count() as usize
            + out.alpha_cutout.index_count() as usize
            + out.translucent.index_count() as usize
            + packed_quads * 6) as u64,
    );
    out
}

pub fn mesh_section(
    snap: &ChunkNeighborSnapshot,
    sy: i32,
    reg: &BlockRegistry,
    opts: MeshOptions,
) -> SectionMeshData {
    let mut out = SectionMeshData::default();
    mesh_section_into(snap, sy, reg, opts, &mut out);
    out
}

#[derive(Copy, Clone, PartialEq, Eq, Default)]
struct GreedyCubeFace {
    tile: [u8; 2],
    color: [u8; 4],
    bucket_kind: u8, // 0 = empty, 1 = opaque, 2 = alpha_cutout, 3 = translucent, 4 = packed liquid, 5 = surface-side liquid (0.875 height)
}

/// Direct `O(1)` section + 6-cardinal-neighbor accessor that eliminates `BTreeMap`
/// lookups during inner 16x16x16 section meshing loops.
struct SectionNeighborhood<'a> {
    center: &'a ChunkSection,
    east: Option<&'a ChunkSection>,
    west: Option<&'a ChunkSection>,
    up: Option<&'a ChunkSection>,
    down: Option<&'a ChunkSection>,
    south: Option<&'a ChunkSection>,
    north: Option<&'a ChunkSection>,
    east_chunk_loaded: bool,
    west_chunk_loaded: bool,
    south_chunk_loaded: bool,
    north_chunk_loaded: bool,
    base_y: i32,
    min_y: i32,
    max_y: i32,
}

impl<'a> SectionNeighborhood<'a> {
    #[inline]
    fn get_block(&self, lx: i32, ly: i32, lz: i32) -> BlockStateId {
        if (0..16).contains(&lx) && (0..16).contains(&ly) && (0..16).contains(&lz) {
            return self.center.get_block(lx as usize, ly as usize, lz as usize);
        }
        if lx >= 16 {
            return self
                .east
                .map(|s| s.get_block((lx - 16) as usize, ly as usize, lz as usize))
                .unwrap_or(BlockStateId::AIR);
        }
        if lx < 0 {
            return self
                .west
                .map(|s| s.get_block((lx + 16) as usize, ly as usize, lz as usize))
                .unwrap_or(BlockStateId::AIR);
        }
        if lz >= 16 {
            return self
                .south
                .map(|s| s.get_block(lx as usize, ly as usize, (lz - 16) as usize))
                .unwrap_or(BlockStateId::AIR);
        }
        if lz < 0 {
            return self
                .north
                .map(|s| s.get_block(lx as usize, ly as usize, (lz + 16) as usize))
                .unwrap_or(BlockStateId::AIR);
        }
        if ly >= 16 {
            return self
                .up
                .map(|s| s.get_block(lx as usize, (ly - 16) as usize, lz as usize))
                .unwrap_or(BlockStateId::AIR);
        }
        self.down
            .map(|s| s.get_block(lx as usize, (ly + 16) as usize, lz as usize))
            .unwrap_or(BlockStateId::AIR)
    }

    #[inline]
    fn get_light_and_sky(&self, lx: i32, ly: i32, lz: i32) -> (u8, u8) {
        let wy = self.base_y + ly;
        if wy >= self.max_y {
            return (15, 15);
        }
        if wy < self.min_y {
            return (4, 0);
        }
        let (sec_opt, x, y, z) =
            if (0..16).contains(&lx) && (0..16).contains(&ly) && (0..16).contains(&lz) {
                (Some(self.center), lx as usize, ly as usize, lz as usize)
            } else if lx >= 16 {
                if !self.east_chunk_loaded {
                    return (15, 15);
                }
                (self.east, (lx - 16) as usize, ly as usize, lz as usize)
            } else if lx < 0 {
                if !self.west_chunk_loaded {
                    return (15, 15);
                }
                (self.west, (lx + 16) as usize, ly as usize, lz as usize)
            } else if lz >= 16 {
                if !self.south_chunk_loaded {
                    return (15, 15);
                }
                (self.south, lx as usize, ly as usize, (lz - 16) as usize)
            } else if lz < 0 {
                if !self.north_chunk_loaded {
                    return (15, 15);
                }
                (self.north, lx as usize, ly as usize, (lz + 16) as usize)
            } else if ly >= 16 {
                (self.up, lx as usize, (ly - 16) as usize, lz as usize)
            } else {
                (self.down, lx as usize, (ly + 16) as usize, lz as usize)
            };
        let Some(s) = sec_opt else {
            return (15, 15);
        };
        let sl = s.get_sky_light(x, y, z);
        let bl = s.get_block_light(x, y, z);
        (sl.max(bl).clamp(4, 15), sl)
    }
}

pub(crate) fn mesh_section_into(
    snap: &ChunkNeighborSnapshot,
    sy: i32,
    reg: &BlockRegistry,
    opts: MeshOptions,
    out: &mut SectionMeshData,
) {
    let Some(sec) = snap.center.sections.get(&sy) else {
        return;
    };
    if sec.is_all_air() {
        return;
    }

    let cx_off = (snap.center.pos.cx() * 16) as f32;
    let cz_off = (snap.center.pos.cz() * 16) as f32;
    let base_y = sy * 16;

    let hood = SectionNeighborhood {
        center: sec,
        east: snap.east.as_ref().and_then(|c| c.sections.get(&sy)),
        west: snap.west.as_ref().and_then(|c| c.sections.get(&sy)),
        up: snap.center.sections.get(&(sy + 1)),
        down: snap.center.sections.get(&(sy - 1)),
        south: snap.south.as_ref().and_then(|c| c.sections.get(&sy)),
        north: snap.north.as_ref().and_then(|c| c.sections.get(&sy)),
        east_chunk_loaded: snap.east.is_some(),
        west_chunk_loaded: snap.west.is_some(),
        south_chunk_loaded: snap.south.is_some(),
        north_chunk_loaded: snap.north.is_some(),
        base_y,
        min_y: snap.center.min_y(),
        max_y: snap.center.max_y(),
    };

    // 1. Fast 2D Greedy Meshing across all 6 faces for full-cube blocks
    //    (merges coplanar faces sharing texture tile + shaded color + bucket into 1 quad)
    let mut has_non_cube = false;
    let mut grid = [GreedyCubeFace::default(); 256];

    for face in Face::ALL {
        let n = face.normal();
        for slice in 0..16usize {
            let mut any_in_slice = false;
            for v in 0..16usize {
                for u in 0..16usize {
                    let (lx, ly, lz) = match face {
                        Face::East | Face::West => (slice, v, u),
                        Face::Up | Face::Down => (u, slice, v),
                        Face::South | Face::North => (u, v, slice),
                    };
                    let cell_idx = (v << 4) | u;
                    let id = sec.get_block(lx, ly, lz);
                    if id == BlockStateId::AIR {
                        grid[cell_idx] = GreedyCubeFace::default();
                        continue;
                    }
                    let meta = reg.get_meta(id);
                    if meta.is_air || (opts.hide_barriers && meta.is_barrier) {
                        grid[cell_idx] = GreedyCubeFace::default();
                        continue;
                    }
                    let is_cube_shape = opts.rough_graphics
                        || matches!(
                            meta.shape,
                            BlockShape::Cube
                                | BlockShape::Leaves
                                | BlockShape::Glass
                                | BlockShape::Chest
                                | BlockShape::Liquid
                        );
                    if !is_cube_shape {
                        has_non_cube = true;
                        grid[cell_idx] = GreedyCubeFace::default();
                        continue;
                    }

                    let fast_leaves = opts.fast_leaves || opts.cull_underground;
                    let treat_leaves_opaque =
                        opts.rough_graphics || (fast_leaves && meta.shape == BlockShape::Leaves);
                    let is_trans = !opts.rough_graphics && meta.is_translucent;

                    let nx = lx as i32 + n.x;
                    let ny = ly as i32 + n.y;
                    let nz = lz as i32 + n.z;
                    let nb = hood.get_block(nx, ny, nz);
                    if nb != BlockStateId::AIR {
                        let nmeta = reg.get_meta(nb);
                        let n_opaque = nmeta.is_full_opaque_cube
                            || (treat_leaves_opaque && nmeta.shape == BlockShape::Leaves);
                        if n_opaque {
                            if !opts.show_hidden_ores || !is_ore(reg, id) {
                                grid[cell_idx] = GreedyCubeFace::default();
                                continue;
                            }
                        } else if is_trans && (nb == id || (meta.is_liquid && nmeta.is_liquid)) {
                            grid[cell_idx] = GreedyCubeFace::default();
                            continue;
                        }
                    }

                    let bucket_kind = if opts.rough_graphics {
                        1
                    } else if meta.is_liquid {
                        // Top face (Up) and submerged faces pack into 8-byte PackedCubeQuad (bit 31 = liquid).
                        // Rare exposed side rim of top-layer water (waterfall edge) uses exact 0.875 height.
                        let is_side = !matches!(face, Face::Up | Face::Down);
                        if is_side {
                            let above = hood.get_block(lx as i32, ly as i32 + 1, lz as i32);
                            if above == BlockStateId::AIR || !reg.get_meta(above).is_liquid {
                                5
                            } else {
                                4
                            }
                        } else {
                            4
                        }
                    } else if meta.is_translucent {
                        3
                    } else if meta.shape == BlockShape::Leaves && fast_leaves {
                        1
                    } else if meta.is_alpha_cutout {
                        2
                    } else {
                        1
                    };

                    let (raw_light, nb_sky) = if meta.is_liquid {
                        let (l, sl) = hood.get_light_and_sky(lx as i32, ly as i32, lz as i32);
                        (l.max(10), sl)
                    } else {
                        hood.get_light_and_sky(nx, ny, nz)
                    };

                    // For distant chunks (>16 chunks away while above ground), cull faces inside
                    // completely dark buried caves (even inside tall mountains!) where sky_light == 0
                    // strictly below the column's surface heightmap.
                    if opts.cull_underground && nb_sky == 0 {
                        let wy = base_y + ly as i32;
                        let col_h = snap.center.heightmap[(lz << 4) | lx];
                        if wy < col_h - 3 {
                            grid[cell_idx] = GreedyCubeFace::default();
                            continue;
                        }
                    }

                    // At distance (>16 chunks), quantize lighting to 4 coarse bands so subtle
                    // 1-level shadow ripples don't shatter greedy-merged quads into 1x1 micro-quads.
                    let light = if opts.cull_underground {
                        match raw_light {
                            14..=15 => 15,
                            10..=13 => 12,
                            6..=9 => 8,
                            _ => 4,
                        }
                    } else {
                        raw_light
                    };
                    let alpha = if meta.is_liquid { 200 } else { 255 };
                    let color = shaded_color(meta, face, light, alpha);
                    let tile = meta.face_tiles[face.index()];
                    grid[cell_idx] = GreedyCubeFace {
                        tile,
                        color,
                        bucket_kind,
                    };
                    any_in_slice = true;
                }
            }

            if !any_in_slice {
                continue;
            }

            for v0 in 0..16usize {
                let mut u0 = 0usize;
                while u0 < 16 {
                    let cell = grid[(v0 << 4) | u0];
                    if cell.bucket_kind == 0 {
                        u0 += 1;
                        continue;
                    }
                    let mut du = 1usize;
                    while u0 + du < 16 && grid[(v0 << 4) | (u0 + du)] == cell {
                        du += 1;
                    }
                    let mut dv = 1usize;
                    'extend_v: while v0 + dv < 16 {
                        let row_base = (v0 + dv) << 4;
                        for k in 0..du {
                            if grid[row_base | (u0 + k)] != cell {
                                break 'extend_v;
                            }
                        }
                        dv += 1;
                    }
                    for rv in v0..(v0 + dv) {
                        let row_base = rv << 4;
                        for ru in u0..(u0 + du) {
                            grid[row_base | ru].bucket_kind = 0;
                        }
                    }

                    let (local_x, local_y, local_z) = match face {
                        Face::East | Face::West => (slice, base_y + v0 as i32, u0),
                        Face::Up | Face::Down => (u0, base_y + slice as i32, v0),
                        Face::South | Face::North => (u0, base_y + v0 as i32, slice),
                    };

                    if cell.bucket_kind != 5 && (-1024..=3071).contains(&local_y) {
                        let batch_lx =
                            (snap.center.pos.cx().rem_euclid(4) as u32) * 16 + local_x as u32;
                        let batch_lz =
                            (snap.center.pos.cz().rem_euclid(4) as u32) * 16 + local_z as u32;
                        let packed = PackedCubeQuad::pack(
                            batch_lx,
                            batch_lz,
                            local_y,
                            du as u32,
                            dv as u32,
                            face,
                            cell.tile,
                            cell.color,
                            cell.bucket_kind == 4,
                        );
                        match cell.bucket_kind {
                            2 => out.packed_cutout.push(packed),
                            3 | 4 => out.packed_translucent.push(packed),
                            _ => out.packed_opaque.push(packed),
                        }
                    } else {
                        let top_trim = if cell.bucket_kind == 5 { 0.125f32 } else { 0.0f32 };
                        let (min, max) = match face {
                            Face::East | Face::West => (
                                [
                                    cx_off + slice as f32,
                                    (base_y + v0 as i32) as f32,
                                    cz_off + u0 as f32,
                                ],
                                [
                                    cx_off + (slice + 1) as f32,
                                    (base_y + (v0 + dv) as i32) as f32 - top_trim,
                                    cz_off + (u0 + du) as f32,
                                ],
                            ),
                            Face::Up | Face::Down => (
                                [
                                    cx_off + u0 as f32,
                                    (base_y + slice as i32) as f32,
                                    cz_off + v0 as f32,
                                ],
                                [
                                    cx_off + (u0 + du) as f32,
                                    (base_y + (slice + 1) as i32) as f32 - top_trim,
                                    cz_off + (v0 + dv) as f32,
                                ],
                            ),
                            Face::South | Face::North => (
                                [
                                    cx_off + u0 as f32,
                                    (base_y + v0 as i32) as f32,
                                    cz_off + slice as f32,
                                ],
                                [
                                    cx_off + (u0 + du) as f32,
                                    (base_y + (v0 + dv) as i32) as f32 - top_trim,
                                    cz_off + (slice + 1) as f32,
                                ],
                            ),
                        };

                        let uv = TextureAtlas::tiled_uv(cell.tile, du, dv);
                        let corners = unit_face_corners(face, min, max);
                        let bucket = match cell.bucket_kind {
                            2 => &mut out.alpha_cutout,
                            3..=5 => &mut out.translucent,
                            _ => &mut out.opaque,
                        };
                        bucket.push_quad(corners, uv, cell.color);
                    }
                    u0 += du;
                }
            }
        }
    }

    if !has_non_cube || opts.rough_graphics {
        return;
    }

    // 2. Non-cube block shapes (Liquids, Slabs, Stairs, Plants, Torches, Fences, etc.)
    for ly in 0..16 {
        let wy = base_y + ly as i32;
        for lz in 0..16 {
            for lx in 0..16 {
                let id = sec.get_block(lx, ly, lz);
                if id == BlockStateId::AIR {
                    continue;
                }
                let meta = reg.get_meta(id);
                if meta.is_air {
                    continue;
                }
                // Beyond 16 chunks (>256 blocks), sub-pixel decorative foliage/fixtures are
                // <1px wide yet generate millions of micro-triangles & cutout discard overdraw.
                if opts.cull_underground
                    && matches!(
                        meta.shape,
                        BlockShape::CrossPlant
                            | BlockShape::Torch
                            | BlockShape::Button
                            | BlockShape::Lever
                            | BlockShape::PressurePlate
                            | BlockShape::Ladder
                            | BlockShape::Vine
                            | BlockShape::SignPost
                            | BlockShape::WallSign
                            | BlockShape::Rail
                            | BlockShape::Redstone
                            | BlockShape::Carpet
                            | BlockShape::SnowLayer
                    )
                {
                    continue;
                }
                let state = reg.get_state(id);
                let bx = cx_off + lx as f32;
                let by = wy as f32;
                let bz = cz_off + lz as f32;

                match meta.shape {
                    BlockShape::Cube
                    | BlockShape::Leaves
                    | BlockShape::Glass
                    | BlockShape::Chest
                    | BlockShape::Liquid => {}
                    BlockShape::Slab => {
                        let is_top = state.properties.get("half").is_some_and(|v| v == "top")
                            || meta.legacy_id.is_some_and(|(_, d)| (d & 8) != 0);
                        let (y0, y1) = if is_top { (0.5, 1.0) } else { (0.0, 0.5) };
                        let bucket = out.bucket_for(meta, opts.fast_leaves);
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by + y0, bz],
                            [bx + 1.0, by + y1, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Stairs => {
                        let is_top = state.properties.get("half").is_some_and(|v| v == "top")
                            || meta.legacy_id.is_some_and(|(_, d)| (d & 4) != 0);
                        let facing = state
                            .properties
                            .get("facing")
                            .map(|s| s.as_str())
                            .unwrap_or("north");
                        let bucket = out.bucket_for(meta, opts.fast_leaves);
                        let (base_y0, base_y1, step_y0, step_y1) = if is_top {
                            (0.5, 1.0, 0.0, 0.5)
                        } else {
                            (0.0, 0.5, 0.5, 1.0)
                        };
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by + base_y0, bz],
                            [bx + 1.0, by + base_y1, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                        let (sx0, sz0, sx1, sz1) = match facing {
                            "east" => (0.5, 0.0, 1.0, 1.0),
                            "west" => (0.0, 0.0, 0.5, 1.0),
                            "south" => (0.0, 0.5, 1.0, 1.0),
                            _ => (0.0, 0.0, 1.0, 0.5),
                        };
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + sx0, by + step_y0, bz + sz0],
                            [bx + sx1, by + step_y1, bz + sz1],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::CrossPlant => {
                        let light = snap.get_light(lx as i32, wy, lz as i32);
                        let col = shaded_color(meta, Face::Up, light, 255);
                        let uv = TextureAtlas::tile_uv(meta.face_tiles[0]);
                        let bucket = &mut out.alpha_cutout;
                        bucket.push_quad(
                            [
                                [bx + 0.15, by + 1.0, bz + 0.15],
                                [bx + 0.85, by + 1.0, bz + 0.85],
                                [bx + 0.85, by, bz + 0.85],
                                [bx + 0.15, by, bz + 0.15],
                            ],
                            uv,
                            col,
                        );
                        bucket.push_quad(
                            [
                                [bx + 0.85, by + 1.0, bz + 0.15],
                                [bx + 0.15, by + 1.0, bz + 0.85],
                                [bx + 0.15, by, bz + 0.85],
                                [bx + 0.85, by, bz + 0.15],
                            ],
                            uv,
                            col,
                        );
                    }
                    BlockShape::Torch => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.4375, by, bz + 0.4375],
                            [bx + 0.5625, by + 0.625, bz + 0.5625],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Fence | BlockShape::FenceGate => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.375, by, bz + 0.375],
                            [bx + 0.625, by + 1.0, bz + 0.625],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                        // Connect horizontally to non-air opaque/fence neighbors
                        for (dx, dz, min_x, min_z, max_x, max_z) in [
                            (1, 0, 0.625, 0.4375, 1.0, 0.5625),
                            (-1, 0, 0.0, 0.4375, 0.375, 0.5625),
                            (0, 1, 0.4375, 0.625, 0.5625, 1.0),
                            (0, -1, 0.4375, 0.0, 0.5625, 0.375),
                        ] {
                            let nb = snap.get_block(lx as i32 + dx, wy, lz as i32 + dz);
                            let nm = reg.get_meta(nb);
                            if nm.is_full_opaque_cube
                                || matches!(nm.shape, BlockShape::Fence | BlockShape::FenceGate)
                            {
                                emit_box_all_faces(
                                    snap,
                                    lx as i32,
                                    wy,
                                    lz as i32,
                                    [bx + min_x, by + 0.375, bz + min_z],
                                    [bx + max_x, by + 0.9375, bz + max_z],
                                    meta,
                                    bucket,
                                    255,
                                    None,
                                );
                            }
                        }
                    }
                    BlockShape::Pane => {
                        let bucket = out.bucket_for(meta, opts.fast_leaves);
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.4375, by, bz],
                            [bx + 0.5625, by + 1.0, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Door => {
                        let facing = state
                            .properties
                            .get("facing")
                            .map(|s| s.as_str())
                            .unwrap_or("north");
                        let (x0, z0, x1, z1) = match facing {
                            "east" => (0.0, 0.0, 0.1875, 1.0),
                            "west" => (0.8125, 0.0, 1.0, 1.0),
                            "south" => (0.0, 0.0, 1.0, 0.1875),
                            _ => (0.0, 0.8125, 1.0, 1.0),
                        };
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + x0, by, bz + z0],
                            [bx + x1, by + 1.0, bz + z1],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Trapdoor => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + 0.1875, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::SnowLayer => {
                        let layers = meta
                            .legacy_id
                            .map(|(_, d)| ((d & 7) + 1) as f32 / 8.0)
                            .unwrap_or(0.125);
                        let bucket = &mut out.opaque;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + layers, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Carpet
                    | BlockShape::Rail
                    | BlockShape::Redstone
                    | BlockShape::PressurePlate => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + 0.0625, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Bed => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + 0.5625, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Cactus => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.0625, by, bz + 0.0625],
                            [bx + 0.9375, by + 1.0, bz + 0.9375],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Cake => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.0625, by, bz + 0.0625],
                            [bx + 0.9375, by + 0.5, bz + 0.9375],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::EnchantTable | BlockShape::EndPortalFrame => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + 0.75, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Ladder | BlockShape::Vine => {
                        let facing = state
                            .properties
                            .get("facing")
                            .map(|s| s.as_str())
                            .unwrap_or("north");
                        let (x0, z0, x1, z1) = match facing {
                            "east" => (0.0, 0.0, 0.0625, 1.0),
                            "west" => (0.9375, 0.0, 1.0, 1.0),
                            "south" => (0.0, 0.0, 1.0, 0.0625),
                            _ => (0.0, 0.9375, 1.0, 1.0),
                        };
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + x0, by, bz + z0],
                            [bx + x1, by + 1.0, bz + z1],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::WallSign | BlockShape::SignPost => {
                        let placement = crate::sign_text::SignPlacement::from_block(
                            glam::IVec3::new(
                                snap.center.pos.cx() * 16 + lx as i32,
                                wy,
                                snap.center.pos.cz() * 16 + lz as i32,
                            ),
                            state,
                            meta,
                        );
                        let bucket = &mut out.opaque;
                        let light = snap.get_light(lx as i32, wy, lz as i32).max(10);
                        let board_col = shaded_color(meta, Face::Up, light, 255);
                        let uv = TextureAtlas::tile_uv(meta.face_tiles[Face::North.index()]);
                        placement.emit_board_box(bucket, uv, board_col);
                        if !placement.is_wall && !placement.is_hanging {
                            emit_box_all_faces(
                                snap,
                                lx as i32,
                                wy,
                                lz as i32,
                                [bx + 0.46, by, bz + 0.46],
                                [bx + 0.54, by + 0.58, bz + 0.54],
                                meta,
                                bucket,
                                255,
                                None,
                            );
                        }
                    }
                    BlockShape::Button | BlockShape::Lever => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.35, by, bz + 0.35],
                            [bx + 0.65, by + 0.6, bz + 0.65],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::PistonBody | BlockShape::PistonHead => {
                        let bucket = &mut out.alpha_cutout;
                        emit_box_all_faces(
                            snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + 1.0, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Air => {}
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_box_all_faces(
    snap: &ChunkNeighborSnapshot,
    lx: i32,
    wy: i32,
    lz: i32,
    min: [f32; 3],
    max: [f32; 3],
    meta: &BlockMeta,
    bucket: &mut MeshBucket,
    alpha: u8,
    cull_same_liquid: Option<(BlockStateId, &BlockRegistry)>,
) {
    let center_light = snap.get_light(lx, wy, lz);
    for face in Face::ALL {
        if let Some((self_id, reg)) = cull_same_liquid {
            let n = face.normal();
            let nb = snap.get_block(lx + n.x, wy + n.y, lz + n.z);
            if nb == self_id || (nb != BlockStateId::AIR && reg.get_meta(nb).is_full_opaque_cube) {
                continue;
            }
        }
        let color = shaded_color(meta, face, center_light.max(10), alpha);
        let uv = TextureAtlas::tile_uv(meta.face_tiles[face.index()]);
        let corners = unit_face_corners(face, min, max);
        bucket.push_quad(corners, uv, color);
    }
}

pub(crate) fn is_ore(reg: &BlockRegistry, id: BlockStateId) -> bool {
    let name = &reg.get_state(id).name;
    name.ends_with("_ore")
}

#[inline]
pub(crate) fn shaded_color(meta: &BlockMeta, face: Face, light: u8, alpha: u8) -> [u8; 4] {
    let dir_shade = match face {
        Face::Up => 1.0,
        Face::North | Face::South => 0.80,
        Face::East | Face::West => 0.68,
        Face::Down => 0.50,
    };
    let light_scale = (light.clamp(4, 15) as f32) / 15.0;
    let tint = if should_tint_face(meta, face) {
        meta.tint.rgb_multiplier()
    } else {
        [1.0, 1.0, 1.0]
    };
    let f = dir_shade * light_scale;
    [
        (tint[0] * f * 255.0).clamp(0.0, 255.0) as u8,
        (tint[1] * f * 255.0).clamp(0.0, 255.0) as u8,
        (tint[2] * f * 255.0).clamp(0.0, 255.0) as u8,
        alpha,
    ]
}

#[inline]
pub(crate) fn unit_face_corners(face: Face, min: [f32; 3], max: [f32; 3]) -> [[f32; 3]; 4] {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    // CCW winding viewed from outside face, matching UV [TL, TR, BR, BL]
    match face {
        Face::East => [[x1, y1, z1], [x1, y1, z0], [x1, y0, z0], [x1, y0, z1]],
        Face::West => [[x0, y1, z0], [x0, y1, z1], [x0, y0, z1], [x0, y0, z0]],
        Face::Up => [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
        Face::Down => [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
        Face::South => [[x0, y1, z1], [x1, y1, z1], [x1, y0, z1], [x0, y0, z1]],
        Face::North => [[x1, y1, z0], [x0, y1, z0], [x0, y0, z0], [x1, y0, z0]],
    }
}

/// Compact 1-bit-per-voxel 3D occupancy grid for an 8x8-chunk LOD tile.
/// Enables instant, zero-disk-I/O 3D raycasting up to 128+ chunks away.
#[derive(Debug, Clone)]
pub struct LodTileOccupancy {
    pub rx8: i32,
    pub rz8: i32,
    pub step: u8,
    pub min_y: i32,
    pub xz_cells: usize,
    pub y_cells: usize,
    bits: Vec<u64>,
}

impl LodTileOccupancy {
    pub fn new_empty(rx8: i32, rz8: i32, step: u8, min_y: i32, y_cells: usize) -> Self {
        let step = if step <= 2 { 2u8 } else { 4u8 };
        let xz_cells = 128 / (step as usize);
        let total_bits = y_cells * xz_cells * xz_cells;
        Self {
            rx8,
            rz8,
            step,
            min_y,
            xz_cells,
            y_cells,
            bits: vec![0u64; total_bits.div_ceil(64)],
        }
    }

    #[inline]
    fn set_bit(&mut self, cx: usize, cy: usize, cz: usize) {
        let idx = (cy * self.xz_cells + cz) * self.xz_cells + cx;
        let word = idx >> 6;
        if let Some(slot) = self.bits.get_mut(word) {
            *slot |= 1u64 << (idx & 63);
        }
    }

    #[inline]
    pub(crate) fn set_bit_public(&mut self, cx: usize, cy: usize, cz: usize) {
        self.set_bit(cx, cy, cz);
    }

    #[inline]
    pub fn is_occupied(&self, cx: usize, cy: usize, cz: usize) -> bool {
        if cx >= self.xz_cells || cz >= self.xz_cells || cy >= self.y_cells {
            return false;
        }
        let idx = (cy * self.xz_cells + cz) * self.xz_cells + cx;
        let word = idx >> 6;
        self.bits
            .get(word)
            .is_some_and(|&w| ((w >> (idx & 63)) & 1) != 0)
    }

    #[inline]
    pub fn is_world_occupied(&self, wx: i32, wy: i32, wz: i32) -> bool {
        let rx = wx - self.rx8 * 128;
        let rz = wz - self.rz8 * 128;
        let ry = wy - self.min_y;
        let s = self.step as i32;
        if !(0..128).contains(&rx) || !(0..128).contains(&rz) || ry < 0 {
            return false;
        }
        let cy = (ry / s) as usize;
        if cy >= self.y_cells {
            return false;
        }
        self.is_occupied((rx / s) as usize, cy, (rz / s) as usize)
    }

    pub fn top_occupied_world_y(&self, wx: i32, wz: i32) -> Option<i32> {
        let rx = wx - self.rx8 * 128;
        let rz = wz - self.rz8 * 128;
        let s = self.step as i32;
        if !(0..128).contains(&rx) || !(0..128).contains(&rz) || self.y_cells == 0 {
            return None;
        }
        let cx = (rx / s) as usize;
        let cz = (rz / s) as usize;
        for cy in (0..self.y_cells).rev() {
            if self.is_occupied(cx, cy, cz) {
                return Some(self.min_y + (cy as i32) * s + s - 1);
            }
        }
        None
    }
}

/// True 3D 6-face greedy-meshed LOD builder for an 8x8-chunk super-region (`128x256x128` blocks).
///
/// - `step`: `2` (LOD 1 mid-range, `64xHx64` grid) or `4` (LOD 2 far horizon, `32xHx32` grid).
/// - Emits all 6 exposed faces (`Up`, `Down`, `East`, `West`, `South`, `North`) so floating blocks
///   stay floating, overhangs/caves stay open, and bedrock/stone undersides & chunk edges remain solid.
/// - Samples exact per-face colors from `palette` (`TextureAtlas` avg RGB * tint).
pub fn mesh_lod_voxel_tile(
    rx8: i32,
    rz8: i32,
    chunks: &[LodChunkVoxels],
    step: u8,
    palette: &LodColorTable,
    min_y: i32,
) -> (SectionMeshData, LodTileOccupancy) {
    let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::LodVoxelMesh);
    let step = if step <= 2 { 2u8 } else { 4u8 };
    let s = step as usize;
    let chunk_xz = 16 / s;
    let grid_xz = 128 / s;

    let max_y_cells = chunks.iter().map(|c| c.populated_y_cells).max().unwrap_or(0);
    let mut occ = LodTileOccupancy::new_empty(rx8, rz8, step, min_y, max_y_cells);
    let mut out = SectionMeshData::default();
    if max_y_cells == 0 || chunks.is_empty() {
        return (out, occ);
    }

    let mut grid = vec![BlockStateId::AIR; max_y_cells * grid_xz * grid_xz];
    let origin_cx = rx8 * 8;
    let origin_cz = rz8 * 8;

    for chunk in chunks {
        let rel_cx = chunk.pos.cx() - origin_cx;
        let rel_cz = chunk.pos.cz() - origin_cz;
        if !(0..8).contains(&rel_cx) || !(0..8).contains(&rel_cz) || chunk.xz_cells != chunk_xz {
            continue;
        }
        let gx_base = (rel_cx as usize) * chunk_xz;
        let gz_base = (rel_cz as usize) * chunk_xz;
        let cy_limit = chunk.populated_y_cells.min(max_y_cells);
        for cy in 0..cy_limit {
            for cz in 0..chunk_xz {
                let gz = gz_base + cz;
                let dst_row = (cy * grid_xz + gz) * grid_xz + gx_base;
                let src_row = (cy * chunk_xz + cz) * chunk_xz;
                for cx in 0..chunk_xz {
                    let b = chunk.cells[src_row + cx];
                    if b != BlockStateId::AIR {
                        grid[dst_row + cx] = b;
                        occ.set_bit(gx_base + cx, cy, gz);
                    }
                }
            }
        }
    }

    let base_wx = (rx8 * 128) as f32;
    let base_wz = (rz8 * 128) as f32;
    let base_wy = min_y as f32;
    let cell = s as f32;
    let quantize = step >= 4;

    let get_cell = |gx: isize, cy: isize, gz: isize| -> BlockStateId {
        if gx < 0
            || gx >= grid_xz as isize
            || cz_out(gz, grid_xz)
            || cy < 0
            || cy >= max_y_cells as isize
        {
            BlockStateId::AIR
        } else {
            grid[((cy as usize) * grid_xz + (gz as usize)) * grid_xz + (gx as usize)]
        }
    };

    let face_entry = |id: BlockStateId, nb: BlockStateId, face: Face, shade: f32| -> Option<([u8; 4], bool)> {
        if id == BlockStateId::AIR {
            return None;
        }
        let vis = palette.get(id);
        if nb != BlockStateId::AIR {
            let nvis = palette.get(nb);
            if vis.is_opaque && nvis.is_opaque {
                return None;
            }
            if !vis.is_opaque && (nvis.is_opaque || nb == id) {
                return None;
            }
        }
        let mut col = scale_rgb(vis.face_colors[face.index()], shade);
        if quantize {
            col[0] &= 0xF8;
            col[1] &= 0xF8;
            col[2] &= 0xF8;
        }
        Some((col, vis.is_translucent))
    };

    // 1. Up (+Y) and Down (-Y): slice along cy, 2D greedy merge over (gx, gz)
    let mut mask_xz: Vec<Option<([u8; 4], bool)>> = vec![None; grid_xz * grid_xz];
    for (face, dy, shade) in [(Face::Up, 1isize, 1.0f32), (Face::Down, -1isize, 0.50f32)] {
        for cy in 0..max_y_cells {
            let mut any = false;
            for gz in 0..grid_xz {
                let row_grid = (cy * grid_xz + gz) * grid_xz;
                let row_mask = gz * grid_xz;
                for gx in 0..grid_xz {
                    let id = grid[row_grid + gx];
                    let entry = if id != BlockStateId::AIR {
                        let nb = get_cell(gx as isize, cy as isize + dy, gz as isize);
                        face_entry(id, nb, face, shade)
                    } else {
                        None
                    };
                    if entry.is_some() {
                        any = true;
                    }
                    mask_xz[row_mask + gx] = entry;
                }
            }
            if !any {
                continue;
            }
            greedy_merge_2d(&mut mask_xz, grid_xz, grid_xz, |u0, v0, w, h, col, is_trans| {
                let x0 = base_wx + (u0 as f32) * cell;
                let x1 = base_wx + ((u0 + w) as f32) * cell;
                let z0 = base_wz + (v0 as f32) * cell;
                let z1 = base_wz + ((v0 + h) as f32) * cell;
                let y0 = base_wy + (cy as f32) * cell;
                let y1 = y0 + cell;
                let bucket = if is_trans { &mut out.translucent } else { &mut out.opaque };
                bucket.push_quad(
                    unit_face_corners(face, [x0, y0, z0], [x1, y1, z1]),
                    [0.0, 0.0, 0.0, 0.0],
                    col,
                );
            });
        }
    }

    // 2. East (+X) and West (-X): slice along gx, 2D greedy merge over (u = gz, v = cy)
    let mut mask_zy: Vec<Option<([u8; 4], bool)>> = vec![None; grid_xz * max_y_cells];
    for (face, dx, shade, nudge) in [
        (Face::East, 1isize, 0.68f32, [-0.05f32, 0.0f32]),
        (Face::West, -1isize, 0.68f32, [0.05f32, 0.0f32]),
    ] {
        for gx in 0..grid_xz {
            let mut any = false;
            for cy in 0..max_y_cells {
                let row_mask = cy * grid_xz;
                for gz in 0..grid_xz {
                    let id = grid[(cy * grid_xz + gz) * grid_xz + gx];
                    let entry = if id != BlockStateId::AIR {
                        let nb = get_cell(gx as isize + dx, cy as isize, gz as isize);
                        face_entry(id, nb, face, shade)
                    } else {
                        None
                    };
                    if entry.is_some() {
                        any = true;
                    }
                    mask_zy[row_mask + gz] = entry;
                }
            }
            if !any {
                continue;
            }
            greedy_merge_2d(&mut mask_zy, grid_xz, max_y_cells, |u0, v0, w, h, col, is_trans| {
                let x0 = base_wx + (gx as f32) * cell;
                let x1 = x0 + cell;
                let z0 = base_wz + (u0 as f32) * cell;
                let z1 = base_wz + ((u0 + w) as f32) * cell;
                let y0 = base_wy + (v0 as f32) * cell;
                let y1 = base_wy + ((v0 + h) as f32) * cell;
                let bucket = if is_trans { &mut out.translucent } else { &mut out.opaque };
                bucket.push_quad(
                    unit_face_corners(face, [x0, y0, z0], [x1, y1, z1]),
                    [nudge[0], nudge[1], nudge[0], nudge[1]],
                    col,
                );
            });
        }
    }

    // 3. South (+Z) and North (-Z): slice along gz, 2D greedy merge over (u = gx, v = cy)
    let mut mask_xy: Vec<Option<([u8; 4], bool)>> = vec![None; grid_xz * max_y_cells];
    for (face, dz, shade, nudge) in [
        (Face::South, 1isize, 0.80f32, [0.0f32, -0.05f32]),
        (Face::North, -1isize, 0.80f32, [0.0f32, 0.05f32]),
    ] {
        for gz in 0..grid_xz {
            let mut any = false;
            for cy in 0..max_y_cells {
                let row_mask = cy * grid_xz;
                let row_grid = (cy * grid_xz + gz) * grid_xz;
                for gx in 0..grid_xz {
                    let id = grid[row_grid + gx];
                    let entry = if id != BlockStateId::AIR {
                        let nb = get_cell(gx as isize, cy as isize, gz as isize + dz);
                        face_entry(id, nb, face, shade)
                    } else {
                        None
                    };
                    if entry.is_some() {
                        any = true;
                    }
                    mask_xy[row_mask + gx] = entry;
                }
            }
            if !any {
                continue;
            }
            greedy_merge_2d(&mut mask_xy, grid_xz, max_y_cells, |u0, v0, w, h, col, is_trans| {
                let x0 = base_wx + (u0 as f32) * cell;
                let x1 = base_wx + ((u0 + w) as f32) * cell;
                let y0 = base_wy + (v0 as f32) * cell;
                let y1 = base_wy + ((v0 + h) as f32) * cell;
                let z0 = base_wz + (gz as f32) * cell;
                let z1 = z0 + cell;
                let bucket = if is_trans { &mut out.translucent } else { &mut out.opaque };
                bucket.push_quad(
                    unit_face_corners(face, [x0, y0, z0], [x1, y1, z1]),
                    [nudge[0], nudge[1], nudge[0], nudge[1]],
                    col,
                );
            });
        }
    }

    (out, occ)
}

#[inline]
fn cz_out(gz: isize, grid_xz: usize) -> bool {
    gz < 0 || gz >= grid_xz as isize
}

pub(crate) fn greedy_merge_2d<F>(
    mask: &mut [Option<([u8; 4], bool)>],
    u_len: usize,
    v_len: usize,
    mut emit: F,
) where
    F: FnMut(usize, usize, usize, usize, [u8; 4], bool),
{
    for v in 0..v_len {
        let mut u = 0usize;
        while u < u_len {
            let idx = v * u_len + u;
            let Some((col, is_trans)) = mask[idx] else {
                u += 1;
                continue;
            };
            let mut w = 1usize;
            while u + w < u_len && mask[v * u_len + u + w] == Some((col, is_trans)) {
                w += 1;
            }
            let mut h = 1usize;
            'rows: while v + h < v_len {
                let row = (v + h) * u_len + u;
                for du in 0..w {
                    if mask[row + du] != Some((col, is_trans)) {
                        break 'rows;
                    }
                }
                h += 1;
            }
            for dv in 0..h {
                let row = (v + dv) * u_len + u;
                for du in 0..w {
                    mask[row + du] = None;
                }
            }
            emit(u, v, w, h, col, is_trans);
            u += w;
        }
    }
}

/// Builds a greedy-merged flat elevation-shaded overhead mesh for 2D Chunk View (`8x8` chunks)
/// using exact `LodColorTable` atlas top-face colors.
pub fn mesh_lod_overhead_tile(
    rx8: i32,
    rz8: i32,
    surfaces: &[LodChunkSurface],
    step: usize,
    palette: &LodColorTable,
    min_y: i32,
    max_y: i32,
) -> SectionMeshData {
    let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::LodSurfaceMesh);
    let mut out = SectionMeshData::default();
    if surfaces.is_empty() {
        return out;
    }

    let step = match step {
        1 => 1usize,
        2 => 2usize,
        _ => 4usize,
    };
    let grid_n = 128 / step;
    let sub_n = 16 / step;
    let mut heights = vec![i32::MIN; grid_n * grid_n];
    let mut colors = vec![[0u8; 4]; grid_n * grid_n];

    let origin_cx = rx8 * 8;
    let origin_cz = rz8 * 8;

    for surf in surfaces {
        let rel_cx = surf.pos.cx() - origin_cx;
        let rel_cz = surf.pos.cz() - origin_cz;
        if !(0..8).contains(&rel_cx) || !(0..8).contains(&rel_cz) {
            continue;
        }
        let gx_base = (rel_cx as usize) * sub_n;
        let gz_base = (rel_cz as usize) * sub_n;

        if step == 1 {
            for lz in 0..16 {
                let dst_row = (gz_base + lz) * grid_n + gx_base;
                let src_row = lz << 4;
                for lx in 0..16 {
                    let h = surf.heights[src_row + lx];
                    heights[dst_row + lx] = h;
                    if h != i32::MIN {
                        let b = surf.blocks[src_row + lx];
                        let col = if b != BlockStateId::AIR {
                            let c = palette.get(b).face_colors[Face::Up.index()];
                            [c[0], c[1], c[2], 255]
                        } else {
                            surf.colors[src_row + lx]
                        };
                        colors[dst_row + lx] = col;
                    }
                }
            }
        } else {
            for sz in 0..sub_n {
                for sx in 0..sub_n {
                    let mut best_h = i32::MIN;
                    for dz in 0..step {
                        let lz = sz * step + dz;
                        for dx in 0..step {
                            let lx = sx * step + dx;
                            let h = surf.heights[(lz << 4) | lx];
                            if h > best_h {
                                best_h = h;
                            }
                        }
                    }
                    if best_h == i32::MIN {
                        continue;
                    }
                    let mut r_sum = 0u32;
                    let mut g_sum = 0u32;
                    let mut b_sum = 0u32;
                    let mut cnt = 0u32;
                    for dz in 0..step {
                        let lz = sz * step + dz;
                        for dx in 0..step {
                            let lx = sx * step + dx;
                            let idx = (lz << 4) | lx;
                            if surf.heights[idx] >= best_h.saturating_sub(2) {
                                let b = surf.blocks[idx];
                                let c = if b != BlockStateId::AIR {
                                    palette.get(b).face_colors[Face::Up.index()]
                                } else {
                                    surf.colors[idx]
                                };
                                r_sum += c[0] as u32;
                                g_sum += c[1] as u32;
                                b_sum += c[2] as u32;
                                cnt += 1;
                            }
                        }
                    }
                    let gidx = (gz_base + sz) * grid_n + (gx_base + sx);
                    heights[gidx] = best_h;
                    let denom = cnt.max(1);
                    colors[gidx] = [
                        ((r_sum / denom) as u8) & 0xF8,
                        ((g_sum / denom) as u8) & 0xF8,
                        ((b_sum / denom) as u8) & 0xF8,
                        255,
                    ];
                }
            }
        }
    }

    let base_wx = (rx8 * 128) as f32;
    let base_wz = (rz8 * 128) as f32;
    let cell = step as f32;
    let total_h = (max_y - min_y).max(1) as f32;

    let mut visited = vec![false; grid_n * grid_n];
    for gz in 0..grid_n {
        for gx in 0..grid_n {
            let idx = gz * grid_n + gx;
            if visited[idx] || heights[idx] == i32::MIN {
                continue;
            }
            let h = heights[idx];
            let c = colors[idx];

            let mut w = 1usize;
            while gx + w < grid_n {
                let nidx = gz * grid_n + (gx + w);
                if visited[nidx] || heights[nidx] != h || colors[nidx] != c {
                    break;
                }
                w += 1;
            }
            let mut d = 1usize;
            'depth: while gz + d < grid_n {
                let row_off = (gz + d) * grid_n + gx;
                for ox in 0..w {
                    let nidx = row_off + ox;
                    if visited[nidx] || heights[nidx] != h || colors[nidx] != c {
                        break 'depth;
                    }
                }
                d += 1;
            }
            for oz in 0..d {
                let row_off = (gz + oz) * grid_n + gx;
                for ox in 0..w {
                    visited[row_off + ox] = true;
                }
            }

            let shade = 0.55 + ((h - min_y) as f32 / total_h).clamp(0.0, 1.0) * 0.45;
            let top_col = scale_rgb(c, shade);
            let x0 = base_wx + (gx as f32) * cell;
            let x1 = base_wx + ((gx + w) as f32) * cell;
            let z0 = base_wz + (gz as f32) * cell;
            let z1 = base_wz + ((gz + d) as f32) * cell;
            let y1 = (h + 1) as f32;

            out.opaque.push_quad(
                unit_face_corners(Face::Up, [x0, y1 - 1.0, z0], [x1, y1, z1]),
                [0.0, 0.0, 0.0, 0.0],
                top_col,
            );
        }
    }

    out
}

#[inline]
pub(crate) fn scale_rgb(c: [u8; 4], factor: f32) -> [u8; 4] {
    [
        (c[0] as f32 * factor).clamp(0.0, 255.0) as u8,
        (c[1] as f32 * factor).clamp(0.0, 255.0) as u8,
        (c[2] as f32 * factor).clamp(0.0, 255.0) as u8,
        c[3],
    ]
}

/// Top-down elevation-shaded mesh for LOD 2 (2D Chunk View) and LOD 1 (Distant chunks).
pub fn mesh_chunk_overhead(chunk: &Chunk, reg: &BlockRegistry, flat_lod1: bool) -> SectionMeshData {
    let mut out = SectionMeshData::default();
    let cx_off = (chunk.pos.cx() * 16) as f32;
    let cz_off = (chunk.pos.cz() * 16) as f32;
    let min_y = chunk.min_y();
    let max_y = chunk.max_y();
    let total_h = (max_y - min_y).max(1) as f32;

    for lz in 0..16 {
        for lx in 0..16 {
            let hm = chunk.heightmap[(lz << 4) | lx].clamp(min_y, max_y);
            let mut found = BlockStateId::AIR;
            let mut found_y = min_y;
            for y in (min_y..=hm.min(max_y - 1)).rev() {
                let b = chunk.get_block(lx, y, lz);
                if b != BlockStateId::AIR {
                    found = b;
                    found_y = y;
                    break;
                }
            }
            if found == BlockStateId::AIR {
                continue;
            }
            let meta = reg.get_meta(found);
            let uv = TextureAtlas::tile_uv(meta.face_tiles[Face::Up.index()]);
            let shade = if flat_lod1 {
                1.0
            } else {
                0.5 + ((found_y - min_y) as f32 / total_h) * 0.5
            };
            let tint = meta.tint.rgb_multiplier();
            let col = [
                (tint[0] * shade * 255.0).clamp(0.0, 255.0) as u8,
                (tint[1] * shade * 255.0).clamp(0.0, 255.0) as u8,
                (tint[2] * shade * 255.0).clamp(0.0, 255.0) as u8,
                255,
            ];
            let bx = cx_off + lx as f32;
            let by = found_y as f32 + 1.0;
            let bz = cz_off + lz as f32;
            out.opaque.push_quad(
                unit_face_corners(Face::Up, [bx, by - 1.0, bz], [bx + 1.0, by, bz + 1.0]),
                uv,
                col,
            );
        }
    }
    out
}

/// Builds a translucent 3D ghost mesh for Clone / Import / Brush previews.
pub fn mesh_volume_preview(
    vol: &SchematicVolume,
    reg: &BlockRegistry,
    alpha: u8,
) -> SectionMeshData {
    let mut out = SectionMeshData::default();
    let w = vol.size.x;
    let h = vol.size.y;
    let l = vol.size.z;

    for y in 0..h {
        for z in 0..l {
            for x in 0..w {
                let id = vol.get_block(x, y, z);
                if id == BlockStateId::AIR {
                    continue;
                }
                let meta = reg.get_meta(id);
                if meta.is_air {
                    continue;
                }
                let bx = x as f32;
                let by = y as f32;
                let bz = z as f32;
                for face in Face::ALL {
                    let n = face.normal();
                    let nx = x as i32 + n.x;
                    let ny = y as i32 + n.y;
                    let nz = z as i32 + n.z;
                    if nx >= 0
                        && nx < w as i32
                        && ny >= 0
                        && ny < h as i32
                        && nz >= 0
                        && nz < l as i32
                    {
                        let nb = vol.get_block(nx as u32, ny as u32, nz as u32);
                        if nb != BlockStateId::AIR && reg.get_meta(nb).is_full_opaque_cube {
                            continue;
                        }
                    }
                    let color = shaded_color(meta, face, 15, alpha);
                    let uv = TextureAtlas::tile_uv(meta.face_tiles[face.index()]);
                    out.translucent.push_quad(
                        unit_face_corners(face, [bx, by, bz], [bx + 1.0, by + 1.0, bz + 1.0]),
                        uv,
                        color,
                    );
                }
            }
        }
    }

    out
}

pub fn build_chunk_snapshots(
    chunks: &BTreeMap<(i32, i32), Chunk>,
    targets: &[(i32, i32)],
) -> Vec<ChunkNeighborSnapshot> {
    targets
        .iter()
        .filter_map(|&(cx, cz)| {
            let center = Arc::new(chunks.get(&(cx, cz))?.clone());
            Some(ChunkNeighborSnapshot {
                center,
                east: chunks.get(&(cx + 1, cz)).cloned().map(Arc::new),
                west: chunks.get(&(cx - 1, cz)).cloned().map(Arc::new),
                south: chunks.get(&(cx, cz + 1)).cloned().map(Arc::new),
                north: chunks.get(&(cx, cz - 1)).cloned().map(Arc::new),
            })
        })
        .collect()
}
