use crate::metadata::IdentityUpdate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum SourceReq {
    Activate,
    /// Plugin-driven wake-up (boot / leaving standby). SDK-side default:
    /// behaves like `Activate`; a plugin may override `wake()`.
    Wake,
    /// The user asked to **play**, explicitly — the Play key, while nothing
    /// is loaded. SDK-side default: behaves like `Activate`; a plugin may
    /// override `play()`.
    ///
    /// Distinct from `Activate` because the two are different intentions
    /// that used to travel as one signal. `Activate` means "this source is
    /// now the one" — a source switch, or a boot — and a source is entitled
    /// to answer that by playing nothing. `Play` means "start now", and
    /// there is no reading of it under which playing nothing is right.
    ///
    /// The distinction was invisible as long as every source answered
    /// `Activate` by playing: the cd gained a setting whose default is to
    /// play nothing on arrival, and the Play key — which went through
    /// `Activate` — went inert with it. Rather than have the cd guess which
    /// of the two situations it was in, the core says which, since it is the
    /// only one that knows.
    Play,
    Deactivate,
    Select(u8),
    Next,
    Prev,
    Eject,
    SetLocale(String),
    /// Enumerate the named presets of this source.
    ///
    /// The correlated reply is a `Noop`: nothing in this pipe carries a list,
    /// `SourceReq` resolving to exactly one `SourceAction` across three layers,
    /// and `SourceClient` requiring `(Some(id), Some(action))` to release its
    /// `oneshot`. The list therefore travels **alongside** the action, in
    /// `SourceMessage::presets`, by the same route as `preset_count` — the
    /// "interesting frame" predicate, outside correlation.
    ListPresets,
    /// The core stopped playback on its own initiative (Stop key on the
    /// remote), **without** the Source having been consulted.
    ///
    /// It is the only command in this situation: `Play` goes through the core,
    /// `Eject` and `Deactivate` go through the Source. Without this
    /// notification, a Source that holds a playback state (the cd, to know
    /// whether a track is really playing) cannot stay accurate, and would
    /// announce metadata for a stopped track.
    Stop,
    /// The player moved **on its own** to the track at index `n` (end of a
    /// disc track), without any user command.
    ///
    /// The core learns it from mpv, but cannot correct the identity: it is
    /// opaque to it, and only the Source knows what "track n" means for what it
    /// is playing. Without this notification, the display and the metadata
    /// stayed on the previous track until the next command.
    PlayerTrack(i64),
}

/// A named preset. `index` is **1-based**, the one `Command::Select` expects,
/// and the sequence may be **sparse**: stations 1, 5 and 99 are legal. Never
/// deduce a rank from an index by subtraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub index: u8,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: SourceReq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "data")]
pub enum SourceAction {
    Noop,
    Play {
        uri: String,
        /// Starting index in the list `uri` designates, when it is one.
        ///
        /// Absent = "start at the beginning", the historical behaviour.
        ///
        /// **Requires `playlist: true`** to work, and that is a lesson paid
        /// for: a `loadfile` on an `.m3u` only unfolds it **afterwards** —
        /// measured, `playlist-count` is 1, then 3 only after an
        /// `end-file`/`start-file`. The position sent right after therefore
        /// arrived out of bounds, playback restarted from the first track,
        /// and the display lost everything. `loadlist` unfolds on the spot.
        ///
        /// The core carries this index **into** the load rather than
        /// correcting the position afterwards (see `Player::load_list`): the
        /// two-step version really did open the list's first entry, and the
        /// core announced it as the playing track for a moment.
        ///
        /// It is the only way for a Source to resume a list at track n — a
        /// digit from the remote, or resumption after a restart.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<i64>,
        /// `uri` designates a **playlist** and not a media item.
        ///
        /// The core then uses `loadlist`, which unfolds the list
        /// **synchronously**, instead of `loadfile` which first treats it as a
        /// single entry. The distinction cannot be guessed from the URI: an
        /// `.m3u8` is a list for a file player and an HLS stream for a radio,
        /// and getting it wrong breaks one or the other.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        playlist: bool,
        /// What `uri` designates has a **normal end**: a disc, a list of files.
        /// When mpv goes idle, it is the end of the content, not a stream cut
        /// to restart.
        ///
        /// Absent (= `false`) means "live stream", the historical behaviour:
        /// this is what keeps the radio's frames unchanged. Replaces the core's
        /// `uri.starts_with("cdda://")` sniffing, which guessed what only the
        /// Source knows.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        finite: bool,
    },
    Stop,
    PlayerNext,
    PlayerPrev,
}

