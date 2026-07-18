use crate::config::Stations;
use crate::player::Player;
use crate::state::{self, PersistedState};
use crate::types::{Command, DiscInfo, Event, Mode, View};
use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

pub struct Core<P: Player> {
    player: P,
    stations: Stations,
    mode: Mode,
    preset: u8,
    volume: u8,
    muted: bool,
    standby: bool,
    stopped: bool,
    cd_present: bool,
    cd_track: i64,
    disc_info: Option<DiscInfo>,
    title: String,
    status: String,
    retry_count: u32,
    state_path: PathBuf,
    stations_path: PathBuf,
    view_tx: watch::Sender<View>,
}

impl<P: Player> Core<P> {
    pub fn new(
        player: P,
        stations: Stations,
        persisted: PersistedState,
        state_path: PathBuf,
        stations_path: PathBuf,
        view_tx: watch::Sender<View>,
    ) -> Self {
        Self {
            player,
            stations,
            mode: persisted.mode,
            preset: persisted.preset,
            volume: persisted.volume,
            muted: false,
            standby: false,
            stopped: false,
            cd_present: false,
            cd_track: 0,
            disc_info: None,
            title: String::new(),
            status: String::new(),
            retry_count: 0,
            state_path,
            stations_path,
            view_tx,
        }
    }

    pub async fn resume(&mut self) -> Result<()> {
        self.player.set_volume(self.volume).await?;
        match self.mode {
            Mode::Radio => self.play_preset(self.preset).await?,
            Mode::Cd => self.play_cd().await?,
        }
        Ok(())
    }

    pub fn set_disc_info(&mut self, info: Option<DiscInfo>) {
        self.disc_info = info;
        self.push_view();
    }

    pub async fn retry_stream(&mut self) -> Result<()> {
        if self.mode == Mode::Radio && !self.standby {
            self.play_preset(self.preset).await?;
        }
        Ok(())
    }

    pub async fn handle_command(&mut self, cmd: Command) -> Result<()> {
        if self.standby && cmd != Command::Power && cmd != Command::ReloadStations {
            return Ok(()); // en veille, seul Power réveille
        }
        match cmd {
            Command::Preset(n) => {
                if self.stations.by_preset(n).is_some() {
                    self.retry_count = 0;
                    self.mode = Mode::Radio;
                    self.play_preset(n).await?;
                    self.persist();
                } else {
                    self.status = "présélection vide".into();
                    self.push_view();
                }
            }
            Command::StationNext | Command::StationPrev => {
                self.retry_count = 0;
                self.mode = Mode::Radio;
                let next = if cmd == Command::StationNext {
                    self.stations.next_preset(self.preset)
                } else {
                    self.stations.prev_preset(self.preset)
                };
                if let Some(n) = next {
                    self.play_preset(n).await?;
                    self.persist();
                }
            }
            Command::VolumeUp | Command::VolumeDown => {
                let v = self.volume as i16 + if cmd == Command::VolumeUp { 5 } else { -5 };
                self.volume = v.clamp(0, 100) as u8;
                self.player.set_volume(self.volume).await?;
                self.status = format!("Volume {}", self.volume);
                self.persist();
                self.push_view();
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
                self.status = if self.muted { "Muet".into() } else { String::new() };
                self.push_view();
            }
            Command::ToggleMode => {
                self.retry_count = 0;
                self.mode = match self.mode {
                    Mode::Radio => Mode::Cd,
                    Mode::Cd => Mode::Radio,
                };
                match self.mode {
                    Mode::Radio => self.play_preset(self.preset).await?,
                    Mode::Cd => self.play_cd().await?,
                }
                self.persist();
            }
            Command::PlayPause => self.player.toggle_pause().await?,
            Command::NextTrack => {
                if self.mode == Mode::Cd {
                    self.player.next().await?;
                }
            }
            Command::PrevTrack => {
                if self.mode == Mode::Cd {
                    self.player.prev().await?;
                }
            }
            Command::Stop => {
                self.stopped = true;
                self.player.stop().await?;
                self.title.clear();
                self.push_view();
            }
            Command::Eject => {
                if self.mode == Mode::Cd {
                    self.player.stop().await?;
                    // L'éjection matérielle est faite par le module cd (main relaie).
                }
            }
            Command::Power => {
                self.standby = !self.standby;
                if self.standby {
                    self.player.stop().await?;
                } else {
                    self.resume().await?;
                }
                self.push_view();
            }
            Command::ReloadStations => {
                match Stations::load(&self.stations_path) {
                    Ok(s) => {
                        self.stations = s;
                        self.status = "Stations rechargées".into();
                    }
                    Err(e) => {
                        tracing::warn!("stations.toml invalide, config conservée: {e}");
                        self.status = "stations.toml invalide".into();
                    }
                }
                self.push_view();
            }
        }
        Ok(())
    }

