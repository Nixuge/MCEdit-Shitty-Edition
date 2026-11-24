use crate::anvil::region::RegionFile;
use ahash::AHashMap;
use glam::{DVec3, IVec3};
use mcedit_core::profiler::{self, Counter, Stage};
use mcedit_core::section::{LIGHT_BYTES, SECTION_VOLUME};
use mcedit_core::{
    BiomeStorage, BlockRegistry, BlockState, BlockStateId, Chunk, ChunkPos, ChunkSection,
    DimensionId, FormatError, FormatResult, LodChunkSurface, LodChunkVoxels, PlayerData,
    RepairReport, WorldFormat, WorldMetadata, lod_non_opaque_priority,
};
use mcedit_nbt::{
    Compression, Endianness, NbtCompound, NbtList, NbtTag, read_nbt, read_nbt_filtered, write_nbt,
};
use parking_lot::{Mutex, RwLock};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION_ANVIL: i32 = 19133;
/// Java 1.13 snapshots start at DataVersion 1451 (1.13 release is 1519).
pub const MIN_POST113_DATA_VERSION: i32 = 1451;
/// Java 1.15 (19w36a) switched `Level.Biomes` from 256 2D ints to 1024 3D (`4x4x64`) ints.
pub const BIOMES_3D_1024_DATA_VERSION: i32 = 2203;
/// Java 1.16 (20w17a) switched section `BlockStates` `LongArray` from unpadded (spanning 64-bit
/// boundaries) to padded (entries never cross 64-bit word boundaries).
pub const PADDED_BITPACK_DATA_VERSION: i32 = 2529;
/// Java 1.17 moved chunk entities into separate `entities/r.<rx>.<rz>.mca` region files.
pub const SEPARATE_ENTITIES_DATA_VERSION: i32 = 2724;
/// Java 1.18 removed the `Level` wrapper compound, introduced `sections[].block_states.{palette,data}`
/// and `sections[].biomes.{palette,data}`, and expanded Overworld height to `Y = -64..320` (`sections -4..=19`).
pub const CAVES_AND_CLIFFS_DATA_VERSION: i32 = 2844;
/// Minecraft 26.3+ (`DataVersion >= 5000`) encodes `sections[].block_states.palette` using
/// `Codec.either(String, Compound)`: a `TAG_List<TAG_String>` when no block in the section has
/// properties, or a `TAG_List<TAG_Compound>` with `{ "": "<id>" }` for property-less blocks and
/// `{ "id": "<id>", "properties": { ... } }` for blocks with properties.
pub const PALETTE_26_3_DATA_VERSION: i32 = 5000;
/// Default DataVersion when creating a new modern post-1.13 Anvil world (Java 1.20.4).
pub const DEFAULT_MODERN_DATA_VERSION: i32 = 3700;

type RegionCacheMap = AHashMap<(DimensionId, i32, i32), Arc<RwLock<RegionFile>>>;

/// Packs `indices` into a 64-bit `LongArray` (`Vec<i64>`) using either:
/// - **Padded** (`padded = true`, Java 1.16+): $\lfloor 64 / \text{bits} \rfloor$ values per `u64`, never crossing word boundaries.
/// - **Unpadded** (`padded = false`, Java 1.13–1.15): continuous bitstream where values can straddle two consecutive `u64` words.
pub fn pack_bit_indices(
    indices: &[u16],
    palette_len: usize,
    min_bits: usize,
    padded: bool,
) -> Vec<i64> {
    if palette_len <= 1 || indices.is_empty() {
        return Vec::new();
    }
    let bits = (palette_len.next_power_of_two().trailing_zeros() as usize).max(min_bits);
    let mask = (1u64 << bits) - 1;

    if padded {
        let values_per_long = 64 / bits;
        let num_longs = indices.len().div_ceil(values_per_long);
        let mut out = vec![0u64; num_longs];
        for (i, &val) in indices.iter().enumerate() {
            let long_idx = i / values_per_long;
            let bit_off = (i % values_per_long) * bits;
            out[long_idx] |= ((val as u64) & mask) << bit_off;
        }
        out.into_iter().map(|w| w as i64).collect()
    } else {
        let total_bits = indices.len() * bits;
        let num_longs = total_bits.div_ceil(64);
        let mut out = vec![0u64; num_longs];
        for (i, &val) in indices.iter().enumerate() {
            let v = (val as u64) & mask;
            let start_bit = i * bits;
            let start_long = start_bit >> 6;
            let end_long = ((i + 1) * bits - 1) >> 6;
            let bit_off = start_bit & 63;
            out[start_long] |= v << bit_off;
            if end_long != start_long {
                let low_bits = 64 - bit_off;
                out[end_long] |= v >> low_bits;
            }
        }
        out.into_iter().map(|w| w as i64).collect()
    }
}

/// Unpacks `count` palette indices from a 64-bit `LongArray` (`data`) supporting both
/// padded (1.16+) and unpadded (1.13–1.15) bit packing.
pub fn unpack_bit_indices(
    data: &[i64],
    count: usize,
    palette_len: usize,
    min_bits: usize,
    padded: bool,
) -> Vec<u16> {
    if palette_len <= 1 || data.is_empty() {
        return vec![0u16; count];
    }
    let mut bits = (palette_len.next_power_of_two().trailing_zeros() as usize).max(min_bits);

    if padded {
        let expected = count.div_ceil(64 / bits);
        if data.len() != expected {
            for candidate_b in min_bits..=15 {
                if count.div_ceil(64 / candidate_b) == data.len() {
                    bits = candidate_b;
                    break;
                }
            }
        }
        let values_per_long = 64 / bits;
        let mask = (1u64 << bits) - 1;
        let mut out = vec![0u16; count];
        let max_idx = (palette_len - 1) as u16;
        for (i, slot) in out.iter_mut().enumerate() {
            let long_idx = i / values_per_long;
            if let Some(&word) = data.get(long_idx) {
                let bit_off = (i % values_per_long) * bits;
                let idx = (((word as u64) >> bit_off) & mask) as u16;
                *slot = idx.min(max_idx);
            }
        }
        out
    } else {
        let expected = (count * bits).div_ceil(64);
        if data.len() != expected && (data.len() * 64).is_multiple_of(count) {
            let inferred = (data.len() * 64) / count;
            if (min_bits..=15).contains(&inferred) {
                bits = inferred;
            }
        }
        let mask = (1u64 << bits) - 1;
        let mut out = vec![0u16; count];
        let max_idx = (palette_len - 1) as u16;
        for (i, slot) in out.iter_mut().enumerate() {
            let start_bit = i * bits;
            let start_long = start_bit >> 6;
            let end_long = ((i + 1) * bits - 1) >> 6;
            let bit_off = start_bit & 63;
            let Some(&w0) = data.get(start_long) else {
                break;
            };
            let raw = if start_long == end_long {
                ((w0 as u64) >> bit_off) & mask
            } else {
                let w1 = data.get(end_long).copied().unwrap_or(0) as u64;
                let low_bits = 64 - bit_off;
                (((w0 as u64) >> bit_off) | (w1 << low_bits)) & mask
            };
            *slot = (raw as u16).min(max_idx);
        }
        out
    }
}

/// Unpacks `out.len()` palette indices directly into `out` without heap allocation.
fn unpack_bit_indices_into(
    data: &[i64],
    out: &mut [u16],
    palette_len: usize,
    min_bits: usize,
    padded: bool,
) {
    let count = out.len();
    if palette_len <= 1 || data.is_empty() {
        out.fill(0);
        return;
    }
    let mut bits = (palette_len.next_power_of_two().trailing_zeros() as usize).max(min_bits);
    let max_idx = (palette_len - 1) as u16;

    if padded {
        let expected = count.div_ceil(64 / bits);
        if data.len() != expected {
            for candidate_b in min_bits..=15 {
                if count.div_ceil(64 / candidate_b) == data.len() {
                    bits = candidate_b;
                    break;
                }
            }
        }
        let values_per_long = 64 / bits;
        let mask = (1u64 << bits) - 1;
        let mut cell_i = 0usize;
        for &word_i64 in data {
            let mut word = word_i64 as u64;
            let limit = (cell_i + values_per_long).min(count);
            while cell_i < limit {
                out[cell_i] = ((word & mask) as u16).min(max_idx);
                word >>= bits;
                cell_i += 1;
            }
            if cell_i >= count {
                break;
            }
        }
        if cell_i < count {
            out[cell_i..].fill(0);
        }
    } else {
        let expected = (count * bits).div_ceil(64);
        if data.len() != expected && (data.len() * 64).is_multiple_of(count) {
            let inferred = (data.len() * 64) / count;
            if (min_bits..=15).contains(&inferred) {
                bits = inferred;
            }
        }
        let mask = (1u64 << bits) - 1;
        for (i, slot) in out.iter_mut().enumerate() {
            let start_bit = i * bits;
            let start_long = start_bit >> 6;
            let end_long = ((i + 1) * bits - 1) >> 6;
            let bit_off = start_bit & 63;
            let Some(&w0) = data.get(start_long) else {
                *slot = 0;
                continue;
            };
            let raw = if start_long == end_long {
                ((w0 as u64) >> bit_off) & mask
            } else {
                let w1 = data.get(end_long).copied().unwrap_or(0) as u64;
                let low_bits = 64 - bit_off;
                (((w0 as u64) >> bit_off) | (w1 << low_bits)) & mask
            };
            *slot = (raw as u16).min(max_idx);
        }
    }
}

