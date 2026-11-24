use crate::block::{BlockMeta, BlockRegistry, BlockShape, BlockStateId};
use crate::chunk::Chunk;
use crate::math::ChunkPos;
use crate::world::{DimensionId, PlayerData, SchematicVolume, WorldMetadata};
use mcedit_nbt::NbtError;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("NBT error: {0}")]
    Nbt(#[from] NbtError),
    #[error("unsupported world or schematic format at {0}")]
    UnsupportedFormat(String),
    #[error("unsupported world version: {0}")]
    UnsupportedVersion(String),
    #[error("corrupt data: {0}")]
    Corrupt(String),
    #[error("session lock lost or held by another process")]
    SessionLockLost,
}

pub type FormatResult<T> = std::result::Result<T, FormatError>;

#[derive(Debug, Clone, Default)]
pub struct RepairReport {
    pub files_scanned: usize,
    pub files_repaired: usize,
    pub chunks_recovered: usize,
    pub chunks_lost: usize,
}

/// Compact 16x16 top-surface summary for fast 2D Chunk View overhead rendering.
#[derive(Debug, Clone)]
pub struct LodChunkSurface {
    pub pos: ChunkPos,
    /// Top non-air block Y per `(z << 4) | x`; `i32::MIN` if column is empty.
    pub heights: [i32; 256],
    /// Top non-air `BlockStateId` per `(z << 4) | x` (for exact atlas color lookup).
    pub blocks: [BlockStateId; 256],
    /// Fallback map color per `(z << 4) | x`.
    pub colors: [[u8; 4]; 256],
}

impl LodChunkSurface {
    pub fn from_chunk(chunk: &Chunk, reg: &BlockRegistry) -> Self {
        let mut heights = [i32::MIN; 256];
        let mut blocks = [BlockStateId::AIR; 256];
        let mut colors = [[0u8; 4]; 256];
        let mut remaining = 256usize;
        let hide_barriers = crate::hide_barriers();

        for (&sy, sec) in chunk.sections.iter().rev() {
            if sec.is_all_air() {
                continue;
            }
            let base_y = sy * 16;
            for ly in (0..16).rev() {
                let y_off = ly << 8;
                let wy = base_y + ly as i32;
                for lz in 0..16 {
                    let yz_off = y_off | (lz << 4);
                    let row = lz << 4;
                    for lx in 0..16 {
                        let idx = row | lx;
                        if heights[idx] != i32::MIN {
                            continue;
                        }
                        let pidx = sec.indices[yz_off | lx] as usize;
                        if pidx == 0 {
                            continue;
                        }
                        let b = sec.palette.get(pidx).copied().unwrap_or(BlockStateId::AIR);
                        if b != BlockStateId::AIR {
                            let meta = reg.get_meta(b);
                            if !meta.is_air && (!hide_barriers || !meta.is_barrier) {
                                heights[idx] = wy;
                                blocks[idx] = b;
                                colors[idx] = tinted_map_color(meta);
                                remaining -= 1;
                                if remaining == 0 {
                                    return Self {
                                        pos: chunk.pos,
                                        heights,
                                        blocks,
                                        colors,
                                    };
                                }
                            }
                        }
                    }
                }
            }
        }
        Self {
            pos: chunk.pos,
            heights,
            blocks,
            colors,
        }
    }
}

/// True 3D downsampled voxel representation of a chunk for distant LOD meshing.
/// Preserves floating structures, caves, undersides, and stone/bedrock edges without 2.5D pillars.
#[derive(Debug, Clone)]
pub struct LodChunkVoxels {
    pub pos: ChunkPos,
    /// Downsampling factor in blocks per voxel (`2` or `4`).
    pub step: u8,
    pub min_y: i32,
    pub xz_cells: usize,
    pub y_cells: usize,
    /// Flat array indexed by `(cy * xz_cells + cz) * xz_cells + cx`.
    pub cells: Vec<BlockStateId>,
    /// Highest non-empty `cy + 1` (0 if chunk is completely empty).
    pub populated_y_cells: usize,
}

