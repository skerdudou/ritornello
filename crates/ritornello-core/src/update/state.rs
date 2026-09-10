//! What the configuration page is told about every component.
//!
//! One pure function over three lists — what is declared, what is on disk,
//! what the release offers — and that is the whole of it. `Availability` began
//! at four states because three were not enough: a declared plugin whose binary
//! is absent used to show as **dead**, which is what the release archives made
//! possible and what sent the diagnosis in the wrong direction. It carries six
//! now — installing a plugin that was never declared, and undeclaring one whose
//! binary stays on disk, are each their own situation licensing its own
//! gesture, and collapsing either into a neighbour would tell the operator to
//! do the wrong thing.

use crate::update::release::{differs, origin, Offer, Origin, Published};
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
    /// Where its announcement said its releases live, **verbatim**: today a
    /// full URL, because that is what `CARGO_PKG_REPOSITORY` holds.
    ///
    /// Stored raw and interpreted through `release::origin`, in one place, so
    /// there is exactly one fact here and no second field to disagree with it.
    /// A plugin that never announced — switched off, dead, predating the
    /// field — says `None`, which is "nothing to say", never "ours".
    pub repository: Option<String>,
}

/// What a third-party plugin's **own** repository publishes for it.
///
/// Kept apart from the official fold rather than concatenated into it, and
/// that separation is the point: a third-party plugin's name is free-form and
/// may collide with an official one, so a single list keyed by name could hand
/// a third-party row the official archive of the same name — the silent swap
/// `a_third_party_plugin_does_not_inherit_a_colliding_official_version`
/// exists to forbid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThirdPartyOffer {
    /// The plugin's name as `plugins.toml` declares it, which is also the name
    /// its announcement echoed back.
    pub name: String,
    pub published: Published,
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
    /// The core's own row only: what its last-installed archive carried
    /// outside what `install_one` ever places (the privileged installer, the
    /// systemd units, the polkit rules — see `archive::core_not_installed`).
    /// Absent until a core install has actually happened; `None` is never
    /// "nothing was left out" — the core's archive always carries something
    /// here, by design (see `installable_from_ui`'s own doc comment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_installed_files: Option<Vec<String>>,
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
    third_party: &[ThirdPartyOffer],
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
        // Only ever filled in by `Worker::install_one`, once a core install
        // has actually read an archive — `component_offers` never sees one.
        not_installed_files: None,
    });

    for plugin in installed {
        // One reading of the announcement, and everything about this row that
        // depends on where the binary came from falls out of it.
        let from = origin(plugin.repository.as_deref());
        // **`Unknown` joins `Ours`, deliberately.** "Announced no repository"
        // and "has not announced at all" are the same value here, and the
        // second covers every row the page offers an Install button for: a
        // declared plugin whose binary is missing, one that is switched off,
        // one that is dead, one the device does not have. Judged against
        // nothing, those rows would carry no offered version and could never
        // be installed — which is three documented gestures gone.
        //
        // The price is stated in `docs/plugins.md` rather than hidden: a
        // third-party plugin that announces no repository *and* takes one of
        // our names is offered our archive of that name. One line of
        // `Cargo.toml` on the author's side settles it.
        //
        // **What stops it is `placement_target`, not the automatic policy.** A
        // row that is switched off or dead announces no version, and
        // `automatic_install_list` does exclude those — but a **running** one
        // announces its version, so the automatic policy does reach it. What
        // it then meets is the check that the archive's bare name is the file
        // this plugin's `exec` declares: our archive carries
        // `ritornello-plugin-<name>`, so the install is refused with a named
        // cause unless the author called their binary exactly that too. Three
        // coincidences, all on their side, and the last one is the one they
        // control.
        let is_third_party = !matches!(from, Origin::Unknown | Origin::Ours);
        // A third-party plugin's version is decided by **its own** repository,
        // and this release says nothing about it: never `plugin_offered`, even
        // when a name collides. An `Origin::Foreign` has no repository this
        // updater can address, so it has no offer either.
        let offered = if is_third_party {
            third_party.iter().find(|o| o.name == plugin.name).map(|o| o.published.version.clone())
        } else {
            plugin_offered(&plugin.name)
        };
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
            kind: if is_third_party { ComponentKind::ThirdParty } else { ComponentKind::Plugin },
            declared: plugin.declared,
            binary_present: plugin.binary_present,
            installed: plugin.version.clone(),
            offered,
            availability,
            installable: None,
            third_party_repo: from.third_party_repo(),
            // A plugin's own row, never the core's: this field is a fact
            // about the core's archive alone.
            not_installed_files: None,
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
            not_installed_files: None,
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
    /// The repository has no published release **that this device could ever
    /// be offered**. A statement, not a fault.
    ///
    /// It arrives as `200` with a list this device read as empty, never as a
    /// status code: the releases endpoint answers `[]` for a repository that
    /// has none, which is why `parse_releases` and not the HTTP layer is what
    /// decides this. Two bodies reduce to it — a genuinely empty list, and one
    /// holding nothing but drafts, which a device polling with no token cannot
    /// see in the first place. A list of prereleases alone is **not** one of
    /// them: that is `OnlyPrereleases`, next.
    NoRelease,
    /// Something is published, and every published release is a prerelease
    /// this device declined.
    ///
    /// Its own variant and not a shade of `NoRelease`, because the two differ
    /// in the only way that matters to a reader: this one names a switch they
    /// own. Telling them "no release published yet" while a beta sits on the
    /// repository states a falsehood about the world and hides the one gesture
    /// that would change the answer.
    OnlyPrereleases,
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
    /// Bare file names in the plugins directory whose **erasure is queued and
    /// has not answered yet**.
    ///
    /// Not `busy`, deliberately, and for the reason `remove_plugin_binary`'s
    /// doc already gives: `busy` is the update card's one line, and an
    /// uninstall is a plugin-management gesture, not an update. This is a
    /// second, per-file fact, and it exists because the alternative was
    /// leaving the operator to guess. An uninstall answers the moment the job
    /// is queued — the declaration is already gone, the binary is not — so the
    /// row correctly reappears as "installed but not declared", and without
    /// this field it also offers "Remove the binary", inviting a gesture that
    /// is already in flight.
    ///
    /// Files and not component names: this is what `Action::RemovePlugin`
    /// names, what `DELETE /api/plugins/binaries/{file}` names, and what the
    /// scan behind an undeclared row finds. A hand-dropped binary may carry no
    /// component name at all.
    ///
    /// **Emptied by the worker on every exit of `remove_plugin_binary`**,
    /// success or failure. A failed erasure must fall back to the plain
    /// undeclared row with the reason on the card above — a row stuck on
    /// "erasure in progress" would be the same lie in the other direction.
    ///
    /// Lives here rather than in a handle of its own because both writers
    /// already hold this lock (the route through `AppState.update`, the worker
    /// through `Worker.state`) and so does `status_json`, which stamps it onto
    /// `PluginStatus::removal_pending`. A dedicated field would have cost
    /// `AppState` and `PluginsControl` a member each and bought nothing.
    ///
    /// **`skip`, not `skip_serializing_if`: this one never goes on the wire.**
    /// It is shared state that happens to live in the struct the payload is
    /// serialized from, and the page already learns the fact where the row it
    /// decorates lives — `PluginStatus::removal_pending` on `/api/status`.
    /// Publishing it here as well would put a second, unread representation of
    /// one fact into the contract, which is the very thing this field's own
    /// doctrine refuses two paragraphs up.
    #[serde(skip)]
    pub pending_removals: Vec<String>,
}

