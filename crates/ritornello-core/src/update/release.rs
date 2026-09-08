//! What a GitHub release says about itself.
//!
//! Every function here is pure and reads a string: the network lives in
//! `download`. That split is what lets the shapes that actually break — an
//! asset name off convention, a rate-limit page instead of JSON, a release
//! that does not exist yet — be tested without a socket.
//!
//! **The release describes itself, and the core embeds no catalogue.** The
//! list of official plugins comes from the asset names, a plugin's
//! `plugins.toml` line comes from inside its archive, and whether it can be
//! installed from the UI is read off the archive's contents. A core has no
//! business knowing the composition of a version it is not.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Fixed at compile time, never read from a configuration file.
///
/// This is the authenticity anchor of the whole feature: HTTPS to GitHub, for
/// this repository and no other. A configurable repository would turn a
/// compromise of the unprivileged core into a compromise of what gets
/// installed.
pub const REPO: &str = "skerdudou/ritornello";

/// The architecture label this binary's archives carry.
///
/// Chosen by the compiler rather than probed at runtime: a binary that
/// downloads for an architecture other than its own is a binary that will not
/// start, and the mistake is not detectable at the moment it is made.
pub const ARCH: &str = if cfg!(target_arch = "arm") {
    "armv7"
} else if cfg!(target_arch = "aarch64") {
    "arm64"
} else {
    "x86_64"
};

/// GitHub requires a User-Agent and answers 403 without one. Same convention
/// as the six other outbound clients in this repository.
pub const USER_AGENT: &str = concat!(
    "ritornello/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/skerdudou/ritornello)"
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// What one asset offers, once its name has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    Core,
    Plugin(String),
    /// Every plugin at once. Never used by the updater — it installs component
    /// by component so a failure names one thing — but recognised so it is not
    /// mistaken for a plugin.
    Bundle,
}

/// The endpoint listing releases, newest first, one page.
///
/// The list and not "latest": a release carries only the components whose
/// version moved, so the newest one does not describe a component that did not
/// change. Folding the list is what recovers the current version of every
/// component — and the catalogue of what exists at all.
///
/// One hundred is the API's maximum for a single page, and the depth this
/// feature accepts: a component not published in the last hundred deliveries
/// would drop out of the catalogue. Written down rather than suffered.
pub fn releases_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases?per_page=100")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// ISO-8601 UTC, kept as text: it is only ever compared, and such
    /// timestamps sort chronologically as plain strings.
    pub published_at: String,
    pub assets: Vec<Asset>,
}

/// One component, as the fold found it: the newest published version, and
/// where to get it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub offer: Offer,
    /// The component's own version, read from the asset's name.
    pub version: String,
    pub url: String,
    pub size: u64,
    /// The release that carries this archive — not necessarily the newest one.
    pub release_tag: String,
    /// The `SHA256SUMS` of that same release, when it has one. The digest of
    /// an archive lives in the release that carries it.
    pub checksums_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleasesError {
    /// The body is not a list of releases. A failure to report, not a state.
    Unreadable,
    /// It parsed, and nothing in it is published. A state to display — "no
    /// release published" — and never a failure.
    NoRelease,
}

#[derive(Deserialize)]
struct WireAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Deserialize)]
struct WireRelease {
    tag_name: String,
    /// Null on a draft, which is filtered out before this is read.
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<WireAsset>,
}

/// Every published release of the body, drafts and prereleases dropped.
///
/// A draft is dropped because the workflow creates one: publishing is the
/// green light, so nothing can leave before a human has read the notes.
pub fn parse_releases(body: &str) -> Result<Vec<Release>, ReleasesError> {
    let wire: Vec<WireRelease> =
        serde_json::from_str(body).map_err(|_| ReleasesError::Unreadable)?;
    let out: Vec<Release> = wire
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .map(|r| Release {
            tag: r.tag_name,
            published_at: r.published_at.unwrap_or_default(),
            assets: r
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                })
                .collect(),
        })
        .collect();
    if out.is_empty() {
        return Err(ReleasesError::NoRelease);
    }
    Ok(out)
}

