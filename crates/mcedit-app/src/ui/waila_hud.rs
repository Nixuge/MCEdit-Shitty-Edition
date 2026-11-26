use crate::editor::EditorState;
use egui::{Color32, Context, FontId, Pos2, Rect, RichText, Stroke, TextureId, Vec2};
use mcedit_core::{BlockMeta, BlockShape, BlockState, BlockStateId, ChunkPos, Face};
use mcedit_nbt::NbtCompound;
use mcedit_render::{TextureAtlas, should_tint_face};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockHudInfo {
    pub display_name: String,
    pub block_id_line: String,
    pub properties_line: Option<String>,
    pub extra_line: Option<String>,
    pub mod_name: String,
    pub tile: [u8; 2],
    pub tint: [u8; 3],
    pub world_tag: Option<&'static str>,
}

/// Renders a classic WAILA / HWYLA / WTHIT style HUD at the top-center of the 3D viewport
/// showing the block currently under the cursor (or crosshair during mouse-look).
pub fn draw(ctx: &Context, editor: &EditorState, viewport_rect: Rect, atlas_tex_id: TextureId) {
    if !editor.config.show_block_hud || editor.world.is_none() || editor.jobs.is_busy() {
        return;
    }
    let Some(info) = resolve_hovered_block(editor) else {
        return;
    };

    let center_x = viewport_rect.center().x;
    let top_y = viewport_rect.min.y + 8.0;

    egui::Area::new(egui::Id::unique("waila_block_hud"))
        .pivot(egui::Align2::CENTER_TOP)
        .fixed_pos(Pos2::new(center_x, top_y))
        .order(egui::Order::Middle)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(16, 12, 24, 222))
                .stroke(Stroke::new(
                    1.5,
                    Color32::from_rgba_unmultiplied(88, 56, 165, 220),
                ))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(9, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Crisp 32x32 block texture icon from the terrain atlas
                        let icon_size = Vec2::splat(32.0);
                        let (icon_rect, _) = ui.allocate_exact_size(icon_size, egui::Sense::hover());
                        let [u0, v0, u1, v1] = TextureAtlas::tile_uv(info.tile);
                        let uv_rect = Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u1, v1));
                        let tint = Color32::from_rgb(info.tint[0], info.tint[1], info.tint[2]);
                        ui.painter()
                            .image(atlas_tex_id, icon_rect, uv_rect, tint);

                        ui.add_space(4.0);

                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.5;

                            // Line 1: Display Name (+ optional Compare World B tag)
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&info.display_name)
                                        .strong()
                                        .size(13.5)
                                        .color(Color32::from_rgb(248, 248, 252)),
                                );
                                if let Some(tag) = info.world_tag {
                                    ui.label(
                                        RichText::new(tag)
                                            .size(10.5)
                                            .color(Color32::from_rgb(115, 168, 255)),
                                    );
                                }
                            });

                            // Line 2: Registry ID (+ legacy id:data when available)
                            ui.label(
                                RichText::new(&info.block_id_line)
                                    .font(FontId::monospace(10.5))
                                    .color(Color32::from_rgb(168, 174, 192)),
                            );

                            // Line 3: Block state properties (if any)
                            if let Some(props) = &info.properties_line {
                                ui.label(
                                    RichText::new(props)
                                        .font(FontId::monospace(9.5))
                                        .color(Color32::from_rgb(138, 196, 196)),
                                );
                            }

                            // Line 4: Block entity info (command block, spawner, container, etc.)
                            if let Some(extra) = &info.extra_line {
                                ui.label(
                                    RichText::new(extra)
                                        .font(FontId::monospace(9.5))
                                        .color(Color32::from_rgb(255, 200, 100)),
                                );
                            }

                            // Line 5: Mod / Namespace name in classic WAILA italic blue
                            ui.label(
                                RichText::new(&info.mod_name)
                                    .italics()
                                    .size(10.5)
                                    .color(Color32::from_rgb(100, 148, 255)),
                            );
                        });
                    });
                });
        });
}

