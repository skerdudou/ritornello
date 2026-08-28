use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// What the device does with the active source when the process starts.
/// Read once, at launch, by `Core::demarrage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPower {
    /// Wake the active source: the device plays again on its own.
    #[default]
    On,
    /// Configure mpv but leave the source asleep, standby on the display.
    Standby,
    /// Whatever the device was doing when it last wrote its state
    /// (`PersistedState::standby`) — on after a crash mid-listening,
    /// standby after a power cut that followed a deliberate standby.
    Previous,
}

/// Comment une date s'écrit sur cet appareil.
///
/// **Un choix fermé et non un motif libre.** Le propriétaire a demandé deux
/// réglages séparés, date et heure ; un motif à la `strftime` serait plus
/// souple et donnerait un afficheur vide au premier motif fautif, sur un
/// appareil de salon où personne ne lit de journal. Trois formes couvrent ce
/// que les pays écrivent réellement, et chacune est infalsifiable.
///
/// Le **séparateur** appartient à la forme et n'est pas un réglage de plus :
/// `2026-12-31` avec des barres obliques ne se lit nulle part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateFormat {
    /// `31/12/2026` — la forme française, et le défaut.
    #[default]
    DayMonthYear,
    /// `2026-12-31` — ISO 8601, celle qui se trie.
    YearMonthDay,
    /// `12/31/2026` — la forme nord-américaine.
    MonthDayYear,
}

