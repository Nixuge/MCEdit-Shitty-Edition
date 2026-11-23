use crate::editor::EditorState;
use crate::tools::ToolId;
use egui::Ui;
use mcedit_formats::FormatRegistry;

pub fn draw(ui: &mut Ui, editor: &mut EditorState) {
    if editor.world.is_none() || editor.copy_stack.is_empty() {
        return;
    }

    egui::Panel::right("clipboard_panel")
        .default_size(210.0)
        .show(ui, |ui| {
            ui.heading("Clipboard");
            ui.separator();

            let mut paste_idx = None;
            let mut export_idx = None;
            let mut delete_idx = None;

            egui::ScrollArea::vertical().show(ui, |ui| {
                for (idx, entry) in editor.copy_stack.iter().enumerate() {
                    ui.group(|ui| {
                        ui.strong(&entry.label);
                        let sz = entry.volume.size;
                        ui.small(format!("{} x {} x {}", sz.x, sz.y, sz.z));
                        ui.horizontal(|ui| {
                            if ui.small_button("Paste").clicked() {
                                paste_idx = Some(idx);
                            }
                            if ui.small_button("Save").clicked() {
                                export_idx = Some(idx);
                            }
                            if ui.small_button("X").clicked() {
                                delete_idx = Some(idx);
                            }
                        });
                    });
                }
            });

            if let Some(i) = paste_idx {
                let vol = editor.copy_stack[i].volume.clone();
                let origin = editor
                    .hover_hit
                    .map(|h| h.adjacent_pos)
                    .unwrap_or_else(|| editor.camera.pos.as_ivec3());
                editor.clone.load_volume(vol, origin);
                editor.active_tool = ToolId::Clone;
                editor.preview_dirty = true;
            }
            if let Some(i) = export_idx
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("MCEdit Schematic", &["schematic"])
                    .set_file_name("clipboard.schematic")
                    .save_file()
            {
                let res = {
                    let reg = editor.registry.read();
                    FormatRegistry::save_schematic(&path, &editor.copy_stack[i].volume, &reg)
                };
                match res {
                    Ok(()) => editor.set_toast(format!("Saved {}", path.display())),
                    Err(e) => editor.set_toast(format!("Save failed: {e}")),
                }
            }
            if let Some(i) = delete_idx {
                editor.copy_stack.remove(i);
            }
        });
}
