//! Résolution des métadonnées du morceau en cours, et composition de
//! l'affichage qui en découle.
//!
//! Deux couches se superposent : ce que le flux annonce lui-même (l'en-tête ICY
//! lu par mpv, affiché **brut**) et ce qu'un plugin `metadata` a appris. La
//! seconde gagne sur la première quand elle correspond à ce qui joue.
//!
//! Tout est ici en fonctions et méthodes pures — aucune socket, aucun routeur,
//! aucune horloge : l'arbitrage entre plugins et les replis d'affichage sont
//! précisément la partie où une erreur ne se voit pas à l'œil sur l'appareil.

use ritornello_proto::{Enrichment, View};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// Origine retenue pour l'affichage quand elle vient du flux lui-même.
pub const ORIGINE_ICY: &str = "icy";

/// Ce qui est affichable du morceau en cours.
///
/// `origin` dit **qui** a fourni l'information (`"icy"` ou le nom du plugin
/// gagnant) : sans elle, un affichage douteux ne serait attribuable à personne,
/// et c'est exactement la question qu'on se pose devant un titre faux.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Morceau {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
    pub origin: Option<String>,
}

/// État du lecteur diffusé à la SPA : ce qui est volatil, et qui a donc besoin
/// d'être **poussé**.
///
/// Un seul état et un seul canal pour tout ce qui bouge — source active, volume,
/// muet, veille, et le morceau quand on le connaît. La route `/api/status`, elle,
/// porte le contrat de navigation (quels plugins existent, lesquels ont une page
/// d'admin) : structurellement stable, lue une fois au montage. Y mêler du
/// volatil obligerait la SPA à la resonder en boucle pour afficher un volume.
///
/// Le morceau est **aplati** dans le JSON (`serde(flatten)`) : l'IHM reçoit un
/// objet plat, sans avoir à distinguer deux niveaux pour un même encart.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PlayerState {
    /// Nom de la Source active, pour que la SPA sache de quoi elle parle.
    pub source: String,
    pub volume: u8,
    pub muted: bool,
    pub standby: bool,
    /// Touche 1-9 correspondant à ce qui joue, telle que la Source active l'a
    /// déclarée (présélection radio, piste cd) : c'est ce que la télécommande
    /// de l'IHM met en évidence. `None` = rien ne joue, ou la Source n'a rien
    /// déclaré.
    pub preset: Option<u8>,
    #[serde(flatten)]
    pub morceau: Morceau,
}

impl Morceau {
    /// Vrai si rien n'est connu du morceau.
    ///
    /// Réservé aux tests : côté IHM, c'est la SPA qui décide quoi montrer d'un
    /// état partiel, et le cœur n'a aucune raison de trancher pour elle.
    #[cfg(test)]
    pub fn est_vide(&self) -> bool {
        self.artist.is_none() && self.title.is_none() && self.album.is_none()
    }
}

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
    /// Enrichissements correspondant à `identity`, par plugin.
    enrichissements: HashMap<String, Enrichment>,
}

