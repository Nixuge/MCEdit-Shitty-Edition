use crate::schema::{ParamDef, ParamKind, ParamMap, get_block, get_int, get_string};
use crate::traits::{FilterPlugin, PluginContext};
use glam::IVec3;
use mcedit_core::{
    BlockState, BlockStateId, BoundingBox, ChunkPos, analyze_box, delete_entities_in_box,
};

pub fn stock_filters() -> Vec<Box<dyn FilterPlugin>> {
    vec![
        Box::new(TopsoilFilter),
        Box::new(SetBiomeFilter),
        Box::new(SmoothFilter),
        Box::new(DecliffFilter),
        Box::new(FindBlocksFilter),
        Box::new(RemoveEntitiesFilter),
    ]
}

fn record_selection_chunks(ctx: &mut PluginContext<'_>, selection: BoundingBox) {
    for cp in selection.iter_chunks() {
        if ctx.world.chunk_exists(ctx.dim, cp) {
            ctx.tx.record_chunk_if_needed(ctx.world, cp);
        }
    }
    ctx.tx.expand_dirty_box(selection);
}

pub struct TopsoilFilter;
impl FilterPlugin for TopsoilFilter {
    fn id(&self) -> &'static str { "topsoil" }
    fn display_name(&self) -> &'static str { "Topsoil" }
    fn category(&self) -> &'static str { "Terrain" }
    fn params(&self) -> Vec<ParamDef> {
        vec![
            ParamDef {
                key: "block".into(),
                label: "Top Block".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:grass").with_prop("snowy", "false"),
                    allow_wildcard: false,
                },
            },
            ParamDef {
                key: "depth".into(),
                label: "Depth".into(),
                kind: ParamKind::Int { default: 1, min: 1, max: 32, step: 1 },
            },
        ]
    }
    fn execute(
        &self,
        ctx: &mut PluginContext<'_>,
        selection: BoundingBox,
        params: &ParamMap,
    ) -> Result<(), String> {
        let (st, _) = get_block(params, "block")
            .map(|(s, w)| (s.clone(), w))
            .unwrap_or_else(|| (BlockState::new("minecraft:grass"), false));
        let fill_id = ctx.world.registry.write().intern(st);
        let depth = get_int(params, "depth", 1).max(1) as i32;
        record_selection_chunks(ctx, selection);

        let total = (selection.size().x as u64) * (selection.size().z as u64);
        let mut done = 0u64;
        for z in selection.min.z..selection.max.z {
            for x in selection.min.x..selection.max.x {
                if ctx.progress.is_cancelled() {
                    return Err("Cancelled".into());
                }
                let mut rem = if selection.max.y >= ctx.world.max_y()
                    || ctx
                        .world
                        .get_block(ctx.dim, IVec3::new(x, selection.max.y, z))
                        == BlockStateId::AIR
                {
                    depth
                } else {
                    0
                };
                for y in (selection.min.y..selection.max.y).rev() {
                    let p = IVec3::new(x, y, z);
                    if ctx.world.get_block(ctx.dim, p) == BlockStateId::AIR {
                        rem = depth;
                    } else if rem > 0 {
                        ctx.world.set_block(ctx.dim, p, fill_id);
                        rem -= 1;
                    }
                }
                done += 1;
                ctx.progress.set(done, total);
            }
        }
        Ok(())
    }
}

