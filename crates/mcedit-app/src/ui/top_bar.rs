use crate::editor::EditorState;
#[cfg(not(target_os = "macos"))]
use crate::ui::create_world::CreateWorldDraft;
#[cfg(not(target_os = "macos"))]
use crate::ui::world_info::WorldInfoDraft;
use egui::Ui;
use mcedit_render::{ViewMode, WorldRenderer};

pub fn draw(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    egui::Panel::top("top_menu_bar").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            // On macOS, File / Edit / View navigation lives in the native NSMenu bar.
            #[cfg(not(target_os = "macos"))]
            {
                draw_in_window_menus(ui, editor, renderer);
                ui.separator();
            }

            // Dimension switcher & View mode quick toggle
            if let Some(w) = &editor.world {
                let dims = w.dimensions.clone();
                let cur_dim = w.active_dim;
                let mut next_dim = cur_dim;
                egui::ComboBox::from_id_salt("dim_select")
                    .selected_text(cur_dim.display_name())
                    .show_ui(ui, |ui| {
                        for d in dims {
                            ui.selectable_value(&mut next_dim, d, d.display_name());
                        }
                    });
                if next_dim != cur_dim {
                    editor.switch_dimension(next_dim, renderer);
                }
            }

            let mode_label = match editor.camera.mode {
                ViewMode::Perspective3D => "Camera View",
                ViewMode::Chunk2D => "Chunk View (2D)",
            };
            if ui.button(mode_label).clicked() {
                editor.camera.mode = match editor.camera.mode {
                    ViewMode::Perspective3D => ViewMode::Chunk2D,
                    ViewMode::Chunk2D => ViewMode::Perspective3D,
                };
            }

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Render Dist:")
                    .on_hover_text("Max render distance in chunks (2..=1024)");
                if ui
                    .add(egui::DragValue::new(&mut editor.config.view_distance).range(2..=1024))
                    .changed()
                {
                    editor.config.save();
                }

                ui.label("LOD Start:")
                    .on_hover_text("Distance in chunks where distant LOD starts (0 = LOD everywhere)");
                if ui
                    .add(egui::DragValue::new(&mut editor.config.detail_distance).range(0..=128))
                    .changed()
                {
                    editor.config.save();
                }

                ui.label("Speed:")
                    .on_hover_text("Flight speed in blocks/sec (also adjustable with Scroll or [ / ])");
                if ui
                    .add(
                        egui::DragValue::new(&mut editor.config.camera_speed)
                            .range(2.0..=500.0)
                            .speed(1.0)
                            .suffix(" b/s"),
                    )
                    .changed()
                {
                    editor.camera.base_speed = editor.config.camera_speed;
                    editor.config.save();
                }
            });

            if let Some(w) = &editor.world
                && w.has_unsaved_changes()
            {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(255, 190, 60), "* Unsaved Edits");
            }
        });
    });
}

