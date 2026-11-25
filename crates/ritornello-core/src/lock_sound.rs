//! The sounds played when the physical remote is locked and unlocked, and the
//! files that make mpv play them.
//!
//! The sounds are rendered in pure Rust from a closed form (the renderer is
//! compiled for the tests and for `regenerate_the_embedded_assets` only: the
//! device reads the committed files): the ones of the
//! web reference (A4 = 440 Hz, a sum of decaying partials per note), with the
//! reference's Web Audio envelope: a 15 ms exponential attack from 0.0001 to
//! [`PEAK`], then an exponential decay to 0.0001 over the timbre's decay time.
//! The lock and unlock motifs are committed as 48 kHz stereo 16-bit WAV files
//! under `crates/ritornello-core/assets/lock-sounds/` and embedded in the
//! binary; `embedded_assets_are_the_rendered_ones` refuses them once the
//! formula changes without them being regenerated. Nothing is synthesised on
//! the device: ffmpeg's `aevalsrc` took 1.45 s (mono) and 1.87 s (stereo) to
//! render the 0.74 s A lock on the Pi, slower than real time.
//!
//! [`write_files`] writes every sound and motif, the preview included (the
//! lock motif then the unlock one, concatenated), into a directory mpv can
//! read, once at start. Two ways to play one from there:
//! while something plays, [`mix_filter`] mixes the file into the audio chain
//! with `amix` (`normalize=0`, the main stream untouched); while mpv is idle,
//! [`idle_uri`] is loaded as a one-off file. A third, for a paused file: the
//! file is mixed over the music silenced ([`mix_filter_over_silence`]) and mpv
//! is unpaused for its length. The mix was measured without a click with
//! `aevalsrc` as its source; reading the file with `amovie` is checked by
//! ffmpeg and mpv in the tests, not yet on the Pi.

use serde::{Deserialize, Serialize};
use std::path::Path;
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

/// The loudness of the chime (`Settings::lock_sound_volume`): one of four
/// steps, persisted as its number, 1 to 4. Level 2 is the reference: the
/// files as rendered, peak 0.525, with no gain step at all.
///
/// The gains are applied by mpv's `volume` filter on the chime branch, not by
/// re-encoding the files. Headroom: the chime is summed with the music by
/// `amix normalize=0`, so the sum clips where the music's level plus the
/// chime's peak at that instant exceeds 1.0. The motifs' own peaks are above
/// the envelope's [`PEAK`] where notes overlap — up to 0.81 (B lock, a bell
/// whose notes ring into each other) at level 2 — so the loud steps are
/// bounded by the chime itself: level 4 (×1.2, +1.6 dB) keeps every motif
/// under full scale on its own
/// (`the_loudest_level_never_clips_a_motif_on_its_own`), where the ×1.5 it
/// had at the previous [`PEAK`] would now clip; level 3 is ×1.1 (+0.8 dB),
/// level 1 ×0.5 (-6 dB). Louder than level 4 is not possible without a
/// limiter on the chime. The owner chose loudness over the music's headroom:
/// the previous levels (an envelope peak of 0.35 at level 2) were too quiet
/// next to the music at every step, and level 2 now sounds as level 4 did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum LockVolume {
    One,
    #[default]
    Two,
    Three,
    Four,
}

impl LockVolume {
    /// The gain of the step against the reference, as the `volume` filter
    /// takes it. `None` at level 2: no step, the graph is the reference's.
    fn gain(self) -> Option<&'static str> {
        match self {
            LockVolume::One => Some("0.5"),
            LockVolume::Two => None,
            LockVolume::Three => Some("1.1"),
            LockVolume::Four => Some("1.2"),
        }
    }
}

impl TryFrom<u8> for LockVolume {
    type Error = String;

    fn try_from(level: u8) -> Result<Self, String> {
        match level {
            1 => Ok(LockVolume::One),
            2 => Ok(LockVolume::Two),
            3 => Ok(LockVolume::Three),
            4 => Ok(LockVolume::Four),
            other => Err(format!("no lock sound level {other}")),
        }
    }
}