pub struct SetBiomeFilter;
impl FilterPlugin for SetBiomeFilter {
    fn id(&self) -> &'static str { "set_biome" }
    fn display_name(&self) -> &'static str { "Set Biome" }
    fn category(&self) -> &'static str { "Terrain" }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "biome".into(),
            label: "Biome".into(),
            kind: ParamKind::Choice {
                options: vec![
                    ("0".into(), "Ocean (0)".into()),
                    ("1".into(), "Plains (1)".into()),
                    ("2".into(), "Desert (2)".into()),
                    ("3".into(), "Extreme Hills (3)".into()),
                    ("4".into(), "Forest (4)".into()),
                    ("5".into(), "Taiga (5)".into()),
                    ("6".into(), "Swampland (6)".into()),
                    ("7".into(), "River (7)".into()),
                    ("8".into(), "Hell / Nether (8)".into()),
                    ("9".into(), "The End (9)".into()),
                    ("14".into(), "Mushroom Island (14)".into()),
                    ("21".into(), "Jungle (21)".into()),
                    ("29".into(), "Roofed Forest (29)".into()),
                    ("35".into(), "Savanna (35)".into()),
                    ("37".into(), "Mesa (37)".into()),
                ],
                default_idx: 1,
            },
        }]
    }
    fn execute(
        &self,
        ctx: &mut PluginContext<'_>,
        selection: BoundingBox,
        params: &ParamMap,
    ) -> Result<(), String> {
        let biome_id = get_string(params, "biome", "1")
            .parse::<u8>()
            .unwrap_or(1);
        record_selection_chunks(ctx, selection);

        for cp in selection.iter_chunks() {
            let origin = cp.world_origin();
            let x0 = (selection.min.x - origin.x).clamp(0, 15) as usize;
            let x1 = (selection.max.x - 1 - origin.x).clamp(0, 15) as usize;
            let z0 = (selection.min.z - origin.z).clamp(0, 15) as usize;
            let z1 = (selection.max.z - 1 - origin.z).clamp(0, 15) as usize;
            if let Ok(Some(chunk)) = ctx.world.get_chunk_mut(ctx.dim, cp) {
                for z in z0..=z1 {
                    for x in x0..=x1 {
                        chunk.biomes.set(x, selection.min.y, z, biome_id);
                    }
                }
                chunk.dirty = true;
            }
        }
        Ok(())
    }
}

