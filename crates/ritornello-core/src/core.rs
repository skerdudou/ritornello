use crate::player::Player;
use crate::state::{self, PersistedState};
use crate::types::Event;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_proto::{Command, SourceAction, SourceReq, View};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, RwLock};

/// Anglais embarqué du cœur (base toujours présente).
pub const EN: &str = include_str!("locales/en.toml");

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait Source: Send + Sync + 'static {
    async fn request(&self, req: SourceReq) -> Result<SourceAction>;
}

pub struct Core<P: Player> {
    player: P,
    sources: HashMap<String, Arc<dyn Source>>,
    source_order: Vec<String>,
    active_source: String,
    volume: u8,
    muted: bool,
    standby: bool,
    stopped: bool,
    retry_count: u32,
    audio_device: Option<String>,
    view: View,
    state_path: PathBuf,
    view_tx: watch::Sender<View>,
    catalog: Arc<RwLock<Catalog>>,
    locale: Option<String>,
    locales_root: PathBuf,
}

impl<P: Player> Core<P> {
    pub fn new(
        player: P,
        sources: HashMap<String, Arc<dyn Source>>,
        persisted: PersistedState,
        state_path: PathBuf,
        view_tx: watch::Sender<View>,
        catalog: Arc<RwLock<Catalog>>,
        locales_root: PathBuf,
    ) -> Self {
        let mut source_order: Vec<String> = sources.keys().cloned().collect();
        source_order.sort();
        let active_source = if sources.contains_key(&persisted.active_source) {
            persisted.active_source.clone()
        } else {
            source_order.first().cloned().unwrap_or_default()
        };
        Self {
            player,
            sources,
            source_order,
            active_source,
            volume: persisted.volume,
            muted: false,
            standby: false,
            stopped: false,
            retry_count: 0,
            audio_device: persisted.audio_device.clone(),
            view: View::default(),
            state_path,
            view_tx,
            catalog,
            locale: persisted.locale.clone(),
            locales_root,
        }
    }

    pub async fn resume(&mut self) -> Result<()> {
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        if let Some(locale) = self.locale.clone() {
            for name in self.source_order.clone() {
                if let Some(src) = self.sources.get(&name) {
                    let _ = src.request(SourceReq::SetLocale(locale.clone())).await;
                }
            }
        }
        let action = self.active().request(SourceReq::Activate).await?;
        self.apply(action).await
    }

    /// Rejoue le contenu courant de la source active (`Activate` demande à la
    /// source de redonner l'URI en cours, pas de passer au contenu suivant).
    pub async fn retry_stream(&mut self) -> Result<()> {
        if !self.standby && !self.stopped {
            let action = self.active().request(SourceReq::Activate).await?;
            self.apply(action).await?;
        }
        Ok(())
    }

    pub fn handle_source_view(&mut self, name: &str, view: View) {
        if self.standby {
            return;
        }
        if name == self.active_source {
            self.view = view;
            self.push_view();
        }
    }