#[cfg(not(target_os = "macos"))]
fn draw_in_window_menus(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    ui.menu_button("File", |ui| {
        if ui.button("Open World...").clicked() {
            ui.close();
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                editor.open_world(&dir, renderer);
            }
        }
        if ui.button("Open level.dat...").clicked() {
            ui.close();
            if let Some(file) = rfd::FileDialog::new()
                .add_filter("level.dat", &["dat"])
                .pick_file()
            {
                editor.open_world(&file, renderer);
            }
        }
        if ui.button("Create New World...").clicked() {
            ui.close();
            editor.create_world = Some(CreateWorldDraft::default());
        }

        ui.menu_button("Recent Worlds", |ui| {
            let recents = editor.config.recent_worlds.clone();
            if recents.is_empty() {
                ui.label("(None)");
            } else {
                for p in recents {
                    if ui.button(p.display().to_string()).clicked() {
                        ui.close();
                        editor.open_world(&p, renderer);
                    }
                }
            }
        });

        ui.separator();
        let has_world = editor.world.is_some();
        if ui
            .add_enabled(
                has_world && !editor.jobs.is_busy(),
                egui::Button::new("Save (Ctrl+S)"),
            )
            .clicked()
        {
            ui.close();
            editor.save_world_async();
        }
        if ui
            .add_enabled(has_world, egui::Button::new("World Info..."))
            .clicked()
        {
            ui.close();
            if let Some(w) = &editor.world {
                editor.world_info = Some(WorldInfoDraft::from_world(w));
            }
        }
        if ui
            .add_enabled(has_world, egui::Button::new("Close World"))
            .clicked()
        {
            ui.close();
            editor.close_world(renderer);
        }
        ui.separator();
        if ui.button("Settings & Keybindings...").clicked() {
            ui.close();
            editor.settings_open = true;
        }
        ui.separator();
        if ui.button("Quit").clicked() {
            ui.close();
            if editor.world.as_ref().is_some_and(|w| w.has_unsaved_changes()) {
                editor.confirm_quit_open = true;
            } else {
                editor.should_exit = true;
            }
        }
    });

    ui.menu_button("Edit", |ui| {
        let can_undo = editor.world.as_ref().is_some_and(|w| w.undo_stack.can_undo());
        let can_redo = editor.world.as_ref().is_some_and(|w| w.undo_stack.can_redo());
        if ui
            .add_enabled(can_undo, egui::Button::new("Undo (Ctrl+Z)"))
            .clicked()
        {
            ui.close();
            editor.undo(renderer);
        }
        if ui
            .add_enabled(can_redo, egui::Button::new("Redo (Ctrl+Y)"))
            .clicked()
        {
            ui.close();
            editor.redo(renderer);
        }
        ui.separator();
        let has_sel = editor.selection.box_.is_some();
        if ui
            .add_enabled(has_sel, egui::Button::new("Copy (Ctrl+C)"))
            .clicked()
        {
            ui.close();
            editor.copy_selection();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Cut (Ctrl+X)"))
            .clicked()
        {
            ui.close();
            editor.cut_selection(renderer);
        }
        if ui
            .add_enabled(
                !editor.copy_stack.is_empty(),
                egui::Button::new("Paste (Ctrl+V)"),
            )
            .clicked()
        {
            ui.close();
            editor.paste_from_clipboard();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Delete Blocks"))
            .clicked()
        {
            ui.close();
            editor.delete_selected_blocks(renderer);
        }
        if ui.button("Deselect (Ctrl+D)").clicked() {
            ui.close();
            editor.selection.box_ = None;
            editor.selection.corner_a = None;
        }
    });

    ui.menu_button("View", |ui| {
        let mut is_2d = editor.camera.mode == ViewMode::Chunk2D;
        if ui.checkbox(&mut is_2d, "2D Chunk View (Tab)").changed() {
            editor.camera.mode = if is_2d {
                ViewMode::Chunk2D
            } else {
                ViewMode::Perspective3D
            };
        }
        ui.separator();
        let mut mesh_changed = false;
        mesh_changed |= ui.checkbox(&mut editor.config.fast_leaves, "Fast Leaves").changed();
        mesh_changed |= ui
            .checkbox(&mut editor.config.rough_graphics, "Rough Graphics")
            .changed();
        mesh_changed |= ui
            .checkbox(&mut editor.config.show_hidden_ores, "Show Hidden Ores")
            .changed();
        mesh_changed |= ui
            .checkbox(&mut editor.config.show_barriers, "Show Barrier Blocks (B)")
            .changed();
        if mesh_changed {
            mcedit_core::set_hide_barriers(!editor.config.show_barriers);
            editor.config.save();
            editor.invalidate_all_meshes(renderer);
        }
        ui.separator();
        if ui
            .checkbox(&mut editor.config.show_entities, "Show Entities")
            .changed()
        {
            editor.config.save();
        }
        if ui
            .checkbox(&mut editor.config.show_block_entities, "Show Block Entities")
            .changed()
        {
            editor.config.save();
        }
        if ui
            .checkbox(&mut editor.config.show_sign_text, "Show Sign Text")
            .changed()
        {
            editor.config.save();
        }
        if ui
            .checkbox(
                &mut editor.config.show_entity_labels,
                "Show Entity Names & Textures",
            )
            .changed()
        {
            editor.config.save();
        }
        ui.checkbox(&mut editor.config.show_chunk_borders, "Show Chunk Borders");
        if ui
            .checkbox(&mut editor.config.show_floor_grid, "Show World Chunk Grid")
            .changed()
        {
            editor.chunk_grid_dirty = true;
            editor.config.save();
        }
        ui.separator();
        if ui.button("Load Resource Pack...").clicked() {
            ui.close();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Resource Pack", &["zip"])
                .pick_file()
            {
                editor.config.resource_pack_path = Some(path.clone());
                editor.config.save();
                editor.pending_atlas_reload = Some(Some(path));
            }
        }
        if editor.config.resource_pack_path.is_some()
            && ui.button("Reset to Default Textures").clicked()
        {
            ui.close();
            editor.config.resource_pack_path = None;
            editor.config.save();
            editor.pending_atlas_reload = Some(None);
        }
    });
}
