use crate::block::{BlockRegistry, BlockStateId};
use crate::chunk::Chunk;
use crate::format::{FormatResult, WorldFormat};
use crate::math::{BoundingBox, ChunkPos};
use crate::undo::UndoStack;
use ahash::{AHashMap, AHashSet};
use glam::{DVec3, IVec3, UVec3};
use mcedit_nbt::{NbtCompound, NbtList, NbtTag};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DimensionId {
    Overworld,
    Nether,
    End,
    Custom(i32),
}

impl DimensionId {
    pub fn display_name(self) -> String {
        match self {
            DimensionId::Overworld => "Overworld (0)".to_string(),
            DimensionId::Nether => "Nether (-1)".to_string(),
            DimensionId::End => "The End (1)".to_string(),
            DimensionId::Custom(id) => format!("Dimension {id}"),
        }
    }

    pub fn from_id(id: i32) -> Self {
        match id {
            0 => DimensionId::Overworld,
            -1 => DimensionId::Nether,
            1 => DimensionId::End,
            other => DimensionId::Custom(other),
        }
    }

    pub fn as_i32(self) -> i32 {
        match self {
            DimensionId::Overworld => 0,
            DimensionId::Nether => -1,
            DimensionId::End => 1,
            DimensionId::Custom(id) => id,
        }
    }

