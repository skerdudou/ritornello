use crate::metadata::IdentityUpdate;
use crate::view::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum SourceReq {
    Activate,
    /// Réveil piloté par le plugin (boot / sortie de veille). Défaut côté SDK :
    /// se comporte comme `Activate` ; un plugin peut surcharger `wake()`.
    Wake,
    Deactivate,
    Select(u8),
    Next,
    Prev,
    Eject,
    SetLocale(String),
    /// Le cœur a arrêté la lecture de sa propre initiative (touche Stop de la
    /// télécommande), **sans** que la Source ait été consultée.
    ///
    /// C'est la seule commande dans ce cas : `Play` traverse le cœur, `Eject` et
    /// `Deactivate` passent par la Source. Sans cette notification, une Source
    /// qui tient un état de lecture (le cd, pour savoir si un morceau joue
    /// vraiment) ne peut pas rester juste, et annoncerait des métadonnées pour
    /// un morceau arrêté.
    Stop,
    /// Le lecteur est passé **de lui-même** à la piste d'index `n` (fin de piste
    /// d'un disque), sans commande de l'utilisateur.
    ///
    /// Le cœur l'apprend de mpv, mais ne peut pas corriger l'identité : elle est
    /// opaque pour lui, et seule la Source sait ce que « piste n » veut dire pour
    /// ce qu'elle joue. Sans cette notification, l'affichage et les métadonnées
    /// restaient sur la piste précédente jusqu'à la prochaine commande.
    PlayerTrack(i64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: SourceReq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "data")]
