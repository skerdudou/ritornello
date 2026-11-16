//! Pure equalizer model: settings, presets, validation, the automatic
//! preamp and the mpv audio-filter strings. No I/O here — persistence, the
//! mpv primitives and the HTTP route build on this module.

// The later tasks (persistence, mpv primitives, core logic, HTTP route) are
// the consumers; until they land nothing outside the tests uses this.
#![allow(dead_code)]

use ritornello_i18n::Chain;
use serde::{Deserialize, Serialize};

/// Centre frequencies of the ten bands, in Hz.
pub const BANDS_HZ: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
pub const GAIN_MIN: i8 = -12;
pub const GAIN_MAX: i8 = 12;
pub const MAX_USER_PRESETS: usize = 20;
pub const MAX_NAME_CHARS: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreset {
    pub name: String,
    pub gains: [i8; 10],
}

/// `serde(default)` at the container, as `Settings` has it: a block written
/// without some field (hand-edited, or by an older core) keeps the fields it
/// does say. It still has to pass `validate` to be believed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqualizerSettings {
    pub enabled: bool,
    pub auto_preamp: bool,
    pub gains: [i8; 10],
    /// `None`, `"builtin:<id>"` or `"user:<name>"`.
    pub preset: Option<String>,
    pub user_presets: Vec<UserPreset>,
}

impl Default for EqualizerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_preamp: true,
            gains: [0; 10],
            preset: None,
            user_presets: Vec::new(),
        }
    }
}

pub struct BuiltinPreset {
    pub id: &'static str,
    pub gains: [i8; 10],
}

pub const BUILTIN_PRESETS: [BuiltinPreset; 7] = [
    BuiltinPreset { id: "flat", gains: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
    BuiltinPreset { id: "pop", gains: [-1, 0, 2, 3, 4, 3, 1, 0, -1, -2] },
    BuiltinPreset { id: "rock", gains: [5, 4, 3, 1, -1, -1, 1, 3, 4, 5] },
    BuiltinPreset { id: "classical", gains: [0, 0, 0, 0, 0, 0, -1, -2, -2, -3] },
    BuiltinPreset { id: "jazz", gains: [3, 2, 1, 2, -1, -1, 0, 1, 2, 3] },
    BuiltinPreset { id: "electronic", gains: [6, 5, 3, 0, -2, 0, 1, 4, 5, 6] },
    BuiltinPreset { id: "voice", gains: [-4, -3, -2, 0, 2, 4, 4, 3, 0, -2] },
];

/// Equalizer validation error. Same shape as `AudioOutputError`: the
/// user-facing text comes from `message(&Chain)`, `Display` is English for
/// the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EqualizerError {
    GainOutOfRange,
    NameEmpty,
    NameTooLong,
    DuplicateName,
    TooManyPresets,
    UnknownPreset,
}

impl EqualizerError {
    pub fn message(&self, catalog: &Chain) -> String {
        let key = match self {
            EqualizerError::GainOutOfRange => "eq_error_gain_range",
            EqualizerError::NameEmpty => "eq_error_name_empty",
            EqualizerError::NameTooLong => "eq_error_name_too_long",
            EqualizerError::DuplicateName => "eq_error_duplicate_name",
            EqualizerError::TooManyPresets => "eq_error_too_many_presets",
            EqualizerError::UnknownPreset => "eq_error_unknown_preset",
        };
        catalog.get(key).to_string()
    }
}

impl std::fmt::Display for EqualizerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EqualizerError::GainOutOfRange => "equalizer gain out of range (-12..12 dB)",
            EqualizerError::NameEmpty => "empty equalizer preset name",
            EqualizerError::NameTooLong => "equalizer preset name too long (40 characters max)",
            EqualizerError::DuplicateName => "equalizer preset name already used",
            EqualizerError::TooManyPresets => "too many equalizer presets (20 max)",
            EqualizerError::UnknownPreset => "unknown equalizer preset",
        })
    }
}

impl std::error::Error for EqualizerError {}

fn gains_in_range(gains: &[i8; 10]) -> bool {
    gains.iter().all(|g| (GAIN_MIN..=GAIN_MAX).contains(g))
}