impl LodChunkVoxels {
    pub fn new_empty(pos: ChunkPos, step: u8, min_y: i32, max_y: i32) -> Self {
        let step = if step <= 2 { 2u8 } else { 4u8 };
        let s = step as usize;
        let xz_cells = 16 / s;
        let y_cells = ((max_y - min_y).max(0) as usize).div_ceil(s);
        Self {
            pos,
            step,
            min_y,
            xz_cells,
            y_cells,
            cells: vec![BlockStateId::AIR; y_cells * xz_cells * xz_cells],
            populated_y_cells: 0,
        }
    }

    #[inline]
    pub fn cell_index(&self, cx: usize, cy: usize, cz: usize) -> usize {
        (cy * self.xz_cells + cz) * self.xz_cells + cx
    }

    #[inline]
    pub fn set_cell(&mut self, cx: usize, cy: usize, cz: usize, id: BlockStateId) {
        if cx < self.xz_cells && cz < self.xz_cells && cy < self.y_cells {
            let idx = self.cell_index(cx, cy, cz);
            self.cells[idx] = id;
            if id != BlockStateId::AIR && cy + 1 > self.populated_y_cells {
                self.populated_y_cells = cy + 1;
            }
        }
    }

    #[inline]
    pub fn get_cell(&self, cx: usize, cy: usize, cz: usize) -> BlockStateId {
        if cx < self.xz_cells && cz < self.xz_cells && cy < self.y_cells {
            self.cells[self.cell_index(cx, cy, cz)]
        } else {
            BlockStateId::AIR
        }
    }

    /// Downsamples an in-memory `Chunk` into a true 3D voxel grid at `step` (`2` or `4`).
    pub fn from_chunk(chunk: &Chunk, step: u8, reg: &BlockRegistry) -> Self {
        let step = if step <= 2 { 2u8 } else { 4u8 };
        let s = step as usize;
        let min_y = chunk.min_y();
        let max_y = chunk.max_y();
        let mut out = Self::new_empty(chunk.pos, step, min_y, max_y);
        let sub_y_per_sec = 16 / s;
        let xz_cells = out.xz_cells;
        let hide_barriers = crate::hide_barriers();

        for (&sy, sec) in &chunk.sections {
            if sec.is_all_air() {
                continue;
            }
            let sec_base_y = sy * 16;
            if sec_base_y + 16 <= min_y || sec_base_y >= max_y {
                continue;
            }
            let base_cy = ((sec_base_y - min_y) / (step as i32)) as usize;
            for scy in 0..sub_y_per_sec {
                let cy = base_cy + scy;
                if cy >= out.y_cells {
                    continue;
                }
                let ly0 = scy * s;
                for cz in 0..xz_cells {
                    let lz0 = cz * s;
                    for cx in 0..xz_cells {
                        let lx0 = cx * s;
                        let mut chosen = BlockStateId::AIR;
                        let mut best_other = BlockStateId::AIR;
                        let mut best_other_prio = 0u8;

                        // Scan top-down layer by layer within the sub-cube so surface blocks
                        // (e.g. grass over dirt, OR water at Y=62 over a riverbed/seabed at Y=60..61)
                        // win over blocks directly beneath them in the same downsampled cell.
                        for dy in (0..s).rev() {
                            let ly = ly0 + dy;
                            let mut layer_opaque = BlockStateId::AIR;
                            let mut layer_opaque_count = 0u16;
                            let mut layer_liquid = BlockStateId::AIR;
                            let mut layer_liquid_count = 0u16;
                            for dz in 0..s {
                                let lz = lz0 + dz;
                                for dx in 0..s {
                                    let b = sec.get_block(lx0 + dx, ly, lz);
                                    if b == BlockStateId::AIR {
                                        continue;
                                    }
                                    let meta = reg.get_meta(b);
                                    if meta.is_air || (hide_barriers && meta.is_barrier) {
                                        continue;
                                    }
                                    if meta.is_full_opaque_cube {
                                        if layer_opaque == BlockStateId::AIR {
                                            layer_opaque = b;
                                        }
                                        layer_opaque_count += 1;
                                    } else if meta.is_liquid {
                                        if layer_liquid == BlockStateId::AIR {
                                            layer_liquid = b;
                                        }
                                        layer_liquid_count += 1;
                                    } else {
                                        let prio = lod_non_opaque_priority(meta);
                                        if prio > best_other_prio {
                                            best_other = b;
                                            best_other_prio = prio;
                                        }
                                    }
                                }
                            }
                            if layer_opaque_count > 0 || layer_liquid_count > 0 {
                                chosen = if layer_liquid_count > layer_opaque_count {
                                    layer_liquid
                                } else {
                                    layer_opaque
                                };
                                break;
                            }
                        }

                        if chosen == BlockStateId::AIR {
                            chosen = best_other;
                        }
                        if chosen != BlockStateId::AIR {
                            let vis_id = reg
                                .get_meta(chosen)
                                .visual_base_id
                                .unwrap_or(chosen);
                            out.set_cell(cx, cy, cz, vis_id);
                        }
                    }
                }
            }
        }
        out
    }

