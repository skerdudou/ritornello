//! Track metadata: identity declared by the source, ICY titles, file tags, plugin enrichments, covers and their extraction.

use super::*;

impl<P: Player> Core<P> {
    /// Applies the selection a frame declares: the preset number and its
    /// readable name. Convention "absent = keep the current value", the
    /// opposite of `status`.
    ///
    /// Called by `apply_declared_facts` alone, which relays it to the two
    /// exits of `handle_source_update`: the frame that recomposes the view
    /// applies it **after** the identity (`set_identity(None)` clears the
    /// selection, an explicit declaration must win), the one that merely
    /// announces a fact applies it before returning. Two copies of these
    /// four lines would diverge.
    pub(super) fn apply_selection(&mut self, preset: Option<u8>, name: Option<String>) {
        if let Some(p) = preset {
            self.preset = Some(p);
        }
        if let Some(n) = name {
            self.preset_name = Some(n);
        }
    }

    /// Applies the cover a Source frame declares.
    ///
    /// The cover follows the same convention as `preset`/`preset_count`:
    /// absent = nothing new, never "no more cover" — a Source does not
    /// repeat the declaration on every status frame that follows (see
    /// `SourceUpdate::cover`). That is why `set_source_cover` must only be
    /// called when the field is `Some`.
    ///
    /// **Called by `apply_declared_facts`**, exactly like `apply_selection`
    /// and for the same reason — it is the one that relays it to the two
    /// exits of `handle_source_update`. On the path that recomposes the
    /// view, the call comes **after** the identity: `set_identity` resets
    /// everything `Metadata` was holding, the Source's cover included, so a
    /// frame that carried both a new identity and its cover must let the
    /// identity speak first — otherwise the freshly declared cover would be
    /// erased right away by that reset. This is exactly the trap that the
    /// comment on `apply_selection`, above, already points out for the
    /// selection.
    ///
    /// On the early-return path there is, by construction, neither identity
    /// nor status, so ordering has no meaning there. **That is the path that
    /// matters**: a Source cover arrives alone, as a spontaneous
    /// notification, so it goes through there almost always — the path that
    /// recomposes the view only serves a frame that would carry a cover *at
    /// the same time* as an identity or a status. Without the early-return
    /// call, the cover is not "applied later": it is lost silently, and that
    /// is the actual defect the covers worksite merge had introduced.
    ///
    /// Called from `handle_source_update` and not from `main`'s `select!`
    /// loop: the head guard (`standby || name != self.active_source`) must
    /// apply to the cover like to everything else in the frame. An inactive
    /// source could otherwise make its cover appear on the track the
    /// **active** source is playing.
    ///
    /// `validated` here, just as `Enrichment::cleaned` does on the other
    /// channel: a cover enters the core through two doors, and the
    /// `ritornello-proto` layer — the one that owns shape validation — only
    /// guarded one of the two. Nothing was exploitable, the core's own
    /// checks covering this path, but a shape rule applied to one door out
    /// of two ends up diverging. A refused reference means "nothing new",
    /// never "no more cover": that is the field's convention (see
    /// `SourceUpdate::cover`), and erasing on a malformed frame would remove
    /// the valid image a previous frame had declared.
    pub(super) fn apply_source_cover(
        &mut self,
        cover: Option<ritornello_proto::CoverRef>,
        name: &str,
    ) {
        if let Some(cover) = cover.and_then(ritornello_proto::CoverRef::validated) {
            self.set_source_cover(Some(cover), name);
        }
    }

    /// Changes what is playing: wipes the metadata slate clean, notifies the
    /// `metadata` plugins, and refreshes the display and the broadcast state.
    ///
    /// `None` = nothing is playing anymore. The core never looks **inside**
    /// the identity: it compares it by equality and relays it as-is.
    pub(super) fn set_identity(&mut self, identity: Option<serde_json::Value>) {
        // "Nothing is playing anymore" takes the current selection with it:
        // the highlighted key designates **what is playing**, not the last
        // press. Done before the equality guard: an identity already at
        // `None` (repeated stop, source switch after a stop) must still
        // leave the selection cleared.
        if identity.is_none() {
            self.preset = None;
            self.preset_name = None;
        }
        if !self.metadata.set_identity(identity) {
            return;
        }
        // The track changed: the previous track's anchor must not keep
        // advancing under the next one's title. The last published position
        // must disappear with it, otherwise the frame emitted right away
        // would carry the old track's position under the new one's title,
        // until the next tick (up to one second).
        self.position_anchor = None;
        self.position_s = None;
        let np = NowPlaying {
            source: self.active_source.clone(),
            identity: self.metadata.identity().cloned(),
            // Always empty at this precise instant (the reset above just
            // erased everything `Metadata` knew), but read from `known()`
            // rather than a frozen `Known::default()`: the value stays
            // correct if the reset were ever to change, and `publish_state`
            // republishes this same field as soon as it stops being empty.
            known: self.metadata.known(),
        };
        // Failure impossible in practice: a `watch::Sender::send` only fails
        // when no receiver is alive anymore, and `main` keeps its own to
        // feed upcoming `metadata` plugin connections. Without consequence
        // for playback anyway: a `warn` would be enough to drown the logs
        // if no metadata plugin were declared.
        let _ = self.now_playing_tx.send(np);
        // The slate changed, so the display must follow — as
        // `handle_icy_title` and `handle_enrichment` do. Without this
        // refresh, `Command::Stop` left the stopped track's title **frozen
        // on the physical display** until the user's next action, while the
        // SPA, for its part, emptied itself correctly. `player_state` reads
        // `self.metadata.state()` on every call, so this single
        // `publish_state` is enough: no more need for the second conditional
        // call to the overlay that the old composed-views channel required.
        self.publish_state();
    }

    /// Title announced by the stream itself (ICY header seen by mpv).
    pub(super) fn handle_icy_title(&mut self, title: String) {
        // Two guards, and **neither** consults the identity: this layer must
        // work without any `metadata` plugin and even against a Source that
        // declares no identity, otherwise the only layer that works on its
        // own went quiet, silently.
        //
        // In standby, nothing must reach the display — same guard as
        // `handle_source_update`. The path is real: `Command::Power` waits
        // for the Source's reply to `Deactivate` (up to 5 s) while mpv is
        // still playing, and a title emitted in that window arrives after
        // the standby view has been pushed.
        //
        // `expecting_stream` is what the core knows **on its own** about
        // playback: set to true on every `Play` it applies, to false on
        // `Stop`. It is what prevents a late title from showing up, and
        // staying there, after a stop.
        if self.standby || !self.expecting_stream {
            return;
        }
        if !self.metadata.set_icy(title) {
            return;
        }
        self.publish_state();
    }

    /// Tags carried by the played file, as mpv exposes them.
    ///
    /// Same guards as ICY, with one difference which is the whole point of
    /// the `playback` field: the "something is playing" guard cannot be
    /// `expecting_stream`, which is **false** precisely during playback of
    /// finite content — hence during the only kind of playback where file
    /// tags exist. Using it would have produced a layer that never shows
    /// up, with nothing in the logs.
    pub(super) fn handle_file_tags(&mut self, track: ritornello_proto::Track) {
        if self.standby || !self.playback {
            return;
        }
        if !self.metadata.set_tags(track) {
            return;
        }
        self.publish_state();
    }

