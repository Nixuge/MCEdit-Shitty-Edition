pub mod atlas;
pub mod camera;
pub mod compare_mesher;
pub mod frustum;
pub mod mesher;
pub mod overlay;
pub mod pipeline;
pub mod raycaster;

pub use atlas::{
    ATLAS_SIZE, DEFAULT_TERRAIN_PNG, GRID_DIM, LodBlockVisual, LodColorTable, TILE_SIZE,
    TextureAtlas, should_tint_face,
};
pub use camera::{CameraController, CameraInputs, ViewMode};
pub use compare_mesher::{
    mesh_chunk_compare, mesh_lod_overhead_tile_compare, mesh_lod_voxel_tile_compare,
};
pub use frustum::Frustum;
pub use mesher::{
    ChunkNeighborSnapshot, LodTileOccupancy, MeshBucket, MeshOptions, PackedCubeQuad,
    SectionMeshData, TerrainVertex, build_chunk_snapshots, mesh_chunk_full, mesh_chunk_overhead,
    mesh_lod_overhead_tile, mesh_lod_voxel_tile, mesh_section, mesh_volume_preview,
};
pub use overlay::{OverlayMeshBuilder, OverlayVertex};
pub use pipeline::{LodRegionKey, SectionMeshKey, WorldRenderer};
pub use raycaster::{
    LodRaycastIndex, RaycastHit, raycast_or_air, raycast_world, raycast_world_with_lod,
};

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use mcedit_core::{BlockRegistry, Chunk, ChunkPos, Face, LodChunkVoxels};

    #[test]
    fn frustum_culls_behind_camera() {
        let cam = CameraController::default();
        let vp = cam.view_proj(1280.0, 720.0);
        let f = Frustum::from_view_proj(vp);

        let in_front_min = Vec3::new(0.0, 50.0, -32.0);
        let in_front_max = Vec3::new(16.0, 66.0, -16.0);
        assert!(f.intersects_aabb(in_front_min, in_front_max));

        let behind_min = Vec3::new(0.0, 50.0, 100.0);
        let behind_max = Vec3::new(16.0, 66.0, 116.0);
        assert!(!f.intersects_aabb(behind_min, behind_max));
    }

    #[test]
    fn mesher_culls_adjacent_opaque_faces() {
        let reg = BlockRegistry::with_pre113_defaults();
        let stone = reg.find_by_legacy(1, 0).unwrap();

        let mut chunk = Chunk::new(ChunkPos::new(0, 0), 0, 15);
        chunk.set_block(4, 10, 4, stone);
        chunk.set_block(5, 10, 4, stone);

        let snap = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk),
            east: None,
            west: None,
            south: None,
            north: None,
        };
        let mesh = mesh_section(&snap, 0, &reg, MeshOptions::default());
        // 2 adjacent stone cubes: internal East/West faces are culled, and the 4 coplanar
        // side pairs (Up, Down, South, North) are greedy-merged into 2x1 quads -> 6 packed quads!
        assert_eq!(mesh.packed_opaque.len(), 6);
    }

    #[test]
    fn mesher_emits_outward_ccw_winding_for_all_6_faces() {
        let reg = BlockRegistry::with_pre113_defaults();
        let stone = reg.find_by_legacy(1, 0).unwrap();

        let mut chunk = Chunk::new(ChunkPos::new(0, 0), 0, 15);
        chunk.set_block(4, 10, 4, stone);
        let snap = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk),
            east: None,
            west: None,
            south: None,
            north: None,
        };
        let mesh = mesh_chunk_full(&snap, &reg, MeshOptions::default());
        assert_eq!(mesh.packed_opaque.len(), 6);

        let center = Vec3::new(4.5, 10.5, 4.5);
        let tri_idx = [[0usize, 2, 1], [0, 3, 2]];
        for (face_i, face) in Face::ALL.iter().enumerate() {
            let expected_n = face.normal().as_vec3();
            let corners = mesh.packed_opaque[face_i].unpack_corners(0.0, 0.0);
            for (tri, idx) in tri_idx.iter().enumerate() {
                let v0 = Vec3::from_array(corners[idx[0]]);
                let v1 = Vec3::from_array(corners[idx[1]]);
                let v2 = Vec3::from_array(corners[idx[2]]);
                let tri_n = (v1 - v0).cross(v2 - v0);
                assert!(
                    tri_n.dot(expected_n) > 0.0,
                    "Face {:?} tri {} had inverted winding! tri_n={:?} expected={:?}",
                    face,
                    tri,
                    tri_n,
                    expected_n
                );
                let tri_center = (v0 + v1 + v2) / 3.0;
                assert!((tri_center - center).dot(expected_n) > 0.0);
            }
        }
    }

    #[test]
    fn lod_color_table_matches_atlas_grass_and_leaves() {
        let reg = BlockRegistry::with_pre113_defaults();
        let atlas = TextureAtlas::default_atlas();
        let palette = atlas.build_lod_colors(&reg);

        let grass = reg.find_by_legacy(2, 0).unwrap();
        let g_top = palette.get(grass).face_colors[Face::Up.index()];
        // Must match terrain.png grass_top (~147) * Grass tint (0.47, 0.75, 0.35) ~= (69, 110, 51)
        assert!(g_top[0] >= 55 && g_top[0] <= 85, "g_top={g_top:?}");
        assert!(g_top[1] >= 95 && g_top[1] <= 125, "g_top={g_top:?}");
        assert!(g_top[2] >= 40 && g_top[2] <= 65, "g_top={g_top:?}");

        // Grass side must NOT be green-tinted on top of its brown dirt texture
        let g_side = palette.get(grass).face_colors[Face::North.index()];
        assert!(g_side[0] > g_side[1], "grass side should keep earthy brown/dirt tone: {g_side:?}");
    }

    #[test]
    fn lod_3d_voxel_preserves_floating_glass_and_bottom_stone() {
        let reg = BlockRegistry::with_pre113_defaults();
        let atlas = TextureAtlas::default_atlas();
        let palette = atlas.build_lod_colors(&reg);

        let stone = reg.find_by_legacy(1, 0).unwrap();
        let grass = reg.find_by_legacy(2, 0).unwrap();
        let red_glass = reg.find_by_legacy(95, 14).unwrap(); // Red Stained Glass

        let mut chunk = Chunk::new(ChunkPos::new(0, 0), 0, 15);
        for z in 0..16 {
            for x in 0..16 {
                for y in 0..64 {
                    chunk.set_block(x, y, z, stone);
                }
                chunk.set_block(x, 64, z, grass);
            }
        }
        // Single floating red glass block high in the sky at Y=120
        chunk.set_block(8, 120, 8, red_glass);

        let vox = LodChunkVoxels::from_chunk(&chunk, 2, &reg);
        let (mesh, occ) = mesh_lod_voxel_tile(0, 0, &[vox], 2, &palette, 0);

        // 1. Bottom stone at Y=0 must have exposed Face::Down vertices at y == 0.0
        assert!(
            mesh.opaque.vertices.iter().any(|v| v.pos[1] == 0.0),
            "Bottom stone/bedrock at Y=0 must not disappear in LOD!"
        );

        // 2. Floating red glass must emit into translucent bucket at Y=120..122, NOT a pillar from Y=66..118!
        assert!(!mesh.translucent.is_empty());
        for y in (66..118).step_by(2) {
            assert!(
                !occ.is_world_occupied(8, y, 8),
                "Air gap below floating glass at y={y} must stay empty!"
            );
        }
        assert!(occ.is_world_occupied(8, 120, 8));
        assert!(occ.is_world_occupied(8, 64, 8));
        assert!(occ.is_world_occupied(8, 0, 8));

        // 3. No opaque vertex should stretch up to Y=120 above the terrain
        let max_opaque_y = mesh
            .opaque
            .vertices
            .iter()
            .map(|v| v.pos[1])
            .fold(0.0f32, f32::max);
        assert!(max_opaque_y <= 66.0, "max_opaque_y={max_opaque_y}");
    }

    #[test]
    fn lod2_preserves_shallow_water_over_seabed() {
        let reg = BlockRegistry::with_pre113_defaults();
        let atlas = TextureAtlas::default_atlas();
        let palette = atlas.build_lod_colors(&reg);

        let sand = reg.find_by_legacy(12, 0).unwrap();
        let water = reg.find_by_legacy(9, 0).unwrap();

        let mut chunk = Chunk::new(ChunkPos::new(0, 0), 0, 15);
        for z in 0..16 {
            for x in 0..16 {
                // Seabed up to Y=61, 1-block-deep water at Y=62 (inside the Y=60..63 LOD 2 cell!)
                for y in 0..=61 {
                    chunk.set_block(x, y, z, sand);
                }
                chunk.set_block(x, 62, z, water);
            }
        }

        let vox = LodChunkVoxels::from_chunk(&chunk, 4, &reg);
        let (mesh, _) = mesh_lod_voxel_tile(0, 0, &[vox], 4, &palette, 0);

        assert!(
            !mesh.translucent.is_empty(),
            "LOD 2 (step=4) must emit translucent water quads when water sits above seabed in the same Y=60..63 cell"
        );
        assert!(
            mesh.translucent.vertices.iter().any(|v| v.pos[1] == 64.0),
            "LOD 2 water top face should sit at y=64.0"
        );
    }

    #[test]
    fn lod0_ocean_water_greedy_merges_into_single_packed_quad() {
        let reg = BlockRegistry::with_pre113_defaults();
        let sand = reg.find_by_legacy(12, 0).unwrap();
        let water = reg.find_by_legacy(9, 0).unwrap();

        let mut chunk = Chunk::new(ChunkPos::new(0, 0), 0, 15);
        for z in 0..16 {
            for x in 0..16 {
                chunk.set_block(x, 61, z, sand);
                chunk.set_block(x, 62, z, water);
            }
        }
        let snap = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk),
            east: None,
            west: None,
            south: None,
            north: None,
        };
        let mesh = mesh_section(&snap, 3, &reg, MeshOptions::default());
        // Entire 16x16 water top surface must greedy-merge into 1 single 8-byte PackedCubeQuad!
        let up_water_quads = mesh
            .packed_translucent
            .iter()
            .filter(|q| q.face() == Face::Up.index() as u32 && (q.w1 & 0x8000_0000) != 0)
            .count();
        assert_eq!(
            up_water_quads, 1,
            "16x16 water surface should merge into 1 packed quad, got {up_water_quads}"
        );
    }

    #[test]
    fn static_chunk_grid_four_states() {
        // 4 chunks in a row:
        // (0, 0): no region, no chunk     -> Empty square (no tri fill, no cross)
        // (1, 0): region exists, no chunk -> X cross (4 line verts, no tri fill)
        // (2, 0): chunk exists & loaded   -> Gray fill (6 tri verts, no red border)
        // (3, 0): chunk exists unloaded   -> Red fill (6 tri verts) + red border (8 line verts)
        let grid = OverlayMeshBuilder::build_static_chunk_grid(
            0,
            3,
            0,
            0,
            -0.05,
            |cp| cp.cx() >= 1,
            |cp| cp.cx() >= 2,
            |cp| cp.cx() == 2,
        );
        // Gray quad (6 verts) + Red quad (6 verts) = 12 triangle verts
        assert_eq!(grid.tri_verts.len(), 12);
        // Shared grid lines: 5 vertical + 2 horizontal = 7 lines = 14 verts
        // + 1 cross (4 verts) + 1 red border (8 verts) = 26 line verts
        assert_eq!(grid.line_verts.len(), 26);

        // Universal grid spanning -2048..=2048 chunks with 1 region (0, 0)
        let univ = OverlayMeshBuilder::build_universal_chunk_grid(
            -2048,
            2048,
            -2048,
            2048,
            -4.0,
            &[(0, 0)],
            |cp| cp.cx() == 0 && cp.cz() == 0,
            |_| true,
        );
        // 1 loaded chunk -> 1 gray quad (6 tri verts)
        assert_eq!(univ.tri_verts.len(), 6);
        assert!((univ.tri_verts[0].pos[1] - (-4.0)).abs() < 1e-5);
        // Universal lines cover (-2048..=2049) * 2 axes = 4098 * 2 * 2 = 16392 verts
        // + 1023 missing chunk crosses in region (0,0) * 4 verts = 4092 verts
        assert_eq!(univ.line_verts.len(), 16392 + 4092);
    }

    #[test]
    fn atlas_loads_embedded_terrain() {
        let atlas = TextureAtlas::default_atlas();
        assert_eq!(atlas.rgba.width(), ATLAS_SIZE);
        assert_eq!(atlas.rgba.height(), ATLAS_SIZE);
        let uv = TextureAtlas::tile_uv([0, 0]);
        assert!(uv[0] < uv[2] && uv[1] < uv[3]);
    }

    #[test]
    fn compare_mesher_classifies_match_mismatch_and_one_sided() {
        let reg = BlockRegistry::with_pre113_defaults();
        let stone = reg.find_by_legacy(1, 0).unwrap();
        let dirt = reg.find_by_legacy(3, 0).unwrap();
        let cobble = reg.find_by_legacy(4, 0).unwrap();

        let cp = ChunkPos::new(0, 0);
        let mut chunk_a = Chunk::new(cp, 0, 15);
        let mut chunk_b = Chunk::new(cp, 0, 15);

        // (2, 10, 2): Match (stone == stone)
        chunk_a.set_block(2, 10, 2, stone);
        chunk_b.set_block(2, 10, 2, stone);
        // (5, 10, 2): Mismatch (stone != dirt) -> Red
        chunk_a.set_block(5, 10, 2, stone);
        chunk_b.set_block(5, 10, 2, dirt);
        // (8, 10, 2): Only in A (cobble vs air) -> Light Gray A
        chunk_a.set_block(8, 10, 2, cobble);
        // (11, 10, 2): Only in B (air vs dirt) -> Slate Gray B
        chunk_b.set_block(11, 10, 2, dirt);

        let snap_a = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk_a),
            east: None,
            west: None,
            south: None,
            north: None,
        };
        let snap_b = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk_b),
            east: None,
            west: None,
            south: None,
            north: None,
        };

        let mesh = mesh_chunk_compare(
            cp,
            Some(&snap_a),
            Some(&snap_b),
            &reg,
            MeshOptions::default(),
            mcedit_core::CompareConfig::default(),
        );
        assert_eq!(mesh.packed_opaque.len(), 6, "matching cube -> 6 packed_opaque faces");
        assert_eq!(mesh.packed_diff_red.len(), 6, "mismatched cube -> 6 packed_diff_red faces");
        assert_eq!(mesh.packed_diff_gray_a.len(), 6, "only-in-A cube -> 6 packed_diff_gray_a faces");
        assert_eq!(mesh.packed_diff_gray_b.len(), 6, "only-in-B cube -> 6 packed_diff_gray_b faces");
    }

    #[test]
    fn compare_mesher_ignore_properties_treats_rotated_stairs_as_match() {
        let mut reg = BlockRegistry::with_pre113_defaults();
        let stair_n = reg.intern(
            mcedit_core::BlockState::new("minecraft:oak_stairs")
                .with_prop("facing", "north")
                .with_prop("half", "bottom"),
        );
        let stair_s = reg.intern(
            mcedit_core::BlockState::new("minecraft:oak_stairs")
                .with_prop("facing", "south")
                .with_prop("half", "bottom"),
        );

        let cp = ChunkPos::new(0, 0);
        let mut chunk_a = Chunk::new(cp, 0, 15);
        let mut chunk_b = Chunk::new(cp, 0, 15);
        chunk_a.set_block(4, 10, 4, stair_n);
        chunk_b.set_block(4, 10, 4, stair_s);

        let snap_a = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk_a),
            east: None,
            west: None,
            south: None,
            north: None,
        };
        let snap_b = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk_b),
            east: None,
            west: None,
            south: None,
            north: None,
        };

        // Strict mode: rotated stairs are flagged as Mismatch (diff_red)
        let strict_cfg = mcedit_core::CompareConfig {
            ignore_properties: false,
            ..Default::default()
        };
        let strict_mesh = mesh_chunk_compare(
            cp,
            Some(&snap_a),
            Some(&snap_b),
            &reg,
            MeshOptions::default(),
            strict_cfg,
        );
        assert!(!strict_mesh.diff_red.is_empty(), "strict mode should flag rotated stairs red");
        assert!(strict_mesh.opaque.is_empty() && strict_mesh.alpha_cutout.is_empty());

        // Type-only mode: same base name -> Match (normal opaque/cutout)
        let type_cfg = mcedit_core::CompareConfig {
            ignore_properties: true,
            ..Default::default()
        };
        let type_mesh = mesh_chunk_compare(
            cp,
            Some(&snap_a),
            Some(&snap_b),
            &reg,
            MeshOptions::default(),
            type_cfg,
        );
        assert!(type_mesh.diff_red.is_empty(), "ignore_properties should not flag rotated stairs");
        assert!(
            !type_mesh.opaque.is_empty() || !type_mesh.alpha_cutout.is_empty(),
            "matching stairs must emit into normal geometry bucket"
        );
    }

    #[test]
    fn compare_mesher_hide_matching_exposes_buried_diff_faces() {
        let reg = BlockRegistry::with_pre113_defaults();
        let stone = reg.find_by_legacy(1, 0).unwrap();
        let diamond_ore = reg.find_by_legacy(56, 0).unwrap();

        let cp = ChunkPos::new(0, 0);
        let mut chunk_a = Chunk::new(cp, 0, 15);
        let mut chunk_b = Chunk::new(cp, 0, 15);
        // 3x3x3 solid stone cube in both, with only the center buried block differing
        for y in 9..=11 {
            for z in 3..=5 {
                for x in 3..=5 {
                    chunk_a.set_block(x, y, z, stone);
                    chunk_b.set_block(x, y, z, stone);
                }
            }
        }
        chunk_a.set_block(4, 10, 4, diamond_ore);

        let snap_a = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk_a),
            east: None,
            west: None,
            south: None,
            north: None,
        };
        let snap_b = ChunkNeighborSnapshot {
            center: std::sync::Arc::new(chunk_b),
            east: None,
            west: None,
            south: None,
            north: None,
        };

        let cmp = mcedit_core::CompareConfig {
            hide_matching: true,
            ..Default::default()
        };
        let mesh = mesh_chunk_compare(
            cp,
            Some(&snap_a),
            Some(&snap_b),
            &reg,
            MeshOptions::default(),
            cmp,
        );
        assert!(mesh.packed_opaque.is_empty(), "matching blocks must be hidden");
        assert_eq!(
            mesh.packed_diff_red.len(),
            6,
            "buried mismatched block must expose all 6 faces when hide_matching is enabled"
        );
    }

    #[test]
    fn compare_lod_voxel_and_overhead_highlight_differences() {
        let reg = BlockRegistry::with_pre113_defaults();
        let atlas = TextureAtlas::default_atlas();
        let palette = atlas.build_lod_colors(&reg);
        let stone = reg.find_by_legacy(1, 0).unwrap();
        let dirt = reg.find_by_legacy(3, 0).unwrap();

        let cp = ChunkPos::new(0, 0);
        let mut chunk_a = Chunk::new(cp, 0, 15);
        let mut chunk_b = Chunk::new(cp, 0, 15);
        // Mismatch column at (0..2, 60..62, 0..2), Only-A at (4..6), Only-B at (8..10)
        for y in 60..62 {
            for z in 0..2 {
                for x in 0..2 {
                    chunk_a.set_block(x, y, z, stone);
                    chunk_b.set_block(x, y, z, dirt);
                    chunk_a.set_block(x + 4, y, z, stone);
                    chunk_b.set_block(x + 8, y, z, stone);
                }
            }
        }

        let vox_a = LodChunkVoxels::from_chunk_exact(&chunk_a, 2, &reg);
        let vox_b = LodChunkVoxels::from_chunk_exact(&chunk_b, 2, &reg);
        let cmp = mcedit_core::CompareConfig::default();

        let (lod_mesh, occ) =
            mesh_lod_voxel_tile_compare(0, 0, &[vox_a], &[vox_b], 2, &palette, 0, &reg, cmp);
        // Mismatch goes to opaque with strong red tint (R > G + 40)
        assert!(
            lod_mesh
                .opaque
                .vertices
                .iter()
                .any(|v| v.color[0] > v.color[1].saturating_add(40)),
            "LOD mismatch must emit red-tinted vertices"
        );
        // Both Only-A and Only-B (semi-transparent by default) go to translucent with distinct grays
        assert!(
            !lod_mesh.translucent.is_empty(),
            "LOD one-sided blocks should emit translucent gray vertices"
        );
        assert!(occ.is_world_occupied(8, 60, 0), "LOD occupancy must include B-only voxels");

        // 2D overhead diff map
        let surf_a = mcedit_core::LodChunkSurface::from_chunk(&chunk_a, &reg);
        let surf_b = mcedit_core::LodChunkSurface::from_chunk(&chunk_b, &reg);
        let ov = mesh_lod_overhead_tile_compare(
            0,
            0,
            &[surf_a],
            &[surf_b],
            1,
            &palette,
            0,
            256,
            &reg,
            cmp,
        );
        assert!(
            ov.opaque
                .vertices
                .iter()
                .any(|v| v.color[0] > v.color[1].saturating_add(40)),
            "2D overhead diff must highlight mismatched columns in red"
        );
    }

    #[test]
    fn wgsl_shaders_validate_with_wgpu_naga() {
        for (name, src) in [
            ("terrain.wgsl", include_str!("shaders/terrain.wgsl")),
            ("overlay.wgsl", include_str!("shaders/overlay.wgsl")),
            ("blur.wgsl", include_str!("shaders/blur.wgsl")),
        ] {
            let module = wgpu::naga::front::wgsl::parse_str(src)
                .unwrap_or_else(|e| panic!("WGSL parse failed for {name}:\n{}", e.emit_to_string(src)));
            let mut validator = wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::all(),
            );
            validator
                .validate(&module)
                .unwrap_or_else(|e| panic!("WGSL validation failed for {name}:\n{}", e.emit_to_string(src)));
        }
    }
}
