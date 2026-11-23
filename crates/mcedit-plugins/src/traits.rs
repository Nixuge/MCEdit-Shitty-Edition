use crate::schema::{ParamDef, ParamMap};
use glam::IVec3;
use mcedit_core::{BoundingBox, DimensionId, EditTransaction, SchematicVolume, World};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Default)]
pub struct ProgressHandle {
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
}

impl ProgressHandle {
    pub fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            done: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
            cancelled,
        }
    }

    pub fn set(&self, done: u64, total: u64) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    pub fn progress(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

pub struct PluginContext<'a> {
    pub world: &'a mut World,
    pub dim: DimensionId,
    pub tx: &'a mut EditTransaction,
    pub progress: ProgressHandle,
    pub active_clipboard: Option<&'a SchematicVolume>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushShape {
    Round,
    Square,
    Diamond,
}

impl BrushShape {
    pub const ALL: [BrushShape; 3] = [BrushShape::Round, BrushShape::Square, BrushShape::Diamond];

    pub fn name(self) -> &'static str {
        match self {
            BrushShape::Round => "Round",
            BrushShape::Square => "Square",
            BrushShape::Diamond => "Diamond",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrushMaskOptions {
    pub width: i32,
    pub height: i32,
    pub length: i32,
    pub shape: BrushShape,
    pub hollow: bool,
    pub noise_percent: u8,
}

impl Default for BrushMaskOptions {
    fn default() -> Self {
        Self {
            width: 3,
            height: 3,
            length: 3,
            shape: BrushShape::Round,
            hollow: false,
            noise_percent: 100,
        }
    }
}

impl BrushMaskOptions {
    pub fn bounding_box(&self, center: IVec3) -> BoundingBox {
        let rx = self.width.max(1) / 2;
        let ry = self.height.max(1) / 2;
        let rz = self.length.max(1) / 2;
        let min = IVec3::new(center.x - rx, center.y - ry, center.z - rz);
        let max = min + IVec3::new(self.width.max(1), self.height.max(1), self.length.max(1));
        BoundingBox { min, max }
    }

    pub fn contains_offset(&self, dx: f32, dy: f32, dz: f32) -> bool {
        let rx = (self.width.max(1) as f32) * 0.5;
        let ry = (self.height.max(1) as f32) * 0.5;
        let rz = (self.length.max(1) as f32) * 0.5;
        let nx = dx / rx;
        let ny = dy / ry;
        let nz = dz / rz;
        match self.shape {
            BrushShape::Square => nx.abs() <= 1.0 && ny.abs() <= 1.0 && nz.abs() <= 1.0,
            BrushShape::Round => (nx * nx + ny * ny + nz * nz) <= 1.0,
            BrushShape::Diamond => (nx.abs() + ny.abs() + nz.abs()) <= 1.0,
        }
    }

    pub fn sample_offsets(&self, seed: u64) -> Vec<IVec3> {
        let w = self.width.max(1);
        let h = self.height.max(1);
        let l = self.length.max(1);
        let rx = w / 2;
        let ry = h / 2;
        let rz = l / 2;
        let mut inside = Vec::new();

        for y in -ry..(h - ry) {
            for z in -rz..(l - rz) {
                for x in -rx..(w - rx) {
                    let fx = x as f32 + if w % 2 == 0 { 0.5 } else { 0.0 };
                    let fy = y as f32 + if h % 2 == 0 { 0.5 } else { 0.0 };
                    let fz = z as f32 + if l % 2 == 0 { 0.5 } else { 0.0 };
                    if self.contains_offset(fx, fy, fz) {
                        inside.push(IVec3::new(x, y, z));
                    }
                }
            }
        }

        if self.hollow && inside.len() > 1 {
            use std::collections::HashSet;
            let set: HashSet<IVec3> = inside.iter().copied().collect();
            inside.retain(|&p| {
                !set.contains(&(p + IVec3::X))
                    || !set.contains(&(p - IVec3::X))
                    || !set.contains(&(p + IVec3::Y))
                    || !set.contains(&(p - IVec3::Y))
                    || !set.contains(&(p + IVec3::Z))
                    || !set.contains(&(p - IVec3::Z))
            });
        }

        if self.noise_percent < 100 {
            let thresh = self.noise_percent as u64;
            inside.retain(|p| {
                let h = hash_pos(seed, *p) % 100;
                h < thresh
            });
        }
        inside
    }
}

fn hash_pos(seed: u64, p: IVec3) -> u64 {
    let mut x = seed
        ^ (p.x as i64 as u64).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (p.y as i64 as u64).wrapping_mul(0xBF58476D1CE4E5B9)
        ^ (p.z as i64 as u64).wrapping_mul(0x94D049BB133111EB);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58476D1CE4E5B9);
    x ^= x >> 27;
    x
}

pub trait BrushPlugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn params(&self) -> Vec<ParamDef>;
    fn uses_shape(&self) -> bool {
        true
    }
    fn apply_point(
        &self,
        ctx: &mut PluginContext<'_>,
        center: IVec3,
        mask: &BrushMaskOptions,
        params: &ParamMap,
        stroke_seed: u64,
    ) -> Result<(), String>;
}

pub trait FilterPlugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn category(&self) -> &'static str;
    fn params(&self) -> Vec<ParamDef>;
    fn execute(
        &self,
        ctx: &mut PluginContext<'_>,
        selection: BoundingBox,
        params: &ParamMap,
    ) -> Result<(), String>;
}

pub struct PluginRegistry {
    pub brushes: Vec<Box<dyn BrushPlugin>>,
    pub filters: Vec<Box<dyn FilterPlugin>>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::with_stock_plugins()
    }
}

impl PluginRegistry {
    pub fn with_stock_plugins() -> Self {
        Self {
            brushes: crate::brushes::stock_brushes(),
            filters: crate::filters::stock_filters(),
        }
    }

    pub fn brush_by_id(&self, id: &str) -> Option<&dyn BrushPlugin> {
        self.brushes
            .iter()
            .find(|b| b.id() == id)
            .map(|b| b.as_ref())
    }

    pub fn filter_by_id(&self, id: &str) -> Option<&dyn FilterPlugin> {
        self.filters
            .iter()
            .find(|f| f.id() == id)
            .map(|f| f.as_ref())
    }
}
