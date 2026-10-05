//! The device-side script: a computed `Plan`, rendered as the POSIX `sh`
//! that root runs once on the device, and bundled into a tar beside every
//! file it places.
//!
//! The script runs in two phases. The **verify** phase changes nothing on
//! the device: it re-checks the shape of every unit and mount root, walks
//! every path the script will touch looking for a symbolic link, checks
//! every archive's members, and extracts the component archives into the
//! bundle to prove every file to place is really there. Only then do the
//! fourteen steps run, in a fixed order: the account exists before it is
//! given a file, the service is stopped before its binary is replaced, and
//! nothing is placed before everything that must go has gone.
//!
//! Every word interpolated into the script goes through `sh_quote`, and
//! every one of them has first been checked here, in Rust, against the same
//! rules `names` applies: the script is generated from an inventory and a
//! device survey, and neither is trusted to be well formed.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail, ensure};

use crate::names::{self, DATA_ROOT, PACKS_ROOT, PLUGINS_TOML, REGISTRY};
use crate::plan::Plan;

/// The two directory trees that belong to the unprivileged `ritornello`
/// account. A file owned by that account may only land inside them; a
/// directory the script creates inside them is given to that account.
const ACCOUNT_TREES: &[&str] = &["/etc/ritornello", "/var/lib/ritornello"];

/// The only owners a placed file may have.
const OWNERS: &[&str] = &["root:root", "ritornello:ritornello"];

/// The only modes a placed file may have: the inventory's own two.
const MODES: &[&str] = &["0644", "0755"];

/// Where every mount point of the files plugin lives.
const MOUNT_ROOT: &str = "/mnt/ritornello";

/// Single-quotes a word for POSIX sh.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

const PRELUDE: &str = r#"#!/bin/sh
# Written by ritornello-install, run once on the device, as root.
set -eu
# RITORNELLO_INSTALL_ROOT exists for the tests only: they run this script
# against a temporary root. The installer's remote command sets it empty
# explicitly (Task 14), and both test hooks below are inert while it is.
R="${RITORNELLO_INSTALL_ROOT:-}"
B="${RITORNELLO_INSTALL_BUNDLE:?}"
say() { printf 'ritornello-install: %s\n' "$*"; }
die() { printf 'ritornello-install: %s\n' "$*" >&2; exit 1; }
# Every helper stops the script with `exit`, never `return`, and POSIX sh
# has no `local`, hence the prefixed variable names.
#
# Both roots are resolved once, physically: every later check compares a
# physical working directory against "$R<plan path>".
if [ -n "$R" ]; then R=$(cd -P -- "$R" && pwd -P) || die "cannot enter the test root"; fi
B=$(cd -P -- "$B" && pwd -P) || die "cannot enter the bundle"
# Test hooks, inert unless RITORNELLO_INSTALL_ROOT is set: a command run at
# named points, so a test can swap a path exactly where a racing process of
# the account would, and a shorter timeout.
hook() {
  if [ -n "$R" ] && [ -n "${RITORNELLO_INSTALL_TEST_HOOK:-}" ]; then "$RITORNELLO_INSTALL_TEST_HOOK" "$@"; fi
}
# A value the test hook may replace (it prints the one to use), so a test
# can reach a branch a real run never would, such as a foreign owner.
hook_value() {
  if [ -n "$R" ] && [ -n "${RITORNELLO_INSTALL_TEST_HOOK:-}" ]; then "$RITORNELLO_INSTALL_TEST_HOOK" value "$@"; else printf '%s\n' "$2"; fi
}
T=10
if [ -n "$R" ]; then T=${RITORNELLO_INSTALL_TEST_TIMEOUT:-10}; fi
# A run that stops after the service was stopped says so, and how to bring
# the radio back, whatever stopped it.
STOPPED=
on_exit() {
  _ex_rc=$?
  if [ "$_ex_rc" -ne 0 ] && [ -n "$STOPPED" ]; then
    printf '%s\n' "ritornello-install: ritornello.service is stopped and this run did not finish; fix the cause above and run the installer again, or restart the version still in place with: systemctl start ritornello.service" >&2
  fi
}
trap on_exit EXIT

