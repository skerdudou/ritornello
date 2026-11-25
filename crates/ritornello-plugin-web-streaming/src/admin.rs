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
//! | `probe` | parses the lines (pure) and queues them | one worker unless one runs: yt-dlp, a line at a time; adds what names one thing through `Store::change` |
//! | `choose` | adds a choice of a `found` line through `Store::change`, drops the line | nothing |
//! | `dismiss` | drops a line, whatever its state | nothing |
//! | `remove`, `move`, `rename` | one change of the table through `Store::change`: validates, writes `entries.toml` and the playlist caches | nothing |
//! | `list_targets` | nothing | read the share roots and `/proc/mounts` (also spawned by `get_data` when the cache is stale) |
//! | `list_dirs`, `create_dir` | nothing | the share's I/O, each under a deadline |
//! | `download` | checks ffmpeg for mp3 (the last probe, in memory), queues the job (no I/O on this thread) | settings write |
//! | `cancel` | flips the job's flag | nothing |
//! | `update_tool` | flags the request | nothing (the scheduler runs it) |
//! | `set_channel` | nothing | settings write, then an update request |
//! | `reprobe` | nothing | the machine probe |
//!
//! The one thing a table change does synchronously is local file writes of a few
//! kilobytes, each under its own deadline.

mod explore;
mod probe;
mod store;
#[cfg(test)]
mod tests;

use explore::Explore;
use probe::ProbeState;
use store::Store;
use ritornello_plugin_web_streaming::reason;
use ritornello_files_mount::mounts::read_proc_mounts;
use ritornello_plugin_sdk::AdminPlugin;
use ritornello_plugin_web_streaming::entries::{Entries, Entry};
use ritornello_plugin_web_streaming::download::{CoverMode, Format, Trim, default_name};
use ritornello_plugin_web_streaming::jobs::{JobOptions, Jobs};
use ritornello_plugin_web_streaming::settings::Channel;
use ritornello_plugin_web_streaming::targets::{files_roots_path, Target};
use ritornello_plugin_web_streaming::playfail::PlaybackFailure;
use ritornello_plugin_web_streaming::tool::Tool;
use ritornello_plugin_web_streaming::ytdlp::{Kind, Ytdlp};
use ritornello_proto::Text;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock as AsyncRwLock;

/// How long a table change may take, its wait for the store's edit lock and
/// its local file writes together: under the 5 s ceiling of `GetData`, which
/// queues behind a `SetData` (it is exclusive). The probe worker changes the
/// table in the background, so a page's change may find the lock taken: it
/// then fails with `save_failed` rather than wait and write for two
/// deadlines. A blocking write cannot be cancelled: after a timeout it may
/// still complete, leaving the disk ahead of the memory (and of the page)
/// until the next change or load; it keeps the edit lock until it ends, so no
/// later change is overwritten by it.
const SAVE_DEADLINE: Duration = Duration::from_secs(4);
/// How many links one `probe` takes: they are probed one after the other.
pub const MAX_LINKS: usize = 20;
/// The settings key holding the last download target.
const LAST_TARGET: &str = "last_target";
/// The settings keys holding the last format and cover choice.
const LAST_FORMAT: &str = "last_format";
const LAST_COVER: &str = "last_cover";

/// A preset as the page drew it: its number and its address.
#[derive(Debug, Deserialize)]
struct Pick {
    preset: u8,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// Reads the links, one per line, queued behind those already being
    /// read; the answers arrive through `get_data`.
    Probe { text: String },
    /// Adds the choice `url` of the `found` line `line`, which then goes
    /// (`stale` when the line or the choice is not there).
    Choose { line: u64, url: String },
    /// Drops a line, whatever its state (none of it is added afterwards).
    Dismiss { line: u64 },
    /// Drops a preset; those after it move up. `preset` and `url` name the row
    /// as the page drew it (`stale` when the table moved meanwhile).
    Remove { preset: u8, url: String },
    /// Puts a preset at the 1-based position `to`.
    Move { preset: u8, url: String, to: u8 },
    /// Retitles a preset (trimmed, never blank).
    Rename { preset: u8, url: String, title: String },
    /// Re-reads where a download may go.
    ListTargets,
    ListDirs { target: Target },
    CreateDir { target: Target, name: String },
    /// Downloads the presets `items` name by number and address (`stale`
    /// when the table moved since the page drew them).
    Download {
        items: Vec<Pick>,
        target: Target,
        #[serde(default)]
        format: Format,
        #[serde(default)]
        cover: CoverMode,
        /// The name typed per preset, by its number as a string (a JSON
        /// key; the internally tagged enum buffers the request, and a number
        /// key would not be read back from that buffer).
        #[serde(default)]
        names: BTreeMap<String, String>,
        /// The part kept per preset, keyed the same way; a single video's
        /// only.
        #[serde(default)]
        trims: BTreeMap<String, Trim>,
    },
    Cancel { job: u64 },
    /// Forgets the downloads that are over; those queued or running stay.
    ClearJobs,
    /// `force` takes a release the rollback guard refused.
    UpdateTool {
        #[serde(default)]
        force: bool,
    },
    SetChannel { channel: Channel },
    /// Looks at the machine again (python3, JavaScript runtime).
    Reprobe,
    /// Forgets the last playback failure the page shows.
    ClearFailure,
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
    /// Every change of the table; shared with the probe task.
    store: Arc<Store>,
    env: Arc<Environment>,
    shared: Arc<Shared>,
}