pub struct SmoothFilter;
impl FilterPlugin for SmoothFilter {
    fn id(&self) -> &'static str { "smooth" }
    fn display_name(&self) -> &'static str { "Smooth Terrain" }
    fn category(&self) -> &'static str { "Terrain" }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "iterations".into(),
            label: "Iterations".into(),
            kind: ParamKind::Int { default: 1, min: 1, max: 10, step: 1 },
        }]
    }
    fn execute(
        &self,
        ctx: &mut PluginContext<'_>,
        selection: BoundingBox,
        params: &ParamMap,
    ) -> Result<(), String> {
        let iters = get_int(params, "iterations", 1).clamp(1, 10);
        record_selection_chunks(ctx, selection);
        let w = selection.size().x as usize;
        let l = selection.size().z as usize;
        if w == 0 || l == 0 {
            return Ok(());
        }

        for _ in 0..iters {
            let mut heights = vec![selection.min.y; w * l];
            let mut top_blocks = vec![BlockStateId::AIR; w * l];
            for rz in 0..l {
                for rx in 0..w {
                    let wx = selection.min.x + rx as i32;
                    let wz = selection.min.z + rz as i32;
                    for wy in (selection.min.y..selection.max.y).rev() {
                        let b = ctx.world.get_block(ctx.dim, IVec3::new(wx, wy, wz));
                        if b != BlockStateId::AIR {
                            heights[rz * w + rx] = wy + 1;
                            top_blocks[rz * w + rx] = b;
                            break;
                        }
                    }
                }
            }

            let mut target_h = heights.clone();
            for rz in 0..l {
                for rx in 0..w {
                    let mut sum = 0i32;
                    let mut count = 0i32;
                    for dz in -1isize..=1 {
                        for dx in -1isize..=1 {
                            let nx = rx as isize + dx;
                            let nz = rz as isize + dz;
                            if nx >= 0 && nx < w as isize && nz >= 0 && nz < l as isize {
                                sum += heights[(nz as usize) * w + (nx as usize)];
                                count += 1;
                            }
                        }
                    }
                    target_h[rz * w + rx] = (sum + count / 2) / count;
                }
            }

            for rz in 0..l {
                for rx in 0..w {
                    let wx = selection.min.x + rx as i32;
                    let wz = selection.min.z + rz as i32;
                    let old_h = heights[rz * w + rx];
                    let new_h = target_h[rz * w + rx].clamp(selection.min.y, selection.max.y);
                    let fill_b = top_blocks[rz * w + rx];
                    if new_h > old_h && fill_b != BlockStateId::AIR {
                        for wy in old_h..new_h {
                            ctx.world.set_block(ctx.dim, IVec3::new(wx, wy, wz), fill_b);
                        }
                    } else if new_h < old_h {
                        for wy in new_h..old_h {
                            ctx.world.set_block(ctx.dim, IVec3::new(wx, wy, wz), BlockStateId::AIR);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct DecliffFilter;
impl FilterPlugin for DecliffFilter {
    fn id(&self) -> &'static str { "decliff" }
    fn display_name(&self) -> &'static str { "Decliff" }
    fn category(&self) -> &'static str { "Terrain" }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "max_step".into(),
            label: "Max Cliff Step".into(),
            kind: ParamKind::Int { default: 2, min: 1, max: 8, step: 1 },
        }]
    }
    fn execute(
        &self,
        ctx: &mut PluginContext<'_>,
        selection: BoundingBox,
        params: &ParamMap,
    ) -> Result<(), String> {
        let max_step = get_int(params, "max_step", 2).max(1) as i32;
        record_selection_chunks(ctx, selection);
        let w = selection.size().x as usize;
        let l = selection.size().z as usize;
        if w < 2 || l < 2 {
            return Ok(());
        }

        let mut heights = vec![selection.min.y; w * l];
        let mut top_blocks = vec![BlockStateId::AIR; w * l];
        for rz in 0..l {
            for rx in 0..w {
                let wx = selection.min.x + rx as i32;
                let wz = selection.min.z + rz as i32;
                for wy in (selection.min.y..selection.max.y).rev() {
                    let b = ctx.world.get_block(ctx.dim, IVec3::new(wx, wy, wz));
                    if b != BlockStateId::AIR {
                        heights[rz * w + rx] = wy + 1;
                        top_blocks[rz * w + rx] = b;
                        break;
                    }
                }
            }
        }

        let mut new_h = heights.clone();
        for rz in 0..l {
            for rx in 0..w {
                let h = heights[rz * w + rx];
                let mut min_nb = h;
                for (dx, dz) in [(1isize, 0), (-1, 0), (0, 1), (0, -1)] {
                    let nx = rx as isize + dx;
                    let nz = rz as isize + dz;
                    if nx >= 0 && nx < w as isize && nz >= 0 && nz < l as isize {
                        min_nb = min_nb.min(heights[(nz as usize) * w + (nx as usize)]);
                    }
                }
                if h - min_nb > max_step {
                    new_h[rz * w + rx] = min_nb + max_step;
                }
            }
        }

        for rz in 0..l {
            for rx in 0..w {
                let wx = selection.min.x + rx as i32;
                let wz = selection.min.z + rz as i32;
                for wy in new_h[rz * w + rx]..heights[rz * w + rx] {
                    ctx.world.set_block(ctx.dim, IVec3::new(wx, wy, wz), BlockStateId::AIR);
                }
                let _ = top_blocks[rz * w + rx];
            }
        }
        Ok(())
    }
}

pub struct FindBlocksFilter;
impl FilterPlugin for FindBlocksFilter {
    fn id(&self) -> &'static str { "find_blocks" }
    fn display_name(&self) -> &'static str { "Find / Count Block" }
    fn category(&self) -> &'static str { "Analysis" }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "target".into(),
            label: "Target Block".into(),
            kind: ParamKind::Block {
                default: BlockState::new("minecraft:diamond_ore"),
                allow_wildcard: true,
            },
        }]
    }
    fn execute(
        &self,
        ctx: &mut PluginContext<'_>,
        selection: BoundingBox,
        params: &ParamMap,
    ) -> Result<(), String> {
        let (st, wildcard) = get_block(params, "target")
            .map(|(s, w)| (s.clone(), w))
            .unwrap_or_else(|| (BlockState::new("minecraft:diamond_ore"), false));
        let target_id = ctx.world.registry.write().intern(st.clone());
        let analysis = analyze_box(ctx.world, ctx.dim, selection);
        let reg = ctx.world.registry.read();
        let count: u64 = analysis
            .block_counts
            .iter()
            .filter(|(id, _)| reg.matches(*id, target_id, wildcard))
            .map(|(_, c)| *c)
            .sum();
        ctx.message = Some(format!("Found {count} matching '{st}' in selection."));
        let _ = ChunkPos::new(0, 0);
        Ok(())
    }
}

pub struct RemoveEntitiesFilter;
impl FilterPlugin for RemoveEntitiesFilter {
    fn id(&self) -> &'static str { "remove_entities" }
    fn display_name(&self) -> &'static str { "Remove Entities" }
    fn category(&self) -> &'static str { "Cleanup" }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "info".into(),
            label: "Removes all entities inside the selection box".into(),
            kind: ParamKind::Label("Removes all entities inside the selection box".into()),
        }]
    }
    fn execute(
        &self,
        ctx: &mut PluginContext<'_>,
        selection: BoundingBox,
        _params: &ParamMap,
    ) -> Result<(), String> {
        let removed = delete_entities_in_box(ctx.world, ctx.dim, selection, ctx.tx);
        ctx.message = Some(format!("Removed {removed} entities."));
        Ok(())
    }
}
