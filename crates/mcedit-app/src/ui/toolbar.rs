use crate::editor::EditorState;
use crate::tools::ToolId;
use egui::{Color32, RichText, Ui};

pub fn draw(ui: &mut Ui, editor: &mut EditorState) {
    if editor.world.is_none() {
        return;
    }
    egui::Panel::bottom("bottom_toolbar")
        .exact_size(42.0)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(8.0);
                for tool in ToolId::ALL {
                    let active = editor.active_tool == tool && editor.should_show_left_panel();
                    let label = if tool == ToolId::Compare && editor.compare.is_active() {
                        format!("[{}] {} *", tool.hotkey_label(), tool.name())
                    } else {
                        format!("[{}] {}", tool.hotkey_label(), tool.name())
                    };
                    let text = if tool == ToolId::Compare && editor.compare.is_active() {
                        RichText::new(label).color(Color32::from_rgb(255, 130, 130))
                    } else {
                        RichText::new(label)
                    };
                    if ui.selectable_label(active, text).clicked() {
                        editor.switch_tool(tool);
                    }
                }
                if let Some(wb) = &editor.compare.world_b {
                    ui.separator();
                    let badge = if editor.compare.config.enabled {
                        RichText::new(format!("vs {}", wb.display_name))
                            .small()
                            .color(Color32::from_rgb(255, 150, 150))
                    } else {
                        RichText::new(format!("vs {} (paused)", wb.display_name))
                            .small()
                            .color(Color32::from_gray(150))
                    };
                    if ui.button(badge).on_hover_text("Open Compare panel").clicked() {
                        editor.switch_tool(ToolId::Compare);
                    }
                }
            });
        });
}
