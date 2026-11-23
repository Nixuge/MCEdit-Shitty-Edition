use crate::schema::{ParamDef, ParamKind, ParamMap, get_block, get_int};
use crate::traits::{BrushMaskOptions, BrushPlugin, PluginContext};
use ahash::{AHashMap, AHashSet};
use glam::IVec3;
use mcedit_core::{
    BlockState, BlockStateId, BoundingBox, ChunkPos, CopyOptions, paste_volume,
};
use std::collections::VecDeque;

pub fn stock_brushes() -> Vec<Box<dyn BrushPlugin>> {
    vec![
        Box::new(FillBrush),
        Box::new(ReplaceBrush),
        Box::new(VariedFillBrush),
        Box::new(VariedReplaceBrush),
        Box::new(TopsoilBrush),
        Box::new(ErodeBrush),
        Box::new(FloodFillBrush),
        Box::new(PasteBrush),
    ]
}

/// 3D integer Bresenham line rasterization for smooth brush drag strokes.
pub fn bresenham_3d(a: IVec3, b: IVec3) -> Vec<IVec3> {
    let mut pts = Vec::new();
    let mut p = a;
    let d = (b - a).abs();
    let s = IVec3::new(
        if b.x > a.x { 1 } else { -1 },
        if b.y > a.y { 1 } else { -1 },
        if b.z > a.z { 1 } else { -1 },
    );

    pts.push(p);
    if d.x >= d.y && d.x >= d.z {
        let mut p1 = 2 * d.y - d.x;
        let mut p2 = 2 * d.z - d.x;
        while p.x != b.x {
            p.x += s.x;
            if p1 >= 0 {
                p.y += s.y;
                p1 -= 2 * d.x;
            }
            if p2 >= 0 {
                p.z += s.z;
                p2 -= 2 * d.x;
            }
            p1 += 2 * d.y;
            p2 += 2 * d.z;
            pts.push(p);
        }
    } else if d.y >= d.x && d.y >= d.z {
        let mut p1 = 2 * d.x - d.y;
        let mut p2 = 2 * d.z - d.y;
        while p.y != b.y {
            p.y += s.y;
            if p1 >= 0 {
                p.x += s.x;
                p1 -= 2 * d.y;
            }
            if p2 >= 0 {
                p.z += s.z;
                p2 -= 2 * d.y;
            }
            p1 += 2 * d.x;
            p2 += 2 * d.z;
            pts.push(p);
        }
    } else {
        let mut p1 = 2 * d.y - d.z;
        let mut p2 = 2 * d.x - d.z;
        while p.z != b.z {
            p.z += s.z;
            if p1 >= 0 {
                p.y += s.y;
                p1 -= 2 * d.z;
            }
            if p2 >= 0 {
                p.x += s.x;
                p2 -= 2 * d.z;
            }
            p1 += 2 * d.y;
            p2 += 2 * d.x;
            pts.push(p);
        }
    }
    pts
}

fn intern_param_block(ctx: &mut PluginContext<'_>, params: &ParamMap, key: &str, default_name: &str) -> (BlockStateId, bool) {
    let (st, wildcard) = get_block(params, key)
        .map(|(s, w)| (s.clone(), w))
        .unwrap_or_else(|| (BlockState::new(default_name), false));
    let id = ctx.world.registry.write().intern(st);
    (id, wildcard)
}

fn record_box_chunks(ctx: &mut PluginContext<'_>, b: BoundingBox) {
    for cp in b.iter_chunks() {
        if ctx.world.chunk_exists(ctx.dim, cp) {
            ctx.tx.record_chunk_if_needed(ctx.world, cp);
        }
    }
    ctx.tx.expand_dirty_box(b);
}

pub struct FillBrush;
impl BrushPlugin for FillBrush {
    fn id(&self) -> &'static str { "fill" }
    fn display_name(&self) -> &'static str { "Fill" }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "block".into(),
            label: "Block".into(),
            kind: ParamKind::Block {
                default: BlockState::new("minecraft:stone"),
                allow_wildcard: false,
            },
        }]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        mask: &BrushMaskOptions,
        params: &ParamMap,
        stroke_seed: u64,
    ) -> Result<(), String> {
        let (fill_id, _) = intern_param_block(ctx, params, "block", "minecraft:stone");
        record_box_chunks(ctx, mask.bounding_box(center));
        for off in mask.sample_offsets(stroke_seed) {
            ctx.world.set_block(ctx.dim, center + off, fill_id);
        }
        Ok(())
    }
}

