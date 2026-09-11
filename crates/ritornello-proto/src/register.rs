//! A plugin's announcement line, written on the core's registration socket
//! right after the plugin has bound its own sockets.
//!
//! The order matters and it is structural: the sockets are bound by the SDK
//! constructor, the announcement is only written by `Runtime::run`. So when
//! the core reads this line, it knows both which kinds exist and that the
//! corresponding sockets already accept a connection.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// this field. The core does not go asking anybody's repository about it —
    /// there is none to ask — but it does **not** leave the row alone: with
    /// nothing announced to say otherwise, the component is judged against the
    /// core's own release, because that is also the state of every plugin that
    /// is switched off, dead, or not yet installed, and those rows have to
    /// stay installable. A third-party plugin that wants to be judged by its
    /// own repository must announce one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// This plugin's own translation layers, language → key → value —
    /// exactly what it holds **embedded** in the binary (`include_str!`);
    /// its on-disk packs are a separate story the core reads for itself by
    /// scanning the packs root, never asked of the plugin.
    ///
    /// **Derived, never asked**, like `covers` and `ui_version`: a plugin
    /// confides its raw TOML sources to the SDK (see
    /// `ritornello_plugin_sdk::Runtime`'s texts-registering method), which
    /// parses and validates them at build time, before this line is ever
    /// written — so the announcement cannot lie, and a broken pack is
    /// refused before a socket even opens rather than discovered on screen.
    ///
    /// `None` and `Some({})` are two different facts, deliberately kept
    /// apart:
    /// - `None` — a binary **predating this field entirely**. It does not
    ///   mean "no text": it means the plugin never had the chance to say.
    ///   The one place this matters in practice is `PROTOCOL_VERSION`
    ///   staying at 1 across this whole effort (an explicit choice, not an
    ///   oversight — see its own doc): nothing at the wire level refuses
    ///   such a plugin, so the core names it instead, on the Système page
    ///   (`PluginStatus::catalog_unknown`), rather than letting it degrade
    ///   in silence.
    /// - `Some({})` — a module that genuinely **has no text of its own**.
    ///   Three plugins ship this way today (`console`, `ouifm-metas`,
    ///   `radiofrance-metas`), and it is what a plugin built against this
    ///   SDK but never calling the texts-registering method announces: an
    ///   up-to-date binary with nothing to confide is not the same fact as
    ///   an old one that was never asked, and an empty string (or an empty
    ///   table standing in for "unknown") would erase exactly that
    ///   distinction — the same reasoning `ui_version` and `repository`
    ///   already document for their own `None`.
    ///
    /// This is also what keeps a future completeness count honest: a
    /// denominator built from `Some(_)` alone would silently grow every
    /// time an old binary is answered for, rather than left out because
    /// nothing was ever confided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<HashMap<String, HashMap<String, String>>>,
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
            catalog: None,
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
            catalog: None,
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
            catalog: None,
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
            catalog: None,
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

    /// A minimal announcement for tests that only care about one field —
    /// `catalog` here — and want `..base_announcement()` to fill in the
    /// rest, rather than repeating all eight neighbouring fields verbatim.
    fn base_announcement() -> Announcement {
        Announcement {
            name: "x".into(),
            kinds: vec![PluginKind::Source],
            admin: false,
            covers: false,
            ui_version: None,
            protocol: PROTOCOL_VERSION,
            version: None,
            repository: None,
            catalog: None,
        }
    }

    /// `None` and `Some({})` are two different facts and the wire must keep them
    /// apart: `None` is a binary predating this field, `Some({})` a component
    /// that has no text at all (three plugins are in that case). Conflating them
    /// would make the completeness denominator wrong and would rob the core of
    /// its only way to name an outdated binary.
    #[test]
    fn an_absent_catalog_and_an_empty_one_do_not_serialise_the_same() {
        let absent = Announcement { catalog: None, ..base_announcement() };
        let empty = Announcement { catalog: Some(Default::default()), ..base_announcement() };
        let a = serde_json::to_string(&absent).unwrap();
        let e = serde_json::to_string(&empty).unwrap();
        assert!(!a.contains("catalog"), "absent must be skipped entirely: {a}");
        assert!(e.contains("catalog"), "empty must be present: {e}");
        let back_a: Announcement = serde_json::from_str(&a).unwrap();
        let back_e: Announcement = serde_json::from_str(&e).unwrap();
        assert_eq!(back_a.catalog, None);
        assert_eq!(back_e.catalog, Some(Default::default()));
    }

    /// A catalog carrying real text survives the round trip, nested map and
    /// all — the shape that makes this field worth having.
    #[test]
    fn a_populated_catalog_survives_a_round_trip() {
        let mut en = HashMap::new();
        en.insert("play".to_string(), "Play".to_string());
        let mut layers = HashMap::new();
        layers.insert("en".to_string(), en);
        let a = Announcement { catalog: Some(layers.clone()), ..base_announcement() };
        let line = serde_json::to_string(&a).unwrap();
        let back: Announcement = serde_json::from_str(&line).unwrap();
        assert_eq!(back.catalog, Some(layers));
    }
}
