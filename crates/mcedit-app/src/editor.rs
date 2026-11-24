use crate::config::{AppConfig, InvalidWorldBehavior};
use crate::jobs::{JobManager, JobOutcome};
use crate::tools::ToolId;
use crate::tools::brush::BrushToolState;
use crate::tools::chunk::ChunkToolState;
use crate::tools::clone::CloneToolState;
use crate::tools::compare::{CompareToolState, CompareWorld};
use crate::tools::fill::FillToolState;
use crate::tools::filter::FilterToolState;
use crate::tools::nbt_explorer::NbtExplorerState;
use crate::tools::player::PlayerToolState;
use crate::tools::select::SelectionState;
use crate::tools::teleport::TeleportToolState;
use crate::ui::block_picker::BlockPickerModal;
use crate::ui::create_world::CreateWorldDraft;
use crate::ui::world_info::WorldInfoDraft;
use ahash::{AHashMap, AHashSet};
use glam::{IVec3, Vec3};
use mcedit_core::{
    AnalysisResult, BlockRegistry, BlockStateId, BoundingBox, Chunk, ChunkPos, DiffKind,
    DimensionId, EditTransaction, Face, LightingEngine, LodChunkSurface, LodChunkVoxels,
    SchematicVolume, World, analyze_box, classify_block_diff, copy_from_format_in_box,
    extract_volume,
};
use mcedit_formats::FormatRegistry;
use mcedit_plugins::PluginRegistry;
use mcedit_render::{
    CameraController, ChunkNeighborSnapshot, LodColorTable, LodRaycastIndex, LodRegionKey,
    LodTileOccupancy, MeshOptions, OverlayMeshBuilder, RaycastHit, SectionMeshData,
    SectionMeshKey, TextureAtlas, ViewMode, WorldRenderer, mesh_chunk_compare, mesh_chunk_full,
    mesh_lod_overhead_tile, mesh_lod_overhead_tile_compare, mesh_lod_voxel_tile,
    mesh_lod_voxel_tile_compare, mesh_volume_preview, raycast_or_air, raycast_world_with_lod,
};
use parking_lot::RwLock;
use rayon::prelude::*;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

type EvictState = (DimensionId, ChunkPos, i32, i32, bool);
type LodScanState = (DimensionId, (i32, i32), i32, i32, bool);
type SharedMeshChunkCache = Arc<RwLock<AHashMap<(DimensionId, ChunkPos), Arc<Chunk>>>>;

pub struct ClipboardEntry {
    pub label: String,
    pub volume: SchematicVolume,
}

#[derive(Debug, Clone)]
pub struct BrowserEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_world: bool,
}

pub struct AnalysisModal {
    pub result: AnalysisResult,
    pub box_: BoundingBox,
}

pub struct InvalidWorldPrompt {
    pub attempted_path: PathBuf,
    pub browse_dir: PathBuf,
    pub error: String,
}

pub enum MeshTaskResult {
    Lod0Chunk {
        world_gen: u64,
        dim: DimensionId,
        pos: ChunkPos,
        mesh: SectionMeshData,
        loaded_clean: Option<Box<Chunk>>,
    },
    Lod0Aborted {
        world_gen: u64,
        dim: DimensionId,
        pos: ChunkPos,
    },
    LodRegion {
        world_gen: u64,
        dim: DimensionId,
        key: LodRegionKey,
        mesh: SectionMeshData,
        occupancy: Option<Arc<LodTileOccupancy>>,
        covered_chunks: Vec<ChunkPos>,
    },
}

pub struct EditorState {
    pub config: AppConfig,
    pub registry: Arc<RwLock<BlockRegistry>>,
    pub lod_colors: Arc<LodColorTable>,
    pub plugins: PluginRegistry,
    pub world: Option<World>,
    pub camera: CameraController,
    pub active_tool: ToolId,
    pub left_panel_open: bool,

    pub selection: SelectionState,
    pub selected_chunks: AHashSet<ChunkPos>,
    pub copy_stack: Vec<ClipboardEntry>,

    pub fill: FillToolState,
    pub clone: CloneToolState,
    pub brush: BrushToolState,
    pub filter: FilterToolState,
    pub chunk: ChunkToolState,
    pub nbt: NbtExplorerState,
    pub player: PlayerToolState,
    pub teleport: TeleportToolState,
    pub compare: CompareToolState,

    pub hover_hit: Option<RaycastHit>,
    pub cursor_ray: Option<(Vec3, Vec3)>,
    pub tool_reach: f32,

    pub jobs: JobManager,
    pub block_picker: Option<BlockPickerModal>,
    pub world_info: Option<WorldInfoDraft>,
    pub create_world: Option<CreateWorldDraft>,
    pub analysis_modal: Option<AnalysisModal>,
    pub file_menu_open: bool,
    pub show_profiler_overlay: bool,
    pub settings_open: bool,
    pub confirm_quit_open: bool,
    pub invalid_world_prompt: Option<InvalidWorldPrompt>,
    pub should_exit: bool,

    pub status_toast: Option<(String, Instant)>,
    pub pending_atlas_reload: Option<Option<PathBuf>>,
    pub preview_dirty: bool,

    // Async multi-LOD meshing & streaming state
    pub lod0_meshed: AHashSet<(DimensionId, ChunkPos)>,
    pub lod0_surface_only: AHashSet<(DimensionId, ChunkPos)>,
    pub lod0_inflight: AHashSet<(DimensionId, ChunkPos)>,
    pub lod0_priority_queue: VecDeque<ChunkPos>,
    pub lod_regions_meshed: AHashMap<(DimensionId, i32, i32), u8>,
    pub lod2d_regions_meshed: AHashSet<(DimensionId, i32, i32)>,
    pub lod_regions_inflight: AHashSet<(DimensionId, LodRegionKey)>,
    pub lod_region_chunks: AHashMap<(DimensionId, LodRegionKey), Vec<ChunkPos>>,
    pub lod_raycast: LodRaycastIndex,
    pub loaded_chunks_for_grid: AHashSet<(DimensionId, ChunkPos)>,
    mesh_tx: Sender<MeshTaskResult>,
    mesh_rx: Receiver<MeshTaskResult>,
    world_gen: u64,
    active_world_gen: Arc<AtomicU64>,
    urgent_near_active: Arc<AtomicUsize>,
    mesh_chunk_cache: SharedMeshChunkCache,

    // Pre-sorted spiral offsets for LOD 0 and cached non-empty 8x8 LOD tiles
    spiral_lod0_radius: i32,
    spiral_lod0_offsets: Vec<(i32, i32)>,
    spiral_cursor: usize,
    last_lod0_cam_cp: Option<ChunkPos>,
    last_evict_state: Option<EvictState>,
    last_lod_scan_state: Option<LodScanState>,
    lod_schedule_dirty: bool,
    last_detail_distance: i32,
    active_lod_tiles: Vec<(i32, i32)>,
    lod_tiles_dirty: bool,

    // World-locked 4-state chunk grid bounded to render distance
    pub chunk_grid_dirty: bool,
    grid_center_cp: ChunkPos,
    last_grid_vd: i32,
    last_grid_rebuild: Instant,
    pub last_streaming_status: Option<(String, Instant)>,
    pub last_dim: Option<DimensionId>,

    // Main-menu folder browser state
    pub browser_dir: PathBuf,
    pub browser_entries: Vec<BrowserEntry>,
    pub browser_dirty: bool,
}

impl EditorState {
    pub fn new(config: AppConfig) -> Self {
        mcedit_core::set_hide_barriers(!config.show_barriers);
        let registry = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
        let lod_colors = {
            let atlas = TextureAtlas::default_atlas();
            let reg = registry.read();
            Arc::new(atlas.build_lod_colors(&reg))
        };
        let browser_dir = config.resolve_browser_dir();
        let plugins = PluginRegistry::with_stock_plugins();
        let brush = BrushToolState::new(&plugins);
        let filter = FilterToolState::new(&plugins);
        let camera = CameraController {
            fov_deg: config.fov_deg,
            fly_mode: config.fly_mode,
            autobrake: config.autobrake,
            base_speed: config.camera_speed,
            ..Default::default()
        };

        let (mesh_tx, mesh_rx) = mpsc::channel();
        let pending_atlas_reload = config.resource_pack_path.clone().map(Some);

        Self {
            config,
            registry,
            lod_colors,
            plugins,
            world: None,
            camera,
            active_tool: ToolId::Select,
            left_panel_open: false,
            selection: SelectionState::default(),
            selected_chunks: AHashSet::new(),
            copy_stack: Vec::new(),
            fill: FillToolState::default(),
            clone: CloneToolState::default(),
            brush,
            filter,
            chunk: ChunkToolState::default(),
            nbt: NbtExplorerState::default(),
            player: PlayerToolState::default(),
            teleport: TeleportToolState::default(),
            compare: CompareToolState::default(),
            hover_hit: None,
            cursor_ray: None,
            tool_reach: 8.0,
            jobs: JobManager::default(),
            block_picker: None,
            world_info: None,
            create_world: None,
            analysis_modal: None,
            file_menu_open: false,
            show_profiler_overlay: false,
            settings_open: false,
            confirm_quit_open: false,
            invalid_world_prompt: None,
            should_exit: false,
            status_toast: None,
            pending_atlas_reload,
            preview_dirty: true,
            lod0_meshed: AHashSet::new(),
            lod0_surface_only: AHashSet::new(),
            lod0_inflight: AHashSet::new(),
            lod0_priority_queue: VecDeque::new(),
            lod_regions_meshed: AHashMap::new(),
            lod2d_regions_meshed: AHashSet::new(),
            lod_regions_inflight: AHashSet::new(),
            lod_region_chunks: AHashMap::new(),
            lod_raycast: LodRaycastIndex::new(),
            loaded_chunks_for_grid: AHashSet::new(),
            mesh_tx,
            mesh_rx,
            world_gen: 1,
            active_world_gen: Arc::new(AtomicU64::new(1)),
            urgent_near_active: Arc::new(AtomicUsize::new(0)),
            mesh_chunk_cache: Arc::new(RwLock::new(AHashMap::new())),
            spiral_lod0_radius: -1,
            spiral_lod0_offsets: Vec::new(),
            spiral_cursor: 0,
            last_lod0_cam_cp: None,
            last_evict_state: None,
            last_lod_scan_state: None,
            lod_schedule_dirty: true,
            last_detail_distance: -1,
            active_lod_tiles: Vec::new(),
            lod_tiles_dirty: true,
            chunk_grid_dirty: true,
            grid_center_cp: ChunkPos::new(0, 0),
            last_grid_vd: -1,
            last_grid_rebuild: Instant::now() - Duration::from_secs(10),
            last_streaming_status: None,
            last_dim: None,
            browser_dir,
            browser_entries: Vec::new(),
            browser_dirty: true,
        }
    }

