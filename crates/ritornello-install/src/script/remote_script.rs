//! The rendered script, run for real: under `/bin/dash` when it exists (the
//! device's own `/bin/sh`), against a temporary root, with shims standing
//! in for every command that needs root or a real system (`systemctl`,
//! `chown`, `useradd`, `userdel`, `mountpoint`, `umount`, `id`). Every
//! other command — `tar`, `install`, `mv`, `rm`, `rmdir`, `mkdir` — is the
//! real one, and every archive is a real `.tar.gz`.
//!
//! These live beside the module rather than under `tests/`: the crate is a
//! binary with no library target, so an integration test could not reach
//! `render` at all.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{bundle, render, sh_quote};
use crate::plan::{Initial, Plan, Put};
use crate::registry::{Recorded, Registry};

const CORE: &str = "ritornello-core-0.2.0-beta.2-armv7.tar.gz";
const RADIO: &str = "ritornello-plugin-radio-0.2.0-beta.2-armv7.tar.gz";
const INPUT: &str = "ritornello-plugin-generic-input-0.2.0-beta.2-armv7.tar.gz";
const FR: &str = "ritornello-lang-fr-0.2.0-beta.2.tar.gz";

const CORE_FILES: &[(&str, &str)] = &[
    ("/usr/local/bin/ritornello-core", "0755"),
    ("/etc/systemd/system/ritornello.service", "0644"),
    ("/etc/systemd/system/ritornello-update.service", "0644"),
    ("/etc/systemd/system/ritornello-rollback.service", "0644"),
    ("/etc/polkit-1/rules.d/50-ritornello-power.rules", "0644"),
    ("/etc/polkit-1/rules.d/52-ritornello-update.rules", "0644"),
    ("/usr/local/lib/ritornello/ritornello-update", "0755"),
];

/// The shell the script runs under, announced once per run.
fn shell() -> &'static str {
    if Path::new("/bin/dash").exists() { "/bin/dash" } else { "sh" }
}