pub enum SourceAction {
    Noop,
    Play { uri: String },
    Stop,
    PlayerNext,
    PlayerPrev,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMessage {
    /// `Some(id)` = réponse corrélée à une requête ; `None` = notification spontanée.
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub action: Option<SourceAction>,
    #[serde(default)]
    pub view: Option<View>,
    /// Identité de ce qui joue **après** cette action, quand la Source a de
    /// quoi la mettre à jour.
    ///
    /// Le champ voyage ici, à côté de la vue, et non dans `SourceAction::Play` :
    /// un CD change de piste sans nouveau `Play` (`PlayerNext` fait avancer mpv),
    /// donc l'identité changerait sans qu'aucun `Play` ne soit émis. Toute
    /// occasion où une Source rapporte une vue devient ainsi une occasion de
    /// corriger l'identité — ce qui couvre le changement de piste d'un disque,
    /// la sélection d'une présélection et l'arrivée différée d'une TOC.
    ///
    /// Absent = « cette trame ne dit rien de l'identité, garde la précédente ».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityUpdate>,
    /// La Source déclare que la `line2` de la vue ci-dessus est un
    /// **remplissage** : ce qu'elle a trouvé à écrire faute de mieux, que le
    /// cœur peut remplacer s'il connaît une métadonnée pour cette place.
    ///
    /// Le plugin cd s'en sert : il écrit « audio CD », et l'album le remplace
    /// quand un plugin `metadata` le rapporte. Sans cette déclaration
    /// **explicite**, la seule façon pour le cœur de savoir s'il peut écrire là
    /// serait de regarder si la ligne est vide — une négociation par l'absence,
    /// où une Source qui veut une ligne vide (une entrée auxiliaire sobre) se
    /// verrait imposer un album sans l'avoir demandé, et devrait écrire une
    /// chaîne factice pour s'en protéger.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub line2_replaceable: bool,
    /// La vue ci-dessus est un message **éphémère** : le cœur l'affiche quelques
    /// secondes, puis fait reparaître la précédente.
    ///
    /// Sans cela, un message d'incident (« présélection vide ») restait à l'écran
    /// indéfiniment, jusqu'à ce que l'utilisateur touche autre chose — alors que
    /// la lecture, elle, continuait sur la station précédente : l'affichage
    /// décrivait durablement un état qui n'existait plus.
    ///
    /// Le cœur emploie le même emplacement et la même échéance que l'incrustation
    /// volume/muet, donc la vue permanente est conservée telle quelle et reparaît
    /// d'elle-même.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub transient: bool,
    /// La touche numérotée de la télécommande à laquelle correspond ce qui joue
    /// **après** cette trame : la présélection pour la radio, la piste pour le
    /// cd. C'est ce qui permet à l'IHM de mettre en évidence la touche active —
    /// une information que la Source seule possède, le cœur n'interprétant
    /// jamais ce que `Select(n)` a voulu dire.
    ///
    /// Absent = « cette trame ne dit rien de la sélection, garde la
    /// précédente ». Le cœur l'oublie de lui-même quand plus rien ne joue
    /// (identité `Nothing`, arrêt, changement de source, veille) : il n'y a
    /// donc pas de forme « effacée » à déclarer ici.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<u8>,
    /// How many numbered presets the source currently offers: stations for
    /// the radio, tracks for the cd. This is what lets the web UI show only
    /// the numbers that exist instead of an unconditional 1-9 grid.
    ///
    /// Absent = "this frame says nothing about the count, keep the previous
    /// one". `Some(0)` is meaningful — "there is nothing to number" (cd
    /// without a disc) — and distinct from absent. The core forgets the
    /// remembered count on source change and standby (the next source
    /// re-declares it on activate/wake), but NOT on stop: a stopped radio
    /// still has its stations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_count: Option<u8>,
    /// The human-readable name the Source gives to the preset carried by
    /// `preset` above (the configured station name for the radio; the cd
    /// plugin never fills this in, since it has nothing to name here — see
    /// its metadata path instead).
    ///
    /// Absent = "this frame says nothing about the name, keep the previous
    /// one" — the same convention as `preset`. It lives and dies with
    /// `preset`: the core clears both together, and only when the identity
    /// is cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_name: Option<String>,
    /// The source's own word about its state, **already translated** by its
    /// catalogue ("NO DISC", "AUDIO CD", "EMPTY PRESET").
    ///
    /// Unlike `preset`, absent means **"no status"**, not "keep the previous
    /// one": a source restates it on every frame, and this is the only
    /// convention that lets a status be cleared at all.
    ///
    /// With `transient` set, the status is an ephemeral message: it feeds the
    /// overlay and leaves the remembered status untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_roundtrip() {
        let r = SourceRequest { id: 4, req: SourceReq::Wake };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"req\":\"Wake\""));
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, SourceReq::Wake);
    }

    #[test]
    fn set_locale_roundtrip() {
        let r = SourceRequest { id: 9, req: SourceReq::SetLocale("fr".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"req\":\"SetLocale\""));
        assert!(json.contains("\"arg\":\"fr\""));
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, SourceReq::SetLocale("fr".into()));
    }

    #[test]
    fn request_roundtrip() {
        let r = SourceRequest { id: 7, req: SourceReq::Select(3) };
        let json = serde_json::to_string(&r).unwrap();
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.req, SourceReq::Select(3));
    }

    #[test]
    fn message_reponse_avec_action_et_vue() {
        let m = SourceMessage {
            id: Some(1),
            action: Some(SourceAction::Play { uri: "http://fip".into() }),
            view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
            identity: Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream"}))),
            line2_replaceable: false,
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, Some(1));
        assert_eq!(back.action, Some(SourceAction::Play { uri: "http://fip".into() }));
        assert_eq!(back.identity, m.identity);
    }

    #[test]
    fn message_notification_sans_id() {
        let m = SourceMessage { id: None, action: None, view: Some(View::default()), identity: None, line2_replaceable: false, transient: false, preset: None, preset_count: None, preset_name: None, status: None };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, None);
        assert_eq!(back.action, None);
    }

    #[test]
    fn identite_absente_et_identite_nulle_ne_disent_pas_la_meme_chose() {
        // C'est la raison d'être de l'enum `IdentityUpdate` : « je ne dis rien
        // de l'identité » (champ omis, donc l'identité courante est conservée)
        // doit rester distinct de « plus rien ne joue » (`Nothing`, donc
        // l'identité courante est oubliée). Un `Option<Option<Value>>` aurait
        // ramené les deux à la même valeur en désérialisation.
        let rien_dit: SourceMessage = serde_json::from_str(r#"{"id":1}"#).unwrap();
        assert_eq!(rien_dit.identity, None);
        let arret: SourceMessage =
            serde_json::from_str(r#"{"id":1,"identity":{"state":"Nothing"}}"#).unwrap();
        assert_eq!(arret.identity, Some(IdentityUpdate::Nothing));
    }

    #[test]
    fn la_selection_fait_le_tour_et_reste_absente_par_defaut() {
        // Roundtrip du champ, et compatibilité : une trame d'un plugin
        // antérieur (sans le champ) doit se relire comme « rien déclaré ».
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            view: None,
            identity: None,
            line2_replaceable: false,
            transient: false,
            preset: Some(4),
            preset_count: None,
            preset_name: None,
            status: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"preset\":4"));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preset, Some(4));
        let ancien: SourceMessage = serde_json::from_str(r#"{"id":3}"#).unwrap();
        assert_eq!(ancien.preset, None);
    }

    #[test]
    fn identite_absente_nest_pas_serialisee() {
        // La majorité des trames ne disent rien de l'identité (SetLocale,
        // Deactivate…) : les alourdir d'un `"identity":null` serait du bruit sur
        // une liaison volontairement lisible à l'œil.
        let m = SourceMessage { id: Some(2), action: None, view: None, identity: None, line2_replaceable: false, transient: false, preset: None, preset_count: None, preset_name: None, status: None };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"id":2,"action":null,"view":null}"#);
    }

    #[test]
    fn le_compte_fait_le_tour_et_reste_absent_par_defaut() {
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            view: None,
            identity: None,
            line2_replaceable: false,
            transient: false,
            preset: None,
            preset_count: Some(23),
            preset_name: None,
            status: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"preset_count\":23"));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preset_count, Some(23));
        // Trame d'un plugin antérieur : rien déclaré.
        let ancien: SourceMessage = serde_json::from_str(r#"{"id":3}"#).unwrap();
        assert_eq!(ancien.preset_count, None);
        // Some(0) est porteur de sens (cd sans disque) et doit voyager tel quel,
        // distinct de l'absence.
        let zero: SourceMessage = serde_json::from_str(r#"{"id":3,"preset_count":0}"#).unwrap();
        assert_eq!(zero.preset_count, Some(0));
    }

    #[test]
    fn le_nom_fait_le_tour_et_reste_absent_par_defaut() {
        // Aller-retour du champ, avec un preset assorti : c'est ainsi que le
        // plugin radio le déclare toujours (voir `play_preset`).
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            view: None,
            identity: None,
            line2_replaceable: false,
            transient: false,
            preset: Some(4),
            preset_count: None,
            preset_name: Some("FIP".into()),
            status: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"preset_name\":\"FIP\""));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.preset_name.as_deref(), Some("FIP"));
    }

    #[test]
    fn une_trame_dun_plugin_anterieur_sans_preset_name_se_relit_comme_rien_declare() {
        // Rétrocompatibilité : un plugin qui ne connaît pas encore ce champ
        // (ou une trame qui ne dit rien du nom) doit se désérialiser sans
        // erreur, le champ retombant sur `None` — « garde la valeur
        // courante », pas « efface-la ».
        let ancien: SourceMessage = serde_json::from_str(r#"{"id":3,"preset":4}"#).unwrap();
        assert_eq!(ancien.preset_name, None);
        assert_eq!(ancien.preset, Some(4));
    }

    #[test]
    fn le_statut_fait_le_tour_et_reste_absent_par_defaut() {
        // Convention différente de `preset`/`preset_name` : ici l'absence est
        // testée sur une trame qui déclare explicitement `status: None` (une
        // Source qui n'a plus rien à dire de son état), pas sur une trame d'un
        // plugin antérieur — voir `Core::handle_source_update` pour la raison.
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            view: None,
            identity: None,
            line2_replaceable: false,
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: Some("PAS DE DISQUE".into()),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"status\":\"PAS DE DISQUE\""));
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status.as_deref(), Some("PAS DE DISQUE"));
        // Une trame d'un plugin antérieur (ou qui ne dit rien du statut) se
        // relit sans erreur, le champ retombant sur `None`.
        let ancien: SourceMessage = serde_json::from_str(r#"{"id":3}"#).unwrap();
        assert_eq!(ancien.status, None);
    }
}
