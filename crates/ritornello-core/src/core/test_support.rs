//! Helpers shared by the tests of the core modules: fake player and sources, rigs. pub(super): visible from core and its children, from nobody else.

use super::*;
use std::sync::Mutex;

/// A `Registry` swept from `root`, shared like production wiring expects.
/// Every rig below that used to build only a `Catalog` for `Wiring.catalog`
/// now needs this too, for `Wiring.registry` — kept in one place so a
/// change to how a test registry is built happens once.
pub(super) fn test_registry(root: &std::path::Path) -> crate::i18n::Shared {
    Arc::new(RwLock::new(crate::i18n::Registry::sweep(root.to_path_buf())))
}

#[derive(Default)]
pub(super) struct FakePlayer {
    pub(super) calls: Arc<Mutex<Vec<String>>>,
    /// What the fake player claims to know about its progress.
    /// `Mutex` rather than a plain field: tests set it after
    /// construction, since `Player` only takes `&self`.
    pub(super) progress: Arc<Mutex<crate::player::Progress>>,
    /// When true, `toggle_pause` fails — mpv absent, socket cut.
    /// Shared and set after construction, for the same reason as
    /// `progress`.
    pub(super) pause_fails: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl crate::player::Player for FakePlayer {
    async fn play(&self, uri: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("play {uri}"));
        Ok(())
    }
    async fn load_list(&self, uri: &str, start: Option<i64>) -> anyhow::Result<()> {
        // The index is recorded **in the same call**, which is the whole
        // point: a test can no longer see a load and a positioning as two
        // separate events, because the player no longer offers that.
        let start = match start {
            Some(n) => n.to_string(),
            None => "auto".to_string(),
        };
        self.calls.lock().unwrap().push(format!("load_list {uri} start={start}"));
        Ok(())
    }
    async fn stop(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("stop".into());
        Ok(())
    }
    async fn toggle_pause(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("pause".into());
        if self.pause_fails.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("mpv unreachable");
        }
        Ok(())
    }
    async fn next(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("next".into());
        Ok(())
    }
    async fn prev(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("prev".into());
        Ok(())
    }
    async fn set_volume(&self, v: u8) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("vol {v}"));
        Ok(())
    }
    async fn set_mute(&self, m: bool) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("mute {m}"));
        Ok(())
    }
    async fn set_audio_device(&self, device: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("audio_device {device}"));
        Ok(())
    }
    async fn progress(&self) -> anyhow::Result<crate::player::Progress> {
        Ok(*self.progress.lock().unwrap())
    }
    async fn seek_relative(&self, delta_s: i64) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("seek_relative {delta_s}"));
        Ok(())
    }
    async fn seek_absolute(&self, position_s: u32) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("seek_absolute {position_s}"));
        Ok(())
    }
    async fn set_chapter(&self, n: i64) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("chapter {n}"));
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct FakeSource {
    pub(super) name: &'static str,
    pub(super) calls: Arc<Mutex<Vec<String>>>,
    /// Every `SourceReq::ArchiveCover` this source received: the identity the
    /// core echoed back, and the path of the staged original.
    ///
    /// Recorded apart from `calls` rather than folded into its `{:?}` line
    /// because the archive tests assert on the two halves — an identity's
    /// `path` field, and the **bytes** at that file — which a formatted string
    /// could only be matched against by substring.
    pub(super) archives: Arc<Mutex<Vec<(serde_json::Value, String)>>>,
    /// This Source takes the file but its reply never comes back in time.
    ///
    /// Models the one failure mode that matters for the hand-over:
    /// `SourceClient::request` gives up after five seconds while the SDK is
    /// still awaiting `archive_cover` inline, so the core sees an error for a
    /// plugin that is at that very moment copying the file onto a share.
    pub(super) refuses_archive: bool,
}

