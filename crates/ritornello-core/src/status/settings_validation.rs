//! Validation of the settings received by PUT /api/settings: the admitted ranges and the typed error that quotes its bounds.

use super::*;

/// Bounds of the settings, defined once and taken from the comparison
/// itself: `SettingsError` reports them as they are in its parameters, so
/// that a change of bound can no longer leave a message lying about its own
/// limits.
pub(super) const INITIAL_DELAY_MS: std::ops::RangeInclusive<u32> = 200..=5000;

pub(super) const REPEAT_INTERVAL_MS: std::ops::RangeInclusive<u32> = 100..=2000;

// Same bounds for both overlay durations: under a second an overlay is
// unreadable and the tens-offset capture becomes impractical (it takes two
// presses inside the window); past roughly fifteen seconds an overlay
// durably hides the "now playing" view.
pub(super) const OVERLAY_MS: std::ops::RangeInclusive<u32> = 1000..=15000;

pub(super) const TENS_WINDOW_MS: std::ops::RangeInclusive<u32> = 1000..=15000;

/// Bounds of the seek step, in seconds. One second at the bottom because a
/// zero step moves nothing; two minutes at the top because beyond that, the
/// key no longer serves to move within a track but to change it.
pub(super) const SEEK_STEP_S: std::ops::RangeInclusive<u32> = 1..=120;

/// Cap of the source cover, in mebibytes.
///
/// The upper bound is **not** a comfort choice: it is
/// `ritornello_proto::COVER_MAX_BYTES`, expressed in the setting's unit. That
/// constant is the promise made to the plugins about what they may receive,
/// and the MPD plugin sizes its own bounds on it without being able to read
/// the core's settings. Computing it here rather than writing "20" forbids
/// the two from ever silently diverging.
pub(super) const COVER_SOURCE_MAX_MIO: std::ops::RangeInclusive<u32> =
    1..=(ritornello_proto::COVER_MAX_BYTES as u32 / (1024 * 1024));

/// Maximum edge of the thumbnail. 64 px at the bottom because below that it is
/// no longer a cover but a dot; 2048 px at the top because beyond that the
/// rendition costs more than it saves — it is already twice what the largest
/// display in the fleet can show.
pub(super) const COVER_MAX_EDGE_PX: std::ops::RangeInclusive<u32> = 64..=2048;

/// JPEG quality. 40 at the bottom: below that, artifacts are visible on the
/// gradients of a cover. 100 at the top, the bound of the format.
pub(super) const COVER_JPEG_QUALITY: std::ops::RangeInclusive<u32> = 40..=100;

/// Cap of the produced thumbnail, in kibibytes. 32 KiB at the bottom, under
/// which a 640 px thumbnail does not fit and the safety net would always
/// trigger; 8 MiB at the top, which makes it inoperative for whoever wants to
/// neutralize it without unticking re-encoding.
pub(super) const COVER_MAX_BYTES_KO: std::ops::RangeInclusive<u32> = 32..=8192;

/// Number of covers kept. 2 at the bottom — less than a round trip between two
/// tracks makes no sense; 100 at the top, which caps the worst-case memory at
/// 200 MiB (each network entry is bounded to 2 MiB, a local entry keeps only a
/// path), beyond which a 1 GiB Pi would put itself in danger.
pub(super) const COVER_CACHE_ENTRIES: std::ops::RangeInclusive<u32> = 2..=100;

/// Cap of pixels to decode, in megapixels — hence four times as many
/// mebibytes of buffer. 1 Mpx at the bottom (already 4 MiB); 64 Mpx at the
/// top, i.e. 256 MiB, which a 1 GiB Pi 2 cannot exceed without putting itself
/// in danger.
pub(super) const COVER_MAX_PIXELS_MPX: std::ops::RangeInclusive<u32> = 1..=64;

/// Settings validation error, one variant per violated bound. Same model as
/// `AudioOutputError`: the `min`/`max` parameters come from the bound actually
/// compared, never copied by hand.
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
    // The next four only describe the rendition. They are validated **even
    // when `cover_rendition` is false**, and that is intended: the UI greys
    // these fields out without clearing them, so their values keep travelling
    // in the PUT. Letting them through unchecked because they are dormant
    // would accept an absurd value that would only reveal itself when the
    // switch is ticked again, very far from the gesture that introduced it.
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
    fn validate_settings_bounds_both_delays() {
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
    fn validate_settings_bounds_both_overlay_durations() {
        use crate::state::Settings;
        assert!(validate_settings(&Settings { overlay_ms: 1000, tens_window_ms: 1000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { overlay_ms: 15000, tens_window_ms: 15000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { overlay_ms: 999, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { overlay_ms: 15001, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { tens_window_ms: 999, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { tens_window_ms: 15001, ..Default::default() }).is_err());
    }

    #[test]
    fn validate_settings_returns_the_right_variant_with_its_bounds() {
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
    fn settings_message_interpolates_the_bounds_against_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/fr.toml"),
            "settings_initial_delay_out_of_range = \"timeout hors bornes ({min}-{max})\"\n",
        )
        .unwrap();
        let cat = ritornello_i18n::Catalog::load("core", "fr", dir.path(), crate::i18n::EN);
        let err = SettingsError::InitialDelay { min: 200, max: 5000 };
        assert_eq!(err.message(&cat), "timeout hors bornes (200-5000)");
    }

    /// The refusal is a sentence from the catalog, never a hard-coded string,
    /// and it **quotes its bounds**: that is the rule "the bounds cannot lie"
    /// that the i18n work laid down.
    #[test]
    fn the_seek_step_refusal_quotes_its_bounds() {
        // Nonexistent path: the catalog falls back to the embedded English,
        // the very one the key must now contain.
        let catalog = ritornello_i18n::Catalog::load(
            "core",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::i18n::EN,
        );
        let message = SettingsError::SeekStep { min: 1, max: 120 }.message(&catalog);
        assert!(message.contains('1') && message.contains("120"), "{message}");
        assert!(!message.contains("{min}"), "key not substituted: {message}");
        assert_ne!(message, "settings_seek_step_out_of_range", "key missing from the catalog");
    }
}
