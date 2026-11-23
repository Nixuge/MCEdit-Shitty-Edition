use crate::editor::EditorState;
use crate::jobs::JobOutcome;
use egui::Ui;
use mcedit_core::{
    ChunkPos, EditTransaction, FlatlandLayer, LightingEngine, create_flatland_chunk,
    default_flatland_layers,
};
use mcedit_render::WorldRenderer;

pub struct ChunkToolState {
    pub flatland_layers: Vec<FlatlandLayer>,
    pub confirm_prune: bool,
}

impl Default for ChunkToolState {
    fn default() -> Self {
        Self {
            flatland_layers: default_flatland_layers(),
            confirm_prune: false,
        }
    }
}

pub fn handle_left_click(editor: &mut EditorState, block_pos: glam::IVec3, shift: bool) {
    let cp = ChunkPos::new(block_pos.x.div_euclid(16), block_pos.z.div_euclid(16));
    if !shift && editor.selected_chunks.contains(&cp) {
        editor.selected_chunks.remove(&cp);
    } else {
        editor.selected_chunks.insert(cp);
    }
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    ui.heading("Chunk Control");
    ui.separator();

    ui.label(format!("Selected Chunks: {}", editor.selected_chunks.len()));
    ui.horizontal(|ui| {
        if ui.button("From Selection").clicked()
            && let Some(b) = editor.selection.box_
        {
            editor.selected_chunks.clear();
            for cp in b.iter_chunks() {
                editor.selected_chunks.insert(cp);
            }
        }
        if ui.button("Clear").clicked() {
            editor.selected_chunks.clear();
        }
    });

    ui.separator();
    let has_chunks = !editor.selected_chunks.is_empty();

    if ui
        .add_enabled(has_chunks, egui::Button::new("Create Flatland Chunks"))
        .clicked()
        && let Some(w) = &mut editor.world
    {
        let dim = w.active_dim;
        let min_sy = w.format.min_section_y();
        let max_sy = w.format.max_section_y();
        let layers = editor.chunk.flatland_layers.clone();
        let targets: Vec<ChunkPos> = editor.selected_chunks.iter().copied().collect();
        let mut created = 0usize;
        let mut tx = EditTransaction::new("Create Chunks", dim, None);
        for cp in &targets {
            if !w.chunk_exists(dim, *cp) {
                tx.record_chunk_if_needed(w, *cp);
                let chunk = {
                    let mut reg = w.registry.write();
                    create_flatland_chunk(*cp, min_sy, max_sy, &layers, &mut reg)
                };
                w.insert_chunk(dim, chunk);
                created += 1;
            }
        }
        tx.commit(w);
        for cp in targets {
            editor.invalidate_chunk(cp, renderer);
        }
        editor.set_toast(format!("Created {created} flatland chunks."));
    }

    if ui
        .add_enabled(has_chunks, egui::Button::new("Relight Selected"))
        .clicked()
        && !editor.jobs.is_busy()
        && let Some(world) = editor.world.take()
    {
        let targets: Vec<ChunkPos> = editor.selected_chunks.iter().copied().collect();
        editor.jobs.spawn_world_job("Relighting Chunks", world, move |mut w, prog| {
            let dim = w.active_dim;
            prog.set(1, 2, format!("Propagating lighting across {} chunks...", targets.len()));
            LightingEngine::relight_chunks(&mut w, dim, &targets);
            JobOutcome::WorldUpdated {
                world: w,
                status: format!("Relit {} chunks.", targets.len()),
                invalidate_all: true,
                dirty_box: None,
            }
        });
    }

    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_chunks, egui::Button::new("Mark Repop"))
            .clicked()
        {
            set_repop(editor, true);
        }
        if ui
            .add_enabled(has_chunks, egui::Button::new("Don't Repop"))
            .clicked()
        {
            set_repop(editor, false);
        }
    });

    ui.separator();
    if ui
        .add_enabled(has_chunks, egui::Button::new("Delete Selected Chunks"))
        .clicked()
        && let Some(w) = &mut editor.world
    {
        let dim = w.active_dim;
        let targets: Vec<ChunkPos> = editor.selected_chunks.iter().copied().collect();
        let mut tx = EditTransaction::new("Delete Chunks", dim, None);
        for &cp in &targets {
            if w.chunk_exists(dim, cp) {
                tx.record_chunk_if_needed(w, cp);
                w.mark_chunk_deleted(dim, cp);
            }
        }
        tx.commit(w);
        for cp in targets {
            editor.invalidate_chunk(cp, renderer);
        }
        editor.set_toast("Marked selected chunks for deletion.");
    }

    if ui
        .add_enabled(has_chunks, egui::Button::new("Prune Unselected..."))
        .clicked()
    {
        editor.chunk.confirm_prune = true;
    }

    if editor.chunk.confirm_prune {
        ui.colored_label(
            egui::Color32::YELLOW,
            "Delete ALL chunks outside selection?",
        );
        ui.horizontal(|ui| {
            if ui.button("Confirm Prune").clicked()
                && let Some(w) = &mut editor.world
            {
                let dim = w.active_dim;
                let all = w.list_chunks(dim);
                let mut tx = EditTransaction::new("Prune Chunks", dim, None);
                let mut count = 0usize;
                for cp in all {
                    if !editor.selected_chunks.contains(&cp) {
                        tx.record_chunk_if_needed(w, cp);
                        w.mark_chunk_deleted(dim, cp);
                        count += 1;
                    }
                }
                tx.commit(w);
                editor.invalidate_all_meshes(renderer);
                editor.chunk.confirm_prune = false;
                editor.set_toast(format!("Pruned {count} chunks."));
            }
            if ui.button("Cancel").clicked() {
                editor.chunk.confirm_prune = false;
            }
        });
    }
}

fn set_repop(editor: &mut EditorState, populated: bool) {
    let Some(w) = &mut editor.world else {
        return;
    };
    let dim = w.active_dim;
    let targets: Vec<ChunkPos> = editor.selected_chunks.iter().copied().collect();
    let mut tx = EditTransaction::new("Set TerrainPopulated", dim, None);
    for cp in targets {
        tx.record_chunk_if_needed(w, cp);
        if let Ok(Some(c)) = w.get_chunk_mut(dim, cp) {
            c.terrain_populated = populated;
            c.dirty = true;
        }
    }
    tx.commit(w);
    editor.set_toast(format!("Set TerrainPopulated = {populated}."));
}
