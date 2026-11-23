use glam::{IVec3, Vec3};
use mcedit_core::{
    BlockRegistry, BlockState, BoundingBox, ChunkPos, CopyOptions, DimensionId, EditTransaction,
    Face, LightingEngine, TransformOp, World, WorldFormat, analyze_box, create_flatland_chunk,
    default_flatland_layers, extract_volume, fill_blocks, paste_volume, transform_volume,
};
use mcedit_formats::{AnvilPre113Format, FormatRegistry};
use mcedit_plugins::{
    BrushMaskOptions, ParamValue, PluginContext, PluginRegistry, ProgressHandle, default_param_map,
};
use mcedit_render::{ViewMode, raycast_world};
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

#[test]
fn end_to_end_world_editing_workflow() {
    let dir = tempdir().unwrap();
    let world_dir = dir.path().join("E2EWorld");
    let schem_path = dir.path().join("tower.schematic");

    let reg = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));

    // 1. Create a fresh pre-1.13 Anvil world and populate 2x2 flatland chunks
    let fmt: Box<dyn WorldFormat> = Box::new(
        AnvilPre113Format::create_new(
            &world_dir,
            "E2E World",
            987654321,
            1,
            IVec3::new(8, 4, 8),
            &reg.read(),
        )
        .unwrap(),
    );
    let mut world = World::open(fmt, reg.clone(), 20).unwrap();
    for cz in 0..2 {
        for cx in 0..2 {
            let cp = ChunkPos::new(cx, cz);
            let chunk = {
                let mut rw = reg.write();
                create_flatland_chunk(cp, 0, 15, &default_flatland_layers(), &mut rw)
            };
            world.insert_chunk(DimensionId::Overworld, chunk);
        }
    }

    // 2. Fill a stone platform
    let sel = BoundingBox::from_min_max(IVec3::new(2, 4, 2), IVec3::new(10, 8, 10));
    let stone = reg.read().find_by_legacy(1, 0).unwrap();
    let mut tx = EditTransaction::new("Fill Stone", DimensionId::Overworld, Some(sel));
    let filled = fill_blocks(
        &mut world,
        DimensionId::Overworld,
        sel,
        stone,
        None,
        false,
        &mut tx,
    );
    tx.commit(&mut world);
    assert_eq!(filled, 8 * 4 * 8);

    // 3. Apply Topsoil filter via PluginRegistry
    let plugins = PluginRegistry::with_stock_plugins();
    let topsoil_filter = plugins.filter_by_id("topsoil").unwrap();
    let mut fparams = default_param_map(&topsoil_filter.params());
    fparams.insert(
        "block".into(),
        ParamValue::Block {
            state: BlockState::new("minecraft:gold_block"),
            match_base_only: false,
        },
    );
    let mut tx_f = EditTransaction::new("Topsoil Gold", DimensionId::Overworld, Some(sel));
    let mut ctx = PluginContext {
        world: &mut world,
        dim: DimensionId::Overworld,
        tx: &mut tx_f,
        progress: ProgressHandle::new(Arc::new(AtomicBool::new(false))),
        active_clipboard: None,
        message: None,
    };
    topsoil_filter.execute(&mut ctx, sel, &fparams).unwrap();
    tx_f.commit(&mut world);

    let gold = reg.read().find_by_legacy(41, 0).unwrap();
    assert_eq!(
        world.get_block(DimensionId::Overworld, IVec3::new(5, 7, 5)),
        gold
    );

    // 4. Undo -> Redo
    assert!(world.undo().is_some());
    assert_eq!(
        world.get_block(DimensionId::Overworld, IVec3::new(5, 7, 5)),
        stone
    );
    assert!(world.redo().is_some());
    assert_eq!(
        world.get_block(DimensionId::Overworld, IVec3::new(5, 7, 5)),
        gold
    );

    // 5. Brush stroke (glowstone)
    let fill_brush = plugins.brush_by_id("fill").unwrap();
    let mut bparams = default_param_map(&fill_brush.params());
    bparams.insert(
        "block".into(),
        ParamValue::Block {
            state: BlockState::new("minecraft:glowstone"),
            match_base_only: false,
        },
    );
    let mut tx_b = EditTransaction::new("Brush Glow", DimensionId::Overworld, None);
    let mut bctx = PluginContext {
        world: &mut world,
        dim: DimensionId::Overworld,
        tx: &mut tx_b,
        progress: ProgressHandle::new(Arc::new(AtomicBool::new(false))),
        active_clipboard: None,
        message: None,
    };
    fill_brush
        .apply_point(
            &mut bctx,
            IVec3::new(6, 9, 6),
            &BrushMaskOptions::default(),
            &bparams,
            42,
        )
        .unwrap();
    tx_b.commit(&mut world);

    // 6. Relight chunks (glowstone emits light 15)
    LightingEngine::relight_chunks(
        &mut world,
        DimensionId::Overworld,
        &[ChunkPos::new(0, 0), ChunkPos::new(1, 0)],
    );
    let chunk0 = world
        .get_chunk(DimensionId::Overworld, ChunkPos::new(0, 0))
        .unwrap()
        .unwrap();
    assert_eq!(chunk0.get_block_light(6, 9, 6), 15);
    assert!(chunk0.get_block_light(8, 9, 6) >= 13);

    // 7. Extract schematic -> rotate -> save .schematic -> reload -> paste at (18, 4, 2)
    let vol = extract_volume(&mut world, DimensionId::Overworld, sel);
    let rotated = {
        let mut rw = reg.write();
        transform_volume(&vol, TransformOp::RotateCW, &mut rw, false)
    };
    FormatRegistry::save_schematic(&schem_path, &rotated, &reg.read()).unwrap();
    let loaded_vol = {
        let mut rw = reg.write();
        FormatRegistry::open_schematic(&schem_path, &mut rw).unwrap()
    };
    let mut tx_p = EditTransaction::new("Paste Schematic", DimensionId::Overworld, None);
    paste_volume(
        &mut world,
        DimensionId::Overworld,
        &loaded_vol,
        IVec3::new(18, 4, 2),
        &CopyOptions::default(),
        &mut tx_p,
    );
    tx_p.commit(&mut world);

    // Analyze pasted box
    let pasted_box = BoundingBox::from_min_max(IVec3::new(18, 4, 2), IVec3::new(26, 8, 10));
    let analysis = analyze_box(&mut world, DimensionId::Overworld, pasted_box);
    assert_eq!(analysis.total_blocks, 8 * 4 * 8);

    // 8. Save world -> close -> reopen from disk and verify persistence
    world.save_all().unwrap();
    drop(world);

    let reg2 = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
    let fmt2 = FormatRegistry::open_world(&world_dir, &reg2.read()).unwrap();
    let mut world2 = World::open(fmt2, reg2.clone(), 20).unwrap();
    assert_eq!(world2.metadata.level_name, "E2E World");
    let got_gold = world2.get_block(DimensionId::Overworld, IVec3::new(20, 7, 5));
    assert_eq!(reg2.read().get_state(got_gold).name, "minecraft:gold_block");

    // 9. Raycast 3D DDA across chunk boundary (0,0) -> (1,0) and Chunk2D hit
    let hit3d = raycast_world(
        &world2,
        DimensionId::Overworld,
        Vec3::new(14.5, 6.5, 5.5),
        Vec3::X,
        32.0,
        ViewMode::Perspective3D,
    )
    .expect("3D DDA should hit pasted stone/gold wall in chunk (1,0)");
    assert_eq!(hit3d.block_pos, IVec3::new(18, 6, 5));
    assert_eq!(hit3d.face, Face::West);

    let hit2d = raycast_world(
        &world2,
        DimensionId::Overworld,
        Vec3::new(20.5, 200.0, 5.5),
        Vec3::NEG_Y,
        300.0,
        ViewMode::Chunk2D,
    )
    .expect("Chunk2D raycast should hit top block");
    assert_eq!(hit2d.block_pos, IVec3::new(20, 7, 5));

    // 10. Verify distant LOD 3D raycast (75 chunks = 1200 blocks away) & midair fallback
    let mut far_chunk = mcedit_core::Chunk::new(ChunkPos::new(75, 0), 0, 15);
    let stone_id = reg2.read().find_by_legacy(1, 0).unwrap();
    for y in 0..80 {
        for z in 0..16 {
            for x in 0..16 {
                far_chunk.set_block(x, y, z, stone_id);
            }
        }
    }
    let far_vox = mcedit_core::LodChunkVoxels::from_chunk(&far_chunk, 4, &reg2.read());
    let atlas = mcedit_render::TextureAtlas::default_atlas();
    let palette = atlas.build_lod_colors(&reg2.read());
    let (_, occ) = mcedit_render::mesh_lod_voxel_tile(9, 0, &[far_vox], 4, &palette, 0);
    let mut lod_idx = mcedit_render::LodRaycastIndex::new();
    lod_idx.insert((9, 0), Arc::new(occ));

    let far_hit = mcedit_render::raycast_world_with_lod(
        &world2,
        DimensionId::Overworld,
        Some(&lod_idx),
        Vec3::new(100.0, 60.5, 8.5),
        Vec3::X,
        2048.0,
        ViewMode::Perspective3D,
    )
    .expect("Should hit distant LOD terrain at x=1200");
    assert!((1198..=1204).contains(&far_hit.block_pos.x), "far_hit={far_hit:?}");

    let sky_hit = mcedit_render::raycast_or_air(
        &world2,
        DimensionId::Overworld,
        Some(&lod_idx),
        Vec3::new(0.0, 100.0, 0.0),
        Vec3::Y,
        2048.0,
        32.0,
        ViewMode::Perspective3D,
    );
    assert_eq!(sky_hit.block_pos, IVec3::new(0, 132, 0));
}