impl Metadonnees {
    pub fn new(ordre: Vec<String>) -> Self {
        Self { ordre, ..Default::default() }
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
        self.enrichissements.clear();
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
    pub fn ajoute(&mut self, plugin: &str, e: Enrichment) -> bool {
        // Normalisation ici plutôt qu'au seul site d'appel : `is_empty` n'a de
        // sens qu'après elle, et cette méthode est publique. Idempotent, et
        // l'invariant devient local au lieu de reposer sur la discipline de
        // l'appelant.
        let e = e.cleaned();
        let Some(courante) = &self.identity else {
            tracing::debug!("enrichissement de {plugin} ignore: plus rien ne joue");
            return false;
        };
        if &e.identity != courante {
            tracing::debug!("enrichissement de {plugin} perime, ignore");
            return false;
        }
        if e.is_empty() {
            tracing::debug!("enrichissement vide de {plugin}, compte comme non-reponse");
            return false;
        }
        if !self.ordre.iter().any(|n| n == plugin) {
            tracing::warn!("enrichissement d'un plugin metadata non declare: {plugin}");
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
        self.enrichissements.insert(plugin.to_string(), e);
        true
    }

    /// Nom du plugin dont l'enrichissement est retenu, s'il y en a un.
    ///
    /// C'est **le gagnant**, pas le dernier à avoir répondu : toute la règle
    /// d'ordre est justifiée par la prévisibilité pour qui débogue, et c'est le
    /// seul instrument de ce débogage.
    pub fn gagnant(&self) -> Option<&str> {
        self.ordre.iter().find(|p| self.enrichissements.contains_key(*p)).map(String::as_str)
    }

    /// Résolution, dans l'ordre : l'enrichissement du plugin le plus
    /// prioritaire ayant répondu, sinon l'ICY brut, sinon rien.
    ///
    /// L'ICY est repris **tel quel** dans `title`, sans découpage sur `" - "` :
    /// la convention existe mais n'est pas garantie, et un enrichissement de
    /// plugin fournit de toute façon des champs déjà séparés. Une station qui
    /// n'annonce que son propre nom ou ses jingles verra donc cela s'afficher —
    /// c'est ce qu'elle émet.
    pub fn etat(&self) -> Morceau {
        for plugin in &self.ordre {
            if let Some(e) = self.enrichissements.get(plugin) {
                return Morceau {
                    artist: e.artist.clone(),
                    title: e.title.clone(),
                    album: e.album.clone(),
                    duration_s: e.duration_s,
                    origin: Some(plugin.clone()),
                };
            }
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
}

/// Ligne de métadonnées à afficher, ou `None` si rien n'est connu.
///
/// Le tiret cadratin est déjà la convention de l'affichage du cd. Une
/// information partielle vaut mieux que rien : l'artiste seul reste affiché,
/// parce qu'il dit déjà quelque chose de ce qu'on écoute.
pub fn ligne_titre(artist: Option<&str>, title: Option<&str>) -> Option<String> {
    match (artist, title) {
        (Some(a), Some(t)) => Some(format!("{a} — {t}")),
        (None, Some(t)) => Some(t.to_string()),
        (Some(a), None) => Some(a.to_string()),
        (None, None) => None,
    }
}

/// Compose la vue affichée : les lignes écrites par la Source, complétées par
/// ce qu'on sait du morceau.
///
/// Deux règles, et une invariante qui les gouverne : **le cœur ne détruit jamais
/// une information que la Source seule possède.**
///
/// - `line3` est la ligne des métadonnées. Elle est libre sur la radio, et sur
///   le cd elle portait le titre de piste — qui revient désormais précisément
///   sous forme d'enrichissement. Rien n'entre donc en conflit.
/// - `line2` reçoit l'album **seulement si la Source a déclaré sa propre
///   `line2` remplaçable** (`line2_replaceable`), c'est-à-dire l'a écrite faute
///   de mieux. Le remplacement est réversible : l'album disparaît-il, la ligne
///   de la Source revient, puisque c'est elle qui est conservée dans `base`.
///
/// Le critère est une déclaration **explicite** et non le fait que la ligne soit
/// vide : avec le vide pour signal, une Source demanderait l'album en se taisant,
/// et celle qui veut une ligne vide n'aurait aucun moyen de le dire.
pub fn composer(base: &View, etat: &Morceau, line2_replaceable: bool) -> View {
    let mut vue = base.clone();
    if let Some(ligne) = ligne_titre(etat.artist.as_deref(), etat.title.as_deref()) {
        vue.line3 = ligne;
    }
    if line2_replaceable {
        if let Some(album) = &etat.album {
            vue.line2 = album.clone();
        }
    }
    vue
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

    #[test]
    fn les_quatre_replis_de_la_ligne_de_titre() {
        assert_eq!(ligne_titre(Some("Miles Davis"), Some("So What")).as_deref(), Some("Miles Davis — So What"));
        assert_eq!(ligne_titre(None, Some("So What")).as_deref(), Some("So What"));
        // Décision du propriétaire : on affiche toute information disponible,
        // même partielle.
        assert_eq!(ligne_titre(Some("Miles Davis"), None).as_deref(), Some("Miles Davis"));
        assert_eq!(ligne_titre(None, None), None);
    }

    /// État complet, tel qu'un plugin `metadata` le fournit pour un disque.
    fn etat_complet() -> Morceau {
        Morceau {
            artist: Some("Miles Davis".into()),
            title: Some("So What".into()),
            album: Some("Kind of Blue".into()),
            ..Default::default()
        }
    }

    #[test]
    fn composer_laisse_la_ligne3_de_la_source_quand_rien_nest_connu() {
        let base = View { line1: "CD 3/12".into(), line2: "audio CD".into(), line3: "deja la".into() };
        let vue = composer(&base, &Morceau::default(), true);
        assert_eq!(vue.line3, "deja la", "le coeur ne vide jamais une ligne ecrite par la Source");
        // Ligne declaree remplacable, mais aucun album connu : l'etiquette de la
        // Source reste. C'est ce qui evite un afficheur a moitie vide sur un
        // disque absent de MusicBrainz ou un appareil hors ligne.
        assert_eq!(vue.line2, "audio CD");
    }

    #[test]
    fn composer_remplit_la_ligne3_sans_toucher_a_une_ligne2_non_declaree() {
        let base = View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: String::new() };
        let vue = composer(&base, &etat_complet(), false);
        assert_eq!(vue.line3, "Miles Davis — So What");
        assert_eq!(vue.line2, "FIP", "le nom de station ne doit jamais etre remplace par un album");
        assert_eq!(vue.line1, "RADIO  P1");
    }

    #[test]
    fn une_ligne2_vide_mais_non_declaree_reste_vide() {
        // C'est la correction du critere : avec le vide pour signal, une Source
        // sobre (une entree auxiliaire n'affichant que son nom) se verrait
        // imposer un album sans l'avoir demande, et devrait ecrire une chaine
        // factice pour s'en proteger.
        let base = View { line1: "AUX".into(), line2: String::new(), line3: String::new() };
        let vue = composer(&base, &etat_complet(), false);
        assert_eq!(vue.line2, "", "sans declaration, le coeur n'ecrit pas ici");
    }

    #[test]
    fn lalbum_remplace_une_ligne2_declaree_remplacable() {
        let base = View { line1: "CD 3/12".into(), line2: "audio CD".into(), line3: String::new() };
        let vue = composer(&base, &etat_complet(), true);
        assert_eq!(vue.line2, "Kind of Blue");
        assert_eq!(vue.line3, "Miles Davis — So What");
    }

    #[test]
    fn le_remplacement_est_reversible() {
        // La ligne de la Source est conservee dans `base`, donc l'album disparu
        // (changement de disque, plugin qui se tait), l'etiquette revient d'elle-
        // meme : le coeur n'a rien detruit.
        let base = View { line1: "CD 3/12".into(), line2: "audio CD".into(), line3: String::new() };
        assert_eq!(composer(&base, &etat_complet(), true).line2, "Kind of Blue");
        assert_eq!(composer(&base, &Morceau::default(), true).line2, "audio CD");
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
            },
        );
        let etat = m.etat();
        assert_eq!(etat.duration_s, Some(214));
    }
}
