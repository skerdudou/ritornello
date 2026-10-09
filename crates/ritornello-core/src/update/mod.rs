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
pub mod catalogue;
pub mod download;
pub mod state;
pub mod schedule;
pub mod placed;
pub mod install_registry;
pub mod sources;

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
    differs, download_name, fold, newest_catalogue_url, origin, parse_checksums, parse_releases,
    releases_url, Channel, Offer, Origin, Published, Release, ReleasesError, ARCH,
    REPO,
};
use crate::compat::Speaks;
use crate::update::state::{
    component_offers, installs_something, judge_contracts, Availability, CheckOutcome,
    ComponentKind, ComponentOffer, Fit, Installed, NotInstallable, ThirdPartyOffer, UpdateState,
};
use ritornello_i18n::Chain;
use ritornello_updater::request::{Action, Request, REQUEST_FORMAT};
use ritornello_updater::target::plugins_dir;
use std::collections::BTreeMap;
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
    /// plugin or a language pack's id.
    Install {
        names: Vec<String>,
        /// `(name, owner/repo)` for each plugin the operator confirmed
        /// installing fresh from a third-party source: the repository the
        /// second consent named (`Worker::install_consented`).
        consented: Vec<(String, String)>,
    },
    /// The scheduler's own job: a check, and — when the policy installs —
    /// the installs that check turns out to make due.
    ///
    /// **One job and not two**, and that is the whole reason it exists: what
    /// an automatic run must install is only known once the check has
    /// answered. A ticker that enqueued `Check` and then `Install` would have
    /// to build the list from the *previous* check, and would install a day
    /// late — or, on the first run of a fresh device, install nothing at all.
    Scheduled {
        /// `UpdatePolicy::install_scope`, decided by the ticker that has the
        /// settings in hand rather than read again here: `None` for a policy
        /// that only checks, otherwise how far the installs may reach.
        install: Option<schedule::InstallScope>,
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
    /// A language pack, by the language it carries. Its own variants rather
    /// than a name squeezed into `Install`: a pack takes a different path
    /// end to end -- its own reader, no privileged step, no plugins.toml
    /// block -- and sharing a variant would make the worker re-derive which
    /// kind it was holding.
    ///
    /// Constructed by `POST /api/languages/{language}` (`update::routes::
    /// language_install_post`, task 9).
    InstallLanguage(String),
    /// Constructed by `DELETE /api/languages/{language}` (`update::routes::
    /// language_remove_delete`, task 9); this task's own test also drives it
    /// through the real loop
    /// (`job_remove_language_reaches_remove_language_through_the_worker_loop`).
    RemoveLanguage(String),
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
        // A pack's row is real (judged the same way every other component
        // is), but placing one on disk takes no privileged step and no
        // staging area -- `install` routes a name that is the id of a pack
        // this check offers (`Checked::packs`, ours or a source's) into
        // `install_pack` *before* it ever reaches `resolve`/`carries` (fix
        // round 2, F1). `false` stays the answer here regardless, and that
        // is still deliberate: this function must never let a pack's row be
        // placed through the plugin/core path, which is wrong for something
        // with no binary, so a name that reaches `resolve` at all -- because
        // no offered pack carries that id -- correctly falls through to
        // `Resolved::Nothing` rather than being matched here.
        Offer::LanguagePack(_) => false,
        // Never installed component by component, and `download_name` already
        // answers `None` for it.
        Offer::Bundle => false,
        // Only `ritornello-install` places a companion: a name that reached
        // here as one must resolve to nothing rather than to its archive.
        Offer::Companion(_) => false,
    }
}

/// What an automatic run is allowed to install, out of what the check found.
///
/// Four exclusions, and each answers a decision of the specification rather
/// than a convenience:
///
/// - a **third-party** component (any row carrying a `third_party_repo`,
///   whatever its kind) is touched only when the policy says so
///   (`InstallScope::IncludingThirdParty`, the fourth policy): its repository
///   is not ours to judge unless the operator chose to trust it that far;
/// - a plugin the device does not have is never *added* by itself — choosing
///   what is installed stays the operator's decision;
/// - a component already known to need a manual step is not attempted again
///   every night, which would download the same archive daily to refuse it
///   for the same reason. A privileged plugin the device **has** is not
///   marked in advance: its update is allowed while its companion does not
///   move (`companion_allows`), so it is tried once per offered version —
///   refused before any download when the companion moved — and
///   `remember_manual_step` marks it then. One the device does not have is
///   marked from its name alone (`deny_privileged_install`) and never tried:
///   adding it is `ritornello-install`'s job;
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
///
///   **A language pack never reaches this clause with anything to compare.**
///   `remember_placed`/`placed::record` has exactly one production caller,
///   `install_one`, and `install_pack` is not it (fix round 2, F1's own
///   review of this function): a pack is never written into `placed.json`,
///   so `placed::version_of(placed, &c.name)` answers `None` for every pack
///   row, always. That makes this clause vacuously true for a pack whenever
///   `c.offered` is `Some` — which it is for every row this filter chain
///   reaches, `UpdateAvailable` meaning exactly that — so it never excludes
///   a pack that would otherwise qualify. What bounds a refused pack instead
///   is the third exclusion: `install_pack` marks a pack the reader refused
///   (`Refusal::Pack`) `installable: Some(false)`, and `carry_installable`
///   keeps that mark for as long as the same version is offered — so a
///   lying archive is downloaded once per offered version, not every night.
///   A pack refused for another reason (no room, a download that failed) is
///   not marked, and is tried again, as a plugin would be.
///
/// - **a break is never installed unattended** (the owner's rule): when the
///   offered core breaks the wire against the running one
///   (`RowContracts::breaking`), the core is left out, and so is every plugin
///   the **running** core refuses (`with_running_core` refused). Such a
///   plugin may only travel in the same request as the core
///   (`Worker::install_group`): installed alone tonight, it would be refused
///   by the core that keeps running until someone presses Install.
///
///   A plugin the running core accepts **stays in**, even when the offered
///   core would refuse it: the core is not installed at night, so that
///   plugin runs under the running core, which accepts it — and the manual
///   gesture that later installs the core updates it again. The page says a
///   major update waits (`UpdateState::major_update_waiting`).
fn automatic_install_list(
    components: &[ComponentOffer],
    placed: &placed::Placed,
    scope: schedule::InstallScope,
) -> Vec<String> {
    let breaking = core_breaks(components);
    components
        .iter()
        .filter(|c| !(breaking && (c.kind == ComponentKind::Core || refused_by_running_core(c))))
        .filter(|c| c.availability == Availability::UpdateAvailable)
        // Keyed on the repository and not on the kind, so a third-party
        // language pack (Task 7) follows the same rule as a third-party
        // plugin without a second arm to forget.
        .filter(|c| c.third_party_repo.is_none() || scope == schedule::InstallScope::IncludingThirdParty)
        .filter(|c| c.installable != Some(false))
        .filter(|c| c.installed.is_some() || placed.contains_key(&placed_key(c)))
        .filter(|c| c.offered.as_deref() != placed::version_of(placed, &placed_key(c)))
        .map(|c| c.name.clone())
        .collect()
}

/// Does the core row offer an update that breaks the wire against the running
/// core? `judge_contracts` sets `breaking` only on a core row that has an
/// update and whose contracts are published.
fn core_breaks(components: &[ComponentOffer]) -> bool {
    components
        .iter()
        .any(|c| c.kind == ComponentKind::Core && c.contracts.breaking && c.offered.is_some())
}

/// Is this row a plugin the **running** core would refuse — one that can only
/// be installed in the same request as the core it needs?
fn refused_by_running_core(row: &ComponentOffer) -> bool {
    matches!(row.contracts.with_running_core, Some(Fit::Refused { .. }))
}

/// The key a component's placement is remembered under: its name for ours, a
/// namespaced key for any row that carries a repository. A stranger's name may
/// collide with ours, and `:` never appears in one of ours.
fn placed_key(c: &ComponentOffer) -> String {
    match &c.third_party_repo {
        Some(repo) => third_party_placed_key(repo, &c.name),
        None => c.name.clone(),
    }
}

/// The key under which a third-party component's placement is remembered.
///
/// The repository is lowercased here, once, for every writer and reader: a
/// fresh install remembers its source as `sources::fresh_offers` names it
/// (lowercased), and that plugin's later updates read the memory under the
/// repository as it announces it, whose case is its author's choice.
pub(crate) fn third_party_placed_key(repo: &str, name: &str) -> String {
    format!("third-party:{}:{name}", repo.to_lowercase())
}

/// What each installed third-party plugin's **own** repository offers for it,
/// out of the answers one sweep collected (`sources::query_sources`).
///
/// The rule the per-plugin requests used to apply, now applied to answers
/// that may serve several plugins at once:
///
/// - **only the repository the plugin itself announced answers for it.** A
///   plugin named `zed` gets nothing from a repository the operator added,
///   even one that publishes a `zed` archive: the announcement is the
///   plugin's own claim about where it comes from, and an added source is
///   not a claim about any installed binary;
/// - **the asset must be named for this plugin.** A repository publishing an
///   archive under another name has published nothing for the binary on this
///   device — and this is also what stops a stranger's release from ever
///   producing an `Offer::Core`, which downstream would be a component the
///   plugin rule never judges.
///
/// A plugin whose repository did not answer usably, or whose answer carries
/// nothing named for it, yields no offer: its row stays `Unknown`, which
/// says "nothing is known" and never "up to date".
///
/// `repo` is the repository **as `origin` reads it**, case kept, because it
/// is the same value the row carries as `third_party_repo` and so the same
/// key `placed_key` reads the placement memory under. The answer is found by
/// its lowercased form, which is how a `SourceTarget` names it.
fn theirs_from(installed: &[Installed], answers: &[sources::SourceAnswer]) -> Vec<ThirdPartyOffer> {
    installed
        .iter()
        .filter_map(|p| {
            let Origin::ThirdParty(repo) = origin(p.repository.as_deref()) else { return None };
            let key = repo.to_lowercase();
            let answer = answers.iter().find(|a| a.repo == key)?;
            let Some(published) = answer
                .published
                .iter()
                .find(|o| matches!(&o.offer, Offer::Plugin(n) if *n == p.name))
            else {
                tracing::info!("update: {repo} publishes no {ARCH} archive named for {}", p.name);
                return None;
            };
            Some(ThirdPartyOffer { name: p.name.clone(), published: published.clone(), repo })
        })
        .collect()
}

/// Every component the announcement says is third-party — **no cap, and
/// `Foreign` included**, unlike `sources::source_targets`.
///
/// Two lists out of one reading, and the difference between them is the point:
/// `source_targets` answers "whom do we go and ask", which is bounded and
/// only covers repositories we can address; this one answers "what is a
/// stranger", which admits no ceiling at all. A component left out of the
/// sources this check consulted, or announcing a repository we cannot address,
/// is not thereby one of ours — and the fall-through that treated it as one is
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
    /// fetched. `repo` is the repository that answered, as the row names it:
    /// the key its placement is remembered under.
    Theirs { published: &'a Published, repo: &'a str },
    /// The announcement says it is a stranger's, and this check did not get an
    /// answer from its repository — past the sources limit, unreachable,
    /// unaddressable, or publishing no archive for this architecture under
    /// this name.
    ///
    /// **Never our release of the same name**, and that is the whole reason
    /// this variant exists rather than falling through: a third-party plugin
    /// keeping its fork's name (`radio`, say) would otherwise be silently
    /// replaced by the official `radio` archive, installed under the plugin
    /// rule with everything that rule allows into `/etc/ritornello`.
    UncheckedThirdParty,
    /// A plugin nobody on this device owns, which exactly one source offers
    /// (`sources::fresh_offers`). `repo` is that source, lowercased: the key
    /// its placement will be remembered under.
    FreshTheirs { published: &'a Published, repo: &'a str },
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
        return Resolved::Theirs { published: &offer.published, repo: &offer.repo };
    }
    // Before `ours` and not after it: the membership test is what keeps a
    // stranger's row from ever being served from our release.
    if checked.third_party.iter().any(|n| n == name) {
        return Resolved::UncheckedThirdParty;
    }
    // After the unchecked guard: an installed stranger whose own repository
    // did not answer must never be answered by another stranger publishing
    // its name. Before `ours`: a fresh name cannot be ours by construction
    // (`sources::fresh_offers`, clause 1), but which list answers must not
    // rest on that — so the order states it rather than relying on it.
    if let Some(offer) = checked.fresh.iter().find(|o| o.name == name) {
        return Resolved::FreshTheirs { published: &offer.published, repo: &offer.repo };
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
///   input presets, examples and the `[[plugin]]` block that the core itself
///   writes;
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

/// Whether a plugin that ships with a companion (`plugins::COMPANIONS`) may
/// be updated from the UI: only when the version the release offers for the
/// companion is **the very one** `ritornello-install` recorded placing.
///
/// The plugin's own archive carries nothing root owns any more — the helper,
/// its unit and its rule are the companion's — so its update is an ordinary
/// `PlacePlugin`. What that placement must never do is put a new plugin
/// beside a companion it no longer matches: the two are built from one
/// shared crate (`ritornello-files-mount`), and the coupled-change guard of
/// `scripts/changed-components.sh` moves both only on a change to that
/// shared library. A companion has a version of its own, independent of the
/// product's: an unchanged helper keeps its number and the plugin updates
/// from the page. A companion version that moved means "this update also
/// changes the companion", which only `ritornello-install` can place, and the
/// plugin waits for it.
///
/// **Equality, never order**, like every version comparison on a device.
/// And every unknown refuses: a release that carries no companion (dropped
/// out of the hundred-release window), a registry that is absent, unreadable
/// or silent about the companion (a device deployed by `deploy.sh`) — none
/// of them proves the two stay in step, and the answer is to send the
/// operator to the program that can place both.
fn companion_allows(offered: Option<&str>, installed: Option<&str>) -> bool {
    match (offered, installed) {
        (Some(offered), Some(installed)) => offered == installed,
        _ => false,
    }
}

/// The version our release offers for `plugin`'s companion, when `plugin`
/// has one and the release carries it. Read off the same fold as every
/// other component (`Offer::Companion`), so no second request is made.
fn companion_offered<'a>(ours: &'a [Published], plugin: &str) -> Option<&'a str> {
    let companion = crate::plugins::companion_of(plugin)?;
    ours.iter()
        .find(|p| matches!(&p.offer, Offer::Companion(c) if c == companion))
        .map(|p| p.version.as_str())
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
///
/// **A companion's refusal is not carried.** A row refused because its
/// companion moved (`needs_companion` set, by `deny_moved_companion` or by
/// `install_one`'s own backstop) is decided afresh at every check from the
/// release and the registry, both of which can change within one offered
/// version — `ritornello-install` run in between, or a registry read that
/// failed once. Carrying that `Some(false)` would keep a row refused for a
/// fact that no longer holds. **Nor is a refusal for unpublished contracts**
/// (`judge_contracts`), for the same reason.
fn carry_installable(previous: &[ComponentOffer], fresh: &mut [ComponentOffer]) {
    // A contested name's `Some(false)` is a fact about this check's answers,
    // stated by `component_offers`, and no earlier answer may overwrite it —
    // on a device's first check `previous` is empty and would erase it.
    for row in fresh.iter_mut().filter(|r| r.conflict_repos.is_none()) {
        row.installable = previous
            .iter()
            .find(|p| p.name == row.name && p.offered == row.offered)
            .filter(|p| p.needs_companion.is_none())
            // Nor contracts that were unpublished: a catalogue that failed
            // to read once, or one republished since, is seen afresh by
            // `judge_contracts` at every check.
            .filter(|p| p.contracts.not_installable_reason.is_none())
            .and_then(|p| p.installable);
    }
}

/// Forces `installable: Some(false)` for every row naming a plugin
/// `plugins::PRIVILEGED_PLUGINS` lists **that `plugins.toml` does not
/// declare** — decided by the plugin's identity and the device's own file,
/// the two facts this check never has to fetch an archive to learn, unlike
/// everything else `installable` can carry.
///
/// **A declared one is left alone.** Installing a privileged plugin is
/// `ritornello-install`'s job, since its companion goes in with it, but
/// updating one is allowed from here while the companion does not move
/// (`companion_allows`) — a fact `install_one` settles, from the worker, at
/// the gesture. So the row keeps whatever `carry_installable` carried
/// (`None` until an attempt, `Some(false)` after a refused one, set by
/// `remember_manual_step`). `declared` and not `installed`: `install_one`
/// tells an update from an installation by the same `plugins.toml`
/// declaration, and a switched-off plugin, which announces no version, is
/// still an update.
///
/// **Called last**, after `carry_installable`, and not folded into it: that
/// function's whole job is carrying a *previous* answer forward, and on the
/// very first check a device ever runs `previous` is empty — folding this
/// rule into the same assignment would have `carry_installable` overwrite it
/// with `None` before anyone ever saw `Some(false)`. Calling this afterwards
/// means the privileged answer always wins for an undeclared row, on the
/// first check exactly as on the hundredth.
///
/// This is what lets `InstallablesDialog.vue` show its sentence instead of an
/// Install button for a **never-installed** privileged plugin. For a declared
/// one, the automatic policy may try the update once per offered version; a
/// companion that moved refuses it before anything is downloaded, and the
/// refusal is remembered for that version.
fn deny_privileged_install(components: &mut [ComponentOffer]) {
    for row in components.iter_mut() {
        if crate::plugins::is_privileged(&row.name) && !row.declared {
            row.installable = Some(false);
        }
    }
}

/// What `ritornello-install` recorded for each companion
/// (`plugins::COMPANIONS`): `(companion, installed version)`, `None` when
/// unknown. One small local read per companion, from the worker — `check`
/// and `conclude_install` call it before they take the state lock, and no
/// route ever does.
fn installed_companions(root: &Path) -> Vec<(&'static str, Option<String>)> {
    crate::plugins::COMPANIONS
        .iter()
        .map(|(_, companion)| (*companion, install_registry::companion_version(root, companion)))
        .collect()
}

