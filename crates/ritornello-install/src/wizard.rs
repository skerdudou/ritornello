//! The questions of an interactive run, in order, with Esc going back one.
//!
//! Each screen answers, or says "back" (`None`). Going back keeps every
//! answer given: the question asked again shows it as its default, and so
//! does each question after it once the operator goes forward again. The
//! owner validated the plugins too quickly, landed on the languages, and had
//! nothing to return with but a "no" at the confirmation and a fresh start.
//!
//! A question that is not shown — the action on a device with nothing of
//! ours, the version with `--version` or a local directory, a checklist with
//! nothing to offer, the permissions with `--grants`, the data of a plugin
//! that keeps it — is skipped both ways: going back never lands on a screen
//! that answers itself and sends the operator forward again.
//!
//! Nothing here goes back past the action: the address and the survey before
//! it are another device's business, and changing device is a new run.

use std::collections::BTreeSet;

use crate::device::DeviceState;
use crate::grants::{self, Grant};
use crate::inventory::Inventory;
use crate::plan::{self, Intent};
use crate::source::Source;
use crate::ui::{self, Action, Choice};

/// One question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Action,
    Version,
    Plugins,
    Packs,
    Grants,
    /// The data of the n-th plugin that goes and has some.
    Erase(usize),
    /// The total removal's one question.
    EraseAll,
}

/// The screens, one method each; `None` is Esc. A trait so that the order,
/// and above all what going back keeps, is tested without a terminal.
pub trait Screens {
    fn action(&mut self, default: Action) -> anyhow::Result<Option<Action>>;
    /// `default`: the tag answered before, or `None` for the source's own.
    fn version(&mut self, default: Option<&str>) -> anyhow::Result<Option<String>>;
    /// The inventory of `tag`, or of the source's own version for `None`.
    /// Not a question: read when a question needs it.
    fn inventory(&mut self, tag: Option<&str>) -> anyhow::Result<Inventory>;
    fn plugins(&mut self, choices: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<Option<BTreeSet<String>>>;
    fn packs(&mut self, choices: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<Option<BTreeSet<String>>>;
    /// Returns the declined grants; `declined` is what is preselected so.
    fn grants(&mut self, declined: &BTreeSet<Grant>) -> anyhow::Result<Option<BTreeSet<Grant>>>;
    fn erase(&mut self, plugin: &str) -> anyhow::Result<Option<bool>>;
    fn erase_all(&mut self) -> anyhow::Result<Option<bool>>;
}

/// The questions, and every answer given so far.
pub struct Wizard<'a> {
    dev: &'a DeviceState,
    ask_action: bool,
    ask_version: bool,
    /// `--grants`, which answers the permissions question.
    declined_by_args: Option<BTreeSet<Grant>>,
    /// The question being asked, as an index into `steps()`.
    at: usize,
    action: Action,
    tag: Option<String>,
    plugins: Option<BTreeSet<String>>,
    packs: Option<BTreeSet<String>>,
    declined: Option<BTreeSet<Grant>>,
    /// The plugins whose data the operator said to erase. Kept by name, so
    /// that a plugin checked again is simply no longer a candidate.
    erase: BTreeSet<String>,
    erase_all: bool,
    /// The inventory read last, and the tag it was read for.
    inv: Option<(Option<String>, Inventory)>,
}

impl<'a> Wizard<'a> {
    /// `action`: the action when it is not asked (`--reinstall`, or a device
    /// with nothing of ours), `None` to ask it.
    pub fn new(
        dev: &'a DeviceState,
        action: Option<Action>,
        ask_version: bool,
        declined_by_args: Option<BTreeSet<Grant>>,
    ) -> Self {
        Self {
            dev,
            ask_action: action.is_none(),
            ask_version,
            declined_by_args,
            at: 0,
            action: action.unwrap_or(Action::InstallOrUpdate),
            tag: None,
            plugins: None,
            packs: None,
            declined: None,
            erase: BTreeSet::new(),
            erase_all: false,
            inv: None,
        }
    }

