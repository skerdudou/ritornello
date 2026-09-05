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
use rand::seq::SliceRandom;
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
    /// A chapter to apply as soon as mpv confirms the disc is open.
    ///
    /// `cdda://` loads the whole disc — the only URI known to work — so
    /// reaching a specific track (arrival resuming the last one, or a digit
    /// pressed before anything was loaded) takes two steps: load the disc,
    /// then seek. This is what carries the wanted chapter across that gap,
    /// from the moment the `Play` is issued to the moment `player_track`
    /// reports mpv really has something open. `None` once applied, or when
    /// no seek is owed (the disc was already loaded, so a direct
    /// `PlayerChapter` was enough).
    pending_chapter: Option<i64>,
    /// Where the next `order` comes from: a real shuffle in production
    /// (`CdOrder::Random`, built in `main()`), an injected permutation or
    /// queue of permutations in tests (see `CdOrder`'s doc for why this is
    /// its own type rather than sharing `ritornello-plugin-files`'s `Order`
    /// — a disc's tracks are their own numbering, with no separate list of
    /// entries behind them).
    draw: CdOrder,
    /// The two play modes, learned from `set_play_mode` and consulted by
    /// `player_track`/`end_of_content`/`next`/`prev` to know whether a track
    /// number should come from the disc's own order or from `order` below.
    random: bool,
    repeat_all: bool,
    /// The pass currently being walked when `random` is on: a permutation of
    /// `0..total_tracks`, drawn by `set_play_mode` on the transition that
    /// turns shuffle on (empty otherwise — sequential playback never reads
    /// it). `next`/`prev` (a manual skip) and `player_track` (mpv's own
    /// natural advance, corrected toward the pass — see its doc) move
    /// `cursor` through it instead of stepping `track` directly, exactly the
    /// translation `ritornello-plugin-files` keeps between mpv's position
    /// and a playlist entry, here between a position in the pass and a
    /// track number. Cleared where a disc swap is *confirmed* — the TOC
    /// arrival in `poll_notification`, and `eject` — so it never holds a
    /// track number the new disc does not have; deliberately **not**
    /// cleared by `forget_disc`, which runs on every presence change
    /// including a mere flicker (see its own doc for why that distinction
    /// matters here specifically).
    order: Vec<i64>,
    /// The position in `order` currently playing. Distinct from `track`,
    /// which already holds the track number itself: `order[cursor] ==
    /// track` is the invariant `next`/`prev`/`player_track` keep by moving
    /// along it instead of re-deriving a position from a track number every
    /// time — an invariant `select` and a resumed track must restore by
    /// hand (`sync_cursor_to_track`) since they set `track` directly.
    cursor: usize,
}

/// Where a drawn playback order comes from, for the disc's random mode.
///
/// Same discipline as `ritornello-plugin-files`'s `Order` — the draw is
/// **injected**, never left to a real RNG in a test, so that a check
/// against a full pass is exact rather than statistical (see
/// `a_shuffled_disc_plays_every_track_once_then_stops`) — but a distinct
/// type: a disc has no separate list of entries to shuffle behind its
/// tracks, so what gets drawn here is directly a permutation of track
/// numbers, not of positions into some other list.
// `main()` only ever builds `CdOrder::Random`: the other two variants are
// constructed by test code alone, so a plain (non-test) build sees them as
// unconstructed — same situation as `ritornello-plugin-files`'s `Order`.
#[allow(dead_code)]
enum CdOrder {
    /// A single permutation, injected in tests that only ever open one
    /// pass: every draw hands back the same vector.
    Fixed(Vec<i64>),
    /// A queue of permutations, injected in tests that must open a second
    /// pass and prove it was drawn again, not replayed: each draw pops the
    /// next entry. Falls back to the identity permutation once exhausted,
    /// rather than panicking on a test that happens to draw once too often.
    Sequence(Vec<Vec<i64>>),
    /// Production: a fresh Fisher-Yates shuffle against the thread RNG at
    /// every draw.
    Random,
}

