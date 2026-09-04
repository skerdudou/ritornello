use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// What the device does with the active source when the process starts.
/// Read once, at launch, by `Core::startup`.
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

/// How a date is written on this device.
///
/// **A closed choice and not a free pattern.** The owner asked for two separate
/// settings, date and time; a `strftime`-style pattern would be more flexible
/// and would give a blank display at the first faulty pattern, on a living-room
/// device where nobody reads a log. Three shapes cover what countries actually
/// write, and each is tamper-proof.
///
/// The **separator** belongs to the shape and is not one more setting:
/// `2026-12-31` with slashes is read nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateFormat {
    /// `31/12/2026` — the French shape, and the default.
    #[default]
    DayMonthYear,
    /// `2026-12-31` — ISO 8601, the one that sorts.
    YearMonthDay,
    /// `12/31/2026` — the North American shape.
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
    /// abandon guard in `apply_command`, or by a source's transient
    /// message in `handle_source_update` — and both of those explicitly
    /// clear the offset too, so it never survives behind a display that
    /// no longer shows it.
    pub tens_window_ms: u32,
    /// Step of the "seek forward" / "seek backward" keys, in seconds.
    ///
    /// Adjustable where the volume step is fixed, because the right value
    /// depends on what one listens to: ten seconds to catch a sentence again,
    /// a minute to cross a movement.
    pub seek_step_s: u32,

    // ---- How the device writes a date and a time ---------------------------
    //
    // **Two settings and not one**, at the owner's request, and the separation
    // is defensible: the order of a date's components and the 12/24 h choice
    // do not vary together from one country to another. One English speaker
    // may want `2026-12-31` and 24 h, another `12/31/2026` and 12 h.
    //
    // They serve two audiences, and that is why they live here rather than in
    // each consumer: the time on the display in standby, and the date of the
    // "last errors" on the System page.
    //
    // **No timezone setting**, and that is deliberate: the display runs *on*
    // the device, so its clock is already the right one; the web page formats
    // on the browser side, hence in the timezone of whoever is looking — which
    // is right for a phone that travels. One more setting could only
    // contradict one of the two.
    /// The order of a date's components. See `DateFormat`.
    pub date_format: DateFormat,
    /// 24-hour time (`13:05`) rather than 12-hour (`1:05 PM`).
    pub clock_24h: bool,

    // ---- Covers: what is kept, then what is read, then what is produced ----
    //
    // Eight settings in three groups, and the UI's two cards carry the
    // distinction rather than decorating it.
    //
    // `cover_cache_budget_mio` comes first, in a card of its own ("what is
    // kept in memory"): it bounds what the appliance *holds*, and is the only
    // one of the eight that says nothing about any single cover.
    //
    // `cover_download_max_mio` and `cover_source_max_mio` bound what the core
    // agrees to bring in — the first for the internet only, the second for
    // every source. Both live outside the switch: `cover_source_max_mio` is
    // the only protection left when re-encoding is disabled, and the cheapest
    // of all, since it is judged on the file size without reading a byte of
    // its content.
    //
    // The remaining five are the switch (`cover_rendition`) and the four
    // settings that only describe the **rendition** — what the core makes to
    // push onto a socket. Switch unchecked, none of those four means anything:
    // the source leaves as is.
    /// Memory allowed to the covers the device keeps at hand, in mebibytes.
    ///
    /// **The budget, not a count** — and that inversion is the point. A number
    /// of entries said nothing about memory: one had to multiply it by two
    /// other settings to learn what it cost, and the product could reach
    /// absurd values without anything objecting. Here the figure the user
    /// reads *is* the memory, and the number of covers becomes a consequence
    /// the page estimates for them.
    ///
    /// What it bounds: the bytes of covers downloaded from the internet, and
    /// the retained thumbnails. A local cover — a `folder.jpg` on a share or a
    /// picture embedded in the audio file — costs only a path, so it weighs on
    /// this budget solely through its thumbnail.
    pub cover_cache_budget_mio: u32,

    /// Cap on a cover **downloaded from the internet**, in mebibytes.
    ///
    /// Was a hard-coded constant, and its value was chosen against a measured
    /// case: the bare `front` of the Cover Art Archive weighs 2,670,705 bytes
    /// where `front-500` returns 75,249. Two mebibytes rules the first out and
    /// lets the second through.
    ///
    /// **The counterpart of `cover_source_max_mio`, and the pair is
    /// deliberate.** One bounds what the device accepts to *download* from a
    /// third party on the internet; the other what it accepts to *read* from a
    /// disk or a share it trusts. Two origins, two questions, two answers —
    /// rather than one figure that would have to mean both.
    pub cover_download_max_mio: u32,

    /// Cap of the **source** cover, in mebibytes.
    ///
    /// Always active, re-encoding or not. Bounded by
    /// `ritornello_proto::COVER_MAX_BYTES` (20 MiB) by validation, and that is
    /// structural: this constant is a **protocol** promise — it tells the
    /// plugins the maximum they can receive, and the MPD plugin sizes its own
    /// bounds on it without being able to consult the core's settings. This
    /// field can therefore only lower it.
    pub cover_source_max_mio: u32,

    /// Re-encode covers before pushing them, or push the source as is?
    ///
    /// Unchecked, the core no longer decodes anything: it pushes the original
    /// bytes, and the memory peak of a publication becomes that of the source
    /// image again (close to 72 MiB for a 20 MiB cover, between the bytes,
    /// their base64 and the JSON line) instead of ~1.8 MiB for a thumbnail. It
    /// is a defensible choice — a display that wants full resolution, a
    /// machine that has the RAM — but it must be made knowingly.
    pub cover_rendition: bool,

    /// Longest side of the thumbnail, in pixels. The ratio is preserved.
    pub cover_max_edge_px: u32,

    /// JPEG quality of the thumbnail, from 1 to 100.
    ///
    /// Only applies to JPEG: a cover with an alpha channel is re-encoded as
    /// PNG, losslessly, because flattening its transparency onto a guessed
    /// background would be a visual choice the device has no business making.
    pub cover_jpeg_quality: u8,

    /// Weight under which a cover is pushed **without being re-encoded**, in
    /// kibibytes.
    ///
    /// The low threshold, and only that: an image already lighter than this —
    /// and no wider than `cover_max_edge_px` — is served as it is, because
    /// re-encoding it would not meaningfully lighten it. It is also the rule a
    /// **supplied** thumbnail is accepted on (`cover_thumb` in the protocol).
    ///
    /// It replaced a single number that was compared to *two* different things:
    /// the weight of the original as this threshold, and the weight of the
    /// produced thumbnail as a net that dropped the cover. Two opposite
    /// intentions on one knob, which is why no value ever felt right. The net
    /// now lives in `Rendition::net`, is derived from the edge, and is not a
    /// setting at all.
    ///
    /// Getting it wrong is cheap: at worst an image is re-encoded that needed
    /// not be, or one slightly heavier is pushed. That is what distinguishes
    /// it from the net it used to share a number with.
    pub cover_passthrough_max_ko: u32,

    /// Cap of **pixels** to decode, in megapixels.
    ///
    /// The anti-decompression-bomb guard, and the only one that really counts:
    /// the dimensions are read in the header **before any allocation**, and a
    /// file that exceeds them is refused without being decoded. A 200 KiB PNG
    /// can announce 30000 × 30000 pixels, i.e. 3.6 GiB of decoded buffer — the
    /// file size says nothing about the decoding cost, and that is exactly
    /// what this setting bounds where `cover_source_max_mio` can do nothing.
    ///
    /// Its label in the UI carries the `w × h × 4` computation, because the
    /// useful value is not the number of megapixels but the mebibytes they
    /// cost: 16 Mpx is 64 MiB of buffer.
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
            // The defaults of the device's country, not neutral defaults: there
            // is no "neutral" date shape, and this one is its owner's.
            date_format: DateFormat::DayMonthYear,
            clock_24h: true,
            // 50 MiB: a floor of about twenty covers even in the worst case
            // (each at the 2 MiB download cap below plus its thumbnail), and
            // closer to a hundred for a NAS library where every cover is
            // local and therefore free — a local cover only ever costs its
            // thumbnail, never the source bytes.
            cover_cache_budget_mio: 50,
            // Rules out the bare `front` of the Cover Art Archive (measured
            // at 2,670,705 bytes) while letting `front-500` (75,249 bytes)
            // through — see the field's doc for the full measurement.
            cover_download_max_mio: 2,
            // The protocol's own cap: by default the core adds no restriction
            // to what the plugins already know how to take.
            cover_source_max_mio: ritornello_proto::COVER_MAX_BYTES as u32 / (1024 * 1024),
            // Enabled by default: on a Pi 2 with 1 GiB shared between mpv, the
            // core, the UI and ten plugins, pushing 20 MiB of raw image is the
            // wrong default even if the device survives it.
            cover_rendition: true,
            // 640 px: beyond what the largest display in the fleet can show,
            // and the web UI only displays the cover at 224 px on its largest
            // breakpoint.
            cover_max_edge_px: 640,
            // 85: the threshold beyond which a JPEG grows without the eye
            // gaining anything, on an image of this size.
            cover_jpeg_quality: 85,
            // 150 KiB, twice a measured 72-73 KiB. Two figures that landed on
            // the same value: what our own encoder produces at 640 px q85
            // (p50 over 78 covers of a real library), and what Cover Art
            // Archive's `front-500` weighs. A supplied thumbnail therefore
            // passes untouched with a factor of two to spare, and the heavy
            // tail — p90 at 316 KiB, which re-encoding divides by 2.4 — is
            // still caught. See `cover.rs`'s measurement benches.
            cover_passthrough_max_ko: 150,
            // 16 Mpx = 64 MiB of decoded buffer. Covers a cover scanned at
            // 4000 × 4000 with margin, and refuses the bomb.
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
    /// Chosen theme preset (opaque name for the core: the preset list lives in
    /// the SPA). Absent = `theme::DEFAULT_THEME`.
    #[serde(default)]
    pub theme: Option<String>,
    /// `"light"` or `"dark"`. Absent = `theme::DEFAULT_MODE`.
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
    fn default_if_file_missing_or_corrupted() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.json");
        assert_eq!(load(&missing), PersistedState::default());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
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
    fn default_is_radio_vol60_without_chosen_output() {
        let d = PersistedState::default();
        assert_eq!(d.active_source, "radio");
        assert_eq!(d.volume, 60);
        assert_eq!(d.audio_device, None);
    }

    #[test]
    fn locale_absent_by_default_and_roundtrip() {
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
    fn theme_and_mode_absent_by_default_and_roundtrip() {
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
    fn an_earlier_state_json_remains_readable() {
        // Backward compatibility: a file written before this version has
        // neither `theme` nor `mode`; it must load without error.
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
    fn default_settings() {
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
    fn a_state_json_without_settings_remains_readable() {
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
    fn settings_roundtrip_and_partial_block() {
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
                cover_cache_budget_mio: 64,
                cover_download_max_mio: 5,
                // Both non-default, same reason: the default is
                // `DayMonthYear` and `true`.
                date_format: DateFormat::YearMonthDay,
                clock_24h: false,
                // Six more non-default values, for the reason written above: a
                // fixture that reused the defaults would not distinguish "the
                // written value survived" from "the default applied".
                // `cover_rendition` at `false` is the case that matters most
                // here — it is the only boolean of the lot, and its default is
                // `true`.
                cover_source_max_mio: 12,
                cover_rendition: false,
                cover_max_edge_px: 800,
                cover_jpeg_quality: 70,
                cover_passthrough_max_ko: 256,
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
    fn persisted_standby_is_false_without_the_key_and_survives_the_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"active_source":"radio","volume":42}"#).unwrap();
        assert!(!load(&path).standby, "without the key, we start awake");

        let st = PersistedState { standby: true, ..Default::default() };
        save(&path, &st).unwrap();
        assert!(load(&path).standby);
    }
}