    pub fn has_skylight(self) -> bool {
        !matches!(self, DimensionId::Nether | DimensionId::End)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorldMetadata {
    pub level_name: String,
    pub seed: i64,
    pub time: i64,
    pub day_time: i64,
    pub game_type: i32,
    pub spawn_pos: IVec3,
    pub data_version: Option<i32>,
    pub format_name: String,
    pub raw_nbt: NbtCompound,
}

impl WorldMetadata {
    pub fn sync_to_nbt(&mut self) {
        let data = match self.raw_nbt.get_compound_mut("Data") {
            Some(c) => c,
            None => {
                self.raw_nbt.insert("Data", NbtCompound::new());
                self.raw_nbt.get_compound_mut("Data").unwrap()
            }
        };
        data.insert("LevelName", self.level_name.clone());
        data.insert("RandomSeed", self.seed);
        if let Some(wgs) = data.get_compound_mut("WorldGenSettings") {
            wgs.insert("seed", self.seed);
        }
        data.insert("Time", self.time);
        data.insert("DayTime", self.day_time);
        data.insert("GameType", self.game_type);
        if let Some(spawn_comp) = data.get_compound_mut("spawn") {
            spawn_comp.insert(
                "pos",
                NbtTag::IntArray(vec![self.spawn_pos.x, self.spawn_pos.y, self.spawn_pos.z]),
            );
        }
        data.insert("SpawnX", self.spawn_pos.x);
        data.insert("SpawnY", self.spawn_pos.y);
        data.insert("SpawnZ", self.spawn_pos.z);
    }

    pub fn sync_from_nbt(&mut self) {
        if let Some(data) = self.raw_nbt.get_compound("Data") {
            if let Some(name) = data.get_string("LevelName") {
                self.level_name = name.to_string();
            }
            if let Some(seed) = data
                .get_long("RandomSeed")
                .or_else(|| data.get_compound("WorldGenSettings").and_then(|w| w.get_long("seed")))
            {
                self.seed = seed;
            }
            if let Some(time) = data.get_long("Time") {
                self.time = time;
            }
            if let Some(day_time) = data.get_long("DayTime") {
                self.day_time = day_time;
            }
            if let Some(gt) = data.get_int("GameType") {
                self.game_type = gt;
            }
            if let Some(spawn_comp) = data.get_compound("spawn")
                && let Some(pos_arr) = spawn_comp.get_int_array("pos")
                && pos_arr.len() >= 3
            {
                self.spawn_pos = IVec3::new(pos_arr[0], pos_arr[1], pos_arr[2]);
            } else {
                let sx = data.get_int("SpawnX").unwrap_or(self.spawn_pos.x);
                let sy = data.get_int("SpawnY").unwrap_or(self.spawn_pos.y);
                let sz = data.get_int("SpawnZ").unwrap_or(self.spawn_pos.z);
                self.spawn_pos = IVec3::new(sx, sy, sz);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerData {
    pub id: String,
    pub display_name: String,
    pub is_singleplayer: bool,
    pub dimension: DimensionId,
    pub pos: DVec3,
    pub rotation: [f32; 2],
    pub raw_nbt: NbtCompound,
    pub dirty: bool,
}

impl PlayerData {
    pub fn sync_to_nbt(&mut self) {
        if matches!(self.raw_nbt.get("Dimension"), Some(NbtTag::String(_))) {
            let dim_str = match self.dimension {
                DimensionId::Overworld => "minecraft:overworld".to_string(),
                DimensionId::Nether => "minecraft:the_nether".to_string(),
                DimensionId::End => "minecraft:the_end".to_string(),
                DimensionId::Custom(id) => format!("minecraft:dim_{id}"),
            };
            self.raw_nbt.insert("Dimension", dim_str);
        } else {
            self.raw_nbt.insert("Dimension", self.dimension.as_i32());
        }
        let pos_list = NbtList::from_items(vec![
            NbtTag::Double(self.pos.x),
            NbtTag::Double(self.pos.y),
            NbtTag::Double(self.pos.z),
        ])
        .unwrap();
        self.raw_nbt.insert("Pos", pos_list);
        let rot_list = NbtList::from_items(vec![
            NbtTag::Float(self.rotation[0]),
            NbtTag::Float(self.rotation[1]),
        ])
        .unwrap();
        self.raw_nbt.insert("Rotation", rot_list);
        self.dirty = true;
    }

    pub fn sync_from_nbt(&mut self) {
        if let Some(dim) = self.raw_nbt.get_int("Dimension") {
            self.dimension = DimensionId::from_id(dim);
        } else if let Some(dim_s) = self.raw_nbt.get_string("Dimension") {
            self.dimension = match dim_s {
                "minecraft:overworld" | "overworld" => DimensionId::Overworld,
                "minecraft:the_nether" | "the_nether" => DimensionId::Nether,
                "minecraft:the_end" | "the_end" => DimensionId::End,
                _ => DimensionId::Overworld,
            };
        }
        if let Some(pos_list) = self.raw_nbt.get_list("Pos")
            && pos_list.len() == 3
        {
            let nums: Vec<f64> = pos_list
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Double(v) => Some(*v),
                    NbtTag::Float(v) => Some(*v as f64),
                    _ => None,
                })
                .collect();
            if nums.len() == 3 {
                self.pos = DVec3::new(nums[0], nums[1], nums[2]);
            }
        }
        if let Some(rot_list) = self.raw_nbt.get_list("Rotation")
            && rot_list.len() == 2
        {
            let nums: Vec<f32> = rot_list
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Float(v) => Some(*v),
                    NbtTag::Double(v) => Some(*v as f32),
                    _ => None,
                })
                .collect();
            if nums.len() == 2 {
                self.rotation = [nums[0], nums[1]];
            }
        }
    }
}

/// Bounded 3D volume used for Copy/Paste, Clone, Brush previews, and `.schematic` files.
#[derive(Debug, Clone)]
pub struct SchematicVolume {
    pub size: UVec3,
    /// Stored in YZX order: `(y * length + z) * width + x`
    pub blocks: Vec<BlockStateId>,
    /// Optional 2D biomes in ZX order: `z * width + x`
    pub biomes: Option<Vec<u8>>,
    pub entities: Vec<NbtCompound>,
    pub block_entities: Vec<NbtCompound>,
    pub tile_ticks: Vec<NbtCompound>,
}

impl SchematicVolume {
    pub fn new(size: UVec3) -> Self {
        let total = (size.x as usize) * (size.y as usize) * (size.z as usize);
        Self {
            size,
            blocks: vec![BlockStateId::AIR; total],
            biomes: None,
            entities: Vec::new(),
            block_entities: Vec::new(),
            tile_ticks: Vec::new(),
        }
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32, z: u32) -> usize {
        ((y as usize) * (self.size.z as usize) + (z as usize)) * (self.size.x as usize) + (x as usize)
    }

    #[inline]
    pub fn get_block(&self, x: u32, y: u32, z: u32) -> BlockStateId {
        if x >= self.size.x || y >= self.size.y || z >= self.size.z {
            return BlockStateId::AIR;
        }
        self.blocks[self.index(x, y, z)]
    }

    #[inline]
    pub fn set_block(&mut self, x: u32, y: u32, z: u32, id: BlockStateId) {
        if x < self.size.x && y < self.size.y && z < self.size.z {
            let i = self.index(x, y, z);
            self.blocks[i] = id;
        }
    }
}

pub struct World {
    pub format: Arc<dyn WorldFormat>,
    pub registry: Arc<RwLock<BlockRegistry>>,
    pub metadata: WorldMetadata,
    pub players: Vec<PlayerData>,
    pub deleted_players: Vec<String>,
    pub active_dim: DimensionId,
    pub dimensions: Vec<DimensionId>,
    pub known_chunks: AHashMap<DimensionId, AHashSet<ChunkPos>>,
    pub known_regions: AHashMap<DimensionId, AHashSet<(i32, i32)>>,
    pub dirty_chunks: AHashMap<(DimensionId, ChunkPos), Option<Chunk>>,
    clean_cache: AHashMap<(DimensionId, ChunkPos), Chunk>,
    readonly_clean_chunks: AHashSet<(DimensionId, ChunkPos)>,
    cache_order: VecDeque<(DimensionId, ChunkPos)>,
    cache_capacity: usize,
    pub undo_stack: UndoStack,
    pub metadata_dirty: bool,
}

impl World {
    pub fn open(
        format: Box<dyn WorldFormat>,
        registry: Arc<RwLock<BlockRegistry>>,
        undo_limit: usize,
    ) -> FormatResult<Self> {
        let format: Arc<dyn WorldFormat> = Arc::from(format);
        format.acquire_session_lock()?;
        let metadata = format.load_metadata()?;
        let players = format.load_players()?;
        let mut dimensions = format.dimensions();
        if dimensions.is_empty() {
            dimensions.push(DimensionId::Overworld);
        }
        let mut known_chunks = AHashMap::new();
        let mut known_regions = AHashMap::new();
        for &dim in &dimensions {
            let list = format.list_chunks(dim)?;
            known_chunks.insert(dim, list.into_iter().collect());
            let regs = format.list_regions(dim)?;
            known_regions.insert(dim, regs.into_iter().collect());
        }

        let active_dim = players
            .first()
            .map(|p| p.dimension)
            .filter(|d| dimensions.contains(d))
            .unwrap_or(DimensionId::Overworld);

        Ok(Self {
            format,
            registry,
            metadata,
            players,
            deleted_players: Vec::new(),
            active_dim,
            dimensions,
            known_chunks,
            known_regions,
            dirty_chunks: AHashMap::new(),
            clean_cache: AHashMap::new(),
            readonly_clean_chunks: AHashSet::new(),
            cache_order: VecDeque::new(),
            cache_capacity: 4096,
            undo_stack: UndoStack::new(undo_limit),
            metadata_dirty: false,
        })
    }

    pub fn insert_loaded_clean_chunk(&mut self, dim: DimensionId, chunk: Chunk) {
        let key = (dim, chunk.pos);
        if !self.dirty_chunks.contains_key(&key) {
            self.readonly_clean_chunks.insert(key);
            self.insert_clean_cache(key, chunk);
        }
    }

    pub fn min_y(&self) -> i32 {
        self.format.min_section_y() * 16
    }

    pub fn max_y(&self) -> i32 {
        (self.format.max_section_y() + 1) * 16
    }

    pub fn chunk_exists(&self, dim: DimensionId, pos: ChunkPos) -> bool {
        if let Some(dirty_entry) = self.dirty_chunks.get(&(dim, pos)) {
            return dirty_entry.is_some();
        }
        self.known_chunks
            .get(&dim)
            .is_some_and(|set| set.contains(&pos))
    }

    pub fn region_exists(&self, dim: DimensionId, pos: ChunkPos) -> bool {
        let rpos = pos.region_pos();
        if self
            .known_regions
            .get(&dim)
            .is_some_and(|set| set.contains(&rpos))
        {
            return true;
        }
        self.chunk_exists(dim, pos)
    }

    pub fn list_chunks(&self, dim: DimensionId) -> Vec<ChunkPos> {
        let mut out: Vec<ChunkPos> = self
            .known_chunks
            .get(&dim)
            .map(|s| {
                s.iter()
                    .copied()
                    .filter(|p| !matches!(self.dirty_chunks.get(&(dim, *p)), Some(None)))
                    .collect()
            })
            .unwrap_or_default();
        for ((d, p), entry) in &self.dirty_chunks {
            if *d == dim && entry.is_some() && !self.known_chunks.get(&dim).is_some_and(|s| s.contains(p)) {
                out.push(*p);
            }
        }
        out
    }

    pub fn get_loaded_chunk(&self, dim: DimensionId, pos: ChunkPos) -> Option<&Chunk> {
        if let Some(entry) = self.dirty_chunks.get(&(dim, pos)) {
            return entry.as_ref();
        }
        self.clean_cache.get(&(dim, pos))
    }

    pub fn get_chunk(&mut self, dim: DimensionId, pos: ChunkPos) -> FormatResult<Option<&Chunk>> {
        let key = (dim, pos);
        if self.dirty_chunks.contains_key(&key) {
            return Ok(self.dirty_chunks.get(&key).and_then(|o| o.as_ref()));
        }
        if self.readonly_clean_chunks.remove(&key) {
            self.clean_cache.remove(&key);
        }
        if !self.clean_cache.contains_key(&key) {
            if !self.known_chunks.get(&dim).is_some_and(|s| s.contains(&pos)) {
                return Ok(None);
            }
            let loaded = {
                let reg_arc = self.registry.clone();
                let mut reg = reg_arc.write();
                self.format.load_chunk(dim, pos, &mut reg)?
            };
            if let Some(chunk) = loaded {
                self.insert_clean_cache(key, chunk);
            } else {
                if let Some(set) = self.known_chunks.get_mut(&dim) {
                    set.remove(&pos);
                }
                return Ok(None);
            }
        }
        Ok(self.clean_cache.get(&key))
    }

    pub fn get_chunk_mut(&mut self, dim: DimensionId, pos: ChunkPos) -> FormatResult<Option<&mut Chunk>> {
        let key = (dim, pos);
        if !self.dirty_chunks.contains_key(&key) {
            if self.readonly_clean_chunks.remove(&key) {
                self.clean_cache.remove(&key);
            }
            if let Some(chunk) = self.clean_cache.remove(&key) {
                self.dirty_chunks.insert(key, Some(chunk));
            } else if self.known_chunks.get(&dim).is_some_and(|s| s.contains(&pos)) {
                let mut reg = self.registry.write();
                if let Some(chunk) = self.format.load_chunk(dim, pos, &mut reg)? {
                    self.dirty_chunks.insert(key, Some(chunk));
                } else {
                    if let Some(set) = self.known_chunks.get_mut(&dim) {
                        set.remove(&pos);
                    }
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }
        Ok(self.dirty_chunks.get_mut(&key).and_then(|o| o.as_mut()))
    }

    pub fn insert_chunk(&mut self, dim: DimensionId, mut chunk: Chunk) {
        let pos = chunk.pos;
        chunk.dirty = true;
        self.readonly_clean_chunks.remove(&(dim, pos));
        self.clean_cache.remove(&(dim, pos));
        self.known_chunks.entry(dim).or_default().insert(pos);
        self.known_regions
            .entry(dim)
            .or_default()
            .insert(pos.region_pos());
        self.dirty_chunks.insert((dim, pos), Some(chunk));
    }

    pub fn mark_chunk_deleted(&mut self, dim: DimensionId, pos: ChunkPos) {
        self.clean_cache.remove(&(dim, pos));
        self.dirty_chunks.insert((dim, pos), None);
    }

    pub fn get_block(&mut self, dim: DimensionId, pos: IVec3) -> BlockStateId {
        if pos.y < self.min_y() || pos.y >= self.max_y() {
            return BlockStateId::AIR;
        }
        let cp = ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
        match self.get_chunk(dim, cp) {
            Ok(Some(c)) => c.get_block(
                pos.x.rem_euclid(16) as usize,
                pos.y,
                pos.z.rem_euclid(16) as usize,
            ),
            _ => BlockStateId::AIR,
        }
    }

    pub fn set_block(&mut self, dim: DimensionId, pos: IVec3, block: BlockStateId) -> bool {
        if pos.y < self.min_y() || pos.y >= self.max_y() {
            return false;
        }
        let cp = ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
        if let Ok(Some(c)) = self.get_chunk_mut(dim, cp) {
            c.set_block(
                pos.x.rem_euclid(16) as usize,
                pos.y,
                pos.z.rem_euclid(16) as usize,
                block,
            )
        } else {
            false
        }
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.undo_stack.is_dirty()
            || !self.dirty_chunks.is_empty()
            || self.metadata_dirty
            || self.players.iter().any(|p| p.dirty)
            || !self.deleted_players.is_empty()
    }

    pub fn save_all(&mut self) -> FormatResult<()> {
        if !self.format.check_session_lock()? {
            return Err(crate::format::FormatError::SessionLockLost);
        }
        let reg_arc = self.registry.clone();
        let dirty_keys: Vec<(DimensionId, ChunkPos)> = self.dirty_chunks.keys().copied().collect();
        for (dim, pos) in dirty_keys {
            if let Some(opt_chunk) = self.dirty_chunks.remove(&(dim, pos)) {
                match opt_chunk {
                    Some(mut chunk) => {
                        {
                            let reg = reg_arc.read();
                            chunk.recompute_heightmap(&reg);
                            chunk.prune_empty_sections();
                            chunk.dirty = false;
                            self.format.save_chunk(dim, &chunk, &reg)?;
                        }
                        self.known_chunks.entry(dim).or_default().insert(pos);
                        self.insert_clean_cache((dim, pos), chunk);
                    }
                    None => {
                        self.format.delete_chunk(dim, pos)?;
                        if let Some(set) = self.known_chunks.get_mut(&dim) {
                            set.remove(&pos);
                        }
                    }
                }
            }
        }

        for player in &mut self.players {
            if player.dirty {
                if player.is_singleplayer {
                    let raw = NbtTag::Compound(player.raw_nbt.clone());
                    if let Some(data) = self.metadata.raw_nbt.get_compound_mut("Data") {
                        data.insert("Player", raw);
                    }
                    self.metadata_dirty = true;
                } else {
                    self.format.save_player(player)?;
                }
                player.dirty = false;
            }
        }
        for del_id in self.deleted_players.drain(..) {
            self.format.delete_player(&del_id)?;
        }

        if self.metadata_dirty {
            self.metadata.sync_to_nbt();
            self.format.save_metadata(&self.metadata)?;
            self.metadata_dirty = false;
        }

        self.undo_stack.mark_saved();
        Ok(())
    }

    pub fn ensure_clean_chunks_loaded(&mut self, dim: DimensionId, coords: impl Iterator<Item = ChunkPos>) -> FormatResult<()> {
        for pos in coords {
            let _ = self.get_chunk(dim, pos)?;
        }
        Ok(())
    }

    pub fn bounding_box_chunks(&self, box_: BoundingBox) -> Vec<ChunkPos> {
        box_.iter_chunks().collect()
    }

    fn insert_clean_cache(&mut self, key: (DimensionId, ChunkPos), chunk: Chunk) {
        if self.clean_cache.len() >= self.cache_capacity
            && let Some(evict_key) = self.cache_order.pop_front()
        {
            self.clean_cache.remove(&evict_key);
        }
        self.clean_cache.insert(key, chunk);
        self.cache_order.push_back(key);
    }
}