impl SourceAction {
    /// Playback of a URI, with the historical defaults: from the beginning,
    /// live stream.
    ///
    /// Going through this constructor rather than the literal variant avoids a
    /// field added later forcing every caller to be touched.
    pub fn play(uri: impl Into<String>) -> Self {
        SourceAction::Play { uri: uri.into(), start: None, finite: false, playlist: false }
    }

    /// Positions playback on the element at index `n` of the list. No effect
    /// on an action that is not a `Play`.
    ///
    /// To be used with `playlist()`: without it, the URI is loaded as a single
    /// media item and the index arrives before the list exists.
    #[must_use]
    pub fn starting_at(self, n: i64) -> Self {
        match self {
            SourceAction::Play { uri, finite, playlist, .. } => {
                SourceAction::Play { uri, start: Some(n), finite, playlist }
            }
            other => other,
        }
    }

    /// Declares that the URI is a **playlist**, to be unfolded as such.
    #[must_use]
    pub fn playlist(self) -> Self {
        match self {
            SourceAction::Play { uri, start, finite, .. } => {
                SourceAction::Play { uri, start, finite, playlist: true }
            }
            other => other,
        }
    }

    /// Declares finite content, whose mpv idleness signals the end and not a
    /// cut. No effect on an action that is not a `Play`.
    #[must_use]
    pub fn finite(self) -> Self {
        match self {
            SourceAction::Play { uri, start, playlist, .. } => {
                SourceAction::Play { uri, start, finite: true, playlist }
            }
            other => other,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceMessage {
    /// `Some(id)` = reply correlated to a request; `None` = spontaneous notification.
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub action: Option<SourceAction>,
    /// Identity of what is playing **after** this action, when the Source has
    /// what it takes to update it.
    ///
    /// A CD changes track without a new `Play` (`PlayerNext` advances mpv), so
    /// the identity would change without any `Play` being emitted. Every
    /// occasion on which a Source reports something new (status, preset) thus
    /// becomes an occasion to correct the identity — which covers a disc's
    /// track change, the selection of a preset and the deferred arrival of a
    /// TOC.
    ///
    /// Absent = "this frame says nothing about the identity, keep the previous
    /// one".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityUpdate>,
    /// The status above is an **ephemeral** message: the core shows it for a
    /// few seconds, then brings the permanent status back.
    ///
    /// Without this, an incident message ("empty preset") stayed on screen
    /// indefinitely, until the user touched something else — while playback,
    /// for its part, continued on the previous station: the display durably
    /// described a state that no longer existed.
    ///
    /// The core uses the same slot and the same deadline as the volume/mute
    /// overlay, so the permanent status is kept as is and reappears on its
    /// own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub transient: bool,
    /// The numbered key of the remote that matches what is playing **after**
    /// this frame: the preset for the radio, the track for the cd. This is
    /// what lets the UI highlight the active key — information only the Source
    /// has, the core never interpreting what `Select(n)` meant.
    ///
    /// Absent = "this frame says nothing about the selection, keep the
    /// previous one". The core forgets it on its own when nothing is playing
    /// anymore (identity `Nothing`, stop, source change, standby): there is
    /// therefore no "cleared" form to declare here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<u8>,
    /// How many numbered presets the source currently offers: stations for
    /// the radio, tracks for the cd. This is what lets the web UI show only
    /// the numbers that exist instead of an unconditional 1-9 grid.
    ///
    /// Absent = "this frame says nothing about the count, keep the previous
    /// one". `Some(0)` is meaningful — "there is nothing to number" (cd
    /// without a disc) — and distinct from absent. The core forgets the
    /// remembered count on source change and standby (the next source
    /// re-declares it on activate/wake), but NOT on stop: a stopped radio
    /// still has its stations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_count: Option<u8>,
    /// The human-readable name the Source gives to the preset carried by
    /// `preset` above (the configured station name for the radio; the cd
    /// plugin never fills this in, since it has nothing to name here — see
    /// its metadata path instead).
    ///
    /// Absent = "this frame says nothing about the name, keep the previous
    /// one" — the same convention as `preset`. It lives and dies with
    /// `preset`: the core clears both together, and only when the identity
    /// is cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_name: Option<String>,
    /// The source's own word about its state, **already translated** by its
    /// catalog ("NO DISC", "AUDIO CD", "EMPTY PRESET").
    ///
    /// Unlike `preset`, absent means **"no status"**, not "keep the previous
    /// one": a source restates it on every frame, and this is the only
    /// convention that lets a status be cleared at all.
    ///
    /// With `transient` set, the status is an ephemeral message: it feeds the
    /// overlay and leaves the remembered status untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Whether this source has a tray to open at all — a **capability of the
    /// source**, not of what is loaded: an empty tray still ejects. It is what
    /// lets the web remote grey out its Eject button on a source that has
    /// nothing to eject, instead of sending a command the source will discard
    /// in silence.
    ///
    /// The SDK stamps it on **every** frame from `SourcePlugin::can_eject`, so
    /// a plugin author overrides one method and never has to remember a
    /// builder call on each declaration path. Absent = "this frame says
    /// nothing", keep the previous value — same convention as `preset_count`,
    /// so a hand-written plugin that ignores the field keeps working (the core
    /// then never leaves its `false` default, and offering nothing is the
    /// right answer when nobody claims the capability).
    ///
    /// Deliberately **not** part of the "is this frame worth forwarding"
    /// predicate in `SourceClient`: a frame carrying only a capability must
    /// stay inert, because a permanent frame without `status` *erases* the
    /// remembered status (see `status` above), and waking up frames that are
    /// dropped today would wipe "NO DISC" off the display. The capability
    /// therefore rides the frames the core already listens to — every path of
    /// a real source (activate, wake, select, next, prev, track change)
    /// declares an identity or a status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_eject: Option<bool>,
    /// The named presets of this source, when it knows how to enumerate them.
    /// Outside correlation, like `preset_count`: it is a fact about the source,
    /// not an answer to a question — and that is what avoids widening
    /// `pending`, `Source::request` and `active_request` to carry a list.
    ///
    /// Absent = "this frame says nothing about the presets, keep the current
    /// value". An **empty** list says the same thing — "this source has no
    /// names", the cd by nature, a track having no name without a database —
    /// and it is absence that travels: the sdk converts an empty list into
    /// absence (see the `ListPresets` arm of `serve_source`), so that the
    /// frame of a source that does not enumerate stays inert on the core side.
    /// Both forms remain **readable** at deserialization, a hand-written plugin
    /// being able to declare `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<Vec<Preset>>,
    /// Cover the Source found for what it is playing.
    ///
    /// This is what lets a Source declare its metadata **without becoming a
    /// `metadata` plugin**: it has the information, it says it on its channel.
    /// Sent as a notification (`id: None`) rather than as a reply to the
    /// `Play`, because finding it may require a `readdir` on an SMB share, and
    /// playback must not wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<crate::CoverRef>,
    /// A ready-made thumbnail for the cover above. See
    /// [`crate::Enrichment::cover_thumb`] for what it is, why it is optional
    /// and what the acceptance rule does and does not decide about it — one
    /// description, not two to drift apart.
    ///
    /// It travels **beside** `cover`, on the same frame and from the same
    /// declaration: a thumbnail on its own describes a cover nobody
    /// announced, and the core keeps the pair together for that reason.
    /// The same serde attributes as `cover`, so that absence stays absence on
    /// the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_thumb: Option<crate::CoverRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame declaring nothing.
    ///
    /// **The one exhaustive literal of this module, and it stays exhaustive
    /// deliberately**: a field added to `SourceMessage` no longer compiles
    /// until someone has named it here. Every other test used to repeat the
    /// same eleven `None`s, which multiplied the cost of an addition without
    /// asking any new question — the field had to be typed out six times to
    /// answer it once. They now declare only what they assert and fall back
    /// on `Default` for the rest.
    fn empty_message() -> SourceMessage {
        SourceMessage {
            id: None,
            action: None,
            identity: None,
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: None,
            can_eject: None,
            presets: None,
            cover: None,
            cover_thumb: None,
        }
    }

    #[test]
    fn list_presets_round_trips_as_a_request() {
        let r = SourceReq::ListPresets;
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"req":"ListPresets"}"#);
        assert_eq!(serde_json::from_str::<SourceReq>(&json).unwrap(), r);
    }

    #[test]
    fn presets_travel_alongside_the_action_not_inside_it() {
        // The property that avoids widening four types: the reply does carry
        // an `action` (so the correlation is released on the `SourceClient`
        // side, which requires `(Some(id), Some(action))`) AND the list
        // alongside. Without an action, the `oneshot` would wait out the 5 s
        // timeout for nothing.
        let msg = SourceMessage {
            id: Some(7),
            action: Some(SourceAction::Noop),
            presets: Some(vec![Preset { index: 1, name: "FIP".into() }]),
            ..empty_message()
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert!(back.action.is_some(), "without an action, the oneshot would wait 5 s for nothing");
        assert_eq!(back.id, Some(7));
        assert_eq!(
            back.presets.as_deref(),
            Some(&[Preset { index: 1, name: "FIP".into() }][..]),
            "{json}"
        );
    }

    #[test]
    fn a_sparse_list_travels_as_is() {
        // Presets are sparse (stations 1, 5, 99): nothing in the transport
        // must renumber or sort them, the dense rank being the consumer's
        // business.
        let msg = SourceMessage {
            presets: Some(vec![
                Preset { index: 5, name: "FIP".into() },
                Preset { index: 1, name: "Inter".into() },
                Preset { index: 99, name: "Info".into() },
            ]),
            ..empty_message()
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        let indices: Vec<u8> = back.presets.unwrap().iter().map(|p| p.index).collect();
        assert_eq!(indices, vec![5, 1, 99], "{json}");
        // And the shape on the wire, by name: `index` and `name`, without
        // renaming. A hand-written consumer (the MPD plugin) reads those two
        // keys.
        assert_eq!(
            serde_json::to_string(&Preset { index: 5, name: "FIP".into() }).unwrap(),
            r#"{"index":5,"name":"FIP"}"#
        );
    }

    // There is **no** test here on "empty list versus absent list". Both say
    // the same thing ("this source has no names"), and the choice to let only
    // one of them travel is made in the sdk, where two tests bite on it
    // (`a_source_that_does_not_enumerate_declares_no_list` on the server side,
    // `a_source_that_does_not_enumerate_does_not_wake_the_core` end to end). At
    // the protocol level, there would be nothing left to bite: serde already
    // reads a missing `Option` field back as `None` on its own,
    // `#[serde(default)]` or not — measured by removing it, no test moves — and
    // normalizing `[]` into `None` at read time would be legitimate rather than
    // faulty. A test forbidding it would be an obstacle, not a safeguard.

    #[test]
    fn an_absent_list_is_not_serialized() {
        // Nearly all frames say nothing about presets: weighing them down with
        // a `"presets":null` would be noise on a link meant to be readable by
        // eye.
        let m = SourceMessage { id: Some(2), ..empty_message() };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"id":2,"action":null}"#);
    }

    #[test]
    fn wake_roundtrip() {
        let r = SourceRequest { id: 4, req: SourceReq::Wake };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"req\":\"Wake\""));
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, SourceReq::Wake);
    }

