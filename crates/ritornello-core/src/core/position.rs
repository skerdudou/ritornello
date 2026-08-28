//! Position de lecture : la progression que mpv rapporte, l'ancre qu'un plugin pose, et ce qui les perime.

use super::*;

impl<P: Player> Core<P> {
    /// Relit où on en est, auprès du fournisseur qui a le droit de parler.
    ///
    /// Deux fournisseurs, jamais en concurrence : mpv pour un contenu fini,
    /// un plugin `metadata` pour un flux. Le `time-pos` d'un flux compte
    /// depuis le début de la connexion et n'a aucun rapport avec le morceau —
    /// il est lu et jeté, jamais publié.
    ///
    /// Ne publie rien : l'appelant décide (le tick publie, `handle_command`
    /// publie déjà en sortie).
    pub async fn rafraichit_position(&mut self) {
        if self.standby || !self.lecture {
            self.oublie_position();
            return;
        }
        if self.expecting_stream {
            // Flux : le `time-pos` de mpv compte depuis le début de la
            // connexion, sans rapport avec le morceau. La position vient donc
            // d'un plugin `metadata`, ancrée à sa réception et avancée ici.
            self.duree_mesuree_s = None;
            self.position_s = self.ancre_position.map(|(depart, pose)| {
                let ecoule = pose.elapsed().as_secs();
                let brute = depart.saturating_add(u32::try_from(ecoule).unwrap_or(u32::MAX));
                // Plafonnée par la durée annoncée : un morceau qui finit avant
                // que la station ne l'annonce ne doit pas afficher
                // « 4:31 / 4:14 ».
                match self.metadonnees.duration_s() {
                    Some(duree) => brute.min(duree),
                    None => brute,
                }
            });
            return;
        }
        match self.player.progression().await {
            Ok(p) => {
                self.position_s = p.position_s.map(|s| s as u32);
                self.duree_mesuree_s = p.duration_s.filter(|d| *d > 0.0).map(|s| s as u32);
            }
            Err(e) => {
                // Une position illisible n'arrête pas la musique : on cesse
                // simplement d'en annoncer une.
                tracing::debug!("playback progress unavailable: {e}");
                self.position_s = None;
                self.duree_mesuree_s = None;
            }
        }
    }

    /// Plus rien ne joue : plus rien à situer.
    pub(super) fn oublie_position(&mut self) {
        self.position_s = None;
        self.duree_mesuree_s = None;
        self.ancre_position = None;
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    async fn la_position_de_mpv_est_publiee_sur_un_contenu_fini() {
        // La source active de `setup()` est `radio` (`PersistedState::default`) :
        // `SourceCycle` bascule vers `cd`, qui répond `play("cdda://").finite()` —
        // un contenu fini.
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.regle_progression(Some(87.4), Some(254.0));
        core.rafraichit_position().await;
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, Some(87), "tronquée, jamais arrondie au-dessus");
        assert_eq!(etat.morceau.duration_s, Some(254));
        assert!(etat.seekable, "un disque se parcourt");
        // 87.6 et non 87.4 : au-dessus de la demi-seconde, une troncature et un
        // arrondi ne donnent plus le même entier, et le test distingue enfin
        // les deux implémentations.
        core.regle_progression(Some(87.6), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
    }

    /// Sur un flux, `time-pos` compte depuis le début de la connexion et n'a
    /// aucun rapport avec le morceau : il est lu et jeté. Sans cette garde, la
    /// radio afficherait un compteur d'écoute croissant à la place de la
    /// position dans le morceau.
    #[tokio::test]
    async fn la_position_de_mpv_est_ecartee_sur_un_flux() {
        let (mut core, _, _, _, _dir) = setup();
        // La source active est déjà `radio` : `PlayPause` sans rien qui joue
        // lui redemande d'activer, et la factice répond `play("http://fip")`
        // sans `finite`.
        core.handle_command(Command::PlayPause).await.unwrap();
        core.regle_progression(Some(1234.0), Some(0.0));
        core.rafraichit_position().await;
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, None);
        assert!(!etat.seekable, "un direct ne se rembobine pas");
    }

    /// Régression : `rafraichit_position` n'effaçait que `duree_mesuree_s`
    /// dans la branche flux, laissant `position_s` figé sur la dernière
    /// valeur mesurée pour un disque. `lecture` repasse à `true` aussitôt
    /// qu'à `false` lors d'un `SourceCycle` (le cœur réactive la nouvelle
    /// source dans la foulée), donc le garde-fou `!self.lecture` ne se
    /// déclenche jamais entre les deux et la position du disque survivait,
    /// affichée indéfiniment sous le flux qui a pris sa place.
    #[tokio::test]
    async fn une_position_de_disque_ne_survit_pas_au_passage_a_un_flux() {
        let (mut core, _, _, _, _dir) = setup();
        // Fait jouer le cd, mesure une position.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.regle_progression(Some(87.0), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        // Retour vers la radio : un flux, sans rapport avec la position du disque.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, None, "la position du disque ne doit pas survivre au flux");
    }

