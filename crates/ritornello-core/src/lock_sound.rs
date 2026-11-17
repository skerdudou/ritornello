//! The sounds played when the physical remote is locked and unlocked, and the
//! strings that make mpv play them.
//!
//! Nothing here touches mpv or the disk: the module turns a setting into text.
//! Each sound is synthesised by ffmpeg's `aevalsrc` from a closed-form
//! expression, so no audio file ships with the core. The sounds are the ones
//! of the web reference (A4 = 440 Hz, a sum of decaying partials per note), and
//! the envelope is the reference's Web Audio one: a 15 ms exponential attack
//! from 0.0001 to 0.22, then an exponential decay to 0.0001 over the timbre's
//! decay time.
//!
//! Two ways to play one, both measured on mpv 0.37 / FFmpeg 6.1 (the spike of
//! the plan): while something plays, [`mix_filter`] mixes the chime into the
//! audio chain without a click (`amix`, `normalize=0`, the main stream
//! untouched); while mpv is idle, [`idle_uri`] is loaded as a one-off file.
//! A third, measured by the paused spike: while paused, the chime is mixed
//! over the music silenced ([`mix_filter_over_silence`]) and mpv is unpaused
//! for its length.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The persisted choice (`Settings::lock_sound`): silence, or one of five
/// sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockSound {
    None,
    #[default]
    A,
    B,
    C,
    D,
    E,
}

/// Which motif of a sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motif {
    Lock,
    Unlock,
    /// The lock motif, then the unlock motif starting right after it ends
    /// (at `duration(sound, Motif::Lock)`): what the config page plays to let
    /// the owner hear a choice.
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Timbre {
    Sine,
    Bell,
    Wood,
}

impl Timbre {
    /// `(ratio to the fundamental, amplitude)` of each partial.
    fn partials(self) -> &'static [(f64, f64)] {
        match self {
            Timbre::Sine => &[(1.0, 1.0), (2.0, 0.05)],
            Timbre::Bell => &[(1.0, 1.0), (2.0, 0.18), (3.01, 0.08)],
            Timbre::Wood => &[(1.0, 1.0), (4.0, 0.06)],
        }
    }

    /// The decay time `D` of the envelope, in seconds.
    fn decay(self) -> f64 {
        match self {
            Timbre::Sine => 0.55,
            Timbre::Bell => 1.4,
            Timbre::Wood => 0.35,
        }
    }
}

const C4: f64 = 261.63;
const G4: f64 = 392.0;
const A4: f64 = 440.0;
const C5: f64 = 523.25;
const D5: f64 = 587.33;
const E5: f64 = 659.25;
const G5: f64 = 783.99;
const A5: f64 = 880.0;

/// The timbre of a sound (`None` has none: it never reaches a synthesiser).
pub(crate) fn timbre(sound: LockSound) -> Timbre {
    match sound {
        LockSound::None | LockSound::A => Timbre::Sine,
        LockSound::B | LockSound::C => Timbre::Bell,
        LockSound::D | LockSound::E => Timbre::Wood,
    }
}

/// `(frequency in Hz, onset in seconds)` of each note of a motif, in order.
pub(crate) fn notes(sound: LockSound, motif: Motif) -> Vec<(f64, f64)> {
    match motif {
        Motif::Preview => {
            let lock = notes(sound, Motif::Lock);
            let shift = duration_secs(sound, Motif::Lock);
            let unlock = notes(sound, Motif::Unlock).into_iter().map(|(f, o)| (f, o + shift));
            lock.into_iter().chain(unlock).collect()
        }
        Motif::Lock => match sound {
            LockSound::None => vec![],
            LockSound::A => vec![(E5, 0.0), (C5, 0.16)],
            LockSound::B => vec![(G5, 0.0), (E5, 0.13), (C5, 0.26)],
            LockSound::C => vec![(A4, 0.0)],
            LockSound::D => vec![(G4, 0.0), (C4, 0.15)],
            LockSound::E => vec![(A5, 0.0), (G5, 0.09), (E5, 0.18), (D5, 0.27)],
        },
        Motif::Unlock => match sound {
            LockSound::None => vec![],
            LockSound::A => vec![(C5, 0.0), (E5, 0.16)],
            LockSound::B => vec![(C5, 0.0), (E5, 0.13), (G5, 0.26)],
            LockSound::C => vec![(A4, 0.0), (E5, 0.2)],
            LockSound::D => vec![(C4, 0.0), (G4, 0.15)],
            LockSound::E => vec![(D5, 0.0), (E5, 0.09), (G5, 0.18), (A5, 0.27)],
        },
    }
}

