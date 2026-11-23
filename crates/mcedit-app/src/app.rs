use crate::config::{AppConfig, KeyAction, PanelBackgroundStyle};
use crate::editor::EditorState;
use crate::tools::{
    ToolId, brush, chunk, clone, fill, filter, nbt_explorer, player, select, teleport,
};
use crate::ui::create_world::CreateWorldDraft;
use crate::ui::native_menu::{MenuCommand, NativeMenuBar};
use crate::ui::world_info::WorldInfoDraft;
use crate::ui::{
    block_picker, clipboard_panel, create_world, settings_dialog, status_bar, toolbar, top_bar,
    welcome, world_info,
};
use glam::{IVec3, Vec2};
use mcedit_core::TransformOp;
use mcedit_render::{
    CameraInputs, OverlayMeshBuilder, TextureAtlas, ViewMode, WorldRenderer,
    raycast_world_with_lod,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
#[cfg(target_os = "linux")]
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum UserEvent {
    Menu(muda::MenuEvent),
    WaylandHover(bool),
    WaylandDrop {
        path: Option<PathBuf>,
        offer_ptr: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MenuSyncState {
    is_2d: bool,
    has_world: bool,
    busy: bool,
    can_undo: bool,
    can_redo: bool,
    has_selection: bool,
    has_clipboard: bool,
    fast_leaves: bool,
    rough_graphics: bool,
    show_hidden_ores: bool,
    show_barriers: bool,
    show_entities: bool,
    show_block_entities: bool,
    show_chunk_borders: bool,
    show_floor_grid: bool,
    has_resource_pack: bool,
    show_perf: bool,
    tracing: bool,
}

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    surface_supports_copy_src: bool,
    renderer: WorldRenderer,
    egui_renderer: egui_wgpu::Renderer,
    egui_state: egui_winit::State,
    atlas_egui_tex: egui::TextureHandle,
}

pub struct McEditApp {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    native_menu: Option<NativeMenuBar>,
    last_menu_sync: Option<MenuSyncState>,
    editor: EditorState,
    overlays: OverlayMeshBuilder,
    held_keys: HashSet<KeyCode>,
    modifiers: ModifiersState,
    right_mouse_down: bool,
    left_mouse_down: bool,
    cursor_pos: Vec2,
    last_frame: Instant,
    fps_ema: f32,
    frame_count: u64,
    applied_vsync: bool,
    initial_world: Option<PathBuf>,
    hovered_drop_file: Option<PathBuf>,
    wayland_hovering: bool,
    #[cfg(target_os = "linux")]
    event_proxy: EventLoopProxy<UserEvent>,
    #[cfg(target_os = "linux")]
    wayland_dnd: Option<crate::wayland_dnd::WaylandDnd>,
}

impl McEditApp {
    pub fn run(initial_world: Option<PathBuf>) -> Result<(), String> {
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .map_err(|e| e.to_string())?;
        event_loop.set_control_flow(ControlFlow::Poll);

        let proxy = event_loop.create_proxy();
        let menu_proxy = proxy.clone();
        muda::MenuEvent::set_event_handler(Some(move |event| {
            let _ = menu_proxy.send_event(UserEvent::Menu(event));
        }));

        let config = AppConfig::load();
        let vsync = config.vsync;
        let trace_env = std::env::var_os("MCEDIT_TRACE").map(PathBuf::from);
        if trace_env.is_some() {
            mcedit_core::profiler::trace_start();
        }
        mcedit_core::profiler::milestones_reset();
        let mut app = Self {
            window: None,
            gpu: None,
            native_menu: None,
            last_menu_sync: None,
            editor: EditorState::new(config),
            overlays: OverlayMeshBuilder::default(),
            held_keys: HashSet::new(),
            modifiers: ModifiersState::empty(),
            right_mouse_down: false,
            left_mouse_down: false,
            cursor_pos: Vec2::ZERO,
            last_frame: Instant::now(),
            fps_ema: 60.0,
            frame_count: 0,
            applied_vsync: vsync,
            initial_world,
            hovered_drop_file: None,
            wayland_hovering: false,
            #[cfg(target_os = "linux")]
            event_proxy: proxy,
            #[cfg(target_os = "linux")]
            wayland_dnd: None,
        };
        let res = event_loop.run_app(&mut app).map_err(|e| e.to_string());
        if let Some(path) = trace_env {
            match mcedit_core::profiler::trace_stop_and_write(&path) {
                Ok(n) => eprintln!("Wrote trace ({n} events) to {}", path.display()),
                Err(e) => eprintln!("Failed to write trace: {e}"),
            }
        }
        res
    }

    fn init_gpu(&mut self, window: Arc<Window>) {
        let size = window.inner_size();
        let w = size.width.max(1);
        let h = size.height.max(1);

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(window.clone()),
        ));
        let surface = instance
            .create_surface(window.clone())
            .expect("create wgpu surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("find compatible GPU adapter");

        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .expect("request wgpu device");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let alpha_mode = caps
            .alpha_modes
            .iter()
            .copied()
            .find(|&m| m == wgpu::CompositeAlphaMode::Opaque)
            .unwrap_or(caps.alpha_modes[0]);

        let surface_supports_copy_src = caps.usages.contains(wgpu::TextureUsages::COPY_SRC);
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if surface_supports_copy_src {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }

        let surface_config = wgpu::SurfaceConfiguration {
            usage,
            format: surface_format,
            width: w,
            height: h,
            present_mode: if self.editor.config.vsync {
                wgpu::PresentMode::AutoVsync
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &surface_config);
        tracing::info!(
            "Initialized wgpu surface {}x{} format={:?} alpha={:?} scale={}",
            w,
            h,
            surface_format,
            alpha_mode,
            window.scale_factor()
        );

        let atlas = match &self.editor.config.resource_pack_path {
            Some(p) => TextureAtlas::from_resource_pack(p),
            None => TextureAtlas::default_atlas(),
        };
        self.editor.update_lod_colors_from_atlas(&atlas);
        let mut renderer = WorldRenderer::new(
            &device,
            &queue,
            surface_format,
            w,
            h,
            &atlas,
            self.editor.config.vram_limit_mb,
        );

        let egui_ctx = egui::Context::default();
        let atlas_img = egui::ColorImage::from_rgba_unmultiplied(
            [atlas.rgba.width() as usize, atlas.rgba.height() as usize],
            atlas.rgba.as_raw(),
        );
        let atlas_egui_tex =
            egui_ctx.load_texture("terrain_atlas", atlas_img, egui::TextureOptions::NEAREST);

        let egui_state = egui_winit::State::new(
            egui_ctx,
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            surface_format,
            egui_wgpu::RendererOptions::default(),
        );

        if let Some(world_path) = self.initial_world.take() {
            self.editor.open_world(&world_path, &mut renderer);
        }

        window.request_redraw();
        self.window = Some(window);
        self.gpu = Some(GpuState {
            surface,
            device,
            queue,
            surface_config,
            surface_supports_copy_src,
            renderer,
            egui_renderer,
            egui_state,
            atlas_egui_tex,
        });
    }

    fn sync_native_menu(&mut self) {
        let Some(menu) = &self.native_menu else {
            return;
        };
        let state = MenuSyncState {
            is_2d: self.editor.camera.mode == ViewMode::Chunk2D,
            has_world: self.editor.world.is_some(),
            busy: self.editor.jobs.is_busy(),
            can_undo: self
                .editor
                .world
                .as_ref()
                .is_some_and(|w| w.undo_stack.can_undo()),
            can_redo: self
                .editor
                .world
                .as_ref()
                .is_some_and(|w| w.undo_stack.can_redo()),
            has_selection: self.editor.selection.box_.is_some(),
            has_clipboard: !self.editor.copy_stack.is_empty(),
            fast_leaves: self.editor.config.fast_leaves,
            rough_graphics: self.editor.config.rough_graphics,
            show_hidden_ores: self.editor.config.show_hidden_ores,
            show_barriers: self.editor.config.show_barriers,
            show_entities: self.editor.config.show_entities,
            show_block_entities: self.editor.config.show_block_entities,
            show_chunk_borders: self.editor.config.show_chunk_borders,
            show_floor_grid: self.editor.config.show_floor_grid,
            has_resource_pack: self.editor.config.resource_pack_path.is_some(),
            show_perf: self.editor.show_profiler_overlay,
            tracing: mcedit_core::profiler::trace_enabled(),
        };
        if self.last_menu_sync == Some(state) {
            return;
        }
        self.last_menu_sync = Some(state);
        menu.sync_state(
            &self.editor.config,
            state.is_2d,
            state.has_world,
            state.busy,
            state.can_undo,
            state.can_redo,
            state.has_selection,
            state.has_clipboard,
            state.show_perf,
            state.tracing,
        );
    }

    fn handle_menu_command(&mut self, cmd: MenuCommand, event_loop: &ActiveEventLoop) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        match cmd {
            MenuCommand::OpenWorld => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.editor.open_world(&dir, &mut gpu.renderer);
                }
            }
            MenuCommand::OpenLevelDat => {
                if let Some(file) = rfd::FileDialog::new()
                    .add_filter("level.dat", &["dat"])
                    .pick_file()
                {
                    self.editor.open_world(&file, &mut gpu.renderer);
                }
            }
            MenuCommand::CreateNewWorld => {
                self.editor.create_world = Some(CreateWorldDraft::default());
            }
            MenuCommand::SaveWorld => {
                self.editor.save_world_async();
            }
            MenuCommand::WorldInfo => {
                if let Some(w) = &self.editor.world {
                    self.editor.world_info = Some(WorldInfoDraft::from_world(w));
                }
            }
            MenuCommand::CloseWorld => {
                self.editor.close_world(&mut gpu.renderer);
            }
            MenuCommand::Quit => {
                if self
                    .editor
                    .world
                    .as_ref()
                    .is_some_and(|w| w.has_unsaved_changes())
                {
                    self.editor.confirm_quit_open = true;
                } else {
                    event_loop.exit();
                }
            }
            MenuCommand::Undo => self.editor.undo(&mut gpu.renderer),
            MenuCommand::Redo => self.editor.redo(&mut gpu.renderer),
            MenuCommand::Copy => self.editor.copy_selection(),
            MenuCommand::Cut => self.editor.cut_selection(&mut gpu.renderer),
            MenuCommand::Paste => self.editor.paste_from_clipboard(),
            MenuCommand::DeleteBlocks => self.editor.delete_selected_blocks(&mut gpu.renderer),
            MenuCommand::Deselect => {
                self.editor.selection.clear();
            }
            MenuCommand::Toggle2DView => {
                self.editor.camera.mode = match self.editor.camera.mode {
                    ViewMode::Perspective3D => ViewMode::Chunk2D,
                    ViewMode::Chunk2D => ViewMode::Perspective3D,
                };
            }
            MenuCommand::ToggleFastLeaves => {
                self.editor.config.fast_leaves = !self.editor.config.fast_leaves;
                self.editor.config.save();
                self.editor.invalidate_all_meshes(&mut gpu.renderer);
            }
            MenuCommand::ToggleRoughGraphics => {
                self.editor.config.rough_graphics = !self.editor.config.rough_graphics;
                self.editor.config.save();
                self.editor.invalidate_all_meshes(&mut gpu.renderer);
            }
            MenuCommand::ToggleHiddenOres => {
                self.editor.config.show_hidden_ores = !self.editor.config.show_hidden_ores;
                self.editor.config.save();
                self.editor.invalidate_all_meshes(&mut gpu.renderer);
            }
            MenuCommand::ToggleBarriers => {
                self.editor.config.show_barriers = !self.editor.config.show_barriers;
                mcedit_core::set_hide_barriers(!self.editor.config.show_barriers);
                self.editor.config.save();
                self.editor.invalidate_all_meshes(&mut gpu.renderer);
                let state = if self.editor.config.show_barriers {
                    "Shown"
                } else {
                    "Hidden"
                };
                self.editor.set_toast(format!("Barrier blocks: {state}"));
            }
            MenuCommand::ToggleEntities => {
                self.editor.config.show_entities = !self.editor.config.show_entities;
                self.editor.config.save();
            }
            MenuCommand::ToggleBlockEntities => {
                self.editor.config.show_block_entities = !self.editor.config.show_block_entities;
                self.editor.config.save();
            }
            MenuCommand::ToggleChunkBorders => {
                self.editor.config.show_chunk_borders = !self.editor.config.show_chunk_borders;
                self.editor.config.save();
            }
            MenuCommand::ToggleFloorGrid => {
                self.editor.config.show_floor_grid = !self.editor.config.show_floor_grid;
                self.editor.chunk_grid_dirty = true;
                self.editor.config.save();
            }
            MenuCommand::LoadResourcePack => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Resource Pack", &["zip"])
                    .pick_file()
                {
                    self.editor.config.resource_pack_path = Some(path.clone());
                    self.editor.config.save();
                    self.editor.pending_atlas_reload = Some(Some(path));
                }
            }
            MenuCommand::ResetDefaultTextures => {
                self.editor.config.resource_pack_path = None;
                self.editor.config.save();
                self.editor.pending_atlas_reload = Some(None);
            }
            MenuCommand::OpenSettings => {
                self.editor.settings_open = true;
            }
            MenuCommand::TogglePerfOverlay => {
                self.editor.show_profiler_overlay = !self.editor.show_profiler_overlay;
            }
            MenuCommand::ToggleTrace => self.editor.toggle_trace(),
        }
        self.sync_native_menu();
    }

    fn set_mouse_look(&mut self, active: bool) {
        self.right_mouse_down = active;
        if let Some(win) = &self.window
            && self.editor.camera.mode == ViewMode::Perspective3D
        {
            if active {
                let _ = win
                    .set_cursor_grab(CursorGrabMode::Locked)
                    .or_else(|_| win.set_cursor_grab(CursorGrabMode::Confined));
                win.set_cursor_visible(false);
            } else {
                let _ = win.set_cursor_grab(CursorGrabMode::None);
                win.set_cursor_visible(true);
            }
        }
    }

    fn handle_shortcut(&mut self, code: KeyCode) -> bool {
        let Some(gpu) = &mut self.gpu else {
            return false;
        };
        let cmd = self.modifiers.control_key() || self.modifiers.super_key();
        if cmd {
            match code {
                KeyCode::KeyZ => {
                    if self.modifiers.shift_key() {
                        self.editor.redo(&mut gpu.renderer);
                    } else {
                        self.editor.undo(&mut gpu.renderer);
                    }
                    return true;
                }
                KeyCode::KeyY => {
                    self.editor.redo(&mut gpu.renderer);
                    return true;
                }
                KeyCode::KeyS => {
                    self.editor.save_world_async();
                    return true;
                }
                KeyCode::KeyC => {
                    self.editor.copy_selection();
                    return true;
                }
                KeyCode::KeyX => {
                    self.editor.cut_selection(&mut gpu.renderer);
                    return true;
                }
                KeyCode::KeyV => {
                    self.editor.paste_from_clipboard();
                    return true;
                }
                KeyCode::KeyD => {
                    self.editor.selection.clear();
                    return true;
                }
                _ => {}
            }
        }

        match code {
            KeyCode::F3 => {
                self.editor.show_profiler_overlay = !self.editor.show_profiler_overlay;
                true
            }
            KeyCode::F4 => {
                self.editor.toggle_trace();
                true
            }
            KeyCode::F11 => {
                if let Some(win) = &self.window {
                    if win.fullscreen().is_some() {
                        win.set_fullscreen(None);
                    } else {
                        win.set_fullscreen(Some(Fullscreen::Borderless(None)));
                    }
                }
                true
            }
            KeyCode::Tab => {
                self.editor.camera.mode = match self.editor.camera.mode {
                    ViewMode::Perspective3D => ViewMode::Chunk2D,
                    ViewMode::Chunk2D => ViewMode::Perspective3D,
                };
                true
            }
            KeyCode::BracketLeft => {
                self.editor.adjust_camera_speed(0.8);
                true
            }
            KeyCode::BracketRight => {
                self.editor.adjust_camera_speed(1.25);
                true
            }
            KeyCode::Digit1 => {
                self.editor.switch_tool(ToolId::Select);
                true
            }
            KeyCode::Digit2 => {
                self.editor.switch_tool(ToolId::Brush);
                true
            }
            KeyCode::Digit3 => {
                self.editor.switch_tool(ToolId::Clone);
                true
            }
            KeyCode::Digit4 => {
                self.editor.switch_tool(ToolId::Fill);
                true
            }
            KeyCode::Digit5 => {
                self.editor.switch_tool(ToolId::Filter);
                true
            }
            KeyCode::Digit6 => {
                self.editor.switch_tool(ToolId::Import);
                true
            }
            KeyCode::Digit7 => {
                self.editor.switch_tool(ToolId::Player);
                true
            }
            KeyCode::Digit8 => {
                self.editor.switch_tool(ToolId::Spawn);
                true
            }
            KeyCode::Digit9 => {
                self.editor.switch_tool(ToolId::Chunk);
                true
            }
            KeyCode::Digit0 => {
                self.editor.switch_tool(ToolId::NbtExplorer);
                true
            }
            KeyCode::KeyT => {
                self.editor.switch_tool(ToolId::Teleport);
                true
            }
            KeyCode::Delete | KeyCode::Backspace => {
                if self.editor.active_tool == ToolId::Select {
                    self.editor.delete_selected_blocks(&mut gpu.renderer);
                    return true;
                }
                false
            }
            KeyCode::KeyE => {
                match self.editor.active_tool {
                    ToolId::Clone => clone::apply_transform(&mut self.editor, TransformOp::RotateCW),
                    ToolId::Fill => fill::apply_transform(&mut self.editor, TransformOp::RotateCW),
                    _ => return false,
                }
                true
            }
            KeyCode::KeyR => {
                match self.editor.active_tool {
                    ToolId::Clone => clone::apply_transform(&mut self.editor, TransformOp::Roll),
                    ToolId::Fill => fill::apply_transform(&mut self.editor, TransformOp::Roll),
                    _ => return false,
                }
                true
            }
            KeyCode::KeyF => {
                match self.editor.active_tool {
                    ToolId::Clone => clone::apply_transform(&mut self.editor, TransformOp::FlipX),
                    ToolId::Fill => fill::apply_transform(&mut self.editor, TransformOp::FlipY),
                    _ => return false,
                }
                true
            }
            KeyCode::KeyG => {
                if self.editor.active_tool == ToolId::Clone {
                    clone::apply_transform(&mut self.editor, TransformOp::FlipZ);
                    return true;
                }
                false
            }
            KeyCode::Enter => {
                if self.editor.active_tool == ToolId::Clone {
                    clone::commit_clone(&mut self.editor, &mut gpu.renderer);
                    return true;
                }
                false
            }
            KeyCode::Escape => {
                if self.editor.invalid_world_prompt.is_some() {
                    self.editor.invalid_world_prompt = None;
                } else if self.editor.file_menu_open {
                    self.editor.file_menu_open = false;
                } else if self.editor.settings_open {
                    self.editor.settings_open = false;
                } else if self.editor.selection.box_.is_some()
                    || self.editor.selection.corner_a.is_some()
                    || self.editor.selection.click_selecting
                {
                    self.editor.selection.clear();
                    self.editor.left_panel_open = false;
                } else if self.editor.active_tool != ToolId::Select {
                    self.editor.active_tool = ToolId::Select;
                    self.editor.left_panel_open = false;
                    self.editor.preview_dirty = true;
                } else if self.editor.left_panel_open {
                    self.editor.left_panel_open = false;
                } else {
                    self.editor.file_menu_open = true;
                }
                true
            }
            _ => {
                let barrier_bind = self
                    .editor
                    .config
                    .keybinds
                    .get(&KeyAction::ToggleBarriers)
                    .map(|s| s.as_str())
                    .unwrap_or("B");
                if matches_keybind(barrier_bind, code, self.modifiers) {
                    self.editor.config.show_barriers = !self.editor.config.show_barriers;
                    mcedit_core::set_hide_barriers(!self.editor.config.show_barriers);
                    self.editor.config.save();
                    self.editor.invalidate_all_meshes(&mut gpu.renderer);
                    let state = if self.editor.config.show_barriers {
                        "Shown"
                    } else {
                        "Hidden"
                    };
                    self.editor.set_toast(format!("Barrier blocks: {state}"));
                    return true;
                }
                false
            }
        }
    }

    fn handle_viewport_left_down(&mut self) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        if self.editor.world.is_none() || self.editor.jobs.is_busy() {
            return;
        }
        let vp_size = Vec2::new(
            gpu.surface_config.width as f32,
            gpu.surface_config.height as f32,
        );
        let (ray_origin, ray_dir) = self.editor.camera.screen_ray(self.cursor_pos, vp_size);
        let hit_pos = self.editor.hover_hit.map(|h| h.block_pos);

        match self.editor.active_tool {
            ToolId::Select => {
                // Always run selection click even with no block under cursor (supports
                // grabbing selection walls from any distance and selecting in midair!)
                select::handle_left_click(&mut self.editor, ray_origin, ray_dir);
            }
            ToolId::Brush => {
                self.editor.brush.is_painting = true;
                if let Some(center) = self.editor.brush_target_pos() {
                    brush::handle_paint_at(&mut self.editor, center, false, &mut gpu.renderer);
                }
            }
            ToolId::Clone => {
                if let Some(bp) = hit_pos {
                    clone::handle_left_click(&mut self.editor, bp);
                }
            }
            ToolId::Chunk => {
                if let Some(bp) = hit_pos {
                    chunk::handle_left_click(&mut self.editor, bp, self.modifiers.shift_key());
                }
            }
            ToolId::Player => {
                if let Some(bp) = hit_pos {
                    player::handle_player_move_click(&mut self.editor, bp);
                }
            }
            ToolId::Spawn => {
                if let Some(bp) = hit_pos {
                    player::handle_spawn_move_click(&mut self.editor, bp);
                }
            }
            ToolId::Teleport => {
                let bp = hit_pos.unwrap_or_else(|| self.editor.selection_target_pos(ray_origin, ray_dir));
                teleport::handle_left_click(&mut self.editor, bp);
            }
            _ => {}
        }
    }

    fn render_frame(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;
        if dt > 0.0001 {
            self.fps_ema = self.fps_ema * 0.9 + (1.0 / dt).min(240.0) * 0.1;
        }

        if self.editor.should_exit {
            event_loop.exit();
            return;
        }

        self.editor.camera.base_speed = self.editor.config.camera_speed;

        // Reload resource pack atlas if requested
        if let Some(pack_opt) = self.editor.pending_atlas_reload.take()
            && let Some(gpu) = &mut self.gpu
        {
            let atlas = match pack_opt {
                Some(p) => TextureAtlas::from_resource_pack(&p),
                None => TextureAtlas::default_atlas(),
            };
            self.editor.update_lod_colors_from_atlas(&atlas);
            gpu.renderer.update_atlas(&gpu.device, &gpu.queue, &atlas);
            self.editor.invalidate_all_meshes(&mut gpu.renderer);
            let atlas_img = egui::ColorImage::from_rgba_unmultiplied(
                [atlas.rgba.width() as usize, atlas.rgba.height() as usize],
                atlas.rgba.as_raw(),
            );
            gpu.atlas_egui_tex
                .set(atlas_img, egui::TextureOptions::NEAREST);
        }

        // Poll background jobs & chunk meshing
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        self.editor.poll_background_jobs(&mut gpu.renderer);

        let want_vsync = self.editor.config.vsync;
        if self.applied_vsync != want_vsync {
            gpu.surface_config.present_mode = if want_vsync {
                wgpu::PresentMode::AutoVsync
            } else {
                wgpu::PresentMode::AutoNoVsync
            };
            gpu.surface.configure(&gpu.device, &gpu.surface_config);
            self.applied_vsync = want_vsync;
        }

        // Update camera movement when egui doesn't own keyboard focus
        let egui_wants_kb = gpu.egui_state.egui_ctx().egui_wants_keyboard_input();
        if self.editor.world.is_some() && !egui_wants_kb && !self.editor.jobs.is_busy() {
            let cam_in = CameraInputs {
                forward: self.held_keys.contains(&KeyCode::KeyW)
                    || self.held_keys.contains(&KeyCode::ArrowUp),
                backward: self.held_keys.contains(&KeyCode::KeyS)
                    || self.held_keys.contains(&KeyCode::ArrowDown),
                left: self.held_keys.contains(&KeyCode::KeyA)
                    || self.held_keys.contains(&KeyCode::ArrowLeft),
                right: self.held_keys.contains(&KeyCode::KeyD)
                    || self.held_keys.contains(&KeyCode::ArrowRight),
                up: self.held_keys.contains(&KeyCode::Space),
                down: self.held_keys.contains(&KeyCode::ShiftLeft)
                    || self.held_keys.contains(&KeyCode::ShiftRight),
                sprint: self.held_keys.contains(&KeyCode::ControlLeft),
                brake: false,
            };
            self.editor.camera.update(dt, &cam_in);
        }

        // Non-blocking multi-tier raycast (LOD 0 + distant 3D LOD + unlimited Y=0 floor)
        let t_ray = Instant::now();
        let vp_size = Vec2::new(
            gpu.surface_config.width as f32,
            gpu.surface_config.height as f32,
        );
        let max_ray_dist = ((self.editor.config.view_distance.clamp(2, 1024) * 16).max(2048)) as f32;
        let egui_wants_ptr = gpu.egui_state.egui_ctx().is_pointer_over_egui();
        if !egui_wants_ptr && !self.right_mouse_down {
            let (origin, dir) = self.editor.camera.screen_ray(self.cursor_pos, vp_size);
            self.editor.cursor_ray = Some((origin, dir));
            if let Some(world) = &self.editor.world {
                let dim = world.active_dim;
                self.editor.hover_hit = raycast_world_with_lod(
                    world,
                    dim,
                    Some(&self.editor.lod_raycast),
                    origin,
                    dir,
                    max_ray_dist,
                    self.editor.camera.mode,
                );
            } else {
                self.editor.hover_hit = None;
            }
        } else if self.right_mouse_down {
            let (origin, dir) = (self.editor.camera.pos, self.editor.camera.forward_vec());
            self.editor.cursor_ray = Some((origin, dir));
            if let Some(world) = &self.editor.world {
                let dim = world.active_dim;
                self.editor.hover_hit = raycast_world_with_lod(
                    world,
                    dim,
                    Some(&self.editor.lod_raycast),
                    origin,
                    dir,
                    max_ray_dist,
                    self.editor.camera.mode,
                );
            }
        } else {
            self.editor.cursor_ray = None;
        }

        let raycast_dur = t_ray.elapsed();
        mcedit_core::profiler::record(mcedit_core::profiler::Stage::FrameRaycast, raycast_dur);

        // Handle active left drag for Select / Brush
        if self.left_mouse_down && !egui_wants_ptr && !self.right_mouse_down {
            if self.editor.active_tool == ToolId::Select {
                let (origin, dir) = self.editor.camera.screen_ray(self.cursor_pos, vp_size);
                select::handle_left_drag(&mut self.editor, origin, dir);
            } else if self.editor.active_tool == ToolId::Brush
                && self.editor.brush.is_painting
                && let Some(center) = self.editor.brush_target_pos()
            {
                brush::handle_paint_at(&mut self.editor, center, true, &mut gpu.renderer);
            }
        }

        let t_pump = Instant::now();
        self.editor.pump_chunk_meshing(&gpu.device, &mut gpu.renderer);
        let pump_dur = t_pump.elapsed();
        mcedit_core::profiler::record(mcedit_core::profiler::Stage::FramePump, pump_dur);
        self.editor.build_overlays(&mut self.overlays);

        let Some(win) = &self.window else {
            return;
        };
        let t_acq = Instant::now();
        let output = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.surface_config);
                win.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => {
                win.request_redraw();
                return;
            }
        };
        let acquire_dur = t_acq.elapsed();
        let target_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mcedit_frame"),
            });

        // 1. Render 3D world + LODs + cached static grid + dynamic overlays (no distance fog)
        let t_render = Instant::now();
        let sky = self.editor.sky_color();
        gpu.renderer.render(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &target_view,
            &self.editor.camera,
            (gpu.surface_config.width, gpu.surface_config.height),
            sky,
            &self.overlays,
        );

        let render_dur = t_render.elapsed();
        mcedit_core::profiler::record(mcedit_core::profiler::Stage::FrameRender, render_dur);

        // 2. Build egui UI
        let t_egui = Instant::now();
        let raw_input = gpu.egui_state.take_egui_input(win);
        let atlas_tex_id = gpu.atlas_egui_tex.id();
        let fps = self.fps_ema;
        let mut left_panel_rect: Option<egui::Rect> = None;
        let mut full_output = gpu.egui_state.egui_ctx().run_ui(raw_input, |ui| {
            let ctx = ui.ctx().clone();
            top_bar::draw(ui, &mut self.editor, &mut gpu.renderer);
            toolbar::draw(ui, &mut self.editor);
            status_bar::draw(ui, &self.editor, &gpu.renderer, fps);
            clipboard_panel::draw(ui, &mut self.editor);
            welcome::draw(ui, &mut self.editor, &mut gpu.renderer);

            if (self.editor.world.is_some() || self.editor.jobs.is_busy())
                && self.editor.should_show_left_panel()
            {
                let panel_frame = match self.editor.config.left_panel_style {
                    PanelBackgroundStyle::BlurredGlass => {
                        egui::Frame::side_top_panel(ui.style())
                            .fill(egui::Color32::from_rgba_unmultiplied(16, 18, 24, 138))
                    }
                    PanelBackgroundStyle::Transparent => {
                        egui::Frame::side_top_panel(ui.style())
                            .fill(egui::Color32::from_rgba_unmultiplied(16, 18, 24, 175))
                    }
                    PanelBackgroundStyle::Solid => egui::Frame::side_top_panel(ui.style()),
                };
                let panel_resp = egui::Panel::left("tool_panel")
                    .default_size(250.0)
                    .frame(panel_frame)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            match self.editor.active_tool {
                                ToolId::Select => {
                                    select::draw_panel(ui, &mut self.editor, &mut gpu.renderer);
                                }
                                ToolId::Fill => {
                                    fill::draw_panel(ui, &mut self.editor, &mut gpu.renderer);
                                }
                                ToolId::Clone | ToolId::Import => {
                                    clone::draw_panel(ui, &mut self.editor, &mut gpu.renderer);
                                }
                                ToolId::Brush => brush::draw_panel(ui, &mut self.editor),
                                ToolId::Chunk => {
                                    chunk::draw_panel(ui, &mut self.editor, &mut gpu.renderer);
                                }
                                ToolId::NbtExplorer => nbt_explorer::draw_panel(ui, &mut self.editor),
                                ToolId::Player => player::draw_player_panel(ui, &mut self.editor),
                                ToolId::Spawn => player::draw_spawn_panel(ui, &mut self.editor),
                                ToolId::Filter => filter::draw_panel(ui, &mut self.editor),
                                ToolId::Teleport => teleport::draw_panel(ui, &mut self.editor),
                            }
                        });
                    });
                left_panel_rect = Some(panel_resp.response.rect);
            }

            block_picker::draw(&ctx, &mut self.editor, Some(atlas_tex_id));
            world_info::draw(&ctx, &mut self.editor, &mut gpu.renderer);
            create_world::draw(&ctx, &mut self.editor, &mut gpu.renderer);
            settings_dialog::draw(&ctx, &mut self.editor, &mut gpu.renderer);
            draw_file_menu_modal(&ctx, &mut self.editor, &mut gpu.renderer);
            draw_analysis_modal(&ctx, &mut self.editor);
            draw_job_modal(&ctx, &mut self.editor);
            draw_quit_modal(&ctx, &mut self.editor);
            draw_invalid_world_modal(&ctx, &mut self.editor, &mut gpu.renderer);
            crate::ui::profiler_overlay::draw(&ctx, &mut self.editor, &gpu.renderer);
            if self.hovered_drop_file.is_some() || self.wayland_hovering {
                let label = self
                    .hovered_drop_file
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("world");
                egui::Area::new(egui::Id::unique("file_drop_overlay"))
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .show(&ctx, |ui| {
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgba_unmultiplied(18, 22, 30, 225))
                            .stroke(egui::Stroke::new(
                                2.0,
                                egui::Color32::from_rgb(90, 175, 255),
                            ))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(28, 18))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(format!("Drop to open: {label}"))
                                        .size(20.0)
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                );
                            });
                    });
            }
        });
        let egui_dur = t_egui.elapsed();
        mcedit_core::profiler::record(mcedit_core::profiler::Stage::FrameEgui, egui_dur);

        // 2b. Apply real-time frosted-glass backdrop blur behind the left panel before rendering egui
        let t_blur = Instant::now();
        if self.editor.config.left_panel_style == PanelBackgroundStyle::BlurredGlass
            && gpu.surface_supports_copy_src
            && let Some(rect) = left_panel_rect
        {
            let ppp = full_output.pixels_per_point;
            let x0 = (rect.min.x * ppp).floor().max(0.0) as u32;
            let y0 = (rect.min.y * ppp).floor().max(0.0) as u32;
            let x1 = (rect.max.x * ppp).ceil().max(x0 as f32) as u32;
            let y1 = (rect.max.y * ppp).ceil().max(y0 as f32) as u32;
            gpu.renderer.apply_panel_backdrop_blur(
                &mut encoder,
                &output.texture,
                &target_view,
                [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)],
            );
        }

        let blur_dur = t_blur.elapsed();
        mcedit_core::profiler::record(mcedit_core::profiler::Stage::FrameBlur, blur_dur);

        gpu.egui_state
            .handle_platform_output(win, full_output.platform_output);

        let tris = gpu
            .egui_state
            .egui_ctx()
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        for (id, deltas) in full_output.textures_delta.set.drain() {
            for delta in deltas {
                gpu.egui_renderer
                    .update_texture(&gpu.device, &gpu.queue, id, &delta);
            }
        }

        let screen_desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gpu.surface_config.width, gpu.surface_config.height],
            pixels_per_point: full_output.pixels_per_point,
        };
        let egui_cmd_bufs = gpu.egui_renderer.update_buffers(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &tris,
            &screen_desc,
        );

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            gpu.egui_renderer
                .render(&mut pass.forget_lifetime(), &tris, &screen_desc);
        }

        let t_present = Instant::now();
        gpu.queue
            .submit(egui_cmd_bufs.into_iter().chain(std::iter::once(encoder.finish())));
        win.pre_present_notify();
        gpu.queue.present(output);
        let present_dur = acquire_dur + t_present.elapsed();
        {
            use mcedit_core::profiler::{self, Counter, FrameSample, Stage};
            profiler::record(Stage::FramePresent, present_dur);
            let ms = |d: std::time::Duration| d.as_secs_f32() * 1000.0;
            profiler::push_frame(FrameSample {
                total_ms: dt * 1000.0,
                pump_ms: ms(pump_dur),
                raycast_ms: ms(raycast_dur),
                render_ms: ms(render_dur),
                blur_ms: ms(blur_dur),
                egui_ms: ms(egui_dur),
                present_ms: ms(present_dur),
                draw_calls: profiler::take(Counter::DrawCalls) as u32,
                triangles: profiler::take(Counter::Triangles),
            });
        }

        for id in full_output.textures_delta.free.drain() {
            gpu.egui_renderer.free_texture(&id);
        }

        self.frame_count += 1;
        if self.frame_count <= 3 {
            tracing::info!(
                "Rendered frame {}: {}x{} egui_tris={} ppp={}",
                self.frame_count,
                gpu.surface_config.width,
                gpu.surface_config.height,
                tris.len(),
                full_output.pixels_per_point
            );
        }

        win.request_redraw();
        self.sync_native_menu();
    }
}

