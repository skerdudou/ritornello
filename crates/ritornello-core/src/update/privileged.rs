//! Whether the core may update itself from the page, judged on the files of
//! its archive that only `ritornello-install` places: its systemd units, its
//! polkit rules and the privileged updater.
//!
//! **Why this exists.** The page installs the core's binary and nothing else
//! of its archive: root's updater can form exactly two paths, and none of
//! these is one (`crates/ritornello-updater/src/target.rs`). A release that
//! changes one of them used to be installed all the same, binary alone, with
//! only its notes to say "Action required" — and the night-time update read
//! no notes. v0.2.0-beta.7 was the case: its unit gave the service write
//! access to the network shares, and a core updated from the page kept the
//! old one. The same rule already held for a plugin whose root companion
//! moves (`update::companion_allows`); the core was exempt from it.
//!
//! **How a file is identified.** By the identity its release's
//! `inventory.json` publishes (format 2): `sha256:<hex>` of a unit or a rule,
//! `version:<number>` of the updater, since its bytes differ at every build
//! even when its code does not. `ritornello-install` records the identity of
//! each file it places, in its registry (`install_registry`), and only it
//! does: a file placed by hand is never vouched for. The two are compared for
//! **equality**, never read for meaning, like every version on a device.
//!
//! **Every unknown refuses**: an inventory that cannot be read or carries no
//! identity, a registry absent, unreadable or written before identities
//! existed. None of them proves the files on the device are the release's,
//! and the answer is the program that can place them.

use std::collections::BTreeMap;

/// Privileged destination path -> identity.
pub type Identities = BTreeMap<String, String>;

/// The identities `inventory.json` publishes for the core's privileged files.
///
/// `None` for anything but a format-2 inventory whose every privileged core
/// file carries a non-empty identity: an identity missing for one file would
/// let that file go unjudged. Read leniently otherwise — only these fields
/// matter here, and the installer is the reader that holds the whole shape.
pub fn core_identities(inventory: &str) -> Option<Identities> {
    let v: serde_json::Value = serde_json::from_str(inventory).ok()?;
    if v["format"] != 2 {
        return None;
    }
    let mut out = Identities::new();
    for f in v["core"]["files"].as_array()? {
        if f["privileged"] != true {
            continue;
        }
        let dest = f["dest"].as_str()?;
        let identity = f["identity"].as_str().filter(|s| !s.is_empty())?;
        out.insert(dest.to_string(), identity.to_string());
    }
    (!out.is_empty()).then_some(out)
}

/// `core_identities` of the inventory at `url`, `None` on any failure, which
/// refuses. One small read, bounded by the download client like a catalogue.
pub async fn fetch_core_identities(client: &reqwest::Client, url: &str) -> Option<Identities> {
    let (status, body) = crate::update::download::fetch_text(client, url).await.ok()?;
    if status != 200 {
        tracing::warn!("update: {url} answered HTTP {status}");
        return None;
    }
    let identities = core_identities(&body);
    if identities.is_none() {
        tracing::warn!("update: {url} names no identity for the core's privileged files");
    }
    identities
}

/// `Ok` when the release's privileged files are exactly the ones
/// `ritornello-install` recorded placing, each with the same identity: the
/// core's update from the page changes nothing only root can place.
///
/// Otherwise the files that need `ritornello-install`, sorted: one offered
/// with another identity, or with none recorded; one recorded that the
/// release no longer places (only the installer removes it). Every offered
/// file when nothing is recorded. **Empty** when the release's own
/// identities are unknown: the refusal stands, but no file can be named.
pub fn files_needing_installer(
    offered: Option<&Identities>,
    recorded: Option<&Identities>,
) -> Result<(), Vec<String>> {
    let Some(offered) = offered else { return Err(Vec::new()) };
    let empty = Identities::new();
    let recorded = recorded.unwrap_or(&empty);
    let mut files: Vec<String> = offered
        .iter()
        .filter(|(path, id)| recorded.get(*path) != Some(*id))
        .map(|(path, _)| path.clone())
        .chain(recorded.keys().filter(|path| !offered.contains_key(*path)).cloned())
        .collect();
    files.sort();
    files.dedup();
    if files.is_empty() { Ok(()) } else { Err(files) }
}

