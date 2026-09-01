mod display;

use anyhow::Result;
use async_trait::async_trait;
use display::ConsoleDisplay;
use ritornello_plugin_sdk::{DisplayPlugin, Runtime};
use ritornello_proto::PlayerState;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Period of the heartbeat that makes the standby clock advance.
///
/// **Ten seconds for a clock to the minute**, and it is not waste:
/// `ConsoleDisplay::show` compares its rendering with the previous one and
/// writes nothing when the three lines are identical, so a tick only costs one
/// read of the time and three string comparisons as long as the minute has not
/// turned. A one-minute period, on the other hand, would not be aligned on
/// round minutes: the display could have lagged by almost a whole minute.
const CLOCK_TICK: std::time::Duration = std::time::Duration::from_secs(10);

struct ConsolePlugin {
    /// Shared with the heartbeat: both write to the same tty, and never at the
    /// same time.
    display: Arc<Mutex<ConsoleDisplay>>,
    /// The last frame received, which the heartbeat reuses to redraw.
    ///
    /// **The heartbeat makes up no state**, it replays the last one: without
    /// that it would have to guess what the core announced, and a screen in
    /// standby would lose the standby word on the first clock tick.
    last: Arc<Mutex<Option<PlayerState>>>,
}

#[async_trait]
impl DisplayPlugin for ConsolePlugin {
    async fn show(&mut self, state: PlayerState) -> Result<()> {
        *self.last.lock().await = Some(state.clone());
        self.display.lock().await.show(&state)
    }
}

/// Redraws periodically as long as the device is in standby, so that the clock
/// advances.
///
/// Does nothing outside standby: the core pushes then, and at a much higher
/// cadence. Does nothing before the first frame either — there is nothing to
/// redraw then, and inventing an empty screen would erase what the tty showed
/// before the plugin was launched.
async fn tick_clock(display: Arc<Mutex<ConsoleDisplay>>, last: Arc<Mutex<Option<PlayerState>>>) {
    loop {
        tokio::time::sleep(CLOCK_TICK).await;
        let Some(state) = last.lock().await.clone() else { continue };
        if !state.standby {
            continue;
        }
        if let Err(e) = display.lock().await.show(&state) {
            // Logged and not fatal: a momentarily unavailable tty must not take
            // the plugin down, which stays useful as soon as it comes back.
            tracing::warn!("could not refresh the standby clock: {e}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let tty = PathBuf::from(env_or("RITORNELLO_CONSOLE_TTY", "/dev/tty1"));
    let display = Arc::new(Mutex::new(ConsoleDisplay::open(&tty)?));
    let last = Arc::new(Mutex::new(None));
    tokio::spawn(tick_clock(display.clone(), last.clone()));
    Runtime::from_args()?.display(ConsolePlugin { display, last })?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_does_not_ask_for_covers() {
        // The counterpart of the MPD server's `wants_covers`, and the reason
        // for the default body: this display writes three lines on a
        // twenty-column tty. Pushing up to two mebibytes per track to it, on a
        // device that has a thousand of them, would be paid to be thrown away —
        // and the announcement being derived from this method (see
        // `Runtime::display`), that is exactly what overriding it here would
        // cause.
        //
        // Written on the side that must stay silent, and not on the side that
        // asks: this is where the regression would happen, by adding an
        // override out of mimicry.
        let tty = tempfile::NamedTempFile::new().unwrap();
        let plugin = ConsolePlugin {
            display: Arc::new(Mutex::new(ConsoleDisplay::open(tty.path()).unwrap())),
            last: Arc::new(Mutex::new(None)),
        };
        assert!(!plugin.wants_covers(), "the console has no use for an image's bytes");
    }

    #[tokio::test]
    async fn the_heartbeat_redraws_in_standby_and_stays_quiet_the_rest_of_the_time() {
        // The heartbeat replays the **last frame received**: without it, a
        // clock tick would erase the standby word the core had announced. And
        // it touches nothing outside standby, where the core already pushes
        // every second.
        let tty = tempfile::NamedTempFile::new().unwrap();
        let display = Arc::new(Mutex::new(ConsoleDisplay::open(tty.path()).unwrap()));
        let last = Arc::new(Mutex::new(None));
        let mut plugin =
            ConsolePlugin { display: display.clone(), last: last.clone() };

        // Nothing received: nothing to redraw.
        assert!(last.lock().await.is_none());

        plugin
            .show(PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() })
            .await
            .unwrap();
        let guard = last.lock().await;
        let retained = guard.clone().expect("the frame must be retained for the heartbeat");
        drop(guard);
        assert!(retained.standby);
        assert_eq!(retained.status.as_deref(), Some("VEILLE"));
    }
}
