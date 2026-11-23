use crate::editor::EditorState;
use egui::Context;
use glam::IVec3;
use mcedit_core::{
    ChunkPos, DimensionId, LightingEngine, World, create_flatland_chunk, default_flatland_layers,
};
use mcedit_formats::{AnvilPost113Format, AnvilPre113Format, DEFAULT_MODERN_DATA_VERSION};
use mcedit_render::WorldRenderer;
use std::path::PathBuf;

pub struct CreateWorldDraft {
    pub folder_path: Option<PathBuf>,
    pub level_name: String,
    pub seed_str: String,
    pub game_type: i32,
    pub chunk_radius: i32,
    pub spawn_pos: IVec3,
    pub post_113_modern: bool,
}

impl Default for CreateWorldDraft {
    fn default() -> Self {
        Self {
            folder_path: None,
            level_name: "New MCEdit World".to_string(),
            seed_str: "12345".to_string(),
            game_type: 1,
            chunk_radius: 2,
            spawn_pos: IVec3::new(0, 4, 0),
            post_113_modern: false,
        }
    }
}

pub fn draw(ctx: &Context, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    let Some(mut draft) = editor.create_world.take() else {
        return;
    };

    let mut close = false;
    let mut create = false;

    egui::Window::new("Create New World")
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Format:");
                ui.selectable_value(
                    &mut draft.post_113_modern,
                    false,
                    "Java 1.12.2 (Pre-1.13, Y: 0..256)",
                );
                ui.selectable_value(
                    &mut draft.post_113_modern,
                    true,
                    "Java 1.20+ (Post-1.13, Y: -64..320)",
                );
            });
            ui.horizontal(|ui| {
                ui.label("Folder:");
                let path_lbl = draft
                    .folder_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(Choose directory...)".to_string());
                ui.label(path_lbl);
                if ui.button("Browse...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    draft.folder_path = Some(dir);
                }
            });

            ui.horizontal(|ui| {
                ui.label("World Name:");
                ui.text_edit_singleline(&mut draft.level_name);
            });
            ui.horizontal(|ui| {
                ui.label("Seed:");
                ui.text_edit_singleline(&mut draft.seed_str);
            });
            ui.horizontal(|ui| {
                ui.label("Chunk Radius:");
                ui.add(egui::DragValue::new(&mut draft.chunk_radius).range(0..=8));
            });

            let modes = [(0, "Survival"), (1, "Creative")];
            let cur_mode = if draft.game_type == 0 { "Survival" } else { "Creative" };
            egui::ComboBox::from_label("Game Mode")
                .selected_text(cur_mode)
                .show_ui(ui, |ui| {
                    for (id, n) in modes {
                        ui.selectable_value(&mut draft.game_type, id, n);
                    }
                });

            ui.separator();
            ui.horizontal(|ui| {
                let can_create = draft.folder_path.is_some();
                if ui
                    .add_enabled(can_create, egui::Button::new("Create & Open"))
                    .clicked()
                {
                    create = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if create {
        if let Some(dir) = draft.folder_path {
            let seed = draft.seed_str.trim().parse::<i64>().unwrap_or(0);
            let reg = editor.registry.clone();
            let fmt_res: Result<Box<dyn mcedit_core::WorldFormat>, _> = if draft.post_113_modern {
                AnvilPost113Format::create_new(
                    &dir,
                    &draft.level_name,
                    seed,
                    draft.game_type,
                    draft.spawn_pos,
                    DEFAULT_MODERN_DATA_VERSION,
                    &reg.read(),
                )
                .map(|f| Box::new(f) as Box<dyn mcedit_core::WorldFormat>)
            } else {
                AnvilPre113Format::create_new(
                    &dir,
                    &draft.level_name,
                    seed,
                    draft.game_type,
                    draft.spawn_pos,
                    &reg.read(),
                )
                .map(|f| Box::new(f) as Box<dyn mcedit_core::WorldFormat>)
            };
            match fmt_res {
                Ok(fmt) => {
                    let min_sy = fmt.min_section_y();
                    let max_sy = fmt.max_section_y();
                    match World::open(fmt, reg.clone(), editor.config.undo_limit) {
                        Ok(mut world) => {
                            let layers = default_flatland_layers();
                            let r = draft.chunk_radius.clamp(0, 8);
                            let mut created = Vec::new();
                            for cz in -r..=r {
                                for cx in -r..=r {
                                    let cp = ChunkPos::new(cx, cz);
                                    let chunk = {
                                        let mut rw = reg.write();
                                        create_flatland_chunk(cp, min_sy, max_sy, &layers, &mut rw)
                                    };
                                    world.insert_chunk(DimensionId::Overworld, chunk);
                                    created.push(cp);
                                }
                            }
                            LightingEngine::relight_chunks(&mut world, DimensionId::Overworld, &created);
                            let _ = world.save_all();
                            editor.install_world(world, Some(dir), renderer);
                        }
                        Err(e) => editor.set_toast(format!("Failed to open created world: {e}")),
                    }
                }
                Err(e) => editor.set_toast(format!("Failed to create world: {e}")),
            }
        }
    } else if !close {
        editor.create_world = Some(draft);
    }
}
