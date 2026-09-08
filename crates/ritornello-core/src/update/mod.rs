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
use crate::update::archive::{core_not_installed, installable_from_ui, DECOMPRESSED_MAX};
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

/// Carries the core's own archive note across a check.
///
/// Unlike `installable`, this describes the **installed** core — the archive
/// that put the currently running binary there — and not the offered one, so
/// it must survive even a check that changes what is offered: only another
/// core install ever produces a fresh value, never a check on its own. Keyed
/// on `ComponentKind::Core` alone rather than name-plus-offered, because
/// there is exactly one core row and its identity does not depend on what a
/// release happens to offer next.
fn carry_core_notes(previous: &[ComponentOffer], fresh: &mut [ComponentOffer]) {
    let note = previous
        .iter()
        .find(|p| p.kind == ComponentKind::Core)
        .and_then(|p| p.not_installed_files.clone());
    if let Some(core) = fresh.iter_mut().find(|c| c.kind == ComponentKind::Core) {
        core.not_installed_files = note;
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

/// Why one component could not be installed.
///
/// An enum and not a ready-made sentence, and that is the point: the sentence
/// belongs to the catalog, and building it needs a lock this code path cannot
/// take inside a `map_err`. So the refusal travels as a reason plus its
/// technical detail, and `refusal_message` turns it into what the page reads —
/// once, in one place, which is also what makes "every refusal is translated"
/// something a test can check rather than a habit.
#[derive(Debug)]
enum Refusal {
    NoRoom,
    /// The release publishes no digest for this archive: no checksum file at
    /// all, or no line for this file in it.
    NoDigest,
    DigestMismatch,
    NeedsManualStep,
    /// The archive, or its checksum file, could not be fetched.
    Download(String),
    /// Everything between having the bytes and having asked systemd: reading
    /// the archive, writing the staged binary, the `/etc/ritornello` files,
    /// the request.
    Prepare(String),
    /// The privileged unit refused or could not be started. Carries
    /// systemctl's own words.
    Privileged(String),
}

impl std::fmt::Display for Refusal {
    /// The **untruncated** technical text, for the log. The page gets
    /// `refusal_message` instead.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRoom => write!(f, "not enough free space"),
            Self::NoDigest => write!(f, "no published digest for this archive"),
            Self::DigestMismatch => write!(f, "the download does not match its published digest"),
            Self::NeedsManualStep => {
                write!(f, "the archive carries something the core may not install")
            }
            Self::Download(d) | Self::Prepare(d) | Self::Privileged(d) => write!(f, "{d}"),
        }
    }
}

/// The sentence the page shows for one refusal.
///
/// Pure over the catalog, so a test can walk every variant and prove that each
/// one resolves to a real entry with its parameters filled in — the global
/// constraint (user-facing text through the catalog, named parameters, both
/// languages) applies to this path as much as to any other, and a `format!`
/// here would reach a French screen in English.
fn refusal_message(catalog: &Catalog, component: &str, why: &Refusal) -> String {
    let (key, detail) = match why {
        Refusal::NoRoom => ("update_no_room", None),
        Refusal::NoDigest => ("update_no_digest", None),
        Refusal::DigestMismatch => ("update_digest_mismatch", None),
        Refusal::NeedsManualStep => ("update_needs_manual_step", None),
        Refusal::Download(d) => ("update_download_failed", Some(d)),
        Refusal::Prepare(d) => ("update_install_failed", Some(d)),
        Refusal::Privileged(d) => ("update_privileged_failed", Some(d)),
    };
    let text = catalog.get(key).replace("{component}", component);
    match detail {
        Some(d) => text.replace("{detail}", d),
        None => text,
    }
}

/// Is every one of these lines describing a plugin that has finished having
/// its say?
///
/// `starting` means launched and not yet heard from; `stalled` means alive,
/// silent, and past its deadline but still able to speak — the registration
/// socket stays open for it. Both are lines that carry **no version yet and
/// may still gain one**, and reading a version off them is reading a `None`
/// that means "wait", not "unknown".
///
/// Every other shape is settled and stays settled without anything else
/// happening: announced (its version is there), disconnected, switched off,
/// binary absent.
fn lines_settled(status: &StatusState, only: Option<&str>) -> bool {
    status
        .plugins
        .iter()
        .filter(|line| only.is_none_or(|name| line.name == name))
        .all(|line| !line.starting && !line.stalled)
}

