pub mod brushes;
pub mod filters;
pub mod schema;
pub mod traits;

pub use brushes::bresenham_3d;
pub use schema::{ParamDef, ParamKind, ParamMap, ParamValue, default_param_map};
pub use traits::{
    BrushMaskOptions, BrushPlugin, BrushShape, FilterPlugin, PluginContext, PluginRegistry,
    ProgressHandle,
};

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{IVec3, UVec3};
    use mcedit_core::{
        BlockRegistry, BlockState, BlockStateId, BoundingBox, ChunkPos, DimensionId,
        EditTransaction, SchematicVolume, World, WorldFormat, create_flatland_chunk,
        default_flatland_layers,
    };
    use mcedit_formats::AnvilPre113Format;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn temp_world() -> (tempfile::TempDir, World) {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let fmt: Box<dyn WorldFormat> = Box::new(
            AnvilPre113Format::create_new(
                dir.path(),
                "PluginTest",
                42,
                1,
                IVec3::ZERO,
                &reg.read(),
            )
            .unwrap(),
        );
        let mut world = World::open(fmt, reg.clone(), 20).unwrap();
        {
            let mut r = reg.write();
            let c = create_flatland_chunk(
                ChunkPos::new(0, 0),
                0,
                15,
                &default_flatland_layers(),
                &mut r,
            );
            world.insert_chunk(DimensionId::Overworld, c);
        }
        (dir, world)
    }

    #[test]
    fn all_stock_brushes_run_cleanly() {
        let (_dir, mut world) = temp_world();
        let plugins = PluginRegistry::with_stock_plugins();
        assert_eq!(plugins.brushes.len(), 8);

        let mut vol = SchematicVolume::new(UVec3::new(2, 2, 2));
        let gold = world.registry.read().find_by_legacy(41, 0).unwrap();
        vol.set_block(0, 0, 0, gold);

        let mask = BrushMaskOptions::default();
        for b in &plugins.brushes {
            let mut params = default_param_map(&b.params());
            if b.id() == "flood_fill" {
                params.insert("max_blocks".into(), ParamValue::Int(16));
            }
            let mut tx = EditTransaction::new(b.display_name(), DimensionId::Overworld, None);
            let mut ctx = PluginContext {
                world: &mut world,
                dim: DimensionId::Overworld,
                tx: &mut tx,
                progress: ProgressHandle::new(Arc::new(AtomicBool::new(false))),
                active_clipboard: Some(&vol),
                message: None,
            };
            b.apply_point(&mut ctx, IVec3::new(8, 3, 8), &mask, &params, 1234)
                .unwrap();
            tx.commit(&mut world);
        }

        // Verify Erode preserved stained clay variant when surrounded by stained clay
        let red_clay = world.registry.read().find_by_legacy(159, 14).unwrap();
        let p = IVec3::new(4, 10, 4);
        for n in [IVec3::X, IVec3::NEG_X, IVec3::Y, IVec3::NEG_Y, IVec3::Z] {
            world.set_block(DimensionId::Overworld, p + n, red_clay);
        }
        world.set_block(DimensionId::Overworld, p, BlockStateId::AIR);
        let erode = plugins.brush_by_id("erode").unwrap();
        let mut tx = EditTransaction::new("Erode", DimensionId::Overworld, None);
        let mut ctx = PluginContext {
            world: &mut world,
            dim: DimensionId::Overworld,
            tx: &mut tx,
            progress: ProgressHandle::new(Arc::new(AtomicBool::new(false))),
            active_clipboard: None,
            message: None,
        };
        let params = default_param_map(&erode.params());
        erode
            .apply_point(
                &mut ctx,
                p,
                &BrushMaskOptions {
                    width: 1,
                    height: 1,
                    length: 1,
                    ..Default::default()
                },
                &params,
                1,
            )
            .unwrap();
        tx.commit(&mut world);
        assert_eq!(world.get_block(DimensionId::Overworld, p), red_clay);
    }

    #[test]
    fn all_stock_filters_run_cleanly() {
        let (_dir, mut world) = temp_world();
        let plugins = PluginRegistry::with_stock_plugins();
        assert_eq!(plugins.filters.len(), 6);
        let sel = BoundingBox::from_min_max(IVec3::new(0, 0, 0), IVec3::new(8, 10, 8));

        for f in &plugins.filters {
            let mut params = default_param_map(&f.params());
            if f.id() == "find_blocks" {
                params.insert(
                    "target".into(),
                    ParamValue::Block {
                        state: BlockState::new("minecraft:dirt"),
                        match_base_only: false,
                    },
                );
            }
            let mut tx = EditTransaction::new(f.display_name(), DimensionId::Overworld, Some(sel));
            let mut ctx = PluginContext {
                world: &mut world,
                dim: DimensionId::Overworld,
                tx: &mut tx,
                progress: ProgressHandle::new(Arc::new(AtomicBool::new(false))),
                active_clipboard: None,
                message: None,
            };
            f.execute(&mut ctx, sel, &params).unwrap();
            tx.commit(&mut world);
        }
    }
}
