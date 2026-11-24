use crate::editor::EditorState;
use ahash::AHashMap;
use egui::{
    Align2, Color32, ColorImage, Context, FontId, Pos2, Rect, Stroke, TextFormat, TextureHandle,
    TextureId, TextureOptions, Vec2 as EguiVec2, text::LayoutJob,
};
use glam::{IVec3, Mat4, Vec2, Vec3};
use mcedit_core::{BlockRegistry, BlockState, ChunkPos};
use mcedit_nbt::{NbtCompound, NbtTag};
use mcedit_render::{
    MeshBucket, SIGN_FONT_ATLAS_H, SIGN_FONT_ATLAS_W, SignPlacement, StyledSpan, TextStyle,
    TextureAtlas, build_sign_font_atlas_rgba, java_named_or_hex_color, obfuscated_ascii_for,
    parse_section_codes, push_sign_text_quads, sign_font_glyph_uv_exact, sign_font_solid_uv,
    spans_has_obfuscated,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SignDisplay {
    pub block_pos: IVec3,
    pub front_lines: Vec<Vec<StyledSpan>>,
    pub back_lines: Vec<Vec<StyledSpan>>,
    pub front_color: Color32,
    pub back_color: Color32,
    pub front_glowing: bool,
    pub back_glowing: bool,
    pub is_hanging: bool,
}

#[derive(Debug, Clone)]
pub struct EntityDisplay {
    pub pos: Vec3,
    pub slug: String,
    pub primary_name: Vec<StyledSpan>,
    pub subtitle: Option<String>,
    pub atlas_tile: Option<[u8; 2]>,
    /// True for invisible armor stands (Minecraft holographic floating nametags):
    /// rendered with only the name in crisp MC pixel font on a flat gray background.
    pub mc_nametag_only: bool,
}

#[derive(Clone, Default)]
struct EntityIconCache {
    pack_path: Option<PathBuf>,
    textures: Arc<parking_lot::RwLock<AHashMap<String, TextureHandle>>>,
}

impl EntityIconCache {
    fn get_or_build(
        &self,
        ctx: &Context,
        slug: &str,
        pack_path: Option<&Path>,
    ) -> TextureHandle {
        if let Some(h) = self.textures.read().get(slug) {
            return h.clone();
        }
        let color_img = if let Some(pp) = pack_path
            && let Some(rgba) = TextureAtlas::load_entity_face_from_resource_pack(pp, slug)
        {
            let size = [rgba.width() as usize, rgba.height() as usize];
            ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
        } else {
            build_procedural_entity_icon(slug)
        };
        let handle = ctx.load_texture(
            format!("entity_icon_{slug}"),
            color_img,
            TextureOptions::NEAREST,
        );
        self.textures
            .write()
            .insert(slug.to_string(), handle.clone());
        handle
    }
}

/// Builds the 3D world-space sign text quad mesh flush onto nearby sign faces.
/// Returns `(mesh, has_obfuscated)` so the caller can gently animate `§k` obfuscated text when visible.
pub fn build_sign_text_mesh(editor: &EditorState, obfuscate_tick: u64) -> (MeshBucket, bool) {
    let mut bucket = MeshBucket::default();
    let mut has_obfuscated = false;
    if !editor.config.show_sign_text || editor.jobs.is_busy() {
        return (bucket, false);
    }
    let Some(world) = &editor.world else {
        return (bucket, false);
    };
    let dim = world.active_dim;
    let cam_pos = editor.camera.pos;
    let cam_cp = ChunkPos::new(
        (cam_pos.x / 16.0).floor() as i32,
        (cam_pos.z / 16.0).floor() as i32,
    );
    let reg = editor.registry.read();

    for dz in -4..=4 {
        for dx in -4..=4 {
            let cp = ChunkPos::new(cam_cp.cx() + dx, cam_cp.cz() + dz);
            let Some(chunk) = world.get_loaded_chunk(dim, cp) else {
                continue;
            };
            for be in &chunk.block_entities {
                let Some(sign) = extract_sign_display(be) else {
                    continue;
                };
                let center_approx = sign.block_pos.as_vec3() + Vec3::splat(0.5);
                if cam_pos.distance_squared(center_approx) > 64.0 * 64.0 {
                    continue;
                }
                if sign.front_lines.iter().any(|l| spans_has_obfuscated(l))
                    || sign.back_lines.iter().any(|l| spans_has_obfuscated(l))
                {
                    has_obfuscated = true;
                }
                let lx = sign.block_pos.x.rem_euclid(16) as usize;
                let lz = sign.block_pos.z.rem_euclid(16) as usize;
                let bid = chunk.get_block(lx, sign.block_pos.y, lz);
                let state = reg.get_state(bid);
                let meta = reg.get_meta(bid);
                let mut placement = SignPlacement::from_block(sign.block_pos, state, meta);
                if sign.is_hanging && !placement.is_hanging {
                    placement.is_hanging = true;
                    placement.center = Vec3::new(
                        sign.block_pos.x as f32 + 0.5,
                        sign.block_pos.y as f32 + 0.34,
                        sign.block_pos.z as f32 + 0.5,
                    );
                    placement.width = 0.875;
                    placement.height = 0.625;
                    placement.thickness = 0.125;
                }
                let fc = sign.front_color;
                let bc = sign.back_color;
                push_sign_text_quads(
                    &mut bucket,
                    &placement,
                    &sign.front_lines,
                    &sign.back_lines,
                    [fc.r(), fc.g(), fc.b(), 255],
                    sign.front_glowing,
                    [bc.r(), bc.g(), bc.b(), 255],
                    sign.back_glowing,
                    obfuscate_tick,
                );
            }
        }
    }

    (bucket, has_obfuscated)
}

/// Renders floating 3D-projected Entity icon + name badges onto the background egui layer
/// (above 3D terrain, behind all menus & panels). Sign text is rendered directly in 3D on signs.
pub fn draw(ctx: &Context, editor: &EditorState, vp_size: Vec2, atlas_tex_id: TextureId) {
    if !editor.config.show_entity_labels || editor.world.is_none() || editor.jobs.is_busy() {
        return;
    }
    let Some(world) = &editor.world else {
        return;
    };
    if vp_size.x <= 1.0 || vp_size.y <= 1.0 {
        return;
    }

    let dim = world.active_dim;
    let vp = editor.camera.view_proj(vp_size.x, vp_size.y);
    let cam_pos = editor.camera.pos;
    let cam_cp = ChunkPos::new(
        (cam_pos.x / 16.0).floor() as i32,
        (cam_pos.z / 16.0).floor() as i32,
    );
    let screen_rect = ctx.content_rect();
    let reg = editor.registry.read();

    const MAX_ENTITY_DIST: f32 = 64.0;
    const MAX_ENTITIES: usize = 120;

    let mut entities: Vec<(f32, Pos2, EntityDisplay)> = Vec::new();

    // 1. Players in the active dimension
    for p in &world.players {
        if p.dimension == dim {
            let world_p = Vec3::new(p.pos.x as f32, p.pos.y as f32 + 2.15, p.pos.z as f32);
            let dist = cam_pos.distance(world_p);
            if dist <= MAX_ENTITY_DIST
                && let Some((screen_p, _)) = project_world_to_screen(vp, world_p, screen_rect)
            {
                let mut player_spans = Vec::new();
                parse_section_codes(&p.display_name, TextStyle::default(), &mut player_spans);
                entities.push((
                    dist,
                    screen_p,
                    EntityDisplay {
                        pos: world_p,
                        slug: "player".to_string(),
                        primary_name: player_spans,
                        subtitle: Some("Player".to_string()),
                        atlas_tile: None,
                        mc_nametag_only: false,
                    },
                ));
            }
        }
    }

    // 2. Scan loaded chunks in +/- 4 chunk radius around camera
    for dz in -4..=4 {
        for dx in -4..=4 {
            let cp = ChunkPos::new(cam_cp.cx() + dx, cam_cp.cz() + dz);
            let Some(chunk) = world.get_loaded_chunk(dim, cp) else {
                continue;
            };
            for ent in editor.chunk_entities_for_display(dim, cp, chunk) {
                if let Some(ed) = extract_entity_display(ent, &reg) {
                    let dist = cam_pos.distance(ed.pos);
                    if dist <= MAX_ENTITY_DIST
                        && let Some((screen_p, _)) =
                            project_world_to_screen(vp, ed.pos, screen_rect)
                    {
                        entities.push((dist, screen_p, ed));
                    }
                }
            }
        }
    }

    if entities.is_empty() {
        return;
    }

    // Keep nearest N, then sort back-to-front so closer badges draw on top
    entities.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    entities.truncate(MAX_ENTITIES);
    entities.reverse();

    // Fetch/invalidate entity icon cache if resource pack changed
    let cache_id = egui::Id::unique("world_labels_entity_icon_cache");
    let icon_cache = {
        let existing = ctx.data(|d| d.get_temp::<EntityIconCache>(cache_id));
        match existing {
            Some(c) if c.pack_path == editor.config.resource_pack_path => c,
            _ => {
                let fresh = EntityIconCache {
                    pack_path: editor.config.resource_pack_path.clone(),
                    textures: Arc::new(parking_lot::RwLock::new(AHashMap::new())),
                };
                ctx.data_mut(|d| d.insert_temp(cache_id, fresh.clone()));
                fresh
            }
        }
    };

    let pack_ref = editor.config.resource_pack_path.as_deref();
    let layer_id = egui::LayerId::new(egui::Order::Background, egui::Id::unique("world_labels"));
    let painter = ctx.layer_painter(layer_id);
    let obfuscate_tick = (ctx.input(|i| i.time) * 14.0) as u64;
    let mut any_obfuscated = false;

    // Lazy-init crisp NEAREST Minecraft bitmap font texture if any invisible armor stand is visible
    let mut mc_font_tex: Option<TextureHandle> = None;
    if entities.iter().any(|(_, _, e)| e.mc_nametag_only) {
        let font_id = egui::Id::unique("world_labels_mc_pixel_font_tex");
        let tex = ctx
            .data(|d| d.get_temp::<TextureHandle>(font_id))
            .unwrap_or_else(|| {
                let rgba = build_sign_font_atlas_rgba();
                let img = ColorImage::from_rgba_unmultiplied(
                    [SIGN_FONT_ATLAS_W as usize, SIGN_FONT_ATLAS_H as usize],
                    &rgba,
                );
                let handle = ctx.load_texture("mc_nametag_pixel_font", img, TextureOptions::NEAREST);
                ctx.data_mut(|d| d.insert_temp(font_id, handle.clone()));
                handle
            });
        mc_font_tex = Some(tex);
    }

    for (dist, screen_p, ent) in entities {
        if spans_has_obfuscated(&ent.primary_name) {
            any_obfuscated = true;
        }
        let fade = ((MAX_ENTITY_DIST - dist) / 12.0).clamp(0.0, 1.0);
        if ent.mc_nametag_only
            && let Some(font_tex) = mc_font_tex.as_ref()
        {
            draw_mc_nametag(&painter, screen_p, &ent, font_tex.id(), fade, obfuscate_tick);
            continue;
        }
        let icon = if ent.atlas_tile.is_none() {
            Some(icon_cache.get_or_build(ctx, &ent.slug, pack_ref))
        } else {
            None
        };
        draw_entity_badge(
            &painter,
            screen_p,
            &ent,
            icon.as_ref(),
            atlas_tex_id,
            fade,
            obfuscate_tick,
        );
    }
    if any_obfuscated {
        ctx.request_repaint_after(Duration::from_millis(75));
    }
}

fn project_world_to_screen(vp: Mat4, world_pos: Vec3, screen_rect: Rect) -> Option<(Pos2, f32)> {
    let clip = vp * world_pos.extend(1.0);
    if clip.w <= 0.05 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if ndc.x < -1.15
        || ndc.x > 1.15
        || ndc.y < -1.15
        || ndc.y > 1.15
        || ndc.z < 0.0
        || ndc.z > 1.0
    {
        return None;
    }
    let sx = screen_rect.min.x + (ndc.x * 0.5 + 0.5) * screen_rect.width();
    let sy = screen_rect.min.y + (0.5 - ndc.y * 0.5) * screen_rect.height();
    Some((egui::pos2(sx, sy), clip.w))
}

fn apply_alpha(c: Color32, factor: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * factor) as u8)
}

