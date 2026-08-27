//! Résolution des métadonnées du morceau en cours.
//!
//! Deux couches se superposent : ce que le flux annonce lui-même (l'en-tête ICY
//! lu par mpv, affiché **brut**) et ce qu'un plugin `metadata` a appris. La
//! seconde gagne sur la première quand elle correspond à ce qui joue.
//!
//! Tout est ici en fonctions et méthodes pures — aucune socket, aucun routeur,
//! aucune horloge : l'arbitrage entre plugins est précisément la partie où une
//! erreur ne se voit pas à l'œil sur l'appareil.
//!
//! La mise en page de l'affichage, elle, ne vit plus ici : c'est au plugin
//! d'affichage de composer ses lignes depuis `PlayerState` (voir
//! `ritornello-plugin-console::display::compose`).

pub use ritornello_proto::{Morceau, PlayerState};

use ritornello_proto::{CoverRef, Enrichment};
use serde_json::Value;
use std::collections::HashMap;

/// Origine retenue pour l'affichage quand elle vient du flux lui-même.
pub const ORIGINE_ICY: &str = "icy";

/// Origine retenue quand l'information vient des **tags du fichier joué**.
///
/// `tags` et non `mpv` : le badge affiché à l'utilisateur doit nommer ce qu'il
/// regarde — d'où vient l'information — et non le composant qui l'a lue, qui
/// est un détail d'implémentation susceptible de changer.
pub const ORIGINE_TAGS: &str = "tags";

/// État de résolution : ce qui joue, ce que le flux en dit, ce que les plugins
/// en disent.
#[derive(Debug, Default)]
pub struct Metadonnees {
    /// Noms des plugins `metadata` dans l'ordre de déclaration de
    /// `plugins.toml`. **L'ordre est la priorité** : le premier déclaré qui a
    /// répondu gagne, et un plugin déclaré plus bas ne l'écrase jamais.
    ordre: Vec<String>,
    /// Identité opaque de ce qui joue, produite par la Source.
    identity: Option<Value>,
    /// Dernier titre ICY vu, brut.
    icy: Option<String>,
    /// Derniers tags vus sur le fichier joué (artiste, titre, album).
    tags: Option<Morceau>,
    /// Enrichissements correspondant à `identity`, par plugin.
    enrichissements: HashMap<String, Enrichment>,
    /// Pochette déclarée par la Source sur son canal, avec son origine.
    /// L'étage le plus bas, et pourtant le plus prioritaire pour l'image : le
    /// `folder.jpg` posé dans le répertoire est celui qu'on a choisi à la main.
    cover_source: Option<(CoverRef, String)>,
    /// Pochette embarquée dans le fichier, lue par le cœur.
    cover_tags: Option<CoverRef>,
    /// Clé du cache, une fois les octets en main. Tant qu'elle est `None`, rien
    /// n'est publié : l'IHM ne doit jamais recevoir l'URL d'une image cassée.
    cover_cle: Option<String>,
    /// Clés dont la récupération a **échoué** pour ce qui joue.
    ///
    /// Une référence retenue n'est qu'une promesse : `cover_retenue` la
    /// désigne dès qu'un contributeur l'annonce, bien avant que les octets
    /// soient en main. Sans mémoire de l'échec, une promesse non tenue
    /// restait pourtant préférée pour toujours — un motif d'URL de station qui
    /// a rouillé (« un motif qui casse rend un silence », dit la conception)
    /// suffisait à faire taire `musicbrainz` définitivement : `known.cover`
    /// restait vrai, donc il ne cherchait rien, et il aurait de toute façon
    /// été distancé s'il avait parlé.
    ///
    /// Des clés et non des `CoverRef` : c'est ce que le canal de retour porte
    /// (voir `Core::pochette_arrivee`), et c'est aussi la granularité juste —
    /// deux contributeurs qui donnent la même URL décrivent la même image et
    /// échouent ensemble.
    pochettes_echouees: std::collections::HashSet<String>,
}

impl Metadonnees {
    pub fn new(ordre: Vec<String>) -> Self {
        Self { ordre, ..Default::default() }
    }

    /// Remplace la liste des plugins `metadata`, donc la priorité d'arbitrage.
    ///
    /// **Remplace, n'ajoute pas** : un greffon `metadata` qui s'annonce après
    /// le démarrage doit prendre sa place de `plugins.toml`, pas la dernière.
    /// La liste est donc recalculée en entier par `register::metadata_order`,
    /// qui reste le seul endroit où l'ordre est décidé.
    ///
    /// Les enrichissements déjà reçus sont conservés : ils décrivent ce qui
    /// joue, ce que l'arrivée d'un greffon ne change pas. Ceux d'un plugin qui
    /// sortirait de la liste cessent simplement d'être consultés — `gagnant`
    /// ne parcourt que `ordre`.
    pub fn set_ordre(&mut self, ordre: Vec<String>) {
        self.ordre = ordre;
    }

    pub fn identity(&self) -> Option<&Value> {
        self.identity.as_ref()
    }

    /// Change ce qui joue. Renvoie `true` si l'identité a réellement changé,
    /// auquel cas **tout l'état de résolution a été remis à zéro**.
    ///
    /// Vider immédiatement l'ICY et les enrichissements est un comportement, pas
    /// un détail : laisser le morceau précédent à l'écran pendant qu'on attend
    /// le suivant serait plus trompeur que n'afficher rien.
    pub fn set_identity(&mut self, identity: Option<Value>) -> bool {
        if self.identity == identity {
            return false;
        }
        self.identity = identity;
        self.icy = None;
        self.tags = None;
        self.enrichissements.clear();
        self.cover_source = None;
        self.cover_tags = None;
        self.cover_cle = None;
        // Vidé avec le reste de l'état par morceau : un échec vaut pour une
        // référence *de ce morceau-là*. La même URL peut parfaitement
        // répondre au morceau suivant — un CDN qui s'est réveillé — et une
        // liste qui survivrait à l'identité empêcherait de la redemander.
        self.pochettes_echouees.clear();
        true
    }

    /// Retient les tags portés par le fichier joué. Renvoie `true` s'ils
    /// apportent du nouveau — mpv republie la propriété `metadata` à chaque
    /// changement de piste, et parfois à l'identique.
    ///
    /// Comme l'ICY, cette couche **ne conditionne rien à l'identité** : elle
    /// doit fonctionner sans aucun plugin, et sans que la Source ait à
    /// déclarer quoi que ce soit. C'est ce qui la rend utile à toute source
    /// jouant un fichier taggé, y compris une source future qui ne saurait
    /// rien de tout ceci.
    pub fn set_tags(&mut self, morceau: Morceau) -> bool {
        if self.tags.as_ref() == Some(&morceau) {
            return false;
        }
        self.tags = Some(morceau);
        true
    }

    /// Retient le titre annoncé par le flux. Renvoie `true` s'il apporte du
    /// nouveau (Icecast répète le même en-tête tout au long d'un morceau).
    ///
    /// **Ne conditionne rien à l'identité.** C'est délibéré, et la première
    /// version le faisait : refuser un titre ICY faute d'identité courante rend
    /// la couche ICY dépendante du bon vouloir de la Source, alors qu'elle doit
    /// fonctionner **sans aucun plugin**. Une Source qui ne déclare pas
    /// d'identité — un plugin tiers, ou un binaire pas encore mis à jour —
    /// privait ainsi l'appareil de la seule couche qui marche toute seule, en
    /// silence et sans rien dans les journaux.
    ///
    /// C'est au cœur de décider si quelque chose joue : il le sait de son côté
    /// (voir `handle_icy_title`), sans rien demander à la Source.
    pub fn set_icy(&mut self, titre: String) -> bool {
        if self.icy.as_deref() == Some(titre.as_str()) {
            return false;
        }
        self.icy = Some(titre);
        // Les enrichissements ne sont **pas** effacés ici, et c'est une décision
        // du propriétaire : un plugin `metadata` garde la priorité sur l'ICY en
        // toutes circonstances.
        //
        // Une version antérieure les effaçait, au motif qu'un titre ICY nouveau
        // prouve que le morceau a changé et que l'enrichissement en mémoire
        // décrit le précédent. C'est exact, mais la conséquence était une
        // alternance visible : à chaque morceau, l'affichage passait par la forme
        // ICY (sur ces flux, « Titre - ARTISTE », parfois le seul nom de la
        // station en remplissage) avant que le plugin ne corrige une seconde plus
        // tard.
        //
        // Compromis assumé, et il est réel : au changement de morceau, le titre
        // précédent reste affiché le temps que le plugin envoie sa trame. Court
        // en pratique — les deux viennent de la même automatisation de la station
        // — mais durable si le plugin cesse de répondre. Un titre légèrement en
        // retard a été juge préférable à une forme qui change deux fois par
        // morceau.
        true
    }

