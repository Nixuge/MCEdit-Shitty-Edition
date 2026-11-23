//! Zero-dependency profiling: lock-free stage accumulators, frame history,
//! streaming milestones and an opt-in Chrome Trace / Perfetto recorder.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

macro_rules! id_enum {
    ($name:ident, { $($v:ident => $label:expr),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(usize)]
        pub enum $name { $($v),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$v),+];
            pub fn label(self) -> &'static str {
                match self { $($name::$v => $label),+ }
            }
            #[inline]
            fn idx(self) -> usize { self as usize }
        }
    };
}

id_enum!(Stage, {
    WorldOpen => "World open / header scan",
    ChunkRead => "Chunk disk read + zlib",
    NbtParse => "NBT parse (DOM build)",
    ChunkDecode => "Chunk section decode",
    Lod0MeshFull => "LOD0 greedy mesh",
    BatchMerge => "4x4 batch merge + GPU upload",
    LodVoxelLoad => "LOD voxel load + downsample",
    LodVoxelMesh => "LOD voxel tile mesh",
    LodSurfaceMesh => "LOD 2D surface tile mesh",
    FramePump => "Frame: pump meshing + uploads",
    FrameRaycast => "Frame: raycast",
    FrameRender => "Frame: world render encode",
    FrameBlur => "Frame: panel backdrop blur",
    FrameEgui => "Frame: egui build + tessellate",
    FramePresent => "Frame: acquire + present (vsync wait)",
});

id_enum!(Counter, {
    ChunksDiskLoaded => "chunks decoded from disk",
    UncompressedNbtBytes => "uncompressed NBT bytes",
    Lod0Chunks => "LOD0 chunks meshed",
    Lod0Vertices => "LOD0 vertices",
    Lod0Indices => "LOD0 indices",
    BatchesRebuilt => "4x4 batches rebuilt",
    DrawCalls => "draw calls (this frame)",
    Triangles => "triangles (this frame)",
});

const STAGE_COUNT: usize = Stage::ALL.len();
const COUNTER_COUNT: usize = Counter::ALL.len();

static STAGE_CALLS: [AtomicU64; STAGE_COUNT] = [const { AtomicU64::new(0) }; STAGE_COUNT];
static STAGE_US: [AtomicU64; STAGE_COUNT] = [const { AtomicU64::new(0) }; STAGE_COUNT];
static STAGE_MAX_US: [AtomicU64; STAGE_COUNT] = [const { AtomicU64::new(0) }; STAGE_COUNT];
static COUNTERS: [AtomicU64; COUNTER_COUNT] = [const { AtomicU64::new(0) }; COUNTER_COUNT];

static EPOCH: OnceLock<Instant> = OnceLock::new();
static TRACE_ON: AtomicBool = AtomicBool::new(false);
static TRACE_EVENTS: Mutex<Vec<TraceEvent>> = Mutex::new(Vec::new());
static THREAD_NAMES: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());
static NEXT_TID: AtomicU32 = AtomicU32::new(1);
const MAX_TRACE_EVENTS: usize = 2_000_000;

struct TraceEvent {
    stage: Stage,
    tid: u32,
    start_us: u64,
    dur_us: u64,
}

thread_local! {
    static TID: u32 = {
        let id = NEXT_TID.fetch_add(1, Ordering::Relaxed);
        let name = std::thread::current()
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("thread-{id}"));
        THREAD_NAMES.lock().push((id, name));
        id
    };
}

#[inline]
fn now_us() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Records one completed `stage` execution of duration `dur`.
#[inline]
pub fn record(stage: Stage, dur: Duration) {
    let us = dur.as_micros() as u64;
    let i = stage.idx();
    STAGE_CALLS[i].fetch_add(1, Ordering::Relaxed);
    STAGE_US[i].fetch_add(us, Ordering::Relaxed);
    STAGE_MAX_US[i].fetch_max(us, Ordering::Relaxed);
    if TRACE_ON.load(Ordering::Relaxed) {
        let tid = TID.with(|t| *t);
        let mut ev = TRACE_EVENTS.lock();
        if ev.len() < MAX_TRACE_EVENTS {
            ev.push(TraceEvent {
                stage,
                tid,
                start_us: now_us().saturating_sub(us),
                dur_us: us,
            });
        }
    }
}

#[inline]
pub fn add(counter: Counter, n: u64) {
    COUNTERS[counter.idx()].fetch_add(n, Ordering::Relaxed);
}