fn build_styled_layout_jobs(
    spans: &[StyledSpan],
    font_id: &FontId,
    default_color: Color32,
    fade: f32,
    obfuscate_tick: u64,
    salt: u64,
) -> (LayoutJob, Option<LayoutJob>) {
    let mut main_job = LayoutJob::default();
    let has_bold = spans.iter().any(|s| s.style.bold && !s.text.is_empty());
    let mut bold_job = has_bold.then(LayoutJob::default);
    let mut char_idx = 0u64;

    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        let base_col = span
            .style
            .color
            .map(|[r, g, b]| Color32::from_rgb(r, g, b))
            .unwrap_or(default_color);
        let col = apply_alpha(base_col, fade);
        let text: String = if span.style.obfuscated {
            span.text
                .chars()
                .map(|c| {
                    char_idx += 1;
                    if c.is_whitespace() {
                        c
                    } else {
                        let seed = salt
                            ^ char_idx.wrapping_mul(9173)
                            ^ obfuscate_tick.wrapping_mul(0x517C_C1B7_2722_0A95);
                        obfuscated_ascii_for(seed) as char
                    }
                })
                .collect()
        } else {
            char_idx += span.text.chars().count() as u64;
            span.text.clone()
        };

        let fmt = TextFormat {
            font_id: font_id.clone(),
            color: col,
            italics: span.style.italic,
            underline: if span.style.underlined {
                Stroke::new(1.0, col)
            } else {
                Stroke::NONE
            },
            strikethrough: if span.style.strikethrough {
                Stroke::new(1.0, col)
            } else {
                Stroke::NONE
            },
            ..Default::default()
        };
        main_job.append(&text, 0.0, fmt);

        if let Some(bj) = bold_job.as_mut() {
            let b_col = if span.style.bold {
                col
            } else {
                Color32::TRANSPARENT
            };
            bj.append(
                &text,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: b_col,
                    italics: span.style.italic,
                    ..Default::default()
                },
            );
        }
    }

    (main_job, bold_job)
}