/// Each shim logs `<name> <args>` to `$SHIM_LOG`, then does what its body
/// says. `systemctl stop` may plant a symbolic link, to stand for something
/// placed on the device after the verify phase has passed; `systemctl
/// is-active` answers "inactive" unless `SHIM_ACTIVE` is set.
const SHIMS: &[(&str, &str)] = &[
    (
        "systemctl",
        r#"if [ "${1:-}" = stop ] && [ -n "${SHIM_PLANT_LINK:-}" ]; then ln -s "$SHIM_PLANT_TARGET" "$SHIM_PLANT_LINK"; fi
if [ "${1:-}" = is-active ]; then if [ -n "${SHIM_ACTIVE:-}" ]; then exit 0; fi; exit 3; fi"#,
    ),
    ("useradd", ""),
    ("userdel", ""),
    (
        "mountpoint",
        r#"[ "$1" = -q ] || exit 2
t=$2
case $t in /*) ;; *) t=$(cd -P -- "$(dirname "$t")" && pwd -P)/$(basename "$t") ;; esac
# A glob on the resolved path: only the probes it matches hang.
if [ -n "${SHIM_MOUNTPOINT_HANG:-}" ]; then case $t in $SHIM_MOUNTPOINT_HANG) exec sleep 30 ;; esac; fi
if [ -n "${SHIM_LATE_FLAG:-}" ] && [ -L "$SHIM_LATE_FLAG" ]; then SHIM_MOUNTED="${SHIM_MOUNTED:-} ${SHIM_LATE_MOUNTED:-}"; fi
for p in ${SHIM_MOUNTED:-}; do [ "$p" = "$t" ] && exit 0; done
exit "${SHIM_MOUNTPOINT_RC:-32}""#,
    ),
    ("umount", r#"exit "${SHIM_UMOUNT_RC:-0}""#),
    (
        "id",
        r#"if [ "$1" = -u ] && [ "$#" -eq 1 ]; then PATH=${PATH#*:} exec id -u; fi
if [ "$1" = -u ] && [ "$2" = ritornello ] && [ -z "${SHIM_USER_EXISTS:-}" ]; then exit 1; fi
echo 1000"#,
    ),
];

/// `chown` logs its own line: the last argument, a name relative to the
/// pinned working directory, is written as the physical path it names. It
/// cannot really change an owner here, so it reports instead what the real
/// one would have done wrong: dereference a symbolic link, when called
/// without `-h`.
const CHOWN: &str = r#"#!/bin/sh
_n=$#; _i=0; _line=chown; _follow=1; _target=
for _a in "$@"; do
  _i=$((_i + 1))
  case $_a in -h|-hR|-Rh) _follow= ;; esac
  if [ "$_i" -eq "$_n" ]; then
    case $_a in .) _a=$(pwd -P) ;; ./*) _a=$(pwd -P)/${_a#./} ;; esac
    _target=$_a
  fi
  _line="$_line $_a"
done
printf '%s\n' "$_line" >> "$SHIM_LOG"
if [ -n "$_follow" ] && [ -L "$_target" ]; then printf 'chown-followed %s\n' "$_target" >> "$SHIM_LOG"; fi
if [ -n "${SHIM_CHOWN_FAIL:-}" ]; then case "$_line" in *"$SHIM_CHOWN_FAIL"*) exit 1 ;; esac; fi
exit 0
"#;

/// The real `awk`, unless `SHIM_AWK_FAIL` asks it to fail as a broken one
/// would (exit 2). Not logged: it changes nothing.
const AWK: &str = r#"#!/bin/sh
if [ -n "${SHIM_AWK_FAIL:-}" ]; then exit 2; fi
PATH=${PATH#*:} exec awk "$@"
"#;

/// The real `timeout`, unless `SHIM_TIMEOUT_RC` asks it to exit with that
/// code instead, as a `timeout` that could not run its command would. Not
/// logged: it changes nothing.
const TIMEOUT: &str = r#"#!/bin/sh
if [ -n "${SHIM_TIMEOUT_RC:-}" ]; then exit "$SHIM_TIMEOUT_RC"; fi
# The real one, looked up without the shims; the command it runs is still
# found through them.
real=$(PATH=${PATH#*:} command -v timeout)
exec "$real" "$@"
"#;

/// The test hook: at the point named by `HOOK_AT` (`<point> <path>`), run
/// `HOOK_DO` once — a process of the account swapping a path at exactly the
/// wrong moment. Asked for a value (`value <name> <default>`), it prints
/// the default, or `HOOK_UID` for `uid`.
const HOOK: &str = r#"#!/bin/sh
if [ "$1" = value ]; then
  if [ "$2" = uid ] && [ -n "${HOOK_UID:-}" ]; then printf '%s\n' "$HOOK_UID"; else printf '%s\n' "$3"; fi
  exit 0
fi
if [ "$*" = "${HOOK_AT:-}" ] && [ ! -e "$HOOK_DONE" ]; then
  : > "$HOOK_DONE"
  sh -c "$HOOK_DO"
fi
exit 0
"#;

struct Rig {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    shims: PathBuf,
    log: PathBuf,
    outside: PathBuf,
    runs: std::cell::Cell<u32>,
}

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
    /// The shim log, with the temporary root written `$R`.
    log: String,
}

impl Run {
    fn logged(&self, line: &str) -> bool {
        self.log.lines().any(|l| l == line)
    }
    fn log_at(&self, line: &str) -> usize {
        self.log.lines().position(|l| l == line).unwrap_or_else(|| panic!("{line:?} not logged:\n{}", self.log))
    }
    /// Nothing was changed: no shim that acts on the system was called.
    fn changed_nothing(&self) -> bool {
        self.log.lines().all(|l| l.starts_with("id ") || l.starts_with("mountpoint "))
    }
    /// A `chown` that would have followed a symbolic link.
    fn followed(&self) -> bool {
        self.log.lines().any(|l| l.starts_with("chown-followed "))
    }
    /// The line a run that stopped the service must end on.
    fn says_the_service_is_stopped(&self) -> bool {
        self.stderr.contains("systemctl start ritornello.service")
    }
}

impl Rig {
    fn new() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        let root = base.join("root");
        let shims = base.join("shims");
        let outside = base.join("outside");
        for d in [&root, &shims, &outside] {
            fs::create_dir_all(d).unwrap();
        }
        let mut scripts: Vec<(String, String)> = SHIMS
            .iter()
            .map(|(name, body)| {
                let text = format!("#!/bin/sh\nprintf '%s\\n' \"$(basename \"$0\") $*\" >> \"$SHIM_LOG\"\n{body}\nexit 0\n");
                (name.to_string(), text)
            })
            .collect();
        scripts.push(("chown".into(), CHOWN.into()));
        scripts.push(("awk".into(), AWK.into()));
        scripts.push(("timeout".into(), TIMEOUT.into()));
        for (name, text) in scripts {
            let p = shims.join(name);
            fs::write(&p, text).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let hook = base.join("hook");
        fs::write(&hook, HOOK).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let log = base.join("shim.log");
        Rig { _tmp: tmp, base, root, shims, log, outside, runs: std::cell::Cell::new(0) }
    }

    fn at(&self, abs: &str) -> PathBuf {
        self.root.join(abs.trim_start_matches('/'))
    }

    fn write(&self, abs: &str, content: &str) {
        let p = self.at(abs);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn read(&self, abs: &str) -> String {
        fs::read_to_string(self.at(abs)).unwrap_or_else(|e| panic!("{abs}: {e}"))
    }

    fn run(&self, plan: &Plan, archives: &BTreeMap<String, Vec<u8>>, env: &[(&str, String)]) -> Run {
        self.run_script(plan, &render(plan).expect("the plan renders"), archives, env)
    }

    fn run_script(&self, plan: &Plan, script: &str, archives: &BTreeMap<String, Vec<u8>>, env: &[(&str, String)]) -> Run {
        let n = self.runs.get() + 1;
        self.runs.set(n);
        let b = self.base.join(format!("bundle-{n}"));
        fs::create_dir_all(&b).unwrap();
        let bytes = bundle(plan, script, archives).expect("the plan bundles");
        tar::Archive::new(bytes.as_slice()).unpack(&b).unwrap();
        let _ = fs::remove_file(&self.log);
        fs::write(&self.log, "").unwrap();
        let path = format!("{}:{}", self.shims.display(), std::env::var("PATH").unwrap_or_default());
        // `RITORNELLO_TEST_UMASK` runs the script under that umask, as a
        // root shell with a stricter one than 022 would.
        let umask = env.iter().find(|(k, _)| *k == "RITORNELLO_TEST_UMASK").map_or("022", |(_, v)| v.as_str());
        let out = Command::new(shell())
            .arg("-c")
            .arg(format!("umask {umask} && exec {} \"$0\"", shell()))
            .arg(b.join("apply.sh"))
            .env("PATH", path)
            .env("RITORNELLO_INSTALL_ROOT", &self.root)
            .env("RITORNELLO_INSTALL_BUNDLE", &b)
            .env("SHIM_LOG", &self.log)
            .env("RITORNELLO_INSTALL_TEST_HOOK", self.base.join("hook"))
            .env("HOOK_DONE", self.base.join(format!("hook-done-{n}")))
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .output()
            .unwrap();
        let r = self.root.display().to_string();
        let run = Run {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            log: fs::read_to_string(&self.log).unwrap().replace(&r, "$R"),
        };
        eprintln!(
            "remote_script: apply.sh ran under {} (exit ok: {})\n--- stdout\n{}--- stderr\n{}--- log\n{}",
            shell(),
            run.ok,
            run.stdout,
            run.stderr,
            run.log
        );
        run
    }

    /// Every file below the root, as `/`-rooted paths.
    fn tree(&self) -> Vec<String> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                out.push(format!("/{}", p.strip_prefix(root).unwrap().display()));
                if p.is_dir() && !p.is_symlink() {
                    walk(&p, root, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &self.root, &mut out);
        out
    }
}

fn mode(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

fn content_of(path: &str) -> String {
    format!("content of {path}\n")
}

/// A `.tar.gz` built the way `package-release.sh` builds one: files staged
/// in a directory, then `tar -C <dir> -czf <out> .`, so every member is
/// named `./...`.
fn release_tar(files: &[(String, String)]) -> Vec<u8> {
    let stage = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let p = stage.path().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("out.tar.gz");
    let status = Command::new("tar")
        .arg("-C")
        .arg(stage.path())
        .args(["--owner=root", "--group=root", "--numeric-owner", "-czf"])
        .arg(&out)
        .arg(".")
        .status()
        .unwrap();
    assert!(status.success());
    fs::read(&out).unwrap()
}

/// A `.tar.gz` built with the `tar` crate, members named bare (`usr/...`).
/// `link` adds a symbolic-link member.
fn crate_tar(files: &[(String, String)], link: Option<(&str, &str)>) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut b = tar::Builder::new(gz);
    for (path, content) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, path, content.as_bytes()).unwrap();
    }
    if let Some((name, target)) = link {
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o777);
        h.set_entry_type(tar::EntryType::Symlink);
        b.append_link(&mut h, name, target).unwrap();
    }
    let mut gz = b.into_inner().unwrap();
    gz.flush().unwrap();
    gz.finish().unwrap()
}

fn files_of(dests: &[&str]) -> Vec<(String, String)> {
    dests.iter().map(|d| (d.trim_start_matches('/').to_string(), content_of(d))).collect()
}

fn put(archive: &str, dest: &str, mode: &str, owner: &str) -> Put {
    Put {
        archive: archive.into(),
        archive_path: dest.trim_start_matches('/').into(),
        dest: dest.into(),
        mode: mode.into(),
        owner: owner.into(),
    }
}

fn plugins_toml() -> String {
    "[[plugin]]\nname = \"radio\"\nexec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"\n\n\
     [[plugin]]\nname = \"generic-input\"\nexec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-generic-input\"\n"
        .into()
}

/// A fresh installation, shaped like the plan `compute` makes from the real
/// inventory for the core, `radio`, `generic-input` and French.
fn fresh_install() -> (Plan, BTreeMap<String, Vec<u8>>) {
    let core_dests: Vec<&str> = CORE_FILES.iter().map(|(d, _)| *d).collect();
    let radio_bin = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio";
    let input_bin = "/usr/local/lib/ritornello/plugins/ritornello-plugin-generic-input";
    let presets = ["/etc/ritornello/input-presets/keyboard.toml", "/etc/ritornello/input-presets/mce.toml"];

    let mut archives = BTreeMap::new();
    archives.insert(CORE.to_string(), release_tar(&files_of(&core_dests)));
    let mut radio = files_of(&[radio_bin]);
    radio.push(("initial-config/stations.example.toml".into(), "# example stations\n".into()));
    archives.insert(RADIO.to_string(), release_tar(&radio));
    let mut input = files_of(&[input_bin, presets[0], presets[1]]);
    input.push(("initial-config/input-bindings.example.toml".into(), "# example bindings\n".into()));
    archives.insert(INPUT.to_string(), release_tar(&input));
    archives.insert(
        FR.to_string(),
        release_tar(&[("pack.toml".into(), "language = \"fr\"\n".into()), ("core.toml".into(), "hello = \"bonjour\"\n".into())]),
    );

    let mut puts: Vec<Put> = CORE_FILES.iter().map(|(d, m)| put(CORE, d, m, "root:root")).collect();
    puts.push(put(RADIO, radio_bin, "0755", "root:root"));
    puts.push(put(INPUT, input_bin, "0755", "root:root"));
    for p in presets {
        puts.push(put(INPUT, p, "0644", "ritornello:ritornello"));
    }
    let privileged: Vec<String> = core_dests[1..].iter().map(|s| s.to_string()).collect();
    let plan = Plan {
        archives: archives.keys().cloned().collect(),
        ensure_user: true,
        puts,
        initial: vec![
            Initial {
                archive: RADIO.into(),
                archive_path: "initial-config/stations.example.toml".into(),
                plugin: "radio".into(),
                target: "stations.toml".into(),
            },
            Initial {
                archive: INPUT.into(),
                archive_path: "initial-config/input-bindings.example.toml".into(),
                plugin: "generic-input".into(),
                target: "input-bindings.toml".into(),
            },
        ],
        packs: vec![(FR.into(), "ritornello-lang-fr".into())],
        plugins_toml: Some(plugins_toml()),
        registry: Some(Registry {
            format: 1,
            components: [(
                "core".to_string(),
                Recorded { version: "0.2.0-beta.2".into(), privileged: privileged.clone() },
            )]
            .into(),
        }),
        // A fresh device recorded nothing: the union is the new record.
        provisional_registry: Some(Registry {
            format: 1,
            components: [("core".to_string(), Recorded { version: "0.2.0-beta.2".into(), privileged })].into(),
        }),
        enable_units: vec!["ritornello.service".into()],
        start_service: true,
        ..Plan::default()
    };
    (plan, archives)
}

/// Removing everything, shaped like `compute`'s `RemoveAll` after
/// `fresh_install`.
fn remove_everything(erase_data: bool) -> Plan {
    let (installed, _) = fresh_install();
    let mut remove_trees: Vec<String> =
        ["/etc/ritornello", "/usr/local/lib/ritornello", "/var/lib/ritornello-update", "/var/lib/ritornello-install"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    if erase_data {
        remove_trees.push("/var/lib/ritornello".into());
    }
    let mut remove_files: Vec<String> = installed.puts.iter().map(|p| p.dest.clone()).collect();
    remove_files.sort();
    Plan {
        stop_service: true,
        disable_units: vec!["ritornello.service".into()],
        remove_files,
        remove_trees,
        unmount_roots: vec!["/mnt/ritornello".into()],
        remove_mount_roots: vec!["/mnt/ritornello".into()],
        remove_plugins_toml: true,
        remove_registry: true,
        remove_user: erase_data,
        ..Plan::default()
    }
}

/// Removing the files plugin and its companion `files-mount`, whose shares
/// live under `/mnt/ritornello`.
fn remove_files_plugin() -> Plan {
    Plan {
        stop_service: true,
        disable_units: vec!["ritornello-media-mount.service".into()],
        unmount_roots: vec!["/mnt/ritornello".into()],
        remove_files: vec![
            "/etc/polkit-1/rules.d/51-ritornello-media.rules".into(),
            "/etc/systemd/system/ritornello-media-mount.service".into(),
            "/usr/local/lib/ritornello/plugins/ritornello-plugin-files".into(),
            "/usr/local/lib/ritornello/ritornello-media-mount".into(),
        ],
        start_service: true,
        ..Plan::default()
    }
}

fn plant_files_plugin(rig: &Rig) {
    for f in &remove_files_plugin().remove_files {
        rig.write(f, "old\n");
    }
}

// --- 1. A fresh installation --------------------------------------------

#[test]
fn a_fresh_installation_places_every_file_with_the_plan_s_mode_and_owner() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[]);
    assert!(run.ok, "{}", run.stderr);

    for p in &plan.puts {
        assert_eq!(rig.read(&p.dest), content_of(&p.dest), "{}", p.dest);
        let want = if p.mode == "0755" { 0o755 } else { 0o644 };
        assert_eq!(mode(&rig.at(&p.dest)), want, "{}", p.dest);
        assert!(run.logged(&format!("chown -h {} $R{}.ritornello-new", p.owner, p.dest)), "{}", p.dest);
    }
    assert_eq!(rig.read("/var/lib/ritornello/plugins/radio/stations.toml"), "# example stations\n");
    assert_eq!(rig.read("/var/lib/ritornello/plugins/generic-input/input-bindings.toml"), "# example bindings\n");
    assert_eq!(mode(&rig.at("/var/lib/ritornello/plugins/radio/stations.toml")), 0o644);
    assert_eq!(rig.read("/etc/ritornello/language-packs/ritornello-lang-fr/pack.toml"), "language = \"fr\"\n");
    assert_eq!(rig.read("/etc/ritornello/plugins.toml"), plugins_toml());
    assert_eq!(rig.read("/var/lib/ritornello-install/installed.toml"), plan.registry.as_ref().unwrap().render());

    assert!(run.logged("useradd --system --home-dir /var/lib/ritornello --no-create-home --shell /usr/sbin/nologin ritornello"));
    assert!(run.logged("chown -h root:root $R/usr/local/bin/ritornello-core.ritornello-new"));
    assert!(run.logged("chown -h ritornello:ritornello $R/var/lib/ritornello/plugins/radio/stations.toml.ritornello-new"));
    assert!(run.logged("chown -hR ritornello:ritornello $R/etc/ritornello/language-packs/.ritornello-lang-fr.ritornello-new"));
    assert_eq!(mode(&rig.at("/etc/ritornello/language-packs/ritornello-lang-fr")), 0o755);
    assert!(!rig.at("/etc/ritornello/language-packs/.ritornello-lang-fr.ritornello-new").exists());
    assert!(!run.followed(), "{}", run.log);
    assert!(!run.says_the_service_is_stopped());
    let reload = run.log_at("systemctl daemon-reload");
    let enable = run.log_at("systemctl enable ritornello.service");
    let restart = run.log_at("systemctl restart ritornello.service");
    assert!(reload < enable && enable < restart, "{}", run.log);
    assert!(run.log_at("id -u ritornello") < run.log_at("chown -h root:root $R/usr/local/bin/ritornello-core.ritornello-new"));
}

/// Written under a temporary name and renamed: the log shows the write
/// under that name, and none is left behind.
#[test]
fn every_file_is_written_under_a_temporary_name_and_none_is_left_behind() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[]);
    assert!(run.ok, "{}", run.stderr);
    let chowned_new = run.log.lines().filter(|l| l.starts_with("chown ") && l.ends_with(".ritornello-new")).count();
    // plugins.toml, installed.toml, the provisional registry, the pack's stage.
    assert_eq!(chowned_new, plan.puts.len() + plan.initial.len() + 4, "{}", run.log);
    let leftovers: Vec<String> = rig.tree().into_iter().filter(|p| p.ends_with(".ritornello-new")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// R23: under `/etc/ritornello` and `/var/lib/ritornello` a created
/// directory is the account's, anywhere else root's, and `-install` is not
/// under `/var/lib/ritornello` for sharing its first letters.
#[test]
fn every_created_directory_gets_the_owner_of_the_tree_it_is_in() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[]);
    assert!(run.ok, "{}", run.stderr);
    for d in [
        "/etc/ritornello",
        "/etc/ritornello/input-presets",
        "/etc/ritornello/language-packs",
        "/var/lib/ritornello",
        "/var/lib/ritornello/plugins",
        "/var/lib/ritornello/plugins/radio",
    ] {
        assert!(run.logged(&format!("chown -h ritornello:ritornello $R{d}")), "{d}\n{}", run.log);
        assert!(!run.logged(&format!("chown -h root:root $R{d}")), "{d}");
    }
    for d in [
        "/etc",
        "/etc/systemd/system",
        "/etc/polkit-1/rules.d",
        "/usr/local/bin",
        "/usr/local/lib/ritornello/plugins",
        "/var/lib",
        "/var/lib/ritornello-install",
    ] {
        assert!(run.logged(&format!("chown -h root:root $R{d}")), "{d}\n{}", run.log);
        assert!(!run.logged(&format!("chown -h ritornello:ritornello $R{d}")), "{d}");
        assert_eq!(mode(&rig.at(d)), 0o755, "{d}");
    }
}

/// A directory that already exists keeps its owner: only what the script
/// creates is chowned.
#[test]
fn an_existing_directory_is_not_chowned_again() {
    let rig = Rig::new();
    fs::create_dir_all(rig.at("/usr/local/bin")).unwrap();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[]);
    assert!(run.ok, "{}", run.stderr);
    assert!(!run.logged("chown -h root:root $R/usr/local/bin"), "{}", run.log);
    assert!(run.logged("chown -h root:root $R/usr/local/lib"), "{}", run.log);
}

// --- 2. An existing configuration ----------------------------------------

#[test]
fn an_existing_configuration_is_not_overwritten() {
    let rig = Rig::new();
    let mine = "# my own stations, added from the browser\n[[station]]\nname = \"FIP\"\n";
    rig.write("/var/lib/ritornello/plugins/radio/stations.toml", mine);
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[("SHIM_USER_EXISTS", "1".into())]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(fs::read(rig.at("/var/lib/ritornello/plugins/radio/stations.toml")).unwrap(), mine.as_bytes());
    assert_eq!(rig.read("/var/lib/ritornello/plugins/generic-input/input-bindings.toml"), "# example bindings\n");
    assert!(!run.log.contains("useradd"), "{}", run.log);
}

/// A `chown` failing in the middle of `put`, with the `mv` after it still
/// succeeding, stops the script: a file left with the wrong owner is not a
/// success.
#[test]
fn a_failing_initial_configuration_stops_the_script() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[("SHIM_CHOWN_FAIL", "stations.toml.ritornello-new".into())]);
    assert!(!run.ok, "a chown that failed must stop the script");
    assert!(!run.stdout.contains("ritornello-install: done"), "{}", run.stdout);
    assert!(!run.logged("systemctl restart ritornello.service"), "{}", run.log);
}

/// A restrictive umask (root's is not always 022) changes nothing: every
/// directory created is 0755, every file has the plan's own mode.
#[test]
fn modes_do_not_depend_on_the_umask() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[("RITORNELLO_TEST_UMASK", "077".into())]);
    assert!(run.ok, "{}", run.stderr);
    for d in [
        "/etc/ritornello",
        "/var/lib/ritornello/plugins/radio",
        "/usr/local/lib/ritornello/plugins",
        "/var/lib/ritornello-install",
        "/etc/ritornello/language-packs/ritornello-lang-fr",
    ] {
        assert_eq!(mode(&rig.at(d)), 0o755, "{d}");
    }
    assert_eq!(mode(&rig.at("/usr/local/bin/ritornello-core")), 0o755);
    assert_eq!(mode(&rig.at("/etc/ritornello/plugins.toml")), 0o644);
}

// --- 3. Removing the files plugin, with shares ---------------------------

/// R3 (a): a mounted share is unmounted, then removed with `rmdir`; one
/// that is not mounted is only removed.
#[test]
fn a_mounted_share_is_unmounted_then_removed() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
    fs::create_dir_all(rig.at("/mnt/ritornello/old")).unwrap();
    let mounted = rig.at("/mnt/ritornello/nas").display().to_string();
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &[("SHIM_MOUNTED", mounted)]);
    assert!(run.ok, "{}", run.stderr);
    assert!(run.logged("umount $R/mnt/ritornello/nas"), "{}", run.log);
    assert!(!run.logged("umount $R/mnt/ritornello/old"), "{}", run.log);
    assert!(!rig.at("/mnt/ritornello/nas").exists() && !rig.at("/mnt/ritornello/old").exists());
    assert!(rig.at("/mnt/ritornello").is_dir(), "the mount root itself stays");
    for f in &remove_files_plugin().remove_files {
        assert!(!rig.at(f).exists(), "{f}");
    }
    assert!(run.log_at("systemctl disable --now ritornello-media-mount.service") < run.log_at("umount $R/mnt/ritornello/nas"));
}

/// R3 (b): a mount point is removed with `rmdir` and nothing stronger. One
/// that is not mounted but still holds a file — a share whose unmount did
/// not really happen — makes `rmdir` fail, and the script stops with the
/// file intact and nothing further removed.
#[test]
fn a_mount_point_still_holding_a_file_stops_the_script_with_the_file_intact() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    rig.write("/mnt/ritornello/nas/Album/01.flac", "music\n");
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &[]);
    assert!(!run.ok);
    assert!(run.stderr.contains("cannot remove"), "{}", run.stderr);
    assert_eq!(rig.read("/mnt/ritornello/nas/Album/01.flac"), "music\n");
    assert_eq!(rig.read("/usr/local/lib/ritornello/plugins/ritornello-plugin-files"), "old\n");
}

// --- 4. A failed unmount -------------------------------------------------

#[test]
fn a_failed_unmount_stops_everything() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    rig.write("/mnt/ritornello/nas/Album/01.flac", "music\n");
    let mut plan = remove_files_plugin();
    let core = "/usr/local/bin/ritornello-core";
    plan.archives.insert(CORE.into());
    plan.puts.push(put(CORE, core, "0755", "root:root"));
    let archives = [(CORE.to_string(), release_tar(&files_of(&[core])))].into();
    let mounted = rig.at("/mnt/ritornello/nas").display().to_string();
    let run = rig.run(&plan, &archives, &[("SHIM_MOUNTED", mounted), ("SHIM_UMOUNT_RC", "1".into())]);
    assert!(!run.ok);
    assert!(run.stderr.contains("cannot unmount"), "{}", run.stderr);
    assert!(run.says_the_service_is_stopped(), "M5: {}", run.stderr);
    assert!(!rig.at(core).exists(), "nothing was placed");
    assert!(!run.stdout.contains("placing"), "{}", run.stdout);
    assert_eq!(rig.read("/mnt/ritornello/nas/Album/01.flac"), "music\n");
    assert_eq!(rig.read("/usr/local/lib/ritornello/plugins/ritornello-plugin-files"), "old\n", "nothing was removed");
}

// --- 5. Removing everything ----------------------------------------------

#[test]
fn removing_everything_without_the_data_keeps_the_data_and_the_account() {
    let rig = Rig::new();
    let (install, archives) = fresh_install();
    assert!(rig.run(&install, &archives, &[]).ok);
    rig.write("/var/lib/ritornello/state.toml", "volume = 40\n");

    let run = rig.run(&remove_everything(false), &BTreeMap::new(), &[("SHIM_USER_EXISTS", "1".into())]);
    assert!(run.ok, "{}", run.stderr);
    for gone in ["/etc/ritornello", "/usr/local/lib/ritornello", "/var/lib/ritornello-install", "/usr/local/bin/ritornello-core"] {
        assert!(!rig.at(gone).exists(), "{gone}");
    }
    for (unit, _) in CORE_FILES {
        assert!(!rig.at(unit).exists(), "{unit}");
    }
    assert_eq!(rig.read("/var/lib/ritornello/state.toml"), "volume = 40\n");
    assert_eq!(rig.read("/var/lib/ritornello/plugins/radio/stations.toml"), "# example stations\n");
    assert!(!run.log.contains("userdel"), "{}", run.log);
    assert!(run.log_at("systemctl stop ritornello.service") < run.log_at("systemctl disable --now ritornello.service"));
    assert!(run.logged("systemctl daemon-reload"));
    assert!(!run.log.contains("restart"), "{}", run.log);
}

#[test]
fn removing_everything_with_the_data_removes_the_data_and_the_account() {
    let rig = Rig::new();
    let (install, archives) = fresh_install();
    assert!(rig.run(&install, &archives, &[]).ok);
    let run = rig.run(&remove_everything(true), &BTreeMap::new(), &[("SHIM_USER_EXISTS", "1".into())]);
    assert!(run.ok, "{}", run.stderr);
    assert!(!rig.at("/var/lib/ritornello").exists());
    assert!(run.logged("userdel ritornello"), "{}", run.log);
}

/// R55: a total removal also removes the mount root, after its shares.
#[test]
fn a_total_removal_removes_the_mount_root_itself() {
    let rig = Rig::new();
    let (install, archives) = fresh_install();
    assert!(rig.run(&install, &archives, &[]).ok);
    fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
    let mounted = rig.at("/mnt/ritornello/nas").display().to_string();
    let run = rig.run(&remove_everything(false), &BTreeMap::new(), &[("SHIM_USER_EXISTS", "1".into()), ("SHIM_MOUNTED", mounted)]);
    assert!(run.ok, "{}", run.stderr);
    assert!(!rig.at("/mnt/ritornello").exists(), "the mount root is gone");
    assert!(rig.at("/mnt").is_dir(), "only the root itself, never /mnt");
    assert!(run.log_at("umount $R/mnt/ritornello/nas") < run.log_at("systemctl daemon-reload"));
}

/// R55: a root that is already absent is fine.
#[test]
fn a_total_removal_with_no_mount_root_is_fine() {
    let rig = Rig::new();
    let (install, archives) = fresh_install();
    assert!(rig.run(&install, &archives, &[]).ok);
    let run = rig.run(&remove_everything(false), &BTreeMap::new(), &[("SHIM_USER_EXISTS", "1".into())]);
    assert!(run.ok, "{}", run.stderr);
    assert!(!rig.at("/mnt/ritornello").exists());
}

/// R55/R56: a file, or a hidden entry, sitting directly in the root is
/// refused by the verify phase: nothing was stopped, removed or unmounted,
/// and the entry survives.
#[test]
fn a_total_removal_refuses_a_stray_entry_in_the_mount_root_before_changing_anything() {
    for stray in ["stray.txt", ".hidden"] {
        let rig = Rig::new();
        let (install, archives) = fresh_install();
        assert!(rig.run(&install, &archives, &[]).ok);
        fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
        let path = format!("/mnt/ritornello/{stray}");
        rig.write(&path, "mine\n");
        let run = rig.run(&remove_everything(false), &BTreeMap::new(), &[("SHIM_USER_EXISTS", "1".into())]);
        assert!(!run.ok, "{stray}");
        assert!(run.stderr.contains("not a share directory") && run.stderr.contains(stray), "{}", run.stderr);
        assert_eq!(rig.read(&path), "mine\n");
        assert!(rig.at("/mnt/ritornello/nas").is_dir(), "the share directory was not touched either");
        assert!(run.changed_nothing(), "{stray}: {}", run.log);
        assert!(!run.says_the_service_is_stopped(), "{}", run.stderr);
    }
}

/// R55: the last check before `rmdir`, kept as a backstop for what appears
/// once the verify phase has passed: a file put there now stops the script
/// with its own message, and survives.
#[test]
fn a_total_removal_stops_on_a_file_that_appears_in_the_mount_root_late() {
    let rig = Rig::new();
    let (install, archives) = fresh_install();
    assert!(rig.run(&install, &archives, &[]).ok);
    fs::create_dir_all(rig.at("/mnt/ritornello")).unwrap();
    let late = rig.at("/mnt/ritornello/late.txt");
    let env = hook_at("checking_root /mnt/ritornello", format!("echo mine > {}", q(&late)));
    let run = rig.run(&remove_everything(false), &BTreeMap::new(), &[&[("SHIM_USER_EXISTS", "1".to_string())][..], &env[..]].concat());
    assert!(!run.ok);
    assert!(run.stderr.contains("/mnt/ritornello") && run.stderr.contains("is not empty"), "{}", run.stderr);
    assert!(late.exists());
}

/// R55: the root is removed with `rmdir` and nothing stronger. A share
/// mounted between the checks and the removal puts content there; `rmdir`
/// refuses it, a recursive delete would have emptied it.
#[test]
fn a_total_removal_never_deletes_what_appears_in_the_mount_root_after_the_checks() {
    let rig = Rig::new();
    let (install, archives) = fresh_install();
    assert!(rig.run(&install, &archives, &[]).ok);
    fs::create_dir_all(rig.at("/mnt/ritornello")).unwrap();
    let late = rig.at("/mnt/ritornello/late.flac");
    let env = hook_at("removing_root /mnt/ritornello", format!("echo the NAS > {}", q(&late)));
    let run = rig.run(&remove_everything(false), &BTreeMap::new(), &[&[("SHIM_USER_EXISTS", "1".to_string())][..], &env[..]].concat());
    assert!(!run.ok);
    assert!(run.stderr.contains("rmdir refused"), "{}", run.stderr);
    assert!(late.exists(), "the content that appeared survives");
}

/// R56: a share or a bind mount sitting ON the root: what the glob finds
/// below it is the NAS's own top level, whose empty directories `rmdir`
/// would delete. The verify phase stops the run before anything changes, for
/// a total removal and for the removal of `files` alone.
#[test]
fn a_mount_root_that_is_itself_mounted_stops_before_anything_changes() {
    for total in [true, false] {
        let rig = Rig::new();
        let plan = if total {
            let (install, archives) = fresh_install();
            assert!(rig.run(&install, &archives, &[]).ok);
            remove_everything(false)
        } else {
            plant_files_plugin(&rig);
            remove_files_plugin()
        };
        fs::create_dir_all(rig.at("/mnt/ritornello/Albums-empty")).unwrap();
        rig.write("/mnt/ritornello/Zfull/01.flac", "music\n");
        let mounted = rig.at("/mnt/ritornello").display().to_string();
        let run = rig.run(&plan, &BTreeMap::new(), &[("SHIM_USER_EXISTS", "1".into()), ("SHIM_MOUNTED", mounted)]);
        assert!(!run.ok, "total={total}");
        assert!(run.stderr.contains("/mnt/ritornello") && run.stderr.contains("itself a mount point"), "{}", run.stderr);
        assert!(rig.at("/mnt/ritornello/Albums-empty").is_dir(), "total={total}: the NAS's empty directory survives");
        assert_eq!(rig.read("/mnt/ritornello/Zfull/01.flac"), "music\n");
        assert!(run.changed_nothing(), "total={total}: {}", run.log);
        assert!(!run.says_the_service_is_stopped(), "{}", run.stderr);
    }
}

/// R56: the same check runs again right before the unmounts: a mount that
/// appears once the service has stopped is still refused, NAS intact.
#[test]
fn a_mount_placed_on_the_root_after_the_verify_phase_is_still_refused() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello/Albums-empty")).unwrap();
    let flag = rig.outside.join("late-flag");
    let env = [
        ("SHIM_PLANT_LINK", flag.display().to_string()),
        ("SHIM_PLANT_TARGET", "/".to_string()),
        ("SHIM_LATE_FLAG", flag.display().to_string()),
        ("SHIM_LATE_MOUNTED", rig.at("/mnt/ritornello").display().to_string()),
    ];
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &env);
    assert!(!run.ok);
    assert!(run.stderr.contains("itself a mount point"), "{}", run.stderr);
    assert!(rig.at("/mnt/ritornello/Albums-empty").is_dir());
    assert!(run.says_the_service_is_stopped(), "the stop did happen: {}", run.stderr);
}

/// R56: exit 1 is ambiguous (util-linux: a failure; busybox: "not a mount
/// point") and cannot be told apart, so it reads as "not a mount point": the
/// `ls -A` and `rmdir` that follow are the fallback, and refuse a live mount.
#[test]
fn a_mountpoint_exit_of_one_is_read_as_not_mounted_and_left_to_rmdir() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &[("SHIM_MOUNTPOINT_RC", "1".into())]);
    assert!(run.ok, "{}", run.stderr);
    assert!(!rig.at("/mnt/ritornello/nas").exists());
}

/// R56: a `mountpoint` that fails with neither 0, 1 nor 32 gives no answer,
/// and no answer stops the run.
#[test]
fn a_mountpoint_that_gives_no_answer_stops_before_anything_changes() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &[("SHIM_MOUNTPOINT_RC", "5".into())]);
    assert!(!run.ok);
    assert!(run.stderr.contains("cannot tell whether /mnt/ritornello is a mount point"), "{}", run.stderr);
    assert!(rig.at("/mnt/ritornello/nas").is_dir());
    assert!(run.changed_nothing(), "{}", run.log);
}

/// R55: removing only the files plugin never removes the root.
#[test]
fn removing_only_files_leaves_the_mount_root() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello")).unwrap();
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &[]);
    assert!(run.ok, "{}", run.stderr);
    assert!(rig.at("/mnt/ritornello").is_dir());
}

// --- The fixed order -----------------------------------------------------

/// Removal before placement. A file an earlier version recorded for one
/// component and this version places for another is both removed and
/// placed; the other order would place the new binary, then erase it.
#[test]
fn a_file_both_removed_and_placed_is_there_at_the_end() {
    let rig = Rig::new();
    let moved = "/usr/local/lib/ritornello/ritornello-media-mount";
    rig.write(moved, "old\n");
    let plan = Plan {
        archives: [CORE.to_string()].into(),
        remove_files: vec![moved.into()],
        puts: vec![put(CORE, moved, "0755", "root:root")],
        ..Plan::default()
    };
    let archives = [(CORE.to_string(), release_tar(&files_of(&[moved])))].into();
    let run = rig.run(&plan, &archives, &[]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(rig.read(moved), content_of(moved));
    let removing = run.stdout.find(&format!("removing {moved}")).unwrap();
    let placing = run.stdout.find(&format!("placing {moved}")).unwrap();
    assert!(removing < placing, "{}", run.stdout);
}

/// The plan `compute` makes for a device whose registry predates the
/// companion — the helper, its unit and its rule recorded under `files` —
/// run for real: the companion's own archive places the three files over
/// the old ones, nothing is removed, disabled or unmounted on the way, the
/// unit is enabled, and the registry written names `files-mount`.
#[test]
fn a_companion_moves_the_files_an_older_registry_recorded_under_its_plugin() {
    use crate::plan::{Intent, compute, tests as p};
    let files_exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-files";
    let moved = [
        "/etc/systemd/system/ritornello-media-mount.service",
        "/etc/polkit-1/rules.d/51-ritornello-media.rules",
        "/usr/local/lib/ritornello/ritornello-media-mount",
    ];
    let old = p::registry(&[("core", &["/etc/systemd/system/ritornello.service"]), ("files", &moved)]);
    let device = p::dev(&[("files", files_exec)], Some(old), &[], &[]);
    let intent = Intent::InstallOrUpdate { plugins: p::set(&["files"]), packs: p::set(&[]), erase_data: p::set(&[]) };
    let plan = compute(&p::inv(), &device, &intent).expect("the plan computes");
    let mount_archive = "ritornello-files-mount-0.2.0-beta.2-arm64.tar.gz";
    assert!(plan.archives.contains(mount_archive), "{:?}", plan.archives);

    let archives: BTreeMap<String, Vec<u8>> = plan
        .archives
        .iter()
        .map(|a| {
            let dests: Vec<&str> = plan.puts.iter().filter(|x| &x.archive == a).map(|x| x.dest.as_str()).collect();
            (a.clone(), release_tar(&files_of(&dests)))
        })
        .collect();
    let rig = Rig::new();
    for f in moved.iter().chain([&files_exec]) {
        rig.write(f, "old\n");
    }
    rig.write("/etc/ritornello/plugins.toml", device.plugins_toml.as_deref().unwrap());
    // A share mounted under the root, holding a file: a wrong unmount would
    // log `umount` and `rmdir` it, which is what this run must never do.
    rig.write("/mnt/ritornello/nas/music.flac", "a song\n");
    let mounted = rig.at("/mnt/ritornello/nas").display().to_string();
    let run = rig.run(&plan, &archives, &[("SHIM_MOUNTED", mounted)]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(rig.read("/mnt/ritornello/nas/music.flac"), "a song\n", "the share is untouched");
    assert!(!run.log.lines().any(|l| l.starts_with("umount")), "{}", run.log);

    for f in moved {
        assert_eq!(rig.read(f), content_of(f), "{f}");
        assert!(!run.stdout.contains(&format!("removing {f}")), "{f}: {}", run.stdout);
    }
    assert_eq!(mode(&rig.at(moved[2])), 0o755);
    assert!(run.logged("systemctl enable ritornello-media-mount.service"), "{}", run.log);
    assert!(!run.log.lines().any(|l| l.starts_with("systemctl disable")), "{}", run.log);
    let written = crate::registry::Registry::parse(&rig.read("/var/lib/ritornello-install/installed.toml")).unwrap();
    assert_eq!(written.components["files-mount"].privileged, moved);
    assert!(!written.components.contains_key("files"), "{written:?}");
}

// --- R23: archive members and sources ------------------------------------

/// `package-release.sh` names members `./usr/...` and the inventory names
/// them `usr/...`: both shapes land where the plan looks.
#[test]
fn members_named_with_or_without_a_leading_dot_slash_are_both_found() {
    let rig = Rig::new();
    let a = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio";
    let b = "/usr/local/lib/ritornello/plugins/ritornello-plugin-cd";
    let dotted = release_tar(&files_of(&[a]));
    let listing = {
        let p = rig.base.join("dotted.tar.gz");
        fs::write(&p, &dotted).unwrap();
        String::from_utf8(Command::new("tar").arg("-tzf").arg(&p).output().unwrap().stdout).unwrap()
    };
    assert!(listing.lines().any(|l| l == format!("./{}", &a[1..])), "the real shape is ./-prefixed:\n{listing}");
    let bare = crate_tar(&files_of(&[b]), None);
    let plan = Plan {
        archives: [RADIO.to_string(), CORE.to_string()].into(),
        puts: vec![put(RADIO, a, "0755", "root:root"), put(CORE, b, "0755", "root:root")],
        ..Plan::default()
    };
    let run = rig.run(&plan, &[(RADIO.to_string(), dotted), (CORE.to_string(), bare)].into(), &[]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(rig.read(a), content_of(a));
    assert_eq!(rig.read(b), content_of(b));
}

/// A file the plan places that its archive lacks is refused before step 1:
/// no account, no stopped service, nothing below the root.
#[test]
fn a_source_missing_from_its_archive_is_refused_before_anything_changes() {
    let rig = Rig::new();
    let mut plan = remove_files_plugin();
    plant_files_plugin(&rig);
    plan.ensure_user = true;
    plan.archives.insert(CORE.into());
    plan.puts = vec![
        put(CORE, "/usr/local/bin/ritornello-core", "0755", "root:root"),
        put(CORE, "/etc/systemd/system/ritornello.service", "0644", "root:root"),
    ];
    let archives = [(CORE.to_string(), release_tar(&files_of(&["/usr/local/bin/ritornello-core"])))].into();
    let run = rig.run(&plan, &archives, &[]);
    assert!(!run.ok);
    assert!(run.stderr.contains("missing from the bundle"), "{}", run.stderr);
    assert!(!run.says_the_service_is_stopped(), "stopped before step 2: {}", run.stderr);
    assert!(run.changed_nothing(), "{}", run.log);
    assert!(!rig.at("/usr/local/bin/ritornello-core").exists());
    assert_eq!(rig.read("/usr/local/lib/ritornello/plugins/ritornello-plugin-files"), "old\n");
}

#[test]
fn an_archive_holding_a_link_is_refused_before_anything_changes() {
    for pack in [false, true] {
        let rig = Rig::new();
        let dest = "/usr/local/bin/ritornello-core";
        let linked = crate_tar(&files_of(&[dest]), Some(("usr/local/lib/evil", "/etc/shadow")));
        let mut plan = Plan { ensure_user: true, ..Plan::default() };
        let mut archives = BTreeMap::new();
        if pack {
            plan.archives.insert(FR.into());
            plan.packs = vec![(FR.into(), "ritornello-lang-fr".into())];
            archives.insert(FR.to_string(), linked);
        } else {
            plan.archives.insert(CORE.into());
            plan.puts = vec![put(CORE, dest, "0755", "root:root")];
            archives.insert(CORE.to_string(), linked);
        }
        let run = rig.run(&plan, &archives, &[]);
        assert!(!run.ok, "pack: {pack}");
        assert!(run.stderr.contains("neither a file nor a directory"), "{}", run.stderr);
        assert!(run.changed_nothing(), "{}", run.log);
        assert!(rig.tree().is_empty(), "{:?}", rig.tree());
    }
}

#[test]
fn a_directory_where_a_file_goes_is_refused_before_anything_changes() {
    let rig = Rig::new();
    fs::create_dir_all(rig.at("/etc/ritornello/plugins.toml")).unwrap();
    let plan = Plan { ensure_user: true, plugins_toml: Some(plugins_toml()), ..Plan::default() };
    let run = rig.run(&plan, &BTreeMap::new(), &[]);
    assert!(!run.ok);
    assert!(run.stderr.contains("a directory is in the way"), "{}", run.stderr);
    assert!(run.changed_nothing(), "{}", run.log);
}

// --- R25: symbolic links -------------------------------------------------

/// `var/lib/ritornello/plugins -> <outside>/plugins`, where the outside
/// directory stands for the NAS.
fn plant_plugins_link(rig: &Rig) -> PathBuf {
    let sentinel = rig.outside.join("plugins/radio/stations.toml");
    fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    fs::write(&sentinel, "the NAS\n").unwrap();
    fs::create_dir_all(rig.at("/var/lib/ritornello")).unwrap();
    symlink(rig.outside.join("plugins"), rig.at("/var/lib/ritornello/plugins")).unwrap();
    sentinel
}

fn erase_radio() -> Plan {
    Plan { ensure_user: true, stop_service: true, remove_trees: vec!["/var/lib/ritornello/plugins/radio".into()], ..Plan::default() }
}

fn place_into_radio() -> (Plan, BTreeMap<String, Vec<u8>>) {
    let dest = "/var/lib/ritornello/plugins/radio/stations.toml";
    let plan = Plan {
        ensure_user: true,
        stop_service: true,
        archives: [RADIO.to_string()].into(),
        puts: vec![put(RADIO, dest, "0644", "ritornello:ritornello")],
        ..Plan::default()
    };
    (plan, [(RADIO.to_string(), release_tar(&files_of(&[dest])))].into())
}

#[test]
fn a_link_on_the_way_to_a_tree_removal_is_refused_before_anything_changes() {
    let rig = Rig::new();
    let sentinel = plant_plugins_link(&rig);
    let run = rig.run(&erase_radio(), &BTreeMap::new(), &[]);
    assert!(!run.ok);
    assert!(run.stderr.contains("is a symbolic link"), "{}", run.stderr);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "the NAS\n");
    assert!(run.changed_nothing(), "{}", run.log);
}

#[test]
fn a_link_on_the_way_to_a_placement_is_refused_before_anything_changes() {
    let rig = Rig::new();
    let sentinel = plant_plugins_link(&rig);
    let (plan, archives) = place_into_radio();
    let run = rig.run(&plan, &archives, &[]);
    assert!(!run.ok);
    assert!(run.stderr.contains("is a symbolic link"), "{}", run.stderr);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "the NAS\n");
    assert!(run.changed_nothing(), "{}", run.log);
}

/// The last component counts too.
#[test]
fn a_placement_that_is_itself_a_link_is_refused() {
    let rig = Rig::new();
    let target = rig.outside.join("mce.toml");
    fs::write(&target, "outside\n").unwrap();
    fs::create_dir_all(rig.at("/etc/ritornello/input-presets")).unwrap();
    symlink(&target, rig.at("/etc/ritornello/input-presets/mce.toml")).unwrap();
    let dest = "/etc/ritornello/input-presets/mce.toml";
    let plan = Plan {
        ensure_user: true,
        archives: [INPUT.to_string()].into(),
        puts: vec![put(INPUT, dest, "0644", "ritornello:ritornello")],
        ..Plan::default()
    };
    let run = rig.run(&plan, &[(INPUT.to_string(), release_tar(&files_of(&[dest])))].into(), &[]);
    assert!(!run.ok);
    assert!(run.changed_nothing(), "{}", run.log);
    assert!(rig.at(dest).is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "outside\n");
}

/// The environment `systemctl stop` needs to plant `link -> target`, after
/// the verify phase has passed.
fn plant_later(link: &Path, target: &Path) -> Vec<(&'static str, String)> {
    vec![("SHIM_PLANT_LINK", link.display().to_string()), ("SHIM_PLANT_TARGET", target.display().to_string())]
}

/// Planted after the verify phase, before the removal: `rmtree` walks the
/// path again itself.
#[test]
fn a_link_planted_after_the_checks_still_stops_a_tree_removal() {
    let rig = Rig::new();
    let sentinel = rig.outside.join("plugins/radio/stations.toml");
    fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    fs::write(&sentinel, "the NAS\n").unwrap();
    fs::create_dir_all(rig.at("/var/lib/ritornello")).unwrap();
    let env = plant_later(&rig.at("/var/lib/ritornello/plugins"), &rig.outside.join("plugins"));
    let run = rig.run(&erase_radio(), &BTreeMap::new(), &env);
    assert!(run.logged("systemctl stop ritornello.service"), "the link was planted after the checks");
    assert!(!run.ok);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "the NAS\n");
}

#[test]
fn a_link_planted_after_the_checks_still_stops_a_placement() {
    let rig = Rig::new();
    let sentinel = rig.outside.join("plugins/radio/stations.toml");
    fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    fs::write(&sentinel, "the NAS\n").unwrap();
    fs::create_dir_all(rig.at("/var/lib/ritornello")).unwrap();
    let env = plant_later(&rig.at("/var/lib/ritornello/plugins"), &rig.outside.join("plugins"));
    let (plan, archives) = place_into_radio();
    let run = rig.run(&plan, &archives, &env);
    assert!(run.logged("systemctl stop ritornello.service"));
    assert!(!run.ok);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "the NAS\n");
}

/// Planted at the placement itself, after the checks: `mkdirs` walks only
/// the parent, so this is `put`'s own walk.
#[test]
fn a_link_planted_after_the_checks_at_the_placement_itself_stops_it() {
    let rig = Rig::new();
    let outside = rig.outside.join("stations.toml");
    fs::write(&outside, "the NAS\n").unwrap();
    fs::create_dir_all(rig.at("/var/lib/ritornello/plugins/radio")).unwrap();
    let link = rig.at("/var/lib/ritornello/plugins/radio/stations.toml");
    let env = plant_later(&link, &outside);
    let (plan, archives) = place_into_radio();
    let run = rig.run(&plan, &archives, &env);
    assert!(run.logged("systemctl stop ritornello.service"));
    assert!(!run.ok);
    assert!(run.stderr.contains("a symbolic link"), "{}", run.stderr);
    assert!(link.is_symlink(), "the link was refused, not replaced");
    assert_eq!(fs::read_to_string(&outside).unwrap(), "the NAS\n");
}

#[test]
fn a_link_planted_after_the_checks_still_stops_a_file_removal() {
    let rig = Rig::new();
    let sentinel = rig.outside.join("etc/plugins.toml");
    fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    fs::write(&sentinel, "the NAS\n").unwrap();
    fs::create_dir_all(rig.at("/etc")).unwrap();
    let env = plant_later(&rig.at("/etc/ritornello"), &rig.outside.join("etc"));
    let plan = Plan { stop_service: true, remove_plugins_toml: true, ..Plan::default() };
    let run = rig.run(&plan, &BTreeMap::new(), &env);
    assert!(run.logged("systemctl stop ritornello.service"));
    assert!(!run.ok);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "the NAS\n");
}

// --- R29: shapes, checked again by the script ----------------------------

/// `render` refuses these words already; the script's own check is proved
/// by editing a rendered script, the one way such a word could reach it.
#[test]
fn the_script_refuses_a_foreign_unit_or_mount_root_itself() {
    let base = remove_files_plugin();
    let script = render(&base).unwrap();
    for (from, to) in [
        ("'ritornello-media-mount.service'", "'ssh.service'"),
        ("'ritornello-media-mount.service'", "'ritornello-A.service'"),
        ("'/mnt/ritornello'", "'/mnt/nas'"),
        ("'/mnt/ritornello'", "'/mnt/ritornello/a/b'"),
        ("'/mnt/ritornello'", "'/mnt/ritornello/nas'"),
    ] {
        let rig = Rig::new();
        plant_files_plugin(&rig);
        let edited = script.replace(from, to);
        assert_ne!(edited, script);
        let run = rig.run_script(&base, &edited, &BTreeMap::new(), &[]);
        assert!(!run.ok, "{to}");
        assert!(run.stderr.contains("refusing"), "{to}: {}", run.stderr);
        assert!(run.changed_nothing(), "{to}: {}", run.log);
    }
}

// --- Fix round 1: races, packs, stops ------------------------------------

/// The environment the hook needs to run `todo` at `at` (`<point> <path>`).
fn hook_at(at: &str, todo: String) -> Vec<(&'static str, String)> {
    vec![("HOOK_AT", at.to_string()), ("HOOK_DO", todo)]
}

fn q(p: &Path) -> String {
    sh_quote(&p.display().to_string())
}

/// `radio`'s data, and a sentinel standing for the NAS share of the same
/// name, where a swapped `plugins` would lead.
fn radio_and_nas(rig: &Rig) -> PathBuf {
    rig.write("/var/lib/ritornello/plugins/radio/state.toml", "radio's own\n");
    let nas = rig.outside.join("radio/Album/01.flac");
    fs::create_dir_all(nas.parent().unwrap()).unwrap();
    fs::write(&nas, "the NAS\n").unwrap();
    nas
}

fn swap_plugins_for_the_nas(rig: &Rig) -> String {
    let plugins = rig.at("/var/lib/ritornello/plugins");
    let real = rig.at("/var/lib/ritornello/plugins.real");
    format!("mv {} {} && ln -s {} {}", q(&plugins), q(&real), q(&rig.outside), q(&plugins))
}

/// I2: swapped between the last `nolink` and the removal, just before the
/// directory is entered: `pwd -P` sees where `cd -P` really went.
#[test]
fn a_swap_before_the_pin_stops_the_tree_removal() {
    let rig = Rig::new();
    let nas = radio_and_nas(&rig);
    let env = hook_at("pin /var/lib/ritornello/plugins", swap_plugins_for_the_nas(&rig));
    let run = rig.run(&erase_radio(), &BTreeMap::new(), &env);
    assert!(!run.ok);
    assert!(run.stderr.contains("now leads elsewhere"), "{}", run.stderr);
    assert_eq!(fs::read_to_string(&nas).unwrap(), "the NAS\n");
    assert!(rig.at("/var/lib/ritornello/plugins.real/radio/state.toml").exists());
    assert!(run.says_the_service_is_stopped(), "{}", run.stderr);
}

/// I2: swapped after the directory was entered and checked: the removal
/// is relative to the pinned directory, so it still removes the real one.
#[test]
fn a_swap_after_the_pin_cannot_redirect_the_tree_removal() {
    let rig = Rig::new();
    let nas = radio_and_nas(&rig);
    let env = hook_at("removing /var/lib/ritornello/plugins/radio", swap_plugins_for_the_nas(&rig));
    let run = rig.run(&erase_radio(), &BTreeMap::new(), &env);
    assert!(run.ok, "{}", run.stderr);
    assert!(rig.at("/var/lib/ritornello/plugins").is_symlink(), "the swap did happen");
    assert_eq!(fs::read_to_string(&nas).unwrap(), "the NAS\n");
    assert!(!rig.at("/var/lib/ritornello/plugins.real/radio").exists(), "the pinned directory's radio was removed");
}

/// I1: the temporary name swapped for a link between `install` and
/// `chown`: `chown -h` changes the link, never what it points at.
#[test]
fn a_temporary_name_swapped_for_a_link_is_never_chowned_through() {
    let rig = Rig::new();
    let shadow = rig.outside.join("shadow");
    fs::write(&shadow, "root's secret\n").unwrap();
    let new = rig.at("/etc/ritornello/plugins.toml.ritornello-new");
    let env = hook_at("installed /etc/ritornello/plugins.toml", format!("rm -f {} && ln -s {} {}", q(&new), q(&shadow), q(&new)));
    let plan = Plan { plugins_toml: Some(plugins_toml()), ..Plan::default() };
    let run = rig.run(&plan, &BTreeMap::new(), &env);
    assert!(run.logged("chown -h ritornello:ritornello $R/etc/ritornello/plugins.toml.ritornello-new"), "{}", run.log);
    assert!(!run.followed(), "{}", run.log);
    assert_eq!(fs::read_to_string(&shadow).unwrap(), "root's secret\n");
}

/// I1: a directory `mkdirs` has just created, swapped for a link before
/// its `chown`: the link is chowned, not its target, and the next level
/// refuses to go through it.
#[test]
fn a_created_directory_swapped_for_a_link_is_never_chowned_through() {
    let rig = Rig::new();
    let target = rig.outside.join("etc-systemd");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(rig.at("/var/lib/ritornello")).unwrap();
    let created = rig.at("/var/lib/ritornello/plugins");
    let env = hook_at("created /var/lib/ritornello/plugins", format!("rmdir {} && ln -s {} {}", q(&created), q(&target), q(&created)));
    let (plan, archives) = place_into_radio();
    let run = rig.run(&plan, &archives, &env);
    assert!(!run.ok);
    assert!(run.logged("chown -h ritornello:ritornello $R/var/lib/ritornello/plugins"), "{}", run.log);
    assert!(!run.followed(), "{}", run.log);
    assert!(fs::read_dir(&target).unwrap().next().is_none(), "nothing was written through the link");
}

/// A `.tar.gz` holding one member whose name the `tar` crate would refuse
/// to write: the header is filled by hand.
fn tar_with_raw_name(name: &str) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut b = tar::Builder::new(gz);
    let body = b"x\n";
    let mut h = tar::Header::new_gnu();
    h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(tar::EntryType::Regular);
    h.set_cksum();
    b.append(&h, &body[..]).unwrap();
    let mut gz = b.into_inner().unwrap();
    gz.flush().unwrap();
    gz.finish().unwrap()
}

/// I3: a pack holding a `../` member is refused before anything changes,
/// not halfway through step 9 with the old pack gone.
#[test]
fn a_pack_member_named_outside_its_directory_is_refused_before_anything_changes() {
    for name in ["../escaped.toml", "./a/../../escaped.toml"] {
        let rig = Rig::new();
        let plan = Plan {
            ensure_user: true,
            archives: [FR.to_string()].into(),
            packs: vec![(FR.into(), "ritornello-lang-fr".into())],
            ..Plan::default()
        };
        let run = rig.run(&plan, &[(FR.to_string(), tar_with_raw_name(name))].into(), &[]);
        assert!(!run.ok, "{name}");
        assert!(run.stderr.contains("named outside its own directory"), "{name}: {}", run.stderr);
        assert!(run.changed_nothing(), "{}", run.log);
        assert!(rig.tree().is_empty(), "{:?}", rig.tree());
    }
}

/// I3: a link planted where the pack goes, after its old directory was
/// removed: the pack is prepared elsewhere and renamed over the link,
/// never written through it.
#[test]
fn a_link_planted_at_the_pack_s_place_is_never_written_through() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let elsewhere = rig.outside.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let dest = rig.at("/etc/ritornello/language-packs/ritornello-lang-fr");
    let env = hook_at("pin /etc/ritornello/language-packs", format!("ln -s {} {}", q(&elsewhere), q(&dest)));
    let run = rig.run(&plan, &archives, &env);
    assert!(run.ok, "{}", run.stderr);
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing was written through the link");
    assert!(!dest.is_symlink() && dest.is_dir());
    assert_eq!(rig.read("/etc/ritornello/language-packs/ritornello-lang-fr/pack.toml"), "language = \"fr\"\n");
}

/// I3: the stage swapped for a directory the account opened: the pack is
/// not extracted into it.
#[test]
fn a_pack_stage_replaced_while_being_prepared_is_refused() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let stage = rig.at("/etc/ritornello/language-packs/.ritornello-lang-fr.ritornello-new");
    let env = hook_at("staged ritornello-lang-fr", format!("rmdir {s} && mkdir -m 0777 {s}", s = q(&stage)));
    let run = rig.run(&plan, &archives, &env);
    assert!(!run.ok);
    assert!(run.stderr.contains("replaced while it was being prepared"), "{}", run.stderr);
    assert!(!stage.join("pack.toml").exists());
}

/// R41: a stop that did not stop the core stops the script, before
/// anything is removed.
#[test]
fn a_service_still_running_after_stop_stops_the_script() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &[("SHIM_ACTIVE", "1".into())]);
    assert!(!run.ok);
    assert!(run.stderr.contains("still running after stop"), "{}", run.stderr);
    assert_eq!(rig.read("/usr/local/lib/ritornello/plugins/ritornello-plugin-files"), "old\n");
    assert!(!run.log.contains("disable"), "{}", run.log);
}

/// M3: a share that does not answer stops the script, naming it, instead
/// of hanging the ssh session.
#[test]
fn a_share_that_does_not_answer_times_out_by_name() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
    let run = rig.run(
        &remove_files_plugin(),
        &BTreeMap::new(),
        &[("SHIM_MOUNTPOINT_HANG", "*/nas".into()), ("RITORNELLO_INSTALL_TEST_TIMEOUT", "1".into())],
    );
    assert!(!run.ok);
    assert!(run.stderr.contains("timed out on mountpoint -q") && run.stderr.contains("/mnt/ritornello/nas"), "{}", run.stderr);
    assert_eq!(rig.read("/usr/local/lib/ritornello/plugins/ritornello-plugin-files"), "old\n");
}

