use mcedit_core::{AnalysisResult, BoundingBox, World};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub struct JobProgress {
    pub title: String,
    pub message: Mutex<String>,
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancelled: Arc<AtomicBool>,
}

impl JobProgress {
    pub fn new(title: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            title: title.into(),
            message: Mutex::new(String::new()),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn set(&self, done: u64, total: u64, msg: impl Into<String>) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        *self.message.lock() = msg.into();
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn fraction(&self) -> Option<f32> {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            None
        } else {
            let d = self.done.load(Ordering::Relaxed).min(t);
            Some(d as f32 / t as f32)
        }
    }
}

pub enum JobOutcome {
    WorldUpdated {
        world: World,
        status: String,
        invalidate_all: bool,
        dirty_box: Option<BoundingBox>,
    },
    WorldSaved {
        world: World,
        result: Result<(), String>,
    },
    AnalysisDone {
        world: World,
        result: AnalysisResult,
    },
    Error {
        world: Option<World>,
        error: String,
    },
}

pub struct ActiveJob {
    pub progress: Arc<JobProgress>,
    pub rx: Receiver<JobOutcome>,
}

#[derive(Default)]
pub struct JobManager {
    pub active: Option<ActiveJob>,
}

impl JobManager {
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    pub fn spawn_world_job<F>(&mut self, title: impl Into<String>, world: World, work: F)
    where
        F: FnOnce(World, Arc<JobProgress>) -> JobOutcome + Send + 'static,
    {
        let progress = JobProgress::new(title);
        let progress_c = progress.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let outcome = work(world, progress_c);
            let _ = tx.send(outcome);
        });
        self.active = Some(ActiveJob { progress, rx });
    }

    pub fn poll(&mut self) -> Option<JobOutcome> {
        let active = self.active.as_ref()?;
        match active.rx.try_recv() {
            Ok(outcome) => {
                self.active = None;
                Some(outcome)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.active = None;
                Some(JobOutcome::Error {
                    world: None,
                    error: "Background job terminated unexpectedly".to_string(),
                })
            }
        }
    }
}
