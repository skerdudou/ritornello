//! Incrustations (volume, muet, dizaines) et deadlines : ce que la boucle de main.rs doit reveiller, et quand.

use super::*;

impl<P: Player> Core<P> {
    /// Affiche (ou prolonge) l'overlay temporaire volume/muet : line 1 le
    /// libellé "volume", line 2 le pourcentage courant ou le message
    /// "muet" selon `self.muted`. Chaque appel repousse l'échéance de
    /// `overlay_ms` (une pression de plus garde l'overlay visible).
    ///
    /// `overlay_ms`, distinct de `tens_window_ms` (voir le commentaire de
    /// `Settings`) : cette incrustation masque la vue « en écoute » et
    /// pourrait vouloir raccourcir un jour, sans affecter le temps laissé
    /// pour composer un `+NN`. `expire_overlay` n'a pas besoin de savoir
    /// laquelle des deux durées a posé l'échéance qu'il désarme : elle est
    /// stockée avec le message, dans `self.overlay`.
    pub(super) async fn show_overlay(&mut self) {
        let mot = if self.muted {
            let cat = self.catalog.read().await;
            cat.get("muted").to_string()
        } else {
            format!("{} %", self.volume)
        };
        let label = self.catalog.read().await.get("volume_label").to_string();
        let deadline = Instant::now() + Duration::from_millis(self.settings.overlay_ms.into());
        self.overlay = Some((
            Overlay::Volume {
                level: self.volume,
                muted: self.muted,
                text: format!("{label} {mot}"),
                remaining_ms: self.settings.overlay_ms,
            },
            deadline,
        ));
    }

    /// Overlay for the pending tens offset ("+10", "+20"): same slot as the
    /// volume overlay, but its own deadline from `tens_window_ms` — the
    /// time left to press the second digit, independent from
    /// `overlay_ms` (see `Settings`). Each press pushes the deadline back,
    /// and `expire_overlay` clears the overlay and the offset together
    /// regardless of which duration is stored here: it reads whatever
    /// deadline is in `self.overlay`, never which field produced it, so
    /// the two stay aligned by construction whatever values the two
    /// settings take.
    pub(super) async fn show_tens_overlay(&mut self) {
        let label = self.catalog.read().await.get("preset_label").to_string();
        let deadline = Instant::now() + Duration::from_millis(self.settings.tens_window_ms.into());
        self.overlay = Some((
            Overlay::Tens {
                offset: self.pending_tens,
                text: format!("{label} +{}", self.pending_tens),
                remaining_ms: self.settings.tens_window_ms,
            },
            deadline,
        ));
    }

    /// Échéance de l'overlay active, s'il y en a un (à read dans `main` avant
    /// le `select!`, à l'image de `retry_at`, pour bâtir la temporisation).
    pub fn overlay_deadline(&self) -> Option<Instant> {
        self.overlay.as_ref().map(|(_, deadline)| *deadline)
    }

    /// Le cœur veut-il être rappelé dans une seconde pour rafraîchir la
    /// position ?
    ///
    /// Armé seulement quand il y a effectivement une position à publier : la
    /// playback en cours, hors veille, ET (un contenu fini — donc mpv a la
    /// parole sur sa position — OU une ancre posée par un plugin `metadata`).
    /// `!self.standby && self.playback` seul armait à tort dans deux cas
    /// trouvés en relecture : un stream qu'aucun plugin `metadata` ne suit (rien
    /// ne fournira jamais de position, l'ancre ne se pose jamais) et la pause
    /// (qui ne remet pas `playback` à faux). Aucune trame n'en ressortait —
    /// `publish_state` déduplique — mais l'appareil interrogeait mpv deux fois
    /// par seconde indéfiniment, pour rien à afficher.
    pub fn tick_position(&self) -> bool {
        !self.standby && self.playback && (!self.expecting_stream || self.position_anchor.is_some())
    }

    /// Efface l'overlay expiré et laisse réapparaître l'état permanent
    /// (source, présélection, statut, track), tenu à jour entre-temps par
    /// les autres chemins du cœur.
    ///
    /// Seul appelant : la boucle de `main`, sans aucune autre publication
    /// après — contrairement aux commands, qui publient elles-mêmes à la
    /// sortie de `handle_command`. Un oubli ici ne casse rien à la
    /// compilation, mais l'écran cesse de se mettre à jour à l'expiration.
    pub fn expire_overlay(&mut self) {
        self.overlay = None;
        self.pending_tens = 0;
        self.publish_state();
    }
}

