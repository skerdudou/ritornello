//! Metadonnees du morceau : identite declaree par la source, titres ICY, tags de fichier, enrichissements des greffons, pochettes et leur extraction.

use super::*;

impl<P: Player> Core<P> {
    /// Applique la sélection qu'une trame déclare : le numéro de présélection et
    /// son nom lisible. Convention « absent = garder la valeur courante », à
    /// l'inverse de `status`.
    ///
    /// Appelée par `applique_les_faits_declares` seule, qui la relaie aux deux
    /// sorties de `handle_source_update` : la trame qui recompose la vue
    /// l'applique **après** l'identité (`set_identity(None)` efface la sélection,
    /// une déclaration explicite doit gagner), celle qui ne fait qu'annoncer un
    /// fait l'applique avant de rendre la main. Deux copies de ces quatre lignes
    /// divergeraient.
    pub(super) fn applique_selection(&mut self, preset: Option<u8>, nom: Option<String>) {
        if let Some(p) = preset {
            self.preset = Some(p);
        }
        if let Some(n) = nom {
            self.preset_name = Some(n);
        }
    }

    /// Applique la pochette qu'une trame de Source déclare.
    ///
    /// La pochette suit la même convention que `preset`/`preset_count` :
    /// absente = rien de neuf, jamais « plus de pochette » — une Source n'en
    /// répète pas la déclaration sur chaque trame de statut qui suit (voir
    /// `SourceUpdate::cover`). C'est pourquoi `set_cover_de_source` ne doit être
    /// appelée que lorsque le champ vaut `Some`.
    ///
    /// **Appelée par `applique_les_faits_declares`**, exactement comme
    /// `applique_selection` et pour la même raison — c'est elle qui la relaie aux
    /// deux sorties de `handle_source_update`. Sur le chemin qui recompose
    /// la vue, l'appel vient **après** l'identité : `set_identity` remet à zéro
    /// tout ce que `Metadonnees` retenait, pochette de la Source comprise, donc
    /// une trame qui porterait à la fois une nouvelle identité et sa pochette
    /// doit laisser l'identité parler d'abord — sans quoi la pochette tout juste
    /// déclarée serait effacée dans la foulée par ce reset. C'est exactement le
    /// piège que le commentaire d'`applique_selection`, plus haut, signale déjà
    /// pour la sélection.
    ///
    /// Sur le chemin du retour anticipé, il n'y a par construction ni identité ni
    /// statut, donc l'ordre n'y a pas de sens. **C'est celui-là qui compte** : une
    /// pochette de Source arrive seule, en notification spontanée, donc elle passe
    /// par là presque toujours — le chemin qui recompose la vue ne sert qu'à la
    /// trame qui porterait une pochette *en même temps* qu'une identité ou un
    /// statut. Sans l'appel du retour anticipé, la pochette n'est pas « appliquée
    /// plus tard » : elle est perdue en silence, et c'est le défaut réel que la
    /// fusion du chantier des pochettes avait introduit.
    ///
    /// Appelée depuis `handle_source_update` et non depuis la boucle `select!`
    /// de `main` : la garde de tête (`standby || name != self.active_source`)
    /// doit s'appliquer à la pochette comme à tout le reste de la trame. Une
    /// source non active pourrait sinon faire apparaître sa pochette sur le
    /// morceau que joue la source **active**.
    ///
    /// `validee` ici, comme `Enrichment::cleaned` le fait sur l'autre canal :
    /// une pochette entre dans le cœur par deux portes, et la couche
    /// `ritornello-proto` — celle qui possède la validation de forme — ne
    /// gardait que l'une des deux. Rien n'était exploitable, les contrôles
    /// propres du cœur couvrant ce chemin, mais une règle de forme appliquée à
    /// une porte sur deux finit par diverger. Une référence refusée vaut « rien
    /// de neuf », jamais « plus de pochette » : c'est la convention du champ
    /// (voir `SourceUpdate::cover`), et effacer sur une trame mal formée
    /// retirerait l'image valide qu'une trame précédente avait déclarée.
    pub(super) fn applique_pochette_de_source(
        &mut self,
        cover: Option<ritornello_proto::CoverRef>,
        name: &str,
    ) {
        if let Some(cover) = cover.and_then(ritornello_proto::CoverRef::validee) {
            self.set_cover_de_source(Some(cover), name);
        }
    }

    /// Change ce qui joue : remet l'ardoise des métadonnées à zéro, prévient les
    /// plugins `metadata`, et rafraîchit affichage et état diffusé.
    ///
    /// `None` = plus rien ne joue. Le cœur ne regarde jamais **dans** l'identité :
    /// il la compare par égalité et la relaie telle quelle.
    pub(super) fn set_identity(&mut self, identity: Option<serde_json::Value>) {
        // « Plus rien ne joue » emporte la sélection courante avec lui : la
        // touche mise en évidence désigne **ce qui joue**, pas la dernière
        // pression. Fait avant le garde d'égalité : une identité déjà à
        // `None` (arrêt répété, bascule de source après un stop) doit quand
        // même laisser la sélection effacée.
        if identity.is_none() {
            self.preset = None;
            self.preset_name = None;
        }
        if !self.metadonnees.set_identity(identity) {
            return;
        }
        // Le morceau a changé : l'ancre du précédent ne doit pas continuer
        // d'avancer sous le titre du suivant. La dernière position publiée
        // doit disparaître avec elle, sans quoi la trame émise dans la
        // foulée porterait la position de l'ancien morceau sous le titre du
        // nouveau, jusqu'au prochain tick (jusqu'à une seconde).
        self.ancre_position = None;
        self.position_s = None;
        let np = NowPlaying {
            source: self.active_source.clone(),
            identity: self.metadonnees.identity().cloned(),
            // Toujours vide à cet instant précis (le reset ci-dessus vient
            // d'effacer tout ce que `Metadonnees` savait), mais lu depuis
            // `known()` plutôt qu'un `Known::default()` figé : la valeur
            // reste correcte si le reset venait un jour à changer, et
            // `publie_etat` republie ce même champ dès qu'il cesse d'être
            // vide.
            known: self.metadonnees.known(),
        };
        // Échec impossible en pratique : un `watch::Sender::send` n'échoue que
        // quand plus aucun récepteur ne vit, et `main` garde le sien pour
        // alimenter les connexions de plugins `metadata` à venir. De toute
        // façon sans conséquence sur la lecture : un `warn` suffirait à noyer
        // les journaux si aucun plugin metadata n'était déclaré.
        let _ = self.now_playing_tx.send(np);
        // L'ardoise a changé, donc l'affichage doit suivre — comme le font
        // `handle_icy_title` et `handle_enrichment`. Sans ce rafraîchissement,
        // `Command::Stop` laissait le titre du morceau arrêté **figé sur
        // l'afficheur** jusqu'à la prochaine action de l'utilisateur, alors que
        // la SPA, elle, se vidait correctement. `etat_lecteur` lit
        // `self.metadonnees.etat()` à chaque appel, donc ce seul `publie_etat`
        // suffit : plus besoin du second appel conditionnel à l'incrustation
        // qu'exigeait l'ancien canal de vues composées.
        self.publie_etat();
    }