#[async_trait::async_trait]
impl Source for FakeSource {
    async fn request(&self, req: SourceReq) -> Result<SourceAction> {
        self.calls.lock().unwrap().push(format!("{}:{:?}", self.name, req));
        // A reserved name to simulate a plugin that no longer answers:
        // `remove_source` must stay correct even when the switch to the
        // incoming source fails, and this is the only way to test it
        // without rigging `FakePlayer`.
        if self.name == "broken" {
            anyhow::bail!("broken plugin does not answer");
        }
        Ok(match (self.name, req) {
            ("radio", SourceReq::Activate) => SourceAction::play("http://fip"),
            ("radio", SourceReq::Select(3)) => SourceAction::play("http://inter"),
            ("radio", SourceReq::Select(_)) => SourceAction::Noop,
            // `.finite()` like the real cd plugin: without this
            // declaration, the end of the disc would pass for a stream
            // cut and the restart would replay the disc in a loop.
            ("cd", SourceReq::Activate) => SourceAction::play("cdda://").finite(),
            (_, SourceReq::Eject) if self.name == "cd" => SourceAction::Stop,
            ("radio", SourceReq::Wake) => SourceAction::play("http://fip"),
            ("cd", SourceReq::Wake) => SourceAction::Noop,
            // The Play key. This fake answers it like an arrival, which is
            // the SDK's default for any source that does not override
            // `play()` — it is not a model of the real cd plugin, whose
            // arrival is configurable and may play nothing while Play still
            // starts the disc. What matters here is that the core sends a
            // distinct request at all: without these two arms the fake
            // would fall through to `Noop` and the Play key would look
            // inert in the tests too.
            ("radio", SourceReq::Play) => SourceAction::play("http://fip"),
            ("cd", SourceReq::Play) => SourceAction::play("cdda://").finite(),
            // Models the cd plugin's own `pending_chapter`: mpv's first
            // track notification after a resume can carry an action of its
            // own (a seek owed since the disc was not open yet), and it is
            // this arm the regression for that path (C1) drives.
            ("cd", SourceReq::PlayerTrack(0)) => SourceAction::PlayerChapter(4),
            // Models a source's own "next pass" answer to an ending: without
            // this arm this fake falls through to `Noop`, and a test could
            // not tell an applied answer from one silently dropped.
            ("radio", SourceReq::EndOfContent) => SourceAction::play("/tmp/list.m3u").playlist().finite(),
            // The hand-over of an original. Recorded rather than merely
            // tolerated: `Noop` is the honest answer of a source that keeps
            // the file, and without this arm the request would fall through
            // to the `_` below and leave nothing a test could read.
            (_, SourceReq::ArchiveCover { identity, file }) => {
                self.archives.lock().unwrap().push((identity, file));
                if self.refuses_archive {
                    anyhow::bail!("no reply within the correlation deadline");
                }
                SourceAction::Noop
            }
            _ => SourceAction::Noop,
        })
    }
}

/// Alias for the test rig (clippy::type_complexity): fake core,
/// call logs of the player and of the sources, state receiver, temporary directory.
pub(super) type Rig = (Core<FakePlayer>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>, watch::Receiver<PlayerState>, tempfile::TempDir);

/// Metadata wiring without an observer: the receivers are dropped
/// right away, the core's `send`s fail silently (already the case in
/// production when no `metadata` plugin is declared). Tests that observe
/// these channels use `setup_metadata`.
/// The declared order a test rig stands in for.
///
/// Alphabetical, and deliberately so: that is the order these rigs had before
/// the source cycle started following `plugins.toml`, and the subject of every
/// one of them is something else. The tests that *are* about the order name it
/// explicitly instead of coming through here.
pub(super) fn declared_order(sources: &HashMap<String, Arc<dyn Source>>) -> Vec<String> {
    let mut names: Vec<String> = sources.keys().cloned().collect();
    names.sort();
    names
}

pub(super) fn silent_wiring(plugins: Vec<String>) -> MetadataWiring {
    MetadataWiring {
        plugins,
        now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
        state: watch::channel(PlayerState::default()).0,
    }
}

/// Minimal cover wiring for the rigs that have no use for it: a fresh
/// cache, and a sender whose reception nobody reads (the receiver is
/// dropped right away — a later send then fails silently, which
/// `start_cover_fetch` already ignores).
pub(super) fn test_covers() -> (Arc<crate::cover::CoverCache>, mpsc::Sender<(String, bool)>) {
    (Arc::new(crate::cover::CoverCache::new()), mpsc::channel(4).0)
}

