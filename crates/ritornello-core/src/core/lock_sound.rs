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
//!   the chime has ended (`lock_sound_mixed`, `expire_lock_sound`). Mixing
//!   (`amix`, `normalize=0`) leaves the main stream untouched: measured
//!   without a click on mpv 0.37 / FFmpeg 6.1;
//! - **silent** (paused, or a file still loading): no chime — nothing would
//!   carry it.
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
/// so the removal never cuts the tail.
const REMOVAL_MARGIN: Duration = Duration::from_millis(500);

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
        let route = match self.player.chime_route().await {
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
                match self.write_chain_with(Some(&filter)).await {
                    Ok(chain) => {
                        let until = Instant::now() + crate::lock_sound::duration(sound, motif) + REMOVAL_MARGIN;
                        self.lock_sound_mixed = Some((chain, until));
                    }
                    Err(e) => tracing::warn!("lock sound while playing: {e}"),
                }
            }
            ChimeRoute::Silent => tracing::debug!("lock sound: nothing audible to carry it, not played"),
        }
    }

    /// The config page lets the owner hear `sound` — whatever the setting
    /// holds — by the same route as a lock change.
    pub async fn preview_lock_sound(&mut self, sound: LockSound) {
        self.play_lock_sound(sound, Motif::Preview).await;
    }

    /// When the chain mixed with a chime must lose it (read in `main` before
    /// the `select!`, like `overlay_deadline`).
    pub fn lock_sound_deadline(&self) -> Option<Instant> {
        self.lock_sound_mixed.as_ref().map(|(_, at)| *at)
    }

    /// The chime has ended: write the plain chain back — only if mpv's `af`
    /// still holds the chain written with the chime. Otherwise something
    /// rewrote it meanwhile (a file start, the equalizer, the audio output),
    /// and the chime is already gone.
    pub async fn expire_lock_sound(&mut self) {
        let Some((chain, _)) = self.lock_sound_mixed.take() else { return };
        if self.eq_chain_written.as_deref() != Some(chain.as_str()) {
            return;
        }
        if let Err(e) = self.write_chain_with(None).await {
            tracing::warn!("lock sound removal: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::*;
    use crate::equalizer::{af_chain, EqualizerSettings};
    use crate::lock_sound::{duration, idle_uri, mix_filter};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    fn calls_of(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    fn aside(sound: LockSound, motif: Motif) -> String {
        format!("aside {}", idle_uri(sound, motif).unwrap())
    }

    fn chime_calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|c| c.starts_with("aside ") || c.starts_with("af")).cloned().collect()
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
        for route in [ChimeRoute::Idle, LIVE] {
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

    #[tokio::test]
    async fn preview_plays_the_requested_sound_not_the_setting() {
        let (mut core, pc, _sc, _rx, _d) = setup();
        assert_eq!(core.settings.lock_sound, LockSound::A);
        core.preview_lock_sound(LockSound::E).await;
        assert_eq!(chime_calls(&pc), vec![aside(LockSound::E, Motif::Preview)]);
    }
}
