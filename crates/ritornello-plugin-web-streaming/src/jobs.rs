//! The download queue: one job at a time, a view anyone may read, and the
//! last twenty remembered in `jobs.json`.
//!
//! A job is a list of preset numbers and a target. It resolves the target when
//! it *starts* (the share may have been remounted read-only since it was
//! queued), then downloads each preset in turn through
//! [`download_preset`](crate::download::download_preset). Per video it records
//! done, failed (with yt-dlp's last line) or skipped.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ritornello_files_mount::mounts::read_proc_mounts;
use ritornello_files_mount::roots::Roots;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::download::{Action, Cancel, Choices, CoverMode, Ctx, Event, Format, Note, PresetEnd, Trim, VideoState, download_preset};
use crate::entries::{Entries, write_atomically};
use crate::reason;
use crate::targets::{Resolved, Target, TargetError, resolve_parts};
use crate::tool::ToolGate;

/// How many jobs `jobs.json` and the view keep.
pub const KEPT_JOBS: usize = 20;

pub type JobId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Cancelled,
    /// The job could not even start (target refused, ...).
    Failed(String),
}

impl JobState {
    fn is_over(&self) -> bool {
        !matches!(self, JobState::Queued | JobState::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub preset: u8,
    pub id: String,
    pub title: String,
    #[serde(flatten)]
    pub state: VideoState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f32>,
    /// What went otherwise than asked (format kept, cover beside the file...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

/// What the operator chose for a whole job, kept with it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOptions {
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub cover: CoverMode,
    /// The name typed per preset: a video's file, a playlist's folder.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub names: std::collections::BTreeMap<u8, String>,
    /// The part kept per preset, a single video's only.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub trims: std::collections::BTreeMap<u8, Trim>,
}

impl JobOptions {
    fn for_preset(&self, preset: u8) -> Choices {
        Choices {
            format: self.format,
            cover: self.cover,
            name: self.names.get(&preset).cloned(),
            trim: self.trims.get(&preset).copied(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub presets: Vec<u8>,
    pub target: Target,
    #[serde(flatten)]
    pub state: JobState,
    pub rows: Vec<Row>,
    /// A job written before there were choices has the defaults.
    #[serde(default)]
    pub options: JobOptions,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobsView {
    pub jobs: Vec<Job>,
}

pub type Resolver =
    Arc<dyn Fn(Target) -> Pin<Box<dyn Future<Output = Result<Resolved, TargetError>> + Send>> + Send + Sync>;

/// The resolver of the real thing: the files plugin's roots, read when the job
/// starts, and the kernel's mount table.
pub fn resolver_for(roots_path: PathBuf, deadline: std::time::Duration) -> Resolver {
    Arc::new(move |target| {
        let roots_path = roots_path.clone();
        Box::pin(async move {
            let path = roots_path.clone();
            let roots = crate::targets::blocking(deadline, move || {
                Roots::load(&path).map_err(|e| std::io::Error::other(e.to_string()))
            })
            .await?;
            resolve_parts(&target, &roots, &read_proc_mounts(), deadline).await
        })
    })
}

struct Queued {
    id: JobId,
    cancel: Arc<Cancel>,
}

struct Inner {
    view: Arc<Mutex<JobsView>>,
    path: PathBuf,
    /// Serialises the writes of `jobs.json`: each one carries the view as it
    /// is when it gets the lock, so the last write is always the newest.
    write: Mutex<()>,
    next_id: AtomicU64,
    cancels: Mutex<HashMap<JobId, Arc<Cancel>>>,
    tx: mpsc::UnboundedSender<Queued>,
    /// Held for reading while a job runs: yt-dlp is never swapped under one.
    gate: ToolGate,
}

pub struct Jobs {
    inner: Arc<Inner>,
}

impl Inner {
    fn with_job<T>(&self, id: JobId, f: impl FnOnce(&mut Job) -> T) -> Option<T> {
        let mut view = self.view.lock().unwrap_or_else(|p| p.into_inner());
        view.jobs.iter_mut().find(|j| j.id == id).map(f)
    }

    /// Every job not over, and the last [`KEPT_JOBS`] that are, written
    /// atomically. An unfinished job is never dropped: the queue may be longer
    /// than twenty. A failure is logged: the view in memory is still right.
    fn persist(&self) {
        let _writing = self.write.lock().unwrap_or_else(|p| p.into_inner());
        let text = {
            let mut view = self.view.lock().unwrap_or_else(|p| p.into_inner());
            let mut over = view.jobs.iter().filter(|j| j.state.is_over()).count();
            view.jobs.retain(|j| {
                if j.state.is_over() && over > KEPT_JOBS {
                    over -= 1;
                    false
                } else {
                    true
                }
            });
            serde_json::to_string(&*view)
        };
        match text {
            Ok(t) => {
                if let Err(e) = write_atomically(&self.path, t.as_bytes()) {
                    tracing::warn!("jobs.json not written: {e:#}");
                }
            }
            Err(e) => tracing::warn!("jobs.json not serialised: {e}"),
        }
    }
}

impl Jobs {
    /// Starts the worker. Needs a running tokio runtime. Jobs left running by
    /// a previous run are marked failed: nothing is resumed.
    pub fn start(
        data_dir: &std::path::Path,
        entries: Arc<AsyncRwLock<Entries>>,
        ctx: Ctx,
        resolver: Resolver,
    ) -> Self {
        let path = data_dir.join("jobs.json");
        let mut view: JobsView =
            std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        for job in &mut view.jobs {
            if !job.state.is_over() {
                job.state = JobState::Failed(reason::INTERRUPTED.into());
            }
            for row in &mut job.rows {
                if matches!(row.state, VideoState::Running | VideoState::Converting) {
                    row.state = VideoState::Failed(reason::INTERRUPTED.into());
                }
            }
        }
        let next_id = view.jobs.iter().map(|j| j.id).max().map_or(1, |m| m + 1);
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            view: Arc::new(Mutex::new(view)),
            path,
            write: Mutex::new(()),
            next_id: AtomicU64::new(next_id),
            cancels: Mutex::new(HashMap::new()),
            tx,
            gate: ToolGate::default(),
        });
        tokio::spawn(worker(inner.clone(), rx, Arc::new(ctx), entries, resolver));
        Self { inner }
    }

    /// What an update of yt-dlp takes for writing before it swaps the tool:
    /// it then waits for the job in progress, and the next job waits for it.
    pub fn tool_gate(&self) -> ToolGate {
        self.inner.gate.clone()
    }

    /// The jobs, for reading. Never hold the lock across an await.
    pub fn view(&self) -> Arc<Mutex<JobsView>> {
        self.inner.view.clone()
    }

    pub fn enqueue(&self, presets: Vec<u8>, target: Target) -> JobId {
        self.enqueue_with(presets, target, JobOptions::default())
    }

    pub fn enqueue_with(&self, presets: Vec<u8>, target: Target, options: JobOptions) -> JobId {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let cancel = Cancel::new();
        self.inner.cancels.lock().unwrap_or_else(|p| p.into_inner()).insert(id, cancel.clone());
        {
            let mut view = self.inner.view.lock().unwrap_or_else(|p| p.into_inner());
            view.jobs.push(Job { id, presets, target, state: JobState::Queued, rows: Vec::new(), options });
        }
        // Off the caller's thread: it is an admin route's, and none may block.
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.persist());
        let _ = self.inner.tx.send(Queued { id, cancel });
        id
    }

    /// Forgets every job that is over, and keeps those queued or running: the
    /// operator's purge. Written off the caller's thread, like an enqueue.
    pub fn clear_finished(&self) {
        {
            let mut view = self.inner.view.lock().unwrap_or_else(|p| p.into_inner());
            view.jobs.retain(|j| !j.state.is_over());
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.persist());
    }

    /// Cancels a queued job, or kills the running one's whole process group.
    /// False when the job is unknown or already over.
    pub fn cancel(&self, id: JobId) -> bool {
        match self.inner.cancels.lock().unwrap_or_else(|p| p.into_inner()).get(&id) {
            Some(c) => {
                c.cancel();
                true
            }
            None => false,
        }
    }
}

async fn worker(
    inner: Arc<Inner>,
    mut rx: mpsc::UnboundedReceiver<Queued>,
    ctx: Arc<Ctx>,
    entries: Arc<AsyncRwLock<Entries>>,
    resolver: Resolver,
) {
    while let Some(queued) = rx.recv().await {
        let state = {
            let _tool_stays = inner.gate.read().await;
            run_job(&inner, &queued, &ctx, &entries, &resolver).await
        };
        inner.with_job(queued.id, |j| {
            if state == JobState::Cancelled {
                // What was never reached is not pending any more.
                for row in j.rows.iter_mut().filter(|r| r.state == VideoState::Pending) {
                    row.state = VideoState::Cancelled;
                }
            }
            j.state = state;
        });
        inner.cancels.lock().unwrap_or_else(|p| p.into_inner()).remove(&queued.id);
        inner.persist();
    }
}

async fn run_job(
    inner: &Arc<Inner>,
    queued: &Queued,
    ctx: &Ctx,
    entries: &Arc<AsyncRwLock<Entries>>,
    resolver: &Resolver,
) -> JobState {
    if queued.cancel.is_cancelled() {
        return JobState::Cancelled;
    }
    let Some((presets, target, options)) = inner.with_job(queued.id, |j| {
        j.state = JobState::Running;
        (j.presets.clone(), j.target.clone(), j.options.clone())
    }) else {
        return JobState::Failed(reason::JOB_LOST.into());
    };
    inner.persist();
    let share = {
        let (resolver, target) = (resolver.clone(), target.clone());
        move || -> crate::download::ShareFuture {
            let (resolver, target) = (resolver.clone(), target.clone());
            Box::pin(async move { resolver(target).await.map(|_| ()) })
        }
    };
    let resolved = match resolver(target).await {
        Ok(r) => r,
        Err(e) => return JobState::Failed(reason::target_error(&e)),
    };
    for preset in presets {
        let entry = entries.read().await.by_preset(preset).cloned();
        let Some(entry) = entry else {
            let row = Row { preset, id: String::new(), title: String::new(), state: VideoState::Failed(reason::NO_SUCH_PRESET.into()), percent: None, notes: Vec::new() };
            inner.with_job(queued.id, |j| j.rows.push(row));
            inner.persist();
            continue;
        };
        let mut base = 0usize;
        let mut sink = |event: Event| {
            let persist = matches!(event, Event::Planned(_) | Event::Converting(_) | Event::Ended(..));
            inner.with_job(queued.id, |job| match event {
                Event::Planned(planned) => {
                    base = job.rows.len();
                    job.rows.extend(planned.into_iter().map(|p| Row {
                        preset,
                        id: p.id,
                        title: p.title,
                        state: match p.action {
                            Action::Skip(reason) => VideoState::Skipped(reason),
                            Action::Download { .. } => VideoState::Pending,
                        },
                        percent: None,
                        notes: Vec::new(),
                    }));
                }
                Event::Started(i) => {
                    if let Some(r) = job.rows.get_mut(base + i) {
                        r.state = VideoState::Running;
                        r.percent = Some(0.0);
                    }
                }
                Event::Progress(i, percent) => {
                    if let Some(r) = job.rows.get_mut(base + i) {
                        r.percent = Some(percent);
                    }
                }
                Event::Converting(i) => {
                    if let Some(r) = job.rows.get_mut(base + i) {
                        r.state = VideoState::Converting;
                        r.percent = None;
                    }
                }
                Event::Noted(i, note) => {
                    if let Some(r) = job.rows.get_mut(base + i) {
                        r.notes.push(note);
                    }
                }
                Event::Ended(i, state) => {
                    if let Some(r) = job.rows.get_mut(base + i) {
                        r.percent = matches!(state, VideoState::Done).then_some(100.0);
                        r.state = state;
                    }
                }
            });
            if persist {
                inner.persist();
            }
        };
        match download_preset(ctx, &entry, &options.for_preset(preset), &resolved, &share, &queued.cancel, &mut sink).await {
            PresetEnd::Finished => {}
            PresetEnd::Cancelled => return JobState::Cancelled,
            // Every row of this preset already says why; the presets after
            // it are not tried.
            PresetEnd::ShareLost(message) => return JobState::Failed(message),
            PresetEnd::Failed(message) => {
                // One row per video, never a duplicate summary: when the
                // preset had already planned its videos (the folder could not
                // be made, say), those still-pending rows are the ones that
                // failed. Only a preset that never got that far (probe
                // failed, nothing cached) has no row of its own: it gets one.
                inner.with_job(queued.id, |j| {
                    let mut marked = false;
                    for row in j.rows.iter_mut().filter(|r| r.preset == preset && r.state == VideoState::Pending) {
                        row.state = VideoState::Failed(message.clone());
                        marked = true;
                    }
                    if !marked {
                        j.rows.push(Row { preset, id: String::new(), title: entry.title.clone(), state: VideoState::Failed(message), percent: None, notes: Vec::new() });
                    }
                });
                inner.persist();
            }
        }
    }
    JobState::Done
}

#[cfg(test)]
mod tests;