impl UpdateState {
    /// Before the first check: the core's own row and nothing else.
    pub fn initial(core_version: &str, installed: &[Installed]) -> Self {
        Self {
            outcome: CheckOutcome::NeverChecked,
            release_version: None,
            release_url: None,
            last_check_unix_s: None,
            components: component_offers(core_version, &[], &[], installed),
            busy: None,
            last_rollback: None,
            // Nothing can be in flight before the first HTTP request: this
            // list only ever grows from a route, and it is not persisted —
            // a core that restarts mid-erasure has no queue left to wait on,
            // and the row then tells the truth from the scan alone.
            pending_removals: Vec::new(),
        }
    }

    /// **A declaration has just been removed and the binary's erasure queued.**
    ///
    /// The row this component keeps is the one a check would compute a moment
    /// later: undeclared, binary still on disk, no version — the process is
    /// stopped, so nothing announces one. `offered` is deliberately left
    /// alone: a release did not change because a device uninstalled
    /// something, and that field is what licenses the gesture back.
    ///
    /// Written here rather than left to the next check because
    /// `GET /api/update` serves a **stored** snapshot. Without it the row went
    /// on claiming the plugin declared and aligned, and since the page builds
    /// each plugin's row by merging this payload with `/api/status`, the row
    /// vanished outright the moment the binary was erased — leaving no gesture
    /// to bring the plugin back short of a GitHub check. That was the defect:
    /// an uninstall made a plugin unreachable from the page.
    pub fn declaration_removed(&mut self, name: &str, file: &str) {
        if let Some(row) = self.components.iter_mut().find(|c| c.name == name) {
            row.declared = false;
            row.binary_present = true;
            row.installed = None;
            row.availability = Availability::Undeclared;
        }
        self.mark_removal_pending(file);
    }