/// Renders an authentic Minecraft-style floating nametag (sharp flat gray rectangle, no icon,
/// no subtitle, crisp NEAREST 5x7 Minecraft pixel font with 1px drop shadow and full Java `§` formatting).
fn draw_mc_nametag(
    painter: &egui::Painter,
    center: Pos2,
    ent: &EntityDisplay,
    font_tex_id: TextureId,
    fade: f32,
    obfuscate_tick: u64,
) {
    let mut styled_chars: Vec<(char, TextStyle)> = Vec::new();
    for span in &ent.primary_name {
        for ch in span.text.chars() {
            styled_chars.push((ch, span.style));
        }
    }
    if styled_chars.is_empty() {
        return;
    }

    let px = 2.0_f32; // crisp 2 logical px per bitmap pixel -> 10x14 glyph
    let gw = 5.0 * px;
    let gh = 7.0 * px;
    let base_adv = 6.0 * px;
    let bold_extra = px;

    let mut total_w = 0.0_f32;
    for &(_, st) in &styled_chars {
        total_w += base_adv + if st.bold { bold_extra } else { 0.0 };
    }
    // Remove trailing 1-pixel inter-character gap so padding is symmetric
    total_w = (total_w - px).max(gw);

    let pad_x = 4.0_f32;
    let pad_y = 2.5_f32;
    let box_size = EguiVec2::new(total_w + pad_x * 2.0, gh + pad_y * 2.0);
    let rect = Rect::from_center_size(
        egui::pos2(center.x.round(), (center.y - box_size.y * 0.5).round()),
        box_size,
    );

    // Classic Minecraft nametag background: sharp corners (0.0), translucent gray, no border
    let bg = apply_alpha(Color32::from_rgba_unmultiplied(32, 32, 32, 130), fade);
    painter.rect_filled(rect, 0.0, bg);

    let salt = (ent.pos.x.to_bits() as u64).wrapping_mul(31)
        ^ (ent.pos.y.to_bits() as u64).wrapping_mul(131)
        ^ (ent.pos.z.to_bits() as u64).wrapping_mul(8191);
    let solid_uv = sign_font_solid_uv();

    let mut mesh = egui::Mesh::with_texture(font_tex_id);
    let push_quad = |mesh: &mut egui::Mesh,
                     x0: f32,
                     x1: f32,
                     y0: f32,
                     y1: f32,
                     top_slant: f32,
                     uv: [f32; 4],
                     color: Color32| {
        let base = mesh.vertices.len() as u32;
        let [u0, v0, u1, v1] = uv;
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x0 + top_slant, y0),
            uv: egui::pos2(u0, v0),
            color,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x1 + top_slant, y0),
            uv: egui::pos2(u1, v0),
            color,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x1, y1),
            uv: egui::pos2(u1, v1),
            color,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x0, y1),
            uv: egui::pos2(u0, v1),
            color,
        });
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    let mut gx = rect.min.x + pad_x;
    let gy = rect.min.y + pad_y;

    for (ci, &(ch, style)) in styled_chars.iter().enumerate() {
        let adv = base_adv + if style.bold { bold_extra } else { 0.0 };
        let base_rgb = style.color.unwrap_or([255, 255, 255]);
        let col = apply_alpha(Color32::from_rgb(base_rgb[0], base_rgb[1], base_rgb[2]), fade);
        let shadow_col = apply_alpha(
            Color32::from_rgb(base_rgb[0] / 4, base_rgb[1] / 4, base_rgb[2] / 4),
            fade,
        );

        if style.underlined {
            let uy0 = gy + gh;
            let uy1 = uy0 + px;
            push_quad(&mut mesh, gx + px, gx + adv + px, uy0 + px, uy1 + px, 0.0, solid_uv, shadow_col);
            push_quad(&mut mesh, gx, gx + adv, uy0, uy1, 0.0, solid_uv, col);
        }

        if ch != ' ' {
            let code = if style.obfuscated {
                let seed = salt
                    ^ ((ci as u64).wrapping_mul(9173))
                    ^ obfuscate_tick.wrapping_mul(0x517C_C1B7_2722_0A95);
                obfuscated_ascii_for(seed)
            } else if (0x20..=0x7E).contains(&(ch as u32)) {
                ch as u8
            } else {
                b'?'
            };
            let uv = sign_font_glyph_uv_exact(code);
            let slant = if style.italic { px * 1.5 } else { 0.0 };

            // Drop shadow (+px, +px)
            push_quad(&mut mesh, gx + px, gx + gw + px, gy + px, gy + gh + px, slant, uv, shadow_col);
            if style.bold {
                push_quad(
                    &mut mesh,
                    gx + px * 2.0,
                    gx + gw + px * 2.0,
                    gy + px,
                    gy + gh + px,
                    slant,
                    uv,
                    shadow_col,
                );
            }
            // Main glyph
            push_quad(&mut mesh, gx, gx + gw, gy, gy + gh, slant, uv, col);
            if style.bold {
                push_quad(&mut mesh, gx + px, gx + gw + px, gy, gy + gh, slant, uv, col);
            }
        }

        if style.strikethrough {
            let sy0 = gy + (gh * 0.5 - px * 0.5).round();
            let sy1 = sy0 + px;
            push_quad(&mut mesh, gx + px, gx + adv + px, sy0 + px, sy1 + px, 0.0, solid_uv, shadow_col);
            push_quad(&mut mesh, gx, gx + adv, sy0, sy1, 0.0, solid_uv, col);
        }

        gx += adv;
    }

    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

fn draw_entity_badge(
    painter: &egui::Painter,
    center: Pos2,
    ent: &EntityDisplay,
    icon_handle: Option<&TextureHandle>,
    atlas_tex_id: TextureId,
    fade: f32,
    obfuscate_tick: u64,
) {
    let name_col = apply_alpha(Color32::from_rgb(245, 248, 255), fade);
    let sub_col = apply_alpha(Color32::from_rgb(175, 198, 225), fade);
    let salt = (ent.pos.x.to_bits() as u64).wrapping_mul(31)
        ^ (ent.pos.y.to_bits() as u64).wrapping_mul(131)
        ^ (ent.pos.z.to_bits() as u64).wrapping_mul(8191);

    let (name_job, bold_job) = build_styled_layout_jobs(
        &ent.primary_name,
        &FontId::proportional(11.5),
        Color32::from_rgb(245, 248, 255),
        fade,
        obfuscate_tick,
        salt,
    );
    let name_galley = painter.layout_job(name_job);
    let bold_galley = bold_job.map(|j| painter.layout_job(j));
    let sub_galley = ent
        .subtitle
        .as_ref()
        .map(|s| painter.layout_no_wrap(s.clone(), FontId::proportional(9.5), sub_col));

    let icon_sz = 18.0_f32;
    let gap = 5.0_f32;
    let pad_x = 5.0_f32;
    let pad_y = 3.0_f32;
    let text_w = name_galley
        .size()
        .x
        .max(sub_galley.as_ref().map_or(0.0, |g| g.size().x));
    let text_h =
        name_galley.size().y + sub_galley.as_ref().map_or(0.0, |g| g.size().y + 1.0);
    let content_h = icon_sz.max(text_h);
    let box_size = EguiVec2::new(pad_x * 2.0 + icon_sz + gap + text_w, pad_y * 2.0 + content_h);
    let rect =
        Rect::from_center_size(egui::pos2(center.x, center.y - box_size.y * 0.5 - 2.0), box_size);

    let bg = apply_alpha(Color32::from_rgba_unmultiplied(18, 22, 30, 205), fade);
    let border = apply_alpha(Color32::from_rgba_unmultiplied(90, 125, 175, 170), fade);
    painter.rect_filled(rect, 4.0, bg);
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, border), egui::StrokeKind::Inside);

    let icon_rect = Rect::from_min_size(
        egui::pos2(rect.min.x + pad_x, rect.center().y - icon_sz * 0.5),
        EguiVec2::splat(icon_sz),
    );
    let tint = apply_alpha(Color32::WHITE, fade);
    if let Some(tile) = ent.atlas_tile {
        let [u0, v0, u1, v1] = TextureAtlas::tile_uv(tile);
        painter.image(
            atlas_tex_id,
            icon_rect,
            Rect::from_min_max(egui::pos2(u0, v0), egui::pos2(u1, v1)),
            tint,
        );
    } else if let Some(handle) = icon_handle {
        painter.image(
            handle.id(),
            icon_rect,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            tint,
        );
    } else {
        painter.text(
            icon_rect.center(),
            Align2::CENTER_CENTER,
            "?",
            FontId::proportional(12.0),
            name_col,
        );
    }

    let text_x = icon_rect.max.x + gap;
    let text_y = rect.center().y - text_h * 0.5;
    let name_h = name_galley.size().y;
    painter.galley(egui::pos2(text_x, text_y), name_galley, name_col);
    if let Some(bg) = bold_galley {
        painter.galley(egui::pos2(text_x + 0.8, text_y), bg, name_col);
    }
    if let Some(sg) = sub_galley {
        painter.galley(egui::pos2(text_x, text_y + name_h + 1.0), sg, sub_col);
    }
}

