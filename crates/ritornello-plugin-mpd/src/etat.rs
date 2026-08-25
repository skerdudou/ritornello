//! État partagé entre la moitié `display` (qui reçoit les trames du cœur) et
//! les sessions clientes MPD (qui répondent aux commandes de lecture).
//!
//! Le point délicat de tout le greffon vit ici, et il n'est pas dans le
//! protocole : **le réveil manqué**. Un client qui envoie `idle` juste après
//! un changement doit repartir immédiatement, pas attendre le changement
//! suivant. Un `Notify` seul perdrait ce réveil — la notification est émise
//! pendant que la session lit encore ses versions et compose sa requête, donc
//! avant qu'elle ne s'inscrive, et elle resterait muette jusqu'au changement
//! d'après. D'où la conception retenue : un compteur monotone par
//! sous-système, que la session mémorise avant de s'endormir, et une
//! comparaison **préalable** dans `attendre`. C'est cette comparaison qui
//! interdit le réveil manqué ; le `Notify` ne sert qu'à ne pas sonder.

use ritornello_proto::{Catalogue, Command, Playback, PlayerState, Preset, SourceCatalogue};
use tokio::sync::{Notify, RwLock};

/// Nombre de sous-systèmes, donc taille du tableau de compteurs. Une constante
/// et non un `Sujet::len()` : c'est la borne du tableau, elle doit être connue
/// à la compilation.
const NB_SUJETS: usize = 4;

/// Les sous-systèmes que `idle` sait nommer, dans l'ordre où ils indexent le
/// tableau de compteurs.
///
/// Un `enum #[repr(usize)]` servant d'indice dans un `[u64; 4]`, et non une
/// table associative : les quatre sujets sont connus à la compilation, et
/// `versions[sujet as usize]` ne peut pas échouer — pas d'`unwrap` sur un
/// `get`, pas de sujet qu'on aurait oublié d'insérer à la construction.
///
/// Les valeurs explicites ne sont pas décoratives : elles sont l'indice, donc
/// **ne pas réordonner** sans réordonner ce que les tests comparent.
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sujet {
    /// Lecture, pause, arrêt, changement de présélection, position.
    Player = 0,
    /// Volume ou sourdine.
    Mixer = 1,
    /// La file d'attente change. Comme la file d'attente MPD *est* la liste des
    /// présélections de la source active, cela veut dire : changement de
    /// source.
    Playlist = 2,
    /// Le catalogue des sources ou de leurs présélections change.
    ///
    /// Son unique déclencheur est `appliquer_catalogue`, et c'est tout le sens
    /// des deux canaux : une trame d'état, même changeant tout le reste, ne le
    /// bouge jamais — sinon un client abonné aux seules listes enregistrées
    /// serait réveillé à chaque seconde de lecture.
    StoredPlaylist = 3,
}

/// Copie cohérente de tout ce qu'une session cliente a besoin de lire pour
/// composer une réponse : l'état poussé par le cœur, ce que le greffon croit
/// de la lecture, et les compteurs.
///
/// Un seul instantané rendu d'un coup, et non quatre accesseurs : une réponse
/// `status` publie l'état *et* la version de file, et les lire par deux prises
/// de verrou successives les laisserait se contredire au milieu d'une réponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Instantane {
    /// La dernière trame reçue du cœur, **éventuellement recouverte d'un
    /// calque optimiste** : `acter_optimiste` y pose le volume qu'une session
    /// vient de demander, avant que le cœur ne l'ait confirmé (voir là-bas).
    /// Ne pas lire ce champ comme le verbatim de ce que le cœur a envoyé — la
    /// trame suivante rétablit la vérité de toute façon, et la comparaison
    /// d'`appliquer_etat` réveille `Mixer` si elle la contredit.
    pub etat: PlayerState,
    /// Ce que le greffon **croit** de la lecture, y compris une bascule qu'il
    /// vient d'émettre et que la trame n'a pas encore confirmée : c'est la
    /// course de `pause`, où un client qui envoie `pause` puis `status` dans
    /// la même foulée lirait sinon l'état d'avant sa propre commande et
    /// afficherait un bouton qui n'a pas bougé.
    pub playback_optimiste: Playback,
    /// Compteur de version de la file d'attente, celui que `status` publie
    /// sous `playlist`.
    ///
    /// **Monotone**, jamais remis à zéro : un client compare la version qu'il
    /// détient à celle-ci pour savoir s'il a manqué quelque chose, et une
    /// remise à zéro lui ferait croire qu'il n'a rien manqué alors que tout a
    /// changé.
    pub version_file: u32,
    /// Un compteur par sujet, du même usage mais pour `idle` : une session
    /// endormie a mémorisé ce tableau, le compare à celui-ci, et repart
    /// aussitôt si quelque chose a bougé pendant qu'elle s'installait.
    pub versions: [u64; NB_SUJETS],
    /// Le dernier catalogue reçu du cœur : **toutes** les sources déclarées,
    /// dans l'ordre de bascule de `SourceCycle`, et les présélections nommées
    /// de chacune quand elle sait les énumérer.
    ///
    /// Il décrit toutes les sources et non la seule active, et c'est
    /// indispensable : `listplaylistinfo "radio"` s'interroge pendant que le cd
    /// joue. Il arrive par un canal distinct des trames d'état (voir
    /// `appliquer_catalogue`), donc il survit à n'importe quel nombre de trames
    /// sans être renvoyé.
    ///
    /// Vide avant la première trame de catalogue : un client verra alors une
    /// liste de listes enregistrées vide, et la file d'attente retombe sur la
    /// synthèse depuis `preset_count`. C'est bien la vérité de cet instant —
    /// le greffon ne sait encore rien du catalogue.
    pub catalogue: Catalogue,
}

impl Instantane {
    /// Les présélections nommées de cette source, telles que le catalogue les
    /// donne — ou `None` si le catalogue ne connaît pas ce nom.
    ///
    /// La distinction compte pour `listplaylistinfo` et `load` : un nom absent
    /// du catalogue est un `ACK 50` (« cette liste n'existe pas »), alors qu'un
    /// nom connu dont la liste est **vide** est une source qui ne sait pas
    /// énumérer — une réponse vide et bien formée, pas une erreur.
    pub fn catalogue_source(&self, nom: &str) -> Option<&SourceCatalogue> {
        self.catalogue.sources.iter().find(|s| s.name == nom)
    }

