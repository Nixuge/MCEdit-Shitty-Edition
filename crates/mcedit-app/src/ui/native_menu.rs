use crate::config::AppConfig;
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    OpenWorld,
    OpenLevelDat,
    CreateNewWorld,
    SaveWorld,
    WorldInfo,
    CloseWorld,
    Quit,
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    DeleteBlocks,
    Deselect,
    Toggle2DView,
    ToggleFastLeaves,
    ToggleRoughGraphics,
    ToggleHiddenOres,
    ToggleBarriers,
    ToggleEntities,
    ToggleBlockEntities,
    ToggleChunkBorders,
    ToggleFloorGrid,
    LoadResourcePack,
    ResetDefaultTextures,
    OpenSettings,
    TogglePerfOverlay,
    ToggleTrace,
}

impl MenuCommand {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "file.open_world" => Some(Self::OpenWorld),
            "file.open_level_dat" => Some(Self::OpenLevelDat),
            "file.new_world" => Some(Self::CreateNewWorld),
            "file.save" => Some(Self::SaveWorld),
            "file.world_info" => Some(Self::WorldInfo),
            "file.close_world" => Some(Self::CloseWorld),
            "app.quit" => Some(Self::Quit),
            "edit.undo" => Some(Self::Undo),
            "edit.redo" => Some(Self::Redo),
            "edit.copy" => Some(Self::Copy),
            "edit.cut" => Some(Self::Cut),
            "edit.paste" => Some(Self::Paste),
            "edit.delete" => Some(Self::DeleteBlocks),
            "edit.deselect" => Some(Self::Deselect),
            "view.chunk_2d" => Some(Self::Toggle2DView),
            "view.fast_leaves" => Some(Self::ToggleFastLeaves),
            "view.rough_graphics" => Some(Self::ToggleRoughGraphics),
            "view.hidden_ores" => Some(Self::ToggleHiddenOres),
            "view.barriers" => Some(Self::ToggleBarriers),
            "view.entities" => Some(Self::ToggleEntities),
            "view.block_entities" => Some(Self::ToggleBlockEntities),
            "view.chunk_borders" => Some(Self::ToggleChunkBorders),
            "view.floor_grid" => Some(Self::ToggleFloorGrid),
            "view.load_resource_pack" => Some(Self::LoadResourcePack),
            "view.reset_textures" => Some(Self::ResetDefaultTextures),
            "view.perf_overlay" => Some(Self::TogglePerfOverlay),
            "view.trace" => Some(Self::ToggleTrace),
            "app.settings" | "file.settings" | "view.settings" => Some(Self::OpenSettings),
            _ => None,
        }
    }
}

pub struct NativeMenuBar {
    #[allow(dead_code)]
    menu: Menu,
    save_item: MenuItem,
    world_info_item: MenuItem,
    close_world_item: MenuItem,
    undo_item: MenuItem,
    redo_item: MenuItem,
    copy_item: MenuItem,
    cut_item: MenuItem,
    paste_item: MenuItem,
    delete_item: MenuItem,
    view_2d_item: CheckMenuItem,
    fast_leaves_item: CheckMenuItem,
    rough_graphics_item: CheckMenuItem,
    hidden_ores_item: CheckMenuItem,
    barriers_item: CheckMenuItem,
    entities_item: CheckMenuItem,
    block_entities_item: CheckMenuItem,
    chunk_borders_item: CheckMenuItem,
    floor_grid_item: CheckMenuItem,
    reset_textures_item: MenuItem,
    perf_overlay_item: CheckMenuItem,
    trace_item: CheckMenuItem,
}

