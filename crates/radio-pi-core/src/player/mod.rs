pub mod mpv;

use anyhow::Result;

#[async_trait::async_trait]
pub trait Player: Send + Sync + 'static {
    async fn play(&self, uri: &str) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn toggle_pause(&self) -> Result<()>;
    async fn next(&self) -> Result<()>;
    async fn prev(&self) -> Result<()>;
    async fn set_volume(&self, volume: u8) -> Result<()>;
    async fn set_mute(&self, mute: bool) -> Result<()>;
    async fn set_audio_device(&self, device: &str) -> Result<()>;
}
