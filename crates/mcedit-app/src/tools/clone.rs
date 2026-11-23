use crate::editor::EditorState;
use egui::Ui;
use glam::{IVec3, UVec3};
use mcedit_core::{
    CopyOptions, EditTransaction, SchematicVolume, TransformOp, extract_volume, paste_volume,
    transform_volume,
};
use mcedit_render::WorldRenderer;

pub struct CloneToolState {
    pub active_volume: Option<SchematicVolume>,
    pub placement_origin: Option<IVec3>,
    pub copy_options: CopyOptions,
    pub blocks_only: bool,
    pub repeat_count: i32,
    pub scale_factor: f32,
    pub nudge_step: i32,
}

impl Default for CloneToolState {
    fn default() -> Self {
        Self {
            active_volume: None,
            placement_origin: None,
            copy_options: CopyOptions::default(),
            blocks_only: false,
            repeat_count: 1,
            scale_factor: 1.0,
            nudge_step: 1,
        }
    }
}

impl CloneToolState {
    pub fn load_volume(&mut self, vol: SchematicVolume, origin: IVec3) {
        self.active_volume = Some(vol);
        self.placement_origin = Some(origin);
    }
}

pub fn apply_transform(editor: &mut EditorState, op: TransformOp) {
    if let Some(vol) = &editor.clone.active_volume {
        let mut reg = editor.registry.write();
        let transformed = transform_volume(vol, op, &mut reg, editor.clone.blocks_only);
        editor.clone.active_volume = Some(transformed);
        editor.preview_dirty = true;
    }
}

pub fn handle_left_click(editor: &mut EditorState, block_pos: IVec3) {
    if editor.clone.active_volume.is_none()
        && let Some(b) = editor.selection.box_
        && let Some(w) = &mut editor.world
    {
        let dim = w.active_dim;
        let vol = extract_volume(w, dim, b);
        editor.clone.load_volume(vol, b.min);
        editor.preview_dirty = true;
        return;
    }

    if let Some(vol) = &editor.clone.active_volume {
        let centered = IVec3::new(
            block_pos.x - (vol.size.x as i32) / 2,
            block_pos.y + 1,
            block_pos.z - (vol.size.z as i32) / 2,
        );
        editor.clone.placement_origin = Some(centered);
        editor.preview_dirty = true;
    }
}

pub fn commit_clone(editor: &mut EditorState, renderer: &mut WorldRenderer) {
    let (Some(vol), Some(origin)) = (
        editor.clone.active_volume.as_ref(),
        editor.clone.placement_origin,
    ) else {
        return;
    };
    let scaled = if (editor.clone.scale_factor - 1.0).abs() > 0.01 {
        rescale_volume(vol, editor.clone.scale_factor)
    } else {
        vol.clone()
    };

    let Some(w) = &mut editor.world else {
        return;
    };
    let dim = w.active_dim;
    let mut tx = EditTransaction::new("Clone / Paste", dim, None);
    let repeat = editor.clone.repeat_count.clamp(1, 64);
    for i in 0..repeat {
        let offset = origin + IVec3::new(i * scaled.size.x as i32, 0, 0);
        paste_volume(w, dim, &scaled, offset, &editor.clone.copy_options, &mut tx);
    }
    let dirty_box = mcedit_core::BoundingBox::from_origin_size(
        origin,
        IVec3::new(
            scaled.size.x as i32 * repeat,
            scaled.size.y as i32,
            scaled.size.z as i32,
        ),
    );
    tx.commit(w);
    editor.invalidate_box(dirty_box, renderer);
    editor.set_toast("Placed cloned blocks.");
}

