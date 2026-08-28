//! Publication : l'etat du lecteur et le catalogue des sources, pousses aux afficheurs, a la SPA et aux greffons metadata.

use super::*;

impl<P: Player> Core<P> {
    /// Diffuse l'état structuré du lecteur : à la SPA, et aux plugins Display
    /// (qui composent eux-mêmes leur mise en page depuis cette même trame).
    pub(crate) fn publie_etat(&self) {
        let etat = self.etat_lecteur();
        // Publié généreusement (à la fin de chaque commande, en plus des
        // chemins de métadonnées), donc dédupliqué : sans cette garde, chaque
        // navigateur connecté et chaque afficheur recevrait une trame
        // identique à la précédente.
        self.etat_tx.send_if_modified(|courant| {
            if *courant == etat {
                false
            } else {
                *courant = etat;
                true
            }
        });
        // `known` republié au même point de passage que l'état structuré :
        // c'est ici que tout chemin qui vient d'ajouter ou de corriger une
        // information de métadonnées (ICY, tags, enrichissement, pochette)
        // finit par converger, et c'est ce qui permet à un plugin `metadata`
        // câblé à chaud — ou simplement lent à répondre — de voir ce qui est
        // déjà connu sans attendre un hypothétique prochain changement
        // d'identité, qui peut ne jamais survenir tant que le même morceau
        // joue. `set_identity` construit lui-même son `NowPlaying` (source et
        // identité en changent aussi) ; ce `send_if_modified` ne fait alors
        // que constater l'égalité et ne republie rien en trop.
        let known = self.metadonnees.known();
        self.now_playing_tx.send_if_modified(|np| {
            if np.known == known {
                false
            } else {
                np.known = known;
                true
            }
        });
    }

    /// Ce qui est structurel : les sources déclarées, **dans l'ordre de
    /// bascule** de `SourceCycle`, et les présélections nommées de chacune
    /// quand elle sait les énumérer.
    ///
    /// L'ordre vient de `source_order` et non des clés de la table : c'est
    /// l'ordre que les clients verront dans `listplaylists`, et il doit être
    /// celui de la touche `SourceCycle` — sinon la liste et la touche
    /// divergent. Une source qui n'énumère pas figure quand même, avec une
    /// liste vide : elle existe, et le consommateur retombe sur `preset_count`.
    pub fn catalogue(&self) -> Catalogue {
        Catalogue {
            sources: self
                .source_order
                .iter()
                .map(|nom| SourceCatalogue {
                    name: nom.clone(),
                    presets: self.presets_par_source.get(nom).cloned().unwrap_or_default(),
                })
                .collect(),
        }
    }

    /// Diffuse le catalogue vers les afficheurs. Jumeau de `publie_etat`, sur
    /// **son propre** canal.
    ///
    /// Appelé là où le catalogue peut changer, et seulement là : à la
    /// construction du cœur (les sources du démarrage), à l'arrivée de
    /// présélections, à `add_source` (une source câblée à chaud apparaît dans la
    /// liste) et à `remove_source` (un greffon éteint en disparaît, sans quoi un
    /// client MPD garderait une liste enregistrée sur laquelle agir). Jamais
    /// depuis `publie_etat`, et `publie_etat` jamais depuis
    /// ici : les deux canaux sont séparés précisément pour ne pas se déclencher
    /// l'un l'autre — sinon les noms de 51 stations repartiraient sur chaque
    /// trame par seconde de lecture, et la déduplication par égalité ne
    /// rattraperait rien puisque les deux valeurs changeraient ensemble.
    ///
    /// Même déduplication que l'état, pour la même raison : une source qui
    /// réannonce la même liste — la radio le fait à chaque enregistrement de sa
    /// page d'admin — ne doit pas réveiller les afficheurs.
    pub(crate) fn publie_catalogue(&self) {
        let catalogue = self.catalogue();
        self.catalogue_tx.send_if_modified(|courant| {
            if *courant == catalogue {
                false
            } else {
                *courant = catalogue;
                true
            }
        });
    }

