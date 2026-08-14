//! Protocole du genre `metadata` : le cœur annonce ce qui joue, un plugin
//! renvoie ce qu'il en sait.
//!
//! Contrairement à `source` (requête/réponse corrélée par `id`) et à `display`
//! ou `input` (sens unique), ce protocole est **bidirectionnel non corrélé** :
//! chaque côté émet quand il a quelque chose à dire. Le cœur ne demande rien,
//! parce qu'il n'a aucun moyen de savoir si un plugin saura répondre ni au bout
//! de combien de temps ; le plugin n'attend pas de réponse, parce qu'un
//! enrichissement n'est ni accepté ni refusé, il est simplement retenu ou
//! périmé.
//!
//! Le garde-fou contre la péremption est l'**écho de l'identité** : un
//! enrichissement porte l'identité auquel il se rapporte, et le cœur jette
//! celui qui ne correspond plus à ce qui joue. Sans cet écho, la réponse lente
//! d'un plugin sur le morceau précédent viendrait écraser le morceau courant.

use serde::{Deserialize, Serialize};

/// Ce que la Source dit de ce qu'elle joue, transporté à côté de la vue.
///
/// Trois états sont nécessaires, et c'est pourquoi ce type est un enum plutôt
/// qu'un `Option` : l'absence du champ dans une trame (« cette réponse ne dit
/// rien de l'identité ») ne doit pas être confondue avec `Nothing` (« plus rien
/// ne joue »), et serde ramène `null` et l'absence à la même valeur pour un
/// `Option`. Les trois cas sont donc : champ absent, `Playing`, `Nothing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// `value` et non `identity` : imbriqué dans `SourceMessage.identity`, ce dernier
// donnerait `"identity":{"state":"Playing","identity":{…}}` — le protocole se
// veut lisible à l'œil dans un `journalctl`.
#[serde(tag = "state", content = "value")]
pub enum IdentityUpdate {
    /// Identité **opaque**, produite par la Source, jamais interprétée par le
    /// cœur — même principe que le JSON opaque du protocole `admin`. Le cœur
    /// se contente de comparer deux identités par égalité.
    Playing(serde_json::Value),
    /// Plus rien ne joue : le cœur oublie l'identité courante et prévient les
    /// plugins `metadata` pour qu'ils cessent leur travail.
    Nothing,
}

/// Cœur → plugin. Émis à chaque changement de ce qui joue, et à l'arrêt
/// (`identity: None`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NowPlaying {
    /// Nom de la Source active (`"radio"`, `"cd"`…), pour qu'un plugin puisse
    /// se taire d'emblée sur une source qu'il ne traite pas, sans avoir à
    /// inspecter la forme de l'identité.
    pub source: String,
    /// `None` = plus rien ne joue.
    #[serde(default)]
    pub identity: Option<serde_json::Value>,
}

/// Plugin → cœur. Émis quand le plugin apprend quelque chose.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Enrichment {
    /// **Écho** de l'identité concernée : le garde-fou de péremption.
    pub identity: serde_json::Value,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub duration_s: Option<u32>,
}

impl Enrichment {
    /// Ramène à `None` toute chaîne vide ou blanche.
    ///
    /// Un plugin qui ne connaît pas l'artiste peut aussi bien envoyer `null`
    /// qu'une chaîne vide — les deux disent la même chose. Normaliser ici évite
    /// que le reste du cœur ait à traiter deux cas, et surtout évite qu'un
    /// `title: ""` compte comme une réponse et bloque un plugin moins
    /// prioritaire qui, lui, connaît le titre (voir `is_empty`).
    pub fn cleaned(mut self) -> Self {
        fn vide(champ: &mut Option<String>) {
            if champ.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *champ = None;
            } else if let Some(s) = champ {
                *s = s.trim().to_string();
            }
        }
        vide(&mut self.artist);
        vide(&mut self.title);
        vide(&mut self.album);
        self
    }

    /// Vrai si l'enrichissement n'apporte aucune information.
    ///
    /// À appeler **après** `cleaned`. Un tel enrichissement compte comme une
    /// non-réponse dans l'arbitrage : un plugin qui reconnaît l'identité mais
    /// n'a encore rien appris ne doit pas bloquer un plugin moins prioritaire.
    pub fn is_empty(&self) -> bool {
        self.artist.is_none() && self.title.is_none() && self.album.is_none()
    }
}

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
    /// Touche numérotée correspondant à ce qui joue, telle que la Source active l'a
    /// déclarée (présélection radio, piste cd) : c'est ce que la télécommande
    /// de l'IHM met en évidence. `None` = rien ne joue, ou la Source n'a rien
    /// déclaré.
    pub preset: Option<u8>,
    /// Nombre de présélections numérotées offertes par la Source active
    /// (stations pour la radio, pistes pour le cd), tel qu'elle l'a déclaré.
    /// `None` = rien déclaré : l'IHM retombe sur la grille 1-9 historique.
    /// `Some(0)` = rien à numéroter (cd sans disque) : aucune touche.
    pub preset_count: Option<u8>,
    /// Nom lisible de la présélection donnée par `preset`, tel que la Source
    /// active l'a déclaré (le nom configuré de la station pour la radio).
    /// `None` : la Source ne nomme rien à cet emplacement (le cd, dont
    /// « audio CD » n'a rien à voir avec une présélection nommée), ou rien ne
    /// joue. Vit et meurt avec `preset` — voir `Core::set_identity`.
    pub preset_name: Option<String>,
    #[serde(flatten)]
    pub morceau: Morceau,
}

