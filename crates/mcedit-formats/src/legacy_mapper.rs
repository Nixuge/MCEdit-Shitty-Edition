use ahash::AHashMap;
use mcedit_core::{BlockMeta, BlockRegistry, BlockState, BlockStateId};
use std::collections::BTreeMap;

/// Fast bidirectional mapper between pre-1.13 `(id, data)` pairs and canonical `BlockStateId`s.
#[derive(Debug, Clone)]
pub struct LegacyIdMapper {
    /// 4096 * 16 lookup table (`None` if not pre-registered)
    to_canon: Vec<Option<BlockStateId>>,
    to_leg: AHashMap<BlockStateId, (u16, u8)>,
}

impl LegacyIdMapper {
    pub fn from_registry(reg: &BlockRegistry) -> Self {
        let mut to_canon = vec![None; 4096 * 16];
        let mut to_leg = AHashMap::new();

        for id in reg.all_ids() {
            let meta = reg.get_meta(id);
            if let Some((bid, data)) = meta.legacy_id
                && (bid as usize) < 4096
                && data < 16
            {
                let idx = (bid as usize) * 16 + (data as usize);
                to_canon[idx].get_or_insert(id);
                to_leg.insert(id, (bid, data));
            }
        }

        Self { to_canon, to_leg }
    }

    #[inline]
    pub fn to_canonical_readonly(&self, id: u16, data: u8) -> BlockStateId {
        let d = data & 0x0F;
        let idx = ((id as usize) & 0x0FFF) * 16 + (d as usize);
        if let Some(sid) = self.to_canon[idx] {
            return sid;
        }
        let base_idx = ((id as usize) & 0x0FFF) * 16;
        self.to_canon[base_idx].unwrap_or(BlockStateId::AIR)
    }

    pub fn to_canonical(&mut self, id: u16, data: u8, reg: &mut BlockRegistry) -> BlockStateId {
        let d = data & 0x0F;
        let idx = ((id as usize) & 0x0FFF) * 16 + (d as usize);
        if let Some(sid) = self.to_canon[idx] {
            return sid;
        }
        // Check if base data=0 exists for this ID; if so create state preserving base block name + data
        let base_idx = ((id as usize) & 0x0FFF) * 16;
        let state = if let Some(base_sid) = self.to_canon[base_idx] {
            let base_name = reg.get_state(base_sid).name.clone();
            let mut props = BTreeMap::new();
            props.insert("data".to_string(), d.to_string());
            BlockState {
                name: base_name,
                properties: props,
            }
        } else {
            let mut props = BTreeMap::new();
            props.insert("data".to_string(), d.to_string());
            BlockState {
                name: format!("mcedit:legacy_{id}"),
                properties: props,
            }
        };

        let meta = if let Some(base_sid) = self.to_canon[base_idx] {
            let mut m = reg.get_meta(base_sid).clone();
            m.legacy_id = Some((id, d));
            m
        } else {
            BlockMeta {
                display_name: format!("Legacy {id}:{d}"),
                legacy_id: Some((id, d)),
                search_text: format!("legacy {id}:{d} {state}"),
                ..Default::default()
            }
        };

        let sid = reg.intern_with_meta(state, meta);
        self.to_canon[idx] = Some(sid);
        self.to_leg.insert(sid, (id, d));
        sid
    }

    pub fn to_legacy(&self, sid: BlockStateId, reg: &BlockRegistry) -> (u16, u8) {
        if let Some(&pair) = self.to_leg.get(&sid) {
            return pair;
        }
        let meta = reg.get_meta(sid);
        if let Some(pair) = meta.legacy_id {
            return pair;
        }
        let state = reg.get_state(sid);
        if let Some(rest) = state.name.strip_prefix("mcedit:legacy_")
            && let Ok(bid) = rest.parse::<u16>()
        {
            let d = state
                .properties
                .get("data")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(0)
                & 0x0F;
            return (bid, d);
        }
        // Fallback: look up any state with same base name
        if let Some(base_sid) = reg.find_by_name(&state.name)
            && let Some(&pair) = self.to_leg.get(&base_sid)
        {
            let d = state
                .properties
                .get("data")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(pair.1)
                & 0x0F;
            return (pair.0, d);
        }
        (1, 0) // Stone fallback if completely unknown post-flattening block
    }
}
