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
    Cover(Cover),
}

/// Plafond des octets d'une pochette poussée dans ce protocole.
///
/// **Propre au transport, et indépendant de tout autre plafond.** Le cœur
/// applique déjà un plafond à un *téléchargement* (2 Mio, pour écarter le
/// `front` nu du Cover Art Archive), mais celui-là ne couvre que ce qui vient
/// du réseau : un `folder.jpg` d'un partage est traité comme de confiance et
/// servi en flux, sans borne de taille — la route HTTP n'a jamais à le
/// matérialiser. Pousser sur un socket **force** la matérialisation, donc
/// oblige à une borne d'ici, qui ne doit pas dépendre de la leur.
///
/// La valeur vient de la mesure : sérialiser une image de `n` octets en une
/// ligne de ce protocole coûte, au pic, environ `3,6 × n` résidents (les
/// octets, leur base64, la ligne rendue). À 2 Mio, cela fait ~7,4 Mio le
/// temps d'un changement de piste, ce qu'un appareil à 1 Gio partagé absorbe ;
/// à 150 Mio — le PNG de partage que la route HTTP du cœur cite comme cas
/// réel — cela ferait 540 Mio, soit la moitié de la machine.
///
/// Dépasser n'est pas une erreur d'allocation mais un refus : le producteur ne
/// matérialise jamais au-delà (il lit `COVER_MAX_BYTES + 1` octets et s'arrête),
/// aucune trame n'est émise, et l'afficheur n'a simplement pas d'image — la
/// même politique d'échec silencieux que la récupération elle-même.
pub const COVER_MAX_BYTES: usize = 2 * 1024 * 1024;

/// La pochette de ce qui joue, poussée aux seuls afficheurs qui l'ont demandée
/// (voir `Announcement::covers`).
///
/// Une trame **autonome** : une ligne porte une image entière, jamais une
/// tranche. C'est ce qui la rend compatible avec la politique de ligne
/// illisible du SDK — `warn` puis `continue`, la connexion survit : sauter une
/// ligne autonome ne perd qu'une image, sauter une tranche produirait une image
/// tronquée qu'aucun contrôle n'écarterait.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cover {
    /// Exactement le `cover_href` que la trame d'état publie pour la même
    /// image (`/api/cover/{clé}`).
    ///
    /// Sans lui, un afficheur devrait deviner à quel état la pochette qu'il
    /// vient de recevoir correspond : les trames arrivent bien dans l'ordre sur
    /// un socket unique, mais rien dans l'image ne dit *laquelle* elle est, et
    /// un greffon qui doit répondre « la pochette de cette piste-là » (le
    /// serveur MPD) n'a pas d'autre corrélation à sa disposition.
    pub href: String,
    /// Type MIME reconnu aux octets d'en-tête, jamais à l'extension ni à un
    /// `Content-Type` déclaré.
    pub mime: String,
    /// Les octets de l'image. En **base64** sur le fil : le protocole est du
    /// JSON par ligne, et un `Vec<u8>` que serde sérialise nu devient un
    /// tableau de nombres décimaux — mesuré à 3,57 fois la taille de l'image,
    /// contre 1,33 pour le base64, et 7,1 × n de pic résident contre 3,6.
    #[serde(with = "octets_base64")]
    pub bytes: Vec<u8>,
}