    /// Retient un enrichissement s'il concerne bien ce qui joue. Renvoie `true`
    /// s'il a été retenu (l'affichage doit alors être recomposé).
    ///
    /// Deux refus, tous deux nécessaires :
    /// - identité qui ne correspond pas : c'est le garde-fou de péremption,
    ///   sans lui la réponse lente d'un plugin sur le morceau précédent
    ///   écraserait le morceau courant ;
    /// - enrichissement entièrement vide : il compte comme une non-réponse,
    ///   sinon un plugin prioritaire qui reconnaît l'identité sans rien savoir
    ///   encore bloquerait un plugin moins prioritaire qui, lui, sait.
    ///
    /// « Entièrement vide » veut dire **rien du tout**, pochette comprise, et
    /// c'est une jonction où deux mécanismes justes s'annulaient : ce refus
    /// est antérieur aux pochettes et se fondait sur `Enrichment::is_empty`,
    /// qui ignore délibérément `cover` pour qu'une pochette seule ne gagne
    /// pas l'arbitrage du *texte*. Conséquence mesurée : le relai générique
    /// de `musicbrainz` — qui émet précisément une pochette et rien d'autre,
    /// et qui est la raison même du réétagement du protocole — était refusé à
    /// la porte, sans autre trace qu'un `debug!`. Le fichier taggé sans image
    /// et la radio qui donne du texte sans photo restaient donc noirs.
    pub fn ajoute(&mut self, plugin: &str, e: Enrichment) -> bool {
        // Normalisation ici plutôt qu'au seul site d'appel : `is_empty` n'a de
        // sens qu'après elle, et cette méthode est publique. Idempotent, et
        // l'invariant devient local au lieu de reposer sur la discipline de
        // l'appelant.
        let e = e.cleaned();
        let Some(courante) = &self.identity else {
            tracing::debug!("enrichment from {plugin} ignored: nothing playing anymore");
            return false;
        };
        if &e.identity != courante {
            tracing::debug!("enrichment from {plugin} stale, ignored");
            return false;
        }
        // `is_empty()` ne parle que du **texte**. La pochette avait déjà dû
        // être exemptée ici ; l'année et les liens sont dans le même cas, et
        // les oublier les perdrait en silence — un contributeur qui n'apporte
        // qu'une année serait compté comme n'ayant rien répondu, et sa valeur
        // jetée avant même d'atteindre l'arbitrage.
        //
        // Aucun de nos greffons n'est dans ce cas aujourd'hui : tous portent
        // du texte quand ils portent une année. C'est précisément pour ça que
        // l'oubli aurait été invisible.
        if e.is_empty() && e.cover.is_none() && e.year.is_none() && e.links.is_empty() {
            tracing::debug!("empty enrichment from {plugin}, counted as no response");
            return false;
        }
        if !self.ordre.iter().any(|n| n == plugin) {
            tracing::warn!("enrichment from an undeclared metadata plugin: {plugin}");
            return false;
        }
        // Rien de nouveau : ne pas le signaler. Un plugin qui rouvre sa
        // connexion à un flux distant réémet le morceau en cours à chaque fois,
        // et sans cette comparaison chaque répétition ferait une écriture vers
        // les afficheurs et une trame SSE vers chaque navigateur connecté —
        // indéfiniment si le tiers ferme aussitôt. `set_icy` déduplique déjà.
        if self.enrichissements.get(plugin) == Some(&e) {
            return false;
        }
        // `enrichissements` est la troisième entrée de `cover_retenue`, à
        // égalité avec `cover_source` et `cover_tags` : un enrichissement qui
        // change la référence retenue (un greffon qui écrase répond après un
        // `fill_only`, par exemple) doit invalider la clé publiée exactement
        // comme `set_cover_source`/`set_cover_tags` le font déjà, sous peine
        // de republier une image périmée sous le nom du nouveau contributeur.
        let avant = self.cover_retenue();
        self.enrichissements.insert(plugin.to_string(), e);
        if self.cover_retenue() != avant {
            self.cover_cle = None;
        }
        true
    }

    /// Nom du plugin dont l'enrichissement est retenu, s'il y en a un.
    ///
    /// C'est **le gagnant**, pas le dernier à avoir répondu : toute la règle
    /// d'ordre est justifiée par la prévisibilité pour qui débogue, et c'est le
    /// seul instrument de ce débogage. Un `fill_only` en est exclu : c'est un
    /// complément, pas un gagnant, et le nommer comme tel désignerait le
    /// mauvais coupable devant un affichage douteux.
    pub fn gagnant(&self) -> Option<&str> {
        self.ordre
            .iter()
            .find(|p| self.enrichissements.get(*p).is_some_and(|e| !e.fill_only))
            .map(String::as_str)
    }

    /// Retient la pochette déclarée par la Source. `true` si c'est du neuf.
    pub fn set_cover_source(&mut self, c: Option<CoverRef>, origine: &str) -> bool {
        let neuf = c.map(|r| (r, origine.to_string()));
        if self.cover_source == neuf {
            return false;
        }
        self.cover_source = neuf;
        // La référence retenue a changé : la clé publiée ne la décrit plus.
        self.cover_cle = None;
        true
    }

    /// Retient la pochette embarquée que le cœur a extraite. `true` si neuf.
    pub fn set_cover_tags(&mut self, c: Option<CoverRef>) -> bool {
        if self.cover_tags == c {
            return false;
        }
        self.cover_tags = c;
        self.cover_cle = None;
        true
    }

