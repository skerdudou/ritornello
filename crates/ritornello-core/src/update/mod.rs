//! Everything the core does about updating itself and its plugins.
//!
//! Split by responsibility rather than by layer: what a release says
//! (`release`), what an archive contains (`archive`), how bytes get onto the
//! disk (`download`), when that happens by itself (`schedule`), and what the
//! page is told (`routes`).
//!
//! What lives *here* is the worker: the one place that does all of it in
//! order, and the only untested thing in the module. That is deliberate —
//! every decision it makes has already been proven by a pure function
//! somewhere below it, and what is left is sockets, files and a child
//! process.

pub mod release;
pub mod archive;
pub mod download;
pub mod state;
pub mod schedule;

pub mod routes;

use crate::plugins::PluginManifest;
use crate::status::{PluginAction, PluginOrder, StatusState};
use crate::update::archive::{installable_from_ui, DECOMPRESSED_MAX};
use crate::update::download::{
    client, digest_hex, enough_room, fetch_capped, fetch_text, DownloadError, COMPRESSED_MAX,
};
use crate::update::release::{
    download_name, fold, parse_checksums, parse_releases, releases_url, Offer, Published,
    ReleasesError, ARCH, REPO,
};
use crate::update::state::{
    component_offers, Availability, CheckOutcome, ComponentKind, ComponentOffer, Installed,
    UpdateState,
};
use ritornello_i18n::Catalog;
use ritornello_updater::request::{Action, Request, REQUEST_FORMAT};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// What the startup power setting must be replaced by, for this start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupOverride {
    /// Whatever the device was doing when it last wrote its state.
    Previous,
    AsConfigured,
}

/// Pure, and the whole of the "do not start playing at 3 a.m." requirement.
///
/// The marker is **read and not consumed**, which is what makes the rollback
/// case work: it puts back the previous core, and that binary must still find
/// the instruction even though the broken one already read it. Nothing to
/// delete, no ownership to hand over, no race.
pub fn startup_override(
    marker: Option<ritornello_updater::marker::Marker>,
    now_unix_s: u64,
) -> StartupOverride {
    match marker {
        Some(m) if ritornello_updater::marker::is_fresh(&m, now_unix_s) => {
            StartupOverride::Previous
        }
        _ => StartupOverride::AsConfigured,
    }
}

/// The instruction this boot must obey, read from the one place it can be
/// written.
///
/// Two lines, and they are separated from `startup_override` on purpose: the
/// "read and not consumed" property lives in the **read**, not in the
/// decision, and a pure function handed a value twice cannot tell a read that
/// deletes the file from one that leaves it. This is what a test can drive
/// against a real marker on a real directory.
pub fn startup_instruction(prefix: &Path, now_unix_s: u64) -> StartupOverride {
    startup_override(ritornello_updater::marker::read(prefix), now_unix_s)
}

/// What the update worker is asked to do. One enum rather than one channel per
/// gesture: they are serialised against each other on purpose — two installs
/// at once would race on the staging directory.
#[derive(Debug, Clone)]
pub enum Job {
    Check,
    /// Install the named components. `core` names the core; anything else is a
    /// plugin.
    Install(Vec<String>),
    /// The scheduler's own job: a check, and — when the policy is
    /// `CheckAndInstall` — the installs that check turns out to make due.
    ///
    /// **One job and not two**, and that is the whole reason it exists: what
    /// an automatic run must install is only known once the check has
    /// answered. A ticker that enqueued `Check` and then `Install` would have
    /// to build the list from the *previous* check, and would install a day
    /// late — or, on the first run of a fresh device, install nothing at all.
    Scheduled {
        /// `UpdatePolicy::CheckAndInstall`, decided by the ticker that has the
        /// settings in hand rather than read again here.
        install: bool,
    },
}

/// The name of the core's own row, and the name the page sends to install it.
/// Written once here rather than quoted at each of its three uses.
const CORE: &str = "core";

/// Plugins first, the core last.
///
/// The core exits at the end of its own install and `Restart=always` brings it
/// back, so anything queued behind it would simply not happen. Pure, because
/// that ordering is the one thing about the install loop that can be wrong
/// without any I/O being involved.
fn install_order(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = names.iter().filter(|n| *n != CORE).cloned().collect();
    if names.iter().any(|n| n == CORE) {
        out.push(CORE.to_string());
    }
    out
}

/// Does this published archive carry the component the page named?
fn carries(published: &Published, name: &str) -> bool {
    match &published.offer {
        Offer::Core => name == CORE,
        Offer::Plugin(plugin) => plugin == name,
        // Never installed component by component, and `download_name` already
        // answers `None` for it.
        Offer::Bundle => false,
    }
}