impl CdOrder {
    /// Hands back a permutation of `0..total_tracks`, consuming one draw.
    fn draw(&mut self, total_tracks: usize) -> Vec<i64> {
        match self {
            CdOrder::Fixed(order) => order.clone(),
            CdOrder::Sequence(queue) => {
                if queue.is_empty() {
                    (0..total_tracks as i64).collect()
                } else {
                    queue.remove(0)
                }
            }
            CdOrder::Random => {
                let mut order: Vec<i64> = (0..total_tracks as i64).collect();
                order.shuffle(&mut rand::rng());
                order
            }
        }
    }
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
        // Any chapter still owed belonged to whatever this arrival is about
        // to replace (a previous `select` while stopped, an arrival cut
        // short by a disc removal before mpv ever confirmed it). Carrying it
        // over would apply a stale destination to a disc this arrival did
        // not ask for.
        self.pending_chapter = None;
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
                // Under shuffle, "first" is the first entry of the drawn
                // pass (`order`, drawn by `set_play_mode`), not literally
                // track 0 — the random mode decides the order of the pass,
                // this setting only decides that a pass starts at all.
                // Falls back to 0 when nothing has been drawn yet (shuffle
                // never engaged, or the TOC was not yet read at the time it
                // was): a degraded but sane default, the same spirit as
                // `resume_track`'s own "TOC not read yet" fallback.
                let first = self.order.first().copied().unwrap_or(0);
                self.cursor = 0;
                self.track = first;
                self.playback = true;
                // Arms the same way `LastTrack` does, even though 0 is
                // also where mpv is expected to open on its own: without
                // this, `select`/`next`/`prev` pressed before the first
                // notification landed would read `pending_chapter` as
                // `None` and "already open" as true (both set `playback`
                // true ahead of confirmation), and send a seek mpv had
                // nothing to act on yet — the exact failure this field
                // exists to close. Costs nothing on the ordinary path:
                // `player_track`'s own guard already treats `wanted == n`
                // as a no-op seek.
                self.pending_chapter = Some(first);
                // The whole disc, exactly as before this setting existed:
                // mpv then exposes the tracks as it prefers, and the plugin
                // learns the index through `player_track`.
                self.issue(SourceAction::play("cdda://").finite())
            }
            OnArrival::LastTrack => {
                let track = self.resume_track();
                self.track = track;
                // Regression I3 (review 1): resuming used to leave `cursor`
                // wherever the last mode transition had put it, so
                // next/prev under shuffle walked away from the resumed
                // track instead of onward from it.
                self.sync_cursor_to_track();
                self.playback = true;
                // Nothing is loaded on an arrival: `cdda://` is the only URI
                // known to open, and the resumed track is reached afterwards
                // by chapter, once `player_track` confirms mpv really has
                // the disc open (see `pending_chapter`).
                self.pending_chapter = Some(track);
                self.issue(SourceAction::play("cdda://").finite())
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
    ///
    /// Deliberately does **not** touch `pending_chapter`: this runs on
    /// *every* presence change, including a mere flicker of the drive (the
    /// disc reported transiently absent while mpv is still reading it) —
    /// and a flicker is exactly what must not lose a seek owed on a disc
    /// that is still, in fact, on its way in. The place that can tell a
    /// flicker from a real swap is the TOC arrival in `poll_notification`,
    /// and that is where the field is cleared instead.
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
        // Deliberately does **not** touch `order`/`cursor` either, for the
        // exact same reason as `pending_chapter` above: this runs on every
        // presence change, flicker included, and a drawn order has no
        // automatic redraw the way `total_tracks` does (recomputed the
        // moment a TOC returns, flicker or not) — nothing else would
        // restore it. Regression P1 (review 2): an earlier version cleared
        // it here, reasoning it followed `total_tracks`'s own precedent;
        // that precedent does not hold, and the result was a flicker during
        // a shuffled pass silently emptying `order`, after which
        // `next`/`prev` and `player_track`'s correction (both bounded by
        // its length) went quiet and the disc kept playing sequentially —
        // with nothing to redraw it short of a fresh `set_play_mode`. The
        // TOC arrival in `poll_notification` is where a swap is actually
        // told apart from a flicker, and that is where `order`/`cursor` are
        // cleared instead (see its own comment).
    }

    /// `next`/`prev` under shuffle: walks `cursor` within the drawn `order`
    /// by `delta` (`1` or `-1`) instead of stepping the physical track
    /// number — a manual skip, unrelated to the automatic corrections
    /// `player_track` makes when mpv advances on its own (see its doc).
    ///
    /// No wrap-around at either end of the pass, the same convention the
    /// disc's own physical order already keeps in `next`/`prev` — a listener
    /// pressing "next" past the pass's last drawn entry gets silence, not a
    /// jump back to its first one.
    fn step_drawn_order(&mut self, delta: i64) -> SourceOutcome {
        let target = self.cursor as i64 + delta;
        if target < 0 || target as usize >= self.order.len() {
            return self.issue(SourceAction::Noop);
        }
        let position = target as usize;
        // Regression P1 (review 3): this used to be the *only* guard, and
        // it is not enough. The sequential path is protected by
        // `total_tracks` resetting to 0 on every presence change
        // (`forget_disc`, unconditional); this one has no such automatic
        // bound, since `order` deliberately survives a mere flicker (see
        // `forget_disc`'s own doc). A disc pulled through a path this
        // plugin cannot observe as a *confirmed* swap or an explicit
        // `eject` — the drive's own physical eject button, never followed
        // by a fresh TOC read — leaves `order`/`cursor` alive indefinitely,
        // and `playback` is deliberately not reset by a presence change
        // either (see `poll_notification`): without this, a manual
        // next/prev pressed in that window would still emit a real seek
        // toward a track of a disc that is no longer there.
        //
        // Only two conditions, not three (review 4): `self.present` is
        // absent from this check on purpose. `forget_disc` runs on *every*
        // presence change and unconditionally zeroes `total_tracks`, and a
        // TOC read completing later carries the epoch it was spawned
        // with — stale once that reset has bumped `self.epoch` — so
        // `!self.present` never holds while `total_tracks > 0`; it added
        // nothing a `total_tracks == 0` check does not already catch, and
        // no test could tell the two apart. `total_tracks == 0` earns its
        // own line rather than folding into the entry check below (`0` is
        // also a lower bound no drawn entry can ever be under, which would
        // make it true trivially the same way): it names the actual
        // condition this guard cares about — the disc's track count is not
        // presently known — including while the disc is still reported
        // present but its TOC has not landed yet (see
        // `next_and_prev_stay_blocked_while_present_but_the_toc_is_not_yet_known`).
        if self.total_tracks == 0 || self.order[position] >= self.total_tracks as i64 {
            return self.issue(SourceAction::Noop);
        }
        self.cursor = position;
        self.track = self.order[self.cursor];
        self.remember();
        if self.pending_chapter.is_some() {
            // A load is still in flight (see `select`): nothing to seek
            // yet, only the destination the eventual notification will
            // apply changes.
            self.pending_chapter = Some(self.track);
            return self.issue(SourceAction::Noop);
        }
        self.issue(SourceAction::PlayerChapter(self.track))
    }

    /// Keeps `order[cursor] == track` true after `track` was set by
    /// something other than walking the pass — a digit typed by hand
    /// (`select`) or a resumed track (`start`'s `OnArrival::LastTrack`).
    ///
    /// Regression I3 (review 1): without this, `next`/`prev` after either
    /// one kept walking wherever the pass had last been left, sending the
    /// listener toward a track already heard instead of one that follows
    /// what was just picked or resumed.
    ///
    /// Leaves `cursor` untouched when `track` is not found in `order` (no
    /// draw yet, or one cleared by a disc swap — see `forget_disc`): there
    /// is nothing sound to reposition to, and next/prev's own bound on
    /// `order`'s length already turns that into silence rather than a wrong
    /// jump.
    fn sync_cursor_to_track(&mut self) {
        if !self.random {
            return;
        }
        if let Some(position) = self.order.iter().position(|&t| t == self.track) {
            self.cursor = position;
        }
    }
}

#[async_trait::async_trait]
impl SourcePlugin for CdSource {
    async fn activate(&mut self) -> SourceOutcome {
        self.arrive()
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        self.playback = false;
        // Leaving the source behind means whatever seek was owed is owed to
        // nothing any more: the next arrival decides fresh.
        self.pending_chapter = None;
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
        // `cdda://n` would name a *device* called "n" to mpv, not track n:
        // the disc always loads whole, and a track is reached by chapter.
        // Whether that chapter can be sent directly depends on whether mpv
        // already has the disc open — and `pending_chapter` already armed
        // means a load is in flight and unconfirmed: `self.playback` alone
        // cannot tell that apart, since both `select` and `start` set it
        // true *before* mpv has said anything, to keep the identity honest
        // the instant a track is chosen.
        let loading = self.pending_chapter.is_some();
        let already_open = self.playback && !loading;
        self.track = (n - 1) as i64;
        // Regression I3 (review 1): a digit typed mid-pass under shuffle
        // must move the pass to where it was just pointed, or next/prev
        // right after would walk toward wherever the pass had been left —
        // a track already heard, not the one the listener just picked.
        self.sync_cursor_to_track();
        self.playback = true;
        self.remember();
        if already_open {
            self.issue(SourceAction::PlayerChapter(self.track))
        } else if loading {
            // A `Play` already went out for an earlier digit and mpv has not
            // confirmed it yet: sending another would reload a disc that is
            // not even open. Only the destination changes; the first pick
            // is silently overridden by the last one, which is the
            // behaviour the remote's digits imply — the most recent press
            // wins.
            self.pending_chapter = Some(self.track);
            self.issue(SourceAction::Noop)
        } else {
            self.pending_chapter = Some(self.track);
            self.issue(SourceAction::play("cdda://").finite())
        }
    }
    async fn next(&mut self) -> SourceOutcome {
        // Nothing playing: a seek on a stopped mpv loads nothing, so
        // skipping a track makes no sense. Above all, `playback` must not be
        // armed here: that would declare a track in progress on a silent
        // device, make a third-party service get queried, and display an
        // artist and a title without a sound.
        if !self.playback {
            return SourceOutcome::new(SourceAction::Noop);
        }
        // Under shuffle, the disc's own numbering is not what a listener
        // pressing next/prev means: it means the drawn pass. See
        // `step_drawn_order`.
        if self.random {
            return self.step_drawn_order(1);
        }
        // The player does not report the real index: we track the requested
        // index, bounded to the last known track (no wrap-around).
        let before = self.track;
        if self.total_tracks > 0 {
            self.track = (self.track + 1).min(self.total_tracks as i64 - 1);
        }
        self.remember();
        if self.pending_chapter.is_some() {
            // A load is still in flight (see `select`): there is nothing to
            // seek yet, only the destination the eventual notification will
            // apply changes.
            self.pending_chapter = Some(self.track);
            return self.issue(SourceAction::Noop);
        }
        if self.track == before {
            // Already on the last track: a chapter identical to the current
            // one would be a pointless seek, sent for nothing. mpv's
            // `playlist-next` used to be the action here, and it is
            // documented to do nothing on the last (here: the only) entry —
            // this is the same boundary, made explicit instead of relying
            // on mpv's silence.
            return self.issue(SourceAction::Noop);
        }
        self.issue(SourceAction::PlayerChapter(self.track))
    }
    async fn prev(&mut self) -> SourceOutcome {
        // See `next`: same guard, same reason.
        if !self.playback {
            return SourceOutcome::new(SourceAction::Noop);
        }
        if self.random {
            return self.step_drawn_order(-1);
        }
        let before = self.track;
        self.track = (self.track - 1).max(0);
        self.remember();
        if self.pending_chapter.is_some() {
            self.pending_chapter = Some(self.track);
            return self.issue(SourceAction::Noop);
        }
        if self.track == before {
            return self.issue(SourceAction::Noop);
        }
        self.issue(SourceAction::PlayerChapter(self.track))
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
        // Whatever seek was still owed no longer applies to a stopped
        // player: without this, the setting could switch to "start at
        // track 1" and the next arrival would still jump to a track a much
        // earlier, unrelated `select` had armed.
        self.pending_chapter = None;
        self.issue(SourceAction::Noop)
    }
    /// A disc's tracks are exactly the kind of finite list random/repeat-all
    /// are for — unlike the radio, which has none. A constant, not derived
    /// from whether a disc is currently in the tray: an empty tray still has
    /// a shape to shuffle or repeat once a disc is inserted, the same way it
    /// still opens (see `can_eject`'s doc).
    fn has_finite_list(&self) -> bool {
        true
    }