    /// La pochette qui gagne, et qui l'a fournie.
    ///
    /// L'ordre n'est pas une liste de priorités arbitraire : il découle des
    /// étages et des intentions. La Source d'abord — le fichier posé dans le
    /// répertoire est l'image choisie à la main. Le cœur ensuite, qui
    /// **complète** : il ne remplace pas ce que la Source a dit, et c'est ce
    /// qui donne au `folder.jpg` sa préséance sans qu'aucune convention n'ait
    /// à être inversée. Les greffons enfin, dans l'ordre de déclaration, un
    /// `fill_only` ne prenant la place de personne.
    ///
    /// Une référence dont la récupération a **échoué** est sautée, à son étage
    /// comme aux autres (voir `pochettes_echouees`) : la préséance dit qui l'on
    /// préfère, elle ne dit pas de préférer indéfiniment une image que
    /// l'appareil n'a pas réussi à obtenir.
    pub fn cover_retenue(&self) -> Option<(CoverRef, String)> {
        if let Some((r, o)) = &self.cover_source {
            if !self.a_echoue(r) {
                return Some((r.clone(), o.clone()));
            }
        }
        if let Some(r) = &self.cover_tags {
            if !self.a_echoue(r) {
                return Some((r.clone(), ORIGINE_TAGS.to_string()));
            }
        }
        // Un greffon qui écrase d'abord, puis un `fill_only`. Deux passes
        // plutôt qu'une : sinon un `fill_only` déclaré haut dans
        // `plugins.toml` passerait devant un greffon spécialisé déclaré plus
        // bas, ce qui est exactement l'inverse de son intention.
        for fill_only in [false, true] {
            for plugin in &self.ordre {
                if let Some(e) = self.enrichissements.get(plugin) {
                    if e.fill_only == fill_only {
                        if let Some(r) = &e.cover {
                            if !self.a_echoue(r) {
                                return Some((r.clone(), plugin.clone()));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Cette référence a-t-elle déjà échoué pour ce qui joue ?
    fn a_echoue(&self, r: &CoverRef) -> bool {
        !self.pochettes_echouees.is_empty()
            && self.pochettes_echouees.contains(&crate::cover::cle(r))
    }

    /// Note qu'une récupération a échoué pour cette clé. Renvoie `true` si la
    /// référence retenue en a changé — l'appelant doit alors relancer une
    /// récupération et republier.
    ///
    /// Le cœur apprend l'échec sur son canal de retour (`succes == false`),
    /// et c'est le seul endroit où il l'apprend : sans cette note, il
    /// redésignerait la même référence morte à chaque passage.
    pub fn marque_pochette_echouee(&mut self, cle: String) -> bool {
        let avant = self.cover_retenue();
        if !self.pochettes_echouees.insert(cle) {
            return false;
        }
        let apres = self.cover_retenue();
        if apres != avant {
            // Même raison qu'ailleurs : la clé publiée ne décrit plus la
            // référence retenue, et la laisser afficherait l'image d'un
            // contributeur sous le nom d'un autre.
            self.cover_cle = None;
            return true;
        }
        false
    }

    /// Publie la clé du cache. `None` = plus rien à montrer.
    pub fn set_cover_href(&mut self, cle: Option<String>) {
        self.cover_cle = cle;
    }

    /// Clé déjà publiée, s'il y en a une. Sert à `Core::lance_pochette` pour
    /// éviter de relancer une récupération dont le résultat est déjà à
    /// l'écran — un enrichissement retenu qui republie à l'identique (une
    /// station qui reconfirme ses métadonnées toutes les trente secondes,
    /// par exemple) ne doit pas relancer une tâche pour un travail déjà fait.
    pub fn cover_publiee(&self) -> Option<&str> {
        self.cover_cle.as_deref()
    }

    /// Ce qui est déjà connu, tel qu'un contributeur a besoin de le voir.
    ///
    /// `cover` dit qu'une pochette est **tenue**, jamais laquelle : un
    /// contributeur n'a pas besoin de l'image pour décider s'il doit en
    /// chercher une.
    ///
    /// « Tenue » veut dire *une référence retenue dont la récupération n'a pas
    /// échoué* — c'est `cover_retenue` qui écarte les échouées, donc ce booléen
    /// redevient faux dès qu'une référence promise s'avère morte. C'est ce qui
    /// rend vraie la promesse de la documentation : « faute d'une pochette,
    /// `musicbrainz` complète depuis l'artiste et l'album que ce greffon vient
    /// de fournir ».
    ///
    /// Passe par `texte_compose()` plutôt que par `etat()` : ce dernier
    /// calcule aussi `cover_href`/`cover_origin`, qu'un greffon ne doit
    /// jamais voir (voir la doc de `Known`), et recalculerait la pochette une
    /// seconde fois après celle faite ici pour `cover`.
    pub fn known(&self) -> ritornello_proto::Known {
        let m = self.texte_compose();
        ritornello_proto::Known {
            artist: m.artist,
            title: m.title,
            album: m.album,
            duration_s: m.duration_s,
            year: m.year,
            cover: self.cover_retenue().is_some(),
            // Verbatim, et depuis `self.icy` et non depuis `m` : `m` est le
            // texte **composé**, où l'ICY n'apparaît qu'en dernier recours.
            stream_title: self.icy.clone(),
        }
    }

    /// Résolution, dans l'ordre : le bloc de texte du contributeur retenu,
    /// complété par les `fill_only`, plus la pochette retenue si les octets
    /// sont en main.
    pub fn etat(&self) -> Morceau {
        let mut m = self.texte_compose();
        // Ne calculer `cover_retenue()` — un parcours de `ordre`, un clone de
        // `CoverRef` et de son origine — que s'il y a une clé à publier :
        // sans clé, aucune pochette n'atteint l'affichage de toute façon
        // (voir `set_cover_href`), et cette méthode est appelée au moins une
        // fois par seconde tant qu'un morceau joue.
        if let Some(cle) = &self.cover_cle {
            if let Some((_, origine)) = self.cover_retenue() {
                m.cover_href = Some(format!("{}{cle}", crate::cover::PREFIXE_HREF));
                m.cover_origin = Some(origine);
            }
        }
        m
    }

    /// Le texte composé : le bloc du contributeur retenu (voir
    /// `bloc_de_texte`), complété par les `fill_only`. Sans la pochette —
    /// `etat()` l'ajoute pour l'affichage, `known()` n'en a pas besoin (voir
    /// sa documentation).
    ///
    /// Les `fill_only` comblent les trous du bloc, sans jamais le contredire.
    /// On ne compose pas champ par champ entre deux contributeurs qui
    /// écrasent : cela mélangerait deux lectures du même flux — l'artiste de
    /// l'un, l'album de l'autre — et afficherait un morceau qui n'existe pas.
    fn texte_compose(&self) -> Morceau {
        let mut m = self.bloc_de_texte();
        for plugin in &self.ordre {
            let Some(e) = self.enrichissements.get(plugin) else { continue };
            if !e.fill_only {
                continue;
            }
            if m.artist.is_none() {
                m.artist = e.artist.clone();
            }
            if m.title.is_none() {
                m.title = e.title.clone();
            }
            if m.album.is_none() {
                m.album = e.album.clone();
            }
            if m.duration_s.is_none() {
                m.duration_s = e.duration_s;
            }
            if m.year.is_none() {
                m.year = e.year;
            }
            // Même règle que les autres champs, décidée avec le propriétaire :
            // le gagnant l'emporte, un `fill_only` ne fait que combler un vide.
            // Pas de fusion par plateforme — ce serait une politique inventée
            // pour un cas que nos sources ne produisent pas, aucune ne donnant
            // à la fois du YouTube et du Deezer.
            if m.links.is_empty() {
                m.links = e.links.clone();
            }
        }
        m
    }

    /// Le bloc de texte du contributeur retenu : le premier greffon qui
    /// **écrase**, sinon les tags du fichier, sinon l'ICY brut, sinon rien.
    ///
    /// Les tags s'intercalent entre les deux couches préexistantes, et c'est
    /// leur place naturelle : un plugin `metadata` va chercher au loin ce que
    /// le fichier ne dit pas (une base en ligne, un flux séparé) et doit donc
    /// garder la main ; l'ICY, lui, décrit un flux, pas un fichier. En
    /// pratique tags et ICY ne coexistent jamais — l'extraction rend `None`
    /// dès qu'une clé `icy-*` est présente, précisément pour qu'une station
    /// annonçant son propre nom dans `title` ne vienne pas supplanter
    /// l'`icy-title` qui porte le vrai morceau.
    ///
    /// L'ICY est repris **tel quel** dans `title`, sans découpage sur `" - "` :
    /// la convention existe mais n'est pas garantie, et un enrichissement de
    /// plugin fournit de toute façon des champs déjà séparés. Une station qui
    /// n'annonce que son propre nom ou ses jingles verra donc cela s'afficher —
    /// c'est ce qu'elle émet.
    fn bloc_de_texte(&self) -> Morceau {
        for plugin in &self.ordre {
            if let Some(e) = self.enrichissements.get(plugin) {
                // Deux exclusions, et la seconde n'est pas la première : un
                // `fill_only` n'est pas candidat par intention, un
                // enrichissement **sans aucun texte** ne l'est pas par
                // contenu. Depuis que `ajoute` retient une pochette seule
                // (voir sa doc), un greffon qui écrase peut n'apporter qu'une
                // image — c'est le cas réel d'un relevé Radio France ou OUI FM
                // qui porte `coverUrl` sans titre. Sans cette seconde
                // exclusion, il deviendrait le bloc retenu avec des champs
                // tous à `None` et effacerait le titre que les tags ou l'ICY
                // affichaient : la pochette gagnée coûterait le texte.
                //
                // `is_empty()` et non « pas de titre » : c'est exactement le
                // prédicat que le protocole utilise déjà pour dire « cette
                // réponse ne dit rien du texte », pochette et durée exclues.
                if e.fill_only || e.is_empty() {
                    continue;
                }
                return Morceau {
                    artist: e.artist.clone(),
                    title: e.title.clone(),
                    album: e.album.clone(),
                    duration_s: e.duration_s,
                    year: e.year,
                    links: e.links.clone(),
                    origin: Some(plugin.clone()),
                    ..Default::default()
                };
            }
        }
        if let Some(tags) = &self.tags {
            return tags.clone();
        }
        match &self.icy {
            Some(icy) => Morceau {
                title: Some(icy.clone()),
                origin: Some(ORIGINE_ICY.to_string()),
                ..Default::default()
            },
            None => Morceau::default(),
        }
    }

    /// Position déclarée par le **gagnant** de l'arbitrage, s'il en déclare
    /// une.
    ///
    /// Ignore les `fill_only`, exactement comme `gagnant()` : une position
    /// n'a de sens que venant de qui suit réellement l'avancement du flux,
    /// jamais d'un complément qui ne fait que combler un texte ou une
    /// pochette. `Core::handle_enrichment` n'appelle cette méthode qu'après
    /// avoir vérifié `gagnant() == Some(plugin)` pour décider s'il faut
    /// réancrer : les deux méthodes doivent nommer le même gagnant, sinon le
    /// garde-fou se déclencherait pour ancrer sur la valeur d'un contributeur
    /// différent de celui qu'il vient d'identifier — ou sur `None` si ce
    /// contributeur, déclaré avant le gagnant dans `plugins.toml`, ne
    /// déclare pas lui-même de position.
    ///
    /// Sortie à part de `etat()` plutôt que glissée dans `Morceau` : `Morceau`
    /// décrit ce qui est affichable d'un morceau, valeurs stables tant qu'il
    /// joue, alors qu'une position ne vaut que pour l'instant où elle a été
    /// dite. Ce module n'a d'ailleurs aucune horloge, et c'est délibéré (voir
    /// l'en-tête) : c'est au cœur d'ancrer cette valeur et de l'avancer.
    pub fn position_s(&self) -> Option<u32> {
        for plugin in &self.ordre {
            if let Some(e) = self.enrichissements.get(plugin) {
                if e.fill_only {
                    continue;
                }
                return e.position_s;
            }
        }
        None
    }

    /// Durée déclarée par le **gagnant** de l'arbitrage, s'il en déclare une,
    /// complétée par un `fill_only` s'il n'en déclare pas.
    ///
    /// Volontairement **asymétrique** avec `position_s()` juste au-dessus,
    /// et ce n'est pas un oubli : cette méthode laisse un `fill_only` combler
    /// une durée que le gagnant ne déclare pas, là où `gagnant()` les ignore
    /// purement. `etat()` fait de même (un fichier dont les tags ne portent
    /// pas la durée, complétée par un greffon qui la connaît) et `known()`
    /// republie cette valeur composée : un accesseur qui ignorerait les
    /// `fill_only` plafonnerait la position (`Core::rafraichit_position`)
    /// contre une durée différente de celle affichée à l'écran. Une position,
    /// elle, n'a pas cet équivalent — personne ne « complète » un avancement
    /// — d'où l'asymétrie avec `position_s()`.
    ///
    /// Elle ne compose donc **pas tout à fait** comme `etat()` : les
    /// enrichissements seuls sont consultés, jamais `self.tags`. Une version
    /// antérieure de ce commentaire promettait l'inverse. Inerte aujourd'hui,
    /// et il faut le dire pour que la prochaine lecture ne s'y arrête pas :
    /// `player::mpv::file_tags` met `duration_s: None` en dur — mpv ne
    /// rapporte pas la durée dans sa propriété `metadata`, elle vient de sa
    /// propre `Progression` — donc la couche des tags n'a jamais de durée à
    /// apporter et la divergence n'a rien à mordre. Le jour où elle en
    /// porterait une, c'est ici qu'il faudrait l'ajouter, entre le gagnant et
    /// les `fill_only`, à la place exacte qu'occupent les tags dans
    /// `bloc_de_texte`.
    ///
    /// Même raison d'être que `position_s` juste au-dessus pour la sortie à
    /// part de `etat()` : lire un entier ne doit pas coûter la
    /// reconstruction d'un `Morceau` entier, chaînes clonées comprises — ce
    /// que le plafonnement de la position ferait une fois par seconde
    /// pendant toute la lecture d'un flux.
    pub fn duration_s(&self) -> Option<u32> {
        let mut duree = None;
        for plugin in &self.ordre {
            if let Some(e) = self.enrichissements.get(plugin) {
                if !e.fill_only {
                    duree = e.duration_s;
                    break;
                }
            }
        }
        if duree.is_none() {
            for plugin in &self.ordre {
                if let Some(e) = self.enrichissements.get(plugin) {
                    if e.fill_only && e.duration_s.is_some() {
                        duree = e.duration_s;
                        break;
                    }
                }
            }
        }
        duree
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn enrichissement(identity: Value, artist: &str, title: &str) -> Enrichment {
        Enrichment {
            identity,
            artist: Some(artist.into()),
            title: Some(title.into()),
            ..Default::default()
        }
    }

    /// Fabrique : un enrichissement qui ecrase, avec les champs donnes.
    fn ecrase(id: &Value, artist: Option<&str>, album: Option<&str>) -> Enrichment {
        Enrichment {
            identity: id.clone(),
            artist: artist.map(str::to_string),
            title: Some("T".into()),
            album: album.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn un_contributeur_qui_ecrase_fournit_son_bloc_et_le_fill_only_comble() {
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["specifique".into(), "generique".into()]);
        m.set_identity(Some(id.clone()));
        // Le specifique connait l'artiste, pas l'album.
        assert!(m.ajoute("specifique", ecrase(&id, Some("A"), None)));
        // Le generique complete : il ne remplace pas l'artiste, il remplit
        // l'album qui manquait.
        assert!(m.ajoute(
            "generique",
            Enrichment {
                identity: id.clone(),
                artist: Some("PAS LUI".into()),
                album: Some("ALBUM".into()),
                fill_only: true,
                ..Default::default()
            }
        ));
        let etat = m.etat();
        assert_eq!(etat.artist.as_deref(), Some("A"), "un fill_only ne remplace jamais");
        assert_eq!(etat.album.as_deref(), Some("ALBUM"), "un fill_only comble un trou");
        assert_eq!(etat.origin.as_deref(), Some("specifique"));
    }

    #[test]
    fn deux_contributeurs_qui_ecrasent_ne_sont_pas_melanges() {
        // Composer champ par champ entre deux qui ecrasent melangerait deux
        // lectures du meme flux et afficherait un morceau qui n'existe pas.
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["premier".into(), "second".into()]);
        m.set_identity(Some(id.clone()));
        m.ajoute("premier", ecrase(&id, Some("A"), None));
        m.ajoute("second", ecrase(&id, Some("B"), Some("ALBUM DU SECOND")));
        let etat = m.etat();
        assert_eq!(etat.artist.as_deref(), Some("A"));
        assert_eq!(etat.album, None, "le bloc du premier fait foi, trous compris");
    }

    #[test]
    fn la_pochette_suit_les_etages_source_puis_tags_puis_greffon() {
        let id = json!({"kind": "file", "path": "/mnt/nas/a.flac"});
        let mut m = Metadonnees::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(id.clone()));

        // Le greffon seul : c'est lui qu'on retient.
        assert!(m.ajoute(
            "musicbrainz",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() }),
                fill_only: true,
                ..Default::default()
            }
        ));
        let (_, origine) = m.cover_retenue().expect("le greffon fournit une pochette");
        assert_eq!(origine, "musicbrainz");

        // La pochette embarquee, lue par le coeur, passe devant le greffon.
        assert!(m.set_cover_tags(Some(CoverRef::Path { path: "/tmp/embarquee.jpg".into() })));
        assert_eq!(m.cover_retenue().unwrap().1, ORIGINE_TAGS);

        // Le fichier pose a cote, declare par la Source, passe devant tout.
        assert!(m.set_cover_source(
            Some(CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }),
            "files"
        ));
        let (r, origine) = m.cover_retenue().unwrap();
        assert_eq!(origine, "files");
        assert_eq!(r, CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() });
    }

    #[test]
    fn une_pochette_seule_est_retenue_et_nomme_son_contributeur() {
        // Le defaut central de la couche : `Enrichment::is_empty` ignore
        // deliberement `cover` — pour qu'une pochette seule ne gagne pas
        // l'arbitrage du *texte* — et `ajoute` refusait tout enrichissement
        // vide, un refus anterieur aux pochettes. Le relai generique de
        // `musicbrainz` emet precisement une pochette et rien d'autre : il
        // etait donc refuse a la porte, et le chemin du Cover Art Archive
        // — la raison meme du reetagement du protocole — ne contribuait jamais
        // rien. Tous les autres tests de pochette d'ici donnent un `title` a
        // leur enrichissement, et c'est pourquoi aucun ne le voyait.
        let id = json!({"kind": "file", "path": "/mnt/nas/a.flac"});
        let mut m = Metadonnees::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.ajoute(
            "musicbrainz",
            Enrichment {
                identity: id,
                cover: Some(CoverRef::Url {
                    url: "https://coverartarchive.org/release/x/front-500".into()
                }),
                fill_only: true,
                ..Default::default()
            }
        ));
        let (r, origine) = m.cover_retenue().expect("une pochette seule doit etre retenue");
        assert_eq!(origine, "musicbrainz");
        assert_eq!(r, CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() });
        assert!(m.known().cover);
        // Et rien du texte : la convention de `is_empty` n'a pas bouge.
        assert!(m.etat().est_vide(), "une pochette seule n'apporte aucun texte");
    }

