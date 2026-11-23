use crate::block::BlockStateId;
use serde::{Deserialize, Serialize};

pub const SECTION_SIZE: usize = 16;
pub const SECTION_VOLUME: usize = 4096;
pub const LIGHT_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSection {
    pub palette: Vec<BlockStateId>,
    pub indices: Vec<u16>,
    pub non_air_count: u16,
    pub block_light: Vec<u8>,
    pub sky_light: Vec<u8>,
}

impl Default for ChunkSection {
    fn default() -> Self {
        Self::new_empty()
    }
}

impl ChunkSection {
    pub fn new_empty() -> Self {
        Self {
            palette: vec![BlockStateId::AIR],
            indices: vec![0u16; SECTION_VOLUME],
            non_air_count: 0,
            block_light: vec![0u8; LIGHT_BYTES],
            sky_light: vec![0xFFu8; LIGHT_BYTES],
        }
    }

    #[inline]
    pub const fn index(x: usize, y: usize, z: usize) -> usize {
        (y << 8) | (z << 4) | x
    }

    #[inline]
    pub fn get_block(&self, x: usize, y: usize, z: usize) -> BlockStateId {
        let p_idx = self.indices[Self::index(x, y, z)] as usize;
        self.palette.get(p_idx).copied().unwrap_or(BlockStateId::AIR)
    }

    pub fn set_block(&mut self, x: usize, y: usize, z: usize, block: BlockStateId) {
        let cell = Self::index(x, y, z);
        let old_id = self.palette[self.indices[cell] as usize];
        if old_id == block {
            return;
        }
        if old_id == BlockStateId::AIR && block != BlockStateId::AIR {
            self.non_air_count = self.non_air_count.saturating_add(1);
        } else if old_id != BlockStateId::AIR && block == BlockStateId::AIR {
            self.non_air_count = self.non_air_count.saturating_sub(1);
        }

        let pal_idx = if let Some(pos) = self.palette.iter().position(|&b| b == block) {
            pos as u16
        } else {
            let idx = self.palette.len() as u16;
            self.palette.push(block);
            idx
        };
        self.indices[cell] = pal_idx;
    }

    pub fn compact_palette(&mut self) {
        let mut used = vec![false; self.palette.len()];
        for &idx in &self.indices {
            if let Some(u) = used.get_mut(idx as usize) {
                *u = true;
            }
        }
        if used.iter().all(|&u| u) {
            return;
        }
        let mut remap = vec![0u16; self.palette.len()];
        let mut new_palette = Vec::new();
        for (old_i, (&block, &is_used)) in self.palette.iter().zip(used.iter()).enumerate() {
            if is_used || block == BlockStateId::AIR {
                remap[old_i] = new_palette.len() as u16;
                new_palette.push(block);
            }
        }
        if new_palette.is_empty() {
            new_palette.push(BlockStateId::AIR);
        }
        for idx in &mut self.indices {
            *idx = remap[*idx as usize];
        }
        self.palette = new_palette;
    }

    #[inline]
    pub fn get_block_light(&self, x: usize, y: usize, z: usize) -> u8 {
        get_nibble(&self.block_light, Self::index(x, y, z))
    }

    #[inline]
    pub fn set_block_light(&mut self, x: usize, y: usize, z: usize, val: u8) {
        set_nibble(&mut self.block_light, Self::index(x, y, z), val);
    }

    #[inline]
    pub fn get_sky_light(&self, x: usize, y: usize, z: usize) -> u8 {
        get_nibble(&self.sky_light, Self::index(x, y, z))
    }

    #[inline]
    pub fn set_sky_light(&mut self, x: usize, y: usize, z: usize, val: u8) {
        set_nibble(&mut self.sky_light, Self::index(x, y, z), val);
    }

    pub fn is_all_air(&self) -> bool {
        self.non_air_count == 0
    }

    pub fn is_trivially_empty(&self) -> bool {
        self.non_air_count == 0
            && self.block_light.iter().all(|&b| b == 0)
            && self.sky_light.iter().all(|&b| b == 0xFF)
    }
}

#[inline]
pub fn get_nibble(buf: &[u8], idx: usize) -> u8 {
    let b = buf[idx >> 1];
    if idx & 1 == 0 {
        b & 0x0F
    } else {
        (b >> 4) & 0x0F
    }
}

#[inline]
pub fn set_nibble(buf: &mut [u8], idx: usize, val: u8) {
    let v = val & 0x0F;
    let byte = &mut buf[idx >> 1];
    if idx & 1 == 0 {
        *byte = (*byte & 0xF0) | v;
    } else {
        *byte = (*byte & 0x0F) | (v << 4);
    }
}