fn resolve_hovered_block(editor: &EditorState) -> Option<BlockHudInfo> {
    let hit = editor.hover_hit?;
    let world = editor.world.as_ref()?;
    let dim = world.active_dim;
    let bp = hit.block_pos;

    let cp = ChunkPos::new(bp.x.div_euclid(16), bp.z.div_euclid(16));
    let lx = bp.x.rem_euclid(16) as usize;
    let lz = bp.z.rem_euclid(16) as usize;
    let loaded_chunk = world.get_loaded_chunk(dim, cp);
    let mut bid = loaded_chunk
        .map(|c| c.get_block(lx, bp.y, lz))
        .unwrap_or(BlockStateId::AIR);
    let mut world_tag = None;
    if bid == BlockStateId::AIR
        && editor.compare.is_active()
        && let Some((mcedit_core::DiffKind::OnlyInB, _, id_b)) = editor.inspect_compare_block(bp)
        && id_b != BlockStateId::AIR
    {
        bid = id_b;
        world_tag = Some("(World B)");
    }
    if bid == BlockStateId::AIR {
        return None;
    }

    let reg = editor.registry.read();
    let state = reg.get_state(bid);
    let meta = reg.get_meta(bid);
    if meta.is_air {
        return None;
    }

    // Look up block entity at this coordinate if the chunk is loaded (World A)
    let be_extra = if world_tag.is_none() {
        loaded_chunk.and_then(|chunk| {
            chunk
                .block_entities
                .iter()
                .find(|be| {
                    be.get_int("x") == Some(bp.x)
                        && be.get_int("y") == Some(bp.y)
                        && be.get_int("z") == Some(bp.z)
                })
                .and_then(extract_block_entity_summary)
        })
    } else {
        None
    };

    Some(format_block_hud_info(state, meta, be_extra, world_tag))
}

pub fn format_block_hud_info(
    state: &BlockState,
    meta: &BlockMeta,
    extra_line: Option<String>,
    world_tag: Option<&'static str>,
) -> BlockHudInfo {
    let (namespace, slug) = state
        .name
        .split_once(':')
        .unwrap_or(("minecraft", state.name.as_str()));

    let clean_meta_name = {
        let raw = meta.display_name.trim();
        // Strip redundant trailing " (namespace:id[props])" appended by BlockRegistry::intern
        if let Some(paren_idx) = raw.rfind(" (")
            && raw.ends_with(')')
            && (raw[paren_idx..].contains(':') || raw[paren_idx..].contains('['))
        {
            raw[..paren_idx].trim()
        } else {
            raw
        }
    };
    let display_name = if meta.visual_base_id.is_some() && !meta.is_air {
        // Synthesized from a visual fallback base (e.g. cherry_log -> oak_log):
        // derive the true human-readable name from the block's own identifier slug.
        prettify_identifier(slug)
    } else if !clean_meta_name.is_empty()
        && clean_meta_name != "Unknown Block"
        && clean_meta_name != state.name
        && !clean_meta_name.contains(':')
    {
        clean_meta_name.to_string()
    } else {
        prettify_identifier(slug)
    };

    let block_id_line = match meta.legacy_id {
        Some((id, data)) => format!("{} ({id}:{data})", state.name),
        None => state.name.clone(),
    };

    let properties_line = if state.properties.is_empty() {
        None
    } else {
        let joined = state
            .properties
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(truncate_chars(&joined, 60))
    };

    let mod_name = if namespace.eq_ignore_ascii_case("minecraft") {
        "Minecraft".to_string()
    } else {
        prettify_identifier(namespace)
    };

    let icon_face = match meta.shape {
        BlockShape::Liquid
        | BlockShape::Rail
        | BlockShape::Redstone
        | BlockShape::Carpet
        | BlockShape::SnowLayer
        | BlockShape::PressurePlate => Face::Up,
        _ => Face::North,
    };
    let tile = meta.face_tiles[icon_face.index()];
    let tint = if should_tint_face(meta, icon_face) {
        let m = meta.tint.rgb_multiplier();
        [
            (m[0] * 255.0).clamp(0.0, 255.0) as u8,
            (m[1] * 255.0).clamp(0.0, 255.0) as u8,
            (m[2] * 255.0).clamp(0.0, 255.0) as u8,
        ]
    } else {
        [255, 255, 255]
    };

    BlockHudInfo {
        display_name,
        block_id_line,
        properties_line,
        extra_line,
        mod_name,
        tile,
        tint,
        world_tag,
    }
}