/// Update carrying nothing: every field at `None`/`false`. Convenient
/// base to compose a minimal frame in a test (see the status tests).
pub(super) fn bare_update() -> SourceUpdate {
    SourceUpdate::default()
}

/// Update carrying only an identity.
pub(super) fn plays(identity: serde_json::Value) -> SourceUpdate {
    SourceUpdate {
        identity: Some(IdentityUpdate::Playing(identity)),
        ..Default::default()
    }
}

/// A named preset, short form for the tests.
pub(super) fn preset_of(index: u8, name: &str) -> Preset {
    Preset { index, name: name.into() }
}

/// Frame carrying **only** named presets: this is exactly the form in
/// which the answer to `ListPresets` reaches the core, the correlated
/// action (`Noop`) leaving by the other path.
pub(super) fn with_presets(presets: Vec<Preset>) -> SourceUpdate {
    let mut u = bare_update();
    u.presets = Some(presets);
    u
}

/// The names of a sources catalog, in the order it carries them.
pub(super) fn names(cat: &SourcesCatalog) -> Vec<String> {
    cat.sources.iter().map(|s| s.name.clone()).collect()
}

pub(super) fn setup() -> Rig {
    setup_persisted(PersistedState::default())
}

/// `setup` with a say on what `state.json` held at launch — what
/// `StartupPower::Previous` reads.
pub(super) fn setup_persisted(persisted: PersistedState) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let player = FakePlayer::default();
    let player_calls = player.calls.clone();
    let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone(), ..Default::default() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone(), ..Default::default() }));
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
    let (covers, cover_tx) = test_covers();
    let manifest_order = declared_order(&sources);
    let core = Core::new(
        player,
        Wiring {
            sources,
            persisted,
            state_path: dir.path().join("state.json"),
            catalog,
            registry: test_registry(&root),
            manifest_order,
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
            metadata: MetadataWiring {
                plugins: vec![],
                now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
                state: state_tx,
            },
        },
        covers,
        cover_tx,
        mpsc::channel(4).0,
    );
    (core, player_calls, source_calls, state_rx, dir)
}

/// Rig observing both metadata channels: what goes down to the
/// plugins, and the structured state that goes up to the SPA and the displays.
///
/// `plugins` carries the declaration order, hence the arbitration priority.
#[allow(clippy::type_complexity)]
pub(super) fn setup_metadata(
    plugins: Vec<String>,
) -> (Core<FakePlayer>, watch::Receiver<NowPlaying>, watch::Receiver<PlayerState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone(), ..Default::default() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls, ..Default::default() }));
    let (np_tx, np_rx) = watch::channel(NowPlaying { source: "radio".into(), identity: None, ..Default::default() });
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
    let (covers, cover_tx) = test_covers();
    let manifest_order = declared_order(&sources);
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources,
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            registry: test_registry(&root),
            manifest_order,
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
            metadata: MetadataWiring { plugins, now_playing: np_tx, state: state_tx },
        },
        covers,
        cover_tx,
        mpsc::channel(4).0,
    );
    (core, np_rx, state_rx, dir)
}

/// Alias of `setup_metadata(vec![])`: the partial-state tests need no
/// `metadata` plugin, only the rig that `setup_metadata` already knows
/// how to build.
pub(super) fn test_core() -> (Core<FakePlayer>, watch::Receiver<NowPlaying>, watch::Receiver<PlayerState>, tempfile::TempDir) {
    setup_metadata(vec![])
}