    /// `file`'s erasure is queued. Idempotent: two presses of the same button
    /// queue two jobs, and the second one finding the file already listed must
    /// not make the list carry it twice — `removal_answered` clears by value,
    /// and a duplicate would survive the first answer.
    pub fn mark_removal_pending(&mut self, file: &str) {
        if !self.pending_removals.iter().any(|f| f == file) {
            self.pending_removals.push(file.to_string());
        }
    }

    /// **The queued erasure answered.** `file` is no longer in flight.
    ///
    /// `removed` false is a failure or a skip: the row stays undeclared, which
    /// is the truth, and the reason is already on the card. Only the in-flight
    /// mark goes — a row left saying "erasure in progress" for ever would be
    /// the same kind of lie as the button that started this.
    ///
    /// On success the component has neither a declaration nor a binary, which
    /// is a row only when a release offers it. When none does there is nothing
    /// to click and the row goes: that is the fourth documented state
    /// answering by its own absence, and it is why publishing a release — not
    /// this method — is what makes an uninstalled plugin installable again.
    pub fn removal_answered(&mut self, name: &str, file: &str, removed: bool) {
        self.pending_removals.retain(|f| f != file);
        if !removed {
            return;
        }
        match self.components.iter().position(|c| c.name == name) {
            Some(i) if self.components[i].offered.is_some() => {
                let row = &mut self.components[i];
                row.declared = false;
                row.binary_present = false;
                row.installed = None;
                row.availability = Availability::NotInstalled;
            }
            Some(i) => {
                self.components.remove(i);
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for the tests that have no third-party offer to make: the
    /// ordinary shape, where every row is judged against our own release.
    fn offers(
        core_version: &str,
        published: &[Published],
        installed: &[Installed],
    ) -> Vec<ComponentOffer> {
        component_offers(core_version, published, &[], installed)
    }

    fn declared(name: &str, version: Option<&str>, binary: bool) -> Installed {
        Installed {
            name: name.to_string(),
            declared: true,
            binary_present: binary,
            version: version.map(str::to_string),
            repository: None,
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
        let offers = offers(
            "0.2.0",
            &[published(Offer::Plugin("radio".to_string()), "0.2.0")],
            &[declared("radio", Some("0.2.0"), true)],
        );
        let radio = offers.iter().find(|o| o.name == "radio").unwrap();
        assert_eq!(radio.availability, Availability::Aligned);
    }

    #[test]
    fn a_declared_plugin_at_another_version_is_offered_an_update() {
        let offers = offers(
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
        let offers = offers(
            "0.2.0",
            &[published(Offer::Plugin("mpd".to_string()), "0.2.0")],
            &[declared("mpd", None, false)],
        );
        let mpd = offers.iter().find(|o| o.name == "mpd").unwrap();
        assert_eq!(mpd.availability, Availability::BinaryMissing);
    }

    #[test]
    fn a_plugin_the_release_offers_and_nothing_declares_can_be_added() {
        let offers = offers(
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
        let offers = offers(
            "0.2.0",
            &[published(Offer::Plugin("cd".to_string()), "0.2.0")],
            &[Installed {
                name: "cd".to_string(),
                declared: false,
                binary_present: true,
                version: None,
                repository: None,
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
        let offers = offers("0.2.0", &[], &[]);
        let core = offers.iter().find(|o| o.kind == ComponentKind::Core).unwrap();
        assert_eq!(core.installed.as_deref(), Some("0.2.0"));
        assert_eq!(core.offered, None);
        // No release means nothing to say about alignment. `Aligned` would be
        // a claim, and `UpdateAvailable` would be a lie.
        assert_eq!(core.availability, Availability::Unknown);
        // Only `Worker::install_one` ever learns this, from an archive it
        // just read; this pure function never sees one.
        assert_eq!(core.not_installed_files, None);
    }

    #[test]
    fn the_bundle_is_never_offered_as_a_component() {
        let offers = offers(
            "0.2.0",
            &[published(Offer::Bundle, "0.2.0")],
            &[],
        );
        assert!(offers.iter().all(|o| o.name != "plugins"), "{offers:#?}");
    }

    #[test]
    fn a_third_party_plugin_is_listed_with_its_repository_and_never_as_official() {
        let offers = offers(
            "0.2.0",
            &[],
            &[Installed {
                name: "someones-plugin".to_string(),
                declared: true,
                binary_present: true,
                version: Some("1.4.0".to_string()),
                repository: Some("https://github.com/someone/their-plugin".to_string()),
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
        let offers = offers(
            "0.2.0",
            &[published(Offer::Plugin("someones-plugin".to_string()), "9.9.9")],
            &[Installed {
                name: "someones-plugin".to_string(),
                declared: true,
                binary_present: true,
                version: Some("1.4.0".to_string()),
                repository: Some("https://github.com/someone/their-plugin".to_string()),
            }],
        );
        let it = offers.iter().find(|o| o.name == "someones-plugin").unwrap();
        assert_eq!(it.offered, None);
        assert_eq!(it.availability, Availability::Unknown);
    }

    /// **The majority path, and the one every other test in this module
    /// misses.** All ten official plugin crates inherit
    /// `repository = "https://github.com/skerdudou/ritornello"` from
    /// `[workspace.package]`, so this literal is exactly what their
    /// announcements carry — while `release::REPO` is the pair
    /// `skerdudou/ritornello`. Compared as raw strings the two can never be
    /// equal, and every official plugin would come out `ThirdParty`: the core
    /// would go asking GitHub for a "third-party" release of its own plugins,
    /// and the row would still render, which is what makes the defect quiet.
    ///
    /// Written with the literal rather than with `REPO` or with whatever the
    /// code computes: a test that reused the code's own value could not catch
    /// this class of defect at all.
    #[test]
    fn a_plugin_announcing_our_own_workspace_url_is_official_and_judged_by_this_release() {
        let offers = offers(
            "0.2.0",
            &[published(Offer::Plugin("radio".to_string()), "0.3.0")],
            &[Installed {
                name: "radio".to_string(),
                declared: true,
                binary_present: true,
                version: Some("0.2.0".to_string()),
                repository: Some("https://github.com/skerdudou/ritornello".to_string()),
            }],
        );
        let radio = offers.iter().find(|o| o.name == "radio").unwrap();
        assert_eq!(radio.kind, ComponentKind::Plugin, "an official plugin, not a stranger");
        assert_eq!(radio.third_party_repo, None, "nothing third-party to check or to show");
        assert_eq!(radio.offered.as_deref(), Some("0.3.0"), "this release is what judges it");
        assert_eq!(radio.availability, Availability::UpdateAvailable);
    }

    /// A third-party plugin's own repository is what answers for it, and the
    /// answer lands on the same two fields every other row uses.
    #[test]
    fn a_third_party_plugin_is_judged_by_the_release_of_its_own_repository() {
        let theirs = ThirdPartyOffer {
            name: "someones-plugin".to_string(),
            published: published(Offer::Plugin("someones-plugin".to_string()), "2.0.0"),
        };
        let rows = component_offers(
            "0.2.0",
            // Ours publishes a colliding name at another version: it must not
            // be the one that answers, which is what tells a correct lookup
            // from one that merely found something.
            &[published(Offer::Plugin("someones-plugin".to_string()), "9.9.9")],
            &[theirs],
            &[Installed {
                name: "someones-plugin".to_string(),
                declared: true,
                binary_present: true,
                version: Some("1.4.0".to_string()),
                repository: Some("https://github.com/someone/their-plugin".to_string()),
            }],
        );
        let it = rows.iter().find(|o| o.name == "someones-plugin").unwrap();
        assert_eq!(it.kind, ComponentKind::ThirdParty);
        assert_eq!(it.third_party_repo.as_deref(), Some("someone/their-plugin"));
        assert_eq!(it.offered.as_deref(), Some("2.0.0"), "its own repository, never ours");
        assert_eq!(it.availability, Availability::UpdateAvailable);
    }

    /// A repository that is present and is not a GitHub URL this updater can
    /// address: third-party, and **not checkable**.
    ///
    /// Never adopted as ours — that would let any string which fails to parse
    /// pass for official — and never shown as up to date, which is the one
    /// answer that would be a claim rather than a silence.
    #[test]
    fn a_repository_we_cannot_address_is_third_party_and_says_so_rather_than_up_to_date() {
        let offers = offers(
            "0.2.0",
            &[published(Offer::Plugin("elsewhere".to_string()), "9.9.9")],
            &[Installed {
                name: "elsewhere".to_string(),
                declared: true,
                binary_present: true,
                version: Some("1.4.0".to_string()),
                repository: Some("https://gitlab.com/someone/thing".to_string()),
            }],
        );
        let it = offers.iter().find(|o| o.name == "elsewhere").unwrap();
        assert_eq!(it.kind, ComponentKind::ThirdParty);
        assert_eq!(
            it.third_party_repo.as_deref(),
            Some("https://gitlab.com/someone/thing"),
            "the row still names where the binary claims to come from"
        );
        assert_eq!(it.offered, None);
        assert_ne!(it.availability, Availability::Aligned, "never a claim of being up to date");
        assert_eq!(it.availability, Availability::Unknown);
    }

    #[test]
    fn a_declared_plugin_the_release_does_not_offer_is_not_shown_as_removable_by_accident() {
        // A plugin dropped from a later release, or one built by hand. Nothing
        // is known about it, and "not installed" would be a lie about a
        // plugin that is running.
        let offers = offers(
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
        let offers = offers(
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
        let offers = offers(
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

    /// A snapshot of a device that declares `console`, runs it at 0.2.0, and
    /// has read a release offering 0.2.1 — the state every test below starts
    /// an uninstall from.
    fn with_console_declared(offered: &[Published]) -> UpdateState {
        UpdateState {
            components: offers("0.2.0", offered, &[declared("console", Some("0.2.0"), true)]),
            ..UpdateState::initial("0.2.0", &[])
        }
    }

    fn row<'a>(state: &'a UpdateState, name: &str) -> Option<&'a ComponentOffer> {
        state.components.iter().find(|c| c.name == name)
    }

    /// **An uninstall leaves a row, and says the erasure is in flight.**
    ///
    /// The regression pinned here is the one an owner actually hit.
    /// `GET /api/update` serves a **stored** snapshot and nothing rebuilt it
    /// after an uninstall, so the row went on claiming the plugin declared and
    /// aligned. The page builds each plugin's row by merging that payload with
    /// `/api/status`, so once the binary was erased neither payload produced
    /// one: the plugin left the table altogether, with no gesture left to
    /// reinstall it short of a GitHub check.
    #[test]
    fn an_uninstall_leaves_an_undeclared_row_and_marks_the_erasure() {
        let mut state = with_console_declared(&[published(Offer::Plugin("console".to_string()), "0.2.1")]);
        state.declaration_removed("console", "ritornello-plugin-console");

        let row = row(&state, "console").expect("the row survives the uninstall");
        assert!(!row.declared, "the [[plugin]] block is gone");
        assert!(row.binary_present, "and the binary is not: its erasure is only queued");
        assert_eq!(row.installed, None, "a stopped plugin announces no version");
        assert_eq!(row.availability, Availability::Undeclared);
        assert_eq!(
            row.offered.as_deref(),
            Some("0.2.1"),
            "a release does not change because a device uninstalled something"
        );
        assert_eq!(state.pending_removals, vec!["ritornello-plugin-console".to_string()]);
    }

    /// The erasure answered: nothing declares it, nothing is on disk, and the
    /// release still offers it — which is exactly the row whose only gesture
    /// is Install. **This is the fix's point**: the way back exists without
    /// waiting on a network check.
    #[test]
    fn an_erased_binary_leaves_a_row_the_release_can_reinstall() {
        let mut state = with_console_declared(&[published(Offer::Plugin("console".to_string()), "0.2.1")]);
        state.declaration_removed("console", "ritornello-plugin-console");
        state.removal_answered("console", "ritornello-plugin-console", true);

        let row = row(&state, "console").expect("something is offered, so there is a row");
        assert!(!row.declared);
        assert!(!row.binary_present);
        assert_eq!(row.installed, None);
        assert_eq!(row.availability, Availability::NotInstalled);
        assert_eq!(row.offered.as_deref(), Some("0.2.1"));
        assert!(state.pending_removals.is_empty(), "nothing is in flight any more");
    }

    /// **And no row when nothing offers it**, which is the fourth documented
    /// state answering by its own absence: there is genuinely nothing to
    /// click. Publishing a release, not this method, is what makes an
    /// uninstalled plugin installable again — the distinction that cost an
    /// owner an afternoon.
    #[test]
    fn an_erased_binary_nothing_offers_leaves_no_row_at_all() {
        let mut state = with_console_declared(&[]);
        state.declaration_removed("console", "ritornello-plugin-console");
        state.removal_answered("console", "ritornello-plugin-console", true);

        assert!(row(&state, "console").is_none());
        assert!(state.pending_removals.is_empty());
    }

    /// A refused or failed erasure: the mark goes, the row does not.
    ///
    /// Both halves matter and for opposite reasons. Keeping the mark would
    /// leave the page saying "erasing…" for as long as the core runs, and
    /// probing for a change that will never come. Moving the row to
    /// `NotInstalled` would claim a binary gone that is still on the device —
    /// and it is the undeclared row, with the reason on the card above, that
    /// tells the operator to go and look.
    #[test]
    fn a_failed_erasure_clears_the_mark_and_keeps_the_undeclared_row() {
        let mut state = with_console_declared(&[published(Offer::Plugin("console".to_string()), "0.2.1")]);
        state.declaration_removed("console", "ritornello-plugin-console");
        state.removal_answered("console", "ritornello-plugin-console", false);

        let row = row(&state, "console").expect("the row stays");
        assert_eq!(row.availability, Availability::Undeclared, "the binary is still there");
        assert!(row.binary_present);
        assert!(state.pending_removals.is_empty(), "but nothing is waiting on it any more");
    }

    /// Two presses queue two jobs, and the first answer clears by value: a
    /// duplicated entry would survive it and strand the row on "erasing…".
    #[test]
    fn queueing_the_same_erasure_twice_lists_the_file_once() {
        let mut state = with_console_declared(&[]);
        state.mark_removal_pending("ritornello-plugin-console");
        state.mark_removal_pending("ritornello-plugin-console");
        assert_eq!(state.pending_removals.len(), 1);

        state.removal_answered("console", "ritornello-plugin-console", false);
        assert!(state.pending_removals.is_empty());
    }
}
