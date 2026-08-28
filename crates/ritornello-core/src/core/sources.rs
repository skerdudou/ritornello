//! Superviseur de sources : l'ordre du cycle, la bascule, l'arrivee a chaud et la mort d'un greffon, et l'application d'une SourceAction.

use super::*;

impl<P: Player> Core<P> {
    /// Nom de la source actuellement active (pour la page de statut vivante).
    pub fn active_source(&self) -> &str {
        &self.active_source
    }

    /// Langue courante, à transmettre au lancement d'un greffon rallumé.
    ///
    /// La langue est passée au processus via `RITORNELLO_LOCALE` : un greffon
    /// rallumé sur un appareil en français doit la retrouver au démarrage,
    /// sans attendre un `SetLocale` — le piège déjà rencontré avec `cd`, qui
    /// réaffichait `NO DISC` faute de langue tant qu'aucun changement de
    /// langue ne survenait après coup.
    pub fn locale_courante(&self) -> Option<String> {
        self.locale.clone()
    }

    /// Ajoute une source découverte **après** le démarrage : un greffon qui a
    /// raté le rendez-vous, ou qu'on a relancé à la main. Renvoie `true` si
    /// c'est un remplacement (ré-annonce d'un greffon déjà câblé).
    ///
    /// `source_order` est **retrié** : le cycle de sources suit l'ordre
    /// alphabétique, et une source arrivée en retard doit y prendre sa place
    /// normale, pas la queue — sinon `SourceCycle` change de sens selon la
    /// chronologie du démarrage.
    ///
    /// Si aucune source n'était active — un démarrage où *aucune* n'avait
    /// répondu — la nouvelle le devient : c'est le seul cas où l'arrivée d'un
    /// greffon change ce qui joue.
    ///
    /// **Ne réveille rien** : cette fonction n'affecte que la table et le nom
    /// de l'active. Le câblage à chaud passe par `cable_source_a_chaud`, qui
    /// enchaîne le réveil — sans quoi une première source arrivée en retard
    /// serait active et muette.
    pub fn add_source(&mut self, name: String, client: Arc<dyn Source>) -> bool {
        let premiere = self.sources.is_empty();
        let remplacement = self.sources.insert(name.clone(), client).is_some();
        if !self.source_order.contains(&name) {
            self.source_order.push(name.clone());
            self.source_order.sort();
        }
        if premiere {
            self.active_source = name;
        }
        // Le catalogue vient de changer de longueur : une source de plus y
        // figure, sans présélections tant qu'elle n'en a pas déclaré. Voir
        // `publie_catalogue` pour la liste à jour de ses points d'appel —
        // `remove_source` en est le symétrique.
        self.publie_catalogue();
        remplacement
    }

    /// Bascule vers `suivante` (ou vers **aucune** source si `None`) : arrêt,
    /// `Deactivate` de la sortante, oublis, persistance, `Activate` de l'entrante.
    ///
    /// Extraite de `Command::SourceCycle` et non recopiée : la désactivation d'un
    /// greffon fait exactement la même chose, et deux versions de cette séquence
    /// divergeraient au premier oubli ajouté d'un côté.
    ///
    /// Trois appelants, donc : `SourceCycle` (qui calcule le nom suivant dans
    /// l'ordre), `SelectSource` (qui le reçoit déjà tout fait, du greffon MPD)
    /// et `remove_source` (qui peut n'avoir aucun nom à donner). Séquence
    /// commune : arrêt du lecteur, `Deactivate` en best-effort, oubli de
    /// l'identité, du compte de présélections, du statut et de l'éjection,
    /// `persist()` **avant** `Activate`, publication finale.
    pub(super) async fn bascule_source(&mut self, suivante: Option<String>) -> Result<()> {
        // Changer de source, c'est toujours changer de ce qui joue — et c'est
        // le cœur qui arrête, sans dépendre des réponses des plugins. Avant,
        // l'action renvoyée par `Deactivate` (le `Stop` du plugin radio) était
        // ignorée, et l'arrêt reposait sur le `Play` de l'`Activate` suivant —
        // que le cd sans disque ne renvoie pas (`Noop`) : l'ancien flux
        // continuait de jouer sous un affichage qui annonçait la nouvelle
        // source, titres ICY compris.
        self.expecting_stream = false;
        self.lecture = false;
        self.player.stop().await?;
        // L'ancienne source est prévenue en best-effort : son arrêt est déjà
        // fait, elle n'a plus qu'à recaler son propre état.
        if let Err(e) = self.demande_active(SourceReq::Deactivate).await {
            tracing::debug!("deactivate: {e}");
        }
        self.active_source = suivante.unwrap_or_default();
        // On l'acte ici sans attendre que la nouvelle Source le déclare :
        // sinon une Source qui omettrait de le faire laisserait l'identité de
        // l'autre en place, et les plugins `metadata` continueraient
        // d'enrichir le morceau précédent.
        self.set_identity(None);
        // Le compte de présélections et le statut annoncés par l'ancienne
        // Source ne veulent rien dire pour la nouvelle : les garder
        // afficherait une fenêtre de numéros qui ne correspond à aucune
        // présélection réelle, ou un statut (« PAS DE DISQUE ») sous le nom
        // d'une source qui n'a encore rien dit — tant que la nouvelle Source
        // n'a pas parlé (ce qui peut ne jamais arriver : une présélection
        // vide déclare une trame éphémère, qui ne touche pas au statut
        // mémorisé).
        self.preset_count = None;
        self.source_status = None;
        // Idem pour l'éjection : la capacité décrit la Source qui s'en va.
        // Sans cet effacement, quitter le cd pour la radio laissait la touche
        // Eject active jusqu'à la première trame de la radio — et pour de bon
        // si elle restait muette.
        self.can_eject = false;
        self.retry_count = 0;
        // Persister **avant** `Activate` : si la nouvelle source ne répond
        // pas (timeout de 5 s du SDK), l'état mémoire, l'état sur disque et
        // l'affichage disent déjà tous la même chose — nouvelle source, rien
        // ne joue. Sans cela, l'échec laissait la bascule à moitié faite :
        // « cd » à l'écran, « radio » dans state.json.
        self.persist();
        if let Some(action) = self.demande_active(SourceReq::Activate).await? {
            self.apply(action).await?;
        }
        // La séquence n'est complète qu'une fois le nouvel état publié : tous
        // les chemins ci-dessus (`set_identity`, `apply`) ne publient que
        // lorsqu'ils changent quelque chose, et rien ne garantit qu'au moins
        // un d'eux le fasse — désactiver l'unique source, ou la désactiver
        // pendant qu'elle joue sans qu'une Source muette ne réponde à temps,
        // n'en déclenche aucun. `handle_command` publie déjà après chaque
        // commande, mais un appelant hors de ce chemin (le décâblage à chaud
        // d'un greffon) laisserait sinon les afficheurs décrire une source qui
        // n'existe plus. Le canal déduplique (`publie_etat`), donc cet appel
        // ne coûte rien de plus sur le chemin `SourceCycle`.
        self.publie_etat();
        Ok(())
    }