/// Like `test_core`, but **keeps** the receiver of the embedded-cover
/// extraction channel instead of dropping it.
///
/// Needed by any test that really lets the detached task of `handle_path`
/// run on a real file: the real result must be drained from the real
/// channel, not reconstructed by a second, independent call to
/// `mpv::embedded_cover` on the test's own side. Before this rework, that
/// second call was worse than redundant — it raced the detached task's
/// write to the same temp file, a real race between two writers discovered
/// in use (see the report of task 6, ruling 1 of the review). The write is
/// gone, but the reason to drain the real channel is not: it is still the
/// only way to assert on the exact `CoverSource` production code produced,
/// rather than one a duplicated computation happens to agree with today.
#[allow(clippy::type_complexity)]
pub(super) fn test_core_with_extraction() -> (
    Core<FakePlayer>,
    watch::Receiver<PlayerState>,
    mpsc::Receiver<(String, Option<crate::cover::CoverSource>)>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone(), ..Default::default() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls, ..Default::default() }));
    let (np_tx, _np_rx) =
        watch::channel(NowPlaying { source: "radio".into(), identity: None, ..Default::default() });
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
    let (covers, cover_tx) = test_covers();
    let (extraction_tx, extraction_rx) = mpsc::channel(4);
    let manifest_order = declared_order(&sources);
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources,
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            registry: test_registry(&root),
            manifest_order,
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
            metadata: MetadataWiring { plugins: vec![], now_playing: np_tx, state: state_tx },
        },
        covers,
        cover_tx,
        extraction_tx,
    );
    (core, state_rx, extraction_rx, dir)
}

impl Core<FakePlayer> {
    /// Sets what the fake player claims to know about its progress.
    pub(super) fn set_progress(&self, position_s: Option<f64>, duration_s: Option<f64>) {
        *self.player.progress.lock().unwrap() =
            crate::player::Progress { position_s, duration_s };
    }

    /// Moves the anchor back by `duration`: the test advances time without sleeping.
    pub(super) fn advance_anchor_for_test(&mut self, duration: std::time::Duration) {
        if let Some((p, set_at)) = self.position_anchor {
            self.position_anchor = Some((p, set_at - duration));
        }
    }
}

/// Core without any source: the startup where *none* answered. This is
/// exactly the situation hotplug wiring must be able to get out of, and
/// the one the core must now know how to serve — the status page is
/// there to show the frozen plugins.
///
/// The state receiver is returned (not dropped as in `silent_wiring`):
/// "no source" is a state to observe, not merely to survive.
pub(super) fn setup_without_source() -> (Core<FakePlayer>, watch::Receiver<PlayerState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
        "core",
        "en",
        &root,
        crate::i18n::EN,
    )));
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let (covers, cover_tx) = test_covers();
    // Nothing declared, because nothing is wired: this rig is the startup
    // where no source answered.
    let manifest_order = vec![];
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources: HashMap::new(),
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            registry: test_registry(&root),
            manifest_order,
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
            metadata: MetadataWiring {
                plugins: vec![],
                now_playing: watch::channel(NowPlaying {
                    source: String::new(),
                    identity: None,
                    ..Default::default()
                })
                .0,
                state: state_tx,
            },
        },
        covers,
        cover_tx,
        mpsc::channel(4).0,
    );
    (core, state_rx, dir)
}

/// Extracts the delay of a `RetryIn`, or fails naming what happened instead.
pub(super) fn restart(outcome: EventOutcome) -> Duration {
    match outcome {
        EventOutcome::RetryIn(d) => d,
        other => panic!("expected RetryIn, got {other:?}"),
    }
}

pub(super) fn enrichment(identity: serde_json::Value, artist: &str, title: &str) -> Enrichment {
    Enrichment {
        identity,
        artist: Some(artist.into()),
        title: Some(title.into()),
        ..Default::default()
    }
}

/// Update carrying only a preset count declared by the Source.
pub(super) fn update_with_count(count: Option<u8>) -> SourceUpdate {
    SourceUpdate {
        preset_count: count,
        ..Default::default()
    }
}

/// Update carrying only a preset name declared by the Source.
pub(super) fn update_with_name(name: Option<&str>) -> SourceUpdate {
    SourceUpdate {
        preset_name: name.map(str::to_string),
        ..Default::default()
    }
}

/// Update carrying the two capabilities the sdk stamps on **every** frame it
/// writes (`can_eject`, `has_finite_list`) — the shape a real plugin's frame
/// actually has. Lets a test exercise both capabilities' identical lifecycle
/// (remembered, published, forgotten on source change/standby/death) in
/// lockstep, without asserting they always carry the *same* value — they are
/// independent capabilities that merely share a wire idiom.
pub(super) fn update_with_capabilities(can_eject: Option<bool>, has_finite_list: Option<bool>) -> SourceUpdate {
    SourceUpdate {
        can_eject,
        has_finite_list,
        ..Default::default()
    }
}

