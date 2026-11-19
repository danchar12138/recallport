//! Long operations (export, restore, merge) run in the background and report progress for the web UI.
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Default, Serialize)]
pub struct JobState {
    pub id: String,
    pub kind: String,
    pub phase: String,
    /// 0.0 to 1.0 over the whole job; `None` while the length of a step is unknown.
    pub fraction: Option<f64>,
    pub finished: bool,
    pub error: Option<String>,
    pub result: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct Job {
    state: Arc<Mutex<JobState>>,
    span: Arc<Mutex<(f64, f64)>>,
}

impl Job {
    pub fn detached() -> Self {
        Self {
            state: Arc::new(Mutex::new(JobState::default())),
            span: Arc::new(Mutex::new((0.0, 1.0))),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start a step that covers `from..to` of the whole job.
    pub fn phase(&self, name: &str, from: f64, to: f64) {
        *self.span.lock().unwrap_or_else(|e| e.into_inner()) = (from, to);
        let mut s = self.lock();
        s.phase = name.to_owned();
        s.fraction = Some(from);
    }

    pub fn step(&self, done: u64, total: u64) {
        let (from, to) = *self.span.lock().unwrap_or_else(|e| e.into_inner());
        let ratio = if total == 0 {
            1.0
        } else {
            (done as f64 / total as f64).min(1.0)
        };
        self.lock().fraction = Some(from + (to - from) * ratio);
    }

    pub fn snapshot(&self) -> JobState {
        self.lock().clone()
    }

    pub fn succeed(&self, result: serde_json::Value) {
        let mut s = self.lock();
        s.finished = true;
        s.fraction = Some(1.0);
        s.result = Some(result);
    }

    pub fn fail(&self, error: String) {
        let mut s = self.lock();
        s.finished = true;
        s.error = Some(error);
    }
}

/// Only one heavy job at a time: they all rewrite or read the whole library.
#[derive(Default)]
pub struct Jobs {
    map: Mutex<HashMap<String, Job>>,
}

impl Jobs {
    pub fn start(&self, kind: &str) -> Option<Job> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.values().any(|j| !j.snapshot().finished) {
            return None;
        }
        // Keep only the most recent few finished jobs.
        if map.len() > 8 {
            map.retain(|_, j| !j.snapshot().finished);
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let job = Job::detached();
        {
            let mut s = job.lock();
            s.id = id.clone();
            s.kind = kind.to_owned();
        }
        map.insert(id, job.clone());
        Some(job)
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn busy(&self) -> bool {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|j| !j.snapshot().finished)
    }
}
