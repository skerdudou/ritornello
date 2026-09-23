//! Names and places: the only strings that ever become a path on the device.

// Read only by the placing and removal code later tasks add; nothing in
// this task's own tests needs them by name (they hard-code the same
// strings instead, as the fixtures the allow-list is checked against).
#[expect(dead_code, reason = "wired by task 15")]
pub const PLUGINS_DIR: &str = "/usr/local/lib/ritornello/plugins";
#[expect(dead_code, reason = "wired by task 15")]
pub const DATA_ROOT: &str = "/var/lib/ritornello/plugins";
#[expect(dead_code, reason = "wired by task 15")]
pub const PACKS_ROOT: &str = "/etc/ritornello/language-packs";
#[expect(dead_code, reason = "wired by task 15")]
pub const PLUGINS_TOML: &str = "/etc/ritornello/plugins.toml";
#[expect(dead_code, reason = "wired by task 15")]
pub const REGISTRY: &str = "/var/lib/ritornello-install/installed.toml";

pub fn valid_plugin_name(name: &str) -> bool {
    ritornello_updater::request::valid_name(name)
}

pub fn pack_id(language: &str) -> String {
    format!("ritornello-lang-{language}")
}

pub fn valid_language(language: &str) -> bool {
    // `valid_pack_id` alone is not enough: it judges the composed pack id
    // (`ritornello-lang-<language>`), whose own leading character is always
    // `r`, so a `language` starting with `-` (`"-fr"`) never touches that
    // check's leading-dash rule at all — the dash lands in the middle of
    // the composed string, not at either end of it. Checked directly here
    // instead of relaxed away.
    !language.is_empty()
        && !language.starts_with('-')
        && !language.ends_with('-')
        && ritornello_i18n::valid_pack_id(&pack_id(language))
}

/// Whether `path` is safe to hand to a POSIX `sh` on the device: an
/// absolute path built entirely of `/`-separated segments drawn from
/// `[A-Za-z0-9._-]`, none of them empty, `.` or `..`, and no trailing `/`.
///
/// This is deliberately an allow-list of bytes, not a denylist of `..`:
/// review I1 found that the previous version — rejecting only exact `.`
/// and `..` segments — let NUL, newline, quotes, `$`, backquotes and empty
/// segments straight through. A NUL byte inside a segment
/// (`/etc/ritornello/.\0./.\0./etc/shadow`) still reads as a harmless
/// dotted name here, in Rust, but dash's own input reader is known (from
/// its `preadbuffer`, "delete nul characters") to strip NUL bytes before
/// parsing a line — turning the same string into
/// `/etc/ritornello/../../etc/shadow` on the one shell the device actually
/// runs. A shell metacharacter (`'`, `` ` ``, `$`, a newline) has the same
/// problem one layer up, in whatever command later embeds this path. None
/// of that can be fixed downstream by quoting alone if this function's own
/// contract is "safe, whatever the registry or the inventory says" — so
/// the byte alphabet is fixed here, once, rather than trusted to every
/// future caller.
fn clean(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    rest.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    })
}

/// Whether the installer may remove this file. Everything else is refused,
/// whatever the registry or the inventory says.
pub fn deletable_file(path: &str) -> bool {
    if !clean(path) {
        return false;
    }
    if path == "/usr/local/bin/ritornello-core" {
        return true;
    }
    const UNDER: &[&str] = &[
        "/usr/local/lib/ritornello/",
        "/etc/ritornello/",
        "/var/lib/ritornello/",
        "/var/lib/ritornello-update/",
        "/var/lib/ritornello-install/",
    ];
    if UNDER.iter().any(|p| path.len() > p.len() && path.starts_with(p)) {
        return true;
    }
    if let Some(f) = path.strip_prefix("/etc/systemd/system/") {
        return !f.contains('/') && valid_ritornello_unit_name(f);
    }
    if let Some(f) = path.strip_prefix("/etc/polkit-1/rules.d/") {
        return !f.contains('/') && valid_ritornello_rule_name(f);
    }
    false
}