impl From<LockVolume> for u8 {
    fn from(level: LockVolume) -> u8 {
        match level {
            LockVolume::One => 1,
            LockVolume::Two => 2,
            LockVolume::Three => 3,
            LockVolume::Four => 4,
        }
    }
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
    #[cfg(test)]
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

/// The envelope's peak. The web reference uses 0.22, which the owner found
/// too quiet next to the music (the mix adds the chime to the stream without
/// attenuating it). 0.35 (about +4 dB) was found still too quiet at every
/// level, and the owner asked for the loudest level to become the default:
/// 0.525 is 0.35 × 1.5, so level 2 now sounds as level 4 did, and every
/// level moved by the same +3.5 dB. Headroom: the mix is float until
/// mpv's output conversion, so the sum only clips where a loud master peaks
/// at the same instant as the chime's attack, and the chime decays from
/// there; the tests keep the peak within 0.4..=0.75 (notes overlap, so a
/// motif peaks above [`PEAK`]: C unlock reaches 0.69), and every motif at
/// the loudest level under full scale on its own.
#[cfg(test)]
const PEAK: f64 = 0.525;

/// The attack of every note, in seconds: from 0.0001 to [`PEAK`].
#[cfg(test)]
const ATTACK: f64 = 0.015;

/// The rate of the rendered files (and the rate the mix is written for when
/// the stream runs at it).
const RATE: u32 = 48_000;

/// The channels of the rendered files. Each channel carries the same sound.
const CHANNELS: u16 = 2;

/// Bytes of the RIFF header `wav_of_pcm` writes, before the samples.
const WAV_HEADER: usize = 44;

/// The sub-directory of the runtime directory where mpv reads the files.
pub const DIR_NAME: &str = "lock-sounds";

const C4: f64 = 261.63;
const G4: f64 = 392.0;
const A4: f64 = 440.0;
const C5: f64 = 523.25;
const D5: f64 = 587.33;
const E5: f64 = 659.25;
const G5: f64 = 783.99;
const A5: f64 = 880.0;

const SOUNDS: [LockSound; 5] = [LockSound::A, LockSound::B, LockSound::C, LockSound::D, LockSound::E];
const MOTIFS: [Motif; 3] = [Motif::Lock, Motif::Unlock, Motif::Preview];

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

/// One note at time `t`: the envelope times the sum of the timbre's
/// partials, zero outside the note. The fundamental of the wood timbre is an
/// exact triangle, `(2/PI) * asin(sin(x))`, the other partials are sines.
#[cfg(test)]
fn voice_at(t: f64, freq: f64, onset: f64, timbre: Timbre) -> f64 {
    let decay = timbre.decay();
    let (a, b, c) = (onset, onset + ATTACK, onset + ATTACK + decay);
    let k1 = (PEAK / 0.0001f64).ln() / ATTACK;
    let k2 = (0.0001f64 / PEAK).ln() / decay;
    let env = if a <= t && t <= b {
        0.0001 * (k1 * (t - a)).exp()
    } else if b <= t && t <= c {
        PEAK * (k2 * (t - b)).exp()
    } else {
        return 0.0;
    };
    let body: f64 = timbre
        .partials()
        .iter()
        .map(|&(ratio, amp)| {
            let x = 2.0 * std::f64::consts::PI * freq * ratio * (t - a);
            let osc = if timbre == Timbre::Wood && ratio == 1.0 {
                (2.0 / std::f64::consts::PI) * x.sin().asin()
            } else {
                x.sin()
            };
            amp * osc
        })
        .sum();
    env * body
}

/// The mono samples of a motif at [`RATE`], `round(duration * RATE)` of them
/// (none for `None`).
#[cfg(test)]
pub(crate) fn samples(sound: LockSound, motif: Motif) -> Vec<f64> {
    let n = (duration_secs(sound, motif) * f64::from(RATE)).round() as usize;
    let t = timbre(sound);
    let voices = notes(sound, motif);
    (0..n)
        .map(|i| {
            let time = i as f64 / f64::from(RATE);
            voices.iter().map(|&(f, o)| voice_at(time, f, o, t)).sum()
        })
        .collect()
}

/// 16-bit little-endian stereo frames of `samples`, the same value on both
/// channels.
#[cfg(test)]
fn pcm16(samples: &[f64]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&x| {
            let b = ((x.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes();
            [b[0], b[1], b[0], b[1]]
        })
        .collect()
}

/// A canonical 44-byte RIFF/WAVE header (PCM, stereo, [`RATE`], 16 bit) in
/// front of `pcm`.
fn wav_of_pcm(pcm: &[u8]) -> Vec<u8> {
    let block_align = CHANNELS * 2;
    let mut out = Vec::with_capacity(WAV_HEADER + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// The samples of a WAV file written by [`wav_of_pcm`].
fn pcm_of(wav: &[u8]) -> &[u8] {
    &wav[WAV_HEADER..]
}

/// The file name of a sound's motif, in the directory of [`write_files`].
fn file_name(sound: LockSound, motif: Motif) -> String {
    let stem = match sound {
        LockSound::None => "none",
        LockSound::A => "a",
        LockSound::B => "b",
        LockSound::C => "c",
        LockSound::D => "d",
        LockSound::E => "e",
    };
    let motif = match motif {
        Motif::Lock => "lock",
        Motif::Unlock => "unlock",
        Motif::Preview => "preview",
    };
    format!("{stem}-{motif}.wav")
}

/// The committed WAV of a lock or unlock motif (`assets/lock-sounds`). The
/// preview is not committed: it is their concatenation (`file_bytes`).
fn embedded(sound: LockSound, motif: Motif) -> Option<&'static [u8]> {
    Some(match (sound, motif) {
        (LockSound::A, Motif::Lock) => include_bytes!("../assets/lock-sounds/a-lock.wav").as_slice(),
        (LockSound::A, Motif::Unlock) => include_bytes!("../assets/lock-sounds/a-unlock.wav").as_slice(),
        (LockSound::B, Motif::Lock) => include_bytes!("../assets/lock-sounds/b-lock.wav").as_slice(),
        (LockSound::B, Motif::Unlock) => include_bytes!("../assets/lock-sounds/b-unlock.wav").as_slice(),
        (LockSound::C, Motif::Lock) => include_bytes!("../assets/lock-sounds/c-lock.wav").as_slice(),
        (LockSound::C, Motif::Unlock) => include_bytes!("../assets/lock-sounds/c-unlock.wav").as_slice(),
        (LockSound::D, Motif::Lock) => include_bytes!("../assets/lock-sounds/d-lock.wav").as_slice(),
        (LockSound::D, Motif::Unlock) => include_bytes!("../assets/lock-sounds/d-unlock.wav").as_slice(),
        (LockSound::E, Motif::Lock) => include_bytes!("../assets/lock-sounds/e-lock.wav").as_slice(),
        (LockSound::E, Motif::Unlock) => include_bytes!("../assets/lock-sounds/e-unlock.wav").as_slice(),
        _ => return None,
    })
}

/// The whole WAV file of a sound's motif, as [`write_files`] writes it.
/// `None` for `LockSound::None`.
fn file_bytes(sound: LockSound, motif: Motif) -> Option<Vec<u8>> {
    if sound == LockSound::None {
        return None;
    }
    match motif {
        Motif::Preview => {
            let lock = embedded(sound, Motif::Lock)?;
            let unlock = embedded(sound, Motif::Unlock)?;
            Some(wav_of_pcm(&[pcm_of(lock), pcm_of(unlock)].concat()))
        }
        Motif::Lock | Motif::Unlock => embedded(sound, motif).map(<[u8]>::to_vec),
    }
}

/// Renders a sound's motif to a WAV file, as the committed assets are made
/// (`regenerate_the_embedded_assets`). `None` for `LockSound::None`.
#[cfg(test)]
pub(crate) fn wav(sound: LockSound, motif: Motif) -> Option<Vec<u8>> {
    if sound == LockSound::None {
        return None;
    }
    Some(wav_of_pcm(&pcm16(&samples(sound, motif))))
}

/// Writes every sound's motifs into `dir` (created if missing), where mpv
/// reads them: called once, when mpv starts.
pub fn write_files(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for sound in SOUNDS {
        for motif in MOTIFS {
            if let Some(bytes) = file_bytes(sound, motif) {
                std::fs::write(dir.join(file_name(sound, motif)), bytes)?;
            }
        }
    }
    Ok(())
}

/// The file mpv loads when it is idle (`dir` as given to [`write_files`]).
pub fn idle_uri(dir: &Path, sound: LockSound, motif: Motif) -> Option<String> {
    if sound == LockSound::None {
        return None;
    }
    Some(dir.join(file_name(sound, motif)).to_string_lossy().into_owned())
}

/// The file-local `af` of an idle load at `volume`: `volume=G`, or `None` at
/// the reference level (the load is then exactly the one measured). It
/// replaces the chain for that one file only, so the equalizer does not
/// shape the chime while it is loaded alone.
pub fn idle_af(volume: LockVolume) -> Option<String> {
    volume.gain().map(|gain| format!("volume={gain}"))
}

/// The filter that mixes the chime into the running audio chain (appended
/// after the equalizer with a comma, alone when the equalizer is off). The
/// file is read at the stream's `sample_rate` and `channels`.
pub fn mix_filter(
    dir: &Path,
    sound: LockSound,
    motif: Motif,
    sample_rate: u32,
    channels: u32,
    volume: LockVolume,
) -> Option<String> {
    chime_graph("anull", dir, sound, motif, sample_rate, channels, volume)
}

/// [`mix_filter`] with the music silenced (`volume=0`): the filter of a
/// chime played while paused, for which mpv is unpaused the chime's length
/// (`player::Player::sound_while_paused`). `amix duration=first` still
/// follows the music, so the chime sounds only while mpv decodes the paused
/// file — hence the end-of-file guard of `player::ChimeRoute::Paused`.
pub fn mix_filter_over_silence(
    dir: &Path,
    sound: LockSound,
    motif: Motif,
    sample_rate: u32,
    channels: u32,
    volume: LockVolume,
) -> Option<String> {
    chime_graph("volume=volume=0", dir, sound, motif, sample_rate, channels, volume)
}

/// The `@chime` entry: the file read by `amovie` (looped once, so it plays
/// once), brought to the stream's rate and, for any channel count but two,
/// to that many channels (`pan`, every channel the left one: the file is the
/// same on both). `music` is the filter the playing stream goes through
/// before it is mixed with the chime. The `volume` step comes after the
/// channel shaping, on the chime only; at level 2 there is none, so that
/// graph is byte for byte the reference's.
#[allow(clippy::too_many_arguments)] // the chain's inputs, as the callers name them
fn chime_graph(
    music: &str,
    dir: &Path,
    sound: LockSound,
    motif: Motif,
    sample_rate: u32,
    channels: u32,
    volume: LockVolume,
) -> Option<String> {
    if sound == LockSound::None {
        return None;
    }
    let path = dir.join(file_name(sound, motif));
    let channels = channels.max(1);
    let shape = if channels == u32::from(CHANNELS) {
        format!("aresample={sample_rate}")
    } else {
        let outs: Vec<String> = (0..channels).map(|i| format!("c{i}=c0")).collect();
        format!("aresample={sample_rate},pan={channels}c|{}", outs.join("|"))
    };
    let step = volume.gain().map(|gain| format!(",volume={gain}")).unwrap_or_default();
    Some(format!(
        "@chime:lavfi=[{music}[m];amovie=filename='{}':loop=1,{shape}{step}[c];[m][c]amix=inputs=2:duration=first:dropout_transition=0:normalize=0]",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const ALL: [LockSound; 5] = [LockSound::A, LockSound::B, LockSound::C, LockSound::D, LockSound::E];

    /// Level 2 is the reference: its graph is the one the device was always
    /// given (no `volume` step), and the other levels differ from it by one
    /// step on the chime's branch alone, never on the music's `[m]`.
    #[test]
    fn the_reference_level_is_the_graph_it_always_was_and_the_others_step_the_chime() {
        let at = |v| mix_filter(files(), LockSound::A, Motif::Lock, 48000, 2, v).unwrap();
        let reference = at(LockVolume::Two);
        assert_eq!(
            reference,
            "@chime:lavfi=[anull[m];amovie=filename='/run/ritornello/lock-sounds/a-lock.wav':loop=1,aresample=48000[c];[m][c]amix=inputs=2:duration=first:dropout_transition=0:normalize=0]"
        );
        for (level, gain) in [(LockVolume::One, "0.5"), (LockVolume::Three, "1.1"), (LockVolume::Four, "1.2")] {
            let stepped = at(level);
            assert_eq!(
                stepped,
                reference.replace("aresample=48000[c]", &format!("aresample=48000,volume={gain}[c]")),
                "{level:?}"
            );
            assert!(stepped.starts_with("@chime:lavfi=[anull[m];"), "the music is untouched: {stepped}");
        }
    }

    /// Over silence the step rides on the chime too, the music still silenced.
    #[test]
    fn the_step_is_on_the_chime_over_silence_as_well() {
        let s = mix_filter_over_silence(files(), LockSound::B, Motif::Preview, 44100, 1, LockVolume::Four).unwrap();
        assert!(s.starts_with("@chime:lavfi=[volume=volume=0[m];"), "{s}");
        assert!(s.contains("aresample=44100,pan=1c|c0=c0,volume=1.2[c]"), "{s}");
    }

    /// The idle load carries the same gain as a file-local `af`, and nothing
    /// at the reference level (the measured load, unchanged).
    #[test]
    fn the_idle_load_carries_the_step_only_off_the_reference() {
        assert_eq!(idle_af(LockVolume::Two), None);
        assert_eq!(idle_af(LockVolume::One).as_deref(), Some("volume=0.5"));
        assert_eq!(idle_af(LockVolume::Three).as_deref(), Some("volume=1.1"));
        assert_eq!(idle_af(LockVolume::Four).as_deref(), Some("volume=1.2"));
    }

    /// The persisted number is 1 to 4, and nothing else is a level.
    #[test]
    fn a_level_is_one_to_four_and_nothing_else() {
        for n in 1u8..=4 {
            assert_eq!(u8::from(LockVolume::try_from(n).unwrap()), n);
        }
        assert!(LockVolume::try_from(0).is_err());
        assert!(LockVolume::try_from(5).is_err());
        assert_eq!(LockVolume::default(), LockVolume::Two);
    }

    /// The directory the strings name, in the tests that only compare them.
    fn files() -> &'static Path {
        Path::new("/run/ritornello/lock-sounds")
    }

    fn ffmpeg_missing() -> bool {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg is not installed: the test is skipped");
            return true;
        }
        false
    }

    /// The `aevalsrc` expression the sounds were first synthesised with, kept
    /// as the oracle the Rust renderer is checked against through ffmpeg. Not
    /// shipped: the device only reads the committed files.
    mod oracle {
        use super::super::*;

        fn num(v: f64) -> String {
            let s = format!("{v:.6}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            if s.is_empty() || s == "-0" {
                "0".to_string()
            } else {
                s.to_string()
            }
        }

        fn voice(freq: f64, onset: f64, timbre: Timbre) -> String {
            let decay = timbre.decay();
            let (a, b, c) = (onset, onset + ATTACK, onset + ATTACK + decay);
            let k1 = (PEAK / 0.0001f64).ln() / ATTACK;
            let k2 = (0.0001f64 / PEAK).ln() / decay;
            let env = format!(
                "if(between(t,{a},{b}),0.0001*exp({k1}*(t-{a})),if(between(t,{b},{c}),{peak}*exp({k2}*(t-{b})),0))",
                a = num(a),
                b = num(b),
                c = num(c),
                k1 = num(k1),
                k2 = num(k2),
                peak = num(PEAK),
            );
            let terms: Vec<String> = timbre
                .partials()
                .iter()
                .map(|&(ratio, amp)| {
                    let x = format!("2*PI*{}*(t-{})", num(freq * ratio), num(a));
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

        /// One channel's expression, and the duration string `aevalsrc` takes.
        pub fn expression(sound: LockSound, motif: Motif) -> (String, String) {
            let t = timbre(sound);
            let voices: Vec<String> = notes(sound, motif).into_iter().map(|(f, o)| voice(f, o, t)).collect();
            (voices.join("+"), num(duration_secs(sound, motif)))
        }
    }

    /// Renders the oracle through ffmpeg to mono f32 at [`RATE`].
    fn ffmpeg_oracle(sound: LockSound, motif: Motif) -> Vec<f32> {
        let (e, d) = oracle::expression(sound, motif);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("oracle.raw");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc=exprs='{e}':s={RATE}:d={d}"))
            .args(["-f", "f32le"])
            .arg(&out)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg refused the oracle of {sound:?} {motif:?}");
        bytes_to_f32(&std::fs::read(&out).unwrap())
    }

    fn bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
        bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
    }

    /// Runs `graph` (an `@chime:lavfi=[…]` entry) over a 1 kHz sine of
    /// `seconds` on `channels` channels at `rate`, through ffmpeg; returns the
    /// interleaved f32 output, as many channels as the stream.
    fn run_graph(graph: &str, rate: u32, channels: u32, seconds: f64) -> Vec<f32> {
        let inner = graph.strip_prefix("@chime:lavfi=[").and_then(|g| g.strip_suffix(']')).expect("an @chime entry");
        let exprs = vec!["0.3*sin(2*PI*1000*t)"; channels as usize].join("|");
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.raw");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc=exprs='{exprs}':s={rate}:d={seconds}"))
            .args(["-filter_complex"])
            .arg(format!("[0:a]{inner}"))
            .args(["-ac", &channels.to_string(), "-ar", &rate.to_string(), "-f", "f32le"])
            .arg(&out)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg refused {graph}");
        bytes_to_f32(&std::fs::read(&out).unwrap())
    }

    /// The first channel of an interleaved output.
    fn first_channel(x: &[f32], channels: u32) -> Vec<f32> {
        x.iter().step_by(channels as usize).copied().collect()
    }

    fn secs(d: Duration) -> f64 {
        d.as_secs_f64()
    }

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
            assert_eq!(idle_uri(files(), LockSound::None, motif), None);
            assert_eq!(mix_filter(files(), LockSound::None, motif, 48000, 2, LockVolume::Two), None);
            assert_eq!(mix_filter_over_silence(files(), LockSound::None, motif, 48000, 2, LockVolume::Two), None);
            assert_eq!(wav(LockSound::None, motif), None);
            assert_eq!(samples(LockSound::None, motif), Vec::<f64>::new());
        }
    }

    #[test]
    fn the_idle_load_is_the_file_of_the_motif() {
        assert_eq!(idle_uri(files(), LockSound::A, Motif::Lock).unwrap(), "/run/ritornello/lock-sounds/a-lock.wav");
        assert_eq!(idle_uri(files(), LockSound::E, Motif::Preview).unwrap(), "/run/ritornello/lock-sounds/e-preview.wav");
    }

    /// The strings name the file, the rate and the channel count; the file
    /// itself is read by `amovie`, not synthesised (the old `aevalsrc` strings
    /// were replaced deliberately: rendering them on the Pi was slower than
    /// real time).
    #[test]
    fn the_mix_filter_reads_the_file_at_the_stream_rate_and_channels() {
        assert_eq!(
            mix_filter(files(), LockSound::A, Motif::Lock, 48000, 2, LockVolume::Two).unwrap(),
            "@chime:lavfi=[anull[m];amovie=filename='/run/ritornello/lock-sounds/a-lock.wav':loop=1,aresample=48000[c];[m][c]amix=inputs=2:duration=first:dropout_transition=0:normalize=0]"
        );
        let mono = mix_filter(files(), LockSound::A, Motif::Lock, 44100, 1, LockVolume::Two).unwrap();
        assert!(mono.contains("aresample=44100,pan=1c|c0=c0[c]"), "{mono}");
        let six = mix_filter(files(), LockSound::C, Motif::Unlock, 48000, 6, LockVolume::Two).unwrap();
        assert!(six.contains("amovie=filename='/run/ritornello/lock-sounds/c-unlock.wav'"), "{six}");
        assert!(six.contains("pan=6c|c0=c0|c1=c0|c2=c0|c3=c0|c4=c0|c5=c0[c]"), "{six}");
    }

    /// The paused chime: the music silenced, the same file, the same mix.
    /// Changed deliberately with the file reading (it was the `aevalsrc`
    /// string before).
    #[test]
    fn the_paused_graph_is_the_measured_one() {
        assert_eq!(
            mix_filter_over_silence(files(), LockSound::A, Motif::Lock, 48000, 2, LockVolume::Two).unwrap(),
            "@chime:lavfi=[volume=volume=0[m];amovie=filename='/run/ritornello/lock-sounds/a-lock.wav':loop=1,aresample=48000[c];[m][c]amix=inputs=2:duration=first:dropout_transition=0:normalize=0]"
        );
        let mono = mix_filter_over_silence(files(), LockSound::C, Motif::Unlock, 44100, 1, LockVolume::Two).unwrap();
        assert!(mono.contains("c-unlock.wav':loop=1,aresample=44100,pan=1c|c0=c0[c]"), "{mono}");
        assert!(mono.starts_with("@chime:lavfi=[volume=volume=0[m];"), "{mono}");
    }

    /// Through ffmpeg, the graph inside the `@chime:lavfi=[…]` entry, with the
    /// file written by `write_files`, fed a 1 kHz sine at 0.3: the music branch
    /// comes out silent (−60 dB re the sine, the spike's threshold), the chime
    /// is there, and after the chime the output is exact silence. The same
    /// graph with the music at gain 1 is the positive control.
    #[test]
    fn the_paused_graph_silences_the_music() {
        if ffmpeg_missing() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path()).unwrap();
        let paused = mix_filter_over_silence(dir.path(), LockSound::A, Motif::Lock, 48000, 1, LockVolume::Two).unwrap();
        let x = first_channel(&run_graph(&paused, 48000, 1, 2.0), 1);
        assert!(x.len() >= 95_000, "the output follows the music's length: {}", x.len());
        let leak = goertzel(&x, 48000.0, 0.0, 1.9, 1000.0);
        assert!(leak <= 0.3e-3, "the music leaks at 1 kHz: {leak}");
        let peak = x[..35_520].iter().fold(0f32, |p, s| p.max(s.abs()));
        assert!(peak > 0.1, "the chime is missing: peak {peak}");
        assert!(x[36_000..].iter().all(|s| *s == 0.0), "after the chime the output is not exact silence");
        // Positive control: the music branch at gain 1 is seen.
        let control = first_channel(&run_graph(&paused.replace("volume=volume=0[m]", "anull[m]"), 48000, 1, 2.0), 1);
        assert!(goertzel(&control, 48000.0, 0.8, 1.9, 1000.0) > 0.25, "the detector does not see the music");
    }

