use crate::anvil::region::RegionFile;
use crate::legacy_mapper::LegacyIdMapper;
use ahash::AHashMap;
use glam::{DVec3, IVec3};
use mcedit_core::section::{LIGHT_BYTES, SECTION_VOLUME, get_nibble, set_nibble};
use mcedit_core::{
    BiomeStorage, BlockRegistry, BlockStateId, Chunk, ChunkPos, ChunkSection, DimensionId,
    FormatError, FormatResult, LodChunkSurface, LodChunkVoxels, PlayerData, RepairReport,
    WorldFormat, WorldMetadata, lod_non_opaque_priority, tinted_map_color,
};
use mcedit_nbt::{Compression, Endianness, NbtCompound, NbtList, NbtTag, read_nbt, write_nbt};
use parking_lot::{Mutex, RwLock};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION_ANVIL: i32 = 19133;
const MAX_PRE113_DATA_VERSION: i32 = 1451; // Java 1.12.2 is 1343; 1.13 snapshots start at 1451+

type RegionCacheMap = AHashMap<(DimensionId, i32, i32), Arc<RwLock<RegionFile>>>;

pub struct AnvilPre113Format {
    root: PathBuf,
    mapper: RwLock<LegacyIdMapper>,
    region_cache: RwLock<RegionCacheMap>,
    session_timestamp: Mutex<i64>,
}

impl AnvilPre113Format {
    pub fn open(path: impl AsRef<Path>, reg: &BlockRegistry) -> FormatResult<Self> {
        let root = resolve_world_dir(path.as_ref())?;
        let level_dat = root.join("level.dat");
        if !level_dat.exists() {
            return Err(FormatError::UnsupportedFormat(root.display().to_string()));
        }
        // Reject Bedrock/Old Pocket directories
        if root.join("chunks.dat").exists() || root.join("db").is_dir() {
            return Err(FormatError::UnsupportedFormat(root.display().to_string()));
        }

        let (_, root_nbt) = mcedit_nbt::read_nbt_file(&level_dat, Endianness::Big)?;
        let data = root_nbt
            .get_compound("Data")
            .ok_or_else(|| FormatError::Corrupt("level.dat missing Data compound".to_string()))?;

        let version = data.get_int("version").unwrap_or(0);
        if version != VERSION_ANVIL {
            return Err(FormatError::UnsupportedVersion(format!(
                "expected Anvil version {VERSION_ANVIL}, found {version}"
            )));
        }

        if let Some(ver_tag) = data.get_compound("Version")
            && let Some(dv) = ver_tag.get_int("Id")
            && dv > MAX_PRE113_DATA_VERSION
        {
            return Err(FormatError::UnsupportedVersion(format!(
                "Java 1.13+ post-flattening world (DataVersion {dv}) is not yet implemented; planned for AnvilPost113Format"
            )));
        }

        Ok(Self {
            root,
            mapper: RwLock::new(LegacyIdMapper::from_registry(reg)),
            region_cache: RwLock::new(AHashMap::new()),
            session_timestamp: Mutex::new(0),
        })
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
        let rf = Arc::new(RwLock::new(RegionFile::open_or_create(&rpath)?));
        let mut cache = self.region_cache.write();
        Ok(Some(cache.entry(key).or_insert(rf).clone()))
    }

    pub fn create_new(
        path: impl AsRef<Path>,
        level_name: &str,
        seed: i64,
        game_type: i32,
        spawn_pos: IVec3,
        reg: &BlockRegistry,
    ) -> FormatResult<Self> {
        let root = path.as_ref().to_path_buf();
        fs::create_dir_all(root.join("region"))?;

        let mut data = NbtCompound::new();
        data.insert("LevelName", level_name.to_string());
        data.insert("RandomSeed", seed);
        data.insert("Time", 0i64);
        data.insert("DayTime", 1000i64);
        data.insert("GameType", game_type);
        data.insert("SpawnX", spawn_pos.x);
        data.insert("SpawnY", spawn_pos.y);
        data.insert("SpawnZ", spawn_pos.z);
        data.insert("version", VERSION_ANVIL);
        data.insert("initialized", 1i8);
        data.insert("generatorName", "flat".to_string());
        data.insert("generatorVersion", 0i32);
        data.insert("MapFeatures", 0i8);
        data.insert("LastPlayed", now_millis());

        let mut ver = NbtCompound::new();
        ver.insert("Id", 1343i32); // 1.12.2
        ver.insert("Name", "1.12.2".to_string());
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

    fn region_dir(&self, dim: DimensionId) -> PathBuf {
        match dim {
            DimensionId::Overworld => self.root.join("region"),
            DimensionId::Nether => self.root.join("DIM-1").join("region"),
            DimensionId::End => self.root.join("DIM1").join("region"),
            DimensionId::Custom(id) => self.root.join(format!("DIM{id}")).join("region"),
        }
    }

    fn region_path(&self, dim: DimensionId, rx: i32, rz: i32) -> PathBuf {
        self.region_dir(dim).join(format!("r.{rx}.{rz}.mca"))
    }
}

impl WorldFormat for AnvilPre113Format {
    fn format_name(&self) -> &'static str {
        "Java Anvil (1.2-1.12.2)"
    }

