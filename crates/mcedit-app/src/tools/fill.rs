use crate::editor::EditorState;
use crate::ui::block_picker::{BlockPickerModal, BlockPickerTarget};
use egui::Ui;
use mcedit_core::{BlockState, EditTransaction, TransformOp, fill_blocks, transform_block_state};
use mcedit_render::WorldRenderer;

pub struct FillToolState {
    pub fill_block: BlockState,
    pub replace_mode: bool,
    pub find_block: BlockState,
    pub match_base_only: bool,
}

impl Default for FillToolState {
    fn default() -> Self {
        Self {
            fill_block: BlockState::new("minecraft:stone"),
            replace_mode: false,
            find_block: BlockState::new("minecraft:dirt"),
            match_base_only: false,
        }
    }
}

pub fn apply_transform(editor: &mut EditorState, op: TransformOp) {
    editor.fill.fill_block = transform_block_state(&editor.fill.fill_block, op);
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState, renderer: &mut WorldRenderer) {
    ui.heading("Fill & Replace");
    ui.separator();

    ui.checkbox(&mut editor.fill.replace_mode, "Replace Mode");

    if editor.fill.replace_mode {
        ui.label("Find Block:");
        if ui
            .button(editor.fill.find_block.to_string())
            .clicked()
        {
            editor.block_picker = Some(BlockPickerModal::new(
                BlockPickerTarget::FillFind,
                editor.fill.find_block.clone(),
                true,
                editor.fill.match_base_only,
            ));
        }
        ui.checkbox(&mut editor.fill.match_base_only, "Any Subtype / Properties");

        if ui.button("Swap Find / Replace").clicked() {
            std::mem::swap(&mut editor.fill.find_block, &mut editor.fill.fill_block);
        }
        ui.separator();
    }

    ui.label("Fill With:");
    if ui.button(editor.fill.fill_block.to_string()).clicked() {
        editor.block_picker = Some(BlockPickerModal::new(
            BlockPickerTarget::FillBlock,
            editor.fill.fill_block.clone(),
            false,
            false,
        ));
    }

    ui.horizontal(|ui| {
        if ui.button("Rotate CW").clicked() {
            apply_transform(editor, TransformOp::RotateCW);
        }
        if ui.button("Flip Y").clicked() {
            apply_transform(editor, TransformOp::FlipY);
        }
    });

    ui.separator();
    let has_sel = editor.selection.box_.is_some();
    if ui
        .add_enabled(has_sel, egui::Button::new("Apply Fill / Replace"))
        .clicked()
        && let Some(box_) = editor.selection.box_
        && let Some(w) = &mut editor.world
    {
        let dim = w.active_dim;
        let fill_id = w.registry.write().intern(editor.fill.fill_block.clone());
        let target_id = if editor.fill.replace_mode {
            Some(w.registry.write().intern(editor.fill.find_block.clone()))
        } else {
            None
        };
        let label = if editor.fill.replace_mode {
            "Replace Blocks"
        } else {
            "Fill Blocks"
        };
        let mut tx = EditTransaction::new(label, dim, Some(box_));
        let changed = fill_blocks(
            w,
            dim,
            box_,
            fill_id,
            target_id,
            editor.fill.match_base_only,
            &mut tx,
        );
        tx.commit(w);
        editor.invalidate_box(box_, renderer);
        editor.set_toast(format!("Modified {changed} blocks."));
    }
    if !has_sel {
        ui.small("Make a selection first with the Select tool.");
    }
}
