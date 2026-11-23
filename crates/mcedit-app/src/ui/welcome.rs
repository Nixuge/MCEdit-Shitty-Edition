use crate::editor::EditorState;
use crate::ui::create_world::CreateWorldDraft;
use egui::Ui;
use mcedit_render::WorldRenderer;

pub fn draw(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    if editor.world.is_some() || editor.jobs.is_busy() {
        return;
    }

    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("MCEdit-rs (MCEdit-Unified Rewrite)");
            ui.label("Modern Rust + wgpu + egui Minecraft World Editor (Java Pre-1.13 Anvil & .schematic)");
            ui.add_space(24.0);

            if ui
                .button(egui::RichText::new("Open World Folder...").size(18.0))
                .clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                editor.open_world(&dir, renderer);
            }
            ui.add_space(8.0);
            if ui
                .button(egui::RichText::new("Open level.dat...").size(16.0))
                .clicked()
                && let Some(file) = rfd::FileDialog::new()
                    .add_filter("level.dat", &["dat"])
                    .pick_file()
            {
                editor.open_world(&file, renderer);
            }
            ui.add_space(8.0);
            if ui
                .button(egui::RichText::new("Create New World...").size(16.0))
                .clicked()
            {
                editor.create_world = Some(CreateWorldDraft::default());
            }

            if !editor.config.recent_worlds.is_empty() {
                ui.add_space(28.0);
                ui.heading("Recent Worlds");
                ui.add_space(8.0);
                let recents = editor.config.recent_worlds.clone();
                for path in recents {
                    if ui.button(path.display().to_string()).clicked() {
                        editor.open_world(&path, renderer);
                    }
                }
            }
        });
    });
}