/// What an automatic run is allowed to install, out of what the check found.
///
/// Four exclusions, and each answers a decision of the specification rather
/// than a convenience:
///
/// - a **third-party** plugin is never touched by the automatic policy — its
///   repository is not ours to judge;
/// - a plugin the device does not have is never *added* by itself — choosing
///   what is installed stays the operator's decision;
/// - a component already known to need a manual step is not attempted again
///   every night, which would download the same archive daily to refuse it
///   for the same reason;
/// - a component whose **installed version is unknown** is left alone. That
///   is not caution for its own sake: a plugin switched off, or dead, or
///   predating the version field never announces one, so `differs` answers
///   "yes" against every release for ever — and without this line the device
///   would download and install that archive again every single night,
///   learning nothing each time. The operator can still install it by hand,
///   where the gesture is asked for once.
fn automatic_install_list(components: &[ComponentOffer]) -> Vec<String> {
    components
        .iter()
        .filter(|c| c.availability == Availability::UpdateAvailable)
        .filter(|c| matches!(c.kind, ComponentKind::Core | ComponentKind::Plugin))
        .filter(|c| c.installable != Some(false))
        .filter(|c| c.installed.is_some())
        .map(|c| c.name.clone())
        .collect()
}

/// Carries a remembered "cannot be installed from here" across a check.
///
/// Installability is read off the archive, so it is only ever learnt at the
/// moment of a gesture (§9 of the design: knowing it in advance would mean
/// fetching every archive daily). A check that rebuilt the rows from scratch
/// would therefore forget it every night, and the automatic policy would
/// re-download the same refused archive every night with it.
///
/// Keyed on the **offered version** as well as the name: a new version of a
/// component is a new archive, and nothing is known about it yet.
fn carry_installable(previous: &[ComponentOffer], fresh: &mut [ComponentOffer]) {
    for row in fresh.iter_mut() {
        row.installable = previous
            .iter()
            .find(|p| p.name == row.name && p.offered == row.offered)
            .and_then(|p| p.installable);
    }
}

/// The release page a tag points at. Built rather than read: the list endpoint
/// does carry an `html_url`, but reconstructing it from the repository fixed at
/// compile time and the tag keeps one less field to parse and cannot point
/// anywhere but at our own repository.
fn release_page(tag: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/{tag}")
}

/// The asset's own file name, which is the key `SHA256SUMS` uses.
///
/// Taken from the URL rather than rebuilt from the offer and the version: the
/// digest must be looked up for the file that was actually fetched, and
/// rebuilding the name would be a second copy of the naming convention that
/// could drift from `classify_asset`'s.
fn asset_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// The archive's own hash against the line `SHA256SUMS` gave for it.
///
/// **An absent line is a refusal, never a pass.** `parse_checksums`
/// deliberately skips a malformed or blank line rather than failing the whole
/// file, so a missing entry is exactly what a truncated or mis-generated
/// checksum file produces; treating absence as "nothing to check" would
/// install unverified bytes.
///
/// A mismatch is a refusal and not a retry: the bytes arrived intact from
/// some server's point of view, and fetching them again is how a loop starts.
///
/// The comparison is an exact `==` on lowercase hex, and that is correct
/// against our own files rather than lucky: CI writes them with
/// `sha256sum *.tar.gz`, which emits lowercase hex, and `parse_checksums`
/// stores the digest verbatim. Any drift in case or whitespace therefore
/// fails **closed**. Neither side is normalised to make a comparison succeed,
/// and the comparison is not constant-time on purpose: this digest guards
/// against a corrupted or mis-served download, not against forgery —
/// `SHA256SUMS` travels in the same release over the same channel, so a
/// hostile release signs its own lies. Authenticity comes from HTTPS to a
/// repository fixed at compile time.
fn verify_digest(name: &str, published: Option<&str>, got: &str) -> Result<(), DownloadError> {
    let Some(expected) = published else {
        return Err(DownloadError::NoDigest(name.to_string()));
    };
    if expected != got {
        return Err(DownloadError::Digest {
            expected: expected.to_string(),
            got: got.to_string(),
        });
    }
    Ok(())
}

/// How long the core waits for the privileged unit. It is a `oneshot` that
/// copies a few tens of megabytes at worst; two minutes is generous on an SD
/// card and still finite.
const PRIVILEGED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long the worker waits for the core loop to acknowledge a plugin
/// restart. The loop kills the process and launches it again, both quick — but
/// an acknowledgment that never came would leave `busy` set for ever, which is
/// the one failure this whole gesture must not have.
const RESTART_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Asks systemd for the privileged unit, and waits for it.
///
/// No `--no-block`: it is a `oneshot`, and the core is not what it stops, so
/// waiting is what lets the next step depend on it having finished. With a
/// deadline, because an I/O that hangs has already made a page disappear in
/// this product — here it would leave `busy` set for ever.
async fn run_privileged_unit() -> Result<(), String> {
    let call = tokio::process::Command::new("systemctl")
        .arg("start")
        .arg("ritornello-update.service")
        .output();
    let output = match tokio::time::timeout(PRIVILEGED_TIMEOUT, call).await {
        Err(_) => {
            return Err(format!(
                "systemctl start timed out after {} s",
                PRIVILEGED_TIMEOUT.as_secs()
            ))
        }
        Ok(Err(e)) => return Err(format!("systemctl unavailable: {e}")),
        Ok(Ok(o)) => o,
    };
    if output.status.success() {
        return Ok(());
    }
    // systemctl's own words, verbatim, all the way to the page: when the
    // polkit rule is missing it names the file, which is exactly the
    // diagnosis. Same choice as the files plugin makes for its mount.
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("systemctl failed ({})", output.status)
    } else {
        stderr
    })
}

