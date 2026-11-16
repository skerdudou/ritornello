//! The equalizer as the core applies it: the fewest mpv commands per change.
//!
//! Measured on a real mpv (the prototype of the equalizer's design):
//! `af-command` on one band does not click; rewriting the chain
//! (`set_property af`) during playback clicks clearly; every new file
//! rebuilds the chain from the recorded `af`, losing whatever `af-command`
//! set; and an idle mpv answers "error running command" to every
//! `af-command` (there is no live chain), while setting `af` works idle too.
//! A loading mpv refuses them the same way, from `start-file` until
//! `playback-restart` (0.1 to 3 s, a radio connecting).
//! The preamp is a 10 Hz high shelf (see `crate::equalizer::af_chain`),
//! which jumps without a click: it is commanded at once, like a band.
//!
//! Hence:
//! - toggling `enabled` rewrites the chain (empty when disabled) — the only
//!   rewrite during playback that changes the equalizer's filters, a brief
//!   cut the design accepts. A lock chime also rewrites the chain twice
//!   during playback (`lock_sound.rs`), but adds and removes only its own
//!   filter after the equalizer's, which it writes exactly as `af` holds
//!   them (`write_held_chain`, `eq_plain_written`) — measured without a
//!   click;
//! - otherwise, while enabled and the chain is live (`eq_live`, from
//!   `playback-restart` to the next file start or idle), one `af-command` per
//!   band whose gain differs from what mpv holds, and one for the preamp
//!   when it moved;
//! - **never a boost before its attenuation**: when the preamp goes down
//!   (more compensation) it is sent first, then the band cuts, then the band
//!   boosts; otherwise the cuts, the boosts, then the preamp. Either way the
//!   signal is never louder than the compensation allows in between;
//! - **the first player error stops the change**: a failed attenuation or
//!   cut followed by boosts is sustained clipping, and every further command
//!   after an IPC timeout would stall the main loop again. What was not sent
//!   stays pending (`eq_preamp_sent`, `eq_bands_sent` track only what mpv
//!   acknowledged); the next change or the next file catches up;
//! - while disabled, or while the chain is not live, nothing but the setting
//!   moves: there is no filter to command. At `playback-restart`
//!   (`equalizer_live`) what changed while the file loaded is sent, by the
//!   same rules; after a seek nothing differs and nothing is sent;
//! - at each file start (`reapply_equalizer`), the whole chain is rewritten
//!   — **only when mpv's `af` no longer holds the current setting**
//!   (`eq_chain_written`). mpv rebuilds the chain from `af` anyway, so a
//!   rewrite is only needed after a change; and since the event crosses the
//!   core's busy loop, a rewrite can land after the audio started, where it
//!   clicks. A failed write leaves `af` unknown, and the next file retries.

use super::*;

impl<P: Player> Core<P> {
    /// Applies a new equalizer setting and persists it. A player error is
    /// returned **after** the setting is saved: it is not lost, and the next
    /// file start rewrites the chain from it anyway.
    pub async fn set_equalizer(&mut self, new: crate::equalizer::EqualizerSettings) -> Result<()> {
        let old_enabled = self.equalizer.enabled;
        self.equalizer = new;
        let result = if old_enabled != self.equalizer.enabled {
            self.write_chain().await
        } else if self.equalizer.enabled && self.eq_live {
            self.command_change().await
        } else {
            // Disabled, or no live chain: no filter to command. A file
            // loading catches up at `playback-restart`; otherwise the next
            // file start sees `af` differ from the setting and rewrites it.
            Ok(())
        };
        self.persist();
        result
    }

    /// The live part of `set_equalizer`: preamp and bands, in the order that
    /// never boosts before attenuating, stopping at the first error.
    async fn command_change(&mut self) -> Result<()> {
        let target = crate::equalizer::effective_preamp_db(&self.equalizer);
        let lowering = target < self.eq_preamp_sent;
        if lowering {
            self.send_preamp(target).await?;
        }
        self.send_bands().await?;
        if !lowering {
            self.send_preamp(target).await?;
        }
        Ok(())
    }

