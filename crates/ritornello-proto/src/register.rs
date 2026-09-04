//! A plugin's announcement line, written on the core's registration socket
//! right after the plugin has bound its own sockets.
//!
//! The order matters and it is structural: the sockets are bound by the SDK
//! constructor, the announcement is only written by `Runtime::run`. So when
//! the core reads this line, it knows both which kinds exist and that the
//! corresponding sockets already accept a connection.

use serde::{Deserialize, Serialize};

/// What a plugin can do. The kind is a property of the **binary**, announced
/// by it, and not a configuration line the operator would have to know (see
/// the same trade-off made for the admin page).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Display,
    Input,
    /// Enriches what the active Source plays without the Source knowing.
    ///
    /// **Order matters** between two `metadata` plugins that answer for the
    /// same track: the first one in `plugins.toml` wins. That order now comes
    /// from the manifest alone, the announcement does not carry it — see
    /// `ritornello-core::register`.
    Metadata,
}

/// One announcement, one line of JSON, one plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Taken verbatim from `--name`. Used to correlate N announcements
    /// arriving on a single socket; the authority on the name stays with the
    /// manifest.
    pub name: String,
    pub kinds: Vec<PluginKind>,
    /// `false` by default: a plugin without an admin page may omit the field.
    #[serde(default)]
    pub admin: bool,
    /// Does this display want to receive the cover bytes?
    ///
    /// Same idiom as `admin` just above, and for the same reason: `false` by
    /// default, so the most common announcement stays the shortest to write,
    /// and a core predating this field reads it back without seeing anything
    /// new.
    ///
    /// **Opt-in, not a default**: a cover weighs up to
    /// `display::COVER_MAX_BYTES`, and a twenty-column display has no use for
    /// it. The core only pushes the bytes to the displays that asked for them,
    /// rather than sending them to all and letting each one throw them away.
    ///
    /// The flag is **derived** from what the plugin registered, never asked of
    /// the caller: see `Runtime::display` in the SDK, which reads
    /// `DisplayPlugin::wants_covers`. That is the invariant of the
    /// registration protocol — the announcement cannot lie.
    #[serde(default)]
    pub covers: bool,
    /// Fingerprint of this plugin's UI assets (`ui.js` **and** `ui.css`
    /// together), so the shell can serve them from a URL that never needs
    /// revalidating.
    ///
    /// Carried by the announcement rather than fetched afterwards: the plugin
    /// already holds those bytes (`include_str!`), so this costs no round
    /// trip — and, like `covers`, it is **derived** from what was registered,
    /// so the announcement cannot lie.
    ///
    /// One fingerprint for both files: they come from the same build and move
    /// together. If either changes, both are refetched — an over-invalidation
    /// worth twice the simplicity.
    ///
    /// `None` = plugin without an admin page, or one predating this field: the
    /// shell then builds an unstamped URL and the old revalidation applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_serialize_in_lowercase() {
        let a = Announcement {
            name: "mpd".into(),
            kinds: vec![PluginKind::Input, PluginKind::Display],
            admin: true,
            covers: true,
            ui_version: None,
        };
        let line = serde_json::to_string(&a).unwrap();
        assert_eq!(
            line,
            r#"{"name":"mpd","kinds":["input","display"],"admin":true,"covers":true}"#
        );
        assert_eq!(serde_json::from_str::<Announcement>(&line).unwrap(), a);
    }

    #[test]
    fn absent_admin_means_false() {
        // A plugin without a page may omit the field: the most common
        // announcement must stay the shortest to write.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"cd","kinds":["source"]}"#).unwrap();
        assert!(!a.admin);
        assert_eq!(a.kinds, vec![PluginKind::Source]);
    }

    #[test]
    fn absent_covers_means_false() {
        // The same idiom as `admin`, and the same consequence: an announcement
        // written before this field — the console's, an external plugin's —
        // reads back without error and **without** asking for covers. That is
        // what protects the twenty-column display.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"console","kinds":["display"],"admin":false}"#).unwrap();
        assert!(!a.covers, "an absent field must never count as an opt-in");
    }

    #[test]
    fn an_unknown_kind_is_an_error_not_a_silence() {
        // A typo in a plugin binary must be reported, not absorbed into a
        // default kind.
        assert!(serde_json::from_str::<Announcement>(r#"{"name":"x","kinds":["sourec"]}"#).is_err());
    }

    #[test]
    fn several_kinds_survive_the_roundtrip() {
        let a = Announcement {
            name: "double".into(),
            kinds: vec![PluginKind::Source, PluginKind::Metadata],
            admin: false,
            covers: false,
            ui_version: None,
        };
        let back: Announcement =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn an_announcement_without_a_ui_version_reads_back() {
        // Additive field, same idiom as `admin` and `covers`: a core predating it
        // reads an old line without seeing anything new, and a plugin without an
        // admin page never writes it.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"x","kinds":["source"]}"#).unwrap();
        assert_eq!(a.ui_version, None);
    }

    #[test]
    fn a_ui_version_survives_a_round_trip() {
        let a = Announcement {
            name: "radio".into(),
            kinds: vec![PluginKind::Source],
            admin: true,
            covers: false,
            ui_version: Some("deadbeef".into()),
        };
        let line = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Announcement>(&line).unwrap(), a);
    }
}
