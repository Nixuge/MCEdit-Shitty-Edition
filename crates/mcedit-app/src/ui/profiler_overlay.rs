use crate::editor::EditorState;
use egui::{Color32, Context, Pos2, Sense, Stroke, vec2};
use mcedit_core::profiler::{self, Counter, Milestone, Stage};
use mcedit_render::WorldRenderer;

fn ms_label(v: Option<f64>) -> String {
    v.map(|m| format!("{m:.0} ms")).unwrap_or_else(|| "-".to_string())
}

pub fn draw(ctx: &Context, editor: &mut EditorState, renderer: &WorldRenderer) {
    if !editor.show_profiler_overlay {
        return;
    }
    let mut open = true;
    egui::Window::new("Performance (F3)")
        .open(&mut open)
        .default_width(380.0)
        .anchor(egui::Align2::RIGHT_TOP, [-8.0, 40.0])
        .show(ctx, |ui| {
            let st = profiler::frame_stats();
            let frames = profiler::frames();
            ui.label(format!(
                "{:.0} fps | avg {:.2} ms | p95 {:.2} ms | max {:.2} ms",
                st.fps, st.avg_ms, st.p95_ms, st.max_ms
            ));

            // Frametime sparkline with 60 Hz / 30 Hz reference lines
            let (resp, painter) =
                ui.allocate_painter(vec2(ui.available_width(), 64.0), Sense::hover());
            let rect = resp.rect;
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(160));
            let top_ms = st.max_ms.max(33.4);
            let y_of = |ms: f32| rect.bottom() - (ms / top_ms).clamp(0.0, 1.0) * rect.height();
            for (ms, col) in [(16.67, Color32::DARK_GREEN), (33.33, Color32::DARK_RED)] {
                painter.line_segment(
                    [Pos2::new(rect.left(), y_of(ms)), Pos2::new(rect.right(), y_of(ms))],
                    Stroke::new(1.0, col),
                );
            }
            if frames.len() > 1 {
                let dx = rect.width() / (profiler::FRAME_HISTORY as f32 - 1.0);
                let pts: Vec<Pos2> = frames
                    .iter()
                    .enumerate()
                    .map(|(i, f)| Pos2::new(rect.left() + i as f32 * dx, y_of(f.total_ms)))
                    .collect();
                for w in pts.windows(2) {
                    painter.line_segment([w[0], w[1]], Stroke::new(1.5, Color32::LIGHT_GREEN));
                }
            }

            if !frames.is_empty() {
                let n = frames.len() as f32;
                let avg = |f: fn(&profiler::FrameSample) -> f32| {
                    frames.iter().map(f).sum::<f32>() / n
                };
                ui.label(format!(
                    "CPU/frame avg: pump {:.2} | ray {:.2} | render {:.2} | blur {:.2} | egui {:.2} | present/vsync {:.2} ms",
                    avg(|f| f.pump_ms),
                    avg(|f| f.raycast_ms),
                    avg(|f| f.render_ms),
                    avg(|f| f.blur_ms),
                    avg(|f| f.egui_ms),
                    avg(|f| f.present_ms),
                ));
                let last = frames[frames.len() - 1];
                let (_, _, vram) = renderer.stats();
                ui.label(format!(
                    "Draw calls {} | {:.2} MTris | VRAM {:.0} MB",
                    last.draw_calls,
                    last.triangles as f64 / 1e6,
                    vram as f64 / (1024.0 * 1024.0)
                ));
            }

            ui.separator();
            ui.strong("Streaming");
            ui.label(format!(
                "First chunk {} | 8x8 area {} | Full detail {}",
                ms_label(profiler::milestone_ms(Milestone::FirstChunk)),
                ms_label(profiler::milestone_ms(Milestone::Near8x8)),
                ms_label(profiler::milestone_ms(Milestone::FullDetail)),
            ));
            ui.label(format!(
                "{} chunks decoded | {} meshed | {} batches rebuilt",
                profiler::counter(Counter::ChunksDiskLoaded),
                profiler::counter(Counter::Lod0Chunks),
                profiler::counter(Counter::BatchesRebuilt),
            ));

            let worker_stages = [
                Stage::ChunkRead,
                Stage::NbtParse,
                Stage::ChunkDecode,
                Stage::Lod0MeshFull,
                Stage::BatchMerge,
                Stage::LodVoxelLoad,
                Stage::LodVoxelMesh,
                Stage::LodSurfaceMesh,
            ];
            let mut rows: Vec<profiler::StageStat> = profiler::snapshot()
                .into_iter()
                .filter(|s| worker_stages.contains(&s.stage) && s.calls > 0)
                .collect();
            rows.sort_by_key(|s| std::cmp::Reverse(s.total_us));
            let total: u64 = rows.iter().map(|s| s.total_us).sum::<u64>().max(1);
            egui::Grid::new("prof_stage_grid").striped(true).show(ui, |ui| {
                ui.strong("Stage (CPU-time, all threads)");
                ui.strong("calls");
                ui.strong("total ms");
                ui.strong("avg us");
                ui.strong("%");
                ui.end_row();
                for s in rows {
                    ui.label(s.stage.label());
                    ui.label(s.calls.to_string());
                    ui.label(format!("{:.1}", s.total_us as f64 / 1000.0));
                    ui.label(format!("{:.0}", s.avg_us()));
                    ui.label(format!("{:.0}", s.total_us as f64 * 100.0 / total as f64));
                    ui.end_row();
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .checkbox(&mut editor.config.vsync, "VSync")
                    .on_hover_text("Uncheck to measure uncapped frame throughput")
                    .changed()
                {
                    editor.config.save();
                }
                let rec = profiler::trace_enabled();
                if ui
                    .button(if rec { "Stop & Save Trace (F4)" } else { "Record Trace (F4)" })
                    .clicked()
                {
                    editor.toggle_trace();
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Reset Counters").clicked() {
                    profiler::reset();
                }
                if ui.button("Copy Report").clicked() {
                    ui.ctx().copy_text(profiler::format_report());
                }
            });
        });
    editor.show_profiler_overlay = open;
}
