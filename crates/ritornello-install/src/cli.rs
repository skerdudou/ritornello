//! The command line: the arguments, what they say the operator wants, and
//! the answers a run stops on when no terminal is there to ask them.
//!
//! The arguments describe the **state wanted**, not a sequence of actions
//! (spec §7). Two rules keep a command from ever reading two ways: an
//! argument left out means "keep what is installed", never "remove it";
//! and `--keep` on a device with nothing installed is an error, not an
//! install of the core alone (`--plugins none` says that).

use std::collections::BTreeSet;

use crate::device::DeviceState;
use crate::inventory::Inventory;
use crate::plan::{self, Intent};

#[derive(Debug, clap::Parser)]
#[command(
    name = "ritornello-install",
    disable_version_flag = true,
    // The installer's own number, first in `-h` and `--help`. `--version` is
    // not the way to read it: that flag selects the product release.
    before_help = concat!("ritornello-install ", env!("CARGO_PKG_VERSION")),
    about = "Installs, updates and removes Ritornello on a device over ssh.",
    long_about = "Installs, updates and removes Ritornello on a device over ssh.\n\n\
                  Without arguments, and in a terminal, it asks. With arguments, they describe the \
                  state wanted: whatever is left out is kept as it is on the device."
)]
pub struct Args {
    /// The device, as account@host. Asked when absent and a terminal is here.
    #[arg(long, env = "RITORNELLO_HOST")]
    pub host: Option<String>,
    /// The plugins wanted, comma-separated; `none` for the core alone.
    #[arg(long, value_delimiter = ',')]
    pub plugins: Option<Vec<String>>,
    /// The languages wanted, comma-separated; `none` for no pack.
    #[arg(long, value_delimiter = ',')]
    pub packs: Option<Vec<String>>,
    /// Keep exactly what is installed: a plain update.
    #[arg(long)]
    pub keep: bool,
    /// Remove everything Ritornello placed.
    #[arg(long)]
    pub remove_all: bool,
    /// Also erase the data of what is removed.
    #[arg(long)]
    pub purge_data: bool,
    /// Place every component and language pack again, even those already
    /// at the offered version: the repair of a unit or a rule changed by
    /// hand. A downgrade needs it only when the registry already records
    /// the older version while the device runs another (the in-app updater
    /// moved past it) and the updater's memory does not say so; otherwise
    /// the versions differ and the component is placed anyway.
    #[arg(long, overrides_with = "reinstall")]
    pub reinstall: bool,
    /// The release to install (a tag); by default the newest final release,
    /// or the newest prerelease when none is final yet.
    #[arg(long)]
    pub version: Option<String>,
    /// Install from a directory of local archives instead of a release.
    #[arg(long)]
    pub from_dir: Option<std::path::PathBuf>,
    /// Do not ask for confirmation.
    #[arg(long)]
    pub yes: bool,
}

impl Args {
    /// Whether any argument says what to do. Known before the inventory is
    /// read, which is what lets the "what to do" screen come before the
    /// version one (spec §7, steps 3 and 4).
    pub fn names_an_intent(&self) -> bool {
        self.keep || self.remove_all || self.plugins.is_some() || self.packs.is_some()
    }
}

/// Arguments that contradict each other, or ask for something that does not
/// exist on this device. Each says which arguments, and what to write
/// instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// `--keep` on a device with no Ritornello.
    NothingToKeep,
    /// `--packs` without `--plugins` on a device with no Ritornello (R51).
    NoPluginsToKeep,
    /// `--keep` with `--plugins` or `--packs`.
    KeepOrChoose,
    /// `--remove-all` with `--keep`, `--plugins` or `--packs`.
    RemoveAllAlone,
    /// `--reinstall` with `--remove-all`.
    ReinstallWithRemoveAll,
    /// `--purge-data` with `--keep`, which chooses nothing to remove.
    PurgeWithKeep,
    /// `--purge-data` with nothing that says what is removed.
    PurgeWithoutIntent,
    /// `none` beside a name in the same list (the flag's name).
    NoneWithOthers(&'static str),
    /// An empty item in the list (the flag's name).
    EmptyItem(&'static str),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToKeep => write!(
                f,
                "--keep: this device has no Ritornello, so there is nothing to keep; \
                 name the plugins with --plugins (--plugins none for the core alone)"
            ),
            Self::NoPluginsToKeep => write!(
                f,
                "--packs without --plugins keeps the installed plugins, and this device has none: \
                 nothing installed to keep; say --plugins none for the core alone, or name the plugins"
            ),
            Self::KeepOrChoose => write!(
                f,
                "--keep keeps exactly what is installed, --plugins and --packs choose: give one or the other"
            ),
            Self::RemoveAllAlone => {
                write!(f, "--remove-all removes everything: it cannot be given with --keep, --plugins or --packs")
            }
            Self::ReinstallWithRemoveAll => write!(
                f,
                "--reinstall places everything again and --remove-all removes everything: give one or the other"
            ),
            Self::PurgeWithKeep => {
                write!(f, "--keep keeps what is declared: to erase a plugin's data, name what stays with --plugins")
            }
            Self::PurgeWithoutIntent => write!(
                f,
                "--purge-data erases the data of what is removed: say what with --plugins, --packs or --remove-all"
            ),
            Self::NoneWithOthers(flag) => {
                write!(f, "{flag}: `none` means an empty list, and cannot stand beside a name")
            }
            Self::EmptyItem(flag) => {
                write!(f, "{flag}: an empty name in the list; write `none` for an empty list")
            }
        }
    }
}

