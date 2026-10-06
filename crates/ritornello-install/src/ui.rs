//! The screens of spec §7, one function each. Every screen takes what it
//! offers and what it preselects, and returns the answer; none of them
//! decides anything. What they present is computed, and tested, elsewhere:
//! the preselection and the plan in `plan`, the options below.
//!
//! `dialoguer` draws on the Windows console as well as on a unix terminal.
//! Every screen runs before `ssh::apply` starts: nothing is asked while the
//! device's own output is being relayed.

use std::collections::BTreeSet;

use anyhow::Context;
use dialoguer::{Confirm, Input, MultiSelect, Password, Select};

use crate::device::DeviceState;
use crate::inventory::Inventory;
use crate::plan::Plan;
use crate::source::Release;

/// What the operator does with a device that already has Ritornello.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    InstallOrUpdate,
    RemoveAll,
}

/// One line of a checklist: the name the answer carries, and its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub label: String,
}

/// The plugins offered: ours, in the release's reference order, then every
/// third-party plugin the device declares, marked as such. The core is not
/// among them: it always comes with the rest.
pub fn plugin_choices(inv: &Inventory, dev: &DeviceState) -> Vec<Choice> {
    let ours = inv.reference_order.iter().filter_map(|n| inv.plugin(n)).map(|c| Choice {
        name: c.name.clone(),
        label: format!("{} ({})", c.name, c.version),
    });
    let mut theirs: Vec<Choice> = Vec::new();
    for d in dev.declared.iter().filter(|d| inv.plugin(&d.name).is_none()) {
        if !theirs.iter().any(|c| c.name == d.name) {
            theirs.push(Choice {
                name: d.name.clone(),
                label: format!("{} (third-party, managed from the web UI)", d.name),
            });
        }
    }
    ours.chain(theirs).collect()
}

/// The languages offered: those the release ships, then those installed
/// that it does not (kept when left checked, as a third-party plugin is).
pub fn pack_choices(inv: &Inventory, installed: &BTreeSet<String>) -> Vec<Choice> {
    let shipped = inv.packs.iter().map(|p| Choice {
        name: p.language.clone(),
        label: format!("{} ({})", p.language, p.version),
    });
    let others = installed.iter().filter(|l| !inv.packs.iter().any(|p| &p.language == *l)).map(|l| Choice {
        name: l.clone(),
        label: format!("{l} (installed, not in this release)"),
    });
    shipped.chain(others).collect()
}

/// The plugins whose data the data screen asks about: those that go, and
/// whose data directory is not empty.
pub fn data_choices(removed: &BTreeSet<String>, dev: &DeviceState) -> Vec<String> {
    removed.iter().filter(|n| dev.data_nonempty.contains(*n)).cloned().collect()
}

/// The data tree a total removal with its data takes whole.
const DATA_TREE: &str = "/var/lib/ritornello";

/// The summary as the lines shown before confirming, and again at the end.
///
/// Read from the plan itself, not only from its `Summary`: `summary.erased`
/// names the plugin data directories the survey saw non-empty, but a total
/// removal with its data takes the whole of `/var/lib/ritornello` — the
/// core's settings included — and the `ritornello` account, whatever the
/// survey saw. The confirmation is the last guard before that, so it says
/// what the plan does.
pub fn summary_lines(plan: &Plan) -> Vec<String> {
    let summary = &plan.summary;
    let mut lines = Vec::new();
    let mut section = |title: &str, names: &[String]| {
        if !names.is_empty() {
            lines.push(format!("  {title}: {}", names.join(", ")));
        }
    };
    section("installed", &summary.installed);
    section("updated", &summary.updated);
    section("removed", &summary.removed);
    section("data erased", &summary.erased);
    section("left as they are (third-party)", &summary.kept_third_party);
    if plan.remove_trees.iter().any(|t| t == DATA_TREE) {
        lines.push(format!("  ALL DATA ERASED: the whole of {DATA_TREE}, every setting and every plugin's data"));
    }
    if plan.remove_user {
        lines.push("  the ritornello account is removed".to_string());
    }
    if lines.is_empty() {
        lines.push("  nothing to change".to_string());
    }
    lines
}

/// Step 1: the address, then the ssh account. An address typed as
/// `account@host` skips the second question.
pub fn ask_host() -> anyhow::Result<String> {
    let address: String = Input::new()
        .with_prompt("Device address")
        .validate_with(|s: &String| if crate::ssh::valid_host(s) { Ok(()) } else { Err("not a host name") })
        .interact_text()
        .context("asking for the device address")?;
    if address.contains('@') {
        return Ok(address);
    }
    let account: String = Input::new()
        .with_prompt("ssh account")
        .default("dietpi".to_string())
        .validate_with(|s: &String| if crate::ssh::valid_host(s) { Ok(()) } else { Err("not an account name") })
        .interact_text()
        .context("asking for the ssh account")?;
    Ok(format!("{account}@{address}"))
}