/// R56: the same for the root itself: it is probed before anything changes,
/// and a root that does not answer stops the run there, naming it.
#[test]
fn a_mount_root_that_does_not_answer_times_out_before_anything_changes() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
    let run = rig.run(
        &remove_files_plugin(),
        &BTreeMap::new(),
        &[("SHIM_MOUNTPOINT_HANG", "*/mnt/ritornello".into()), ("RITORNELLO_INSTALL_TEST_TIMEOUT", "1".into())],
    );
    assert!(!run.ok);
    assert!(run.stderr.contains("timed out on mountpoint -q ./ritornello"), "{}", run.stderr);
    assert!(run.changed_nothing(), "{}", run.log);
    assert!(rig.at("/mnt/ritornello/nas").is_dir());
}

/// M1: a member check that could not run is a refusal, not a pass.
#[test]
fn a_member_check_that_cannot_run_refuses() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[("SHIM_AWK_FAIL", "1".into())]);
    assert!(!run.ok);
    assert!(run.stderr.contains("could not be checked"), "{}", run.stderr);
    assert!(run.changed_nothing(), "{}", run.log);
}

/// R42: a run that stops at its first placement leaves a registry that
/// already records everything the run was about to place.
#[test]
fn the_registry_is_written_before_the_first_placement() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[("SHIM_CHOWN_FAIL", "ritornello-core.ritornello-new".into())]);
    assert!(!run.ok);
    assert!(!rig.at("/usr/local/bin/ritornello-core").exists());
    assert_eq!(rig.read("/var/lib/ritornello-install/installed.toml"), plan.provisional_registry.as_ref().unwrap().render());
}