    /// Retourne Some(délai) si la boucle principale doit reprogrammer un retry du flux.
    pub async fn handle_event(&mut self, ev: Event) -> Option<Duration> {
        match ev {
            Event::Title(t) => {
                self.title = t;
                self.status.clear();
                self.retry_count = 0;
                self.push_view();
            }
            Event::TrackChanged(n) => {
                self.cd_track = n.max(0);
                self.push_view();
            }
            Event::PlaybackActive => {
                self.retry_count = 0;
            }
            Event::PlaybackIdle => {
                if self.mode == Mode::Radio && !self.standby && !self.stopped {
                    let delay = (RETRY_BASE * 2u32.pow(self.retry_count)).min(RETRY_MAX);
                    self.retry_count = (self.retry_count + 1).min(4);
                    self.status = "connexion…".into();
                    self.push_view();
                    return Some(delay);
                }
            }
            Event::CdInserted => {
                self.cd_present = true;
                self.status = "CD inséré".into();
                if self.mode == Mode::Cd && !self.standby {
                    let _ = self.play_cd().await;
                }
                self.push_view();
            }
            Event::CdRemoved => {
                self.cd_present = false;
                self.disc_info = None;
                self.cd_track = 0;
                if self.mode == Mode::Cd {
                    let _ = self.player.stop().await;
                }
                self.push_view();
            }
        }
        None
    }

    async fn play_preset(&mut self, n: u8) -> Result<()> {
        if let Some(st) = self.stations.by_preset(n) {
            self.stopped = false;
            self.preset = n;
            self.title.clear();
            self.status.clear();
            let url = st.url.clone();
            self.player.play(&url).await?;
        } else {
            self.status = "présélection vide".into();
        }
        self.push_view();
        Ok(())
    }

    async fn play_cd(&mut self) -> Result<()> {
        self.title.clear();
        if self.cd_present {
            self.stopped = false;
            self.status.clear();
            self.player.play("cdda://").await?;
        } else {
            self.status = "pas de disque".into();
        }
        self.push_view();
        Ok(())
    }

