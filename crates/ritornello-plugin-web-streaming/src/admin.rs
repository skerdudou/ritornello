//! The admin half: the page's data and its operations.
//!
//! **No operation waits for yt-dlp, the network, a share or a job.** The
//! admin protocol gives `GetData` five seconds and `SetData` thirty, `SetData`
//! is exclusive (a slow one holds back every `GetData` behind it), and an I/O
//! left without a deadline has already made a page disappear on this project.
//! So every slow thing is spawned, its progress lives in [`Shared`], and the
//! page reads it back through `get_data` while it polls.
//!
//! | op | does, in the call | spawns |
//! |---|---|---|
//! | `probe` | parses the lines (pure) | one task: yt-dlp, a line at a time |
//! | `save` | validates, writes `entries.toml` and the playlist caches | nothing |
//! | `list_targets` | nothing | read the share roots and `/proc/mounts` (also spawned by `get_data` when the cache is stale) |
//! | `list_dirs`, `create_dir` | nothing | the share's I/O, each under a deadline |
//! | `download` | queues the job (no I/O on this thread) | settings write |
//! | `cancel` | flips the job's flag | nothing |
//! | `update_tool` | flags the request | nothing (the scheduler runs it) |
//! | `set_channel` | nothing | settings write, then an update request |
//! | `reprobe` | nothing | the machine probe |
//!
//! The one thing `save` does synchronously is local file writes of a few
//! kilobytes, each under its own deadline.

mod explore;
mod probe;
#[cfg(test)]
mod tests;

use explore::Explore;
use probe::ProbeState;
use ritornello_plugin_web_streaming::reason;
use ritornello_files_mount::mounts::read_proc_mounts;
use ritornello_plugin_sdk::AdminPlugin;
use ritornello_plugin_web_streaming::entries::{Entries, Entry, ListCache};
use ritornello_plugin_web_streaming::jobs::Jobs;
use ritornello_plugin_web_streaming::link::parse_link;
use ritornello_plugin_web_streaming::settings::Channel;
use ritornello_plugin_web_streaming::targets::{self, files_roots_path, Target};
use ritornello_plugin_web_streaming::playfail::PlaybackFailure;
use ritornello_plugin_web_streaming::tool::Tool;
use ritornello_plugin_web_streaming::ytdlp::{Kind, Ytdlp};
use ritornello_proto::Text;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock as AsyncRwLock;

/// How long `save` waits for its local file writes: under the 5 s ceiling of
/// `GetData`, which queues behind a `SetData` (it is exclusive). A blocking
/// write cannot be cancelled: after a timeout it may still complete, leaving
/// the disk ahead of the memory (and of the page) until the next load.
const SAVE_DEADLINE: Duration = Duration::from_secs(4);
/// How many links one `probe` takes: they are probed one after the other.
pub const MAX_LINKS: usize = 20;
/// The settings key holding the last download target.
const LAST_TARGET: &str = "last_target";

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// Reads the links, one per line; the answers arrive through `get_data`.
    Probe { text: String },
    /// Saves the whole table.
    Save { entries: Vec<Entry> },
    /// Re-reads where a download may go.
    ListTargets,
    ListDirs { target: Target },
    CreateDir { target: Target, name: String },
    Download { presets: Vec<u8>, target: Target },
    Cancel { job: u64 },
    /// `force` takes a release the rollback guard refused.
    UpdateTool {
        #[serde(default)]
        force: bool,
    },
    SetChannel { channel: Channel },
    /// Looks at the machine again (python3, JavaScript runtime).
    Reprobe,
}

/// What the spawned tasks leave for `get_data`. Plain `std` locks, never held
/// across an await.
#[derive(Default)]
pub(crate) struct Shared {
    pub probe: Mutex<ProbeState>,
    pub targets: Mutex<TargetsCache>,
    pub explore: Mutex<Explore>,
    pub reprobing: AtomicBool,
}

/// The shares a download may go to, as last read, and when.
#[derive(Default)]
pub(crate) struct TargetsCache {
    list: Vec<String>,
    fetched: Option<std::time::Instant>,
    /// A refresh is running: no second one is started.
    refreshing: bool,
    /// After a failed read: not before, so a roots file that cannot be read
    /// is not re-read by every poll of the page.
    retry_at: Option<std::time::Instant>,
}

