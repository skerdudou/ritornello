//! The lock sound: the chime the core plays on every lock change, and the
//! preview the config page asks for (`crate::lock_sound` makes the strings).
//!
//! Where it plays depends on what mpv is doing at that instant
//! (`Player::chime_route`):
//! - **idle** (nothing loaded, standby included): the chime is loaded on its
//!   own (`Player::play_aside`). mpv's gate keeps that load's events away
//!   from the core, so nothing else moves — not `playback`, not `standby`,
//!   no source request, no publication: the device stays asleep;
//! - **playing**: the chime is mixed into the running audio chain, after the
//!   equalizer (`mix_filter`), and the chain without it is written back once
//!   the chime has ended (`lock_sound_mixed`, `expire_lock_sound`). Both
//!   writes build on the chain mpv's `af` holds (`write_held_chain`), not on
//!   the setting: the equalizer's entries keep their arguments even after a
//!   gain moved live, and the live gains stand. Mixing
//!   (`amix`, `normalize=0`) leaves the main stream untouched: measured
//!   without a click on mpv 0.37 / FFmpeg 6.1;
//! - **paused**: mpv is unpaused for the chime's length, the music silenced
//!   in the chain (`mix_filter_over_silence`, built on the chain `af` holds
//!   like the playing one), then paused again with the plain chain written
//!   back and a seekable file put back where it stood
//!   (`Player::sound_while_paused`, `end_sound_while_paused`,
//!   `lock_sound_paused`). The core's own state never moves: `playback`
//!   stays paused and nothing is published; mpv's gate keeps the chime's
//!   `playback-restart`s from the core and freezes `progress`, the only
//!   thing the position frames read; a track boundary the chime crosses is
//!   not acted on (`handle_event`), so neither the sources nor the sleep
//!   timer's end of track see it; an alarm only reads the core's state;
//! - **silent** (a file still loading, a real load in flight, a paused file
//!   too close to its end): no chime — nothing would carry it.
//!
//! **A paused chime is always ended** (`finish_paused_chime`), since mpv
//! plays under the core's feet until then: at its deadline; at once when
//! starting it failed (mpv may hold the chain whatever the error, the music
//! at gain 0); before every command of the user's but the volume, the
//! mute, `+10` and the lock (`handle_command`) — the user's intent wins, and
//! the command applies to the paused file they saw: a play/pause toggled on
//! mpv unpaused for the chime would pause it for good; before every path
//! of the core's own that moves the transport or rewrites the whole chain
//! (`toggle_pause` for an alarm, the sleep timer's standby, a source's
//! action, a source switch, an equalizer toggle, an output change); and
//! when mpv goes idle, or a file starts. Those two end it in place
//! (`finish_paused_chime_in_place`): nothing to put back on an idle mpv, and
//! a file start the core did not send (it ends the chime before each load)
//! is mpv going on to the next entry, unpaused for the chime — paused again,
//! the new file waits as the user left the old one, and no seek moves it.
//! A second chime is checked against the end of the file for its own
//! length (`player::paused_chime_fits`, with the duration frozen at the
//! pause), and not played when it would reach it.
//!
//! **A second lock change during a paused chime** is played from the
//! core's record, not from `chime_route`, which answers `Silent` then (mpv
//! is unpaused, and a `Live` chime would bring the music back): the new
//! motif over the same plain chain, the file put back at the **first**
//! `resume_at` — the point the user paused at, where mpv's position has
//! moved with the chime — and the end pushed back to the new chime's. One
//! end restores once.
//!
//! Known edges, accepted: the end is reckoned from the call, while the
//! chime only sounds once mpv has refilled after the flush seek (1.4 s on
//! the dev machine under `arealtime`, expected in milliseconds on the
//! device); when the end comes before the flush's `playback-restart`, mpv's
//! gate counts that one as the restore's, and the restore's own reaches the
//! core when the user resumes — `equalizer_live` then finds nothing to
//! send. The route, the start and the end run on the core loop, each within
//! a total budget of its own (`player::mpv`: 1 s, 1.5 s, 1.5 s), never mpv's
//! 5 s per command: on an mpv that stops answering, a lock press holds the
//! loop at most about 4 s (route, start, then the end the failed start
//! calls), and the end at its deadline 1.5 s. A start that gave up is ended
//! at once; an end that gave up has still written each of its steps, which
//! an mpv answering late applies in order, and leaves `af` unknown, so the
//! resume (`toggle_pause`) or a file start writes the setting's chain
//! before the music can be heard — never resumed at gain 0. And when
//! mpv goes on to the next file by itself and a lock change is handled
//! before that `FileStarting`, the second-chime path reads the new file's
//! progress and flushes it to the old pause point; the `FileStarting` that
//! follows ends the chime in place, so the new file stays paused at that
//! position — a very narrow window, like the queued `TrackChanged` race.
//!
//! Muted means no chime at all, preview included: the owner silenced the
//! device. Every failure is logged and swallowed — the lock itself must
//! never fail because a chime could not play — and nothing is retried: after
//! an IPC timeout mpv may well have loaded the chime anyway.

use super::*;
use crate::lock_sound::{LockSound, Motif};
use crate::player::ChimeRoute;

/// How long the mixed chain is kept after the chime's end before the plain
/// chain is written back: margin for the time mpv takes to build the chain,
/// so the removal never cuts the tail. The end of a paused chime waits the
/// same.
const REMOVAL_MARGIN: Duration = Duration::from_millis(500);

/// A lock chime sounding over the paused file (`ChimeRoute::Paused`): what
/// ends it, and what a second chime replacing it reuses.
#[derive(Debug)]
pub(super) struct PausedChime {
    /// The chain mpv's `af` held before the chime, written back at its end.
    plain: String,
    /// Where to put the file back: the first chime's, kept by a second.
    resume_at: Option<f64>,
    sample_rate: u32,
    channels: u32,
    /// When to end it.
    until: Instant,
}

