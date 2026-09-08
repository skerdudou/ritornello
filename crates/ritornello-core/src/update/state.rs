//! What the configuration page is told about every component.
//!
//! One pure function over three lists — what is declared, what is on disk,
//! what the release offers — and that is the whole of it. The four states it
//! distinguishes exist because three were not enough: a declared plugin whose
//! binary is absent used to show as **dead**, which is what the release
//! archives made possible and what sent the diagnosis in the wrong direction.

use crate::update::release::{differs, Offer, Published};
use serde::Serialize;

/// What the core knows about one plugin, before the release is consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    /// Present in `plugins.toml`.
    pub declared: bool,
    /// Its `exec` path exists on disk.
    pub binary_present: bool,
    /// As its announcement gave it. `None` for a plugin that never announced —
    /// switched off, absent, or predating the field.
    pub version: Option<String>,
    /// `owner/repo`, as its announcement gave it. Present makes it
    /// third-party.
    pub third_party_repo: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Core,
    Plugin,
    ThirdParty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Installed, and at the release's version.
    Aligned,
    /// Installed at another version.
    UpdateAvailable,
    /// Declared, and its binary is not on disk.
    BinaryMissing,
    /// The release offers it and nothing declares it.
    NotInstalled,
    /// Its binary is there and nothing declares it.
    Undeclared,
    /// Nothing can be said: no release read yet, a third-party plugin whose
    /// own repository decides, or a plugin this release does not carry.
    ///
    /// A state of its own rather than a default, because both defaults lie:
    /// `Aligned` would be a claim and `UpdateAvailable` an invitation to
    /// install something that does not exist.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentOffer {
    pub name: String,
    pub kind: ComponentKind,
    pub declared: bool,
    pub binary_present: bool,
    pub installed: Option<String>,
    pub offered: Option<String>,
    pub availability: Availability,
    /// Derived from the archive's contents once it has been fetched, so absent
    /// until a check has read it. `Some(false)` is the files plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub third_party_repo: Option<String>,
}

/// Declared plugins first, **in file order** — that order is the priority, for
/// the source cycle and for metadata arbitration alike — then what the release
/// offers and nothing declares. The core comes first of all.
///
/// `published` carries at most one entry per component (see `release::fold`):
/// a component this release never published simply has no entry, and that is
/// answered `Availability::Unknown` for that component alone, its neighbours
/// unaffected.
pub fn component_offers(
    core_version: &str,
    published: &[Published],
    installed: &[Installed],
) -> Vec<ComponentOffer> {
    let core_offered = published.iter().find(|p| p.offer == Offer::Core).map(|p| p.version.clone());

    // The bundle installs nothing component by component, so it is never a
    // row: a row named "plugins" would invite a gesture that does not exist.
    let plugin_offered = |name: &str| -> Option<String> {
        published
            .iter()
            .find(|p| matches!(&p.offer, Offer::Plugin(n) if n == name))
            .map(|p| p.version.clone())
    };
    let official: Vec<&str> = published
        .iter()
        .filter_map(|p| match &p.offer {
            Offer::Plugin(name) => Some(name.as_str()),
            Offer::Core | Offer::Bundle => None,
        })
        .collect();

    let mut out = Vec::with_capacity(installed.len() + official.len() + 1);
    out.push(ComponentOffer {
        name: "core".to_string(),
        kind: ComponentKind::Core,
        declared: true,
        binary_present: true,
        installed: Some(core_version.to_string()),
        offered: core_offered.clone(),
        availability: match &core_offered {
            None => Availability::Unknown,
            Some(v) if differs(Some(core_version), v) => Availability::UpdateAvailable,
            Some(_) => Availability::Aligned,
        },
        installable: None,
        third_party_repo: None,
    });

    for plugin in installed {
        let third_party = plugin.third_party_repo.is_some();
        // A third-party plugin's version is decided by its own repository, and
        // this release says nothing about it.
        let offered = if third_party { None } else { plugin_offered(&plugin.name) };
        let availability = if plugin.declared && !plugin.binary_present {
            Availability::BinaryMissing
        } else if !plugin.declared && plugin.binary_present {
            Availability::Undeclared
        } else {
            match offered.as_deref() {
                None => Availability::Unknown,
                Some(v) if differs(plugin.version.as_deref(), v) => Availability::UpdateAvailable,
                Some(_) => Availability::Aligned,
            }
        };
        out.push(ComponentOffer {
            name: plugin.name.clone(),
            kind: if third_party { ComponentKind::ThirdParty } else { ComponentKind::Plugin },
            declared: plugin.declared,
            binary_present: plugin.binary_present,
            installed: plugin.version.clone(),
            offered,
            availability,
            installable: None,
            third_party_repo: plugin.third_party_repo.clone(),
        });
    }

    for name in official {
        if installed.iter().any(|p| p.name == name) {
            continue;
        }
        out.push(ComponentOffer {
            name: name.to_string(),
            kind: ComponentKind::Plugin,
            declared: false,
            binary_present: false,
            installed: None,
            offered: plugin_offered(name),
            availability: Availability::NotInstalled,
            installable: None,
            third_party_repo: None,
        });
    }
    out
}