/// Declares a finite list on `source` — the capability the two play modes
/// need to be honoured at all (see `handle_command`'s guard arm).
///
/// `setup()` starts with the capability unknown, hence false by the
/// convention of `SourceMessage::has_finite_list`, so **a test that arms
/// `random` or `repeat_all` for a reason of its own must say this first** or
/// the command is refused and the test proves nothing about what it meant to
/// prove. `can_eject` deliberately left alone: the two capabilities share a
/// wire idiom, not a value.
pub(super) fn declare_finite_list<P: crate::player::Player>(core: &mut Core<P>, source: &str) {
    core.handle_source_update(source, update_with_capabilities(None, Some(true)));
}

/// Frame in the shape `serve_source` really produces: `can_eject` and
/// `has_finite_list` stamped, because the SDK stamps both on **every** frame
/// it writes (see the doc of `SourceMessage::can_eject`).
///
/// To be preferred over `bare_update()` in any test that claims to
/// describe a frame coming from a real plugin: `SourceUpdate::default()`
/// leaves both at `None`, a shape the SDK cannot emit, and a test built on
/// it may attest a failure mode that does not exist.
pub(super) fn sdk_frame() -> SourceUpdate {
    SourceUpdate { can_eject: Some(false), has_finite_list: Some(false), ..SourceUpdate::default() }
}

/// Short timings so pacing tests run in tens of milliseconds. The core does
/// not validate bounds (that's the HTTP layer's job), so this is legal.
pub(super) fn quick_settings() -> crate::state::Settings {
    crate::state::Settings {
        volume_repeat_initial_ms: 30,
        volume_repeat_interval_ms: 25,
        ..Default::default()
    }
}

/// Builds a real mp3 with an embedded cover, via ffmpeg — same principle
/// as the mp3-with-cover fixture of `player::mpv::tests`, duplicated here
/// for lack of a simple way to share a test utility between modules.
/// Returns `None` if ffmpeg is absent: the test skips itself rather than
/// failing, it is not a dependency of the core.
///
/// **The image used to have to stay different from the one in
/// `player::mpv::tests`, and no longer does — kept distinct anyway, for
/// clarity.** `player::mpv::embedded_cover` used to name a temp file after
/// the *content* of the image and write it to the `temp_dir()` **shared** by
/// every test of this binary — which run in parallel; two fixtures carrying
/// the same image would then collide there, and the tests here additionally
/// went through `CoverCache`, whose eviction **deleted** those files, which
/// is exactly what produced an intermittent failure in the neighbour reading
/// a file erased or rewritten under it. `embedded_cover` now only probes the
/// container and writes nothing, so that collision cannot occur anymore;
/// green and 32×32 remains distinct from the red 16×16 of
/// `player::mpv::tests` simply so a mismatch between the two is easy to spot.
pub(super) fn test_mp3_with_cover(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let image = dir.join("cover.jpg");
    let output = dir.join("with_cover.mp3");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "color=c=green:s=32x32:d=1"])
        .args(["-frames:v", "1"])
        .arg(&image)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=1")
            .arg("-i")
            .arg(&image)
            .args(["-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-c:v", "copy"])
            .args(["-id3v2_version", "3"])
            .args(["-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"])
            .arg(&output)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    ok.then_some(output)
}

