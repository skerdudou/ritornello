//! Validation des reglages recus par PUT /api/settings : les plages admises et l'erreur typee qui cite ses bornes.

use super::*;

/// Bornes des réglages, définies une seule fois et prises à la
/// comparaison elle-même : `SettingsError` les reporte telles quelles dans
/// ses paramètres, pour qu'un changement de borne ne puisse plus laisser un
/// message qui mente sur ses propres limites.
pub(super) const INITIAL_DELAY_MS: std::ops::RangeInclusive<u32> = 200..=5000;

pub(super) const REPEAT_INTERVAL_MS: std::ops::RangeInclusive<u32> = 100..=2000;

// Same bounds for both overlay durations: under a second an overlay is
// unreadable and the tens-offset capture becomes impractical (it takes two
// presses inside the window); past roughly fifteen seconds an overlay
// durably hides the "now playing" view.
pub(super) const OVERLAY_MS: std::ops::RangeInclusive<u32> = 1000..=15000;

pub(super) const TENS_WINDOW_MS: std::ops::RangeInclusive<u32> = 1000..=15000;

/// Bornes du pas de déplacement, en secondes. Une seconde en bas parce qu'un
/// pas nul ne déplace rien ; deux minutes en haut parce qu'au-delà, la touche
/// ne sert plus à se déplacer dans une piste mais à en changer.
pub(super) const SEEK_STEP_S: std::ops::RangeInclusive<u32> = 1..=120;

/// Plafond de la pochette source, en mébioctets.
///
/// La borne haute n'est **pas** un choix de confort : c'est
/// `ritornello_proto::COVER_MAX_BYTES`, exprimée dans l'unité du réglage.
/// Cette constante est la promesse faite aux greffons sur ce qu'ils peuvent
/// recevoir, et le greffon MPD dimensionne ses propres bornes dessus sans
/// pouvoir lire les réglages du cœur. La calculer ici plutôt que d'écrire
/// « 20 » interdit qu'un jour les deux divergent en silence.
pub(super) const COVER_SOURCE_MAX_MIO: std::ops::RangeInclusive<u32> =
    1..=(ritornello_proto::COVER_MAX_BYTES as u32 / (1024 * 1024));

/// Côté maximal de la vignette. 64 px en bas parce qu'en dessous ce n'est plus
/// une pochette mais une pastille ; 2048 px en haut parce qu'au-delà le rendu
/// coûte plus que ce qu'il économise — c'est déjà le double de ce que le plus
/// grand afficheur du parc sait montrer.
pub(super) const COVER_MAX_EDGE_PX: std::ops::RangeInclusive<u32> = 64..=2048;

/// Qualité JPEG. 40 en bas : en dessous, les artefacts sont visibles sur les
/// dégradés d'une pochette. 100 en haut, la borne du format.
pub(super) const COVER_JPEG_QUALITY: std::ops::RangeInclusive<u32> = 40..=100;

/// Plafond de la vignette produite, en kibioctets. 32 Kio en bas, sous quoi une
/// vignette 640 px ne tient pas et le filet se déclencherait toujours ; 8 Mio en
/// haut, ce qui le rend inopérant pour qui veut le neutraliser sans décocher le
/// réencodage.
pub(super) const COVER_MAX_BYTES_KO: std::ops::RangeInclusive<u32> = 32..=8192;

/// Nombre de pochettes gardées. 2 en bas — moins qu'un aller-retour entre deux
/// pistes n'a aucun sens ; 100 en haut, ce qui plafonne le pire cas mémoire à
/// 200 Mio (chaque entrée réseau est bornée à 2 Mio, une entrée locale ne garde
/// qu'un chemin), au-delà de quoi un Pi à 1 Gio se mettrait en danger.
pub(super) const COVER_CACHE_ENTRIES: std::ops::RangeInclusive<u32> = 2..=100;

/// Plafond de pixels à décoder, en mégapixels — donc quatre fois autant de
/// mébioctets de tampon. 1 Mpx en bas (déjà 4 Mio) ; 64 Mpx en haut, soit
/// 256 Mio, ce qu'un Pi 2 à 1 Gio ne peut pas dépasser sans se mettre en
/// danger.
pub(super) const COVER_MAX_PIXELS_MPX: std::ops::RangeInclusive<u32> = 1..=64;

/// Erreur de validation des réglages, une variante par borne violée. Même
/// modèle que `AudioOutputError` : les paramètres `min`/`max` viennent de la
/// borne effectivement comparée, jamais recopiés à la main.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    InitialDelay { min: u32, max: u32 },
    RepeatInterval { min: u32, max: u32 },
    Overlay { min: u32, max: u32 },
    TensWindow { min: u32, max: u32 },
    SeekStep { min: u32, max: u32 },
    CoverSourceMax { min: u32, max: u32 },
    CoverMaxEdge { min: u32, max: u32 },
    CoverJpegQuality { min: u32, max: u32 },
    CoverMaxBytes { min: u32, max: u32 },
    CoverMaxPixels { min: u32, max: u32 },
    CoverCacheEntries { min: u32, max: u32 },
}