    /// État complet du lecteur : ce qui est volatil, donc ce que la SPA reçoit
    /// en flux poussé.
    pub fn etat_lecteur(&self) -> PlayerState {
        PlayerState {
            source: self.active_source.clone(),
            volume: self.volume,
            muted: self.muted,
            standby: self.standby,
            preset: self.preset,
            preset_name: self.preset_name.clone(),
            preset_count: self.preset_count,
            // La veille gagne sur le statut de la source : l'appareil dort, ce
            // que raconte la source n'a plus cours.
            status: if self.standby { self.standby_status.clone() } else { self.source_status.clone() },
            overlay: self.overlay.as_ref().map(|(o, echeance)| {
                let restant = echeance.saturating_duration_since(Instant::now()).as_millis();
                // Le `remaining_ms` mémorisé n'est jamais lu : il est réécrit
                // ici à chaque publication. L'égalité d'`Overlay` l'ignore,
                // donc ce rafraîchissement ne défait pas la déduplication des
                // trames.
                o.clone().avec_restant(u32::try_from(restant).unwrap_or(u32::MAX))
            }),
            // Gardée **ici**, à la publication, et non effacée dans chacun des
            // cinq chemins qui posent `lecture = false` (arrêt, veille,
            // changement de source, fin de contenu, `SourceAction::Stop`).
            // Un point unique ne peut pas être oublié ; cinq appels
            // sprinkled le seraient au sixième chemin ajouté, et la barre
            // resterait figée sur la dernière valeur connue sans que rien ne
            // le signale.
            position_s: if self.lecture && !self.standby { self.position_s } else { None },
            // Même raison qu'au-dessus : calculé à la publication plutôt
            // qu'entretenu dans les cinq chemins qui posent `lecture = false`.
            playback: if !self.lecture || self.standby {
                Playback::Stopped
            } else if self.paused {
                Playback::Paused
            } else {
                Playback::Playing
            },
            // `lecture` et non `expecting_stream` : la première dit « quelque
            // chose joue », la seconde « c'est un flux relançable ». Un
            // contenu déplaçable est exactement ce qui joue sans être un flux.
            seekable: self.lecture && !self.standby && !self.expecting_stream,
            // Rien à voir avec ce qui joue : un tiroir vide s'ouvre quand
            // même, et c'est la Source qui a le tiroir. La veille est le seul
            // état qui l'annule, parce qu'elle n'y laisse passer aucune
            // commande.
            can_eject: self.can_eject && !self.standby,
            // Une préférence de rendu, poussée avec le reste : un afficheur ne
            // va jamais rien chercher de côté, et l'horloge qu'il dessine en
            // veille est quelque chose qu'il montre. Elle ne bouge qu'au geste
            // de l'utilisateur, donc elle ne provoque aucune trame en trop.
            clock: ritornello_proto::Horloge {
                date: match self.settings.date_format {
                    crate::state::DateFormat::DayMonthYear => ritornello_proto::FormatDate::DayMonthYear,
                    crate::state::DateFormat::YearMonthDay => ritornello_proto::FormatDate::YearMonthDay,
                    crate::state::DateFormat::MonthDayYear => ritornello_proto::FormatDate::MonthDayYear,
                },
                douze_heures: !self.settings.clock_24h,
            },
            morceau: {
                let mut m = self.metadonnees.etat();
                // Précédence : la durée mesurée par mpv l'emporte sur celle
                // qu'un plugin annonce. `origin` continue de désigner qui a
                // fourni le **morceau** (artiste, titre, album) et non qui a
                // fourni la durée — imprécision assumée plutôt qu'un second
                // champ d'origine pour une seule valeur numérique.
                if self.lecture && !self.standby && self.duree_mesuree_s.is_some() {
                    m.duration_s = self.duree_mesuree_s;
                }
                m
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    async fn letat_du_lecteur_diffuse_volume_muet_veille_et_source() {
        // Le volume n'est expose par aucune route : sa place est ce canal
        // pousse, avec le reste de ce qui est volatil. Une branche de
        // `handle_command` qui oublierait de publier laisserait l'IHM afficher
        // un etat perime sans que rien ne le signale — d'ou la publication a la
        // sortie de **toute** commande, et d'ou ce test qui les parcourt.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        let initial = etat_rx.borrow().clone();
        assert_eq!(initial.volume, 60, "le volume persiste doit etre connu des le demarrage");
        assert_eq!(initial.source, "radio");
        assert!(!initial.muted);
        assert!(!initial.standby);

        core.handle_command(Command::VolumeUp).await.unwrap();
        assert_eq!(etat_rx.borrow().volume, 65);
        core.handle_command(Command::VolumeDown).await.unwrap();
        assert_eq!(etat_rx.borrow().volume, 60);

        core.handle_command(Command::Mute).await.unwrap();
        assert!(etat_rx.borrow().muted);
        core.handle_command(Command::Mute).await.unwrap();
        assert!(!etat_rx.borrow().muted);

        core.handle_command(Command::Power).await.unwrap();
        assert!(etat_rx.borrow().standby, "la veille doit se voir dans l'IHM");
        core.handle_command(Command::Power).await.unwrap();
        assert!(!etat_rx.borrow().standby);
    }

    #[tokio::test]
    async fn le_morceau_est_aplati_dans_le_json_de_letat() {
        // L'IHM recoit un objet plat : un seul encart, pas deux niveaux a
        // distinguer.
        let (mut core, _np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        let json = serde_json::to_value(core.etat_lecteur()).unwrap();
        assert_eq!(json["source"], "radio");
        assert_eq!(json["volume"], 60);
        assert_eq!(json["artist"], "Miles Davis", "aplati, pas sous `morceau`");
        assert_eq!(json["title"], "So What");
        assert_eq!(json["origin"], "ouifm");
    }

    #[tokio::test]
    async fn le_catalogue_suit_lordre_de_bascule_des_sources() {
        // C'est l'ordre que les clients verront dans `listplaylists`, et il doit
        // etre celui de `SourceCycle` : sinon la liste et la touche divergent.
        //
        // Compare a l'ordre **observe** en pressant la touche, et non a
        // `source_order` : comparer le catalogue au champ dont il est construit
        // ne prouverait rien.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.add_source("files".into(), Arc::new(FakeSource { name: "files", calls: source_calls }));
        let attendu = noms(&core.catalogue());
        assert_eq!(attendu.len(), 3);

        core.handle_command(Command::SelectSource(attendu[0].clone())).await.unwrap();
        let mut tour = vec![core.active_source().to_string()];
        for _ in 1..attendu.len() {
            core.handle_command(Command::SourceCycle).await.unwrap();
            tour.push(core.active_source().to_string());
        }
        assert_eq!(attendu, tour, "le catalogue doit enumerer dans le sens de la touche");
    }

    #[tokio::test]
    async fn le_catalogue_porte_les_sources_du_demarrage_sans_attendre_une_preselection() {
        // Les sources cablees au rendez-vous sont connues des la construction :
        // c'est `Core::new` qui publie, et sans cette publication le canal
        // garderait son `Catalogue::default()` vide. Un afficheur relaye avant la
        // premiere preselection — donc avant tout changement — lirait alors
        // « aucune source », et un client MPD repondrait un `listplaylists` vide.
        //
        // Assere la valeur **courante** du canal, celle que le relais envoie a la
        // connexion, et non un changement : c'est exactement ce que voit un
        // afficheur qui arrive.
        let (core, _pc, _sc, _rx, _d) = setup();
        let cat_rx = core.catalogue_tx.subscribe();
        assert_eq!(
            noms(&cat_rx.borrow()),
            vec!["cd".to_string(), "radio".into()],
            "le catalogue doit porter les sources du demarrage des la construction"
        );
    }

    #[tokio::test]
    async fn le_catalogue_ne_republie_pas_pour_une_liste_identique() {
        // Meme deduplication que l'etat : une source qui reannonce la meme liste
        // — la radio le fait a chaque enregistrement de sa page d'admin — ne doit
        // pas reveiller les afficheurs.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut cat_rx = core.catalogue_tx.subscribe();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(cat_rx.has_changed().unwrap(), "la premiere liste, elle, est une nouvelle");
        let _ = cat_rx.borrow_and_update();

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(!cat_rx.has_changed().unwrap(), "la meme liste ne doit rien reveiller");

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP 2")]));
        assert!(cat_rx.has_changed().unwrap(), "une liste differente, si");
    }

    #[tokio::test]
    async fn publier_letat_ne_republie_pas_le_catalogue() {
        // La propriete des deux canaux separes. Sans elle, 51 noms de station
        // voyageraient sur chaque trame par seconde de lecture.
        //
        // Ce qui est assere est **la notification**, pas l'absence d'appel : un
        // couplage qui passerait par `publie_catalogue` serait dedoublonne, donc
        // n'atteindrait aucun afficheur, donc ne casserait pas la propriete. Un
        // `catalogue_tx.send(...)` depuis `publie_etat` — l'ecriture naturelle du
        // couplage — la casse, et ce test tombe.
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        let cat_rx = core.catalogue_tx.subscribe();
        let vu = cat_rx.borrow().clone();
        let _ = etat_rx.borrow_and_update();

        core.handle_command(Command::VolumeUp).await.unwrap();
        core.publie_etat();
        assert!(etat_rx.has_changed().unwrap(), "l'etat, lui, a bien bouge");
        assert!(!cat_rx.has_changed().unwrap(), "le catalogue a bouge pour rien");
        assert_eq!(*cat_rx.borrow(), vu, "et il porte toujours la meme chose");
    }

    #[tokio::test]
    async fn la_veille_gagne_sur_le_statut_de_la_source() {
        // L'appareil dort : ce que raconte la source n'a plus cours, même si
        // elle continue (en pratique elle ne le fait pas) à en déclarer un.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut update = update_nu();
        update.status = Some("FIP".into());
        core.handle_source_update("radio", update);
        assert_eq!(core.etat_lecteur().status.as_deref(), Some("FIP"));

        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(
            core.etat_lecteur().status.as_deref(),
            Some("STANDBY"),
            "le mot de veille gagne sur le statut mémorisé de la source"
        );

        // Révision I2 (revue de branche) : ce test affirmait auparavant que le
        // réveil rendait la main au statut mémorisé ("FIP"), inchangé tant que
        // la Source n'en redéclarait pas un nouveau. C'était exactement le
        // bogue signalé par la revue — le statut d'une source pouvait survivre
        // à la veille et réapparaître sous une source qui n'a encore rien dit
        // (voir `le_statut_de_la_source_ne_survit_pas_a_la_mise_en_veille`).
        // La veille l'oublie désormais, comme `preset_count`.
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(
            core.etat_lecteur().status,
            None,
            "le réveil ne doit pas faire réapparaître un statut que la source n'a pas redéclaré"
        );
    }
}
