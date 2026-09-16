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

/// Weight under which a cover is pushed without re-encoding, in kibibytes.
/// 16 KiB at the bottom: below that nothing real passes untouched and the
/// threshold would be inert. 2 MiB at the top, past which one is no longer
/// setting a threshold but disabling re-encoding by the back door — the switch
/// is there for that, and says so.
pub(super) const COVER_PASSTHROUGH_MAX_KO: std::ops::RangeInclusive<u32> = 16..=2048;

/// Memory budget for covers, in mebibytes. 8 at the bottom, under which
/// even one worst-case entry would not fit and the cache would thrash;
/// 256 at the top, already a quarter of a 1 GiB Pi's memory.
///
/// **That quarter is only true because the cached buffers are trimmed**, and
/// the figure this ceiling was chosen from was briefly wrong. The budget sums
/// `len()`; the allocator charges `capacity()`, and both cached buffers are
/// grown from an empty `Vec` — chunk by chunk in `cover::download`, by the
/// encoder in `cover::encode`. Geometric growth leaves a capacity of up to
/// nearly twice the length, so an untrimmed cache at this ceiling could
/// occupy close to *half* a 1 GiB Pi instead. Both call sites now
/// `shrink_to_fit` before handing their buffer over; remove either and this
/// bound stops meaning what it says.
pub(super) const COVER_CACHE_BUDGET_MIO: std::ops::RangeInclusive<u32> = 8..=256;

/// Cap on a downloaded cover, in mebibytes. Its ceiling is
/// `ritornello_proto::COVER_MAX_BYTES` expressed in this unit, computed
/// rather than written out, for the same reason as `COVER_SOURCE_MAX_MIO`:
/// the two must never silently diverge from the protocol's promise.
pub(super) const COVER_DOWNLOAD_MAX_MIO: std::ops::RangeInclusive<u32> =
    1..=(ritornello_proto::COVER_MAX_BYTES as u32 / (1024 * 1024));

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
    CoverPassthroughMax { min: u32, max: u32 },
    CoverMaxPixels { min: u32, max: u32 },
    CoverCacheBudget { min: u32, max: u32 },
    CoverDownloadMax { min: u32, max: u32 },
    UpdateHour { value: u32 },
}

