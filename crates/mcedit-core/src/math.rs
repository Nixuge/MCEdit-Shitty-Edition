use glam::{IVec2, IVec3, Vec3};
use std::cmp::Ordering;
use std::ops::{Add, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkPos(pub IVec2);

impl PartialOrd for ChunkPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ChunkPos {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.0.x, self.0.y).cmp(&(other.0.x, other.0.y))
    }
}

impl ChunkPos {
    pub const fn new(cx: i32, cz: i32) -> Self {
        Self(IVec2::new(cx, cz))
    }

    pub const fn cx(self) -> i32 {
        self.0.x
    }

    pub const fn cz(self) -> i32 {
        self.0.y
    }

    pub fn region_pos(self) -> (i32, i32) {
        (self.0.x.div_euclid(32), self.0.y.div_euclid(32))
    }

    pub fn region_local(self) -> (usize, usize) {
        (
            self.0.x.rem_euclid(32) as usize,
            self.0.y.rem_euclid(32) as usize,
        )
    }

    pub const fn world_origin(self) -> IVec3 {
        IVec3::new(self.0.x * 16, 0, self.0.y * 16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockPos(pub IVec3);

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self(IVec3::new(x, y, z))
    }

    pub const fn chunk_pos(self) -> ChunkPos {
        ChunkPos::new(self.0.x.div_euclid(16), self.0.z.div_euclid(16))
    }

    pub const fn section_y(self) -> i32 {
        self.0.y.div_euclid(16)
    }

    pub const fn local_in_chunk(self) -> (usize, i32, usize) {
        (
            self.0.x.rem_euclid(16) as usize,
            self.0.y,
            self.0.z.rem_euclid(16) as usize,
        )
    }

    pub const fn local_in_section(self) -> (usize, usize, usize) {
        (
            self.0.x.rem_euclid(16) as usize,
            self.0.y.rem_euclid(16) as usize,
            self.0.z.rem_euclid(16) as usize,
        )
    }

    pub fn offset(self, face: Face) -> Self {
        Self(self.0 + face.normal())
    }
}

impl From<IVec3> for BlockPos {
    fn from(v: IVec3) -> Self {
        Self(v)
    }
}

impl Add<IVec3> for BlockPos {
    type Output = Self;
    fn add(self, rhs: IVec3) -> Self {
        Self(self.0 + rhs)
    }
}

impl Sub<IVec3> for BlockPos {
    type Output = Self;
    fn sub(self, rhs: IVec3) -> Self {
        Self(self.0 - rhs)
    }
}

/// Face directions matching legacy `pymclevel/faces.py` index ordering (0..6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Face {
    East = 0,  // +X (RIGHT)
    West = 1,  // -X (LEFT)
    Up = 2,    // +Y (TOP)
    Down = 3,  // -Y (BOTTOM)
    South = 4, // +Z (BACKWARD)
    North = 5, // -Z (FORWARD)
}

impl Face {
    pub const ALL: [Face; 6] = [
        Face::East,
        Face::West,
        Face::Up,
        Face::Down,
        Face::South,
        Face::North,
    ];

    pub const fn normal(self) -> IVec3 {
        match self {
            Face::East => IVec3::new(1, 0, 0),
            Face::West => IVec3::new(-1, 0, 0),
            Face::Up => IVec3::new(0, 1, 0),
            Face::Down => IVec3::new(0, -1, 0),
            Face::South => IVec3::new(0, 0, 1),
            Face::North => IVec3::new(0, 0, -1),
        }
    }

    pub const fn normal_f32(self) -> Vec3 {
        match self {
            Face::East => Vec3::X,
            Face::West => Vec3::NEG_X,
            Face::Up => Vec3::Y,
            Face::Down => Vec3::NEG_Y,
            Face::South => Vec3::Z,
            Face::North => Vec3::NEG_Z,
        }
    }

