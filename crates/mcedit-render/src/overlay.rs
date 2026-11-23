use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use mcedit_core::{BoundingBox, ChunkPos, Face};

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct OverlayVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

#[derive(Debug, Default)]
pub struct OverlayMeshBuilder {
    pub line_verts: Vec<OverlayVertex>,
    pub tri_verts: Vec<OverlayVertex>,
}

impl OverlayMeshBuilder {
    pub fn clear(&mut self) {
        self.line_verts.clear();
        self.tri_verts.clear();
    }

    pub fn push_line(&mut self, a: Vec3, b: Vec3, color: [f32; 4]) {
        self.line_verts.push(OverlayVertex {
            pos: a.to_array(),
            color,
        });
        self.line_verts.push(OverlayVertex {
            pos: b.to_array(),
            color,
        });
    }

    pub fn push_wire_box(&mut self, min: Vec3, max: Vec3, color: [f32; 4]) {
        let c = [
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(min.x, max.y, min.z),
            Vec3::new(max.x, max.y, min.z),
            Vec3::new(max.x, max.y, max.z),
            Vec3::new(min.x, max.y, max.z),
        ];
        let edges = [
            (0, 1), (1, 2), (2, 3), (3, 0),
            (4, 5), (5, 6), (6, 7), (7, 4),
            (0, 4), (1, 5), (2, 6), (3, 7),
        ];
        for (a, b) in edges {
            self.push_line(c[a], c[b], color);
        }
    }