impl std::error::Error for ArgError {}

/// An answer the run needs, with no terminal to ask it on. The program
/// stops and names it; it never guesses (spec §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    Host,
    Intent,
    Confirmation,
    SudoPassword,
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host => write!(f, "missing: --host (or RITORNELLO_HOST)"),
            Self::Intent => write!(f, "missing: what to do, as --plugins, --packs, --keep or --remove-all"),
            Self::Confirmation => write!(f, "missing: --yes, since there is no terminal to confirm on"),
            // Never a password in an argument or a variable: it would sit in
            // the shell's history, or in the environment of every child.
            Self::SudoPassword => write!(f, "missing: a passwordless sudo, or connect as root"),
        }
    }
}

impl std::error::Error for Missing {}

/// One comma-separated list: `none` alone is the empty set.
fn list(items: &[String], flag: &'static str) -> Result<BTreeSet<String>, ArgError> {
    if items.iter().any(String::is_empty) {
        return Err(ArgError::EmptyItem(flag));
    }
    if items.iter().any(|i| i == "none") {
        return if items.len() == 1 { Ok(BTreeSet::new()) } else { Err(ArgError::NoneWithOthers(flag)) };
    }
    Ok(items.iter().cloned().collect())
}

/// The plugins that would go if `wanted` were chosen — everything the
/// device declares, and every plugin of ours only its registry records
/// (the plan removes those too, R30), or whose companion it records —
/// minus `wanted`. Only these may have their data erased
/// (`PlanError::EraseNotRemoved` otherwise).
pub fn removed_by(inv: &Inventory, dev: &DeviceState, wanted: &BTreeSet<String>) -> BTreeSet<String> {
    let declared = dev.declared.iter().map(|d| d.name.clone());
    let recorded = dev.registry.iter().flat_map(|r| r.components.keys()).filter_map(|n| {
        if inv.plugin(n).is_some() {
            Some(n.clone())
        } else {
            inv.companions.iter().find(|c| &c.name == n).map(|c| c.with.clone())
        }
    });
    declared.chain(recorded).filter(|n| !wanted.contains(n)).collect()
}

/// Every contradiction the arguments hold on their own, whatever the
/// device: checked before anything connects, and again by
/// `intent_from_args`.
pub fn check_combination(a: &Args) -> Result<(), ArgError> {
    let chooses = a.plugins.is_some() || a.packs.is_some();
    if a.remove_all && (a.keep || chooses) {
        return Err(ArgError::RemoveAllAlone);
    }
    if a.remove_all && a.reinstall {
        return Err(ArgError::ReinstallWithRemoveAll);
    }
    if a.keep && chooses {
        return Err(ArgError::KeepOrChoose);
    }
    if a.keep && a.purge_data {
        return Err(ArgError::PurgeWithKeep);
    }
    if a.purge_data && !a.remove_all && !a.keep && !chooses {
        return Err(ArgError::PurgeWithoutIntent);
    }
    if let Some(items) = &a.plugins {
        list(items, "--plugins")?;
    }
    if let Some(items) = &a.packs {
        list(items, "--packs")?;
    }
    Ok(())
}

