use crate::anvil::post113::AnvilPost113Format;
use crate::anvil::pre113::AnvilPre113Format;
use crate::schematic::classic::McSchematicFormat;
use mcedit_core::{
    BlockRegistry, FormatError, FormatResult, SchematicFormat, SchematicVolume, WorldFormat,
};
use std::path::Path;

pub struct FormatRegistry;

impl FormatRegistry {
    pub fn open_world(
        path: impl AsRef<Path>,
        registry: &BlockRegistry,
    ) -> FormatResult<Box<dyn WorldFormat>> {
        let p = path.as_ref();
        match AnvilPre113Format::open(p, registry) {
            Ok(fmt) => Ok(Box::new(fmt)),
            Err(FormatError::UnsupportedVersion(_)) => {
                let fmt = AnvilPost113Format::open(p, registry)?;
                Ok(Box::new(fmt))
            }
            Err(e) => Err(e),
        }
    }

    pub fn open_schematic(
        path: impl AsRef<Path>,
        registry: &mut BlockRegistry,
    ) -> FormatResult<SchematicVolume> {
        let p = path.as_ref();
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "schematic" || ext.is_empty() {
            return McSchematicFormat.load(p, registry);
        }
        Err(FormatError::UnsupportedFormat(p.display().to_string()))
    }

    pub fn save_schematic(
        path: impl AsRef<Path>,
        volume: &SchematicVolume,
        registry: &BlockRegistry,
    ) -> FormatResult<()> {
        McSchematicFormat.save(path.as_ref(), volume, registry)
    }
}
