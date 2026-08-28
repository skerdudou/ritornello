//! Commandes de la telecommande et de l'IHM : machine lecture/veille, volume, dizaines, deplacement dans la piste, entree tenue, demarrage.

use super::*;

impl<P: Player> Core<P> {
    pub async fn handle_command(&mut self, cmd: Command) -> Result<()> {
        if self.standby && cmd != Command::Power {
            return Ok(());
        }
        let issue = self.appliquer_commande(cmd).await;
        // Publication à la sortie de **toute** commande, plutôt qu'un appel dans
        // chacune : volume, muet, veille et source active y changent, et une
        // branche oubliée laisserait l'IHM afficher un état périmé sans que rien
        // ne le signale. Le canal déduplique, donc publier pour rien ne coûte
        // aucune trame. Publié même en cas d'erreur : l'état partiel atteint est
        // ce que l'IHM doit montrer.
        self.publie_etat();
        issue
    }

    /// Volume absolu, la seule voie pour un réglage qui ne vient d'aucune touche :
    /// le `setvol` de MPD. Mêmes effets de bord que le pas relatif — mpv, disque,
    /// incrustation — parce qu'un volume changé depuis le réseau doit s'annoncer à
    /// l'écran comme celui changé depuis la télécommande.
    pub(super) async fn set_volume(&mut self, v: u8) -> Result<()> {
        self.volume = v.min(100);
        self.player.set_volume(self.volume).await?;
        self.persist();
        self.show_overlay().await;
        Ok(())
    }

    /// One volume step (±5), applied to mpv, persisted, shown as an overlay.
    /// Shared by fresh presses and held repeats; only the caller decides how
    /// to re-arm `volume_deadline`.
    pub(super) async fn step_volume(&mut self, up: bool) -> Result<()> {
        let v = self.volume as i16 + if up { 5 } else { -5 };
        self.set_volume(v.clamp(0, 100) as u8).await
    }

    /// Entry point for everything that used to call `handle_command`: fresh
    /// commands pass through unchanged; held (autorepeat) volume commands are
    /// paced by `volume_deadline`. Held on any other command is a no-op — the
    /// remote's autorepeat only means something for the volume.
    pub async fn handle_input(&mut self, msg: InputMessage) -> Result<()> {
        if !msg.held {
            return self.handle_command(msg.cmd).await;
        }
        if self.standby {
            return Ok(());
        }
        let up = match msg.cmd {
            Command::VolumeUp => true,
            Command::VolumeDown => false,
            _ => return Ok(()),
        };
        let Some(deadline) = self.volume_deadline else { return Ok(()) };
        if Instant::now() < deadline {
            return Ok(());
        }
        let issue = self.step_volume(up).await;
        self.volume_deadline =
            Some(Instant::now() + Duration::from_millis(self.settings.volume_repeat_interval_ms.into()));
        // Same publication contract as `handle_command`: the UI must see the
        // new volume even if mpv errored mid-way.
        self.publie_etat();
        issue
    }

    /// New settings from `PUT /api/settings` (via the `select!` loop of main).
    /// No bounds check here: the HTTP layer validates, and tests rely on tiny
    /// timings.
    pub fn set_settings(&mut self, s: crate::state::Settings) {
        // Poussé dans le cache de pochettes, qui est le seul autre porteur de
        // ces réglages. Ici et non dans un bras du `select!` : `set_settings`
        // est le point de passage **unique** de tout changement de réglages —
        // la route HTTP comme le chargement au démarrage —, donc le seul
        // endroit où la propagation ne peut pas être oubliée par un futur
        // appelant. Synchrone parce que `CoverCache` garde ces réglages sous un
        // verrou `std::sync` exprès pour ça.
        self.covers.set_reglages(crate::cover::Reglages::from(&s));
        self.settings = s;
        self.persist();
    }

