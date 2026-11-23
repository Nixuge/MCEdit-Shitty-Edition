use crate::camera::ViewMode;
use crate::mesher::LodTileOccupancy;
use ahash::AHashMap;
use glam::{IVec3, Vec3};
use mcedit_core::{BlockStateId, ChunkPos, DimensionId, Face, World};
use std::sync::Arc;

/// Index of loaded 8x8-chunk LOD 3D occupancy grids keyed by `(rx8, rz8)` for
/// unlimited-distance, zero-disk-I/O raycasting across distant LOD terrain.
pub type LodRaycastIndex = AHashMap<(i32, i32), Arc<LodTileOccupancy>>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RaycastHit {
    pub block_pos: IVec3,
    pub adjacent_pos: IVec3,
    pub face: Face,
    pub distance: f32,
}

/// CPU 3D Amanatides-Woo voxel DDA raycaster + Chunk2D column heightmap fallback.
/// Uses ONLY already-loaded chunks in memory so cursor hover never stalls on disk I/O.
pub fn raycast_world(
    world: &World,
    dim: DimensionId,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
    mode: ViewMode,
) -> Option<RaycastHit> {
    raycast_world_with_lod(world, dim, None, origin, dir, max_distance, mode)
}

/// Multi-tier zero-disk-I/O raycaster:
/// 1. Exact 1x1x1 block DDA through loaded LOD 0 chunks
/// 2. True 3D voxel DDA through distant `LodRaycastIndex` tiles (up to 128+ chunks away)
/// 3. Unlimited-distance `Y = 0` ground plane intersection (up to 16,384 blocks)
pub fn raycast_world_with_lod(
    world: &World,
    dim: DimensionId,
    lod_index: Option<&LodRaycastIndex>,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
    mode: ViewMode,
) -> Option<RaycastHit> {
    if dir.length_squared() < 1e-10 {
        return None;
    }
    let dir = dir.normalize();
    let min_y = world.min_y();
    let max_y = world.max_y();

    if mode == ViewMode::Chunk2D {
        let wx = origin.x.floor() as i32;
        let wz = origin.z.floor() as i32;
        let cp = ChunkPos::new(wx.div_euclid(16), wz.div_euclid(16));
        if let Some(chunk) = world.get_loaded_chunk(dim, cp) {
            let lx = wx.rem_euclid(16) as usize;
            let lz = wz.rem_euclid(16) as usize;
            let h = chunk.heightmap[(lz << 4) | lx];
            for y in (min_y..=h.min(max_y - 1)).rev() {
                if chunk.get_block(lx, y, lz) != BlockStateId::AIR {
                    let pos = IVec3::new(wx, y, wz);
                    return Some(RaycastHit {
                        block_pos: pos,
                        adjacent_pos: pos + IVec3::Y,
                        face: Face::Up,
                        distance: (origin.y - y as f32).abs(),
                    });
                }
            }
        }
        if let Some(lod_map) = lod_index {
            let rx8 = cp.cx().div_euclid(8);
            let rz8 = cp.cz().div_euclid(8);
            if let Some(tile) = lod_map.get(&(rx8, rz8))
                && let Some(y) = tile.top_occupied_world_y(wx, wz)
            {
                let pos = IVec3::new(wx, y, wz);
                return Some(RaycastHit {
                    block_pos: pos,
                    adjacent_pos: pos + IVec3::Y,
                    face: Face::Up,
                    distance: (origin.y - y as f32).abs(),
                });
            }
        }
        let pos = IVec3::new(wx, min_y, wz);
        return Some(RaycastHit {
            block_pos: pos,
            adjacent_pos: pos + IVec3::Y,
            face: Face::Up,
            distance: (origin.y - min_y as f32).abs(),
        });
    }

    // Advance ray to enter [min_y, max_y] if starting outside vertical world bounds
    let mut t = 0.0f32;
    if origin.y >= max_y as f32 {
        if dir.y >= -1e-6 {
            return None;
        }
        let dt = (max_y as f32 - 0.001 - origin.y) / dir.y;
        if dt > max_distance {
            return intersect_floor_plane(origin, dir, min_y);
        }
        t = dt.max(0.0);
    } else if origin.y < min_y as f32 {
        if dir.y <= 1e-6 {
            return None;
        }
        let dt = (min_y as f32 + 0.001 - origin.y) / dir.y;
        if dt > max_distance {
            return intersect_floor_plane(origin, dir, min_y);
        }
        t = dt.max(0.0);
    }

    let step_sign = IVec3::new(
        if dir.x >= 0.0 { 1 } else { -1 },
        if dir.y >= 0.0 { 1 } else { -1 },
        if dir.z >= 0.0 { 1 } else { -1 },
    );
    let inv_dir = Vec3::new(
        if dir.x.abs() > 1e-8 { 1.0 / dir.x } else { f32::INFINITY },
        if dir.y.abs() > 1e-8 { 1.0 / dir.y } else { f32::INFINITY },
        if dir.z.abs() > 1e-8 { 1.0 / dir.z } else { f32::INFINITY },
    );

    let mut hit_face = Face::Up;
    let start_p = origin + dir * t;
    let start_voxel = IVec3::new(
        start_p.x.floor() as i32,
        start_p.y.floor() as i32,
        start_p.z.floor() as i32,
    );
    let started_solid = sample_loaded_block(world, dim, start_voxel) != BlockStateId::AIR;

    let mut cached_cp: Option<ChunkPos> = None;
    let mut cached_chunk: Option<&mcedit_core::Chunk> = None;

    for step_idx in 0..16384 {
        if t > max_distance {
            break;
        }
        let p = origin + dir * (t + 1e-4);
        let voxel = IVec3::new(
            p.x.floor() as i32,
            p.y.floor() as i32,
            p.z.floor() as i32,
        );
        if (voxel.y < min_y && step_sign.y < 0) || (voxel.y >= max_y && step_sign.y > 0) {
            break;
        }

        let cp = ChunkPos::new(voxel.x.div_euclid(16), voxel.z.div_euclid(16));
        if cached_cp != Some(cp) {
            cached_cp = Some(cp);
            cached_chunk = world.get_loaded_chunk(dim, cp);
        }

        // Tier 1: Exact block check in loaded LOD 0 chunk (skips empty 16x16x16 sections in 1 step!)
        if let Some(chunk) = cached_chunk {
            if voxel.y >= min_y && voxel.y < max_y {
                let sy = voxel.y.div_euclid(16);
                if let Some(sec) = chunk.sections.get(&sy)
                    && !sec.is_all_air()
                {
                    let b = sec.get_block(
                        voxel.x.rem_euclid(16) as usize,
                        voxel.y.rem_euclid(16) as usize,
                        voxel.z.rem_euclid(16) as usize,
                    );
                    if b != BlockStateId::AIR && (!started_solid || step_idx > 0) {
                        return Some(RaycastHit {
                            block_pos: voxel,
                            adjacent_pos: voxel + hit_face.normal(),
                            face: hit_face,
                            distance: t,
                        });
                    }
                    let (next_t, next_face) = step_grid_cell(origin, inv_dir, step_sign, voxel, 1);
                    t = next_t.max(t + 1e-4);
                    hit_face = next_face;
                    continue;
                } else {
                    // Empty 16x16x16 air section: jump across the entire section cell at once!
                    let sec_origin = IVec3::new(cp.cx() * 16, sy * 16, cp.cz() * 16);
                    let (next_t, next_face) =
                        step_grid_cell(origin, inv_dir, step_sign, sec_origin, 16);
                    t = next_t.max(t + 1e-4);
                    hit_face = next_face;
                    continue;
                }
            }
            let (next_t, next_face) = step_grid_cell(origin, inv_dir, step_sign, voxel, 1);
            t = next_t.max(t + 1e-4);
            hit_face = next_face;
            continue;
        }

        // Tier 2: Distant 3D LOD occupancy check (step = 2 or 4 blocks per DDA step!)
        if let Some(lod_map) = lod_index {
            let rx8 = cp.cx().div_euclid(8);
            let rz8 = cp.cz().div_euclid(8);
            if let Some(tile) = lod_map.get(&(rx8, rz8)) {
                if tile.is_world_occupied(voxel.x, voxel.y, voxel.z) {
                    return Some(RaycastHit {
                        block_pos: voxel,
                        adjacent_pos: voxel + hit_face.normal(),
                        face: hit_face,
                        distance: t,
                    });
                }
                let s = tile.step as i32;
                let cell_origin = IVec3::new(
                    voxel.x.div_euclid(s) * s,
                    voxel.y.div_euclid(s) * s,
                    voxel.z.div_euclid(s) * s,
                );
                let (next_t, next_face) = step_grid_cell(origin, inv_dir, step_sign, cell_origin, s);
                t = next_t.max(t + 1e-4);
                hit_face = next_face;
                continue;
            }
        }

        // Tier 3: Neither loaded chunk nor LOD tile -> jump straight across the entire 16x16 chunk!
        let chunk_origin = IVec3::new(cp.cx() * 16, min_y, cp.cz() * 16);
        let (next_t, next_face) = step_chunk_column(origin, inv_dir, step_sign, chunk_origin, max_y - min_y);
        t = next_t.max(t + 1e-3);
        hit_face = next_face;
    }

    // Tier 4: Unlimited-distance Y = min_y ground plane intersection
    intersect_floor_plane(origin, dir, min_y)
}