impl NativeMenuBar {
    #[cfg(target_os = "macos")]
    pub fn init(config: &AppConfig, is_2d: bool) -> Option<Self> {
        let menu = Menu::new();

        // macOS Application Menu ("MCEdit")
        let settings_app = MenuItem::with_id(
            "app.settings",
            "Settings...",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::Comma)),
        );
        let quit_app = MenuItem::with_id(
            "app.quit",
            "Quit MCEdit",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyQ)),
        );
        let app_menu = Submenu::with_items(
            "MCEdit",
            true,
            &[
                &PredefinedMenuItem::about(None, None),
                &PredefinedMenuItem::separator(),
                &settings_app,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                &quit_app,
            ],
        )
        .ok()?;

        // File Menu
        let open_world = MenuItem::with_id(
            "file.open_world",
            "Open World...",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyO)),
        );
        let open_level_dat = MenuItem::with_id("file.open_level_dat", "Open level.dat...", true, None);
        let new_world = MenuItem::with_id(
            "file.new_world",
            "Create New World...",
            true,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyN)),
        );
        let save_item = MenuItem::with_id(
            "file.save",
            "Save World",
            false,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyS)),
        );
        let world_info_item = MenuItem::with_id(
            "file.world_info",
            "World Info...",
            false,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyI)),
        );
        let close_world_item = MenuItem::with_id(
            "file.close_world",
            "Close World",
            false,
            Some(Accelerator::new(Some(Modifiers::META), Code::KeyW)),
        );
        let file_settings = MenuItem::with_id(
            "file.settings",
            "Settings & Keybindings...",
            true,
            None,
        );
        let file_menu = Submenu::with_items(
            "File",
            true,
            &[
                &open_world,
                &open_level_dat,
                &new_world,
                &PredefinedMenuItem::separator(),
                &save_item,
                &world_info_item,
                &close_world_item,
                &PredefinedMenuItem::separator(),
                &file_settings,
            ],
        )
        .ok()?;

        // Edit Menu (accelerators handled cleanly via winit/egui when typing in inputs;
        // File & App accelerators work natively via NSMenu)
        let undo_item = MenuItem::with_id("edit.undo", "Undo", false, None);
        let redo_item = MenuItem::with_id("edit.redo", "Redo", false, None);
        let copy_item = MenuItem::with_id("edit.copy", "Copy Selection", false, None);
        let cut_item = MenuItem::with_id("edit.cut", "Cut Selection", false, None);
        let paste_item = MenuItem::with_id("edit.paste", "Paste Clipboard", false, None);
        let delete_item = MenuItem::with_id("edit.delete", "Delete Selected Blocks", false, None);
        let deselect_item = MenuItem::with_id("edit.deselect", "Deselect", true, None);
        let edit_menu = Submenu::with_items(
            "Edit",
            true,
            &[
                &undo_item,
                &redo_item,
                &PredefinedMenuItem::separator(),
                &cut_item,
                &copy_item,
                &paste_item,
                &delete_item,
                &deselect_item,
            ],
        )
        .ok()?;

        // View Menu
        let view_2d_item =
            CheckMenuItem::with_id("view.chunk_2d", "2D Chunk View (Tab)", true, is_2d, None);
        let fast_leaves_item = CheckMenuItem::with_id(
            "view.fast_leaves",
            "Fast Leaves",
            true,
            config.fast_leaves,
            None,
        );
        let rough_graphics_item = CheckMenuItem::with_id(
            "view.rough_graphics",
            "Rough Graphics",
            true,
            config.rough_graphics,
            None,
        );
        let hidden_ores_item = CheckMenuItem::with_id(
            "view.hidden_ores",
            "Show Hidden Ores",
            true,
            config.show_hidden_ores,
            None,
        );
        let barriers_item = CheckMenuItem::with_id(
            "view.barriers",
            "Show Barrier Blocks",
            true,
            config.show_barriers,
            None,
        );
        let entities_item = CheckMenuItem::with_id(
            "view.entities",
            "Show Entities",
            true,
            config.show_entities,
            None,
        );
        let block_entities_item = CheckMenuItem::with_id(
            "view.block_entities",
            "Show Block Entities",
            true,
            config.show_block_entities,
            None,
        );
        let chunk_borders_item = CheckMenuItem::with_id(
            "view.chunk_borders",
            "Show Chunk Borders",
            true,
            config.show_chunk_borders,
            None,
        );
        let floor_grid_item = CheckMenuItem::with_id(
            "view.floor_grid",
            "Show World Chunk Grid",
            true,
            config.show_floor_grid,
            None,
        );
        let perf_overlay_item = CheckMenuItem::with_id(
            "view.perf_overlay",
            "Performance Overlay (F3)",
            true,
            false,
            None,
        );
        let trace_item = CheckMenuItem::with_id(
            "view.trace",
            "Record Trace (F4)",
            true,
            false,
            None,
        );
        let load_pack = MenuItem::with_id("view.load_resource_pack", "Load Resource Pack...", true, None);
        let reset_textures_item = MenuItem::with_id(
            "view.reset_textures",
            "Reset to Default Textures",
            config.resource_pack_path.is_some(),
            None,
        );
        let view_menu = Submenu::with_items(
            "View",
            true,
            &[
                &view_2d_item,
                &PredefinedMenuItem::separator(),
                &fast_leaves_item,
                &rough_graphics_item,
                &hidden_ores_item,
                &barriers_item,
                &PredefinedMenuItem::separator(),
                &entities_item,
                &block_entities_item,
                &chunk_borders_item,
                &floor_grid_item,
                &PredefinedMenuItem::separator(),
                &perf_overlay_item,
                &trace_item,
                &PredefinedMenuItem::separator(),
                &load_pack,
                &reset_textures_item,
            ],
        )
        .ok()?;

        // Standard macOS Window Menu
        let window_menu = Submenu::with_items(
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::maximize(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::fullscreen(None),
            ],
        )
        .ok()?;

        menu.append_items(&[&app_menu, &file_menu, &edit_menu, &view_menu, &window_menu])
            .ok()?;
        menu.init_for_nsapp();

        Some(Self {
            menu,
            save_item,
            world_info_item,
            close_world_item,
            undo_item,
            redo_item,
            copy_item,
            cut_item,
            paste_item,
            delete_item,
            view_2d_item,
            fast_leaves_item,
            rough_graphics_item,
            hidden_ores_item,
            barriers_item,
            entities_item,
            block_entities_item,
            chunk_borders_item,
            floor_grid_item,
            reset_textures_item,
            perf_overlay_item,
            trace_item,
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn init(_config: &AppConfig, _is_2d: bool) -> Option<Self> {
        None
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sync_state(
        &self,
        config: &AppConfig,
        is_2d: bool,
        has_world: bool,
        busy: bool,
        can_undo: bool,
        can_redo: bool,
        has_selection: bool,
        has_clipboard: bool,
        show_perf: bool,
        tracing: bool,
    ) {
        self.save_item.set_enabled(has_world && !busy);
        self.world_info_item.set_enabled(has_world);
        self.close_world_item.set_enabled(has_world);
        self.undo_item.set_enabled(can_undo);
        self.redo_item.set_enabled(can_redo);
        self.copy_item.set_enabled(has_selection);
        self.cut_item.set_enabled(has_selection);
        self.paste_item.set_enabled(has_clipboard);
        self.delete_item.set_enabled(has_selection);

        self.perf_overlay_item.set_checked(show_perf);
        self.trace_item.set_checked(tracing);
        self.view_2d_item.set_checked(is_2d);
        self.fast_leaves_item.set_checked(config.fast_leaves);
        self.rough_graphics_item.set_checked(config.rough_graphics);
        self.hidden_ores_item.set_checked(config.show_hidden_ores);
        self.barriers_item.set_checked(config.show_barriers);
        self.entities_item.set_checked(config.show_entities);
        self.block_entities_item.set_checked(config.show_block_entities);
        self.chunk_borders_item.set_checked(config.show_chunk_borders);
        self.floor_grid_item.set_checked(config.show_floor_grid);
        self.reset_textures_item
            .set_enabled(config.resource_pack_path.is_some());
    }
}
