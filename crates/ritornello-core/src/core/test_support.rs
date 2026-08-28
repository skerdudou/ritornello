//! Aides partagees par les tests des modules de core : player et sources factices, montages. pub(super) : visibles de core et de ses enfants, de personne d'autre.

use super::*;
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct FakePlayer {
    pub(super) calls: Arc<Mutex<Vec<String>>>,
    /// Ce que le player factice prétend savoir de sa progress.
    /// `Mutex` et non champ simple : les tests le règlent après
    /// construction, `Player` ne prenant que `&self`.
    pub(super) progress: Arc<Mutex<crate::player::Progress>>,
    /// Quand c'est vrai, `toggle_pause` échoue — mpv absent, socket coupé.
    /// Partagé et posé après construction, pour la même raison que
    /// `progress`.
    pub(super) pause_echoue: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl crate::player::Player for FakePlayer {
    async fn play(&self, uri: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("play {uri}"));
        Ok(())
    }
    async fn load_list(&self, uri: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("load_list {uri}"));
        Ok(())
    }
    async fn stop(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("stop".into());
        Ok(())
    }
    async fn toggle_pause(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("pause".into());
        if self.pause_echoue.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("mpv injoignable");
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
    async fn set_playlist_pos(&self, n: i64) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("playlist-pos {n}"));
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
        // Un name réservé pour simuler un greffon qui ne répond plus :
        // `remove_source` doit rester correct même quand la bascule vers
        // l'entrante échoue, et c'est le seul moyen de le tester sans
        // truquer `FakePlayer`.
        if self.name == "casse" {
            anyhow::bail!("plugin casse ne répond pas");
        }
        Ok(match (self.name, req) {
            ("radio", SourceReq::Activate) => SourceAction::play("http://fip"),
            ("radio", SourceReq::Select(3)) => SourceAction::play("http://inter"),
            ("radio", SourceReq::Select(_)) => SourceAction::Noop,
            // `.finite()` comme le vrai plugin cd : sans cette
            // déclaration, la fin du disque passerait pour une coupure de
            // stream et la restart rejouerait le disque en boucle.
            ("cd", SourceReq::Activate) => SourceAction::play("cdda://").finite(),
            (_, SourceReq::Eject) if self.name == "cd" => SourceAction::Stop,
            ("radio", SourceReq::Wake) => SourceAction::play("http://fip"),
            ("cd", SourceReq::Wake) => SourceAction::Noop,
            _ => SourceAction::Noop,
        })
    }
}

/// Alias pour le montage de test (clippy::type_complexity) : cœur factice,
/// logs d'appels du player et des sources, récepteur d'état, répertoire temporaire.
pub(super) type Rig = (Core<FakePlayer>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>, watch::Receiver<PlayerState>, tempfile::TempDir);

/// Câblage métadonnées sans observateur : les récepteurs sont lâchés
/// aussitôt, les `send` du cœur échouent silencieusement (c'est déjà le cas
/// en production quand aucun plugin `metadata` n'est déclaré). Les tests qui
/// observent ces canaux utilisent `setup_metadata`.
pub(super) fn silent_wiring(plugins: Vec<String>) -> MetadataWiring {
    MetadataWiring {
        plugins,
        now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
        state: watch::channel(PlayerState::default()).0,
    }
}

/// Câblage minimal des pochettes pour les montages qui n'en ont pas
/// l'usage : un cache neuf, et un émetteur dont personne ne read la
/// réception (le récepteur est lâché aussitôt — un envoi ultérieur
/// échoue alors en silence, ce que `start_cover_fetch` ignore déjà).
pub(super) fn test_covers() -> (Arc<crate::cover::CoverCache>, mpsc::Sender<(String, bool)>) {
    (Arc::new(crate::cover::CoverCache::new()), mpsc::channel(4).0)
}

/// Mise à jour ne portant rien : tous les champs à `None`/`false`. Base
/// commode pour composer une trame minimale dans un test (voir les tests
/// de statut).
pub(super) fn bare_update() -> SourceUpdate {
    SourceUpdate::default()
}

/// Mise à jour ne portant qu'une identité.
pub(super) fn plays(identity: serde_json::Value) -> SourceUpdate {
    SourceUpdate {
        identity: Some(IdentityUpdate::Playing(identity)),
        transient: false,
        preset: None,
        preset_count: None,
        preset_name: None,
        status: None,
        can_eject: None,
        presets: None,
        cover: None,
    }
}

/// Une présélection nommée, forme courte pour les tests.
pub(super) fn preset_of(index: u8, name: &str) -> Preset {
    Preset { index, name: name.into() }
}

/// Trame ne portant **que** des présélections nommées : c'est exactement la
/// forme sous laquelle la réponse à `ListPresets` atteint le cœur, l'action
/// corrélée (`Noop`) partant par l'autre voie.
pub(super) fn with_presets(presets: Vec<Preset>) -> SourceUpdate {
    let mut u = bare_update();
    u.presets = Some(presets);
    u
}

/// Les names d'un sources_catalog, dans l'order où il les porte.
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

/// Rig observant les deux canaux de métadonnées : ce qui descend vers
/// les plugins, et l'état structuré qui monte vers la SPA et les afficheurs.
///
/// `plugins` porte l'order de déclaration, donc la priorité d'arbitrage.
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

/// Alias de `setup_metadata(vec![])` : les tests de l'état partiel
/// n'ont besoin d'aucun greffon `metadata`, seulement du montage que
/// `setup_metadata` sait déjà construire.
pub(super) fn test_core() -> (Core<FakePlayer>, watch::Receiver<NowPlaying>, watch::Receiver<PlayerState>, tempfile::TempDir) {
    setup_metadata(vec![])
}

