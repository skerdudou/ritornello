//! Source plugin "cd": disc presence, playback, current track, eject.
//!
//! It knows **no** metadata provider. What it knows about the disc, it declares
//! in the track identity (the raw TOC and the track index); artist, album and
//! titles come from a `metadata` plugin — for example
//! `ritornello-plugin-musicbrainz` — arbitrated by the core. A slow network
//! call therefore no longer lives in the process that must answer track
//! commands.

mod cd;

use anyhow::Result;
use ritornello_plugin_sdk::{Notification, Runtime, SourceOutcome, SourcePlugin};
use ritornello_proto::SourceAction;
use std::path::PathBuf;
use tokio::sync::mpsc;

use ritornello_i18n::Catalog;

const CD_EN: &str = include_str!("locales/en.toml");

/// Result of a TOC read: validity epoch, raw TOC if readable, number of
/// tracks.
type ReadToc = (u64, Option<String>, usize);

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct CdSource {
    cd_dev: String,
    present: bool,
    track: i64,
    /// Raw TOC of the inserted disc (`cd-discid` output), as it goes into the
    /// identity. `None` until it has been read, or if it is unreadable.
    toc: Option<String>,
    /// TOC of the previous disc, the only way to tell a **presence flicker** of
    /// the drive (same disc, playback goes on) from a **disc swap** (nothing
    /// can play any more).
    previous_toc: Option<String>,
    total_tracks: usize,
    /// True if the plugin requested playback and has not stopped it since.
    ///
    /// Needed for the identity: a disc **present in the tray** is not a track
    /// **being played**, and only the latter has metadata to display. Without
    /// this distinction, inserting a disc without starting anything would make
    /// a third-party service get queried for nothing.
    playback: bool,
    epoch: u64,
    presence_rx: mpsc::Receiver<bool>,
    toc_tx: mpsc::Sender<ReadToc>,
    toc_rx: mpsc::Receiver<ReadToc>,
    catalog: Catalog,
    locales_root: PathBuf,
}

impl CdSource {
    /// Complete outcome: action, status, preset and identity of what plays.
    fn issue(&self, action: SourceAction) -> SourceOutcome {
        let outcome = SourceOutcome::new(action);
        // The Source's permanent status: what the SPA's Player card now
        // displays (see `SourceMessage::status`).
        let outcome = if self.present {
            outcome.status(self.catalog.get("cd_audio"))
        } else {
            outcome.status(self.catalog.get("no_disc"))
        };
        // The count is a property of the inserted disc, not of playback: it is
        // declared on every frame, 0 when no TOC is known (no disc, or the
        // TOC is still being read).
        let count = match &self.toc {
            Some(_) => u8::try_from(self.total_tracks).unwrap_or(255),
            None => 0,
        };
        let outcome = outcome.preset_count(count);
        match (self.playback && self.present, &self.toc) {
            // The TOC designates the disc, the index designates the track: both
            // are needed, a track change being a change of what plays.
            (true, Some(toc)) => {
                let outcome = outcome.plays(serde_json::json!({
                    "kind": "disc",
                    "toc": toc,
                    "tracks": self.total_tracks,
                    "track": self.track,
                }));
                // The current track is the key to highlight.
                match u8::try_from(self.track + 1) {
                    Ok(n) => outcome.preset(n),
                    Err(_) => outcome,
                }
            }
            // Nothing plays, or nothing identifiable (TOC not read yet,
            // unreadable, empty drive). We say so: a partial identity would
            // make the plugins work for nothing.
            _ => outcome.plays_nothing(),
        }
    }

    fn spawn_toc_read(&self) {
        let cd_dev = self.cd_dev.clone();
        let tx = self.toc_tx.clone();
        let epoch = self.epoch;
        tokio::spawn(async move {
            let read = tokio::task::spawn_blocking(move || {
                cd::read_toc(&cd_dev).and_then(|raw| {
                    let n = cd::toc_ntracks(&raw)?;
                    Ok((raw.trim().to_string(), n))
                })
            })
            .await;
            let result = match read {
                Ok(Ok((raw, n))) => (epoch, Some(raw), n),
                Ok(Err(e)) => {
                    tracing::info!("TOC unreadable: {e}");
                    (epoch, None, 0)
                }
                Err(e) => {
                    tracing::warn!("TOC task interrupted: {e}");
                    (epoch, None, 0)
                }
            };
            let _ = tx.send(result).await;
        });
    }

