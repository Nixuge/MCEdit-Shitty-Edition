use egui::Ui;
use mcedit_plugins::{ParamDef, ParamKind, ParamMap, ParamValue};

/// Draws an automatic `egui` parameter form and returns `Some(key)` if a Block Picker button was clicked.
pub fn draw_param_form(ui: &mut Ui, defs: &[ParamDef], params: &mut ParamMap) -> Option<String> {
    let mut clicked_block_key = None;

    for def in defs {
        match &def.kind {
            ParamKind::Label(text) => {
                ui.label(text);
            }
            ParamKind::Bool { default } => {
                let val = match params
                    .entry(def.key.clone())
                    .or_insert(ParamValue::Bool(*default))
                {
                    ParamValue::Bool(b) => b,
                    other => {
                        *other = ParamValue::Bool(*default);
                        let ParamValue::Bool(b) = other else { unreachable!() };
                        b
                    }
                };
                ui.checkbox(val, &def.label);
            }
            ParamKind::Int {
                default,
                min,
                max,
                step,
            } => {
                let val = match params
                    .entry(def.key.clone())
                    .or_insert(ParamValue::Int(*default))
                {
                    ParamValue::Int(i) => i,
                    other => {
                        *other = ParamValue::Int(*default);
                        let ParamValue::Int(i) = other else { unreachable!() };
                        i
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(&def.label);
                    ui.add(
                        egui::DragValue::new(val)
                            .speed(*step as f64 * 0.25)
                            .range(*min..=*max),
                    );
                });
            }
            ParamKind::Float {
                default,
                min,
                max,
                step,
            } => {
                let val = match params
                    .entry(def.key.clone())
                    .or_insert(ParamValue::Float(*default))
                {
                    ParamValue::Float(f) => f,
                    other => {
                        *other = ParamValue::Float(*default);
                        let ParamValue::Float(f) = other else { unreachable!() };
                        f
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(&def.label);
                    ui.add(egui::DragValue::new(val).speed(*step).range(*min..=*max));
                });
            }
            ParamKind::String { default } => {
                let val = match params
                    .entry(def.key.clone())
                    .or_insert_with(|| ParamValue::String(default.clone()))
                {
                    ParamValue::String(s) => s,
                    other => {
                        *other = ParamValue::String(default.clone());
                        let ParamValue::String(s) = other else { unreachable!() };
                        s
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(&def.label);
                    ui.text_edit_singleline(val);
                });
            }
            ParamKind::Choice {
                options,
                default_idx,
            } => {
                let default_id = options
                    .get(*default_idx)
                    .or_else(|| options.first())
                    .map(|(id, _)| id.clone())
                    .unwrap_or_default();
                let val = match params
                    .entry(def.key.clone())
                    .or_insert_with(|| ParamValue::Choice(default_id.clone()))
                {
                    ParamValue::Choice(s) => s,
                    other => {
                        *other = ParamValue::Choice(default_id);
                        let ParamValue::Choice(s) = other else { unreachable!() };
                        s
                    }
                };
                let selected_label = options
                    .iter()
                    .find(|(id, _)| id == val)
                    .map(|(_, lbl)| lbl.as_str())
                    .unwrap_or(val.as_str());
                egui::ComboBox::from_label(&def.label)
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        for (id, lbl) in options {
                            ui.selectable_value(val, id.clone(), lbl);
                        }
                    });
            }
            ParamKind::Block { default, .. } => {
                let (state, wildcard) = match params
                    .entry(def.key.clone())
                    .or_insert_with(|| ParamValue::Block {
                        state: default.clone(),
                        match_base_only: false,
                    }) {
                    ParamValue::Block {
                        state,
                        match_base_only,
                    } => (state.clone(), *match_base_only),
                    other => {
                        *other = ParamValue::Block {
                            state: default.clone(),
                            match_base_only: false,
                        };
                        (default.clone(), false)
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(&def.label);
                    let lbl = if wildcard {
                        format!("{} (Any)", state.name)
                    } else {
                        state.to_string()
                    };
                    if ui.button(lbl).clicked() {
                        clicked_block_key = Some(def.key.clone());
                    }
                });
            }
        }
    }

    clicked_block_key
}