    /// A file is starting: mpv rebuilds the chain from `af`, dropping what
    /// `af-command` set. Rewrite `af` only if it no longer holds the current
    /// setting; either way the chain now carries the setting, so whatever a
    /// failed command left pending is moot. mpv refuses `af-command` until
    /// `playback-restart` (`equalizer_live`). A lock chime still mixed in
    /// makes `af` differ from the setting, so the new file gets the plain
    /// chain and the chime's removal is dropped with it (`write_chain_with`).
    pub async fn reapply_equalizer(&mut self) {
        self.eq_live = false;
        let chain = crate::equalizer::af_chain(&self.equalizer);
        if self.eq_chain_written.as_deref() == Some(chain.as_str()) {
            self.eq_preamp_sent = crate::equalizer::effective_preamp_db(&self.equalizer);
            self.eq_bands_sent = self.equalizer.gains;
            return;
        }
        if let Err(e) = self.write_chain().await {
            tracing::warn!("equalizer at file start: {e}");
        }
    }

    /// `playback-restart`: the chain is live and takes `af-command`. Catch up
    /// on whatever changed while it was loading — what mpv holds
    /// (`eq_bands_sent`, `eq_preamp_sent`, set by the chain written at the
    /// file start) against the setting — in the order of any change. After
    /// a seek, nothing differs and nothing is sent.
    pub(super) async fn equalizer_live(&mut self) {
        self.eq_live = true;
        if !self.equalizer.enabled {
            return;
        }
        if let Err(e) = self.command_change().await {
            tracing::warn!("equalizer at playback restart: {e}");
        }
    }

    /// mpv went idle: no live chain any more, so no `af-command` until the
    /// next file's `playback-restart` (`equalizer_live`).
    pub(super) fn equalizer_idle(&mut self) {
        self.eq_live = false;
    }

    /// The audio output changed: mpv may rebuild the chain from `af` when it
    /// reloads the output, dropping what `af-command` set. Rewrite it with
    /// the current setting; the change cuts the sound anyway. A lock chime
    /// mixed in goes with it, equalizer off included (the plain chain is then
    /// empty), rather than sounding on until its own removal on an output
    /// it was not mixed for.
    pub(super) async fn reapply_equalizer_after_output_change(&mut self) {
        if !self.equalizer.enabled && self.lock_sound_mixed.is_none() {
            return;
        }
        if let Err(e) = self.write_chain().await {
            tracing::warn!("equalizer after an audio output change: {e}");
        }
    }

    /// Writes the whole chain of the current setting (empty when disabled)
    /// and records what mpv then holds. A failure leaves `af` unknown.
    async fn write_chain(&mut self) -> Result<()> {
        self.write_chain_with(None).await.map(|_| ())
    }

    /// `write_chain`, with `extra` (a lock chime, `lock_sound.rs`) after the
    /// equalizer's filters; returns the chain written. Any whole-chain write
    /// drops a chime mixed before it, so its pending removal goes too
    /// (`lock_sound_mixed`): the caller mixing a new one sets it again.
    async fn write_chain_with(&mut self, extra: Option<&str>) -> Result<String> {
        let eq = crate::equalizer::af_chain(&self.equalizer);
        let chain = with_extra(&eq, extra);
        self.lock_sound_mixed = None;
        let result = self.player.set_audio_filters(&chain).await;
        self.eq_chain_written = result.is_ok().then(|| chain.clone());
        self.eq_plain_written = result.is_ok().then_some(eq);
        self.eq_preamp_sent = crate::equalizer::effective_preamp_db(&self.equalizer);
        self.eq_bands_sent = self.equalizer.gains;
        result.map(|()| chain)
    }