impl SettingsError {
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            SettingsError::InitialDelay { min, max } => catalog
                .get("settings_initial_delay_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::RepeatInterval { min, max } => catalog
                .get("settings_repeat_interval_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::Overlay { min, max } => catalog
                .get("settings_overlay_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::TensWindow { min, max } => catalog
                .get("settings_tens_window_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::SeekStep { min, max } => catalog
                .get("settings_seek_step_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::CoverSourceMax { min, max } => catalog
                .get("settings_cover_source_max_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::CoverMaxEdge { min, max } => catalog
                .get("settings_cover_max_edge_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::CoverJpegQuality { min, max } => catalog
                .get("settings_cover_jpeg_quality_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::CoverMaxBytes { min, max } => catalog
                .get("settings_cover_max_bytes_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::CoverCacheEntries { min, max } => catalog
                .get("settings_cover_cache_entries_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
            SettingsError::CoverMaxPixels { min, max } => catalog
                .get("settings_cover_max_pixels_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
        }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::InitialDelay { min, max } => {
                write!(f, "initial delay out of range ({min}-{max} ms)")
            }
            SettingsError::RepeatInterval { min, max } => {
                write!(f, "repeat interval out of range ({min}-{max} ms)")
            }
            SettingsError::Overlay { min, max } => {
                write!(f, "overlay duration out of range ({min}-{max} ms)")
            }
            SettingsError::TensWindow { min, max } => {
                write!(f, "tens-offset entry window out of range ({min}-{max} ms)")
            }
            SettingsError::SeekStep { min, max } => {
                write!(f, "seek step out of range ({min}-{max} s)")
            }
            SettingsError::CoverSourceMax { min, max } => {
                write!(f, "source cover ceiling out of range ({min}-{max} MiB)")
            }
            SettingsError::CoverMaxEdge { min, max } => {
                write!(f, "cover thumbnail edge out of range ({min}-{max} px)")
            }
            SettingsError::CoverJpegQuality { min, max } => {
                write!(f, "cover JPEG quality out of range ({min}-{max})")
            }
            SettingsError::CoverMaxBytes { min, max } => {
                write!(f, "rendered cover ceiling out of range ({min}-{max} KiB)")
            }
            SettingsError::CoverMaxPixels { min, max } => {
                write!(f, "cover decode ceiling out of range ({min}-{max} Mpx)")
            }
            SettingsError::CoverCacheEntries { min, max } => {
                write!(f, "cover cache size out of range ({min}-{max} entries)")
            }
        }
    }
}

impl std::error::Error for SettingsError {}

/// Bounds for the hold-to-repeat timings. Pure function, same model as
/// `validate_audio_device`: the core itself accepts anything (tests use tiny
/// timings), the HTTP surface is where user input is checked.
pub fn validate_settings(s: &crate::state::Settings) -> Result<(), SettingsError> {
    if !INITIAL_DELAY_MS.contains(&s.volume_repeat_initial_ms) {
        return Err(SettingsError::InitialDelay {
            min: *INITIAL_DELAY_MS.start(),
            max: *INITIAL_DELAY_MS.end(),
        });
    }
    if !REPEAT_INTERVAL_MS.contains(&s.volume_repeat_interval_ms) {
        return Err(SettingsError::RepeatInterval {
            min: *REPEAT_INTERVAL_MS.start(),
            max: *REPEAT_INTERVAL_MS.end(),
        });
    }
    if !OVERLAY_MS.contains(&s.overlay_ms) {
        return Err(SettingsError::Overlay { min: *OVERLAY_MS.start(), max: *OVERLAY_MS.end() });
    }
    if !TENS_WINDOW_MS.contains(&s.tens_window_ms) {
        return Err(SettingsError::TensWindow {
            min: *TENS_WINDOW_MS.start(),
            max: *TENS_WINDOW_MS.end(),
        });
    }
    if !SEEK_STEP_S.contains(&s.seek_step_s) {
        return Err(SettingsError::SeekStep {
            min: *SEEK_STEP_S.start(),
            max: *SEEK_STEP_S.end(),
        });
    }
    if !COVER_SOURCE_MAX_MIO.contains(&s.cover_source_max_mio) {
        return Err(SettingsError::CoverSourceMax {
            min: *COVER_SOURCE_MAX_MIO.start(),
            max: *COVER_SOURCE_MAX_MIO.end(),
        });
    }
    // Les quatre suivants ne décrivent que le rendu. Ils sont validés **même
    // quand `cover_rendition` est faux**, et c'est voulu : l'IHM grise ces
    // champs sans les vider, donc leurs valeurs continuent de voyager dans le
    // PUT. Les laisser passer sans contrôle sous prétexte qu'ils dorment
    // ferait accepter une valeur absurde qui ne se révélerait qu'au moment de
    // recocher l'interrupteur, très loin du geste qui l'a introduite.
    if !COVER_MAX_EDGE_PX.contains(&s.cover_max_edge_px) {
        return Err(SettingsError::CoverMaxEdge {
            min: *COVER_MAX_EDGE_PX.start(),
            max: *COVER_MAX_EDGE_PX.end(),
        });
    }
    if !COVER_JPEG_QUALITY.contains(&u32::from(s.cover_jpeg_quality)) {
        return Err(SettingsError::CoverJpegQuality {
            min: *COVER_JPEG_QUALITY.start(),
            max: *COVER_JPEG_QUALITY.end(),
        });
    }
    if !COVER_MAX_BYTES_KO.contains(&s.cover_max_bytes_ko) {
        return Err(SettingsError::CoverMaxBytes {
            min: *COVER_MAX_BYTES_KO.start(),
            max: *COVER_MAX_BYTES_KO.end(),
        });
    }
    if !COVER_CACHE_ENTRIES.contains(&s.cover_cache_entries) {
        return Err(SettingsError::CoverCacheEntries {
            min: *COVER_CACHE_ENTRIES.start(),
            max: *COVER_CACHE_ENTRIES.end(),
        });
    }
    if !COVER_MAX_PIXELS_MPX.contains(&s.cover_max_pixels_mpx) {
        return Err(SettingsError::CoverMaxPixels {
            min: *COVER_MAX_PIXELS_MPX.start(),
            max: *COVER_MAX_PIXELS_MPX.end(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_settings_borne_les_deux_delais() {
        use crate::state::Settings;
        assert!(validate_settings(&Settings::default()).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 200, volume_repeat_interval_ms: 100, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 5000, volume_repeat_interval_ms: 2000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 199, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 5001, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_interval_ms: 99, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_interval_ms: 2001, ..Default::default() }).is_err());
    }

    #[test]
    fn validate_settings_borne_les_deux_durees_dincrustation() {
        use crate::state::Settings;
        assert!(validate_settings(&Settings { overlay_ms: 1000, tens_window_ms: 1000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { overlay_ms: 15000, tens_window_ms: 15000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { overlay_ms: 999, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { overlay_ms: 15001, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { tens_window_ms: 999, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { tens_window_ms: 15001, ..Default::default() }).is_err());
    }

    #[test]
    fn validate_settings_rend_la_bonne_variante_avec_ses_bornes() {
        use crate::state::Settings;
        assert_eq!(
            validate_settings(&Settings { volume_repeat_initial_ms: 1, ..Default::default() }),
            Err(SettingsError::InitialDelay { min: 200, max: 5000 })
        );
        assert_eq!(
            validate_settings(&Settings { volume_repeat_interval_ms: 1, ..Default::default() }),
            Err(SettingsError::RepeatInterval { min: 100, max: 2000 })
        );
        assert_eq!(
            validate_settings(&Settings { overlay_ms: 1, ..Default::default() }),
            Err(SettingsError::Overlay { min: 1000, max: 15000 })
        );
        assert_eq!(
            validate_settings(&Settings { tens_window_ms: 1, ..Default::default() }),
            Err(SettingsError::TensWindow { min: 1000, max: 15000 })
        );
        assert_eq!(
            validate_settings(&Settings { seek_step_s: 0, ..Default::default() }),
            Err(SettingsError::SeekStep { min: 1, max: 120 })
        );
    }

    #[test]
    fn message_settings_interpole_les_bornes_contre_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "settings_initial_delay_out_of_range = \"delai hors bornes ({min}-{max})\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::i18n::EN);
        let err = SettingsError::InitialDelay { min: 200, max: 5000 };
        assert_eq!(err.message(&cat), "delai hors bornes (200-5000)");
    }

    /// Le refus est une phrase du catalogue, jamais une chaîne en dur, et il
    /// **cite ses bornes** : c'est la règle « les bornes ne peuvent pas
    /// mentir » que le chantier i18n a posée.
    #[test]
    fn le_refus_du_pas_cite_ses_bornes() {
        // Chemin inexistant : le catalogue retombe sur l'anglais embarqué,
        // celui-là même que la clé doit désormais contenir.
        let catalogue = ritornello_i18n::Catalog::load(
            "core",
            "en",
            std::path::Path::new("/inexistant"),
            crate::i18n::EN,
        );
        let message = SettingsError::SeekStep { min: 1, max: 120 }.message(&catalogue);
        assert!(message.contains('1') && message.contains("120"), "{message}");
        assert!(!message.contains("{min}"), "clé non substituée : {message}");
        assert_ne!(message, "settings_seek_step_out_of_range", "clé absente du catalogue");
    }
}