/// What the arguments say the operator wants, or `None` when they say
/// nothing about it (a terminal will then ask).
pub fn intent_from_args(a: &Args, dev: &DeviceState, inv: &Inventory) -> Result<Option<Intent>, ArgError> {
    check_combination(a)?;
    if a.remove_all {
        return Ok(Some(Intent::RemoveAll { erase_data: a.purge_data }));
    }
    if a.keep {
        if dev.is_fresh() {
            return Err(ArgError::NothingToKeep);
        }
        let (plugins, packs) = plan::preselection(inv, dev);
        return Ok(Some(Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: a.reinstall }));
    }
    if a.plugins.is_none() && a.packs.is_none() {
        return Ok(None);
    }
    // R51, §7's `--keep` rule applied to a left-out `--plugins`: on a fresh
    // device there are no plugins to keep, and the core alone is only ever
    // reached by saying `--plugins none`.
    if a.plugins.is_none() && dev.is_fresh() {
        return Err(ArgError::NoPluginsToKeep);
    }
    // An argument left out keeps what is installed: its absence never
    // means "remove it all".
    let (installed_plugins, installed_packs) = plan::preselection(inv, dev);
    let plugins = match &a.plugins {
        Some(items) => list(items, "--plugins")?,
        None => installed_plugins,
    };
    let packs = match &a.packs {
        Some(items) => list(items, "--packs")?,
        None => installed_packs,
    };
    let erase_data = if a.purge_data {
        removed_by(inv, dev, &plugins).into_iter().filter(|n| dev.data_nonempty.contains(n)).collect()
    } else {
        BTreeSet::new()
    };
    Ok(Some(Intent::InstallOrUpdate { plugins, packs, erase_data, reinstall: a.reinstall }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::tests::{CD_EXEC, RADIO_EXEC, THEIRS_EXEC, dev, inv, registry, set};
    use clap::Parser;

    fn args(argv: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("ritornello-install").chain(argv.iter().copied()))
            .expect("the arguments parse")
    }

    /// radio and cd of ours, a third party's, French installed, and data
    /// for cd and the third party.
    fn installed() -> DeviceState {
        dev(
            &[("radio", RADIO_EXEC), ("cd", CD_EXEC), ("theirs", THEIRS_EXEC)],
            None,
            &["ritornello-lang-fr"],
            &["cd", "theirs", "radio"],
        )
    }

    fn fresh() -> DeviceState {
        dev(&[], None, &[], &[])
    }

    fn intent(argv: &[&str], device: &DeviceState) -> Result<Option<Intent>, ArgError> {
        intent_from_args(&args(argv), device, &inv())
    }

    fn install(plugins: &[&str], packs: &[&str], erase: &[&str]) -> Option<Intent> {
        Some(Intent::InstallOrUpdate { plugins: set(plugins), packs: set(packs), erase_data: set(erase), reinstall: false })
    }

    #[test]
    fn remove_all_carries_purge_data() {
        assert_eq!(intent(&["--remove-all"], &installed()), Ok(Some(Intent::RemoveAll { erase_data: false })));
        assert_eq!(
            intent(&["--remove-all", "--purge-data"], &installed()),
            Ok(Some(Intent::RemoveAll { erase_data: true }))
        );
    }

    #[test]
    fn keep_on_an_installed_device_is_the_preselection() {
        let device = installed();
        let (plugins, packs) = plan::preselection(&inv(), &device);
        assert_eq!(
            intent(&["--keep"], &device),
            Ok(Some(Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false }))
        );
        assert_eq!(intent(&["--keep"], &device), Ok(install(&["cd", "radio", "theirs"], &["fr"], &[])));
    }

    /// **[MUTATION]**: drop the `dev.is_fresh()` check from `--keep` — this
    /// test fails, since `--keep` would then install the core alone.
    #[test]
    fn keep_on_a_fresh_device_is_an_error_not_the_core_alone() {
        let err = intent(&["--keep"], &fresh()).unwrap_err();
        assert_eq!(err, ArgError::NothingToKeep);
        assert!(err.to_string().contains("--plugins none"), "{err}");
    }

    /// The installer says which one it is in both forms of its help, and
    /// `--version` stays what it always was: the product release to install,
    /// never this program's own number.
    #[test]
    fn the_help_names_the_installers_own_number_and_version_still_selects_a_release() {
        use clap::CommandFactory;
        let own = crate::own_version::OWN_VERSION;
        let line = format!("ritornello-install {own}");
        let mut command = Args::command();
        for help in [command.render_help().to_string(), command.render_long_help().to_string()] {
            assert!(help.starts_with(&line), "the help must open with {line:?}: {help}");
        }
        use clap::Parser;
        let args = Args::try_parse_from(["ritornello-install", "--version", "v0.2.0"]).unwrap();
        assert_eq!(args.version.as_deref(), Some("v0.2.0"));
    }

    #[test]
    fn plugins_none_is_the_empty_set() {
        assert_eq!(intent(&["--plugins", "none"], &fresh()), Ok(install(&[], &[], &[])));
        assert_eq!(intent(&["--plugins", "none", "--packs", "none"], &installed()), Ok(install(&[], &[], &[])));
    }

    /// Only what goes, and only what has data: `radio` stays, `cd` and the
    /// third party's go with their data.
    #[test]
    fn purge_data_erases_the_data_of_the_removed_plugins_that_have_some() {
        assert_eq!(
            intent(&["--plugins", "radio", "--purge-data"], &installed()),
            Ok(install(&["radio"], &["fr"], &["cd", "theirs"]))
        );
        assert_eq!(intent(&["--plugins", "radio"], &installed()), Ok(install(&["radio"], &["fr"], &[])));
        // cd goes, but has no data: there is nothing of it to erase.
        let device = dev(&[("radio", RADIO_EXEC), ("cd", CD_EXEC)], None, &[], &["radio"]);
        assert_eq!(intent(&["--plugins", "radio", "--purge-data"], &device), Ok(install(&["radio"], &[], &[])));
    }

    /// A plugin of ours only the registry records goes too (R30): its data
    /// may go with it.
    #[test]
    fn purge_data_reaches_a_plugin_only_the_registry_records() {
        let device = dev(&[("radio", RADIO_EXEC)], Some(registry(&[("cd", &[])])), &[], &["cd"]);
        assert_eq!(intent(&["--plugins", "radio", "--purge-data"], &device), Ok(install(&["radio"], &[], &["cd"])));
    }

    /// The same for a plugin only its companion's record remembers: `files`
    /// records nothing itself, `files-mount` does, and the plan removes
    /// `files` for it — so its data may go too.
    ///
    /// **[MUTATION]**: make `removed_by` ignore a recorded companion (the
    /// `else` branch answering `None`) — this test fails.
    #[test]
    fn purge_data_reaches_a_plugin_only_its_companion_s_record_remembers() {
        let device = dev(&[("radio", RADIO_EXEC)], Some(registry(&[("files-mount", &[])])), &[], &["files"]);
        assert_eq!(
            intent(&["--plugins", "radio", "--purge-data"], &device),
            Ok(install(&["radio"], &[], &["files"]))
        );
        let intent = intent(&["--plugins", "radio", "--purge-data"], &device).unwrap().unwrap();
        assert!(plan::compute(&inv(), &device, &intent).is_ok(), "the plan agrees that files goes");
    }

    /// `--reinstall` rides on whatever says what is wanted, and is refused
    /// beside a total removal, which places nothing.
    ///
    /// **[MUTATION]**: build either install intent with `reinstall: false`
    /// — this test fails.
    #[test]
    fn reinstall_combines_with_keep_and_plugins_but_not_with_remove_all() {
        let reinstalls = |argv: &[&str]| match intent(argv, &installed()) {
            Ok(Some(Intent::InstallOrUpdate { reinstall, .. })) => reinstall,
            other => panic!("{argv:?}: {other:?}"),
        };
        assert!(reinstalls(&["--keep", "--reinstall"]));
        assert!(reinstalls(&["--plugins", "radio", "--reinstall", "--version", "v0.2.0"]));
        assert!(reinstalls(&["--packs", "fr", "--reinstall"]));
        assert!(!reinstalls(&["--keep"]));
        assert!(!reinstalls(&["--plugins", "radio"]));
        assert_eq!(intent(&["--remove-all", "--reinstall"], &installed()), Err(ArgError::ReinstallWithRemoveAll));
        assert!(ArgError::ReinstallWithRemoveAll.to_string().contains("give one or the other"));
        // Alone, it does not say what is wanted: the screens ask.
        assert_eq!(intent(&["--reinstall"], &installed()), Ok(None));
    }

    /// A repeated `--reinstall` (the operator's, beside the one `deploy.sh`
    /// adds) is the same request, not an error.
    #[test]
    fn a_repeated_reinstall_is_harmless() {
        assert!(args(&["--keep", "--reinstall", "--reinstall"]).reinstall);
    }

    /// `deploy/deploy.sh` adds `--reinstall` unless its arguments already
    /// say `--reinstall` or `--remove-all`: its own `--self-test` runs that
    /// rule on its cases, with nothing built.
    ///
    /// **[MUTATION]**: drop `--remove-all` from `needs_reinstall`'s `case`
    /// — this test fails.
    #[test]
    #[cfg(unix)]
    fn deploy_sh_adds_reinstall_unless_told_otherwise() {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/deploy.sh");
        let out = std::process::Command::new("bash").arg(&script).arg("--self-test").output().expect("bash runs");
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("self-test passed"));
    }

    #[test]
    fn keep_and_a_choice_are_one_or_the_other() {
        assert_eq!(intent(&["--keep", "--plugins", "radio"], &installed()), Err(ArgError::KeepOrChoose));
        assert_eq!(intent(&["--keep", "--packs", "fr"], &installed()), Err(ArgError::KeepOrChoose));
    }

    #[test]
    fn remove_all_stands_alone() {
        for argv in [
            &["--remove-all", "--plugins", "radio"][..],
            &["--remove-all", "--packs", "fr"],
            &["--remove-all", "--keep"],
        ] {
            assert_eq!(intent(argv, &installed()), Err(ArgError::RemoveAllAlone), "{argv:?}");
        }
    }

    #[test]
    fn no_intent_argument_leaves_the_question_to_the_terminal() {
        assert_eq!(intent(&[], &installed()), Ok(None));
        assert_eq!(intent(&["--yes", "--version", "v0.2.0"], &fresh()), Ok(None));
    }

    /// **[MUTATION]**: read an absent `--packs` as `none` — this test fails,
    /// since French would then be removed.
    #[test]
    fn an_absent_packs_keeps_the_installed_packs() {
        assert_eq!(intent(&["--plugins", "radio,cd,theirs"], &installed()), Ok(install(&["cd", "radio", "theirs"], &["fr"], &[])));
    }

    #[test]
    fn an_absent_plugins_keeps_the_installed_plugins() {
        assert_eq!(intent(&["--packs", "none"], &installed()), Ok(install(&["cd", "radio", "theirs"], &[], &[])));
    }

    /// R51: on a fresh device a left-out `--plugins` has nothing to keep,
    /// exactly as `--keep` has not; the core alone is `--plugins none`.
    ///
    /// **[MUTATION]**: drop the `dev.is_fresh()` check on a left-out
    /// `--plugins` — this test fails, since `--packs fr` would then install
    /// the core alone with French.
    #[test]
    fn packs_without_plugins_on_a_fresh_device_is_an_error() {
        let err = intent(&["--packs", "fr"], &fresh()).unwrap_err();
        assert_eq!(err, ArgError::NoPluginsToKeep);
        let m = err.to_string();
        assert!(m.contains("nothing installed to keep") && m.contains("--plugins none"), "{m}");
        assert_eq!(intent(&["--plugins", "none", "--packs", "fr"], &fresh()), Ok(install(&[], &["fr"], &[])));
    }

    #[test]
    fn purge_data_needs_something_removed_to_mean_anything() {
        assert_eq!(intent(&["--purge-data"], &installed()), Err(ArgError::PurgeWithoutIntent));
        assert_eq!(intent(&["--keep", "--purge-data"], &installed()), Err(ArgError::PurgeWithKeep));
    }

    #[test]
    fn none_stands_alone_and_no_item_is_empty() {
        assert_eq!(intent(&["--plugins", "none,radio"], &installed()), Err(ArgError::NoneWithOthers("--plugins")));
        assert_eq!(intent(&["--packs", "fr,"], &installed()), Err(ArgError::EmptyItem("--packs")));
    }

    #[test]
    fn a_comma_separated_list_is_split() {
        assert_eq!(intent(&["--plugins", "radio,cd", "--packs", "fr,de"], &fresh()), Ok(install(&["cd", "radio"], &["de", "fr"], &[])));
    }

    /// Every missing answer names the argument that would give it; the sudo
    /// password names the two ways around one, since none is ever taken
    /// from an argument.
    #[test]
    fn each_missing_answer_is_named() {
        assert_eq!(Missing::Host.to_string(), "missing: --host (or RITORNELLO_HOST)");
        assert!(Missing::Intent.to_string().contains("--plugins"));
        assert!(Missing::Confirmation.to_string().contains("--yes"));
        assert_eq!(Missing::SudoPassword.to_string(), "missing: a passwordless sudo, or connect as root");
    }

    #[test]
    fn the_command_line_is_well_formed() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }
}
