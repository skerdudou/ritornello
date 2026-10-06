//! This installer's own number, and the notice that a newer one exists.
//!
//! The installer has a publication channel of its own: releases tagged
//! `installer-vX.Y.Z`, whose number is `[package] version` of this crate (a
//! finished `X.Y.Z`, moved only when the installer changes), and one fixed
//! release tagged `installer` whose assets always are the newest installer's —
//! the address the README links, with no version in it.
//!
//! **The installer never updates itself.** It is an unsigned binary that runs
//! root commands on a device: replacing it from the network is exactly the
//! step this project refuses to automate. It only says that a newer one
//! exists, with the link to take it from, and carries on.
//!
//! Nothing here may block or fail a run. The notice is computed from the
//! releases list the installer has already fetched to find the product release
//! (no request of its own), and every way it can be wrong — a tag nobody can
//! read, a version it cannot parse, a system it has no archive for — ends in
//! saying nothing.

use crate::source::REPO;

/// This installer's number, as the crate declares it. Not the product's, and
/// not what `--version` means (that selects the product release to install).
pub const OWN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A release is the installer's when its tag is the fixed `installer` or a
/// numbered `installer-vX.Y.Z`. Not a bare prefix test: these are the two
/// shapes this repository publishes, and the core's own filter
/// (`update::release::is_installer_tag`) is the same rule — one must follow
/// the other.
pub fn is_installer_tag(tag: &str) -> bool {
    tag == "installer" || tag.starts_with("installer-v")
}

/// `major.minor.patch`, digits only: what an installer tag may carry after
/// `installer-v`, and what this crate's own number must be. A prerelease
/// suffix, a fourth part, a sign, an empty part or a number that does not fit
/// are all "not a version" — a tag that is none is skipped, never guessed at.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let mut next = || {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u64>().ok()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// The version an installer tag names, or `None` for the fixed `installer`,
/// for a product tag, and for anything unreadable.
fn tag_version(tag: &str) -> Option<(u64, u64, u64)> {
    parse(tag.strip_prefix("installer-v")?)
}

/// The newest installer among `tags`, if it is strictly newer than `own`,
/// as `X.Y.Z`. Compared as numbers, so `0.10.0` is newer than `0.9.0`.
/// Equal, older, unreadable and absent all answer `None`.
pub fn newer_installer<'a>(tags: impl IntoIterator<Item = &'a str>, own: &str) -> Option<String> {
    let own = parse(own)?;
    let newest = tags.into_iter().filter_map(tag_version).max()?;
    (newest > own).then(|| format!("{}.{}.{}", newest.0, newest.1, newest.2))
}

/// The archive that is this system's installer, by the name the fixed release
/// carries it under: the full target triple, `.zip` on Windows. `None` for a
/// system the installer is not published for, which was built from source.
pub fn archive_name(os: &str, arch: &str) -> Option<String> {
    let (triple, ext) = match (os, arch) {
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", "zip"),
        ("macos", "aarch64") => ("aarch64-apple-darwin", "tar.gz"),
        ("macos", "x86_64") => ("x86_64-apple-darwin", "tar.gz"),
        ("linux", "x86_64") => ("x86_64-unknown-linux-musl", "tar.gz"),
        ("linux", "aarch64") => ("aarch64-unknown-linux-musl", "tar.gz"),
        _ => return None,
    };
    Some(format!("ritornello-install-{triple}.{ext}"))
}

/// Where the newest installer for `archive` is, with no version in it.
fn download_url(archive: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/installer/{archive}")
}

/// The sentence printed when a newer installer exists. `archive` is this
/// system's; a system with none is sent to the fixed release's page instead.
pub fn notice(newer: &str, own: &str, archive: Option<&str>) -> String {
    let link = match archive {
        Some(archive) => download_url(archive),
        None => format!("https://github.com/{REPO}/releases/tag/installer"),
    };
    format!("A newer ritornello-install exists ({newer}; this one is {own}): {link}")
}

/// What to print about a newer installer, given every tag the releases list
/// carries, or nothing. This installer's own number and system.
pub fn newer_installer_notice<'a>(tags: impl IntoIterator<Item = &'a str>) -> Option<String> {
    newer_installer_notice_for(tags, OWN_VERSION, std::env::consts::OS, std::env::consts::ARCH)
}