    /// Oublie une source dont le greffon est mort **de lui-même** — panique,
    /// `SIGSEGV`, tué à la main. Rend `false` si ce nom n'était pas une source.
    ///
    /// **La différence avec `remove_source` est délibérée, et elle tient en une
    /// phrase : celui-là bascule, celui-ci non.** Les deux évincent la même
    /// chose du catalogue, pour la même raison (un client MPD ne doit pas voir
    /// une liste enregistrée pour une source qu'il ne peut plus atteindre) ;
    /// seule diffère la conséquence sur ce qui joue, parce que seule diffère la
    /// question de qui a décidé.
    ///
    /// * `remove_source` : **l'opérateur a demandé** que cette source s'en aille.
    ///   Basculer vers la suivante est la suite de son geste, et arrêter le
    ///   lecteur d'abord est ce qui empêche l'ancien flux de continuer sous le
    ///   nom de la nouvelle source.
    /// * ici : **personne n'a rien demandé**. Un greffon de Source est un
    ///   *contrôleur* — il dit quoi jouer, il ne joue pas. Le flux est tenu par
    ///   mpv, qui est un enfant du cœur et que la mort du greffon ne touche pas.
    ///   Arrêter mpv et basculer sur le cd, c'est transformer la panne d'un
    ///   contrôleur en silence, puis présenter à l'écran une source que
    ///   l'utilisateur n'a pas choisie : deux fautes, dont la seconde est du
    ///   mensonge. On ne fait donc ni l'un ni l'autre — la musique continue,
    ///   `active_source` garde le nom de la source qui a disparu, et la page de
    ///   statut dit la vérité entière (« radio », active, non joint).
    ///
    /// Ce qui est quand même oublié : les présélections nommées (le catalogue ne
    /// doit pas proposer d'agir sur un greffon mort) et, si c'était l'active, les
    /// deux **capacités** qu'elle avait déclarées — `preset_count` et
    /// `can_eject`. Celles-là décrivent ce qu'un greffon sait faire, et il n'est
    /// plus là pour le faire : laisser la touche Eject allumée ou la grille de
    /// présélections ouverte donnerait des commandes qui ne peuvent plus aboutir.
    /// `bascule_source` les efface déjà pour ce motif exact.
    ///
    /// Ce qui est gardé, et c'est aussi voulu : `source_status` et l'identité de
    /// ce qui joue. Elles décrivent **le morceau en cours**, qui joue encore ;
    /// les effacer noircirait l'afficheur au milieu d'un titre. `persist()` n'est
    /// pas appelée : `active_source` n'a pas changé, et l'état sur disque nomme
    /// donc toujours la source que l'utilisateur a choisie — au prochain
    /// démarrage le greffon est relancé et la retrouve.
    ///
    /// Non-`async` : c'est la conséquence directe de ne pas basculer. Aucun
    /// `Deactivate` à envoyer (le pair est mort), aucun `Activate` à attendre.
    pub fn oublie_source_morte(&mut self, name: &str) -> bool {
        let Some(pos) = self.source_order.iter().position(|n| n == name) else {
            return false;
        };
        self.sources.remove(name);
        self.source_order.remove(pos);
        self.presets_par_source.remove(name);
        if self.active_source == name {
            self.preset_count = None;
            self.can_eject = false;
        }
        self.publie_catalogue();
        // Publier l'état aussi : `can_eject` et `preset_count` en font partie, et
        // aucun autre chemin ne le fera — ce bras-ci n'est pas une commande.
        self.publie_etat();
        true
    }

    /// Retire une source décâblée — un greffon qu'on vient d'éteindre depuis
    /// l'IHM. Rend `false` si ce nom n'était pas une source.
    ///
    /// **À ne pas confondre avec `oublie_source_morte`**, qui traite la mort
    /// *subie* du même greffon : celle-là ne bascule pas et n'arrête pas le
    /// lecteur. La doc de l'autre porte la comparaison des deux chemins.
    ///
    /// Si c'était l'active, la **suivante du cycle** prend sa place, ou aucune
    /// s'il n'en reste pas : `demande_active` tolère déjà l'absence de source, et
    /// démarrer sans source est légitime depuis l'enregistrement à chaud.
    ///
    /// L'ordre est délicat : la bascule doit avoir lieu **avant** le retrait de la
    /// table, parce que c'est elle qui envoie `Deactivate` à la source sortante —
    /// retirée d'abord, elle ne recevrait rien et le greffon garderait son état
    /// interne pour sa prochaine vie.
    pub async fn remove_source(&mut self, name: &str) -> Result<bool> {
        let Some(pos) = self.source_order.iter().position(|n| n == name) else {
            return Ok(false);
        };
        if self.active_source == name {
            let suivante = if self.source_order.len() > 1 {
                Some(self.source_order[(pos + 1) % self.source_order.len()].clone())
            } else {
                None
            };
            // Pas de `?` : la bascule peut échouer (l'entrante ne répond pas à
            // `Activate`, ou l'arrêt lui-même échoue), mais le retrait qui suit
            // doit avoir lieu quand même. Un greffon qu'on éteint doit finir
            // entièrement décâblé — jamais à moitié, avec un `SourceCycle` qui
            // pourrait encore retomber sur un processus qui n'existe plus —
            // c'est tout le principe d'un accusé qui ne décrit qu'un état déjà
            // vrai.
            if let Err(e) = self.bascule_source(suivante.clone()).await {
                tracing::warn!("switching away from {name} while removing it: {e:#}");
                // `bascule_source` pose `active_source` **avant** son étage qui
                // peut échouer (`Activate`) mais **après** un `stop()` qui peut
                // lui aussi échouer : selon l'étage en cause, `active_source`
                // peut encore nommer la source qu'on est en train de retirer de
                // la table. La reposer ici est sans risque dans les deux cas.
                self.active_source = suivante.unwrap_or_default();
            }
        }
        self.sources.remove(name);
        self.source_order.remove(pos);
        // Les présélections nommées de la source partent avec elle, et le
        // catalogue est republié dans la foulée.
        //
        // Ce n'est pas du ménage : le catalogue est le seul canal par lequel un
        // client MPD apprend qu'une liste enregistrée existe. Laissée en place,
        // l'entrée ferait figurer dans `listplaylists` une source qui n'existe
        // plus, et un client pourrait **agir** dessus — un `load "radio"` sur un
        // greffon éteint. Le garde de `Command::SelectSource` le refuserait
        // (`source_order` ne porte plus le nom), mais l'utilisateur, lui, verrait
        // une liste qui ment jusqu'au redémarrage : les clients MPD mettent
        // volontiers `listplaylists` en cache.
        //
        // `source_order` est vidé juste au-dessus, donc `catalogue()` ne cite
        // déjà plus cette source ; retirer aussi la table évite qu'un greffon
        // rallumé sous le même nom hérite silencieusement de la liste de sa vie
        // précédente au lieu d'attendre son propre `ListPresets` (voir
        // `cable_source_a_chaud`).
        self.presets_par_source.remove(name);
        self.publie_catalogue();
        Ok(true)
    }

