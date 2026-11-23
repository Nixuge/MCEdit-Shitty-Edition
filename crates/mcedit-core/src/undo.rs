use crate::block::{BlockRegistry, BlockState, BlockStateId};
use crate::chunk::{BiomeStorage, Chunk};
use crate::math::{BoundingBox, ChunkPos};
use crate::section::{ChunkSection, LIGHT_BYTES, SECTION_VOLUME};
use crate::world::{DimensionId, PlayerData, World, WorldMetadata};
use ahash::AHashMap;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use mcedit_nbt::NbtCompound;
use std::collections::BTreeMap;
use std::io::{Read, Write};

#[derive(Debug, Clone)]
pub struct CompressedSection {
    palette: Vec<BlockState>,
    compressed_payload: Vec<u8>,
    non_air_count: u16,
}

impl CompressedSection {
    pub fn from_section(sec: &ChunkSection, reg: &BlockRegistry) -> Self {
        let palette: Vec<BlockState> = sec
            .palette
            .iter()
            .map(|&id| reg.get_state(id).clone())
            .collect();

        let mut raw = Vec::with_capacity(SECTION_VOLUME * 2 + LIGHT_BYTES * 2);
        for &idx in &sec.indices {
            raw.extend_from_slice(&idx.to_le_bytes());
        }
        raw.extend_from_slice(&sec.block_light);
        raw.extend_from_slice(&sec.sky_light);

        let mut enc = DeflateEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&raw).expect("deflate write");
        let compressed_payload = enc.finish().expect("deflate finish");

