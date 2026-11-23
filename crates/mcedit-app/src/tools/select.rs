use crate::editor::EditorState;
use egui::Ui;
use glam::{IVec3, Vec3};
use mcedit_core::{
    BoundingBox, ChunkPos, EditTransaction, Face, delete_entities_in_box, delete_tile_ticks_in_box,
};
use mcedit_render::WorldRenderer;

pub const EDGE_CLICKTHROUGH_FACTOR: f32 = 0.1;

#[derive(Debug, Default)]
pub struct SelectionState {
    pub box_: Option<BoundingBox>,
    pub corner_a: Option<IVec3>,
    /// True after clicking and releasing on a single block; awaiting the second corner click.
    pub click_selecting: bool,
    /// Anchor block while left mouse button is held down to drag-create a new selection.
    pub creating_drag_start: Option<IVec3>,
    pub dragging_face: Option<FaceDrag>,
    pub nudge_step: i32,
}

#[derive(Debug, Clone)]
pub struct FaceDrag {
    pub initial_face: Face,
    pub active_face: Face,
    pub original_box: BoundingBox,
    /// Tracking plane axis index (`0` = X, `1` = Y, `2` = Z), matching MCEdit's `dragResizeDimension`.
    pub tracking_dim: usize,
    /// Coordinate of the tracking plane along `tracking_dim`, matching MCEdit's `dragResizePosition`.
    pub tracking_pos: f32,
}

impl SelectionState {
    pub fn ensure_nudge_step(&mut self) -> i32 {
        if self.nudge_step <= 0 {
            self.nudge_step = 1;
        }
        self.nudge_step
    }

    pub fn clear(&mut self) {
        self.box_ = None;
        self.corner_a = None;
        self.click_selecting = false;
        self.creating_drag_start = None;
        self.dragging_face = None;
    }
}

/// Port of MCEdit-Unified's `EditorTool.findBestTrackingPlane(face)`:
/// Zeroes out the camera vector along `face.axis()` and returns the axis (`0..3`)
/// with the largest absolute camera vector component.
pub fn find_best_tracking_dim(face: Face, camera_forward: Vec3) -> usize {
    let mut cv = camera_forward.abs().to_array();
    cv[face.axis()] = 0.0;
    if cv[0] >= cv[1] && cv[0] >= cv[2] {
        0
    } else if cv[1] >= cv[2] {
        1
    } else {
        2
    }
}

/// Handles left mouse button down for the Selection tool:
/// 1. Checks `box_face_under_cursor` (with MCEdit's 10% silhouette-edge click-through so every
///    face — including bottom and back — can be grabbed through front faces from any angle).
/// 2. If in two-click mode, completes the selection at the clicked block.
/// 3. Otherwise begins click-and-drag selection creation.
pub fn handle_left_click(editor: &mut EditorState, ray_origin: Vec3, ray_dir: Vec3) {
    let dir = ray_dir.normalize_or(Vec3::NEG_Z);
    let cam_fwd = editor.camera.forward_vec();

    // 1. Grab any selection wall (including click-through back/bottom walls) from any distance
    if !editor.selection.click_selecting
        && let Some(b) = editor.selection.box_
        && let Some((face, hit_pt)) =
            b.box_face_under_cursor(ray_origin, dir, cam_fwd, EDGE_CLICKTHROUGH_FACTOR)
    {
        let tracking_dim = find_best_tracking_dim(face, dir);
        editor.selection.dragging_face = Some(FaceDrag {
            initial_face: face,
            active_face: face,
            original_box: b,
            tracking_dim,
            tracking_pos: hit_pt[tracking_dim],
        });
        return;
    }

    let target = editor.selection_target_pos(ray_origin, dir);

    // 2. Second click of a classic two-click selection
    if editor.selection.click_selecting
        && let Some(ca) = editor.selection.corner_a.take()
    {
        editor.selection.box_ = Some(
            editor.clamp_selection_box(BoundingBox::from_corners_inclusive(ca, target)),
        );
        editor.selection.click_selecting = false;
        editor.selection.creating_drag_start = None;
        return;
    }

    // 3. Begin click-and-drag creation (turns into two-click mode if released on the same block)
    editor.selection.creating_drag_start = Some(target);
    editor.selection.corner_a = Some(target);
    editor.selection.click_selecting = false;
    editor.selection.box_ = Some(
        editor.clamp_selection_box(BoundingBox::from_corners_inclusive(target, target)),
    );
}

