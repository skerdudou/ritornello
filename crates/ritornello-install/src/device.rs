//! The read-only survey the installer runs on the device before it asks
//! anything, and the parser of that survey's output.
//!
//! `probe_script` is plain POSIX `sh`: it runs over ssh against a target
//! that may not be a Pi at all and owes the installer nothing already in
//! place. `parse` turns its output — framed section by section with a
//! per-run nonce so no line a file happens to contain can be mistaken for a
//! section boundary — into a `DeviceState` the rest of the installer reads.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use serde::Deserialize;

use crate::registry::Registry;

/// The read-only survey the installer runs first, before asking anything.
///
/// Plain POSIX sh and base tools only: the target may not be a Pi and owes
/// us nothing installed. Every section is framed by a per-run nonce, so no
/// line a file happens to contain can be mistaken for a section boundary.
///
/// `m()` prints a *leading* newline before its own marker, not just a
/// trailing one: `cat`'s output for `plugins.toml` or `installed.toml`
/// carries whatever bytes the file has, and a hand-edited or
/// non-newline-terminated file (`printf` without a trailing `\n`, `vim`
/// with `noeol`, a write cut short) leaves its last line unterminated. Only
/// a trailing `m() { printf '@@%s@@ %s\n' ...; }` would then have its
/// marker glued onto that dangling line with no separating `\n` at all,
/// which `sections` cannot recognise as a header (it matches whole lines),
/// so the section after it never gets an entry and a fully successful
/// transfer reads as truncated. The leading `\n` guarantees the next
/// marker always starts its own line whatever the previous writer left
/// behind — it either terminates a dangling line (no trailing `\n` on the
/// device's file) or opens a harmless blank line after an already-complete
/// one (the ordinary case). `sections` and `parse` tolerate that extra
/// blank line: scalar sections are read through `.trim()`, `PACKS`/`DATA`
/// are filtered for non-empty lines, and a trailing blank line inside TOML
/// content parses the same as none.
///
/// A file's content section is always followed by its own `*_STATE`
/// section (R36): `absent`, `present` (empty or not) or `unreadable`. The
/// state is taken from `cat`'s own exit status, after the content, so a
/// read that failed partway never passes for a complete one; an unreadable
/// file used to read as nothing at all, which an update then overwrote.
///
/// A file that does not stat is absent only if `gone` can prove it (R38):
/// behind a directory that cannot be searched, `[ -e ]` and `[ -L ]` are
/// both false exactly as for a missing file. `gone` walks up to the nearest
/// ancestor that stats: a directory there must be searchable for the
/// absence below it to be true; anything else, and the file is unreadable.
///
/// Because every marker opens with `\n`, a file's section body is always
/// exactly its bytes plus that one `\n` — whether or not the file ended in a
/// newline — which is what lets `parse` hand the plan the file byte for
/// byte (see `file_content`).
///
/// Every file is read through `head -c`, at most one byte past
/// `READ_CAP`: a file the account points one of these paths at, however
/// large, costs the installer that much memory and no more, and the one
/// byte too many is what lets `file_content` tell a file over the cap from
/// one exactly at it — and refuse it rather than parse a truncated prefix.
///
/// The registry's owner and mode follow its state (`REGISTRY_OWNER`, from
/// `stat -L`, so a symbolic link is judged by the file it reaches): the
/// registry is trusted only when root owns it and neither its group nor
/// anyone else may write it (see `registry_trusted`).
pub fn probe_script(nonce: &str) -> String {
    format!(
        r#"R="${{RITORNELLO_INSTALL_ROOT:-}}"
m() {{ printf '\n@@{n}@@ %s\n' "$1"; }}
m KERNEL; uname -s
m MACHINE; uname -m
m SYSTEMD; [ -d "$R/run/systemd/system" ] && echo yes || echo no
m UID; id -u
m SUDO
if [ "$(id -u)" = 0 ]; then echo not-needed
elif ! command -v sudo >/dev/null 2>&1; then echo absent
elif sudo -n true 2>/dev/null; then echo no-password
else echo password; fi
m CORE; [ -x "$R/usr/local/bin/ritornello-core" ] && echo yes || echo no
m USER; id -u ritornello >/dev/null 2>&1 && echo yes || echo no
gone() {{
  p="${{1%/*}}"
  while [ -n "$p" ]; do
    if [ -d "$p" ]; then [ -x "$p" ]; return; fi
    if [ -e "$p" ] || [ -L "$p" ]; then return 0; fi
    p="${{p%/*}}"
  done
}}
rd() {{
  if [ ! -e "$1" ] && [ ! -L "$1" ]; then
    if gone "$1"; then s=absent; else s=unreadable; fi
  elif [ -f "$1" ] && head -c {over} "$1" 2>/dev/null; then s=present
  else s=unreadable; fi
}}
m PLUGINS_TOML; rd "$R/etc/ritornello/plugins.toml"
m PLUGINS_TOML_STATE; echo "$s"
m REGISTRY; rd "$R/var/lib/ritornello-install/installed.toml"
m REGISTRY_STATE; echo "$s"
m REGISTRY_OWNER; stat -L -c '%u %a' "$R/var/lib/ritornello-install/installed.toml" 2>/dev/null || true
m PLACED; rd "$R/var/lib/ritornello/staging/placed.json"
m PLACED_STATE; echo "$s"
m PACKS; ls -1 "$R/etc/ritornello/language-packs" 2>/dev/null || true
m DATA
for d in "$R"/var/lib/ritornello/plugins/*/; do
  [ -d "$d" ] || continue
  [ -n "$(ls -A "$d" 2>/dev/null)" ] && basename "$d"
done
m END
"#,
        n = nonce,
        over = READ_CAP + 1
    )
}

/// The most the survey reads of any one file: far above any real
/// `plugins.toml`, `installed.toml` or `placed.json` (a few kilobytes).
pub const READ_CAP: usize = 1024 * 1024;

/// Whether `stat -L -c '%u %a'`'s answer for the registry says root owns
/// it and neither its group nor others may write it. Anything else — the
/// account's own file, a mode with `g+w` or `o+w`, no answer at all — is a
/// registry the account may have written, which may never justify leaving
/// a component alone (`plan::is_current`): it is then read as absent, and
/// everything is placed again.
fn registry_trusted(owner: &str) -> bool {
    let mut parts = owner.split_whitespace();
    let (Some(uid), Some(mode), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    uid == "0" && u32::from_str_radix(mode, 8).is_ok_and(|m| m & 0o022 == 0)
}

/// One `[[plugin]]` block from the device's `plugins.toml` — `name` and
/// `exec` only. The other keys (`args`, and whatever a later plugin adds)
/// are the plugin's own business; `declared` never reads them, and no
/// `deny_unknown_fields` refuses them either.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Declared {
    pub name: String,
    pub exec: String,
}

/// The shape `plugins.toml` is read through: an array of `[[plugin]]`
/// blocks, nothing else asked of the file.
#[derive(Debug, Default, Deserialize)]
struct PluginsToml {
    #[serde(default)]
    plugin: Vec<Declared>,
}

/// Whether the device needs `sudo` at all, and whether asking for one will
/// prompt for a password. `NoPassword` is the common case on the real
/// device: DietPi's `dietpi` account has passwordless sudo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sudo {
    NotNeeded,
    NoPassword,
    Password,
    Absent,
}

/// Everything the read-only survey found, before the installer asks
/// anything of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceState {
    pub kernel: String,
    pub machine: String,
    pub systemd: bool,
    pub uid: u32,
    pub sudo: Sudo,
    pub core_present: bool,
    pub plugins_toml: Option<String>,
    pub declared: Vec<Declared>,
    pub registry: Option<Registry>,
    /// The device has an `installed.toml`, but not one root alone could
    /// have written (`registry_trusted`): `registry` is `None` for it.
    pub registry_ignored: bool,
    /// What the core's in-app updater says it last placed, component name
    /// to version, from `/var/lib/ritornello/staging/placed.json`.
    ///
    /// **Untrusted**: that file belongs to the unprivileged `ritornello`
    /// account, which can write anything into it. It is read only because
    /// the updater moves binaries without touching the root-owned registry,
    /// so it is the one place a registry that has fallen behind shows. It
    /// may therefore only ever make the plan reinstall a component, never
    /// skip one (see `plan::is_current`). Empty when the file is absent,
    /// unreadable or does not parse: that only ever means "no evidence".
    pub updater_placed: BTreeMap<String, String>,
    pub packs: BTreeSet<String>,
    pub data_nonempty: BTreeSet<String>,
    pub user_exists: bool,
}

/// One entry of the updater's `placed.json`: only its version is read.
/// Every other key (`not_installed_files`, and whatever a later core adds)
/// is ignored rather than refused — the file is evidence for doing more,
/// and a reader that refused it would throw that evidence away.
#[derive(Debug, Deserialize)]
struct PlacedEntry {
    version: String,
}

/// The updater's memory, or nothing. Whatever is wrong with the file — a
/// state other than `present`, bytes that are not the expected JSON — the
/// answer is an empty memory: the core itself reads a corrupt `placed.json`
/// the same way (`update::placed::read`), and an empty memory here only
/// means the registry alone decides, which is the trusted half anyway.
fn updater_placed(body: &str, state: &str) -> BTreeMap<String, String> {
    if state.trim() != "present" {
        return BTreeMap::new();
    }
    let text = body.strip_suffix('\n').unwrap_or(body);
    if text.len() > READ_CAP {
        return BTreeMap::new();
    }
    serde_json::from_str::<BTreeMap<String, PlacedEntry>>(text)
        .map(|m| m.into_iter().map(|(name, entry)| (name, entry.version)).collect())
        .unwrap_or_default()
}

impl DeviceState {
    /// The archive-naming label for this device's architecture, or `None`
    /// for one the installer has no archive for.
    pub fn arch_label(&self) -> Option<&'static str> {
        match self.machine.as_str() {
            "armv7l" => Some("armv7"),
            "aarch64" => Some("arm64"),
            "x86_64" => Some("x86_64"),
            _ => None,
        }
    }

    /// Whether nothing Ritornello has ever installed is on this device: no
    /// core binary, and no plugin declared in `plugins.toml` either.
    pub fn is_fresh(&self) -> bool {
        !self.core_present && self.declared.is_empty()
    }
}

/// The sections `probe_script` always frames, in the order it emits them.
/// Every one of them must be found, or the output is treated as truncated
/// rather than as a device that simply has nothing to report: a section
/// cut off reads the same as an empty one, and only the header line marks
/// the difference. Whether a file is absent, empty or unreadable is its
/// `*_STATE` section's to say, never its content's.
///
/// Nothing here validates the *names* this survey reads back — `declared`
/// plugin names, `packs`, `data_nonempty` directory names. That is
/// `names.rs`'s job (`valid_plugin_name`, `valid_language`), and it must run
/// on every one of these strings before any of them forms a path; `parse`
/// only reconstructs what the device reported, untrusted.
const SECTIONS: &[&str] = &[
    "KERNEL",
    "MACHINE",
    "SYSTEMD",
    "UID",
    "SUDO",
    "CORE",
    "USER",
    "PLUGINS_TOML",
    "PLUGINS_TOML_STATE",
    "REGISTRY",
    "REGISTRY_STATE",
    "REGISTRY_OWNER",
    "PLACED",
    "PLACED_STATE",
    "PACKS",
    "DATA",
    "END",
];

/// Splits `output` on `probe_script`'s own `@@<nonce>@@ <SECTION>` markers.
/// A section `SECTIONS` names but the split never reached is refused by
/// name — that is what a transfer cut short in the middle looks like — and
/// so is a marker naming a section this parser does not know, since that
/// can only mean `probe_script` and `parse` have drifted apart.
fn sections<'a>(output: &'a str, nonce: &str) -> anyhow::Result<BTreeMap<&'a str, String>> {
    let prefix = format!("@@{nonce}@@ ");
    let mut found: BTreeMap<&'a str, String> = BTreeMap::new();
    let mut current: Option<&'a str> = None;
    for line in output.split('\n') {
        if let Some(name) = line.strip_prefix(prefix.as_str()) {
            let name = name.trim_end_matches('\r');
            anyhow::ensure!(
                SECTIONS.contains(&name),
                "device survey emitted an unknown section {name:?}"
            );
            found.entry(name).or_default();
            current = Some(name);
        } else if let Some(name) = current {
            let body = found.get_mut(name).expect("inserted when this section's header line was read, above");
            body.push_str(line);
            body.push('\n');
        }
    }
    for name in SECTIONS.iter().copied() {
        anyhow::ensure!(
            found.contains_key(name),
            "device survey is missing the {name} section — output looks truncated"
        );
    }
    Ok(found)
}

/// One file the survey `cat`s, from its content section and its state
/// section (R36): `None` when it is absent, `Some` of its exact bytes when
/// present — `Some("")` for an empty file, which is not an absent one — and
/// a named error when it exists but could not be read, rather than a device
/// that seems to have none (an update would then overwrite it).
///
/// The body is the file's bytes plus the one `\n` the next marker opens
/// with (see `probe_script`); exactly that one is taken off, so a file with
/// no trailing newline and one with it come back as they are.
fn file_content(file: &str, body: &str, state: &str) -> anyhow::Result<Option<String>> {
    let content = body.strip_suffix('\n').unwrap_or(body);
    // Over the cap, only a prefix was read: refused by name rather than
    // parsed, since a `plugins.toml` cut at a block boundary would still
    // parse — as a device declaring fewer plugins than it does.
    anyhow::ensure!(
        content.len() <= READ_CAP,
        "{file} on the device is larger than {READ_CAP} bytes, which no file of Ritornello's ever is: \
         refusing to read part of it; inspect it by hand"
    );
    match state.trim() {
        "absent" => {
            anyhow::ensure!(
                content.is_empty(),
                "device survey reports {file} absent but carries content for it"
            );
            Ok(None)
        }
        "present" => Ok(Some(content.to_string())),
        "unreadable" => anyhow::bail!(
            "{file} exists on the device but could not be read: refusing to treat it as absent"
        ),
        other => anyhow::bail!("device survey's state for {file} is unrecognised: {other:?}"),
    }
}

/// Parses the read-only survey's output into a `DeviceState`.
///
/// Refuses, by a named error rather than by reading the device as empty:
/// output missing a section (a truncated transfer), and a `plugins.toml`
/// that does not parse as TOML — a broken file must never read as "nothing
/// installed", or the installer would go on to remove everything it finds.
pub fn parse(output: &str, nonce: &str) -> anyhow::Result<DeviceState> {
    let sections = sections(output, nonce)?;
    let get = |name: &str| sections.get(name).expect("checked present by `sections` above").as_str();

    let kernel = get("KERNEL").trim().to_string();
    let machine = get("MACHINE").trim().to_string();
    let systemd = get("SYSTEMD").trim() == "yes";
    let uid: u32 =
        get("UID").trim().parse().context("device survey's UID section is not a number")?;
    let sudo = match get("SUDO").trim() {
        "not-needed" => Sudo::NotNeeded,
        "no-password" => Sudo::NoPassword,
        "password" => Sudo::Password,
        "absent" => Sudo::Absent,
        other => anyhow::bail!("device survey's SUDO section is unrecognised: {other:?}"),
    };
    let core_present = get("CORE").trim() == "yes";
    let user_exists = get("USER").trim() == "yes";

    let plugins_toml = file_content("plugins.toml", get("PLUGINS_TOML"), get("PLUGINS_TOML_STATE"))?;
    let declared = match &plugins_toml {
        None => Vec::new(),
        Some(text) => {
            let parsed: PluginsToml =
                toml::from_str(text).context("plugins.toml on the device does not parse")?;
            parsed.plugin
        }
    };

    // A registry present but empty is refused like any other that does not
    // parse: the installer always writes `format`, and a registry read as
    // empty would forget every privileged file it records.
    // A registry root does not own, or others may write, is read as absent:
    // never parsed, never trusted, so nothing is skipped on its word and
    // everything is placed again (which also rewrites it as root's own).
    let registry_present = get("REGISTRY_STATE").trim() == "present";
    let registry_ignored = registry_present && !registry_trusted(get("REGISTRY_OWNER"));
    let registry = file_content("installed.toml", get("REGISTRY"), get("REGISTRY_STATE"))?
        .filter(|_| !registry_ignored)
        .map(|text| {
            Registry::parse(&text).context(
                "installed.toml on the device does not parse: restore it from a backup, or remove it \
                 to let ritornello-install treat the device as unrecorded (privileged files it placed \
                 will then not be removed)",
            )
        })
        .transpose()?;
    let updater_placed = updater_placed(get("PLACED"), get("PLACED_STATE"));

    let packs: BTreeSet<String> =
        get("PACKS").lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    let data_nonempty: BTreeSet<String> =
        get("DATA").lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();

    Ok(DeviceState {
        kernel,
        machine,
        systemd,
        uid,
        sudo,
        core_present,
        plugins_toml,
        declared,
        registry,
        registry_ignored,
        updater_placed,
        packs,
        data_nonempty,
        user_exists,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `DeviceState`, for tests that only care about one field
    /// (`arch_label`) and would otherwise have to spell out all twelve.
    fn minimal(machine: &str) -> DeviceState {
        DeviceState {
            kernel: String::new(),
            machine: machine.to_string(),
            systemd: false,
            uid: 0,
            sudo: Sudo::Absent,
            core_present: false,
            plugins_toml: None,
            declared: Vec::new(),
            registry: None,
            registry_ignored: false,
            updater_placed: BTreeMap::new(),
            packs: BTreeSet::new(),
            data_nonempty: BTreeSet::new(),
            user_exists: false,
        }
    }

    #[test]
    fn arch_label_covers_the_three_shipped_architectures_and_refuses_the_rest() {
        assert_eq!(minimal("armv7l").arch_label(), Some("armv7"));
        assert_eq!(minimal("aarch64").arch_label(), Some("arm64"));
        assert_eq!(minimal("x86_64").arch_label(), Some("x86_64"));
        assert_eq!(minimal("mips").arch_label(), None);
    }

    #[test]
    fn is_fresh_needs_both_no_core_and_no_declared_plugin() {
        let mut s = minimal("aarch64");
        assert!(s.is_fresh());
        s.core_present = true;
        assert!(!s.is_fresh());
        s.core_present = false;
        s.declared.push(Declared { name: "radio".to_string(), exec: "ritornello-plugin-radio".to_string() });
        assert!(!s.is_fresh());
    }

    /// (a) A hand-captured survey, exercising every section at once: two
    /// declared plugins (one of them third-party, with an extra key
    /// `declared` must ignore), a registry, two language packs and one
    /// non-empty data directory.
    #[test]
    fn parse_reads_a_hand_captured_survey() {
        let text = "\
@@n1@@ KERNEL
Linux
@@n1@@ MACHINE
x86_64
@@n1@@ SYSTEMD
yes
@@n1@@ UID
1000
@@n1@@ SUDO
no-password
@@n1@@ CORE
yes
@@n1@@ USER
no
@@n1@@ PLUGINS_TOML
[[plugin]]
name = \"radio\"
exec = \"ritornello-plugin-radio\"

[[plugin]]
name = \"acme-widget\"
exec = \"acme-widget-bin\"
args = [\"--quiet\"]
@@n1@@ PLUGINS_TOML_STATE
present
@@n1@@ REGISTRY
format = 1

[components.radio]
version = \"0.2.0\"
privileged = []
@@n1@@ REGISTRY_STATE
present
@@n1@@ REGISTRY_OWNER
0 644
@@n1@@ PLACED
@@n1@@ PLACED_STATE
absent
@@n1@@ PACKS
ritornello-lang-fr
ritornello-lang-es
@@n1@@ DATA
radio
@@n1@@ END
";
        let state = parse(text, "n1").expect("a well-formed survey parses");
        assert_eq!(state.kernel, "Linux");
        assert_eq!(state.machine, "x86_64");
        assert!(state.systemd);
        assert_eq!(state.uid, 1000);
        assert_eq!(state.sudo, Sudo::NoPassword);
        assert!(state.core_present);
        assert!(!state.user_exists);
        assert_eq!(
            state.declared,
            vec![
                Declared { name: "radio".to_string(), exec: "ritornello-plugin-radio".to_string() },
                Declared { name: "acme-widget".to_string(), exec: "acme-widget-bin".to_string() },
            ],
            "declared must read name and exec only, ignoring the third-party plugin's extra `args` key"
        );
        assert!(state.plugins_toml.is_some());
        assert!(!state.is_fresh());
        assert_eq!(
            state.packs,
            BTreeSet::from(["ritornello-lang-fr".to_string(), "ritornello-lang-es".to_string()])
        );
        assert_eq!(state.data_nonempty, BTreeSet::from(["radio".to_string()]));
        let registry = state.registry.expect("a non-empty REGISTRY section parses");
        assert_eq!(registry.format, 1);
        assert_eq!(registry.components.len(), 1);
        assert!(registry.components.contains_key("radio"));
    }

    /// (c) A line that looks like a section marker for a *different* nonce,
    /// sitting inside `plugins.toml`'s own content (a TOML comment, so the
    /// file still parses), must not be mistaken for a real boundary: the
    /// second plugin declared after it, and every section that follows,
    /// must still be read intact.
    #[test]
    fn a_foreign_nonce_marker_inside_plugins_toml_does_not_cut_the_section() {
        let text = "\
@@real@@ KERNEL
Linux
@@real@@ MACHINE
aarch64
@@real@@ SYSTEMD
no
@@real@@ UID
0
@@real@@ SUDO
not-needed
@@real@@ CORE
no
@@real@@ USER
no
@@real@@ PLUGINS_TOML
# @@other@@ END
[[plugin]]
name = \"radio\"
exec = \"ritornello-plugin-radio\"

[[plugin]]
name = \"cd\"
exec = \"ritornello-plugin-cd\"
@@real@@ PLUGINS_TOML_STATE
present
@@real@@ REGISTRY
@@real@@ REGISTRY_STATE
absent
@@real@@ REGISTRY_OWNER
0 644
@@real@@ PLACED
@@real@@ PLACED_STATE
absent
@@real@@ PACKS
@@real@@ DATA
@@real@@ END
";
        let state = parse(text, "real").expect("a foreign-nonce lookalike line is just content");
        assert_eq!(
            state.declared,
            vec![
                Declared { name: "radio".to_string(), exec: "ritornello-plugin-radio".to_string() },
                Declared { name: "cd".to_string(), exec: "ritornello-plugin-cd".to_string() },
            ],
            "both plugins, including the one declared after the lookalike line, must be read"
        );
        assert!(state.registry.is_none());
        assert!(state.packs.is_empty());
        assert!(state.data_nonempty.is_empty());
    }

    /// (e) An unreadable `plugins.toml` is a named error, not an empty
    /// device — otherwise a broken file would read as "nothing installed"
    /// and the installer would go on to remove everything it finds.
    #[test]
    fn an_unparseable_plugins_toml_is_a_named_error_not_an_empty_device() {
        let text = "\
@@n@@ KERNEL
Linux
@@n@@ MACHINE
x86_64
@@n@@ SYSTEMD
no
@@n@@ UID
0
@@n@@ SUDO
not-needed
@@n@@ CORE
no
@@n@@ USER
no
@@n@@ PLUGINS_TOML
this is not valid toml [[[
@@n@@ PLUGINS_TOML_STATE
present
@@n@@ REGISTRY
@@n@@ REGISTRY_STATE
absent
@@n@@ REGISTRY_OWNER
0 644
@@n@@ PLACED
@@n@@ PLACED_STATE
absent
@@n@@ PACKS
@@n@@ DATA
@@n@@ END
";
        let err = parse(text, "n").expect_err("garbage TOML must not read as an empty device");
        assert!(err.to_string().contains("plugins.toml"), "{err}");
    }

    /// Step 5(b): the truncated-output test the brief asks for. A transfer
    /// cut short after the PACKS section — DATA and END never arrive — must
    /// be refused by name, not read as a device with no data directories at
    /// all.
    #[test]
    fn truncated_output_missing_trailing_sections_is_a_named_error() {
        let text = "\
@@n@@ KERNEL
Linux
@@n@@ MACHINE
x86_64
@@n@@ SYSTEMD
no
@@n@@ UID
0
@@n@@ SUDO
not-needed
@@n@@ CORE
no
@@n@@ USER
no
@@n@@ PLUGINS_TOML
@@n@@ PLUGINS_TOML_STATE
absent
@@n@@ REGISTRY
@@n@@ REGISTRY_STATE
absent
@@n@@ REGISTRY_OWNER
0 644
@@n@@ PLACED
@@n@@ PLACED_STATE
absent
@@n@@ PACKS
ritornello-lang-fr
";
        let err = parse(text, "n").expect_err("a transfer cut short must not read as a bare device");
        assert!(err.to_string().contains("DATA"), "{err}");
    }

    /// (b) The script actually runs, under `dash` when it's available
    /// (WSL has it) and under `sh` otherwise, against a temporary root
    /// imitating a device: a core binary, a declared plugin, one language
    /// pack, one non-empty data directory next to an empty one, and a
    /// `run/systemd/system` directory.
    #[test]
    #[cfg(unix)]
    fn probe_script_runs_for_real_against_a_fixture_root() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        std::fs::create_dir_all(root.join("etc/ritornello/language-packs/ritornello-lang-fr"))
            .expect("packs dir");
        std::fs::write(
            root.join("etc/ritornello/plugins.toml"),
            "[[plugin]]\nname = \"radio\"\nexec = \"ritornello-plugin-radio\"\n",
        )
        .expect("plugins.toml");

        std::fs::create_dir_all(root.join("usr/local/bin")).expect("bin dir");
        let core_path = root.join("usr/local/bin/ritornello-core");
        std::fs::write(&core_path, "#!/bin/sh\n").expect("core binary");
        let mut perms = std::fs::metadata(&core_path).expect("core metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&core_path, perms).expect("chmod core binary");

        std::fs::create_dir_all(root.join("var/lib/ritornello/plugins/radio")).expect("radio data dir");
        std::fs::write(root.join("var/lib/ritornello/plugins/radio/stations.toml"), "")
            .expect("stations.toml");
        std::fs::create_dir_all(root.join("var/lib/ritornello/plugins/cd")).expect("empty cd data dir");

        std::fs::create_dir_all(root.join("run/systemd/system")).expect("systemd dir");

        let shell = if std::path::Path::new("/bin/dash").exists() { "/bin/dash" } else { "sh" };
        println!("running probe_script under {shell}");

        let nonce = "fixturenonce";
        let out = std::process::Command::new(shell)
            .arg("-c")
            .arg(probe_script(nonce))
            .env("RITORNELLO_INSTALL_ROOT", root)
            .output()
            .expect("the shell runs");
        assert!(out.status.success(), "probe_script exited non-zero:\n{}", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8(out.stdout).expect("probe_script's output is UTF-8");

        let state = parse(&stdout, nonce).expect("the real script's output parses");
        assert!(state.core_present, "{stdout}");
        assert!(state.systemd, "{stdout}");
        assert_eq!(state.packs, BTreeSet::from(["ritornello-lang-fr".to_string()]), "{stdout}");
        assert_eq!(state.data_nonempty, BTreeSet::from(["radio".to_string()]), "{stdout}");
        assert_eq!(state.declared.len(), 1, "{stdout}");
    }

    /// Review fix round 1, Important finding: a `plugins.toml` or
    /// `installed.toml` with no trailing newline used to glue the next
    /// marker onto the file's last line, so `REGISTRY` (or whatever
    /// followed) never got a `found` entry and a fully healthy device read
    /// as truncated. This is the shape `probe_script` now actually emits
    /// for such a file: a single `\n` — contributed entirely by `m()`'s own
    /// leading newline, not by the file — separates the dangling last line
    /// from the next marker, with no blank line in between; every other
    /// section (whose underlying command *does* end in `\n`) gets one.
    #[test]
    fn parse_reads_plugins_toml_and_registry_without_a_trailing_newline() {
        let text = "\
@@n@@ KERNEL
Linux

@@n@@ MACHINE
x86_64

@@n@@ SYSTEMD
no

@@n@@ UID
0

@@n@@ SUDO
not-needed

@@n@@ CORE
no

@@n@@ USER
no

@@n@@ PLUGINS_TOML
[[plugin]]
name = \"radio\"
exec = \"ritornello-plugin-radio\"
@@n@@ PLUGINS_TOML_STATE
present
@@n@@ REGISTRY
format = 1

[components.radio]
version = \"0.2.0\"
privileged = []
@@n@@ REGISTRY_STATE
present
@@n@@ REGISTRY_OWNER
0 644
@@n@@ PLACED
@@n@@ PLACED_STATE
absent
@@n@@ PACKS
ritornello-lang-fr

@@n@@ DATA
radio

@@n@@ END
";
        let state = parse(text, "n")
            .expect("a marker glued directly onto an unterminated file's last line must still parse");
        assert_eq!(state.kernel, "Linux");
        assert_eq!(state.machine, "x86_64");
        assert!(!state.systemd);
        assert_eq!(state.sudo, Sudo::NotNeeded);
        assert_eq!(
            state.declared,
            vec![Declared { name: "radio".to_string(), exec: "ritornello-plugin-radio".to_string() }],
            "the REGISTRY marker must not have been swallowed into PLUGINS_TOML's own body"
        );
        let registry = state.registry.expect("REGISTRY, glued to plugins.toml's unterminated last line, must still be found");
        assert_eq!(registry.format, 1);
        assert!(registry.components.contains_key("radio"));
        assert_eq!(state.packs, BTreeSet::from(["ritornello-lang-fr".to_string()]));
        assert_eq!(state.data_nonempty, BTreeSet::from(["radio".to_string()]));
    }

    /// The same finding, reproduced end to end: `probe_script` actually
    /// runs against a fixture whose `plugins.toml` and `installed.toml` are
    /// written without a trailing newline, under the same shell selection
    /// (`dash` when present) as the other real-execution test.
    #[test]
    #[cfg(unix)]
    fn probe_script_runs_for_real_with_no_trailing_newline_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        std::fs::create_dir_all(root.join("etc/ritornello/language-packs/ritornello-lang-fr"))
            .expect("packs dir");
        // Deliberately no trailing `\n` on either file: this is the exact
        // shape (`printf` without a final `\n`, `vim` with `noeol`, a write
        // cut short) the review found broke the survey.
        std::fs::write(
            root.join("etc/ritornello/plugins.toml"),
            "[[plugin]]\nname = \"radio\"\nexec = \"ritornello-plugin-radio\"",
        )
        .expect("plugins.toml without a trailing newline");
        std::fs::create_dir_all(root.join("var/lib/ritornello-install")).expect("install state dir");
        std::fs::write(
            root.join("var/lib/ritornello-install/installed.toml"),
            "format = 1\n\n[components.radio]\nversion = \"0.2.0\"\nprivileged = []",
        )
        .expect("installed.toml without a trailing newline");

        std::fs::create_dir_all(root.join("usr/local/bin")).expect("bin dir");
        let core_path = root.join("usr/local/bin/ritornello-core");
        std::fs::write(&core_path, "#!/bin/sh\n").expect("core binary");
        let mut perms = std::fs::metadata(&core_path).expect("core metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&core_path, perms).expect("chmod core binary");

        std::fs::create_dir_all(root.join("var/lib/ritornello/plugins/radio")).expect("radio data dir");
        std::fs::write(root.join("var/lib/ritornello/plugins/radio/stations.toml"), "")
            .expect("stations.toml");

        std::fs::create_dir_all(root.join("run/systemd/system")).expect("systemd dir");

        let shell = if std::path::Path::new("/bin/dash").exists() { "/bin/dash" } else { "sh" };
        println!("running probe_script under {shell}");

        let nonce = "notrailingnewline";
        let out = std::process::Command::new(shell)
            .arg("-c")
            .arg(probe_script(nonce))
            .env("RITORNELLO_INSTALL_ROOT", root)
            .output()
            .expect("the shell runs");
        assert!(out.status.success(), "probe_script exited non-zero:\n{}", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8(out.stdout).expect("probe_script's output is UTF-8");

        // The fixture's registry belongs to whoever runs the tests, not to
        // root: as surveyed, it is not trusted. Read as root's, it parses.
        if !running_as_root() {
            let state = parse(&stdout, nonce).expect("parses");
            assert!(state.registry_ignored && state.registry.is_none(), "{stdout}");
        }
        let stdout = as_if_root(&stdout, nonce);
        let state = match parse(&stdout, nonce) {
            Ok(state) => state,
            Err(e) => panic!("a device whose files lack a trailing newline must still parse as complete: {e}\n{stdout}"),
        };
        assert!(state.core_present, "{stdout}");
        assert_eq!(state.declared.len(), 1, "{stdout}");
        let registry = state.registry.expect("installed.toml without a trailing newline must still be found");
        assert!(registry.components.contains_key("radio"), "{stdout}");
        assert_eq!(state.packs, BTreeSet::from(["ritornello-lang-fr".to_string()]), "{stdout}");
        assert_eq!(
            state.plugins_toml.as_deref(),
            Some("[[plugin]]\nname = \"radio\"\nexec = \"ritornello-plugin-radio\""),
            "the file's exact bytes, with no newline added: an unchanged plan writes it back as it was"
        );
    }

    /// R36: a survey whose two files are given as `(body, state)` — the
    /// body exactly as `probe_script` frames it, i.e. the file's bytes plus
    /// the one `\n` the next marker opens with.
    fn survey_with(plugins_toml: (&str, &str), registry: (&str, &str)) -> String {
        survey_with_placed(plugins_toml, registry, ("", "absent"))
    }

    /// The same, with the updater's `placed.json` too.
    fn survey_with_placed(plugins_toml: (&str, &str), registry: (&str, &str), placed: (&str, &str)) -> String {
        survey_full(plugins_toml, registry, "0 644", placed)
    }

    /// Every file of the survey, and the registry's `stat -L -c '%u %a'`.
    fn survey_full(plugins_toml: (&str, &str), registry: (&str, &str), owner: &str, placed: (&str, &str)) -> String {
        format!(
            "\n@@n@@ KERNEL\nLinux\n\n@@n@@ MACHINE\nx86_64\n\n@@n@@ SYSTEMD\nyes\n\n@@n@@ UID\n0\n\
             \n@@n@@ SUDO\nnot-needed\n\n@@n@@ CORE\nyes\n\n@@n@@ USER\nyes\n\
             \n@@n@@ PLUGINS_TOML\n{}\n@@n@@ PLUGINS_TOML_STATE\n{}\n\
             \n@@n@@ REGISTRY\n{}\n@@n@@ REGISTRY_STATE\n{}\n@@n@@ REGISTRY_OWNER\n{}\n\
             \n@@n@@ PLACED\n{}\n@@n@@ PLACED_STATE\n{}\n\
             \n@@n@@ PACKS\n\n@@n@@ DATA\n\n@@n@@ END\n",
            plugins_toml.0, plugins_toml.1, registry.0, registry.1, owner, placed.0, placed.1
        )
    }

    #[cfg(unix)]
    fn running_as_root() -> bool {
        let uid = std::process::Command::new("id").arg("-u").output().expect("id runs");
        String::from_utf8_lossy(&uid.stdout).trim() == "0"
    }

    /// The survey's output with the registry's owner line replaced by
    /// root's: what the same files would say on a device.
    #[cfg(unix)]
    fn as_if_root(stdout: &str, nonce: &str) -> String {
        let marker = format!("@@{nonce}@@ REGISTRY_OWNER\n");
        let at = stdout.find(&marker).expect("the owner section") + marker.len();
        let end = at + stdout[at..].find('\n').expect("one line");
        format!("{}0 644{}", &stdout[..at], &stdout[end..])
    }

    const SAME_VERSION_REGISTRY: &str = "format = 1\n\n[components.radio]\nversion = \"0.2.0\"\nprivileged = []\n";

    /// Only a registry root owns and only root may write is trusted; any
    /// other is read as absent — the fail-safe direction, since an absent
    /// registry makes the plan place everything.
    ///
    /// **[MUTATION]**, one per condition of `registry_trusted`, each
    /// reddening this test: drop `uid == "0"`; drop the `0o022` mask (or
    /// narrow it to `0o002`).
    #[test]
    fn a_registry_root_does_not_own_or_others_may_write_is_not_trusted() {
        let read = |owner: &str| {
            parse(&survey_full((RADIO_BLOCK, "present"), (SAME_VERSION_REGISTRY, "present"), owner, ("", "absent")), "n")
                .unwrap_or_else(|e| panic!("{owner:?}: {e}"))
        };
        for owner in ["0 644", "0 600", "0 444", "0 4644"] {
            let state = read(owner);
            assert!(state.registry.is_some() && !state.registry_ignored, "{owner:?}");
        }
        for owner in ["1000 644", "0 664", "0 646", "0 666", "1000 600", "", "0", "0 rw", "0 644 x"] {
            let state = read(owner);
            assert!(state.registry.is_none() && state.registry_ignored, "{owner:?}");
        }
        // An untrusted registry is not even parsed: garbage in it is no
        // refusal, only an unrecorded device.
        let state = parse(&survey_full((RADIO_BLOCK, "present"), ("format = [[[", "present"), "1000 644", ("", "absent")), "n")
            .expect("an untrusted registry is never parsed");
        assert!(state.registry_ignored);
        // No registry at all is not an ignored one.
        let absent = parse(&survey_full((RADIO_BLOCK, "present"), ("", "absent"), "", ("", "absent")), "n").unwrap();
        assert!(!absent.registry_ignored && absent.registry.is_none());
    }

    /// The plan's side of it: an ignored registry leaves the device
    /// unrecorded, so a component the registry would have vouched for is
    /// placed again.
    #[test]
    fn an_ignored_registry_makes_the_plan_place_everything() {
        use crate::plan::tests as p;
        let trusted = p::current_device();
        assert!(p::keep(&trusted).nothing_to_do);
        let mut ignored = p::current_device();
        ignored.registry = None;
        ignored.registry_ignored = true;
        let plan = p::keep(&ignored);
        assert!(!plan.nothing_to_do);
        assert!(plan.summary.up_to_date.is_empty(), "{:?}", plan.summary);
    }

    /// The survey reads at most `READ_CAP` bytes and one more: a file of
    /// exactly the cap is read whole, one byte over is refused by name for
    /// `plugins.toml` and the registry (a prefix of `plugins.toml` could
    /// parse, as a device declaring fewer plugins), and is no evidence for
    /// the updater's memory.
    ///
    /// **[MUTATION]**: drop the `READ_CAP` check from `file_content` —
    /// this test fails.
    #[test]
    fn a_file_over_the_read_cap_is_refused_not_parsed_in_part() {
        let comment = |n: usize| format!("#{}", "x".repeat(n - 1));
        let at_cap = comment(READ_CAP);
        let state = parse(&survey_with((&at_cap, "present"), ("", "absent")), "n").expect("exactly the cap is read whole");
        assert_eq!(state.plugins_toml.as_deref().map(str::len), Some(READ_CAP));
        let over = comment(READ_CAP + 1);
        let err = parse(&survey_with((&over, "present"), ("", "absent")), "n").unwrap_err();
        assert!(err.to_string().contains("plugins.toml") && err.to_string().contains("larger than"), "{err}");
        let err = parse(&survey_with((RADIO_BLOCK, "present"), (&over, "present")), "n").unwrap_err();
        assert!(err.to_string().contains("installed.toml"), "{err}");
        let big_json = format!("{}{}", PLACED_JSON, " ".repeat(READ_CAP));
        let state =
            parse(&survey_with_placed((RADIO_BLOCK, "present"), ("", "absent"), (&big_json, "present")), "n").unwrap();
        assert!(state.updater_placed.is_empty(), "a memory over the cap is no evidence");
    }

    /// For real: a huge `plugins.toml` costs the survey `READ_CAP + 1`
    /// bytes of output, not its size, and the run is refused by name.
    ///
    /// **[MUTATION]**: read with `cat` again in `rd` — this test fails on
    /// the output's size.
    #[test]
    #[cfg(unix)]
    fn probe_script_reads_no_more_than_the_cap_of_a_huge_file() {
        let stdout = run_probe(|root| {
            let mut text = String::from(RADIO_BLOCK);
            text.push_str(&format!("#{}\n", "x".repeat(3 * READ_CAP)));
            std::fs::write(root.join("etc/ritornello/plugins.toml"), text).expect("a huge plugins.toml");
        });
        assert!(stdout.len() < READ_CAP + 4096, "{} bytes of survey", stdout.len());
        let err = parse(&stdout, "real").unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
    }

    /// For real: `stat -L` answers the shape `registry_trusted` reads, and
    /// a registry the test's own account owns is not trusted (skipped when
    /// the tests run as root, whose files these would be).
    #[test]
    // GNU `stat -c` only: the probe runs on the device, which is GNU/Linux.
    #[cfg(target_os = "linux")]
    fn probe_script_reports_the_registry_s_owner_and_mode() {
        if running_as_root() {
            println!("SKIPPED: running as root, whose registry is trusted");
            return;
        }
        let stdout = run_probe(|root| {
            std::fs::write(root.join("var/lib/ritornello-install/installed.toml"), SAME_VERSION_REGISTRY)
                .expect("registry");
        });
        let line = stdout.split("@@real@@ REGISTRY_OWNER\n").nth(1).unwrap().lines().next().unwrap();
        let mut parts = line.split(' ');
        assert!(parts.next().unwrap().parse::<u32>().unwrap() > 0, "{line}");
        assert!(u32::from_str_radix(parts.next().unwrap(), 8).is_ok(), "{line}");
        let state = parse(&stdout, "real").unwrap();
        assert!(state.registry_ignored && state.registry.is_none(), "{stdout}");
        let state = parse(&as_if_root(&stdout, "real"), "real").unwrap();
        assert!(state.registry.is_some(), "{stdout}");
    }

    /// The owner's Pi's `placed.json`, verbatim (captured 2026-10-06, owner
    /// and mode `ritornello:ritornello 0644`, readable by the ssh account):
    /// what `update::placed::record` wrote after the web UI's updater placed
    /// the core and eight plugins. The core's archive note rides along; only
    /// each version is read.
    const PLACED_JSON: &str = r#"{"cd":{"version":"0.2.0-beta.3"},"core":{"version":"0.2.0-beta.3","not_installed_files":["etc/polkit-1/rules.d/52-ritornello-update.rules","etc/polkit-1/rules.d/50-ritornello-power.rules","etc/systemd/system/ritornello-update.service","etc/systemd/system/ritornello.service","etc/systemd/system/ritornello-rollback.service","usr/local/lib/ritornello/ritornello-update"]},"generic-input":{"version":"0.2.0-beta.3"},"mpd":{"version":"0.2.0-beta.3"},"musicbrainz":{"version":"0.2.0-beta.3"},"nrj-metas":{"version":"0.2.0-beta.3"},"ouifm-metas":{"version":"0.2.0-beta.3"},"radio":{"version":"0.2.0-beta.3"},"radiofrance-metas":{"version":"0.2.0-beta.3"}}"#;

    #[test]
    fn the_updater_s_memory_is_read_version_by_version() {
        let state = parse(&survey_with_placed((RADIO_BLOCK, "present"), ("", "absent"), (PLACED_JSON, "present")), "n")
            .unwrap();
        let names: Vec<&str> = state.updater_placed.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["cd", "core", "generic-input", "mpd", "musicbrainz", "nrj-metas", "ouifm-metas", "radio", "radiofrance-metas"]
        );
        assert!(state.updater_placed.values().all(|v| v == "0.2.0-beta.3"), "{:?}", state.updater_placed);
    }

    /// An untrusted file the run can do without: absent, unreadable, or not
    /// the expected JSON, it is no evidence at all — never a refusal, which
    /// would let the unprivileged account stop every install.
    ///
    /// **[MUTATION]**: make `updater_placed` refuse what does not parse
    /// (`.expect` in place of `.unwrap_or_default()`) — this test fails.
    #[test]
    fn an_absent_unreadable_or_garbage_updater_memory_is_no_evidence() {
        for placed in [("", "absent"), ("", "unreadable"), ("{\"core\": {\"vers", "present"), ("[1, 2]", "present"), ("", "present")] {
            let state = parse(&survey_with_placed((RADIO_BLOCK, "present"), ("", "absent"), placed), "n")
                .unwrap_or_else(|e| panic!("{placed:?}: {e}"));
            assert!(state.updater_placed.is_empty(), "{placed:?}: {:?}", state.updater_placed);
        }
    }

    /// For real: the survey reads the file where the core writes it.
    #[test]
    #[cfg(unix)]
    fn probe_script_reads_the_updater_s_memory_where_the_core_keeps_it() {
        let stdout = run_probe(|root| {
            std::fs::create_dir_all(root.join("var/lib/ritornello/staging")).expect("staging dir");
            std::fs::write(root.join("var/lib/ritornello/staging/placed.json"), PLACED_JSON).expect("placed.json");
        });
        let state = parse(&stdout, "real").expect("parses");
        assert_eq!(state.updater_placed.get("radio").map(String::as_str), Some("0.2.0-beta.3"), "{stdout}");
        let stdout = run_probe(|_| {});
        assert!(parse(&stdout, "real").unwrap().updater_placed.is_empty(), "{stdout}");
    }

    const RADIO_BLOCK: &str = "[[plugin]]\nname = \"radio\"\nexec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"\n";

    #[test]
    fn an_absent_plugins_toml_parses_as_none() {
        let state = parse(&survey_with(("", "absent"), ("", "absent")), "n").unwrap();
        assert_eq!(state.plugins_toml, None);
        assert!(state.declared.is_empty());
        assert_eq!(state.registry, None);
    }

    #[test]
    fn a_present_but_empty_plugins_toml_is_not_absent() {
        let state = parse(&survey_with(("", "present"), ("", "absent")), "n").unwrap();
        assert_eq!(state.plugins_toml.as_deref(), Some(""));
        assert!(state.declared.is_empty());
    }

    #[test]
    fn an_unreadable_plugins_toml_or_registry_is_a_named_error() {
        let err = parse(&survey_with(("", "unreadable"), ("", "absent")), "n").unwrap_err();
        assert!(err.to_string().contains("plugins.toml") && err.to_string().contains("could not be read"), "{err}");
        let err = parse(&survey_with((RADIO_BLOCK, "present"), ("", "unreadable")), "n").unwrap_err();
        assert!(err.to_string().contains("installed.toml") && err.to_string().contains("could not be read"), "{err}");
    }

    /// An empty `installed.toml` is not an empty registry: read as one, it
    /// would forget every privileged file it records.
    #[test]
    fn a_present_but_empty_registry_is_refused() {
        let err = parse(&survey_with((RADIO_BLOCK, "present"), ("", "present")), "n").unwrap_err();
        assert!(err.to_string().contains("installed.toml"), "{err}");
    }

    #[test]
    fn a_state_that_contradicts_its_content_or_is_unknown_is_refused() {
        assert!(parse(&survey_with((RADIO_BLOCK, "absent"), ("", "absent")), "n").is_err());
        assert!(parse(&survey_with((RADIO_BLOCK, "maybe"), ("", "absent")), "n").is_err());
    }

    /// Byte for byte, both ways a file can end: with its newline, the body
    /// carries it plus the marker's; without, only the marker's.
    #[test]
    fn a_present_plugins_toml_is_read_byte_for_byte() {
        let state = parse(&survey_with((RADIO_BLOCK, "present"), ("", "absent")), "n").unwrap();
        assert_eq!(state.plugins_toml.as_deref(), Some(RADIO_BLOCK));
        let unterminated = RADIO_BLOCK.trim_end_matches('\n');
        let state = parse(&survey_with((unterminated, "present"), ("", "absent")), "n").unwrap();
        assert_eq!(state.plugins_toml.as_deref(), Some(unterminated));
        let crlf = "# x\r\n[[plugin]]\r\nname = \"radio\"\r\nexec = \"/x\"\r\n";
        let state = parse(&survey_with((crlf, "present"), ("", "absent")), "n").unwrap();
        assert_eq!(state.plugins_toml.as_deref(), Some(crlf));
    }

    /// Runs `probe_script` for real, under `dash` when present, against a
    /// root whose files are set up by `arrange`.
    #[cfg(unix)]
    fn run_probe(arrange: impl FnOnce(&std::path::Path)) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("etc/ritornello")).expect("etc dir");
        std::fs::create_dir_all(dir.path().join("var/lib/ritornello-install")).expect("install state dir");
        arrange(dir.path());
        let shell = if std::path::Path::new("/bin/dash").exists() { "/bin/dash" } else { "sh" };
        let out = std::process::Command::new(shell)
            .arg("-c")
            .arg(probe_script("real"))
            .env("RITORNELLO_INSTALL_ROOT", dir.path())
            .output()
            .expect("the shell runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).expect("UTF-8")
    }

    /// R36 for real: an empty file is present, a missing one absent.
    #[test]
    #[cfg(unix)]
    fn probe_script_tells_an_empty_file_from_a_missing_one() {
        let stdout = run_probe(|root| {
            std::fs::write(root.join("etc/ritornello/plugins.toml"), "").expect("empty plugins.toml");
        });
        let state = parse(&stdout, "real").expect("parses");
        assert_eq!(state.plugins_toml.as_deref(), Some(""), "{stdout}");
        assert_eq!(state.registry, None, "{stdout}");
        let stdout = run_probe(|_| {});
        assert_eq!(parse(&stdout, "real").unwrap().plugins_toml, None, "{stdout}");
    }

    /// R36 for real: a `plugins.toml` with mode 000 must be refused by name,
    /// never read as absent. Root reads a 000 file anyway, so the test is
    /// skipped (and says why) when it runs as root.
    #[test]
    #[cfg(unix)]
    fn probe_script_reports_an_unreadable_file_as_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let uid = std::process::Command::new("id").arg("-u").output().expect("id runs");
        if String::from_utf8_lossy(&uid.stdout).trim() == "0" {
            println!("SKIPPED: running as root, which reads a mode-000 file anyway");
            return;
        }
        let stdout = run_probe(|root| {
            let path = root.join("etc/ritornello/plugins.toml");
            std::fs::write(&path, RADIO_BLOCK).expect("plugins.toml");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
        });
        assert!(stdout.contains("@@real@@ PLUGINS_TOML_STATE\nunreadable"), "{stdout}");
        let err = parse(&stdout, "real").expect_err("an unreadable plugins.toml must not read as absent");
        assert!(err.to_string().contains("could not be read"), "{err}");
    }

    /// R38(a): behind a directory that cannot be searched, `[ -e ]` and
    /// `[ -L ]` are both false — exactly what a missing file answers — so a
    /// `plugins.toml` there used to read as absent, and an update would
    /// then write a fresh one over it. Its parent, and its grandparent,
    /// each made unsearchable in turn. Skipped under root, which searches a
    /// mode-000 directory anyway.
    ///
    /// **[MUTATION]**: read an unsearchable parent as absent (`rd`'s first
    /// branch back to `s=absent` alone) — this test fails.
    #[test]
    #[cfg(unix)]
    fn a_file_behind_an_unsearchable_directory_is_unreadable_not_absent() {
        use std::os::unix::fs::PermissionsExt;

        let uid = std::process::Command::new("id").arg("-u").output().expect("id runs");
        if String::from_utf8_lossy(&uid.stdout).trim() == "0" {
            println!("SKIPPED: running as root, which searches a mode-000 directory anyway");
            return;
        }
        let shell = if std::path::Path::new("/bin/dash").exists() { "/bin/dash" } else { "sh" };
        for locked in ["etc/ritornello", "etc"] {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::create_dir_all(dir.path().join("etc/ritornello")).expect("etc dir");
            std::fs::write(dir.path().join("etc/ritornello/plugins.toml"), RADIO_BLOCK).expect("plugins.toml");
            let locked = dir.path().join(locked);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(probe_script("real"))
                .env("RITORNELLO_INSTALL_ROOT", dir.path())
                .output()
                .expect("the shell runs");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
            let stdout = String::from_utf8(out.stdout).expect("UTF-8");
            assert!(stdout.contains("@@real@@ PLUGINS_TOML_STATE\nunreadable"), "{}: {stdout}", locked.display());
            // The registry, whose own directories are all searchable, is
            // still told absent: the walk stops at the first one it can see.
            assert!(stdout.contains("@@real@@ REGISTRY_STATE\nabsent"), "{}: {stdout}", locked.display());
        }
    }

    /// The same walk, with nothing locked: a file whose parent directory is
    /// itself missing is absent, not unreadable.
    #[test]
    #[cfg(unix)]
    fn a_file_whose_directories_are_missing_is_absent() {
        let stdout = run_probe(|root| {
            std::fs::remove_dir_all(root.join("etc")).expect("remove etc");
        });
        assert!(stdout.contains("@@real@@ PLUGINS_TOML_STATE\nabsent"), "{stdout}");
    }

    /// R38(b): an empty or broken registry is refused, and the refusal
    /// says what to do about it.
    #[test]
    fn an_unparsable_registry_says_what_to_do() {
        for body in ["", "format = [[["] {
            let err = parse(&survey_with((RADIO_BLOCK, "present"), (body, "present")), "n").unwrap_err();
            let m = format!("{err:#}");
            assert!(m.contains("restore it from a backup, or remove it"), "{body:?}: {m}");
            assert!(m.contains("privileged files it placed will then not be removed"), "{body:?}: {m}");
        }
    }

    /// The same risk as `script::tests::the_device_script_carries_no_carriage_return`,
    /// for the other text the device runs with `sh`.
    #[test]
    fn the_probe_script_carries_no_carriage_return() {
        assert!(!probe_script("nonce").contains('\r'));
    }
}