impl ApplicationHandler<UserEvent> for McEditApp {
    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "linux")]
        if let Some(dnd) = &self.wayland_dnd {
            dnd.dispatch_pending();
        }
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(menu_ev) => {
                if let Some(cmd) = MenuCommand::from_id(menu_ev.id.as_ref()) {
                    self.handle_menu_command(cmd, event_loop);
                }
            }
            UserEvent::WaylandHover(hovering) => {
                self.wayland_hovering = hovering;
            }
            UserEvent::WaylandDrop { path, offer_ptr } => {
                self.wayland_hovering = false;
                #[cfg(target_os = "linux")]
                if let Some(dnd) = &self.wayland_dnd {
                    dnd.finish_offer(offer_ptr);
                }
                #[cfg(not(target_os = "linux"))]
                let _ = offer_ptr;
                if let Some(p) = path {
                    if self.editor.jobs.is_busy() {
                        self.editor
                            .set_toast("Wait for current operation to finish before opening a world.");
                    } else if let Some(gpu) = &mut self.gpu {
                        self.editor.open_world(&p, &mut gpu.renderer);
                        self.sync_native_menu();
                    } else {
                        self.initial_world = Some(p);
                    }
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.native_menu.is_none() {
            self.native_menu = NativeMenuBar::init(
                &self.editor.config,
                self.editor.camera.mode == ViewMode::Chunk2D,
            );
        }
        if self.window.is_none() {
            let default_size = event_loop
                .primary_monitor()
                .or_else(|| event_loop.available_monitors().next())
                .map(|m| m.size())
                .unwrap_or(PhysicalSize::new(1360, 820));
            let attrs = Window::default_attributes()
                .with_title("MCEdit")
                .with_window_icon(load_window_icon())
                .with_inner_size(default_size)
                .with_min_inner_size(PhysicalSize::new(960, 620))
                .with_maximized(true);
            let win = Arc::new(
                event_loop
                    .create_window(attrs)
                    .expect("create main window"),
            );
            win.set_maximized(true);
            #[cfg(target_os = "linux")]
            {
                self.wayland_dnd =
                    crate::wayland_dnd::WaylandDnd::attach(&win, self.event_proxy.clone());
            }
            self.init_gpu(win);
            self.sync_native_menu();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event
            && self.right_mouse_down
        {
            if self.editor.camera.mode == ViewMode::Perspective3D {
                self.editor.camera.apply_mouse_delta(
                    dx as f32,
                    dy as f32,
                    self.editor.config.mouse_sensitivity,
                );
            } else {
                self.editor.camera.pan_2d(dx as f32, dy as f32);
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if let Some(gpu) = &mut self.gpu
            && let Some(win) = &self.window
        {
            let resp = gpu.egui_state.on_window_event(win, &event);
            if resp.consumed
                && !matches!(
                    event,
                    WindowEvent::RedrawRequested
                        | WindowEvent::Resized(_)
                        | WindowEvent::DroppedFile(_)
                        | WindowEvent::HoveredFile(_)
                        | WindowEvent::HoveredFileCancelled
                )
            {
                return;
            }
        }

        match event {
            WindowEvent::HoveredFile(path) => {
                self.hovered_drop_file = Some(path);
            }
            WindowEvent::HoveredFileCancelled => {
                self.hovered_drop_file = None;
            }
            WindowEvent::DroppedFile(path) => {
                self.hovered_drop_file = None;
                if self.editor.jobs.is_busy() {
                    self.editor
                        .set_toast("Wait for current operation to finish before opening a world.");
                } else if let Some(gpu) = &mut self.gpu {
                    self.editor.open_world(&path, &mut gpu.renderer);
                    self.sync_native_menu();
                } else {
                    self.initial_world = Some(path);
                }
            }
            WindowEvent::CloseRequested => {
                if self
                    .editor
                    .world
                    .as_ref()
                    .is_some_and(|w| w.has_unsaved_changes())
                {
                    self.editor.confirm_quit_open = true;
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(new_size) => {
                if let Some(gpu) = &mut self.gpu
                    && new_size.width > 0
                    && new_size.height > 0
                {
                    gpu.surface_config.width = new_size.width;
                    gpu.surface_config.height = new_size.height;
                    gpu.surface.configure(&gpu.device, &gpu.surface_config);
                    gpu.renderer
                        .resize(&gpu.device, new_size.width, new_size.height);
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = Vec2::new(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
                };
                if scroll_y.abs() < 1e-4 {
                    return;
                }
                if self.editor.camera.mode == ViewMode::Chunk2D {
                    if let Some(gpu) = &self.gpu {
                        let vp = Vec2::new(
                            gpu.surface_config.width as f32,
                            gpu.surface_config.height as f32,
                        );
                        self.editor.camera.zoom_2d(scroll_y, self.cursor_pos, vp);
                    }
                } else if self.right_mouse_down || self.editor.active_tool != ToolId::Brush {
                    let factor = if scroll_y > 0.0 { 1.15 } else { 1.0 / 1.15 };
                    self.editor.adjust_camera_speed(factor);
                } else {
                    self.editor.tool_reach = (self.editor.tool_reach + scroll_y).clamp(2.0, 64.0);
                    self.editor.preview_dirty = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let over_ui = self
                    .gpu
                    .as_ref()
                    .is_some_and(|g| g.egui_state.egui_ctx().is_pointer_over_egui());
                match button {
                    MouseButton::Right => {
                        if state == ElementState::Pressed && !over_ui && self.editor.world.is_some()
                        {
                            self.set_mouse_look(true);
                        } else if state == ElementState::Released {
                            self.set_mouse_look(false);
                        }
                    }
                    MouseButton::Left => {
                        if state == ElementState::Pressed && !over_ui {
                            self.left_mouse_down = true;
                            self.handle_viewport_left_down();
                        } else if state == ElementState::Released {
                            self.left_mouse_down = false;
                            select::handle_left_release(&mut self.editor);
                            brush::end_stroke(&mut self.editor);
                        }
                    }
                    MouseButton::Middle => {
                        if state == ElementState::Pressed
                            && !over_ui
                            && let Some(hit) = self.editor.hover_hit
                            && let Some(w) = &mut self.editor.world
                        {
                            let dim = w.active_dim;
                            let id = w.get_block(dim, hit.block_pos);
                            let st = w.registry.read().get_state(id).clone();
                            match self.editor.active_tool {
                                ToolId::Fill => self.editor.fill.fill_block = st,
                                ToolId::Brush => {
                                    let bid = self
                                        .editor
                                        .brush
                                        .selected_mode_id(&self.editor.plugins)
                                        .to_string();
                                    if let Some(map) = self.editor.brush.params_by_id.get_mut(&bid)
                                    {
                                        map.insert(
                                            "block".into(),
                                            mcedit_plugins::ParamValue::Block {
                                                state: st,
                                                match_base_only: false,
                                            },
                                        );
                                        self.editor.preview_dirty = true;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let egui_kb = self
                        .gpu
                        .as_ref()
                        .is_some_and(|g| g.egui_state.egui_ctx().egui_wants_keyboard_input());
                    match event.state {
                        ElementState::Pressed => {
                            if !egui_kb && !self.handle_shortcut(code) {
                                self.held_keys.insert(code);
                            }
                        }
                        ElementState::Released => {
                            self.held_keys.remove(&code);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.render_frame(event_loop);
            }
            _ => {}
        }
    }
}

fn draw_file_menu_modal(
    ctx: &egui::Context,
    editor: &mut EditorState,
    renderer: &mut WorldRenderer,
) {
    if !editor.file_menu_open {
        return;
    }
    let mut open = editor.file_menu_open;
    let mut close_now = false;
    egui::Window::new("File Menu")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(260.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.vertical_centered_justified(|ui| {
                if ui.button("Open World...").clicked() {
                    close_now = true;
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        editor.open_world(&dir, renderer);
                    }
                }
                if ui.button("Open level.dat...").clicked() {
                    close_now = true;
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("level.dat", &["dat"])
                        .pick_file()
                    {
                        editor.open_world(&file, renderer);
                    }
                }
                if ui.button("Create New World...").clicked() {
                    close_now = true;
                    editor.create_world = Some(CreateWorldDraft::default());
                }

                ui.separator();
                let has_world = editor.world.is_some();
                if ui
                    .add_enabled(
                        has_world && !editor.jobs.is_busy(),
                        egui::Button::new("Save World"),
                    )
                    .clicked()
                {
                    close_now = true;
                    editor.save_world_async();
                }
                if ui
                    .add_enabled(has_world, egui::Button::new("World Info..."))
                    .clicked()
                {
                    close_now = true;
                    if let Some(w) = &editor.world {
                        editor.world_info = Some(WorldInfoDraft::from_world(w));
                    }
                }
                if ui
                    .add_enabled(has_world, egui::Button::new("Close World"))
                    .clicked()
                {
                    close_now = true;
                    editor.close_world(renderer);
                }

                ui.separator();
                if ui.button("Settings & Keybindings...").clicked() {
                    close_now = true;
                    editor.settings_open = true;
                }

                ui.separator();
                if ui.button("Quit MCEdit").clicked() {
                    close_now = true;
                    if editor.world.as_ref().is_some_and(|w| w.has_unsaved_changes()) {
                        editor.confirm_quit_open = true;
                    } else {
                        editor.should_exit = true;
                    }
                }
            });
        });
    editor.file_menu_open = open && !close_now;
}

fn draw_analysis_modal(ctx: &egui::Context, editor: &mut EditorState) {
    let Some(modal) = editor.analysis_modal.take() else {
        return;
    };
    let mut keep = true;
    egui::Window::new("Selection Analysis")
        .open(&mut keep)
        .default_width(480.0)
        .default_height(420.0)
        .show(ctx, |ui| {
            let sz = modal.box_.size();
            ui.label(format!(
                "Region: {}x{}x{} ({} blocks)",
                sz.x, sz.y, sz.z, modal.result.total_blocks
            ));
            if ui.button("Export CSV...").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("CSV", &["csv"])
                    .set_file_name("analysis.csv")
                    .save_file()
            {
                let reg = editor.registry.read();
                let _ = std::fs::write(path, modal.result.to_csv(&reg));
            }
            ui.separator();
            let reg = editor.registry.read();
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                egui::Grid::new("analysis_grid").striped(true).show(ui, |ui| {
                    ui.strong("Type");
                    ui.strong("Name");
                    ui.strong("Count");
                    ui.end_row();
                    for &(id, cnt) in &modal.result.block_counts {
                        ui.label("Block");
                        ui.label(format!(
                            "{} ({})",
                            reg.get_meta(id).display_name,
                            reg.get_state(id)
                        ));
                        ui.label(cnt.to_string());
                        ui.end_row();
                    }
                    for (name, cnt) in &modal.result.entity_counts {
                        ui.label("Entity");
                        ui.label(name);
                        ui.label(cnt.to_string());
                        ui.end_row();
                    }
                    for (name, cnt) in &modal.result.block_entity_counts {
                        ui.label("BlockEntity");
                        ui.label(name);
                        ui.label(cnt.to_string());
                        ui.end_row();
                    }
                });
            });
        });
    if keep {
        editor.analysis_modal = Some(modal);
    }
}

fn draw_job_modal(ctx: &egui::Context, editor: &mut EditorState) {
    let Some(active) = &editor.jobs.active else {
        return;
    };
    egui::Window::new(&active.progress.title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let msg = active.progress.message.lock().clone();
            ui.label(msg);
            if let Some(frac) = active.progress.fraction() {
                ui.add(egui::ProgressBar::new(frac).show_percentage());
            } else {
                ui.spinner();
            }
            if ui.button("Cancel").clicked() {
                active.progress.cancel();
            }
        });
}

fn draw_quit_modal(ctx: &egui::Context, editor: &mut EditorState) {
    if !editor.confirm_quit_open {
        return;
    }
    egui::Window::new("Unsaved Changes")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label("You have unsaved edits. Save before quitting?");
            ui.horizontal(|ui| {
                if ui.button("Save and Quit").clicked() {
                    editor.confirm_quit_open = false;
                    if let Some(w) = &mut editor.world {
                        let _ = w.save_all();
                    }
                    editor.should_exit = true;
                }
                if ui.button("Quit Without Saving").clicked() {
                    editor.confirm_quit_open = false;
                    editor.should_exit = true;
                }
                if ui.button("Cancel").clicked() {
                    editor.confirm_quit_open = false;
                }
            });
        });
}

fn draw_invalid_world_modal(
    ctx: &egui::Context,
    editor: &mut EditorState,
    renderer: &mut WorldRenderer,
) {
    let Some(prompt) = editor.invalid_world_prompt.take() else {
        return;
    };
    let mut keep = true;
    let mut close_now = false;
    let mut open_in_browser = false;
    egui::Window::new("Invalid World")
        .open(&mut keep)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(&prompt.error);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(prompt.attempted_path.display().to_string())
                    .small()
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(8.0);
            ui.label("Open this folder in Browse Worlds instead?");
            if editor
                .world
                .as_ref()
                .is_some_and(|w| w.has_unsaved_changes())
            {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Warning: Your currently open world has unsaved changes.",
                    )
                    .small()
                    .color(egui::Color32::from_rgb(240, 190, 90)),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Open in Browse Worlds").clicked() {
                    open_in_browser = true;
                    close_now = true;
                }
                if ui.button("Cancel").clicked() {
                    close_now = true;
                }
            });
        });

    if open_in_browser {
        editor.open_folder_in_browser(prompt.browse_dir, renderer);
    } else if keep && !close_now {
        editor.invalid_world_prompt = Some(prompt);
    }
}

fn matches_keybind(binding: &str, code: KeyCode, mods: ModifiersState) -> bool {
    let mut want_ctrl = false;
    let mut want_shift = false;
    let mut want_alt = false;
    let mut key_part = "";
    for token in binding.split('+') {
        let t = token.trim();
        if t.eq_ignore_ascii_case("ctrl")
            || t.eq_ignore_ascii_case("control")
            || t.eq_ignore_ascii_case("cmd")
            || t.eq_ignore_ascii_case("super")
        {
            want_ctrl = true;
        } else if t.eq_ignore_ascii_case("shift") {
            want_shift = true;
        } else if t.eq_ignore_ascii_case("alt") || t.eq_ignore_ascii_case("option") {
            want_alt = true;
        } else {
            key_part = t;
        }
    }
    let has_ctrl = mods.control_key() || mods.super_key();
    if has_ctrl != want_ctrl || mods.shift_key() != want_shift || mods.alt_key() != want_alt {
        return false;
    }
    let Some(expected) = parse_keycode(key_part) else {
        return false;
    };
    code == expected
}

fn parse_keycode(s: &str) -> Option<KeyCode> {
    match s.to_ascii_uppercase().as_str() {
        "A" => Some(KeyCode::KeyA),
        "B" => Some(KeyCode::KeyB),
        "C" => Some(KeyCode::KeyC),
        "D" => Some(KeyCode::KeyD),
        "E" => Some(KeyCode::KeyE),
        "F" => Some(KeyCode::KeyF),
        "G" => Some(KeyCode::KeyG),
        "H" => Some(KeyCode::KeyH),
        "I" => Some(KeyCode::KeyI),
        "J" => Some(KeyCode::KeyJ),
        "K" => Some(KeyCode::KeyK),
        "L" => Some(KeyCode::KeyL),
        "M" => Some(KeyCode::KeyM),
        "N" => Some(KeyCode::KeyN),
        "O" => Some(KeyCode::KeyO),
        "P" => Some(KeyCode::KeyP),
        "Q" => Some(KeyCode::KeyQ),
        "R" => Some(KeyCode::KeyR),
        "S" => Some(KeyCode::KeyS),
        "T" => Some(KeyCode::KeyT),
        "U" => Some(KeyCode::KeyU),
        "V" => Some(KeyCode::KeyV),
        "W" => Some(KeyCode::KeyW),
        "X" => Some(KeyCode::KeyX),
        "Y" => Some(KeyCode::KeyY),
        "Z" => Some(KeyCode::KeyZ),
        "F1" => Some(KeyCode::F1),
        "F2" => Some(KeyCode::F2),
        "F3" => Some(KeyCode::F3),
        "F4" => Some(KeyCode::F4),
        "F5" => Some(KeyCode::F5),
        "F6" => Some(KeyCode::F6),
        "F7" => Some(KeyCode::F7),
        "F8" => Some(KeyCode::F8),
        "F9" => Some(KeyCode::F9),
        "F10" => Some(KeyCode::F10),
        "F12" => Some(KeyCode::F12),
        _ => None,
    }
}

fn load_window_icon() -> Option<winit::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../../../assets/mcedit_icon.png");
    let img = image::load_from_memory(ICON_PNG).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

#[allow(dead_code)]
fn _anchor(_: IVec3) {}