// --- Fix round 2: residuals A, B, C and N1 --------------------------------

/// A pack archive built with the `tar` crate, with a `./` entry of the
/// given mode and files of the given mode.
fn pack_tar(dot_mode: u32, file_mode: u32) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut b = tar::Builder::new(gz);
    let mut h = tar::Header::new_gnu();
    h.set_size(0);
    h.set_mode(dot_mode);
    h.set_entry_type(tar::EntryType::Directory);
    b.append_data(&mut h, "./", &[][..]).unwrap();
    for (name, body) in [("./pack.toml", "language = \"fr\"\n"), ("./core.toml", "hello = \"bonjour\"\n")] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(file_mode);
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, name, body.as_bytes()).unwrap();
    }
    let mut gz = b.into_inner().unwrap();
    gz.flush().unwrap();
    gz.finish().unwrap()
}

fn pack_only(archive: Vec<u8>) -> (Plan, BTreeMap<String, Vec<u8>>) {
    let plan = Plan {
        ensure_user: true,
        archives: [FR.to_string()].into(),
        packs: vec![(FR.into(), "ritornello-lang-fr".into())],
        ..Plan::default()
    };
    (plan, [(FR.to_string(), archive)].into())
}

/// A: a stage a crashed run left behind, whatever pack it was for, is
/// removed before the fresh one is made, so it can never be adopted.
#[test]
fn a_stale_stage_from_a_crashed_run_is_removed_first() {
    let rig = Rig::new();
    for stale in [".ritornello-lang-fr.ritornello-new", ".ritornello-lang-de.ritornello-new"] {
        let d = rig.at(&format!("/etc/ritornello/language-packs/{stale}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("left-behind.toml"), "stale\n").unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (plan, archives) = pack_only(release_tar(&[
        ("pack.toml".into(), "language = \"fr\"\n".into()),
        ("core.toml".into(), "hello = \"bonjour\"\n".into()),
    ]));
    let run = rig.run(&plan, &archives, &[]);
    assert!(run.ok, "{}", run.stderr);
    assert!(!rig.at("/etc/ritornello/language-packs/ritornello-lang-fr/left-behind.toml").exists());
    assert!(rig.at("/etc/ritornello/language-packs/ritornello-lang-fr/pack.toml").exists());
    assert!(!rig.at("/etc/ritornello/language-packs/.ritornello-lang-de.ritornello-new").exists());
}

