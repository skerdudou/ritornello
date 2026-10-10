//! The plan: what to stop, unmount, remove, place, write and start, computed
//! from three inputs — the release inventory, the device survey, and what
//! the operator chose — and nothing else.
//!
//! This is pure: no I/O, no path touched. Task 12 renders a `Plan` as the
//! POSIX `sh` script root runs on the device, so a wrong plan here deletes
//! the wrong thing as root. Two rules carry most of the weight:
//!
//! - **Removal takes the union of the registry and the inventory.** A file
//!   an older version placed, which the newer one no longer lists, is only
//!   known from the registry; trusting the inventory alone would leave it
//!   behind — a unit still enabled, a polkit rule still granting. This holds
//!   for a component that is kept, too: an update that drops a unit removes
//!   it (R27).
//! - **Every path the plan removes is checked last**, against
//!   `names::deletable_file` / `names::deletable_tree`, whatever the registry
//!   or the inventory said. A single refusal yields no plan at all, never a
//!   partial one.
//!
//! Symlink checks on the device (a planted `plugins -> /mnt/...`) are the
//! script's business, not the plan's (R25): they need the device.

use std::collections::{BTreeMap, BTreeSet};

use crate::device::DeviceState;
use crate::inventory::{Companion, Component, Inventory};
use crate::names::{self, DATA_ROOT, PACKS_ROOT, PLUGINS_DIR};
use crate::registry::{Recorded, Registry};

/// Where a unit file lives. A unit the plan removes is disabled first,
/// whether or not the inventory still names it in `enable`.
const SYSTEMD_DIR: &str = "/etc/systemd/system/";

/// The one place a component may mount under: itself, or one lowercase
/// leaf below it.
const MOUNT_BASE: &str = "/mnt/ritornello";

/// What a device with no `plugins.toml` at all is given: a header and no
/// entry, which the core reads as "no plugin" and which `append_block`
/// (the core's own, when a plugin is installed from the web UI) can add to.
const PLUGINS_TOML_HEADER: &str = "# The plugins the core starts, in priority order: the first declared\n\
# wins a metadata arbitration. Written by ritornello-install; each entry\n\
# needs only `name` and `exec`.\n";

/// What the operator chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// The core, plus exactly `plugins` (ours and third-party ones already
    /// declared) and exactly the languages in `packs`. Anything declared and
    /// not in `plugins` is removed; its data stays unless it is named in
    /// `erase_data`.
    ///
    /// A component or a pack the device already has at the offered version
    /// is left as it is (`is_current`), unless `reinstall` says to place
    /// everything again — the repair of a hand-damaged unit or rule, or a
    /// device whose files no longer match what its registry says.
    InstallOrUpdate {
        plugins: BTreeSet<String>,
        packs: BTreeSet<String>,
        erase_data: BTreeSet<String>,
        reinstall: bool,
    },
    /// Everything Ritornello placed. `/var/lib/ritornello` and the
    /// `ritornello` account go only with `erase_data`.
    RemoveAll { erase_data: bool },
}

/// One file to place from a component's archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Put {
    pub archive: String,
    pub archive_path: String,
    pub dest: String,
    pub mode: String,
    pub owner: String,
}

/// One initial configuration, written only when its target is absent.
/// `target` is relative to the plugin's own data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Initial {
    pub archive: String,
    pub archive_path: String,
    pub plugin: String,
    pub target: String,
}

/// What the operator is told, before anything runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub erased: Vec<String>,
    pub kept_third_party: Vec<String>,
    /// Components and language packs already on the device at the offered
    /// version, left exactly as they are.
    pub up_to_date: Vec<String>,
    /// Plugins of ours the registry still records but `plugins.toml` no
    /// longer declares, with no root file of their own: uninstalled from
    /// the web interface. Nothing of theirs runs any more; the plan only
    /// removes what may be left of their binary and forgets their record.
    pub cleared: Vec<String>,
    /// Language packs placed (installed or replaced) and removed, by id.
    pub languages_placed: Vec<String>,
    pub languages_removed: Vec<String>,
}

/// Everything the device script does, in the order it does it: stop,
/// disable, unmount, remove, place, write, enable, start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Every archive the script needs, downloaded before anything changes.
    pub archives: BTreeSet<String>,
    pub ensure_user: bool,
    pub stop_service: bool,
    pub disable_units: Vec<String>,
    pub unmount_roots: Vec<String>,
    /// Mount roots removed themselves, with `rmdir` and nothing else, once
    /// `unmount_roots` has emptied them (R55). Only a total removal sets it:
    /// an install, an update and the removal of just `files` leave the root.
    pub remove_mount_roots: Vec<String>,
    pub remove_files: Vec<String>,
    pub remove_trees: Vec<String>,
    pub puts: Vec<Put>,
    pub initial: Vec<Initial>,
    /// `(archive, pack id)`.
    pub packs: Vec<(String, String)>,
    /// The new content of `plugins.toml`; `None` leaves the file alone (or
    /// removes it, with `remove_plugins_toml`).
    pub plugins_toml: Option<String>,
    pub remove_plugins_toml: bool,
    /// `Some` is written; `None` with `remove_registry` deletes it.
    pub registry: Option<Registry>,
    /// Written after the removals and before the first file is placed
    /// (R42): every privileged path the device's old registry recorded or
    /// this plan places, so a run that stops halfway still leaves a
    /// registry that knows every file it may have placed. `registry`
    /// replaces it at the end. `None` for a total removal.
    pub provisional_registry: Option<Registry>,
    pub remove_registry: bool,
    pub enable_units: Vec<String>,
    pub start_service: bool,
    pub remove_user: bool,
    /// The device already is what was asked: nothing to place, extract or
    /// remove, `plugins.toml` and the registry unchanged. The run says so
    /// and stops before the confirmation, the sudo password, the downloads
    /// and the apply; the service is neither stopped nor started.
    pub nothing_to_do: bool,
    pub summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    UnknownPlugin(String),
    UnknownPack(String),
    InvalidName(String),
    NotDeletable(String),
    ThirdPartyExecOutsidePluginsDir { name: String, exec: String },
    EraseNotRemoved(String),
    UnknownArch(String),
    NoSystemd,
    /// The device's `plugins.toml` could not be rewritten, or a plugin the
    /// inventory ships carries no block to insert.
    PluginsToml(String),
    /// The inventory names a unit or a mount root outside the shapes
    /// Ritornello ships.
    InvalidInventory(String),
    /// A path under the data tree would go without its plugin's data having
    /// been named for erasure.
    DataNotAsked(String),
    /// `plugins.toml` declares a plugin under the name of a component that
    /// ships beside a plugin (`files-mount`), which is never a plugin.
    DeclaredUnderCompanionName(String),
    /// `plugins.toml` declares a plugin under a language pack's id
    /// (`ritornello-lang-fr`), which keys that pack's registry record.
    DeclaredUnderPackId(String),
}