    /// Asks from the current question to the last, and returns what the
    /// operator wants. Called again after `back_from_confirmation`.
    pub fn ask(&mut self, screens: &mut impl Screens) -> anyhow::Result<Intent> {
        loop {
            if self.at >= self.before_inventory() {
                self.read_inventory(screens)?;
            }
            let steps = self.steps();
            let Some(&step) = steps.get(self.at) else { return Ok(self.intent()) };
            if self.ask_one(step, screens)? {
                self.at += 1;
            } else {
                // Esc on the first question asks it again.
                self.at = self.at.saturating_sub(1);
            }
        }
    }

    /// The inventory the answers were given against. Only after `ask`.
    pub fn inventory(&self) -> &Inventory {
        &self.inv.as_ref().expect("`ask` reads the inventory before it returns").1
    }

    /// Whether Esc at the confirmation has a question to return to.
    pub fn can_go_back(&self) -> bool {
        !self.steps().is_empty()
    }

    /// Esc at the confirmation: the next `ask` starts at the last question.
    pub fn back_from_confirmation(&mut self) {
        self.at = self.steps().len().saturating_sub(1);
    }

    /// How many questions come before the inventory is needed: the action,
    /// and the version that names which inventory.
    fn before_inventory(&self) -> usize {
        usize::from(self.ask_action) + usize::from(self.version_asked())
    }

    fn version_asked(&self) -> bool {
        self.ask_version && self.action != Action::RemoveAll
    }

    /// Reads the inventory the answers so far call for, unless it is the
    /// one already read: the chosen version's, or the source's own for a
    /// total removal (which removes what the newest inventory knows) and
    /// when no version is asked.
    fn read_inventory(&mut self, screens: &mut impl Screens) -> anyhow::Result<()> {
        let wanted = if self.version_asked() { self.tag.clone() } else { None };
        if self.inv.as_ref().is_some_and(|(tag, _)| *tag == wanted) {
            return Ok(());
        }
        let inv = screens.inventory(wanted.as_deref())?;
        self.inv = Some((wanted, inv));
        Ok(())
    }

    /// The questions the answers so far lead to, in order. Those after the
    /// version are only known once its inventory is read.
    fn steps(&self) -> Vec<Step> {
        let mut steps = Vec::new();
        if self.ask_action {
            steps.push(Step::Action);
        }
        if self.action == Action::RemoveAll {
            steps.push(Step::EraseAll);
            return steps;
        }
        if self.version_asked() {
            steps.push(Step::Version);
        }
        let Some((_, inv)) = &self.inv else { return steps };
        let (_, installed_packs) = plan::preselection(inv, self.dev);
        if !ui::plugin_choices(inv, self.dev).is_empty() {
            steps.push(Step::Plugins);
        }
        if !ui::pack_choices(inv, &installed_packs).is_empty() {
            steps.push(Step::Packs);
        }
        if self.declined_by_args.is_none() {
            steps.push(Step::Grants);
        }
        steps.extend((0..self.erase_candidates().len()).map(Step::Erase));
        steps
    }

    /// The plugins that go with the plugins answered, and have data.
    fn erase_candidates(&self) -> Vec<String> {
        match (&self.inv, &self.plugins) {
            (Some((_, inv)), Some(plugins)) => {
                ui::data_choices(&crate::cli::removed_by(inv, self.dev, plugins), self.dev)
            }
            _ => Vec::new(),
        }
    }

    /// Asks one question and keeps its answer; `false` is "back".
    fn ask_one(&mut self, step: Step, screens: &mut impl Screens) -> anyhow::Result<bool> {
        let answered = match step {
            Step::Action => screens.action(self.action)?.map(|a| {
                self.action = a;
            }),
            Step::Version => screens.version(self.tag.as_deref())?.map(|t| self.tag = Some(t)),
            Step::Plugins => {
                let inv = self.inventory();
                let choices = ui::plugin_choices(inv, self.dev);
                let checked = self.plugins.clone().unwrap_or_else(|| plan::preselection(inv, self.dev).0);
                screens.plugins(&choices, &checked)?.map(|p| self.plugins = Some(p))
            }
            Step::Packs => {
                let inv = self.inventory();
                let (_, installed) = plan::preselection(inv, self.dev);
                let choices = ui::pack_choices(inv, &installed);
                let checked = self.packs.clone().unwrap_or(installed);
                screens.packs(&choices, &checked)?.map(|p| self.packs = Some(p))
            }
            Step::Grants => {
                let declined =
                    self.declined.clone().unwrap_or_else(|| grants::recorded_declined(self.dev.registry.as_ref()));
                screens.grants(&declined)?.map(|d| self.declined = Some(d))
            }
            Step::Erase(i) => {
                let name = self.erase_candidates()[i].clone();
                screens.erase(&name)?.map(|yes| {
                    if yes {
                        self.erase.insert(name);
                    } else {
                        self.erase.remove(&name);
                    }
                })
            }
            Step::EraseAll => screens.erase_all()?.map(|yes| self.erase_all = yes),
        };
        if answered.is_some() && step == Step::Action {
            // Refused as soon as the action is known, before the version and
            // the inventory (`check_ca_certificates`).
            crate::check_ca_certificates(self.dev, self.action, false)?;
        }
        Ok(answered.is_some())
    }

