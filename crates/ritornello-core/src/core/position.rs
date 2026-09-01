//! Playback position: the progress mpv reports, the anchor a plugin sets, and what makes them stale.

use super::*;

impl<P: Player> Core<P> {
    /// Re-reads where we are, from the provider that has the right to speak.
    ///
    /// Two providers, never competing: mpv for finite content, a `metadata`
    /// plugin for a stream. The `time-pos` of a stream counts from the start
    /// of the connection and has nothing to do with the track — it is read
    /// and discarded, never published.
    ///
    /// Publishes nothing: the caller decides (the tick publishes,
    /// `handle_command` already publishes on exit).
    pub async fn refresh_position(&mut self) {
        if self.standby || !self.playback {
            self.forget_position();
            return;
        }
        if self.expecting_stream {
            // Stream: mpv's `time-pos` counts from the start of the
            // connection, unrelated to the track. The position therefore
            // comes from a `metadata` plugin, anchored at its reception and
            // advanced here.
            self.measured_duration_s = None;
            self.position_s = self.position_anchor.map(|(start, set_at)| {
                let elapsed = set_at.elapsed().as_secs();
                let raw = start.saturating_add(u32::try_from(elapsed).unwrap_or(u32::MAX));
                // Capped by the announced duration: a track that ends before
                // the station announces it must not display "4:31 / 4:14".
                match self.metadata.duration_s() {
                    Some(duration) => raw.min(duration),
                    None => raw,
                }
            });
            return;
        }
        match self.player.progress().await {
            Ok(p) => {
                self.position_s = p.position_s.map(|s| s as u32);
                self.measured_duration_s = p.duration_s.filter(|d| *d > 0.0).map(|s| s as u32);
            }
            Err(e) => {
                // An unreadable position does not stop the music: we simply
                // stop announcing one.
                tracing::debug!("playback progress unavailable: {e}");
                self.position_s = None;
                self.measured_duration_s = None;
            }
        }
    }

    /// Nothing plays anymore: nothing left to locate.
    pub(super) fn forget_position(&mut self) {
        self.position_s = None;
        self.measured_duration_s = None;
        self.position_anchor = None;
    }
}

#[cfg(test)]
mod tests {
    use crate::core::*;
    use crate::core::test_support::*;