    /// Câble une source qui s'annonce **après** le démarrage. Renvoie `true`
    /// s'il s'agit d'un remplacement (ré-annonce d'un greffon déjà câblé).
    ///
    /// Deux chemins, et c'est tout l'intérêt de les tenir ensemble ici :
    ///
    /// - **Première source du cœur** (la table était vide) : le démarrage est
    ///   rejoué par `resume`, donc `SetLocale` puis `Wake`, dans cet ordre.
    ///   `add_source` ne fait que désigner l'active ; sans ce réveil, une source
    ///   arrivée à t+30 s serait active et **muette** jusqu'à ce que
    ///   l'utilisateur touche quelque chose — l'appareil aurait l'air en panne
    ///   alors que tout est câblé.
    /// - **Source supplémentaire, ou cœur en veille** : seule la langue est due.
    ///   Réveiller ici rallumerait un appareil qu'on a volontairement éteint, et
    ///   changerait ce qui joue parce qu'un greffon a fini de démarrer.
    ///
    /// L'état est publié dans les deux cas : le nom de la source vient
    /// d'apparaître dans la trame, et la SPA comme les afficheurs annonçaient
    /// jusque-là « aucune source ». (`resume` publie déjà pour le premier.)
    pub async fn cable_source_a_chaud(
        &mut self,
        name: String,
        client: Arc<dyn Source>,
    ) -> Result<bool> {
        let premiere = self.sources.is_empty();
        let remplacement = self.add_source(name.clone(), client);
        if premiere && !self.standby {
            self.resume().await?;
        } else {
            self.envoie_locale_a(&name).await;
            self.publie_etat();
        }
        Ok(remplacement)
    }

    /// Pousse la langue courante à **une seule** source : celle qui vient
    /// d'être câblée à chaud.
    ///
    /// `resume` et `set_locale` ne servent que les sources présentes dans la
    /// table au moment de leur appel. Une source arrivée après — greffon qui a
    /// raté le rendez-vous, ou relancé à la main sans son argument de langue —
    /// n'aurait jamais reçu `SetLocale` : sur un appareil en français, un `cd`
    /// relancé revenait en affichant `NO DISC` dans sa ligne de statut, et le
    /// serait resté jusqu'au prochain changement de langue.
    ///
    /// Sans effet si le cœur n'a pas de langue réglée : le greffon garde alors
    /// son défaut, qui est le même que celui du cœur. Best-effort comme les
    /// deux autres chemins — une source qui ne répond pas à `SetLocale` ne doit
    /// pas empêcher son câblage.
    pub async fn envoie_locale_a(&self, name: &str) {
        let Some(locale) = self.locale.clone() else {
            return;
        };
        if let Some(src) = self.sources.get(name) {
            if let Err(e) = src.request(SourceReq::SetLocale(locale)).await {
                tracing::warn!("SetLocale to {name}: {e}");
            }
        }
    }

    /// Remplace l'ordre d'arbitrage des plugins `metadata`.
    ///
    /// Appelé après chaque annonce tardive avec la liste **complète**
    /// recalculée depuis le manifeste : la priorité est celle de
    /// `plugins.toml`, jamais celle d'arrivée des annonces.
    pub fn set_metadata_order(&mut self, ordre: Vec<String>) {
        self.metadonnees.set_ordre(ordre);
    }