/// Extracts visible sign lines across pre-1.20 (`Text1..Text4`), 1.20+ (`front_text`/`back_text`),
/// and 1.21.5+ NBT compound text components.
pub fn extract_sign_display(be: &NbtCompound) -> Option<SignDisplay> {
    let id = be.get_string("id")?;
    let id_lower = id.to_ascii_lowercase();
    if !id_lower.ends_with("sign") {
        return None;
    }
    let x = be.get_int("x")?;
    let y = be.get_int("y")?;
    let z = be.get_int("z")?;
    let is_hanging = id_lower.contains("hanging_sign");

    let mut front_lines: Vec<Vec<StyledSpan>> = Vec::new();
    let mut back_lines: Vec<Vec<StyledSpan>> = Vec::new();
    let mut front_color_name: Option<String> = None;
    let mut back_color_name: Option<String> = None;
    let front_glowing;
    let mut back_glowing = false;

    if let Some(front) = be.get_compound("front_text") {
        if let Some(msgs) = front.get_list("messages") {
            for item in msgs.items() {
                front_lines.push(extract_mc_styled_text(item));
            }
        }
        if let Some(c) = front.get_string("color") {
            front_color_name = Some(c.to_string());
        }
        front_glowing = front.get_bool("has_glowing_text").unwrap_or(false);

        if let Some(back) = be.get_compound("back_text") {
            if let Some(msgs) = back.get_list("messages") {
                for item in msgs.items() {
                    back_lines.push(extract_mc_styled_text(item));
                }
            }
            if let Some(c) = back.get_string("color") {
                back_color_name = Some(c.to_string());
            }
            back_glowing = back.get_bool("has_glowing_text").unwrap_or(false);
        }
    } else {
        for key in ["Text1", "Text2", "Text3", "Text4"] {
            if let Some(tag) = be.get(key) {
                front_lines.push(extract_mc_styled_text(tag));
            }
        }
        if let Some(c) = be.get_string("Color") {
            front_color_name = Some(c.to_string());
        }
        front_glowing = be.get_bool("GlowingText").unwrap_or(false);
    }

    // Trim trailing/leading empty lines; skip completely blank decorative signs
    let line_is_blank = |spans: &[StyledSpan]| spans.iter().all(|s| s.text.trim().is_empty());
    let trim_lines = |lines: Vec<Vec<StyledSpan>>| -> Vec<Vec<StyledSpan>> {
        let mut out: Vec<Vec<StyledSpan>> = lines
            .into_iter()
            .map(|mut spans| {
                while let Some(last) = spans.last_mut() {
                    let trimmed_len = last.text.trim_end().len();
                    last.text.truncate(trimmed_len);
                    if last.text.is_empty() {
                        spans.pop();
                    } else {
                        break;
                    }
                }
                spans
            })
            .collect();
        while out.last().is_some_and(|l| line_is_blank(l)) {
            out.pop();
        }
        while out.first().is_some_and(|l| line_is_blank(l)) {
            out.remove(0);
        }
        out
    };
    let front_lines = trim_lines(front_lines);
    let back_lines = trim_lines(back_lines);
    if front_lines.is_empty() && back_lines.is_empty() {
        return None;
    }

    let front_color = mc_dye_color(front_color_name.as_deref().unwrap_or("black"));
    let back_color = mc_dye_color(back_color_name.as_deref().unwrap_or("black"));
    Some(SignDisplay {
        block_pos: IVec3::new(x, y, z),
        front_lines,
        back_lines,
        front_color,
        back_color,
        front_glowing,
        back_glowing,
        is_hanging,
    })
}

/// Extracts display name, icon slug, optional subtitle, and world position from an entity NBT compound.
pub fn extract_entity_display(
    ent: &NbtCompound,
    reg: &BlockRegistry,
) -> Option<EntityDisplay> {
    let pos_list = ent.get_list("Pos")?;
    if pos_list.len() < 3 {
        return None;
    }
    let mut coords = [0.0_f32; 3];
    for (i, tag) in pos_list.items().iter().take(3).enumerate() {
        coords[i] = match tag {
            NbtTag::Double(v) => *v as f32,
            NbtTag::Float(v) => *v,
            _ => return None,
        };
    }

    let raw_id = ent.get_string("id").unwrap_or("unknown");
    let slug = normalize_entity_slug(raw_id);
    let pretty_type = prettify_slug(&slug);

    let custom_name = ent
        .get("CustomName")
        .map(extract_mc_styled_text)
        .filter(|spans| spans.iter().any(|s| !s.text.trim().is_empty()));

    let mut atlas_tile: Option<[u8; 2]> = None;
    let mut text_display_spans: Option<Vec<StyledSpan>> = None;
    let extra_detail = match slug.as_str() {
        "item" => ent.get_compound("Item").and_then(|item| {
            let item_id = item.get_string("id")?;
            let item_slug = item_id.strip_prefix("minecraft:").unwrap_or(item_id);
            atlas_tile = block_tile_for_id(item_id, reg);
            let cnt = item
                .get_byte("Count")
                .map(|b| b as i32)
                .or_else(|| item.get_int("count"))
                .unwrap_or(1);
            if cnt > 1 {
                Some(format!("{} x{}", prettify_slug(item_slug), cnt))
            } else {
                Some(prettify_slug(item_slug))
            }
        }),
        "item_frame" | "glow_item_frame" => ent.get_compound("Item").and_then(|item| {
            let item_id = item.get_string("id")?;
            let item_slug = item_id.strip_prefix("minecraft:").unwrap_or(item_id);
            atlas_tile = block_tile_for_id(item_id, reg);
            Some(prettify_slug(item_slug))
        }),
        "painting" => ent
            .get_string("variant")
            .or_else(|| ent.get_string("Motive"))
            .map(|m| prettify_slug(m.strip_prefix("minecraft:").unwrap_or(m))),
        "villager" => ent
            .get_compound("VillagerData")
            .and_then(|vd| vd.get_string("profession"))
            .map(|p| p.strip_prefix("minecraft:").unwrap_or(p))
            .filter(|&p| p != "none")
            .map(prettify_slug),
        "text_display" => {
            if let Some(tag) = ent.get("text") {
                let spans = extract_mc_styled_text(tag);
                if spans.iter().any(|s| !s.text.trim().is_empty()) {
                    text_display_spans = Some(spans);
                }
            }
            None
        }
        "block_display" => ent
            .get_compound("block_state")
            .and_then(|bs| bs.get_string("Name"))
            .map(|name| {
                atlas_tile = block_tile_for_id(name, reg);
                prettify_slug(name.strip_prefix("minecraft:").unwrap_or(name))
            }),
        "item_display" => ent
            .get_compound("item")
            .and_then(|it| it.get_string("id"))
            .map(|name| {
                atlas_tile = block_tile_for_id(name, reg);
                prettify_slug(name.strip_prefix("minecraft:").unwrap_or(name))
            }),
        "falling_block" => ent
            .get_compound("BlockState")
            .and_then(|bs| bs.get_string("Name"))
            .or_else(|| ent.get_string("Block"))
            .map(|name| {
                atlas_tile = block_tile_for_id(name, reg);
                prettify_slug(name.strip_prefix("minecraft:").unwrap_or(name))
            }),
        _ => None,
    };

    let is_invisible_armor_stand =
        slug == "armor_stand" && ent.get_bool("Invisible").unwrap_or(false);

    let (primary_name, subtitle) = if is_invisible_armor_stand {
        let name = custom_name.unwrap_or_else(|| vec![StyledSpan::plain(pretty_type)]);
        (name, None)
    } else if let Some(cn) = custom_name {
        let sub = match extra_detail {
            Some(d) => format!("{pretty_type} • {d}"),
            None => pretty_type,
        };
        (cn, Some(sub))
    } else if slug == "text_display"
        && let Some(txt) = text_display_spans
    {
        (txt, Some("Text Display".to_string()))
    } else {
        (vec![StyledSpan::plain(pretty_type)], extra_detail)
    };

    let y_offset = match slug.as_str() {
        "armor_stand" => {
            if ent.get_bool("Marker").unwrap_or(false) {
                0.25
            } else if ent.get_bool("Small").unwrap_or(false) {
                1.15
            } else {
                2.05
            }
        }
        "item" | "experience_orb" | "arrow" => 0.65,
        "iron_golem" | "enderman" | "warden" => 2.75,
        "villager" | "zombie" | "skeleton" | "creeper" | "witch" | "piglin"
        | "zombified_piglin" => 2.05,
        _ => 1.35,
    };

    Some(EntityDisplay {
        pos: Vec3::new(coords[0], coords[1] + y_offset, coords[2]),
        slug,
        primary_name,
        subtitle,
        atlas_tile,
        mc_nametag_only: is_invisible_armor_stand,
    })
}