/// Prochaine échéance du tick de position, à partir de l'état d'armement et
/// de l'échéance courante.
///
/// Fonction pure, et c'est tout son intérêt : la boucle `select!` de `main`
/// n'est couverte par aucun test, et le défaut que cette logique corrige — une
/// échéance **relative**, recréée à chaque tour, qui repartait de zéro à chaque
/// réveil de la boucle et repoussait le tick indéfiniment sur un appareil
/// active — ne se voit pas en lisant le code appelant.
///
/// `arme` = le cœur veut être rappelé ; `courante` = l'échéance déjà posée,
/// s'il y en a une ; `maintenant` = l'instant de référence, injecté pour que
/// le test n'ait pas d'horloge à attendre.
pub fn next_deadline(arme: bool, courante: Option<Instant>, maintenant: Instant) -> Option<Instant> {
    match (arme, courante) {
        (false, _) => None,
        (true, Some(at)) => Some(at),
        (true, None) => Some(maintenant + Duration::from_secs(1)),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    pub(super) async fn volume_up_affiche_temporairement_le_volume() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        state_rx.borrow_and_update();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let e = state_rx.borrow_and_update().clone();
        // PersistedState::default().volume == 60, VolumeUp += 5.
        assert_eq!(e.volume, 65);
        match e.overlay {
            Some(Overlay::Volume { level, muted, text, .. }) => {
                assert_eq!(level, 65);
                assert!(!muted);
                assert_eq!(text, "VOLUME 65 %");
            }
            autre => panic!("attendu une incrustation Volume, obtenu {autre:?}"),
        }
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    pub(super) async fn mute_affiche_loverlay_muet() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        state_rx.borrow_and_update();
        core.handle_command(Command::Mute).await.unwrap();
        match state_rx.borrow_and_update().overlay.clone() {
            Some(Overlay::Volume { muted, text, .. }) => {
                assert!(muted);
                assert_eq!(text, "VOLUME MUTED");
            }
            autre => panic!("attendu une incrustation Volume, obtenu {autre:?}"),
        }
        assert!(core.overlay_deadline().is_some());
    }

    #[tokio::test]
    pub(super) async fn une_mise_a_jour_source_pendant_loverlay_ne_le_remplace_pas_et_reapparait_a_expiration() {
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let avec_overlay = state_rx.borrow_and_update().clone();
        assert!(matches!(avec_overlay.overlay, Some(Overlay::Volume { .. })));

        // La mise a jour source arrive pendant l'overlay : elle est memorisee
        // (le name de présélection change) mais l'overlay reste affiche.
        let mut update = bare_update();
        update.preset_name = Some("FIP".into());
        core.handle_source_update("radio", update);
        let pendant = state_rx.borrow().clone();
        assert!(matches!(pendant.overlay, Some(Overlay::Volume { .. })), "l'overlay reste affiche");
        assert_eq!(pendant.preset_name.as_deref(), Some("FIP"), "mais l'state sous-jacent est deja a jour");

        // A l'expiration, l'overlay disparait et la mise a jour memorisee est visible.
        core.expire_overlay();
        let apres = state_rx.borrow_and_update().clone();
        assert!(apres.overlay.is_none());
        assert_eq!(apres.preset_name.as_deref(), Some("FIP"));
        assert!(core.overlay_deadline().is_none());
    }

    #[test]
    pub(super) fn overlay_deadline_est_none_sans_overlay_actif() {
        let (core, _pc, _sc, _rx, _d) = setup();
        assert!(core.overlay_deadline().is_none());
    }

    #[tokio::test]
    pub(super) async fn une_nouvelle_pression_repousse_lecheance_de_lloverlay() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let d1 = core.overlay_deadline().unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let d2 = core.overlay_deadline().unwrap();
        // Strictement supérieur : `>=` passerait aussi avec une échéance
        // jamais repoussée (`d2 == d1`), soit exactement le défaut que ce
        // test prétend attraper. Deux `Instant::now()` successifs sont
        // toujours distincts sur les horloges monotones visées.
        assert!(d2 > d1);
    }

    #[tokio::test]
    pub(super) async fn la_mise_en_veille_efface_lincrustation_volume() {
        // Régression (revue 2026-07-27) : l'incrustation garde la priorité
        // dans `player_state`, donc « VOLUME 65 % » restait affiché jusqu'à 2 s
        // après l'extinction avant que le mot de veille n'apparaisse.
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        assert!(matches!(state_rx.borrow_and_update().overlay, Some(Overlay::Volume { .. })));
        core.handle_command(Command::Power).await.unwrap();
        let veille = state_rx.borrow_and_update().clone();
        assert!(veille.overlay.is_none());
        assert_eq!(veille.status.as_deref(), Some("STANDBY"));
        assert!(core.overlay_deadline().is_none());
    }

    #[tokio::test]
    pub(super) async fn le_tick_ne_s_arme_pas_quand_rien_ne_joue() {
        let (mut core, _, _, _, _dir) = setup();
        assert!(!core.tick_position(), "rien ne plays : rien à rafraîchir");
        // Bascule vers `cd`, contenu fini : mpv a la parole sur sa position,
        // le tick a donc quelque chose à publier.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(core.tick_position(), "contenu fini en cours de playback : on suit sa position");
        core.handle_command(Command::Stop).await.unwrap();
        assert!(!core.tick_position());
    }

    /// Cas trouvé en relecture : `radio` n'est pas un contenu fini (mpv ne
    /// fournit pas sa position) et aucun plugin `metadata` n'a posé d'ancre —
    /// personne ne suit ce stream, il n'y a rien à publier. Sans ce garde,
    /// l'appareil interrogerait mpv deux fois par seconde indéfiniment pour
    /// une trame que la déduplication absorbe systématiquement.
    #[tokio::test]
    pub(super) async fn un_flux_sans_ancre_narme_pas_le_tick() {
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(!core.tick_position(), "stream sans ancre : rien a publier");
    }

    #[tokio::test]
    pub(super) async fn le_tick_ne_s_arme_pas_en_veille() {
        let (mut core, _, _, _, _dir) = setup();
        // Bascule vers `cd`, contenu fini : le tick a une position à publier.
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(core.tick_position());
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.tick_position(), "l'appareil dort");
        // Le garde `!standby` est défensif : aucun path atteignable ne pose
        // aujourd'hui la veille en laissant `playback` vrai (`Command::Power`
        // remet les deux). On construit donc l'état à la main, sans quoi ce
        // test passerait à l'identique si le garde disparaissait. `expecting_stream`
        // reste `false` (contenu fini) pour isoler précisément le garde de veille.
        core.playback = true;
        core.standby = true;
        assert!(!core.tick_position(), "la veille l'emporte, même si la playback n'a pas été remise à zéro");
    }

    /// L'échéance déjà posée **survit** aux tours de boucle : c'est tout
    /// l'objet du correctif. Une échéance relative recréée à chaque réveil du
    /// `select!` — commande, événement mpv, enrichment — repartait de zéro,
    /// et le tick n'arrivait jamais sur un appareil active.
    #[test]
    pub(super) fn une_echeance_posee_ne_se_deplace_pas_aux_tours_suivants() {
        let t0 = Instant::now();
        let posee = next_deadline(true, None, t0).unwrap();
        assert_eq!(posee, t0 + Duration::from_secs(1));
        // Trois tours de boucle plus tard, sur un appareil très occupé :
        for retard in [10, 200, 900] {
            let plus_tard = t0 + Duration::from_millis(retard);
            assert_eq!(
                next_deadline(true, Some(posee), plus_tard),
                Some(posee),
                "l'échéance a glissé de {retard} ms"
            );
        }
    }

    #[test]
    pub(super) fn desarme_l_echeance_est_oubliee() {
        let t0 = Instant::now();
        assert_eq!(next_deadline(false, Some(t0), t0), None);
        assert_eq!(next_deadline(false, None, t0), None);
    }

    /// La règle qui protège les messages éphémères : le tick republie l'état
    /// **avec** l'incrustation en cours, intacte, et sans toucher à son
    /// échéance. C'est l'afficheur qui décide de la mettre par-dessus ou à
    /// côté ; le cœur reste seul maître du moment où elle disparaît.
    #[tokio::test]
    pub(super) async fn un_rafraichissement_de_position_laisse_l_incrustation_intacte() {
        let (mut core, _, _, _, _dir) = setup();
        // Un contenu **fini** : c'est le seul cas où mpv fournit une position,
        // donc le seul où le rafraîchissement a quelque chose à publier.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let echeance_avant = core.overlay_deadline();
        assert!(core.player_state().overlay.is_some(), "l'incrustation volume est là");
        core.set_progress(Some(30.0), Some(254.0));
        core.refresh_position().await;
        assert!(core.player_state().overlay.is_some(), "et elle y reste");
        assert_eq!(core.overlay_deadline(), echeance_avant, "son échéance n'a pas bougé");
        assert_eq!(core.player_state().position_s, Some(30));
    }

    #[tokio::test]
    pub(super) async fn un_enrichissement_pendant_loverlay_ne_le_remplace_pas() {
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_command(Command::VolumeUp).await.unwrap();
        let avec_overlay = state_rx.borrow_and_update().clone();
        assert!(matches!(avec_overlay.overlay, Some(Overlay::Volume { .. })));

        core.handle_enrichment("ouifm", enrichment(id, "Miles Davis", "So What"));
        let pendant = state_rx.borrow().clone();
        assert!(matches!(pendant.overlay, Some(Overlay::Volume { .. })), "l'overlay volume reste affiche");
        assert_eq!(pendant.track.title.as_deref(), Some("So What"), "mais le track est deja a jour dessous");
        // ... et le titre reste disponible dès l'expiration.
        core.expire_overlay();
        assert_eq!(state_rx.borrow_and_update().track.title.as_deref(), Some("So What"));
    }

    #[tokio::test]
    pub(super) async fn overlay_volume_et_decalage_ont_des_echeances_independantes() {
        // Le test qui compte (brief) : avec deux durées différentes,
        // l'incrustation volume suit `overlay_ms` et celle du cumul suit
        // `tens_window_ms`. C'est l'assertion qui échouerait si quelqu'un
        // recouplait les deux durées derrière un seul champ. Échéances
        // comparées à `Instant::now()`, pas de sommeil.
        //
        // Les durées sont **délibérément énormes** au regard de ce que fait le
        // test. Avec `overlay_ms: 1000` et un pivot à 2000 ms, l'assertion
        // exigeait implicitement que `handle_command` rende la main en moins
        // d'une seconde : une hypothèse d'exécution rapide, donc un flake en
        // puissance dès que la machine est chargée par les autres binaires de
        // test. Le pivot à 300 s entre 60 s et 600 s prouve exactement la même
        // propriété, en laissant quatre minutes de marge à une commande qui
        // prend des microsecondes.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(crate::state::Settings {
            overlay_ms: 60_000,
            tens_window_ms: 600_000,
            ..Default::default()
        });

        let avant = Instant::now();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let echeance_volume = core.overlay_deadline().unwrap();
        assert!(
            echeance_volume < avant + Duration::from_millis(300_000),
            "l'incrustation volume doit suivre overlay_ms (60 s), pas tens_window_ms"
        );

        core.handle_command(Command::Plus10).await.unwrap();
        let echeance_decalage = core.overlay_deadline().unwrap();
        assert!(
            echeance_decalage > avant + Duration::from_millis(300_000),
            "l'incrustation du cumul doit suivre tens_window_ms (600 s), pas overlay_ms"
        );
    }

    #[tokio::test]
    pub(super) async fn volume_deadline_ne_survit_pas_a_la_veille() {
        // A deadline armed before standby must not let a held key step the
        // volume after waking: it has to re-press first.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(quick_settings());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65, arms the deadline
        core.handle_command(Command::Power).await.unwrap();    // standby, clears it
        core.handle_command(Command::Power).await.unwrap();    // wake
        // L'absence d'deadline est affirmee directement, au lieu d'etre deduite
        // d'un sommeil de 40 ms : c'est elle qu'on teste, et une assertion sur
        // l'state ne depend d'aucune horloge.
        assert!(core.volume_deadline.is_none(), "la veille doit avoir efface l'deadline");
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.player_state().volume, 65, "pas de deadline restante : le held ne fait rien");
    }
}
