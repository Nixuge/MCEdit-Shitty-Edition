use crate::legacy_mapper::LegacyIdMapper;
use glam::UVec3;
use mcedit_core::{
    BlockRegistry, BlockStateId, FormatError, FormatResult, SchematicFormat, SchematicVolume,
};
use mcedit_nbt::{Compression, Endianness, NbtCompound, NbtList, NbtTag};
use std::path::Path;

pub struct McSchematicFormat;

impl SchematicFormat for McSchematicFormat {
    fn format_name(&self) -> &'static str {
        "MCEdit Schematic (.schematic)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["schematic"]
    }

    fn load(&self, path: &Path, registry: &mut BlockRegistry) -> FormatResult<SchematicVolume> {
        let (root_name, mut root) = mcedit_nbt::read_nbt_file(path, Endianness::Big)?;
        if root_name != "Schematic" && !root.contains_key("Blocks") {
            return Err(FormatError::UnsupportedFormat(path.display().to_string()));
        }

        let w = root.get_short("Width").unwrap_or(0).max(0) as u32;
        let h = root.get_short("Height").unwrap_or(0).max(0) as u32;
        let l = root.get_short("Length").unwrap_or(0).max(0) as u32;

        let mut vol = SchematicVolume::new(UVec3::new(w, h, l));
        let total = (w as usize) * (h as usize) * (l as usize);

        let blocks = root
            .get_byte_array("Blocks")
            .ok_or_else(|| FormatError::Corrupt("schematic missing Blocks".to_string()))?;
        let data = root.get_byte_array("Data");
        let add_blocks = root.get_byte_array("AddBlocks");

        if blocks.len() < total {
            return Err(FormatError::Corrupt("schematic Blocks length mismatch".to_string()));
        }

        let mut mapper = LegacyIdMapper::from_registry(registry);
        for (i, &b) in blocks.iter().enumerate().take(total) {
            let low_id = (b as u8) as u16;
            // WorldEdit AddBlocks: high nibble first (even index -> >> 4, odd index -> & 0x0F)
            let high_id = add_blocks
                .and_then(|ab| ab.get(i >> 1).copied())
                .map(|b| {
                    let ub = b as u8;
                    let nib = if i & 1 == 0 { (ub >> 4) & 0x0F } else { ub & 0x0F };
                    (nib as u16) << 8
                })
                .unwrap_or(0);
            let full_id = low_id | high_id;
            let dval = data.and_then(|d| d.get(i).copied()).unwrap_or(0) as u8 & 0x0F;
            if full_id != 0 || dval != 0 {
                vol.blocks[i] = mapper.to_canonical(full_id, dval, registry);
            }
        }

        if let Some(biomes) = root.get_byte_array("Biomes")
            && biomes.len() == (w as usize) * (l as usize)
        {
            vol.biomes = Some(biomes.iter().map(|&b| b as u8).collect());
        }

        if let Some(NbtTag::List(ents)) = root.remove("Entities") {
            vol.entities = ents
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }
        if let Some(NbtTag::List(bes)) = root.remove("TileEntities") {
            vol.block_entities = bes
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }
        if let Some(NbtTag::List(tts)) = root.remove("TileTicks") {
            vol.tile_ticks = tts
                .items()
                .iter()
                .filter_map(|t| match t {
                    NbtTag::Compound(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
        }

        Ok(vol)
    }

    fn save(
        &self,
        path: &Path,
        volume: &SchematicVolume,
        registry: &BlockRegistry,
    ) -> FormatResult<()> {
        let mapper = LegacyIdMapper::from_registry(registry);
        let total = volume.blocks.len();
        let mut blocks = vec![0i8; total];
        let mut data = vec![0i8; total];
        let mut add_blocks = vec![0u8; total.div_ceil(2)];
        let mut has_add = false;

        for (i, &sid) in volume.blocks.iter().enumerate() {
            if sid != BlockStateId::AIR {
                let (bid, dval) = mapper.to_legacy(sid, registry);
                blocks[i] = (bid & 0xFF) as u8 as i8;
                data[i] = (dval & 0x0F) as i8;
                if bid > 255 {
                    has_add = true;
                    let high = ((bid >> 8) & 0x0F) as u8;
                    if i & 1 == 0 {
                        add_blocks[i >> 1] |= high << 4;
                    } else {
                        add_blocks[i >> 1] |= high;
                    }
                }
            }
        }

        let mut root = NbtCompound::new();
        root.insert("Width", volume.size.x as i16);
        root.insert("Height", volume.size.y as i16);
        root.insert("Length", volume.size.z as i16);
        root.insert("Materials", "Alpha".to_string());
        root.insert("Blocks", NbtTag::ByteArray(blocks));
        root.insert("Data", NbtTag::ByteArray(data));
        if has_add {
            root.insert(
                "AddBlocks",
                NbtTag::ByteArray(add_blocks.into_iter().map(|b| b as i8).collect()),
            );
        }
        if let Some(biomes) = &volume.biomes {
            root.insert(
                "Biomes",
                NbtTag::ByteArray(biomes.iter().map(|&b| b as i8).collect()),
            );
        }

        let ents = NbtList::from_items(
            volume
                .entities
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        root.insert("Entities", ents);

        let bes = NbtList::from_items(
            volume
                .block_entities
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        root.insert("TileEntities", bes);

        let tts = NbtList::from_items(
            volume
                .tile_ticks
                .iter()
                .cloned()
                .map(NbtTag::Compound)
                .collect(),
        )?;
        root.insert("TileTicks", tts);

        mcedit_nbt::write_nbt_file(
            path,
            "Schematic",
            &root,
            Endianness::Big,
            Compression::Gzip,
        )?;
        Ok(())
    }
}