    /// Startup: what the process does with the active source when it
    /// launches, per `settings.startup_power`. Called once by `main`, which
    /// treats a failure as best-effort — startup must never put systemd in a
    /// restart loop.
    ///
    /// The `On` branch persists before waking: `start_in_standby` writes
    /// `standby: true` on the other side, and without this the file would
    /// keep saying "in standby" after a boot that woke everything up — a
    /// later switch to `Previous` would then resurrect a standby the device
    /// left behind long ago.
    pub async fn demarrage(&mut self) -> Result<()> {
        let en_veille = match self.settings.startup_power {
            StartupPower::On => false,
            StartupPower::Standby => true,
            StartupPower::Previous => self.veille_persistee,
        };
        if en_veille {
            return self.start_in_standby().await;
        }
        // `resume` est aussi la moitie « reveil » de `Command::Power`, ou le
        // drapeau est deja baisse ; ici c'est cette methode qui le baisse,
        // pour que le fichier decrive un appareil reveille.
        self.standby = false;
        self.persist();
        self.resume().await
    }

    /// Startup in standby (`settings.startup_power`): mpv is configured
    /// (volume, audio device) so a later wake starts right, but the active
    /// source is not woken and the display shows the standby status.
    ///
    /// `standby_status` is not resolved here: it already is, since
    /// construction (see its doc) — no catalogue read on this path.
    pub async fn start_in_standby(&mut self) -> Result<()> {
        self.standby = true;
        // Written before touching mpv, like the standby half of
        // `Command::Power`: what is on disk must describe the device even if
        // the calls below fail.
        self.persist();
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        self.publie_etat();
        // A held key must re-press after standby: stale deadlines don't survive it.
        self.volume_deadline = None;
        Ok(())
    }