/// Whether step 3 is asked (R52): whenever anything of ours is on the
/// device — the core, a declared plugin, a registry, a pack, the account —
/// even what `is_fresh` calls fresh, so that a device holding only
/// leftovers can still be cleaned from the screens.
pub fn offers_remove_all(dev: &DeviceState) -> bool {
    dev.core_present || !dev.declared.is_empty() || dev.registry.is_some() || !dev.packs.is_empty() || dev.user_exists
}

/// Step 3, on a device that has something of Ritornello's.
pub fn ask_action() -> anyhow::Result<Action> {
    let i = Select::new()
        .with_prompt("Ritornello is, or was, on this device")
        .items(["Install or update", "Remove everything"])
        .default(0)
        .interact()
        .context("asking what to do")?;
    Ok(if i == 0 { Action::InstallOrUpdate } else { Action::RemoveAll })
}

/// Step 4: the recent releases, newest first, `default` preselected.
pub fn ask_version(releases: &[Release], default: &str) -> anyhow::Result<String> {
    let mut recent: Vec<&Release> = releases.iter().collect();
    recent.sort_by(|a, b| b.published_at.cmp(&a.published_at).then_with(|| b.tag.cmp(&a.tag)));
    recent.truncate(15);
    if !recent.iter().any(|r| r.tag == default)
        && let Some(r) = releases.iter().find(|r| r.tag == default)
    {
        recent.insert(0, r);
    }
    let labels: Vec<String> =
        recent.iter().map(|r| if r.prerelease { format!("{} (prerelease)", r.tag) } else { r.tag.clone() }).collect();
    let at = recent.iter().position(|r| r.tag == default).unwrap_or(0);
    let i = Select::new()
        .with_prompt("Version")
        .items(&labels)
        .default(at)
        .interact()
        .context("asking for the version")?;
    Ok(recent[i].tag.clone())
}

