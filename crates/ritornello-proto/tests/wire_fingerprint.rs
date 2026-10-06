//! The wire fingerprint: every message that crosses the core/plugin wire,
//! serialized from a deterministic sample and compared, whole, with a
//! committed fixture (`wire-fingerprint.txt`).
//!
//! **What this is for.** A device installs a component only when its version
//! differs, so a change to the wire that an old plugin cannot understand must
//! be a conscious decision: `PROTOCOL_VERSION` moves, every component that
//! links this crate moves with it, and the release script refuses to publish
//! otherwise. Without this test, renaming a field would compile, pass every
//! unit test of both ends, and ship a core that silently cannot talk to the
//! plugins already on the device.
//!
//! **What a red test means.** Either the wire changed (read the message the
//! failure prints) or `PROTOCOL_VERSION` changed without the fixture being
//! regenerated. Regenerate with `UPDATE_WIRE_FINGERPRINT=1 cargo test -p
//! ritornello-proto --test wire_fingerprint`, then read the diff of the
//! fixture: it is the exact record of what moved.
//!
//! **What is covered, and what is not.** Both directions of every sample:
//! the serialized form (compared with the fixture) and the read side (the
//! recorded JSON must parse back, round-trip, and tolerate an unknown field).
//! Not covered: a removed `alias`, a stricter validation of a value an old
//! plugin used to send, and any field this sample leaves unpopulated; keep
//! the samples fuller than the real traffic.
//!
//! The samples are deterministic: no `HashMap` with more than one entry, so
//! no iteration order can leak into the output. Every enum variant of the wire
//! appears at least once, and every optional field of the structs appears
//! populated in one sample and defaulted in another. `label` is an exhaustive
//! `match` on purpose: a new variant does not compile until it is named
//! here, which is the moment to give it a sample.

use ritornello_proto::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

fn one<K: Into<String>, V: Into<String>>(k: K, v: V) -> HashMap<String, String> {
    HashMap::from([(k.into(), v.into())])
}

fn command_label(c: &Command) -> &'static str {
    match c {
        Command::Select(_) => "Select",
        Command::Next => "Next",
        Command::Prev => "Prev",
        Command::VolumeUp => "VolumeUp",
        Command::VolumeDown => "VolumeDown",
        Command::Mute => "Mute",
        Command::SourceCycle => "SourceCycle",
        Command::PlayPause => "PlayPause",
        Command::Stop => "Stop",
        Command::Eject => "Eject",
        Command::Power => "Power",
        Command::Plus10 => "Plus10",
        Command::SeekForward => "SeekForward",
        Command::SeekBackward => "SeekBackward",
        Command::SeekTo(_) => "SeekTo",
        Command::SetVolume(_) => "SetVolume",
        Command::SelectSource(_) => "SelectSource",
        Command::ToggleRandom => "ToggleRandom",
        Command::CycleRepeat => "CycleRepeat",
        Command::SetRandom(_) => "SetRandom",
        Command::SetRepeat(_) => "SetRepeat",
    }
}

fn source_req_label(r: &SourceReq) -> &'static str {
    match r {
        SourceReq::Activate => "Activate",
        SourceReq::Wake => "Wake",
        SourceReq::Play => "Play",
        SourceReq::Deactivate => "Deactivate",
        SourceReq::Select(_) => "Select",
        SourceReq::Next => "Next",
        SourceReq::Prev => "Prev",
        SourceReq::Eject => "Eject",
        SourceReq::ListPresets => "ListPresets",
        SourceReq::Stop => "Stop",
        SourceReq::PlayerTrack(_) => "PlayerTrack",
        SourceReq::EndOfContent => "EndOfContent",
        SourceReq::SetPlayMode { .. } => "SetPlayMode",
        SourceReq::ArchiveCover { .. } => "ArchiveCover",
    }
}

fn source_action_label(a: &SourceAction) -> &'static str {
    match a {
        SourceAction::Noop => "Noop",
        SourceAction::Play { .. } => "Play",
        SourceAction::Stop => "Stop",
        SourceAction::PlayerNext => "PlayerNext",
        SourceAction::PlayerPrev => "PlayerPrev",
        SourceAction::PlayerChapter(_) => "PlayerChapter",
    }
}