        Self {
            palette,
            compressed_payload,
            non_air_count: sec.non_air_count,
        }
    }

    pub fn to_section(&self, reg: &mut BlockRegistry) -> ChunkSection {
        let palette: Vec<BlockStateId> = self
            .palette
            .iter()
            .map(|s| reg.intern(s.clone()))
            .collect();

        let mut dec = DeflateDecoder::new(&self.compressed_payload[..]);
        let mut raw = Vec::with_capacity(SECTION_VOLUME * 2 + LIGHT_BYTES * 2);
        dec.read_to_end(&mut raw).expect("deflate read");

        let mut indices = Vec::with_capacity(SECTION_VOLUME);
        for i in 0..SECTION_VOLUME {
            let lo = raw[i * 2];
            let hi = raw[i * 2 + 1];
            indices.push(u16::from_le_bytes([lo, hi]));
        }
        let off = SECTION_VOLUME * 2;
        let block_light = raw[off..off + LIGHT_BYTES].to_vec();
        let sky_light = raw[off + LIGHT_BYTES..off + LIGHT_BYTES * 2].to_vec();

        ChunkSection {
            palette,
            indices,
            non_air_count: self.non_air_count,
            block_light,
            sky_light,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkSnapshot {
    pub pos: ChunkPos,
    pub min_section_y: i32,
    pub max_section_y: i32,
    pub sections: BTreeMap<i32, CompressedSection>,
    pub biomes: BiomeStorage,
    pub heightmap: [i32; 256],
    pub entities: Vec<NbtCompound>,
    pub block_entities: Vec<NbtCompound>,
    pub tile_ticks: Vec<NbtCompound>,
    pub terrain_populated: bool,
    pub extra_nbt: NbtCompound,
}

impl ChunkSnapshot {
    pub fn from_chunk(chunk: &Chunk, reg: &BlockRegistry) -> Self {
        let sections = chunk
            .sections
            .iter()
            .map(|(&sy, sec)| (sy, CompressedSection::from_section(sec, reg)))
            .collect();
        Self {
            pos: chunk.pos,
            min_section_y: chunk.min_section_y,
            max_section_y: chunk.max_section_y,
            sections,
            biomes: chunk.biomes.clone(),
            heightmap: chunk.heightmap,
            entities: chunk.entities.clone(),
            block_entities: chunk.block_entities.clone(),
            tile_ticks: chunk.tile_ticks.clone(),
            terrain_populated: chunk.terrain_populated,
            extra_nbt: chunk.extra_nbt.clone(),
        }
    }

    pub fn to_chunk(&self, reg: &mut BlockRegistry) -> Chunk {
        let sections = self
            .sections
            .iter()
            .map(|(&sy, cs)| (sy, cs.to_section(reg)))
            .collect();
        Chunk {
            pos: self.pos,
            min_section_y: self.min_section_y,
            max_section_y: self.max_section_y,
            sections,
            biomes: self.biomes.clone(),
            heightmap: self.heightmap,
            entities: self.entities.clone(),
            block_entities: self.block_entities.clone(),
            tile_ticks: self.tile_ticks.clone(),
            terrain_populated: self.terrain_populated,
            extra_nbt: self.extra_nbt.clone(),
            dirty: true,
            needs_lighting: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkDelta {
    pub pos: ChunkPos,
    pub before: Option<ChunkSnapshot>,
    pub after: Option<ChunkSnapshot>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum UndoKind {
    Chunks {
        dim: DimensionId,
        deltas: Vec<ChunkDelta>,
    },
    Metadata {
        before: WorldMetadata,
        after: WorldMetadata,
    },
    Players {
        before: Vec<PlayerData>,
        after: Vec<PlayerData>,
    },
}

#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub label: String,
    pub dirty_box: Option<BoundingBox>,
    pub kind: UndoKind,
}

#[derive(Debug)]
pub struct UndoStack {
    entries: Vec<UndoEntry>,
    cursor: usize,
    saved_cursor: Option<usize>,
    limit: usize,
}

impl UndoStack {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            saved_cursor: Some(0),
            limit: limit.max(1),
        }
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
    }

    pub fn push(&mut self, entry: UndoEntry) {
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
            if let Some(saved) = self.saved_cursor
                && saved > self.cursor
            {
                self.saved_cursor = None;
            }
        }
        self.entries.push(entry);
        self.cursor = self.entries.len();

        while self.entries.len() > self.limit {
            self.entries.remove(0);
            self.cursor -= 1;
            self.saved_cursor = self.saved_cursor.and_then(|s| s.checked_sub(1));
        }
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_cursor != Some(self.cursor)
    }

    pub fn mark_saved(&mut self) {
        self.saved_cursor = Some(self.cursor);
    }

    pub fn peek_undo_label(&self) -> Option<&str> {
        self.cursor.checked_sub(1).map(|i| self.entries[i].label.as_str())
    }

    pub fn peek_redo_label(&self) -> Option<&str> {
        self.entries.get(self.cursor).map(|e| e.label.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl World {
    pub fn undo(&mut self) -> Option<UndoEntry> {
        if !self.undo_stack.can_undo() {
            return None;
        }
        self.undo_stack.cursor -= 1;
        let entry = self.undo_stack.entries[self.undo_stack.cursor].clone();
        self.apply_undo_entry(&entry, true);
        Some(entry)
    }

    pub fn redo(&mut self) -> Option<UndoEntry> {
        if !self.undo_stack.can_redo() {
            return None;
        }
        let entry = self.undo_stack.entries[self.undo_stack.cursor].clone();
        self.undo_stack.cursor += 1;
        self.apply_undo_entry(&entry, false);
        Some(entry)
    }

    fn apply_undo_entry(&mut self, entry: &UndoEntry, reverse: bool) {
        match &entry.kind {
            UndoKind::Chunks { dim, deltas } => {
                let mut reg = self.registry.write();
                for d in deltas {
                    let target = if reverse { &d.before } else { &d.after };
                    match target {
                        Some(snap) => {
                            let chunk = snap.to_chunk(&mut reg);
                            self.dirty_chunks.insert((*dim, d.pos), Some(chunk));
                            self.known_chunks.entry(*dim).or_default().insert(d.pos);
                        }
                        None => {
                            self.dirty_chunks.insert((*dim, d.pos), None);
                        }
                    }
                }
            }
            UndoKind::Metadata { before, after } => {
                self.metadata = if reverse { before.clone() } else { after.clone() };
                self.metadata_dirty = true;
            }
            UndoKind::Players { before, after } => {
                self.players = if reverse { before.clone() } else { after.clone() };
                for p in &mut self.players {
                    p.dirty = true;
                }
            }
        }
    }
}

/// Copy-on-write transaction recording before/after chunk snapshots for atomic Undo/Redo.
pub struct EditTransaction {
    label: String,
    dim: DimensionId,
    dirty_box: Option<BoundingBox>,
    before_chunks: AHashMap<ChunkPos, Option<ChunkSnapshot>>,
}

impl EditTransaction {
    pub fn new(label: impl Into<String>, dim: DimensionId, dirty_box: Option<BoundingBox>) -> Self {
        Self {
            label: label.into(),
            dim,
            dirty_box,
            before_chunks: AHashMap::new(),
        }
    }

    pub fn record_chunk_if_needed(&mut self, world: &mut World, pos: ChunkPos) {
        if self.before_chunks.contains_key(&pos) {
            return;
        }
        let reg_arc = world.registry.clone();
        let snap = match world.get_chunk(self.dim, pos) {
            Ok(Some(c)) => {
                let reg = reg_arc.read();
                Some(ChunkSnapshot::from_chunk(c, &reg))
            }
            _ => None,
        };
        self.before_chunks.insert(pos, snap);
    }

    pub fn expand_dirty_box(&mut self, box_: BoundingBox) {
        self.dirty_box = Some(match self.dirty_box {
            Some(prev) => prev.union(box_),
            None => box_,
        });
    }

    pub fn commit(self, world: &mut World) {
        if self.before_chunks.is_empty() {
            return;
        }
        let reg_arc = world.registry.clone();
        let mut deltas = Vec::with_capacity(self.before_chunks.len());
        for (pos, before) in self.before_chunks {
            let after = match world.get_chunk(self.dim, pos) {
                Ok(Some(c)) => {
                    let reg = reg_arc.read();
                    Some(ChunkSnapshot::from_chunk(c, &reg))
                }
                _ => None,
            };
            deltas.push(ChunkDelta { pos, before, after });
        }
        world.undo_stack.push(UndoEntry {
            label: self.label,
            dirty_box: self.dirty_box,
            kind: UndoKind::Chunks {
                dim: self.dim,
                deltas,
            },
        });
    }
}