/// Comme `test_core`, mais **garde** le récepteur du canal
/// d'extraction de cover embarquée plutôt que de le lâcher.
///
/// Nécessaire pour tout test qui laisse réellement tourner la tâche
/// détachée de `handle_path` sur un vrai fichier : celle-ci est l'unique
/// écrivaine légitime du fichier temporaire, et un test qui relirait les
/// tags une seconde fois de son côté (pour reconstituer le `CoverRef`
/// attendu) écrirait en concurrence avec elle sur le même path — une
/// vraie course entre deux écrivains, découverte à l'usage (voir le
/// rapport de tâche 6, ruling 1 de la revue).
#[allow(clippy::type_complexity)]
pub(super) fn test_core_with_extraction() -> (
    Core<FakePlayer>,
    watch::Receiver<PlayerState>,
    mpsc::Receiver<(String, Option<ritornello_proto::CoverRef>)>,
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
    /// Règle ce que le player factice prétend savoir de sa progress.
    pub(super) fn set_progress(&self, position_s: Option<f64>, duration_s: Option<f64>) {
        *self.player.progress.lock().unwrap() =
            crate::player::Progress { position_s, duration_s };
    }

    /// Recule l'ancre de `duration` : le test avance le temps sans dormir.
    pub(super) fn advance_anchor_for_test(&mut self, duration: std::time::Duration) {
        if let Some((p, pose)) = self.position_anchor {
            self.position_anchor = Some((p, pose - duration));
        }
    }
}

/// Cœur sans aucune source : le démarrage où *aucune* n'a répondu. C'est
/// exactement la situation dont le câblage à chaud doit pouvoir sortir, et
/// celle que le cœur doit désormais savoir serve — la page de statut est là
/// pour montrer les plugins figés.
///
/// Le récepteur d'état est rendition (et non lâché comme dans `silent_wiring`) :
/// « aucune source » est un état à observer, pas seulement à survivre.
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

/// Extrait le délai d'un `RetryIn`, ou échoue en nommant ce qui est arrivé.
pub(super) fn restart(outcome: EventOutcome) -> Duration {
    match outcome {
        EventOutcome::RetryIn(d) => d,
        autre => panic!("attendu RetryIn, obtenu {autre:?}"),
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

/// Mise à jour ne portant qu'un compte de présélections déclaré par la Source.
pub(super) fn update_with_count(compte: Option<u8>) -> SourceUpdate {
    SourceUpdate {
        identity: None,
        transient: false,
        preset: None,
        preset_count: compte,
        preset_name: None,
        status: None,
        can_eject: None,
        presets: None,
        cover: None,
    }
}

/// Mise à jour ne portant qu'un name de présélection déclaré par la Source.
pub(super) fn update_with_name(name: Option<&str>) -> SourceUpdate {
    SourceUpdate {
        identity: None,
        transient: false,
        preset: None,
        preset_count: None,
        preset_name: name.map(str::to_string),
        status: None,
        can_eject: None,
        presets: None,
        cover: None,
    }
}

/// Mise à jour ne portant que la capacité d'éjection déclarée par la Source.
pub(super) fn update_with_eject(peut: Option<bool>) -> SourceUpdate {
    SourceUpdate {
        identity: None,
        transient: false,
        preset: None,
        preset_count: None,
        preset_name: None,
        status: None,
        can_eject: peut,
        presets: None,
        cover: None,
    }
}

/// Trame à la forme que `serve_source` produit vraiment : `can_eject`
/// estampillé, parce que le SDK l'estampille sur **chaque** trame qu'il
/// écrit (voir la doc de `SourceMessage::can_eject`).
///
/// À préférer à `bare_update()` dans tout test qui prétend décrire une trame
/// venue d'un vrai greffon : `SourceUpdate::default()` laisse `can_eject` à
/// `None`, une forme que le SDK ne peut pas émettre, et un test bâti dessus
/// peut attester un mode de défaillance qui n'existe pas.
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

/// Fabrique un mp3 réel avec une cover embarquée, via ffmpeg — même
/// principe que `player::mpv::tests::mp3_avec_pochette`, dupliqué ici
/// faute d'un moyen simple de partager un utilitaire de test entre
/// modules. Rend `None` si ffmpeg est absent : le test se saute plutôt
/// que d'échouer, ce n'est pas une dépendance du cœur.
///
/// **L'image doit rester différente de celle de `player::mpv::tests`, et ce
/// n'est pas cosmétique.** Depuis que le fichier temporaire est nommé
/// d'après le *contenu* de l'image, deux fixtures portant la même image
/// visent le même path dans le `temp_dir()` **partagé** par tous les tests
/// de ce binaire — qui tournent en parallèle. Les tests d'ici traversent en
/// plus `CoverCache`, dont l'éviction **supprime** ces fichiers : la
/// collision s'est manifestée comme un échec intermittent chez le voisin,
/// qui lisait un fichier effacé ou réécrit sous lui. Les deux fixtures
/// partageaient `color=c=red:s=16x16`.
pub(super) fn test_mp3_with_cover(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let image = dir.join("cover.jpg");
    let sortie = dir.join("avec_pochette.mp3");
    let ok = std::process::Command::new("ffmpeg")
        // Verte et 32×32 : voir la doc ci-dessus, elle **ne doit pas**
        // coïncider avec celle de `player::mpv::tests`.
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
            .arg(&sortie)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    ok.then_some(sortie)
}

/// Pack français livré dans le dépôt (invariant : mêmes clés que l'anglais embarqué).
pub(super) fn fr_pack() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/core/fr.toml");
    std::fs::read_to_string(p).expect("pack fr livre")
}