/// A: a stale stage swapped in for the fresh one, root's and 0700 like it,
/// is refused because it is not empty.
#[test]
fn a_stage_swapped_for_a_stale_one_is_refused() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let stage = rig.at("/etc/ritornello/language-packs/.ritornello-lang-fr.ritornello-new");
    let env = hook_at(
        "staged ritornello-lang-fr",
        format!("rmdir {s} && mkdir -m 0700 {s} && echo stale > {s}/left-behind.toml", s = q(&stage)),
    );
    let run = rig.run(&plan, &archives, &env);
    assert!(!run.ok);
    assert!(run.stderr.contains("(not empty)"), "{}", run.stderr);
    assert!(!rig.at("/etc/ritornello/language-packs/ritornello-lang-fr").exists());
}

/// B: the archive's own `./` never widens the stage while root extracts
/// into it.
#[test]
fn the_archive_s_own_directory_mode_never_widens_the_stage() {
    let rig = Rig::new();
    let (plan, archives) = pack_only(pack_tar(0o755, 0o644));
    let stage = rig.at("/etc/ritornello/language-packs/.ritornello-lang-fr.ritornello-new");
    let seen = rig.base.join("stage-mode");
    let env = hook_at("extracted ritornello-lang-fr", format!("ls -ldn {} > {}", q(&stage), q(&seen)));
    let run = rig.run(&plan, &archives, &env);
    assert!(run.ok, "{}", run.stderr);
    let mode = fs::read_to_string(&seen).unwrap();
    assert!(mode.starts_with("drwx------"), "{mode}");
    assert_eq!(rig.read("/etc/ritornello/language-packs/ritornello-lang-fr/pack.toml"), "language = \"fr\"\n");
}

