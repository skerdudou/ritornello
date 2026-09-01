//! Publication: the player state and the sources catalog, pushed to the displays, to the SPA and to the metadata plugins.

use super::*;

impl<P: Player> Core<P> {
    /// Broadcasts the structured state of the player: to the SPA, and to
    /// the Display plugins (which compose their own layout from this same frame).
    pub(crate) fn publish_state(&self) {
        let state = self.player_state();
        // Published generously (at the end of every command, in addition to
        // the metadata paths), hence deduplicated: without this guard, every
        // connected browser and every display would receive a frame
        // identical to the previous one.
        self.state_tx.send_if_modified(|current| {
            if *current == state {
                false
            } else {
                *current = state;
                true
            }
        });
        // `known` republished at the same checkpoint as the structured
        // state: this is where every path that has just added or corrected
        // a piece of metadata (ICY, tags, enrichment, cover) ends up
        // converging, and this is what lets a hotplugged `metadata` plugin —
        // or simply one slow to answer — see what is already known without
        // waiting for a hypothetical next identity change, which may never
        // happen as long as the same track plays. `set_identity` builds its
        // own `NowPlaying` (source and identity change too); this
        // `send_if_modified` then merely observes equality and republishes
        // nothing extra.
        let known = self.metadata.known();
        self.now_playing_tx.send_if_modified(|np| {
            if np.known == known {
                false
            } else {
                np.known = known;
                true
            }
        });
    }

    /// What is structural: the declared sources, **in the switching order**
    /// of `SourceCycle`, and the named presets of each one when it knows how
    /// to enumerate them.
    ///
    /// The order comes from `source_order` and not from the table keys: it
    /// is the order clients will see in `listplaylists`, and it must be that
    /// of the `SourceCycle` key — otherwise the list and the key diverge. A
    /// source that does not enumerate still appears, with an empty list: it
    /// exists, and the consumer falls back on `preset_count`.
    pub fn sources_catalog(&self) -> SourcesCatalog {
        SourcesCatalog {
            sources: self
                .source_order
                .iter()
                .map(|name| SourceCatalog {
                    name: name.clone(),
                    presets: self.presets_par_source.get(name).cloned().unwrap_or_default(),
                })
                .collect(),
        }
    }

    /// Broadcasts the catalog to the displays. Twin of `publish_state`, on
    /// **its own** channel.
    ///
    /// Called where the catalog can change, and only there: at the core's
    /// construction (the startup sources), on the arrival of presets, at
    /// `add_source` (a hotplugged source appears in the list) and at
    /// `remove_source` (a plugin that went off disappears from it, otherwise
    /// an MPD client would keep a stored list to act upon). Never from
    /// `publish_state`, and `publish_state` never from here: the two
    /// channels are separated precisely so as not to trigger each other —
    /// otherwise the names of 51 stations would go out again on every frame
    /// per second of playback, and deduplication by equality would catch
    /// nothing since both values would change together.
    ///
    /// Same deduplication as the state, for the same reason: a source that
    /// re-announces the same list — the radio does it at every save of its
    /// admin page — must not wake the displays.
    pub(crate) fn publish_catalog(&self) {
        let sources_catalog = self.sources_catalog();
        self.sources_catalog_tx.send_if_modified(|current| {
            if *current == sources_catalog {
                false
            } else {
                *current = sources_catalog;
                true
            }
        });
    }