    /// Reset on disc change: the epoch invalidates any TOC read still in
    /// flight.
    fn forget_disc(&mut self) {
        self.track = 0;
        // The last **known** TOC is kept: it is what will tell, when the next
        // one arrives, whether the disc changed or the drive simply flickered.
        // Overwriting with `None` would lose that memory — a flicker produces
        // two presence changes, hence two passes through here, and the second
        // would erase what the first had just retained.
        if let Some(known) = self.toc.take() {
            self.previous_toc = Some(known);
        }
        self.total_tracks = 0;
        self.epoch = self.epoch.wrapping_add(1);
    }
}

#[async_trait::async_trait]
impl SourcePlugin for CdSource {
    async fn activate(&mut self) -> SourceOutcome {
        self.playback = self.present;
        if self.present {
            self.issue(SourceAction::play("cdda://").finite())
        } else {
            self.issue(SourceAction::Noop)
        }
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        self.playback = false;
        SourceOutcome::new(SourceAction::Stop).plays_nothing()
    }
    async fn wake(&mut self) -> SourceOutcome {
        // Wake: refresh the display ("no disc" / disc info) without emitting a
        // Play — the cd does not start by itself, so nothing plays and there is
        // no metadata to look up.
        self.playback = false;
        self.issue(SourceAction::Noop)
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        if !self.present || n == 0 {
            return SourceOutcome::new(SourceAction::Noop);
        }
        if self.total_tracks > 0 && (n as usize) > self.total_tracks {
            return self.issue(SourceAction::Noop);
        }
        self.track = (n - 1) as i64;
        self.playback = true;
        self.issue(SourceAction::play(format!("cdda://{n}")).finite())
    }
    async fn next(&mut self) -> SourceOutcome {
        // Nothing playing: `playlist-next` on a stopped mpv loads nothing, so
        // skipping a track makes no sense. Above all, `playback` must not be
        // armed here: that would declare a track in progress on a silent
        // device, make a third-party service get queried, and display an
        // artist and a title without a sound.
        if !self.playback {
            return SourceOutcome::new(SourceAction::Noop);
        }
        // The player does not report the real index: we track the requested
        // index, bounded to the last known track (no wrap-around).
        if self.total_tracks > 0 {
            self.track = (self.track + 1).min(self.total_tracks as i64 - 1);
        }
        self.issue(SourceAction::PlayerNext)
    }
    async fn prev(&mut self) -> SourceOutcome {
        // See `next`: same guard, same reason.
        if !self.playback {
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.track = (self.track - 1).max(0);
        self.issue(SourceAction::PlayerPrev)
    }
    async fn stop(&mut self) -> SourceOutcome {
        // Stop decided by the core, which the Source would not have known
        // otherwise.
        //
        // Goes through `issue()`, like `activate`/`wake`/`select`: a permanent
        // frame without a status ERASES the status memorized on the core side
        // (see `SourceMessage::status`), it does not leave it as is. Before
        // this fix, the screen went blank ("CD" and two empty lines) at the end
        // of the disc as on the Stop key, although the disc remained inserted —
        // see this project's register. `issue()` declares no preset here:
        // `self.playback` has just been set to false, so its `plays_nothing()`
        // branch applies, without `preset`, exactly as before.
        self.playback = false;
        self.issue(SourceAction::Noop)
    }
    async fn player_track(&mut self, n: i64) -> SourceOutcome {
        // The disc advances by itself at the end of a track: this is the
        // **only** path by which the plugin learns it, mpv not reporting the
        // index otherwise. Without this, the display and the metadata stayed on
        // the previous track until the user pressed a key.
        if !self.present || n < 0 {
            return SourceOutcome::new(SourceAction::Noop);
        }
        if self.total_tracks > 0 && n >= self.total_tracks as i64 {
            // Index outside the disc: do not follow a value known to be wrong.
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.track = n;
        // The player announces a track advance: so it is playing, whatever the
        // plugin believed until now. This is also what repairs the state after
        // a presence flicker of the drive.
        self.playback = true;
        self.issue(SourceAction::Noop)
    }
    /// The drive has a tray, disc or not: it is even without a disc that it is
    /// opened most often. Returning `self.present` here would grey out the key
    /// exactly when it is needed.
    fn can_eject(&self) -> bool {
        true
    }

    async fn eject(&mut self) -> SourceOutcome {
        let cd_dev = self.cd_dev.clone();
        // `spawn_blocking` alone is enough: the `eject` command blocks while
        // the tray opens, and the answer to the core does not wait for it. The
        // `JoinHandle` is dropped deliberately — `cd::eject` logs its own
        // failures, there is nothing to collect here.
        tokio::task::spawn_blocking(move || cd::eject(&cd_dev));
        self.present = false;
        self.playback = false;
        self.forget_disc();
        self.issue(SourceAction::Stop)
    }

    async fn set_locale(&mut self, locale: String) {
        self.catalog = Catalog::load("cd", &locale, &self.locales_root, CD_EN);
    }

    async fn poll_notification(&mut self) -> Option<Notification> {
        tokio::select! {
            presence = self.presence_rx.recv() => {
                let present = presence?;
                self.present = present;
                // `playback` is **not** touched here, and that is deliberate:
                // `issue` already requires `playback && present`, so a gone
                // disc announces nothing. Resetting it to false would break the
                // presence-flicker case — the drive transiently reports "no
                // disc" while mpv is still reading, and the disc's metadata
                // would stay off until the end, with nothing to repair it. The
                // disc-swap case is handled when the new TOC arrives: that is
                // the first moment it can be told apart from a flicker.
                self.forget_disc();
                if present {
                    self.spawn_toc_read();
                }
                // An inserted disc does not play yet: `plays_nothing`, via
                // `issue`, which takes `playback` into account.
                Some(self.notification())
            }
            toc = self.toc_rx.recv() => {
                let (epoch, toc, total_tracks) = toc?;
                if epoch != self.epoch {
                    return None;
                }
                self.total_tracks = total_tracks;
                // Disc **different** from the previous one: it was swapped, so
                // nothing can be playing — mpv no longer plays what it was
                // playing, and no `Play` was emitted for this disc. Same TOC:
                // it was a presence flicker of the drive, the playback state is
                // kept and the metadata comes back.
                //
                // The comparison only happens if a previous TOC is known: on
                // the first disc it is `None`, and a playback the user has just
                // started must absolutely not be switched off.
                if let Some(previous) = &self.previous_toc {
                    if Some(previous) != toc.as_ref() {
                        self.playback = false;
                    }
                }
                self.toc = toc;
                // Deferred arrival of the TOC: this is the moment the track
                // becomes identifiable, hence when the `metadata` plugins can
                // finally work — hence the identity in the notification.
                Some(self.notification())
            }
        }
    }
}

impl CdSource {
    /// Spontaneous notification carrying the status **and** the identity, built
    /// from the same outcome as the answers to requests (so as not to have two
    /// identity rules to keep consistent).
    fn notification(&self) -> Notification {
        let issue = self.issue(SourceAction::Noop);
        Notification {
            identity: issue.identity,
            // Never transient: what the cd reports (disc inserted, TOC read,
            // track changed) describes the durable state of the device.
            transient: false,
            preset: issue.preset,
            // The TOC can arrive after activation (async read): without this,
            // the count declared at activation (0, TOC unknown yet) would
            // never be corrected once the disc is actually readable.
            preset_count: issue.preset_count,
            // The cd plugin never names a preset (see `SourceMessage::preset_name`).
            preset_name: issue.preset_name,
            // Same status logic as any other frame: presence flips it.
            status: issue.status,
            // The cd never enumerates named presets: a track has no name
            // without a database. `list_presets` keeps the default empty list,
            // and a spontaneous frame has nothing to republish here.
            presets: None,
            // The cd is not in the scope of that project: it does not declare a
            // cover yet (see `SourceMessage::cover`).
            cover: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let cd_dev = env_or("RITORNELLO_CD_DEV", "/dev/sr0");

    let (presence_tx, presence_rx) = mpsc::channel(8);
    tokio::spawn(cd::watch(PathBuf::from(cd_dev.clone()), presence_tx));

    let (toc_tx, toc_rx) = mpsc::channel::<ReadToc>(4);

    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));

    let source = CdSource {
        cd_dev,
        present: false,
        track: 0,
        toc: None,
        previous_toc: None,
        total_tracks: 0,
        playback: false,
        epoch: 0,
        presence_rx,
        toc_tx,
        toc_rx,
        catalog: Catalog::load("cd", "en", &locales_root, CD_EN),
        locales_root,
    };
    Runtime::from_args()?.source(source)?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::IdentityUpdate;

    fn source_with_channels() -> (CdSource, mpsc::Sender<bool>, mpsc::Sender<ReadToc>) {
        let (presence_tx, presence_rx) = mpsc::channel(8);
        let (toc_tx, toc_rx) = mpsc::channel(4);
        let source = CdSource {
            cd_dev: "/dev/sr0".into(),
            present: true,
            track: 0,
            toc: None,
            previous_toc: None,
            total_tracks: 0,
            playback: false,
            epoch: 5,
            presence_rx,
            toc_tx: toc_tx.clone(),
            toc_rx,
            catalog: Catalog::load("cd", "en", std::path::Path::new("/nonexistent"), CD_EN),
            locales_root: std::path::PathBuf::from("/nonexistent"),
        };
        (source, presence_tx, toc_tx)
    }

    /// Disc read and playing: the state where the identity is complete.
    fn playing_source() -> CdSource {
        let (mut source, _p, _t) = source_with_channels();
        source.toc = Some("3 150 22767 41887 63000".into());
        source.total_tracks = 3;
        source.playback = true;
        source
    }

    #[tokio::test]
    async fn stale_result_ignored_fresh_result_applied() {
        let (mut source, _presence_tx, toc_tx) = source_with_channels();
        // A stale result (epoch 4, while source.epoch == 5) is ignored.
        toc_tx.send((4, Some("9 1 2 3".into()), 99)).await.unwrap();
        let n = source.poll_notification().await;
        assert!(n.is_none(), "a stale result must produce no notification");
        assert_eq!(source.total_tracks, 0, "the state must not be modified by a stale result");
        assert!(source.toc.is_none());

        // An up-to-date result (epoch 5) is applied.
        toc_tx.send((5, Some("12 150 200".into()), 12)).await.unwrap();
        let n = source.poll_notification().await;
        assert!(n.is_some());
        assert_eq!(source.total_tracks, 12);
    }

    #[tokio::test]
    async fn the_toc_arrival_makes_the_track_identifiable() {
        // This is the moment that unblocks the `metadata` plugins: before it,
        // the disc plays but nothing identifies it.
        let (mut source, _p, toc_tx) = source_with_channels();
        source.playback = true;
        let before = source.issue(SourceAction::Noop);
        assert_eq!(before.identity, Some(IdentityUpdate::Nothing), "without a TOC, nothing is identifiable");

        toc_tx.send((5, Some("3 150 22767 41887 63000".into()), 3)).await.unwrap();
        let n = source.poll_notification().await.expect("notification expected");
        assert_eq!(
            n.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({
                "kind": "disc",
                "toc": "3 150 22767 41887 63000",
                "tracks": 3,
                "track": 0,
            })))
        );
        // The TOC arrives asynchronously, after the activation that declared
        // 0 (count unknown): the notification must correct the count,
        // otherwise the displayed window of numbers stays wrong.
        assert_eq!(n.preset_count, Some(3));
    }