fn admin_req_label(r: &AdminReq) -> &'static str {
    match r {
        AdminReq::GetAsset(_) => "GetAsset",
        AdminReq::GetData => "GetData",
        AdminReq::SetData(_) => "SetData",
        AdminReq::Ping => "Ping",
    }
}

fn admin_result_label(r: &AdminResult) -> &'static str {
    match r {
        AdminResult::Asset { .. } => "Asset",
        AdminResult::Data(_) => "Data",
        AdminResult::Set { .. } => "Set",
        AdminResult::Pong => "Pong",
        AdminResult::Expired => "Expired",
    }
}

fn frame_label(f: &DisplayFrame) -> &'static str {
    match f {
        DisplayFrame::State(_) => "State",
        DisplayFrame::Catalog(_) => "Catalog",
        DisplayFrame::Cover(_) => "Cover",
    }
}

fn overlay_label(o: &Overlay) -> &'static str {
    match o {
        Overlay::Volume { .. } => "Volume",
        Overlay::Tens { .. } => "Tens",
        Overlay::Message { .. } => "Message",
    }
}

fn link_label(l: &Link) -> &'static str {
    match l {
        Link::Youtube { .. } => "Youtube",
        Link::Deezer { .. } => "Deezer",
        Link::AppleMusic { .. } => "AppleMusic",
    }
}

fn cover_ref_label(c: &CoverRef) -> &'static str {
    match c {
        CoverRef::Url { .. } => "Url",
        CoverRef::Path { .. } => "Path",
    }
}

fn text_label(t: &Text) -> &'static str {
    match t {
        Text::Keyed { .. } => "Keyed",
        Text::Verbatim(_) => "Verbatim",
    }
}

fn identity_label(i: &IdentityUpdate) -> &'static str {
    match i {
        IdentityUpdate::Playing(_) => "Playing",
        IdentityUpdate::Nothing => "Nothing",
    }
}

fn kind_label(k: &PluginKind) -> &'static str {
    match k {
        PluginKind::Source => "Source",
        PluginKind::Display => "Display",
        PluginKind::Input => "Input",
        PluginKind::Metadata => "Metadata",
    }
}

fn playback_label(p: &Playback) -> &'static str {
    match p {
        Playback::Stopped => "Stopped",
        Playback::Playing => "Playing",
        Playback::Paused => "Paused",
    }
}

fn date_label(d: &DateFormat) -> &'static str {
    match d {
        DateFormat::DayMonthYear => "DayMonthYear",
        DateFormat::YearMonthDay => "YearMonthDay",
        DateFormat::MonthDayYear => "MonthDayYear",
    }
}

fn repeat_label(r: &Repeat) -> &'static str {
    match r {
        Repeat::Off => "Off",
        Repeat::All => "All",
        Repeat::One => "One",
    }
}

/// Appends one `name = json` line per sample.
struct Lines(Vec<String>);

impl Lines {
    /// Records one sample, and checks the read side of the wire as well:
    /// the recorded JSON must parse back into `T` and serialize again to the
    /// same text, and, when it is an object, must still parse with an
    /// unknown extra field in it (an old plugin must ignore what a newer core
    /// adds). That is what catches a `#[serde(default)]` removed from a field
    /// that is skipped when empty, or a `deny_unknown_fields` added: neither
    /// changes what is serialized, both break an old reader.
    fn add<T: Serialize + DeserializeOwned>(&mut self, name: &str, v: &T) {
        let json = serde_json::to_string(v).expect("a wire message always serializes");
        let back: T = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{name}: its own JSON no longer parses back ({e}): {json}"));
        let again = serde_json::to_string(&back).expect("a wire message always serializes");
        assert_eq!(again, json, "{name}: the JSON does not survive a round trip");
        if let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str::<serde_json::Value>(&json) {
            map.insert("an_unknown_future_field".into(), json!(1));
            let widened = serde_json::Value::Object(map).to_string();
            serde_json::from_str::<T>(&widened)
                .unwrap_or_else(|e| panic!("{name}: an unknown field is no longer ignored ({e})"));
        }
        self.0.push(format!("{name} = {json}"));
    }
}