    #[test]
    fn une_pochette_seule_qui_ecrase_ne_vide_pas_le_titre_deja_affiche() {
        // Le piege de la moitie 2, et il est reel : `radiofrance-metas` et
        // `ouifm-metas` ecrasent (`fill_only` faux) et construisent leur trame
        // depuis un releve qui peut porter `coverUrl` sans titre. Retenu pour
        // sa pochette — il faut bien qu'il le soit, moitie 1 — un tel
        // enrichissement ne doit pas pour autant devenir le bloc de texte
        // retenu avec tous ses champs a `None` : ce serait echanger la ligne
        // affichee contre une image.
        let mut m = Metadonnees::new(vec!["radiofrance".into()]);
        let id = json!({"kind": "stream", "url": "https://fip"});
        m.set_identity(Some(id.clone()));
        m.set_icy("Miles Davis - So What".into());
        assert!(m.ajoute(
            "radiofrance",
            Enrichment {
                identity: id.clone(),
                cover: Some(CoverRef::Url { url: "https://www.radiofrance.fr/x.jpg".into() }),
                ..Default::default()
            }
        ));
        let etat = m.etat();
        assert_eq!(etat.title.as_deref(), Some("Miles Davis - So What"), "l'ICY doit rester affiche");
        assert_eq!(etat.origin.as_deref(), Some("icy"));
        assert_eq!(m.cover_retenue().unwrap().1, "radiofrance", "et la pochette est bien la sienne");

        // Meme garantie face aux tags du fichier, l'autre couche que ce bloc
        // vide aurait recouverte.
        let mut m = Metadonnees::new(vec!["radiofrance".into()]);
        m.set_identity(Some(id.clone()));
        m.set_tags(Morceau {
            title: Some("So What".into()),
            origin: Some(ORIGINE_TAGS.to_string()),
            ..Default::default()
        });
        assert!(m.ajoute(
            "radiofrance",
            Enrichment {
                identity: id,
                cover: Some(CoverRef::Url { url: "https://www.radiofrance.fr/x.jpg".into() }),
                ..Default::default()
            }
        ));
        assert_eq!(m.etat().title.as_deref(), Some("So What"));
        assert_eq!(m.etat().origin.as_deref(), Some(ORIGINE_TAGS));
    }

