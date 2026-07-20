use crate::keymap::map_key;
use anyhow::{Context, Result};
use evdev::{Device, EventType};
use ritornello_proto::Command;
use tokio::sync::mpsc;

pub fn find_device(name_contains: &str) -> Result<Device> {
    let needle = name_contains.to_lowercase();
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("").to_lowercase();
        if name.contains(&needle) {
            tracing::info!("télécommande: {} ({})", dev.name().unwrap_or("?"), path.display());
            return Ok(dev);
        }
    }
    anyhow::bail!("aucun périphérique input dont le nom contient « {name_contains} »")
}

pub async fn run(device: Device, tx: mpsc::Sender<Command>) -> Result<()> {
    let mut stream = device.into_event_stream().context("event stream evdev")?;
    loop {
        let ev = stream.next_event().await?;
        if ev.event_type() == EventType::KEY && ev.value() == 1 {
            if let Some(cmd) = map_key(ev.code()) {
                tracing::debug!("touche {} -> {:?}", ev.code(), cmd);
                let _ = tx.send(cmd).await;
            }
        }
    }
}
