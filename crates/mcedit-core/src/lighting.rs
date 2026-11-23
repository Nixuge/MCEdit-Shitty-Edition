use crate::block::{BlockRegistry, BlockStateId};
use crate::chunk::Chunk;
use crate::math::ChunkPos;
use crate::world::{DimensionId, World};
use ahash::{AHashMap, AHashSet};
use glam::IVec3;
use std::collections::VecDeque;

pub struct LightingEngine;

impl LightingEngine {
    /// Relight the specified chunks (propagating up to 14 blocks into neighboring chunks).
    pub fn relight_chunks(
        world: &mut World,
        dim: DimensionId,
        target_chunks: &[ChunkPos],
    ) {
        if target_chunks.is_empty() {
            return;
        }

        let min_y = world.min_y();
        let max_y = world.max_y();
        let has_sky = dim.has_skylight();

        // Collect target chunks + 1-chunk halo
        let target_set: AHashSet<ChunkPos> = target_chunks.iter().copied().collect();
        let mut halo_set = AHashSet::new();
        for &cp in target_chunks {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    halo_set.insert(ChunkPos::new(cp.cx() + dx, cp.cz() + dz));
                }
            }
        }

        // Load chunks into working map
        let mut chunks: AHashMap<ChunkPos, Chunk> = AHashMap::new();
        for &cp in &halo_set {
            if target_set.contains(&cp) {
                if let Ok(Some(c)) = world.get_chunk_mut(dim, cp) {
                    chunks.insert(cp, c.clone());
                }
            } else if let Ok(Some(c)) = world.get_chunk(dim, cp) {
                chunks.insert(cp, c.clone());
            }
        }

        let reg = world.registry.read();

        // Step 1: Initialize target chunks (heightmap, vertical skylight, emission seeds)
        let mut sky_queue: VecDeque<IVec3> = VecDeque::new();
        let mut block_queue: VecDeque<IVec3> = VecDeque::new();

        for &cp in target_chunks {
            let Some(chunk) = chunks.get_mut(&cp) else {
                continue;
            };
            chunk.recompute_heightmap(&reg);
            let origin = cp.world_origin();

            // Clear lights in existing sections of target chunk, then seed
            for (&sy, sec) in &mut chunk.sections {
                sec.block_light.fill(0);
                sec.sky_light.fill(0);
                let base_y = sy * 16;
                for ly in 0..16 {
                    let wy = base_y + ly as i32;
                    for lz in 0..16 {
                        for lx in 0..16 {
                            let b = sec.get_block(lx, ly, lz);
                            let em = reg.get_meta(b).light_emission;
                            if em > 0 {
                                sec.set_block_light(lx, ly, lz, em.min(15));
                                block_queue.push_back(IVec3::new(
                                    origin.x + lx as i32,
                                    wy,
                                    origin.z + lz as i32,
                                ));
                            }
                        }
                    }
                }
            }

            if has_sky {
                for lz in 0..16 {
                    for lx in 0..16 {
                        let mut light: i32 = 15;
                        for wy in (min_y..max_y).rev() {
                            let sy = wy.div_euclid(16);
                            let Some(sec) = chunk.sections.get_mut(&sy) else {
                                if light > 0 {
                                    // Empty section keeps full vertical light
                                    continue;
                                } else {
                                    break;
                                }
                            };
                            let ly = wy.rem_euclid(16) as usize;
                            let b = sec.get_block(lx, ly, lz);
                            let op = reg.get_meta(b).opacity as i32;
                            if op >= 15 {
                                light = 0;
                            } else {
                                light = (light - op).max(0);
                            }
                            if light > 0 {
                                sec.set_sky_light(lx, ly, lz, light as u8);
                                sky_queue.push_back(IVec3::new(
                                    origin.x + lx as i32,
                                    wy,
                                    origin.z + lz as i32,
                                ));
                            }
                            if light == 0 {
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Seed from halo borders into target chunks
        for &cp in &halo_set {
            if target_set.contains(&cp) {
                continue;
            }
            if !chunks.contains_key(&cp) {
                continue;
            }
            let origin = cp.world_origin();
            for dx in [-1, 1] {
                let ncp = ChunkPos::new(cp.cx() + dx, cp.cz());
                if target_set.contains(&ncp) {
                    let lx = if dx == 1 { 15 } else { 0 };
                    for lz in 0..16 {
                        for wy in min_y..max_y {
                            let pos = IVec3::new(origin.x + lx, wy, origin.z + lz);
                            if get_block_light_map(&chunks, pos) > 1 {
                                block_queue.push_back(pos);
                            }
                            if has_sky && get_sky_light_map(&chunks, pos) > 1 {
                                sky_queue.push_back(pos);
                            }
                        }
                    }
                }
            }
            for dz in [-1, 1] {
                let ncp = ChunkPos::new(cp.cx(), cp.cz() + dz);
                if target_set.contains(&ncp) {
                    let lz = if dz == 1 { 15 } else { 0 };
                    for lx in 0..16 {
                        for wy in min_y..max_y {
                            let pos = IVec3::new(origin.x + lx, wy, origin.z + lz);
                            if get_block_light_map(&chunks, pos) > 1 {
                                block_queue.push_back(pos);
                            }
                            if has_sky && get_sky_light_map(&chunks, pos) > 1 {
                                sky_queue.push_back(pos);
                            }
                        }
                    }
                }
            }
        }

        // Step 2: BFS Flood-fill BlockLight & SkyLight
        propagate_light(&mut chunks, &reg, &mut block_queue, min_y, max_y, false);
        if has_sky {
            propagate_light(&mut chunks, &reg, &mut sky_queue, min_y, max_y, true);
        }
        drop(reg);

        // Write modified target chunks back to world
        for &cp in target_chunks {
            if let Some(mut c) = chunks.remove(&cp) {
                c.dirty = true;
                c.needs_lighting = false;
                world.insert_chunk(dim, c);
            }
        }
    }
}

const NEIGHBORS: [IVec3; 6] = [
    IVec3::new(1, 0, 0),
    IVec3::new(-1, 0, 0),
    IVec3::new(0, 1, 0),
    IVec3::new(0, -1, 0),
    IVec3::new(0, 0, 1),
    IVec3::new(0, 0, -1),
];

fn propagate_light(
    chunks: &mut AHashMap<ChunkPos, Chunk>,
    reg: &BlockRegistry,
    queue: &mut VecDeque<IVec3>,
    min_y: i32,
    max_y: i32,
    is_sky: bool,
) {
    while let Some(pos) = queue.pop_front() {
        let cur = if is_sky {
            get_sky_light_map(chunks, pos)
        } else {
            get_block_light_map(chunks, pos)
        };
        if cur <= 1 {
            continue;
        }
        for n_off in NEIGHBORS {
            let np = pos + n_off;
            if np.y < min_y || np.y >= max_y {
                continue;
            }
            let ncp = ChunkPos::new(np.x.div_euclid(16), np.z.div_euclid(16));
            let Some(chunk) = chunks.get_mut(&ncp) else {
                continue;
            };
            let lx = np.x.rem_euclid(16) as usize;
            let lz = np.z.rem_euclid(16) as usize;
            let block = chunk.get_block(lx, np.y, lz);
            let opacity = if block == BlockStateId::AIR {
                1
            } else {
                reg.get_meta(block).opacity.max(1)
            };
            let next_light = cur.saturating_sub(opacity);
            if next_light == 0 {
                continue;
            }
            let existing = if is_sky {
                chunk.get_sky_light(lx, np.y, lz)
            } else {
                chunk.get_block_light(lx, np.y, lz)
            };
            if next_light > existing {
                if is_sky {
                    chunk.set_sky_light(lx, np.y, lz, next_light);
                } else {
                    chunk.set_block_light(lx, np.y, lz, next_light);
                }
                queue.push_back(np);
            }
        }
    }
}

fn get_block_light_map(chunks: &AHashMap<ChunkPos, Chunk>, pos: IVec3) -> u8 {
    let cp = ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
    chunks
        .get(&cp)
        .map(|c| {
            c.get_block_light(
                pos.x.rem_euclid(16) as usize,
                pos.y,
                pos.z.rem_euclid(16) as usize,
            )
        })
        .unwrap_or(0)
}

fn get_sky_light_map(chunks: &AHashMap<ChunkPos, Chunk>, pos: IVec3) -> u8 {
    let cp = ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
    chunks
        .get(&cp)
        .map(|c| {
            c.get_sky_light(
                pos.x.rem_euclid(16) as usize,
                pos.y,
                pos.z.rem_euclid(16) as usize,
            )
        })
        .unwrap_or(15)
}