/// The privileged files whose `sha256:` identity the archive does not bear
/// out: what it carries at that path hashes otherwise, or is absent.
///
/// The judgement above trusts the release's inventory; this is the same
/// release's archive, read at the gesture, holding it to its word. A
/// `version:` identity is not checked here: no byte of a binary says which
/// number built it.
pub fn archive_disagrees(offered: &Identities, digests: &BTreeMap<String, String>) -> Vec<String> {
    offered
        .iter()
        .filter_map(|(dest, id)| {
            let hex = id.strip_prefix("sha256:")?;
            let path = dest.trim_start_matches('/');
            (digests.get(path).map(String::as_str) != Some(hex)).then(|| dest.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT: &str = "/etc/systemd/system/ritornello.service";
    const RULE: &str = "/etc/polkit-1/rules.d/52-ritornello-update.rules";
    const UPDATER: &str = "/usr/local/lib/ritornello/ritornello-update";

    fn ids(pairs: &[(&str, &str)]) -> Identities {
        pairs.iter().map(|(p, i)| (p.to_string(), i.to_string())).collect()
    }

    /// The real inventory this repository publishes, so a change of its
    /// shape reddens here and not on a device.
    fn real_inventory() -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out = std::process::Command::new("python3")
            .arg(root.join("scripts/install-inventory.py"))
            .output()
            .expect("python3 is available: the packaging tests already need it");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn the_real_inventory_names_every_privileged_file_of_the_core() {
        let got = core_identities(&real_inventory()).expect("the real inventory carries identities");
        let paths: Vec<&str> = got.keys().map(String::as_str).collect();
        assert_eq!(
            paths,
            vec![
                "/etc/polkit-1/rules.d/50-ritornello-power.rules",
                RULE,
                "/etc/systemd/system/ritornello-rollback.service",
                "/etc/systemd/system/ritornello-update.service",
                UNIT,
                UPDATER,
            ]
        );
        assert!(got[UNIT].starts_with("sha256:"), "{}", got[UNIT]);
        assert!(got[UPDATER].starts_with("version:"), "{}", got[UPDATER]);
    }

    /// Format 1 carried no identity at all, and an identity missing for one
    /// privileged file would let that file go unjudged: both read as unknown.
    #[test]
    fn another_format_or_a_missing_identity_is_unknown() {
        let real = real_inventory();
        assert!(core_identities(&real.replacen("\"format\": 2", "\"format\": 1", 1)).is_none());
        let id = core_identities(&real).unwrap()[UNIT].clone();
        let without = real.replacen(&format!("\"identity\": \"{id}\""), "\"identity\": \"\"", 1);
        assert_ne!(without, real);
        assert!(core_identities(&without).is_none());
        assert!(core_identities("not json").is_none());
    }

    #[test]
    fn the_same_identities_allow_the_update() {
        let offered = ids(&[(UNIT, "sha256:a"), (UPDATER, "version:1.0.0")]);
        assert_eq!(files_needing_installer(Some(&offered), Some(&offered.clone())), Ok(()));
    }

    /// The beta.7 case: one unit changed, the rest did not.
    #[test]
    fn one_changed_file_refuses_and_is_the_one_named() {
        let offered = ids(&[(UNIT, "sha256:new"), (RULE, "sha256:r"), (UPDATER, "version:1.0.0")]);
        let recorded = ids(&[(UNIT, "sha256:old"), (RULE, "sha256:r"), (UPDATER, "version:1.0.0")]);
        assert_eq!(files_needing_installer(Some(&offered), Some(&recorded)), Err(vec![UNIT.to_string()]));
    }

    /// The updater is judged by its number, like a companion.
    #[test]
    fn a_moved_updater_number_refuses() {
        let offered = ids(&[(UPDATER, "version:1.0.1")]);
        let recorded = ids(&[(UPDATER, "version:1.0.0")]);
        assert_eq!(files_needing_installer(Some(&offered), Some(&recorded)), Err(vec![UPDATER.to_string()]));
    }

    /// A registry written before identities existed (every device deployed
    /// before this), or none at all: every offered file is unknown.
    #[test]
    fn nothing_recorded_names_every_offered_file() {
        let offered = ids(&[(UNIT, "sha256:a"), (RULE, "sha256:r")]);
        assert_eq!(
            files_needing_installer(Some(&offered), None),
            Err(vec![RULE.to_string(), UNIT.to_string()])
        );
    }

    /// A file the release no longer places is the installer's to remove.
    #[test]
    fn a_recorded_file_the_release_dropped_refuses() {
        let offered = ids(&[(UNIT, "sha256:a")]);
        let recorded = ids(&[(UNIT, "sha256:a"), (RULE, "sha256:r")]);
        assert_eq!(files_needing_installer(Some(&offered), Some(&recorded)), Err(vec![RULE.to_string()]));
    }

    /// The release's identities unknown: refused, with nothing to name.
    #[test]
    fn unknown_offered_identities_refuse_without_naming() {
        assert_eq!(files_needing_installer(None, Some(&ids(&[(UNIT, "sha256:a")]))), Err(vec![]));
        assert_eq!(files_needing_installer(None, None), Err(vec![]));
    }

    #[test]
    fn the_archive_must_bear_out_every_hashed_identity() {
        let offered = ids(&[(UNIT, "sha256:aa"), (RULE, "sha256:bb"), (UPDATER, "version:1.0.0")]);
        let digests: BTreeMap<String, String> = [
            ("etc/systemd/system/ritornello.service".to_string(), "aa".to_string()),
            ("etc/polkit-1/rules.d/52-ritornello-update.rules".to_string(), "cc".to_string()),
        ]
        .into();
        // The rule hashes otherwise; the updater is not read for bytes.
        assert_eq!(archive_disagrees(&offered, &digests), vec![RULE.to_string()]);
        // Absent from the archive is a disagreement too.
        let only_unit: BTreeMap<String, String> =
            [("etc/systemd/system/ritornello.service".to_string(), "aa".to_string())].into();
        assert_eq!(archive_disagrees(&offered, &only_unit), vec![RULE.to_string()]);
    }
}
