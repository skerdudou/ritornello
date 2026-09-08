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

/// The endpoint that ignores drafts and prereleases, and answers 404 when no
/// release has been published.
pub fn latest_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub assets: Vec<Asset>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestError {
    /// The body parsed, but the release is a draft or a prerelease.
    NotPublished,
    /// Not a release at all: a rate-limit page, an error object, HTML.
    Malformed(String),
}

impl std::fmt::Display for LatestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPublished => write!(f, "the latest release is a draft or a prerelease"),
            Self::Malformed(d) => write!(f, "the release could not be read: {d}"),
        }
    }
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

pub fn parse_latest(body: &str) -> Result<Release, LatestError> {
    let raw: RawRelease = serde_json::from_str(body)
        .map_err(|e| LatestError::Malformed(e.to_string()))?;
    if raw.draft || raw.prerelease {
        return Err(LatestError::NotPublished);
    }
    // The archive names carry the bare version, and so does the workspace, so
    // the leading `v` of the tag is stripped here rather than at four call
    // sites.
    let version = raw.tag_name.strip_prefix('v').unwrap_or(&raw.tag_name).to_string();
    Ok(Release {
        tag: raw.tag_name,
        version,
        assets: raw
            .assets
            .into_iter()
            .map(|a| Asset { name: a.name, url: a.browser_download_url, size: a.size })
            .collect(),
    })
}

/// `<base>-<version>-<arch>.tar.gz`, read from the right-hand side.
///
/// The bundle is tested BEFORE the singular prefix, and that order is the
/// point: `ritornello-plugins-` also starts with `ritornello-plugin`, so the
/// other order would read the bundle as a plugin named "s".
pub fn classify_asset(name: &str, version: &str, arch: &str) -> Option<Offer> {
    let suffix = format!("-{version}-{arch}.tar.gz");
    let stem = name.strip_suffix(&suffix)?;
    if stem == "ritornello-core" {
        return Some(Offer::Core);
    }
    if stem == "ritornello-plugins" {
        return Some(Offer::Bundle);
    }
    let plugin = stem.strip_prefix("ritornello-plugin-")?;
    // An empty name would come from `ritornello-plugin--0.2.0-...`, and a
    // name starting with a dash from a malformed build. Neither is a plugin.
    if plugin.is_empty() || plugin.starts_with('-') {
        return None;
    }
    Some(Offer::Plugin(plugin.to_string()))
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

/// Is this component out of step with the release?
///
/// `differs` and not `is_newer`. The repository has one version and the
/// endpoint always returns the latest published release, so the only question
/// is alignment — which is also the right question after a rollback has left a
/// component behind. Ordering versions would be code and tests for a question
/// nobody asks.
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
    fn each_asset_shape_is_recognised() {
        assert_eq!(
            classify_asset("ritornello-core-0.2.0-armv7.tar.gz", "0.2.0", "armv7"),
            Some(Offer::Core)
        );
        assert_eq!(
            classify_asset("ritornello-plugin-radio-0.2.0-armv7.tar.gz", "0.2.0", "armv7"),
            Some(Offer::Plugin("radio".to_string()))
        );
        assert_eq!(
            classify_asset("ritornello-plugin-nrj-metas-0.2.0-armv7.tar.gz", "0.2.0", "armv7"),
            Some(Offer::Plugin("nrj-metas".to_string()))
        );
        assert_eq!(
            classify_asset("ritornello-plugins-0.2.0-armv7.tar.gz", "0.2.0", "armv7"),
            Some(Offer::Bundle)
        );
    }

    /// The one that would have bitten. `ritornello-plugins-` and
    /// `ritornello-plugin-` differ by a single letter, and reading the bundle
    /// as a plugin named "s" would offer the operator a plugin that does not
    /// exist — then fail to install it, with no clue why.
    #[test]
    fn the_bundle_is_never_read_as_a_plugin_named_s() {
        assert_eq!(
            classify_asset("ritornello-plugins-0.2.0-armv7.tar.gz", "0.2.0", "armv7"),
            Some(Offer::Bundle)
        );
    }

    #[test]
    fn another_architecture_or_another_version_is_not_ours() {
        assert_eq!(classify_asset("ritornello-core-0.2.0-arm64.tar.gz", "0.2.0", "armv7"), None);
        assert_eq!(classify_asset("ritornello-core-0.1.9-armv7.tar.gz", "0.2.0", "armv7"), None);
    }

    #[test]
    fn anything_off_convention_is_ignored_rather_than_guessed() {
        for name in [
            "SHA256SUMS",
            "ritornello-core-0.2.0-armv7.zip",
            "ritornello-plugin--0.2.0-armv7.tar.gz",
            "ritornello-plugin-0.2.0-armv7.tar.gz",
            "notes.md",
            "",
        ] {
            assert_eq!(classify_asset(name, "0.2.0", "armv7"), None, "{name:?}");
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

    #[test]
    fn the_release_body_gives_the_tag_the_version_and_the_assets() {
        let body = r#"{
          "tag_name": "v0.3.0",
          "draft": false,
          "prerelease": false,
          "assets": [
            { "name": "ritornello-core-0.3.0-armv7.tar.gz", "browser_download_url": "https://example/core", "size": 4096 },
            { "name": "SHA256SUMS", "browser_download_url": "https://example/sums", "size": 512 }
          ]
        }"#;
        let release = parse_latest(body).expect("a well-formed release parses");
        assert_eq!(release.tag, "v0.3.0");
        // The version is the tag without its leading `v`: that is what the
        // archive names carry, and what the workspace version is.
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "ritornello-core-0.3.0-armv7.tar.gz");
        assert_eq!(release.assets[0].url, "https://example/core");
    }

    #[test]
    fn a_tag_without_the_leading_v_is_taken_as_is() {
        let body = r#"{ "tag_name": "0.3.0", "draft": false, "prerelease": false, "assets": [] }"#;
        assert_eq!(parse_latest(body).unwrap().version, "0.3.0");
    }

    /// The endpoint filters drafts and prereleases itself, so these two flags
    /// should never arrive set. Refusing them anyway is cheap, and the
    /// alternative is a device that installs something nobody has read the
    /// notes for — the exact gate the draft release exists to be.
    #[test]
    fn a_draft_or_a_prerelease_is_refused_even_though_the_endpoint_filters_them() {
        let draft = r#"{ "tag_name": "v0.3.0", "draft": true, "prerelease": false, "assets": [] }"#;
        assert!(matches!(parse_latest(draft), Err(LatestError::NotPublished)));
        let pre = r#"{ "tag_name": "v0.3.0", "draft": false, "prerelease": true, "assets": [] }"#;
        assert!(matches!(parse_latest(pre), Err(LatestError::NotPublished)));
    }

    #[test]
    fn a_body_that_is_not_a_release_is_a_malformed_error_and_not_a_panic() {
        assert!(matches!(parse_latest("null"), Err(LatestError::Malformed(_))));
        assert!(matches!(parse_latest("<html>rate limited</html>"), Err(LatestError::Malformed(_))));
    }

    /// `differs` and not `is_newer`, deliberately.
    ///
    /// The repository has ONE version, and the endpoint always returns the
    /// latest published release. So the only question worth asking is "is this
    /// component aligned with that release", which also gives the right answer
    /// after a rollback has left a component behind. Ordering versions would
    /// be code, and tests, for a question nobody asks.
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