/// What one component's install placed, once the privileged unit has run.
enum Placed {
    /// The core binary. Nothing follows it: the process has to leave for the
    /// new binary to be the one running.
    Core,
    /// A plugin binary. Its process is still the old one until the core loop
    /// stops it and launches it again.
    Plugin,
}

/// Everything the worker needs, and nothing it could read twice.
///
/// A struct rather than nine parameters threaded through five async
/// functions: it is built once in `main`, where all of these already exist.
pub struct Worker {
    /// What `GET /api/update` serves. The worker is its only writer.
    pub state: Arc<RwLock<UpdateState>>,
    /// Every message this worker publishes goes through it: the page shows
    /// `busy` and `outcome` as they arrive, without a second lookup.
    pub catalog: Arc<RwLock<Catalog>>,
    /// Where a plugin's announced version is read. The binary is the only
    /// thing that knows it, and it says so in its announcement.
    pub status: Arc<RwLock<StatusState>>,
    /// `plugins.toml`: the authority on what is declared and where each
    /// binary should be. Read per check rather than remembered, like every
    /// other reader of this file — a plugin installed while the core runs
    /// must appear without a restart.
    pub manifest: PathBuf,
    /// The core loop's ear, for the restart that follows a plugin's
    /// replacement.
    pub plugins_tx: mpsc::Sender<PluginOrder>,
    /// `<state dir>/staging`: what the service writes and root distrusts. Not
    /// to be confused with `/var/lib/ritornello-update`, which only root
    /// writes.
    pub staging: PathBuf,
    /// Filesystem root, `/` in service. A field for the same reason the
    /// privileged crate takes a `prefix`: it is what keeps the paths this code
    /// forms inspectable rather than compiled in.
    pub root: PathBuf,
    /// This binary's own version, for the core's row.
    pub core_version: &'static str,
    /// How the core leaves once its binary has been replaced. The same hook
    /// the System tab's restart button uses, and for the same reason: mpv must
    /// die with it, and `std::process::exit` runs no `Drop`.
    pub restart: crate::system::RestartHook,
}

impl Worker {
    async fn message(&self, key: &str) -> String {
        self.catalog.read().await.get(key).to_string()
    }

    /// A catalog message with its one named parameter filled in. Named and
    /// never concatenated: a number glued to a label is not translatable, a
    /// lesson already paid for here.
    async fn message_for(&self, key: &str, component: &str) -> String {
        self.message(key).await.replace("{component}", component)
    }

    async fn set_busy(&self, busy: Option<String>) {
        self.state.write().await.busy = busy;
    }

    /// Publishes a failure. `CheckOutcome::Failed` is the only free-text
    /// channel this payload has, and the page reads it once `busy` has gone
    /// back to `None` — which `run_worker` does at the end of every job,
    /// whatever happened, so no path can leave the buttons disabled.
    async fn publish_failure(&self, message: String) {
        tracing::warn!("update: {message}");
        let mut state = self.state.write().await;
        state.outcome = CheckOutcome::Failed(message);
    }

    /// What the core knows about its plugins, before the release is consulted.
    ///
    /// Two sources, and neither alone is enough: `plugins.toml` says what is
    /// declared and where its binary should be, and the status lines say what
    /// each plugin announced about itself.
    async fn installed(&self) -> Vec<Installed> {
        let manifest = match PluginManifest::load(&self.manifest) {
            Ok(m) => m,
            Err(e) => {
                // An unreadable manifest is not a reason to report every
                // plugin as absent: answering an empty list keeps the core's
                // own row honest and says nothing about plugins rather than
                // saying something false.
                tracing::warn!("update: reading {}: {e:#}", self.manifest.display());
                return Vec::new();
            }
        };
        let statuses = self.status.read().await;
        manifest
            .plugins
            .iter()
            .map(|p| {
                let line = statuses.plugins.iter().find(|l| l.name == p.name);
                Installed {
                    name: p.name.clone(),
                    declared: true,
                    binary_present: Path::new(&p.exec).exists(),
                    version: line.and_then(|l| l.version.clone()),
                    // Relayed from the announcement in Task 17; until then no
                    // plugin can declare a repository, so none is third-party.
                    third_party_repo: None,
                }
            })
            .collect()
    }