fn block_tile_for_id(id: &str, reg: &BlockRegistry) -> Option<[u8; 2]> {
    let bs = BlockState::new(id);
    let bid = reg.resolve_readonly(&bs)?;
    if bid == mcedit_core::BlockStateId::AIR {
        return None;
    }
    Some(reg.get_meta(bid).face_tiles[mcedit_core::Face::North.index()])
}

/// Recursively extracts Minecraft Java Edition styled spans from text components across:
/// - Plain strings with `§` Java formatting codes (pre-1.8 and modern embedded codes)
/// - JSON text components (`{"text": "...", "color": "...", "bold": true, "extra": [...]}`) (1.8–1.21.4)
/// - Native NBT compound text components (`{text: "...", color: "...", extra: [...]}`) (1.21.5+)
pub fn extract_mc_styled_text(tag: &NbtTag) -> Vec<StyledSpan> {
    let mut out = Vec::new();
    collect_nbt_styled(tag, TextStyle::default(), &mut out);
    out
}

fn collect_nbt_styled(tag: &NbtTag, inherited: TextStyle, out: &mut Vec<StyledSpan>) {
    match tag {
        NbtTag::String(s) => collect_json_or_plain_styled(s, inherited, out),
        NbtTag::Compound(c) => {
            let style = merge_nbt_style(c, inherited);
            if let Some(t) = c.get_string("text") {
                parse_section_codes(t, style, out);
            } else if let Some(t) = c.get_string("") {
                parse_section_codes(t, style, out);
            } else if let Some(tr) = c.get_string("translate") {
                parse_section_codes(tr, style, out);
            }
            if let Some(extra) = c.get_list("extra") {
                for item in extra.items() {
                    collect_nbt_styled(item, style, out);
                }
            }
        }
        NbtTag::List(list) => {
            let mut elem_base = inherited;
            for (i, item) in list.items().iter().enumerate() {
                if i == 0
                    && let NbtTag::Compound(c) = item
                {
                    elem_base = merge_nbt_style(c, inherited);
                }
                collect_nbt_styled(item, elem_base, out);
            }
        }
        _ => {}
    }
}

fn merge_nbt_style(c: &NbtCompound, mut style: TextStyle) -> TextStyle {
    if let Some(col_name) = c.get_string("color")
        && let Some(rgb) = java_named_or_hex_color(col_name)
    {
        style.color = Some(rgb);
    }
    if let Some(b) = c.get_bool("bold") {
        style.bold = b;
    }
    if let Some(b) = c.get_bool("italic") {
        style.italic = b;
    }
    if let Some(b) = c.get_bool("underlined") {
        style.underlined = b;
    }
    if let Some(b) = c.get_bool("strikethrough") {
        style.strikethrough = b;
    }
    if let Some(b) = c.get_bool("obfuscated") {
        style.obfuscated = b;
    }
    style
}

fn collect_json_or_plain_styled(raw: &str, inherited: TextStyle, out: &mut Vec<StyledSpan>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "null" || trimmed == "\"\"" {
        return;
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.starts_with('"'))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        collect_json_styled(&val, inherited, out);
        return;
    }
    parse_section_codes(trimmed, inherited, out);
}

fn merge_json_style(map: &serde_json::Map<String, serde_json::Value>, mut style: TextStyle) -> TextStyle {
    if let Some(col_name) = map.get("color").and_then(|v| v.as_str())
        && let Some(rgb) = java_named_or_hex_color(col_name)
    {
        style.color = Some(rgb);
    }
    if let Some(b) = map.get("bold").and_then(|v| v.as_bool()) {
        style.bold = b;
    }
    if let Some(b) = map.get("italic").and_then(|v| v.as_bool()) {
        style.italic = b;
    }
    if let Some(b) = map.get("underlined").and_then(|v| v.as_bool()) {
        style.underlined = b;
    }
    if let Some(b) = map.get("strikethrough").and_then(|v| v.as_bool()) {
        style.strikethrough = b;
    }
    if let Some(b) = map.get("obfuscated").and_then(|v| v.as_bool()) {
        style.obfuscated = b;
    }
    style
}

fn collect_json_styled(val: &serde_json::Value, inherited: TextStyle, out: &mut Vec<StyledSpan>) {
    match val {
        serde_json::Value::String(s) => parse_section_codes(s, inherited, out),
        serde_json::Value::Array(arr) => {
            let mut elem_base = inherited;
            for (i, v) in arr.iter().enumerate() {
                if i == 0
                    && let serde_json::Value::Object(map) = v
                {
                    elem_base = merge_json_style(map, inherited);
                }
                collect_json_styled(v, elem_base, out);
            }
        }
        serde_json::Value::Object(map) => {
            let style = merge_json_style(map, inherited);
            if let Some(t) = map.get("text").and_then(|v| v.as_str()) {
                parse_section_codes(t, style, out);
            } else if let Some(tr) = map.get("translate").and_then(|v| v.as_str()) {
                parse_section_codes(tr, style, out);
            }
            if let Some(extra) = map.get("extra") {
                collect_json_styled(extra, style, out);
            }
        }
        serde_json::Value::Number(n) => parse_section_codes(&n.to_string(), inherited, out),
        serde_json::Value::Bool(b) => {
            parse_section_codes(if *b { "true" } else { "false" }, inherited, out)
        }
        serde_json::Value::Null => {}
    }
}

fn mc_dye_color(name: &str) -> Color32 {
    match name.to_ascii_lowercase().as_str() {
        "white" => Color32::from_rgb(250, 250, 250),
        "orange" => Color32::from_rgb(245, 130, 35),
        "magenta" => Color32::from_rgb(205, 85, 200),
        "light_blue" | "aqua" => Color32::from_rgb(80, 190, 240),
        "yellow" => Color32::from_rgb(250, 225, 60),
        "lime" => Color32::from_rgb(120, 215, 45),
        "pink" => Color32::from_rgb(245, 145, 175),
        "gray" | "dark_gray" => Color32::from_rgb(75, 80, 88),
        "light_gray" | "silver" => Color32::from_rgb(165, 165, 165),
        "cyan" | "dark_aqua" => Color32::from_rgb(25, 145, 155),
        "purple" | "dark_purple" => Color32::from_rgb(140, 55, 185),
        "blue" | "dark_blue" => Color32::from_rgb(48, 80, 195),
        "brown" => Color32::from_rgb(115, 70, 40),
        "green" | "dark_green" => Color32::from_rgb(65, 125, 35),
        "red" | "dark_red" => Color32::from_rgb(195, 40, 40),
        "gold" => Color32::from_rgb(235, 175, 35),
        _ => Color32::from_rgb(24, 18, 12), // dark ink on oak sign
    }
}