fn newer_installer_notice_for<'a>(
    tags: impl IntoIterator<Item = &'a str>,
    own: &str,
    os: &str,
    arch: &str,
) -> Option<String> {
    let newer = newer_installer(tags, own)?;
    Some(notice(&newer, own, archive_name(os, arch).as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: (&str, &str) = ("linux", "x86_64");

    fn notice_for(tags: &[&str], own: &str) -> Option<String> {
        newer_installer_notice_for(tags.iter().copied(), own, LINUX.0, LINUX.1)
    }

    #[test]
    fn a_newer_installer_is_announced_with_this_systems_archive() {
        assert_eq!(
            notice_for(&["v0.2.0", "installer", "installer-v0.3.1", "installer-v0.3.0"], "0.2.0").as_deref(),
            Some(
                "A newer ritornello-install exists (0.3.1; this one is 0.2.0): \
                 https://github.com/skerdudou/ritornello/releases/download/installer/\
                 ritornello-install-x86_64-unknown-linux-musl.tar.gz"
            )
        );
        // The patch digit alone is enough.
        assert!(notice_for(&["installer-v0.2.1"], "0.2.0").is_some());
    }

    #[test]
    fn the_newest_is_found_by_number_and_not_by_text_or_by_order() {
        // `0.10.0` sorts below `0.9.0` as text; the list's own order is not
        // trusted either.
        let notice = notice_for(&["installer-v0.9.0", "installer-v0.10.0", "installer-v0.2.5"], "0.9.0").unwrap();
        assert!(notice.contains("(0.10.0; this one is 0.9.0)"), "{notice}");
        let notice = notice_for(&["installer-v0.2.5", "installer-v0.10.0", "installer-v0.9.0"], "0.9.0").unwrap();
        assert!(notice.contains("(0.10.0;"), "{notice}");
    }

    #[test]
    fn an_equal_or_older_installer_says_nothing() {
        assert_eq!(notice_for(&["installer-v0.2.0"], "0.2.0"), None);
        assert_eq!(notice_for(&["installer-v0.2.0", "installer-v0.1.9"], "0.2.1"), None);
        assert_eq!(notice_for(&["installer-v0.9.9"], "0.10.0"), None);
        assert_eq!(notice_for(&["installer-v1.0.0"], "2.0.0"), None);
    }

    #[test]
    fn no_installer_tag_says_nothing() {
        assert_eq!(notice_for(&[], "0.2.0"), None);
        // The fixed release carries no number in its tag; a product tag is
        // not an installer's, whatever number it names.
        assert_eq!(notice_for(&["installer", "v9.9.9", "v0.2.1-beta.1"], "0.2.0"), None);
    }

    #[test]
    fn a_tag_that_cannot_be_read_is_ignored_and_never_hides_a_good_one() {
        for garbage in [
            "installer-v",
            "installer-v1",
            "installer-v1.2",
            "installer-v1.2.3.4",
            "installer-v1.2.x",
            "installer-vx.y.z",
            "installer-v+1.2.3",
            "installer-v-1.2.3",
            "installer-v1..3",
            "installer-v1.2.3-beta.1",
            "installer-v 1.2.3",
            "installer-v99999999999999999999.0.0",
            "installer-1.2.3",
            "Installer-v9.0.0",
            "installers-v9.0.0",
        ] {
            assert_eq!(notice_for(&[garbage], "0.2.0"), None, "{garbage}");
            // Beside a readable newer one, the readable one is still found.
            let notice = notice_for(&[garbage, "installer-v0.2.1"], "0.2.0")
                .unwrap_or_else(|| panic!("{garbage} hid a good tag"));
            assert!(notice.contains("(0.2.1;"), "{notice}");
        }
    }

    #[test]
    fn an_unreadable_own_number_says_nothing() {
        assert_eq!(notice_for(&["installer-v9.0.0"], "0.2.0-beta.3"), None);
        assert_eq!(notice_for(&["installer-v9.0.0"], ""), None);
    }

    #[test]
    fn this_crates_own_number_is_one_the_notice_can_compare() {
        assert!(parse(OWN_VERSION).is_some(), "{OWN_VERSION} is not a finished X.Y.Z");
        assert!(is_installer_tag("installer") && is_installer_tag("installer-v1.0.0"));
        assert!(!is_installer_tag("v0.2.0") && !is_installer_tag("installers"));
    }

    #[test]
    fn every_published_system_has_an_archive_the_readme_links() {
        let readme = include_str!("../../../README.md");
        let mut seen = 0;
        for (os, arch) in [
            ("windows", "x86_64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
        ] {
            let name = archive_name(os, arch).unwrap_or_else(|| panic!("no archive for {os} {arch}"));
            let link = format!("https://github.com/{REPO}/releases/download/installer/{name}");
            assert!(readme.contains(&link), "the README does not link {link}");
            seen += 1;
        }
        assert_eq!(seen, 5);
        // A system the installer is not published for is sent to the page.
        assert_eq!(archive_name("freebsd", "x86_64"), None);
        assert_eq!(archive_name("windows", "aarch64"), None);
        let n = newer_installer_notice_for(["installer-v9.0.0"], "0.2.0", "freebsd", "x86_64").unwrap();
        assert!(n.ends_with("/releases/tag/installer"), "{n}");
    }
}
