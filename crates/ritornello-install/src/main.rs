//! `ritornello-install`: installs, updates and removes Ritornello on a
//! device over ssh, from a workstation.
//!
//! The flow is spec §7's: the host, the read-only survey, the source and
//! its version, the inventory, what the operator wants (from the arguments,
//! or from the screens), the plan, the summary and its confirmation, the
//! sudo password when one is needed, the archives fetched and verified, the
//! bundle, the one invocation that applies it, and the report.
//!
//! Two rules shape it. Every question comes before `ssh::apply`: the
//! device's own output is relayed while it runs, and nothing is asked over
//! it. And when an answer is missing with no terminal to ask it on, the run
//! stops and names it, as early as it can be known, before anything is
//! downloaded or sent.

mod cli;
mod device;
mod inventory;
mod names;
mod plan;
mod registry;
mod script;
mod source;
mod ssh;
mod ui;

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::Parser;

use cli::{Args, Missing};
use device::{DeviceState, Sudo};
use inventory::Inventory;
use plan::{Intent, PlanError};
use source::Source;
use ui::Action;

/// **The debug-only seam the dry run uses**: the `ssh` client to run,
/// instead of `ssh`. The same shape as `source::TEST_RELEASES_URL_ENV`:
/// under `#[cfg(debug_assertions)]` the read is absent from a release
/// build, rather than merely inactive in one — this program runs as root on
/// a device whatever the client it is given does. Guarded by
/// `the_debug_only_ssh_seam_cannot_reach_a_release_build`.
#[cfg(debug_assertions)]
const TEST_SSH_PROGRAM_ENV: &str = "RITORNELLO_INSTALL_TEST_SSH_PROGRAM";

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ritornello-install: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The survey's per-run nonce (R28): 128 bits from the operating system's
/// own random source, hex-encoded. A nonce anyone could predict — the
/// time, the pid — would let a file on the device print a section marker
/// the parser takes for the survey's own.
fn nonce() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("reading the system's random source: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether a terminal is here to ask on: the answers are read from stdin,
/// and dialoguer draws its screens on stderr, so both must be one. With
/// either redirected (`2>log`), the run takes the non-interactive path and
/// stops on the `Missing` answer it needs, rather than on a prompt that
/// cannot be drawn.
fn is_terminal(stdin: bool, stderr: bool) -> bool {
    stdin && stderr
}

/// What is missing before anything connects, when no terminal is here: the
/// host, what to do, the confirmation. The sudo password is known missing
/// only once the survey says one is needed.
fn missing_before_connecting(args: &Args, terminal: bool) -> Option<Missing> {
    if terminal {
        return None;
    }
    if args.host.is_none() {
        Some(Missing::Host)
    } else if !args.names_an_intent() {
        Some(Missing::Intent)
    } else if !args.yes {
        Some(Missing::Confirmation)
    } else {
        None
    }
}

fn target(host: &str) -> ssh::Target {
    let target = ssh::Target::new(host);
    #[cfg(debug_assertions)]
    if let Ok(program) = std::env::var(TEST_SSH_PROGRAM_ENV) {
        return ssh::Target { ssh_program: program.into(), ..target };
    }
    target
}

/// What the survey alone refuses, before the inventory is even read.
fn check_device(dev: &DeviceState) -> anyhow::Result<()> {
    anyhow::ensure!(dev.kernel == "Linux", "this device runs {:?}, not Linux: Ritornello runs on Linux", dev.kernel);
    if !dev.systemd {
        bail!(PlanError::NoSystemd);
    }
    if dev.arch_label().is_none() {
        bail!(PlanError::UnknownArch(dev.machine.clone()));
    }
    Ok(())
}

/// What applying a plan needs of the device's `sudo`: refused only once
/// there is a plan to apply, so that a device already up to date is told
/// so even with no passwordless `sudo` and no terminal to ask a password
/// on — nothing will be run as root there. Still before the summary, the
/// confirmation and any archive download.
fn check_sudo(sudo: Sudo, terminal: bool) -> anyhow::Result<()> {
    // `Sudo::Absent` is refused here, with ssh's own sentence for it.
    ssh::remote_apply_command(sudo)?;
    if sudo == Sudo::Password && !terminal {
        bail!(Missing::SudoPassword);
    }
    Ok(())
}

/// The source, with its version: `--from-dir`, or a GitHub release — the
/// one `--version` names, the one the version screen picks when `ask`, or
/// the default.
fn open_source(args: &Args, ask: bool) -> anyhow::Result<Box<dyn Source>> {
    if let Some(dir) = &args.from_dir {
        anyhow::ensure!(
            args.version.is_none(),
            "--version names a release and --from-dir a local directory: give one or the other"
        );
        return Ok(Box::new(source::LocalDir::new(dir.clone())));
    }
    let mut github = source::GitHub::new(args.version.as_deref())?;
    if ask && args.version.is_none() {
        let default = github.tag().to_string();
        let tag = ui::ask_version(github.releases(), &default)?;
        github.select(&tag)?;
    }
    Ok(Box::new(github))
}

/// Steps 5 to 7, or the total removal's one question.
fn ask_intent(inv: &Inventory, dev: &DeviceState, action: Action) -> anyhow::Result<Intent> {
    if action == Action::RemoveAll {
        return Ok(Intent::RemoveAll { erase_data: ui::ask_erase_all()? });
    }
    let (installed_plugins, installed_packs) = plan::preselection(inv, dev);
    let plugins = ui::ask_plugins(&ui::plugin_choices(inv, dev), &installed_plugins)?;
    let packs = ui::ask_packs(&ui::pack_choices(inv, &installed_packs), &installed_packs)?;
    let candidates = ui::data_choices(&cli::removed_by(inv, dev, &plugins), dev);
    let erase_data = if candidates.is_empty() { BTreeSet::new() } else { ui::ask_erase(&candidates)? };
    Ok(Intent::InstallOrUpdate { plugins, packs, erase_data, reinstall: action == Action::Repair })
}

/// Asks for the sudo password and proves it, three tries at most. Every
/// try is a call of its own that sends the password line and nothing else.
fn sudo_password(target: &ssh::Target, control: &ssh::ControlDir, host: &str) -> anyhow::Result<String> {
    for _ in 0..3 {
        let password = ui::ask_sudo_password(host)?;
        if !ssh::valid_password(&password) {
            eprintln!("The sudo password cannot contain a line break.");
            continue;
        }
        match ssh::check_sudo_password(target, control, &password) {
            Ok(()) => return Ok(password),
            Err(e) => eprintln!("{e:#}"),
        }
    }
    bail!("no sudo password was accepted; nothing was sent to the device")
}

/// Where the web interface answers: the host without its account.
fn web_address(host: &str) -> String {
    let address = host.rsplit_once('@').map_or(host, |(_, a)| a);
    if address.contains(':') { format!("http://[{address}]:8080/") } else { format!("http://{address}:8080/") }
}

fn run(args: &Args) -> anyhow::Result<()> {
    let terminal = is_terminal(std::io::stdin().is_terminal(), std::io::stderr().is_terminal());
    cli::check_combination(args)?;
    if let Some(missing) = missing_before_connecting(args, terminal) {
        bail!(missing);
    }

    // 1. The host.
    let host = match &args.host {
        Some(host) => host.clone(),
        None => ui::ask_host()?,
    };
    anyhow::ensure!(ssh::valid_host(&host), "{host:?} is not a plain ssh host: write it as account@host");
    let target = target(&host);
    // One per run, shared by every call below, alive until after `apply`:
    // that shared master is what makes one authentication.
    let control = ssh::ControlDir::new(&target)?;

    // 2. The survey.
    eprintln!("Surveying {host}...");
    let nonce = nonce()?;
    let output = ssh::probe(&target, &control, &device::probe_script(&nonce))?;
    let dev = device::parse(&output, &nonce).context("reading the device survey")?;
    check_device(&dev)?;
    if dev.registry_ignored {
        eprintln!(
            "{} on {host} is not root's own (owner or mode): it is ignored, and everything is placed again",
            names::REGISTRY
        );
    }

    // 3. What to do, on a device that has Ritornello, when the arguments
    // do not say.
    // `--reinstall` has already said which: the repair.
    let interactive = terminal && !args.names_an_intent();
    let action = if args.reinstall {
        Action::Repair
    } else if interactive && ui::offers_remove_all(&dev) {
        ui::ask_action()?
    } else {
        Action::InstallOrUpdate
    };

    // 4 and 5. The source, its version, its inventory. A total removal
    // does not ask for a version: it removes what the registry and the
    // newest inventory know.
    let mut source = open_source(args, interactive && action != Action::RemoveAll)?;
    eprintln!("Reading the inventory of {}...", source.label());
    let inv = Inventory::parse(&source.inventory()?)?;

    // 6. What the operator wants.
    let intent = match cli::intent_from_args(args, &dev, &inv)? {
        Some(intent) => intent,
        None if terminal => ask_intent(&inv, &dev, action)?,
        None => bail!(Missing::Intent),
    };

    // 7. The plan: a refusal is said in one sentence, and nothing is sent.
    let plan = plan::compute(&inv, &dev, &intent).map_err(|e| anyhow!("{e} (nothing was sent to the device)"))?;

    // 8 to 11.
    let source_label = source.label();
    let outcome = {
        let mut live = Live {
            target: &target,
            control: &control,
            host: &host,
            sudo: dev.sudo,
            terminal,
            source: source.as_mut(),
            product: &inv.product,
            source_label: &source_label,
        };
        carry_out(&plan, args.yes, &mut live)?
    };
    // Ends the shared ssh connection now rather than at the end of `main`.
    // Off Unix there is no shared connection and `ControlDir` has no `Drop`,
    // so the call is a no-op there — kept, so the order reads the same on
    // every system.
    #[cfg_attr(not(unix), allow(clippy::drop_non_drop))]
    drop(control);

    // 12. The report.
    match outcome {
        Outcome::Declined => {}
        Outcome::UpToDate => println!("The web interface: {}", web_address(&host)),
        Outcome::Applied => {
            eprintln!("Done on {host}:");
            for line in ui::summary_lines(&plan) {
                eprintln!("{line}");
            }
            if plan.start_service {
                println!("The web interface: {}", web_address(&host));
            }
        }
    }
    Ok(())
}

/// How steps 8 to 11 ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The device already is what was asked: nothing was asked, fetched or
    /// sent.
    UpToDate,
    /// The operator said no at the confirmation.
    Declined,
    Applied,
}