    /// Learns the two play modes together (see `SourcePlugin::set_play_mode`
    /// for why they travel as one call).
    ///
    /// Only draws (and resets the cursor) on an actual **transition** of
    /// `random`, the same guard `ritornello-plugin-files` keeps: the core
    /// rediffuses this request whenever *either* mode changes, and also at
    /// wake-up and hot-plug, so most calls land here with `random`
    /// unchanged — redrawing unconditionally would throw away a pass
    /// already under way for nothing.
    ///
    /// Deliberately does **not** move `track` or emit a seek: this method
    /// has no action to return (see the trait's doc), so a shuffle engaged
    /// mid-playback only prepares the order the *next* pass-boundary event
    /// (`next`/`prev`, or `player_track`'s own correction toward it) will
    /// read — mpv's own position is corrected the moment one of those
    /// actually fires, not immediately.
    async fn set_play_mode(&mut self, random: bool, repeat_all: bool) {
        let random_changed = random != self.random;
        self.random = random;
        self.repeat_all = repeat_all;
        if !random_changed {
            return;
        }
        // Identity when `random` just turned false, so a mode change never
        // leaves a stale shuffled order behind once shuffle is turned back
        // off — mirroring `ritornello-plugin-files`'s own reasoning, even
        // though sequential playback here never reads `order` at all.
        self.order = if random {
            self.draw.draw(self.total_tracks)
        } else {
            (0..self.total_tracks as i64).collect()
        };
        self.cursor = 0;
    }