# /etc/ritornello and /var/lib/ritornello belong to the unprivileged
# account, which can plant a symbolic link anywhere below them, or swap one
# in while this script runs: a `plugins -> /mnt/ritornello` would turn
# root's `rm -rf` into a delete of the NAS, and a followed `chown` would
# give the account any file. Two defences:
#
# - `nolink` walks a path from `/`, component by component, the last one
#   included, and refuses any link: an early, readable refusal;
# - `pin` enters a directory with `cd -P` and checks with `pwd -P` that it
#   is really "$R<dir>", reached without a link. Every removal, placement
#   and chown then acts on a relative `./name` from that pinned directory,
#   so a swap of any ancestor after the check changes nothing: the working
#   directory is an inode, not a path. Every `chown` is `-h`, so it never
#   follows the last component either.
nolink() {
  case $1 in /*) ;; *) die "refusing $1: not an absolute path" ;; esac
  _nl_rest=${1#/}
  _nl_at=
  while [ -n "$_nl_rest" ]; do
    case $_nl_rest in
      */*) _nl_seg=${_nl_rest%%/*}; _nl_rest=${_nl_rest#*/} ;;
      *) _nl_seg=$_nl_rest; _nl_rest= ;;
    esac
    case $_nl_seg in ''|.|..) die "refusing $1: not a plain path" ;; esac
    _nl_at=$_nl_at/$_nl_seg
    if [ -L "$R$_nl_at" ]; then die "refusing $1: $_nl_at is a symbolic link"; fi
  done
}
pin() {
  if [ "$1" = / ]; then _pn_want=${R:-/}; else _pn_want=$R$1; fi
  hook pin "$1"
  cd -P -- "$_pn_want" || die "refusing $1: cannot enter it"
  [ "$(pwd -P)" = "$_pn_want" ] || die "refusing $1: a symbolic link now leads elsewhere"
  hook pinned "$1"
}
# Creates the missing directories of an absolute path, one component at a
# time, each from its pinned parent. `mkdir -m` sets the mode at creation,
# whatever the umask, so no `chmod` ever acts on a name. Under the
# account's two trees a created directory is the account's, elsewhere
# root's; an existing one is left as it is.
mkdirs() {
  nolink "$1"
  _md_rest=${1#/}
  _md_at=
  while [ -n "$_md_rest" ]; do
    case $_md_rest in
      */*) _md_seg=${_md_rest%%/*}; _md_rest=${_md_rest#*/} ;;
      *) _md_seg=$_md_rest; _md_rest= ;;
    esac
    pin "${_md_at:-/}"
    _md_at=$_md_at/$_md_seg
    if [ -L "./$_md_seg" ]; then die "refusing $1: $_md_at is a symbolic link"; fi
    if [ -d "./$_md_seg" ]; then continue; fi
    mkdir -m 0755 "./$_md_seg"
    hook created "$_md_at"
    case $_md_at in
      /etc/ritornello|/etc/ritornello/*|/var/lib/ritornello|/var/lib/ritornello/*)
        chown -h ritornello:ritornello "./$_md_seg" ;;
      *) chown -h root:root "./$_md_seg" ;;
    esac
  done
  cd /
}
# Gives an existing directory to an owner, from its pinned parent.
own() {
  pin "$(dirname "$2")"
  _ow_name=${2##*/}
  if [ -L "./$_ow_name" ]; then die "refusing $2: a symbolic link"; fi
  chown -h "$1" "./$_ow_name"
  cd /
}
# Every write goes to a temporary name and is renamed into place, so a power
# cut never leaves half a binary or half a unit behind. Mode and owner come
# from the plan, never from the archive: `install -m` on a name it has just
# created, `chown -h` on that temporary name, before the rename. `mv -T`
# never treats the destination as a directory to move into.
put() {
  _pt_dir=$(dirname "$2")
  _pt_name=${2##*/}
  mkdirs "$_pt_dir"
  pin "$_pt_dir"
  if [ -L "./$_pt_name" ]; then die "refusing $2: a symbolic link"; fi
  if [ -d "./$_pt_name" ]; then die "refusing to place $2: a directory is in the way"; fi
  say "placing $2"
  rm -f "./$_pt_name.ritornello-new"
  install -m "$3" "$1" "./$_pt_name.ritornello-new"
  hook installed "$2"
  chown -h "$4" "./$_pt_name.ritornello-new"
  mv -fT "./$_pt_name.ritornello-new" "./$_pt_name"
  cd /
}
# Removals, from the pinned parent. A parent that is not there holds
# nothing to remove. `--one-file-system` is a second belt: a mount found
# inside a removed tree stops the removal instead of emptying the share.
rmfile() {
  nolink "$1"
  if [ ! -d "$R$(dirname "$1")" ]; then return 0; fi
  pin "$(dirname "$1")"
  say "removing $1"
  rm -f "./${1##*/}"
  cd /
}
rmtree() {
  nolink "$1"
  if [ ! -d "$R$(dirname "$1")" ]; then return 0; fi
  pin "$(dirname "$1")"
  _rt_name=${1##*/}
  if [ -L "./$_rt_name" ]; then die "refusing $1: a symbolic link"; fi
  say "removing $1"
  hook removing "$1"
  rm -rf --one-file-system "./$_rt_name"
  cd /
}
# Mount points are unmounted, then removed with rmdir, never recursively: a
# recursive delete of a mounted share would delete the NAS's content. An
# unmount that fails, or a mount point that is not empty once unmounted,
# stops everything. Anything that touches a mount point runs under
# `timeout`: a dead network share can block a stat forever.
bounded() {
  _bd_rc=0
  timeout "$T" "$@" || _bd_rc=$?
  # 124 is a timeout; 125, 126, 127 and 137 mean the command itself could
  # not run or was killed. None of them is an answer, and taking one for
  # "not mounted" or "not a directory" would skip a share unseen.
  case $_bd_rc in
    124) die "timed out on $*: a share that does not answer; stopping, no file was removed or placed" ;;
    125|126|127|137) die "could not run $* (exit $_bd_rc); stopping, no file was removed or placed" ;;
  esac
  return "$_bd_rc"
}
# A root that is itself a mount point (a share or a bind mount placed right
# on it) is refused before anything under it is touched: what `unmount_under`
# would find below it is the NAS's own top level, and its `rmdir` would
# delete the NAS's empty directories. Probed from the pinned parent. util-linux
# `mountpoint` exits 0 for a mount point and 32 for a plain directory; 1 is a
# failure it does not tell from "no" (busybox uses it for "no"), so 1 is read
# as "not a mount point" and the later `ls -A` and `rmdir` are the fallback.
# Any other status, and a timeout, is no answer and stops.
mount_free() {
  nolink "$1"
  _mf_name=${1##*/}
  bounded test -d "$R$(dirname "$1")" || return 0
  pin "$(dirname "$1")"
  if bounded test -L "./$_mf_name"; then die "refusing $1: a symbolic link"; fi
  if ! bounded test -d "./$_mf_name"; then cd /; return 0; fi
  _mf_rc=0
  bounded mountpoint -q "./$_mf_name" || _mf_rc=$?
  cd /
  case $_mf_rc in
    0) die "refusing $1: it is itself a mount point; unmount it first. Stopping, nothing was changed" ;;
    1|32) ;;
    *) die "cannot tell whether $1 is a mount point (mountpoint exit $_mf_rc); stopping, nothing was changed" ;;
  esac
}
# On a total removal, whatever sits directly in the root must be a directory
# `unmount_under` will unmount and remove: a file, a link or a hidden entry
# would stop the run at its very end, after the service was stopped.
root_clean() {
  nolink "$1"
  bounded test -d "$R$1" || return 0
  for _rc_e in "$R$1"/* "$R$1"/.[!.]* "$R$1"/..?*; do
    if ! bounded test -e "$_rc_e" && ! bounded test -L "$_rc_e"; then continue; fi
    case ${_rc_e##*/} in
      .*) die "refusing $1: it holds ${_rc_e##*/}, which is not a share directory; stopping, nothing was changed" ;;
    esac
    if bounded test -L "$_rc_e"; then die "refusing $1: ${_rc_e##*/} is a symbolic link; stopping, nothing was changed"; fi
    bounded test -d "$_rc_e" || die "refusing $1: it holds ${_rc_e##*/}, which is not a share directory; stopping, nothing was changed"
  done
}
unmount_under() {
  nolink "$1"
  mount_free "$1"
  for m in "$R$1"/*; do
    if bounded test -L "$m"; then die "refusing $m: a symbolic link"; fi
    bounded test -d "$m" || continue
    if bounded mountpoint -q "$m"; then
      bounded umount "$m" || die "cannot unmount $m; stopping, no file was removed or placed"
    fi
    bounded rmdir "$m" || die "cannot remove $m: not empty, not mounted; stopping, no file was removed or placed"
  done
}
# The mount root itself goes last, on a total removal only, and with `rmdir`:
# a recursive delete of a root that is still a mounted share would delete the
# NAS's content. Still a mount point, or not empty, stops everything. A root
# that is already absent holds nothing to remove.
rmroot() {
  nolink "$1"
  mount_shape "$1"
  _rr_name=${1##*/}
  mount_free "$1"
  bounded test -d "$R$(dirname "$1")" || return 0
  pin "$(dirname "$1")"
  if bounded test -L "./$_rr_name"; then die "refusing $1: a symbolic link"; fi
  if ! bounded test -d "./$_rr_name"; then cd /; return 0; fi
  hook checking_root "$1"
  _rr_left=$(bounded ls -A "./$_rr_name") || die "cannot list $1; stopping"
  if [ -n "$_rr_left" ]; then die "cannot remove $1: it is not empty; stopping"; fi
  hook removing_root "$1"
  bounded rmdir "./$_rr_name" || die "cannot remove $1: rmdir refused it; stopping"
  cd /
}
# A language pack is extracted as root into a fresh directory no one else
# can enter, on the destination's own file system, then renamed into place
# and only then given to the account: the account never sees the tree
# before root has finished writing it, and root never writes through
# anything the account placed. The bundle cannot serve as that directory:
# it lives in /tmp, a tmpfs on DietPi, and a move from there is a copy.
pack() {
  rmtree "$PACKS/$2"
  mkdirs "$PACKS"
  pin "$PACKS"
  _pk_stage=".$2.ritornello-new"
  # Every stage a crashed run left behind goes first, whatever pack it was
  # for: root-owned and 0700, one of them would pass the check below if
  # the account swapped it in for the fresh one. A link is removed as a
  # link, by name.
  for _pk_old in ./.ritornello-lang-*.ritornello-new; do
    if [ -L "$_pk_old" ]; then rm -f "$_pk_old"; continue; fi
    [ -e "$_pk_old" ] || continue
    rmtree "$PACKS/${_pk_old#./}"
    pin "$PACKS"
  done
  mkdir -m 0700 "./$_pk_stage"
  hook staged "$2"
  cd -P -- "./$_pk_stage" || die "refusing $PACKS/$_pk_stage: cannot enter it"
  [ "$(pwd -P)" = "$R$PACKS/$_pk_stage" ] || die "refusing $PACKS/$_pk_stage: a symbolic link now leads elsewhere"
  # Still the directory created above: this script's own, closed to
  # everyone else, and empty.
  set -- "$1" "$2" "$(ls -ldn . | awk '{ print $1 " " $3 }')" "$(hook_value uid "$(id -u)")"
  case $3 in "drwx------ $4"|"drwx------+ $4"|"drwx------. $4") ;; *) die "refusing $PACKS/$_pk_stage: replaced while it was being prepared" ;; esac
  if [ -n "$(ls -A .)" ]; then die "refusing $PACKS/$_pk_stage: replaced while it was being prepared (not empty)"; fi
  # `--no-overwrite-dir`: the archive's own `./` never changes the stage's
  # mode, so it stays 0700 until root has finished.
  tar -xzf "$B/$1" --no-same-owner --no-overwrite-dir
  hook extracted "$2"
  chown -hR ritornello:ritornello .
  chmod 0755 .
  pin "$PACKS"
  # Whatever non-directory the account left at the pack's name (a link,
  # say) is removed by name, never followed; `mv -T` would refuse it.
  if [ -L "./$2" ] || [ -f "./$2" ]; then rm -f "./$2"; fi
  mv -fT "./$_pk_stage" "./$2"
  cd /
}
PACKS=/etc/ritornello/language-packs
# Verify-phase checks: none of them changes the device.
unit_shape() {
  case $1 in
    ritornello.service) ;;
    ritornello-*.service)
      _us=${1#ritornello-}; _us=${_us%.service}
      case $_us in ''|*[!a-z0-9-]*) die "refusing unit $1: not one of Ritornello's" ;; esac ;;
    *) die "refusing unit $1: not one of Ritornello's" ;;
  esac
}
mount_shape() {
  [ "$1" = /mnt/ritornello ] || die "refusing mount root $1: not /mnt/ritornello"
}
placeable() {
  nolink "$1"
  if [ -d "$R$1" ]; then die "refusing to place $1: a directory is in the way"; fi
}
# Every member of an archive is a plain file or a directory, named relative
# and without a `..` segment: a link member could make a later copy read,
# or an extraction write, outside its directory, and GNU tar skipping a
# `..` member exits 2 halfway through. awk's own failure is told apart
# from a refusal: a check that could not run is not a check that passed.
members() {
  tar -tvzf "$B/$1" > "$B/.members" || die "refusing $1: cannot list it"
  tar -tzf "$B/$1" > "$B/.names" || die "refusing $1: cannot list it"
  _mb_rc=0
  awk 'substr($0, 1, 1) != "-" && substr($0, 1, 1) != "d" { bad = 1 } END { exit bad ? 3 : 0 }' "$B/.members" || _mb_rc=$?
  case $_mb_rc in
    0) ;;
    3) die "refusing $1: it holds a member that is neither a file nor a directory" ;;
    *) die "refusing $1: its members could not be checked" ;;
  esac
  awk '/^\// || /(^|\/)\.\.(\/|$)/ { bad = 1 } END { exit bad ? 3 : 0 }' "$B/.names" || _mb_rc=$?
  case $_mb_rc in
    0) ;;
    3) die "refusing $1: it holds a member named outside its own directory" ;;
    *) die "refusing $1: its members could not be checked" ;;
  esac
  # No member writable by group or others: between root's extraction and
  # the handover, nobody but root may write into the tree.
  awk 'substr($1, 6, 1) == "w" || substr($1, 9, 1) == "w" { bad = 1 } END { exit bad ? 3 : 0 }' "$B/.members" || _mb_rc=$?
  case $_mb_rc in
    0) ;;
    3) die "refusing $1: it holds a member writable by group or others" ;;
    *) die "refusing $1: its members could not be checked" ;;
  esac
}
need() {
  if [ ! -f "$1" ] || [ -L "$1" ]; then die "missing from the bundle: $1"; fi
}
# Members may be named `./usr/...` or `usr/...`: extracted into a directory
# of their own, both land at the same path below it. Language packs are
# extracted too, into the bundle, only to prove they extract cleanly before
# anything changes; step 9 extracts them again, into a stage beside their
# destination (see `pack`).
extract() { mkdir -p "$B/$2/$1"; tar -xzf "$B/$1" -C "$B/$2/$1" --no-same-owner; }
"#;

