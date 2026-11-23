//! Headless benchmark profiler for chunk loading / meshing / batching.
//!
//! Usage: mcedit-profile <world_path> [--detail-radius N] [--lod-tiles N]
//!                       [--trace trace.json] [--json summary.json]

use ahash::{AHashMap, AHashSet};
use mcedit_core::profiler::{self, Stage};
use mcedit_core::{BlockRegistry, Chunk, ChunkPos, DimensionId, World};
use mcedit_formats::FormatRegistry;
use mcedit_render::{
    ChunkNeighborSnapshot, MeshOptions, SectionMeshData, TextureAtlas, mesh_chunk_full,
    mesh_lod_voxel_tile,
};
use parking_lot::RwLock;
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

struct Args {
    world: PathBuf,
    detail_radius: i32,
    lod_tiles: usize,
    trace: Option<PathBuf>,
    json: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut world = None;
    let mut detail_radius = 12;
    let mut lod_tiles = 16;
    let mut trace = None;
    let mut json = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--detail-radius" => {
                detail_radius = it.next().and_then(|v| v.parse().ok()).unwrap_or(12)
            }
            "--lod-tiles" => lod_tiles = it.next().and_then(|v| v.parse().ok()).unwrap_or(16),
            "--trace" => trace = it.next().map(PathBuf::from),
            "--json" => json = it.next().map(PathBuf::from),
            "-h" | "--help" => {
                return Err("usage: mcedit-profile <world_path> [--detail-radius N] [--lod-tiles N] [--trace out.json] [--json out.json]".into());
            }
            other => world = Some(PathBuf::from(other)),
        }
    }
    Ok(Args {
        world: world.ok_or("missing <world_path> (see --help)")?,
        detail_radius,
        lod_tiles,
        trace,
        json,
    })
}

struct Phase {
    name: String,
    chunks: usize,
    load_ms: f64,
    mesh_ms: f64,
    total_ms: f64,
    vertices: u64,
}

impl Phase {
    fn print(&self) {
        let cps = if self.total_ms > 0.0 {
            self.chunks as f64 / (self.total_ms / 1000.0)
        } else {
            0.0
        };
        println!(
            "== {} ==\n  chunks {} | load {:.1} ms | mesh {:.1} ms | wall {:.1} ms | {:.0} chunks/s | {:.2}M verts",
            self.name,
            self.chunks,
            self.load_ms,
            self.mesh_ms,
            self.total_ms,
            cps,
            self.vertices as f64 / 1e6
        );
    }
}

fn neighbors(cp: ChunkPos) -> [ChunkPos; 5] {
    [
        cp,
        ChunkPos::new(cp.cx() + 1, cp.cz()),
        ChunkPos::new(cp.cx() - 1, cp.cz()),
        ChunkPos::new(cp.cx(), cp.cz() + 1),
        ChunkPos::new(cp.cx(), cp.cz() - 1),
    ]
}