pub fn validate(s: &EqualizerSettings) -> Result<(), EqualizerError> {
    if !gains_in_range(&s.gains) {
        return Err(EqualizerError::GainOutOfRange);
    }
    if s.user_presets.len() > MAX_USER_PRESETS {
        return Err(EqualizerError::TooManyPresets);
    }
    let mut seen: Vec<String> = Vec::new();
    for p in &s.user_presets {
        let name = p.name.trim();
        if name.is_empty() {
            return Err(EqualizerError::NameEmpty);
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(EqualizerError::NameTooLong);
        }
        if !gains_in_range(&p.gains) {
            return Err(EqualizerError::GainOutOfRange);
        }
        let folded = name.to_lowercase();
        if seen.contains(&folded) {
            return Err(EqualizerError::DuplicateName);
        }
        seen.push(folded);
    }
    if let Some(preset) = &s.preset {
        let known = if let Some(id) = preset.strip_prefix("builtin:") {
            BUILTIN_PRESETS.iter().any(|b| b.id == id)
        } else if let Some(name) = preset.strip_prefix("user:") {
            s.user_presets.iter().any(|p| p.name == name)
        } else {
            false
        };
        if !known {
            return Err(EqualizerError::UnknownPreset);
        }
    }
    Ok(())
}

const SAMPLE_RATE: f64 = 48_000.0;

/// Magnitude in dB of an RBJ peaking biquad (the one ffmpeg's `equalizer`
/// uses, width in octaves = 1) at frequency `f`.
fn peaking_response_db(f0: f64, gain_db: f64, f: f64) -> f64 {
    use std::f64::consts::{LN_2, PI};
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * f0 / SAMPLE_RATE;
    let alpha = w0.sin() * (LN_2 / 2.0 * 1.0 * w0 / w0.sin()).sinh();
    let b = [1.0 + alpha * a, -2.0 * w0.cos(), 1.0 - alpha * a];
    let den = [1.0 + alpha / a, -2.0 * w0.cos(), 1.0 - alpha / a];
    let w = 2.0 * PI * f / SAMPLE_RATE;
    let mag2 = |c: &[f64; 3]| {
        let re = c[0] + c[1] * w.cos() + c[2] * (2.0 * w).cos();
        let im = -(c[1] * w.sin() + c[2] * (2.0 * w).sin());
        re * re + im * im
    };
    10.0 * (mag2(&b) / mag2(&den)).log10()
}

/// Opposite of the peak of the combined response, never above 0.
pub fn preamp_db(gains: &[i8; 10]) -> f64 {
    const POINTS: usize = 400;
    let (lo, hi) = (20f64.ln(), 20_000f64.ln());
    let mut peak = f64::MIN;
    for i in 0..POINTS {
        let f = (lo + (hi - lo) * i as f64 / (POINTS - 1) as f64).exp();
        let total: f64 = BANDS_HZ
            .iter()
            .zip(gains)
            .map(|(&hz, &g)| peaking_response_db(f64::from(hz), f64::from(g), f))
            .sum();
        peak = peak.max(total);
    }
    -peak.max(0.0)
}

pub fn effective_preamp_db(s: &EqualizerSettings) -> f64 {
    if s.auto_preamp {
        preamp_db(&s.gains)
    } else {
        0.0
    }
}

