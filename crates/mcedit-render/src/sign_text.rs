use crate::mesher::{MeshBucket, TerrainVertex};
use glam::{IVec3, Vec3};
use mcedit_core::{BlockMeta, BlockShape, BlockState};
use std::f32::consts::TAU;

pub const SIGN_FONT_ATLAS_W: u32 = 128; // 16 glyphs * 8 px
pub const SIGN_FONT_ATLAS_H: u32 = 64; // 8 rows * 8 px

/// Minecraft Java Edition per-span text formatting state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextStyle {
    /// Explicit RGB override (from `§0`–`§f`, named JSON color, or `#RRGGBB`).
    /// When `None`, the sign dye color or label default color is used.
    pub color: Option<[u8; 3]>,
    pub bold: bool,
    pub italic: bool,
    pub underlined: bool,
    pub strikethrough: bool,
    pub obfuscated: bool,
}

impl TextStyle {
    #[inline]
    pub fn clear_formatting(&mut self) {
        self.bold = false;
        self.italic = false;
        self.underlined = false;
        self.strikethrough = false;
        self.obfuscated = false;
    }
}

/// A contiguous run of text sharing one `TextStyle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: TextStyle,
}

impl StyledSpan {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: TextStyle::default(),
        }
    }
}

/// Maps Minecraft **Java Edition** `§0`–`§f` color codes (case-insensitive) to exact sRGB.
/// Bedrock-only codes (`§g`–`§v`) intentionally return `None`.
pub fn java_color_code(code: char) -> Option<[u8; 3]> {
    match code.to_ascii_lowercase() {
        '0' => Some([0x00, 0x00, 0x00]), // black
        '1' => Some([0x00, 0x00, 0xAA]), // dark_blue
        '2' => Some([0x00, 0xAA, 0x00]), // dark_green
        '3' => Some([0x00, 0xAA, 0xAA]), // dark_aqua
        '4' => Some([0xAA, 0x00, 0x00]), // dark_red
        '5' => Some([0xAA, 0x00, 0xAA]), // dark_purple
        '6' => Some([0xFF, 0xAA, 0x00]), // gold
        '7' => Some([0xAA, 0xAA, 0xAA]), // gray
        '8' => Some([0x55, 0x55, 0x55]), // dark_gray
        '9' => Some([0x55, 0x55, 0xFF]), // blue
        'a' => Some([0x55, 0xFF, 0x55]), // green
        'b' => Some([0x55, 0xFF, 0xFF]), // aqua
        'c' => Some([0xFF, 0x55, 0x55]), // red
        'd' => Some([0xFF, 0x55, 0xFF]), // light_purple
        'e' => Some([0xFF, 0xFF, 0x55]), // yellow
        'f' => Some([0xFF, 0xFF, 0xFF]), // white
        _ => None,
    }
}

