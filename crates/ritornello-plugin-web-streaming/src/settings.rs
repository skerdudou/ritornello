//! `settings.toml` of the plugin's data directory: the release channel of
//! yt-dlp, when it was last checked, and what is installed.
//!
//! Keys this version does not know are kept as they are, so a writer that
//! only moves `last_check` never erases what the page saved next to it.

use crate::entries::write_atomically;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where yt-dlp comes from. Both are signed with the same key (checked on
/// the captures in `tests/fixtures/sums/`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Nightly,
}

impl Channel {
    /// The channel's releases.
    pub fn releases(self) -> &'static str {
        match self {
            Channel::Stable => "https://github.com/yt-dlp/yt-dlp/releases/",
            Channel::Nightly => "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/",
        }
    }

    /// Answers with a redirect to `<releases>tag/<the latest tag>`
    /// (measured: 302, on both channels).
    pub fn latest_url(self) -> String {
        format!("{}latest", self.releases())
    }

    /// Where the files of the release `tag` are served from. Everything of
    /// one update comes from one tag, never from two releases.
    pub fn download_base(self, tag: &str) -> String {
        format!("{}download/{tag}/", self.releases())
    }

    /// The tag a `latest` redirect names: only `<releases>tag/<tag>`, and a
    /// tag made of `[0-9A-Za-z._-]` only.
    pub fn tag_from_location(self, location: &str) -> Result<String, String> {
        let tag = location
            .strip_prefix(&format!("{}tag/", self.releases()))
            .ok_or_else(|| format!("unexpected latest release address: {location}"))?;
        let ok = !tag.is_empty()
            && tag.len() <= 64
            && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !tag.starts_with('.');
        if ok { Ok(tag.to_string()) } else { Err(format!("unexpected release tag: {tag:?}")) }
    }
}

/// What the last install put in place.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InstalledRecord {
    /// What `yt-dlp --version` answered once installed.
    pub version: String,
    pub channel: Channel,
    /// The SHA-256 of the asset, as the signed list gave it (lowercase hex).
    pub sha256: String,
    /// The release tag it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub channel: Channel,
    /// Unix seconds of the last check that reached a verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<InstalledRecord>,
    /// Whatever else is in the file, written back untouched.
    #[serde(flatten)]
    pub other: toml::Table,
}

impl Settings {
    /// The settings on disk; the defaults when the file is absent or
    /// unreadable (logged: the next check then simply downloads again).
    pub fn load(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("{} unreadable ({e}): defaults used", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomically(path, toml::to_string(self)?.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_keep_keys_they_do_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "channel = \"nightly\"\nlast_target = { root = \"nas\", path = \"music\" }\n").unwrap();
        let mut s = Settings::load(&path);
        assert_eq!(s.channel, Channel::Nightly);
        s.last_check = Some(42);
        s.installed = Some(InstalledRecord {
            version: "2026.10.26".into(),
            channel: Channel::Nightly,
            sha256: "ab".into(),
            tag: Some("2026.10.26".into()),
        });
        s.save(&path).unwrap();
        let again = Settings::load(&path);
        assert_eq!(again, s);
        assert_eq!(again.other["last_target"]["root"].as_str(), Some("nas"));
    }

    #[test]
    fn absent_or_garbage_settings_are_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, "channel = \"beta\"").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert_eq!(Settings::default().channel, Channel::Stable);
    }

    #[test]
    fn each_channel_has_its_own_repository() {
        assert_eq!(Channel::Stable.latest_url(), "https://github.com/yt-dlp/yt-dlp/releases/latest");
        assert_eq!(
            Channel::Nightly.download_base("2026.10.29.232701"),
            "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/download/2026.10.29.232701/"
        );
        assert_eq!(serde_json::to_string(&Channel::Nightly).unwrap(), "\"nightly\"");
    }

    /// The real redirects, as measured on 2026-11-02.
    #[test]
    fn the_latest_tag_is_read_from_the_redirect_and_nothing_else() {
        assert_eq!(
            Channel::Stable.tag_from_location("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.10.26"),
            Ok("2026.10.26".into())
        );
        assert_eq!(
            Channel::Nightly
                .tag_from_location("https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/tag/2026.10.29.232701"),
            Ok("2026.10.29.232701".into())
        );
        for bad in [
            "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/tag/2026.10.29", // the other channel
            "https://evil.example/yt-dlp/yt-dlp/releases/tag/2026.10.26",
            "https://github.com/yt-dlp/yt-dlp/releases/tag/",
            "https://github.com/yt-dlp/yt-dlp/releases/tag/../../x",
            "https://github.com/yt-dlp/yt-dlp/releases/tag/a/b",
            "https://github.com/yt-dlp/yt-dlp/releases/tag/a?b",
        ] {
            assert!(Channel::Stable.tag_from_location(bad).is_err(), "{bad}");
        }
    }
}