    /// Path of the file mpv actually opened (`path` property), to pull the
    /// embedded cover out of it. Only arms a **detached** extraction: see
    /// `extraction_arrived` for the follow-up, when the result arrives.
    ///
    /// Same "something is playing" guard as the tags (`playback`, not
    /// `expecting_stream`, for the same reason): `path` is republished for a
    /// stream just as for a file.
    ///
    /// **The core completes, it does not overwrite**: if a cover is already
    /// held — a Source's `folder.jpg`, notably — the extraction is not even
    /// launched, which saves a pointless file read and preserves the
    /// precedence `Metadata::selected_cover` intends.
    ///
    /// **Always detached, never run on this thread.** `mpv::
    /// embedded_cover` opens and walks the file with `lofty`, a strictly
    /// blocking call, potentially on a network share that may never answer.
    /// Running it here would freeze the entire core loop — mpv, commands,
    /// HTTP — for the duration of the block, not just this extraction. This
    /// project already lived through that incident on a silent cifs mount
    /// (see `health.rs`), hence `Health::bounded`: `spawn_blocking` to get
    /// off the async thread, under a deadline, with a circuit breaker per
    /// mount point so as not to lose a pool thread on every new track as
    /// long as the share stays silent.
    pub(super) fn handle_path(&mut self, path: String) {
        // Retained before every guard below: it is what `extraction_arrived`
        // compares on arrival to reject a late reply for a track already
        // replaced, including when `standby`/`playback` changed in the
        // meantime.
        self.current_path = Some(path.clone());
        if self.standby || !self.playback {
            return;
        }
        if self.metadata.known().cover {
            return;
        }
        // A stream has no tags, and `lofty` has nothing to open on a URL:
        // no point paying the task + channel round trip for a case that can
        // never succeed (`embedded_cover` would refuse it anyway).
        if path.contains("://") {
            return;
        }
        if self.extraction_in_flight.as_deref() == Some(path.as_str()) {
            return;
        }
        self.extraction_in_flight = Some(path.clone());
        let tx = self.extraction_tx.clone();
        let health = self.health.clone();
        tokio::spawn(async move {
            let to_read = path.clone();
            // **The two `None`s are distinguished, and the previous
            // `.flatten()` conflated them.** "This file has no embedded
            // cover" and "the share did not answer within the timeout" give
            // the same screen — no image — and used to give the same trace:
            // none. That is exactly what was missing to answer "why was the
            // cover not pushed".
            let r = match health
                .bounded(std::path::Path::new(&path), move || mpv::embedded_cover(&to_read))
                .await
            {
                // The circuit breaker handed back control: a real incident
                // (silent share), hence `warn` — it belongs in the map of
                // recent errors.
                None => {
                    tracing::warn!("embedded cover: {path} did not answer in time");
                    None
                }
                // Actual answer: this file carries no image. Ordinary case,
                // hence `info`.
                Some(None) => {
                    tracing::info!("no embedded cover in {path}");
                    None
                }
                Some(Some(c)) => Some(c),
            };
            let _ = tx.send((path, r)).await;
        });
    }

    /// A detached embedded-cover extraction (`handle_path`) has finished.
    /// Symmetric with `cover_arrived`: the staleness check happens here, on
    /// arrival, not at launch.
    pub async fn extraction_arrived(&mut self, path: String, r: Option<crate::cover::CoverSource>) {
        // Released whatever the outcome and before any check below — same
        // reason as `cover_in_flight` in `cover_arrived`: without this,
        // this same track played again later would stay blocked for the
        // rest of the process.
        if self.extraction_in_flight.as_deref() == Some(path.as_str()) {
            self.extraction_in_flight = None;
        }
        // mpv already moved on to another file: this reply describes a
        // track that is no longer playing, and must not settle on the next
        // one.
        if self.current_path.as_deref() != Some(path.as_str()) {
            return;
        }
        // Another channel provided a cover while this one was in flight
        // (the Source, or a plugin): the core completes, it does not
        // overwrite.
        if self.metadata.known().cover {
            return;
        }
        if !self.metadata.set_cover_tags(r) {
            return;
        }
        self.start_cover_fetch();
        self.publish_state();
    }

    /// Enrichment reported by a `metadata` plugin. Nothing happens if it is
    /// stale, empty, or emitted by an undeclared plugin (see
    /// `Metadata::add`).
    pub fn handle_enrichment(&mut self, plugin: &str, e: Enrichment) {
        if !self.metadata.add(plugin, e) {
            return;
        }
        // We log **the winner**, not the one that just answered: a
        // lower-priority plugin can be held in reserve without displaying
        // anything, and a log naming it would lie in the only case where it
        // gets consulted — attributing a dubious display.
        match self.metadata.winner() {
            Some(winner) if winner != plugin => {
                tracing::debug!("metadata displayed: {winner} (response from {plugin} held in reserve)");
            }
            Some(winner) => tracing::debug!("metadata displayed: {winner}"),
            None => {}
        }
        // Set the anchor on reception: it is the only instant when the
        // announced elapsed time is exact.
        //
        // **Only when the winner is the one that just spoke**, and that is a
        // defect found in review. A plugin held in reserve can answer at any
        // time (a corrected title, a cover) without learning anything new
        // about progress: re-anchoring then would re-read the winner's
        // **unchanged** position while dating it to now, and the bar would
        // jump back by everything it had advanced. The `match` above already
        // distinguishes the two cases for the log.
        //
        // A winner re-emitting identically never gets here: `add`
        // deduplicates and returns `false`. And a higher-priority plugin
        // answering for the first time **becomes** the winner, so its
        // announcement does anchor, which is intended.
        if self.metadata.winner() == Some(plugin) {
            self.position_anchor = self.metadata.position_s().map(|p| (p, Instant::now()));
        }
        // The enrichment just retained may have changed the cover that
        // `selected_cover` designates (an overwriting plugin answering after
        // a `fill_only`, for instance): `add` already invalidated the
        // published key in that case, it is up to `start_cover_fetch` to
        // relaunch the fetch for the new target.
        self.start_cover_fetch();
        self.publish_state();
    }

    /// Retains the cover a Source just declared on its own channel (see
    /// `SourceMessage::cover`, Task 2).
    pub fn set_source_cover(&mut self, c: Option<ritornello_proto::CoverRef>, origin: &str) {
        if self.metadata.set_cover_source(c, origin) {
            self.start_cover_fetch();
            self.publish_state();
        }
    }