    pub(super) async fn appliquer_commande(&mut self, cmd: Command) -> Result<()> {
        // Any command other than Plus10/Select abandons a pending tens
        // sequence: pressing volume mid-sequence is a change of mind, not a
        // step of it. When an offset was actually armed, its `+NN` overlay
        // must go with it: `etat_lecteur` gives the overlay absolute
        // priority, and none of the arms below (PlayPause, Stop, Next, Prev,
        // Eject) rewrite it on their own, so without clearing it here the
        // display would keep showing an offset that no longer applies until
        // the deadline expires on its own. `handle_command`'s trailing
        // `publie_etat` picks up the clear — no need to publish here too.
        // The `!= 0` guard on `mem::take` keeps VolumeUp/Mute/Power
        // unaffected: they already overwrite or clear `self.overlay` right
        // after this.
        if !matches!(cmd, Command::Plus10 | Command::Select(_))
            && std::mem::take(&mut self.pending_tens) != 0
        {
            self.overlay = None;
        }
        match cmd {
            Command::Select(n) => {
                let tens = std::mem::take(&mut self.pending_tens);
                if tens != 0 {
                    // The consumed offset's overlay has said what it had to
                    // say; the source's own status takes over.
                    self.overlay = None;
                }
                let n = n.saturating_add(tens);
                if n == 0 {
                    // Key 0 with no pending offset: nothing to select.
                    return Ok(());
                }
                self.retry_count = 0;
                if let Some(action) = self.demande_active(SourceReq::Select(n)).await? {
                    self.apply(action).await?;
                }
            }
            // `Next`/`Prev` portent maintenant les deux sémantiques : la
            // source active décide (préselection pour la radio, piste pour
            // le cd — voir `SourcePlugin::next`/`prev` de chaque plugin).
            // Remettre `retry_count` à 0 ici est correct pour un changement
            // de préselection (nouveau flux radio, un retry sur l'ancien
            // n'aurait plus de sens) et inoffensif pour un changement de
            // piste cd (`retry_count` ne concerne que la relance d'un flux
            // réseau attendu, pas la lecture cd) : rien à distinguer entre
            // les deux sources sur ce point.
            Command::Next => {
                self.retry_count = 0;
                if let Some(action) = self.demande_active(SourceReq::Next).await? {
                    self.apply(action).await?;
                }
            }
            Command::Prev => {
                self.retry_count = 0;
                if let Some(action) = self.demande_active(SourceReq::Prev).await? {
                    self.apply(action).await?;
                }
            }
            Command::Eject => {
                if let Some(action) = self.demande_active(SourceReq::Eject).await? {
                    self.apply(action).await?;
                }
            }
            Command::VolumeUp | Command::VolumeDown => {
                self.step_volume(cmd == Command::VolumeUp).await?;
                self.volume_deadline = Some(
                    Instant::now() + Duration::from_millis(self.settings.volume_repeat_initial_ms.into()),
                );
            }
            Command::SetVolume(v) => {
                // Pas de `volume_deadline` a rearmer : ce n'est pas une touche,
                // rien ne peut etre maintenu.
                self.set_volume(v).await?;
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
                self.show_overlay().await;
            }
            Command::PlayPause => {
                if self.lecture {
                    // Basculer la croyance **après** que mpv a accepté, jamais
                    // avant. Le `?` propage un échec de `toggle_pause` et laisse
                    // `paused` intact : c'est cette valeur-là que
                    // `PlayerState.playback` publie, et à laquelle le greffon
                    // MPD compare ses `pause 0`/`pause 1` — un cœur qui se croit
                    // en pause devant un mpv qui joue fait répondre « paused » à
                    // un client dont la musique continue, et le `pause 0`
                    // suivant est alors jugé sans effet et ignoré.
                    self.player.toggle_pause().await?;
                    self.paused = !self.paused;
                } else {
                    // Rien n'est chargé : `stop` **vide la liste de mpv**, si
                    // bien que « basculer la pause » n'a plus rien à reprendre.
                    // La touche Lecture ne faisait donc rien du tout après un
                    // Stop, sur toutes les sources — mesuré sur la radio comme
                    // sur les fichiers. On redemande à la source active de
                    // jouer, ce qui est exactement ce que la touche promet.
                    //
                    // `lecture` et non `expecting_stream` : la première dit
                    // « quelque chose joue, de quelque nature », la seconde ne
                    // vaut que pour les flux relançables. Une pause, elle, ne
                    // touche ni l'une ni l'autre — la reprise reste donc un
                    // simple basculement, sans rechargement.
                    if let Some(action) = self.demande_active(SourceReq::Activate).await? {
                        self.apply(action).await?;
                    }
                }
            }
            Command::Stop => {
                self.expecting_stream = false;
                self.lecture = false;
                self.player.stop().await?;
                // Oublier l'identité **avant** de prévenir la Source : cet
                // appel efface le titre de l'afficheur, et une Source
                // injoignable ferait attendre jusqu'à 5 s (timeout de
                // `SourceClient::request`) avec le morceau arrêté encore à
                // l'écran.
                self.set_identity(None);
                // La Source n'a pas été consultée pour cet arrêt : le lui dire,
                // sinon celle qui tient un état de lecture propre (le cd) le
                // garderait faux et annoncerait plus tard des métadonnées pour
                // un morceau à l'arrêt. Au mieux : une Source muette n'empêche
                // rien.
                if let Err(e) = self.demande_active(SourceReq::Stop).await {
                    tracing::debug!("stop notification to source: {e}");
                }
            }
            Command::Power => {
                self.standby = !self.standby;
                // Persister **avant** de prevenir la Source, pour la meme
                // raison qu'au `SourceCycle` plus bas : une Source
                // injoignable fait attendre jusqu'a 5 s, et `StartupPower::
                // Previous` doit retrouver la veille voulue meme si le
                // courant est coupe pendant cette attente.
                self.persist();
                if self.standby {
                    let _ = self.demande_active(SourceReq::Deactivate).await;
                    self.player.stop().await?;
                    self.expecting_stream = false;
                    self.lecture = false;
                    // Même raison qu'au-dessus : la réponse de la Source à
                    // `Deactivate` est ignorée, et la vue de veille qui suit
                    // passerait outre le garde-fou de `handle_source_update`.
                    self.set_identity(None);
                    // Le compte de présélections et le statut n'ont de sens que
                    // pour la Source active : la veille les oublie tous les
                    // deux, et la prochaine Source (activate/wake) les
                    // redéclarera si elle en a. Sans cet effacement, le statut
                    // de l'ancienne source (« PAS DE DISQUE ») survivait à la
                    // veille en mémoire, prêt à réapparaître au réveil avant
                    // que la Source n'ait reparlé.
                    self.preset_count = None;
                    self.source_status = None;
                    // Même sort pour la capacité d'éjection : en veille aucune
                    // commande ne passe de toute façon (`handle_command`), et
                    // la Source la redéclarera au réveil.
                    self.can_eject = false;
                    // L'incrustation volume/muet ne survit pas à la mise en
                    // veille : elle garde la priorité dans `etat_lecteur`, et
                    // « VOLUME 65 % » restait à l'écran jusqu'à 2 s après
                    // l'extinction avant que le mot de veille n'apparaisse.
                    self.overlay = None;
                    // `standby_status` n'est pas résolu ici : il l'est déjà,
                    // depuis la construction et chaque `set_locale` (voir sa
                    // doc) — plus jamais au moment de poser la veille.
                    // A held key must re-press after standby: stale deadlines don't survive it.
                    self.volume_deadline = None;
                } else {
                    self.resume().await?;
                }
            }
            Command::SourceCycle => {
                // La source active peut ne plus être dans l'ordre : c'est l'état
                // que laisse `oublie_source_morte` — le greffon a disparu, la
                // musique continue, et son nom reste affiché. Dans ce cas, la
                // touche Source doit repartir de la **première** source
                // disponible. Un `position().unwrap_or(0)` suivi du `+ 1`
                // sautait la première pour aller à la seconde, ce qui rendait
                // une source inatteignable au clavier tant qu'on n'avait pas
                // fait un tour complet.
                let suivante = match self.source_order.iter().position(|n| n == &self.active_source) {
                    Some(idx) => self.source_order.get((idx + 1) % self.source_order.len()).cloned(),
                    None => self.source_order.first().cloned(),
                };
                self.bascule_source(suivante).await?;
            }
            Command::SelectSource(nom) => {
                // Inconnue : ignorée en silence, comme une touche non liée. Le
                // greffon MPD a déjà répondu `ACK 50` de son côté — il ne
                // propose que des noms reçus du catalogue, donc arriver ici
                // veut dire que la source a disparu entre-temps (un greffon
                // qu'on vient d'éteindre depuis l'IHM, par exemple).
                if !self.source_order.iter().any(|n| n == &nom) {
                    tracing::debug!("unknown source {nom} ignored");
                    return Ok(());
                }
                // Déjà active : ne rien faire. Un `load` redondant ne doit pas
                // couper ce qui joue, et c'est exactement ce qu'un client
                // envoie en rouvrant son écran.
                if nom != self.active_source {
                    // `Some(nom)` : `bascule_source` admet `None` — « plus
                    // aucune source » — mais ce chemin-ci désigne toujours un
                    // nom, le garde ci-dessus l'ayant vérifié dans l'ordre.
                    self.bascule_source(Some(nom)).await?;
                }
            }
            Command::Plus10 => {
                let next = self.pending_tens.saturating_add(10);
                self.pending_tens = match self.preset_count {
                    // Wrap past the last useful decade: the largest
                    // reachable multiple of 10 is (count / 10) * 10
                    // (station 20 is +10 +10 then 0, so offset 20 must
                    // stay allowed for a count of 20).
                    Some(count) if next > (count / 10) * 10 => 0,
                    // No known count: saturate, don't wrap — we can't know
                    // where the end is.
                    None => next.min(240),
                    _ => next,
                };
                if self.pending_tens == 0 {
                    self.overlay = None;
                } else {
                    self.show_tens_overlay().await;
                }
            }
            Command::SeekForward | Command::SeekBackward => {
                // Ignorée en silence sur un contenu non parcourable : la
                // touche se comporte comme une touche non liée, ce que la
                // télécommande sait déjà faire. Un message n'apprendrait rien
                // à qui vient d'appuyer.
                if self.lecture && !self.expecting_stream {
                    let pas = i64::from(self.settings.seek_step_s);
                    let delta = if cmd == Command::SeekForward { pas } else { -pas };
                    self.player.seek_relative(delta).await?;
                    self.rafraichit_position().await;
                }
            }
            Command::SeekTo(position_s) => {
                if self.lecture && !self.expecting_stream {
                    self.player.seek_absolute(position_s).await?;
                    self.rafraichit_position().await;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

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
    async fn les_commandes_sans_aucune_source_ne_font_rien_et_ne_paniquent_pas() {
        // Les treize requêtes à la source active passaient par le même
        // `panic!` : la moindre touche de télécommande sur un appareil sans
        // source arrêtait le cœur. Sans source, une commande **ne fait rien**,
        // et le journal le dit en `debug` — ce n'est pas une anomalie.
        let (mut core, _rx, dir) = setup_sans_source();
        for cmd in [
            Command::Select(1),
            Command::Next,
            Command::Prev,
            Command::Eject,
            Command::Stop,
            Command::PlayPause,
            Command::SourceCycle,
            // Veille, puis réveil : le second repasse par `resume`.
            Command::Power,
            Command::Power,
        ] {
            let libelle = format!("{cmd:?}");
            core.handle_command(cmd).await.unwrap_or_else(|e| panic!("{libelle}: {e}"));
        }
        // Les deux événements du lecteur qui notifient la source, et la relance
        // de flux : mêmes appels, même table vide.
        core.handle_event(Event::TrackChanged(2)).await;
        core.handle_event(Event::PlaybackIdle).await;
        core.retry_stream().await.unwrap();
        assert_eq!(core.active_source(), "", "aucune commande n'a pu designer une source");
        drop(dir);
    }

    #[tokio::test]
    async fn la_touche_lecture_relance_quand_rien_ne_joue() {
        // Défaut signalé, et il touchait **toutes** les sources : `stop` vide la
        // liste de mpv, donc « basculer la pause » n'avait plus rien à reprendre
        // et la touche Lecture ne faisait rien du tout. Mesuré sur la radio comme
        // sur les fichiers avant correction.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://fip")).await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        player_calls.lock().unwrap().clear();

        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(
            *player_calls.lock().unwrap(),
            vec!["play http://fip".to_string()],
            "la source active doit etre redemandee, pas une pause dans le vide"
        );
    }

    #[tokio::test]
    async fn la_touche_lecture_bascule_la_pause_quand_ca_joue() {
        // Garde-fou du test précédent : une pause doit rester une pause, et non
        // devenir un rechargement qui repartirait du début de la piste.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.apply(SourceAction::play("http://fip")).await.unwrap();
        player_calls.lock().unwrap().clear();

        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["pause".to_string()]);
        // Et une deuxième fois : mettre en pause ne fait pas « cesser de
        // jouer », donc la reprise reste un simple basculement.
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(*player_calls.lock().unwrap(), vec!["pause".to_string(), "pause".to_string()]);
    }

    #[tokio::test]
    async fn la_pause_et_la_reprise_se_lisent_dans_letat_publie() {
        // Le champ le plus lu de la commande `status` de MPD : sans lui, aucun
        // client ne peut afficher le bon bouton.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Stopped);
    }

    #[tokio::test]
    async fn un_echec_de_mpv_ne_change_pas_la_croyance_du_coeur_sur_la_pause() {
        // `paused` était basculé **avant** `toggle_pause`, donc un échec de mpv
        // laissait le cœur croire l'inverse de la vérité. Ce n'est pas cosmétique
        // : c'est cette valeur que `PlayerState.playback` publie, et à laquelle le
        // greffon MPD compare ses `pause 0`/`pause 1`. Un cœur qui se croit en
        // pause devant un mpv qui joue répond « paused » à un client dont la
        // musique continue, puis juge le `pause 0` suivant sans effet.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);

        core.player.pause_echoue.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            core.handle_command(Command::PlayPause).await.is_err(),
            "l'echec de mpv doit remonter, il ne doit pas etre avale"
        );
        assert_eq!(
            core.etat_lecteur().playback,
            Playback::Playing,
            "mpv a refuse : le coeur doit continuer de dire ce qui est vrai"
        );

        // Et la reprise du dialogue remet la bascule en marche : le drapeau n'a
        // pas ete abime, il n'a simplement pas bouge.
        core.player.pause_echoue.store(false, std::sync::atomic::Ordering::SeqCst);
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
    }

    #[tokio::test]
    async fn une_pause_ne_survit_pas_a_un_nouveau_play() {
        // Le seul effacement de `paused` est celui du `Play` applique : si on
        // l'oubliait, une pause d'hier rendrait une lecture neuve « en pause ».
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture (radio, http://fip)
        core.handle_command(Command::PlayPause).await.unwrap(); // met en pause
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
        // Selectionne une autre preselection radio (`http://inter`) : un nouveau
        // `Play` est applique, et `paused` doit retomber par ce seul chemin.
        core.handle_command(Command::Select(3)).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
    }

    #[tokio::test]
    async fn la_veille_dit_larret_meme_si_la_pause_etait_posee() {
        // Ce test isole seulement l'oubli du drapeau `paused` : `Command::Power`
        // pose `standby = true` et `lecture = false` dans le meme pas, donc il
        // ne peut pas distinguer laquelle des deux conditions fait le travail.
        // Ce qu'il prouve : un `paused` pose plus tot ne doit pas fuiter dans
        // l'etat rapporte pendant la veille.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture
        core.handle_command(Command::PlayPause).await.unwrap(); // met en pause
        assert_eq!(core.etat_lecteur().playback, Playback::Paused);
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Stopped);
    }

    #[tokio::test]
    async fn le_volume_absolu_remplace_le_volume_et_le_borne() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SetVolume(40)).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 40);
        core.handle_command(Command::SetVolume(200)).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 100, "borne haute");
        core.handle_command(Command::SetVolume(0)).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 0);
    }

    #[tokio::test]
    async fn le_volume_absolu_ecrit_une_incrustation_comme_le_pas_relatif() {
        // Un volume change depuis le reseau doit s'annoncer a l'ecran comme celui
        // change depuis la telecommande.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SetVolume(40)).await.unwrap();
        assert!(core.etat_lecteur().overlay.is_some());
    }

    #[tokio::test]
    async fn volume_maintenu_est_ignore_avant_le_delai_initial() {
        // Échéance **pilotée**, jamais attendue. La version precedente reposait
        // sur le fait que deux lignes consecutives s'executent en moins des
        // 30 ms du delai initial : sous charge -- un `cargo test --workspace`
        // qui compile encore pendant qu'il teste -- l'ordonnancement depassait
        // cette marge et le test tombait, une fois sur quelques dizaines. Ce
        // qu'il verifie ne depend plus de la vitesse de la machine.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 60 -> 65, arme l'echeance
        // Repoussee loin : la repetition n'a aucune raison d'avoir lieu, quelle
        // que soit la lenteur de ce qui precede.
        core.volume_deadline = Some(Instant::now() + Duration::from_secs(60));
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65, "une repetition avant le delai initial ne fait rien");
    }

    #[tokio::test]
    async fn volume_maintenu_repete_apres_le_delai_puis_a_lintervalle() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65

        // Échéance atteinte : la premiere repetition passe. `Instant::now()` est
        // deja dans le passe quand `handle_input` le relit -- le temps ne
        // recule pas, donc ce declenchement est certain.
        let posee = Instant::now();
        core.volume_deadline = Some(posee);
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70, "premiere repetition apres le delai initial");

        // Elle a rearme l'echeance pour l'intervalle suivant. Compare a celle
        // qu'on avait **posee**, et non a `Instant::now()` : la nouvelle vaut
        // « instant de la repetition + intervalle », donc elle est posterieure a
        // `posee` quoi qu'il arrive. La comparer au present reintroduirait la
        // course que ce test existe pour supprimer.
        let rearmee = core.volume_deadline.expect("l'intervalle doit etre rearme");
        assert!(rearmee > posee, "l'echeance n'a pas ete rearmee apres la repetition");

        // Une echeance dans le futur bloque la repetition suivante.
        core.volume_deadline = Some(Instant::now() + Duration::from_secs(60));
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70);

        // Intervalle ecoule : une repetition de plus, et une seule.
        core.volume_deadline = Some(Instant::now());
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 75, "puis une par intervalle");
    }

    #[tokio::test]
    async fn volume_maintenu_sans_pression_initiale_ne_fait_rien() {
        // A held event with no prior press (core restarted mid-hold): no
        // deadline is armed, nothing moves.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_input(InputMessage { cmd: Command::VolumeDown, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 60);
    }

    #[tokio::test]
    async fn held_sur_une_commande_non_volume_est_ignore() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_input(InputMessage { cmd: Command::Next, held: true }).await.unwrap();
        assert!(source_calls.lock().unwrap().is_empty(), "un Next maintenu ne doit pas atteindre la source");
    }

    #[tokio::test]
    async fn volume_maintenu_est_bloque_en_veille() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65, arms the deadline
        core.handle_command(Command::Power).await.unwrap();    // standby
        // Aucun sommeil : la veille court-circuite `handle_input` **avant** de
        // regarder l'echeance, donc attendre qu'elle expire ne prouvait rien.
        // L'echeance est posee au passe pour que le test echoue si ce
        // court-circuit disparaissait.
        core.volume_deadline = Some(Instant::now());
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65);
    }

    #[tokio::test]
    async fn handle_input_non_held_equivaut_a_handle_command() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_input(InputMessage::from(Command::Select(3))).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn plus10_saffiche_et_repousse_son_echeance() {
        // Chaque appui montre le cumul (+10, +20) dans l'incrustation, avec la
        // même échéance que le volume.
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(core.overlay_deadline().is_some());
        match etat_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Tens { offset, text, .. }) => {
                assert_eq!(offset, 10);
                assert_eq!(text, "PRESET +10");
            }
            autre => panic!("attendu une incrustation Tens, obtenu {autre:?}"),
        };
        core.handle_command(Command::Plus10).await.unwrap();
        match etat_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Tens { offset, text, .. }) => {
                assert_eq!(offset, 20);
                assert_eq!(text, "PRESET +20");
            }
            autre => panic!("attendu une incrustation Tens, obtenu {autre:?}"),
        };
    }

    #[tokio::test]
    async fn le_decalage_est_consomme_par_la_touche_chiffre() {
        // +10 puis 4 = présélection 14 ; le décalage ne survit pas à sa
        // consommation.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_avec_compte(Some(23)));
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Select(4)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(14)")));
        core.handle_command(Command::Select(4)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(4)")));
    }

    #[tokio::test]
    async fn la_touche_zero_seule_ne_fait_rien() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Select(0)).await.unwrap();
        assert!(!source_calls.lock().unwrap().iter().any(|c| c.contains("Select(0)")));
    }

    #[tokio::test]
    async fn zero_atteint_les_multiples_de_dix() {
        // 20 stations : +10 +10 puis 0 = 20 — le décalage 20 doit rester permis.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_avec_compte(Some(20)));
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::Select(0)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(20)")));
    }

    #[tokio::test]
    async fn plus10_reboucle_apres_la_derniere_dizaine() {
        // 23 stations : décalages utiles 10 et 20 ; le troisième appui revient
        // à zéro et éteint l'incrustation, comme la fenêtre web.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", update_avec_compte(Some(23)));
        for _ in 0..3 {
            core.handle_command(Command::Plus10).await.unwrap();
        }
        assert!(core.overlay_deadline().is_none());
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn une_autre_commande_abandonne_le_decalage() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn abandonner_le_decalage_efface_aussi_son_incrustation() {
        // `VolumeUp` masque le défaut (il écrit son propre overlay juste
        // après) : `PlayPause` n'écrit aucun overlay, donc rien ne doit
        // effacer le `+NN` à sa place si ce n'est le garde d'abandon
        // lui-même. Sans le correctif, l'incrustation restait à l'écran
        // jusqu'à son échéance alors que le décalage était déjà abandonné.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(core.overlay_deadline().is_some(), "l'incrustation +10 doit être affichée");
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(
            core.overlay_deadline().is_none(),
            "l'incrustation +NN doit disparaître avec le décalage abandonné"
        );
    }

    #[tokio::test]
    async fn lecheance_de_lincrustation_oublie_le_decalage() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        core.expire_overlay();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn sans_compte_connu_le_decalage_sature_sans_reboucler() {
        // Pas de compte déclaré : on ne sait pas où est la fin, donc pas de
        // rebouclage — saturation à 240.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        for _ in 0..30 {
            core.handle_command(Command::Plus10).await.unwrap();
        }
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(243)")));
    }

    #[tokio::test]
    async fn set_settings_persiste() {
        let (mut core, _pc, _sc, _rx, dir) = setup();
        core.set_settings(crate::state::Settings {
            volume_repeat_initial_ms: 800,
            volume_repeat_interval_ms: 250,
            startup_power: StartupPower::Previous,
            ..Default::default()
        });
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.settings.volume_repeat_initial_ms, 800);
        assert_eq!(st.settings.startup_power, StartupPower::Previous);
    }

    #[tokio::test]
    async fn les_touches_de_deplacement_agissent_sur_un_contenu_fini() {
        let (mut core, calls, _, _, _dir) = setup();
        // Contenu fini : bascule de `radio` (source active par défaut) vers `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekBackward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        let journal = calls.lock().unwrap().clone();
        assert!(journal.contains(&"seek_relative 10".to_string()), "{journal:?}");
        assert!(journal.contains(&"seek_relative -10".to_string()), "{journal:?}");
        assert!(journal.contains(&"seek_absolute 198".to_string()), "{journal:?}");
    }

    /// Sur un direct, la touche ne fait rien — comme une touche non liée. Pas
    /// de message, pas de trame : le contenu n'est pas parcourable, et le dire
    /// n'apprendrait rien à qui vient d'appuyer.
    #[tokio::test]
    async fn les_touches_de_deplacement_sont_ignorees_sur_un_flux() {
        let (mut core, calls, _, _, _dir) = setup();
        // Flux : `radio` est déjà la source active, `PlayPause` la fait jouer.
        core.handle_command(Command::PlayPause).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        assert!(
            calls.lock().unwrap().iter().all(|c| !c.starts_with("seek_")),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn le_pas_de_deplacement_suit_le_reglage() {
        let (mut core, calls, _, _, _dir) = setup();
        // `set_settings` existe déjà (elle sert la route `PUT /api/settings`).
        core.set_settings(crate::state::Settings { seek_step_s: 30, ..Default::default() });
        // Contenu fini : bascule de `radio` vers `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"seek_relative 30".to_string()));
    }

    #[tokio::test]
    async fn un_message_ephemere_desarme_un_decalage_en_cours() {
        // Le message éphémère d'une source (« présélection vide ») emprunte
        // le même emplacement d'overlay que le cumul +NN et le lui vole :
        // sans désarmer le décalage ici, l'appui suivant sur un chiffre
        // composerait encore l'ancien décalage alors que l'écran ne montre
        // plus +NN mais le message de la source.
        let (mut core, _pc, source_calls, mut etat_rx, _d) = setup();
        core.handle_command(Command::Plus10).await.unwrap();
        assert!(matches!(etat_rx.borrow_and_update().overlay, Some(Overlay::Tens { .. })));

        let mut ephemere = update_nu();
        ephemere.transient = true;
        ephemere.status = Some("empty preset".into());
        core.handle_source_update("radio", ephemere);

        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(
            source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")),
            "sans décalage armé, Select(3) doit demander la présélection 3"
        );
        assert!(
            !source_calls.lock().unwrap().iter().any(|c| c.contains("Select(13)")),
            "le décalage abandonné par le message éphémère ne doit pas être appliqué"
        );
    }

    #[tokio::test]
    async fn demarrage_en_veille_applique_le_volume_sans_reveiller_la_source() {
        let (mut core, player_calls, source_calls, mut etat_rx, _d) = setup();
        core.start_in_standby().await.unwrap();
        // mpv is configured (volume applied) so waking later starts right...
        // (FakePlayer::set_volume records "vol {v}", see FakePlayer above.)
        assert!(player_calls.lock().unwrap().iter().any(|c| c.starts_with("vol ")));
        // ...but the source was NOT woken, and the display shows standby.
        assert!(!source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")), "pas de Wake en veille");
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));
        assert!(core.etat_lecteur().standby);
        // Power then wakes normally.
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.etat_lecteur().standby);
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")));
    }

    /// Les trois valeurs de `startup_power`, sur le seul critere observable :
    /// la source est-elle reveillee ? `Previous` est teste dans ses deux sens,
    /// sinon un `Previous` traite comme `On` passerait la moitie du test.
    #[tokio::test]
    async fn le_demarrage_suit_le_reglage_de_mise_sous_tension() {
        async fn reveille(startup_power: StartupPower, veille_persistee: bool) -> bool {
            let persisted = PersistedState {
                standby: veille_persistee,
                settings: crate::state::Settings { startup_power, ..Default::default() },
                ..Default::default()
            };
            let (mut core, _pc, source_calls, _rx, _d) = setup_persiste(persisted);
            core.demarrage().await.unwrap();
            // Le verrou est relache par cette liaison, pas garde jusqu'a la
            // fin du bloc : sinon `source_calls` est libere avant lui.
            let a_reveille = source_calls.lock().unwrap().iter().any(|c| c.contains("Wake"));
            a_reveille
        }

        assert!(reveille(StartupPower::On, true).await, "« allume » ignore la veille sur disque");
        assert!(!reveille(StartupPower::Standby, false).await, "« veille » ne reveille jamais");
        assert!(reveille(StartupPower::Previous, false).await, "etait allume : on rallume");
        assert!(!reveille(StartupPower::Previous, true).await, "etait en veille : on y reste");
    }

    #[tokio::test]
    async fn larret_est_notifie_a_la_source_active() {
        // `Command::Stop` est la seule commande qui change l'état de lecture
        // sans consulter la Source : sans cette notification, une Source qui
        // tient un état de lecture propre (le cd) le garderait faux.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Stop"));
    }
}
