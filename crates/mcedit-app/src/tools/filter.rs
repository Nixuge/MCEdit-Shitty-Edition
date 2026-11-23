use crate::editor::EditorState;
use crate::jobs::JobOutcome;
use crate::ui::param_ui;
use egui::Ui;
use mcedit_core::{BlockState, EditTransaction};
use mcedit_plugins::{
    ParamKind, ParamMap, ParamValue, PluginContext, PluginRegistry, ProgressHandle,
    default_param_map,
};
use std::collections::HashMap;

pub struct FilterToolState {
    pub selected_idx: usize,
    pub params_by_id: HashMap<String, ParamMap>,
}

impl FilterToolState {
    pub fn new(plugins: &PluginRegistry) -> Self {
        let mut params_by_id = HashMap::new();
        for f in &plugins.filters {
            params_by_id.insert(f.id().to_string(), default_param_map(&f.params()));
        }
        Self {
            selected_idx: 0,
            params_by_id,
        }
    }
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState) {
    ui.heading("Filter Tool");
    ui.separator();

    if editor.plugins.filters.is_empty() {
        ui.label("No filters registered.");
        return;
    }

    let cur_label = editor
        .plugins
        .filters
        .get(editor.filter.selected_idx)
        .map(|f| format!("[{}] {}", f.category(), f.display_name()))
        .unwrap_or_default();

    egui::ComboBox::from_label("Filter")
        .selected_text(cur_label)
        .show_ui(ui, |ui| {
            for (i, f) in editor.plugins.filters.iter().enumerate() {
                ui.selectable_value(
                    &mut editor.filter.selected_idx,
                    i,
                    format!("[{}] {}", f.category(), f.display_name()),
                );
            }
        });

    ui.separator();
    if let Some(filter) = editor.plugins.filters.get(editor.filter.selected_idx) {
        let fid = filter.id().to_string();
        let defs = filter.params();
        let params = editor.filter.params_by_id.entry(fid.clone()).or_default();

        if let Some(pick_key) = param_ui::draw_param_form(ui, &defs, params) {
            let current = match params.get(&pick_key) {
                Some(ParamValue::Block {
                    state,
                    match_base_only,
                }) => (state.clone(), *match_base_only),
                _ => (BlockState::new("minecraft:stone"), false),
            };
            let allow_wild = defs.iter().any(|d| {
                d.key == pick_key
                    && matches!(d.kind, ParamKind::Block { allow_wildcard: true, .. })
            });
            editor.block_picker = Some(crate::ui::block_picker::BlockPickerModal::new(
                crate::ui::block_picker::BlockPickerTarget::FilterParam {
                    filter_id: fid.clone(),
                    key: pick_key,
                },
                current.0,
                allow_wild,
                current.1,
            ));
        }

        ui.separator();
        let has_sel = editor.selection.box_.is_some() && !editor.jobs.is_busy();
        if ui
            .add_enabled(has_sel, egui::Button::new("Run Filter"))
            .clicked()
            && let Some(sel) = editor.selection.box_
            && let Some(world) = editor.world.take()
        {
            let filter_idx = editor.filter.selected_idx;
            let params_clone = editor
                .filter
                .params_by_id
                .get(&fid)
                .cloned()
                .unwrap_or_default();
            let title = format!("Filter: {}", filter.display_name());

            editor.jobs.spawn_world_job(title, world, move |mut w, job_prog| {
                let plugins = PluginRegistry::with_stock_plugins();
                let Some(f) = plugins.filters.get(filter_idx) else {
                    return JobOutcome::Error {
                        world: Some(w),
                        error: "Filter missing".into(),
                    };
                };
                let dim = w.active_dim;
                let mut tx = EditTransaction::new(f.display_name(), dim, Some(sel));
                let mut ctx = PluginContext {
                    world: &mut w,
                    dim,
                    tx: &mut tx,
                    progress: ProgressHandle::new(job_prog.cancelled.clone()),
                    active_clipboard: None,
                    message: None,
                };
                job_prog.set(1, 2, format!("Running {}...", f.display_name()));
                match f.execute(&mut ctx, sel, &params_clone) {
                    Ok(()) => {
                        let msg = ctx
                            .message
                            .unwrap_or_else(|| format!("Completed filter: {}", f.display_name()));
                        tx.commit(&mut w);
                        JobOutcome::WorldUpdated {
                            world: w,
                            status: msg,
                            invalidate_all: false,
                            dirty_box: Some(sel),
                        }
                    }
                    Err(e) => JobOutcome::Error {
                        world: Some(w),
                        error: e,
                    },
                }
            });
        }
        if editor.selection.box_.is_none() {
            ui.small("Make a selection first with the Select tool.");
        }
    }
}
