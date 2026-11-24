use crate::atlas::{LodColorTable, TextureAtlas};
use crate::mesher::{
    ChunkNeighborSnapshot, LodTileOccupancy, MeshBucket, MeshOptions, PackedCubeQuad,
    SectionMeshData, emit_box_all_faces, greedy_merge_2d, is_ore, mesh_chunk_full,
    mesh_lod_overhead_tile, mesh_lod_voxel_tile, mesh_section_into, scale_rgb, shaded_color,
    unit_face_corners,
};
use mcedit_core::{
    BlockRegistry, BlockShape, BlockStateId, ChunkPos, CompareConfig, CompareViewMode, DiffKind,
    Face, LodChunkSurface, LodChunkVoxels, classify_block_diff,
};
use std::collections::BTreeSet;

/// Slot codes used during LOD 0 compare meshing.
const SLOT_NONE: u8 = 0;
const SLOT_MATCH: u8 = 1;
const SLOT_MISMATCH_RED: u8 = 2;
const SLOT_ONLY_A_GRAY: u8 = 3;
const SLOT_ONLY_B_GRAY: u8 = 4;

#[derive(Copy, Clone, PartialEq, Eq, Default)]
struct CompareGreedyFace {
    tile: [u8; 2],
    color: [u8; 4],
    bucket_kind: u8, // 0=empty, 1=opaque, 2=cutout, 3=translucent, 4=packed liquid, 5=surface-side liquid
    diff_slot: u8,   // SLOT_MATCH / SLOT_MISMATCH_RED / SLOT_ONLY_A_GRAY / SLOT_ONLY_B_GRAY
}

#[inline]
fn resolve_slot_and_block(
    a_id: BlockStateId,
    b_id: BlockStateId,
    reg: &BlockRegistry,
    hide_barriers: bool,
    cmp: CompareConfig,
) -> (BlockStateId, u8) {
    match cmp.view_mode {
        CompareViewMode::OnlyWorldA => {
            if a_id == BlockStateId::AIR || mcedit_core::is_effective_air(a_id, reg, hide_barriers) {
                (BlockStateId::AIR, SLOT_NONE)
            } else {
                (a_id, SLOT_MATCH)
            }
        }
        CompareViewMode::OnlyWorldB => {
            if b_id == BlockStateId::AIR || mcedit_core::is_effective_air(b_id, reg, hide_barriers) {
                (BlockStateId::AIR, SLOT_NONE)
            } else {
                (b_id, SLOT_MATCH)
            }
        }
        CompareViewMode::Diff => {
            let kind = classify_block_diff(a_id, b_id, reg, cmp.ignore_properties, hide_barriers);
            match kind {
                DiffKind::Match => {
                    if cmp.hide_matching
                        || a_id == BlockStateId::AIR
                        || mcedit_core::is_effective_air(a_id, reg, hide_barriers)
                    {
                        (BlockStateId::AIR, SLOT_NONE)
                    } else {
                        (a_id, SLOT_MATCH)
                    }
                }
                DiffKind::Mismatch => {
                    if cmp.show_mismatch {
                        (a_id, SLOT_MISMATCH_RED)
                    } else if cmp.hide_matching {
                        (BlockStateId::AIR, SLOT_NONE)
                    } else {
                        (a_id, SLOT_MATCH)
                    }
                }
                DiffKind::OnlyInA => {
                    if cmp.show_only_a {
                        (a_id, SLOT_ONLY_A_GRAY)
                    } else if cmp.hide_matching {
                        (BlockStateId::AIR, SLOT_NONE)
                    } else {
                        (a_id, SLOT_MATCH)
                    }
                }
                DiffKind::OnlyInB => {
                    if cmp.show_only_b {
                        (b_id, SLOT_ONLY_B_GRAY)
                    } else {
                        (BlockStateId::AIR, SLOT_NONE)
                    }
                }
            }
        }
    }
}