    /// Detaches the fetch of the retained cover, if it is neither already
    /// cached nor in flight.
    ///
    /// Detached, because a ten-second download must not hold up the loop
    /// that answers commands. And **abandoned if the identity changes**: it
    /// is `cover_arrived` that checks, on arrival, that the key still
    /// describes what is playing — same safeguard as the identity echo of
    /// the text (`Metadata::add`), for the same reason: a late reply for
    /// the previous track must never settle on the next one.
    pub fn start_cover_fetch(&mut self) {
        let Some((r, _)) = self.metadata.selected_cover() else {
            // Nothing left to show (identity changed, cover removed): clear
            // the published URL rather than leaving it pointing at an image
            // that no longer matches what is playing.
            self.metadata.set_cover_href(None);
            return;
        };
        let key = crate::cover::key(&r);
        if self.metadata.published_cover() == Some(key.as_str()) {
            // Already published under this same key: nothing to redo.
            // Without this guard, a retained enrichment republishing
            // identically (a station reconfirming its metadata every thirty
            // seconds, for instance) would relaunch a task, a `contains` and
            // a channel round trip for work already done — and would rearm
            // `cover_in_flight` needlessly.
            return;
        }
        if self.cover_in_flight.as_deref() == Some(key.as_str()) {
            // Already in flight for this same target: a second request
            // would not learn anything sooner, and would double the network
            // traffic.
            return;
        }
        let covers = self.covers.clone();
        let tx = self.cover_tx.clone();
        self.cover_in_flight = Some(key.clone());
        // An embedded source must not take the `contains` short-circuit
        // below. The key is content-addressed (`cover::key` hashes the
        // picture's bytes, not the audio path — see its doc), which is
        // exactly what lets a fifteen-track album share one entry; but the
        // payload behind that entry names one specific audio file, and
        // `contains` alone cannot tell "still the same file" from "a
        // different file that once produced the same key". Two ways for
        // that to go wrong if this branch is removed: the first file named
        // gets moved, renamed or deleted, and every other track sharing its
        // key starts 404ing even though it still carries the very picture
        // the key promises; or the first file gets retagged in place (same
        // path, a new picture) and a later track that still carries the
        // *old* picture recomputes the same old key, finds `contains` true,
        // and is served the retagged file's *new* bytes under a key that
        // was never computed from them — a content-addressed key silently
        // lying about its own content. Always re-probing and re-inserting
        // settles both: the entry ends up naming whichever file this
        // `fetch` actually just saw carry this exact picture. This costs
        // nothing for `Embedded` — `cover::fetch` performs no IO on it, see
        // its doc — which is why the branch stays worth keeping for `Ref`:
        // there, skipping a re-download over the internet is the entire
        // point of the guard.
        let is_embedded = matches!(&r, crate::cover::CoverSource::Embedded { .. });
        tokio::spawn(async move {
            if !is_embedded && covers.contains(&key).await {
                let _ = tx.send((key, true)).await;
                return;
            }
            // Stopwatch: this is **the step the owner suspects** — the
            // image provider taking a long time to answer. Without a
            // measurement, the delay between a track's announcement and the
            // appearance of its cover could not be attributed to any
            // particular step.
            let started = std::time::Instant::now();
            match crate::cover::fetch(&r).await {
                Some(p) => {
                    tracing::info!("cover {key} fetched in {:?}", started.elapsed());
                    covers.insert(key.clone(), p).await;
                    let _ = tx.send((key, true)).await;
                }
                // Silent failure: the device shows no image, and that is
                // all. A 404 from the Cover Art Archive is the common case.
                // Reported anyway (`false`): it is what releases
                // `cover_in_flight`, without which this key would stay
                // blocked for the rest of the process — including if the
                // same folder (hence the same key) becomes the target again
                // later.
                None => {
                    // `info` and not `debug`: it is the other half of the
                    // diagnosis. "No cover found" and "cover found but then
                    // impossible to serve" (see the `warn` in `cover_get`)
                    // give the same screen — a ♫ — and nothing allowed
                    // telling them apart after the fact. A 404 from the
                    // Cover Art Archive remains an ordinary case, hence
                    // `info` rather than `warn`: it has no place in the map
                    // of recent errors.
                    tracing::info!("no cover found for {key}");
                    let _ = tx.send((key, false)).await;
                }
            }
        });
    }

    /// A detached fetch has finished (`success`), whether it succeeded or
    /// not. Publishes the local URL, **if it still describes what is
    /// playing**: the check happens here, on arrival, not at launch — that
    /// is what prevents the cover of an already-replaced track from
    /// settling on the next one.
    pub async fn cover_arrived(&mut self, key: String, success: bool) {
        // The marker is released as soon as this key comes back, **whatever
        // the outcome** — network failure, cover no longer retained, or
        // success — and **before** any staleness check below. Without this,
        // a failure or an already-replaced track left this key blocked for
        // the rest of the process: `start_cover_fetch` then refused to
        // relaunch a fetch for this same key, even when it became the
        // target again (the same album folder, hence the same key, is
        // played again later) and even if the bytes ended up cached.
        if self.cover_in_flight.as_deref() == Some(key.as_str()) {
            self.cover_in_flight = None;
        }
        // The staleness check holds for **both** outcomes, and that is
        // deliberate: a failure arriving after a track change describes a
        // reference that what is playing now does not target. Recording it
        // in the current track's failure registry would blacken a key never
        // tried for it — and if a contributor proposed this same image
        // here, it would be discarded without having been attempted a
        // single time. The failure holds for the track where it happened,
        // like all the rest of this state (see `Metadata::failed_covers`).
        let Some((r, _)) = self.metadata.selected_cover() else {
            // Nothing is playing anymore, or no cover retained anymore: the
            // reply arrives too late to be meaningful.
            return;
        };
        if crate::cover::key(&r) != key {
            // The previous track's cover (or a reference replaced since):
            // without this check, it would settle on the current track.
            return;
        }
        if !success {
            // The failure is **recorded**, and that is what unblocks the
            // contributors located below. A retained reference is only a
            // promise: without this note, `selected_cover` kept preferring
            // a dead URL, `known.cover` stayed true, and `musicbrainz` —
            // silent because it believes a cover is held — had no chance to
            // compensate. This is exactly the case the design anticipates:
            // "a pattern that breaks yields silence".
            //
            // Relaunch and republish only if the retained reference has
            // actually changed: that is what gives the contributor below
            // its chance, and what avoids republishing for nothing.
            if self.metadata.mark_cover_failed(key) {
                self.start_cover_fetch();
                self.publish_state();
            }
            return;
        }
        // Re-checked rather than trusting `success` alone: the cache is
        // bounded (`cover_cache_entries` entries, FIFO eviction) and this
        // key may have been evicted between the deposit and the consumption
        // of this message by `main`'s loop — a case all the more real since
        // the channel is deliberately narrow (capacity 4).
        if !self.covers.contains(&key).await {
            // Evicted between the deposit and the consumption of this
            // message: the cache only keeps `cover_cache_entries` entries.
            // Silent until now, even though it is a cover **lost after
            // having been fetched** — the worst case, and the hardest to
            // attribute without a trace.
            tracing::warn!("cover {key} evicted before it could be published");
            return;
        }
        // The positive trace, which closes the timeline: it is the one that
        // says *when* the image finally arrived, where the owner could only
        // observe "much later".
        tracing::info!("cover {key} published");
        self.metadata.set_cover_href(Some(key));
        self.publish_state();
    }

