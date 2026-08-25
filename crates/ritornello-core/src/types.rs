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
    /// Chemin du fichier que mpv a réellement ouvert (propriété `path`).
    ///
    /// La seule façon dont le cœur apprend ce détail : il ne l'extrait jamais
    /// de l'identité opaque de la Source, par principe (voir `OBSERVEES` dans
    /// `player::mpv`). Sert uniquement à tenter l'extraction de la pochette
    /// embarquée — un flux n'a pas de chemin exploitable, et mpv le republie
    /// alors telle quelle (son URL), sans que cela porte à conséquence : la
    /// tentative d'extraction s'arrête d'elle-même sur un schéma.
    Path(String),
    // idle-active=false : la lecture a démarré
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
}