    /// Les présélections de la source **active**, ou une tranche vide. C'est
    /// d'elles que la file d'attente MPD est faite.
    pub fn presets_actifs(&self) -> &[Preset] {
        self.catalogue_source(&self.etat.source).map_or(&[], |s| s.presets.as_slice())
    }
    /// Ce qu'il faut publier comme état de lecture : l'optimiste, pas le brut
    /// de la trame.
    ///
    /// Appelé par le `status` de `commandes.rs` depuis la Task 6. L'écrire ici
    /// plutôt qu'à chaque site de réponse évite que l'un d'eux ait à se
    /// souvenir *lequel* des deux champs fait foi — et un test de ce module-là
    /// échoue si `status` lit `etat.playback`.
    pub fn playback(&self) -> Playback {
        self.playback_optimiste
    }
}

/// Ce que toutes les sessions clientes partagent : l'instantané courant et le
/// réveil des `idle` en attente.
///
/// Le verrou est un `tokio::sync::RwLock` et non un `Mutex` : les sessions ne
/// font presque que lire, et l'une qui compose un `listplaylistinfo` de 51
/// lignes ne doit pas retarder les autres. Les seuls écrivains sont la moitié
/// `display` (une trame) et une session qui vient d'émettre une commande.
#[derive(Default)]
pub struct EtatPartage {
    inner: RwLock<Instantane>,
    /// Réveille les `idle` en attente. `notify_waiters` et non `notify_one` :
    /// un changement concerne **tous** les dormeurs, et un permis mémorisé
    /// pour un seul d'entre eux serait pire qu'inutile ici — la comparaison
    /// des compteurs joue déjà le rôle de la mémoire.
    reveil: Notify,
}

/// Marque un sujet comme ayant bougé, sans doublon.
///
/// Le dédoublonnage n'est pas cosmétique : une liste de commandes MPD peut
/// contenir deux `pause`, et incrémenter deux fois le compteur pour un seul
/// passage sous le verrou ferait publier deux changements là où il n'y en a
/// qu'un.
fn marquer(bouges: &mut Vec<Sujet>, sujet: Sujet) {
    if !bouges.contains(&sujet) {
        bouges.push(sujet);
    }
}

impl EtatPartage {
    /// Copie de l'instantané courant. Une copie et non une garde : aucune
    /// session ne doit retenir le verrou au-delà de l'instant de la lecture,
    /// même si elle compose ensuite une réponse longue.
    ///
    /// Chaque session cliente l'invoque une fois par commande, pour répondre
    /// depuis la copie plutôt que sous le verrou. Les compteurs qu'un `idle`
    /// mémorise sont dans cette même copie : c'est ce qui les rend cohérents
    /// avec l'état publié dans la même réponse.
    pub async fn lire(&self) -> Instantane {
        self.inner.read().await.clone()
    }

    /// Copie du tableau de compteurs, à mémoriser **avant** de traiter un
    /// `idle` et à repasser à `attendre`.
    ///
    /// C'est la moitié utile du dispositif anti-réveil-manqué : la session lit
    /// ces valeurs au même moment qu'elle lit l'état qu'elle publie, si bien
    /// que tout ce qui bouge après cette lecture est nécessairement un
    /// changement qu'elle n'a pas encore vu.
    ///
    /// **Sans appelant en production, et la session ne doit pas en devenir
    /// un.** C'est le point à retenir de ce commentaire, parce qu'une méthode
    /// publique inutilisée donne envie de la câbler : la session lit
    /// `Instantane::versions` de la copie que `lire` lui rend déjà, dans la
    /// **même** prise de verrou que l'état qu'elle va publier. Appeler
    /// `versions` à côté prendrait le verrou une seconde fois et pourrait lire
    /// les compteurs d'un autre instant que l'état — c'est-à-dire rouvrir
    /// exactement la course que ce module a été bâti pour fermer, et de la
    /// façon la plus sournoise : un `idle` mémoriserait des compteurs plus
    /// récents que le `status` qu'il vient de rendre, et dormirait sur un
    /// changement que son client n'a jamais vu.
    ///
    /// Gardée malgré tout : ses propres tests l'emploient, et un appelant qui
    /// n'aurait besoin que des compteurs (sans rien publier) serait légitime.
    #[allow(dead_code)]
    pub async fn versions(&self) -> [u64; NB_SUJETS] {
        self.inner.read().await.versions
    }

    /// Applique une trame du cœur : elle fait autorité sur tout.
    ///
    /// Les sujets qui bougent sont décidés **par comparaison champ par champ**
    /// avec l'état précédent, et pas par le seul fait qu'une trame soit
    /// arrivée : le cœur déduplique déjà, mais une reconnexion de la moitié
    /// `display` renvoie l'état courant, et cela ne doit pas passer pour un
    /// changement — sinon chaque redémarrage du greffon réveillerait tous les
    /// clients pour rien.
    pub async fn appliquer_etat(&self, etat: PlayerState) {
        let mut bouges = Vec::new();
        {
            let mut inst = self.inner.write().await;
            let avant = &inst.etat;

            if etat.volume != avant.volume || etat.muted != avant.muted {
                marquer(&mut bouges, Sujet::Mixer);
            }
            if etat.source != avant.source {
                // Deux sujets pour un seul champ : la file d'attente *est* la
                // liste des présélections de la source active, donc changer de
                // source change la file (`playlist`) ; et ce qui joue change
                // avec elle (`player`). Un client qui n'écoute que `player`
                // doit apprendre qu'on a changé de source.
                marquer(&mut bouges, Sujet::Playlist);
                marquer(&mut bouges, Sujet::Player);
            }
            if etat.preset_count != avant.preset_count {
                // `preset_count` est ce dont la file d'attente MPD est faite
                // **à défaut de liste nommée** : pour une source qui ne sait
                // pas énumérer (le cd, les fichiers), c'est tout ce que le
                // greffon sait de la file. Un disque inséré passe de
                // `None`/`Some(0)` à
                // `Some(12)` sans changer de nom de source, et sans cette
                // comparaison aucun client n'apprendrait qu'il y a douze
                // pistes à jouer — l'action la plus ordinaire qui soit.
                //
                // `Playlist` seul, et **pas** `Player` : c'est la file qui a
                // changé, pas ce qui joue. (`source` bouge les deux parce
                // qu'elle change les deux ; `preset_count` seul ne touche pas
                // au morceau courant.)
                marquer(&mut bouges, Sujet::Playlist);
            }
            if etat.playback != avant.playback
                || etat.preset != avant.preset
                || etat.position_s != avant.position_s
                || etat.morceau != avant.morceau
            {
                marquer(&mut bouges, Sujet::Player);
            }

            // La trame écrase l'optimisme, y compris quand elle le contredit :
            // l'optimisme n'est qu'un pont jeté entre la commande émise et sa
            // confirmation, et le laisser survivre à une trame ferait mentir
            // `status` indéfiniment si le cœur avait refusé la bascule.
            inst.playback_optimiste = etat.playback;
            inst.etat = etat;

            for sujet in &bouges {
                inst.versions[*sujet as usize] += 1;
            }
            if bouges.contains(&Sujet::Playlist) {
                // Exactement quand `Playlist` bouge : les deux compteurs
                // disent la même chose à deux publics (`idle` et le champ
                // `playlist` de `status`), et les désynchroniser ferait
                // répondre `plchanges` à côté du réveil qui vient de partir.
                inst.version_file += 1;
            }
        }
        if !bouges.is_empty() {
            tracing::trace!("mpd frame moved subsystems {bouges:?}");
            self.reveil.notify_waiters();
        }
    }

