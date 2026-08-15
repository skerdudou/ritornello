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
    /// Tags portés par le **fichier** en cours de lecture, tels que mpv les
    /// expose dans cette même propriété `metadata`.
    ///
    /// Distinct d'`IcyTitle`, et exclusif de lui : l'un décrit un flux, l'autre
    /// un fichier. Le champ `origin` du morceau est déjà renseigné à
    /// l'extraction, l'événement porte donc un morceau prêt à afficher.
    FileTags(ritornello_proto::Morceau),
    // idle-active=false : la lecture a démarré
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
}
