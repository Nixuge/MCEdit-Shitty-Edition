use mcedit_core::BlockState;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum ParamKind {
    Bool {
        default: bool,
    },
    Int {
        default: i64,
        min: i64,
        max: i64,
        step: i64,
    },
    Float {
        default: f64,
        min: f64,
        max: f64,
        step: f64,
    },
    String {
        default: String,
    },
    Choice {
        /// `(stable_id, display_label)`
        options: Vec<(String, String)>,
        default_idx: usize,
    },
    Block {
        default: BlockState,
        allow_wildcard: bool,
    },
    Label(String),
}

#[derive(Debug, Clone)]
pub struct ParamDef {
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
}

impl ParamDef {
    pub fn default_value(&self) -> Option<ParamValue> {
        match &self.kind {
            ParamKind::Bool { default } => Some(ParamValue::Bool(*default)),
            ParamKind::Int { default, .. } => Some(ParamValue::Int(*default)),
            ParamKind::Float { default, .. } => Some(ParamValue::Float(*default)),
            ParamKind::String { default } => Some(ParamValue::String(default.clone())),
            ParamKind::Choice {
                options,
                default_idx,
            } => options
                .get(*default_idx)
                .or_else(|| options.first())
                .map(|(id, _)| ParamValue::Choice(id.clone())),
            ParamKind::Block { default, .. } => Some(ParamValue::Block {
                state: default.clone(),
                match_base_only: false,
            }),
            ParamKind::Label(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Choice(String),
    Block {
        state: BlockState,
        match_base_only: bool,
    },
}

pub type ParamMap = BTreeMap<String, ParamValue>;

pub fn default_param_map(defs: &[ParamDef]) -> ParamMap {
    let mut map = ParamMap::new();
    for d in defs {
        if let Some(v) = d.default_value() {
            map.insert(d.key.clone(), v);
        }
    }
    map
}

pub fn get_bool(params: &ParamMap, key: &str, fallback: bool) -> bool {
    match params.get(key) {
        Some(ParamValue::Bool(v)) => *v,
        _ => fallback,
    }
}

pub fn get_int(params: &ParamMap, key: &str, fallback: i64) -> i64 {
    match params.get(key) {
        Some(ParamValue::Int(v)) => *v,
        _ => fallback,
    }
}

pub fn get_float(params: &ParamMap, key: &str, fallback: f64) -> f64 {
    match params.get(key) {
        Some(ParamValue::Float(v)) => *v,
        _ => fallback,
    }
}

pub fn get_string<'a>(params: &'a ParamMap, key: &str, fallback: &'a str) -> &'a str {
    match params.get(key) {
        Some(ParamValue::String(v)) | Some(ParamValue::Choice(v)) => v.as_str(),
        _ => fallback,
    }
}

pub fn get_block<'a>(params: &'a ParamMap, key: &str) -> Option<(&'a BlockState, bool)> {
    match params.get(key) {
        Some(ParamValue::Block {
            state,
            match_base_only,
        }) => Some((state, *match_base_only)),
        _ => None,
    }
}
