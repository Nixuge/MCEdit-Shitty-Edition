use crate::editor::EditorState;
use crate::tools::ToolId;
use egui::Ui;

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
                    let label = format!("[{}] {}", tool.hotkey_label(), tool.name());
                    if ui.selectable_label(active, label).clicked() {
                        editor.switch_tool(tool);
                    }
                }
            });
        });
}