/// For each component, the newest release that carries an archive for this
/// architecture.
///
/// Sorted here rather than trusted from the API: GitHub documents the endpoint
/// as newest-first, but a change of that order would make the device install
/// old versions over new ones with nothing to notice. An ISO-8601 UTC
/// timestamp sorts chronologically as text, so the guard costs a string
/// comparison.
pub fn fold(releases: &[Release], arch: &str) -> Vec<Published> {
    let mut ordered: Vec<&Release> = releases.iter().collect();
    ordered.sort_by(|a, b| b.published_at.cmp(&a.published_at));

    let mut out: Vec<Published> = Vec::new();
    for release in ordered {
        let checksums = release
            .assets
            .iter()
            .find(|a| a.name == "SHA256SUMS")
            .map(|a| a.url.clone());
        for asset in &release.assets {
            let Some((offer, version)) = classify_asset(&asset.name, arch) else {
                continue;
            };
            // First wins: the list is newest-first, so a later release
            // carrying the same component is an older version of it.
            if out.iter().any(|p| p.offer == offer) {
                continue;
            }
            out.push(Published {
                offer,
                version,
                url: asset.url.clone(),
                size: asset.size,
                release_tag: release.tag.clone(),
                checksums_url: checksums.clone(),
            });
        }
    }
    out
}

/// Three dot-separated non-empty runs of digits, and nothing else.
///
/// Hand-rolled rather than a regex, like `valid_name` on the privileged side:
/// one dependency fewer, and the rule is short enough to read. It is what
/// stops `ritornello-plugin-radio-armv7.tar.gz` from being read as a plugin
/// named "radio" at version "armv7".
fn is_version(s: &str) -> bool {
    let mut parts = 0;
    for part in s.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        parts += 1;
    }
    parts == 3
}

/// `<base>-<version>-<arch>.tar.gz`, read from the right-hand side, yielding
/// the component AND the version it carries.
///
/// The version is extracted rather than matched: a release no longer has one
/// number that every archive in it shares, so nothing is known in advance to
/// compare a name against.
///
/// The bundle is tested BEFORE the singular prefix, and that order is the
/// point: `ritornello-plugins-` also starts with `ritornello-plugin`, so the
/// other order would read the bundle as a plugin named "s".
///
/// The plugin's version is split from the RIGHT, because three plugin names in
/// this repository contain a dash (`nrj-metas`, `generic-input`,
/// `radiofrance-metas`) while a version never does.
pub fn classify_asset(name: &str, arch: &str) -> Option<(Offer, String)> {
    let stem = name.strip_suffix(&format!("-{arch}.tar.gz"))?;
    if let Some(version) = stem.strip_prefix("ritornello-core-") {
        return is_version(version).then(|| (Offer::Core, version.to_string()));
    }
    if let Some(version) = stem.strip_prefix("ritornello-plugins-") {
        return is_version(version).then(|| (Offer::Bundle, version.to_string()));
    }
    let rest = stem.strip_prefix("ritornello-plugin-")?;
    let (plugin, version) = rest.rsplit_once('-')?;
    if plugin.is_empty() || plugin.starts_with('-') || !is_version(version) {
        return None;
    }
    Some((Offer::Plugin(plugin.to_string()), version.to_string()))
}

/// `<hex>  <name>` per line, as `sha256sum` writes it.
///
/// A malformed or blank line is skipped rather than fatal: a checksum file
/// that gained a comment must not make the whole release unreadable, and a
/// missing entry is caught later, when the archive it was for is verified.
pub fn parse_checksums(text: &str) -> BTreeMap<String, String> {
    let mut sums = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(digest) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        // No shape check on the digest here. It would have to accept whatever
        // the tests use, and the real verification is the comparison against
        // the archive's own hash — where a wrong digest fails for the right
        // reason instead of being rejected for its length.
        sums.insert(name.to_string(), digest.to_string());
    }
    sums
}

/// Is this component out of step with what is published?
///
/// `differs` and not `is_newer`. The fold gives the most recently published
/// version of each component, so the only question is alignment — which is
/// also the right question after a rollback has left a component behind, where
/// "newer" would answer wrongly. Ordering versions would be code and tests for
/// a question nobody asks.
pub fn differs(installed: Option<&str>, offered: &str) -> bool {
    installed != Some(offered)
}