/// Loads `targets` (+ neighbors) cold from disk, then meshes each target.
fn run_batch(
    name: &str,
    world: &World,
    reg: &Arc<RwLock<BlockRegistry>>,
    dim: DimensionId,
    targets: &[ChunkPos],
    parallel: bool,
) -> (Phase, Vec<(ChunkPos, SectionMeshData)>) {
    let t_all = Instant::now();
    let mut needed: Vec<ChunkPos> = Vec::new();
    let mut seen = AHashSet::new();
    for &cp in targets {
        for n in neighbors(cp) {
            if world.chunk_exists(dim, n) && seen.insert(n) {
                needed.push(n);
            }
        }
    }
    let fmt = world.format.clone();
    let load = |p: &ChunkPos| -> Option<(ChunkPos, Arc<Chunk>)> {
        let r = reg.read();
        fmt.load_chunk_readonly(dim, *p, &r)
            .ok()
            .flatten()
            .map(|c| (*p, Arc::new(c)))
    };
    let t_load = Instant::now();
    let loaded: Vec<(ChunkPos, Arc<Chunk>)> = if parallel {
        needed.par_iter().filter_map(load).collect()
    } else {
        needed.iter().filter_map(load).collect()
    };
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
    let map: AHashMap<ChunkPos, Arc<Chunk>> = loaded.into_iter().collect();

    let t_mesh = Instant::now();
    let mesh_one = |cp: &ChunkPos| -> Option<(ChunkPos, SectionMeshData)> {
        let center = map.get(cp)?.clone();
        let snap = ChunkNeighborSnapshot {
            center,
            east: map.get(&ChunkPos::new(cp.cx() + 1, cp.cz())).cloned(),
            west: map.get(&ChunkPos::new(cp.cx() - 1, cp.cz())).cloned(),
            south: map.get(&ChunkPos::new(cp.cx(), cp.cz() + 1)).cloned(),
            north: map.get(&ChunkPos::new(cp.cx(), cp.cz() - 1)).cloned(),
        };
        let r = reg.read();
        Some((*cp, mesh_chunk_full(&snap, &r, MeshOptions::default())))
    };
    let meshes: Vec<(ChunkPos, SectionMeshData)> = if parallel {
        targets.par_iter().filter_map(mesh_one).collect()
    } else {
        targets.iter().filter_map(mesh_one).collect()
    };
    let mesh_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;
    let vertices = meshes
        .iter()
        .map(|(_, m)| {
            (m.opaque.vertices.len() + m.alpha_cutout.vertices.len() + m.translucent.vertices.len())
                as u64
        })
        .sum();
    (
        Phase {
            name: name.to_string(),
            chunks: meshes.len(),
            load_ms,
            mesh_ms,
            total_ms: t_all.elapsed().as_secs_f64() * 1000.0,
            vertices,
        },
        meshes,
    )
}

fn square_targets(
    world: &World,
    dim: DimensionId,
    center: ChunkPos,
    r: i32,
) -> Vec<ChunkPos> {
    let mut v: Vec<(i32, ChunkPos)> = Vec::new();
    for dz in -r..=r {
        for dx in -r..=r {
            let cp = ChunkPos::new(center.cx() + dx, center.cz() + dz);
            if world.chunk_exists(dim, cp) {
                v.push((dx * dx + dz * dz, cp));
            }
        }
    }
    v.sort_by_key(|(d, _)| *d);
    v.into_iter().map(|(_, c)| c).collect()
}