    #[tokio::test]
    async fn mpv_position_is_published_on_finite_content() {
        // The active source of `setup()` is `radio` (`PersistedState::default`):
        // `SourceCycle` switches to `cd`, which answers `play("cdda://").finite()` —
        // finite content.
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.set_progress(Some(87.4), Some(254.0));
        core.refresh_position().await;
        let state = core.player_state();
        assert_eq!(state.position_s, Some(87), "truncated, never rounded up");
        assert_eq!(state.track.duration_s, Some(254));
        assert!(state.seekable, "a disc can be seeked");
        // 87.6 rather than 87.4: above the half second, a truncation and a
        // rounding no longer give the same integer, and the test finally
        // tells the two implementations apart.
        core.set_progress(Some(87.6), Some(254.0));
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(87));
    }

    /// On a stream, `time-pos` counts from the start of the connection and
    /// has nothing to do with the track: it is read and discarded. Without
    /// this guard, the radio would display a growing listening counter in
    /// place of the position within the track.
    #[tokio::test]
    async fn mpv_position_is_discarded_on_a_stream() {
        let (mut core, _, _, _, _dir) = setup();
        // The active source is already `radio`: `PlayPause` with nothing
        // playing asks it to activate again, and the fake answers
        // `play("http://fip")` without `finite`.
        core.handle_command(Command::PlayPause).await.unwrap();
        core.set_progress(Some(1234.0), Some(0.0));
        core.refresh_position().await;
        let state = core.player_state();
        assert_eq!(state.position_s, None);
        assert!(!state.seekable, "a live stream cannot be rewound");
    }

    /// Regression: `refresh_position` only cleared `measured_duration_s` in
    /// the stream branch, leaving `position_s` frozen on the last value
    /// measured for a disc. `playback` goes back to `true` as soon as it
    /// went to `false` during a `SourceCycle` (the core reactivates the new
    /// source right away), so the `!self.playback` guard never fires
    /// between the two and the disc position survived, displayed
    /// indefinitely under the stream that took its place.
    #[tokio::test]
    async fn a_disc_position_does_not_survive_the_switch_to_a_stream() {
        let (mut core, _, _, _, _dir) = setup();
        // Plays the cd, measures a position.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.set_progress(Some(87.0), Some(254.0));
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(87));
        // Back to the radio: a stream, unrelated to the disc position.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, None, "the disc position must not survive the stream");
    }

    #[tokio::test]
    async fn stop_forgets_the_position() {
        let (mut core, _, _, _, _dir) = setup();
        // Switch to `cd`, finite content: see the test above.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.set_progress(Some(87.0), Some(254.0));
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(87));
        core.handle_command(Command::Stop).await.unwrap();
        let state = core.player_state();
        assert_eq!(state.position_s, None, "nothing plays anymore, nothing left to locate");
        assert_eq!(state.track.duration_s, None);
        assert!(!state.seekable);
    }

    /// The duration measured by mpv wins over the one a plugin announces:
    /// the real disc prevails over what an online database says about it.
    #[tokio::test]
    async fn mpv_duration_wins_over_a_plugin_s() {
        let (mut core, _np_rx, _state_rx, _dir) = setup_metadata(vec!["musicbrainz".into()]);
        // Switch to `cd`, finite content: otherwise `refresh_position`
        // would discard mpv's measurement as if it were a stream.
        core.handle_command(Command::SourceCycle).await.unwrap();
        let id = serde_json::json!({"disc": "abc", "track": 2});
        core.handle_source_update("cd", plays(id.clone()));
        core.handle_enrichment(
            "musicbrainz",
            Enrichment {
                identity: id,
                title: Some("So What".into()),
                duration_s: Some(999),
                ..Default::default()
            },
        );
        core.set_progress(Some(10.0), Some(545.0));
        core.refresh_position().await;
        assert_eq!(core.player_state().track.duration_s, Some(545));
    }

    /// Between two polls of the live stream — several tens of seconds at
    /// Radio France — it is the core that advances the bar, from the anchor
    /// set at reception.
    #[tokio::test]
    async fn an_enrichment_anchor_advances_on_its_own() {
        let (mut core, _np_rx, _state_rx, _dir) = setup_metadata(vec!["radiofrance".into()]);
        // A **stream**: the only context where the anchor speaks (on finite
        // content, mpv has the floor). `radio` is already the active source.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(254),
                position_s: Some(87),
                ..Default::default()
            },
        );
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(87));
        core.advance_anchor_for_test(std::time::Duration::from_secs(3));
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(90));
    }

    /// A track that ends before the station announces it must not display
    /// "4:31 / 4:14".
    #[tokio::test]
    async fn the_announced_position_is_capped_by_the_duration() {
        let (mut core, _np_rx, _state_rx, _dir) = setup_metadata(vec!["radiofrance".into()]);
        // Stream: `radio` is already the active source of this rig.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(100),
                position_s: Some(98),
                ..Default::default()
            },
        );
        core.advance_anchor_for_test(std::time::Duration::from_secs(30));
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(100));
    }

    /// The anchor of the previous track must not keep advancing under the
    /// title of the next one.
    #[tokio::test]
    async fn an_identity_change_clears_the_anchor() {
        let (mut core, _np_rx, _state_rx, _dir) = setup_metadata(vec!["radiofrance".into()]);
        // Stream: `radio` is already the active source of this rig.
        core.handle_command(Command::PlayPause).await.unwrap();
        let first = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", plays(first.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment { identity: first, title: Some("A".into()), position_s: Some(50), ..Default::default() },
        );
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(50));
        core.handle_source_update("radio", plays(serde_json::json!({"url": "deux"})));
        // Even before the refresh: the position of the previous track must
        // not survive under the title of the next one (fixed defect).
        assert_eq!(core.player_state().position_s, None, "stale position under the next title");
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, None);
    }

    /// Regression: a plugin held in reserve that answers (corrected title,
    /// cover found later) must not re-anchor the position on the —
    /// unchanged — value of the winner, otherwise the bar would brutally
    /// move back by everything it had advanced since the winner's previous
    /// announcement.
    #[tokio::test]
    async fn a_plugin_in_reserve_does_not_move_the_position_back() {
        let (mut core, _np_rx, _state_rx, _dir) =
            setup_metadata(vec!["radiofrance".into(), "ouifm".into()]);
        // Stream: `radio` is already the active source of this rig.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", plays(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id.clone(),
                title: Some("Bikwix".into()),
                position_s: Some(87),
                ..Default::default()
            },
        );
        core.advance_anchor_for_test(std::time::Duration::from_secs(30));
        core.refresh_position().await;
        assert_eq!(core.player_state().position_s, Some(117));
        // `ouifm` answers, but is not the winner: nothing new on the
        // progress.
        core.handle_enrichment(
            "ouifm",
            Enrichment { identity: id, title: Some("Other title".into()), ..Default::default() },
        );
        core.refresh_position().await;
        assert_eq!(
            core.player_state().position_s,
            Some(117),
            "a plugin in reserve must not move the position back"
        );
    }
}