    /// Like `from_chunk`, but preserves the exact interned `BlockStateId` (without collapsing to
    /// `visual_base_id`) so LOD terrain comparison distinguishes modern block variants accurately.
    pub fn from_chunk_exact(chunk: &Chunk, step: u8, reg: &BlockRegistry) -> Self {
        let step = if step <= 2 { 2u8 } else { 4u8 };
        let s = step as usize;
        let min_y = chunk.min_y();
        let max_y = chunk.max_y();
        let mut out = Self::new_empty(chunk.pos, step, min_y, max_y);
        let sub_y_per_sec = 16 / s;
        let hide_barriers = crate::hide_barriers();

        for (&sy, sec) in &chunk.sections {
            if sec.is_all_air() {
                continue;
            }
            let base_wy = sy * 16;
            if base_wy < min_y || base_wy >= max_y {
                continue;
            }
            let base_cy = ((base_wy - min_y) as usize) / s;
            for lcy in 0..sub_y_per_sec {
                let cy = base_cy + lcy;
                let y0 = lcy * s;
                let y1 = y0 + s;
                for cz in 0..out.xz_cells {
                    let z0 = cz * s;
                    let z1 = z0 + s;
                    for cx in 0..out.xz_cells {
                        let x0 = cx * s;
                        let x1 = x0 + s;
                        let mut best_opaque = BlockStateId::AIR;
                        let mut best_opaque_y = 0usize;
                        let mut best_non_opaque = BlockStateId::AIR;
                        let mut best_non_opaque_prio = 0u8;
                        let mut best_non_opaque_y = 0usize;
                        for ly in (y0..y1).rev() {
                            let y_off = ly << 8;
                            for lz in z0..z1 {
                                let yz_off = y_off | (lz << 4);
                                for lx in x0..x1 {
                                    let pidx = sec.indices[yz_off | lx] as usize;
                                    if pidx == 0 {
                                        continue;
                                    }
                                    let b = sec
                                        .palette
                                        .get(pidx)
                                        .copied()
                                        .unwrap_or(BlockStateId::AIR);
                                    if b == BlockStateId::AIR {
                                        continue;
                                    }
                                    let meta = reg.get_meta(b);
                                    if meta.is_air || (hide_barriers && meta.is_barrier) {
                                        continue;
                                    }
                                    if meta.is_full_opaque_cube {
                                        if best_opaque == BlockStateId::AIR {
                                            best_opaque = b;
                                            best_opaque_y = ly;
                                        }
                                    } else {
                                        let prio = lod_non_opaque_priority(meta);
                                        if prio > best_non_opaque_prio
                                            || (prio == best_non_opaque_prio
                                                && ly > best_non_opaque_y)
                                        {
                                            best_non_opaque = b;
                                            best_non_opaque_prio = prio;
                                            best_non_opaque_y = ly;
                                        }
                                    }
                                }
                            }
                        }
                        let chosen = if best_opaque != BlockStateId::AIR {
                            if best_non_opaque != BlockStateId::AIR
                                && best_non_opaque_prio >= 4
                                && best_non_opaque_y > best_opaque_y
                            {
                                best_non_opaque
                            } else {
                                best_opaque
                            }
                        } else {
                            best_non_opaque
                        };
                        if chosen != BlockStateId::AIR {
                            out.set_cell(cx, cy, cz, chosen);
                        }
                    }
                }
            }
        }
        out
    }
}

#[inline]
pub fn lod_non_opaque_priority(meta: &BlockMeta) -> u8 {
    if meta.is_liquid {
        4
    } else if meta.shape == BlockShape::Leaves {
        3
    } else if matches!(meta.shape, BlockShape::Slab | BlockShape::Stairs | BlockShape::Glass)
        || meta.is_translucent
    {
        2
    } else {
        1
    }
}

