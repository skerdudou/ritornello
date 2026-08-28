//! Reglages persistes : sortie audio, langue, theme, et l'ecriture de state.json.

use super::*;

impl<P: Player> Core<P> {
    /// Applies an output choice from the config page. `None` means "follow
    /// the system default": mpv gets its native `auto` back (settable at
    /// runtime), and nothing is recorded on disk — the same state as a fresh
    /// install, where `resume()` sends no device at all.
    pub async fn set_audio_device(&mut self, device: Option<String>) -> Result<()> {
        match &device {
            Some(d) => self.player.set_audio_device(d).await?,
            None => self.player.set_audio_device("auto").await?,
        }
        self.audio_device = device;
        self.persist();
        Ok(())
    }

    /// Change la langue courante : reconstruit le catalogue partagé du cœur
    /// (lu par la page de statut), persiste l'état, et pousse `SetLocale` à
    /// chaque plugin Source connecté (best-effort).
    ///
    /// Appelée depuis la boucle `select!` de `main` sur réception du canal
    /// `locale_rx`, lui-même alimenté par la route `PUT /api/locale`.
    ///
    /// Résout aussi `standby_status` dans le catalogue tout neuf, et publie
    /// l'état : sans ce dernier, changer de langue pendant la veille laissait
    /// le mot affiché dans l'ancienne langue jusqu'au prochain cycle
    /// `Command::Power` (voir la doc de `standby_status`).
    pub async fn set_locale(&mut self, locale: String) -> Result<()> {
        self.locale = Some(locale.clone());
        let nouveau = Catalog::load("core", &locale, &self.locales_root, crate::i18n::EN);
        self.standby_status = Some(resout_standby_status(&nouveau));
        *self.catalog.write().await = nouveau;
        self.persist();
        for name in self.source_order.clone() {
            if let Some(src) = self.sources.get(&name) {
                if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                    tracing::warn!("SetLocale to {name}: {e}");
                }
            }
        }
        self.publie_etat();
        Ok(())
    }

    /// Change le thème courant et le persiste. Contrairement à `set_locale`,
    /// rien n'est poussé aux plugins : le thème est un réglage d'apparence de
    /// l'IHM web, dont aucun plugin n'a connaissance.
    ///
    /// Appelée depuis la boucle `select!` de `main` sur réception du canal
    /// `theme_rx`, lui-même alimenté par la route `PUT /api/theme`.
    pub fn set_theme(&mut self, t: crate::theme::ThemeState) {
        self.theme = Some(t.theme);
        self.mode = Some(t.mode);
        self.persist();
    }

    pub(super) fn persist(&self) {
        let st = PersistedState {
            active_source: self.active_source.clone(),
            volume: self.volume,
            standby: self.standby,
            audio_device: self.audio_device.clone(),
            locale: self.locale.clone(),
            theme: self.theme.clone(),
            mode: self.mode.clone(),
            settings: self.settings.clone(),
        };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistence failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn resume_applique_la_sortie_audio_persistee() {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let persisted = PersistedState {
            active_source: "radio".into(),
            volume: 60,
            standby: false,
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            theme: None,
            mode: None,
            settings: crate::state::Settings::default(),
        };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device bluealsa:DEV=XX".to_string()));
    }

    #[tokio::test]
    async fn set_audio_device_applique_et_persiste() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device(Some("hw:CARD=Headphones".into())).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device hw:CARD=Headphones".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device.as_deref(), Some("hw:CARD=Headphones"));
    }

    #[tokio::test]
    async fn set_audio_device_none_revient_au_defaut_systeme() {
        // "System default" from the config page: nothing imposed on mpv
        // anymore (its native `auto`), and no device recorded on disk.
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device(Some("hw:CARD=Headphones".into())).await.unwrap();
        core.set_audio_device(None).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device auto".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device, None);
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

    #[tokio::test]
    async fn le_mot_de_veille_est_traduit_par_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (etat_tx, mut etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "fr", &root, crate::i18n::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            etat: etat_tx,
        };
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("VEILLE"));
    }

    #[tokio::test]
    async fn changer_de_langue_en_veille_republie_aussitot_le_mot_de_veille() {
        // Régression (M1+M9, revue de branche) : le mot de veille n'était
        // résolu qu'au moment de poser la veille (`Command::Power`), et
        // `set_locale` ne publiait de toute façon aucun état. Changer de
        // langue *pendant* la veille laissait donc le mot affiché dans
        // l'ancienne langue jusqu'au prochain cycle Power.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/fr.toml"), "standby = \"VEILLE\"\n").unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (etat_tx, mut etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        // Construction en anglais : "STANDBY", la valeur embarquée de la clé.
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            etat: etat_tx,
        };
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));
        core.set_locale("fr".into()).await.unwrap();
        assert_eq!(
            etat_rx.borrow_and_update().status.as_deref(),
            Some("VEILLE"),
            "set_locale doit republier aussitot le nouveau mot de veille, sans attendre un nouveau cycle Power"
        );
    }

    /// La veille sur disque doit decrire l'appareil, pas une intention : c'est
    /// tout ce que `StartupPower::Previous` a pour se decider au prochain
    /// demarrage. Les deux sens de la bascule et les deux branches du
    /// demarrage l'ecrivent.
    #[tokio::test]
    async fn la_veille_est_persistee_a_chaque_bascule() {
        let (mut core, _pc, _sc, _rx, dir) = setup();
        let sur_disque = || crate::state::load(&dir.path().join("state.json")).standby;

        core.handle_command(Command::Power).await.unwrap(); // veille
        assert!(sur_disque(), "la mise en veille s'ecrit");
        core.handle_command(Command::Power).await.unwrap(); // reveil
        assert!(!sur_disque(), "le reveil aussi");

        // Et un demarrage remet le fichier d'accord avec ce qu'il a fait,
        // dans les deux sens : sans cela, « etat precedent » choisi plus tard
        // ressusciterait une veille que l'appareil a quittee depuis longtemps.
        core.start_in_standby().await.unwrap();
        assert!(sur_disque());
        core.demarrage().await.unwrap(); // reglage par defaut : « allume »
        assert!(!sur_disque());
    }

    #[test]
    fn en_embarque_du_coeur_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(crate::i18n::EN).unwrap().is_empty());
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::i18n::EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }
}
