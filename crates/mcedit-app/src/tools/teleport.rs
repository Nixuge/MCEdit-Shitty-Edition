use crate::editor::EditorState;
use egui::Ui;
use glam::{IVec3, Vec3};
use mcedit_core::ChunkPos;

pub struct TeleportToolState {
    pub target_pos: IVec3,
    pub target_chunk: ChunkPos,
    pub initialized_for_world: bool,
}

impl Default for TeleportToolState {
    fn default() -> Self {
        Self {
            target_pos: IVec3::new(0, 80, 0),
            target_chunk: ChunkPos::new(0, 0),
            initialized_for_world: false,
        }
    }
}

pub fn handle_left_click(editor: &mut EditorState, block_pos: IVec3) {
    let dest = Vec3::new(
        block_pos.x as f32 + 0.5,
        block_pos.y as f32 + 6.0,
        block_pos.z as f32 + 0.5,
    );
    editor.teleport_camera_to(dest);
    editor.teleport.target_pos = block_pos;
    editor.teleport.target_chunk = ChunkPos::new(
        block_pos.x.div_euclid(16),
        block_pos.z.div_euclid(16),
    );
    editor.set_toast(format!(
        "Teleported camera to ({}, {}, {}).",
        block_pos.x,
        block_pos.y + 6,
        block_pos.z
    ));
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState) {
    ui.heading("Teleport Tool");
    ui.separator();
    ui.small("Click any block or distant LOD terrain in the viewport to jump there.");
    ui.add_space(4.0);

    if !editor.teleport.initialized_for_world {
        let p = editor.camera.pos;
        editor.teleport.target_pos = IVec3::new(p.x.round() as i32, p.y.round() as i32, p.z.round() as i32);
        editor.teleport.target_chunk = ChunkPos::new(
            (p.x / 16.0).floor() as i32,
            (p.z / 16.0).floor() as i32,
        );
        editor.teleport.initialized_for_world = true;
    }

    let (min_y, max_y) = editor.world_y_bounds();

    ui.strong("Block Coordinates (X, Y, Z)");
    ui.horizontal(|ui| {
        ui.label("X:");
        ui.add(egui::DragValue::new(&mut editor.teleport.target_pos.x).speed(1.0));
        ui.label("Y:");
        ui.add(
            egui::DragValue::new(&mut editor.teleport.target_pos.y)
                .range(min_y..=(max_y + 128))
                .speed(1.0),
        );
        ui.label("Z:");
        ui.add(egui::DragValue::new(&mut editor.teleport.target_pos.z).speed(1.0));
    });
    ui.horizontal(|ui| {
        if ui.button("Teleport to (X, Y, Z)").clicked() {
            let tp = editor.teleport.target_pos;
            editor.teleport_camera_to(Vec3::new(tp.x as f32 + 0.5, tp.y as f32, tp.z as f32 + 0.5));
            editor.set_toast(format!("Teleported to ({}, {}, {}).", tp.x, tp.y, tp.z));
        }
        if ui.button("Use Current Cam").clicked() {
            let p = editor.camera.pos;
            editor.teleport.target_pos =
                IVec3::new(p.x.round() as i32, p.y.round() as i32, p.z.round() as i32);
        }
    });

    ui.separator();
    ui.strong("Chunk Coordinates (CX, CZ)");
    let mut cx = editor.teleport.target_chunk.cx();
    let mut cz = editor.teleport.target_chunk.cz();
    ui.horizontal(|ui| {
        ui.label("CX:");
        ui.add(egui::DragValue::new(&mut cx).speed(1.0));
        ui.label("CZ:");
        ui.add(egui::DragValue::new(&mut cz).speed(1.0));
    });
    editor.teleport.target_chunk = ChunkPos::new(cx, cz);
    if ui.button("Teleport to Chunk").clicked() {
        let bx = cx * 16 + 8;
        let bz = cz * 16 + 8;
        let by = editor.camera.pos.y.clamp((min_y + 16) as f32, (max_y + 32) as f32);
        editor.teleport_camera_to(Vec3::new(bx as f32, by, bz as f32));
        editor.teleport.target_pos = IVec3::new(bx, by as i32, bz);
        editor.set_toast(format!("Teleported to chunk ({cx}, {cz}) -> ({bx}, {by:.0}, {bz})."));
    }

    let Some(world) = &editor.world else {
        return;
    };
    let dim = world.active_dim;
    let spawn = world.metadata.spawn_pos;
    let players: Vec<(String, Vec3)> = world
        .players
        .iter()
        .filter(|p| p.dimension == dim)
        .map(|p| {
            (
                p.display_name.clone(),
                Vec3::new(p.pos.x as f32, p.pos.y as f32 + 2.0, p.pos.z as f32),
            )
        })
        .collect();

    let cam_cp = ChunkPos::new(
        (editor.camera.pos.x / 16.0).floor() as i32,
        (editor.camera.pos.z / 16.0).floor() as i32,
    );
    let nearest_chunk = world
        .known_chunks
        .get(&dim)
        .and_then(|set| {
            set.iter().copied().min_by_key(|cp| {
                let dx = (cp.cx() - cam_cp.cx()) as i64;
                let dz = (cp.cz() - cam_cp.cz()) as i64;
                dx * dx + dz * dz
            })
        });
    let mut regions: Vec<(i32, i32)> = world
        .known_regions
        .get(&dim)
        .map(|s| s.iter().copied().collect())
        .unwrap_or_default();
    regions.sort_unstable();

    ui.separator();
    ui.strong("Quick Locations");
    if ui
        .button(format!(
            "World Spawn ({}, {}, {})",
            spawn.x, spawn.y, spawn.z
        ))
        .clicked()
    {
        editor.teleport_camera_to(Vec3::new(
            spawn.x as f32 + 0.5,
            spawn.y as f32 + 10.0,
            spawn.z as f32 + 0.5,
        ));
        editor.set_toast(format!(
            "Teleported to World Spawn ({}, {}, {}).",
            spawn.x, spawn.y, spawn.z
        ));
    }

    if let Some(nc) = nearest_chunk {
        let bx = nc.cx() * 16 + 8;
        let bz = nc.cz() * 16 + 8;
        if ui
            .button(format!(
                "Nearest Generated Chunk ({}, {}) [X={}, Z={}]",
                nc.cx(),
                nc.cz(),
                bx,
                bz
            ))
            .clicked()
        {
            let by = editor.camera.pos.y.clamp(80.0, (max_y + 16) as f32);
            editor.teleport_camera_to(Vec3::new(bx as f32, by, bz as f32));
            editor.set_toast(format!("Teleported to nearest chunk ({}, {}).", nc.cx(), nc.cz()));
        }
    }

    if !players.is_empty() {
        ui.separator();
        ui.strong("Players");
        for (name, ppos) in players {
            if ui
                .button(format!(
                    "{} ({:.0}, {:.0}, {:.0})",
                    name, ppos.x, ppos.y, ppos.z
                ))
                .clicked()
            {
                editor.teleport_camera_to(ppos);
                editor.set_toast(format!("Teleported to {name}."));
            }
        }
    }

    if !regions.is_empty() {
        ui.separator();
        ui.strong(format!("Regions in Dimension ({})", regions.len()));
        egui::ScrollArea::vertical()
            .id_salt("teleport_regions_scroll")
            .max_height(180.0)
            .show(ui, |ui| {
                for (rx, rz) in regions {
                    let center_x = rx * 512 + 256;
                    let center_z = rz * 512 + 256;
                    if ui
                        .button(format!(
                            "r.{rx}.{rz}.mca  (X: {center_x}, Z: {center_z})"
                        ))
                        .clicked()
                    {
                        let by = editor.camera.pos.y.clamp(96.0, (max_y + 16) as f32);
                        editor.teleport_camera_to(Vec3::new(center_x as f32, by, center_z as f32));
                        editor.set_toast(format!(
                            "Teleported to region r.{rx}.{rz}.mca ({center_x}, {by:.0}, {center_z})."
                        ));
                    }
                }
            });
    }
}