fn track_full() -> Track {
    Track {
        artist: Some("Artist".into()),
        title: Some("Title".into()),
        album: Some("Album".into()),
        duration_s: Some(215),
        year: Some(1999),
        links: vec![Link::Youtube { url: "https://example.invalid/y".into() }],
        origin: Some("musicbrainz".into()),
        cover_href: Some("/cover/1".into()),
        cover_origin: Some("deezer".into()),
        provenance: Provenance {
            fields: BTreeMap::from([("title".to_string(), "musicbrainz".to_string())]),
            misses: vec!["album".into()],
            derived: BTreeMap::from([("year".to_string(), "title".to_string())]),
        },
    }
}

fn state_full() -> PlayerState {
    PlayerState {
        source: "radio".into(),
        volume: 42,
        muted: true,
        standby: true,
        preset: Some(3),
        preset_count: Some(10),
        preset_name: Some("Jazz".into()),
        status: Some("buffering".into()),
        overlay: Some(Overlay::Message { text: "hello".into(), remaining_ms: 1500 }),
        position_s: Some(12),
        playback: Playback::Playing,
        seekable: true,
        can_eject: true,
        has_finite_list: true,
        random: true,
        repeat: Repeat::One,
        clock: Clock { date: DateFormat::YearMonthDay, twelve_hour: true },
        track: track_full(),
    }
}