    fn persist(&self) {
        let st = PersistedState { mode: self.mode, preset: self.preset, volume: self.volume };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistance impossible: {e}");
        }
    }

    fn view(&self) -> View {
        if self.standby {
            return View { line1: "VEILLE".into(), line2: String::new(), line3: String::new() };
        }
        match self.mode {
            Mode::Radio => {
                let name = self.stations.by_preset(self.preset).map(|s| s.name.clone());
                View {
                    line1: format!("RADIO  P{}", self.preset),
                    line2: name.unwrap_or_else(|| self.status.clone()),
                    line3: if self.status.is_empty() { self.title.clone() } else { self.status.clone() },
                }
            }
            Mode::Cd => {
                let (line1, line2, line3) = if !self.cd_present {
                    ("CD".into(), "pas de disque".into(), String::new())
                } else if let Some(info) = &self.disc_info {
                    let n = self.cd_track as usize;
                    (
                        format!("CD  {}/{}", n + 1, info.tracks.len()),
                        format!("{} — {}", info.artist, info.album),
                        info.tracks.get(n).cloned().unwrap_or_default(),
                    )
                } else {
                    (
                        format!("CD  piste {}", self.cd_track + 1),
                        "CD audio".into(),
                        if self.status.is_empty() { self.title.clone() } else { self.status.clone() },
                    )
                };
                View { line1, line2, line3 }
            }
        }
    }

    fn push_view(&self) {
        let _ = self.view_tx.send(self.view());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Station, Stations};
    use crate::state::PersistedState;
    use crate::types::{Command, Event, Mode};
    use std::sync::{Arc, Mutex};
    use tokio::sync::watch;

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
    }

    fn setup(persisted: PersistedState) -> (Core<FakePlayer>, Arc<Mutex<Vec<String>>>, watch::Receiver<crate::types::View>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let stations = Stations {
            stations: vec![
                Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 },
                Station { name: "Inter".into(), url: "http://inter".into(), preset: 3 },
            ],
        };
        stations.save(&dir.path().join("stations.toml")).unwrap();
        let player = FakePlayer::default();
        let calls = player.calls.clone();
        let (tx, rx) = watch::channel(crate::types::View::default());
        let core = Core::new(
            player,
            stations,
            persisted,
            dir.path().join("state.json"),
            dir.path().join("stations.toml"),
            tx,
        );
        (core, calls, rx, dir)
    }

    #[tokio::test]
    async fn resume_rejoue_la_station_persistee() {
        let (mut core, calls, _rx, _d) =
            setup(PersistedState { mode: Mode::Radio, preset: 3, volume: 40 });
        core.resume().await.unwrap();
        let log = calls.lock().unwrap().clone();
        assert!(log.contains(&"vol 40".to_string()));
        assert!(log.contains(&"play http://inter".to_string()));
    }

    #[tokio::test]
    async fn preset_change_de_station_et_persiste() {
        let (mut core, calls, rx, dir) = setup(PersistedState::default());
        core.handle_command(Command::Preset(3)).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"play http://inter".to_string()));
        assert!(rx.borrow().line2.contains("Inter"));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.preset, 3);
    }

    #[tokio::test]
    async fn preset_vide_ne_joue_rien() {
        let (mut core, calls, rx, dir) = setup(PersistedState::default());
        core.handle_command(Command::Preset(7)).await.unwrap();
        assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("play")));
        assert!(rx.borrow().line3.contains("vide"));
        // le preset courant n'a pas change : l'etat persiste garde 1
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.preset, 1);
    }

    #[tokio::test]
    async fn backoff_reinitialise_au_changement_manuel_de_station() {
        let (mut core, _calls, _rx, _d) = setup(PersistedState::default());
        core.resume().await.unwrap();
        let d1 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        let d2 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert!(d2 > d1);
        core.handle_command(Command::Preset(3)).await.unwrap();
        let d3 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert_eq!(d3, d1); // repart a la base apres action manuelle
    }

    #[tokio::test]
    async fn volume_borne_0_100_pas_de_5() {
        let (mut core, calls, _rx, _d) =
            setup(PersistedState { mode: Mode::Radio, preset: 1, volume: 98 });
        core.handle_command(Command::VolumeUp).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let log = calls.lock().unwrap().clone();
        assert!(log.contains(&"vol 100".to_string()));
        assert_eq!(log.iter().filter(|c| *c == "vol 100").count(), 2); // plafonné
    }

    #[tokio::test]
    async fn toggle_mode_bascule_radio_cd() {
        let (mut core, calls, rx, _d) = setup(PersistedState::default());
        core.handle_event(Event::CdInserted).await;
        core.handle_command(Command::ToggleMode).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"play cdda://".to_string()));
        assert!(rx.borrow().line1.contains("CD"));
        core.handle_command(Command::ToggleMode).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"play http://fip".to_string()));
    }

    #[tokio::test]
    async fn toggle_mode_sans_disque_affiche_message() {
        let (mut core, calls, rx, _d) = setup(PersistedState::default());
        core.handle_command(Command::ToggleMode).await.unwrap();
        assert!(!calls.lock().unwrap().contains(&"play cdda://".to_string()));
        assert!(rx.borrow().line2.to_lowercase().contains("pas de disque"));
    }

    #[tokio::test]
    async fn idle_en_radio_declenche_backoff_croissant() {
        let (mut core, _calls, rx, _d) = setup(PersistedState::default());
        core.resume().await.unwrap();
        let d1 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        let d2 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert!(d2 > d1);
        assert!(rx.borrow().line3.contains("connexion"));
        // Un titre reçu = lecture repartie -> backoff réinitialisé
        core.handle_event(Event::Title("ok".into())).await;
        let d3 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert_eq!(d3, d1);
    }

    #[tokio::test]
    async fn power_met_en_veille_et_reprend() {
        let (mut core, calls, rx, _d) = setup(PersistedState::default());
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"stop".to_string()));
        assert!(rx.borrow().line1.to_lowercase().contains("veille"));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 2);
    }

    #[tokio::test]
    async fn cd_retire_stoppe_en_mode_cd() {
        let (mut core, calls, rx, _d) = setup(PersistedState::default());
        core.handle_event(Event::CdInserted).await;
        core.handle_command(Command::ToggleMode).await.unwrap();
        core.handle_event(Event::CdRemoved).await;
        assert!(calls.lock().unwrap().contains(&"stop".to_string()));
        assert!(rx.borrow().line2.to_lowercase().contains("pas de disque"));
    }

    #[tokio::test]
    async fn titres_musicbrainz_affiches_en_cd() {
        let (mut core, _calls, rx, _d) = setup(PersistedState::default());
        core.handle_event(Event::CdInserted).await;
        core.handle_command(Command::ToggleMode).await.unwrap();
        core.set_disc_info(Some(crate::types::DiscInfo {
            artist: "Miles Davis".into(),
            album: "Kind of Blue".into(),
            tracks: vec!["So What".into(), "Freddie Freeloader".into()],
        }));
        core.handle_event(Event::TrackChanged(1)).await;
        let v = rx.borrow().clone();
        assert!(v.line2.contains("Miles Davis"));
        assert!(v.line3.contains("Freddie Freeloader"));
        assert!(v.line1.contains("2/2"));
    }

    #[tokio::test]
    async fn stop_intentionnel_ne_declenche_pas_de_retry() {
        let (mut core, _calls, _rx, _d) = setup(PersistedState::default());
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, None);
        // une nouvelle lecture rearme le retry
        core.handle_command(Command::Preset(1)).await.unwrap();
        assert!(core.handle_event(Event::PlaybackIdle).await.is_some());
    }

    #[tokio::test]
    async fn reload_stations_traite_en_veille() {
        let (mut core, calls, _rx, dir) = setup(PersistedState::default());
        core.handle_command(Command::Power).await.unwrap(); // veille
        let nouvelles = Stations {
            stations: vec![Station { name: "Nova".into(), url: "http://nova".into(), preset: 1 }],
        };
        nouvelles.save(&dir.path().join("stations.toml")).unwrap();
        core.handle_command(Command::ReloadStations).await.unwrap();
        core.handle_command(Command::Power).await.unwrap(); // reveil
        assert!(calls.lock().unwrap().contains(&"play http://nova".to_string()));
    }

    #[tokio::test]
    async fn preset_invalide_ne_bascule_pas_le_mode() {
        let (mut core, _calls, rx, _d) = setup(PersistedState::default());
        core.handle_event(Event::CdInserted).await;
        core.handle_command(Command::ToggleMode).await.unwrap();
        core.handle_command(Command::Preset(7)).await.unwrap();
        assert!(rx.borrow().line1.contains("CD")); // toujours en mode CD
    }
}
