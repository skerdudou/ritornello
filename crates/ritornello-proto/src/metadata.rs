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