/// Waits until the status lines have settled, or gives up.
///
/// **Polling, and there is no channel to wait on instead**: the status lines
/// are an `RwLock` that a dozen sites in `main` update in place, and giving
/// them a notification channel to serve one reader would be a new invariant
/// for every one of those sites to remember.
///
/// Answers whether it settled, so the caller can say in the log that it read
/// a device that had not finished starting. Bounded, because a plugin that is
/// alive and silent for ever is a state this product explicitly has a word
/// for, and waiting on it is not an option.
async fn await_settled(
    status: &RwLock<StatusState>,
    only: Option<&str>,
    within: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if lines_settled(&*status.read().await, only) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// How long the worker gives the plugins to finish speaking before it reads a
/// version off their lines.
///
/// The two moments that need it are the same moment seen twice: a check that
/// runs seconds after boot, and a check that runs seconds after a plugin was
/// relaunched with a new binary. In both, a line that has not settled says
/// `version: None`, which `differs` reads as "out of step with every release"
/// — so without this wait the boot-time catch-up run would judge every
/// still-silent plugin unknown, skip it, and burn the day.
///
/// Fifteen seconds, one notch above `STARTUP_TIMEOUT` in `main`: past that
/// deadline the core itself has given up on a silent plugin and written
/// `stalled` on its line, so waiting longer would only be waiting on a state
/// nothing is going to change.
const SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Coarse on purpose: this runs at most twice a day, and what it is waiting
/// for takes seconds.
const SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Writes through a temporary beside the target, then `rename`.
///
/// The third copy of a three-line rule in this repository, and it is written
/// out rather than shared because neither of the other two fits: `plugins.rs`'s
/// takes a `&str` and derives its temporary name from a `.toml` extension it
/// assumes, and the privileged crate's is `pub(crate)` to a crate that must
/// not gain a dependant. A fourth copy would be the sign that this belongs in
/// a crate of its own — this one names the file rather than its extension, so
/// it works for a locale catalog and an input preset alike.
/// Where the core's own archive note lives: beside the staging area, which
/// this same unprivileged service already owns and creates before a download
/// starts. Public so `main` can read it back at boot with the same path —
/// see `read_core_archive_notes` there, the counterpart of
/// `read_rollback_report`.
pub fn core_notes_path(staging: &Path) -> PathBuf {
    staging.join("core-archive-notes.json")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // The rename error is the one worth reporting; a cleanup that fails in
        // turn must not mask it.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// What the page is told once an install pass is over, or `None` when the
/// pass changed nothing and the check's own answer must stand.
///
/// **A failure wins over a success**, and that is the decision worth pulling
/// out here: a gesture asked for on five rows where one refused must not read
/// as a clean install. Every failure is in the log with its component; the
/// payload has one message field, so the page gets the one that needs acting
/// on.
///
/// Among successes it is the **last** that is reported, because §9 asks this
/// field for "le compte rendu de la dernière tentative" — the most recent
/// thing that happened. With one component installed, which is the ordinary
/// gesture, there is nothing to choose between.
fn install_report(
    catalog: &Catalog,
    placed: &[(String, String)],
    failure: Option<String>,
) -> Option<CheckOutcome> {
    if let Some(message) = failure {
        return Some(CheckOutcome::Failed(message));
    }
    let (component, version) = placed.last()?;
    Some(CheckOutcome::Installed(
        catalog
            .get("update_installed")
            .replace("{component}", component)
            .replace("{version}", version),
    ))
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

    /// The same list, but only once the plugins have finished speaking.
    ///
    /// **This is the whole of the boot-time catch-up repair.** The scheduler's
    /// first tick fires as the main loop starts, which can be moments before a
    /// slow plugin has been hot-wired; its line then carries no version,
    /// `differs(None, offered)` is true against every release, and the run
    /// would leave that plugin's row reading "not installed / update
    /// available" until the next check — a day later, since the day has
    /// already been noted. Waiting is what makes the catch-up run see the
    /// device it is judging.
    ///
    /// Every caller that reads a version goes through here rather than through
    /// `installed` directly, so a manual check clicked three seconds after
    /// boot gets the same answer as one clicked an hour later.
    async fn installed_when_settled(&self) -> Vec<Installed> {
        if !await_settled(&self.status, None, SETTLE_TIMEOUT).await {
            tracing::warn!(
                "update: some plugins were still silent after {} s; their rows will say what is known so far",
                SETTLE_TIMEOUT.as_secs()
            );
        }
        self.installed().await
    }

    /// What the core knows about its plugins, before the release is consulted.
    ///
    /// Two sources, and neither alone is enough: `plugins.toml` says what is
    /// declared and where its binary should be, and the status lines say what
    /// each plugin announced about itself.
    ///
    /// Read through `installed_when_settled`, never directly: a line that has
    /// not settled carries a `None` version that means "wait", not "unknown".
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
                let installed = self.installed_when_settled().await;
                let mut components = component_offers(self.core_version, &[], &installed);
                let mut state = self.state.write().await;
                carry_core_notes(&state.components, &mut components);
                state.outcome = CheckOutcome::NoRelease;
                state.release_version = None;
                state.release_url = None;
                state.last_check_unix_s = Some(now_unix_s());
                state.components = components;
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
        let installed = self.installed_when_settled().await;
        let mut components = component_offers(self.core_version, &published, &installed);
        let core = published.iter().find(|p| p.offer == Offer::Core);
        let mut state = self.state.write().await;
        carry_installable(&state.components, &mut components);
        carry_core_notes(&state.components, &mut components);
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
        // `(component, version)` per plugin actually placed. The core is never
        // in here: it exits at the end of its own install and this function
        // has already returned.
        let mut placed: Vec<(String, String)> = Vec::new();
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
                Ok(Placed::Plugin) => {
                    self.restart_plugin(&name).await;
                    placed.push((name.clone(), offered.version.clone()));
                }
                Ok(Placed::Core) => {
                    // The end of the gesture, and of this process: the new
                    // binary is on disk, and `Restart=always` is what runs it.
                    // The marker the installer just wrote is what makes that
                    // restart preserving.
                    tracing::info!("update: the core has been replaced, leaving so systemd starts the new one");
                    (self.restart)();
                    return;
                }
                Err(why) => {
                    // The untruncated technical text to the log, the catalog
                    // sentence to the page: they are different audiences, and
                    // only one of them reads French.
                    tracing::warn!("update: installing {name}: {why}");
                    let catalog = self.catalog.read().await;
                    let message = refusal_message(&catalog, &name, &why);
                    drop(catalog);
                    first_failure.get_or_insert(message);
                }
            }
        }
        self.conclude_install(published, &placed, first_failure).await;
    }

    /// The end of an install pass: the rows the page reads, and the report of
    /// what just happened.
    ///
    /// **Without this, a successful install is indistinguishable from nothing
    /// having happened.** `busy` clears, `outcome` still says the `Ok` the
    /// check left, and the row still reads "0.2.0 → 0.3.0 available" — which
    /// reads as a failure to whoever just clicked Install. Two observables
    /// change here, and both matter: the row goes to "up to date", and the
    /// card gets a sentence naming what was installed.
    ///
    /// The rows are rebuilt only when something was actually placed — a pass
    /// that refused everything has nothing new to say about the device, and
    /// waiting on the plugins to settle for it would be waiting for nothing.
    /// They come from the **same** `published` the check returned, so there is
    /// no second GitHub round trip and no window between what was seen and
    /// what was installed; and through `installed_when_settled`, because the
    /// restarted plugin re-announces asynchronously and a row rebuilt an
    /// instant too early would say `installed: null` — briefly *more* wrong
    /// than the stale one. `carry_installable` runs here for the same reason
    /// it runs after a check: a component refused for a manual step must not
    /// forget it.
    async fn conclude_install(
        &self,
        published: &[Published],
        placed: &[(String, String)],
        failure: Option<String>,
    ) {
        if !placed.is_empty() {
            let installed = self.installed_when_settled().await;
            let mut components = component_offers(self.core_version, published, &installed);
            let mut state = self.state.write().await;
            carry_installable(&state.components, &mut components);
            carry_core_notes(&state.components, &mut components);
            state.components = components;
        }
        let report = {
            let catalog = self.catalog.read().await;
            install_report(&catalog, placed, failure)
        };
        let Some(report) = report else { return };
        match &report {
            CheckOutcome::Failed(message) => tracing::warn!("update: {message}"),
            other => tracing::info!("update: {other:?}"),
        }
        self.state.write().await.outcome = report;
    }

    /// One component: room, bytes, digest, archive, staging, and the
    /// privileged unit.
    ///
    /// Refuses with a `Refusal` and never with a sentence: the sentence comes
    /// from the catalog, and this function is not where the catalog lock
    /// belongs. `install` builds it, once, for whatever comes back.
    async fn install_one(
        &self,
        client: &reqwest::Client,
        name: &str,
        offered: &Published,
    ) -> Result<Placed, Refusal> {
        let is_core = offered.offer == Offer::Core;
        let root = self.root.to_string_lossy().to_string();
        if !enough_room(crate::system::disk_usage(&root), offered.size as usize) {
            return Err(Refusal::NoRoom);
        }
        // The digest comes from the release that carries the archive, not from
        // one release-wide file: two components installed in one gesture may
        // legitimately read two different `SHA256SUMS`.
        let Some(checksums_url) = &offered.checksums_url else {
            return Err(Refusal::NoDigest);
        };
        let bytes = fetch_capped(client, &offered.url, COMPRESSED_MAX)
            .await
            .map_err(|e| Refusal::Download(format!("the archive of {name}: {e}")))?;
        let (status, sums_body) = fetch_text(client, checksums_url)
            .await
            .map_err(|e| Refusal::Download(format!("the checksums of {name}: {e}")))?;
        if status != 200 {
            tracing::warn!("update: {checksums_url} answered HTTP {status}");
            return Err(Refusal::NoDigest);
        }
        let sums = parse_checksums(&sums_body);
        let file = asset_name(&offered.url);
        if let Err(e) = verify_digest(file, sums.get(file).map(String::as_str), &digest_hex(&bytes))
        {
            // The exact figures go to the log; the page gets the sentence
            // that names the component, which is what its reader can act on.
            tracing::warn!("update: {name}: {e}");
            return Err(match e {
                DownloadError::Digest { .. } => Refusal::DigestMismatch,
                _ => Refusal::NoDigest,
            });
        }

        let contents = archive::read(&bytes, DECOMPRESSED_MAX)
            .map_err(|e| Refusal::Prepare(format!("reading the archive of {name}: {e}")))?;
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
            return Err(Refusal::NeedsManualStep);
        }
        let Some(staged) = download_name(&offered.offer) else {
            return Err(Refusal::Prepare(format!("no staged name for {name}")));
        };

        std::fs::create_dir_all(&self.staging)
            .map_err(|e| Refusal::Prepare(format!("creating {}: {e}", self.staging.display())))?;
        // Read now, while `contents` still has its `entries`: a successful
        // core placement ends with this process exiting a few lines below, so
        // by the time anyone could read the note back from `self.state` the
        // process that computed it is gone. See `write_core_archive_notes`.
        let core_notes = is_core.then(|| core_not_installed(&contents.entries));
        let action = if is_core {
            let binary = contents.core_binary.ok_or_else(|| {
                Refusal::Prepare(format!("the archive of {name} carries no core binary"))
            })?;
            self.write_staged(&staged, &binary)?;
            Action::PlaceCore { staged: staged.clone() }
        } else {
            let (file, binary) = contents.binary.ok_or_else(|| {
                Refusal::Prepare(format!("the archive of {name} carries no plugin binary"))
            })?;
            self.write_staged(&staged, &binary)?;
            Action::PlacePlugin { file, staged: staged.clone() }
        };
        // Written by the core, unprivileged, because the service already owns
        // `/etc/ritornello`: root has no business touching it, which is what
        // keeps its list of paths down to two.
        //
        // Written **before** the unit runs, so a unit that then fails leaves
        // the new locale catalogs beside the old binary. Harmless as things
        // stand — `Catalog::get` falls back to the embedded English and, past
        // that, to the key itself — and the alternative (placing them after)
        // would leave the new binary beside the old catalogs, which is the
        // same mismatch the other way round with no fallback at all.
        self.write_etc_files(&contents.etc_files)?;

        let request = Request { format: REQUEST_FORMAT, actions: vec![action] };
        let request_path = self.staging.join("request.json");
        let text = serde_json::to_string(&request)
            .map_err(|e| Refusal::Prepare(format!("the request: {e}")))?;
        std::fs::write(&request_path, text)
            .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", request_path.display())))?;

        if let Err(detail) = run_privileged_unit().await {
            // systemctl's own words travel verbatim to the page: when the
            // polkit rule is missing it names the file, which is the whole
            // diagnosis.
            return Err(Refusal::Privileged(detail));
        }
        // Written only once the placement actually succeeded: a refusal
        // above must not claim a note about a core that was never placed.
        if let Some(entries) = &core_notes {
            self.write_core_archive_notes(entries);
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

    fn write_staged(&self, staged: &str, bytes: &[u8]) -> Result<(), Refusal> {
        let path = self.staging.join(staged);
        std::fs::write(&path, bytes)
            .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", path.display())))
    }

    /// Records what this core's own archive did not install, so the row can
    /// still say so after the restart that follows a successful placement.
    ///
    /// Best-effort and never a `Refusal`: a release note the page fails to
    /// show is a much smaller loss than an install refused over writing it,
    /// and by the time this runs the binary is already placed.
    fn write_core_archive_notes(&self, entries: &[String]) {
        let path = core_notes_path(&self.staging);
        let text = match serde_json::to_string(entries) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("update: encoding the core's archive note: {e}");
                return;
            }
        };
        if let Err(e) = write_atomic(&path, text.as_bytes()) {
            tracing::warn!("update: writing {}: {e}", path.display());
        }
    }

    /// The locale catalogs and input presets a release owns.
    ///
    /// Written unconditionally, which is why `ETC_PREFIXES` is two named
    /// subdirectories and not `etc/ritornello/` at large: the operator's own
    /// files live in that directory too.
    ///
    /// Through a temporary and a `rename`, like every other file this product
    /// writes: this is a device one unplugs, and a `fr.toml` cut in half by a
    /// power cut is a catalog that no longer parses — every message in it
    /// falls back to its key, on screen.
    fn write_etc_files(&self, files: &[(String, Vec<u8>)]) -> Result<(), Refusal> {
        for (path, bytes) in files {
            let target = self.root.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Refusal::Prepare(format!("creating {}: {e}", parent.display()))
                })?;
            }
            write_atomic(&target, bytes)
                .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", target.display())))?;
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
            // `false` means the core loop refused to *stop* it: `hot_unplug`
            // answers `false` only for a plugin whose process the core does
            // not own, and nothing was tried to start. So the old process is
            // still running the old binary, and the new one sits on disk
            // waiting for someone to kill it. The loop's own log names the
            // remedy.
            Ok(Ok(false)) => tracing::warn!(
                "update: {name} was replaced on disk, but the core does not own its process and could not stop it — the old one is still running"
            ),
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
    use crate::status::PluginStatus;
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
    ///
    /// **What this does not prove**, and it should be read for exactly what it
    /// is: it pins the *rule's* answer, not the *branch* in `install_one`.
    /// Dropping the `!is_core &&` there would leave every test in this module
    /// green. That is the worker-is-untested doctrine, and it is why the
    /// reason lives in a comment at the call site as well as here.
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
            not_installed_files: None,
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

    /// The counterpart of `a_check_remembers_that_a_component_needs_a_manual_step`
    /// for the core's own note: a plain check must not erase it, because only
    /// another core install ever produces a fresh one.
    #[test]
    fn a_check_remembers_the_core_archive_note() {
        let mut previous = row("core", ComponentKind::Core, Availability::Aligned);
        previous.not_installed_files = Some(vec!["etc/systemd/system/ritornello.service".to_string()]);
        let mut fresh = vec![row("core", ComponentKind::Core, Availability::UpdateAvailable)];
        carry_core_notes(&[previous], &mut fresh);
        assert_eq!(
            fresh[0].not_installed_files,
            Some(vec!["etc/systemd/system/ritornello.service".to_string()])
        );
    }

    /// The note is a fact about the **installed** core, not about what a
    /// release offers next: unlike `installable`, a change of `offered` must
    /// not reset it — the test that would catch a wrongly-keyed
    /// implementation copying `carry_installable`'s pairing verbatim.
    #[test]
    fn the_core_note_survives_a_new_offered_version_unlike_installable() {
        let mut previous = row("core", ComponentKind::Core, Availability::UpdateAvailable);
        previous.offered = Some("0.3.0".to_string());
        previous.not_installed_files = Some(vec!["usr/local/lib/ritornello/ritornello-update".to_string()]);
        let mut fresh = vec![row("core", ComponentKind::Core, Availability::UpdateAvailable)];
        fresh[0].offered = Some("0.4.0".to_string());
        carry_core_notes(&[previous], &mut fresh);
        assert_eq!(
            fresh[0].not_installed_files,
            Some(vec!["usr/local/lib/ritornello/ritornello-update".to_string()])
        );
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

    /// A release's locale catalog lands whole or not at all.
    ///
    /// The cheap proof that the write went through a `rename` rather than
    /// straight onto the target — the same shape the privileged crate uses
    /// for its own manifest: nothing named after the temporary survives, and
    /// the temporary is not the target. A truncated `fr.toml` is a catalog
    /// that no longer parses, and every message in it falls back to its key,
    /// on screen.
    #[test]
    fn a_locale_catalog_is_written_through_a_temporary_and_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fr.toml");
        write_atomic(&target, b"hello = \"bonjour\"\n").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello = \"bonjour\"\n");
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "fr.toml")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// The French pack this repository ships, loaded as a real catalog rather
    /// than parsed as a table: what the test needs to know is what a French
    /// screen would actually receive.
    fn french() -> Catalog {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        Catalog::load("core", "fr", &root, crate::i18n::EN)
    }

    /// **Every refusal reaches the page as a translated sentence.** A
    /// `format!` on this path is a French screen reading English, which is
    /// the constraint this repository states first and has already paid for
    /// once.
    ///
    /// The list is written out, one entry per variant, rather than derived:
    /// that is what makes a new variant have to appear here before it can
    /// reach a screen. Both catalogs, because a French string that dropped
    /// `{component}` would say "could not be downloaded" about nothing in
    /// particular, and the parity test only compares key *sets*.
    #[test]
    fn every_refusal_is_a_translated_sentence_with_its_parameters_filled_in() {
        let english = Catalog::load(
            "core",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::i18n::EN,
        );
        let all = [
            Refusal::NoRoom,
            Refusal::NoDigest,
            Refusal::DigestMismatch,
            Refusal::NeedsManualStep,
            Refusal::Download("connection reset by peer".to_string()),
            Refusal::Prepare("no space left on device".to_string()),
            Refusal::Privileged("Job for ritornello-update.service failed".to_string()),
        ];
        for catalog in [&english, &french()] {
            for why in &all {
                let message = refusal_message(catalog, "radio", why);
                // `Catalog::get` answers the key itself when it knows none,
                // so a key missing from either pack shows up here.
                assert!(
                    !message.starts_with("update_"),
                    "{why:?} fell through to its own key: {message}"
                );
                assert!(
                    !message.contains('{'),
                    "{why:?} left a parameter unfilled: {message}"
                );
            }
        }
    }

    // ---- The worker, against a temporary root ---------------------------
    //
    // Not a test of the worker as a whole: it fetches from GitHub and starts
    // a systemd unit, neither of which exists here. What these two drive is
    // the one thing it does that is not I/O — reading the device's own state
    // at the right *moment*.

    /// A worker whose manifest declares one plugin whose binary exists, and
    /// whose status lines the test owns.
    fn worker_rig(status: Arc<RwLock<StatusState>>) -> (Worker, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let exec = dir.path().join("ritornello-plugin-radio");
        std::fs::write(&exec, b"not a real binary, only its presence is read\n").unwrap();
        let manifest = dir.path().join("plugins.toml");
        std::fs::write(
            &manifest,
            format!("[[plugin]]\nname = \"radio\"\nexec = {:?}\n", exec.to_string_lossy()),
        )
        .unwrap();
        let worker = Worker {
            state: Arc::new(RwLock::new(UpdateState::initial("0.2.0", &[]))),
            catalog: Arc::new(RwLock::new(Catalog::load(
                "core",
                "en",
                dir.path(),
                crate::i18n::EN,
            ))),
            status,
            manifest,
            plugins_tx: mpsc::channel(1).0,
            staging: dir.path().join("staging"),
            root: dir.path().to_path_buf(),
            core_version: "0.2.0",
            restart: Arc::new(|| {}),
        };
        (worker, dir)
    }

    fn one_line(line: PluginStatus) -> Arc<RwLock<StatusState>> {
        Arc::new(RwLock::new(StatusState {
            plugins: vec![line],
            active_source: "radio".to_string(),
            protocol: ritornello_proto::PROTOCOL_VERSION,
        }))
    }

    /// Launched, not yet heard from, still inside its deadline. What the core
    /// writes for a plugin it has just relaunched.
    fn starting_line() -> Arc<RwLock<StatusState>> {
        one_line(PluginStatus::startup("radio"))
    }

    /// Alive, silent, past its deadline — and still able to speak, because the
    /// registration socket stays open for it. What the startup rendezvous
    /// leaves for a plugin that was too slow for its ten seconds.
    fn stalled_line() -> Arc<RwLock<StatusState>> {
        one_line(PluginStatus::unknown_kind("radio", true))
    }

    /// Replaces the silent line with the one an announcement produces, after
    /// a moment — the shape of a plugin binding its sockets on an SD card.
    fn announces_shortly(status: Arc<RwLock<StatusState>>, version: &str) {
        let version = version.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            status.write().await.plugins = vec![PluginStatus {
                version: Some(version),
                ..PluginStatus::kind("radio", "source", true, false)
            }];
        });
    }

    /// **The two shapes `lines_settled` calls unsettled, one test each, and
    /// neither can stand in for the other.**
    ///
    /// A line that has not settled carries `version: None`, which
    /// `differs(None, offered)` reads as "out of step with every release":
    /// the row goes to "not installed / update available" until the next
    /// check a day later, and the automatic policy skips the plugin for want
    /// of a known version. That is the run catch-up exists for.
    ///
    /// This one covers `starting` — launched, inside its deadline — which is
    /// what a plugin the core has just relaunched reads as. Its twin below
    /// covers `stalled`, and **that is the shape that happens at boot**: the
    /// rendezvous is awaited before the main loop starts and fills the lines
    /// of everything that announced in time (`main.rs:1564-1600`), so the
    /// plugin the scheduler's first tick can still catch out is the one that
    /// missed the ten-second deadline — and its line says `stalled`, never
    /// `starting`. Dropping either half of the predicate leaves one of these
    /// two green and the other red; that is the point of writing both.
    #[tokio::test]
    async fn a_run_that_starts_before_a_plugin_has_spoken_still_reads_its_version() {
        let status = starting_line();
        let (worker, _dir) = worker_rig(status.clone());
        announces_shortly(status, "0.3.0");
        let installed = worker.installed_when_settled().await;
        let radio = installed.iter().find(|i| i.name == "radio").expect("the declared plugin");
        assert_eq!(
            radio.version.as_deref(),
            Some("0.3.0"),
            "the run read the line while the plugin was still starting"
        );
        assert!(radio.binary_present);
    }

    /// The twin, and the one that describes the real production window: a
    /// plugin too slow for the startup rendezvous is written off as `stalled`
    /// and hot-wired afterwards, so its line gains a version *after* the
    /// scheduler's first tick has already fired.
    ///
    /// A `stalled` plugin is not a dead one — the registration socket stays
    /// open for it and `hotplug` will take its late announcement — which is
    /// exactly why this shape must count as "may still gain a version" and
    /// not as "nothing more to learn".
    #[tokio::test]
    async fn a_run_that_starts_while_a_slow_plugin_is_written_off_still_reads_its_version() {
        let status = stalled_line();
        let (worker, _dir) = worker_rig(status.clone());
        announces_shortly(status, "0.3.0");
        let installed = worker.installed_when_settled().await;
        let radio = installed.iter().find(|i| i.name == "radio").expect("the declared plugin");
        assert_eq!(
            radio.version.as_deref(),
            Some("0.3.0"),
            "the run read the line while the plugin was still reported stalled"
        );
    }

    fn radio_published(version: &str) -> Vec<Published> {
        vec![Published {
            offer: Offer::Plugin("radio".to_string()),
            version: version.to_string(),
            url: format!("https://x/ritornello-plugin-radio-{version}-x86_64.tar.gz"),
            size: 0,
            release_tag: format!("v{version}"),
            checksums_url: None,
        }]
    }

    /// **A successful install must stop looking like nothing happened.**
    /// After the binary is replaced and the plugin restarted, the row still
    /// said "0.2.0 → 0.3.0 available" and `outcome` still said `Ok` until the
    /// next check — which reads as a failure to whoever just clicked Install.
    ///
    /// Both observables in one test, because they are one event: the row goes
    /// to "up to date", and the card gets a sentence naming the component and
    /// its new version.
    ///
    /// Same event shape as the test above, and deliberately so: the restarted
    /// plugin re-announces asynchronously, so a row rebuilt an instant too
    /// early would say `installed: null` — briefly *more* wrong than the
    /// stale one.
    #[tokio::test]
    async fn a_replaced_plugin_stops_being_offered_the_update_it_has_just_had() {
        let status = starting_line();
        let (worker, _dir) = worker_rig(status.clone());
        announces_shortly(status, "0.3.0");
        worker
            .conclude_install(
                &radio_published("0.3.0"),
                &[("radio".to_string(), "0.3.0".to_string())],
                None,
            )
            .await;
        let state = worker.state.read().await;
        let radio = state.components.iter().find(|c| c.name == "radio").expect("the declared plugin");
        assert_eq!(radio.installed.as_deref(), Some("0.3.0"));
        assert_eq!(radio.availability, Availability::Aligned);
        assert_eq!(
            state.outcome,
            CheckOutcome::Installed("radio updated to 0.3.0".to_string())
        );
    }

    /// A pass that placed nothing has nothing new to say about the device, so
    /// it neither waits on the plugins nor rebuilds the rows — and the report
    /// is the refusal, not a success.
    ///
    /// The wait is what this bounds: with a line left `starting` for ever, a
    /// pass that rebuilt the rows anyway would sit here for
    /// `SETTLE_TIMEOUT`. The deadline below is far under it.
    #[tokio::test]
    async fn a_pass_that_placed_nothing_reports_the_refusal_without_waiting() {
        let status = starting_line();
        let (worker, _dir) = worker_rig(status);
        let before = worker.state.read().await.components.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            worker.conclude_install(&radio_published("0.3.0"), &[], Some("nope".to_string())),
        )
        .await
        .expect("a pass that placed nothing waited on the plugins anyway");
        let state = worker.state.read().await;
        assert_eq!(state.outcome, CheckOutcome::Failed("nope".to_string()));
        assert_eq!(state.components, before, "nothing was placed, so nothing moved");
    }

    /// The report a pass ends on, in table form: a failure wins over a
    /// success, the last success is the one named, and a pass that did
    /// nothing leaves the check's own answer alone.
    #[test]
    fn a_failed_component_never_lets_a_pass_read_as_a_clean_install() {
        let english = Catalog::load(
            "core",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::i18n::EN,
        );
        let two = [
            ("radio".to_string(), "0.3.0".to_string()),
            ("mpd".to_string(), "0.3.1".to_string()),
        ];
        assert_eq!(
            install_report(&english, &two, None),
            Some(CheckOutcome::Installed("mpd updated to 0.3.1".to_string())),
            "the most recent attempt is the one reported"
        );
        // Something refused: the page must not read "mpd updated" and leave
        // the operator to find the failure in the log.
        assert_eq!(
            install_report(&english, &two, Some("no room".to_string())),
            Some(CheckOutcome::Failed("no room".to_string()))
        );
        // Nothing placed and nothing refused — every named component was
        // skipped for want of a published archive. The check's own answer
        // stands.
        assert_eq!(install_report(&english, &[], None), None);

        // And the sentence renders in French too, with both parameters: the
        // parity test compares key sets, not what is inside them.
        let french = install_report(&french(), &two, None);
        let Some(CheckOutcome::Installed(message)) = french else { panic!("{french:?}") };
        assert!(!message.contains('{'), "{message}");
        assert!(message.contains("mpd") && message.contains("0.3.1"), "{message}");
    }
}