/// Fused single-pass bit-unpack + palette remap directly into `dest_indices` (`len == 4096`),
/// returning the total `non_air_count` without allocating an intermediate `Vec<u16>`.
fn unpack_and_remap_section(
    data: &[i64],
    pal_remap: &[u16],
    dest_indices: &mut [u16],
    palette_len: usize,
    padded: bool,
) -> u16 {
    if palette_len <= 1 || data.is_empty() {
        return 0;
    }
    let count = dest_indices.len();
    let mut bits = (palette_len.next_power_of_two().trailing_zeros() as usize).max(4);
    let max_idx = palette_len - 1;
    let mut non_air = 0u16;

    if padded {
        let expected = count.div_ceil(64 / bits);
        if data.len() != expected {
            for candidate_b in 4..=15 {
                if count.div_ceil(64 / candidate_b) == data.len() {
                    bits = candidate_b;
                    break;
                }
            }
        }
        let values_per_long = 64 / bits;
        let mask = (1u64 << bits) - 1;
        let mut cell_i = 0usize;
        for &word_i64 in data {
            let mut word = word_i64 as u64;
            let limit = (cell_i + values_per_long).min(count);
            while cell_i < limit {
                let raw_idx = ((word & mask) as usize).min(max_idx);
                word >>= bits;
                let mapped = pal_remap[raw_idx];
                if mapped != 0 {
                    dest_indices[cell_i] = mapped;
                    non_air = non_air.saturating_add(1);
                }
                cell_i += 1;
            }
            if cell_i >= count {
                break;
            }
        }
    } else {
        let expected = (count * bits).div_ceil(64);
        if data.len() != expected && (data.len() * 64).is_multiple_of(count) {
            let inferred = (data.len() * 64) / count;
            if (4..=15).contains(&inferred) {
                bits = inferred;
            }
        }
        let mask = (1u64 << bits) - 1;
        for (i, slot) in dest_indices.iter_mut().enumerate() {
            let start_bit = i * bits;
            let start_long = start_bit >> 6;
            let end_long = ((i + 1) * bits - 1) >> 6;
            let bit_off = start_bit & 63;
            let Some(&w0) = data.get(start_long) else {
                break;
            };
            let raw = if start_long == end_long {
                ((w0 as u64) >> bit_off) & mask
            } else {
                let w1 = data.get(end_long).copied().unwrap_or(0) as u64;
                let low_bits = 64 - bit_off;
                (((w0 as u64) >> bit_off) | (w1 << low_bits)) & mask
            };
            let mapped = pal_remap[(raw as usize).min(max_idx)];
            if mapped != 0 {
                *slot = mapped;
                non_air = non_air.saturating_add(1);
            }
        }
    }
    non_air
}

pub struct AnvilPost113Format {
    root: PathBuf,
    data_version: i32,
    min_sec_y: i32,
    max_sec_y: i32,
    region_cache: RwLock<RegionCacheMap>,
    entity_region_cache: RwLock<RegionCacheMap>,
    session_timestamp: Mutex<i64>,
}

impl AnvilPost113Format {
    pub fn open(path: impl AsRef<Path>, _reg: &BlockRegistry) -> FormatResult<Self> {
        let root = resolve_world_dir(path.as_ref())?;
        let level_dat = root.join("level.dat");
        if !level_dat.exists() {
            return Err(FormatError::UnsupportedFormat(root.display().to_string()));
        }
        if root.join("chunks.dat").exists() || root.join("db").is_dir() {
            return Err(FormatError::UnsupportedFormat(root.display().to_string()));
        }

        let (_, root_nbt) = mcedit_nbt::read_nbt_file(&level_dat, Endianness::Big)?;
        let data = root_nbt
            .get_compound("Data")
            .ok_or_else(|| FormatError::Corrupt("level.dat missing Data compound".to_string()))?;

        let version = data.get_int("version").unwrap_or(VERSION_ANVIL);
        if version != VERSION_ANVIL {
            return Err(FormatError::UnsupportedVersion(format!(
                "expected Anvil version {VERSION_ANVIL}, found {version}"
            )));
        }

        let data_version = data
            .get_compound("Version")
            .and_then(|v| v.get_int("Id"))
            .or_else(|| data.get_int("DataVersion"))
            .unwrap_or(0);

        if data_version <= MIN_POST113_DATA_VERSION {
            return Err(FormatError::UnsupportedVersion(format!(
                "DataVersion {data_version} is pre-1.13; use AnvilPre113Format"
            )));
        }

        let (min_sec_y, max_sec_y) = if data_version >= CAVES_AND_CLIFFS_DATA_VERSION {
            (-4, 19) // Y = -64..320 (384 blocks tall)
        } else {
            (0, 15) // Y = 0..256 (256 blocks tall)
        };

        Ok(Self {
            root,
            data_version,
            min_sec_y,
            max_sec_y,
            region_cache: RwLock::new(AHashMap::new()),
            entity_region_cache: RwLock::new(AHashMap::new()),
            session_timestamp: Mutex::new(0),
        })
    }

    pub fn create_new(
        path: impl AsRef<Path>,
        level_name: &str,
        seed: i64,
        game_type: i32,
        spawn_pos: IVec3,
        data_version: i32,
        reg: &BlockRegistry,
    ) -> FormatResult<Self> {
        let dv = data_version.max(MIN_POST113_DATA_VERSION + 1);
        let root = path.as_ref().to_path_buf();
        fs::create_dir_all(root.join("region"))?;
        if dv >= SEPARATE_ENTITIES_DATA_VERSION {
            fs::create_dir_all(root.join("entities"))?;
        }

        let mut data = NbtCompound::new();
        data.insert("LevelName", level_name.to_string());
        data.insert("RandomSeed", seed);
        data.insert("DataVersion", dv);
        data.insert("Time", 0i64);
        data.insert("DayTime", 1000i64);
        data.insert("GameType", game_type);
        data.insert("SpawnX", spawn_pos.x);
        data.insert("SpawnY", spawn_pos.y);
        data.insert("SpawnZ", spawn_pos.z);
        data.insert("version", VERSION_ANVIL);
        data.insert("initialized", 1i8);
        data.insert("LastPlayed", now_millis());

        let mut wgs = NbtCompound::new();
        wgs.insert("seed", seed);
        wgs.insert("generate_features", 1i8);
        data.insert("WorldGenSettings", wgs);

        let mut ver = NbtCompound::new();
        ver.insert("Id", dv);
        ver.insert(
            "Name",
            if dv >= CAVES_AND_CLIFFS_DATA_VERSION {
                "1.20.4".to_string()
            } else {
                "1.16.5".to_string()
            },
        );
        ver.insert("Snapshot", 0i8);
        data.insert("Version", ver);

        let mut root_nbt = NbtCompound::new();
        root_nbt.insert("Data", data);

        mcedit_nbt::write_nbt_file(
            root.join("level.dat"),
            "",
            &root_nbt,
            Endianness::Big,
            Compression::Gzip,
        )?;

        let fmt = Self::open(&root, reg)?;
        fmt.acquire_session_lock()?;
        Ok(fmt)
    }

    pub fn data_version(&self) -> i32 {
        self.data_version
    }

    fn is_padded_bitpack(&self, chunk_dv: i32) -> bool {
        chunk_dv >= PADDED_BITPACK_DATA_VERSION
    }

    fn is_118_layout(&self, chunk_dv: i32, root: &NbtCompound) -> bool {
        chunk_dv >= CAVES_AND_CLIFFS_DATA_VERSION || !root.contains_key("Level")
    }

    fn uses_separate_entities(&self, chunk_dv: i32) -> bool {
        chunk_dv >= SEPARATE_ENTITIES_DATA_VERSION
    }

    fn dim_root_dir(&self, dim: DimensionId) -> PathBuf {
        // Support Minecraft 26.x+ (`dimensions/minecraft/<dim_name>/`) as well as 1.13–1.21.4 (`<root>`, `DIM-1`, `DIM1`)
        let modern_dim = match dim {
            DimensionId::Overworld => self.root.join("dimensions").join("minecraft").join("overworld"),
            DimensionId::Nether => self.root.join("dimensions").join("minecraft").join("the_nether"),
            DimensionId::End => self.root.join("dimensions").join("minecraft").join("the_end"),
            DimensionId::Custom(id) => self.root.join("dimensions").join("minecraft").join(format!("dim_{id}")),
        };
        if modern_dim.is_dir() || self.root.join("dimensions").join("minecraft").is_dir() {
            return modern_dim;
        }
        match dim {
            DimensionId::Overworld => self.root.clone(),
            DimensionId::Nether => self.root.join("DIM-1"),
            DimensionId::End => self.root.join("DIM1"),
            DimensionId::Custom(id) => self.root.join(format!("DIM{id}")),
        }
    }

    fn region_dir(&self, dim: DimensionId) -> PathBuf {
        self.dim_root_dir(dim).join("region")
    }

    fn entities_dir(&self, dim: DimensionId) -> PathBuf {
        self.dim_root_dir(dim).join("entities")
    }

    fn region_path(&self, dim: DimensionId, rx: i32, rz: i32) -> PathBuf {
        self.region_dir(dim).join(format!("r.{rx}.{rz}.mca"))
    }

    fn entity_region_path(&self, dim: DimensionId, rx: i32, rz: i32) -> PathBuf {
        self.entities_dir(dim).join(format!("r.{rx}.{rz}.mca"))
    }

    fn get_cached_region(
        &self,
        dim: DimensionId,
        rx: i32,
        rz: i32,
        create_if_missing: bool,
    ) -> FormatResult<Option<Arc<RwLock<RegionFile>>>> {
        let key = (dim, rx, rz);
        if let Some(rf) = self.region_cache.read().get(&key) {
            return Ok(Some(rf.clone()));
        }
        let rpath = self.region_path(dim, rx, rz);
        if !create_if_missing && !rpath.exists() {
            return Ok(None);
        }
        if create_if_missing && let Some(parent) = rpath.parent() {
            fs::create_dir_all(parent)?;
        }
        let rf = Arc::new(RwLock::new(RegionFile::open_or_create(&rpath)?));
        let mut cache = self.region_cache.write();
        Ok(Some(cache.entry(key).or_insert(rf).clone()))
    }

    fn get_cached_entity_region(
        &self,
        dim: DimensionId,
        rx: i32,
        rz: i32,
        create_if_missing: bool,
    ) -> FormatResult<Option<Arc<RwLock<RegionFile>>>> {
        let key = (dim, rx, rz);
        if let Some(rf) = self.entity_region_cache.read().get(&key) {
            return Ok(Some(rf.clone()));
        }
        let rpath = self.entity_region_path(dim, rx, rz);
        if !create_if_missing && !rpath.exists() {
            return Ok(None);
        }
        if create_if_missing && let Some(parent) = rpath.parent() {
            fs::create_dir_all(parent)?;
        }
        let rf = Arc::new(RwLock::new(RegionFile::open_or_create(&rpath)?));
        let mut cache = self.entity_region_cache.write();
        Ok(Some(cache.entry(key).or_insert(rf).clone()))
    }

