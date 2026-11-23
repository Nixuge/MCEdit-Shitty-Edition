use crate::editor::EditorState;
use egui::Context;
use mcedit_core::{BlockState, BlockStateId};
use mcedit_plugins::ParamValue;

#[derive(Debug, Clone)]
pub enum BlockPickerTarget {
    FillBlock,
    FillFind,
    BrushParam { brush_id: String, key: String },
    FilterParam { filter_id: String, key: String },
}

pub struct BlockPickerModal {
    pub target: BlockPickerTarget,
    pub query: String,
    pub selected: BlockState,
    pub allow_wildcard: bool,
    pub match_base_only: bool,
    pub cached_ids: Vec<BlockStateId>,
    pub last_query: String,
}

impl BlockPickerModal {
    pub fn new(
        target: BlockPickerTarget,
        initial: BlockState,
        allow_wildcard: bool,
        match_base_only: bool,
    ) -> Self {
        Self {
            target,
            query: String::new(),
            selected: initial,
            allow_wildcard,
            match_base_only,
            cached_ids: Vec::new(),
            last_query: "__init__".to_string(),
        }
    }
}

pub fn draw(ctx: &Context, editor: &mut EditorState, atlas_tex_id: Option<egui::TextureId>) {
    let Some(mut modal) = editor.block_picker.take() else {
        return;
    };

    if modal.query != modal.last_query {
        let reg = editor.registry.read();
        modal.cached_ids = reg.search(&modal.query);
        modal.last_query = modal.query.clone();
    }

    let mut close = false;
    let mut confirm = false;

    egui::Window::new("Block Picker")
        .collapsible(false)
        .resizable(true)
        .default_width(480.0)
        .default_height(520.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Search:");
                let resp = ui.text_edit_singleline(&mut modal.query);
                if modal.last_query.is_empty() {
                    resp.request_focus();
                }
                if ui.button("Clear").clicked() {
                    modal.query.clear();
                }
            });

            if modal.allow_wildcard {
                ui.checkbox(
                    &mut modal.match_base_only,
                    "Match Any Subtype / Properties",
                );
            }

            ui.separator();
            let reg = editor.registry.read();
            let row_h = 24.0;
            let total_rows = modal.cached_ids.len();

            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show_rows(ui, row_h, total_rows, |ui, range| {
                    for idx in range {
                        let id = modal.cached_ids[idx];
                        let st = reg.get_state(id);
                        let meta = reg.get_meta(id);
                        let is_sel = st == &modal.selected;

                        ui.horizontal(|ui| {
                            if let Some(tid) = atlas_tex_id {
                                let tile = meta.face_tiles[2]; // top face
                                let u0 = tile[0] as f32 / 32.0;
                                let v0 = tile[1] as f32 / 32.0;
                                let uv = egui::Rect::from_min_max(
                                    egui::pos2(u0, v0),
                                    egui::pos2(u0 + 1.0 / 32.0, v0 + 1.0 / 32.0),
                                );
                                let img = egui::Image::new(egui::load::SizedTexture::new(
                                    tid,
                                    egui::vec2(18.0, 18.0),
                                ))
                                .uv(uv);
                                ui.add(img);
                            }

                            let leg_str = meta
                                .legacy_id
                                .map(|(bid, d)| format!(" [{bid}:{d}]"))
                                .unwrap_or_default();
                            let label = format!("{}{}  ({})", meta.display_name, leg_str, st);
                            let resp = ui.selectable_label(is_sel, label);
                            if resp.clicked() {
                                modal.selected = st.clone();
                            }
                            if resp.double_clicked() {
                                modal.selected = st.clone();
                                confirm = true;
                            }
                        });
                    }
                });

            ui.separator();
            ui.label(format!("Selected: {}", modal.selected));
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    confirm = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if confirm {
        match modal.target {
            BlockPickerTarget::FillBlock => {
                editor.fill.fill_block = modal.selected;
            }
            BlockPickerTarget::FillFind => {
                editor.fill.find_block = modal.selected;
                editor.fill.match_base_only = modal.match_base_only;
            }
            BlockPickerTarget::BrushParam { brush_id, key } => {
                editor
                    .brush
                    .params_by_id
                    .entry(brush_id)
                    .or_default()
                    .insert(
                        key,
                        ParamValue::Block {
                            state: modal.selected,
                            match_base_only: modal.match_base_only,
                        },
                    );
                editor.preview_dirty = true;
            }
            BlockPickerTarget::FilterParam { filter_id, key } => {
                editor
                    .filter
                    .params_by_id
                    .entry(filter_id)
                    .or_default()
                    .insert(
                        key,
                        ParamValue::Block {
                            state: modal.selected,
                            match_base_only: modal.match_base_only,
                        },
                    );
            }
        }
    } else if !close {
        editor.block_picker = Some(modal);
    }
}