/// At most two decimals, no trailing zeros, never `-0`.
fn fmt_db(v: f64) -> String {
    let mut s = format!("{v:.2}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

pub fn af_chain(s: &EqualizerSettings) -> String {
    if !s.enabled {
        return String::new();
    }
    let mut parts = vec![format!(
        "@eqpre:lavfi=[volume={}dB]",
        fmt_db(effective_preamp_db(s))
    )];
    for (i, (hz, g)) in BANDS_HZ.iter().zip(s.gains).enumerate() {
        parts.push(format!("@eq{i}:lavfi=[equalizer=f={hz}:t=o:w=1:g={g}]"));
    }
    parts.join(",")
}

/// One `af-command` call: label, command, argument, target filter.
#[derive(Debug, Clone, PartialEq)]
pub struct AfCommand {
    pub label: String,
    pub command: String,
    pub arg: String,
    pub target: String,
}

pub fn band_command(band: usize, gain: i8) -> AfCommand {
    AfCommand {
        label: format!("eq{band}"),
        command: "g".into(),
        arg: gain.to_string(),
        target: "equalizer".into(),
    }
}

pub fn preamp_command(db: f64) -> AfCommand {
    AfCommand {
        label: "eqpre".into(),
        command: "volume".into(),
        arg: format!("{}dB", fmt_db(db)),
        target: "volume".into(),
    }
}

/// Intermediate preamp values up to `to_db` inclusive, each step at most
/// 1 dB (a larger jump clicks). Empty when equal.
pub fn preamp_ramp(from_db: f64, to_db: f64) -> Vec<f64> {
    let diff = to_db - from_db;
    if diff == 0.0 {
        return Vec::new();
    }
    let steps = ((diff.abs() - 1e-9).ceil() as usize).max(1);
    (1..=steps)
        .map(|i| {
            if i == steps {
                to_db
            } else {
                from_db + diff * i as f64 / steps as f64
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> UserPreset {
        UserPreset { name: name.into(), gains: [0; 10] }
    }

    fn with_users(names: &[&str]) -> EqualizerSettings {
        EqualizerSettings {
            user_presets: names.iter().map(|n| named(n)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn default_is_disabled_flat_with_compensation() {
        let s = EqualizerSettings::default();
        assert!(!s.enabled);
        assert!(s.auto_preamp);
        assert_eq!(s.gains, [0; 10]);
        assert_eq!(s.preset, None);
        assert!(s.user_presets.is_empty());
    }

    #[test]
    fn builtins_are_in_range_and_flat_is_flat() {
        for b in &BUILTIN_PRESETS {
            assert!(b.gains.iter().all(|g| (-12..=12).contains(g)), "{}", b.id);
        }
        assert_eq!(BUILTIN_PRESETS[0].gains, [0; 10]);
        let ids: Vec<_> = BUILTIN_PRESETS.iter().map(|b| b.id).collect();
        assert_eq!(
            ids,
            ["flat", "pop", "rock", "classical", "jazz", "electronic", "voice"]
        );
    }

    #[test]
    fn validate_refuses_each_bad_shape() {
        let mut s = EqualizerSettings::default();
        s.gains[3] = 13;
        assert_eq!(validate(&s), Err(EqualizerError::GainOutOfRange));
        s.gains[3] = -13;
        assert_eq!(validate(&s), Err(EqualizerError::GainOutOfRange));

        assert_eq!(validate(&with_users(&["  "])), Err(EqualizerError::NameEmpty));
        assert_eq!(
            validate(&with_users(&[&"é".repeat(41)])),
            Err(EqualizerError::NameTooLong)
        );
        assert_eq!(
            validate(&with_users(&["Salon", "salon"])),
            Err(EqualizerError::DuplicateName)
        );
        let many: Vec<String> = (0..21).map(|i| format!("p{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(validate(&with_users(&refs)), Err(EqualizerError::TooManyPresets));

        let mut s = EqualizerSettings {
            preset: Some("user:absent".into()),
            ..Default::default()
        };
        assert_eq!(validate(&s), Err(EqualizerError::UnknownPreset));
        s.preset = Some("builtin:nope".into());
        assert_eq!(validate(&s), Err(EqualizerError::UnknownPreset));

        let twenty: Vec<String> = (0..20).map(|i| format!("p{i}")).collect();
        let refs: Vec<&str> = twenty.iter().map(String::as_str).collect();
        let mut ok = with_users(&refs);
        ok.user_presets[0].name = "é".repeat(40);
        ok.preset = Some(format!("user:{}", "é".repeat(40)));
        assert_eq!(validate(&ok), Ok(()));
        ok.preset = Some("builtin:rock".into());
        assert_eq!(validate(&ok), Ok(()));
    }

    #[test]
    fn preamp_is_zero_for_cuts_only() {
        assert_eq!(preamp_db(&[-6; 10]), 0.0);
    }

    #[test]
    fn preamp_matches_a_lone_band() {
        let mut g = [0; 10];
        g[5] = 12;
        let p = preamp_db(&g);
        assert!((-12.5..=-11.5).contains(&p), "{p}");
        // Pinned against an independent RBJ implementation (bw = 1 octave).
        assert!((p + 12.00).abs() < 0.05, "{p}");
    }

    #[test]
    fn preamp_exceeds_max_for_adjacent_boosts() {
        let mut g = [0; 10];
        g[5] = 12;
        g[6] = 12;
        let p = preamp_db(&g);
        assert!(p < -12.5, "{p}");
        assert!((p + 14.60).abs() < 0.05, "{p}");
    }

    fn flat_chain(pre: &str) -> String {
        let mut parts = vec![format!("@eqpre:lavfi=[volume={pre}]")];
        for (i, hz) in BANDS_HZ.iter().enumerate() {
            parts.push(format!("@eq{i}:lavfi=[equalizer=f={hz}:t=o:w=1:g=0]"));
        }
        parts.join(",")
    }

    #[test]
    fn chain_is_empty_when_disabled() {
        assert_eq!(af_chain(&EqualizerSettings::default()), "");
    }

    #[test]
    fn chain_matches_the_measured_form() {
        let mut s = EqualizerSettings { enabled: true, ..Default::default() };
        assert_eq!(af_chain(&s), flat_chain("0dB"));
        assert!(af_chain(&s).starts_with(
            "@eqpre:lavfi=[volume=0dB],@eq0:lavfi=[equalizer=f=31:t=o:w=1:g=0],"
        ));

        s.gains[5] = 6;
        let c = af_chain(&s);
        assert!(c.contains("@eq5:lavfi=[equalizer=f=1000:t=o:w=1:g=6]"), "{c}");
        assert!(c.contains("[volume=-"), "{c}");

        s.auto_preamp = false;
        assert!(af_chain(&s).starts_with("@eqpre:lavfi=[volume=0dB],"));
    }

    #[test]
    fn commands_carry_their_target() {
        let b = band_command(5, 6);
        assert_eq!(
            (b.label.as_str(), b.command.as_str(), b.arg.as_str(), b.target.as_str()),
            ("eq5", "g", "6", "equalizer")
        );
        let p = preamp_command(-6.0);
        assert_eq!(
            (p.label.as_str(), p.command.as_str(), p.arg.as_str(), p.target.as_str()),
            ("eqpre", "volume", "-6dB", "volume")
        );
        assert_eq!(preamp_command(-0.0).arg, "0dB");
        assert_eq!(preamp_command(-3.456).arg, "-3.46dB");
    }

    #[test]
    fn ramp_steps_by_at_most_one_db() {
        let r = preamp_ramp(0.0, -12.0);
        assert_eq!(r.len(), 12);
        assert_eq!(*r.last().unwrap(), -12.0);
        let mut prev = 0.0;
        for v in &r {
            assert!((v - prev).abs() <= 1.0 + 1e-9);
            prev = *v;
        }
        assert!(preamp_ramp(-3.0, -3.0).is_empty());
        assert_eq!(*preamp_ramp(-2.5, 0.0).last().unwrap(), 0.0);
    }

    #[test]
    fn validate_boundaries() {
        for g in [12, -12] {
            let s = EqualizerSettings { gains: [g; 10], ..Default::default() };
            assert_eq!(validate(&s), Ok(()));
        }

        for (g, ok) in [(13, false), (-13, false), (12, true), (-12, true)] {
            let mut u = with_users(&["a"]);
            u.user_presets[0].gains[9] = g;
            let want = if ok { Ok(()) } else { Err(EqualizerError::GainOutOfRange) };
            assert_eq!(validate(&u), want, "user gain {g}");
        }

        assert_eq!(
            validate(&with_users(&[" Salon ", "salon"])),
            Err(EqualizerError::DuplicateName)
        );
        for bad in ["builtin:", "user:"] {
            let s = EqualizerSettings { preset: Some(bad.into()), ..Default::default() };
            assert_eq!(validate(&s), Err(EqualizerError::UnknownPreset), "{bad}");
        }
        let padded = format!("  {}  ", "x".repeat(40));
        assert_eq!(validate(&with_users(&[&padded])), Ok(()));
    }

    #[test]
    fn limit_texts_carry_the_constants() {
        let dir = tempfile::tempdir().unwrap();
        let cat = ritornello_i18n::Chain::load_for_tests("core", "en", dir.path(), crate::i18n::EN);
        for (err, n) in [
            (EqualizerError::GainOutOfRange, GAIN_MAX.to_string()),
            (EqualizerError::NameTooLong, MAX_NAME_CHARS.to_string()),
            (EqualizerError::TooManyPresets, MAX_USER_PRESETS.to_string()),
        ] {
            assert!(err.to_string().contains(&n), "Display {err:?}");
            assert!(err.message(&cat).contains(&n), "catalog {err:?}");
        }
    }
}
