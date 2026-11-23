use crate::editor::EditorState;
use crate::ui::param_ui;
use egui::Ui;
use glam::{IVec3, UVec3};
use mcedit_core::{BlockRegistry, BlockState, EditTransaction, SchematicVolume};
use mcedit_plugins::{
    BrushMaskOptions, BrushShape, ParamMap, PluginContext, PluginRegistry, ProgressHandle,
    bresenham_3d, default_param_map, schema::get_block,
};
use mcedit_render::WorldRenderer;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub struct BrushToolState {
    pub selected_idx: usize,
    pub mask: BrushMaskOptions,
    pub params_by_id: HashMap<String, ParamMap>,
    pub stroke_seed: u64,
    pub last_stroke_pos: Option<IVec3>,
    pub is_painting: bool,
    pub preview_offset: IVec3,
}

impl BrushToolState {
    pub fn new(plugins: &PluginRegistry) -> Self {
        let mut params_by_id = HashMap::new();
        for b in &plugins.brushes {
            params_by_id.insert(b.id().to_string(), default_param_map(&b.params()));
        }
        Self {
            selected_idx: 0,
            mask: BrushMaskOptions::default(),
            params_by_id,
            stroke_seed: 1,
            last_stroke_pos: None,
            is_painting: false,
            preview_offset: IVec3::ZERO,
        }
    }

    pub fn selected_mode_id<'a>(&self, plugins: &'a PluginRegistry) -> &'a str {
        plugins
            .brushes
            .get(self.selected_idx)
            .map(|b| b.id())
            .unwrap_or("fill")
    }

    pub fn build_preview_volume(
        &self,
        plugins: &PluginRegistry,
        reg: &BlockRegistry,
    ) -> Option<(SchematicVolume, IVec3)> {
        let b = plugins.brushes.get(self.selected_idx)?;
        if !b.uses_shape() {
            return None;
        }
        let params = self.params_by_id.get(b.id())?;
        let state = get_block(params, "block")
            .or_else(|| get_block(params, "block_a"))
            .map(|(s, _)| s.clone())
            .unwrap_or_else(|| BlockState::new("minecraft:stone"));
        let bid = reg
            .find(&state)
            .or_else(|| reg.find_by_name(&state.name))
            .or_else(|| reg.find_by_legacy(1, 0))
            .unwrap_or(mcedit_core::BlockStateId(1));

        let w = self.mask.width.clamp(1, 32) as u32;
        let h = self.mask.height.clamp(1, 32) as u32;
        let l = self.mask.length.clamp(1, 32) as u32;
        let rx = w as i32 / 2;
        let ry = h as i32 / 2;
        let rz = l as i32 / 2;
        let mut vol = SchematicVolume::new(UVec3::new(w, h, l));
        for off in self.mask.sample_offsets(self.stroke_seed) {
            let x = off.x + rx;
            let y = off.y + ry;
            let z = off.z + rz;
            if x >= 0 && y >= 0 && z >= 0 {
                vol.set_block(x as u32, y as u32, z as u32, bid);
            }
        }
        Some((vol, IVec3::new(-rx, -ry, -rz)))
    }
}

pub fn handle_paint_at(
    editor: &mut EditorState,
    center: IVec3,
    is_drag: bool,
    renderer: &mut WorldRenderer,
) {
    let points = if is_drag {
        if let Some(prev) = editor.brush.last_stroke_pos {
            if prev == center {
                return;
            }
            bresenham_3d(prev, center)
        } else {
            vec![center]
        }
    } else {
        editor.brush.stroke_seed = editor.brush.stroke_seed.wrapping_add(1);
        vec![center]
    };
    editor.brush.last_stroke_pos = Some(center);

    let mode_idx = editor.brush.selected_idx;
    let Some(brush) = editor.plugins.brushes.get(mode_idx) else {
        return;
    };
    let mode_id = brush.id().to_string();
    let params = editor
        .brush
        .params_by_id
        .get(&mode_id)
        .cloned()
        .unwrap_or_default();
    let mask = editor.brush.mask.clone();
    let seed = editor.brush.stroke_seed;
    let clipboard = editor.copy_stack.first().map(|e| e.volume.clone());

    let Some(world) = &mut editor.world else {
        return;
    };
    let dim = world.active_dim;
    let mut tx = EditTransaction::new(format!("Brush: {}", brush.display_name()), dim, None);
    let mut total_box = None;

    for pt in points {
        let mut ctx = PluginContext {
            world,
            dim,
            tx: &mut tx,
            progress: ProgressHandle::new(Arc::new(AtomicBool::new(false))),
            active_clipboard: clipboard.as_ref(),
            message: None,
        };
        if let Err(err) = brush.apply_point(&mut ctx, pt, &mask, &params, seed) {
            editor.set_toast(err);
            return;
        }
        let b = mask.bounding_box(pt);
        total_box = Some(match total_box {
            Some(prev) => mcedit_core::BoundingBox::union(prev, b),
            None => b,
        });
    }
    tx.commit(world);
    if let Some(b) = total_box {
        editor.invalidate_box(b, renderer);
    }
}

