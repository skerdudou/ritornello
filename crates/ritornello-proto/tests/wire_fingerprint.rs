//! The wire fingerprint: every message that crosses the core/plugin wire,
//! serialized from a deterministic sample and compared, whole, with a
//! committed fixture (`wire-fingerprint.txt`).
//!
//! **Sections.** The fixture has one section per contract (`source`,
//! `display`, `input`, `metadata`, `admin`), each introduced by a header that
//! carries that contract's version (`[source 1.0]`), preceded by the
//! announcement (`[announcement protocol=2]`). A type that travels in several
//! contracts (`Repeat`, `Preset`, `Text`, `CoverRef`, `Link`) is sampled
//! again in each of them, on purpose: changing it changes every section it
//! travels in, and each of those contracts demands its own decision.
//!
//! **What this is for.** A device installs a component only when its version
//! differs, so a change to the wire that an old plugin cannot understand must
//! be a conscious decision. Without this test, renaming a field would
//! compile, pass every unit test of both ends, and ship a core that silently
//! cannot talk to the plugins already on the device.
//!
//! **The rule.** A contract section's content cannot change unless its
//! version moves: the MAJOR for a break, the MINOR for a compatible addition.
//! The test refuses that change even when the fixture is being regenerated
//! (`UPDATE_WIRE_FINGERPRINT=1`), since regenerating must not be a way
//! around the decision, and it compares versions as (major, minor): the
//! recorded one must be strictly lower, so a bump, a regeneration and a
//! revert of the constant do not get around it. Bump the version in
//! `src/contract.rs` first, then regenerate and read the diff of the
//! fixture: it is the exact record of what moved.
//!
//! The fixture itself is guarded: one that cannot be read, a header that
//! names no section, a section named twice, lines before the first header, or
//! a contract section missing from it are refused, never regenerated from.
//! The one deliberate way to add a contract: move `PROTOCOL_VERSION` in
//! `src/lib.rs` (a contract name is a closed enum, so an older core cannot
//! read an announcement naming a new one, and everything is republished),
//! put an empty `[<name> 0.0]` header in the fixture by hand, then
//! regenerate. The samples of the announcement use fixed contract versions,
//! so that a contract bump does not rewrite it.
//!
//! **The announcement differs.** It is the bootstrap, read before any
//! contract is known, and it has no version of its own to bump: an addition
//! is compatible (regenerate and say why in the commit), a break moves
//! `PROTOCOL_VERSION`, which republishes everything.
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

