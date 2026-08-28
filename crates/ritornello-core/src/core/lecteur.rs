//! Evenements du lecteur mpv : fin de lecture, relance a rebours croissant, reprise au reveil, et la demande relayee a la source active.

use super::*;

impl<P: Player> Core<P> {
    pub async fn resume(&mut self) -> Result<()> {
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        if let Some(locale) = self.locale.clone() {
            for name in self.source_order.clone() {
                if let Some(src) = self.sources.get(&name) {
                    if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                        tracing::warn!("SetLocale to {name}: {e}");
                    }
                }
            }
        }
        if let Some(action) = self.demande_active(SourceReq::Wake).await? {
            self.apply(action).await?;
        }
        // L'IHM doit connaître le volume et la source dès le premier affichage,
        // sans attendre qu'on touche à quelque chose.
        self.publie_etat();
        Ok(())
    }

    /// Rejoue le contenu courant de la source active (`Activate` demande à la
    /// source de redonner l'URI en cours, pas de passer au contenu suivant).
    pub async fn retry_stream(&mut self) -> Result<()> {
        if !self.standby && self.expecting_stream {
            if let Some(action) = self.demande_active(SourceReq::Activate).await? {
                self.apply(action).await?;
            }
        }
        Ok(())
    }

    pub async fn handle_event(&mut self, ev: Event) -> EventOutcome {
        match ev {
            // Un seul endroit décide quelles variantes attestent la vivacité
            // du flux : la boucle de `main` (qui tient l'échéance `retry_at`)
            // et ce compteur suivent le même verdict via `StreamAlive`, au
            // lieu de dupliquer la liste des variantes de part et d'autre.
            Event::Title(_) | Event::PlaybackActive => {
                self.retry_count = 0;
                return EventOutcome::StreamAlive;
            }
            // Volontairement sans effet sur `retry_count` : la vivacité du flux
            // est déjà attestée par `PlaybackActive`, et un titre ICY n'est pas
            // une preuve de lecture (une station peut en envoyer un puis se
            // taire). Ici, uniquement des métadonnées.
            Event::IcyTitle(titre) => self.handle_icy_title(titre),
            // Même statut que l'ICY vis-à-vis de `retry_count` : des
            // métadonnées ne prouvent pas que la lecture est vivante.
            Event::FileTags(morceau) => self.handle_file_tags(morceau),
            // Même statut que les tags vis-à-vis de `retry_count` : le chemin
            // n'atteste rien de la vivacité du flux, il sert uniquement à la
            // pochette embarquée.
            Event::Path(chemin) => self.handle_path(chemin),
            // Le lecteur a changé de piste de lui-même : fin de piste d'un
            // disque, pression sur aucune touche. Le cœur le sait (mpv le lui
            // dit) mais ne peut pas corriger l'identité — elle est opaque pour
            // lui. Il le dit donc à la Source, qui renverra vue et identité par
            // le canal habituel. Sans cela, l'affichage et les métadonnées
            // restaient sur la piste précédente jusqu'à la prochaine commande.
            //
            // L'événement arrive aussi pour les changements **demandés** (la
            // Source vient déjà de se recaler) : elle renvoie alors la même
            // identité, que le cœur reconnaît comme inchangée, et la vue
            // identique n'est pas repoussée.
            Event::TrackChanged(n) => {
                if !self.standby {
                    if let Err(e) = self.demande_active(SourceReq::PlayerTrack(n)).await {
                        tracing::debug!("track notification to source: {e}");
                    }
                }
            }
            Event::PlaybackIdle => {
                if !self.standby && self.expecting_stream {
                    let delay = (RETRY_BASE * 2u32.pow(self.retry_count)).min(RETRY_MAX);
                    self.retry_count = (self.retry_count + 1).min(4);
                    return EventOutcome::RetryIn(delay);
                }
                // Fin de lecture **normale** (fin de disque, notamment) : le
                // dire à la Source, seule à pouvoir recaler son état de
                // lecture, sa vue et son identité — le cœur ne peut pas
                // inventer « plus rien ne joue » à sa place, l'identité est
                // opaque. Sans cela, la fin d'un disque laissait la dernière
                // piste et ses métadonnées affichées indéfiniment.
                // Idempotent quand l'arrêt vient d'une commande (la Source a
                // déjà été prévenue par `Command::Stop`).
                //
                // Plus rien ne joue : sans cela, les tags du dernier fichier
                // resteraient recevables et un ultime rafraîchissement de mpv
                // les remettrait à l'écran après la fin de la liste.
                self.lecture = false;
                if !self.standby {
                    if let Err(e) = self.demande_active(SourceReq::Stop).await {
                        tracing::debug!("stop notification to source: {e}");
                    }
                }
            }
        }
        EventOutcome::Nothing
    }

    /// Requête à la source active, **s'il y en a une**.
    ///
    /// `Ok(None)` n'est pas une erreur : depuis l'enregistrement à chaud, le
    /// cœur peut tourner sans aucune source. Un greffon `source` qui rate la
    /// fenêtre de rendez-vous s'annonce à t+30 s et est câblé sans redémarrage,
    /// et refuser de démarrer à t+10 s pour l'attendre supprimait la page de
    /// statut précisément quand on voulait l'y voir figé.
    ///
    /// C'est ce que le `panic!("unknown active source")` d'avant interdisait :
    /// il ne protégeait aucun invariant — `Core::new` retombe déjà sur la
    /// première source triée, donc le nom n'est introuvable que si la table est
    /// **vide** — et il aurait échangé un refus de démarrer lisible contre un
    /// arrêt brutal au démarrage, sans page pour le raconter.
    ///
    /// Sans source, une commande **ne fait rien** et le dit en `debug` : ce
    /// n'est pas une anomalie, seulement un appareil qui n'a rien à lire.
    /// Un `warn` remplirait le tampon d'erreurs de l'IHM à chaque touche.
    pub(super) async fn demande_active(&self, req: SourceReq) -> Result<Option<SourceAction>> {
        let Some(source) = self.sources.get(&self.active_source) else {
            tracing::debug!("no active source, dropping {req:?}");
            return Ok(None);
        };
        source.request(req).await.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn resume_active_la_source_persistee() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Wake"));
    }

    #[tokio::test]
    async fn resume_envoie_wake_pas_activate() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:Wake"));
        assert!(!calls.iter().any(|c| c == "radio:Activate"));
    }

    #[tokio::test]
    async fn resume_sans_aucune_source_publie_letat_au_lieu_de_paniquer() {
        // Le premier appelant de la source active au démarrage, et donc le
        // premier à mourir : `active()` paniquait sur une table vide, et `resume`
        // tourne avant que le serveur web n'ait servi une seule page. Un
        // `panic!` là aurait supprimé la page de statut précisément quand on
        // voulait y voir les greffons figés.
        let (mut core, mut etat_rx, dir) = setup_sans_source();
        core.resume().await.unwrap();
        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(etat.source, "", "la chaine vide EST l'absence, c'est au rendu de la nommer");
        assert!(!etat.standby, "le coeur demarre, il n'entre pas en veille pour autant");
        drop(dir);
    }

    #[tokio::test]
    async fn stop_intentionnel_ne_declenche_pas_de_retry() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
    }

    #[tokio::test]
    async fn backoff_croissant_puis_reinitialise_par_un_titre() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        let d1 = relance(core.handle_event(Event::PlaybackIdle).await);
        let d2 = relance(core.handle_event(Event::PlaybackIdle).await);
        assert!(d2 > d1);
        // Un titre atteste la vivacité du flux : c'est aussi le verdict que la
        // boucle de `main` suit pour annuler l'échéance de relance.
        assert_eq!(core.handle_event(Event::Title("ok".into())).await, EventOutcome::StreamAlive);
        let d3 = relance(core.handle_event(Event::PlaybackIdle).await);
        assert_eq!(d3, d1);
    }

    #[tokio::test]
    async fn wake_noop_ne_declenche_pas_de_retry_cd_reste_silencieux() {
        // Regression (revue finale 2.2) : le cd repond Noop a Wake (pas de lecture
        // au boot/reveil). L'ancienne porte de retry (!stopped) laissait quand meme
        // planifier une relance sur le prochain PlaybackIdle, ce qui faisait
        // demarrer le cd tout seul ~2s apres. Avec expecting_stream, aucun Play
        // n'a ete emis => pas de retry.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: Arc::new(Mutex::new(Vec::new())) }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
    }

    #[tokio::test]
    async fn un_contenu_fini_n_arme_pas_la_relance_un_flux_live_si() {
        // Mesuré au banc mpv 0.37 : en fin de liste de fichiers, mpv passe
        // `idle` exactement comme lors d'une coupure de flux. Tant que le cœur
        // reniflait l'URI (`cdda://`), un chemin de fichier tombait du mauvais
        // côté — relance exponentielle au lieu de l'arrêt propre, et la liste
        // repartait en boucle depuis la première piste.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("/var/lib/ritornello/plugin-files.m3u").finite())
            .await
            .unwrap();
        assert!(!core.expecting_stream, "un contenu fini ne doit pas armer la relance");

        core.apply(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
        assert!(core.expecting_stream, "un flux live doit rester relançable");
    }

    #[tokio::test]
    async fn une_liste_se_charge_par_load_list_puis_se_positionne() {
        // Le défaut que ce test aurait dû attraper, et qu'il attrape désormais.
        //
        // Avec `loadfile`, mpv ne déplie un `.m3u` qu'**après** coup : mesuré
        // sur mpv 0.37, `playlist-count` vaut 1, puis 3 seulement après un
        // `end-file`/`start-file`. Le `playlist-pos` enchaîné arrivait donc hors
        // bornes, la lecture repartait de la première piste, et l'affichage
        // perdait présélection et titre. `loadlist` déplie sur-le-champ — sa
        // réponse porte même `num_entries` — ce qui rend cet enchaînement sûr.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(
            SourceAction::play("/var/lib/ritornello/plugin-files.m3u")
                .playlist()
                .starting_at(4)
                .finite(),
        )
        .await
        .unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec![
                "load_list /var/lib/ritornello/plugin-files.m3u".to_string(),
                "playlist-pos 4".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn un_media_reste_charge_par_loadfile() {
        // La distinction est déclarée par la Source, jamais devinée de l'URI :
        // un `.m3u8` est une liste pour un lecteur de fichiers et un flux HLS
        // pour une radio. Renifler l'extension casserait l'un des deux.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://icecast/fip.m3u8")).await.unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec!["play http://icecast/fip.m3u8".to_string()]
        );
    }

    #[tokio::test]
    async fn un_play_sans_index_ne_positionne_rien() {
        // Le chemin de la radio : aucune commande superflue sur la socket mpv.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://icecast/fip.mp3")).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["play http://icecast/fip.mp3".to_string()]);
    }

    #[tokio::test]
    async fn wake_play_declenche_bien_un_retry_apres_idle() {
        // Contraste avec le test precedent : quand Wake resulte en Play (radio),
        // un flux est bien attendu, donc un PlaybackIdle doit programmer un retry.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(matches!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::RetryIn(_)));
    }

    #[tokio::test]
    async fn la_fin_du_disque_ne_relance_pas_la_lecture_et_previent_la_source() {
        // Régression (revue 2026-07-27) : `Play cdda://` posait
        // `expecting_stream`, donc la fin du disque (mpv idle) déclenchait la
        // machinerie de relance des flux réseau : `Activate` → `Play cdda://`
        // → le disque repartait de la piste 1, indéfiniment.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
        let persisted = PersistedState { active_source: "cd".into(), ..PersistedState::default() };
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted, state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        // Unique source : SourceCycle re-active « cd », qui répond `Play cdda://`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        // Fin du disque : pas de relance, et la Source est prévenue — elle
        // seule peut recaler sa vue et son identité sur « plus rien ne joue ».
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, EventOutcome::Nothing);
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Stop"));
    }

    #[tokio::test]
    async fn une_avance_de_piste_du_lecteur_est_relayee_a_la_source() {
        // mpv rapporte l'avance, le cœur ne peut pas corriger une identité
        // opaque : il la fait corriger par la Source, seule à savoir ce que
        // « piste 2 » veut dire.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_event(Event::TrackChanged(2)).await;
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:PlayerTrack(2)"));
    }

    #[tokio::test]
    async fn une_avance_de_piste_en_veille_nest_pas_relayee() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_event(Event::TrackChanged(2)).await;
        assert!(source_calls.lock().unwrap().is_empty(), "rien ne doit partir en veille");
    }
}