pub fn handle_left_drag(editor: &mut EditorState, ray_origin: Vec3, ray_dir: Vec3) {
    let dir = ray_dir.normalize_or(Vec3::NEG_Z);

    if let Some(mut drag) = editor.selection.dragging_face.clone() {
        // Port of MCEdit-Unified's `dragResizePoint` + `selectionPointsFromDragResize`
        let dim = drag.tracking_dim;
        let distance = drag.tracking_pos - ray_origin[dim];
        let denom = if dir[dim].abs() > 1e-4 {
            dir[dim]
        } else {
            dir[dim].signum() * 1e-4
        };
        let scale = distance / denom;
        if scale < 0.0 {
            return;
        }
        let point = ray_origin + dir * scale;

        let drag_axis = drag.initial_face.axis();
        let is_max = drag.initial_face.is_max_side();
        let mut o = drag.original_box.min;
        let mut m = drag.original_box.max;
        let new_plane = (point[drag_axis] + 0.5).floor() as i32;

        if is_max {
            m[drag_axis] = new_plane;
        } else {
            o[drag_axis] = new_plane;
        }
        let m_inclusive = m - IVec3::ONE;
        let other_face_pos = if is_max {
            drag.original_box.min[drag_axis] as f32
        } else {
            drag.original_box.max[drag_axis] as f32
        };
        let direction = if is_max { -1.0 } else { 1.0 };
        drag.active_face = if (point[drag_axis] - other_face_pos) * direction > 0.0 {
            drag.initial_face.opposite()
        } else {
            drag.initial_face
        };

        let clamped_o = editor.clamp_block_pos_y(o);
        let clamped_m = editor.clamp_block_pos_y(m_inclusive);
        editor.selection.box_ = Some(
            editor.clamp_selection_box(BoundingBox::from_corners_inclusive(clamped_o, clamped_m)),
        );
        editor.selection.dragging_face = Some(drag);
        return;
    }

    if let Some(start) = editor.selection.creating_drag_start {
        let current = editor.selection_drag_target_pos(ray_origin, dir, start);
        editor.selection.box_ = Some(
            editor.clamp_selection_box(BoundingBox::from_corners_inclusive(start, current)),
        );
    }
}