fn play_request_label(r: &PlayRequest) -> &'static str {
    match r {
        PlayRequest::IfActive => "IfActive",
        PlayRequest::Switch => "Switch",
        PlayRequest::WakeAndSwitch => "WakeAndSwitch",
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

fn contract_label(c: &Contract) -> &'static str {
    match c {
        Contract::Source => "Source",
        Contract::Display => "Display",
        Contract::Input => "Input",
        Contract::Metadata => "Metadata",
        Contract::Admin => "Admin",
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
        location: Some("loc".into()),
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

// Shared types: each helper records the samples of one type, and is called
// once in every section the type travels in.

fn add_repeat(l: &mut Lines) {
    for r in [Repeat::Off, Repeat::All, Repeat::One] {
        l.add(&format!("Repeat::{}", repeat_label(&r)), &r);
    }
}

fn add_preset(l: &mut Lines) {
    l.add("Preset", &Preset { index: 1, name: "One".into() });
}

fn add_texts(l: &mut Lines) {
    let texts = [
        Text::Keyed { key: "k".into(), params: HashMap::new() },
        Text::Keyed { key: "k".into(), params: one("name", "x") },
        Text::Verbatim("plain".into()),
    ];
    for (i, t) in texts.iter().enumerate() {
        l.add(&format!("Text::{}#{i}", text_label(t)), t);
    }
}

fn add_cover_refs(l: &mut Lines) {
    let covers = [
        CoverRef::Url { url: "https://example.invalid/c.jpg".into() },
        CoverRef::Path { path: "/cache/c.jpg".into() },
    ];
    for c in &covers {
        l.add(&format!("CoverRef::{}", cover_ref_label(c)), c);
    }
}

fn add_links(l: &mut Lines) {
    let links = [
        Link::Youtube { url: "https://example.invalid/y".into() },
        Link::Deezer { url: "https://example.invalid/d".into() },
        Link::AppleMusic { url: "https://example.invalid/a".into() },
    ];
    for k in &links {
        l.add(&format!("Link::{}", link_label(k)), k);
    }
}

fn announcement_section() -> Vec<String> {
    let mut l = Lines(Vec::new());
    for k in [PluginKind::Source, PluginKind::Display, PluginKind::Input, PluginKind::Metadata] {
        l.add(&format!("PluginKind::{}", kind_label(&k)), &k);
    }
    for c in Contract::ALL {
        l.add(&format!("Contract::{}", contract_label(&c)), &c);
    }
    l.add("ContractVersion", &ContractVersion::new(1, 2));
    l.add(
        "Announcement::minimal",
        &Announcement {
            name: "p".into(),
            kinds: vec![PluginKind::Source],
            admin: false,
            covers: false,
            ui_version: None,
            protocol: PROTOCOL_VERSION,
            contracts: BTreeMap::new(),
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
            contracts: Contract::ALL.into_iter().map(|c| (c, ContractVersion::new(1, 0))).collect(),
            version: Some("1.2.3".into()),
            repository: Some("https://example.invalid/r".into()),
            catalog: Some(HashMap::from([("en".to_string(), one("key", "text"))])),
        },
    );
    l.0
}

fn source_section() -> Vec<String> {
    let mut l = Lines(Vec::new());
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
    add_preset(&mut l);

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

    add_texts(&mut l);
    let identities = [IdentityUpdate::Playing(json!({"id": 1})), IdentityUpdate::Nothing];
    for i in &identities {
        l.add(&format!("IdentityUpdate::{}", identity_label(i)), i);
    }
    add_cover_refs(&mut l);
    add_repeat(&mut l);

    for r in [PlayRequest::IfActive, PlayRequest::Switch, PlayRequest::WakeAndSwitch] {
        l.add(&format!("PlayRequest::{}", play_request_label(&r)), &r);
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
            location: Some("loc".into()),
            status_text: Some(Text::Verbatim("s".into())),
            can_eject: Some(true),
            has_finite_list: Some(true),
            presets: Some(vec![Preset { index: 1, name: "One".into() }]),
            cover: Some(CoverRef::Path { path: "/c".into() }),
            cover_thumb: Some(CoverRef::Url { url: "https://example.invalid/t".into() }),
            cover_archivable: Some(true),
            play_request: Some(PlayRequest::Switch),
        },
    );
    l.0
}

fn display_section() -> Vec<String> {
    let mut l = Lines(Vec::new());
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
    add_links(&mut l);
    add_repeat(&mut l);
    add_preset(&mut l);
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
    l.0
}

fn input_section() -> Vec<String> {
    let mut l = Lines(Vec::new());
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
    add_repeat(&mut l);
    l.0
}

fn metadata_section() -> Vec<String> {
    let mut l = Lines(Vec::new());
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
    add_cover_refs(&mut l);
    add_links(&mut l);
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
    l.0
}

fn admin_section() -> Vec<String> {
    let mut l = Lines(Vec::new());
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
    add_texts(&mut l);
    l.0
}

/// The wire name of a contract, as it appears in a section header.
fn contract_name(c: Contract) -> &'static str {
    match c {
        Contract::Source => "source",
        Contract::Display => "display",
        Contract::Input => "input",
        Contract::Metadata => "metadata",
        Contract::Admin => "admin",
    }
}

/// Produces the sample lines of one section.
type SectionFn = fn() -> Vec<String>;

