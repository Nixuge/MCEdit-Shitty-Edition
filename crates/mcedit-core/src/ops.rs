use crate::block::{BlockRegistry, BlockState, BlockStateId};
use crate::chunk::Chunk;
use crate::math::{BoundingBox, ChunkPos};
use crate::undo::EditTransaction;
use crate::world::{DimensionId, SchematicVolume, World};
use ahash::AHashMap;
use glam::{IVec3, UVec3};
use mcedit_nbt::{NbtList, NbtTag};

#[derive(Debug, Clone)]
pub struct CopyOptions {
    pub copy_air: bool,
    pub copy_water: bool,
    pub copy_biomes: bool,
    pub update_command_block_coords: bool,
    pub update_spawner_coords: bool,
    pub regenerate_entity_uuids: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            copy_air: true,
            copy_water: true,
            copy_biomes: false,
            update_command_block_coords: true,
            update_spawner_coords: true,
            regenerate_entity_uuids: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnalysisResult {
    pub total_blocks: u64,
    pub block_counts: Vec<(BlockStateId, u64)>,
    pub entity_counts: Vec<(String, u64)>,
    pub block_entity_counts: Vec<(String, u64)>,
}

impl AnalysisResult {
    pub fn to_csv(&self, reg: &BlockRegistry) -> String {
        let mut out = String::from("Category,ID,Name,Count\n");
        for &(id, count) in &self.block_counts {
            let state = reg.get_state(id);
            let meta = reg.get_meta(id);
            out.push_str(&format!(
                "Block,\"{}\",\"{}\",{}\n",
                state,
                meta.display_name.replace('"', "\"\""),
                count
            ));
        }
        for (name, count) in &self.entity_counts {
            out.push_str(&format!("Entity,\"{name}\",\"{name}\",{count}\n"));
        }
        for (name, count) in &self.block_entity_counts {
            out.push_str(&format!("BlockEntity,\"{name}\",\"{name}\",{count}\n"));
        }
        out
    }
}

pub fn fill_blocks(
    world: &mut World,
    dim: DimensionId,
    box_: BoundingBox,
    fill_with: BlockStateId,
    replace_target: Option<BlockStateId>,
    match_base_only: bool,
    tx: &mut EditTransaction,
) -> u64 {
    let min_y = world.min_y();
    let max_y = world.max_y();
    let clamped = BoundingBox {
        min: IVec3::new(box_.min.x, box_.min.y.max(min_y), box_.min.z),
        max: IVec3::new(box_.max.x, box_.max.y.min(max_y), box_.max.z),
    };
    if clamped.is_empty() {
        return 0;
    }

    let chunks: Vec<ChunkPos> = clamped.iter_chunks().collect();
    let mut changed = 0u64;

    for cp in chunks {
        if !world.chunk_exists(dim, cp) {
            continue;
        }
        tx.record_chunk_if_needed(world, cp);
        let origin = cp.world_origin();
        let x0 = (clamped.min.x - origin.x).clamp(0, 15) as usize;
        let x1 = (clamped.max.x - 1 - origin.x).clamp(0, 15) as usize;
        let z0 = (clamped.min.z - origin.z).clamp(0, 15) as usize;
        let z1 = (clamped.max.z - 1 - origin.z).clamp(0, 15) as usize;

        let reg = world.registry.clone();
        if let Ok(Some(chunk)) = world.get_chunk_mut(dim, cp) {
            let reg_r = reg.read();
            for y in clamped.min.y..clamped.max.y {
                for z in z0..=z1 {
                    for x in x0..=x1 {
                        let cur = chunk.get_block(x, y, z);
                        let should_set = match replace_target {
                            None => true,
                            Some(t) => reg_r.matches(cur, t, match_base_only),
                        };
                        if should_set && chunk.set_block(x, y, z, fill_with) {
                            changed += 1;
                        }
                    }
                }
            }
            // Remove overwritten block entities inside selection
            chunk.block_entities.retain(|be| {
                if let (Some(bx), Some(by), Some(bz)) =
                    (be.get_int("x"), be.get_int("y"), be.get_int("z"))
                {
                    let p = IVec3::new(bx, by, bz);
                    if clamped.contains(p) {
                        return replace_target.is_some();
                    }
                }
                true
            });
        }
    }

    tx.expand_dirty_box(clamped);
    changed
}

pub fn extract_volume(
    world: &mut World,
    dim: DimensionId,
    box_: BoundingBox,
) -> SchematicVolume {
    let size = box_.size();
    let mut vol = SchematicVolume::new(UVec3::new(size.x as u32, size.y as u32, size.z as u32));
    if box_.is_empty() {
        return vol;
    }

    let mut biomes = vec![1u8; (size.x as usize) * (size.z as usize)];
    let chunks: Vec<ChunkPos> = box_.iter_chunks().collect();
    let y_start = box_.min.y.max(world.min_y());
    let y_end = box_.max.y.min(world.max_y());

    for cp in chunks {
        let Ok(Some(chunk)) = world.get_chunk(dim, cp) else {
            continue;
        };
        let origin = cp.world_origin();
        let x0 = (box_.min.x - origin.x).clamp(0, 15);
        let x1 = (box_.max.x - 1 - origin.x).clamp(0, 15);
        let z0 = (box_.min.z - origin.z).clamp(0, 15);
        let z1 = (box_.max.z - 1 - origin.z).clamp(0, 15);

        for z in z0..=z1 {
            let wz = origin.z + z;
            let rz = (wz - box_.min.z) as u32;
            for x in x0..=x1 {
                let wx = origin.x + x;
                let rx = (wx - box_.min.x) as u32;
                biomes[(rz as usize) * (size.x as usize) + (rx as usize)] =
                    chunk.biomes.get(x as usize, box_.min.y, z as usize);

                for wy in y_start..y_end {
                    let ry = (wy - box_.min.y) as u32;
                    let b = chunk.get_block(x as usize, wy, z as usize);
                    vol.set_block(rx, ry, rz, b);
                }
            }
        }

        for be in &chunk.block_entities {
            if let (Some(bx), Some(by), Some(bz)) = (be.get_int("x"), be.get_int("y"), be.get_int("z"))
                && box_.contains(IVec3::new(bx, by, bz))
            {
                let mut cloned = be.clone();
                cloned.insert("x", bx - box_.min.x);
                cloned.insert("y", by - box_.min.y);
                cloned.insert("z", bz - box_.min.z);
                vol.block_entities.push(cloned);
            }
        }

        for ent in &chunk.entities {
            if let Some(pos_list) = ent.get_list("Pos")
                && pos_list.len() == 3
            {
                let coords: Vec<f64> = pos_list
                    .items()
                    .iter()
                    .filter_map(|t| match t {
                        NbtTag::Double(v) => Some(*v),
                        NbtTag::Float(v) => Some(*v as f64),
                        _ => None,
                    })
                    .collect();
                if coords.len() == 3 {
                    let ipos = IVec3::new(
                        coords[0].floor() as i32,
                        coords[1].floor() as i32,
                        coords[2].floor() as i32,
                    );
                    if box_.contains(ipos) {
                        let mut cloned = ent.clone();
                        let new_pos = NbtList::from_items(vec![
                            NbtTag::Double(coords[0] - box_.min.x as f64),
                            NbtTag::Double(coords[1] - box_.min.y as f64),
                            NbtTag::Double(coords[2] - box_.min.z as f64),
                        ])
                        .unwrap();
                        cloned.insert("Pos", new_pos);
                        vol.entities.push(cloned);
                    }
                }
            }
        }
    }

    vol.biomes = Some(biomes);
    vol
}

pub fn paste_volume(
    world: &mut World,
    dim: DimensionId,
    vol: &SchematicVolume,
    dest_origin: IVec3,
    options: &CopyOptions,
    tx: &mut EditTransaction,
) {
    let dest_box = BoundingBox::from_origin_size(
        dest_origin,
        IVec3::new(vol.size.x as i32, vol.size.y as i32, vol.size.z as i32),
    );
    let min_y = world.min_y();
    let max_y = world.max_y();
    let clamped = BoundingBox {
        min: IVec3::new(dest_box.min.x, dest_box.min.y.max(min_y), dest_box.min.z),
        max: IVec3::new(dest_box.max.x, dest_box.max.y.min(max_y), dest_box.max.z),
    };
    if clamped.is_empty() {
        return;
    }

    let reg_arc = world.registry.clone();
    let chunks: Vec<ChunkPos> = clamped.iter_chunks().collect();

    for cp in chunks {
        if !world.chunk_exists(dim, cp) {
            continue;
        }
        tx.record_chunk_if_needed(world, cp);
        let origin = cp.world_origin();
        let x0 = (clamped.min.x - origin.x).clamp(0, 15);
        let x1 = (clamped.max.x - 1 - origin.x).clamp(0, 15);
        let z0 = (clamped.min.z - origin.z).clamp(0, 15);
        let z1 = (clamped.max.z - 1 - origin.z).clamp(0, 15);

        if let Ok(Some(chunk)) = world.get_chunk_mut(dim, cp) {
            let reg = reg_arc.read();
            for z in z0..=z1 {
                let wz = origin.z + z;
                let sz = (wz - dest_origin.z) as u32;
                for x in x0..=x1 {
                    let wx = origin.x + x;
                    let sx = (wx - dest_origin.x) as u32;

                    if options.copy_biomes
                        && let Some(biomes) = &vol.biomes
                    {
                        let b = biomes[(sz as usize) * (vol.size.x as usize) + (sx as usize)];
                        chunk.biomes.set(x as usize, clamped.min.y, z as usize, b);
                    }

                    for wy in clamped.min.y..clamped.max.y {
                        let sy = (wy - dest_origin.y) as u32;
                        let id = vol.get_block(sx, sy, sz);
                        if !options.copy_air && id == BlockStateId::AIR {
                            continue;
                        }
                        if !options.copy_water && reg.get_meta(id).is_liquid {
                            continue;
                        }
                        chunk.set_block(x as usize, wy, z as usize, id);
                    }
                }
            }

            // Clear overwritten block entities inside pasted region
            chunk.block_entities.retain(|be| {
                if let (Some(bx), Some(by), Some(bz)) =
                    (be.get_int("x"), be.get_int("y"), be.get_int("z"))
                {
                    !clamped.contains(IVec3::new(bx, by, bz))
                } else {
                    true
                }
            });
        }
    }

    // Paste block entities with coordinate offsetting
    for be in &vol.block_entities {
        if let (Some(rx), Some(ry), Some(rz)) = (be.get_int("x"), be.get_int("y"), be.get_int("z")) {
            let wx = dest_origin.x + rx;
            let wy = dest_origin.y + ry;
            let wz = dest_origin.z + rz;
            if !clamped.contains(IVec3::new(wx, wy, wz)) {
                continue;
            }
            let cp = ChunkPos::new(wx.div_euclid(16), wz.div_euclid(16));
            if let Ok(Some(chunk)) = world.get_chunk_mut(dim, cp) {
                let mut cloned = be.clone();
                cloned.insert("x", wx);
                cloned.insert("y", wy);
                cloned.insert("z", wz);
                if options.update_spawner_coords
                    && let Some(spawn_data) = cloned.get_compound_mut("SpawnData")
                    && let Some(pos_list) = spawn_data.get_list_mut("Pos")
                    && pos_list.len() == 3
                {
                    offset_double_list(pos_list, dest_origin);
                }
                chunk.block_entities.push(cloned);
            }
        }
    }

    // Paste entities
    for ent in &vol.entities {
        if let Some(pos_list) = ent.get_list("Pos")
            && pos_list.len() == 3
        {
            let coords: Vec<f64> = pos_list
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Double(v) => Some(*v),
                    NbtTag::Float(v) => Some(*v as f64),
                    _ => None,
                })
                .collect();
            if coords.len() == 3 {
                let wx = coords[0] + dest_origin.x as f64;
                let wy = coords[1] + dest_origin.y as f64;
                let wz = coords[2] + dest_origin.z as f64;
                let cp = ChunkPos::new((wx.floor() as i32).div_euclid(16), (wz.floor() as i32).div_euclid(16));
                if let Ok(Some(chunk)) = world.get_chunk_mut(dim, cp) {
                    let mut cloned = ent.clone();
                    let new_pos = NbtList::from_items(vec![
                        NbtTag::Double(wx),
                        NbtTag::Double(wy),
                        NbtTag::Double(wz),
                    ])
                    .unwrap();
                    cloned.insert("Pos", new_pos);
                    if options.regenerate_entity_uuids {
                        cloned.remove("UUIDMost");
                        cloned.remove("UUIDLeast");
                    }
                    chunk.entities.push(cloned);
                }
            }
        }
    }

    tx.expand_dirty_box(clamped);
}