impl<P: Player> Core<P> {
    /// Plays `motif` of `sound` by the route mpv offers now (see the module).
    pub(super) async fn play_lock_sound(&mut self, sound: LockSound, motif: Motif) {
        if sound == LockSound::None {
            return;
        }
        if self.muted {
            tracing::debug!("lock sound: muted, not played");
            return;
        }
        // A paused chime sounds: mpv, unpaused for it, would answer `Silent`.
        // The new one replaces it from the record (see the module).
        if let Some(chime) = &self.lock_sound_paused {
            let (plain, resume_at) = (chime.plain.clone(), chime.resume_at);
            let (sample_rate, channels) = (chime.sample_rate, chime.channels);
            // The second chime plays from the first pause point for its own
            // length: the route's near-end guard, passed for the first one,
            // is checked again for it (a long preview after a short lock
            // chime). The duration is the one mpv gave at the pause:
            // `progress` stays frozen there while a chime sounds, without a
            // question to mpv. Too close to the end: the first chime goes on
            // alone — losing a chime, never advancing the file.
            if let Some(at) = resume_at {
                let duration = self.player.progress().await.ok().and_then(|p| p.duration_s);
                if !crate::player::paused_chime_fits(at, duration, crate::lock_sound::duration(sound, motif)) {
                    tracing::debug!("lock sound: a second chime would reach the end of the paused file, not played");
                    return;
                }
            }
            self.sound_while_paused(plain, sound, motif, sample_rate, channels, resume_at).await;
            return;
        }
        let route = match self.player.chime_route(crate::lock_sound::duration(sound, motif)).await {
            Ok(route) => route,
            Err(e) => {
                tracing::warn!("lock sound: where to play it: {e}");
                return;
            }
        };
        match route {
            ChimeRoute::Idle => {
                let Some(uri) = crate::lock_sound::idle_uri(sound, motif) else { return };
                if let Err(e) = self.player.play_aside(&uri).await {
                    tracing::warn!("lock sound while idle: {e}");
                }
            }
            ChimeRoute::Live { sample_rate, channels } => {
                let Some(filter) = crate::lock_sound::mix_filter(sound, motif, sample_rate, channels) else { return };
                match self.write_held_chain(Some(&filter)).await {
                    Ok(chain) => {
                        let until = Instant::now() + crate::lock_sound::duration(sound, motif) + REMOVAL_MARGIN;
                        self.lock_sound_mixed = Some((chain, until));
                    }
                    Err(e) => tracing::warn!("lock sound while playing: {e}"),
                }
            }
            ChimeRoute::Paused { sample_rate, channels, resume_at } => {
                let plain = match self.eq_plain_written.clone() {
                    Some(plain) => plain,
                    // `af` unknown (a failed write): the setting's chain is
                    // written whole first — paused, nothing sounds to click.
                    None => match self.write_held_chain(None).await {
                        Ok(plain) => plain,
                        Err(e) => {
                            tracing::warn!("lock sound while paused: {e}");
                            return;
                        }
                    },
                };
                self.sound_while_paused(plain, sound, motif, sample_rate, channels, resume_at).await;
            }
            ChimeRoute::Silent => tracing::debug!("lock sound: nothing audible to carry it, not played"),
        }
    }

    /// Sounds `motif` over the paused file, the music silenced after
    /// `plain` (the chain `af` held), and records what ends it — replacing
    /// a paused chime already sounding, whose end is pushed back. Whatever
    /// mpv answered, the record makes sure the chime is ended; on an error
    /// it is ended at once.
    async fn sound_while_paused(
        &mut self,
        plain: String,
        sound: LockSound,
        motif: Motif,
        sample_rate: u32,
        channels: u32,
        resume_at: Option<f64>,
    ) {
        let Some(filter) = crate::lock_sound::mix_filter_over_silence(sound, motif, sample_rate, channels) else {
            return;
        };
        let chain = super::equalizer::with_extra(&plain, Some(&filter));
        // A playing chime still in `af` (the user paused during it) goes
        // with this write: nothing of it is left to remove.
        self.lock_sound_mixed = None;
        let result = self.player.sound_while_paused(&chain, resume_at).await;
        self.eq_chain_written = result.is_ok().then_some(chain);
        let until = Instant::now() + crate::lock_sound::duration(sound, motif) + REMOVAL_MARGIN;
        self.lock_sound_paused = Some(PausedChime { plain, resume_at, sample_rate, channels, until });
        if let Err(e) = result {
            tracing::warn!("lock sound while paused: {e}");
            self.finish_paused_chime().await;
        }
    }

    /// Ends a paused chime, if one sounds: mpv paused again, the plain
    /// chain written back, the file put back at the first pause point (see
    /// the module for every caller).
    pub(super) async fn finish_paused_chime(&mut self) {
        self.end_paused_chime(true).await;
    }

    /// `finish_paused_chime` where the paused file is gone from under the
    /// chime — mpv idle, or a new file started by mpv itself: paused again
    /// and the plain chain back, but nothing put back. There is no point to
    /// seek to (a seek on an idle mpv is refused, after waiting the whole
    /// bounded wait for a chain rebuild that never comes; on a new file it
    /// would move that file).
    pub(super) async fn finish_paused_chime_in_place(&mut self) {
        self.end_paused_chime(false).await;
    }