    /// Writes the plain chain mpv's `af` already holds (`eq_plain_written`)
    /// with `extra` after it — a lock chime — or alone when `extra` is
    /// `None`, which removes a chime; returns the chain written.
    ///
    /// The equalizer's entries are written with the arguments `af` already
    /// carries, not rebuilt from the setting: after a gain moved live
    /// (`af-command`, which never updates `af`) the setting's chain would
    /// change an entry mid-playback, the rewrite that clicks. So the gains
    /// mpv holds live (`eq_preamp_sent`, `eq_bands_sent`) are left as they
    /// are, and the next file start still rewrites the setting's chain,
    /// since `af` differs from it.
    ///
    /// When `af` is unknown (a failed write) there is nothing to build on:
    /// the setting's chain is written whole instead (`write_chain_with`).
    /// Like every whole-chain write, it drops a chime mixed before it
    /// (`lock_sound_mixed`); the caller mixing a new one sets it again.
    pub(super) async fn write_held_chain(&mut self, extra: Option<&str>) -> Result<String> {
        let Some(plain) = self.eq_plain_written.clone() else {
            return self.write_chain_with(extra).await;
        };
        let chain = with_extra(&plain, extra);
        self.lock_sound_mixed = None;
        let result = self.player.set_audio_filters(&chain).await;
        self.eq_chain_written = result.is_ok().then(|| chain.clone());
        if result.is_err() {
            self.eq_plain_written = None;
        }
        result.map(|()| chain)
    }

    /// The preamp, when it differs from what mpv holds.
    async fn send_preamp(&mut self, db: f64) -> Result<()> {
        if db != self.eq_preamp_sent {
            self.player.audio_filter_command(&crate::equalizer::preamp_command(db)).await?;
            self.eq_preamp_sent = db;
        }
        Ok(())
    }

    /// One `af-command` per band whose gain differs from what mpv holds,
    /// **stopping at the first error**: the bands not sent stay pending
    /// (`eq_bands_sent` unchanged for them).
    ///
    /// **The cuts go first, then the boosts.** Overlapping bands add up: a
    /// raise sent while a neighbour still holds its old boost can peak above
    /// both settings (+12 moved from 1 kHz to 500 Hz peaks several dB above
    /// +12 in between). Each band's response grows with its gain at every
    /// frequency, so after the cuts every gain is min(old, new) and during the
    /// boosts each stays at most its final value: the peak never exceeds the
    /// new setting's, which the preamp in place already compensates. A cut
    /// that failed must therefore not be followed by the boosts.
    async fn send_bands(&mut self) -> Result<()> {
        let order = (0..self.eq_bands_sent.len())
            .filter(|&b| self.equalizer.gains[b] < self.eq_bands_sent[b])
            .chain((0..self.eq_bands_sent.len()).filter(|&b| self.equalizer.gains[b] > self.eq_bands_sent[b]))
            .collect::<Vec<_>>();
        for band in order {
            let gain = self.equalizer.gains[band];
            self.player.audio_filter_command(&crate::equalizer::band_command(band, gain)).await?;
            self.eq_bands_sent[band] = gain;
        }
        Ok(())
    }
}