    /// mpv went idle at the end of the finite list `has_finite_list`
    /// declares.
    ///
    /// Corrected (review 1, C1 — the original brief had this method walk
    /// the drawn pass one track at a time, which does not happen: the whole
    /// disc is **one loaded item**, so mpv never goes idle between its
    /// chapters — it advances to the physically next one on its own, and
    /// that is reported through `player_track`, not here (see its doc for
    /// the shuffle correction it now carries). This fires once, whether
    /// shuffling or not, at the disc's true end — the same convention
    /// `ritornello-plugin-files`'s `end_of_content` documents.
    ///
    /// Without repeat-all, behaves like the default `stop()`. With it,
    /// unlike files (which reissues `Play` to rebuild its m3u), the disc is
    /// already loaded: going back to the first track — the drawn pass's own
    /// first entry when shuffling, a fresh draw so the next pass is drawn
    /// again and not replayed — is a plain chapter seek.
    async fn end_of_content(&mut self) -> SourceOutcome {
        // A real end-of-content cannot fire on a disc mpv has not actually
        // opened yet: any seek still owed from an arrival or a `select` is
        // moot once mpv itself reports the list ran its course.
        self.pending_chapter = None;
        if !self.repeat_all {
            return self.stop().await;
        }
        self.cursor = 0;
        self.track = if self.random {
            // The next pass is drawn again, not replayed: see `CdOrder`'s
            // doc on why `Sequence` exists to prove exactly this.
            self.order = self.draw.draw(self.total_tracks);
            self.order.first().copied().unwrap_or(0)
        } else {
            0
        };
        self.playback = true;
        self.remember();
        self.issue(SourceAction::PlayerChapter(self.track))
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
        // The player announces a track advance: so it is playing, whatever the
        // plugin believed until now. This is also what repairs the state after
        // a presence flicker of the drive.
        self.playback = true;
        // A chapter was owed since the disc was not loaded yet (see
        // `pending_chapter`): this notification is taken as the first proof
        // mpv has it open (unmeasured — see `player::mpv`'s comment on
        // `playlist-pos`/`chapter`), so the wanted chapter can finally be
        // applied — once, not on every later track change, which is why
        // `take()` and not a read.
        //
        // Consumed only once `total_tracks` is known — `self.total_tracks >
        // 0` is deliberately the *first* condition, so `take()` never even
        // runs otherwise. `total_tracks == 0` means which disc this even is
        // is not established yet (the TOC read is asynchronous, and a
        // presence flicker resets it to 0 until the next one lands): a
        // notification can arrive in that window, before the TOC does, and
        // taking the pending value here would throw away the user's
        // intention on a passing ignorance, with no way back — exactly
        // N1's failure, in the other arrival order. Waiting costs nothing:
        // the risk of applying it to the *wrong* disc is already covered by
        // the branch that confirms a different TOC, which disarms it
        // outright (see `poll_notification`) — so by the time `total_tracks`
        // is known again, either this is still the same disc (the wanted
        // chapter is exactly right) or the value is already gone (nothing
        // left here to misapply).
        //
        // Once known, re-checked against `total_tracks` rather than trusted
        // as still valid: `select`'s own out-of-range guard above only
        // fires once `total_tracks` is known, and is silently skipped while
        // it is still 0. A digit pressed in that window can arm a
        // destination the disc turns out too short for once its real TOC
        // lands; this is what actually catches that, not a hypothetical
        // race.
        if self.total_tracks > 0 && self.pending_chapter.is_some() {
            let wanted = self.pending_chapter.take().expect("checked above");
            if wanted < self.total_tracks as i64 && wanted != n {
                self.track = wanted;
                self.remember();
                return self.issue(SourceAction::PlayerChapter(wanted));
            }
            // Confirmed on this very notification — directly (`wanted ==
            // n`), or a now-invalid destination discarded (`wanted >=
            // total_tracks`, see
            // `a_selection_made_before_the_toc_is_read_is_abandoned_if_it_turns_out_invalid`).
            // Either way, this is mpv reporting the disc *just* opened, not
            // yet a natural mid-pass advance under shuffle: falling into
            // the correction below on this same notification would jump
            // away from the pass's own first entry the instant it was
            // confirmed, skipping it without ever having played a note of
            // it.
            self.track = n;
            // P2 (review 2): this branch sets `track` directly rather than
            // through `step_drawn_order`, so it must restore
            // `order[cursor] == track` itself — a discarded, out-of-range
            // destination (the `wanted >= total_tracks` case just above)
            // would otherwise leave `cursor` wherever it was, and the next
            // correction would walk from a position that no longer matches
            // what is actually playing.
            self.sync_cursor_to_track();
            self.remember();
            return self.issue(SourceAction::Noop);
        }
        // Corrected (review 1, C1): a disc's chapters cannot be reordered
        // for mpv the way a playlist can, so under shuffle it is *this*
        // natural advance — not `end_of_content`, which only fires once, at
        // the disc's true physical end — that must be redirected toward the
        // pass's real next entry. Only past the confirmation above: while a
        // load is still in flight, the destination is `pending_chapter`'s
        // job, not this one's.
        //
        // Bounded by `self.order.get`/`total_tracks` (review 1, I2 —
        // corrected review 2, P2: `order` is cleared only on a *confirmed*
        // swap or an explicit `eject`, deliberately not on every presence
        // change, see `forget_disc`'s own doc — so a disc changed through a
        // path neither one observes can leave it holding a track number
        // the disc no longer has): a target outside the disc's own known
        // track count must never turn into a seek; falling through to
        // trust mpv's own report is the safe default.
        if self.random
            && self.total_tracks > 0
            && self.cursor + 1 < self.order.len()
            && let Some(&expected) = self.order.get(self.cursor + 1)
            && expected < self.total_tracks as i64
        {
            self.cursor += 1;
            self.track = expected;
            self.remember();
            if expected == n {
                // mpv's own physically next chapter happens to coincide
                // with the pass's next drawn entry: nothing to correct.
                return self.issue(SourceAction::Noop);
            }
            // The assumed (unmeasured — no CD drive available to confirm
            // it against real hardware) cost of not using an EDL to bound
            // each chapter — rejected because applying it to a different
            // chapter means reloading the disc, the same broken cost
            // `select`'s own doc already rules out: mpv keeps playing `n`
            // physically forward for an instant before this seek lands.
            return self.issue(SourceAction::PlayerChapter(expected));
        }
        self.track = n;
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
        // Unlike `forget_disc`'s own reset, this one is decided by the
        // user: the tray is really opening, not flickering, so any seek
        // still owed is owed to nothing any more — same reasoning the
        // confirmed-swap branch of `poll_notification` applies to `order`
        // and `cursor` below.
        self.pending_chapter = None;
        self.order.clear();
        self.cursor = 0;
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
                //
                // Known gap, left open (review 3): if a `select` arms
                // `pending_chapter` for disc A before A's own *first* TOC
                // read has ever completed, and A is swapped for B before
                // that read lands, `previous_toc` is still `None` — nothing
                // to compare B against, so this branch does not run, and
                // the pending chapter survives to be applied to B if B
                // happens to have enough tracks. The window is short (one
                // disc's TOC read, not yet finished) and a core restart
                // would likely close it anyway, but it is real: this is
                // the price of moving the reset here instead of clearing it
                // unconditionally on every disc change, which is what N1
                // ruled out for the flicker case.
                if let Some(previous) = &self.previous_toc
                    && Some(previous) != toc.as_ref()
                {
                    self.playback = false;
                    // This is the moment a swap is told apart from a mere
                    // flicker (see `forget_disc`): only now is it certain
                    // that whatever seek was owed belonged to a disc that
                    // is really gone. Clearing it earlier, on every
                    // presence change, would also clear it on a flicker —
                    // losing a resume still in flight, silently, the next
                    // track notification then landing on track 1 and
                    // overwriting the real resume point with it.
                    self.pending_chapter = None;
                    // Same reasoning, same place (regression P1, review 2):
                    // a drawn order surviving a swap could hold track
                    // numbers the new disc does not have, so it is cleared
                    // here — confirmed swap, not a flicker — and nowhere
                    // earlier (see `forget_disc`'s own doc for the version
                    // of this that clears it on every presence change, and
                    // why that broke shuffle across a mere flicker).
                    self.order.clear();
                    self.cursor = 0;
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
        pending_chapter: None,
        draw: CdOrder::Random,
        random: false,
        repeat_all: false,
        order: Vec::new(),
        cursor: 0,
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
            pending_chapter: None,
            // Unused unless a test overrides it (see
            // `source_with_disc_and_order`/`source_with_disc_and_draw_queue`):
            // no test relies on `main()`'s own `CdOrder::Random` producing a
            // particular value, since that would be statistical rather than
            // exact.
            draw: CdOrder::Fixed(Vec::new()),
            random: false,
            repeat_all: false,
            order: Vec::new(),
            cursor: 0,
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

    /// The same, arriving with `setting` in force — **not yet arrived**:
    /// `playback` starts false, as it does before any `activate`/`wake`/`play`
    /// has run. Every existing test that cares about the post-arrival state
    /// already calls one of those, which recomputes `playback` from scratch;
    /// the tests added for task 6 call `select`/`activate` directly on this
    /// fixture and depend on finding "nothing loaded" here, matching a real
    /// arrival.
    fn source_arriving_with(setting: OnArrival) -> CdSource {
        let mut source = playing_source();
        source.playback = false;
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

    /// Arms a resume point directly, without going through a real track
    /// change. `track` must be a valid index on the fixture's own TOC (3
    /// tracks): widening `total_tracks` to fit an arbitrary value would make
    /// the fixture disagree with the TOC it carries, and test nothing that
    /// `resuming_refuses_a_track_outside_the_disc` does not already cover on
    /// its own.
    fn remember_track(source: &mut CdSource, track: i64) {
        let toc = source.toc.clone().expect("fixture carries a toc");
        source.remembered = Some(Remembered { toc, track });
    }

    /// A disc with `tracks` tracks, read and not yet arrived (`playback`
    /// false, matching `source_arriving_with`'s own starting point). The
    /// TOC's actual content does not matter to any of these tests: every
    /// guard in this file checks `total_tracks`, not the string itself.
    fn disc_with(tracks: i64) -> CdSource {
        let mut source = source_arriving_with(OnArrival::FirstTrack);
        source.total_tracks = tracks as usize;
        source.toc = Some(format!("{tracks} 150"));
        source
    }

    /// A disc wired to draw a fixed order for its random pass — the same
    /// discipline `ritornello-plugin-files`'s `Order` follows for shuffle,
    /// injected here too so a full pass is proven exact rather than
    /// statistical (see `CdOrder`'s doc for why this is its own type).
    fn source_with_disc_and_order(tracks: i64, order: Vec<i64>) -> CdSource {
        let mut source = disc_with(tracks);
        source.draw = CdOrder::Fixed(order);
        source
    }

    /// The same, wired to draw a queue of permutations — for the one test
    /// that must prove a second pass under repeat-all is drawn again, not
    /// replayed (see `CdOrder::Sequence`'s doc).
    fn source_with_disc_and_draw_queue(tracks: i64, queue: Vec<Vec<i64>>) -> CdSource {
        let mut source = disc_with(tracks);
        source.draw = CdOrder::Sequence(queue);
        source
    }

    /// A disc with no injected draw — for tests that only exercise
    /// `repeat_all` under sequential order, or the capability flag, where
    /// the draw mechanism never runs.
    async fn source_with_disc(tracks: usize) -> CdSource {
        disc_with(tracks as i64)
    }

    #[tokio::test]
    async fn a_shuffled_pass_advances_through_player_track_and_covers_every_track_once() {
        // Regression C1 (review 1): the original version of this test drove
        // the pass through `end_of_content`, which does not happen — the
        // whole disc is one loaded item, so mpv never goes idle between
        // chapters; it advances to the physically next one on its own, and
        // the core reports *that* as `player_track`. This is the path that
        // actually carries a shuffled pass, and no test exercised it before
        // this one — which is why the defect passed review.
        //
        // The physical values fed below are deliberately **not** a
        // no-repeat traversal of their own (`1` three times): a plugin that
        // merely accepted mpv's report (`self.track = n`) would produce
        // `[2, 1, 1, 1]` here, not each track once — only a correction
        // toward the drawn order does.
        let mut s = source_with_disc_and_order(4, vec![2, 0, 3, 1]);
        s.set_play_mode(true, false).await;
        let arrival = s.activate().await;
        assert_eq!(arrival.action, SourceAction::play("cdda://").finite());
        // mpv confirms the disc open at its own first physical chapter:
        // the already-armed `pending_chapter` (see `start`) turns that into
        // the pass's real first entry, not yet a correction (see
        // `player_track`'s doc on why this one notification is exempt).
        assert_eq!(s.player_track(0).await.action, SourceAction::PlayerChapter(2));

        let mut seen = vec![s.track];
        for physically_next in [1, 1, 1] {
            match s.player_track(physically_next).await.action {
                SourceAction::PlayerChapter(t) => seen.push(t),
                SourceAction::Noop => seen.push(s.track),
                other => panic!("{other:?}"),
            }
        }
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2, 3], "each track exactly once");
    }

    #[tokio::test]
    async fn a_shuffled_pass_without_repeat_stops_at_the_disc_true_end() {
        // `end_of_content` fires once, at the disc's real physical end —
        // corrected (C1) from walking the pass itself, which `player_track`
        // now does.
        let mut s = source_with_disc_and_order(4, vec![2, 0, 3, 1]);
        s.set_play_mode(true, false).await;
        s.activate().await;
        assert!(matches!(s.end_of_content().await.action, SourceAction::Noop));
    }

    #[tokio::test]
    async fn repeat_all_reloads_the_disc_from_the_first_track() {
        let mut s = source_with_disc(3).await;
        s.set_play_mode(false, true).await;
        s.activate().await;
        assert_eq!(s.end_of_content().await.action, SourceAction::PlayerChapter(0));
    }

    #[tokio::test]
    async fn the_disc_declares_a_finite_list() {
        assert!(source_with_disc(3).await.has_finite_list());
    }

    #[tokio::test]
    async fn repeat_all_under_shuffle_draws_a_fresh_order_for_the_next_pass() {
        // The same trap `ritornello-plugin-files` guards against (see its
        // `repeat_all_opens_a_new_pass_with_a_new_draw`): a `Fixed` order
        // would pass a test like this one just by replaying the same
        // permutation forever. Only a queue proves the second pass was
        // actually drawn again, not replayed.
        let mut s = source_with_disc_and_draw_queue(3, vec![vec![2, 0, 1], vec![1, 2, 0]]);
        s.set_play_mode(true, true).await;
        s.activate().await;
        // `end_of_content` fires once, at the disc's true end (see C1):
        // the first drawn pass's interior is `player_track`'s concern, not
        // exercised here.
        let out = s.end_of_content().await;
        assert_eq!(
            out.action,
            SourceAction::PlayerChapter(1),
            "first entry of the freshly drawn second pass"
        );
        assert_eq!(s.order, vec![1, 2, 0], "the next pass is drawn again, not replayed");
    }

    #[tokio::test]
    async fn a_flicker_during_a_shuffled_pass_leaves_the_drawn_order_intact() {
        // Regression P1 (review 2): an earlier version cleared `order` and
        // `cursor` in `forget_disc`, which runs on *every* presence change
        // — the exact reason this same function already had to give up
        // clearing `pending_chapter` a few rounds ago (see its own doc). A
        // test that calls `forget_disc` directly would only prove the
        // clearing itself works, not that it runs in the right place — the
        // same lesson as C1: for a mechanism triggered by an event, the
        // test must start from the event.
        let mut source = playing_source();
        source.random = true;
        source.order = vec![2, 0, 1];
        source.cursor = 0;
        source.track = 2;

        let (toc_tx, toc_rx) = mpsc::channel(4);
        source.toc_tx = toc_tx.clone();
        source.toc_rx = toc_rx;
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;

        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        // The TOC confirms it really was a flicker: same disc as before.
        let epoch = source.epoch;
        let toc = "3 150 22767 41887 63000".to_string();
        toc_tx.send((epoch, Some(toc), 3)).await.unwrap();
        source.poll_notification().await;

        assert_eq!(source.order, vec![2, 0, 1], "a flicker must not clear the drawn order");
        assert_eq!(source.cursor, 0);
        // And the pass still corrects afterward, proving the order was not
        // merely left in place but still usable.
        assert_eq!(source.player_track(1).await.action, SourceAction::PlayerChapter(0));
    }

    #[tokio::test]
    async fn a_confirmed_disc_swap_clears_the_drawn_order() {
        // Regression I2 (review 1), moved to the branch that actually
        // confirms a swap (P1, review 2): a stale order surviving one could
        // hold track numbers that do not exist on the new disc. Cleared in
        // the same branch, and for the same reason, `pending_chapter`
        // already is (see `a_confirmed_disc_swap_disarms_a_pending_chapter`).
        let mut source = playing_source();
        source.random = true;
        source.order = vec![2, 0, 1];
        source.cursor = 1;

        let (toc_tx, toc_rx) = mpsc::channel(4);
        source.toc_tx = toc_tx.clone();
        source.toc_rx = toc_rx;
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;

        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        // A genuinely different disc, not the one the order was drawn for.
        let epoch = source.epoch;
        let toc = "5 150 20000 40000 60000 80000 100000".to_string();
        toc_tx.send((epoch, Some(toc), 5)).await.unwrap();
        source.poll_notification().await;

        assert!(source.order.is_empty());
        assert_eq!(source.cursor, 0);
    }

    #[tokio::test]
    async fn ejecting_clears_the_drawn_order() {
        // P3 (review 3): symmetric to the confirmed-swap test above, which
        // had one; `eject`'s own reset did not.
        let mut source = playing_source();
        source.random = true;
        source.order = vec![2, 0, 1];
        source.cursor = 1;
        source.eject().await;
        assert!(source.order.is_empty());
        assert_eq!(source.cursor, 0);
    }

    #[tokio::test]
    async fn next_and_prev_do_not_seek_after_the_disc_is_pulled_without_a_confirmed_swap() {
        // Regression P1 (review 3): fixing where `order` gets cleared (P1,
        // review 2) removed an accidental protection along with the bug —
        // while `order` was cleared on *every* presence change, a pulled
        // disc left nothing for `step_drawn_order` to walk. Now that it
        // only clears on a confirmed swap or an explicit `eject`, a disc
        // pulled through neither path (the drive's own physical eject
        // button, never followed by a fresh TOC read) leaves `order` and
        // `cursor` alive — and `playback` is not reset by a presence change
        // either (see `poll_notification`'s own doc) — so nothing used to
        // stop a manual next/prev from emitting a real seek toward a track
        // that is no longer there. The sequential path never had this
        // hole: `total_tracks` resets to 0 on every presence change, and
        // `next`/`prev`'s own bound on it already caught it there.
        //
        // What actually blocks it below is `total_tracks == 0` — a
        // presence change always zeroes it (`forget_disc`) — not a check on
        // `self.present` directly, which this guard does not carry (see
        // `step_drawn_order`'s own comment on why not). This test alone
        // cannot tell the two apart, since both are true together here;
        // `next_and_prev_stay_blocked_while_present_but_the_toc_is_not_yet_known`,
        // right below, at least separates "present" from "track count
        // known" — though no test data can go further and isolate
        // `total_tracks == 0` from the entry-bound condition too (see that
        // test's own comment on why not).
        let mut source = playing_source();
        source.random = true;
        source.order = vec![2, 0, 1];
        source.cursor = 1;
        source.track = 0;

        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;
        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;

        assert_eq!(source.next().await.action, SourceAction::Noop);
        assert_eq!(source.prev().await.action, SourceAction::Noop);
    }

    #[tokio::test]
    async fn next_and_prev_stay_blocked_while_present_but_the_toc_is_not_yet_known() {
        // Review 4/5: a regression fixture for the window this guard must
        // cover, not a proof that `total_tracks == 0` is the only thing
        // blocking here — no test data can isolate that condition from the
        // entry-bound one below it, and that is not for lack of trying:
        // every entry in `order` is a non-negative track index, so once
        // `total_tracks` is 0 the entry check already reads `index >= 0`,
        // trivially true on its own (the same fact `step_drawn_order`'s own
        // comment admits two paragraphs up). Removing `total_tracks == 0`
        // from the guard leaves this test green too. What it does fix in
        // place: the disc **present**, but its track count not yet known —
        // the flicker window between a presence return and its TOC
        // actually landing (see `an_inserted_but_unread_disc_is_not_a_track`
        // for the same window elsewhere in this file) — must not let a
        // manual next/prev through.
        let mut source = playing_source();
        source.random = true;
        source.order = vec![2, 0, 1];
        source.cursor = 1;
        source.track = 0;
        source.total_tracks = 0;
        source.toc = None;

        assert_eq!(source.next().await.action, SourceAction::Noop);
        assert_eq!(source.prev().await.action, SourceAction::Noop);
    }

    #[tokio::test]
    async fn a_correction_pointing_outside_the_known_disc_is_not_applied() {
        // Defense in depth for I2: even if a stale `order` survived somehow
        // (its own clearing is the confirmed-swap branch's job, proven
        // above), a target beyond the disc's own known track count must
        // never be sent to mpv as a seek.
        let mut source = playing_source();
        source.total_tracks = 2;
        source.toc = Some("2 150".into());
        source.random = true;
        source.order = vec![0, 5]; // 5 does not exist on a 2-track disc
        source.cursor = 0;
        source.track = 0;

        let out = source.player_track(1).await;
        assert_eq!(out.action, SourceAction::Noop, "no correction applied outside the known disc");
        assert_eq!(source.track, 1, "falls back to trusting mpv's own report");
    }

    #[tokio::test]
    async fn a_discarded_pending_chapter_still_resyncs_the_cursor_under_shuffle() {
        // P2 (review 2): the branch of `player_track` that discards an
        // out-of-range `pending_chapter` sets `track` directly (mpv's own
        // report), not through `step_drawn_order` — so it must resync
        // `cursor` itself, or a later correction would walk from wherever
        // the pass had last been rather than from the track just accepted.
        let mut source = playing_source();
        source.total_tracks = 4;
        source.toc = Some("4 150".into());
        source.random = true;
        source.order = vec![2, 0, 3, 1];
        source.cursor = 0;
        source.pending_chapter = Some(9); // out of range on a 4-track disc

        let out = source.player_track(0).await;
        assert_eq!(out.action, SourceAction::Noop);
        assert_eq!(source.track, 0);
        assert_eq!(source.cursor, 1, "cursor resynced to where track 0 sits in the drawn order");

        // Proves the resync happened, not just its value: from cursor 1 the
        // next expected entry is `order[2] == 3`; a cursor left at 0 would
        // have expected `order[1] == 0` instead and corrected to that
        // different track.
        assert_eq!(source.player_track(1).await.action, SourceAction::PlayerChapter(3));
    }

    #[tokio::test]
    async fn selecting_a_track_while_shuffling_repositions_the_cursor_there() {
        // Regression I3 (review 1): without this, `next`/`prev` after a
        // digit typed by hand kept walking wherever the pass had last been
        // left, sending the listener toward a track already heard rather
        // than one that follows what was just picked.
        let mut source = playing_source();
        source.total_tracks = 4;
        source.toc = Some("4 150".into());
        source.random = true;
        source.order = vec![2, 0, 3, 1];
        source.cursor = 0;
        source.track = 2;

        // Picks track 4 (index 3), which sits at position 2 of the drawn
        // order (`order[2] == 3`).
        source.select(4).await;
        assert_eq!(source.cursor, 2);
        assert_eq!(source.next().await.action, SourceAction::PlayerChapter(1));
    }

    #[tokio::test]
    async fn resuming_while_shuffling_repositions_the_cursor_to_the_remembered_track() {
        // Same regression (I3), the resume half: `start`'s `LastTrack` arm
        // also sets `track` directly.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        source.total_tracks = 4;
        source.toc = Some("4 150".into());
        source.random = true;
        source.order = vec![2, 0, 3, 1];
        remember_track(&mut source, 3);

        source.activate().await;
        assert_eq!(source.cursor, 2, "track 3 sits at position 2 of the drawn order");
        // mpv confirms the disc open exactly at the resumed chapter: the
        // armed `pending_chapter` resolves without a correction (see
        // `player_track`'s doc on why this one notification is exempt) —
        // only past it does `next` mean anything to seek.
        source.player_track(3).await;
        assert_eq!(source.next().await.action, SourceAction::PlayerChapter(1));
    }

    #[tokio::test]
    async fn next_and_prev_follow_the_drawn_order_under_shuffle() {
        // The trap the pass mechanism exists to avoid: a naive next/prev
        // bounded on the physical track number would go from 2 to 3 (its
        // physical neighbour); the drawn order says the entry after 2 is 0.
        let mut source = playing_source();
        source.total_tracks = 4;
        source.toc = Some("4 150".into());
        source.random = true;
        source.order = vec![2, 0, 3, 1];
        source.cursor = 0;
        source.track = 2;

        assert_eq!(source.next().await.action, SourceAction::PlayerChapter(0));
        assert_eq!(source.next().await.action, SourceAction::PlayerChapter(3));
        assert_eq!(source.prev().await.action, SourceAction::PlayerChapter(0));
    }

    #[tokio::test]
    async fn next_and_prev_do_not_wrap_the_drawn_order() {
        let mut source = playing_source();
        source.total_tracks = 4;
        source.toc = Some("4 150".into());
        source.random = true;
        source.order = vec![2, 0, 3, 1];

        source.cursor = 0;
        source.track = 2;
        assert_eq!(source.prev().await.action, SourceAction::Noop, "already at the first drawn entry");

        source.cursor = 3;
        source.track = 1;
        assert_eq!(source.next().await.action, SourceAction::Noop, "already at the last drawn entry");
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
        assert_eq!(out.action, SourceAction::PlayerChapter(1));
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
        // Disc read, but nothing started: seeking a chapter on a stopped
        // mpv has nothing to seek. Declaring a playback here would make a
        // third-party service get queried and display an artist and a
        // title on a silent device.
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
        assert_eq!(out.action, SourceAction::PlayerChapter(1));
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
        assert_eq!(out.action, SourceAction::PlayerChapter(0));
        assert_eq!(out.preset, Some(1));
        assert_eq!(source.track, 0);
        // Lower bound: on the first track, prev stays at 0.
        let _ = source.prev().await;
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn selecting_a_track_seeks_the_chapter_instead_of_opening_a_device() {
        // `cdda://3` names a *device* called "3" in mpv, not track 3: the old
        // form asked libcdio to open a drive that does not exist. Tracks are
        // chapters of the one loaded entry, and chapters are zero-based where
        // the remote's digits are one-based.
        let mut source = playing_source();
        let out = source.select(3).await;
        assert_eq!(out.action, SourceAction::PlayerChapter(2));
    }

    #[tokio::test]
    async fn selecting_while_nothing_plays_loads_the_disc_then_seeks() {
        // A chapter loads nothing: with the disc not open, the digit has to
        // load it first and the position has to wait for the player to say it
        // is there.
        let mut source = source_arriving_with(OnArrival::Nothing);
        let out = source.select(3).await;
        assert_eq!(out.action, SourceAction::play("cdda://").finite());
        // The player reports the disc opened at its first track; only then does
        // the wanted chapter get applied.
        let out = source.player_track(0).await;
        assert_eq!(out.action, SourceAction::PlayerChapter(2));
        // And it is applied once, not on every later track change: `1` here
        // (not the `2` just applied) is deliberate — with `wanted == 2`
        // still equal to the notified index, a mutation that replaced
        // `take()` with a non-consuming read would pass just as well, since
        // the "already there" guard alone would produce the same `Noop`. A
        // later, *different* index is the only way to prove the pending
        // value was actually consumed rather than merely matched.
        assert_eq!(source.player_track(1).await.action, SourceAction::Noop);
        assert_eq!(source.track, 1);
    }

    #[tokio::test]
    async fn a_pending_chapter_matching_the_notified_track_is_a_noop() {
        // The other half of the guard in `player_track`: when the wanted
        // chapter happens to be exactly where mpv opened (a fresh install
        // resuming at track 0, which is also mpv's own opening chapter),
        // applying it again would be a pointless seek to where playback
        // already is.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        // Nothing remembered: `resume_track` falls back to 0, arming
        // `pending_chapter` with the same value mpv will report on its own.
        let _ = source.activate().await;
        let out = source.player_track(0).await;
        assert_eq!(out.action, SourceAction::Noop);
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn resuming_the_last_track_goes_through_the_disc_and_a_chapter() {
        // The resume path was built on the same broken URI as `select`, so it
        // could never have worked on the device — including its "nothing
        // remembered" fallback, which emitted `cdda://1`.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        remember_track(&mut source, 2);
        let out = source.activate().await;
        assert_eq!(out.action, SourceAction::play("cdda://").finite());
        assert_eq!(source.player_track(0).await.action, SourceAction::PlayerChapter(2));
    }

    #[tokio::test]
    async fn a_stop_disarms_a_pending_chapter() {
        // Regression (review 1, I1): nothing cleared `pending_chapter` on a
        // stop. A resume that armed it, followed by the core's own `Stop`
        // (disc removed, or simply the Stop key) before mpv ever confirmed
        // the disc was open, left the seek waiting for a track notification
        // that stop just made moot — and a later, unrelated arrival would
        // have inherited and applied a stale destination.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        remember_track(&mut source, 2);
        source.activate().await;
        assert_eq!(source.pending_chapter, Some(2));
        source.stop().await;
        assert_eq!(source.pending_chapter, None);
        assert_eq!(source.player_track(0).await.action, SourceAction::Noop);
    }

    #[tokio::test]
    async fn an_arrival_disarms_whatever_was_pending_before_it() {
        // Regression (review 2, N3): the unconditional reset at the top of
        // `start` had no test of its own. A destination armed by whatever
        // came before (an earlier session, a `select` on a since-replaced
        // disc) must not survive into an arrival that plays nothing.
        let mut source = source_arriving_with(OnArrival::Nothing);
        source.pending_chapter = Some(4);
        let out = source.activate().await;
        assert_eq!(out.action, SourceAction::Noop);
        assert_eq!(source.pending_chapter, None);
    }

    #[tokio::test]
    async fn deactivating_disarms_a_pending_chapter() {
        // Regression (review 2, N3): `deactivate`'s own reset had no test.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        remember_track(&mut source, 2);
        source.activate().await;
        assert_eq!(source.pending_chapter, Some(2));
        source.deactivate().await;
        assert_eq!(source.pending_chapter, None);
    }

    #[tokio::test]
    async fn a_presence_flicker_while_a_chapter_is_pending_still_lets_it_apply() {
        // Regression (review 2, N1): `forget_disc` runs on *every* presence
        // change, including a mere flicker of the drive — it transiently
        // reporting "no disc" while mpv is still opening it — and used to
        // clear `pending_chapter` there too. A flicker during a resume then
        // lost the destination silently: the next track notification
        // landed on track 0, and `remember()` overwrote the real resume
        // point with it.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        remember_track(&mut source, 2);
        let out = source.activate().await;
        assert_eq!(out.action, SourceAction::play("cdda://").finite());
        assert_eq!(source.pending_chapter, Some(2));

        // The drive flickers while mpv is still opening the disc.
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;
        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        assert_eq!(source.pending_chapter, Some(2), "a flicker must not disarm it");

        // The TOC confirms it really was a flicker: same disc as before.
        let epoch = source.epoch;
        let toc = "3 150 22767 41887 63000".to_string();
        source.toc_tx.clone().send((epoch, Some(toc), 3)).await.unwrap();
        source.poll_notification().await;
        assert_eq!(
            source.pending_chapter,
            Some(2),
            "a confirmed flicker must not disarm it either"
        );

        // mpv finally reports the disc open: the resume is still owed, and applied.
        assert_eq!(source.player_track(0).await.action, SourceAction::PlayerChapter(2));
    }

    #[tokio::test]
    async fn a_track_notification_before_the_toc_catches_up_does_not_lose_the_pending_chapter() {
        // Regression (review 3, P1): the same silent loss as N1, in the
        // other arrival order. A presence flicker resets `total_tracks` to
        // 0 (`forget_disc`), and the TOC re-read is asynchronous: if mpv's
        // own track notification reaches the plugin before that re-read
        // completes, `pending_chapter` used to be consumed (`take()`) and
        // thrown away regardless, because the `total_tracks` bound was
        // checked *after* the value was already gone.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        remember_track(&mut source, 2);
        source.activate().await;
        assert_eq!(source.pending_chapter, Some(2));

        // The drive flickers, resetting `total_tracks` to 0.
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;
        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        assert_eq!(source.total_tracks, 0);

        // mpv's notification arrives before the TOC re-read does: nothing
        // can be judged yet, so nothing is consumed.
        let out = source.player_track(0).await;
        assert_eq!(out.action, SourceAction::Noop);
        assert_eq!(
            source.pending_chapter,
            Some(2),
            "the intention must survive an ignorance the plugin cannot yet resolve"
        );

        // The TOC catches up: same disc as before.
        let epoch = source.epoch;
        let toc = "3 150 22767 41887 63000".to_string();
        source.toc_tx.clone().send((epoch, Some(toc), 3)).await.unwrap();
        source.poll_notification().await;
        assert_eq!(source.pending_chapter, Some(2));

        // The next notification finally applies it.
        assert_eq!(source.player_track(0).await.action, SourceAction::PlayerChapter(2));
    }

    #[tokio::test]
    async fn a_confirmed_disc_swap_disarms_a_pending_chapter() {
        // Regression (review 3, P2): moving the reset (N1) split it across
        // two branches — a confirmed swap, and `eject` — and neither had a
        // test proving it actually runs there.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        remember_track(&mut source, 2);
        source.activate().await;
        assert_eq!(source.pending_chapter, Some(2));

        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;
        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;

        // A genuinely different disc, not the one the resume was armed for.
        let epoch = source.epoch;
        let toc = "5 150 20000 40000 60000 80000 100000".to_string();
        source.toc_tx.clone().send((epoch, Some(toc), 5)).await.unwrap();
        source.poll_notification().await;
        assert_eq!(source.pending_chapter, None);

        // Nothing jumps anywhere on the following notification.
        assert_eq!(source.player_track(0).await.action, SourceAction::Noop);
    }

    #[tokio::test]
    async fn ejecting_disarms_a_pending_chapter() {
        // Regression (review 3, P2): same reason as the swap test above —
        // `eject`'s own reset had no test of its own either.
        let mut source = source_arriving_with(OnArrival::LastTrack);
        remember_track(&mut source, 2);
        source.activate().await;
        assert_eq!(source.pending_chapter, Some(2));
        source.eject().await;
        assert_eq!(source.pending_chapter, None);
    }

    #[tokio::test]
    async fn selecting_before_the_first_notification_lands_arms_first_track_too() {
        // Regression (review 2, N2): `FirstTrack` used to arm nothing, so a
        // digit pressed before the first notification landed read
        // `pending_chapter` as `None` and `playback` as already true (both
        // set ahead of confirmation) — "already open" — and sent a seek
        // mpv had nothing to act on yet, exactly the failure `pending_chapter`
        // exists to close. `FirstTrack` now arms `Some(0)` too, even though
        // 0 is also where mpv is expected to open unprompted; the
        // `wanted == n` guard in `player_track` (proven by
        // `a_pending_chapter_matching_the_notified_track_is_a_noop`) means
        // this costs nothing on the path where nobody presses a digit.
        let mut source = source_arriving_with(OnArrival::FirstTrack);
        let arrival = source.activate().await;
        assert_eq!(arrival.action, SourceAction::play("cdda://").finite());
        let out = source.select(3).await;
        assert_eq!(out.action, SourceAction::Noop, "a load is already in flight");
        assert_eq!(source.player_track(0).await.action, SourceAction::PlayerChapter(2));
    }

    #[tokio::test]
    async fn a_selection_made_before_the_toc_is_read_is_abandoned_if_it_turns_out_invalid() {
        // Regression (review 2, N3): the concrete path the
        // `wanted < total_tracks` guard in `player_track` protects.
        // `select`'s own out-of-range guard only fires once `total_tracks`
        // is known, and is silently skipped while it is still 0 — the TOC
        // not read yet. A digit pressed in that window can arm a
        // destination the disc turns out too short for once its real TOC
        // lands.
        let (mut source, _p, toc_tx) = source_with_channels();
        // No TOC yet: total_tracks == 0, so `select`'s own guard is skipped
        // regardless of how large `n` is.
        let out = source.select(9).await;
        assert_eq!(out.action, SourceAction::play("cdda://").finite());
        assert_eq!(source.pending_chapter, Some(8));

        // The TOC arrives: only 3 tracks, so track 9 never existed.
        let epoch = source.epoch;
        toc_tx.send((epoch, Some("3 150 22767 41887 63000".into()), 3)).await.unwrap();
        source.poll_notification().await;

        // mpv reports the disc open at its own first track: the stale
        // destination is abandoned rather than applied outside the disc.
        let out = source.player_track(0).await;
        assert_eq!(out.action, SourceAction::Noop);
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn a_later_selection_overrides_an_unconfirmed_earlier_one() {
        // Regression (review 1, I2): `select` took `self.playback` alone for
        // "the disc is open", but both `select` and `start` set it true
        // *before* mpv confirms anything. A second digit pressed before the
        // first notification landed was therefore treated as "already
        // open": it sent a `PlayerChapter` mpv had nothing to seek yet, and
        // the still-armed first `pending_chapter` then won on the next
        // notification — the first choice outlived the last one.
        let mut source = source_arriving_with(OnArrival::Nothing);
        let first = source.select(1).await;
        assert_eq!(first.action, SourceAction::play("cdda://").finite());
        let second = source.select(3).await;
        assert_eq!(
            second.action,
            SourceAction::Noop,
            "the load already went out; a second one would reload an unopened disc"
        );
        // Track 3 (index 2), not track 1 (index 0): the last press must win.
        assert_eq!(source.player_track(0).await.action, SourceAction::PlayerChapter(2));
    }

    #[tokio::test]
    async fn next_and_prev_walk_the_chapters() {
        // `PlayerNext` sent `playlist-next`, documented without a flag as doing
        // nothing when the last (here: the only) entry is playing: the screen
        // advanced, the sound did not.
        let mut source = playing_source();
        // Widened so the walk has room to move in both directions from track
        // 3 (index 2): `playing_source`'s own 3 tracks would already put
        // `select(3)` on the last one, which is exactly the boundary the
        // *next* test below exercises instead. The TOC is widened to match
        // — 5 tracks, not 3 — so the fixture does not disagree with the
        // count it declares (same lesson as `remember_track`, review 1).
        source.total_tracks = 5;
        source.toc = Some("5 150 20000 40000 60000 80000 100000".into());
        source.select(3).await;
        assert_eq!(source.next().await.action, SourceAction::PlayerChapter(3));
        assert_eq!(source.prev().await.action, SourceAction::PlayerChapter(2));
    }

    #[tokio::test]
    async fn next_does_not_run_past_the_last_track() {
        let mut source = playing_source();
        let last_track = source.total_tracks as u8;
        source.select(last_track).await;
        assert_eq!(
            source.next().await.action,
            SourceAction::Noop,
            "no wrap-around, and no chapter beyond the disc"
        );
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
        // The disc loads whole — `cdda://3` would name a *device* called "3"
        // to mpv, not track 3 — and the resumed track is reached by chapter
        // once mpv confirms the disc is open (see
        // `resuming_the_last_track_goes_through_the_disc_and_a_chapter`).
        assert_eq!(out.action, SourceAction::play("cdda://").finite());
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
        assert_eq!(out.action, SourceAction::play("cdda://").finite());
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn resuming_starts_at_the_first_track_when_nothing_is_known_yet() {
        // Two cases, one behaviour, and it is deliberate: the setting says
        // start the disc, so the disc starts.
        //
        // Nothing remembered — a fresh install:
        //
        // The action alone cannot tell a successful resume from this
        // fallback — both load `cdda://` whole (review 1, I3) — so `track`
        // and `pending_chapter` are what actually prove "first track" was
        // chosen: `Some(2)` (the remembered track) would pass the action
        // assertion just as well.
        let mut fresh = source_arriving_with(OnArrival::LastTrack);
        assert_eq!(fresh.activate().await.action, SourceAction::play("cdda://").finite());
        assert_eq!(fresh.track, 0);
        assert_eq!(fresh.pending_chapter, Some(0));

        // TOC not read yet — the read is asynchronous, and a plugin cannot
        // ask for playback later on (a spontaneous notification carries a
        // state, never an action). So a boot that outruns the TOC read
        // resumes at the first track rather than trusting a number it cannot
        // check.
        let mut unread = source_arriving_with(OnArrival::LastTrack);
        unread.toc = None;
        unread.remembered =
            Some(Remembered { toc: "3 150 22767 41887 63000".into(), track: 2 });
        assert_eq!(unread.activate().await.action, SourceAction::play("cdda://").finite());
        assert_eq!(unread.track, 0);
        assert_eq!(unread.pending_chapter, Some(0));
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
        assert_eq!(source.activate().await.action, SourceAction::play("cdda://").finite());
        // The action is the same whether the guard fired or not (review 1,
        // I3): only the destination proves it did — track 7 was refused,
        // not silently carried through to `pending_chapter`.
        assert_eq!(source.track, 0);
        assert_eq!(source.pending_chapter, Some(0));
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
        assert_eq!(source.play().await.action, SourceAction::play("cdda://").finite());
        // The action alone would be identical to "start at track 1" (review
        // 1, I3); the destination is what proves Play actually obeyed the
        // resume rather than the arrival default.
        assert_eq!(source.track, 2);
        assert_eq!(source.pending_chapter, Some(2));
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