/// Raycasts terrain (LOD 0 + distant LOD + Y=0 floor), falling back to a midair block
/// `air_reach` blocks along `dir` when pointing into open sky.
#[allow(clippy::too_many_arguments)]
pub fn raycast_or_air(
    world: &World,
    dim: DimensionId,
    lod_index: Option<&LodRaycastIndex>,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
    air_reach: f32,
    mode: ViewMode,
) -> RaycastHit {
    if let Some(hit) = raycast_world_with_lod(world, dim, lod_index, origin, dir, max_distance, mode)
    {
        return hit;
    }
    let dir_n = dir.normalize_or(Vec3::NEG_Z);
    let dist = air_reach.max(2.0);
    let p = origin + dir_n * dist;
    let y = (p.y.floor() as i32).clamp(world.min_y(), world.max_y() - 1);
    let bp = IVec3::new(p.x.floor() as i32, y, p.z.floor() as i32);
    RaycastHit {
        block_pos: bp,
        adjacent_pos: bp + IVec3::Y,
        face: Face::Up,
        distance: dist,
    }
}

#[inline]
fn sample_loaded_block(world: &World, dim: DimensionId, pos: IVec3) -> BlockStateId {
    if pos.y < world.min_y() || pos.y >= world.max_y() {
        return BlockStateId::AIR;
    }
    let cp = ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
    world
        .get_loaded_chunk(dim, cp)
        .map(|c| {
            c.get_block(
                pos.x.rem_euclid(16) as usize,
                pos.y,
                pos.z.rem_euclid(16) as usize,
            )
        })
        .unwrap_or(BlockStateId::AIR)
}