/// How the last **attempt** went — a check, or an install.
///
/// Not "the last check", though it started that way: an install refused for
/// want of space, of a digest or of the privileged unit has nowhere else to
/// say so, and §9 of the design asks the card for "le compte rendu de la
/// dernière tentative". One field for both because the page shows one
/// sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum CheckOutcome {
    NeverChecked,
    /// The repository has no published release. A statement, not a fault —
    /// this is what a device sees until the first one is published.
    ///
    /// It arrives as `200` with an empty list, never as a status code: the
    /// releases endpoint answers `[]` for a repository that has none, which
    /// is why `parse_releases` and not the HTTP layer is what decides this.
    NoRelease,
    Ok,
    /// A component was installed. Carries the sentence naming it and its new
    /// version.
    ///
    /// Its own variant rather than `Ok` with a message, because the two are
    /// different events and the page reacts differently: `Ok` is "the check
    /// answered", and this is "something changed on this device just now".
    /// Without it, a successful install was indistinguishable from nothing
    /// having happened — `busy` cleared, `outcome` still said `Ok`, and only
    /// the row moved.
    Installed(String),
    /// Carries a message already taken from the catalog, so the page renders
    /// it without a second lookup.
    Failed(String),
}

/// The payload of `GET /api/update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateState {
    pub outcome: CheckOutcome,
    pub release_version: Option<String>,
    pub release_url: Option<String>,
    pub last_check_unix_s: Option<u64>,
    pub components: Vec<ComponentOffer>,
    /// What is happening right now, as a catalog message, or `None` when idle.
    /// The page shows it and disables its buttons.
    pub busy: Option<String>,
    /// Left by the rollback unit. Without it, the only trace of a 3 a.m.
    /// rollback would be a version number that did not move.
    ///
    /// Typed as the privileged crate's own report (Ruling 3): it duplicates
    /// nothing, and its field names are the wire contract this payload
    /// exposes to the page.
    pub last_rollback: Option<ritornello_updater::rollback::Report>,
}