fn section(title: &str) {
    println!("\n##### {title}");
    profiler::reset();
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if args.trace.is_some() {
        profiler::trace_start();
    }
    let mut json_phases = Vec::new();

    // ---- Phase 1: world open
    section("Phase 1: world open + header scan");
    let t = Instant::now();
    let reg = Arc::new(RwLock::new(BlockRegistry::with_pre113_defaults()));
    let fmt = match FormatRegistry::open_world(&args.world, &reg.read()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("failed to open world: {e}");
            std::process::exit(1);
        }
    };
    let world = match World::open(fmt, reg.clone(), 10) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("failed to load world: {e}");
            std::process::exit(1);
        }
    };
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    profiler::record(Stage::WorldOpen, t.elapsed());
    let dim = world.active_dim;
    let set = world.known_chunks.get(&dim).cloned().unwrap_or_default();
    println!(
        "{} ({}) opened in {:.1} ms | dim {:?} | {} chunks",
        world.metadata.level_name,
        world.format.format_name(),
        open_ms,
        dim,
        set.len()
    );
    if set.is_empty() {
        eprintln!("no chunks in active dimension");
        std::process::exit(1);
    }

    // Camera: player pos, else spawn; snap to nearest generated chunk when far from any.
    let (cx, cz) = match world.players.first() {
        Some(p) => ((p.pos.x / 16.0).floor() as i32, (p.pos.z / 16.0).floor() as i32),
        None => (
            world.metadata.spawn_pos.x.div_euclid(16),
            world.metadata.spawn_pos.z.div_euclid(16),
        ),
    };
    let mut center = ChunkPos::new(cx, cz);
    let nearest = set
        .iter()
        .copied()
        .min_by_key(|c| {
            let dx = (c.cx() - cx) as i64;
            let dz = (c.cz() - cz) as i64;
            dx * dx + dz * dz
        })
        .unwrap();
    if (nearest.cx() - cx).abs().max((nearest.cz() - cz).abs()) > 16 {
        center = nearest;
    }
    println!("profiling around chunk ({}, {})", center.cx(), center.cz());
    json_phases.push(serde_json::json!({"phase": "open", "ms": open_ms, "chunks": set.len()}));

    // ---- Phase 2: single-thread 9x9
    section("Phase 2: 9x9 immediate area, single thread, cold");
    let near = square_targets(&world, dim, center, 4);
    let (p, _) = run_batch("9x9 single-thread", &world, &reg, dim, &near, false);
    p.print();
    println!("{}", profiler::format_report());
    json_phases.push(serde_json::json!({
        "phase": p.name, "chunks": p.chunks, "load_ms": p.load_ms, "mesh_ms": p.mesh_ms,
        "wall_ms": p.total_ms, "vertices": p.vertices,
    }));
    let single_ms = p.total_ms;

    // ---- Phase 3: rayon 9x9
    section("Phase 3: 9x9 immediate area, rayon parallel, cold");
    let (p, _) = run_batch("9x9 parallel", &world, &reg, dim, &near, true);
    p.print();
    println!(
        "  speedup vs single-thread: {:.2}x on {} threads",
        single_ms / p.total_ms.max(0.001),
        rayon::current_num_threads()
    );
    println!("{}", profiler::format_report());
    json_phases.push(serde_json::json!({
        "phase": p.name, "chunks": p.chunks, "load_ms": p.load_ms, "mesh_ms": p.mesh_ms,
        "wall_ms": p.total_ms, "vertices": p.vertices,
    }));

    // ---- Phase 4: full detail radius + batch merge simulation
    section(&format!(
        "Phase 4: detail radius {} + 4x4 batch merge simulation",
        args.detail_radius
    ));
    let detail = square_targets(&world, dim, center, args.detail_radius);
    let (p, meshes) = run_batch("detail radius", &world, &reg, dim, &detail, true);
    p.print();

    let mut groups: AHashMap<(i32, i32), Vec<usize>> = AHashMap::new();
    for (i, (cp, _)) in meshes.iter().enumerate() {
        groups
            .entry((cp.cx().div_euclid(4), cp.cz().div_euclid(4)))
            .or_default()
            .push(i);
    }
    // Incremental: re-merge the whole 4x4 batch every time one more chunk arrives.
    let t_inc = Instant::now();
    let mut inc_bytes = 0usize;
    let mut arrived: AHashMap<(i32, i32), Vec<usize>> = AHashMap::new();
    for (i, (cp, _)) in meshes.iter().enumerate() {
        let key = (cp.cx().div_euclid(4), cp.cz().div_euclid(4));
        let list = arrived.entry(key).or_default();
        list.push(i);
        let mut merged = SectionMeshData::default();
        for &j in list.iter() {
            merged.append(&meshes[j].1);
        }
        inc_bytes += merged.byte_size();
    }
    let inc_ms = t_inc.elapsed().as_secs_f64() * 1000.0;
    // Coalesced: merge each batch exactly once.
    let t_co = Instant::now();
    let mut co_bytes = 0usize;
    for list in groups.values() {
        let mut merged = SectionMeshData::default();
        for &j in list {
            merged.append(&meshes[j].1);
        }
        co_bytes += merged.byte_size();
    }
    let co_ms = t_co.elapsed().as_secs_f64() * 1000.0;
    println!(
        "  batch merge: incremental {:.1} ms ({:.1} MB copied) vs coalesced {:.1} ms ({:.1} MB) over {} batches -> {:.1}x waste",
        inc_ms,
        inc_bytes as f64 / 1e6,
        co_ms,
        co_bytes as f64 / 1e6,
        groups.len(),
        inc_ms / co_ms.max(0.001)
    );
    println!("{}", profiler::format_report());
    json_phases.push(serde_json::json!({
        "phase": p.name, "chunks": p.chunks, "load_ms": p.load_ms, "mesh_ms": p.mesh_ms,
        "wall_ms": p.total_ms, "vertices": p.vertices,
        "batch_incremental_ms": inc_ms, "batch_coalesced_ms": co_ms,
    }));

    // ---- Phase 5: distant LOD voxel tiles
    section("Phase 5: distant LOD voxel tiles (step 2)");
    let palette = TextureAtlas::default_atlas().build_lod_colors(&reg.read());
    let min_y = world.min_y();
    let r2 = (args.detail_radius * args.detail_radius) as i64;
    let mut tiles: AHashMap<(i32, i32), Vec<ChunkPos>> = AHashMap::new();
    for &cp in &set {
        let dx = (cp.cx() - center.cx()) as i64;
        let dz = (cp.cz() - center.cz()) as i64;
        if dx * dx + dz * dz > r2 {
            tiles
                .entry((cp.cx().div_euclid(8), cp.cz().div_euclid(8)))
                .or_default()
                .push(cp);
        }
    }
    let mut tile_list: Vec<((i32, i32), Vec<ChunkPos>)> = tiles.into_iter().collect();
    tile_list.sort_by_key(|((rx, rz), _)| {
        let dx = (*rx * 8 + 4 - center.cx()) as i64;
        let dz = (*rz * 8 + 4 - center.cz()) as i64;
        dx * dx + dz * dz
    });
    tile_list.truncate(args.lod_tiles);
    let t_lod = Instant::now();
    let mut lod_chunks = 0usize;
    let mut load_us = 0u128;
    let mut mesh_us = 0u128;
    for ((rx8, rz8), chunks) in &tile_list {
        let t = Instant::now();
        let voxels: Vec<_> = chunks
            .par_iter()
            .filter_map(|&cp| {
                let r = reg.read();
                world.format.load_chunk_lod_voxels(dim, cp, 2, &r).ok().flatten()
            })
            .collect();
        load_us += t.elapsed().as_micros();
        lod_chunks += voxels.len();
        let t = Instant::now();
        let _ = mesh_lod_voxel_tile(*rx8, *rz8, &voxels, 2, &palette, min_y);
        mesh_us += t.elapsed().as_micros();
    }
    println!(
        "== LOD tiles ==\n  {} tiles / {} chunks | load {:.1} ms | mesh {:.1} ms | wall {:.1} ms",
        tile_list.len(),
        lod_chunks,
        load_us as f64 / 1000.0,
        mesh_us as f64 / 1000.0,
        t_lod.elapsed().as_secs_f64() * 1000.0
    );
    println!("{}", profiler::format_report());
    json_phases.push(serde_json::json!({
        "phase": "lod tiles", "tiles": tile_list.len(), "chunks": lod_chunks,
        "load_ms": load_us as f64 / 1000.0, "mesh_ms": mesh_us as f64 / 1000.0,
    }));

    if let Some(path) = &args.trace {
        match profiler::trace_stop_and_write(path) {
            Ok(n) => println!("wrote trace ({n} events) to {}", path.display()),
            Err(e) => eprintln!("trace write failed: {e}"),
        }
    }
    if let Some(path) = &args.json {
        let doc = serde_json::json!({
            "world": args.world.display().to_string(),
            "threads": rayon::current_num_threads(),
            "phases": json_phases,
        });
        match std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap_or_default()) {
            Ok(()) => println!("wrote summary to {}", path.display()),
            Err(e) => eprintln!("json write failed: {e}"),
        }
    }
}