/// Meshes a single LOD 0 chunk at `target_pos` comparing `snap_a` (World A) against `snap_b` (World B).
///
/// - Matching blocks route into the standard `packed_opaque`/`packed_cutout`/`packed_translucent` buckets.
/// - Mismatched blocks route into `packed_diff_red` / `diff_red`.
/// - World-A-only blocks route into `packed_diff_gray_a` / `diff_gray_a`.
/// - World-B-only blocks route into `packed_diff_gray_b` / `diff_gray_b`.
pub fn mesh_chunk_compare(
    target_pos: ChunkPos,
    snap_a: Option<&ChunkNeighborSnapshot>,
    snap_b: Option<&ChunkNeighborSnapshot>,
    reg: &BlockRegistry,
    opts: MeshOptions,
    cmp: CompareConfig,
) -> SectionMeshData {
    let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::Lod0MeshFull);

    // Fast bypass when viewing a single world without offset
    if cmp.view_mode == CompareViewMode::OnlyWorldA {
        return snap_a
            .map(|sa| mesh_chunk_full(sa, reg, opts))
            .unwrap_or_default();
    }
    if cmp.view_mode == CompareViewMode::OnlyWorldB
        && let Some(sb) = snap_b
        && sb.center.pos == target_pos
    {
        return mesh_chunk_full(sb, reg, opts);
    }

    let mut out = SectionMeshData::default();
    let mut section_ys = BTreeSet::new();
    if let Some(sa) = snap_a {
        for &sy in sa.center.sections.keys() {
            section_ys.insert(sy);
        }
    }
    if let Some(sb) = snap_b {
        for &sy in sb.center.sections.keys() {
            section_ys.insert(sy);
        }
    }

    // Disable underground cave culling when hide_matching is active so underground diffs show up.
    let mut effective_opts = opts;
    if cmp.hide_matching {
        effective_opts.cull_underground = false;
    }

    for sy in section_ys {
        // O(1) fast path for byte-identical sections in Diff mode
        if cmp.view_mode == CompareViewMode::Diff
            && let (Some(sa), Some(sb)) = (snap_a, snap_b)
            && let (Some(sec_a), Some(sec_b)) = (sa.center.sections.get(&sy), sb.center.sections.get(&sy))
            && sec_a.palette == sec_b.palette
            && sec_a.indices == sec_b.indices
            && sa.center.pos == target_pos
        {
            if cmp.hide_matching {
                continue;
            }
            mesh_section_into(sa, sy, reg, effective_opts, &mut out);
            continue;
        }

        mesh_section_compare_into(target_pos, snap_a, snap_b, sy, reg, effective_opts, cmp, &mut out);
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn mesh_section_compare_into(
    target_pos: ChunkPos,
    snap_a: Option<&ChunkNeighborSnapshot>,
    snap_b: Option<&ChunkNeighborSnapshot>,
    sy: i32,
    reg: &BlockRegistry,
    opts: MeshOptions,
    cmp: CompareConfig,
    out: &mut SectionMeshData,
) {
    let sec_a = snap_a.and_then(|s| s.center.sections.get(&sy));
    let sec_b = snap_b.and_then(|s| s.center.sections.get(&sy));
    let a_empty = sec_a.is_none_or(|s| s.is_all_air());
    let b_empty = sec_b.is_none_or(|s| s.is_all_air());
    if a_empty && b_empty {
        return;
    }

    let base_y = sy * 16;
    let cx_off = (target_pos.cx() * 16) as f32;
    let cz_off = (target_pos.cz() * 16) as f32;
    let batch_x_base = (target_pos.cx().rem_euclid(4) as u32) * 16;
    let batch_z_base = (target_pos.cz().rem_euclid(4) as u32) * 16;

    // 1. Pre-classify all 4096 voxels in this section for O(1) interior lookups
    let mut cell_ids = [BlockStateId::AIR; 4096];
    let mut cell_slots = [SLOT_NONE; 4096];
    let mut any_visible = false;

    for ly in 0..16usize {
        let y_off = ly << 8;
        for lz in 0..16usize {
            let yz_off = y_off | (lz << 4);
            for lx in 0..16usize {
                let a_id = sec_a.map(|s| s.get_block(lx, ly, lz)).unwrap_or(BlockStateId::AIR);
                let b_id = sec_b.map(|s| s.get_block(lx, ly, lz)).unwrap_or(BlockStateId::AIR);
                if a_id == BlockStateId::AIR && b_id == BlockStateId::AIR {
                    continue;
                }
                let (id, slot) = resolve_slot_and_block(a_id, b_id, reg, opts.hide_barriers, cmp);
                if slot != SLOT_NONE {
                    let idx = yz_off | lx;
                    cell_ids[idx] = id;
                    cell_slots[idx] = slot;
                    any_visible = true;
                }
            }
        }
    }

    if !any_visible {
        return;
    }

    let get_cell = |lx: i32, ly: i32, lz: i32| -> (BlockStateId, u8) {
        if (0..16).contains(&lx) && (0..16).contains(&ly) && (0..16).contains(&lz) {
            let idx = ((ly as usize) << 8) | ((lz as usize) << 4) | (lx as usize);
            return (cell_ids[idx], cell_slots[idx]);
        }
        let wy = base_y + ly;
        let a_id = snap_a
            .map(|s| s.get_block(lx, wy, lz))
            .unwrap_or(BlockStateId::AIR);
        let b_id = snap_b
            .map(|s| s.get_block(lx, wy, lz))
            .unwrap_or(BlockStateId::AIR);
        if a_id == BlockStateId::AIR && b_id == BlockStateId::AIR {
            return (BlockStateId::AIR, SLOT_NONE);
        }
        resolve_slot_and_block(a_id, b_id, reg, opts.hide_barriers, cmp)
    };

    let get_light = |lx: i32, ly: i32, lz: i32, slot: u8| -> u8 {
        let wy = base_y + ly;
        if slot == SLOT_ONLY_B_GRAY {
            snap_b.map(|s| s.get_light(lx, wy, lz)).unwrap_or(15)
        } else {
            snap_a
                .map(|s| s.get_light(lx, wy, lz))
                .or_else(|| snap_b.map(|s| s.get_light(lx, wy, lz)))
                .unwrap_or(15)
        }
    };

    let only_a_solid = cmp.only_a_alpha >= 0.99;
    let only_b_solid = cmp.only_b_alpha >= 0.99;

    // 2. 2D Greedy Meshing across all 6 faces for full-cube blocks
    let mut has_non_cube = false;
    let mut grid = [CompareGreedyFace::default(); 256];

    for face in Face::ALL {
        let n = face.normal();
        for slice in 0..16usize {
            let mut any_in_slice = false;
            for v in 0..16usize {
                for u in 0..16usize {
                    let (lx, ly, lz) = match face {
                        Face::East | Face::West => (slice, v, u),
                        Face::Up | Face::Down => (u, slice, v),
                        Face::South | Face::North => (u, v, slice),
                    };
                    let cell_idx = (v << 4) | u;
                    let vox_idx = (ly << 8) | (lz << 4) | lx;
                    let slot = cell_slots[vox_idx];
                    if slot == SLOT_NONE {
                        grid[cell_idx] = CompareGreedyFace::default();
                        continue;
                    }
                    let id = cell_ids[vox_idx];
                    let meta = reg.get_meta(id);
                    let is_cube_shape = opts.rough_graphics
                        || matches!(
                            meta.shape,
                            BlockShape::Cube
                                | BlockShape::Leaves
                                | BlockShape::Glass
                                | BlockShape::Chest
                                | BlockShape::Liquid
                        );
                    if !is_cube_shape {
                        has_non_cube = true;
                        grid[cell_idx] = CompareGreedyFace::default();
                        continue;
                    }

                    let fast_leaves = opts.fast_leaves || opts.cull_underground;
                    let treat_leaves_opaque =
                        opts.rough_graphics || (fast_leaves && meta.shape == BlockShape::Leaves);
                    let is_trans = !opts.rough_graphics && meta.is_translucent;

                    let nx = lx as i32 + n.x;
                    let ny = ly as i32 + n.y;
                    let nz = lz as i32 + n.z;
                    let (nb, nslot) = get_cell(nx, ny, nz);
                    if nslot != SLOT_NONE && nb != BlockStateId::AIR {
                        let nmeta = reg.get_meta(nb);
                        let n_opaque_geom = nmeta.is_full_opaque_cube
                            || (treat_leaves_opaque && nmeta.shape == BlockShape::Leaves);
                        // Neighbor opacity rules preserve crisp visibility of solid blocks behind
                        // semi-transparent OnlyInA / OnlyInB volumes, while culling internal faces
                        // within same-slot volumes to eliminate overdraw.
                        let nb_occludes_self = match slot {
                            SLOT_MATCH | SLOT_MISMATCH_RED => {
                                n_opaque_geom
                                    && matches!(
                                        nslot,
                                        SLOT_MATCH | SLOT_MISMATCH_RED
                                    )
                                    || (n_opaque_geom && nslot == SLOT_ONLY_A_GRAY && only_a_solid)
                                    || (n_opaque_geom && nslot == SLOT_ONLY_B_GRAY && only_b_solid)
                            }
                            SLOT_ONLY_A_GRAY => {
                                n_opaque_geom
                                    && matches!(
                                        nslot,
                                        SLOT_MATCH | SLOT_MISMATCH_RED | SLOT_ONLY_A_GRAY
                                    )
                            }
                            SLOT_ONLY_B_GRAY => {
                                n_opaque_geom
                                    && matches!(
                                        nslot,
                                        SLOT_MATCH | SLOT_MISMATCH_RED | SLOT_ONLY_B_GRAY
                                    )
                            }
                            _ => false,
                        };

                        if nb_occludes_self {
                            if !opts.show_hidden_ores || !is_ore(reg, id) {
                                grid[cell_idx] = CompareGreedyFace::default();
                                continue;
                            }
                        } else if is_trans
                            && nslot == slot
                            && (nb == id || (meta.is_liquid && nmeta.is_liquid))
                        {
                            grid[cell_idx] = CompareGreedyFace::default();
                            continue;
                        }
                    }

                    let bucket_kind = if opts.rough_graphics {
                        1
                    } else if meta.is_liquid {
                        let is_side = !matches!(face, Face::Up | Face::Down);
                        if is_side {
                            let (above, aslot) = get_cell(lx as i32, ly as i32 + 1, lz as i32);
                            if aslot == SLOT_NONE
                                || above == BlockStateId::AIR
                                || !reg.get_meta(above).is_liquid
                            {
                                5
                            } else {
                                4
                            }
                        } else {
                            4
                        }
                    } else if meta.is_translucent {
                        3
                    } else if meta.shape == BlockShape::Leaves && fast_leaves {
                        1
                    } else if meta.is_alpha_cutout {
                        2
                    } else {
                        1
                    };

                    let light = if meta.is_liquid {
                        get_light(lx as i32, ly as i32, lz as i32, slot).max(10)
                    } else {
                        get_light(nx, ny, nz, slot)
                    };
                    let alpha = if meta.is_liquid { 200 } else { 255 };
                    let color = shaded_color(meta, face, light, alpha);
                    let tile = meta.face_tiles[face.index()];
                    grid[cell_idx] = CompareGreedyFace {
                        tile,
                        color,
                        bucket_kind,
                        diff_slot: slot,
                    };
                    any_in_slice = true;
                }
            }

            if !any_in_slice {
                continue;
            }

            for v0 in 0..16usize {
                let mut u0 = 0usize;
                while u0 < 16 {
                    let cell = grid[(v0 << 4) | u0];
                    if cell.bucket_kind == 0 {
                        u0 += 1;
                        continue;
                    }
                    let mut du = 1usize;
                    while u0 + du < 16 && grid[(v0 << 4) | (u0 + du)] == cell {
                        du += 1;
                    }
                    let mut dv = 1usize;
                    'extend_v: while v0 + dv < 16 {
                        let row_base = (v0 + dv) << 4;
                        for k in 0..du {
                            if grid[row_base | (u0 + k)] != cell {
                                break 'extend_v;
                            }
                        }
                        dv += 1;
                    }
                    for rv in v0..(v0 + dv) {
                        let row_base = rv << 4;
                        for ru in u0..(u0 + du) {
                            grid[row_base | ru].bucket_kind = 0;
                        }
                    }

                    let (local_x, local_y, local_z) = match face {
                        Face::East | Face::West => (slice, base_y + v0 as i32, u0),
                        Face::Up | Face::Down => (u0, base_y + slice as i32, v0),
                        Face::South | Face::North => (u0, base_y + v0 as i32, slice),
                    };

                    if cell.bucket_kind != 5 && (-1024..=3071).contains(&local_y) {
                        let packed = PackedCubeQuad::pack(
                            batch_x_base + local_x as u32,
                            batch_z_base + local_z as u32,
                            local_y,
                            du as u32,
                            dv as u32,
                            face,
                            cell.tile,
                            cell.color,
                            cell.bucket_kind == 4,
                        );
                        match cell.diff_slot {
                            SLOT_MISMATCH_RED => out.packed_diff_red.push(packed),
                            SLOT_ONLY_A_GRAY => out.packed_diff_gray_a.push(packed),
                            SLOT_ONLY_B_GRAY => out.packed_diff_gray_b.push(packed),
                            _ => match cell.bucket_kind {
                                2 => out.packed_cutout.push(packed),
                                3 | 4 => out.packed_translucent.push(packed),
                                _ => out.packed_opaque.push(packed),
                            },
                        }
                    } else {
                        let top_trim = if cell.bucket_kind == 5 { 0.125f32 } else { 0.0f32 };
                        let (min, max) = match face {
                            Face::East | Face::West => (
                                [
                                    cx_off + slice as f32,
                                    (base_y + v0 as i32) as f32,
                                    cz_off + u0 as f32,
                                ],
                                [
                                    cx_off + (slice + 1) as f32,
                                    (base_y + (v0 + dv) as i32) as f32 - top_trim,
                                    cz_off + (u0 + du) as f32,
                                ],
                            ),
                            Face::Up | Face::Down => (
                                [
                                    cx_off + u0 as f32,
                                    (base_y + slice as i32) as f32,
                                    cz_off + v0 as f32,
                                ],
                                [
                                    cx_off + (u0 + du) as f32,
                                    (base_y + (slice + 1) as i32) as f32 - top_trim,
                                    cz_off + (v0 + dv) as f32,
                                ],
                            ),
                            Face::South | Face::North => (
                                [
                                    cx_off + u0 as f32,
                                    (base_y + v0 as i32) as f32,
                                    cz_off + slice as f32,
                                ],
                                [
                                    cx_off + (u0 + du) as f32,
                                    (base_y + (v0 + dv) as i32) as f32 - top_trim,
                                    cz_off + (slice + 1) as f32,
                                ],
                            ),
                        };
                        let uv = TextureAtlas::tiled_uv(cell.tile, du, dv);
                        let corners = unit_face_corners(face, min, max);
                        let bucket = match cell.diff_slot {
                            SLOT_MISMATCH_RED => &mut out.diff_red,
                            SLOT_ONLY_A_GRAY => &mut out.diff_gray_a,
                            SLOT_ONLY_B_GRAY => &mut out.diff_gray_b,
                            _ => match cell.bucket_kind {
                                2 => &mut out.alpha_cutout,
                                3..=5 => &mut out.translucent,
                                _ => &mut out.opaque,
                            },
                        };
                        bucket.push_quad(corners, uv, cell.color);
                    }
                    u0 += du;
                }
            }
        }
    }

    if !has_non_cube || opts.rough_graphics {
        return;
    }

    // 3. Non-cube blocks (stairs, slabs, fences, plants, torches, doors, etc.)
    let fallback_snap = snap_a.or(snap_b);
    let Some(ref_snap) = fallback_snap else {
        return;
    };

    for ly in 0..16usize {
        let wy = base_y + ly as i32;
        for lz in 0..16usize {
            for lx in 0..16usize {
                let idx = (ly << 8) | (lz << 4) | lx;
                let slot = cell_slots[idx];
                if slot == SLOT_NONE {
                    continue;
                }
                let id = cell_ids[idx];
                let meta = reg.get_meta(id);
                if meta.is_air
                    || matches!(
                        meta.shape,
                        BlockShape::Cube
                            | BlockShape::Leaves
                            | BlockShape::Glass
                            | BlockShape::Chest
                            | BlockShape::Liquid
                            | BlockShape::Air
                    )
                {
                    continue;
                }

                let active_snap = if slot == SLOT_ONLY_B_GRAY {
                    snap_b.unwrap_or(ref_snap)
                } else {
                    snap_a.unwrap_or(ref_snap)
                };
                let bucket: &mut MeshBucket = match slot {
                    SLOT_MISMATCH_RED => &mut out.diff_red,
                    SLOT_ONLY_A_GRAY => &mut out.diff_gray_a,
                    SLOT_ONLY_B_GRAY => &mut out.diff_gray_b,
                    _ => {
                        if meta.is_translucent {
                            &mut out.translucent
                        } else if meta.is_alpha_cutout {
                            &mut out.alpha_cutout
                        } else {
                            &mut out.opaque
                        }
                    }
                };

                let bx = cx_off + lx as f32;
                let by = wy as f32;
                let bz = cz_off + lz as f32;
                let state = reg.get_state(id);

                match meta.shape {
                    BlockShape::Slab => {
                        let is_top = state.properties.get("half").is_some_and(|v| v == "top")
                            || meta.legacy_id.is_some_and(|(_, d)| (d & 8) != 0);
                        let (y0, y1) = if is_top { (0.5, 1.0) } else { (0.0, 0.5) };
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by + y0, bz],
                            [bx + 1.0, by + y1, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Stairs => {
                        let is_top = state.properties.get("half").is_some_and(|v| v == "top")
                            || meta.legacy_id.is_some_and(|(_, d)| (d & 4) != 0);
                        let facing = state
                            .properties
                            .get("facing")
                            .map(|s| s.as_str())
                            .unwrap_or("north");
                        let (base_y0, base_y1, step_y0, step_y1) = if is_top {
                            (0.5, 1.0, 0.0, 0.5)
                        } else {
                            (0.0, 0.5, 0.5, 1.0)
                        };
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by + base_y0, bz],
                            [bx + 1.0, by + base_y1, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                        let (sx0, sz0, sx1, sz1) = match facing {
                            "east" => (0.5, 0.0, 1.0, 1.0),
                            "west" => (0.0, 0.0, 0.5, 1.0),
                            "south" => (0.0, 0.5, 1.0, 1.0),
                            _ => (0.0, 0.0, 1.0, 0.5),
                        };
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + sx0, by + step_y0, bz + sz0],
                            [bx + sx1, by + step_y1, bz + sz1],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::CrossPlant => {
                        let light = active_snap.get_light(lx as i32, wy, lz as i32);
                        let col = shaded_color(meta, Face::Up, light, 255);
                        let uv = TextureAtlas::tile_uv(meta.face_tiles[0]);
                        bucket.push_quad(
                            [
                                [bx + 0.15, by + 1.0, bz + 0.15],
                                [bx + 0.85, by + 1.0, bz + 0.85],
                                [bx + 0.85, by, bz + 0.85],
                                [bx + 0.15, by, bz + 0.15],
                            ],
                            uv,
                            col,
                        );
                        bucket.push_quad(
                            [
                                [bx + 0.85, by + 1.0, bz + 0.15],
                                [bx + 0.15, by + 1.0, bz + 0.85],
                                [bx + 0.15, by, bz + 0.85],
                                [bx + 0.85, by, bz + 0.15],
                            ],
                            uv,
                            col,
                        );
                    }
                    BlockShape::Torch => {
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.4375, by, bz + 0.4375],
                            [bx + 0.5625, by + 0.625, bz + 0.5625],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Fence | BlockShape::FenceGate => {
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.375, by, bz + 0.375],
                            [bx + 0.625, by + 1.0, bz + 0.625],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::SnowLayer => {
                        let layers = meta
                            .legacy_id
                            .map(|(_, d)| ((d & 7) + 1) as f32 / 8.0)
                            .unwrap_or(0.125);
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + layers, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    BlockShape::Carpet
                    | BlockShape::Rail
                    | BlockShape::Redstone
                    | BlockShape::PressurePlate => {
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx, by, bz],
                            [bx + 1.0, by + 0.0625, bz + 1.0],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                    _ => {
                        // Doors, trapdoors, panes, beds, signs, buttons, etc.
                        emit_box_all_faces(
                            active_snap,
                            lx as i32,
                            wy,
                            lz as i32,
                            [bx + 0.0625, by, bz + 0.0625],
                            [bx + 0.9375, by + 0.875, bz + 0.9375],
                            meta,
                            bucket,
                            255,
                            None,
                        );
                    }
                }
            }
        }
    }
}

#[inline]
fn blend_lod_diff_color(
    base: [u8; 4],
    shade: f32,
    slot: u8,
    cmp: CompareConfig,
) -> ([u8; 4], bool) {
    let s = cmp.overlay_strength.clamp(0.0, 1.0);
    let mix_u8 = |a: f32, b: f32| -> u8 { (a * (1.0 - s) + b * s).clamp(0.0, 255.0) as u8 };
    let sh = shade.max(0.35);
    match slot {
        SLOT_MISMATCH_RED => (
            [
                mix_u8(base[0] as f32, 240.0 * sh),
                mix_u8(base[1] as f32, 42.0 * sh),
                mix_u8(base[2] as f32, 42.0 * sh),
                255,
            ],
            false,
        ),
        SLOT_ONLY_A_GRAY => {
            let alpha = (cmp.only_a_alpha.clamp(0.25, 1.0) * 255.0) as u8;
            (
                [
                    mix_u8(base[0] as f32, 87.0 * sh),
                    mix_u8(base[1] as f32, 194.0 * sh),
                    mix_u8(base[2] as f32, 255.0 * sh),
                    alpha,
                ],
                alpha < 252,
            )
        }
        SLOT_ONLY_B_GRAY => {
            let alpha = (cmp.only_b_alpha.clamp(0.15, 1.0) * 255.0) as u8;
            (
                [
                    mix_u8(base[0] as f32, 41.0 * sh),
                    mix_u8(base[1] as f32, 92.0 * sh),
                    mix_u8(base[2] as f32, 240.0 * sh),
                    alpha,
                ],
                alpha < 252,
            )
        }
        _ => (base, base[3] < 250),
    }
}

/// True 3D 6-face greedy-meshed LOD 1/2 builder comparing World A and World B over an 8x8 tile.
#[allow(clippy::too_many_arguments)]
pub fn mesh_lod_voxel_tile_compare(
    rx8: i32,
    rz8: i32,
    chunks_a: &[LodChunkVoxels],
    chunks_b: &[LodChunkVoxels],
    step: u8,
    palette: &LodColorTable,
    min_y: i32,
    reg: &BlockRegistry,
    cmp: CompareConfig,
) -> (SectionMeshData, LodTileOccupancy) {
    if cmp.view_mode == CompareViewMode::OnlyWorldA {
        return mesh_lod_voxel_tile(rx8, rz8, chunks_a, step, palette, min_y);
    }
    if cmp.view_mode == CompareViewMode::OnlyWorldB {
        return mesh_lod_voxel_tile(rx8, rz8, chunks_b, step, palette, min_y);
    }

    let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::LodVoxelMesh);
    let step = if step <= 2 { 2u8 } else { 4u8 };
    let s = step as usize;
    let chunk_xz = 16 / s;
    let grid_xz = 128 / s;

    let max_y_a = chunks_a.iter().map(|c| c.populated_y_cells).max().unwrap_or(0);
    let max_y_b = chunks_b.iter().map(|c| c.populated_y_cells).max().unwrap_or(0);
    let max_y_cells = max_y_a.max(max_y_b);

    let mut occ = LodTileOccupancy::new_empty(rx8, rz8, step, min_y, max_y_cells);
    let mut out = SectionMeshData::default();
    if max_y_cells == 0 || (chunks_a.is_empty() && chunks_b.is_empty()) {
        return (out, occ);
    }

    let total_cells = max_y_cells * grid_xz * grid_xz;
    let mut grid_a = vec![BlockStateId::AIR; total_cells];
    let mut grid_b = vec![BlockStateId::AIR; total_cells];
    let origin_cx = rx8 * 8;
    let origin_cz = rz8 * 8;

    let fill_grid = |chunks: &[LodChunkVoxels], dst: &mut [BlockStateId]| {
        for chunk in chunks {
            let rel_cx = chunk.pos.cx() - origin_cx;
            let rel_cz = chunk.pos.cz() - origin_cz;
            if !(0..8).contains(&rel_cx) || !(0..8).contains(&rel_cz) || chunk.xz_cells != chunk_xz {
                continue;
            }
            let gx_base = (rel_cx as usize) * chunk_xz;
            let gz_base = (rel_cz as usize) * chunk_xz;
            let cy_limit = chunk.populated_y_cells.min(max_y_cells);
            let y_shift = ((chunk.min_y - min_y) / (step as i32)) as isize;
            for cy in 0..cy_limit {
                let dst_cy = cy as isize + y_shift;
                if dst_cy < 0 || dst_cy >= max_y_cells as isize {
                    continue;
                }
                let dst_cy = dst_cy as usize;
                for cz in 0..chunk_xz {
                    let gz = gz_base + cz;
                    let dst_row = (dst_cy * grid_xz + gz) * grid_xz + gx_base;
                    let src_row = (cy * chunk_xz + cz) * chunk_xz;
                    for cx in 0..chunk_xz {
                        let b = chunk.cells[src_row + cx];
                        if b != BlockStateId::AIR {
                            dst[dst_row + cx] = b;
                        }
                    }
                }
            }
        }
    };
    fill_grid(chunks_a, &mut grid_a);
    fill_grid(chunks_b, &mut grid_b);

    let hide_barriers = mcedit_core::hide_barriers();
    let mut resolved_id = vec![BlockStateId::AIR; total_cells];
    let mut resolved_slot = vec![SLOT_NONE; total_cells];
    for idx in 0..total_cells {
        let a = grid_a[idx];
        let b = grid_b[idx];
        if a == BlockStateId::AIR && b == BlockStateId::AIR {
            continue;
        }
        let (id, slot) = resolve_slot_and_block(a, b, reg, hide_barriers, cmp);
        if slot != SLOT_NONE {
            resolved_id[idx] = id;
            resolved_slot[idx] = slot;
            let cy = idx / (grid_xz * grid_xz);
            let rem = idx % (grid_xz * grid_xz);
            let gz = rem / grid_xz;
            let gx = rem % grid_xz;
            occ.set_bit_public(gx, cy, gz);
        }
    }

    let base_wx = (rx8 * 128) as f32;
    let base_wz = (rz8 * 128) as f32;
    let base_wy = min_y as f32;
    let cell = s as f32;
    let quantize = step >= 4;

    let get_cell = |gx: isize, cy: isize, gz: isize| -> (BlockStateId, u8) {
        if gx < 0
            || gx >= grid_xz as isize
            || gz < 0
            || gz >= grid_xz as isize
            || cy < 0
            || cy >= max_y_cells as isize
        {
            (BlockStateId::AIR, SLOT_NONE)
        } else {
            let idx = ((cy as usize) * grid_xz + (gz as usize)) * grid_xz + (gx as usize);
            (resolved_id[idx], resolved_slot[idx])
        }
    };

    let only_a_solid = cmp.only_a_alpha >= 0.99;
    let only_b_solid = cmp.only_b_alpha >= 0.99;

    let face_entry = |id: BlockStateId,
                      slot: u8,
                      nb: BlockStateId,
                      nslot: u8,
                      face: Face,
                      shade: f32|
     -> Option<([u8; 4], bool)> {
        if slot == SLOT_NONE || id == BlockStateId::AIR {
            return None;
        }
        let vis = palette.get_with_reg(id, reg);
        if nslot != SLOT_NONE && nb != BlockStateId::AIR {
            let nvis = palette.get_with_reg(nb, reg);
            let nb_solid_slot = matches!(nslot, SLOT_MATCH | SLOT_MISMATCH_RED)
                || (nslot == SLOT_ONLY_A_GRAY && (only_a_solid || slot == SLOT_ONLY_A_GRAY))
                || (nslot == SLOT_ONLY_B_GRAY && (only_b_solid || slot == SLOT_ONLY_B_GRAY));
            if vis.is_opaque && nvis.is_opaque && nb_solid_slot {
                return None;
            }
            if !vis.is_opaque && nslot == slot && (nvis.is_opaque || nb == id) {
                return None;
            }
        }
        let base_col = scale_rgb(vis.face_colors[face.index()], shade);
        let (mut col, diff_trans) = blend_lod_diff_color(base_col, shade, slot, cmp);
        if quantize {
            col[0] &= 0xF8;
            col[1] &= 0xF8;
            col[2] &= 0xF8;
        }
        Some((col, vis.is_translucent || diff_trans))
    };

    // 1. Up & Down
    let mut mask_xz: Vec<Option<([u8; 4], bool)>> = vec![None; grid_xz * grid_xz];
    for (face, dy, shade) in [(Face::Up, 1isize, 1.0f32), (Face::Down, -1isize, 0.50f32)] {
        for cy in 0..max_y_cells {
            let mut any = false;
            for gz in 0..grid_xz {
                let row_grid = (cy * grid_xz + gz) * grid_xz;
                let row_mask = gz * grid_xz;
                for gx in 0..grid_xz {
                    let slot = resolved_slot[row_grid + gx];
                    let entry = if slot != SLOT_NONE {
                        let id = resolved_id[row_grid + gx];
                        let (nb, nslot) = get_cell(gx as isize, cy as isize + dy, gz as isize);
                        face_entry(id, slot, nb, nslot, face, shade)
                    } else {
                        None
                    };
                    if entry.is_some() {
                        any = true;
                    }
                    mask_xz[row_mask + gx] = entry;
                }
            }
            if !any {
                continue;
            }
            greedy_merge_2d(&mut mask_xz, grid_xz, grid_xz, |u0, v0, w, h, col, is_trans| {
                let x0 = base_wx + (u0 as f32) * cell;
                let x1 = base_wx + ((u0 + w) as f32) * cell;
                let z0 = base_wz + (v0 as f32) * cell;
                let z1 = base_wz + ((v0 + h) as f32) * cell;
                let y0 = base_wy + (cy as f32) * cell;
                let y1 = y0 + cell;
                let bucket = if is_trans { &mut out.translucent } else { &mut out.opaque };
                bucket.push_quad(
                    unit_face_corners(face, [x0, y0, z0], [x1, y1, z1]),
                    [0.0, 0.0, 0.0, 0.0],
                    col,
                );
            });
        }
    }

    // 2. East & West
    let mut mask_zy: Vec<Option<([u8; 4], bool)>> = vec![None; grid_xz * max_y_cells];
    for (face, dx, shade, nudge) in [
        (Face::East, 1isize, 0.68f32, [-0.05f32, 0.0f32]),
        (Face::West, -1isize, 0.68f32, [0.05f32, 0.0f32]),
    ] {
        for gx in 0..grid_xz {
            let mut any = false;
            for cy in 0..max_y_cells {
                let row_mask = cy * grid_xz;
                for gz in 0..grid_xz {
                    let idx = (cy * grid_xz + gz) * grid_xz + gx;
                    let slot = resolved_slot[idx];
                    let entry = if slot != SLOT_NONE {
                        let id = resolved_id[idx];
                        let (nb, nslot) = get_cell(gx as isize + dx, cy as isize, gz as isize);
                        face_entry(id, slot, nb, nslot, face, shade)
                    } else {
                        None
                    };
                    if entry.is_some() {
                        any = true;
                    }
                    mask_zy[row_mask + gz] = entry;
                }
            }
            if !any {
                continue;
            }
            greedy_merge_2d(&mut mask_zy, grid_xz, max_y_cells, |u0, v0, w, h, col, is_trans| {
                let x0 = base_wx + (gx as f32) * cell;
                let x1 = x0 + cell;
                let z0 = base_wz + (u0 as f32) * cell;
                let z1 = base_wz + ((u0 + w) as f32) * cell;
                let y0 = base_wy + (v0 as f32) * cell;
                let y1 = base_wy + ((v0 + h) as f32) * cell;
                let bucket = if is_trans { &mut out.translucent } else { &mut out.opaque };
                bucket.push_quad(
                    unit_face_corners(face, [x0, y0, z0], [x1, y1, z1]),
                    [nudge[0], nudge[1], nudge[0], nudge[1]],
                    col,
                );
            });
        }
    }

    // 3. South & North
    let mut mask_xy: Vec<Option<([u8; 4], bool)>> = vec![None; grid_xz * max_y_cells];
    for (face, dz, shade, nudge) in [
        (Face::South, 1isize, 0.80f32, [0.0f32, -0.05f32]),
        (Face::North, -1isize, 0.80f32, [0.0f32, 0.05f32]),
    ] {
        for gz in 0..grid_xz {
            let mut any = false;
            for cy in 0..max_y_cells {
                let row_mask = cy * grid_xz;
                let row_grid = (cy * grid_xz + gz) * grid_xz;
                for gx in 0..grid_xz {
                    let slot = resolved_slot[row_grid + gx];
                    let entry = if slot != SLOT_NONE {
                        let id = resolved_id[row_grid + gx];
                        let (nb, nslot) = get_cell(gx as isize, cy as isize, gz as isize + dz);
                        face_entry(id, slot, nb, nslot, face, shade)
                    } else {
                        None
                    };
                    if entry.is_some() {
                        any = true;
                    }
                    mask_xy[row_mask + gx] = entry;
                }
            }
            if !any {
                continue;
            }
            greedy_merge_2d(&mut mask_xy, grid_xz, max_y_cells, |u0, v0, w, h, col, is_trans| {
                let x0 = base_wx + (u0 as f32) * cell;
                let x1 = base_wx + ((u0 + w) as f32) * cell;
                let y0 = base_wy + (v0 as f32) * cell;
                let y1 = base_wy + ((v0 + h) as f32) * cell;
                let z0 = base_wz + (gz as f32) * cell;
                let z1 = z0 + cell;
                let bucket = if is_trans { &mut out.translucent } else { &mut out.opaque };
                bucket.push_quad(
                    unit_face_corners(face, [x0, y0, z0], [x1, y1, z1]),
                    [nudge[0], nudge[1], nudge[0], nudge[1]],
                    col,
                );
            });
        }
    }

    (out, occ)
}

/// Top-down 2D Chunk View comparison tile over an 8x8 chunk super-region.
#[allow(clippy::too_many_arguments)]
pub fn mesh_lod_overhead_tile_compare(
    rx8: i32,
    rz8: i32,
    surfs_a: &[LodChunkSurface],
    surfs_b: &[LodChunkSurface],
    step: usize,
    palette: &LodColorTable,
    min_y: i32,
    max_y: i32,
    reg: &BlockRegistry,
    cmp: CompareConfig,
) -> SectionMeshData {
    if cmp.view_mode == CompareViewMode::OnlyWorldA {
        return mesh_lod_overhead_tile(rx8, rz8, surfs_a, step, palette, min_y, max_y);
    }
    if cmp.view_mode == CompareViewMode::OnlyWorldB {
        return mesh_lod_overhead_tile(rx8, rz8, surfs_b, step, palette, min_y, max_y);
    }

    let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::LodSurfaceMesh);
    let mut out = SectionMeshData::default();
    if surfs_a.is_empty() && surfs_b.is_empty() {
        return out;
    }

    let step = match step {
        1 => 1usize,
        2 => 2usize,
        _ => 4usize,
    };
    let grid_n = 128 / step;
    let sub_n = 16 / step;
    let origin_cx = rx8 * 8;
    let origin_cz = rz8 * 8;

    let sample_grid = |surfs: &[LodChunkSurface]| -> (Vec<i32>, Vec<BlockStateId>, Vec<[u8; 4]>) {
        let mut heights = vec![i32::MIN; grid_n * grid_n];
        let mut blocks = vec![BlockStateId::AIR; grid_n * grid_n];
        let mut colors = vec![[0u8; 4]; grid_n * grid_n];
        for surf in surfs {
            let rel_cx = surf.pos.cx() - origin_cx;
            let rel_cz = surf.pos.cz() - origin_cz;
            if !(0..8).contains(&rel_cx) || !(0..8).contains(&rel_cz) {
                continue;
            }
            let gx_base = (rel_cx as usize) * sub_n;
            let gz_base = (rel_cz as usize) * sub_n;
            for sz in 0..sub_n {
                for sx in 0..sub_n {
                    let mut best_h = i32::MIN;
                    let mut best_b = BlockStateId::AIR;
                    let mut best_c = [0u8; 4];
                    for dz in 0..step {
                        let lz = sz * step + dz;
                        for dx in 0..step {
                            let lx = sx * step + dx;
                            let idx = (lz << 4) | lx;
                            let h = surf.heights[idx];
                            if h > best_h {
                                best_h = h;
                                best_b = surf.blocks[idx];
                                best_c = if best_b != BlockStateId::AIR {
                                    let c = palette.get_with_reg(best_b, reg).face_colors
                                        [Face::Up.index()];
                                    [c[0], c[1], c[2], 255]
                                } else {
                                    surf.colors[idx]
                                };
                            }
                        }
                    }
                    let gidx = (gz_base + sz) * grid_n + (gx_base + sx);
                    heights[gidx] = best_h;
                    blocks[gidx] = best_b;
                    colors[gidx] = best_c;
                }
            }
        }
        (heights, blocks, colors)
    };

    let (ha, ba, ca) = sample_grid(surfs_a);
    let (hb, bb, cb) = sample_grid(surfs_b);

    let total_h = (max_y - min_y).max(1) as f32;
    let mut final_h = vec![i32::MIN; grid_n * grid_n];
    let mut final_c = vec![[0u8; 4]; grid_n * grid_n];

    for idx in 0..(grid_n * grid_n) {
        let a_ok = ha[idx] != i32::MIN;
        let b_ok = hb[idx] != i32::MIN;
        if !a_ok && !b_ok {
            continue;
        }
        let (h, base_c, slot) = match (a_ok, b_ok) {
            (true, false) => {
                if cmp.show_only_a {
                    (ha[idx], ca[idx], SLOT_ONLY_A_GRAY)
                } else if cmp.hide_matching {
                    continue;
                } else {
                    (ha[idx], ca[idx], SLOT_MATCH)
                }
            }
            (false, true) => {
                if cmp.show_only_b {
                    (hb[idx], cb[idx], SLOT_ONLY_B_GRAY)
                } else {
                    continue;
                }
            }
            (true, true) => {
                let same = ha[idx] == hb[idx]
                    && reg.matches(ba[idx], bb[idx], cmp.ignore_properties);
                if same {
                    if cmp.hide_matching {
                        continue;
                    }
                    (ha[idx], ca[idx], SLOT_MATCH)
                } else if cmp.show_mismatch {
                    (ha[idx].max(hb[idx]), ca[idx], SLOT_MISMATCH_RED)
                } else if cmp.hide_matching {
                    continue;
                } else {
                    (ha[idx], ca[idx], SLOT_MATCH)
                }
            }
            (false, false) => continue,
        };
        let shade = 0.55 + ((h - min_y) as f32 / total_h).clamp(0.0, 1.0) * 0.45;
        let shaded = scale_rgb(base_c, shade);
        let (mut col, _) = blend_lod_diff_color(shaded, shade, slot, cmp);
        col[3] = 255;
        final_h[idx] = h;
        final_c[idx] = col;
    }

    let base_wx = (rx8 * 128) as f32;
    let base_wz = (rz8 * 128) as f32;
    let cell = step as f32;
    let mut visited = vec![false; grid_n * grid_n];
    for gz in 0..grid_n {
        for gx in 0..grid_n {
            let idx = gz * grid_n + gx;
            if visited[idx] || final_h[idx] == i32::MIN {
                continue;
            }
            let h = final_h[idx];
            let c = final_c[idx];
            let mut w = 1usize;
            while gx + w < grid_n {
                let nidx = gz * grid_n + (gx + w);
                if visited[nidx] || final_h[nidx] != h || final_c[nidx] != c {
                    break;
                }
                w += 1;
            }
            let mut d = 1usize;
            'depth: while gz + d < grid_n {
                let row_off = (gz + d) * grid_n + gx;
                for ox in 0..w {
                    let nidx = row_off + ox;
                    if visited[nidx] || final_h[nidx] != h || final_c[nidx] != c {
                        break 'depth;
                    }
                }
                d += 1;
            }
            for oz in 0..d {
                let row_off = (gz + oz) * grid_n + gx;
                for ox in 0..w {
                    visited[row_off + ox] = true;
                }
            }
            let x0 = base_wx + (gx as f32) * cell;
            let x1 = base_wx + ((gx + w) as f32) * cell;
            let z0 = base_wz + (gz as f32) * cell;
            let z1 = base_wz + ((gz + d) as f32) * cell;
            let y1 = (h + 1) as f32;
            out.opaque.push_quad(
                unit_face_corners(Face::Up, [x0, y1 - 1.0, z0], [x1, y1, z1]),
                [0.0, 0.0, 0.0, 0.0],
                c,
            );
        }
    }

    out
}