    fn root_path(&self) -> &Path {
        &self.root
    }

    fn min_section_y(&self) -> i32 {
        0
    }

    fn max_section_y(&self) -> i32 {
        15
    }

    fn dimensions(&self) -> Vec<DimensionId> {
        let mut dims = vec![DimensionId::Overworld];
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
        let (_, raw_nbt) = mcedit_nbt::read_nbt_file(self.root.join("level.dat"), Endianness::Big)?;
        let data = raw_nbt
            .get_compound("Data")
            .ok_or_else(|| FormatError::Corrupt("level.dat missing Data".to_string()))?;

        let data_version = data
            .get_compound("Version")
            .and_then(|v| v.get_int("Id"));

        let mut meta = WorldMetadata {
            level_name: data.get_string("LevelName").unwrap_or("World").to_string(),
            seed: data.get_long("RandomSeed").unwrap_or(0),
            time: data.get_long("Time").unwrap_or(0),
            day_time: data.get_long("DayTime").unwrap_or(0),
            game_type: data.get_int("GameType").unwrap_or(1),
            spawn_pos: IVec3::new(
                data.get_int("SpawnX").unwrap_or(0),
                data.get_int("SpawnY").unwrap_or(64),
                data.get_int("SpawnZ").unwrap_or(0),
            ),
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

        mcedit_core::profiler::add(mcedit_core::profiler::Counter::UncompressedNbtBytes, raw.len() as u64);
        let (_, mut root) = {
            let _p = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::NbtParse);
            read_nbt(&raw, Endianness::Big)?
        };
        mcedit_core::profiler::add(mcedit_core::profiler::Counter::ChunksDiskLoaded, 1);
        let _d = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::ChunkDecode);
        let Some(NbtTag::Compound(mut level)) = root.remove("Level") else {
            return Err(FormatError::Corrupt("chunk missing Level compound".to_string()));
        };

        let mut chunk = Chunk::new(pos, 0, 15);
        chunk.terrain_populated = level.get_bool("TerrainPopulated").unwrap_or(true);

        if let Some(arr) = level.get_byte_array("Biomes")
            && arr.len() >= 256
        {
            let mut cols = [1u8; 256];
            for (i, &b) in arr[..256].iter().enumerate() {
                cols[i] = b as u8;
            }
            chunk.biomes = BiomeStorage::Columns2D(cols);
        }
        level.remove("Biomes");

        if let Some(hm) = level.get_int_array("HeightMap")
            && hm.len() >= 256
        {
            chunk.heightmap.copy_from_slice(&hm[..256]);
        }
        level.remove("HeightMap");

        if let Some(NbtTag::List(ents)) = level.remove("Entities") {
            chunk.entities = ents
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }
        if let Some(NbtTag::List(bes)) = level.remove("TileEntities") {
            chunk.block_entities = bes
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }
        if let Some(NbtTag::List(tts)) = level.remove("TileTicks") {
            chunk.tile_ticks = tts
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }

