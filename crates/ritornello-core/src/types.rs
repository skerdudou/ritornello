#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Title(String),
    /// Titre annoncé par le flux lui-même (métadonnées ICY, clé `icy-title`).
    ///
    /// Distinct de `Title`, qui vient de `media-title` : ce dernier retombe sur
    /// l'URL du flux quand la station n'envoie rien, ce qui afficherait une URL
    /// en guise de titre. `IcyTitle` ne porte donc que ce que la station a
    /// réellement émis.
    IcyTitle(String),
    // idle-active=false : la lecture a démarré
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
}