/// Renders `plan` as the script root runs on the device, or refuses a plan
/// holding a word the script must never see.
pub fn render(plan: &Plan) -> anyhow::Result<String> {
    check(plan)?;
    let q = sh_quote;
    // `"$B"/x/'<archive>'/'<path>'`: one shell word, whatever the quotes.
    let src = |archive: &str, path: &str| format!("\"$B\"/x/{}/{}", q(archive), q(path));
    let data_dir = |plugin: &str| format!("{DATA_ROOT}/{plugin}");
    let pack_dir = |id: &str| format!("{PACKS_ROOT}/{id}");

    let mut out = String::from(PRELUDE);
    let mut line = |l: String| {
        out.push_str(&l);
        out.push('\n');
    };

    // --- Verify. Nothing in this phase changes the device.
    line(String::new());
    line("# Verify: nothing below changes the device until all of it has passed.".into());
    line("say 'checking the plan against the device'".into());
    for u in plan.disable_units.iter().chain(&plan.enable_units) {
        line(format!("unit_shape {}", q(u)));
    }
    for m in plan.unmount_roots.iter().chain(&plan.remove_mount_roots) {
        line(format!("mount_shape {}", q(m)));
    }
    // A root that is itself a mount stops the run before anything changes,
    // and a total removal refuses what it could not remove at the very end.
    let roots: BTreeSet<&String> = plan.unmount_roots.iter().chain(&plan.remove_mount_roots).collect();
    for m in roots {
        line(format!("mount_free {}", q(m)));
    }
    for m in &plan.remove_mount_roots {
        line(format!("root_clean {}", q(m)));
    }
    let mut touched: BTreeSet<String> = BTreeSet::new();
    touched.extend(plan.unmount_roots.iter().cloned());
    touched.extend(plan.remove_mount_roots.iter().cloned());
    touched.extend(plan.remove_files.iter().cloned());
    touched.extend(plan.remove_trees.iter().cloned());
    for i in &plan.initial {
        touched.insert(data_dir(&i.plugin));
        touched.insert(format!("{}/{}", data_dir(&i.plugin), i.target));
    }
    for (_, id) in &plan.packs {
        touched.insert(pack_dir(id));
        touched.insert(format!("{PACKS_ROOT}/.{id}.ritornello-new"));
    }
    if plan.remove_plugins_toml {
        touched.insert(PLUGINS_TOML.to_string());
    }
    if plan.remove_registry {
        touched.insert(REGISTRY.to_string());
    }
    for t in &touched {
        line(format!("nolink {}", q(t)));
    }
    let mut placed: BTreeSet<&str> = plan.puts.iter().map(|p| p.dest.as_str()).collect();
    if plan.plugins_toml.is_some() {
        placed.insert(PLUGINS_TOML);
    }
    if plan.registry.is_some() || plan.provisional_registry.is_some() {
        placed.insert(REGISTRY);
    }
    for p in &placed {
        line(format!("placeable {}", q(p)));
    }
    for a in &plan.archives {
        line(format!("members {}", q(a)));
    }
    // The component archives, into `x/`; the packs, into `p/`, only to
    // prove they extract cleanly before anything changes.
    let extracted: BTreeSet<&str> = plan
        .puts
        .iter()
        .map(|p| p.archive.as_str())
        .chain(plan.initial.iter().map(|i| i.archive.as_str()))
        .collect();
    for a in &extracted {
        line(format!("extract {} x", q(a)));
    }
    for (a, _) in &plan.packs {
        line(format!("extract {} p", q(a)));
    }
    for p in &plan.puts {
        line(format!("need {}", src(&p.archive, &p.archive_path)));
    }
    for i in &plan.initial {
        line(format!("need {}", src(&i.archive, &i.archive_path)));
    }
    for (a, _) in &plan.packs {
        line(format!("need \"$B\"/{}", q(a)));
    }
    if plan.plugins_toml.is_some() {
        line("need \"$B\"/'plugins.toml'".into());
    }
    if plan.registry.is_some() {
        line("need \"$B\"/'installed.toml'".into());
    }
    if plan.provisional_registry.is_some() {
        line("need \"$B\"/'installed.provisional.toml'".into());
    }

    // --- The fourteen steps, in their fixed order.
    line(String::new());
    line("# 1. The account, before anything is given to it.".into());
    if plan.ensure_user {
        line(
            "id -u ritornello >/dev/null 2>&1 || useradd --system --home-dir /var/lib/ritornello \
             --no-create-home --shell /usr/sbin/nologin ritornello"
                .into(),
        );
    }
    line("# 2. The service, stopped before its binary is replaced.".into());
    if plan.stop_service {
        line("STOPPED=1".into());
        line("systemctl stop ritornello.service || true".into());
        // A stop that failed must not let root work under a running core
        // (R41). A unit that does not exist yet is not active.
        line(
            "if systemctl is-active --quiet ritornello.service; then die 'ritornello.service is still running after stop; stopping'; fi"
                .into(),
        );
    }
    line("# 3.".into());
    for u in &plan.disable_units {
        line(format!("systemctl disable --now {} || true", q(u)));
    }
    line("# 4. Shares, unmounted before anything is removed.".into());
    for m in &plan.unmount_roots {
        line(format!("unmount_under {}", q(m)));
    }
    for m in &plan.remove_mount_roots {
        line(format!("rmroot {}", q(m)));
    }
    line("# 5. Everything that goes, before anything is placed.".into());
    for f in &plan.remove_files {
        line(format!("rmfile {}", q(f)));
    }
    for t in &plan.remove_trees {
        line(format!("rmtree {}", q(t)));
    }
    line("# 6. The component archives were extracted by the verify phase.".into());
    line("# 6.5. Recorded before placed: a run stopping from here on leaves a registry that knows every file.".into());
    if plan.provisional_registry.is_some() {
        line(format!("put \"$B\"/'installed.provisional.toml' {} '0644' 'root:root'", q(REGISTRY)));
    }
    line("# 7.".into());
    for p in &plan.puts {
        line(format!("put {} {} {} {}", src(&p.archive, &p.archive_path), q(&p.dest), q(&p.mode), q(&p.owner)));
    }
    line("# 8. Initial configuration, written only where none exists.".into());
    for i in &plan.initial {
        let dir = data_dir(&i.plugin);
        let target = format!("{dir}/{}", i.target);
        line(format!("mkdirs {}", q(&dir)));
        line(format!("own ritornello:ritornello {}", q(&dir)));
        line(format!(
            "if [ ! -e \"$R\"{} ]; then put {} {} '0644' 'ritornello:ritornello'; fi",
            q(&target),
            src(&i.archive, &i.archive_path),
            q(&target)
        ));
    }
    line("# 9. Language packs, each replaced whole.".into());
    for (archive, id) in &plan.packs {
        line(format!("pack {} {}", q(archive), q(id)));
    }
    line("# 10.".into());
    if plan.plugins_toml.is_some() {
        line(format!("put \"$B\"/'plugins.toml' {} '0644' 'ritornello:ritornello'", q(PLUGINS_TOML)));
    } else if plan.remove_plugins_toml {
        line(format!("rmfile {}", q(PLUGINS_TOML)));
    }
    line("# 11.".into());
    if plan.registry.is_some() {
        line(format!("put \"$B\"/'installed.toml' {} '0644' 'root:root'", q(REGISTRY)));
    } else if plan.remove_registry {
        line(format!("rmfile {}", q(REGISTRY)));
    }
    line("# 12.".into());
    line("systemctl daemon-reload".into());
    for u in &plan.enable_units {
        line(format!("systemctl enable {}", q(u)));
    }
    if plan.start_service {
        line("systemctl restart ritornello.service".into());
        line("STOPPED=".into());
    }
    line("# 13.".into());
    if plan.remove_user {
        line("userdel ritornello || true".into());
    }
    line("# 14.".into());
    line("say done".into());
    Ok(out)
}