/// Parses a Minecraft Java Edition JSON/NBT `"color"` field (one of the 16 named Java colors
/// or a `#RRGGBB` hex color added in 1.16).
pub fn java_named_or_hex_color(name: &str) -> Option<[u8; 3]> {
    let trimmed = name.trim();
    if let Some(hex) = trimmed.strip_prefix('#')
        && hex.len() == 6
        && let Ok(v) = u32::from_str_radix(hex, 16)
    {
        return Some([((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8]);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "black" => Some([0x00, 0x00, 0x00]),
        "dark_blue" => Some([0x00, 0x00, 0xAA]),
        "dark_green" => Some([0x00, 0xAA, 0x00]),
        "dark_aqua" => Some([0x00, 0xAA, 0xAA]),
        "dark_red" => Some([0xAA, 0x00, 0x00]),
        "dark_purple" => Some([0xAA, 0x00, 0xAA]),
        "gold" => Some([0xFF, 0xAA, 0x00]),
        "gray" => Some([0xAA, 0xAA, 0xAA]),
        "dark_gray" => Some([0x55, 0x55, 0x55]),
        "blue" => Some([0x55, 0x55, 0xFF]),
        "green" => Some([0x55, 0xFF, 0x55]),
        "aqua" => Some([0x55, 0xFF, 0xFF]),
        "red" => Some([0xFF, 0x55, 0x55]),
        "light_purple" => Some([0xFF, 0x55, 0xFF]),
        "yellow" => Some([0xFF, 0xFF, 0x55]),
        "white" => Some([0xFF, 0xFF, 0xFF]),
        _ => None,
    }
}

/// Parses Minecraft **Java Edition** `§` formatting codes inside `raw`, starting from `base_style`
/// and pushing styled spans into `out`.
///
/// Follows Java Edition rules:
/// - `§0`–`§f` sets the color AND resets `bold`/`italic`/`underlined`/`strikethrough`/`obfuscated`.
/// - `§k` obfuscated, `§l` bold, `§m` strikethrough, `§n` underline, `§o` italic.
/// - `§r` resets both color and formatting back to default.
/// - Non-Java codes (e.g. Bedrock `§g`–`§v`) are stripped without altering style.
pub fn parse_section_codes(raw: &str, base_style: TextStyle, out: &mut Vec<StyledSpan>) {
    let mut style = base_style;
    let mut buf = String::new();
    let mut chars = raw.chars();

    let flush = |buf: &mut String, style: TextStyle, out: &mut Vec<StyledSpan>| {
        if buf.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut()
            && last.style == style
        {
            last.text.push_str(buf);
            buf.clear();
            return;
        }
        out.push(StyledSpan {
            text: std::mem::take(buf),
            style,
        });
    };

    while let Some(c) = chars.next() {
        if c == '§' {
            let Some(code) = chars.next() else {
                break;
            };
            if let Some(rgb) = java_color_code(code) {
                flush(&mut buf, style, out);
                style.color = Some(rgb);
                style.clear_formatting();
            } else {
                match code.to_ascii_lowercase() {
                    'k' => {
                        flush(&mut buf, style, out);
                        style.obfuscated = true;
                    }
                    'l' => {
                        flush(&mut buf, style, out);
                        style.bold = true;
                    }
                    'm' => {
                        flush(&mut buf, style, out);
                        style.strikethrough = true;
                    }
                    'n' => {
                        flush(&mut buf, style, out);
                        style.underlined = true;
                    }
                    'o' => {
                        flush(&mut buf, style, out);
                        style.italic = true;
                    }
                    'r' => {
                        flush(&mut buf, style, out);
                        style = TextStyle::default();
                    }
                    _ => {} // Unknown / non-Java code: strip pair, leave style unchanged
                }
            }
        } else {
            buf.push(c);
        }
    }
    flush(&mut buf, style, out);
}

/// Concatenates the visible plain text of a slice of styled spans.
pub fn spans_plain_text(spans: &[StyledSpan]) -> String {
    let total: usize = spans.iter().map(|s| s.text.len()).sum();
    let mut out = String::with_capacity(total);
    for s in spans {
        out.push_str(&s.text);
    }
    out
}

/// Returns `true` if any span contains non-whitespace obfuscated (`§k`) text.
pub fn spans_has_obfuscated(spans: &[StyledSpan]) -> bool {
    spans
        .iter()
        .any(|s| s.style.obfuscated && s.text.chars().any(|c| !c.is_whitespace()))
}

/// Picks a deterministic cycling printable ASCII glyph for Minecraft `§k` obfuscated text.
pub fn obfuscated_ascii_for(seed: u64) -> u8 {
    const POOL: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz0123456789#$%&@*+?";
    // SplitMix64 avalanche
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    POOL[(x as usize) % POOL.len()]
}

/// Exact 3D world-space geometry of a Minecraft wooden sign board, shared by both the
/// chunk mesher (wooden sign board) and the 3D in-world sign text renderer.
#[derive(Debug, Clone, Copy)]
pub struct SignPlacement {
    pub center: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub normal: Vec3,
    pub width: f32,
    pub height: f32,
    pub thickness: f32,
    pub is_wall: bool,
    pub is_hanging: bool,
}

impl SignPlacement {
    /// Computes exact 3D board placement from a sign block's world coordinate, state, and metadata.
    pub fn from_block(block_pos: IVec3, state: &BlockState, meta: &BlockMeta) -> Self {
        let bx = block_pos.x as f32;
        let by = block_pos.y as f32;
        let bz = block_pos.z as f32;
        let name = state.name.as_str();
        let is_hanging = name.contains("hanging_sign");
        let is_wall = meta.shape == BlockShape::WallSign || name.contains("wall_sign") || name.contains("wall_hanging_sign");

        if is_wall {
            let facing = state
                .properties
                .get("facing")
                .map(|s| s.as_str())
                .unwrap_or_else(|| match meta.legacy_id.map(|(_, d)| d) {
                    Some(2) => "north",
                    Some(3) => "south",
                    Some(4) => "west",
                    Some(5) => "east",
                    _ => "north",
                });
            let (normal, right) = match facing {
                "east" => (Vec3::X, -Vec3::Z),
                "west" => (-Vec3::X, Vec3::Z),
                "south" => (Vec3::Z, Vec3::X),
                _ => (-Vec3::Z, -Vec3::X), // north
            };
            if is_hanging {
                return Self {
                    center: Vec3::new(bx + 0.5, by + 0.34, bz + 0.5),
                    right,
                    up: Vec3::Y,
                    normal,
                    width: 0.875,
                    height: 0.625,
                    thickness: 0.125,
                    is_wall: true,
                    is_hanging: true,
                };
            }
            let thickness = 0.08;
            let width = 0.94;
            let height = 0.48;
            let center = Vec3::new(bx + 0.5, by + 0.53, bz + 0.5) - normal * (0.5 - thickness * 0.5);
            return Self {
                center,
                right,
                up: Vec3::Y,
                normal,
                width,
                height,
                thickness,
                is_wall: true,
                is_hanging: false,
            };
        }

        // Standing or ceiling-hanging sign: 16-angle `rotation` (0=South, 4=West, 8=North, 12=East)
        let rot: u8 = state
            .properties
            .get("rotation")
            .and_then(|s| s.parse::<u8>().ok())
            .or_else(|| meta.legacy_id.map(|(_, d)| d & 15))
            .unwrap_or(0)
            & 15;
        let theta = (rot as f32) * (TAU / 16.0);
        let normal = Vec3::new(-theta.sin(), 0.0, theta.cos()).normalize();
        let right = Vec3::new(theta.cos(), 0.0, theta.sin()).normalize();

        if is_hanging {
            Self {
                center: Vec3::new(bx + 0.5, by + 0.34, bz + 0.5),
                right,
                up: Vec3::Y,
                normal,
                width: 0.875,
                height: 0.625,
                thickness: 0.125,
                is_wall: false,
                is_hanging: true,
            }
        } else {
            Self {
                center: Vec3::new(bx + 0.5, by + 0.82, bz + 0.5),
                right,
                up: Vec3::Y,
                normal,
                width: 0.94,
                height: 0.48,
                thickness: 0.08,
                is_wall: false,
                is_hanging: false,
            }
        }
    }

    /// Emits the 6 outward-CCW wooden faces of this sign board (oriented by `right`, `up`, `normal`)
    /// into `bucket` using the provided texture UV and base directional-shaded color.
    pub fn emit_board_box(
        &self,
        bucket: &mut MeshBucket,
        uv: [f32; 4],
        base_color: [u8; 4],
    ) {
        let hw = self.width * 0.5;
        let hh = self.height * 0.5;
        let ht = self.thickness * 0.5;
        let r = self.right * hw;
        let u = self.up * hh;
        let n = self.normal * ht;
        let c = self.center;

        let f_tl = (c + n - r + u).to_array();
        let f_tr = (c + n + r + u).to_array();
        let f_br = (c + n + r - u).to_array();
        let f_bl = (c + n - r - u).to_array();

        let b_tl = (c - n - r + u).to_array();
        let b_tr = (c - n + r + u).to_array();
        let b_br = (c - n + r - u).to_array();
        let b_bl = (c - n - r - u).to_array();

        let scale = |col: [u8; 4], f: f32| -> [u8; 4] {
            [
                (col[0] as f32 * f).clamp(0.0, 255.0) as u8,
                (col[1] as f32 * f).clamp(0.0, 255.0) as u8,
                (col[2] as f32 * f).clamp(0.0, 255.0) as u8,
                col[3],
            ]
        };

        // Outward CCW quads matching shared_quad_ibuf `[0, 2, 1, 0, 3, 2]` (TL, TR, BR, BL):
        // Front (+normal)
        bucket.push_quad([f_tl, f_tr, f_br, f_bl], uv, scale(base_color, 0.92));
        // Back (-normal)
        bucket.push_quad([b_tr, b_tl, b_bl, b_br], uv, scale(base_color, 0.85));
        // Top (+up)
        bucket.push_quad([b_tl, b_tr, f_tr, f_tl], uv, base_color);
        // Bottom (-up)
        bucket.push_quad([f_bl, f_br, b_br, b_bl], uv, scale(base_color, 0.60));
        // Right (+right)
        bucket.push_quad([f_tr, b_tr, b_br, f_br], uv, scale(base_color, 0.74));
        // Left (-right)
        bucket.push_quad([b_tl, f_tl, f_bl, b_bl], uv, scale(base_color, 0.74));
    }
}

/// Emits 3D world-space textured glyph quads flush against a sign's front (and optional back) face,
/// supporting all Minecraft Java Edition colors and formatting (`§0`–`§f`, `§k`–`§o`, `§r`).
#[allow(clippy::too_many_arguments)]
pub fn push_sign_text_quads(
    out: &mut MeshBucket,
    placement: &SignPlacement,
    front_lines: &[Vec<StyledSpan>],
    back_lines: &[Vec<StyledSpan>],
    front_default_color: [u8; 4],
    front_glowing: bool,
    back_default_color: [u8; 4],
    back_glowing: bool,
    obfuscate_tick: u64,
) {
    let front_offset = placement.thickness * 0.5 + 0.009;
    let pos_salt = ((placement.center.x.to_bits() as u64).wrapping_mul(31))
        ^ ((placement.center.y.to_bits() as u64).wrapping_mul(131))
        ^ ((placement.center.z.to_bits() as u64).wrapping_mul(8191));
    if !front_lines.is_empty() {
        emit_sign_face_lines(
            out,
            placement.center + placement.normal * front_offset,
            placement.right,
            placement.up,
            placement.normal,
            placement.width,
            placement.height,
            front_lines,
            front_default_color,
            front_glowing,
            pos_salt,
            obfuscate_tick,
        );
    }
    if !back_lines.is_empty() && (!placement.is_wall || placement.is_hanging) {
        emit_sign_face_lines(
            out,
            placement.center - placement.normal * front_offset,
            -placement.right,
            placement.up,
            -placement.normal,
            placement.width,
            placement.height,
            back_lines,
            back_default_color,
            back_glowing,
            pos_salt ^ 0xA5A5_5A5A_u64,
            obfuscate_tick,
        );
    }
}

#[inline]
fn glowing_outline_color(col: [u8; 4]) -> [u8; 4] {
    let luma = (col[0] as u32 * 299 + col[1] as u32 * 587 + col[2] as u32 * 114) / 1000;
    if luma < 40 {
        [240, 236, 215, 255]
    } else {
        [(col[0] / 4), (col[1] / 4), (col[2] / 4), 255]
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_sign_face_lines(
    out: &mut MeshBucket,
    face_center: Vec3,
    right: Vec3,
    up: Vec3,
    normal: Vec3,
    board_w: f32,
    board_h: f32,
    lines: &[Vec<StyledSpan>],
    default_color: [u8; 4],
    glowing: bool,
    face_salt: u64,
    obfuscate_tick: u64,
) {
    let usable_w = board_w * 0.90;
    let usable_h = board_h * 0.86;
    let num_rows = lines.len().clamp(1, 4);
    let base_row_h = usable_h / 4.0;
    let base_glyph_h = base_row_h * 0.80;
    let base_glyph_w = base_glyph_h * (6.0 / 7.0);
    let base_advance = base_glyph_w * 1.05;
    let solid_uv = sign_font_solid_uv();

    // Vertically center the populated lines on the board
    let total_block_h = (num_rows as f32) * base_row_h;
    let top_y = total_block_h * 0.5;

    let mut styled_chars: Vec<(char, TextStyle)> = Vec::new();
    for (row_idx, line) in lines.iter().take(4).enumerate() {
        styled_chars.clear();
        for span in line {
            for ch in span.text.chars() {
                styled_chars.push((ch, span.style));
            }
        }
        if styled_chars.is_empty() {
            continue;
        }

        let raw_w: f32 = styled_chars
            .iter()
            .map(|(_, st)| if st.bold { base_advance * 1.14 } else { base_advance })
            .sum();
        let fit_scale = if raw_w > usable_w {
            usable_w / raw_w
        } else {
            1.0
        };
        let gw = base_glyph_w * fit_scale;
        let gh = base_glyph_h * fit_scale;
        let adv = base_advance * fit_scale;
        let line_w = raw_w * fit_scale;

        let row_center_y = top_y - (row_idx as f32 + 0.5) * base_row_h;
        let gy_top = row_center_y + gh * 0.5;
        let gy_bot = row_center_y - gh * 0.5;
        let mut gx = -line_w * 0.5;

        for (ci, &(ch, style)) in styled_chars.iter().enumerate() {
            let char_adv = if style.bold { adv * 1.14 } else { adv };
            let col = style
                .color
                .map(|[r, g, b]| [r, g, b, 255])
                .unwrap_or(default_color);

            // Underline (§n) & Strikethrough (§m) span spaces too, matching Java Edition
            if style.underlined {
                let uy_top = gy_bot - gh * 0.04;
                let uy_bot = gy_bot - gh * 0.16;
                push_oriented_glyph(
                    out,
                    face_center + normal * 0.0005,
                    right,
                    up,
                    gx,
                    gx + char_adv,
                    uy_top,
                    uy_bot,
                    0.0,
                    solid_uv,
                    col,
                );
            }
            if style.strikethrough {
                let sy_top = row_center_y + gh * 0.06;
                let sy_bot = row_center_y - gh * 0.06;
                push_oriented_glyph(
                    out,
                    face_center + normal * 0.001,
                    right,
                    up,
                    gx,
                    gx + char_adv,
                    sy_top,
                    sy_bot,
                    0.0,
                    solid_uv,
                    col,
                );
            }

            if ch == ' ' {
                gx += char_adv;
                continue;
            }

            let code = if style.obfuscated {
                let seed = face_salt
                    ^ ((row_idx as u64).wrapping_mul(1009))
                    ^ ((ci as u64).wrapping_mul(9173))
                    ^ obfuscate_tick.wrapping_mul(0x517C_C1B7_2722_0A95);
                obfuscated_ascii_for(seed)
            } else if (0x20..=0x7E).contains(&(ch as u32)) {
                ch as u8
            } else {
                b'?'
            };
            let uv = glyph_uv(code);
            let italic_slant = if style.italic { gw * 0.28 } else { 0.0 };
            let bold_dx = gw * 0.16;

            if glowing {
                let outline_col = glowing_outline_color(col);
                let o = gw * 0.14;
                let back_n = normal * -0.002;
                for (ox, oy) in [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o)] {
                    push_oriented_glyph(
                        out,
                        face_center + back_n,
                        right,
                        up,
                        gx + ox,
                        gx + gw + ox,
                        gy_top + oy,
                        gy_bot + oy,
                        italic_slant,
                        uv,
                        outline_col,
                    );
                }
            }

            push_oriented_glyph(
                out,
                face_center,
                right,
                up,
                gx,
                gx + gw,
                gy_top,
                gy_bot,
                italic_slant,
                uv,
                col,
            );

            if style.bold {
                push_oriented_glyph(
                    out,
                    face_center + normal * 0.0003,
                    right,
                    up,
                    gx + bold_dx,
                    gx + gw + bold_dx,
                    gy_top,
                    gy_bot,
                    italic_slant,
                    uv,
                    col,
                );
            }

            gx += char_adv;
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn push_oriented_glyph(
    out: &mut MeshBucket,
    origin: Vec3,
    right: Vec3,
    up: Vec3,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    top_slant: f32,
    uv: [f32; 4],
    color: [u8; 4],
) {
    let tl = (origin + right * (x0 + top_slant) + up * y_top).to_array();
    let tr = (origin + right * (x1 + top_slant) + up * y_top).to_array();
    let br = (origin + right * x1 + up * y_bot).to_array();
    let bl = (origin + right * x0 + up * y_bot).to_array();
    let [u0, v0, u1, v1] = uv;
    out.vertices.extend_from_slice(&[
        TerrainVertex { pos: tl, uv: [u0, v0], color },
        TerrainVertex { pos: tr, uv: [u1, v0], color },
        TerrainVertex { pos: br, uv: [u1, v1], color },
        TerrainVertex { pos: bl, uv: [u0, v1], color },
    ]);
}

#[inline]
pub fn sign_font_solid_uv() -> [f32; 4] {
    // Cell 0x7F (col 15, row 7) is filled completely solid white for underline/strikethrough bars
    let u0 = (15.0 * 8.0 + 2.0) / (SIGN_FONT_ATLAS_W as f32);
    let v0 = (7.0 * 8.0 + 2.0) / (SIGN_FONT_ATLAS_H as f32);
    let u1 = (15.0 * 8.0 + 6.0) / (SIGN_FONT_ATLAS_W as f32);
    let v1 = (7.0 * 8.0 + 6.0) / (SIGN_FONT_ATLAS_H as f32);
    [u0, v0, u1, v1]
}

/// Exact 5x7 glyph UV bounds inside the 128x64 bitmap font atlas for crisp 2D NEAREST pixel rendering.
#[inline]
pub fn sign_font_glyph_uv_exact(ascii: u8) -> [f32; 4] {
    let col = (ascii & 0x0F) as f32;
    let row = ((ascii >> 4) & 0x07) as f32;
    let u0 = (col * 8.0 + 1.0) / (SIGN_FONT_ATLAS_W as f32);
    let v0 = (row * 8.0 + 1.0) / (SIGN_FONT_ATLAS_H as f32);
    let u1 = (col * 8.0 + 6.0) / (SIGN_FONT_ATLAS_W as f32);
    let v1 = (row * 8.0 + 8.0) / (SIGN_FONT_ATLAS_H as f32);
    [u0, v0, u1, v1]
}

#[inline]
fn glyph_uv(ascii: u8) -> [f32; 4] {
    let col = (ascii & 0x0F) as f32;
    let row = ((ascii >> 4) & 0x07) as f32;
    // Sample inner 6x7 glyph pixels inside the 8x8 cell so adjacent glyphs never bleed
    let u0 = (col * 8.0 + 0.5) / (SIGN_FONT_ATLAS_W as f32);
    let v0 = (row * 8.0 + 0.5) / (SIGN_FONT_ATLAS_H as f32);
    let u1 = (col * 8.0 + 6.5) / (SIGN_FONT_ATLAS_W as f32);
    let v1 = (row * 8.0 + 7.5) / (SIGN_FONT_ATLAS_H as f32);
    [u0, v0, u1, v1]
}

/// Builds the crisp 128x64 RGBA8 bitmap font atlas for ASCII 0..127 (16x8 grid of 8x8 cells).
/// Pixels are pure white `(255, 255, 255, 255)` on transparent `(0, 0, 0, 0)` so vertex color tints freely.
pub fn build_sign_font_atlas_rgba() -> Vec<u8> {
    let mut rgba = vec![0u8; (SIGN_FONT_ATLAS_W * SIGN_FONT_ATLAS_H * 4) as usize];
    for ch in 32u8..=126u8 {
        let rows = ascii_5x7_bitmap(ch);
        let cell_x = ((ch & 0x0F) as u32) * 8;
        let cell_y = (((ch >> 4) & 0x07) as u32) * 8;
        for (ry, &bits) in rows.iter().enumerate() {
            for rx in 0..5u32 {
                if (bits & (1u8 << (4 - rx))) != 0 {
                    let px = cell_x + 1 + rx;
                    let py = cell_y + 1 + (ry as u32);
                    let idx = ((py * SIGN_FONT_ATLAS_W + px) * 4) as usize;
                    rgba[idx] = 255;
                    rgba[idx + 1] = 255;
                    rgba[idx + 2] = 255;
                    rgba[idx + 3] = 255;
                }
            }
        }
    }
    // Fill unused cell 0x7F (col 15, row 7) solid white for underline/strikethrough quads
    for py in (7 * 8)..(8 * 8) {
        for px in (15 * 8)..(16 * 8) {
            let idx = ((py * SIGN_FONT_ATLAS_W + px) * 4) as usize;
            rgba[idx] = 255;
            rgba[idx + 1] = 255;
            rgba[idx + 2] = 255;
            rgba[idx + 3] = 255;
        }
    }
    rgba
}

/// Clean 5x7 pixel bitmap font for printable ASCII `32..=126` (MSB = leftmost pixel of 5).
fn ascii_5x7_bitmap(ch: u8) -> [u8; 7] {
    match ch {
        b' ' => [0, 0, 0, 0, 0, 0, 0],
        b'!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        b'"' => [0x0A, 0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00],
        b'#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        b'$' => [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04],
        b'%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        b'&' => [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D],
        b'\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        b'(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        b')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        b'*' => [0x00, 0x04, 0x15, 0x0E, 0x15, 0x04, 0x00],
        b'+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        b',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        b'-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        b'.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        b'/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        b'0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        b'1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        b'2' => [0x0E, 0x11, 0x01, 0x06, 0x08, 0x10, 0x1F],
        b'3' => [0x1F, 0x02, 0x04, 0x06, 0x01, 0x11, 0x0E],
        b'4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        b'5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        b'6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        b'7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        b'8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        b'9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        b':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        b';' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x04, 0x08],
        b'<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        b'=' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        b'>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        b'?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        b'@' => [0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0E],
        b'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        b'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        b'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        b'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        b'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        b'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        b'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        b'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        b'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        b'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        b'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        b'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        b'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        b'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        b'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        b'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        b'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        b'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        b'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        b'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        b'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        b'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        b'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        b'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        b'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        b'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        b'[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        b'\\' => [0x10, 0x10, 0x08, 0x04, 0x02, 0x01, 0x01],
        b']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
        b'^' => [0x04, 0x0A, 0x11, 0x00, 0x00, 0x00, 0x00],
        b'_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        b'`' => [0x08, 0x04, 0x02, 0x00, 0x00, 0x00, 0x00],
        b'a' => [0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F],
        b'b' => [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x1E],
        b'c' => [0x00, 0x00, 0x0E, 0x10, 0x10, 0x11, 0x0E],
        b'd' => [0x01, 0x01, 0x0F, 0x11, 0x11, 0x11, 0x0F],
        b'e' => [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E],
        b'f' => [0x06, 0x09, 0x08, 0x1C, 0x08, 0x08, 0x08],
        b'g' => [0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x0E],
        b'h' => [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x11],
        b'i' => [0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E],
        b'j' => [0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0C],
        b'k' => [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12],
        b'l' => [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        b'm' => [0x00, 0x00, 0x1A, 0x15, 0x15, 0x15, 0x15],
        b'n' => [0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x11],
        b'o' => [0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E],
        b'p' => [0x00, 0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10],
        b'q' => [0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x01],
        b'r' => [0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10],
        b's' => [0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E],
        b't' => [0x08, 0x08, 0x1C, 0x08, 0x08, 0x09, 0x06],
        b'u' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x13, 0x0D],
        b'v' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04],
        b'w' => [0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0A],
        b'x' => [0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11],
        b'y' => [0x00, 0x11, 0x11, 0x0F, 0x01, 0x11, 0x0E],
        b'z' => [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F],
        b'{' => [0x02, 0x04, 0x04, 0x08, 0x04, 0x04, 0x02],
        b'|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        b'}' => [0x08, 0x04, 0x04, 0x02, 0x04, 0x04, 0x08],
        b'~' => [0x00, 0x08, 0x15, 0x02, 0x00, 0x00, 0x00],
        _ => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
    }
}