    pub fn push_filled_box(&mut self, min: Vec3, max: Vec3, color: [f32; 4]) {
        let [x0, y0, z0] = min.to_array();
        let [x1, y1, z1] = max.to_array();
        let faces = [
            [[x1, y1, z1], [x1, y1, z0], [x1, y0, z0], [x1, y0, z1]],
            [[x0, y1, z0], [x0, y1, z1], [x0, y0, z1], [x0, y0, z0]],
            [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
            [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
            [[x0, y1, z1], [x1, y1, z1], [x1, y0, z1], [x0, y0, z1]],
            [[x1, y1, z0], [x0, y1, z0], [x0, y0, z0], [x1, y0, z0]],
        ];
        for f in faces {
            self.push_quad(f, color);
        }
    }

    pub fn push_face_highlight(&mut self, box_: BoundingBox, face: Face, color: [f32; 4]) {
        let min = box_.min.as_vec3();
        let max = box_.max.as_vec3();
        let eps = 0.02;
        let [x0, y0, z0] = (min - Vec3::splat(eps)).to_array();
        let [x1, y1, z1] = (max + Vec3::splat(eps)).to_array();
        let quad = face_quad_corners(face, [x0, y0, z0], [x1, y1, z1]);
        self.push_quad(quad, color);
    }

    /// Renders the hovered/dragged selection face highlight across the entire face
    /// (full-face translucent fill + full-face wireframe outline).
    pub fn push_mcedit_face_highlight(
        &mut self,
        box_: BoundingBox,
        face: Face,
        fill_color: [f32; 4],
        wire_color: [f32; 4],
    ) {
        let bmin = box_.min.as_vec3();
        let bmax = box_.max.as_vec3();
        let eps = 0.02;

        let full_corners = face_quad_corners(
            face,
            (bmin - Vec3::splat(eps)).to_array(),
            (bmax + Vec3::splat(eps)).to_array(),
        );
        self.push_quad(full_corners, fill_color);
        for i in 0..4 {
            let a = Vec3::from_array(full_corners[i]);
            let b = Vec3::from_array(full_corners[(i + 1) % 4]);
            self.push_line(a, b, wire_color);
        }
    }

    pub fn push_quad(&mut self, corners: [[f32; 3]; 4], color: [f32; 4]) {
        let v: Vec<OverlayVertex> = corners
            .into_iter()
            .map(|pos| OverlayVertex { pos, color })
            .collect();
        self.tri_verts
            .extend_from_slice(&[v[0], v[1], v[2], v[0], v[2], v[3]]);
    }

    /// Builds a static world-space 4-state chunk grid at `y` across `[min_cx..=max_cx, min_cz..=max_cz]`:
    /// - No region file & no chunk: empty square outline
    /// - Region file exists, no chunk: square outline with an `X` cross inside
    /// - Chunk exists & loaded: greedy-merged gray fill
    /// - Chunk exists on disk, not yet loaded: greedy-merged red fill + red outline
    #[allow(clippy::too_many_arguments)]
    pub fn build_static_chunk_grid<FRegion, FExists, FLoaded>(
        min_cx: i32,
        max_cx: i32,
        min_cz: i32,
        max_cz: i32,
        y: f32,
        region_exists: FRegion,
        chunk_exists: FExists,
        chunk_loaded: FLoaded,
    ) -> Self
    where
        FRegion: Fn(ChunkPos) -> bool,
        FExists: Fn(ChunkPos) -> bool,
        FLoaded: Fn(ChunkPos) -> bool,
    {
        let mut builder = Self::default();
        if max_cx < min_cx || max_cz < min_cz {
            return builder;
        }

        const STATE_EMPTY: u8 = 0; // no region file & no chunk
        const STATE_CROSS: u8 = 1; // region file exists, no chunk
        const STATE_GRAY: u8 = 2; // chunk exists & loaded
        const STATE_RED: u8 = 3; // chunk exists & not yet loaded

        let col_grid_line = [0.46, 0.52, 0.62, 0.50];
        let col_region_cross = [0.54, 0.58, 0.68, 0.55];
        let col_loaded_gray = [0.52, 0.56, 0.62, 0.34];
        let col_unloaded_red = [0.88, 0.14, 0.14, 0.48];
        let col_unloaded_border = [1.0, 0.28, 0.28, 0.88];

        let width = (max_cx - min_cx + 1) as usize;
        let depth = (max_cz - min_cz + 1) as usize;
        let mut states = vec![STATE_EMPTY; width * depth];

        // 1. Shared outer grid lines (covers all cells in minimal line segments)
        let x_min = (min_cx * 16) as f32;
        let x_max = ((max_cx + 1) * 16) as f32;
        let z_min = (min_cz * 16) as f32;
        let z_max = ((max_cz + 1) * 16) as f32;

        for cx in min_cx..=(max_cx + 1) {
            let x = (cx * 16) as f32;
            builder.push_line(Vec3::new(x, y, z_min), Vec3::new(x, y, z_max), col_grid_line);
        }
        for cz in min_cz..=(max_cz + 1) {
            let z = (cz * 16) as f32;
            builder.push_line(Vec3::new(x_min, y, z), Vec3::new(x_max, y, z), col_grid_line);
        }

        // 2. Classify every cell and emit X crosses / red outlines
        for gz in 0..depth {
            let cz = min_cz + gz as i32;
            let z0 = (cz * 16) as f32;
            let z1 = z0 + 16.0;
            for gx in 0..width {
                let cx = min_cx + gx as i32;
                let cp = ChunkPos::new(cx, cz);
                let exists = chunk_exists(cp);
                let state = if exists {
                    if chunk_loaded(cp) {
                        STATE_GRAY
                    } else {
                        STATE_RED
                    }
                } else if region_exists(cp) {
                    STATE_CROSS
                } else {
                    STATE_EMPTY
                };
                states[gz * width + gx] = state;

                let x0 = (cx * 16) as f32;
                let x1 = x0 + 16.0;
                match state {
                    STATE_CROSS => {
                        builder.push_line(Vec3::new(x0, y, z0), Vec3::new(x1, y, z1), col_region_cross);
                        builder.push_line(Vec3::new(x1, y, z0), Vec3::new(x0, y, z1), col_region_cross);
                    }
                    STATE_RED => {
                        let yl = y + 0.01;
                        builder.push_line(Vec3::new(x0, yl, z0), Vec3::new(x1, yl, z0), col_unloaded_border);
                        builder.push_line(Vec3::new(x1, yl, z0), Vec3::new(x1, yl, z1), col_unloaded_border);
                        builder.push_line(Vec3::new(x1, yl, z1), Vec3::new(x0, yl, z1), col_unloaded_border);
                        builder.push_line(Vec3::new(x0, yl, z1), Vec3::new(x0, yl, z0), col_unloaded_border);
                    }
                    _ => {}
                }
            }
        }

        // 3. 2D greedy-merge Gray (loaded) and Red (unloaded) fill quads for minimal GPU vertex count
        let mut visited = vec![false; width * depth];
        for gz in 0..depth {
            for gx in 0..width {
                let idx = gz * width + gx;
                if visited[idx] {
                    continue;
                }
                let st = states[idx];
                if st != STATE_GRAY && st != STATE_RED {
                    continue;
                }
                let mut w = 1usize;
                while gx + w < width && !visited[gz * width + gx + w] && states[gz * width + gx + w] == st {
                    w += 1;
                }
                let mut d = 1usize;
                'depth: while gz + d < depth {
                    let row = (gz + d) * width + gx;
                    for ox in 0..w {
                        if visited[row + ox] || states[row + ox] != st {
                            break 'depth;
                        }
                    }
                    d += 1;
                }
                for oz in 0..d {
                    let row = (gz + oz) * width + gx;
                    for ox in 0..w {
                        visited[row + ox] = true;
                    }
                }
                let x0 = ((min_cx + gx as i32) * 16) as f32;
                let x1 = ((min_cx + (gx + w) as i32) * 16) as f32;
                let z0 = ((min_cz + gz as i32) * 16) as f32;
                let z1 = ((min_cz + (gz + d) as i32) * 16) as f32;
                let col = if st == STATE_GRAY {
                    col_loaded_gray
                } else {
                    col_unloaded_red
                };
                builder.push_quad([[x0, y, z0], [x1, y, z0], [x1, y, z1], [x0, y, z1]], col);
            }
        }

        builder
    }

    /// Builds a horizon-spanning static world chunk grid at `y`:
    /// - Draws 16x16 empty square grid lines EVERYWHERE across `[grid_min_cx..=grid_max_cx, grid_min_cz..=grid_max_cz]`
    ///   (e.g. 4096x4096 chunks = 65,536x65,536 blocks in only ~16k line vertices!).
    /// - Inside each known 32x32 `.mca` region in `regions`:
    ///   - Missing chunk slot -> `X` cross inside square
    ///   - Existing & loaded chunk -> greedy-merged Gray fill
    ///   - Existing & unloaded chunk -> greedy-merged Red fill + red outline
    #[allow(clippy::too_many_arguments)]
    pub fn build_universal_chunk_grid<FExists, FLoaded>(
        grid_min_cx: i32,
        grid_max_cx: i32,
        grid_min_cz: i32,
        grid_max_cz: i32,
        y: f32,
        regions: &[(i32, i32)],
        chunk_exists: FExists,
        chunk_loaded: FLoaded,
    ) -> Self
    where
        FExists: Fn(ChunkPos) -> bool,
        FLoaded: Fn(ChunkPos) -> bool,
    {
        let mut builder = Self::default();
        if grid_max_cx < grid_min_cx || grid_max_cz < grid_min_cz {
            return builder;
        }

        const STATE_CROSS: u8 = 1;
        const STATE_GRAY: u8 = 2;
        const STATE_RED: u8 = 3;

        let col_grid_line = [0.46, 0.52, 0.62, 0.50];
        let col_region_cross = [0.54, 0.58, 0.68, 0.55];
        let col_loaded_gray = [0.52, 0.56, 0.62, 0.34];
        let col_unloaded_red = [0.88, 0.14, 0.14, 0.48];
        let col_unloaded_border = [1.0, 0.28, 0.28, 0.88];

        // 1. Universal 16x16 grid lines across the entire world range
        let x_min = (grid_min_cx * 16) as f32;
        let x_max = ((grid_max_cx + 1) * 16) as f32;
        let z_min = (grid_min_cz * 16) as f32;
        let z_max = ((grid_max_cz + 1) * 16) as f32;

        for cx in grid_min_cx..=(grid_max_cx + 1) {
            let x = (cx * 16) as f32;
            builder.push_line(Vec3::new(x, y, z_min), Vec3::new(x, y, z_max), col_grid_line);
        }
        for cz in grid_min_cz..=(grid_max_cz + 1) {
            let z = (cz * 16) as f32;
            builder.push_line(Vec3::new(x_min, y, z), Vec3::new(x_max, y, z), col_grid_line);
        }

        // 2. Populate X crosses, Gray fills, and Red fills inside each 32x32 .mca region,
        // clipped strictly to `[grid_min_cx..=grid_max_cx, grid_min_cz..=grid_max_cz]` (render distance)
        let mut states = [0u8; 32 * 32];
        let mut visited = [false; 32 * 32];

        for &(rx, rz) in regions {
            let base_cx = rx * 32;
            let base_cz = rz * 32;
            if base_cx + 31 < grid_min_cx
                || base_cx > grid_max_cx
                || base_cz + 31 < grid_min_cz
                || base_cz > grid_max_cz
            {
                continue;
            }
            states.fill(0);
            visited.fill(false);

            let lx_min = (grid_min_cx - base_cx).clamp(0, 32) as usize;
            let lx_max = (grid_max_cx + 1 - base_cx).clamp(0, 32) as usize;
            let lz_min = (grid_min_cz - base_cz).clamp(0, 32) as usize;
            let lz_max = (grid_max_cz + 1 - base_cz).clamp(0, 32) as usize;

            for lz in lz_min..lz_max {
                let cz = base_cz + lz as i32;
                let z0 = (cz * 16) as f32;
                let z1 = z0 + 16.0;
                for lx in lx_min..lx_max {
                    let cx = base_cx + lx as i32;
                    let cp = ChunkPos::new(cx, cz);
                    let st = if chunk_exists(cp) {
                        if chunk_loaded(cp) {
                            STATE_GRAY
                        } else {
                            STATE_RED
                        }
                    } else {
                        STATE_CROSS
                    };
                    states[lz * 32 + lx] = st;

                    let x0 = (cx * 16) as f32;
                    let x1 = x0 + 16.0;
                    match st {
                        STATE_CROSS => {
                            builder.push_line(Vec3::new(x0, y, z0), Vec3::new(x1, y, z1), col_region_cross);
                            builder.push_line(Vec3::new(x1, y, z0), Vec3::new(x0, y, z1), col_region_cross);
                        }
                        STATE_RED => {
                            let yl = y + 0.01;
                            builder.push_line(Vec3::new(x0, yl, z0), Vec3::new(x1, yl, z0), col_unloaded_border);
                            builder.push_line(Vec3::new(x1, yl, z0), Vec3::new(x1, yl, z1), col_unloaded_border);
                            builder.push_line(Vec3::new(x1, yl, z1), Vec3::new(x0, yl, z1), col_unloaded_border);
                            builder.push_line(Vec3::new(x0, yl, z1), Vec3::new(x0, yl, z0), col_unloaded_border);
                        }
                        _ => {}
                    }
                }
            }

            // Greedy-merge Gray and Red quads within the visible slice of this 32x32 region
            for lz in lz_min..lz_max {
                for lx in lx_min..lx_max {
                    let idx = lz * 32 + lx;
                    if visited[idx] {
                        continue;
                    }
                    let st = states[idx];
                    if st != STATE_GRAY && st != STATE_RED {
                        continue;
                    }
                    let mut w = 1usize;
                    while lx + w < 32 && !visited[lz * 32 + lx + w] && states[lz * 32 + lx + w] == st {
                        w += 1;
                    }
                    let mut d = 1usize;
                    'depth: while lz + d < 32 {
                        let row = (lz + d) * 32 + lx;
                        for ox in 0..w {
                            if visited[row + ox] || states[row + ox] != st {
                                break 'depth;
                            }
                        }
                        d += 1;
                    }
                    for oz in 0..d {
                        let row = (lz + oz) * 32 + lx;
                        for ox in 0..w {
                            visited[row + ox] = true;
                        }
                    }
                    let x0 = ((base_cx + lx as i32) * 16) as f32;
                    let x1 = ((base_cx + (lx + w) as i32) * 16) as f32;
                    let z0 = ((base_cz + lz as i32) * 16) as f32;
                    let z1 = ((base_cz + (lz + d) as i32) * 16) as f32;
                    let col = if st == STATE_GRAY {
                        col_loaded_gray
                    } else {
                        col_unloaded_red
                    };
                    builder.push_quad([[x0, y, z0], [x1, y, z0], [x1, y, z1], [x0, y, z1]], col);
                }
            }
        }

        builder
    }

    pub fn push_chunk_marker(&mut self, cp: ChunkPos, y_min: f32, y_max: f32, color: [f32; 4]) {
        let min = Vec3::new((cp.cx() * 16) as f32, y_min, (cp.cz() * 16) as f32);
        let max = Vec3::new((cp.cx() * 16 + 16) as f32, y_max, (cp.cz() * 16 + 16) as f32);
        self.push_filled_box(min, max, color);
        self.push_wire_box(min, max, [color[0], color[1], color[2], 0.85]);
    }
}

fn face_quad_corners(face: Face, min: [f32; 3], max: [f32; 3]) -> [[f32; 3]; 4] {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    match face {
        Face::East => [[x1, y1, z1], [x1, y1, z0], [x1, y0, z0], [x1, y0, z1]],
        Face::West => [[x0, y1, z0], [x0, y1, z1], [x0, y0, z1], [x0, y0, z0]],
        Face::Up => [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
        Face::Down => [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
        Face::South => [[x0, y1, z1], [x1, y1, z1], [x1, y0, z1], [x0, y0, z1]],
        Face::North => [[x1, y1, z0], [x0, y1, z0], [x0, y0, z0], [x1, y0, z0]],
    }
}
