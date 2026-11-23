use image::{RgbaImage, imageops::FilterType};
use mcedit_core::{BlockMeta, BlockRegistry, BlockStateId, Face, TintKind};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

pub const DEFAULT_TERRAIN_PNG: &[u8] = include_bytes!("../../../assets/terrain.png");
pub const ATLAS_SIZE: u32 = 512;
pub const TILE_SIZE: u32 = 16;
pub const GRID_DIM: u32 = ATLAS_SIZE / TILE_SIZE; // 32

/// Precomputed per-block visual properties for distant LOD meshing,
/// sampled directly from the active `TextureAtlas` so LOD colors match 3D textures.
#[derive(Debug, Clone, Copy)]
pub struct LodBlockVisual {
    /// Average RGBA per `Face::index()` (`[East, West, Up, Down, South, North]`).
    pub face_colors: [[u8; 4]; 6],
    pub is_opaque: bool,
    pub is_translucent: bool,
}

impl Default for LodBlockVisual {
    fn default() -> Self {
        Self {
            face_colors: [[128, 128, 128, 255]; 6],
            is_opaque: true,
            is_translucent: false,
        }
    }
}

/// Fast `BlockStateId`-indexed lookup table of atlas-sampled LOD face colors.
#[derive(Debug, Clone)]
pub struct LodColorTable {
    entries: Vec<LodBlockVisual>,
}

impl LodColorTable {
    #[inline]
    pub fn get(&self, id: BlockStateId) -> LodBlockVisual {
        self.entries
            .get(id.0 as usize)
            .copied()
            .unwrap_or_default()
    }
}

/// Returns true if `meta.tint` should be multiplied onto `face` (avoids tinting
/// already-colored dirt/grass-trim side faces on `minecraft:grass`).
#[inline]
pub fn should_tint_face(meta: &BlockMeta, face: Face) -> bool {
    match meta.tint {
        TintKind::None => false,
        TintKind::Grass => meta.face_tiles[face.index()] == meta.face_tiles[Face::Up.index()],
        _ => true,
    }
}

pub struct TextureAtlas {
    pub rgba: RgbaImage,
}

impl TextureAtlas {
    pub fn default_atlas() -> Self {
        let img = image::load_from_memory(DEFAULT_TERRAIN_PNG)
            .expect("embedded terrain.png must decode")
            .to_rgba8();
        let rgba = if img.width() == ATLAS_SIZE && img.height() == ATLAS_SIZE {
            img
        } else {
            image::imageops::resize(&img, ATLAS_SIZE, ATLAS_SIZE, FilterType::Nearest)
        };
        Self { rgba }
    }

    /// Loads default atlas and overlays any matching block textures from a Minecraft resource pack (`.zip` or directory).
    pub fn from_resource_pack(path: &Path) -> Self {
        let mut atlas = Self::default_atlas();
        let pack_map = load_pack_block_textures(path);
        if pack_map.is_empty() {
            return atlas;
        }
        for (name, tile_coords) in pack_texture_slots() {
            if let Some(tile_img) = pack_map.get(*name) {
                let px = (tile_coords[0] as u32) * TILE_SIZE;
                let py = (tile_coords[1] as u32) * TILE_SIZE;
                if px + TILE_SIZE <= ATLAS_SIZE && py + TILE_SIZE <= ATLAS_SIZE {
                    let resized =
                        image::imageops::resize(tile_img, TILE_SIZE, TILE_SIZE, FilterType::Nearest);
                    image::imageops::overlay(&mut atlas.rgba, &resized, px as i64, py as i64);
                }
            }
        }
        atlas
    }

