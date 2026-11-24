pub mod block;
pub mod chunk;
pub mod compare;
pub mod format;
pub mod lighting;
pub mod math;
pub mod ops;
pub mod profiler;
pub mod rotation;
pub mod section;
pub mod undo;
pub mod world;

use std::sync::atomic::{AtomicBool, Ordering};

static HIDE_BARRIERS: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn set_hide_barriers(hide: bool) {
    HIDE_BARRIERS.store(hide, Ordering::Relaxed);
}

#[inline]
pub fn hide_barriers() -> bool {
    HIDE_BARRIERS.load(Ordering::Relaxed)
}

pub use block::{BlockMeta, BlockRegistry, BlockShape, BlockState, BlockStateId, TintKind};
pub use chunk::{BiomeStorage, Chunk};
pub use compare::{CompareConfig, CompareViewMode, DiffKind, classify_block_diff, is_effective_air};
pub use format::{
    FormatError, FormatResult, LodChunkSurface, LodChunkVoxels, RepairReport, SchematicFormat,
    WorldFormat, lod_non_opaque_priority, tinted_map_color,
};
pub use lighting::LightingEngine;
pub use math::{BlockPos, BoundingBox, ChunkPos, Face};
pub use ops::{
    AnalysisResult, CopyOptions, FlatlandLayer, analyze_box, copy_from_format_in_box,
    create_flatland_chunk, default_flatland_layers, delete_entities_in_box,
    delete_tile_ticks_in_box, extract_volume, fill_blocks, paste_volume,
};
pub use rotation::{TransformOp, transform_block_state, transform_volume};
pub use section::ChunkSection;
pub use undo::{EditTransaction, UndoEntry, UndoKind, UndoStack};
pub use world::{DimensionId, PlayerData, SchematicVolume, World, WorldMetadata};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_coord_chunk_division() {
        let p = BlockPos::new(-1, 64, -17);
        assert_eq!(p.chunk_pos(), ChunkPos::new(-1, -2));
        assert_eq!(p.local_in_section(), (15, 0, 15));

        let c = ChunkPos::new(-1, -33);
        assert_eq!(c.region_pos(), (-1, -2));
        assert_eq!(c.region_local(), (31, 31));
    }

    #[test]
    fn section_palette_and_arbitrary_y() {
        let reg = BlockRegistry::with_pre113_defaults();
        let stone = reg.find_by_legacy(1, 0).unwrap();
        let granite = reg.find_by_legacy(1, 1).unwrap();
        let mut chunk = Chunk::new(ChunkPos::new(0, 0), -4, 19);
        assert!(chunk.set_block(3, -64, 5, stone));
        assert!(chunk.set_block(4, 300, 6, granite));
        assert_eq!(chunk.get_block(3, -64, 5), stone);
        assert_eq!(chunk.get_block(4, 300, 6), granite);
        assert_eq!(chunk.sections.get(&-4).unwrap().non_air_count, 1);

        // Compact palette after removing granite
        assert!(chunk.set_block(4, 300, 6, BlockStateId::AIR));
        let sec = chunk.sections.get_mut(&18).unwrap();
        sec.compact_palette();
        assert_eq!(sec.palette, vec![BlockStateId::AIR]);
    }

    #[test]
    fn block_rotation_symmetry() {
        let stair = BlockState::new("minecraft:oak_stairs")
            .with_prop("facing", "north")
            .with_prop("half", "bottom");
        let mut s = stair.clone();
        for _ in 0..4 {
            s = transform_block_state(&s, TransformOp::RotateCW);
        }
        assert_eq!(s, stair);

        let flipped = transform_block_state(&stair, TransformOp::FlipY);
        assert_eq!(flipped.properties.get("half").unwrap(), "top");
        assert_eq!(
            transform_block_state(&flipped, TransformOp::FlipY),
            stair
        );

        let rail = BlockState::new("minecraft:rail").with_prop("shape", "north_east");
        let r_cw = transform_block_state(&rail, TransformOp::RotateCW);
        assert_eq!(r_cw.properties.get("shape").unwrap(), "south_east");
        let r_ccw = transform_block_state(&r_cw, TransformOp::RotateCCW);
        assert_eq!(r_ccw, rail);
    }

    #[test]
    fn registry_loads_all_pre113_blocks() {
        let reg = BlockRegistry::with_pre113_defaults();
        assert!(reg.len() > 500);
        let air = reg.find_by_legacy(0, 0).unwrap();
        assert_eq!(air, BlockStateId::AIR);
        let stone = reg.find_by_legacy(1, 0).unwrap();
        assert_eq!(reg.get_state(stone).name, "minecraft:stone");
        let concrete = reg.find_by_legacy(251, 14).unwrap();
        assert_eq!(reg.get_state(concrete).name, "minecraft:concrete");
    }
}
