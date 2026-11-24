use crate::editor::EditorState;
use ahash::{AHashMap, AHashSet};
use egui::{Color32, RichText, Ui};
use glam::IVec3;
use mcedit_core::{
    BlockRegistry, BlockStateId, Chunk, ChunkPos, CompareConfig, CompareViewMode, DiffKind,
    DimensionId, WorldFormat,
};
use mcedit_formats::FormatRegistry;
use mcedit_render::WorldRenderer;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type CompareChunkCache = Arc<RwLock<AHashMap<(DimensionId, ChunkPos), Arc<Chunk>>>>;

/// Strictly read-only reference world (World B) loaded without acquiring `session.lock`.
pub struct CompareWorld {
    pub path: PathBuf,
    pub display_name: String,
    pub format_name: String,
    pub format: Arc<dyn WorldFormat>,
    pub known_chunks: AHashMap<DimensionId, AHashSet<ChunkPos>>,
    pub min_y: i32,
    pub max_y: i32,
    pub chunk_cache: CompareChunkCache,
}

impl CompareWorld {
    pub fn open_readonly(
        path: &Path,
        registry: &RwLock<BlockRegistry>,
    ) -> Result<Self, String> {
        let fmt_box = {
            let reg = registry.read();
            FormatRegistry::open_world(path, &reg).map_err(|e| e.to_string())?
        };
        let format: Arc<dyn WorldFormat> = Arc::from(fmt_box);
        // Never acquire session.lock on the comparison reference world!
        let metadata = format.load_metadata().map_err(|e| e.to_string())?;
        let mut dimensions = format.dimensions();
        if dimensions.is_empty() {
            dimensions.push(DimensionId::Overworld);
        }
        let mut known_chunks = AHashMap::new();
        for &dim in &dimensions {
            let list = format.list_chunks(dim).map_err(|e| e.to_string())?;
            known_chunks.insert(dim, list.into_iter().collect());
        }
        let root_path = format.root_path().to_path_buf();
        let display_name = if !metadata.level_name.trim().is_empty() {
            metadata.level_name.clone()
        } else {
            root_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("World B")
                .to_string()
        };
        let min_y = format.min_section_y() * 16;
        let max_y = (format.max_section_y() + 1) * 16;
        Ok(Self {
            path: root_path,
            display_name,
            format_name: metadata.format_name,
            format,
            known_chunks,
            min_y,
            max_y,
            chunk_cache: Arc::new(RwLock::new(AHashMap::new())),
        })
    }

    pub fn chunk_exists(&self, dim: DimensionId, pos: ChunkPos) -> bool {
        self.known_chunks
            .get(&dim)
            .is_some_and(|set| set.contains(&pos))
    }

    pub fn chunk_count(&self, dim: DimensionId) -> usize {
        self.known_chunks.get(&dim).map_or(0, |s| s.len())
    }

    pub fn get_cached_block(&self, dim: DimensionId, pos: IVec3) -> Option<BlockStateId> {
        if pos.y < self.min_y || pos.y >= self.max_y {
            return Some(BlockStateId::AIR);
        }
        let cp = ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
        if !self.chunk_exists(dim, cp) {
            return Some(BlockStateId::AIR);
        }
        let cache = self.chunk_cache.read();
        cache.get(&(dim, cp)).map(|c| {
            c.get_block(
                pos.x.rem_euclid(16) as usize,
                pos.y,
                pos.z.rem_euclid(16) as usize,
            )
        })
    }
}

pub struct CompareToolState {
    pub world_b: Option<CompareWorld>,
    pub config: CompareConfig,
    pub copy_only_differing: bool,
    pub last_error: Option<String>,
}

impl Default for CompareToolState {
    fn default() -> Self {
        Self {
            world_b: None,
            config: CompareConfig::default(),
            copy_only_differing: true,
            last_error: None,
        }
    }
}

impl CompareToolState {
    #[inline]
    pub fn is_active(&self) -> bool {
        self.world_b.is_some() && self.config.enabled
    }
}