    /// Titre annoncé par le flux lui-même (en-tête ICY vu par mpv).
    pub(super) fn handle_icy_title(&mut self, titre: String) {
        // Deux gardes, et **aucune** ne consulte l'identité : cette couche doit
        // fonctionner sans plugin `metadata` et même face à une Source qui ne
        // déclare aucune identité, sinon la seule couche qui marche toute seule
        // se taisait en silence.
        //
        // En veille, rien ne doit atteindre l'affichage — même garde que
        // `handle_source_update`. Le chemin est réel : `Command::Power` attend
        // la réponse de la Source à `Deactivate` (jusqu'à 5 s) pendant que mpv
        // joue encore, et un titre émis dans cet intervalle arrive après que la
        // vue de veille a été poussée.
        //
        // `expecting_stream` est ce que le cœur sait **de lui-même** de la
        // lecture : mis à vrai sur chaque `Play` qu'il applique, à faux sur
        // `Stop`. C'est ce qui empêche un titre en retard de s'afficher, et d'y
        // rester, après un arrêt.
        if self.standby || !self.expecting_stream {
            return;
        }
        if !self.metadonnees.set_icy(titre) {
            return;
        }
        self.publie_etat();
    }

    /// Tags portés par le fichier joué, tels que mpv les expose.
    ///
    /// Mêmes gardes que l'ICY, à une différence près qui est tout l'objet du
    /// champ `lecture` : la garde « ça joue » ne peut pas être
    /// `expecting_stream`, qui vaut **faux** précisément pendant la lecture
    /// d'un contenu fini — donc pendant la seule lecture où des tags de
    /// fichier existent. S'en servir aurait produit une couche qui ne
    /// s'affiche jamais, sans rien dans les journaux.
    pub(super) fn handle_file_tags(&mut self, morceau: ritornello_proto::Morceau) {
        if self.standby || !self.lecture {
            return;
        }
        if !self.metadonnees.set_tags(morceau) {
            return;
        }
        self.publie_etat();
    }

    /// Chemin du fichier que mpv a réellement ouvert (propriété `path`), pour
    /// en tirer la pochette embarquée. N'arme **qu'**une extraction détachée :
    /// voir `extraction_arrivee` pour la suite, à l'arrivée du résultat.
    ///
    /// Même garde « ça joue » que les tags (`lecture`, pas `expecting_stream`,
    /// pour la même raison) : `path` est republié aussi bien pour un flux que
    /// pour un fichier.
    ///
    /// **Le cœur complète, il n'écrase pas** : si une pochette est déjà tenue
    /// — le `folder.jpg` d'une Source, notamment — l'extraction n'est même
    /// pas lancée, ce qui économise une lecture de fichier pour rien et
    /// préserve la préséance voulue par `Metadonnees::cover_retenue`.
    ///
    /// **Toujours détachée, jamais exécutée sur ce fil.** `mpv::
    /// pochette_embarquee` ouvre et parcourt le fichier avec `lofty`, un
    /// appel strictement bloquant, potentiellement sur un partage réseau qui
    /// peut ne jamais répondre. L'exécuter ici figerait la boucle du cœur
    /// entière — mpv, les commandes, l'HTTP — le temps du blocage, pas
    /// seulement cette extraction. Ce projet a déjà vécu cet incident sur un
    /// montage cifs muet (voir `sante.rs`), d'où `Sante::borne` : `spawn_blocking`
    /// pour sortir du fil asynchrone, sous délai, avec un disjoncteur par
    /// point de montage pour ne pas perdre un fil du pool à chaque nouvelle
    /// piste tant que le partage reste muet.
    pub(super) fn handle_path(&mut self, chemin: String) {
        // Retenu avant toute garde ci-dessous : c'est ce qu'`extraction_arrivee`
        // compare à l'arrivée pour rejeter une réponse tardive sur une piste
        // déjà remplacée, y compris quand `standby`/`lecture` ont changé
        // entre-temps.
        self.chemin_courant = Some(chemin.clone());
        if self.standby || !self.lecture {
            return;
        }
        if self.metadonnees.known().cover {
            return;
        }
        // Un flux n'a pas de tags, et `lofty` n'a rien à ouvrir sur une URL :
        // autant ne pas payer l'aller-retour tâche + canal pour un cas qui ne
        // peut jamais aboutir (`pochette_embarquee` le refuserait de toute
        // façon).
        if chemin.contains("://") {
            return;
        }
        if self.extraction_en_vol.as_deref() == Some(chemin.as_str()) {
            return;
        }
        self.extraction_en_vol = Some(chemin.clone());
        let tx = self.extraction_tx.clone();
        let sante = self.sante.clone();
        tokio::spawn(async move {
            let a_lire = chemin.clone();
            let r = sante
                .borne(std::path::Path::new(&chemin), move || mpv::pochette_embarquee(&a_lire))
                .await
                .flatten();
            let _ = tx.send((chemin, r)).await;
        });
    }

    /// Une extraction détachée de pochette embarquée (`handle_path`) s'est
    /// terminée. Symétrique de `pochette_arrivee` : la vérification de
    /// péremption se fait ici, à l'arrivée, pas au lancement.
    pub async fn extraction_arrivee(&mut self, chemin: String, r: Option<ritornello_proto::CoverRef>) {
        // Libéré quelle que soit l'issue et avant toute vérification
        // ci-dessous — même raison que `pochette_en_vol` dans
        // `pochette_arrivee` : sans cela, cette même piste rejouée plus tard
        // resterait bloquée pour le reste du processus.
        if self.extraction_en_vol.as_deref() == Some(chemin.as_str()) {
            self.extraction_en_vol = None;
        }
        // mpv est déjà passé à un autre fichier : cette réponse décrit une
        // piste qui n'est plus jouée, et ne doit pas s'installer sur la
        // suivante.
        if self.chemin_courant.as_deref() != Some(chemin.as_str()) {
            return;
        }
        // Une autre voie a fourni une pochette pendant que celle-ci était en
        // vol (la Source, ou un greffon) : le cœur complète, il n'écrase pas.
        if self.metadonnees.known().cover {
            return;
        }
        if !self.metadonnees.set_cover_tags(r) {
            return;
        }
        self.lance_pochette();
        self.publie_etat();
    }