    pub async fn handle_command(&mut self, cmd: Command) -> Result<()> {
        if self.standby && cmd != Command::Power {
            return Ok(());
        }
        match cmd {
            Command::Select(n) => {
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Select(n)).await?;
                self.apply(action).await?;
            }
            Command::Next => {
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Next).await?;
                self.apply(action).await?;
            }
            Command::Prev => {
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Prev).await?;
                self.apply(action).await?;
            }
            Command::NextTrack => {
                let action = self.active().request(SourceReq::NextTrack).await?;
                self.apply(action).await?;
            }
            Command::PrevTrack => {
                let action = self.active().request(SourceReq::PrevTrack).await?;
                self.apply(action).await?;
            }
            Command::Eject => {
                let action = self.active().request(SourceReq::Eject).await?;
                self.apply(action).await?;
            }
            Command::VolumeUp | Command::VolumeDown => {
                let v = self.volume as i16 + if cmd == Command::VolumeUp { 5 } else { -5 };
                self.volume = v.clamp(0, 100) as u8;
                self.player.set_volume(self.volume).await?;
                self.persist();
                self.push_view();
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
            }
            Command::PlayPause => self.player.toggle_pause().await?,
            Command::Stop => {
                self.stopped = true;
                self.player.stop().await?;
            }
            Command::Power => {
                self.standby = !self.standby;
                if self.standby {
                    let _ = self.active().request(SourceReq::Deactivate).await;
                    self.player.stop().await?;
                    self.view = self.standby_view().await;
                    self.push_view();
                } else {
                    self.resume().await?;
                }
            }
            Command::SourceCycle => {
                let _ = self.active().request(SourceReq::Deactivate).await;
                let idx = self.source_order.iter().position(|n| n == &self.active_source).unwrap_or(0);
                let next_idx = (idx + 1) % self.source_order.len().max(1);
                if let Some(next_name) = self.source_order.get(next_idx).cloned() {
                    self.active_source = next_name;
                }
                self.retry_count = 0;
                self.stopped = false;
                let action = self.active().request(SourceReq::Activate).await?;
                self.apply(action).await?;
                self.persist();
            }
        }
        Ok(())
    }

    pub async fn handle_event(&mut self, ev: Event) -> Option<Duration> {
        match ev {
            Event::Title(_) | Event::PlaybackActive => {
                self.retry_count = 0;
            }
            Event::TrackChanged(_) => {}
            Event::PlaybackIdle => {
                if !self.standby && !self.stopped {
                    let delay = (RETRY_BASE * 2u32.pow(self.retry_count)).min(RETRY_MAX);
                    self.retry_count = (self.retry_count + 1).min(4);
                    return Some(delay);
                }
            }
        }
        None
    }

    fn active(&self) -> Arc<dyn Source> {
        self.sources
            .get(&self.active_source)
            .cloned()
            .unwrap_or_else(|| panic!("source active inconnue: {}", self.active_source))
    }

    async fn apply(&mut self, action: SourceAction) -> Result<()> {
        match action {
            SourceAction::Noop => {}
            SourceAction::Play { uri } => {
                self.stopped = false;
                self.player.play(&uri).await?;
            }
            SourceAction::Stop => {
                self.stopped = true;
                self.player.stop().await?;
            }
            SourceAction::PlayerNext => self.player.next().await?,
            SourceAction::PlayerPrev => self.player.prev().await?,
        }
        Ok(())
    }

    pub async fn set_audio_device(&mut self, device: String) -> Result<()> {
        self.player.set_audio_device(&device).await?;
        self.audio_device = Some(device);
        self.persist();
        Ok(())
    }

    /// Change la langue courante : reconstruit le catalogue partagé du cœur
    /// (lu par la page de statut), persiste l'état, et pousse `SetLocale` à
    /// chaque plugin Source connecté (best-effort).
    ///
    /// Appelée depuis la boucle `select!` de `main` sur réception du canal
    /// `locale_rx`, lui-même alimenté par la route `PUT /api/locale`.
    pub async fn set_locale(&mut self, locale: String) -> Result<()> {
        self.locale = Some(locale.clone());
        *self.catalog.write().await = Catalog::load("core", &locale, &self.locales_root, EN);
        self.persist();
        for name in self.source_order.clone() {
            if let Some(src) = self.sources.get(&name) {
                if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                    tracing::warn!("SetLocale vers {name}: {e}");
                }
            }
        }
        Ok(())
    }

    fn persist(&self) {
        let st = PersistedState {
            active_source: self.active_source.clone(),
            volume: self.volume,
            audio_device: self.audio_device.clone(),
            locale: self.locale.clone(),
        };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistance impossible: {e}");
        }
    }

    fn push_view(&self) {
        let _ = self.view_tx.send(self.view.clone());
    }

    async fn standby_view(&self) -> View {
        let cat = self.catalog.read().await;
        View { line1: cat.get("standby").to_string(), line2: String::new(), line3: String::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct FakePlayer {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::player::Player for FakePlayer {
        async fn play(&self, uri: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("play {uri}"));
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("stop".into());
            Ok(())
        }
        async fn toggle_pause(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("pause".into());
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
    }

    struct FakeSource {
        name: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Source for FakeSource {
        async fn request(&self, req: SourceReq) -> Result<SourceAction> {
            self.calls.lock().unwrap().push(format!("{}:{:?}", self.name, req));
            Ok(match (self.name, req) {
                ("radio", SourceReq::Activate) => SourceAction::Play { uri: "http://fip".into() },
                ("radio", SourceReq::Select(3)) => SourceAction::Play { uri: "http://inter".into() },
                ("radio", SourceReq::Select(_)) => SourceAction::Noop,
                ("cd", SourceReq::Activate) => SourceAction::Play { uri: "cdda://".into() },
                (_, SourceReq::Eject) if self.name == "cd" => SourceAction::Stop,
                _ => SourceAction::Noop,
            })
        }
    }

    fn setup() -> (Core<FakePlayer>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>, watch::Receiver<View>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
        let (tx, rx) = watch::channel(View::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let core = Core::new(player, sources, PersistedState::default(), dir.path().join("state.json"), tx, catalog, root);
        (core, player_calls, source_calls, rx, dir)
    }

    #[tokio::test]
    async fn resume_active_la_source_persistee() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Activate"));
    }

    #[tokio::test]
    async fn select_relaye_a_la_source_active_sans_changer_active_source() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://inter".to_string()));
        // Select agit sur la source deja active ; seul SourceCycle change active_source.
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "radio");
    }

    #[tokio::test]
    async fn source_cycle_bascule_et_persiste() {
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Deactivate"));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Activate"));
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
    }

    #[tokio::test]
    async fn standby_bloque_tout_sauf_power() {
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
        core.handle_command(Command::Select(3)).await.unwrap();
        // aucun nouvel appel "play" apres la veille tant qu'on n'a pas fait Power a nouveau
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 1);
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 2);
    }

    #[tokio::test]
    async fn veille_affiche_un_message_dedie_et_ignore_les_vues_pendant_ce_temps() {
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(rx.borrow_and_update().line1, "STANDBY");
        core.handle_source_view("radio", View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() });
        assert_eq!(rx.borrow().line1, "STANDBY"); // toujours en veille, la vue source est ignoree
        core.handle_command(Command::Power).await.unwrap();
        core.handle_source_view("radio", View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() });
        assert_eq!(rx.borrow_and_update().line1, "RADIO  P1"); // le reveil laisse la source reprendre l'affichage
    }

    #[tokio::test]
    async fn resume_applique_la_sortie_audio_persistee() {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (tx, _rx) = watch::channel(View::default());
        let persisted = PersistedState { active_source: "radio".into(), volume: 60, audio_device: Some("bluealsa:DEV=XX".into()), locale: None };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::core::EN)));
        let mut core = Core::new(player, sources, persisted, dir.path().join("state.json"), tx, catalog, root);
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device bluealsa:DEV=XX".to_string()));
    }

    #[tokio::test]
    async fn set_audio_device_applique_et_persiste() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device("hw:CARD=Headphones".into()).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device hw:CARD=Headphones".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device.as_deref(), Some("hw:CARD=Headphones"));
    }

    #[tokio::test]
    async fn stop_intentionnel_ne_declenche_pas_de_retry() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, None);
    }

    #[tokio::test]
    async fn backoff_croissant_puis_reinitialise_par_un_titre() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        let d1 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        let d2 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert!(d2 > d1);
        core.handle_event(Event::Title("ok".into())).await;
        let d3 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert_eq!(d3, d1);
    }

    #[tokio::test]
    async fn vue_dune_source_inactive_est_ignoree() {
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_source_view("cd", View { line1: "CD".into(), line2: "".into(), line3: "".into() });
        assert!(rx.borrow().line1.is_empty()); // la vue de "cd" (inactive) n'a pas ete appliquee
        core.handle_source_view("radio", View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() });
        assert!(rx.borrow_and_update().line1.contains("RADIO"));
    }

    #[tokio::test]
    async fn standby_view_est_traduit_par_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (tx, mut rx) = watch::channel(View::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "fr", &root, crate::core::EN)));
        let mut core = Core::new(player, sources, PersistedState::default(), dir.path().join("state.json"), tx, catalog, root);
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(rx.borrow_and_update().line1, "VEILLE");
    }

    #[tokio::test]
    async fn set_locale_persiste_et_notifie_les_sources() {
        let (mut core, _pc, source_calls, _rx, dir) = setup();
        core.set_locale("fr".into()).await.unwrap();
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:SetLocale(\"fr\")"));
        assert!(calls.iter().any(|c| c == "cd:SetLocale(\"fr\")"));
        drop(calls);
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.locale.as_deref(), Some("fr"));
    }
}