#[inline]
pub fn tinted_map_color(meta: &BlockMeta) -> [u8; 4] {
    let tint = meta.tint.rgb_multiplier();
    let mc = meta.map_color;
    let use_tint = !matches!(meta.tint, crate::block::TintKind::None)
        && mc[0] == mc[1]
        && mc[1] == mc[2];
    if use_tint {
        [
            (mc[0] as f32 * tint[0]).clamp(0.0, 255.0) as u8,
            (mc[1] as f32 * tint[1]).clamp(0.0, 255.0) as u8,
            (mc[2] as f32 * tint[2]).clamp(0.0, 255.0) as u8,
            255,
        ]
    } else {
        [mc[0], mc[1], mc[2], 255]
    }
}

pub trait WorldFormat: Send + Sync {
    fn format_name(&self) -> &'static str;
    fn root_path(&self) -> &Path;
    fn min_section_y(&self) -> i32;
    fn max_section_y(&self) -> i32;
    fn dimensions(&self) -> Vec<DimensionId>;
    fn load_metadata(&self) -> FormatResult<WorldMetadata>;
    fn save_metadata(&self, meta: &WorldMetadata) -> FormatResult<()>;
    fn list_chunks(&self, dim: DimensionId) -> FormatResult<Vec<ChunkPos>>;
    /// Returns all `(rx, rz)` 32x32-chunk region files present on disk for `dim`
    /// (including empty region files containing zero chunks).
    fn list_regions(&self, dim: DimensionId) -> FormatResult<Vec<(i32, i32)>> {
        let mut set = ahash::AHashSet::new();
        for cp in self.list_chunks(dim)? {
            set.insert(cp.region_pos());
        }
        Ok(set.into_iter().collect())
    }
    fn load_chunk(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &mut BlockRegistry,
    ) -> FormatResult<Option<Chunk>>;
    fn load_chunk_readonly(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<Chunk>> {
        let mut tmp_reg = registry.clone();
        self.load_chunk(dim, pos, &mut tmp_reg)
    }
    /// Loads a chunk for read-only inspection/comparison while registering any unseen
    /// block states into the shared `BlockRegistry` (fast read-lock path, write-lock only
    /// on first encounter of a new state). Guarantees exact `BlockStateId` equality across worlds.
    fn load_chunk_with_registry(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &parking_lot::RwLock<BlockRegistry>,
    ) -> FormatResult<Option<Chunk>> {
        let reg = registry.read();
        self.load_chunk_readonly(dim, pos, &reg)
    }
    fn load_chunk_lod_surface(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<LodChunkSurface>> {
        Ok(self
            .load_chunk_readonly(dim, pos, registry)?
            .map(|c| LodChunkSurface::from_chunk(&c, registry)))
    }
    fn load_chunk_lod_voxels(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        step: u8,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<LodChunkVoxels>> {
        Ok(self
            .load_chunk_readonly(dim, pos, registry)?
            .map(|c| LodChunkVoxels::from_chunk(&c, step, registry)))
    }
    fn save_chunk(
        &self,
        dim: DimensionId,
        chunk: &Chunk,
        registry: &BlockRegistry,
    ) -> FormatResult<()>;
    fn delete_chunk(&self, dim: DimensionId, pos: ChunkPos) -> FormatResult<()>;
    fn load_players(&self) -> FormatResult<Vec<PlayerData>>;
    fn save_player(&self, player: &PlayerData) -> FormatResult<()>;
    fn delete_player(&self, id: &str) -> FormatResult<()>;
    fn acquire_session_lock(&self) -> FormatResult<()>;
    fn check_session_lock(&self) -> FormatResult<bool>;
    fn repair_regions(&self, dim: DimensionId) -> FormatResult<RepairReport>;
}

pub trait SchematicFormat: Send + Sync {
    fn format_name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn load(&self, path: &Path, registry: &mut BlockRegistry) -> FormatResult<SchematicVolume>;
    fn save(
        &self,
        path: &Path,
        volume: &SchematicVolume,
        registry: &BlockRegistry,
    ) -> FormatResult<()>;
}