    pub(super) async fn apply(&mut self, action: SourceAction) -> Result<()> {
        match action {
            SourceAction::Noop => {}
            SourceAction::Play { uri, start, finite, playlist } => {
                // La machinerie de relance (`expecting_stream` puis
                // `PlaybackIdle` → retry) n'existe que pour les flux réseau :
                // un contenu qui se termine est une fin normale, pas une
                // panne. Le confondre avec une coupure faisait redémarrer le
                // disque en boucle : fin du disque → mpv idle → relance ~2 s
                // → `Activate` → `Play cdda://` → piste 1.
                //
                // C'est la Source qui le déclare, et non le cœur qui le
                // devine : celui-ci reniflait `cdda://`, si bien qu'un chemin
                // de fichier — mesuré au banc, mpv passant `idle` en fin de
                // liste exactement comme lors d'une coupure — tombait du
                // mauvais côté.
                self.expecting_stream = !finite;
                self.lecture = true;
                // Seul endroit où `lecture` passe à vrai : c'est ici, et
                // nulle part ailleurs, que `paused` doit retomber, sans quoi
                // une pause d'hier rendrait une lecture neuve « en pause ».
                self.paused = false;
                // `loadlist` pour une liste, `loadfile` pour un média : c'est la
                // Source qui le déclare, et le cœur ne le devine pas. Un `.m3u8`
                // est une liste pour un lecteur de fichiers et un flux HLS pour
                // une radio ; renifler l'URI casserait l'un ou l'autre.
                if playlist {
                    self.player.load_list(&uri).await?;
                } else {
                    self.player.play(&uri).await?;
                }
                // Positionnement après le chargement, et cet ordre n'est sûr que
                // grâce à `loadlist` : avec `loadfile`, mpv ne déplie la liste
                // qu'après coup — mesuré — et cet index tombait hors bornes
                // avant que la lecture ne reparte de la première piste.
                if let Some(n) = start {
                    self.player.set_playlist_pos(n).await?;
                }
            }
            SourceAction::Stop => {
                self.expecting_stream = false;
                self.lecture = false;
                self.player.stop().await?;
            }
            SourceAction::PlayerNext => self.player.next().await?,
            SourceAction::PlayerPrev => self.player.prev().await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;
    use std::sync::Mutex;

    /// Source qui n'a jamais rien à jouer : un lecteur cd sans disque.
    struct SourceVide;

    #[async_trait::async_trait]
    impl Source for SourceVide {
        async fn request(&self, _req: SourceReq) -> Result<SourceAction> {
            Ok(SourceAction::Noop)
        }
    }

    /// Source dont l'activation échoue — un plugin bloqué, que le SDK
    /// sanctionne par un timeout.
    struct SourceEnPanne;

    #[async_trait::async_trait]
    impl Source for SourceEnPanne {
        async fn request(&self, req: SourceReq) -> Result<SourceAction> {
            match req {
                SourceReq::Activate => anyhow::bail!("timeout"),
                _ => Ok(SourceAction::Noop),
            }
        }
    }

    #[test]
    fn active_source_retourne_la_source_courante() {
        let (core, _pc, _sc, _rx, _d) = setup();
        // PersistedState::default().active_source == "radio".
        assert_eq!(core.active_source(), "radio");
    }

    #[test]
    fn add_source_retrie_lordre_du_cycle_au_lieu_dajouter_en_queue() {
        // `SourceCycle` suit l'ordre alphabétique. Une source arrivée en retard
        // qui resterait en queue ferait changer le sens du cycle selon la
        // chronologie du démarrage — l'utilisateur presserait la même touche et
        // n'obtiendrait pas la même source d'un jour à l'autre.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let nouvelle = Arc::new(FakeSource { name: "files", calls: source_calls });
        assert!(!core.add_source("files".into(), nouvelle), "ce n'est pas un remplacement");
        assert_eq!(core.source_order, vec!["cd".to_string(), "files".into(), "radio".into()]);
        assert_eq!(
            core.active_source(),
            "radio",
            "une source deja active ne doit pas etre supplantee par une arrivee tardive"
        );
    }

    #[test]
    fn add_source_signale_un_remplacement_sans_dupliquer_lordre() {
        // Ré-annonce d'un greffon déjà câblé : le client est remplacé, le cycle
        // ne gagne pas une entrée en double.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let remplacant = Arc::new(FakeSource { name: "radio", calls: source_calls });
        assert!(core.add_source("radio".into(), remplacant));
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
        assert_eq!(core.active_source(), "radio");
    }

    #[test]
    fn add_source_active_la_premiere_source_et_seulement_la_premiere() {
        // Le seul cas où l'arrivée d'un greffon change ce qui joue : aucune
        // source n'avait répondu au démarrage, donc rien n'était actif.
        let (mut core, _rx, dir) = setup_sans_source();
        assert_eq!(core.active_source(), "");
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.add_source("radio".into(), Arc::new(FakeSource { name: "radio", calls: calls.clone() }));
        assert_eq!(core.active_source(), "radio");
        // La deuxième n'y touche pas, même si son nom passe avant dans l'ordre.
        core.add_source("cd".into(), Arc::new(FakeSource { name: "cd", calls }));
        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
        drop(dir);
    }

    #[tokio::test]
    async fn remove_source_bascule_sur_la_suivante() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        assert_eq!(core.active_source(), "radio");

        assert!(core.remove_source("radio").await.unwrap());

        assert_eq!(core.active_source(), "cd", "la suivante du cycle prend la place");
        assert_eq!(core.source_order, vec!["cd".to_string()]);
        let calls = source_calls.lock().unwrap();
        assert!(
            calls.iter().any(|c| c == "radio:Deactivate"),
            "la sortante est prévenue avant de disparaître : {calls:?}"
        );
        assert!(calls.iter().any(|c| c == "cd:Activate"), "l'entrante est activée : {calls:?}");
    }

    #[tokio::test]
    async fn une_reponse_de_preselections_en_retard_ne_ressuscite_pas_une_source_retiree() {
        // La course : `ListPresets` est détaché, donc sa réponse peut arriver
        // après l'extinction du greffon. Sans protection, elle réinsérait
        // l'entrée que `remove_source` venait d'évincer — et le catalogue
        // recommençait à annoncer à un client MPD une liste enregistrée sur
        // laquelle il pouvait agir. C'est exactement le défaut que l'éviction
        // existe pour empêcher.
        //
        // **Ce qui protège est le retour anticipé en tête de
        // `handle_source_update`** (`!self.sources.contains_key(name)`), et non
        // une garde posée près de l'insertion. Ce test existe parce que rien ne
        // l'épinglait : le retour anticipé est arrivé pour la trame *entière*,
        // et sa doc décrit bien ce cas, mais aucune assertion ne l'aurait
        // empêché de disparaître. Vérifié par mutation : le retirer fait tomber
        // ce test.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(core.remove_source("radio").await.unwrap());
        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string()]);