/// One sentence per refusal, naming what is refused and what to do about
/// it: a single refusal blocks the whole plan, so the operator must be able
/// to act on the message alone.
impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPlugin(n) => write!(
                f,
                "no plugin named {n:?} in this release or on the device: check its spelling, or leave it out"
            ),
            Self::UnknownPack(l) => write!(
                f,
                "no language pack for {l:?} in this release or on the device: check its spelling, or leave it out"
            ),
            Self::InvalidName(n) => {
                write!(f, "{n:?} is not a valid plugin name or language: correct it and run again")
            }
            // The one refusal an operator can cause by hand: a directory
            // dropped among the packs, which the survey lists whatever it is.
            Self::NotDeletable(p) if p.strip_prefix(PACKS_ROOT).is_some_and(|r| r.starts_with('/')) => write!(
                f,
                "{p:?} is not a language pack, and it is not removed: move it out of {PACKS_ROOT} by hand, \
                 then run again"
            ),
            Self::NotDeletable(p) => write!(
                f,
                "refusing to remove {p:?}, which is outside Ritornello's own places: the device's \
                 {} or the release names it, so inspect that file before running again",
                names::REGISTRY
            ),
            Self::ThirdPartyExecOutsidePluginsDir { name, exec } => write!(
                f,
                "third-party plugin {name:?} runs {exec:?}, outside {PLUGINS_DIR}, which is never deleted: \
                 keep {name:?}, or remove its [[plugin]] entry from {} by hand",
                names::PLUGINS_TOML
            ),
            Self::EraseNotRemoved(n) => {
                write!(f, "the data of {n:?} is erased only when {n:?} is removed: remove it too, or keep its data")
            }
            Self::UnknownArch(m) => write!(
                f,
                "no archive for this device's architecture ({m:?}): Ritornello ships for armv7l, aarch64 or x86_64"
            ),
            Self::NoSystemd => {
                write!(f, "this device does not run systemd, which Ritornello needs: install on a Linux that runs systemd")
            }
            Self::PluginsToml(d) => write!(
                f,
                "{} cannot be rewritten ({d}): fix the file by hand, then run again",
                names::PLUGINS_TOML
            ),
            Self::InvalidInventory(d) => write!(
                f,
                "the release inventory names something Ritornello never ships ({d}): it is not trusted, \
                 choose another release"
            ),
            Self::DataNotAsked(p) => write!(
                f,
                "refusing to remove {p:?}, which holds a plugin's data that was not asked to be erased: \
                 the device's {} or the release names it, so inspect that file before running again",
                names::REGISTRY
            ),
            Self::DeclaredUnderCompanionName(n) => write!(
                f,
                "{} declares a plugin named {n:?}, which is the name of a component Ritornello installs \
                 beside a plugin, never a plugin: remove that [[plugin]] block from {} by hand, then run again",
                names::PLUGINS_TOML,
                names::PLUGINS_TOML
            ),
            Self::DeclaredUnderPackId(n) => write!(
                f,
                "{} declares a plugin named {n:?}, which is the name of a language pack, never a plugin: \
                 remove that [[plugin]] block from {} by hand, then run again",
                names::PLUGINS_TOML,
                names::PLUGINS_TOML
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// Turns what the device has and what the operator chose into a plan, or
/// into the one reason there is none.
pub fn compute(inv: &Inventory, dev: &DeviceState, intent: &Intent) -> Result<Plan, PlanError> {
    if !dev.systemd {
        return Err(PlanError::NoSystemd);
    }
    let arch = dev.arch_label().ok_or_else(|| PlanError::UnknownArch(dev.machine.clone()))?;
    validate_inventory(inv)?;
    // A companion's name is its registry key: a `plugins.toml` entry of the
    // same name would be taken for a third-party plugin, and keeping it
    // would carry the companion's record over after its files are gone.
    if let Some(d) = dev.declared.iter().find(|d| inv.companions.iter().any(|c| c.name == d.name)) {
        return Err(PlanError::DeclaredUnderCompanionName(d.name.clone()));
    }
    // The same for a language pack's id, which keys the pack's record: a
    // third-party plugin of that name, kept, would carry the pack's record
    // over as its own, or lose it.
    if let Some(d) = dev.declared.iter().find(|d| language_of(&d.name).is_some()) {
        return Err(PlanError::DeclaredUnderPackId(d.name.clone()));
    }
    match intent {
        Intent::RemoveAll { erase_data } => remove_all(inv, dev, *erase_data),
        Intent::InstallOrUpdate { plugins, packs, erase_data, reinstall } => {
            install_or_update(inv, dev, arch, plugins, packs, erase_data, *reinstall)
        }
    }
}

/// What the operator is offered first: every plugin `plugins.toml`
/// declares, ours and third-party ones alike, and the language of every
/// pack present. Both empty on a fresh device.
pub fn preselection(inv: &Inventory, dev: &DeviceState) -> (BTreeSet<String>, BTreeSet<String>) {
    let _ = inv;
    let plugins = dev.declared.iter().map(|d| d.name.clone()).collect();
    let packs = dev.packs.iter().filter_map(|id| language_of(id)).map(str::to_string).collect();
    (plugins, packs)
}

/// The language a pack directory's name carries, if it is shaped like one.
fn language_of(pack_id: &str) -> Option<&str> {
    pack_id.strip_prefix("ritornello-lang-").filter(|l| names::valid_language(l))
}

/// Whether `exec` is a file directly inside the plugins directory — not
/// nested below it, not beside it, and named with `names`' own byte rule
/// (no `.`, no `..`, nothing a shell would read twice). `deletable_file`
/// carries that rule; for a path under `/usr/local/lib/ritornello/` it
/// answers exactly "is every segment clean".
fn in_plugins_dir(exec: &str) -> bool {
    exec.strip_prefix(PLUGINS_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|file| !file.is_empty() && !file.contains('/'))
        && names::deletable_file(exec)
}

/// The inventory is the release's word, but its unit names reach
/// `systemctl` and its mount roots reach `umount`/`rmdir` as root: both are
/// held to the shapes Ritornello actually ships before anything is planned.
fn validate_inventory(inv: &Inventory) -> Result<(), PlanError> {
    // A companion's name keys its registry record, and its `with` decides
    // when it is placed and when removed: a name another component already
    // uses would merge two records into one, and a `with` naming no plugin
    // of the release would leave it placed by nothing and removed by nothing.
    for c in &inv.companions {
        if !names::valid_plugin_name(&c.name) {
            return Err(PlanError::InvalidInventory(format!("companion name {:?}", c.name)));
        }
        let taken = c.name == inv.core.name
            || inv.plugin(&c.name).is_some()
            || inv.companions.iter().filter(|o| o.name == c.name).count() > 1;
        if taken {
            return Err(PlanError::InvalidInventory(format!("companion {:?}: its name is already another component's", c.name)));
        }
        if inv.plugin(&c.with).is_none() {
            return Err(PlanError::InvalidInventory(format!(
                "companion {:?}: ships with {:?}, which the release does not ship",
                c.name, c.with
            )));
        }
    }
    let companions: Vec<Component> = inv.companions.iter().map(Companion::as_component).collect();
    for c in std::iter::once(&inv.core).chain(inv.plugins.iter()).chain(companions.iter()) {
        for u in &c.enable {
            // `deletable_file` under the systemd directory is exactly the
            // unit-name shape (`ritornello.service`, `ritornello-<x>.service`).
            if !names::deletable_file(&format!("{SYSTEMD_DIR}{u}")) {
                return Err(PlanError::InvalidInventory(format!("{}: unit {u:?}", c.name)));
            }
        }
        if let Some(root) = &c.mount_root {
            // Exactly the one root the release ships (R40): a sub-root that
            // is itself a mounted share would have the script `rmdir` the
            // NAS's own empty directories.
            if root != MOUNT_BASE {
                return Err(PlanError::InvalidInventory(format!("{}: mount root {root:?}", c.name)));
            }
        }
    }
    Ok(())
}

/// A third-party plugin's binary, removable only from the plugins
/// directory: anywhere else it may be a system binary the plugin merely
/// runs (`/usr/bin/mpv`), and it is not ours to delete.
fn third_party_binary(name: &str, exec: &str) -> Result<String, PlanError> {
    if in_plugins_dir(exec) {
        Ok(exec.to_string())
    } else {
        Err(PlanError::ThirdPartyExecOutsidePluginsDir { name: name.to_string(), exec: exec.to_string() })
    }
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// Whose data a plan may touch under `/var/lib/ritornello`.
enum DataScope<'a> {
    /// Only `DATA_ROOT/<name>` for these names (an install or update; a
    /// total removal without its data, with an empty set).
    Only(&'a BTreeSet<String>),
    /// All of it: a total removal with its data.
    All,
}

/// The data tree's own root, which only a total removal with its data may
/// remove, and under which nothing else is ever removed unless asked.
const DATA_TREE: &str = "/var/lib/ritornello";

/// The last steps every plan shares.
///
/// **Invariant (R31): a plan never removes a path something it keeps still
/// uses** — the declared `exec` of a plugin it keeps (ours or a third
/// party's: two entries may share a binary), or the `dest` of a file this
/// same plan places (a remove-then-place of one path would otherwise hinge
/// on the script's step order). Such paths are dropped from `remove_files`
/// before anything else looks at it.
///
/// A unit file the plan removes is then disabled first, whatever the
/// inventory's `enable` says, so a stale unit never stays enabled with its
/// file gone. Then every path is checked against Ritornello's own places,
/// and against "data only when asked"; one refusal and there is no plan.
fn finish(
    mut plan: Plan,
    mut remove_files: BTreeSet<String>,
    kept_execs: &BTreeSet<String>,
    data: DataScope<'_>,
) -> Result<Plan, PlanError> {
    let placed: BTreeSet<&str> = plan.puts.iter().map(|p| p.dest.as_str()).collect();
    remove_files.retain(|f| !kept_execs.contains(f) && !placed.contains(f.as_str()));
    for f in &remove_files {
        if let Some(unit) = f.strip_prefix(SYSTEMD_DIR) {
            push_unique(&mut plan.disable_units, unit);
        }
    }
    plan.remove_files = remove_files.into_iter().collect();
    for f in &plan.remove_files {
        if !names::deletable_file(f) {
            return Err(PlanError::NotDeletable(f.clone()));
        }
    }
    for t in &plan.remove_trees {
        if !names::deletable_tree(t) {
            return Err(PlanError::NotDeletable(t.clone()));
        }
    }
    // Data only when asked, enforced rather than incidental: whatever put a
    // path under the data tree into this plan (a registry entry, a future
    // inventory), it goes only if its plugin's data was named for erasure.
    if let DataScope::Only(erase) = data {
        let asked = |p: &str| {
            p.strip_prefix(DATA_ROOT)
                .and_then(|r| r.strip_prefix('/'))
                .map(|r| r.split('/').next().unwrap_or(""))
                .is_some_and(|owner| erase.contains(owner))
        };
        let in_data = |p: &str| p == DATA_TREE || p.starts_with(&format!("{DATA_TREE}/"));
        for p in plan.remove_files.iter().chain(&plan.remove_trees) {
            if in_data(p) && !asked(p) {
                return Err(PlanError::DataNotAsked(p.clone()));
            }
        }
    }
    Ok(plan)
}

/// Whether the device already has `name` at `offered`, with exactly the
/// privileged files `identity` names, each recorded as that same content, so
/// that placing it again would change nothing.
///
/// **The trust rule: only root-owned data may justify a skip.** Skipping is
/// the one decision here that makes the installer do *less* as root, so it
/// rests on `/var/lib/ritornello-install/installed.toml` alone (`recorded`),
/// which only root writes (root:root 0644). Everything the unprivileged
/// `ritornello` account can write — the in-app updater's `placed.json`
/// (`untrusted`), `plugins.toml`, a pack's `pack.toml` — may only ever push
/// toward reinstalling, never toward skipping: an account that could forge
/// a version there would otherwise keep a stale or tampered root unit, rule
/// or binary in place across every run meant to repair it. Concretely:
///
/// - no record, another version recorded, another set of privileged files
///   recorded, or one recorded with another identity (none at all, for a
///   registry written before identities existed): not current, whatever
///   anything else says. Placing it again is what records the identities
///   the core compares before updating itself from the page;
/// - a record that matches, but an updater memory naming another version
///   for the same component (the updater moved its binary without telling
///   the registry): not current either;
/// - an updater memory that *agrees* adds nothing: it is never consulted
///   for a yes, so a forged one can never turn a no into one.
///
/// Whether the component is on the device at all is the caller's
/// precondition (`was_there`), and is likewise only ever a reason to place.
fn is_current(
    recorded: &BTreeMap<String, Recorded>,
    untrusted: &BTreeMap<String, String>,
    name: &str,
    offered: &str,
    identity: &BTreeMap<String, String>,
) -> bool {
    let Some(rec) = recorded.get(name) else { return false };
    let same_files = {
        let a: BTreeSet<&String> = rec.privileged.iter().collect();
        let b: BTreeSet<&String> = identity.keys().collect();
        a == b
    };
    if rec.version != offered || !same_files || rec.identity != *identity {
        return false;
    }
    untrusted.get(name).is_none_or(|v| v == offered)
}

/// Two registries that record the same components at the same versions
/// with the same privileged files and identities, in whatever order:
/// rewriting one with the other changes nothing the installer or the core
/// ever reads.
fn same_registry(a: &Registry, b: &Registry) -> bool {
    let files = |r: &Recorded| r.privileged.iter().cloned().collect::<BTreeSet<String>>();
    a.format == b.format
        && a.components.len() == b.components.len()
        && a.components.iter().all(|(name, ra)| {
            b.components.get(name).is_some_and(|rb| {
                ra.version == rb.version && files(ra) == files(rb) && ra.identity == rb.identity
            })
        })
}

fn install_or_update(
    inv: &Inventory,
    dev: &DeviceState,
    arch: &str,
    plugins: &BTreeSet<String>,
    packs: &BTreeSet<String>,
    erase_data: &BTreeSet<String>,
    reinstall: bool,
) -> Result<Plan, PlanError> {
    let ours: BTreeSet<&str> = inv.plugins.iter().map(|p| p.name.as_str()).collect();
    let declared: BTreeSet<&str> = dev.declared.iter().map(|d| d.name.as_str()).collect();

    // 1. Names first, before any of them is looked up or joined to a path.
    // Only where a name forms a path or is written (R32): `erase_data`
    // names form `DATA_ROOT/<name>`, languages form a pack id, and a chosen
    // name is validated unless it is a third party's already declared —
    // that one is merely kept, never joined to anything, and its own
    // spelling (`Acme_Widget`) is not ours to refuse.
    let merely_kept = |n: &str| declared.contains(n) && !ours.contains(n);
    if let Some(bad) = plugins
        .iter()
        .filter(|n| !merely_kept(n))
        .chain(erase_data)
        .find(|n| !names::valid_plugin_name(n))
    {
        return Err(PlanError::InvalidName(bad.clone()));
    }
    if let Some(bad) = packs.iter().find(|l| !names::valid_language(l)) {
        return Err(PlanError::InvalidName(bad.clone()));
    }

    // 2. Ours, declared, third-party.
    if let Some(unknown) = plugins.iter().find(|n| !ours.contains(n.as_str()) && !declared.contains(n.as_str())) {
        return Err(PlanError::UnknownPlugin(unknown.clone()));
    }
    // A pack the release does not ship but the device has (a third party's,
    // or one an older release carried) is kept when chosen, exactly as a
    // third-party plugin is: the preselection offers it.
    let shipped = |l: &str| inv.packs.iter().find(|p| p.language == l);
    if let Some(unknown) = packs.iter().find(|l| shipped(l).is_none() && !dev.packs.contains(&names::pack_id(l))) {
        return Err(PlanError::UnknownPack(unknown.clone()));
    }

    let recorded: BTreeMap<String, Recorded> =
        dev.registry.as_ref().map(|r| r.components.clone()).unwrap_or_default();

    // 3. What goes, and whose data may go with it. A plugin of ours is on
    // the device when `plugins.toml` declares it **or the registry records
    // it** (R30): one whose block was removed by hand, or by an older UI,
    // still has its root unit enabled at boot and its polkit rule granting,
    // and only the registry remembers them.
    // A plugin whose privileged files moved to a companion records nothing
    // itself: the companion's record is what remembers it.
    let kept_ours: Vec<&Component> = inv.plugins.iter().filter(|p| plugins.contains(&p.name)).collect();
    let companion_recorded =
        |plugin: &str| inv.companions.iter().any(|c| c.with == plugin && recorded.contains_key(&c.name));
    let removed_ours: Vec<&Component> = inv
        .plugins
        .iter()
        .filter(|p| declared.contains(p.name.as_str()) || recorded.contains_key(&p.name) || companion_recorded(&p.name))
        .filter(|p| !plugins.contains(&p.name))
        .collect();
    let kept_third: Vec<&str> = dev
        .declared
        .iter()
        .map(|d| d.name.as_str())
        .filter(|n| !ours.contains(n) && plugins.contains(*n))
        .collect();
    let removed_third: Vec<(&str, &str)> = dev
        .declared
        .iter()
        .filter(|d| !ours.contains(d.name.as_str()) && !plugins.contains(&d.name))
        .map(|d| (d.name.as_str(), d.exec.as_str()))
        .collect();
    let removed: BTreeSet<&str> =
        removed_ours.iter().map(|c| c.name.as_str()).chain(removed_third.iter().map(|(n, _)| *n)).collect();
    if let Some(kept) = erase_data.iter().find(|n| !removed.contains(n.as_str())) {
        return Err(PlanError::EraseNotRemoved(kept.clone()));
    }

    // A companion follows its plugin, and is never chosen on its own: it is
    // placed whenever its plugin is kept, and goes whenever its plugin goes
    // (a recorded companion puts its plugin among those that go, above).
    let companions: Vec<(Component, &str)> =
        inv.companions.iter().map(|c| (c.as_component(), c.with.as_str())).collect();
    let with_of: BTreeMap<&str, &str> = companions.iter().map(|(c, w)| (c.name.as_str(), *w)).collect();
    // Each plugin, followed by its companions.
    let mut placed: Vec<&Component> = vec![&inv.core];
    for p in &kept_ours {
        placed.push(p);
        placed.extend(companions.iter().filter(|(_, w)| *w == p.name).map(|(c, _)| c));
    }
    let mut going: Vec<&Component> = Vec::new();
    // A plugin of ours only an unprivileged record remembers — not declared,
    // nothing privileged recorded for it, no companion recorded: since every
    // plugin placed is recorded, this is one uninstalled from the web
    // interface. Its leftovers go like any removed plugin's, but the summary
    // says what actually happens: it was already gone, and its record is
    // cleared. (A plugin whose root files are still recorded is removed in
    // earnest, R30, and said so.)
    let already_gone = |p: &Component| {
        !declared.contains(p.name.as_str())
            && !companion_recorded(&p.name)
            && recorded.get(&p.name).is_none_or(|r| r.privileged.is_empty())
    };
    let mut cleared: BTreeSet<&str> = BTreeSet::new();
    for p in &removed_ours {
        going.push(p);
        let its_companions: Vec<&Component> =
            companions.iter().filter(|(_, w)| *w == p.name).map(|(c, _)| c).collect();
        if already_gone(p) {
            cleared.insert(p.name.as_str());
            cleared.extend(its_companions.iter().map(|c| c.name.as_str()));
        }
        going.extend(its_companions);
    }

    let mut plan = Plan::default();
    let mut remove_files = BTreeSet::new();
    let mut new_registry = BTreeMap::new();

    // 4. The core, every kept plugin of ours and each one's companions:
    // placed in full, unless the device already has it as offered.
    for c in placed {
        // 10. The registry records every component placed, with its
        // version and what it places privileged (none, for most plugins):
        // the version is what lets the next run leave it alone.
        let privileged: Vec<String> = c.files.iter().filter(|f| f.privileged).map(|f| f.dest.clone()).collect();
        // `Inventory::parse` refused a privileged file without one.
        let identity: BTreeMap<String, String> = c
            .files
            .iter()
            .filter(|f| f.privileged)
            .filter_map(|f| Some((f.dest.clone(), f.identity.clone()?)))
            .collect();
        // A companion was there with its plugin, or on its own record.
        let was_there = if c.name == inv.core.name {
            dev.core_present
        } else if let Some(with) = with_of.get(c.name.as_str()) {
            declared.contains(with) || recorded.contains_key(&c.name)
        } else {
            declared.contains(c.name.as_str())
        };
        let current =
            !reinstall && was_there && is_current(&recorded, &dev.updater_placed, &c.name, &c.version, &identity);
        if !current {
            let archive = c.archive_for(arch);
            plan.archives.insert(archive.clone());
            for f in &c.files {
                plan.puts.push(Put {
                    archive: archive.clone(),
                    archive_path: f.archive_path.clone(),
                    dest: f.dest.clone(),
                    mode: f.mode.clone(),
                    owner: f.owner.clone(),
                });
            }
            for i in &c.initial_config {
                plan.initial.push(Initial {
                    archive: archive.clone(),
                    archive_path: i.archive_path.clone(),
                    plugin: c.name.clone(),
                    target: i.target.clone(),
                });
            }
        }
        // Enabled even when left as it is: `systemctl enable` of an enabled
        // unit changes nothing, and the list is what the script re-asserts.
        for u in &c.enable {
            push_unique(&mut plan.enable_units, u);
        }
        // R27: what the previous version placed and this one no longer does.
        // (A current component recorded exactly these files: nothing here.)
        if let Some(old) = recorded.get(&c.name) {
            for p in &old.privileged {
                if !c.files.iter().any(|f| &f.dest == p) {
                    remove_files.insert(p.clone());
                }
            }
        }
        new_registry.insert(c.name.clone(), Recorded { version: c.version.clone(), privileged, identity });
        if current {
            plan.summary.up_to_date.push(c.name.clone());
        } else if was_there {
            plan.summary.updated.push(c.name.clone());
        } else {
            plan.summary.installed.push(c.name.clone());
        }
    }

    // 5. Every plugin of ours that goes: the union of what the inventory
    // places (its binary included, for one the registry alone remembers:
    // the file may be gone already, and the script's `rm -f` does not mind)
    // and what the registry recorded, plus every binary a declared `exec`
    // names for it — which is what the core actually ran, once per entry
    // when the name is declared twice. Each one's companions go with it, in
    // the same way.
    for c in going {
        for u in &c.enable {
            push_unique(&mut plan.disable_units, u);
        }
        if let Some(root) = &c.mount_root {
            push_unique(&mut plan.unmount_roots, root);
        }
        for f in &c.files {
            remove_files.insert(f.dest.clone());
        }
        if let Some(old) = recorded.get(&c.name) {
            remove_files.extend(old.privileged.iter().cloned());
        }
        for d in dev.declared.iter().filter(|d| d.name == c.name) {
            if in_plugins_dir(&d.exec) {
                remove_files.insert(d.exec.clone());
            }
        }
        if cleared.contains(c.name.as_str()) {
            plan.summary.cleared.push(c.name.clone());
        } else {
            plan.summary.removed.push(c.name.clone());
        }
    }

    // 6. Every third-party plugin that goes: its binary, from the plugins
    // directory only.
    for (name, exec) in &removed_third {
        remove_files.insert(third_party_binary(name, exec)?);
        plan.summary.removed.push(name.to_string());
    }
    plan.summary.kept_third_party = kept_third.iter().map(|n| n.to_string()).collect();

    // 7. Data, only of what goes, only when asked.
    for n in erase_data {
        if dev.data_nonempty.contains(n) {
            plan.remove_trees.push(format!("{DATA_ROOT}/{n}"));
            plan.summary.erased.push(n.clone());
        }
    }

    // 8. Language packs: exactly the chosen ones. Each is recorded in the
    // registry under its pack id (`ritornello-lang-fr`), with its version and
    // no privileged file, so that a pack already there at the offered
    // version is left alone. The pack's own `pack.toml` also carries a
    // version, but the pack directory belongs to the `ritornello` account:
    // by `is_current`'s trust rule it could only ever argue for replacing
    // the pack, so it is not read at all. The directory being there is
    // likewise only a precondition — a recorded pack whose directory is gone
    // is placed again.
    let wanted_ids: BTreeSet<String> = packs.iter().map(|l| names::pack_id(l)).collect();
    for l in packs {
        let id = names::pack_id(l);
        if let Some(p) = shipped(l) {
            let current = !reinstall
                && dev.packs.contains(&id)
                && is_current(&recorded, &BTreeMap::new(), &id, &p.version, &BTreeMap::new());
            if current {
                plan.summary.up_to_date.push(id.clone());
            } else {
                plan.archives.insert(p.archive.clone());
                plan.packs.push((p.archive.clone(), id.clone()));
                plan.summary.languages_placed.push(id.clone());
            }
            new_registry.insert(id, Recorded { version: p.version.clone(), privileged: Vec::new(), identity: BTreeMap::new() });
        } else if let Some(old) = recorded.get(&id) {
            // Kept as it is, not shipped by this release: so is its record.
            new_registry.insert(id, old.clone());
        }
    }
    // Ours only: a third-party pack is the core's, installed from a source
    // the operator added, and is neither offered here nor removed. Anything
    // else unexpected still goes to `deletable_tree`, which refuses it.
    for id in &dev.packs {
        if !wanted_ids.contains(id) && !names::third_party_pack_id(id) {
            plan.remove_trees.push(format!("{PACKS_ROOT}/{id}"));
            plan.summary.languages_removed.push(id.clone());
        }
    }

    // A recorded component nothing keeps any more — gone from the release
    // and not a third-party plugin still chosen — loses its files. One that
    // is still chosen as a third-party plugin keeps its entry as it was. (A
    // companion whose plugin goes lands here too, and its recorded files are
    // already in `remove_files`; so does a pack no longer chosen, whose
    // record says it places nothing privileged.)
    for (name, old) in &recorded {
        if new_registry.contains_key(name) || name == &inv.core.name || ours.contains(name.as_str()) {
            continue;
        }
        if kept_third.contains(&name.as_str()) {
            new_registry.insert(name.clone(), old.clone());
        } else {
            remove_files.extend(old.privileged.iter().cloned());
            // A pack recorded but no longer on the device (removed from the
            // web interface): only its record goes, and the summary says so
            // rather than leaving that registry write unexplained.
            if language_of(name).is_some() && !dev.packs.contains(name) {
                plan.summary.cleared.push(name.clone());
            }
        }
    }

    // 9. plugins.toml: blocks out for what goes and is declared, blocks in,
    // where the reference order puts them, for what arrives — a plugin the
    // registry alone remembered included. Untouched otherwise, except that
    // a device with no file at all gets one: the core starts without it,
    // but its own update worker reads it before declaring a plugin
    // installed from the web UI, and refuses when it is absent.
    let unblock: Vec<&str> = removed.iter().copied().filter(|n| declared.contains(n)).collect();
    let arriving: Vec<&Component> = kept_ours.iter().copied().filter(|c| !declared.contains(c.name.as_str())).collect();
    if unblock.is_empty() && arriving.is_empty() {
        plan.plugins_toml = Some(dev.plugins_toml.clone().unwrap_or_else(|| PLUGINS_TOML_HEADER.to_string()));
    } else {
        let edit = |e: ritornello_manifest::EditError| PlanError::PluginsToml(e.to_string());
        let reference: Vec<&str> = inv.reference_order.iter().map(String::as_str).collect();
        let mut text = dev.plugins_toml.clone().unwrap_or_else(|| PLUGINS_TOML_HEADER.to_string());
        for name in &unblock {
            // A name declared twice has two blocks; `remove_entry` takes one
            // per call, so go on until none is left. The first call must
            // succeed (the name is declared); a later refusal only means
            // there is nothing more to remove.
            text = ritornello_manifest::remove_entry(&text, name).map_err(edit)?;
            while let Ok(next) = ritornello_manifest::remove_entry(&text, name) {
                text = next;
            }
        }
        for c in &arriving {
            let block = c
                .block
                .as_deref()
                .ok_or_else(|| PlanError::PluginsToml(format!("the release carries no block for {:?}", c.name)))?;
            text = ritornello_manifest::insert_block_in_reference_order(&text, block, &c.name, &reference)
                .map_err(edit)?;
        }
        plan.plugins_toml = Some(text);
    }

    plan.provisional_registry = Some(provisional(&recorded, &new_registry));
    plan.registry = Some(Registry { format: 1, components: new_registry });

    // 11.
    plan.ensure_user = true;
    plan.stop_service = dev.core_present;
    plan.start_service = true;

    // 12.
    // Only a binary in the plugins directory is something a kept entry
    // "still runs" (R35). `plugins.toml` is writable without root, so an
    // `exec` pointing anywhere else — at a polkit rule, a unit, a helper —
    // must never shield it: it would stay granting, or enabled, and the new
    // registry would forget it. No privileged file lives in that directory.
    let kept_execs: BTreeSet<String> = dev
        .declared
        .iter()
        .filter(|d| plugins.contains(&d.name) && in_plugins_dir(&d.exec))
        .map(|d| d.exec.clone())
        .collect();
    let plan = finish(plan, remove_files, &kept_execs, DataScope::Only(erase_data))?;
    Ok(settle(plan, dev))
}

/// What is left to do once every component and pack has been weighed.
///
/// Nothing placed, extracted, disabled, unmounted or removed, and
/// `plugins.toml` written back as it is: the core has nothing new to start
/// with, so the service is neither stopped nor restarted (and the account,
/// which the survey saw, is not ensured). Then either the registry is
/// unchanged too — the device already is what was asked, `nothing_to_do` —
/// or only a record differs (a pack or a plugin uninstalled from the web
/// interface, whose record is cleared), and the run writes the registry
/// alone. Checked on the finished plan, after `finish`, so that nothing
/// the plan does can be missed by this test.
fn settle(mut plan: Plan, dev: &DeviceState) -> Plan {
    let quiet = plan.archives.is_empty()
        && plan.puts.is_empty()
        && plan.initial.is_empty()
        && plan.packs.is_empty()
        && plan.disable_units.is_empty()
        && plan.unmount_roots.is_empty()
        && plan.remove_mount_roots.is_empty()
        && plan.remove_files.is_empty()
        && plan.remove_trees.is_empty()
        && !plan.remove_plugins_toml
        && !plan.remove_registry
        && !plan.remove_user
        && plan.plugins_toml == dev.plugins_toml
        && dev.user_exists;
    if !quiet {
        return plan;
    }
    plan.ensure_user = false;
    plan.stop_service = false;
    plan.start_service = false;
    let unchanged = match (&plan.registry, &dev.registry) {
        (Some(new), Some(old)) => same_registry(new, old),
        _ => false,
    };
    if unchanged {
        plan.nothing_to_do = true;
    }
    plan
}

/// The union of what the device recorded and what this plan records: per
/// component, the new version when there is one, and every privileged path
/// either side names.
///
/// **Identities are the one thing it does not unite.** This record is
/// written before anything is placed, and stays if the run stops halfway; the
/// core reads identities as a promise of what is on the disk. So it keeps only
/// an identity both sides agree on: one this plan would change is dropped —
/// unknown, which makes the core refuse until a run completes — and one only
/// the plan names is not recorded yet.
fn provisional(old: &BTreeMap<String, Recorded>, new: &BTreeMap<String, Recorded>) -> Registry {
    let mut components = old.clone();
    for (name, rec) in new {
        let entry = components
            .entry(name.clone())
            .or_insert_with(|| Recorded { version: rec.version.clone(), privileged: Vec::new(), identity: BTreeMap::new() });
        entry.version = rec.version.clone();
        for p in &rec.privileged {
            if !entry.privileged.contains(p) {
                entry.privileged.push(p.clone());
            }
        }
        entry.identity.retain(|path, id| rec.identity.get(path) == Some(id));
    }
    Registry { format: 1, components }
}

fn remove_all(inv: &Inventory, dev: &DeviceState, erase_data: bool) -> Result<Plan, PlanError> {
    let mut plan = Plan { stop_service: dev.core_present, ..Plan::default() };
    let mut remove_files = BTreeSet::new();
    let ours: BTreeSet<&str> = inv.plugins.iter().map(|p| p.name.as_str()).collect();
    let companions: Vec<Component> = inv.companions.iter().map(Companion::as_component).collect();

    for c in std::iter::once(&inv.core).chain(inv.plugins.iter()).chain(companions.iter()) {
        for u in &c.enable {
            push_unique(&mut plan.disable_units, u);
        }
        if let Some(root) = &c.mount_root {
            push_unique(&mut plan.unmount_roots, root);
        }
        remove_files.extend(c.files.iter().map(|f| f.dest.clone()));
    }
    if let Some(reg) = &dev.registry {
        for old in reg.components.values() {
            remove_files.extend(old.privileged.iter().cloned());
        }
    }
    for d in &dev.declared {
        if ours.contains(d.name.as_str()) {
            // Ours: the inventory's own binary is already listed; an `exec`
            // that names another file in the plugins directory goes too.
            if in_plugins_dir(&d.exec) {
                remove_files.insert(d.exec.clone());
            }
        } else {
            remove_files.insert(third_party_binary(&d.name, &d.exec)?);
        }
    }

    plan.remove_trees = [
        "/etc/ritornello",
        "/usr/local/lib/ritornello",
        "/var/lib/ritornello-update",
        "/var/lib/ritornello-install",
    ]
    .iter()
    .map(|t| t.to_string())
    .collect();
    if erase_data {
        plan.remove_trees.push("/var/lib/ritornello".to_string());
        plan.summary.erased = dev.data_nonempty.iter().cloned().collect();
    }
    plan.remove_user = erase_data && dev.user_exists;
    plan.remove_mount_roots = vec![MOUNT_BASE.to_string()];
    plan.remove_plugins_toml = true;
    plan.remove_registry = true;
    if dev.core_present {
        plan.summary.removed.push(inv.core.name.clone());
    }
    plan.summary.removed.extend(dev.declared.iter().map(|d| d.name.clone()));
    // A companion was on the device with its plugin, or on its own record.
    let recorded = |n: &str| dev.registry.as_ref().is_some_and(|r| r.components.contains_key(n));
    for c in &inv.companions {
        if dev.declared.iter().any(|d| d.name == c.with) || recorded(&c.name) {
            plan.summary.removed.push(c.name.clone());
        }
    }

    let none = BTreeSet::new();
    let data = if erase_data { DataScope::All } else { DataScope::Only(&none) };
    finish(plan, remove_files, &BTreeSet::new(), data)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::device::{Declared, Sudo};
    use serde_json::{Value, json};

    /// The registry key the core's privileged files are recorded under: the
    /// inventory's own name for it (`inv.core.name`), which the plan reads.
    /// The core is always installed and always kept; it is never one of
    /// `plugins`.
    const CORE: &str = "core";

    pub(crate) const RADIO_EXEC: &str = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio";
    pub(crate) const CD_EXEC: &str = "/usr/local/lib/ritornello/plugins/ritornello-plugin-cd";
    const FILES_EXEC: &str = "/usr/local/lib/ritornello/plugins/ritornello-plugin-files";
    pub(crate) const THEIRS_EXEC: &str = "/usr/local/lib/ritornello/plugins/theirs-bin";
    const MEDIA_UNIT: &str = "/etc/systemd/system/ritornello-media-mount.service";
    const MEDIA_RULE: &str = "/etc/polkit-1/rules.d/51-ritornello-media.rules";
    const MEDIA_HELPER: &str = "/usr/local/lib/ritornello/ritornello-media-mount";
    /// The files plugin's companion, and its registry key.
    const MOUNT: &str = "files-mount";
    const MOUNT_ARCHIVE: &str = "ritornello-files-mount-0.2.0-beta.2-arm64.tar.gz";

    /// One `files` entry in `install-inventory.py`'s own shape.
    fn file(path: &str, mode: &str, owner: &str, privileged: bool) -> Value {
        let mut entry = json!({
            "archive_path": path,
            "dest": format!("/{path}"),
            "mode": mode,
            "owner": owner,
            "privileged": privileged,
        });
        if privileged {
            entry["identity"] = json!(test_identity(&format!("/{path}")));
        }
        entry
    }

    /// The identity `inv()` gives the privileged file placed at `dest`, and
    /// the one a registry records for it once placed.
    pub(crate) fn test_identity(dest: &str) -> String {
        format!("sha256:fixture{dest}")
    }

    /// `paths` with the identity `inv()` gives each.
    pub(crate) fn identities(paths: &[&str]) -> BTreeMap<String, String> {
        paths.iter().map(|p| (p.to_string(), test_identity(p))).collect()
    }

    /// A plugin component in the real shape: its binary under the plugins
    /// directory, then `extra` files, and the block `install-inventory.py`
    /// writes for it.
    fn plugin(name: &str, extra: Vec<Value>, initial: Value, enable: Value, mount_root: Value) -> Value {
        let mut files = vec![file(
            &format!("usr/local/lib/ritornello/plugins/ritornello-plugin-{name}"),
            "0755",
            "root:root",
            false,
        )];
        files.extend(extra);
        json!({
            "name": name,
            "version": "0.2.0-beta.2",
            "archive": format!("ritornello-plugin-{name}-0.2.0-beta.2-{{arch}}.tar.gz"),
            "files": files,
            "initial_config": initial,
            "enable": enable,
            "mount_root": mount_root,
            "block": format!(
                "[[plugin]]\nname = \"{name}\"\nexec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-{name}\"\n"
            ),
        })
    }

    /// A trimmed inventory in the exact shape `scripts/install-inventory.py`
    /// writes (format 2), parsed through `Inventory::parse` so its
    /// `deny_unknown_fields` holds the shape to the real one.
    pub(crate) fn inv() -> Inventory {
        let simple = |n: &str| plugin(n, vec![], json!([]), json!([]), Value::Null);
        let doc = json!({
            "format": 2,
            "product": "0.2.0-beta.2",
            "reference_order": ["radio", "cd", "files", "nrj-metas", "musicbrainz"],
            "core": {
                "name": "core",
                "version": "0.2.0-beta.2",
                "archive": "ritornello-core-0.2.0-beta.2-{arch}.tar.gz",
                "files": [
                    file("usr/local/bin/ritornello-core", "0755", "root:root", false),
                    file("etc/systemd/system/ritornello.service", "0644", "root:root", true),
                ],
                "initial_config": [],
                "enable": ["ritornello.service"],
                "mount_root": null,
                "block": null,
            },
            "plugins": [
                plugin(
                    "radio",
                    vec![],
                    json!([{ "archive_path": "initial-config/stations.example.toml", "target": "stations.toml" }]),
                    json!([]),
                    Value::Null,
                ),
                simple("cd"),
                simple("files"),
                simple("nrj-metas"),
                simple("musicbrainz"),
            ],
            // The files plugin's root helper, its unit and its rule ship
            // beside it, in an archive of their own.
            "companions": [{
                "name": "files-mount",
                "version": "0.2.0-beta.2",
                "archive": "ritornello-files-mount-0.2.0-beta.2-{arch}.tar.gz",
                "files": [
                    file("etc/systemd/system/ritornello-media-mount.service", "0644", "root:root", true),
                    file("etc/polkit-1/rules.d/51-ritornello-media.rules", "0644", "root:root", true),
                    file("usr/local/lib/ritornello/ritornello-media-mount", "0755", "root:root", true),
                ],
                "initial_config": [],
                "enable": ["ritornello-media-mount.service"],
                "mount_root": "/mnt/ritornello",
                "block": null,
                "with": "files",
            }],
            "packs": [
                { "language": "fr", "version": "0.2.0-beta.2", "archive": "ritornello-lang-fr-0.2.0-beta.2.tar.gz" },
            ],
        });
        Inventory::parse(&doc.to_string()).expect("the test inventory has the real shape")
    }

    /// The `[[plugin]]` blocks a device would carry for `declared`, under a
    /// one-line header, joined the way `deploy/plugins.example.toml` is.
    fn toml_for(declared: &[(&str, &str)]) -> String {
        let blocks: Vec<String> =
            declared.iter().map(|(n, e)| format!("[[plugin]]\nname = \"{n}\"\nexec = \"{e}\"\n")).collect();
        format!("# The plugins the core starts, in priority order.\n{}", blocks.join("\n"))
    }

    /// A surveyed device. Anything declared or recorded means the core is
    /// there and so is its account; nothing at all is a fresh device.
    pub(crate) fn dev(declared: &[(&str, &str)], registry: Option<Registry>, packs: &[&str], data: &[&str]) -> DeviceState {
        let installed = !declared.is_empty() || registry.is_some();
        DeviceState {
            kernel: "Linux".to_string(),
            machine: "aarch64".to_string(),
            systemd: true,
            uid: 1000,
            sudo: Sudo::NoPassword,
            core_present: installed,
            plugins_toml: (!declared.is_empty()).then(|| toml_for(declared)),
            declared: declared
                .iter()
                .map(|(n, e)| Declared { name: n.to_string(), exec: e.to_string() })
                .collect(),
            registry,
            registry_ignored: false,
            updater_placed: BTreeMap::new(),
            packs: packs.iter().map(|p| p.to_string()).collect(),
            data_nonempty: data.iter().map(|d| d.to_string()).collect(),
            user_exists: installed,
        }
    }

    pub(crate) fn registry(entries: &[(&str, &[&str])]) -> Registry {
        Registry {
            format: 1,
            components: entries
                .iter()
                .map(|(n, paths)| {
                    (
                        n.to_string(),
                        Recorded {
                            version: "0.2.0-beta.1".to_string(),
                            privileged: paths.iter().map(|p| p.to_string()).collect(),
                            identity: identities(paths),
                        },
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn install(plugins: &[&str], packs: &[&str], erase: &[&str]) -> Intent {
        Intent::InstallOrUpdate { plugins: set(plugins), packs: set(packs), erase_data: set(erase), reinstall: false }
    }

    fn declared_in(plan: &Plan) -> Vec<String> {
        ritornello_manifest::names_in_order(plan.plugins_toml.as_deref().expect("plugins.toml is written"))
            .expect("the written plugins.toml parses")
    }

    fn dests(plan: &Plan) -> Vec<&str> {
        plan.puts.iter().map(|p| p.dest.as_str()).collect()
    }

    fn has(v: &[String], s: &str) -> bool {
        v.iter().any(|x| x == s)
    }

    // --- 1 ---------------------------------------------------------------

    #[test]
    fn a_fresh_device_gets_the_core_and_what_was_chosen() {
        let plan = compute(&inv(), &dev(&[], None, &[], &[]), &install(&["radio"], &[], &[])).unwrap();
        let d = dests(&plan);
        assert!(d.contains(&"/usr/local/bin/ritornello-core"), "{d:?}");
        assert!(d.contains(&"/etc/systemd/system/ritornello.service"), "{d:?}");
        assert!(d.contains(&RADIO_EXEC), "{d:?}");
        assert_eq!(d.len(), 3, "nothing but the core and radio: {d:?}");
        let core_bin = plan.puts.iter().find(|p| p.dest == "/usr/local/bin/ritornello-core").unwrap();
        assert_eq!(core_bin.archive, "ritornello-core-0.2.0-beta.2-arm64.tar.gz");
        assert_eq!(core_bin.archive_path, "usr/local/bin/ritornello-core");
        assert_eq!((core_bin.mode.as_str(), core_bin.owner.as_str()), ("0755", "root:root"));
        assert_eq!(
            plan.archives,
            set(&["ritornello-core-0.2.0-beta.2-arm64.tar.gz", "ritornello-plugin-radio-0.2.0-beta.2-arm64.tar.gz"])
        );
        assert_eq!(
            plan.initial,
            vec![Initial {
                archive: "ritornello-plugin-radio-0.2.0-beta.2-arm64.tar.gz".to_string(),
                archive_path: "initial-config/stations.example.toml".to_string(),
                plugin: "radio".to_string(),
                target: "stations.toml".to_string(),
            }]
        );
        assert_eq!(declared_in(&plan), vec!["radio"]);
        assert_eq!(plan.enable_units, vec!["ritornello.service"]);
        assert!(plan.ensure_user);
        assert!(plan.start_service);
        assert!(!plan.stop_service);
        assert!(!plan.remove_plugins_toml && !plan.remove_registry && !plan.remove_user);
        assert!(plan.remove_files.is_empty() && plan.remove_trees.is_empty(), "{plan:?}");
        assert!(plan.disable_units.is_empty() && plan.unmount_roots.is_empty(), "{plan:?}");
        let reg = plan.registry.as_ref().expect("the registry is written");
        assert_eq!(reg.format, 1);
        assert_eq!(
            reg.components.get(CORE).map(|r| r.privileged.clone()),
            Some(vec!["/etc/systemd/system/ritornello.service".to_string()])
        );
        assert_eq!(reg.components.get(CORE).map(|r| r.version.as_str()), Some("0.2.0-beta.2"));
        assert_eq!(
            reg.components.get("radio"),
            Some(&Recorded { version: "0.2.0-beta.2".to_string(), privileged: vec![], identity: BTreeMap::new() }),
            "radio places nothing privileged, and is recorded all the same, with its version"
        );
        assert_eq!(plan.summary.installed, vec!["core", "radio"]);
        assert!(plan.summary.updated.is_empty() && plan.summary.removed.is_empty());
    }

    // --- 2 ---------------------------------------------------------------

    #[test]
    fn an_update_keeps_what_is_installed_and_replaces_its_files() {
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC)], None, &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "cd"], &[], &[])).unwrap();
        assert!(plan.remove_files.is_empty(), "{:?}", plan.remove_files);
        assert_eq!(plan.plugins_toml, device.plugins_toml, "unchanged, byte for byte");
        assert!(plan.stop_service);
        assert!(dests(&plan).contains(&CD_EXEC));
        assert_eq!(plan.summary.updated, vec!["core", "radio", "cd"]);
        assert!(plan.summary.installed.is_empty());
    }

    // --- 3 ---------------------------------------------------------------

    /// The binary is taken from the device's own `exec`, which is what the
    /// core actually runs — here a name the inventory never placed.
    #[test]
    fn unchecking_a_plugin_removes_its_binary_and_its_block_but_not_its_data() {
        let odd_exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-cd-old";
        let device = dev(&[("radio", RADIO_EXEC), ("cd", odd_exec)], None, &[], &["cd"]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&plan.remove_files, odd_exec), "{:?}", plan.remove_files);
        assert!(has(&plan.remove_files, CD_EXEC), "{:?}", plan.remove_files);
        assert_eq!(declared_in(&plan), vec!["radio"]);
        assert!(!has(&plan.remove_trees, "/var/lib/ritornello/plugins/cd"), "{:?}", plan.remove_trees);
        assert!(!dests(&plan).contains(&CD_EXEC));
        assert_eq!(plan.summary.removed, vec!["cd"]);
        assert!(plan.summary.erased.is_empty());
    }

    // --- 4 ---------------------------------------------------------------

    #[test]
    fn erasing_the_data_of_a_removed_plugin_removes_its_directory() {
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC)], None, &[], &["cd"]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &["cd"])).unwrap();
        assert!(has(&plan.remove_trees, "/var/lib/ritornello/plugins/cd"), "{:?}", plan.remove_trees);
        assert_eq!(plan.summary.erased, vec!["cd"]);
    }

    // --- 5 ---------------------------------------------------------------

    #[test]
    fn erasing_the_data_of_a_kept_plugin_is_refused() {
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC)], None, &[], &["radio"]);
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &[], &["radio"])),
            Err(PlanError::EraseNotRemoved("radio".to_string()))
        );
    }

    // --- 6 ---------------------------------------------------------------

    /// The files plugin's root files are recorded under its companion: the
    /// plugin itself places nothing privileged.
    fn files_registry() -> Registry {
        registry(&[
            (CORE, &["/etc/systemd/system/ritornello.service"]),
            (MOUNT, &[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER]),
        ])
    }

    #[test]
    fn removing_a_privileged_plugin_takes_its_unit_rule_helper_and_mounts() {
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(files_registry()), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&plan.disable_units, "ritornello-media-mount.service"), "{:?}", plan.disable_units);
        assert_eq!(plan.unmount_roots, vec!["/mnt/ritornello"]);
        for p in [MEDIA_HELPER, MEDIA_UNIT, MEDIA_RULE, FILES_EXEC] {
            assert!(has(&plan.remove_files, p), "{p}: {:?}", plan.remove_files);
        }
        let reg = plan.registry.as_ref().unwrap();
        assert!(!reg.components.contains_key("files") && !reg.components.contains_key(MOUNT), "{reg:?}");
        assert!(reg.components.contains_key(CORE));
        assert!(!plan.enable_units.iter().any(|u| u.contains("media")), "{:?}", plan.enable_units);
        assert!(!plan.archives.contains(MOUNT_ARCHIVE), "{:?}", plan.archives);
        assert_eq!(plan.summary.removed, vec!["files", MOUNT]);
    }

    // --- 7 ---------------------------------------------------------------

    #[test]
    fn a_file_the_old_version_placed_and_the_new_one_forgot_is_removed_too() {
        let old = "/etc/polkit-1/rules.d/50-ritornello-old.rules";
        let reg = registry(&[(CORE, &[]), (MOUNT, &[MEDIA_UNIT, old])]);
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(reg), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&plan.remove_files, old), "{:?}", plan.remove_files);
        assert!(has(&plan.remove_files, MEDIA_RULE), "the inventory's side of the union: {:?}", plan.remove_files);
    }

    // --- R27 -------------------------------------------------------------

    /// R27: a component that is kept, not removed, whose new version stops
    /// placing a privileged file the old one placed. Only the registry knows
    /// the file; the update must take it away, and a unit so removed is
    /// disabled first rather than left enabled with no file behind it.
    #[test]
    fn an_update_removes_a_privileged_file_the_new_version_no_longer_places() {
        let stale_unit = "/etc/systemd/system/ritornello-media-old.service";
        let stale_rule = "/etc/polkit-1/rules.d/50-ritornello-legacy.rules";
        let reg = registry(&[
            (CORE, &["/etc/systemd/system/ritornello.service", stale_rule]),
            (MOUNT, &[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER, stale_unit]),
        ]);
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(reg), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "files"], &[], &[])).unwrap();
        let mut removed = plan.remove_files.clone();
        removed.sort();
        assert_eq!(removed, vec![stale_rule.to_string(), stale_unit.to_string()], "nothing still placed, only the stale");
        assert_eq!(plan.disable_units, vec!["ritornello-media-old.service"]);
        let new = plan.registry.as_ref().unwrap();
        assert_eq!(
            new.components.get(MOUNT).map(|r| r.privileged.clone()),
            Some(vec![MEDIA_UNIT.to_string(), MEDIA_RULE.to_string(), MEDIA_HELPER.to_string()])
        );
        assert!(plan.unmount_roots.is_empty(), "files is kept: nothing unmounts");
        assert!(has(&plan.enable_units, "ritornello-media-mount.service"));
    }

    /// A registry entry for a component neither the release nor
    /// `plugins.toml` knows any more (a plugin the release stopped
    /// shipping): nothing keeps its files, so they go, and so does its entry.
    #[test]
    fn a_component_the_release_no_longer_ships_loses_its_recorded_files() {
        let gone = "/etc/systemd/system/ritornello-gone.service";
        let reg = registry(&[(CORE, &[]), ("gone", &[gone])]);
        let device = dev(&[("radio", RADIO_EXEC)], Some(reg), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&plan.remove_files, gone), "{:?}", plan.remove_files);
        assert!(has(&plan.disable_units, "ritornello-gone.service"), "{:?}", plan.disable_units);
        assert!(!plan.registry.as_ref().unwrap().components.contains_key("gone"));
    }

    /// The same component, still declared and chosen: it is now a
    /// third-party plugin as far as this release knows, so nothing of it is
    /// removed — and its record is carried over, or a later removal would no
    /// longer know which root files it left behind.
    #[test]
    fn a_recorded_component_still_chosen_keeps_its_files_and_its_record() {
        let gone = "/etc/systemd/system/ritornello-gone.service";
        let reg = registry(&[(CORE, &[]), ("gone", &[gone])]);
        let device = dev(
            &[("radio", RADIO_EXEC), ("gone", "/usr/local/lib/ritornello/plugins/ritornello-plugin-gone")],
            Some(reg.clone()),
            &[],
            &[],
        );
        let plan = compute(&inv(), &device, &install(&["radio", "gone"], &[], &[])).unwrap();
        assert!(plan.remove_files.is_empty(), "{:?}", plan.remove_files);
        assert!(plan.disable_units.is_empty(), "{:?}", plan.disable_units);
        assert_eq!(plan.registry.as_ref().unwrap().components.get("gone"), reg.components.get("gone"));
    }

    // --- fix round 1 -----------------------------------------------------

    /// C1 / R30: `files` no longer declared (its block removed by hand, or
    /// by an older UI), and remembered only by its companion's record. Its
    /// root unit is still enabled at boot and its rule still grants: it must
    /// be removed with the full treatment, binary by the inventory's `dest`.
    ///
    /// **[MUTATION]**: drop `|| companion_recorded(&p.name)` from
    /// `removed_ours` — this test fails (nothing of `files` goes).
    #[test]
    fn a_recorded_plugin_no_longer_declared_is_still_removed_with_its_units() {
        let device = dev(&[("radio", RADIO_EXEC)], Some(files_registry()), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&plan.disable_units, "ritornello-media-mount.service"), "{:?}", plan.disable_units);
        assert_eq!(plan.unmount_roots, vec!["/mnt/ritornello"]);
        for p in [MEDIA_HELPER, MEDIA_UNIT, MEDIA_RULE, FILES_EXEC] {
            assert!(has(&plan.remove_files, p), "{p}: {:?}", plan.remove_files);
        }
        let reg = plan.registry.as_ref().unwrap();
        assert!(!reg.components.contains_key("files") && !reg.components.contains_key(MOUNT), "{reg:?}");
        assert_eq!(plan.summary.removed, vec!["files", MOUNT]);
        assert_eq!(plan.plugins_toml, device.plugins_toml, "no block to remove: the file is left as it was");
    }

    /// R42: the registry written before the first placement knows every
    /// privileged path of the old record and of the new one, with the new
    /// versions; a total removal writes none.
    #[test]
    fn the_provisional_registry_is_the_union_of_the_old_record_and_the_new() {
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(files_registry()), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        let prov = plan.provisional_registry.as_ref().expect("an install writes a provisional registry");
        let fin = plan.registry.as_ref().unwrap();
        assert!(!fin.components.contains_key(MOUNT));
        let old = files_registry();
        assert_eq!(prov.components.get(MOUNT), old.components.get(MOUNT), "a removed companion stays recorded until the end");
        assert!(!fin.components.is_empty());
        for (name, rec) in &fin.components {
            let p = prov.components.get(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(p.version, rec.version);
            for path in &rec.privileged {
                assert!(p.privileged.contains(path), "{name}: {path}");
            }
        }
        let gone = compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }).unwrap();
        assert!(gone.provisional_registry.is_none());
    }

    /// C1 / R30, the other side: the same plugin, chosen, is installed like a
    /// new one — its block inserted, its files placed, its record rewritten.
    #[test]
    fn a_recorded_plugin_no_longer_declared_but_chosen_is_reinstalled() {
        let device = dev(&[("radio", RADIO_EXEC)], Some(files_registry()), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "files"], &[], &[])).unwrap();
        assert_eq!(declared_in(&plan), vec!["radio", "files"]);
        assert!(dests(&plan).contains(&MEDIA_UNIT));
        assert!(plan.remove_files.is_empty() && plan.disable_units.is_empty(), "{plan:?}");
        assert!(has(&plan.enable_units, "ritornello-media-mount.service"));
        let reg = plan.registry.as_ref().unwrap();
        assert_eq!(reg.components["files"].privileged, Vec::<String>::new(), "files places nothing privileged: {reg:?}");
        let rec = reg.components.get(MOUNT).cloned().unwrap();
        assert_eq!(rec.version, "0.2.0-beta.2");
        assert_eq!(rec.privileged, vec![MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER]);
        assert_eq!(plan.summary.installed, vec!["files"]);
        assert!(has(&plan.summary.updated, MOUNT), "its record says it was there: {:?}", plan.summary);
    }

    /// I1 / R31: a third party's `remote2` runs the very binary `cd` runs.
    /// Unchecking `cd` must not take `remote2`'s binary with it.
    #[test]
    fn a_binary_a_kept_plugin_still_runs_is_never_removed() {
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC), ("remote2", CD_EXEC)], None, &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "remote2"], &[], &[])).unwrap();
        assert!(!has(&plan.remove_files, CD_EXEC), "{:?}", plan.remove_files);
        assert_eq!(declared_in(&plan), vec!["radio", "remote2"]);
    }

    /// N1 / R35: `plugins.toml` is writable without root. A kept third-party
    /// entry whose `exec` names `files`' polkit rule must not shield the rule
    /// when `files` goes: only a binary in the plugins directory is shielded.
    #[test]
    fn a_kept_exec_outside_the_plugins_directory_shields_nothing() {
        let device = dev(
            &[("radio", RADIO_EXEC), ("files", FILES_EXEC), ("x", MEDIA_RULE)],
            Some(files_registry()),
            &[],
            &[],
        );
        let plan = compute(&inv(), &device, &install(&["radio", "x"], &[], &[])).unwrap();
        assert!(has(&plan.remove_files, MEDIA_RULE), "{:?}", plan.remove_files);
        assert!(!plan.registry.as_ref().unwrap().components.contains_key(MOUNT));
    }

    /// N1 / R35, the stale-unit variant: an R27 stale unit, whose only way
    /// into `disable_units` is being removed, named as a kept entry's `exec`.
    #[test]
    fn a_kept_exec_naming_a_stale_unit_does_not_keep_it_enabled() {
        let stale_unit = "/etc/systemd/system/ritornello-media-old.service";
        let reg = registry(&[(CORE, &[]), (MOUNT, &[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER, stale_unit])]);
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC), ("x", stale_unit)], Some(reg), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "files", "x"], &[], &[])).unwrap();
        assert!(has(&plan.remove_files, stale_unit), "{:?}", plan.remove_files);
        assert!(has(&plan.disable_units, "ritornello-media-old.service"), "{:?}", plan.disable_units);
    }

    /// N2 / R36: a `plugins.toml` present but empty is not "absent": it is
    /// left as it is, never replaced by the header.
    #[test]
    fn an_empty_plugins_toml_is_kept_not_replaced_by_the_header() {
        let mut device = dev(&[], None, &[], &[]);
        device.core_present = true;
        device.plugins_toml = Some(String::new());
        let plan = compute(&inv(), &device, &install(&[], &[], &[])).unwrap();
        assert_eq!(plan.plugins_toml.as_deref(), Some(""));
    }

    /// Nothing arrives, nothing leaves: `plugins.toml` is written back byte
    /// for byte as read — CRLF, comments, odd spacing and a last line with
    /// no newline included.
    #[test]
    fn plugins_toml_is_byte_identical_when_nothing_arrives_or_leaves() {
        let text = "# hand-edited\r\n[[plugin]]\r\nname   = \"radio\"  # the radio\r\nexec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"";
        let mut device = dev(&[("radio", RADIO_EXEC)], None, &[], &[]);
        device.plugins_toml = Some(text.to_string());
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert_eq!(plan.plugins_toml.as_deref(), Some(text));
    }

    /// R31, second half: a path this same plan places is never also removed
    /// — here the core's own unit, which a (mistaken) registry recorded for
    /// `files` too. Nor is that unit disabled.
    #[test]
    fn a_path_this_plan_places_is_never_also_removed() {
        let core_unit = "/etc/systemd/system/ritornello.service";
        let reg = registry(&[(CORE, &[core_unit]), ("files", &[MEDIA_UNIT, core_unit])]);
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(reg), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(!has(&plan.remove_files, core_unit), "{:?}", plan.remove_files);
        assert!(!has(&plan.disable_units, "ritornello.service"), "{:?}", plan.disable_units);
        assert!(has(&plan.remove_files, MEDIA_UNIT));
    }

    /// I2 / R32: a declared third-party name that is not one of ours to
    /// spell is kept without being judged; it is judged only where it would
    /// form a path.
    #[test]
    fn a_kept_third_party_name_need_not_be_a_valid_name_of_ours() {
        let exec = "/usr/local/lib/ritornello/plugins/acme-widget";
        let device = dev(&[("radio", RADIO_EXEC), ("Acme_Widget", exec)], None, &[], &[]);
        let (plugins, packs) = preselection(&inv(), &device);
        let plan = compute(&inv(), &device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false })
            .expect("the preselection of a device with Acme_Widget plans");
        assert_eq!(plan.summary.kept_third_party, vec!["Acme_Widget"]);
        // Removed, it goes by its exec; its data, by name, is still refused.
        let gone = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&gone.remove_files, exec));
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &[], &["Acme_Widget"])),
            Err(PlanError::InvalidName("Acme_Widget".to_string()))
        );
    }

    /// M1: a file name in the plugins directory that is `.`, `..` or carries
    /// a byte the shell would read twice is refused as outside it, by name.
    #[test]
    fn a_third_party_exec_with_an_unclean_file_name_is_outside_the_plugins_directory() {
        for exec in [
            "/usr/local/lib/ritornello/plugins/..",
            "/usr/local/lib/ritornello/plugins/.",
            "/usr/local/lib/ritornello/plugins/a b",
            "/usr/local/lib/ritornello/plugins/$(reboot)",
        ] {
            let device = dev(&[("theirs", exec)], None, &[], &[]);
            assert_eq!(
                compute(&inv(), &device, &install(&[], &[], &[])),
                Err(PlanError::ThirdPartyExecOutsidePluginsDir { name: "theirs".to_string(), exec: exec.to_string() }),
                "{exec:?}"
            );
        }
    }

    /// M2: data only when asked, enforced at the end whatever put the path
    /// in — here a registry entry recording a file under `cd`'s data.
    #[test]
    fn nothing_under_the_data_tree_goes_unless_its_data_was_named() {
        let inside = "/var/lib/ritornello/plugins/cd/cache.db";
        let reg = registry(&[(CORE, &[]), ("cd", &[inside])]);
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC)], Some(reg.clone()), &[], &["cd"]);
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &[], &[])),
            Err(PlanError::DataNotAsked(inside.to_string()))
        );
        assert_eq!(
            compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }),
            Err(PlanError::DataNotAsked(inside.to_string()))
        );
        // Asked, it goes.
        assert!(compute(&inv(), &device, &install(&["radio"], &[], &["cd"])).is_ok());
        assert!(compute(&inv(), &device, &Intent::RemoveAll { erase_data: true }).is_ok());
        // Another plugin's data being named does not cover `cd`'s.
        let other = registry(&[(CORE, &[]), ("cd", &["/var/lib/ritornello/plugins/radio/x"])]);
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC)], Some(other), &[], &[]);
        assert!(matches!(
            compute(&inv(), &device, &install(&["radio"], &[], &["cd"])),
            Err(PlanError::DataNotAsked(_))
        ));
    }

    /// M3: a name of ours declared twice ran two binaries; both go.
    #[test]
    fn a_name_of_ours_declared_twice_loses_both_binaries() {
        let second = "/usr/local/lib/ritornello/plugins/ritornello-plugin-cd-2";
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC), ("cd", second)], None, &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&plan.remove_files, CD_EXEC) && has(&plan.remove_files, second), "{:?}", plan.remove_files);
    }

    /// M7: a name declared twice loses every one of its blocks.
    #[test]
    fn a_name_declared_twice_loses_every_block() {
        let second = "/usr/local/lib/ritornello/plugins/ritornello-plugin-cd-2";
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC), ("cd", second)], None, &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert_eq!(declared_in(&plan), vec!["radio"]);
    }

    /// M5: the core starts without `plugins.toml`, but its update worker
    /// refuses to declare a plugin installed from the web UI when the file
    /// is absent. A fresh device always gets one.
    #[test]
    fn a_fresh_device_always_gets_a_plugins_toml() {
        let plan = compute(&inv(), &dev(&[], None, &[], &[]), &install(&[], &[], &[])).unwrap();
        let text = plan.plugins_toml.expect("a header-only plugins.toml is written");
        assert_eq!(text, PLUGINS_TOML_HEADER);
        let block = inv().plugin("cd").unwrap().block.clone().unwrap();
        let added = ritornello_manifest::append_block(&text, &block, "cd").expect("the core can declare into it");
        assert_eq!(ritornello_manifest::names_in_order(&added).unwrap(), vec!["cd"]);
        let with_radio = compute(&inv(), &dev(&[], None, &[], &[]), &install(&["radio"], &[], &[])).unwrap();
        assert!(with_radio.plugins_toml.unwrap().starts_with(PLUGINS_TOML_HEADER));
    }

    /// The review's warning: a unit name or a mount root from the inventory
    /// reaches `systemctl` / `umount` as root, so it is held to the shapes
    /// Ritornello ships before anything is planned.
    #[test]
    fn an_inventory_unit_or_mount_root_of_the_wrong_shape_is_refused() {
        let device = dev(&[], None, &[], &[]);
        let mut bad = inv();
        bad.core.enable = vec!["sshd.service".to_string()];
        assert!(matches!(compute(&bad, &device, &install(&[], &[], &[])), Err(PlanError::InvalidInventory(_))));
        // The companion's unit and mount root are held to the same shapes:
        // they are what actually reaches `systemctl` and `umount` today.
        let mut bad = inv();
        bad.companions[0].enable = vec!["sshd.service".to_string()];
        assert!(matches!(compute(&bad, &device, &install(&[], &[], &[])), Err(PlanError::InvalidInventory(_))));
        for root in ["/", "/mnt", "/mnt/ritornello/../..", "/mnt/ritornello/a/b", "/mnt/ritornellox", "/mnt/ritornello/"] {
            let mut bad = inv();
            bad.companions[0].mount_root = Some(root.to_string());
            assert!(
                matches!(compute(&bad, &device, &Intent::RemoveAll { erase_data: false }), Err(PlanError::InvalidInventory(_))),
                "{root:?}"
            );
            let mut bad = inv();
            bad.plugins.iter_mut().find(|p| p.name == "files").unwrap().mount_root = Some(root.to_string());
            assert!(
                matches!(compute(&bad, &device, &Intent::RemoveAll { erase_data: false }), Err(PlanError::InvalidInventory(_))),
                "a plugin's {root:?}"
            );
        }
        // R40: a sub-root is refused too, however well formed.
        let mut leaf = inv();
        leaf.companions[0].mount_root = Some("/mnt/ritornello/nas-1".to_string());
        assert!(matches!(
            compute(&leaf, &device, &Intent::RemoveAll { erase_data: false }),
            Err(PlanError::InvalidInventory(_))
        ));
        assert!(compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }).is_ok());
    }

    /// A companion's name keys its registry record and its `with` decides
    /// when it is placed: a `with` naming no plugin of the release, a name
    /// that is not bare, or a name another component already uses is not
    /// trusted.
    ///
    /// **[MUTATION]**, one per branch of `validate_inventory`'s companion
    /// checks, each reddening this test: drop the `valid_plugin_name` check;
    /// drop `c.name == inv.core.name`; drop `inv.plugin(&c.name).is_some()`;
    /// drop the duplicate count; drop the `inv.plugin(&c.with)` check.
    #[test]
    fn a_companion_that_follows_no_plugin_or_takes_another_s_name_is_refused() {
        let device = dev(&[], None, &[], &[]);
        let refused = |edit: &dyn Fn(&mut Inventory)| {
            let mut bad = inv();
            edit(&mut bad);
            compute(&bad, &device, &install(&["radio"], &[], &[]))
        };
        let invalid = |r: Result<Plan, PlanError>, what: &str| {
            assert!(matches!(r, Err(PlanError::InvalidInventory(_))), "{what}: {r:?}")
        };
        invalid(refused(&|i| i.companions[0].with = "nas".to_string()), "a with the release does not ship");
        invalid(refused(&|i| i.companions[0].with = "core".to_string()), "the core is not a plugin");
        invalid(refused(&|i| i.companions[0].name = "../files-mount".to_string()), "a name that is not bare");
        invalid(refused(&|i| i.companions[0].name = "core".to_string()), "the core's name");
        invalid(refused(&|i| i.companions[0].name = "cd".to_string()), "a plugin's name");
        invalid(
            refused(&|i| {
                let twin = i.companions[0].clone();
                i.companions.push(twin);
            }),
            "two companions of one name",
        );
        assert!(refused(&|_| {}).is_ok());
    }

    // --- The companion ---------------------------------------------------

    /// `files` chosen on a fresh device brings its companion, from the
    /// companion's own archive, with its unit enabled and its own record.
    /// Without `files`, nothing of the companion is planned.
    ///
    /// **[MUTATION]**: stop pushing a kept plugin's companions onto
    /// `placed` — this test fails.
    #[test]
    fn a_companion_is_placed_whenever_its_plugin_is() {
        let plan = compute(&inv(), &dev(&[], None, &[], &[]), &install(&["radio", "files"], &[], &[])).unwrap();
        for d in [MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER] {
            let p = plan.puts.iter().find(|p| p.dest == d).unwrap_or_else(|| panic!("{d} is not placed"));
            assert_eq!(p.archive, MOUNT_ARCHIVE, "{d}");
        }
        assert!(plan.archives.contains(MOUNT_ARCHIVE));
        assert!(has(&plan.enable_units, "ritornello-media-mount.service"));
        let reg = plan.registry.as_ref().unwrap();
        assert_eq!(reg.components[MOUNT].privileged, vec![MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER]);
        assert_eq!(reg.components["files"].privileged, Vec::<String>::new(), "recorded, with no root file");
        assert_eq!(plan.summary.installed, vec!["core", "radio", "files", MOUNT]);
        assert_eq!(declared_in(&plan), vec!["radio", "files"], "a companion is never declared");

        let without = compute(&inv(), &dev(&[], None, &[], &[]), &install(&["radio"], &[], &[])).unwrap();
        assert!(!without.archives.contains(MOUNT_ARCHIVE), "{:?}", without.archives);
        assert!(!dests(&without).contains(&MEDIA_UNIT));
        assert!(without.enable_units.iter().all(|u| u == "ritornello.service"), "{:?}", without.enable_units);
    }

    /// An update of `files` updates its companion, and the summary says so.
    #[test]
    fn an_update_of_a_plugin_updates_its_companion() {
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(files_registry()), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "files"], &[], &[])).unwrap();
        assert_eq!(plan.summary.updated, vec!["core", "radio", "files", MOUNT]);
        assert!(plan.summary.installed.is_empty());
        assert!(plan.remove_files.is_empty() && plan.disable_units.is_empty() && plan.unmount_roots.is_empty(), "{plan:?}");
    }

    /// The preselection is what `plugins.toml` declares: a companion, never
    /// declared, is never in it, even when the registry records it. Fed back,
    /// it keeps the companion through its plugin.
    #[test]
    fn the_preselection_never_lists_a_companion() {
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(files_registry()), &[], &[]);
        let (plugins, packs) = preselection(&inv(), &device);
        assert_eq!(plugins, set(&["radio", "files"]));
        let plan = compute(&inv(), &device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false }).unwrap();
        assert!(dests(&plan).contains(&MEDIA_HELPER));
    }

    /// R66: a `plugins.toml` entry under a companion's name is refused
    /// before anything is planned, whatever the choice. Kept as a third
    /// party while `files` goes, it would carry the companion's record over
    /// the removal of the very files that record names.
    ///
    /// **[MUTATION]**: drop the declared-under-a-companion-name check from
    /// `compute` — this test fails (the kept entry plans, and the record
    /// survives).
    #[test]
    fn a_plugin_declared_under_a_companion_s_name_is_refused() {
        let impostor = "/usr/local/lib/ritornello/plugins/files-mount";
        let device = dev(
            &[("radio", RADIO_EXEC), ("files", FILES_EXEC), (MOUNT, impostor)],
            Some(files_registry()),
            &[],
            &[],
        );
        let refused = Err(PlanError::DeclaredUnderCompanionName(MOUNT.to_string()));
        assert_eq!(compute(&inv(), &device, &install(&["radio", MOUNT], &[], &[])), refused);
        assert_eq!(compute(&inv(), &device, &install(&["radio", "files"], &[], &[])), refused);
        assert_eq!(compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }), refused);
        let (plugins, packs) = preselection(&inv(), &device);
        assert_eq!(
            compute(&inv(), &device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false }),
            refused
        );
    }

    /// A `plugins.toml` entry under a language pack's id is refused like
    /// one under a companion's name, whatever the choice: kept as a third
    /// party, it would take over the pack's registry record.
    ///
    /// **[MUTATION]**: drop the declared-under-a-pack-id check from
    /// `compute` — this test fails.
    #[test]
    fn a_plugin_declared_under_a_pack_id_is_refused() {
        let exec = "/usr/local/lib/ritornello/plugins/ritornello-lang-fr";
        let device = dev(&[("radio", RADIO_EXEC), ("ritornello-lang-fr", exec)], None, &["ritornello-lang-fr"], &[]);
        let refused = Err(PlanError::DeclaredUnderPackId("ritornello-lang-fr".to_string()));
        assert_eq!(compute(&inv(), &device, &install(&["radio", "ritornello-lang-fr"], &["fr"], &[])), refused);
        assert_eq!(compute(&inv(), &device, &install(&["radio"], &["fr"], &[])), refused);
        assert_eq!(compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }), refused);
        says(
            PlanError::DeclaredUnderPackId("ritornello-lang-fr".into()),
            &["\"ritornello-lang-fr\"", "language pack", "remove that [[plugin]] block"],
        );
        // A name that merely starts like one but is no pack id is a plugin.
        let device = dev(&[("radio", RADIO_EXEC), ("ritornello-lang-", THEIRS_EXEC)], None, &[], &[]);
        assert!(compute(&inv(), &device, &install(&["radio", "ritornello-lang-"], &[], &[])).is_ok());
    }

    #[test]
    fn a_plugin_under_a_companion_s_name_says_to_remove_its_block() {
        says(
            PlanError::DeclaredUnderCompanionName("files-mount".into()),
            &["\"files-mount\"", "remove that [[plugin]] block from /etc/ritornello/plugins.toml by hand"],
        );
    }

    /// A companion is not a plugin: it cannot be chosen by name.
    #[test]
    fn a_companion_cannot_be_chosen_on_its_own() {
        assert_eq!(
            compute(&inv(), &dev(&[], None, &[], &[]), &install(&[MOUNT], &[], &[])),
            Err(PlanError::UnknownPlugin(MOUNT.to_string()))
        );
    }

    /// The owner's Pi, `installed.toml` verbatim as the installer wrote it
    /// before the companion existed: the helper, its unit and its rule are
    /// recorded under `files`.
    const PI_REGISTRY: &str = r#"format = 1

