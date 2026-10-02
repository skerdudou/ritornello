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
use crate::inventory::{Component, Inventory};
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
    InstallOrUpdate {
        plugins: BTreeSet<String>,
        packs: BTreeSet<String>,
        erase_data: BTreeSet<String>,
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
    match intent {
        Intent::RemoveAll { erase_data } => remove_all(inv, dev, *erase_data),
        Intent::InstallOrUpdate { plugins, packs, erase_data } => {
            install_or_update(inv, dev, arch, plugins, packs, erase_data)
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
    for c in std::iter::once(&inv.core).chain(inv.plugins.iter()) {
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

fn install_or_update(
    inv: &Inventory,
    dev: &DeviceState,
    arch: &str,
    plugins: &BTreeSet<String>,
    packs: &BTreeSet<String>,
    erase_data: &BTreeSet<String>,
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
    let kept_ours: Vec<&Component> = inv.plugins.iter().filter(|p| plugins.contains(&p.name)).collect();
    let removed_ours: Vec<&Component> = inv
        .plugins
        .iter()
        .filter(|p| declared.contains(p.name.as_str()) || recorded.contains_key(&p.name))
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

    let mut plan = Plan::default();
    let mut remove_files = BTreeSet::new();
    let mut new_registry = BTreeMap::new();

    // 4. The core and every kept plugin of ours: placed in full.
    for c in std::iter::once(&inv.core).chain(kept_ours.iter().copied()) {
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
        for u in &c.enable {
            push_unique(&mut plan.enable_units, u);
        }
        // 10. The registry records what this version places privileged.
        let privileged: Vec<String> = c.files.iter().filter(|f| f.privileged).map(|f| f.dest.clone()).collect();
        // R27: what the previous version placed and this one no longer does.
        if let Some(old) = recorded.get(&c.name) {
            for p in &old.privileged {
                if !c.files.iter().any(|f| &f.dest == p) {
                    remove_files.insert(p.clone());
                }
            }
        }
        if !privileged.is_empty() {
            new_registry.insert(c.name.clone(), Recorded { version: c.version.clone(), privileged });
        }
        let was_there = if c.name == inv.core.name { dev.core_present } else { declared.contains(c.name.as_str()) };
        if was_there {
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
    // when the name is declared twice.
    for c in &removed_ours {
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
        plan.summary.removed.push(c.name.clone());
    }

    // 6. Every third-party plugin that goes: its binary, from the plugins
    // directory only.
    for (name, exec) in &removed_third {
        remove_files.insert(third_party_binary(name, exec)?);
        plan.summary.removed.push(name.to_string());
    }
    plan.summary.kept_third_party = kept_third.iter().map(|n| n.to_string()).collect();

    // A recorded component nothing keeps any more — gone from the release
    // and not a third-party plugin still chosen — loses its files. One that
    // is still chosen as a third-party plugin keeps its entry as it was.
    for (name, old) in &recorded {
        if new_registry.contains_key(name) || name == &inv.core.name || ours.contains(name.as_str()) {
            continue;
        }
        if kept_third.contains(&name.as_str()) {
            new_registry.insert(name.clone(), old.clone());
        } else {
            remove_files.extend(old.privileged.iter().cloned());
        }
    }

    // 7. Data, only of what goes, only when asked.
    for n in erase_data {
        if dev.data_nonempty.contains(n) {
            plan.remove_trees.push(format!("{DATA_ROOT}/{n}"));
            plan.summary.erased.push(n.clone());
        }
    }

    // 8. Language packs: exactly the chosen ones.
    let wanted_ids: BTreeSet<String> = packs.iter().map(|l| names::pack_id(l)).collect();
    for l in packs {
        if let Some(p) = shipped(l) {
            plan.archives.insert(p.archive.clone());
            plan.packs.push((p.archive.clone(), names::pack_id(l)));
        }
    }
    for id in &dev.packs {
        if !wanted_ids.contains(id) {
            plan.remove_trees.push(format!("{PACKS_ROOT}/{id}"));
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
    finish(plan, remove_files, &kept_execs, DataScope::Only(erase_data))
}

/// The union of what the device recorded and what this plan records: per
/// component, the new version when there is one, and every privileged path
/// either side names.
fn provisional(old: &BTreeMap<String, Recorded>, new: &BTreeMap<String, Recorded>) -> Registry {
    let mut components = old.clone();
    for (name, rec) in new {
        let entry = components
            .entry(name.clone())
            .or_insert_with(|| Recorded { version: rec.version.clone(), privileged: Vec::new() });
        entry.version = rec.version.clone();
        for p in &rec.privileged {
            if !entry.privileged.contains(p) {
                entry.privileged.push(p.clone());
            }
        }
    }
    Registry { format: 1, components }
}

fn remove_all(inv: &Inventory, dev: &DeviceState, erase_data: bool) -> Result<Plan, PlanError> {
    let mut plan = Plan { stop_service: dev.core_present, ..Plan::default() };
    let mut remove_files = BTreeSet::new();
    let ours: BTreeSet<&str> = inv.plugins.iter().map(|p| p.name.as_str()).collect();

    for c in std::iter::once(&inv.core).chain(inv.plugins.iter()) {
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

    /// One `files` entry in `install-inventory.py`'s own shape.
    fn file(path: &str, mode: &str, owner: &str, privileged: bool) -> Value {
        json!({
            "archive_path": path,
            "dest": format!("/{path}"),
            "mode": mode,
            "owner": owner,
            "privileged": privileged,
        })
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
    /// writes (format 1), parsed through `Inventory::parse` so its
    /// `deny_unknown_fields` holds the shape to the real one.
    pub(crate) fn inv() -> Inventory {
        let simple = |n: &str| plugin(n, vec![], json!([]), json!([]), Value::Null);
        let doc = json!({
            "format": 1,
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
                plugin(
                    "files",
                    vec![
                        file("etc/systemd/system/ritornello-media-mount.service", "0644", "root:root", true),
                        file("etc/polkit-1/rules.d/51-ritornello-media.rules", "0644", "root:root", true),
                        file("usr/local/lib/ritornello/ritornello-media-mount", "0755", "root:root", true),
                    ],
                    json!([]),
                    json!(["ritornello-media-mount.service"]),
                    json!("/mnt/ritornello"),
                ),
                simple("nrj-metas"),
                simple("musicbrainz"),
            ],
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
        Intent::InstallOrUpdate { plugins: set(plugins), packs: set(packs), erase_data: set(erase) }
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
        assert!(!reg.components.contains_key("radio"), "radio places nothing privileged");
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

    fn files_registry() -> Registry {
        registry(&[
            (CORE, &["/etc/systemd/system/ritornello.service"]),
            ("files", &[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER]),
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
        assert!(!reg.components.contains_key("files"), "{reg:?}");
        assert!(reg.components.contains_key(CORE));
        assert!(!plan.enable_units.iter().any(|u| u.contains("media")), "{:?}", plan.enable_units);
    }

    // --- 7 ---------------------------------------------------------------

    #[test]
    fn a_file_the_old_version_placed_and_the_new_one_forgot_is_removed_too() {
        let old = "/etc/polkit-1/rules.d/50-ritornello-old.rules";
        let reg = registry(&[(CORE, &[]), ("files", &[MEDIA_UNIT, old])]);
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
            ("files", &[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER, stale_unit]),
        ]);
        let device = dev(&[("radio", RADIO_EXEC), ("files", FILES_EXEC)], Some(reg), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio", "files"], &[], &[])).unwrap();
        let mut removed = plan.remove_files.clone();
        removed.sort();
        assert_eq!(removed, vec![stale_rule.to_string(), stale_unit.to_string()], "nothing still placed, only the stale");
        assert_eq!(plan.disable_units, vec!["ritornello-media-old.service"]);
        let new = plan.registry.as_ref().unwrap();
        assert_eq!(
            new.components.get("files").map(|r| r.privileged.clone()),
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

    /// C1 / R30: `files` recorded by the registry but no longer declared
    /// (its block removed by hand, or by an older UI). Its root unit is
    /// still enabled at boot and its rule still grants: it must be removed
    /// with the full treatment, binary by the inventory's `dest`.
    #[test]
    fn a_recorded_plugin_no_longer_declared_is_still_removed_with_its_units() {
        let device = dev(&[("radio", RADIO_EXEC)], Some(files_registry()), &[], &[]);
        let plan = compute(&inv(), &device, &install(&["radio"], &[], &[])).unwrap();
        assert!(has(&plan.disable_units, "ritornello-media-mount.service"), "{:?}", plan.disable_units);
        assert_eq!(plan.unmount_roots, vec!["/mnt/ritornello"]);
        for p in [MEDIA_HELPER, MEDIA_UNIT, MEDIA_RULE, FILES_EXEC] {
            assert!(has(&plan.remove_files, p), "{p}: {:?}", plan.remove_files);
        }
        assert!(!plan.registry.as_ref().unwrap().components.contains_key("files"));
        assert_eq!(plan.summary.removed, vec!["files"]);
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
        assert!(!fin.components.contains_key("files"));
        let old = files_registry();
        assert_eq!(prov.components.get("files"), old.components.get("files"), "a removed plugin stays recorded until the end");
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
        let rec = plan.registry.as_ref().unwrap().components.get("files").cloned().unwrap();
        assert_eq!(rec.version, "0.2.0-beta.2");
        assert_eq!(rec.privileged, vec![MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER]);
        assert_eq!(plan.summary.installed, vec!["files"]);
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
        assert!(!plan.registry.as_ref().unwrap().components.contains_key("files"));
    }

    /// N1 / R35, the stale-unit variant: an R27 stale unit, whose only way
    /// into `disable_units` is being removed, named as a kept entry's `exec`.
    #[test]
    fn a_kept_exec_naming_a_stale_unit_does_not_keep_it_enabled() {
        let stale_unit = "/etc/systemd/system/ritornello-media-old.service";
        let reg = registry(&[(CORE, &[]), ("files", &[MEDIA_UNIT, MEDIA_RULE, MEDIA_HELPER, stale_unit])]);
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
        let plan = compute(&inv(), &device, &Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new() })
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
        for root in ["/", "/mnt", "/mnt/ritornello/../..", "/mnt/ritornello/a/b", "/mnt/ritornellox", "/mnt/ritornello/"] {
            let mut bad = inv();
            bad.plugins.iter_mut().find(|p| p.name == "files").unwrap().mount_root = Some(root.to_string());
            assert!(
                matches!(compute(&bad, &device, &Intent::RemoveAll { erase_data: false }), Err(PlanError::InvalidInventory(_))),
                "{root:?}"
            );
        }
        // R40: a sub-root is refused too, however well formed.
        let mut leaf = inv();
        leaf.plugins.iter_mut().find(|p| p.name == "files").unwrap().mount_root = Some("/mnt/ritornello/nas-1".to_string());
        assert!(matches!(
            compute(&leaf, &device, &Intent::RemoveAll { erase_data: false }),
            Err(PlanError::InvalidInventory(_))
        ));
        assert!(compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }).is_ok());
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
        let intent = Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new() };
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
            &Intent::InstallOrUpdate { plugins: all, packs: langs, erase_data: BTreeSet::new() },
        )
        .expect("the real inventory plans on a fresh device");
        assert_eq!(declared_in(&plan), real.reference_order);

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
