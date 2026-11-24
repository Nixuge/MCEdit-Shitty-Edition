use crate::config::{FileMenuEscapeBehavior, InvalidWorldBehavior, PanelBackgroundStyle};
use crate::editor::EditorState;
use egui::Context;
use mcedit_render::WorldRenderer;

pub fn draw(ctx: &Context, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    if !editor.settings_open {
        return;
    }
    let mut open = editor.settings_open;
    egui::Window::new("Settings & Keybindings")
        .open(&mut open)
        .resizable(true)
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.heading("Graphics & Camera");
            if ui
                .add(
                    egui::Slider::new(&mut editor.config.view_distance, 2..=1024)
                        .logarithmic(true)
                        .text("Render Distance (Chunks)"),
                )
                .changed()
            {
                editor.config.save();
            }
            if ui
                .add(
                    egui::Slider::new(&mut editor.config.detail_distance, 0..=128)
                        .text("LOD Start Distance (Chunks)"),
                )
                .changed()
            {
                editor.config.save();
            }
            if ui
                .add(
                    egui::Slider::new(&mut editor.config.camera_speed, 2.0..=500.0)
                        .logarithmic(true)
                        .text("Flight Speed (blocks/s)"),
                )
                .changed()
            {
                editor.camera.base_speed = editor.config.camera_speed;
                editor.config.save();
            }
            if ui
                .add(egui::Slider::new(&mut editor.config.fov_deg, 30.0..=110.0).text("FOV"))
                .changed()
            {
                editor.camera.fov_deg = editor.config.fov_deg;
                editor.config.save();
            }
            if ui
                .add(
                    egui::Slider::new(&mut editor.config.mouse_sensitivity, 0.05..=0.6)
                        .text("Mouse Sensitivity"),
                )
                .changed()
            {
                editor.config.save();
            }
            if ui
                .add(
                    egui::Slider::new(&mut editor.config.vram_limit_mb, 128..=16384)
                        .text("VRAM Mesh Limit (MB)"),
                )
                .changed()
            {
                renderer.vram_limit_bytes = editor.config.vram_limit_mb * 1024 * 1024;
                editor.request_reschedule();
                editor.config.save();
            }
            if ui
                .checkbox(&mut editor.config.fly_mode, "Fly Mode (Horizontal Forward)")
                .changed()
            {
                editor.camera.fly_mode = editor.config.fly_mode;
                editor.config.save();
            }
            if ui
                .checkbox(&mut editor.config.autobrake, "Autobrake")
                .changed()
            {
                editor.camera.autobrake = editor.config.autobrake;
                editor.config.save();
            }
            if ui
                .checkbox(
                    &mut editor.config.keep_position_on_reload,
                    "Keep position between map reloads",
                )
                .on_hover_text(
                    "Preserves camera position, angle, and dimension when reloading or switching worlds while the app stays open (resets on app relaunch).",
                )
                .changed()
            {
                editor.config.save();
            }
            if ui
                .checkbox(&mut editor.config.show_sign_text, "Show Sign Text")
                .changed()
            {
                editor.config.save();
            }
            if ui
                .checkbox(
                    &mut editor.config.show_entity_labels,
                    "Show Entity Names & Textures",
                )
                .changed()
            {
                editor.config.save();
            }
            if ui
                .checkbox(&mut editor.config.vsync, "VSync")
                .changed()
            {
                editor.config.save();
            }
            if ui
                .add_enabled(
                    !editor.config.vsync,
                    egui::Slider::new(&mut editor.config.max_fps, 0..=1000)
                        .custom_formatter(|n, _| {
                            let fps = n as u32;
                            if fps == 0 {
                                "Unlimited".to_string()
                            } else {
                                format!("{fps} FPS")
                            }
                        })
                        .text("Max FPS (0 = Unlimited)"),
                )
                .changed()
            {
                editor.config.save();
            }
            ui.horizontal(|ui| {
                ui.label("Left Panel Background:");
                let mut style = editor.config.left_panel_style;
                egui::ComboBox::from_id_salt("left_panel_bg_style")
                    .selected_text(style.label())
                    .show_ui(ui, |ui| {
                        for s in PanelBackgroundStyle::ALL {
                            ui.selectable_value(&mut style, s, s.label());
                        }
                    });
                if style != editor.config.left_panel_style {
                    editor.config.left_panel_style = style;
                    editor.config.save();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Behavior on Open Invalid World:");
                let mut behavior = editor.config.invalid_world_behavior;
                egui::ComboBox::from_id_salt("invalid_world_behavior")
                    .selected_text(behavior.label())
                    .show_ui(ui, |ui| {
                        for b in InvalidWorldBehavior::ALL {
                            ui.selectable_value(&mut behavior, b, b.label());
                        }
                    });
                if behavior != editor.config.invalid_world_behavior {
                    editor.config.invalid_world_behavior = behavior;
                    editor.config.save();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Escape on File Menu:");
                let mut behavior = editor.config.file_menu_escape_behavior;
                egui::ComboBox::from_id_salt("file_menu_escape_behavior")
                    .selected_text(behavior.label())
                    .show_ui(ui, |ui| {
                        for b in FileMenuEscapeBehavior::ALL {
                            ui.selectable_value(&mut behavior, b, b.label());
                        }
                    });
                if behavior != editor.config.file_menu_escape_behavior {
                    editor.config.file_menu_escape_behavior = behavior;
                    editor.config.save();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Undo History Limit:");
                if ui
                    .add(egui::DragValue::new(&mut editor.config.undo_limit).range(1..=100))
                    .changed()
                {
                    if let Some(w) = &mut editor.world {
                        w.undo_stack.set_limit(editor.config.undo_limit);
                    }
                    editor.config.save();
                }
            });

            ui.separator();
            ui.heading("Keybindings");
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                let actions: Vec<_> = editor.config.keybinds.keys().copied().collect();
                let mut changed_any = false;
                for act in actions {
                    if let Some(binding) = editor.config.keybinds.get_mut(&act) {
                        ui.horizontal(|ui| {
                            ui.label(act.label());
                            if ui.text_edit_singleline(binding).changed() {
                                changed_any = true;
                            }
                        });
                    }
                }
                if changed_any {
                    editor.config.save();
                }
            });
        });
    editor.settings_open = open;
}