fn keyed(key: &str) -> Text {
    Text::Keyed { key: key.into(), params: HashMap::new() }
}

fn keyed_with(key: &str, param: &str, value: impl ToString) -> Text {
    Text::Keyed { key: key.into(), params: HashMap::from([(param.to_string(), value.to_string())]) }
}

/// A map the page keys by preset number as a string (see `Op::Download`),
/// keyed by the number.
fn by_preset<V>(map: BTreeMap<String, V>) -> Result<BTreeMap<u8, V>, Text> {
    map.into_iter()
        .map(|(preset, value)| preset.parse::<u8>().map(|p| (p, value)))
        .collect::<Result<_, _>>()
        .map_err(|e| keyed_with("bad_request", "detail", e))
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
            store: Arc::new(Store::new(data.clone(), entries.clone(), saved)),
            data,
            entries,
            tool,
            jobs,
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

    /// `table` is the presets as read once: the picks are judged against it
    /// and the job keeps the picked rows from it.
    fn download(&self, items: Vec<Pick>, target: Target, mut options: JobOptions, table: &[Entry]) -> Result<(), Text> {
        if items.is_empty() {
            return Err(keyed("nothing_selected"));
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(twice) = items.iter().find(|p| !seen.insert(p.preset)) {
            return Err(keyed_with("duplicate_preset", "preset", twice.preset));
        }
        let mut picked = Vec::with_capacity(items.len());
        for item in &items {
            let row = table.iter().find(|e| e.preset == item.preset && e.url == item.url).ok_or_else(|| keyed("stale"))?;
            picked.push(row.clone());
        }
        let presets: Vec<u8> = picked.iter().map(|e| e.preset).collect();
        let playlists: Vec<u8> = table.iter().filter(|e| e.kind == Kind::Playlist).map(|e| e.preset).collect();
        // mp3 is only ever a conversion by ffmpeg: not offered without it,
        // and refused here whatever the page sent. (m4a goes through: the
        // site's own m4a needs no ffmpeg, and the row says when the original
        // had to be kept.)
        if options.format == Format::Mp3 && self.tool.capabilities().ffmpeg_path.is_none() {
            return Err(keyed("ffmpeg_missing"));
        }
        // A cut that keeps everything is none; one of a preset not asked
        // for is dropped. What is left must be a single video's, keep
        // something, and find ffmpeg: never a whole download that ends up
        // not cut, nor an empty file.
        options.trims.retain(|preset, trim| presets.contains(preset) && !trim.is_whole());
        if let Some(preset) = options.trims.keys().find(|p| playlists.contains(p)) {
            return Err(keyed_with("trim_playlist", "preset", preset));
        }
        if let Some((preset, _)) = options.trims.iter().find(|(_, t)| t.is_empty()) {
            return Err(keyed_with("trim_empty", "preset", preset));
        }
        if !options.trims.is_empty() && self.tool.capabilities().ffmpeg_path.is_none() {
            return Err(keyed("trim_needs_ffmpeg"));
        }
        let (format, cover) = (options.format, options.cover);
        self.jobs.enqueue_with(picked, target.clone(), options);
        // Remembered for the next time; the write is off this thread.
        let tool = self.tool.clone();
        tokio::task::spawn_blocking(move || {
            let values = (toml::Value::try_from(&target), toml::Value::try_from(format), toml::Value::try_from(cover));
            let (Ok(target), Ok(format), Ok(cover)) = values else {
                return tracing::warn!("last download choices not serialised: {values:?}");
            };
            if let Err(e) = tool.edit_settings(|s| {
                s.other.insert(LAST_TARGET.to_string(), target);
                s.other.insert(LAST_FORMAT.to_string(), format);
                s.other.insert(LAST_COVER.to_string(), cover);
            }) {
                tracing::warn!("last download choices not remembered: {e:#}");
            }
        });
        Ok(())
    }

    /// The last format and cover choice, the defaults when none was made
    /// (or one this version does not know).
    fn last_choices(&self) -> (Format, CoverMode) {
        let other = self.tool.settings().other;
        let format = other.get(LAST_FORMAT).cloned().and_then(|v| v.try_into().ok()).unwrap_or_default();
        let cover = other.get(LAST_COVER).cloned().and_then(|v| v.try_into().ok()).unwrap_or_default();
        (format, cover)
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
        // The snapshot is the job's own, not the page's.
        if let Some(fields) = job.as_object_mut() {
            fields.remove("entries");
        }
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
        // The lines before the table: a line that becomes a preset in between
        // then shows as both (the next read drops the line), never as neither
        // with `running` false, which would stop the page's polling.
        let probe = self.shared.probe.lock().unwrap_or_else(|p| p.into_inner()).view();
        let mut entries = self.entries.read().await.entry.clone();
        entries.sort_by_key(|e| e.preset);
        // The names a download would give, shown and editable in the dialog:
        // computed from the saved presets, no network.
        let default_names: BTreeMap<String, String> =
            entries.iter().map(|e| (e.preset.to_string(), default_name(e))).collect();
        let (last_format, last_cover) = self.last_choices();
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
            "last_format": last_format,
            "last_cover": last_cover,
            "default_names": default_names,
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
            Op::Probe { text } => probe::start(&self.shared, &self.ytdlp, &self.store, &text),
            Op::Choose { line, url } => {
                let found = self.shared.probe.lock().unwrap_or_else(|p| p.into_inner()).choice(line, &url);
                let found = found.ok_or_else(|| keyed("stale"))?;
                probe::add(&self.shared.probe, &self.store, line, found).await
            }
            Op::Dismiss { line } => {
                self.shared.probe.lock().unwrap_or_else(|p| p.into_inner()).dismiss(line);
                Ok(())
            }
            Op::Remove { preset, url } => {
                self.store
                    .change(&self.shared.probe, |rows| {
                        let at = store::find(rows, preset, &url)?;
                        rows.remove(at);
                        Ok(())
                    })
                    .await
            }
            Op::Move { preset, url, to } => {
                self.store
                    .change(&self.shared.probe, |rows| {
                        let at = store::find(rows, preset, &url)?;
                        let to = usize::from(to);
                        if !(1..=rows.len()).contains(&to) {
                            return Err(keyed_with("bad_request", "detail", format!("position {to} is outside 1 to {}", rows.len())));
                        }
                        let row = rows.remove(at);
                        rows.insert(to - 1, row);
                        Ok(())
                    })
                    .await
            }
            Op::Rename { preset, url, title } => {
                self.store
                    .change(&self.shared.probe, |rows| {
                        let at = store::find(rows, preset, &url)?;
                        let title = title.trim();
                        if title.is_empty() {
                            return Err(keyed("empty_title"));
                        }
                        rows[at].title = title.to_string();
                        Ok(())
                    })
                    .await
            }
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
            Op::Download { items, target, format, cover, names, trims } => {
                let names = by_preset(names)?;
                let trims = by_preset(trims)?;
                // The table read once: the picks are judged against it, the
                // job keeps its rows, and a cut is a single video's by its kinds.
                let table = self.entries.read().await.entry.clone();
                self.download(items, target, JobOptions { format, cover, names, trims }, &table)
            }
            Op::Cancel { job } => {
                // A job that ended a moment ago is not an error.
                self.jobs.cancel(job);
                Ok(())
            }
            Op::ClearJobs => {
                self.jobs.clear_finished();
                Ok(())
            }
            Op::ClearFailure => {
                // One small file of the plugin's own data directory, under the
                // same deadline `get_data` reads it with: never a stalled page.
                let data = self.data.clone();
                let removal = tokio::task::spawn_blocking(move || PlaybackFailure::clear(&data));
                match tokio::time::timeout(FAILURE_READ_DEADLINE, removal).await {
                    Ok(Ok(Ok(()))) => Ok(()),
                    Ok(Ok(Err(e))) => Err(keyed_with("clear_failure_failed", "detail", e.to_string())),
                    _ => Err(keyed_with("clear_failure_failed", "detail", "timed out".to_string())),
                }
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
