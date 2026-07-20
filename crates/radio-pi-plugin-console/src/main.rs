mod display;

use anyhow::Result;
use async_trait::async_trait;
use display::ConsoleDisplay;
use radio_pi_plugin_sdk::{run_display_plugin, DisplayPlugin};
use radio_pi_proto::View;
use std::path::PathBuf;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct ConsolePlugin {
    display: ConsoleDisplay,
}

#[async_trait]
impl DisplayPlugin for ConsolePlugin {
    async fn show(&mut self, view: View) -> Result<()> {
        self.display.show(&view)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = socket_path_from_args();
    let tty = PathBuf::from(env_or("RADIO_PI_CONSOLE_TTY", "/dev/tty1"));

    let display = ConsoleDisplay::open(&tty)?;
    run_display_plugin(ConsolePlugin { display }, &socket_path).await
}