    #[tokio::test]
    async fn an_inserted_but_unread_disc_is_not_a_track() {
        let (mut source, presence_tx, _t) = source_with_channels();
        source.present = false;
        presence_tx.send(true).await.unwrap();
        let n = source.poll_notification().await.expect("notification expected");
        assert!(source.present);
        assert_eq!(
            n.identity,
            Some(IdentityUpdate::Nothing),
            "the cd does not start by itself: nothing plays, so nothing to enrich"
        );
    }

    #[tokio::test]
    async fn changing_track_changes_the_identity() {
        let mut source = playing_source();
        let out = source.next().await;
        assert_eq!(out.action, SourceAction::PlayerNext);
        let expected = serde_json::json!({
            "kind": "disc",
            "toc": "3 150 22767 41887 63000",
            "tracks": 3,
            "track": 1,
        });
        assert_eq!(out.identity, Some(IdentityUpdate::Playing(expected)));
    }

    #[tokio::test]
    async fn the_drive_declares_it_can_eject_disc_or_not() {
        // The capability describes the tray, not its content: it is precisely
        // without a disc that the tray gets opened. Deriving it from `present`
        // would grey out the key exactly when it is needed.
        let source = playing_source();
        assert!(source.can_eject());
        let (mut empty, _p, _t) = source_with_channels();
        empty.present = false;
        assert!(empty.can_eject(), "an empty tray opens too");
    }