fn offset_double_list(list: &mut NbtList, delta: IVec3) {
    let items = list.items_mut();
    if items.len() == 3 {
        if let NbtTag::Double(v) = &mut items[0] {
            *v += delta.x as f64;
        }
        if let NbtTag::Double(v) = &mut items[1] {
            *v += delta.y as f64;
        }
        if let NbtTag::Double(v) = &mut items[2] {
            *v += delta.z as f64;
        }
    }
}

pub fn analyze_box(world: &mut World, dim: DimensionId, box_: BoundingBox) -> AnalysisResult {
    let mut blocks_map: AHashMap<BlockStateId, u64> = AHashMap::new();
    let mut entity_map: AHashMap<String, u64> = AHashMap::new();
    let mut be_map: AHashMap<String, u64> = AHashMap::new();
    let mut total_blocks = 0u64;

    let min_y = world.min_y();
    let max_y = world.max_y();
    let clamped = BoundingBox {
        min: IVec3::new(box_.min.x, box_.min.y.max(min_y), box_.min.z),
        max: IVec3::new(box_.max.x, box_.max.y.min(max_y), box_.max.z),
    };

    for cp in clamped.iter_chunks() {
        let Ok(Some(chunk)) = world.get_chunk(dim, cp) else {
            continue;
        };
        let origin = cp.world_origin();
        let x0 = (clamped.min.x - origin.x).clamp(0, 15) as usize;
        let x1 = (clamped.max.x - 1 - origin.x).clamp(0, 15) as usize;
        let z0 = (clamped.min.z - origin.z).clamp(0, 15) as usize;
        let z1 = (clamped.max.z - 1 - origin.z).clamp(0, 15) as usize;

        for y in clamped.min.y..clamped.max.y {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let id = chunk.get_block(x, y, z);
                    *blocks_map.entry(id).or_insert(0) += 1;
                    total_blocks += 1;
                }
            }
        }

        for be in &chunk.block_entities {
            if let (Some(bx), Some(by), Some(bz)) = (be.get_int("x"), be.get_int("y"), be.get_int("z"))
                && clamped.contains(IVec3::new(bx, by, bz))
            {
                let id = be.get_string("id").unwrap_or("unknown").to_string();
                *be_map.entry(id).or_insert(0) += 1;
            }
        }

        for ent in &chunk.entities {
            if let Some(pos_list) = ent.get_list("Pos")
                && pos_list.len() == 3
            {
                let coords: Vec<f64> = pos_list
                    .items()
                    .iter()
                    .filter_map(|t| match t {
                        NbtTag::Double(v) => Some(*v),
                        NbtTag::Float(v) => Some(*v as f64),
                        _ => None,
                    })
                    .collect();
                if coords.len() == 3 {
                    let ipos = IVec3::new(
                        coords[0].floor() as i32,
                        coords[1].floor() as i32,
                        coords[2].floor() as i32,
                    );
                    if clamped.contains(ipos) {
                        let id = ent.get_string("id").unwrap_or("unknown").to_string();
                        *entity_map.entry(id).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    let mut block_counts: Vec<_> = blocks_map.into_iter().collect();
    block_counts.sort_by_key(|b| std::cmp::Reverse(b.1));
    let mut entity_counts: Vec<_> = entity_map.into_iter().collect();
    entity_counts.sort_by_key(|b| std::cmp::Reverse(b.1));
    let mut block_entity_counts: Vec<_> = be_map.into_iter().collect();
    block_entity_counts.sort_by_key(|b| std::cmp::Reverse(b.1));

    AnalysisResult {
        total_blocks,
        block_counts,
        entity_counts,
        block_entity_counts,
    }
}

pub fn delete_entities_in_box(
    world: &mut World,
    dim: DimensionId,
    box_: BoundingBox,
    tx: &mut EditTransaction,
) -> usize {
    let mut removed = 0usize;
    let chunks: Vec<ChunkPos> = box_.iter_chunks().collect();
    for cp in chunks {
        if !world.chunk_exists(dim, cp) {
            continue;
        }
        tx.record_chunk_if_needed(world, cp);
        if let Ok(Some(chunk)) = world.get_chunk_mut(dim, cp) {
            let before = chunk.entities.len();
            chunk.entities.retain(|ent| {
                if let Some(pos_list) = ent.get_list("Pos")
                    && pos_list.len() == 3
                {
                    let coords: Vec<f64> = pos_list
                        .items()
                        .iter()
                        .filter_map(|t| match t {
                            NbtTag::Double(v) => Some(*v),
                            NbtTag::Float(v) => Some(*v as f64),
                            _ => None,
                        })
                        .collect();
                    if coords.len() == 3 {
                        let ipos = IVec3::new(
                            coords[0].floor() as i32,
                            coords[1].floor() as i32,
                            coords[2].floor() as i32,
                        );
                        return !box_.contains(ipos);
                    }
                }
                true
            });
            removed += before - chunk.entities.len();
            if before != chunk.entities.len() {
                chunk.dirty = true;
            }
        }
    }
    tx.expand_dirty_box(box_);
    removed
}

pub fn delete_tile_ticks_in_box(
    world: &mut World,
    dim: DimensionId,
    box_: BoundingBox,
    tx: &mut EditTransaction,
) -> usize {
    let mut removed = 0usize;
    let chunks: Vec<ChunkPos> = box_.iter_chunks().collect();
    for cp in chunks {
        if !world.chunk_exists(dim, cp) {
            continue;
        }
        tx.record_chunk_if_needed(world, cp);
        if let Ok(Some(chunk)) = world.get_chunk_mut(dim, cp) {
            let before = chunk.tile_ticks.len();
            chunk.tile_ticks.retain(|tt| {
                if let (Some(x), Some(y), Some(z)) =
                    (tt.get_int("x"), tt.get_int("y"), tt.get_int("z"))
                {
                    !box_.contains(IVec3::new(x, y, z))
                } else {
                    true
                }
            });
            removed += before - chunk.tile_ticks.len();
            if before != chunk.tile_ticks.len() {
                chunk.dirty = true;
            }
        }
    }
    tx.expand_dirty_box(box_);
    removed
}

#[derive(Debug, Clone)]
pub struct FlatlandLayer {
    pub block: BlockState,
    pub height: i32,
}

pub fn default_flatland_layers() -> Vec<FlatlandLayer> {
    vec![
        FlatlandLayer {
            block: BlockState::new("minecraft:bedrock"),
            height: 1,
        },
        FlatlandLayer {
            block: BlockState::new("minecraft:dirt"),
            height: 2,
        },
        FlatlandLayer {
            block: BlockState::new("minecraft:grass").with_prop("snowy", "false"),
            height: 1,
        },
    ]
}

pub fn create_flatland_chunk(
    pos: ChunkPos,
    min_section_y: i32,
    max_section_y: i32,
    layers: &[FlatlandLayer],
    reg: &mut BlockRegistry,
) -> Chunk {
    let mut chunk = Chunk::new(pos, min_section_y, max_section_y);
    let mut y = min_section_y * 16;
    for layer in layers {
        let block_id = reg.intern(layer.block.clone());
        for _ in 0..layer.height.max(0) {
            if y >= (max_section_y + 1) * 16 {
                break;
            }
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set_block(x, y, z, block_id);
                }
            }
            y += 1;
        }
    }
    chunk.recompute_heightmap(reg);
    chunk.terrain_populated = true;
    chunk.dirty = true;
    chunk
}