fn color_swatch(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, Color32::from_gray(40)),
        egui::StrokeKind::Inside,
    );
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    ui.heading("Compare Worlds");
    ui.label(
        RichText::new("Diff active terrain (World A) against a read-only reference world (World B).")
            .small()
            .color(Color32::from_gray(180)),
    );
    ui.separator();

    // 1. Reference World (World B) Loader
    let mut path_to_open: Option<PathBuf> = None;
    let mut close_b = false;

    if let Some(wb) = &editor.compare.world_b {
        let active_dim = editor
            .world
            .as_ref()
            .map(|w| w.active_dim)
            .unwrap_or(DimensionId::Overworld);
        let chunks_in_dim = wb.chunk_count(active_dim);
        ui.label(RichText::new(format!("World B: {}", wb.display_name)).strong())
            .on_hover_text(wb.path.display().to_string());
        ui.label(
            RichText::new(format!(
                "{} | {} chunks in dim",
                wb.format_name, chunks_in_dim
            ))
            .small()
            .color(Color32::from_gray(175)),
        );
        ui.horizontal(|ui| {
            if ui.button("Change World B...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                path_to_open = Some(dir);
            }
            if ui.button("Close B").clicked() {
                close_b = true;
            }
        });
    } else {
        ui.horizontal(|ui| {
            if ui.button("Open World B Folder...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                path_to_open = Some(dir);
            }
        });
        if ui.button("Open World B level.dat...").clicked()
            && let Some(file) = rfd::FileDialog::new()
                .add_filter("level.dat", &["dat"])
                .pick_file()
        {
            path_to_open = Some(file);
        }
        ui.label(
            RichText::new("Tip: Drag & drop a world folder or level.dat here to compare.")
                .small()
                .color(Color32::from_gray(160)),
        );
    }

    // Quick-pick from Recent Worlds (excluding currently open World A)
    let world_a_root = editor
        .world
        .as_ref()
        .map(|w| w.format.root_path().to_path_buf());
    let recent_candidates: Vec<PathBuf> = editor
        .config
        .recent_worlds
        .iter()
        .filter(|p| world_a_root.as_ref() != Some(*p))
        .take(6)
        .cloned()
        .collect();
    if !recent_candidates.is_empty() {
        egui::CollapsingHeader::new("Quick Pick from Recent Worlds")
            .default_open(editor.compare.world_b.is_none())
            .show(ui, |ui| {
                for p in recent_candidates {
                    let label = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| p.display().to_string());
                    if ui
                        .button(&label)
                        .on_hover_text(p.display().to_string())
                        .clicked()
                    {
                        path_to_open = Some(p);
                    }
                }
            });
    }

    // Folder Browser inside the Compare sidebar when no World B is loaded
    if editor.compare.world_b.is_none() {
        if editor.browser_dirty {
            editor.refresh_browser_entries();
        }
        ui.separator();
        ui.label(RichText::new("Browse Worlds").strong());
        ui.horizontal_wrapped(|ui| {
            let can_up = editor.browser_dir.parent().is_some();
            if ui
                .add_enabled(can_up, egui::Button::new("Up").small())
                .clicked()
                && let Some(parent) = editor.browser_dir.parent()
            {
                editor.set_browser_dir(parent.to_path_buf());
            }
            if let Some(saves) = crate::config::AppConfig::minecraft_saves_dir()
                && ui.small_button("Saves").clicked()
            {
                editor.set_browser_dir(saves);
            }
            if let Some(base) = directories::BaseDirs::new()
                && ui.small_button("Home").clicked()
            {
                editor.set_browser_dir(base.home_dir().to_path_buf());
            }
            if ui.small_button("Refresh").clicked() {
                editor.browser_dirty = true;
            }
        });
        ui.label(
            RichText::new(editor.browser_dir.display().to_string())
                .small()
                .color(Color32::GRAY),
        );

        if editor.browser_dir.join("level.dat").is_file() {
            let name = editor
                .browser_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Current Folder");
            if ui
                .button(
                    RichText::new(format!("Compare: {name}"))
                        .strong()
                        .color(Color32::from_rgb(120, 200, 255)),
                )
                .clicked()
            {
                path_to_open = Some(editor.browser_dir.clone());
            }
        }

        let mut entries = editor.browser_entries.clone();
        entries.sort_by(|a, b| {
            b.is_world
                .cmp(&a.is_world)
                .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
        });

        let avail_h = (ui.available_height() - 12.0).max(160.0);
        egui::ScrollArea::vertical()
            .id_salt("compare_folder_browser")
            .max_height(avail_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if entries.is_empty() {
                    ui.label(RichText::new("(Empty folder)").small().color(Color32::GRAY));
                }
                for entry in entries {
                    ui.horizontal(|ui| {
                        if entry.is_world {
                            let is_world_a = world_a_root.as_ref() == Some(&entry.path);
                            let label = if is_world_a {
                                format!("[World] {} (A)", entry.name)
                            } else {
                                format!("[World] {}", entry.name)
                            };
                            let color = if is_world_a {
                                Color32::from_gray(160)
                            } else {
                                Color32::from_rgb(140, 210, 255)
                            };
                            if ui
                                .button(RichText::new(label).color(color))
                                .on_hover_text(entry.path.display().to_string())
                                .clicked()
                            {
                                path_to_open = Some(entry.path.clone());
                            }
                            if ui
                                .small_button("->")
                                .on_hover_text("Open subfolder")
                                .clicked()
                            {
                                editor.set_browser_dir(entry.path);
                            }
                        } else if ui.button(format!("[Dir] {}", entry.name)).clicked() {
                            editor.set_browser_dir(entry.path);
                        }
                    });
                }
            });
    }

    if let Some(err) = &editor.compare.last_error {
        ui.colored_label(Color32::from_rgb(255, 110, 110), err);
    }

    if close_b {
        editor.close_compare_world(renderer);
        return;
    }
    if let Some(p) = path_to_open {
        editor.open_compare_world(&p, renderer);
    }

    if editor.compare.world_b.is_none() {
        return;
    }

    ui.separator();

    // 2. Active Toggle & Quick Blink View Mode
    let mut remesh_needed = false;
    let mut lod_remesh_needed = false;

    if ui
        .checkbox(&mut editor.compare.config.enabled, "Enable Comparison Overlay")
        .changed()
    {
        remesh_needed = true;
    }

    ui.add_enabled_ui(editor.compare.config.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label("View:");
            for (mode, lbl) in [
                (CompareViewMode::OnlyWorldA, "World A"),
                (CompareViewMode::Diff, "Diff"),
                (CompareViewMode::OnlyWorldB, "World B"),
            ] {
                if ui
                    .selectable_label(editor.compare.config.view_mode == mode, lbl)
                    .clicked()
                    && editor.compare.config.view_mode != mode
                {
                    editor.compare.config.view_mode = mode;
                    remesh_needed = true;
                }
            }
        });

        ui.separator();
        ui.label(RichText::new("Overlay Filters").strong());

        ui.horizontal(|ui| {
            color_swatch(ui, Color32::from_rgba_unmultiplied(240, 48, 48, 220));
            remesh_needed |= ui
                .checkbox(&mut editor.compare.config.show_mismatch, "Mismatched (Red)")
                .changed();
        });
        ui.horizontal(|ui| {
            color_swatch(ui, Color32::from_rgba_unmultiplied(87, 194, 255, 220));
            remesh_needed |= ui
                .checkbox(
                    &mut editor.compare.config.show_only_a,
                    "Only in World A (Light Blue)",
                )
                .changed();
        });
        ui.horizontal(|ui| {
            color_swatch(ui, Color32::from_rgba_unmultiplied(55, 110, 245, 220));
            remesh_needed |= ui
                .checkbox(
                    &mut editor.compare.config.show_only_b,
                    "Only in World B (Blue)",
                )
                .changed();
        });

        ui.add_space(4.0);
        remesh_needed |= ui
            .checkbox(
                &mut editor.compare.config.hide_matching,
                "Hide Matching Blocks (Diff-Only)",
            )
            .on_hover_text("Hides identical terrain so only differing and one-sided blocks appear.")
            .changed();
        remesh_needed |= ui
            .checkbox(
                &mut editor.compare.config.ignore_properties,
                "Ignore Block Properties (Type Only)",
            )
            .on_hover_text("Compares base block names only, ignoring rotation, slab half, waterlogged, etc.")
            .changed();

        ui.separator();
        ui.label(RichText::new("Overlay Transparency").strong());

        let tint_resp = ui.add(
            egui::Slider::new(&mut editor.compare.config.overlay_strength, 0.15..=0.95)
                .text("Tint")
                .fixed_decimals(2),
        );
        let a_alpha_resp = ui.add(
            egui::Slider::new(&mut editor.compare.config.only_a_alpha, 0.25..=1.0)
                .text("A-Only Opacity")
                .fixed_decimals(2),
        );
        let b_alpha_resp = ui.add(
            egui::Slider::new(&mut editor.compare.config.only_b_alpha, 0.15..=1.0)
                .text("B-Only Opacity")
                .fixed_decimals(2),
        );
        if tint_resp.changed() || a_alpha_resp.changed() || b_alpha_resp.changed() {
            editor.sync_compare_uniform(renderer);
        }
        // Re-bake distant LOD colors once the user finishes dragging a transparency slider
        if tint_resp.drag_stopped() || a_alpha_resp.drag_stopped() || b_alpha_resp.drag_stopped() {
            lod_remesh_needed = true;
        }

        // Optional chunk alignment offset
        egui::CollapsingHeader::new("Alignment Offset (Chunks)")
            .default_open(editor.compare.config.chunk_offset != (0, 0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("dCX:");
                    remesh_needed |= ui
                        .add(
                            egui::DragValue::new(&mut editor.compare.config.chunk_offset.0)
                                .speed(0.2),
                        )
                        .changed();
                    ui.label("dCZ:");
                    remesh_needed |= ui
                        .add(
                            egui::DragValue::new(&mut editor.compare.config.chunk_offset.1)
                                .speed(0.2),
                        )
                        .changed();
                    if editor.compare.config.chunk_offset != (0, 0) && ui.button("Reset").clicked()
                    {
                        editor.compare.config.chunk_offset = (0, 0);
                        remesh_needed = true;
                    }
                });
            });
    });

    if remesh_needed {
        editor.sync_compare_uniform(renderer);
        editor.invalidate_all_meshes(renderer);
    } else if lod_remesh_needed {
        editor.invalidate_lod_meshes_only(renderer);
    }

    // 3. Live Cursor Diff Inspector
    ui.separator();
    ui.label(RichText::new("Cursor Block Diff").strong());
    if let Some(hit) = editor.hover_hit {
        let bp = hit.block_pos;
        ui.label(
            RichText::new(format!("Pos: ({}, {}, {})", bp.x, bp.y, bp.z))
                .small()
                .color(Color32::from_gray(180)),
        );
        if let Some((kind, id_a, id_b)) = editor.inspect_compare_block(bp) {
            let (status_txt, status_col) = match kind {
                DiffKind::Match => ("Match", Color32::from_rgb(120, 225, 130)),
                DiffKind::Mismatch => ("Mismatch", Color32::from_rgb(255, 100, 100)),
                DiffKind::OnlyInA => ("Only in World A", Color32::from_rgb(100, 200, 255)),
                DiffKind::OnlyInB => ("Only in World B", Color32::from_rgb(95, 145, 255)),
            };
            ui.colored_label(status_col, RichText::new(status_txt).strong());
            let reg = editor.registry.read();
            let str_a = if id_a == BlockStateId::AIR {
                "minecraft:air".to_string()
            } else {
                reg.get_state(id_a).to_string()
            };
            let str_b = if id_b == BlockStateId::AIR {
                "minecraft:air".to_string()
            } else {
                reg.get_state(id_b).to_string()
            };
            ui.label(RichText::new(format!("A: {str_a}")).small());
            ui.label(RichText::new(format!("B: {str_b}")).small());
        } else {
            ui.label(
                RichText::new("(Chunk streaming...)")
                    .small()
                    .color(Color32::from_gray(150)),
            );
        }
    } else {
        ui.label(
            RichText::new("Hover a block to inspect A vs B")
                .small()
                .color(Color32::from_gray(150)),
        );
    }

    // 4. Sync World B -> World A inside current selection (Undoable)
    ui.separator();
    ui.label(RichText::new("Sync Selection (World B -> World A)").strong());
    ui.checkbox(
        &mut editor.compare.copy_only_differing,
        "Only overwrite differing blocks",
    );
    let has_sel = editor.selection.box_.is_some();
    if let Some(b) = editor.selection.box_ {
        let s = b.size();
        ui.label(
            RichText::new(format!("Selection: {}x{}x{}", s.x, s.y, s.z))
                .small()
                .color(Color32::from_gray(180)),
        );
    } else {
        ui.label(
            RichText::new("Make a selection with [1] Select to copy terrain from World B.")
                .small()
                .color(Color32::from_gray(150)),
        );
    }
    if ui
        .add_enabled(
            has_sel && !editor.jobs.is_busy(),
            egui::Button::new("Copy World B -> World A"),
        )
        .on_hover_text("Copies blocks & block entities from World B into World A inside the selection (Undoable with Ctrl+Z).")
        .clicked()
    {
        editor.copy_compare_b_to_a_in_selection(renderer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcedit_core::{BoundingBox, EditTransaction, World, copy_from_format_in_box};
    use mcedit_formats::AnvilPost113Format;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn compare_copy_b_to_a_is_undoable_and_readonly() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("mcedit_compare_test_{nanos}"));
        let dir_a = base.join("world_a");
        let dir_b = base.join("world_b");

        let reg_arc = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let (stone, dirt, gold) = {
            let reg = reg_arc.read();
            (
                reg.find_by_legacy(1, 0).unwrap(),
                reg.find_by_legacy(3, 0).unwrap(),
                reg.find_by_legacy(41, 0).unwrap(),
            )
        };

        // Create World A with chunk (0,0) = stone
        let fmt_a = {
            let reg = reg_arc.read();
            AnvilPost113Format::create_new(&dir_a, "WorldA", 1, 1, IVec3::new(0, 64, 0), 3700, &reg)
                .unwrap()
        };
        let mut world_a = World::open(Box::new(fmt_a), reg_arc.clone(), 16).unwrap();
        let mut ca = Chunk::new(ChunkPos::new(0, 0), -4, 19);
        ca.set_block(2, 64, 2, stone);
        world_a.insert_chunk(DimensionId::Overworld, ca);
        world_a.save_all().unwrap();

        // Create World B with chunk (0,0) = dirt and chunk (1,0) = gold (B-only chunk!)
        let fmt_b_init = {
            let reg = reg_arc.read();
            AnvilPost113Format::create_new(&dir_b, "WorldB", 2, 1, IVec3::new(0, 64, 0), 3700, &reg)
                .unwrap()
        };
        {
            let mut wb_init = World::open(Box::new(fmt_b_init), reg_arc.clone(), 4).unwrap();
            let mut cb0 = Chunk::new(ChunkPos::new(0, 0), -4, 19);
            cb0.set_block(2, 64, 2, dirt);
            wb_init.insert_chunk(DimensionId::Overworld, cb0);
            let mut cb1 = Chunk::new(ChunkPos::new(1, 0), -4, 19);
            cb1.set_block(1, 64, 1, gold);
            wb_init.insert_chunk(DimensionId::Overworld, cb1);
            wb_init.save_all().unwrap();
        }
        // Delete World B's session.lock so we can verify CompareWorld never creates it!
        let lock_b = dir_b.join("session.lock");
        let _ = std::fs::remove_file(&lock_b);
        assert!(!lock_b.exists());

        let cmp_world = CompareWorld::open_readonly(&dir_b, &reg_arc).unwrap();
        assert!(
            !lock_b.exists(),
            "CompareWorld::open_readonly must NEVER create or modify World B's session.lock"
        );
        assert!(cmp_world.chunk_exists(DimensionId::Overworld, ChunkPos::new(1, 0)));

        // Copy B -> A across both chunk (0,0) and B-only chunk (1,0)
        let sel = BoundingBox::from_corners_inclusive(IVec3::new(0, 64, 0), IVec3::new(20, 64, 5));
        let mut tx = EditTransaction::new("Copy from World B", DimensionId::Overworld, Some(sel));
        let changed = copy_from_format_in_box(
            &mut world_a,
            DimensionId::Overworld,
            cmp_world.format.as_ref(),
            sel,
            (0, 0),
            true,
            false,
            &mut tx,
        );
        assert_eq!(changed, 2, "expected 2 differing blocks copied (dirt + gold)");
        tx.commit(&mut world_a);

        assert_eq!(
            world_a.get_block(DimensionId::Overworld, IVec3::new(2, 64, 2)),
            dirt
        );
        assert_eq!(
            world_a.get_block(DimensionId::Overworld, IVec3::new(17, 64, 1)),
            gold
        );

        // Undo must restore World A completely (including removing the newly created chunk (1,0))
        world_a.undo();
        assert_eq!(
            world_a.get_block(DimensionId::Overworld, IVec3::new(2, 64, 2)),
            stone
        );
        assert!(!world_a.chunk_exists(DimensionId::Overworld, ChunkPos::new(1, 0)));

        let _ = std::fs::remove_dir_all(&base);
    }
}