    /// Applique un catalogue reçu du cœur : la liste des sources et leurs
    /// présélections nommées.
    ///
    /// **Deux sujets, et pas toujours les deux.**
    /// - `StoredPlaylist` bouge dès que le catalogue diffère du précédent :
    ///   c'est le sous-système que MPD réserve aux listes enregistrées, et
    ///   chaque source *est* une liste enregistrée ici.
    /// - `Playlist` (et avec lui `version_file`) ne bouge que si les
    ///   présélections de la source **active** ont changé — la file d'attente
    ///   vient de là et de nulle part ailleurs. Renommer une station d'une
    ///   source qui ne joue pas change les listes enregistrées sans toucher à
    ///   la file : réveiller `Playlist` ferait retélécharger 51 lignes à tous
    ///   les clients pour rien, et un `plchanges` répondrait une file
    ///   identique sous une version neuve.
    ///
    /// Comparaison et non affectation sèche, exactement comme `appliquer_etat`
    /// et pour la même raison : le cœur envoie la valeur courante **à la
    /// connexion**, donc une reconnexion de la moitié `display` repasse ici
    /// avec un catalogue identique, et cela ne doit pas passer pour un
    /// changement — sinon chaque redémarrage du greffon réveillerait tous les
    /// clients.
    pub async fn appliquer_catalogue(&self, catalogue: Catalogue) {
        let mut bouges = Vec::new();
        {
            let mut inst = self.inner.write().await;
            if inst.catalogue == catalogue {
                return;
            }
            // Tout changement réel du catalogue bouge `StoredPlaylist` : on est
            // passé la déduplication ci-dessus, donc le catalogue diffère
            // vraiment. C'est ce qui réveille un client endormi sur
            // `idle stored_playlist` — le seul sous-système que rien
            // n'incrémentait avant cette tâche.
            marquer(&mut bouges, Sujet::StoredPlaylist);
            // Lu avant l'écrasement, sur le nom de source de l'instantané
            // courant : c'est la source active telle que la dernière trame
            // d'état l'a dite, la seule autorité sur ce qui joue.
            let presets_avant = inst.presets_actifs().to_vec();
            inst.catalogue = catalogue;
            if inst.presets_actifs() != presets_avant.as_slice() {
                marquer(&mut bouges, Sujet::Playlist);
            }

            for sujet in &bouges {
                inst.versions[*sujet as usize] += 1;
            }
            if bouges.contains(&Sujet::Playlist) {
                // Le même appariement que dans `appliquer_etat` : les deux
                // compteurs disent la même chose à deux publics (`idle` et le
                // champ `playlist` de `status`), et les désynchroniser ferait
                // répondre `plchanges` à côté du réveil qui vient de partir.
                inst.version_file += 1;
            }
        }
        if !bouges.is_empty() {
            tracing::trace!("mpd catalogue moved subsystems {bouges:?}");
            self.reveil.notify_waiters();
        }
    }

    /// Acte ce que le greffon vient d'émettre, avant que le cœur ne le
    /// confirme.
    ///
    /// **Deux commandes seulement**, et c'est délibéré : `PlayPause` (bascule
    /// `Playing`↔`Paused`) et `SetVolume` (pose le volume). Tout le reste est
    /// ignoré, parce que deviner l'effet d'un `Select` sur la position, le
    /// morceau ou la présélection serait faux plus souvent que juste — c'est
    /// la source active qui décide, et elle seule. Un `status` légèrement en
    /// retard est bénin ; un `status` qui invente un morceau ne l'est pas.
    ///
    /// Le volume, lui, est posé dans `etat` faute d'un champ optimiste à part.
    /// C'est voulu et sans risque : la trame suivante l'écrase de toute façon,
    /// et si le cœur avait borné ou refusé la valeur, la comparaison
    /// d'`appliquer_etat` verra la différence et réveillera `Mixer`. Le seul
    /// effet de bord est que la trame *confirmante* ne rebouge rien — d'où
    /// l'incrément fait ici même.
    ///
    /// **L'asymétrie avec `PlayPause` est voulue**, et il faut l'écrire parce
    /// qu'elle se lit comme un oubli : la bascule ne touche pas
    /// `etat.playback`, donc la trame confirmante rebouge `Player` une seconde
    /// fois — un `changed: player` redondant. C'est le choix conservateur.
    /// `SetVolume` porte une valeur absolue que le cœur honore presque
    /// toujours au bit près : sans l'incrément fait ici, la trame confirmante
    /// serait identique et *personne* ne serait réveillé — il n'y avait donc
    /// pas le choix. `PlayPause` ne porte aucune valeur : c'est le greffon qui
    /// calcule la bascule, et la source active peut très bien finir ailleurs
    /// (un direct qu'on ne met pas en pause). Laisser `etat.playback` intact
    /// garde la trame comme seule autorité sur ce champ, et le prix est un
    /// réveil de trop. Ce prix est le bon sens de la dissymétrie : un réveil
    /// superflu coûte au client une interrogation `status` redondante, un
    /// réveil manquant lui coûte la justesse de son écran.
    ///
    /// Appelée par la session **après** avoir poussé les commandes sur le
    /// canal, jamais avant : acter une bascule qu'on n'a pas émise ferait
    /// mentir `status` jusqu'à la trame suivante.
    pub async fn acter_optimiste(&self, commandes: &[Command]) {
        let mut bouges = Vec::new();
        {
            let mut inst = self.inner.write().await;
            for commande in commandes {
                match commande {
                    Command::PlayPause => match inst.playback_optimiste {
                        // Sans effet à l'arrêt : `PlayPause` y démarre une
                        // lecture dont le greffon ne sait ni quoi ni où, donc
                        // il attend la trame plutôt que d'annoncer `Playing`
                        // sur un morceau vide.
                        Playback::Stopped => {}
                        Playback::Playing => {
                            inst.playback_optimiste = Playback::Paused;
                            marquer(&mut bouges, Sujet::Player);
                        }
                        Playback::Paused => {
                            inst.playback_optimiste = Playback::Playing;
                            marquer(&mut bouges, Sujet::Player);
                        }
                    },
                    Command::SetVolume(niveau) => {
                        let niveau = *niveau;
                        // Comparaison et non affectation seche : un `setvol`
                        // qui repose le volume courant (M.A.L.P. en envoie a
                        // chaque relachement de curseur) ne doit pas reveiller
                        // tous les autres clients pour rien.
                        if inst.etat.volume != niveau {
                            inst.etat.volume = niveau;
                            marquer(&mut bouges, Sujet::Mixer);
                        }
                    }
                    _ => {}
                }
            }
            for sujet in &bouges {
                inst.versions[*sujet as usize] += 1;
            }
            // Pas de `version_file` ici : aucune des deux commandes actées ne
            // touche la file d'attente.
        }
        if !bouges.is_empty() {
            tracing::trace!("mpd optimistic update moved subsystems {bouges:?}");
            self.reveil.notify_waiters();
        }
    }