    /// Computes exact per-face average colors for every registered `BlockStateId` directly
    /// from the pixels in this `TextureAtlas` multiplied by block tint.
    pub fn build_lod_colors(&self, reg: &BlockRegistry) -> LodColorTable {
        let mut tile_avgs = [[[128u8, 128, 128, 255]; GRID_DIM as usize]; GRID_DIM as usize];

        for ty in 0..GRID_DIM {
            for tx in 0..GRID_DIM {
                let mut r_op = 0u64;
                let mut g_op = 0u64;
                let mut b_op = 0u64;
                let mut cnt_op = 0u64;

                let mut r_tr = 0u64;
                let mut g_tr = 0u64;
                let mut b_tr = 0u64;
                let mut a_tr = 0u64;
                let mut cnt_tr = 0u64;

                let x0 = tx * TILE_SIZE;
                let y0 = ty * TILE_SIZE;
                for py in y0..(y0 + TILE_SIZE) {
                    for px in x0..(x0 + TILE_SIZE) {
                        let p = self.rgba.get_pixel(px, py).0;
                        if p[3] >= 128 {
                            r_op += p[0] as u64;
                            g_op += p[1] as u64;
                            b_op += p[2] as u64;
                            cnt_op += 1;
                        } else if p[3] >= 8 {
                            r_tr += p[0] as u64;
                            g_tr += p[1] as u64;
                            b_tr += p[2] as u64;
                            a_tr += p[3] as u64;
                            cnt_tr += 1;
                        }
                    }
                }

                tile_avgs[ty as usize][tx as usize] = if let Some(r) = r_op.checked_div(cnt_op) {
                    [
                        r as u8,
                        (g_op / cnt_op.max(1)) as u8,
                        (b_op / cnt_op.max(1)) as u8,
                        255,
                    ]
                } else if let Some(r) = r_tr.checked_div(cnt_tr) {
                    let denom = cnt_tr.max(1);
                    [
                        r as u8,
                        (g_tr / denom) as u8,
                        (b_tr / denom) as u8,
                        ((a_tr / denom) as u8).clamp(110, 210),
                    ]
                } else {
                    [128, 128, 128, 255]
                };
            }
        }

        let mut entries = Vec::with_capacity(reg.len());
        for id in reg.all_ids() {
            let meta = reg.get_meta(id);
            let mut face_colors = [[0u8; 4]; 6];
            for face in Face::ALL {
                let tile = meta.face_tiles[face.index()];
                let base = if tile == [0, 21] {
                    // Fallback missing-texture tile: use map_color
                    [meta.map_color[0], meta.map_color[1], meta.map_color[2], 255]
                } else {
                    let tx = (tile[0] as usize).min((GRID_DIM - 1) as usize);
                    let ty = (tile[1] as usize).min((GRID_DIM - 1) as usize);
                    tile_avgs[ty][tx]
                };

                let rgb = if should_tint_face(meta, face) {
                    let t = meta.tint.rgb_multiplier();
                    [
                        (base[0] as f32 * t[0]).clamp(0.0, 255.0) as u8,
                        (base[1] as f32 * t[1]).clamp(0.0, 255.0) as u8,
                        (base[2] as f32 * t[2]).clamp(0.0, 255.0) as u8,
                    ]
                } else {
                    [base[0], base[1], base[2]]
                };

                let alpha = if meta.is_liquid {
                    195
                } else if meta.is_translucent {
                    base[3].clamp(115, 200)
                } else {
                    255
                };
                face_colors[face.index()] = [rgb[0], rgb[1], rgb[2], alpha];
            }

            entries.push(LodBlockVisual {
                face_colors,
                is_opaque: meta.is_full_opaque_cube,
                is_translucent: meta.is_translucent,
            });
        }

        LodColorTable { entries }
    }

    #[inline]
    pub fn tile_uv(tile: [u8; 2]) -> [f32; 4] {
        let u0 = (tile[0] as f32) / (GRID_DIM as f32);
        let v0 = (tile[1] as f32) / (GRID_DIM as f32);
        let step = 1.0 / (GRID_DIM as f32);
        let eps = 0.0001;
        [u0 + eps, v0 + eps, u0 + step - eps, v0 + step - eps]
    }

    /// Encodes a tiled atlas UV rectangle for a greedy-meshed `u_span x v_span` quad (`1..=16`).
    /// For `1x1` quads, returns standard `[0.0, 1.0]` atlas UVs; for merged quads, encodes
    /// `tile * 32.0 + 2.0 + local_span` (`>= 2.0`) which `terrain.wgsl` repeats per block.
    #[inline]
    pub fn tiled_uv(tile: [u8; 2], u_span: usize, v_span: usize) -> [f32; 4] {
        if u_span <= 1 && v_span <= 1 {
            return Self::tile_uv(tile);
        }
        let base_u = (tile[0] as f32) * 32.0 + 2.0;
        let base_v = (tile[1] as f32) * 32.0 + 2.0;
        let eps = 0.001;
        [
            base_u + eps,
            base_v + eps,
            base_u + (u_span as f32) - eps,
            base_v + (v_span as f32) - eps,
        ]
    }
}