fn normalize_entity_slug(raw: &str) -> String {
    let stripped = raw.strip_prefix("minecraft:").unwrap_or(raw);
    match stripped {
        "PigZombie" => "zombified_piglin".to_string(),
        "MinecartRideable" => "minecart".to_string(),
        "MinecartChest" => "chest_minecart".to_string(),
        "MinecartHopper" => "hopper_minecart".to_string(),
        "MinecartTNT" => "tnt_minecart".to_string(),
        "MinecartFurnace" => "furnace_minecart".to_string(),
        "PrimedTnt" => "tnt".to_string(),
        "XPOrb" => "experience_orb".to_string(),
        "VillagerGolem" => "iron_golem".to_string(),
        "SnowMan" => "snow_golem".to_string(),
        "Ozelot" => "ocelot".to_string(),
        "EntityHorse" => "horse".to_string(),
        "LavaSlime" => "magma_cube".to_string(),
        "MushroomCow" => "mooshroom".to_string(),
        "FallingSand" => "falling_block".to_string(),
        "EnderCrystal" => "end_crystal".to_string(),
        other => {
            // Convert legacy CamelCase (e.g. "ArmorStand", "ItemFrame", "CaveSpider") -> snake_case
            let mut out = String::with_capacity(other.len() + 4);
            for (i, ch) in other.chars().enumerate() {
                if ch.is_ascii_uppercase() {
                    if i > 0 && !out.ends_with('_') {
                        out.push('_');
                    }
                    out.push(ch.to_ascii_lowercase());
                } else {
                    out.push(ch);
                }
            }
            out
        }
    }
}

