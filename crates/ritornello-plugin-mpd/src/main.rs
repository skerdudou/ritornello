mod admin;
mod commands;
mod config;
mod state;
// Only compiled under `cargo test`: `ui_placeholder_js` is used nowhere at
// run time in this crate, only by `build.rs` (separate compilation, via
// `include!`) and by its own tests. Compiling it permanently into the binary
// would trigger a `dead_code` that `-D warnings` would refuse (see
// `generic-input/src/main.rs`, same trap).
#[cfg(test)]
mod placeholder;
mod protocol;
mod session;

use admin::MpdAdmin;
use anyhow::Result;
use config::Config;
use state::SharedState;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{DisplayPlugin, InputPlugin, Runtime};
use ritornello_proto::{SourcesCatalog, Cover, InputMessage, PlayerState};
use session::listen;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

pub(crate) const MPD_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// `display` half: receives each frame from the core and drops it into the
/// shared state, read by the client sessions.
struct MpdDisplay {
    state: Arc<SharedState>,
}

#[async_trait::async_trait]
impl DisplayPlugin for MpdDisplay {
    async fn show(&mut self, state: PlayerState) -> Result<()> {
        self.state.apply_state(state).await;
        Ok(())
    }

    /// The catalog, on its own channel: it is where the saved playlists
    /// (`listplaylists`) and the real names of the queue come from.
    ///
    /// The trait's default body ignores this frame; this plugin is the first
    /// display to take an interest in it. A single call at startup, then one
    /// per real change — not one per state frame, and that is the whole point
    /// of the two channels (see `SharedState::apply_catalog`).
    async fn sources_catalog(&mut self, c: SourcesCatalog) -> Result<()> {
        self.state.apply_catalog(c).await;
        Ok(())
    }

    /// **This is the line that switches on the cover feature for the whole
    /// device.** The core only pushes the bytes to the displays that ask for
    /// them, the announcement is derived from this method (see
    /// `Runtime::display`), and this plugin is the only one in the repository
    /// to override it: the console keeps the default body and therefore never
    /// receives a megabyte it would throw away.
    ///
    /// Overridden because it has a real use for it and not because it *can*:
    /// `albumart` and `readpicture` must return bytes, and no other route gives
    /// them to it — the `cover_href` of the state frame is a URL of the core's
    /// HTTP server, which the plugin has neither the right nor the means to go
    /// and read.
    fn wants_covers(&self) -> bool {
        true
    }

    /// The cover of what is playing. Dropped into the shared state, from which
    /// the sessions serve it in chunks.
    async fn cover(&mut self, c: Cover) -> Result<()> {
        self.state.apply_cover(c).await;
        Ok(())
    }
}

/// `input` half: drains the channel fed by the client sessions.
struct MpdInput {
    rx: mpsc::Receiver<InputMessage>,
}

#[async_trait::async_trait]
impl InputPlugin for MpdInput {
    async fn next_command(&mut self) -> Result<InputMessage> {
        // As long as `accept_loop` runs (its loop is infinite, see
        // `session.rs`), it holds a clone of the sender, so this `recv()`
        // stays pending indefinitely rather than returning `None` — same
        // contract as `EvdevInput::next_command` on the `generic-input` side,
        // where forgetting this property had made the plugin exit with
        // `exit 0` right at startup (regression of 2026-07-27).
        self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("no mpd session sends commands yet"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let path = PathBuf::from(env_or("RITORNELLO_MPD_CONFIG", "/etc/ritornello/mpd.toml"));
    let config = Config::load(&path);

    // **Bound before the announcement.** This is the same doctrine the SDK
    // holds for its Unix sockets — bind first, announce next — and it gives a
    // useful behaviour here: a port 6600 already taken makes the plugin fail
    // (the `?` leaves `main` before even building a `Runtime`) without it
    // announcing itself, so the core reports it dead before announcement and
    // the status page shows it. Otherwise a busy port would have to be guessed
    // from the logs.
    let listener = TcpListener::bind((config.listen.as_str(), config.port)).await?;
    tracing::info!("mpd server listening on {}:{}", config.listen, config.port);

    let state = Arc::new(SharedState::default());
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    // The channel through which the admin page makes the network half rebind,
    // without restarting the plugin (see `session::listen`). A `watch` and not
    // an `mpsc`: only the **last** configuration matters, and two saves in a
    // row must not cause two rebinds of which the first would already be
    // stale.
    let (rebind_tx, rebind_rx) = tokio::sync::watch::channel(config.clone());
    tokio::spawn(listen(listener, rebind_rx, state.clone(), cmd_tx));

    // A Display/Input plugin receives no `SetLocale` (the protocol only
    // provides it for sources): the page's language comes from the
    // environment, as in generic-input.
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    let locale = env_or("RITORNELLO_LOCALE", "en");
    let catalog = Arc::new(RwLock::new(Catalog::load("mpd", &locale, &locales_root, MPD_EN)));
    let admin = MpdAdmin {
        config_path: path,
        config: RwLock::new(config),
        catalog,
        rebind_tx: Some(rebind_tx),
    };

    Runtime::from_args()?
        .input(MpdInput { rx: cmd_rx })?
        .display(MpdDisplay { state })?
        .admin(admin)?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_mpd_en_is_not_empty() {
        assert!(!ritornello_i18n::try_parse(MPD_EN).unwrap().is_empty());
    }

    #[tokio::test]
    async fn mpd_display_drops_the_received_state_into_the_shared_state() {
        let state = Arc::new(SharedState::default());
        let mut display = MpdDisplay { state: state.clone() };
        let sent = PlayerState { volume: 17, ..Default::default() };
        display.show(sent.clone()).await.unwrap();
        assert_eq!(state.read().await.state, sent);
    }

    #[tokio::test]
    async fn mpd_display_drops_the_received_catalog_into_the_shared_state() {
        // The only path through which the catalog enters the plugin: without
        // this override, the trait's default body would ignore it silently and
        // `listplaylists` would stay empty forever.
        let state = Arc::new(SharedState::default());
        let mut display = MpdDisplay { state: state.clone() };
        let sent = SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog {
                name: "radio".into(),
                presets: vec![ritornello_proto::Preset { index: 5, name: "Nova".into() }],
            }],
        };

        display.sources_catalog(sent.clone()).await.unwrap();

        assert_eq!(state.read().await.sources_catalog, sent);
    }