    /// The mix graph is accepted by ffmpeg for every shape the stream can
    /// have (rate, channels), and the chime is in the output: the file is read
    /// and brought to the stream's rate and channel count.
    #[test]
    fn the_mix_graph_is_accepted_at_every_stream_shape() {
        if ffmpeg_missing() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path()).unwrap();
        for (rate, channels) in [(48000, 2), (44100, 2), (44100, 1), (48000, 6)] {
            let graph = mix_filter(dir.path(), LockSound::A, Motif::Lock, rate, channels, LockVolume::Two).unwrap();
            let out = run_graph(&graph, rate, channels, 1.0);
            let frames = out.len() / channels as usize;
            assert_eq!(frames, rate as usize, "{rate} Hz, {channels} channels: {frames} frames");
            let left = first_channel(&out, channels);
            let chime_peak = left[..(rate as usize * 7 / 10)].iter().fold(0f32, |p, s| p.max(s.abs()));
            assert!(chime_peak > 0.2, "{rate} Hz, {channels} channels: no chime ({chime_peak})");
        }
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

    /// The committed files are the rendered sounds: a change to the formula
    /// without regenerating them fails here. To regenerate, run
    /// `cargo test -p ritornello-core regenerate_the_embedded_assets -- --ignored`.
    #[test]
    fn embedded_assets_are_the_rendered_ones() {
        for sound in ALL {
            for motif in [Motif::Lock, Motif::Unlock] {
                let committed = embedded(sound, motif).expect("a committed file");
                let rendered = wav(sound, motif).unwrap();
                assert!(
                    committed == rendered.as_slice(),
                    "{sound:?} {motif:?}: the committed file differs from the formula; regenerate the assets"
                );
            }
        }
    }