    pub const fn opposite(self) -> Self {
        match self {
            Face::East => Face::West,
            Face::West => Face::East,
            Face::Up => Face::Down,
            Face::Down => Face::Up,
            Face::South => Face::North,
            Face::North => Face::South,
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    /// Axis index: `0` = X (East/West), `1` = Y (Up/Down), `2` = Z (South/North).
    pub const fn axis(self) -> usize {
        match self {
            Face::East | Face::West => 0,
            Face::Up | Face::Down => 1,
            Face::South | Face::North => 2,
        }
    }

    /// True for the `+` side (`max` bound) of the axis (`East`, `Up`, `South`).
    pub const fn is_max_side(self) -> bool {
        matches!(self, Face::East | Face::Up | Face::South)
    }

    pub const fn from_axis_side(axis: usize, is_max: bool) -> Self {
        match (axis, is_max) {
            (0, true) => Face::East,
            (0, false) => Face::West,
            (1, true) => Face::Up,
            (1, false) => Face::Down,
            (_, true) => Face::South,
            (_, false) => Face::North,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Face::East => "east",
            Face::West => "west",
            Face::Up => "up",
            Face::Down => "down",
            Face::South => "south",
            Face::North => "north",
        }
    }
}

/// Half-open axis-aligned integer bounding box `[min, max)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundingBox {
    pub min: IVec3,
    pub max: IVec3,
}

impl BoundingBox {
    pub fn from_min_max(a: IVec3, b: IVec3) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    pub fn from_corners_inclusive(a: IVec3, b: IVec3) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b) + IVec3::ONE,
        }
    }

    pub fn from_origin_size(origin: IVec3, size: IVec3) -> Self {
        let end = origin + size;
        Self::from_min_max(origin, end)
    }

    pub fn size(self) -> IVec3 {
        (self.max - self.min).max(IVec3::ZERO)
    }

    pub fn is_empty(self) -> bool {
        self.min.x >= self.max.x || self.min.y >= self.max.y || self.min.z >= self.max.z
    }

    pub fn volume(self) -> u64 {
        let s = self.size();
        (s.x as u64) * (s.y as u64) * (s.z as u64)
    }

    pub fn contains(self, pos: IVec3) -> bool {
        pos.x >= self.min.x
            && pos.x < self.max.x
            && pos.y >= self.min.y
            && pos.y < self.max.y
            && pos.z >= self.min.z
            && pos.z < self.max.z
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let min = self.min.max(other.min);
        let max = self.max.min(other.max);
        let b = Self { min, max };
        if b.is_empty() { None } else { Some(b) }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    pub fn expand(self, amount: i32) -> Self {
        Self {
            min: self.min - IVec3::splat(amount),
            max: self.max + IVec3::splat(amount),
        }
    }

    pub fn shift(self, delta: IVec3) -> Self {
        Self {
            min: self.min + delta,
            max: self.max + delta,
        }
    }

    pub fn nudge_face(self, face: Face, delta: i32) -> Self {
        let mut out = self;
        match face {
            Face::East => out.max.x = (out.max.x + delta).max(out.min.x + 1),
            Face::West => out.min.x = (out.min.x + delta).min(out.max.x - 1),
            Face::Up => out.max.y = (out.max.y + delta).max(out.min.y + 1),
            Face::Down => out.min.y = (out.min.y + delta).min(out.max.y - 1),
            Face::South => out.max.z = (out.max.z + delta).max(out.min.z + 1),
            Face::North => out.min.z = (out.min.z + delta).min(out.max.z - 1),
        }
        out
    }

    pub fn chunk_bounds(self) -> (ChunkPos, ChunkPos) {
        if self.is_empty() {
            return (ChunkPos::new(0, 0), ChunkPos::new(-1, -1));
        }
        let min_c = ChunkPos::new(self.min.x.div_euclid(16), self.min.z.div_euclid(16));
        let max_c = ChunkPos::new(
            (self.max.x - 1).div_euclid(16),
            (self.max.z - 1).div_euclid(16),
        );
        (min_c, max_c)
    }

    pub fn iter_chunks(self) -> impl Iterator<Item = ChunkPos> {
        let (min_c, max_c) = self.chunk_bounds();
        (min_c.cx()..=max_c.cx())
            .flat_map(move |cx| (min_c.cz()..=max_c.cz()).map(move |cz| ChunkPos::new(cx, cz)))
    }

    pub fn chunk_box(pos: ChunkPos, min_y: i32, max_y: i32) -> Self {
        let o = pos.world_origin();
        Self {
            min: IVec3::new(o.x, min_y, o.z),
            max: IVec3::new(o.x + 16, max_y, o.z + 16),
        }
    }

    /// Ray-AABB slab intersection returning `(distance, hit_face)` for the nearest entry or exit face.
    pub fn ray_face_hit(self, origin: Vec3, dir: Vec3) -> Option<(f32, Face)> {
        let bmin = self.min.as_vec3();
        let bmax = self.max.as_vec3();

        let mut tmin = f32::NEG_INFINITY;
        let mut tmax = f32::INFINITY;
        let mut hit_face_min = Face::West;
        let mut hit_face_max = Face::East;

        for axis in 0..3 {
            let o = origin[axis];
            let d = dir[axis];
            let min_v = bmin[axis];
            let max_v = bmax[axis];

            if d.abs() < 1e-8 {
                if o < min_v || o > max_v {
                    return None;
                }
            } else {
                let inv_d = 1.0 / d;
                let mut t1 = (min_v - o) * inv_d;
                let mut t2 = (max_v - o) * inv_d;
                let (mut f1, mut f2) = match axis {
                    0 => (Face::West, Face::East),
                    1 => (Face::Down, Face::Up),
                    _ => (Face::North, Face::South),
                };
                if t1 > t2 {
                    std::mem::swap(&mut t1, &mut t2);
                    std::mem::swap(&mut f1, &mut f2);
                }
                if t1 > tmin {
                    tmin = t1;
                    hit_face_min = f1;
                }
                if t2 < tmax {
                    tmax = t2;
                    hit_face_max = f2;
                }
                if tmin > tmax {
                    return None;
                }
            }
        }

        if tmax < 0.0 {
            return None;
        }
        if tmin >= 0.0 {
            Some((tmin, hit_face_min))
        } else {
            Some((tmax, hit_face_max))
        }
    }

    /// Port of MCEdit-Unified's `EditorTool.boxFaceUnderCursor`:
    /// Intersects the cursor ray with all 6 box faces and picks the nearest face, EXCEPT when
    /// the hit point lies within `edge_factor` (0.1) of a silhouette edge whose adjacent face
    /// faces away from the camera — in which case it clicks through to the opposite/far face
    /// (allowing every face, including the bottom or back, to be grabbed from any camera angle).
    pub fn box_face_under_cursor(
        self,
        camera_pos: Vec3,
        mouse_ray_dir: Vec3,
        camera_forward: Vec3,
        edge_factor: f32,
    ) -> Option<(Face, Vec3)> {
        let bmin = self.min.as_vec3();
        let bmax = self.max.as_vec3();
        let size = bmax - bmin;

        let mut hits: Vec<(f32, Face, Vec3)> = Vec::with_capacity(6);
        for dim in 0..3 {
            let dim1 = (dim + 1) % 3;
            let dim2 = (dim + 2) % 3;
            let nd = mouse_ray_dir[dim];
            if nd.abs() <= 1e-8 {
                continue;
            }
            let neg = nd < 0.0;
            for (is_max, plane_coord) in [(true, bmax[dim]), (false, bmin[dim])] {
                let d = plane_coord - camera_pos[dim];
                if d >= 0.0 || (neg && d <= 0.0) {
                    let scale = d / nd;
                    if scale < 0.0 {
                        continue;
                    }
                    let pt = camera_pos + mouse_ray_dir * scale;
                    if pt[dim1] >= bmin[dim1]
                        && pt[dim1] <= bmax[dim1]
                        && pt[dim2] >= bmin[dim2]
                        && pt[dim2] <= bmax[dim2]
                    {
                        let dist_sq = (pt - camera_pos).length_squared();
                        hits.push((dist_sq, Face::from_axis_side(dim, is_max), pt));
                    }
                }
            }
        }

        if hits.is_empty() {
            return None;
        }

        let &(_, near_face, near_pt) = hits
            .iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))?;
        let &(_, far_face, far_pt) = hits
            .iter()
            .max_by(|a, b| a.0.total_cmp(&b.0))?;

        let dim = near_face.axis();
        let dim1 = (dim + 1) % 3;
        let dim2 = (dim + 2) % 3;

        for d in [dim1, dim2] {
            let edge_width = size[d] * edge_factor;
            let mut facenormal = Vec3::ZERO;
            let mut camera_behind = false;

            if near_pt[d] - bmin[d] < edge_width {
                facenormal[d] = -1.0;
                camera_behind = camera_pos[d] - bmin[d] > 0.0;
            }
            if near_pt[d] - bmax[d] > -edge_width {
                facenormal[d] = 1.0;
                camera_behind = camera_pos[d] - bmax[d] < 0.0;
            }

            if facenormal.dot(camera_forward) > 0.0 || camera_behind {
                return Some((far_face, far_pt));
            }
        }

        Some((near_face, near_pt))
    }
}