impl SettingsError {
    pub fn message(&self, catalog: &Catalog) -> String {
        // Every arm below used to chain two `.replace()` calls
        // (`{min}` then `{max}`) directly on the resolved string — the same
        // shape task 10b removed from `core`, `admin` and `update`. `min`
        // and `max` are `u32::to_string()`, so they can never themselves
        // contain the literal text `{min}`/`{max}` today, but that safety
        // rests entirely on the values staying numeric, a property nothing
        // here enforces; routed through `ritornello_i18n::interpolate` like
        // every other producer instead of leaving twelve copy-paste
        // instances of the banned shape in the tree.
        let (key, min, max) = match self {
            SettingsError::InitialDelay { min, max } => {
                ("settings_initial_delay_out_of_range", *min, *max)
            }
            SettingsError::RepeatInterval { min, max } => {
                ("settings_repeat_interval_out_of_range", *min, *max)
            }
            SettingsError::Overlay { min, max } => ("settings_overlay_out_of_range", *min, *max),
            SettingsError::TensWindow { min, max } => {
                ("settings_tens_window_out_of_range", *min, *max)
            }
            SettingsError::SeekStep { min, max } => {
                ("settings_seek_step_out_of_range", *min, *max)
            }
            SettingsError::CoverSourceMax { min, max } => {
                ("settings_cover_source_max_out_of_range", *min, *max)
            }
            SettingsError::CoverMaxEdge { min, max } => {
                ("settings_cover_max_edge_out_of_range", *min, *max)
            }
            SettingsError::CoverJpegQuality { min, max } => {
                ("settings_cover_jpeg_quality_out_of_range", *min, *max)
            }
            SettingsError::CoverPassthroughMax { min, max } => {
                ("settings_cover_passthrough_max_out_of_range", *min, *max)
            }
            SettingsError::CoverCacheBudget { min, max } => {
                ("settings_cover_cache_budget_out_of_range", *min, *max)
            }
            SettingsError::CoverDownloadMax { min, max } => {
                ("settings_cover_download_max_out_of_range", *min, *max)
            }
            SettingsError::CoverMaxPixels { min, max } => {
                ("settings_cover_max_pixels_out_of_range", *min, *max)
            }
            SettingsError::UpdateHour { value } => {
                return ritornello_i18n::interpolate(
                    catalog.get("settings_update_hour_range"),
                    [("value", value.to_string().as_str())],
                );
            }
        };
        ritornello_i18n::interpolate(
            catalog.get(key),
            [("min", min.to_string().as_str()), ("max", max.to_string().as_str())],
        )
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
            SettingsError::CoverPassthroughMax { min, max } => {
                write!(f, "cover pass-through threshold out of range ({min}-{max} KiB)")
            }
            SettingsError::CoverMaxPixels { min, max } => {
                write!(f, "cover decode ceiling out of range ({min}-{max} Mpx)")
            }
            SettingsError::CoverCacheBudget { min, max } => {
                write!(f, "cover cache budget out of range ({min}-{max} MiB)")
            }
            SettingsError::CoverDownloadMax { min, max } => {
                write!(f, "cover download ceiling out of range ({min}-{max} MiB)")
            }
            SettingsError::UpdateHour { value } => {
                write!(f, "update hour out of range (0-23), got {value}")
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
    if !COVER_CACHE_BUDGET_MIO.contains(&s.cover_cache_budget_mio) {
        return Err(SettingsError::CoverCacheBudget {
            min: *COVER_CACHE_BUDGET_MIO.start(),
            max: *COVER_CACHE_BUDGET_MIO.end(),
        });
    }
    if !COVER_DOWNLOAD_MAX_MIO.contains(&s.cover_download_max_mio) {
        return Err(SettingsError::CoverDownloadMax {
            min: *COVER_DOWNLOAD_MAX_MIO.start(),
            max: *COVER_DOWNLOAD_MAX_MIO.end(),
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
    if !COVER_PASSTHROUGH_MAX_KO.contains(&s.cover_passthrough_max_ko) {
        return Err(SettingsError::CoverPassthroughMax {
            min: *COVER_PASSTHROUGH_MAX_KO.start(),
            max: *COVER_PASSTHROUGH_MAX_KO.end(),
        });
    }
    if !COVER_MAX_PIXELS_MPX.contains(&s.cover_max_pixels_mpx) {
        return Err(SettingsError::CoverMaxPixels {
            min: *COVER_MAX_PIXELS_MPX.start(),
            max: *COVER_MAX_PIXELS_MPX.end(),
        });
    }
    // 0-23. A settings file edited by hand can say 24, and an hour that never
    // matches is a policy that silently never runs.
    if s.update_hour > 23 {
        return Err(SettingsError::UpdateHour { value: s.update_hour });
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
    fn a_budget_outside_its_bounds_is_refused() {
        use crate::state::Settings;
        let mut s = Settings { cover_cache_budget_mio: 7, ..Default::default() };
        assert!(validate_settings(&s).is_err(), "7 MiB is below the 8 MiB floor");
        s.cover_cache_budget_mio = 257;
        assert!(validate_settings(&s).is_err(), "257 MiB is above the 256 MiB ceiling");
        s.cover_cache_budget_mio = 50;
        assert!(validate_settings(&s).is_ok());
    }

    #[test]
    fn a_passthrough_threshold_outside_its_bounds_is_refused() {
        use crate::state::Settings;
        let mut s = Settings { cover_passthrough_max_ko: 15, ..Default::default() };
        assert!(validate_settings(&s).is_err(), "15 KiB is below the 16 KiB floor");
        s.cover_passthrough_max_ko = 2049;
        assert!(validate_settings(&s).is_err(), "2049 KiB is above the 2048 KiB ceiling");
        s.cover_passthrough_max_ko = 150;
        assert!(validate_settings(&s).is_ok(), "the product default must validate");
    }

    #[test]
    fn a_download_ceiling_above_the_protocol_promise_is_refused() {
        use crate::state::Settings;
        // The protocol guarantees plugins at most COVER_MAX_BYTES; a download
        // ceiling above it would promise what the protocol does not.
        let mut s = Settings {
            cover_download_max_mio: (ritornello_proto::COVER_MAX_BYTES as u32 / (1024 * 1024)) + 1,
            ..Default::default()
        };
        assert!(validate_settings(&s).is_err());
        s.cover_download_max_mio = 0;
        assert!(validate_settings(&s).is_err(), "zero would refuse every download");
    }

    /// `update_hour` has a single bound: `u32` already refuses a value below
    /// 0, so there is no neighbouring floor to test — only the ceiling at 23.
    #[test]
    fn the_update_hour_stays_within_the_day() {
        use crate::state::Settings;
        assert!(
            validate_settings(&Settings { update_hour: 23, ..Default::default() }).is_ok(),
            "23 is the last valid hour"
        );
        assert_eq!(
            validate_settings(&Settings { update_hour: 24, ..Default::default() }),
            Err(SettingsError::UpdateHour { value: 24 })
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

    /// `SettingsError::message` used to chain two `.replace()` calls on
    /// every `min`/`max` arm (task 10b, F-2 review round): the same shape
    /// removed from every other producer of catalog text. This walks every
    /// variant and proves the substitution is still complete after routing
    /// through `ritornello_i18n::interpolate` — no arm was left resolving to
    /// its own key or with an unfilled `{min}`/`{max}`/`{value}` token.
    #[test]
    fn every_settings_error_resolves_with_its_bounds_filled_in() {
        let catalog = ritornello_i18n::Catalog::load(
            "core",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::i18n::EN,
        );
        let all = [
            SettingsError::InitialDelay { min: 200, max: 5000 },
            SettingsError::RepeatInterval { min: 100, max: 2000 },
            SettingsError::Overlay { min: 1000, max: 15000 },
            SettingsError::TensWindow { min: 1000, max: 15000 },
            SettingsError::SeekStep { min: 1, max: 120 },
            SettingsError::CoverSourceMax { min: 1, max: 20 },
            SettingsError::CoverMaxEdge { min: 64, max: 2048 },
            SettingsError::CoverJpegQuality { min: 40, max: 100 },
            SettingsError::CoverPassthroughMax { min: 16, max: 2048 },
            SettingsError::CoverMaxPixels { min: 1, max: 64 },
            SettingsError::CoverCacheBudget { min: 8, max: 256 },
            SettingsError::CoverDownloadMax { min: 1, max: 20 },
            SettingsError::UpdateHour { value: 24 },
        ];
        for err in &all {
            let message = err.message(&catalog);
            assert!(!message.contains('{'), "{err:?} left a parameter unfilled: {message}");
            assert!(!message.starts_with("settings_"), "{err:?} fell through to its own key: {message}");
        }
    }

    /// Pinned exact strings for the two `min`/`max` arms already covered
    /// above by a different assertion shape, plus `UpdateHour`'s single
    /// parameter: proof that routing through `interpolate` produced **the
    /// same output** as the chained `.replace()` calls it replaced, not
    /// merely "some" substitution.
    #[test]
    fn interpolate_produces_the_exact_same_strings_the_chained_replace_did() {
        let catalog = ritornello_i18n::Catalog::load(
            "core",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::i18n::EN,
        );
        assert_eq!(
            SettingsError::InitialDelay { min: 200, max: 5000 }.message(&catalog),
            "initial delay out of range (200-5000 ms)"
        );
        assert_eq!(
            SettingsError::CoverJpegQuality { min: 40, max: 100 }.message(&catalog),
            "cover JPEG quality out of range (40-100)"
        );
        assert_eq!(
            SettingsError::UpdateHour { value: 24 }.message(&catalog),
            "The update hour must be between 0 and 23, not 24"
        );
    }
}
