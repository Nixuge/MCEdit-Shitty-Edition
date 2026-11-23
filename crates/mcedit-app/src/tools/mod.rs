pub mod brush;
pub mod chunk;
pub mod clone;
pub mod fill;
pub mod filter;
pub mod nbt_explorer;
pub mod player;
pub mod select;
pub mod teleport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolId {
    Select,
    Brush,
    Clone,
    Fill,
    Filter,
    Import,
    Player,
    Spawn,
    Chunk,
    NbtExplorer,
    Teleport,
}

impl ToolId {
    pub const ALL: [ToolId; 11] = [
        ToolId::Select,
        ToolId::Brush,
        ToolId::Clone,
        ToolId::Fill,
        ToolId::Filter,
        ToolId::Import,
        ToolId::Player,
        ToolId::Spawn,
        ToolId::Chunk,
        ToolId::NbtExplorer,
        ToolId::Teleport,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ToolId::Select => "Select",
            ToolId::Brush => "Brush",
            ToolId::Clone => "Clone",
            ToolId::Fill => "Fill",
            ToolId::Filter => "Filter",
            ToolId::Import => "Import",
            ToolId::Player => "Player",
            ToolId::Spawn => "Spawn",
            ToolId::Chunk => "Chunk",
            ToolId::NbtExplorer => "NBT",
            ToolId::Teleport => "Teleport",
        }
    }

    pub fn hotkey_label(self) -> &'static str {
        match self {
            ToolId::Select => "1",
            ToolId::Brush => "2",
            ToolId::Clone => "3",
            ToolId::Fill => "4",
            ToolId::Filter => "5",
            ToolId::Import => "6",
            ToolId::Player => "7",
            ToolId::Spawn => "8",
            ToolId::Chunk => "9",
            ToolId::NbtExplorer => "0",
            ToolId::Teleport => "T",
        }
    }
}
