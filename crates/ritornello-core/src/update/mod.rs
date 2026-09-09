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
pub mod placed;

pub mod routes;

use crate::plugins::PluginManifest;
use crate::status::{PluginAction, PluginOrder, StatusState};
use crate::update::archive::{
    core_not_installed, installable_from_ui, only_its_own_binary, DECOMPRESSED_MAX,
};
use crate::update::download::{
    client, digest_hex, enough_room, fetch_capped, fetch_text, DownloadError, COMPRESSED_MAX,
};
use crate::update::release::{
    download_name, fold, origin, parse_checksums, parse_releases, releases_url, releases_url_for,
    Channel, Offer, Origin, Published, ReleasesError, ARCH, REPO,
};
use crate::update::state::{
    component_offers, Availability, CheckOutcome, ComponentKind, ComponentOffer, Installed,
    ThirdPartyOffer, UpdateState,
};
use ritornello_i18n::Catalog;
use ritornello_updater::request::{Action, Request, REQUEST_FORMAT};
use ritornello_updater::target::plugins_dir;
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
/// **Two dated files, because two different processes restart this device and
/// only one of them can read the marker.** An install writes the marker, and
/// the core it installed reads it. A rollback *consumes* that marker — it
/// must, or a second `OnFailure=` would roll back twice — and then restarts
/// the core it put back; that core finds no marker at all, and until this
/// function read the second file it fell through to the Startup setting, whose
/// default is *on*, and woke a device that had been asleep. Three documents on
/// this branch, this comment included, asserted the opposite ("read and not
/// consumed"); none of them was true of the rollback unit, and no test on the
/// branch ever ran `rollback()`.
///
/// So the rollback's own restart is authorised by its own dated file: the
/// report it already writes and never deletes (`rollback::is_fresh`). Either
/// file, while fresh, means "this boot was started by something that was
/// already running, so keep doing what it was doing".
pub fn startup_override(
    marker: Option<ritornello_updater::marker::Marker>,
    rollback: Option<&ritornello_updater::rollback::Report>,
    now_unix_s: u64,
) -> StartupOverride {
    let after_an_install =
        marker.is_some_and(|m| ritornello_updater::marker::is_fresh(&m, now_unix_s));
    let after_a_rollback =
        rollback.is_some_and(|r| ritornello_updater::rollback::is_fresh(r, now_unix_s));
    if after_an_install || after_a_rollback {
        StartupOverride::Previous
    } else {
        StartupOverride::AsConfigured
    }
}

/// The instruction this boot must obey, read from the two places it can be
/// written.
///
/// Separated from `startup_override` on purpose: whether a read consumes its
/// file lives in the **read**, not in the decision, and a pure function handed
/// a value twice cannot tell the two apart. This is what a test can drive
/// against a real directory a real `rollback()` has just been through — which
/// is the test that was missing, and the reason the defect above survived
/// twenty reviews.
pub fn startup_instruction(prefix: &Path, now_unix_s: u64) -> StartupOverride {
    startup_override(
        ritornello_updater::marker::read(prefix),
        read_rollback_report(prefix).as_ref(),
        now_unix_s,
    )
}

/// What the rollback unit left behind, if anything.
///
/// `None` for an absent, unreadable or corrupt file, exactly as `marker::read`
/// answers for its own: both readers — the page's note and the startup
/// instruction above — have a correct answer without it. **Never deleted**: it
/// is the only trace of a nocturnal rollback, and the next rollback overwrites
/// it.
pub fn read_rollback_report(prefix: &Path) -> Option<ritornello_updater::rollback::Report> {
    let path = ritornello_updater::rollback::report_path(prefix);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&text) {
        Ok(report) => Some(report),
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", path.display());
            None
        }
    }
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
    /// The slow half of an uninstall (see `status::plugin_status::plugin_delete`):
    /// the declaration is already gone from `plugins.toml` and the core has
    /// already stopped the plugin, and what is left is asking the privileged
    /// unit to erase `file` from the plugins directory.
    ///
    /// Queued rather than run on the request thread for the reason every
    /// other privileged call already is: `run_privileged_unit` can take up to
    /// `PRIVILEGED_TIMEOUT`, and going through this same queue is what keeps
    /// it from racing an install over the staging directory.
    RemovePlugin {
        /// For the log only.
        name: String,
        /// The bare file name in the plugins directory — not necessarily
        /// equal to `name`, and the manifest can no longer answer this
        /// question once the declaration is gone, which is why the caller
        /// carries it here rather than this job re-reading it.
        file: String,
    },
}

/// The name of the core's own row, the name the page sends to install it, and
/// the key its entry takes in `placed::Placed`. Written once here rather than
/// quoted at each of its uses.
pub(crate) const CORE: &str = "core";

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
/// - a component whose **installed version is unknown and that this updater
///   has never placed** is left alone. That is not caution for its own sake: a
///   plugin switched off, or dead, or predating the version field never
///   announces one, so `differs` answers "yes" against every release for ever
///   — and without this line the device would download and install that
///   archive again every single night, learning nothing each time. The
///   operator can still install it by hand, where the gesture is asked for
///   once.
///
///   The second half of that clause is what task 12B added, and it does not
///   loosen the guard: a component this updater never touched stays excluded
///   exactly as before. What it lets back in is the plugin whose **new binary
///   this updater placed** and which then died before announcing — the one
///   case where the version is unknown *because of an update*, and the one
///   case where an automatic repair is worth most. Its cost is bounded by the
///   fifth exclusion below — but only on the **success** path, and that is
///   worth being exact about: the memory is written after the privileged unit
///   has placed the bytes, so a run that fails before that (no room, a bad
///   digest, the unit refused) records nothing and is tried again the next
///   night. That is what already happens for a component whose version *is*
///   known, and it is bounded by the same things; what changes here is only
///   that the previous cost for a switched-off plugin was zero.
///
/// - a component whose **offered version is the one already placed** is left
///   alone. Reaching this line at all means the placement did not take: had it
///   taken, the component would announce that version and its row would read
///   `Aligned` rather than `UpdateAvailable`. So "offered == placed" is
///   exactly "this automatic policy has already tried this archive and the
///   device did not keep it" — a core that crash-looped and was rolled back, a
///   plugin whose new binary dies before it speaks — and trying it again
///   tonight, and every night until a newer release appears, teaches the
///   device nothing and overwrites the rollback report each time.
///
///   Only the **automatic** policy consults this: `Job::Install` never comes
///   through here, so an operator may always retry by hand — which is also the
///   only way out if this memory is ever wrong.
fn automatic_install_list(components: &[ComponentOffer], placed: &placed::Placed) -> Vec<String> {
    components
        .iter()
        .filter(|c| c.availability == Availability::UpdateAvailable)
        .filter(|c| matches!(c.kind, ComponentKind::Core | ComponentKind::Plugin))
        .filter(|c| c.installable != Some(false))
        .filter(|c| c.installed.is_some() || placed.contains_key(&c.name))
        .filter(|c| c.offered.as_deref() != placed::version_of(placed, &c.name))
        .map(|c| c.name.clone())
        .collect()
}

/// How many third-party repositories one check is allowed to query.
///
/// Without a ceiling, ten third-party plugins make ten requests a day to ten
/// different servers, and a single slow host blocks the whole check behind its
/// own timeout — a check that never answers leaves `busy` set and the buttons
/// disabled. Four covers the real use and bounds the worst case; the plugins
/// past it are simply not checked this time, which reads as `Unknown` and
/// never as "up to date".
const THIRD_PARTY_MAX: usize = 4;

/// One repository this check will ask about one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// The plugin's name, which is also the name the asset must carry: a
    /// repository publishing an archive under another name has published
    /// nothing for the binary sitting on this device.
    name: String,
    /// `owner/repo`, kept for the log — the address beside it is already
    /// formed, so nothing downstream ever builds one.
    repo: String,
    /// Where to ask, formed **here** by `release::releases_url_for`. That
    /// keeps the one place a host is chosen the one place a host is chosen:
    /// `third_party_offers` receives an address and never composes one, so no
    /// announced string can reach a URL template.
    url: String,
}

/// The third-party repositories this check will query, and the addresses to
/// query them at.
///
/// Pure, and separated from the requests it feeds, because the ceiling is the
/// one thing about this list that can be wrong without any I/O being involved.
///
/// **In `plugins.toml` order and truncated, never sampled**: the order is
/// already the priority order this product uses everywhere else, and a stable
/// prefix means the same four plugins are checked every day rather than a
/// different four each time.
///
/// An `Origin::Foreign` is deliberately absent: there is no GitHub endpoint to
/// address for it, so it must not consume one of the four either.
fn third_party_targets(installed: &[Installed]) -> Vec<Target> {
    installed
        .iter()
        .filter_map(|p| match origin(p.repository.as_deref()) {
            Origin::ThirdParty(repo) => Some(Target {
                name: p.name.clone(),
                url: releases_url_for(&repo),
                repo,
            }),
            Origin::Unknown | Origin::Ours | Origin::Foreign(_) => None,
        })
        .take(THIRD_PARTY_MAX)
        .collect()
}

/// Every component the announcement says is third-party — **no cap, and
/// `Foreign` included**, unlike `third_party_targets`.
///
/// Two lists out of one reading, and the difference between them is the point:
/// `third_party_targets` answers "whom do we go and ask", which is bounded and
/// only covers repositories we can address; this one answers "what is a
/// stranger", which admits no ceiling at all. A component left out of the four
/// this check consulted, or announcing a repository we cannot address, is not
/// thereby one of ours — and the fall-through that treated it as one is
/// exactly how our own release ends up installed under a colliding name.
fn third_party_names(installed: &[Installed]) -> Vec<String> {
    installed
        .iter()
        .filter(|p| !matches!(origin(p.repository.as_deref()), Origin::Unknown | Origin::Ours))
        .map(|p| p.name.clone())
        .collect()
}

/// What one named component may be installed from.
#[derive(Debug, PartialEq, Eq)]
enum Resolved<'a> {
    /// Our own release publishes it.
    Ours(&'a Published),
    /// Its own repository published it, and that archive is what will be
    /// fetched.
    Theirs(&'a Published),
    /// The announcement says it is a stranger's, and this check did not get an
    /// answer from its repository — over the cap of four, unreachable,
    /// unaddressable, or publishing no archive for this architecture under
    /// this name.
    ///
    /// **Never our release of the same name**, and that is the whole reason
    /// this variant exists rather than falling through: a third-party plugin
    /// keeping its fork's name (`radio`, say) would otherwise be silently
    /// replaced by the official `radio` archive, installed under the plugin
    /// rule with everything that rule allows into `/etc/ritornello`.
    UncheckedThirdParty,
    /// Nothing published carries this name at all: dropped out of the
    /// hundred-release window, or one this release never carried.
    ///
    /// `install`'s own handling of this variant is a named refusal
    /// (`Refusal::NothingPublished`), not a silent skip — see its doc.
    Nothing,
}

/// Which of the two lists answers for this name — decided by **what the
/// component is**, never by which lookup happened to return something.
fn resolve<'a>(checked: &'a Checked, name: &str) -> Resolved<'a> {
    if let Some(offer) = checked.theirs.iter().find(|o| o.name == name) {
        return Resolved::Theirs(&offer.published);
    }
    // Before `ours` and not after it: the membership test is what keeps a
    // stranger's row from ever being served from our release.
    if checked.third_party.iter().any(|n| n == name) {
        return Resolved::UncheckedThirdParty;
    }
    match checked.ours.iter().find(|p| carries(p, name)) {
        Some(published) => Resolved::Ours(published),
        None => Resolved::Nothing,
    }
}

