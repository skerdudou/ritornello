use crate::metadata::PlayerState;
use crate::source::Preset;
use serde::{Deserialize, Serialize};

/// Une ligne du protocole `display`.
///
/// Étiquetage **adjacent** et non interne : `PlayerState` contient un
/// `serde(flatten)` (`Morceau`), et le croisement flatten × internally-tagged
/// est un angle mort connu de serde. Ici le `data` d'une trame d'état est
/// exactement le JSON qui voyageait avant l'enveloppe.
///
/// **Cet enum est fait pour grandir** : chaque nouvelle variante est un message
/// qu'un afficheur peut ignorer jusqu'à ce qu'il s'y intéresse (voir le corps
/// par défaut de `DisplayPlugin::catalogue` dans le SDK). Rien ne doit donc
/// supposer le nombre de variantes — ni un `match` exhaustif hors du SDK, ni un
/// comptage dans un test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", content = "data", rename_all = "lowercase")]
// `PlayerState` pèse une bonne centaine d'octets de plus qu'un catalogue, et
// clippy voudrait le mettre dans un `Box`. Refusé : cette enveloppe est
// construite, sérialisée puis jetée dans la même expression — les octets
// vivent sur la pile le temps d'un `to_string`. La boîte échangerait cela
// contre une allocation par trame et par afficheur, plusieurs fois par seconde
// de lecture, ce qui est exactement le sens contraire sur un Pi 2 B.
#[allow(clippy::large_enum_variant)]
pub enum DisplayFrame {
    State(PlayerState),
    Catalogue(Catalogue),
}

/// Ce qui est structurel et rarement changeant : les sources déclarées, dans
/// l'ordre de bascule de `SourceCycle`, et les présélections nommées de chacune
/// quand elle sait les énumérer.
///
/// Volontairement **hors** de `PlayerState` : celui-ci est un instantané,
/// déduplique par égalité et se reconstruit à chaque publication ; un catalogue
/// y ferait voyager cinquante noms de station sur chaque trame de lecture.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalogue {
    pub sources: Vec<SourceCatalogue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCatalogue {
    pub name: String,
    /// Vide = cette source ne sait pas énumérer. Le consommateur retombe sur
    /// `preset_count`, qui reste la vérité du nombre.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenveloppe_dune_trame_detat_porte_le_json_qui_voyageait_avant() {
        // L'étiquetage adjacent garantit que le `data` est exactement l'ancienne
        // charge utile : c'est ce qui rend la migration vérifiable.
        let etat = PlayerState { source: "radio".into(), volume: 40, ..Default::default() };
        let nu = serde_json::to_value(&etat).unwrap();
        let enveloppe = serde_json::to_value(DisplayFrame::State(etat.clone())).unwrap();
        assert_eq!(enveloppe["frame"], "state");
        assert_eq!(enveloppe["data"], nu);
    }

    #[test]
    fn une_trame_de_catalogue_fait_le_tour() {
        let trame = DisplayFrame::Catalogue(Catalogue {
            sources: vec![SourceCatalogue {
                name: "radio".into(),
                presets: vec![Preset { index: 1, name: "FIP".into() }],
            }],
        });
        let json = serde_json::to_string(&trame).unwrap();
        assert!(json.contains(r#""frame":"catalogue""#), "{json}");
        assert_eq!(serde_json::from_str::<DisplayFrame>(&json).unwrap(), trame);
    }

    #[test]
    fn une_ligne_detat_du_fil_se_relit_en_trame_detat() {
        // Le sens **lecture**, depuis les octets écrits à la main plutôt que
        // depuis un aller-retour : un aller-retour reste vrai si l'étiquetage
        // change des deux côtés à la fois, ce qui est exactement le cas où un
        // afficheur d'une version et un cœur d'une autre ne se comprennent plus.
        //
        // Séparé du test de catalogue exprès : une boucle sur un tableau de
        // trames aurait à être retouchée à chaque variante ajoutée, et
        // `DisplayFrame` est fait pour grandir.
        let ligne = r#"{"frame":"state","data":{"source":"cd","volume":30,"muted":false,"standby":false,"preset":3}}"#;
        match serde_json::from_str::<DisplayFrame>(ligne).unwrap() {
            DisplayFrame::State(e) => {
                assert_eq!(e.source, "cd");
                assert_eq!(e.preset, Some(3));
                assert_eq!(e.volume, 30);
            }
            autre => panic!("une trame d'etat etait attendue, obtenu {autre:?}"),
        }
    }

    #[test]
    fn une_source_sans_preselections_nommees_ne_serialise_aucune_liste() {
        let c = SourceCatalogue { name: "cd".into(), presets: Vec::new() };
        assert!(!serde_json::to_string(&c).unwrap().contains("presets"));
    }

    #[test]
    fn une_source_sans_liste_se_relit_sans_erreur() {
        // Le pendant du `skip_serializing_if` : ce que le sérialiseur omet, le
        // désérialiseur doit l'accepter, sans quoi une trame émise par le cœur
        // serait illisible par l'afficheur.
        let c: SourceCatalogue = serde_json::from_str(r#"{"name":"cd"}"#).unwrap();
        assert_eq!(c.presets, Vec::new());
    }

    // Ce qu'une trame d'un genre inconnu (la `cover` d'un chantier à venir)
    // devient est vérifié là où c'est observable, dans le SDK : voir
    // `une_trame_illisible_ne_ferme_pas_la_connexion`. Le tester ici n'aurait
    // rien mordu — aucune configuration de `DisplayFrame` ne peut avaler un
    // genre inconnu, les champs de `PlayerState` étant obligatoires, et
    // l'assertion aurait passé même en retirant l'étiquetage (mesuré).
}