fn extract_block_entity_summary(be: &NbtCompound) -> Option<String> {
    if let Some(cmd) = be.get_string("Command") {
        let t = cmd.trim();
        if !t.is_empty() {
            return Some(format!("Command: {}", truncate_chars(t, 52)));
        }
    }
    if let Some(spawn_data) = be.get_compound("SpawnData") {
        let entity_id = spawn_data
            .get_compound("entity")
            .and_then(|e| e.get_string("id"))
            .or_else(|| spawn_data.get_string("id"));
        if let Some(eid) = entity_id {
            let slug = eid.strip_prefix("minecraft:").unwrap_or(eid);
            return Some(format!("Spawner: {}", prettify_identifier(slug)));
        }
    } else if let Some(eid) = be.get_string("EntityId") {
        let slug = eid.strip_prefix("minecraft:").unwrap_or(eid);
        return Some(format!("Spawner: {}", prettify_identifier(slug)));
    }
    if let Some(items) = be.get_list("Items") {
        let n = items.len();
        if n > 0 {
            return Some(format!(
                "Container: {n} {}",
                if n == 1 { "stack" } else { "stacks" }
            ));
        }
    }
    None
}

fn prettify_identifier(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().to_string();
                    out.push_str(chars.as_str());
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() > max_chars {
        s.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcedit_core::BlockRegistry;
    use mcedit_nbt::{NbtList, NbtTag};

    #[test]
    fn formats_vanilla_modded_and_block_entity_waila_info() {
        let mut reg = BlockRegistry::with_pre113_defaults();

        // 1. Grass block
        let grass_id = reg.intern(BlockState::new("minecraft:grass_block").with_prop("snowy", "false"));
        let grass = format_block_hud_info(reg.get_state(grass_id), reg.get_meta(grass_id), None, None);
        assert_eq!(grass.display_name, "Grass Block");
        assert_eq!(grass.mod_name, "Minecraft");
        assert_eq!(grass.properties_line.as_deref(), Some("snowy=false"));

        // 2. Modded block fallback prettifies both block name and mod namespace
        let mod_id = reg.intern(BlockState::new("biomes_o_plenty:cherry_log").with_prop("axis", "y"));
        let modded = format_block_hud_info(reg.get_state(mod_id), reg.get_meta(mod_id), None, None);
        assert_eq!(modded.display_name, "Cherry Log");
        assert_eq!(modded.mod_name, "Biomes O Plenty");
        assert_eq!(modded.properties_line.as_deref(), Some("axis=y"));

        // 3. Block entity extras: command block & chest
        let mut cb = NbtCompound::new();
        cb.insert("Command", "/tp @a 0 64 0");
        assert_eq!(
            extract_block_entity_summary(&cb).as_deref(),
            Some("Command: /tp @a 0 64 0")
        );

        let mut chest = NbtCompound::new();
        chest.insert(
            "Items",
            NbtList::from_items(vec![NbtTag::Compound(NbtCompound::new())]).unwrap(),
        );
        assert_eq!(
            extract_block_entity_summary(&chest).as_deref(),
            Some("Container: 1 stack")
        );
    }
}