/// Rewrites the picture embedded in `output` **in place**, same path, via
/// ffmpeg — the retag scenario the stale-embedded-path bug lives in: a
/// track's audio file changes what it carries without moving. Same pipeline
/// as `test_mp3_with_cover`, parameterized on `color` so the new picture is
/// provably different from whatever was there before. `false` if ffmpeg is
/// absent, exactly like `test_mp3_with_cover`.
pub(super) fn retag_embedded_cover(output: &std::path::Path, color: &str) -> bool {
    let image = output.with_extension(format!("{color}.jpg"));
    std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c={color}:s=32x32:d=1"))
        .args(["-frames:v", "1"])
        .arg(&image)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=1")
            .arg("-i")
            .arg(&image)
            .args(["-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-c:v", "copy"])
            .args(["-id3v2_version", "3"])
            .args(["-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"])
            .arg(output)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// The raw bytes of the picture embedded in `path`, read directly with
/// `lofty` — what a test compares served bytes against, independently of
/// anything `cover.rs` does with them.
pub(super) fn embedded_picture_bytes(path: &std::path::Path) -> Vec<u8> {
    let file = lofty::probe::Probe::open(path).expect("probe the test fixture").read().expect("read tags");
    lofty::file::TaggedFileExt::primary_tag(&file)
        .or_else(|| lofty::file::TaggedFileExt::first_tag(&file))
        .expect("the fixture carries a tag")
        .pictures()
        .first()
        .expect("the fixture carries a picture")
        .data()
        .to_vec()
}

/// Like `test_core_with_extraction`, but keeps the **cover** channel instead
/// of the extraction one.
///
/// Needed by any test that must observe whether `start_cover_fetch`'s real
/// detached task actually re-fetched and re-inserted a cache entry, rather
/// than short-circuiting on `contains`. Hand-replaying the end of the
/// detached task — as most other tests in this module do, calling
/// `cover::fetch`/`insert` directly — would bypass the very guard the
/// stale-embedded-path bug lives in, and prove nothing about it.
#[allow(clippy::type_complexity)]
pub(super) fn test_core_with_cover_channel() -> (
    Core<FakePlayer>,
    watch::Receiver<PlayerState>,
    mpsc::Receiver<(String, bool)>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone(), ..Default::default() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls, ..Default::default() }));
    let (np_tx, _np_rx) =
        watch::channel(NowPlaying { source: "radio".into(), identity: None, ..Default::default() });
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
        "core",
        "en",
        &root,
        crate::i18n::EN,
    )));
    let covers = Arc::new(crate::cover::CoverCache::new());
    let (cover_tx, cover_rx) = mpsc::channel(4);
    let manifest_order = declared_order(&sources);
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources,
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            registry: test_registry(&root),
            manifest_order,
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
            metadata: MetadataWiring { plugins: vec![], now_playing: np_tx, state: state_tx },
        },
        covers,
        cover_tx,
        mpsc::channel(4).0,
    );
    (core, state_rx, cover_rx, dir)
}

/// French pack shipped in the repository (invariant: same keys as the embedded English).
pub(super) fn fr_pack() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/core/fr.toml");
    std::fs::read_to_string(p).expect("shipped fr pack")
}

/// Name of the rig's only Source, and it is `files` on purpose: the offer to
/// keep an original is that plugin's, and this whole path exists for it.
pub(super) const ARCHIVING_SOURCE: &str = "files";

/// Name of the rig's only `metadata` contributor — the one that finds covers
/// on the internet, which is the only kind worth bringing back to the share.
pub(super) const ARCHIVING_CONTRIBUTOR: &str = "musicbrainz";

/// The **full-size original** the network answers with, and the very bytes a
/// test then expects to find in the staged file.
///
/// One value named once: armed into the cache seam by `archiving_core` and
/// read back by the assertion, so the test cannot pass by comparing two
/// different things that happen to agree.
pub(super) fn original() -> Vec<u8> {
    crate::cover::fixtures::jpeg_decodable(1500, 1500)
}

/// The thumbnail a finished fetch would have deposited in the cache. Small
/// and real: `cover_arrived` refuses to publish a key the cache does not
/// hold, so something has to be there, and a decodable image keeps the entry
/// honest.
fn deposited_thumbnail() -> Vec<u8> {
    crate::cover::fixtures::jpeg_decodable(300, 300)
}

/// The spontaneous notification by which a Source declares it would keep a
/// network cover, in the shape the SDK really writes (see `sdk_frame`: both
/// capabilities are stamped on **every** frame).
pub(super) fn offers_archive() -> SourceUpdate {
    SourceUpdate { cover_archivable: Some(true), ..sdk_frame() }
}