/// Reads and zeroes a counter (used for per-frame counters).
#[inline]
pub fn take(counter: Counter) -> u64 {
    COUNTERS[counter.idx()].swap(0, Ordering::Relaxed)
}

pub fn counter(counter: Counter) -> u64 {
    COUNTERS[counter.idx()].load(Ordering::Relaxed)
}

/// RAII timing guard; records into the stage accumulators on drop.
pub struct Scope {
    stage: Stage,
    start: Instant,
}

#[inline]
pub fn scope(stage: Stage) -> Scope {
    Scope {
        stage,
        start: Instant::now(),
    }
}

impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {
        record(self.stage, self.start.elapsed());
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StageStat {
    pub stage: Stage,
    pub calls: u64,
    pub total_us: u64,
    pub max_us: u64,
}

impl StageStat {
    pub fn avg_us(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.total_us as f64 / self.calls as f64
        }
    }
}

pub fn snapshot() -> Vec<StageStat> {
    Stage::ALL
        .iter()
        .map(|&s| StageStat {
            stage: s,
            calls: STAGE_CALLS[s.idx()].load(Ordering::Relaxed),
            total_us: STAGE_US[s.idx()].load(Ordering::Relaxed),
            max_us: STAGE_MAX_US[s.idx()].load(Ordering::Relaxed),
        })
        .collect()
}

pub fn reset() {
    for i in 0..STAGE_COUNT {
        STAGE_CALLS[i].store(0, Ordering::Relaxed);
        STAGE_US[i].store(0, Ordering::Relaxed);
        STAGE_MAX_US[i].store(0, Ordering::Relaxed);
    }
    for c in &COUNTERS {
        c.store(0, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- frames

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameSample {
    pub total_ms: f32,
    pub pump_ms: f32,
    pub raycast_ms: f32,
    pub render_ms: f32,
    pub blur_ms: f32,
    pub egui_ms: f32,
    pub present_ms: f32,
    pub draw_calls: u32,
    pub triangles: u64,
}

pub const FRAME_HISTORY: usize = 120;
static FRAMES: Mutex<VecDeque<FrameSample>> = Mutex::new(VecDeque::new());

pub fn push_frame(s: FrameSample) {
    let mut f = FRAMES.lock();
    if f.len() >= FRAME_HISTORY {
        f.pop_front();
    }
    f.push_back(s);
}

pub fn frames() -> Vec<FrameSample> {
    FRAMES.lock().iter().copied().collect()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub fps: f32,
    pub avg_ms: f32,
    pub p95_ms: f32,
    pub max_ms: f32,
}

pub fn frame_stats() -> FrameStats {
    let f = FRAMES.lock();
    if f.is_empty() {
        return FrameStats::default();
    }
    let mut v: Vec<f32> = f.iter().map(|s| s.total_ms).collect();
    let avg = v.iter().sum::<f32>() / v.len() as f32;
    v.sort_by(|a, b| a.total_cmp(b));
    let p95 = v[((v.len() as f32 * 0.95) as usize).min(v.len() - 1)];
    FrameStats {
        fps: if avg > 0.0 { 1000.0 / avg } else { 0.0 },
        avg_ms: avg,
        p95_ms: p95,
        max_ms: *v.last().unwrap_or(&0.0),
    }
}

// ------------------------------------------------------------ milestones

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Milestone {
    FirstChunk,
    Near8x8,
    FullDetail,
}

static MS_START: AtomicU64 = AtomicU64::new(0);
static MS_SLOTS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

pub fn milestones_reset() {
    MS_START.store(now_us(), Ordering::Relaxed);
    for s in &MS_SLOTS {
        s.store(0, Ordering::Relaxed);
    }
}

/// Marks `m` as reached (first call wins until the next `milestones_reset`).
pub fn milestone_mark(m: Milestone) {
    let slot = &MS_SLOTS[m as usize];
    if slot.load(Ordering::Relaxed) == 0 {
        let d = now_us().saturating_sub(MS_START.load(Ordering::Relaxed)) + 1;
        let _ = slot.compare_exchange(0, d, Ordering::Relaxed, Ordering::Relaxed);
    }
}

pub fn milestone_ms(m: Milestone) -> Option<f64> {
    match MS_SLOTS[m as usize].load(Ordering::Relaxed) {
        0 => None,
        v => Some((v - 1) as f64 / 1000.0),
    }
}

// ----------------------------------------------------------------- trace

pub fn trace_enabled() -> bool {
    TRACE_ON.load(Ordering::Relaxed)
}

pub fn trace_start() {
    TRACE_EVENTS.lock().clear();
    TRACE_ON.store(true, Ordering::Relaxed);
}

/// Stops recording and writes Chrome Trace Event JSON to `path`; returns the event count.
pub fn trace_stop_and_write(path: &Path) -> std::io::Result<usize> {
    TRACE_ON.store(false, Ordering::Relaxed);
    let events = std::mem::take(&mut *TRACE_EVENTS.lock());
    let names = THREAD_NAMES.lock().clone();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, chrome_trace_json(&events, &names))?;
    Ok(events.len())
}

fn json_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            c if (c as u32) < 0x20 => vec![' '],
            c => vec![c],
        })
        .collect()
}