        if let Some(NbtTag::List(sec_list)) = level.remove("Sections") {
            let mut mapper = self.mapper.write();
            for item in sec_list.items() {
                let NbtTag::Compound(sec_nbt) = item else {
                    continue;
                };
                let sy = sec_nbt.get_byte("Y").unwrap_or(0) as i32;
                let blocks = sec_nbt.get_byte_array("Blocks");
                let add = sec_nbt.get_byte_array("Add");
                let data = sec_nbt.get_byte_array("Data");

                let mut section = ChunkSection::new_empty();
                if let Some(b_arr) = blocks
                    && b_arr.len() == SECTION_VOLUME
                {
                    for y in 0..16 {
                        for z in 0..16 {
                            for x in 0..16 {
                                let i = (y << 8) | (z << 4) | x;
                                let low_id = (b_arr[i] as u8) as u16;
                                let high_id = add
                                    .filter(|a| a.len() == LIGHT_BYTES)
                                    .map(|a| {
                                        let u8_slice = unsafe {
                                            std::slice::from_raw_parts(a.as_ptr() as *const u8, a.len())
                                        };
                                        (get_nibble(u8_slice, i) as u16) << 8
                                    })
                                    .unwrap_or(0);
                                let full_id = low_id | high_id;
                                let dval = data
                                    .filter(|d| d.len() == LIGHT_BYTES)
                                    .map(|d| {
                                        let u8_slice = unsafe {
                                            std::slice::from_raw_parts(d.as_ptr() as *const u8, d.len())
                                        };
                                        get_nibble(u8_slice, i)
                                    })
                                    .unwrap_or(0);

                                if full_id != 0 || dval != 0 {
                                    let sid = mapper.to_canonical(full_id, dval, registry);
                                    section.set_block(x, y, z, sid);
                                }
                            }
                        }
                    }
                }

                if let Some(bl) = sec_nbt.get_byte_array("BlockLight")
                    && bl.len() == LIGHT_BYTES
                {
                    for (i, &v) in bl.iter().enumerate() {
                        section.block_light[i] = v as u8;
                    }
                }
                if let Some(sl) = sec_nbt.get_byte_array("SkyLight")
                    && sl.len() == LIGHT_BYTES
                {
                    for (i, &v) in sl.iter().enumerate() {
                        section.sky_light[i] = v as u8;
                    }
                }
                chunk.sections.insert(sy, section);
            }
        }