/// Bundles the script with everything it reads from `$B`: `apply.sh`,
/// `plugins.toml` and `installed.toml` when the plan writes them, and every
/// archive the plan names, taken from `archives`. Uncompressed: the archives
/// inside already are.
pub fn bundle(plan: &Plan, script: &str, archives: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    let mut b = tar::Builder::new(Vec::new());
    append(&mut b, "apply.sh", script.as_bytes(), 0o755)?;
    if let Some(text) = &plan.plugins_toml {
        append(&mut b, "plugins.toml", text.as_bytes(), 0o644)?;
    }
    if let Some(registry) = &plan.registry {
        append(&mut b, "installed.toml", registry.render().as_bytes(), 0o644)?;
    }
    if let Some(registry) = &plan.provisional_registry {
        append(&mut b, "installed.provisional.toml", registry.render().as_bytes(), 0o644)?;
    }
    for a in &plan.archives {
        ensure!(bare_archive(a), "refusing archive name {a:?}");
        let bytes = archives.get(a).with_context(|| format!("the plan needs {a}, which was not downloaded"))?;
        append(&mut b, a, bytes, 0o644)?;
    }
    b.into_inner().context("finishing the bundle")
}

fn append(b: &mut tar::Builder<Vec<u8>>, name: &str, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(bytes.len() as u64);
    h.set_mode(mode);
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h.set_entry_type(tar::EntryType::Regular);
    b.append_data(&mut h, name, bytes).with_context(|| format!("adding {name} to the bundle"))
}