/// `ritornello.service`, or `ritornello-<bare name>.service`. A prefix
/// match (`f.starts_with("ritornello")`) also accepted another package's
/// `ritornellofoo.service`, since nothing required a separator after the
/// shared prefix (M2). `ritornello-foo.service` staying accepted is
/// intentional: it is still inside Ritornello's own namespace.
fn valid_ritornello_unit_name(f: &str) -> bool {
    if f == "ritornello.service" {
        return true;
    }
    let Some(stem) = f.strip_prefix("ritornello-").and_then(|s| s.strip_suffix(".service")) else {
        return false;
    };
    !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `<two digits>-ritornello-<bare name>.rules`, matching every rule the
/// real inventory ships (`50-`, `51-` and `52-ritornello-*.rules`) and
/// nothing else. A substring match (`f.contains("-ritornello-")`) also
/// accepted `x-ritornello-.rules` and a third party's own
/// `49-foo-ritornello-bridge.rules` (M1): removing either through the
/// registry would silently drop a permission the third party's own rule
/// granted.
fn valid_ritornello_rule_name(f: &str) -> bool {
    let Some(rest) = f.strip_suffix(".rules") else {
        return false;
    };
    let bytes = rest.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_digit() || !bytes[1].is_ascii_digit() {
        return false;
    }
    let Some(bare) = rest[2..].strip_prefix("-ritornello-") else {
        return false;
    };
    !bare.is_empty() && bare.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether the installer may remove this directory recursively. Never
/// anything under `/mnt`: a mount point is removed with `rmdir`, after a
/// confirmed unmount, and a recursive delete of a mounted share would delete
/// the NAS's content.
pub fn deletable_tree(path: &str) -> bool {
    if !clean(path) {
        return false;
    }
    const ROOTS: &[&str] = &[
        "/etc/ritornello",
        "/var/lib/ritornello",
        "/var/lib/ritornello-update",
        "/var/lib/ritornello-install",
        "/usr/local/lib/ritornello",
    ];
    if ROOTS.contains(&path) {
        return true;
    }
    if let Some(n) = path.strip_prefix("/var/lib/ritornello/plugins/") {
        return valid_plugin_name(n);
    }
    if let Some(id) = path.strip_prefix("/etc/ritornello/language-packs/") {
        return id.strip_prefix("ritornello-lang-").is_some_and(valid_language);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_name_rule_is_the_privileged_side_s_own() {
        for n in ["radio", "nrj-metas", "a", "", "..", "A", "a/b", "-a", "a.b"] {
            assert_eq!(valid_plugin_name(n), ritornello_updater::request::valid_name(n), "{n:?}");
        }
    }

    #[test]
    fn a_regionalised_language_forms_a_pack_id() {
        assert!(valid_language("fr") && valid_language("pt-BR") && valid_language("es-419"));
        for bad in ["", "..", "fr/x", "-fr", "fr-"] {
            assert!(!valid_language(bad), "{bad:?}");
        }
    }

    /// The last line of defence: whatever a registry or an inventory says, the
    /// installer never removes a file outside Ritornello's own locations.
    #[test]
    fn only_ritornello_s_own_files_are_deletable() {
        for ok in [
            "/usr/local/bin/ritornello-core",
            "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio",
            "/usr/local/lib/ritornello/ritornello-media-mount",
            "/etc/systemd/system/ritornello.service",
            "/etc/systemd/system/ritornello-media-mount.service",
            "/etc/polkit-1/rules.d/51-ritornello-media.rules",
            "/etc/ritornello/plugins.toml",
            "/var/lib/ritornello-install/installed.toml",
        ] {
            assert!(deletable_file(ok), "{ok}");
        }
        for bad in [
            "/usr/local/bin/other",
            "/usr/bin/mpv",
            "/etc/systemd/system/sshd.service",
            "/etc/systemd/system/ritornello/../sshd.service",
            "/etc/polkit-1/rules.d/10-other.rules",
            "/etc/passwd",
            "/etc/ritornello",
            "/usr/local/lib/ritornello/../../bin/sh",
            "relative/path",
            "/mnt/ritornello/nas",
            "/var/lib/ritornello-update/../../etc/shadow",
            // The bare directory root, trailing slash included: `clean()`
            // now refuses a trailing `/` outright (an empty final
            // segment), which is what makes this one refused today — not
            // the `UNDER` length check, which the fix below leaves
            // provably unreachable for this exact input (a clean path can
            // never equal a `UNDER` prefix, since the prefix itself ends
            // in `/` and `clean()` no longer admits that).
            "/etc/ritornello/",
            // I1: a NUL byte disguises a `..` segment from Rust's own
            // `split('/')` (`.\0.` is not literally `..`), but dash's
            // input reader is known to strip NUL bytes before parsing a
            // line, turning this back into `/etc/ritornello/../../etc/shadow`
            // on the device's own shell.
            "/etc/ritornello/.\0./.\0./etc/shadow",
            // I1: shell metacharacters — a newline, a quote, a `$` — pass
            // clean()'s old any-byte segment check outright.
            "/etc/ritornello/x\n rm -rf /",
            "/etc/ritornello/x'; rm -rf / #",
            "/etc/ritornello/$(reboot)",
            // I1: an empty segment (`//`) and a trailing `/` on a file
            // that is not the bare directory root.
            "/etc/ritornello//plugins.toml",
            "/etc/ritornello/plugins.toml/",
            // M1: a substring match on `-ritornello-` also accepted a bare
            // prefix with no digits, and a third party's own rule that
            // merely mentions Ritornello in its name.
            "/etc/polkit-1/rules.d/x-ritornello-.rules",
            "/etc/polkit-1/rules.d/49-foo-ritornello-bridge.rules",
            // M2: `starts_with("ritornello")` had no separator, so another
            // package's unit sharing the same prefix was also accepted.
            "/etc/systemd/system/ritornellofoo.service",
        ] {
            assert!(!deletable_file(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_ritornello_s_own_trees_are_deletable_and_never_a_mount_root() {
        for ok in [
            "/etc/ritornello",
            "/var/lib/ritornello",
            "/var/lib/ritornello-update",
            "/var/lib/ritornello-install",
            "/usr/local/lib/ritornello",
            "/var/lib/ritornello/plugins/radio",
            "/etc/ritornello/language-packs/ritornello-lang-pt-BR",
        ] {
            assert!(deletable_tree(ok), "{ok}");
        }
        for bad in [
            "/mnt/ritornello",
            "/mnt/ritornello/nas",
            "/var/lib",
            "/etc",
            "/",
            "/var/lib/ritornello/plugins/../../..",
            "/var/lib/ritornello/plugins/Radio",
            "/etc/ritornello/language-packs/../..",
        ] {
            assert!(!deletable_tree(bad), "{bad}");
        }
    }

    /// Every `dest` the real inventory places must be one the installer can
    /// delete again — otherwise a total removal would refuse to clean up
    /// Ritornello's own files, which is the exact defect a delete allow-list
    /// exists to make impossible rather than merely unlikely.
    #[test]
    fn every_real_inventory_dest_is_deletable() {
        let inv = crate::inventory::tests::real_inventory();
        let mut checked = 0;
        for c in std::iter::once(&inv.core).chain(inv.plugins.iter()) {
            for f in &c.files {
                assert!(deletable_file(&f.dest), "{}: {} is not deletable", c.name, f.dest);
                checked += 1;
            }
        }
        assert!(checked > 0, "checked no file at all — the walk is wrong");
    }
}