    /// What the answers say. A screen that was skipped answers as it did
    /// when it returned at once on nothing to offer: nothing checked.
    fn intent(&self) -> Intent {
        if self.action == Action::RemoveAll {
            return Intent::RemoveAll { erase_data: self.erase_all };
        }
        let candidates = self.erase_candidates();
        Intent::InstallOrUpdate {
            plugins: self.plugins.clone().unwrap_or_default(),
            packs: self.packs.clone().unwrap_or_default(),
            erase_data: self.erase.iter().filter(|n| candidates.contains(n)).cloned().collect(),
            reinstall: self.action == Action::Repair,
            declined: self
                .declined_by_args
                .clone()
                .or_else(|| self.declined.clone())
                .unwrap_or_else(|| grants::recorded_declined(self.dev.registry.as_ref())),
        }
    }
}

/// The real screens: the terminal, and the source for the version and its
/// inventory.
pub struct Terminal<'s> {
    pub source: &'s mut dyn Source,
    /// The source's own tag before any answer: what a total removal, or a
    /// run with no version screen, reads its inventory from.
    pub initial: Option<&'s str>,
}

impl Screens for Terminal<'_> {
    fn action(&mut self, default: Action) -> anyhow::Result<Option<Action>> {
        ui::ask_action(default)
    }

    fn version(&mut self, default: Option<&str>) -> anyhow::Result<Option<String>> {
        let (releases, own) = self.source.versions().expect("the version is asked only of a source that has several");
        let default = default.or(self.initial).unwrap_or(own).to_string();
        ui::ask_version(releases, &default)
    }

    fn inventory(&mut self, tag: Option<&str>) -> anyhow::Result<Inventory> {
        if let Some(tag) = tag.or(self.initial) {
            self.source.select(tag)?;
        }
        eprintln!("Reading the inventory of {}...", self.source.label());
        Inventory::parse(&self.source.inventory()?)
    }

    fn plugins(&mut self, choices: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<Option<BTreeSet<String>>> {
        ui::ask_plugins(choices, checked)
    }

    fn packs(&mut self, choices: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<Option<BTreeSet<String>>> {
        ui::ask_packs(choices, checked)
    }

    fn grants(&mut self, declined: &BTreeSet<Grant>) -> anyhow::Result<Option<BTreeSet<Grant>>> {
        ui::ask_grants(declined)
    }

    fn erase(&mut self, plugin: &str) -> anyhow::Result<Option<bool>> {
        ui::ask_erase(plugin)
    }

    fn erase_all(&mut self) -> anyhow::Result<Option<bool>> {
        ui::ask_erase_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::tests::{RADIO_EXEC, dev, inv, set};

    /// Screens that answer from a script and write down what each one was
    /// shown. A reply of `"esc"` is Esc; otherwise a list is comma
    /// separated, and a yes-or-no question reads `"y"` or `"n"`.
    struct Scripted {
        replies: Vec<&'static str>,
        shown: Vec<String>,
    }

    impl Scripted {
        fn new(replies: &[&'static str]) -> Self {
            Self { replies: replies.to_vec(), shown: Vec::new() }
        }

        fn reply(&mut self, shown: String) -> Option<&'static str> {
            self.shown.push(shown);
            assert!(!self.replies.is_empty(), "a question nobody scripted: {:?}", self.shown);
            Some(self.replies.remove(0)).filter(|r| *r != "esc")
        }
    }

    fn list(s: &str) -> BTreeSet<String> {
        s.split(',').filter(|n| !n.is_empty()).map(str::to_string).collect()
    }

    fn joined(names: &BTreeSet<String>) -> String {
        names.iter().cloned().collect::<Vec<_>>().join(",")
    }

    impl Screens for Scripted {
        fn action(&mut self, default: Action) -> anyhow::Result<Option<Action>> {
            Ok(self.reply(format!("action {default:?}")).map(|r| match r {
                "install" => Action::InstallOrUpdate,
                "repair" => Action::Repair,
                "remove" => Action::RemoveAll,
                other => panic!("no such action {other}"),
            }))
        }
        fn version(&mut self, default: Option<&str>) -> anyhow::Result<Option<String>> {
            Ok(self.reply(format!("version {}", default.unwrap_or("-"))).map(str::to_string))
        }
        fn inventory(&mut self, tag: Option<&str>) -> anyhow::Result<Inventory> {
            self.shown.push(format!("read {}", tag.unwrap_or("own")));
            Ok(inv())
        }
        fn plugins(&mut self, _: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<Option<BTreeSet<String>>> {
            Ok(self.reply(format!("plugins [{}]", joined(checked))).map(list))
        }
        fn packs(&mut self, _: &[Choice], checked: &BTreeSet<String>) -> anyhow::Result<Option<BTreeSet<String>>> {
            Ok(self.reply(format!("packs [{}]", joined(checked))).map(list))
        }
        fn grants(&mut self, declined: &BTreeSet<Grant>) -> anyhow::Result<Option<BTreeSet<Grant>>> {
            let names: Vec<&str> = declined.iter().map(|g| g.name()).collect();
            Ok(self
                .reply(format!("grants declined [{}]", names.join(",")))
                .map(|r| list(r).iter().map(|n| Grant::parse(n).expect("a grant")).collect()))
        }
        fn erase(&mut self, plugin: &str) -> anyhow::Result<Option<bool>> {
            Ok(self.reply(format!("erase {plugin}")).map(|r| r == "y"))
        }
        fn erase_all(&mut self) -> anyhow::Result<Option<bool>> {
            Ok(self.reply("erase all".to_string()).map(|r| r == "y"))
        }
    }

    /// The owner's case: Enter on the plugins too soon, Esc on the
    /// languages. The plugins come back as they were answered, not as the
    /// device has them; going forward again, so do the languages. The
    /// inventory is read once: the version did not change.
    ///
    /// **[MUTATION]**: preselect from the device again on the way back
    /// (drop `self.plugins.clone()` or `self.packs.clone()` in `ask_one`),
    /// or move forward on Esc — this test fails.
    #[test]
    fn going_back_shows_every_answer_as_it_was_given() {
        let fresh = dev(&[], None, &[], &[]);
        let mut wizard = Wizard::new(&fresh, Some(Action::InstallOrUpdate), true, None);
        let mut screens =
            Scripted::new(&["v2", "radio", "esc", "radio,cd", "fr", "esc", "fr", "esc", "esc", "radio,cd", "fr", ""]);
        let intent = wizard.ask(&mut screens).unwrap();
        assert_eq!(
            screens.shown,
            [
                "version -",
                "read v2",
                "plugins []",
                "packs []", // esc
                "plugins [radio]",
                "packs []",
                "grants declined []", // esc
                "packs [fr]",
                "grants declined []", // esc
                "packs [fr]",         // esc
                "plugins [cd,radio]",
                "packs [fr]",
                "grants declined []",
            ]
        );
        let Intent::InstallOrUpdate { plugins, packs, erase_data, reinstall, declined } = intent else {
            panic!("an install")
        };
        assert_eq!((plugins, packs), (set(&["radio", "cd"]), set(&["fr"])));
        assert!(erase_data.is_empty() && !reinstall && declined.is_empty());
    }

    /// Esc on the first question asks it again. A version changed on the
    /// way back is read again; the same version answered again is not.
    ///
    /// **[MUTATION]**: drop the `is_some_and` early return in
    /// `read_inventory` (a second "read v1"), or always read the source's
    /// own version (`wanted = None`) — this test fails.
    #[test]
    fn the_inventory_is_read_again_only_for_another_version() {
        let fresh = dev(&[], None, &[], &[]);
        let mut wizard = Wizard::new(&fresh, Some(Action::InstallOrUpdate), true, None);
        let mut screens = Scripted::new(&["esc", "v2", "esc", "v1", "esc", "v1", "radio", "", ""]);
        wizard.ask(&mut screens).unwrap();
        assert_eq!(
            screens.shown,
            [
                "version -", // esc: asked again
                "version -",
                "read v2",
                "plugins []", // esc
                "version v2",
                "read v1",
                "plugins []", // esc
                "version v1",
                "plugins []",
                "packs []",
                "grants declined []",
            ]
        );
    }

    /// Back to the action, a total removal: no version is asked, and its
    /// inventory is the source's own again, not the version chosen on the
    /// way — a total removal removes what the newest inventory knows.
    ///
    /// **[MUTATION]**: drop `self.action != Action::RemoveAll` from
    /// `version_asked` — this test fails.
    #[test]
    fn a_total_removal_reached_by_going_back_reads_the_sources_own_inventory() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &[], &[]);
        let mut wizard = Wizard::new(&device, None, true, None);
        let mut screens = Scripted::new(&["install", "v1", "esc", "esc", "remove", "esc", "remove", "y"]);
        let intent = wizard.ask(&mut screens).unwrap();
        assert_eq!(
            screens.shown,
            [
                "action InstallOrUpdate",
                "version -",
                "read v1",
                "plugins [radio]", // esc
                "version v1",      // esc
                "action InstallOrUpdate",
                "read own",
                "erase all", // esc
                "action RemoveAll",
                "erase all",
            ]
        );
        assert_eq!(intent, Intent::RemoveAll { erase_data: true });
    }

    /// Esc at the confirmation resumes at the last question — here the
    /// data of the plugin unchecked — and Esc there skips the permissions
    /// that `--grants` answered, landing on the languages. An answer changed
    /// on the way back is the one the intent carries.
    ///
    /// **[MUTATION]**: let `back_from_confirmation` restart at the first
    /// question, or show the permissions screen despite `--grants` — this
    /// test fails.
    #[test]
    fn back_from_the_confirmation_resumes_at_the_last_question_shown() {
        let device = dev(&[("radio", RADIO_EXEC)], None, &[], &["radio"]);
        let mut wizard = Wizard::new(&device, Some(Action::InstallOrUpdate), false, Some(BTreeSet::new()));
        let mut screens = Scripted::new(&["", "fr", "y"]);
        let Intent::InstallOrUpdate { erase_data, .. } = wizard.ask(&mut screens).unwrap() else { panic!() };
        assert_eq!(erase_data, set(&["radio"]));
        assert!(wizard.can_go_back());

        wizard.back_from_confirmation();
        screens.replies = vec!["esc", "fr", "n"];
        let Intent::InstallOrUpdate { plugins, packs, erase_data, .. } = wizard.ask(&mut screens).unwrap() else {
            panic!()
        };
        assert_eq!(
            screens.shown,
            [
                "read own",
                "plugins [radio]",
                "packs []",
                "erase radio",
                "erase radio", // esc
                "packs [fr]",
                "erase radio",
            ]
        );
        assert!(plugins.is_empty() && erase_data.is_empty(), "{plugins:?} {erase_data:?}");
        assert_eq!(packs, set(&["fr"]));
    }

    /// The action answered, a device without a CA bundle is refused at
    /// once, before the version; a total removal goes on.
    #[test]
    fn a_device_without_ca_certificates_is_refused_right_after_the_action() {
        let mut bare = dev(&[("radio", RADIO_EXEC)], None, &[], &[]);
        bare.ca_certificates = false;
        let mut screens = Scripted::new(&["install"]);
        assert!(Wizard::new(&bare, None, true, None).ask(&mut screens).is_err());
        assert_eq!(screens.shown, ["action InstallOrUpdate"]);
        let mut screens = Scripted::new(&["remove", "n"]);
        let intent = Wizard::new(&bare, None, true, None).ask(&mut screens).unwrap();
        assert_eq!(intent, Intent::RemoveAll { erase_data: false });
    }
}