    #[tokio::test]
    async fn ejecting_declares_that_nothing_plays_any_more() {
        let mut source = playing_source();
        let out = source.eject().await;
        assert_eq!(out.action, SourceAction::Stop);
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
        assert!(source.toc.is_none(), "the ejected disc's TOC must not survive");
    }

    #[tokio::test]
    async fn skipping_a_track_without_playback_in_progress_declares_nothing() {
        // Disc read, but nothing started: `playlist-next` on a stopped mpv
        // loads nothing. Declaring a playback here would make a third-party
        // service get queried and display an artist and a title on a silent
        // device.
        let (mut source, _p, _t) = source_with_channels();
        source.toc = Some("3 150 22767 41887 63000".into());
        source.total_tracks = 3;
        source.playback = false;

        let out = source.next().await;
        assert_eq!(out.action, SourceAction::Noop);
        assert!(out.identity.is_none(), "nothing must be announced to the metadata plugins");
        assert_eq!(source.track, 0, "the index must not move");

        let out = source.prev().await;
        assert_eq!(out.action, SourceAction::Noop);
        assert!(out.identity.is_none());
    }

    #[tokio::test]
    async fn a_stop_decided_by_the_core_updates_the_playback_state() {
        // `Command::Stop` does not go through the Source: without this
        // notification, `playback` would stay true and the plugin would later
        // announce metadata for a stopped track.
        let mut source = playing_source();
        let out = source.stop().await;
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
        assert!(!source.playback);
        // And the consequence: nothing is announced any more, even when a TOC arrives.
        assert_eq!(source.issue(SourceAction::Noop).identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn a_stopped_disc_still_declares_its_status() {
        // Regression I1 (branch review): `stop()` did not go through `issue()`
        // and therefore declared no status. A permanent frame without a status
        // ERASES the status memorized on the core side (documented convention
        // of `SourceMessage::status`): the screen went blank ("CD" and two
        // empty lines) at the end of the disc as on the Stop key, although the
        // disc remained inserted. This guarantee is what makes the mitigation
        // recorded in the register true ("the audio CD status stays
        // displayed"): without it, the owner's ruling on losing the track
        // number at stop rested on a non-existent promise.
        let mut source = playing_source();
        let out = source.stop().await;
        assert_eq!(out.status.as_deref(), Some("audio CD"), "the disc is still present");
        assert_eq!(out.preset, None, "nothing plays: no key must be highlighted");
    }

    #[tokio::test]
    async fn a_stop_without_a_disc_declares_no_disc() {
        let (mut source, _p, _t) = source_with_channels();
        source.present = false;
        let out = source.stop().await;
        assert_eq!(out.status.as_deref(), Some("no disc"));
    }

    #[tokio::test]
    async fn automatic_track_advance_updates_preset_and_identity() {
        // Track EOF: the disc advances without any key being pressed. Before
        // this notification, the display and the metadata stayed on the
        // previous track until the user's next command.
        let mut source = playing_source();
        let out = source.player_track(2).await;
        assert_eq!(source.track, 2);
        // "CD 3/3": the track (preset) and the total (preset_count).
        assert_eq!(out.preset, Some(3));
        assert_eq!(out.preset_count, Some(3));
        assert_eq!(
            out.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({
                "kind": "disc",
                "toc": "3 150 22767 41887 63000",
                "tracks": 3,
                "track": 2,
            })))
        );
    }

