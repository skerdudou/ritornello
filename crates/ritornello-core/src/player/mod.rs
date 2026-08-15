pub mod mpv;

use anyhow::Result;

#[async_trait::async_trait]
pub trait Player: Send + Sync + 'static {
    async fn play(&self, uri: &str) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn toggle_pause(&self) -> Result<()>;
    async fn next(&self) -> Result<()>;
    async fn prev(&self) -> Result<()>;
    /// Positionne la lecture sur l'élément d'index `n` de la liste courante.
    ///
    /// Employé juste après un `play` désignant une liste (`.m3u`) : mpv la
    /// résout dès la commande de chargement, il n'y a donc pas de dépliage
    /// différé à attendre avant de pouvoir se positionner.
    async fn set_playlist_pos(&self, n: i64) -> Result<()>;
    async fn set_volume(&self, volume: u8) -> Result<()>;
    async fn set_mute(&self, mute: bool) -> Result<()>;
    async fn set_audio_device(&self, device: &str) -> Result<()>;
}