/// How long a failed read of the shares waits before the next one.
const TARGETS_RETRY: Duration = Duration::from_secs(5);

/// What the environment hands the admin half, apart from the files.
pub struct Environment {
    /// The files plugin's declared roots.
    pub roots_path: PathBuf,
    /// The kernel's mount table, as text.
    pub mounts: Arc<dyn Fn() -> String + Send + Sync>,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// How old the shares may be before `get_data` has them read again: a
    /// share is mounted by the files plugin after this one starts, and
    /// unmounted whenever.
    pub targets_max_age: Duration,
}

impl Environment {
    pub fn real(data: &std::path::Path) -> Self {
        Self {
            roots_path: files_roots_path(data),
            mounts: Arc::new(read_proc_mounts),
            arch: std::env::consts::ARCH.to_string(),
            targets_max_age: Duration::from_secs(30),
        }
    }
}

pub struct WebStreamingAdmin {
    data: PathBuf,
    entries: Arc<AsyncRwLock<Entries>>,
    ytdlp: Arc<Ytdlp>,
    tool: Tool,
    jobs: Jobs,
    /// Ticks after each successful save: the source half announces the new
    /// table to the core.
    saved: tokio::sync::watch::Sender<()>,
    env: Arc<Environment>,
    shared: Arc<Shared>,
}

fn keyed(key: &str) -> Text {
    Text::Keyed { key: key.into(), params: HashMap::new() }
}

fn keyed_with(key: &str, param: &str, value: impl ToString) -> Text {
    Text::Keyed { key: key.into(), params: HashMap::from([(param.to_string(), value.to_string())]) }
}

impl WebStreamingAdmin {
    /// Needs a running tokio runtime.
    pub fn new(
        data: PathBuf,
        entries: Arc<AsyncRwLock<Entries>>,
        tool: Tool,
        jobs: Jobs,
        saved: tokio::sync::watch::Sender<()>,
        env: Environment,
    ) -> Self {
        Self {
            ytdlp: Arc::new(Ytdlp::new(&data)),
            data,
            entries,
            tool,
            jobs,
            saved,
            env: Arc::new(env),
            shared: Arc::new(Shared::default()),
        }
    }

    /// Reads the shares again, in a task: never on the caller's thread (the
    /// roots file and the mount table are read under a deadline, off any
    /// admin route). One at a time.
    fn refresh_targets(&self) {
        {
            let mut cache = self.shared.targets.lock().unwrap_or_else(|p| p.into_inner());
            if cache.refreshing {
                return;
            }
            cache.refreshing = true;
        }
        let (shared, env) = (self.shared.clone(), self.env.clone());
        tokio::spawn(async move {
            let found = explore::targets_now(&env).await;
            let mut cache = shared.targets.lock().unwrap_or_else(|p| p.into_inner());
            cache.refreshing = false;
            match found {
                Some(list) => {
                    cache.list = list;
                    cache.fetched = Some(std::time::Instant::now());
                    cache.retry_at = None;
                }
                // A read that failed says nothing about the shares: what
                // was known stays, and the read is tried again soon.
                None => cache.retry_at = Some(std::time::Instant::now() + TARGETS_RETRY),
            }
        });
    }

    /// The last target, when its share is still one a download may go to.
    fn last_target(&self, targets: &[String]) -> Option<Target> {
        let value = self.tool.settings().other.get(LAST_TARGET)?.clone();
        let target: Target = value.try_into().ok()?;
        targets.contains(&target.root).then_some(target)
    }

