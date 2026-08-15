use crate::metadata::IdentityUpdate;
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
    Play {
        uri: String,
        /// Index de départ dans la liste que `uri` désigne, quand c'en est une.
        ///
        /// Absent = « commence au début », le comportement historique. Le cœur
        /// applique `playlist-pos` juste après `loadfile` : mesuré fiable, mpv
        /// résolvant un `.m3u` dès la commande, sans dépliage différé.
        ///
        /// C'est l'unique moyen pour une Source de reprendre une liste à la
        /// piste n — chiffre de la télécommande, ou reprise après redémarrage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<i64>,
        /// Ce que `uri` désigne a une **fin normale** : un disque, une liste de
        /// fichiers. Quand mpv devient inactif, c'est la fin du contenu, pas
        /// une coupure de flux à relancer.
        ///
        /// Absent (= `false`) veut dire « flux live », le comportement
        /// historique : c'est ce qui garde les trames de la radio inchangées.
        /// Remplace le reniflage `uri.starts_with("cdda://")` du cœur, qui
        /// devinait ce que seule la Source sait.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        finite: bool,
    },
    Stop,
    PlayerNext,
    PlayerPrev,
}

impl SourceAction {
    /// Lecture d'une URI, aux défauts historiques : depuis le début, flux live.
    ///
    /// Passer par ce constructeur plutôt que par la variante littérale évite
    /// qu'un champ ajouté plus tard n'oblige à retoucher tous les appelants.
    pub fn play(uri: impl Into<String>) -> Self {
        SourceAction::Play { uri: uri.into(), start: None, finite: false }
    }

    /// Positionne la lecture sur l'élément d'index `n` de la liste. Sans effet
    /// sur une action qui n'est pas un `Play`.
    #[must_use]
    pub fn starting_at(self, n: i64) -> Self {
        match self {
            SourceAction::Play { uri, finite, .. } => {
                SourceAction::Play { uri, start: Some(n), finite }
            }
            autre => autre,
        }
    }

    /// Déclare un contenu fini, dont l'inactivité de mpv signale la fin et non
    /// une coupure. Sans effet sur une action qui n'est pas un `Play`.
    #[must_use]
    pub fn finite(self) -> Self {
        match self {
            SourceAction::Play { uri, start, .. } => {
                SourceAction::Play { uri, start, finite: true }
            }
            autre => autre,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMessage {
    /// `Some(id)` = réponse corrélée à une requête ; `None` = notification spontanée.
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub action: Option<SourceAction>,
    /// Identité de ce qui joue **après** cette action, quand la Source a de
    /// quoi la mettre à jour.
    ///
    /// Un CD change de piste sans nouveau `Play` (`PlayerNext` fait avancer
    /// mpv), donc l'identité changerait sans qu'aucun `Play` ne soit émis.
    /// Toute occasion où une Source rapporte du neuf (statut, présélection)
    /// devient ainsi une occasion de corriger l'identité — ce qui couvre le
    /// changement de piste d'un disque, la sélection d'une présélection et
    /// l'arrivée différée d'une TOC.
    ///
    /// Absent = « cette trame ne dit rien de l'identité, garde la précédente ».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityUpdate>,
    /// Le statut ci-dessus est un message **éphémère** : le cœur l'affiche
    /// quelques secondes, puis fait reparaître le statut permanent.
    ///
    /// Sans cela, un message d'incident (« présélection vide ») restait à l'écran
    /// indéfiniment, jusqu'à ce que l'utilisateur touche autre chose — alors que
    /// la lecture, elle, continuait sur la station précédente : l'affichage
    /// décrivait durablement un état qui n'existait plus.
    ///
    /// Le cœur emploie le même emplacement et la même échéance que l'incrustation
    /// volume/muet, donc le statut permanent est conservé tel quel et reparaît
    /// de lui-même.
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
    fn play_sans_champs_neufs_reste_serialise_a_l_identique() {
        // La garantie de compatibilité : une trame émise par le plugin radio
        // ne doit pas changer d'un octet. Sans quoi les traces d'un
        // `journalctl` deviendraient impossibles à comparer d'une version à
        // l'autre, sur une liaison voulue lisible à l'œil.
        let a = SourceAction::play("http://icecast.radiofrance.fr/fip-midfi.mp3");
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            r#"{"action":"Play","data":{"uri":"http://icecast.radiofrance.fr/fip-midfi.mp3"}}"#
        );
    }

    #[test]
    fn start_et_finite_font_le_tour() {
        let a = SourceAction::play("/var/lib/ritornello/plugin-files.m3u").starting_at(4).finite();
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains(r#""start":4"#), "{json}");
        assert!(json.contains(r#""finite":true"#), "{json}");
        let back: SourceAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn une_trame_anterieure_se_relit_en_flux_live_depuis_le_debut() {
        // Un plugin antérieur n'émet ni `start` ni `finite` : les défauts
        // doivent reproduire exactement le comportement historique (flux live,
        // début de liste), sans quoi une mise à jour partielle des binaires
        // changerait silencieusement la lecture.
        let back: SourceAction =
            serde_json::from_str(r#"{"action":"Play","data":{"uri":"http://x"}}"#).unwrap();
        assert_eq!(back, SourceAction::Play { uri: "http://x".into(), start: None, finite: false });
    }

    #[test]
    fn les_constructeurs_ne_touchent_pas_aux_autres_actions() {
        // `starting_at` et `finite` sont écrits pour être enchaînables sans
        // que l'appelant ait à savoir quelle variante il tient. Le garde-fou :
        // appliqués ailleurs, ils ne doivent rien transformer en `Play`.
        assert_eq!(SourceAction::Stop.starting_at(3), SourceAction::Stop);
        assert_eq!(SourceAction::Noop.finite(), SourceAction::Noop);
        assert_eq!(SourceAction::PlayerNext.starting_at(1).finite(), SourceAction::PlayerNext);
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
    fn message_reponse_avec_action_et_identite() {
        let m = SourceMessage {
            id: Some(1),
            action: Some(SourceAction::play("http://fip")),
            identity: Some(IdentityUpdate::Playing(serde_json::json!({"kind": "stream"}))),
            transient: false,
            preset: None,
            preset_count: None,
            preset_name: None,
            status: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, Some(1));
        assert_eq!(back.action, Some(SourceAction::play("http://fip")));
        assert_eq!(back.identity, m.identity);
    }

    #[test]
    fn message_notification_sans_id() {
        let m = SourceMessage { id: None, action: None, identity: None, transient: false, preset: None, preset_count: None, preset_name: None, status: None };
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
            identity: None,
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
        let m = SourceMessage { id: Some(2), action: None, identity: None, transient: false, preset: None, preset_count: None, preset_name: None, status: None };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"id":2,"action":null}"#);
    }

    #[test]
    fn le_compte_fait_le_tour_et_reste_absent_par_defaut() {
        let m = SourceMessage {
            id: Some(3),
            action: Some(SourceAction::Noop),
            identity: None,
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
            identity: None,
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
            identity: None,
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