#[inline]
fn step_grid_cell(
    origin: Vec3,
    inv_dir: Vec3,
    step_sign: IVec3,
    cell_min: IVec3,
    size: i32,
) -> (f32, Face) {
    let bx = (cell_min.x + if step_sign.x > 0 { size } else { 0 }) as f32;
    let by = (cell_min.y + if step_sign.y > 0 { size } else { 0 }) as f32;
    let bz = (cell_min.z + if step_sign.z > 0 { size } else { 0 }) as f32;

    let tx = if inv_dir.x.is_finite() { (bx - origin.x) * inv_dir.x } else { f32::INFINITY };
    let ty = if inv_dir.y.is_finite() { (by - origin.y) * inv_dir.y } else { f32::INFINITY };
    let tz = if inv_dir.z.is_finite() { (bz - origin.z) * inv_dir.z } else { f32::INFINITY };

    if tx < ty {
        if tx < tz {
            (tx, if step_sign.x > 0 { Face::West } else { Face::East })
        } else {
            (tz, if step_sign.z > 0 { Face::North } else { Face::South })
        }
    } else if ty < tz {
        (ty, if step_sign.y > 0 { Face::Down } else { Face::Up })
    } else {
        (tz, if step_sign.z > 0 { Face::North } else { Face::South })
    }
}

#[inline]
fn step_chunk_column(
    origin: Vec3,
    inv_dir: Vec3,
    step_sign: IVec3,
    chunk_min: IVec3,
    world_h: i32,
) -> (f32, Face) {
    let bx = (chunk_min.x + if step_sign.x > 0 { 16 } else { 0 }) as f32;
    let by = (chunk_min.y + if step_sign.y > 0 { world_h } else { 0 }) as f32;
    let bz = (chunk_min.z + if step_sign.z > 0 { 16 } else { 0 }) as f32;

    let tx = if inv_dir.x.is_finite() { (bx - origin.x) * inv_dir.x } else { f32::INFINITY };
    let ty = if inv_dir.y.is_finite() { (by - origin.y) * inv_dir.y } else { f32::INFINITY };
    let tz = if inv_dir.z.is_finite() { (bz - origin.z) * inv_dir.z } else { f32::INFINITY };

    if tx < ty {
        if tx < tz {
            (tx, if step_sign.x > 0 { Face::West } else { Face::East })
        } else {
            (tz, if step_sign.z > 0 { Face::North } else { Face::South })
        }
    } else if ty < tz {
        (ty, if step_sign.y > 0 { Face::Down } else { Face::Up })
    } else {
        (tz, if step_sign.z > 0 { Face::North } else { Face::South })
    }
}

fn intersect_floor_plane(origin: Vec3, dir: Vec3, floor_y: i32) -> Option<RaycastHit> {
    let fy = floor_y as f32;
    if dir.y.abs() < 1e-5 {
        return None;
    }
    let plane_t = (fy - origin.y) / dir.y;
    if plane_t > 0.0 && plane_t <= 65536.0 {
        let p = origin + dir * plane_t;
        let bp = IVec3::new(p.x.floor() as i32, floor_y, p.z.floor() as i32);
        let face = if dir.y < 0.0 { Face::Up } else { Face::Down };
        return Some(RaycastHit {
            block_pos: bp,
            adjacent_pos: bp + face.normal(),
            face,
            distance: plane_t,
        });
    }
    None
}
