mod input;
mod keymap;

use anyhow::Result;
use radio_pi_plugin_sdk::{run_input_plugin, InputPlugin};
use radio_pi_proto::Command;
use std::path::PathBuf;
use tokio::sync::mpsc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct MceInput {
    rx: mpsc::Receiver<Command>,
}

#[async_trait::async_trait]
impl InputPlugin for MceInput {
    async fn next_command(&mut self) -> Result<Command> {
        self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("boucle evdev terminee"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = socket_path_from_args();
    let input_name = env_or("RADIO_PI_MCE_INPUT_NAME", "Media Center");

    let device = input::find_device(&input_name)?;
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        if let Err(e) = input::run(device, tx).await {
            tracing::error!("boucle evdev terminee: {e}");
        }
    });

    run_input_plugin(MceInput { rx }, &socket_path).await
}