fn chrome_trace_json(events: &[TraceEvent], names: &[(u32, String)]) -> String {
    let mut out = String::with_capacity(events.len() * 96 + 256);
    out.push_str("{\"displayTimeUnit\":\"ms\",\"traceEvents\":[");
    let mut first = true;
    for (tid, name) in names {
        if !first {
            out.push(',');
        }
        first = false;
        let _ = write!(
            out,
            "{{\"name\":\"thread_name\",\"ph\":\"M\",\"pid\":1,\"tid\":{tid},\"args\":{{\"name\":\"{}\"}}}}",
            json_escape(name)
        );
    }
    for e in events {
        if !first {
            out.push(',');
        }
        first = false;
        let _ = write!(
            out,
            "{{\"name\":\"{}\",\"cat\":\"mcedit\",\"ph\":\"X\",\"pid\":1,\"tid\":{},\"ts\":{},\"dur\":{}}}",
            json_escape(e.stage.label()),
            e.tid,
            e.start_us,
            e.dur_us
        );
    }
    out.push_str("]}");
    out
}

// ---------------------------------------------------------------- report

pub fn format_report() -> String {
    let mut out = String::new();
    let st = frame_stats();
    let _ = writeln!(
        out,
        "Frame: {:.0} fps | avg {:.2} ms | p95 {:.2} ms | max {:.2} ms",
        st.fps, st.avg_ms, st.p95_ms, st.max_ms
    );
    for (label, m) in [
        ("first chunk", Milestone::FirstChunk),
        ("8x8 area", Milestone::Near8x8),
        ("full detail", Milestone::FullDetail),
    ] {
        match milestone_ms(m) {
            Some(ms) => {
                let _ = writeln!(out, "Time to {label}: {ms:.0} ms");
            }
            None => {
                let _ = writeln!(out, "Time to {label}: -");
            }
        }
    }
    let _ = writeln!(
        out,
        "{:<40} {:>8} {:>11} {:>10} {:>10}",
        "Stage", "calls", "total ms", "avg us", "max us"
    );
    let mut stats = snapshot();
    stats.retain(|s| s.calls > 0);
    stats.sort_by_key(|s| std::cmp::Reverse(s.total_us));
    for s in stats {
        let _ = writeln!(
            out,
            "{:<40} {:>8} {:>11.1} {:>10.1} {:>10}",
            s.stage.label(),
            s.calls,
            s.total_us as f64 / 1000.0,
            s.avg_us(),
            s.max_us
        );
    }
    for &c in Counter::ALL {
        let v = counter(c);
        if v > 0 {
            let _ = writeln!(out, "{:<40} {v}", c.label());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_milestones_and_chrome_trace_export() {
        trace_start();
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..10 {
                        let _g = scope(Stage::Lod0MeshFull);
                        std::thread::sleep(Duration::from_micros(50));
                    }
                });
            }
        });
        add(Counter::Lod0Chunks, 3);
        milestones_reset();
        milestone_mark(Milestone::FirstChunk);
        assert!(milestone_ms(Milestone::FirstChunk).is_some());
        assert!(milestone_ms(Milestone::FullDetail).is_none());

        let snap = snapshot();
        let mesh = snap.iter().find(|s| s.stage == Stage::Lod0MeshFull).unwrap();
        assert!(mesh.calls >= 40);
        assert!(counter(Counter::Lod0Chunks) >= 3);

        let path = std::env::temp_dir().join("mcedit_profiler_test_trace.json");
        let n = trace_stop_and_write(&path).unwrap();
        assert!(n >= 40);
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["traceEvents"].as_array().unwrap().len() >= 40);
        assert!(!trace_enabled());
        let _ = std::fs::remove_file(path);
        assert!(format_report().contains("LOD0 greedy mesh"));

        push_frame(FrameSample {
            total_ms: 10.0,
            ..Default::default()
        });
        assert!(frame_stats().avg_ms > 0.0);
    }
}