    #[tokio::test]
    async fn l_arret_oublie_la_position() {
        let (mut core, _, _, _, _dir) = setup();
        // Bascule vers `cd`, contenu fini : voir le test ci-dessus.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.regle_progression(Some(87.0), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        core.handle_command(Command::Stop).await.unwrap();
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, None, "plus rien ne joue, plus rien à situer");
        assert_eq!(etat.morceau.duration_s, None);
        assert!(!etat.seekable);
    }

    /// La durée mesurée par mpv l'emporte sur celle qu'un plugin annonce : le
    /// disque réel prime sur ce qu'une base en ligne en dit.
    #[tokio::test]
    async fn la_duree_de_mpv_l_emporte_sur_celle_d_un_plugin() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["musicbrainz".into()]);
        // Bascule vers `cd`, contenu fini : sans quoi `rafraichit_position`
        // écarterait la mesure de mpv comme s'il s'agissait d'un flux.
        core.handle_command(Command::SourceCycle).await.unwrap();
        let id = serde_json::json!({"disc": "abc", "track": 2});
        core.handle_source_update("cd", joue(id.clone()));
        core.handle_enrichment(
            "musicbrainz",
            Enrichment {
                identity: id,
                title: Some("So What".into()),
                duration_s: Some(999),
                ..Default::default()
            },
        );
        core.regle_progression(Some(10.0), Some(545.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().morceau.duration_s, Some(545));
    }

    /// Entre deux interrogations du direct — plusieurs dizaines de secondes
    /// chez Radio France — c'est le cœur qui fait avancer la barre, depuis
    /// l'ancre posée à la réception.
    #[tokio::test]
    async fn l_ancre_d_un_enrichissement_avance_toute_seule() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Un **flux** : c'est le seul contexte où l'ancre parle (sur un
        // contenu fini, mpv a la parole). `radio` est déjà la source active.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(254),
                position_s: Some(87),
                ..Default::default()
            },
        );
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        core.avance_ancre_pour_test(std::time::Duration::from_secs(3));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(90));
    }

    /// Un morceau qui finit avant que la station ne l'annonce ne doit pas
    /// afficher « 4:31 / 4:14 ».
    #[tokio::test]
    async fn la_position_annoncee_est_plafonnee_par_la_duree() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(100),
                position_s: Some(98),
                ..Default::default()
            },
        );
        core.avance_ancre_pour_test(std::time::Duration::from_secs(30));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(100));
    }

    /// L'ancre du morceau précédent ne doit pas continuer d'avancer sous le
    /// titre du suivant.
    #[tokio::test]
    async fn un_changement_d_identite_efface_l_ancre() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let un = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(un.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment { identity: un, title: Some("A".into()), position_s: Some(50), ..Default::default() },
        );
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(50));
        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        // Avant meme le rafraichissement : la position du morceau precedent
        // ne doit pas survivre sous le titre du suivant (defaut corrige).
        assert_eq!(core.etat_lecteur().position_s, None, "position perimee sous le titre suivant");
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, None);
    }

    /// Régression : un plugin retenu en réserve qui répond (titre corrigé,
    /// pochette trouvée plus tard) ne doit pas réancrer la position sur la
    /// valeur — inchangée — du gagnant, faute de quoi la barre reculerait
    /// brutalement de tout ce qu'elle avait avancé depuis la précédente
    /// annonce du gagnant.
    #[tokio::test]
    async fn un_plugin_en_reserve_ne_fait_pas_reculer_la_position() {
        let (mut core, _np_rx, _etat_rx, _dir) =
            setup_metadonnees(vec!["radiofrance".into(), "ouifm".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id.clone(),
                title: Some("Bikwix".into()),
                position_s: Some(87),
                ..Default::default()
            },
        );
        core.avance_ancre_pour_test(std::time::Duration::from_secs(30));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(117));
        // `ouifm` répond, mais n'est pas le gagnant : rien de neuf sur
        // l'avancement.
        core.handle_enrichment(
            "ouifm",
            Enrichment { identity: id, title: Some("Autre titre".into()), ..Default::default() },
        );
        core.rafraichit_position().await;
        assert_eq!(
            core.etat_lecteur().position_s,
            Some(117),
            "un plugin en reserve ne doit pas faire reculer la position"
        );
    }
}