pub struct ReplaceBrush;
impl BrushPlugin for ReplaceBrush {
    fn id(&self) -> &'static str { "replace" }
    fn display_name(&self) -> &'static str { "Replace" }
    fn params(&self) -> Vec<ParamDef> {
        vec![
            ParamDef {
                key: "target".into(),
                label: "Find".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:stone"),
                    allow_wildcard: true,
                },
            },
            ParamDef {
                key: "block".into(),
                label: "Replace With".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:cobblestone"),
                    allow_wildcard: false,
                },
            },
        ]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        mask: &BrushMaskOptions,
        params: &ParamMap,
        stroke_seed: u64,
    ) -> Result<(), String> {
        let (target_id, wildcard) = intern_param_block(ctx, params, "target", "minecraft:stone");
        let (fill_id, _) = intern_param_block(ctx, params, "block", "minecraft:cobblestone");
        record_box_chunks(ctx, mask.bounding_box(center));
        for off in mask.sample_offsets(stroke_seed) {
            let p = center + off;
            let cur = ctx.world.get_block(ctx.dim, p);
            let matched = ctx.world.registry.read().matches(cur, target_id, wildcard);
            if matched {
                ctx.world.set_block(ctx.dim, p, fill_id);
            }
        }
        Ok(())
    }
}

pub struct VariedFillBrush;
impl BrushPlugin for VariedFillBrush {
    fn id(&self) -> &'static str { "varied_fill" }
    fn display_name(&self) -> &'static str { "Varied Fill" }
    fn params(&self) -> Vec<ParamDef> {
        vec![
            ParamDef {
                key: "block_a".into(),
                label: "Block A".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:stone"),
                    allow_wildcard: false,
                },
            },
            ParamDef {
                key: "block_b".into(),
                label: "Block B".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:cobblestone"),
                    allow_wildcard: false,
                },
            },
            ParamDef {
                key: "chance_a".into(),
                label: "Chance A %".into(),
                kind: ParamKind::Int { default: 50, min: 0, max: 100, step: 5 },
            },
        ]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        mask: &BrushMaskOptions,
        params: &ParamMap,
        stroke_seed: u64,
    ) -> Result<(), String> {
        let (a_id, _) = intern_param_block(ctx, params, "block_a", "minecraft:stone");
        let (b_id, _) = intern_param_block(ctx, params, "block_b", "minecraft:cobblestone");
        let chance_a = get_int(params, "chance_a", 50).clamp(0, 100) as u64;
        record_box_chunks(ctx, mask.bounding_box(center));
        for off in mask.sample_offsets(stroke_seed) {
            let p = center + off;
            let roll = mix_hash(stroke_seed, p) % 100;
            let chosen = if roll < chance_a { a_id } else { b_id };
            ctx.world.set_block(ctx.dim, p, chosen);
        }
        Ok(())
    }
}

pub struct VariedReplaceBrush;
impl BrushPlugin for VariedReplaceBrush {
    fn id(&self) -> &'static str { "varied_replace" }
    fn display_name(&self) -> &'static str { "Varied Replace" }
    fn params(&self) -> Vec<ParamDef> {
        vec![
            ParamDef {
                key: "target".into(),
                label: "Find".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:stone"),
                    allow_wildcard: true,
                },
            },
            ParamDef {
                key: "block_a".into(),
                label: "Replace A".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:cobblestone"),
                    allow_wildcard: false,
                },
            },
            ParamDef {
                key: "block_b".into(),
                label: "Replace B".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:mossy_cobblestone"),
                    allow_wildcard: false,
                },
            },
            ParamDef {
                key: "chance_a".into(),
                label: "Chance A %".into(),
                kind: ParamKind::Int { default: 50, min: 0, max: 100, step: 5 },
            },
        ]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        mask: &BrushMaskOptions,
        params: &ParamMap,
        stroke_seed: u64,
    ) -> Result<(), String> {
        let (target_id, wildcard) = intern_param_block(ctx, params, "target", "minecraft:stone");
        let (a_id, _) = intern_param_block(ctx, params, "block_a", "minecraft:cobblestone");
        let (b_id, _) = intern_param_block(ctx, params, "block_b", "minecraft:mossy_cobblestone");
        let chance_a = get_int(params, "chance_a", 50).clamp(0, 100) as u64;
        record_box_chunks(ctx, mask.bounding_box(center));
        for off in mask.sample_offsets(stroke_seed) {
            let p = center + off;
            let cur = ctx.world.get_block(ctx.dim, p);
            if ctx.world.registry.read().matches(cur, target_id, wildcard) {
                let roll = mix_hash(stroke_seed, p) % 100;
                ctx.world.set_block(ctx.dim, p, if roll < chance_a { a_id } else { b_id });
            }
        }
        Ok(())
    }
}