fn fingerprint() -> String {
    let mut l = Lines(Vec::new());

    // Commands and input.
    let commands = [
        Command::Select(3),
        Command::Next,
        Command::Prev,
        Command::VolumeUp,
        Command::VolumeDown,
        Command::Mute,
        Command::SourceCycle,
        Command::PlayPause,
        Command::Stop,
        Command::Eject,
        Command::Power,
        Command::Plus10,
        Command::SeekForward,
        Command::SeekBackward,
        Command::SeekTo(90),
        Command::SetVolume(55),
        Command::SelectSource("cd".into()),
        Command::ToggleRandom,
        Command::CycleRepeat,
        Command::SetRandom(true),
        Command::SetRepeat(Repeat::All),
    ];
    for c in &commands {
        l.add(&format!("Command::{}", command_label(c)), c);
    }
    l.add("InputMessage", &InputMessage::from(Command::Next));
    l.add("InputMessage::held", &InputMessage { cmd: Command::VolumeUp, held: true });
    for r in [Repeat::Off, Repeat::All, Repeat::One] {
        l.add(&format!("Repeat::{}", repeat_label(&r)), &r);
    }

    // Source requests and answers.
    let requests = [
        SourceReq::Activate,
        SourceReq::Wake,
        SourceReq::Play,
        SourceReq::Deactivate,
        SourceReq::Select(2),
        SourceReq::Next,
        SourceReq::Prev,
        SourceReq::Eject,
        SourceReq::ListPresets,
        SourceReq::Stop,
        SourceReq::PlayerTrack(4),
        SourceReq::EndOfContent,
        SourceReq::SetPlayMode { random: true, repeat: Repeat::All },
        SourceReq::ArchiveCover { identity: json!({"k": "v"}), file: "/tmp/c.jpg".into() },
    ];
    for r in &requests {
        l.add(&format!("SourceReq::{}", source_req_label(r)), r);
    }
    l.add("SourceRequest", &SourceRequest { id: 7, req: SourceReq::Next });
    l.add("Preset", &Preset { index: 1, name: "One".into() });

    let actions = [
        SourceAction::Noop,
        SourceAction::play("http://example.invalid/stream"),
        SourceAction::play("file:///a.mp3").starting_at(3).playlist().finite().loopable(),
        SourceAction::Stop,
        SourceAction::PlayerNext,
        SourceAction::PlayerPrev,
        SourceAction::PlayerChapter(2),
    ];
    for (i, a) in actions.iter().enumerate() {
        l.add(&format!("SourceAction::{}#{i}", source_action_label(a)), a);
    }

    let texts = [
        Text::Keyed { key: "k".into(), params: HashMap::new() },
        Text::Keyed { key: "k".into(), params: one("name", "x") },
        Text::Verbatim("plain".into()),
    ];
    for (i, t) in texts.iter().enumerate() {
        l.add(&format!("Text::{}#{i}", text_label(t)), t);
    }

    let identities = [IdentityUpdate::Playing(json!({"id": 1})), IdentityUpdate::Nothing];
    for i in &identities {
        l.add(&format!("IdentityUpdate::{}", identity_label(i)), i);
    }
    let covers = [
        CoverRef::Url { url: "https://example.invalid/c.jpg".into() },
        CoverRef::Path { path: "/cache/c.jpg".into() },
    ];
    for c in &covers {
        l.add(&format!("CoverRef::{}", cover_ref_label(c)), c);
    }
    let links = [
        Link::Youtube { url: "https://example.invalid/y".into() },
        Link::Deezer { url: "https://example.invalid/d".into() },
        Link::AppleMusic { url: "https://example.invalid/a".into() },
    ];
    for k in &links {
        l.add(&format!("Link::{}", link_label(k)), k);
    }

    l.add("SourceMessage::default", &SourceMessage::default());
    l.add(
        "SourceMessage::full",
        &SourceMessage {
            id: Some(9),
            action: Some(SourceAction::Stop),
            identity: Some(IdentityUpdate::Nothing),
            transient: true,
            preset: Some(2),
            preset_count: Some(10),
            preset_name: Some("Name".into()),
            status_text: Some(Text::Verbatim("s".into())),
            can_eject: Some(true),
            has_finite_list: Some(true),
            presets: Some(vec![Preset { index: 1, name: "One".into() }]),
            cover: Some(CoverRef::Path { path: "/c".into() }),
            cover_thumb: Some(CoverRef::Url { url: "https://example.invalid/t".into() }),
            cover_archivable: Some(true),
        },
    );

    // Registration.
    for k in [PluginKind::Source, PluginKind::Display, PluginKind::Input, PluginKind::Metadata] {
        l.add(&format!("PluginKind::{}", kind_label(&k)), &k);
    }
    l.add(
        "Announcement::minimal",
        &Announcement {
            name: "p".into(),
            kinds: vec![PluginKind::Source],
            admin: false,
            covers: false,
            ui_version: None,
            protocol: PROTOCOL_VERSION,
            version: None,
            repository: None,
            catalog: None,
        },
    );
    l.add(
        "Announcement::full",
        &Announcement {
            name: "p".into(),
            kinds: vec![PluginKind::Source, PluginKind::Display, PluginKind::Input, PluginKind::Metadata],
            admin: true,
            covers: true,
            ui_version: Some("abc123".into()),
            protocol: PROTOCOL_VERSION,
            version: Some("1.2.3".into()),
            repository: Some("https://example.invalid/r".into()),
            catalog: Some(HashMap::from([("en".to_string(), one("key", "text"))])),
        },
    );

    // Admin.
    let admin_reqs = [
        AdminReq::GetAsset("ui.js".into()),
        AdminReq::GetData,
        AdminReq::SetData(json!({"a": 1})),
        AdminReq::Ping,
    ];
    for r in &admin_reqs {
        l.add(&format!("AdminReq::{}", admin_req_label(r)), r);
    }
    l.add("AdminRequest", &AdminRequest { id: 1, deadline_ms: None, req: AdminReq::Ping });
    l.add("AdminRequest::deadline", &AdminRequest { id: 1, deadline_ms: Some(4000), req: AdminReq::GetData });
    let admin_results = [
        AdminResult::Asset { mime: "text/javascript".into(), body: Some("YQ==".into()) },
        AdminResult::Asset { mime: "text/javascript".into(), body: None },
        AdminResult::Data(json!({"a": 1})),
        AdminResult::Set { ok: true, error_text: None },
        AdminResult::Set { ok: false, error_text: Some(Text::Verbatim("no".into())) },
        AdminResult::Pong,
        AdminResult::Expired,
    ];
    for (i, r) in admin_results.iter().enumerate() {
        l.add(&format!("AdminResult::{}#{i}", admin_result_label(r)), r);
    }
    l.add("AdminResponse", &AdminResponse { id: 1, result: AdminResult::Pong });

    // Display, player state and metadata.
    let overlays = [
        Overlay::Volume { level: 30, muted: false, text: "30".into(), remaining_ms: 800 },
        Overlay::Tens { offset: 10, text: "+10".into(), remaining_ms: 800 },
        Overlay::Message { text: "m".into(), remaining_ms: 800 },
    ];
    for o in &overlays {
        l.add(&format!("Overlay::{}", overlay_label(o)), o);
    }
    for p in [Playback::Stopped, Playback::Playing, Playback::Paused] {
        l.add(&format!("Playback::{}", playback_label(&p)), &p);
    }
    for d in [DateFormat::DayMonthYear, DateFormat::YearMonthDay, DateFormat::MonthDayYear] {
        l.add(&format!("DateFormat::{}", date_label(&d)), &d);
    }
    l.add("Clock", &Clock { date: DateFormat::MonthDayYear, twelve_hour: true });
    l.add("Track::full", &track_full());
    l.add("Track::default", &Track::default());
    l.add("PlayerState::default", &PlayerState::default());
    l.add("PlayerState::full", &state_full());
    let frames = [
        DisplayFrame::State(state_full()),
        DisplayFrame::Catalog(SourcesCatalog {
            sources: vec![
                SourceCatalog { name: "radio".into(), presets: vec![Preset { index: 1, name: "One".into() }] },
                SourceCatalog { name: "cd".into(), presets: vec![] },
            ],
        }),
        DisplayFrame::Cover(Cover { href: "/cover/1".into(), mime: "image/jpeg".into(), bytes: vec![1, 2, 3] }),
    ];
    for f in &frames {
        l.add(&format!("DisplayFrame::{}", frame_label(f)), f);
    }
    l.add("NowPlaying::default", &NowPlaying::default());
    l.add(
        "NowPlaying::full",
        &NowPlaying {
            source: "radio".into(),
            identity: Some(json!({"station": "x"})),
            known: Known {
                artist: Some("A".into()),
                title: Some("T".into()),
                album: Some("Al".into()),
                duration_s: Some(10),
                year: Some(2001),
                cover: true,
                stream_title: Some("A - T".into()),
            },
        },
    );
    l.add("Enrichment::default", &Enrichment::default());
    l.add(
        "Enrichment::full",
        &Enrichment {
            identity: json!({"station": "x"}),
            artist: Some("A".into()),
            title: Some("T".into()),
            album: Some("Al".into()),
            duration_s: Some(10),
            year: Some(2001),
            links: vec![Link::Deezer { url: "https://example.invalid/d".into() }],
            position_s: Some(5),
            cover: Some(CoverRef::Url { url: "https://example.invalid/c".into() }),
            cover_thumb: Some(CoverRef::Path { path: "/t".into() }),
            fill_only: true,
            searched: true,
            derived_from: Some("title".into()),
        },
    );

    let mut text = format!("PROTOCOL_VERSION={PROTOCOL_VERSION}\n");
    for line in l.0 {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("wire-fingerprint.txt")
}

#[test]
fn the_wire_fingerprint_matches_the_committed_fixture() {
    let now = fingerprint();
    let path = fixture_path();
    if std::env::var_os("UPDATE_WIRE_FINGERPRINT").is_some() {
        std::fs::write(&path, &now).expect("the fixture is writable");
        return;
    }
    // Compared with line endings normalized: a Windows checkout may hand the
    // fixture back with CRLF, which says nothing about the wire.
    let fixture = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace("\r\n", "\n");

    let recorded = fixture
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("PROTOCOL_VERSION="))
        .and_then(|n| n.parse::<u32>().ok())
        .expect("the fixture's first line is PROTOCOL_VERSION=<n>");
    assert_eq!(
        recorded, PROTOCOL_VERSION,
        "PROTOCOL_VERSION is {PROTOCOL_VERSION} but the wire fingerprint was taken under {recorded}. \
         Regenerate it on purpose with UPDATE_WIRE_FINGERPRINT=1 \
         cargo test -p ritornello-proto --test wire_fingerprint, and read the diff of \
         tests/wire-fingerprint.txt: a bump of PROTOCOL_VERSION must go with a wire break, \
         and a wire break must go with a bump."
    );

    if now != fixture {
        let first = now
            .lines()
            .zip(fixture.lines())
            .find(|(a, b)| a != b)
            .map(|(a, b)| format!("now:      {a}\nrecorded: {b}"))
            .unwrap_or_else(|| "a sample was added or removed".to_string());
        panic!(
            "The wire format changed.\n{first}\n\n\
             If an old plugin can no longer understand it (renamed/removed field or variant, \
             changed type), this is a BREAK: bump PROTOCOL_VERSION in src/lib.rs and update this \
             fingerprint. If it is compatible (an added optional field, an added variant nobody \
             old receives), update the fingerprint only, and say why in the commit.\n\
             Regenerate with: UPDATE_WIRE_FINGERPRINT=1 cargo test -p ritornello-proto --test wire_fingerprint"
        );
    }
}