/// B: a member writable by group or others is refused before anything
/// changes, a directory as well as a file.
#[test]
fn a_member_writable_by_group_or_others_is_refused_before_anything_changes() {
    for (dot, file) in [(0o755, 0o666), (0o755, 0o620), (0o777, 0o644)] {
        let rig = Rig::new();
        let (plan, archives) = pack_only(pack_tar(dot, file));
        let run = rig.run(&plan, &archives, &[]);
        assert!(!run.ok, "{dot:o} {file:o}");
        assert!(run.stderr.contains("writable by group or others"), "{dot:o} {file:o}: {}", run.stderr);
        assert!(run.changed_nothing(), "{}", run.log);
        assert!(rig.tree().is_empty(), "{:?}", rig.tree());
    }
}

/// C: the stage must belong to whoever runs the script. A uid other than
/// the real one, injected through the test hook, stands for a stage the
/// account made: the check refuses it.
#[test]
fn a_stage_owned_by_anyone_else_is_refused() {
    let rig = Rig::new();
    let (plan, archives) = fresh_install();
    let run = rig.run(&plan, &archives, &[("HOOK_UID", "4242".into())]);
    assert!(!run.ok);
    assert!(run.stderr.contains("replaced while it was being prepared"), "{}", run.stderr);
    assert!(!rig.at("/etc/ritornello/language-packs/ritornello-lang-fr").exists());
}