/// Behavior settings, edited on the config page (`PUT /api/settings`).
/// Container-level `serde(default)`: a partial block in a hand-edited
/// state.json fills in with defaults instead of failing to load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Hold-to-repeat: delay before the first repeated volume step.
    pub volume_repeat_initial_ms: u32,
    /// Hold-to-repeat: delay between subsequent volume steps.
    pub volume_repeat_interval_ms: u32,
    /// On, standby, or "as it was" at launch — see `StartupPower`.
    pub startup_power: StartupPower,
    /// How long the volume/mute overlay and sources' transient messages
    /// (e.g. "empty preset") stay on screen before the permanent view
    /// reappears. Deliberately a separate field from `tens_window_ms`,
    /// not shared: this overlay hides the "now playing" view and may want
    /// to shrink one day, while the tens-offset window below must stay
    /// comfortable regardless — coupling them would forbid tuning either
    /// on its own.
    pub overlay_ms: u32,
    /// How long the remote's pending `+10`/`+20`/... offset stays armed,
    /// shown as the `+NN` overlay: the time left to press the second
    /// digit. Independent from `overlay_ms` for the same reason in
    /// reverse — see that field's comment. The core stores each overlay's
    /// own deadline (`overlay: Option<(Overlay, Instant)>`), so
    /// `show_tens_overlay` reading this field and `expire_overlay`
    /// staying oblivious to which duration produced the deadline keeps
    /// the offset and its overlay disarming together **on expiry**,
    /// whatever the two values are. That alone would not be enough: the
    /// overlay slot can also be taken over before its deadline — by the
    /// abandon guard in `appliquer_commande`, or by a source's transient
    /// message in `handle_source_update` — and both of those explicitly
    /// clear the offset too, so it never survives behind a display that
    /// no longer shows it.
    pub tens_window_ms: u32,
    /// Pas des touches « avancer » / « reculer », en secondes.
    ///
    /// Réglable là où le pas de volume est figé, parce que la bonne valeur
    /// dépend de ce qu'on écoute : dix secondes pour rattraper une phrase,
    /// une minute pour traverser un mouvement.
    pub seek_step_s: u32,

    // ---- Comment l'appareil écrit une date et une heure ------------------
    //
    // **Deux réglages et non un**, à la demande du propriétaire, et la
    // séparation se défend : l'ordre des composants d'une date et le choix
    // 12/24 h ne varient pas ensemble d'un pays à l'autre. Un anglophone peut
    // vouloir `2026-12-31` et 24 h, un autre `12/31/2026` et 12 h.
    //
    // Ils servent deux publics, et c'est pour cela qu'ils vivent ici plutôt
    // que dans chaque consommateur : l'heure de l'afficheur en veille, et la
    // date des « dernières erreurs » de la page Système.
    //
    // **Aucun réglage de fuseau**, et c'est délibéré : l'afficheur tourne
    // *sur* l'appareil, donc son horloge est déjà la bonne ; la page web
    // formate côté navigateur, donc dans le fuseau de qui regarde — ce qui est
    // juste pour un téléphone qui voyage. Un réglage de plus ne pourrait que
    // contredire l'un des deux.
    /// L'ordre des composants d'une date. Voir `DateFormat`.
    pub date_format: DateFormat,
    /// Heure sur 24 h (`13:05`) plutôt que sur 12 h (`1:05 PM`).
    pub clock_24h: bool,

    // ---- Pochettes : ce qui entre, puis ce qui sort ----------------------
    //
    // Deux étages qu'il ne faut pas confondre, et c'est pourquoi le premier
    // réglage vit **hors** de l'interrupteur dans l'IHM.
    //
    // `cover_source_max_mio` borne ce que le cœur accepte de lire, quoi qu'il
    // arrive ensuite : c'est la seule protection quand le réencodage est
    // désactivé, et la plus économique de toutes, puisqu'elle se juge sur la
    // taille du fichier sans lire un octet de son contenu.
    //
    // Les cinq autres ne décrivent que le **rendu** — ce que le cœur fabrique
    // pour le pousser sur un socket. Interrupteur décoché, aucun n'a de sens :
    // la source part telle quelle.
    /// Plafond de la pochette **source**, en mébioctets.
    ///
    /// Toujours actif, réencodage ou pas. Borné à
    /// `ritornello_proto::COVER_MAX_BYTES` (20 Mio) par la validation, et c'est
    /// structurel : cette constante est une promesse de **protocole** — elle dit
    /// aux greffons le maximum qu'ils peuvent recevoir, et le greffon MPD
    /// dimensionne ses propres bornes dessus sans pouvoir consulter les
    /// réglages du cœur. Ce champ ne peut donc que l'abaisser.
    pub cover_source_max_mio: u32,

    /// Réencoder les pochettes avant de les pousser, ou pousser la source
    /// telle quelle ?
    ///
    /// Décoché, le cœur ne décode plus rien : il pousse les octets d'origine, et
    /// le pic mémoire d'une publication redevient celui de l'image source (près
    /// de 72 Mio pour une pochette de 20 Mio, entre les octets, leur base64 et
    /// la ligne JSON) au lieu de ~1,8 Mio pour une vignette. C'est un choix
    /// défendable — un afficheur qui veut la pleine résolution, une machine qui
    /// a la RAM — mais il faut le faire en le sachant.
    pub cover_rendition: bool,

    /// Côté le plus long de la vignette, en pixels. Le rapport est conservé.
    pub cover_max_edge_px: u32,

    /// Qualité JPEG de la vignette, de 1 à 100.
    ///
    /// Ne s'applique qu'au JPEG : une pochette à canal alpha est réencodée en
    /// PNG, sans perte, parce qu'aplatir sa transparence sur un fond deviné
    /// serait un choix visuel que l'appareil n'a pas à faire.
    pub cover_jpeg_quality: u8,

    /// Plafond de la vignette **produite**, en kibioctets.
    ///
    /// Un filet, pas une cible : le côté maximal borne déjà le nombre de pixels,
    /// donc une vignette dépasse ce plafond seulement sur une image
    /// pathologiquement bruitée. Dépassement = rien n'est poussé, et le journal
    /// nomme ce réglage — plutôt qu'une boucle de réencodages dégressifs dont le
    /// coût serait invisible.
    pub cover_max_bytes_ko: u32,

    /// Plafond de **pixels** à décoder, en mégapixels.
    ///
    /// La garde anti-bombe de décompression, et la seule qui compte vraiment :
    /// les dimensions sont lues dans l'en-tête **avant toute allocation**, et un
    /// fichier qui les dépasse est refusé sans être décodé. Un PNG de 200 Kio
    /// peut annoncer 30000 × 30000 pixels, soit 3,6 Gio de tampon décodé — la
    /// taille du fichier ne dit rien du coût du décodage, et c'est exactement ce
    /// que ce réglage borne là où `cover_source_max_mio` ne peut rien.
    ///
    /// Son libellé dans l'IHM porte le calcul `l × h × 4`, parce que la valeur
    /// utile n'est pas le nombre de mégapixels mais les mébioctets qu'ils
    /// coûtent : 16 Mpx, c'est 64 Mio de tampon.
    pub cover_max_pixels_mpx: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume_repeat_initial_ms: 800,
            volume_repeat_interval_ms: 200,
            startup_power: StartupPower::On,
            overlay_ms: 5000,
            tens_window_ms: 5000,
            seek_step_s: 10,
            // Les défauts du pays de l'appareil, pas des défauts neutres : il
            // n'y a pas de forme « neutre » de date, et celle-ci est celle de
            // son propriétaire.
            date_format: DateFormat::DayMonthYear,
            clock_24h: true,
            // Le plafond du protocole lui-même : par défaut le cœur n'ajoute
            // aucune restriction à ce que les greffons savent déjà encaisser.
            cover_source_max_mio: ritornello_proto::COVER_MAX_BYTES as u32 / (1024 * 1024),
            // Activé par défaut : sur un Pi 2 à 1 Gio partagé entre mpv, le
            // cœur, l'IHM et dix greffons, pousser 20 Mio d'image brute est le
            // mauvais défaut même si l'appareil y survit.
            cover_rendition: true,
            // 640 px : au-delà de ce que le plus grand afficheur du parc sait
            // montrer, et l'IHM web n'affiche la pochette qu'à 224 px sur son
            // plus grand palier.
            cover_max_edge_px: 640,
            // 85 : le seuil au-delà duquel un JPEG grossit sans que l'œil y
            // gagne, sur une image de cette taille.
            cover_jpeg_quality: 85,
            // 512 Kio, soit largement au-dessus d'une vignette 640 px typique
            // (60 à 120 Kio) : le filet ne doit pas se déclencher en usage
            // normal, sinon ce n'est plus un filet mais une limite.
            cover_max_bytes_ko: 512,
            // 16 Mpx = 64 Mio de tampon décodé. Couvre une pochette scannée en
            // 4000 × 4000 avec de la marge, et refuse la bombe.
            cover_max_pixels_mpx: 16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub active_source: String,
    pub volume: u8,
    /// Whether the device was in standby when this state was last written.
    /// Only `StartupPower::Previous` reads it; every path that toggles
    /// standby writes it (see `Core::persist` callers), so it describes the
    /// last observed reality rather than an intention.
    #[serde(default)]
    pub standby: bool,
    #[serde(default)]
    pub audio_device: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    /// Preset de thème choisi (nom opaque pour le cœur : la liste des presets
    /// vit dans la SPA). Absent = `theme::DEFAULT_THEME`.
    #[serde(default)]
    pub theme: Option<String>,
    /// `"light"` ou `"dark"`. Absent = `theme::DEFAULT_MODE`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Behavior settings (hold-to-repeat timings, startup power state).
    #[serde(default)]
    pub settings: Settings,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self { active_source: "radio".into(), volume: 60, standby: false, audio_device: None, locale: None, theme: None, mode: None, settings: Settings::default() }
    }
}

