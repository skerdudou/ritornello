//! Source plugin "cd": disc presence, playback, current track, eject.
//!
//! It knows **no** metadata provider. What it knows about the disc, it declares
//! in the track identity (the raw TOC and the track index); artist, album and
//! titles come from a `metadata` plugin — for example
//! `ritornello-plugin-musicbrainz` — arbitrated by the core. A slow network
//! call therefore no longer lives in the process that must answer track
//! commands.

mod admin;
mod cd;
// Only compiled under `cargo test`: `ui_placeholder_js` is used nowhere at
// run time in this crate, only by `build.rs` (separate compilation, via
// `include!`) and by its own tests. Compiling it permanently into the binary
// would trigger a `dead_code` that `-D warnings` would refuse (see
// `mpd/src/main.rs`, same trap).
#[cfg(test)]
mod placeholder;
mod state;

use admin::CdAdmin;

use anyhow::Result;
use ritornello_plugin_sdk::{Notification, Runtime, SourceOutcome, SourcePlugin};
use ritornello_proto::SourceAction;
use state::{OnArrival, Remembered};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
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
    /// Shared with the Admin half, which serves it to its page — the same
    /// arrangement as the radio, and for the same reason: `SetLocale` reaches
    /// the Source half only, and a private copy on each side would leave the
    /// page in the old language until the plugin restarted.
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
    /// What to do when the source is arrived at, shared with the Admin half
    /// that writes it. Read at each arrival rather than copied at startup: a
    /// setting changed from the page must apply to the next press, not to the
    /// next reboot.
    on_arrival: Arc<RwLock<OnArrival>>,
    /// Where the setting and the resume point live. Written by this half for
    /// the resume point only, always through `state::update` — the Admin half
    /// writes the setting into the same file.
    state_path: PathBuf,
    /// In-memory copy of the resume point, so an arrival does not read the
    /// disk. Kept in step with the file by `remember`.
    remembered: Option<Remembered>,
}