    /// Attend qu'un des `sujets` demandés bouge par rapport aux compteurs
    /// `vues`, et rend la liste de ceux qui ont bougé — dans l'ordre où ils
    /// ont été demandés.
    ///
    /// **Compare d'abord, attend ensuite.** Si quelque chose a bougé depuis
    /// que l'appelant a lu `vues`, la fonction rend la main sans jamais
    /// toucher au `Notify` : c'est là et nulle part ailleurs que le réveil
    /// manqué est interdit.
    ///
    /// L'inscription au réveil est faite *sous le verrou de lecture*, avant la
    /// comparaison. Sans cela le trou se rouvrirait d'un cran plus loin : un
    /// `notify_waiters` émis entre la comparaison et le premier sondage du
    /// `Notified` ne trouverait aucun inscrit, et le dormeur attendrait le
    /// changement d'après. Un écrivain a besoin du verrou en écriture, donc
    /// tant que la garde en lecture est tenue, aucun changement ne peut se
    /// glisser entre l'inscription et la comparaison.
    ///
    /// La boucle n'est pas de la prudence en trop : `notify_waiters` réveille
    /// tous les dormeurs, y compris ceux dont aucun sujet demandé n'a bougé,
    /// et ceux-là doivent se rendormir.
    ///
    /// Appelée par la session pour tenir un `idle`. Une liste de sujets vide
    /// n'en sort jamais, et c'est le contrat : voir `Issue::Attendre`.
    pub async fn attendre(&self, sujets: &[Sujet], vues: [u64; NB_SUJETS]) -> Vec<Sujet> {
        loop {
            let notifie = self.reveil.notified();
            tokio::pin!(notifie);
            let bouges = {
                let inst = self.inner.read().await;
                // `enable` inscrit le futur maintenant plutôt qu'au premier
                // sondage : voir le raisonnement sur le verrou ci-dessus.
                let _ = notifie.as_mut().enable();
                sujets
                    .iter()
                    .copied()
                    .filter(|sujet| inst.versions[*sujet as usize] != vues[*sujet as usize])
                    .collect::<Vec<_>>()
            };
            if !bouges.is_empty() {
                return bouges;
            }
            notifie.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un catalogue tel que le cœur en émet : chaque source déclarée est
    /// nommée, et ses présélections sont celles qu'elle sait énumérer (vides
    /// pour le cd et les fichiers, qui restent au corps par défaut de
    /// `list_presets`).
    fn catalogue_de(sources: &[(&str, &[(u8, &str)])]) -> Catalogue {
        Catalogue {
            sources: sources
                .iter()
                .map(|(nom, presets)| SourceCatalogue {
                    name: (*nom).to_string(),
                    presets: presets
                        .iter()
                        .map(|(index, nom)| Preset { index: *index, name: (*nom).to_string() })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Le plus petit catalogue que le cœur puisse émettre : une source nommée,
    /// avec une présélection.
    fn catalogue_a_une_source() -> Catalogue {
        catalogue_de(&[("radio", &[(1, "FIP")])])
    }

    #[tokio::test]
    async fn lire_rend_letat_par_defaut_avant_toute_application() {
        let partage = EtatPartage::default();
        assert_eq!(partage.lire().await, Instantane::default());
    }

    #[tokio::test]
    async fn appliquer_etat_remplace_ce_que_lire_rend_ensuite() {
        let partage = EtatPartage::default();
        let nouvel_etat = PlayerState { volume: 42, source: "radio".into(), ..Default::default() };

        partage.appliquer_etat(nouvel_etat.clone()).await;

        assert_eq!(partage.lire().await.etat, nouvel_etat);
    }

    #[tokio::test]
    async fn une_trame_qui_change_le_volume_reveille_mixer_et_pas_playlist() {
        let e = EtatPartage::default();
        let avant = e.versions().await;
        e.appliquer_etat(PlayerState { volume: 40, ..Default::default() }).await;
        let apres = e.versions().await;
        assert_ne!(avant[Sujet::Mixer as usize], apres[Sujet::Mixer as usize]);
        assert_eq!(avant[Sujet::Playlist as usize], apres[Sujet::Playlist as usize]);
        assert_eq!(avant[Sujet::Player as usize], apres[Sujet::Player as usize], "le volume n'est pas du player");
    }

    #[tokio::test]
    async fn une_trame_qui_change_la_sourdine_reveille_mixer() {
        // `muted` compte autant que `volume` : les clients MPD coupent le son
        // en posant `setvol 0`, mais la sourdine peut aussi venir de la
        // telecommande, et le client doit l'apprendre.
        let e = EtatPartage::default();
        let avant = e.versions().await;
        e.appliquer_etat(PlayerState { muted: true, ..Default::default() }).await;
        let apres = e.versions().await;
        assert_ne!(avant[Sujet::Mixer as usize], apres[Sujet::Mixer as usize]);
    }

    #[tokio::test]
    async fn une_trame_identique_ne_reveille_personne() {
        // Le coeur deduplique deja, mais une reconnexion renvoie l'etat
        // courant : il ne doit pas passer pour un changement.
        let e = EtatPartage::default();
        let trame = PlayerState {
            volume: 40,
            source: "radio".into(),
            playback: Playback::Playing,
            preset: Some(3),
            preset_count: Some(51),
            position_s: Some(12),
            ..Default::default()
        };
        e.appliquer_etat(trame.clone()).await;
        let avant = e.versions().await;
        let version_file = e.lire().await.version_file;

        e.appliquer_etat(trame).await;

        assert_eq!(avant, e.versions().await);
        assert_eq!(version_file, e.lire().await.version_file, "la file n'a pas bouge non plus");
    }

    #[tokio::test]
    async fn un_changement_de_source_reveille_playlist_et_player() {
        // La file d'attente EST la liste des preselections de la source
        // active : changer de source change la file, et change aussi ce qui
        // joue.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let avant = e.versions().await;

        e.appliquer_etat(PlayerState { source: "cd".into(), ..Default::default() }).await;

        let apres = e.versions().await;
        assert_ne!(avant[Sujet::Playlist as usize], apres[Sujet::Playlist as usize]);
        assert_ne!(avant[Sujet::Player as usize], apres[Sujet::Player as usize]);
        assert_eq!(avant[Sujet::Mixer as usize], apres[Sujet::Mixer as usize], "le volume n'a pas bouge");
    }

    #[tokio::test]
    async fn un_disque_insere_change_la_file_dattente() {
        // `preset_count` est la longueur de la file MPD (`playlistlength`) :
        // un disque insere fait passer le lecteur CD de « rien a numeroter » a
        // douze pistes, sans changer de nom de source. Sans reveil de
        // `Playlist` ni avance de `version_file`, un client reste sur une file
        // vide et l'action la plus ordinaire du monde ne se voit pas depuis le
        // telephone. Et `Player` ne doit pas bouger : la file a change, pas ce
        // qui joue.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { source: "cd".into(), preset_count: Some(0), ..Default::default() })
            .await;
        let avant = e.versions().await;
        let version_file = e.lire().await.version_file;

        e.appliquer_etat(PlayerState { source: "cd".into(), preset_count: Some(12), ..Default::default() })
            .await;

        let apres = e.versions().await;
        assert_ne!(avant[Sujet::Playlist as usize], apres[Sujet::Playlist as usize], "la file a change");
        assert!(
            e.lire().await.version_file > version_file,
            "version_file doit avancer avec la file, sinon plchanges mentira"
        );
        assert_eq!(avant[Sujet::Player as usize], apres[Sujet::Player as usize], "ce qui joue n'a pas change");
        assert_eq!(avant[Sujet::Mixer as usize], apres[Sujet::Mixer as usize]);
    }

    #[tokio::test]
    async fn chaque_transition_de_preset_count_bouge_la_file() {
        // Les trois transitions que le type `Option<u8>` rend distinctes, a
        // source constante. Celle qui porte tout le poids de ce test est
        // **`None` -> `Some(0)`** : c'est la seule que perdrait une
        // comparaison ecrite sur `unwrap_or(0)`, et les deux valeurs ne
        // decrivent pas la meme file. `None` veut dire « la source n'a rien
        // declare » — le consommateur retombe sur la grille historique 1-9,
        // donc neuf entrees ; `Some(0)` veut dire « rien a numeroter » — un
        // lecteur CD sans disque, donc zero entree. Les confondre ferait
        // rater l'insertion d'un disque dans une source qui ne declarait rien
        // auparavant.
        //
        // Les deux autres lignes couvrent les deux sens du mouvement, mais il
        // faut savoir ce qu'elles valent comme preuve : elles passeraient
        // aussi sous `unwrap_or(0)` (0 != 12, puis 12 != 0). Seule la premiere
        // separe les deux implementations.
        let transitions: [(&str, Option<u8>, Option<u8>); 3] = [
            ("rien declare -> zero piste", None, Some(0)),
            ("zero piste -> douze pistes", Some(0), Some(12)),
            ("douze pistes -> rien declare", Some(12), None),
        ];
        for (nom, depart, arrivee) in transitions {
            let e = EtatPartage::default();
            e.appliquer_etat(PlayerState { source: "cd".into(), preset_count: depart, ..Default::default() })
                .await;
            let avant = e.versions().await;
            let version_file = e.lire().await.version_file;

            e.appliquer_etat(PlayerState { source: "cd".into(), preset_count: arrivee, ..Default::default() })
                .await;

            let apres = e.versions().await;
            assert_ne!(
                avant[Sujet::Playlist as usize],
                apres[Sujet::Playlist as usize],
                "{nom} : la file doit bouger"
            );
            assert!(
                e.lire().await.version_file > version_file,
                "{nom} : version_file doit avancer avec la file"
            );
            assert_eq!(
                avant[Sujet::Player as usize],
                apres[Sujet::Player as usize],
                "{nom} : ce qui joue n'a pas change"
            );
        }
    }

    #[tokio::test]
    async fn le_morceau_la_position_et_la_preselection_reveillent_player_seul() {
        // Les trois champs que le brief nomme sous `player`, chacun teste
        // separement : oublier l'un des trois laisserait un client muet
        // pendant tout un morceau.
        let base = PlayerState { source: "radio".into(), ..Default::default() };
        let variantes: [(&str, PlayerState); 3] = [
            ("playback", PlayerState { playback: Playback::Playing, ..base.clone() }),
            ("position", PlayerState { position_s: Some(7), ..base.clone() }),
            ("preselection", PlayerState { preset: Some(4), ..base.clone() }),
        ];
        for (nom, trame) in variantes {
            let e = EtatPartage::default();
            e.appliquer_etat(base.clone()).await;
            let avant = e.versions().await;

            e.appliquer_etat(trame).await;

            let apres = e.versions().await;
            assert_ne!(avant[Sujet::Player as usize], apres[Sujet::Player as usize], "{nom} devrait bouger player");
            assert_eq!(avant[Sujet::Playlist as usize], apres[Sujet::Playlist as usize], "{nom} ne touche pas la file");
            assert_eq!(avant[Sujet::Mixer as usize], apres[Sujet::Mixer as usize], "{nom} ne touche pas le mixer");
        }
    }

    #[tokio::test]
    async fn le_titre_du_morceau_reveille_player() {
        // Un flux radio ne change ni de source ni de preselection quand le
        // morceau change : c'est le seul signal que le client recevra.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let avant = e.versions().await;

        let mut trame = PlayerState { source: "radio".into(), ..Default::default() };
        trame.morceau.title = Some("Sonate".into());
        e.appliquer_etat(trame).await;

        assert_ne!(avant[Sujet::Player as usize], e.versions().await[Sujet::Player as usize]);
    }

    #[tokio::test]
    async fn aucune_trame_ne_bouge_stored_playlist() {
        // Le seul declencheur de ce sujet est `appliquer_catalogue`. Une trame
        // qui change tout le reste ne doit pas l'incrementer au passage.
        let e = EtatPartage::default();
        let avant = e.versions().await;

        e.appliquer_etat(PlayerState {
            volume: 30,
            muted: true,
            source: "cd".into(),
            playback: Playback::Playing,
            preset: Some(2),
            position_s: Some(3),
            ..Default::default()
        })
        .await;

        assert_eq!(
            avant[Sujet::StoredPlaylist as usize],
            e.versions().await[Sujet::StoredPlaylist as usize]
        );
    }

    #[tokio::test]
    async fn un_catalogue_neuf_reveille_stored_playlist() {
        let e = EtatPartage::default();
        let avant = e.versions().await;

        e.appliquer_catalogue(catalogue_a_une_source()).await;

        let apres = e.versions().await;
        assert_ne!(avant[Sujet::StoredPlaylist as usize], apres[Sujet::StoredPlaylist as usize]);
        assert_eq!(
            e.lire().await.catalogue,
            catalogue_a_une_source(),
            "le catalogue doit aussi etre memorise, pas seulement compte"
        );
    }

    #[tokio::test]
    async fn un_catalogue_identique_ne_reveille_personne() {
        // Le coeur envoie la valeur courante **a la connexion** : une
        // reconnexion de la moitie `display` repasse ici avec le meme
        // catalogue, et ne doit pas passer pour un changement — sinon chaque
        // redemarrage du greffon reveille tous les clients.
        let e = EtatPartage::default();
        e.appliquer_catalogue(catalogue_a_une_source()).await;
        let avant = e.versions().await;
        let version_file = e.lire().await.version_file;

        e.appliquer_catalogue(catalogue_a_une_source()).await;

        assert_eq!(avant, e.versions().await);
        assert_eq!(version_file, e.lire().await.version_file);
    }

    #[tokio::test]
    async fn un_catalogue_qui_touche_la_source_active_bouge_aussi_la_file() {
        // La file d'attente MPD *est* la liste des preselections de la source
        // active : renommer une station de la radio pendant qu'elle joue
        // change la file, donc `Playlist` et `version_file` avec elle.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { source: "radio".into(), ..Default::default() }).await;
        e.appliquer_catalogue(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let avant = e.versions().await;
        let version_file = e.lire().await.version_file;

        e.appliquer_catalogue(catalogue_de(&[("radio", &[(1, "FIP Rock")]), ("cd", &[])])).await;

        let apres = e.versions().await;
        assert_ne!(avant[Sujet::StoredPlaylist as usize], apres[Sujet::StoredPlaylist as usize]);
        assert_ne!(avant[Sujet::Playlist as usize], apres[Sujet::Playlist as usize]);
        assert!(
            e.lire().await.version_file > version_file,
            "version_file doit avancer avec la file, sinon plchanges mentira"
        );
    }

    #[tokio::test]
    async fn un_catalogue_qui_ne_touche_quune_source_inactive_laisse_la_file_tranquille() {
        // Le pendant, et celui qui a une valeur : la radio se renomme une
        // station pendant que le cd joue. Les listes enregistrees ont change,
        // la file d'attente non — reveiller `Playlist` ferait retelecharger la
        // file a tous les clients, et `plchanges` rendrait une file identique
        // sous une version neuve.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { source: "cd".into(), ..Default::default() }).await;
        e.appliquer_catalogue(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let avant = e.versions().await;
        let version_file = e.lire().await.version_file;

        e.appliquer_catalogue(catalogue_de(&[("radio", &[(1, "FIP"), (5, "Nova")]), ("cd", &[])]))
            .await;

        let apres = e.versions().await;
        assert_ne!(
            avant[Sujet::StoredPlaylist as usize],
            apres[Sujet::StoredPlaylist as usize],
            "les listes enregistrees ont bel et bien change"
        );
        assert_eq!(
            avant[Sujet::Playlist as usize],
            apres[Sujet::Playlist as usize],
            "la file d'attente vient de la source active, et elle n'a pas bouge"
        );
        assert_eq!(version_file, e.lire().await.version_file);
        assert_eq!(
            avant[Sujet::Player as usize],
            apres[Sujet::Player as usize],
            "un catalogue ne dit rien de ce qui joue"
        );
    }

    #[tokio::test]
    async fn le_catalogue_ne_voyage_pas_avec_chaque_trame_detat() {
        // Non-regression du choix des deux canaux : dix trames d'etat, un seul
        // catalogue. Les trames sont toutes differentes (le volume monte),
        // donc chacune reveille bel et bien quelque chose — ce test ne peut
        // pas passer parce qu'il n'y aurait rien eu a appliquer.
        let e = EtatPartage::default();
        e.appliquer_catalogue(catalogue_a_une_source()).await;
        let apres_catalogue = e.versions().await;
        for v in 1..=10u8 {
            e.appliquer_etat(PlayerState { volume: v, ..Default::default() }).await;
        }
        let apres = e.versions().await;
        assert_eq!(apres[Sujet::StoredPlaylist as usize], apres_catalogue[Sujet::StoredPlaylist as usize]);
        assert_eq!(
            apres[Sujet::Mixer as usize],
            apres_catalogue[Sujet::Mixer as usize] + 10,
            "les dix trames doivent avoir compte pour dix, sinon ce test ne prouve rien"
        );
        assert_eq!(e.lire().await.catalogue, catalogue_a_une_source(), "et le catalogue survit aux trames");
    }

    #[tokio::test]
    async fn catalogue_source_distingue_le_nom_inconnu_de_la_liste_vide() {
        // La distinction sur laquelle reposent `listplaylistinfo` et `load` :
        // un nom absent du catalogue est un `ACK 50`, un nom connu sans
        // preselection est une reponse vide et bien formee.
        let e = EtatPartage::default();
        e.appliquer_catalogue(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let inst = e.lire().await;

        assert!(inst.catalogue_source("nawak").is_none());
        assert_eq!(inst.catalogue_source("cd").map(|s| s.presets.len()), Some(0));
        assert_eq!(inst.catalogue_source("radio").map(|s| s.presets.len()), Some(1));
    }

    #[tokio::test]
    async fn les_presets_actifs_suivent_la_source_que_la_trame_designe() {
        // `presets_actifs` lit le nom de source de la derniere trame : c'est
        // elle et non le catalogue qui dit ce qui joue.
        let e = EtatPartage::default();
        e.appliquer_catalogue(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;

        e.appliquer_etat(PlayerState { source: "cd".into(), ..Default::default() }).await;
        assert!(e.lire().await.presets_actifs().is_empty());

        e.appliquer_etat(PlayerState { source: "radio".into(), ..Default::default() }).await;
        assert_eq!(e.lire().await.presets_actifs().len(), 1);
    }

    #[tokio::test]
    async fn un_catalogue_reveille_un_dormeur_inscrit_sur_les_listes_enregistrees() {
        // La contrepartie utile : un client qui dort sur `stored_playlist`
        // doit repartir quand le catalogue arrive, et c'est le seul evenement
        // qui le reveillera jamais.
        let e = std::sync::Arc::new(EtatPartage::default());
        let vues = e.versions().await;
        let dormeur = {
            let e = e.clone();
            tokio::spawn(async move { e.attendre(&[Sujet::StoredPlaylist], vues).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        e.appliquer_catalogue(catalogue_a_une_source()).await;

        assert_eq!(dormeur.await.unwrap(), vec![Sujet::StoredPlaylist]);
    }

    #[tokio::test]
    async fn la_version_de_file_est_monotone() {
        // Jamais remise a zero : un client qui compare croirait n'avoir rien
        // manque. Le troisieme tour revient a "radio", la valeur initiale, et
        // c'est justement le cas qu'une implementation derivee de l'etat (et
        // non d'un compteur) raterait.
        let e = EtatPartage::default();
        let mut precedente = e.lire().await.version_file;
        for source in ["radio", "cd", "radio"] {
            e.appliquer_etat(PlayerState { source: source.into(), ..Default::default() }).await;
            let v = e.lire().await.version_file;
            assert!(v > precedente, "{v} devrait depasser {precedente}");
            precedente = v;
        }
    }

    #[tokio::test]
    async fn la_version_de_file_ne_bouge_que_quand_la_file_bouge() {
        // Le pendant du test precedent : monotone ne veut pas dire "qui monte
        // a chaque trame". Un `plchanges` rendrait sinon toute la file a
        // chaque seconde de lecture.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let avant = e.lire().await.version_file;

        e.appliquer_etat(PlayerState { source: "radio".into(), volume: 50, position_s: Some(9), ..Default::default() })
            .await;

        assert_eq!(avant, e.lire().await.version_file);
    }

    #[tokio::test]
    async fn un_changement_survenu_avant_lattente_ne_se_perd_pas() {
        // LE test qui compte : la session lit les versions, un changement
        // arrive, *ensuite* elle s'endort. Elle doit repartir aussitot. Avec
        // un `Notify` seul, ce reveil serait perdu et le client resterait muet
        // jusqu'au changement suivant.
        let e = EtatPartage::default();
        let vues = e.versions().await;
        e.appliquer_etat(PlayerState { volume: 40, ..Default::default() }).await;
        // Pas de `timeout` ici : si l'attente bloque, le test pend et l'echec
        // est franc. Une marge d'horloge serait un flake en puissance.
        let changes = e.attendre(&[Sujet::Mixer], vues).await;
        assert_eq!(changes, vec![Sujet::Mixer]);
    }

    #[tokio::test]
    async fn lattente_ne_rend_que_les_sujets_demandes() {
        let e = EtatPartage::default();
        let vues = e.versions().await;
        e.appliquer_etat(PlayerState { volume: 40, source: "cd".into(), ..Default::default() }).await;
        let changes = e.attendre(&[Sujet::Mixer], vues).await;
        assert_eq!(changes, vec![Sujet::Mixer], "playlist a change mais n'etait pas demande");
    }

    #[tokio::test]
    async fn lattente_rend_les_sujets_dans_lordre_demande() {
        // L'ordre est celui de la demande et non celui de l'enum : c'est ce
        // que la session ecrira en lignes `changed:`, et un ordre stable est
        // ce qui rend cette sortie testable a la Task 8.
        let e = EtatPartage::default();
        let vues = e.versions().await;
        e.appliquer_etat(PlayerState { volume: 40, source: "cd".into(), ..Default::default() }).await;

        let changes = e.attendre(&[Sujet::Playlist, Sujet::Mixer, Sujet::Player], vues).await;

        assert_eq!(changes, vec![Sujet::Playlist, Sujet::Mixer, Sujet::Player]);
    }

    #[tokio::test]
    async fn une_trame_arrivee_pendant_lattente_reveille_le_dormeur() {
        // L'autre moitie du dispositif : quand la comparaison prealable ne
        // trouve rien, c'est le `Notify` qui doit rendre la main. Le dormeur
        // est lance dans une tache et les `yield_now` lui laissent atteindre
        // son point d'attente (ordonnanceur mono-tache de `#[tokio::test]`,
        // donc la tache en file passe avant celle qui cede).
        //
        // Aucune horloge : si la notification n'arrive pas, le `await` du
        // handle pend et l'echec est franc. Un `timeout` "assez long" serait
        // un flake en puissance — c'est exactement la famille de tests que le
        // chantier precedent a du supprimer.
        let e = std::sync::Arc::new(EtatPartage::default());
        let vues = e.versions().await;
        let dormeur = {
            let e = e.clone();
            tokio::spawn(async move { e.attendre(&[Sujet::Player], vues).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() }).await;

        assert_eq!(dormeur.await.unwrap(), vec![Sujet::Player]);
    }

    #[tokio::test]
    async fn un_dormeur_ne_repart_pas_sur_un_sujet_qui_nest_pas_le_sien() {
        // `notify_waiters` reveille tout le monde, donc un dormeur inscrit sur
        // `Mixer` seul est bel et bien reveille par une trame `player` — et
        // doit se rendormir. Sans la boucle de `attendre`, il rendrait une
        // liste vide et la session ecrirait un `OK` sans `changed:`, ce
        // qu'aucun client MPD ne sait interpreter.
        let e = std::sync::Arc::new(EtatPartage::default());
        let vues = e.versions().await;
        let dormeur = {
            let e = e.clone();
            tokio::spawn(async move { e.attendre(&[Sujet::Mixer], vues).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        // Ne bouge que `player` : le dormeur est reveille pour rien.
        e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(!dormeur.is_finished(), "un reveil sur un autre sujet ne doit pas terminer l'attente");

        // Puis ce qu'il attendait vraiment.
        e.appliquer_etat(PlayerState { playback: Playback::Playing, volume: 22, ..Default::default() }).await;
        assert_eq!(dormeur.await.unwrap(), vec![Sujet::Mixer]);
    }

    #[tokio::test]
    async fn letat_optimiste_devance_la_trame_puis_lui_cede() {
        // La course de `pause` : le greffon acte la bascule des qu'il l'emet,
        // et la trame suivante fait autorite.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        e.acter_optimiste(&[Command::PlayPause]).await;
        assert_eq!(e.lire().await.playback(), Playback::Paused, "acte avant la trame");
        e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        assert_eq!(e.lire().await.playback(), Playback::Playing, "la trame fait autorite");
    }

    #[tokio::test]
    async fn la_bascule_optimiste_repart_de_la_valeur_optimiste() {
        // Deux `pause` d'affilee reviennent a l'etat de depart : la bascule
        // lit `playback_optimiste` et non la trame, sinon la seconde
        // rebasculerait depuis `Playing` et rendrait encore `Paused`.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() }).await;

        e.acter_optimiste(&[Command::PlayPause]).await;
        e.acter_optimiste(&[Command::PlayPause]).await;

        assert_eq!(e.lire().await.playback(), Playback::Playing);
    }

    #[tokio::test]
    async fn la_bascule_optimiste_est_sans_effet_a_larret() {
        // `PlayPause` a l'arret demarre une lecture dont le greffon ne sait
        // ni quoi ni ou : il attend la trame plutot que d'annoncer `Playing`
        // sur un morceau vide.
        let e = EtatPartage::default();
        let avant = e.versions().await;

        e.acter_optimiste(&[Command::PlayPause]).await;

        assert_eq!(e.lire().await.playback(), Playback::Stopped);
        assert_eq!(avant, e.versions().await, "rien a annoncer, donc aucun reveil");
    }

    #[tokio::test]
    async fn acter_un_volume_le_publie_aussitot_et_reveille_mixer() {
        // Un client qui envoie `setvol 70` puis `status` dans la meme foulee
        // doit lire 70, et les autres clients doivent etre reveilles : la
        // trame confirmante, elle, sera identique et ne bougera rien.
        let e = EtatPartage::default();
        let avant = e.versions().await;

        e.acter_optimiste(&[Command::SetVolume(70)]).await;

        assert_eq!(e.lire().await.etat.volume, 70);
        assert_ne!(avant[Sujet::Mixer as usize], e.versions().await[Sujet::Mixer as usize]);
    }

    #[tokio::test]
    async fn acter_le_volume_deja_en_place_ne_reveille_personne() {
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { volume: 70, ..Default::default() }).await;
        let avant = e.versions().await;

        e.acter_optimiste(&[Command::SetVolume(70)]).await;

        assert_eq!(avant, e.versions().await);
    }

    #[tokio::test]
    async fn acter_ignore_les_commandes_dont_leffet_ne_se_devine_pas() {
        // Deviner ce qu'un `Select` fait a la position, au morceau ou a la
        // preselection serait faux plus souvent que juste : c'est la source
        // active qui decide.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { playback: Playback::Playing, volume: 30, ..Default::default() }).await;
        let avant_instantane = e.lire().await;

        e.acter_optimiste(&[
            Command::Select(4),
            Command::Next,
            Command::Prev,
            Command::Stop,
            Command::SeekTo(30),
            Command::Mute,
            Command::VolumeUp,
            Command::SourceCycle,
        ])
        .await;

        assert_eq!(avant_instantane, e.lire().await, "aucune de ces commandes ne s'acte");
    }

    #[tokio::test]
    async fn une_liste_de_deux_bascules_ne_compte_quun_changement() {
        // Le dedoublonnage de `marquer` : deux `pause` dans une meme liste de
        // commandes MPD passent sous le verrou une seule fois, et un seul
        // changement est publie — l'etat final, lui, est bien celui des deux
        // bascules.
        let e = EtatPartage::default();
        e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        let avant = e.versions().await;

        e.acter_optimiste(&[Command::PlayPause, Command::PlayPause]).await;

        assert_eq!(
            avant[Sujet::Player as usize] + 1,
            e.versions().await[Sujet::Player as usize],
            "un seul incrément pour une seule prise de verrou"
        );
        assert_eq!(e.lire().await.playback(), Playback::Playing);
    }

    #[test]
    fn les_sujets_indexent_le_tableau_sans_trou() {
        // La conception repose sur `sujet as usize` : si un jour une variante
        // recevait une valeur hors bornes ou en double, l'indexation
        // paniquerait ou deux sujets partageraient un compteur.
        let indices = [
            Sujet::Player as usize,
            Sujet::Mixer as usize,
            Sujet::Playlist as usize,
            Sujet::StoredPlaylist as usize,
        ];
        let mut vus = [false; NB_SUJETS];
        for i in indices {
            assert!(i < NB_SUJETS, "{i} sort du tableau de compteurs");
            assert!(!vus[i], "deux sujets partagent l'indice {i}");
            vus[i] = true;
        }
        assert!(vus.iter().all(|v| *v), "un indice du tableau n'a pas de sujet");
    }
}