fn rescale_volume(src: &SchematicVolume, scale: f32) -> SchematicVolume {
    let s = scale.clamp(0.25, 4.0);
    let nw = ((src.size.x as f32 * s).round() as u32).max(1);
    let nh = ((src.size.y as f32 * s).round() as u32).max(1);
    let nl = ((src.size.z as f32 * s).round() as u32).max(1);
    let mut dst = SchematicVolume::new(UVec3::new(nw, nh, nl));
    for y in 0..nh {
        let sy = ((y as f32 / s) as u32).min(src.size.y - 1);
        for z in 0..nl {
            let sz = ((z as f32 / s) as u32).min(src.size.z - 1);
            for x in 0..nw {
                let sx = ((x as f32 / s) as u32).min(src.size.x - 1);
                dst.set_block(x, y, z, src.get_block(sx, sy, sz));
            }
        }
    }
    dst
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    ui.heading("Clone / Import");
    ui.separator();

    if editor.clone.active_volume.is_none() {
        if let Some(b) = editor.selection.box_ {
            if ui.button("Pick Up Selection").clicked()
                && let Some(w) = &mut editor.world
            {
                let dim = w.active_dim;
                let vol = extract_volume(w, dim, b);
                editor.clone.load_volume(vol, b.min);
                editor.preview_dirty = true;
            }
        } else {
            ui.label("Make a selection or Import a .schematic.");
        }
        if ui.button("Import .schematic...").clicked() {
            editor.open_import_dialog();
        }
        return;
    }

    let sz = editor.clone.active_volume.as_ref().unwrap().size;
    ui.label(format!("Volume: {} x {} x {}", sz.x, sz.y, sz.z));

    if let Some(origin) = &mut editor.clone.placement_origin {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Origin:");
            changed |= ui.add(egui::DragValue::new(&mut origin.x)).changed();
            changed |= ui.add(egui::DragValue::new(&mut origin.y)).changed();
            changed |= ui.add(egui::DragValue::new(&mut origin.z)).changed();
        });
        let step = editor.clone.nudge_step.max(1);
        ui.horizontal(|ui| {
            ui.label("Nudge:");
            ui.add(egui::DragValue::new(&mut editor.clone.nudge_step).range(1..=64));
            if ui.button("-X").clicked() { origin.x -= step; changed = true; }
            if ui.button("+X").clicked() { origin.x += step; changed = true; }
            if ui.button("-Y").clicked() { origin.y -= step; changed = true; }
            if ui.button("+Y").clicked() { origin.y += step; changed = true; }
            if ui.button("-Z").clicked() { origin.z -= step; changed = true; }
            if ui.button("+Z").clicked() { origin.z += step; changed = true; }
        });
        if changed {
            editor.preview_dirty = true;
        }
    }

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Rotate CW (E)").clicked() {
            apply_transform(editor, TransformOp::RotateCW);
        }
        if ui.button("Rotate CCW").clicked() {
            apply_transform(editor, TransformOp::RotateCCW);
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Roll (R)").clicked() {
            apply_transform(editor, TransformOp::Roll);
        }
        if ui.button("Flip X (F)").clicked() {
            apply_transform(editor, TransformOp::FlipX);
        }
        if ui.button("Flip Y").clicked() {
            apply_transform(editor, TransformOp::FlipY);
        }
        if ui.button("Mirror Z (G)").clicked() {
            apply_transform(editor, TransformOp::FlipZ);
        }
    });

    ui.separator();
    ui.checkbox(&mut editor.clone.copy_options.copy_air, "Copy Air");
    ui.checkbox(&mut editor.clone.copy_options.copy_water, "Copy Liquids");
    ui.checkbox(&mut editor.clone.copy_options.copy_biomes, "Copy Biomes");
    ui.checkbox(&mut editor.clone.copy_options.update_spawner_coords, "Offset Spawner Coords");
    ui.checkbox(&mut editor.clone.copy_options.regenerate_entity_uuids, "Regenerate Entity UUIDs");
    ui.checkbox(&mut editor.clone.blocks_only, "Transform Blocks Only");

    ui.horizontal(|ui| {
        ui.label("Repeat X:");
        ui.add(egui::DragValue::new(&mut editor.clone.repeat_count).range(1..=32));
        ui.label("Scale:");
        ui.add(egui::DragValue::new(&mut editor.clone.scale_factor).speed(0.05).range(0.25..=4.0));
    });

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Clone! (Place)").clicked() {
            commit_clone(editor, renderer);
        }
        if ui.button("Clear").clicked() {
            editor.clone.active_volume = None;
            editor.clone.placement_origin = None;
            editor.preview_dirty = true;
        }
    });
}