/// The bare name a staged file gets, for one offer.
///
/// Must satisfy `ritornello_updater::request::valid_name`: lowercase, digits
/// and dashes only. That is why the plugin's name is used as is — plugin names
/// in this repository already obey that shape, and `valid_name` on the
/// privileged side is what refuses one that does not.
pub fn download_name(offer: &Offer) -> Option<String> {
    match offer {
        Offer::Core => Some("staged-core".to_string()),
        Offer::Plugin(name) => Some(format!("staged-plugin-{name}")),
        Offer::Bundle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asset_name_yields_its_component_and_its_own_version() {
        assert_eq!(
            classify_asset("ritornello-core-0.2.1-armv7.tar.gz", "armv7"),
            Some((Offer::Core, "0.2.1".to_string()))
        );
        assert_eq!(
            classify_asset("ritornello-plugin-radio-0.2.4-armv7.tar.gz", "armv7"),
            Some((Offer::Plugin("radio".to_string()), "0.2.4".to_string()))
        );
        assert_eq!(
            classify_asset("ritornello-plugins-0.2.7-armv7.tar.gz", "armv7"),
            Some((Offer::Bundle, "0.2.7".to_string()))
        );
    }

    /// The case the whole extraction rule exists for: three plugin names in
    /// this repository contain a dash, so the version cannot be found by
    /// splitting at the FIRST dash after the prefix.
    #[test]
    fn a_plugin_name_containing_dashes_survives_extraction() {
        for (name, plugin) in [
            ("ritornello-plugin-nrj-metas-0.2.1-armv7.tar.gz", "nrj-metas"),
            ("ritornello-plugin-generic-input-0.2.0-armv7.tar.gz", "generic-input"),
            (
                "ritornello-plugin-radiofrance-metas-0.3.12-armv7.tar.gz",
                "radiofrance-metas",
            ),
        ] {
            let (offer, _) = classify_asset(name, "armv7").expect(name);
            assert_eq!(offer, Offer::Plugin(plugin.to_string()), "{name}");
        }
    }

    #[test]
    fn a_name_that_is_not_a_component_archive_is_not_one() {
        for name in [
            // No version at all.
            "ritornello-plugin-radio-armv7.tar.gz",
            // Two-part version: a release must not be read from a name that
            // does not say which patch it is.
            "ritornello-plugin-radio-0.2-armv7.tar.gz",
            // Four parts.
            "ritornello-plugin-radio-0.2.0.1-armv7.tar.gz",
            // Not numeric.
            "ritornello-plugin-radio-x.y.z-armv7.tar.gz",
            // Empty plugin name.
            "ritornello-plugin--0.2.0-armv7.tar.gz",
            // Another architecture: downloading it would install a binary
            // that cannot start.
            "ritornello-plugin-radio-0.2.0-arm64.tar.gz",
            // No architecture.
            "ritornello-core-0.2.0.tar.gz",
            // Not an archive.
            "SHA256SUMS",
            // Close enough to matter.
            "ritornello-core-0.2.0-armv7.tar.gz.sig",
            "ritornello-plugin-0.2.0-armv7.tar.gz",
            "",
        ] {
            assert_eq!(classify_asset(name, "armv7"), None, "{name}");
        }
    }

    fn body(releases: &str) -> String {
        format!("[{releases}]")
    }

    fn rel(tag: &str, published_at: &str, draft: bool, pre: bool, assets: &[&str]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|n| {
                format!(
                    r#"{{"name":"{n}","browser_download_url":"https://x/{n}","size":10}}"#
                )
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","published_at":"{published_at}","draft":{draft},"prerelease":{pre},"assets":[{}]}}"#,
            assets.join(",")
        )
    }

    #[test]
    fn the_newest_release_that_carries_a_component_wins() {
        let text = body(&[
            rel("v0.2.7", "2026-09-08T10:00:00Z", false, false, &[
                "ritornello-plugin-radio-0.2.4-armv7.tar.gz",
                "SHA256SUMS",
            ]),
            rel("v0.2.6", "2026-08-01T10:00:00Z", false, false, &[
                "ritornello-core-0.2.1-armv7.tar.gz",
                "ritornello-plugin-radio-0.2.3-armv7.tar.gz",
                "SHA256SUMS",
            ]),
        ]
        .join(","));
        let published = fold(&parse_releases(&text).unwrap(), "armv7");

        // Two components, not three: radio appears in both releases and the
        // fold must keep only the newest. Asserted here rather than left to
        // another test, because `.find()` below would return the right entry
        // even with a stale duplicate sitting behind it — the test would pass
        // while the property it is named for was broken.
        assert_eq!(published.len(), 2, "one entry per component, not per asset");

        let radio = published
            .iter()
            .find(|p| p.offer == Offer::Plugin("radio".to_string()))
            .expect("radio");
        assert_eq!(radio.version, "0.2.4");
        assert_eq!(radio.release_tag, "v0.2.7");

        // The core did not move in v0.2.7, so it is still installable — from
        // the release that last carried it. This is the property that makes
        // "publish only what changed" workable, and the reason a published
        // release can never be deleted.
        let core = published.iter().find(|p| p.offer == Offer::Core).expect("core");
        assert_eq!(core.version, "0.2.1");
        assert_eq!(core.release_tag, "v0.2.6");
        assert_eq!(core.checksums_url.as_deref(), Some("https://x/SHA256SUMS"));
    }

    /// GitHub documents this endpoint as newest-first, but the fold does not
    /// take that on trust: an ISO-8601 UTC timestamp sorts chronologically as
    /// plain text, so ordering costs a string comparison and no date parsing.
    /// Were the order ever to change, the device would otherwise install old
    /// versions over new ones — silently.
    #[test]
    fn an_out_of_order_list_is_still_folded_newest_first() {
        let text = body(&[
            rel("v0.2.6", "2026-08-01T10:00:00Z", false, false, &[
                "ritornello-plugin-radio-0.2.3-armv7.tar.gz",
            ]),
            rel("v0.2.7", "2026-09-08T10:00:00Z", false, false, &[
                "ritornello-plugin-radio-0.2.4-armv7.tar.gz",
            ]),
        ]
        .join(","));
        let published = fold(&parse_releases(&text).unwrap(), "armv7");
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].version, "0.2.4");
    }

    #[test]
    fn drafts_and_prereleases_are_not_offered() {
        let text = body(&[
            rel("v0.2.8", "2026-09-09T10:00:00Z", true, false, &[
                "ritornello-plugin-radio-0.2.9-armv7.tar.gz",
            ]),
            rel("v0.2.7", "2026-09-08T10:00:00Z", false, true, &[
                "ritornello-plugin-radio-0.2.8-armv7.tar.gz",
            ]),
            rel("v0.2.6", "2026-08-01T10:00:00Z", false, false, &[
                "ritornello-plugin-radio-0.2.3-armv7.tar.gz",
            ]),
        ]
        .join(","));
        let published = fold(&parse_releases(&text).unwrap(), "armv7");
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].version, "0.2.3");
    }

    #[test]
    fn another_architecture_is_not_offered() {
        let text = body(&rel("v0.2.7", "2026-09-08T10:00:00Z", false, false, &[
            "ritornello-plugin-radio-0.2.4-arm64.tar.gz",
        ]));
        assert!(fold(&parse_releases(&text).unwrap(), "armv7").is_empty());
    }

    #[test]
    fn an_empty_list_is_no_release_and_not_a_failure() {
        assert_eq!(parse_releases("[]"), Err(ReleasesError::NoRelease));
        // Only drafts is the same answer: nothing has been published.
        let text = body(&rel("v0.2.8", "2026-09-09T10:00:00Z", true, false, &[]));
        assert_eq!(parse_releases(&text), Err(ReleasesError::NoRelease));
    }

    #[test]
    fn a_body_that_is_not_a_release_list_is_unreadable() {
        for text in ["", "{}", "not json", r#"{"message":"Not Found"}"#] {
            assert_eq!(parse_releases(text), Err(ReleasesError::Unreadable), "{text}");
        }
    }

    #[test]
    fn the_checksum_file_is_read_in_the_shape_sha256sum_writes() {
        // Two spaces, because that is what `sha256sum` emits for a binary
        // read. A single-space variant exists for text mode; accept both
        // rather than depend on which the runner used.
        let text = "\
abc123  ritornello-core-0.2.0-armv7.tar.gz
def456 ritornello-plugin-radio-0.2.0-armv7.tar.gz
";
        let sums = parse_checksums(text);
        assert_eq!(sums.get("ritornello-core-0.2.0-armv7.tar.gz").map(String::as_str), Some("abc123"));
        assert_eq!(sums.get("ritornello-plugin-radio-0.2.0-armv7.tar.gz").map(String::as_str), Some("def456"));
    }

    #[test]
    fn a_blank_or_malformed_checksum_line_is_skipped_not_fatal() {
        let sums = parse_checksums("\n   \nnot-a-line\nabc  ok.tar.gz\n");
        assert_eq!(sums.len(), 1);
        assert_eq!(sums.get("ok.tar.gz").map(String::as_str), Some("abc"));
    }

    /// `differs` and not `is_newer`, deliberately.
    ///
    /// The fold gives the most recently published version of each component,
    /// so the only question worth asking is "is this component aligned with
    /// what was folded", which also gives the right answer after a rollback
    /// has left a component behind. Ordering versions would be code, and
    /// tests, for a question nobody asks.
    #[test]
    fn alignment_is_what_is_compared_not_ordering() {
        assert!(!differs(Some("0.2.0"), "0.2.0"));
        assert!(differs(Some("0.1.9"), "0.2.0"));
        assert!(differs(Some("0.3.0"), "0.2.0"), "a component ahead of the release also differs");
        // A plugin that announced no version at all: it predates the field, so
        // it cannot be shown as aligned.
        assert!(differs(None, "0.2.0"));
    }
}