    #[tokio::test]
    async fn the_playing_track_is_declared_as_the_active_key() {
        // The current track (0-indexed internally) is the key the UI
        // highlights, whatever its number.
        let mut source = playing_source();
        let out = source.player_track(2).await;
        assert_eq!(out.preset, Some(3));
        // Without playback, no key to highlight.
        source.playback = false;
        assert_eq!(source.issue(SourceAction::Noop).preset, None);
        // Beyond the 9th track, the key still matches: the remote's +10 and
        // the web window give access to it.
        source.playback = true;
        source.total_tracks = 12;
        source.track = 10;
        assert_eq!(source.issue(SourceAction::Noop).preset, Some(11));
    }

    #[test]
    fn the_track_count_follows_the_toc() {
        // Known TOC -> total tracks; no TOC (no disc, or TOC read in
        // progress) -> 0, "nothing to number".
        let mut source = playing_source();
        source.total_tracks = 12;
        assert_eq!(source.issue(SourceAction::Noop).preset_count, Some(12));

        source.toc = None;
        assert_eq!(source.issue(SourceAction::Noop).preset_count, Some(0));
    }

    #[tokio::test]
    async fn a_track_advance_outside_the_disc_or_without_a_disc_is_ignored() {
        let mut source = playing_source();
        // Index beyond the known number of tracks: a value known to be wrong.
        let out = source.player_track(9).await;
        assert!(out.identity.is_none());
        assert_eq!(source.track, 0, "the index must not follow a wrong value");
        // `-1` is what mpv reports when there is no chapter.
        assert!(source.player_track(-1).await.identity.is_none());
        // Without a disc, nothing to track.
        source.present = false;
        assert!(source.player_track(1).await.identity.is_none());
    }

