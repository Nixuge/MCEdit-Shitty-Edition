use crate::editor::EditorState;
use crate::tools::ToolId;
use egui::Ui;
use glam::{DVec3, IVec3, Vec3};
use mcedit_core::{BlockStateId, DimensionId, PlayerData, UndoEntry, UndoKind};
use mcedit_nbt::{NbtCompound, NbtTag};

#[derive(Default)]
pub struct PlayerToolState {
    pub selected_idx: usize,
    pub new_player_name: String,
    pub verify_safe_spawn: bool,
}

pub fn handle_player_move_click(editor: &mut EditorState, block_pos: IVec3) {
    let Some(w) = &mut editor.world else {
        return;
    };
    if w.players.is_empty() {
        return;
    }
    let idx = editor.player.selected_idx.min(w.players.len() - 1);
    let before = w.players.clone();
    let dim = w.active_dim;
    {
        let p = &mut w.players[idx];
        p.pos = DVec3::new(
            block_pos.x as f64 + 0.5,
            block_pos.y as f64 + 1.0,
            block_pos.z as f64 + 0.5,
        );
        p.dimension = dim;
        p.sync_to_nbt();
    }
    let after = w.players.clone();
    w.undo_stack.push(UndoEntry {
        label: "Move Player".into(),
        dirty_box: None,
        kind: UndoKind::Players { before, after },
    });
    editor.set_toast("Moved player.");
}

pub fn handle_spawn_move_click(editor: &mut EditorState, block_pos: IVec3) {
    let Some(w) = &mut editor.world else {
        return;
    };
    if w.active_dim != DimensionId::Overworld {
        editor.set_toast("World Spawn is only editable in the Overworld.");
        return;
    }
    let mut target = block_pos + IVec3::Y;
    if editor.player.verify_safe_spawn {
        // Scan upward for 2 air blocks above solid ground
        for y in target.y..(w.max_y() - 2) {
            let b0 = w.get_block(DimensionId::Overworld, IVec3::new(target.x, y, target.z));
            let b1 = w.get_block(DimensionId::Overworld, IVec3::new(target.x, y + 1, target.z));
            if b0 == BlockStateId::AIR && b1 == BlockStateId::AIR {
                target.y = y;
                break;
            }
        }
    }
    let before = w.metadata.clone();
    w.metadata.spawn_pos = target;
    w.metadata.sync_to_nbt();
    let after = w.metadata.clone();
    w.metadata_dirty = true;
    w.undo_stack.push(UndoEntry {
        label: "Move World Spawn".into(),
        dirty_box: None,
        kind: UndoKind::Metadata { before, after },
    });
    editor.set_toast(format!("Moved Spawn to ({}, {}, {}).", target.x, target.y, target.z));
}

pub fn draw_player_panel(ui: &mut Ui, editor: &mut EditorState) {
    ui.heading("Player Tool");
    ui.separator();

    let Some(w) = &mut editor.world else {
        return;
    };

    if w.players.is_empty() {
        ui.label("No players found in world.");
    } else {
        if editor.player.selected_idx >= w.players.len() {
            editor.player.selected_idx = 0;
        }
        let cur_name = w.players[editor.player.selected_idx].display_name.clone();
        egui::ComboBox::from_label("Player")
            .selected_text(cur_name)
            .show_ui(ui, |ui| {
                for (i, p) in w.players.iter().enumerate() {
                    ui.selectable_value(&mut editor.player.selected_idx, i, &p.display_name);
                }
            });

        let (p_pos, p_dim, is_sp) = {
            let p = &w.players[editor.player.selected_idx];
            (p.pos, p.dimension, p.is_singleplayer)
        };
        ui.label(format!(
            "Pos: ({:.1}, {:.1}, {:.1})",
            p_pos.x, p_pos.y, p_pos.z
        ));
        ui.label(format!("Dim: {}", p_dim.display_name()));

        ui.horizontal(|ui| {
            if ui.button("Goto Player").clicked() {
                editor.camera.pos = Vec3::new(p_pos.x as f32, p_pos.y as f32 + 2.0, p_pos.z as f32);
            }
            if ui.button("Align to Camera").clicked() {
                let before = w.players.clone();
                let cam_pos = editor.camera.pos;
                let dim = w.active_dim;
                let pm = &mut w.players[editor.player.selected_idx];
                pm.pos = DVec3::new(cam_pos.x as f64, cam_pos.y as f64, cam_pos.z as f64);
                pm.dimension = dim;
                pm.sync_to_nbt();
                let after = w.players.clone();
                w.undo_stack.push(UndoEntry {
                    label: "Align Player to Camera".into(),
                    dirty_box: None,
                    kind: UndoKind::Players { before, after },
                });
            }
        });

        ui.horizontal(|ui| {
            if ui.button("Edit Player NBT").clicked() {
                let id = w.players[editor.player.selected_idx].id.clone();
                editor.nbt.load_player(w, &id);
                editor.active_tool = ToolId::NbtExplorer;
            }
            let can_remove = !is_sp;
            if ui
                .add_enabled(can_remove, egui::Button::new("Remove"))
                .clicked()
            {
                let before = w.players.clone();
                let removed = w.players.remove(editor.player.selected_idx);
                w.deleted_players.push(removed.id);
                let after = w.players.clone();
                w.undo_stack.push(UndoEntry {
                    label: "Remove Player".into(),
                    dirty_box: None,
                    kind: UndoKind::Players { before, after },
                });
            }
        });
    }

    ui.separator();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut editor.player.new_player_name)
                .hint_text("UUID or Name")
                .desired_width(120.0),
        );
        if ui.button("Add Player").clicked() && !editor.player.new_player_name.trim().is_empty() {
            let name = editor.player.new_player_name.trim().to_string();
            let before = w.players.clone();
            let mut raw = NbtCompound::new();
            raw.insert("Health", 20.0f32);
            raw.insert("playerGameType", 1i32);
            let mut new_p = PlayerData {
                id: name.clone(),
                display_name: name,
                is_singleplayer: false,
                dimension: w.active_dim,
                pos: DVec3::new(
                    editor.camera.pos.x as f64,
                    editor.camera.pos.y as f64,
                    editor.camera.pos.z as f64,
                ),
                rotation: [0.0, 0.0],
                raw_nbt: raw,
                dirty: true,
            };
            new_p.sync_to_nbt();
            if let Some(NbtTag::Int(_)) = new_p.raw_nbt.get("Dimension") {}
            w.players.push(new_p);
            let after = w.players.clone();
            w.undo_stack.push(UndoEntry {
                label: "Add Player".into(),
                dirty_box: None,
                kind: UndoKind::Players { before, after },
            });
            editor.player.new_player_name.clear();
        }
    });
    ui.small("Click in 3D view to move selected player.");
}

pub fn draw_spawn_panel(ui: &mut Ui, editor: &mut EditorState) {
    ui.heading("World Spawn Tool");
    ui.separator();

    if let Some(w) = &editor.world {
        let sp = w.metadata.spawn_pos;
        ui.label(format!("Spawn: ({}, {}, {})", sp.x, sp.y, sp.z));
        if ui.button("Teleport Camera to Spawn").clicked() {
            editor.camera.pos = Vec3::new(sp.x as f32 + 0.5, sp.y as f32 + 6.0, sp.z as f32 + 0.5);
        }
    }
    ui.checkbox(&mut editor.player.verify_safe_spawn, "Auto-find Safe Y");
    ui.small("Click in Overworld 3D view to move World Spawn.");
}