/// `plain` with `extra` after it; either may be empty.
fn with_extra(plain: &str, extra: Option<&str>) -> String {
    match extra {
        Some(extra) if !extra.is_empty() && !plain.is_empty() => format!("{plain},{extra}"),
        Some(extra) if !extra.is_empty() => extra.to_string(),
        _ => plain.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::*;
    use crate::equalizer::{af_chain, effective_preamp_db, preamp_command, EqualizerSettings};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    fn enabled() -> EqualizerSettings {
        EqualizerSettings { enabled: true, ..Default::default() }
    }

    fn with_band(mut s: EqualizerSettings, band: usize, gain: i8) -> EqualizerSettings {
        s.gains[band] = gain;
        s
    }

    /// A core launched with `eq` in `state.json`, its player calls cleared.
    /// mpv is idle: nothing is loaded yet.
    fn launched_with(eq: EqualizerSettings) -> Rig {
        let rig = setup_persisted(PersistedState { equalizer: eq, ..Default::default() });
        rig.1.lock().unwrap().clear();
        rig
    }

    /// `launched_with`, then a file started and its playback restarted: mpv
    /// holds a live chain that `af-command` can reach (an idle or loading
    /// mpv refuses every one).
    async fn playing_with(eq: EqualizerSettings) -> Rig {
        let mut rig = loading_with(eq).await;
        rig.0.handle_event(Event::PlaybackRestarted).await;
        rig.1.lock().unwrap().clear();
        rig
    }

    /// `launched_with`, then a file started but its playback not restarted
    /// yet: mpv refuses `af-command` (measured), as it does idle.
    async fn loading_with(eq: EqualizerSettings) -> Rig {
        let mut rig = setup_persisted(PersistedState { equalizer: eq, ..Default::default() });
        rig.0.handle_event(Event::FileStarting).await;
        rig.1.lock().unwrap().clear();
        rig
    }

    fn calls_of(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    fn band_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.starts_with("af_cmd eq") && !c.starts_with("af_cmd eqpre")).cloned().collect()
    }

    fn preamp_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.starts_with("af_cmd eqpre")).cloned().collect()
    }

    fn preamp_call(db: f64) -> String {
        let c = preamp_command(db);
        format!("af_cmd {} {} {} {}", c.label, c.command, c.arg, c.target)
    }

    fn band_call(band: usize, gain: i8) -> String {
        format!("af_cmd eq{band} g {gain} equalizer")
    }

    fn disk_equalizer(dir: &tempfile::TempDir) -> EqualizerSettings {
        crate::state::load(&dir.path().join("state.json")).equalizer
    }

    #[tokio::test]
    async fn enabling_sets_the_chain() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(EqualizerSettings::default());
        let new = with_band(enabled(), 5, 6);
        core.set_equalizer(new.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test]
    async fn disabling_clears_the_chain() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(with_band(enabled(), 5, 6));
        let new = EqualizerSettings { enabled: false, ..with_band(enabled(), 5, 6) };
        core.set_equalizer(new.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec!["af ".to_string()]);
        assert_eq!(disk_equalizer(&dir), new);
    }

    /// The preamp is a 10 Hz high shelf, which jumps without a click: one
    /// command, at once, no ramp.
    #[tokio::test]
    async fn moving_one_band_sends_one_band_command_and_one_preamp_command() {
        let (mut core, calls, _sc, _rx, dir) = playing_with(enabled()).await;
        let new = with_band(enabled(), 5, 6);
        let target = effective_preamp_db(&new);
        assert!(target < -5.0, "{target}");
        core.set_equalizer(new.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec![preamp_call(target), band_call(5, 6)]);
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test]
    async fn a_boost_waits_for_its_attenuation_and_a_cut_goes_first() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        let boosted = with_band(enabled(), 5, 12);
        core.set_equalizer(boosted.clone()).await.unwrap();
        // The boost only once the attenuation is in place.
        assert_eq!(calls_of(&calls), vec![preamp_call(effective_preamp_db(&boosted)), band_call(5, 12)]);

        // Back down: the cut first, then the preamp rises.
        calls.lock().unwrap().clear();
        core.set_equalizer(enabled()).await.unwrap();
        assert_eq!(calls_of(&calls), vec![band_call(5, 0), preamp_call(0.0)]);
    }

    /// Overlapping bands add up: the cut must reach mpv before the boost, or
    /// the preamp in place is not enough between the two commands. And the
    /// preamp goes first when it goes down, last when it goes up.
    #[tokio::test]
    async fn band_cuts_reach_mpv_before_band_boosts() {
        // Preamp going down (+6 at 1 kHz to +12 at 500 Hz): preamp, cut, boost.
        let to = with_band(enabled(), 4, 12);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(with_band(enabled(), 5, 6)).await;
        core.set_equalizer(to.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec![preamp_call(effective_preamp_db(&to)), band_call(5, 0), band_call(4, 12)]);

        // Preamp going up (+12 at 1 kHz to +6 at 500 Hz): cut, boost, preamp.
        let to = with_band(enabled(), 4, 6);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(with_band(enabled(), 5, 12)).await;
        core.set_equalizer(to.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec![band_call(5, 0), band_call(4, 6), preamp_call(effective_preamp_db(&to))]);

        // Preamp unchanged (manual, 0 both sides): cut, boost, no preamp.
        let manual = |b, g| EqualizerSettings { auto_preamp: false, ..with_band(enabled(), b, g) };
        let (mut core, calls, _sc, _rx, _dir) = playing_with(manual(5, 12)).await;
        core.set_equalizer(manual(4, 12)).await.unwrap();
        assert_eq!(calls_of(&calls), vec![band_call(5, 0), band_call(4, 12)]);
    }

    #[tokio::test]
    async fn disabled_changes_touch_no_player_but_persist() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(EqualizerSettings::default());
        let new = with_band(EqualizerSettings { auto_preamp: false, ..Default::default() }, 2, -7);
        core.set_equalizer(new.clone()).await.unwrap();
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert_eq!(disk_equalizer(&dir), new);
    }

    #[tokio::test]
    async fn auto_preamp_toggle_sends_the_preamp_only() {
        let start = with_band(enabled(), 5, 6);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(start.clone()).await;
        core.set_equalizer(EqualizerSettings { auto_preamp: false, ..start.clone() }).await.unwrap();
        assert_eq!(calls_of(&calls), vec![preamp_call(0.0)]);

        calls.lock().unwrap().clear();
        core.set_equalizer(start.clone()).await.unwrap();
        assert_eq!(calls_of(&calls), vec![preamp_call(effective_preamp_db(&start))]);
    }

    /// A failed attenuation must not be followed by the boost it was there
    /// to compensate; the setting is kept and the next file repairs mpv.
    #[tokio::test]
    async fn a_failed_preamp_command_stops_before_the_boost() {
        let (mut core, calls, _sc, _rx, dir) = playing_with(enabled()).await;
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        let new = with_band(enabled(), 5, 12);
        assert!(core.set_equalizer(new.clone()).await.is_err());
        assert_eq!(calls_of(&calls), vec![preamp_call(effective_preamp_db(&new))]);
        assert_eq!(disk_equalizer(&dir), new);

        core.player.af_cmd_fails.store(false, Ordering::SeqCst);
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
    }

    /// A failed cut followed by boosts is sustained clipping, and each
    /// further command after a 5 s IPC timeout stalls the main loop again.
    #[tokio::test]
    async fn a_failed_band_command_stops_the_change_and_the_next_file_repairs_it() {
        let (mut core, calls, _sc, _rx, dir) = playing_with(with_band(enabled(), 5, 12)).await;
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        // Less boost: the preamp would rise after the bands.
        let new = with_band(enabled(), 4, 6);
        assert!(effective_preamp_db(&new) > effective_preamp_db(&with_band(enabled(), 5, 12)));
        assert!(core.set_equalizer(new.clone()).await.is_err());
        assert_eq!(calls_of(&calls), vec![band_call(5, 0)], "nothing after the failed cut");
        assert_eq!(disk_equalizer(&dir), new);

        core.player.af_cmd_fails.store(false, Ordering::SeqCst);
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
    }

    #[tokio::test]
    async fn a_failed_disable_is_repaired_at_the_next_file_start() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(with_band(enabled(), 5, 6));
        core.player.af_fails.store(true, Ordering::SeqCst);
        let off = EqualizerSettings { enabled: false, ..with_band(enabled(), 5, 6) };
        assert!(core.set_equalizer(off.clone()).await.is_err());
        assert_eq!(disk_equalizer(&dir), off, "saved despite the error");

        core.player.af_fails.store(false, Ordering::SeqCst);
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec!["af ".to_string()]);
        // Repaired: a healthy disabled equalizer costs nothing per file.
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
    }

    #[tokio::test]
    async fn file_start_rewrites_the_chain_with_current_gains() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        let current = with_band(with_band(enabled(), 5, 6), 0, -3);
        core.set_equalizer(current.clone()).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&current))]);
        core.handle_event(Event::PlaybackRestarted).await;

        // A later change sends only the band that moved since.
        calls.lock().unwrap().clear();
        core.set_equalizer(with_band(current.clone(), 0, -2)).await.unwrap();
        assert_eq!(band_calls(&calls), vec![band_call(0, -2)]);

        let (mut core, calls, _sc, _rx, _dir) = launched_with(EqualizerSettings::default());
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
    }

    /// An idle mpv answers "error running command" to every `af-command`
    /// (measured): adjusting the equalizer with nothing playing used to
    /// return that error, logged as a warning into "Recent errors".
    #[tokio::test]
    async fn an_idle_mpv_gets_no_band_command_and_the_next_file_carries_the_change() {
        let (mut core, calls, _sc, _rx, dir) = launched_with(enabled());
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        let new = with_band(enabled(), 5, 6);
        core.set_equalizer(new.clone()).await.unwrap();
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert_eq!(disk_equalizer(&dir), new);

        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
    }

    #[tokio::test]
    async fn playback_idle_ends_the_commands_until_the_next_file() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(enabled()).await;
        core.handle_event(Event::PlaybackIdle).await;
        calls.lock().unwrap().clear();
        let new = with_band(enabled(), 4, 3);
        core.set_equalizer(new.clone()).await.unwrap();
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&new))]);
    }

    /// A rewrite crossing the core's busy loop can land after the audio
    /// started, and then it clicks: mpv's own `af` already holds the right
    /// chain unless the setting moved since it was last written.
    #[tokio::test]
    async fn a_file_start_rewrites_only_a_chain_that_changed() {
        let start = with_band(enabled(), 5, 6);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(start.clone()).await;
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "nothing changed: {:?}", calls_of(&calls));

        let next = with_band(start.clone(), 0, -3);
        core.set_equalizer(next.clone()).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(calls_of(&calls), vec![format!("af {}", af_chain(&next))]);
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "written once: {:?}", calls_of(&calls));

        // A change undone within the file: mpv's `af` still holds the
        // setting, and the next file needs nothing.
        core.set_equalizer(start.clone()).await.unwrap();
        core.set_equalizer(next.clone()).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        core.handle_event(Event::PlaybackRestarted).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        // …and a later change sends only what moved from that setting.
        core.set_equalizer(start.clone()).await.unwrap();
        assert_eq!(band_calls(&calls), vec![band_call(0, 0)]);
    }

    /// Commands that failed left mpv's live chain behind the setting, but a
    /// file start whose `af` already holds the setting rebuilds the chain at
    /// the setting: it counts as sent, and the same setting again sends
    /// nothing.
    #[tokio::test]
    async fn a_file_start_without_rewrite_counts_the_setting_as_sent() {
        let start = with_band(enabled(), 5, 6);
        let (mut core, calls, _sc, _rx, _dir) = playing_with(start.clone()).await;
        // Out (the preamp lands, the boost fails), and back (the preamp
        // fails): mpv's live preamp is left at the abandoned setting's.
        *core.player.af_cmd_fails_for.lock().unwrap() = Some("eq5".into());
        assert!(core.set_equalizer(with_band(enabled(), 5, 12)).await.is_err());
        assert_eq!(preamp_calls(&calls).len(), 1, "{:?}", calls_of(&calls));
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        assert!(core.set_equalizer(start.clone()).await.is_err());
        core.player.af_cmd_fails.store(false, Ordering::SeqCst);
        *core.player.af_cmd_fails_for.lock().unwrap() = None;
        calls.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        core.handle_event(Event::PlaybackRestarted).await;
        assert!(calls_of(&calls).is_empty(), "af already holds the setting: {:?}", calls_of(&calls));

        core.set_equalizer(start.clone()).await.unwrap();
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
    }

    /// Between `start-file` and `playback-restart` (up to 3 s, a radio
    /// connecting) mpv refuses `af-command` (measured). A change made then
    /// used to log a warning and be heard only from the next file — hours
    /// later on a radio — while the page showed it. It now waits, silently,
    /// and reaches mpv at `playback-restart`, in the usual order.
    #[tokio::test]
    async fn a_change_while_loading_is_sent_at_playback_restart() {
        // Preamp going down: preamp first, then the boost.
        let (mut core, calls, _sc, _rx, dir) = loading_with(enabled()).await;
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        let new = with_band(enabled(), 5, 12);
        core.set_equalizer(new.clone()).await.expect("no command, so no error");
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert_eq!(disk_equalizer(&dir), new);
        core.player.af_cmd_fails.store(false, Ordering::SeqCst);
        core.handle_event(Event::PlaybackRestarted).await;
        assert_eq!(calls_of(&calls), vec![preamp_call(effective_preamp_db(&new)), band_call(5, 12)]);

        // Preamp going up: the cut, the boost, then the preamp.
        let (mut core, calls, _sc, _rx, _dir) = loading_with(with_band(enabled(), 5, 12)).await;
        let new = with_band(enabled(), 4, 6);
        core.set_equalizer(new.clone()).await.unwrap();
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        core.handle_event(Event::PlaybackRestarted).await;
        assert_eq!(calls_of(&calls), vec![band_call(5, 0), band_call(4, 6), preamp_call(effective_preamp_db(&new))]);

        // Caught up: a change after that sends only what moves.
        calls.lock().unwrap().clear();
        core.set_equalizer(with_band(new.clone(), 0, -3)).await.unwrap();
        assert_eq!(band_calls(&calls), vec![band_call(0, -3)]);
    }

    /// The case that happens: something plays, the next file (a radio
    /// connecting) starts, and the equalizer moves before its playback
    /// restarts. The file start must end the live window it inherited.
    #[tokio::test]
    async fn a_change_while_the_next_file_loads_waits_for_its_playback_restart() {
        let (mut core, calls, _sc, _rx, dir) = playing_with(enabled()).await;
        core.handle_event(Event::FileStarting).await;
        assert!(calls_of(&calls).is_empty(), "af holds the setting: {:?}", calls_of(&calls));
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        let new = with_band(enabled(), 5, 12);
        core.set_equalizer(new.clone()).await.expect("no command while loading, so no error");
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
        assert_eq!(disk_equalizer(&dir), new);

        core.player.af_cmd_fails.store(false, Ordering::SeqCst);
        core.handle_event(Event::PlaybackRestarted).await;
        assert_eq!(calls_of(&calls), vec![preamp_call(effective_preamp_db(&new)), band_call(5, 12)]);
    }

    /// `playback-restart` also follows every seek: with nothing changed, it
    /// costs no command.
    #[tokio::test]
    async fn a_playback_restart_with_nothing_changed_sends_nothing() {
        let (mut core, calls, _sc, _rx, _dir) = playing_with(with_band(enabled(), 5, 6)).await;
        core.handle_event(Event::PlaybackRestarted).await;
        assert!(calls_of(&calls).is_empty(), "seek: {:?}", calls_of(&calls));

        core.handle_event(Event::FileStarting).await;
        core.handle_event(Event::PlaybackRestarted).await;
        assert!(calls_of(&calls).is_empty(), "next file: {:?}", calls_of(&calls));
    }

    /// Disabled, mpv holds no equalizer chain: nothing to catch up on.
    #[tokio::test]
    async fn a_disabled_equalizer_catches_up_on_nothing() {
        let (mut core, calls, _sc, _rx, _dir) = loading_with(EqualizerSettings::default()).await;
        core.set_equalizer(with_band(EqualizerSettings::default(), 5, 6)).await.unwrap();
        core.handle_event(Event::PlaybackRestarted).await;
        assert!(calls_of(&calls).is_empty(), "{:?}", calls_of(&calls));
    }

    /// The catch-up obeys the rule of every change: the first error stops
    /// it, before the boost its attenuation was there to compensate.
    #[tokio::test]
    async fn a_failed_catch_up_stops_at_the_first_error() {
        let (mut core, calls, _sc, _rx, _dir) = loading_with(enabled()).await;
        core.set_equalizer(with_band(enabled(), 5, 12)).await.unwrap();
        core.player.af_cmd_fails.store(true, Ordering::SeqCst);
        core.handle_event(Event::PlaybackRestarted).await;
        assert_eq!(calls_of(&calls).len(), 1, "{:?}", calls_of(&calls));
    }
}