/// What steps 8 to 11 do to the world, one method each, so that
/// `carry_out`'s order — and above all what it never reaches — can be
/// tested without a terminal, a network or a device.
trait Steps {
    fn show_summary(&mut self, plan: &plan::Plan);
    fn say_up_to_date(&mut self, plan: &plan::Plan);
    /// Whether the device can be asked to apply anything (`check_sudo`).
    fn check_can_apply(&mut self) -> anyhow::Result<()>;
    fn confirm(&mut self) -> anyhow::Result<bool>;
    fn sudo_password(&mut self) -> anyhow::Result<Option<String>>;
    fn archive(&mut self, name: &str) -> anyhow::Result<Vec<u8>>;
    fn apply(&mut self, bundle: &[u8], password: Option<&str>) -> anyhow::Result<()>;
}

/// Steps 8 to 11: the summary and its confirmation, the sudo password,
/// every archive, the bundle and the one invocation that applies it.
///
/// A plan with nothing to do stops first, before any of them: a device
/// already up to date is not asked for a password, sends no download
/// request, and its radio is not stopped for a run that would change
/// nothing.
fn carry_out(plan: &plan::Plan, yes: bool, steps: &mut impl Steps) -> anyhow::Result<Outcome> {
    if plan.nothing_to_do {
        steps.say_up_to_date(plan);
        return Ok(Outcome::UpToDate);
    }
    steps.check_can_apply()?;

    // 8. The summary, and its confirmation.
    steps.show_summary(plan);
    if !yes && !steps.confirm()? {
        eprintln!("Nothing was changed.");
        return Ok(Outcome::Declined);
    }

    // 9. The sudo password, the last question.
    let password = steps.sudo_password()?;

    // 10. Every archive, verified before it is read as one; then the script
    // and the bundle.
    let mut archives = BTreeMap::new();
    for name in &plan.archives {
        eprintln!("Fetching {name}...");
        archives.insert(name.clone(), steps.archive(name)?);
    }
    let script = script::render(plan)?;
    let bundle = script::bundle(plan, &script, &archives)?;

    // 11. Applied in one invocation, its output relayed as it arrives.
    steps.apply(&bundle, password.as_deref())?;
    Ok(Outcome::Applied)
}