pub fn end_stroke(editor: &mut EditorState) {
    editor.brush.is_painting = false;
    editor.brush.last_stroke_pos = None;
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState) {
    ui.heading("Brush Tool");
    ui.separator();

    let current_name = editor
        .plugins
        .brushes
        .get(editor.brush.selected_idx)
        .map(|b| b.display_name())
        .unwrap_or("Fill");
    egui::ComboBox::from_label("Mode")
        .selected_text(current_name)
        .show_ui(ui, |ui| {
            for (i, b) in editor.plugins.brushes.iter().enumerate() {
                if ui
                    .selectable_value(&mut editor.brush.selected_idx, i, b.display_name())
                    .clicked()
                {
                    editor.preview_dirty = true;
                }
            }
        });

    let uses_shape = editor
        .plugins
        .brushes
        .get(editor.brush.selected_idx)
        .is_some_and(|b| b.uses_shape());

    if uses_shape {
        let mut shape_changed = false;
        egui::ComboBox::from_label("Shape")
            .selected_text(editor.brush.mask.shape.name())
            .show_ui(ui, |ui| {
                for s in BrushShape::ALL {
                    shape_changed |= ui
                        .selectable_value(&mut editor.brush.mask.shape, s, s.name())
                        .changed();
                }
            });
        ui.horizontal(|ui| {
            ui.label("W:");
            shape_changed |= ui
                .add(egui::DragValue::new(&mut editor.brush.mask.width).range(1..=32))
                .changed();
            ui.label("H:");
            shape_changed |= ui
                .add(egui::DragValue::new(&mut editor.brush.mask.height).range(1..=32))
                .changed();
            ui.label("L:");
            shape_changed |= ui
                .add(egui::DragValue::new(&mut editor.brush.mask.length).range(1..=32))
                .changed();
        });
        shape_changed |= ui.checkbox(&mut editor.brush.mask.hollow, "Hollow").changed();
        let mut noise = editor.brush.mask.noise_percent as i32;
        if ui
            .add(egui::Slider::new(&mut noise, 1..=100).text("Noise %"))
            .changed()
        {
            editor.brush.mask.noise_percent = noise as u8;
            shape_changed = true;
        }
        if shape_changed {
            editor.preview_dirty = true;
        }
    }

    ui.separator();
    if let Some(brush) = editor.plugins.brushes.get(editor.brush.selected_idx) {
        let bid = brush.id().to_string();
        let defs = brush.params();
        let params = editor.brush.params_by_id.entry(bid.clone()).or_default();
        if let Some(pick_key) = param_ui::draw_param_form(ui, &defs, params) {
            let current = match params.get(&pick_key) {
                Some(mcedit_plugins::ParamValue::Block { state, match_base_only }) => {
                    (state.clone(), *match_base_only)
                }
                _ => (BlockState::new("minecraft:stone"), false),
            };
            let allow_wild = defs.iter().any(|d| {
                d.key == pick_key
                    && matches!(d.kind, mcedit_plugins::ParamKind::Block { allow_wildcard: true, .. })
            });
            editor.block_picker = Some(crate::ui::block_picker::BlockPickerModal::new(
                crate::ui::block_picker::BlockPickerTarget::BrushParam {
                    brush_id: bid,
                    key: pick_key,
                },
                current.0,
                allow_wild,
                current.1,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use mcedit_render::mesh_volume_preview;

    #[test]
    fn brush_preview_builds_without_deadlock_when_holding_registry_read_lock() {
        let editor = EditorState::new(AppConfig::default());
        let reg = editor.registry.read();
        let (vol, off) = editor
            .brush
            .build_preview_volume(&editor.plugins, &reg)
            .expect("default Fill brush must produce a preview volume");
        let mesh = mesh_volume_preview(&vol, &reg, 160);
        assert!(!mesh.translucent.is_empty());
        assert!(off.x <= 0 && off.y <= 0 && off.z <= 0);
    }
}
