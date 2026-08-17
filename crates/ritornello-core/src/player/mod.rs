pub mod mpv;

use anyhow::Result;

#[async_trait::async_trait]
pub trait Player: Send + Sync + 'static {
    async fn play(&self, uri: &str) -> Result<()>;
    /// Charge `uri` en tant que **liste de lecture**, dépliée sur-le-champ.
    ///
    /// Distinct de `play`, et c'est une leçon payée : `loadfile` sur un `.m3u`
    /// ne le déplie qu'après coup. Mesuré sur mpv 0.37 — `playlist-count` vaut
    /// 1, puis 3 seulement après un `end-file`/`start-file`. Tout ce qui suit
    /// immédiatement (un `playlist-pos`) arrivait donc hors bornes, et le
    /// `playlist-pos = 0` du dépliage ramenait la lecture au début.
    async fn load_list(&self, uri: &str) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn toggle_pause(&self) -> Result<()>;
    async fn next(&self) -> Result<()>;
    async fn prev(&self) -> Result<()>;
    /// Positionne la lecture sur l'élément d'index `n` de la liste courante.
    ///
    /// À n'employer qu'après un `load_list`, jamais après un `play` : c'est
    /// `loadlist` qui garantit que la liste est déjà dépliée au moment où cet
    /// index est appliqué.
    async fn set_playlist_pos(&self, n: i64) -> Result<()>;
    async fn set_volume(&self, volume: u8) -> Result<()>;
    async fn set_mute(&self, mute: bool) -> Result<()>;
    async fn set_audio_device(&self, device: &str) -> Result<()>;
}
