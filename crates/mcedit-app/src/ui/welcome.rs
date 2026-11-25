use crate::config::AppConfig;
use crate::editor::EditorState;
use crate::ui::create_world::CreateWorldDraft;
use egui::Ui;
use mcedit_render::WorldRenderer;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserSort {
    WorldsFirst,
    NameAsc,
    NameDesc,
    ModifiedNewest,
    ModifiedOldest,
}

impl BrowserSort {
    const ALL: [Self; 5] = [
        Self::WorldsFirst,
        Self::NameAsc,
        Self::NameDesc,
        Self::ModifiedNewest,
        Self::ModifiedOldest,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::WorldsFirst => "Worlds First",
            Self::NameAsc => "Name (A-Z)",
            Self::NameDesc => "Name (Z-A)",
            Self::ModifiedNewest => "Newest",
            Self::ModifiedOldest => "Oldest",
        }
    }
}

#[derive(Clone)]
struct MtimeCache {
    dir: PathBuf,
    entries_len: usize,
    mtimes: Arc<HashMap<PathBuf, SystemTime>>,
}

pub fn draw(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    if editor.world.is_some() || editor.jobs.is_busy() {
        return;
    }
    let mut invalidate_mtimes = false;
    if editor.browser_dirty {
        editor.refresh_browser_entries();
        invalidate_mtimes = true;
    }

    let sort_id = egui::Id::unique("welcome_browser_sort_mode");
    let mtime_cache_id = egui::Id::unique("welcome_browser_mtime_cache");
    let mut sort_mode = ui
        .ctx()
        .data(|d| d.get_temp::<BrowserSort>(sort_id))
        .unwrap_or(BrowserSort::WorldsFirst);

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
                            && let Some(parent) =
                                editor.browser_dir.parent().map(|p| p.to_path_buf())
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
                            invalidate_mtimes = true;
                        }
                        egui::ComboBox::from_id_salt("welcome_browser_sort")
                            .width(105.0)
                            .selected_text(sort_mode.label())
                            .show_ui(ui, |ui| {
                                for mode in BrowserSort::ALL {
                                    ui.selectable_value(&mut sort_mode, mode, mode.label());
                                }
                            });
                    });

                    cols[0]
                        .ctx()
                        .data_mut(|d| d.insert_temp(sort_id, sort_mode));

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

                    let mut entries = editor.browser_entries.clone();
                    let mtimes = if matches!(
                        sort_mode,
                        BrowserSort::ModifiedNewest | BrowserSort::ModifiedOldest
                    ) {
                        let cached = cols[0]
                            .ctx()
                            .data(|d| d.get_temp::<MtimeCache>(mtime_cache_id));
                        let valid = !invalidate_mtimes
                            && cached.as_ref().is_some_and(|c| {
                                c.dir == editor.browser_dir && c.entries_len == entries.len()
                            });
                        if valid {
                            cached.map(|c| c.mtimes)
                        } else {
                            let map: HashMap<PathBuf, SystemTime> = entries
                                .iter()
                                .filter_map(|e| {
                                    let mtime = if e.is_world {
                                        std::fs::metadata(e.path.join("level.dat"))
                                            .and_then(|m| m.modified())
                                            .or_else(|_| {
                                                std::fs::metadata(&e.path)
                                                    .and_then(|m| m.modified())
                                            })
                                            .ok()
                                    } else {
                                        std::fs::metadata(&e.path)
                                            .and_then(|m| m.modified())
                                            .ok()
                                    };
                                    mtime.map(|t| (e.path.clone(), t))
                                })
                                .collect();
                            let mtimes = Arc::new(map);
                            cols[0].ctx().data_mut(|d| {
                                d.insert_temp(
                                    mtime_cache_id,
                                    MtimeCache {
                                        dir: editor.browser_dir.clone(),
                                        entries_len: entries.len(),
                                        mtimes: Arc::clone(&mtimes),
                                    },
                                );
                            });
                            Some(mtimes)
                        }
                    } else {
                        None
                    };

                    let cmp_name = |a: &str, b: &str| {
                        a.to_ascii_lowercase()
                            .cmp(&b.to_ascii_lowercase())
                            .then_with(|| a.cmp(b))
                    };

                    entries.sort_by(|a, b| match sort_mode {
                        BrowserSort::WorldsFirst => b
                            .is_world
                            .cmp(&a.is_world)
                            .then_with(|| cmp_name(&a.name, &b.name)),
                        BrowserSort::NameAsc => cmp_name(&a.name, &b.name),
                        BrowserSort::NameDesc => cmp_name(&b.name, &a.name),
                        BrowserSort::ModifiedNewest => {
                            let ta = mtimes.as_deref().and_then(|m| m.get(&a.path));
                            let tb = mtimes.as_deref().and_then(|m| m.get(&b.path));
                            tb.cmp(&ta).then_with(|| cmp_name(&a.name, &b.name))
                        }
                        BrowserSort::ModifiedOldest => {
                            let ta = mtimes.as_deref().and_then(|m| m.get(&a.path));
                            let tb = mtimes.as_deref().and_then(|m| m.get(&b.path));
                            match (ta, tb) {
                                (Some(ta), Some(tb)) => ta.cmp(tb),
                                (Some(_), None) => Ordering::Less,
                                (None, Some(_)) => Ordering::Greater,
                                (None, None) => Ordering::Equal,
                            }
                            .then_with(|| cmp_name(&a.name, &b.name))
                        }
                    });

                    let folder_worlds: Vec<PathBuf> = entries
                        .iter()
                        .filter(|e| e.is_world)
                        .map(|e| e.path.clone())
                        .collect();
                    let browser_dir = editor.browser_dir.clone();

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
                            let mut world_idx = 0usize;
                            for entry in entries {
                                ui.horizontal(|ui| {
                                    if entry.is_world {
                                        let idx = world_idx;
                                        world_idx += 1;
                                        if ui
                                            .button(
                                                egui::RichText::new(format!(
                                                    "[World] {}",
                                                    entry.name
                                                ))
                                                .color(egui::Color32::from_rgb(150, 225, 160)),
                                            )
                                            .clicked()
                                        {
                                            editor.open_world_from_folder(
                                                &browser_dir,
                                                folder_worlds.clone(),
                                                idx,
                                                renderer,
                                            );
                                        }
                                        if ui
                                            .small_button("->")
                                            .on_hover_text("Open subfolder")
                                            .clicked()
                                        {
                                            editor.set_browser_dir(entry.path);
                                        }
                                    } else if ui.button(format!("[Dir] {}", entry.name)).clicked()
                                    {
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