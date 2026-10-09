//! A source asking to be played: `PlayRequest`, carried by a spontaneous notification, and what the core decides to do with it.

use super::*;
use ritornello_proto::PlayRequest;

impl<P: Player> Core<P> {
    /// Entry point of every Source frame the main loop receives: the frame's
    /// facts first (`handle_source_update`), then its play request if it
    /// carries one. Returns whether it did — the caller then refreshes what
    /// it mirrors of the active source, as after a command.
    ///
    /// A request is taken out **before** the frame is applied, because
    /// applying consumes it; it is acted on **after**, so that a frame which
    /// both declares something (a disc's track count, say) and asks to be
    /// played is described before it is played. A failed request is logged,
    /// never returned: a source's wish going wrong is not the main loop's
    /// business to stop for.
    pub async fn handle_source_frame(&mut self, name: &str, mut update: SourceUpdate) -> bool {
        let request = update.play_request.take();
        self.handle_source_update(name, update);
        let Some(request) = request else { return false };
        if let Err(e) = self.handle_play_request(name, request).await {
            tracing::warn!("play request from {name}: {e:#}");
        }
        true
    }

    /// Acts on what a source asked for: be played if it is already the
    /// active one, be switched to, or wake the device and be switched to.
    ///
    /// **The core decides; the source only states what it wants.** The
    /// owner's rule is that a decision is carried by the component that knows
    /// what it needs to know, and only the core knows whether the device is in
    /// standby, which source is active and whether something already plays. A
    /// source that sent `Switch` gets nothing in standby, and one that is
    /// already playing is not restarted: the request is a wish, never an
    /// order (see `PlayRequest`).
    ///
    /// **`Play`, not `Activate`, to the incoming source.** `Activate` means
    /// "you are now the one", which a source may answer by playing nothing —
    /// that is precisely what the cd's arrival setting is for. A source that
    /// asked to be played answers its own request; sending it `Activate`
    /// would let that setting mute it, and sending `Activate` then `Play`
    /// would load the content twice.
    ///
    /// **Standby is left without waking the old source.** The Power key's
    /// wake sends `Wake` to the active source, which answers by playing what
    /// it played before standby. Here the device is about to switch to the
    /// requesting source: waking the old one would start the radio for an
    /// instant, only to cut it off for the disc. So the flag is lowered and
    /// written exactly as the Power key does it (`leave_standby`), mpv is
    /// prepared (`prepare_player`), and the switch sends the only request
    /// that plays.
    ///
    /// A request from a name that is not in the cycle order — unknown, just
    /// switched off, or forgotten after its plugin died while its frame was in
    /// the channel — does nothing at all, before any state is touched.
    ///
    /// Publishes on the way out, even on error, like `handle_command`: the
    /// partial state reached is what the displays must show, and the channel
    /// deduplicates.
    pub async fn handle_play_request(&mut self, name: &str, request: PlayRequest) -> Result<()> {
        let outcome = self.act_on_play_request(name, request).await;
        self.publish_state();
        outcome
    }

    async fn act_on_play_request(&mut self, name: &str, request: PlayRequest) -> Result<()> {
        // One journal line per decision, whatever it is: the owner who
        // inserts a disc and hears nothing (or hears it start) has
        // `journalctl` to find out which rule answered.
        if !self.source_order.iter().any(|n| n == name) {
            tracing::info!("play request {request:?} from {name} ignored: not a wired source");
            return Ok(());
        }
        let mut request = request;
        if self.standby {
            if request != PlayRequest::WakeAndSwitch {
                tracing::info!("play request {request:?} from {name} ignored: the device is in standby");
                return Ok(());
            }
            tracing::info!("play request {request:?} from {name}: leaving standby for {name}");
            self.leave_standby();
            self.prepare_player().await?;
            request = PlayRequest::Switch;
        }
        if name != self.active_source {
            if request == PlayRequest::IfActive {
                tracing::info!(
                    "play request {request:?} from {name} ignored: not the active source ({})",
                    self.active_source
                );
                return Ok(());
            }
            tracing::info!("play request {request:?} from {name}: switching to {name}");
            return self.cycle_source(Some(name.to_string()), SourceReq::Play).await;
        }
        if self.playback {
            tracing::info!("play request {request:?} from {name} ignored: {name} already plays");
            return Ok(());
        }
        tracing::info!("play request {request:?} from {name}: asking {name} to play");
        if let Some(action) = self.active_request(SourceReq::Play).await? {
            self.apply(action).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::test_support::*;
    use crate::core::*;
    use ritornello_proto::PlayRequest;

    /// The source requests recorded, play-mode broadcasts left out: every
    /// wake and every switch hands the mode out (`push_play_mode`,
    /// `send_play_mode_to`), and it asks no source to play.
    fn calls(log: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        log.lock().unwrap().iter().filter(|c| !c.contains(":SetPlayMode")).cloned().collect()
    }

    #[tokio::test]
    async fn a_request_from_an_unknown_source_does_nothing() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        for request in [PlayRequest::IfActive, PlayRequest::Switch, PlayRequest::WakeAndSwitch] {
            core.handle_play_request("ghost", request).await.unwrap();
        }
        assert!(calls(&source_calls).is_empty(), "no source was asked anything: {:?}", calls(&source_calls));
        assert!(calls(&player_calls).is_empty(), "mpv was not touched: {:?}", calls(&player_calls));
        assert_eq!(core.active_source(), "radio");
    }

    /// The plugin of the requesting source died and was forgotten while its
    /// frame was still in the channel: the name left the cycle order, and a
    /// late request must not resurrect it as the active source.
    #[tokio::test]
    async fn a_request_from_a_forgotten_source_does_nothing() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        assert!(core.forget_dead_source("cd"));
        core.handle_play_request("cd", PlayRequest::Switch).await.unwrap();
        assert_eq!(core.active_source(), "radio");
        assert!(calls(&source_calls).is_empty(), "{:?}", calls(&source_calls));
    }