/// The whole fixture: the announcement first, then one section per contract,
/// each introduced by its header line.
fn fingerprint() -> String {
    let mut text = String::new();
    let mut push = |header: String, lines: Vec<String>| {
        text.push_str(&header);
        text.push('\n');
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
    };
    push(format!("[announcement protocol={PROTOCOL_VERSION}]"), announcement_section());
    let contract_sections: [(Contract, SectionFn); 5] = [
        (Contract::Source, source_section),
        (Contract::Display, display_section),
        (Contract::Input, input_section),
        (Contract::Metadata, metadata_section),
        (Contract::Admin, admin_section),
    ];
    for (contract, section) in contract_sections {
        push(format!("[{} {}]", contract_name(contract), contract.current()), section());
    }
    text
}

/// The command that regenerates the fixture, quoted in every refusal.
const REGENERATE: &str = "UPDATE_WIRE_FINGERPRINT=1 cargo test -p ritornello-proto --test wire_fingerprint";

fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("wire-fingerprint.txt")
}

/// One section of the fixture: its header line and its sample lines.
struct Section {
    header: String,
    lines: Vec<String>,
}

/// Splits a fixture into its sections. A line starting with `[` opens one.
/// A line before the first header is filed under an empty header, which
/// names no section and is refused by the test.
fn sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for line in text.lines() {
        if line.starts_with('[') {
            out.push(Section { header: line.to_string(), lines: Vec::new() });
        } else {
            if out.is_empty() {
                out.push(Section { header: String::new(), lines: Vec::new() });
            }
            out.last_mut().expect("a section was just pushed").lines.push(line.to_string());
        }
    }
    out
}

/// The section's name: `[source 1.0]` is `source`, `[announcement protocol=2]`
/// is `announcement`. The version is what a header adds to the name.
fn section_name(header: &str) -> &str {
    header.trim_start_matches('[').trim_end_matches(']').split(' ').next().unwrap_or("")
}

/// Two headers name the same section, whatever their versions.
fn same_section(a: &str, b: &str) -> bool {
    !section_name(a).is_empty() && section_name(a) == section_name(b)
}

/// A contract section, as opposed to the announcement.
fn is_contract(header: &str) -> bool {
    Contract::ALL.iter().any(|c| contract_name(*c) == section_name(header))
}

/// The mark a contract header carries while its version has been in no
/// release: `[source 1.1 next]`. Such a section may still change in place —
/// the owner's rule: an unpublished version is "next", completed rather than
/// bumped. Whether a marked version was in fact published is checked at
/// release time by `scripts/changed-components.sh`, which can see git.
const UNPUBLISHED: &str = "next";

#[test]
fn a_header_reads_with_or_without_the_unpublished_mark() {
    assert_eq!(header_version("[source 1.1]"), Some((1, 1)));
    assert_eq!(header_version("[source 1.1 next]"), Some((1, 1)));
    assert_eq!(header_version("[source 1.1 later]"), None, "only `next` is a mark");
    assert_eq!(header_version("[source 1.1 next next]"), None, "one mark at most");
    assert!(is_unpublished("[source 1.1 next]"));
    assert!(!is_unpublished("[source 1.1]"));
    assert!(!is_unpublished("[announcement protocol=2]"));
}

#[test]
fn the_written_header_marks_what_no_release_carried() {
    let rec = |h: &str, lines: &[&str]| Section { header: h.into(), lines: lines.iter().map(|l| l.to_string()).collect() };
    // Raised: the new version is published nowhere.
    assert_eq!(written_header("[source 1.2]", &rec("[source 1.1]", &["a"])), "[source 1.2 next]");
    // Not raised: the recorded mark is kept, present or absent.
    assert_eq!(written_header("[source 1.1]", &rec("[source 1.1 next]", &["a"])), "[source 1.1 next]");
    assert_eq!(written_header("[source 1.1]", &rec("[source 1.1]", &["a"])), "[source 1.1]");
}