    /// The check. Two small requests — the release list and nothing else — and
    /// no archive: knowing in advance whether every component is installable
    /// would mean fetching eleven archives a day for a fact that changes once
    /// per release.
    ///
    /// Returns the fold so a scheduled run can install from it without asking
    /// GitHub the same question twice.
    async fn check(&self, client: &reqwest::Client) -> Option<Vec<Published>> {
        self.set_busy(Some(self.message("update_checking").await)).await;
        let (status, body) = match fetch_text(client, &releases_url()).await {
            Ok(answer) => answer,
            Err(e) => {
                let message = self
                    .message("update_check_failed")
                    .await
                    .replace("{detail}", &e.to_string());
                self.publish_failure(message).await;
                return None;
            }
        };
        if status != 200 {
            // A non-200 is a failure and must be named as one rather than
            // parsed as a body: the list endpoint answers `200 []` for a
            // repository with no releases, so "no release" never arrives as a
            // status code.
            let message = self
                .message("update_check_failed")
                .await
                .replace("{detail}", &format!("HTTP {status}"));
            self.publish_failure(message).await;
            return None;
        }
        let releases = match parse_releases(&body) {
            Ok(releases) => releases,
            Err(ReleasesError::NoRelease) => {
                // A state, and never a failure: this is what a device sees
                // until the first release is published.
                //
                // The rows are rebuilt against an empty offer rather than
                // left as they were: a repository that has no release offers
                // nothing, and `component_offers` answers `Unknown` for every
                // component — which is the truth, where a leftover
                // "0.3.0 available" from a previous check would be a claim
                // about a release that is no longer there.
                let installed = self.installed().await;
                let mut state = self.state.write().await;
                state.outcome = CheckOutcome::NoRelease;
                state.release_version = None;
                state.release_url = None;
                state.last_check_unix_s = Some(now_unix_s());
                state.components = component_offers(self.core_version, &[], &installed);
                return None;
            }
            Err(ReleasesError::Unreadable) => {
                let message = self
                    .message("update_check_failed")
                    .await
                    .replace("{detail}", "the answer is not a list of releases");
                self.publish_failure(message).await;
                return None;
            }
        };
        let published = fold(&releases, ARCH);
        let installed = self.installed().await;
        let mut components = component_offers(self.core_version, &published, &installed);
        let core = published.iter().find(|p| p.offer == Offer::Core);
        let mut state = self.state.write().await;
        carry_installable(&state.components, &mut components);
        state.outcome = CheckOutcome::Ok;
        state.release_version = core.map(|p| p.version.clone());
        state.release_url = core.map(|p| release_page(&p.release_tag));
        state.last_check_unix_s = Some(now_unix_s());
        state.components = components;
        Some(published)
    }

    /// Installs the named components, plugins first and the core last.
    ///
    /// A component that fails names its cause and the next one is still
    /// attempted: one refusal should say one thing, not cancel a gesture the
    /// operator asked for on five rows. Only the **first** cause reaches the
    /// page, which is the honest limit of a payload with one message field.
    async fn install(&self, client: &reqwest::Client, published: &[Published], names: &[String]) {
        let mut first_failure: Option<String> = None;
        for name in install_order(names) {
            let Some(offered) = published.iter().find(|p| carries(p, &name)) else {
                // A name this release does not carry: a third-party plugin, or
                // a component that dropped out of the hundred-release window.
                // Nothing to install and nothing to say to the page — the row
                // already reads `Unknown`.
                tracing::warn!("update: nothing published for {name}, skipping it");
                continue;
            };
            self.set_busy(Some(self.message_for("update_installing", &name).await))
                .await;
            match self.install_one(client, &name, offered).await {
                Ok(Placed::Plugin) => self.restart_plugin(&name).await,
                Ok(Placed::Core) => {
                    // The end of the gesture, and of this process: the new
                    // binary is on disk, and `Restart=always` is what runs it.
                    // The marker the installer just wrote is what makes that
                    // restart preserving.
                    tracing::info!("update: the core has been replaced, leaving so systemd starts the new one");
                    (self.restart)();
                    return;
                }
                Err(message) => {
                    tracing::warn!("update: installing {name}: {message}");
                    first_failure.get_or_insert(message);
                }
            }
        }
        if let Some(message) = first_failure {
            self.publish_failure(message).await;
        }
    }