    #[tokio::test]
    async fn a_track_advance_attests_playback() {
        // The player announces the advance: so it is playing, whatever the
        // plugin believed. This is what repairs the state after a presence
        // flicker.
        let (mut source, _p, _t) = source_with_channels();
        source.toc = Some("3 150 22767 41887 63000".into());
        source.total_tracks = 3;
        source.playback = false;
        let out = source.player_track(1).await;
        assert!(source.playback);
        assert!(matches!(out.identity, Some(IdentityUpdate::Playing(_))));
    }

    #[tokio::test]
    async fn a_swapped_disc_does_not_wrongly_switch_playback_off() {
        // Impossible to tell apart before the new TOC arrives: same TOC, it was
        // a drive flicker and playback goes on; different TOC, the disc was
        // swapped and nothing can play — no `Play` was emitted for this one.
        let mut source = playing_source();
        let (toc_tx, toc_rx) = mpsc::channel(4);
        source.toc_tx = toc_tx.clone();
        source.toc_rx = toc_rx;
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;

        // The disc is removed, then **another** one is inserted.
        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        let epoch = source.epoch;
        toc_tx.send((epoch, Some("12 150 200 300".into()), 12)).await.unwrap();
        let n = source.poll_notification().await.expect("notification");

        assert!(!source.playback, "nothing plays: no Play was emitted for this disc");
        assert_eq!(
            n.identity,
            Some(IdentityUpdate::Nothing),
            "announcing an identity would make a third party get queried for a stopped disc"
        );
    }

