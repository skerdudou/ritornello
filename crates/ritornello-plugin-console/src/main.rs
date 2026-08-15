mod display;

use anyhow::Result;
use async_trait::async_trait;
use display::ConsoleDisplay;
use ritornello_plugin_sdk::{run_display_plugin, DisplayPlugin};
use ritornello_proto::PlayerState;
use std::path::PathBuf;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct ConsolePlugin {
    display: ConsoleDisplay,
}

#[async_trait]
impl DisplayPlugin for ConsolePlugin {
    async fn show(&mut self, state: PlayerState) -> Result<()> {
        self.display.show(&state)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = ritornello_plugin_sdk::socket_path();
    let tty = PathBuf::from(env_or("RITORNELLO_CONSOLE_TTY", "/dev/tty1"));

    let display = ConsoleDisplay::open(&tty)?;
    run_display_plugin(ConsolePlugin { display }, &socket_path).await
}