    /// Enrichissement remonté par un plugin `metadata`. Rien ne se passe s'il
    /// est périmé, vide, ou émis par un plugin non déclaré (voir
    /// `Metadonnees::ajoute`).
    pub fn handle_enrichment(&mut self, plugin: &str, e: Enrichment) {
        if !self.metadonnees.ajoute(plugin, e) {
            return;
        }
        // On journalise **le gagnant**, pas celui qui vient de répondre : un
        // plugin moins prioritaire peut être retenu en réserve sans rien
        // afficher, et un journal qui le nommerait mentirait dans le seul cas
        // où on le consulte — celui d'un affichage douteux à attribuer.
        match self.metadonnees.gagnant() {
            Some(gagnant) if gagnant != plugin => {
                tracing::debug!("metadata displayed: {gagnant} (response from {plugin} held in reserve)");
            }
            Some(gagnant) => tracing::debug!("metadata displayed: {gagnant}"),
            None => {}
        }
        // Poser l'ancre à la réception : c'est le seul instant où l'écoulé
        // annoncé est exact.
        //
        // **Seulement quand c'est le gagnant qui vient de parler**, et c'est un
        // défaut trouvé en relecture. Un plugin retenu en réserve peut répondre
        // à tout moment (un titre corrigé, une pochette) sans rien apprendre de
        // neuf sur l'avancement : réancrer alors relirait la position
        // **inchangée** du gagnant en la datant de maintenant, et la barre
        // reculerait d'un coup de tout ce qu'elle avait avancé. Le `match`
        // ci-dessus distingue déjà les deux cas pour le journal.
        //
        // Un gagnant qui réémet à l'identique n'arrive jamais ici : `ajoute`
        // déduplique et rend `false`. Et un plugin plus prioritaire qui répond
        // pour la première fois **devient** le gagnant, donc son annonce ancre
        // bien, ce qui est voulu.
        if self.metadonnees.gagnant() == Some(plugin) {
            self.ancre_position = self.metadonnees.position_s().map(|p| (p, Instant::now()));
        }
        // L'enrichissement qui vient d'être retenu peut avoir changé la
        // pochette que `cover_retenue` désigne (un greffon qui écrase
        // répondant après un `fill_only`, par exemple) : `ajoute` a déjà
        // invalidé la clé publiée dans ce cas, à `lance_pochette` de relancer
        // la récupération pour la nouvelle cible.
        self.lance_pochette();
        self.publie_etat();
    }

    /// Retient la pochette qu'une Source vient de déclarer sur son propre
    /// canal (voir `SourceMessage::cover`, Task 2).
    pub fn set_cover_de_source(&mut self, c: Option<ritornello_proto::CoverRef>, origine: &str) {
        if self.metadonnees.set_cover_source(c, origine) {
            self.lance_pochette();
            self.publie_etat();
        }
    }

    /// Détache la récupération de la pochette retenue, si elle n'est pas déjà
    /// en cache ni en vol.
    ///
    /// Détachée, parce qu'un téléchargement de dix secondes ne doit pas
    /// retenir la boucle qui répond aux commandes. Et **abandonnée si
    /// l'identité change** : c'est `pochette_arrivee` qui vérifie, à
    /// l'arrivée, que la clé décrit encore ce qui joue — même garde-fou que
    /// l'écho d'identité du texte (`Metadonnees::ajoute`), pour la même
    /// raison : une réponse tardive sur le morceau précédent ne doit jamais
    /// s'installer sur le suivant.
    pub fn lance_pochette(&mut self) {
        let Some((r, _)) = self.metadonnees.cover_retenue() else {
            // Plus rien à montrer (identité changée, pochette retirée) :
            // effacer l'URL publiée plutôt que de laisser pointer une image
            // qui ne correspond plus à ce qui joue.
            self.metadonnees.set_cover_href(None);
            return;
        };
        let cle = crate::cover::cle(&r);
        if self.metadonnees.cover_publiee() == Some(cle.as_str()) {
            // Déjà publiée sous cette même clé : rien à refaire. Sans cette
            // garde, un enrichissement retenu qui republie à l'identique (une
            // station qui reconfirme ses métadonnées toutes les trente
            // secondes, par exemple) relancerait une tâche, un `contient` et
            // un aller-retour de canal pour un travail déjà fait — et
            // réarmerait `pochette_en_vol` sans nécessité.
            return;
        }
        if self.pochette_en_vol.as_deref() == Some(cle.as_str()) {
            // Déjà en vol pour cette même cible : une seconde requête
            // n'apprendrait rien de plus tôt, et doublerait le trafic réseau.
            return;
        }
        let covers = self.covers.clone();
        let tx = self.pochette_tx.clone();
        self.pochette_en_vol = Some(cle.clone());
        tokio::spawn(async move {
            if covers.contient(&cle).await {
                let _ = tx.send((cle, true)).await;
                return;
            }
            match crate::cover::recupere(&r).await {
                Some(p) => {
                    covers.insere(cle.clone(), p).await;
                    let _ = tx.send((cle, true)).await;
                }
                // Échec silencieux : l'appareil n'affiche pas d'image, et
                // c'est tout. Un 404 du Cover Art Archive est le cas courant.
                // Rapporté quand même (`false`) : c'est ce qui libère
                // `pochette_en_vol`, sans quoi cette clé resterait bloquée
                // pour le reste du processus — y compris si le même dossier
                // (donc la même clé) redevient la cible plus tard.
                None => {
                    tracing::debug!("no cover for {cle}");
                    let _ = tx.send((cle, false)).await;
                }
            }
        });
    }

    /// Une récupération détachée s'est terminée (`succes`), qu'elle ait
    /// abouti ou non. Publie l'URL locale, **si elle décrit encore ce qui
    /// joue** : la vérification se fait ici, à l'arrivée, pas au lancement —
    /// c'est ce qui empêche la pochette d'un morceau déjà remplacé de
    /// s'installer sur le suivant.
    pub async fn pochette_arrivee(&mut self, cle: String, succes: bool) {
        // Le marqueur se libère dès que cette clé revient, **quelle que soit
        // l'issue** — échec réseau, pochette qui n'est plus retenue, ou
        // succès — et **avant** toute vérification de péremption ci-dessous.
        // Sans cela, un échec ou un morceau déjà remplacé laissait cette clé
        // bloquée pour le reste du processus : `lance_pochette` refusait
        // ensuite de relancer une récupération pour cette même clé, même
        // quand elle redevenait la cible (le même dossier d'album, donc la
        // même clé, est rejoué plus tard) et même si les octets finissaient
        // par être en cache.
        if self.pochette_en_vol.as_deref() == Some(cle.as_str()) {
            self.pochette_en_vol = None;
        }
        // Le contrôle de péremption vaut pour les **deux** issues, et c'est
        // délibéré : un échec qui arrive après un changement de morceau décrit
        // une référence que ce qui joue maintenant ne vise pas. L'inscrire au
        // registre des échecs du morceau courant y noircirait une clé jamais
        // essayée pour lui — et si un contributeur proposait cette même image
        // ici, elle serait écartée sans qu'on l'ait tentée une seule fois.
        // L'échec vaut pour le morceau où il a eu lieu, comme tout le reste de
        // cet état (voir `Metadonnees::pochettes_echouees`).
        let Some((r, _)) = self.metadonnees.cover_retenue() else {
            // Plus rien ne joue, ou plus aucune pochette retenue : la
            // réponse arrive trop tard pour avoir un sens.
            return;
        };
        if crate::cover::cle(&r) != cle {
            // La pochette du morceau précédent (ou d'une référence
            // remplacée depuis) : sans cette vérification, elle s'installerait
            // sur le morceau courant.
            return;
        }
        if !succes {
            // L'échec est **retenu**, et c'est ce qui débloque les
            // contributeurs situés en dessous. Une référence retenue n'est
            // qu'une promesse : sans cette note, `cover_retenue` continuait de
            // préférer une URL morte, `known.cover` restait vrai, et
            // `musicbrainz` — muet parce qu'il croit une pochette tenue —
            // n'avait aucune chance de compenser. C'est exactement le cas que
            // la conception anticipe : « un motif qui casse rend un silence ».
            //
            // Relancer et republier seulement si la référence retenue a
            // réellement changé : c'est ce qui donne sa chance au contributeur
            // du dessous, et ce qui évite de republier pour rien.
            if self.metadonnees.marque_pochette_echouee(cle) {
                self.lance_pochette();
                self.publie_etat();
            }
            return;
        }
        // Recontrôlé plutôt que fait confiance à `succes` seul : le cache est
        // borné (`ENTREES` entrées, éviction FIFO) et cette clé a pu être
        // évincée entre le dépôt et la consommation de ce message par la
        // boucle de `main` — un cas d'autant plus réel que le canal est
        // volontairement étroit (capacité 4).
        if !self.covers.contient(&cle).await {
            return;
        }
        self.metadonnees.set_cover_href(Some(cle));
        self.publie_etat();
    }

