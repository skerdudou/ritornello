//! Admin half: the page managing the roots and the playback list.
//!
//! It shares the roots table and the current list with the Source half,
//! behind asynchronous locks. The two halves run in separate tasks: a
//! failure here must never cut the audio.
//!
//! The admin protocol is **request/response** and pushes nothing. That is
//! why the scan is an asynchronous task whose progress the page polls,
//! rather than a stream of events.

use crate::state;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_files::m3u::Entry;
use ritornello_plugin_files::playlist::Playlist;
use ritornello_plugin_files::roots::{Root, RootKind, Roots};
use ritornello_plugin_files::health::Health;
use ritornello_plugin_files::store::{self, Location};
use ritornello_plugin_files::volumes;
use ritornello_plugin_files::{mount, scan};
use ritornello_plugin_sdk::AdminPlugin;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

/// Progress of the running scan, as the page reads it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanProgress {
    pub running: bool,
    pub found: usize,
    pub dir: String,
    /// Refusal or incident of the **last** scan. Kept after the end: it is
    /// the only way for the page to learn that an addition failed, the
    /// `add_dir` call having returned long before.
    pub error: Option<String>,
}

/// Progress of the duration probing, as the page reads it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DurationsProgress {
    pub running: bool,
    pub done: usize,
    pub total: usize,
}

/// How many tracks are probed before re-taking the lock.
///
/// Neither one by one — the lock would be taken thousands of times, competing
/// with playback — nor all at once, which would show no progress and lose
/// everything if the probing were aborted along the way.
const PROBE_BATCH: usize = 25;