    async fn save(&self, entries: Vec<Entry>) -> Result<(), Text> {
        let table = Entries { entry: entries };
        table.validate().map_err(|e| match e {
            // Past 99 it is one preset too many; 0 is a number that does not exist.
            ritornello_plugin_web_streaming::entries::ValidationError::PresetOutOfRange { preset, .. } if preset > 99 => {
                keyed("too_many_entries")
            }
            ritornello_plugin_web_streaming::entries::ValidationError::PresetOutOfRange { preset, .. } => {
                keyed_with("preset_out_of_range", "preset", preset)
            }
            ritornello_plugin_web_streaming::entries::ValidationError::DuplicatePreset { preset } => {
                keyed_with("duplicate_preset", "preset", preset)
            }
        })?;
        for e in &table.entry {
            // A YouTube link must name what the preset says; another site's
            // link names whatever its probe found, so it fits either kind.
            let fits = parse_link(&e.url).ok().is_some_and(|l| match e.kind {
                Kind::Video => l.watch_url().is_some(),
                Kind::Playlist => l.cache_key().is_some(),
            });
            if !fits {
                return Err(keyed("bad_entry"));
            }
        }
        // The playlists the operator just probed are cached by this very
        // save: the first `select` of a new playlist never meets an empty
        // cache (which would refuse it, and refresh in the background).
        let probed = self.shared.probe.lock().unwrap_or_else(|p| p.into_inner()).lists_for(&table);
        let (data, path) = (self.data.clone(), self.data.join("entries.toml"));
        let to_write = table.clone();
        let written = targets::blocking(SAVE_DEADLINE, move || {
            let cache = ListCache::new(&data);
            for (key, videos) in &probed {
                if let Err(e) = cache.store(key, videos) {
                    // The select then refreshes the cache by itself.
                    tracing::warn!("playlist {key} not cached: {e:#}");
                }
            }
            to_write.save(&path).map_err(|e| std::io::Error::other(format!("{e:#}")))
        })
        .await;
        if let Err(e) = written {
            // The path and the cause stay in the log, not on the page.
            tracing::warn!("entries not saved: {e}");
            return Err(keyed("save_failed"));
        }
        // Stored now: their copy in memory is not needed any more.
        self.shared.probe.lock().unwrap_or_else(|p| p.into_inner()).forget_lists_of(&table);
        *self.entries.write().await = table;
        // On every successful save, as the radio does: the core's own merge
        // drops what did not change.
        self.saved.send_replace(());
        Ok(())
    }