    /// One component: room, bytes, digest, archive, staging, and the
    /// privileged unit.
    ///
    /// Every refusal is a catalog message naming the component, because that
    /// string is what the page shows — there is no second place to look.
    async fn install_one(
        &self,
        client: &reqwest::Client,
        name: &str,
        offered: &Published,
    ) -> Result<Placed, String> {
        let is_core = offered.offer == Offer::Core;
        let root = self.root.to_string_lossy().to_string();
        if !enough_room(crate::system::disk_usage(&root), offered.size as usize) {
            return Err(self.message_for("update_no_room", name).await);
        }
        // The digest comes from the release that carries the archive, not from
        // one release-wide file: two components installed in one gesture may
        // legitimately read two different `SHA256SUMS`.
        let Some(checksums_url) = &offered.checksums_url else {
            return Err(self.message_for("update_no_digest", name).await);
        };
        let bytes = fetch_capped(client, &offered.url, COMPRESSED_MAX)
            .await
            .map_err(|e| format!("downloading {name}: {e}"))?;
        let (status, sums_body) = fetch_text(client, checksums_url)
            .await
            .map_err(|e| format!("downloading the checksums of {name}: {e}"))?;
        if status != 200 {
            return Err(self.message_for("update_no_digest", name).await);
        }
        let sums = parse_checksums(&sums_body);
        let file = asset_name(&offered.url);
        if let Err(e) = verify_digest(file, sums.get(file).map(String::as_str), &digest_hex(&bytes))
        {
            // The exact figures go to the log; the page gets the sentence
            // that names the component, which is what its reader can act on.
            tracing::warn!("update: {name}: {e}");
            return Err(match e {
                DownloadError::Digest { .. } => {
                    self.message_for("update_digest_mismatch", name).await
                }
                _ => self.message_for("update_no_digest", name).await,
            });
        }

        let contents = archive::read(&bytes, DECOMPRESSED_MAX)
            .map_err(|e| format!("reading the archive of {name}: {e}"))?;
        // **The core is not judged by this rule, and that is not an
        // oversight.** `installable_from_ui` asks whether an archive holds
        // anything root would have to place outside the plugins directory —
        // the core's own archive always does, since it carries the core binary
        // itself, two systemd units and a polkit rule. Root can form the core
        // binary's path, and the units and the rule are read by nobody here:
        // they are listed so the page can say the release changes them, and
        // never written. A release that changes them says "Action required" in
        // its notes, which is the mechanism the design gives that case.
        if !is_core && !installable_from_ui(&contents.entries) {
            self.remember_manual_step(name).await;
            return Err(self.message_for("update_needs_manual_step", name).await);
        }
        let Some(staged) = download_name(&offered.offer) else {
            return Err(format!("no staged name for {name}"));
        };

        std::fs::create_dir_all(&self.staging)
            .map_err(|e| format!("creating {}: {e}", self.staging.display()))?;
        let action = if is_core {
            let binary = contents
                .core_binary
                .ok_or_else(|| format!("the archive of {name} carries no core binary"))?;
            self.write_staged(&staged, &binary)?;
            Action::PlaceCore { staged: staged.clone() }
        } else {
            let (file, binary) = contents
                .binary
                .ok_or_else(|| format!("the archive of {name} carries no plugin binary"))?;
            self.write_staged(&staged, &binary)?;
            Action::PlacePlugin { file, staged: staged.clone() }
        };
        // Written by the core, unprivileged, because the service already owns
        // `/etc/ritornello`: root has no business touching it, which is what
        // keeps its list of paths down to two.
        self.write_etc_files(&contents.etc_files)?;

        let request = Request { format: REQUEST_FORMAT, actions: vec![action] };
        let request_path = self.staging.join("request.json");
        let text = serde_json::to_string(&request).map_err(|e| format!("the request: {e}"))?;
        std::fs::write(&request_path, text)
            .map_err(|e| format!("writing {}: {e}", request_path.display()))?;

        if let Err(detail) = run_privileged_unit().await {
            // systemctl's own words travel verbatim to the page: when the
            // polkit rule is missing it names the file, which is the whole
            // diagnosis.
            return Err(self
                .message("update_privileged_failed")
                .await
                .replace("{detail}", &detail));
        }
        // The installer **copies** what it places (it renames a copy made
        // inside the target's own directory, since a rename across mounts is
        // not atomic), so the staged binary survives its own installation.
        // Left alone they accumulate: one uncompressed binary per component,
        // for ever, on the SD card of a device with no janitor. Removed
        // best-effort — the bytes are now at their target, and a stale
        // `request.json` naming a file that no longer exists is refused by
        // the installer rather than silently re-applied.
        if let Err(e) = std::fs::remove_file(self.staging.join(&staged)) {
            tracing::debug!("update: leaving {staged} in staging: {e}");
        }
        Ok(if is_core { Placed::Core } else { Placed::Plugin })
    }

    fn write_staged(&self, staged: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.staging.join(staged);
        std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
    }