    /// The cache the detached task of `start_cover_fetch` fills — **the
    /// same one** as the HTTP `AppState`'s, see the `covers` field doc.
    /// Test-only: it is what lets tests prove the sharing without going
    /// through `main.rs`, which is not testable as such.
    #[cfg(test)]
    pub(crate) fn app_covers(&self) -> &Arc<crate::cover::CoverCache> {
        &self.covers
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;
    use crate::cover::CoverSource;

    #[tokio::test]
    async fn a_late_metadata_plugin_takes_its_manifest_place_in_arbitration() {
        // The easiest invariant to break in hot wiring: the priority is the
        // one from `plugins.toml`, never the arrival order of announcements.
        // Only `musicbrainz` announced itself in time; `ouifm` arrives after
        // startup even though the manifest declares it **before** it.
        // Appending it at the tail would make it lose the arbitration, and
        // the priority would depend on the startup chronology.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec!["musicbrainz".into()]);
        let id = serde_json::json!({"url": "one"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment("musicbrainz", enrichment(id.clone(), "Base", "Online"));
        assert_eq!(state_rx.borrow().track.artist.as_deref(), Some("Base"));

        // What `main` does when a late announcement is received: recompute
        // the **complete** list from the manifest, then hand it back to the
        // core. The ordering logic stays in `register::metadata_order`, a
        // single place.
        let manifest = vec!["ouifm".to_string(), "musicbrainz".to_string()];
        let mut gathered = crate::register::Gathered::default();
        for name in ["musicbrainz", "ouifm"] {
            gathered.announcements.insert(
                name.to_string(),
                ritornello_proto::Announcement {
                    name: name.to_string(),
                    kinds: vec![ritornello_proto::PluginKind::Metadata],
                    admin: false,
                    covers: false,
                },
            );
        }
        core.set_metadata_order(crate::register::metadata_order(&manifest, &gathered));

        core.handle_enrichment("ouifm", enrichment(id, "Station", "Direct"));
        assert_eq!(
            core.metadata.winner(),
            Some("ouifm"),
            "the latecomer is declared earlier in the manifest: it must win"
        );
        assert_eq!(state_rx.borrow().track.artist.as_deref(), Some("Station"));
    }

    #[tokio::test]
    async fn the_declared_selection_is_broadcast_then_forgotten_when_nothing_plays() {
        // The numbered key highlighted on the web UI's remote designates
        // **what is playing**: it follows the Source's declaration, and
        // disappears on stop rather than staying on the last press.
        // The preset name follows exactly the same rule: that is the point
        // of the spec that matters (the lifecycle of `preset_name` is that
        // of `preset`, locked in here).
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        let mut update = plays(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(state_rx.borrow().preset, Some(2));
        assert_eq!(state_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(state_rx.borrow().preset, None);
        assert_eq!(state_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn switching_source_forgets_the_previous_ones_selection() {
        // The radio's preset 2 means nothing to the cd: leaving it
        // highlighted after the switch would designate a random key. Same
        // for its name: "France Inter" displayed after switching to the cd
        // would be a station name attributed to a disc.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        let mut update = plays(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(state_rx.borrow().preset, Some(2));
        assert_eq!(state_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert_eq!(state_rx.borrow().preset, None);
        assert_eq!(state_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn the_identity_declared_by_the_source_is_announced_to_plugins() {
        let (mut core, np_rx, _state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        let id = serde_json::json!({"kind": "stream", "url": "http://ouifm"});
        core.handle_source_update("radio", plays(id.clone()));
        let np = np_rx.borrow().clone();
        assert_eq!(np.source, "radio");
        assert_eq!(np.identity, Some(id));
    }

    #[tokio::test]
    async fn an_identity_from_an_inactive_source_is_ignored() {
        // The cd can report the insertion of a disc while the radio is
        // playing: announcing that identity would make the plugins work on
        // a track that comes out of no speaker.
        let (mut core, np_rx, _state_rx, _d) = setup_metadata(vec![]);
        core.handle_source_update("cd", plays(serde_json::json!({"kind": "disc"})));
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn icy_is_broadcast_to_the_spa() {
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        // `resume` puts the radio in playback: without it the core rightly
        // discards any ICY title, nothing playing.
        core.resume().await.unwrap();
        core.handle_source_update("radio", plays(serde_json::json!({"url": "one"})));
        assert_eq!(state_rx.borrow().track.title, None);

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let state = state_rx.borrow().clone();
        assert_eq!(state.track.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(state.track.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn a_plugin_enrichment_overrides_icy() {
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "one"});
        core.handle_source_update("radio", plays(id.clone()));
        // Filler text actually emitted by OUI FM on its main stream.
        core.handle_event(Event::IcyTitle("Now Playing info goes here".into())).await;
        // Without this check, the rest of the test would pass just as well
        // if the ICY had never entered: we would not be verifying "the
        // enrichment wins" but "the ICY is absent".
        assert_eq!(state_rx.borrow().track.title.as_deref(), Some("Now Playing info goes here"));
        core.handle_enrichment("ouifm", enrichment(id, "Shaka Ponk", "Wanna Get Free"));
        let state = state_rx.borrow().clone();
        assert_eq!(state.track.artist.as_deref(), Some("Shaka Ponk"));
        assert_eq!(state.track.title.as_deref(), Some("Wanna Get Free"));
        assert_eq!(state.track.origin.as_deref(), Some("ouifm"));
    }

    #[tokio::test]
    async fn a_stale_enrichment_does_not_touch_the_display() {
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        core.handle_source_update("radio", plays(serde_json::json!({"url": "two"})));
        state_rx.borrow_and_update();
        core.handle_enrichment(
            "ouifm",
            enrichment(serde_json::json!({"url": "one"}), "Old", "Track"),
        );
        assert!(!state_rx.has_changed().unwrap(), "the late reply must publish nothing");
        assert!(core.player_state().track.is_empty());
    }

    #[tokio::test]
    async fn changing_track_immediately_clears_the_previous_one() {
        // The previous track must not stay on screen while waiting for the
        // next one: it is a behavior, not a detail.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "one"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment("ouifm", enrichment(id, "Miles Davis", "So What"));
        assert_eq!(state_rx.borrow().track.title.as_deref(), Some("So What"));

        core.handle_source_update("radio", plays(serde_json::json!({"url": "two"})));
        assert!(state_rx.borrow().track.is_empty(), "the slate must be clean right away");
    }

    #[tokio::test]
    async fn a_stop_requested_from_the_remote_clears_the_display_title() {
        // Defect found in review: `set_identity` did not refresh the
        // display. The SPA emptied itself (state channel), but the physical
        // display kept the stopped track's title until the user's next
        // action — all night long on a device turned off in the evening.
        // The old test only asserted the `now_playing` channel: it passed
        // just as well against the wrong code.
        let (mut core, np_rx, state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        let id = serde_json::json!({"url": "one"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment("ouifm", enrichment(id, "Miles Davis", "So What"));
        assert_eq!(state_rx.borrow().track.title.as_deref(), Some("So What"));

        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None, "the plugins must stop their work");
        assert!(state_rx.borrow().track.is_empty(), "the title must not stay displayed");
    }

    #[tokio::test]
    async fn an_icy_title_arriving_in_standby_does_not_reach_the_published_state() {
        // Real path: `Command::Power` waits for the Source's reply to
        // `Deactivate` (up to 5 s) while mpv is still playing. A title
        // emitted in that window arrives after the standby state has been
        // published — and since nothing happens anymore in standby, it
        // would stay there for weeks.
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", plays(serde_json::json!({"url": "one"})));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(state_rx.borrow_and_update().status.as_deref(), Some("STANDBY"));

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let state = state_rx.borrow().clone();
        assert_eq!(state.status.as_deref(), Some("STANDBY"));
        assert!(state.track.is_empty(), "no title must stick onto the standby state");
    }

    #[tokio::test]
    async fn standby_blocks_icy_even_with_a_live_identity() {
        // Two guards cover this path, and this one is not redundant:
        // entering standby normally clears the identity, but
        // `Command::Power` can return on the error of `player.stop()`
        // **before** doing so, leaving standby active with a live identity.
        // The state is therefore set directly here to exercise the standby
        // guard alone.
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec![]);
        core.resume().await.unwrap(); // sets `expecting_stream` (the radio is playing)
        core.handle_source_update("radio", plays(serde_json::json!({"url": "one"})));
        state_rx.borrow_and_update();
        // Standby set directly: it is the state reached when
        // `Command::Power` returns on the error of `player.stop()`, hence
        // with playback still expected. The standby guard is then the only
        // one acting.
        core.standby = true;
        assert!(core.expecting_stream, "otherwise this test would not exercise the standby guard");

        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        assert!(!state_rx.has_changed().unwrap(), "nothing must reach the published state in standby");
        assert_eq!(state_rx.borrow().track.title, None);
    }

    #[tokio::test]
    async fn icy_shows_even_if_the_source_declares_no_identity() {
        // Regression met in a real-world trial: the ICY layer was
        // conditioned on the Source's identity declaration, hence mute
        // against a plugin that does not declare one — and mute
        // **silently**, without a single log line. Yet it is the only layer
        // supposed to work without any `metadata` plugin.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.resume().await.unwrap();
        // No identity is ever declared: only the preset name arrives.
        core.handle_source_update("radio", update_with_name(Some("FIP")));
        core.handle_event(Event::IcyTitle("Made Up - TAHITI 80".into())).await;
        assert_eq!(state_rx.borrow().track.title.as_deref(), Some("Made Up - TAHITI 80"));
        assert_eq!(state_rx.borrow().track.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn an_icy_title_arriving_after_a_stop_is_ignored() {
        let (mut core, _np_rx, mut state_rx, _d) = setup_metadata(vec![]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", plays(serde_json::json!({"url": "one"})));
        core.handle_command(Command::Stop).await.unwrap();
        state_rx.borrow_and_update();

        core.handle_event(Event::IcyTitle("a late title".into())).await;
        assert!(!state_rx.has_changed().unwrap(), "nothing must be published");
        assert_eq!(state_rx.borrow().track.title, None, "the SPA must not announce any track");
    }

    #[tokio::test]
    async fn entering_standby_forgets_the_identity() {
        let (mut core, np_rx, _state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", plays(serde_json::json!({"url": "one"})));
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(np_rx.borrow().identity, None);
    }

    #[tokio::test]
    async fn entering_standby_forgets_the_selection_and_its_name() {
        // The point of the spec that matters: `preset_name` lives and dies
        // with `preset`, and the only place that clears them is
        // `set_identity(None)` — which `Command::Power` reaches when
        // entering standby, like `Stop` and `SourceCycle` already covered
        // above.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        let mut update = plays(serde_json::json!({"kind": "stream", "url": "http://inter"}));
        update.preset = Some(2);
        update.preset_name = Some("France Inter".into());
        core.handle_source_update("radio", update);
        assert_eq!(state_rx.borrow().preset, Some(2));
        assert_eq!(state_rx.borrow().preset_name.as_deref(), Some("France Inter"));
        core.handle_command(Command::Power).await.unwrap(); // enters standby
        assert_eq!(state_rx.borrow().preset, None);
        assert_eq!(state_rx.borrow().preset_name, None);
    }

    #[tokio::test]
    async fn switching_source_forgets_the_previous_identity() {
        let (mut core, np_rx, _state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        core.handle_source_update("radio", plays(serde_json::json!({"url": "one"})));
        core.handle_command(Command::SourceCycle).await.unwrap();
        let np = np_rx.borrow().clone();
        assert_eq!(np.identity, None);
        assert_eq!(np.source, "cd", "the announcement carries the new active source");
    }

    #[tokio::test]
    async fn a_declared_but_silent_metadata_plugin_does_not_eclipse_icy() {
        // A declared plugin that never answers (dead process, silent socket)
        // must not deprive the device of the base layer: the title announced
        // by the stream must keep showing, attributed to `icy`.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec!["dead".into()]);
        core.resume().await.unwrap();
        core.handle_source_update("radio", plays(serde_json::json!({"url": "one"})));
        core.handle_event(Event::IcyTitle("Mandrillus Sphynx - Bikwix".into())).await;
        let state = state_rx.borrow().clone();
        assert_eq!(state.track.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(state.track.origin.as_deref(), Some("icy"));
    }

    #[tokio::test]
    async fn a_malformed_source_cover_does_not_touch_the_one_that_holds() {
        // `CoverRef::validated` is `ritornello-proto`'s shape rule, and it
        // only applied to one of the two input channels (the plugins' one).
        // A refused reference means "nothing new" — never "no more cover":
        // that is the field's convention, and erasing on a malformed frame
        // would remove the valid image already declared.
        let (mut core, _np_rx, _state_rx, tmp) = test_core();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let good = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let id = serde_json::json!({"kind": "file", "path": "/a.flac"});

        let mut update = plays(id);
        update.cover = Some(good.clone());
        core.handle_source_update("radio", update.clone());
        assert!(core.metadata.known().cover);

        // Relative path: refused by the shape rule. Nothing must move.
        update.identity = None;
        update.cover = Some(ritornello_proto::CoverRef::Path { path: "relative/folder.jpg".into() });
        core.handle_source_update("radio", update.clone());
        assert_eq!(
            core.metadata.selected_cover().map(|(r, _)| r),
            Some(CoverSource::Ref(good)),
            "a malformed reference must neither settle nor erase the one that holds"
        );

        // And neither does a plain-text URL to a literal IP, the other half
        // of what `validated` refuses.
        update.cover =
            Some(ritornello_proto::CoverRef::Url { url: "http://192.168.1.1/a.jpg".into() });
        core.handle_source_update("radio", update);
        assert_eq!(core.metadata.selected_cover().map(|(_, o)| o), Some("radio".to_string()));
    }

    /// A contributor that just got hot-wired, or that answers slowly, must
    /// see what is already known — otherwise it can neither complete what
    /// is missing, nor abstain on what is already filled.
    #[tokio::test]
    async fn the_emitted_now_playing_carries_the_partial_state() {
        let (mut core, mut np_rx, _state_rx, _tmp) = test_core();
        core.set_identity(Some(serde_json::json!({"kind": "stream", "url": "u"})));
        // `handle_icy_title` requires a stream actually expected (see its
        // guard): without this line, the title would be silently ignored
        // and this test would prove nothing.
        core.expecting_stream = true;
        core.handle_icy_title("OUI FM".into());
        core.publish_state();
        // A contributor must see what is already known, otherwise it can
        // neither complete nor abstain.
        let np = np_rx.borrow_and_update().clone();
        assert_eq!(np.known.title.as_deref(), Some("OUI FM"));
        assert!(!np.known.cover);
    }

    #[tokio::test]
    async fn an_arrived_cover_becomes_a_local_url_in_the_state() {
        let (mut core, _np_rx, mut state_rx, tmp) = test_core();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let s = CoverSource::Ref(r.clone());

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_source_cover(Some(r), "files");
        // The fetch is detached: the test waits for it explicitly rather
        // than sleeping, so as not to manufacture a flake.
        let key = crate::cover::key(&s);
        let p = crate::cover::fetch(&s).await.expect("the test image must be readable");
        core.app_covers().insert(key.clone(), p).await;
        core.cover_arrived(key.clone(), true).await;

        let state = state_rx.borrow_and_update().clone();
        assert_eq!(state.track.cover_href.as_deref(), Some(&format!("/api/cover/{key}")[..]));
        assert_eq!(state.track.cover_origin.as_deref(), Some("files"));
    }

    #[tokio::test]
    async fn a_failed_fetch_frees_the_contributors_below() {
        // The junction the review found: `known.cover` was true as soon as
        // a reference was *retained*, and `selected_cover` kept preferring
        // that reference after its fetch failed. A station URL pattern that
        // rusted therefore silenced `musicbrainz` for good — a case the
        // design explicitly anticipates.
        let (mut core, mut np_rx, _state_rx, _tmp) = setup_metadata(vec![
            "radiofrance".into(),
            "musicbrainz".into(),
        ]);
        let id = serde_json::json!({"url": "https://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        let dead =
            ritornello_proto::CoverRef::Url { url: "https://api.radiofrance.fr/rusted".into() };
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                cover: Some(dead.clone()),
                ..Default::default()
            },
        );
        assert!(np_rx.borrow_and_update().known.cover, "a reference is held, we do not know yet");

        // What the detached task reports when the fetch yielded nothing:
        // `success == false`.
        core.cover_arrived(crate::cover::key(&CoverSource::Ref(dead)), false).await;
        let np = np_rx.borrow_and_update().clone();
        assert!(!np.known.cover, "an unkept promise must give the floor back to the others");
        // And the text this same plugin provides has not moved: that is
        // precisely what lets `musicbrainz` search on this artist and this
        // album, as the documentation promises.
        assert_eq!(np.known.title.as_deref(), Some("So What"));
        assert_eq!(np.known.artist.as_deref(), Some("Miles Davis"));
    }

    #[tokio::test]
    async fn a_failure_arriving_after_a_track_change_is_not_recorded() {
        // The failure registry holds for the track where the failures
        // happened. A late failure, arrived after the identity change, must
        // therefore not enter it: it would blacken a key never tried for
        // the current track, and discard that image even though it could
        // perfectly well answer.
        let (mut core, _np_rx, _state_rx, _tmp) = setup_metadata(vec!["musicbrainz".into()]);
        let first = serde_json::json!({"url": "one"});
        core.handle_source_update("radio", plays(first.clone()));
        let image = ritornello_proto::CoverRef::Url {
            url: "https://coverartarchive.org/release/x/front-500".into(),
        };
        core.handle_enrichment(
            "musicbrainz",
            Enrichment {
                identity: first,
                title: Some("T".into()),
                cover: Some(image.clone()),
                ..Default::default()
            },
        );

        // Next track, then the previous one's failure finally arrives.
        let second = serde_json::json!({"url": "two"});
        core.handle_source_update("radio", plays(second.clone()));
        core.cover_arrived(crate::cover::key(&CoverSource::Ref(image.clone())), false).await;

        // The same plugin proposes the same image for this track: never
        // tried here, it must be retained.
        core.handle_enrichment(
            "musicbrainz",
            Enrichment { identity: second, title: Some("T2".into()), cover: Some(image), ..Default::default() },
        );
        assert!(
            core.metadata.known().cover,
            "a stale failure must not condemn the next track's reference"
        );
    }

    /// The risk flagged by task 3's review: two distinct `Arc<CoverCache>`
    /// would compile and let every other test in this module through, but
    /// the cover the core just deposited would never be readable by the
    /// real HTTP route. This test therefore goes through `status::router`
    /// and a real request, with exactly the same `Arc` as the one exposed
    /// by `app_covers()`.
    #[tokio::test]
    async fn the_http_route_serves_what_the_core_just_deposited() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let (mut core, _np_rx, _state_rx, tmp) = test_core();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let s = CoverSource::Ref(r.clone());
        let key = crate::cover::key(&s);

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_source_cover(Some(r), "files");
        let p = crate::cover::fetch(&s).await.expect("the test image must be readable");
        core.app_covers().insert(key.clone(), p).await;
        core.cover_arrived(key.clone(), true).await;

        // The only field that matters for this proof: the rest of the
        // `AppState` comes from the generic test rig, never consulted by
        // this route.
        let app = crate::status::router(crate::status::AppState {
            covers: core.app_covers().clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get(format!("/api/cover/{key}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the route must read from the same cache the core filled"
        );
    }

    /// The cover of an already-replaced track must never settle onto the
    /// next one: the staleness check happens on arrival, not at launch —
    /// same safeguard as the enrichments' identity echo.
    #[tokio::test]
    async fn a_stale_cover_does_not_settle_onto_the_next_track() {
        let (mut core, _np_rx, mut state_rx, tmp) = test_core();
        let old = tmp.path().join("old.jpg");
        std::fs::write(&old, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r_old = ritornello_proto::CoverRef::Path { path: old.to_string_lossy().into_owned() };
        let s_old = CoverSource::Ref(r_old.clone());
        let key_old = crate::cover::key(&s_old);

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_source_cover(Some(r_old), "files");

        // The track changes before the old cover's retrieval has had time
        // to arrive, and the new one declares its **own** cover (a
        // different reference): the target `selected_cover` designates
        // changes with identity, without ever becoming `None` again — it
        // is `cover_arrived`'s key comparison, not merely the absence of a
        // target, that must reject the late reply.
        let new = tmp.path().join("new.jpg");
        std::fs::write(&new, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r_new = ritornello_proto::CoverRef::Path { path: new.to_string_lossy().into_owned() };
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/b.flac"})));
        core.set_source_cover(Some(r_new), "files");
        state_rx.borrow_and_update();

        // The OLD cover's late reply arrives anyway.
        let p = crate::cover::fetch(&s_old).await.expect("the test image must be readable");
        core.app_covers().insert(key_old.clone(), p).await;
        core.cover_arrived(key_old, true).await;

        assert!(
            !state_rx.has_changed().unwrap_or(false),
            "a stale cover must publish nothing about the next track"
        );
        assert_eq!(
            core.player_state().track.cover_href, None,
            "the previous track's cover must not settle onto the next one"
        );
    }

    /// Exact repro of the critical defect found in review (task 5): the
    /// in-flight marker must be released even when the arrival publishes
    /// nothing (track already replaced), otherwise coming back later to the
    /// same folder — hence the same key, a `folder.jpg` is shared by every
    /// track of an album — would never relaunch anything again:
    /// `start_cover_fetch` would see the key perpetually "in flight" and
    /// abandon silently.
    #[tokio::test]
    async fn the_in_flight_marker_is_released_even_when_the_arrival_publishes_nothing() {
        let (mut core, _np_rx, mut state_rx, tmp) = test_core();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };
        let s = CoverSource::Ref(r.clone());
        let key = crate::cover::key(&s);

        // 1. An album track declares cover K: `start_cover_fetch` arms the
        // marker. The real detached task also runs in the background, but
        // nothing below waits for its outcome — like the other tests in
        // this module, this one simulates the arrival itself rather than
        // sleeping.
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_source_cover(Some(r.clone()), "files");
        assert_eq!(core.cover_in_flight.as_deref(), Some(key.as_str()));

        // 2. The track changes before the response arrives: nothing is
        // retained anymore, but the marker does not move on its own —
        // `cover_arrived` is responsible for releasing it, on arrival.
        core.set_identity(Some(serde_json::json!({"kind": "stream", "url": "u"})));
        assert_eq!(core.cover_in_flight.as_deref(), Some(key.as_str()));
        // The identity change already publishes on its own side (title
        // cleared): this frame is consumed so the next assertion only
        // judges what `cover_arrived` publishes, or not, by itself.
        state_rx.borrow_and_update();

        // 3. The response arrives anyway, successfully (the bytes are
        // indeed in hand, just nothing left to show with them). Before the
        // fix, this method returned here without ever touching the
        // marker.
        core.cover_arrived(key.clone(), true).await;
        assert_eq!(core.cover_in_flight, None, "the marker must be released even when nothing is published");
        assert!(
            !state_rx.has_changed().unwrap_or(false),
            "nothing is retained: this arrival must publish nothing"
        );

        // 4. The same folder — hence the same key — becomes the target
        // again. Without the fix, `start_cover_fetch` stayed stuck forever
        // on this key and this album never showed a cover again before a
        // restart.
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.set_source_cover(Some(r.clone()), "files");
        assert_eq!(
            core.cover_in_flight.as_deref(),
            Some(key.as_str()),
            "a new retrieval must be able to restart for the same key"
        );
        let p = crate::cover::fetch(&s).await.expect("the test image must be readable");
        core.app_covers().insert(key.clone(), p).await;
        core.cover_arrived(key.clone(), true).await;

        let state = state_rx.borrow_and_update().clone();
        assert_eq!(
            state.track.cover_href.as_deref(),
            Some(&format!("/api/cover/{key}")[..]),
            "coming back to the same key must be able to publish a cover again"
        );
    }

    /// A cover frame is only processed if it comes from the **active**
    /// Source — the same guard as the rest of the frame (identity, status,
    /// preset). Regression found in review: the previous wiring called
    /// `set_source_cover` outside `handle_source_update`, without going
    /// back through its head guard, so an inactive Source could make its
    /// cover appear on the track the active Source is playing.
    #[tokio::test]
    async fn a_cover_from_an_inactive_source_is_not_retained() {
        let (mut core, _np_rx, state_rx, tmp) = test_core();
        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: image.to_string_lossy().into_owned() };

        // `cd` is not the active source (`radio` is, by default).
        core.handle_source_update(
            "cd",
            SourceUpdate {
                identity: None,
                transient: false,
                preset: None,
                preset_count: None,
                preset_name: None,
                status: None,
                can_eject: None,
                presets: None,
                cover: Some(r),
            },
        );
        assert_eq!(core.cover_in_flight, None, "an inactive source must trigger no retrieval");
        assert!(!state_rx.has_changed().unwrap_or(false));
    }

    /// The path announced by mpv (`Event::Path`) arms a **detached**
    /// extraction: `handle_event` returns immediately, with nothing known
    /// yet — the follow-up (`set_cover_tags` → `true`, `start_cover_fetch`,
    /// `publish_state`) only happens once the result arrives on the
    /// channel.
    ///
    /// The real channel is drained here, rather than replayed by hand as
    /// `cover_arrived` does elsewhere in this file: re-reading the tags a
    /// second time to reconstruct the expected `CoverSource` used to write
    /// concurrently with the detached task on the **same** temp file
    /// (defect found in practice, see `test_core_with_extraction`). There is
    /// no writer left to race today, but draining the real channel still
    /// buys something a hand-reconstructed value cannot: proof that
    /// production code, not a parallel computation believed to agree with
    /// it, is what this assertion checks.
    #[tokio::test]
    async fn the_mpv_path_triggers_extraction_and_arms_the_retrieval() {
        let (mut core, mut state_rx, mut extraction_rx, tmp) = test_core_with_extraction();
        let Some(f) = test_mp3_with_cover(tmp.path()) else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let path = f.to_string_lossy().into_owned();

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": path})));
        core.playback = true;
        state_rx.borrow_and_update();

        assert_eq!(
            core.handle_event(Event::Path(path.clone())).await,
            EventOutcome::Nothing,
            "a path proves nothing about the stream's liveness"
        );

        // It is HERE, and only here, that the extraction is verified to be
        // truly detached (ruling 1 of this task's review) — on a real mp3
        // with an embedded cover, not on a nonexistent path that would
        // fail just as fast synchronously or detached and would therefore
        // prove nothing. `#[tokio::test]` runs on a **single-threaded**
        // runtime (`current_thread`), and `handle_event`'s `Event::Path`
        // arm contains no `.await` before returning: if `handle_path` were
        // still running `embedded_cover` synchronously (a regression that
        // would remove the `tokio::spawn` or the call to
        // `Health::bounded`), `known().cover` would already be true at
        // this exact instant, in the same poll as the `.await` above —
        // there exists no execution universe, fast or slow, in which a
        // synchronous extraction would let this assertion pass. Do not
        // weaken or remove this line without replacing it with an
        // equivalent proof.
        assert!(!core.metadata.known().cover, "the extraction must be detached, never synchronous");
        assert!(!state_rx.has_changed().unwrap_or(false));

        // Waits for the real result on the real channel — no clock here,
        // this is a real async rendezvous on the task `handle_path`
        // detached.
        let (received_path, r) =
            extraction_rx.recv().await.expect("the extraction channel must deliver a result");
        assert_eq!(received_path, path);
        let r = r.expect("the extraction must have succeeded on this test file");
        core.extraction_arrived(received_path, Some(r.clone())).await;

        assert!(core.metadata.known().cover);
        let (retained, origin) = core.metadata.selected_cover().expect("a cover must be retained");
        assert_eq!(origin, crate::metadata::ORIGIN_TAGS);
        assert_eq!(retained, r);
        assert!(state_rx.has_changed().unwrap(), "set_cover_tags returned true: a frame must come out");

        // Replays the end of the detached retrieval by hand, like the
        // other tests in this module: the key `start_cover_fetch` arms
        // must be the one for the temp file the extraction wrote.
        let key = crate::cover::key(&r);
        assert_eq!(core.cover_in_flight.as_deref(), Some(key.as_str()));
        let p = crate::cover::fetch(&r).await.expect("the temp file must be readable");
        core.app_covers().insert(key.clone(), p).await;
        core.cover_arrived(key.clone(), true).await;

        let state = state_rx.borrow_and_update().clone();
        assert_eq!(state.track.cover_href.as_deref(), Some(&format!("/api/cover/{key}")[..]));
        assert_eq!(state.track.cover_origin.as_deref(), Some(crate::metadata::ORIGIN_TAGS));
    }

    /// The core completes, it does not overwrite: a cover already held
    /// (here a Source's, the highest priority) prevents the extraction,
    /// even when mpv announces a file that itself carries a valid embedded
    /// cover.
    #[tokio::test]
    async fn a_cover_already_known_prevents_any_extraction() {
        let (mut core, _np_rx, mut state_rx, tmp) = test_core();
        let Some(f) = test_mp3_with_cover(tmp.path()) else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let folder = tmp.path().join("folder.jpg");
        std::fs::write(&folder, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = ritornello_proto::CoverRef::Path { path: folder.to_string_lossy().into_owned() };

        core.set_identity(Some(serde_json::json!({"kind": "file", "path": "/a.flac"})));
        core.playback = true;
        core.set_source_cover(Some(r.clone()), "files");
        state_rx.borrow_and_update();

        core.handle_event(Event::Path(f.to_string_lossy().into_owned())).await;

        assert!(
            !state_rx.has_changed().unwrap(),
            "no extraction attempted, hence no extra frame"
        );
        let (retained, origin) = core.metadata.selected_cover().unwrap();
        assert_eq!(origin, "files", "the Source's folder.jpg keeps precedence");
        assert_eq!(retained, CoverSource::Ref(r));
    }

    #[tokio::test]
    async fn a_cover_alone_is_retained_and_does_not_clear_the_status() {
        // **The defect the cover-art project's merge produced: every
        // Source cover lost silently.** A cover deliberately arrives
        // alone, as a spontaneous notification, with neither identity nor
        // status (see `SourceMessage::cover`): that is its normal shape.
        // It therefore takes the early return — and the application the
        // merge added lived at the very bottom of `handle_source_update`,
        // after that `return`. It was never reached.
        //
        // What is pinned down here is therefore **the application on the
        // early-return path**, not the fact that `cover` is part of
        // `carries_a_fact`: that predicate is a tautology, `serve_source`
        // stamping `can_eject` on every frame (see the body of
        // `handle_source_update`). The frame already passed the guard
        // before `cover` was added to it.
        //
        // The frame is therefore built with `sdk_frame()` and not
        // `bare_update()`: with `can_eject: None`, it would describe a
        // shape the SDK cannot emit, and the status assertion would attest
        // a failure mode that does not exist. This assertion stays, as a
        // second line of defense: it will hold if the stamping ever
        // becomes conditional.
        let (mut core, _np_rx, _state_rx, tmp) = test_core();
        let mut permanent = sdk_frame();
        permanent.status = Some("LIVE".into());
        core.handle_source_update("radio", permanent);

        let image = tmp.path().join("folder.jpg");
        std::fs::write(&image, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let mut cover_alone = sdk_frame();
        cover_alone.cover = Some(ritornello_proto::CoverRef::Path {
            path: image.to_string_lossy().into_owned(),
        });
        core.handle_source_update("radio", cover_alone);

        assert!(
            core.metadata.selected_cover().is_some(),
            "the cover must be retained: the early return is the only path \
             by which a Source cover reaches the core"
        );
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("LIVE"),
            "and the remembered status must survive"
        );
    }

    /// Face 1 of the stale-embedded-path bug: `cover::key` is derived from
    /// the picture's **content**, so every track of an album shares one
    /// cache entry — but the payload behind that entry used to keep naming
    /// whichever track was probed *first*, forever. Move, rename or delete
    /// that one file and the shared entry breaks for the whole album, even
    /// though every other track still carries the exact picture the key
    /// promises.
    ///
    /// Production change that defeats this test: reinstating the `contains`
    /// short-circuit for `CoverSource::Embedded` in `start_cover_fetch`
    /// (i.e. removing the branch this fix adds). See the task report for
    /// the observed failure of this test before the fix.
    #[tokio::test]
    async fn a_second_track_of_the_same_album_refreshes_the_stale_audio_path() {
        let (mut core, _state_rx, mut cover_rx, tmp) = test_core_with_cover_channel();
        let Some(track1) = test_mp3_with_cover(tmp.path()) else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        // Track 2 of the same album: a distinct file, byte-for-byte the
        // same embedded picture — exactly the situation `cover::key` is
        // built to deduplicate.
        let track2 = tmp.path().join("track2.mp3");
        std::fs::copy(&track1, &track2).unwrap();

        let path1 = track1.to_string_lossy().into_owned();
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": path1})));
        core.current_path = Some(path1.clone());
        let r1 = crate::player::mpv::embedded_cover(&path1).expect("track1 carries a cover");
        let key = crate::cover::key(&r1);
        core.extraction_arrived(path1.clone(), Some(r1)).await;
        let (k, ok) = cover_rx.recv().await.expect("the first probe must trigger a real fetch");
        assert_eq!(k, key);
        assert!(ok, "an unseen key must be fetched successfully");
        // **Fed back, and it is not decoration.** In production `main`'s
        // loop drains this channel into `cover_arrived`, which is the only
        // thing that releases `cover_in_flight`. A test that merely reads
        // the message off the channel leaves the marker armed for this key
        // forever: track 2's `start_cover_fetch` then returns on the
        // in-flight guard, sends nothing, and the second `recv` below
        // blocks until the harness kills the run.
        core.cover_arrived(k, ok).await;

        // Track 2 starts playing: same picture, hence the same key — and
        // the entry must therefore be refreshed to name what is playing
        // *now*, not merely confirmed as already known.
        let path2 = track2.to_string_lossy().into_owned();
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": path2})));
        core.current_path = Some(path2.clone());
        let r2 = crate::player::mpv::embedded_cover(&path2).expect("track2 carries the same cover");
        assert_eq!(crate::cover::key(&r2), key, "both tracks must share one cache entry");
        core.extraction_arrived(path2.clone(), Some(r2)).await;
        // A message comes back **either way** — the short-circuit branch
        // reports `(key, true)` just as a real fetch does — so this `recv`
        // proves only that a task ran to completion, never that the entry
        // was refreshed. It is the request below that separates fixed from
        // unfixed. Awaited all the same, and that is what makes the test
        // deterministic: the detached task inserts before it sends, so once
        // this returns the cache is settled and no sleep is needed.
        let (k2, ok2) = cover_rx.recv().await.expect("track 2 must run a cover task of its own");
        assert_eq!(k2, key);
        assert!(ok2);

        // Track 1 is gone. If the shared entry still named it, this route
        // would 404 even though the album's picture is still perfectly
        // available — inside track 2, which the entry must now name.
        std::fs::remove_file(&track1).unwrap();
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let app = crate::status::router(crate::status::AppState {
            covers: core.app_covers().clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get(format!("/api/cover/{key}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the shared entry must have followed track2, not stayed pinned on the deleted track1"
        );
    }

    /// Face 2 of the stale-embedded-path bug, and the more dangerous one:
    /// the entry does not merely go stale, it serves the **wrong picture**
    /// while looking perfectly valid. Key `K` is content-addressed from
    /// track1's original picture. Track1 gets retagged in place (same
    /// path, a new picture). Track7, elsewhere, carries the very picture
    /// track1 used to: its probe recomputes the same `K`, and before this
    /// fix `covers.contains(K)` alone was enough to serve the entry as-is —
    /// which by then names track1's path, and therefore reads its *new*
    /// picture, never the one `K` was ever computed from.
    #[tokio::test]
    async fn a_retag_never_serves_its_stale_picture_under_a_still_valid_key() {
        let (mut core, _state_rx, mut cover_rx, tmp) = test_core_with_cover_channel();
        let Some(track1) = test_mp3_with_cover(tmp.path()) else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let track7 = tmp.path().join("track7.mp3");
        std::fs::copy(&track1, &track7).unwrap();
        // Grabbed from track7, which is never retagged below, so what this
        // test compares against cannot itself be a stale read.
        let old_picture = embedded_picture_bytes(&track7);

        let path1 = track1.to_string_lossy().into_owned();
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": path1})));
        core.current_path = Some(path1.clone());
        let r1 = crate::player::mpv::embedded_cover(&path1).expect("track1 carries a cover");
        let key = crate::cover::key(&r1);
        core.extraction_arrived(path1.clone(), Some(r1)).await;
        let (k, ok) = cover_rx.recv().await.expect("the first probe must trigger a real fetch");
        // Fed back for the same reason as in the test above: `cover_arrived`
        // is what releases `cover_in_flight`, and without it track 7's fetch
        // below never starts and its `recv` never returns.
        core.cover_arrived(k, ok).await;

        // Track 1 is retagged: same path, a deliberately different
        // picture.
        assert!(
            retag_embedded_cover(&track1, "red"),
            "ffmpeg must still be available to retag the file"
        );

        // Track 7 starts playing. It still carries the OLD picture, so it
        // recomputes the very same key `K`.
        let path7 = track7.to_string_lossy().into_owned();
        core.set_identity(Some(serde_json::json!({"kind": "file", "path": path7})));
        core.current_path = Some(path7.clone());
        let r7 = crate::player::mpv::embedded_cover(&path7).expect("track7 carries the old cover");
        assert_eq!(crate::cover::key(&r7), key, "track7 must recompute the same content-addressed key");
        core.extraction_arrived(path7.clone(), Some(r7)).await;
        // Same reading as in the test above: this only says a task ran —
        // the short-circuit reports success too. What it buys is ordering:
        // the insert precedes the send, so the request below sees a settled
        // cache without any sleep. The bytes are what judge the fix.
        cover_rx.recv().await.expect("track 7 must run a cover task of its own");

        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let app = crate::status::router(crate::status::AppState {
            covers: core.app_covers().clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get(format!("/api/cover/{key}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let served = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        // Compared through `content_key` rather than as raw slices, and it
        // is not only for a readable failure: `content_key` is the very
        // function `key` hashes, so this states the promise the URL makes —
        // *these bytes fingerprint to what K was built from* — instead of a
        // two-hundred-byte array diff that says the same thing unreadably.
        assert_eq!(
            crate::cover::content_key(&served),
            crate::cover::content_key(&old_picture),
            "K must keep serving the picture it was computed from — track7's — never track1's new one"
        );
    }
}