[components.core]
version = "0.2.0-beta.2"
privileged = [
    "/etc/systemd/system/ritornello.service",
    "/etc/systemd/system/ritornello-update.service",
    "/etc/systemd/system/ritornello-rollback.service",
    "/etc/polkit-1/rules.d/50-ritornello-power.rules",
    "/etc/polkit-1/rules.d/52-ritornello-update.rules",
    "/usr/local/lib/ritornello/ritornello-update",
]

[components.files]
version = "0.2.0-beta.2"
privileged = [
    "/etc/systemd/system/ritornello-media-mount.service",
    "/etc/polkit-1/rules.d/51-ritornello-media.rules",
    "/usr/local/lib/ritornello/ritornello-media-mount",
]
"#;

    /// The Pi as surveyed: every plugin of the real release declared, the
    /// core there, and the registry above.
    fn the_pi(real: &Inventory) -> DeviceState {
        let declared: Vec<(String, String)> = real
            .plugins
            .iter()
            .map(|p| (p.name.clone(), format!("{PLUGINS_DIR}/ritornello-plugin-{}", p.name)))
            .collect();
        let declared: Vec<(&str, &str)> = declared.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let registry = Registry::parse(PI_REGISTRY).expect("the Pi's registry parses");
        dev(&declared, Some(registry), &["ritornello-lang-fr"], &[])
    }

    /// The migration the owner's next `deploy.sh` performs, with the real
    /// inventory: the three root files change owner in the registry, from
    /// `files` to `files-mount`, and nothing of them is removed, disabled or
    /// unmounted on the way — the new archive places them where they are.
    ///
    /// **[MUTATION]**: drop `!placed.contains(f.as_str())` from `finish`'s
    /// retain (R31 no longer applies) — this test fails: R27 on `files` puts
    /// the three paths in `remove_files` and the unit in `disable_units`.
    #[test]
    fn a_registry_from_before_the_companion_moves_its_files_without_removing_them() {
        let real = crate::inventory::tests::real_inventory();
        let device = the_pi(&real);
        let (plugins, packs) = preselection(&real, &device);
        assert!(plugins.contains("files"));
        let plan = compute(&real, &device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false })
            .expect("the Pi's registry plans");

        let moved = [MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER];
        for p in moved {
            assert!(!has(&plan.remove_files, p), "{p} removed: {:?}", plan.remove_files);
        }
        assert!(plan.disable_units.is_empty(), "nothing is disabled: {:?}", plan.disable_units);
        assert!(plan.unmount_roots.is_empty(), "{:?}", plan.unmount_roots);
        assert!(plan.remove_mount_roots.is_empty());
        assert!(plan.remove_trees.is_empty(), "no tree goes: {:?}", plan.remove_trees);
        let mount = real.companions.iter().find(|c| c.name == MOUNT).unwrap();
        for p in moved {
            let put = plan.puts.iter().find(|x| x.dest == p).unwrap_or_else(|| panic!("{p} not placed"));
            assert_eq!(put.archive, mount.as_component().archive_for("arm64"), "{p}");
        }
        assert!(has(&plan.enable_units, "ritornello-media-mount.service"));

        let reg = plan.registry.as_ref().unwrap();
        let rec = reg.components.get(MOUNT).expect("files-mount is recorded");
        let mut recorded = rec.privileged.clone();
        recorded.sort();
        let mut want: Vec<String> = moved.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(recorded, want);
        assert_eq!(rec.version, mount.version);
        assert_eq!(
            reg.components["files"].privileged,
            Vec::<String>::new(),
            "files records nothing privileged any more: {reg:?}"
        );
        // What the core recorded is placed again: nothing of it goes either.
        assert!(plan.remove_files.is_empty(), "no removal at all: {:?}", plan.remove_files);
        assert!(has(&plan.summary.updated, "files") && has(&plan.summary.updated, MOUNT), "{:?}", plan.summary);
        // Halfway through, the registry still knows every root file under
        // one record or the other.
        let prov = plan.provisional_registry.as_ref().unwrap();
        for p in moved {
            assert!(prov.components[MOUNT].privileged.iter().any(|x| x == p), "{p}");
        }
    }

    /// The same Pi, with `files` unchecked: the three files the old registry
    /// recorded under `files` go, the unit is disabled, the shares unmounted.
    ///
    /// **[MUTATION]**: stop pushing a removed plugin's companions onto
    /// `going` — this test fails (nothing unmounts). The three paths
    /// themselves are removed twice over, by the old `files` record and by
    /// the companion's inventory entry: the union rule.
    #[test]
    fn a_registry_from_before_the_companion_loses_its_files_when_files_goes() {
        let real = crate::inventory::tests::real_inventory();
        let device = the_pi(&real);
        let (mut plugins, packs) = preselection(&real, &device);
        plugins.remove("files");
        let plan = compute(&real, &device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false })
            .expect("the Pi without files plans");
        for p in [MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER, FILES_EXEC] {
            assert!(has(&plan.remove_files, p), "{p}: {:?}", plan.remove_files);
        }
        assert!(has(&plan.disable_units, "ritornello-media-mount.service"), "{:?}", plan.disable_units);
        assert_eq!(plan.unmount_roots, vec!["/mnt/ritornello"]);
        assert!(!dests(&plan).contains(&MEDIA_HELPER));
        let reg = plan.registry.as_ref().unwrap();
        assert!(!reg.components.contains_key("files") && !reg.components.contains_key(MOUNT), "{reg:?}");
        assert!(has(&plan.summary.removed, "files") && has(&plan.summary.removed, MOUNT), "{:?}", plan.summary);
    }

    // --- 8 ---------------------------------------------------------------

    #[test]
    fn a_registry_path_outside_ritornello_s_places_stops_the_plan() {
        let reg = registry(&[(CORE, &[]), ("files", &[MEDIA_UNIT, "/etc/passwd"])]);
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(reg), &[], &[]);
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &[], &[])),
            Err(PlanError::NotDeletable("/etc/passwd".to_string()))
        );
        // The same path recorded for a component that is kept must stop the
        // plan just as surely (R27 puts it in `remove_files` too).
        let reg = registry(&[(CORE, &["/etc/passwd"])]);
        let device = dev(&[("radio", RADIO_EXEC)], Some(reg), &[], &[]);
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &[], &[])),
            Err(PlanError::NotDeletable("/etc/passwd".to_string()))
        );
    }

    // --- 9 ---------------------------------------------------------------

    #[test]
    fn a_third_party_plugin_stays_when_checked_and_goes_when_unchecked() {
        let device = dev(&[("radio", RADIO_EXEC), ("theirs", THEIRS_EXEC)], None, &[], &[]);

        let kept = compute(&inv(), &device, &install(&["radio", "theirs"], &[], &[])).unwrap();
        assert!(!has(&kept.remove_files, THEIRS_EXEC), "{:?}", kept.remove_files);
        assert!(!has(&kept.summary.removed, "theirs"));
        assert_eq!(kept.summary.kept_third_party, vec!["theirs"]);
        assert_eq!(declared_in(&kept), vec!["radio", "theirs"]);
        assert!(!kept.puts.iter().any(|p| p.dest == THEIRS_EXEC), "we never place a third party's binary");

        let gone = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&gone.remove_files, THEIRS_EXEC), "{:?}", gone.remove_files);
        assert_eq!(declared_in(&gone), vec!["radio"]);
        assert_eq!(gone.summary.removed, vec!["theirs"]);
        assert!(gone.summary.kept_third_party.is_empty());
    }

    // --- 10 --------------------------------------------------------------

    #[test]
    fn a_third_party_binary_outside_the_plugins_directory_is_refused() {
        let device = dev(&[("radio", RADIO_EXEC), ("theirs", "/usr/bin/mpv")], None, &[], &[]);
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &[], &[])),
            Err(PlanError::ThirdPartyExecOutsidePluginsDir {
                name: "theirs".to_string(),
                exec: "/usr/bin/mpv".to_string()
            })
        );
        // A nested path under the plugins directory is not "in" it either.
        let device = dev(&[("theirs", "/usr/local/lib/ritornello/plugins/sub/theirs-bin")], None, &[], &[]);
        assert!(matches!(
            compute(&inv(), &device, &install(&[], &[], &[])),
            Err(PlanError::ThirdPartyExecOutsidePluginsDir { .. })
        ));
        // Kept, it is not ours to judge: nothing of it is removed.
        let device = dev(&[("theirs", "/usr/bin/mpv")], None, &[], &[]);
        assert!(compute(&inv(), &device, &install(&["theirs"], &[], &[])).is_ok());
    }

    // --- 11 --------------------------------------------------------------

    #[test]
    fn a_plugin_block_arrives_where_the_reference_order_puts_it() {
        let mb_exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-musicbrainz";
        let device = dev(&[("radio", RADIO_EXEC), ("musicbrainz", mb_exec)], None, &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "musicbrainz", "nrj-metas"], &[], &[])).unwrap();
        assert_eq!(declared_in(&plan), vec!["radio", "nrj-metas", "musicbrainz"]);
        assert!(plan.plugins_toml.as_deref().unwrap().starts_with("# The plugins the core starts"));
        assert_eq!(plan.summary.installed, vec!["nrj-metas"]);
    }

    // --- 12 --------------------------------------------------------------

    #[test]
    fn a_language_is_installed_and_another_removed() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &["ritornello-lang-de"], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &["fr"], &[])).unwrap();
        assert_eq!(
            plan.packs,
            vec![("ritornello-lang-fr-0.2.0-beta.2.tar.gz".to_string(), "ritornello-lang-fr".to_string())]
        );
        assert!(plan.archives.contains("ritornello-lang-fr-0.2.0-beta.2.tar.gz"));
        assert!(
            has(&plan.remove_trees, "/etc/ritornello/language-packs/ritornello-lang-de"),
            "{:?}",
            plan.remove_trees
        );
    }

    /// A pack present on the device that this release does not ship (a
    /// third party's, or one an older release carried): the preselection
    /// offers it, so choosing it must keep it rather than fail as unknown.
    #[test]
    fn a_language_pack_the_release_does_not_ship_stays_when_checked() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &["ritornello-lang-eo"], &[]);
        let kept = compute(&inv(), &device, &install(&["radio"], &["fr", "eo"], &[])).unwrap();
        assert!(kept.remove_trees.is_empty(), "{:?}", kept.remove_trees);
        assert_eq!(kept.packs.len(), 1, "only the release's own pack is placed: {:?}", kept.packs);
        let gone = compute(&inv(), &device, &install(&["radio"], &["fr"], &[])).unwrap();
        assert_eq!(gone.remove_trees, vec!["/etc/ritornello/language-packs/ritornello-lang-eo"]);
    }

    /// The tree half of rule 12: a directory under the packs root that is
    /// not shaped like a pack (the survey lists whatever `ls` finds) stops
    /// the plan by name, rather than being handed to root's `rm -rf`.
    #[test]
    fn a_stray_directory_among_the_language_packs_stops_the_plan() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &["notes"], &[]);
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &["fr"], &[])),
            Err(PlanError::NotDeletable("/etc/ritornello/language-packs/notes".to_string()))
        );
    }

    const XLANG: &str = "ritornello-xlang-fr-0123456789ab";

    /// A third-party pack (placed by the core from a source the operator
    /// added) is not this installer's: an ordinary run keeps it and succeeds.
    /// **[MUTATION]** drop the third-party exception: red (`NotDeletable`).
    #[test]
    fn a_third_party_pack_survives_an_ordinary_run() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &[XLANG, "ritornello-lang-de"], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &["fr"], &[])).unwrap();
        assert_eq!(plan.remove_trees, vec!["/etc/ritornello/language-packs/ritornello-lang-de"], "ours only");
        // Not offered as one of ours either. The exact set, not `.all(…)`,
        // which an empty preselection would also have passed.
        assert_eq!(preselection(&inv(), &device).1, set(&["de"]));
    }

    /// Nothing looser than the exact shape is spared: each name below breaks
    /// one part of it and still stops the plan. **[MUTATION]** drop any one
    /// check of `third_party_pack_id`: its line goes red.
    #[test]
    fn a_name_that_only_looks_like_a_third_party_pack_still_stops_the_plan() {
        for bad in [
            "ritornello-xlang-fr-0123456789a",   // eleven digits
            "ritornello-xlang-fr-0123456789AB",  // uppercase hex
            "ritornello-xlang-fr-0123456789ag",  // not hex
            "ritornello-xlang--0123456789ab",    // no language
            "ritornello-xlong-fr-0123456789ab",  // another prefix
        ] {
            let device = dev(&[("radio", RADIO_EXEC)], None, &[bad], &[]);
            assert_eq!(
                compute(&inv(), &device, &install(&["radio"], &["fr"], &[])),
                Err(PlanError::NotDeletable(format!("/etc/ritornello/language-packs/{bad}"))),
                "{bad}"
            );
        }
    }

    /// A total removal takes a third-party pack with the rest: it lives under
    /// `/etc/ritornello`, which goes whole.
    #[test]
    fn a_total_removal_takes_a_third_party_pack_with_the_rest() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &[XLANG], &[]);
        let plan = compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }).unwrap();
        assert!(has(&plan.remove_trees, "/etc/ritornello"), "{:?}", plan.remove_trees);
        assert!(names::PACKS_ROOT.starts_with("/etc/ritornello/"));
    }

    // --- 13 --------------------------------------------------------------

    fn full_device() -> DeviceState {
        dev(
            &[("radio", RADIO_EXEC), ("files", FILES_EXEC), ("theirs", THEIRS_EXEC)],
            Some(files_registry()),
            &["ritornello-lang-fr"],
            &["radio"],
        )
    }

    /// R55: only a total removal removes the mount root itself. Removing
    /// `files` alone unmounts under it and leaves it; an install never
    /// touches it.
    #[test]
    fn only_a_total_removal_removes_the_mount_root() {
        for erase in [false, true] {
            let plan = compute(&inv(), &full_device(), &Intent::RemoveAll { erase_data: erase }).unwrap();
            assert_eq!(plan.remove_mount_roots, vec!["/mnt/ritornello"], "erase_data={erase}");
        }
        let with_files = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(files_registry()), &[], &[]);
        let dropped = compute(&inv(), &with_files, &install(&["radio"], &[], &[])).unwrap();
        assert_eq!(dropped.unmount_roots, vec!["/mnt/ritornello"], "the removal of files does unmount");
        assert!(dropped.remove_mount_roots.is_empty(), "{:?}", dropped.remove_mount_roots);
        let kept = compute(&inv(), &with_files, &install(&["radio", "files"], &[], &[])).unwrap();
        assert!(kept.remove_mount_roots.is_empty(), "{:?}", kept.remove_mount_roots);
        let fresh = compute(&inv(), &dev(&[], None, &[], &[]), &install(&["radio", "files"], &[], &[])).unwrap();
        assert!(fresh.remove_mount_roots.is_empty(), "{:?}", fresh.remove_mount_roots);
    }

    #[test]
    fn total_removal_keeps_the_data_and_the_account_unless_told_otherwise() {
        let plan = compute(&inv(), &full_device(), &Intent::RemoveAll { erase_data: false }).unwrap();
        // No registry at all: the inventory alone still names the
        // companion's files, unit and mount root.
        let mut unrecorded = full_device();
        unrecorded.registry = None;
        let bare = compute(&inv(), &unrecorded, &Intent::RemoveAll { erase_data: false }).unwrap();
        assert!(has(&bare.remove_files, MEDIA_HELPER) && has(&bare.disable_units, "ritornello-media-mount.service"), "{bare:?}");
        assert!(has(&bare.summary.removed, MOUNT), "there with its declared plugin: {:?}", bare.summary);
        // The companion's record alone says it is there, too.
        let recorded_only = dev(&[("radio", RADIO_EXEC)], Some(files_registry()), &[], &[]);
        let gone = compute(&inv(), &recorded_only, &Intent::RemoveAll { erase_data: false }).unwrap();
        assert!(has(&gone.summary.removed, MOUNT), "there on its own record: {:?}", gone.summary);
        let neither = compute(&inv(), &dev(&[("radio", RADIO_EXEC)], None, &[], &[]), &Intent::RemoveAll { erase_data: false });
        assert!(!has(&neither.unwrap().summary.removed, MOUNT), "never there, never said to go");
        for t in ["/etc/ritornello", "/usr/local/lib/ritornello", "/var/lib/ritornello-update", "/var/lib/ritornello-install"] {
            assert!(has(&plan.remove_trees, t), "{t}: {:?}", plan.remove_trees);
        }
        assert!(!has(&plan.remove_trees, "/var/lib/ritornello"), "{:?}", plan.remove_trees);
        assert!(!plan.remove_user);
        assert!(has(&plan.unmount_roots, "/mnt/ritornello"));
        assert!(!plan.start_service && !plan.ensure_user);
        assert!(plan.stop_service);
        assert!(plan.archives.is_empty() && plan.puts.is_empty() && plan.packs.is_empty() && plan.initial.is_empty());
        for f in ["/usr/local/bin/ritornello-core", "/etc/systemd/system/ritornello.service", MEDIA_UNIT, MEDIA_RULE, THEIRS_EXEC] {
            assert!(has(&plan.remove_files, f), "{f}: {:?}", plan.remove_files);
        }
        for u in ["ritornello.service", "ritornello-media-mount.service"] {
            assert!(has(&plan.disable_units, u), "{u}: {:?}", plan.disable_units);
        }
        assert!(plan.enable_units.is_empty());
        assert!(plan.remove_registry && plan.registry.is_none());
        assert!(plan.remove_plugins_toml && plan.plugins_toml.is_none());
        assert!(plan.summary.erased.is_empty());
        assert!(has(&plan.summary.removed, "theirs") && has(&plan.summary.removed, "core"));
        assert!(has(&plan.summary.removed, MOUNT), "the companion goes with everything: {:?}", plan.summary);
        assert!(has(&plan.remove_files, MEDIA_HELPER), "{:?}", plan.remove_files);
    }

    // --- 14 --------------------------------------------------------------

    #[test]
    fn total_removal_with_the_data_removes_the_account() {
        let plan = compute(&inv(), &full_device(), &Intent::RemoveAll { erase_data: true }).unwrap();
        assert!(has(&plan.remove_trees, "/var/lib/ritornello"), "{:?}", plan.remove_trees);
        assert!(plan.remove_user);
        assert_eq!(plan.summary.erased, vec!["radio"]);
        // No account to remove: nothing says to remove one.
        let mut no_user = full_device();
        no_user.user_exists = false;
        assert!(!compute(&inv(), &no_user, &Intent::RemoveAll { erase_data: true }).unwrap().remove_user);
    }

    #[test]
    fn total_removal_refuses_a_third_party_binary_outside_the_plugins_directory() {
        let device = dev(&[("theirs", "/usr/bin/mpv")], None, &[], &[]);
        assert!(matches!(
            compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }),
            Err(PlanError::ThirdPartyExecOutsidePluginsDir { .. })
        ));
    }

    // --- 15 --------------------------------------------------------------

    #[test]
    fn an_unknown_plugin_or_language_is_named() {
        let device = dev(&[], None, &[], &[]);
        assert_eq!(
            compute(&inv(), &device, &install(&["radio", "nope"], &[], &[])),
            Err(PlanError::UnknownPlugin("nope".to_string()))
        );
        assert_eq!(
            compute(&inv(), &device, &install(&["radio"], &["xx"], &[])),
            Err(PlanError::UnknownPack("xx".to_string()))
        );
        assert_eq!(
            compute(&inv(), &device, &install(&["core"], &[], &[])),
            Err(PlanError::UnknownPlugin("core".to_string())),
            "the core is always installed, never chosen"
        );
    }

    // --- 16 --------------------------------------------------------------

    #[test]
    fn a_name_that_is_not_bare_is_refused_before_anything() {
        let device = dev(&[], None, &[], &[]);
        assert_eq!(
            compute(&inv(), &device, &install(&["../x"], &[], &[])),
            Err(PlanError::InvalidName("../x".to_string()))
        );
        assert_eq!(
            compute(&inv(), &device, &install(&[], &[], &["../x"])),
            Err(PlanError::InvalidName("../x".to_string()))
        );
        assert_eq!(
            compute(&inv(), &device, &install(&[], &["../x"], &[])),
            Err(PlanError::InvalidName("../x".to_string()))
        );
    }

    #[test]
    fn a_device_without_systemd_or_of_an_unknown_architecture_is_refused() {
        let mut device = dev(&[], None, &[], &[]);
        device.systemd = false;
        assert_eq!(compute(&inv(), &device, &install(&[], &[], &[])), Err(PlanError::NoSystemd));
        let mut device = dev(&[], None, &[], &[]);
        device.machine = "mips".to_string();
        assert_eq!(
            compute(&inv(), &device, &install(&[], &[], &[])),
            Err(PlanError::UnknownArch("mips".to_string()))
        );
    }

    // --- 17 --------------------------------------------------------------

    #[test]
    fn the_preselection_is_what_is_installed_and_nothing_on_a_fresh_device() {
        let device = dev(
            &[("radio", RADIO_EXEC), ("theirs", THEIRS_EXEC)],
            None,
            &["ritornello-lang-de", "ritornello-lang-fr"],
            &[],
        );
        assert_eq!(preselection(&inv(), &device), (set(&["radio", "theirs"]), set(&["de", "fr"])));
        assert_eq!(preselection(&inv(), &dev(&[], None, &[], &[])), (BTreeSet::new(), BTreeSet::new()));
        // The preselection, fed back unchanged, is a valid choice.
        let (plugins, packs) = preselection(&inv(), &device);
        let intent = Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false };
        assert!(compute(&inv(), &device, &intent).is_ok());
    }

    // --- 18 --------------------------------------------------------------

    /// Belt and braces: the computation itself only ever produces deletable
    /// paths, not merely the final filter.
    #[test]
    fn every_path_a_plan_removes_passes_the_deletable_checks() {
        let old = "/etc/polkit-1/rules.d/50-ritornello-old.rules";
        let plans = [
            compute(
                &inv(),
                &dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC)], None, &[], &["cd"]),
                &install(&["radio"], &[], &["cd"]),
            ),
            compute(
                &inv(),
                &dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(files_registry()), &[], &[]),
                &install(&["radio"], &[], &[]),
            ),
            compute(
                &inv(),
                &dev(
                    &[("radio", RADIO_EXEC), ("files", FILES_EXEC)],
                    Some(registry(&[(CORE, &[]), ("files", &[MEDIA_UNIT, old])])),
                    &[],
                    &[],
                ),
                &install(&["radio"], &[], &[]),
            ),
            compute(
                &inv(),
                &dev(&[("radio", RADIO_EXEC)], None, &["ritornello-lang-de"], &[]),
                &install(&["radio"], &["fr"], &[]),
            ),
            compute(&inv(), &full_device(), &Intent::RemoveAll { erase_data: false }),
            compute(&inv(), &full_device(), &Intent::RemoveAll { erase_data: true }),
        ];
        for plan in plans {
            let plan = plan.expect("each of these plans computes");
            assert!(
                !plan.remove_files.is_empty() || !plan.remove_trees.is_empty(),
                "a plan that removes nothing proves nothing"
            );
            for f in &plan.remove_files {
                assert!(names::deletable_file(f), "{f}");
            }
            for t in &plan.remove_trees {
                assert!(names::deletable_tree(t), "{t}");
            }
        }
    }

    /// The real inventory, not the trimmed one: installing everything on a
    /// fresh device, then removing everything with the data, stays inside
    /// Ritornello's own places end to end, and forgets nothing it placed.
    #[test]
    fn the_real_inventory_installs_and_removes_everything_cleanly() {
        let real = crate::inventory::tests::real_inventory();
        let all: BTreeSet<String> = real.plugins.iter().map(|p| p.name.clone()).collect();
        let langs: BTreeSet<String> = real.packs.iter().map(|p| p.language.clone()).collect();
        let plan = compute(
            &real,
            &dev(&[], None, &[], &[]),
            &Intent::InstallOrUpdate { plugins: all, packs: langs, erase_data: BTreeSet::new(), reinstall: false },
        )
        .expect("the real inventory plans on a fresh device");
        assert_eq!(declared_in(&plan), real.reference_order);
        assert!(dests(&plan).contains(&MEDIA_HELPER), "the files plugin brings its companion");
        assert!(plan.registry.as_ref().unwrap().components.contains_key(MOUNT));

        let declared: Vec<(String, String)> = real
            .plugins
            .iter()
            .map(|p| (p.name.clone(), format!("{PLUGINS_DIR}/ritornello-plugin-{}", p.name)))
            .collect();
        let declared: Vec<(&str, &str)> = declared.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let installed = dev(&declared, plan.registry.clone(), &["ritornello-lang-fr"], &["radio"]);
        let gone = compute(&real, &installed, &Intent::RemoveAll { erase_data: true })
            .expect("the real inventory removes cleanly");
        for put in &plan.puts {
            assert!(has(&gone.remove_files, &put.dest), "{}", put.dest);
        }
        let recorded: Vec<&String> =
            plan.registry.as_ref().unwrap().components.values().flat_map(|r| &r.privileged).collect();
        assert!(!recorded.is_empty());
        for p in recorded {
            assert!(has(&gone.remove_files, p), "{p}");
        }
    }

    // --- CAPTURE (temporary) ----------------------------------------------

    fn projection(plan: &Plan) -> String {
        format!(
            "{:#?}",
            (
                (&plan.archives, plan.ensure_user, plan.stop_service, &plan.disable_units, &plan.unmount_roots),
                (&plan.remove_mount_roots, &plan.remove_files, &plan.remove_trees, &plan.puts, &plan.initial),
                (&plan.packs, &plan.plugins_toml, plan.remove_plugins_toml, plan.remove_registry),
                (&plan.enable_units, plan.start_service, plan.remove_user),
                (
                    &plan.summary.installed,
                    &plan.summary.updated,
                    &plan.summary.removed,
                    &plan.summary.erased,
                    &plan.summary.kept_third_party
                ),
            )
        )
    }

    /// The device the two fixtures were captured on: `files_registry()`
    /// (the core and the companion, recorded at the offered version, the
    /// plugins not recorded at all), radio, files and a third party's
    /// declared, French installed.
    fn capture_device() -> DeviceState {
        let mut reg = files_registry();
        for r in reg.components.values_mut() {
            r.version = OFFERED.to_string();
        }
        dev(
            &[("radio", RADIO_EXEC), ("files", FILES_EXEC), ("theirs", THEIRS_EXEC)],
            Some(reg),
            &["ritornello-lang-fr"],
            &["radio"],
        )
    }

    fn reinstall(plugins: &[&str], packs: &[&str]) -> Intent {
        Intent::InstallOrUpdate { plugins: set(plugins), packs: set(packs), erase_data: BTreeSet::new(), reinstall: true }
    }

    /// The fixtures were written by `git checkout` with the platform's line
    /// ends; the projection has `\n` only.
    fn fixture(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    /// `--reinstall` and the repair screen reproduce, field for field, the
    /// plan this installer computed before it ever skipped anything:
    /// `testdata/reinstall-plan-*.txt` are that plan, captured from the code
    /// as it stood at `c45c19d6` (the registry aside — it now records every
    /// component — and the summary's new headings, which are empty here).
    /// The device is one an ordinary run would mostly leave alone.
    ///
    /// **[MUTATION]**: drop `!reinstall &&` from either `current` — the core
    /// and the companion (or the pack) are then left out, and this test fails.
    #[test]
    fn a_reinstall_plans_exactly_what_was_planned_before_skipping_existed() {
        let device = capture_device();
        let a = compute(&inv(), &device, &reinstall(&["radio", "files", "theirs"], &["fr"])).unwrap();
        assert_eq!(projection(&a), fixture(include_str!("testdata/reinstall-plan-a.txt")));
        let b = compute(&inv(), &device, &reinstall(&["radio", "files", "cd", "theirs"], &["fr"])).unwrap();
        assert_eq!(projection(&b), fixture(include_str!("testdata/reinstall-plan-b.txt")));
        for plan in [&a, &b] {
            assert!(plan.summary.up_to_date.is_empty() && plan.summary.cleared.is_empty(), "{:?}", plan.summary);
            assert!(!plan.nothing_to_do);
        }
        // And on a device that is entirely up to date: everything again.
        let full = compute(&inv(), &current_device(), &reinstall(&["radio", "files", "theirs"], &["fr"])).unwrap();
        assert_eq!(projection(&full), fixture(include_str!("testdata/reinstall-plan-a.txt")));
    }

    /// The same device, an ordinary run: what its registry records at the
    /// offered version is left alone, what it does not record is placed.
    #[test]
    fn an_ordinary_run_on_the_capture_device_places_only_the_unrecorded() {
        let device = capture_device();
        let plan = compute(&inv(), &device, &install(&["radio", "files", "theirs"], &["fr"], &[])).unwrap();
        assert_eq!(plan.summary.up_to_date, vec![CORE, MOUNT]);
        assert_eq!(plan.summary.updated, vec!["radio", "files"], "not recorded: placed again (fail-safe)");
        assert_eq!(plan.summary.languages_placed, vec!["ritornello-lang-fr"], "no record of the pack: placed");
        assert!(plan.stop_service && plan.start_service && plan.ensure_user);
    }

    // --- Leaving alone what is up to date --------------------------------

    /// The version every component and pack of `inv()` offers.
    const OFFERED: &str = "0.2.0-beta.2";

    /// What this installer records after placing `inv()`'s core, radio,
    /// files (and so its companion) and French.
    pub(crate) fn current_registry() -> Registry {
        let rec = |privileged: &[&str]| Recorded {
            version: OFFERED.to_string(),
            privileged: privileged.iter().map(|p| p.to_string()).collect(),
            identity: identities(privileged),
        };
        Registry {
            format: 1,
            components: [
                (CORE, rec(&["/etc/systemd/system/ritornello.service"])),
                ("radio", rec(&[])),
                ("files", rec(&[])),
                (MOUNT, rec(&[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER])),
                ("ritornello-lang-fr", rec(&[])),
            ]
            .into_iter()
            .map(|(n, r)| (n.to_string(), r))
            .collect(),
        }
    }

    /// A device on which that run has just finished.
    pub(crate) fn current_device() -> DeviceState {
        dev(
            &[("radio", RADIO_EXEC), ("files", FILES_EXEC), ("theirs", THEIRS_EXEC)],
            Some(current_registry()),
            &["ritornello-lang-fr"],
            &["radio"],
        )
    }

    /// The ordinary re-run, as `--keep` asks for it.
    pub(crate) fn keep(device: &DeviceState) -> Plan {
        let (plugins, packs) = preselection(&inv(), device);
        compute(&inv(), device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false })
            .expect("the re-run plans")
    }

    /// Whether `plan` places, extracts or removes nothing at all.
    fn touches_nothing(plan: &Plan) -> bool {
        plan.archives.is_empty()
            && plan.puts.is_empty()
            && plan.initial.is_empty()
            && plan.packs.is_empty()
            && plan.remove_files.is_empty()
            && plan.remove_trees.is_empty()
            && plan.disable_units.is_empty()
            && plan.unmount_roots.is_empty()
    }

    /// A registry written before identities existed records the core at the
    /// offered version, with the same files, and nothing about their
    /// content. The core reads that as unknown and refuses to update itself
    /// from the page, so the run must not leave it as is: it places the core
    /// again and records what it placed.
    ///
    /// **[MUTATION]**: drop `rec.identity != *identity` from `is_current` —
    /// this test fails, the core being called up to date.
    #[test]
    fn a_record_without_identities_places_the_core_again_and_records_them() {
        let mut device = current_device();
        for rec in device.registry.as_mut().unwrap().components.values_mut() {
            rec.identity.clear();
        }
        let plan = keep(&device);
        assert!(plan.summary.updated.contains(&CORE.to_string()), "{:?}", plan.summary);
        assert!(plan.summary.updated.contains(&MOUNT.to_string()), "{:?}", plan.summary);
        assert!(plan.puts.iter().any(|p| p.dest == "/etc/systemd/system/ritornello.service"));
        let core = &plan.registry.as_ref().expect("a run records").components[CORE];
        assert_eq!(core.identity, identities(&["/etc/systemd/system/ritornello.service"]));
    }

    /// One identity that differs is enough: the file on the device is not
    /// the one the release carries, whatever the version says.
    #[test]
    fn another_identity_recorded_places_the_core_again() {
        let mut device = current_device();
        let core = device.registry.as_mut().unwrap().components.get_mut(CORE).unwrap();
        core.identity.insert("/etc/systemd/system/ritornello.service".into(), "sha256:something-else".into());
        let plan = keep(&device);
        assert!(plan.summary.updated.contains(&CORE.to_string()), "{:?}", plan.summary);
        assert!(!plan.summary.updated.contains(&MOUNT.to_string()), "{:?}", plan.summary);
    }

    /// The provisional record is written before anything is placed and stays
    /// if the run stops: it must never vouch for a content not yet on the
    /// disk. An identity the run is about to change is dropped (unknown, so
    /// the core refuses until a run completes); one it keeps stays.
    ///
    /// **[MUTATION]**: drop the `retain` from `provisional` — this test fails,
    /// the old identity standing for a file about to be replaced.
    #[test]
    fn the_provisional_record_never_vouches_for_a_file_not_yet_placed() {
        let mut device = current_device();
        let core = device.registry.as_mut().unwrap().components.get_mut(CORE).unwrap();
        core.identity.insert("/etc/systemd/system/ritornello.service".into(), "sha256:the-old-unit".into());
        let plan = keep(&device);
        let prov = plan.provisional_registry.as_ref().expect("a run that places writes a provisional record");
        assert!(prov.components[CORE].identity.is_empty(), "{:?}", prov.components[CORE]);
        assert_eq!(
            prov.components[MOUNT].identity,
            identities(&[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER]),
            "an identity the run does not change stays"
        );
    }

    /// The owner's case: a re-run on a device already up to date downloads
    /// nothing, places nothing, does not stop the radio, and says so.
    ///
    /// **[MUTATION]**: make `current` always false (today's behaviour) —
    /// this test fails on the archives. Drop `plan.nothing_to_do = true`
    /// from `settle` — it fails on the flag.
    #[test]
    fn a_same_version_rerun_plans_nothing() {
        let device = current_device();
        let plan = keep(&device);
        assert!(touches_nothing(&plan), "{plan:?}");
        assert!(plan.nothing_to_do);
        assert!(!plan.stop_service && !plan.start_service && !plan.ensure_user, "{plan:?}");
        assert_eq!(plan.summary.up_to_date, vec![CORE, "radio", "files", MOUNT, "ritornello-lang-fr"]);
        assert!(plan.summary.installed.is_empty() && plan.summary.updated.is_empty(), "{:?}", plan.summary);
        assert!(plan.summary.removed.is_empty() && plan.summary.cleared.is_empty(), "{:?}", plan.summary);
        assert_eq!(plan.summary.kept_third_party, vec!["theirs"]);
        assert!(same_registry(plan.registry.as_ref().unwrap(), &current_registry()));
        assert_eq!(plan.plugins_toml, device.plugins_toml);
        // The units are still the ones the script would enable.
        assert_eq!(plan.enable_units, vec!["ritornello.service", "ritornello-media-mount.service"]);
    }

    /// The registry's order of privileged files is not a change: what the
    /// file says is the same set.
    #[test]
    fn privileged_files_recorded_in_another_order_are_the_same_record() {
        let mut reg = current_registry();
        reg.components.get_mut(MOUNT).unwrap().privileged.reverse();
        let mut device = current_device();
        device.registry = Some(reg);
        assert!(keep(&device).nothing_to_do);
    }

    /// One plugin moved: only it is downloaded and placed (its initial
    /// configuration offered again, written only where none exists), the
    /// service is stopped and restarted around it, the rest is left alone.
    ///
    /// **[MUTATION]**: drop the `stop_service`/`start_service` resets'
    /// `quiet` guard (always reset them) — this test fails.
    #[test]
    fn one_plugin_bumped_is_the_only_one_placed() {
        let mut bumped = inv();
        let radio = bumped.plugins.iter_mut().find(|p| p.name == "radio").unwrap();
        radio.version = "0.2.0-beta.3".to_string();
        radio.archive = "ritornello-plugin-radio-0.2.0-beta.3-{arch}.tar.gz".to_string();
        let device = current_device();
        let (plugins, packs) = preselection(&bumped, &device);
        let plan = compute(&bumped, &device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false })
            .unwrap();
        assert_eq!(plan.archives, set(&["ritornello-plugin-radio-0.2.0-beta.3-arm64.tar.gz"]));
        assert_eq!(dests(&plan), vec![RADIO_EXEC]);
        assert_eq!(plan.initial.len(), 1, "{:?}", plan.initial);
        assert!(plan.packs.is_empty());
        assert!(plan.stop_service && plan.start_service && plan.ensure_user);
        assert!(!plan.nothing_to_do);
        assert_eq!(plan.summary.updated, vec!["radio"]);
        assert_eq!(plan.summary.up_to_date, vec![CORE, "files", MOUNT, "ritornello-lang-fr"]);
        assert_eq!(plan.registry.as_ref().unwrap().components["radio"].version, "0.2.0-beta.3");
    }

    /// Fail-safe: whatever the registry cannot vouch for is placed again —
    /// no registry at all, one written before plugins were recorded, one
    /// that records another version or other privileged files, a component
    /// it does not name.
    ///
    /// **[MUTATION]**, one per condition of `is_current`, each reddening
    /// this test: answer `true` for an unrecorded name; drop the version
    /// comparison; drop the privileged-files comparison.
    #[test]
    fn what_the_registry_cannot_vouch_for_is_placed_again() {
        let placed_names = |device: &DeviceState| {
            let plan = keep(device);
            assert!(!plan.nothing_to_do);
            let mut names = plan.summary.updated.clone();
            names.extend(plan.summary.languages_placed.iter().cloned());
            names
        };
        let all = vec![CORE, "radio", "files", MOUNT, "ritornello-lang-fr"];

        let mut none = current_device();
        none.registry = None;
        assert_eq!(placed_names(&none), all, "no registry");

        let mut before = current_device();
        before.registry = Some(capture_device().registry.unwrap());
        assert_eq!(placed_names(&before), vec!["radio", "files", "ritornello-lang-fr"], "plugins and packs unrecorded");

        let mut older = current_device();
        older.registry.as_mut().unwrap().components.get_mut("files").unwrap().version = "0.2.0-beta.1".into();
        assert_eq!(placed_names(&older), vec!["files"], "another version");

        let mut fewer = current_device();
        fewer.registry.as_mut().unwrap().components.get_mut(MOUNT).unwrap().privileged.pop();
        assert_eq!(placed_names(&fewer), vec![MOUNT], "another set of privileged files");

        let mut unnamed = current_device();
        unnamed.registry.as_mut().unwrap().components.remove(CORE);
        assert_eq!(placed_names(&unnamed), vec![CORE], "a component it does not name");
    }

    /// The registry is a precondition, never the whole proof: a component
    /// recorded but not on the device — the core's binary gone, a plugin no
    /// longer declared but chosen, a pack's directory gone — is placed.
    #[test]
    fn a_recorded_component_that_is_not_there_is_placed() {
        let mut no_core = current_device();
        no_core.core_present = false;
        let plan = keep(&no_core);
        assert_eq!(plan.summary.installed, vec![CORE]);
        assert!(!plan.nothing_to_do);

        let mut no_pack = current_device();
        no_pack.packs.clear();
        let (plugins, _) = preselection(&inv(), &no_pack);
        let plan = compute(&inv(), &no_pack, &Intent::InstallOrUpdate { plugins, packs: set(&["fr"]), erase_data: BTreeSet::new(), reinstall: false })
            .unwrap();
        assert_eq!(plan.summary.languages_placed, vec!["ritornello-lang-fr"]);
        assert_eq!(plan.packs.len(), 1);
    }

    /// The trust rule, from the untrusted side: the in-app updater's memory
    /// (account-writable) naming another version reinstalls; naming the
    /// offered version while the registry disagrees still reinstalls —
    /// only the registry can say yes; and agreeing with a registry that
    /// agrees changes nothing.
    ///
    /// **[MUTATION]**: drop the `untrusted` check from `is_current` — the
    /// first assertion fails. Let a matching `untrusted` version vouch on
    /// its own (`|| untrusted.get(name) == Some(offered)` before the
    /// registry is consulted) — the second fails.
    #[test]
    fn the_updater_s_memory_can_only_ever_add_work() {
        let mut moved = current_device();
        moved.updater_placed.insert("radio".into(), "0.2.0-beta.3".into());
        let plan = keep(&moved);
        assert_eq!(plan.summary.updated, vec!["radio"], "the updater moved radio: placed again");
        assert_eq!(dests(&plan), vec![RADIO_EXEC]);
        assert!(plan.stop_service && plan.start_service);

        let mut forged = current_device();
        forged.registry.as_mut().unwrap().components.get_mut(CORE).unwrap().version = "0.2.0-beta.1".into();
        forged.updater_placed.insert(CORE.into(), OFFERED.into());
        assert_eq!(keep(&forged).summary.updated, vec![CORE], "an agreeing memory never makes a skip");
        let mut unrecorded = current_device();
        unrecorded.registry.as_mut().unwrap().components.remove("files");
        unrecorded.updater_placed.insert("files".into(), OFFERED.into());
        assert_eq!(keep(&unrecorded).summary.updated, vec!["files"], "nor stands in for a missing record");

        let mut agrees = current_device();
        agrees.updater_placed.insert("radio".into(), OFFERED.into());
        agrees.updater_placed.insert(CORE.into(), OFFERED.into());
        assert!(keep(&agrees).nothing_to_do, "a memory that agrees takes nothing away either");
    }

    /// The pack's own version is in a file its account owns, so a pack is
    /// skipped on the registry's word only; its record follows it.
    #[test]
    fn a_pack_at_the_recorded_version_is_left_alone_and_another_is_placed() {
        let mut stale = current_device();
        stale.registry.as_mut().unwrap().components.get_mut("ritornello-lang-fr").unwrap().version = "0.2.0-beta.1".into();
        let plan = keep(&stale);
        assert_eq!(plan.packs, vec![("ritornello-lang-fr-0.2.0-beta.2.tar.gz".to_string(), "ritornello-lang-fr".to_string())]);
        assert_eq!(plan.archives, set(&["ritornello-lang-fr-0.2.0-beta.2.tar.gz"]));
        assert!(plan.puts.is_empty(), "nothing else is placed: {:?}", plan.puts);
        assert_eq!(plan.registry.as_ref().unwrap().components["ritornello-lang-fr"].version, OFFERED);
    }

    /// A pack removed from the web interface: still recorded, no longer on
    /// the device, not chosen. Only its record goes — said in the summary —
    /// and nothing is placed or removed, so the service is left running.
    #[test]
    fn a_pack_gone_from_the_device_only_loses_its_record() {
        let mut gone = current_device();
        gone.packs.clear();
        let plan = keep(&gone);
        assert!(touches_nothing(&plan), "{plan:?}");
        assert!(!plan.nothing_to_do, "the registry still changes");
        assert!(!plan.registry.as_ref().unwrap().components.contains_key("ritornello-lang-fr"));
        assert_eq!(plan.summary.cleared, vec!["ritornello-lang-fr"]);
        assert!(!plan.stop_service && !plan.start_service, "a registry write alone needs no restart");
    }

    /// Decision 1's consequence: a plugin of ours placed by this installer
    /// is now recorded even with no root file, so one uninstalled from the
    /// web interface is "on the device" for R30. It goes like any removed
    /// plugin — its binary's leftover `rm -f`, its record — but the summary
    /// says what it is: already gone, record cleared, never "removed".
    ///
    /// **[MUTATION]**: report every removed plugin under `removed` — this
    /// test fails.
    #[test]
    fn a_plugin_uninstalled_from_the_web_ui_is_reported_as_cleared() {
        let mut device = current_device();
        device.registry.as_mut().unwrap().components.insert(
            "cd".to_string(),
            Recorded { version: OFFERED.to_string(), privileged: vec![], identity: BTreeMap::new() },
        );
        let plan = keep(&device);
        assert_eq!(plan.summary.cleared, vec!["cd"]);
        assert!(plan.summary.removed.is_empty(), "{:?}", plan.summary);
        assert!(has(&plan.remove_files, CD_EXEC), "{:?}", plan.remove_files);
        assert!(!plan.registry.as_ref().unwrap().components.contains_key("cd"));
        let lines = crate::ui::summary_lines(&plan).join("\n");
        assert!(lines.contains("no longer on the device, record cleared: cd"), "{lines}");
        // `files`, whose companion's root files are still recorded, is a
        // removal in earnest (R30), and said so.
        let mut files_gone = current_device();
        files_gone.declared.retain(|d| d.name != "files");
        let plan = keep(&files_gone);
        assert_eq!(plan.summary.removed, vec!["files", MOUNT]);
        assert!(plan.summary.cleared.is_empty());
    }

    /// What `settle` needs from the device before it calls a run quiet:
    /// the account, the core, and a `plugins.toml` that is written back as
    /// it was. Each missing one is work to do.
    ///
    /// **[MUTATION]**, one per condition, each reddening this test: drop
    /// `dev.user_exists`; drop `plan.plugins_toml == dev.plugins_toml`.
    #[test]
    fn a_missing_account_or_plugins_toml_is_not_up_to_date() {
        let mut no_user = current_device();
        no_user.user_exists = false;
        let plan = keep(&no_user);
        assert!(!plan.nothing_to_do && plan.ensure_user, "{plan:?}");

        // The core alone, up to date, and no `plugins.toml` at all: the
        // header the core's update worker needs is still to be written.
        let mut reg = current_registry();
        reg.components.retain(|n, _| n == CORE || n == "ritornello-lang-fr");
        let mut no_toml = dev(&[], Some(reg), &["ritornello-lang-fr"], &[]);
        assert!(no_toml.plugins_toml.is_none() && no_toml.core_present);
        let plan = compute(&inv(), &no_toml, &install(&[], &["fr"], &[])).unwrap();
        assert!(touches_nothing(&plan), "{plan:?}");
        assert!(!plan.nothing_to_do, "the header must be written: {plan:?}");
        // With the header there, the same device is up to date.
        no_toml.plugins_toml = Some(PLUGINS_TOML_HEADER.to_string());
        assert!(compute(&inv(), &no_toml, &install(&[], &["fr"], &[])).unwrap().nothing_to_do);
    }

    /// The registry the installer now writes — plugins with no root file,
    /// packs — round-trips through its own parser, and is read by the
    /// core's lenient one (`ritornello-core`'s `install_registry`, tested
    /// on this very shape there).
    #[test]
    fn the_registry_now_records_every_component_and_round_trips() {
        let plan = compute(&inv(), &dev(&[], None, &[], &[]), &install(&["radio", "files"], &["fr"], &[])).unwrap();
        let reg = plan.registry.unwrap();
        assert!(same_registry(&reg, &current_registry()), "{reg:?}");
        assert_eq!(Registry::parse(&reg.render()).unwrap(), reg);
        assert!(reg.render().contains("[components.radio]\nversion = \"0.2.0-beta.2\"\nprivileged = []"), "{}", reg.render());
    }

    // --- 19 --------------------------------------------------------------

    /// Each refusal reaches the operator as one sentence that names what
    /// it refuses and says what to do about it: a strict refusal blocks the
    /// whole plan, and a bare "not deletable" leaves the operator stuck.
    fn says(e: PlanError, parts: &[&str]) {
        let m = e.to_string();
        assert!(!m.contains('\n'), "one line: {m}");
        for p in parts {
            assert!(m.contains(p), "{e:?} must say {p:?}: {m}");
        }
    }

    #[test]
    fn an_unknown_plugin_or_pack_says_to_leave_it_out() {
        says(PlanError::UnknownPlugin("acme".into()), &["\"acme\"", "leave it out"]);
        says(PlanError::UnknownPack("xx".into()), &["\"xx\"", "leave it out"]);
    }

    #[test]
    fn an_invalid_name_says_to_correct_it() {
        says(PlanError::InvalidName("Bad/Name".into()), &["\"Bad/Name\"", "correct"]);
    }

    /// The stray directory among the packs is the one refusal an operator
    /// can actually cause by hand, so it names the directory to move.
    #[test]
    fn a_stray_pack_directory_says_to_move_it_out() {
        says(
            PlanError::NotDeletable(format!("{PACKS_ROOT}/notes")),
            &["\"/etc/ritornello/language-packs/notes\"", "move it out of /etc/ritornello/language-packs"],
        );
    }

    #[test]
    fn any_other_undeletable_path_says_where_it_came_from() {
        says(PlanError::NotDeletable("/usr/bin/mpv".into()), &["\"/usr/bin/mpv\"", "installed.toml"]);
    }

    #[test]
    fn a_third_party_exec_outside_the_plugins_dir_says_to_remove_its_entry_by_hand() {
        says(
            PlanError::ThirdPartyExecOutsidePluginsDir { name: "theirs".into(), exec: "/usr/bin/mpv".into() },
            &["\"theirs\"", "\"/usr/bin/mpv\"", "remove its [[plugin]] entry from /etc/ritornello/plugins.toml by hand"],
        );
    }

    #[test]
    fn erasing_the_data_of_a_kept_plugin_says_to_remove_it_or_keep_the_data() {
        says(PlanError::EraseNotRemoved("radio".into()), &["\"radio\"", "remove it too, or keep its data"]);
    }

    #[test]
    fn an_unknown_architecture_names_the_ones_shipped() {
        says(PlanError::UnknownArch("mips".into()), &["\"mips\"", "armv7l, aarch64 or x86_64"]);
    }

    #[test]
    fn no_systemd_says_what_is_needed() {
        says(PlanError::NoSystemd, &["systemd", "a Linux that runs systemd"]);
    }

    #[test]
    fn a_plugins_toml_that_cannot_be_rewritten_says_to_fix_it() {
        says(PlanError::PluginsToml("line 3: oops".into()), &["line 3: oops", "/etc/ritornello/plugins.toml", "fix"]);
    }

    #[test]
    fn an_untrusted_inventory_says_to_choose_another_release() {
        says(PlanError::InvalidInventory("files: unit \"x\"".into()), &["files: unit \"x\"", "choose another release"]);
    }

    #[test]
    fn data_not_asked_names_the_path_and_the_registry() {
        says(
            PlanError::DataNotAsked("/var/lib/ritornello/plugins/cd/x".into()),
            &["\"/var/lib/ritornello/plugins/cd/x\"", "installed.toml"],
        );
    }
}