pub fn handle_left_release(editor: &mut EditorState) {
    if editor.selection.dragging_face.take().is_some() {
        return;
    }
    if let Some(start) = editor.selection.creating_drag_start.take() {
        let dragged_to_box = editor
            .selection
            .box_
            .is_some_and(|b| b.size() != IVec3::ONE);
        if dragged_to_box {
            editor.selection.corner_a = None;
            editor.selection.click_selecting = false;
        } else {
            editor.selection.corner_a = Some(start);
            editor.selection.click_selecting = true;
            editor.selection.box_ = None;
        }
    }
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    ui.heading("Selection Tool");
    ui.separator();

    if let Some(mut b) = editor.selection.box_ {
        let sz = b.size();
        ui.label(format!(
            "Size: {} x {} x {} ({} blocks)",
            sz.x,
            sz.y,
            sz.z,
            b.volume()
        ));
        let (min_y, max_y) = editor.world_y_bounds();
        ui.horizontal(|ui| {
            ui.label("Min:");
            ui.add(egui::DragValue::new(&mut b.min.x));
            ui.add(egui::DragValue::new(&mut b.min.y).range(min_y..=(max_y - 1)));
            ui.add(egui::DragValue::new(&mut b.min.z));
        });
        ui.horizontal(|ui| {
            ui.label("Max:");
            ui.add(egui::DragValue::new(&mut b.max.x));
            ui.add(egui::DragValue::new(&mut b.max.y).range((min_y + 1)..=max_y));
            ui.add(egui::DragValue::new(&mut b.max.z));
        });
        b = editor.clamp_selection_box(b);
        editor.selection.box_ = Some(b);

        ui.separator();
        ui.label("Nudge Box:");
        let step = editor.selection.ensure_nudge_step();
        ui.horizontal(|ui| {
            ui.label("Step:");
            ui.add(egui::DragValue::new(&mut editor.selection.nudge_step).range(1..=64));
        });
        ui.horizontal(|ui| {
            if ui.button("-X").clicked() {
                editor.selection.box_ = Some(b.shift(IVec3::new(-step, 0, 0)));
            }
            if ui.button("+X").clicked() {
                editor.selection.box_ = Some(b.shift(IVec3::new(step, 0, 0)));
            }
            if ui.button("-Y").clicked() {
                let dy = (-step).max(min_y - b.min.y);
                editor.selection.box_ = Some(editor.clamp_selection_box(b.shift(IVec3::new(0, dy, 0))));
            }
            if ui.button("+Y").clicked() {
                let dy = step.min(max_y - b.max.y);
                editor.selection.box_ = Some(editor.clamp_selection_box(b.shift(IVec3::new(0, dy, 0))));
            }
            if ui.button("-Z").clicked() {
                editor.selection.box_ = Some(b.shift(IVec3::new(0, 0, -step)));
            }
            if ui.button("+Z").clicked() {
                editor.selection.box_ = Some(b.shift(IVec3::new(0, 0, step)));
            }
        });

        ui.separator();
        if ui.button("Select Chunks in Box").clicked() {
            editor.selected_chunks.clear();
            for cp in b.iter_chunks() {
                editor.selected_chunks.insert(cp);
            }
            editor.set_toast(format!("Selected {} chunks.", editor.selected_chunks.len()));
        }
        if ui.button("Select All (Full Height)").clicked()
            && let Some(w) = &editor.world
        {
            let mut full = b;
            full.min.y = w.min_y();
            full.max.y = w.max_y();
            editor.selection.box_ = Some(full);
        }
        ui.horizontal(|ui| {
            if ui.button("Copy").clicked() {
                editor.copy_selection();
            }
            if ui.button("Cut").clicked() {
                editor.cut_selection(renderer);
            }
            if ui.button("Export .schematic").clicked() {
                editor.export_selection_schematic();
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Analyze...").clicked() {
                editor.analyze_selection_async();
            }
            if ui.button("Delete Blocks").clicked() {
                editor.delete_selected_blocks(renderer);
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Delete Entities").clicked()
                && let Some(w) = &mut editor.world
            {
                let dim = w.active_dim;
                let mut tx = EditTransaction::new("Delete Entities", dim, Some(b));
                let n = delete_entities_in_box(w, dim, b, &mut tx);
                tx.commit(w);
                editor.set_toast(format!("Deleted {n} entities."));
            }
            if ui.button("Delete TileTicks").clicked()
                && let Some(w) = &mut editor.world
            {
                let dim = w.active_dim;
                let mut tx = EditTransaction::new("Delete TileTicks", dim, Some(b));
                let n = delete_tile_ticks_in_box(w, dim, b, &mut tx);
                tx.commit(w);
                editor.set_toast(format!("Deleted {n} tile ticks."));
            }
        });
        if ui.button("Deselect").clicked() {
            editor.selection.clear();
        }
    } else {
        if let Some(ca) = editor.selection.corner_a {
            ui.label(format!("Corner 1: ({}, {}, {})", ca.x, ca.y, ca.z));
            ui.label("Click a second block anywhere to complete box.");
        } else {
            ui.label("Click & drag to create a selection, or click two corners.");
            ui.label("Drag any wall to resize; hover near edges to grab opposite walls.");
        }
        if ui.button("Select Chunks -> Box").clicked() && !editor.selected_chunks.is_empty() {
            let mut min_c = ChunkPos::new(i32::MAX, i32::MAX);
            let mut max_c = ChunkPos::new(i32::MIN, i32::MIN);
            for &cp in &editor.selected_chunks {
                min_c.0 = min_c.0.min(cp.0);
                max_c.0 = max_c.0.max(cp.0);
            }
            let (min_y, max_y) = editor
                .world
                .as_ref()
                .map(|w| (w.min_y(), w.max_y()))
                .unwrap_or((0, 256));
            editor.selection.box_ = Some(BoundingBox {
                min: glam::IVec3::new(min_c.cx() * 16, min_y, min_c.cz() * 16),
                max: glam::IVec3::new((max_c.cx() + 1) * 16, max_y, (max_c.cz() + 1) * 16),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use mcedit_render::RaycastHit;

    #[test]
    fn selection_drag_create_two_click_and_distant_face_resize() {
        let mut ed = EditorState::new(AppConfig::default());

        // 1. Click-and-drag creation
        ed.hover_hit = Some(RaycastHit {
            block_pos: IVec3::new(10, 60, 10),
            adjacent_pos: IVec3::new(10, 61, 10),
            face: Face::Up,
            distance: 20.0,
        });
        handle_left_click(&mut ed, Vec3::new(10.0, 84.0, 10.0), Vec3::NEG_Y);
        assert_eq!(ed.selection.creating_drag_start, Some(IVec3::new(10, 60, 10)));

        // Drag mouse to (20, 70, 20) -> 11x11x11 box [10..21, 60..71, 10..21]
        ed.hover_hit = Some(RaycastHit {
            block_pos: IVec3::new(20, 70, 20),
            adjacent_pos: IVec3::new(20, 71, 20),
            face: Face::Up,
            distance: 25.0,
        });
        handle_left_drag(
            &mut ed,
            Vec3::new(10.0, 84.0, 10.0),
            (Vec3::new(20.5, 70.5, 20.5) - Vec3::new(10.0, 84.0, 10.0)).normalize(),
        );
        handle_left_release(&mut ed);
        assert_eq!(
            ed.selection.box_,
            Some(BoundingBox::from_corners_inclusive(
                IVec3::new(10, 60, 10),
                IVec3::new(20, 70, 20)
            ))
        );
        assert!(!ed.selection.click_selecting);

        // 2. Grab East face (+X at x=21) in the center of the face from 600 blocks away
        ed.hover_hit = None;
        let far_origin = Vec3::new(621.0, 100.0, 15.5);
        ed.camera.pos = far_origin;
        let aim_east = (Vec3::new(21.0, 65.5, 15.5) - far_origin).normalize();
        handle_left_click(&mut ed, far_origin, aim_east);
        assert!(ed.selection.dragging_face.is_some());
        assert_eq!(
            ed.selection.dragging_face.as_ref().unwrap().initial_face,
            Face::East
        );

        // Drag East face +10 blocks outward (to x=31)
        let aim_new = (Vec3::new(31.0, 65.5, 15.5) - far_origin).normalize();
        handle_left_drag(&mut ed, far_origin, aim_new);
        handle_left_release(&mut ed);
        assert_eq!(ed.selection.box_.unwrap().max.x, 31);

        // 3. MCEdit click-through edge test:
        // Looking from top-left (-X, +Y) at the West face (x=10) near its bottom edge (y=60.3, within 10% of 60..71)
        // must click through the West face and grab the BOTTOM (Face::Down) of the selection!
        let top_left_cam = Vec3::new(0.0, 80.0, 15.5);
        let near_bottom_edge_on_west = Vec3::new(10.0, 60.4, 15.5);
        let click_ray = (near_bottom_edge_on_west - top_left_cam).normalize();
        let b = ed.selection.box_.unwrap();
        let (grabbed_face, _) = b
            .box_face_under_cursor(top_left_cam, click_ray, click_ray, EDGE_CLICKTHROUGH_FACTOR)
            .expect("should hit West face and click through to Down face");
        assert_eq!(
            grabbed_face,
            Face::Down,
            "Hovering near bottom edge of West face from top-left must grab Face::Down!"
        );
    }
}