/// A release archive's own file name, bare, as it lands in `$B`.
fn bare_archive(a: &str) -> bool {
    a.ends_with(".tar.gz")
        && a.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && a.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// A path inside an archive, or below a plugin's data directory.
fn clean_relative(p: &str) -> bool {
    names::clean(&format!("/{p}"))
}

/// Exactly the one root the release ships (R40): a sub-root that is itself
/// a mounted share would have `unmount_under` `rmdir` the NAS's own empty
/// directories.
fn mount_root_shape(m: &str) -> bool {
    m == MOUNT_ROOT
}

fn in_account_tree(path: &str) -> bool {
    ACCOUNT_TREES.iter().any(|t| path.strip_prefix(t).is_some_and(|r| r.starts_with('/')))
}

/// Everything the script will interpolate, checked before a line of it is
/// written. The plan already checks its removals; they are checked again
/// here because this is the last place a word can be refused.
fn check(plan: &Plan) -> anyhow::Result<()> {
    for u in plan.disable_units.iter().chain(&plan.enable_units) {
        ensure!(names::valid_ritornello_unit_name(u), "refusing unit {u:?}: not one of Ritornello's");
    }
    for m in plan.unmount_roots.iter().chain(&plan.remove_mount_roots) {
        ensure!(mount_root_shape(m), "refusing mount root {m:?}: not {MOUNT_ROOT}");
    }
    for f in &plan.remove_files {
        ensure!(names::deletable_file(f), "refusing to remove {f:?}");
    }
    for t in &plan.remove_trees {
        ensure!(names::deletable_tree(t), "refusing to remove {t:?}");
    }
    for a in &plan.archives {
        ensure!(bare_archive(a), "refusing archive name {a:?}");
    }
    let known = |a: &str| -> anyhow::Result<()> {
        ensure!(plan.archives.contains(a), "{a:?} is used but not among the plan's archives");
        Ok(())
    };
    for p in &plan.puts {
        known(&p.archive)?;
        ensure!(clean_relative(&p.archive_path), "refusing archive path {:?}", p.archive_path);
        // Only what the installer could also remove: Ritornello's own places.
        ensure!(names::deletable_file(&p.dest), "refusing to place {:?}", p.dest);
        ensure!(MODES.contains(&p.mode.as_str()), "refusing mode {:?} for {:?}", p.mode, p.dest);
        ensure!(OWNERS.contains(&p.owner.as_str()), "refusing owner {:?} for {:?}", p.owner, p.dest);
        // A file the account owns outside its own trees is a file it can
        // replace; root runs some of them.
        if p.owner != "root:root" && !in_account_tree(&p.dest) {
            bail!("refusing owner {:?} for {:?}: outside the account's own trees", p.owner, p.dest);
        }
    }
    for i in &plan.initial {
        known(&i.archive)?;
        ensure!(clean_relative(&i.archive_path), "refusing archive path {:?}", i.archive_path);
        ensure!(names::valid_plugin_name(&i.plugin), "refusing plugin name {:?}", i.plugin);
        ensure!(clean_relative(&i.target), "refusing initial configuration target {:?}", i.target);
    }
    for (a, id) in &plan.packs {
        known(a)?;
        let language = id.strip_prefix("ritornello-lang-").unwrap_or("");
        ensure!(names::valid_language(language), "refusing language pack {id:?}");
    }
    Ok(())
}

// Linux only, not every Unix: the script is written for the device, and it
// relies on GNU coreutils and util-linux (`mv -T`, `rm --one-file-system`,
// `timeout`), which macOS does not have — run there, these tests measure the
// workstation's userland, not the script. Measured: 38 of them failed on the
// macOS leg of the `installer` job, and none elsewhere.
#[cfg(all(test, target_os = "linux"))]
mod remote_script;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Initial, Put};

    fn put(dest: &str, mode: &str, owner: &str) -> Put {
        Put {
            archive: "core.tar.gz".into(),
            archive_path: dest.trim_start_matches('/').into(),
            dest: dest.into(),
            mode: mode.into(),
            owner: owner.into(),
        }
    }

    fn base() -> Plan {
        Plan {
            archives: ["core.tar.gz".to_string()].into(),
            puts: vec![put("/usr/local/bin/ritornello-core", "0755", "root:root")],
            ..Plan::default()
        }
    }

    fn refused(plan: &Plan) -> String {
        render(plan).expect_err("must be refused").to_string()
    }

    #[test]
    fn a_plain_plan_renders() {
        render(&base()).expect("renders");
    }

    #[test]
    fn a_unit_outside_ritornello_s_namespace_is_refused_both_ways() {
        for bad in ["ssh.service", "ritornellofoo.service", "ritornello-.service", "ritornello-A.service", "ritornello-x.timer"] {
            let mut p = base();
            p.disable_units = vec![bad.into()];
            assert!(refused(&p).contains("unit"), "disable {bad}");
            let mut p = base();
            p.enable_units = vec![bad.into()];
            assert!(refused(&p).contains("unit"), "enable {bad}");
        }
        let mut p = base();
        p.enable_units = vec!["ritornello.service".into(), "ritornello-media-mount.service".into()];
        render(&p).expect("Ritornello's own units render");
    }

    #[test]
    fn a_mount_root_off_its_shape_is_refused() {
        for bad in ["/mnt", "/mnt/nas", "/mnt/ritornello/", "/mnt/ritornello/nas-1", "/mnt/ritornello/a/b", "/mnt/ritornello/A", "/mnt/ritornellox", "/mnt/ritornello/.."] {
            let mut p = base();
            p.unmount_roots = vec![bad.into()];
            assert!(refused(&p).contains("mount root"), "{bad}");
        }
        let mut p = base();
        p.unmount_roots = vec!["/mnt/ritornello".into()];
        render(&p).expect("the one root the release ships renders");
        for bad in ["/mnt", "/mnt/nas", "/mnt/ritornello/", "/mnt/ritornello/nas", "/mnt/ritornello/.."] {
            let mut p = base();
            p.remove_mount_roots = vec![bad.into()];
            assert!(refused(&p).contains("mount root"), "remove {bad}");
        }
        let mut p = base();
        p.remove_mount_roots = vec!["/mnt/ritornello".into()];
        render(&p).expect("the one root renders for removal too");
    }

    #[test]
    fn a_placement_outside_ritornello_s_places_is_refused() {
        for bad in ["/etc/shadow", "/usr/local/bin/other", "/etc/systemd/system/ssh.service"] {
            let mut p = base();
            p.puts = vec![put(bad, "0644", "root:root")];
            assert!(refused(&p).contains("refusing to place"), "{bad}");
        }
    }

    #[test]
    fn a_mode_or_an_owner_the_inventory_never_uses_is_refused() {
        let mut p = base();
        p.puts = vec![put("/usr/local/bin/ritornello-core", "4755", "root:root")];
        assert!(refused(&p).contains("mode"));
        let mut p = base();
        // Inside the account's own tree, so only the owner list can refuse it.
        p.puts = vec![put("/etc/ritornello/input-presets/mce.toml", "0644", "pi:pi")];
        assert!(refused(&p).contains("refusing owner \"pi:pi\""));
    }

    /// The account owning a binary root runs is the account replacing it.
    #[test]
    fn the_account_owns_files_only_inside_its_own_trees() {
        for bad in ["/usr/local/lib/ritornello/ritornello-update", "/etc/systemd/system/ritornello.service", "/var/lib/ritornello-install/installed.toml"] {
            let mut p = base();
            p.puts = vec![put(bad, "0644", "ritornello:ritornello")];
            assert!(refused(&p).contains("outside the account's own trees"), "{bad}");
        }
        for ok in ["/etc/ritornello/input-presets/mce.toml", "/var/lib/ritornello/plugins/radio/x.toml"] {
            let mut p = base();
            p.puts = vec![put(ok, "0644", "ritornello:ritornello")];
            render(&p).expect(ok);
        }
    }

    #[test]
    fn an_archive_word_the_bundle_cannot_hold_is_refused() {
        for bad in ["../core.tar.gz", "a/core.tar.gz", ".core.tar.gz", "core.zip", "co'e.tar.gz", "core$.tar.gz"] {
            let mut p = base();
            p.archives = [bad.to_string()].into();
            p.puts[0].archive = bad.into();
            assert!(refused(&p).contains("archive name"), "{bad}");
        }
        let mut p = base();
        p.puts[0].archive = "other.tar.gz".into();
        assert!(refused(&p).contains("not among the plan's archives"));
        let mut p = base();
        p.puts[0].archive_path = "../../etc/shadow".into();
        assert!(refused(&p).contains("archive path"));
    }

    #[test]
    fn an_initial_configuration_or_a_pack_off_its_shape_is_refused() {
        let init = |plugin: &str, target: &str| Initial {
            archive: "core.tar.gz".into(),
            archive_path: "initial-config/x.toml".into(),
            plugin: plugin.into(),
            target: target.into(),
        };
        for (plugin, target) in [("../x", "a.toml"), ("radio", "../../../etc/passwd"), ("radio", "")] {
            let mut p = base();
            p.initial = vec![init(plugin, target)];
            assert!(render(&p).is_err(), "{plugin} {target}");
        }
        let mut p = base();
        p.initial = vec![init("radio", "stations.toml")];
        render(&p).expect("a plain initial configuration renders");
        for bad in ["ritornello-lang-", "ritornello-lang-../x", "fr", "ritornello-lang--fr"] {
            let mut p = base();
            p.packs = vec![("core.tar.gz".into(), bad.into())];
            assert!(refused(&p).contains("language pack"), "{bad}");
        }
    }

    #[test]
    fn removals_are_checked_again_here() {
        let mut p = base();
        p.remove_files = vec!["/etc/passwd".into()];
        assert!(refused(&p).contains("refusing to remove"));
        let mut p = base();
        p.remove_trees = vec!["/mnt/ritornello".into()];
        assert!(refused(&p).contains("refusing to remove"));
    }

    /// The verify phase precedes step 1, and the steps run in their fixed
    /// order: account, stop, disable, unmount, remove, place, initial,
    /// packs, plugins.toml, registry, systemd, account removal.
    #[test]
    fn the_steps_are_rendered_in_their_fixed_order() {
        let mut p = base();
        p.archives.insert("ritornello-lang-fr-1.tar.gz".into());
        p.ensure_user = true;
        p.stop_service = true;
        p.disable_units = vec!["ritornello-media-mount.service".into()];
        p.unmount_roots = vec!["/mnt/ritornello".into()];
        p.remove_mount_roots = vec!["/mnt/ritornello".into()];
        p.remove_files = vec!["/etc/polkit-1/rules.d/51-ritornello-media.rules".into()];
        p.remove_trees = vec!["/var/lib/ritornello/plugins/files".into()];
        p.initial = vec![Initial {
            archive: "core.tar.gz".into(),
            archive_path: "initial-config/stations.example.toml".into(),
            plugin: "radio".into(),
            target: "stations.toml".into(),
        }];
        p.packs = vec![("ritornello-lang-fr-1.tar.gz".into(), "ritornello-lang-fr".into())];
        p.plugins_toml = Some(String::new());
        p.registry = Some(crate::registry::Registry { format: 1, components: BTreeMap::new() });
        p.provisional_registry = Some(crate::registry::Registry { format: 1, components: BTreeMap::new() });
        p.enable_units = vec!["ritornello.service".into()];
        p.start_service = true;
        p.remove_user = true;
        let s = render(&p).unwrap();
        let body = &s[PRELUDE.len()..];
        let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("{needle} missing"));
        let order = [
            "extract 'ritornello-lang-fr-1.tar.gz' p",
            "need \"$B\"/x/'core.tar.gz'/'usr/local/bin/ritornello-core'",
            "need \"$B\"/'installed.provisional.toml'",
            "useradd",
            "STOPPED=1",
            "systemctl stop ritornello.service",
            "systemctl is-active --quiet ritornello.service",
            "systemctl disable --now 'ritornello-media-mount.service'",
            "unmount_under '/mnt/ritornello'",
            "rmroot '/mnt/ritornello'",
            "rmfile '/etc/polkit-1/rules.d/51-ritornello-media.rules'",
            "rmtree '/var/lib/ritornello/plugins/files'",
            "put \"$B\"/'installed.provisional.toml'",
            "put \"$B\"/x/'core.tar.gz'/'usr/local/bin/ritornello-core'",
            "mkdirs '/var/lib/ritornello/plugins/radio'",
            "pack 'ritornello-lang-fr-1.tar.gz' 'ritornello-lang-fr'",
            "put \"$B\"/'plugins.toml'",
            "put \"$B\"/'installed.toml'",
            "systemctl daemon-reload",
            "systemctl enable 'ritornello.service'",
            "systemctl restart ritornello.service",
            "STOPPED=\n",
            "userdel ritornello",
            "say done",
        ];
        for w in order.windows(2) {
            assert!(at(w[0]) < at(w[1]), "{} must come before {}", w[0], w[1]);
        }
    }

    #[test]
    fn the_bundle_holds_the_script_and_what_it_reads() {
        let mut p = base();
        p.plugins_toml = Some("# plugins\n".into());
        p.registry = Some(crate::registry::Registry { format: 1, components: BTreeMap::new() });
        p.provisional_registry = Some(crate::registry::Registry { format: 1, components: BTreeMap::new() });
        let archives: BTreeMap<String, Vec<u8>> = [("core.tar.gz".to_string(), b"gz".to_vec())].into();
        let bytes = bundle(&p, "#!/bin/sh\n", &archives).unwrap();
        let mut a = tar::Archive::new(bytes.as_slice());
        let names: Vec<String> =
            a.entries().unwrap().map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["apply.sh", "plugins.toml", "installed.toml", "installed.provisional.toml", "core.tar.gz"]);
        let err = bundle(&p, "", &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("not downloaded"), "{err}");
        let mut none = base();
        none.plugins_toml = None;
        let bytes = bundle(&none, "", &archives).unwrap();
        assert_eq!(tar::Archive::new(bytes.as_slice()).entries().unwrap().count(), 2);
    }

    /// The device runs this text with `sh`, and a `\r` before each newline
    /// turns every command into one with a trailing carriage return --
    /// `then\r` is not `then`. The script is a string inside a `.rs` file,
    /// and a Windows checkout with `core.autocrlf=true` writes that file
    /// with CRLF: rustc normalises line endings of the source it reads, but
    /// this test is what says so on the platform where the risk is born
    /// (the `installer` job of `ci.yml` runs it on Windows).
    #[test]
    fn the_device_script_carries_no_carriage_return() {
        assert!(!PRELUDE.contains('\r'));
        assert!(!render(&base()).unwrap().contains('\r'));
    }
}
