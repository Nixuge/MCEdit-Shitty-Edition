use crate::editor::EditorState;
use egui::Ui;
use mcedit_render::WorldRenderer;
use std::time::Duration;

pub fn draw(ui: &mut Ui, editor: &EditorState, renderer: &WorldRenderer, fps: f32) {
    egui::Panel::bottom("status_bar")
        .exact_size(24.0)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let p = editor.camera.pos;
                ui.label(format!("Cam: ({:.1}, {:.1}, {:.1})", p.x, p.y, p.z));
                ui.separator();

                if let Some(hit) = editor.hover_hit {
                    let bp = hit.block_pos;
                    ui.label(format!("Cursor: ({}, {}, {})", bp.x, bp.y, bp.z));
                    if editor.compare.is_active()
                        && let Some((kind, id_a, id_b)) = editor.inspect_compare_block(bp)
                        && kind != mcedit_core::DiffKind::Match
                    {
                        ui.separator();
                        let reg = editor.registry.read();
                        let short = |id: mcedit_core::BlockStateId| -> String {
                            if id == mcedit_core::BlockStateId::AIR {
                                "air".to_string()
                            } else {
                                let s = &reg.get_state(id).name;
                                s.strip_prefix("minecraft:").unwrap_or(s).to_string()
                            }
                        };
                        match kind {
                            mcedit_core::DiffKind::Mismatch => {
                                ui.colored_label(
                                    egui::Color32::from_rgb(255, 110, 110),
                                    format!("Diff: {} != {}", short(id_a), short(id_b)),
                                );
                            }
                            mcedit_core::DiffKind::OnlyInA => {
                                ui.colored_label(
                                    egui::Color32::from_rgb(100, 200, 255),
                                    format!("Only A: {}", short(id_a)),
                                );
                            }
                            mcedit_core::DiffKind::OnlyInB => {
                                ui.colored_label(
                                    egui::Color32::from_rgb(95, 145, 255),
                                    format!("Only B: {}", short(id_b)),
                                );
                            }
                            mcedit_core::DiffKind::Match => {}
                        }
                    }
                } else {
                    ui.label("Cursor: -");
                }
                ui.separator();

                if let Some(b) = editor.selection.box_ {
                    let s = b.size();
                    ui.label(format!("Sel: {}x{}x{}", s.x, s.y, s.z));
                    ui.separator();
                }

                let (lod0_cnt, lod_reg_cnt, vram_b) = renderer.stats();
                ui.label(format!(
                    "FPS: {:.0} | Speed: {:.0} b/s | 3D: {} | LOD: {} ({:.1} MB)",
                    fps,
                    editor.config.camera_speed,
                    lod0_cnt,
                    lod_reg_cnt,
                    vram_b as f64 / (1024.0 * 1024.0)
                ));

                if let Some((msg, at)) = &editor.status_toast
                    && at.elapsed() < Duration::from_secs(5)
                {
                    ui.separator();
                    ui.colored_label(egui::Color32::LIGHT_GREEN, msg);
                }

                if let Some(proc_msg) = editor.streaming_status() {
                    ui.separator();
                    ui.spinner();
                    ui.colored_label(egui::Color32::from_rgb(255, 200, 70), proc_msg);
                }
            });
        });
}
