use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum KeyAction {
    MoveForward,
    MoveBackward,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    Sprint,
    ToggleViewMode,
    Undo,
    Redo,
    Save,
    Copy,
    Cut,
    Paste,
    Delete,
    Deselect,
    RotateCW,
    Roll,
    FlipX,
    MirrorZ,
    ToggleBarriers,
}

impl KeyAction {
    pub fn label(self) -> &'static str {
        match self {
            KeyAction::MoveForward => "Move Forward",
            KeyAction::MoveBackward => "Move Backward",
            KeyAction::MoveLeft => "Strafe Left",
            KeyAction::MoveRight => "Strafe Right",
            KeyAction::MoveUp => "Fly Up",
            KeyAction::MoveDown => "Fly Down",
            KeyAction::Sprint => "Sprint",
            KeyAction::ToggleViewMode => "Toggle 3D / 2D Chunk View",
            KeyAction::Undo => "Undo",
            KeyAction::Redo => "Redo",
            KeyAction::Save => "Save World",
            KeyAction::Copy => "Copy Selection",
            KeyAction::Cut => "Cut Selection",
            KeyAction::Paste => "Paste Clipboard",
            KeyAction::Delete => "Delete Selected Blocks",
            KeyAction::Deselect => "Deselect",
            KeyAction::RotateCW => "Rotate 90°",
            KeyAction::Roll => "Roll 90°",
            KeyAction::FlipX => "Flip Vertical / X",
            KeyAction::MirrorZ => "Mirror Z",
            KeyAction::ToggleBarriers => "Show / Hide Barrier Blocks",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PanelBackgroundStyle {
    #[default]
    BlurredGlass,
    Transparent,
    Solid,
}

impl PanelBackgroundStyle {
    pub const ALL: [Self; 3] = [Self::BlurredGlass, Self::Transparent, Self::Solid];

    pub fn label(self) -> &'static str {
        match self {
            Self::BlurredGlass => "Blurred Glass",
            Self::Transparent => "Transparent",
            Self::Solid => "Solid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InvalidWorldBehavior {
    #[default]
    ShowToast,
    AskOpenInBrowser,
    OpenInBrowser,
}

impl InvalidWorldBehavior {
    pub const ALL: [Self; 3] = [
        Self::ShowToast,
        Self::AskOpenInBrowser,
        Self::OpenInBrowser,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ShowToast => "Show Error Toast",
            Self::AskOpenInBrowser => "Ask to Open in Browse Worlds",
            Self::OpenInBrowser => "Open in Browse Worlds",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FileMenuEscapeBehavior {
    #[default]
    HideMenu,
    CloseWorld,
}

impl FileMenuEscapeBehavior {
    pub const ALL: [Self; 2] = [Self::HideMenu, Self::CloseWorld];

    pub fn label(self) -> &'static str {
        match self {
            Self::HideMenu => "Hide File Menu",
            Self::CloseWorld => "Close Current World",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub view_distance: i32,
    pub detail_distance: i32,
    pub camera_speed: f32,
    pub fov_deg: f32,
    pub target_fps: u32,
    pub vram_limit_mb: usize,
    pub undo_limit: usize,
    pub max_copies: usize,
    pub fast_leaves: bool,
    pub rough_graphics: bool,
    pub show_hidden_ores: bool,
    #[serde(default = "default_show_barriers")]
    pub show_barriers: bool,
    pub show_entities: bool,
    pub show_block_entities: bool,
    pub show_chunk_borders: bool,
    pub show_terrain_populated: bool,
    pub show_ceiling: bool,
    pub show_floor_grid: bool,
    pub left_panel_style: PanelBackgroundStyle,
    pub invalid_world_behavior: InvalidWorldBehavior,
    pub file_menu_escape_behavior: FileMenuEscapeBehavior,
    pub vsync: bool,
    pub mouse_sensitivity: f32,
    pub fly_mode: bool,
    pub autobrake: bool,
    pub resource_pack_path: Option<PathBuf>,
    pub recent_worlds: Vec<PathBuf>,
    pub last_browser_dir: Option<PathBuf>,
    pub keybinds: BTreeMap<KeyAction, String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut keybinds = BTreeMap::new();
        keybinds.insert(KeyAction::MoveForward, "W".into());
        keybinds.insert(KeyAction::MoveBackward, "S".into());
        keybinds.insert(KeyAction::MoveLeft, "A".into());
        keybinds.insert(KeyAction::MoveRight, "D".into());
        keybinds.insert(KeyAction::MoveUp, "Space".into());
        keybinds.insert(KeyAction::MoveDown, "LShift".into());
        keybinds.insert(KeyAction::Sprint, "LControl".into());
        keybinds.insert(KeyAction::ToggleViewMode, "Tab".into());
        keybinds.insert(KeyAction::Undo, "Ctrl+Z".into());
        keybinds.insert(KeyAction::Redo, "Ctrl+Y".into());
        keybinds.insert(KeyAction::Save, "Ctrl+S".into());
        keybinds.insert(KeyAction::Copy, "Ctrl+C".into());
        keybinds.insert(KeyAction::Cut, "Ctrl+X".into());
        keybinds.insert(KeyAction::Paste, "Ctrl+V".into());
        keybinds.insert(KeyAction::Delete, "Delete".into());
        keybinds.insert(KeyAction::Deselect, "Ctrl+D".into());
        keybinds.insert(KeyAction::RotateCW, "E".into());
        keybinds.insert(KeyAction::Roll, "R".into());
        keybinds.insert(KeyAction::FlipX, "F".into());
        keybinds.insert(KeyAction::MirrorZ, "G".into());
        keybinds.insert(KeyAction::ToggleBarriers, "B".into());

        Self {
            view_distance: 64,
            detail_distance: 12,
            camera_speed: 35.0,
            fov_deg: 70.0,
            target_fps: 60,
            vram_limit_mb: 4096,
            undo_limit: 25,
            max_copies: 32,
            fast_leaves: false,
            rough_graphics: false,
            show_hidden_ores: false,
            show_barriers: true,
            show_entities: true,
            show_block_entities: true,
            show_chunk_borders: false,
            show_terrain_populated: false,
            show_ceiling: false,
            show_floor_grid: true,
            left_panel_style: PanelBackgroundStyle::BlurredGlass,
            invalid_world_behavior: InvalidWorldBehavior::AskOpenInBrowser,
            file_menu_escape_behavior: FileMenuEscapeBehavior::HideMenu,
            vsync: true,
            mouse_sensitivity: 0.18,
            fly_mode: false,
            autobrake: true,
            resource_pack_path: None,
            recent_worlds: Vec::new(),
            last_browser_dir: None,
            keybinds,
        }
    }
}

impl AppConfig {
    pub fn config_path() -> Option<PathBuf> {
        ProjectDirs::from("net", "mcedit-unified", "mcedit-rs")
            .map(|p| p.config_dir().join("config.toml"))
    }

    pub fn traces_dir() -> PathBuf {
        ProjectDirs::from("net", "mcedit-unified", "mcedit-rs")
            .map(|p| p.config_dir().join("traces"))
            .unwrap_or_else(|| std::env::temp_dir().join("mcedit-traces"))
    }

    pub fn load() -> Self {
        let defaults = Self::default();
        let Some(path) = Self::config_path() else {
            return defaults;
        };
        if let Ok(text) = fs::read_to_string(&path)
            && let Ok(mut cfg) = toml::from_str::<Self>(&text)
        {
            for (action, bind) in defaults.keybinds {
                cfg.keybinds.entry(action).or_insert(bind);
            }
            return cfg;
        }
        defaults
    }

    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = fs::write(path, text);
        }
    }

    pub fn push_recent_world(&mut self, path: PathBuf) {
        self.recent_worlds.retain(|p| p != &path);
        self.recent_worlds.insert(0, path);
        self.recent_worlds.truncate(8);
        self.save();
    }

    /// Returns the default Minecraft `saves` directory if it exists on this OS.
    pub fn minecraft_saves_dir() -> Option<PathBuf> {
        let base = directories::BaseDirs::new()?;
        #[cfg(target_os = "macos")]
        let p = base.data_dir().join("minecraft").join("saves");
        #[cfg(target_os = "windows")]
        let p = base.data_dir().join(".minecraft").join("saves");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let p = base.home_dir().join(".minecraft").join("saves");
        p.is_dir().then_some(p)
    }

    /// Resolves the initial folder for the main-menu browser:
    /// uses `last_browser_dir` if it still exists, otherwise Minecraft `saves`, home, or cwd.
    pub fn resolve_browser_dir(&self) -> PathBuf {
        if let Some(p) = &self.last_browser_dir
            && p.is_dir()
        {
            return p.clone();
        }
        if let Some(saves) = Self::minecraft_saves_dir() {
            return saves;
        }
        if let Some(base) = directories::BaseDirs::new()
            && base.home_dir().is_dir()
        {
            return base.home_dir().to_path_buf();
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    }
}

fn default_show_barriers() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_dir_persists_when_valid_and_falls_back_when_deleted() {
        let valid_dir = std::env::temp_dir().join(format!("mcedit_browser_test_{}", std::process::id()));
        std::fs::create_dir_all(&valid_dir).unwrap();

        let cfg = AppConfig {
            last_browser_dir: Some(valid_dir.clone()),
            ..Default::default()
        };
        assert_eq!(cfg.resolve_browser_dir(), valid_dir);

        // Remove the folder and verify it falls back cleanly to an existing directory
        std::fs::remove_dir_all(&valid_dir).unwrap();
        let fallback = cfg.resolve_browser_dir();
        assert_ne!(fallback, valid_dir);
        assert!(fallback.is_dir());
    }
}