fn checklist(prompt: &str, choices: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<BTreeSet<String>> {
    if choices.is_empty() {
        return Ok(BTreeSet::new());
    }
    let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
    let defaults: Vec<bool> = choices.iter().map(|c| checked.contains(&c.name)).collect();
    let picked = MultiSelect::new()
        .with_prompt(prompt)
        .items(&labels)
        .defaults(&defaults)
        .interact()
        .with_context(|| format!("asking for the {}", prompt.to_lowercase()))?;
    Ok(picked.into_iter().map(|i| choices[i].name.clone()).collect())
}

/// Step 5: installed means checked; a fresh device, nothing.
pub fn ask_plugins(choices: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<BTreeSet<String>> {
    checklist("Plugins (space to check, enter to go on)", choices, checked)
}

/// Step 6, the same rule.
pub fn ask_packs(choices: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<BTreeSet<String>> {
    checklist("Languages (space to check, enter to go on)", choices, checked)
}

/// Step 7: one question per unchecked plugin that has data. Only called
/// with at least one; the answer is what to erase.
pub fn ask_erase(candidates: &[String]) -> anyhow::Result<BTreeSet<String>> {
    let mut erase = BTreeSet::new();
    for name in candidates {
        let yes = Confirm::new()
            .with_prompt(format!("Also erase the data of {name}?"))
            .default(false)
            .interact()
            .context("asking about a plugin's data")?;
        if yes {
            erase.insert(name.clone());
        }
    }
    Ok(erase)
}

/// The one question of a total removal (spec §8).
pub fn ask_erase_all() -> anyhow::Result<bool> {
    Confirm::new()
        .with_prompt("Also erase the data (/var/lib/ritornello) and the ritornello account?")
        .default(false)
        .interact()
        .context("asking about the data")
}

/// Step 8's summary, shown before the confirmation (or instead of it, with
/// `--yes`).
pub fn show_summary(plan: &Plan, product: &str, source: &str, host: &str) {
    eprintln!("On {host}, Ritornello {product} from {source}:");
    for line in summary_lines(plan) {
        eprintln!("{line}");
    }
}

/// Step 8's confirmation.
pub fn confirm() -> anyhow::Result<bool> {
    Confirm::new().with_prompt("Go ahead?").default(false).interact().context("asking for confirmation")
}

/// The sudo password, never echoed.
pub fn ask_sudo_password(host: &str) -> anyhow::Result<String> {
    Password::new()
        .with_prompt(format!("sudo password on {host}"))
        .allow_empty_password(true)
        .interact()
        .context("asking for the sudo password")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::tests::{RADIO_EXEC, THEIRS_EXEC, dev, inv, set};
    use crate::plan::{Intent, Summary, compute};

    fn names(choices: &[Choice]) -> Vec<&str> {
        choices.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn plugins_are_ours_in_reference_order_then_third_party_ones_marked() {
        let device = dev(&[("theirs", THEIRS_EXEC), ("radio", RADIO_EXEC)], None, &[], &[]);
        let choices = plugin_choices(&inv(), &device);
        assert_eq!(names(&choices), ["radio", "cd", "files", "nrj-metas", "musicbrainz", "theirs"]);
        assert_eq!(choices[0].label, "radio (0.2.0-beta.2)");
        assert_eq!(choices[5].label, "theirs (third-party, managed from the web UI)");
        assert!(!names(&choices).contains(&"core"), "the core always comes with the rest");
    }

    #[test]
    fn packs_are_the_shipped_ones_then_the_installed_ones_the_release_lacks() {
        let choices = pack_choices(&inv(), &set(&["de", "fr"]));
        assert_eq!(names(&choices), ["fr", "de"]);
        assert_eq!(choices[1].label, "de (installed, not in this release)");
    }

    /// The data screen appears only when something that goes has data.
    #[test]
    fn only_a_removed_plugin_with_data_is_asked_about() {
        let device = dev(&[("radio", RADIO_EXEC), ("theirs", THEIRS_EXEC)], None, &[], &["radio", "theirs"]);
        assert_eq!(data_choices(&set(&["theirs"]), &device), ["theirs"]);
        let device = dev(&[("radio", RADIO_EXEC), ("theirs", THEIRS_EXEC)], None, &[], &["radio"]);
        assert!(data_choices(&set(&["theirs"]), &device).is_empty());
    }

    #[test]
    fn the_summary_lists_every_non_empty_heading() {
        let summary = Summary {
            installed: vec!["cd".into()],
            updated: vec!["core".into(), "radio".into()],
            removed: vec![],
            erased: vec![],
            kept_third_party: vec!["theirs".into()],
        };
        let plan = Plan { summary, ..Plan::default() };
        assert_eq!(
            summary_lines(&plan),
            ["  installed: cd", "  updated: core, radio", "  left as they are (third-party): theirs"]
        );
        assert_eq!(summary_lines(&Plan::default()), ["  nothing to change"]);
    }

    /// A total removal with its data, on a device whose survey saw no data
    /// at all: the summary still says the whole data tree goes, and the
    /// account with it — read from the plan, not from the survey.
    ///
    /// **[MUTATION]**: drop either line `summary_lines` derives from
    /// `plan.remove_trees` / `plan.remove_user` — this test fails.
    #[test]
    fn a_total_removal_with_its_data_names_the_data_root_and_the_account() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &[], &[]);
        assert!(device.data_nonempty.is_empty() && device.user_exists);
        let plan = compute(&inv(), &device, &Intent::RemoveAll { erase_data: true }).unwrap();
        assert!(plan.summary.erased.is_empty(), "the survey saw no data: {:?}", plan.summary);
        let lines = summary_lines(&plan).join("\n");
        assert!(lines.contains("the whole of /var/lib/ritornello"), "{lines}");
        assert!(lines.contains("the ritornello account is removed"), "{lines}");
        // Without its data, neither is said, since neither is done.
        let plan = compute(&inv(), &device, &Intent::RemoveAll { erase_data: false }).unwrap();
        let lines = summary_lines(&plan).join("\n");
        assert!(!lines.contains("/var/lib/ritornello") && !lines.contains("account"), "{lines}");
    }

    /// R52: "Remove everything" is offered whenever anything of ours is on
    /// the device, not only when it is not fresh.
    ///
    /// **[MUTATION]**: reduce `offers_remove_all` to `!dev.is_fresh()` —
    /// this test fails.
    #[test]
    fn remove_everything_is_offered_whenever_anything_of_ours_is_there() {
        let nothing = dev(&[], None, &[], &[]);
        assert!(!offers_remove_all(&nothing));
        let mut d = nothing.clone();
        d.core_present = true;
        assert!(offers_remove_all(&d), "the core");
        let d = dev(&[("radio", RADIO_EXEC)], None, &[], &[]);
        assert!(offers_remove_all(&d), "a declared plugin");
        let mut d = nothing.clone();
        d.registry = Some(crate::plan::tests::registry(&[]));
        assert!(offers_remove_all(&d), "a registry");
        let mut d = nothing.clone();
        d.packs.insert("ritornello-lang-fr".into());
        assert!(offers_remove_all(&d), "a pack");
        let mut d = nothing.clone();
        d.user_exists = true;
        assert!(offers_remove_all(&d), "the account");
    }
}