fn load_pack_block_textures(path: &Path) -> HashMap<String, RgbaImage> {
    let mut out = HashMap::new();
    if path.is_dir() {
        for sub in ["assets/minecraft/textures/blocks", "assets/minecraft/textures/block"] {
            let dir = path.join(sub);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("png")
                        && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                        && let Ok(img) = image::open(&p)
                    {
                        let mut rgba = img.to_rgba8();
                        if rgba.height() > rgba.width() && rgba.width() > 0 {
                            rgba = image::imageops::crop_imm(&rgba, 0, 0, rgba.width(), rgba.width())
                                .to_image();
                        }
                        out.insert(stem.to_string(), rgba);
                    }
                }
            }
        }
    } else if path.is_file()
        && let Ok(file) = File::open(path)
        && let Ok(mut archive) = ZipArchive::new(file)
    {
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                let name = entry.name().to_string();
                let stem = name
                    .strip_prefix("assets/minecraft/textures/blocks/")
                    .or_else(|| name.strip_prefix("assets/minecraft/textures/block/"))
                    .and_then(|s| s.strip_suffix(".png"));
                if let Some(stem) = stem
                    && !stem.contains('/')
                {
                    let mut buf = Vec::new();
                    if entry.read_to_end(&mut buf).is_ok()
                        && let Ok(img) = image::load_from_memory(&buf)
                    {
                        let mut rgba = img.to_rgba8();
                        if rgba.height() > rgba.width() && rgba.width() > 0 {
                            rgba = image::imageops::crop_imm(&rgba, 0, 0, rgba.width(), rgba.width())
                                .to_image();
                        }
                        out.insert(stem.to_string(), rgba);
                    }
                }
            }
        }
    }
    out
}

/// Core subset of `terrain.png` 32x32 grid tile slots for Java Resource Pack overlays.
fn pack_texture_slots() -> &'static [(&'static str, [u8; 2])] {
    &[
        ("grass_top", [0, 0]),
        ("stone", [1, 0]),
        ("dirt", [2, 0]),
        ("grass_side", [3, 0]),
        ("planks_oak", [4, 0]),
        ("stone_slab_side", [5, 0]),
        ("stone_slab_top", [6, 0]),
        ("brick", [7, 0]),
        ("tnt_side", [8, 0]),
        ("tnt_top", [9, 0]),
        ("tnt_bottom", [10, 0]),
        ("cobblestone", [0, 1]),
        ("bedrock", [1, 1]),
        ("sand", [2, 1]),
        ("gravel", [3, 1]),
        ("log_oak", [4, 1]),
        ("log_oak_top", [5, 1]),
        ("iron_block", [6, 1]),
        ("gold_block", [7, 1]),
        ("diamond_block", [8, 1]),
        ("emerald_block", [9, 1]),
        ("redstone_block", [10, 1]),
        ("gold_ore", [0, 2]),
        ("iron_ore", [1, 2]),
        ("coal_ore", [2, 2]),
        ("bookshelf", [3, 2]),
        ("cobblestone_mossy", [4, 2]),
        ("obsidian", [5, 2]),
        ("sponge", [0, 3]),
        ("glass", [1, 3]),
        ("diamond_ore", [2, 3]),
        ("redstone_ore", [3, 3]),
        ("leaves_oak", [4, 3]),
        ("stonebrick", [6, 3]),
        ("wool_colored_white", [0, 4]),
        ("snow", [2, 4]),
        ("ice", [3, 4]),
        ("netherrack", [7, 6]),
        ("soul_sand", [8, 6]),
        ("glowstone", [9, 6]),
        ("water_still", [13, 12]),
        ("lava_still", [13, 14]),
    ]
}