    fn download(&self, presets: Vec<u8>, target: Target) -> Result<(), Text> {
        if presets.is_empty() {
            return Err(keyed("nothing_selected"));
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(twice) = presets.iter().find(|p| !seen.insert(**p)) {
            return Err(keyed_with("duplicate_preset", "preset", twice));
        }
        self.jobs.enqueue(presets, target.clone());
        // Remembered for the next time; the write is off this thread.
        let tool = self.tool.clone();
        tokio::task::spawn_blocking(move || {
            let value = match toml::Value::try_from(&target) {
                Ok(v) => v,
                Err(e) => return tracing::warn!("last target not serialised: {e}"),
            };
            if let Err(e) = tool.edit_settings(|s| {
                s.other.insert(LAST_TARGET.to_string(), value);
            }) {
                tracing::warn!("last target not remembered: {e:#}");
            }
        });
        Ok(())
    }
}

/// The jobs as the page reads them: what the plugin wrote in a failure or a
/// skip becomes a code to translate, what yt-dlp or the system said stays
/// as `detail` (see `reason`).
fn jobs_for_page(jobs: &[ritornello_plugin_web_streaming::jobs::Job]) -> serde_json::Value {
    let mut value = serde_json::to_value(jobs).unwrap_or_default();
    let code_the_detail = |item: &mut serde_json::Value| {
        let failed_or_skipped = matches!(item["state"].as_str(), Some("failed" | "skipped"));
        if let (true, Some(text)) = (failed_or_skipped, item["detail"].as_str().map(str::to_string)) {
            item["detail"] = serde_json::to_value(reason::coded(&text)).unwrap_or_default();
        }
    };
    for job in value.as_array_mut().into_iter().flatten() {
        code_the_detail(job);
        for row in job["rows"].as_array_mut().into_iter().flatten() {
            code_the_detail(row);
        }
    }
    value
}

/// How long `get_data` waits for the record of the last failed playback, a
/// small file of the plugin's own data directory: far under the route's
/// ceiling, and nothing (rather than a stalled page) when it is not met.
const FAILURE_READ_DEADLINE: Duration = Duration::from_millis(500);

impl WebStreamingAdmin {
    /// The last playback that failed (`playfail`), read off the route's
    /// task under [`FAILURE_READ_DEADLINE`].
    async fn last_failure(&self) -> Option<PlaybackFailure> {
        let data = self.data.clone();
        let read = tokio::task::spawn_blocking(move || PlaybackFailure::load(&data));
        tokio::time::timeout(FAILURE_READ_DEADLINE, read).await.ok()?.ok()?
    }
}

#[async_trait::async_trait]
impl AdminPlugin for WebStreamingAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string())),
            "ui.css" => Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string())),
            _ => None,
        }
    }

    async fn get_data(&self) -> serde_json::Value {
        let mut entries = self.entries.read().await.entry.clone();
        entries.sort_by_key(|e| e.preset);
        let (targets, stale) = {
            let cache = self.shared.targets.lock().unwrap_or_else(|p| p.into_inner());
            let old = cache.fetched.is_none_or(|at| at.elapsed() >= self.env.targets_max_age);
            let stale = old && cache.retry_at.is_none_or(|at| std::time::Instant::now() >= at);
            (cache.list.clone(), stale)
        };
        // Spawned, never read here: the answer of this call is the cache.
        if stale {
            self.refresh_targets();
        }
        let probe = self.shared.probe.lock().unwrap_or_else(|p| p.into_inner()).view();
        let explore = self.shared.explore.lock().unwrap_or_else(|p| p.into_inner()).view();
        let jobs = jobs_for_page(&self.jobs.view().lock().unwrap_or_else(|p| p.into_inner()).jobs);
        let caps = self.tool.capabilities();
        let hopeless = caps.js_runtime.is_none() && ritornello_plugin_web_streaming::tool::runtime::own_runtime(&self.env.arch).is_none();
        let mut capabilities = serde_json::to_value(&caps).unwrap_or_default();
        capabilities["checking"] = self.shared.reprobing.load(Ordering::SeqCst).into();
        // None found, and the plugin installs none of its own on this
        // machine (deno on x86_64 and aarch64, node 22 on armv7): the page
        // must not suggest that one is on its way.
        capabilities["js_hopeless"] = hopeless.into();
        let mut data = serde_json::json!({
            "entries": entries,
            "probe": probe,
            "last_target": self.last_target(&targets),
            "targets": targets,
            "explore": explore,
            "jobs": jobs,
            "tool": self.tool.status(),
            "capabilities": capabilities,
        });
        // Awaited last, once every other part was taken: the state shown is
        // one instant's, not one from before this read and one from after.
        data["last_failure"] = serde_json::to_value(self.last_failure().await).unwrap_or_default();
        data
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), Text> {
        let op: Op =
            serde_json::from_value(data).map_err(|e| keyed_with("bad_request", "detail", e.to_string()))?;
        match op {
            Op::Probe { text } => probe::start(&self.shared, &self.ytdlp, &text),
            Op::Save { entries } => self.save(entries).await,
            Op::ListTargets => {
                self.refresh_targets();
                Ok(())
            }
            Op::ListDirs { target } => {
                explore::list_dirs(&self.shared, &self.env, target);
                Ok(())
            }
            Op::CreateDir { target, name } => {
                explore::create_dir(&self.shared, &self.env, target, name);
                Ok(())
            }
            Op::Download { presets, target } => self.download(presets, target),
            Op::Cancel { job } => {
                // A job that ended a moment ago is not an error.
                self.jobs.cancel(job);
                Ok(())
            }
            Op::UpdateTool { force } => {
                self.tool.request_update(force);
                Ok(())
            }
            Op::SetChannel { channel } => {
                let tool = self.tool.clone();
                tokio::spawn(async move {
                    let edit = {
                        let tool = tool.clone();
                        tokio::task::spawn_blocking(move || tool.edit_settings(|s| s.channel = channel)).await
                    };
                    match edit {
                        Ok(Ok(())) => tool.request_update(false),
                        Ok(Err(e)) => tracing::warn!("channel not saved: {e:#}"),
                        Err(e) => tracing::warn!("channel not saved: {e}"),
                    }
                });
                Ok(())
            }
            Op::Reprobe => {
                if !self.shared.reprobing.swap(true, Ordering::SeqCst) {
                    let (tool, shared) = (self.tool.clone(), self.shared.clone());
                    tokio::spawn(async move {
                        tool.recheck().await;
                        shared.reprobing.store(false, Ordering::SeqCst);
                    });
                }
                Ok(())
            }
        }
    }
}