impl UpdateState {
    /// Before the first check: the core's own row and nothing else.
    pub fn initial(core_version: &str, installed: &[Installed]) -> Self {
        Self {
            outcome: CheckOutcome::NeverChecked,
            release_version: None,
            release_url: None,
            last_check_unix_s: None,
            components: component_offers(core_version, &[], installed),
            busy: None,
            last_rollback: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(name: &str, version: Option<&str>, binary: bool) -> Installed {
        Installed {
            name: name.to_string(),
            declared: true,
            binary_present: binary,
            version: version.map(str::to_string),
            third_party_repo: None,
        }
    }

    /// Small constructor to keep the fixtures readable now that a `Published`
    /// carries five fields per component (Ruling 7): only the offer and the
    /// version matter to these tests, so the rest is filled with values no
    /// test inspects.
    fn published(offer: Offer, version: &str) -> Published {
        Published {
            offer,
            version: version.to_string(),
            url: "https://x/asset.tar.gz".to_string(),
            size: 0,
            release_tag: "v0.0.0".to_string(),
            checksums_url: None,
        }
    }

    #[test]
    fn a_declared_plugin_with_its_binary_and_the_release_version_is_aligned() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Plugin("radio".to_string()), "0.2.0")],
            &[declared("radio", Some("0.2.0"), true)],
        );
        let radio = offers.iter().find(|o| o.name == "radio").unwrap();
        assert_eq!(radio.availability, Availability::Aligned);
    }

    #[test]
    fn a_declared_plugin_at_another_version_is_offered_an_update() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Plugin("radio".to_string()), "0.3.0")],
            &[declared("radio", Some("0.2.0"), true)],
        );
        let radio = offers.iter().find(|o| o.name == "radio").unwrap();
        assert_eq!(radio.availability, Availability::UpdateAvailable);
    }

    /// The wart the previous chantier created, and the one this repairs: the
    /// archives let the core and a SUBSET of plugins be installed, while the
    /// shipped plugins.toml declares them all. Today such a plugin shows as
    /// dead, which sends the diagnosis in the wrong direction.
    #[test]
    fn a_declared_plugin_whose_binary_is_absent_is_not_installed_rather_than_dead() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Plugin("mpd".to_string()), "0.2.0")],
            &[declared("mpd", None, false)],
        );
        let mpd = offers.iter().find(|o| o.name == "mpd").unwrap();
        assert_eq!(mpd.availability, Availability::BinaryMissing);
    }

    #[test]
    fn a_plugin_the_release_offers_and_nothing_declares_can_be_added() {
        let offers = component_offers(
            "0.2.0",
            &[
                published(Offer::Plugin("radio".to_string()), "0.2.0"),
                published(Offer::Plugin("mpd".to_string()), "0.2.0"),
            ],
            &[declared("radio", Some("0.2.0"), true)],
        );
        let mpd = offers.iter().find(|o| o.name == "mpd").unwrap();
        assert_eq!(mpd.availability, Availability::NotInstalled);
        assert!(!mpd.declared);
    }

    #[test]
    fn a_binary_present_but_undeclared_is_reported_as_such() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Plugin("cd".to_string()), "0.2.0")],
            &[Installed {
                name: "cd".to_string(),
                declared: false,
                binary_present: true,
                version: None,
                third_party_repo: None,
            }],
        );
        let cd = offers.iter().find(|o| o.name == "cd").unwrap();
        // Not an anomaly and not something to repair by itself: it is what a
        // hand-edited file or an interrupted uninstall leaves. The page offers
        // the two gestures that end it.
        assert_eq!(cd.availability, Availability::Undeclared);
    }

    #[test]
    fn the_core_is_always_a_component_even_when_no_release_is_known() {
        let offers = component_offers("0.2.0", &[], &[]);
        let core = offers.iter().find(|o| o.kind == ComponentKind::Core).unwrap();
        assert_eq!(core.installed.as_deref(), Some("0.2.0"));
        assert_eq!(core.offered, None);
        // No release means nothing to say about alignment. `Aligned` would be
        // a claim, and `UpdateAvailable` would be a lie.
        assert_eq!(core.availability, Availability::Unknown);
    }

    #[test]
    fn the_bundle_is_never_offered_as_a_component() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Bundle, "0.2.0")],
            &[],
        );
        assert!(offers.iter().all(|o| o.name != "plugins"), "{offers:#?}");
    }

    #[test]
    fn a_third_party_plugin_is_listed_with_its_repository_and_never_as_official() {
        let offers = component_offers(
            "0.2.0",
            &[],
            &[Installed {
                name: "someones-plugin".to_string(),
                declared: true,
                binary_present: true,
                version: Some("1.4.0".to_string()),
                third_party_repo: Some("someone/their-plugin".to_string()),
            }],
        );
        let it = offers.iter().find(|o| o.name == "someones-plugin").unwrap();
        assert_eq!(it.kind, ComponentKind::ThirdParty);
        assert_eq!(it.third_party_repo.as_deref(), Some("someone/their-plugin"));
        // Its own repository decides its version, not ours: this release says
        // nothing about it.
        assert_eq!(it.offered, None);
        assert_eq!(it.availability, Availability::Unknown);
    }

    /// A third-party plugin's name is free-form, chosen by its own author —
    /// the core keeps no registry that would refuse a name already used by an
    /// official plugin. The collision only needs the official plugin to be
    /// one this device does not currently have installed, which is exactly
    /// what `published` looks like here: an entry for "someones-plugin" that
    /// nothing in `installed` claims as official.
    ///
    /// Without the `third_party` guard on `offered`, this plugin would be
    /// shown `UpdateAvailable` toward the OFFICIAL version, and accepting
    /// that "update" would overwrite a third-party binary with the official
    /// one under the same name — silently swapping what the operator
    /// installed for something else.
    ///
    /// Deliberately a **new** test rather than widening
    /// `a_third_party_plugin_is_listed_with_its_repository_and_never_as_official`:
    /// that one publishes nothing at all, so it cannot tell a correct guard
    /// from an absent one that merely finds no matching entry either way. A
    /// fixture failing for two reasons proves neither.
    #[test]
    fn a_third_party_plugin_does_not_inherit_a_colliding_official_version() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Plugin("someones-plugin".to_string()), "9.9.9")],
            &[Installed {
                name: "someones-plugin".to_string(),
                declared: true,
                binary_present: true,
                version: Some("1.4.0".to_string()),
                third_party_repo: Some("someone/their-plugin".to_string()),
            }],
        );
        let it = offers.iter().find(|o| o.name == "someones-plugin").unwrap();
        assert_eq!(it.offered, None);
        assert_eq!(it.availability, Availability::Unknown);
    }

    #[test]
    fn a_declared_plugin_the_release_does_not_offer_is_not_shown_as_removable_by_accident() {
        // A plugin dropped from a later release, or one built by hand. Nothing
        // is known about it, and "not installed" would be a lie about a
        // plugin that is running.
        let offers = component_offers(
            "0.2.0",
            &[],
            &[declared("legacy", Some("0.2.0"), true)],
        );
        let it = offers.iter().find(|o| o.name == "legacy").unwrap();
        assert_eq!(it.availability, Availability::Unknown);
        assert_eq!(it.offered, None);
    }

    /// The behaviour Ruling 7 requires a test for and the task text cannot
    /// express: `published` is no longer a single release version applied to
    /// every component. Two plugins are installed, and `published` mentions
    /// only one of them — the other must come out `Unknown` on its own,
    /// while its neighbour is answered normally. A plausible implementation
    /// that instead defaults an absent entry to `Aligned` would pass every
    /// other test in this module and still let the page claim a
    /// never-published plugin is up to date.
    #[test]
    fn a_component_missing_from_published_is_unknown_on_its_own_while_its_neighbour_is_answered() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Plugin("radio".to_string()), "0.2.0")],
            &[
                declared("radio", Some("0.2.0"), true),
                declared("mpd", Some("0.2.0"), true),
            ],
        );
        let radio = offers.iter().find(|o| o.name == "radio").unwrap();
        assert_eq!(radio.availability, Availability::Aligned);
        let mpd = offers.iter().find(|o| o.name == "mpd").unwrap();
        assert_eq!(mpd.availability, Availability::Unknown);
        assert_eq!(mpd.offered, None);
    }

    #[test]
    fn the_order_of_declared_plugins_is_preserved_because_it_is_the_priority() {
        let offers = component_offers(
            "0.2.0",
            &[published(Offer::Plugin("mpd".to_string()), "0.2.0")],
            &[
                declared("radio", Some("0.2.0"), true),
                declared("cd", Some("0.2.0"), true),
                declared("musicbrainz", Some("0.2.0"), true),
            ],
        );
        let names: Vec<&str> = offers
            .iter()
            .filter(|o| o.kind != ComponentKind::Core)
            .map(|o| o.name.as_str())
            .collect();
        // Declared ones first, in file order; then what could be added.
        assert_eq!(names, vec!["radio", "cd", "musicbrainz", "mpd"]);
    }
}