/// The real steps: the terminal, the release, the device.
struct Live<'a> {
    target: &'a ssh::Target,
    control: &'a ssh::ControlDir,
    host: &'a str,
    sudo: Sudo,
    terminal: bool,
    source: &'a mut dyn Source,
    product: &'a str,
    source_label: &'a str,
}

impl Steps for Live<'_> {
    fn show_summary(&mut self, plan: &plan::Plan) {
        ui::show_summary(plan, self.product, self.source_label, self.host);
    }

    fn say_up_to_date(&mut self, plan: &plan::Plan) {
        eprintln!("{}", ui::up_to_date_line(self.host, self.product, self.source_label));
        for line in ui::summary_lines(plan) {
            eprintln!("{line}");
        }
    }

    fn check_can_apply(&mut self) -> anyhow::Result<()> {
        check_sudo(self.sudo, self.terminal)
    }

    fn confirm(&mut self) -> anyhow::Result<bool> {
        ui::confirm()
    }

    fn sudo_password(&mut self) -> anyhow::Result<Option<String>> {
        match self.sudo {
            Sudo::Password => Ok(Some(sudo_password(self.target, self.control, self.host)?)),
            Sudo::NotNeeded | Sudo::NoPassword | Sudo::Absent => Ok(None),
        }
    }

    fn archive(&mut self, name: &str) -> anyhow::Result<Vec<u8>> {
        self.source.archive(name)
    }

    fn apply(&mut self, bundle: &[u8], password: Option<&str>) -> anyhow::Result<()> {
        eprintln!("Applying on {}...", self.host);
        ssh::apply(self.target, self.control, self.sudo, password, bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonce_is_32_hex_digits_and_never_twice_the_same() {
        let a = nonce().unwrap();
        let b = nonce().unwrap();
        assert_eq!(a.len(), 32, "{a}");
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{a}");
        assert_ne!(a, b);
    }

    /// Unpredictable, not merely different: two nonces drawn one after the
    /// other agree, digit for digit, about as often as chance says (2 of 32
    /// on average; 16 or more has odds below 1e-10). A nonce built from the
    /// time and the pid shares the pid and the clock's high digits.
    ///
    /// **[MUTATION]**: build the nonce from `SystemTime` and
    /// `std::process::id()` — this test fails.
    #[test]
    fn two_nonces_in_a_row_share_no_more_than_chance() {
        let a = nonce().unwrap();
        let b = nonce().unwrap();
        let same = a.bytes().zip(b.bytes()).filter(|(x, y)| x == y).count();
        assert!(same < 16, "{a} and {b} agree on {same} of 32 digits");
    }

    fn args(argv: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("ritornello-install").chain(argv.iter().copied())).unwrap()
    }

    /// No terminal: each missing answer is named before anything
    /// connects, host first.
    #[test]
    fn without_a_terminal_the_first_missing_answer_is_named_before_connecting() {
        // `RITORNELLO_HOST`, when set, fills `--host` from the environment.
        if std::env::var_os("RITORNELLO_HOST").is_none() {
            assert_eq!(missing_before_connecting(&args(&["--keep", "--yes"]), false), Some(Missing::Host));
        }
        let host = ["--host", "pi@device"];
        assert_eq!(missing_before_connecting(&args(&[host[0], host[1], "--yes"]), false), Some(Missing::Intent));
        assert_eq!(missing_before_connecting(&args(&[host[0], host[1], "--keep"]), false), Some(Missing::Confirmation));
        assert_eq!(missing_before_connecting(&args(&[host[0], host[1], "--keep", "--yes"]), false), None);
        assert_eq!(missing_before_connecting(&args(&[]), true), None, "a terminal asks instead");
    }

    #[test]
    fn a_terminal_needs_both_stdin_and_stderr() {
        assert!(is_terminal(true, true));
        assert!(!is_terminal(true, false), "stderr redirected: dialoguer cannot draw");
        assert!(!is_terminal(false, true), "stdin redirected: nothing to read an answer from");
        assert!(!is_terminal(false, false));
    }

    /// Records every step `carry_out` reaches, and answers each one.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<String>,
        /// What `check_can_apply` answers: `check_sudo` for this device.
        sudo: Option<(Sudo, bool)>,
    }

    impl Steps for Recorder {
        fn show_summary(&mut self, _: &plan::Plan) {
            self.calls.push("summary".into());
        }
        fn say_up_to_date(&mut self, _: &plan::Plan) {
            self.calls.push("up to date".into());
        }
        fn check_can_apply(&mut self) -> anyhow::Result<()> {
            self.calls.push("check".into());
            match self.sudo {
                Some((sudo, terminal)) => check_sudo(sudo, terminal),
                None => Ok(()),
            }
        }
        fn confirm(&mut self) -> anyhow::Result<bool> {
            self.calls.push("confirm".into());
            Ok(true)
        }
        fn sudo_password(&mut self) -> anyhow::Result<Option<String>> {
            self.calls.push("sudo".into());
            Ok(None)
        }
        fn archive(&mut self, name: &str) -> anyhow::Result<Vec<u8>> {
            self.calls.push(format!("fetch {name}"));
            Ok(Vec::new())
        }
        fn apply(&mut self, _: &[u8], _: Option<&str>) -> anyhow::Result<()> {
            self.calls.push("apply".into());
            Ok(())
        }
    }

    /// The owner's case, end to end from the plan: a device already up to
    /// date is told so, and nothing else happens — no confirmation, no sudo
    /// password, no download, no apply (so no stop of the service either).
    /// The same device with one plugin moved goes the whole way, fetching
    /// that one archive only.
    ///
    /// **[MUTATION]**: drop the `nothing_to_do` return from `carry_out` —
    /// this test fails (the summary, the confirmation, the sudo password and
    /// the apply are reached).
    #[test]
    fn an_up_to_date_device_is_told_so_before_any_question_download_or_apply() {
        use crate::plan::tests::{RADIO_EXEC, THEIRS_EXEC, dev, inv};
        use crate::registry::{Recorded, Registry};
        let rec = |v: &str, p: &[&str]| Recorded { version: v.into(), privileged: p.iter().map(|s| s.to_string()).collect() };
        let registry = Registry {
            format: 1,
            components: [
                ("core".to_string(), rec("0.2.0-beta.2", &["/etc/systemd/system/ritornello.service"])),
                ("radio".to_string(), rec("0.2.0-beta.2", &[])),
            ]
            .into_iter()
            .collect(),
        };
        let device = dev(&[("radio", RADIO_EXEC), ("theirs", THEIRS_EXEC)], Some(registry), &[], &[]);
        let (plugins, packs) = plan::preselection(&inv(), &device);
        let keep = Intent::InstallOrUpdate { plugins, packs, erase_data: BTreeSet::new(), reinstall: false };

        let plan = plan::compute(&inv(), &device, &keep).unwrap();
        let mut steps = Recorder::default();
        assert_eq!(carry_out(&plan, false, &mut steps).unwrap(), Outcome::UpToDate);
        assert_eq!(steps.calls, ["up to date"]);

        let mut moved = device.clone();
        moved.registry.as_mut().unwrap().components.get_mut("radio").unwrap().version = "0.2.0-beta.1".into();
        let plan = plan::compute(&inv(), &moved, &keep).unwrap();
        let mut steps = Recorder::default();
        assert_eq!(carry_out(&plan, false, &mut steps).unwrap(), Outcome::Applied);
        assert_eq!(
            steps.calls,
            ["check", "summary", "confirm", "sudo", "fetch ritornello-plugin-radio-0.2.0-beta.2-arm64.tar.gz", "apply"]
        );
    }

    /// A device whose `sudo` asks for a password, with no terminal to ask
    /// it on (`--keep --yes` from a script): already up to date, it is told
    /// so — nothing would run as root. With something to do, the missing
    /// password stops the run before the summary, any download and the
    /// apply; so does a device with no `sudo` at all.
    ///
    /// **[MUTATION]**: call `check_can_apply` before the `nothing_to_do`
    /// return in `carry_out` (the old order) — this test fails.
    #[test]
    fn a_missing_sudo_password_only_matters_when_there_is_something_to_apply() {
        use crate::plan::tests as p;
        let up_to_date = p::keep(&p::current_device());
        assert!(up_to_date.nothing_to_do);
        let mut steps = Recorder { sudo: Some((Sudo::Password, false)), ..Recorder::default() };
        assert_eq!(carry_out(&up_to_date, true, &mut steps).unwrap(), Outcome::UpToDate);
        assert_eq!(steps.calls, ["up to date"]);

        let mut behind = p::current_device();
        behind.registry.as_mut().unwrap().components.get_mut("radio").unwrap().version = "0.2.0-beta.1".into();
        let plan = p::keep(&behind);
        for sudo in [Sudo::Password, Sudo::Absent] {
            let mut steps = Recorder { sudo: Some((sudo, false)), ..Recorder::default() };
            let err = carry_out(&plan, true, &mut steps).unwrap_err();
            assert_eq!(steps.calls, ["check"], "{sudo:?}: {err}");
        }
        let err = check_sudo(Sudo::Password, false).unwrap_err();
        assert_eq!(err.to_string(), Missing::SudoPassword.to_string());
        // A terminal asks the password later; root or a passwordless sudo
        // needs none.
        assert!(check_sudo(Sudo::Password, true).is_ok());
        assert!(check_sudo(Sudo::NoPassword, false).is_ok() && check_sudo(Sudo::NotNeeded, false).is_ok());
    }

    /// The up-to-date line names the device, the release and its source,
    /// and says that nothing was changed.
    #[test]
    fn the_up_to_date_line_says_nothing_was_changed() {
        let line = ui::up_to_date_line("dietpi@radio", "0.3.0", "GitHub release v0.3.0");
        assert_eq!(line, "dietpi@radio is already up to date with Ritornello 0.3.0 from GitHub release v0.3.0: nothing was changed.");
    }

    #[test]
    fn the_web_address_drops_the_account() {
        assert_eq!(web_address("dietpi@192.168.0.57"), "http://192.168.0.57:8080/");
        assert_eq!(web_address("radio.local"), "http://radio.local:8080/");
        assert_eq!(web_address("pi@fe80::1"), "http://[fe80::1]:8080/");
    }

    /// The same guard as `source.rs`'s for its own seam: the line that reads
    /// the fake ssh client's path must sit right under
    /// `#[cfg(debug_assertions)]`, or a release build would run whatever
    /// that variable names in place of `ssh`. Built from pieces so this
    /// assertion is not one of its own matches.
    #[test]
    fn the_debug_only_ssh_seam_cannot_reach_a_release_build() {
        let here = include_str!("main.rs");
        let lines: Vec<&str> = here.lines().collect();
        let read_pattern = ["std::env::var(", "TEST_SSH_PROGRAM_ENV", ")"].concat();
        let read_line = lines
            .iter()
            .position(|l| l.contains(&read_pattern))
            .expect("the seam's own read must still exist for this guard to mean anything");
        let guard_pattern = ["#[cfg(", "debug_assertions", ")]"].concat();
        let guarded = lines[..read_line].iter().rev().take(1).any(|l| l.contains(&guard_pattern));
        assert!(guarded, "the line reading {read_pattern:?} must be immediately preceded by {guard_pattern:?}");
        let declared = lines.iter().position(|l| l.contains("const TEST_SSH_PROGRAM_ENV")).expect("declared");
        assert!(lines[declared - 1].contains(&guard_pattern), "the constant itself is debug-only");
    }
}
