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
    /// Protocol this binary was compiled against, compared by the core against
    /// its own `PROTOCOL_VERSION`.
    ///
    /// Derived, never asked: the SDK writes it from the constant, so a plugin
    /// author cannot get it wrong and cannot lie about it — the invariant of
    /// this whole handshake.
    ///
    /// Absent = `1`, and that is not a fallback but a definition: the field
    /// was introduced while the protocol was at 1, so an announcement written
    /// before it existed describes protocol 1 exactly. The core therefore
    /// always compares a number, never an `Option`.
    #[serde(default = "default_protocol")]
    pub protocol: u32,
    /// Version of the plugin binary itself.
    ///
    /// Relayed to the configuration page so the operator can see **what is
    /// actually installed**: nothing forbids replacing a single plugin, and in
    /// that case the versions on the device legitimately differ.
    ///
    /// `None` = a binary predating this field — same idiom as `ui_version`,
    /// and for the same reason: "unknown" is a real state, which an empty
    /// string would silently turn into a claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Where this binary's releases live, as its own manifest gives it —
    /// today a full URL, because that is what `CARGO_PKG_REPOSITORY` holds.
    ///
    /// **Derived, never asked**, like `version`, `protocol` and `covers`: the
    /// SDK reads `CARGO_PKG_REPOSITORY` from the plugin's own manifest. That
    /// is the invariant of this handshake — the announcement cannot lie.
    ///
    /// Relayed **verbatim**, and the core is what interprets it: this crate
    /// depends on nothing, and normalising a URL here would bake a GitHub
    /// convention into the protocol. `ritornello_core::update::release`
    /// parses it (`parse_repo_url`, then `origin`) before comparing it to
    /// anything.
    ///
    /// It is also the whole of the third-party story. A plugin announcing
    /// **our** repository is one of ours; anything else is third-party, and
    /// the core offers to check it against that repository rather than
    /// against this release. There is no flag to declare and nothing for an
    /// operator to configure.
    ///
    /// `None` = a plugin whose manifest names no repository, or one predating
    /// this field: it is simply left alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

/// Serde needs a function, not a literal, for a non-zero default.
fn default_protocol() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::PROTOCOL_VERSION;

    #[test]
    fn kinds_serialize_in_lowercase() {
        let a = Announcement {
            name: "mpd".into(),
            kinds: vec![PluginKind::Input, PluginKind::Display],
            admin: true,
            covers: true,
            ui_version: None,
            protocol: PROTOCOL_VERSION,
            version: Some("0.2.0".into()),
            repository: Some("https://github.com/skerdudou/ritornello".into()),
        };
        let line = serde_json::to_string(&a).unwrap();
        assert_eq!(
            line,
            r#"{"name":"mpd","kinds":["input","display"],"admin":true,"covers":true,"protocol":1,"version":"0.2.0","repository":"https://github.com/skerdudou/ritornello"}"#
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
    fn an_absent_protocol_means_one() {
        // The same idiom as `absent_covers_means_false`, and it is what lets a
        // binary built before this field ever existed keep being read. One is
        // not a guess: it names the protocol as it stood when the field was
        // introduced, so silence and "1" mean exactly the same thing forever.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"cd","kinds":["source"]}"#).unwrap();
        assert_eq!(a.protocol, 1, "silence must read back as the original protocol");
    }

    #[test]
    fn an_absent_version_stays_unknown() {
        // `None` and not a string: a plugin predating the field says nothing
        // about its version, and inventing one here would be the announcement
        // lying — the very thing this protocol forbids.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"cd","kinds":["source"]}"#).unwrap();
        assert_eq!(a.version, None);
    }

    #[test]
    fn the_protocol_travels_and_the_version_is_omitted_when_unknown() {
        // Two opposite conventions on purpose, each matching a neighbouring
        // field: `protocol` is always written (like `admin`), because a reader
        // must never have to guess it; `version` is omitted when absent (like
        // `ui_version`), because "unknown" is a real state that an empty
        // string would erase.
        let a = Announcement {
            name: "radio".into(),
            kinds: vec![PluginKind::Source],
            admin: true,
            covers: false,
            ui_version: None,
            protocol: PROTOCOL_VERSION,
            version: None,
            repository: None,
        };
        let line = serde_json::to_string(&a).unwrap();
        assert!(line.contains(r#""protocol":1"#), "the protocol must always travel: {line}");
        assert!(!line.contains("version\":null"), "an unknown version is omitted, not null: {line}");
        assert_eq!(serde_json::from_str::<Announcement>(&line).unwrap(), a);
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
            protocol: PROTOCOL_VERSION,
            version: None,
            repository: None,
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
            protocol: PROTOCOL_VERSION,
            version: None,
            repository: None,
        };
        let line = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Announcement>(&line).unwrap(), a);
    }

    #[test]
    fn an_absent_repository_stays_unknown() {
        // Same idiom as `version`, and the same consequence: a binary built
        // before this field says nothing about where its releases live, and
        // inventing "ours" for it would let any silent plugin pass for
        // official.
        let line = r#"{"name":"x","kinds":["source"]}"#;
        let a: Announcement = serde_json::from_str(line).unwrap();
        assert_eq!(a.repository, None);
    }
}