    /// The locale catalogs and input presets a release owns.
    ///
    /// Written unconditionally, which is why `ETC_PREFIXES` is two named
    /// subdirectories and not `etc/ritornello/` at large: the operator's own
    /// files live in that directory too.
    fn write_etc_files(&self, files: &[(String, Vec<u8>)]) -> Result<(), String> {
        for (path, bytes) in files {
            let target = self.root.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("creating {}: {e}", parent.display()))?;
            }
            std::fs::write(&target, bytes)
                .map_err(|e| format!("writing {}: {e}", target.display()))?;
        }
        Ok(())
    }

    /// Is this plugin switched on in `plugins.toml`?
    ///
    /// `false` for a name the file does not declare and for a file that
    /// cannot be read: both mean the core has no basis for launching a
    /// process, and the loop would refuse it anyway for want of an `exec`.
    fn enabled(&self, name: &str) -> bool {
        match PluginManifest::load(&self.manifest) {
            Ok(manifest) => manifest
                .plugins
                .iter()
                .find(|p| p.name == name)
                .is_some_and(|p| p.enabled),
            Err(e) => {
                tracing::warn!("update: reading {}: {e:#}", self.manifest.display());
                false
            }
        }
    }

    /// Marks the row so the page keeps saying so, and so the automatic policy
    /// does not try this archive again every night.
    async fn remember_manual_step(&self, name: &str) {
        let mut state = self.state.write().await;
        for row in state.components.iter_mut().filter(|c| c.name == name) {
            row.installable = Some(false);
        }
    }

    /// Its binary has just been replaced: the core loop stops the process and
    /// launches it again, and the plugin announces itself as it would after
    /// any manual relaunch.
    ///
    /// **A plugin the operator switched off stays switched off.** Its new
    /// binary is on disk and will be the one that runs when it is switched
    /// back on — but `hot_unplug` on a stopped plugin succeeds (there is
    /// nothing to kill), so the restart would go straight on to `relaunch`
    /// and start a process nobody asked for. `plugins.toml` is the authority
    /// on that choice, read here rather than remembered, exactly as
    /// `plugin_enabled_put` reads it.
    ///
    /// A plugin that is enabled and simply **dead** is a different case and
    /// is relaunched: replacing the binary and starting it again is precisely
    /// the gesture that used to require a restart of the whole core after a
    /// plugin was refused for its protocol.
    async fn restart_plugin(&self, name: &str) {
        if !self.enabled(name) {
            tracing::info!(
                "update: {name} is switched off, its new binary will be used the next time it is switched on"
            );
            return;
        }
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let order =
            PluginOrder { name: name.to_string(), action: PluginAction::Restart, ack: ack_tx };
        if self.plugins_tx.send(order).await.is_err() {
            tracing::warn!("update: the core loop is gone, {name} keeps running its old binary");
            return;
        }
        match tokio::time::timeout(RESTART_ACK_TIMEOUT, ack_rx).await {
            Ok(Ok(true)) => tracing::info!("update: {name} replaced and restarted"),
            // `relaunch` answering `false` is already in the log with its
            // cause, which the UI shows. The new binary is on disk either way.
            Ok(Ok(false)) => tracing::warn!("update: {name} was replaced but would not start again"),
            Ok(Err(_)) => tracing::warn!("update: no acknowledgment for the restart of {name}"),
            Err(_) => tracing::warn!(
                "update: the core loop did not acknowledge the restart of {name} within {} s",
                RESTART_ACK_TIMEOUT.as_secs()
            ),
        }
    }
}