    fn load_separate_entities(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
    ) -> FormatResult<Vec<NbtCompound>> {
        let (rx, rz) = pos.region_pos();
        let Some(erf_arc) = self.get_cached_entity_region(dim, rx, rz, false)? else {
            return Ok(Vec::new());
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = erf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(Vec::new());
        };
        let (_, mut root) = read_nbt(&raw, Endianness::Big)?;
        if let Some(NbtTag::List(ents)) = root.remove("Entities") {
            return Ok(ents
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect());
        }
        Ok(Vec::new())
    }

    fn save_separate_entities(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        entities: &[NbtCompound],
    ) -> FormatResult<()> {
        let (rx, rz) = pos.region_pos();
        let create = !entities.is_empty();
        let Some(erf_arc) = self.get_cached_entity_region(dim, rx, rz, create)? else {
            return Ok(());
        };
        let mut root = NbtCompound::new();
        root.insert("DataVersion", self.data_version);
        root.insert("Position", NbtTag::IntArray(vec![pos.cx(), pos.cz()]));
        let list = NbtList::from_items(
            entities
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        root.insert("Entities", list);
        let raw = write_nbt("", &root, Endianness::Big, Compression::None)?;
        let (lx, lz) = pos.region_local();
        erf_arc.write().write_chunk_nbt(lx, lz, &raw)?;
        Ok(())
    }

    fn decode_chunk_nbt<F>(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        mut root: NbtCompound,
        readonly_fast: bool,
        mut resolve_state: F,
    ) -> FormatResult<Chunk>
    where
        F: FnMut(BlockState) -> BlockStateId,
    {
        let chunk_dv = root.get_int("DataVersion").unwrap_or(self.data_version);
        let padded = self.is_padded_bitpack(chunk_dv);
        let is_118 = self.is_118_layout(chunk_dv, &root);

        let mut container = if is_118 {
            root
        } else {
            match root.remove("Level") {
                Some(NbtTag::Compound(l)) => l,
                _ => return Err(FormatError::Corrupt("chunk missing Level compound".to_string())),
            }
        };

        let y_pos = container.get_int("yPos").unwrap_or(self.min_sec_y);
        let min_sy = self.min_sec_y.min(y_pos);
        let mut chunk = Chunk::new(pos, min_sy, self.max_sec_y);
        chunk.terrain_populated = true;

        // 1. Biomes (skip during readonly background meshing)
        if !readonly_fast {
            if let Some(NbtTag::IntArray(biomes_int)) = container.remove("Biomes") {
                if biomes_int.len() >= 1024 {
                    let mut map = BTreeMap::new();
                    for sy in 0..16i32 {
                        let mut cells = [1u8; 64];
                        let base = (sy as usize) * 64;
                        for (i, cell) in cells.iter_mut().enumerate() {
                            *cell = (biomes_int[base + i] & 0xFF) as u8;
                        }
                        map.insert(sy, cells);
                    }
                    chunk.biomes = BiomeStorage::Cells3D(map);
                } else if biomes_int.len() >= 256 {
                    let mut cols = [1u8; 256];
                    for (i, &b) in biomes_int[..256].iter().enumerate() {
                        cols[i] = (b & 0xFF) as u8;
                    }
                    chunk.biomes = BiomeStorage::Columns2D(cols);
                }
            } else if let Some(NbtTag::ByteArray(biomes_byte)) = container.remove("Biomes")
                && biomes_byte.len() >= 256
            {
                let mut cols = [1u8; 256];
                for (i, &b) in biomes_byte[..256].iter().enumerate() {
                    cols[i] = b as u8;
                }
                chunk.biomes = BiomeStorage::Columns2D(cols);
            }
        }

        // 2. Entities (inline 1.13–1.16.5 or separate 1.17+ entities/*.mca; skip separate disk open on readonly_fast)
        let inline_ents = container
            .remove("Entities")
            .or_else(|| container.remove("entities"));
        if let Some(NbtTag::List(ents)) = inline_ents {
            chunk.entities = ents
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }
        if !readonly_fast && chunk.entities.is_empty() && self.uses_separate_entities(chunk_dv) {
            chunk.entities = self.load_separate_entities(dim, pos).unwrap_or_default();
        }

        // 3. Block Entities (`block_entities` in 1.18+, `TileEntities` in 1.13–1.17)
        let bes_tag = container
            .remove("block_entities")
            .or_else(|| container.remove("TileEntities"));
        if let Some(NbtTag::List(bes)) = bes_tag {
            chunk.block_entities = bes
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }

        // 4. Tile Ticks (`block_ticks` in 1.18+, `TileTicks` in 1.13–1.17)
        if !readonly_fast {
            let tts_tag = container
                .remove("block_ticks")
                .or_else(|| container.remove("TileTicks"));
            if let Some(NbtTag::List(tts)) = tts_tag {
                chunk.tile_ticks = tts
                    .items()
                    .iter()
                    .filter_map(|t| match t {
                        NbtTag::Compound(c) => Some(c.clone()),
                        _ => None,
                    })
                    .collect();
            }
        }

        // 5. Sections (`sections` in 1.18+, `Sections` in 1.13–1.17)
        let secs_tag = container
            .remove("sections")
            .or_else(|| container.remove("Sections"));
        let mut cells_3d_biomes: BTreeMap<i32, [u8; 64]> = if readonly_fast {
            BTreeMap::new()
        } else {
            match &chunk.biomes {
                BiomeStorage::Cells3D(m) => m.clone(),
                _ => BTreeMap::new(),
            }
        };
        let mut has_118_biomes = false;

        if let Some(NbtTag::List(sec_list)) = secs_tag {
            for item in sec_list.items() {
                let NbtTag::Compound(sec_nbt) = item else {
                    continue;
                };
                let sy = parse_section_y(sec_nbt);
                if sy < chunk.min_section_y {
                    chunk.min_section_y = sy;
                }
                if sy > chunk.max_section_y {
                    chunk.max_section_y = sy;
                }

                // Decode 1.18+ 3D 4x4x4 paletted section biomes (`sections[].biomes`) when not readonly_fast
                if !readonly_fast
                    && let Some(biomes_comp) = sec_nbt.get_compound("biomes")
                    && let Some(pal_list) = biomes_comp.get_list("palette")
                {
                    let biome_ids: Vec<u8> = pal_list
                        .items()
                        .iter()
                        .map(|t| match t {
                            NbtTag::String(s) => biome_name_to_id(s),
                            _ => 1,
                        })
                        .collect();
                    if !biome_ids.is_empty() {
                        has_118_biomes = true;
                        let mut sec_biomes = [biome_ids[0]; 64];
                        if biome_ids.len() > 1
                            && let Some(data) = biomes_comp.get_long_array("data")
                        {
                            let unpacked =
                                unpack_bit_indices(data, 64, biome_ids.len(), 1, true);
                            for (i, &pidx) in unpacked.iter().enumerate() {
                                sec_biomes[i] = biome_ids
                                    .get(pidx as usize)
                                    .copied()
                                    .unwrap_or(biome_ids[0]);
                            }
                        }
                        cells_3d_biomes.insert(sy, sec_biomes);
                    }
                }

                // Decode section block palette & bit-packed LongArray
                let (pal_opt, data_opt) = extract_section_palette_and_data(sec_nbt);
                let mut section = ChunkSection::new_empty();
                let mut has_blocks = false;

                if let Some(pal_list) = pal_opt {
                    let mut pal_ids: Vec<BlockStateId> = Vec::with_capacity(pal_list.len());
                    for entry in pal_list.items() {
                        if let Some(st) = parse_palette_entry(entry) {
                            pal_ids.push(resolve_state(st));
                        } else {
                            pal_ids.push(BlockStateId::AIR);
                        }
                    }

                    if pal_ids.len() == 1 {
                        let single = pal_ids[0];
                        if single != BlockStateId::AIR {
                            has_blocks = true;
                            section.palette = vec![BlockStateId::AIR, single];
                            section.indices.fill(1u16);
                            section.non_air_count = SECTION_VOLUME as u16;
                        }
                    } else if pal_ids.len() > 1
                        && let Some(longs) = data_opt
                    {
                        // Build compact section palette BEFORE unpacking so all-air sections skip bit unpacking
                        let mut section_pal = vec![BlockStateId::AIR];
                        let mut pal_remap = vec![0u16; pal_ids.len()];
                        for (pi, &bid) in pal_ids.iter().enumerate() {
                            if bid == BlockStateId::AIR {
                                pal_remap[pi] = 0;
                            } else if let Some(existing) =
                                section_pal.iter().position(|&b| b == bid)
                            {
                                pal_remap[pi] = existing as u16;
                            } else {
                                let new_idx = section_pal.len() as u16;
                                section_pal.push(bid);
                                pal_remap[pi] = new_idx;
                            }
                        }

                        if section_pal.len() > 1 {
                            let non_air = unpack_and_remap_section(
                                longs,
                                &pal_remap,
                                &mut section.indices,
                                pal_ids.len(),
                                padded,
                            );
                            section.palette = section_pal;
                            section.non_air_count = non_air;
                            has_blocks = non_air > 0;
                        }
                    }
                }

                let mut has_light = false;
                if let Some(bl) = sec_nbt.get_byte_array("BlockLight")
                    && bl.len() == LIGHT_BYTES
                {
                    for (i, &v) in bl.iter().enumerate() {
                        section.block_light[i] = v as u8;
                    }
                    has_light = true;
                }
                if let Some(sl) = sec_nbt.get_byte_array("SkyLight")
                    && sl.len() == LIGHT_BYTES
                {
                    for (i, &v) in sl.iter().enumerate() {
                        section.sky_light[i] = v as u8;
                    }
                    has_light = true;
                }

                if has_blocks || has_light {
                    chunk.sections.insert(sy, section);
                }
            }
        }

        if has_118_biomes {
            chunk.biomes = BiomeStorage::Cells3D(cells_3d_biomes);
        }

        // Fast top-down section heightmap calculation (zero BTreeMap lookups!)
        let min_y = chunk.min_y();
        chunk.heightmap.fill(min_y);
        let mut found = [false; 256];
        let mut remaining = 256usize;
        for (&sy, sec) in chunk.sections.iter().rev() {
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
                        if !found[col] && sec.indices[yz_off | x] != 0 {
                            chunk.heightmap[col] = wy;
                            found[col] = true;
                            remaining -= 1;
                            if remaining == 0 {
                                break;
                            }
                        }
                    }
                    if remaining == 0 {
                        break;
                    }
                }
                if remaining == 0 {
                    break;
                }
            }
            if remaining == 0 {
                break;
            }
        }

        if !readonly_fast {
            chunk.extra_nbt = container;
        }
        chunk.dirty = false;
        Ok(chunk)
    }
}