pub struct TopsoilBrush;
impl BrushPlugin for TopsoilBrush {
    fn id(&self) -> &'static str { "topsoil" }
    fn display_name(&self) -> &'static str { "Topsoil" }
    fn params(&self) -> Vec<ParamDef> {
        vec![
            ParamDef {
                key: "block".into(),
                label: "Block".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:grass").with_prop("snowy", "false"),
                    allow_wildcard: false,
                },
            },
            ParamDef {
                key: "depth".into(),
                label: "Depth".into(),
                kind: ParamKind::Int { default: 1, min: 1, max: 16, step: 1 },
            },
        ]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        mask: &BrushMaskOptions,
        params: &ParamMap,
        stroke_seed: u64,
    ) -> Result<(), String> {
        let (fill_id, _) = intern_param_block(ctx, params, "block", "minecraft:grass");
        let depth = get_int(params, "depth", 1).max(1) as i32;
        let b = mask.bounding_box(center);
        record_box_chunks(ctx, b);

        // Find highest solid block inside mask column and paint downward `depth` solid blocks
        let offsets = mask.sample_offsets(stroke_seed);
        let mut columns: AHashMap<(i32, i32), (i32, i32)> = AHashMap::new();
        for off in offsets {
            let p = center + off;
            columns
                .entry((p.x, p.z))
                .and_modify(|(mn, mx)| {
                    *mn = (*mn).min(p.y);
                    *mx = (*mx).max(p.y);
                })
                .or_insert((p.y, p.y));
        }

        for ((wx, wz), (y_min, y_max)) in columns {
            let mut rem = 0;
            for wy in (y_min..=y_max).rev() {
                let p = IVec3::new(wx, wy, wz);
                let cur = ctx.world.get_block(ctx.dim, p);
                if cur == BlockStateId::AIR {
                    rem = depth;
                } else if rem > 0 {
                    ctx.world.set_block(ctx.dim, p, fill_id);
                    rem -= 1;
                }
            }
        }
        Ok(())
    }
}

pub struct ErodeBrush;
impl BrushPlugin for ErodeBrush {
    fn id(&self) -> &'static str { "erode" }
    fn display_name(&self) -> &'static str { "Erode" }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "strength".into(),
            label: "Strength".into(),
            kind: ParamKind::Int { default: 1, min: 1, max: 5, step: 1 },
        }]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        mask: &BrushMaskOptions,
        params: &ParamMap,
        stroke_seed: u64,
    ) -> Result<(), String> {
        let strength = get_int(params, "strength", 1).clamp(1, 5);
        let b = mask.bounding_box(center);
        record_box_chunks(ctx, b);
        let offsets = mask.sample_offsets(stroke_seed);

        for _ in 0..strength {
            let mut changes: Vec<(IVec3, BlockStateId)> = Vec::new();
            for &off in &offsets {
                let p = center + off;
                let cur = ctx.world.get_block(ctx.dim, p);
                let mut air_neighbors = 0;
                let mut solid_counts: AHashMap<BlockStateId, usize> = AHashMap::new();
                for n in [
                    IVec3::X,
                    IVec3::NEG_X,
                    IVec3::Y,
                    IVec3::NEG_Y,
                    IVec3::Z,
                    IVec3::NEG_Z,
                ] {
                    let nb = ctx.world.get_block(ctx.dim, p + n);
                    if nb == BlockStateId::AIR {
                        air_neighbors += 1;
                    } else {
                        *solid_counts.entry(nb).or_insert(0) += 1;
                    }
                }
                if cur != BlockStateId::AIR && air_neighbors >= 4 {
                    changes.push((p, BlockStateId::AIR));
                } else if cur == BlockStateId::AIR && (6 - air_neighbors) >= 4 {
                    // Preserve blockstate variant via majority vote (fixes legacy erode clay bug!)
                    if let Some((&best, _)) = solid_counts.iter().max_by_key(|(_, c)| *c) {
                        changes.push((p, best));
                    }
                }
            }
            for (p, id) in changes {
                ctx.world.set_block(ctx.dim, p, id);
            }
        }
        Ok(())
    }
}