/// Last onset + 0.03 + the decay: the reference's oscillator stop time.
fn duration_secs(sound: LockSound, motif: Motif) -> f64 {
    let last = notes(sound, motif).iter().map(|&(_, o)| o).fold(None, |m: Option<f64>, o| {
        Some(m.map_or(o, |m| m.max(o)))
    });
    match last {
        Some(last) => last + 0.03 + timbre(sound).decay(),
        None => 0.0,
    }
}

/// How long the motif lasts, tail included (zero for `None`).
pub fn duration(sound: LockSound, motif: Motif) -> Duration {
    Duration::from_secs_f64(duration_secs(sound, motif))
}

/// At most 6 decimals, trailing zeros and a dangling point stripped, never
/// `-0`: the shape the measured strings have (`0`, `0.16`, `1318.5`).
fn num(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// One note: the envelope times the sum of the timbre's partials.
fn voice(freq: f64, onset: f64, timbre: Timbre) -> String {
    let decay = timbre.decay();
    let (a, b, c) = (onset, onset + 0.015, onset + 0.015 + decay);
    let k1 = 2200f64.ln() / 0.015;
    let k2 = (0.0001f64 / 0.22).ln() / decay;
    let env = format!(
        "if(between(t,{a},{b}),0.0001*exp({k1}*(t-{a})),if(between(t,{b},{c}),0.22*exp({k2}*(t-{b})),0))",
        a = num(a),
        b = num(b),
        c = num(c),
        k1 = num(k1),
        k2 = num(k2),
    );
    let terms: Vec<String> = timbre
        .partials()
        .iter()
        .map(|&(ratio, amp)| {
            let x = format!("2*PI*{}*(t-{})", num(freq * ratio), num(a));
            // The wood fundamental is a triangle, exactly: ffmpeg's evaluator
            // accepts `asin` (checked on FFmpeg 6.1.1), so no harmonic
            // approximation is needed.
            let osc = if timbre == Timbre::Wood && ratio == 1.0 {
                format!("(2/PI)*asin(sin({x}))")
            } else {
                format!("sin({x})")
            };
            format!("{}*{osc}", num(amp))
        })
        .collect();
    format!("{env}*({})", terms.join("+"))
}

/// One channel's `aevalsrc` expression; `None` for silence.
pub fn expression(sound: LockSound, motif: Motif) -> Option<String> {
    if sound == LockSound::None {
        return None;
    }
    let t = timbre(sound);
    let voices: Vec<String> = notes(sound, motif).into_iter().map(|(f, o)| voice(f, o, t)).collect();
    Some(voices.join("+"))
}

/// The one-off file mpv loads when it is idle (always 48 kHz, one channel).
pub fn idle_uri(sound: LockSound, motif: Motif) -> Option<String> {
    let e = expression(sound, motif)?;
    Some(format!(
        "av://lavfi:aevalsrc=exprs='{e}':s=48000:d={}",
        num(duration_secs(sound, motif))
    ))
}

/// The filter that mixes the chime into the running audio chain (appended
/// after the equalizer with a comma, alone when the equalizer is off). The
/// expression is repeated once per channel of the stream being played.
pub fn mix_filter(sound: LockSound, motif: Motif, sample_rate: u32, channels: u32) -> Option<String> {
    chime_graph("anull", sound, motif, sample_rate, channels)
}

/// [`mix_filter`] with the music silenced (`volume=0`): the filter of a
/// chime played while paused, for which mpv is unpaused the chime's length
/// (`player::Player::sound_while_paused`). Measured by the paused spike
/// (mpv 0.37 / FFmpeg 6.1, 48 and 44.1 kHz, equalizer on and off): the music
/// at gain 0 is silent bit-exactly and the chime comes out sample for sample
/// as the pure `aevalsrc` render. `amix duration=first` still follows the
/// music, so the chime sounds only while mpv decodes the paused file — hence
/// the end-of-file guard of `player::ChimeRoute::Paused`.
#[cfg_attr(not(test), expect(dead_code, reason = "the core plays the paused chime in the next task"))]
pub fn mix_filter_over_silence(sound: LockSound, motif: Motif, sample_rate: u32, channels: u32) -> Option<String> {
    chime_graph("volume=volume=0", sound, motif, sample_rate, channels)
}

/// The `@chime` entry; `music` is the filter the playing stream goes through
/// before it is mixed with the chime.
fn chime_graph(music: &str, sound: LockSound, motif: Motif, sample_rate: u32, channels: u32) -> Option<String> {
    let e = expression(sound, motif)?;
    let exprs = vec![e; channels.max(1) as usize].join("|");
    Some(format!(
        "@chime:lavfi=[{music}[m];aevalsrc=exprs='{exprs}':s={sample_rate}:d={}[c];[m][c]amix=inputs=2:duration=first:dropout_transition=0:normalize=0]",
        num(duration_secs(sound, motif))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const ALL: [LockSound; 5] = [LockSound::A, LockSound::B, LockSound::C, LockSound::D, LockSound::E];

    #[test]
    fn the_table_is_the_reference() {
        let c4 = 261.63;
        let (g4, a4, c5, d5, e5, g5, a5) = (392.0, 440.0, 523.25, 587.33, 659.25, 783.99, 880.0);
        type Notes = Vec<(f64, f64)>;
        let table: [(LockSound, Notes, Notes); 5] = [
            (LockSound::A, vec![(e5, 0.0), (c5, 0.16)], vec![(c5, 0.0), (e5, 0.16)]),
            (
                LockSound::B,
                vec![(g5, 0.0), (e5, 0.13), (c5, 0.26)],
                vec![(c5, 0.0), (e5, 0.13), (g5, 0.26)],
            ),
            (LockSound::C, vec![(a4, 0.0)], vec![(a4, 0.0), (e5, 0.2)]),
            (LockSound::D, vec![(g4, 0.0), (c4, 0.15)], vec![(c4, 0.0), (g4, 0.15)]),
            (
                LockSound::E,
                vec![(a5, 0.0), (g5, 0.09), (e5, 0.18), (d5, 0.27)],
                vec![(d5, 0.0), (e5, 0.09), (g5, 0.18), (a5, 0.27)],
            ),
        ];
        for (sound, lock, unlock) in table {
            assert_eq!(notes(sound, Motif::Lock), lock, "{sound:?} lock");
            assert_eq!(notes(sound, Motif::Unlock), unlock, "{sound:?} unlock");
        }
        assert_eq!(timbre(LockSound::A), Timbre::Sine);
        assert_eq!(timbre(LockSound::B), Timbre::Bell);
        assert_eq!(timbre(LockSound::C), Timbre::Bell);
        assert_eq!(timbre(LockSound::D), Timbre::Wood);
        assert_eq!(timbre(LockSound::E), Timbre::Wood);
    }

    fn secs(d: Duration) -> f64 {
        d.as_secs_f64()
    }

    #[test]
    fn durations_follow_the_last_onset_and_the_decay() {
        let close = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} != {b}");
        close(secs(duration(LockSound::A, Motif::Lock)), 0.16 + 0.03 + 0.55);
        close(secs(duration(LockSound::C, Motif::Unlock)), 0.2 + 0.03 + 1.4);
        close(secs(duration(LockSound::E, Motif::Lock)), 0.27 + 0.03 + 0.35);
        close(secs(duration(LockSound::A, Motif::Preview)), 0.74 + 0.74);
        for motif in [Motif::Lock, Motif::Unlock, Motif::Preview] {
            assert_eq!(duration(LockSound::None, motif), Duration::ZERO);
        }
    }

    #[test]
    fn the_preview_plays_the_unlock_motif_after_the_lock_one() {
        let preview = notes(LockSound::C, Motif::Preview);
        let lock_end = secs(duration(LockSound::C, Motif::Lock));
        assert_eq!(preview.len(), 3);
        assert_eq!(preview[0], (440.0, 0.0));
        assert_eq!(preview[1], (440.0, lock_end));
        assert_eq!(preview[2], (659.25, 0.2 + lock_end));
    }

    #[test]
    fn none_produces_nothing() {
        for motif in [Motif::Lock, Motif::Unlock, Motif::Preview] {
            assert_eq!(expression(LockSound::None, motif), None);
            assert_eq!(idle_uri(LockSound::None, motif), None);
            assert_eq!(mix_filter(LockSound::None, motif, 48000, 2), None);
        }
    }

    #[test]
    fn the_strings_are_the_measured_ones() {
        // Verbatim from the spike (Task 1): A lock, then the head of C unlock.
        let a_lock = "if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,0.565),0.22*exp(-13.993114*(t-0.015)),0))*(1*sin(2*PI*659.25*(t-0))+0.05*sin(2*PI*1318.5*(t-0)))+if(between(t,0.16,0.175),0.0001*exp(513.080843*(t-0.16)),if(between(t,0.175,0.725),0.22*exp(-13.993114*(t-0.175)),0))*(1*sin(2*PI*523.25*(t-0.16))+0.05*sin(2*PI*1046.5*(t-0.16)))";
        assert_eq!(expression(LockSound::A, Motif::Lock).unwrap(), a_lock);
        assert_eq!(
            idle_uri(LockSound::A, Motif::Lock).unwrap(),
            format!("av://lavfi:aevalsrc=exprs='{a_lock}':s=48000:d=0.74")
        );
        assert_eq!(
            mix_filter(LockSound::A, Motif::Lock, 48000, 2).unwrap(),
            format!(
                "@chime:lavfi=[anull[m];aevalsrc=exprs='{a_lock}|{a_lock}':s=48000:d=0.74[c];[m][c]amix=inputs=2:duration=first:dropout_transition=0:normalize=0]"
            )
        );
        let c_unlock = expression(LockSound::C, Motif::Unlock).unwrap();
        assert!(c_unlock.starts_with("if(between(t,0,0.015),0.0001*exp(513.080843*(t-0)),if(between(t,0.015,1.415),0.22*exp(-5.497295*(t-0.015)),0))*(1*sin(2*PI*440*(t-0))+0.18*sin(2*PI*880*(t-0))+0.08*sin(2*PI*1324.4*(t-0)))+"));
        assert!(c_unlock.ends_with("0.08*sin(2*PI*1984.3425*(t-0.2)))"));
        assert!(idle_uri(LockSound::C, Motif::Unlock).unwrap().ends_with(":s=48000:d=1.63"));
        // The wood fundamental is the exact triangle.
        assert!(expression(LockSound::D, Motif::Lock).unwrap().contains("1*(2/PI)*asin(sin(2*PI*392*(t-0)))"));
    }

    #[test]
    fn mix_filter_repeats_the_expression_per_channel_and_names_the_rate() {
        let e = expression(LockSound::A, Motif::Lock).unwrap();
        let stereo = mix_filter(LockSound::A, Motif::Lock, 44100, 2).unwrap();
        assert!(stereo.starts_with("@chime:lavfi=["));
        assert!(stereo.contains(":s=44100:"));
        assert!(stereo.contains(&format!("exprs='{e}|{e}'")));
        assert_eq!(stereo.matches(&e).count(), 2);

        let mono = mix_filter(LockSound::A, Motif::Lock, 44100, 1).unwrap();
        assert_eq!(mono.matches(&e).count(), 1);
        assert!(mono.contains(&format!("exprs='{e}'")));
        assert!(!mono.contains('|'), "no channel separator with one channel");
    }

    #[test]
    fn the_paused_graph_is_the_measured_one() {
        // Verbatim from the paused spike (9b, Task 1, kf48: 20/20 clean).
        let a_lock = expression(LockSound::A, Motif::Lock).unwrap();
        assert_eq!(
            mix_filter_over_silence(LockSound::A, Motif::Lock, 48000, 2).unwrap(),
            format!(
                "@chime:lavfi=[volume=volume=0[m];aevalsrc=exprs='{a_lock}|{a_lock}':s=48000:d=0.74[c];[m][c]amix=inputs=2:duration=first:dropout_transition=0:normalize=0]"
            )
        );
        let mono = mix_filter_over_silence(LockSound::C, Motif::Unlock, 44100, 1).unwrap();
        assert!(mono.contains(":s=44100:d=1.63[c]"), "{mono}");
        assert!(!mono.contains('|'));
        for motif in [Motif::Lock, Motif::Unlock, Motif::Preview] {
            assert_eq!(mix_filter_over_silence(LockSound::None, motif, 48000, 2), None);
        }
    }

    /// Through ffmpeg, the graph inside the `@chime:lavfi=[…]` entry, fed a
    /// 1 kHz sine at 0.3: the music branch comes out silent (−60 dB re the
    /// sine, the spike's threshold), the chime is there, and after the chime
    /// the output is exact silence. The same graph with the music at gain 1
    /// is the positive control: the detector sees the sine.
    #[test]
    fn the_paused_graph_silences_the_music() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg is not installed: the render test is skipped");
            return;
        }
        let run = |filter: &str| -> Vec<f32> {
            let graph = filter.strip_prefix("@chime:lavfi=[").and_then(|g| g.strip_suffix(']')).expect("an @chime entry");
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out.raw");
            let status = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "aevalsrc=exprs='0.3*sin(2*PI*1000*t)':s=48000:d=2"])
                .args(["-filter_complex"])
                .arg(format!("[0:a]{graph}"))
                .args(["-ac", "1", "-ar", "48000", "-f", "f32le"])
                .arg(&out)
                .status()
                .unwrap();
            assert!(status.success(), "ffmpeg refused {graph}");
            let bytes = std::fs::read(&out).unwrap();
            bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
        };
        let paused = mix_filter_over_silence(LockSound::A, Motif::Lock, 48000, 1).unwrap();
        let x = run(&paused);
        assert!(x.len() >= 95_000, "the output follows the music's length: {}", x.len());
        let leak = goertzel(&x, 48000.0, 0.0, 1.9, 1000.0);
        assert!(leak <= 0.3e-3, "the music leaks at 1 kHz: {leak}");
        let peak = x[..35_520].iter().fold(0f32, |p, s| p.max(s.abs()));
        assert!(peak > 0.1, "the chime is missing: peak {peak}");
        assert!(x[36_000..].iter().all(|s| *s == 0.0), "after the chime the output is not exact silence");
        // Positive control: the music branch at gain 1 is seen.
        let control = run(&paused.replace("volume=volume=0[m]", "anull[m]"));
        assert!(goertzel(&control, 48000.0, 0.8, 1.9, 1000.0) > 0.25, "the detector does not see the music");
    }

    /// Renders `aevalsrc` to mono f32 samples at 48 kHz, or `None` without ffmpeg.
    fn render(sound: LockSound, motif: Motif) -> Option<Vec<f32>> {
        let e = expression(sound, motif)?;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("chime.raw");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc=exprs='{e}':s=48000:d={}", num(duration_secs(sound, motif))))
            .args(["-f", "f32le"])
            .arg(&out)
            .status()
            .ok()?;
        assert!(status.success(), "ffmpeg refused the expression of {sound:?} {motif:?}");
        let bytes = std::fs::read(&out).unwrap();
        Some(bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
    }

    /// Hann-windowed single-bin DFT magnitude (amplitude units) of
    /// `samples[from..to]` at `freq`.
    fn goertzel(samples: &[f32], rate: f64, from: f64, to: f64, freq: f64) -> f64 {
        let (i0, i1) = ((from * rate) as usize, (to * rate) as usize);
        let n = (i1 - i0) as f64;
        let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
        for (k, &x) in samples[i0..i1].iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / n).cos();
            let ph = 2.0 * std::f64::consts::PI * freq * (i0 + k) as f64 / rate;
            re += w * x as f64 * ph.cos();
            im += w * x as f64 * ph.sin();
            wsum += w;
        }
        2.0 * (re * re + im * im).sqrt() / wsum
    }

    #[test]
    fn the_expression_renders_the_reference_sound() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg is not installed: the render test is skipped");
            return;
        }
        for (sound, motif) in [(LockSound::A, Motif::Lock), (LockSound::C, Motif::Unlock), (LockSound::E, Motif::Lock)]
        {
            let x = render(sound, motif).expect("ffmpeg runs");
            let label = format!("{sound:?} {motif:?}");
            let peak = x.iter().fold(0f32, |p, s| p.max(s.abs()));
            assert!((0.18..=0.5).contains(&peak), "{label}: peak {peak}");
            assert!(x[0].abs() < 0.001, "{label}: the first sample is {} (a click)", x[0]);
            let tail = &x[x.len() - 480..];
            assert!(tail.iter().all(|s| s.abs() < 0.001), "{label}: the last 10 ms are not silent");
            let motif_notes = notes(sound, motif);
            for &(f, o) in &motif_notes {
                let own = goertzel(&x, 48000.0, o + 0.02, o + 0.07, f);
                for &(other, _) in motif_notes.iter().filter(|&&(g, _)| g != f) {
                    let leak = goertzel(&x, 48000.0, o + 0.02, o + 0.07, other);
                    assert!(own > 2.0 * leak, "{label}: {f} Hz at {o}s is {own}, {other} Hz is {leak}");
                }
                assert!(own > 0.05, "{label}: the note {f} Hz at {o}s is missing ({own})");
            }
        }
    }

    #[test]
    fn every_motif_of_every_sound_is_accepted_by_ffmpeg() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        for sound in ALL {
            for motif in [Motif::Lock, Motif::Unlock, Motif::Preview] {
                assert!(render(sound, motif).is_some());
            }
        }
    }
}
