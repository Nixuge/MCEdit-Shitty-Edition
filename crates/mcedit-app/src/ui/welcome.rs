use crate::config::AppConfig;
use crate::editor::EditorState;
use crate::ui::create_world::CreateWorldDraft;
use egui::Ui;
use mcedit_render::WorldRenderer;

pub fn draw(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    if editor.world.is_some() || editor.jobs.is_busy() {
        return;
    }
    if editor.browser_dirty {
        editor.refresh_browser_entries();
    }

    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(36.0);
            ui.heading("MCEdit-rs (MCEdit-Unified Rewrite)");
            ui.label(
                "Modern Rust + wgpu + egui Minecraft World Editor (Java Anvil & .schematic)",
            );
            ui.add_space(18.0);

            ui.horizontal(|ui| {
                let total_w = ui.available_width();
                let btn_row_w = 460.0_f32.min(total_w);
                ui.add_space(((total_w - btn_row_w) * 0.5).max(0.0));
                if ui
                    .button(egui::RichText::new("Open World Folder...").size(16.0))
                    .clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_directory(&editor.browser_dir)
                        .pick_folder()
                {
                    editor.open_world(&dir, renderer);
                }
                if ui
                    .button(egui::RichText::new("Open level.dat...").size(16.0))
                    .clicked()
                    && let Some(file) = rfd::FileDialog::new()
                        .set_directory(&editor.browser_dir)
                        .add_filter("level.dat", &["dat"])
                        .pick_file()
                {
                    editor.open_world(&file, renderer);
                }
                if ui
                    .button(egui::RichText::new("Create New World...").size(16.0))
                    .clicked()
                {
                    editor.create_world = Some(CreateWorldDraft::default());
                }
            });

            ui.add_space(22.0);
        });

        let avail_w = ui.available_width();
        let container_w = 920.0_f32.min(avail_w - 32.0).max(320.0);
        let side_pad = ((avail_w - container_w) * 0.5).max(0.0);
        
        let avail_h = (ui.available_height() - 24.0).max(200.0);
        
        ui.horizontal(|ui| {
            ui.add_space(side_pad);
            
            ui.allocate_ui(egui::vec2(container_w, avail_h), |ui| {
                let col_w = ((container_w - 20.0) * 0.5).max(200.0);
                let has_current_world = editor.browser_dir.join("level.dat").is_file();
                let browser_header_h = if has_current_world { 114.0 } else { 86.0 };
                let browser_list_h = (avail_h - browser_header_h).max(160.0);
                let recent_list_h = (avail_h - 36.0).max(160.0);

                ui.columns(2, |cols| {
                    // Left column: Simple persistent Folder Browser
                    cols[0].set_max_width(col_w);
                    cols[0].heading("Browse Worlds");
                    cols[0].add_space(4.0);

                    cols[0].horizontal(|ui| {
                        if ui.button("Up").clicked()
                            && let Some(parent) = editor.browser_dir.parent().map(|p| p.to_path_buf())
                        {
                            editor.set_browser_dir(parent);
                        }
                        if let Some(saves) = AppConfig::minecraft_saves_dir()
                            && ui.button("MC Saves").clicked()
                        {
                            editor.set_browser_dir(saves);
                        }
                        if let Some(base) = directories::BaseDirs::new()
                            && ui.button("Home").clicked()
                        {
                            editor.set_browser_dir(base.home_dir().to_path_buf());
                        }
                        if ui.button("Refresh").clicked() {
                            editor.browser_dirty = true;
                        }
                    });

                    cols[0].label(
                        egui::RichText::new(editor.browser_dir.display().to_string())
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                    cols[0].add_space(4.0);

                    if has_current_world {
                        let name = editor
                            .browser_dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Current Folder");
                        if cols[0]
                            .button(
                                egui::RichText::new(format!("Open World: {name}"))
                                    .strong()
                                    .color(egui::Color32::from_rgb(130, 220, 140)),
                            )
                            .clicked()
                        {
                            let dir = editor.browser_dir.clone();
                            editor.open_world(&dir, renderer);
                        }
                        cols[0].add_space(4.0);
                    }

                    let entries = editor.browser_entries.clone();
                    egui::ScrollArea::vertical()
                        .id_salt("welcome_folder_browser")
                        .max_height(browser_list_h)
                        .auto_shrink([false, false])
                        .show(&mut cols[0], |ui| {
                            if entries.is_empty() {
                                ui.label(
                                    egui::RichText::new("(Empty folder)")
                                        .color(egui::Color32::GRAY),
                                );
                            }
                            for entry in entries {
                                ui.horizontal(|ui| {
                                    if entry.is_world {
                                        if ui
                                            .button(
                                                egui::RichText::new(format!("[World] {}", entry.name))
                                                    .color(egui::Color32::from_rgb(150, 225, 160)),
                                            )
                                            .clicked()
                                        {
                                            editor.open_world(&entry.path, renderer);
                                        }
                                        if ui.small_button("->").on_hover_text("Open subfolder").clicked() {
                                            editor.set_browser_dir(entry.path);
                                        }
                                    } else if ui.button(format!("[Dir] {}", entry.name)).clicked() {
                                        editor.set_browser_dir(entry.path);
                                    }
                                });
                            }
                        });

                    // Right column: Recent Worlds
                    cols[1].set_max_width(col_w);
                    cols[1].heading("Recent Worlds");
                    cols[1].add_space(6.0);
                    let recents = editor.config.recent_worlds.clone();
                    egui::ScrollArea::vertical()
                        .id_salt("welcome_recent_worlds")
                        .max_height(recent_list_h)
                        .auto_shrink([false, false])
                        .show(&mut cols[1], |ui| {
                            if recents.is_empty() {
                                ui.label(
                                    egui::RichText::new("No recent worlds yet.")
                                        .color(egui::Color32::GRAY),
                                );
                            } else {
                                for path in recents {
                                    if ui.button(path.display().to_string()).clicked() {
                                        editor.open_world(&path, renderer);
                                    }
                                }
                            }
                        });
                });
            });
        });
    });
}
