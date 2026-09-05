//! Helpers shared by the tests of the core modules: fake player and sources, rigs. pub(super): visible from core and its children, from nobody else.

use super::*;
use std::sync::Mutex;

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
}

pub(super) struct FakeSource {
    pub(super) name: &'static str,
    pub(super) calls: Arc<Mutex<Vec<String>>>,
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
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
    let (covers, cover_tx) = test_covers();
    let core = Core::new(
        player,
        Wiring {
            sources,
            persisted,
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
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
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls }));
    let (np_tx, np_rx) = watch::channel(NowPlaying { source: "radio".into(), identity: None, ..Default::default() });
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
    let (covers, cover_tx) = test_covers();
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources,
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
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
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls }));
    let (np_tx, _np_rx) =
        watch::channel(NowPlaying { source: "radio".into(), identity: None, ..Default::default() });
    let (state_tx, state_rx) = watch::channel(PlayerState::default());
    let root = dir.path().to_path_buf();
    let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
    let (covers, cover_tx) = test_covers();
    let (extraction_tx, extraction_rx) = mpsc::channel(4);
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources,
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
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
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources: HashMap::new(),
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
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

/// Update carrying only the eject capability declared by the Source.
pub(super) fn update_with_eject(can: Option<bool>) -> SourceUpdate {
    SourceUpdate {
        can_eject: can,
        ..Default::default()
    }
}

/// Frame in the shape `serve_source` really produces: `can_eject`
/// stamped, because the SDK stamps it on **every** frame it writes (see
/// the doc of `SourceMessage::can_eject`).
///
/// To be preferred over `bare_update()` in any test that claims to
/// describe a frame coming from a real plugin: `SourceUpdate::default()`
/// leaves `can_eject` at `None`, a shape the SDK cannot emit, and a test
/// built on it may attest a failure mode that does not exist.
pub(super) fn sdk_frame() -> SourceUpdate {
    SourceUpdate { can_eject: Some(false), ..SourceUpdate::default() }
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
    sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
    sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls }));
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
    let core = Core::new(
        FakePlayer::default(),
        Wiring {
            sources,
            persisted: PersistedState::default(),
            state_path: dir.path().join("state.json"),
            catalog,
            locales_root: root,
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