    async fn end_paused_chime(&mut self, put_back: bool) {
        let Some(chime) = self.lock_sound_paused.take() else { return };
        let resume_at = chime.resume_at.filter(|_| put_back);
        match self.player.end_sound_while_paused(&chime.plain, resume_at).await {
            // `af` holds the plain chain again, the equalizer's entries as
            // they were: the gains mpv holds live are unchanged.
            Ok(()) => {
                self.eq_chain_written = Some(chime.plain.clone());
                self.eq_plain_written = Some(chime.plain);
            }
            // Every step was tried (and written, even past the end's
            // budget), but which one failed is not known, so neither is
            // `af`: it may still hold the chime's chain, the music at gain
            // 0. Marked unknown, it is repaired before the music can be
            // heard: the resume writes the setting's chain whole first
            // (`toggle_pause`, paused: no click), a file start writes it
            // anyway, and the next chime writes it before itself. Not
            // rewritten here: an mpv that did not answer the end would hold
            // the core loop for the whole of a write's 5 s.
            Err(e) => {
                tracing::warn!("lock sound end while paused: {e}");
                self.eq_chain_written = None;
                self.eq_plain_written = None;
            }
        }
    }

    /// The config page lets the owner hear `sound` — whatever the setting
    /// holds — by the same route as a lock change.
    pub async fn preview_lock_sound(&mut self, sound: LockSound) {
        self.play_lock_sound(sound, Motif::Preview).await;
    }

    /// When the chain mixed with a chime must lose it, or a paused chime
    /// must end (read in `main` before the `select!`, like
    /// `overlay_deadline`).
    pub fn lock_sound_deadline(&self) -> Option<Instant> {
        let mixed = self.lock_sound_mixed.as_ref().map(|(_, at)| *at);
        let paused = self.lock_sound_paused.as_ref().map(|chime| chime.until);
        mixed.into_iter().chain(paused).min()
    }