    /// Complete state of the player: what is volatile, hence what the SPA
    /// receives as a pushed stream.
    pub fn player_state(&self) -> PlayerState {
        PlayerState {
            source: self.active_source.clone(),
            volume: self.volume,
            muted: self.muted,
            standby: self.standby,
            preset: self.preset,
            preset_name: self.preset_name.clone(),
            preset_count: self.preset_count,
            // Standby wins over the source status: the device sleeps, what
            // the source says no longer applies.
            status: if self.standby { self.standby_status.clone() } else { self.source_status.clone() },
            overlay: self.overlay.as_ref().map(|(o, deadline)| {
                let remaining = deadline.saturating_duration_since(Instant::now()).as_millis();
                // The stored `remaining_ms` is never read: it is rewritten
                // here at every publication. `Overlay` equality ignores it,
                // so this refresh does not undo frame deduplication.
                o.clone().with_remaining(u32::try_from(remaining).unwrap_or(u32::MAX))
            }),
            // Guarded **here**, at publication, and not cleared in each of
            // the five paths that set `playback = false` (stop, standby,
            // source change, end of content, `SourceAction::Stop`).
            // A single point cannot be forgotten; five sprinkled calls would
            // be at the sixth path added, and the bar would stay frozen on
            // the last known value without anything signalling it.
            position_s: if self.playback && !self.standby { self.position_s } else { None },
            // Same reason as above: computed at publication rather than
            // maintained in the five paths that set `playback = false`.
            playback: if !self.playback || self.standby {
                Playback::Stopped
            } else if self.paused {
                Playback::Paused
            } else {
                Playback::Playing
            },
            // `playback` and not `expecting_stream`: the first says
            // "something plays", the second "it is a restartable stream".
            // Seekable content is exactly what plays without being a stream.
            seekable: self.playback && !self.standby && !self.expecting_stream,
            // Nothing to do with what plays: an empty tray still opens, and
            // it is the Source that has the tray. Standby is the only state
            // that cancels it, because it lets no command through.
            can_eject: self.can_eject && !self.standby,
            // A rendering preference, pushed with the rest: a display never
            // fetches anything on the side, and the clock it draws in
            // standby is something it shows. It only moves on a user
            // gesture, so it causes no extra frame.
            clock: ritornello_proto::Clock {
                date: match self.settings.date_format {
                    crate::state::DateFormat::DayMonthYear => ritornello_proto::DateFormat::DayMonthYear,
                    crate::state::DateFormat::YearMonthDay => ritornello_proto::DateFormat::YearMonthDay,
                    crate::state::DateFormat::MonthDayYear => ritornello_proto::DateFormat::MonthDayYear,
                },
                twelve_hour: !self.settings.clock_24h,
            },
            track: {
                let mut m = self.metadata.state();
                // Precedence: the duration measured by mpv wins over the one
                // a plugin announces. `origin` keeps designating who provided
                // the **track** (artist, title, album) and not who provided
                // the duration — an accepted imprecision rather than a second
                // origin field for a single numeric value.
                if self.playback && !self.standby && self.measured_duration_s.is_some() {
                    m.duration_s = self.measured_duration_s;
                }
                m
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    async fn the_player_state_broadcasts_volume_mute_standby_and_source() {
        // The volume is exposed by no route: its place is this pushed
        // channel, with the rest of what is volatile. A branch of
        // `handle_command` that forgot to publish would let the UI display
        // a stale state without anything signalling it — hence publication
        // at the exit of **every** command, and hence this test that walks
        // through them.
        let (mut core, _np_rx, state_rx, _d) = setup_metadata(vec![]);
        core.resume().await.unwrap();
        let initial = state_rx.borrow().clone();
        assert_eq!(initial.volume, 60, "the persisted volume must be known from startup");
        assert_eq!(initial.source, "radio");
        assert!(!initial.muted);
        assert!(!initial.standby);

        core.handle_command(Command::VolumeUp).await.unwrap();
        assert_eq!(state_rx.borrow().volume, 65);
        core.handle_command(Command::VolumeDown).await.unwrap();
        assert_eq!(state_rx.borrow().volume, 60);

        core.handle_command(Command::Mute).await.unwrap();
        assert!(state_rx.borrow().muted);
        core.handle_command(Command::Mute).await.unwrap();
        assert!(!state_rx.borrow().muted);

        core.handle_command(Command::Power).await.unwrap();
        assert!(state_rx.borrow().standby, "standby must be visible in the UI");
        core.handle_command(Command::Power).await.unwrap();
        assert!(!state_rx.borrow().standby);
    }

    #[tokio::test]
    async fn the_track_is_flattened_in_the_state_json() {
        // The UI receives a flat object: a single panel, not two levels to
        // tell apart.
        let (mut core, _np_rx, _state_rx, _d) = setup_metadata(vec!["ouifm".into()]);
        core.resume().await.unwrap();
        let id = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment("ouifm", enrichment(id, "Miles Davis", "So What"));
        let json = serde_json::to_value(core.player_state()).unwrap();
        assert_eq!(json["source"], "radio");
        assert_eq!(json["volume"], 60);
        assert_eq!(json["artist"], "Miles Davis", "flattened, not under `track`");
        assert_eq!(json["title"], "So What");
        assert_eq!(json["origin"], "ouifm");
    }

    #[tokio::test]
    async fn the_catalog_follows_the_source_switching_order() {
        // This is the order clients will see in `listplaylists`, and it must
        // be that of `SourceCycle`: otherwise the list and the key diverge.
        //
        // Compared to the order **observed** by pressing the key, and not to
        // `source_order`: comparing the catalog to the field it is built
        // from would prove nothing.
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.add_source("files".into(), Arc::new(FakeSource { name: "files", calls: source_calls }));
        let expected = names(&core.sources_catalog());
        assert_eq!(expected.len(), 3);

        core.handle_command(Command::SelectSource(expected[0].clone())).await.unwrap();
        let mut round = vec![core.active_source().to_string()];
        for _ in 1..expected.len() {
            core.handle_command(Command::SourceCycle).await.unwrap();
            round.push(core.active_source().to_string());
        }
        assert_eq!(expected, round, "the catalog must enumerate in the direction of the key");
    }

    #[tokio::test]
    async fn the_catalog_carries_the_startup_sources_without_waiting_for_a_preset() {
        // The sources wired at the rendezvous are known from construction:
        // it is `Core::new` that publishes, and without this publication the
        // channel would keep its empty `SourcesCatalog::default()`. A display
        // relayed before the first preset — hence before any change — would
        // then read "no source", and an MPD client would answer an empty
        // `listplaylists`.
        //
        // Asserts the **current** value of the channel, the one the relay
        // sends at connection, and not a change: this is exactly what a
        // display that arrives sees.
        let (core, _pc, _sc, _rx, _d) = setup();
        let cat_rx = core.sources_catalog_tx.subscribe();
        assert_eq!(
            names(&cat_rx.borrow()),
            vec!["cd".to_string(), "radio".into()],
            "the catalog must carry the startup sources from construction"
        );
    }

    #[tokio::test]
    async fn the_catalog_does_not_republish_for_an_identical_list() {
        // Same deduplication as the state: a source that re-announces the
        // same list — the radio does it at every save of its admin page —
        // must not wake the displays.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut cat_rx = core.sources_catalog_tx.subscribe();
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        assert!(cat_rx.has_changed().unwrap(), "the first list, though, is a new one");
        let _ = cat_rx.borrow_and_update();

        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        assert!(!cat_rx.has_changed().unwrap(), "the same list must wake nothing");

        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP 2")]));
        assert!(cat_rx.has_changed().unwrap(), "a different list does");
    }

    #[tokio::test]
    async fn publishing_the_state_does_not_republish_the_catalog() {
        // The property of the two separate channels. Without it, 51 station
        // names would travel on every frame per second of playback.
        //
        // What is asserted is **the notification**, not the absence of a
        // call: a coupling that went through `publish_catalog` would be
        // deduplicated, hence would reach no display, hence would not break
        // the property. A `sources_catalog_tx.send(...)` from `publish_state`
        // — the natural way to write the coupling — breaks it, and this test
        // falls.
        let (mut core, _pc, _sc, mut state_rx, _d) = setup();
        core.handle_source_update("radio", with_presets(vec![preset_of(1, "FIP")]));
        let cat_rx = core.sources_catalog_tx.subscribe();
        let seen = cat_rx.borrow().clone();
        let _ = state_rx.borrow_and_update();

        core.handle_command(Command::VolumeUp).await.unwrap();
        core.publish_state();
        assert!(state_rx.has_changed().unwrap(), "the state, for its part, did move");
        assert!(!cat_rx.has_changed().unwrap(), "the catalog moved for nothing");
        assert_eq!(*cat_rx.borrow(), seen, "and it still carries the same thing");
    }

    #[tokio::test]
    async fn standby_wins_over_the_source_status() {
        // The device sleeps: what the source says no longer applies, even
        // if it keeps (in practice it does not) declaring one.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        let mut update = bare_update();
        update.status = Some("FIP".into());
        core.handle_source_update("radio", update);
        assert_eq!(core.player_state().status.as_deref(), Some("FIP"));

        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(
            core.player_state().status.as_deref(),
            Some("STANDBY"),
            "the standby word wins over the stored source status"
        );

        // Revision I2 (branch review): this test previously asserted that
        // waking gave the floor back to the stored status ("FIP"), unchanged
        // as long as the Source did not redeclare a new one. That was
        // exactly the bug reported by the review — a source's status could
        // survive standby and reappear under a source that has not said
        // anything yet (see `the_source_status_does_not_survive_entering_standby`).
        // Standby now forgets it, like `preset_count`.
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(
            core.player_state().status,
            None,
            "waking must not make a status reappear that the source has not redeclared"
        );
    }
}