/// Rig of the cover-archiving tests: one `files`-shaped Source playing off a
/// share, one contributor finding covers on the internet, and a record of
/// every hand-over the Source received.
///
/// **A wrapper rather than a bare `Core`, for two reasons.** The `TempDir`
/// has to outlive the core — dropped, it takes `state.json`'s directory with
/// it — and the tests read what the fake Source recorded, which no field of
/// `Core` carries. `Deref`/`DerefMut` keep every core method (`app_covers`,
/// `handle_source_update`, …) reachable as though the rig were the core.
pub(super) struct ArchivingRig {
    core: Core<FakePlayer>,
    /// Every hand-over received, in order.
    archives: Arc<Mutex<Vec<(serde_json::Value, String)>>>,
    /// Whether this Source makes the offer at all. Read by `play_file`, which
    /// re-declares it after every identity — as `plugin-files` does, and as
    /// the core requires: the offer is forgotten on identity change, because
    /// it describes a folder and not a session.
    offers: bool,
    _dir: tempfile::TempDir,
}

impl std::ops::Deref for ArchivingRig {
    type Target = Core<FakePlayer>;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl std::ops::DerefMut for ArchivingRig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl Drop for ArchivingRig {
    /// A Source owns a staged original the moment it is handed over, deletion
    /// included (see `SourceReq::ArchiveCover`). This fake keeps it instead,
    /// so that a test can read the bytes back — so the rig deletes it here,
    /// rather than leaving a megabyte per run in the `temp_dir()` every test
    /// of this binary shares.
    fn drop(&mut self) {
        for (_, file) in self.archives.lock().unwrap().iter() {
            let _ = std::fs::remove_file(file);
        }
    }
}

impl ArchivingRig {
    /// The frames a `files`-shaped Source sends when it opens `path`: the
    /// identity of what is playing, then — if this Source makes the offer —
    /// the spontaneous notification carrying it.
    ///
    /// **That order is production's, and it is also the order the core
    /// requires.** `plugin-files` declares the offer on its folder-probe
    /// notification, never on the reply that announces the track; and
    /// `set_identity` forgets the offer, so a rig that declared it once at
    /// construction would watch the first track erase it.
    pub(super) async fn play_file(&mut self, path: &str) {
        self.core.handle_source_update(
            ARCHIVING_SOURCE,
            plays(serde_json::json!({"kind": "file", "path": path})),
        );
        if self.offers {
            self.core.handle_source_update(ARCHIVING_SOURCE, offers_archive());
        }
    }

    /// Plays `path` without ever sending the offer notification — the shape
    /// of a folder-probe that found a cover already in place, and so never
    /// declares `cover_archivable(true)`. `poll_notification` never sends
    /// `(false)`: `set_identity`, triggered by the identity frame this sends,
    /// is the *only* thing that can withdraw an offer a previous folder made.
    pub(super) async fn play_file_in_a_folder_with_its_own_cover(&mut self, path: &str) {
        self.core.handle_source_update(
            ARCHIVING_SOURCE,
            plays(serde_json::json!({"kind": "file", "path": path})),
        );
    }

    /// A contributor announces a cover it found **on the internet**, and the
    /// fetch of its thumbnail completes.
    pub(super) async fn declare_network_cover(&mut self, url: &str) {
        let identity = self.core.metadata.identity().cloned().expect("something must be playing");
        let cover = ritornello_proto::CoverRef::Url { url: url.to_string() };
        self.core.handle_enrichment(
            ARCHIVING_CONTRIBUTOR,
            Enrichment { identity, cover: Some(cover.clone()), ..Default::default() },
        );
        self.finish_the_fetch(crate::cover::CoverSource::Ref(cover)).await;
    }

    /// The Source declares the `folder.jpg` sitting beside the track — the
    /// image that is already on the share, and the tier that outranks every
    /// contributor.
    pub(super) async fn declare_local_cover(&mut self, path: &str) {
        let cover = ritornello_proto::CoverRef::Path { path: path.to_string() };
        self.core.set_source_cover(Some(cover.clone()), None, ARCHIVING_SOURCE);
        self.finish_the_fetch(crate::cover::CoverSource::Ref(cover)).await;
    }

