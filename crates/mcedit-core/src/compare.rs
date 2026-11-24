use crate::block::{BlockRegistry, BlockStateId};

/// Per-block diff classification between World A (active) and World B (comparison reference).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffKind {
    /// Both worlds have the same block (or both are air).
    Match = 0,
    /// Both worlds have a non-air block, and they differ.
    Mismatch = 1,
    /// Non-air in World A, air or missing chunk in World B.
    OnlyInA = 2,
    /// Air or missing chunk in World A, non-air in World B.
    OnlyInB = 3,
}

/// Viewport display mode while a comparison world is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CompareViewMode {
    /// Normal diff view: matching blocks normal, mismatches red, one-sided blocks gray.
    #[default]
    Diff,
    /// Temporarily view World A normally without unloading World B.
    OnlyWorldA,
    /// Temporarily view World B normally without unloading World A.
    OnlyWorldB,
}

/// Configuration controlling how World A and World B are compared and rendered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompareConfig {
    pub enabled: bool,
    pub view_mode: CompareViewMode,
    /// Show semi-transparent red overlay on blocks that differ between A and B.
    pub show_mismatch: bool,
    /// Show semi-transparent light-gray overlay on blocks only present in World A.
    pub show_only_a: bool,
    /// Show semi-transparent darker slate-gray overlay on blocks only present in World B.
    pub show_only_b: bool,
    /// When true, matching blocks are hidden (Diff-Only X-Ray mode).
    pub hide_matching: bool,
    /// When true, compare base block name only (ignoring rotation, slab half, waterlogged, etc.).
    pub ignore_properties: bool,
    /// Tint strength of the red/gray overlay over the underlying block texture (`0.0..=1.0`).
    pub overlay_strength: f32,
    /// Surface opacity of Only-in-A blocks (`0.25..=1.0`).
    pub only_a_alpha: f32,
    /// Surface opacity of Only-in-B ghost blocks (`0.15..=1.0`).
    pub only_b_alpha: f32,
    /// Optional chunk offset applied when sampling World B (`cp_b = cp_a + chunk_offset`).
    pub chunk_offset: (i32, i32),
}

impl Default for CompareConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            view_mode: CompareViewMode::Diff,
            show_mismatch: true,
            show_only_a: true,
            show_only_b: true,
            hide_matching: false,
            ignore_properties: false,
            overlay_strength: 0.55,
            only_a_alpha: 0.85,
            only_b_alpha: 0.62,
            chunk_offset: (0, 0),
        }
    }
}

impl CompareConfig {
    /// Pack live shader uniform parameters `[overlay_strength, only_a_alpha, only_b_alpha, 0.0]`.
    #[inline]
    pub fn shader_params(&self) -> [f32; 4] {
        [
            self.overlay_strength.clamp(0.0, 1.0),
            self.only_a_alpha.clamp(0.15, 1.0),
            self.only_b_alpha.clamp(0.10, 1.0),
            0.0,
        ]
    }
}

/// Classifies a single voxel `(a, b)` into `DiffKind`, treating air and hidden barriers as empty.
#[inline]
pub fn classify_block_diff(
    a: BlockStateId,
    b: BlockStateId,
    reg: &BlockRegistry,
    ignore_properties: bool,
    hide_barriers: bool,
) -> DiffKind {
    let a_empty = is_effective_air(a, reg, hide_barriers);
    let b_empty = is_effective_air(b, reg, hide_barriers);
    match (a_empty, b_empty) {
        (true, true) => DiffKind::Match,
        (false, true) => DiffKind::OnlyInA,
        (true, false) => DiffKind::OnlyInB,
        (false, false) => {
            if reg.matches(a, b, ignore_properties) {
                DiffKind::Match
            } else {
                DiffKind::Mismatch
            }
        }
    }
}

#[inline]
pub fn is_effective_air(id: BlockStateId, reg: &BlockRegistry, hide_barriers: bool) -> bool {
    if id == BlockStateId::AIR {
        return true;
    }
    let meta = reg.get_meta(id);
    meta.is_air || (hide_barriers && meta.is_barrier)
}
