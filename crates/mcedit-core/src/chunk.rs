use crate::block::{BlockRegistry, BlockStateId};
use crate::math::ChunkPos;
use crate::section::ChunkSection;
use mcedit_nbt::NbtCompound;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum BiomeStorage {
    Columns2D([u8; 256]),
    Cells3D(BTreeMap<i32, [u8; 64]>),
}

impl Default for BiomeStorage {
    fn default() -> Self {
        Self::Columns2D([1u8; 256]) // Plains default
    }
}

impl BiomeStorage {
    pub fn get(&self, x: usize, y: i32, z: usize) -> u8 {
        match self {
            BiomeStorage::Columns2D(cols) => cols[((z & 15) << 4) | (x & 15)],
            BiomeStorage::Cells3D(cells) => {
                let sy = y.div_euclid(16);
                if let Some(sec) = cells.get(&sy) {
                    let cx = (x & 15) >> 2;
                    let cy = (y.rem_euclid(16) as usize) >> 2;
                    let cz = (z & 15) >> 2;
                    sec[(cy << 4) | (cz << 2) | cx]
                } else {
                    1
                }
            }
        }
    }

    pub fn set(&mut self, x: usize, y: i32, z: usize, biome: u8) {
        match self {
            BiomeStorage::Columns2D(cols) => {
                cols[((z & 15) << 4) | (x & 15)] = biome;
            }
            BiomeStorage::Cells3D(cells) => {
                let sy = y.div_euclid(16);
                let sec = cells.entry(sy).or_insert([1u8; 64]);
                let cx = (x & 15) >> 2;
                let cy = (y.rem_euclid(16) as usize) >> 2;
                let cz = (z & 15) >> 2;
                sec[(cy << 4) | (cz << 2) | cx] = biome;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub pos: ChunkPos,
    pub min_section_y: i32,
    pub max_section_y: i32,
    pub sections: BTreeMap<i32, ChunkSection>,
    pub biomes: BiomeStorage,
    pub heightmap: [i32; 256],
    pub entities: Vec<NbtCompound>,
    pub block_entities: Vec<NbtCompound>,
    pub tile_ticks: Vec<NbtCompound>,
    pub terrain_populated: bool,
    pub extra_nbt: NbtCompound,
    pub dirty: bool,
    pub needs_lighting: bool,
}

impl Chunk {
    pub fn new(pos: ChunkPos, min_section_y: i32, max_section_y: i32) -> Self {
        Self {
            pos,
            min_section_y,
            max_section_y,
            sections: BTreeMap::new(),
            biomes: BiomeStorage::default(),
            heightmap: [0; 256],
            entities: Vec::new(),
            block_entities: Vec::new(),
            tile_ticks: Vec::new(),
            terrain_populated: true,
            extra_nbt: NbtCompound::new(),
            dirty: false,
            needs_lighting: false,
        }
    }

    pub fn min_y(&self) -> i32 {
        self.min_section_y * 16
    }

    pub fn max_y(&self) -> i32 {
        (self.max_section_y + 1) * 16
    }

    pub fn in_y_bounds(&self, y: i32) -> bool {
        y >= self.min_y() && y < self.max_y()
    }

    pub fn get_block(&self, x: usize, y: i32, z: usize) -> BlockStateId {
        let sy = y.div_euclid(16);
        if let Some(sec) = self.sections.get(&sy) {
            sec.get_block(x & 15, y.rem_euclid(16) as usize, z & 15)
        } else {
            BlockStateId::AIR
        }
    }

    pub fn set_block(&mut self, x: usize, y: i32, z: usize, block: BlockStateId) -> bool {
        if !self.in_y_bounds(y) {
            return false;
        }
        let sy = y.div_euclid(16);
        let ly = y.rem_euclid(16) as usize;
        if block == BlockStateId::AIR && !self.sections.contains_key(&sy) {
            return false;
        }
        let sec = self.sections.entry(sy).or_default();
        let old = sec.get_block(x & 15, ly, z & 15);
        if old == block {
            return false;
        }
        sec.set_block(x & 15, ly, z & 15, block);
        self.dirty = true;
        true
    }

    pub fn get_block_light(&self, x: usize, y: i32, z: usize) -> u8 {
        let sy = y.div_euclid(16);
        self.sections
            .get(&sy)
            .map(|s| s.get_block_light(x & 15, y.rem_euclid(16) as usize, z & 15))
            .unwrap_or(0)
    }

    pub fn set_block_light(&mut self, x: usize, y: i32, z: usize, val: u8) {
        if !self.in_y_bounds(y) {
            return;
        }
        let sy = y.div_euclid(16);
        if let Some(sec) = self.sections.get_mut(&sy) {
            sec.set_block_light(x & 15, y.rem_euclid(16) as usize, z & 15, val);
        } else if val > 0 {
            let sec = self.sections.entry(sy).or_default();
            sec.set_block_light(x & 15, y.rem_euclid(16) as usize, z & 15, val);
        }
    }

    pub fn get_sky_light(&self, x: usize, y: i32, z: usize) -> u8 {
        let sy = y.div_euclid(16);
        self.sections
            .get(&sy)
            .map(|s| s.get_sky_light(x & 15, y.rem_euclid(16) as usize, z & 15))
            .unwrap_or(15)
    }

    pub fn set_sky_light(&mut self, x: usize, y: i32, z: usize, val: u8) {
        if !self.in_y_bounds(y) {
            return;
        }
        let sy = y.div_euclid(16);
        if let Some(sec) = self.sections.get_mut(&sy) {
            sec.set_sky_light(x & 15, y.rem_euclid(16) as usize, z & 15, val);
        } else if val < 15 {
            let sec = self.sections.entry(sy).or_default();
            sec.set_sky_light(x & 15, y.rem_euclid(16) as usize, z & 15, val);
        }
    }

    pub fn recompute_heightmap(&mut self, registry: &BlockRegistry) {
        let min_y = self.min_y();
        self.heightmap.fill(min_y);
        let mut found = [false; 256];
        let mut remaining = 256usize;

        for (&sy, sec) in self.sections.iter().rev() {
            if sec.is_all_air() {
                continue;
            }
            let base_y = sy * 16;
            for ly in (0..16).rev() {
                let y_off = ly << 8;
                let wy = base_y + ly as i32 + 1;
                for z in 0..16 {
                    let yz_off = y_off | (z << 4);
                    let row = z << 4;
                    for x in 0..16 {
                        let col = row | x;
                        if found[col] {
                            continue;
                        }
                        let pidx = sec.indices[yz_off | x] as usize;
                        if pidx == 0 {
                            continue;
                        }
                        let b = sec.palette.get(pidx).copied().unwrap_or(BlockStateId::AIR);
                        if b != BlockStateId::AIR && registry.get_meta(b).opacity > 0 {
                            self.heightmap[col] = wy;
                            found[col] = true;
                            remaining -= 1;
                            if remaining == 0 {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn prune_empty_sections(&mut self) {
        self.sections.retain(|_, s| !s.is_trivially_empty());
    }
}