    /// Returns a human-readable activity string when background chunk/LOD meshing,
    /// spiral scanning, or render-distance grid rebuilding is active.
    pub fn streaming_status(&self) -> Option<String> {
        self.world.as_ref()?;
        let lod0_active = self.lod0_inflight.len() + self.lod0_priority_queue.len();
        let lod_active = self.lod_regions_inflight.len();
        let spiral_active = self.camera.mode != ViewMode::Chunk2D
            && self.config.detail_distance > 0
            && self.spiral_cursor < self.spiral_lod0_offsets.len();

        if lod0_active > 0 && lod_active > 0 {
            return Some(format!(
                "Processing terrain ({lod0_active} chunks, {lod_active} LOD tiles)..."
            ));
        }
        if lod0_active > 0 {
            return Some(format!("Processing 3D chunks ({lod0_active} in flight)..."));
        }
        if lod_active > 0 {
            return Some(format!("Processing LOD terrain ({lod_active} tiles)..."));
        }
        if spiral_active {
            let pct = ((self.spiral_cursor as f32
                / (self.spiral_lod0_offsets.len().max(1) as f32))
                * 100.0)
                .clamp(0.0, 99.0) as u32;
            return Some(format!("Scanning chunks in range ({pct}%)..."));
        }
        if self.chunk_grid_dirty {
            return Some("Rebuilding chunk grid...".to_string());
        }
        if let Some((msg, at)) = &self.last_streaming_status
            && at.elapsed() < Duration::from_millis(300)
        {
            return Some(msg.clone());
        }
        None
    }

    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.status_toast = Some((msg.into(), Instant::now()));
    }

    /// Starts a Chrome Trace / Perfetto recording, or stops it and saves it to the traces dir.
    pub fn toggle_trace(&mut self) {
        use mcedit_core::profiler;
        if profiler::trace_enabled() {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let path = AppConfig::traces_dir().join(format!("mcedit-trace-{secs}.json"));
            match profiler::trace_stop_and_write(&path) {
                Ok(n) => self.set_toast(format!("Saved trace ({n} events): {}", path.display())),
                Err(e) => self.set_toast(format!("Trace save failed: {e}")),
            }
        } else {
            profiler::trace_start();
            self.set_toast("Trace recording started (F4 to stop & save)");
        }
    }

    pub fn update_lod_colors_from_atlas(&mut self, atlas: &TextureAtlas) {
        let reg = self.registry.read();
        self.lod_colors = Arc::new(atlas.build_lod_colors(&reg));
    }

    pub fn adjust_camera_speed(&mut self, factor: f32) {
        let next = (self.config.camera_speed * factor).clamp(2.0, 500.0);
        if (next - self.config.camera_speed).abs() > 0.05 {
            self.config.camera_speed = (next * 10.0).round() / 10.0;
            self.camera.base_speed = self.config.camera_speed;
            self.config.save();
            self.set_toast(format!("Flight speed: {:.0} blocks/s", self.config.camera_speed));
        }
    }

    /// Returns the active world's valid half-open block Y bounds `(min_y, max_y)`
    /// (e.g., `(0, 256)` for pre-1.18 worlds, `(-64, 320)` for 1.18+ worlds).
    #[inline]
    pub fn world_y_bounds(&self) -> (i32, i32) {
        self.world
            .as_ref()
            .map(|w| (w.min_y(), w.max_y()))
            .unwrap_or((0, 256))
    }

    /// Clamps an inclusive block coordinate's Y to `[min_y, max_y - 1]`.
    #[inline]
    pub fn clamp_block_pos_y(&self, mut pos: IVec3) -> IVec3 {
        let (min_y, max_y) = self.world_y_bounds();
        pos.y = pos.y.clamp(min_y, (max_y - 1).max(min_y));
        pos
    }

    /// Clamps a half-open selection `BoundingBox`'s vertical span `[min.y, max.y)` to `[min_y, max_y]`.
    #[inline]
    pub fn clamp_selection_box(&self, mut b: BoundingBox) -> BoundingBox {
        let (min_y, max_y) = self.world_y_bounds();
        b.min.y = b.min.y.clamp(min_y, (max_y - 1).max(min_y));
        b.max.y = b.max.y.clamp((b.min.y + 1).min(max_y), max_y);
        b.max = b.max.max(b.min + IVec3::ONE);
        b
    }

    /// Resolves a world block coordinate for the Selection tool from ANY distance:
    /// LOD 0 block -> distant LOD 1/2 3D voxel -> unlimited Y=0 floor -> midair ray fallback.
    pub fn selection_target_pos(&self, ray_origin: Vec3, ray_dir: Vec3) -> IVec3 {
        if let Some(hit) = self.hover_hit {
            return self.clamp_block_pos_y(hit.block_pos);
        }
        if let Some(world) = &self.world {
            let max_d = ((self.config.view_distance.clamp(2, 1024) * 16).max(2048)) as f32;
            let pos = raycast_or_air(
                world,
                world.active_dim,
                Some(&self.lod_raycast),
                ray_origin,
                ray_dir,
                max_d,
                32.0,
                self.camera.mode,
            )
            .block_pos;
            return self.clamp_block_pos_y(pos);
        }
        let dir = ray_dir.normalize_or(Vec3::NEG_Z);
        let p = ray_origin + dir * 32.0;
        self.clamp_block_pos_y(IVec3::new(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32))
    }

    /// Resolves the live opposite corner while drag-creating a selection box.
    /// Uses live terrain/LOD/Y=0 hit when available, or smoothly tracks on the horizontal
    /// plane `y = start.y` (or matching distance in open sky) so midair drags never jump.
    pub fn selection_drag_target_pos(&self, ray_origin: Vec3, ray_dir: Vec3, start: IVec3) -> IVec3 {
        if let Some(hit) = self.hover_hit {
            return self.clamp_block_pos_y(hit.block_pos);
        }
        let dir = ray_dir.normalize_or(Vec3::NEG_Z);
        let (min_y, max_y) = self.world_y_bounds();

        let plane_y = start.y as f32 + 0.5;
        if dir.y.abs() > 0.05 {
            let t = (plane_y - ray_origin.y) / dir.y;
            if t > 0.0 && t <= 1024.0 {
                let p = ray_origin + dir * t;
                return IVec3::new(
                    p.x.floor() as i32,
                    start.y.clamp(min_y, max_y - 1),
                    p.z.floor() as i32,
                );
            }
        }

        let anchor_dist = (start.as_vec3() + Vec3::splat(0.5) - ray_origin)
            .length()
            .clamp(8.0, 256.0);
        let p = ray_origin + dir * anchor_dist;
        IVec3::new(
            p.x.floor() as i32,
            (p.y.floor() as i32).clamp(min_y, max_y - 1),
            p.z.floor() as i32,
        )
    }

    pub fn set_browser_dir(&mut self, dir: PathBuf) {
        if dir.is_dir() {
            self.browser_dir = dir.clone();
            self.config.last_browser_dir = Some(dir);
            self.config.save();
            self.browser_dirty = true;
        }
    }

    pub fn refresh_browser_entries(&mut self) {
        if !self.browser_dir.is_dir() {
            self.browser_dir = self.config.resolve_browser_dir();
        }
        let mut entries = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.browser_dir) {
            for item in rd.flatten() {
                let path = item.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let is_world = path.join("level.dat").is_file();
                entries.push(BrowserEntry {
                    name: name.to_string(),
                    path,
                    is_world,
                });
            }
        }
        entries.sort_by(|a, b| {
            b.is_world
                .cmp(&a.is_world)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.browser_entries = entries;
        self.browser_dirty = false;
    }

    pub fn open_folder_in_browser(&mut self, dir: PathBuf, renderer: &mut WorldRenderer) {
        if !dir.is_dir() {
            self.set_toast(format!("Folder does not exist: {}", dir.display()));
            return;
        }
        if self.world.is_some() {
            self.close_world(renderer);
        }
        self.set_browser_dir(dir.clone());
        self.set_toast(format!("Browsing folder: {}", dir.display()));
    }

    fn handle_invalid_world(&mut self, path: &Path, error: String, renderer: &mut WorldRenderer) {
        let browse_dir = if path.is_dir() {
            Some(path.to_path_buf())
        } else {
            path.parent().filter(|p| p.is_dir()).map(|p| p.to_path_buf())
        };

        match (self.config.invalid_world_behavior, browse_dir) {
            (InvalidWorldBehavior::OpenInBrowser, Some(dir)) => {
                self.open_folder_in_browser(dir, renderer);
            }
            (InvalidWorldBehavior::AskOpenInBrowser, Some(dir)) => {
                self.invalid_world_prompt = Some(InvalidWorldPrompt {
                    attempted_path: path.to_path_buf(),
                    browse_dir: dir,
                    error,
                });
            }
            _ => {
                self.set_toast(error);
            }
        }
    }

    pub fn open_world(&mut self, path: &Path, renderer: &mut WorldRenderer) {
        let open_res = {
            let reg = self.registry.read();
            FormatRegistry::open_world(path, &reg)
        };
        match open_res {
            Ok(fmt) => match World::open(fmt, self.registry.clone(), self.config.undo_limit) {
                Ok(world) => {
                    let browse_parent = if path.is_file() {
                        path.parent().and_then(|p| p.parent())
                    } else {
                        path.parent()
                    };
                    if let Some(parent) = browse_parent
                        && parent.is_dir()
                    {
                        self.browser_dir = parent.to_path_buf();
                        self.config.last_browser_dir = Some(parent.to_path_buf());
                        self.browser_dirty = true;
                    }
                    self.install_world(world, Some(path.to_path_buf()), renderer);
                }
                Err(e) => {
                    self.handle_invalid_world(path, format!("Failed to load world: {e}"), renderer);
                }
            },
            Err(e) => {
                self.handle_invalid_world(path, format!("Unsupported world: {e}"), renderer);
            }
        }
    }

    pub fn teleport_camera_to(&mut self, pos: Vec3) {
        self.camera.pos = pos;
        self.camera.velocity = Vec3::ZERO;
        self.spiral_cursor = 0;
        self.last_lod0_cam_cp = None;
        self.last_evict_state = None;
        self.last_lod_scan_state = None;
        self.lod_schedule_dirty = true;
        self.chunk_grid_dirty = true;
    }

    pub fn install_world(
        &mut self,
        world: World,
        recent_path: Option<PathBuf>,
        renderer: &mut WorldRenderer,
    ) {
        if let Some(p) = recent_path {
            self.config.push_recent_world(p);
        }
        if let Some(p) = world.players.first() {
            self.camera.pos = Vec3::new(p.pos.x as f32, p.pos.y as f32 + 2.0, p.pos.z as f32);
            self.camera.yaw_deg = -p.rotation[0] + 180.0;
            self.camera.pitch_deg = -p.rotation[1];
        } else {
            let sp = world.metadata.spawn_pos;
            self.camera.pos = Vec3::new(sp.x as f32 + 0.5, sp.y as f32 + 12.0, sp.z as f32 + 0.5);
        }

        // If the initial camera position is far outside any generated chunks in the active dimension,
        // snap to the generated chunk closest to the initial camera / spawn position!
        let active_dim = world.active_dim;
        if let Some(chunk_set) = world.known_chunks.get(&active_dim)
            && !chunk_set.is_empty()
        {
            let cam_cp = ChunkPos::new(
                (self.camera.pos.x / 16.0).floor() as i32,
                (self.camera.pos.z / 16.0).floor() as i32,
            );
            let vd = self.config.view_distance.clamp(16, 1024) as i64;
            let has_nearby = chunk_set.iter().any(|cp| {
                let dx = (cp.cx() - cam_cp.cx()).abs() as i64;
                let dz = (cp.cz() - cam_cp.cz()).abs() as i64;
                dx <= vd && dz <= vd
            });
            if !has_nearby
                && let Some(nearest) = chunk_set.iter().copied().min_by_key(|cp| {
                    let dx = (cp.cx() - cam_cp.cx()) as i64;
                    let dz = (cp.cz() - cam_cp.cz()) as i64;
                    dx * dx + dz * dz
                })
            {
                self.camera.pos = Vec3::new(
                    (nearest.cx() * 16 + 8) as f32,
                    self.camera.pos.y.max(80.0),
                    (nearest.cz() * 16 + 8) as f32,
                );
            }
        }

        self.teleport = TeleportToolState::default();

        self.selection = SelectionState::default();
        self.selected_chunks.clear();
        self.clone.active_volume = None;
        self.clone.placement_origin = None;
        self.nbt.reset_to_world(&world);
        self.last_dim = Some(world.active_dim);
        self.world = Some(world);
        self.invalidate_all_meshes(renderer);
        self.set_toast("World opened.");
    }

    pub fn close_world(&mut self, renderer: &mut WorldRenderer) {
        self.world = None;
        self.compare.world_b = None;
        self.compare.last_error = None;
        self.selection = SelectionState::default();
        self.selected_chunks.clear();
        self.clone.active_volume = None;
        self.clone.placement_origin = None;
        self.invalidate_all_meshes(renderer);
    }

    pub fn open_compare_world(&mut self, path: &Path, renderer: &mut WorldRenderer) {
        match CompareWorld::open_readonly(path, &self.registry) {
            Ok(wb) => {
                let name = wb.display_name.clone();
                self.compare.world_b = Some(wb);
                self.compare.config.enabled = true;
                self.compare.last_error = None;
                self.sync_compare_uniform(renderer);
                self.invalidate_all_meshes(renderer);
                self.set_toast(format!("Comparing against: {name}"));
            }
            Err(e) => {
                self.compare.last_error = Some(format!("Failed to open World B: {e}"));
            }
        }
    }

    pub fn close_compare_world(&mut self, renderer: &mut WorldRenderer) {
        self.compare.world_b = None;
        self.compare.last_error = None;
        self.invalidate_all_meshes(renderer);
        self.set_toast("Closed comparison world.");
    }

    pub fn sync_compare_uniform(&self, renderer: &mut WorldRenderer) {
        renderer.diff_params = self.compare.config.shader_params();
    }

    /// Invalidates distant LOD 1/2/2D tiles only (preserving LOD 0 chunks), used when
    /// transparency sliders finish dragging so distant LOD colors match the updated sliders.
    pub fn invalidate_lod_meshes_only(&mut self, renderer: &mut WorldRenderer) {
        let keys: Vec<LodRegionKey> = self
            .lod_regions_meshed
            .iter()
            .map(|(&(_, rx8, rz8), &lod)| LodRegionKey { rx8, rz8, lod })
            .chain(
                self.lod2d_regions_meshed
                    .iter()
                    .map(|&(_, rx8, rz8)| LodRegionKey { rx8, rz8, lod: 3 }),
            )
            .collect();
        for key in keys {
            renderer.remove_lod_region(&key);
        }
        self.lod_regions_meshed.clear();
        self.lod2d_regions_meshed.clear();
        self.lod_regions_inflight.clear();
        self.lod_region_chunks.clear();
        self.lod_raycast.clear();
        self.lod_schedule_dirty = true;
    }

    /// Returns `(DiffKind, block_a, block_b)` at world coordinate `pos` using only in-memory
    /// cached chunks (zero disk I/O). Returns `None` if either chunk is still streaming.
    pub fn inspect_compare_block(
        &self,
        pos: IVec3,
    ) -> Option<(DiffKind, BlockStateId, BlockStateId)> {
        let world = self.world.as_ref()?;
        let wb = self.compare.world_b.as_ref()?;
        let dim = world.active_dim;
        let cp_a = ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
        let lx = pos.x.rem_euclid(16) as usize;
        let lz = pos.z.rem_euclid(16) as usize;

        let id_a = if pos.y < world.min_y() || pos.y >= world.max_y() || !world.chunk_exists(dim, cp_a)
        {
            BlockStateId::AIR
        } else if let Some(c) = world.get_loaded_chunk(dim, cp_a) {
            c.get_block(lx, pos.y, lz)
        } else {
            let cache = self.mesh_chunk_cache.read();
            let c = cache.get(&(dim, cp_a))?;
            c.get_block(lx, pos.y, lz)
        };

        let off = self.compare.config.chunk_offset;
        let pos_b = IVec3::new(pos.x + off.0 * 16, pos.y, pos.z + off.1 * 16);
        let id_b = wb.get_cached_block(dim, pos_b)?;

        let reg = self.registry.read();
        let kind = classify_block_diff(
            id_a,
            id_b,
            &reg,
            self.compare.config.ignore_properties,
            !self.config.show_barriers,
        );
        Some((kind, id_a, id_b))
    }

    /// Raycasts the active world (and cached World B chunks when Compare mode is active)
    /// so cursor hover works seamlessly on B-only ghost blocks as well as World A blocks.
    pub fn raycast_cursor(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<RaycastHit> {
        let world = self.world.as_ref()?;
        let dim = world.active_dim;
        let hit_a = raycast_world_with_lod(
            world,
            dim,
            Some(&self.lod_raycast),
            origin,
            dir,
            max_dist,
            self.camera.mode,
        );
        if !self.compare.is_active() || !self.compare.config.show_only_b {
            return hit_a;
        }
        let Some(wb) = &self.compare.world_b else {
            return hit_a;
        };
        let hit_b = self.raycast_compare_world_b(wb, dim, origin, dir, max_dist);
        match (hit_a, hit_b) {
            (Some(a), Some(b)) => Some(if b.distance < a.distance { b } else { a }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    fn raycast_compare_world_b(
        &self,
        wb: &CompareWorld,
        dim: DimensionId,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
    ) -> Option<RaycastHit> {
        if dir.length_squared() < 1e-10 {
            return None;
        }
        let dir = dir.normalize();
        let off = self.compare.config.chunk_offset;
        let min_y = wb.min_y;
        let max_y = wb.max_y;
        let cache = wb.chunk_cache.read();

        if self.camera.mode == ViewMode::Chunk2D {
            let wx = origin.x.floor() as i32;
            let wz = origin.z.floor() as i32;
            let cp_b = ChunkPos::new(wx.div_euclid(16) + off.0, wz.div_euclid(16) + off.1);
            if let Some(chunk) = cache.get(&(dim, cp_b)) {
                let lx = wx.rem_euclid(16) as usize;
                let lz = wz.rem_euclid(16) as usize;
                let h = chunk.heightmap[(lz << 4) | lx];
                for y in (min_y..=h.min(max_y - 1)).rev() {
                    if chunk.get_block(lx, y, lz) != BlockStateId::AIR {
                        let pos = IVec3::new(wx, y, wz);
                        return Some(RaycastHit {
                            block_pos: pos,
                            adjacent_pos: pos + IVec3::Y,
                            face: Face::Up,
                            distance: (origin.y - y as f32).abs(),
                        });
                    }
                }
            }
            return None;
        }

        let mut t = 0.0f32;
        if origin.y >= max_y as f32 {
            if dir.y >= -1e-6 {
                return None;
            }
            let dt = (max_y as f32 - 0.001 - origin.y) / dir.y;
            if dt > max_distance {
                return None;
            }
            t = dt.max(0.0);
        } else if origin.y < min_y as f32 {
            if dir.y <= 1e-6 {
                return None;
            }
            let dt = (min_y as f32 + 0.001 - origin.y) / dir.y;
            if dt > max_distance {
                return None;
            }
            t = dt.max(0.0);
        }

        let step_sign = IVec3::new(
            if dir.x >= 0.0 { 1 } else { -1 },
            if dir.y >= 0.0 { 1 } else { -1 },
            if dir.z >= 0.0 { 1 } else { -1 },
        );
        let inv_dir = Vec3::new(
            if dir.x.abs() > 1e-8 { 1.0 / dir.x } else { f32::INFINITY },
            if dir.y.abs() > 1e-8 { 1.0 / dir.y } else { f32::INFINITY },
            if dir.z.abs() > 1e-8 { 1.0 / dir.z } else { f32::INFINITY },
        );

        let mut hit_face = Face::Up;
        let max_steps = (max_distance.min(512.0) * 2.0) as usize;
        for _ in 0..max_steps {
            if t > max_distance {
                break;
            }
            let p = origin + dir * (t + 1e-4);
            let voxel = IVec3::new(
                p.x.floor() as i32,
                p.y.floor() as i32,
                p.z.floor() as i32,
            );
            if (voxel.y < min_y && step_sign.y < 0) || (voxel.y >= max_y && step_sign.y > 0) {
                break;
            }
            let cp_b = ChunkPos::new(
                voxel.x.div_euclid(16) + off.0,
                voxel.z.div_euclid(16) + off.1,
            );
            if let Some(chunk) = cache.get(&(dim, cp_b))
                && voxel.y >= min_y
                && voxel.y < max_y
            {
                let sy = voxel.y.div_euclid(16);
                if let Some(sec) = chunk.sections.get(&sy)
                    && !sec.is_all_air()
                {
                    let b = sec.get_block(
                        voxel.x.rem_euclid(16) as usize,
                        voxel.y.rem_euclid(16) as usize,
                        voxel.z.rem_euclid(16) as usize,
                    );
                    if b != BlockStateId::AIR {
                        return Some(RaycastHit {
                            block_pos: voxel,
                            adjacent_pos: voxel + hit_face.normal(),
                            face: hit_face,
                            distance: t,
                        });
                    }
                }
            }
            let next_x = if step_sign.x > 0 {
                (voxel.x + 1) as f32
            } else {
                voxel.x as f32
            };
            let next_y = if step_sign.y > 0 {
                (voxel.y + 1) as f32
            } else {
                voxel.y as f32
            };
            let next_z = if step_sign.z > 0 {
                (voxel.z + 1) as f32
            } else {
                voxel.z as f32
            };
            let tx = (next_x - origin.x) * inv_dir.x;
            let ty = (next_y - origin.y) * inv_dir.y;
            let tz = (next_z - origin.z) * inv_dir.z;
            if tx <= ty && tx <= tz {
                t = tx.max(t + 1e-4);
                hit_face = if step_sign.x > 0 { Face::West } else { Face::East };
            } else if ty <= tz {
                t = ty.max(t + 1e-4);
                hit_face = if step_sign.y > 0 { Face::Down } else { Face::Up };
            } else {
                t = tz.max(t + 1e-4);
                hit_face = if step_sign.z > 0 { Face::North } else { Face::South };
            }
        }
        None
    }

    pub fn copy_compare_b_to_a_in_selection(&mut self, renderer: &mut WorldRenderer) {
        let Some(box_) = self.selection.box_ else {
            return;
        };
        let Some(format_b) = self.compare.world_b.as_ref().map(|wb| wb.format.clone()) else {
            return;
        };
        let chunk_offset = self.compare.config.chunk_offset;
        let only_differing = self.compare.copy_only_differing;
        let ignore_properties = self.compare.config.ignore_properties;

        let Some(world) = &mut self.world else {
            return;
        };
        let dim = world.active_dim;
        let mut tx = EditTransaction::new("Copy from World B", dim, Some(box_));
        let changed = copy_from_format_in_box(
            world,
            dim,
            format_b.as_ref(),
            box_,
            chunk_offset,
            only_differing,
            ignore_properties,
            &mut tx,
        );
        if changed > 0 {
            tx.commit(world);
            self.invalidate_box(box_, renderer);
            self.set_toast(format!("Copied {changed} blocks from World B (Ctrl+Z to undo)."));
        } else {
            self.set_toast("No differing blocks found in selection.");
        }
    }

    pub fn switch_dimension(&mut self, new_dim: DimensionId, renderer: &mut WorldRenderer) {
        let Some(world) = &mut self.world else {
            return;
        };
        let old_dim = world.active_dim;
        if old_dim == new_dim {
            return;
        }
        match (old_dim, new_dim) {
            (DimensionId::Overworld, DimensionId::Nether) => {
                self.camera.pos.x *= 0.125;
                self.camera.pos.z *= 0.125;
            }
            (DimensionId::Nether, DimensionId::Overworld) => {
                self.camera.pos.x *= 8.0;
                self.camera.pos.z *= 8.0;
            }
            (_, DimensionId::End) => {
                self.camera.pos = Vec3::new(0.0, 96.0, 0.0);
            }
            _ => {}
        }
        world.active_dim = new_dim;
        self.last_dim = Some(new_dim);
        self.invalidate_all_meshes(renderer);
        self.set_toast(format!("Switched to {}", new_dim.display_name()));
    }

    pub fn should_show_left_panel(&self) -> bool {
        self.left_panel_open
            || self.active_tool != ToolId::Select
            || self.selection.box_.is_some()
            || self.selection.corner_a.is_some()
            || self.selection.click_selecting
    }

    pub fn switch_tool(&mut self, tool: ToolId) {
        if tool == ToolId::Import {
            self.open_import_dialog();
            return;
        }
        self.active_tool = tool;
        self.left_panel_open = true;
        self.preview_dirty = true;
    }

    pub fn open_import_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("MCEdit Schematic", &["schematic"])
            .pick_file()
        {
            let res = {
                let mut reg = self.registry.write();
                FormatRegistry::open_schematic(&path, &mut reg)
            };
            match res {
                Ok(vol) => {
                    let label = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Imported")
                        .to_string();
                    self.push_clipboard(label, vol.clone());
                    self.clone.load_volume(
                        vol,
                        self.hover_hit
                            .map(|h| h.adjacent_pos)
                            .unwrap_or_else(|| self.camera.pos.as_ivec3()),
                    );
                    self.active_tool = ToolId::Clone;
                    self.preview_dirty = true;
                    self.set_toast("Schematic loaded into Clone placement.");
                }
                Err(e) => self.set_toast(format!("Import failed: {e}")),
            }
        }
    }

    pub fn push_clipboard(&mut self, label: String, volume: SchematicVolume) {
        self.copy_stack.insert(0, ClipboardEntry { label, volume });
        self.copy_stack.truncate(self.config.max_copies.max(1));
    }

    pub fn save_world_async(&mut self) {
        if self.jobs.is_busy() {
            return;
        }
        let Some(world) = self.world.take() else {
            return;
        };
        self.jobs.spawn_world_job("Saving World", world, move |mut w, prog| {
            prog.set(0, 2, "Relighting modified chunks...");
            let dim = w.active_dim;
            let dirty_relight: Vec<ChunkPos> = w
                .dirty_chunks
                .iter()
                .filter_map(|((d, p), oc)| {
                    if *d == dim && oc.as_ref().is_some_and(|c| c.needs_lighting) {
                        Some(*p)
                    } else {
                        None
                    }
                })
                .collect();
            if !dirty_relight.is_empty() {
                LightingEngine::relight_chunks(&mut w, dim, &dirty_relight);
            }
            prog.set(1, 2, "Writing region files and metadata...");
            let res = w.save_all().map_err(|e| e.to_string());
            JobOutcome::WorldSaved { world: w, result: res }
        });
    }

    pub fn undo(&mut self, renderer: &mut WorldRenderer) {
        let Some(world) = &mut self.world else {
            return;
        };
        if let Some(entry) = world.undo() {
            let label = entry.label.clone();
            if let Some(b) = entry.dirty_box {
                self.invalidate_box(b, renderer);
            } else {
                self.invalidate_all_meshes(renderer);
            }
            self.set_toast(format!("Undid: {label}"));
        }
    }

    pub fn redo(&mut self, renderer: &mut WorldRenderer) {
        let Some(world) = &mut self.world else {
            return;
        };
        if let Some(entry) = world.redo() {
            let label = entry.label.clone();
            if let Some(b) = entry.dirty_box {
                self.invalidate_box(b, renderer);
            } else {
                self.invalidate_all_meshes(renderer);
            }
            self.set_toast(format!("Redid: {label}"));
        }
    }

    pub fn copy_selection(&mut self) {
        let Some(box_) = self.selection.box_ else {
            return;
        };
        let Some(world) = &mut self.world else {
            return;
        };
        let dim = world.active_dim;
        let vol = extract_volume(world, dim, box_);
        let sz = vol.size;
        self.push_clipboard(format!("Copy {}x{}x{}", sz.x, sz.y, sz.z), vol);
        self.set_toast("Copied selection to clipboard.");
    }

    pub fn cut_selection(&mut self, renderer: &mut WorldRenderer) {
        let Some(box_) = self.selection.box_ else {
            return;
        };
        let Some(world) = &mut self.world else {
            return;
        };
        let dim = world.active_dim;
        let vol = extract_volume(world, dim, box_);
        let sz = vol.size;
        let mut tx = EditTransaction::new("Cut Selection", dim, Some(box_));
        mcedit_core::fill_blocks(
            world,
            dim,
            box_,
            mcedit_core::BlockStateId::AIR,
            None,
            false,
            &mut tx,
        );
        tx.commit(world);
        self.push_clipboard(format!("Cut {}x{}x{}", sz.x, sz.y, sz.z), vol);
        self.invalidate_box(box_, renderer);
        self.set_toast("Cut selection to clipboard.");
    }

    pub fn paste_from_clipboard(&mut self) {
        if let Some(top) = self.copy_stack.first().map(|e| e.volume.clone()) {
            let origin = self
                .hover_hit
                .map(|h| h.adjacent_pos)
                .unwrap_or_else(|| self.camera.pos.as_ivec3());
            self.clone.load_volume(top, origin);
            self.active_tool = ToolId::Clone;
            self.preview_dirty = true;
        }
    }

    pub fn delete_selected_blocks(&mut self, renderer: &mut WorldRenderer) {
        let Some(box_) = self.selection.box_ else {
            return;
        };
        let Some(world) = &mut self.world else {
            return;
        };
        let dim = world.active_dim;
        let mut tx = EditTransaction::new("Delete Blocks", dim, Some(box_));
        let count = mcedit_core::fill_blocks(
            world,
            dim,
            box_,
            mcedit_core::BlockStateId::AIR,
            None,
            false,
            &mut tx,
        );
        tx.commit(world);
        self.invalidate_box(box_, renderer);
        self.set_toast(format!("Deleted {count} blocks."));
    }

    pub fn analyze_selection_async(&mut self) {
        let Some(box_) = self.selection.box_ else {
            return;
        };
        let Some(world) = self.world.take() else {
            return;
        };
        self.jobs.spawn_world_job("Analyzing Selection", world, move |mut w, prog| {
            prog.set(1, 2, "Scanning chunks...");
            let dim = w.active_dim;
            let result = analyze_box(&mut w, dim, box_);
            JobOutcome::AnalysisDone { world: w, result }
        });
    }

    pub fn export_selection_schematic(&mut self) {
        let Some(box_) = self.selection.box_ else {
            return;
        };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("MCEdit Schematic", &["schematic"])
            .set_file_name("export.schematic")
            .save_file()
        {
            let Some(world) = &mut self.world else {
                return;
            };
            let dim = world.active_dim;
            let vol = extract_volume(world, dim, box_);
            let res = {
                let reg = self.registry.read();
                FormatRegistry::save_schematic(&path, &vol, &reg)
            };
            match res {
                Ok(()) => self.set_toast(format!("Exported {}", path.display())),
                Err(e) => self.set_toast(format!("Export failed: {e}")),
            }
        }
    }

    pub fn invalidate_box(&mut self, box_: BoundingBox, renderer: &mut WorldRenderer) {
        let expanded = box_.expand(1);
        for cp in expanded.iter_chunks() {
            self.invalidate_chunk(cp, renderer);
        }
    }

    pub fn invalidate_chunk(&mut self, pos: ChunkPos, renderer: &mut WorldRenderer) {
        renderer.invalidate_chunk(pos);
        let rx8 = pos.cx().div_euclid(8);
        let rz8 = pos.cz().div_euclid(8);

        self.lod0_meshed.retain(|(_, p)| *p != pos);
        self.lod0_inflight.retain(|(_, p)| *p != pos);
        self.mesh_chunk_cache.write().retain(|(_, p), _| *p != pos);
        self.lod_regions_meshed
            .retain(|(_, rx, rz), _| !(*rx == rx8 && *rz == rz8));
        self.lod2d_regions_meshed
            .retain(|(_, rx, rz)| !(*rx == rx8 && *rz == rz8));
        self.lod_regions_inflight
            .retain(|(_, k)| !(k.rx8 == rx8 && k.rz8 == rz8));
        self.lod_region_chunks
            .retain(|(_, k), _| !(k.rx8 == rx8 && k.rz8 == rz8));
        self.lod_raycast.remove(&(rx8, rz8));

        if let Some(world) = &self.world {
            let dim = world.active_dim;
            if !world.chunk_exists(dim, pos) {
                self.loaded_chunks_for_grid.remove(&(dim, pos));
            }
        }
        if !self.lod0_priority_queue.contains(&pos) {
            self.lod0_priority_queue.push_front(pos);
        }
        self.lod_tiles_dirty = true;
        self.lod_schedule_dirty = true;
        self.chunk_grid_dirty = true;
    }

    pub fn invalidate_all_meshes(&mut self, renderer: &mut WorldRenderer) {
        self.world_gen = self.world_gen.wrapping_add(1);
        self.active_world_gen
            .store(self.world_gen, Ordering::Relaxed);
        self.urgent_near_active.store(0, Ordering::Relaxed);
        self.mesh_chunk_cache.write().clear();
        mcedit_core::profiler::milestones_reset();
        while self.mesh_rx.try_recv().is_ok() {}
        renderer.clear_all_meshes();
        self.lod0_meshed.clear();
        self.lod0_surface_only.clear();
        self.lod0_inflight.clear();
        self.lod0_priority_queue.clear();
        self.lod_regions_meshed.clear();
        self.lod2d_regions_meshed.clear();
        self.lod_regions_inflight.clear();
        self.lod_region_chunks.clear();
        self.lod_raycast.clear();
        self.loaded_chunks_for_grid.clear();
        self.spiral_cursor = 0;
        self.last_lod0_cam_cp = None;
        self.last_evict_state = None;
        self.last_lod_scan_state = None;
        self.lod_schedule_dirty = true;
        self.lod_tiles_dirty = true;
        self.chunk_grid_dirty = true;
    }

    pub fn poll_background_jobs(&mut self, renderer: &mut WorldRenderer) {
        if let Some(outcome) = self.jobs.poll() {
            match outcome {
                JobOutcome::WorldUpdated {
                    world,
                    status,
                    invalidate_all,
                    dirty_box,
                } => {
                    self.world = Some(world);
                    if invalidate_all {
                        self.invalidate_all_meshes(renderer);
                    } else if let Some(b) = dirty_box {
                        self.invalidate_box(b, renderer);
                    }
                    self.set_toast(status);
                }
                JobOutcome::WorldSaved { world, result } => {
                    self.world = Some(world);
                    match result {
                        Ok(()) => self.set_toast("World saved successfully."),
                        Err(e) => self.set_toast(format!("Save failed: {e}")),
                    }
                }
                JobOutcome::AnalysisDone { world, result } => {
                    self.world = Some(world);
                    if let Some(box_) = self.selection.box_ {
                        self.analysis_modal = Some(AnalysisModal { result, box_ });
                    }
                }
                JobOutcome::Error { world, error } => {
                    if let Some(w) = world {
                        self.world = Some(w);
                    }
                    self.set_toast(format!("Error: {error}"));
                }
            }
        }
    }

    fn ensure_lod0_spiral(&mut self, lod0_r: i32) {
        if self.spiral_lod0_radius != lod0_r {
            self.spiral_lod0_radius = lod0_r;
            let mut offsets = Vec::new();
            if lod0_r > 0 {
                let r2 = lod0_r * lod0_r;
                for dz in -lod0_r..=lod0_r {
                    for dx in -lod0_r..=lod0_r {
                        let d2 = dx * dx + dz * dz;
                        if d2 <= r2 {
                            offsets.push((d2, dx, dz));
                        }
                    }
                }
                offsets.sort_unstable_by_key(|(d2, _, _)| *d2);
            }
            self.spiral_lod0_offsets = offsets.into_iter().map(|(_, dx, dz)| (dx, dz)).collect();
        }
    }

    pub fn request_reschedule(&mut self) {
        self.lod_schedule_dirty = true;
        self.spiral_cursor = 0;
    }

    fn sync_renderer_evictions(&mut self, dim: DimensionId, renderer: &mut WorldRenderer) {
        let (evicted_lod0, evicted_lods) = renderer.take_evictions();
        if evicted_lod0.is_empty() && evicted_lods.is_empty() {
            return;
        }
        for cp in evicted_lod0 {
            self.lod0_surface_only.remove(&(dim, cp));
            if self.lod0_meshed.remove(&(dim, cp)) {
                let rx8 = cp.cx().div_euclid(8);
                let rz8 = cp.cz().div_euclid(8);
                let covered_by_lod = self.lod_regions_meshed.contains_key(&(dim, rx8, rz8))
                    || self.lod2d_regions_meshed.contains(&(dim, rx8, rz8));
                if !covered_by_lod && self.loaded_chunks_for_grid.remove(&(dim, cp)) {
                    self.chunk_grid_dirty = true;
                }
            }
        }
        for key in evicted_lods {
            if key.lod == 3 {
                self.lod2d_regions_meshed.remove(&(dim, key.rx8, key.rz8));
            } else if self.lod_regions_meshed.get(&(dim, key.rx8, key.rz8)) == Some(&key.lod) {
                self.lod_regions_meshed.remove(&(dim, key.rx8, key.rz8));
                self.lod_raycast.remove(&(key.rx8, key.rz8));
            }
            if let Some(chunks) = self.lod_region_chunks.remove(&(dim, key)) {
                for cp in chunks {
                    if !self.lod0_meshed.contains(&(dim, cp))
                        && self.loaded_chunks_for_grid.remove(&(dim, cp))
                    {
                        self.chunk_grid_dirty = true;
                    }
                }
            }
        }
    }

    pub fn pump_chunk_meshing(&mut self, device: &wgpu::Device, renderer: &mut WorldRenderer) {
        let cam_cp = ChunkPos::new(
            (self.camera.pos.x / 16.0).floor() as i32,
            (self.camera.pos.z / 16.0).floor() as i32,
        );
        renderer.set_camera_chunk(cam_cp);
        renderer.diff_params = self.compare.config.shader_params();

        // 1. Drain completed async meshes from rayon with a 4ms frame time budget so
        //    bursts of nearby 8x8 chunks appear on screen immediately in a single frame.
        let upload_start = Instant::now();
        let mut uploaded_any = false;
        let cmp_active = self.compare.is_active();
        let cmp_off = self.compare.config.chunk_offset;
        while upload_start.elapsed() < Duration::from_millis(4) {
            let Ok(result) = self.mesh_rx.try_recv() else {
                break;
            };
            match result {
                MeshTaskResult::Lod0Chunk {
                    world_gen,
                    dim,
                    pos,
                    mesh,
                    loaded_clean,
                } => {
                    if world_gen != self.world_gen {
                        continue;
                    }
                    uploaded_any = true;
                    self.lod0_inflight.remove(&(dim, pos));
                    let exists_in_b = cmp_active
                        && self.compare.world_b.as_ref().is_some_and(|wb| {
                            wb.chunk_exists(
                                dim,
                                ChunkPos::new(pos.cx() + cmp_off.0, pos.cz() + cmp_off.1),
                            )
                        });
                    if let Some(world) = &mut self.world
                        && world.active_dim == dim
                        && (world.chunk_exists(dim, pos) || exists_in_b)
                    {
                        if let Some(clean_chunk) = loaded_clean {
                            world.insert_loaded_clean_chunk(dim, *clean_chunk);
                        }
                        renderer.upload_section(
                            device,
                            SectionMeshKey {
                                chunk: pos,
                                section_y: i32::MIN,
                                lod: 0,
                            },
                            mesh,
                        );
                        self.lod0_meshed.insert((dim, pos));
                        if self.loaded_chunks_for_grid.insert((dim, pos)) {
                            self.chunk_grid_dirty = true;
                        }
                    }
                }
                MeshTaskResult::Lod0Aborted {
                    world_gen,
                    dim,
                    pos,
                } => {
                    if world_gen != self.world_gen {
                        continue;
                    }
                    self.lod0_inflight.remove(&(dim, pos));
                }
                MeshTaskResult::LodRegion {
                    world_gen,
                    dim,
                    key,
                    mesh,
                    occupancy,
                    covered_chunks,
                } => {
                    if world_gen != self.world_gen {
                        continue;
                    }
                    uploaded_any = true;
                    self.lod_regions_inflight.remove(&(dim, key));
                    if self.world.as_ref().is_some_and(|w| w.active_dim == dim) {
                        if key.lod == 3 {
                            self.lod2d_regions_meshed.insert((dim, key.rx8, key.rz8));
                        } else {
                            if let Some(&old_lod) =
                                self.lod_regions_meshed.get(&(dim, key.rx8, key.rz8))
                                && old_lod != key.lod
                            {
                                let old_key = LodRegionKey {
                                    rx8: key.rx8,
                                    rz8: key.rz8,
                                    lod: old_lod,
                                };
                                renderer.remove_lod_region(&old_key);
                                self.lod_region_chunks.remove(&(dim, old_key));
                            }
                            self.lod_regions_meshed
                                .insert((dim, key.rx8, key.rz8), key.lod);
                            if let Some(occ) = occupancy {
                                self.lod_raycast.insert((key.rx8, key.rz8), occ);
                            }
                        }
                        renderer.upload_lod_region(device, key, &mesh);
                        for &cp in &covered_chunks {
                            if self.loaded_chunks_for_grid.insert((dim, cp)) {
                                self.chunk_grid_dirty = true;
                            }
                        }
                        self.lod_region_chunks.insert((dim, key), covered_chunks);
                    }
                }
            }
        }

        // 2. Check active world & dimension transitions
        let vd = self.config.view_distance.clamp(2, 1024);
        if (cam_cp.cx() - self.grid_center_cp.cx())
            .abs()
            .max((cam_cp.cz() - self.grid_center_cp.cz()).abs())
            >= 2
            || self.last_grid_vd != vd
        {
            self.grid_center_cp = cam_cp;
            self.last_grid_vd = vd;
            self.chunk_grid_dirty = true;
        }

        // While a background job owns `self.world`, keep existing GPU meshes and chunk grid intact.
        if self.jobs.is_busy() {
            return;
        }

        let Some(dim) = self.world.as_ref().map(|w| w.active_dim) else {
            if self.chunk_grid_dirty {
                renderer.set_chunk_grid(device, None);
                self.chunk_grid_dirty = false;
            }
            return;
        };
        if self.last_dim != Some(dim) {
            self.last_dim = Some(dim);
            self.invalidate_all_meshes(renderer);
        }
        self.sync_renderer_evictions(dim, renderer);

        let cam_r8 = (cam_cp.cx().div_euclid(8), cam_cp.cz().div_euclid(8));
        let detail_r = self.config.detail_distance.clamp(0, 128).min(vd);
        let reg_r = (vd + 7) / 8 + 1;
        let is_2d = self.camera.mode == ViewMode::Chunk2D;

        let detail_changed = self.last_detail_distance != detail_r;
        self.last_detail_distance = detail_r;
        self.ensure_lod0_spiral(detail_r);
        if detail_changed || self.last_lod0_cam_cp != Some(cam_cp) {
            self.spiral_cursor = 0;
            self.last_lod0_cam_cp = Some(cam_cp);
        }

        if self.lod_tiles_dirty
            && let Some(world) = &self.world
        {
            self.lod_tiles_dirty = false;
            self.lod_schedule_dirty = true;
            let mut tile_set = AHashSet::new();
            for cp in world.list_chunks(dim) {
                tile_set.insert((cp.cx().div_euclid(8), cp.cz().div_euclid(8)));
            }
            if self.compare.is_active()
                && let Some(wb) = &self.compare.world_b
                && let Some(b_chunks) = wb.known_chunks.get(&dim)
            {
                let off = self.compare.config.chunk_offset;
                for &cp_b in b_chunks {
                    let cp_a = ChunkPos::new(cp_b.cx() - off.0, cp_b.cz() - off.1);
                    tile_set.insert((cp_a.cx().div_euclid(8), cp_a.cz().div_euclid(8)));
                }
            }
            self.active_lod_tiles = tile_set.into_iter().collect();
        }

        // 3. Evict out-of-range LOD 0 chunks and far LOD super-regions ONLY when camera chunk or distance config changes
        let evict_state = (dim, cam_cp, detail_r, vd, is_2d);
        if self.last_evict_state != Some(evict_state) {
            self.last_evict_state = Some(evict_state);

            if !is_2d {
                let to_evict_lod0: Vec<ChunkPos> = self
                    .lod0_meshed
                    .iter()
                    .filter_map(|&(d, cp)| {
                        if d != dim {
                            return None;
                        }
                        let dx = cp.cx() - cam_cp.cx();
                        let dz = cp.cz() - cam_cp.cz();
                        let out_of_range = if detail_r == 0 {
                            true
                        } else if detail_changed {
                            dx * dx + dz * dz > detail_r * detail_r
                        } else {
                            dx.abs().max(dz.abs()) > detail_r + 2
                        };
                        if out_of_range { Some(cp) } else { None }
                    })
                    .collect();
                if !to_evict_lod0.is_empty() {
                    for cp in to_evict_lod0 {
                        renderer.remove_chunk_lod0_only(cp);
                        self.lod0_meshed.remove(&(dim, cp));
                        self.lod0_surface_only.remove(&(dim, cp));
                    }
                    self.lod_schedule_dirty = true;
                }
            }

            let reg_evict_r = reg_r + 2;
            let to_evict_regs: Vec<LodRegionKey> = self
                .lod_regions_meshed
                .iter()
                .filter_map(|(&(d, rx8, rz8), &lod)| {
                    if d == dim
                        && (rx8 - cam_r8.0).abs().max((rz8 - cam_r8.1).abs()) > reg_evict_r
                    {
                        Some(LodRegionKey { rx8, rz8, lod })
                    } else {
                        None
                    }
                })
                .collect();
            if !to_evict_regs.is_empty() {
                for key in to_evict_regs {
                    renderer.remove_lod_region(&key);
                    self.lod_regions_meshed.remove(&(dim, key.rx8, key.rz8));
                    self.lod_raycast.remove(&(key.rx8, key.rz8));
                    if let Some(chunks) = self.lod_region_chunks.remove(&(dim, key)) {
                        for cp in chunks {
                            if !self.lod0_meshed.contains(&(dim, cp)) {
                                self.loaded_chunks_for_grid.remove(&(dim, cp));
                                self.chunk_grid_dirty = true;
                            }
                        }
                    }
                }
            }
        }

        let Some(world) = &self.world else {
            return;
        };
        let compare_ctx = if self.compare.is_active() {
            self.compare
                .world_b
                .as_ref()
                .map(|wb| (wb.format.clone(), wb.chunk_cache.clone(), self.compare.config))
        } else {
            None
        };
        let compare_wb_ref = if self.compare.is_active() {
            self.compare.world_b.as_ref()
        } else {
            None
        };
        let cmp_offset = self.compare.config.chunk_offset;
        let chunk_exists_union = |cp: ChunkPos| -> bool {
            world.chunk_exists(dim, cp)
                || compare_wb_ref.is_some_and(|wb| {
                    wb.chunk_exists(
                        dim,
                        ChunkPos::new(cp.cx() + cmp_offset.0, cp.cz() + cmp_offset.1),
                    )
                })
        };

        // 4. Schedule LOD 0 full-3D chunks completely off the main thread (when detail_r > 0).
        //    Tier 1: Immediate 8x8 / 9x9 area around the player (`dx, dz in -4..=4`) + edited priority chunks
        //            ALWAYS load first in small 8-chunk concentric rings and preempt any outer-ring work.
        //    Tier 2: Outer spiral (`d2 > 16..detail_r^2`) streams outward once the immediate 8x8 area is 100% loaded.
        let mut near_area_pending = false;
        if !is_2d && detail_r > 0 {
            // Upgrade any distant surface-only chunks to full underground meshing when
            // the player flies within 16 chunks of them or descends below Y=40.
            if !self.lod0_surface_only.is_empty() {
                let cam_underground = self.camera.pos.y <= 40.0;
                let to_upgrade: Vec<ChunkPos> = self
                    .lod0_surface_only
                    .iter()
                    .filter_map(|&(d, cp)| {
                        if d != dim {
                            return None;
                        }
                        if cam_underground {
                            return Some(cp);
                        }
                        let dx = cp.cx() - cam_cp.cx();
                        let dz = cp.cz() - cam_cp.cz();
                        if dx * dx + dz * dz <= 16 * 16 {
                            Some(cp)
                        } else {
                            None
                        }
                    })
                    .collect();
                for cp in to_upgrade {
                    self.lod0_surface_only.remove(&(dim, cp));
                    self.lod0_meshed.remove(&(dim, cp));
                    self.lod0_priority_queue.push_back(cp);
                }
            }

            let mut urgent_targets: Vec<ChunkPos> = Vec::new();
            while let Some(cp) = self.lod0_priority_queue.pop_front() {
                if chunk_exists_union(cp)
                    && !self.lod0_meshed.contains(&(dim, cp))
                    && !self.lod0_inflight.contains(&(dim, cp))
                    && !urgent_targets.contains(&cp)
                {
                    urgent_targets.push(cp);
                }
            }

            // Always check the immediate 8x8 / 9x9 area (`dx.abs() <= 4 && dz.abs() <= 4`) in radial order
            for &(dx, dz) in &self.spiral_lod0_offsets {
                if dx * dx + dz * dz > 32 {
                    break;
                }
                if dx.abs() <= 4 && dz.abs() <= 4 {
                    let cp = ChunkPos::new(cam_cp.cx() + dx, cam_cp.cz() + dz);
                    if chunk_exists_union(cp) && !self.lod0_meshed.contains(&(dim, cp)) {
                        near_area_pending = true;
                        if !self.lod0_inflight.contains(&(dim, cp)) && !urgent_targets.contains(&cp)
                        {
                            urgent_targets.push(cp);
                        }
                    }
                }
            }

            {
                use mcedit_core::profiler::{Milestone, milestone_mark};
                if !self.lod0_meshed.is_empty() {
                    milestone_mark(Milestone::FirstChunk);
                    if !near_area_pending {
                        milestone_mark(Milestone::Near8x8);
                        if self.spiral_cursor >= self.spiral_lod0_offsets.len()
                            && self.lod0_inflight.is_empty()
                        {
                            milestone_mark(Milestone::FullDetail);
                        }
                    }
                }
            }

            let mut batches_to_spawn: Vec<(Vec<ChunkPos>, bool)> = Vec::new();
            if !urgent_targets.is_empty() {
                for sub in urgent_targets.chunks(8) {
                    batches_to_spawn.push((sub.to_vec(), true));
                }
            } else if !near_area_pending && self.lod0_inflight.len() < 96 {
                // If VRAM is near its limit and no LOD regions remain to evict, only schedule
                // outer LOD 0 chunks that are strictly closer than the furthest loaded LOD 0 chunk.
                let lod0_vram_cap_d2 =
                    if renderer.is_vram_near_limit() && !renderer.has_lod_regions() {
                        renderer.furthest_lod0_dist_sq()
                    } else {
                        None
                    };
                let max_new = (96 - self.lod0_inflight.len()).min(32);
                let mut outer_targets: Vec<ChunkPos> = Vec::with_capacity(max_new);
                let mut scanned_this_frame = 0usize;
                while outer_targets.len() < max_new
                    && self.spiral_cursor < self.spiral_lod0_offsets.len()
                    && scanned_this_frame < 8192
                {
                    let (dx, dz) = self.spiral_lod0_offsets[self.spiral_cursor];
                    if let Some(max_d2) = lod0_vram_cap_d2 {
                        let cand_d2 = (dx as i64) * (dx as i64) + (dz as i64) * (dz as i64);
                        if cand_d2 + 16 >= max_d2 {
                            break;
                        }
                    }
                    self.spiral_cursor += 1;
                    scanned_this_frame += 1;
                    let cp = ChunkPos::new(cam_cp.cx() + dx, cam_cp.cz() + dz);
                    if chunk_exists_union(cp)
                        && !self.lod0_meshed.contains(&(dim, cp))
                        && !self.lod0_inflight.contains(&(dim, cp))
                    {
                        outer_targets.push(cp);
                    }
                }
                for sub in outer_targets.chunks(16) {
                    batches_to_spawn.push((sub.to_vec(), false));
                }
            }

            for (batch_targets, is_urgent) in batches_to_spawn {
                let mut dirty_overrides: AHashMap<ChunkPos, Option<Arc<Chunk>>> = AHashMap::new();
                for &cp in &batch_targets {
                    self.lod0_inflight.insert((dim, cp));
                    for neighbor in [
                        cp,
                        ChunkPos::new(cp.cx() + 1, cp.cz()),
                        ChunkPos::new(cp.cx() - 1, cp.cz()),
                        ChunkPos::new(cp.cx(), cp.cz() + 1),
                        ChunkPos::new(cp.cx(), cp.cz() - 1),
                    ] {
                        if let Some(dirty_opt) = world.dirty_chunks.get(&(dim, neighbor)) {
                            dirty_overrides
                                .entry(neighbor)
                                .or_insert_with(|| dirty_opt.as_ref().cloned().map(Arc::new));
                        }
                    }
                }

                if is_urgent {
                    self.urgent_near_active
                        .fetch_add(batch_targets.len(), Ordering::Relaxed);
                }

                let format_arc = world.format.clone();
                let reg_arc = self.registry.clone();
                let cache_arc = self.mesh_chunk_cache.clone();
                let tx = self.mesh_tx.clone();
                let task_gen = self.world_gen;
                let active_gen = self.active_world_gen.clone();
                let urgent_near = self.urgent_near_active.clone();
                let cmp_batch = compare_ctx.clone();
                let mesh_opts = MeshOptions {
                    fast_leaves: self.config.fast_leaves,
                    rough_graphics: self.config.rough_graphics,
                    show_hidden_ores: self.config.show_hidden_ores,
                    hide_barriers: !self.config.show_barriers,
                    cull_underground: false,
                };
                let cam_above_ground = self.camera.pos.y > 40.0;
                let batch_cull_flags: Vec<(ChunkPos, bool)> = batch_targets
                    .iter()
                    .map(|&cp| {
                        let dx = cp.cx() - cam_cp.cx();
                        let dz = cp.cz() - cam_cp.cz();
                        let cull_ug = cmp_batch.is_none()
                            && !is_urgent
                            && dim == DimensionId::Overworld
                            && cam_above_ground
                            && !self.config.show_hidden_ores
                            && dx * dx + dz * dz > 16 * 16;
                        if cull_ug {
                            self.lod0_surface_only.insert((dim, cp));
                        } else {
                            self.lod0_surface_only.remove(&(dim, cp));
                        }
                        (cp, cull_ug)
                    })
                    .collect();

                rayon::spawn(move || {
                    let abort_batch = |targets: &[ChunkPos]| {
                        if is_urgent {
                            urgent_near.fetch_sub(targets.len(), Ordering::Relaxed);
                        }
                        if active_gen.load(Ordering::Relaxed) == task_gen {
                            for &cp in targets {
                                let _ = tx.send(MeshTaskResult::Lod0Aborted {
                                    world_gen: task_gen,
                                    dim,
                                    pos: cp,
                                });
                            }
                        }
                    };

                    if active_gen.load(Ordering::Relaxed) != task_gen
                        || (!is_urgent && urgent_near.load(Ordering::Relaxed) > 0)
                    {
                        abort_batch(&batch_targets);
                        return;
                    }

                    // Collect unique chunks (targets + 4 cardinal neighbors) that are neither
                    // in `dirty_overrides` nor already cached in `mesh_chunk_cache`.
                    let mut needed: Vec<ChunkPos> = Vec::with_capacity(batch_targets.len() * 5);
                    {
                        let cache_r = cache_arc.read();
                        for &cp in &batch_targets {
                            for neighbor in [
                                cp,
                                ChunkPos::new(cp.cx() + 1, cp.cz()),
                                ChunkPos::new(cp.cx() - 1, cp.cz()),
                                ChunkPos::new(cp.cx(), cp.cz() + 1),
                                ChunkPos::new(cp.cx(), cp.cz() - 1),
                            ] {
                                if !dirty_overrides.contains_key(&neighbor)
                                    && !cache_r.contains_key(&(dim, neighbor))
                                    && !needed.contains(&neighbor)
                                {
                                    needed.push(neighbor);
                                }
                            }
                        }
                    }

                    if !needed.is_empty() {
                        let use_exact_reg = cmp_batch.is_some();
                        let loaded_from_disk: Vec<(ChunkPos, Arc<Chunk>)> = needed
                            .into_par_iter()
                            .filter_map(|p| {
                                if active_gen.load(Ordering::Relaxed) != task_gen
                                    || (!is_urgent && urgent_near.load(Ordering::Relaxed) > 0)
                                {
                                    return None;
                                }
                                if let Some(existing) = cache_arc.read().get(&(dim, p)).cloned() {
                                    return Some((p, existing));
                                }
                                if use_exact_reg {
                                    format_arc
                                        .load_chunk_with_registry(dim, p, &reg_arc)
                                        .ok()
                                        .flatten()
                                        .map(|c| (p, Arc::new(c)))
                                } else {
                                    let reg = reg_arc.read();
                                    format_arc
                                        .load_chunk_readonly(dim, p, &reg)
                                        .ok()
                                        .flatten()
                                        .map(|c| (p, Arc::new(c)))
                                }
                            })
                            .collect();

                        if !loaded_from_disk.is_empty() {
                            let mut cache_w = cache_arc.write();
                            if cache_w.len() > 8192 {
                                cache_w.clear();
                            }
                            for (p, arc_c) in loaded_from_disk {
                                cache_w.insert((dim, p), arc_c);
                            }
                        }
                    }

                    // If Compare mode is active, load World B's matching chunks + 4 neighbors in parallel
                    if let Some((fmt_b, cache_b, cmp_cfg)) = &cmp_batch {
                        let off = cmp_cfg.chunk_offset;
                        let mut needed_b: Vec<ChunkPos> = Vec::with_capacity(batch_targets.len() * 5);
                        {
                            let cb_r = cache_b.read();
                            for &cp in &batch_targets {
                                for nb_a in [
                                    cp,
                                    ChunkPos::new(cp.cx() + 1, cp.cz()),
                                    ChunkPos::new(cp.cx() - 1, cp.cz()),
                                    ChunkPos::new(cp.cx(), cp.cz() + 1),
                                    ChunkPos::new(cp.cx(), cp.cz() - 1),
                                ] {
                                    let p_b = ChunkPos::new(nb_a.cx() + off.0, nb_a.cz() + off.1);
                                    if !cb_r.contains_key(&(dim, p_b)) && !needed_b.contains(&p_b) {
                                        needed_b.push(p_b);
                                    }
                                }
                            }
                        }
                        if !needed_b.is_empty() {
                            let loaded_b: Vec<(ChunkPos, Arc<Chunk>)> = needed_b
                                .into_par_iter()
                                .filter_map(|p_b| {
                                    if active_gen.load(Ordering::Relaxed) != task_gen
                                        || (!is_urgent && urgent_near.load(Ordering::Relaxed) > 0)
                                    {
                                        return None;
                                    }
                                    if let Some(existing) = cache_b.read().get(&(dim, p_b)).cloned()
                                    {
                                        return Some((p_b, existing));
                                    }
                                    fmt_b
                                        .load_chunk_with_registry(dim, p_b, &reg_arc)
                                        .ok()
                                        .flatten()
                                        .map(|c| (p_b, Arc::new(c)))
                                })
                                .collect();
                            if !loaded_b.is_empty() {
                                let mut cb_w = cache_b.write();
                                if cb_w.len() > 8192 {
                                    cb_w.clear();
                                }
                                for (p_b, arc_c) in loaded_b {
                                    cb_w.insert((dim, p_b), arc_c);
                                }
                            }
                        }
                    }

                    if active_gen.load(Ordering::Relaxed) != task_gen
                        || (!is_urgent && urgent_near.load(Ordering::Relaxed) > 0)
                    {
                        abort_batch(&batch_targets);
                        return;
                    }

                    batch_cull_flags.into_par_iter().for_each(|(cp, cull_ug)| {
                        if active_gen.load(Ordering::Relaxed) != task_gen {
                            if is_urgent {
                                urgent_near.fetch_sub(1, Ordering::Relaxed);
                            }
                            return;
                        }
                        if !is_urgent && urgent_near.load(Ordering::Relaxed) > 0 {
                            let _ = tx.send(MeshTaskResult::Lod0Aborted {
                                world_gen: task_gen,
                                dim,
                                pos: cp,
                            });
                            return;
                        }

                        let fetch_a = |p: ChunkPos| -> Option<Arc<Chunk>> {
                            if let Some(dirty_opt) = dirty_overrides.get(&p) {
                                return dirty_opt.clone();
                            }
                            cache_arc.read().get(&(dim, p)).cloned()
                        };

                        if let Some((_, cache_b, cmp_cfg)) = &cmp_batch {
                            let off = cmp_cfg.chunk_offset;
                            let fetch_b = |p_a: ChunkPos| -> Option<Arc<Chunk>> {
                                let p_b = ChunkPos::new(p_a.cx() + off.0, p_a.cz() + off.1);
                                cache_b.read().get(&(dim, p_b)).cloned()
                            };
                            let center_a = fetch_a(cp);
                            let center_b = fetch_b(cp);
                            if center_a.is_none() && center_b.is_none() {
                                if is_urgent {
                                    urgent_near.fetch_sub(1, Ordering::Relaxed);
                                }
                                let _ = tx.send(MeshTaskResult::Lod0Chunk {
                                    world_gen: task_gen,
                                    dim,
                                    pos: cp,
                                    mesh: SectionMeshData::default(),
                                    loaded_clean: None,
                                });
                                return;
                            }
                            let loaded_clean = if !dirty_overrides.contains_key(&cp) {
                                center_a.as_ref().map(|c| Box::new((**c).clone()))
                            } else {
                                None
                            };
                            let snap_a = center_a.map(|center| ChunkNeighborSnapshot {
                                center,
                                east: fetch_a(ChunkPos::new(cp.cx() + 1, cp.cz())),
                                west: fetch_a(ChunkPos::new(cp.cx() - 1, cp.cz())),
                                south: fetch_a(ChunkPos::new(cp.cx(), cp.cz() + 1)),
                                north: fetch_a(ChunkPos::new(cp.cx(), cp.cz() - 1)),
                            });
                            let snap_b = center_b.map(|center| ChunkNeighborSnapshot {
                                center,
                                east: fetch_b(ChunkPos::new(cp.cx() + 1, cp.cz())),
                                west: fetch_b(ChunkPos::new(cp.cx() - 1, cp.cz())),
                                south: fetch_b(ChunkPos::new(cp.cx(), cp.cz() + 1)),
                                north: fetch_b(ChunkPos::new(cp.cx(), cp.cz() - 1)),
                            });
                            let mut opts = mesh_opts;
                            opts.cull_underground = cull_ug;
                            let mesh = {
                                let reg = reg_arc.read();
                                mesh_chunk_compare(
                                    cp,
                                    snap_a.as_ref(),
                                    snap_b.as_ref(),
                                    &reg,
                                    opts,
                                    *cmp_cfg,
                                )
                            };
                            if is_urgent {
                                urgent_near.fetch_sub(1, Ordering::Relaxed);
                            }
                            let _ = tx.send(MeshTaskResult::Lod0Chunk {
                                world_gen: task_gen,
                                dim,
                                pos: cp,
                                mesh,
                                loaded_clean,
                            });
                            return;
                        }

                        let Some(center) = fetch_a(cp) else {
                            if is_urgent {
                                urgent_near.fetch_sub(1, Ordering::Relaxed);
                            }
                            let _ = tx.send(MeshTaskResult::Lod0Chunk {
                                world_gen: task_gen,
                                dim,
                                pos: cp,
                                mesh: SectionMeshData::default(),
                                loaded_clean: None,
                            });
                            return;
                        };
                        let east = fetch_a(ChunkPos::new(cp.cx() + 1, cp.cz()));
                        let west = fetch_a(ChunkPos::new(cp.cx() - 1, cp.cz()));
                        let south = fetch_a(ChunkPos::new(cp.cx(), cp.cz() + 1));
                        let north = fetch_a(ChunkPos::new(cp.cx(), cp.cz() - 1));

                        let loaded_clean = if !dirty_overrides.contains_key(&cp) {
                            Some(Box::new((*center).clone()))
                        } else {
                            None
                        };
                        let snap = ChunkNeighborSnapshot {
                            center,
                            east,
                            west,
                            south,
                            north,
                        };
                        let mut opts = mesh_opts;
                        opts.cull_underground = cull_ug;
                        let mesh = {
                            let reg = reg_arc.read();
                            mesh_chunk_full(&snap, &reg, opts)
                        };
                        if is_urgent {
                            urgent_near.fetch_sub(1, Ordering::Relaxed);
                        }
                        let _ = tx.send(MeshTaskResult::Lod0Chunk {
                            world_gen: task_gen,
                            dim,
                            pos: cp,
                            mesh,
                            loaded_clean,
                        });
                    });
                });
            }
        }

        // 5. Schedule true-3D 8x8-chunk LOD super-regions across active_lod_tiles (supports VD up to 1024 in O(non_empty_tiles)!)
        //    In 3D mode, never schedule LOD 1/2 tasks while the immediate 8x8 area around the player (`near_area_pending`)
        //    is still loading, so 100% of CPU threads finish the player's surroundings first!
        let lod_scan_state = (dim, cam_r8, detail_r, vd, is_2d);
        if self.last_lod_scan_state != Some(lod_scan_state) {
            self.last_lod_scan_state = Some(lod_scan_state);
            self.lod_schedule_dirty = true;
        }
        if self.lod_schedule_dirty
            && (is_2d || !near_area_pending)
            && self.lod_regions_inflight.len() < 16
            && !self.active_lod_tiles.is_empty()
        {
            let min_y = world.min_y();
            let max_y = world.max_y();
            let lod1_limit = (detail_r + 32).max(40);
            let detail_r2 = detail_r * detail_r;
            let vram_near_limit = renderer.is_vram_near_limit();
            let furthest_lod_d2 = if vram_near_limit {
                renderer.furthest_lod_dist_sq(is_2d)
            } else {
                None
            };
            // If VRAM is already full of LOD 0 chunks (no LOD regions left), skip scheduling
            // LOD regions until VRAM frees up or the camera moves.
            let vram_full_without_lods = vram_near_limit && furthest_lod_d2.is_none();

            let mut candidates: Vec<(i32, i32, i32, u8, u8)> = Vec::new();
            if !vram_full_without_lods {
                for &(rx8, rz8) in &self.active_lod_tiles {
                    let center_cx = rx8 * 8 + 4;
                    let center_cz = rz8 * 8 + 4;
                    let dx_i64 = (center_cx - cam_cp.cx()) as i64;
                    let dz_i64 = (center_cz - cam_cp.cz()) as i64;
                    let cand_d2 = dx_i64 * dx_i64 + dz_i64 * dz_i64;
                    // When VRAM is near its limit, only schedule tiles strictly closer than
                    // the furthest currently loaded LOD tile (with ~16-chunk hysteresis) so we
                    // never thrash by evicting and re-scheduling the same horizon tiles.
                    if let Some(max_d2) = furthest_lod_d2
                        && cand_d2 + 256 >= max_d2
                    {
                        continue;
                    }
                    let dist_chunks = ((dx_i64 as f32).hypot(dz_i64 as f32)) as i32;
                    if !is_2d && dist_chunks > vd + 8 {
                        continue;
                    }
                    // In 3D mode, if the furthest corner of this 8x8 tile is strictly within detail_r,
                    // all chunks in this tile are inside the LOD 0 detail radius — never schedule redundant LOD 1/2!
                    if !is_2d && detail_r > 0 {
                        let max_dx = (rx8 * 8 - cam_cp.cx())
                            .abs()
                            .max((rx8 * 8 + 7 - cam_cp.cx()).abs());
                        let max_dz = (rz8 * 8 - cam_cp.cz())
                            .abs()
                            .max((rz8 * 8 + 7 - cam_cp.cz()).abs());
                        if max_dx * max_dx + max_dz * max_dz <= detail_r2 {
                            continue;
                        }
                    }
                    let (desired_lod, step) = if is_2d {
                        let s = if dist_chunks > 64 { 2u8 } else { 1u8 };
                        (3u8, s)
                    } else if dist_chunks <= lod1_limit {
                        (1u8, 2u8)
                    } else {
                        (2u8, 4u8)
                    };
                    let key = LodRegionKey {
                        rx8,
                        rz8,
                        lod: desired_lod,
                    };
                    let already_done = if is_2d {
                        self.lod2d_regions_meshed.contains(&(dim, rx8, rz8))
                    } else {
                        self.lod_regions_meshed.get(&(dim, rx8, rz8)) == Some(&desired_lod)
                    };
                    if !already_done && !self.lod_regions_inflight.contains(&(dim, key)) {
                        candidates.push((dist_chunks, rx8, rz8, desired_lod, step));
                    }
                }
            }
            candidates.sort_unstable_by_key(|c| c.0);
            let total_candidates = candidates.len();

            let mut dispatched = 0usize;
            for (_, rx8, rz8, desired_lod, step) in candidates {
                if dispatched >= 4 || self.lod_regions_inflight.len() >= 16 {
                    break;
                }
                let key = LodRegionKey {
                    rx8,
                    rz8,
                    lod: desired_lod,
                };

                let mut tile_chunks: Vec<(ChunkPos, Option<Arc<Chunk>>, bool, bool)> = Vec::new();
                for lz in 0..8 {
                    for lx in 0..8 {
                        let cp = ChunkPos::new(rx8 * 8 + lx, rz8 * 8 + lz);
                        let in_a = world.chunk_exists(dim, cp);
                        let in_b = compare_wb_ref.is_some_and(|wb| {
                            wb.chunk_exists(
                                dim,
                                ChunkPos::new(cp.cx() + cmp_offset.0, cp.cz() + cmp_offset.1),
                            )
                        });
                        if in_a || in_b {
                            let override_chunk = if in_a {
                                world
                                    .dirty_chunks
                                    .get(&(dim, cp))
                                    .and_then(|o| o.as_ref().cloned().map(Arc::new))
                            } else {
                                None
                            };
                            tile_chunks.push((cp, override_chunk, in_a, in_b));
                        }
                    }
                }

                if tile_chunks.is_empty() {
                    if is_2d {
                        self.lod2d_regions_meshed.insert((dim, rx8, rz8));
                    } else {
                        self.lod_regions_meshed.insert((dim, rx8, rz8), desired_lod);
                    }
                    continue;
                }

                self.lod_regions_inflight.insert((dim, key));
                dispatched += 1;

                let format_arc = world.format.clone();
                let reg_arc = self.registry.clone();
                let cache_arc = self.mesh_chunk_cache.clone();
                let palette_arc = self.lod_colors.clone();
                let tx = self.mesh_tx.clone();
                let task_gen = self.world_gen;
                let active_gen = self.active_world_gen.clone();
                let cmp_lod = compare_ctx.clone();
                let tile_min_y = compare_wb_ref.map_or(min_y, |wb| min_y.min(wb.min_y));
                let tile_max_y = compare_wb_ref.map_or(max_y, |wb| max_y.max(wb.max_y));

                if is_2d {
                    rayon::spawn(move || {
                        if active_gen.load(Ordering::Relaxed) != task_gen {
                            return;
                        }
                        if let Some((fmt_b, cache_b, cmp_cfg)) = cmp_lod {
                            let off = cmp_cfg.chunk_offset;
                            let pairs: Vec<(
                                ChunkPos,
                                Option<LodChunkSurface>,
                                Option<LodChunkSurface>,
                            )> = tile_chunks
                                .into_par_iter()
                                .filter_map(|(cp, override_c, in_a, in_b)| {
                                    if active_gen.load(Ordering::Relaxed) != task_gen {
                                        return None;
                                    }
                                    let surf_a = if in_a {
                                        let cached = override_c
                                            .or_else(|| cache_arc.read().get(&(dim, cp)).cloned());
                                        let c = cached.or_else(|| {
                                            format_arc
                                                .load_chunk_with_registry(dim, cp, &reg_arc)
                                                .ok()
                                                .flatten()
                                                .map(Arc::new)
                                        });
                                        c.map(|c| LodChunkSurface::from_chunk(&c, &reg_arc.read()))
                                    } else {
                                        None
                                    };
                                    let surf_b = if in_b {
                                        let cp_b = ChunkPos::new(cp.cx() + off.0, cp.cz() + off.1);
                                        let cached = cache_b.read().get(&(dim, cp_b)).cloned();
                                        let c = cached.or_else(|| {
                                            fmt_b
                                                .load_chunk_with_registry(dim, cp_b, &reg_arc)
                                                .ok()
                                                .flatten()
                                                .map(Arc::new)
                                        });
                                        c.map(|c| {
                                            let mut s =
                                                LodChunkSurface::from_chunk(&c, &reg_arc.read());
                                            s.pos = cp;
                                            s
                                        })
                                    } else {
                                        None
                                    };
                                    if surf_a.is_some() || surf_b.is_some() {
                                        Some((cp, surf_a, surf_b))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            if active_gen.load(Ordering::Relaxed) != task_gen {
                                return;
                            }
                            let mut surfs_a = Vec::new();
                            let mut surfs_b = Vec::new();
                            let mut covered_chunks = Vec::with_capacity(pairs.len());
                            for (cp, sa, sb) in pairs {
                                covered_chunks.push(cp);
                                if let Some(s) = sa {
                                    surfs_a.push(s);
                                }
                                if let Some(s) = sb {
                                    surfs_b.push(s);
                                }
                            }
                            let reg = reg_arc.read();
                            let mesh = mesh_lod_overhead_tile_compare(
                                rx8,
                                rz8,
                                &surfs_a,
                                &surfs_b,
                                step as usize,
                                &palette_arc,
                                tile_min_y,
                                tile_max_y,
                                &reg,
                                cmp_cfg,
                            );
                            let _ = tx.send(MeshTaskResult::LodRegion {
                                world_gen: task_gen,
                                dim,
                                key,
                                mesh,
                                occupancy: None,
                                covered_chunks,
                            });
                            return;
                        }
                        let surfaces: Vec<LodChunkSurface> = tile_chunks
                            .into_par_iter()
                            .filter_map(|(cp, override_c, in_a, _)| {
                                if !in_a || active_gen.load(Ordering::Relaxed) != task_gen {
                                    return None;
                                }
                                let cached = override_c
                                    .or_else(|| cache_arc.read().get(&(dim, cp)).cloned());
                                let reg = reg_arc.read();
                                if let Some(c) = cached {
                                    Some(LodChunkSurface::from_chunk(&c, &reg))
                                } else {
                                    format_arc
                                        .load_chunk_lod_surface(dim, cp, &reg)
                                        .ok()
                                        .flatten()
                                }
                            })
                            .collect();
                        if active_gen.load(Ordering::Relaxed) != task_gen {
                            return;
                        }
                        let covered_chunks: Vec<ChunkPos> = surfaces.iter().map(|s| s.pos).collect();
                        let mesh = mesh_lod_overhead_tile(
                            rx8,
                            rz8,
                            &surfaces,
                            step as usize,
                            &palette_arc,
                            min_y,
                            max_y,
                        );
                        let _ = tx.send(MeshTaskResult::LodRegion {
                            world_gen: task_gen,
                            dim,
                            key,
                            mesh,
                            occupancy: None,
                            covered_chunks,
                        });
                    });
                } else {
                    rayon::spawn(move || {
                        if active_gen.load(Ordering::Relaxed) != task_gen {
                            return;
                        }
                        if let Some((fmt_b, cache_b, cmp_cfg)) = cmp_lod {
                            let off = cmp_cfg.chunk_offset;
                            let pairs: Vec<(
                                ChunkPos,
                                Option<LodChunkVoxels>,
                                Option<LodChunkVoxels>,
                            )> = tile_chunks
                                .into_par_iter()
                                .filter_map(|(cp, override_c, in_a, in_b)| {
                                    if active_gen.load(Ordering::Relaxed) != task_gen {
                                        return None;
                                    }
                                    let vox_a = if in_a {
                                        let cached = override_c
                                            .or_else(|| cache_arc.read().get(&(dim, cp)).cloned());
                                        let c = cached.or_else(|| {
                                            format_arc
                                                .load_chunk_with_registry(dim, cp, &reg_arc)
                                                .ok()
                                                .flatten()
                                                .map(Arc::new)
                                        });
                                        c.map(|c| {
                                            LodChunkVoxels::from_chunk_exact(
                                                &c,
                                                step,
                                                &reg_arc.read(),
                                            )
                                        })
                                    } else {
                                        None
                                    };
                                    let vox_b = if in_b {
                                        let cp_b = ChunkPos::new(cp.cx() + off.0, cp.cz() + off.1);
                                        let cached = cache_b.read().get(&(dim, cp_b)).cloned();
                                        let c = cached.or_else(|| {
                                            fmt_b
                                                .load_chunk_with_registry(dim, cp_b, &reg_arc)
                                                .ok()
                                                .flatten()
                                                .map(Arc::new)
                                        });
                                        c.map(|c| {
                                            let mut v = LodChunkVoxels::from_chunk_exact(
                                                &c,
                                                step,
                                                &reg_arc.read(),
                                            );
                                            v.pos = cp;
                                            v
                                        })
                                    } else {
                                        None
                                    };
                                    if vox_a.is_some() || vox_b.is_some() {
                                        Some((cp, vox_a, vox_b))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            if active_gen.load(Ordering::Relaxed) != task_gen {
                                return;
                            }
                            let mut voxels_a = Vec::new();
                            let mut voxels_b = Vec::new();
                            let mut covered_chunks = Vec::with_capacity(pairs.len());
                            for (cp, va, vb) in pairs {
                                covered_chunks.push(cp);
                                if let Some(v) = va {
                                    voxels_a.push(v);
                                }
                                if let Some(v) = vb {
                                    voxels_b.push(v);
                                }
                            }
                            let reg = reg_arc.read();
                            let (mesh, occ) = mesh_lod_voxel_tile_compare(
                                rx8,
                                rz8,
                                &voxels_a,
                                &voxels_b,
                                step,
                                &palette_arc,
                                tile_min_y,
                                &reg,
                                cmp_cfg,
                            );
                            let _ = tx.send(MeshTaskResult::LodRegion {
                                world_gen: task_gen,
                                dim,
                                key,
                                mesh,
                                occupancy: Some(Arc::new(occ)),
                                covered_chunks,
                            });
                            return;
                        }
                        let voxels: Vec<LodChunkVoxels> = tile_chunks
                            .into_par_iter()
                            .filter_map(|(cp, override_c, in_a, _)| {
                                if !in_a || active_gen.load(Ordering::Relaxed) != task_gen {
                                    return None;
                                }
                                let cached = override_c
                                    .or_else(|| cache_arc.read().get(&(dim, cp)).cloned());
                                let reg = reg_arc.read();
                                if let Some(c) = cached {
                                    Some(LodChunkVoxels::from_chunk(&c, step, &reg))
                                } else {
                                    format_arc
                                        .load_chunk_lod_voxels(dim, cp, step, &reg)
                                        .ok()
                                        .flatten()
                                }
                            })
                            .collect();
                        if active_gen.load(Ordering::Relaxed) != task_gen {
                            return;
                        }
                        let covered_chunks: Vec<ChunkPos> = voxels.iter().map(|v| v.pos).collect();
                        let (mesh, occ) =
                            mesh_lod_voxel_tile(rx8, rz8, &voxels, step, &palette_arc, min_y);
                        let _ = tx.send(MeshTaskResult::LodRegion {
                            world_gen: task_gen,
                            dim,
                            key,
                            mesh,
                            occupancy: Some(Arc::new(occ)),
                            covered_chunks,
                        });
                    });
                }
            }
            if dispatched >= total_candidates {
                self.lod_schedule_dirty = false;
            }
        }

        // Flush any dirty 4x4 LOD 0 GPU batches modified this frame and sync any VRAM evictions
        renderer.flush_dirty_batches(device);
        self.sync_renderer_evictions(dim, renderer);

        // 6. Rebuild static world-wide 4-state chunk grid at Y=0 when dirty.
        //    Throttle to 1s during active streaming so massive VD=512 grid scans don't hitch frame pacing,
        //    and rebuild promptly (90ms) once streaming settles.
        let streaming = !self.lod0_inflight.is_empty() || !self.lod_regions_inflight.is_empty();
        let grid_due = self.chunk_grid_dirty
            && ((!streaming && self.last_grid_rebuild.elapsed() >= Duration::from_millis(90))
                || self.last_grid_rebuild.elapsed() >= Duration::from_millis(1000));
        if grid_due {
            self.rebuild_static_chunk_grid(device, renderer);
            self.last_streaming_status =
                Some(("Rebuilding chunk grid...".to_string(), Instant::now()));
        } else if uploaded_any || streaming {
            let lod0_active = self.lod0_inflight.len() + self.lod0_priority_queue.len();
            let lod_active = self.lod_regions_inflight.len();
            let msg = if lod0_active > 0 && lod_active > 0 {
                format!("Processing terrain ({lod0_active} chunks, {lod_active} LOD tiles)...")
            } else if lod0_active > 0 {
                format!("Processing 3D chunks ({lod0_active} in flight)...")
            } else if lod_active > 0 {
                format!("Processing LOD terrain ({lod_active} tiles)...")
            } else {
                "Processing terrain...".to_string()
            };
            self.last_streaming_status = Some((msg, Instant::now()));
        }

        // 7. Update 3D Clone/Brush Preview Mesh if dirty, or translate brush preview origin in O(1)
        if self.preview_dirty {
            self.preview_dirty = false;
            self.rebuild_preview_mesh(device, renderer);
        } else if self.active_tool == ToolId::Brush
            && let Some(center) = self.brush_target_pos()
        {
            renderer.preview_origin = (center + self.brush.preview_offset).as_vec3();
        }
    }

    fn rebuild_static_chunk_grid(&mut self, device: &wgpu::Device, renderer: &mut WorldRenderer) {
        self.chunk_grid_dirty = false;
        self.last_grid_rebuild = Instant::now();

        if !self.config.show_floor_grid {
            renderer.set_chunk_grid(device, None);
            return;
        }
        let Some(world) = &self.world else {
            renderer.set_chunk_grid(device, None);
            return;
        };

        let vd = self.config.view_distance.clamp(2, 1024);
        let min_cx = self.grid_center_cp.cx() - vd;
        let max_cx = self.grid_center_cp.cx() + vd;
        let min_cz = self.grid_center_cp.cz() - vd;
        let max_cz = self.grid_center_cp.cz() + vd;

        let dim = world.active_dim;
        let mut region_set: AHashSet<(i32, i32)> = world
            .known_regions
            .get(&dim)
            .cloned()
            .unwrap_or_default();
        for cp in world.list_chunks(dim) {
            region_set.insert(cp.region_pos());
        }
        let compare_wb = if self.compare.is_active() {
            self.compare.world_b.as_ref()
        } else {
            None
        };
        let cmp_off = self.compare.config.chunk_offset;
        if let Some(wb) = compare_wb
            && let Some(b_chunks) = wb.known_chunks.get(&dim)
        {
            for &cp_b in b_chunks {
                let cp_a = ChunkPos::new(cp_b.cx() - cmp_off.0, cp_b.cz() - cmp_off.1);
                region_set.insert(cp_a.region_pos());
            }
        }
        let mut regions: Vec<(i32, i32)> = region_set.into_iter().collect();
        regions.sort_unstable();

        // Position the grid 4.0 blocks below the bottom of the map so the map clearly floats above the grid
        let grid_y = world.min_y() as f32 - 4.0;
        let grid = OverlayMeshBuilder::build_universal_chunk_grid(
            min_cx,
            max_cx,
            min_cz,
            max_cz,
            grid_y,
            &regions,
            |cp| {
                world.chunk_exists(dim, cp)
                    || compare_wb.is_some_and(|wb| {
                        wb.chunk_exists(
                            dim,
                            ChunkPos::new(cp.cx() + cmp_off.0, cp.cz() + cmp_off.1),
                        )
                    })
            },
            |cp| self.loaded_chunks_for_grid.contains(&(dim, cp)),
        );
        renderer.set_chunk_grid(device, Some(&grid));
    }

    pub fn rebuild_preview_mesh(&mut self, device: &wgpu::Device, renderer: &mut WorldRenderer) {
        let reg = self.registry.read();
        match self.active_tool {
            ToolId::Clone => {
                if let (Some(vol), Some(origin)) =
                    (&self.clone.active_volume, self.clone.placement_origin)
                {
                    let mesh = mesh_volume_preview(vol, &reg, 180);
                    renderer.set_preview_mesh(device, origin.as_vec3(), Some(&mesh));
                    return;
                }
            }
            ToolId::Brush => {
                if let Some(center) = self.brush_target_pos()
                    && let Some((vol, off)) =
                        self.brush.build_preview_volume(&self.plugins, &reg)
                {
                    self.brush.preview_offset = off;
                    let mesh = mesh_volume_preview(&vol, &reg, 160);
                    renderer.set_preview_mesh(
                        device,
                        (center + off).as_vec3(),
                        Some(&mesh),
                    );
                    return;
                }
            }
            _ => {}
        }
        renderer.set_preview_mesh(device, Vec3::ZERO, None);
    }

    pub fn brush_target_pos(&self) -> Option<IVec3> {
        if let Some(hit) = self.hover_hit {
            let mode_id = self.brush.selected_mode_id(&self.plugins);
            let surface = matches!(mode_id, "fill" | "varied_fill" | "paste");
            Some(if surface {
                hit.adjacent_pos
            } else {
                hit.block_pos
            })
        } else {
            let dir = self.camera.forward_vec();
            Some((self.camera.pos + dir * self.tool_reach).floor().as_ivec3())
        }
    }

    pub fn build_overlays(&self, overlays: &mut OverlayMeshBuilder) {
        overlays.clear();

        let mut hovering_selection_face = false;

        // Selection Box & Face Hover Highlight (works from ANY distance using cursor_ray directly!)
        if let Some(b) = self.selection.box_ {
            let min = b.min.as_vec3();
            let max = b.max.as_vec3();
            let is_dragging = self.selection.creating_drag_start.is_some();
            overlays.push_filled_box(min, max, [0.25, 0.55, 1.0, 0.14]);
            let wire_col = if is_dragging {
                [1.0, 0.92, 0.35, 0.98]
            } else {
                [0.4, 0.8, 1.0, 0.92]
            };
            overlays.push_wire_box(min, max, wire_col);

            if self.active_tool == ToolId::Select && !is_dragging {
                let cam_fwd = self.camera.forward_vec();
                if let Some(drag) = &self.selection.dragging_face {
                    hovering_selection_face = true;
                    overlays.push_mcedit_face_highlight(
                        b,
                        drag.active_face,
                        [0.45, 0.78, 1.0, 0.34],
                        [0.95, 0.65, 0.2, 0.92],
                    );
                } else if !self.selection.click_selecting
                    && let Some((ro, rd)) = self.cursor_ray
                    && let Some((face, _)) = b.box_face_under_cursor(
                        ro,
                        rd,
                        cam_fwd,
                        crate::tools::select::EDGE_CLICKTHROUGH_FACTOR,
                    )
                {
                    hovering_selection_face = true;
                    overlays.push_mcedit_face_highlight(
                        b,
                        face,
                        [0.45, 0.78, 1.0, 0.28],
                        [0.95, 0.65, 0.2, 0.92],
                    );
                }
            }
        } else if let Some(ca) = self.selection.corner_a {
            // Two-click mode: highlight corner 1 + live preview box to current cursor target
            overlays.push_filled_box(
                ca.as_vec3(),
                (ca + IVec3::ONE).as_vec3(),
                [0.2, 0.6, 1.0, 0.35],
            );
            overlays.push_wire_box(
                ca.as_vec3(),
                (ca + IVec3::ONE).as_vec3(),
                [0.4, 0.9, 1.0, 0.95],
            );
            if self.selection.click_selecting
                && let Some((ro, rd)) = self.cursor_ray
            {
                let target = self.selection_target_pos(ro, rd);
                let preview = BoundingBox::from_corners_inclusive(ca, target);
                overlays.push_filled_box(
                    preview.min.as_vec3(),
                    preview.max.as_vec3(),
                    [0.35, 0.75, 1.0, 0.10],
                );
                overlays.push_wire_box(
                    preview.min.as_vec3(),
                    preview.max.as_vec3(),
                    [1.0, 0.92, 0.35, 0.85],
                );
            }
        }

        // Hover Block Reticle (suppressed while hovering/resizing a selection wall, matching MCEdit)
        if !hovering_selection_face {
            if let Some(hit) = self.hover_hit {
                let p = match self.active_tool {
                    ToolId::Brush
                        if matches!(
                            self.brush.selected_mode_id(&self.plugins),
                            "fill" | "varied_fill" | "paste"
                        ) =>
                    {
                        hit.adjacent_pos
                    }
                    _ => hit.block_pos,
                };
                let mn = p.as_vec3();
                overlays.push_wire_box(mn, mn + Vec3::ONE, [1.0, 1.0, 0.4, 0.95]);
            } else if self.active_tool == ToolId::Select
                && self.selection.dragging_face.is_none()
                && let Some((ro, rd)) = self.cursor_ray
            {
                let p = self.selection_target_pos(ro, rd).as_vec3();
                overlays.push_wire_box(p, p + Vec3::ONE, [0.45, 0.95, 1.0, 0.75]);
            }
        }

        // Clone Bounding Box
        if self.active_tool == ToolId::Clone
            && let (Some(vol), Some(origin)) =
                (&self.clone.active_volume, self.clone.placement_origin)
        {
            let mn = origin.as_vec3();
            let mx = mn + vol.size.as_vec3();
            overlays.push_wire_box(mn, mx, [0.3, 1.0, 0.4, 0.95]);
        }

        // Chunk selection, borders, markers & entities
        if let Some(world) = &self.world {
            let dim = world.active_dim;
            let min_y = world.min_y() as f32;
            let max_y = world.max_y() as f32;

            for &cp in &self.selected_chunks {
                overlays.push_chunk_marker(cp, min_y, max_y, [0.2, 0.85, 0.4, 0.16]);
            }

            if self.config.show_chunk_borders {
                let cam_cp = ChunkPos::new(
                    (self.camera.pos.x / 16.0).floor() as i32,
                    (self.camera.pos.z / 16.0).floor() as i32,
                );
                for dz in -4..=4 {
                    for dx in -4..=4 {
                        let cp = ChunkPos::new(cam_cp.cx() + dx, cam_cp.cz() + dz);
                        let x = (cp.cx() * 16) as f32;
                        let z = (cp.cz() * 16) as f32;
                        overlays.push_line(
                            Vec3::new(x, min_y, z),
                            Vec3::new(x, max_y, z),
                            [1.0, 1.0, 0.0, 0.4],
                        );
                    }
                }
            }

            if dim == DimensionId::Overworld {
                let sp = world.metadata.spawn_pos.as_vec3();
                overlays.push_filled_box(sp, sp + Vec3::new(1.0, 2.0, 1.0), [0.2, 1.0, 0.3, 0.35]);
                overlays.push_wire_box(sp, sp + Vec3::new(1.0, 2.0, 1.0), [0.2, 1.0, 0.3, 0.95]);
            }

            for p in &world.players {
                if p.dimension == dim {
                    let pos = Vec3::new(p.pos.x as f32, p.pos.y as f32, p.pos.z as f32);
                    let mn = pos - Vec3::new(0.4, 0.0, 0.4);
                    let mx = pos + Vec3::new(0.4, 1.8, 0.4);
                    overlays.push_filled_box(mn, mx, [1.0, 0.5, 0.15, 0.4]);
                    overlays.push_wire_box(mn, mx, [1.0, 0.7, 0.2, 0.95]);
                }
            }

            if self.config.show_entities || self.config.show_block_entities {
                let cam_cp = ChunkPos::new(
                    (self.camera.pos.x / 16.0).floor() as i32,
                    (self.camera.pos.z / 16.0).floor() as i32,
                );
                for dz in -4..=4 {
                    for dx in -4..=4 {
                        let cp = ChunkPos::new(cam_cp.cx() + dx, cam_cp.cz() + dz);
                        if let Some(chunk) = world.get_loaded_chunk(dim, cp) {
                            if self.config.show_block_entities {
                                for be in &chunk.block_entities {
                                    if let (Some(x), Some(y), Some(z)) =
                                        (be.get_int("x"), be.get_int("y"), be.get_int("z"))
                                    {
                                        let mn = Vec3::new(x as f32, y as f32, z as f32);
                                        overlays.push_wire_box(
                                            mn,
                                            mn + Vec3::ONE,
                                            [1.0, 0.9, 0.2, 0.75],
                                        );
                                    }
                                }
                            }
                            if self.config.show_entities {
                                for ent in &chunk.entities {
                                    if let Some(pos_list) = ent.get_list("Pos")
                                        && pos_list.len() == 3
                                    {
                                        let coords: Vec<f32> = pos_list
                                            .items()
                                            .iter()
                                            .filter_map(|t| match t {
                                                mcedit_nbt::NbtTag::Double(v) => Some(*v as f32),
                                                mcedit_nbt::NbtTag::Float(v) => Some(*v),
                                                _ => None,
                                            })
                                            .collect();
                                        if coords.len() == 3 {
                                            let c = Vec3::new(coords[0], coords[1], coords[2]);
                                            overlays.push_wire_box(
                                                c - Vec3::new(0.3, 0.0, 0.3),
                                                c + Vec3::new(0.3, 0.8, 0.3),
                                                [1.0, 0.25, 0.25, 0.8],
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn sky_color(&self) -> [f32; 4] {
        match self.world.as_ref().map(|w| w.active_dim).or(self.last_dim) {
            Some(DimensionId::Nether) => [0.22, 0.04, 0.04, 1.0],
            Some(DimensionId::End) => [0.08, 0.09, 0.11, 1.0],
            _ => [0.48, 0.68, 0.95, 1.0],
        }
    }
}