/// Marks, **at the check**, every row the companion rule would refuse at
/// the gesture: `installable: Some(false)` and `needs_companion` naming the
/// companion, so the page says "this update also changes the mount helper:
/// update with ritornello-install" before anyone presses anything, and the
/// automatic policy never spends an attempt on it.
///
/// A row is marked when it is one of ours (`ComponentKind::Plugin`),
/// declared, ships with a companion, has something on offer
/// (`UpdateAvailable` or `BinaryMissing`) — and `companion_allows` refuses
/// the release's companion version against the installed one. An undeclared
/// one is `deny_privileged_install`'s: installing it is a different sentence.
///
/// Recomputed on every check and called after `carry_installable`, which
/// never carries a companion's refusal: a registry `ritornello-install`
/// has since updated, or a read that failed once, is seen afresh. It only
/// ever marks, never clears: a row it leaves alone keeps what the other
/// rules decided. `install_one` still asks the same question at the
/// gesture, since the registry can change between the check and the press.
fn deny_moved_companion(
    components: &mut [ComponentOffer],
    published: &[Published],
    installed: &[(&'static str, Option<String>)],
) {
    for row in components.iter_mut() {
        let Some(companion) = crate::plugins::companion_of(&row.name) else { continue };
        if row.kind != ComponentKind::Plugin
            || !row.declared
            || !matches!(row.availability, Availability::UpdateAvailable | Availability::BinaryMissing)
        {
            continue;
        }
        let installed_version =
            installed.iter().find(|(c, _)| *c == companion).and_then(|(_, v)| v.as_deref());
        if !companion_allows(companion_offered(published, &row.name), installed_version) {
            row.installable = Some(false);
            row.needs_companion = Some(companion.to_string());
        }
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
    /// A plugin that ships with a companion (`plugins::COMPANIONS`), which
    /// this page may not place: an update whose companion moved or cannot be
    /// shown not to have (`companion_allows`), or an installation, which
    /// places the companion too. Carries the companion's name.
    ///
    /// Its own variant and not `NeedsManualStep`, whose sentence explains an
    /// archive holding a unit, a rule or a root-run binary: the plugin's
    /// archive holds none of them, and that sentence would send the operator
    /// looking for something that is not there.
    NeedsCompanionStep(&'static str),
    /// A **third-party** archive carrying anything besides its own binary: a
    /// unit, a polkit rule, a nested path, an input preset, an initial
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
    /// A third-party plugin installed from scratch under a name it may not
    /// take, or whose binary is not named for it: the core writes its
    /// `[[plugin]]` block itself (`third_party_fragment`), and refuses to
    /// write one that would declare a binary named for someone else, a name
    /// reserved for ours, or a name taken on this device since the check.
    /// Carries which rule refused.
    NotItsOwnName(String),
    /// A fresh offer whose binary is already in the plugins directory with
    /// nothing declaring it — most likely the leftover of an earlier attempt
    /// whose declaration failed. Its own sentence, because the operator can
    /// act on it: the plugins page lists that file as installed but not
    /// declared, and removes it. Carries the file's path.
    LeftoverBinary(String),
    /// A third-party component whose own repository could not be consulted by
    /// this check — past the sources limit, unreachable, unaddressable, or
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
    /// A language pack archive the pack reader turned down, or a pack whose
    /// manifest names a different language than the one it was installed
    /// under, or a source other than the repository it was fetched from.
    /// Only ever built by `place_pack`'s own refusals: a
    /// removal that fails does not go through this enum at all —
    /// `remove_language` builds its catalog message directly from
    /// `store::remove`'s own error, since it has no `name`/`why` pair to
    /// hand `refusal_message` outside an install pass. The detail is the
    /// reader's own sentence, naming which rule refused it.
    Pack(String),
    /// Nothing published carries this name at all: dropped out of the
    /// hundred-release window this check reads, or never one of ours.
    ///
    /// A refusal and not a silent skip, for the same reason `ThirdPartyUnchecked`
    /// is one (task 18's review, C1): the operator pressed a specific row's
    /// gesture, and "nothing happened" reads as the request never having
    /// reached the server, not as the honest "there is nothing to install"
    /// it actually means.
    NothingPublished,
    /// `plugins.toml` could not be read by the check this install rests on
    /// (`Checked::plugins_unknown`). Whose plugin a name is cannot be told
    /// then — an installed fork named like ours would look like ours — so
    /// nothing but a language pack or the core itself is installed until the file reads again.
    PluginsUnreadable,
    /// A plugin offered fresh by a third-party source, installed without the
    /// operator having confirmed **that** source for it: no repository named
    /// in the request, or another one than the source offering it now
    /// (spec §4.5, the second consent). Carries the source that offers it.
    NotConsented(String),
    /// The release carrying this component's archive publishes no contracts
    /// for it (`NotInstallable::ContractsUnpublished`): the device cannot know
    /// what it would run, so it is not installed from here, by hand either.
    /// Refused at the gesture before anything is fetched.
    ContractsUnpublished,
    /// A breaking core and the plugins that depend on it travel in one
    /// request (`Worker::install_group`), and `failed` could not be prepared:
    /// no request was written, so neither the core nor any of those plugins
    /// moved. `reason` is that member's own refusal, for the log.
    GroupPostponed { failed: String, reason: Box<Refusal> },
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
            Self::NeedsCompanionStep(companion) => {
                write!(f, "it ships with {companion}, which only ritornello-install places")
            }
            Self::ThirdPartyArchive => {
                write!(f, "a third-party archive may carry nothing but its own binary")
            }
            Self::NotItsOwnFile(d) | Self::NotItsOwnName(d) => write!(f, "{d}"),
            Self::LeftoverBinary(path) => write!(f, "{path} already exists and nothing declares it"),
            Self::ThirdPartyUnchecked => {
                write!(f, "its own repository was not consulted by this check")
            }
            Self::NoFragment => write!(f, "the archive carries no plugins.toml block"),
            Self::Download(d) | Self::Prepare(d) | Self::Privileged(d) | Self::Pack(d) => {
                write!(f, "{d}")
            }
            Self::NothingPublished => write!(f, "nothing published carries this name"),
            Self::PluginsUnreadable => write!(f, "plugins.toml could not be read, so whose plugin this is is unknown"),
            Self::NotConsented(repo) => {
                write!(f, "offered by {repo}, which is not the repository confirmed for it")
            }
            Self::ContractsUnpublished => {
                write!(f, "the release carrying it publishes no contracts for it")
            }
            Self::GroupPostponed { failed, reason } => write!(
                f,
                "{failed} could not be prepared ({reason}); the core and the plugins that depend on it were not installed"
            ),
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
fn refusal_message(catalog: &Chain, component: &str, why: &Refusal) -> String {
    let (key, param): (&str, Option<(&str, &str)>) = match why {
        Refusal::NoRoom => ("update_no_room", None),
        Refusal::NoDigest => ("update_no_digest", None),
        Refusal::DigestMismatch => ("update_digest_mismatch", None),
        Refusal::NeedsManualStep => ("update_needs_manual_step", None),
        Refusal::NeedsCompanionStep(c) => ("update_needs_companion_step", Some(("companion", *c))),
        Refusal::ThirdPartyArchive => ("update_third_party_archive", None),
        Refusal::NotItsOwnFile(d) => ("update_wrong_file", Some(("detail", d.as_str()))),
        Refusal::NotItsOwnName(d) => ("update_wrong_name", Some(("detail", d.as_str()))),
        Refusal::LeftoverBinary(path) => ("update_leftover_binary", Some(("path", path.as_str()))),
        Refusal::ThirdPartyUnchecked => ("update_third_party_unchecked", None),
        Refusal::NoFragment => ("update_no_fragment", None),
        Refusal::Download(d) => ("update_download_failed", Some(("detail", d.as_str()))),
        Refusal::Prepare(d) => ("update_install_failed", Some(("detail", d.as_str()))),
        Refusal::Privileged(d) => ("update_privileged_failed", Some(("detail", d.as_str()))),
        Refusal::Pack(d) => ("update_pack_refused", Some(("detail", d.as_str()))),
        Refusal::NothingPublished => ("update_nothing_published", None),
        Refusal::PluginsUnreadable => ("update_plugins_unreadable", None),
        Refusal::NotConsented(repo) => ("update_not_consented", Some(("repo", repo.as_str()))),
        // The same sentence the row already shows before any press.
        Refusal::ContractsUnpublished => ("update_row_contracts_unpublished", None),
        Refusal::GroupPostponed { .. } => ("update_group_postponed", None),
    };
    // A postponed group names the member that failed, whichever name the
    // caller reports it under: that member is what the operator can act on.
    let component = match why {
        Refusal::GroupPostponed { failed, .. } => failed.as_str(),
        _ => component,
    };
    let mut params: Vec<(&str, &str)> = vec![("component", component)];
    params.extend(param);
    ritornello_i18n::interpolate(catalog.get(key), params)
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

/// The `[[plugin]]` block the core writes for a third-party plugin it
/// installs from scratch (spec §4.3).
///
/// **Built by the core, never read from the archive.** A stranger's archive
/// may carry nothing but its own binary (`only_its_own_binary`), so it has no
/// block to offer — and a block it did offer would choose its own `exec`,
/// which is the one line of `plugins.toml` that says what the core runs. So
/// the block says exactly two things, both the core's: the name the source
/// offered, and the plugins directory root places the binary in, joined with
/// that binary's file.
///
/// Refused (`NotItsOwnName`) unless:
///
/// - `file` is **exactly** `ritornello-plugin-<name>`. Not
///   `plugins::component_name_from_file(file) == name`: that answers `zed`
///   for a bare file `zed` too (preflight ruling P2). Exact equality is also
///   what keeps the row and the undeclared-binary scan, which names a binary
///   by that function, agreeing on whose binary it is;
/// - `file` is a valid bare name — it is what root joins onto the plugins
///   directory, and a name near the length limit makes a file past it;
/// - `name` is a valid bare name and not `sources::reserved`. `reserved`
///   holds `!valid_name` itself; the first check is stated here anyway, so
///   this rule does not rest on what another function chooses to reserve.
fn third_party_fragment(name: &str, file: &str, plugins_dir: &Path) -> Result<String, Refusal> {
    use ritornello_updater::request::valid_name;
    let own = format!("ritornello-plugin-{name}");
    if file != own {
        return Err(Refusal::NotItsOwnName(format!("the archive carries {file}, not {own}")));
    }
    if !valid_name(file) {
        return Err(Refusal::NotItsOwnName(format!("{file} is not a valid file name")));
    }
    if !valid_name(name) {
        return Err(Refusal::NotItsOwnName(format!("{name} is not a valid plugin name")));
    }
    if sources::reserved(name) {
        return Err(Refusal::NotItsOwnName(format!("{name} is reserved")));
    }
    let exec = plugins_dir.join(file);
    let exec = exec
        .to_str()
        .ok_or_else(|| Refusal::Prepare(format!("{} is not UTF-8", exec.display())))?;
    // Through `toml_edit` rather than `format!`: the plugins directory is
    // the device's, and quoting it is the TOML writer's job.
    let mut block = toml_edit::Table::new();
    block["name"] = toml_edit::value(name);
    block["exec"] = toml_edit::value(exec);
    let mut blocks = toml_edit::ArrayOfTables::new();
    blocks.push(block);
    let mut doc = toml_edit::DocumentMut::new();
    doc["plugin"] = toml_edit::Item::ArrayOfTables(blocks);
    Ok(doc.to_string())
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
/// — `<plugin's data directory>/<dir>/<file>` is a shape nothing packs and the
/// core has no reason to create — and a dotted one, which would let an archive
/// name the very temporary `write_atomic` writes beside its target.
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
/// not gain a dependant. This one names the file rather than its extension, so
/// it works for an input preset and a language pack file alike — and,
/// `pub(crate)` within this binary, also for `langpack::store`'s pack files
/// and manifest, which is why there is still no fourth copy.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
    catalog: &Chain,
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
        ritornello_i18n::interpolate(
            catalog.get("update_installed_new"),
            [("component", last.component.as_str())],
        )
    } else {
        ritornello_i18n::interpolate(
            catalog.get("update_installed"),
            [("component", last.component.as_str()), ("version", last.version.as_str())],
        )
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
    // Every `request.json` written for the privileged unit on this thread, in
    // order: `request.json` itself is overwritten by the next request, and the
    // grouped install's whole property is the **sequence** — the compatible
    // plugins one by one, then one request ending with the core.
    static SEEN_REQUESTS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
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

/// Where the archive `install_one` is handed comes from, as `resolve` decided.
///
/// Three values and not a `third_party: bool`, because two third-party cases
/// are not one: **only a fresh offer** (`Resolved::FreshTheirs`) may be
/// declared by the core. A `Theirs` update whose block is missing — removed
/// by the operator since the check, or unreadable — is refused before any
/// write, as it always was (`declaration_needed`): re-declaring it would undo
/// the operator's own gesture, and under the automatic policy would be a
/// first declaration nobody consented to (spec §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// Our own release.
    Ours,
    /// An installed third-party plugin's own repository.
    Theirs,
    /// A source offering a plugin nobody on this device owns.
    Fresh,
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

/// One component made ready by `Worker::stage`: its binary is in the staging
/// directory and its `/etc/ritornello` files are written, and root has been
/// asked nothing yet. What `Worker::place` needs to put it in a request and
/// to remember it once placed.
struct Staged {
    name: String,
    version: String,
    /// Remembered under a namespaced key (`third_party_placed_key`).
    third_party: bool,
    repo: Option<String>,
    action: Action,
    /// The bare name of the staged file, removed once placed.
    staged_file: String,
    /// The `[[plugin]]` block to write once the binary is placed: `Some` only
    /// for a plugin the device did not have.
    fragment: Option<String>,
    /// For the core: what its archive carries that nothing here installs.
    core_notes: Option<Vec<String>>,
}

impl Staged {
    fn is_core(&self) -> bool {
        matches!(self.action, Action::PlaceCore { .. })
    }
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
    /// What each third-party plugin's own repository publishes for it, derived
    /// from `sources` by `theirs_from`.
    theirs: Vec<ThirdPartyOffer>,
    /// Every component the announcements call a stranger's, uncapped — see
    /// `third_party_names`. Carried beside `theirs` because a name absent from
    /// `theirs` is not thereby one of ours: it may simply be past the sources
    /// limit, or one whose server did not answer.
    third_party: Vec<String>,
    /// Every source that answered this check usably, in the order it was
    /// asked (`sources::query_sources`).
    sources: Vec<sources::SourceAnswer>,
    /// The plugins nobody on this device owns that exactly one source offers
    /// (`sources::fresh_offers`). Empty whenever our own release list gave no
    /// fold to judge ownership against — see `Checked::judge_strangers`.
    fresh: Vec<sources::FreshOffer>,
    /// The names nobody owns that several sources offer: none is believed.
    conflicts: Vec<sources::Conflict>,
    /// Every language pack on offer, ours and each source's, each under its
    /// own id (`sources::pack_offers`). Unlike `fresh`, made whether or not
    /// our release list was read: a pack's id carries its source, so no
    /// ownership has to be judged for it.
    packs: Vec<sources::PackOffer>,
    /// `plugins.toml` could not be read by this check, so what the device
    /// has — and so whose plugin a name is — is not known. `third_party`
    /// and `theirs` are then built from an empty list, which says nothing
    /// is a stranger's: a fork named like ours would resolve to `Ours` and
    /// have its binary replaced. `install` therefore refuses every install
    /// but a language pack's and the core's own while this is set (`Refusal::PluginsUnreadable`).
    plugins_unknown: bool,
    /// What the catalogues this check read say each shipped component
    /// speaks, keyed by catalogue URL (`fetch_contracts`). Only the ones read:
    /// a URL absent here was not needed or failed, and either way no row it
    /// carries is vouched for (`judge_rows`).
    contracts: ContractsByUrl,
}

/// Catalogue URL -> component name -> what that component's archive speaks.
type ContractsByUrl = BTreeMap<String, BTreeMap<String, Speaks>>;

/// The catalogue that describes what `row` offers: the one of **the release
/// carrying its archive** (`Published::catalogue_url`), never the newest —
/// a component not republished has no contracts in a newer catalogue, and an
/// older archive cannot be described with a newer tree's numbers. `None` for
/// a pack, a row offering nothing, or one whose offer resolves nowhere.
fn row_catalogue<'a>(checked: &'a Checked, row: &ComponentOffer) -> Option<&'a str> {
    if row.kind == ComponentKind::LanguagePack || row.offered.is_none() {
        return None;
    }
    match resolve(checked, &row.name) {
        Resolved::Ours(published)
        | Resolved::Theirs { published, .. }
        | Resolved::FreshTheirs { published, .. } => published.catalogue_url.as_deref(),
        Resolved::UncheckedThirdParty | Resolved::Nothing => None,
    }
}

/// The distinct catalogues a check must read: those carrying an archive a
/// row would install. Each is fetched once however many rows it describes.
fn contract_urls(checked: &Checked, rows: &[ComponentOffer]) -> Vec<String> {
    let mut urls: Vec<String> = rows
        .iter()
        .filter(|r| installs_something(r))
        .filter_map(|r| row_catalogue(checked, r).map(str::to_string))
        .collect();
    urls.sort();
    urls.dedup();
    urls
}

/// What each row's offered archive speaks, by row name, from the catalogue
/// that carries it (`row_catalogue`). A row missing here has no published
/// contracts.
fn offered_speaks(checked: &Checked, rows: &[ComponentOffer]) -> BTreeMap<String, Speaks> {
    rows.iter()
        .filter_map(|row| {
            let speaks = checked.contracts.get(row_catalogue(checked, row)?)?.get(&row.name)?;
            Some((row.name.clone(), speaks.clone()))
        })
        .collect()
}

/// The last rule on a row, after every other one: what its archive speaks
/// and what that means against the core it will meet (`judge_contracts`).
/// One call shared by the check and by `conclude_install`, so a row rebuilt
/// after an install says what the check said.
fn judge_rows(rows: &mut [ComponentOffer], checked: &Checked, live: &[(String, Speaks)]) {
    let offered = offered_speaks(checked, rows);
    judge_contracts(rows, &offered, &Speaks::this_core(), live);
}

/// Reads every catalogue in `urls` at once, each bounded by the download
/// client's limits and by the sources' deadline, so a hanging server holds
/// the check no longer than a hanging source would. A catalogue that fails
/// is simply absent from the answer — the rows it carries then say their
/// contracts are unpublished, and the check itself goes on.
async fn fetch_contracts(client: &reqwest::Client, urls: Vec<String>) -> ContractsByUrl {
    let reads = urls.into_iter().map(|url| async move {
        let read = tokio::time::timeout(sources::SOURCES_DEADLINE, catalogue::fetch(client, &url))
            .await
            .ok()
            .flatten();
        if read.is_none() {
            tracing::warn!("update: {url} could not be read; what it carries cannot be installed from the device");
        }
        read.map(|c| (url, c.contracts))
    });
    futures::future::join_all(reads).await.into_iter().flatten().collect()
}

impl Checked {
    /// Decides which of the sources' plugins may be offered fresh, against
    /// **this** check's own fold of our release (spec §4.1).
    ///
    /// Only ever called with a fold that was actually read. When our release
    /// list answered nothing usable for this device (`NoRelease`,
    /// `OnlyPrereleases`), `ours` is empty because nothing is known, not
    /// because nothing is ours: judged against it, a stranger publishing
    /// `radio` would look like the only owner of a name that is ours. So
    /// that branch never calls this, and offers nothing fresh.
    fn judge_strangers(&mut self, installed: &[Installed]) {
        (self.fresh, self.conflicts) = sources::fresh_offers(&self.sources, &self.ours, installed);
    }
}

/// Every pack offered for one language — ours and each source's — out of an
/// already performed check (spec §5.2: one gesture installs them all).
///
/// **Not a second network round trip.** `checked.packs` is built from
/// `check()`'s own fold of our release list and from the answers of the same
/// sweep (`sources::pack_offers`), where a language pack sits as
/// `Offer::LanguagePack` exactly like the core or a plugin sits as
/// `Offer::Core`/`Offer::Plugin` — see `release::fold`, `release::
/// classify_asset`. `install_language` is handed the same `checked` the job
/// loop already produced for this run (`run_worker`'s `Job::InstallLanguage`
/// arm), the same way `install`/`install_one` are handed it for the core and
/// for a plugin, rather than asking GitHub a second time for one component.
///
/// `check()`'s own network call has a test seam since task 14
/// (`release::TEST_RELEASES_URL_ENV`, compiled only under
/// `#[cfg(debug_assertions)]`); see that constant's own doc for what it
/// covers and does not.
fn offered_packs<'a>(checked: &'a Checked, language: &str) -> Vec<&'a sources::PackOffer> {
    // Without case: `pt-BR` from us and `pt-br` from a source are one
    // language, so one gesture (BCP 47 tags compare case-insensitively).
    checked.packs.iter().filter(|p| p.language.eq_ignore_ascii_case(language)).collect()
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
    pub catalog: Arc<RwLock<Chain>>,
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
    /// Where every plugin's own data directory lives -- the same root
    /// `main` launches every plugin with (`RITORNELLO_PLUGIN_DATA_DIR` is
    /// this joined with the plugin's name, via `plugins::data_dir_for`), and
    /// it must be exactly that root: `write_initial_config` forms
    /// `<plugin_data_root>/<name>` to place a fresh install's initial
    /// configuration, and if the two ever disagreed the file would land
    /// where the plugin it is meant for never looks. Not to be confused with
    /// `packs_root`, a different root for a different kind of installed
    /// thing.
    pub plugin_data_root: PathBuf,
    /// This binary's own version, for the core's row.
    pub core_version: &'static str,
    /// How the core leaves once its binary has been replaced. The same hook
    /// the System tab's restart button uses, and for the same reason: mpv must
    /// die with it, and `std::process::exit` runs no `Drop`.
    pub restart: crate::system::RestartHook,
    /// The one process-wide i18n registry (`crate::i18n::Shared`'s doc):
    /// what this worker reads to learn which language packs are installed
    /// (`Registry::installed_packs`, for the row `component_offers` gives
    /// each one), and what `install_language`/`remove_language` resweep
    /// (`Registry::resweep_async`) once they have written or removed a
    /// pack's own directory under `packs_root`.
    pub registry: crate::i18n::Shared,
    /// Where an installed language pack's own directory lives -- its own
    /// root, separate from anything a component archive writes (see
    /// `i18n::registry`'s module doc): `install_language`/`remove_language`
    /// write only here, and `registry`'s own packs root must be exactly the
    /// same path, or a resweep would look for what this worker just wrote in
    /// the wrong place.
    pub packs_root: PathBuf,
    /// The same channel `PUT /api/locale` writes into, cloned rather than a
    /// second one of its own: `remove_language` puts the device back on
    /// English exactly as a person picking it from the interface would, and
    /// through the one door that persists the choice.
    pub locale_tx: mpsc::Sender<String>,
    /// The language currently in use -- the same handle `AppState.
    /// locale_current` is, cloned rather than a second cell. Read to decide
    /// whether a removal must also send the device back to English: two
    /// independent copies of "the language in use" could disagree about
    /// which language a removal is judged against.
    pub locale_current: Arc<RwLock<Option<String>>>,
    /// The repositories the operator added as update sources: the handle
    /// `AppState.update_sources` is. Read per check, like `settings`, so an
    /// addition is seen by the next one without a restart.
    pub update_sources: Arc<RwLock<Vec<String>>>,
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
    ///
    /// **Single-parameter only, by contract.** A caller that needs a second
    /// parameter must not chain a further `.replace()`/`interpolate()` call
    /// onto this method's result — that composition is exactly the
    /// chained-replace defect this crate spent task 10b removing, just
    /// split across two call sites instead of one (`removal_failed` did
    /// precisely this before that task). Reach for
    /// `ritornello_i18n::interpolate` directly with every parameter in one
    /// call instead.
    async fn message_for(&self, key: &str, component: &str) -> String {
        ritornello_i18n::interpolate(&self.message(key).await, [("component", component)])
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
        self.installed_when_settled_known().await.unwrap_or_default()
    }

    /// `installed_when_settled`, saying `None` when the list cannot be known:
    /// `plugins.toml` unreadable. Only the check reads this form, because it
    /// is the one place where "nothing installed" and "not known" lead to
    /// different answers (`settle_with_release`).
    async fn installed_when_settled_known(&self) -> Option<Vec<Installed>> {
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
    ///
    /// `None` when `plugins.toml` cannot be read. Most callers take that as an
    /// empty list (`installed_when_settled`): it keeps the core's own row
    /// honest and says nothing about plugins rather than something false. The
    /// check must not: to the ownership rule, an empty list says every name is
    /// free.
    async fn installed(&self) -> Option<Vec<Installed>> {
        let manifest = match PluginManifest::load(&self.manifest) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("update: reading {}: {e:#}", self.manifest.display());
                return None;
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
        Some(out)
    }

    /// What each running plugin announced it speaks, from its status lines:
    /// the truth about a binary no gesture replaces. One entry per plugin (a
    /// plugin with two kinds has two lines saying the same thing).
    async fn live_speaks(&self) -> Vec<(String, Speaks)> {
        let status = self.status.read().await;
        let mut out: Vec<(String, Speaks)> = Vec::new();
        for line in &status.plugins {
            if let Some(speaks) = &line.speaks
                && !out.iter().any(|(name, _)| *name == line.name)
            {
                out.push((line.name.clone(), speaks.clone()));
            }
        }
        out
    }

    /// The language packs this device already has, as `component_offers`
    /// wants them: `(pack id, installed version)`.
    ///
    /// Read from the shared registry rather than from a scan of this
    /// worker's own root: `registry` is the same handle `Registry::chain_for`
    /// resolves text through, and `Registry::installed_packs` already holds
    /// what the last sweep found in memory (see that method's own doc) — a
    /// second, independent directory read here could disagree with it.
    async fn installed_packs(&self) -> Vec<(String, String)> {
        self.registry
            .read()
            .await
            .installed_packs()
            .iter()
            .map(|p| (p.id.clone(), p.manifest.version.clone()))
            .collect()
    }

    /// Installs a language: **every** pack this check offers for it, ours and
    /// each source's, in one gesture (spec §5.2).
    ///
    /// One pack refused does not cancel the others: each is attempted, and the
    /// first refusal is what comes back, with the id of the pack it concerns —
    /// the page has one message field, and naming the wrong pack would send
    /// the operator after the wrong source.
    ///
    /// `checked` is `check()`'s own fold, read once by the caller (the job
    /// loop, exactly as for the core and for a plugin) rather than fetched a
    /// second time here — see `offered_packs`.
    async fn install_language(&self, checked: &Checked, language: &str) -> Result<(), (String, Refusal)> {
        let offers = offered_packs(checked, language);
        if offers.is_empty() {
            return Err((crate::langpack::store::pack_id(language), Refusal::NothingPublished));
        }
        let mut first: Option<(String, Refusal)> = None;
        for offer in offers {
            if let Err(why) = self.install_pack(offer).await {
                tracing::warn!("update: installing {}: {why}", offer.id);
                first.get_or_insert((offer.id.clone(), why));
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// One pack, and what a refusal by the pack reader leaves behind: the row
    /// marked `installable: Some(false)`, the way `remember_manual_step` marks
    /// a component, so the automatic policy does not fetch the same refused
    /// archive every night (`carry_installable` keeps the mark while the same
    /// version is offered, and drops it for a new one).
    ///
    /// Only `Refusal::Pack` — a fact about the archive itself — is remembered.
    /// No room, a failed download or a digest that did not match say nothing
    /// lasting about this version, and are tried again.
    async fn install_pack(&self, offer: &sources::PackOffer) -> Result<(), Refusal> {
        let result = self.place_pack(offer).await;
        if matches!(result, Err(Refusal::Pack(_))) {
            self.remember_manual_step(&offer.id).await;
        }
        result
    }

    /// Places one language pack. **The privileged installer is not involved,
    /// and that is the design rather than an optimisation.**
    ///
    /// A pack carries no binary, so there is nothing for root to place:
    /// `target.rs` keeps its two path shapes, no `Action` is formed, and
    /// nothing is written into the staging directory. What the core does
    /// here it does with its own, unprivileged hands, into a root it alone
    /// chooses — the archive never names a destination: the directory is the
    /// offer's id, formed from who answered (`sources::pack_offers`).
    ///
    /// The order is `install_one`'s, deliberately: room, then digest, then
    /// read, then the manifest's two checks, then write. A refusal at any step
    /// before the write has written nothing.
    async fn place_pack(&self, offer: &sources::PackOffer) -> Result<(), Refusal> {
        let id = offer.id.as_str();
        let language = offer.language.as_str();
        let offered = &offer.published;
        let root = self.root.to_string_lossy().to_string();
        if !enough_room(crate::system::disk_usage(&root), offered.size as usize) {
            return Err(Refusal::NoRoom);
        }
        let client = download::client().map_err(|e| Refusal::Download(e.to_string()))?;
        let Some(checksums_url) = &offered.checksums_url else {
            return Err(Refusal::NoDigest);
        };
        let bytes = fetch_capped(&client, &offered.url, COMPRESSED_MAX)
            .await
            .map_err(|e| Refusal::Download(format!("the archive of {id}: {e}")))?;
        let (status, sums_body) = fetch_text(&client, checksums_url)
            .await
            .map_err(|e| Refusal::Download(format!("the checksums of {id}: {e}")))?;
        if status != 200 {
            return Err(Refusal::NoDigest);
        }
        let sums = parse_checksums(&sums_body);
        let file = asset_name(&offered.url);
        if let Err(e) = verify_digest(file, sums.get(file).map(String::as_str), &digest_hex(&bytes))
        {
            tracing::warn!("update: {id}: {e}");
            return Err(match e {
                DownloadError::Digest { .. } => Refusal::DigestMismatch,
                _ => Refusal::NoDigest,
            });
        }
        // The pack's OWN reader, never the component one. Everything a pack
        // may carry, and every refusal, lives there.
        let contents = crate::langpack::archive::read(&bytes, ritornello_i18n::MAX_BYTES)
            .map_err(|e| Refusal::Pack(e.to_string()))?;
        // The manifest must agree with what we asked for. An archive that
        // says "de" under the French pack's name would otherwise install
        // German files into the French pack's directory.
        if contents.manifest.language != language {
            return Err(Refusal::Pack(format!(
                "the archive of {id} declares the language {:?}",
                contents.manifest.language
            )));
        }
        // And the source it names must be the repository it was fetched from
        // (spec §4.2) — ours included, since `scripts/package-release.sh`
        // writes the official URL into every pack we publish. Without this, a
        // stranger's pack claiming our repository would pass for ours the day
        // it is moved, and `inventory` would list it under the wrong id or not
        // at all. Compared lowercased: GitHub does. A `source` that does not
        // read as a repository is refused, **never taken for ours**: `None`
        // matches nothing here.
        let expected = offer.repo.as_deref().unwrap_or(release::REPO).to_lowercase();
        let declared = release::parse_repo_url(&contents.manifest.source).map(|r| r.to_lowercase());
        if declared.as_deref() != Some(expected.as_str()) {
            return Err(Refusal::Pack(format!(
                "the archive of {id} names the source {:?}, and it was published by {expected}",
                contents.manifest.source
            )));
        }
        crate::langpack::store::install(&self.packs_root, id, &contents)
            .map_err(|e| Refusal::Prepare(format!("writing {id}: {e}")))?;
        crate::i18n::Registry::resweep_async(&self.registry).await;
        self.mark_pack_row(id, Some(contents.manifest.version.clone())).await;
        Ok(())
    }

    /// Patches this pack's own row in `state.components` right after a real
    /// install or removal.
    ///
    /// **Without this, the row a device just told to install never says
    /// so.** `language_pack_rows` (`status::locales`) reads `installed` off
    /// `ComponentOffer`, and that field is a snapshot `check()` takes once,
    /// at the top of `Job::InstallLanguage`/before either language job even
    /// starts — neither `install_language` nor `remove_language` otherwise
    /// touches it, and nothing else in this worker calls `check()` again on
    /// their behalf. The gap is invisible to every test that came before
    /// this one: `installing_a_pack_stages_nothing_and_asks_root_for_
    /// nothing` and its neighbours assert the disk, never this row, and
    /// `status::locales`'s own tests hand-construct `state.components`
    /// rather than drive it through a real install. It is exactly what a
    /// page polling `/api/locale` for the row to settle (`ConfigView.vue`'s
    /// `pollLanguageWhileBusy`) needs to be able to observe — a real
    /// install that never updates its own row would poll for the full
    /// twenty seconds and give up, looking indistinguishable from a job
    /// that silently failed.
    async fn mark_pack_row(&self, id: &str, installed: Option<String>) {
        let mut state = self.state.write().await;
        for row in state.components.iter_mut().filter(|c| c.name == id) {
            row.installed = installed.clone();
            row.availability = match (&row.installed, &row.offered) {
                (None, _) => Availability::NotInstalled,
                (Some(i), Some(o)) if differs(Some(i.as_str()), o) => Availability::UpdateAvailable,
                (Some(_), Some(_)) => Availability::Aligned,
                // Unreachable for a real pack row today: `component_offers`
                // (`update::state`) only ever creates a language-pack row
                // for a pack on offer (`Checked::packs`), so `offered`
                // is `Some` by construction wherever `mark_pack_row` finds
                // one. Dead code, kept in step with that same function's
                // own core row anyway (`None => Availability::Unknown`,
                // fix round 1, item 5) rather than left to invent a second,
                // different rule for the identical shape ("nothing to
                // judge this against") elsewhere in the same module.
                (Some(_), None) => Availability::Unknown,
            };
        }
    }

    /// Removes a language — **every** installed pack of it, ours and each
    /// source's (spec §5.2) — and puts the device back on English if that was
    /// the language in use.
    ///
    /// The packs are the ones whose own manifest names this language
    /// (`Registry::installed_packs`, which `inventory` has checked against
    /// each directory's id), plus our own id whatever the registry holds, as
    /// before. One removal refused does not stop the others; the first refusal
    /// is what the page hears, and the language stays chosen, since some of
    /// it is still there.
    ///
    /// **The stored choice goes too, and only on a deliberate removal.** Any
    /// other disappearance — a pack that fails to reinstall, a component
    /// update — keeps it, which is what makes recovery silent. Here the
    /// operator asked for the language to go, so an appliance that slipped
    /// back into it by itself the day a pack reappeared would be acting on
    /// an intention nobody still holds.
    async fn remove_language(&self, language: &str) {
        let mut ids = vec![crate::langpack::store::pack_id(language)];
        for pack in self.registry.read().await.installed_packs() {
            // Without case, as `offered_packs` gathers them.
            if pack.manifest.language.eq_ignore_ascii_case(language) && !ids.contains(&pack.id) {
                ids.push(pack.id.clone());
            }
        }
        let mut refused: Option<(String, String)> = None;
        let mut removed: Vec<&str> = Vec::new();
        for id in &ids {
            match crate::langpack::store::remove(&self.packs_root, id) {
                Ok(true) => tracing::info!("update: {id} removed"),
                Ok(false) => tracing::info!("update: {id} was not installed"),
                Err(e) => {
                    tracing::warn!("update: removing {id}: {e}");
                    refused.get_or_insert((id.clone(), e.to_string()));
                    continue;
                }
            }
            removed.push(id);
        }
        crate::i18n::Registry::resweep_async(&self.registry).await;
        for id in removed {
            self.mark_pack_row(id, None).await;
        }
        if let Some((id, detail)) = refused {
            // Not `message_for`: that helper fills only `{component}`, and
            // this refusal's catalog text also names the cause, the same two
            // parameters `refusal_message` gives the install path's own
            // `Refusal::Pack`.
            let message = ritornello_i18n::interpolate(
                &self.message("update_pack_refused").await,
                [("component", id.as_str()), ("detail", detail.as_str())],
            );
            self.publish_failure(message).await;
            return;
        }
        if self.locale_current.read().await.as_deref() == Some(language) {
            // Through the channel the HTTP layer already uses, so the core
            // persists it exactly as a person picking English would.
            let _ = self.locale_tx.send("en".to_string()).await;
        }
    }

    /// One source's answer: one request, and the **same** `parse_releases`
    /// and `fold` as our own release list — drafts and prereleases dropped by
    /// one rule, the newest archive for this architecture picked by one rule,
    /// and the version read off the asset name rather than off the tag, which
    /// is what makes an offered version the version of the archive that would
    /// actually be installed.
    ///
    /// `None` for an answer that is not usable — no connection, a status that
    /// is not 200 (whatever body came with it), a body that is not a release
    /// list. A repository that answered a release list with nothing for this
    /// channel did answer, and answered nothing: `Some` of an empty list, so
    /// its report says "answered" rather than "silent".
    async fn fetch_source(
        &self,
        client: &reqwest::Client,
        channel: Channel,
        target: sources::SourceTarget,
    ) -> Option<Vec<Published>> {
        let sources::SourceTarget { repo, url } = target;
        let (status, body) = match fetch_text(client, &url).await {
            Ok(answer) => answer,
            Err(e) => {
                tracing::warn!("update: {repo}: {e}");
                return None;
            }
        };
        if status != 200 {
            tracing::warn!("update: {repo} answered HTTP {status}");
            return None;
        }
        match parse_releases(&body, channel) {
            Ok(releases) => Some(fold(&releases, ARCH)),
            Err(ReleasesError::NoRelease | ReleasesError::OnlyPrereleases) => Some(Vec::new()),
            Err(ReleasesError::Unreadable) => {
                tracing::warn!("update: {repo} did not answer a list of releases");
                None
            }
        }
    }

    /// Asks every source at once (`sources::query_sources`), on the channel
    /// the setting names **now**.
    ///
    /// One read for the whole sweep: the setting cannot meaningfully change
    /// between two sources inside one check, and re-reading per source would
    /// let it, which would make a check's answer depend on which server
    /// answered first.
    async fn sweep_sources(
        &self,
        client: &reqwest::Client,
        targets: &[sources::SourceTarget],
        deadline: std::time::Duration,
    ) -> Vec<sources::SourceAnswer> {
        let channel = self.channel().await;
        sources::query_sources(targets, |t| self.fetch_source(client, channel, t), deadline).await
    }

    /// The sources one check asks, from the device as it is now.
    ///
    /// The operator's list is cloned and its guard dropped **before** any
    /// request: the routes write through that lock, and one held across a
    /// twenty-second sweep would hold every route that touches it.
    async fn targets_now(&self, installed: &[Installed]) -> Vec<sources::SourceTarget> {
        let added = self.update_sources.read().await.clone();
        // The sources installed third-party packs name, from the registry's
        // last sweep: memory, not a directory walk.
        let pack_sources = sources::pack_sources(self.registry.read().await.installed_packs());
        sources::source_targets(installed, &pack_sources, &added)
    }

    /// The check. Two small requests — the release list and nothing else — and
    /// no archive: knowing in advance whether every component is installable
    /// would mean fetching eleven archives a day for a fact that changes once
    /// per release.
    ///
    /// Returns the fold so a scheduled run can install from it without asking
    /// GitHub the same question twice.
    ///
    /// A check that fails before the sources are asked leaves the previous
    /// `source_reports` in place: acceptable, because the outcome names the
    /// failure, so the page never presents those reports as fresh.
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
            Err(e @ (ReleasesError::NoRelease | ReleasesError::OnlyPrereleases)) => {
                // A state, and never a failure. Two of them, sharing every
                // line of this branch but the sentence they end on: nothing is
                // published at all, or something is and this channel declined
                // it. They behave identically — the same empty offer, the same
                // third-party rows — because in both cases *this* device has
                // nothing of ours to install; they read differently because
                // only one of them is undone by a switch its reader owns.
                // `_known`, as `settle_check` reads it: an unreadable
                // `plugins.toml` must reach `Checked::plugins_unknown` on
                // this branch too, or an install after it would take every
                // installed stranger for one of ours.
                let installed = self.installed_when_settled_known().await;
                // Our repository publishing nothing says nothing about a
                // stranger's, so the third-party rows are still answered.
                let targets = self.targets_now(installed.as_deref().unwrap_or_default()).await;
                let answers = self.sweep_sources(client, &targets, sources::SOURCES_DEADLINE).await;
                // `Some` with an empty `ours`, and not `None`: this branch has
                // just offered third-party updates on the page, and returning
                // `None` would make Install do nothing and say nothing about
                // them. Our own components resolve to `Nothing` from an empty
                // list, which is the truth here.
                return Some(self.settle_without_release(client, e, installed.as_deref(), &targets, answers).await);
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
        self.settle_check(client, &releases).await
    }

    /// The rest of a check, once our release list has been read and parsed:
    /// the rows, and every rule that marks them. Split from `check` only so a
    /// test can drive this whole half — the registry read included — with a
    /// release list of its own, since `check` itself fetches from GitHub.
    async fn settle_check(&self, client: &reqwest::Client, releases: &[Release]) -> Option<Checked> {
        let published = fold(releases, ARCH);
        let installed = self.installed_when_settled_known().await;
        let targets = self.targets_now(installed.as_deref().unwrap_or_default()).await;
        let answers = self.sweep_sources(client, &targets, sources::SOURCES_DEADLINE).await;
        Some(self.settle_with_release(client, published, installed.as_deref(), &targets, answers).await)
    }

    /// A check whose release list was read, once the sources have answered.
    /// Its I/O is the registry, the companions file and the catalogues that
    /// describe what is offered (`fetch_contracts`, as in
    /// `settle_without_release`), so a test hands it the answers a sweep
    /// would have collected and serves those catalogues itself.
    ///
    /// `known` is `None` when `plugins.toml` could not be read. The rows are
    /// then built from an empty list, as before, but **no fresh offer and no
    /// conflict is made**: the reasoning of `settle_without_release`, applied
    /// to the other list ownership is judged against. With the device's own
    /// plugins unknown, every installed name — a third-party `zed` included —
    /// would look free, and a stranger publishing `zed` would be offered as
    /// its owner
    /// (`a_stranger_is_offered_nothing_fresh_while_plugins_toml_is_unreadable`).
    async fn settle_with_release(
        &self,
        client: &reqwest::Client,
        published: Vec<Published>,
        known: Option<&[Installed]>,
        targets: &[sources::SourceTarget],
        answers: Vec<sources::SourceAnswer>,
    ) -> Checked {
        let installed = known.unwrap_or_default();
        let mut checked = Checked {
            theirs: theirs_from(installed, &answers),
            third_party: third_party_names(installed),
            packs: sources::pack_offers(&published, &answers),
            ours: published,
            sources: answers,
            fresh: Vec::new(),
            conflicts: Vec::new(),
            plugins_unknown: known.is_none(),
            contracts: ContractsByUrl::new(),
        };
        // Here and only here: `ours` is a fold that was actually read, so
        // which names are ours is known — and only when what the device has
        // is known too.
        if let Some(installed) = known {
            checked.judge_strangers(installed);
        }
        let installed_packs = self.installed_packs().await;
        let mut components = component_offers(
            self.core_version,
            &checked.ours,
            &checked.theirs,
            installed,
            &installed_packs,
            &checked.packs,
            &checked.fresh,
            &checked.conflicts,
        );
        // The catalogues of the releases carrying what is offered, read
        // before the state lock is taken, like every other I/O here.
        checked.contracts = fetch_contracts(client, contract_urls(&checked, &components)).await;
        let live = self.live_speaks().await;
        let core = checked.ours.iter().find(|p| p.offer == Offer::Core);
        // Read before the state lock is taken: a file read has no business
        // holding the lock every route reads through.
        let companions = installed_companions(&self.root);
        let mut state = self.state.write().await;
        carry_installable(&state.components, &mut components);
        carry_core_notes(&state.components, &mut components);
        deny_privileged_install(&mut components);
        deny_moved_companion(&mut components, &checked.ours, &companions);
        judge_rows(&mut components, &checked, &live);
        state.major_update_waiting = core_breaks(&components);
        state.outcome = CheckOutcome::Ok;
        state.release_version = core.map(|p| p.version.clone());
        state.release_url = core.map(|p| release_page(&p.release_tag));
        state.catalogue_url = newest_catalogue_url(&checked.ours);
        state.last_check_unix_s = Some(now_unix_s());
        state.components = components;
        state.source_reports = sources::reports_of(targets, &checked.sources);
        state.source_catalogues = sources::source_catalogues(&checked.sources, &checked.fresh);
        drop(state);
        checked
    }

    /// A check whose release list held nothing this device could be offered
    /// (`NoRelease`, `OnlyPrereleases`), once the sources have answered.
    ///
    /// **No fresh offer and no conflict here, by construction.** `ours` is
    /// empty because nothing of ours could be read, not because nothing is
    /// ours: judged against it, a stranger publishing `radio` would be the
    /// only owner of our own plugin's name, and an operator who then
    /// installed it would have handed that name to them
    /// (`a_stranger_is_offered_nothing_fresh_while_our_release_list_is_unread`).
    /// The installed third-party plugins' own updates (`theirs`) are still
    /// answered: those are judged by the plugin's own announcement, which
    /// owes nothing to our release.
    ///
    /// `known` is `None` when `plugins.toml` could not be read, as for
    /// `settle_with_release`: the rows are built from an empty list and the
    /// check says so (`Checked::plugins_unknown`).
    async fn settle_without_release(
        &self,
        client: &reqwest::Client,
        why: ReleasesError,
        known: Option<&[Installed]>,
        targets: &[sources::SourceTarget],
        answers: Vec<sources::SourceAnswer>,
    ) -> Checked {
        let installed = known.unwrap_or_default();
        let mut checked = Checked {
            ours: Vec::new(),
            theirs: theirs_from(installed, &answers),
            third_party: third_party_names(installed),
            // A source's pack is still offered: its id carries its source, so
            // nothing of ours has to be known to place it where it belongs.
            packs: sources::pack_offers(&[], &answers),
            sources: answers,
            fresh: Vec::new(),
            conflicts: Vec::new(),
            plugins_unknown: known.is_none(),
            contracts: ContractsByUrl::new(),
        };
        let installed_packs = self.installed_packs().await;
        // The rows are rebuilt against an empty offer rather than left as
        // they were: a repository that has no release offers nothing, and
        // `component_offers` answers `Unknown` for every component — which is
        // the truth, where a leftover "0.3.0 available" from a previous check
        // would be a claim about a release that is no longer there.
        let mut components = component_offers(
            self.core_version,
            &[],
            &checked.theirs,
            installed,
            &installed_packs,
            &checked.packs,
            &checked.fresh,
            &checked.conflicts,
        );
        // A third-party update is judged by its own release's catalogue, which
        // owes nothing to ours.
        checked.contracts = fetch_contracts(client, contract_urls(&checked, &components)).await;
        let live = self.live_speaks().await;
        let mut state = self.state.write().await;
        // As after a check that read our release: a refusal remembered for
        // an offered version must survive this branch too, or a third-party
        // archive refused yesterday (`install_pack`'s mark, P5) is fetched
        // again tonight. Keyed on `(name, offered)`, so our own rows, offered
        // nothing here, only meet a previous row that was offered nothing
        // too — and `deny_privileged_install` decides after it, as it does
        // in `settle_with_release`.
        carry_installable(&state.components, &mut components);
        carry_core_notes(&state.components, &mut components);
        deny_privileged_install(&mut components);
        judge_rows(&mut components, &checked, &live);
        state.major_update_waiting = core_breaks(&components);
        state.outcome = match why {
            ReleasesError::OnlyPrereleases => CheckOutcome::OnlyPrereleases,
            _ => CheckOutcome::NoRelease,
        };
        state.release_version = None;
        state.release_url = None;
        state.catalogue_url = None;
        state.last_check_unix_s = Some(now_unix_s());
        state.components = components;
        state.source_reports = sources::reports_of(targets, &checked.sources);
        // Nothing is offered fresh here (see above), so no source has a name
        // its catalogue may describe.
        state.source_catalogues = Vec::new();
        drop(state);
        checked
    }

    /// Installs the named components, plugins first and the core last.
    ///
    /// A component that fails names its cause and the next one is still
    /// attempted: one refusal should say one thing, not cancel a gesture the
    /// operator asked for on five rows. Only the **first** cause reaches the
    /// page, which is the honest limit of a payload with one message field.
    ///
    /// **A language pack is routed to `install_pack` here, before
    /// `resolve`/`carries` ever sees its name** (fix round 2, F1 of the
    /// whole-branch review). Before this, `carries` answered `false` for
    /// every `Offer::LanguagePack` by design, so a pack's row always resolved
    /// to `Resolved::Nothing` and refused with "nothing published carries
    /// this name" — even while the release genuinely offered it, and even
    /// while the config page's own Update button installed that same pack
    /// correctly through `POST /api/languages/{language}`. The two routes
    /// now agree: a name that is the id of a pack this same `checked` offers
    /// (`Checked::packs`, ours or a source's) is installed exactly the way
    /// `Job::InstallLanguage` installs each of its packs — no staging, no
    /// privileged unit. A name shaped like a pack id but not backed by an
    /// offer falls through to `resolve` unchanged, which still answers
    /// `Resolved::Nothing` for it — the same honest refusal as before, now
    /// reached only when it is true.
    async fn install(&self, client: &reqwest::Client, checked: &Checked, names: &[String]) {
        self.install_consented(client, checked, names, &[]).await
    }

    /// `install`, with the repositories the operator confirmed for the
    /// plugins asked for fresh: `(name, owner/repo)`, as the page's second
    /// consent named them (spec §4.5).
    ///
    /// **A fresh offer installs only from the repository confirmed for it.**
    /// The check this install rests on is run again at the gesture, and by
    /// then another trusted source may be the one offering that name (the
    /// first removed, a second added): the confirmation named a repository,
    /// so that repository is the only one the archive may come from. None
    /// confirmed — an automatic run, a request without one — installs
    /// nothing fresh at all (`Refusal::NotConsented`). An update of an
    /// installed plugin, ours or a stranger's, needs no such consent: its
    /// source is the one its own announcement names.
    async fn install_consented(
        &self,
        client: &reqwest::Client,
        checked: &Checked,
        names: &[String],
        consented: &[(String, String)],
    ) {
        let mut first_failure: Option<String> = None;
        // `(component, version)` per plugin actually placed. The core is never
        // in here: it exits at the end of its own install and this function
        // has already returned.
        let mut placed: Vec<Placement> = Vec::new();
        // The rows the check this install rests on has just written: what
        // each offered component speaks, and whether the core breaks.
        let rows = self.state.read().await.components.clone();
        // **A breaking core travels with the plugins that depend on it.** On
        // a wire break, a plugin the running core refuses can only be placed
        // in the same request as the core (`install_group`); every other
        // plugin goes first, alone and restarted, as always — it runs under
        // the running core and survives a rollback of the new one. A
        // dependent the operator did not tick is simply not in `names`: the
        // core goes without it (the dialog warned), and the new core refuses
        // it afterwards.
        let group: Option<Vec<String>> = (names.iter().any(|n| n == CORE) && core_breaks(&rows))
            .then(|| {
                names
                    .iter()
                    .filter(|n| *n != CORE)
                    .filter(|n| rows.iter().any(|r| r.name == **n && refused_by_running_core(r)))
                    .cloned()
                    .collect()
            });
        for name in install_order(names) {
            if let Some(dependents) = &group
                && (name == CORE || dependents.contains(&name))
            {
                continue;
            }
            // By id, and only an id this check offers: ours (`ritornello-lang-
            // fr`) or a source's (`ritornello-xlang-fr-<h12>`), each carrying
            // the source it must come from.
            if let Some(offer) = checked.packs.iter().find(|p| p.id == name) {
                self.set_busy(Some(self.message_for("update_installing", &name).await))
                    .await;
                // Read before `install_pack` runs: it is what marks
                // this exact row `installed` on success
                // (`mark_pack_row`), so reading it afterwards would
                // always find one and report every install as an
                // update, never a first installation.
                let was_installed = self
                    .state
                    .read()
                    .await
                    .components
                    .iter()
                    .any(|c| c.name == name && c.installed.is_some());
                match self.install_pack(offer).await {
                    Ok(()) => {
                        placed.push(Placement {
                            component: name.clone(),
                            version: offer.published.version.clone(),
                            fresh: !was_installed,
                        });
                    }
                    Err(why) => {
                        tracing::warn!("update: installing {name}: {why}");
                        let catalog = self.catalog.read().await;
                        let message = refusal_message(&catalog, &name, &why);
                        drop(catalog);
                        first_failure.get_or_insert(message);
                    }
                }
                continue;
            }
            let (offered, repo, provenance) =
                match self.resolve_for_install(checked, &rows, &name, consented) {
                    Ok(resolved) => resolved,
                    Err(why) => {
                        tracing::warn!("update: not installing {name}: {why}");
                        let catalog = self.catalog.read().await;
                        let message = refusal_message(&catalog, &name, &why);
                        drop(catalog);
                        first_failure.get_or_insert(message);
                        continue;
                    }
                };
            self.set_busy(Some(self.message_for("update_installing", &name).await))
                .await;
            // The repository that answered travels with its offer: it is the
            // placement memory's key, and the very repository the archive is
            // about to come from.
            let companion = if provenance == Provenance::Ours {
                companion_offered(&checked.ours, &name)
            } else {
                None
            };
            match self
                .install_one(client, &name, offered, provenance, repo, companion)
                .await
            {
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
        if let Some(dependents) = group {
            match self.install_group(client, checked, &rows, &dependents, consented).await {
                Ok(()) => {
                    // As for a core installed alone: the process leaves, and
                    // the new core starts every plugin, the group's included.
                    tracing::info!(
                        "update: the core and {dependents:?} have been replaced, leaving so systemd starts the new core"
                    );
                    (self.restart)();
                    return;
                }
                Err(why) => {
                    tracing::warn!("update: installing the core with {dependents:?}: {why}");
                    let catalog = self.catalog.read().await;
                    let message = refusal_message(&catalog, CORE, &why);
                    drop(catalog);
                    first_failure.get_or_insert(message);
                }
            }
        }
        self.conclude_install(checked, &placed, first_failure).await;
    }

    /// What `name` is to be installed from, or why it may not be — every
    /// refusal that needs no byte of the archive. One function for the
    /// component-by-component path and for the group, so a member of the
    /// group is held to exactly the rules it would meet alone.
    ///
    /// `rows` are the rows of the check this install rests on.
    fn resolve_for_install<'c>(
        &self,
        checked: &'c Checked,
        rows: &[ComponentOffer],
        name: &str,
        consented: &[(String, String)],
    ) -> Result<(&'c Published, Option<&'c str>, Provenance), Refusal> {
        // Past the packs, every name is a plugin's or the core's, and
        // whose plugin it is was judged against `plugins.toml`. Unread,
        // that judgement said "nobody's": a plugin is refused rather than
        // resolved, or a fork named like ours would be replaced by our
        // archive. **The core is exempt**: `core` is reserved, no fork
        // takes its shape, its placement path is its own — and a
        // self-update is exactly what may repair a device whose
        // `plugins.toml` the running core cannot parse.
        if checked.plugins_unknown && name != CORE {
            return Err(Refusal::PluginsUnreadable);
        }
        // Whose plugin it is comes first: unknown, nothing else about it
        // can be trusted.
        //
        // **Not installable from the device, by hand either**: the release
        // carrying this archive does not say what it speaks, so nothing can
        // tell whether it would run (`judge_contracts`). Only this reason is
        // honoured here: an `installable: Some(false)` remembered from an
        // archive refused earlier, or from a moved companion, stops only the
        // automatic policy, and a hand install retries it as it always has.
        if rows.iter().any(|r| {
            r.name == name
                && r.contracts.not_installable_reason == Some(NotInstallable::ContractsUnpublished)
        }) {
            return Err(Refusal::ContractsUnpublished);
        }
        // What the component **is** decides which list answers for it —
        // never which lookup happened to return something. See `resolve`.
        match resolve(checked, name) {
            Resolved::Theirs { published, repo } => Ok((published, Some(repo), Provenance::Theirs)),
            Resolved::Ours(published) => Ok((published, None, Provenance::Ours)),
            // A named refusal and not a silent skip: the operator ticked
            // this row, and "nothing happened" would read as a failure of
            // the gesture rather than as what it is.
            Resolved::UncheckedThirdParty => Err(Refusal::ThirdPartyUnchecked),
            Resolved::FreshTheirs { published, repo } => {
                // The second consent named a repository: only that one may
                // supply this name (see `install_consented`).
                let confirmed = consented.iter().any(|(n, r)| {
                    *n == name
                        && sources::normalize_repo(r).is_some_and(|r| r.eq_ignore_ascii_case(repo))
                });
                if !confirmed {
                    return Err(Refusal::NotConsented(repo.to_string()));
                }
                // The check judged this name free; the gesture asks again,
                // because the device may have changed since and a fresh
                // offer must never replace anyone's plugin.
                self.still_unowned(name)?;
                // Its `[[plugin]]` block is the core's own, written by
                // `stage` (`third_party_fragment`); `repo` is the key the
                // placement is remembered under, which is the one this
                // plugin's later updates (`Theirs`) read.
                Ok((published, Some(repo), Provenance::Fresh))
            }
            // A named refusal, not a silent skip (task 18's review, C1): a
            // `missing_binary` or `undeclared_binary` row's badge does
            // **not** read `Unknown` (it reads "Not installed" or "Installed
            // but not declared" — see `ConfigView.vue`), so silence here was
            // never the harmless case its old comment assumed. It is also
            // the one shape that made "Declare" fail with no toast at all
            // when the row's name did not match the release.
            Resolved::Nothing => Err(Refusal::NothingPublished),
        }
    }

    /// **A breaking core and the plugins that depend on it, in one request.**
    ///
    /// Why one request: the privileged installer rewrites its backup manifest
    /// at every request, and the rollback (`OnFailure=` of the core's unit,
    /// only when the request replaced the core) restores every entry of that
    /// manifest. Sent one by one, as outside a break, the core's request
    /// would erase the plugins' backups, and a rollback would put the old
    /// core back beside new plugins it refuses — a silent device. In one
    /// request, a rollback restores the core **and** them.
    ///
    /// The compatible plugins have already gone, one by one, before this
    /// runs (`install_consented`): the running core accepts them, so they
    /// survive a rollback of the new one.
    ///
    /// In order: every refusal that needs no download, for each member; room
    /// for **the whole group** (each archive may fit alone and the group
    /// not); every member staged, dependents first and the core last; one
    /// request, plugins before the core — the order only matters if a write
    /// fails half-way, and then the core is never the one placed without
    /// them. **Any member that cannot be prepared keeps the whole group out**
    /// (`Refusal::GroupPostponed`): no request is written, the staged
    /// binaries are cleared, and placing the core without that plugin would
    /// have switched it off without the operator choosing so. The
    /// `/etc/ritornello` presets and a fresh member's initial configuration,
    /// written while staging, stay: the same harmless leftover a refused
    /// privileged step already leaves (see `stage`).
    ///
    /// On success every placement is remembered and every fresh member
    /// declared, and **no plugin of the group is restarted**: the core is
    /// about to leave, and the new one starts them all. A declaration that
    /// fails is logged and does not stop the restart — the core is already
    /// placed, and the binary left undeclared shows on the page as such.
    ///
    /// The request itself is not transactional: a failure inside root may
    /// leave some plugins placed and not the core. The page then reports the
    /// privileged failure, and the next press completes it.
    async fn install_group(
        &self,
        client: &reqwest::Client,
        checked: &Checked,
        rows: &[ComponentOffer],
        dependents: &[String],
        consented: &[(String, String)],
    ) -> Result<(), Refusal> {
        self.set_busy(Some(self.message_for("update_installing", CORE).await)).await;
        let postponed = |failed: &str, reason: Refusal| Refusal::GroupPostponed {
            failed: failed.to_string(),
            reason: Box::new(reason),
        };
        let mut members = Vec::new();
        for name in dependents.iter().map(String::as_str).chain([CORE]) {
            let (offered, repo, provenance) = self
                .resolve_for_install(checked, rows, name, consented)
                .map_err(|why| postponed(name, why))?;
            members.push((name, offered, repo, provenance));
        }
        let total: u64 = members.iter().map(|(_, offered, _, _)| offered.size).sum();
        let root = self.root.to_string_lossy().to_string();
        if !enough_room(crate::system::disk_usage(&root), total as usize) {
            return Err(Refusal::NoRoom);
        }
        let mut staged = Vec::new();
        for (name, offered, repo, provenance) in &members {
            let companion = if *provenance == Provenance::Ours {
                companion_offered(&checked.ours, name)
            } else {
                None
            };
            match self.stage(client, name, offered, *provenance, *repo, companion).await {
                Ok(member) => staged.push(member),
                Err(why) => {
                    // Nothing was asked of root: clear what this group put in
                    // staging, the failing member's own leftover included.
                    for (_, offered, _, _) in &members {
                        if let Some(file) = download_name(&offered.offer)
                            && let Err(e) = std::fs::remove_file(self.staging.join(&file))
                            && e.kind() != std::io::ErrorKind::NotFound
                        {
                            tracing::debug!("update: leaving {file} in staging: {e}");
                        }
                    }
                    return Err(postponed(name, why));
                }
            }
        }
        if let Err(why) = self.place(&staged).await {
            tracing::warn!(
                "update: the grouped request failed; some of its plugins may have been placed without the core, and installing again completes it"
            );
            return Err(why);
        }
        for member in staged.iter().filter(|m| !m.is_core()) {
            if let Some(fragment) = &member.fragment
                && let Err(why) = self.write_declaration(&member.name, fragment)
            {
                tracing::warn!("update: {} is placed but could not be declared: {why}", member.name);
            }
        }
        Ok(())
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
            let installed_packs = self.installed_packs().await;
            let mut components = component_offers(
                self.core_version,
                &checked.ours,
                &checked.theirs,
                &installed,
                &installed_packs,
                &checked.packs,
                &checked.fresh,
                &checked.conflicts,
            );
            let companions = installed_companions(&self.root);
            let live = self.live_speaks().await;
            let mut state = self.state.write().await;
            carry_installable(&state.components, &mut components);
            carry_core_notes(&state.components, &mut components);
            deny_privileged_install(&mut components);
            deny_moved_companion(&mut components, &checked.ours, &companions);
            judge_rows(&mut components, checked, &live);
            state.major_update_waiting = core_breaks(&components);
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
    ///
    /// `companion_offered` is the version our release offers for this
    /// plugin's companion (`companion_offered`, `None` when the plugin has
    /// none or the release carries none): it is what `companion_allows`
    /// compares with the version `ritornello-install` recorded.
    ///
    /// `stage` then `commit_one`: the two halves a grouped install
    /// (`install_group`) runs separately, so several components can be
    /// prepared before one request places them all.
    async fn install_one(
        &self,
        client: &reqwest::Client,
        name: &str,
        offered: &Published,
        provenance: Provenance,
        repo: Option<&str>,
        companion_offered: Option<&str>,
    ) -> Result<Placed, Refusal> {
        let staged = self.stage(client, name, offered, provenance, repo, companion_offered).await?;
        self.commit_one(staged).await
    }

    /// Everything about one component that happens **before** root is asked
    /// anything: room, bytes, digest, archive, the staged binary, the
    /// `/etc/ritornello` files and, for a fresh plugin, its initial
    /// configuration. No request is written.
    async fn stage(
        &self,
        client: &reqwest::Client,
        name: &str,
        offered: &Published,
        provenance: Provenance,
        repo: Option<&str>,
        companion_offered: Option<&str>,
    ) -> Result<Staged, Refusal> {
        let is_core = offered.offer == Offer::Core;
        let third_party = provenance != Provenance::Ours;
        // **A privileged plugin's companion is `ritornello-install`'s**, and
        // both questions about it are answered before a byte is downloaded:
        // neither needs the archive.
        //
        // - Installing one from here is refused whatever its archive holds —
        //   its companion goes in with it (`deny_privileged_install` gives
        //   the row the same answer from the plugin's name).
        // - Updating one of ours is allowed while its companion does not
        //   move (`companion_allows`). A third-party plugin that happens to
        //   bear the name is judged by `only_its_own_binary` alone: our
        //   release's companion says nothing about a stranger's binary.
        //
        // The registry is read here, from the worker, never from a route.
        if !is_core && let Some(companion) = crate::plugins::companion_of(name) {
            let refused = if !self.declared(name) {
                true
            } else if third_party {
                false
            } else {
                let installed = install_registry::companion_version(&self.root, companion);
                let allowed = companion_allows(companion_offered, installed.as_deref());
                tracing::info!(
                    "update: {name}: its companion {companion} is offered at {} and installed at {} -- {}",
                    companion_offered.unwrap_or("nothing"),
                    installed.as_deref().unwrap_or("an unknown version"),
                    if allowed { "placing its binary alone" } else { "refused, ritornello-install places both" }
                );
                !allowed
            };
            if refused {
                self.remember_companion_step(name, companion).await;
                return Err(Refusal::NeedsCompanionStep(companion));
            }
        } else if !is_core && crate::plugins::is_privileged(name) && !self.declared(name) {
            // A privileged plugin with no companion cannot exist
            // (`every_privileged_plugin_agrees_with_packaging_toml`); refused
            // all the same rather than trusted to be impossible.
            self.remember_manual_step(name).await;
            return Err(Refusal::NeedsManualStep);
        }
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
        // A **fresh offer**, and only a fresh offer, gets **the core's**
        // block, built from the offered name and the device's plugins
        // directory — never from the archive, which may carry nothing but its
        // binary anyway (`archive_allowed` above). See `third_party_fragment`,
        // and `Provenance` for why a `Theirs` update with no block is refused
        // instead.
        //
        // Ownership is asked once more here, after the download and before
        // any write: `install` asked before downloading, and the device may
        // have changed in between. `still_unowned` and not `declared`, which
        // answers `false` for an unreadable file.
        let fragment = if provenance == Provenance::Fresh {
            self.still_unowned(name)?;
            let Some((file, _)) = &contents.binary else {
                return Err(Refusal::Prepare(format!(
                    "the archive of {name} carries no plugin binary"
                )));
            };
            Some(third_party_fragment(name, file, &plugins_dir(&self.root))?)
        } else {
            declaration_needed(is_core, declared, contents.fragment.as_deref())?
        };
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
        // keeps its list of paths down to one.
        //
        // Written **before** the unit runs, so a unit that then fails leaves
        // the new input presets beside the old binary. Harmless as things
        // stand — a preset is a default a plugin falls back on, not a live
        // dependency — and the alternative (placing them after) would leave
        // the new binary beside the old presets, which is the same mismatch
        // the other way round.
        self.write_etc_files(&contents.etc_files)?;
        // Only for a plugin that was not there. On an update, the operator's
        // station list is already in place and `write_initial_config` would
        // leave it alone anyway — but not asking the question at all is what
        // makes that guarantee independent of a `exists()` call.
        if fresh {
            self.write_initial_config(name, &contents.initial_config)?;
        }
        Ok(Staged {
            name: name.to_string(),
            version: offered.version.clone(),
            third_party,
            repo: repo.map(str::to_string),
            action,
            staged_file: staged,
            fragment,
            core_notes,
        })
    }

    /// The second half of `install_one`: one request for one staged
    /// component, then its declaration when it is fresh.
    async fn commit_one(&self, staged: Staged) -> Result<Placed, Refusal> {
        self.place(std::slice::from_ref(&staged)).await?;
        // The order matters, and it is not the intuitive one: **the binary is
        // placed before the declaration is written.**
        //
        // A declaration pointing at a file that is not there is exactly the
        // state this chantier repairs elsewhere (`Availability::BinaryMissing`),
        // and creating it on a failure path would be careless. The reverse
        // leftover — a binary nobody declares — is harmless, visible on the
        // page as `Undeclared`, and undone by one gesture.
        if let Some(fragment) = &staged.fragment {
            self.write_declaration(&staged.name, fragment)?;
            return Ok(Placed::NewPlugin);
        }
        Ok(if staged.is_core() { Placed::Core } else { Placed::Plugin })
    }

    /// Writes **one** request placing every member, in the order given, asks
    /// root for it once, and — only once it succeeded — remembers each
    /// placement and clears each staged file.
    ///
    /// One request is what makes a group one unit for the rollback: the
    /// privileged installer rewrites its backup manifest at every request
    /// and the rollback restores every entry of the **last** one, so members
    /// placed by separate requests would each erase the previous one's
    /// backup.
    async fn place(&self, members: &[Staged]) -> Result<(), Refusal> {
        let request = Request {
            format: REQUEST_FORMAT,
            actions: members.iter().map(|m| m.action.clone()).collect(),
        };
        let request_path = self.staging.join("request.json");
        let text = serde_json::to_string(&request)
            .map_err(|e| Refusal::Prepare(format!("the request: {e}")))?;
        std::fs::write(&request_path, &text)
            .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", request_path.display())))?;
        #[cfg(test)]
        SEEN_REQUESTS.with(|seen| seen.borrow_mut().push(text.clone()));

        if let Err(detail) = run_privileged_unit().await {
            // systemctl's own words travel verbatim to the page. They do not
            // name the missing polkit rule — see `run_privileged_unit` — but
            // they are the only account of the failure that exists on this
            // side of the boundary.
            return Err(Refusal::Privileged(detail));
        }
        for member in members {
            // Written only once the placement actually succeeded: a refusal
            // above must not claim that anything was placed. And written
            // **here**, before the caller sees success — a core placement
            // ends with the process leaving, so this is the last instant at
            // which anything can be remembered about it. See
            // `remember_placed`, and `automatic_install_list` for what reads
            // it. A third party is remembered under a namespaced key, never
            // its bare name: that name is a stranger's choice and may be one
            // of ours.
            match (member.third_party, member.repo.as_deref()) {
                (false, _) => {
                    self.remember_placed(&member.name, &member.version, member.core_notes.clone())
                }
                (true, Some(repo)) => self.remember_placed(
                    &third_party_placed_key(repo, &member.name),
                    &member.version,
                    member.core_notes.clone(),
                ),
                // No repository to namespace by: nothing is remembered, which
                // fails open (the component is attempted again) as the module
                // doc says it should.
                (true, None) => {}
            }
            // The installer **copies** what it places (it renames a copy made
            // inside the target's own directory, since a rename across mounts
            // is not atomic), so the staged binary survives its own
            // installation. Left alone they accumulate: one uncompressed
            // binary per component, for ever, on the SD card of a device with
            // no janitor. Removed best-effort — the bytes are now at their
            // target, and a stale `request.json` naming a file that no longer
            // exists is refused by the installer rather than silently
            // re-applied.
            if let Err(e) = std::fs::remove_file(self.staging.join(&member.staged_file)) {
                tracing::debug!("update: leaving {} in staging: {e}", member.staged_file);
            }
        }
        Ok(())
    }

    fn write_staged(&self, staged: &str, bytes: &[u8]) -> Result<(), Refusal> {
        let path = self.staging.join(staged);
        std::fs::write(&path, bytes)
            .map_err(|e| Refusal::Prepare(format!("writing {}: {e}", path.display())))
    }

    /// Writes down what this pass just placed, and — for the core — what its
    /// archive carried that nothing here installs.
    ///
    /// **Called from `place`, after the privileged unit has succeeded and
    /// before it returns**, which is what puts it before the restart: a core
    /// placement — alone or in a group — ends with `install` calling
    /// `self.restart`, and that hook does not return. Anything written after
    /// it would be written never. Keeping the write inside `place` rather than
    /// at the `Ok` arms of `install` is what makes that ordering a fact about
    /// the shape of the code instead of a rule several call sites have to
    /// remember.
    ///
    /// **A third-party component is remembered under a namespaced key**
    /// (`third_party_placed_key`), now that the fourth policy may install one
    /// unattended and the memory has something to bound. Its name is chosen by
    /// its own author and may collide with one of ours, which is the very
    /// reason `Checked` keeps two lists: the `third-party:<repo>:` prefix makes
    /// the collision impossible here rather than reasoned about.
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
    /// **One production caller, on purpose** (`place`). The tests reach
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

    /// The input presets a release owns.
    ///
    /// Written unconditionally, which is why `ETC_PREFIXES` is one named
    /// subdirectory and not `etc/ritornello/` at large: the operator's own
    /// files live in that directory too.
    ///
    /// Through a temporary and a `rename`, like every other file this product
    /// writes: this is a device one unplugs, and a preset file cut in half by
    /// a power cut is one that no longer parses — a plugin that reads it
    /// simply falls back on its own defaults.
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

    /// The plugin's own configuration, written **only where there is none**,
    /// into the plugin's own data directory — never under `self.root`, and
    /// never one of `archive::ETC_PREFIXES`, whose two subdirectories belong
    /// to the release and are overwritten on every update. This is where the
    /// operator's own files live, written once.
    ///
    /// `plugins::data_dir_for(&self.plugin_data_root, name)` forms the
    /// directory, the same one `main` hands the plugin as
    /// `RITORNELLO_PLUGIN_DATA_DIR`: writing anywhere else would place a
    /// station list or a set of key bindings where the plugin it is meant
    /// for never looks. A `name` that direction refuses is refused here too,
    /// before any path is formed or any byte written.
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
    fn write_initial_config(&self, name: &str, files: &[(String, Vec<u8>)]) -> Result<(), Refusal> {
        let Some(dir) = crate::plugins::data_dir_for(&self.plugin_data_root, name) else {
            return Err(Refusal::Prepare(format!(
                "{name} is not a valid plugin name; refusing to write its initial configuration"
            )));
        };
        for (entry, bytes) in files {
            let Some(file_name) = initial_config_target(entry) else {
                // Listed on the page as something the archive carries, and
                // written nowhere — the rule this whole module follows.
                tracing::warn!(
                    "update: not writing the initial configuration entry {entry:?}: it is not a bare file name"
                );
                continue;
            };
            let target = dir.join(&file_name);
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

    /// Is `name` still nobody's on this device, as it was when the check
    /// offered it fresh?
    ///
    /// Read from the disk at the gesture, since the check may be hours old:
    /// `plugins.toml` readable (an unreadable file proves nothing, and
    /// `declared` would answer `false` there), no block of that name, no
    /// block running the file the plugin would be placed as, and no such file
    /// already in the plugins directory. Any of the last three means
    /// installing would replace a binary someone else owns.
    fn still_unowned(&self, name: &str) -> Result<(), Refusal> {
        let manifest = PluginManifest::load(&self.manifest).map_err(|e| {
            Refusal::NotItsOwnName(format!("{} cannot be read: {e:#}", self.manifest.display()))
        })?;
        let target = plugins_dir(&self.root).join(format!("ritornello-plugin-{name}"));
        // Case-insensitively, as `sources::fresh_offers` judges ownership.
        if manifest.plugins.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
            return Err(Refusal::NotItsOwnName(format!("{name} is declared on this device")));
        }
        if let Some(p) = manifest.plugins.iter().find(|p| Path::new(&p.exec) == target) {
            return Err(Refusal::NotItsOwnName(format!("{} runs {}", p.name, target.display())));
        }
        if target.exists() {
            return Err(Refusal::LeftoverBinary(target.display().to_string()));
        }
        Ok(())
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

    /// `remember_manual_step` for a companion's refusal: the row also names
    /// the companion, so the page says what to do about it, and the next
    /// check recomputes it rather than carrying it (`carry_installable`).
    async fn remember_companion_step(&self, name: &str, companion: &str) {
        let mut state = self.state.write().await;
        for row in state.components.iter_mut().filter(|c| c.name == name) {
            row.installable = Some(false);
            row.needs_companion = Some(companion.to_string());
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
    /// plugin was refused for its contracts.
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
        let removed = self.erase_plugin_binary(name, file).await;
        // **Answered on every path out of the erasure, and that is the whole
        // point of the wrapper.** The page is probing this row until the mark
        // clears, so a path that returned without clearing would leave it
        // saying "erasure in progress" for as long as the core runs — the same
        // lie as the button this replaced, only harder to notice. On failure
        // the row falls back to the plain undeclared one, which is the truth,
        // with the reason `removal_failed` has already put on the card.
        self.state.write().await.removal_answered(name, file, removed);
    }

    /// The erasure itself. `true` when the privileged unit reported the file
    /// gone; `false` for every refusal, skip and failure — each of which has
    /// already published its own sentence by the time this returns.
    async fn erase_plugin_binary(&self, name: &str, file: &str) -> bool {
        match PluginManifest::load(&self.manifest) {
            Ok(m) if m.plugins.iter().any(|p| {
                Path::new(&p.exec).file_name().and_then(|f| f.to_str()) == Some(file)
            }) => {
                tracing::warn!(
                    "update: {file} is now declared by a plugin; the queued removal for {name} was skipped"
                );
                self.publish_failure(self.message_for("update_removal_skipped", name).await).await;
                return false;
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
            return false;
        }
        let request_path = self.staging.join("request.json");
        let text = match serde_json::to_string(&request) {
            Ok(t) => t,
            Err(e) => {
                self.removal_failed(name, format!("encoding the request: {e}")).await;
                return false;
            }
        };
        if let Err(e) = std::fs::write(&request_path, text) {
            self.removal_failed(name, format!("writing {}: {e}", request_path.display())).await;
            return false;
        }
        match run_privileged_unit().await {
            Ok(()) => {
                tracing::info!("update: {name}'s binary removed");
                true
            }
            Err(detail) => {
                self.removal_failed(name, detail).await;
                false
            }
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
        // Both parameters together, not `message_for`'s single-token
        // substitution followed by a further `.replace()`: that composition
        // is itself the chained-replace defect (task 10b) — a `name` that
        // happened to contain the literal text `{detail}` would be rewritten
        // by the second call.
        let template = self.message("update_removal_failed").await;
        let message = ritornello_i18n::interpolate(&template, [("component", name), ("detail", detail.as_str())]);
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
            Job::Install { names, consented } => {
                // A check first, always: it is what gives the download URLs
                // and the digests of the release as it stands right now, and
                // it costs two small requests next to an archive.
                if let Some(checked) = worker.check(&client).await {
                    worker.install_consented(&client, &checked, &names, &consented).await;
                }
            }
            Job::Scheduled { install } => {
                if let Some(checked) = worker.check(&client).await
                    && let Some(scope) = install
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
                        scope,
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
            Job::InstallLanguage(language) => {
                // A check first, for the same reason `Job::Install` takes
                // one: it is what gives the download URL and the digest of
                // the release as it stands right now, and a language pack
                // is judged by that same fold (`Offer::LanguagePack`), not
                // by a second, pack-only request.
                //
                // This arm has no test of its own that drives it through
                // `run_worker`. `check()`'s list endpoint does have a test
                // seam since task 14 (`release::TEST_RELEASES_URL_ENV`, see
                // `offered_packs`'s own doc), so a real dispatch test is
                // possible here now -- set the env var to a local server for
                // the duration of one `#[serial]`-style test, drive
                // `run_worker` with this exact job, and assert on
                // `worker.state`. Nobody has written it: every test below
                // that exercises `install_language`/`remove_language` calls
                // them directly instead (see this suite's own header comment
                // over "Language packs"), which proves the pack logic without
                // proving this arm routes to it, and that gap is what would
                // remain open if this comment were the only thing fixed here.
                if let Some(checked) = worker.check(&client).await
                    && let Err((id, e)) = worker.install_language(&checked, &language).await
                {
                    let message = refusal_message(&*worker.catalog.read().await, &id, &e);
                    worker.publish_failure(message).await;
                }
            }
            Job::RemoveLanguage(language) => {
                worker.remove_language(&language).await;
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
            "etc/ritornello/input-presets/",
            "etc/ritornello/input-presets/radio/",
            "etc/ritornello/input-presets/radio/default.toml",
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

    /// **Refusal 1 of 3: a check asks only the repositories it can address,
    /// and a stranger is a stranger whether it was asked or not.**
    ///
    /// The ceiling itself (`sources::SOURCES_MAX`, sixteen, a stable prefix)
    /// is `sources::tests::targets_are_a_stable_prefix_of_sixteen`'s; what is
    /// pinned here is the reading of a real announcement list: **six**
    /// third-party plugins with an official one, an unaddressable one and a
    /// silent one interleaved, so a fixture made only of third-party rows
    /// could not tell "the third-party repositories" from "the rows".
    #[test]
    fn a_check_asks_the_addressable_third_party_repositories_in_file_order() {
        let installed = vec![
            // Ours — the majority path. It must not be asked as a source.
            announcing("radio", Some("https://github.com/skerdudou/ritornello")),
            announcing("alpha", Some("https://github.com/a/alpha")),
            // Present and unaddressable: there is no GitHub endpoint to call
            // for it, so it must not consume a slot either.
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
        let targets = sources::source_targets(&installed, &[], &[]);
        let asked: Vec<&str> = targets.iter().map(|t| t.repo.as_str()).collect();
        assert_eq!(
            asked,
            vec!["a/alpha", "b/bravo", "c/charlie", "d/delta", "e/echo", "f/foxtrot"],
            "the addressable third-party repositories, in file order, and nothing else"
        );
        // The address is formed there and nowhere downstream, so that is
        // where the host a request goes to is decided.
        assert_eq!(
            targets[0].url,
            "https://api.github.com/repos/a/alpha/releases?per_page=100"
        );

        // **The limit bounds who is asked, never who counts as a stranger.**
        // The same fixture answered by `third_party_names`: all six, plus the
        // unaddressable one, and neither of the two that are ours or silent.
        // Conflating the two lists is what let a third-party plugin past the
        // limit — or one on GitLab — fall through to our own release under a
        // colliding name.
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
        let catalog = with(&["etc/ritornello/input-presets/theirs/default.toml"]);
        assert!(
            installable_from_ui(&catalog),
            "the plugin rule accepts an input preset — that is what makes this fixture the discriminating one"
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
            needs_companion: None,
            conflict_repos: None,
            contracts: Default::default(),
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
            // F1 of the whole-branch review: a language pack is no longer
            // excluded by kind. An already-installed pack with an update
            // available must be updated by the same nightly policy that
            // updates `radio` above -- excluding it here was a gap the spec
            // (§7.2, §10.4) never asked for, found alongside `install()`'s
            // own `Resolved::Nothing` misrouting.
            row("ritornello-lang-fr", ComponentKind::LanguagePack, Availability::UpdateAvailable),
            third_party,
            refused,
            silent,
        ];
        assert_eq!(
            automatic_install_list(&components, &nothing_placed(), schedule::InstallScope::Official),
            names(&["core", "radio", "ritornello-lang-fr"])
        );
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
            automatic_install_list(&rows, &placed::read(&night_one.staging), schedule::InstallScope::Official),
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
            automatic_install_list(&rows, &placed::read(&night_two.staging), schedule::InstallScope::Official),
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
            automatic_install_list(&rows, &memory, schedule::InstallScope::Official),
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
            automatic_install_list(&core_offered("0.2.0", "0.5.0"), &placed::read(&worker.staging), schedule::InstallScope::Official),
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
            automatic_install_list(&[console], &placed::read(&worker.staging), schedule::InstallScope::Official),
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
            automatic_install_list(&[console.clone()], &placed::read(&worker.staging), schedule::InstallScope::Official),
            names(&["console"]),
            "the release after the one that broke it is exactly where an automatic repair is worth most"
        );

        // And the archive that broke it is still refused, so the repair costs
        // one download per released version and never one per night.
        console.offered = Some("0.4.1".to_string());
        assert_eq!(
            automatic_install_list(&[console], &placed::read(&worker.staging), schedule::InstallScope::Official),
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
    /// a third-party plugin past the sources limit, a slow server, a GitLab fork — and not by
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
            sources: Vec::new(),
            fresh: Vec::new(),
            conflicts: Vec::new(),
            packs: Vec::new(),
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
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
                repo: "Someone/Radio-Fork".to_string(),
            }],
            third_party: names(&["radio"]),
            sources: Vec::new(),
            fresh: Vec::new(),
            conflicts: Vec::new(),
            packs: Vec::new(),
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
        };
        match resolve(&consulted, "radio") {
            Resolved::Theirs { published, repo } => {
                assert_eq!(
                    published.version, "2.0.0",
                    "its own repository answers, never the colliding official entry"
                );
                assert_eq!(repo, "Someone/Radio-Fork", "and it names that repository, as the row does");
            }
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
    /// automatic policy under the official-only scope.**
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
            automatic_install_list(&[theirs, mine], &nothing_placed(), schedule::InstallScope::Official),
            names(&["radio"]),
            "a third-party plugin is never installed under the official-only scope, even when its own repository offers a newer version"
        );
    }

    fn memory(entries: &[(&str, &str)]) -> placed::Placed {
        entries
            .iter()
            .map(|(key, version)| {
                (
                    key.to_string(),
                    placed::PlacedComponent { version: version.to_string(), not_installed_files: None },
                )
            })
            .collect()
    }

    fn third_party_row(name: &str, repo: &str) -> ComponentOffer {
        let mut c = row(name, ComponentKind::ThirdParty, Availability::UpdateAvailable);
        c.third_party_repo = Some(repo.to_string());
        c.offered = Some("2.0.0".to_string());
        c
    }

    /// The fourth policy's whole reach, and its edge: a third-party plugin is
    /// updated only when the scope says so, and the official row beside it is
    /// updated either way (without it an empty answer would pass).
    ///
    /// Each operand of the predicate has its own half: `Official` keeps the
    /// stranger out (so a filter that always lets it in fails), and
    /// `IncludingThirdParty` lets it in (so a filter that always keeps it out
    /// fails).
    #[test]
    fn a_third_party_plugin_is_updated_only_when_the_scope_includes_third_parties() {
        let theirs = third_party_row("someones-plugin", "someone/theirs");
        let mine = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        let rows = [theirs, mine];
        assert_eq!(
            automatic_install_list(&rows, &nothing_placed(), schedule::InstallScope::Official),
            names(&["radio"])
        );
        assert_eq!(
            automatic_install_list(&rows, &nothing_placed(), schedule::InstallScope::IncludingThirdParty),
            names(&["someones-plugin", "radio"])
        );
    }

    /// The same reach for a third-party **language pack**, with its row built
    /// the way the check builds it (`component_offers` over `pack_offers`,
    /// P6): an installed stranger's pack is updated only under the fourth
    /// policy, and our own pack of the same language either way.
    #[test]
    fn a_third_party_pack_is_updated_only_when_the_scope_includes_third_parties() {
        let pack = |version: &str| Published {
            offer: Offer::LanguagePack("fr".into()),
            version: version.into(),
            url: "https://x/p.tar.gz".into(),
            size: 0,
            release_tag: "t".into(),
            checksums_url: None,
            catalogue_url: None,
        };
        let answers = [sources::SourceAnswer { repo: "z/zed".into(), published: vec![pack("2.0.0")] }];
        let packs = sources::pack_offers(&[pack("0.3.0")], &answers);
        let theirs = crate::langpack::store::third_party_pack_id("fr", "z/zed");
        let ours = crate::langpack::store::pack_id("fr");
        let installed = vec![(theirs.clone(), "1.0.0".to_string()), (ours.clone(), "0.2.0".to_string())];
        let rows = component_offers("0.2.0", &[], &[], &[], &installed, &packs, &[], &[]);
        assert_eq!(
            automatic_install_list(&rows, &nothing_placed(), schedule::InstallScope::Official),
            vec![ours.clone()]
        );
        assert_eq!(
            automatic_install_list(&rows, &nothing_placed(), schedule::InstallScope::IncludingThirdParty),
            vec![ours, theirs]
        );
    }

    /// A row that carries a repository is third-party whatever its kind says:
    /// the filter keys on the repository, which is what a third-party language
    /// pack carries.
    #[test]
    fn a_row_with_a_repository_is_third_party_whatever_its_kind() {
        let mut odd = row("odd", ComponentKind::Plugin, Availability::UpdateAvailable);
        odd.third_party_repo = Some("someone/theirs".to_string());
        assert_eq!(
            automatic_install_list(&[odd.clone()], &nothing_placed(), schedule::InstallScope::Official),
            Vec::<String>::new()
        );
        assert_eq!(
            automatic_install_list(&[odd], &nothing_placed(), schedule::InstallScope::IncludingThirdParty),
            names(&["odd"])
        );
    }

    /// The placement memory of a third party is its own: offered == placed
    /// under the **namespaced** key excludes it, and a plain-name entry (one of
    /// ours, same name) does not.
    #[test]
    fn a_third_party_is_judged_by_its_namespaced_placement_only() {
        let scope = schedule::InstallScope::IncludingThirdParty;
        let theirs = third_party_row("radio", "someone/theirs");
        let key = third_party_placed_key("someone/theirs", "radio");
        assert_eq!(key, "third-party:someone/theirs:radio");
        // Tried and kept nothing: its own entry says this archive was placed.
        assert_eq!(
            automatic_install_list(std::slice::from_ref(&theirs), &memory(&[(&key, "2.0.0")]), scope),
            Vec::<String>::new()
        );
        // Our `radio` having been placed at that very version says nothing
        // about the stranger's.
        assert_eq!(
            automatic_install_list(std::slice::from_ref(&theirs), &memory(&[("radio", "2.0.0")]), scope),
            names(&["radio"])
        );
        // And an entry for another repository's `radio` does not shield it.
        let other = third_party_placed_key("someone/else", "radio");
        assert_eq!(
            automatic_install_list(&[theirs], &memory(&[(&other, "2.0.0")]), scope),
            names(&["radio"])
        );
    }

    /// The other direction of the collision: a stranger's placement does not
    /// shield one of ours that bears the same name.
    #[test]
    fn a_third_party_placement_does_not_shield_an_official_component_of_the_same_name() {
        let mine = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        let key = third_party_placed_key("someone/theirs", "radio");
        assert_eq!(
            automatic_install_list(
                std::slice::from_ref(&mine),
                &memory(&[(&key, "0.3.0")]),
                schedule::InstallScope::IncludingThirdParty
            ),
            names(&["radio"])
        );
        // Control: our own entry at the offered version does shield it.
        assert_eq!(
            automatic_install_list(
                &[mine],
                &memory(&[("radio", "0.3.0")]),
                schedule::InstallScope::IncludingThirdParty
            ),
            Vec::<String>::new()
        );
    }

    /// The "never announced a version" guard reads the same namespaced key:
    /// a third party this updater placed and that then died is repaired, one
    /// it never touched is left alone, and a same-named official placement
    /// does not count as its own.
    #[test]
    fn a_silent_third_party_is_admitted_by_its_own_placement_only() {
        let scope = schedule::InstallScope::IncludingThirdParty;
        let mut silent = third_party_row("radio", "someone/theirs");
        silent.installed = None;
        let key = third_party_placed_key("someone/theirs", "radio");
        assert_eq!(
            automatic_install_list(&[silent.clone()], &memory(&[(&key, "1.0.0")]), scope),
            names(&["radio"])
        );
        assert_eq!(
            automatic_install_list(&[silent.clone()], &nothing_placed(), scope),
            Vec::<String>::new()
        );
        assert_eq!(
            automatic_install_list(&[silent], &memory(&[("radio", "1.0.0")]), scope),
            Vec::<String>::new()
        );
        // And an official silent row is not admitted by a stranger's entry.
        let mut mine = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        mine.installed = None;
        assert_eq!(
            automatic_install_list(&[mine], &memory(&[(&key, "1.0.0")]), scope),
            Vec::<String>::new()
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

    /// The gap this closes: on a device's very **first** check, `previous` is
    /// empty, so `carry_installable` alone would leave a never-installed
    /// files row at `installable: None` — exactly what
    /// `InstallablesDialog.vue` reads as "show the Install button" and what
    /// `automatic_install_list`'s `installable != Some(false)` filter reads
    /// as "safe to install unattended". `deny_privileged_install` must answer
    /// `Some(false)` from the plugin's name alone, with no previous check to
    /// carry anything from.
    #[test]
    fn a_never_checked_privileged_plugin_is_still_refused() {
        let mut fresh = vec![row("files", ComponentKind::Plugin, Availability::NotInstalled)];
        fresh[0].declared = false;
        fresh[0].installable = None;
        deny_privileged_install(&mut fresh);
        assert_eq!(fresh[0].installable, Some(false));
    }

    /// A privileged plugin the device **declares** may be updated from the
    /// UI while its companion does not move, which `install_one` settles at
    /// the gesture. So the row keeps what it carried — `None` before any
    /// attempt, `Some(false)` after a refused one.
    ///
    /// **[MUTATION]**: drop `&& !row.declared` from `deny_privileged_install`
    /// — this test fails (the declared row is forced to `Some(false)`).
    #[test]
    fn a_declared_privileged_plugin_keeps_what_it_carried() {
        let mut fresh = vec![row("files", ComponentKind::Plugin, Availability::UpdateAvailable)];
        assert!(fresh[0].declared, "the fixture is the update of a declared plugin");
        deny_privileged_install(&mut fresh);
        assert_eq!(fresh[0].installable, None, "nothing is known before the archive is read");

        let mut previous = row("files", ComponentKind::Plugin, Availability::UpdateAvailable);
        previous.installable = Some(false);
        let mut fresh = vec![row("files", ComponentKind::Plugin, Availability::UpdateAvailable)];
        carry_installable(&[previous], &mut fresh);
        deny_privileged_install(&mut fresh);
        assert_eq!(fresh[0].installable, Some(false), "a refusal of this version is remembered");
    }

    /// The counterpart: an ordinary plugin is not touched by this rule at
    /// all, not even set to `Some(true)` -- `deny_privileged_install` answers
    /// nothing for a plugin it does not refuse, leaving whatever
    /// `carry_installable` or `component_offers` already decided in place.
    #[test]
    fn deny_privileged_install_leaves_an_ordinary_plugin_alone() {
        let mut fresh = vec![row("radio", ComponentKind::Plugin, Availability::UpdateAvailable)];
        deny_privileged_install(&mut fresh);
        assert_eq!(fresh[0].installable, None);
        // Undeclared too: an ordinary plugin the device does not have stays
        // installable from the UI. Without this half, dropping the
        // `is_privileged` operand (refusing every undeclared row) stayed green.
        let mut fresh = vec![row("radio", ComponentKind::Plugin, Availability::NotInstalled)];
        fresh[0].declared = false;
        deny_privileged_install(&mut fresh);
        assert_eq!(fresh[0].installable, None);
    }

    /// `carry_installable` runs first in every real call site and must not be
    /// allowed to win: a stale `previous` row (there should never be one, but
    /// the ordering is what guarantees it, not the data) must not un-refuse a
    /// privileged plugin the device does not declare.
    #[test]
    fn deny_privileged_install_overrides_whatever_carry_installable_set() {
        let mut previous = row("files", ComponentKind::Plugin, Availability::NotInstalled);
        previous.declared = false;
        previous.installable = Some(true);
        let mut fresh = vec![row("files", ComponentKind::Plugin, Availability::NotInstalled)];
        fresh[0].declared = false;
        carry_installable(&[previous], &mut fresh);
        assert_eq!(fresh[0].installable, Some(true), "carry_installable alone would leave this wrong");
        deny_privileged_install(&mut fresh);
        assert_eq!(fresh[0].installable, Some(false), "deny_privileged_install must win, called last");
    }

    // ---- A plugin with a companion: updated while the companion stays ----

    /// **[MUTATION]**: `offered == installed` replaced by `true` — red.
    #[test]
    fn a_companion_that_moved_refuses_the_update() {
        assert!(companion_allows(Some("0.2.0"), Some("0.2.0")));
        assert!(!companion_allows(Some("0.2.1"), Some("0.2.0")));
    }

    /// **[MUTATION]**: the catch-all arm answers `true` — red on both.
    #[test]
    fn an_unknown_companion_version_on_either_side_refuses_the_update() {
        assert!(!companion_allows(Some("0.2.0"), None), "installed version unknown");
        assert!(!companion_allows(None, Some("0.2.0")), "the release carries no companion");
        assert!(!companion_allows(None, None));
    }

    /// The offered version is read off the fold, for the plugin's own
    /// companion only — another companion listed first is not it.
    ///
    /// **[MUTATION]**: match any `Offer::Companion(_)` — red on `files`
    /// (the other companion's version). **[MUTATION]**: fall back on a
    /// companion for a plugin that has none — red on `radio`.
    #[test]
    fn the_offered_companion_is_the_plugin_s_own() {
        let companion = |name: &str, version: &str| Published {
            offer: Offer::Companion(name.to_string()),
            version: version.to_string(),
            url: String::new(),
            size: 0,
            release_tag: "v0.2.0-beta.2".to_string(),
            checksums_url: None,
            catalogue_url: None,
        };
        let ours = vec![companion("other-mount", "9.9.9"), companion("files-mount", "0.2.0-beta.2")];
        assert_eq!(companion_offered(&ours, "files"), Some("0.2.0-beta.2"));
        assert_eq!(companion_offered(&ours, "radio"), None);
        assert_eq!(companion_offered(&[], "files"), None);
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

    /// A release's input preset lands whole or not at all.
    ///
    /// The cheap proof that the write went through a `rename` rather than
    /// straight onto the target — the same shape the privileged crate uses
    /// for its own manifest: nothing named after the temporary survives, and
    /// the temporary is not the target. A truncated preset file is one that
    /// no longer parses, and a plugin that reads it falls back on its own
    /// defaults.
    #[test]
    fn an_input_preset_is_written_through_a_temporary_and_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("default.toml");
        write_atomic(&target, b"binding = \"play\"\n").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"binding = \"play\"\n");
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "default.toml")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// The French pack this repository ships, loaded as a real catalog rather
    /// than parsed as a table: what the test needs to know is what a French
    /// screen would actually receive.
    fn french() -> Chain {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales");
        Chain::load_for_tests("core", "fr", &root, crate::i18n::EN)
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
        let english = Chain::load_for_tests(
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
            Refusal::NeedsCompanionStep("files-mount"),
            Refusal::ThirdPartyArchive,
            Refusal::NotItsOwnFile("it is declared to run /a/b, and the archive carries c".to_string()),
            Refusal::NotItsOwnName("the archive carries zed, not ritornello-plugin-zed".to_string()),
            Refusal::LeftoverBinary("/usr/local/lib/ritornello/plugins/ritornello-plugin-zed".to_string()),
            Refusal::ThirdPartyUnchecked,
            Refusal::NoFragment,
            Refusal::Download("connection reset by peer".to_string()),
            Refusal::Prepare("no space left on device".to_string()),
            Refusal::Privileged("Job for ritornello-update.service failed".to_string()),
            Refusal::Pack("the archive of ritornello-lang-fr declares the language \"de\"".to_string()),
            Refusal::NothingPublished,
            Refusal::PluginsUnreadable,
            Refusal::NotConsented("z/zed".to_string()),
            Refusal::ContractsUnpublished,
            Refusal::GroupPostponed { failed: "mpd".to_string(), reason: Box::new(Refusal::DigestMismatch) },
        ];
        for catalog in [&english, &french()] {
            for why in &all {
                let message = refusal_message(catalog, "radio", why);
                // `Chain::get` answers the key itself when it knows none,
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

    /// **[MUTATION target — see `ritornello_i18n::interpolate`'s own
    /// `parameter_order_cannot_change_the_result`]**: `refusal_message`
    /// composes exactly two parameters, `component` then (conditionally)
    /// `detail`, and used to fold them with chained `.replace()` calls in
    /// that order — so a `component` that happened to contain the literal
    /// text `{detail}` got rewritten a second time by the `detail` pass,
    /// indistinguishable from the template's own placeholder. `component`
    /// here is a plugin name the core itself names, not user text, but the
    /// mechanism must hold regardless of who supplies the string: `detail`
    /// carries raw `systemctl`/tokio output, which is exactly the
    /// unconstrained kind of text this whole task exists to protect against.
    #[test]
    fn refusal_message_does_not_let_the_component_name_rewrite_the_detail_token() {
        let english =
            Chain::load_for_tests("core", "en", std::path::Path::new("/nonexistent"), crate::i18n::EN);
        let message = refusal_message(
            &english,
            "radio {detail}",
            &Refusal::Download("connection reset by peer".to_string()),
        );
        assert_eq!(message, "Could not download radio {detail}: connection reset by peer");
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
            catalog: Arc::new(RwLock::new(Chain::load_for_tests("core", "en", root, crate::i18n::EN))),
            status,
            manifest,
            plugins_tx: mpsc::channel(1).0,
            // The product default: finished releases only. A test that wants
            // the other channel writes to this handle — see
            // `asking_for_prereleases_is_what_makes_one_visible`.
            settings: Arc::new(RwLock::new(crate::state::Settings::default())),
            staging: root.join("staging"),
            root: root.to_path_buf(),
            plugin_data_root: root.join("plugins"),
            core_version: "0.2.0",
            restart: Arc::new(|| {}),
            // An empty registry: most tests in this module never install a
            // pack, so `Registry::installed_packs` answering nothing is the
            // correct fixture, not a shortcut. `pack_rig` below points
            // `packs_root` at this exact same directory, so a pack it
            // installs is exactly what a resweep of this registry finds.
            registry: Arc::new(RwLock::new(crate::i18n::seeded_registry(root.join("packs")))),
            packs_root: root.join("packs"),
            // Unused by most tests: a language pack test replaces this with
            // a channel it can `try_recv()` on — see `bare_pack_rig`.
            locale_tx: mpsc::channel(1).0,
            locale_current: Arc::new(RwLock::new(None)),
            update_sources: Arc::new(RwLock::new(Vec::new())),
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
            contracts: crate::status::core_contracts(),
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
        let checked = Checked { ours: vec![published], theirs: vec![], third_party: vec![], sources: vec![], fresh: vec![], conflicts: vec![], packs: vec![], plugins_unknown: false, contracts: ContractsByUrl::new() };
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
                let catalog = Chain::load_for_tests("core", "en", Path::new("/nonexistent"), crate::i18n::EN);
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

    /// **The in-flight mark is lifted on the skip path too**, which is the
    /// whole reason the erasure sits inside a wrapper rather than clearing the
    /// mark at each of its own six exits.
    ///
    /// The page probes this row for as long as the mark stands, so a path that
    /// returned without lifting it would leave the row reading "erasing…" for
    /// as long as the core runs, and probing for a change that can never come
    /// — the same lie as the button this replaced, only harder to notice. The
    /// skip is the right path to pin it on: it is the one refusal reachable
    /// without the privileged unit, so the test is fast and deterministic.
    #[tokio::test]
    async fn a_skipped_erasure_still_lifts_the_in_flight_mark() {
        let status = one_line(PluginStatus::kind("radio", "source", true, false));
        let (worker, _dir) = worker_rig(status);
        worker.state.write().await.mark_removal_pending("ritornello-plugin-radio");

        worker.remove_plugin_binary("orphan", "ritornello-plugin-radio").await;

        assert!(
            worker.state.read().await.pending_removals.is_empty(),
            "the erasure answered — by refusing — so nothing waits on it any more"
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
        let packs = sources::pack_offers(&published, &[]);
        Checked { ours: published, theirs: Vec::new(), third_party: Vec::new(), sources: Vec::new(), fresh: Vec::new(), conflicts: Vec::new(), packs, plugins_unknown: false, contracts: ContractsByUrl::new() }
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
            catalogue_url: None,
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
            catalogue_url: None,
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
            SEEN_REQUESTS.with(|seen| seen.borrow_mut().clear());
            Self
        }
    }

    impl Drop for Privileged {
        fn drop(&mut self) {
            FAKE_PRIVILEGED.with(|f| *f.borrow_mut() = None);
            SEEN_REQUESTS.with(|seen| seen.borrow_mut().clear());
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

    // ---- Language packs: `install_language`/`remove_language` -----------
    //
    // Built on `worker_at`, per Ruling C10 -- the same rig every other
    // install/refusal test above already uses, and the one that owns
    // `staging`. A second, parallel rig would let "nothing was staged" pass
    // by asserting against a directory nothing ever writes into.
    //
    // `install_language` takes the release's own fold (`Checked`) the same
    // way `install_one` takes a `Published` -- see `offered_packs`'s own doc.
    // `releases_url()` does have a test seam since task 14
    // (`release::TEST_RELEASES_URL_ENV`), but nothing below uses it: every
    // rig here builds the `Checked` its `install_language` call is handed
    // directly, the same way `served`/`served_core` build the `Published`
    // `install_one`'s own tests hand it, so none of these tests drives a
    // real `check()` either. That is a choice of scope, not a limitation of
    // the seam -- see the `Job::InstallLanguage` arm's own comment in
    // `run_worker` for what a test that did use it would look like.

    /// A worker (`worker_at`'s own rig), with the packs root task 8 gives it
    /// pointed at the same directory its `registry` already sweeps, and a
    /// fresh, four-deep locale channel a test can `try_recv()` on.
    fn bare_pack_rig() -> (Worker, tempfile::TempDir, mpsc::Receiver<String>) {
        let dir = tempfile::tempdir().unwrap();
        let mut worker = worker_at(dir.path(), stalled_line());
        let (locale_tx, locale_rx) = mpsc::channel(4);
        worker.locale_tx = locale_tx;
        worker.locale_current = Arc::new(RwLock::new(None));
        (worker, dir, locale_rx)
    }

    /// Everything `bare_pack_rig` built, plus the `Checked` the test's
    /// `install_language` call is handed.
    struct PackRig {
        worker: Worker,
        _dir: tempfile::TempDir,
        staging: PathBuf,
        packs_root: PathBuf,
        locale_current: Arc<RwLock<Option<String>>>,
        locale_rx: tokio::sync::Mutex<mpsc::Receiver<String>>,
        checked: Checked,
    }

    fn finish_pack_rig(
        worker: Worker,
        dir: tempfile::TempDir,
        locale_rx: mpsc::Receiver<String>,
        checked: Checked,
    ) -> PackRig {
        PackRig {
            staging: worker.staging.clone(),
            packs_root: worker.packs_root.clone(),
            locale_current: worker.locale_current.clone(),
            locale_rx: tokio::sync::Mutex::new(locale_rx),
            checked,
            worker,
            _dir: dir,
        }
    }

    /// A pack's `pack.toml`, built the way `contents` (in `langpack::store`'s
    /// own tests) builds one, but as raw text rather than a parsed
    /// `PackManifest`: these rigs go through the real archive bytes, not a
    /// value constructed in memory.
    fn pack_manifest_toml(language: &str, version: &str, modules: &[&str]) -> String {
        let list = modules.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(", ");
        format!(
            "language = {language:?}\nversion = {version:?}\nsource = \"https://github.com/skerdudou/ritornello\"\nmodules = [{list}]\n"
        )
    }

    /// A gzipped tar shaped like a real language pack archive: `pack.toml`
    /// plus one `<module>.toml` per entry of `modules`.
    fn pack_archive(modules: &[(&str, &str)], language: &str, version: &str) -> Vec<u8> {
        let names: Vec<&str> = modules.iter().map(|(m, _)| *m).collect();
        let manifest = pack_manifest_toml(language, version, &names);
        let file_names: Vec<String> = names.iter().map(|m| format!("{m}.toml")).collect();
        let mut entries: Vec<(&str, &[u8])> = vec![("pack.toml", manifest.as_bytes())];
        for (i, (_, body)) in modules.iter().enumerate() {
            entries.push((&file_names[i], body.as_bytes()));
        }
        targz(&entries)
    }

    /// A `Published` for the language pack `language`/`version`, its archive
    /// and its checksums served by two local, one-shot listeners -- the same
    /// rig `served`/`served_core` build for a plugin's or the core's own
    /// archive, extended to `Offer::LanguagePack` and to the pack naming
    /// convention (`pack_id`'s own doc: `ritornello-lang-fr-0.2.0.tar.gz`).
    async fn served_pack(language: &str, version: &str, archive: &[u8]) -> Published {
        let id = crate::langpack::store::pack_id(language);
        let file = format!("{id}-{version}.tar.gz");
        let url = serve_once(archive.to_vec(), &file).await;
        let sums = format!("{}  {file}\n", digest_hex(archive));
        let checksums_url = serve_once(sums.into_bytes(), "SHA256SUMS").await;
        Published {
            offer: Offer::LanguagePack(language.to_string()),
            version: version.to_string(),
            url,
            size: 0,
            release_tag: format!("v{version}"),
            checksums_url: Some(checksums_url),
            catalogue_url: None,
        }
    }

    /// A sound pack archive for `language`/`version`, covering exactly
    /// `modules`, offered and ready to install.
    async fn pack_rig(modules: &[(&str, &str)], language: &str, version: &str) -> PackRig {
        let archive = pack_archive(modules, language, version);
        let published = served_pack(language, version, &archive).await;
        let (worker, dir, locale_rx) = bare_pack_rig();
        finish_pack_rig(worker, dir, locale_rx, ours(vec![published]))
    }

    /// Like `pack_rig`, but the archive served is exactly `bytes` -- for a
    /// fixture that must be refused by the pack reader before a single file
    /// is written, rather than one built to be accepted.
    async fn pack_rig_with_bytes(bytes: Vec<u8>, language: &str, version: &str) -> PackRig {
        let published = served_pack(language, version, &bytes).await;
        let (worker, dir, locale_rx) = bare_pack_rig();
        finish_pack_rig(worker, dir, locale_rx, ours(vec![published]))
    }

    /// An archive shaped nothing like a language pack -- not gzip, not tar,
    /// the same "rate limited" body `something_that_is_not_a_gzipped_tar_
    /// is_refused_without_panicking` (`langpack::archive`'s own tests) already
    /// drives through the reader directly.
    fn bad_pack_archive() -> Vec<u8> {
        b"<html>rate limited</html>".to_vec()
    }

    /// A French pack (`fr`, `0.2.1`) whose checksums file names a digest that
    /// does not match its own archive -- so `install_language` reaches the
    /// real download and is refused at `verify_digest`, never at reading the
    /// archive at all. Mirrors `served_with_wrong_digest`.
    async fn pack_rig_with_wrong_digest() -> PackRig {
        let archive = pack_archive(&[("core", "k = \"v\"\n")], "fr", "0.2.1");
        let id = crate::langpack::store::pack_id("fr");
        let file = format!("{id}-0.2.1.tar.gz");
        let url = serve_once(archive, &file).await;
        let sums = format!("{}  {file}\n", digest_hex(b"not the archive's real bytes"));
        let checksums_url = serve_once(sums.into_bytes(), "SHA256SUMS").await;
        let published = Published {
            offer: Offer::LanguagePack("fr".to_string()),
            version: "0.2.1".to_string(),
            url,
            size: 0,
            release_tag: "v0.2.1".to_string(),
            checksums_url: Some(checksums_url),
            catalogue_url: None,
        };
        let (worker, dir, locale_rx) = bare_pack_rig();
        finish_pack_rig(worker, dir, locale_rx, ours(vec![published]))
    }

    /// A pack served **as** the offer for `"fr"` whose own `pack.toml`
    /// declares `"de"` -- the digest matches (it is computed over the real
    /// bytes), so this reaches the manifest-language check specifically,
    /// rather than being turned away earlier for an unrelated reason.
    async fn pack_rig_with_mismatched_language() -> PackRig {
        let archive = pack_archive(&[("core", "k = \"v\"\n")], "de", "0.2.1");
        let published = served_pack("fr", "0.2.1", &archive).await;
        let (worker, dir, locale_rx) = bare_pack_rig();
        finish_pack_rig(worker, dir, locale_rx, ours(vec![published]))
    }

    /// **The property this whole chantier turns on.** Installing a pack
    /// forms no privileged action at all: nothing is staged, no request.json
    /// is written, and the privileged unit is never asked to run. Driven
    /// from the staging directory, which every privileged path in this
    /// module writes into before it can do anything.
    #[tokio::test]
    async fn installing_a_pack_stages_nothing_and_asks_root_for_nothing() {
        let rig = pack_rig(&[("core", "standby = \"VEILLE\"\n")], "fr", "0.2.1").await;
        rig.worker.install_language(&rig.checked, "fr").await.expect("the pack installs");
        assert!(
            !rig.staging.join("request.json").exists(),
            "a pack must never form a privileged request"
        );
        assert!(rig.packs_root.join("ritornello-lang-fr/core.toml").exists());
    }

    /// **The defect between task 8 and task 9, proven end to end.** A
    /// regionalised language code (`pt-BR`) is accepted everywhere else in
    /// this product -- `valid_locale`, `ritornello_i18n::pack::valid_language`,
    /// and `classify_asset` all pass it -- so it must also install and
    /// uninstall through the real worker, not merely satisfy `valid_pack_id`
    /// in isolation. Before the fix this failed at `install_language`'s call
    /// into `langpack::store::install`, refused as "not a bare name" even
    /// though every earlier step (offer, download, digest, archive read,
    /// manifest-language check) had already accepted `pt-BR`.
    #[tokio::test]
    async fn installing_and_removing_a_regionalised_language_pack_round_trips() {
        let rig = pack_rig(&[("core", "standby = \"PARADO\"\n")], "pt-BR", "0.2.1").await;
        rig.worker.install_language(&rig.checked, "pt-BR").await.expect("the pack installs");
        assert!(rig.packs_root.join("ritornello-lang-pt-BR/core.toml").exists());

        rig.worker.remove_language("pt-BR").await;
        assert!(!rig.packs_root.join("ritornello-lang-pt-BR").exists());
    }

    /// **F1 of the whole-branch review.** A `LanguagePack` row reached
    /// through the generic `Job::Install`/`install()` path must actually
    /// install -- never fall through to `Resolved::Nothing` the way
    /// `carries`'s "not through this path" answer used to send it. Before
    /// the fix, this test's own assertions failed: `install()` left
    /// `state.outcome` reading `Failed("No release publishes
    /// ritornello-lang-fr…")` and wrote nothing under `packs_root`, even
    /// though `rig.checked` is the exact same offer `install_language`
    /// installs directly in the tests above.
    #[tokio::test]
    async fn a_language_pack_reaches_install_language_through_the_generic_install_job() {
        let rig = pack_rig(&[("core", "standby = \"VEILLE\"\n")], "fr", "0.2.1").await;
        let id = crate::langpack::store::pack_id("fr");

        rig.worker.install(&client().unwrap(), &rig.checked, &names(&[id.as_str()])).await;

        assert!(
            rig.packs_root.join("ritornello-lang-fr/core.toml").exists(),
            "the pack must actually be written to disk, not merely offered"
        );
        assert!(
            matches!(&rig.worker.state.read().await.outcome, CheckOutcome::Installed(_)),
            "a pack genuinely offered and installed must not read as a refusal, got {:?}",
            rig.worker.state.read().await.outcome
        );
    }

    /// **F1's mixed-gesture case.** `install_order` keeps the caller's own
    /// order for anything but the core, so before the fix a pack named
    /// ahead of a genuinely failing plugin let the pack's own **bogus**
    /// refusal win as `first_failure` (`install` keeps only the first) --
    /// masking the plugin's real, distinct cause. A device asking for both
    /// in one gesture would have read "nothing published carries
    /// ritornello-lang-fr" on the card: true of nothing, and silent about
    /// the plugin that actually failed to verify.
    ///
    /// Reused rather than invented: `mpd`'s digest-mismatch rig is the same
    /// `served_with_wrong_digest` the undeclared-binary test above already
    /// drives through `install()`, so this test adds only the pack half of
    /// the gesture.
    #[tokio::test]
    async fn a_failing_plugin_is_not_masked_by_a_language_pack_in_the_same_gesture() {
        let rig = pack_rig(&[("core", "standby = \"VEILLE\"\n")], "fr", "0.2.1").await;
        let id = crate::langpack::store::pack_id("fr");
        let bad_plugin = served_with_wrong_digest("mpd", &targz(&[("x", b"y")])).await;
        let mut checked = rig.checked;
        checked.ours.push(bad_plugin);

        rig.worker.install(&client().unwrap(), &checked, &names(&[id.as_str(), "mpd"])).await;

        assert!(
            rig.packs_root.join("ritornello-lang-fr/core.toml").exists(),
            "the pack must have installed silently -- it is not what failed"
        );
        let expected = {
            let catalog = rig.worker.catalog.read().await;
            refusal_message(&catalog, "mpd", &Refusal::DigestMismatch)
        };
        let outcome_message = match &rig.worker.state.read().await.outcome {
            CheckOutcome::Failed(message) => message.clone(),
            other => panic!("expected the plugin's own digest-mismatch refusal, got {other:?}"),
        };
        assert_eq!(
            outcome_message, expected,
            "the real refusal (mpd's digest mismatch) must surface, not a stale \
             'nothing published' about the pack that actually installed"
        );
    }

    /// **Task 14's own finding: the row a device just installed must say
    /// so.** `state.components`'s own copy of this pack's row is a
    /// snapshot `check()` takes once, before `install_language`/
    /// `remove_language` ever runs -- and neither of them otherwise
    /// touches it, so a page polling `/api/locale` for this exact field
    /// could poll forever without ever seeing the gesture settle. Found
    /// while writing the e2e journey (the one test that drives this path
    /// through a real `check()`), invisible to every test above: none of
    /// them reads this row, only the disk. Seeded here the way a real
    /// `check()` would have left it, since these rigs call
    /// `install_language`/`remove_language` directly rather than through a
    /// real `check()` -- see this suite's own header comment.
    #[tokio::test]
    async fn installing_and_removing_updates_this_packs_own_row_without_a_second_check() {
        let rig = pack_rig(&[("core", "k = \"v\"\n")], "fr", "0.2.1").await;
        let id = crate::langpack::store::pack_id("fr");
        rig.worker.state.write().await.components.push(ComponentOffer {
            name: id.clone(),
            kind: ComponentKind::LanguagePack,
            declared: false,
            binary_present: false,
            installed: None,
            offered: Some("0.2.1".to_string()),
            availability: Availability::NotInstalled,
            installable: Some(true),
            third_party_repo: None,
            not_installed_files: None,
            needs_companion: None,
            conflict_repos: None,
            contracts: Default::default(),
        });

        rig.worker.install_language(&rig.checked, "fr").await.expect("the pack installs");
        let row = |state: &UpdateState| state.components.iter().find(|c| c.name == id).cloned();
        let after_install = row(&*rig.worker.state.read().await).expect("the row still exists");
        assert_eq!(after_install.installed.as_deref(), Some("0.2.1"), "the row must say installed");
        assert_eq!(after_install.availability, Availability::Aligned);

        rig.worker.remove_language("fr").await;
        let after_remove = row(&*rig.worker.state.read().await).expect("the row still exists");
        assert_eq!(after_remove.installed, None, "the row must say not installed any more");
        assert_eq!(after_remove.availability, Availability::NotInstalled);
    }

    /// A refused archive writes nothing at all -- not the sound half of it,
    /// not an empty directory. The refusal names its cause on the card.
    #[tokio::test]
    async fn a_refused_pack_archive_leaves_the_disk_untouched() {
        let rig = pack_rig_with_bytes(bad_pack_archive(), "fr", "0.2.1").await;
        assert!(rig.worker.install_language(&rig.checked, "fr").await.is_err());
        assert!(!rig.packs_root.join("ritornello-lang-fr").exists());
    }

    /// A digest that does not match is refused before the archive is even
    /// read -- same order as install_one, for the same reason.
    #[tokio::test]
    async fn a_pack_whose_digest_does_not_match_is_refused_before_it_is_read() {
        let rig = pack_rig_with_wrong_digest().await;
        assert!(matches!(
            rig.worker.install_language(&rig.checked, "fr").await,
            Err((_, Refusal::DigestMismatch))
        ));
        assert!(!rig.packs_root.join("ritornello-lang-fr").exists());
    }

    /// The manifest's own language must agree with what was asked for -- an
    /// archive that says "de" under the French pack's id would otherwise
    /// install German files into `ritornello-lang-fr`. Step 5's mutation
    /// table asks for this test explicitly: no test drove this check before
    /// it was written.
    #[tokio::test]
    async fn a_pack_whose_manifest_names_a_different_language_is_refused() {
        let rig = pack_rig_with_mismatched_language().await;
        let err = rig
            .worker
            .install_language(&rig.checked, "fr")
            .await
            .expect_err("a language mismatch must be refused");
        assert!(matches!(err, (_, Refusal::Pack(_))), "{err:?}");
        assert!(!rig.packs_root.join("ritornello-lang-fr").exists());
    }

    // ---- Packs from any source (Task 7) -----------------------------------

    /// A sound one-module pack archive whose `pack.toml` names `source`.
    fn sourced_pack_archive(language: &str, version: &str, source: &str) -> Vec<u8> {
        let manifest =
            format!("language = {language:?}\nversion = {version:?}\nsource = {source:?}\nmodules = [\"core\"]\n");
        targz(&[("pack.toml", manifest.as_bytes()), ("core.toml", b"k = \"v\"\n")])
    }

    /// The offer a source `repo` makes for `language`, its archive served
    /// under the name a source publishes it (spec §3: the same
    /// `ritornello-lang-<language>-<version>.tar.gz` as ours).
    async fn third_party_offer(language: &str, version: &str, repo: &str, archive: &[u8]) -> sources::PackOffer {
        let published = served_pack(language, version, archive).await;
        let answers = [sources::SourceAnswer { repo: repo.to_string(), published: vec![published] }];
        sources::pack_offers(&[], &answers).remove(0)
    }

    /// Our offer for `language`, its archive being exactly `archive`.
    async fn official_offer(language: &str, version: &str, archive: &[u8]) -> sources::PackOffer {
        sources::pack_offers(&[served_pack(language, version, archive).await], &[]).remove(0)
    }

    /// `bare_pack_rig` on a device whose plugin has already announced, so an
    /// install pass that placed something does not wait `SETTLE_TIMEOUT`
    /// for a line that will never speak before it rebuilds the rows.
    fn settled_pack_rig() -> (Worker, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let line = PluginStatus { version: Some("0.2.0".to_string()), ..PluginStatus::kind("radio", "source", true, false) };
        (worker_at(dir.path(), one_line(line)), dir)
    }

    fn checked_with_packs(packs: Vec<sources::PackOffer>) -> Checked {
        Checked { packs, ..ours(Vec::new()) }
    }

    /// Nothing at all under the packs root: not a directory, not a file.
    fn nothing_under(root: &Path) -> bool {
        std::fs::read_dir(root).map(|mut d| d.next().is_none()).unwrap_or(true)
    }

    fn xlang(language: &str, repo: &str) -> String {
        crate::langpack::store::third_party_pack_id(language, repo)
    }

    /// Spec §4.2, Review Focus 5: a stranger's pack whose `pack.toml` names
    /// another source is refused before a byte is written.
    #[tokio::test]
    async fn a_third_party_pack_naming_another_source_is_refused_and_writes_nothing() {
        let archive = sourced_pack_archive("fr", "1.0.0", "https://github.com/other/repo");
        let offer = third_party_offer("fr", "1.0.0", "z/zed", &archive).await;
        let (worker, _dir) = settled_pack_rig();
        let err = worker.install_language(&checked_with_packs(vec![offer]), "fr").await.expect_err("refused");
        assert_eq!(err.0, xlang("fr", "z/zed"), "the refusal names the pack it concerns");
        assert!(matches!(err.1, Refusal::Pack(_)), "{err:?}");
        assert!(nothing_under(&worker.packs_root), "nothing is written under the packs root");
    }

    /// The other direction: our offer whose manifest names a stranger's
    /// repository is refused too — the check is not for strangers only.
    /// **[MUTATION]** skip the source check when `offer.repo` is `None`: red.
    #[tokio::test]
    async fn an_official_pack_naming_a_third_party_source_is_refused() {
        let archive = sourced_pack_archive("fr", "1.0.0", "https://github.com/z/zed");
        let offer = official_offer("fr", "1.0.0", &archive).await;
        let (worker, _dir) = settled_pack_rig();
        let err = worker.install_language(&checked_with_packs(vec![offer]), "fr").await.expect_err("refused");
        assert!(matches!(err.1, Refusal::Pack(_)), "{err:?}");
        assert!(nothing_under(&worker.packs_root));
    }

    /// P14: a `source` that does not read as a repository is no source at
    /// all, and never taken for ours. A bare `owner/repo` is exactly that
    /// shape (`parse_repo_url` wants the URL). **[MUTATION]** treat an
    /// unreadable source as ours: red.
    #[tokio::test]
    async fn a_pack_whose_source_does_not_read_is_refused_even_from_us() {
        let archive = sourced_pack_archive("fr", "1.0.0", "skerdudou/ritornello");
        let offer = official_offer("fr", "1.0.0", &archive).await;
        let (worker, _dir) = settled_pack_rig();
        let err = worker.install_language(&checked_with_packs(vec![offer]), "fr").await.expect_err("refused");
        assert!(matches!(err.1, Refusal::Pack(_)), "{err:?}");
        assert!(nothing_under(&worker.packs_root));
    }

    /// GitHub compares repositories without case, so a manifest naming
    /// `Z/Zed` for a pack fetched from `z/zed` is that source.
    /// **[MUTATION]** compare case-sensitively: red.
    #[tokio::test]
    async fn a_source_named_in_another_case_is_the_same_source() {
        let archive = sourced_pack_archive("fr", "1.0.0", "https://github.com/Z/Zed");
        let offer = third_party_offer("fr", "1.0.0", "z/zed", &archive).await;
        let (worker, _dir) = settled_pack_rig();
        worker.install_language(&checked_with_packs(vec![offer]), "fr").await.expect("installs");
        assert!(worker.packs_root.join(xlang("fr", "z/zed")).join("core.toml").exists());
    }

    /// Spec §5.2: one gesture installs every pack of the language, each in
    /// its own directory, and one gesture removes them all.
    #[tokio::test]
    async fn installing_a_language_installs_every_offered_pack_and_removing_it_removes_them_all() {
        let ours = official_offer("fr", "0.2.1", &sourced_pack_archive("fr", "0.2.1", "https://github.com/skerdudou/ritornello")).await;
        let theirs = third_party_offer("fr", "1.0.0", "z/zed", &sourced_pack_archive("fr", "1.0.0", "https://github.com/z/zed")).await;
        let (worker, _dir) = settled_pack_rig();
        worker.install_language(&checked_with_packs(vec![ours, theirs]), "fr").await.expect("both install");
        let ours_dir = worker.packs_root.join(crate::langpack::store::pack_id("fr"));
        let theirs_dir = worker.packs_root.join(xlang("fr", "z/zed"));
        assert!(ours_dir.join("core.toml").exists() && theirs_dir.join("core.toml").exists());
        assert_eq!(worker.registry.read().await.installed_packs().len(), 2, "both found again by the sweep");

        worker.remove_language("fr").await;
        assert!(!ours_dir.exists(), "ours removed");
        assert!(!theirs_dir.exists(), "theirs removed with it");
    }

    /// A language is one language whatever case a source spells it in:
    /// `pt-br` from a source and `pt-BR` from us install and go together.
    /// **[MUTATION]** compare case-sensitively in `offered_packs`, then in
    /// `remove_language`: red each time.
    #[tokio::test]
    async fn a_language_spelled_in_another_case_is_one_gesture() {
        let ours = official_offer("pt-BR", "0.2.1", &sourced_pack_archive("pt-BR", "0.2.1", "https://github.com/skerdudou/ritornello")).await;
        let theirs = third_party_offer("pt-br", "1.0.0", "z/zed", &sourced_pack_archive("pt-br", "1.0.0", "https://github.com/z/zed")).await;
        let (worker, _dir) = settled_pack_rig();
        worker.install_language(&checked_with_packs(vec![ours, theirs]), "pt-BR").await.expect("both install");
        let theirs_dir = worker.packs_root.join(xlang("pt-br", "z/zed"));
        assert!(theirs_dir.join("core.toml").exists(), "theirs installed in the same gesture");
        worker.remove_language("pt-BR").await;
        assert!(!theirs_dir.exists(), "and removed in the same gesture");
        assert!(!worker.packs_root.join(crate::langpack::store::pack_id("pt-BR")).exists());
    }

    /// A language's gesture is that language's only: installing one does not
    /// install a source's pack of another, and removing one leaves another
    /// language's pack alone.
    #[tokio::test]
    async fn a_language_gesture_never_touches_a_pack_of_another_language() {
        let de = third_party_offer("de", "1.0.0", "z/zed", &sourced_pack_archive("de", "1.0.0", "https://github.com/z/zed")).await;
        let fr = third_party_offer("fr", "1.0.0", "z/zed", &sourced_pack_archive("fr", "1.0.0", "https://github.com/z/zed")).await;
        let (worker, _dir) = settled_pack_rig();
        let checked = checked_with_packs(vec![de, fr]);
        worker.install_language(&checked, "de").await.unwrap();
        assert!(!worker.packs_root.join(xlang("fr", "z/zed")).exists(), "installing de installs no fr");
        worker.install_language(&checked, "fr").await.unwrap();
        worker.remove_language("fr").await;
        assert!(!worker.packs_root.join(xlang("fr", "z/zed")).exists());
        assert!(worker.packs_root.join(xlang("de", "z/zed")).exists());
    }

    /// One pack refused does not cancel its neighbours in the same gesture,
    /// and the refusal that comes back names the pack that was refused.
    #[tokio::test]
    async fn one_refused_pack_does_not_stop_the_others_of_its_language() {
        let liar = third_party_offer("fr", "1.0.0", "z/zed", &sourced_pack_archive("fr", "1.0.0", "https://github.com/x/y")).await;
        let ours = official_offer("fr", "0.2.1", &sourced_pack_archive("fr", "0.2.1", "https://github.com/skerdudou/ritornello")).await;
        let (worker, _dir) = settled_pack_rig();
        let err = worker.install_language(&checked_with_packs(vec![liar, ours]), "fr").await.expect_err("one refused");
        assert_eq!(err.0, xlang("fr", "z/zed"));
        assert!(worker.packs_root.join(crate::langpack::store::pack_id("fr")).join("core.toml").exists(), "ours still installed");
    }

    /// P1: the generic install job, given a third-party pack's id, installs
    /// that pack — routed by the checked offers, not by `language_of`, which
    /// knows only our ids.
    #[tokio::test]
    async fn the_install_job_installs_a_third_party_pack_by_its_id() {
        let offer = third_party_offer("pt-BR", "1.0.0", "z/zed", &sourced_pack_archive("pt-BR", "1.0.0", "https://github.com/z/zed")).await;
        let id = offer.id.clone();
        let (worker, _dir) = settled_pack_rig();
        worker.install(&client().unwrap(), &checked_with_packs(vec![offer]), &names(&[id.as_str()])).await;
        assert!(worker.packs_root.join(&id).join("core.toml").exists());
        assert!(
            matches!(&worker.state.read().await.outcome, CheckOutcome::Installed(_)),
            "{:?}",
            worker.state.read().await.outcome
        );
    }

    /// Puts a third-party pack on disk at `version`, as an earlier install
    /// would have, and lets the registry find it.
    async fn preinstall_third_party(worker: &Worker, language: &str, version: &str, repo: &str) {
        let archive = sourced_pack_archive(language, version, &format!("https://github.com/{repo}"));
        let contents = crate::langpack::archive::read(&archive, ritornello_i18n::MAX_BYTES).unwrap();
        crate::langpack::store::install(&worker.packs_root, &xlang(language, repo), &contents).unwrap();
        crate::i18n::Registry::resweep_async(&worker.registry).await;
    }

    /// Which half of a check settles the rows in `scheduled_run`.
    #[derive(Clone, Copy)]
    enum Branch {
        /// Our release list answered nothing for this channel — today's
        /// branch for a device on the stable channel (`OnlyPrereleases`).
        WithoutRelease,
        /// Our release list was read (here, with nothing of ours in it).
        WithRelease,
    }

    async fn settle(worker: &Worker, answers: Vec<sources::SourceAnswer>, branch: Branch) -> Checked {
        match branch {
            Branch::WithoutRelease => {
                worker.settle_without_release(&client().unwrap(), ReleasesError::OnlyPrereleases, Some(&[][..]), &[], answers).await
            }
            Branch::WithRelease => worker.settle_with_release(&client().unwrap(), Vec::new(), Some(&[][..]), &[], answers).await,
        }
    }

    /// D2: **the sources dialog's reports after a check that found no release
    /// of ours for this device** — the branch every stable-channel device
    /// takes today. Each source asked has its report: what an answering one
    /// published, and `answered: false` for a silent one.
    /// **[MUTATION]** drop the `source_reports` write in
    /// `settle_without_release`: red.
    #[tokio::test]
    async fn a_check_without_a_release_of_ours_still_reports_what_each_source_said() {
        let (worker, _dir) = settled_pack_rig();
        let targets = sources::source_targets(&[], &[], &["z/zed".to_string(), "b/bee".to_string()]);
        let pack = Published { offer: Offer::LanguagePack("fr".into()), ..stranger_plugin("x", "1.0.0") };
        let answers = vec![sources::SourceAnswer { repo: "z/zed".into(), published: vec![stranger_plugin("zed", "1.0.0"), pack] }];
        worker.settle_without_release(&client().unwrap(), ReleasesError::OnlyPrereleases, Some(&[][..]), &targets, answers).await;
        let state = worker.state.read().await;
        assert_eq!(state.outcome, CheckOutcome::OnlyPrereleases);
        assert_eq!(
            state.source_reports,
            vec![
                (
                    "z/zed".to_string(),
                    sources::SourceReport { answered: true, plugins: vec!["zed".into()], languages: vec!["fr".into()] },
                ),
                ("b/bee".to_string(), sources::SourceReport { answered: false, plugins: vec![], languages: vec![] }),
            ]
        );
    }

    /// B4, the other branch: a check that found no release of ours while
    /// `plugins.toml` was unreadable says so too, or an install after it
    /// would take every installed stranger for one of ours.
    /// **[MUTATION]** `plugins_unknown: false` in `settle_without_release`:
    /// red.
    #[tokio::test]
    async fn a_check_without_a_release_of_ours_still_knows_plugins_toml_was_unreadable() {
        let (worker, _dir) = settled_pack_rig();
        let unknown = worker.settle_without_release(&client().unwrap(), ReleasesError::NoRelease, None, &[], Vec::new()).await;
        assert!(unknown.plugins_unknown);
        let known = worker.settle_without_release(&client().unwrap(), ReleasesError::NoRelease, Some(&[][..]), &[], Vec::new()).await;
        assert!(!known.plugins_unknown);
    }

    /// One scheduled run under the fourth policy, minus the request to our
    /// own release list (`check` asks GitHub): the sources' answers settle
    /// the rows through `branch`, the automatic list is drawn from them, and
    /// what it names is installed.
    async fn scheduled_run(worker: &Worker, answers: Vec<sources::SourceAnswer>, branch: Branch) -> Vec<String> {
        let checked = settle(worker, answers, branch).await;
        let list = automatic_install_list(
            &worker.state.read().await.components,
            &placed::read(&worker.staging),
            schedule::InstallScope::IncludingThirdParty,
        );
        if !list.is_empty() {
            worker.install(&client().unwrap(), &checked, &list).await;
        }
        list
    }

    /// P1: the automatic policy reaches an **installed** third-party pack and
    /// updates it from its own source.
    #[tokio::test]
    async fn the_fourth_policy_updates_an_installed_third_party_pack() {
        let (worker, _dir) = settled_pack_rig();
        preinstall_third_party(&worker, "fr", "1.0.0", "z/zed").await;
        let published = served_pack("fr", "2.0.0", &sourced_pack_archive("fr", "2.0.0", "https://github.com/z/zed")).await;
        let answers = vec![sources::SourceAnswer { repo: "z/zed".into(), published: vec![published] }];

        assert_eq!(scheduled_run(&worker, answers, Branch::WithoutRelease).await, vec![xlang("fr", "z/zed")]);

        let registry = worker.registry.read().await;
        let pack = registry.installed_packs().iter().find(|p| p.id == xlang("fr", "z/zed")).expect("still there");
        assert_eq!(pack.manifest.version, "2.0.0");
    }

    /// A server that answers every connection with `body`, and counts them.
    async fn serve_counting(body: Vec<u8>, file: &str) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            while let Ok((mut socket, _)) = listener.accept().await {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut ignored = [0u8; 4096];
                let _ = socket.read(&mut ignored).await;
                let head = format!("HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://127.0.0.1:{port}/{file}"), hits)
    }

    /// P5: a third-party pack the reader refuses is downloaded **once** per
    /// offered version, not every night — its row is marked like a
    /// component's manual step, and the mark is carried by `(name, offered)`.
    /// A new version is a new archive and is tried again.
    /// **[MUTATION]** drop the mark in `install_pack`: two downloads, red on
    /// both branches. **[MUTATION]** drop `carry_installable` from
    /// `settle_without_release`: red on that branch — the one every stable
    /// device takes today.
    #[tokio::test]
    async fn a_refused_third_party_pack_is_downloaded_once_across_two_scheduled_runs() {
        refused_pack_is_fetched_once(Branch::WithoutRelease).await;
    }

    #[tokio::test]
    async fn a_refused_third_party_pack_is_downloaded_once_when_our_release_was_read() {
        refused_pack_is_fetched_once(Branch::WithRelease).await;
    }

    async fn refused_pack_is_fetched_once(branch: Branch) {
        let (worker, _dir) = settled_pack_rig();
        preinstall_third_party(&worker, "fr", "1.0.0", "z/zed").await;
        let archive = sourced_pack_archive("fr", "2.0.0", "https://github.com/someone/else");
        let file = "ritornello-lang-fr-2.0.0.tar.gz";
        let (url, downloads) = serve_counting(archive.clone(), file).await;
        let (checksums_url, _) = serve_counting(format!("{}  {file}\n", digest_hex(&archive)).into_bytes(), "SHA256SUMS").await;
        let offer = |version: &str| Published {
            offer: Offer::LanguagePack("fr".into()),
            version: version.into(),
            url: url.clone(),
            size: 0,
            release_tag: "v2.0.0".into(),
            checksums_url: Some(checksums_url.clone()),
            catalogue_url: None,
        };
        let answers = |version: &str| vec![sources::SourceAnswer { repo: "z/zed".into(), published: vec![offer(version)] }];
        let id = xlang("fr", "z/zed");

        assert_eq!(scheduled_run(&worker, answers("2.0.0"), branch).await, vec![id.clone()], "first night: tried");
        assert_eq!(scheduled_run(&worker, answers("2.0.0"), branch).await, Vec::<String>::new(), "second night: not again");
        assert_eq!(downloads.load(std::sync::atomic::Ordering::SeqCst), 1, "one download in two nights");
        let row = worker.state.read().await.components.iter().find(|c| c.name == id).cloned().unwrap();
        assert_eq!(row.installable, Some(false));

        // A new version resets it: the mark belongs to the archive refused.
        settle(&worker, answers("2.0.1"), branch).await;
        let list = automatic_install_list(
            &worker.state.read().await.components,
            &placed::read(&worker.staging),
            schedule::InstallScope::IncludingThirdParty,
        );
        assert_eq!(list, vec![id]);
    }

    /// The check asks the source an installed third-party pack names, as it
    /// asks a plugin's: `targets_now` reads the registry.
    #[tokio::test]
    async fn the_check_asks_the_source_of_an_installed_third_party_pack() {
        let (worker, _dir) = settled_pack_rig();
        preinstall_third_party(&worker, "fr", "1.0.0", "z/zed").await;
        let targets = worker.targets_now(&[]).await;
        assert_eq!(targets.iter().map(|t| t.repo.as_str()).collect::<Vec<_>>(), vec!["z/zed"]);
    }

    /// §7.3: removing the pack of the language in use sends the interface
    /// back to English, and the stored choice goes with it -- so the device
    /// does not slip back into that language on its own the day a pack for
    /// it reappears.
    #[tokio::test]
    async fn removing_the_pack_in_use_sends_the_device_back_to_english() {
        let rig = pack_rig(&[("core", "k = \"v\"\n")], "fr", "0.2.1").await;
        rig.worker.install_language(&rig.checked, "fr").await.unwrap();
        *rig.locale_current.write().await = Some("fr".to_string());
        rig.worker.remove_language("fr").await;
        assert_eq!(rig.locale_rx.lock().await.try_recv().ok(), Some("en".to_string()));
    }

    /// The mirror, and it is the half that is easy to get wrong: removing
    /// some OTHER language's pack must not touch the chosen one.
    #[tokio::test]
    async fn removing_another_language_leaves_the_chosen_one_alone() {
        let rig = pack_rig(&[("core", "k = \"v\"\n")], "de", "0.2.1").await;
        rig.worker.install_language(&rig.checked, "de").await.unwrap();
        *rig.locale_current.write().await = Some("fr".to_string());
        rig.worker.remove_language("de").await;
        assert!(rig.locale_rx.lock().await.try_recv().is_err(), "nothing was sent");
    }

    /// The one place this task's `interpolate` deviation (see the doc on
    /// `remove_language`'s `Err` arm) could leave a literal `{detail}` or
    /// `{component}` token on screen: `every_refusal_is_a_translated_
    /// sentence_with_its_parameters_filled_in` only drives the install path's
    /// own `Refusal::Pack` through `refusal_message`, never this method's
    /// own `Err` arm. `"x/y"` makes `pack_id` produce an id that is not a
    /// bare name, so `store::remove` refuses it before touching disk, and
    /// the refusal's own sentence is what `remove_language` must resolve
    /// cleanly.
    #[tokio::test]
    async fn a_refused_removal_reaches_the_page_with_every_parameter_filled_in() {
        let rig = pack_rig(&[("core", "k = \"v\"\n")], "fr", "0.2.1").await;
        rig.worker.remove_language("x/y").await;
        match &rig.worker.state.read().await.outcome {
            CheckOutcome::Failed(message) => {
                assert!(!message.contains('{'), "a parameter was left unfilled: {message}");
                assert!(
                    message.contains("ritornello-lang-x/y"),
                    "the message must name the id, not merely be non-empty: {message}"
                );
            }
            other => panic!("a refused removal must reach the page, not only the log: {other:?}"),
        }
    }

    /// **The wiring itself, not only the method it calls.** `run_worker`'s
    /// `Job::RemoveLanguage` arm must actually reach `remove_language` — this
    /// drives it through the real loop rather than only through a direct
    /// call, the same distinction task 18's review drew for `install()`.
    /// `Job::InstallLanguage` is not driven the same way here, and has no
    /// test of its own: its own arm always opens a real `check()` first
    /// (task 9's own brief: "the worker is the only thing that has read the
    /// release"), which needs `releases_url()` — the same call `offered_packs`
    /// takes an already-performed `Checked` to avoid repeating. A test that
    /// only constructed and cloned the value, without driving the loop, was
    /// tried and measured to prove nothing (gutting the arm to a no-op left
    /// it green) and was removed rather than kept as decoration. `check()`'s
    /// list endpoint has had a test seam since task 14
    /// (`release::TEST_RELEASES_URL_ENV`); the arm's own comment in
    /// `run_worker` says what a test built on it would need to do, and that
    /// none has been written.
    #[tokio::test]
    async fn job_remove_language_reaches_remove_language_through_the_worker_loop() {
        let rig = pack_rig(&[("core", "k = \"v\"\n")], "fr", "0.2.1").await;
        rig.worker.install_language(&rig.checked, "fr").await.unwrap();
        *rig.locale_current.write().await = Some("fr".to_string());
        let PackRig { worker, locale_rx, packs_root, .. } = rig;
        let mut locale_rx = locale_rx.into_inner();

        let (tx, rx) = mpsc::channel(4);
        let handle = tokio::spawn(run_worker(worker, rx));
        tx.send(Job::RemoveLanguage("fr".to_string())).await.unwrap();
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("run_worker hung")
            .expect("run_worker panicked");

        assert!(!packs_root.join("ritornello-lang-fr").exists());
        assert_eq!(locale_rx.try_recv().ok(), Some("en".to_string()));
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
            sources: Vec::new(),
            fresh: Vec::new(),
            conflicts: Vec::new(),
            packs: Vec::new(),
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
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
            sources: Vec::new(),
            fresh: Vec::new(),
            conflicts: Vec::new(),
            packs: Vec::new(),
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
        };

        assert!(
            memory_at_the_exit(&mut worker, &checked).await.is_some(),
            "a manual install of the version the automatic policy skips must still reach the restart"
        );
    }

    /// B4 corrected: **the core's own update is not refused while
    /// `plugins.toml` is unreadable** — it may be what repairs the device —
    /// while a plugin still is, under the same check. Two gestures, because
    /// a core placement ends the pass (the process leaves) before any
    /// outcome is written: radio alone is refused by name; the core alone
    /// goes all the way to its restart.
    /// **[MUTATION]** drop `&& name != CORE`: red on the restart.
    /// **[MUTATION]** drop the whole guard: red on radio's refusal.
    #[tokio::test]
    async fn an_unreadable_plugins_toml_still_lets_the_core_update_itself_and_refuses_a_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let mut worker = worker_at(dir.path(), stalled_line());
        let _privileged = Privileged::answers(Ok(()));
        let mut published = vec![served_core(&core_archive()).await];
        published.extend(radio_published("0.3.0"));
        let checked = Checked { plugins_unknown: true, ..ours(published) };

        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install(&client().unwrap(), &checked, &names(&["radio"])),
        )
        .await
        .expect("the install pass hung");
        let expected = refusal_message(&*worker.catalog.read().await, "radio", &Refusal::PluginsUnreadable);
        assert_eq!(
            worker.state.read().await.outcome,
            CheckOutcome::Failed(expected),
            "radio refused for the unreadable file, not resolved to our archive"
        );

        assert!(
            memory_at_the_exit(&mut worker, &checked).await.is_some(),
            "the core went past the gate, all the way to its restart"
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
            catalogue_url: None,
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

    /// A source to ask at a local address, named as a `SourceTarget` names
    /// it (lowercased `owner/repo`).
    fn local_target(repo: &str, url: String) -> sources::SourceTarget {
        sources::SourceTarget { repo: repo.to_string(), url }
    }

    /// One installed plugin per `(name, owner/repo)`, each announcing its
    /// repository the way a real status line does.
    fn announcing_each(list: &[(&str, &str)]) -> Vec<Installed> {
        list.iter()
            .map(|(name, repo)| announcing(name, Some(&format!("https://github.com/{repo}"))))
            .collect()
    }

    /// **The last decision on the path that lets bytes arrive from a
    /// repository we do not control, and the one that had no test.**
    ///
    /// Six repositories answering six different ways, driven through the
    /// real `fetch_text` against real sockets by the real sweep, and the
    /// offers derived from those answers by the real `theirs_from`. Two
    /// properties in one run, because they are one loop:
    ///
    /// - **an asset must be named for the plugin it is being fetched for.** A
    ///   repository publishing `ritornello-plugin-radio-…` when it is
    ///   `bravo`'s has published nothing for the binary on this device, and a
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
            local_target(
                "b/bravo",
                serve_once(releases_body(&[&asset_for("radio", "3.0.0")]), "releases").await,
            ),
            // Answers properly.
            local_target(
                "a/alpha",
                serve_once(releases_body(&[&asset_for("alpha", "2.0.0")]), "releases").await,
            ),
            // Nothing listening at all.
            local_target("c/charlie", refused_url().await),
            // Rate-limited — and its body is a **perfectly good** release
            // listing naming delta's own asset. Deliberately: an error body
            // that also failed to parse would be refused by the next clause
            // for a different reason, and the fixture could then not tell the
            // status check from its absence. What this pins is that the
            // **status** decides, not the bytes that came with it.
            local_target(
                "d/delta",
                serve_with(403, releases_body(&[&asset_for("delta", "6.0.0")]), "releases").await,
            ),
            // 200, and a body that is not a release list.
            local_target("e/echo", serve_once(b"<html>not json</html>".to_vec(), "releases").await),
            // Publishes a CORE archive: a stranger must never offer one.
            local_target(
                "f/foxtrot",
                serve_once(
                    releases_body(&[
                        &format!("ritornello-core-4.0.0-{ARCH}.tar.gz"),
                        &asset_for("foxtrot", "5.1.0"),
                    ]),
                    "releases",
                )
                .await,
            ),
        ];
        let installed = announcing_each(&[
            ("bravo", "b/bravo"),
            ("alpha", "a/alpha"),
            ("charlie", "c/charlie"),
            ("delta", "d/delta"),
            ("echo", "e/echo"),
            ("foxtrot", "f/foxtrot"),
        ]);

        let answers = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.sweep_sources(&client, &targets, sources::SOURCES_DEADLINE),
        )
        .await
        .expect("the sweep hung");
        let offers = theirs_from(&installed, &answers);

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
        // And the three failures are absent from the answers themselves, not
        // merely filtered out later: the report says "did not answer".
        assert_eq!(
            answers.iter().map(|a| a.repo.as_str()).collect::<Vec<_>>(),
            vec!["b/bravo", "a/alpha", "f/foxtrot"],
        );
    }

    /// **The switch is what makes a prerelease visible, and it is read on the
    /// path rather than remembered.**
    ///
    /// Two runs of the same worker over two repositories — one publishing a
    /// finished release, one publishing a prerelease — with nothing changed
    /// between them but `update_prereleases`. Real sockets, the real
    /// `fetch_text`, the real `sweep_sources` — the very call the check makes,
    /// which reads the setting itself.
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
        async fn two_repositories() -> Vec<sources::SourceTarget> {
            vec![
                local_target(
                    "a/alpha",
                    serve_once(releases_body(&[&asset_for("alpha", "2.0.0")]), "releases").await,
                ),
                local_target(
                    "b/bravo",
                    serve_once(prerelease_body(&[&asset_for("bravo", "3.0.0-beta.1")]), "releases")
                        .await,
                ),
            ]
        }

        async fn sweep(worker: &Worker, client: &reqwest::Client) -> Vec<(String, String)> {
            let targets = two_repositories().await;
            let answers = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                worker.sweep_sources(client, &targets, sources::SOURCES_DEADLINE),
            )
            .await
            .expect("the sweep hung");
            let installed = announcing_each(&[("alpha", "a/alpha"), ("bravo", "b/bravo")]);
            theirs_from(&installed, &answers)
                .into_iter()
                .map(|o| (o.name, o.published.version))
                .collect()
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

    /// **One deadline, on real sockets.** Two repositories answer, a third
    /// accepts the connection into its backlog and never answers it — the
    /// shape of a host that is up and stuck, which no status code produces.
    /// The sweep must come back with the two answers once the (short,
    /// injected) deadline falls, not after the request's own 60 s timeout.
    ///
    /// A real clock, deliberately: a paused one would jump to the deadline
    /// while the two live sockets are still idle on I/O. The outer timeout is
    /// what turns a sweep that lost its deadline into a red assertion.
    #[tokio::test]
    async fn a_stuck_host_on_a_real_socket_costs_the_deadline_and_no_more() {
        let (worker, _dir) = worker_rig(starting_line());
        let client = client().unwrap();
        let stuck = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stuck_url = format!("http://127.0.0.1:{}/releases", stuck.local_addr().unwrap().port());
        let targets = vec![
            local_target(
                "a/alpha",
                serve_once(releases_body(&[&asset_for("alpha", "2.0.0")]), "releases").await,
            ),
            local_target("s/stuck", stuck_url),
            // Answers a release list with nothing in it: answered, offering
            // nothing — present, and not to be confused with silence.
            local_target("e/empty", serve_once(b"[]".to_vec(), "releases").await),
        ];
        let deadline = std::time::Duration::from_secs(2);
        let started = std::time::Instant::now();
        let answers = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            worker.sweep_sources(&client, &targets, deadline),
        )
        .await
        .expect("the sweep outlived its deadline: the stuck host held it");
        assert!(started.elapsed() >= deadline, "the stuck host was waited for until the deadline");
        assert_eq!(
            answers.iter().map(|a| (a.repo.as_str(), a.published.len())).collect::<Vec<_>>(),
            vec![("a/alpha", 1), ("e/empty", 0)],
        );
        assert_eq!(
            sources::reports_of(&targets, &answers)
                .into_iter()
                .map(|(repo, r)| (repo, r.answered))
                .collect::<Vec<_>>(),
            vec![("a/alpha".to_string(), true), ("s/stuck".to_string(), false), ("e/empty".to_string(), true)],
        );
        drop(stuck);
    }

    /// **A check replaces the per-source report, it does not add to it.** The
    /// sources a check asks are GitHub addresses this test cannot reach, so it
    /// drives the other half: a report left by an earlier check, for a source
    /// the device no longer reads, must be gone after the next one — or the
    /// page would go on saying what a removed repository offered. With no
    /// source to ask, the check also returns without waiting for anything.
    #[tokio::test]
    async fn a_check_records_what_each_source_answered() {
        let status = one_line(PluginStatus {
            version: Some("0.2.0".into()),
            repository: Some("https://github.com/skerdudou/ritornello".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        });
        let (worker, _dir) = worker_rig(status);
        worker.state.write().await.source_reports = vec![(
            "gone/repo".to_string(),
            sources::SourceReport { answered: true, plugins: vec!["zed".into()], languages: vec![] },
        )];
        let body = releases_body(&[&asset_for("radio", "0.3.0")]);
        let releases = parse_releases(std::str::from_utf8(&body).unwrap(), Channel::Stable).unwrap();
        let checked = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.settle_check(&client().unwrap(), &releases),
        )
        .await
        .expect("settle_check hung")
        .expect("a check over a readable release list");
        assert!(checked.sources.is_empty());
        assert_eq!(worker.state.read().await.source_reports, Vec::new(), "the earlier report must not survive");
    }

    /// **Only the repository a plugin announced answers for it.** An operator-
    /// added source publishing an archive under an installed plugin's name is
    /// not that plugin's repository, and must not become its update — that
    /// would let any added repository replace any installed binary by naming
    /// it. The announcement is matched whatever its case, and the offer
    /// carries the repository spelled as the row spells it, which is the
    /// placement memory's key.
    #[test]
    fn a_plugin_is_answered_by_its_own_repository_and_no_other() {
        let installed = vec![
            announcing("zed", Some("https://github.com/Z/Zed")),
            announcing("radio", Some("https://github.com/skerdudou/ritornello")),
        ];
        let answer = |repo: &str, name: &str, version: &str| sources::SourceAnswer {
            repo: repo.to_string(),
            published: vec![Published {
                offer: Offer::Plugin(name.to_string()),
                version: version.to_string(),
                url: "https://x/a".to_string(),
                size: 1,
                release_tag: "v1".to_string(),
                checksums_url: None,
                catalogue_url: None,
            }],
        };
        // An added source offers zed (and radio), and comes first.
        let answers = vec![answer("o/other", "zed", "9.9.9"), answer("o/other", "radio", "9.9.9")];
        assert!(
            theirs_from(&installed, &answers).is_empty(),
            "a repository nobody announced answers for no installed plugin"
        );
        let answers = vec![answer("o/other", "zed", "9.9.9"), answer("z/zed", "zed", "1.2.0")];
        let offers = theirs_from(&installed, &answers);
        assert_eq!(
            offers.iter().map(|o| (o.name.as_str(), o.published.version.as_str(), o.repo.as_str())).collect::<Vec<_>>(),
            vec![("zed", "1.2.0", "Z/Zed")],
        );
    }

    /// A plugin offer for `name` at `version`, as a stranger's fold would
    /// carry it.
    fn stranger_plugin(name: &str, version: &str) -> Published {
        Published {
            offer: Offer::Plugin(name.to_string()),
            version: version.to_string(),
            url: format!("https://x/{name}"),
            size: 1,
            release_tag: "v1".to_string(),
            checksums_url: Some("https://x/SHA256SUMS".to_string()),
            catalogue_url: None,
        }
    }

    fn source_answer(repo: &str, published: Vec<Published>) -> sources::SourceAnswer {
        sources::SourceAnswer { repo: repo.to_string(), published }
    }

    /// A check over our release (publishing `radio`) and three sources: a
    /// fork republishing `radio`, a lone `zed`, and `dup` offered twice —
    /// judged the way `settle_with_release` judges them.
    fn checked_with_strangers() -> Checked {
        let mut checked = Checked {
            ours: radio_published("0.3.0"),
            theirs: Vec::new(),
            third_party: Vec::new(),
            sources: vec![
                source_answer("evil/fork", vec![stranger_plugin("radio", "9.9.9"), stranger_plugin("dup", "1.0.0")]),
                source_answer("z/zed", vec![stranger_plugin("zed", "1.0.0")]),
                source_answer("a/one", vec![stranger_plugin("dup", "2.0.0")]),
            ],
            fresh: Vec::new(),
            conflicts: Vec::new(),
            packs: Vec::new(),
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
        };
        checked.judge_strangers(&[]);
        checked
    }

    /// The three answers `resolve` owes the ownership rule: a fresh name is
    /// the stranger's to offer, a contested one is nobody's, and ours stays
    /// ours whoever else publishes it.
    #[test]
    fn resolve_offers_a_fresh_name_from_its_source_and_nothing_for_a_contested_one() {
        let checked = checked_with_strangers();
        match resolve(&checked, "zed") {
            Resolved::FreshTheirs { published, repo } => {
                assert_eq!((published.version.as_str(), repo), ("1.0.0", "z/zed"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(resolve(&checked, "dup"), Resolved::Nothing, "a contested name is installed from nobody");
        match resolve(&checked, "radio") {
            Resolved::Ours(published) => assert_eq!(published.version, "0.3.0", "never the fork's 9.9.9"),
            other => panic!("{other:?}"),
        }
    }

    /// **The order inside `resolve`.** An installed third-party `zed` whose
    /// own repository did not answer, and another source offering `zed`
    /// fresh. `fresh_offers` never produces that pair (an installed name is
    /// owned), so the `Checked` is built by hand: what is pinned is that
    /// `resolve` does not lean on that construction.
    ///
    /// **[MUTATION]** move the fresh lookup before the unchecked guard: red.
    #[test]
    fn an_unchecked_third_party_is_never_answered_by_a_fresh_offer_of_its_name() {
        let checked = Checked {
            ours: Vec::new(),
            theirs: Vec::new(),
            third_party: names(&["zed"]),
            sources: Vec::new(),
            fresh: vec![sources::FreshOffer {
                name: "zed".to_string(),
                repo: "other/zed".to_string(),
                published: stranger_plugin("zed", "6.6.6"),
            }],
            conflicts: Vec::new(),
            packs: Vec::new(),
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
        };
        assert_eq!(resolve(&checked, "zed"), Resolved::UncheckedThirdParty);
    }

    /// **The rows a first check writes**, through `settle_with_release`: a
    /// fresh row naming its source, and a contested row that stays
    /// `installable: Some(false)` — on a device's first check there is no
    /// earlier row for `carry_installable` to read, and it must not erase
    /// what `component_offers` stated.
    ///
    /// **[MUTATION]** drop the `conflict_repos` filter in `carry_installable`:
    /// red. **[MUTATION]** drop the `judge_strangers` call: red.
    #[tokio::test]
    async fn a_check_with_a_release_writes_fresh_and_contested_rows() {
        let (worker, _dir) = worker_rig(starting_line());
        let installed: Vec<Installed> = Vec::new();
        let answers = vec![
            source_answer("z/zed", vec![stranger_plugin("zed", "1.0.0")]),
            source_answer("b/two", vec![stranger_plugin("dup", "1.0.0")]),
            source_answer("a/one", vec![stranger_plugin("dup", "2.0.0")]),
        ];
        let checked = worker.settle_with_release(&client().unwrap(), radio_published("0.3.0"), Some(&installed), &[], answers).await;
        assert_eq!(checked.fresh.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["zed"]);
        let state = worker.state.read().await;
        let zed = state.components.iter().find(|c| c.name == "zed").expect("a fresh row");
        assert_eq!((zed.offered.as_deref(), zed.third_party_repo.as_deref()), (Some("1.0.0"), Some("z/zed")));
        let dup = state.components.iter().find(|c| c.name == "dup").expect("a contested row");
        assert_eq!(dup.installable, Some(false));
        assert_eq!(dup.conflict_repos, Some(vec!["a/one".to_string(), "b/two".to_string()]));
        // What `GET /api/update/catalogue?repo=` may select: the one source
        // with a fresh offer, for that name alone.
        // **[MUTATION]** drop the `source_catalogues` write: red.
        let catalogues: Vec<(&str, &[String])> =
            state.source_catalogues.iter().map(|c| (c.repo.as_str(), c.names.as_slice())).collect();
        assert_eq!(catalogues, vec![("z/zed", &["zed".to_string()][..])]);
    }

    /// **Preflight ruling P4.** A device on the stable channel while only
    /// prereleases are published: our release list yields no fold, so which
    /// names are ours is unknown, and a source publishing `radio` must not
    /// be offered as `radio`'s owner. A lone `zed` is not offered either —
    /// ownership cannot be judged at all without our fold, so nothing is.
    ///
    /// **[MUTATION]** call `judge_strangers` in `settle_without_release`:
    /// red.
    #[tokio::test]
    async fn a_stranger_is_offered_nothing_fresh_while_our_release_list_is_unread() {
        let (worker, _dir) = worker_rig(starting_line());
        let answers = vec![
            source_answer("evil/fork", vec![stranger_plugin("radio", "9.9.9")]),
            source_answer("z/zed", vec![stranger_plugin("zed", "1.0.0")]),
        ];
        let checked = worker.settle_without_release(&client().unwrap(), ReleasesError::OnlyPrereleases, Some(&[][..]), &[], answers).await;
        assert!(checked.fresh.is_empty() && checked.conflicts.is_empty(), "{:?} {:?}", checked.fresh, checked.conflicts);
        assert_eq!(resolve(&checked, "radio"), Resolved::Nothing);
        let state = worker.state.read().await;
        assert_eq!(state.outcome, CheckOutcome::OnlyPrereleases, "the branch this test means to drive");
        assert!(
            state.components.iter().all(|c| c.name != "radio" && c.name != "zed"),
            "{:#?}",
            state.components
        );
    }

    /// **The other list ownership is judged against.** `plugins.toml`
    /// unreadable: the device's plugins are unknown, so an installed
    /// third-party `zed` is unknown too, and a source publishing `zed` must
    /// not be offered as its owner. Two halves: `installed` says "unknown"
    /// rather than "nothing", and the check then judges no stranger.
    ///
    /// **[MUTATION]** `installed` answering `Some(Vec::new())` on a read
    /// error: red. **[MUTATION]** judging against an empty list when
    /// unknown: red.
    #[tokio::test]
    async fn a_stranger_is_offered_nothing_fresh_while_plugins_toml_is_unreadable() {
        let (worker, _dir) = worker_rig(starting_line());
        std::fs::write(&worker.manifest, "[[plugin\nthis is not toml").unwrap();
        assert_eq!(worker.installed().await, None, "unreadable is not empty");
        assert!(worker.installed_when_settled().await.is_empty(), "every other caller reads it as before");

        let answers = vec![source_answer("evil/zed", vec![stranger_plugin("zed", "9.9.9")])];
        let checked = worker.settle_with_release(&client().unwrap(), radio_published("0.3.0"), None, &[], answers).await;
        assert!(checked.fresh.is_empty() && checked.conflicts.is_empty(), "{:?} {:?}", checked.fresh, checked.conflicts);
        assert_eq!(resolve(&checked, "zed"), Resolved::Nothing);
        assert!(worker.state.read().await.components.iter().all(|c| c.name != "zed"));
    }

    // ---- Task 6: a third-party plugin installed from scratch -------------

    /// The plugins directory a device really has, for the pure tests.
    const DEVICE_PLUGINS: &str = "/usr/local/lib/ritornello/plugins";

    #[test]
    fn a_fresh_third_party_block_names_the_plugin_and_its_placed_binary() {
        let dir = Path::new(DEVICE_PLUGINS);
        let block = third_party_fragment("zed", "ritornello-plugin-zed", dir).unwrap();
        let edited = crate::plugins::edit::append_block("", &block, "zed")
            .expect("append_block accepts what we synthesise");
        assert!(edited.contains("name = \"zed\""), "{edited}");
        assert!(
            edited.contains("exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-zed\""),
            "{edited}"
        );
        // Nothing else: no `enabled`, no option a stranger could have chosen.
        let manifest: PluginManifest = toml::from_str(&edited).unwrap();
        assert_eq!(manifest.plugins.len(), 1);
        assert!(manifest.plugins[0].enabled);
    }

    /// Each case isolates **one** operand of the predicate where it can, so
    /// dropping that operand alone turns this test red (see the mutation
    /// table in the task report):
    ///
    /// - exact file equality: `("zed", "zed")` — `zed` is a valid bare name
    ///   and not reserved, and `component_name_from_file("zed")` answers
    ///   `zed`, which is why that function cannot be the predicate
    ///   (preflight ruling P2); also `radio`'s file, `zed2`, `../`;
    /// - the file a valid bare name: a name of 60 characters is valid and
    ///   free, but its file is 78 characters, past `valid_name`'s 64;
    /// - the name a valid bare name: `-zed` starts with a dash, while
    ///   `ritornello-plugin--zed` is a valid file (`reserved` also refuses
    ///   it, since it holds `!valid_name` itself — two guards on purpose);
    /// - not reserved: `files` (a companion's plugin), `files-mount`,
    ///   `core`, and a pack id.
    #[test]
    fn a_fresh_third_party_block_refuses_a_binary_named_for_someone_else() {
        let dir = Path::new(DEVICE_PLUGINS);
        let long = "z".repeat(60);
        let long_file = format!("ritornello-plugin-{long}");
        let cases: Vec<(&str, &str)> = vec![
            ("zed", "ritornello-plugin-radio"),
            ("zed", "zed"),
            ("zed", "ritornello-plugin-zed2"),
            ("zed", "../ritornello-plugin-zed"),
            ("Zed", "ritornello-plugin-Zed"),
            (long.as_str(), long_file.as_str()),
            ("-zed", "ritornello-plugin--zed"),
            ("files", "ritornello-plugin-files"),
            ("files-mount", "ritornello-plugin-files-mount"),
            ("core", "ritornello-plugin-core"),
            ("ritornello-lang-fr", "ritornello-plugin-ritornello-lang-fr"),
            ("ritornello-xlang-fr-0123456789ab", "ritornello-plugin-ritornello-xlang-fr-0123456789ab"),
        ];
        for (name, file) in cases {
            assert!(
                matches!(third_party_fragment(name, file, dir), Err(Refusal::NotItsOwnName(_))),
                "{name} / {file}"
            );
        }
        // Control: the same predicate accepts the one honest shape, at the
        // longest name whose file still fits.
        let fits = "z".repeat(64 - "ritornello-plugin-".len());
        assert!(third_party_fragment(&fits, &format!("ritornello-plugin-{fits}"), dir).is_ok());
    }

    /// A status line that has finished speaking, so an install pass's
    /// `installed_when_settled` does not wait out its fifteen seconds.
    fn announced_radio() -> Arc<RwLock<StatusState>> {
        one_line(PluginStatus {
            version: Some("0.2.0".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        })
    }

    /// A stranger's archive as the strict rule wants it: its own binary,
    /// nothing else.
    fn zed_archive() -> Vec<u8> {
        targz(&[("usr/local/lib/ritornello/plugins/ritornello-plugin-zed", b"ZED")])
    }

    /// **The block written is the core's, byte for byte**, built from the
    /// offered name and the device's plugins directory. Driven through the
    /// real `install_one`: the request root is asked to carry out names the
    /// plugin's own file, and the placement is remembered under the
    /// source's namespaced key, so the nightly bound holds from the first
    /// update on.
    #[tokio::test]
    async fn install_one_declares_a_fresh_third_party_plugin_with_the_block_the_core_synthesises() {
        let (worker, dir) = worker_rig(announced_radio());
        let _privileged = Privileged::answers(Ok(()));
        let before = std::fs::read_to_string(&worker.manifest).unwrap();
        let published = served("zed", &zed_archive()).await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client().unwrap(), "zed", &published, Provenance::Fresh, Some("z/zed"), None),
        )
        .await
        .expect("install_one hung");
        assert!(matches!(outcome, Ok(Placed::NewPlugin)), "{:?}", outcome.as_ref().err());

        let block = third_party_fragment("zed", "ritornello-plugin-zed", &plugins_dir(dir.path())).unwrap();
        let expected = crate::plugins::edit::append_block(&before, &block, "zed").unwrap();
        assert_eq!(std::fs::read_to_string(&worker.manifest).unwrap(), expected);

        let request: Request =
            serde_json::from_str(&std::fs::read_to_string(worker.staging.join("request.json")).unwrap()).unwrap();
        assert!(
            matches!(&request.actions[..], [Action::PlacePlugin { file, .. }] if file == "ritornello-plugin-zed"),
            "{request:?}"
        );
        let memory = placed::read(&worker.staging);
        assert_eq!(memory.keys().cloned().collect::<Vec<_>>(), vec![third_party_placed_key("z/zed", "zed")]);
    }

    /// Spec §3: `SHA256SUMS` is mandatory. An offer with no checksums file is
    /// refused before a byte is downloaded, and `plugins.toml` is untouched.
    #[tokio::test]
    async fn install_one_refuses_a_fresh_third_party_offer_without_a_checksums_file() {
        let (worker, _dir) = worker_rig(announced_radio());
        let _privileged = Privileged::answers(Ok(()));
        let before = std::fs::read_to_string(&worker.manifest).unwrap();
        let mut published = served("zed", &zed_archive()).await;
        published.checksums_url = None;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client().unwrap(), "zed", &published, Provenance::Fresh, Some("z/zed"), None),
        )
        .await
        .expect("install_one hung");
        assert!(matches!(outcome, Err(Refusal::NoDigest)), "{:?}", outcome.as_ref().err());
        assert_eq!(std::fs::read_to_string(&worker.manifest).unwrap(), before);
        assert!(!worker.staging.join("request.json").exists());
    }

    /// An archive bringing its own `[[plugin]]` block — here one that would
    /// run a file outside the plugins directory — is refused whole, and
    /// nothing is written: neither the block, nor the core's own, nor a
    /// request for root.
    #[tokio::test]
    async fn install_one_refuses_a_fresh_archive_that_brings_its_own_block() {
        let (worker, _dir) = worker_rig(announced_radio());
        let _privileged = Privileged::answers(Ok(()));
        let before = std::fs::read_to_string(&worker.manifest).unwrap();
        // At the archive's root, where `archive::read` takes a fragment from
        // (`archive::FRAGMENT_NAME`) — proven below, so the fixture really
        // carries a block and not merely an extra file.
        let archive = targz(&[
            ("usr/local/lib/ritornello/plugins/ritornello-plugin-zed", b"ZED"),
            ("./plugins.toml.fragment", b"[[plugin]]\nname = \"zed\"\nexec = \"/tmp/evil\"\n"),
        ]);
        assert!(
            archive::read(&archive, DECOMPRESSED_MAX).unwrap().fragment.is_some_and(|f| f.contains("/tmp/evil")),
            "the fixture carries a block the reader sees"
        );
        let published = served("zed", &archive).await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client().unwrap(), "zed", &published, Provenance::Fresh, Some("z/zed"), None),
        )
        .await
        .expect("install_one hung");
        // The file first: under a mutation that lets the archive through,
        // what goes red must be the block it wrote.
        assert_eq!(std::fs::read_to_string(&worker.manifest).unwrap(), before);
        assert!(matches!(outcome, Err(Refusal::ThirdPartyArchive)), "{:?}", outcome.as_ref().err());
        assert!(!worker.staging.join("request.json").exists());
    }

    /// A check offering one fresh plugin, `zed`, from `z/zed`, at `published`.
    fn checked_with_fresh(published: Published) -> Checked {
        Checked {
            ours: radio_published("0.3.0"),
            theirs: Vec::new(),
            third_party: Vec::new(),
            sources: Vec::new(),
            fresh: vec![sources::FreshOffer { name: "zed".to_string(), repo: "z/zed".to_string(), published }],
            conflicts: Vec::new(),
            packs: Vec::new(),
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
        }
    }

    /// **From the gesture**, not from the method: `install` routes a fresh
    /// offer to `install_one` with its source, and the page reads a first
    /// installation.
    #[tokio::test]
    async fn installing_a_fresh_offer_declares_it_and_remembers_it_under_its_source() {
        let (worker, _dir) = worker_rig(announced_radio());
        let _privileged = Privileged::answers(Ok(()));
        let checked = checked_with_fresh(served("zed", &zed_archive()).await);
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            // Confirmed under another spelling of the same repository: the
            // comparison is GitHub's, case-insensitive.
            worker.install_consented(&client().unwrap(), &checked, &names(&["zed"]), &zed_from("Z/Zed")),
        )
        .await
        .expect("install() hung");
        let catalog = Chain::load_for_tests("core", "en", Path::new("/nonexistent"), crate::i18n::EN);
        assert_eq!(
            worker.state.read().await.outcome,
            install_report(&catalog, &[Placement { component: "zed".into(), version: "2.0.0".into(), fresh: true }], None)
                .unwrap()
        );
        assert!(worker.declared("zed"));
        assert!(placed::read(&worker.staging).contains_key(&third_party_placed_key("z/zed", "zed")));
    }

    /// **The check's verdict is re-asked at the gesture.** Between the check
    /// that offered `zed` and the click, the name or its file may have been
    /// taken; installing then would replace someone's plugin with a
    /// stranger's binary — the one thing a fresh offer must never do. Five
    /// ways it can be taken, each refused by name, before any download (the
    /// offer's URL is never served) and with nothing written. A name taken
    /// under another case counts, as it does for `sources::fresh_offers`;
    /// a binary already on disk gets the sentence that says how to clear it.
    #[tokio::test]
    async fn installing_a_fresh_offer_is_refused_once_its_name_or_file_is_taken() {
        enum Taken {
            Declared,
            DeclaredOtherCase,
            ExecOfAnother,
            FileOnDisk,
            ManifestUnreadable,
        }
        for taken in [
            Taken::Declared,
            Taken::DeclaredOtherCase,
            Taken::ExecOfAnother,
            Taken::FileOnDisk,
            Taken::ManifestUnreadable,
        ] {
            let (worker, dir) = worker_rig(announced_radio());
            let _privileged = Privileged::answers(Ok(()));
            let zed_file = plugins_dir(dir.path()).join("ritornello-plugin-zed");
            let mut manifest = std::fs::read_to_string(&worker.manifest).unwrap();
            let catalog = Chain::load_for_tests("core", "en", Path::new("/nonexistent"), crate::i18n::EN);
            let leftover = matches!(taken, Taken::FileOnDisk)
                .then(|| refusal_message(&catalog, "zed", &Refusal::LeftoverBinary(zed_file.display().to_string())));
            match taken {
                Taken::Declared => {
                    manifest.push_str("\n[[plugin]]\nname = \"zed\"\nexec = \"/opt/zed\"\n");
                }
                Taken::DeclaredOtherCase => {
                    manifest.push_str("\n[[plugin]]\nname = \"Zed\"\nexec = \"/opt/zed\"\n");
                }
                Taken::ExecOfAnother => {
                    manifest.push_str(&format!("\n[[plugin]]\nname = \"myzed\"\nexec = {:?}\n", zed_file.to_string_lossy()));
                }
                Taken::FileOnDisk => std::fs::write(&zed_file, b"SOMEONE ELSE'S").unwrap(),
                Taken::ManifestUnreadable => manifest = "[[plugin\nnot toml".to_string(),
            }
            std::fs::write(&worker.manifest, &manifest).unwrap();
            let checked = checked_with_fresh(stranger_plugin("zed", "1.0.0"));
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                worker.install_consented(&client().unwrap(), &checked, &names(&["zed"]), &zed_from("z/zed")),
            )
            .await
            .expect("install() hung");
            let outcome = worker.state.read().await.outcome.clone();
            let CheckOutcome::Failed(message) = &outcome else { panic!("{outcome:?}") };
            match leftover {
                Some(expected) => assert_eq!(message, &expected),
                None => {
                    let head = refusal_message(&catalog, "zed", &Refusal::NotItsOwnName(String::new()));
                    assert!(message.starts_with(head.trim_end_matches(|c: char| !c.is_alphanumeric())), "{message}");
                }
            }
            assert_eq!(std::fs::read_to_string(&worker.manifest).unwrap(), manifest);
            assert!(!worker.staging.join("request.json").exists());
        }
    }

    /// The consent the page sends for `zed`: `[("zed", repo)]`.
    fn zed_from(repo: &str) -> Vec<(String, String)> {
        vec![("zed".to_string(), repo.to_string())]
    }

    /// B3: **a fresh offer installs only from the repository the second
    /// consent named.** The check at the gesture finds `zed` offered by
    /// `z/zed`; an install confirmed for `b/bee` (the source that offered it
    /// when the operator clicked, removed since), one confirmed for another
    /// name, and one confirmed for nothing are each refused by name, before
    /// any download (the offer's URL is never served) and with nothing
    /// written. **[MUTATION]** drop the repository comparison: red on
    /// `b/bee`. **[MUTATION]** drop the name comparison: red on `other`.
    /// **[MUTATION]** drop the whole guard: red on all three.
    #[tokio::test]
    async fn a_fresh_offer_is_refused_unless_its_own_repository_was_confirmed() {
        let catalog = Chain::load_for_tests("core", "en", Path::new("/nonexistent"), crate::i18n::EN);
        let expected = refusal_message(&catalog, "zed", &Refusal::NotConsented("z/zed".into()));
        let confirmations: [(&str, Vec<(String, String)>); 3] = [
            ("another repository", zed_from("b/bee")),
            ("another name", vec![("other".to_string(), "z/zed".to_string())]),
            ("nothing confirmed", Vec::new()),
        ];
        for (case, consented) in confirmations {
            let (worker, _dir) = worker_rig(announced_radio());
            let _privileged = Privileged::answers(Ok(()));
            let before = std::fs::read_to_string(&worker.manifest).unwrap();
            let checked = checked_with_fresh(stranger_plugin("zed", "1.0.0"));
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                worker.install_consented(&client().unwrap(), &checked, &names(&["zed"]), &consented),
            )
            .await
            .expect("install() hung");
            assert_eq!(worker.state.read().await.outcome, CheckOutcome::Failed(expected.clone()), "{case}");
            assert_eq!(std::fs::read_to_string(&worker.manifest).unwrap(), before, "{case}");
            assert!(!worker.staging.join("request.json").exists(), "{case}");
        }
    }

    /// B4: **an unreadable `plugins.toml` installs nothing but a pack.** The
    /// check could not tell whose plugin a name is, so `radio` — which might
    /// be an installed fork's — is refused by name rather than resolved to
    /// our archive, while a language pack, whose id carries its source,
    /// still installs. Both asked in one gesture, through the check's own
    /// settle (`known: None`), so the flag is the one a real check sets.
    /// **[MUTATION]** drop the `plugins_unknown` guard in `install_consented`:
    /// red on the outcome. **[MUTATION]** set `plugins_unknown: false` in
    /// `settle_with_release`: red too.
    #[tokio::test]
    async fn an_unreadable_plugins_toml_installs_nothing_but_a_pack() {
        let rig = pack_rig(&[("core", "k = \"v\"\n")], "fr", "0.2.0").await;
        let mut published = rig.checked.ours.clone();
        published.extend(radio_published("0.3.0"));
        let checked = rig.worker.settle_with_release(&client().unwrap(), published, None, &[], Vec::new()).await;
        assert!(checked.plugins_unknown);
        let id = crate::langpack::store::pack_id("fr");
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            rig.worker.install(&client().unwrap(), &checked, &names(&[id.as_str(), "radio"])),
        )
        .await
        .expect("install() hung");
        let expected = refusal_message(&*rig.worker.catalog.read().await, "radio", &Refusal::PluginsUnreadable);
        assert_eq!(rig.worker.state.read().await.outcome, CheckOutcome::Failed(expected));
        assert!(!rig.staging.join("request.json").exists(), "nothing asked of root");
        assert!(rig.packs_root.join(&id).join("core.toml").exists(), "the pack placed");
    }

    /// **Only a fresh offer is declared by the core.** A `Theirs` update
    /// finding no block for its plugin — the file unreadable, or the block
    /// removed by the operator since the check — is refused with
    /// `NoFragment` before anything is written, as it was before fresh
    /// installs existed: placing the binary first would be a
    /// replace-then-fail on an unreadable file, and re-declaring would undo
    /// the operator's own gesture, without a second consent under the
    /// automatic policy (spec §4.5).
    ///
    /// **[MUTATION]** synthesise for any third party (`provenance !=
    /// Provenance::Ours`) instead of `== Provenance::Fresh`: both red.
    #[tokio::test]
    async fn a_theirs_update_finding_no_block_is_refused_before_any_write() {
        for unreadable in [true, false] {
            let (worker, dir) = worker_rig(announced_radio());
            let _privileged = Privileged::answers(Ok(()));
            let manifest = if unreadable {
                "[[plugin\nnot toml".to_string()
            } else {
                let other = plugins_dir(dir.path()).join("ritornello-plugin-other");
                format!("[[plugin]]\nname = \"other\"\nexec = {:?}\n", other.to_string_lossy())
            };
            std::fs::write(&worker.manifest, &manifest).unwrap();
            let archive = targz(&[("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"THEIRS")]);
            let published = served("radio", &archive).await;
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                worker.install_one(&client().unwrap(), "radio", &published, Provenance::Theirs, Some("someone/radio"), None),
            )
            .await
            .expect("install_one hung");
            assert!(matches!(outcome, Err(Refusal::NoFragment)), "unreadable={unreadable}: {:?}", outcome.as_ref().err());
            assert!(!worker.staging.join("request.json").exists(), "unreadable={unreadable}: root was asked to place it");
            assert_eq!(std::fs::read_to_string(&worker.manifest).unwrap(), manifest, "unreadable={unreadable}");
        }
    }

    /// **Ownership re-asked inside `install_one`**, after the download:
    /// `install` asked before downloading, and `zed` became declared in
    /// between — here with the very `exec` the core would have written, so
    /// `placement_target` has nothing to object to. Refused by name, nothing
    /// placed, nothing written.
    ///
    /// **[MUTATION]** drop the `still_unowned` call in `install_one`: red.
    #[tokio::test]
    async fn a_fresh_offer_declared_since_the_gesture_began_is_refused_before_any_write() {
        let (worker, dir) = worker_rig(announced_radio());
        let _privileged = Privileged::answers(Ok(()));
        let zed_file = plugins_dir(dir.path()).join("ritornello-plugin-zed");
        let mut manifest = std::fs::read_to_string(&worker.manifest).unwrap();
        manifest.push_str(&format!("\n[[plugin]]\nname = \"zed\"\nexec = {:?}\n", zed_file.to_string_lossy()));
        std::fs::write(&worker.manifest, &manifest).unwrap();
        let published = served("zed", &zed_archive()).await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client().unwrap(), "zed", &published, Provenance::Fresh, Some("z/zed"), None),
        )
        .await
        .expect("install_one hung");
        assert!(matches!(outcome, Err(Refusal::NotItsOwnName(_))), "{:?}", outcome.as_ref().err());
        assert!(!worker.staging.join("request.json").exists());
        assert_eq!(std::fs::read_to_string(&worker.manifest).unwrap(), manifest);
    }

    /// **Second consent (spec §4.5).** The first installation of a fresh
    /// third-party plugin is always a gesture: even under the policy that
    /// installs third parties, and even with a placement remembered under
    /// its key (a plugin installed once and since removed), the row is
    /// `NotInstalled` and the robot leaves it alone.
    ///
    /// **[MUTATION]** drop the `UpdateAvailable` filter of
    /// `automatic_install_list`: red.
    #[tokio::test]
    async fn the_automatic_policy_never_installs_a_fresh_offer() {
        let (worker, _dir) = worker_rig(announced_radio());
        let answers = vec![source_answer("z/zed", vec![stranger_plugin("zed", "1.0.0")])];
        let nothing: Vec<Installed> = Vec::new();
        let checked = worker.settle_with_release(&client().unwrap(), radio_published("0.3.0"), Some(&nothing), &[], answers).await;
        assert_eq!(checked.fresh.len(), 1, "the fixture offers zed fresh");
        let state = worker.state.read().await;
        let zed = state.components.iter().find(|c| c.name == "zed").expect("a fresh row");
        assert_eq!(zed.availability, Availability::NotInstalled);
        let key = third_party_placed_key("z/zed", "zed");
        for memory in [nothing_placed(), memory(&[(&key, "0.9.0")])] {
            let list = automatic_install_list(
                &state.components,
                &memory,
                schedule::InstallScope::IncludingThirdParty,
            );
            assert!(!list.contains(&"zed".to_string()), "{list:?}");
        }
    }

    /// The placement key is one spelling per repository: a fresh install
    /// remembers its source lowercased (`sources::fresh_offers`), and the
    /// plugin's later updates read it under the repository as it announces
    /// it, whose case is its author's.
    #[test]
    fn a_third_party_placement_key_ignores_the_repository_s_case() {
        assert_eq!(third_party_placed_key("Z/Zed", "zed"), third_party_placed_key("z/zed", "zed"));
    }

    /// **RULING 64 at the call site: the third-party path really does call the
    /// stricter rule.**
    ///
    /// The archive carries its binary **and an input preset** — a shape
    /// `installable_from_ui` accepts, asserted here so the fixture is proven to
    /// be the discriminating one. Deleting the `archive_allowed` guard in
    /// `install_one` makes this red, and what goes red is not only the outcome:
    /// the mutated path writes the stranger's preset under `/etc/ritornello`
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
            ("etc/ritornello/input-presets/radio/default.toml", b"a = \"b\"\n"),
        ]);
        assert!(
            installable_from_ui(&archive::read(&archive, DECOMPRESSED_MAX).unwrap().entries),
            "our own rule accepts this archive — that is what makes it the discriminating fixture"
        );
        let published = served("radio", &archive).await;
        let client = client().unwrap();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client, "radio", &published, Provenance::Theirs, None, None),
        )
        .await
        .expect("install_one hung");
        assert!(
            matches!(outcome, Err(Refusal::ThirdPartyArchive)),
            "expected ThirdPartyArchive, got {:?}",
            outcome.as_ref().err()
        );
        assert!(
            !dir.path().join("etc/ritornello/input-presets/radio/default.toml").exists(),
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
            worker.install_one(&client, "theirs", &published, Provenance::Theirs, None, None),
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
        let targets = worker.targets_now(&installed).await;
        assert_eq!(
            targets.iter().map(|t| t.repo.as_str()).collect::<Vec<_>>(),
            vec!["someone/their-plugin"],
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
            worker.targets_now(&installed).await.is_empty(),
            "an official plugin must never send the core asking a stranger's repository about it"
        );
    }

    /// **The check reads the operator's list.** An added source is asked after
    /// the announced one, in its stored form normalised, and the handle is
    /// read as it is at check time — not as it was at construction.
    #[tokio::test]
    async fn the_check_asks_the_operator_s_added_sources_after_the_announced_ones() {
        let status = one_line(PluginStatus {
            version: Some("1.4.0".into()),
            repository: Some("https://github.com/someone/their-plugin".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        });
        let (worker, _dir) = worker_rig(status);
        let installed = worker.installed_when_settled().await;
        *worker.update_sources.write().await = vec!["Z/Added".to_string()];
        let targets = worker.targets_now(&installed).await;
        assert_eq!(
            targets.iter().map(|t| t.repo.as_str()).collect::<Vec<_>>(),
            vec!["someone/their-plugin", "z/added"],
            "the announced repository first, then the operator's own"
        );
    }

    /// The sixteen-source ceiling, through the check's own path: seventeen
    /// added sources ask sixteen, and they are the first sixteen.
    #[tokio::test]
    async fn the_check_asks_no_more_than_sixteen_sources() {
        let (worker, _dir) = worker_rig(one_line(PluginStatus {
            version: Some("0.2.0".into()),
            repository: Some("https://github.com/skerdudou/ritornello".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        }));
        let installed = worker.installed_when_settled().await;
        *worker.update_sources.write().await = (0..17).map(|i| format!("o/r{i:02}")).collect();
        let targets = worker.targets_now(&installed).await;
        assert_eq!(targets.len(), sources::SOURCES_MAX);
        assert_eq!(targets.last().map(|t| t.repo.as_str()), Some("o/r15"));
    }

    fn radio_published(version: &str) -> Vec<Published> {
        vec![Published {
            offer: Offer::Plugin("radio".to_string()),
            version: version.to_string(),
            url: format!("https://x/ritornello-plugin-radio-{version}-x86_64.tar.gz"),
            size: 0,
            release_tag: format!("v{version}"),
            checksums_url: None,
            catalogue_url: None,
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
        let english = Chain::load_for_tests(
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

    /// Same defect as `refusal_message_does_not_let_the_component_name_rewrite_the_detail_token`,
    /// at `install_report`'s own two-parameter template
    /// (`"{component} updated to {version}"`): a `component` that happens to
    /// contain the literal text `{version}` must not be rewritten by the
    /// `version` substitution that used to follow it in a chained
    /// `.replace()`.
    #[test]
    fn install_report_does_not_let_the_component_name_rewrite_the_version_token() {
        let english = Chain::load_for_tests(
            "core",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::i18n::EN,
        );
        let placed = [replaced("mpd {version}", "0.3.1")];
        assert_eq!(
            install_report(&english, &placed, None),
            Some(CheckOutcome::Installed("mpd {version} updated to 0.3.1".to_string())),
        );
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
    /// alone would pass with the other's branch deleted. And it lands under
    /// the plugin's own data directory, not under `etc/ritornello` — the
    /// directory a release used to fill before this task moved it.
    #[test]
    fn a_shipped_configuration_only_fills_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));
        let data_dir = worker.plugin_data_root.join("radio");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("stations.toml"), b"# what the operator built\n").unwrap();

        worker
            .write_initial_config(
                "radio",
                &[
                    ("stations.example.toml".to_string(), b"# the shipped defaults\n".to_vec()),
                    ("input-bindings.example.toml".to_string(), b"# the shipped bindings\n".to_vec()),
                ],
            )
            .unwrap();

        assert_eq!(
            std::fs::read(data_dir.join("stations.toml")).unwrap(),
            b"# what the operator built\n",
            "a station list built from the browser must survive an installation"
        );
        assert_eq!(
            std::fs::read(data_dir.join("input-bindings.toml")).unwrap(),
            b"# the shipped bindings\n",
            "there was nothing there: the plugin must not start on an empty file"
        );
        assert!(
            !dir.path().join("etc/ritornello").exists(),
            "the initial configuration must land in the plugin's own data directory, never under etc/ritornello"
        );
    }

    /// A name `plugins::data_dir_for` refuses never gets as far as a path:
    /// the refusal comes before any directory is created and before any byte
    /// is written, anywhere.
    #[test]
    fn an_invalid_plugin_name_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));

        let result = worker.write_initial_config(
            "../x",
            &[("stations.example.toml".to_string(), b"# the shipped defaults\n".to_vec())],
        );

        assert!(matches!(result, Err(Refusal::Prepare(_))), "{result:?}");
        assert!(
            !worker.plugin_data_root.exists(),
            "an invalid name must not create the plugin data root at all"
        );
        assert!(!dir.path().join("etc/ritornello").exists());
    }

    /// Driven through the real `install_one`, not called directly: this is
    /// what proves the component's own name actually reaches
    /// `write_initial_config` from the one caller that has it, rather than a
    /// name typed once into both the call and the assertion.
    #[tokio::test]
    async fn a_fresh_install_writes_the_initial_configuration_under_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));
        let _privileged = Privileged::answers(Ok(()));

        let archive = targz(&[
            ("usr/local/lib/ritornello/plugins/ritornello-plugin-newsource", b"ELF"),
            ("initial-config/stations.example.toml", b"# the shipped defaults\n"),
            ("plugins.toml.fragment", b"[[plugin]]\nname = \"newsource\"\nexec = \"/anything\"\n"),
        ]);
        let published = served("newsource", &archive).await;
        let client = client().unwrap();

        worker.install_one(&client, "newsource", &published, Provenance::Ours, None, None).await.unwrap();

        assert_eq!(
            std::fs::read(worker.plugin_data_root.join("newsource").join("stations.toml")).unwrap(),
            b"# the shipped defaults\n",
            "a fresh install must place the archive's initial configuration under the \
             installing component's own name, not any other plugin's"
        );
    }

    /// A worker whose `plugins.toml` also declares `files`, its binary in
    /// place, and — when `companion` is given — `ritornello-install`'s
    /// registry recording `files-mount` at that version, at the path the
    /// installer writes it, below the same root.
    fn files_worker(root: &Path, declared: bool, companion: Option<&str>) -> Worker {
        let worker = worker_at(root, one_line(PluginStatus::startup("files")));
        if declared {
            let exec = plugins_dir(root).join("ritornello-plugin-files");
            std::fs::write(&exec, b"ELF old").unwrap();
            let mut manifest = std::fs::read_to_string(&worker.manifest).unwrap();
            manifest.push_str(&format!("\n[[plugin]]\nname = \"files\"\nexec = {:?}\n", exec.to_string_lossy()));
            std::fs::write(&worker.manifest, manifest).unwrap();
        }
        if let Some(version) = companion {
            let path = install_registry::path(root);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "format = 1\n\n[components.files]\nversion = \"0.2.0\"\nprivileged = []\n\n\
                     [components.files-mount]\nversion = {version:?}\nprivileged = [\"{MEDIA_HELPER}\"]\n"
                ),
            )
            .unwrap();
        }
        worker
    }

    const MEDIA_HELPER: &str = "/usr/local/lib/ritornello/ritornello-media-mount";

    /// The files plugin's archive in its shape since the companion: its
    /// binary, an example and the fragment — nothing root owns.
    fn files_archive() -> Vec<u8> {
        targz(&[
            ("usr/local/lib/ritornello/plugins/ritornello-plugin-files", b"ELF new"),
            ("examples/media-roots.example.toml", b"# roots\n"),
            ("plugins.toml.fragment", b"[[plugin]]\nname = \"files\"\n"),
        ])
    }

    async fn install_files(worker: &Worker, companion_offered: Option<&str>) -> Result<Placed, Refusal> {
        let published = served("files", &files_archive()).await;
        let client = client().unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client, "files", &published, Provenance::Ours, None, companion_offered),
        )
        .await
        .expect("install_one hung")
    }

    /// **The allowed update, driven through the real `install_one`.** The
    /// companion offered is the one installed, so root is asked for exactly
    /// one thing — the plugin's binary, through the unchanged `PlacePlugin`.
    ///
    /// **[MUTATION]**: refuse every plugin that has a companion (`refused =
    /// true`) — this test fails (`NeedsCompanionStep`).
    #[tokio::test]
    async fn an_equal_companion_version_allows_the_update_with_one_place_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, Some("0.2.0-beta.2"));
        let _privileged = Privileged::answers(Ok(()));

        let outcome = install_files(&worker, Some("0.2.0-beta.2")).await;
        assert!(matches!(outcome, Ok(Placed::Plugin)), "{:?}", outcome.as_ref().err());

        let request: Request =
            serde_json::from_str(&std::fs::read_to_string(worker.staging.join("request.json")).unwrap()).unwrap();
        assert_eq!(request.actions.len(), 1, "{:?}", request.actions);
        match &request.actions[0] {
            Action::PlacePlugin { file, .. } => assert_eq!(file, "ritornello-plugin-files"),
            other => panic!("expected PlacePlugin, got {other:?}"),
        }
    }

    /// A release that moves the companion: refused before anything is
    /// downloaded or staged, remembered on the row, and the sentence names
    /// the companion.
    ///
    /// **[MUTATION]**: pass `None` for the installed version at the call
    /// site — the equal test above fails; pass `companion_offered` for it —
    /// this one fails.
    #[tokio::test]
    async fn a_differing_companion_version_refuses_the_update() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, Some("0.2.0-beta.2"));
        worker.state.write().await.components.push(row("files", ComponentKind::Plugin, Availability::UpdateAvailable));

        let outcome = install_files(&worker, Some("0.2.0-beta.3")).await;
        assert!(matches!(outcome, Err(Refusal::NeedsCompanionStep("files-mount"))), "{:?}", outcome.as_ref().err());
        assert!(!worker.staging.join("request.json").exists());
        let state = worker.state.read().await;
        let files = state.components.iter().find(|c| c.name == "files").unwrap();
        assert_eq!(files.installable, Some(false), "the refusal is remembered for this version");
    }

    /// No registry on the device — one deployed by `deploy.sh`, say: the
    /// installed version is unknown, and that refuses.
    #[tokio::test]
    async fn an_unknown_installed_companion_version_refuses_the_update() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, None);
        let outcome = install_files(&worker, Some("0.2.0-beta.2")).await;
        assert!(matches!(outcome, Err(Refusal::NeedsCompanionStep(_))), "{:?}", outcome.as_ref().err());
        assert!(!worker.staging.join("request.json").exists());
    }

    /// A release that carries no companion at all.
    #[tokio::test]
    async fn a_companion_absent_from_the_release_refuses_the_update() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, Some("0.2.0-beta.2"));
        let outcome = install_files(&worker, None).await;
        assert!(matches!(outcome, Err(Refusal::NeedsCompanionStep(_))), "{:?}", outcome.as_ref().err());
        assert!(!worker.staging.join("request.json").exists());
    }

    /// A first install, through the real `install_one`, with a registry and
    /// a release that would match: `plugins.toml` does not declare `files`,
    /// so it stays `ritornello-install`'s job — its archive alone no longer
    /// says so.
    ///
    /// **[MUTATION]**: drop the `!self.declared(name)` branch — this test
    /// fails.
    #[tokio::test]
    async fn a_first_install_of_a_plugin_with_a_companion_is_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), false, Some("0.2.0-beta.2"));
        let _privileged = Privileged::answers(Ok(()));
        let outcome = install_files(&worker, Some("0.2.0-beta.2")).await;
        assert!(matches!(outcome, Err(Refusal::NeedsCompanionStep(_))), "{:?}", outcome.as_ref().err());
        assert!(!worker.staging.join("request.json").exists());
    }

    /// The wiring, through the real `install`: the companion's version is
    /// read off the same check's fold and reaches `install_one`. With the
    /// release's companion at the installed version, the binary is placed.
    ///
    /// **[MUTATION]**: pass `None` instead of `companion_offered(…)` in
    /// `install` — this test fails (the gesture is refused).
    #[tokio::test]
    async fn install_hands_the_release_s_companion_version_to_install_one() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, Some("0.2.0-beta.2"));
        let _privileged = Privileged::answers(Ok(()));
        let plugin = served("files", &files_archive()).await;
        let companion = Published {
            offer: Offer::Companion("files-mount".to_string()),
            version: "0.2.0-beta.2".to_string(),
            url: "http://127.0.0.1:9/never-fetched".to_string(),
            size: 0,
            release_tag: "v0.2.0-beta.2".to_string(),
            checksums_url: None,
            catalogue_url: None,
        };
        let checked = Checked { ours: vec![companion, plugin], theirs: vec![], third_party: vec![], sources: vec![], fresh: vec![], conflicts: vec![], packs: vec![], plugins_unknown: false, contracts: ContractsByUrl::new() };
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            worker.install(&client().unwrap(), &checked, &["files".to_string()]),
        )
        .await
        .expect("install() hung");
        let request: Request =
            serde_json::from_str(&std::fs::read_to_string(worker.staging.join("request.json")).unwrap()).unwrap();
        assert!(
            matches!(request.actions.as_slice(), [Action::PlacePlugin { file, .. }] if file == "ritornello-plugin-files"),
            "{:?}",
            request.actions
        );
    }

    // ---- R71: the row says the companion moved, at the check ----

    /// A companion's offer, as the fold yields one.
    fn companion_offer(version: &str) -> Published {
        Published {
            offer: Offer::Companion("files-mount".to_string()),
            version: version.to_string(),
            url: "http://127.0.0.1:9/never-fetched".to_string(),
            size: 0,
            release_tag: format!("v{version}"),
            checksums_url: None,
            catalogue_url: None,
        }
    }

    fn installed_files_mount(version: Option<&str>) -> Vec<(&'static str, Option<String>)> {
        vec![("files-mount", version.map(str::to_string))]
    }

    fn files_update_row() -> ComponentOffer {
        let mut r = row("files", ComponentKind::Plugin, Availability::UpdateAvailable);
        r.installed = Some("0.2.0".to_string());
        r.offered = Some("0.3.0".to_string());
        r
    }

    /// Both halves: a companion the release moves marks the row, naming it;
    /// the same companion at the installed version leaves the row alone.
    ///
    /// **[MUTATION]**: drop the `!` before `companion_allows` in
    /// `deny_moved_companion` — red on both halves.
    #[test]
    fn a_moved_companion_marks_the_row_at_the_check() {
        let mut rows = vec![files_update_row()];
        deny_moved_companion(&mut rows, &[companion_offer("0.3.0")], &installed_files_mount(Some("0.2.0")));
        assert_eq!(rows[0].installable, Some(false));
        assert_eq!(rows[0].needs_companion.as_deref(), Some("files-mount"));

        let mut rows = vec![files_update_row()];
        deny_moved_companion(&mut rows, &[companion_offer("0.2.0")], &installed_files_mount(Some("0.2.0")));
        assert_eq!((rows[0].installable, rows[0].needs_companion.as_deref()), (None, None));
    }

    /// Either side unknown marks the row: no registry record, or a release
    /// that carries no companion.
    ///
    /// **[MUTATION]**: read the installed version from the companion's
    /// offer instead (`installed` ignored) — red on the first half.
    #[test]
    fn an_unknown_companion_version_marks_the_row() {
        let mut rows = vec![files_update_row()];
        deny_moved_companion(&mut rows, &[companion_offer("0.3.0")], &installed_files_mount(None));
        assert_eq!(rows[0].needs_companion.as_deref(), Some("files-mount"));

        let mut rows = vec![files_update_row()];
        deny_moved_companion(&mut rows, &[], &installed_files_mount(Some("0.2.0")));
        assert_eq!(rows[0].needs_companion.as_deref(), Some("files-mount"));
    }

    /// **[MUTATION]**: `let Some(companion) = …` falling back on
    /// `"files-mount"` for any plugin — red.
    #[test]
    fn a_plugin_without_a_companion_is_never_marked() {
        let mut rows = vec![row("radio", ComponentKind::Plugin, Availability::UpdateAvailable)];
        deny_moved_companion(&mut rows, &[], &installed_files_mount(None));
        assert_eq!((rows[0].installable, rows[0].needs_companion.as_deref()), (None, None));
    }

    /// Only a declared update of ours is this rule's. Each case would be
    /// marked but for one operand.
    ///
    /// **[MUTATION]**, one per operand, each red on its own case:
    /// - drop `row.kind != ComponentKind::Plugin` — the third-party row;
    /// - drop `!row.declared` — the undeclared row;
    /// - drop the availability test — the aligned row;
    /// - narrow it to `UpdateAvailable` alone — the missing-binary row.
    #[test]
    fn only_a_declared_update_of_ours_is_marked() {
        let moved = [companion_offer("0.3.0")];
        let installed = installed_files_mount(Some("0.2.0"));
        let mark = |r: ComponentOffer| {
            let mut rows = vec![r];
            deny_moved_companion(&mut rows, &moved, &installed);
            rows.remove(0).needs_companion
        };
        let mut third = files_update_row();
        third.kind = ComponentKind::ThirdParty;
        assert_eq!(mark(third), None, "a stranger's binary under the name");
        let mut undeclared = files_update_row();
        undeclared.declared = false;
        assert_eq!(mark(undeclared), None, "an installation, deny_privileged_install's sentence");
        let mut aligned = files_update_row();
        aligned.availability = Availability::Aligned;
        assert_eq!(mark(aligned), None, "nothing on offer");
        let mut missing = files_update_row();
        missing.availability = Availability::BinaryMissing;
        assert_eq!(mark(missing).as_deref(), Some("files-mount"), "placing the binary again is an update");
    }

    /// A companion's refusal is recomputed, never carried: the row refused
    /// at the last check is cleared once `ritornello-install` has updated the
    /// companion — within the same offered version — while a refusal for
    /// another reason is still carried.
    ///
    /// **[MUTATION]**: drop `.filter(|p| p.needs_companion.is_none())` from
    /// `carry_installable` — red on the first half.
    #[test]
    fn a_companion_refusal_is_recomputed_not_carried() {
        let mut previous = files_update_row();
        previous.installable = Some(false);
        previous.needs_companion = Some("files-mount".to_string());
        let mut fresh = vec![files_update_row()];
        carry_installable(&[previous], &mut fresh);
        deny_moved_companion(&mut fresh, &[companion_offer("0.3.0")], &installed_files_mount(Some("0.3.0")));
        assert_eq!((fresh[0].installable, fresh[0].needs_companion.as_deref()), (None, None));

        let mut previous = files_update_row();
        previous.installable = Some(false);
        let mut fresh = vec![files_update_row()];
        carry_installable(&[previous], &mut fresh);
        deny_moved_companion(&mut fresh, &[companion_offer("0.3.0")], &installed_files_mount(Some("0.3.0")));
        assert_eq!(fresh[0].installable, Some(false), "another refusal of this version is remembered");
    }

    /// Declares `files` on `worker` with a settled status line at 0.2.0, and
    /// records `files-mount` at `companion` in the registry (or removes it).
    async fn files_announced(worker: &Worker, root: &Path, companion: Option<&str>) {
        worker.status.write().await.plugins = vec![PluginStatus {
            version: Some("0.2.0".to_string()),
            ..PluginStatus::kind("files", "source", true, false)
        }];
        let path = install_registry::path(root);
        match companion {
            Some(version) => {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(
                    &path,
                    format!("format = 1\n[components.files-mount]\nversion = {version:?}\nprivileged = []\n"),
                )
                .unwrap();
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    fn files_row(worker_state: &UpdateState) -> ComponentOffer {
        worker_state.components.iter().find(|c| c.name == "files").expect("a files row").clone()
    }

    /// **Through the check itself** (`settle_check`, the half of `check` after
    /// the release list is fetched): the registry is read, the row is marked
    /// before anyone presses anything, and the next check — once
    /// `ritornello-install` has recorded the new companion — unmarks it.
    ///
    /// **[MUTATION]**: remove the `deny_moved_companion` call in
    /// `settle_check` — red on the first half.
    #[tokio::test]
    async fn a_check_marks_the_row_before_any_press_and_the_next_one_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, None);
        files_announced(&worker, dir.path(), Some("0.2.0")).await;
        let body = releases_body(&[
            &asset_for("files", "0.3.0"),
            &format!("ritornello-files-mount-0.3.0-{ARCH}.tar.gz"),
        ]);
        let mut releases = parse_releases(std::str::from_utf8(&body).unwrap(), Channel::Stable).unwrap();
        // The release publishes what `files` speaks: without it the row would
        // be refused for its unpublished contracts on both halves, and the
        // companion's mark — what this test is about — could not be told apart.
        let (catalogue, _) = serve_counting(
            contracts_body(&[("files", speaks_source(running_source().major, running_source().minor))]),
            "catalogue.json",
        )
        .await;
        releases[0].assets.push(asset("catalogue.json", &catalogue));
        let client = client().unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(30), worker.settle_check(&client, &releases))
            .await
            .expect("settle_check hung");
        let files = files_row(&*worker.state.read().await);
        assert_eq!(files.availability, Availability::UpdateAvailable, "{files:?}");
        assert_eq!((files.installable, files.needs_companion.as_deref()), (Some(false), Some("files-mount")));

        files_announced(&worker, dir.path(), Some("0.3.0")).await;
        tokio::time::timeout(std::time::Duration::from_secs(30), worker.settle_check(&client, &releases))
            .await
            .expect("settle_check hung");
        let files = files_row(&*worker.state.read().await);
        assert_eq!((files.installable, files.needs_companion.as_deref()), (None, None));
    }

    /// The same marking after an install pass (`conclude_install` rebuilds
    /// the rows): another plugin placed, `files` still waits for its
    /// companion, and its row says so.
    ///
    /// **[MUTATION]**: remove the `deny_moved_companion` call in
    /// `conclude_install` — red.
    #[tokio::test]
    async fn an_install_pass_keeps_the_moved_companion_on_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, None);
        files_announced(&worker, dir.path(), Some("0.2.0")).await;
        let _privileged = Privileged::answers(Ok(()));
        let radio =
            served("radio", &targz(&[("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF")])).await;
        let mut files_offer = radio.clone();
        files_offer.offer = Offer::Plugin("files".to_string());
        files_offer.version = "0.3.0".to_string();
        let checked = Checked {
            ours: vec![radio, files_offer, companion_offer("0.3.0")],
            theirs: vec![],
            third_party: vec![],
            sources: vec![],
            fresh: vec![],
            conflicts: vec![],
            packs: vec![],
            plugins_unknown: false,
            contracts: ContractsByUrl::new(),
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            worker.install(&client().unwrap(), &checked, &["radio".to_string()]),
        )
        .await
        .expect("install() hung");
        let files = files_row(&*worker.state.read().await);
        assert_eq!((files.installable, files.needs_companion.as_deref()), (Some(false), Some("files-mount")));
    }

    /// The backstop at the gesture names the companion on the row too, so
    /// the page says the same thing a check would have.
    #[tokio::test]
    async fn the_gesture_s_refusal_names_the_companion_on_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, Some("0.2.0-beta.2"));
        worker.state.write().await.components.push(row("files", ComponentKind::Plugin, Availability::UpdateAvailable));
        let outcome = install_files(&worker, Some("0.2.0-beta.3")).await;
        assert!(matches!(outcome, Err(Refusal::NeedsCompanionStep(_))), "{:?}", outcome.as_ref().err());
        let state = worker.state.read().await;
        assert_eq!(files_row(&state).needs_companion.as_deref(), Some("files-mount"));
    }

    /// M3: a declared **third-party** plugin that happens to be named `files`
    /// is judged by `only_its_own_binary` alone — our release's companion
    /// says nothing about a stranger's binary.
    ///
    /// **[MUTATION]**: `else if third_party { false }` → `true` — red.
    #[tokio::test]
    async fn a_declared_third_party_plugin_named_like_ours_is_not_held_to_our_companion() {
        let dir = tempfile::tempdir().unwrap();
        let worker = files_worker(dir.path(), true, None);
        let _privileged = Privileged::answers(Ok(()));
        let archive = targz(&[("usr/local/lib/ritornello/plugins/ritornello-plugin-files", b"THEIRS")]);
        let published = served("files", &archive).await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install_one(&client().unwrap(), "files", &published, Provenance::Theirs, None, None),
        )
        .await
        .expect("install_one hung");
        assert!(matches!(outcome, Ok(Placed::Plugin)), "{:?}", outcome.as_ref().err());
    }

    /// M7: a name that is a companion's resolves to nothing, even with its
    /// archive in our release — only `ritornello-install` places it.
    ///
    /// **[MUTATION]**: `Offer::Companion(c) => c == name` in `carries` — red.
    #[test]
    fn a_companion_s_name_resolves_to_nothing() {
        let checked = Checked { ours: vec![companion_offer("0.3.0")], theirs: vec![], third_party: vec![], sources: vec![], fresh: vec![], conflicts: vec![], packs: vec![], plugins_unknown: false, contracts: ContractsByUrl::new() };
        assert_eq!(resolve(&checked, "files-mount"), Resolved::Nothing);
    }

    /// A plugin with no companion is updated as before, with no registry and
    /// no companion in the release.
    ///
    /// **[MUTATION]**: apply the companion rule to every plugin (`companion_of`
    /// answering `Some` for any name) — this test fails.
    #[tokio::test]
    async fn a_plugin_without_a_companion_is_updated_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));
        let _privileged = Privileged::answers(Ok(()));
        let archive = targz(&[("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF")]);
        let published = served("radio", &archive).await;
        let client = client().unwrap();
        let outcome = worker.install_one(&client, "radio", &published, Provenance::Ours, None, None).await;
        assert!(matches!(outcome, Ok(Placed::Plugin)), "{:?}", outcome.as_ref().err());
    }

    /// What `install_one` remembers, and under which key. Driven through the
    /// real `install_one` and read back from the file the next night's process
    /// would read: a third party is written under its namespaced key and never
    /// its bare name, one of ours under its name, and a third party with no
    /// repository to namespace by under nothing.
    #[tokio::test]
    async fn install_one_remembers_a_placement_under_the_key_its_origin_dictates() {
        for (provenance, repo, expected) in [
            (Provenance::Theirs, Some("someone/theirs"), Some("third-party:someone/theirs:radio")),
            (Provenance::Ours, None, Some("radio")),
            (Provenance::Theirs, None, None),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let worker = worker_at(dir.path(), one_line(PluginStatus::startup("radio")));
            let _privileged = Privileged::answers(Ok(()));
            let archive = targz(&[("usr/local/lib/ritornello/plugins/ritornello-plugin-radio", b"ELF")]);
            let published = served("radio", &archive).await;
            let outcome = worker
                .install_one(&client().unwrap(), "radio", &published, provenance, repo, None)
                .await;
            assert!(matches!(outcome, Ok(Placed::Plugin)), "{:?}", outcome.as_ref().err());
            let keys: Vec<String> = placed::read(&worker.staging).keys().cloned().collect();
            assert_eq!(keys, expected.map(str::to_string).into_iter().collect::<Vec<_>>(), "{provenance:?}");
        }
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

    // ---- What each offered component speaks, read at the check -----------

    /// A `catalogue.json` publishing what each named component speaks, and
    /// nothing else.
    fn contracts_body(entries: &[(&str, Speaks)]) -> Vec<u8> {
        let contracts: BTreeMap<&str, &Speaks> = entries.iter().map(|(n, s)| (*n, s)).collect();
        serde_json::to_vec(&serde_json::json!({"components": {}, "contracts": contracts})).unwrap()
    }

    /// What a plugin speaking the source contract at `major.minor` speaks.
    fn speaks_source(major: u32, minor: u32) -> Speaks {
        Speaks {
            protocol: ritornello_proto::PROTOCOL_VERSION,
            contracts: BTreeMap::from([(
                ritornello_proto::Contract::Source,
                ritornello_proto::ContractVersion::new(major, minor),
            )]),
        }
    }

    fn running_source() -> ritornello_proto::ContractVersion {
        ritornello_proto::Contract::Source.current()
    }

    fn asset(name: &str, url: &str) -> release::Asset {
        release::Asset { name: name.to_string(), url: url.to_string(), size: 1 }
    }

    fn check_row(state: &UpdateState, name: &str) -> ComponentOffer {
        state.components.iter().find(|c| c.name == name).cloned().unwrap_or_else(|| panic!("no {name} row"))
    }

    /// **The contracts come from the release that carries the archive, not
    /// from the newest one** — two releases, `radio`'s archive in the older.
    /// The newer catalogue carries a decoy entry for `radio` speaking another
    /// version; the row must say what the older one says. And each catalogue
    /// is fetched once however many rows it describes (the core and `mpd`
    /// share the newer one).
    ///
    /// **[MUTATION]** read every row's contracts from
    /// `newest_catalogue_url` instead of its own `catalogue_url`: red on the
    /// `radio` assertion.
    #[tokio::test]
    async fn a_check_reads_each_component_s_contracts_from_the_release_that_carries_it() {
        let status = one_line(PluginStatus {
            version: Some("0.2.0".into()),
            repository: Some("https://github.com/skerdudou/ritornello".into()),
            speaks: Some(speaks_source(running_source().major, running_source().minor)),
            ..PluginStatus::kind("radio", "source", true, false)
        });
        let (worker, _dir) = worker_rig(status);
        let carried = speaks_source(running_source().major, running_source().minor);
        let decoy = speaks_source(running_source().major, running_source().minor + 7);
        let mpd = speaks_source(running_source().major, running_source().minor);
        let (newer_url, newer_hits) = serve_counting(
            contracts_body(&[("core", Speaks::this_core()), ("mpd", mpd.clone()), ("radio", decoy)]),
            "catalogue.json",
        )
        .await;
        let (older_url, older_hits) =
            serve_counting(contracts_body(&[("radio", carried.clone())]), "catalogue.json").await;
        let core_asset = format!("ritornello-core-0.3.0-{ARCH}.tar.gz");
        let releases = vec![
            Release {
                tag: "v0.3.0".into(),
                published_at: "2026-10-02T00:00:00Z".into(),
                assets: vec![
                    asset(&core_asset, "https://x/core"),
                    asset(&asset_for("mpd", "0.3.0"), "https://x/mpd"),
                    asset("catalogue.json", &newer_url),
                ],
            },
            Release {
                tag: "v0.2.5".into(),
                published_at: "2026-09-01T00:00:00Z".into(),
                assets: vec![asset(&asset_for("radio", "0.2.5"), "https://x/radio"), asset("catalogue.json", &older_url)],
            },
        ];

        tokio::time::timeout(std::time::Duration::from_secs(30), worker.settle_check(&client().unwrap(), &releases))
            .await
            .expect("settle_check hung")
            .expect("a check over a readable release list");

        let state = worker.state.read().await;
        let radio = check_row(&state, "radio");
        assert_eq!(radio.availability, Availability::UpdateAvailable, "{radio:?}");
        assert_eq!(radio.contracts.speaks, Some(carried), "radio's contracts come from v0.2.5's catalogue");
        assert_eq!(radio.contracts.with_core, Some(state::Fit::Compatible));
        assert_eq!((radio.installable, radio.contracts.not_installable_reason), (None, None));
        let core = check_row(&state, "core");
        assert_eq!(core.contracts.speaks, Some(Speaks::this_core()));
        assert!(!core.contracts.breaking);
        assert_eq!(check_row(&state, "mpd").contracts.speaks, Some(mpd));
        use std::sync::atomic::Ordering::SeqCst;
        assert_eq!((newer_hits.load(SeqCst), older_hits.load(SeqCst)), (1, 1), "each catalogue is read once");
    }

    /// A catalogue that cannot be read does not fail the check: it is
    /// answered, and what that catalogue carries is not installable from the
    /// device, with the reason — never treated as compatible.
    ///
    /// **[MUTATION]** treat a missing contracts entry as compatible in
    /// `judge_contracts` (no `deny`): red on the `installable` assertion.
    #[tokio::test]
    async fn a_catalogue_that_cannot_be_read_makes_what_it_carries_not_installable_and_the_check_goes_on() {
        let status = one_line(PluginStatus {
            version: Some("0.2.0".into()),
            repository: Some("https://github.com/skerdudou/ritornello".into()),
            ..PluginStatus::kind("radio", "source", true, false)
        });
        let (worker, _dir) = worker_rig(status);
        let releases = vec![Release {
            tag: "v0.3.0".into(),
            published_at: "2026-10-02T00:00:00Z".into(),
            assets: vec![
                asset(&asset_for("radio", "0.3.0"), "https://x/radio"),
                asset("catalogue.json", &refused_url().await),
            ],
        }];

        tokio::time::timeout(std::time::Duration::from_secs(30), worker.settle_check(&client().unwrap(), &releases))
            .await
            .expect("settle_check hung")
            .expect("the check still answers");

        let state = worker.state.read().await;
        assert_eq!(state.outcome, CheckOutcome::Ok);
        let radio = check_row(&state, "radio");
        assert_eq!(
            (radio.installable, radio.contracts.not_installable_reason),
            (Some(false), Some(state::NotInstallable::ContractsUnpublished)),
            "{radio:?}"
        );
        assert_eq!(radio.contracts.with_core, None);
    }

    /// A refusal for unpublished contracts is a fact about one check's
    /// reading, not about the archive: the next check, which may read the
    /// catalogue, decides afresh rather than inheriting it.
    #[test]
    fn a_contracts_refusal_is_not_carried_to_the_next_check() {
        let mut previous = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        previous.installable = Some(false);
        previous.contracts.not_installable_reason = Some(state::NotInstallable::ContractsUnpublished);
        let mut fresh = vec![row("radio", ComponentKind::Plugin, Availability::UpdateAvailable)];
        carry_installable(&[previous], &mut fresh);
        assert_eq!(fresh[0].installable, None);
    }

    // ---- A breaking core and its dependents: one request ------------------
    //
    // Driven through the real `install` pass with the privileged unit's
    // answer faked (`Privileged`) and every request it was handed recorded
    // (`SEEN_REQUESTS`): the property is the **sequence** of requests, and
    // `request.json` alone only keeps the last one.

    /// The requests the privileged unit was handed on this thread, in order,
    /// each as the list of what it places: a plugin by its file, the core as
    /// `core`.
    fn seen_requests() -> Vec<Vec<String>> {
        SEEN_REQUESTS.with(|seen| {
            seen.borrow()
                .iter()
                .map(|text| {
                    let request: Request = serde_json::from_str(text).unwrap();
                    request
                        .actions
                        .iter()
                        .map(|action| match action {
                            Action::PlaceCore { .. } => CORE.to_string(),
                            Action::PlacePlugin { file, .. } => file.clone(),
                            other => format!("{other:?}"),
                        })
                        .collect()
                })
                .collect()
        })
    }

    /// Refused by the running core: a plugin that can only travel with the
    /// core. Which refusal does not matter to the installer.
    fn refused_fit() -> Fit {
        Fit::Refused { refusal: crate::compat::Refusal::Legacy { found: 1 } }
    }

    /// The rows a check leaves when it offers a core (`breaking` or not),
    /// `mpd` refused by the running core, and `radio` accepted by it.
    fn break_rows(breaking: bool) -> Vec<ComponentOffer> {
        let mut core = row(CORE, ComponentKind::Core, Availability::UpdateAvailable);
        core.contracts.breaking = breaking;
        let mut mpd = row("mpd", ComponentKind::Plugin, Availability::UpdateAvailable);
        mpd.contracts.with_running_core = Some(refused_fit());
        let mut radio = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        radio.contracts.with_running_core = Some(Fit::Compatible);
        vec![core, mpd, radio]
    }

    fn plugin_archive(name: &str) -> Vec<u8> {
        targz(&[(&format!("usr/local/lib/ritornello/plugins/ritornello-plugin-{name}"), b"ELF")])
    }

    /// A worker declaring `radio` and `mpd`, whose rows are `rows`, whose
    /// core loop acknowledges every order and records it, and whose restart
    /// hook counts its calls and snapshots the placement memory at that
    /// instant — the instant a real core leaves.
    struct GroupRig {
        worker: Worker,
        _dir: tempfile::TempDir,
        orders: Arc<std::sync::Mutex<Vec<(String, PluginAction)>>>,
        exits: Arc<std::sync::Mutex<Vec<placed::Placed>>>,
    }

    async fn group_rig(rows: Vec<ComponentOffer>) -> GroupRig {
        let dir = tempfile::tempdir().unwrap();
        let mut worker = worker_at(dir.path(), one_line(PluginStatus::kind("radio", "source", true, false)));
        let mpd = plugins_dir(dir.path()).join("ritornello-plugin-mpd");
        std::fs::write(&mpd, b"the old mpd").unwrap();
        let mut manifest = std::fs::read_to_string(&worker.manifest).unwrap();
        manifest.push_str(&format!("\n[[plugin]]\nname = \"mpd\"\nexec = {:?}\n", mpd.to_string_lossy()));
        std::fs::write(&worker.manifest, manifest).unwrap();
        let (tx, mut rx) = mpsc::channel::<PluginOrder>(8);
        worker.plugins_tx = tx;
        let orders: Arc<std::sync::Mutex<Vec<(String, PluginAction)>>> = Arc::default();
        let recorded = orders.clone();
        tokio::spawn(async move {
            while let Some(order) = rx.recv().await {
                recorded.lock().unwrap().push((order.name.clone(), order.action));
                let _ = order.ack.send(true);
            }
        });
        let exits: Arc<std::sync::Mutex<Vec<placed::Placed>>> = Arc::default();
        let at_exit = exits.clone();
        let staging = worker.staging.clone();
        worker.restart = Arc::new(move || at_exit.lock().unwrap().push(placed::read(&staging)));
        worker.state.write().await.components = rows;
        GroupRig { worker, _dir: dir, orders, exits }
    }

    /// Our release offering the core, `mpd` and `radio`, each served once.
    async fn group_release(mpd: Published) -> Checked {
        ours(vec![
            served_core(&core_archive()).await,
            mpd,
            served("radio", &plugin_archive("radio")).await,
        ])
    }

    async fn run_install(worker: &Worker, checked: &Checked, list: &[&str]) {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            worker.install(&client().unwrap(), checked, &names(list)),
        )
        .await
        .expect("the install pass hung");
    }

    /// **The break, end to end.** `radio`, which the running core accepts,
    /// goes first in its own request and is restarted, as outside a break;
    /// `mpd`, which the running core refuses, travels in **one** request
    /// with the core, the core last; `mpd` is not restarted (the new core
    /// starts it) and the process leaves once.
    ///
    /// **[MUTATION]** put `PlaceCore` first in the group's request: red on
    /// the sequence. **[MUTATION]** restart the group's plugins after the
    /// request: red on the orders.
    #[tokio::test]
    async fn a_breaking_core_goes_in_one_request_with_its_dependents_core_last() {
        let rig = group_rig(break_rows(true)).await;
        let _privileged = Privileged::answers(Ok(()));
        let checked = group_release(served("mpd", &plugin_archive("mpd")).await).await;

        run_install(&rig.worker, &checked, &[CORE, "mpd", "radio"]).await;

        assert_eq!(
            seen_requests(),
            vec![
                vec!["ritornello-plugin-radio".to_string()],
                vec!["ritornello-plugin-mpd".to_string(), CORE.to_string()],
            ],
            "the compatible plugin alone first, then the dependent and the core in one request, core last"
        );
        assert_eq!(
            *rig.orders.lock().unwrap(),
            vec![("radio".to_string(), PluginAction::Restart)],
            "only the compatible plugin is restarted under the running core"
        );
        assert_eq!(rig.exits.lock().unwrap().len(), 1, "the core leaves once, after the group");
    }

    /// Every member of the group is remembered **before** the process
    /// leaves: the next night's process reads that memory, and the one that
    /// placed the group no longer exists by then.
    ///
    /// **[MUTATION]** remember only the core in `place` (or after the
    /// restart): red.
    #[tokio::test]
    async fn every_member_of_the_group_is_remembered_before_the_process_leaves() {
        let rig = group_rig(break_rows(true)).await;
        let _privileged = Privileged::answers(Ok(()));
        let checked = group_release(served("mpd", &plugin_archive("mpd")).await).await;

        run_install(&rig.worker, &checked, &[CORE, "mpd", "radio"]).await;

        let exits = rig.exits.lock().unwrap();
        let memory = exits.first().expect("the install never reached the restart");
        assert_eq!(placed::version_of(memory, "mpd"), Some("2.0.0"));
        assert_eq!(placed::version_of(memory, CORE), Some("2.0.0"));
        assert_eq!(
            memory[CORE].not_installed_files.as_deref(),
            Some(["etc/systemd/system/ritornello-rollback.service".to_string()].as_slice()),
            "and the core's note too"
        );
    }

    /// **A dependent that cannot be prepared keeps the core out.** `mpd`'s
    /// digest does not match: no request carrying the core is ever written,
    /// nothing of the group is left in staging, the process does not leave,
    /// `radio` — which went first — stays installed, and the page names
    /// `mpd` and says the rest waits.
    ///
    /// **[MUTATION]** carry on staging the rest of the group after a failure
    /// (and send it): red on the sequence.
    #[tokio::test]
    async fn a_dependent_that_fails_to_stage_keeps_the_core_out() {
        let rig = group_rig(break_rows(true)).await;
        let _privileged = Privileged::answers(Ok(()));
        let checked = group_release(served_with_wrong_digest("mpd", &plugin_archive("mpd")).await).await;

        run_install(&rig.worker, &checked, &[CORE, "mpd", "radio"]).await;

        assert_eq!(seen_requests(), vec![vec!["ritornello-plugin-radio".to_string()]], "no request carries the core");
        assert!(rig.exits.lock().unwrap().is_empty(), "the core did not leave");
        assert_eq!(*rig.orders.lock().unwrap(), vec![("radio".to_string(), PluginAction::Restart)]);
        for file in ["staged-core", "staged-plugin-mpd"] {
            assert!(!rig.worker.staging.join(file).exists(), "{file} left in staging");
        }
        let english = Chain::load_for_tests("core", "en", Path::new("/nonexistent"), crate::i18n::EN);
        let expected = ritornello_i18n::interpolate(english.get("update_group_postponed"), [("component", "mpd")]);
        assert_eq!(rig.worker.state.read().await.outcome, CheckOutcome::Failed(expected));
    }

    /// **Room for the group as a whole.** Each archive fits alone, the two do
    /// not: nothing is fetched, nothing written, nothing asked of root.
    ///
    /// **[MUTATION]** check the room of each member alone: red.
    #[tokio::test]
    async fn the_group_s_room_is_checked_as_a_whole() {
        let rig = group_rig(break_rows(true)).await;
        let _privileged = Privileged::answers(Ok(()));
        let disk = crate::system::disk_usage(&rig.worker.root.to_string_lossy());
        let available_kb = disk.as_ref().expect("this test needs the free space of its temporary root").available_kb;
        let size = available_kb * 1024 * 2 / 9;
        assert!(enough_room(disk, size as usize), "each archive fits alone");
        assert!(!enough_room(disk, (2 * size) as usize), "the two together do not");
        let nowhere = refused_url().await;
        let core = Published { size, url: nowhere.clone(), ..served_core(b"never fetched").await };
        let mpd = Published { size, url: nowhere, ..served("mpd", b"never fetched").await };
        let checked = ours(vec![core, mpd]);

        run_install(&rig.worker, &checked, &[CORE, "mpd"]).await;

        assert!(seen_requests().is_empty(), "nothing asked of root");
        assert!(rig.exits.lock().unwrap().is_empty());
        assert!(!rig.worker.staging.join("staged-core").exists() && !rig.worker.staging.join("staged-plugin-mpd").exists());
        let expected = refusal_message(&*rig.worker.catalog.read().await, CORE, &Refusal::NoRoom);
        assert_eq!(rig.worker.state.read().await.outcome, CheckOutcome::Failed(expected));
    }

    /// **Outside a break nothing changes**: the same three rows with a core
    /// that does not break, and each component goes in its own request,
    /// plugins first, each plugin restarted, the core last.
    #[tokio::test]
    async fn a_non_breaking_core_keeps_the_per_component_path() {
        let rig = group_rig(break_rows(false)).await;
        let _privileged = Privileged::answers(Ok(()));
        let checked = group_release(served("mpd", &plugin_archive("mpd")).await).await;

        run_install(&rig.worker, &checked, &[CORE, "mpd", "radio"]).await;

        assert_eq!(
            seen_requests(),
            vec![
                vec!["ritornello-plugin-mpd".to_string()],
                vec!["ritornello-plugin-radio".to_string()],
                vec![CORE.to_string()],
            ]
        );
        assert_eq!(
            *rig.orders.lock().unwrap(),
            vec![("mpd".to_string(), PluginAction::Restart), ("radio".to_string(), PluginAction::Restart)]
        );
        assert_eq!(rig.exits.lock().unwrap().len(), 1);
    }

    /// The owner unticked the dependent: the dialog warned, and the core
    /// installs anyway — alone, in a request of its own. Nothing else.
    #[tokio::test]
    async fn an_unticked_dependent_is_left_behind_and_the_core_installs() {
        let rig = group_rig(break_rows(true)).await;
        let _privileged = Privileged::answers(Ok(()));
        let checked = ours(vec![served_core(&core_archive()).await]);

        run_install(&rig.worker, &checked, &[CORE]).await;

        assert_eq!(seen_requests(), vec![vec![CORE.to_string()]]);
        assert!(rig.orders.lock().unwrap().is_empty());
        assert_eq!(rig.exits.lock().unwrap().len(), 1);
    }

    /// **Unpublished contracts refuse the gesture too**, by hand, before
    /// anything is fetched — while a row marked `installable: Some(false)`
    /// for another reason (an archive refused earlier) is still retried by
    /// hand, as it always was.
    ///
    /// **[MUTATION]** drop the check in `resolve_for_install`: red on the
    /// first half. **[MUTATION]** refuse on `installable == Some(false)`
    /// instead: red on the second.
    #[tokio::test]
    async fn a_component_whose_contracts_are_unpublished_is_refused_at_the_gesture() {
        let mut unpublished = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        unpublished.installable = Some(false);
        unpublished.contracts.not_installable_reason = Some(NotInstallable::ContractsUnpublished);
        let rig = group_rig(vec![unpublished]).await;
        let _privileged = Privileged::answers(Ok(()));
        let checked = ours(vec![served("radio", &plugin_archive("radio")).await]);

        run_install(&rig.worker, &checked, &["radio"]).await;

        assert!(seen_requests().is_empty(), "nothing asked of root");
        let expected = refusal_message(&*rig.worker.catalog.read().await, "radio", &Refusal::ContractsUnpublished);
        assert_eq!(rig.worker.state.read().await.outcome, CheckOutcome::Failed(expected));
        drop(_privileged);

        let mut refused_before = row("radio", ComponentKind::Plugin, Availability::UpdateAvailable);
        refused_before.installable = Some(false);
        let rig = group_rig(vec![refused_before]).await;
        let _privileged = Privileged::answers(Ok(()));
        let checked = ours(vec![served("radio", &plugin_archive("radio")).await]);

        run_install(&rig.worker, &checked, &["radio"]).await;

        assert_eq!(seen_requests(), vec![vec!["ritornello-plugin-radio".to_string()]], "a hand retry still goes through");
    }

    /// **The night never installs a break.** With the offered core breaking:
    /// not the core, not `mpd` (refused by the running core — alone it would
    /// be refused until someone installs the core); `radio`, which the
    /// running core accepts, still updates even though the offered core
    /// would refuse it, because the running core is the one it will meet
    /// tonight; `cd` too. The same rows without a break install everything.
    ///
    /// **[MUTATION]** drop the break filter: red.
    #[test]
    fn the_nightly_run_never_installs_a_break() {
        let rows = |breaking: bool| {
            let mut rows = break_rows(breaking);
            rows[2].contracts.with_core = Some(refused_fit());
            let mut cd = row("cd", ComponentKind::Plugin, Availability::UpdateAvailable);
            cd.contracts.with_running_core = Some(Fit::Compatible);
            rows.push(cd);
            rows
        };
        let list = |breaking| automatic_install_list(&rows(breaking), &nothing_placed(), schedule::InstallScope::Official);
        assert_eq!(list(true), names(&["radio", "cd"]));
        assert_eq!(list(false), names(&[CORE, "mpd", "radio", "cd"]), "without a break, everything as before");
    }

    /// **The page learns that a major update waits, from the check that
    /// sees it — and only from that one.** A check offering a core with
    /// another bootstrap sets the flag; the next check, offering a core
    /// that does not break, clears it.
    ///
    /// **[MUTATION]** never clear the flag (`|=`): red on the second check.
    #[tokio::test]
    async fn a_major_update_is_flagged_by_the_check_that_sees_it_and_only_by_that_one() {
        let (worker, _dir) = worker_rig(one_line(PluginStatus::kind("radio", "source", true, false)));
        let core_asset = format!("ritornello-core-0.3.0-{ARCH}.tar.gz");
        let release = |catalogue: &str| {
            vec![Release {
                tag: "v0.3.0".into(),
                published_at: "2026-10-02T00:00:00Z".into(),
                assets: vec![asset(&core_asset, "https://x/core"), asset("catalogue.json", catalogue)],
            }]
        };
        let breaking = Speaks { protocol: ritornello_proto::PROTOCOL_VERSION + 1, ..Speaks::this_core() };
        let (breaking_url, _) = serve_counting(contracts_body(&[("core", breaking)]), "catalogue.json").await;
        let (aligned_url, _) = serve_counting(contracts_body(&[("core", Speaks::this_core())]), "catalogue.json").await;

        for (url, expected) in [(breaking_url, true), (aligned_url, false)] {
            tokio::time::timeout(std::time::Duration::from_secs(30), worker.settle_check(&client().unwrap(), &release(&url)))
                .await
                .expect("settle_check hung")
                .expect("a check over a readable release list");
            let state = worker.state.read().await;
            assert_eq!(check_row(&state, CORE).contracts.breaking, expected);
            assert_eq!(state.major_update_waiting, expected);
        }
    }
}