    #[tokio::test]
    async fn a_state_frame_does_not_touch_the_catalog_already_received() {
        // Both halves of the same display write into the same shared state,
        // and each must touch only its own: a `show` that reset the snapshot
        // afresh would erase the catalog received at startup, and nothing
        // would send it again.
        let state = Arc::new(SharedState::default());
        let mut display = MpdDisplay { state: state.clone() };
        let sources_catalog = SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog { name: "radio".into(), presets: vec![] }],
        };
        display.sources_catalog(sources_catalog.clone()).await.unwrap();

        display.show(PlayerState { source: "radio".into(), volume: 17, ..Default::default() }).await.unwrap();

        let snapshot = state.read().await;
        assert_eq!(snapshot.sources_catalog, sources_catalog);
        assert_eq!(snapshot.state.volume, 17);
    }

    #[test]
    fn the_mpd_display_asks_for_covers() {
        // **The opt-in, pinned.** This is the value that switches the feature
        // on for the whole device: the core derives the announcement from
        // `wants_covers` (see `Runtime::display`), and nobody else overrides
        // it. Without this test, reverting to the default body would break
        // *no* other test of the plugin — the session tests push the cover
        // into the shared state directly — and `albumart` would reply
        // `ACK 50` on the real device without anything flagging it.
        let display = MpdDisplay { state: Arc::new(SharedState::default()) };
        assert!(display.wants_covers(), "the MPD server must receive the bytes");
    }

    #[tokio::test]
    async fn mpd_display_drops_the_received_cover_into_the_shared_state() {
        // The only path through which an image enters the plugin. Without this
        // override, the trait's default body would swallow it silently and
        // `albumart` would never reply anything — a plugin that *asks for*
        // covers and throws them away is exactly what the core cannot tell
        // apart on its own.
        let state = Arc::new(SharedState::default());
        let mut display = MpdDisplay { state: state.clone() };
        let sent = Cover {
            href: "/api/cover/1a2b3c".into(),
            mime: "image/jpeg".into(),
            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
        };

        display.cover(sent.clone()).await.unwrap();

        let held = state.read().await.cover.expect("the cover must be held");
        assert_eq!(held.href, sent.href);
        assert_eq!(held.mime, sent.mime);
        assert_eq!(*held.bytes, sent.bytes);
    }

    #[tokio::test(start_paused = true)]
    async fn the_input_half_waits_as_long_as_a_channel_sender_lives() {
        // Targeted regression: the one documented on `EvdevInput` on the
        // `generic-input` side (2026-07-27), where forgetting this property
        // made the plugin exit with exit 0 right at startup for lack of an
        // open device. Here, the sender living as long as `accept_loop` runs,
        // this `next_command` must never finish on its own.
        //
        // Simulated clock (`start_paused`): with no sender sending and no
        // other timer in play, tokio advances virtual time up to the `sleep`
        // deadline as soon as everything else is pending — the property under
        // test therefore does not depend on guessing how long "long enough"
        // is on the machine running the test.
        let (tx, rx) = mpsc::channel::<InputMessage>(4);
        let mut input = MpdInput { rx };
        tokio::select! {
            _ = input.next_command() => panic!("next_command must not finish while a sender lives"),
            _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {}
        }
        // Sender dropped: clean end, the error names the cause.
        drop(tx);
        let e = input.next_command().await.unwrap_err();
        assert!(e.to_string().contains("mpd session"), "unexpected error: {e}");
    }
}
