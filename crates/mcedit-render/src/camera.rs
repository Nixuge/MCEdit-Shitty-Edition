use glam::{Mat4, Vec2, Vec3, Vec4};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Perspective3D,
    Chunk2D,
}

#[derive(Debug, Clone, Default)]
pub struct CameraInputs {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub sprint: bool,
    pub brake: bool,
}

#[derive(Debug, Clone)]
pub struct CameraController {
    pub mode: ViewMode,
    pub pos: Vec3,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub velocity: Vec3,
    pub fov_deg: f32,
    /// Pixels per block in 2D Chunk View
    pub chunk_scale: f32,
    pub fly_mode: bool,
    pub autobrake: bool,
    pub base_speed: f32,
}

impl Default for CameraController {
    fn default() -> Self {
        Self {
            mode: ViewMode::Perspective3D,
            pos: Vec3::new(8.0, 72.0, 8.0),
            yaw_deg: 0.0,
            pitch_deg: -25.0,
            velocity: Vec3::ZERO,
            fov_deg: 70.0,
            chunk_scale: 1.0,
            fly_mode: false,
            autobrake: true,
            base_speed: 35.0,
        }
    }
}

impl CameraController {
    pub fn forward_vec(&self) -> Vec3 {
        let yaw = self.yaw_deg.to_radians();
        let pitch = self.pitch_deg.to_radians();
        Vec3::new(
            -yaw.sin() * pitch.cos(),
            pitch.sin(),
            -yaw.cos() * pitch.cos(),
        )
        .normalize()
    }

    pub fn right_vec(&self) -> Vec3 {
        let yaw = self.yaw_deg.to_radians();
        Vec3::new(yaw.cos(), 0.0, -yaw.sin()).normalize()
    }

    pub fn apply_mouse_delta(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        if self.mode == ViewMode::Perspective3D {
            self.yaw_deg = (self.yaw_deg - dx * sensitivity).rem_euclid(360.0);
            self.pitch_deg = (self.pitch_deg - dy * sensitivity).clamp(-89.9, 89.9);
        }
    }

    pub fn pan_2d(&mut self, dx_px: f32, dy_px: f32) {
        let scale = self.chunk_scale.max(0.05);
        self.pos.x -= dx_px / scale;
        self.pos.z -= dy_px / scale;
    }

    pub fn zoom_2d(&mut self, scroll_delta: f32, cursor_px: Vec2, viewport_size: Vec2) {
        if viewport_size.x <= 0.0 || viewport_size.y <= 0.0 {
            return;
        }
        let old_scale = self.chunk_scale;
        let factor = if scroll_delta > 0.0 { 1.25 } else { 0.8 };
        let new_scale = (old_scale * factor).clamp(1.0 / 16.0, 8.0);
        if (new_scale - old_scale).abs() < 1e-6 {
            return;
        }
        // Keep world point under cursor stationary
        let offset = cursor_px - viewport_size * 0.5;
        self.pos.x += offset.x * (1.0 / old_scale - 1.0 / new_scale);
        self.pos.z += offset.y * (1.0 / old_scale - 1.0 / new_scale);
        self.chunk_scale = new_scale;
    }

    pub fn update(&mut self, dt_secs: f32, inputs: &CameraInputs) {
        let dt = dt_secs.clamp(0.0, 0.125);
        let mut dir = Vec3::ZERO;

        match self.mode {
            ViewMode::Perspective3D => {
                let fwd = if self.fly_mode {
                    let yaw = self.yaw_deg.to_radians();
                    Vec3::new(-yaw.sin(), 0.0, -yaw.cos())
                } else {
                    self.forward_vec()
                };
                let right = self.right_vec();

                if inputs.forward {
                    dir += fwd;
                }
                if inputs.backward {
                    dir -= fwd;
                }
                if inputs.right {
                    dir += right;
                }
                if inputs.left {
                    dir -= right;
                }
                if inputs.up {
                    dir += Vec3::Y;
                }
                if inputs.down {
                    dir -= Vec3::Y;
                }
            }
            ViewMode::Chunk2D => {
                if inputs.forward {
                    dir -= Vec3::Z;
                }
                if inputs.backward {
                    dir += Vec3::Z;
                }
                if inputs.right {
                    dir += Vec3::X;
                }
                if inputs.left {
                    dir -= Vec3::X;
                }
            }
        }

        let mult = if inputs.sprint {
            3.0
        } else if self.mode == ViewMode::Chunk2D {
            2.0 / self.chunk_scale.max(0.1)
        } else {
            1.0
        };
        let target_speed = self.base_speed * mult;

        if dir.length_squared() > 0.0 {
            let accel = dir.normalize() * target_speed * 8.0;
            self.velocity += accel * dt;
            if self.velocity.length() > target_speed {
                self.velocity = self.velocity.normalize() * target_speed;
            }
        } else if self.autobrake {
            self.velocity *= (1.0 - 14.0 * dt).max(0.0);
        } else {
            self.velocity *= (1.0 - 4.0 * dt).max(0.0);
        }

        if inputs.brake {
            self.velocity *= 0.5;
        }

        self.pos += self.velocity * dt;
        self.pos.y = self.pos.y.clamp(-1000.0, 3800.0);
    }

    pub fn view_matrix(&self) -> Mat4 {
        match self.mode {
            ViewMode::Perspective3D => {
                let fwd = self.forward_vec();
                Mat4::look_to_rh(self.pos, fwd, Vec3::Y)
            }
            ViewMode::Chunk2D => {
                // Top-down looking along -Y, North (-Z) is up on screen
                let eye = Vec3::new(self.pos.x, 512.0, self.pos.z);
                Mat4::look_to_rh(eye, Vec3::NEG_Y, Vec3::NEG_Z)
            }
        }
    }

    pub fn proj_matrix(&self, width: f32, height: f32) -> Mat4 {
        let w = width.max(1.0);
        let h = height.max(1.0);
        match self.mode {
            ViewMode::Perspective3D => {
                let aspect = w / h;
                let fov_y = self.fov_deg.clamp(25.0, 120.0).to_radians();
                Mat4::perspective_rh(fov_y, aspect, 0.5, 65536.0)
            }
            ViewMode::Chunk2D => {
                let hw = 0.5 * w / self.chunk_scale.max(0.05);
                let hh = 0.5 * h / self.chunk_scale.max(0.05);
                Mat4::orthographic_rh(-hw, hw, -hh, hh, -2048.0, 2048.0)
            }
        }
    }

    pub fn view_proj(&self, width: f32, height: f32) -> Mat4 {
        self.proj_matrix(width, height) * self.view_matrix()
    }

    pub fn screen_ray(&self, cursor_px: Vec2, viewport_size: Vec2) -> (Vec3, Vec3) {
        let w = viewport_size.x.max(1.0);
        let h = viewport_size.y.max(1.0);
        let ndc_x = (2.0 * cursor_px.x / w) - 1.0;
        let ndc_y = 1.0 - (2.0 * cursor_px.y / h);

        let inv_vp = self.view_proj(w, h).inverse();
        let near = inv_vp * Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
        let far = inv_vp * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
        let p0 = near.truncate() / near.w;
        let p1 = far.truncate() / far.w;
        (p0, (p1 - p0).normalize())
    }
}