    #[tokio::test]
    async fn the_same_disc_reread_after_a_flicker_keeps_its_playback() {
        let mut source = playing_source();
        let current_toc = source.toc.clone().expect("toc set by the fixture");
        let (toc_tx, toc_rx) = mpsc::channel(4);
        source.toc_tx = toc_tx.clone();
        source.toc_rx = toc_rx;
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;

        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        let epoch = source.epoch;
        toc_tx.send((epoch, Some(current_toc), 3)).await.unwrap();
        let n = source.poll_notification().await.expect("notification");

        assert!(source.playback, "same disc: playback never stopped");
        assert!(
            matches!(n.identity, Some(IdentityUpdate::Playing(_))),
            "the metadata must come back after a flicker"
        );
    }

    #[tokio::test]
    async fn a_presence_flicker_does_not_switch_the_metadata_off() {
        // The drive may transiently report "no disc" while mpv is still
        // reading. Before the fix, `playback` was reset to false on the return
        // of presence and never re-armed: the disc's metadata stayed off until
        // the end, with nothing to switch it back on.
        let mut source = playing_source();
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;
        presence_tx.send(false).await.unwrap();
        let n = source.poll_notification().await.expect("notification");
        assert_eq!(n.identity, Some(IdentityUpdate::Nothing), "disc gone: nothing plays");

        presence_tx.send(true).await.unwrap();
        let _ = source.poll_notification().await;
        assert!(source.playback, "playback must not have been switched off by the flicker");
    }

    #[tokio::test]
    async fn the_status_uses_the_catalog_after_set_locale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("cd")).unwrap();
        std::fs::write(dir.path().join("cd/fr.toml"), "no_disc = \"PAS DE DISQUE\"\n").unwrap();

        let (mut source, _presence_tx, _toc_tx) = source_with_channels();
        source.present = false;
        source.locales_root = dir.path().to_path_buf();
        source.set_locale("fr".into()).await;
        assert_eq!(source.issue(SourceAction::Noop).status.as_deref(), Some("PAS DE DISQUE"));
    }

    #[tokio::test]
    async fn the_status_declares_the_absence_or_presence_of_a_disc() {
        // This is what the SPA's Player card now displays (see
        // `SourceMessage::status`): "no disc" or "audio CD", depending on
        // `self.present`, on every frame.
        let (mut source, _presence_tx, _toc_tx) = source_with_channels();
        source.present = false;
        let outcome = source.activate().await;
        assert_eq!(outcome.status.as_deref(), Some("no disc"));

        let mut source = playing_source();
        let outcome = source.activate().await;
        assert_eq!(outcome.status.as_deref(), Some("audio CD"));
    }

    #[tokio::test]
    async fn next_increments_bounds_and_returns_the_preset() {
        let mut source = playing_source();
        source.track = 0;
        let out = source.next().await;
        assert_eq!(out.action, SourceAction::PlayerNext);
        assert_eq!(out.preset, Some(2), "the preset must follow the track");
        assert_eq!(source.track, 1);
        // Upper bound: on the last track, next does not wrap around.
        source.track = 2;
        let _ = source.next().await;
        assert_eq!(source.track, 2);
    }

    #[tokio::test]
    async fn prev_decrements_bounded_at_zero() {
        let mut source = playing_source();
        source.track = 1;
        let out = source.prev().await;
        assert_eq!(out.action, SourceAction::PlayerPrev);
        assert_eq!(out.preset, Some(1));
        assert_eq!(source.track, 0);
        // Lower bound: on the first track, prev stays at 0.
        let _ = source.prev().await;
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn wake_refreshes_without_playing() {
        let (mut source, _p, _t) = source_with_channels();
        source.present = false;
        let out = source.wake().await;
        assert_eq!(out.action, SourceAction::Noop, "cd must not play on wake");
        assert_eq!(out.status.as_deref(), Some("no disc"));
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[test]
    fn embedded_en_cd_is_not_empty() {
        assert!(!ritornello_i18n::try_parse(CD_EN).unwrap().is_empty());
    }
}