fn prettify_slug(slug: &str) -> String {
    slug.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.push_str(chars.as_str());
                    s
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Generates crisp 8x8 pixel-art Minecraft-style face/entity icons rendered with NEAREST filtering.
fn build_procedural_entity_icon(slug: &str) -> ColorImage {
    let p = |r: u8, g: u8, b: u8| Color32::from_rgb(r, g, b);
    let mut px = [Color32::TRANSPARENT; 64];

    let fill = |px: &mut [Color32; 64], c: Color32| {
        px.fill(c);
    };
    let set = |px: &mut [Color32; 64], x: usize, y: usize, c: Color32| {
        if x < 8 && y < 8 {
            px[y * 8 + x] = c;
        }
    };
    let rect = |px: &mut [Color32; 64], x0: usize, y0: usize, w: usize, h: usize, c: Color32| {
        for y in y0..(y0 + h).min(8) {
            for x in x0..(x0 + w).min(8) {
                px[y * 8 + x] = c;
            }
        }
    };

    match slug {
        "creeper" => {
            fill(&mut px, p(78, 175, 72));
            for i in [0, 3, 6, 9, 14, 21, 31, 40, 48, 55, 60] {
                px[i] = p(60, 148, 56);
            }
            rect(&mut px, 1, 2, 2, 2, p(18, 22, 18));
            rect(&mut px, 5, 2, 2, 2, p(18, 22, 18));
            rect(&mut px, 3, 4, 2, 2, p(18, 22, 18));
            rect(&mut px, 2, 5, 4, 2, p(18, 22, 18));
            set(&mut px, 2, 7, p(18, 22, 18));
            set(&mut px, 5, 7, p(18, 22, 18));
        }
        "zombie" | "husk" | "drowned" => {
            let (skin, dark, eye) = match slug {
                "husk" => (p(178, 156, 114), p(145, 124, 86), p(36, 30, 22)),
                "drowned" => (p(116, 185, 170), p(70, 125, 116), p(110, 245, 235)),
                _ => (p(84, 134, 72), p(62, 102, 52), p(20, 28, 20)),
            };
            fill(&mut px, skin);
            rect(&mut px, 0, 0, 8, 2, dark);
            rect(&mut px, 1, 3, 2, 1, eye);
            rect(&mut px, 5, 3, 2, 1, eye);
            rect(&mut px, 3, 5, 2, 1, dark);
            rect(&mut px, 2, 6, 4, 1, dark);
        }
        "skeleton" | "stray" | "wither_skeleton" => {
            let (bone, shade, eye) = match slug {
                "wither_skeleton" => (p(52, 52, 56), p(36, 36, 40), p(18, 18, 20)),
                "stray" => (p(155, 175, 182), p(115, 135, 145), p(215, 248, 255)),
                _ => (p(212, 212, 206), p(175, 175, 168), p(38, 38, 42)),
            };
            fill(&mut px, bone);
            rect(&mut px, 0, 7, 8, 1, shade);
            rect(&mut px, 1, 3, 2, 1, eye);
            rect(&mut px, 5, 3, 2, 1, eye);
            rect(&mut px, 3, 4, 2, 1, shade);
            rect(&mut px, 1, 6, 6, 1, eye);
        }
        "enderman" => {
            fill(&mut px, p(22, 22, 26));
            rect(&mut px, 0, 3, 3, 1, p(225, 125, 255));
            set(&mut px, 1, 3, p(255, 205, 255));
            rect(&mut px, 5, 3, 3, 1, p(225, 125, 255));
            set(&mut px, 6, 3, p(255, 205, 255));
        }
        "spider" | "cave_spider" => {
            let body = if slug == "cave_spider" {
                p(24, 52, 56)
            } else {
                p(46, 40, 38)
            };
            fill(&mut px, body);
            rect(&mut px, 0, 2, 2, 1, p(220, 28, 28));
            rect(&mut px, 6, 2, 2, 1, p(220, 28, 28));
            rect(&mut px, 2, 3, 1, 2, p(240, 42, 42));
            rect(&mut px, 5, 3, 1, 2, p(240, 42, 42));
            rect(&mut px, 2, 6, 4, 1, p(20, 18, 18));
        }
        "villager" | "wandering_trader" | "witch" | "pillager" | "vindicator" | "evoker" => {
            let (skin, brow, eye) = match slug {
                "pillager" | "vindicator" | "evoker" => (p(148, 156, 156), p(56, 54, 52), p(62, 125, 92)),
                "witch" => (p(185, 148, 124), p(64, 42, 92), p(120, 48, 165)),
                _ => (p(189, 139, 108), p(96, 62, 42), p(45, 138, 62)),
            };
            fill(&mut px, skin);
            rect(&mut px, 0, 2, 8, 1, brow);
            set(&mut px, 1, 3, p(245, 245, 245));
            set(&mut px, 2, 3, eye);
            set(&mut px, 5, 3, eye);
            set(&mut px, 6, 3, p(245, 245, 245));
            rect(&mut px, 3, 4, 2, 3, p(165, 114, 86)); // iconic nose
        }
        "pig" | "piglin" | "zombified_piglin" => {
            let base = if slug == "zombified_piglin" {
                p(215, 150, 152)
            } else {
                p(240, 160, 164)
            };
            fill(&mut px, base);
            if slug == "zombified_piglin" {
                rect(&mut px, 4, 0, 4, 4, p(94, 145, 76));
            }
            set(&mut px, 0, 3, p(250, 250, 250));
            set(&mut px, 1, 3, p(30, 26, 26));
            set(&mut px, 6, 3, p(30, 26, 26));
            set(&mut px, 7, 3, p(250, 250, 250));
            rect(&mut px, 2, 4, 4, 3, p(228, 134, 142));
            set(&mut px, 2, 5, p(120, 56, 60));
            set(&mut px, 5, 5, p(120, 56, 60));
        }
        "cow" | "mooshroom" => {
            let base = if slug == "mooshroom" {
                p(168, 32, 32)
            } else {
                p(76, 56, 42)
            };
            fill(&mut px, base);
            rect(&mut px, 0, 0, 1, 2, p(175, 170, 162)); // horns
            rect(&mut px, 7, 0, 1, 2, p(175, 170, 162));
            rect(&mut px, 2, 0, 3, 2, p(225, 220, 215));
            set(&mut px, 1, 3, p(22, 18, 18));
            set(&mut px, 6, 3, p(22, 18, 18));
            rect(&mut px, 1, 5, 6, 3, p(185, 176, 170));
            set(&mut px, 2, 6, p(55, 48, 46));
            set(&mut px, 5, 6, p(55, 48, 46));
        }
        "sheep" => {
            fill(&mut px, p(238, 236, 232));
            rect(&mut px, 1, 2, 6, 5, p(224, 182, 158));
            set(&mut px, 1, 3, p(250, 250, 250));
            set(&mut px, 2, 3, p(28, 24, 24));
            set(&mut px, 5, 3, p(28, 24, 24));
            set(&mut px, 6, 3, p(250, 250, 250));
            rect(&mut px, 3, 5, 2, 2, p(235, 160, 160));
        }
        "chicken" => {
            fill(&mut px, p(245, 245, 242));
            set(&mut px, 1, 2, p(24, 24, 24));
            set(&mut px, 6, 2, p(24, 24, 24));
            rect(&mut px, 2, 4, 4, 2, p(228, 178, 56)); // beak
            rect(&mut px, 3, 6, 2, 2, p(210, 42, 42)); // wattle
        }
        "wolf" | "fox" | "cat" | "ocelot" => {
            let (fur, nose) = match slug {
                "fox" => (p(225, 118, 36), p(245, 240, 235)),
                "cat" | "ocelot" => (p(235, 185, 86), p(240, 150, 155)),
                _ => (p(205, 202, 198), p(165, 160, 155)),
            };
            fill(&mut px, fur);
            set(&mut px, 1, 3, p(250, 250, 250));
            set(&mut px, 2, 3, p(30, 26, 24));
            set(&mut px, 5, 3, p(30, 26, 24));
            set(&mut px, 6, 3, p(250, 250, 250));
            rect(&mut px, 2, 5, 4, 3, nose);
            rect(&mut px, 3, 5, 2, 1, p(30, 26, 26));
        }
        "bee" => {
            fill(&mut px, p(242, 196, 48));
            rect(&mut px, 0, 0, 8, 1, p(58, 42, 24));
            rect(&mut px, 1, 2, 2, 3, p(28, 24, 24));
            rect(&mut px, 5, 2, 2, 3, p(28, 24, 24));
            set(&mut px, 1, 2, p(120, 225, 245));
            set(&mut px, 6, 2, p(120, 225, 245));
        }
        "slime" | "magma_cube" => {
            let (bg, eye) = if slug == "magma_cube" {
                (p(64, 24, 20), p(255, 185, 35))
            } else {
                (p(114, 198, 92), p(38, 72, 32))
            };
            fill(&mut px, bg);
            rect(&mut px, 1, 2, 2, 2, eye);
            rect(&mut px, 5, 2, 2, 2, eye);
            set(&mut px, 3, 5, eye);
        }
        "ghast" => {
            fill(&mut px, p(242, 242, 242));
            rect(&mut px, 1, 3, 2, 1, p(48, 48, 52));
            rect(&mut px, 5, 3, 2, 1, p(48, 48, 52));
            rect(&mut px, 3, 5, 2, 2, p(138, 48, 48));
        }
        "blaze" => {
            fill(&mut px, p(240, 178, 36));
            rect(&mut px, 0, 0, 8, 2, p(215, 128, 24));
            rect(&mut px, 1, 3, 2, 1, p(40, 22, 12));
            rect(&mut px, 5, 3, 2, 1, p(40, 22, 12));
            set(&mut px, 2, 3, p(255, 245, 180));
            set(&mut px, 5, 3, p(255, 245, 180));
        }
        "iron_golem" | "snow_golem" => {
            let (bg, eye) = if slug == "snow_golem" {
                (p(220, 125, 32), p(38, 22, 10))
            } else {
                (p(208, 202, 195), p(185, 42, 42))
            };
            fill(&mut px, bg);
            rect(&mut px, 1, 3, 2, 1, eye);
            rect(&mut px, 5, 3, 2, 1, eye);
            rect(&mut px, 3, 4, 2, 3, p(165, 158, 150));
        }
        "player" => {
            fill(&mut px, p(198, 152, 122));
            rect(&mut px, 0, 0, 8, 2, p(68, 46, 30)); // hair
            set(&mut px, 0, 2, p(68, 46, 30));
            set(&mut px, 7, 2, p(68, 46, 30));
            set(&mut px, 1, 3, p(248, 248, 248));
            set(&mut px, 2, 3, p(78, 70, 158));
            set(&mut px, 5, 3, p(78, 70, 158));
            set(&mut px, 6, 3, p(248, 248, 248));
            rect(&mut px, 2, 6, 4, 1, p(118, 72, 54));
        }
        "armor_stand" => {
            fill(&mut px, p(45, 48, 56));
            rect(&mut px, 3, 1, 2, 6, p(196, 158, 104));
            rect(&mut px, 1, 3, 6, 1, p(196, 158, 104));
            rect(&mut px, 1, 7, 6, 1, p(155, 155, 160));
        }
        "minecart" | "chest_minecart" | "hopper_minecart" | "tnt_minecart" | "furnace_minecart" => {
            fill(&mut px, p(38, 42, 50));
            rect(&mut px, 1, 3, 6, 4, p(138, 144, 152));
            rect(&mut px, 2, 4, 4, 2, p(72, 76, 84));
            if slug == "tnt_minecart" {
                rect(&mut px, 2, 2, 4, 2, p(215, 48, 42));
            } else if slug == "chest_minecart" {
                rect(&mut px, 2, 2, 4, 2, p(188, 134, 58));
            }
        }
        "boat" | "chest_boat" => {
            fill(&mut px, p(42, 92, 155));
            rect(&mut px, 1, 4, 6, 3, p(168, 126, 74));
            rect(&mut px, 2, 3, 4, 1, p(122, 88, 48));
        }
        "item_frame" | "glow_item_frame" => {
            let rim = if slug == "glow_item_frame" {
                p(95, 235, 225)
            } else {
                p(178, 134, 82)
            };
            fill(&mut px, rim);
            rect(&mut px, 1, 1, 6, 6, p(132, 76, 48));
            rect(&mut px, 3, 3, 2, 2, p(235, 215, 120));
        }
        "painting" => {
            fill(&mut px, p(165, 122, 72));
            rect(&mut px, 1, 1, 6, 3, p(105, 175, 235));
            rect(&mut px, 1, 4, 6, 3, p(88, 165, 76));
            rect(&mut px, 5, 2, 1, 1, p(250, 230, 85));
        }
        "item" => {
            fill(&mut px, p(42, 48, 60));
            rect(&mut px, 2, 2, 4, 4, p(86, 220, 235));
            rect(&mut px, 3, 3, 2, 2, p(195, 250, 255));
        }
        "experience_orb" => {
            fill(&mut px, p(28, 40, 28));
            rect(&mut px, 2, 2, 4, 4, p(145, 245, 68));
            rect(&mut px, 3, 3, 2, 2, p(245, 255, 120));
        }
        "tnt" => {
            fill(&mut px, p(215, 48, 38));
            rect(&mut px, 0, 3, 8, 2, p(242, 242, 242));
            set(&mut px, 1, 3, p(30, 30, 30));
            set(&mut px, 4, 3, p(30, 30, 30));
            set(&mut px, 6, 3, p(30, 30, 30));
        }
        "text_display" | "block_display" | "item_display" => {
            fill(&mut px, p(48, 64, 96));
            rect(&mut px, 1, 2, 6, 1, p(220, 235, 255));
            rect(&mut px, 2, 4, 4, 1, p(170, 205, 255));
            rect(&mut px, 1, 6, 5, 1, p(170, 205, 255));
        }
        _ => {
            // Deterministic color-coded mob badge for any other / modded entity
            let mut hash: u32 = 2166136261;
            for b in slug.bytes() {
                hash ^= b as u32;
                hash = hash.wrapping_mul(16777619);
            }
            let r = 90 + ((hash & 0x7F) as u8);
            let g = 90 + (((hash >> 8) & 0x7F) as u8);
            let b = 90 + (((hash >> 16) & 0x7F) as u8);
            fill(&mut px, p(r, g, b));
            let dark = p(r / 2, g / 2, b / 2);
            rect(&mut px, 0, 0, 8, 1, dark);
            rect(&mut px, 0, 7, 8, 1, dark);
            set(&mut px, 2, 3, p(24, 24, 28));
            set(&mut px, 5, 3, p(24, 24, 28));
            rect(&mut px, 3, 5, 2, 1, dark);
        }
    }

    ColorImage {
        size: [8, 8],
        source_size: EguiVec2::new(8.0, 8.0),
        pixels: px.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcedit_nbt::NbtList;
    use mcedit_render::spans_plain_text;

    #[test]
    fn extracts_pre120_and_post120_sign_text() {
        // 1. Pre-1.20 JSON Text1..Text4 sign
        let mut old_sign = NbtCompound::new();
        old_sign.insert("id", "minecraft:sign");
        old_sign.insert("x", 10i32);
        old_sign.insert("y", 64i32);
        old_sign.insert("z", -5i32);
        old_sign.insert("Text1", r#"{"text":"Welcome"}"#);
        old_sign.insert("Text2", r#"{"extra":[{"text":"§aBase"}],"text":"To "}"#);
        old_sign.insert("Text3", r#"{"text":""}"#);
        old_sign.insert("Text4", r#"{"text":""}"#);

        let parsed_old = extract_sign_display(&old_sign).expect("old sign must parse");
        let old_plain: Vec<String> = parsed_old
            .front_lines
            .iter()
            .map(|l| spans_plain_text(l))
            .collect();
        assert_eq!(old_plain, vec!["Welcome", "To Base"]);
        // Second line has "To " (default) + "Base" (§a green #55FF55)
        assert_eq!(parsed_old.front_lines[1].len(), 2);
        assert_eq!(parsed_old.front_lines[1][0].style.color, None);
        assert_eq!(parsed_old.front_lines[1][1].text, "Base");
        assert_eq!(parsed_old.front_lines[1][1].style.color, Some([0x55, 0xFF, 0x55]));
        assert!(parsed_old.back_lines.is_empty());

        // 2. 1.20+/1.21.5+ front_text + back_text hanging sign (homogeneous Compound list)
        let mut msg1 = NbtCompound::new();
        msg1.insert("", "§6§lFront Header");
        let mut msg2 = NbtCompound::new();
        msg2.insert("text", "Second Line");
        msg2.insert("color", "#FF8800");
        msg2.insert("italic", 1i8);
        let mut front = NbtCompound::new();
        front.insert(
            "messages",
            NbtList::from_items(vec![
                NbtTag::Compound(msg1),
                NbtTag::Compound(msg2),
                NbtTag::Compound(NbtCompound::new()),
                NbtTag::Compound(NbtCompound::new()),
            ])
            .unwrap(),
        );
        front.insert("color", "white");
        front.insert("has_glowing_text", 1i8);

        let mut back = NbtCompound::new();
        back.insert(
            "messages",
            NbtList::from_items(vec![
                NbtTag::String("§b§nBack Note".to_string()),
                NbtTag::String(String::new()),
            ])
            .unwrap(),
        );

        let mut new_sign = NbtCompound::new();
        new_sign.insert("id", "minecraft:hanging_sign");
        new_sign.insert("x", 0i32);
        new_sign.insert("y", 70i32);
        new_sign.insert("z", 0i32);
        new_sign.insert("front_text", front);
        new_sign.insert("back_text", back);

        let parsed_new = extract_sign_display(&new_sign).expect("1.20+ sign must parse");
        assert!(parsed_new.is_hanging);
        assert!(parsed_new.front_glowing);
        let new_front_plain: Vec<String> = parsed_new
            .front_lines
            .iter()
            .map(|l| spans_plain_text(l))
            .collect();
        let new_back_plain: Vec<String> = parsed_new
            .back_lines
            .iter()
            .map(|l| spans_plain_text(l))
            .collect();
        assert_eq!(new_front_plain, vec!["Front Header", "Second Line"]);
        assert_eq!(parsed_new.front_lines[0][0].style.color, Some([0xFF, 0xAA, 0x00]));
        assert!(parsed_new.front_lines[0][0].style.bold);
        assert_eq!(parsed_new.front_lines[1][0].style.color, Some([0xFF, 0x88, 0x00]));
        assert!(parsed_new.front_lines[1][0].style.italic);
        assert_eq!(new_back_plain, vec!["Back Note"]);
        assert_eq!(parsed_new.back_lines[0][0].style.color, Some([0x55, 0xFF, 0xFF]));
        assert!(parsed_new.back_lines[0][0].style.underlined);
    }

    #[test]
    fn extracts_entity_custom_names_and_item_details() {
        let reg = BlockRegistry::with_pre113_defaults();

        // Named zombie with Java formatting codes + JSON color
        let mut z = NbtCompound::new();
        z.insert("id", "minecraft:zombie");
        z.insert(
            "Pos",
            NbtList::from_items(vec![
                NbtTag::Double(1.0),
                NbtTag::Double(64.0),
                NbtTag::Double(2.0),
            ])
            .unwrap(),
        );
        z.insert("CustomName", r#"{"text":"§c§lBob §rthe Zombie"}"#);
        let ez = extract_entity_display(&z, &reg).unwrap();
        assert_eq!(ez.slug, "zombie");
        assert_eq!(spans_plain_text(&ez.primary_name), "Bob the Zombie");
        assert_eq!(ez.primary_name[0].style.color, Some([0xFF, 0x55, 0x55]));
        assert!(ez.primary_name[0].style.bold);
        assert_eq!(ez.primary_name[1].style, TextStyle::default());
        assert_eq!(ez.subtitle.as_deref(), Some("Zombie"));

        // Dropped stone item stack
        let mut it_stack = NbtCompound::new();
        it_stack.insert("id", "minecraft:stone");
        it_stack.insert("Count", 12i8);
        let mut item_ent = NbtCompound::new();
        item_ent.insert("id", "minecraft:item");
        item_ent.insert(
            "Pos",
            NbtList::from_items(vec![
                NbtTag::Double(4.0),
                NbtTag::Double(65.0),
                NbtTag::Double(4.0),
            ])
            .unwrap(),
        );
        item_ent.insert("Item", it_stack);
        let ei = extract_entity_display(&item_ent, &reg).unwrap();
        assert_eq!(ei.slug, "item");
        assert_eq!(ei.subtitle.as_deref(), Some("Stone x12"));
        assert!(ei.atlas_tile.is_some());

        // Legacy pre-1.11 PigZombie
        assert_eq!(normalize_entity_slug("PigZombie"), "zombified_piglin");
        assert_eq!(normalize_entity_slug("ArmorStand"), "armor_stand");

        // Invisible armor stand -> MC nametag-only mode, no subtitle; visible armor stand -> normal
        let mut holo = NbtCompound::new();
        holo.insert("id", "minecraft:armor_stand");
        holo.insert(
            "Pos",
            NbtList::from_items(vec![
                NbtTag::Double(0.0),
                NbtTag::Double(70.0),
                NbtTag::Double(0.0),
            ])
            .unwrap(),
        );
        holo.insert("Invisible", 1i8);
        holo.insert("Marker", 1i8);
        holo.insert("CustomName", r#"{"text":"§e§lSpawn Hub"}"#);
        let eh = extract_entity_display(&holo, &reg).unwrap();
        assert!(eh.mc_nametag_only);
        assert!(eh.subtitle.is_none());
        assert_eq!(spans_plain_text(&eh.primary_name), "Spawn Hub");
        assert!((eh.pos.y - 70.25).abs() < 1e-4);

        holo.insert("Invisible", 0i8);
        let ev = extract_entity_display(&holo, &reg).unwrap();
        assert!(!ev.mc_nametag_only);
        assert_eq!(ev.subtitle.as_deref(), Some("Armor Stand"));
    }
}