/// N1: a `timeout` that could not run its command is not an answer: the
/// script stops, naming the mount point, instead of skipping the share.
#[test]
fn a_mount_point_check_that_cannot_run_stops_the_script() {
    let rig = Rig::new();
    plant_files_plugin(&rig);
    fs::create_dir_all(rig.at("/mnt/ritornello/nas")).unwrap();
    let run = rig.run(&remove_files_plugin(), &BTreeMap::new(), &[("SHIM_TIMEOUT_RC", "125".into())]);
    assert!(!run.ok);
    // Nothing can be probed, so the very first probe of the verify phase
    // (the mount root's parent) is where the run stops.
    assert!(run.stderr.contains("could not run") && run.stderr.contains("/mnt"), "{}", run.stderr);
    assert!(rig.at("/mnt/ritornello/nas").is_dir());
    assert_eq!(rig.read("/usr/local/lib/ritornello/plugins/ritornello-plugin-files"), "old\n");
}

#[test]
fn sh_quote_survives_dash() {
    for s in ["plain", "it's", "'", "''", "$(touch /tmp/x)", "`id`", "a b\tc", "new\nline", "\\", "\"", "*"] {
        let out = Command::new(shell()).arg("-c").arg(format!("printf %s {}", sh_quote(s))).output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), s);
    }
}