/// The `(major, minor)` of a contract header such as `[source 1.0]` or
/// `[source 1.1 next]`; `None` when the header carries no well-formed version,
/// or a third word other than the unpublished mark.
fn header_version(header: &str) -> Option<(u32, u32)> {
    let inner = header.strip_prefix('[')?.strip_suffix(']')?;
    let mut words = inner.split(' ');
    words.next()?;
    let (major, minor) = words.next()?.split_once('.')?;
    match (words.next(), words.next()) {
        (None, None) | (Some(UNPUBLISHED), None) => {}
        _ => return None,
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The header carries the unpublished mark.
fn is_unpublished(header: &str) -> bool {
    header.trim_start_matches('[').trim_end_matches(']').split(' ').nth(2) == Some(UNPUBLISHED)
}

/// The header this run records for a contract section: the generated one
/// (never marked), plus the mark when its version is unpublished — raised
/// above the recorded one, or recorded as unpublished and not raised.
fn written_header(generated: &str, recorded: &Section) -> String {
    let raised = match (header_version(generated), header_version(&recorded.header)) {
        (Some(now), Some(before)) => now > before,
        _ => false,
    };
    if raised || is_unpublished(&recorded.header) {
        format!("{} {UNPUBLISHED}]", generated.trim_end_matches(']'))
    } else {
        generated.to_string()
    }
}

/// The constant to edit for a contract section, named in the refusals.
fn contract_constant(header: &str) -> String {
    format!("{}_CONTRACT", section_name(header).to_uppercase())
}

fn first_difference(recorded: &[String], now: &[String]) -> String {
    for (i, (a, b)) in now.iter().zip(recorded.iter()).enumerate() {
        if a != b {
            return format!("  line {}\n  now:      {a}\n  recorded: {b}", i + 1);
        }
    }
    match now.len().cmp(&recorded.len()) {
        std::cmp::Ordering::Greater => format!("  a sample was added: {}", now[recorded.len()]),
        std::cmp::Ordering::Less => format!("  a sample was removed: {}", recorded[now.len()]),
        std::cmp::Ordering::Equal => "  (none)".to_string(),
    }
}

fn first_difference_text(recorded: &str, now: &str) -> String {
    let r: Vec<String> = recorded.lines().map(String::from).collect();
    let n: Vec<String> = now.lines().map(String::from).collect();
    first_difference(&r, &n)
}

#[test]
fn the_wire_fingerprint_matches_the_committed_fixture() {
    let now = fingerprint();
    let path = fixture_path();
    // Compared with line endings normalized: a Windows checkout may hand the
    // fixture back with CRLF, which says nothing about the wire.
    // A fixture that cannot be read is a defect, never a blank to regenerate
    // from: regenerating from nothing would accept any change.
    let recorded_text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "cannot read the wire fingerprint fixture {} ({e}). A missing fixture is a defect: \
                 restore it from git (git checkout -- <path>); do not regenerate it from nothing.",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    let update = std::env::var_os("UPDATE_WIRE_FINGERPRINT").is_some();
    let recorded = sections(&recorded_text);
    let current = sections(&now);

    let mut refusals = Vec::new();

    // The recorded fixture must be well formed and must not hold a section the
    // test no longer produces (dropped or renamed here, or a header typo).
    for r in &recorded {
        let name = section_name(&r.header);
        if r.header.is_empty() {
            refusals.push(
                "the fixture has lines before its first section header; restore it from git".to_string(),
            );
        } else if recorded.iter().filter(|o| same_section(&o.header, &r.header)).count() > 1 {
            // Only the first of two same-named sections would be compared, so a
            // lower header slipped above the real one would let any change of
            // the contract through: a fixture names each section once.
            refusals.push(format!(
                "{}: the fixture names the section `{name}` more than once; restore it from git.",
                r.header
            ));
        } else if !current.iter().any(|s| same_section(&s.header, &r.header)) {
            refusals.push(format!(
                "{}: the fixture holds a section named `{name}` that this test does not produce \
                 (an unknown name, a typo in a header, or a contract dropped or renamed here). \
                 Restore the fixture from git, or fix the test.",
                r.header
            ));
        } else if is_contract(&r.header) && header_version(&r.header).is_none() {
            refusals.push(format!("{}: the header of a contract section must read `[<name> <major>.<minor>]` or `[<name> <major>.<minor> next]`.", r.header));
        }
    }

    for s in &current {
        let Some(r) = recorded.iter().find(|r| same_section(&r.header, &s.header)) else {
            if is_contract(&s.header) {
                refusals.push(format!(
                    "{}: this contract has no section in the fixture. Adding a contract is a deliberate, \
                     rare change: it moves PROTOCOL_VERSION in crates/ritornello-proto/src/lib.rs (an older \
                     core cannot read an announcement naming a new contract), then add an empty header \
                     `[{} 0.0]` to the fixture by hand, then regenerate with: {REGENERATE}. \
                     (If the fixture was damaged, restore it from git instead.)",
                    s.header,
                    section_name(&s.header)
                ));
            } else {
                refusals.push(format!(
                    "{}: the fixture has no such section; restore it from git.",
                    s.header
                ));
            }
            continue;
        };
        // A contract section whose content moved while its version did not
        // move UP: a decision was skipped, unless the recorded header carries
        // the `next` mark: a version no release has carried may still change
        // in place. Compared as (major, minor), so
        // bumping, regenerating, then putting the constant back and
        // regenerating again is refused too. A version that went DOWN is
        // refused even when the lines are identical: the test cannot see git,
        // so a lower header over the content of a higher one would record
        // that content under the old version, which is the same bypass. A
        // legitimate revert of a bump restores the fixture from git instead.
        // Refused even under UPDATE_WIRE_FINGERPRINT.
        if is_contract(&s.header) {
            let ok = match (header_version(&s.header), header_version(&r.header)) {
                (Some(now_v), Some(before_v)) => now_v > before_v || (now_v == before_v && (r.lines == s.lines || is_unpublished(&r.header))),
                _ => false,
            };
            if !ok {
                refusals.push(format!(
                    "{}: the messages of this contract changed but its version did not move up, \
                     or its version moved down (recorded: {}). A version never goes down: restore the \
                     fixture from git.\n  A break (an old peer would misread it): bump the MAJOR of \
                     {} in crates/ritornello-proto/src/contract.rs.\n  A compatible addition: bump its \
                     MINOR.\n  A version no release has carried may change in place: its header in the \
                     fixture reads `[<name> <major>.<minor> next]`, and `scripts/changed-components.sh` \
                     refuses a release that still carries the mark or that changed a published section.\n  \
                     Then regenerate with: {REGENERATE}\n  First difference:\n{}",
                    s.header,
                    r.header,
                    contract_constant(&s.header),
                    first_difference(&r.lines, &s.lines)
                ));
            }
        }
    }
    assert!(refusals.is_empty(), "{}", refusals.join("\n\n"));

    // The text to record: the generated one, with each contract header
    // carrying the unpublished mark as `written_header` decides. The
    // announcement section has no mark.
    let mut expected = String::new();
    for s in &current {
        let header = match recorded.iter().find(|r| same_section(&r.header, &s.header)) {
            Some(r) if is_contract(&s.header) => written_header(&s.header, r),
            _ => s.header.clone(),
        };
        expected.push_str(&header);
        expected.push('\n');
        for line in &s.lines {
            expected.push_str(line);
            expected.push('\n');
        }
    }

    if update {
        std::fs::write(&path, &expected).expect("the fixture is writable");
        return;
    }
    assert!(
        expected == recorded_text,
        "the wire fingerprint is out of date (a version moved, or the announcement changed).\n\
         The announcement is the bootstrap: an addition is compatible (regenerate and say why in the commit); \
         a break moves PROTOCOL_VERSION, which republishes everything.\n\
         Regenerate with: {REGENERATE}\n\
         First difference:\n{}",
        first_difference_text(&recorded_text, &expected)
    );
}
