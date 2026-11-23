use crate::editor::EditorState;
use egui::Context;
use glam::IVec3;
use mcedit_core::{UndoEntry, UndoKind, World};
use mcedit_render::WorldRenderer;

pub struct WorldInfoDraft {
    pub level_name: String,
    pub seed_str: String,
    pub time: i64,
    pub day_time: i64,
    pub game_type: i32,
    pub spawn_pos: IVec3,
    pub format_name: String,
    pub chunk_count: usize,
}

impl WorldInfoDraft {
    pub fn from_world(w: &World) -> Self {
        Self {
            level_name: w.metadata.level_name.clone(),
            seed_str: w.metadata.seed.to_string(),
            time: w.metadata.time,
            day_time: w.metadata.day_time,
            game_type: w.metadata.game_type,
            spawn_pos: w.metadata.spawn_pos,
            format_name: w.metadata.format_name.clone(),
            chunk_count: w.list_chunks(w.active_dim).len(),
        }
    }
}

pub fn draw(ctx: &Context, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    let Some(mut draft) = editor.world_info.take() else {
        return;
    };

    let mut close = false;
    let mut apply = false;

    egui::Window::new("World Info")
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.label(format!("Format: {}", draft.format_name));
            ui.label(format!("Active Dimension Chunks: {}", draft.chunk_count));
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("World Name:");
                ui.text_edit_singleline(&mut draft.level_name);
            });
            ui.horizontal(|ui| {
                ui.label("Seed:");
                ui.text_edit_singleline(&mut draft.seed_str);
            });

            let modes = [
                (0, "Survival"),
                (1, "Creative"),
                (2, "Adventure"),
                (3, "Spectator"),
            ];
            let mode_name = modes
                .iter()
                .find(|(id, _)| *id == draft.game_type)
                .map(|(_, n)| *n)
                .unwrap_or("Unknown");
            egui::ComboBox::from_label("Game Mode")
                .selected_text(mode_name)
                .show_ui(ui, |ui| {
                    for (id, name) in modes {
                        ui.selectable_value(&mut draft.game_type, id, name);
                    }
                });

            ui.horizontal(|ui| {
                ui.label("Time of Day (0-24000):");
                let mut tod = draft.day_time.rem_euclid(24000);
                if ui.add(egui::Slider::new(&mut tod, 0..=23999)).changed() {
                    let days = draft.day_time.div_euclid(24000);
                    draft.day_time = days * 24000 + tod;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Total Time:");
                ui.add(egui::DragValue::new(&mut draft.time));
            });

            ui.horizontal(|ui| {
                ui.label("Spawn XYZ:");
                ui.add(egui::DragValue::new(&mut draft.spawn_pos.x));
                ui.add(egui::DragValue::new(&mut draft.spawn_pos.y));
                ui.add(egui::DragValue::new(&mut draft.spawn_pos.z));
            });

            ui.separator();
            if ui.button("Repair Region Files (.mca)...").clicked()
                && let Some(w) = &editor.world
            {
                match w.format.repair_regions(w.active_dim) {
                    Ok(rep) => {
                        editor.set_toast(format!(
                            "Scanned {} region(s), repaired {}, recovered {} chunks.",
                            rep.files_scanned, rep.files_repaired, rep.chunks_recovered
                        ));
                        editor.invalidate_all_meshes(renderer);
                    }
                    Err(e) => editor.set_toast(format!("Repair error: {e}")),
                }
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if apply {
        if let Some(w) = &mut editor.world {
            let before = w.metadata.clone();
            w.metadata.level_name = draft.level_name;
            if let Ok(seed) = draft.seed_str.trim().parse::<i64>() {
                w.metadata.seed = seed;
            }
            w.metadata.time = draft.time;
            w.metadata.day_time = draft.day_time;
            w.metadata.game_type = draft.game_type;
            w.metadata.spawn_pos = draft.spawn_pos;
            w.metadata.sync_to_nbt();
            let after = w.metadata.clone();
            w.metadata_dirty = true;
            w.undo_stack.push(UndoEntry {
                label: "Edit World Info".into(),
                dirty_box: None,
                kind: UndoKind::Metadata { before, after },
            });
            editor.set_toast("Updated World Info.");
        }
    } else if !close {
        editor.world_info = Some(draft);
    }
}