    /// Replays the end of a fetch exactly as `main`'s loop does: the
    /// thumbnail the detached task would have deposited, then `cover_arrived`
    /// — which is where archiving is decided.
    ///
    /// **A `Pair`, not bare `Bytes`.** `fetch` never deposits `Bytes` for a
    /// `Ref` — only a `Pair` (see `cover::fetch`'s doc) — and `remember_full`,
    /// the memo `full_size` writes back after a download, only ever finds a
    /// `Pair` to write onto (`entries.iter_mut().find(...)` matches nothing
    /// else). A rig depositing `Bytes` would make every enlargement download
    /// again forever, silently: `full_size` still returns bytes either way,
    /// so nothing here would fail, only a test asserting on
    /// `full_downloads()` around the archive path — the one proof this
    /// feature's design calls "iso to an enlargement" — would fail, or worse,
    /// never get written because the rig could not carry it.
    ///
    /// Then **awaits** whatever archive task that decision detached. A
    /// detached task nobody awaits is a race, not a background job: without
    /// this the assertions below would read a record the task had not written
    /// yet, and would pass or fail on the scheduler's mood.
    async fn finish_the_fetch(&mut self, s: crate::cover::CoverSource) {
        let key = crate::cover::key(&s);
        let crate::cover::CoverSource::Ref(full) = s else {
            unreachable!("this rig only ever declares covers by reference");
        };
        self.core
            .covers
            .insert(
                key.clone(),
                crate::cover::CoverPayload::Pair {
                    thumb: deposited_thumbnail(),
                    thumb_mime: "image/jpeg",
                    full,
                    fetched: None,
                },
            )
            .await;
        self.core.cover_arrived(key, true).await;
        self.core.settle_cover_archive().await;
    }

    /// Every hand-over the Source received: the echoed identity, and the path
    /// of the staged original.
    pub(super) fn archive_requests(&self) -> Vec<(serde_json::Value, String)> {
        self.archives.lock().unwrap().clone()
    }
}

/// A core whose Source offers to keep originals, and whose network answers
/// with `original()`.
pub(super) async fn archiving_core() -> ArchivingRig {
    archiving_rig(true, true, false)
}

/// The same, with a Source that never makes the offer — radio, in effect,
/// which wins a network cover on every track and has nowhere to put it.
pub(super) async fn core_without_offer() -> ArchivingRig {
    archiving_rig(false, true, false)
}

/// The same as `archiving_core`, with the cache seam left unarmed: every
/// attempt at the original yields nothing, exactly as a 404 or a cut Wi-Fi
/// would.
pub(super) async fn archiving_core_without_network() -> ArchivingRig {
    archiving_rig(true, false, false)
}

/// The same as `archiving_core`, with a Source whose reply never arrives — the
/// slow copy onto an SMB share that outlives the five-second correlation.
pub(super) async fn archiving_core_whose_source_never_replies() -> ArchivingRig {
    archiving_rig(true, true, true)
}

fn archiving_rig(offers: bool, network: bool, refuses_archive: bool) -> ArchivingRig {
    let dir = tempfile::tempdir().unwrap();
    let archives: Arc<Mutex<Vec<(serde_json::Value, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
    sources.insert(
        ARCHIVING_SOURCE.into(),
        Arc::new(FakeSource {
            name: ARCHIVING_SOURCE,
            archives: archives.clone(),
            refuses_archive,
            ..Default::default()
        }),
    );
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load(
        "core",
        "en",
        &root,
        crate::i18n::EN,
    )));
    let (covers, cover_tx) = test_covers();
    if network {
        covers.answer_full_downloads_with(original(), "image/jpeg");
    }
    let manifest_order = declared_order(&sources);
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources,
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            registry: test_registry(&root),
            manifest_order,
            sources_catalog: watch::channel(SourcesCatalog::default()).0,
            metadata: MetadataWiring {
                plugins: vec![ARCHIVING_CONTRIBUTOR.to_string()],
                now_playing: watch::channel(NowPlaying::default()).0,
                state: watch::channel(PlayerState::default()).0,
            },
        },
        covers,
        cover_tx,
        mpsc::channel(4).0,
    );
    ArchivingRig { core, archives, offers, _dir: dir }
}