    #[tokio::test]
    async fn if_active_on_another_source_does_nothing() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.handle_play_request("cd", PlayRequest::IfActive).await.unwrap();
        assert_eq!(core.active_source(), "radio");
        assert!(calls(&source_calls).is_empty(), "{:?}", calls(&source_calls));
        assert!(calls(&player_calls).is_empty(), "{:?}", calls(&player_calls));
    }

    #[tokio::test]
    async fn if_active_on_the_active_idle_source_asks_it_to_play() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.handle_play_request("radio", PlayRequest::IfActive).await.unwrap();
        assert_eq!(calls(&source_calls), vec!["radio:Play".to_string()], "Play, not Activate");
        assert!(calls(&player_calls).contains(&"play http://fip".to_string()));
        assert!(core.playback);
    }

    #[tokio::test]
    async fn switch_from_another_source_switches_and_sends_play_not_activate() {
        let (mut core, player_calls, source_calls, mut state_rx, dir) = setup();
        core.handle_play_request("cd", PlayRequest::Switch).await.unwrap();
        let log = calls(&source_calls);
        assert!(log.contains(&"radio:Deactivate".to_string()), "{log:?}");
        assert!(log.contains(&"cd:Play".to_string()), "the incoming source is asked to play: {log:?}");
        assert!(!log.contains(&"cd:Activate".to_string()), "never Activate on a requested switch: {log:?}");
        assert!(calls(&player_calls).contains(&"play cdda://".to_string()));
        assert_eq!(core.active_source(), "cd");
        assert_eq!(crate::state::load(&dir.path().join("state.json")).active_source, "cd", "the switch is persisted");
        assert_eq!(state_rx.borrow_and_update().source, "cd", "the displays see the new source");
    }

    #[tokio::test]
    async fn a_request_from_the_active_source_already_playing_restarts_nothing() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(core.playback, "precondition: the radio plays after the wake");
        source_calls.lock().unwrap().clear();
        player_calls.lock().unwrap().clear();
        for request in [PlayRequest::IfActive, PlayRequest::Switch, PlayRequest::WakeAndSwitch] {
            core.handle_play_request("radio", request).await.unwrap();
        }
        assert!(calls(&source_calls).is_empty(), "nothing asked of the source: {:?}", calls(&source_calls));
        assert!(calls(&player_calls).is_empty(), "nothing restarted in mpv: {:?}", calls(&player_calls));
    }

    #[tokio::test]
    async fn in_standby_only_wake_and_switch_acts() {
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        source_calls.lock().unwrap().clear();
        player_calls.lock().unwrap().clear();
        for (name, request) in [
            ("cd", PlayRequest::IfActive),
            ("cd", PlayRequest::Switch),
            ("radio", PlayRequest::IfActive),
            ("radio", PlayRequest::Switch),
        ] {
            core.handle_play_request(name, request).await.unwrap();
            assert!(core.player_state().standby, "{name} {request:?} left standby");
        }
        assert!(calls(&source_calls).is_empty(), "{:?}", calls(&source_calls));
        assert!(calls(&player_calls).is_empty(), "{:?}", calls(&player_calls));
        assert_eq!(core.active_source(), "radio");
        assert!(crate::state::load(&dir.path().join("state.json")).standby, "standby still on disk");
    }

    #[tokio::test]
    async fn wake_and_switch_leaves_standby_without_waking_the_old_source() {
        let (mut core, player_calls, source_calls, mut state_rx, dir) = setup();
        core.set_audio_device(Some("bluealsa:DEV=XX".into())).await.unwrap();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(state_rx.borrow_and_update().standby);
        source_calls.lock().unwrap().clear();
        player_calls.lock().unwrap().clear();
        core.handle_play_request("cd", PlayRequest::WakeAndSwitch).await.unwrap();
        // mpv is prepared as the Power key's wake prepares it (final review,
        // F3): the volume and the output the owner chose, and the play mode
        // handed to every source — the old one included, which a switch alone
        // never reaches.
        let mpv = player_calls.lock().unwrap().clone();
        assert!(mpv.iter().any(|c| c.starts_with("vol ")), "volume set on the wake: {mpv:?}");
        assert!(mpv.contains(&"audio_device bluealsa:DEV=XX".to_string()), "audio output set on the wake: {mpv:?}");
        let all = source_calls.lock().unwrap().clone();
        assert!(all.iter().any(|c| c.starts_with("radio:SetPlayMode")), "play mode broadcast on the wake: {all:?}");
        let log = calls(&source_calls);
        assert!(!log.iter().any(|c| c.contains("Wake")), "no source woken: {log:?}");
        assert!(!log.contains(&"radio:Play".to_string()) && !log.contains(&"radio:Activate".to_string()), "the old source is never asked to play: {log:?}");
        assert!(log.contains(&"cd:Play".to_string()), "{log:?}");
        assert_eq!(core.active_source(), "cd");
        let state = state_rx.borrow_and_update().clone();
        assert!(!state.standby, "the displays left standby");
        assert_eq!(state.source, "cd");
        let disk = crate::state::load(&dir.path().join("state.json"));
        assert!(!disk.standby, "the wake is persisted");
        assert_eq!(disk.active_source, "cd");
    }

    /// The active source itself asks to wake the device: it is the one asked
    /// to play, with `Play` — never `Wake`, which the Power key sends.
    #[tokio::test]
    async fn wake_and_switch_from_the_active_source_asks_it_to_play() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_play_request("radio", PlayRequest::WakeAndSwitch).await.unwrap();
        assert_eq!(calls(&source_calls), vec!["radio:Play".to_string()]);
        assert!(!core.player_state().standby);
    }

    #[tokio::test]
    async fn the_power_key_still_wakes_the_active_source() {
        let (mut core, _pc, source_calls, _rx, dir) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(crate::state::load(&dir.path().join("state.json")).standby);
        source_calls.lock().unwrap().clear();
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(calls(&source_calls), vec!["radio:Wake".to_string()]);
        assert!(!core.player_state().standby);
        assert!(!crate::state::load(&dir.path().join("state.json")).standby);
    }

    #[tokio::test]
    async fn every_ordinary_switch_still_activates() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(calls(&source_calls).contains(&"cd:Activate".to_string()), "{:?}", calls(&source_calls));
        source_calls.lock().unwrap().clear();
        core.handle_command(Command::SelectSource("radio".into())).await.unwrap();
        let log = calls(&source_calls);
        assert!(log.contains(&"radio:Activate".to_string()), "{log:?}");
        assert!(!log.iter().any(|c| c.ends_with(":Play")), "{log:?}");
    }

    /// Final review, F2: every decision leaves one line at `info`, the level
    /// the device's journal keeps, naming the request and the source.
    #[tokio::test]
    async fn every_decision_is_written_to_the_journal() {
        use tracing_subscriber::fmt::MakeWriter;
        #[derive(Clone, Default)]
        struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Buffer;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        // `#[tokio::test]` runs on one thread: the default holds across awaits.
        let _guard = tracing::subscriber::set_default(subscriber);
        let take = || String::from_utf8(std::mem::take(&mut *buffer.0.lock().unwrap())).unwrap();

        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.handle_play_request("ghost", PlayRequest::Switch).await.unwrap();
        assert!(take().contains("play request Switch from ghost ignored: not a wired source"));
        core.handle_play_request("cd", PlayRequest::IfActive).await.unwrap();
        assert!(take().contains("play request IfActive from cd ignored: not the active source (radio)"));
        core.handle_play_request("radio", PlayRequest::IfActive).await.unwrap();
        assert!(take().contains("play request IfActive from radio: asking radio to play"));
        core.handle_play_request("radio", PlayRequest::Switch).await.unwrap();
        assert!(take().contains("play request Switch from radio ignored: radio already plays"));
        core.handle_command(Command::Power).await.unwrap();
        take();
        core.handle_play_request("cd", PlayRequest::Switch).await.unwrap();
        assert!(take().contains("play request Switch from cd ignored: the device is in standby"));
        core.handle_play_request("cd", PlayRequest::WakeAndSwitch).await.unwrap();
        let log = take();
        assert!(log.contains("play request WakeAndSwitch from cd: leaving standby for cd"), "{log}");
        assert!(log.contains("play request Switch from cd: switching to cd"), "{log}");
    }

    /// The path the main loop takes: a frame carrying a request reaches
    /// `handle_play_request`, and the caller is told so.
    #[tokio::test]
    async fn a_frame_carrying_a_request_is_acted_on() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        let mut frame = bare_update();
        frame.play_request = Some(PlayRequest::IfActive);
        assert!(core.handle_source_frame("radio", frame).await);
        assert_eq!(calls(&source_calls), vec!["radio:Play".to_string()]);
        assert!(!core.handle_source_frame("radio", bare_update()).await, "a frame without a request reports none");
    }
}