pub fn load(path: &Path) -> PersistedState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, state: &PersistedState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaut_si_fichier_absent_ou_corrompu() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.json");
        assert_eq!(load(&missing), PersistedState::default());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{pas du json").unwrap();
        assert_eq!(load(&bad), PersistedState::default());
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState {
            active_source: "cd".into(),
            volume: 35,
            standby: false,
            audio_device: Some("bluealsa:DEV=XX".into()),
            locale: None,
            theme: None,
            mode: None,
            settings: Settings::default(),
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn defaut_est_radio_vol60_sans_sortie_choisie() {
        let d = PersistedState::default();
        assert_eq!(d.active_source, "radio");
        assert_eq!(d.volume, 60);
        assert_eq!(d.audio_device, None);
    }

    #[test]
    fn locale_absente_par_defaut_et_roundtrip() {
        assert_eq!(PersistedState::default().locale, None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState {
            active_source: "radio".into(),
            volume: 50,
            standby: false,
            audio_device: None,
            locale: Some("fr".into()),
            theme: None,
            mode: None,
            settings: Settings::default(),
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn theme_et_mode_absents_par_defaut_et_roundtrip() {
        assert_eq!(PersistedState::default().theme, None);
        assert_eq!(PersistedState::default().mode, None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState {
            active_source: "radio".into(),
            volume: 50,
            standby: false,
            audio_device: None,
            locale: None,
            theme: Some("cyberpunk".into()),
            mode: Some("dark".into()),
            settings: Settings::default(),
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn un_state_json_anterieur_reste_lisible() {
        // Compatibilite ascendante : un fichier ecrit avant cette version n'a
        // ni `theme` ni `mode` ; il doit se charger sans erreur.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"audio_device":null,"locale":"fr"}"#,
        )
        .unwrap();
        let st = load(&path);
        assert_eq!(st.volume, 42);
        assert_eq!(st.locale.as_deref(), Some("fr"));
        assert_eq!(st.theme, None);
        assert_eq!(st.mode, None);
    }

    #[test]
    fn settings_par_defaut() {
        let s = Settings::default();
        assert_eq!(s.volume_repeat_initial_ms, 800);
        assert_eq!(s.volume_repeat_interval_ms, 200);
        assert_eq!(s.startup_power, StartupPower::On);
        assert_eq!(s.overlay_ms, 5000);
        assert_eq!(s.tens_window_ms, 5000);
        assert_eq!(s.seek_step_s, 10);
        assert_eq!(PersistedState::default().settings, Settings::default());
    }

    #[test]
    fn un_state_json_sans_settings_reste_lisible() {
        // Backward compatibility: a state.json written before this version has
        // no `settings` block; it must load with the defaults.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"audio_device":null,"locale":"fr"}"#,
        )
        .unwrap();
        let st = load(&path);
        assert_eq!(st.settings, Settings::default());
        assert_eq!(st.volume, 42);
    }

    #[test]
    fn settings_roundtrip_et_bloc_partiel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        // Non-default values throughout, overlay_ms/tens_window_ms included:
        // a fixture carrying 5000 would no longer distinguish "the default
        // applied" from "the written value survived" — exactly the defect a
        // review flagged on the volume fixture above.
        let st = PersistedState {
            settings: Settings {
                volume_repeat_initial_ms: 900,
                volume_repeat_interval_ms: 250,
                startup_power: StartupPower::Previous,
                overlay_ms: 6000,
                tens_window_ms: 7000,
                seek_step_s: 45,
                // Non-défaut toutes les deux, même raison : le défaut est
                // `DayMonthYear` et `true`.
                date_format: DateFormat::YearMonthDay,
                clock_24h: false,
                // Six valeurs non-défaut de plus, pour la raison écrite
                // au-dessus : une fixture qui reprendrait les défauts ne
                // distinguerait pas « la valeur écrite a survécu » de « le
                // défaut s'est appliqué ». `cover_rendition` à `false` est le
                // cas qui compte le plus ici — c'est le seul booléen du lot, et
                // son défaut est `true`.
                cover_source_max_mio: 12,
                cover_rendition: false,
                cover_max_edge_px: 800,
                cover_jpeg_quality: 70,
                cover_max_bytes_ko: 256,
                cover_max_pixels_mpx: 24,
            },
            ..Default::default()
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
        // A hand-edited partial block falls back to defaults for what's missing.
        std::fs::write(&path, r#"{"active_source":"radio","volume":42,"settings":{"startup_power":"standby"}}"#).unwrap();
        let st = load(&path);
        assert_eq!(st.settings.startup_power, StartupPower::Standby);
        assert_eq!(st.settings.volume_repeat_initial_ms, 800);
        assert_eq!(st.settings.overlay_ms, 5000);
        assert_eq!(st.settings.tens_window_ms, 5000);
        assert_eq!(st.settings.seek_step_s, 10);
    }

    #[test]
    fn la_veille_persistee_vaut_faux_sans_la_cle_et_survit_au_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"active_source":"radio","volume":42}"#).unwrap();
        assert!(!load(&path).standby, "sans la cle, on repart eveille");

        let st = PersistedState { standby: true, ..Default::default() };
        save(&path, &st).unwrap();
        assert!(load(&path).standby);
    }
}