/// Les octets d'une pochette, en base64 sur le fil.
///
/// Le plafond est appliqué **à la lecture** et **avant le décodage** : c'est ce
/// qui empêche une ligne démesurée de faire allouer les octets qu'elle annonce.
/// Pas à l'écriture, et c'est délibéré — un refus de sérialisation
/// remonterait à l'appelant comme un échec d'envoi indistinguable d'un socket
/// mort, ce qui fait sortir de boucle le relais du cœur et prive l'afficheur de
/// *tout* pour le reste du processus. Le plafond est donc gardé là où il peut
/// être traité : au moment de matérialiser les octets, qui ne dépasse jamais.
mod octets_base64 {
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(o: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(o))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        use serde::de::Error as _;
        // `Cow` et non `String` : `serde_json::from_str` emprunte le texte de
        // la ligne quand il n'a pas d'échappement à défaire, ce qui évite une
        // copie de 1,33 × n avant même le décodage.
        let texte = std::borrow::Cow::<str>::deserialize(d)?;
        // Le plafond, contrôlé sur la **longueur du texte** : quatre caractères
        // de base64 valent trois octets, donc la taille décodée est connue
        // avant d'allouer quoi que ce soit. Les `=` de remplissage sont
        // retirés, sans quoi la borne serait effective jusqu'à deux octets trop
        // tôt — assez pour refuser une image de *exactement* `COVER_MAX_BYTES`,
        // que le producteur a le droit d'émettre.
        let remplissage = texte.bytes().rev().take_while(|b| *b == b'=').count();
        if (texte.len() / 4 * 3).saturating_sub(remplissage) > super::COVER_MAX_BYTES {
            return Err(D::Error::custom(format!(
                "cover refused: over {} bytes",
                super::COVER_MAX_BYTES
            )));
        }
        base64::engine::general_purpose::STANDARD
            .decode(texte.as_bytes())
            .map_err(D::Error::custom)
    }
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

    // Ce qu'une trame d'un genre inconnu devient est vérifié là où c'est
    // observable, dans le SDK : voir
    // `une_trame_illisible_ne_ferme_pas_la_connexion`. Le tester ici n'aurait
    // rien mordu — aucune configuration de `DisplayFrame` ne peut avaler un
    // genre inconnu, les champs de `PlayerState` étant obligatoires, et
    // l'assertion aurait passé même en retirant l'étiquetage (mesuré).

    // -- la trame de pochette ------------------------------------------------

    /// Des octets qui ne sont pas du texte : c'est justement ce qu'un JSON par
    /// ligne ne peut pas porter nu, et ce que l'encodage doit rendre au bit
    /// près. `0x0A` y est présent exprès — c'est le séparateur de ligne du
    /// protocole, et le voir survivre est la propriété qui compte.
    fn octets_hostiles() -> Vec<u8> {
        let mut v = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        v.extend_from_slice(b"\n\r\0\"\\{}");
        v.extend((0u16..=255).map(|b| b as u8));
        v
    }

    #[test]
    fn une_trame_de_pochette_rend_les_octets_au_bit_pres_et_sans_saut_de_ligne() {
        let octets = octets_hostiles();
        let trame = DisplayFrame::Cover(Cover {
            href: "/api/cover/1a2b3c4d".into(),
            mime: "image/jpeg".into(),
            bytes: octets.clone(),
        });
        let json = serde_json::to_string(&trame).unwrap();
        assert!(json.contains(r#""frame":"cover""#), "{json}");
        // Le protocole est délimité par des sauts de ligne : une trame qui en
        // contiendrait un couperait la ligne en deux, et les deux moitiés
        // seraient illisibles.
        assert!(!json.contains('\n'), "une trame ne doit contenir aucun saut de ligne");
        assert_eq!(serde_json::from_str::<DisplayFrame>(&json).unwrap(), trame);
    }

    #[test]
    fn les_octets_dune_pochette_voyagent_en_base64_pas_en_tableau_de_nombres() {
        // Ce que serde ferait d'un `Vec<u8>` nu — `[255,216,...]` — a été
        // mesuré à 3,57 fois la taille de l'image et 7,1 × n de pic résident,
        // contre 1,33 et 3,6 pour le base64. C'est la seule raison de
        // l'encodage, et c'est donc ce que ce test garde.
        let trame = DisplayFrame::Cover(Cover {
            href: "/api/cover/x".into(),
            mime: "image/png".into(),
            bytes: vec![0xFF, 0xD8, 0xFF],
        });
        let json = serde_json::to_string(&trame).unwrap();
        assert!(json.contains(r#""bytes":"/9j/""#), "{json}");
        assert!(!json.contains("255"), "les octets ne doivent pas voyager en nombres : {json}");
    }

    #[test]
    fn une_pochette_au_dela_du_plafond_est_refusee_a_la_lecture() {
        // Refusée **avant** le décodage : la longueur du texte base64 dit déjà
        // la taille décodée, donc rien n'est alloué pour une ligne démesurée.
        // Un refus, pas une panique d'allocation.
        let trop = "A".repeat((COVER_MAX_BYTES + 3) / 3 * 4 + 4);
        let ligne = format!(
            r#"{{"frame":"cover","data":{{"href":"/api/cover/x","mime":"image/jpeg","bytes":"{trop}"}}}}"#
        );
        let e = serde_json::from_str::<DisplayFrame>(&ligne).unwrap_err();
        assert!(e.to_string().contains("over"), "message inattendu : {e}");
    }

    #[test]
    fn une_pochette_juste_sous_le_plafond_passe() {
        // Le pendant du refus : la borne doit être *au* plafond, pas en
        // dessous. Sans ce test, un plafond divisé par deux par erreur
        // passerait inaperçu.
        let octets = vec![0x41u8; COVER_MAX_BYTES];
        let trame = DisplayFrame::Cover(Cover {
            href: "/api/cover/x".into(),
            mime: "image/jpeg".into(),
            bytes: octets.clone(),
        });
        let json = serde_json::to_string(&trame).unwrap();
        match serde_json::from_str::<DisplayFrame>(&json).unwrap() {
            DisplayFrame::Cover(c) => assert_eq!(c.bytes.len(), octets.len()),
            autre => panic!("une trame de pochette etait attendue : {autre:?}"),
        }
    }

    #[test]
    fn un_base64_invalide_est_une_erreur_pas_des_octets_arbitraires() {
        let ligne = r#"{"frame":"cover","data":{"href":"/api/cover/x","mime":"image/jpeg","bytes":"!!!!"}}"#;
        assert!(
            serde_json::from_str::<DisplayFrame>(ligne).is_err(),
            "un encodage invalide doit etre une erreur : le SDK la traite en ligne illisible"
        );
    }
}