    /// Le cache que la tâche détachée de `lance_pochette` remplit — **le
    /// même** que celui de l'`AppState` HTTP, voir la doc du champ `covers`.
    /// Réservé aux tests : c'est ce qui leur permet de prouver le partage
    /// sans passer par `main.rs`, qui n'est pas testable en tant que tel.
    #[cfg(test)]
    pub(crate) fn app_covers(&self) -> &Arc<crate::cover::CoverCache> {
        &self.covers
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    async fn un_metadata_tardif_prend_sa_place_du_manifeste_dans_larbitrage() {
        // L'invariant le plus facile à casser du câblage à chaud : la priorité
        // est celle de `plugins.toml`, jamais celle d'arrivée des annonces.
        // Seul `musicbrainz` s'est annoncé à temps ; `ouifm` arrive après le
        // démarrage alors que le manifeste le déclare **avant** lui. Un ajout en
        // queue le ferait perdre l'arbitrage, et la priorité dépendrait de la
        // chronologie du démarrage.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["musicbrainz".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("musicbrainz", enrichissement(id.clone(), "Base", "En ligne"));
        assert_eq!(etat_rx.borrow().morceau.artist.as_deref(), Some("Base"));

        // Ce que fait `main` à la réception d'une annonce tardive : recalculer la
        // liste **complète** depuis le manifeste, puis la remettre au cœur. La
        // logique d'ordre reste dans `register::metadata_order`, un seul endroit.
        let manifeste = vec!["ouifm".to_string(), "musicbrainz".to_string()];
        let mut rassemble = crate::register::Gathered::default();
        for nom in ["musicbrainz", "ouifm"] {
            rassemble.announcements.insert(
                nom.to_string(),
                ritornello_proto::Announcement {
                    name: nom.to_string(),
                    kinds: vec![ritornello_proto::PluginKind::Metadata],
                    admin: false,
                    covers: false,
                },
            );
        }
        core.set_metadata_order(crate::register::metadata_order(&manifeste, &rassemble));

        core.handle_enrichment("ouifm", enrichissement(id, "Station", "Direct"));
        assert_eq!(
            core.metadonnees.gagnant(),
            Some("ouifm"),
            "le tardif est declare avant dans le manifeste : il doit gagner"
        );
        assert_eq!(etat_rx.borrow().morceau.artist.as_deref(), Some("Station"));
    }

    #[tokio::test]
    async fn la_selection_declaree_est_diffusee_puis_oubliee_quand_rien_ne_joue() {
        // La touche numérotée mise en évidence sur la télécommande de l'IHM désigne
        // **ce qui joue** : elle suit la déclaration de la Source, et
        // disparaît à l'arrêt plutôt que de rester sur la dernière pression.
        // Le nom de présélection suit exactement la même règle : c'est le
        // point du cahier des charges qui compte (le cycle de vie de
        // `preset_name` est celui de `preset`, verrouillé ici).
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        assert_eq!(etat_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(etat_rx.borrow().preset, None);
        assert_eq!(etat_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn changer_de_source_oublie_la_selection_de_lancienne() {
        // La présélection 2 de la radio ne veut rien dire pour le cd : la
        // laisser en évidence après la bascule désignerait une touche au
        // hasard. Même chose pour son nom : "France Inter" affiché après un
        // passage au cd serait un nom de station attribué à un disque.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        assert_eq!(etat_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(etat_rx.borrow().preset, None);
        assert_eq!(etat_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn lidentite_declaree_par_la_source_est_annoncee_aux_plugins() {
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"kind": "stream", "url": "http://ouifm"});
        core.handle_source_update("radio", joue(id.clone()));
        let np = np_rx.borrow().clone();
        assert_eq!(np.source, "radio");
        assert_eq!(np.identity, Some(id));
    }

    #[tokio::test]
    async fn une_identite_dune_source_inactive_est_ignoree() {
        // Le cd peut rapporter l'insertion d'un disque pendant que la radio
        // joue : annoncer cette identité ferait travailler les plugins sur un
        // morceau qui ne sort d'aucun haut-parleur.
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec![]);
        core.handle_source_update("cd", joue(serde_json::json!({"kind": "disc"})));
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn licy_est_diffuse_a_la_spa() {
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        // `resume` met la radio en lecture : sans quoi le cœur écarte à raison
        // tout titre ICY, rien ne jouant.
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        assert_eq!(etat_rx.borrow().morceau.title, None);

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.morceau.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(etat.morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn un_enrichissement_de_plugin_ecrase_licy() {
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        // Texte de remplissage réellement émis par OUI FM sur son flux principal.
        core.handle_event(Event::IcyTitle("Now Playing info goes here".into())).await;
        // Sans ce contrôle, la suite du test passerait aussi bien si l'ICY
        // n'était jamais entré : ce n'est pas « l'enrichissement gagne » qu'on
        // vérifierait, mais « l'ICY est absent ».
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("Now Playing info goes here"));
        core.handle_enrichment("ouifm", enrichissement(id, "Shaka Ponk", "Wanna Get Free"));
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.morceau.artist.as_deref(), Some("Shaka Ponk"));
        assert_eq!(etat.morceau.title.as_deref(), Some("Wanna Get Free"));
        assert_eq!(etat.morceau.origin.as_deref(), Some("ouifm"));
    }

    #[tokio::test]
    async fn un_enrichissement_perime_ne_touche_pas_laffichage() {
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        etat_rx.borrow_and_update();
        core.handle_enrichment(
            "ouifm",
            enrichissement(serde_json::json!({"url": "un"}), "Ancien", "Morceau"),
        );
        assert!(!etat_rx.has_changed().unwrap(), "la reponse en retard ne doit rien publier");
        assert!(core.etat_lecteur().morceau.est_vide());
    }

    #[tokio::test]
    async fn changer_de_morceau_efface_immediatement_le_precedent() {
        // Le morceau précédent ne doit pas rester à l'écran pendant qu'on
        // attend le suivant : c'est un comportement, pas un détail.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("So What"));

        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        assert!(etat_rx.borrow().morceau.est_vide(), "l'ardoise doit etre nette aussitot");
    }

    #[tokio::test]
    async fn larret_demande_par_la_telecommande_efface_le_titre_de_lafficheur() {
        // Défaut trouvé en revue : `set_identity` ne rafraîchissait pas
        // l'affichage. La SPA se vidait (canal d'état), mais l'afficheur
        // physique gardait le titre du morceau arrêté jusqu'à la prochaine
        // action de l'utilisateur — toute la nuit sur un appareil qu'on arrête
        // le soir. L'ancien test n'assertionnait que le canal `now_playing` :
        // il passait aussi bien contre le code faux.
        let (mut core, np_rx, etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment("ouifm", enrichissement(id, "Miles Davis", "So What"));
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("So What"));

        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None, "les plugins doivent cesser leur travail");
        assert!(etat_rx.borrow().morceau.est_vide(), "le titre ne doit pas rester affiche");
    }

    #[tokio::test]
    async fn un_titre_icy_arrivant_en_veille_natteint_pas_letat_publie() {
        // Chemin réel : `Command::Power` attend la réponse de la Source à
        // `Deactivate` (jusqu'à 5 s) pendant que mpv joue encore. Un titre émis
        // dans cet intervalle arrive après que l'état de veille a été publié —
        // et rien ne se produisant plus en veille, il y resterait des semaines.
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(etat_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.status.as_deref(), Some("STANDBY"));
        assert!(etat.morceau.est_vide(), "aucun titre ne doit se coller sur l'etat de veille");
    }

    #[tokio::test]
    async fn la_veille_bloque_licy_meme_avec_une_identite_vivante() {
        // Deux gardes couvrent ce chemin, et celle-ci n'est pas redondante : la
        // mise en veille efface normalement l'identité, mais `Command::Power`
        // peut rendre la main sur l'erreur de `player.stop()` **avant** de le
        // faire, laissant la veille active avec une identité vivante. L'état est
        // donc posé directement ici pour éprouver la garde de veille seule.
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap(); // pose `expecting_stream` (la radio joue)
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        etat_rx.borrow_and_update();
        // Veille posée directement : c'est l'état atteint quand `Command::Power`
        // rend la main sur l'erreur de `player.stop()`, donc avec une lecture
        // encore attendue. La garde de veille est alors la seule à agir.
        core.standby = true;
        assert!(core.expecting_stream, "sans quoi ce test n'eprouverait pas la garde de veille");

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        assert!(!etat_rx.has_changed().unwrap(), "rien ne doit atteindre l'etat publie en veille");
        assert_eq!(etat_rx.borrow().morceau.title, None);
    }

    #[tokio::test]
    async fn licy_saffiche_meme_si_la_source_ne_declare_aucune_identite() {
        // Régression rencontrée en essai réel : la couche ICY était
        // conditionnée à la déclaration d'identité de la Source, donc muette
        // face à un plugin qui ne la déclare pas — et muette **en silence**,
        // sans une ligne de journal. C'est pourtant la seule couche censée
        // fonctionner sans aucun plugin `metadata`.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        // Aucune identité n'est jamais déclarée : seul le nom de présélection arrive.
        core.handle_source_update("radio", update_avec_nom(Some("FIP")));
        core.handle_event(Event::IcyTitle("Made Up - TAHITI 80".into())).await;
        assert_eq!(etat_rx.borrow().morceau.title.as_deref(), Some("Made Up - TAHITI 80"));
        assert_eq!(etat_rx.borrow().morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn un_titre_icy_arrivant_apres_un_arret_est_ignore() {
        let (mut core, _np_rx, mut etat_rx, _d) = setup_metadonnees(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::Stop).await.unwrap();
        etat_rx.borrow_and_update();

        core.handle_event(Event::IcyTitle("un titre en retard".into())).await;
        assert!(!etat_rx.has_changed().unwrap(), "rien ne doit etre publie");
        assert_eq!(etat_rx.borrow().morceau.title, None, "la SPA ne doit pas annoncer de morceau");
    }

    #[tokio::test]
    async fn la_mise_en_veille_oublie_lidentite() {
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn la_mise_en_veille_oublie_la_selection_et_son_nom() {
        // Le point du cahier des charges qui compte : `preset_name` vit et
        // meurt avec `preset`, et le seul endroit qui les efface est
        // `set_identity(None)` — que `Command::Power` atteint en entrant en
        // veille, comme `Stop` et `SourceCycle` déjà couverts plus haut.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec![]);
        let mut update = joue(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(etat_rx.borrow().preset, Some(2));
        assert_eq!(etat_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::Power).await.unwrap(); // entre en veille
        assert_eq!(etat_rx.borrow().preset, None);
        assert_eq!(etat_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn changer_de_source_oublie_lidentite_precedente() {
        let (mut core, np_rx, _etat_rx, _d) = setup_metadonnees(vec!["ouifm".into()]);
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_command(Command::SourceCycle).await.unwrap();
        let np = np_rx.borrow().clone();
        assert_eq!(np.identity, None);
        assert_eq!(np.source, "cd", "l'annonce porte la nouvelle source active");
    }

    #[tokio::test]
    async fn un_plugin_metadata_declare_mais_muet_neclipse_pas_licy() {
        // Un plugin déclaré qui ne répond jamais (processus mort, socket muette)
        // ne doit pas priver l'appareil de la couche de base : le titre annoncé
        // par le flux doit continuer de s'afficher, attribué à `icy`.
        let (mut core, _np_rx, etat_rx, _d) = setup_metadonnees(vec!["mort".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", joue(serde_json::json!({"url": "un"})));
        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let etat = etat_rx.borrow().clone();
        assert_eq!(etat.morceau.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(etat.morceau.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn une_pochette_de_source_mal_formee_ne_touche_pas_a_celle_qui_tient() {
        // `CoverRef::validee` est la règle de forme de `ritornello-proto`, et
        // elle ne s'appliquait qu'à un des deux canaux d'entrée (celui des
        // greffons). Une référence refusée vaut « rien de neuf » — jamais
        // « plus de pochette » : c'est la convention du champ, et effacer sur
        // une trame mal formée retirerait l'image valide déjà déclarée.
        let (mut core, _np_rx, _etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let bonne = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let id = serde_json::json!({"kind": "file", "path": "/a.flac"});

        let mut update = joue(id);
        update.cover = Some(bonne.clone());
        core.handle_source_update("radio", update.clone());
        assert!(core.metadonnees.known().cover);

        // Chemin relatif : refusé par la forme. Rien ne doit bouger.
        update.identity = None;
        update.cover = Some(ritornello_proto::CoverRef::Path { path: "relatif/folder.jpg".into() });
        core.handle_source_update("radio", update.clone());
        assert_eq!(
            core.metadonnees.cover_retenue().map(|(r, _)| r),
            Some(bonne),
            "une reference mal formee ne doit ni s'installer ni effacer celle qui tient"
        );

        // Et une URL en clair vers une IP littérale non plus, l'autre moitié
        // de ce que `validee` refuse.
        update.cover =
            Some(ritornello_proto::CoverRef::Url { url: "http://192.168.1.1/a.jpg".into() });
        core.handle_source_update("radio", update);
        assert_eq!(core.metadonnees.cover_retenue().map(|(_, o)| o), Some("radio".to_string()));
    }

    /// Un contributeur qui vient de se câbler à chaud, ou qui répond
    /// lentement, doit voir ce qui est déjà connu — sinon il ne peut ni
    /// compléter ce qui manque, ni s'abstenir sur ce qui est déjà rempli.
    #[tokio::test]
    async fn le_now_playing_emis_porte_letat_partiel() {
        let (mut core, mut np_rx, _etat_rx, _tmp) = core_de_test();
        core.set_identity(Some(serde_json::json!({"kind": "stream", "url": "u"})));
        // `handle_icy_title` exige un flux effectivement attendu (voir sa
        // garde) : sans cette ligne, le titre serait ignoré en silence et ce
        // test n'éprouverait rien.
        core.expecting_stream = true;
        core.handle_icy_title("OUI FM".into());
        core.publie_etat();
        // Un contributeur doit voir ce qui est deja connu, sinon il ne peut ni
        // completer ni s'abstenir.
        let np = np_rx.borrow_and_update().clone();
        assert_eq!(np.known.title.as_deref(), Some("OUI FM"));
        assert!(!np.known.cover);
    }

    #[tokio::test]
    async fn une_pochette_arrivee_devient_une_url_locale_dans_letat() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        // La recuperation est detachee : on l'attend explicitement dans le test
        // plutot que de dormir, pour ne pas fabriquer un flake.
        let cle = crate::cover::cle(&r);
        let p = crate::cover::recupere(&r).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(etat.morceau.cover_href.as_deref(), Some(&format!("/api/cover/{cle}")[..]));
        assert_eq!(etat.morceau.cover_origin.as_deref(), Some("files"));
    }

    #[tokio::test]
    async fn une_recuperation_echouee_libere_les_contributeurs_du_dessous() {
        // La jonction que la revue a trouvée : `known.cover` était vrai dès
        // qu'une référence était *retenue*, et `cover_retenue` continuait de
        // préférer cette référence après l'échec de sa récupération. Un motif
        // d'URL de station qui a rouillé faisait donc taire `musicbrainz`
        // définitivement — cas que la conception anticipe explicitement.
        let (mut core, mut np_rx, _etat_rx, _tmp) = setup_metadonnees(vec![
            "radiofrance".into(),
            "musicbrainz".into(),
        ]);
        let id = serde_json::json!({"url": "https://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        let morte =
            ritornello_proto::CoverRef::Url { url: "https://api.radiofrance.fr/rouille".into() };
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                cover: Some(morte.clone()),
                ..Default::default()
            },
        );
        assert!(np_rx.borrow_and_update().known.cover, "une reference est tenue, on ne sait pas encore");

        // Ce que la tâche détachée rapporte quand la récupération n'a rien
        // rendu : `succes == false`.
        core.pochette_arrivee(crate::cover::cle(&morte), false).await;
        let np = np_rx.borrow_and_update().clone();
        assert!(!np.known.cover, "une promesse non tenue doit rendre la parole aux autres");
        // Et le texte que ce même greffon fournit n'a pas bougé : c'est bien
        // ce qui permet à `musicbrainz` de chercher sur cet artiste et cet
        // album, comme la documentation le promet.
        assert_eq!(np.known.title.as_deref(), Some("So What"));
        assert_eq!(np.known.artist.as_deref(), Some("Miles Davis"));
    }

    #[tokio::test]
    async fn un_echec_arrive_apres_un_changement_de_morceau_nest_pas_inscrit() {
        // Le registre des échecs vaut pour le morceau où ils ont eu lieu. Un
        // échec en retard, arrivé après le changement d'identité, ne doit donc
        // pas y entrer : il y noircirait une clé jamais essayée pour le
        // morceau courant, et écarterait cette image alors qu'elle pourrait
        // parfaitement répondre.
        let (mut core, _np_rx, _etat_rx, _tmp) = setup_metadonnees(vec!["musicbrainz".into()]);
        let une = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(une.clone()));
        let image = ritornello_proto::CoverRef::Url {
            url: "https://coverartarchive.org/release/x/front-500".into(),
        };
        core.handle_enrichment(
            "musicbrainz",
            Enrichment {
                identity: une,
                title: Some("T".into()),
                cover: Some(image.clone()),
                ..Default::default()
            },
        );

        // Morceau suivant, puis l'échec du précédent qui arrive enfin.
        let deux = serde_json::json!({"url": "deux"});
        core.handle_source_update("radio", joue(deux.clone()));
        core.pochette_arrivee(crate::cover::cle(&image), false).await;

        // Le même greffon propose la même image pour ce morceau-ci : jamais
        // essayée ici, elle doit être retenue.
        core.handle_enrichment(
            "musicbrainz",
            Enrichment { identity: deux, title: Some("T2".into()), cover: Some(image), ..Default::default() },
        );
        assert!(
            core.metadonnees.known().cover,
            "un echec perime ne doit pas condamner la reference du morceau suivant"
        );
    }

    /// Le risque signalé par la revue de la tâche 3 : deux `Arc<CoverCache>`
    /// distincts compileraient et laisseraient passer tous les autres tests
    /// de ce module, mais la pochette que le cœur vient de déposer ne serait
    /// jamais lisible par la vraie route HTTP. Ce test passe donc par
    /// `status::router` et une vraie requête, avec exactement le même `Arc`
    /// que celui exposé par `app_covers()`.
    #[tokio::test]
    async fn la_route_http_sert_ce_que_le_coeur_vient_de_deposer() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let (mut core, _np_rx, _etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let cle = crate::cover::cle(&r);

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        let p = crate::cover::recupere(&r).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        // Le seul champ qui compte pour cette preuve : le reste de l'`AppState`
        // vient du montage de test générique, jamais consulté par cette route.
        let app = crate::status::router(crate::status::AppState {
            covers: core.app_covers().clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get(format!("/api/cover/{cle}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "la route doit lire dans le meme cache que celui rempli par le coeur"
        );
    }

    /// La pochette d'un morceau déjà remplacé ne doit jamais s'installer sur
    /// le suivant : la vérification de péremption se fait à l'arrivée, pas au
    /// lancement — même garde-fou que l'écho d'identité des enrichissements.
    #[tokio::test]
    async fn une_pochette_perimee_ne_s_installe_pas_sur_le_morceau_suivant() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let ancienne = tmp.path().join("ancienne.jpg");
        std::fs::write(&ancienne, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r_ancienne = ritornello_proto::CoverRef::Path { path: ancienne.to_string_lossy().into_owned() };
        let cle_ancienne = crate::cover::cle(&r_ancienne);

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r_ancienne.clone()), "files");

        // Le morceau change avant que la récupération de l'ancienne pochette
        // n'ait eu le temps d'arriver, et le nouveau déclare sa **propre**
        // pochette (une référence différente) : la cible que `cover_retenue`
        // désigne change avec l'identité, sans jamais redevenir `None` — c'est
        // le comparaison de clé de `pochette_arrivee`, pas seulement l'absence
        // de cible, qui doit rejeter la réponse tardive.
        let nouvelle = tmp.path().join("nouvelle.jpg");
        std::fs::write(&nouvelle, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r_nouvelle = ritornello_proto::CoverRef::Path { path: nouvelle.to_string_lossy().into_owned() };
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/b.flac"})));
        core.set_cover_de_source(Some(r_nouvelle), "files");
        etat_rx.borrow_and_update();

        // La réponse tardive de l'ANCIENNE pochette arrive quand même.
        let p = crate::cover::recupere(&r_ancienne).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle_ancienne.clone(), p).await;
        core.pochette_arrivee(cle_ancienne, true).await;

        assert!(
            !etat_rx.has_changed().unwrap_or(false),
            "la pochette perimee ne doit rien publier sur le morceau suivant"
        );
        assert_eq!(
            core.etat_lecteur().morceau.cover_href, None,
            "la pochette du morceau precedent ne doit pas s'installer sur le suivant"
        );
    }

    /// Repro exacte du défaut critique relevé en revue (tâche 5) : le
    /// marqueur en vol doit se libérer même quand l'arrivée ne publie rien
    /// (morceau déjà remplacé), sans quoi revenir plus tard sur le même
    /// dossier — donc la même clé, un `folder.jpg` est partagé par toutes
    /// les pistes d'un album — ne relançait plus jamais rien : `lance_pochette`
    /// voyait la clé perpétuellement « en vol » et abandonnait en silence.
    #[tokio::test]
    async fn le_marqueur_en_vol_se_libere_meme_quand_larrivee_ne_publie_rien() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let cle = crate::cover::cle(&r);

        // 1. Un morceau d'album déclare la pochette K : `lance_pochette` arme
        // le marqueur. La vraie tâche détachée tourne aussi en tâche de fond,
        // mais rien ci-dessous n'attend son issue — comme les autres tests de
        // ce module, celui-ci simule lui-même l'arrivée plutôt que de dormir.
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        assert_eq!(core.pochette_en_vol.as_deref(), Some(cle.as_str()));

        // 2. Le morceau change avant que la réponse n'arrive : plus rien
        // n'est retenu, mais le marqueur, lui, ne bouge pas tout seul — c'est
        // `pochette_arrivee` qui a la charge de le libérer, à l'arrivée.
        core.set_identity(Some(serde_json::json!({"kind": "stream", "url": "u"})));
        assert_eq!(core.pochette_en_vol.as_deref(), Some(cle.as_str()));
        // Le changement d'identité publie déjà de son côté (titre effacé) :
        // on consomme cette trame pour que l'assertion suivante ne juge que
        // ce que `pochette_arrivee` publie, ou non, par elle-même.
        etat_rx.borrow_and_update();

        // 3. La réponse arrive quand même, en succès (les octets sont bien en
        // main, seulement plus rien à montrer avec). Avant le correctif,
        // cette méthode retournait ici sans jamais toucher au marqueur.
        core.pochette_arrivee(cle.clone(), true).await;
        assert_eq!(core.pochette_en_vol, None, "le marqueur doit se liberer meme sans rien publier");
        assert!(
            !etat_rx.has_changed().unwrap_or(false),
            "rien n'est retenu : cette arrivee ne doit rien publier"
        );

        // 4. Le même dossier — donc la même clé — redevient la cible. Sans le
        // correctif, `lance_pochette` restait bloquée à jamais sur cette clé
        // et cet album n'affichait plus jamais de pochette avant redémarrage.
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_cover_de_source(Some(r.clone()), "files");
        assert_eq!(
            core.pochette_en_vol.as_deref(),
            Some(cle.as_str()),
            "une nouvelle recuperation doit pouvoir repartir pour la meme cle"
        );
        let p = crate::cover::recupere(&r).await.expect("l'image de test doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(
            etat.morceau.cover_href.as_deref(),
            Some(&format!("/api/cover/{cle}")[..]),
            "revenir sur la meme cle doit a nouveau pouvoir publier une pochette"
        );
    }

    /// Une trame de couverture n'est traitée que si elle vient de la Source
    /// **active** — même garde que le reste de la trame (identité, statut,
    /// présélection). Régression relevée en revue : le câblage précédent
    /// appelait `set_cover_de_source` en dehors de `handle_source_update`,
    /// sans repasser par sa garde de tête, si bien qu'une Source inactive
    /// pouvait faire apparaître sa pochette sur le morceau que joue la
    /// Source active.
    #[tokio::test]
    async fn une_pochette_dune_source_inactive_nest_pas_retenue() {
        let (mut core, _np_rx, etat_rx, tmp) = core_de_test();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };

        // `cd` n'est pas la source active (`radio` l'est, par défaut).
        core.handle_source_update(
            "cd",
            SourceUpdate {
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: Some(r),
            },
        );
        assert_eq!(core.pochette_en_vol, None, "une source inactive ne doit declencher aucune recuperation");
        assert!(!etat_rx.has_changed().unwrap_or(false));
    }

    /// Le chemin annoncé par mpv (`Event::Path`) arme une extraction
    /// **détachée** : `handle_event` rend la main aussitôt, sans que rien ne
    /// soit encore connu — la suite (`set_cover_tags` → `true`,
    /// `lance_pochette`, `publie_etat`) n'a lieu qu'à l'arrivée du résultat
    /// sur le canal.
    ///
    /// Le vrai canal est vidé ici, plutôt que rejoué à la main comme le fait
    /// `pochette_arrivee` ailleurs dans ce fichier : relire les tags une
    /// seconde fois pour reconstituer le `CoverRef` attendu écrirait en
    /// concurrence avec la tâche détachée sur le **même** fichier temporaire
    /// (défaut trouvé à l'usage, voir `core_de_test_avec_extraction`). La
    /// tâche détachée doit rester l'unique écrivaine.
    #[tokio::test]
    async fn le_chemin_mpv_declenche_lextraction_et_larmement_de_la_recuperation() {
        let (mut core, mut etat_rx, mut extraction_rx, tmp) = core_de_test_avec_extraction();
        let Some(f) = mp3_avec_pochette_de_test(tmp.path()) else {
            eprintln!("ffmpeg absent : test saute");
            return;
        };
        let chemin = f.to_string_lossy().into_owned();

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": chemin})));
        core.lecture = true;
        etat_rx.borrow_and_update();

        assert_eq!(
            core.handle_event(Event::Path(chemin.clone())).await,
            EventOutcome::Nothing,
            "un chemin ne prouve rien de la vivacite du flux"
        );

        // C'est ICI, et seulement ici, que se vérifie que l'extraction est
        // réellement détachée (ruling 1 de la revue de cette tâche) — sur un
        // vrai mp3 à pochette embarquée, pas sur un chemin inexistant qui
        // échouerait de toute façon aussi vite en synchrone qu'en détaché et
        // ne prouverait donc rien. `#[tokio::test]` tourne sur un runtime
        // **mono-fil** (`current_thread`), et le bras `Event::Path` de
        // `handle_event` ne contient aucun `.await` avant de rendre la main :
        // si `handle_path` exécutait encore `pochette_embarquee` en
        // synchrone (régression qui supprimerait le `tokio::spawn` ou l'appel
        // à `Sante::borne`), `known().cover` serait déjà vrai à cet instant
        // précis, dans le même sondage (poll) que l'`.await` ci-dessus — il
        // n'existe aucun univers d'exécution, rapide ou lent, où une
        // extraction synchrone laisserait cette assertion passer. Ne pas
        // affaiblir ni retirer cette ligne sans la remplacer par une preuve
        // équivalente.
        assert!(!core.metadonnees.known().cover, "l'extraction doit etre detachee, jamais synchrone");
        assert!(!etat_rx.has_changed().unwrap_or(false));

        // Attend le vrai résultat sur le vrai canal — pas d'horloge ici,
        // c'est un rendez-vous asynchrone réel sur la tâche que `handle_path`
        // a détachée.
        let (chemin_recu, r) =
            extraction_rx.recv().await.expect("le canal d'extraction doit livrer un resultat");
        assert_eq!(chemin_recu, chemin);
        let r = r.expect("l'extraction a du reussir sur ce fichier de test");
        core.extraction_arrivee(chemin_recu, Some(r.clone())).await;

        assert!(core.metadonnees.known().cover);
        let (retenue, origine) = core.metadonnees.cover_retenue().expect("une pochette doit etre retenue");
        assert_eq!(origine, crate::metadata::ORIGINE_TAGS);
        assert_eq!(retenue, r);
        assert!(etat_rx.has_changed().unwrap(), "set_cover_tags a renvoye vrai : une trame doit sortir");

        // Rejoue la fin de la récupération détachée à la main, comme les
        // autres tests de ce module : la clé armée par `lance_pochette` doit
        // être celle du fichier temporaire écrit par l'extraction.
        let cle = crate::cover::cle(&r);
        assert_eq!(core.pochette_en_vol.as_deref(), Some(cle.as_str()));
        let p = crate::cover::recupere(&r).await.expect("le fichier temporaire doit etre lisible");
        core.app_covers().insere(cle.clone(), p).await;
        core.pochette_arrivee(cle.clone(), true).await;

        let etat = etat_rx.borrow_and_update().clone();
        assert_eq!(etat.morceau.cover_href.as_deref(), Some(&format!("/api/cover/{cle}")[..]));
        assert_eq!(etat.morceau.cover_origin.as_deref(), Some(crate::metadata::ORIGINE_TAGS));
    }

    /// Le cœur complète, il n'écrase pas : une pochette déjà tenue (ici celle
    /// d'une Source, la plus prioritaire) empêche l'extraction, même quand
    /// mpv annonce un fichier qui, lui, porte une pochette embarquée valide.
    #[tokio::test]
    async fn une_pochette_deja_connue_empeche_toute_extraction() {
        let (mut core, _np_rx, mut etat_rx, tmp) = core_de_test();
        let Some(f) = mp3_avec_pochette_de_test(tmp.path()) else {
            eprintln!("ffmpeg absent : test saute");
            return;
        };
        let folder = tmp.path().join("folder.jpg");
        std::fs::write(&folder, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: folder.to_string_lossy().into_owned() };

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.lecture = true;
        core.set_cover_de_source(Some(r.clone()), "files");
        etat_rx.borrow_and_update();

        core.handle_event(Event::Path(f.to_string_lossy().into_owned())).await;

        assert!(
            !etat_rx.has_changed().unwrap(),
            "aucune extraction tentee, donc aucune trame supplementaire"
        );
        let (retenue, origine) = core.metadonnees.cover_retenue().unwrap();
        assert_eq!(origine, "files", "le folder.jpg de la Source garde la preseance");
        assert_eq!(retenue, r);
    }

    #[tokio::test]
    async fn une_pochette_seule_est_retenue_et_nefface_pas_le_statut() {
        // **Le défaut que la fusion du chantier des pochettes a produit : chaque
        // pochette de Source perdue en silence.** Une pochette arrive
        // volontairement seule, en notification spontanée, sans identité ni
        // statut (voir `SourceMessage::cover`) : c'est sa forme normale. Elle
        // prend donc le retour anticipé — et l'application posée par la fusion
        // vivait tout en bas de `handle_source_update`, après ce `return`. Elle
        // n'était jamais atteinte.
        //
        // Ce qui est épinglé ici est donc **l'application sur le chemin du
        // retour anticipé**, et non le fait que `cover` figure dans
        // `porte_un_fait` : ce prédicat est une tautologie, `serve_source`
        // estampillant `can_eject` sur chaque trame (voir le corps de
        // `handle_source_update`). La trame passait déjà le garde avant qu'on y
        // ajoute `cover`.
        //
        // La trame est donc construite par `trame_du_sdk()` et non `update_nu()` :
        // avec `can_eject: None`, elle décrirait une forme que le SDK ne peut pas
        // émettre, et l'assertion sur le statut y attesterait un mode de
        // défaillance qui n'existe pas. Cette assertion reste, en second rang :
        // elle vaudra si l'estampille devient un jour conditionnelle.
        let (mut core, _np_rx, _etat_rx, tmp) = core_de_test();
        let mut permanent = trame_du_sdk();
        permanent.status = Some("EN DIRECT".into());
        core.handle_source_update("radio", permanent);

        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let mut pochette_seule = trame_du_sdk();
        pochette_seule.cover = Some(ritornello_proto::CoverRef::Path {
            path: image.to_string_lossy().into_owned(),
        });
        core.handle_source_update("radio", pochette_seule);

        assert!(
            core.metadonnees.cover_retenue().is_some(),
            "la pochette doit etre retenue : le retour anticipe est le seul chemin \
             par lequel une pochette de Source atteint le coeur"
        );
        assert_eq!(
            core.etat_lecteur().status.as_deref(),
            Some("EN DIRECT"),
            "et le statut memorise doit survivre"
        );
    }
}