impl CdSource {
    /// Complete outcome: action, status, preset and identity of what plays.
    fn issue(&self, action: SourceAction) -> SourceOutcome {
        let outcome = SourceOutcome::new(action);
        // The Source's permanent status: what the SPA's Player card now
        // displays (see `SourceMessage::status`).
        let outcome = if self.present {
            outcome.status(self.catalog.read().unwrap().get("cd_audio"))
        } else {
            outcome.status(self.catalog.read().unwrap().get("no_disc"))
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

    /// What the source does when it is arrived at — by the source key
    /// (`Activate`) or by a boot / standby exit (`Wake`), which both land
    /// here on purpose.
    ///
    /// The setting is read at each arrival and never cached in this struct: a
    /// value changed from the page must apply to the next press, not to the
    /// next restart.
    fn arrive(&mut self) -> SourceOutcome {
        let setting = *self.on_arrival.read().unwrap();
        self.start(setting)
    }

    /// The Play key, which is **not** an arrival: the user asked to play, so
    /// something plays.
    ///
    /// The setting is still obeyed on *where* to start — that part of it is
    /// a preference about the disc, not about arriving — but its "play
    /// nothing" answers a question nobody asked here. "Nothing" describes
    /// what an arrival should do; pressing Play is not arriving, and there
    /// is no reading of that key under which playing nothing is right.
    ///
    /// Falling back on the first track rather than on the resume: the two
    /// are the same when a resume point exists, and the first track is the
    /// only answer that needs no memory at all.
    fn play_now(&mut self) -> SourceOutcome {
        let where_to = match *self.on_arrival.read().unwrap() {
            OnArrival::Nothing => OnArrival::FirstTrack,
            elsewhere => elsewhere,
        };
        self.start(where_to)
    }

    /// Shared by both entries above, so the two can never drift on what
    /// "start at track 1" or "resume" means.
    fn start(&mut self, setting: OnArrival) -> SourceOutcome {
        // No disc: nothing can start, whatever the setting says. `playback`
        // goes false so the frame announces a status without an identity —
        // `issue` requires both, and a disc absent from the tray is not a
        // track being played.
        if !self.present {
            self.playback = false;
            return self.issue(SourceAction::Noop);
        }
        match setting {
            OnArrival::Nothing => {
                self.playback = false;
                self.issue(SourceAction::Noop)
            }
            OnArrival::FirstTrack => {
                self.track = 0;
                self.playback = true;
                // The whole disc, exactly as before this setting existed:
                // mpv then exposes the tracks as it prefers, and the plugin
                // learns the index through `player_track`.
                self.issue(SourceAction::play("cdda://").finite())
            }
            OnArrival::LastTrack => {
                let track = self.resume_track();
                self.track = track;
                self.playback = true;
                // 1-based in the URI, and **the same expression as
                // `select`**: `cdda://n` plays from track n to the end of the
                // disc. Going through the same form as a digit pressed on the
                // remote is deliberate — a resume then behaves exactly like a
                // selection, which is one behaviour to understand instead of
                // two.
                self.issue(SourceAction::play(format!("cdda://{}", track + 1)).finite())
            }
        }
    }

    /// The track a resume must start on; `0` — the first — when there is
    /// nothing to resume.
    ///
    /// Three cases fall back to the first track, and they are deliberately
    /// **not** told apart: the setting says start the disc, so the disc
    /// starts.
    /// - nothing remembered yet;
    /// - a different disc in the tray. Applying the remembered number would
    ///   drop the listener into the middle of an unrelated record, or outside
    ///   its track count altogether. This is what the TOC is for, and the
    ///   plugin already reads it to tell a swap from a flicker of the tray;
    /// - the TOC not read yet. This one is a genuine limitation and it is
    ///   worth stating: the read is asynchronous (`spawn_toc_read`), and a
    ///   plugin has no way to ask for playback later — a spontaneous
    ///   notification carries a state, never an action. So a boot whose TOC
    ///   read has not landed yet resumes at the first track. The everyday
    ///   case, pressing the source key on a disc that has been sitting in the
    ///   drive, has had its TOC read long since.
    fn resume_track(&self) -> i64 {
        let (Some(toc), Some(remembered)) = (&self.toc, &self.remembered) else {
            return 0;
        };
        if &remembered.toc != toc {
            return 0;
        }
        // The same TOC is the same disc, so an out-of-range number should not
        // happen — but this file is editable by hand on the device, and a bad
        // value must not send mpv outside the disc.
        if self.total_tracks > 0 && remembered.track >= self.total_tracks as i64 {
            return 0;
        }
        remembered.track.max(0)
    }

    /// Records the track being listened to, so a later resume can find it.
    ///
    /// Called from every path that moves `self.track` while something plays.
    /// Recorded **whatever the setting is**: switching the setting on should
    /// work right away, not from the next track change onwards.
    ///
    /// Nothing is recorded while the TOC is unknown: a track number without
    /// the disc it belongs to is precisely what `resume_track` refuses to
    /// trust.
    fn remember(&mut self) {
        let Some(toc) = self.toc.clone() else {
            return;
        };
        let remembered = Remembered { toc, track: self.track };
        self.remembered = Some(remembered.clone());
        // Logged, never propagated — the same policy as the files plugin's
        // `persist`: a read-only `/var/lib` must cost the resume after a
        // reboot, not the playback in progress.
        if let Err(e) = state::update(&self.state_path, |s| s.remembered = Some(remembered)) {
            tracing::warn!("persisting the current track: {e}");
        }
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
        self.arrive()
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        self.playback = false;
        SourceOutcome::new(SourceAction::Stop).plays_nothing()
    }
    async fn wake(&mut self) -> SourceOutcome {
        // **The same function as `activate`, and that is the point of the
        // setting.** These two used to disagree without anyone having decided
        // it: the source key started track 1 while a boot started nothing,
        // because this method was overridden and the other was not. Whoever
        // owned the appliance was going to be surprised by one of the two.
        // Now a single value governs both, and its default — play nothing —
        // is what the old `wake` did.
        self.arrive()
    }
    async fn play(&mut self) -> SourceOutcome {
        // The only source that needs to override this: for the others,
        // arriving and being told to play are the same thing. See
        // `play_now`.
        self.play_now()
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
        self.remember();
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
        self.remember();
        self.issue(SourceAction::PlayerNext)
    }
    async fn prev(&mut self) -> SourceOutcome {
        // See `next`: same guard, same reason.
        if !self.playback {
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.track = (self.track - 1).max(0);
        self.remember();
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
        // The disc advancing on its own is exactly what a resume must find
        // again: without this, listening straight through an album would
        // remember only the track the listener had picked by hand.
        self.remember();
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
        *self.catalog.write().unwrap() = Catalog::load("cd", &locale, &self.locales_root, CD_EN);
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
                if let Some(previous) = &self.previous_toc
                    && Some(previous) != toc.as_ref()
                {
                    self.playback = false;
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
            // cover yet (see `SourceMessage::cover`), hence no thumbnail
            // either — the pair is never split.
            cover: None,
            cover_thumb: None,
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

    let state_path =
        PathBuf::from(env_or("RITORNELLO_CD_STATE", "/var/lib/ritornello/plugin-cd.json"));
    let persisted = state::load(&state_path);
    // Shared, not copied into each half: the page writes it and the Source
    // half reads it at every arrival, so a change applies to the next press.
    let on_arrival = Arc::new(RwLock::new(persisted.on_arrival));
    let catalog = Arc::new(RwLock::new(Catalog::load("cd", "en", &locales_root, CD_EN)));

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
        catalog: catalog.clone(),
        locales_root: locales_root.clone(),
        on_arrival: on_arrival.clone(),
        state_path: state_path.clone(),
        remembered: persisted.remembered,
    };
    let admin = CdAdmin { state_path, on_arrival, catalog, locales_root };
    Runtime::from_args()?.source(source)?.admin(admin)?.run().await
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
            catalog: Arc::new(RwLock::new(Catalog::load(
                "cd",
                "en",
                std::path::Path::new("/nonexistent"),
                CD_EN,
            ))),
            locales_root: std::path::PathBuf::from("/nonexistent"),
            on_arrival: Arc::new(RwLock::new(OnArrival::default())),
            // A writable path that no test reads: `remember` is called by
            // every track change, and pointing it at an unwritable place
            // would fill the test output with warnings for nothing. The tests
            // that do look at what was persisted set this field to a
            // `TempDir` of their own (see `source_remembering_into`).
            state_path: std::env::temp_dir().join("ritornello-cd-tests").join("plugin-cd.json"),
            remembered: None,
        };
        (source, presence_tx, toc_tx)
    }

    /// A disc read and playing, whose resume point is persisted into a
    /// directory the caller owns — the only way to assert on the file.
    fn source_remembering_into(dir: &tempfile::TempDir) -> CdSource {
        let mut source = playing_source();
        source.state_path = dir.path().join("plugin-cd.json");
        source
    }

    /// The same, arriving with `setting` in force.
    fn source_arriving_with(setting: OnArrival) -> CdSource {
        let source = playing_source();
        *source.on_arrival.write().unwrap() = setting;
        source
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

    #[tokio::test]
    async fn arriving_plays_nothing_by_default() {
        // The owner's decision: starting the drive is a physical act, and the
        // default must not perform it. Note this changes what the source key
        // used to do — it started track 1 — which is the point of the
        // setting.
        let mut source = playing_source();
        source.playback = false;
        let out = source.activate().await;
        assert_eq!(out.action, SourceAction::Noop);
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing), "nothing plays, nothing to enrich");
        assert!(!source.playback);
    }

    #[tokio::test]
    async fn one_setting_governs_the_source_key_and_the_boot_alike() {
        // The whole reason this setting exists. These two used to disagree
        // without anyone deciding it: the key started track 1, a boot started
        // nothing. Whichever of the two the owner had in mind, the other was
        // going to surprise them.
        let expected = SourceAction::play("cdda://").finite();
        let mut by_key = source_arriving_with(OnArrival::FirstTrack);
        assert_eq!(by_key.activate().await.action, expected);
        let mut by_boot = source_arriving_with(OnArrival::FirstTrack);
        assert_eq!(by_boot.wake().await.action, expected);

        let mut by_key = source_arriving_with(OnArrival::Nothing);
        assert_eq!(by_key.activate().await.action, SourceAction::Noop);
        let mut by_boot = source_arriving_with(OnArrival::Nothing);
        assert_eq!(by_boot.wake().await.action, SourceAction::Noop);
    }

    #[tokio::test]
    async fn an_absent_disc_plays_nothing_whatever_the_setting_says() {
        for setting in [OnArrival::Nothing, OnArrival::FirstTrack, OnArrival::LastTrack] {
            let mut source = source_arriving_with(setting);
            source.present = false;
            let out = source.activate().await;
            assert_eq!(out.action, SourceAction::Noop, "{setting:?} on an empty tray");
            assert!(!source.playback, "{setting:?} must not claim a playback");
        }
    }

    #[tokio::test]
    async fn resuming_finds_the_track_back_on_the_same_disc() {
        let mut source = source_arriving_with(OnArrival::LastTrack);
        source.remembered =
            Some(Remembered { toc: "3 150 22767 41887 63000".into(), track: 2 });
        let out = source.activate().await;
        // 1-based in the URI, like a digit pressed on the remote: `cdda://3`
        // plays from track 3 to the end of the disc.
        assert_eq!(out.action, SourceAction::play("cdda://3").finite());
        assert_eq!(source.track, 2);
        assert_eq!(out.preset, Some(3), "the highlighted key must be the resumed track");
    }

    #[tokio::test]
    async fn resuming_on_another_disc_starts_at_the_first_track() {
        // The guard that matters: a track number applied to whatever disc is
        // in the tray would drop the listener into the middle of an unrelated
        // record. The plugin already knows the difference — it reads the TOC.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        source.remembered = Some(Remembered { toc: "9 150 30000 60000".into(), track: 2 });
        let out = source.activate().await;
        assert_eq!(out.action, SourceAction::play("cdda://1").finite());
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn resuming_starts_at_the_first_track_when_nothing_is_known_yet() {
        // Two cases, one behaviour, and it is deliberate: the setting says
        // start the disc, so the disc starts.
        //
        // Nothing remembered — a fresh install:
        let mut fresh = source_arriving_with(OnArrival::LastTrack);
        assert_eq!(fresh.activate().await.action, SourceAction::play("cdda://1").finite());

        // TOC not read yet — the read is asynchronous, and a plugin cannot
        // ask for playback later on (a spontaneous notification carries a
        // state, never an action). So a boot that outruns the TOC read
        // resumes at the first track rather than trusting a number it cannot
        // check.
        let mut unread = source_arriving_with(OnArrival::LastTrack);
        unread.toc = None;
        unread.remembered =
            Some(Remembered { toc: "3 150 22767 41887 63000".into(), track: 2 });
        assert_eq!(unread.activate().await.action, SourceAction::play("cdda://1").finite());
    }

    #[tokio::test]
    async fn resuming_refuses_a_track_outside_the_disc() {
        // The same TOC is the same disc, so this should not happen — but the
        // state file is editable by hand on the device, and mpv must not be
        // sent outside the disc.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        source.remembered =
            Some(Remembered { toc: "3 150 22767 41887 63000".into(), track: 7 });
        assert_eq!(source.total_tracks, 3);
        assert_eq!(source.activate().await.action, SourceAction::play("cdda://1").finite());
    }

    #[tokio::test]
    async fn the_disc_advancing_on_its_own_is_what_a_resume_finds_back() {
        // Without persisting on this path, listening straight through an
        // album would only ever remember the track the listener picked by
        // hand.
        let dir = tempfile::tempdir().unwrap();
        let mut source = source_remembering_into(&dir);
        source.player_track(2).await;
        let persisted = state::load(&source.state_path).remembered.expect("a resume point");
        assert_eq!(persisted.track, 2);
        assert_eq!(persisted.toc, "3 150 22767 41887 63000", "the disc must travel with the track");
    }

    #[tokio::test]
    async fn every_way_of_changing_track_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = source_remembering_into(&dir);
        source.select(3).await;
        assert_eq!(state::load(&source.state_path).remembered.unwrap().track, 2, "select");
        source.next().await;
        assert_eq!(state::load(&source.state_path).remembered.unwrap().track, 2, "next, bounded");
        source.prev().await;
        assert_eq!(state::load(&source.state_path).remembered.unwrap().track, 1, "prev");
    }

    #[tokio::test]
    async fn a_track_is_never_remembered_without_its_disc() {
        // A number alone is exactly what `resume_track` refuses to trust, so
        // recording one would be recording a value we have decided to ignore.
        let dir = tempfile::tempdir().unwrap();
        let mut source = source_remembering_into(&dir);
        source.toc = None;
        source.player_track(2).await;
        assert!(state::load(&source.state_path).remembered.is_none());
        assert!(source.remembered.is_none());
    }

    #[tokio::test]
    async fn the_play_key_starts_the_disc_even_when_arrival_plays_nothing() {
        // The defect this exists to forbid: "play nothing" describes an
        // arrival, and the Play key is not an arrival. With the two sharing
        // one signal, the default setting made that key inert — only a track
        // number could start the disc.
        let mut source = source_arriving_with(OnArrival::Nothing);
        source.playback = false;
        let out = source.play().await;
        assert_eq!(out.action, SourceAction::play("cdda://").finite());
        assert!(source.playback);
        // And arriving still plays nothing: the fix must not have quietly
        // turned the default into "start".
        let mut arriving = source_arriving_with(OnArrival::Nothing);
        assert_eq!(arriving.activate().await.action, SourceAction::Noop);
    }

    #[tokio::test]
    async fn the_play_key_still_obeys_where_to_start() {
        // The half of the setting that is a preference about the disc rather
        // than about arriving: someone who asked to resume expects Play to
        // resume too.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        source.remembered = Some(Remembered { toc: "3 150 22767 41887 63000".into(), track: 2 });
        assert_eq!(source.play().await.action, SourceAction::play("cdda://3").finite());
    }

    #[tokio::test]
    async fn the_play_key_cannot_start_an_empty_tray() {
        let mut source = source_arriving_with(OnArrival::FirstTrack);
        source.present = false;
        let out = source.play().await;
        assert_eq!(out.action, SourceAction::Noop);
        assert!(!source.playback);
    }

    #[tokio::test]
    async fn the_setting_is_read_at_each_arrival_never_cached() {
        // Changing it from the page must apply to the next press, not to the
        // next restart — which is why the Source half holds the shared value
        // and not a copy of it.
        let mut source = source_arriving_with(OnArrival::Nothing);
        assert_eq!(source.activate().await.action, SourceAction::Noop);
        *source.on_arrival.write().unwrap() = OnArrival::FirstTrack;
        assert_eq!(source.activate().await.action, SourceAction::play("cdda://").finite());
    }

    #[test]
    fn embedded_en_cd_is_not_empty() {
        assert!(!ritornello_i18n::try_parse(CD_EN).unwrap().is_empty());
    }
}