/// Which rule an archive must pass to be installed from the UI.
///
/// One function for the three answers, because they are one decision made
/// once, at one call site in `install_one`:
///
/// - the **core** is exempt (task 12, ruling 49): its archive always carries
///   two systemd units, polkit rules and the privileged installer, so the
///   plugin rule would refuse every core update there will ever be. Root can
///   form the core binary's path; the units and the rules are listed for the
///   page and written by nobody here;
/// - one of **ours** is judged by `installable_from_ui`, which allows the
///   locale catalogs, input presets, examples and the `[[plugin]]` block that
///   the core itself writes;
/// - a **third-party** component gets `only_its_own_binary`, which allows none
///   of that. **No third-party component is ever exempt**, and the core's
///   exemption must never be generalised into one: it is a statement about one
///   archive built in this repository, not about archives that carry units.
fn archive_allowed(is_core: bool, third_party: bool, entries: &[String]) -> bool {
    if is_core {
        return true;
    }
    if third_party {
        return only_its_own_binary(entries);
    }
    installable_from_ui(entries)
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
    /// A **third-party** archive carrying anything besides its own binary: a
    /// unit, a polkit rule, a nested path, a locale catalog, an initial
    /// configuration, a `[[plugin]]` block, a second binary.
    ///
    /// Its own variant and not `NeedsManualStep`, because the two sentences
    /// say different things to different people: that one tells the operator
    /// to read our release notes, and this one names a rule a stranger's
    /// archive broke, which no release note of ours will explain.
    ThirdPartyArchive,
    /// The archive's binary is not the file this component is declared to run:
    /// it names a sibling, or the declaration points outside the plugins
    /// directory. Carries which two names disagreed.
    NotItsOwnFile(String),
    /// A third-party component whose own repository could not be consulted by
    /// this check — over the cap of four, unreachable, unaddressable, or
    /// publishing no archive for this architecture under this name.
    ///
    /// A refusal and **not** a silent skip, because the alternative that used
    /// to happen here was worse than either: falling through to our own
    /// release and installing the official archive of a colliding name.
    ThirdPartyUnchecked,
    /// A plugin the device does not declare, whose archive carries no
    /// `[[plugin]]` block. Installing it would place a binary nothing ever
    /// launches — the silent failure this repository's own documentation
    /// records making three times.
    NoFragment,
    /// The archive, or its checksum file, could not be fetched.
    Download(String),
    /// Everything between having the bytes and having asked systemd: reading
    /// the archive, writing the staged binary, the `/etc/ritornello` files,
    /// the request.
    Prepare(String),
    /// The privileged unit refused or could not be started. Carries
    /// systemctl's own words.
    Privileged(String),
    /// Nothing published carries this name at all: dropped out of the
    /// hundred-release window this check reads, or never one of ours.
    ///
    /// A refusal and not a silent skip, for the same reason `ThirdPartyUnchecked`
    /// is one (task 18's review, C1): the operator pressed a specific row's
    /// gesture, and "nothing happened" reads as the request never having
    /// reached the server, not as the honest "there is nothing to install"
    /// it actually means.
    NothingPublished,
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
            Self::ThirdPartyArchive => {
                write!(f, "a third-party archive may carry nothing but its own binary")
            }
            Self::NotItsOwnFile(d) => write!(f, "{d}"),
            Self::ThirdPartyUnchecked => {
                write!(f, "its own repository was not consulted by this check")
            }
            Self::NoFragment => write!(f, "the archive carries no plugins.toml block"),
            Self::Download(d) | Self::Prepare(d) | Self::Privileged(d) => write!(f, "{d}"),
            Self::NothingPublished => write!(f, "nothing published carries this name"),
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
        Refusal::ThirdPartyArchive => ("update_third_party_archive", None),
        Refusal::NotItsOwnFile(d) => ("update_wrong_file", Some(d)),
        Refusal::ThirdPartyUnchecked => ("update_third_party_unchecked", None),
        Refusal::NoFragment => ("update_no_fragment", None),
        Refusal::Download(d) => ("update_download_failed", Some(d)),
        Refusal::Prepare(d) => ("update_install_failed", Some(d)),
        Refusal::Privileged(d) => ("update_privileged_failed", Some(d)),
        Refusal::NothingPublished => ("update_nothing_published", None),
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

/// Where a plugin's own configuration lives, under the worker's `root`.
///
/// Not one of `archive::ETC_PREFIXES`: those two subdirectories belong to the
/// release and are overwritten on every update, whereas what lands directly in
/// this directory belongs to the operator and is written once.
const ETC_DIR: &str = "etc/ritornello";

/// The `[[plugin]]` block this install must write, if any.
///
/// Three answers, and the middle one is the whole point: a component the file
/// does not declare is an **installation**, and installing a plugin whose block
/// nobody adds means a plugin that ships and never starts, in silence — an
/// error this repository's own documentation records making three times.
/// Refusing here, rather than after the binary is on disk, is what keeps that
/// from becoming a half-installed device.
///
/// Pure, and separated from the I/O around it deliberately: the branch it
/// governs sits between a download and a systemd unit, and neither of those can
/// be produced by a test.
fn declaration_needed(
    is_core: bool,
    declared: bool,
    fragment: Option<&str>,
) -> Result<Option<String>, Refusal> {
    // The core declares nothing in `plugins.toml`, and a component already
    // declared keeps whatever the operator wrote about it: the archive's own
    // block is for a name the file does not carry yet.
    if is_core || declared {
        return Ok(None);
    }
    match fragment {
        Some(fragment) => Ok(Some(fragment.to_string())),
        None => Err(Refusal::NoFragment),
    }
}

/// Is the file root will be asked to place **this component's own**?
///
/// `installable_from_ui` and `only_its_own_binary` both count binaries and
/// neither reads the name; `install_one` then hands that bare name to the
/// privileged installer, which validates its *shape* and forms
/// `plugins_dir/<name>`. So an archive naming a **sibling** gets that sibling
/// overwritten with its own bytes.
///
/// No privilege is gained by that — the plugins directory is where a plugin
/// binary belongs either way, the root-run helpers live in its parent, and
/// `valid_name` still holds on the privileged side. What is gained is the
/// **choice of which** of the installed plugins gets replaced, and by whoever
/// built the archive rather than by the operator who ticked one row. That is
/// an integrity decision, so it is made here rather than left to the archive.
///
/// The parent is checked as well, and it is not belt and braces: root only
/// ever writes into the plugins directory, so a declaration whose `exec` lives
/// anywhere else gets a file placed where its own `exec` will never look — an
/// install that reports success and changes nothing, with a row that then
/// claims the new version.
///
/// Asked **only of a component the manifest already declares**: a fresh
/// install has no `exec` to compare against, and the name the archive carries
/// is the one its own fragment is about to declare.
fn placement_target(exec: &str, plugins_dir: &Path, file: &str) -> Result<(), Refusal> {
    let path = Path::new(exec);
    if path.parent() != Some(plugins_dir) {
        return Err(Refusal::NotItsOwnFile(format!(
            "it is declared to run {exec}, which is not in {}",
            plugins_dir.display()
        )));
    }
    if path.file_name().and_then(|n| n.to_str()) != Some(file) {
        return Err(Refusal::NotItsOwnFile(format!(
            "it is declared to run {exec}, and the archive carries {file}"
        )));
    }
    Ok(())
}

/// The operating name a shipped initial configuration takes on the device.
///
/// `stations.example.toml` becomes `stations.toml`: one file in the archive
/// serves as both the reference and the starting point (see
/// `deploy/packaging.toml`), and it is the operating name the plugin reads.
/// Anything else keeps the name it arrived with.
///
/// `None` for a name that is not a bare file name, and this is where that is
/// decided rather than where the bytes are written: `archive::read` has already
/// refused `..` and absolute paths, so what is left to refuse is a nested entry
/// — `/etc/ritornello/<dir>/<file>` is a shape nothing packs and the core has
/// no reason to create — and a dotted one, which would let an archive name the
/// very temporary `write_atomic` writes beside its target.
fn initial_config_target(entry: &str) -> Option<String> {
    if entry.is_empty() || entry.contains('/') || entry.starts_with('.') {
        return None;
    }
    Some(match entry.strip_suffix(".example.toml") {
        Some(stem) => format!("{stem}.toml"),
        None => entry.to_string(),
    })
}

/// Writes through a temporary beside the target, then `rename`.
///
/// The third copy of a three-line rule in this repository, and it is written
/// out rather than shared because neither of the other two fits: `plugins.rs`'s
/// takes a `&str` and derives its temporary name from a `.toml` extension it
/// assumes, and the privileged crate's is `pub(crate)` to a crate that must
/// not gain a dependant. A fourth copy would be the sign that this belongs in
/// a crate of its own — this one names the file rather than its extension, so
/// it works for a locale catalog and an input preset alike.
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
    placed: &[Placement],
    failure: Option<String>,
) -> Option<CheckOutcome> {
    if let Some(message) = failure {
        return Some(CheckOutcome::Failed(message));
    }
    let last = placed.last()?;
    // A plugin that was not there is not "updated to" anything: the sentence
    // for a first installation names no version, because there is no version
    // it moved from. The row beside it already carries the one it now has.
    let text = if last.fresh {
        catalog.get("update_installed_new").replace("{component}", &last.component)
    } else {
        catalog
            .get("update_installed")
            .replace("{component}", &last.component)
            .replace("{version}", &last.version)
    };
    Some(CheckOutcome::Installed(text))
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

// **A test's answer for `run_privileged_unit`, and the only way past it on a
// machine with no systemd.**
//
// A red light rather than a field on `Worker`, and the difference matters:
// this exists nowhere in a release build, no production struct gains a
// pluggable member, and nothing invites a future reader to think the
// privileged step can be swapped out. Each test runs on its own thread and
// `Privileged` puts the light out on the way through `Drop`, so no test can
// inherit another's answer.
//
// It is what makes the one property this whole memory exists for observable —
// that the note of what was placed is written **before** the process leaves.
// Everything downstream of the privileged call (the memory, the restart hook)
// is unreachable without it, and "unreachable" was the wrong word: what was
// missing was a red light, not an injectable design.
//
// A `//` comment and not a `///` one: a doc comment on a macro invocation is
// an `unused doc comment` error under `-D warnings`.
#[cfg(test)]
thread_local! {
    static FAKE_PRIVILEGED: std::cell::RefCell<Option<Result<(), String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Asks systemd for the privileged unit, and waits for it.
///
/// No `--no-block`: it is a `oneshot`, and the core is not what it stops, so
/// waiting is what lets the next step depend on it having finished. With a
/// deadline, because an I/O that hangs has already made a page disappear in
/// this product — here it would leave `busy` set for ever.
async fn run_privileged_unit() -> Result<(), String> {
    #[cfg(test)]
    if let Some(answer) = FAKE_PRIVILEGED.with(|f| f.borrow().clone()) {
        return answer;
    }
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
    // systemctl's own words, verbatim, all the way to the page. Same choice
    // as the files plugin makes for its mount, and for the same reason: a
    // sentence we paraphrase is a sentence that goes stale. It does **not**
    // name the missing polkit rule — systemctl has no idea which `.rules`
    // file would have granted the action, and says only `Access denied` or
    // `Interactive authentication required`; `docs/installation.md` is where
    // the reader is told to read those two as "the rule is not installed".
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
    /// A plugin the device did not have: its binary is placed, its initial
    /// configuration is on disk and its `[[plugin]]` block is written. Nothing
    /// runs it yet — the core loop has never heard of it.
    NewPlugin,
}

/// One component actually placed during a pass.
///
/// `fresh` is what tells "updated to 0.3.0" from "installed": a plugin that
/// was not there has no previous version to have been updated from, and
/// saying so would be a small lie on the one line the card shows.
struct Placement {
    component: String,
    version: String,
    fresh: bool,
}

/// What one check learnt, kept whole so an install that follows it asks GitHub
/// nothing a second time.
///
/// **Two lists and not one concatenated**, and that separation is the security
/// property rather than tidiness: a third-party plugin's name is chosen by its
/// own author and may collide with an official one, so a single list keyed by
/// name could hand a third-party row — or a third-party install — the official
/// archive of that name, silently swapping what the operator installed for
/// something else.
struct Checked {
    /// The fold of **our** release list.
    ours: Vec<Published>,
    /// What each third-party plugin's own repository publishes for it, at most
    /// `THIRD_PARTY_MAX` of them.
    theirs: Vec<ThirdPartyOffer>,
    /// Every component the announcements call a stranger's, uncapped — see
    /// `third_party_names`. Carried beside `theirs` because a name absent from
    /// `theirs` is not thereby one of ours: it may simply be the fifth
    /// third-party plugin, or one whose server did not answer.
    third_party: Vec<String>,
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
    /// The behaviour settings, for the one field this worker reads:
    /// `update_prereleases`. Read **per check** and never remembered, for the
    /// same reason `manifest` is — an owner who ticks the box expects the next
    /// check to obey it, not the next restart. The same handle
    /// `GET`/`PUT /api/settings` serves, so there is no second copy to keep in
    /// step.
    pub settings: Arc<RwLock<crate::state::Settings>>,
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

    /// The channel this check reads in, from the setting as it stands now.
    ///
    /// The guard is dropped before returning — the `bool` is copied out — so
    /// no caller can hold the settings lock across the network I/O that
    /// follows. That is not a stylistic preference here: no HTTP route in this
    /// product may block, and `PUT /api/settings` takes this same lock to
    /// write.
    async fn channel(&self) -> Channel {
        Channel::from_setting(self.settings.read().await.update_prereleases)
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
    /// **Three** sources, and neither of the first two alone is enough:
    /// `plugins.toml` says what is declared and where its binary should be,
    /// the status lines say what each plugin announced about itself, and a
    /// scan of the plugins directory (`plugins::undeclared_binaries`) is what
    /// makes `Availability::Undeclared` reachable at all — a binary sitting
    /// there with nothing declaring it appears in neither of the first two.
    /// The same scan feeds `PluginStatus::undeclared_binary` on `/api/status`
    /// (see `status::status_json`), so the two payloads read one fact rather
    /// than risking two (RULING 63).
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
        let mut out: Vec<Installed> = manifest
            .plugins
            .iter()
            .map(|p| {
                let line = statuses.plugins.iter().find(|l| l.name == p.name);
                Installed {
                    name: p.name.clone(),
                    declared: true,
                    binary_present: Path::new(&p.exec).exists(),
                    version: line.and_then(|l| l.version.clone()),
                    // Relayed from the announcement, verbatim and unparsed,
                    // for the same reason as `version`: the binary is the only
                    // thing that knows. `release::origin` is what reads it,
                    // once, wherever the answer is needed.
                    repository: line.and_then(|l| l.repository.clone()),
                }
            })
            .collect();
        let dir = plugins_dir(&self.root);
        for file in crate::plugins::undeclared_binaries(&dir, &manifest) {
            out.push(Installed {
                // The **component** name, not the bare file the scan
                // returns: `resolve`/`carries` below match a component name
                // against the release, and a row left named by its file
                // (`ritornello-plugin-mpd`) can never match the release's own
                // `mpd` — the exact defect a review of task 18 found, which
                // made "Declare" fail with nothing reaching the page at all.
                name: crate::plugins::component_name_from_file(&file).to_string(),
                declared: false,
                binary_present: true,
                version: None,
                // A binary nothing declares has never been launched by this
                // core, so it has announced nothing — there is no repository
                // to read, and inventing one would make a stranger's file
                // official.
                repository: None,
            });
        }
        out
    }

    /// What each third-party plugin's own repository publishes for it.
    ///
    /// At most `THIRD_PARTY_MAX` requests, decided by `third_party_targets`
    /// before a single socket is opened. Each answer goes through the **same**
    /// `parse_releases` and `fold` as our own: drafts and prereleases dropped
    /// by one rule, the newest archive for this architecture picked by one
    /// rule, and the version read off the asset name rather than off the tag —
    /// which is what makes an offered version the version of the archive that
    /// would actually be installed, rather than a number nothing can deliver.
    ///
    /// A repository that answers badly, or that publishes nothing for this
    /// architecture and this plugin name, simply yields no offer: its row
    /// stays `Unknown`, which says "nothing is known" and never "up to date".
    /// It is never a failure of the whole check — a stranger's server being
    /// down must not blank out the core's own row.
    async fn third_party_offers(
        &self,
        client: &reqwest::Client,
        targets: &[Target],
    ) -> Vec<ThirdPartyOffer> {
        // One read for the whole sweep: the setting cannot meaningfully change
        // between two strangers' repositories inside one check, and re-reading
        // per target would let it, which would make a check's answer depend on
        // the order the targets happen to be in.
        let channel = self.channel().await;
        let mut out = Vec::with_capacity(targets.len());
        for Target { name, repo, url } in targets {
            let (status, body) = match fetch_text(client, url).await {
                Ok(answer) => answer,
                Err(e) => {
                    tracing::warn!("update: {repo} (for the third-party plugin {name}): {e}");
                    continue;
                }
            };
            if status != 200 {
                tracing::warn!("update: {repo} answered HTTP {status} for {name}");
                continue;
            }
            let releases = match parse_releases(&body, channel) {
                Ok(releases) => releases,
                Err(e) => {
                    tracing::warn!("update: {repo} published nothing usable for {name}: {e:?}");
                    continue;
                }
            };
            // **The asset must be named for this plugin.** A repository that
            // publishes an archive under another name has published nothing
            // for the binary sitting on this device — and this is also what
            // stops a stranger's release from producing an `Offer::Core`,
            // which downstream would be a component the plugin rule never
            // judges.
            let Some(published) = fold(&releases, ARCH)
                .into_iter()
                .find(|p| matches!(&p.offer, Offer::Plugin(n) if n == name))
            else {
                tracing::info!("update: {repo} publishes no {ARCH} archive named for {name}");
                continue;
            };
            out.push(ThirdPartyOffer { name: name.clone(), published });
        }
        out
    }

    /// The check. Two small requests — the release list and nothing else — and
    /// no archive: knowing in advance whether every component is installable
    /// would mean fetching eleven archives a day for a fact that changes once
    /// per release.
    ///
    /// Returns the fold so a scheduled run can install from it without asking
    /// GitHub the same question twice.
    async fn check(&self, client: &reqwest::Client) -> Option<Checked> {
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
        let releases = match parse_releases(&body, self.channel().await) {
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
                // Our repository publishing nothing says nothing about a
                // stranger's, so the third-party rows are still answered.
                let theirs = self.third_party_offers(client, &third_party_targets(&installed)).await;
                let mut components =
                    component_offers(self.core_version, &[], &theirs, &installed);
                let mut state = self.state.write().await;
                carry_core_notes(&state.components, &mut components);
                state.outcome = CheckOutcome::NoRelease;
                state.release_version = None;
                state.release_url = None;
                state.last_check_unix_s = Some(now_unix_s());
                state.components = components;
                // `Some` with an empty `ours`, and not `None`: this branch has
                // just offered third-party updates on the page, and returning
                // `None` would make Install do nothing and say nothing about
                // them. Our own components resolve to `Nothing` from an empty
                // list, which is the truth here.
                return Some(Checked {
                    ours: Vec::new(),
                    theirs,
                    third_party: third_party_names(&installed),
                });
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
        let theirs = self.third_party_offers(client, &third_party_targets(&installed)).await;
        let mut components =
            component_offers(self.core_version, &published, &theirs, &installed);
        let core = published.iter().find(|p| p.offer == Offer::Core);
        let mut state = self.state.write().await;
        carry_installable(&state.components, &mut components);
        carry_core_notes(&state.components, &mut components);
        state.outcome = CheckOutcome::Ok;
        state.release_version = core.map(|p| p.version.clone());
        state.release_url = core.map(|p| release_page(&p.release_tag));
        state.last_check_unix_s = Some(now_unix_s());
        state.components = components;
        Some(Checked { ours: published, theirs, third_party: third_party_names(&installed) })
    }

    /// Installs the named components, plugins first and the core last.
    ///
    /// A component that fails names its cause and the next one is still
    /// attempted: one refusal should say one thing, not cancel a gesture the
    /// operator asked for on five rows. Only the **first** cause reaches the
    /// page, which is the honest limit of a payload with one message field.
    async fn install(&self, client: &reqwest::Client, checked: &Checked, names: &[String]) {
        let mut first_failure: Option<String> = None;
        // `(component, version)` per plugin actually placed. The core is never
        // in here: it exits at the end of its own install and this function
        // has already returned.
        let mut placed: Vec<Placement> = Vec::new();
        for name in install_order(names) {
            // What the component **is** decides which list answers for it —
            // never which lookup happened to return something. See `resolve`.
            let (offered, third_party) = match resolve(checked, &name) {
                Resolved::Theirs(published) => (published, true),
                Resolved::Ours(published) => (published, false),
                Resolved::UncheckedThirdParty => {
                    // A named refusal and not a silent skip: the operator
                    // ticked this row, and "nothing happened" would read as a
                    // failure of the gesture rather than as what it is.
                    tracing::warn!(
                        "update: {name} is a third-party plugin whose repository this check did not consult, skipping it"
                    );
                    let catalog = self.catalog.read().await;
                    let message = refusal_message(&catalog, &name, &Refusal::ThirdPartyUnchecked);
                    drop(catalog);
                    first_failure.get_or_insert(message);
                    continue;
                }
                Resolved::Nothing => {
                    // A named refusal, not a silent skip (task 18's review,
                    // C1): a `missing_binary` or `undeclared_binary` row's
                    // badge does **not** read `Unknown` (it reads "Not
                    // installed" or "Installed but not declared" — see
                    // `ConfigView.vue`), so silence here was never the
                    // harmless case its old comment assumed. It is also the
                    // one shape that made "Declare" fail with no toast at
                    // all when the row's name did not match the release.
                    tracing::warn!("update: nothing published for {name}, skipping it");
                    let catalog = self.catalog.read().await;
                    let message = refusal_message(&catalog, &name, &Refusal::NothingPublished);
                    drop(catalog);
                    first_failure.get_or_insert(message);
                    continue;
                }
            };
            self.set_busy(Some(self.message_for("update_installing", &name).await))
                .await;
            match self.install_one(client, &name, offered, third_party).await {
                Ok(Placed::Plugin) => {
                    self.restart_plugin(&name).await;
                    placed.push(Placement {
                        component: name.clone(),
                        version: offered.version.clone(),
                        fresh: false,
                    });
                }
                Ok(Placed::NewPlugin) => {
                    self.start_declared_plugin(&name).await;
                    placed.push(Placement {
                        component: name.clone(),
                        version: offered.version.clone(),
                        fresh: true,
                    });
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
        self.conclude_install(checked, &placed, first_failure).await;
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
        checked: &Checked,
        placed: &[Placement],
        failure: Option<String>,
    ) {
        if !placed.is_empty() {
            let installed = self.installed_when_settled().await;
            let mut components =
                component_offers(self.core_version, &checked.ours, &checked.theirs, &installed);
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
        third_party: bool,
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
        // **The core is not judged by the plugin rule, and that is not an
        // oversight.** `installable_from_ui` asks whether an archive holds
        // anything root would have to place outside the plugins directory —
        // the core's own archive always does, since it carries the core binary
        // itself, two systemd units and a polkit rule. Root can form the core
        // binary's path, and the units and the rule are read by nobody here:
        // they are listed so the page can say the release changes them, and
        // never written. A release that changes them says "Action required" in
        // its notes, which is the mechanism the design gives that case.
        //
        // **A third-party archive is judged more strictly, and is never
        // exempt**: only its own binary, nothing for `/etc/ritornello` and no
        // `[[plugin]]` block. See `archive_allowed`, which holds all three
        // answers, and `only_its_own_binary` for what this refusal stops.
        if !archive_allowed(is_core, third_party, &contents.entries) {
            self.remember_manual_step(name).await;
            return Err(if third_party {
                Refusal::ThirdPartyArchive
            } else {
                Refusal::NeedsManualStep
            });
        }
        // The rule above counts binaries; this one **names** the one that will
        // be placed. Both are refusals, both happen before a single byte is
        // written, and they are separate because the second is a fact about
        // this component's declaration rather than about the archive alone —
        // see `placement_target` for what an archive naming a sibling buys.
        //
        // The same rule for ours and for a stranger's: `installable_from_ui`
        // does not read the name either, so an official archive built wrong
        // would place the wrong file just as quietly. No exemption here, not
        // even the core's — the core has no `exec` in `plugins.toml`, so the
        // question is not asked of it at all rather than waived for it.
        if !is_core
            && let Some((file, _)) = &contents.binary
            && let Some(exec) = self.exec_of(name)
        {
            placement_target(&exec, &plugins_dir(&self.root), file)?;
        }
        // A component nothing declares is an **installation**, not a
        // replacement: it needs a declaration, an initial configuration, and a
        // launch of a plugin the core has never heard of. Read from
        // `plugins.toml` rather than from the row the page showed: the file is
        // the authority, and the row was computed at the last check.
        //
        // Decided here, before a single byte is written: see
        // `declaration_needed` for what an archive with no block costs.
        // `!is_core &&` so that `declared` means what it says: the core's name
        // is not in `plugins.toml` and this must not read the file looking for
        // it. `declaration_needed` makes the core's own decision from `is_core`
        // alone.
        let declared = !is_core && self.declared(name);
        let fragment = declaration_needed(is_core, declared, contents.fragment.as_deref())?;
        // One decision, read once: a block to write is what makes this an
        // installation rather than a replacement.
        let fresh = fragment.is_some();
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
        // Only for a plugin that was not there. On an update, the operator's
        // station list is already in place and `write_initial_config` would
        // leave it alone anyway — but not asking the question at all is what
        // makes that guarantee independent of a `exists()` call.
        if fresh {
            self.write_initial_config(&contents.initial_config)?;
        }

        let request = Request { format: REQUEST_FORMAT, actions: vec![action] };
        let request_path = self.staging.join("request.json");
        let text = serde_json::to_string(&request)
            .map_err(|e| Refusal::Prepare(format!("the request: {e}")))?;
        std::fs::write(&request_path, text)
            .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", request_path.display())))?;

        if let Err(detail) = run_privileged_unit().await {
            // systemctl's own words travel verbatim to the page. They do not
            // name the missing polkit rule — see `run_privileged_unit` — but
            // they are the only account of the failure that exists on this
            // side of the boundary.
            return Err(Refusal::Privileged(detail));
        }
        // Written only once the placement actually succeeded: a refusal above
        // must not claim that anything was placed. And written **here**,
        // before this function returns — the core's caller leaves the process
        // the moment it sees `Placed::Core`, so this is the last instant at
        // which anything can be remembered about a core update. See
        // `remember_placed`, and `automatic_install_list` for what reads it.
        if !third_party {
            self.remember_placed(name, &offered.version, core_notes);
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
        // The order matters, and it is not the intuitive one: **the binary is
        // placed before the declaration is written.**
        //
        // A declaration pointing at a file that is not there is exactly the
        // state this chantier repairs elsewhere (`Availability::BinaryMissing`),
        // and creating it on a failure path would be careless. The reverse
        // leftover — a binary nobody declares — is harmless, visible on the
        // page as `Undeclared`, and undone by one gesture.
        if let Some(fragment) = &fragment {
            self.write_declaration(name, fragment)?;
            return Ok(Placed::NewPlugin);
        }
        Ok(if is_core { Placed::Core } else { Placed::Plugin })
    }

    fn write_staged(&self, staged: &str, bytes: &[u8]) -> Result<(), Refusal> {
        let path = self.staging.join(staged);
        std::fs::write(&path, bytes)
            .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", path.display())))
    }

    /// Writes down what this pass just placed, and — for the core — what its
    /// archive carried that nothing here installs.
    ///
    /// **Called from `install_one`, after the privileged unit has succeeded
    /// and before it returns**, which is what puts it before the restart: the
    /// core's placement ends with `install` calling `self.restart`, and that
    /// hook does not return. Anything written after it would be written never.
    /// Keeping the write inside `install_one` rather than at the three `Ok`
    /// arms of `install` is what makes that ordering a fact about the shape of
    /// the code instead of a rule three call sites have to remember.
    ///
    /// **Nothing is remembered for a third-party component**, and that is not
    /// an oversight: the automatic policy never installs one (see
    /// `automatic_install_list`), so there is nothing for the memory to bound
    /// — and a third-party plugin's name is chosen by its own author and may
    /// collide with one of ours, which is the very reason `Checked` keeps two
    /// lists. Not writing the entry is how that collision is made impossible
    /// here rather than reasoned about.
    ///
    /// **A manual placement is written down too**, and the brief only asked
    /// for the automatic policy's own. It is the better reading: what the
    /// memory records is that *this device* was given that archive and did not
    /// keep it, and that fact is no less true when a person clicked the
    /// button. So the robot learns from a failed hand install as well and does
    /// not repeat it at three in the morning, while the person is never
    /// stopped from trying again — `automatic_install_list` is the only reader
    /// there is. Said out loud in `docs/interface.md` too, since it is the
    /// difference between "the robot gave up" and "nobody may install this".
    ///
    /// Best-effort and never a `Refusal`: by the time this runs the bytes are
    /// already at their target, and refusing an install that has happened
    /// would be a lie. A memory that fails to be written costs one more
    /// attempt at the next run, which is where this started.
    ///
    /// **One production caller, on purpose** (`install_one`). The tests reach
    /// the memory through `placed::record` instead, which leaves this method
    /// dead the moment that call is deleted and makes
    /// `cargo clippy --all-targets -- -D warnings` say so. That tripwire is
    /// belt to the braces of
    /// `the_memory_of_a_core_install_is_on_disk_by_the_time_the_process_leaves`,
    /// which observes the real thing — and it **disarms silently** if a future
    /// test ever calls this method directly, so route new tests through
    /// `placed::record`.
    fn remember_placed(&self, name: &str, version: &str, not_installed_files: Option<Vec<String>>) {
        if let Err(e) = placed::record(&self.staging, name, version, not_installed_files) {
            tracing::warn!(
                "update: writing {}: {e}",
                placed::path(&self.staging).display()
            );
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

    /// The plugin's own configuration, written **only where there is none**.
    ///
    /// Unlike `write_etc_files`, which replaces unconditionally: what lands
    /// here is a station list or a set of key bindings, and those two files
    /// hold what the operator produced. The archive ships them so a first
    /// installation is not an empty screen, never so a release can put its own
    /// back.
    ///
    /// A failure is a `Refusal` and not a warning: an operator who installs
    /// the radio and gets no stations has an install that did not do what it
    /// said, and saying so beats leaving them to notice.
    fn write_initial_config(&self, files: &[(String, Vec<u8>)]) -> Result<(), Refusal> {
        let dir = self.root.join(ETC_DIR);
        for (entry, bytes) in files {
            let Some(name) = initial_config_target(entry) else {
                // Listed on the page as something the archive carries, and
                // written nowhere — the rule this whole module follows.
                tracing::warn!(
                    "update: not writing the initial configuration entry {entry:?}: it is not a bare file name"
                );
                continue;
            };
            let target = dir.join(&name);
            if target.exists() {
                tracing::info!("update: {} already exists, keeping it", target.display());
                continue;
            }
            std::fs::create_dir_all(&dir)
                .map_err(|e| Refusal::Prepare(format!("creating {}: {e}", dir.display())))?;
            write_atomic(&target, bytes)
                .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", target.display())))?;
            tracing::info!("update: {} written from the archive", target.display());
        }
        Ok(())
    }

    /// Appends the archive's `[[plugin]]` block to `plugins.toml`.
    ///
    /// The fragment travels **as the archive wrote it**: nothing here builds
    /// one, prepends a description to one, or copies the plugin catalogue's
    /// text into the file. `plugins/edit.rs` is safe against a headerless
    /// ambiguity as long as no caller synthesises a comment of its own, and
    /// the release fragments carry none (see that module's doc). An installed
    /// entry therefore arrives without a description, which is what happens on
    /// a hand-deployed device too and what the operator can add.
    fn write_declaration(&self, name: &str, fragment: &str) -> Result<(), Refusal> {
        let text = std::fs::read_to_string(&self.manifest)
            .map_err(|e| Refusal::Prepare(format!("reading {}: {e}", self.manifest.display())))?;
        let updated = crate::plugins::edit::append_block(&text, fragment, name)
            .map_err(|e| Refusal::Prepare(format!("declaring {name}: {e}")))?;
        // Through a temporary and a rename, like every other write to this
        // file: a `plugins.toml` cut in half by a power cut is a device that
        // launches nothing at all on the next boot.
        write_atomic(&self.manifest, updated.as_bytes())
            .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", self.manifest.display())))
    }

    /// Is this plugin declared in `plugins.toml`?
    ///
    /// `false` for a file that cannot be read, for the same reason `enabled`
    /// answers `false` there: the core has no basis for saying otherwise. The
    /// install that follows then tries to append to that same file and refuses
    /// with the read error, which is louder than a silent replacement of a
    /// binary nothing declares.
    fn declared(&self, name: &str) -> bool {
        match PluginManifest::load(&self.manifest) {
            Ok(manifest) => manifest.plugins.iter().any(|p| p.name == name),
            Err(e) => {
                tracing::warn!("update: reading {}: {e:#}", self.manifest.display());
                false
            }
        }
    }

    /// The `exec` `plugins.toml` declares for this plugin, if it declares one.
    ///
    /// Read from the file and never from the row the page showed, for the same
    /// reason `declared` is: the file is the authority, and the row was
    /// computed at the last check. `None` for a name the file does not carry
    /// and for a file that cannot be read — in both cases there is no
    /// declaration to hold an archive to, and `install_one` then falls back on
    /// the refusals that already cover a fresh install.
    fn exec_of(&self, name: &str) -> Option<String> {
        match PluginManifest::load(&self.manifest) {
            Ok(manifest) => {
                manifest.plugins.iter().find(|p| p.name == name).map(|p| p.exec.clone())
            }
            Err(e) => {
                tracing::warn!("update: reading {}: {e:#}", self.manifest.display());
                None
            }
        }
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

    /// Its binary is placed and its block is written: the core loop must take
    /// the declaration into account and launch it.
    ///
    /// **No `enabled` check here, unlike `restart_plugin`, and that is not an
    /// oversight.** The loop has to read `plugins.toml` anyway — it is where
    /// the name, the `exec` and the new order come from — so it reads the
    /// switch at the same time and does not launch a plugin the block declares
    /// off. Filtering here instead would send nothing at all, and the core
    /// would keep no `exec` for that name: switching the plugin on from the
    /// page afterwards would then fail until the next restart of the core.
    async fn start_declared_plugin(&self, name: &str) {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let order =
            PluginOrder { name: name.to_string(), action: PluginAction::Declare, ack: ack_tx };
        if self.plugins_tx.send(order).await.is_err() {
            tracing::warn!(
                "update: the core loop is gone, {name} is declared and will start with the core"
            );
            return;
        }
        match tokio::time::timeout(RESTART_ACK_TIMEOUT, ack_rx).await {
            Ok(Ok(true)) => tracing::info!("update: {name} installed and declared"),
            // The declaration is on disk either way: what failed is the
            // launch, and the next boot reads the same file.
            Ok(Ok(false)) => tracing::warn!(
                "update: {name} is installed and declared, but the core could not start it — its own log names the cause"
            ),
            Ok(Err(_)) => tracing::warn!("update: no acknowledgment for the declaration of {name}"),
            Err(_) => tracing::warn!(
                "update: the core loop did not acknowledge the declaration of {name} within {} s",
                RESTART_ACK_TIMEOUT.as_secs()
            ),
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

    /// The slow half of an uninstall (see `Job::RemovePlugin`'s doc): the
    /// core already stopped `name` and its `plugins.toml` block is already
    /// gone, so all that is left is asking the privileged unit to erase
    /// `file` from the plugins directory.
    ///
    /// Does not touch `busy`: that field is the update card's, and an
    /// uninstall is a plugin-management gesture, not an update — the two
    /// only share this queue because both ultimately reach the same
    /// privileged unit and the same staging directory, which must not be
    /// touched by two of them at once.
    ///
    /// **Re-reads the manifest before erasing anything** (task 18's review,
    /// I1): `Job::RemovePlugin` is serialised behind any install already in
    /// flight, and an install places its binary *before* writing its
    /// declaration — so a file undeclared when the route accepted the
    /// request can be declared by the time this runs. What was true at
    /// enqueue time is not trusted; what is true right now, immediately
    /// before the file is actually erased, is.
    async fn remove_plugin_binary(&self, name: &str, file: &str) {
        match PluginManifest::load(&self.manifest) {
            Ok(m) if m.plugins.iter().any(|p| {
                Path::new(&p.exec).file_name().and_then(|f| f.to_str()) == Some(file)
            }) => {
                tracing::warn!(
                    "update: {file} is now declared by a plugin; the queued removal for {name} was skipped"
                );
                self.publish_failure(self.message_for("update_removal_skipped", name).await).await;
                return;
            }
            Ok(_) => {}
            // A transient read failure is not a reason to refuse an erasure
            // that was already accepted: proceed as `installed()` does on the
            // same error, rather than block an uninstall on it.
            Err(e) => {
                tracing::warn!("update: reading {} before removing {file}: {e:#}", self.manifest.display());
            }
        }
        let request =
            Request { format: REQUEST_FORMAT, actions: vec![Action::RemovePlugin { file: file.to_string() }] };
        if let Err(e) = std::fs::create_dir_all(&self.staging) {
            self.removal_failed(name, format!("creating {}: {e}", self.staging.display())).await;
            return;
        }
        let request_path = self.staging.join("request.json");
        let text = match serde_json::to_string(&request) {
            Ok(t) => t,
            Err(e) => {
                self.removal_failed(name, format!("encoding the request: {e}")).await;
                return;
            }
        };
        if let Err(e) = std::fs::write(&request_path, text) {
            self.removal_failed(name, format!("writing {}: {e}", request_path.display())).await;
            return;
        }
        match run_privileged_unit().await {
            Ok(()) => tracing::info!("update: {name}'s binary removed"),
            Err(detail) => self.removal_failed(name, detail).await,
        }
    }

    /// **The erasure did not happen, and the page has to hear about it.**
    ///
    /// Uninstall and "Remove the binary" answer 204 the moment the job is
    /// queued, which is honest in itself — the queue is real and the
    /// privileged unit can take two minutes. What was not honest is that the
    /// *outcome* of that job reached nothing but the journal. Without the
    /// polkit rule — a state `docs/installation.md` explicitly supports — both
    /// gestures answered "OK" for ever and erased nothing: the row simply came
    /// back as "Installed but not declared", with no account of why.
    ///
    /// `CheckOutcome::Failed` is the one free-text channel this payload has,
    /// and the update card shows it — the same channel every install refusal
    /// already uses. Deliberately not `busy`: that field belongs to the update
    /// card's own gesture, and an uninstall is not one.
    async fn removal_failed(&self, name: &str, detail: String) {
        tracing::warn!("update: erasing {name}'s binary: {detail}");
        let message = self
            .message_for("update_removal_failed", name)
            .await
            .replace("{detail}", &detail);
        self.publish_failure(message).await;
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
                if let Some(checked) = worker.check(&client).await {
                    worker.install(&client, &checked, &names).await;
                }
            }
            Job::Scheduled { install } => {
                if let Some(checked) = worker.check(&client).await
                    && install
                {
                    // The memory is read from disk at the moment of the
                    // decision rather than held in the `Worker`: it is
                    // written by the process that then leaves, so the run
                    // that has to honour it is very often a *later* process.
                    // One reader, one writer, one file, and no copy to keep
                    // in step with it.
                    let names = automatic_install_list(
                        &worker.state.read().await.components,
                        &placed::read(&worker.staging),
                    );
                    if names.is_empty() {
                        tracing::debug!("update: scheduled run, nothing to install");
                    } else {
                        tracing::info!("update: scheduled run installing {names:?}");
                        worker.install(&client, &checked, &names).await;
                    }
                }
            }
            Job::RemovePlugin { name, file } => {
                worker.remove_plugin_binary(&name, &file).await;
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

    /// A rollback report dated `at`, as the unit leaves one behind.
    fn report(at: u64) -> ritornello_updater::rollback::Report {
        ritornello_updater::rollback::Report {
            at_unix_s: at,
            restored: vec!["core".into()],
            failed: vec![],
            core_restored: true,
        }
    }

    /// The requirement the owner raised, and it is the one that would have
    /// been missed: the startup power setting defaults to **on**, so a restart
    /// at 3 a.m. would wake the active source and start playing music nobody
    /// asked for.
    #[test]
    fn a_restart_right_after_an_install_preserves_whatever_the_player_was_doing() {
        assert_eq!(startup_override(Some(marker(1_000)), None, 1_002), StartupOverride::Previous);
    }

    #[test]
    fn a_restart_long_after_an_install_obeys_the_setting_again() {
        let stale = 1_000 + ritornello_updater::marker::MARKER_WINDOW_S + 1;
        assert_eq!(
            startup_override(Some(marker(1_000)), None, stale),
            StartupOverride::AsConfigured
        );
    }

    #[test]
    fn an_ordinary_boot_obeys_the_setting() {
        assert_eq!(startup_override(None, None, 5_000), StartupOverride::AsConfigured);
    }

    /// **One operand each**, because the rule is a disjunction and a test that
    /// fed both would pass with either half deleted. The rollback's own dated
    /// file has to carry the instruction on its own: by the time the restored
    /// core boots, the marker is gone.
    #[test]
    fn a_restart_right_after_a_rollback_preserves_it_too_with_no_marker_left() {
        assert_eq!(startup_override(None, Some(&report(1_000)), 1_002), StartupOverride::Previous);
        let stale = 1_000 + ritornello_updater::marker::MARKER_WINDOW_S + 1;
        assert_eq!(
            startup_override(None, Some(&report(1_000)), stale),
            StartupOverride::AsConfigured,
            "a rollback last March must not override the Startup setting in September"
        );
    }

    /// **The three-in-the-morning requirement on the path that actually broke
    /// it, driven through a real `rollback()`.**
    ///
    /// This replaces two tests that were named for the reverted core and never
    /// ran a rollback: one handed the same `Marker` value in twice, the other
    /// read the same file twice with nothing in between. Both passed
    /// identically whether or not the rollback deleted the marker — and it
    /// does delete it, deliberately, so that a second `OnFailure=` cannot roll
    /// back twice. The device woke up, and the suite was green.
    ///
    /// Here the marker is really consumed by `rollback::rollback`, and the
    /// restored core still has to answer `Previous`.
    #[test]
    fn the_core_a_real_rollback_put_back_still_knows_not_to_wake_the_device() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();
        let staging = prefix.join("staging");
        std::fs::create_dir_all(prefix.join("usr/local/bin")).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(prefix.join("usr/local/bin/ritornello-core"), b"the core that worked")
            .unwrap();
        std::fs::write(staging.join("staged-core"), b"the core that does not start").unwrap();

        // 03:00 — the privileged unit places the new core and arms the net.
        let applied = ritornello_updater::apply::apply(
            prefix,
            &staging,
            &Request {
                format: REQUEST_FORMAT,
                actions: vec![Action::PlaceCore { staged: "staged-core".to_string() }],
            },
        )
        .unwrap();
        ritornello_updater::marker::arm(prefix, &applied, 1_000).unwrap();
        assert_eq!(
            startup_instruction(prefix, 1_002),
            StartupOverride::Previous,
            "the core that was just installed keeps the device as it was"
        );

        // 03:00:10 — it never starts, systemd exhausts the start limit, and
        // the rollback unit puts the previous binary back. It consumes the
        // marker on the way, which is what left the restored core with nothing
        // to read.
        let done = ritornello_updater::rollback::rollback(prefix, 1_030)
            .unwrap()
            .expect("a fresh marker authorises the rollback");
        assert!(done.core_restored, "the fixture must really have rolled the core back");
        assert!(
            ritornello_updater::marker::read(prefix).is_none(),
            "the authorisation is consumed exactly once — that part was always right"
        );

        // 03:00:11 — the restored core boots. Startup is `on` by default, and
        // the device was asleep.
        assert_eq!(
            startup_instruction(prefix, 1_031),
            StartupOverride::Previous,
            "the core the rollback put back must not wake a device that was in standby"
        );
    }

    /// No marker and no report: the ordinary boot, and the setting decides.
    #[test]
    fn a_directory_with_neither_file_leaves_the_setting_alone() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(startup_instruction(dir.path(), 1_002), StartupOverride::AsConfigured);
    }

    /// **A plugin gesture arms no rollback**, driven through a real `apply`
    /// and a real `rollback` from the core's own side of the boundary.
    ///
    /// A plugin runs in a process of its own and cannot crash-loop the core.
    /// Before this, the privileged binary armed the net for every apply, so
    /// any unrelated core start-limit failure inside the ten-minute window
    /// deleted a just-installed plugin binary — undoing a gesture nobody
    /// asked to undo, and leaving the real cause alone.
    #[test]
    fn a_plugin_gesture_arms_no_rollback_and_an_unrelated_crash_undoes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();
        let staging = prefix.join("staging");
        let installed = plugins_dir(prefix).join("ritornello-plugin-mpd");
        std::fs::create_dir_all(plugins_dir(prefix)).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("staged-plugin-mpd"), b"the new mpd").unwrap();

        let applied = ritornello_updater::apply::apply(
            prefix,
            &staging,
            &Request {
                format: REQUEST_FORMAT,
                actions: vec![Action::PlacePlugin {
                    file: "ritornello-plugin-mpd".to_string(),
                    staged: "staged-plugin-mpd".to_string(),
                }],
            },
        )
        .unwrap();
        assert!(!applied.core_replaced, "the fixture must be a plugin-only apply");
        assert!(installed.exists(), "and it must really have placed the binary");

        ritornello_updater::marker::arm(prefix, &applied, 1_000).unwrap();
        assert!(
            ritornello_updater::marker::read(prefix).is_none(),
            "a plugin gesture must leave no authorisation to roll the device back"
        );
        assert!(
            ritornello_updater::rollback::rollback(prefix, 1_030).unwrap().is_none(),
            "so an unrelated crash loop finds nothing to undo"
        );
        assert!(installed.exists(), "the plugin the operator installed is still there");
        assert_eq!(
            startup_instruction(prefix, 1_031),
            StartupOverride::AsConfigured,
            "and a boot after a plugin gesture is an ordinary boot, which reads the setting"
        );
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

    /// A declared, running plugin that announced `repository`.
    fn announcing(name: &str, repository: Option<&str>) -> Installed {
        Installed {
            name: name.to_string(),
            declared: true,
            binary_present: true,
            version: Some("1.0.0".to_string()),
            repository: repository.map(str::to_string),
        }
    }

    /// **Refusal 1 of 3: at most four third-party repositories per check.**
    ///
    /// Without the ceiling, ten third-party plugins make ten requests a day to
    /// ten different servers, and a single slow host blocks the whole check
    /// behind its own timeout — which leaves `busy` set and every button on
    /// the page disabled.
    ///
    /// **Six** third-party plugins and not four, with an official one, an
    /// unaddressable one and a silent one interleaved: a fixture of exactly
    /// four could not tell a ceiling from its absence, and a fixture made only
    /// of third-party rows could not tell "the first four third-party
    /// repositories" from "the first four rows".
    #[test]
    fn a_check_queries_at_most_four_third_party_repositories() {
        let installed = vec![
            // Ours — the majority path. It must not consume one of the four.
            announcing("radio", Some("https://github.com/skerdudou/ritornello")),
            announcing("alpha", Some("https://github.com/a/alpha")),
            // Present and unaddressable: there is no GitHub endpoint to call
            // for it, so it must not consume one of the four either.
            announcing("elsewhere", Some("https://gitlab.com/x/y")),
            announcing("bravo", Some("https://github.com/b/bravo")),
            // Announced nothing at all: switched off, dead, or predating the
            // field.
            announcing("silent", None),
            announcing("charlie", Some("https://github.com/c/charlie")),
            announcing("delta", Some("https://github.com/d/delta")),
            announcing("echo", Some("https://github.com/e/echo")),
            announcing("foxtrot", Some("https://github.com/f/foxtrot")),
        ];
        let targets = third_party_targets(&installed);
        let asked: Vec<(&str, &str)> =
            targets.iter().map(|t| (t.name.as_str(), t.repo.as_str())).collect();
        assert_eq!(
            asked,
            vec![
                ("alpha", "a/alpha"),
                ("bravo", "b/bravo"),
                ("charlie", "c/charlie"),
                ("delta", "d/delta"),
            ],
            "at most four repositories are queried, and they are the first four in file order"
        );
        assert_eq!(targets.len(), THIRD_PARTY_MAX);
        // The address is formed here and nowhere downstream, so this is where
        // the host a request goes to is decided.
        assert_eq!(
            targets[0].url,
            "https://api.github.com/repos/a/alpha/releases?per_page=100"
        );

        // **The cap bounds who is asked, never who counts as a stranger.**
        // The same fixture answered by `third_party_names`: all six, plus the
        // unaddressable one, and neither of the two that are ours or silent.
        // Conflating the two lists is what let a fifth third-party plugin — or
        // one on GitLab — fall through to our own release under a colliding
        // name.
        assert_eq!(
            third_party_names(&installed),
            names(&["alpha", "elsewhere", "bravo", "charlie", "delta", "echo", "foxtrot"]),
            "being a stranger admits no ceiling, and an unaddressable repository is still a stranger's"
        );
    }

    /// **Refusal 2 of 3: a third-party archive may carry nothing but its own
    /// binary.**
    ///
    /// The privileged side cannot write outside the plugins directory anyway,
    /// but the **core** can write `/etc/ritornello` and can append to
    /// `plugins.toml` — so without this rule a stranger's archive would be
    /// handed the operator's configuration directory, and a `[[plugin]]` block
    /// naming any `exec` path it liked, by the one component allowed to do
    /// both.
    ///
    /// The three shapes `installable_from_ui` already refuses are here so the
    /// third-party path is shown to refuse them too — but the two that matter
    /// most are the **catalog** and the **fragment**: our own rule accepts
    /// both, so they are the only fixtures that can tell "the third-party path
    /// calls the stricter rule" from "the third-party path calls the plugin
    /// rule". A fixture both rules answer the same way proves neither.
    #[test]
    fn a_third_party_archive_may_carry_nothing_but_its_own_binary() {
        let dirs = [
            "usr/",
            "usr/local/",
            "usr/local/lib/",
            "usr/local/lib/ritornello/",
            "usr/local/lib/ritornello/plugins/",
        ];
        let with = |extra: &[&str]| -> Vec<String> {
            let mut all: Vec<&str> = dirs.to_vec();
            all.push("usr/local/lib/ritornello/plugins/ritornello-plugin-theirs");
            all.extend_from_slice(extra);
            names(&all)
        };

        // The only shape that passes: one binary, directly under the plugins
        // prefix, and nothing else.
        assert!(
            archive_allowed(false, true, &with(&[])),
            "a third-party archive carrying only its binary must install"
        );

        assert!(
            !archive_allowed(false, true, &with(&["etc/systemd/system/theirs.service"])),
            "a third-party archive carrying a systemd unit must be refused"
        );
        assert!(
            !archive_allowed(
                false,
                true,
                &with(&["etc/polkit-1/rules.d/60-theirs.rules"])
            ),
            "a third-party archive carrying a polkit rule must be refused"
        );
        assert!(
            !archive_allowed(
                false,
                true,
                &with(&["usr/local/lib/ritornello/plugins/sub/evil"])
            ),
            "a third-party archive carrying a nested path under the plugins prefix must be refused"
        );
        // The same shape with **no** bare binary beside it, and it is the one
        // that actually proves the nested-path rule: with the binary present,
        // the assertion above is answered by the "exactly one binary" clause
        // instead, so deleting the `/` check leaves it green. Measured, not
        // reasoned about.
        assert!(
            !archive_allowed(
                false,
                true,
                &names(&[
                    "usr/",
                    "usr/local/",
                    "usr/local/lib/",
                    "usr/local/lib/ritornello/",
                    "usr/local/lib/ritornello/plugins/",
                    "usr/local/lib/ritornello/plugins/sub/",
                    "usr/local/lib/ritornello/plugins/sub/evil",
                ])
            ),
            "a nested path is not a bare name the privileged side could ever form"
        );
        assert!(
            !archive_allowed(
                false,
                true,
                &with(&["usr/local/lib/ritornello/plugins/ritornello-plugin-second"])
            ),
            "two binaries would make the core pick one silently"
        );

        // The two that separate this rule from the plugin rule. Both are
        // asserted against `installable_from_ui` first, so the fixture is
        // proven to be one our own rule accepts.
        let catalog = with(&["etc/ritornello/locales/theirs/fr.toml"]);
        assert!(
            installable_from_ui(&catalog),
            "the plugin rule accepts a locale catalog — that is what makes this fixture the discriminating one"
        );
        assert!(
            !archive_allowed(false, true, &catalog),
            "a third-party archive must not be handed /etc/ritornello by the core"
        );

        let fragment = with(&["plugins.toml.fragment"]);
        assert!(
            installable_from_ui(&fragment),
            "the plugin rule accepts a [[plugin]] block — the second discriminating fixture"
        );
        assert!(
            !archive_allowed(false, true, &fragment),
            "a [[plugin]] block names an exec path, and appending a stranger's is asking the core to launch it"
        );

        // And the two neighbouring answers of the same function, so this test
        // cannot pass by the rule having quietly become "nothing installs":
        // one of ours keeps the plugin rule, and the core keeps its exemption.
        assert!(archive_allowed(false, false, &catalog), "one of ours may ship its catalog");
        assert!(
            archive_allowed(true, false, &names(&["etc/systemd/system/ritornello.service"])),
            "the core's archive always carries units, and root can form its binary's path"
        );
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

    /// Nothing has ever been placed by this updater: the memory an automatic
    /// run reads on a device that has only ever been deployed by hand.
    fn nothing_placed() -> placed::Placed {
        placed::Placed::new()
    }

    /// The exclusions of the automatic policy, each on its own row so a
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
        //
        // **Its own name, not a second `console` row.** It used to share that
        // name with the `BinaryMissing` row below, which was harmless while
        // the decision knew nothing but the rows — and stopped being harmless
        // the moment the memory keyed itself by component name: two rows that
        // cannot be told apart is the class of fixture this chantier has been
        // caught by seven times, and this table is fed `nothing_placed()`
        // precisely so it never has to be.
        let mut silent = row("generic-input", ComponentKind::Plugin, Availability::UpdateAvailable);
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
        assert_eq!(automatic_install_list(&components, &nothing_placed()), names(&["core", "radio"]));
    }

    // ---- What the updater placed, and the nights that follow --------------

    /// The rows a check builds on a device running 0.2.0 that is offered
    /// 0.4.1 — night after night, because a rollback puts 0.2.0 back and the
    /// release does not change. That identity between the two nights *is* the
    /// first defect: nothing in these rows can tell "not tried yet" from
    /// "tried and reverted".
    fn core_offered(installed: &str, offered: &str) -> Vec<ComponentOffer> {
        let mut core = row("core", ComponentKind::Core, Availability::UpdateAvailable);
        core.installed = Some(installed.to_string());
        core.offered = Some(offered.to_string());
        vec![core]
    }

    /// **The two nights, through the file and through a second process.**
    ///
    /// Night 1 installs 0.4.1; it does not start; systemd exhausts its start
    /// limit, the rollback unit puts 0.2.0 back, and the device reboots on the
    /// old binary. Night 2 sees *exactly the same rows* — that is why the
    /// comparison between what runs and what is offered can never settle this
    /// — and must install nothing.
    ///
    /// Deliberately built on **two** workers rather than one: night 2 runs in
    /// a different process, so what it reads has to have come off the disk at
    /// a path it derived for itself. A memory held in the `Worker` would pass
    /// a one-worker test and fail on the device.
    ///
    /// The first assertion is not decoration: without it, a rule that refused
    /// everything for ever would satisfy the second.
    ///
    /// **What this one does not reach, and what does.** The write itself
    /// happens in `install_one`, behind a release server and the privileged
    /// unit; this test starts from a memory that already exists, so it proves
    /// the rule and not the writing of it. Two other things cover that:
    /// `the_memory_of_a_core_install_is_on_disk_by_the_time_the_process_leaves`
    /// drives the real pass and observes the write against the restart, and
    /// these tests go through `placed::record` rather than
    /// `Worker::remember_placed` so that deleting the production call leaves
    /// the method dead and `cargo clippy --all-targets -- -D warnings` refuses
    /// the build.
    #[test]
    fn a_core_release_that_was_rolled_back_is_not_installed_again_the_next_night() {
        let dir = tempfile::tempdir().unwrap();
        let rows = core_offered("0.2.0", "0.4.1");

        // Night 1: nothing has ever been placed on this device.
        let night_one = worker_at(dir.path(), stalled_line());
        assert_eq!(
            automatic_install_list(&rows, &placed::read(&night_one.staging)),
            names(&["core"]),
            "the first night installs it: this policy has never placed 0.4.1 here"
        );
        // What `install_one` writes the instant the privileged unit reports
        // the bytes are in place — before `install` calls the restart hook,
        // which on a device does not return.
        placed::record(
            &night_one.staging,
            "core",
            "0.4.1",
            Some(vec!["etc/systemd/system/ritornello.service".to_string()]),
        )
        .unwrap();

        // 0.4.1 never starts. The rollback unit puts 0.2.0 back and the
        // device comes up on it, in a new process.
        let night_two = worker_at(dir.path(), stalled_line());
        assert_eq!(
            automatic_install_list(&rows, &placed::read(&night_two.staging)),
            Vec::<String>::new(),
            "the second night installs nothing: 0.4.1 is the version this policy already placed and the device did not keep"
        );
    }

    /// **The way out, and the reason the memory is only ever read by the
    /// automatic policy.** The operator is allowed to try 0.4.1 again — it is
    /// also the only escape if this memory is ever wrong.
    ///
    /// The skip is a property of `automatic_install_list`, which has exactly
    /// one caller — the `Job::Scheduled` arm of `run_worker`. `Job::Install`
    /// carries the operator's names to `install` untouched, and the only
    /// transform between the two is `install_order`, asserted here beside it.
    /// The stronger half of the claim — that nothing further down consults the
    /// memory either — is proven over the real install pass by
    /// `an_install_asked_for_by_hand_goes_through_a_version_the_memory_has_given_up_on`.
    #[test]
    fn the_memory_never_stands_in_the_way_of_an_install_asked_for_by_hand() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), stalled_line());
        placed::record(&worker.staging, "core", "0.4.1", None).unwrap();
        let rows = core_offered("0.2.0", "0.4.1");
        let memory = placed::read(&worker.staging);

        assert_eq!(
            automatic_install_list(&rows, &memory),
            Vec::<String>::new(),
            "the automatic policy has given up on this version"
        );
        assert_eq!(
            install_order(&names(&["core"])),
            names(&["core"]),
            "and nothing between the operator's tick and `install_one` drops it"
        );
    }

    /// A **newer** release than the one that was rolled back goes in. The
    /// memory says "0.4.1 did not work here", not "the core is frozen".
    ///
    /// Its own test rather than a second assertion above, because the memory
    /// is non-empty here: an implementation that skipped a component merely
    /// for *having* an entry would pass the two-nights test and fail this one.
    #[test]
    fn a_release_newer_than_the_one_that_was_rolled_back_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), stalled_line());
        placed::record(&worker.staging, "core", "0.4.1", None).unwrap();
        assert_eq!(
            automatic_install_list(&core_offered("0.2.0", "0.5.0"), &placed::read(&worker.staging)),
            names(&["core"]),
            "0.5.0 is not the version that failed, and nothing is known against it"
        );
    }

    /// **The guard is not loosened.** A plugin whose installed version is
    /// unknown and that this updater has never placed stays excluded, exactly
    /// as before: switched off, dead of its own accord, or predating the
    /// version field, it would otherwise be re-downloaded every night for
    /// ever.
    ///
    /// The memory is deliberately **not empty** — it carries an entry for
    /// another component entirely — so this cannot pass by the lookup being
    /// asked of a map that answers `None` to everything.
    #[test]
    fn a_plugin_this_updater_never_placed_stays_excluded_while_its_version_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), stalled_line());
        placed::record(&worker.staging, "radio", "1.7.3", None).unwrap();

        let mut console = row("console", ComponentKind::Plugin, Availability::UpdateAvailable);
        console.installed = None;
        console.offered = Some("0.4.1".to_string());
        assert_eq!(
            automatic_install_list(&[console], &placed::read(&worker.staging)),
            Vec::<String>::new(),
            "an unknown version this updater is not responsible for is still nobody's business to fix nightly"
        );
    }

    /// **The second defect, and its repair.** A plugin whose new binary dies
    /// before announcing keeps an unknown installed version for ever, so the
    /// guard above would exclude it from every automatic run — including the
    /// release that repairs it. Having placed it is what tells this case apart
    /// from the one above.
    ///
    /// The offered version is deliberately **not** the one that was placed:
    /// that is the difference between "the release that would fix it" and the
    /// broken archive itself, which the two-nights rule still refuses.
    #[test]
    fn a_plugin_whose_placed_binary_never_spoke_is_repaired_by_the_next_release() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), stalled_line());
        placed::record(&worker.staging, "console", "0.4.1", None).unwrap();

        let mut console = row("console", ComponentKind::Plugin, Availability::UpdateAvailable);
        console.installed = None;
        console.offered = Some("0.5.0".to_string());
        assert_eq!(
            automatic_install_list(&[console.clone()], &placed::read(&worker.staging)),
            names(&["console"]),
            "the release after the one that broke it is exactly where an automatic repair is worth most"
        );

        // And the archive that broke it is still refused, so the repair costs
        // one download per released version and never one per night.
        console.offered = Some("0.4.1".to_string());
        assert_eq!(
            automatic_install_list(&[console], &placed::read(&worker.staging)),
            Vec::<String>::new(),
            "the same broken archive is not fetched again tonight"
        );
    }

    /// **A stranger's archive does not get to choose which of your plugins it
    /// replaces.**
    ///
    /// `installable_from_ui` and `only_its_own_binary` both count binaries and
    /// neither reads the name, and `install_one` hands that bare name to root,
    /// which validates its shape and forms `plugins_dir/<name>`. So without
    /// this rule an archive for `theirs` carrying
    /// `ritornello-plugin-radio` gets the **official radio binary** overwritten
    /// with the stranger's bytes — no privilege gained, and the wrong process
    /// running someone else's code.
    ///
    /// One case per operand, because it is a conjunction: the second row breaks
    /// the name and the third breaks the directory, and each alone must refuse.
    #[test]
    fn an_archive_may_not_name_a_sibling_for_root_to_replace() {
        let dir = Path::new("/usr/local/lib/ritornello/plugins");
        assert!(
            placement_target(
                "/usr/local/lib/ritornello/plugins/ritornello-plugin-theirs",
                dir,
                "ritornello-plugin-theirs"
            )
            .is_ok(),
            "the ordinary shape: the declaration and the archive name the same file"
        );
        assert!(
            matches!(
                placement_target(
                    "/usr/local/lib/ritornello/plugins/ritornello-plugin-theirs",
                    dir,
                    "ritornello-plugin-radio"
                ),
                Err(Refusal::NotItsOwnFile(_))
            ),
            "an archive naming a sibling must never reach the privileged installer"
        );
        assert!(
            matches!(
                placement_target(
                    "/opt/theirs/ritornello-plugin-theirs",
                    dir,
                    "ritornello-plugin-theirs"
                ),
                Err(Refusal::NotItsOwnFile(_))
            ),
            "root only writes into the plugins directory: a declaration pointing elsewhere would get a file its own exec never looks at, and a row claiming the new version"
        );
    }

    /// **A third-party plugin whose repository was not consulted is never
    /// served from our release.**
    ///
    /// The fall-through this replaces was reachable by an ordinary failure —
    /// the fifth third-party plugin, a slow server, a GitLab fork — and not by
    /// an attack: the row would fall to `ours`, where a colliding name (a fork
    /// of `radio` kept as `radio`) resolves to the **official** archive and is
    /// then installed under the plugin rule, `/etc/ritornello` files and all.
    ///
    /// The three other answers are asserted beside it so this cannot pass by
    /// `resolve` having quietly become "nothing resolves".
    #[test]
    fn a_third_party_plugin_whose_repository_was_not_consulted_is_never_served_from_ours() {
        let unconsulted = Checked {
            ours: radio_published("9.9.9"),
            theirs: Vec::new(),
            third_party: names(&["radio"]),
        };
        assert_eq!(
            resolve(&unconsulted, "radio"),
            Resolved::UncheckedThirdParty,
            "our own release must never answer for a name the announcement calls a stranger's"
        );

        let consulted = Checked {
            ours: radio_published("9.9.9"),
            theirs: vec![ThirdPartyOffer {
                name: "radio".to_string(),
                published: radio_published("2.0.0").remove(0),
            }],
            third_party: names(&["radio"]),
        };
        match resolve(&consulted, "radio") {
            Resolved::Theirs(published) => assert_eq!(
                published.version, "2.0.0",
                "its own repository answers, never the colliding official entry"
            ),
            other => panic!("{other:?}"),
        }

        let mine = ours(radio_published("0.3.0"));
        match resolve(&mine, "radio") {
            Resolved::Ours(published) => assert_eq!(published.version, "0.3.0"),
            other => panic!("{other:?}"),
        }
        assert_eq!(resolve(&mine, "mpd"), Resolved::Nothing);
    }

    /// **Refusal 3 of 3: a third-party component is never taken by the
    /// automatic policy, whatever that policy is.**
    ///
    /// Its own test rather than the row inside the table above, because the
    /// refusal only became load-bearing now: since a third-party plugin's own
    /// repository answers for it, its row can legitimately read
    /// `UpdateAvailable` with a known installed version and a known offered
    /// one — so it passes every other filter in that list, and the kind is the
    /// only thing left standing between an unattended device and bytes from a
    /// repository nobody vetted.
    ///
    /// The official row beside it is not decoration: without it, a function
    /// that returned nothing at all would pass.
    #[test]
    fn the_automatic_policy_never_installs_from_a_third_party_repository() {
        let mut theirs =
            row("someones-plugin", ComponentKind::ThirdParty, Availability::UpdateAvailable);
        theirs.third_party_repo = Some("someone/theirs".to_string());
        theirs.offered = Some("2.0.0".to_string());
        let mine = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        assert_eq!(
            automatic_install_list(&[theirs, mine], &nothing_placed()),
            names(&["radio"]),
            "a third-party plugin is never installed while nobody is watching, even when its own repository offers a newer version"
        );
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
            Refusal::ThirdPartyArchive,
            Refusal::NotItsOwnFile("it is declared to run /a/b, and the archive carries c".to_string()),
            Refusal::ThirdPartyUnchecked,
            Refusal::NoFragment,
            Refusal::Download("connection reset by peer".to_string()),
            Refusal::Prepare("no space left on device".to_string()),
            Refusal::Privileged("Job for ritornello-update.service failed".to_string()),
            Refusal::NothingPublished,
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
        let worker = worker_at(dir.path(), status);
        (worker, dir)
    }

    /// The same worker, on a root the caller owns — for the tests that write
    /// files under it and then read them back.
    fn worker_at(root: &Path, status: Arc<RwLock<StatusState>>) -> Worker {
        // In the **real** plugins directory, not at the bare root: that is
        // where a declared binary lives on a device, and `placement_target`
        // now refuses an install for a declaration pointing anywhere else.
        let dir = plugins_dir(root);
        std::fs::create_dir_all(&dir).unwrap();
        let exec = dir.join("ritornello-plugin-radio");
        std::fs::write(&exec, b"not a real binary, only its presence is read\n").unwrap();
        let manifest = root.join("plugins.toml");
        std::fs::write(
            &manifest,
            format!("[[plugin]]\nname = \"radio\"\nexec = {:?}\n", exec.to_string_lossy()),
        )
        .unwrap();
        Worker {
            state: Arc::new(RwLock::new(UpdateState::initial("0.2.0", &[]))),
            catalog: Arc::new(RwLock::new(Catalog::load("core", "en", root, crate::i18n::EN))),
            status,
            manifest,
            plugins_tx: mpsc::channel(1).0,
            // The product default: finished releases only. A test that wants
            // the other channel writes to this handle — see
            // `asking_for_prereleases_is_what_makes_one_visible`.
            settings: Arc::new(RwLock::new(crate::state::Settings::default())),
            staging: root.join("staging"),
            root: root.to_path_buf(),
            core_version: "0.2.0",
            restart: Arc::new(|| {}),
        }
    }

    /// A component that was already on the device and has just been replaced.
    fn replaced(component: &str, version: &str) -> Placement {
        Placement {
            component: component.to_string(),
            version: version.to_string(),
            fresh: false,
        }
    }

    /// A component the device did not have.
    fn installed(component: &str, version: &str) -> Placement {
        Placement { component: component.to_string(), version: version.to_string(), fresh: true }
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

    /// RULING 51: `Availability::Undeclared` needs a producer, and this is
    /// it — a binary sitting in the real plugins directory that nothing
    /// declares becomes an `Installed` row with `declared: false,
    /// binary_present: true`, alongside — not instead of — the declared
    /// plugin's own row, which lives in that same directory and must not be
    /// swept up with it.
    #[tokio::test]
    async fn a_binary_with_no_declaration_is_reported_as_installed_but_undeclared() {
        let status = one_line(PluginStatus::kind("radio", "source", true, false));
        let (worker, dir) = worker_rig(status);
        let plugins_dir = ritornello_updater::target::plugins_dir(dir.path());
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::write(plugins_dir.join("ritornello-plugin-orphan"), b"").unwrap();

        let installed = worker.installed_when_settled().await;

        assert!(installed.iter().any(|i| i.name == "radio"), "the declared plugin must still be there");
        // Named by the component the release would publish, not by the bare
        // file the scan found underneath it — see `component_name_from_file`.
        let orphan = installed
            .iter()
            .find(|i| i.name == "orphan")
            .expect("the undeclared binary must be reported, named as the release would know it");
        assert!(!orphan.declared);
        assert!(orphan.binary_present);
    }

    /// **C1 of task 18's review.** Before the fix, an `undeclared_binary`
    /// row's `Installed.name` was the bare file (`ritornello-plugin-mpd`);
    /// `resolve`/`carries` only ever match a **component** name (`mpd`)
    /// against the release; so the name the row sent to `install()` never
    /// matched anything, `resolve` returned `Resolved::Nothing`, and that arm
    /// is a `tracing::warn!` plus a silent `continue` — no catalog message,
    /// `state.outcome` untouched. The operator pressed "Declare" and nothing
    /// happened, with no toast.
    ///
    /// This reads the name from `Worker::installed()` itself — never
    /// hard-coded as `"mpd"`, which would only prove `resolve` can match a
    /// name that was never wrong in the first place — and calls `install()`
    /// with it, which is what the route actually calls, not `resolve()` in
    /// isolation. It asserts on `state.outcome`, the payload `/api/update`
    /// serves: the one thing a review of this task singled out as the
    /// difference between "a function returns the right value" and "the
    /// gesture actually reaches the page". Before the fix, `state.outcome`
    /// stays whatever it was (nothing happened); after it, `install_one` is
    /// genuinely entered — a real request lands on the mock server below —
    /// and fails at digest verification (`served_with_wrong_digest`), never
    /// at the real, un-mockable `systemctl` this sandbox has no unit for.
    #[tokio::test]
    async fn declaring_an_undeclared_binary_by_its_component_name_reaches_install_one() {
        let status = one_line(PluginStatus::kind("radio", "source", true, false));
        let (worker, dir) = worker_rig(status);
        let dir_plugins = plugins_dir(dir.path());
        // What a hand-drop or an interrupted uninstall leaves: present,
        // undeclared, named the way the release's own convention names it.
        std::fs::write(dir_plugins.join("ritornello-plugin-mpd"), b"old").unwrap();

        let fragment = format!(
            "[[plugin]]\nname = \"mpd\"\nexec = {:?}\n",
            dir_plugins.join("ritornello-plugin-mpd").to_string_lossy()
        );
        let archive = targz(&[
            ("usr/local/lib/ritornello/plugins/ritornello-plugin-mpd", b"NEW"),
            ("plugins.toml.fragment", fragment.as_bytes()),
        ]);
        let published = served_with_wrong_digest("mpd", &archive).await;
        let checked = Checked { ours: vec![published], theirs: vec![], third_party: vec![] };
        let client = client().unwrap();

        // The name **as the row itself reports it** — not hard-coded as
        // "mpd" here, which would only prove `resolve` can match a name that
        // was never wrong in the first place. This is what ties the two
        // halves of the fix together: `Worker::installed()` must derive the
        // component name, and `install()` must then resolve *that* name
        // against the release.
        let installed = worker.installed_when_settled().await;
        let orphan =
            installed.iter().find(|i| !i.declared).expect("the undeclared binary must be reported");
        let name = orphan.name.clone();

        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install(&client, &checked, &[name]),
        )
        .await
        .expect("install() hung");

        match &worker.state.read().await.outcome {
            CheckOutcome::Failed(message) => {
                // Exact equality, not `contains("mpd")` (re-review Finding 1):
                // the unmapped file name is `ritornello-plugin-mpd`, and its
                // own refusal message — "No release publishes
                // ritornello-plugin-mpd…" — still contains the substring
                // "mpd" (its own last three characters), so a `contains`
                // check here cannot tell the fixed name from the broken one
                // and stays green under the exact mutation this test exists
                // to catch. Confirmed by reverting `component_name_from_file`
                // to the identity function and re-running: with the old
                // `contains` assertion the test stayed green; with this exact
                // comparison it reddens (`state.outcome` carries
                // `update_nothing_published` filled with the file name
                // instead of `update_digest_mismatch` filled with `mpd`).
                let catalog = Catalog::load("core", "en", Path::new("/nonexistent"), crate::i18n::EN);
                let expected = refusal_message(&catalog, "mpd", &Refusal::DigestMismatch);
                assert_eq!(message, &expected, "the refusal must name exactly the component `mpd`");
            }
            other => panic!(
                "expected install_one to have been reached and refused at digest \
                 verification, got {other:?} — resolve() likely fell back to \
                 Resolved::Nothing and never made the request at all"
            ),
        }
    }

    /// I1's second adjustment: `Job::RemovePlugin` is serialised behind any
    /// install in flight, and an install places its binary *before* writing
    /// its declaration — so a file undeclared when the route accepted the
    /// request can be declared by the time this job actually runs.
    /// `remove_plugin_binary` must re-read the manifest and skip rather than
    /// erase a now-declared plugin's binary.
    #[tokio::test]
    async fn remove_plugin_binary_skips_a_file_that_became_declared_while_queued() {
        let status = one_line(PluginStatus::kind("radio", "source", true, false));
        let (worker, _dir) = worker_rig(status);
        // `worker_rig` already declares "radio" with exactly this exec file
        // name — the file this call names is, right now, a declared plugin's
        // own binary.
        worker.remove_plugin_binary("orphan", "ritornello-plugin-radio").await;

        assert!(
            !worker.staging.join("request.json").exists(),
            "a file that is now declared must never reach the privileged installer"
        );
    }

    /// The other side of the same check: a file matching no declared plugin's
    /// exec proceeds to the privileged side (observed here as `request.json`
    /// reaching the staging directory, the same seam the two tests above this
    /// one already use — `run_privileged_unit` is never awaited to succeed).
    #[tokio::test]
    async fn remove_plugin_binary_proceeds_for_a_file_matching_no_declared_exec() {
        let status = one_line(PluginStatus::kind("radio", "source", true, false));
        let (worker, _dir) = worker_rig(status);
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.remove_plugin_binary("orphan", "ritornello-plugin-orphan"),
        )
        .await
        .expect("remove_plugin_binary hung");

        assert!(
            worker.staging.join("request.json").exists(),
            "an undeclared file's removal must reach the privileged installer"
        );
    }

    /// A check that found only our own release, which is the ordinary shape.
    fn ours(published: Vec<Published>) -> Checked {
        Checked { ours: published, theirs: Vec::new(), third_party: Vec::new() }
    }

    // ---- The refusals AT THEIR CALL SITE --------------------------------
    //
    // The doctrine in this module is that the worker is untested, and for most
    // of it that is right: it fetches from GitHub and starts a systemd unit.
    // But both refusals below happen **before** any byte is written and before
    // `systemctl` is ever reached, which is exactly what makes them
    // observable: a one-shot `TcpListener` for the archive and one for its
    // `SHA256SUMS` is the whole rig, and no `systemctl` is needed at all.
    //
    // Written down because the first version of this task called that
    // impossible: the missing seam was never a seam, only the assumption that
    // `install_one` had to run to completion to be observed.

    /// A gzipped tar of `(path, bytes)`, as a release archive is built.
    fn targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).expect("append");
        }
        let tar = builder.into_inner().expect("finish");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).expect("compress");
        gz.finish().expect("finish gz")
    }

    /// One HTTP/1.1 answer of `status`, served to the first connection, at a
    /// URL whose last segment is `file` — that is what `asset_name` reads to
    /// look the digest up in `SHA256SUMS`.
    async fn serve_with(status: u16, body: Vec<u8>, file: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored).await;
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://127.0.0.1:{port}/{file}")
    }

    async fn serve_once(body: Vec<u8>, file: &str) -> String {
        serve_with(200, body, file).await
    }

    /// An address nothing is listening on: the listener is bound only to
    /// reserve a port, then dropped. Drives the connection-failure branch,
    /// which no status code can produce.
    async fn refused_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}/releases")
    }

    /// A `Published` pointing at those two servers, with a digest that
    /// matches — so the download and the verification both succeed and what
    /// the test observes is the refusal, not a checksum failure.
    async fn served(name: &str, archive: &[u8]) -> Published {
        let file = format!("ritornello-plugin-{name}-2.0.0-x86_64.tar.gz");
        let url = serve_once(archive.to_vec(), &file).await;
        let sums = format!("{}  {file}\n", digest_hex(archive));
        let checksums_url = serve_once(sums.into_bytes(), "SHA256SUMS").await;
        Published {
            offer: Offer::Plugin(name.to_string()),
            version: "2.0.0".to_string(),
            url,
            size: 0,
            release_tag: "v2.0.0".to_string(),
            checksums_url: Some(checksums_url),
        }
    }

    /// The same rig as `served`, but the checksums file names a digest that
    /// does not match the archive — so `install_one` reaches the real
    /// download and fails at `verify_digest`, never at `run_privileged_unit`.
    ///
    /// Built for exactly one test
    /// (`declaring_an_undeclared_binary_by_its_component_name_reaches_install_one`):
    /// that test needs a refusal that proves `install_one` was actually
    /// entered — a real request against a real socket — without depending on
    /// this sandbox's `systemctl` at all, the same "no systemctl is needed"
    /// doctrine the comment above `targz` already states for its two
    /// neighbours.
    /// **Success theatre, closed and pinned.** The route answers 204 the
    /// instant the erasure is queued; until the failure had a channel of its
    /// own, a device without the polkit rule said "OK" to every Uninstall and
    /// every "Remove the binary" for ever, erased nothing, and put the row
    /// back as "Installed but not declared" with no account anywhere the
    /// operator looks.
    ///
    /// Driven through the real `remove_plugin_binary`, with the privileged
    /// unit answering exactly what a missing polkit rule produces. The
    /// assertion is on `outcome`, which is what the card renders — not on the
    /// log, which is where this used to stop.
    #[tokio::test]
    async fn a_binary_that_could_not_be_erased_says_so_on_the_page() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), stalled_line());
        let _privileged = Privileged::answers(Err("Access denied".to_string()));

        // A file no declaration names, which is the state an uninstall leaves
        // behind: `radio` is the one `worker_at` declares, so `mpd` cannot
        // collide with it.
        worker.remove_plugin_binary("mpd", "ritornello-plugin-mpd").await;

        match &worker.state.read().await.outcome {
            CheckOutcome::Failed(message) => {
                assert!(message.contains("mpd"), "the sentence must name the component: {message}");
                assert!(
                    message.contains("Access denied"),
                    "and carry the installer's own words, which are the whole diagnosis: {message}"
                );
            }
            other => panic!("a failed erasure must reach the page, not only the log: {other:?}"),
        }
    }

    /// The other half: an erasure that is skipped because the file became
    /// declared again is not a failure of the privileged step, and it is not
    /// silence either — the operator asked for something that did not happen.
    #[tokio::test]
    async fn an_erasure_skipped_because_the_file_is_declared_again_also_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), stalled_line());
        // `worker_at` declares `radio` with exactly this file.
        worker.remove_plugin_binary("radio", "ritornello-plugin-radio").await;
        match &worker.state.read().await.outcome {
            CheckOutcome::Failed(message) => {
                assert!(message.contains("radio"), "{message}");
                assert!(!message.contains('{'), "a parameter was left unfilled: {message}");
            }
            other => panic!("a skipped erasure must reach the page too: {other:?}"),
        }
    }

    /// The same rig for the **core's** own archive: the asset name carries no
    /// plugin, and the offer is `Offer::Core`, which is what sends
    /// `install_one` down the branch that ends in the restart.
    async fn served_core(archive: &[u8]) -> Published {
        let file = format!("ritornello-core-2.0.0-{ARCH}.tar.gz");
        let url = serve_once(archive.to_vec(), &file).await;
        let sums = format!("{}  {file}\n", digest_hex(archive));
        let checksums_url = serve_once(sums.into_bytes(), "SHA256SUMS").await;
        Published {
            offer: Offer::Core,
            version: "2.0.0".to_string(),
            url,
            size: 0,
            release_tag: "v2.0.0".to_string(),
            checksums_url: Some(checksums_url),
        }
    }

    /// Holds the privileged unit's answer for one test, and puts the light out
    /// again on the way out.
    ///
    /// A guard rather than a bare set, so no test can inherit another's answer
    /// even if two of them ever share a thread. Nothing production-side can
    /// see this: `FAKE_PRIVILEGED` is `cfg(test)`.
    struct Privileged;

    impl Privileged {
        fn answers(answer: Result<(), String>) -> Self {
            FAKE_PRIVILEGED.with(|f| *f.borrow_mut() = Some(answer));
            Self
        }
    }

    impl Drop for Privileged {
        fn drop(&mut self) {
            FAKE_PRIVILEGED.with(|f| *f.borrow_mut() = None);
        }
    }

    /// A core archive: its binary, plus one file the installer never places —
    /// which is what `archive::core_not_installed` puts in the note.
    fn core_archive() -> Vec<u8> {
        targz(&[
            (archive::CORE_BINARY, b"ELF, as far as this test is concerned"),
            ("etc/systemd/system/ritornello-rollback.service", b"[Unit]\n"),
        ])
    }

    /// Runs one install pass for the core and answers what the memory looked
    /// like **at the instant the restart hook fired** — which on a device is
    /// the instant the process stops existing. `None` means it never fired.
    ///
    /// The hook is the observer, and that is the whole trick: it is already
    /// injectable, it is already the last thing a core install does, and
    /// snapshotting inside it is the only way to see which side of the exit a
    /// write fell on.
    async fn memory_at_the_exit(worker: &mut Worker, checked: &Checked) -> Option<placed::Placed> {
        let staging = worker.staging.clone();
        let seen: Arc<std::sync::Mutex<Option<placed::Placed>>> = Arc::default();
        let recorder = seen.clone();
        worker.restart = Arc::new(move || {
            *recorder.lock().unwrap() = Some(placed::read(&staging));
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install(&client().unwrap(), checked, &names(&[CORE])),
        )
        .await
        .expect("the install pass hung");
        let snapshot = seen.lock().unwrap();
        snapshot.clone()
    }

    /// **The property this whole task exists for, observed rather than
    /// reasoned about: the note of what was placed is on disk before the
    /// process leaves.**
    ///
    /// The first version of this work called it unreachable and settled for a
    /// dead-code tripwire, on the grounds that getting here needed a release
    /// server and an injectable privileged step. Half of that was already in
    /// this module (`served`, `targz`), and the other half is a `cfg(test)`
    /// red light inside `run_privileged_unit` — no field on `Worker`, no
    /// production struct touched. That is one more instance of this project's
    /// oldest lesson: "untestable" is one hard part welded to one merely
    /// unbuilt one.
    ///
    /// What it catches that the tripwire cannot: moving the write to **after**
    /// `(self.restart)()`. That leaves `remember_placed` called, so
    /// `-D warnings` is satisfied and every other test in the workspace stays
    /// green — and on a device it is exactly the original defect, a write
    /// performed by a process that has already exited.
    ///
    /// The version is `2.0.0`, which no other fixture here places and which no
    /// `worker_at` core version equals, so a snapshot taken off the wrong file
    /// or the wrong key cannot read as a pass.
    #[tokio::test]
    async fn the_memory_of_a_core_install_is_on_disk_by_the_time_the_process_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let mut worker = worker_at(dir.path(), stalled_line());
        let _privileged = Privileged::answers(Ok(()));
        let checked = Checked {
            ours: vec![served_core(&core_archive()).await],
            theirs: Vec::new(),
            third_party: Vec::new(),
        };

        let memory = memory_at_the_exit(&mut worker, &checked)
            .await
            .expect("the install never reached the restart");

        assert_eq!(
            placed::version_of(&memory, CORE),
            Some("2.0.0"),
            "the placed version must already be on disk when the restart hook fires: written after it, it would be written by a process that no longer exists"
        );
        assert_eq!(
            memory[CORE].not_installed_files.as_deref(),
            Some(["etc/systemd/system/ritornello-rollback.service".to_string()].as_slice()),
            "and so must the note of what the archive carried and nobody installed"
        );
    }

    /// **The way out, driven through the real install pass.**
    ///
    /// The memory already says this policy placed 2.0.0 and the device did not
    /// keep it — which is exactly what stops an automatic run. The operator
    /// ticks the row anyway, and the install goes all the way to the restart.
    ///
    /// Its companion over the decision function states the same rule; this one
    /// proves that nothing between `Job::Install` and the privileged unit
    /// consults the memory at all. A defect that moved the check down into
    /// `install` or `install_one` — the tempting "fix" — reddens here and
    /// nowhere else.
    #[tokio::test]
    async fn an_install_asked_for_by_hand_goes_through_a_version_the_memory_has_given_up_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut worker = worker_at(dir.path(), stalled_line());
        placed::record(&worker.staging, CORE, "2.0.0", None).unwrap();
        let _privileged = Privileged::answers(Ok(()));
        let checked = Checked {
            ours: vec![served_core(&core_archive()).await],
            theirs: Vec::new(),
            third_party: Vec::new(),
        };

        assert!(
            memory_at_the_exit(&mut worker, &checked).await.is_some(),
            "a manual install of the version the automatic policy skips must still reach the restart"
        );
    }

    async fn served_with_wrong_digest(name: &str, archive: &[u8]) -> Published {
        let file = format!("ritornello-plugin-{name}-2.0.0-x86_64.tar.gz");
        let url = serve_once(archive.to_vec(), &file).await;
        let sums = format!("{}  {file}\n", digest_hex(b"not the archive's real bytes"));
        let checksums_url = serve_once(sums.into_bytes(), "SHA256SUMS").await;
        Published {
            offer: Offer::Plugin(name.to_string()),
            version: "2.0.0".to_string(),
            url,
            size: 0,
            release_tag: "v2.0.0".to_string(),
            checksums_url: Some(checksums_url),
        }
    }

    /// A GitHub releases listing carrying exactly these asset names, one
    /// published release. Written out rather than built from a fixture helper
    /// because what it must exercise is `parse_releases`' own reading.
    fn releases_body(assets: &[&str]) -> Vec<u8> {
        one_release(assets, false)
    }

    /// The same list, with the release flagged as a **prerelease**: the one
    /// bit that decides whether a device on the finished-releases channel may
    /// see it at all.
    fn prerelease_body(assets: &[&str]) -> Vec<u8> {
        one_release(assets, true)
    }

    fn one_release(assets: &[&str], prerelease: bool) -> Vec<u8> {
        let assets: Vec<String> = assets
            .iter()
            .map(|n| {
                format!(r#"{{"name":"{n}","browser_download_url":"https://x/{n}","size":1}}"#)
            })
            .collect();
        format!(
            r#"[{{"tag_name":"v2.0.0","published_at":"2026-01-01T00:00:00Z","draft":false,"prerelease":{prerelease},"assets":[{}]}}]"#,
            assets.join(",")
        )
        .into_bytes()
    }

    /// The asset name a repository must publish for the plugin `name`, for the
    /// architecture this binary was built for.
    fn asset_for(name: &str, version: &str) -> String {
        format!("ritornello-plugin-{name}-{version}-{ARCH}.tar.gz")
    }

    /// **The last decision on the path that lets bytes arrive from a
    /// repository we do not control, and the one that had no test.**
    ///
    /// Five repositories answering five different ways, driven through the
    /// real `fetch_text` against real sockets. Two properties in one run,
    /// because they are one loop:
    ///
    /// - **an asset must be named for the plugin it is being fetched for.** A
    ///   repository publishing `ritornello-plugin-radio-…` when asked about
    ///   `bravo` has published nothing for the binary on this device, and a
    ///   repository publishing `ritornello-core-…` must never produce an offer
    ///   at all — a core offer from a stranger is the one component the plugin
    ///   rule never judges;
    /// - **one bad answer costs only its own row.** The three failure shapes
    ///   are distinct branches — a refused connection, a non-200, a body that
    ///   is not a release list — and the two good repositories are placed
    ///   **around** them, so an implementation that gave up on the first
    ///   failure would lose `foxtrot` and one that gave up on the last would
    ///   lose nothing visible.
    #[tokio::test]
    async fn a_third_party_repository_answers_only_for_the_plugin_it_names() {
        let (worker, _dir) = worker_rig(starting_line());
        let client = client().unwrap();

        let targets = vec![
            // Publishes an archive named for somebody else's plugin.
            Target {
                name: "bravo".to_string(),
                repo: "b/bravo".to_string(),
                url: serve_once(releases_body(&[&asset_for("radio", "3.0.0")]), "releases").await,
            },
            // Answers properly.
            Target {
                name: "alpha".to_string(),
                repo: "a/alpha".to_string(),
                url: serve_once(releases_body(&[&asset_for("alpha", "2.0.0")]), "releases").await,
            },
            // Nothing listening at all.
            Target {
                name: "charlie".to_string(),
                repo: "c/charlie".to_string(),
                url: refused_url().await,
            },
            // Rate-limited — and its body is a **perfectly good** release
            // listing naming delta's own asset. Deliberately: an error body
            // that also failed to parse would be refused by the next clause
            // for a different reason, and the fixture could then not tell the
            // status check from its absence. What this pins is that the
            // **status** decides, not the bytes that came with it.
            Target {
                name: "delta".to_string(),
                repo: "d/delta".to_string(),
                url: serve_with(403, releases_body(&[&asset_for("delta", "6.0.0")]), "releases")
                    .await,
            },
            // 200, and a body that is not a release list.
            Target {
                name: "echo".to_string(),
                repo: "e/echo".to_string(),
                url: serve_once(b"<html>not json</html>".to_vec(), "releases").await,
            },
            // Publishes a CORE archive: a stranger must never offer one.
            Target {
                name: "foxtrot".to_string(),
                repo: "f/foxtrot".to_string(),
                url: serve_once(
                    releases_body(&[
                        &format!("ritornello-core-4.0.0-{ARCH}.tar.gz"),
                        &asset_for("foxtrot", "5.1.0"),
                    ]),
                    "releases",
                )
                .await,
            },
        ];

        let offers = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.third_party_offers(&client, &targets),
        )
        .await
        .expect("third_party_offers hung");

        let got: Vec<(&str, &str)> =
            offers.iter().map(|o| (o.name.as_str(), o.published.version.as_str())).collect();
        assert_eq!(
            got,
            vec![("alpha", "2.0.0"), ("foxtrot", "5.1.0")],
            "only the repositories that published an archive named for their own plugin answer, and a bad answer costs only its own row"
        );
        // The stranger publishing a core archive offered its plugin and
        // nothing else: no `Offer::Core` can reach the rows this way.
        assert!(
            offers.iter().all(|o| matches!(&o.published.offer, Offer::Plugin(n) if *n == o.name)),
            "a third-party offer is always a plugin offer, named for that plugin: {offers:#?}"
        );
    }

    /// **The switch is what makes a prerelease visible, and it is read on the
    /// path rather than remembered.**
    ///
    /// Two runs of the same worker over two repositories — one publishing a
    /// finished release, one publishing a prerelease — with nothing changed
    /// between them but `update_prereleases`. Real sockets, the real
    /// `fetch_text`, the real `third_party_offers`.
    ///
    /// What this pins is not the filter itself, which `parse_releases` has its
    /// own tests for on both channels, but that the setting is **consulted by
    /// the code that fetches**: a worker that had copied the value at
    /// construction, or that never passed it down, would answer the same
    /// thing twice and this test would catch it.
    ///
    /// It also drives a prerelease version through `classify_asset` on a real
    /// path — `…-3.0.0-beta.1-<arch>.tar.gz`, the very name whose extra dash
    /// made the archive invisible before this branch.
    #[tokio::test]
    async fn asking_for_prereleases_is_what_makes_one_visible() {
        let (worker, _dir) = worker_rig(starting_line());
        let client = client().unwrap();

        // Fresh listeners for each run: `serve_once` answers one request.
        async fn two_repositories() -> Vec<Target> {
            vec![
                Target {
                    name: "alpha".to_string(),
                    repo: "a/alpha".to_string(),
                    url: serve_once(releases_body(&[&asset_for("alpha", "2.0.0")]), "releases")
                        .await,
                },
                Target {
                    name: "bravo".to_string(),
                    repo: "b/bravo".to_string(),
                    url: serve_once(
                        prerelease_body(&[&asset_for("bravo", "3.0.0-beta.1")]),
                        "releases",
                    )
                    .await,
                },
            ]
        }

        async fn sweep(worker: &Worker, client: &reqwest::Client) -> Vec<(String, String)> {
            let targets = two_repositories().await;
            let offers = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                worker.third_party_offers(client, &targets),
            )
            .await
            .expect("third_party_offers hung");
            offers.into_iter().map(|o| (o.name, o.published.version)).collect()
        }

        // Off, which is the product default and is left untouched here.
        assert!(
            !worker.settings.read().await.update_prereleases,
            "the rig must start on the default, or this test proves nothing"
        );
        assert_eq!(
            sweep(&worker, &client).await,
            vec![("alpha".to_string(), "2.0.0".to_string())],
            "a prerelease must not be offered to a device that never asked for one"
        );

        // On, and nothing else changed.
        worker.settings.write().await.update_prereleases = true;
        assert_eq!(
            sweep(&worker, &client).await,
            vec![
                ("alpha".to_string(), "2.0.0".to_string()),
                ("bravo".to_string(), "3.0.0-beta.1".to_string()),
            ],
            "the finished release still answers, and the prerelease now does too — \
             with its whole version, dash included"
        );
    }

    /// **RULING 64 at the call site: the third-party path really does call the
    /// stricter rule.**
    ///
    /// The archive carries its binary **and a locale catalog** — a shape
    /// `installable_from_ui` accepts, asserted here so the fixture is proven to
    /// be the discriminating one. Deleting the `archive_allowed` guard in
    /// `install_one` makes this red, and what goes red is not only the outcome:
    /// the mutated path writes the stranger's catalog under `/etc/ritornello`
    /// and goes on to `systemctl`.
    #[tokio::test]
    async fn install_one_refuses_a_third_party_archive_before_writing_anything() {
        let status = one_line(PluginStatus {
            version: Some("1.0.0".into()),
            repository: Some("https://github.com/someone/radio".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        });
        let (worker, dir) = worker_rig(status);
        let archive = targz(&[
            ("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF"),
            ("etc/ritornello/locales/radio/fr.toml", b"a = \"b\"\n"),
        ]);
        assert!(
            installable_from_ui(&archive::read(&archive, DECOMPRESSED_MAX).unwrap().entries),
            "our own rule accepts this archive — that is what makes it the discriminating fixture"
        );
        let published = served("radio", &archive).await;
        let client = client().unwrap();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client, "radio", &published, true),
        )
        .await
        .expect("install_one hung");
        assert!(
            matches!(outcome, Err(Refusal::ThirdPartyArchive)),
            "expected ThirdPartyArchive, got {:?}",
            outcome.as_ref().err()
        );
        assert!(
            !dir.path().join("etc/ritornello/locales/radio/fr.toml").exists(),
            "the core wrote /etc/ritornello for a stranger's archive"
        );
        assert!(
            !worker.staging.join("request.json").exists(),
            "a refused archive must never reach the privileged installer"
        );
    }

    /// The same seam for the other half of the boundary: an archive that
    /// passes the strict rule — one binary, nothing else — but names a
    /// **sibling**.
    ///
    /// Without `placement_target`, this writes a `request.json` asking root to
    /// place `ritornello-plugin-radio` from the stranger's bytes, and the page
    /// then says "theirs updated to 2.0.0" while radio's row still reads
    /// aligned until its next restart runs someone else's code.
    #[tokio::test]
    async fn install_one_refuses_an_archive_that_names_another_plugins_file() {
        let status = one_line(PluginStatus {
            version: Some("1.0.0".into()),
            repository: Some("https://github.com/someone/theirs".into()),
            ..PluginStatus::kind("theirs", "source", true, false)
        });
        let (worker, dir) = worker_rig(status);
        // `theirs` is declared, and its own file sits beside radio's.
        let theirs_exec = plugins_dir(dir.path()).join("ritornello-plugin-theirs");
        std::fs::write(&theirs_exec, b"x").unwrap();
        let mut manifest = std::fs::read_to_string(&worker.manifest).unwrap();
        manifest.push_str(&format!(
            "\n[[plugin]]\nname = \"theirs\"\nexec = {:?}\n",
            theirs_exec.to_string_lossy()
        ));
        std::fs::write(&worker.manifest, manifest).unwrap();

        let archive =
            targz(&[("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"STRANGER")]);
        assert!(
            only_its_own_binary(&archive::read(&archive, DECOMPRESSED_MAX).unwrap().entries),
            "the strict archive rule accepts this — it counts binaries and does not read names, which is why the second refusal exists"
        );
        let published = served("theirs", &archive).await;
        let client = client().unwrap();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client, "theirs", &published, true),
        )
        .await
        .expect("install_one hung");
        assert!(
            matches!(outcome, Err(Refusal::NotItsOwnFile(_))),
            "expected NotItsOwnFile, got {:?}",
            outcome.as_ref().err()
        );
        assert!(
            !worker.staging.join("request.json").exists(),
            "root must never be asked to place a file the plugin is not declared to run"
        );
        assert_eq!(
            std::fs::read(plugins_dir(dir.path()).join("ritornello-plugin-radio")).unwrap(),
            b"not a real binary, only its presence is read\n",
            "the sibling the archive named must be untouched"
        );
    }

    /// **The hop RULING 5 exists for**, driven from the event rather than from
    /// the method: `Installed.repository` is fed from the published status
    /// line, so without this relay no row is ever classified third-party, no
    /// repository is ever queried, and the whole third-party path silently
    /// does nothing while every row still renders.
    ///
    /// Both ends are asserted: the raw string arrives verbatim, and it is the
    /// fact the third-party path actually acts on.
    #[tokio::test]
    async fn the_repository_a_plugin_announced_reaches_the_rows_that_judge_it() {
        let status = one_line(PluginStatus {
            version: Some("1.4.0".into()),
            repository: Some("https://github.com/someone/their-plugin".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        });
        let (worker, _dir) = worker_rig(status);
        let installed = worker.installed_when_settled().await;
        let radio = installed.iter().find(|i| i.name == "radio").expect("the declared plugin");
        assert_eq!(
            radio.repository.as_deref(),
            Some("https://github.com/someone/their-plugin"),
            "relayed verbatim and unparsed, exactly as the announcement gave it"
        );
        let targets = third_party_targets(&installed);
        assert_eq!(
            targets.iter().map(|t| (t.name.as_str(), t.repo.as_str())).collect::<Vec<_>>(),
            vec![("radio", "someone/their-plugin")],
            "and it is what decides which repository the check goes and asks"
        );
    }

    /// The other side of the same relay, and the majority path: the ten
    /// official plugins all announce the workspace's full URL, and a line
    /// carrying it must produce **no** third-party request at all.
    #[tokio::test]
    async fn a_plugin_announcing_our_own_url_makes_no_third_party_request() {
        let status = one_line(PluginStatus {
            version: Some("0.2.0".into()),
            repository: Some("https://github.com/skerdudou/ritornello".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        });
        let (worker, _dir) = worker_rig(status);
        let installed = worker.installed_when_settled().await;
        assert!(
            third_party_targets(&installed).is_empty(),
            "an official plugin must never send the core asking a stranger's repository about it"
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
                &ours(radio_published("0.3.0")),
                &[replaced("radio", "0.3.0")],
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
            worker.conclude_install(&ours(radio_published("0.3.0")), &[], Some("nope".to_string())),
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
        let two = [replaced("radio", "0.3.0"), replaced("mpd", "0.3.1")];
        assert_eq!(
            install_report(&english, &two, None),
            Some(CheckOutcome::Installed("mpd updated to 0.3.1".to_string())),
            "the most recent attempt is the one reported"
        );
        // A plugin that was not there has no version it moved from, and the
        // sentence must not invent one. Same list, same last entry, one field
        // different: what separates the two sentences is `fresh` and nothing
        // else.
        let fresh = [replaced("radio", "0.3.0"), installed("mpd", "0.3.1")];
        assert_eq!(
            install_report(&english, &fresh, None),
            Some(CheckOutcome::Installed("mpd installed".to_string())),
            "a first installation is not an update to a version it never had"
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
        // And so does the sentence for a first installation, which is a
        // second key and therefore a second thing the French pack can be
        // missing.
        let fresh_fr = install_report(&french(), &fresh, None);
        let Some(CheckOutcome::Installed(message)) = fresh_fr else { panic!("{fresh_fr:?}") };
        assert!(!message.starts_with("update_"), "{message}");
        assert!(!message.contains('{'), "{message}");
        assert!(message.contains("mpd"), "{message}");

        let french = install_report(&french(), &two, None);
        let Some(CheckOutcome::Installed(message)) = french else { panic!("{french:?}") };
        assert!(!message.contains('{'), "{message}");
        assert!(message.contains("mpd") && message.contains("0.3.1"), "{message}");
    }

    /// **A plugin the device does not declare, whose archive carries no block,
    /// is refused.** Installing it would place a binary nothing ever launches,
    /// and the whole point of refusing here is that it happens before a single
    /// byte is written.
    ///
    /// One case per operand rather than one per outcome: what makes the refusal
    /// reachable is the conjunction "not declared" **and** "no fragment", and
    /// each of the first two rows below breaks exactly one of them.
    #[test]
    fn a_plugin_nobody_would_declare_is_refused_before_anything_is_written() {
        let block = "[[plugin]]\nname = \"mpd\"\nexec = \"/opt/mpd\"\n";
        // Not declared, no block: the silent failure this refusal exists for.
        assert!(matches!(declaration_needed(false, false, None), Err(Refusal::NoFragment)));
        // Not declared, a block: installed and declared from it.
        assert_eq!(
            declaration_needed(false, false, Some(block)).unwrap().as_deref(),
            Some(block),
            "the archive's own block, handed over unchanged"
        );
        // Declared already, no block: an ordinary update, and the entry the
        // operator wrote is left exactly as it is.
        assert_eq!(declaration_needed(false, true, None).unwrap(), None);
        // Declared already, and the archive carries its block anyway — which
        // every plugin archive does. Appending it would be a duplicate entry.
        assert_eq!(declaration_needed(false, true, Some(block)).unwrap(), None);
        // The core declares nothing in `plugins.toml`, whatever its archive
        // carries and whatever `declared` says about a name it does not have.
        assert_eq!(declaration_needed(true, false, None).unwrap(), None);
        assert_eq!(declaration_needed(true, false, Some(block)).unwrap(), None);
    }

    /// The naming a shipped configuration takes on the device, and what is
    /// refused before a path is ever formed.
    #[test]
    fn an_initial_configuration_lands_under_its_operating_name_and_nowhere_else() {
        assert_eq!(
            initial_config_target("stations.example.toml").as_deref(),
            Some("stations.toml"),
            "the archive ships the reference file; the device wants the operating one"
        );
        assert_eq!(
            initial_config_target("input-bindings.example.toml").as_deref(),
            Some("input-bindings.toml")
        );
        // Not a `.example.toml`: it keeps the name it arrived with rather than
        // being mangled.
        assert_eq!(initial_config_target("presets.json").as_deref(), Some("presets.json"));
        // A nested entry: `/etc/ritornello/<dir>/<file>` is a shape nothing
        // packs, and the core does not create directories an archive names.
        assert_eq!(initial_config_target("sub/stations.example.toml"), None);
        assert_eq!(initial_config_target("sub/"), None);
        // A dotted name would let an archive designate the very temporary
        // `write_atomic` writes beside its target.
        assert_eq!(initial_config_target(".stations.toml.tmp"), None);
        assert_eq!(initial_config_target(""), None);
    }

    /// **What is already there is never replaced, and what is missing is
    /// written.** Both halves in one test because they are one rule, and each
    /// alone would pass with the other's branch deleted.
    #[test]
    fn a_shipped_configuration_only_fills_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));
        let etc = dir.path().join(ETC_DIR);
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(etc.join("stations.toml"), b"# what the operator built\n").unwrap();

        worker
            .write_initial_config(&[
                ("stations.example.toml".to_string(), b"# the shipped defaults\n".to_vec()),
                ("input-bindings.example.toml".to_string(), b"# the shipped bindings\n".to_vec()),
            ])
            .unwrap();

        assert_eq!(
            std::fs::read(etc.join("stations.toml")).unwrap(),
            b"# what the operator built\n",
            "a station list built from the browser must survive an installation"
        );
        assert_eq!(
            std::fs::read(etc.join("input-bindings.toml")).unwrap(),
            b"# the shipped bindings\n",
            "there was nothing there: the plugin must not start on an empty file"
        );
    }

    /// The declaration is written **from the archive's own fragment**, and
    /// after the binary. Driven through `write_declaration` rather than
    /// `append_block` directly: what is asserted is that the worker reads the
    /// file, hands the fragment over unchanged and writes the result back
    /// atomically — none of which `append_block` does.
    #[test]
    fn declaring_a_plugin_appends_the_archives_own_block_and_leaves_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));
        let before = std::fs::read_to_string(&worker.manifest).unwrap();

        worker
            .write_declaration("mpd", "[[plugin]]\nname = \"mpd\"\nexec = \"/opt/mpd\"\n")
            .unwrap();

        let after = std::fs::read_to_string(&worker.manifest).unwrap();
        assert!(after.starts_with(&before), "the entries already there are untouched:\n{after}");
        let names = crate::plugins::edit::names_in_order(&after).unwrap();
        assert_eq!(names, vec!["radio".to_string(), "mpd".to_string()], "the new one lands last");
        // A second time is refused rather than duplicated: `plugins.toml` with
        // two blocks of one name is a file the core cannot act on.
        assert!(worker
            .write_declaration("mpd", "[[plugin]]\nname = \"mpd\"\nexec = \"/opt/mpd\"\n")
            .is_err());
        // And no temporary is left beside it: this is a device one unplugs.
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A plugin the file does not declare is a fresh install; one it declares
    /// is a replacement. This is the predicate the whole gesture branches on,
    /// and it reads the file rather than the row the page last showed.
    #[test]
    fn what_the_file_declares_is_what_says_install_from_replace() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));
        assert!(worker.declared("radio"));
        assert!(!worker.declared("mpd"));
        // An unreadable manifest answers "not declared", the same answer
        // `enabled` gives there, and the append that follows fails loudly.
        std::fs::remove_file(&worker.manifest).unwrap();
        assert!(!worker.declared("radio"));
    }
}