pub struct FilesAdmin {
    pub roots_path: PathBuf,
    pub creds_dir: PathBuf,
    pub internal_playlists: PathBuf,
    pub state_path: PathBuf,
    pub roots: Arc<AsyncRwLock<Roots>>,
    pub playlist: Arc<AsyncRwLock<Playlist>>,
    pub catalog: Arc<RwLock<Catalog>>,
    pub scan: Arc<Mutex<ScanProgress>>,
    /// Running scan task. Launching a new one **aborts** the previous one:
    /// two clicks must not leave two concurrent walks saturating a slow
    /// share.
    pub scan_task: Option<tokio::task::JoinHandle<()>>,
    /// Entries of a loaded m3u that no rule could resolve. Reported to the
    /// page, never dropped silently.
    pub unresolved: Arc<Mutex<Vec<String>>>,
    /// Last folder content or search result requested by the page.
    ///
    /// `set_data` only returns an `Ok`/`Err`, with no payload: the content
    /// therefore travels through `get_data`, exactly like the directory
    /// search of the radio plugin stores its results before the page reads
    /// them back.
    pub browse: Arc<Mutex<serde_json::Value>>,
    /// Announces the preset count to the Source half as soon as it changes,
    /// without waiting for a track to be played — otherwise the grid of the
    /// web remote would keep the old set of numbers.
    pub preset_count_tx: tokio::sync::watch::Sender<u8>,
    /// The wizard in progress. Lives here rather than behind its own lock:
    /// only one dialog is open at a time, and the admin protocol is
    /// sequential.
    pub explore: ritornello_plugin_files::explore::Browser,
    /// Result of the last mount reconciliation.
    ///
    /// Mounting now follows the declaration: the user no longer clicks
    /// "Mount". A failure must therefore not get lost — without this field,
    /// a declared source would stay "not mounted" without ever saying why.
    pub mount_error: Arc<Mutex<Option<String>>>,
    /// Whether `smbclient` is usable. Probed at startup, re-probed on every
    /// connection attempt.
    pub smb_ok: Arc<std::sync::atomic::AtomicBool>,
    /// The list has changed since the Source half handed it to mpv.
    ///
    /// Shared with it: this is the only channel available, since SDK
    /// notifications cannot carry an action.
    pub playlist_changed: Arc<std::sync::atomic::AtomicBool>,
    /// Whether the Source half is playing right now.
    ///
    /// Lets the page decide whether clearing the list must also ask the core
    /// to stop: doing so while another source is playing would cut that one.
    pub plays: Arc<std::sync::atomic::AtomicBool>,
    /// Circuit breaker for the media paths.
    ///
    /// Every filesystem read triggered by an admin request **must** go
    /// through it. The admin protocol is serial and the core gives up after
    /// five seconds: a single `is_file` that never completes wedges the
    /// whole plugin, page included. See `health` for the measurement.
    pub health: Arc<Health>,
    /// Progress of the duration probing.
    pub durations: Arc<Mutex<DurationsProgress>>,
    /// Running probe. Launching a new one **abandons** the previous one:
    /// after loading a list, probing the old one is useless.
    pub durations_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Declares a source in a single gesture: the wizard has already
    /// collected everything, there is no table left to rewrite and no name
    /// to type.
    ///
    /// The passphrase only travels in that direction: `Root` does not carry
    /// it, so `get_data` cannot return it inadvertently, even if someone
    /// adds a field later.
    AddSource {
        kind: RootKind,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        host: String,
        #[serde(default)]
        share: String,
        #[serde(default)]
        subpath: Option<String>,
        #[serde(default)]
        user: String,
        #[serde(default)]
        domain: String,
        /// **Empty means "take the session's one, falling back to the one
        /// already saved"**. The page cannot send back a secret it never
        /// receives, and the wizard must not make the user retype it at
        /// confirmation when it was just used to connect.
        #[serde(default)]
        password: String,
        #[serde(default)]
        writable: bool,
    },
    RemoveSource {
        name: String,
    },
    SetWritable {
        name: String,
        writable: bool,
    },
    ExploreOpen {
        kind: ritornello_plugin_files::explore::Kind,
    },
    ExploreClose,
    ExploreLocal {
        path: String,
    },
    SmbConnect {
        host: String,
        #[serde(default)]
        user: String,
        #[serde(default)]
        password: String,
        #[serde(default)]
        domain: String,
    },
    SmbBrowse {
        share: String,
        #[serde(default)]
        path: String,
    },
    /// Back to the share list already obtained, without a new network call.
    SmbShares,
    Mount,
    Browse { root: String, #[serde(default)] path: String },
    Search { root: String, #[serde(default)] path: String, query: String },
    AddDir { root: String, #[serde(default)] path: String },
    AddFile { root: String, path: String },
    Remove { index: usize },
    Move { from: usize, to: usize },
    Clear,
    SavePlaylist { name: String, r#where: String },
    LoadPlaylist { name: String, r#where: String },
    /// Loads a `.m3u` **found while browsing a source**, designated by its
    /// path — as opposed to `LoadPlaylist`, which fetches a *saved* list by
    /// its name from a store.
    LoadM3u { root: String, path: String },
}

impl FilesAdmin {
    fn phrase(&self, key: &str) -> String {
        self.catalog.read().unwrap().get(key).to_string()
    }

    /// Resolves a **relative** path provided by the page against the named
    /// root, refusing anything that would escape it.
    ///
    /// This is the escape guard on the page side: `name` is already
    /// validated by `Roots`, but `path` comes from the browser with every
    /// request. A `../../etc` there would browse — and add to a playback
    /// list — files outside any declared root.
    async fn under_root(&self, root: &str, path: &str) -> Result<PathBuf, String> {
        let roots = self.roots.read().await;
        let r = roots
            .by_name(root)
            .ok_or_else(|| self.phrase("unknown_root").replace("{name}", root))?;
        let base = r.base_dir();
        let target = if path.is_empty() { base.clone() } else { base.join(path) };
        drop(roots);
        // Comparison on the canonicalised forms: the only one that resists
        // symbolic links, a textual `.` or `..` possibly being neutralised
        // by the filesystem itself.
        //
        // Under the circuit breaker, because `canonicalize` touches the
        // disk: on a reconnecting share it never returns, and it sits here
        // on the path of **every** `set_data` targeting a root. A clean
        // refusal beats a wedged admin loop, which would take the page down
        // with it.
        let (b, c) = (base.clone(), target.clone());
        let Some(canon) = self.health.bounded(&target, move || Ok((b.canonicalize()?, c.canonicalize()?))).await
        else {
            return Err(self
                .phrase("root_unresponsive")
                .replace("{path}", &target.display().to_string()));
        };
        let Ok::<(PathBuf, PathBuf), std::io::Error>((base_c, target_c)) = canon else {
            return Err(self.phrase("scan_io_error").replace("{path}", &target.display().to_string()));
        };
        if !target_c.starts_with(&base_c) {
            return Err(self.phrase("scan_io_error").replace("{path}", path));
        }
        Ok(target_c)
    }

    /// Publishes the list to the Source half and persists it, after every
    /// modification. The count leaves **before** the disk write: the web
    /// grid should not have to wait for a slow `/var/lib`.
    async fn playlist_changed(&self) {
        // mpv plays a **copy** of the list, written at the last `Play`. Any
        // modification diverges from it, and the Admin half cannot tell mpv
        // anything: the SDK forbids notifications from carrying an action.
        // This flag is therefore the only way to warn the Source half, which
        // will hand over the up-to-date list at the next order it receives.
        self.playlist_changed.store(true, Ordering::Relaxed);
        let list = self.playlist.read().await;
        let _ = self.preset_count_tx.send(list.preset_count());
        let stored: Vec<state::StoredEntry> =
            list.entries.iter().map(state::StoredEntry::from).collect();
        let index = list.index;
        drop(list);
        if let Err(e) = state::update(&self.state_path, |s| {
            s.playlist = stored;
            s.index = index;
        }) {
            tracing::warn!("persisting the playlist: {e}");
        }
    }

    /// Adds tracks to the list, honouring the cap.
    async fn add(&self, paths: Vec<PathBuf>) -> Result<(), String> {
        let mut list = self.playlist.write().await;
        if list.entries.len() + paths.len() > scan::MAX_TRACKS {
            return Err(self
                .phrase("too_many_tracks")
                .replace("{cap}", &scan::MAX_TRACKS.to_string()));
        }
        list.entries.extend(
            paths.into_iter().map(|path| Entry { path, title: None, duration_s: None }),
        );
        Ok(())
    }

    /// Writes the credentials file consumed by `mount.cifs`.
    ///
    /// The permissions are set **at creation**, not afterwards: creating
    /// then restricting would leave a window during which the passphrase
    /// would be readable by everyone.
    fn write_credentials(path: &Path, user: &str, password: &str, domain: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("cred.tmp");
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        use std::io::Write;
        writeln!(f, "username={user}")?;
        writeln!(f, "password={password}")?;
        if !domain.is_empty() {
            writeln!(f, "domain={domain}")?;
        }
        f.sync_all()?;
        drop(f);
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Reconciles the mounts, **and only if there is something to mount or
    /// unmount**.
    ///
    /// Without this guard, declaring a plain folder of the device asked
    /// systemd to start the mount unit, which requires a polkit
    /// authorisation: the page then displayed "the last mount attempt
    /// failed — interactive authentication required" to someone who had
    /// just added a USB stick and had asked for nothing of the sort. An
    /// alarming message for work that had no reason to happen.
    ///
    /// `even_without_smb` covers removal: the departing source may be the
    /// last share of the table, and it still has to be unmounted.
    async fn reconcile_roots(&self, table: &Roots, even_without_smb: bool) {
        if !even_without_smb && !table.root.iter().any(|r| r.kind == RootKind::Smb) {
            *self.mount_error.lock().unwrap() = None;
            return;
        }
        *self.mount_error.lock().unwrap() = mount::reconcile(mount::UNIT).await.err();
    }

    /// Starts probing the missing durations, as a background task.
    ///
    /// As a background task because there is no choice: the admin protocol
    /// has a 5 s cap, and a two-thousand-track list coming from a share
    /// needs more. The page follows the progress by polling, exactly as for
    /// the scan.
    ///
    /// Only probes what is missing: a duration coming from an `#EXTINF` or
    /// an earlier probe is kept, and `StoredEntry` persists it — a restart
    /// therefore re-probes nothing.
    ///
    /// The results are applied **by path** and not by index: the page can
    /// reorder or remove tracks during the probing, and applying by position
    /// would write one file's duration onto another.
    fn start_probe(
        playlist: Arc<AsyncRwLock<Playlist>>,
        durations: Arc<Mutex<DurationsProgress>>,
        state_path: PathBuf,
        health: Arc<Health>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let to_probe: Vec<PathBuf> = {
                let list = playlist.read().await;
                let mut v: Vec<PathBuf> = list
                    .entries
                    .iter()
                    .filter(|e| e.duration_s.is_none())
                    .map(|e| e.path.clone())
                    .collect();
                // The same file may appear twice: probing it once is enough,
                // the duration will be set on all its occurrences.
                v.sort();
                v.dedup();
                v
            };
            if to_probe.is_empty() {
                *durations.lock().unwrap() = DurationsProgress::default();
                return;
            }
            *durations.lock().unwrap() =
                DurationsProgress { running: true, done: 0, total: to_probe.len() };

            let mut done = 0usize;
            // Split into batches **per mount point**: a batch thus never
            // mixes two shares, and the circuit breaker of a silent mount
            // immediately discards all its following batches without running
            // anything — otherwise each batch would go back to waiting on
            // the same share.
            let batches: Vec<Vec<PathBuf>> = health
                .group(&to_probe)
                .into_iter()
                .flat_map(|(_, indices)| {
                    let paths: Vec<PathBuf> =
                        indices.iter().map(|&i| to_probe[i].clone()).collect();
                    paths.chunks(PROBE_BATCH).map(<[PathBuf]>::to_vec).collect::<Vec<_>>()
                })
                .collect();
            for batch in batches {
                let anchor = batch[0].clone();
                // Under the circuit breaker, and not merely `spawn_blocking`:
                // leaving the async thread protects the admin loop, but not
                // the pool. On a blocked share, every `reprobe` restart lost
                // one more thread there, without ever getting one back — the
                // circuit breaker is what bounded that leak to one thread
                // per mount point.
                let measured = health
                    .bounded(&anchor, move || {
                        batch.into_iter()
                            .map(|p| {
                                let d = ritornello_plugin_files::duration::probe(&p);
                                (p, d)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                // This mount does not answer: move on to the next batch,
                // without abandoning the probing — the local tracks of the
                // same list must go through. The remaining batches of the
                // same share will be discarded at no cost by the circuit
                // breaker.
                //
                // `done` does not advance for a skipped batch: the page will
                // show fewer measured durations than tracks, which is the
                // truth.
                let Some(measured) = measured else { continue };

                {
                    let mut list = playlist.write().await;
                    for (path, duration) in &measured {
                        let Some(d) = duration else { continue };
                        for e in list.entries.iter_mut() {
                            // `is_none` again: between the survey and now, a
                            // load may have set a duration.
                            if e.path == *path && e.duration_s.is_none() {
                                e.duration_s = Some(*d);
                            }
                        }
                    }
                    let stored: Vec<state::StoredEntry> =
                        list.entries.iter().map(state::StoredEntry::from).collect();
                    let index = list.index;
                    drop(list);
                    // Persist at every batch: a probe interrupted midway
                    // keeps what it has already found, instead of redoing
                    // everything.
                    if let Err(e) = state::update(&state_path, |s| {
                        s.playlist = stored;
                        s.index = index;
                    }) {
                        tracing::warn!("persisting track lengths: {e}");
                    }
                }

                done += measured.len();
                let mut p = durations.lock().unwrap();
                p.done = done;
            }
            let mut p = durations.lock().unwrap();
            p.running = false;
        })
    }

    /// Restarts the probing, abandoning the one that was running.
    fn reprobe(&mut self) {
        if let Some(t) = self.durations_task.take() {
            t.abort();
        }
        self.durations_task = Some(Self::start_probe(
            self.playlist.clone(),
            self.durations.clone(),
            self.state_path.clone(),
            self.health.clone(),
        ));
    }

    /// Writes the roots table, atomically.
    ///
    /// The temporary file then the rename: a power cut in the middle of a
    /// direct write would leave a truncated table, which the next startup
    /// would refuse — hence no source at all.
    fn write_table(&self, table: &Roots) -> Result<(), String> {
        let text = toml::to_string_pretty(table).map_err(|e| {
            tracing::warn!("serialising the roots table: {e}");
            self.phrase("store_io_error").replace("{path}", &self.roots_path.display().to_string())
        })?;
        let tmp = self.roots_path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &self.roots_path)).map_err(
            |e| {
                tracing::warn!("saving the roots table: {e}");
                self.phrase("store_io_error").replace("{path}", &self.roots_path.display().to_string())
            },
        )
    }

    /// Reads back the passphrase already saved for a root.
    ///
    /// Used when the page sends an empty one: it cannot send back what it
    /// never received.
    fn existing_password(path: &Path) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        content
            .lines()
            .find_map(|l| l.strip_prefix("password="))
            .map(str::to_string)
    }
}

#[async_trait::async_trait]
impl AdminPlugin for FilesAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => {
                Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string()))
            }
            "ui.css" => {
                Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string()))
            }
            _ => None,
        }
    }

    fn catalog(&self) -> serde_json::Value {
        let cat = self.catalog.read().unwrap();
        serde_json::json!(cat.entries())
    }

    async fn get_data(&self) -> serde_json::Value {
        let roots = self.roots.read().await;
        // Each root leaves with its mount state, but **never** its
        // passphrase: `Root` does not carry it, so there is nothing to
        // filter here — the type guarantees the absence, not vigilance.
        let root_values: Vec<serde_json::Value> = roots
            .root
            .iter()
            .map(|r| {
                let mut v = serde_json::to_value(r).unwrap_or_default();
                if let Some(o) = v.as_object_mut() {
                    o.insert(
                        "mounted".into(),
                        serde_json::json!(mount::state(r) == mount::MountState::Mounted),
                    );
                }
                v
            })
            .collect();
        // The internal store is local by construction (`/var/lib`): it is
        // read directly. The roots, however, can be shares, so each one goes
        // under its own circuit breaker — one `read_dir` per root and per
        // page poll was one of the two calls that wedged the plugin on
        // 2026-08-17.
        let mut saved = store::in_dir(&self.internal_playlists, Location::Internal);
        let to_collect: Vec<(PathBuf, String)> =
            roots.root.iter().map(|r| (r.base_dir(), r.name.clone())).collect();
        drop(roots);
        for (dir, name) in to_collect {
            let d = dir.clone();
            if let Some(v) =
                self.health.bounded(&dir, move || store::in_dir(&d, Location::Root(name))).await
            {
                saved.extend(v);
            }
        }

        let list = self.playlist.read().await;
        let paths: Vec<PathBuf> = list.entries.iter().map(|e| e.path.clone()).collect();
        let described: Vec<(String, String, Option<u32>)> = list
            .entries
            .iter()
            .map(|e| (e.path.to_string_lossy().into_owned(), e.display_name(), e.duration_s))
            .collect();
        let index = list.index;
        drop(list);
        // Grouped by mount point and bounded: a single delay covers all the
        // tracks of a share, instead of one blocking call per track.
        let missing = self.health.missing(&paths).await;
        let tracks: Vec<serde_json::Value> = described
            .into_iter()
            .zip(missing)
            .map(|((path, name, duration_s), is_missing)| {
                serde_json::json!({
                    "path": path,
                    "name": name,
                    "duration_s": duration_s,
                    // Flagged, never hidden: a list that shrinks without
                    // saying anything is a defect that takes months to
                    // attribute.
                    //
                    // `null` when the mount does not answer: saying "not
                    // found" would blame the files for a failure that is the
                    // share's, and would send the search for the defect to
                    // the wrong place. The page displays it as
                    // indeterminate.
                    "missing": is_missing,
                })
            })
            .collect();

        // `std::sync` guards taken after the last `.await`: none of them
        // crosses an await point.
        let scan = self.scan.lock().unwrap().clone();
        let unresolved = self.unresolved.lock().unwrap().clone();
        let browse = self.browse.lock().unwrap().clone();
        let volumes = volumes::volumes(&volumes::read_proc_mounts());
        let mount_error = self.mount_error.lock().unwrap().clone();
        let can_browse_smb = self.smb_ok.load(std::sync::atomic::Ordering::Relaxed);
        let explore = self.explore.view();
        serde_json::json!({
            "roots": root_values,
            "volumes": volumes,
            "can_browse_smb": can_browse_smb,
            // What the page does with it: decide whether clearing the list
            // must also request the stop. Without this information it would
            // cut the radio while clearing a files list that was not
            // playing.
            "playing": self.plays.load(std::sync::atomic::Ordering::Relaxed),
            // Progress of the duration probing: this is what makes the page
            // poll until they arrive, then stop.
            "durations": self.durations.lock().unwrap().clone(),
            "explore": explore,
            "mount_error": mount_error,
            // Mount points from which a probe never came back. Told to the
            // page so it can explain the silence: without them, the user
            // sees durations that never arrive and indeterminate states with
            // no indication of cause.
            "unresponsive": self.health.silent().iter()
                .map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "playlist": tracks,
            "index": index,
            "scan": scan,
            "browse": browse,
            "saved": saved.iter().map(|s| serde_json::json!({
                "name": s.name,
                "where": match &s.location {
                    Location::Internal => "internal".to_string(),
                    Location::Root(n) => n.clone(),
                },
            })).collect::<Vec<_>>(),
            "unresolved": unresolved,
        })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let op: Op = serde_json::from_value(data)
            .map_err(|e| self.phrase("bad_request").replace("{detail}", &e.to_string()))?;
        match op {
            Op::AddSource {
                kind,
                path,
                host,
                share,
                subpath,
                user,
                domain,
                password,
                writable,
            } => {
                let mut table = self.roots.read().await.clone();
                // Only the exact duplicate is refused: two different dirs of
                // the same share are two legitimate sources, which mount the
                // share twice — legal, cheap, and above all unsurprising.
                // Merging by widening the common subpath would silently
                // change the scope of an already declared source.
                let duplicate = table.root.iter().any(|r| {
                    r.kind == kind
                        && r.host == host
                        && r.share == share
                        && r.subpath == subpath
                        && r.path == path
                });
                if duplicate {
                    return Err(self.phrase("duplicate_source"));
                }
                let taken: Vec<&str> = table.root.iter().map(|r| r.name.as_str()).collect();
                let hint = match kind {
                    RootKind::Smb => share.clone(),
                    RootKind::Local => path
                        .clone()
                        .unwrap_or_default()
                        .rsplit('/')
                        .find(|s| !s.is_empty())
                        .unwrap_or("disque")
                        .to_string(),
                };
                let name = ritornello_plugin_files::roots::derive_name(&hint, &taken);
                let root = Root {
                    name: name.clone(),
                    kind,
                    path,
                    host: host.clone(),
                    share,
                    subpath,
                    user: user.clone(),
                    domain: domain.clone(),
                    writable,
                };
                table.root.push(root);
                // Validate **before** writing anything: a credentials file
                // laid down for a source refused afterwards would remain
                // orphaned on disk, with a passphrase inside.
                table.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;

                if kind == RootKind::Smb {
                    let r = table.by_name(&name).expect("just inserted");
                    let path = r.credentials_path(&self.creds_dir);
                    let secret = if !password.is_empty() {
                        password
                    } else if let Some(c) = self.explore.credentials(&host) {
                        c.password
                    } else {
                        Self::existing_password(&path).unwrap_or_default()
                    };
                    Self::write_credentials(&path, &user, &secret, &domain).map_err(|e| {
                        tracing::warn!("writing credentials for {name}: {e}");
                        self.phrase("store_io_error")
                            .replace("{path}", &path.display().to_string())
                    })?;
                }
                self.write_table(&table)?;
                // Mounting follows the declaration: no more button to find.
                self.reconcile_roots(&table, false).await;
                // And if it did not go through, the declaration is undone.
                //
                // The criterion is the **observed state of this source**, not
                // the return code of the reconciliation: `systemctl start`
                // applies to the whole unit, it can fail because of a
                // sleeping third-party share, and cancelling the addition of
                // a healthy share then would be wrong. Reported from use: a
                // source stayed registered after a refused mount, and it had
                // to be removed by hand before retrying.
                //
                // The scope stops at the declaration. An already accepted
                // source stays until manual removal: a momentarily
                // unreachable share must not vanish from the table.
                if kind == RootKind::Smb
                    && mount::state(table.by_name(&name).expect("just inserted"))
                        != mount::MountState::Mounted
                {
                    let detail = self
                        .mount_error
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| self.phrase("mount_silent_failure"));
                    let i = table
                        .root
                        .iter()
                        .position(|r| r.name == name)
                        .expect("the source was just inserted");
                    let removed = table.root.remove(i);
                    self.write_table(&table)?;
                    // The credentials file leaves with it: keeping it would
                    // let a passphrase outlive a source that never existed.
                    let _ = std::fs::remove_file(removed.credentials_path(&self.creds_dir));
                    // Set straight back to `None`, without going through
                    // `reconcile_roots`: that one would run a real
                    // `systemctl start` as soon as ANOTHER SMB source
                    // remains in the table, and its only effect would then
                    // be to rewrite `mount_error` — the page would display
                    // "the last mount attempt failed" for a source that is
                    // no longer declared. Nothing needs to be unmounted
                    // anyway: the share being removed was precisely not
                    // mounted.
                    *self.mount_error.lock().unwrap() = None;
                    *self.roots.write().await = table;
                    return Err(self.phrase("share_not_declared").replace("{detail}", &detail));
                }
                *self.roots.write().await = table;
                Ok(())
            }

            Op::RemoveSource { name } => {
                let mut table = self.roots.read().await.clone();
                let Some(i) = table.root.iter().position(|r| r.name == name) else {
                    return Err(self.phrase("unknown_source").replace("{name}", &name));
                };
                let removed = table.root.remove(i);
                self.write_table(&table)?;
                // The credentials file leaves with the source: keeping it
                // would let a passphrase outlive what justified it.
                let _ = std::fs::remove_file(removed.credentials_path(&self.creds_dir));
                // `even_without_smb`: the departing source may be the last
                // share of the table, and it still has to be unmounted.
                self.reconcile_roots(&table, removed.kind == RootKind::Smb).await;
                *self.roots.write().await = table;
                Ok(())
            }

            Op::SetWritable { name, writable } => {
                let mut table = self.roots.read().await.clone();
                let Some(r) = table.root.iter_mut().find(|r| r.name == name) else {
                    return Err(self.phrase("unknown_source").replace("{name}", &name));
                };
                r.writable = writable;
                self.write_table(&table)?;
                // Remounting is essential: `ro` is a mount option, not a
                // flag re-read at every write. Without reconciliation,
                // allowing writes would change nothing until the next
                // reboot.
                self.reconcile_roots(&table, false).await;
                *self.roots.write().await = table;
                Ok(())
            }

            Op::ExploreOpen { kind } => {
                self.explore.open(kind);
                Ok(())
            }
            Op::ExploreClose => {
                self.explore.close();
                Ok(())
            }
            Op::ExploreLocal { path } => self.explore.local(&path).await,
            Op::SmbConnect { host, user, password, domain } => {
                // Re-probe here: installing the package without restarting
                // the service must give a correct result rather than a stale
                // refusal.
                self.smb_ok.store(
                    ritornello_plugin_files::smb::available().await,
                    std::sync::atomic::Ordering::Relaxed,
                );
                self.explore.connect(host, user, password, domain);
                Ok(())
            }
            Op::SmbBrowse { share, path } => {
                self.explore.browse(share, path);
                Ok(())
            }
            Op::SmbShares => {
                self.explore.to_shares();
                Ok(())
            }

            Op::Mount => mount::reconcile(mount::UNIT).await,

            Op::Browse { root, path } => {
                let dir = self.under_root(&root, &path).await?;
                let cat = self.catalog.clone();
                let content = tokio::task::spawn_blocking(move || scan::list_dir(&dir))
                    .await
                    .map_err(|e| format!("browse task: {e}"))?
                    .map_err(|e| e.message(&cat.read().unwrap()))?;
                *self.browse.lock().unwrap() = serde_json::json!({
                    "root": root,
                    "path": path,
                    "dirs": content.dirs,
                    "files": content.audio,
                    // Playback playlists travel separately: they are not
                    // added to the current list, they replace it.
                    "playlists": content.playlists,
                    "results": [],
                    // Empty, and it is a marker, not an omission: the page
                    // uses it to tell the answer to a browse apart from the
                    // one to a search on the same folder.
                    "query": "",
                });
                Ok(())
            }

            Op::Search { root, path, query } => {
                // Two resolutions, two roles: `dir` is the folder being
                // searched, `base` the root the results are reported
                // against. Confusing them would return paths relative to the
                // subfolder, which an `add_file` would resolve elsewhere.
                let dir = self.under_root(&root, &path).await?;
                let base = self.under_root(&root, "").await?;
                let cat = self.catalog.clone();
                let pattern = query.clone();
                let (found, end) = tokio::task::spawn_blocking(move || {
                    scan::search(&dir, &pattern, 200, scan::MAX_VISITS, scan::SEARCH_TIMEOUT)
                })
                .await
                .map_err(|e| format!("search task: {e}"))?
                .map_err(|e| e.message(&cat.read().unwrap()))?;
                // Paths **relative to the root**: that is what the page
                // sends back later in an `add_file`, and an absolute path
                // there would be refused by the escape guard.
                let relative: Vec<String> = found
                    .iter()
                    .filter_map(|p| p.strip_prefix(&base).ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .collect();
                *self.browse.lock().unwrap() = serde_json::json!({
                    "root": root,
                    // The searched folder, not an empty string: the page
                    // only keeps the answer to the request it just made, and
                    // this (path, query) pair is what identifies it.
                    "path": path,
                    "query": query,
                    "dirs": [],
                    "files": [],
                    "playlists": [],
                    "results": relative,
                    // Two fields and not one boolean: the two causes of
                    // stopping do not call for the same advice. "truncated"
                    // invites refining the pattern, "gave_up" invites going
                    // down into a subfolder. Confusing them displayed "No
                    // results" — hence "this file does not exist" — for a
                    // search that had simply given up before reaching it.
                    "truncated": end == scan::SearchEnd::TooManyResults,
                    "gave_up": end == scan::SearchEnd::Interrupted,
                });
                Ok(())
            }

            Op::AddDir { root, path } => {
                let dir = self.under_root(&root, &path).await?;
                if let Some(t) = self.scan_task.take() {
                    // Two clicks must not leave two concurrent walks
                    // saturating a slow share.
                    t.abort();
                }
                *self.scan.lock().unwrap() = ScanProgress {
                    running: true,
                    found: 0,
                    dir: path.clone(),
                    error: None,
                };
                let progress = self.scan.clone();
                let playlist = self.playlist.clone();
                let catalog = self.catalog.clone();
                let state = self.scan.clone();
                let counter = Arc::new(AtomicUsize::new(0));
                let tx = self.preset_count_tx.clone();
                let changed = self.playlist_changed.clone();
                let playlist_for_durations = self.playlist.clone();
                let durations = self.durations.clone();
                let state_path_for_durations = self.state_path.clone();
                let health_for_durations = self.health.clone();
                let state_path = self.state_path.clone();
                self.scan_task = Some(tokio::spawn(async move {
                    let c = counter.clone();
                    let p = progress.clone();
                    let found = tokio::task::spawn_blocking(move || {
                        scan::walk_with(&dir, scan::MAX_TRACKS, &|n, d| {
                            c.store(n, Ordering::Relaxed);
                            if let Ok(mut g) = p.lock() {
                                g.found = n;
                                g.dir = d.display().to_string();
                            }
                        })
                    })
                    .await;
                    let outcome = match found {
                        Ok(Ok(paths)) => {
                            let mut list = playlist.write().await;
                            if list.entries.len() + paths.len() > scan::MAX_TRACKS {
                                Err(catalog
                                    .read()
                                    .unwrap()
                                    .get("too_many_tracks")
                                    .replace("{cap}", &scan::MAX_TRACKS.to_string()))
                            } else {
                                list.entries.extend(paths.into_iter().map(|path| Entry {
                                    path,
                                    title: None,
                                    duration_s: None,
                                }));
                                let count = list.preset_count();
                                let stored: Vec<state::StoredEntry> =
                                    list.entries.iter().map(state::StoredEntry::from).collect();
                                let index = list.index;
                                drop(list);
                                // Same reason as in `playlist_changed`: mpv
                                // plays a copy, and this flag is the only
                                // channel to the Source half.
                                changed.store(true, Ordering::Relaxed);
                                let _ = tx.send(count);
                                // The probing starts from here and not from
                                // the handler: that one returned long before
                                // the recursive walk added anything. Its
                                // handle is not kept — a concurrent probe
                                // only duplicates work, it never sets a
                                // wrong duration.
                                Self::start_probe(
                                    playlist_for_durations,
                                    durations,
                                    state_path_for_durations,
                                    health_for_durations,
                                );
                                if let Err(e) = state::update(&state_path, |s| {
                                    s.playlist = stored;
                                    s.index = index;
                                }) {
                                    tracing::warn!("persisting the playlist: {e}");
                                }
                                Ok(())
                            }
                        }
                        Ok(Err(e)) => Err(e.message(&catalog.read().unwrap())),
                        Err(e) => Err(format!("scan task: {e}")),
                    };
                    if let Ok(mut g) = state.lock() {
                        g.running = false;
                        g.error = outcome.err();
                    }
                }));
                Ok(())
            }

            Op::AddFile { root, path } => {
                let file = self.under_root(&root, &path).await?;
                self.add(vec![file]).await?;
                self.playlist_changed().await;
                self.reprobe();
                Ok(())
            }

            Op::Remove { index } => {
                let mut list = self.playlist.write().await;
                if index >= list.entries.len() {
                    return Err(self.phrase("bad_request").replace("{detail}", "index"));
                }
                let was_current = list.index == index;
                list.entries.remove(index);
                // The playback index follows: removing a track before the
                // playing one would otherwise shift the whole numbering
                // under the listener's feet.
                //
                // Removing **the one being listened to** is the special
                // case: playback stops (the page asks the core), and we
                // start over from the beginning. Leaving the index on the
                // freed position kept the highlight on a track nobody had
                // chosen — the one that slid into the place of the departed.
                if was_current {
                    list.index = 0;
                } else if list.index > index {
                    list.index -= 1;
                } else if list.index >= list.entries.len() {
                    list.index = 0;
                }
                drop(list);
                self.playlist_changed().await;
                Ok(())
            }

            Op::Move { from, to } => {
                let mut list = self.playlist.write().await;
                if from >= list.entries.len() || to >= list.entries.len() {
                    return Err(self.phrase("bad_request").replace("{detail}", "index"));
                }
                let e = list.entries.remove(from);
                list.entries.insert(to, e);
                // **The index follows the playing track.** It did not, and
                // the defect was visible: reordering the list left the
                // highlight on a position that now held another track — and
                // the Source half would have restarted the wrong one.
                //
                // Three cases, and only three: the playing track is the one
                // being moved, or the move steps over it in one direction,
                // or in the other.
                list.index = if list.index == from {
                    to
                } else if from < list.index && to >= list.index {
                    list.index - 1
                } else if from > list.index && to <= list.index {
                    list.index + 1
                } else {
                    list.index
                };
                drop(list);
                self.playlist_changed().await;
                Ok(())
            }

            Op::Clear => {
                let mut list = self.playlist.write().await;
                list.entries.clear();
                list.index = 0;
                drop(list);
                self.unresolved.lock().unwrap().clear();
                self.playlist_changed().await;
                // Abandons a running probe: it covered tracks that are no
                // longer there, and its progress would lie on screen.
                self.reprobe();
                Ok(())
            }

            Op::SavePlaylist { name, r#where } => {
                let dest = if r#where == "internal" {
                    Location::Internal
                } else {
                    Location::Root(r#where)
                };
                let roots = self.roots.read().await;
                let list = self.playlist.read().await;
                store::save(&list.entries, &name, &dest, &self.internal_playlists, &roots)
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))
            }

            Op::LoadPlaylist { name, r#where } => {
                let from = if r#where == "internal" {
                    Location::Internal
                } else {
                    Location::Root(r#where)
                };
                let roots = self.roots.read().await;
                let loaded = store::load(&name, &from, &self.internal_playlists, &roots)
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                drop(roots);
                *self.unresolved.lock().unwrap() = loaded.unresolved;
                let mut list = self.playlist.write().await;
                list.entries = loaded.entries;
                list.index = 0;
                drop(list);
                self.playlist_changed().await;
                self.reprobe();
                Ok(())
            }

            Op::LoadM3u { root, path } => {
                // An m3u found while browsing a source, as opposed to the
                // **saved** playlists that `LoadPlaylist` fetches by name
                // from a store. Here it is a file like any other, designated
                // by its path, so the escape guard applies.
                let file = self.under_root(&root, &path).await?;
                if !scan::is_playlist(&file) {
                    return Err(self.phrase("not_a_playlist").replace("{path}", &path));
                }
                let text = std::fs::read_to_string(&file).map_err(|e| {
                    tracing::warn!("reading {}: {e}", file.display());
                    self.phrase("store_io_error").replace("{path}", &path)
                })?;
                // Relative paths resolve first against the directory **of
                // the m3u**, as the format dictates; the root only serves
                // the fallbacks (absolute path from another machine,
                // Windows drive letter).
                let folder = file.parent().unwrap_or(&file).to_path_buf();
                let base = {
                    let roots = self.roots.read().await;
                    roots.by_name(&root).map(|r| r.base_dir()).unwrap_or_else(|| folder.clone())
                };
                let loaded = ritornello_plugin_files::m3u::parse(&text, &folder, &base);
                if loaded.entries.len() > scan::MAX_TRACKS {
                    return Err(self
                        .phrase("too_many_tracks")
                        .replace("{cap}", &scan::MAX_TRACKS.to_string()));
                }
                // Reported, never dropped silently: a list shorter than its
                // file is a defect that takes months to attribute.
                *self.unresolved.lock().unwrap() = loaded.unresolved;
                let mut list = self.playlist.write().await;
                list.entries = loaded.entries;
                list.index = 0;
                drop(list);
                self.playlist_changed().await;
                // An m3u may carry `#EXTINF` lines, but rarely all of them:
                // the probing only fills what is missing.
                self.reprobe();
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An admin over temporary directories, with a local root declared. The
    /// tempdir is leaked on purpose: the admin lives for the duration of the
    /// test, and dropping it would erase the files it writes.
    fn test_admin() -> (FilesAdmin, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root_dir = dir.path().to_path_buf();
        std::mem::forget(dir);
        std::fs::create_dir_all(root_dir.join("media")).unwrap();
        let (tx, _rx) = tokio::sync::watch::channel(0u8);
        let sources_catalog = Arc::new(RwLock::new(Catalog::load(
            "files",
            "en",
            &root_dir,
            ritornello_plugin_files::FILES_EN,
        )));
        let smb_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let health = Arc::new(ritornello_plugin_files::health::Health::new());
        let admin = FilesAdmin {
            roots_path: root_dir.join("media-roots.toml"),
            creds_dir: root_dir.join("creds"),
            internal_playlists: root_dir.join("playlists"),
            state_path: root_dir.join("plugin-files.json"),
            roots: Arc::new(AsyncRwLock::new(Roots::default())),
            playlist: Arc::new(AsyncRwLock::new(Playlist::default())),
            catalog: sources_catalog.clone(),
            scan: Arc::new(Mutex::new(ScanProgress::default())),
            scan_task: None,
            unresolved: Arc::new(Mutex::new(Vec::new())),
            browse: Arc::new(Mutex::new(serde_json::json!({}))),
            preset_count_tx: tx,
            playlist_changed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            plays: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            durations: Arc::new(Mutex::new(DurationsProgress::default())),
            durations_task: None,
            explore: ritornello_plugin_files::explore::Browser::new(
                root_dir.join("creds"),
                sources_catalog.clone(),
                smb_ok.clone(),
                health.clone(),
            ),
            mount_error: Arc::new(Mutex::new(None)),
            smb_ok,
            health,
        };
        (admin, root_dir)
    }

    fn add_share(password: &str) -> serde_json::Value {
        serde_json::json!({
            "op": "add_source", "kind": "smb", "host": "192.168.1.20",
            "share": "musique", "subpath": "Ma Musique", "user": "steven",
            "domain": "", "writable": false, "password": password
        })
    }

    #[tokio::test]
    async fn an_added_source_gets_a_derived_name() {
        // The user no longer types a name: it must be derived, valid, and
        // derived from the share to stay readable under /mnt/ritornello.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("p")).await.unwrap();
        let roots = admin.roots.read().await;
        assert_eq!(roots.root.len(), 1);
        assert_eq!(roots.root[0].name, "musique");
        assert_eq!(roots.root[0].subpath.as_deref(), Some("Ma Musique"));
    }

    #[tokio::test]
    async fn adding_a_local_folder_requests_no_mount() {
        // Defect found by the end-to-end journey, and invisible here without
        // this test: the reconciliation ran on every declaration, including
        // for a folder of the device. It requires polkit, so it failed, and
        // the page announced "the last mount attempt failed — interactive
        // authentication required" to someone who had simply plugged in a
        // USB stick.
        let dir = tempfile::tempdir().unwrap();
        let (mut admin, _) = test_admin();
        admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "local",
                "path": dir.path().display().to_string(),
                "host": "", "share": "", "user": "", "domain": "",
                "password": "", "writable": false
            }))
            .await
            .unwrap();
        assert_eq!(admin.roots.read().await.root.len(), 1);
        assert!(
            admin.mount_error.lock().unwrap().is_none(),
            "no mount should be attempted without a single declared share"
        );
    }

    #[tokio::test]
    async fn two_sources_of_the_same_share_do_not_fight_over_their_name() {
        // Without de-duplication, the second one would overwrite the first
        // one's credentials file and fight over its mount point.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n\
             //192.168.1.20/musique /mnt/ritornello/musique-2 cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("p")).await.unwrap();
        let mut second = add_share("p");
        second["subpath"] = serde_json::json!("Rock");
        admin.set_data(second).await.unwrap();
        let roots = admin.roots.read().await;
        assert_eq!(roots.root.len(), 2);
        assert_ne!(roots.root[0].name, roots.root[1].name);
    }

    #[tokio::test]
    async fn the_exact_duplicate_is_refused() {
        // Two identical sources would mount the same share twice at the same
        // logical place, with neither serving any further purpose.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("p")).await.unwrap();
        let err = admin.set_data(add_share("p")).await.unwrap_err();
        assert!(err.contains(' '), "raw key: {err}");
    }

    #[tokio::test]
    async fn removing_a_source_deletes_its_credentials_file() {
        // Otherwise a .cred containing a passphrase would outlive, on disk,
        // the source that justified it.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("secret")).await.unwrap();
        let cred = admin.creds_dir.join("musique.cred");
        assert!(cred.exists());
        admin
            .set_data(serde_json::json!({"op": "remove_source", "name": "musique"}))
            .await
            .unwrap();
        assert!(!cred.exists(), "the credentials file outlived the source");
        assert!(admin.roots.read().await.root.is_empty());
    }

    /// Serialises the tests that divert `/proc/mounts`.
    ///
    /// `std::env::set_var` is process-global, and the tests of one binary
    /// run in parallel inside it: without this lock, one test's fake file is
    /// read by another, with a failure that never reproduces on its own.
    static PROC_MOUNTS_LOCK: Mutex<()> = Mutex::new(());

    /// Guard returned by `divert_proc_mounts`.
    ///
    /// Carries the serialisation lock **and** clears the environment
    /// variable in turn, in a `Drop` — not in a line repeated at the end of
    /// each test. The lock is indeed released by its own `Drop` even if the
    /// test panics; the environment variable, however, was not before this
    /// guard, and `mount::state` has honoured it since this branch: a
    /// panicking test therefore left the whole remaining suite reading a
    /// fake `/proc/mounts` pointing at an already deleted tempdir — a single
    /// failure turned into an unreadable cascade.
    struct ProcMountsGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for ProcMountsGuard {
        fn drop(&mut self) {
            // SAFETY: see the note in `divert_proc_mounts`.
            unsafe { std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS") };
        }
    }

    /// Writes a fake `/proc/mounts` and makes the code under test read it.
    ///
    /// Returns the guard: the caller must keep it alive until the end of the
    /// test (`let _guard = ...`, never `let _ = ...`, which would release it
    /// immediately).
    fn divert_proc_mounts(root_dir: &std::path::Path, content: &str) -> ProcMountsGuard {
        let lock = PROC_MOUNTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fake = root_dir.join("mounts");
        std::fs::write(&fake, content).unwrap();
        // SAFETY: edition 2024 made this `unsafe` because mutating the
        // environment races with any concurrent `getenv`. The lock above
        // serialises every writer of the variable in this test binary: this
        // module compiles into the plugin's binary, whose tests are their own
        // executable, and the library has its twin fixture in `volumes`. The
        // residual risk, a read from another thread while `environ` is
        // reallocated, is not ours to remove and exists only under test.
        unsafe { std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &fake) };
        ProcMountsGuard { _lock: lock }
    }

    #[tokio::test]
    async fn get_data_reports_the_volumes_and_the_smb_capability() {
        let (admin, root_dir) = test_admin();
        let _guard =
            divert_proc_mounts(&root_dir, "/dev/sda1 /media/usb vfat rw 0 0\nproc /proc proc rw 0 0\n");
        let d = admin.get_data().await;
        assert_eq!(d["volumes"][0]["path"], "/media/usb");
        assert_eq!(d["volumes"].as_array().unwrap().len(), 1, "proc must not be offered");
        assert!(d["can_browse_smb"].is_boolean());
        assert!(d["explore"].is_object());
    }

    #[tokio::test]
    async fn a_share_that_does_not_mount_is_not_declared() {
        // Reported from use: the source appeared in the list even though the
        // mount had failed, and it had to be removed by hand before
        // retrying. The declaration is therefore fully undone — table and
        // credentials file — and the refusal goes back up to the dialog,
        // which keeps the input.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(&root_dir, "proc /proc proc rw 0 0\n");
        let err = admin.set_data(add_share("p")).await.unwrap_err();
        assert!(err.contains(' '), "raw key sent back to the screen: {err}");
        assert!(
            admin.roots.read().await.root.is_empty(),
            "the source stayed declared despite the mount failure"
        );
        assert!(
            !admin.creds_dir.join("musique.cred").exists(),
            "a passphrase outlived a source that does not exist"
        );
    }

    #[tokio::test]
    async fn a_healthy_share_does_not_gain_an_error_banner_when_a_neighbour_is_refused() {
        // Review defect: the second `reconcile_roots` of the rollback branch
        // runs a real `systemctl start` as soon as ANOTHER SMB source
        // remains in the table -- the case here -- and its only effect is
        // then to rewrite `mount_error`. The page could display "the last
        // mount attempt failed" for a source that is no longer declared,
        // exactly what the comment claimed to avoid.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("p")).await.unwrap();
        assert_eq!(admin.roots.read().await.root.len(), 1, "the healthy source must be declared");

        let mut second = add_share("p");
        second["share"] = serde_json::json!("absent");
        second["subpath"] = serde_json::json!("Rien");
        let err = admin.set_data(second).await.unwrap_err();
        assert!(err.contains(' '), "raw key sent back to the screen: {err}");

        assert_eq!(
            admin.roots.read().await.root.len(),
            1,
            "only the healthy source must stay declared"
        );
        assert!(
            admin.mount_error.lock().unwrap().is_none(),
            "no mount failure banner must survive the refusal of a neighbouring source"
        );
    }

    #[tokio::test]
    async fn an_actually_mounted_share_stays_declared() {
        // The other half, and the reason for the criterion: `systemctl` is
        // global, it can fail because of a broken third-party share. What
        // decides is the observed state of THIS source, not the return code
        // of the reconciliation. Without that, a sleeping NAS elsewhere
        // would cancel the addition of a healthy share.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("p")).await.unwrap();
        assert_eq!(admin.roots.read().await.root.len(), 1);
    }

    /// Test `/proc/mounts`: a local root and a separate share, so that the
    /// silence of one does not take down the other.
    const SILENT_MOUNTS: &str = "/dev/root / ext4 rw 0 0\n\
                                //192.168.1.15/musique /mnt/ritornello/nas cifs ro,soft 0 0\n";

    #[tokio::test]
    async fn get_data_returns_promptly_and_does_not_lie_when_a_mount_is_silent() {
        // The non-regression test for the 2026-08-17 incident: `get_data`
        // did one `is_file` per track and one `read_dir` per root, on the
        // async thread. The admin protocol being serial, a cifs mount
        // blocked in the kernel wedged the whole plugin there — to the point
        // of timing out `ui.js`, which is nothing but an `include_str!`.
        let (mut admin, _r) = test_admin();
        admin.health = Arc::new(ritornello_plugin_files::health::Health::for_test(
            std::time::Duration::from_millis(50),
            SILENT_MOUNTS.to_string(),
            vec![PathBuf::from("/mnt/ritornello/nas")],
        ));
        admin.playlist.write().await.entries = vec![
            Entry { path: PathBuf::from("/mnt/ritornello/nas/a.mp3"), title: None, duration_s: None },
            Entry { path: PathBuf::from("/home/pi/absent.mp3"), title: None, duration_s: None },
        ];

        // Deliberately wide margin. The circuit breaker is **already open**
        // here (the mount went `silent` at test setup), so `bounded` returns
        // without running anything: promptness is acquired by construction,
        // and the real guard of this test is the value assertion below. One
        // second made this line a fast-execution assumption — a flake under
        // the load of the other test binaries — while proving nothing more.
        // It now only punishes a catastrophic regression, a `get_data` that
        // would truly block.
        let start = std::time::Instant::now();
        let d = admin.get_data().await;
        assert!(start.elapsed() < std::time::Duration::from_secs(10), "{:?}", start.elapsed());

        // `null` and not `true`: that is the whole point of the fix. Saying
        // "not found" for a sleeping share would blame the files for a
        // failure that is the mount's. A direct `is_file` would return
        // `true` here, and this test would fall — that is what makes it
        // useful.
        assert!(d["playlist"][0]["missing"].is_null(), "{}", d["playlist"][0]);
        // The local track, however, is still judged: one mount's circuit
        // breaker must not make the others indeterminate.
        assert_eq!(d["playlist"][1]["missing"], serde_json::json!(true));
        assert_eq!(d["unresponsive"], serde_json::json!(["/mnt/ritornello/nas"]));
    }

    #[tokio::test]
    async fn toggling_writability_does_not_lose_the_password() {
        // Without this, changing writability would require removing then
        // redeclaring, hence retyping the passphrase.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("secret-du-nas")).await.unwrap();
        admin
            .set_data(serde_json::json!({"op": "set_writable", "name": "musique", "writable": true}))
            .await
            .unwrap();
        assert!(admin.roots.read().await.by_name("musique").unwrap().writable);
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=secret-du-nas"), "{cred}");
    }

    #[tokio::test]
    async fn get_data_never_returns_the_password() {
        // It has no reason to travel to the browser, and the page does not
        // need it to display a share's state. The guarantee is carried by
        // the type: neither `Root` nor the wizard's view contain the field.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("secret-du-nas")).await.unwrap();
        let text = serde_json::to_string(&admin.get_data().await).unwrap();
        assert!(!text.contains("password"), "{text}");
        assert!(!text.contains("secret-du-nas"), "{text}");
    }

    #[tokio::test]
    async fn an_empty_password_reuses_the_sessions_one() {
        // The wizard just used it to connect: making the user retype it at
        // confirmation would be an extra entry for nothing, and the page
        // cannot send back a secret it never receives.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.explore.open(ritornello_plugin_files::explore::Kind::Smb);
        admin.explore.connect(
            "192.168.1.20".into(),
            "steven".into(),
            "secret-du-nas".into(),
            String::new(),
        );
        admin.set_data(add_share("")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=secret-du-nas"), "{cred}");
    }

    #[tokio::test]
    async fn an_empty_password_keeps_the_one_already_saved() {
        // Last resort, when the dialog was closed in the meantime:
        // redeclaring a source of the same name must not silently break a
        // mount that worked, for lack of a passphrase.
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        std::fs::create_dir_all(&admin.creds_dir).unwrap();
        std::fs::write(
            admin.creds_dir.join("musique.cred"),
            "username=steven\npassword=secret-du-nas\n",
        )
        .unwrap();
        admin.set_data(add_share("")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=secret-du-nas"), "{cred}");
    }

    #[tokio::test]
    async fn a_new_password_replaces_the_old_one() {
        // Guard rail for the rule above: "empty = keep" must not become
        // "the password can never be changed again".
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        std::fs::create_dir_all(&admin.creds_dir).unwrap();
        std::fs::write(admin.creds_dir.join("musique.cred"), "username=steven\npassword=ancien\n")
            .unwrap();
        admin.set_data(add_share("nouveau")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=nouveau"), "{cred}");
        assert!(!cred.contains("ancien"), "{cred}");
    }

    #[tokio::test]
    async fn an_invalid_source_is_refused_with_a_message_naming_the_culprit() {
        let (mut admin, _) = test_admin();
        let err = admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "smb", "host": "nas,uid=0",
                "share": "musique", "user": "u"
            }))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "raw key sent back to the screen: {err}");
        assert!(err.contains("nas,uid=0"), "the refusal must name what is wrong: {err}");
    }

    #[tokio::test]
    async fn a_refused_source_leaves_no_credentials_file() {
        // Validation passes **before** any write: a file laid down for a
        // source refused afterwards would remain orphaned on disk, with a
        // passphrase inside.
        let (mut admin, _) = test_admin();
        let _ = admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "smb", "host": "nas,uid=0",
                "share": "musique", "user": "u", "password": "p"
            }))
            .await
            .unwrap_err();
        assert!(!admin.creds_dir.join("musique.cred").exists());
        assert!(!admin.roots_path.exists(), "the table must not have been written either");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_credentials_file_is_written_as_0600() {
        // Permissions set **at creation**, not afterwards: creating then
        // restricting would leave a window during which the passphrase would
        // be readable by everyone.
        use std::os::unix::fs::PermissionsExt;
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("secret")).await.unwrap();
        let meta = std::fs::metadata(admin.creds_dir.join("musique.cred")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn the_saved_table_reads_back_unchanged() {
        let (mut admin, root_dir) = test_admin();
        let _guard = divert_proc_mounts(
            &root_dir,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(add_share("p")).await.unwrap();
        let reread = Roots::load(&admin.roots_path).unwrap();
        assert_eq!(reread.root.len(), 1);
        assert_eq!(reread.root[0].host, "192.168.1.20");
        // And the password is not in it: it lives in the credentials file,
        // which only `mount.cifs` will read.
        let toml = std::fs::read_to_string(&admin.roots_path).unwrap();
        assert!(!toml.contains("password"), "{toml}");
    }

    #[tokio::test]
    async fn removing_a_track_before_the_playing_one_shifts_the_index() {
        // Without this shift, the whole numbering would slide under the
        // listener's feet: track 4 would become track 3 while still
        // listening to the same one.
        let (admin, _) = test_admin();
        {
            let mut list = admin.playlist.write().await;
            list.entries = (1..=4)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/m/{i}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect();
            list.index = 2;
        }
        let mut admin = admin;
        admin.set_data(serde_json::json!({"op": "remove", "index": 0})).await.unwrap();
        let list = admin.playlist.read().await;
        assert_eq!(list.entries.len(), 3);
        assert_eq!(list.index, 1, "the playing track must stay the same");
    }

    /// Waits for the duration probing to finish, or gives up after a timeout.
    ///
    /// The probing is **asynchronous** on purpose: the admin protocol has a
    /// 5 s cap, and a list coming from a share needs more. A test must
    /// therefore wait for it, not assume it finished by the time the
    /// operation returned.
    async fn wait_for_durations(admin: &FilesAdmin) {
        for _ in 0..200 {
            let p = admin.durations.lock().unwrap().clone();
            if p.total > 0 && !p.running {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the duration probing never completed");
    }

    /// Makes a real mp3, or returns `None` if ffmpeg is missing.
    fn mp3_of(seconds: u32, path: &Path) -> Option<()> {
        std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={seconds}"))
            .arg(path)
            .status()
            .ok()
            .filter(|s| s.success())
            .map(|_| ())
    }

    #[tokio::test]
    async fn adding_a_file_probes_its_duration_in_the_background() {
        // The requirement: missing durations fill themselves in, without
        // blocking the addition — a folder of a thousand tracks would
        // exceed the core's 5 s cap.
        let (mut admin, root_dir) = admin_with_local_root().await;
        let media = root_dir.join("media");
        std::fs::create_dir_all(&media).unwrap();
        if mp3_of(3, &media.join("piste.mp3")).is_none() {
            eprintln!("ffmpeg missing: test skipped");
            return;
        }
        admin
            .set_data(serde_json::json!({
                "op": "add_file", "root": "local", "path": "piste.mp3"
            }))
            .await
            .unwrap();
        wait_for_durations(&admin).await;
        let list = admin.playlist.read().await;
        let d = list.entries[0].duration_s.expect("a duration was expected");
        assert!((2..=4).contains(&d), "duration read {d}");
    }

    #[tokio::test]
    async fn an_already_known_duration_is_not_overwritten() {
        // Those from an `#EXTINF` are the authority: the file may be an
        // excerpt, and reprobing over it would erase what the list asserted.
        let (admin, _) = admin_with_local_root().await;
        {
            let mut list = admin.playlist.write().await;
            list.entries = vec![Entry {
                path: PathBuf::from("/m/inexistant.mp3"),
                title: None,
                duration_s: Some(245),
            }];
        }
        let mut admin = admin;
        admin.reprobe();
        // Nothing to probe: the probing terminates without touching anything.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(admin.playlist.read().await.entries[0].duration_s, Some(245));
        assert_eq!(admin.durations.lock().unwrap().total, 0, "nothing needed probing");
    }

    #[tokio::test]
    async fn probed_durations_are_persisted() {
        // Without persistence, every restart would reprobe the whole list —
        // thousands of header reads on a share, for nothing.
        let (mut admin, root_dir) = admin_with_local_root().await;
        let media = root_dir.join("media");
        std::fs::create_dir_all(&media).unwrap();
        if mp3_of(2, &media.join("p.mp3")).is_none() {
            eprintln!("ffmpeg missing: test skipped");
            return;
        }
        admin
            .set_data(serde_json::json!({"op": "add_file", "root": "local", "path": "p.mp3"}))
            .await
            .unwrap();
        wait_for_durations(&admin).await;
        let state = state::load(&admin.state_path);
        assert!(state.playlist[0].duration_s.is_some(), "the duration must survive a restart");
    }

    #[tokio::test]
    async fn removing_the_playing_track_starts_over_from_the_beginning() {
        // Reported defect: the index stayed on the freed position, so the
        // highlight landed on the track that had slid into the place of the
        // departed one — a track the user had not chosen. We start over
        // from the beginning, which goes hand in hand with the stop the
        // page requests.
        let (admin, _) = test_admin();
        {
            let mut list = admin.playlist.write().await;
            list.entries = (1..=4)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/m/{i}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect();
            list.index = 2;
        }
        let mut admin = admin;
        admin.set_data(serde_json::json!({"op": "remove", "index": 2})).await.unwrap();
        let list = admin.playlist.read().await;
        assert_eq!(list.index, 0, "we start over from the start");
        assert_eq!(list.entries.len(), 3);
    }

    #[tokio::test]
    async fn reordering_the_list_keeps_the_highlight_on_the_playing_track() {
        // Defect reported in use: `move` swapped the tracks without touching
        // the index. The highlight stayed on a position that now held
        // something else, and the Source half would have restarted the
        // wrong track.
        //
        // The three cases that move the index, and one that must not touch it.
        let cases = [
            // (index before, from, to, expected index, what is being tested)
            (2usize, 2usize, 0usize, 0usize, "moving the playing track itself"),
            (2, 0, 3, 1, "a move steps over it downstream"),
            (1, 3, 0, 2, "a move steps over it upstream"),
            (0, 2, 3, 0, "a move that does not concern it"),
        ];
        for (before, from, to, expected, what) in cases {
            let (admin, _) = test_admin();
            {
                let mut list = admin.playlist.write().await;
                list.entries = (1..=4)
                    .map(|i| Entry {
                        path: PathBuf::from(format!("/m/{i}.mp3")),
                        title: None,
                        duration_s: None,
                    })
                    .collect();
                list.index = before;
            }
            let mut admin = admin;
            admin
                .set_data(serde_json::json!({"op": "move", "from": from, "to": to}))
                .await
                .unwrap();
            assert_eq!(admin.playlist.read().await.index, expected, "{what}");
        }
    }

    #[tokio::test]
    async fn reordering_never_loses_the_playing_track() {
        // Guard rail for the previous test, expressed on what really
        // matters: whatever the move, the index must designate **the same
        // file**.
        for from in 0..4usize {
            for to in 0..4usize {
                let (admin, _) = test_admin();
                {
                    let mut list = admin.playlist.write().await;
                    list.entries = (1..=4)
                        .map(|i| Entry {
                            path: PathBuf::from(format!("/m/{i}.mp3")),
                            title: None,
                            duration_s: None,
                        })
                        .collect();
                    list.index = 2;
                }
                let mut admin = admin;
                admin
                    .set_data(serde_json::json!({"op": "move", "from": from, "to": to}))
                    .await
                    .unwrap();
                let list = admin.playlist.read().await;
                assert_eq!(
                    list.entries[list.index].path,
                    PathBuf::from("/m/3.mp3"),
                    "move {from} -> {to} lost the playing track"
                );
            }
        }
    }

    /// An admin with a local root declared on `media`, and its path.
    async fn admin_with_local_root() -> (FilesAdmin, PathBuf) {
        let (admin, root_dir) = test_admin();
        *admin.roots.write().await = Roots {
            root: vec![Root {
                name: "local".into(),
                kind: RootKind::Local,
                path: Some(root_dir.join("media").display().to_string()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable: false,
            }],
        };
        (admin, root_dir)
    }

    #[tokio::test]
    async fn a_browsed_m3u_loads_and_replaces_the_list() {
        // The requirement: being able to load an m3u **found on the
        // source**, by its path, rather than a saved list looked up by name
        // in a store.
        let (mut admin, root_dir) = admin_with_local_root().await;
        let media = root_dir.join("media");
        std::fs::create_dir_all(media.join("Album")).unwrap();
        std::fs::write(media.join("Album/01.mp3"), b"").unwrap();
        std::fs::write(media.join("Album/02.mp3"), b"").unwrap();
        // Paths **relative to the m3u**, as the format requires.
        std::fs::write(media.join("Album/tout.m3u"), "01.mp3\n02.mp3\n").unwrap();

        admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "Album/tout.m3u"
            }))
            .await
            .unwrap();
        let list = admin.playlist.read().await;
        assert_eq!(list.entries.len(), 2);
        assert_eq!(list.entries[0].path, media.join("Album/01.mp3"));
        assert_eq!(list.index, 0, "we start over from the start of the loaded list");
    }

    #[tokio::test]
    async fn an_m3u_reports_what_it_could_not_resolve() {
        // Reported, never dropped silently: a list shorter than its file is
        // a defect that takes months to attribute.
        let (mut admin, root_dir) = admin_with_local_root().await;
        let media = root_dir.join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("present.mp3"), b"").unwrap();
        std::fs::write(media.join("list.m3u"), "present.mp3\nZ:\\elsewhere\\absent.mp3\n").unwrap();

        admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "list.m3u"
            }))
            .await
            .unwrap();
        assert_eq!(admin.playlist.read().await.entries.len(), 1);
        assert_eq!(admin.unresolved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn loading_something_other_than_an_m3u_is_refused() {
        // Without this guard, the list would be replaced by the interpreted
        // content of an arbitrary file — an audio binary read as text.
        let (mut admin, root_dir) = admin_with_local_root().await;
        let media = root_dir.join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("piste.mp3"), b"").unwrap();
        let err = admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "piste.mp3"
            }))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "raw key sent back to the screen: {err}");
        assert!(err.contains("piste.mp3"), "the refusal must name the culprit: {err}");
    }

    #[tokio::test]
    async fn loading_an_m3u_outside_the_root_is_refused() {
        // The escape guard applies as for any path coming from the browser:
        // `load_m3u` must not become a way to play an arbitrary file.
        let (mut admin, root_dir) = admin_with_local_root().await;
        std::fs::create_dir_all(root_dir.join("media")).unwrap();
        std::fs::write(root_dir.join("dehors.m3u"), "x\n").unwrap();
        let err = admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "../dehors.m3u"
            }))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "raw key: {err}");
        assert!(admin.playlist.read().await.entries.is_empty());
    }

    #[tokio::test]
    async fn clearing_the_list_also_clears_the_unresolved_entries() {
        // They described the previous list: leaving them would display a
        // warning about nothing, that nothing would ever clear.
        let (mut admin, _) = test_admin();
        admin.unresolved.lock().unwrap().push("Z:\\absent.mp3".into());
        admin.set_data(serde_json::json!({"op": "clear"})).await.unwrap();
        assert!(admin.unresolved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_path_that_escapes_the_root_is_refused() {
        // The escape guard: `path` comes from the browser with every
        // request, and a `../..` there would browse then add files outside
        // any declared root.
        let (mut admin, root_dir) = test_admin();
        *admin.roots.write().await = Roots {
            root: vec![Root {
                name: "local".into(),
                kind: RootKind::Local,
                path: Some(root_dir.join("media").display().to_string()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable: false,
            }],
        };
        let err = admin
            .set_data(serde_json::json!({"op": "browse", "root": "local", "path": "../.."}))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "raw key: {err}");
    }

    #[tokio::test]
    async fn browsing_stores_the_content_for_get_data() {
        // `set_data` only returns an Ok/Err: the content must therefore
        // travel through `get_data`, without which the page would have no
        // way to obtain it.
        let (mut admin, root_dir) = test_admin();
        std::fs::create_dir_all(root_dir.join("media/Album")).unwrap();
        std::fs::write(root_dir.join("media/Album/01.mp3"), b"").unwrap();
        std::fs::write(root_dir.join("media/notes.txt"), b"").unwrap();
        *admin.roots.write().await = Roots {
            root: vec![Root {
                name: "local".into(),
                kind: RootKind::Local,
                path: Some(root_dir.join("media").display().to_string()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable: false,
            }],
        };
        admin
            .set_data(serde_json::json!({"op": "browse", "root": "local", "path": ""}))
            .await
            .unwrap();
        let data = admin.get_data().await;
        assert_eq!(data["browse"]["dirs"], serde_json::json!(["Album"]));
        // `notes.txt` is not an audio file: it has no business being in a
        // music browsing tree.
        assert_eq!(data["browse"]["files"], serde_json::json!([]));
    }

    /// Declares a populated local root, and returns its path.
    async fn populated_local_root(admin: &mut FilesAdmin) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        std::mem::forget(dir);
        std::fs::create_dir_all(base.join("A")).unwrap();
        std::fs::create_dir_all(base.join("B")).unwrap();
        std::fs::write(base.join("A/miles.mp3"), b"").unwrap();
        std::fs::write(base.join("B/miles.mp3"), b"").unwrap();
        admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "local",
                "path": base.display().to_string(),
                "host": "", "share": "", "user": "", "domain": "",
                "password": "", "writable": false
            }))
            .await
            .unwrap();
        base
    }

    #[tokio::test]
    async fn a_search_is_limited_to_the_requested_folder() {
        // Reported from use: the search always started from the root, so it
        // raked the whole NAS regardless of the open folder — slow, and
        // flooded with namesakes from elsewhere.
        let (mut admin, _) = test_admin();
        let base = populated_local_root(&mut admin).await;
        let name = admin.roots.read().await.root[0].name.clone();
        admin
            .set_data(serde_json::json!({"op": "search", "root": name, "path": "A", "query": "miles"}))
            .await
            .unwrap();
        let d = admin.get_data().await;
        let results = d["browse"]["results"].as_array().unwrap().clone();
        // Only one: the one in B is outside the requested folder.
        assert_eq!(results.len(), 1, "the search overflowed the folder: {results:?}");
        // Relative to the ROOT and not to the searched folder: this is the
        // form the page sends back afterwards in an `add_file`, and a path
        // relative to the subfolder would designate a non-existent file
        // there.
        assert_eq!(results[0].as_str().unwrap(), "A/miles.mp3");
        assert_eq!(d["browse"]["path"].as_str().unwrap(), "A");
        assert_eq!(d["browse"]["query"].as_str().unwrap(), "miles");
        drop(base);
    }

    #[tokio::test]
    async fn a_browse_is_distinguished_from_a_search_by_its_empty_query() {
        // The two land in the same place on the plugin side. Without this
        // marker, the page could not tell the response to its browse apart
        // from one to a search on the same folder, and would fill the level
        // with search results.
        let (mut admin, _) = test_admin();
        let base = populated_local_root(&mut admin).await;
        let name = admin.roots.read().await.root[0].name.clone();
        admin
            .set_data(serde_json::json!({"op": "browse", "root": name, "path": "A"}))
            .await
            .unwrap();
        let d = admin.get_data().await;
        assert_eq!(d["browse"]["query"].as_str().unwrap(), "");
        drop(base);
    }
}