/// Seconds since the epoch, or zero.
///
/// Zero rather than a branch: it only ever feeds `last_check_unix_s` and
/// `is_fresh`, and `marker::is_fresh` already documents "fresh" as the safe
/// answer for a clock that has not been set yet.
pub fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One job at a time, for the life of the process.
///
/// Serial by construction: two installs at once would race on the staging
/// directory, and there is nothing here worth doing in parallel.
pub async fn run_worker(worker: Worker, mut rx: mpsc::Receiver<Job>) {
    while let Some(job) = rx.recv().await {
        let client = match client() {
            Ok(client) => client,
            Err(e) => {
                let message = worker
                    .message("update_check_failed")
                    .await
                    .replace("{detail}", &e.to_string());
                worker.publish_failure(message).await;
                worker.set_busy(None).await;
                continue;
            }
        };
        match job {
            Job::Check => {
                worker.check(&client).await;
            }
            Job::Install(names) => {
                // A check first, always: it is what gives the download URLs
                // and the digests of the release as it stands right now, and
                // it costs two small requests next to an archive.
                if let Some(published) = worker.check(&client).await {
                    worker.install(&client, &published, &names).await;
                }
            }
            Job::Scheduled { install } => {
                if let Some(published) = worker.check(&client).await
                    && install
                {
                    let names = automatic_install_list(&worker.state.read().await.components);
                    if names.is_empty() {
                        tracing::debug!("update: scheduled run, nothing to install");
                    } else {
                        tracing::info!("update: scheduled run installing {names:?}");
                        worker.install(&client, &published, &names).await;
                    }
                }
            }
        }
        worker.set_busy(None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_updater::marker::Marker;

    fn marker(at: u64) -> Marker {
        Marker {
            at_unix_s: at,
            core_replaced: true,
            placed: vec!["core".into()],
            removed: vec![],
        }
    }

    /// The requirement the owner raised, and it is the one that would have
    /// been missed: the startup power setting defaults to **on**, so a restart
    /// at 3 a.m. would wake the active source and start playing music nobody
    /// asked for.
    #[test]
    fn a_restart_right_after_an_install_preserves_whatever_the_player_was_doing() {
        assert_eq!(startup_override(Some(marker(1_000)), 1_002), StartupOverride::Previous);
    }

    #[test]
    fn a_restart_long_after_an_install_obeys_the_setting_again() {
        let stale = 1_000 + ritornello_updater::marker::MARKER_WINDOW_S + 1;
        assert_eq!(startup_override(Some(marker(1_000)), stale), StartupOverride::AsConfigured);
    }

    #[test]
    fn an_ordinary_boot_obeys_the_setting() {
        assert_eq!(startup_override(None, 5_000), StartupOverride::AsConfigured);
    }

    /// The case the owner pointed at, and the reason the marker is not
    /// consumed on read: the rollback puts back the PREVIOUS core, and it must
    /// still find the instruction even though the broken one already read it.
    #[test]
    fn the_reverted_core_finds_the_instruction_the_broken_one_had_already_read() {
        let m = marker(1_000);
        assert_eq!(startup_override(Some(m.clone()), 1_002), StartupOverride::Previous);
        // Read again, by a different binary, seconds later. Same answer.
        assert_eq!(startup_override(Some(m), 1_020), StartupOverride::Previous);
    }

    /// The whole of the rollback case, driven through the file: the reverted
    /// core must find the instruction even though the broken one already read
    /// it.
    ///
    /// Deliberately **not** the same test as the pure one above, and not a
    /// duplicate of it: that one hands the same value in twice, which passes
    /// identically whether the read consumes the marker or not. Only a second
    /// read of the same file can tell those two apart.
    #[test]
    fn a_second_core_reads_the_same_instruction_off_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let applied = ritornello_updater::apply::Applied {
            placed: vec!["core".into()],
            removed: vec![],
            core_replaced: true,
        };
        ritornello_updater::marker::write(dir.path(), &applied, 1_000).unwrap();
        // The core that was just installed reads it...
        assert_eq!(startup_instruction(dir.path(), 1_002), StartupOverride::Previous);
        // ...and so does the one the rollback put back, seconds later.
        assert_eq!(startup_instruction(dir.path(), 1_020), StartupOverride::Previous);
    }

    /// No marker at all: the ordinary boot, and the setting decides.
    #[test]
    fn a_directory_with_no_marker_leaves_the_setting_alone() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(startup_instruction(dir.path(), 1_002), StartupOverride::AsConfigured);
    }

    /// **Why the core is exempt from `installable_from_ui`**, pinned rather
    /// than left as a comment in `install_one`.
    ///
    /// That rule asks whether an archive holds anything root would have to
    /// place outside the plugins directory — and the core's own archive
    /// always does: it carries the core binary itself, the privileged
    /// installer, two systemd units and two polkit rules. Applying the plugin
    /// rule to it would refuse **every core update there will ever be**,
    /// which is a failure nothing else in this module would catch.
    ///
    /// The two lists below are the real output of
    /// `scripts/package-release.sh x86_64-unknown-linux-gnu x86_64`, read off
    /// the archives it produced, with the leading `./` stripped and
    /// directories left as `archive::read` normalises them.
    #[test]
    fn the_core_archive_could_never_pass_the_rule_that_governs_a_plugin() {
        let core = names(&[
            "etc/",
            "etc/polkit-1/",
            "etc/polkit-1/rules.d/",
            "etc/polkit-1/rules.d/52-ritornello-update.rules",
            "etc/polkit-1/rules.d/50-ritornello-power.rules",
            "etc/ritornello/",
            "etc/ritornello/locales/",
            "etc/ritornello/locales/common/",
            "etc/ritornello/locales/common/fr.toml",
            "etc/ritornello/locales/core/",
            "etc/ritornello/locales/core/fr.toml",
            "etc/systemd/",
            "etc/systemd/system/",
            "etc/systemd/system/ritornello.service",
            "etc/systemd/system/ritornello-update.service",
            "etc/systemd/system/ritornello-rollback.service",
            "usr/",
            "usr/local/",
            "usr/local/lib/",
            "usr/local/lib/ritornello/",
            "usr/local/lib/ritornello/ritornello-update",
            "usr/local/bin/",
            "usr/local/bin/ritornello-core",
        ]);
        assert!(!installable_from_ui(&core));

        // The same list for a plugin, so this test cannot pass by the rule
        // having quietly become "nothing is installable".
        let radio = names(&[
            "etc/",
            "etc/ritornello/",
            "etc/ritornello/locales/",
            "etc/ritornello/locales/radio/",
            "etc/ritornello/locales/radio/fr.toml",
            "examples/",
            "examples/stations.example.toml",
            "usr/",
            "usr/local/",
            "usr/local/lib/",
            "usr/local/lib/ritornello/",
            "usr/local/lib/ritornello/plugins/",
            "usr/local/lib/ritornello/plugins/ritornello-plugin-radio",
            "plugins.toml.fragment",
        ]);
        assert!(installable_from_ui(&radio));
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The core exits at the end of its own install, so anything queued behind
    /// it never happens.
    #[test]
    fn the_core_is_installed_after_every_plugin_whatever_order_it_was_asked_in() {
        assert_eq!(install_order(&names(&["core", "radio", "mpd"])), names(&["radio", "mpd", "core"]));
        assert_eq!(install_order(&names(&["radio", "core"])), names(&["radio", "core"]));
        // The order among plugins is the order the page sent, untouched.
        assert_eq!(install_order(&names(&["mpd", "radio"])), names(&["mpd", "radio"]));
        assert_eq!(install_order(&names(&["core"])), names(&["core"]));
    }

    fn row(name: &str, kind: ComponentKind, availability: Availability) -> ComponentOffer {
        ComponentOffer {
            name: name.to_string(),
            kind,
            declared: true,
            binary_present: true,
            installed: Some("0.2.0".to_string()),
            offered: Some("0.3.0".to_string()),
            availability,
            installable: None,
            third_party_repo: None,
        }
    }

    /// The four exclusions of the automatic policy, each on its own row so a
    /// filter that disappeared has somewhere to be caught. Stated positively
    /// too: without the `radio` row, a function that returned nothing at all
    /// would pass.
    #[test]
    fn an_automatic_run_updates_what_is_installed_and_never_adds_or_touches_a_third_party() {
        let mut third_party = row("someones-plugin", ComponentKind::ThirdParty, Availability::UpdateAvailable);
        third_party.third_party_repo = Some("someone/theirs".to_string());
        let mut refused = row("files", ComponentKind::Plugin, Availability::UpdateAvailable);
        refused.installable = Some(false);
        // Switched off, so it never announced a version: `differs` says yes
        // against every release for ever, and without the guard this row
        // alone would make the device re-download the same archive nightly.
        let mut silent = row("console", ComponentKind::Plugin, Availability::UpdateAvailable);
        silent.installed = None;
        let components = vec![
            row("core", ComponentKind::Core, Availability::UpdateAvailable),
            row("radio", ComponentKind::Plugin, Availability::UpdateAvailable),
            row("mpd", ComponentKind::Plugin, Availability::Aligned),
            row("cd", ComponentKind::Plugin, Availability::NotInstalled),
            row("console", ComponentKind::Plugin, Availability::BinaryMissing),
            row("legacy", ComponentKind::Plugin, Availability::Unknown),
            third_party,
            refused,
            silent,
        ];
        assert_eq!(automatic_install_list(&components), names(&["core", "radio"]));
    }

    /// Installability is only ever learnt at the moment of a gesture, so a
    /// check that forgot it would send the automatic policy back to the same
    /// archive every night.
    #[test]
    fn a_check_remembers_that_a_component_needs_a_manual_step() {
        let mut previous = row("files", ComponentKind::Plugin, Availability::UpdateAvailable);
        previous.installable = Some(false);
        let mut fresh = vec![row("files", ComponentKind::Plugin, Availability::UpdateAvailable)];
        carry_installable(&[previous], &mut fresh);
        assert_eq!(fresh[0].installable, Some(false));
    }

    /// A new version is a new archive, and nothing is known about it yet.
    #[test]
    fn a_newly_published_version_is_not_judged_on_the_previous_one() {
        let mut previous = row("files", ComponentKind::Plugin, Availability::UpdateAvailable);
        previous.installable = Some(false);
        previous.offered = Some("0.2.9".to_string());
        let mut fresh = vec![row("files", ComponentKind::Plugin, Availability::UpdateAvailable)];
        carry_installable(&[previous], &mut fresh);
        assert_eq!(fresh[0].installable, None);
    }

    /// The three shapes `SHA256SUMS` can take for one archive, and only one
    /// of them installs anything.
    #[test]
    fn an_archive_installs_only_when_its_published_digest_matches() {
        assert!(verify_digest("x.tar.gz", Some("abc"), "abc").is_ok());
        // A truncated or mis-generated checksum file: `parse_checksums` skips
        // the broken line, so what reaches here is an absence. Treating it as
        // "nothing to check" would install unverified bytes.
        assert!(matches!(
            verify_digest("x.tar.gz", None, "abc"),
            Err(DownloadError::NoDigest(name)) if name == "x.tar.gz"
        ));
        assert!(matches!(
            verify_digest("x.tar.gz", Some("abc"), "abd"),
            Err(DownloadError::Digest { .. })
        ));
        // Case is not normalised on either side: drift fails closed.
        assert!(matches!(
            verify_digest("x.tar.gz", Some("ABC"), "abc"),
            Err(DownloadError::Digest { .. })
        ));
    }

    #[test]
    fn the_asset_name_is_what_sha256sums_keys_on() {
        assert_eq!(
            asset_name("https://github.com/x/y/releases/download/v0.3.0/ritornello-plugin-radio-0.2.4-armv7.tar.gz"),
            "ritornello-plugin-radio-0.2.4-armv7.tar.gz"
        );
    }
}