    /// The chime has ended: write the plain chain back — only if mpv's `af`
    /// still holds the chain written with the chime. Otherwise something
    /// rewrote it meanwhile (a file start, the equalizer, the audio output
    /// — which rewrites it whenever a chime is mixed, equalizer off
    /// included: `reapply_equalizer_after_output_change`), and the chime is
    /// already gone; each of those clears `lock_sound_mixed` too.
    ///
    /// A paused chime is ended instead (`finish_paused_chime`). The two
    /// never sound together: a paused chime replaces the playing one, and
    /// mpv is not asked for a route while it sounds.
    pub async fn expire_lock_sound(&mut self) {
        if self.lock_sound_paused.is_some() {
            self.finish_paused_chime().await;
            return;
        }
        let Some((chain, _)) = self.lock_sound_mixed.take() else { return };
        if self.eq_chain_written.as_deref() != Some(chain.as_str()) {
            return;
        }
        if let Err(e) = self.write_held_chain(None).await {
            tracing::warn!("lock sound removal: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::*;
    use crate::equalizer::{af_chain, EqualizerSettings};
    use crate::lock_sound::{duration, idle_uri, mix_filter, mix_filter_over_silence};
    use ritornello_proto::Playback;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    fn calls_of(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    fn aside(sound: LockSound, motif: Motif) -> String {
        format!("aside {}", idle_uri(sound, motif).unwrap())
    }

    fn chime_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("aside ") || c.starts_with("af") || c.starts_with("paused-chime"))
            .cloned()
            .collect()
    }

    fn set_route(core: &Core<FakePlayer>, route: ChimeRoute) {
        *core.player.route.lock().unwrap() = route;
    }

    const LIVE: ChimeRoute = ChimeRoute::Live { sample_rate: 48000, channels: 2 };

    /// A core playing with `eq`, its player calls cleared.
    fn playing_with(eq: EqualizerSettings) -> Rig {
        let rig = setup_persisted(PersistedState { equalizer: eq, ..Default::default() });
        set_route(&rig.0, LIVE);
        rig.1.lock().unwrap().clear();
        rig
    }

    fn enabled() -> EqualizerSettings {
        EqualizerSettings { enabled: true, ..Default::default() }
    }

    #[tokio::test]
    async fn locking_in_standby_plays_aside_and_changes_nothing_else() {
        let (mut core, pc, sc, rx, _d) = setup();
        core.start_in_standby().await.unwrap();
        assert!(rx.borrow().standby, "otherwise the test proves nothing");
        let playback_before = rx.borrow().playback;
        pc.lock().unwrap().clear();
        sc.lock().unwrap().clear();
        set_route(&core, ChimeRoute::Idle);

        core.handle_command(Command::SetInputLock(true)).await.unwrap();

        let calls = calls_of(&pc);
        assert_eq!(calls.last(), Some(&aside(LockSound::A, Motif::Lock)), "{calls:?}");
        assert!(
            !calls.iter().any(|c| c.starts_with("play ") || c.starts_with("load_list") || c == "stop"),
            "the chime must not load anything the core sees: {calls:?}"
        );
        assert!(rx.borrow().standby, "locking must not wake the device");
        assert_eq!(rx.borrow().playback, playback_before, "no playback appears");
        assert!(sc.lock().unwrap().is_empty(), "no source was asked anything: {:?}", sc.lock().unwrap());
    }

    #[tokio::test]
    async fn unlocking_plays_the_unlock_motif() {
        let (mut core, pc, _sc, _rx, _d) = setup();
        core.handle_command(Command::SetInputLock(false)).await.unwrap();
        assert_eq!(chime_calls(&pc), vec![aside(LockSound::A, Motif::Unlock)]);
        // No change: the key still answers, like the overlay.
        core.handle_command(Command::SetInputLock(false)).await.unwrap();
        assert_eq!(chime_calls(&pc), vec![aside(LockSound::A, Motif::Unlock), aside(LockSound::A, Motif::Unlock)]);
    }

    #[tokio::test]
    async fn the_chosen_sound_is_played_and_none_plays_nothing() {
        let (mut core, pc, _sc, _rx, _d) = setup();
        core.settings.lock_sound = LockSound::C;
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        assert_eq!(chime_calls(&pc), vec![aside(LockSound::C, Motif::Lock)]);

        pc.lock().unwrap().clear();
        core.settings.lock_sound = LockSound::None;
        core.handle_command(Command::SetInputLock(false)).await.unwrap();
        set_route(&core, LIVE);
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        assert_eq!(chime_calls(&pc), Vec::<String>::new());
        assert!(core.lock_sound_deadline().is_none());
    }

    #[tokio::test]
    async fn muted_plays_nothing() {
        for route in [ChimeRoute::Idle, LIVE, PAUSED] {
            let (mut core, pc, _sc, _rx, _d) = setup();
            core.handle_command(Command::Mute).await.unwrap();
            assert!(core.player_state().muted, "otherwise the test proves nothing");
            set_route(&core, route);
            pc.lock().unwrap().clear();
            core.handle_command(Command::SetInputLock(true)).await.unwrap();
            core.handle_command(Command::SetInputLock(false)).await.unwrap();
            core.preview_lock_sound(LockSound::E).await;
            assert_eq!(chime_calls(&pc), Vec::<String>::new(), "{route:?}");
            assert!(core.lock_sound_deadline().is_none(), "{route:?}");
        }
    }

    #[tokio::test]
    async fn no_chime_when_the_route_is_silent() {
        let (mut core, pc, _sc, _rx, _d) = setup();
        set_route(&core, ChimeRoute::Silent);
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        core.preview_lock_sound(LockSound::B).await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new());
        assert!(core.lock_sound_deadline().is_none());
    }

    #[tokio::test]
    async fn a_failed_route_query_is_logged_not_fatal() {
        let (mut core, pc, _sc, rx, _d) = setup();
        core.player.route_fails.store(true, Ordering::SeqCst);
        core.handle_command(Command::SetInputLock(true)).await.expect("the lock never fails for its chime");
        assert!(rx.borrow().input_locked);
        assert!(matches!(core.player_state().overlay, Some(Overlay::Message { .. })), "the overlay is shown");
        assert_eq!(chime_calls(&pc), Vec::<String>::new());
    }

    #[tokio::test]
    async fn while_playing_the_chime_is_mixed_then_removed() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = playing_with(eq.clone());
        let before = Instant::now();
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        let after = Instant::now();
        let filter = mix_filter(LockSound::A, Motif::Lock, 48000, 2).unwrap();
        assert_eq!(chime_calls(&pc), vec![format!("af {},{filter}", af_chain(&eq))]);

        let length = duration(LockSound::A, Motif::Lock);
        assert!((length.as_secs_f64() - 0.74).abs() < 1e-6, "the chime the deadline is reckoned from: {length:?}");
        let at = core.lock_sound_deadline().expect("a removal is due");
        assert!(at >= before + length + Duration::from_millis(500), "{at:?}");
        assert!(at <= after + length + Duration::from_millis(500), "{at:?}");

        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&eq))]);
        assert!(core.lock_sound_deadline().is_none(), "one removal, then nothing to wake for");
        // The plain chain is what mpv holds: the next file start writes nothing.
        pc.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new());
    }

    /// A band moved live (`af-command`) leaves mpv's `af` holding the old
    /// gains. Rebuilding the chime's chain from the setting changed the
    /// equalizer's entries mid-playback — the rewrite that clicks — and its
    /// removal then wrote the setting's chain once more. The chime is mixed
    /// into the chain mpv holds, and its removal writes that same chain back:
    /// the equalizer's entries are never touched, and the live gains stand.
    #[tokio::test]
    async fn a_chime_after_a_live_band_change_leaves_the_equalizer_entries_alone() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = setup_persisted(PersistedState { equalizer: eq.clone(), ..Default::default() });
        core.handle_event(Event::FileStarting).await;
        core.handle_event(Event::PlaybackRestarted).await;
        set_route(&core, LIVE);
        let moved = EqualizerSettings { gains: [0, 0, 0, 0, 0, 6, 0, 0, 0, 0], ..eq.clone() };
        core.set_equalizer(moved.clone()).await.unwrap();
        assert!(
            calls_of(&pc).iter().any(|c| c.starts_with("af_cmd eq5 ")),
            "the band went live, otherwise the test proves nothing: {:?}",
            calls_of(&pc)
        );
        pc.lock().unwrap().clear();

        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        let filter = mix_filter(LockSound::A, Motif::Lock, 48000, 2).unwrap();
        assert_eq!(chime_calls(&pc), vec![format!("af {},{filter}", af_chain(&eq))], "mixed into the chain mpv holds");

        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&eq))], "the same plain chain back");

        // No gain reverted: the live gains are still counted as held, so the
        // same setting again sends nothing…
        pc.lock().unwrap().clear();
        core.set_equalizer(moved.clone()).await.unwrap();
        assert_eq!(chime_calls(&pc), Vec::<String>::new());
        // …and the next file still gets the setting's chain, written whole.
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&moved))]);
    }

    #[tokio::test]
    async fn with_the_equalizer_off_the_chime_is_the_whole_chain() {
        let (mut core, pc, _sc, _rx, _d) = playing_with(EqualizerSettings::default());
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        let filter = mix_filter(LockSound::A, Motif::Lock, 48000, 2).unwrap();
        assert_eq!(chime_calls(&pc), vec![format!("af {filter}")]);
        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec!["af ".to_string()]);
    }

    #[tokio::test]
    async fn a_second_chime_while_mixed_replaces_the_first_and_one_removal_restores() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = playing_with(eq.clone());
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        let first = core.lock_sound_deadline().unwrap();
        core.handle_command(Command::SetInputLock(false)).await.unwrap();
        let second = core.lock_sound_deadline().unwrap();
        assert!(second >= first, "the deadline is the second chime's");
        let lock = mix_filter(LockSound::A, Motif::Lock, 48000, 2).unwrap();
        let unlock = mix_filter(LockSound::A, Motif::Unlock, 48000, 2).unwrap();
        assert_eq!(
            chime_calls(&pc),
            vec![format!("af {},{lock}", af_chain(&eq)), format!("af {},{unlock}", af_chain(&eq))],
            "the second replaces the first: never two chimes stacked"
        );

        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&eq))], "a single restore");
    }

    #[tokio::test]
    async fn a_file_start_during_a_mixed_chime_cancels_its_removal() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = playing_with(eq.clone());
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        pc.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&eq))], "the file start writes the plain chain");
        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new(), "nothing left to remove");

        // The same after the equalizer toggled meanwhile.
        let (mut core, pc, _sc, _rx, _d) = playing_with(eq.clone());
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        let off = EqualizerSettings { enabled: false, ..eq.clone() };
        core.set_equalizer(off.clone()).await.unwrap();
        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new(), "the toggle already wrote the chain without the chime");
    }

    /// An output change drops a mixed chime with the equalizer off too, where
    /// it would otherwise leave mpv's chain alone and the chime outlive the
    /// output it was mixed for.
    #[tokio::test]
    async fn an_output_change_drops_a_mixed_chime_equalizer_off_included() {
        let (mut core, pc, _sc, _rx, _d) = playing_with(EqualizerSettings::default());
        core.set_audio_device(Some("hw:1".into())).await.unwrap();
        assert_eq!(chime_calls(&pc), Vec::<String>::new(), "equalizer off, no chime: the chain is left alone");

        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        pc.lock().unwrap().clear();
        core.set_audio_device(None).await.unwrap();
        assert_eq!(chime_calls(&pc), vec!["af ".to_string()], "the plain (empty) chain is written back");
        assert!(core.lock_sound_deadline().is_none(), "nothing left to remove");
        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new());

        // Equalizer on: the rewrite drops the chime as before.
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = playing_with(eq.clone());
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        pc.lock().unwrap().clear();
        core.set_audio_device(Some("hw:1".into())).await.unwrap();
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&eq))]);
        assert!(core.lock_sound_deadline().is_none());
    }

    #[tokio::test]
    async fn preview_plays_the_requested_sound_not_the_setting() {
        let (mut core, pc, _sc, _rx, _d) = setup();
        assert_eq!(core.settings.lock_sound, LockSound::A);
        core.preview_lock_sound(LockSound::E).await;
        assert_eq!(chime_calls(&pc), vec![aside(LockSound::E, Motif::Preview)]);
    }

    // --- While paused ---

    /// Where the paused spike's file stood (kf48 cycle 0).
    const AT: f64 = 4.416;
    const PAUSED: ChimeRoute = ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at: Some(AT) };

    fn paused_chime(plain: &str, motif: Motif, resume_at: Option<f64>) -> String {
        let mix = mix_filter_over_silence(LockSound::A, motif, 48000, 2).unwrap();
        let chain = if plain.is_empty() { mix } else { format!("{plain},{mix}") };
        format!("paused-chime {chain} {resume_at:?}")
    }

    fn paused_chime_end(plain: &str, resume_at: Option<f64>) -> String {
        format!("paused-chime-end {plain} {resume_at:?}")
    }

    /// A core whose file played (its chain live), then paused by the user;
    /// the route is `PAUSED`, the player calls cleared.
    async fn paused_with(eq: EqualizerSettings) -> Rig {
        let (mut core, pc, sc, rx, d) = setup_persisted(PersistedState { equalizer: eq, ..Default::default() });
        core.resume().await.unwrap();
        core.handle_event(Event::FileStarting).await;
        core.handle_event(Event::PlaybackRestarted).await;
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(core.playback && core.paused, "otherwise the test proves nothing");
        set_route(&core, PAUSED);
        pc.lock().unwrap().clear();
        (core, pc, sc, rx, d)
    }

    /// `paused_with`, the lock's chime sounding over the paused file.
    async fn chiming_while_paused(eq: EqualizerSettings) -> Rig {
        let (mut core, pc, sc, rx, d) = paused_with(eq.clone()).await;
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        assert_eq!(chime_calls(&pc), vec![paused_chime(&af_chain(&eq), Motif::Lock, Some(AT))]);
        pc.lock().unwrap().clear();
        (core, pc, sc, rx, d)
    }

    #[tokio::test]
    async fn while_paused_the_chime_sounds_over_silence_then_the_file_is_put_back() {
        let eq = enabled();
        let (mut core, pc, _sc, rx, _d) = paused_with(eq.clone()).await;
        let before = Instant::now();
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        let after = Instant::now();
        assert_eq!(chime_calls(&pc), vec![paused_chime(&af_chain(&eq), Motif::Lock, Some(AT))]);
        let length = duration(LockSound::A, Motif::Lock);
        let at = core.lock_sound_deadline().expect("an end is due");
        assert!(at >= before + length + REMOVAL_MARGIN && at <= after + length + REMOVAL_MARGIN, "{at:?}");
        // The core's own state never moves.
        assert!(core.playback && core.paused);
        assert_eq!(rx.borrow().playback, Playback::Paused);

        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![paused_chime_end(&af_chain(&eq), Some(AT))]);
        assert!(core.lock_sound_deadline().is_none(), "one end, then nothing to wake for");
        core.expire_lock_sound().await;
        assert_eq!(calls_of(&pc).len(), 1, "a single end: {:?}", calls_of(&pc));
        assert!(core.playback && core.paused);
        assert_eq!(rx.borrow().playback, Playback::Paused);

        // mpv's `af` holds the setting's chain again: the next file start
        // writes nothing.
        pc.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_paused_stream_is_not_put_back() {
        let (mut core, pc, _sc, _rx, _d) = paused_with(EqualizerSettings::default()).await;
        set_route(&core, ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at: None });
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![paused_chime("", Motif::Lock, None), paused_chime_end("", None)]);
    }

    /// The paused chime is built on the chain mpv's `af` holds, like the
    /// playing one: after a band moved live, the equalizer's entries are
    /// written as `af` carries them and the live gains stand.
    #[tokio::test]
    async fn a_paused_chime_after_a_live_band_change_leaves_the_equalizer_entries_alone() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = paused_with(eq.clone()).await;
        let moved = EqualizerSettings { gains: [0, 0, 0, 0, 0, 6, 0, 0, 0, 0], ..eq.clone() };
        core.set_equalizer(moved.clone()).await.unwrap();
        assert!(calls_of(&pc).iter().any(|c| c.starts_with("af_cmd eq5 ")), "{:?}", calls_of(&pc));
        pc.lock().unwrap().clear();

        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        core.expire_lock_sound().await;
        assert_eq!(
            chime_calls(&pc),
            vec![paused_chime(&af_chain(&eq), Motif::Lock, Some(AT)), paused_chime_end(&af_chain(&eq), Some(AT))]
        );
        pc.lock().unwrap().clear();
        core.set_equalizer(moved.clone()).await.unwrap();
        assert_eq!(chime_calls(&pc), Vec::<String>::new(), "no gain reverted");
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&moved))]);
    }

    /// `af` unknown after a failed write: the setting's chain is written
    /// whole first (paused: nothing to click), and the chime built on it.
    #[tokio::test]
    async fn a_paused_chime_on_an_unknown_chain_writes_the_setting_first() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = paused_with(EqualizerSettings::default()).await;
        core.player.af_fails.store(true, Ordering::SeqCst);
        core.set_equalizer(eq.clone()).await.unwrap_err();
        core.player.af_fails.store(false, Ordering::SeqCst);
        pc.lock().unwrap().clear();
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        core.expire_lock_sound().await;
        assert_eq!(
            chime_calls(&pc),
            vec![
                format!("af {}", af_chain(&eq)),
                paused_chime(&af_chain(&eq), Motif::Lock, Some(AT)),
                paused_chime_end(&af_chain(&eq), Some(AT)),
            ]
        );
    }

    #[tokio::test]
    async fn a_preview_while_paused_takes_the_same_path_and_publishes_nothing() {
        let (mut core, pc, _sc, mut rx, _d) = paused_with(EqualizerSettings::default()).await;
        rx.borrow_and_update();
        core.preview_lock_sound(LockSound::A).await;
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![paused_chime("", Motif::Preview, Some(AT)), paused_chime_end("", Some(AT))]);
        assert!(!rx.has_changed().unwrap(), "nothing published by the chime");
    }

    /// mpv answers `Silent` while a paused chime sounds (it is unpaused):
    /// a second lock change is played from the core's own record — the new
    /// motif over the same silenced chain, the file put back at the
    /// **first** pause point, the end pushed back — and ends once.
    #[tokio::test]
    async fn a_second_chime_while_paused_replaces_the_first_and_one_end_restores_the_first_point() {
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        let first = core.lock_sound_deadline().unwrap();
        let asked = core.player.route_asked.lock().unwrap().len();
        // Whatever mpv would answer now is not asked.
        set_route(&core, ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at: Some(9.0) });
        core.handle_command(Command::SetInputLock(false)).await.unwrap();
        assert_eq!(core.player.route_asked.lock().unwrap().len(), asked, "mpv is not asked");
        assert_eq!(chime_calls(&pc), vec![paused_chime("", Motif::Unlock, Some(AT))]);
        assert!(core.lock_sound_deadline().unwrap() >= first, "the end is the second chime's");

        pc.lock().unwrap().clear();
        core.expire_lock_sound().await;
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![paused_chime_end("", Some(AT))], "a single end, at the first point");
    }

    #[tokio::test]
    async fn a_failed_paused_chime_is_ended_at_once() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = paused_with(eq.clone()).await;
        core.player.paused_chime_fails.store(true, Ordering::SeqCst);
        core.handle_command(Command::SetInputLock(true)).await.expect("the lock never fails for its chime");
        assert_eq!(
            chime_calls(&pc),
            vec![paused_chime(&af_chain(&eq), Motif::Lock, Some(AT)), paused_chime_end(&af_chain(&eq), Some(AT))],
            "the chain may be applied whatever the error: the end follows"
        );
        assert!(core.lock_sound_deadline().is_none());
        pc.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new(), "the end put the setting's chain back");
    }

    /// A failed end (mpv not answering within its budget, or refusing a
    /// step) may leave the chime's chain in `af` — the music at gain 0. It
    /// is not rewritten on the spot, which would hold the core loop for a
    /// whole write's timeout on an mpv that does not answer: `af` is marked
    /// unknown, and the resume writes the setting's chain whole first, the
    /// file still paused (no click), so the music never comes back silent.
    /// A file start writes it too.
    #[tokio::test]
    async fn a_failed_paused_chime_end_is_repaired_before_the_music_resumes() {
        let eq = enabled();
        let (mut core, pc, _sc, rx, _d) = chiming_while_paused(eq.clone()).await;
        core.player.paused_chime_end_fails.store(true, Ordering::SeqCst);
        core.expire_lock_sound().await;
        assert_eq!(calls_of(&pc), vec![paused_chime_end(&af_chain(&eq), Some(AT))], "nothing more on the loop");
        assert!(core.lock_sound_deadline().is_none());
        assert!(core.paused);
        pc.lock().unwrap().clear();
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(calls_of(&pc), vec![format!("af {}", af_chain(&eq)), "pause".to_string()], "the chain, then the resume");
        assert_eq!(rx.borrow().playback, Playback::Playing);
        // Known again: the next resume writes nothing more.
        core.handle_command(Command::PlayPause).await.unwrap();
        pc.lock().unwrap().clear();
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(calls_of(&pc), vec!["pause".to_string()]);

        // An alarm resumes through the same toggle.
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(eq.clone()).await;
        core.player.paused_chime_end_fails.store(true, Ordering::SeqCst);
        core.expire_lock_sound().await;
        pc.lock().unwrap().clear();
        core.play().await.unwrap();
        assert_eq!(calls_of(&pc), vec![format!("af {}", af_chain(&eq)), "pause".to_string()]);

        // Or a file start, if one comes first.
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(eq.clone()).await;
        core.player.paused_chime_end_fails.store(true, Ordering::SeqCst);
        core.expire_lock_sound().await;
        pc.lock().unwrap().clear();
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), vec![format!("af {}", af_chain(&eq))]);
    }

    /// The start failing too (mpv gone silent on the chime): the end is
    /// tried at once, and the resume repairs the chain all the same.
    #[tokio::test]
    async fn a_paused_chime_whose_start_and_end_both_fail_is_repaired_at_the_resume() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = paused_with(eq.clone()).await;
        core.player.paused_chime_fails.store(true, Ordering::SeqCst);
        core.player.paused_chime_end_fails.store(true, Ordering::SeqCst);
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        assert_eq!(
            chime_calls(&pc),
            vec![paused_chime(&af_chain(&eq), Motif::Lock, Some(AT)), paused_chime_end(&af_chain(&eq), Some(AT))]
        );
        assert!(core.lock_sound_deadline().is_none());
        pc.lock().unwrap().clear();
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(calls_of(&pc), vec![format!("af {}", af_chain(&eq)), "pause".to_string()]);
    }

    /// The user's command applies to the paused file the user saw: the
    /// chime is ended first (paused again, plain chain, put back), then the
    /// command runs. A play/pause sent to mpv still unpaused for the chime
    /// would pause it, and the music would never resume.
    #[tokio::test]
    async fn a_user_command_during_a_paused_chime_ends_it_first() {
        let commands = [
            Command::PlayPause,
            Command::Next,
            Command::Prev,
            Command::Stop,
            Command::SourceCycle,
            Command::SelectSource("cd".into()),
            Command::SelectSource("radio".into()),
            Command::Select(2),
            Command::Eject,
            Command::Power,
            Command::SeekForward,
            Command::SeekBackward,
            Command::SeekTo(10),
            Command::ToggleRandom,
            Command::SetSleepTimer { minutes: 10, mode: ritornello_proto::SleepMode::Immediate },
        ];
        for cmd in commands {
            let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
            core.handle_command(cmd.clone()).await.ok();
            let calls = calls_of(&pc);
            assert_eq!(calls.first(), Some(&paused_chime_end("", Some(AT))), "{cmd:?}: {calls:?}");
            assert_eq!(calls.iter().filter(|c| c.starts_with("paused-chime")).count(), 1, "{cmd:?}: {calls:?}");
            assert!(core.lock_sound_deadline().is_none(), "{cmd:?}");
        }
    }

    /// A source asking to be played comes from no command: the switch ends
    /// the chime before its stop all the same.
    #[tokio::test]
    async fn a_source_switching_during_a_paused_chime_ends_it_first() {
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        core.handle_play_request("cd", ritornello_proto::PlayRequest::Switch).await.unwrap();
        assert_eq!(core.active_source(), "cd");
        let calls = calls_of(&pc);
        assert_eq!(calls.first(), Some(&paused_chime_end("", Some(AT))), "{calls:?}");
        assert!(core.lock_sound_deadline().is_none());
    }

    /// Every source action moves the transport. Each path that applies one
    /// today has already ended the chime (a command, a switch, mpv idle);
    /// `apply` ends it too, so that a new path cannot forget to.
    #[tokio::test]
    async fn a_source_action_during_a_paused_chime_ends_it_first() {
        for action in [SourceAction::PlayerNext, SourceAction::PlayerChapter(3), SourceAction::Stop] {
            let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
            core.apply(action.clone()).await.unwrap();
            let calls = calls_of(&pc);
            assert_eq!(calls.first(), Some(&paused_chime_end("", Some(AT))), "{action:?}: {calls:?}");
        }
        // `Noop` moves nothing: the chime sounds on.
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        core.apply(SourceAction::Noop).await.unwrap();
        assert_eq!(calls_of(&pc), Vec::<String>::new());
        assert!(core.lock_sound_deadline().is_some());
    }

    #[tokio::test]
    async fn play_pause_during_a_paused_chime_resumes_the_music() {
        let (mut core, pc, _sc, rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        core.handle_command(Command::PlayPause).await.unwrap();
        assert_eq!(calls_of(&pc), vec![paused_chime_end("", Some(AT)), "pause".to_string()]);
        assert!(core.playback && !core.paused);
        assert_eq!(rx.borrow().playback, Playback::Playing);
    }

    /// The volume, the mute, `+10` and the lock itself move nothing of the
    /// transport: the chime sounds on.
    #[tokio::test]
    async fn the_volume_and_plus_ten_leave_a_paused_chime_sounding() {
        for cmd in [
            Command::VolumeUp,
            Command::VolumeDown,
            Command::SetVolume(30),
            Command::Mute,
            Command::Plus10,
            Command::InputLockRefused,
        ] {
            let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
            core.handle_command(cmd.clone()).await.unwrap();
            assert!(!calls_of(&pc).iter().any(|c| c.starts_with("paused-chime")), "{cmd:?}: {:?}", calls_of(&pc));
            assert!(core.lock_sound_deadline().is_some(), "{cmd:?}");
        }
    }

    /// Every load the core sends ends the chime first, so a file start
    /// while it sounds is mpv going on to the next entry by itself,
    /// unpaused for the chime: the next file would play at full gain under
    /// a display that says paused. The chime is ended in place — mpv
    /// paused again, the plain chain back, no seek (it would move the new
    /// file) — and the core stays paused. The plain chain being the
    /// setting's, the file start writes nothing more.
    #[tokio::test]
    async fn a_file_start_during_a_paused_chime_pauses_the_new_file_without_a_seek() {
        let eq = enabled();
        let (mut core, pc, _sc, rx, _d) = chiming_while_paused(eq.clone()).await;
        core.handle_event(Event::FileStarting).await;
        assert_eq!(chime_calls(&pc), vec![paused_chime_end(&af_chain(&eq), None)]);
        assert!(core.lock_sound_deadline().is_none());
        assert!(core.playback && core.paused);
        assert_eq!(rx.borrow().playback, Playback::Paused);
        core.expire_lock_sound().await;
        assert_eq!(chime_calls(&pc), vec![paused_chime_end(&af_chain(&eq), None)], "nothing left to end");
    }

    /// mpv went idle under the chime without a file start: it is ended, so
    /// that the plain chain is back for whatever loads next — without a
    /// seek, which an idle mpv refuses after the whole bounded wait for a
    /// chain rebuild that never comes.
    #[tokio::test]
    async fn an_idle_mpv_during_a_paused_chime_ends_it_without_a_seek() {
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        core.handle_event(Event::PlaybackIdle).await;
        assert_eq!(calls_of(&pc).first(), Some(&paused_chime_end("", None)), "{:?}", calls_of(&pc));
        assert!(core.lock_sound_deadline().is_none());
    }

    /// A second chime plays from the first pause point for its own length:
    /// the near-end guard the route applied to the first is applied to it,
    /// with the duration mpv gave at the pause (`progress`, frozen). Too
    /// close to the end, the first chime goes on alone.
    #[tokio::test]
    async fn a_second_paused_chime_too_long_for_the_end_of_the_file_is_not_played() {
        let lock = duration(LockSound::A, Motif::Lock).as_secs_f64();
        let preview = duration(LockSound::A, Motif::Preview).as_secs_f64();
        // Room for the lock chime (0.74 + 1 s), not for the preview (1.48 + 1 s).
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        core.set_progress(Some(AT), Some(AT + lock + 1.1));
        let first = core.lock_sound_deadline().unwrap();
        core.preview_lock_sound(LockSound::A).await;
        assert_eq!(chime_calls(&pc), Vec::<String>::new(), "the first chime goes on alone");
        assert_eq!(core.lock_sound_deadline(), Some(first), "its end unchanged");

        // Just enough room: played.
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        core.set_progress(Some(AT), Some(AT + preview + 1.01));
        core.preview_lock_sound(LockSound::A).await;
        assert_eq!(chime_calls(&pc), vec![paused_chime("", Motif::Preview, Some(AT))]);

        // No duration known (a file mpv gives none of): no guard, as for the route.
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(EqualizerSettings::default()).await;
        core.set_progress(Some(AT), None);
        core.preview_lock_sound(LockSound::A).await;
        assert_eq!(chime_calls(&pc), vec![paused_chime("", Motif::Preview, Some(AT))]);

        // A stream (`resume_at` None): its duration is its cache end, no guard.
        let (mut core, pc, _sc, _rx, _d) = paused_with(EqualizerSettings::default()).await;
        set_route(&core, ChimeRoute::Paused { sample_rate: 48000, channels: 2, resume_at: None });
        core.set_progress(Some(1.2), Some(1.386667));
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        core.preview_lock_sound(LockSound::A).await;
        assert_eq!(
            chime_calls(&pc),
            vec![paused_chime("", Motif::Lock, None), paused_chime("", Motif::Preview, None)]
        );
    }

    /// A whole-chain write of the equalizer or of an output change would
    /// write the plain chain with mpv unpaused: the music back at full gain
    /// under the chime. The chime is ended first.
    #[tokio::test]
    async fn an_equalizer_toggle_or_an_output_change_ends_a_paused_chime_first() {
        let eq = enabled();
        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(eq.clone()).await;
        core.set_equalizer(EqualizerSettings::default()).await.unwrap();
        assert_eq!(chime_calls(&pc), vec![paused_chime_end(&af_chain(&eq), Some(AT)), "af ".to_string()]);

        let (mut core, pc, _sc, _rx, _d) = chiming_while_paused(eq.clone()).await;
        core.set_audio_device(Some("hw:1".into())).await.unwrap();
        let calls = calls_of(&pc);
        assert_eq!(calls.first(), Some(&paused_chime_end(&af_chain(&eq), Some(AT))), "{calls:?}");
        assert!(core.lock_sound_deadline().is_none());
    }

    /// A track boundary crossed by the chime itself (a disc paused just
    /// before a track's end: its tracks are chapters, which the end-of-file
    /// guard does not see) is mpv's, not the listener's: the core does not
    /// act on it, and the restore's notification of the track it was on
    /// changes nothing either.
    #[tokio::test]
    async fn a_track_change_during_a_paused_chime_is_not_acted_on() {
        let (mut core, pc, sc, _rx, _d) = paused_with(EqualizerSettings::default()).await;
        core.handle_event(Event::TrackChanged(1)).await;
        assert_eq!(core.track_ref, Some(1));
        core.handle_command(Command::SetInputLock(true)).await.unwrap();
        sc.lock().unwrap().clear();
        core.handle_event(Event::TrackChanged(2)).await;
        assert_eq!(core.track_ref, Some(1), "the chime's crossing is no track change");
        assert!(sc.lock().unwrap().is_empty(), "no source was told: {:?}", sc.lock().unwrap());
        core.expire_lock_sound().await;
        assert!(calls_of(&pc).iter().any(|c| c.starts_with("paused-chime-end")));
        // The restore puts the disc back on track 1, which mpv notifies.
        core.handle_event(Event::TrackChanged(1)).await;
        assert_eq!(core.track_ref, Some(1));
    }
}
