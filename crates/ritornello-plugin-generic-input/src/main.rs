mod admin;
mod bindings;
mod devices;
mod learn;
// Only compiled under `cargo test`: `ui_placeholder_js` is not used at
// runtime anywhere in this crate (unlike the core's `placeholder_html`, used
// as a fallback by `web.rs`), only by `build.rs` (separate compilation, via
// `include!`) and by its own tests. Compiling it into the binary at all
// times would trigger a `dead_code` that `-D warnings` would reject.
#[cfg(test)]
mod placeholder;
mod presets;

use crate::admin::GenericInputAdmin;
use crate::bindings::Bindings;
use crate::devices::Hub;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{InputPlugin, Runtime};
use ritornello_proto::InputMessage;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

pub(crate) const GENERIC_INPUT_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Input half: consumes the mpsc fed by all the evdev playback tasks,
/// regardless of the originating device.
struct EvdevInput {
    rx: mpsc::Receiver<InputMessage>,
}

#[async_trait::async_trait]
impl InputPlugin for EvdevInput {
    async fn next_command(&mut self) -> Result<InputMessage> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("all evdev loops have ended"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let bindings_path =
        PathBuf::from(env_or("RITORNELLO_INPUT_BINDINGS", "/etc/ritornello/input-bindings.toml"));
    let presets_root =
        PathBuf::from(env_or("RITORNELLO_INPUT_PRESETS", "/etc/ritornello/input-presets"));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    // An Input plugin does not receive a `SetLocale` (the protocol only
    // provides it for sources): the page's language comes from the environment.
    let locale = env_or("RITORNELLO_LOCALE", "en");
    let catalog = Arc::new(RwLock::new(Catalog::load(
        "generic-input",
        &locale,
        &locales_root,
        GENERIC_INPUT_EN,
    )));

    let (tx, rx) = mpsc::channel(32);
    let hub = Hub::new(Bindings::load(&bindings_path), tx);
    let input_root = PathBuf::from(devices::INPUT_DIR);
    let opened = hub.open_new_devices(&input_root);
    tracing::info!("{opened} input device(s) opened");

    // The two halves stay independent: a page failure must not cut off the
    // remote. `Runtime::run` now holds both, each in its own task — the page
    // is no longer conditional, since the plugin itself announces that it
    // has one.
    let admin = GenericInputAdmin {
        bindings_path,
        presets_root,
        input_root,
        hub,
        catalog,
        locales_root,
    };
    Runtime::from_args()?.input(EvdevInput { rx })?.admin(admin)?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_generic_input_en_is_non_empty() {
        assert!(!ritornello_i18n::try_parse(GENERIC_INPUT_EN).unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_input_half_lives_as_long_as_a_channel_sender_exists() {
        // Regression (2026-07-27): declared without `admin = true` and with
        // no evdev device open (WSL, missing rights on /dev/input), the
        // plugin exited immediately with exit 0 — the `.map(...)` closure
        // that built the admin half captured the hub even without being
        // called, and dropped it; but the hub holds the sender of the
        // commands channel. The contract `main` must uphold is this: as
        // long as a sender lives, `next_command` waits instead of ending.
        let (tx, rx) = mpsc::channel::<InputMessage>(4);
        let mut input = EvdevInput { rx };
        tokio::select! {
            _ = input.next_command() => panic!("next_command must not end while a sender lives"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
        // Sender dropped: clean end, the error names the cause.
        drop(tx);
        let e = input.next_command().await.unwrap_err();
        assert!(e.to_string().contains("evdev loops"));
    }

    // The `default_paths` test that used to live here only tested `env_or`,
    // i.e. `std::env`: it passed for any program whatsoever. Removed rather
    // than kept for the count (review 2026-07-27).
}