impl Morceau {
    /// Vrai si rien n'est connu du morceau.
    ///
    /// Réservé aux tests : côté IHM, c'est la SPA qui décide quoi montrer d'un
    /// état partiel, et le cœur n'a aucune raison de trancher pour elle.
    pub fn est_vide(&self) -> bool {
        self.artist.is_none() && self.title.is_none() && self.album.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn now_playing_roundtrip_avec_identite() {
        let np = NowPlaying {
            source: "radio".into(),
            identity: Some(json!({"kind": "stream", "url": "https://ouifm/ouifm-high.mp3"})),
        };
        let back: NowPlaying = serde_json::from_str(&serde_json::to_string(&np).unwrap()).unwrap();
        assert_eq!(back, np);
    }

    #[test]
    fn now_playing_roundtrip_sans_identite() {
        let np = NowPlaying { source: "cd".into(), identity: None };
        let json = serde_json::to_string(&np).unwrap();
        let back: NowPlaying = serde_json::from_str(&json).unwrap();
        assert_eq!(back.identity, None);
    }

    #[test]
    fn enrichment_roundtrip() {
        let e = Enrichment {
            identity: json!({"kind": "disc", "track": 3}),
            artist: Some("Miles Davis".into()),
            title: Some("So What".into()),
            album: Some("Kind of Blue".into()),
            duration_s: Some(545),
        };
        let back: Enrichment = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn enrichment_accepte_les_champs_absents() {
        // Un plugin minimal n'envoie que ce qu'il connaît : les champs manquants
        // ne doivent pas faire échouer la lecture de la trame.
        let e: Enrichment = serde_json::from_str(r#"{"identity":{"k":1},"title":"Bikwix"}"#).unwrap();
        assert_eq!(e.title.as_deref(), Some("Bikwix"));
        assert_eq!(e.artist, None);
        assert_eq!(e.duration_s, None);
    }

    #[test]
    fn identity_update_distingue_les_trois_etats() {
        // Playing et Nothing doivent se distinguer sur le fil, et l'absence du
        // champ (testée dans source.rs) est un troisième cas.
        let joue = IdentityUpdate::Playing(json!({"kind": "stream"}));
        assert_eq!(
            serde_json::to_string(&joue).unwrap(),
            r#"{"state":"Playing","value":{"kind":"stream"}}"#
        );
        assert_eq!(serde_json::to_string(&IdentityUpdate::Nothing).unwrap(), r#"{"state":"Nothing"}"#);
        let back: IdentityUpdate = serde_json::from_str(r#"{"state":"Nothing"}"#).unwrap();
        assert_eq!(back, IdentityUpdate::Nothing);
    }

    #[test]
    fn cleaned_ramene_le_blanc_a_none_et_elague() {
        let e = Enrichment {
            identity: json!(1),
            artist: Some("   ".into()),
            title: Some("  So What  ".into()),
            album: Some(String::new()),
            duration_s: None,
        }
        .cleaned();
        assert_eq!(e.artist, None);
        assert_eq!(e.title.as_deref(), Some("So What"));
        assert_eq!(e.album, None);
    }

    #[test]
    fn is_empty_ne_compte_que_les_champs_de_texte() {
        assert!(Enrichment { identity: json!(1), ..Default::default() }.is_empty());
        // Une durée seule ne fait pas un enrichissement affichable : elle ne
        // suffit pas à gagner l'arbitrage contre un plugin qui connaît le titre.
        let duree_seule = Enrichment { identity: json!(1), duration_s: Some(210), ..Default::default() };
        assert!(duree_seule.is_empty());
        let artiste_seul =
            Enrichment { identity: json!(1), artist: Some("FIP".into()), ..Default::default() };
        assert!(!artiste_seul.is_empty());
    }
}
