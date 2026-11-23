pub mod anvil;
pub mod legacy_mapper;
pub mod registry;
pub mod schematic;

pub use anvil::post113::{
    AnvilPost113Format, CAVES_AND_CLIFFS_DATA_VERSION, DEFAULT_MODERN_DATA_VERSION,
    MIN_POST113_DATA_VERSION, PADDED_BITPACK_DATA_VERSION, SEPARATE_ENTITIES_DATA_VERSION,
    pack_bit_indices, unpack_bit_indices,
};
pub use anvil::pre113::AnvilPre113Format;
pub use anvil::region::RegionFile;
pub use legacy_mapper::LegacyIdMapper;
pub use registry::FormatRegistry;
pub use schematic::classic::McSchematicFormat;

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{IVec3, UVec3};
    use mcedit_core::{
        BlockRegistry, BlockState, BoundingBox, ChunkPos, CopyOptions, DimensionId,
        EditTransaction, LightingEngine, SchematicVolume, World, WorldFormat, analyze_box,
        create_flatland_chunk, default_flatland_layers, fill_blocks, paste_volume,
    };
    use mcedit_nbt::{Compression, Endianness, NbtCompound};
    use parking_lot::RwLock;
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn anvil_pre113_roundtrip_12bit_and_modded_blocks() {
        let dir = tempdir().unwrap();
        let reg = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));

        // Create world
        let fmt = AnvilPre113Format::create_new(
            dir.path(),
            "Test World",
            123456,
            1,
            IVec3::new(0, 5, 0),
            &reg.read(),
        )
        .unwrap();

        let mut world = World::open(Box::new(fmt), reg.clone(), 20).unwrap();

        // Create chunk (0, 0) and (-1, -1)
        {
            let mut r = reg.write();
            let c0 = create_flatland_chunk(
                ChunkPos::new(0, 0),
                0,
                15,
                &default_flatland_layers(),
                &mut r,
            );
            let c1 = create_flatland_chunk(
                ChunkPos::new(-1, -1),
                0,
                15,
                &default_flatland_layers(),
                &mut r,
            );
            world.insert_chunk(DimensionId::Overworld, c0);
            world.insert_chunk(DimensionId::Overworld, c1);
        }

        // Put a 12-bit modded block (>255) and a block with data
        let modded_id = {
            let mut mapper = LegacyIdMapper::from_registry(&reg.read());
            let mut r = reg.write();
            mapper.to_canonical(1025, 7, &mut r)
        };
        assert!(world.set_block(DimensionId::Overworld, IVec3::new(-3, 20, -4), modded_id));

        let diamond_block = reg.read().find_by_legacy(57, 0).unwrap();
        let mut tx = EditTransaction::new(
            "Fill Diamond",
            DimensionId::Overworld,
            Some(BoundingBox::from_min_max(IVec3::new(0, 4, 0), IVec3::new(4, 6, 4))),
        );
        fill_blocks(
            &mut world,
            DimensionId::Overworld,
            BoundingBox::from_min_max(IVec3::new(0, 4, 0), IVec3::new(4, 6, 4)),
            diamond_block,
            None,
            false,
            &mut tx,
        );
        tx.commit(&mut world);

        LightingEngine::relight_chunks(
            &mut world,
            DimensionId::Overworld,
            &[ChunkPos::new(0, 0), ChunkPos::new(-1, -1)],
        );
        world.save_all().unwrap();
        drop(world);

        // Reload with fresh registry to verify lossless modded block round-trip
        let reg2 = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let fmt2 = FormatRegistry::open_world(dir.path(), &reg2.read()).unwrap();
        let mut world2 = World::open(fmt2, reg2.clone(), 20).unwrap();
        assert_eq!(world2.metadata.level_name, "Test World");
        let got_diamond = world2.get_block(DimensionId::Overworld, IVec3::new(2, 5, 2));
        assert_eq!(
            reg2.read().get_state(got_diamond).name,
            "minecraft:diamond_block"
        );
        let got_modded = world2.get_block(DimensionId::Overworld, IVec3::new(-3, 20, -4));
        assert_eq!(
            reg2.read().get_state(got_modded),
            &BlockState::new("mcedit:legacy_1025").with_prop("data", "7")
        );
    }

    #[test]
    fn schematic_addblocks_high_nibble_first_roundtrip() {
        let dir = tempdir().unwrap();
        let schem_path = dir.path().join("test.schematic");
        let reg = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));

        let modded0 = {
            let mut mapper = LegacyIdMapper::from_registry(&reg.read());
            let mut r = reg.write();
            mapper.to_canonical(513, 2, &mut r)
        };
        let modded1 = {
            let mut mapper = LegacyIdMapper::from_registry(&reg.read());
            let mut r = reg.write();
            mapper.to_canonical(770, 5, &mut r)
        };

        let mut vol = SchematicVolume::new(UVec3::new(2, 1, 1));
        vol.set_block(0, 0, 0, modded0);
        vol.set_block(1, 0, 0, modded1);

        FormatRegistry::save_schematic(&schem_path, &vol, &reg.read()).unwrap();

        // Check raw AddBlocks byte directly: 513 -> high=2, 770 -> high=3 => 0x23
        let (_, nbt) = mcedit_nbt::read_nbt_file(&schem_path, Endianness::Big).unwrap();
        assert_eq!(nbt.get_byte_array("AddBlocks").unwrap(), &[0x23i8]);

        let mut reg2 = BlockRegistry::with_pre113_defaults();
        let loaded = FormatRegistry::open_schematic(&schem_path, &mut reg2).unwrap();
        assert_eq!(
            reg2.get_state(loaded.get_block(0, 0, 0)),
            &BlockState::new("mcedit:legacy_513").with_prop("data", "2")
        );
        assert_eq!(
            reg2.get_state(loaded.get_block(1, 0, 0)),
            &BlockState::new("mcedit:legacy_770").with_prop("data", "5")
        );
    }

    #[test]
    fn anvil_post113_unpadded_114_and_padded_120_negative_y_entities_biomes_roundtrip() {
        // 1. Verify 5-bit unpadded (1.13-1.15, straddling 64-bit longs) vs padded (1.16+) bit packing
        let mut indices = vec![0u16; 4096];
        for (i, slot) in indices.iter_mut().enumerate() {
            *slot = (i % 23) as u16; // 23 palette entries -> 5 bits per entry
        }
        let unpadded_longs = pack_bit_indices(&indices, 23, 4, false);
        assert_eq!(unpadded_longs.len(), (4096 * 5) / 64); // 320 longs (straddles 64-bit boundaries)
        let unpacked_unpadded = unpack_bit_indices(&unpadded_longs, 4096, 23, 4, false);
        assert_eq!(unpacked_unpadded, indices);

        let padded_longs = pack_bit_indices(&indices, 23, 4, true);
        assert_eq!(padded_longs.len(), 4096usize.div_ceil(64 / 5)); // 342 longs (12 entries per 64-bit word)
        let unpacked_padded = unpack_bit_indices(&padded_longs, 4096, 23, 4, true);
        assert_eq!(unpacked_padded, indices);

        // 2. Test full 1.20.4 (DataVersion 3700) world with negative sections (Y = -64..320),
        //    separate `entities/r.0.0.mca`, and 3D 4x4x4 paletted section biomes
        let dir = tempdir().unwrap();
        let reg = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let fmt = AnvilPost113Format::create_new(
            dir.path(),
            "Modern 1.20 World",
            987654321,
            1,
            IVec3::new(0, 64, 0),
            DEFAULT_MODERN_DATA_VERSION,
            &reg.read(),
        )
        .unwrap();
        assert_eq!(fmt.min_section_y(), -4);
        assert_eq!(fmt.max_section_y(), 19);

        let mut world = World::open(Box::new(fmt), reg.clone(), 20).unwrap();
        assert_eq!(world.min_y(), -64);
        assert_eq!(world.max_y(), 320);

        let deepslate = reg.write().intern(BlockState::new("minecraft:deepslate"));
        let grass_block = reg
            .write()
            .intern(BlockState::new("minecraft:grass_block").with_prop("snowy", "false"));
        let copper = reg.write().intern(BlockState::new("minecraft:copper_block"));

        let mut chunk = mcedit_core::Chunk::new(ChunkPos::new(0, 0), -4, 19);
        chunk.biomes = mcedit_core::BiomeStorage::Cells3D( std::collections::BTreeMap::new() );
        chunk.biomes.set(4, -60, 4, 174); // minecraft:dripstone_caves at Y=-60
        chunk.biomes.set(4, 70, 4, 132); // minecraft:flower_forest at Y=70
        chunk.set_block(2, -64, 3, deepslate);
        chunk.set_block(5, 64, 5, grass_block);
        chunk.set_block(8, 310, 8, copper);

        let mut ent = NbtCompound::new();
        ent.insert("id", "minecraft:allay".to_string());
        chunk.entities.push(ent);

        world.insert_chunk(DimensionId::Overworld, chunk);
        world.save_all().unwrap();
        drop(world);

        // Verify separate `entities/r.0.0.mca` file was created on disk
        assert!(dir.path().join("entities").join("r.0.0.mca").exists());

        // Reopen through FormatRegistry auto-detection with a fresh registry
        let reg2 = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let fmt2 = FormatRegistry::open_world(dir.path(), &reg2.read()).unwrap();
        let mut world2 = World::open(fmt2, reg2.clone(), 20).unwrap();
        assert_eq!(world2.min_y(), -64);
        assert_eq!(world2.max_y(), 320);

        let b_neg = world2.get_block(DimensionId::Overworld, IVec3::new(2, -64, 3));
        assert_eq!(reg2.read().get_state(b_neg).name, "minecraft:deepslate");

        let b_mid = world2.get_block(DimensionId::Overworld, IVec3::new(5, 64, 5));
        assert_eq!(
            reg2.read().get_state(b_mid),
            &BlockState::new("minecraft:grass_block").with_prop("snowy", "false")
        );

        let b_top = world2.get_block(DimensionId::Overworld, IVec3::new(8, 310, 8));
        assert_eq!(reg2.read().get_state(b_top).name, "minecraft:copper_block");

        let c_reloaded = world2
            .get_chunk(DimensionId::Overworld, ChunkPos::new(0, 0))
            .unwrap()
            .unwrap();
        assert_eq!(c_reloaded.entities.len(), 1);
        assert_eq!(
            c_reloaded.entities[0].get_string("id"),
            Some("minecraft:allay")
        );
        assert_eq!(c_reloaded.biomes.get(4, -60, 4), 174);
        assert_eq!(c_reloaded.biomes.get(4, 70, 4), 132);

        // 3. Verify Minecraft 26.x+ (`dimensions/minecraft/overworld/region`, `players/data/*.dat`,
        //    and `Data.spawn.pos` IntArray) directory & NBT layout round-trips cleanly.
        let dir26 = tempdir().unwrap();
        std::fs::create_dir_all(
            dir26
                .path()
                .join("dimensions")
                .join("minecraft")
                .join("overworld")
                .join("region"),
        )
        .unwrap();
        std::fs::create_dir_all(dir26.path().join("players").join("data")).unwrap();

        let mut spawn_comp = NbtCompound::new();
        spawn_comp.insert(
            "pos",
            mcedit_nbt::NbtTag::IntArray(vec![10047, 66, 3327]),
        );
        spawn_comp.insert("dimension", "minecraft:overworld".to_string());
        let mut ver26 = NbtCompound::new();
        ver26.insert("Id", 4903i32);
        ver26.insert("Name", "26.2".to_string());
        let mut data26 = NbtCompound::new();
        data26.insert("version", 19133i32);
        data26.insert("DataVersion", 4903i32);
        data26.insert("LevelName", "Canada Alpha Test".to_string());
        data26.insert("spawn", spawn_comp);
        data26.insert("Version", ver26);
        let mut root26 = NbtCompound::new();
        root26.insert("Data", data26);
        mcedit_nbt::write_nbt_file(
            dir26.path().join("level.dat"),
            "",
            &root26,
            Endianness::Big,
            Compression::Gzip,
        )
        .unwrap();

        let reg_c = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let fmt_c = FormatRegistry::open_world(dir26.path(), &reg_c.read()).unwrap();
        let mut world_c = World::open(fmt_c, reg_c.clone(), 10).unwrap();
        assert_eq!(world_c.metadata.level_name, "Canada Alpha Test");
        assert_eq!(world_c.metadata.spawn_pos, IVec3::new(10047, 66, 3327));

        let mut c26 = mcedit_core::Chunk::new(ChunkPos::new(627, 207), -4, 19);
        let pale_oak = reg_c
            .write()
            .intern(BlockState::new("minecraft:pale_oak_log"));
        c26.set_block(7, 66, 15, pale_oak);
        world_c.insert_chunk(DimensionId::Overworld, c26);
        world_c.save_all().unwrap();
        drop(world_c);

        assert!(
            dir26
                .path()
                .join("dimensions")
                .join("minecraft")
                .join("overworld")
                .join("region")
                .join("r.19.6.mca")
                .exists()
        );

        // Verify Minecraft 26.3+ (DataVersion = 5023) `Codec.either(String, Compound)` palette encoding & decoding:
        // - Property-less sections encode `block_states.palette` as `TAG_List<TAG_String>`
        // - Sections with properties encode `block_states.palette` as `TAG_List<TAG_Compound>`
        //   with `{ "": "<id>" }` for property-less entries and `{ "id": "<id>", "properties": { ... } }`
        //   for entries with properties.
        let dir263 = tempdir().unwrap();
        let mut level263_data = NbtCompound::new();
        level263_data.insert("LevelName", "MC26_3_Test");
        level263_data.insert("DataVersion", 5023i32);
        let mut ver263 = NbtCompound::new();
        ver263.insert("Id", 5023i32);
        ver263.insert("Name", "26.3");
        level263_data.insert("Version", ver263);
        let mut level263_root = NbtCompound::new();
        level263_root.insert("Data", level263_data);
        mcedit_nbt::write_nbt_file(
            dir263.path().join("level.dat"),
            "",
            &level263_root,
            Endianness::Big,
            Compression::Gzip,
        )
        .unwrap();
        std::fs::create_dir_all(
            dir263
                .path()
                .join("dimensions")
                .join("minecraft")
                .join("overworld")
                .join("region"),
        )
        .unwrap();

        let reg_263 = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let fmt_263 = FormatRegistry::open_world(dir263.path(), &reg_263.read()).unwrap();
        let mut world_263 = World::open(fmt_263, reg_263.clone(), 10).unwrap();

        let (stone_id, leaves_prop_id) = {
            let mut r = reg_263.write();
            let s = r.find_by_name("minecraft:stone").unwrap();
            let l = r.intern(BlockState::parse(
                "minecraft:acacia_leaves[distance=3,persistent=false]",
            ));
            (s, l)
        };

        let mut c263 = mcedit_core::Chunk::new(ChunkPos::new(-33, 33), -4, 19);
        // Section Y=0: only property-less blocks -> should encode as TAG_List<TAG_String>
        c263.set_block(2, 4, 2, stone_id);
        // Section Y=4: includes block with properties -> should encode as TAG_List<TAG_Compound> with {"": ...} and {"id": ..., "properties": ...}
        c263.set_block(3, 68, 3, stone_id);
        c263.set_block(4, 68, 4, leaves_prop_id);
        world_263.insert_chunk(DimensionId::Overworld, c263);
        world_263.save_all().unwrap();
        drop(world_263);

        // Inspect raw saved NBT in r.-2.1.mca to confirm 26.3 palette schema compliance
        let rpath_263 = dir263
            .path()
            .join("dimensions")
            .join("minecraft")
            .join("overworld")
            .join("region")
            .join("r.-2.1.mca");
        let rf_263 = RegionFile::open_or_create(&rpath_263).unwrap();
        let raw_263 = rf_263.read_chunk_nbt(31, 1).unwrap().unwrap();
        let (_, root_263) = mcedit_nbt::read_nbt(&raw_263, Endianness::Big).unwrap();
        let secs_263 = root_263.get_list("sections").unwrap();
        assert_eq!(secs_263.len(), 24);
        let mut checked_y0 = false;
        let mut checked_y4 = false;
        for item in secs_263.items() {
            let mcedit_nbt::NbtTag::Compound(sc) = item else {
                continue;
            };
            let sy = sc.get_byte("Y").unwrap();
            let pal = sc
                .get_compound("block_states")
                .unwrap()
                .get_list("palette")
                .unwrap();
            if sy == 0 {
                assert!(matches!(
                    pal.items().first(),
                    Some(mcedit_nbt::NbtTag::String(_))
                ));
                checked_y0 = true;
            } else if sy == 4 {
                assert!(matches!(
                    pal.items().first(),
                    Some(mcedit_nbt::NbtTag::Compound(_))
                ));
                let has_empty_key = pal.items().iter().any(|e| {
                    matches!(e, mcedit_nbt::NbtTag::Compound(c) if c.get_string("") == Some("minecraft:stone"))
                });
                let has_id_prop = pal.items().iter().any(|e| {
                    matches!(e, mcedit_nbt::NbtTag::Compound(c) if c.get_string("id") == Some("minecraft:acacia_leaves") && c.get_compound("properties").is_some())
                });
                assert!(has_empty_key && has_id_prop);
                checked_y4 = true;
            }
        }
        assert!(checked_y0 && checked_y4);

        // Reopen and verify both readonly and mutable chunk loading decode 26.3 sections accurately
        let fmt_263_re = FormatRegistry::open_world(dir263.path(), &reg_263.read()).unwrap();
        let ro_chunk = fmt_263_re
            .load_chunk_readonly(DimensionId::Overworld, ChunkPos::new(-33, 33), &reg_263.read())
            .unwrap()
            .unwrap();
        assert_eq!(ro_chunk.get_block(2, 4, 2), stone_id);
        let mut world_263_re = World::open(fmt_263_re, reg_263.clone(), 10).unwrap();
        assert_eq!(
            world_263_re.get_block(DimensionId::Overworld, IVec3::new(-33 * 16 + 2, 4, 33 * 16 + 2)),
            stone_id
        );
        assert_eq!(
            world_263_re.get_block(DimensionId::Overworld, IVec3::new(-33 * 16 + 4, 68, 33 * 16 + 4)),
            leaves_prop_id
        );
    }

    #[test]
    fn region_repair_and_undo_redo() {
        let dir = tempdir().unwrap();
        let rpath = dir.path().join("r.0.0.mca");
        let mut rf = RegionFile::open_or_create(&rpath).unwrap();
        let mut c = NbtCompound::new();
        c.insert("x", 42i32);
        let raw = mcedit_nbt::write_nbt("", &c, Endianness::Big, Compression::None).unwrap();
        rf.write_chunk_nbt(0, 0, &raw).unwrap();
        rf.write_chunk_nbt(1, 0, &raw).unwrap();
        drop(rf);

        // Simulate corrupt OOB offset on chunk (2, 0)
        {
            let mut f = OpenOptions::new().read(true).write(true).open(&rpath).unwrap();
            f.seek(SeekFrom::Start(8)).unwrap();
            f.write_all(&((999u32 << 8) | 1u32).to_be_bytes()).unwrap();
        }
        let report = RegionFile::repair(&rpath).unwrap();
        assert_eq!(report.files_repaired, 1);
        assert_eq!(report.chunks_recovered, 2);

        // Verify undo/redo & analysis
        let wdir = tempdir().unwrap();
        let reg = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let fmt = AnvilPre113Format::create_new(wdir.path(), "UndoTest", 1, 1, IVec3::ZERO, &reg.read()).unwrap();
        let mut world = World::open(Box::new(fmt), reg.clone(), 10).unwrap();
        {
            let mut r = reg.write();
            let c0 = create_flatland_chunk(ChunkPos::new(0, 0), 0, 15, &default_flatland_layers(), &mut r);
            world.insert_chunk(DimensionId::Overworld, c0);
        }
        world.save_all().unwrap();
        assert!(!world.has_unsaved_changes());

        let gold = reg.read().find_by_legacy(41, 0).unwrap();
        let b = BoundingBox::from_min_max(IVec3::new(0, 0, 0), IVec3::new(2, 2, 2));
        let mut tx = EditTransaction::new("Fill Gold", DimensionId::Overworld, Some(b));
        fill_blocks(&mut world, DimensionId::Overworld, b, gold, None, false, &mut tx);
        tx.commit(&mut world);

        assert_eq!(world.get_block(DimensionId::Overworld, IVec3::new(1, 1, 1)), gold);
        assert!(world.undo().is_some());
        let dirt = reg.read().find_by_legacy(3, 0).unwrap();
        assert_eq!(world.get_block(DimensionId::Overworld, IVec3::new(1, 1, 1)), dirt);
        assert!(world.redo().is_some());
        assert_eq!(world.get_block(DimensionId::Overworld, IVec3::new(1, 1, 1)), gold);

        // Test paste_volume + analyze_box
        let mut vol = SchematicVolume::new(UVec3::new(1, 1, 1));
        vol.set_block(0, 0, 0, gold);
        let mut tx2 = EditTransaction::new("Paste", DimensionId::Overworld, None);
        paste_volume(&mut world, DimensionId::Overworld, &vol, IVec3::new(5, 5, 5), &CopyOptions::default(), &mut tx2);
        tx2.commit(&mut world);

        let res = analyze_box(&mut world, DimensionId::Overworld, BoundingBox::from_min_max(IVec3::new(5, 5, 5), IVec3::new(6, 6, 6)));
        assert_eq!(res.total_blocks, 1);
        assert!(res.to_csv(&reg.read()).contains("Gold"));
    }
}