        chunk.extra_nbt = level;
        chunk.dirty = false;
        Ok(Some(chunk))
    }

    fn load_chunk_readonly(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        _registry: &BlockRegistry,
    ) -> FormatResult<Option<Chunk>> {
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(None);
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = rf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(None);
        };

        mcedit_core::profiler::add(mcedit_core::profiler::Counter::UncompressedNbtBytes, raw.len() as u64);
        let (_, mut root) = {
            let _p = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::NbtParse);
            read_nbt(&raw, Endianness::Big)?
        };
        mcedit_core::profiler::add(mcedit_core::profiler::Counter::ChunksDiskLoaded, 1);
        let _d = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::ChunkDecode);
        let Some(NbtTag::Compound(mut level)) = root.remove("Level") else {
            return Err(FormatError::Corrupt("chunk missing Level compound".to_string()));
        };

        let mut chunk = Chunk::new(pos, 0, 15);
        chunk.terrain_populated = level.get_bool("TerrainPopulated").unwrap_or(true);

        if let Some(arr) = level.get_byte_array("Biomes")
            && arr.len() >= 256
        {
            let mut cols = [1u8; 256];
            for (i, &b) in arr[..256].iter().enumerate() {
                cols[i] = b as u8;
            }
            chunk.biomes = BiomeStorage::Columns2D(cols);
        }
        level.remove("Biomes");

        if let Some(hm) = level.get_int_array("HeightMap")
            && hm.len() >= 256
        {
            chunk.heightmap.copy_from_slice(&hm[..256]);
        }
        level.remove("HeightMap");

        if let Some(NbtTag::List(ents)) = level.remove("Entities") {
            chunk.entities = ents
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }
        if let Some(NbtTag::List(bes)) = level.remove("TileEntities") {
            chunk.block_entities = bes
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }
        if let Some(NbtTag::List(tts)) = level.remove("TileTicks") {
            chunk.tile_ticks = tts
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }

        if let Some(NbtTag::List(sec_list)) = level.remove("Sections") {
            let mapper = self.mapper.read();
            for item in sec_list.items() {
                let NbtTag::Compound(sec_nbt) = item else {
                    continue;
                };
                let sy = sec_nbt.get_byte("Y").unwrap_or(0) as i32;
                let blocks = sec_nbt.get_byte_array("Blocks");
                let add = sec_nbt.get_byte_array("Add");
                let data = sec_nbt.get_byte_array("Data");

                let mut section = ChunkSection::new_empty();
                if let Some(b_arr) = blocks
                    && b_arr.len() == SECTION_VOLUME
                {
                    let add_u8 = add.filter(|a| a.len() == LIGHT_BYTES).map(|a| unsafe {
                        std::slice::from_raw_parts(a.as_ptr() as *const u8, a.len())
                    });
                    let data_u8 = data.filter(|d| d.len() == LIGHT_BYTES).map(|d| unsafe {
                        std::slice::from_raw_parts(d.as_ptr() as *const u8, d.len())
                    });
                    for y in 0..16 {
                        for z in 0..16 {
                            for x in 0..16 {
                                let i = (y << 8) | (z << 4) | x;
                                let low_id = (b_arr[i] as u8) as u16;
                                let high_id = add_u8
                                    .map(|a| (get_nibble(a, i) as u16) << 8)
                                    .unwrap_or(0);
                                let full_id = low_id | high_id;
                                if full_id != 0 {
                                    let dval = data_u8.map(|d| get_nibble(d, i)).unwrap_or(0);
                                    let sid = mapper.to_canonical_readonly(full_id, dval);
                                    if sid != mcedit_core::BlockStateId::AIR {
                                        section.set_block(x, y, z, sid);
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(bl) = sec_nbt.get_byte_array("BlockLight")
                    && bl.len() == LIGHT_BYTES
                {
                    for (i, &v) in bl.iter().enumerate() {
                        section.block_light[i] = v as u8;
                    }
                }
                if let Some(sl) = sec_nbt.get_byte_array("SkyLight")
                    && sl.len() == LIGHT_BYTES
                {
                    for (i, &v) in sl.iter().enumerate() {
                        section.sky_light[i] = v as u8;
                    }
                }
                chunk.sections.insert(sy, section);
            }
        }
        chunk.extra_nbt = level;
        chunk.dirty = false;
        Ok(Some(chunk))
    }

    fn load_chunk_lod_surface(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<LodChunkSurface>> {
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(None);
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = rf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(None);
        };
        mcedit_core::profiler::add(mcedit_core::profiler::Counter::UncompressedNbtBytes, raw.len() as u64);
        let (_, mut root) = {
            let _p = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::NbtParse);
            read_nbt(&raw, Endianness::Big)?
        };
        mcedit_core::profiler::add(mcedit_core::profiler::Counter::ChunksDiskLoaded, 1);
        let _d = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::ChunkDecode);
        let Some(NbtTag::Compound(mut level)) = root.remove("Level") else {
            return Ok(None);
        };

        let mut heights = [i32::MIN; 256];
        let mut blocks = [BlockStateId::AIR; 256];
        let mut colors = [[0u8; 4]; 256];
        let mut remaining = 256usize;
        let hide_barriers = mcedit_core::hide_barriers();

        if let Some(NbtTag::List(sec_list)) = level.remove("Sections") {
            let mut secs: Vec<(i32, &NbtCompound)> = sec_list
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some((c.get_byte("Y").unwrap_or(0) as i32, c)),
                    _ => None,
                })
                .collect();
            secs.sort_by_key(|b| std::cmp::Reverse(b.0)); // Top section downward

            let mapper = self.mapper.read();
            for (sy, sec_nbt) in secs {
                if remaining == 0 {
                    break;
                }
                let Some(b_arr) = sec_nbt.get_byte_array("Blocks") else {
                    continue;
                };
                if b_arr.len() != SECTION_VOLUME {
                    continue;
                }
                let add_u8 = sec_nbt
                    .get_byte_array("Add")
                    .filter(|a| a.len() == LIGHT_BYTES)
                    .map(|a| unsafe {
                        std::slice::from_raw_parts(a.as_ptr() as *const u8, a.len())
                    });
                let data_u8 = sec_nbt
                    .get_byte_array("Data")
                    .filter(|d| d.len() == LIGHT_BYTES)
                    .map(|d| unsafe {
                        std::slice::from_raw_parts(d.as_ptr() as *const u8, d.len())
                    });
                let base_y = sy * 16;

                for ly in (0..16).rev() {
                    if remaining == 0 {
                        break;
                    }
                    let y_off = ly << 8;
                    for lz in 0..16 {
                        let z_off = lz << 4;
                        for lx in 0..16 {
                            let col_idx = z_off | lx;
                            if heights[col_idx] != i32::MIN {
                                continue;
                            }
                            let i = y_off | col_idx;
                            let low_id = (b_arr[i] as u8) as u16;
                            let high_id = add_u8
                                .map(|a| (get_nibble(a, i) as u16) << 8)
                                .unwrap_or(0);
                            let full_id = low_id | high_id;
                            if full_id != 0 {
                                let dval = data_u8.map(|d| get_nibble(d, i)).unwrap_or(0);
                                let sid = mapper.to_canonical_readonly(full_id, dval);
                                let meta = registry.get_meta(sid);
                                if !meta.is_air && (!hide_barriers || !meta.is_barrier) {
                                    heights[col_idx] = base_y + ly as i32;
                                    blocks[col_idx] = sid;
                                    colors[col_idx] = tinted_map_color(meta);
                                    remaining -= 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(Some(LodChunkSurface {
            pos,
            heights,
            blocks,
            colors,
        }))
    }

    fn load_chunk_lod_voxels(
        &self,
        dim: DimensionId,
        pos: ChunkPos,
        step: u8,
        registry: &BlockRegistry,
    ) -> FormatResult<Option<LodChunkVoxels>> {
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(None);
        };
        let (lx, lz) = pos.region_local();
        let Some(raw) = rf_arc.read().read_chunk_nbt(lx, lz)? else {
            return Ok(None);
        };
        mcedit_core::profiler::add(mcedit_core::profiler::Counter::UncompressedNbtBytes, raw.len() as u64);
        let (_, mut root) = {
            let _p = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::NbtParse);
            read_nbt(&raw, Endianness::Big)?
        };
        mcedit_core::profiler::add(mcedit_core::profiler::Counter::ChunksDiskLoaded, 1);
        let _d = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::ChunkDecode);
        let Some(NbtTag::Compound(mut level)) = root.remove("Level") else {
            return Ok(None);
        };

        let step = if step <= 2 { 2u8 } else { 4u8 };
        let s = step as usize;
        let xz_cells = 16 / s;
        let sub_y_per_sec = 16 / s;
        let mut out = LodChunkVoxels::new_empty(pos, step, 0, 256);
        let hide_barriers = mcedit_core::hide_barriers();

        if let Some(NbtTag::List(sec_list)) = level.remove("Sections") {
            let mapper = self.mapper.read();
            for item in sec_list.items() {
                let NbtTag::Compound(sec_nbt) = item else {
                    continue;
                };
                let sy = sec_nbt.get_byte("Y").unwrap_or(0) as i32;
                if !(0..=15).contains(&sy) {
                    continue;
                }
                let Some(b_arr) = sec_nbt.get_byte_array("Blocks") else {
                    continue;
                };
                if b_arr.len() != SECTION_VOLUME {
                    continue;
                }
                let add_u8 = sec_nbt
                    .get_byte_array("Add")
                    .filter(|a| a.len() == LIGHT_BYTES)
                    .map(|a| unsafe {
                        std::slice::from_raw_parts(a.as_ptr() as *const u8, a.len())
                    });
                let data_u8 = sec_nbt
                    .get_byte_array("Data")
                    .filter(|d| d.len() == LIGHT_BYTES)
                    .map(|d| unsafe {
                        std::slice::from_raw_parts(d.as_ptr() as *const u8, d.len())
                    });

                let base_cy = (sy as usize) * sub_y_per_sec;
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

                            // Top-down layer by layer so surface blocks (grass over dirt, OR water
                            // at Y=62 over shallow seabed/riverbed at Y=60..61) take precedence.
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
                                        let low_id = (b_arr[i] as u8) as u16;
                                        let high_id = add_u8
                                            .map(|a| (get_nibble(a, i) as u16) << 8)
                                            .unwrap_or(0);
                                        let full_id = low_id | high_id;
                                        if full_id == 0 {
                                            continue;
                                        }
                                        let dval = data_u8.map(|d| get_nibble(d, i)).unwrap_or(0);
                                        let sid = mapper.to_canonical_readonly(full_id, dval);
                                        if sid == BlockStateId::AIR {
                                            continue;
                                        }
                                        let meta = registry.get_meta(sid);
                                        if meta.is_air || (hide_barriers && meta.is_barrier) {
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

        let mut level = chunk.extra_nbt.clone();
        level.insert("xPos", chunk.pos.cx());
        level.insert("zPos", chunk.pos.cz());
        level.insert("TerrainPopulated", chunk.terrain_populated);
        if !level.contains_key("LightPopulated") {
            level.insert("LightPopulated", true);
        }
        if !level.contains_key("LastUpdate") {
            level.insert("LastUpdate", 0i64);
        }

        let mut biome_bytes = vec![1i8; 256];
        match &chunk.biomes {
            BiomeStorage::Columns2D(cols) => {
                for (i, &b) in cols.iter().enumerate() {
                    biome_bytes[i] = b as i8;
                }
            }
            BiomeStorage::Cells3D(_) => {
                for z in 0..16 {
                    for x in 0..16 {
                        biome_bytes[(z << 4) | x] = chunk.biomes.get(x, 0, z) as i8;
                    }
                }
            }
        }
        level.insert("Biomes", NbtTag::ByteArray(biome_bytes));
        level.insert("HeightMap", NbtTag::IntArray(chunk.heightmap.to_vec()));

        let ents = NbtList::from_items(
            chunk
                .entities
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        level.insert("Entities", ents);

        let bes = NbtList::from_items(
            chunk
                .block_entities
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        level.insert("TileEntities", bes);

        let tts = NbtList::from_items(
            chunk
                .tile_ticks
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        level.insert("TileTicks", tts);

        let mapper = self.mapper.read();
        let mut sec_tags = Vec::new();
        for (&sy, sec) in &chunk.sections {
            if !(0..=15).contains(&sy) || sec.is_trivially_empty() {
                continue;
            }
            let mut blocks_arr = vec![0i8; SECTION_VOLUME];
            let mut add_arr = vec![0u8; LIGHT_BYTES];
            let mut data_arr = vec![0u8; LIGHT_BYTES];
            let mut has_add = false;

            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let i = (y << 8) | (z << 4) | x;
                        let sid = sec.get_block(x, y, z);
                        if sid != mcedit_core::BlockStateId::AIR {
                            let (bid, dval) = mapper.to_legacy(sid, registry);
                            blocks_arr[i] = (bid & 0xFF) as u8 as i8;
                            if bid > 255 {
                                has_add = true;
                                set_nibble(&mut add_arr, i, ((bid >> 8) & 0x0F) as u8);
                            }
                            if dval != 0 {
                                set_nibble(&mut data_arr, i, dval);
                            }
                        }
                    }
                }
            }

            let mut sec_nbt = NbtCompound::new();
            sec_nbt.insert("Y", sy as i8);
            sec_nbt.insert("Blocks", NbtTag::ByteArray(blocks_arr));
            if has_add {
                sec_nbt.insert(
                    "Add",
                    NbtTag::ByteArray(add_arr.into_iter().map(|b| b as i8).collect()),
                );
            }
            sec_nbt.insert(
                "Data",
                NbtTag::ByteArray(data_arr.into_iter().map(|b| b as i8).collect()),
            );
            sec_nbt.insert(
                "BlockLight",
                NbtTag::ByteArray(sec.block_light.iter().map(|&b| b as i8).collect()),
            );
            sec_nbt.insert(
                "SkyLight",
                NbtTag::ByteArray(sec.sky_light.iter().map(|&b| b as i8).collect()),
            );
            sec_tags.push(NbtTag::Compound(sec_nbt));
        }

        level.insert("Sections", NbtList::from_items(sec_tags)?);

        let mut root = NbtCompound::new();
        root.insert("Level", level);
        let raw_nbt = write_nbt("", &root, Endianness::Big, Compression::None)?;
        let (lx, lz) = chunk.pos.region_local();
        rf_arc.write().write_chunk_nbt(lx, lz, &raw_nbt)?;
        Ok(())
    }

    fn delete_chunk(&self, dim: DimensionId, pos: ChunkPos) -> FormatResult<()> {
        let (rx, rz) = pos.region_pos();
        let Some(rf_arc) = self.get_cached_region(dim, rx, rz, false)? else {
            return Ok(());
        };
        let (lx, lz) = pos.region_local();
        rf_arc.write().delete_chunk(lx, lz)?;
        Ok(())
    }

    fn load_players(&self) -> FormatResult<Vec<PlayerData>> {
        let mut out = Vec::new();
        // Singleplayer from level.dat
        let (_, level_nbt) = mcedit_nbt::read_nbt_file(self.root.join("level.dat"), Endianness::Big)?;
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

        // Multiplayer playerdata/ (*.dat) or legacy players/
        for dir_name in ["playerdata", "players"] {
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
        let pdir = self.root.join("playerdata");
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
        for dir_name in ["playerdata", "players"] {
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
        let rdir = self.region_dir(dim);
        let mut total = RepairReport::default();
        if !rdir.is_dir() {
            return Ok(total);
        }
        for entry in fs::read_dir(&rdir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("mca") {
                let rep = RegionFile::repair(&path)?;
                total.files_scanned += rep.files_scanned;
                total.files_repaired += rep.files_repaired;
                total.chunks_recovered += rep.chunks_recovered;
                total.chunks_lost += rep.chunks_lost;
            }
        }
        Ok(total)
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