pub struct FloodFillBrush;
impl BrushPlugin for FloodFillBrush {
    fn id(&self) -> &'static str { "flood_fill" }
    fn display_name(&self) -> &'static str { "Flood Fill" }
    fn uses_shape(&self) -> bool { false }
    fn params(&self) -> Vec<ParamDef> {
        vec![
            ParamDef {
                key: "block".into(),
                label: "Fill With".into(),
                kind: ParamKind::Block {
                    default: BlockState::new("minecraft:stone"),
                    allow_wildcard: false,
                },
            },
            ParamDef {
                key: "max_blocks".into(),
                label: "Max Blocks".into(),
                kind: ParamKind::Int { default: 4096, min: 1, max: 65536, step: 256 },
            },
        ]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        _mask: &BrushMaskOptions,
        params: &ParamMap,
        _stroke_seed: u64,
    ) -> Result<(), String> {
        let (fill_id, _) = intern_param_block(ctx, params, "block", "minecraft:stone");
        let max_blocks = get_int(params, "max_blocks", 4096).max(1) as usize;
        let target_id = ctx.world.get_block(ctx.dim, center);
        if target_id == fill_id {
            return Ok(());
        }

        let min_y = ctx.world.min_y();
        let max_y = ctx.world.max_y();
        let mut queue = VecDeque::new();
        let mut visited = AHashSet::new();
        queue.push_back(center);
        visited.insert(center);
        let mut filled = 0usize;

        while let Some(p) = queue.pop_front() {
            if filled >= max_blocks || ctx.progress.is_cancelled() {
                break;
            }
            let cp = ChunkPos::new(p.x.div_euclid(16), p.z.div_euclid(16));
            if !ctx.world.chunk_exists(ctx.dim, cp) {
                continue;
            }
            ctx.tx.record_chunk_if_needed(ctx.world, cp);
            ctx.world.set_block(ctx.dim, p, fill_id);
            ctx.tx
                .expand_dirty_box(BoundingBox::from_corners_inclusive(p, p));
            filled += 1;

            for n in [
                IVec3::X,
                IVec3::NEG_X,
                IVec3::Y,
                IVec3::NEG_Y,
                IVec3::Z,
                IVec3::NEG_Z,
            ] {
                let np = p + n;
                if np.y >= min_y
                    && np.y < max_y
                    && !visited.contains(&np)
                    && ctx.world.get_block(ctx.dim, np) == target_id
                {
                    visited.insert(np);
                    queue.push_back(np);
                }
            }
        }
        Ok(())
    }
}

pub struct PasteBrush;
impl BrushPlugin for PasteBrush {
    fn id(&self) -> &'static str { "paste" }
    fn display_name(&self) -> &'static str { "Paste" }
    fn uses_shape(&self) -> bool { false }
    fn params(&self) -> Vec<ParamDef> {
        vec![ParamDef {
            key: "info".into(),
            label: "Stamps top Clipboard entry at cursor".into(),
            kind: ParamKind::Label("Stamps top Clipboard entry at cursor".into()),
        }]
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        _mask: &BrushMaskOptions,
        _params: &ParamMap,
        _stroke_seed: u64,
    ) -> Result<(), String> {
        let Some(vol) = ctx.active_clipboard.cloned() else {
            return Err("Clipboard is empty. Copy or import a schematic first.".to_string());
        };
        let origin = center - IVec3::new(vol.size.x as i32 / 2, 0, vol.size.z as i32 / 2);
        paste_volume(
            ctx.world,
            ctx.dim,
            &vol,
            origin,
            &CopyOptions::default(),
            ctx.tx,
        );
        Ok(())
    }
}

fn mix_hash(seed: u64, p: IVec3) -> u64 {
    let mut x = seed
        ^ (p.x as i64 as u64).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (p.y as i64 as u64).wrapping_mul(0xBF58476D1CE4E5B9)
        ^ (p.z as i64 as u64).wrapping_mul(0x94D049BB133111EB);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58476D1CE4E5B9);
    x ^ (x >> 27)
}
