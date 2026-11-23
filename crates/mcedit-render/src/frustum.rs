use glam::Vec4;

/// 6-plane frustum extracted from a `wgpu` (`z in [0, 1]`) view-projection matrix.
#[derive(Debug, Clone, Copy)]
pub struct Frustum {
    /// Plane `(nx, ny, nz, d)` where `n.dot(p) + d >= 0` is inside.
    planes: [Vec4; 6],
}

impl Frustum {
    pub fn from_view_proj(vp: glam::Mat4) -> Self {
        let r0 = vp.row(0);
        let r1 = vp.row(1);
        let r2 = vp.row(2);
        let r3 = vp.row(3);

        let raw = [
            r3 + r0, // Left
            r3 - r0, // Right
            r3 + r1, // Bottom
            r3 - r1, // Top
            r2,      // Near (wgpu z >= 0)
            r3 - r2, // Far  (wgpu z <= w)
        ];

        let mut planes = [Vec4::ZERO; 6];
        for (i, p) in raw.into_iter().enumerate() {
            let len = p.truncate().length();
            planes[i] = if len > 1e-8 { p / len } else { p };
        }
        Self { planes }
    }

    pub fn intersects_aabb(&self, min: glam::Vec3, max: glam::Vec3) -> bool {
        for &p in &self.planes {
            let n = p.truncate();
            // Positive vertex (furthest along plane normal)
            let pv = glam::Vec3::new(
                if n.x >= 0.0 { max.x } else { min.x },
                if n.y >= 0.0 { max.y } else { min.y },
                if n.z >= 0.0 { max.z } else { min.z },
            );
            if n.dot(pv) + p.w < 0.0 {
                return false;
            }
        }
        true
    }
}