    #[test]
    fn set_locale_roundtrip() {
        let r = SourceRequest { id: 9, req: SourceReq::SetLocale("fr".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"req\":\"SetLocale\""));
        assert!(json.contains("\"arg\":\"fr\""));
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, SourceReq::SetLocale("fr".into()));
    }

    #[test]
    fn play_without_new_fields_stays_serialized_identically() {
        // The compatibility guarantee: a frame emitted by the radio plugin
        // must not change by a single byte. Otherwise the traces of a
        // `journalctl` would become impossible to compare from one version to
        // the next, on a link meant to be readable by eye.
        let a = SourceAction::play("http://icecast.radiofrance.fr/fip-midfi.mp3");
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            r#"{"action":"Play","data":{"uri":"http://icecast.radiofrance.fr/fip-midfi.mp3"}}"#
        );
    }

    #[test]
    fn start_and_finite_round_trip() {
        let a = SourceAction::play("/var/lib/ritornello/plugin-files.m3u").starting_at(4).finite();
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains(r#""start":4"#), "{json}");
        assert!(json.contains(r#""finite":true"#), "{json}");
        let back: SourceAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn an_earlier_frame_reads_back_as_a_live_stream_from_the_beginning() {
        // An earlier plugin emits neither `start` nor `finite`: the defaults
        // must reproduce exactly the historical behaviour (live stream, start
        // of list), otherwise a partial update of the binaries would silently
        // change playback.
        let back: SourceAction =
            serde_json::from_str(r#"{"action":"Play","data":{"uri":"http://x"}}"#).unwrap();
        assert_eq!(
            back,
            SourceAction::Play {
                uri: "http://x".into(),
                start: None,
                finite: false,
                playlist: false
            }
        );
    }

    #[test]
    fn the_builders_do_not_touch_the_other_actions() {
        // `starting_at` and `finite` are written to be chainable without the
        // caller having to know which variant it holds. The safeguard: applied
        // elsewhere, they must turn nothing into a `Play`.
        assert_eq!(SourceAction::Stop.starting_at(3), SourceAction::Stop);
        assert_eq!(SourceAction::Noop.finite(), SourceAction::Noop);
        assert_eq!(SourceAction::PlayerNext.starting_at(1).finite(), SourceAction::PlayerNext);
    }

    #[test]
    fn request_roundtrip() {
        let r = SourceRequest { id: 7, req: SourceReq::Select(3) };
        let json = serde_json::to_string(&r).unwrap();
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.req, SourceReq::Select(3));
    }

    #[test]
    fn reply_message_with_action_and_identity() {
        let m = SourceMessage {
            id: Some(1),
            action: Some(SourceAction::play("http://fip")),
            identity: Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream"}))),
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, Some(1));
        assert_eq!(back.action, Some(SourceAction::play("http://fip")));
        assert_eq!(back.identity, m.identity);
    }

    #[test]
    fn notification_message_without_id() {
        let m = SourceMessage::default();
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, None);
        assert_eq!(back.action, None);
    }

    #[test]
    fn absent_identity_and_null_identity_do_not_say_the_same_thing() {
        // This is the raison d'être of the `IdentityUpdate` enum: "I say
        // nothing about the identity" (field omitted, so the current identity
        // is kept) must stay distinct from "nothing is playing anymore"
        // (`Nothing`, so the current identity is forgotten). An
        // `Option<Option<Value>>` would have mapped both to the same value at
        // deserialization.
        let says_nothing: SourceMessage = serde_json::from_str(r#"{"id":1}"#).unwrap();
        assert_eq!(says_nothing.identity, None);
        let stopped: SourceMessage =
            serde_json::from_str(r#"{"id":1,"identity":{"state":"Nothing"}}"#).unwrap();
        assert_eq!(stopped.identity, Some(IdentityUpdate::Nothing));
    }

    #[test]
    fn the_selection_round_trips_and_stays_absent_by_default() {
        // Round trip of the field, and compatibility: a frame from an earlier
        // plugin (without the field) must read back as "nothing declared".
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            preset: Some(4),
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"preset\":4"));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preset, Some(4));
        let old: SourceMessage = serde_json::from_str(r#"{"id":3}"#).unwrap();
        assert_eq!(old.preset, None);
    }

    #[test]
    fn absent_identity_is_not_serialized() {
        // Most frames say nothing about the identity (SetLocale, Deactivate…):
        // weighing them down with an `"identity":null` would be noise on a
        // link deliberately readable by eye.
        let m = SourceMessage { id: Some(2), ..Default::default() };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"id":2,"action":null}"#);
    }

    #[test]
    fn the_eject_capability_round_trips_and_stays_absent_by_default() {
        let m = SourceMessage {
            id: Some(4),
            action: Some(SourceAction::Noop),
            can_eject: Some(true),
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"can_eject\":true"), "{json}");
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.can_eject, Some(true));
        // Frame from a plugin predating the field: nothing declared, and not
        // "false". The core does not distinguish the two, but the protocol
        // must — this is what lets a silent frame not withdraw the capability.
        let old: SourceMessage = serde_json::from_str(r#"{"id":4}"#).unwrap();
        assert_eq!(old.can_eject, None);
        // An explicit `false` is distinct from absence, and travels.
        let refusal: SourceMessage = serde_json::from_str(r#"{"id":4,"can_eject":false}"#).unwrap();
        assert_eq!(refusal.can_eject, Some(false));
    }

    #[test]
    fn the_count_round_trips_and_stays_absent_by_default() {
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            preset_count: Some(23),
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"preset_count\":23"));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preset_count, Some(23));
        // Frame from an earlier plugin: nothing declared.
        let old: SourceMessage = serde_json::from_str(r#"{"id":3}"#).unwrap();
        assert_eq!(old.preset_count, None);
        // Some(0) is meaningful (cd without a disc) and must travel as is,
        // distinct from absence.
        let zero: SourceMessage = serde_json::from_str(r#"{"id":3,"preset_count":0}"#).unwrap();
        assert_eq!(zero.preset_count, Some(0));
    }

    #[test]
    fn the_name_round_trips_and_stays_absent_by_default() {
        // Round trip of the field, with a matching preset: that is how the
        // radio plugin always declares it (see `play_preset`).
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            preset: Some(4),
            preset_name: Some("FIP".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"preset_name\":\"FIP\""));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preset_name.as_deref(), Some("FIP"));
    }

    #[test]
    fn a_frame_from_an_earlier_plugin_without_preset_name_reads_back_as_nothing_declared() {
        // Backward compatibility: a plugin that does not know this field yet
        // (or a frame that says nothing about the name) must deserialize
        // without error, the field falling back to `None` — "keep the current
        // value", not "clear it".
        let old: SourceMessage = serde_json::from_str(r#"{"id":3,"preset":4}"#).unwrap();
        assert_eq!(old.preset_name, None);
        assert_eq!(old.preset, Some(4));
    }

    #[test]
    fn the_source_message_carries_a_cover_and_stays_silent_without_one() {
        let msg = SourceMessage {
            id: None,
            cover: Some(ritornello_proto_test_cover()),
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""kind":"path""#), "{json}");
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cover, msg.cover);

        // Additive: a silent frame stays byte-for-byte identical to what it
        // was before this work.
        let silent = SourceMessage::default();
        assert!(!serde_json::to_string(&silent).unwrap().contains("cover"));
    }

    #[test]
    fn the_source_message_carries_a_thumb_beside_its_cover() {
        // The pair on the Source's channel, and the same two properties as on
        // the enrichment side: it travels when present, and absence stays
        // absence. The production change that would make the last assertion
        // fail: dropping `skip_serializing_if`, which would make every frame
        // a source has ever emitted grow by a `"cover_thumb":null`.
        let msg = SourceMessage {
            cover: Some(crate::CoverRef::Url { url: "https://example.org/front.jpg".into() }),
            cover_thumb: Some(crate::CoverRef::Url {
                url: "https://example.org/front-500.jpg".into(),
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("front-500.jpg"), "{json}");
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cover_thumb, msg.cover_thumb);

        // A frame from a plugin that knows nothing of the field reads back,
        // the thumbnail simply absent.
        let old: SourceMessage =
            serde_json::from_str(r#"{"cover":{"kind":"path","path":"/nas/a/folder.jpg"}}"#)
                .unwrap();
        assert!(old.cover.is_some());
        assert!(old.cover_thumb.is_none());
        assert!(!serde_json::to_string(&SourceMessage::default()).unwrap().contains("cover_thumb"));
    }

    /// Local factory: avoids repeating the path in several tests.
    fn ritornello_proto_test_cover() -> crate::CoverRef {
        crate::CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }
    }

    #[test]
    fn the_status_round_trips_and_stays_absent_by_default() {
        // Different convention from `preset`/`preset_name`: here absence is
        // tested on a frame that explicitly declares `status: None` (a Source
        // that has nothing more to say about its state), not on a frame from
        // an earlier plugin — see `Core::handle_source_update` for the reason.
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            status: Some("NO DISC".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"status\":\"NO DISC\""));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status.as_deref(), Some("NO DISC"));
        // A frame from an earlier plugin (or one that says nothing about the
        // status) reads back without error, the field falling back to `None`.
        let old: SourceMessage = serde_json::from_str(r#"{"id":3}"#).unwrap();
        assert_eq!(old.status, None);
    }
}