impl WorldFormat for AnvilPost113Format {
    fn format_name(&self) -> &'static str {
        if self.data_version >= CAVES_AND_CLIFFS_DATA_VERSION {
            "Java Anvil (1.18-1.21+)"
        } else {
            "Java Anvil (1.13-1.17.1)"
        }
    }

    fn root_path(&self) -> &Path {
        &self.root
    }

    fn min_section_y(&self) -> i32 {
        self.min_sec_y
    }

    fn max_section_y(&self) -> i32 {
        self.max_sec_y
    }

    fn dimensions(&self) -> Vec<DimensionId> {
        let mut dims = vec![DimensionId::Overworld];
        if self
            .root
            .join("dimensions")
            .join("minecraft")
            .join("the_nether")
            .is_dir()
        {
            dims.push(DimensionId::Nether);
        }
        if self
            .root
            .join("dimensions")
            .join("minecraft")
            .join("the_end")
            .is_dir()
        {
            dims.push(DimensionId::End);
        }
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && entry.path().is_dir()
                    && let Some(num_str) = name.strip_prefix("DIM")
                    && let Ok(id) = num_str.parse::<i32>()
                    && id != 0
                {
                    let d = DimensionId::from_id(id);
                    if !dims.contains(&d) {
                        dims.push(d);
                    }
                }
            }
        }
        dims
    }

    fn load_metadata(&self) -> FormatResult<WorldMetadata> {
        let (_, raw_nbt) =
            mcedit_nbt::read_nbt_file(self.root.join("level.dat"), Endianness::Big)?;
        let data = raw_nbt
            .get_compound("Data")
            .ok_or_else(|| FormatError::Corrupt("level.dat missing Data".to_string()))?;

        let data_version = data
            .get_compound("Version")
            .and_then(|v| v.get_int("Id"))
            .or_else(|| data.get_int("DataVersion"))
            .or(Some(self.data_version));

        let mut seed = data
            .get_long("RandomSeed")
            .or_else(|| {
                data.get_compound("WorldGenSettings")
                    .and_then(|w| w.get_long("seed"))
            })
            .unwrap_or(0);
        if seed == 0 {
            let wgs_path = self
                .root
                .join("data")
                .join("minecraft")
                .join("world_gen_settings.dat");
            if let Ok((_, wgs_nbt)) = mcedit_nbt::read_nbt_file(&wgs_path, Endianness::Big)
                && let Some(s) = wgs_nbt
                    .get_compound("data")
                    .and_then(|d| d.get_long("seed"))
            {
                seed = s;
            }
        }

        let spawn_pos = if let Some(spawn_comp) = data.get_compound("spawn")
            && let Some(pos_arr) = spawn_comp.get_int_array("pos")
            && pos_arr.len() >= 3
        {
            IVec3::new(pos_arr[0], pos_arr[1], pos_arr[2])
        } else {
            IVec3::new(
                data.get_int("SpawnX").unwrap_or(0),
                data.get_int("SpawnY").unwrap_or(64),
                data.get_int("SpawnZ").unwrap_or(0),
            )
        };

        let mut meta = WorldMetadata {
            level_name: data.get_string("LevelName").unwrap_or("World").to_string(),
            seed,
            time: data.get_long("Time").unwrap_or(0),
            day_time: data.get_long("DayTime").unwrap_or(0),
            game_type: data.get_int("GameType").unwrap_or(1),
            spawn_pos,
            data_version,
            format_name: self.format_name().to_string(),
            raw_nbt,
        };
        meta.sync_from_nbt();
        Ok(meta)
    }

    fn save_metadata(&self, meta: &WorldMetadata) -> FormatResult<()> {
        let mut m = meta.clone();
        m.sync_to_nbt();
        mcedit_nbt::write_nbt_file(
            self.root.join("level.dat"),
            "",
            &m.raw_nbt,
            Endianness::Big,
            Compression::Gzip,
        )?;
        Ok(())
    }

    fn list_chunks(&self, dim: DimensionId) -> FormatResult<Vec<ChunkPos>> {
        let rdir = self.region_dir(dim);
        if !rdir.is_dir() {
            return Ok(Vec::new());
        }
        let mut chunks = Vec::new();
        for entry in fs::read_dir(&rdir)?.flatten() {
            let path = entry.path();
            if let Some(fname) = path.file_name().and_then(|n| n.to_str())
                && let Some((rx, rz)) = parse_region_filename(fname)
                && let Ok(rf) = RegionFile::open_or_create(&path)
            {
                for (lx, lz) in rf.list_present() {
                    chunks.push(ChunkPos::new(rx * 32 + lx as i32, rz * 32 + lz as i32));
                }
            }
        }
        Ok(chunks)
    }

    fn list_regions(&self, dim: DimensionId) -> FormatResult<Vec<(i32, i32)>> {
        let rdir = self.region_dir(dim);
        if !rdir.is_dir() {
            return Ok(Vec::new());
        }
        let mut regs = Vec::new();
        for entry in fs::read_dir(&rdir)?.flatten() {
            let path = entry.path();
            if let Some(fname) = path.file_name().and_then(|n| n.to_str())
                && let Some((rx, rz)) = parse_region_filename(fname)
            {
                regs.push((rx, rz));
            }
        }
        Ok(regs)
    }

    fn load_chunk(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &mut BlockRegistry,
    ) -> FormatResult<Option<Chunk>> {
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(None);
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = rf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(None);
        };
        profiler::add(Counter::UncompressedNbtBytes, raw.len() as u64);
        let (_, root) = {
            let _p = profiler::scope(Stage::NbtParse);
            read_nbt(&raw, Endianness::Big)?
        };
        profiler::add(Counter::ChunksDiskLoaded, 1);
        let _d = profiler::scope(Stage::ChunkDecode);
        let chunk = self.decode_chunk_nbt(dim, pos, root, false, |st| registry.intern(st))?;
        Ok(Some(chunk))
    }

    fn load_chunk_readonly(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<Chunk>> {
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(None);
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = rf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(None);
        };
        profiler::add(Counter::UncompressedNbtBytes, raw.len() as u64);
        let (_, root) = {
            let _p = profiler::scope(Stage::NbtParse);
            read_nbt_filtered(&raw, Endianness::Big, |k| {
                matches!(
                    k,
                    "DataVersion"
                        | "yPos"
                        | "Level"
                        | "sections"
                        | "Sections"
                        | "block_entities"
                        | "TileEntities"
                        | "Entities"
                        | "entities"
                )
            })?
        };
        profiler::add(Counter::ChunksDiskLoaded, 1);
        let _d = profiler::scope(Stage::ChunkDecode);
        let fallback_stone = registry.find_by_legacy(1, 0).unwrap_or(BlockStateId::AIR);
        let chunk = self.decode_chunk_nbt(dim, pos, root, true, |st| {
            registry.resolve_readonly(&st).unwrap_or_else(|| {
                if st.name.ends_with("air") {
                    BlockStateId::AIR
                } else {
                    fallback_stone
                }
            })
        })?;
        Ok(Some(chunk))
    }

    fn load_chunk_with_registry(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &RwLock<BlockRegistry>,
    ) -> FormatResult<Option<Chunk>> {
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(None);
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = rf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(None);
        };
        profiler::add(Counter::UncompressedNbtBytes, raw.len() as u64);
        let (_, root) = {
            let _p = profiler::scope(Stage::NbtParse);
            read_nbt_filtered(&raw, Endianness::Big, |k| {
                matches!(
                    k,
                    "DataVersion"
                        | "yPos"
                        | "Level"
                        | "sections"
                        | "Sections"
                        | "block_entities"
                        | "TileEntities"
                )
            })?
        };
        profiler::add(Counter::ChunksDiskLoaded, 1);
        let _d = profiler::scope(Stage::ChunkDecode);
        let chunk = self.decode_chunk_nbt(dim, pos, root, true, |st| {
            if let Some(id) = registry.read().find_exact(&st) {
                return id;
            }
            registry.write().intern(st)
        })?;
        Ok(Some(chunk))
    }

    fn load_chunk_lod_surface(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<LodChunkSurface>> {
        let Some(chunk) = self.load_chunk_readonly(dim, pos, registry)? else {
            return Ok(None);
        };
        Ok(Some(LodChunkSurface::from_chunk(&chunk, registry)))
    }

    fn load_chunk_entities(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
    ) -> FormatResult<Vec<NbtCompound>> {
        if self.uses_separate_entities(self.data_version) {
            self.load_separate_entities(dim, pos)
        } else {
            Ok(Vec::new())
        }
    }

    fn load_chunk_lod_voxels(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        step: u8,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<LodChunkVoxels>> {
        let _prof = profiler::scope(Stage::LodVoxelLoad);
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(None);
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = rf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(None);
        };
        let (_, mut root) = read_nbt_filtered(&raw, Endianness::Big, |k| {
            matches!(k, "DataVersion" | "Level" | "sections" | "Sections")
        })?;
        let chunk_dv = root.get_int("DataVersion").unwrap_or(self.data_version);
        let padded = self.is_padded_bitpack(chunk_dv);
        let is_118 = self.is_118_layout(chunk_dv, &root);

        let mut container = if is_118 {
            root
        } else {
            let Some(NbtTag::Compound(l)) = root.remove("Level") else {
                return Ok(None);
            };
            l
        };

        let step = if step <= 2 { 2u8 } else { 4u8 };
        let s = step as usize;
        let xz_cells = 16 / s;
        let sub_y_per_sec = 16 / s;
        let min_y = self.min_sec_y * 16;
        let max_y = (self.max_sec_y + 1) * 16;
        let mut out = LodChunkVoxels::new_empty(pos, step, min_y, max_y);
        let fallback_stone = registry.find_by_legacy(1, 0).unwrap_or(BlockStateId::AIR);

        let secs_tag = container
            .remove("sections")
            .or_else(|| container.remove("Sections"));
        let mut unpacked = [0u16; SECTION_VOLUME];
        if let Some(NbtTag::List(sec_list)) = secs_tag {
            for item in sec_list.items() {
                let NbtTag::Compound(sec_nbt) = item else {
                    continue;
                };
                let sy = parse_section_y(sec_nbt);
                if sy < self.min_sec_y || sy > self.max_sec_y {
                    continue;
                }
                let (pal_opt, data_opt) = extract_section_palette_and_data(sec_nbt);
                let Some(pal_list) = pal_opt else {
                    continue;
                };
                if pal_list.is_empty() {
                    continue;
                }

                let hide_barriers = mcedit_core::hide_barriers();
                let mut pal_ids: Vec<BlockStateId> = Vec::with_capacity(pal_list.len());
                let mut any_non_air = false;
                for entry in pal_list.items() {
                    if let Some(st) = parse_palette_entry(entry) {
                        let bid = registry.resolve_readonly(&st).unwrap_or_else(|| {
                            if st.name.ends_with("air") {
                                BlockStateId::AIR
                            } else {
                                fallback_stone
                            }
                        });
                        let vis_id = registry.get_meta(bid).visual_base_id.unwrap_or(bid);
                        let vis_meta = registry.get_meta(vis_id);
                        if vis_id != BlockStateId::AIR
                            && !vis_meta.is_air
                            && (!hide_barriers || !vis_meta.is_barrier)
                        {
                            any_non_air = true;
                            pal_ids.push(vis_id);
                        } else {
                            pal_ids.push(BlockStateId::AIR);
                        }
                    } else {
                        pal_ids.push(BlockStateId::AIR);
                    }
                }
                if !any_non_air {
                    continue;
                }

                let base_cy = ((sy - self.min_sec_y) as usize) * sub_y_per_sec;
                if pal_ids.len() == 1 {
                    let single = pal_ids[0];
                    for scy in 0..sub_y_per_sec {
                        let cy = base_cy + scy;
                        for cz in 0..xz_cells {
                            for cx in 0..xz_cells {
                                out.set_cell(cx, cy, cz, single);
                            }
                        }
                    }
                    continue;
                }

                let Some(longs) = data_opt else {
                    continue;
                };
                unpack_bit_indices_into(longs, &mut unpacked, pal_ids.len(), 4, padded);

                for scy in 0..sub_y_per_sec {
                    let cy = base_cy + scy;
                    let ly0 = scy * s;
                    for cz in 0..xz_cells {
                        let lz0 = cz * s;
                        for cx in 0..xz_cells {
                            let lx0 = cx * s;
                            let mut chosen = BlockStateId::AIR;
                            let mut best_other = BlockStateId::AIR;
                            let mut best_other_prio = 0u8;

                            // Top-down layer by layer so surface water (e.g. Y=62) wins over
                            // shallow seabed/riverbed (Y=60..61) in the same 4x4x4 LOD 2 cell.
                            for dy in (0..s).rev() {
                                let y_off = (ly0 + dy) << 8;
                                let mut layer_opaque = BlockStateId::AIR;
                                let mut layer_opaque_count = 0u16;
                                let mut layer_liquid = BlockStateId::AIR;
                                let mut layer_liquid_count = 0u16;
                                for dz in 0..s {
                                    let yz_off = y_off | ((lz0 + dz) << 4);
                                    for dx in 0..s {
                                        let i = yz_off | (lx0 + dx);
                                        let sid = pal_ids
                                            .get(unpacked[i] as usize)
                                            .copied()
                                            .unwrap_or(BlockStateId::AIR);
                                        if sid == BlockStateId::AIR {
                                            continue;
                                        }
                                        let meta = registry.get_meta(sid);
                                        if meta.is_air {
                                            continue;
                                        }
                                        if meta.is_full_opaque_cube {
                                            if layer_opaque == BlockStateId::AIR {
                                                layer_opaque = sid;
                                            }
                                            layer_opaque_count += 1;
                                        } else if meta.is_liquid {
                                            if layer_liquid == BlockStateId::AIR {
                                                layer_liquid = sid;
                                            }
                                            layer_liquid_count += 1;
                                        } else {
                                            let prio = lod_non_opaque_priority(meta);
                                            if prio > best_other_prio {
                                                best_other = sid;
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
                                out.set_cell(cx, cy, cz, chosen);
                            }
                        }
                    }
                }
            }
        }
        Ok(Some(out))
    }

    fn save_chunk(
        &self,
        dim: DimensionId,
        chunk: &Chunk,
        registry: &BlockRegistry,
    ) -> FormatResult<()> {
        let (rx, rz) = chunk.pos.region_pos();
        let rf_arc = self.get_cached_region(dim, rx, rz, true)?.unwrap();

        let save_dv = chunk
            .extra_nbt
            .get_int("DataVersion")
            .unwrap_or(self.data_version)
            .max(self.data_version);
        let padded = self.is_padded_bitpack(save_dv);
        let is_118 = save_dv >= CAVES_AND_CLIFFS_DATA_VERSION;
        let use_26_3_palette = save_dv >= PALETTE_26_3_DATA_VERSION;
        let separate_ents = self.uses_separate_entities(save_dv);

        let mut container = chunk.extra_nbt.clone();
        container.remove("Heightmaps");
        container.insert("xPos", chunk.pos.cx());
        container.insert("zPos", chunk.pos.cz());
        if is_118 {
            container.insert("DataVersion", save_dv);
            container.insert("yPos", chunk.min_section_y);
            container.insert("Status", "minecraft:full".to_string());
            container.insert("isLightOn", 1i8);
        } else {
            container.insert("Status", "full".to_string());
            if !container.contains_key("LastUpdate") {
                container.insert("LastUpdate", 0i64);
            }
        }

        // Save entities either to separate `entities/r.<rx>.<rz>.mca` (1.17+) or inline `Level.Entities` (1.13–1.16.5)
        if separate_ents {
            container.remove("Entities");
            container.remove("entities");
            self.save_separate_entities(dim, chunk.pos, &chunk.entities)?;
        } else {
            let ents = NbtList::from_items(
                chunk
                    .entities
                    .iter()
                    .cloned()
                    .map(NbtTag::Compound)
                    .collect(),
            )?;
            container.insert("Entities", ents);
        }

        // Save block entities & tile ticks
        let bes = NbtList::from_items(
            chunk
                .block_entities
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        let tts = NbtList::from_items(
            chunk
                .tile_ticks
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        if is_118 {
            container.insert("block_entities", bes);
            container.insert("block_ticks", tts);
        } else {
            container.insert("TileEntities", bes);
            container.insert("TileTicks", tts);
        }

        // Save 1.13–1.17 container-level Biomes IntArray
        if !is_118 {
            if self.data_version >= BIOMES_3D_1024_DATA_VERSION {
                let mut biomes_1024 = vec![1i32; 1024];
                for sy in 0..16i32 {
                    let base = (sy as usize) * 64;
                    for cy in 0..4usize {
                        for cz in 0..4usize {
                            for cx in 0..4usize {
                                let cell_i = (cy << 4) | (cz << 2) | cx;
                                let b = chunk.biomes.get(cx * 4, sy * 16 + (cy as i32) * 4, cz * 4);
                                biomes_1024[base + cell_i] = b as i32;
                            }
                        }
                    }
                }
                container.insert("Biomes", NbtTag::IntArray(biomes_1024));
            } else {
                let mut biomes_256 = vec![1i32; 256];
                for z in 0..16usize {
                    for x in 0..16usize {
                        biomes_256[(z << 4) | x] = chunk.biomes.get(x, 0, z) as i32;
                    }
                }
                container.insert("Biomes", NbtTag::IntArray(biomes_256));
            }
        }

        // Encode sections
        let min_sy = chunk.min_section_y.min(self.min_sec_y);
        let max_sy = chunk.max_section_y.max(self.max_sec_y);
        let mut sec_tags = Vec::new();

        for sy in min_sy..=max_sy {
            let sec_opt = chunk.sections.get(&sy);
            if !is_118 && sec_opt.is_none_or(|s| s.is_trivially_empty()) {
                continue;
            }

            let mut sec_nbt = NbtCompound::new();
            sec_nbt.insert("Y", sy as i8);

            // Build section block palette & bit-packed LongArray
            let (pal_nbt_list, packed_longs) = if let Some(sec) = sec_opt
                && !sec.is_all_air()
            {
                let mut compact = sec.clone();
                compact.compact_palette();
                // Normalize any pre-1.13 BlockRegistry states (e.g. `minecraft:stone[variant=stone]`,
                // `minecraft:grass[snowy=false]`, `minecraft:wool[color=red]`) into valid post-1.13
                // block states and deduplicate palette entries if normalization merges two entries.
                let mut norm_palette: Vec<BlockState> = Vec::with_capacity(compact.palette.len());
                let mut remap: Vec<u16> = Vec::with_capacity(compact.palette.len());
                for &bid in &compact.palette {
                    let norm = normalize_post113_block_state(registry.get_state(bid));
                    if let Some(existing_idx) = norm_palette.iter().position(|s| s == &norm) {
                        remap.push(existing_idx as u16);
                    } else {
                        let new_idx = norm_palette.len() as u16;
                        norm_palette.push(norm);
                        remap.push(new_idx);
                    }
                }
                if norm_palette.len() != compact.palette.len() {
                    for slot in &mut compact.indices {
                        *slot = remap.get(*slot as usize).copied().unwrap_or(0);
                    }
                }
                let state_refs: Vec<&BlockState> = norm_palette.iter().collect();
                let pal_list = encode_palette_list(&state_refs, use_26_3_palette)?;
                let longs = if norm_palette.len() > 1 {
                    pack_bit_indices(&compact.indices, norm_palette.len(), 4, padded)
                } else {
                    Vec::new()
                };
                (pal_list, longs)
            } else {
                let air_state = BlockState::new("minecraft:air");
                let pal_list = encode_palette_list(&[&air_state], use_26_3_palette)?;
                (pal_list, Vec::new())
            };

            if is_118 {
                let mut bs_comp = NbtCompound::new();
                bs_comp.insert("palette", pal_nbt_list);
                if !packed_longs.is_empty() {
                    bs_comp.insert("data", NbtTag::LongArray(packed_longs));
                }
                sec_nbt.insert("block_states", bs_comp);

                // Encode 1.18+ 3D 4x4x4 paletted section biomes
                let mut biome_pal: Vec<u8> = Vec::new();
                let mut biome_indices = [0u16; 64];
                for cy in 0..4usize {
                    for cz in 0..4usize {
                        for cx in 0..4usize {
                            let cell_i = (cy << 4) | (cz << 2) | cx;
                            let bid = chunk.biomes.get(cx * 4, sy * 16 + (cy as i32) * 4, cz * 4);
                            let pidx = if let Some(pos) = biome_pal.iter().position(|&b| b == bid) {
                                pos as u16
                            } else {
                                let idx = biome_pal.len() as u16;
                                biome_pal.push(bid);
                                idx
                            };
                            biome_indices[cell_i] = pidx;
                        }
                    }
                }
                if biome_pal.is_empty() {
                    biome_pal.push(1);
                }
                let biome_pal_tags: Vec<NbtTag> = biome_pal
                    .iter()
                    .map(|&b| NbtTag::String(biome_id_to_name(b).to_string()))
                    .collect();
                let mut biomes_comp = NbtCompound::new();
                biomes_comp.insert("palette", NbtList::from_items(biome_pal_tags)?);
                if biome_pal.len() > 1 {
                    let b_longs =
                        pack_bit_indices(&biome_indices, biome_pal.len(), 1, true);
                    biomes_comp.insert("data", NbtTag::LongArray(b_longs));
                }
                sec_nbt.insert("biomes", biomes_comp);
            } else {
                sec_nbt.insert("Palette", pal_nbt_list);
                if !packed_longs.is_empty() {
                    sec_nbt.insert("BlockStates", NbtTag::LongArray(packed_longs));
                }
            }

            if let Some(sec) = sec_opt {
                sec_nbt.insert(
                    "BlockLight",
                    NbtTag::ByteArray(sec.block_light.iter().map(|&b| b as i8).collect()),
                );
                sec_nbt.insert(
                    "SkyLight",
                    NbtTag::ByteArray(sec.sky_light.iter().map(|&b| b as i8).collect()),
                );
            }
            sec_tags.push(NbtTag::Compound(sec_nbt));
        }

        if is_118 {
            container.insert("sections", NbtList::from_items(sec_tags)?);
        } else {
            container.insert("Sections", NbtList::from_items(sec_tags)?);
        }

        let root = if is_118 {
            container
        } else {
            let mut r = NbtCompound::new();
            r.insert("DataVersion", self.data_version);
            r.insert("Level", container);
            r
        };

        let raw_nbt = write_nbt("", &root, Endianness::Big, Compression::None)?;
        let (lx, lz) = chunk.pos.region_local();
        rf_arc.write().write_chunk_nbt(lx, lz, &raw_nbt)?;
        Ok(())
    }

    fn delete_chunk(&self, dim: DimensionId, pos: ChunkPos) -> FormatResult<()> {
        let (rx, rz) = pos.region_pos();
        let (lx, lz) = pos.region_local();
        if let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? {
            rf_arc.write().delete_chunk(lx, lz)?;
        }
        if let Some(erf_arc) = self.get_cached_entity_region(dim, rx, rz, false)? {
            erf_arc.write().delete_chunk(lx, lz)?;
        }
        Ok(())
    }

    fn load_players(&self) -> FormatResult<Vec<PlayerData>> {
        let mut out = Vec::new();
        let (_, level_nbt) =
            mcedit_nbt::read_nbt_file(self.root.join("level.dat"), Endianness::Big)?;
        if let Some(data) = level_nbt.get_compound("Data")
            && let Some(player_nbt) = data.get_compound("Player")
        {
            let mut p = PlayerData {
                id: "Player".to_string(),
                display_name: "Player (Singleplayer)".to_string(),
                is_singleplayer: true,
                dimension: DimensionId::Overworld,
                pos: DVec3::ZERO,
                rotation: [0.0, 0.0],
                raw_nbt: player_nbt.clone(),
                dirty: false,
            };
            p.sync_from_nbt();
            out.push(p);
        }

        for dir_name in ["players/data", "playerdata", "players"] {
            let pdir = self.root.join(dir_name);
            if !pdir.is_dir() {
                continue;
            }
            for entry in fs::read_dir(&pdir)?.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("dat")
                    && let Ok((_, pnbt)) = mcedit_nbt::read_nbt_file(&path, Endianness::Big)
                {
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let mut p = PlayerData {
                        id: stem.clone(),
                        display_name: stem,
                        is_singleplayer: false,
                        dimension: DimensionId::Overworld,
                        pos: DVec3::ZERO,
                        rotation: [0.0, 0.0],
                        raw_nbt: pnbt,
                        dirty: false,
                    };
                    p.sync_from_nbt();
                    out.push(p);
                }
            }
        }
        Ok(out)
    }

    fn save_player(&self, player: &PlayerData) -> FormatResult<()> {
        if player.is_singleplayer {
            return Ok(());
        }
        let pdir = if self.root.join("players").join("data").is_dir() {
            self.root.join("players").join("data")
        } else {
            self.root.join("playerdata")
        };
        fs::create_dir_all(&pdir)?;
        let mut p = player.clone();
        p.sync_to_nbt();
        mcedit_nbt::write_nbt_file(
            pdir.join(format!("{}.dat", p.id)),
            "",
            &p.raw_nbt,
            Endianness::Big,
            Compression::Gzip,
        )?;
        Ok(())
    }

    fn delete_player(&self, id: &str) -> FormatResult<()> {
        for dir_name in ["players/data", "playerdata", "players"] {
            let path = self.root.join(dir_name).join(format!("{id}.dat"));
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    fn acquire_session_lock(&self) -> FormatResult<()> {
        let ts = now_millis();
        *self.session_timestamp.lock() = ts;
        fs::write(self.root.join("session.lock"), ts.to_be_bytes())?;
        Ok(())
    }

    fn check_session_lock(&self) -> FormatResult<bool> {
        let lock_path = self.root.join("session.lock");
        if !lock_path.exists() {
            return Ok(true);
        }
        let bytes = fs::read(&lock_path)?;
        if bytes.len() != 8 {
            return Ok(false);
        }
        let disk_ts = i64::from_be_bytes(bytes.try_into().unwrap());
        let expected = *self.session_timestamp.lock();
        Ok(expected == 0 || disk_ts == expected)
    }

    fn repair_regions(&self, dim: DimensionId) -> FormatResult<RepairReport> {
        self.region_cache.write().retain(|(d, _, _), _| *d != dim);
        self.entity_region_cache
            .write()
            .retain(|(d, _, _), _| *d != dim);
        let mut total = RepairReport::default();
        for dir in [self.region_dir(dim), self.entities_dir(dim)] {
            if !dir.is_dir() {
                continue;
            }
            for entry in fs::read_dir(&dir)?.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("mca") {
                    let rep = RegionFile::repair(&path)?;
                    total.files_scanned += rep.files_scanned;
                    total.files_repaired += rep.files_repaired;
                    total.chunks_recovered += rep.chunks_recovered;
                    total.chunks_lost += rep.chunks_lost;
                }
            }
        }
        Ok(total)
    }
}

fn parse_section_y(sec_nbt: &NbtCompound) -> i32 {
    sec_nbt
        .get_byte("Y")
        .map(|b| b as i32)
        .or_else(|| sec_nbt.get_int("Y"))
        .unwrap_or(0)
}

fn extract_section_palette_and_data(
    sec_nbt: &NbtCompound,
) -> (Option<&NbtList>, Option<&[i64]>) {
    if let Some(bs) = sec_nbt.get_compound("block_states") {
        (bs.get_list("palette"), bs.get_long_array("data"))
    } else {
        (sec_nbt.get_list("Palette"), sec_nbt.get_long_array("BlockStates"))
    }
}

/// Parses a single section `palette` entry across all post-flattening formats:
/// - **1.13–26.2 (`TAG_Compound`)**: `{ "Name": "minecraft:...", "Properties"?: { ... } }`
/// - **26.3+ simple (`TAG_String`)**: `"minecraft:..."`
/// - **26.3+ mixed (`TAG_Compound`)**: `{ "": "minecraft:..." }` or `{ "id": "minecraft:...", "properties": { ... } }`
fn parse_palette_entry(entry: &NbtTag) -> Option<BlockState> {
    match entry {
        NbtTag::String(s) => Some(BlockState::new(s.as_str())),
        NbtTag::Compound(c) => {
            if let Some(simple_name) = c.get_string("") {
                return Some(BlockState::new(simple_name));
            }
            let name = c
                .get_string("Name")
                .or_else(|| c.get_string("id"))
                .unwrap_or("minecraft:air");
            let mut properties = BTreeMap::new();
            if let Some(props) = c
                .get_compound("Properties")
                .or_else(|| c.get_compound("properties"))
            {
                for (k, v) in props.iter() {
                    let val_str = match v {
                        NbtTag::String(s) => s.clone(),
                        NbtTag::Byte(b) => b.to_string(),
                        NbtTag::Short(s) => s.to_string(),
                        NbtTag::Int(i) => i.to_string(),
                        NbtTag::Long(l) => l.to_string(),
                        _ => continue,
                    };
                    properties.insert(k.to_string(), val_str);
                }
            }
            Some(BlockState {
                name: name.to_string(),
                properties,
            })
        }
        _ => None,
    }
}

/// Normalizes any pre-1.13 `BlockState` (e.g. from the Block Picker or a 1.12 schematic)
/// into its valid post-1.13 / 26.3+ flattened block name and properties, while leaving
/// already-flattened 1.13–26.3+ block states intact.
pub fn normalize_post113_block_state(state: &BlockState) -> BlockState {
    let mut name = state.name.clone();
    let mut props = state.properties.clone();
    props.remove("data");
    props.remove("legacy_data");

    match name.as_str() {
        "minecraft:stone" => {
            if let Some(v) = props.remove("variant") {
                name = match v.as_str() {
                    "granite" => "minecraft:granite",
                    "smooth_granite" => "minecraft:polished_granite",
                    "diorite" => "minecraft:diorite",
                    "smooth_diorite" => "minecraft:polished_diorite",
                    "andesite" => "minecraft:andesite",
                    "smooth_andesite" => "minecraft:polished_andesite",
                    _ => "minecraft:stone",
                }
                .to_string();
            }
        }
        "minecraft:grass" => {
            name = "minecraft:grass_block".to_string();
        }
        "minecraft:grass_path" => {
            name = "minecraft:dirt_path".to_string();
        }
        "minecraft:dirt" => {
            match props.remove("variant").as_deref() {
                Some("coarse_dirt") => {
                    name = "minecraft:coarse_dirt".to_string();
                    props.remove("snowy");
                }
                Some("podzol") => {
                    name = "minecraft:podzol".to_string();
                }
                _ => {
                    props.remove("snowy");
                }
            }
        }
        "minecraft:planks" => {
            let v = props.remove("variant").unwrap_or_else(|| "oak".to_string());
            name = format!("minecraft:{v}_planks");
        }
        "minecraft:sapling" => {
            let v = props.remove("type").unwrap_or_else(|| "oak".to_string());
            name = format!("minecraft:{v}_sapling");
        }
        "minecraft:sand" => {
            let v = props.remove("variant");
            if v.as_deref() == Some("red_sand") {
                name = "minecraft:red_sand".to_string();
            }
        }
        "minecraft:log" | "minecraft:log2" => {
            let default_v = if name == "minecraft:log2" { "acacia" } else { "oak" };
            let v = props
                .remove("variant")
                .unwrap_or_else(|| default_v.to_string());
            if props.get("axis").map(|s| s.as_str()) == Some("none") {
                props.insert("axis".to_string(), "y".to_string());
                name = format!("minecraft:{v}_wood");
            } else {
                name = format!("minecraft:{v}_log");
            }
        }
        "minecraft:leaves" | "minecraft:leaves2" => {
            let default_v = if name == "minecraft:leaves2" { "acacia" } else { "oak" };
            let v = props
                .remove("variant")
                .unwrap_or_else(|| default_v.to_string());
            props.remove("check_decay");
            props.remove("decayable");
            name = format!("minecraft:{v}_leaves");
        }
        "minecraft:sponge" => {
            let w = props.remove("wet");
            if w.as_deref() == Some("true") {
                name = "minecraft:wet_sponge".to_string();
            }
        }
        "minecraft:sandstone" => {
            if let Some(t) = props.remove("type") {
                name = match t.as_str() {
                    "chiseled_sandstone" => "minecraft:chiseled_sandstone",
                    "smooth_sandstone" => "minecraft:cut_sandstone",
                    _ => "minecraft:sandstone",
                }
                .to_string();
            }
        }
        "minecraft:red_sandstone" => {
            if let Some(t) = props.remove("type") {
                name = match t.as_str() {
                    "chiseled_red_sandstone" => "minecraft:chiseled_red_sandstone",
                    "smooth_red_sandstone" => "minecraft:cut_red_sandstone",
                    _ => "minecraft:red_sandstone",
                }
                .to_string();
            }
        }
        "minecraft:tallgrass" => {
            name = match props.remove("type").as_deref() {
                Some("dead_bush") => "minecraft:dead_bush",
                Some("fern") => "minecraft:fern",
                _ => "minecraft:short_grass",
            }
            .to_string();
        }
        "minecraft:deadbush" => {
            name = "minecraft:dead_bush".to_string();
        }
        "minecraft:yellow_flower" => {
            props.remove("type");
            name = "minecraft:dandelion".to_string();
        }
        "minecraft:red_flower" => {
            name = match props.remove("type").as_deref() {
                Some("blue_orchid") => "minecraft:blue_orchid",
                Some("allium") => "minecraft:allium",
                Some("houstonia") => "minecraft:azure_bluet",
                Some("red_tulip") => "minecraft:red_tulip",
                Some("orange_tulip") => "minecraft:orange_tulip",
                Some("white_tulip") => "minecraft:white_tulip",
                Some("pink_tulip") => "minecraft:pink_tulip",
                Some("oxeye_daisy") => "minecraft:oxeye_daisy",
                _ => "minecraft:poppy",
            }
            .to_string();
        }
        "minecraft:wool"
        | "minecraft:carpet"
        | "minecraft:stained_glass"
        | "minecraft:stained_glass_pane"
        | "minecraft:concrete"
        | "minecraft:concrete_powder" => {
            let col = props.remove("color").unwrap_or_else(|| "white".to_string());
            let col = if col == "silver" { "light_gray" } else { col.as_str() };
            let suffix = name.strip_prefix("minecraft:").unwrap_or(&name);
            name = format!("minecraft:{col}_{suffix}");
        }
        "minecraft:stained_hardened_clay" => {
            let col = props.remove("color").unwrap_or_else(|| "white".to_string());
            let col = if col == "silver" { "light_gray" } else { col.as_str() };
            name = format!("minecraft:{col}_terracotta");
        }
        "minecraft:hardened_clay" => {
            name = "minecraft:terracotta".to_string();
        }
        "minecraft:stonebrick" => {
            name = match props.remove("variant").as_deref() {
                Some("mossy_stonebrick") => "minecraft:mossy_stone_bricks",
                Some("cracked_stonebrick") => "minecraft:cracked_stone_bricks",
                Some("chiseled_stonebrick") => "minecraft:chiseled_stone_bricks",
                _ => "minecraft:stone_bricks",
            }
            .to_string();
        }
        "minecraft:monster_egg" => {
            name = match props.remove("variant").as_deref() {
                Some("cobblestone") => "minecraft:infested_cobblestone",
                Some("stone_brick") => "minecraft:infested_stone_bricks",
                Some("mossy_brick") => "minecraft:infested_mossy_stone_bricks",
                Some("cracked_brick") => "minecraft:infested_cracked_stone_bricks",
                Some("chiseled_brick") => "minecraft:infested_chiseled_stone_bricks",
                _ => "minecraft:infested_stone",
            }
            .to_string();
        }
        "minecraft:quartz_block" => {
            if let Some(v) = props.remove("variant") {
                match v.as_str() {
                    "chiseled" => name = "minecraft:chiseled_quartz_block".to_string(),
                    "lines_y" => {
                        name = "minecraft:quartz_pillar".to_string();
                        props.insert("axis".to_string(), "y".to_string());
                    }
                    "lines_x" => {
                        name = "minecraft:quartz_pillar".to_string();
                        props.insert("axis".to_string(), "x".to_string());
                    }
                    "lines_z" => {
                        name = "minecraft:quartz_pillar".to_string();
                        props.insert("axis".to_string(), "z".to_string());
                    }
                    _ => {}
                }
            }
        }
        "minecraft:prismarine" => {
            if let Some(v) = props.remove("variant") {
                name = match v.as_str() {
                    "prismarine_bricks" => "minecraft:prismarine_bricks",
                    "dark_prismarine" => "minecraft:dark_prismarine",
                    _ => "minecraft:prismarine",
                }
                .to_string();
            }
        }
        "minecraft:cobblestone_wall" => {
            if props.remove("variant").as_deref() == Some("mossy_cobblestone") {
                name = "minecraft:mossy_cobblestone_wall".to_string();
            }
            props.clear();
        }
        "minecraft:wooden_slab" | "minecraft:double_wooden_slab" => {
            let is_double = name == "minecraft:double_wooden_slab";
            let v = props.remove("variant").unwrap_or_else(|| "oak".to_string());
            if let Some(h) = props.remove("half") {
                props.insert("type".to_string(), h);
            } else if is_double {
                props.insert("type".to_string(), "double".to_string());
            }
            name = format!("minecraft:{v}_slab");
        }
        "minecraft:stone_slab"
        | "minecraft:double_stone_slab"
        | "minecraft:stone_slab2"
        | "minecraft:double_stone_slab2"
        | "minecraft:purpur_slab"
        | "minecraft:purpur_double_slab" => {
            let is_double = name.contains("double");
            props.remove("seamless");
            let v = props.remove("variant").unwrap_or_else(|| {
                if name.contains("purpur") {
                    "purpur".to_string()
                } else if name.contains("slab2") {
                    "red_sandstone".to_string()
                } else {
                    "stone".to_string()
                }
            });
            if let Some(h) = props.remove("half") {
                props.insert("type".to_string(), h);
            } else if is_double {
                props.insert("type".to_string(), "double".to_string());
            }
            name = match v.as_str() {
                "stone" => "minecraft:smooth_stone_slab",
                "sandstone" => "minecraft:sandstone_slab",
                "wood_old" => "minecraft:petrified_oak_slab",
                "cobblestone" => "minecraft:cobblestone_slab",
                "brick" => "minecraft:brick_slab",
                "stone_brick" => "minecraft:stone_brick_slab",
                "nether_brick" => "minecraft:nether_brick_slab",
                "quartz" => "minecraft:quartz_slab",
                "red_sandstone" => "minecraft:red_sandstone_slab",
                "purpur" | "default" => "minecraft:purpur_slab",
                _ => "minecraft:stone_slab",
            }
            .to_string();
        }
        "minecraft:brick_block" => name = "minecraft:bricks".to_string(),
        "minecraft:nether_brick" => name = "minecraft:nether_bricks".to_string(),
        "minecraft:red_nether_brick" => name = "minecraft:red_nether_bricks".to_string(),
        "minecraft:end_bricks" => name = "minecraft:end_stone_bricks".to_string(),
        "minecraft:magma" => name = "minecraft:magma_block".to_string(),
        "minecraft:web" => name = "minecraft:cobweb".to_string(),
        "minecraft:waterlily" => name = "minecraft:lily_pad".to_string(),
        "minecraft:reeds" => name = "minecraft:sugar_cane".to_string(),
        "minecraft:snow_layer" => name = "minecraft:snow".to_string(),
        "minecraft:noteblock" => name = "minecraft:note_block".to_string(),
        "minecraft:golden_rail" => name = "minecraft:powered_rail".to_string(),
        "minecraft:mob_spawner" => name = "minecraft:spawner".to_string(),
        "minecraft:wooden_door" => name = "minecraft:oak_door".to_string(),
        "minecraft:trapdoor" => name = "minecraft:oak_trapdoor".to_string(),
        "minecraft:fence" => name = "minecraft:oak_fence".to_string(),
        "minecraft:fence_gate" => name = "minecraft:oak_fence_gate".to_string(),
        "minecraft:wooden_pressure_plate" => name = "minecraft:oak_pressure_plate".to_string(),
        "minecraft:wooden_button" => name = "minecraft:oak_button".to_string(),
        "minecraft:stone_stairs" => name = "minecraft:cobblestone_stairs".to_string(),
        "minecraft:quartz_ore" => name = "minecraft:nether_quartz_ore".to_string(),
        "minecraft:slime" => name = "minecraft:slime_block".to_string(),
        "minecraft:melon_block" => name = "minecraft:melon".to_string(),
        "minecraft:lit_pumpkin" => name = "minecraft:jack_o_lantern".to_string(),
        "minecraft:lit_furnace" => {
            name = "minecraft:furnace".to_string();
            props.insert("lit".to_string(), "true".to_string());
        }
        "minecraft:lit_redstone_ore" => {
            name = "minecraft:redstone_ore".to_string();
            props.insert("lit".to_string(), "true".to_string());
        }
        "minecraft:lit_redstone_lamp" => {
            name = "minecraft:redstone_lamp".to_string();
            props.insert("lit".to_string(), "true".to_string());
        }
        "minecraft:sign" | "minecraft:standing_sign" => name = "minecraft:oak_sign".to_string(),
        "minecraft:wall_sign" => name = "minecraft:oak_wall_sign".to_string(),
        "minecraft:tnt" => {
            props.remove("explode");
        }
        "minecraft:pumpkin" => {
            props.remove("facing");
        }
        "minecraft:cauldron" => {
            props.remove("level");
        }
        "minecraft:flower_pot" => {
            props.remove("contents");
        }
        "minecraft:anvil" => {
            props.remove("damage");
        }
        "minecraft:skull" => {
            name = "minecraft:skeleton_skull".to_string();
            props.remove("nodrop");
            props.remove("facing");
        }
        _ => {}
    }

    // "variant" was removed from all vanilla blocks in 1.13 The Flattening
    props.remove("variant");

    BlockState {
        name,
        properties: props,
    }
}

fn canonical_post113_name(state: &BlockState) -> &str {
    if state.name == "minecraft:grass" {
        "minecraft:grass_block"
    } else {
        state.name.as_str()
    }
}

fn encode_palette_list(
    states: &[&BlockState],
    use_26_3_palette: bool,
) -> FormatResult<NbtList> {
    if use_26_3_palette {
        let any_has_props = states.iter().any(|s| !s.properties.is_empty());
        if !any_has_props {
            let items: Vec<NbtTag> = states
                .iter()
                .map(|s| NbtTag::String(canonical_post113_name(s).to_string()))
                .collect();
            return Ok(NbtList::from_items(items)?);
        }
        let items: Vec<NbtTag> = states
            .iter()
            .map(|s| {
                let mut c = NbtCompound::new();
                let name = canonical_post113_name(s).to_string();
                if s.properties.is_empty() {
                    c.insert("", name);
                } else {
                    c.insert("id", name);
                    let mut props = NbtCompound::new();
                    for (k, v) in &s.properties {
                        props.insert(k.clone(), v.clone());
                    }
                    c.insert("properties", props);
                }
                NbtTag::Compound(c)
            })
            .collect();
        Ok(NbtList::from_items(items)?)
    } else {
        let items: Vec<NbtTag> = states
            .iter()
            .map(|s| {
                let mut c = NbtCompound::new();
                c.insert("Name", canonical_post113_name(s).to_string());
                if !s.properties.is_empty() {
                    let mut props = NbtCompound::new();
                    for (k, v) in &s.properties {
                        props.insert(k.clone(), v.clone());
                    }
                    c.insert("Properties", props);
                }
                NbtTag::Compound(c)
            })
            .collect();
        Ok(NbtList::from_items(items)?)
    }
}

pub fn biome_name_to_id(name: &str) -> u8 {
    let slug = name.strip_prefix("minecraft:").unwrap_or(name);
    match slug {
        "ocean" => 0,
        "plains" => 1,
        "desert" => 2,
        "windswept_hills" | "mountains" | "extreme_hills" => 3,
        "forest" => 4,
        "taiga" => 5,
        "swamp" | "swampland" | "mangrove_swamp" => 6,
        "river" => 7,
        "nether_wastes" | "hell" => 8,
        "the_end" | "sky" | "small_end_islands" | "end_midlands" | "end_highlands" | "end_barrens" => 9,
        "frozen_ocean" => 10,
        "frozen_river" => 11,
        "snowy_plains" | "snowy_tundra" | "ice_plains" => 12,
        "snowy_mountains" | "snowy_slopes" | "frozen_peaks" | "jagged_peaks" => 13,
        "mushroom_fields" | "mushroom_island" => 14,
        "beach" => 16,
        "jungle" => 21,
        "sparse_jungle" | "jungle_edge" => 23,
        "deep_ocean" => 24,
        "stony_shore" | "stone_shore" | "stony_peaks" => 25,
        "snowy_beach" | "cold_beach" => 26,
        "birch_forest" | "old_growth_birch_forest" => 27,
        "dark_forest" | "roofed_forest" => 29,
        "snowy_taiga" | "cold_taiga" | "grove" => 30,
        "old_growth_pine_taiga" | "old_growth_spruce_taiga" | "giant_tree_taiga" => 32,
        "windswept_forest" | "wooded_mountains" => 34,
        "savanna" | "windswept_savanna" => 35,
        "savanna_plateau" => 36,
        "badlands" | "mesa" | "eroded_badlands" => 37,
        "wooded_badlands" | "wooded_badlands_plateau" => 38,
        "warm_ocean" => 44,
        "lukewarm_ocean" | "deep_lukewarm_ocean" => 45,
        "cold_ocean" | "deep_cold_ocean" => 46,
        "deep_frozen_ocean" => 50,
        "sunflower_plains" | "meadow" => 129,
        "flower_forest" | "cherry_grove" => 132,
        "ice_spikes" => 140,
        "bamboo_jungle" => 168,
        "soul_sand_valley" => 170,
        "crimson_forest" => 171,
        "warped_forest" => 172,
        "basalt_deltas" => 173,
        "dripstone_caves" => 174,
        "lush_caves" => 175,
        "deep_dark" => 176,
        _ => 1,
    }
}

pub fn biome_id_to_name(id: u8) -> &'static str {
    match id {
        0 => "minecraft:ocean",
        1 => "minecraft:plains",
        2 => "minecraft:desert",
        3 => "minecraft:windswept_hills",
        4 => "minecraft:forest",
        5 => "minecraft:taiga",
        6 => "minecraft:swamp",
        7 => "minecraft:river",
        8 => "minecraft:nether_wastes",
        9 => "minecraft:the_end",
        10 => "minecraft:frozen_ocean",
        11 => "minecraft:frozen_river",
        12 => "minecraft:snowy_plains",
        13 => "minecraft:snowy_slopes",
        14 => "minecraft:mushroom_fields",
        16 => "minecraft:beach",
        21 => "minecraft:jungle",
        23 => "minecraft:sparse_jungle",
        24 => "minecraft:deep_ocean",
        25 => "minecraft:stony_shore",
        26 => "minecraft:snowy_beach",
        27 => "minecraft:birch_forest",
        29 => "minecraft:dark_forest",
        30 => "minecraft:snowy_taiga",
        32 => "minecraft:old_growth_pine_taiga",
        34 => "minecraft:windswept_forest",
        35 => "minecraft:savanna",
        36 => "minecraft:savanna_plateau",
        37 => "minecraft:badlands",
        38 => "minecraft:wooded_badlands",
        44 => "minecraft:warm_ocean",
        45 => "minecraft:lukewarm_ocean",
        46 => "minecraft:cold_ocean",
        50 => "minecraft:deep_frozen_ocean",
        129 => "minecraft:sunflower_plains",
        132 => "minecraft:flower_forest",
        140 => "minecraft:ice_spikes",
        168 => "minecraft:bamboo_jungle",
        170 => "minecraft:soul_sand_valley",
        171 => "minecraft:crimson_forest",
        172 => "minecraft:warped_forest",
        173 => "minecraft:basalt_deltas",
        174 => "minecraft:dripstone_caves",
        175 => "minecraft:lush_caves",
        176 => "minecraft:deep_dark",
        _ => "minecraft:plains",
    }
}

fn resolve_world_dir(path: &Path) -> FormatResult<PathBuf> {
    if path.is_file()
        && path.file_name().and_then(|s| s.to_str()) == Some("level.dat")
        && let Some(parent) = path.parent()
    {
        return Ok(parent.to_path_buf());
    }
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }
    Err(FormatError::UnsupportedFormat(path.display().to_string()))
}

fn parse_region_filename(fname: &str) -> Option<(i32, i32)> {
    let stem = fname.strip_prefix("r.")?.strip_suffix(".mca")?;
    let (rx_s, rz_s) = stem.split_once('.')?;
    Some((rx_s.parse().ok()?, rz_s.parse().ok()?))
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