    /// Writes the committed files from the formula. Run after a change to
    /// the formula, then review and commit the diff of `assets/lock-sounds`.
    #[test]
    #[ignore = "regenerates crates/ritornello-core/assets/lock-sounds"]
    fn regenerate_the_embedded_assets() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/lock-sounds"));
        std::fs::create_dir_all(dir).unwrap();
        for sound in ALL {
            for motif in [Motif::Lock, Motif::Unlock] {
                std::fs::write(dir.join(file_name(sound, motif)), wav(sound, motif).unwrap()).unwrap();
            }
        }
    }

    #[test]
    fn the_files_are_48_khz_stereo_16_bit_and_the_preview_is_lock_then_unlock() {
        for sound in ALL {
            let lock = embedded(sound, Motif::Lock).unwrap();
            let header = &lock[..WAV_HEADER];
            assert_eq!(&header[0..4], b"RIFF");
            assert_eq!(&header[8..16], b"WAVEfmt ");
            assert_eq!(u16::from_le_bytes([header[22], header[23]]), 2, "channels");
            assert_eq!(u32::from_le_bytes(header[24..28].try_into().unwrap()), 48000, "rate");
            assert_eq!(u16::from_le_bytes([header[34], header[35]]), 16, "bits");
            assert_eq!(&header[36..40], b"data");
            assert_eq!(u32::from_le_bytes(header[40..44].try_into().unwrap()) as usize, lock.len() - WAV_HEADER);

            // The preview's samples are the lock's then the unlock's, bit for
            // bit: no voice of one motif sounds inside the other's span.
            let preview = file_bytes(sound, Motif::Preview).unwrap();
            let unlock = embedded(sound, Motif::Unlock).unwrap();
            assert!(
                pcm_of(&preview) == [pcm_of(lock), pcm_of(unlock)].concat().as_slice(),
                "{sound:?}: the preview is not the lock then the unlock"
            );
            let joined: Vec<f64> = [samples(sound, Motif::Lock), samples(sound, Motif::Unlock)].concat();
            let formula = samples(sound, Motif::Preview);
            assert_eq!(formula.len(), joined.len(), "{sound:?}");
            let (at, worst) = formula
                .iter()
                .zip(&joined)
                .map(|(a, b)| (a - b).abs())
                .enumerate()
                .fold((0, 0.0), |acc, (i, d)| if d > acc.1 { (i, d) } else { acc });
            // Not bit-exact at the note ends: a note's envelope is 0.0001 on
            // its last sample, inclusive, and the shift of the unlock motif
            // (a float sum) can put that sample on either side of the bound.
            // One such sample differs by about 1e-4, a few LSB of 16 bit.
            assert!(
                worst < 2e-4,
                "{sound:?}: the closed form differs from the concatenation by {worst} at sample {at} of {} ({} against {})",
                formula.len(),
                formula[at],
                joined[at]
            );
        }
    }

    #[test]
    fn the_committed_sounds_have_the_reference_size() {
        let total: usize = ALL
            .iter()
            .flat_map(|&s| [Motif::Lock, Motif::Unlock].map(|m| embedded(s, m).unwrap().len()))
            .sum();
        // 10.28 s of stereo 48 kHz 16-bit, plus a header per file.
        assert_eq!(total, (10.28 * 192_000.0_f64).round() as usize + 10 * WAV_HEADER);
    }

    #[test]
    fn write_files_puts_every_motif_where_mpv_is_told() {
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path()).unwrap();
        for sound in ALL {
            for motif in MOTIFS {
                let path = dir.path().join(file_name(sound, motif));
                let on_disk = std::fs::read(&path).unwrap();
                assert!(on_disk == file_bytes(sound, motif).unwrap().as_slice(), "{}", path.display());
            }
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 15);
    }

    /// Every motif of every sound, at the loudest level's gain, stays under
    /// full scale on its own: a chime that clipped before any music is added
    /// would crackle on every lock. Level 4 was ×1.5 before the envelope's
    /// peak went from 0.35 to 0.525; kept at ×1.5, C unlock would reach 1.04.
    #[test]
    fn the_loudest_level_never_clips_a_motif_on_its_own() {
        let gain: f64 = LockVolume::Four.gain().unwrap().parse().unwrap();
        for sound in ALL {
            for motif in MOTIFS {
                let peak = samples(sound, motif).iter().fold(0f64, |p, s| p.max(s.abs()));
                assert!(peak * gain < 1.0, "{sound:?} {motif:?}: {peak} × {gain} clips");
            }
        }
    }

    /// The renderer's own guards: the peak, a click at the first sample, the
    /// silent tail, and each note at its onset audible above the others.
    #[test]
    fn the_rendered_sound_keeps_its_peak_and_its_notes() {
        for (sound, motif) in [(LockSound::A, Motif::Lock), (LockSound::C, Motif::Unlock), (LockSound::E, Motif::Lock)] {
            let x: Vec<f32> = samples(sound, motif).iter().map(|&v| v as f32).collect();
            let label = format!("{sound:?} {motif:?}");
            let peak = x.iter().fold(0f32, |p, s| p.max(s.abs()));
            assert!((0.4..=0.75).contains(&peak), "{label}: peak {peak}");
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

    /// The Rust renderer against the `aevalsrc` it replaces, through ffmpeg:
    /// the same closed form, the same samples, up to the float rounding of
    /// the expression's six-decimal constants. Covers the wood triangle.
    #[test]
    fn the_render_is_the_aevalsrc_of_the_reference() {
        if ffmpeg_missing() {
            return;
        }
        for (sound, motif) in [
            (LockSound::A, Motif::Lock),
            (LockSound::B, Motif::Unlock),
            (LockSound::C, Motif::Unlock),
            (LockSound::D, Motif::Lock),
            (LockSound::E, Motif::Lock),
        ] {
            let oracle = ffmpeg_oracle(sound, motif);
            let ours = samples(sound, motif);
            let label = format!("{sound:?} {motif:?}");
            assert_eq!(ours.len(), oracle.len(), "{label}: length");
            let worst = ours.iter().zip(&oracle).map(|(a, b)| (a - f64::from(*b)).abs()).fold(0.0, f64::max);
            assert!(worst < 1e-4, "{label}: the render differs from the oracle by {worst}");
        }
    }

    #[test]
    fn every_motif_file_is_decoded_by_ffmpeg() {
        if ffmpeg_missing() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path()).unwrap();
        for sound in ALL {
            for motif in MOTIFS {
                let path = dir.path().join(file_name(sound, motif));
                let out = dir.path().join("decoded.raw");
                let status = Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-i"])
                    .arg(&path)
                    .args(["-f", "f32le"])
                    .arg(&out)
                    .status()
                    .unwrap();
                assert!(status.success(), "ffmpeg refused {}", path.display());
                let frames = std::fs::metadata(&out).unwrap().len() as usize / 4 / 2;
                let expected = (duration_secs(sound, motif) * f64::from(RATE)).round() as usize;
                assert_eq!(frames, expected, "{}", path.display());
            }
        }
    }
}