        // La réponse en retard arrive maintenant, sur un nom que le cœur ne
        // câble plus.
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(5, "OUI FM")]));

        assert!(
            !core.presets_par_source.contains_key("radio"),
            "une source retirée ne doit pas revenir par une réponse en vol"
        );
        assert_eq!(
            noms(&core.catalogue()),
            vec!["cd".to_string()],
            "et le catalogue ne doit pas la réannoncer"
        );
    }

    #[tokio::test]
    async fn retirer_une_source_la_sort_du_catalogue_avec_ses_preselections() {
        // Fusion des deux chantiers : `remove_source` (extinction à chaud d'un
        // greffon) est arrivé par un côté, `presets_par_source` et le canal de
        // catalogue par l'autre — et rien ne les reliait. Laissée en place,
        // l'entrée faisait figurer dans le `listplaylists` d'un client MPD une
        // source éteinte, sur laquelle il pouvait **agir** : le `load` serait
        // refusé par le garde de `SelectSource`, mais l'utilisateur verrait une
        // liste qui mente jusqu'au redémarrage, les clients MPD mettant
        // volontiers ce catalogue en cache.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(5, "OUI FM")]));
        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string(), "radio".into()]);

        assert!(core.remove_source("radio").await.unwrap());

        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string()], "la source sort du catalogue");
        assert!(
            !core.presets_par_source.contains_key("radio"),
            "ses présélections partent avec elle : un greffon rallumé sous le même nom \
             doit attendre son propre ListPresets, pas hériter de sa vie précédente"
        );
    }

    #[tokio::test]
    async fn une_source_disparue_ne_recoit_plus_de_bascule_et_sort_du_catalogue() {
        // **Le danger commun aux deux chemins de disparition d'un greffon.** Un
        // greffon disparu qui laissait son nom dans `source_order` et ses
        // présélections dans `presets_par_source` faisait garder à un client MPD
        // sa liste enregistrée en cache, et un `load` dessus **passait** le garde
        // de `SelectSource`. La bascule partait alors vers un socket mort et
        // payait jusqu'à deux délais de 5 s du protocole des sources —
        // `Deactivate` puis `Activate` — dans la boucle principale, muette
        // pendant ce temps. Ce test-ci prend le chemin volontaire
        // (`remove_source`) ; son jumeau juste en dessous prend celui de la mort
        // subie (`oublie_source_morte`), et c'est leur *différence* qui est
        // épinglée là-bas.
        //
        // Le test épingle les deux moitiés à la suite : la sortie du catalogue,
        // et le fait qu'un `SelectSource` sur ce nom ne parle plus à personne.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert!(noms(&core.catalogue()).contains(&"radio".to_string()));

        // Ce que fait le bras `plugin_waits` quand la mort n'était pas voulue.
        assert!(core.remove_source("radio").await.unwrap());
        // La bascule vers « cd » a déjà eu lieu et a parlé : on ne veut observer
        // que ce qui suit.
        source_calls.lock().unwrap().clear();

        // Ce qu'un client MPD envoie encore, son catalogue étant en cache.
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();

        let appels = source_calls.lock().unwrap().clone();
        assert!(
            appels.is_empty(),
            "aucune requete ne doit partir apres la disparition de la source, obtenu {appels:?}"
        );
        assert_eq!(core.active_source(), "cd", "et ce qui joue n'a pas bouge");
        assert!(
            !noms(&core.catalogue()).contains(&"radio".to_string()),
            "la source disparue ne doit plus figurer au catalogue"
        );
        assert!(!core.presets_par_source.contains_key("radio"));
    }

    #[tokio::test]
    async fn la_mort_subie_du_greffon_actif_evince_sans_arreter_la_musique_ni_changer_de_source() {
        // **La décision du constat 3, épinglée.** Le bras de sortie de processus
        // appelait `remove_source`, qui bascule quand c'était l'active : une
        // panique du greffon radio arrêtait donc mpv et affichait « cd » sur un
        // appareil dont l'utilisateur avait choisi la radio. Or un greffon de
        // Source est un *contrôleur* — le flux est tenu par mpv, enfant du cœur,
        // que la mort du greffon ne touche pas.
        //
        // Trois propriétés dans un seul test, parce que c'est leur conjonction
        // qui est la décision : rien ne s'arrête, rien ne bascule, et le
        // catalogue oublie quand même.
        let (mut core, player_calls, source_calls, etat_rx, _d) = setup();
        core.handle_command(Command::PlayPause).await.unwrap(); // la radio joue
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));
        assert_eq!(etat_rx.borrow().playback, Playback::Playing);
        player_calls.lock().unwrap().clear();
        source_calls.lock().unwrap().clear();

        assert!(core.oublie_source_morte("radio"));

        assert_eq!(
            core.active_source(),
            "radio",
            "personne n'a demande de changer de source : le nom affiche doit rester celui \
             que l'utilisateur a choisi, greffon mort ou non"
        );
        assert_eq!(
            etat_rx.borrow().playback,
            Playback::Playing,
            "la panne d'un controleur ne doit pas faire taire mpv, qui n'est pas dans le greffon"
        );
        assert!(
            player_calls.lock().unwrap().is_empty(),
            "aucun ordre au lecteur : obtenu {:?}",
            player_calls.lock().unwrap()
        );
        assert!(
            source_calls.lock().unwrap().is_empty(),
            "ni Deactivate ni Activate : le pair est mort et l'autre source n'a rien demande, \
             obtenu {:?}",
            source_calls.lock().unwrap()
        );
        // Et l'eviction, elle, a bien eu lieu : c'est la moitie commune aux deux
        // chemins.
        assert_eq!(noms(&core.catalogue()), vec!["cd".to_string()]);
        assert!(!core.presets_par_source.contains_key("radio"));
        // Les capacites de la source morte sont oubliees : une touche Eject
        // allumee ou une grille de preselections ouverte proposeraient des
        // commandes qui ne peuvent plus aboutir.
        assert!(!etat_rx.borrow().can_eject);
        assert_eq!(etat_rx.borrow().preset_count, None);
    }

    #[tokio::test]
    async fn apres_la_mort_de_la_source_active_la_touche_source_repart_de_la_premiere() {
        // Le corollaire de la décision ci-dessus : `active_source` ne figure plus
        // dans `source_order`, et `SourceCycle` doit quand même mener quelque
        // part d'utile. Un `position().unwrap_or(0)` suivi d'un `+ 1` sautait la
        // première source, qui devenait inatteignable au clavier.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let files = Arc::new(FakeSource { name: "files", calls: source_calls });
        core.add_source("files".into(), files);
        assert_eq!(core.source_order, vec!["cd".to_string(), "files".into(), "radio".into()]);
        assert!(core.oublie_source_morte("radio"));

        core.handle_command(Command::SourceCycle).await.unwrap();

        assert_eq!(core.active_source(), "cd", "la premiere source restante, pas la seconde");
    }

    #[tokio::test]
    async fn une_reponse_de_catalogue_encore_en_vol_ne_ressuscite_pas_une_source_evincee() {
        // Le fan-out des `ListPresets` est **détaché** : la requête part dans sa
        // propre tâche, et `remove_source` peut s'exécuter entre elle et sa
        // réponse. Cette réponse-là arrive donc pour de vrai après l'éviction, et
        // `presets_par_source.insert` se fait délibérément **avant** le garde de
        // source active (le catalogue décrit toutes les sources, pas celle qui
        // joue) : la liste était donc ré-insérée après coup, le catalogue
        // republié annonçait une liste enregistrée pour une source qui n'existe
        // plus, et un client MPD pouvait `load` dessus.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(core.remove_source("radio").await.unwrap());
        assert!(!noms(&core.catalogue()).contains(&"radio".to_string()));

        // La réponse en vol, telle que le `SourceClient` la relaie : une liste
        // non vide, sans identité ni statut — la forme exacte qu'une trame de
        // `ListPresets` prend sur le fil.
        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(5, "OUI FM")]));

        assert!(
            !core.presets_par_source.contains_key("radio"),
            "une reponse pour une source que le coeur ne connait plus doit etre jetee"
        );
        assert!(
            !noms(&core.catalogue()).contains(&"radio".to_string()),
            "et le catalogue ne doit pas la faire reapparaitre"
        );
    }

    #[tokio::test]
    async fn une_reponse_de_catalogue_pour_une_source_inactive_mais_vivante_est_toujours_prise() {
        // Le pendant du test ci-dessus, et il est nécessaire : un garde trop
        // large aurait aussi jeté les listes des sources **vivantes mais non
        // actives**, ce qui est justement le cas que `presets_par_source` existe
        // pour servir — `listplaylistinfo "radio"` pendant que le cd joue.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(core.active_source(), "cd");

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP")]));

        assert_eq!(
            core.presets_par_source.get("radio").map(|p| p.len()),
            Some(1),
            "la source n'est pas active, mais elle existe : sa liste doit entrer au catalogue"
        );
    }

    #[tokio::test]
    async fn le_catalogue_est_republie_quand_une_source_est_retiree() {
        // Le retrait ne suffit pas : sans la publication, les afficheurs déjà
        // connectés garderaient la version précédente du catalogue — le canal
        // étant `watch`, personne ne la redemande.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut cat_rx = core.catalogue_tx.subscribe();
        cat_rx.borrow_and_update();

        assert!(core.remove_source("radio").await.unwrap());

        assert!(cat_rx.has_changed().unwrap(), "le canal du catalogue doit avoir bougé");
        assert_eq!(noms(&cat_rx.borrow_and_update()), vec!["cd".to_string()]);
    }

    #[tokio::test]
    async fn desactiver_la_source_active_republie_letat_sans_les_reliquats_de_la_sortante() {
        // Fix de revue finale : `bascule_source` est emprunté par
        // `remove_source` (donc par la désactivation à chaud d'un greffon)
        // en dehors de `handle_command`, seul endroit qui publiait jusqu'ici.
        // Sans un `publie_etat` propre à `bascule_source`, la trame reçue par
        // la SPA et les afficheurs continuait de nommer la source sortante,
        // avec son compte de présélections, son statut et sa capacité
        // d'éjection.
        let (mut core, _pc, _sc, etat_rx, _d) = setup();
        core.handle_source_update(
            "radio",
            SourceUpdate {
                identity: Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream"}))),
                transient: false,
                preset: Some(3),
                preset_count: Some(23),
                preset_name: Some("France Inter".into()),
                status: Some("EN DIRECT".into()),
                can_eject: Some(true),
                presets: None,
                cover: None,
            },
        );
        assert_eq!(etat_rx.borrow().source, "radio");
        assert_eq!(etat_rx.borrow().preset_count, Some(23));
        assert!(etat_rx.borrow().can_eject);

        assert!(core.remove_source("radio").await.unwrap());

        let etat = etat_rx.borrow();
        assert_eq!(etat.source, "cd", "la trame doit nommer l'entrante, pas la sortante");
        assert_eq!(etat.preset_count, None, "le compte de preselections de la sortante ne doit pas survivre");
        assert_eq!(etat.status, None, "le statut de la sortante ne doit pas survivre");
        assert!(!etat.can_eject, "la capacite d'ejection decrit la sortante, pas l'entrante");
    }

    #[tokio::test]
    async fn remove_source_de_la_derniere_laisse_le_coeur_sans_source() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(core.remove_source("cd").await.unwrap());
        assert!(core.remove_source("radio").await.unwrap());

        // Aucune source est un état légitime : `demande_active` le tolère, et
        // démarrer sans source est accepté depuis l'enregistrement à chaud.
        assert_eq!(core.active_source(), "");
        assert!(core.source_order.is_empty());
        // Et une commande dans cet état ne panique pas.
        core.handle_input(InputMessage::from(Command::Next)).await.unwrap();
    }

    #[tokio::test]
    async fn remove_source_dune_source_inactive_ne_touche_pas_a_ce_qui_joue() {
        let (mut core, player_calls, _sc, _rx, _d) = setup();

        assert!(core.remove_source("cd").await.unwrap());

        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["radio".to_string()]);
        assert!(
            !player_calls.lock().unwrap().iter().any(|c| c == "stop"),
            "retirer une source inactive n'arrête pas ce qui joue"
        );
    }

    #[tokio::test]
    async fn remove_source_dun_nom_inconnu_est_un_non_evenement() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        assert!(!core.remove_source("jamais-vu").await.unwrap());
        assert_eq!(core.active_source(), "radio");
        assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
    }

    #[tokio::test]
    async fn remove_source_reste_complet_quand_lentrante_echoue_a_lactivation() {
        // Retirer la source active bascule vers la suivante du cycle ; ici la
        // suivante est "casse", dont `Activate` échoue systématiquement (voir
        // `FakeSource::request`). Le retrait doit malgré tout être complet :
        // un greffon qu'on éteint ne doit jamais rester à moitié câblé, avec
        // un `SourceCycle` qui pourrait retomber sur un processus déjà tué.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.add_source("casse".into(), Arc::new(FakeSource { name: "casse", calls: source_calls }));
        assert_eq!(core.source_order, vec!["casse".to_string(), "cd".into(), "radio".into()]);
        assert_eq!(core.active_source(), "radio");

        assert!(
            core.remove_source("radio").await.unwrap(),
            "le retrait a bien lieu malgré l'échec de la bascule vers l'entrante"
        );

        assert!(
            !core.sources.contains_key("radio"),
            "la source tuée ne doit plus figurer dans la table, même si la bascule a échoué"
        );
        assert!(!core.source_order.contains(&"radio".to_string()));
        assert_ne!(
            core.active_source(),
            "radio",
            "le cœur ne doit plus nommer une source qu'il vient de retirer de sa table"
        );
    }

    #[tokio::test]
    async fn une_source_cablee_a_chaud_recoit_la_langue_courante() {
        // `resume` et `set_locale` ne servent que les sources présentes dans la
        // table au moment de leur appel. Sans ce chemin-là, une source arrivée
        // après n'aurait jamais reçu `SetLocale` : sur un appareil en français,
        // un `cd` relancé à la main revenait en affichant `NO DISC`.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.set_locale("fr".into()).await.unwrap();

        let tardives: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud(
            "files".into(),
            Arc::new(FakeSource { name: "files", calls: tardives.clone() }),
        )
        .await
        .unwrap();

        // La langue, et **rien d'autre** : `files` n'est pas la première source
        // du cœur, donc elle n'est pas réveillée — ce qui joue ne change pas
        // parce qu'un greffon a fini de démarrer.
        assert_eq!(
            tardives.lock().unwrap().as_slice(),
            ["files:SetLocale(\"fr\")".to_string()]
        );
        assert_eq!(core.active_source(), "radio");
        assert_eq!(
            source_calls.lock().unwrap().iter().filter(|c| c.starts_with("radio:SetLocale")).count(),
            1,
            "seule la source cablee a chaud est concernee, les autres ne sont pas renotifiees"
        );
    }

    #[tokio::test]
    async fn sans_langue_reglee_rien_nest_pousse_a_la_source_cablee_a_chaud() {
        // Aucune langue côté cœur : le greffon garde son défaut, qui est le
        // même. Pousser `SetLocale(None)` n'existe pas, et pousser « en » de
        // force écraserait un greffon lancé avec sa propre langue.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let tardives: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud(
            "files".into(),
            Arc::new(FakeSource { name: "files", calls: tardives.clone() }),
        )
        .await
        .unwrap();
        assert!(tardives.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn la_premiere_source_cablee_a_chaud_est_reveillee() {
        // `add_source` ne fait que désigner l'active : ni `SetLocale`, ni `Wake`,
        // ni `Activate`. Une source arrivée à t+30 s serait donc active et
        // **muette** jusqu'à ce que l'utilisateur touche quelque chose —
        // l'appareil aurait l'air en panne alors que tout est câblé.
        let (mut core, mut etat_rx, dir) = setup_sans_source();
        core.set_locale("fr".into()).await.unwrap();
        let vus: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        assert!(
            !core
                .cable_source_a_chaud(
                    "radio".into(),
                    Arc::new(FakeSource { name: "radio", calls: vus.clone() })
                )
                .await
                .unwrap(),
            "premier cablage, pas un remplacement"
        );

        assert_eq!(
            vus.lock().unwrap().as_slice(),
            ["radio:SetLocale(\"fr\")".to_string(), "radio:Wake".into()],
            "la langue AVANT le reveil, exactement comme au demarrage"
        );
        // Le `Play` renvoyé par `Wake` a bien été appliqué : quelque chose joue.
        assert!(core.player.calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert_eq!(etat_rx.borrow_and_update().source, "radio");
        drop(dir);
    }

    #[tokio::test]
    async fn la_premiere_source_cablee_a_chaud_ne_reveille_pas_un_coeur_en_veille() {
        // La veille est un état **voulu** : l'arrivée d'un greffon ne rallume pas
        // l'appareil. Seule la langue est due, pour que la source ne compose pas
        // sa première trame dans la langue de son lancement.
        let (mut core, _rx, dir) = setup_sans_source();
        core.set_locale("fr".into()).await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        let vus: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud(
            "radio".into(),
            Arc::new(FakeSource { name: "radio", calls: vus.clone() }),
        )
        .await
        .unwrap();

        assert_eq!(vus.lock().unwrap().as_slice(), ["radio:SetLocale(\"fr\")".to_string()]);
        assert!(
            !core.player.calls.lock().unwrap().iter().any(|c| c.starts_with("play")),
            "rien ne doit se mettre a jouer pendant la veille"
        );
        drop(dir);
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
    async fn le_cycle_de_source_se_comporte_exactement_comme_avant_lextraction() {
        // Filet de l'extraction : le corps a change de fonction, pas de sens.
        // Memes assertions que `source_cycle_bascule_et_persiste`, la preuve
        // que basculer_vers rejoue exactement le comportement du bloc qu'elle
        // remplace.
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Deactivate"));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Activate"));
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
    }

    #[tokio::test]
    async fn la_source_par_son_nom_bascule_comme_le_cycle() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
        assert_eq!(core.active_source(), "cd");
    }

    #[tokio::test]
    async fn une_source_inconnue_est_ignoree_sans_rien_couper() {
        // La garde qui compte : sans elle, un nom errant viderait la source active.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SelectSource("nexistepas".into())).await.unwrap();
        assert_eq!(core.active_source(), "radio");
    }

    #[tokio::test]
    async fn selectionner_la_source_deja_active_ne_coupe_pas_ce_qui_joue() {
        // C'est exactement ce qu'un client MPD envoie en rouvrant son ecran : un
        // `load` redondant ne doit pas arreter la lecture.
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        // La bascule complete (stop puis Activate) ramenerait aussi a `Playing`
        // pour cette source factice : le champ `playback` seul ne distingue pas
        // un redondant traite en no-op d'un redondant qui a coupe puis relance.
        // L'absence de tout nouvel appel `stop` est la preuve qui bite.
        player_calls.lock().unwrap().clear();
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();
        assert_eq!(core.etat_lecteur().playback, Playback::Playing);
        assert!(
            !player_calls.lock().unwrap().iter().any(|c| c == "stop"),
            "un load redondant ne doit meme pas arreter puis relancer mpv"
        );
    }

    #[tokio::test]
    async fn changer_de_source_arrete_la_lecture_meme_si_la_nouvelle_na_rien_a_jouer() {
        // Régression (revue 2026-07-27) : l'action renvoyée par `Deactivate`
        // était ignorée et l'arrêt reposait sur le `Play` de l'`Activate`
        // suivant — que le cd sans disque ne renvoie pas (`Noop`). La radio
        // continuait de jouer sous un affichage qui annonçait « cd », titres
        // ICY compris.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        sources.insert("cd".into(), Arc::new(SourceVide));
        let (etat_tx, etat_rx) = watch::channel(PlayerState::default());
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let metadata = MetadataCablage {
            plugins: vec![],
            now_playing: watch::channel(NowPlaying { source: String::new(), identity: None, ..Default::default() }).0,
            etat: etat_tx,
        };
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata, catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        core.handle_command(Command::SourceCycle).await.unwrap();
        // C'est le cœur qui a arrêté mpv, sans dépendre des plugins.
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
        // Et un titre ICY en retard de l'ancien flux n'atteint plus personne :
        // plus aucun flux n'est attendu.
        core.handle_event(Event::IcyTitle("en retard".into())).await;
        assert_eq!(etat_rx.borrow().morceau.title, None);
    }

    #[tokio::test]
    async fn un_echec_dactivation_laisse_la_bascule_coherente() {
        // Régression (revue 2026-07-27) : `persist()` n'était appelé qu'après
        // un `Activate` réussi. Son échec laissait la bascule à moitié faite :
        // « cd » en mémoire et à l'écran, « radio » dans state.json, et
        // l'ancien flux toujours audible.
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        sources.insert("cd".into(), Arc::new(SourceEnPanne));
        let root = dir.path().to_path_buf();
        let catalog = Arc::new(tokio::sync::RwLock::new(ritornello_i18n::Catalog::load("core", "en", &root, crate::i18n::EN)));
        let (covers, pochette_tx) = covers_de_test();
        let mut core = Core::new(player, Cablage { sources, persisted: PersistedState::default(), state_path: dir.path().join("state.json"), catalog, locales_root: root, metadata: cablage_muet(vec![]), catalogue: watch::channel(Catalogue::default()).0 }, covers, pochette_tx, mpsc::channel(4).0);
        core.resume().await.unwrap();
        assert!(core.handle_command(Command::SourceCycle).await.is_err());
        // L'état est cohérent : nouvelle source partout, et rien ne joue.
        assert_eq!(core.active_source(), "cd");
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
    }

    #[tokio::test]
    async fn une_source_cablee_a_chaud_entre_dans_le_catalogue() {
        // Un greffon qui a rate le rendez-vous doit apparaitre dans la liste que
        // les clients interrogent, sans redemarrage — donc `add_source` publie.
        let (mut core, _rx, dir) = setup_sans_source();
        let mut cat_rx = core.catalogue_tx.subscribe();
        assert!(core.catalogue().sources.is_empty(), "aucune source au demarrage");
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud("radio".into(), Arc::new(FakeSource { name: "radio", calls }))
            .await
            .unwrap();
        assert!(cat_rx.has_changed().unwrap(), "les afficheurs doivent l'apprendre");
        assert_eq!(noms(&cat_rx.borrow_and_update()), vec!["radio".to_string()]);
        drop(dir);
    }

    #[tokio::test]
    async fn une_source_cablee_a_chaud_finit_avec_ses_preselections() {
        // Le chemin complet du greffon qui a rate le rendez-vous : il entre dans
        // le catalogue avec une liste vide, puis sa reponse a `ListPresets` — que
        // le cablage a chaud demande desormais, comme le demarrage — la remplit.
        //
        // La source cablee en second n'est **pas** l'active, ce qui est le cas
        // reel (une `radio` tardive pendant que le `cd` joue) : la liste doit donc
        // franchir le garde de source active, et la publication doit remplacer la
        // liste vide au lieu d'etre dedoublonnee.
        let (mut core, _rx, dir) = setup_sans_source();
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        core.cable_source_a_chaud("cd".into(), Arc::new(FakeSource { name: "cd", calls: calls.clone() }))
            .await
            .unwrap();
        core.cable_source_a_chaud("radio".into(), Arc::new(FakeSource { name: "radio", calls }))
            .await
            .unwrap();
        assert_eq!(core.active_source(), "cd", "la premiere cablee reste l'active");
        let mut cat_rx = core.catalogue_tx.subscribe();
        assert_eq!(noms(&cat_rx.borrow()), vec!["cd".to_string(), "radio".into()]);

        core.handle_source_update("radio", avec_presets(vec![pres(1, "FIP"), pres(9, "OUI FM")]));
        assert!(cat_rx.has_changed().unwrap(), "les afficheurs doivent l'apprendre");
        let cat = cat_rx.borrow_and_update().clone();
        let radio = cat.sources.iter().find(|s| s.name == "radio").expect("radio est declaree");
        assert_eq!(radio.presets, vec![pres(1, "FIP"), pres(9, "OUI FM")]);
        drop(dir);
    }

    #[tokio::test]
    async fn le_statut_de_lancienne_source_ne_survit_pas_a_un_changement_de_source() {
        // Régression I2 (revue de branche) : `source_status` n'était effacé
        // qu'à la trame suivante de la nouvelle Source. Un "cd" sans disque
        // déclare "pas de disque" ; l'utilisateur bascule sur "radio" qui n'a
        // aucune présélection configurée (une trame transitoire ne touche pas
        // au statut mémorisé) : sans ce correctif, l'écran continuait
        // d'afficher "pas de disque" sous la source "radio".
        let (mut core, _pc, _sc, mut etat_rx, _d) = setup();
        core.resume().await.unwrap();
        let mut update = update_nu();
        update.status = Some("pas de disque".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("pas de disque"));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(
            etat_rx.borrow_and_update().status,
            None,
            "le statut de l'ancienne source ne doit pas survivre au changement de source"
        );
    }

    #[tokio::test]
    async fn changer_de_source_diffuse_la_nouvelle_source() {
        // Piege : `SourceCycle` appelle `set_identity(None)`, qui sort sans rien
        // publier quand l'identite etait **deja** nulle — cas du cd sans disque.
        // La source active a pourtant change. C'est ce qui justifie de publier a
        // la sortie de la commande plutot que depuis `set_identity`.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        assert_eq!(etat_rx.borrow().source, "");
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(etat_rx.borrow().source, "cd");
    }
}