    #[test]
    fn un_fill_only_declare_avant_ne_passe_pas_devant_un_greffon_specialise_pour_la_pochette() {
        // Le mecanisme que le brief justifie explicitement : deux passes, pas
        // une, sinon un `fill_only` declare haut dans `plugins.toml`
        // passerait devant un greffon specialise declare plus bas, l'inverse
        // de son intention. En collabant les deux passes en une seule boucle,
        // ce test echoue alors que les autres n'en disent rien.
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadonnees::new(vec!["filler".into(), "specialise".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.ajoute(
            "filler",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/a/front-500".into() }),
                fill_only: true,
                ..Default::default()
            }
        ));
        assert!(m.ajoute(
            "specialise",
            Enrichment {
                identity: id,
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() }),
                ..Default::default()
            }
        ));
        let (r, origine) = m.cover_retenue().expect("le greffon specialise fournit une pochette");
        assert_eq!(origine, "specialise", "declare plus bas, il ne doit pourtant pas ceder au fill_only");
        assert_eq!(r, CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() });
    }

    #[test]
    fn un_known_qui_ne_porte_que_licy_brut_est_impossible() {
        // `Known::est_vide` ne compte pas `stream_title`, et j'ai d'abord cru a
        // un oubli : ce predicat est le `skip_serializing_if` de
        // `NowPlaying::known`, donc un `Known` juge vide **disparait de la
        // trame**, et un greffon ne reverrait jamais la chaine ICY brute.
        //
        // Ce n'est pas un defaut : l'etat est inatteignable. Des que `icy` est
        // renseigne, `bloc_de_texte` garantit un titre — celui d'un gagnant, des
        // tags, ou l'ICY lui-meme en dernier recours — donc `est_vide` est faux
        // par un autre champ. Et `set_icy` ne recoit jamais de chaine blanche,
        // `player::mpv::icy_title` la filtrant en amont.
        //
        // Mais cette surete tient a un invariant tenu **ailleurs**, pas au
        // predicat. Ce test le verrouille : si `bloc_de_texte` cessait un jour
        // de reporter l'ICY dans le titre, l'omission deviendrait une perte
        // silencieuse, et c'est ici qu'on l'apprendrait.
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["greffon".into()]);
        m.set_identity(Some(id));
        assert!(m.set_icy("Mandrillus Sphynx - Bikwix".into()));
        let k = m.known();
        assert_eq!(k.stream_title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert!(!k.est_vide(), "l'ICY seul remplit deja le titre, donc la trame le porte");
        assert_eq!(k.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"), "l'invariant en question");
    }

    #[test]
    fn lannee_et_les_liens_suivent_la_regle_du_gagnant() {
        // Regle tranchee avec le proprietaire : le gagnant l'emporte, un
        // `fill_only` ne fait que combler un vide. Pas de fusion par
        // plateforme — ce serait une politique inventee pour un cas que nos
        // sources ne produisent pas.
        use ritornello_proto::Link;
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["gagnant".into(), "filler".into()]);
        m.set_identity(Some(id.clone()));
        m.ajoute(
            "gagnant",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                year: Some(1959),
                links: vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
                ..Default::default()
            },
        );
        m.ajoute(
            "filler",
            Enrichment {
                identity: id.clone(),
                year: Some(1999),
                links: vec![Link::Deezer { url: "https://www.deezer.com/track/1".into() }],
                fill_only: true,
                ..Default::default()
            },
        );
        let etat = m.etat();
        assert_eq!(etat.year, Some(1959), "le fill_only n'ecrase pas");
        assert_eq!(
            etat.links,
            vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
            "pas de fusion : les liens du gagnant, et eux seuls"
        );
    }

    #[test]
    fn un_fill_only_comble_lannee_et_les_liens_que_le_gagnant_ignore() {
        use ritornello_proto::Link;
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["gagnant".into(), "filler".into()]);
        m.set_identity(Some(id.clone()));
        m.ajoute(
            "gagnant",
            Enrichment { identity: id.clone(), title: Some("T".into()), ..Default::default() },
        );
        m.ajoute(
            "filler",
            Enrichment {
                identity: id.clone(),
                year: Some(1999),
                links: vec![Link::Deezer { url: "https://www.deezer.com/track/1".into() }],
                fill_only: true,
                ..Default::default()
            },
        );
        let etat = m.etat();
        assert_eq!(etat.year, Some(1999));
        assert_eq!(etat.links.len(), 1);
        // Et `known()` republie l'annee composee, pour qu'un greffon sache
        // qu'elle est deja tenue.
        assert_eq!(m.known().year, Some(1999));
    }

    #[test]
    fn le_gagnant_ignore_un_fill_only_arrive_en_premier() {
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["filler".into(), "specialise".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.ajoute(
            "filler",
            Enrichment { identity: id.clone(), title: Some("T".into()), fill_only: true, ..Default::default() }
        ));
        assert_eq!(m.gagnant(), None, "un fill_only seul n'est jamais le gagnant");
        assert!(m.ajoute(
            "specialise",
            Enrichment { identity: id, title: Some("T2".into()), ..Default::default() }
        ));
        assert_eq!(m.gagnant(), Some("specialise"));
    }

    #[test]
    fn un_greffon_qui_ecrase_invalide_la_cle_dune_pochette_de_fill_only_deja_publiee() {
        // Sequence qui echappait a la premiere version : un fill_only fournit
        // une pochette, le coeur va chercher les octets et publie la cle ;
        // puis un greffon specialise repond avec une pochette differente.
        // `ajoute` est la troisieme voie de mutation de la reference retenue,
        // a egalite avec `set_cover_source`/`set_cover_tags`, et doit donc
        // invalider la cle exactement comme elles le font deja.
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadonnees::new(vec!["specialise".into(), "musicbrainz".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.ajoute(
            "musicbrainz",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/a/front-500".into() }),
                fill_only: true,
                ..Default::default()
            }
        ));
        assert_eq!(m.cover_retenue().unwrap().1, "musicbrainz");
        m.set_cover_href(Some("clea".into()));
        assert_eq!(m.etat().cover_href.as_deref(), Some("/api/cover/clea"));

        assert!(m.ajoute(
            "specialise",
            Enrichment {
                identity: id,
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() }),
                ..Default::default()
            }
        ));
        let (r, origine) = m.cover_retenue().unwrap();
        assert_eq!(origine, "specialise");
        assert_eq!(r, CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() });
        assert!(
            m.etat().cover_href.is_none(),
            "la cle perimee ne doit pas rester publiee sous la nouvelle origine"
        );
    }

    #[test]
    fn un_changement_didentite_vide_la_pochette_comme_le_reste() {
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadonnees::new(vec![]);
        m.set_identity(Some(id));
        m.set_cover_source(Some(CoverRef::Path { path: "/a/folder.jpg".into() }), "files");
        m.set_cover_tags(Some(CoverRef::Path { path: "/b/embarquee.jpg".into() }));
        m.set_cover_href(Some("abcd".into()));
        assert!(m.set_identity(Some(json!({"kind": "file", "path": "/b.flac"}))));
        assert!(m.cover_retenue().is_none(), "laisser la pochette precedente serait plus trompeur que rien");
        assert!(m.etat().cover_href.is_none());
    }

    #[test]
    fn une_pochette_dont_la_recuperation_a_echoue_laisse_la_place_au_suivant() {
        // La conception l'anticipe : « un motif qui casse rend un silence ».
        // Sans memoire de l'echec, ce silence etait definitif — `known.cover`
        // restait vrai parce qu'une reference etait *retenue*, donc
        // `musicbrainz` se taisait, et il aurait de toute facon ete distance
        // s'il avait parle.
        let id = json!({"kind": "stream", "url": "https://fip"});
        let mut m = Metadonnees::new(vec!["radiofrance".into(), "musicbrainz".into()]);
        m.set_identity(Some(id.clone()));
        let morte = CoverRef::Url { url: "https://api.radiofrance.fr/v1/embed/image/rouille".into() };
        assert!(m.ajoute(
            "radiofrance",
            Enrichment {
                identity: id.clone(),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                cover: Some(morte.clone()),
                ..Default::default()
            }
        ));
        assert!(m.known().cover, "tant qu'on ne sait pas, la reference est tenue");
        // Une cle qui n'est pas la sienne ne change rien.
        assert!(!m.marque_pochette_echouee("une-autre-cle".into()));
        assert!(m.known().cover);

        m.set_cover_href(Some(crate::cover::cle(&morte)));
        assert!(m.marque_pochette_echouee(crate::cover::cle(&morte)));
        assert!(!m.known().cover, "une promesse non tenue ne doit plus faire taire les autres");
        assert!(m.cover_retenue().is_none());
        assert!(m.etat().cover_href.is_none(), "la cle publiee ne decrit plus rien");
        // Le texte, lui, n'a pas bouge : c'est la pochette qui a echoue.
        assert_eq!(m.etat().title.as_deref(), Some("So What"));

        // Ce qui permet enfin a `musicbrainz` de compenser.
        let caa = CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() };
        assert!(m.ajoute(
            "musicbrainz",
            Enrichment {
                identity: id,
                cover: Some(caa.clone()),
                fill_only: true,
                ..Default::default()
            }
        ));
        let (r, origine) = m.cover_retenue().expect("le compensateur doit passer");
        assert_eq!((r, origine.as_str()), (caa, "musicbrainz"));
    }

    #[test]
    fn un_changement_didentite_oublie_les_echecs_de_pochette() {
        // Un echec vaut pour une reference **de ce morceau-la** : la meme URL
        // peut repondre au suivant (un CDN qui s'est reveille), et une liste
        // qui survivrait a l'identite empecherait de la redemander.
        let mut m = Metadonnees::new(vec!["radiofrance".into()]);
        m.set_identity(Some(json!({"url": "un"})));
        let r = CoverRef::Url { url: "https://www.radiofrance.fr/x.jpg".into() };
        m.set_cover_source(Some(r.clone()), "radio");
        assert!(m.marque_pochette_echouee(crate::cover::cle(&r)));
        assert!(m.cover_retenue().is_none());

        assert!(m.set_identity(Some(json!({"url": "deux"}))));
        m.set_cover_source(Some(r.clone()), "radio");
        assert_eq!(
            m.cover_retenue().map(|(r, _)| r),
            Some(r),
            "l'ardoise des echecs doit etre remise a zero avec le reste"
        );
    }

    #[test]
    fn known_expose_ce_qui_est_connu_et_si_une_pochette_est_tenue() {
        let id = json!({"kind": "stream"});
        let mut m = Metadonnees::new(vec!["p".into()]);
        m.set_identity(Some(id.clone()));
        m.ajoute("p", ecrase(&id, Some("A"), None));
        let k = m.known();
        assert_eq!(k.artist.as_deref(), Some("A"));
        assert_eq!(k.album, None, "un champ vide est ce qui invite un contributeur a chercher");
        assert!(!k.cover);

        m.set_cover_tags(Some(CoverRef::Path { path: "/x/c.jpg".into() }));
        assert!(m.known().cover, "une pochette tenue doit faire taire un fill_only");
    }

    #[test]
    fn le_cover_href_publie_est_l_url_locale() {
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadonnees::new(vec![]);
        m.set_identity(Some(id));
        m.set_cover_source(Some(CoverRef::Path { path: "/a/folder.jpg".into() }), "files");
        // Tant que les octets ne sont pas en main, rien n'est publie : l'IHM ne
        // doit jamais recevoir l'URL d'une image cassee.
        assert!(m.etat().cover_href.is_none());
        m.set_cover_href(Some("1a2b3c4d".into()));
        let etat = m.etat();
        assert_eq!(etat.cover_href.as_deref(), Some("/api/cover/1a2b3c4d"));
        assert_eq!(etat.cover_origin.as_deref(), Some("files"));

        // La cle peut redevenir None (fetch invalide, pas encore refait)
        // pendant que la reference elle-meme reste retenue : rien ne doit
        // s'afficher tant qu'aucune cle valide n'est publiee.
        m.set_cover_href(None);
        assert!(m.etat().cover_href.is_none(), "cle effacee, la reference pourtant toujours retenue");
        assert!(m.cover_retenue().is_some(), "la reference elle-meme n'a pas bouge");
    }

    #[test]
    fn un_enrichissement_perime_est_ignore() {
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "deux"})));
        let retenu = m.ajoute("ouifm", enrichissement(json!({"url": "un"}), "A", "T"));
        assert!(!retenu);
        assert!(m.etat().est_vide());
    }

    #[test]
    fn un_changement_didentite_vide_licy_et_les_enrichissements() {
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "un"})));
        assert!(m.set_icy("Station - Jingle".into()));
        assert!(m.ajoute("ouifm", enrichissement(json!({"url": "un"}), "A", "T")));
        assert!(!m.etat().est_vide());

        assert!(m.set_identity(Some(json!({"url": "deux"}))));
        assert!(m.etat().est_vide(), "l'ardoise doit etre remise a zero immediatement");
    }

    #[test]
    fn identite_inchangee_ne_remet_rien_a_zero() {
        // Une Source peut redonner la même identité (relance de flux après
        // coupure) : ce n'est pas un nouveau morceau, l'affichage doit tenir.
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "un"})));
        m.set_icy("Miles Davis - So What".into());
        assert!(!m.set_identity(Some(json!({"url": "un"}))));
        assert_eq!(m.etat().title.as_deref(), Some("Miles Davis - So What"));
    }

    #[test]
    fn un_plugin_lemporte_sur_les_tags_du_fichier() {
        // Même règle que face à l'ICY, et pour la même raison : un plugin va
        // chercher au loin ce que le fichier ne dit pas, et ce qu'il a appris
        // doit rester affiché.
        let mut m = Metadonnees::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(json!({"kind": "file", "path": "/x/03.flac"})));
        m.set_tags(Morceau {
            title: Some("piste 03".into()),
            origin: Some(ORIGINE_TAGS.to_string()),
            ..Default::default()
        });
        m.ajoute(
            "musicbrainz",
            enrichissement(json!({"kind": "file", "path": "/x/03.flac"}), "Miles Davis", "So What"),
        );
        let etat = m.etat();
        assert_eq!(etat.title.as_deref(), Some("So What"));
        assert_eq!(etat.origin.as_deref(), Some("musicbrainz"));
    }

    #[test]
    fn les_tags_lemportent_sur_licy_et_sont_attribues() {
        // Les deux ne coexistent jamais en pratique (l'extraction se tait dès
        // qu'une clé ICY est là), mais l'ordre doit être écrit : l'ICY décrit
        // un flux, les tags décrivent le fichier réellement joué.
        let mut m = Metadonnees::new(vec![]);
        m.set_icy("Station - Jingle".into());
        m.set_tags(Morceau {
            artist: Some("Miles Davis".into()),
            title: Some("So What".into()),
            origin: Some(ORIGINE_TAGS.to_string()),
            ..Default::default()
        });
        let etat = m.etat();
        assert_eq!(etat.title.as_deref(), Some("So What"));
        assert_eq!(etat.origin.as_deref(), Some("tags"));
    }

    #[test]
    fn un_changement_didentite_vide_aussi_les_tags() {
        // Sans cela, les tags de la piste précédente resteraient affichés le
        // temps que mpv publie ceux de la suivante.
        let mut m = Metadonnees::new(vec![]);
        m.set_identity(Some(json!({"kind": "file", "path": "/x/01.mp3"})));
        m.set_tags(Morceau { title: Some("Piste 1".into()), ..Default::default() });
        assert!(m.set_identity(Some(json!({"kind": "file", "path": "/x/02.mp3"}))));
        assert!(m.etat().est_vide());
    }

    #[test]
    fn des_tags_repetes_ne_declenchent_rien() {
        // mpv republie `metadata` plus souvent qu'il ne change : sans cette
        // déduplication, chaque republication ferait repeindre les afficheurs.
        let mut m = Metadonnees::new(vec![]);
        let tags = Morceau { title: Some("So What".into()), ..Default::default() };
        assert!(m.set_tags(tags.clone()));
        assert!(!m.set_tags(tags));
    }

    #[test]
    fn lenrichissement_gagne_sur_licy() {
        // Cas mesuré d'OUI FM : son en-tête ICY vaut le texte de remplissage
        // « Now Playing info goes here », que la surcharge du plugin doit
        // écraser — sinon ce texte s'afficherait à la place du morceau.
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "un"})));
        m.set_icy("Now Playing info goes here".into());
        m.ajoute("ouifm", enrichissement(json!({"url": "un"}), "Shaka Ponk", "Wanna Get Free"));
        let etat = m.etat();
        assert_eq!(etat.artist.as_deref(), Some("Shaka Ponk"));
        assert_eq!(etat.title.as_deref(), Some("Wanna Get Free"));
        assert_eq!(etat.origin.as_deref(), Some("ouifm"));
    }

    #[test]
    fn licy_seul_est_affiche_brut_et_attribue() {
        let mut m = Metadonnees::new(vec![]);
        m.set_identity(Some(json!({"url": "un"})));
        m.set_icy("Mandrillus Sphynx - Bikwix".into());
        let etat = m.etat();
        // Aucun découpage sur " - " : la convention n'est pas garantie.
        assert_eq!(etat.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(etat.artist, None);
        assert_eq!(etat.origin.as_deref(), Some("icy"));
    }

    #[test]
    fn le_premier_plugin_declare_gagne_quel_que_soit_lordre_darrivee() {
        // Le second déclaré répond **en premier** : c'est le cas qui distingue
        // « ordre de déclaration » de « premier arrivé ». Le résultat ne doit
        // pas dépendre de la latence réseau, sinon la même installation
        // afficherait autre chose d'un démarrage à l'autre.
        let mut m = Metadonnees::new(vec!["prioritaire".into(), "secondaire".into()]);
        let id = json!({"url": "un"});
        m.set_identity(Some(id.clone()));
        assert!(m.ajoute("secondaire", enrichissement(id.clone(), "Second", "Titre second")));
        assert_eq!(m.etat().artist.as_deref(), Some("Second"));

        assert!(m.ajoute("prioritaire", enrichissement(id.clone(), "Premier", "Titre premier")));
        assert_eq!(m.etat().artist.as_deref(), Some("Premier"));

        // Et un nouvel enrichissement du moins prioritaire ne reprend pas la main.
        assert!(m.ajoute("secondaire", enrichissement(id, "Second bis", "Titre second bis")));
        assert_eq!(m.etat().artist.as_deref(), Some("Premier"));
    }

    #[test]
    fn un_enrichissement_vide_laisse_gagner_le_suivant() {
        let mut m = Metadonnees::new(vec!["prioritaire".into(), "secondaire".into()]);
        let id = json!({"url": "un"});
        m.set_identity(Some(id.clone()));
        let vide = Enrichment { identity: id.clone(), ..Default::default() };
        assert!(!m.ajoute("prioritaire", vide), "un enrichissement vide compte comme une non-reponse");
        assert!(m.ajoute("secondaire", enrichissement(id, "Second", "Titre")));
        assert_eq!(m.etat().artist.as_deref(), Some("Second"));
    }

    #[test]
    fn un_enrichissement_hors_lecture_est_ignore() {
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        // Plus rien ne joue : rien à enrichir.
        assert!(!m.ajoute("ouifm", enrichissement(json!({"url": "un"}), "A", "T")));
        assert!(m.etat().est_vide());
    }

    #[test]
    fn un_plugin_non_declare_est_refuse() {
        // Sans ce refus, un plugin absent de `plugins.toml` n'aurait aucune
        // priorité définie et n'apparaîtrait jamais dans la résolution : son
        // enrichissement serait stocké pour rien, ce qui donnerait un état
        // silencieusement inerte plutôt qu'un avertissement.
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        let id = json!({"url": "un"});
        m.set_identity(Some(id.clone()));
        assert!(!m.ajoute("inconnu", enrichissement(id, "A", "T")));
        assert!(m.etat().est_vide());
    }

    #[test]
    fn un_plugin_garde_la_priorite_meme_sur_un_titre_icy_plus_recent() {
        // Decision du proprietaire : un plugin `metadata` est prioritaire sur
        // l'ICY **en toutes circonstances**. Une version anterieure effacait les
        // enrichissements a chaque nouveau titre ICY (celui-ci prouvant que le
        // morceau a change), ce qui faisait passer l'affichage par la forme ICY
        // — « Titre - ARTISTE » sur ces flux — avant correction par le plugin,
        // deux fois par morceau.
        //
        // Compromis assume, verifie ici : au changement de morceau, c'est le
        // titre precedent qui reste affiche jusqu'a la trame suivante du plugin.
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        let id = json!({"kind": "stream", "url": "http://ouifm3"});
        m.set_identity(Some(id.clone()));
        m.set_icy("Made Up - TAHITI 80".into());
        m.ajoute("ouifm", enrichissement(id.clone(), "TAHITI 80", "MADE UP"));
        assert_eq!(m.etat().origin.as_deref(), Some("ouifm"));

        // Morceau suivant : le flux l'annonce, le plugin n'a pas encore parle.
        assert!(m.set_icy("Fade To Grey - VISAGE".into()), "l'ICY est bien retenu");
        let etat = m.etat();
        assert_eq!(etat.origin.as_deref(), Some("ouifm"), "le plugin garde la main");
        assert_eq!(etat.artist.as_deref(), Some("TAHITI 80"));

        // Puis le plugin rattrape.
        m.ajoute("ouifm", enrichissement(id, "VISAGE", "FADE TO GREY"));
        assert_eq!(m.etat().artist.as_deref(), Some("VISAGE"));
    }

    #[test]
    fn licy_reprend_la_main_quand_la_station_change() {
        // La priorite du plugin ne vaut que pour **ce qui joue** : changer de
        // station change l'identite, ce qui remet l'ardoise a zero. Sans quoi le
        // titre d'une station suivrait sur la suivante.
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        let une = json!({"kind": "stream", "url": "http://ouifm3"});
        m.set_identity(Some(une.clone()));
        m.ajoute("ouifm", enrichissement(une, "TAHITI 80", "MADE UP"));
        assert_eq!(m.etat().origin.as_deref(), Some("ouifm"));

        m.set_identity(Some(json!({"kind": "stream", "url": "http://fip"})));
        m.set_icy("Miles Davis - So What".into());
        let etat = m.etat();
        assert_eq!(etat.origin.as_deref(), Some("icy"));
        assert_eq!(etat.title.as_deref(), Some("Miles Davis - So What"));
    }

    #[test]
    fn licy_repete_ne_declenche_rien() {
        let mut m = Metadonnees::new(vec![]);
        m.set_identity(Some(json!(1)));
        assert!(m.set_icy("Miles Davis - So What".into()));
        assert!(!m.set_icy("Miles Davis - So What".into()), "Icecast repete le meme en-tete");
    }

    #[test]
    fn un_titre_icy_est_retenu_meme_sans_identite_declaree() {
        // La couche ICY ne dépend pas du bon vouloir de la Source : une Source
        // qui ne déclare aucune identité (plugin tiers, binaire pas encore mis à
        // jour) ne doit pas priver l'appareil de la seule couche qui fonctionne
        // sans plugin. C'est au cœur de savoir si quelque chose joue — voir
        // `Core::handle_icy_title`, qui s'appuie sur `expecting_stream`.
        let mut m = Metadonnees::new(vec![]);
        assert!(m.set_icy("Miles Davis - So What".into()));
        assert_eq!(m.etat().title.as_deref(), Some("Miles Davis - So What"));
        assert_eq!(m.etat().origin.as_deref(), Some("icy"));
    }

    #[test]
    fn un_enrichissement_identique_ne_declenche_rien() {
        // Un plugin qui rouvre sa connexion à un flux distant réémet le morceau
        // en cours à chaque fois. Sans cette déduplication, chaque répétition
        // provoquait une écriture vers les afficheurs et une trame SSE vers
        // chaque navigateur connecté.
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        let id = json!({"url": "un"});
        m.set_identity(Some(id.clone()));
        assert!(m.ajoute("ouifm", enrichissement(id.clone(), "A", "T")));
        assert!(!m.ajoute("ouifm", enrichissement(id.clone(), "A", "T")));
        // Les blancs étant normalisés à l'entrée, la même information sous une
        // autre forme ne passe pas non plus.
        let avec_blancs = Enrichment {
            identity: id.clone(),
            artist: Some("  A ".into()),
            title: Some("T".into()),
            ..Default::default()
        };
        assert!(!m.ajoute("ouifm", avec_blancs));
        // Un vrai changement passe.
        assert!(m.ajoute("ouifm", enrichissement(id, "A", "Autre titre")));
    }

    #[test]
    fn le_gagnant_est_le_plugin_le_plus_prioritaire_ayant_repondu() {
        // C'est ce que le cœur journalise : nommer le dernier à avoir répondu
        // mentirait dans le seul cas où l'on consulte ce journal — un affichage
        // douteux à attribuer.
        let mut m = Metadonnees::new(vec!["prioritaire".into(), "secondaire".into()]);
        let id = json!({"url": "un"});
        m.set_identity(Some(id.clone()));
        assert_eq!(m.gagnant(), None);
        m.ajoute("secondaire", enrichissement(id.clone(), "Second", "T"));
        assert_eq!(m.gagnant(), Some("secondaire"));
        m.ajoute("prioritaire", enrichissement(id.clone(), "Premier", "T"));
        assert_eq!(m.gagnant(), Some("prioritaire"));
        // Une nouvelle réponse du moins prioritaire ne change pas le gagnant.
        m.ajoute("secondaire", enrichissement(id, "Second bis", "T"));
        assert_eq!(m.gagnant(), Some("prioritaire"));
    }

    /// La position suit le **gagnant** de l'arbitrage, comme le reste du
    /// morceau : un plugin moins prioritaire retenu en réserve ne doit pas
    /// imposer sa propre horloge.
    #[test]
    fn la_position_est_celle_du_gagnant() {
        let mut m = Metadonnees::new(vec!["radiofrance".into(), "ouifm".into()]);
        m.set_identity(Some(json!({"url": "https://fip"})));
        m.ajoute(
            "ouifm",
            Enrichment {
                identity: json!({"url": "https://fip"}),
                title: Some("depuis ouifm".into()),
                position_s: Some(200),
                ..Default::default()
            },
        );
        assert_eq!(m.position_s(), Some(200));
        m.ajoute(
            "radiofrance",
            Enrichment {
                identity: json!({"url": "https://fip"}),
                title: Some("depuis radiofrance".into()),
                position_s: Some(12),
                ..Default::default()
            },
        );
        assert_eq!(m.position_s(), Some(12), "le plus prioritaire l'emporte");
    }

    #[test]
    fn sans_enrichissement_il_n_y_a_pas_de_position() {
        let m = Metadonnees::new(vec!["radiofrance".into()]);
        assert_eq!(m.position_s(), None);
    }

    #[test]
    fn la_chaine_brute_survit_a_lenrichissement_qui_lecrase() {
        // La propriété dont dépend toute la fonctionnalité. L'identité d'une
        // radio est l'URL du flux : elle ne change pas entre deux morceaux, et
        // `set_icy` n'efface délibérément pas les enrichissements. Donc sans
        // ce champ, un greffon qui a une fois écrit un artiste ne reverrait
        // plus jamais la chaîne ICY, et ne pourrait plus rien découper — « ça
        // marche une fois ».
        let mut m = Metadonnees::new(vec!["musicbrainz".to_string()]);
        let identite = serde_json::json!({ "kind": "stream", "url": "http://exemple/flux.mp3" });
        m.set_identity(Some(identite.clone()));
        assert!(m.set_icy("Miles Davis - So What".into()));

        // Le greffon corrige, en écrasant : le titre composé devient le sien.
        assert!(m.ajoute(
            "musicbrainz",
            ritornello_proto::Enrichment {
                identity: identite.clone(),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                ..Default::default()
            }
        ));
        assert_eq!(m.known().title.as_deref(), Some("So What"));

        // Morceau suivant, même station : l'enrichissement précédent est toujours
        // là (identité inchangée), mais la chaîne brute doit être la neuve.
        assert!(m.set_icy("John Coltrane - Naima".into()));
        assert_eq!(
            m.known().stream_title.as_deref(),
            Some("John Coltrane - Naima"),
            "le brut doit suivre le flux, pas la composition"
        );
    }

    #[test]
    fn sans_icy_le_champ_reste_vide() {
        let m = Metadonnees::new(vec![]);
        assert_eq!(m.known().stream_title, None);
    }

    #[test]
    fn letat_porte_la_source_et_la_duree() {
        let mut m = Metadonnees::new(vec!["ouifm".into()]);
        let id = json!({"url": "un"});
        m.set_identity(Some(id.clone()));
        m.ajoute(
            "ouifm",
            Enrichment {
                identity: id,
                artist: Some("Shaka Ponk".into()),
                title: Some("Wanna Get Free".into()),
                album: None,
                duration_s: Some(214),
                position_s: None,
                ..Default::default()
            },
        );
        let etat = m.etat();
        assert_eq!(etat.duration_s, Some(214));
    }
}
