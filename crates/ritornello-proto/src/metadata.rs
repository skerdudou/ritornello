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
    /// Écoulé dans le morceau **au moment de l'émission**, en secondes.
    ///
    /// Un écoulé relatif plutôt qu'un horodatage absolu : rien à synchroniser
    /// entre deux horloges, et c'est la convention de `duration_s` juste
    /// au-dessus. Le cœur l'ancre à la réception et l'avance lui-même ensuite
    /// (voir `Core::rafraichit_position`).
    #[serde(default)]
    pub position_s: Option<u32>,
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Morceau {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
    pub origin: Option<String>,
}

/// A transient overlay the appliance is showing right now, carrying **both**
/// the raw value and the resolved words: a display can draw a volume gauge
/// from `level`, or simply print `text`, without needing a catalogue of its
/// own.
///
/// `remaining_ms` is informative. The core alone owns the deadline — it
/// publishes a frame when the overlay expires — so a display may animate a
/// countdown but never decides when the overlay ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Overlay {
    /// Volume/mute overlay.
    Volume { level: u8, muted: bool, text: String, remaining_ms: u32 },
    /// Pending tens offset being composed on the remote (`+10`, `+20`).
    Tens { offset: u8, text: String, remaining_ms: u32 },
    /// Ephemeral message from a source ("empty preset").
    Message { text: String, remaining_ms: u32 },
}

/// Égalité **volontairement écrite à la main** : elle ignore `remaining_ms`.
///
/// Deux incrustations qui ne diffèrent que par le temps restant décrivent le
/// même écran, et `Core::publie_etat` déduplique les trames par égalité. Une
/// dérive automatique ferait passer chaque rafraîchissement redondant pour un
/// changement — plusieurs chemins du cœur rafraîchissent pour un même
/// événement — et chaque afficheur réimprimerait la même chose.
///
/// Écrite ici, sur `Overlay`, et non sur `PlayerState` : au niveau de la
/// charge utile il faudrait comparer à la main tous les autres champs pour ne
/// traiter spécialement qu'un champ imbriqué dans un enum sous une `Option`,
/// et chaque champ ajouté plus tard serait un oubli en puissance.
impl PartialEq for Overlay {
    fn eq(&self, autre: &Self) -> bool {
        match (self, autre) {
            (
                Self::Volume { level: a, muted: ma, text: ta, .. },
                Self::Volume { level: b, muted: mb, text: tb, .. },
            ) => a == b && ma == mb && ta == tb,
            (Self::Tens { offset: a, text: ta, .. }, Self::Tens { offset: b, text: tb, .. }) => {
                a == b && ta == tb
            }
            (Self::Message { text: ta, .. }, Self::Message { text: tb, .. }) => ta == tb,
            _ => false,
        }
    }
}

impl Overlay {
    /// Replaces the remaining time, computed at publication from the deadline
    /// the core holds. The `remaining_ms` stored in `self` is therefore never
    /// read — and since equality ignores it, refreshing it does not defeat the
    /// frame deduplication.
    #[must_use]
    pub fn avec_restant(self, restant_ms: u32) -> Self {
        match self {
            Self::Volume { level, muted, text, .. } => {
                Self::Volume { level, muted, text, remaining_ms: restant_ms }
            }
            Self::Tens { offset, text, .. } => Self::Tens { offset, text, remaining_ms: restant_ms },
            Self::Message { text, .. } => Self::Message { text, remaining_ms: restant_ms },
        }
    }
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// The appliance's current state as a **resolved sentence**: the status a
    /// source declared ("NO DISC", "AUDIO CD") or the core's standby word.
    /// One slot, because there is never more than one status at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The transient overlay showing right now, if any. Displays render it as
    /// they see fit; the SPA ignores it (it shows the volume in plain sight
    /// and has its own toasts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
    /// Où en est ce qui joue, en secondes, **à l'instant de la publication**.
    ///
    /// `None` = personne n'a de quoi répondre : rien ne joue, ou c'est un flux
    /// que nul plugin `metadata` ne suit. Deux fournisseurs alimentent ce
    /// champ sans jamais se disputer — mpv pour un contenu fini, un plugin
    /// `metadata` pour un flux — parce que le contexte décide lequel des deux
    /// a le droit de parler (voir `Core::rafraichit_position`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_s: Option<u32>,
    /// Ce qui joue accepte un déplacement : c'est le `finite` que la Source a
    /// déclaré à son `Play`, rendu visible aux consommateurs.
    ///
    /// Un champ à part entière plutôt qu'une déduction de `duration_s` : les
    /// deux notions divergent exactement là où ça compte — Radio France
    /// annonce la durée d'un morceau sur un direct qu'on ne peut pas
    /// rembobiner, un fichier sans étiquette de durée reste parcourable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seekable: bool,
    /// La Source active a de quoi éjecter (voir `SourceMessage::can_eject`) :
    /// c'est ce qui permet à la télécommande web de griser sa touche Eject
    /// plutôt que d'émettre une commande que la Source jettera en silence.
    ///
    /// **Faux par défaut** : ne pas savoir, c'est n'offrir rien — la même
    /// convention que les capacités d'extinction de `system.rs`. Un booléen et
    /// non un `Option` : côté consommateur, « la Source n'a rien déclaré » et
    /// « la Source ne peut pas éjecter » appellent le même bouton grisé, et un
    /// troisième état n'aurait aucun rendu propre.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_eject: bool,
    #[serde(flatten)]
    pub morceau: Morceau,
}

impl Morceau {
    /// Vrai si rien n'est connu du morceau.
    ///
    /// N'a d'appelant que dans les tests, et c'est voulu : côté IHM, c'est la
    /// SPA qui décide quoi montrer d'un état partiel, et le cœur n'a aucune
    /// raison de trancher pour elle.
    ///
    /// Cette convention n'est plus tenue par le compilateur. Elle l'était par un
    /// `#[cfg(test)]`, du temps où la structure vivait dans le cœur avec ses
    /// tests ; un tel attribut ne survit pas au passage dans un crate séparé,
    /// où il ne s'applique qu'à la compilation de ce crate-là et ferait
    /// disparaître la méthode pour tous les autres.
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
            position_s: None,
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
            position_s: None,
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

    #[test]
    fn overlay_volume_fait_un_aller_retour_json() {
        let o = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let json = serde_json::to_string(&o).unwrap();
        // Étiquetage interne : un objet plat, plus simple à lire côté web qu'un
        // couple {"kind":…,"data":{…}}.
        assert!(json.contains("\"kind\":\"volume\""));
        assert!(json.contains("\"level\":65"));
        let back: Overlay = serde_json::from_str(&json).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn overlay_cumul_et_message_font_un_aller_retour_json() {
        let t = Overlay::Tens { offset: 20, text: "PRESELECTION +20".into(), remaining_ms: 3000 };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"tens\""));
        assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), t);

        let m = Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 5000 };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"kind\":\"message\""));
        assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), m);
    }

    #[test]
    fn deux_incrustations_ne_differant_que_par_le_temps_restant_sont_egales() {
        // La garantie qui protège la déduplication de `publie_etat` : deux trames
        // qui ne diffèrent que par le temps restant décrivent le même écran. Sans
        // cette égalité, chaque rafraîchissement redondant serait poussé, et
        // chaque afficheur réimprimerait la même chose.
        let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let b = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 120 };
        assert_eq!(a, b);
    }

    #[test]
    fn une_incrustation_qui_differe_ailleurs_reste_differente() {
        // Garde-fou de l'égalité ci-dessus : elle ignore le temps restant, et rien
        // d'autre.
        let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let b = Overlay::Volume { level: 66, muted: false, text: "VOLUME 66 %".into(), remaining_ms: 4200 };
        assert_ne!(a, b);
        let c = Overlay::Message { text: "X".into(), remaining_ms: 1 };
        let d = Overlay::Message { text: "Y".into(), remaining_ms: 1 };
        assert_ne!(c, d);
    }

    #[test]
    fn avec_restant_ne_touche_qu_au_temps_restant_des_trois_variantes() {
        // La méthode n'aura son premier appelant qu'au moment où le cœur
        // publiera un temps restant frais. Sans ce test, une permutation de
        // champs entre variantes — reconstruire un `offset` depuis un `level` —
        // compilerait et ne se verrait qu'à l'intégration. L'égalité d'`Overlay`
        // ignorant `remaining_ms`, elle ne peut pas servir ici : on déstructure.
        let v = Overlay::Volume { level: 65, muted: true, text: "VOLUME MUET".into(), remaining_ms: 4000 };
        match v.avec_restant(7) {
            Overlay::Volume { level, muted, text, remaining_ms } => {
                assert_eq!((level, muted, text.as_str(), remaining_ms), (65, true, "VOLUME MUET", 7));
            }
            autre => panic!("la variante doit être préservée, obtenu {autre:?}"),
        }
        let t = Overlay::Tens { offset: 20, text: "+20".into(), remaining_ms: 4000 };
        match t.avec_restant(8) {
            Overlay::Tens { offset, text, remaining_ms } => {
                assert_eq!((offset, text.as_str(), remaining_ms), (20, "+20", 8));
            }
            autre => panic!("la variante doit être préservée, obtenu {autre:?}"),
        }
        let m = Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 4000 };
        match m.avec_restant(9) {
            Overlay::Message { text, remaining_ms } => {
                assert_eq!((text.as_str(), remaining_ms), ("PRESELECTION VIDE", 9));
            }
            autre => panic!("la variante doit être préservée, obtenu {autre:?}"),
        }
    }

    #[test]
    fn les_deux_champs_neufs_sont_absents_du_json_quand_ils_sont_vides() {
        // La charge utile de la SPA ne doit pas se remplir de nulls.
        let json = serde_json::to_string(&PlayerState::default()).unwrap();
        assert!(!json.contains("status"));
        assert!(!json.contains("overlay"));
    }

    #[test]
    fn playerstate_desyrialise_le_morceau_aplati_et_une_incrustation() {
        // C'est le chemin réel des afficheurs (`run_display_plugin` désérialise
        // exactement cette forme, voir le SDK) : `#[serde(flatten)]` sur le
        // morceau combiné à un enum étiqueté en interne (`Overlay`, `kind`) est
        // la conjonction la plus susceptible de surprendre avec serde. Les
        // autres tests de ce fichier ne couvrent que l'un ou l'autre
        // séparément ; en cas de régression ici, le symptôme serait muet côté
        // utilisateur (un `warn!` dans les logs et un écran figé).
        let json = r#"{
            "source": "radio",
            "volume": 65,
            "muted": false,
            "standby": false,
            "preset": 3,
            "preset_count": 12,
            "preset_name": "France Inter",
            "status": "RADIO",
            "overlay": {"kind": "volume", "level": 65, "muted": false, "text": "VOLUME 65 %", "remaining_ms": 4000},
            "artist": "Miles Davis",
            "title": "So What",
            "album": "Kind of Blue",
            "duration_s": 545,
            "origin": "icy"
        }"#;
        let etat: PlayerState = serde_json::from_str(json).unwrap();
        assert_eq!(etat.source, "radio");
        assert_eq!(etat.preset_name.as_deref(), Some("France Inter"));
        assert_eq!(
            etat.overlay,
            Some(Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4000 })
        );
        // Le morceau aplati : ces champs viennent du même niveau JSON que
        // `source`/`preset`/`overlay`, pas d'un objet imbriqué.
        assert_eq!(etat.morceau.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(etat.morceau.title.as_deref(), Some("So What"));
        assert_eq!(etat.morceau.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(etat.morceau.duration_s, Some(545));
        assert_eq!(etat.morceau.origin.as_deref(), Some("icy"));
    }

    #[test]
    fn player_state_serialise_position_et_seekable_quand_ils_disent_quelque_chose() {
        let etat = PlayerState {
            source: "cd".into(),
            position_s: Some(87),
            seekable: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(json.contains(r#""position_s":87"#), "{json}");
        assert!(json.contains(r#""seekable":true"#), "{json}");
    }

    /// Additif : une trame muette sur ces deux champs reste identique à
    /// l'octet près à ce qu'elle était avant ce chantier, et une trame
    /// écrite par un binaire antérieur se relit sans eux.
    #[test]
    fn player_state_tait_position_et_seekable_quand_ils_ne_disent_rien() {
        let etat = PlayerState { source: "radio".into(), ..Default::default() };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(!json.contains("position_s"), "{json}");
        assert!(!json.contains("seekable"), "{json}");
        let ancienne = r#"{"source":"radio","volume":50,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null}"#;
        let relue: PlayerState = serde_json::from_str(ancienne).unwrap();
        assert_eq!(relue.position_s, None);
        assert!(!relue.seekable);
    }

    #[test]
    fn enrichment_porte_une_position() {
        let e = Enrichment {
            identity: json!({"kind": "stream"}),
            position_s: Some(42),
            ..Default::default()
        };
        let back: Enrichment = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.position_s, Some(42));
        let sans = r#"{"identity":{"kind":"stream"}}"#;
        assert_eq!(serde_json::from_str::<Enrichment>(sans).unwrap().position_s, None);
    }
}
