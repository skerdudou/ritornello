//! Ce qu'une commande MPD devient : un instantané en entrée, des lines en
//! sortie. **Aucune E/S, aucune horloge.**
//!
//! Cette pureté est le point du module et non une élégance : la table de
//! correspondance entre une commande MPD et la façade de l'appareil est ce
//! qu'un client voit en premier, et c'est aussi ce qui se vérifie le plus mal
//! à l'œil. Une fonction qui ne fait que choisir se teste line par line ;
//! la session (Task 8) garde pour elle tout ce qui touche la chaussette.
//!
//! Son appelant est `session.rs`, qui read les lines et écrit les réponses :
//! c'est lui, et lui seul, qui appelle `handle`.

use crate::state::{Snapshot, Subsystem};
use crate::protocol::{ack, line, Ack};
use ritornello_proto::{Command, Playback, Preset, SourceCatalog};
use std::ops::Range;
use std::sync::Arc;

/// Ce que le traitement d'une commande demande à la session de faire.
///
/// La décision est **pure** et l'application impure : ce module choisit, la
/// session écrit sur la chaussette et push_cover sur le canal. C'est ce qui rend la
/// table de correspondance vérifiable au test unitaire.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Ces lines, puis `OK` — que la session pose, pas nous : dans une liste
    /// de commands, un seul `OK` clôt l'ensemble.
    Reply { lines: Vec<String>, cmds: Vec<Command> },
    /// `ACK` déjà mis en forme. Dans une liste, elle interrompt la suite.
    Reject(String),
    /// `idle` : wait l'un de ces subsystems.
    ///
    /// **La liste peut etre clear**, et cela ne veut pas dire « repondre tout de
    /// suite » : un client qui n'a nomme que des sous-systemes que ce greffon
    /// n'emet jamais (`idle database`) doit wait pour toujours. C'est le
    /// comportement MPD correct — il a demande a etre prevenu d'un changement
    /// qui n'arrive jamais. La Task 8 ne doit donc pas handle le clear comme un
    /// `OK`.
    Wait(Vec<Subsystem>),
    /// `noidle` reçu hors attente : `OK` sec.
    Cancel,
    /// Une réponse **binaire** : `albumart` et `readpicture`.
    ///
    /// Une issue à part, et non des `lines` : les bytes d'une image ne sont
    /// pas de l'UTF-8, donc ils ne peuvent pas voyager dans le `Vec<String>`
    /// de `Reply` — et surtout ils ne doivent pas traverser l'accumulateur
    /// de texte de la session, qui est ce qui a été trouvé amplificateur d'un
    /// facteur 2048 sur ce même port. Voir `Binary`.
    Bytes(Binary),
    /// `close` : `OK` puis fermeture.
    Close,
    /// `binarylimit <N>` : la session retient cette size de chunk pour ses
    /// réponses binaires, puis répond `OK`.
    ///
    /// Une issue à part parce que c'est un fait sur la **connection** et non sur
    /// l'appareil — la même raison qui fait vivre l'état de liste et l'attente
    /// d'`idle` dans `session.rs`. La valeur portée est déjà bornée (voir
    /// `binarylimit`), la session n'a rien à revérifier.
    BinaryLimit(usize),
}

/// Une réponse binaire toute décidée : l'en-tête textuel, l'image, et la
/// fenêtre de cette réponse dans l'image.
///
/// **L'image est partagée, la chunk est un intervalle** : ce module remainder
/// pur (aucune E/S, aucune allocation d'image), la session n'a plus qu'à
/// écrire. Le clone de l'`Arc` est un incrément de compteur, donc composer
/// cette issue ne recopie **jamais** les bytes, même pour une image de
/// 20 Mio (`COVER_MAX_BYTES`) ; ce que la session écrira est borné par
/// `MAX_CHUNK` et par lui seul. En revanche ce clone **retient** cette
/// génération d'image jusqu'à la fin de l'écriture : voir le produit calculé
/// sur `MAX_CHUNK`.
#[derive(Clone, PartialEq)]
pub struct Binary {
    /// `size: <total>`, et pour `readpicture` `type: <mime>` — dans cet order,
    /// celui de MPD.
    pub header: Vec<String>,
    /// L'image **entière**, partagée avec l'état (jamais copiée).
    pub image: Arc<Vec<u8>>,
    /// La fenêtre à écrire. Toujours dans les bounds de `image` et d'au plus
    /// `MAX_CHUNK` bytes : c'est `albumart` qui l'établit, et la session
    /// s'y fie pour indexer sans vérifier.
    pub chunk: Range<usize>,
}

/// `Debug` écrit à la main, pour la même raison que celui de `HeldCover` : le
/// dérivé imprimerait vingt mébioctets d'image dans le message d'un test raté.
impl std::fmt::Debug for Binary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binary")
            .field("header", &self.header)
            .field("image", &format_args!("{} o", self.image.len()))
            .field("chunk", &self.chunk)
            .finish()
    }
}

impl Outcome {
    /// `OK` sec.
    pub fn ok() -> Self {
        Outcome::Reply { lines: Vec::new(), cmds: Vec::new() }
    }

    pub fn lines(lines: Vec<String>) -> Self {
        Outcome::Reply { lines, cmds: Vec::new() }
    }

    /// `OK` sec, plus une commande à émettre vers le cœur.
    ///
    /// Sans appelant avant la **Task 7** : aucune commande de playback seule
    /// n'agit sur l'appareil, et c'est une propriété qu'un test de ce module
    /// vérifie explicitement.
    pub fn acting(cmd: Command) -> Self {
        Outcome::Reply { lines: Vec::new(), cmds: vec![cmd] }
    }
}

/// Les commands que ce serveur gère réellement, et rien d'autre.
///
/// **C'est la commande `commands` qui rend le greffon honnête** : un client
/// correct y read ce qui existe et grise le remainder de lui-même. La différence
/// entre « des onglets vides » et « des onglets qui plantent » tient à cette
/// liste, donc elle ne doit jamais promettre plus que le `match` de `handle`.
/// Un test parcourt la liste et vérifie que chaque name y est réellement traité.
///
/// Ordre alphabétique : les clients n'en tirent rien, mais un trou se voit.
pub const COMMANDS: &[&str] = &[
    "add",
    "addid",
    "albumart",
    "binarylimit",
    "clear",
    "close",
    "commands",
    "count",
    "currentsong",
    "decoders",
    "find",
    "getvol",
    "idle",
    "list",
    "listall",
    "listallinfo",
    "listfiles",
    "listplaylistinfo",
    "listplaylists",
    "load",
    "lsinfo",
    "next",
    "noidle",
    "notcommands",
    "outputs",
    "password",
    "pause",
    "ping",
    "play",
    "playid",
    "playlistinfo",
    "plchanges",
    "previous",
    "readpicture",
    "search",
    "seek",
    "seekcur",
    "seekid",
    "setvol",
    "stats",
    "status",
    "stop",
    "tagtypes",
    "urlhandlers",
    "volume",
];

/// Une entrée de la file d'attente : son index de présélection (**creux**,
/// base 1, celui que `Command::Select` attend) et son name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub index: u8,
    pub name: String,
}

/// Les entrées d'une liste de présélections nommées, telle que le sources_catalog la
/// donne. Les indices sont recopiés **tels quels**, y compris creux : rien ici
/// ne dérive un rang d'un index.
fn named_entries(presets: &[Preset]) -> Vec<Entry> {
    presets.iter().map(|p| Entry { index: p.index, name: p.name.clone() }).collect()
}

/// La file d'attente MPD : les présélections de la source active.
///
/// **Deux branches, et l'order entre elles est le sujet.**
/// 1. La **vraie liste**, quand le sources_catalog en donne une non clear pour la
///    source active. Ses indices sont ceux de la source, éventuellement
///    **creux** : `preset_count` est le *maximum* des numéros et non leur
///    nombre, donc des stations 1, 5 et 99 sont légales, là où les positions
///    MPD restent denses. La correspondance passe donc par le **rang** dans
///    cette liste (`position_to_index`), jamais par une soustraction de 1.
/// 2. La **synthèse**, à défaut : le greffon fabrique `1..=preset_count`, et la
///    suite est alors dense par construction (`Pos = Id - 1`). C'est le cas du
///    cd, qui ne sait pas énumérer — son entrée de sources_catalog
///    porte une liste clear, ce qui veut dire « je n'ai que des numéros » et non
///    « je n'ai rien ». Retomber sur `preset_count` est alors la seule façon de
///    voir les douze pistes d'un disque.
///
/// `None` devient **zéro entrée** et non les dix de la grille par défaut de
/// l'IHM : cette grille est un pavé numérique, pas une liste. Annoncer dix
/// entrées ferait demander à un client dix choses dont aucune n'existe.
pub fn queue(inst: &Snapshot) -> Vec<Entry> {
    let presets = inst.active_presets();
    if !presets.is_empty() {
        return named_entries(presets);
    }
    let n = inst.state.preset_count.unwrap_or(0);
    (1..=n).map(|i| Entry { index: i, name: i.to_string() }).collect()
}

/// La date que MPD attend sur chaque entrée de `listplaylists`, faute d'en
/// avoir une.
///
/// Aucune date n'existe côté appareil : une source n'est pas un fichier, elle
/// n'a ni date de modification ni rien qui y ressemble, et en fabriquer une
/// depuis l'horloge courante ferait croire à un client qu'une liste vient de
/// changer chaque fois qu'il la relit. Une constante, donc — et l'époque plutôt
/// qu'une date arbitraire, parce qu'elle se read comme « inconnue ».
///
/// **Émise et non omise** : le champ est facultatif dans la documentation du
/// protocol, mais des clients le lisent sans le garder (libmpdclient trie ses
/// listes dessus), et son absence les fait trébucher. Le rendre coûte une line
/// et ne mentira jamais, puisqu'il ne bougera jamais.
const UNKNOWN_DATE: &str = "1970-01-01T00:00:00Z";

/// Traite une commande déjà découpée. `index` est son rang dans une liste de
/// commands (0 hors liste) : il doit traverser jusqu'à l'`ACK`, sinon un
/// client ne sait pas laquelle de ses commands a échoué.
/// `binary_limit` est la size de chunk que **cette connection** accepte
/// (voir `binarylimit`) : un fait sur la connection, que seule la session
/// connaît, et qui n'entre nulle part ailleurs que dans les deux commands de
/// cover.
pub fn handle(
    inst: &Snapshot,
    index: usize,
    args: &[String],
    binary_limit: usize,
) -> Outcome {
    // Line clear : la session ne devrait pas en soumettre, mais cette fonction
    // est totale par construction plutôt que par convention — un `args[0]` sur
    // une chunk clear serait une panique, donc une connection coupée.
    let Some(cmd) = args.first() else {
        return Outcome::Reject(ack(Ack::Unknown, index, "", "unsupported"));
    };
    let remainder = &args[1..];
    match cmd.as_str() {
        "status" => Outcome::lines(status(inst)),
        "currentsong" => Outcome::lines(currentsong(inst)),
        "playlistinfo" => playlistinfo(inst, index, remainder),
        "plchanges" => plchanges(inst, index, remainder),
        // Chaque source du sources_catalog **est** une liste enregistrée : c'est la
        // correspondance qui rend l'appareil lisible depuis un client MPD, où
        // « load la liste radio » veut dire « écouter la radio ».
        "listplaylists" => Outcome::lines(listplaylists(inst)),
        // Interrogeable sur n'importe quelle source, y compris une qui ne plays
        // pas : c'est un fait sur une source, pas sur ce qui plays.
        "listplaylistinfo" => listplaylistinfo(inst, index, remainder),
        "commands" => Outcome::lines(COMMANDS.iter().map(|c| line("command", c)).collect()),
        // Son pendant, que de vieux clients demandent juste apres `commands`.
        // Vide, et c'est la reponse honnete : `notcommands` liste ce que le mot
        // de passe current *interdit*, or il n'y a pas de mot de passe ici (voir
        // la spec, § Reseau), donc rien n'est interdit par permission. Ce qui
        // n'existe pas est simplement absent de `commands`.
        "notcommands" => Outcome::ok(),
        // Les quatre seules étiquettes que `Track` porte. En annoncer
        // d'autres ferait chercher au client des tris que rien n'alimente ; en
        // oublier une que `currentsong` émet est le défaut inverse, et il était
        // là : un client a le droit de ne read que les lines des étiquettes
        // annoncées ici, l'année restait donc invisible chez lui. La liste doit
        // rester le miroir exact de ce que `currentsong` peut push.
        "tagtypes" => Outcome::lines(
            ["Artist", "Album", "Title", "Date"].iter().map(|t| line("tagtype", t)).collect(),
        ),
        // Une sortie unique et toujours active : l'appareil a une sortie audio,
        // que la page d'admin choisit. `enableoutput`/`disableoutput` sont
        // refusées, donc rien ici n'est pilotable — mais un client qui ne voit
        // aucune sortie affiche « muet » et n'insiste pas.
        "outputs" => Outcome::lines(vec![
            line("outputid", 0),
            line("outputname", "default"),
            line("outputenabled", 1),
        ]),
        "stats" => Outcome::lines(stats(inst)),
        // `OK` sec, mais **présentes** : une commande inconnue au montage peut
        // faire renoncer un client avant qu'il n'affiche un écran. Aucune
        // valeur à donner (il n'y a ni greffon de décodage ni schéma d'URL à
        // exposer), et une liste clear est une réponse bien formée.
        "decoders" | "urlhandlers" => Outcome::ok(),
        "ping" => Outcome::ok(),
        // Acceptée sans rien vérifier : le serveur n'a pas de mot de passe (voir
        // la spec, § Réseau), et un client configuré avec un mot de passe ne
        // doit pas être rejeté pour autant. Sans argument non plus : il n'y a
        // rien à contrôler, donc rien à refuser.
        "password" => Outcome::ok(),
        "close" => Outcome::Close,
        // `idle` ne *répond* pas ici : ce module choisit les subsystems, la session
        // (Task 8) tient l'attente et décide qu'un `idle` dans une liste de
        // commands est illégal. Le découpage des names de subsystems, lui, est pur.
        "idle" => idle(index, remainder),
        "noidle" => Outcome::Cancel,
        // `play [POS]` : POS est le **rang** dans la file (base 0, celui que
        // `Pos` publie), jamais l'index de présélection moins un — les deux
        // ne coïncident plus dès qu'une source énumère une liste creuse.
        // Sans argument, ce n'est pas une sélection mais la touche
        // Lecture : on restart ce qui était chargé.
        "play" => play(inst, index, remainder),
        // `playid <ID>` : l'index tel quel, mais vérifié dans la file — un
        // `ID` à l'intérieur du maximum (`preset_count`) sans être une entrée
        // réelle de la file creuse doit refuser ; une bounded ne suffit pas.
        "playid" => playid(inst, index, remainder),
        // Bascule sans argument ; sinon n'agit que si l'état optimiste diffère
        // de la cible — c'est ce qui ferme la course d'un client qui
        // renverrait la même commande deux fois. Voir `pause`.
        "pause" => pause(inst, index, remainder),
        "stop" => Outcome::acting(Command::Stop),
        // La distinction présélection/piste n'est pas d'ici : c'est la source
        // active qui l'interprète (voir la doc de `Command::Next`).
        "next" => Outcome::acting(Command::Next),
        "previous" => Outcome::acting(Command::Prev),
        "setvol" => setvol(inst, index, remainder),
        // Dépréciée par MPD mais encore émise par de vieux clients : relative
        // au volume current, et bornée ici (voir `volume`) plutôt que de
        // laisser déborder `Command::SetVolume`, qui lui est absolu.
        "volume" => volume(inst, index, remainder),
        // `seek`/`seekid` ignorent leur premier argument (position ou id) :
        // `SeekTo` ne sait pas changer de piste en même temps, et MPD n'envoie
        // ce kind de commande que sur ce qui plays déjà.
        "seek" => seek(index, "seek", remainder),
        "seekid" => seek(index, "seekid", remainder),
        // Seule forme qui accepte un relatif (`+n`/`-n`), résolu ici depuis
        // `position_s` puisque `Command::SeekTo` ne porte qu'un absolu.
        "seekcur" => seekcur(inst, index, remainder),
        // **Les deux names répondent exactement la même chose, et ce n'est pas
        // un raccourci.** Pour MPD ce sont deux origines différentes :
        // `albumart` cherche un fichier *à côté* de la piste (un `cover.jpg`
        // dans son dossier), `readpicture` une image *embarquée* dans ses
        // étiquettes. Cet appareil, lui, n'a qu'**une** cover par piste,
        // quelle que soit son origine : le cœur l'a déjà arbitrée entre le
        // fichier voisin, l'étiquette embarquée et le réseau, et n'en publie
        // qu'une. Distinguer ici demanderait au greffon une information que le
        // protocol d'affichage ne porte pas — et surtout, M.A.L.P. essaie l'un
        // puis l'autre : répondre à un seul des deux ferait dépendre l'affichage
        // de la cover de l'order dans lequel le client s'y prend.
        // Seule différence, celle de MPD : `readpicture` publie un `type:`.
        "albumart" => cover(inst, index, "albumart", remainder, binary_limit),
        "readpicture" => cover(inst, index, "readpicture", remainder, binary_limit),
        // La size de chunk que ce client accepte. Gérée et non refusée : à
        // la version que la bannière announcement (0.23.5), un client la considère
        // comme acquise et l'envoie **au moment de se connecter** — M.A.L.P. le
        // fait. Un `ACK 5` en pleine séquence de connection est le pire moment
        // pour être refusé, et la commande a de surcroît un effet réel ici :
        // une cover de 500 Kio demandait soixante-deux allers-retours en
        // tranches de 8 Kio.
        "binarylimit" => binarylimit(index, remainder),
        // Le volume seul, sans le remainder de `status` (MPD 0.23). Un client qui
        // ne veut que le curseur n'a pas à relire quinze lines.
        "getvol" => Outcome::lines(vec![line("volume", published_volume(inst))]),
        // `load <name>` bascule de source. Elle n'add pas à la file (MPD y
        // *concatène* une liste enregistrée) : ici la file d'attente **est**
        // la liste de la source active, donc la load, c'est la choisir.
        // Le refus n'est plus fixe : le sources_catalog dit quels names existent.
        "load" => load(inst, index, remainder),
        // **Toucher une piste dans une liste enregistrée doit la jouer.**
        // C'était `ACK 5` : le propriétaire l'a signalé, et c'est le geste le
        // plus ordinaire qui soit une fois qu'un client sait lister les
        // sources. Un client qui « plays » une entrée l'add d'abord à la
        // file (`add`/`addid`), souvent après l'avoir vidée (`clear`).
        //
        // Ce n'est **pas** un retour sur le refus de l'édition de file, qui
        // remainder entier : réordonner, supprimer, insérer à une position n'a
        // aucun sens ici, la file *est* la liste de la source active et elle ne
        // nous appartient pas. Ce que ces trois-là font, c'est traduire « plays
        // cette entrée-ci » dans le seul vocabulaire que l'appareil ait :
        // choisir la source, puis la présélection. L'URI le permet parce que
        // c'est **nous** qui l'avons publiée (`currentsong`, `listplaylistinfo`,
        // `lsinfo`) et qu'elle nomme les deux.
        "add" | "addid" => add(inst, index, cmd, remainder),
        // Accepté sans rien faire, et il faut dire pourquoi : il n'y a pas de
        // file à vider — la file est la liste de la source. Un `ACK` ici
        // interromprait la liste de commands `clear`/`add`/`play` que le
        // client envoie pour jouer une piste, donc le refus coûterait
        // exactement la fonction qu'on vient d'add. Le client relira
        // `status` et y trouvera la file inchangée : une surprise bénigne,
        // contre un geste qui marche.
        "clear" => Outcome::ok(),
        // **Le navigateur de fichiers d'un client, rendition utile plutôt que
        // refusé.** `lsinfo` était dans la liste des refus assumés, au motif
        // qu'il n'y a pas de base de données à parcourir. C'est vrai des
        // fichiers, et faux de ce que l'appareil contains réellement : ses
        // sources, et les présélections de chacune. La racine rend donc les
        // mêmes listes enregistrées que `listplaylists` — ce que fait le vrai
        // MPD, qui les publie à la racine de son répertoire de musique — et un
        // name de source rend ses entrées. Un client y navigue alors comme dans
        // une bibliothèque, celle que l'appareil a.
        "lsinfo" => lsinfo(inst, index, remainder),
        // Les interrogations de **base de données**, bien formées et vides.
        //
        // Vides parce qu'il n'y en a pas : rien n'indexe d'étiquettes ici, et
        // les inventer serait mentir. Bien formées parce que le refus, lui,
        // était un défaut visible — un client dont l'onglet « Albums » reçoit
        // `ACK 5` affiche une erreur, là où une liste clear affiche un onglet
        // clear. C'est exactement la distinction que la doc de `COMMANDS`
        // énonce ; elle supposait un client qui grise ce qu'il ne trouve pas
        // dans `commands`, et M.A.L.P. ne le fait pas.
        //
        // `count` est du même lot mais rend deux champs plutôt que rien : les
        // clients les lisent sans les tester.
        "list" | "listall" | "listallinfo" | "listfiles" => Outcome::ok(),
        "find" | "search" => search(index, cmd, remainder),
        "count" => Outcome::lines(vec![line("songs", 0), line("playtime", 0)]),
        // Tout le remainder est refusé du même refus, sans distinguer l'inconnu du
        // volontairement non géré — MPD ne les distingue pas non plus, et
        // `commands` dit déjà ce qui existe. Deux de ces refus méritent leur
        // raison écrite : `update` n'a aucun sens (il n'y a pas de base de
        // données à indexer), et `kill` est refusée et non ignorée, parce
        // qu'éteindre l'appareil depuis le réseau sans authentification serait
        // une capacité qu'aucune télécommande de la pièce n'a.
        _ => Outcome::Reject(ack(Ack::Unknown, index, cmd, "unsupported")),
    }
}

/// Le mot que MPD attend pour `state`.
fn mpd_state(playback: Playback) -> &'static str {
    match playback {
        Playback::Playing => "play",
        Playback::Paused => "pause",
        Playback::Stopped => "stop",
    }
}

/// Des seconds au format décimal de MPD (`12.000`).
fn seconds(s: u32) -> String {
    format!("{:.3}", f64::from(s))
}

/// Où en est la playback dans la file d'attente : la **position dense** et
/// l'**index creux**, ou rien.
///
/// Rien du tout si la présélection courante n'est pas dans la file : mieux
/// vaut un `status` muet sur ce point qu'un `song` désignant une position que
/// le client ne trouvera pas dans le `playlistinfo` qu'il vient de read.
/// Un seul endroit pour les deux réponses qui en ont besoin (`status` et
/// `currentsong`), sinon elles finiraient par se contredire.
fn current(inst: &Snapshot, file: &[Entry]) -> Option<(usize, u8)> {
    let preset = inst.state.preset?;
    let position = file.iter().position(|e| e.index == preset)?;
    Some((position, preset))
}

/// L'URI d'une entrée. Un schéma à nous : le greffon ne sert aucun octet, et
/// un client n'a besoin que d'une clé stable pour distinguer deux entrées.
pub fn uri(source: &str, index: u8) -> String {
    format!("ritornello://{source}/{index}")
}

/// Taille d'une chunk de réponse binaire, en bytes.
///
/// **8 Kio, la valeur par défaut de MPD lui-même** (`binarylimit`), et le
/// chiffre n'est pas repris par imitation : c'est le cap qu'un client qui
/// n'envoie pas de `binarylimit` — donc tous ceux que ce greffon peut serve,
/// puisqu'il ne gère pas cette commande — s'attend à ne jamais voir dépassé.
/// Servir 64 Kio à un client dimensionné pour 8 serait un dépassement de
/// buffer chez lui, provoqué par nous.
///
/// **Confronté à `MAX_RESPONSE` (1 Mio), le cap du path texte.** Les deux
/// bornent la même chose — les bytes qu'une requête fait écrire — mais elles
/// n'ont ni la même valeur ni le même rôle, et l'écart de 128 est délibéré :
///
/// * `MAX_RESPONSE` doit être large parce qu'il bounded une réponse **composée**,
///   dont la size est décidée par ce que le client a demandé (une liste de
///   soixante `playlistinfo`) et non par nous. C'est un cap de dernier
///   recours, atteint par accumulation.
/// * `MAX_CHUNK` bounded une réponse dont **nous** choisissons la size : le
///   client ne demande pas « toute l'image », il demande « à partir d'ici », et
///   c'est le serveur qui décide combien il en donne. Rien n'oblige donc à
///   laisser une seule requête écrire un mébioctet, et une image de 2 Mio — un
///   dixième du cap, voir juste en dessous — se sert en 256 allers-retours
///   dont chacun coûte 8 Kio de buffer transitoire au lieu d'un seul
///   aller-retour qui en coûterait 2048.
///
/// **Le compte d'allers-retours, et pourquoi 8 Kio remainder le bon choix malgré
/// lui.** `COVER_MAX_BYTES` vaut 20 Mio, donc le cap de cover se sert en
/// ~2560 allers-retours, chacun payant un aller-retour réseau complet (le
/// client ne peut pas les grouper : l'offset de chaque requête dépend du `size:`
/// que la précédente a rendition, et une liste de commands est envoyée entière
/// avant d'être lue). Sur un Wi-Fi domestique à 20 ms d'aller-retour, cela fait
/// une minute pour une image. Le chiffre est vrai et il est mauvais ; il ne
/// justifie pourtant pas de lever ce cap :
///
/// 1. **Il décrit le cap, pas le trafic.** Une cover réelle pèse 75 Kio
///    (mesure du Cover Art Archive) à quelques centaines de kibioctets pour une
///    étiquette embarquée : 10 à 50 allers-retours, une fraction de seconde.
///    Les 20 Mio sont la bounded de refus du protocol d'affichage, pas une
///    size que le cœur produit.
/// 2. **8 Kio n'est pas un choix, c'est le contrat.** C'est la valeur par
///    défaut de `binarylimit` chez MPD, donc ce qu'un client qui ne l'a pas
///    relevée s'attend à ne jamais voir dépassé — et ce greffon ne gère pas
///    `binarylimit`, donc **aucun** de ses clients ne peut l'avoir relevée.
///    Servir 64 Kio à un client dimensionné pour 8 est un dépassement de
///    buffer chez lui, provoqué par nous, en échange de quelques dizaines de
///    millisecondes.
/// 3. **Le levier existe et il est du bon côté** : implémenter `binarylimit`
///    laisserait le client demander des tranches plus grandes, ce qui est
///    exactement la façon dont MPD résout ce compromis. C'est un ajout de
///    fonction, pas une correction ; ce qu'il ne faut pas faire, c'est relever
///    `MAX_CHUNK` unilatéralement.
///
/// Conséquence, et c'est le point : le path binaire **ne passe pas** par
/// l'accumulateur de texte et n'a donc aucun facteur d'amplification à lui.
/// Le pire cas *transitoire* d'une connection qui ne fait que des `albumart` est
/// `MAX_CHUNK` + l'en-tête ≈ 8,3 Kio de buffer, contre les ≈ 2,3 Mio que le
/// path texte autorise (voir `MAX_RESPONSE`) — soit trois millièmes.
///
/// **Ce qui n'est pas borné par connection, et il faut l'écrire en produit** —
/// c'est la troisième fois qu'une bounded de ce fichier est documentée trop
/// favorablement, donc voici le chiffre et non une nuance. L'image vit une
/// seule fois dans le processus **par génération**, pas une seule fois tout
/// court : `execute` tient son clone d'`Snapshot` et la réponse binaire tient
/// son propre clone de l'`Arc`, tous deux pendant le `write_all`. Un client qui
/// demande une chunk puis cesse de read épingle donc sa génération pour aussi
/// longtemps qu'il le veut, et une cover poussée entre-temps en crée une
/// autre qu'une deuxième session peut épingler à son tour. Le pire cas est
/// `MAX_SESSIONS × COVER_MAX_BYTES` = 16 × 20 Mio = **320 Mio**, plus la
/// génération que l'état tient lui-même, soit **340 Mio** sur un appareil d'un
/// gibioctet partagé avec mpv.
///
/// Il demande une immobilisation délibérée *et* des pochettes proches du
/// cap : ce n'est pas un accident, c'est un client hostile — mais le modèle
/// de menace de ce port (ouvert à tout le réseau local, sans mot de passe)
/// accepte déjà cette figure, et c'est pour elle que `MAX_SESSIONS` et
/// `MAX_RESPONSE` existent.
///
/// **Aucune mitigation n'est ajoutée ici, et c'est un choix argumenté.** Les
/// deux leviers réels sont hors de portée ou pires que le mal : abaisser
/// `COVER_MAX_BYTES` vit dans `ritornello-proto` et concerne tout l'appareil ;
/// mettre une échéance sur le `write_all` binaire introduirait la première
/// horloge du path de session, pour ne protéger que d'un client qui a déjà
/// choisi de nuire. Sérialiser les réponses binaires derrière un sémaphore
/// serait franchement nuisible : un seul client immobile priverait alors tous
/// les autres de cover. La bounded est donc **connue et écrite**, ce qui est ce
/// qui manquait.
pub const MAX_CHUNK: usize = 8 * 1024;

/// `albumart <uri> <offset>` et `readpicture <uri> <offset>` : une chunk de
/// la cover de ce qui plays.
///
/// **L'URI est vérifiée strictement contre ce qui plays à cet instant**, et
/// c'est la décision de conception de ce bras. Ours `currentsong` publie
/// `file: ritornello://<source>/<index>`, donc `albumart ritornello://radio/17`
/// veut dire « la cover de ce que la présélection 17 plays *maintenant* » —
/// une URI dont le contenu change sous elle, ce qui n'arrive jamais dans un MPD
/// ordinaire où une URI est un fichier. Deux réponses étaient défendables :
///
/// * **Servir quand même** (ignorer l'URI). Le client obtient toujours une
///   image, mais **la mauvaise** dès que sa demande est en retard d'une piste,
///   et le dégât est durable : les clients mettent la cover en cache **sous
///   l'URI demandée** (M.A.L.P. le fait), donc répondre l'image de la station
///   17 à une demande pour la station 3 empoisonne ce cache — la station 3
///   montrera une image fausse tant que le client n'est pas relancé, et rien
///   ne viendra jamais l'invalider.
/// * **Reject** (retenu). Le refus est transitoire et se répare tout seul :
///   le client redemande au réveil de `player` suivant, qu'un changement de
///   cover provoque justement (voir `apply_cover`). Et la rigueur ne
///   coûte rien de légitime — un client demande l'image de ce qu'il vient de
///   read dans `currentsong`, c'est-à-dire l'URI courante.
///
/// La même exigence porte sur le `href` : la cover tenue doit être celle que
/// la trame d'état courante announcement. Sans ce second contrôle, la fenêtre entre
/// l'état (envoyé d'abord) et la cover (envoyée ensuite) ferait serve
/// l'image de la piste précédente **sous l'URI de la nouvelle** — le cas
/// empoisonnant décrit ci-dessus, atteint sans qu'aucun client n'ait rien fait
/// de travers.
/// Cette commande demande-t-elle une image que l'appareil **a annoncée** mais
/// que le greffon ne tient pas encore ?
///
/// **La fenêtre qu'elle nomme est celle qui faisait disparaître les pochettes.**
/// Le cœur envoie l'état d'abord, les bytes ensuite (voir `display_relay`) :
/// à chaque changement de piste il existe donc un instant — le temps de read un
/// `folder.jpg` sur un partage, ou de le télécharger — où la trame announcement déjà
/// le `cover_href` suivant alors que la cover tenue est encore la
/// précédente. Or c'est exactement l'instant où le client se réveille et
/// demande l'image, puisque c'est cette même trame qui l'a réveillé.
///
/// Le bras `albumart` répondait alors « No file exists ». Le raisonnement
/// d'origine — le client redemandera au réveil suivant — vaut pour un client
/// idéal ; M.A.L.P., lui, **mémorise l'absence** par piste pour ne pas
/// marteler le serveur, et ne redemandait donc jamais. La cover restait
/// clear jusqu'au track suivant, où le même défaut recommençait.
///
/// La réponse est d'wait, brièvement, plutôt que de refuser : la session
/// s'en charge (voir `wait_cover`). Cette fonction ne fait que **dire
/// s'il y a lieu d'wait**, et remainder donc pure comme le remainder du module.
///
/// Faux dès que le refus est définitif — rien ne plays, aucune image annoncée,
/// URI d'une autre piste, arguments mal formés : wait ne changerait rien à
/// aucun d'eux, et faire patienter un client trois seconds pour un refus
/// certain serait pire que le refus.
pub fn cover_announced_but_missing(inst: &Snapshot, args: &[String]) -> bool {
    let Some(name) = args.first() else { return false };
    if name != "albumart" && name != "readpicture" {
        return false;
    }
    // Même forme que celle qu'exige `cover` : deux arguments, un offset
    // numérique. Une commande mal formée sera refusée, il n'y a rien à wait.
    let [_, demandee, offset] = args else { return false };
    if offset.parse::<usize>().is_err() {
        return false;
    }
    let Some(annoncee) = inst.state.track.cover_href.as_deref() else { return false };
    let Some(preset) = inst.state.preset else { return false };
    if *demandee != uri(&inst.state.source, preset) {
        return false;
    }
    // La seule situation qui se répare toute seule : l'image annoncée n'est pas
    // (encore) celle qu'on tient.
    inst.cover.as_ref().map(|p| p.href.as_str()) != Some(annoncee)
}

fn cover(
    inst: &Snapshot,
    index: usize,
    name: &str,
    remainder: &[String],
    limit: usize,
) -> Outcome {
    let [demandee, offset] = remainder else {
        return Outcome::Reject(ack(Ack::Arg, index, name, "wrong number of arguments"));
    };
    let Ok(offset) = offset.parse::<usize>() else {
        return Outcome::Reject(ack(Ack::Arg, index, name, "integer expected"));
    };
    // Le refus « il n'y a pas d'image ici », commun aux quatre gardes qui
    // suivent : le client n'a pas à savoir *laquelle* a échoué, et le
    // distinguer lui apprendrait l'état interne du greffon sans lui donner
    // aucune conduite différente à tenir — dans les quatre cas il n'y a pas
    // d'image à cette URI, et dans les quatre cas il redemandera au réveil
    // suivant.
    let absente = || Outcome::Reject(ack(Ack::NoExist, index, name, "No file exists"));
    let Some(cover) = inst.cover.as_ref() else {
        return absente();
    };
    // Rien ne plays de numéroté : aucune URI ne peut désigner quoi que ce soit,
    // et `currentsong` n'en publie d'ailleurs aucune.
    let Some(preset) = inst.state.preset else {
        return absente();
    };
    if *demandee != uri(&inst.state.source, preset) {
        return absente();
    }
    if Some(cover.href.as_str()) != inst.state.track.cover_href.as_deref() {
        return absente();
    }
    let size = cover.bytes.len();
    // `>` et non `>=`, exactement comme MPD : à `offset == size` le client a
    // déjà tout, et la réponse bien formée est une chunk clear — la refuser
    // ferait échouer un client qui ferme sa boucle par une requête de trop.
    // Au-delà, l'offset est faux et c'est un défaut d'argument.
    if offset > size {
        return Outcome::Reject(ack(Ack::Arg, index, name, "Offset too large"));
    }
    // La chunk que **ce client** accepte (voir `binarylimit`), jamais plus
    // que le cap du greffon. `MAX_CHUNK` remainder la valeur par défaut, celle
    // que reçoit un client qui n'a rien demandé.
    let fin = size.min(offset + limit.min(MAX_CHUNK_CAP));
    // `size:` est la size de **l'image entière** et non de la chunk : c'est
    // elle qui dit au client combien d'allers-retours il lui remainder. Les
    // confondre ferait s'arrêter le client à la première chunk.
    let mut header = vec![line("size", size)];
    if name == "readpicture" {
        // Le seul écart entre les deux commands, et c'est celui de MPD :
        // `readpicture` announcement le type MIME, `albumart` non.
        header.push(line("type", &cover.mime));
    }
    Outcome::Bytes(Binary { header, image: cover.bytes.clone(), chunk: offset..fin })
}

/// Le volume tel que le protocol MPD l'exprime.
///
/// `muted` écrase le volume mémorisé. MPD n'a pas de sourdine : les clients
/// coupent le son en posant `setvol 0` et s'attendent donc à relire 0 quand
/// c'est coupé. Rapporter 65 sur un appareil muet ferait afficher un curseur à
/// 65 sur un silence.
///
/// Un seul endroit pour `status` et `getvol` : deux volumes qui se
/// contrediraient seraient un défaut invisible jusqu'au jour où un client read
/// les deux.
fn published_volume(inst: &Snapshot) -> u8 {
    if inst.state.muted {
        0
    } else {
        inst.state.volume
    }
}

fn status(inst: &Snapshot) -> Vec<String> {
    let file = queue(inst);
    let mut lines = vec![line("volume", published_volume(inst))];
    // Rapportées à zéro et **pas omises** : les clients les lisent toujours, et
    // leur absence les fait mal se comporter. Les *écrire* est refusé (Task 7),
    // donc c'est le seul endroit où le greffon publie une valeur qu'il ne sait
    // pas changer — voir la spec, § Ce que le greffon ne fait pas.
    for key in ["repeat", "random", "single", "consume"] {
        lines.push(line(key, 0));
    }
    lines.push(line("playlist", inst.queue_version));
    // La **longueur de la file**, pas le maximum des indices : c'est le nombre
    // d'entrées qu'un client va demander. Les deux coïncident sur une file
    // synthétisée, et **divergent** dès qu'une source énumère une liste creuse
    // — trois stations numérotées 1, 5 et 99 font `playlistlength: 3`, jamais
    // 99. Publier le maximum ferait demander à un client quatre-vingt-seize
    // entrées qui n'existent pas.
    lines.push(line("playlistlength", file.len()));
    // Aucun fondu enchaîné ici, mais le champ est lu par des clients qui
    // affichent un réglage. Trois décimales comme `elapsed` et `duration`.
    lines.push(line("mixrampdb", "0.000"));
    // L'état **optimiste**, jamais le brut de la trame : un client qui envoie
    // `pause` puis `status` dans la même foulée lirait sinon l'état d'avant sa
    // propre commande, et son bouton n'aurait pas bougé.
    lines.push(line("state", mpd_state(inst.playback())));
    if inst.playback() != Playback::Stopped {
        // `song`/`songid` **absents** et non à zéro : `songid: 0` désignerait
        // une entrée réelle, donc un client afficherait la mauvaise line en
        // surbrillance.
        if let Some((position, index)) = current(inst, &file) {
            lines.push(line("song", position));
            lines.push(line("songid", index));
        }
    }
    if let Some(position_s) = inst.state.position_s {
        // `time` est déprécié mais encore lu ; il n'apparaît que si la position
        // est connue, et un total inconnu (un direct) s'y écrit 0 — c'est ce
        // que MPD fait des stream.
        let total = inst.state.track.duration_s.unwrap_or(0);
        lines.push(line("time", format!("{position_s}:{total}")));
        lines.push(line("elapsed", seconds(position_s)));
    }
    // Indépendante de la position : Radio France announcement la durée d'un track
    // sur un direct dont personne ne connaît l'avancement.
    if let Some(duration) = inst.state.track.duration_s {
        lines.push(line("duration", seconds(duration)));
    }
    lines
}

fn currentsong(inst: &Snapshot) -> Vec<String> {
    // Rien du tout — donc un `OK` sec — quand aucune présélection n'est
    // désignée. Gardé sur `preset` et non sur l'état de playback : une playback
    // en pause a toujours un track current, et MPD le publie.
    let Some(preset) = inst.state.preset else {
        return Vec::new();
    };
    let file = queue(inst);
    let mut lines = vec![line("file", uri(&inst.state.source, preset))];
    let track = &inst.state.track;
    // Un champ absent de `Track` ne produit **pas** de line : `Artist: `
    // vaut pire qu'aucune line, un client l'affiche comme un artiste clear.
    // Le titre seul a un repli, le name de la présélection — c'est le name de la
    // station, la seule chose qu'on sache d'un stream sans étiquette ICY.
    if let Some(titre) = track.title.as_deref().or(inst.state.preset_name.as_deref()) {
        lines.push(line("Title", titre));
    }
    if let Some(artiste) = &track.artist {
        lines.push(line("Artist", artiste));
    }
    if let Some(album) = &track.album {
        lines.push(line("Album", album));
    }
    // `Date` est le name du tag dans le protocol MPD, et il y est libre :
    // beaucoup de bibliothèques y mettent une année seule. On y met donc
    // l'année telle quelle, sans la maquiller en date complète qu'on n'a pas.
    if let Some(annee) = track.year {
        lines.push(line("Date", annee));
    }
    if let Some(duration) = track.duration_s {
        // `Time` en entier (déprécié), `duration` en décimal : les deux, parce
        // que les clients se partagent entre les deux selon leur âge.
        lines.push(line("Time", duration));
        lines.push(line("duration", seconds(duration)));
    }
    if let Some((position, index)) = current(inst, &file) {
        lines.push(line("Pos", position));
        lines.push(line("Id", index));
    }
    lines
}

/// Les lines d'une entrée de la file : sa position dense, son index creux.
fn entry_lines(source: &str, position: usize, entree: &Entry) -> Vec<String> {
    vec![
        line("file", uri(source, entree.index)),
        line("Title", &entree.name),
        line("Pos", position),
        line("Id", entree.index),
    ]
}

/// Les lines d'une chunk de la file. `Pos` remainder la position **absolue**
/// dans la file et non le rang dans la chunk : c'est la clé avec laquelle le
/// client désignera l'entrée ensuite, et la décaler ferait jouer autre chose que
/// ce qu'il a touché à l'écran.
fn queue_lines(inst: &Snapshot, file: &[Entry], plage: Range<usize>) -> Vec<String> {
    let debut = plage.start;
    file[plage]
        .iter()
        .enumerate()
        .flat_map(|(decalage, entree)| entry_lines(&inst.state.source, debut + decalage, entree))
        .collect()
}

/// Analyse un argument de position MPD : soit une position seule (`3`), soit une
/// plage `START:END` dont la **fin est exclue**, `START:` valant « jusqu'au
/// bout ». Rend les bounds déjà ramenées à la file, ou `None` si l'argument est
/// malformé.
///
/// La grammaire de MPD est `playlistinfo [[SONGPOS] | [START:END]]`, et un
/// client qui fenêtre sa file (M.A.L.P. le fait) demande `0:100`. Reject une
/// requête bien formée lui ferait afficher une file clear sur les 51 stations de
/// la radio : la plage s'implémente, elle ne se déclare pas non gérée.
///
/// **Trois hors-bounds qui ne se répondent pas pareil**, et l'asymétrie est
/// celle de MPD :
/// - une **plage** qui commence après la fin rend une chunk **clear**. Un
///   client qui fenêtre peut demander `50:100` juste après que la file a
///   rétréci ; sa requête est bien formée, la réponse est « il n'y a rien
///   là-bas », pas une erreur.
/// - une **position seule** hors bounds remainder un refus : elle désigne une entrée
///   précise qui n'existe pas, et un `OK` sec laisserait croire à un trou dans
///   la file.
/// - `START > END` est **malformé** : aucun client correct ne le produit, MPD le
///   refuse aussi, et l'accepter masquerait le bogue de l'appelant.
fn bounds(arg: &str, longueur: usize) -> Option<Range<usize>> {
    if let Some((debut, fin)) = arg.split_once(':') {
        let debut: usize = debut.parse().ok()?;
        let fin = if fin.is_empty() { longueur } else { fin.parse::<usize>().ok()? };
        if fin < debut {
            return None;
        }
        Some(debut.min(longueur)..fin.min(longueur))
    } else {
        let position: usize = arg.parse().ok()?;
        if position >= longueur {
            return None;
        }
        Some(position..position + 1)
    }
}

fn playlistinfo(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let file = queue(inst);
    let Some(arg) = args.first() else {
        return Outcome::lines(queue_lines(inst, &file, 0..file.len()));
    };
    match bounds(arg, file.len()) {
        Some(plage) => Outcome::lines(queue_lines(inst, &file, plage)),
        None => Outcome::Reject(ack(Ack::Arg, index, "playlistinfo", "bad song index")),
    }
}

fn plchanges(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let Some(version) = args.first().and_then(|a| a.parse::<u32>().ok()) else {
        return Outcome::Reject(ack(Ack::Arg, index, "plchanges", "integer expected"));
    };
    if version == inst.queue_version {
        // Rien à dire, et c'est tout l'intérêt de la commande : un client qui
        // détient la version courante n'a pas à recevoir 51 lines. Avant
        // d'analyser la plage, donc : il n'y a rien à fenêtrer dans une réponse
        // clear.
        return Outcome::ok();
    }
    // La file entière, faute de savoir ce qui a changé dedans : la file *est* la
    // liste des présélections de la source active, et un changement de source la
    // remplace en totalité. La grammaire est `plchanges VERSION [START:END]` :
    // la même fenêtre que `playlistinfo`.
    let file = queue(inst);
    let plage = match args.get(1) {
        None => 0..file.len(),
        Some(arg) => match bounds(arg, file.len()) {
            Some(plage) => plage,
            None => return Outcome::Reject(ack(Ack::Arg, index, "plchanges", "bad song index")),
        },
    };
    Outcome::lines(queue_lines(inst, &file, plage))
}

/// `listplaylists` : une entrée par source du sources_catalog, **dans l'order reçu**
/// — celui de la bascule de `SourceCycle`, donc celui que l'utilisateur voit
/// sur sa télécommande. Ne pas trier : l'order porte une information.
///
/// Rien du tout avant la première trame de sources_catalog, et c'est la vérité de cet
/// instant : le greffon ne connaît alors aucune source. Un client relira après
/// son réveil sur `stored_playlist`.
fn listplaylists(inst: &Snapshot) -> Vec<String> {
    inst.sources_catalog
        .sources
        .iter()
        .flat_map(|s| [line("playlist", &s.name), line("Last-Modified", UNKNOWN_DATE)])
        .collect()
}

/// Les lines d'une entrée de liste **enregistrée** : son URI et son name, et
/// rien de plus.
///
/// **Pas de `Pos` ni d'`Id` ici**, contrairement à `entry_lines` : ces deux
/// étiquettes désignent une entrée de la *file d'attente*, et une liste
/// enregistrée n'est pas chargée. Les émettre pour une source qui ne plays pas
/// donnerait à un client des positions qu'il ne retrouverait pas dans son
/// `playlistinfo` — c'est aussi ce que fait MPD, qui ne les publie que pour la
/// file.
fn playlist_lines(source: &str, entree: &Entry) -> Vec<String> {
    vec![line("file", uri(source, entree.index)), line("Title", &entree.name)]
}

/// Le name d'une liste enregistrée tel qu'un client l'a écrit, résolu en source
/// du sources_catalog. `Err` est l'`ACK 50` déjà mis en forme.
///
/// Un seul endroit pour `listplaylistinfo` et `load` : les deux doivent
/// répondre au *même* jeu de names que `listplaylists` announcement, et les laisser
/// chercher chacune de son côté les ferait diverger un jour.
fn named_playlist<'a>(
    inst: &'a Snapshot,
    index: usize,
    cmd: &str,
    args: &[String],
) -> Result<&'a SourceCatalog, String> {
    let Some(name) = args.first() else {
        return Err(ack(Ack::Arg, index, cmd, "wrong number of arguments"));
    };
    inst.source_catalog(name).ok_or_else(|| {
        // `ACK 50` et non `ACK 2` : le name est bien formé, c'est la liste qui
        // n'existe pas — la distinction est celle que MPD fait, et un client
        // qui la read sait qu'il doit relire `listplaylists` plutôt que de
        // corriger sa syntaxe.
        ack(Ack::NoExist, index, cmd, "no such playlist")
    })
}

/// Les entrées d'une source nommée, telles que `listplaylistinfo` et `lsinfo`
/// les rendent toutes deux.
///
/// **La même règle que `queue` là où elle peut s'appliquer, et il faut
/// qu'elle soit la même** : une source qui ne sait pas énumérer (le cd) porte
/// une liste clear, et ses entrées se synthétisent depuis le compte. Mais
/// `preset_count` ne décrit que la source **active** — pour une autre, le
/// greffon ne sait rien du nombre, et une liste clear est alors la réponse
/// honnête. Le sources_catalog ne porte pas de compte, il n'y a pas de meilleure
/// réponse.
fn source_entries(inst: &Snapshot, source: &SourceCatalog) -> Vec<Entry> {
    if source.presets.is_empty() && source.name == inst.state.source {
        queue(inst)
    } else {
        named_entries(&source.presets)
    }
}

/// `lsinfo [URI]` : la racine, ou le contenu d'une source.
///
/// Sans argument (ou sur `/`, que des clients envoient pour la racine), rend
/// les listes enregistrées — c'est-à-dire les sources —, exactement comme
/// `listplaylists`. Avec un name de source, ses entrées.
///
/// **Aucune line `directory:`**, et c'est délibéré : elle ferait wait à un
/// client une arborescence à descendre, alors que l'appareil n'en a pas. Les
/// sources sont des listes, pas des dossiers, et `playlist:` est le mot juste —
/// le même que celui sous lequel `load` les accepte.
fn lsinfo(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let cible = args.first().map(String::as_str).unwrap_or("");
    if cible.is_empty() || cible == "/" {
        return Outcome::lines(listplaylists(inst));
    }
    match inst.source_catalog(cible) {
        Some(source) => Outcome::lines(
            source_entries(inst, source)
                .iter()
                .flat_map(|e| playlist_lines(&source.name, e))
                .collect(),
        ),
        // `ACK 50` comme `listplaylistinfo` : le name est bien formé, c'est ce
        // qu'il désigne qui n'existe pas.
        None => Outcome::Reject(ack(Ack::NoExist, index, "lsinfo", "No such directory")),
    }
}

/// L'inverse d'[`uri`] : la source et l'index qu'une de nos URI désigne.
///
/// `None` pour tout ce qui n'est pas de nous — un path de fichier, une URL
/// http, une URI tronquée. Coupée au **dernier** `/` : un name de source vient
/// de `plugins.toml` et rien ne lui interdit d'en contenir un, alors que
/// l'index, lui, n'en contains jamais.
fn from_uri(uri: &str) -> Option<(&str, u8)> {
    let remainder = uri.strip_prefix("ritornello://")?;
    let (source, index) = remainder.rsplit_once('/')?;
    if source.is_empty() {
        return None;
    }
    Some((source, index.parse().ok()?))
}

/// `add <URI>` / `addid <URI> [POS]` : jouer l'entrée que cette URI désigne.
///
/// **La position d'`addid` est ignorée**, et c'est cohérent avec tout le
/// remainder : il n'y a pas de file à insérer dedans, seulement une source à
/// choisir et une présélection à lancer. La refuser ferait échouer un client
/// qui la fournit sans y tenir.
///
/// Deux commands émises quand la source visée n'est pas l'active, une seule
/// sinon. L'order compte et il est garanti : la session les push_cover dans l'order
/// sur le canal d'entrée, que le cœur dépile en série.
fn add(inst: &Snapshot, index: usize, cmd: &str, args: &[String]) -> Outcome {
    let Some(uri) = args.first() else {
        return Outcome::Reject(ack(Ack::Arg, index, cmd, "wrong number of arguments"));
    };
    // `ACK 50` : l'URI est bien formée en tant que chaîne, c'est ce qu'elle
    // désigne qui n'existe pas — la distinction que MPD fait, et celle qui dit
    // au client de relire plutôt que de corriger sa syntaxe.
    let absente = || Outcome::Reject(ack(Ack::NoExist, index, cmd, "No such song"));
    let Some((source, index)) = from_uri(uri) else { return absente() };
    let Some(sources_catalog) = inst.source_catalog(source) else { return absente() };
    // Vérifié dans la liste de **cette** source et pas seulement contre une
    // bounded : une table creuse a des trous, et un index qui tombe dedans ne
    // plays rien. Même règle que `playid`.
    let entries = source_entries(inst, sources_catalog);
    if !index_exists(&entries, index) {
        return absente();
    }
    let mut cmds = Vec::new();
    if source != inst.state.source {
        // Le name du **sources_catalog** et non l'argument brut, comme `load` : les
        // deux sont égaux par construction, et émettre celui que le cœur nous a
        // donné garde le greffon incapable d'inventer un name de source.
        cmds.push(Command::SelectSource(sources_catalog.name.clone()));
    }
    cmds.push(Command::Select(index));
    let mut lines = Vec::new();
    if cmd == "addid" {
        // Le seul écart entre les deux commands, et c'est celui de MPD :
        // `addid` rend l'identifiant de ce qu'il vient d'add.
        lines.push(line("Id", index));
    }
    Outcome::Reply { lines, cmds }
}

/// `find`/`search` : bien formées, et vides.
///
/// Le refus des arguments manquants est conservé — c'est celui de MPD, et un
/// client qui envoie une requête tronquée doit l'apprendre plutôt que de croire
/// que sa search n'a rien donné.
fn search(index: usize, cmd: &str, args: &[String]) -> Outcome {
    if args.is_empty() {
        return Outcome::Reject(ack(Ack::Arg, index, cmd, "too few arguments"));
    }
    Outcome::ok()
}

/// Plafond d'une chunk binaire qu'un client peut demander, en bytes.
///
/// **64 Kio, et le chiffre bounded une dépense réelle.** Une chunk est un
/// buffer que la session écrit d'un coup ; le pire cas est
/// `MAX_SESSIONS × MAX_CHUNK_CAP`, soit 16 × 64 Kio = **1 Mio** sur un
/// appareil d'un gibioctet — négligeable, là où laisser un client demander
/// n'importe quoi ne le serait pas. Le gain est net dans l'autre sens : une
/// cover de 500 Kio passe de soixante-deux allers-retours à huit.
pub const MAX_CHUNK_CAP: usize = 64 * 1024;

/// Plancher d'une chunk binaire. En dessous, l'en-tête textuel coûterait plus
/// que les bytes qu'il announcement.
const MIN_CHUNK: usize = 64;

/// `binarylimit <N>` : la size de chunk que ce client accepte.
///
/// **Bornée des deux côtés, silencieusement.** MPD refuse une valeur sous son
/// propre plancher ; ici la valeur est ramenée dans `[MIN_CHUNK,
/// MAX_CHUNK_CAP]` plutôt que refusée, parce que la bounded haute est une
/// décision **à nous** (voir `MAX_CHUNK_CAP`) et non une règle du
/// protocol : refuser `binarylimit 1048576` ferait échouer la connection d'un
/// client parfaitement correct qui demande simplement plus que ce qu'on veut
/// serve. Une chunk plus petite que demandée est toujours licite — la valeur
/// est un **maximum** que le serveur ne doit pas dépasser, pas un contrat de
/// size exacte.
fn binarylimit(index: usize, args: &[String]) -> Outcome {
    let Some(n) = args.first().and_then(|a| a.parse::<usize>().ok()) else {
        return Outcome::Reject(ack(Ack::Arg, index, "binarylimit", "integer expected"));
    };
    Outcome::BinaryLimit(n.clamp(MIN_CHUNK, MAX_CHUNK_CAP))
}

fn listplaylistinfo(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let source = match named_playlist(inst, index, "listplaylistinfo", args) {
        Ok(source) => source,
        Err(refus) => return Outcome::Reject(refus),
    };
    // Partagée avec `lsinfo`, qui doit répondre exactement la même chose du
    // même name : voir `source_entries`.
    let entries = source_entries(inst, source);
    Outcome::lines(entries.iter().flat_map(|e| playlist_lines(&source.name, e)).collect())
}

/// `load <name>` : choisir la source de ce name.
///
/// Le greffon refuse lui-même un name absent du sources_catalog plutôt que d'émettre
/// un `SelectSource` que le cœur ignorerait en silence (voir la doc de
/// `Command::SelectSource`) : il ne propose que des names qu'il a reçus, donc
/// c'est à lui de savoir lesquels existent. Un `OK` suivi de rien serait la
/// pire réponse possible pour un client, qui attendrait un changement de file
/// qui n'arrive jamais.
fn load(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    match named_playlist(inst, index, "load", args) {
        // Le name du **sources_catalog** et non l'argument brut : les deux sont égaux
        // par construction (`source_catalog` compare exactement), mais
        // émettre celui que le cœur nous a donné garde le greffon incapable
        // d'inventer un name de source.
        Ok(source) => Outcome::acting(Command::SelectSource(source.name.clone())),
        Err(refus) => Outcome::Reject(refus),
    }
}

fn stats(inst: &Snapshot) -> Vec<String> {
    // `uptime` à 0 **délibérément** : le rendre juste demanderait de mémoriser
    // un instant de départ, donc une horloge de plus dans un module qui n'en a
    // aucune, pour une valeur qu'aucun client d'ici n'utilise. Même raison pour
    // les durées de playback cumulées.
    vec![
        line("artists", 0),
        line("albums", 0),
        line("songs", queue(inst).len()),
        line("uptime", 0),
        line("db_playtime", 0),
        line("db_update", 0),
        line("playtime", 0),
    ]
}

/// Ce que vaut un name de sous-système écrit dans un `idle`.
enum IdleName {
    /// Un des quatre que ce greffon sait faire bouger.
    Ours(Subsystem),
    /// Un sous-système du vocabulaire MPD que nous n'émettrons **jamais**.
    NeverEmitted,
    /// Un mot que MPD lui-même ne connaît pas.
    Unknown,
}

/// Le name MPD d'un sous-système, tel qu'un client l'écrit dans son `idle`.
///
/// **Le vocabulaire entier de MPD, pas seulement le nôtre.** C'est la
/// distinction qui décide si un client démarre : tout ce qui est bâti sur
/// `mpd_send_idle_mask` de libmpdclient envoie une liste explicite — en
/// pratique `database update stored_playlist playlist player mixer output
/// options` — et un `ACK` sur son premier `idle` le fait boucler ou renoncer.
/// Reject un mot que MPD ignore est juste ; refuser un mot **légal** est le
/// même défaut vu de l'autre côté.
///
/// Un sous-système légal que nous n'émettons jamais est donc accepté puis
/// écarté en silence, et l'attente qui en résulte peut ne jamais se terminer.
/// C'est le comportement MPD correct et non un oubli : le client a demandé
/// qu'on le prévienne si ça changeait, et ça ne change jamais.
fn idle_name(name: &str) -> IdleName {
    match name {
        "player" => IdleName::Ours(Subsystem::Player),
        "mixer" => IdleName::Ours(Subsystem::Mixer),
        "playlist" => IdleName::Ours(Subsystem::Playlist),
        "stored_playlist" => IdleName::Ours(Subsystem::StoredPlaylist),
        // Le remainder du vocabulaire de MPD. Aucun n'a de déclencheur ici : il n'y
        // a pas de base de données à indexer (`database`, `update`), une seule
        // sortie qu'on ne pilote pas (`output`), aucune option modifiable
        // (`options`), ni partition, ni étiquette collée, ni abonnement, ni
        // message, ni voisinage, ni montage annoncé sur ce protocol.
        "database" | "update" | "output" | "options" | "partition" | "sticker"
        | "subscription" | "message" | "neighbor" | "mount" => IdleName::NeverEmitted,
        _ => IdleName::Unknown,
    }
}

fn idle(index: usize, args: &[String]) -> Outcome {
    if args.is_empty() {
        // Sans argument, tous les subsystems comptent.
        return Outcome::Wait(vec![
            Subsystem::Player,
            Subsystem::Mixer,
            Subsystem::Playlist,
            Subsystem::StoredPlaylist,
        ]);
    }
    let mut subsystems = Vec::new();
    for name in args {
        match idle_name(name) {
            // Dédoublonné, comme `mark` côté état : `idle player player` ne
            // décrit qu'une seule attente.
            IdleName::Ours(s) => {
                if !subsystems.contains(&s) {
                    subsystems.push(s);
                }
            }
            // Accepté puis écarté : voir `idle_name`. La liste peut finish clear,
            // et c'est une attente qui ne se terminera jamais — la bonne
            // réponse, pas un oubli.
            IdleName::NeverEmitted => {}
            // Un mot que MPD ne connaît pas : refusé et non ignoré, sinon un
            // client qui a mal orthographié son sous-système resterait muet
            // pour toujours, ce qui se diagnostique bien plus mal qu'un `ACK`.
            IdleName::Unknown => {
                return Outcome::Reject(ack(Ack::Arg, index, "idle", "unrecognized idle event"))
            }
        }
    }
    Outcome::Wait(subsystems)
}

// ----------------------------------------------------------------------
// Les commands d'action : ce qui demande quelque chose à l'appareil.
// ----------------------------------------------------------------------

/// Traduit une position MPD (le **rang**, base 0, celui que `Pos` publie) en
/// l'index de présélection qui s'y trouve. `None` si la position dépasse la
/// file.
///
/// Extraite en fonction pure, séparée de `play`, pour se tester aussi sur une
/// file construite à la main. Elle est le seul path autorisé de la position
/// vers l'index : dès qu'une source énumère une liste creuse, « l'index moins
/// un » n'est plus le rang, et le décalage qu'une soustraction introduirait
/// ferait jouer une station voisine de celle qu'on a touchée à l'écran.
fn position_to_index(file: &[Entry], position: usize) -> Option<u8> {
    file.get(position).map(|e| e.index)
}

/// Vrai si cet index de présélection existe réellement dans la file — pas
/// seulement dans les bounds de son maximum.
///
/// La distinction est sans effet sur une file synthétisée (« exister » et
/// « être ≤ au maximum » y sont la même chose) et décisive sur une file creuse,
/// où `preset_count` remainder un maximum et non un compte : un `playid` sur un
/// trou de la suite doit refuser, là qu'une comparaison de bounded le laisserait
/// passer à tort.
fn index_exists(file: &[Entry], index: u8) -> bool {
    file.iter().any(|e| e.index == index)
}

/// Un temps MPD absolu, en seconds tronquées. `None` si non numérique, non
/// fini ou négatif — jamais un temps négatif ramené à zéro en silence pour
/// cette forme (contrairement à la résolution du relatif de `seekcur`, où zéro
/// est la bonne réponse à un recul trop grand).
///
/// **`inf` et `nan` sont non numériques pour ce protocol**, même si
/// `f64::from_str` les accepte : `seek 0 inf` rendait `SeekTo(u32::MAX)` et
/// `seek 0 nan` rendait `SeekTo(0)`, tous deux **en silence**, contre la règle
/// que ce module énonce douze lines plus haut — un argument absent ou non
/// numérique est un `Ack::Arg`, jamais un défaut muet. C'est la même classe que
/// le débordement d'`i16` de `volume`, sur le même port sans authentification,
/// à deux mètres de là.
fn absolute_time(s: &str) -> Option<u32> {
    let v: f64 = s.parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        None
    } else {
        Some(v as u32)
    }
}

fn play(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let Some(arg) = args.first() else {
        // La touche Lecture, pas une sélection : restart ce qui était chargé
        // (ou démarre, pour une source qui sait quoi faire même à l'arrêt —
        // c'est à elle de décider, pas à ce greffon).
        return Outcome::acting(Command::PlayPause);
    };
    let Ok(position) = arg.parse::<usize>() else {
        return Outcome::Reject(ack(Ack::Arg, index, "play", "need a positive integer"));
    };
    match position_to_index(&queue(inst), position) {
        Some(index) => Outcome::acting(Command::Select(index)),
        None => Outcome::Reject(ack(Ack::Arg, index, "play", "bad song index")),
    }
}

fn playid(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let Some(id) = args.first().and_then(|a| a.parse::<u8>().ok()) else {
        return Outcome::Reject(ack(Ack::Arg, index, "playid", "need a positive integer"));
    };
    if index_exists(&queue(inst), id) {
        Outcome::acting(Command::Select(id))
    } else {
        Outcome::Reject(ack(Ack::Arg, index, "playid", "no such song"))
    }
}

/// `pause [0|1]`. Sans argument, bascule ; avec, n'émet que si l'état diffère
/// de la cible — c'est ce qui ferme la course décrite dans la spec (§ `pause`
/// dans `PlayerState.playback`) : un `pause 1` renvoyé deux fois par un client
/// qui n'a pas vu la confirmation ne doit pas relancer la playback.
///
/// **À l'arrêt, n'émet jamais rien**, quel que soit l'argument : `PlayPause`
/// y démarrerait une playback dont ni la source ni ce greffon ne savent ni quoi
/// ni où (voir `SharedState::acknowledge_optimistic`), ce qu'un client n'a pas
/// demandé en appuyant sur « pause ». La validation de l'argument passe
/// **avant** cette garde : un `pause 2` malformé doit rester un `ACK` même à
/// l'arrêt, pas être avalé en silence.
fn pause(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let cible = match args.first().map(String::as_str) {
        None => None,
        Some("0") => Some(Playback::Playing),
        Some("1") => Some(Playback::Paused),
        Some(_) => return Outcome::Reject(ack(Ack::Arg, index, "pause", "boolean expected")),
    };
    if inst.playback() == Playback::Stopped {
        return Outcome::ok();
    }
    match cible {
        None => Outcome::acting(Command::PlayPause),
        Some(cible) if inst.playback() != cible => Outcome::acting(Command::PlayPause),
        Some(_) => Outcome::ok(),
    }
}

/// `setvol <0-100>` : pose le volume, et **lève la sourdine s'il est au-dessus
/// de zéro**.
///
/// Le premier point est le protocol ; le second est la seule issue qu'un client
/// MPD ait pour rallumer le son de cet appareil.
///
/// **Pourquoi il fallait le faire.** MPD n'a pas de sourdine, donc `status`
/// publie `volume: 0` dès que l'appareil est muet (voir `status`) — c'est juste,
/// les clients coupent le son en posant `setvol 0` et s'attendent à relire 0.
/// Mais *aucune* commande MPD ne pouvait lever cette sourdine : un client
/// remontait son curseur, `SetVolume(40)` partait, le volume changeait
/// réellement, et le son restait coupé. L'utilisateur n'avait plus de sortie
/// depuis son téléphone — seule la télécommande de la pièce pouvait le
/// dépanner. Un utilisateur qui remonte un curseur demande sans ambiguïté à
/// entendre quelque chose ; c'est la playback qu'on retient.
///
/// **Émise conditionnellement, parce que `Command::Mute` est une bascule** et
/// non une pose : l'émettre alors que rien n'est coupé *couperait* le son. La
/// garde sur `state.muted` est donc la même forme conditionnelle que `pause
/// 0`/`pause 1` emploie contre `playback`, et pour la même raison.
///
/// **L'order des deux commands ne change pas le résultat**, et il faut l'écrire
/// parce que ce paragraphe a d'abord prétendu le contraire : il affirmait que le
/// cœur, en levant la sourdine, reposait le volume mémorisé, donc qu'il fallait
/// poser le volume après. C'est faux, et une raison fausse est pire qu'une
/// raison absente — elle fait croire à un mécanisme de restauration dont un
/// player déduira des choses. Le bras `Command::Mute` du cœur fait
/// `muted = !muted` puis `set_mute(muted)`, et rien d'autre : le niveau et la
/// sourdine sont deux propriétés indépendantes, deux appels distincts à mpv.
/// `SetVolume(40)` puis `Mute`, ou `Mute` puis `SetVolume(40)`, laissent donc
/// tous deux un appareil non muet à 40.
///
/// L'order retenu — `SetVolume` d'abord — ne se plays que sur l'**intervalle**
/// entre les deux, qui existe bel et bien : elles traversent le canal d'entrée
/// l'une après l'autre, et chacune attend mpv.
///
/// * **Ce qu'on entend, et c'est la raison qui pèse.** Poser le niveau pendant
///   que la sortie est encore muette est inaudible, donc le son revient *déjà*
///   à 40. L'order inverse le ferait revenir au niveau mémorisé — jusqu'à 100 —
///   le temps d'un aller-retour avant de retomber. Sur un appareil dont le
///   volume mémorisé peut être bien au-dessus de ce que le client demande, c'est
///   la seule des deux différences qui se remarque.
/// * **Ce qu'on voit.** Les deux commands appellent `show_overlay`, qui read le
///   `muted` et le `volume` de l'instant. L'incrustation *finale* dit « 40 % »
///   dans les deux ordres ; seule l'intermédiaire diffère, et l'order retenu y
///   affiche « muet » — un mot encore juste à cet instant — au lieu de
///   l'ancien niveau, un nombre qui ne l'est plus.
///
/// **`setvol 0` ne coupe pas pour autant**, et c'est la règle inverse
/// inchangée : voir la spec, § « La sourdine, un cas à ne pas rater ».
/// `SetVolume(0)` pose zéro, `Mute` bascule ; les confondre ferait qu'un client
/// remontant le volume après un `setvol 0` trouverait le son toujours coupé —
/// exactement le défaut qu'on répare ici, réintroduit par l'autre bout.
fn setvol(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    match args.first().and_then(|a| a.parse::<u8>().ok()) {
        Some(v) if v <= 100 => Outcome::Reply { lines: Vec::new(), cmds: unmute(inst, v) },
        _ => Outcome::Reject(ack(Ack::Arg, index, "setvol", "invalid volume")),
    }
}

/// Les commands d'une pose de volume : `SetVolume`, plus `Mute` si l'appareil
/// est muet et que le volume demandé n'est pas zéro.
///
/// Un seul endroit pour `setvol` et `volume` : les deux sont le même geste
/// (« monte le son »), et laisser l'une démuter sans l'autre ferait dépendre le
/// retour du son de l'âge du client — `volume` est dépréciée par MPD, donc
/// c'est la vieille moitié du parc qui resterait coincée.
fn unmute(inst: &Snapshot, niveau: u8) -> Vec<Command> {
    let mut cmds = vec![Command::SetVolume(niveau)];
    if niveau > 0 && inst.state.muted {
        cmds.push(Command::Mute);
    }
    cmds
}

/// `volume <±n>` : dépréciée par MPD mais encore émise par de vieux clients.
/// Relative au volume current et **bornée ici** — `Command::SetVolume` est
/// absolu, donc c'est ce module qui doit calculer et clamper, pas le cœur.
fn volume(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    match args.first().and_then(|a| a.parse::<i16>().ok()) {
        Some(delta) => {
            // Élargi en `i32` avant l'addition : `delta` couvre tout `i16`
            // (±32767), et un volume current même faible (1) additionné à
            // `i16::MAX` déborde `i16` avant que `.clamp` n'ait pu acting — un
            // panic en debug/test (overflow checks active par défaut), une
            // valeur fausse en release. `i32` contains les deux opérandes
            // (volume ≤ 100, delta ≤ 32767) sans aucun risque de dépassement,
            // donc le clamp remainder le seul endroit qui bounded.
            let nouveau = (i32::from(inst.state.volume) + i32::from(delta)).clamp(0, 100) as u8;
            // Même levée de sourdine que `setvol`, et par le même path : voir
            // `unmute`. Le calcul part du volume **mémorisé** et non du zéro
            // que `status` publie quand c'est coupé — c'est le seul point de
            // départ qui ait un sens, et il rend `volume +5` sur un appareil
            // muet équivalent à ce que la télécommande ferait.
            Outcome::Reply { lines: Vec::new(), cmds: unmute(inst, nouveau) }
        }
        None => Outcome::Reject(ack(Ack::Arg, index, "volume", "invalid volume")),
    }
}

/// `seek <POS> <T>` / `seekid <ID> <T>` : le premier argument (position ou id)
/// est ignoré — `Command::SeekTo` ne sait pas changer de piste en même temps,
/// et MPD n'envoie ce kind de commande que sur ce qui plays déjà. `T` est
/// toujours absolu ici ; seul `seekcur` accepte le relatif (voir `seekcur`).
fn seek(index: usize, cmd: &str, args: &[String]) -> Outcome {
    match args.get(1).and_then(|a| absolute_time(a)) {
        Some(t) => Outcome::acting(Command::SeekTo(t)),
        None => Outcome::Reject(ack(Ack::Arg, index, cmd, "float expected")),
    }
}

/// `seekcur <T>` : `T` est `+n`, `-n`, ou un absolu décimal. `Command` ne
/// porte qu'un positionnement absolu, donc le relatif est résolu ici, depuis
/// `position_s`, tronqué en seconds et **jamais négatif** — un recul plus
/// grand que la position rend `0`, pas un temps négatif.
fn seekcur(inst: &Snapshot, index: usize, args: &[String]) -> Outcome {
    let refuser = |message: &str| Outcome::Reject(ack(Ack::Arg, index, "seekcur", message));
    let Some(arg) = args.first() else {
        return refuser("float expected");
    };
    let seconds = if arg.starts_with('+') || arg.starts_with('-') {
        let Ok(delta) = arg.parse::<f64>() else {
            return refuser("float expected");
        };
        // La même règle que `absolute_time`, sur l'autre forme : `+inf` et `-nan`
        // se parsent, et sans cette garde `seekcur +inf` rendait
        // `SeekTo(u32::MAX)` en silence. Le relatif tolère le négatif (un recul
        // trop grand vaut zéro), jamais le non fini — il n'y a pas de position
        // à laquelle « l'infini » se ramène.
        if !delta.is_finite() {
            return refuser("float expected");
        }
        let Some(base) = inst.state.position_s else {
            // Rien à résoudre depuis : un relatif sans point de départ connu
            // inventerait un temps, ce qu'aucun défaut silencieux ne doit
            // faire (voir la règle du brief sur les arguments hors bounds).
            return refuser("no current position");
        };
        // `.max(0.0)` est explicite plutôt qu'implicite : la conversion
        // `f64 -> u32` sature déjà à 0 sur un flottant négatif depuis Rust
        // 1.45, donc son retrait ne changerait rien à ce résultat-ci — mais
        // rien ne doit dépendre à l'œil de le savoir pour read cette line.
        (f64::from(base) + delta).max(0.0) as u32
    } else {
        match absolute_time(arg) {
            Some(t) => t,
            None => return refuser("float expected"),
        }
    };
    Outcome::acting(Command::SeekTo(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SharedState;
    use ritornello_proto::{SourcesCatalog, Track, PlayerState};

    // ------------------------------------------------------------------
    // Les instantanés de référence.
    //
    // Un constructeur par situation, tous bâtis sur `depuis` : les **Tasks 7 et
    // 13** réutilisent ces mêmes constructeurs (`instantane_en_pause`,
    // `instantane_au_volume`, `instantane_avec_presets`…), donc ils vivent en
    // un seul endroit et s'étendent par ajout, jamais par retouche des
    // existants — un test de la Task 6 qui changerait de valeur de référence
    // parce que la Task 7 a eu besoin d'un champ serait un faux échec.
    // ------------------------------------------------------------------

    /// Enveloppe une trame dans un instantané cohérent.
    ///
    /// `optimistic_playback` recopie `state.playback` : c'est l'état au repos, une
    /// fois la trame confirmante arrivée. Un test veut au contraire les voir
    /// diverger, et il le pose lui-même — c'est justement la propriété qu'il
    /// vérifie.
    ///
    /// `queue_version` vaut 7 et non 0, pour que `playlist: 7` ne puisse pas
    /// passer par accident derrière une implémentation qui publierait une
    /// constante.
    fn depuis(state: PlayerState) -> Snapshot {
        Snapshot { optimistic_playback: state.playback, state, queue_version: 7, ..Default::default() }
    }

    /// La radio à l'arrêt : trois présélections, rien ne plays.
    fn radio_arretee() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 40,
            preset_count: Some(3),
            ..Default::default()
        }
    }

    /// La radio sur sa deuxième présélection, avec un track complet.
    fn radio_qui_joue(playback: Playback) -> PlayerState {
        PlayerState {
            playback,
            preset: Some(2),
            preset_name: Some("France Inter".into()),
            position_s: Some(12),
            track: Track {
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                album: Some("Kind of Blue".into()),
                duration_s: Some(545),
                year: Some(1959),
                // Le protocol MPD n'a pas de champ de lien : le greffon n'en
                // read aucun, meme raison que `cover_href` plus bas.
                links: Vec::new(),
                origin: Some("musicbrainz".into()),
                // Le protocol MPD n'a pas de champ de cover : le greffon
                // n'en read aucun, mais le litteral doit rester complet — c'est
                // ce qui force a revoir ce test quand un champ apparait.
                cover_href: None,
                cover_origin: None,
                // Meme raison : le protocol MPD ne porte aucune provenance,
                // le greffon n'en read pas, et le litteral remainder complet pour
                // qu'un champ add force a repasser ici.
                provenance: Default::default(),
            },
            ..radio_arretee()
        }
    }

    fn instantane_arrete() -> Snapshot {
        depuis(radio_arretee())
    }

    /// Le son coupé, mais un volume mémorisé bien réel.
    fn instantane_muet(volume: u8) -> Snapshot {
        depuis(PlayerState { volume, muted: true, ..radio_arretee() })
    }

    fn instantane_en_lecture() -> Snapshot {
        depuis(radio_qui_joue(Playback::Playing))
    }

    fn instantane_en_pause() -> Snapshot {
        depuis(radio_qui_joue(Playback::Paused))
    }

    /// Une station qui plays sans la moindre étiquette ICY : elle a un name de
    /// présélection, et rien d'autre.
    fn instantane_sans_titre() -> Snapshot {
        depuis(PlayerState {
            playback: Playback::Playing,
            preset: Some(1),
            preset_name: Some("Chérie FM".into()),
            ..radio_arretee()
        })
    }

    /// Une source qui déclare un nombre de présélections sans les nommer.
    fn instantane_sans_presets(source: &str, combien: u8) -> Snapshot {
        depuis(PlayerState {
            source: source.into(),
            preset_count: Some(combien),
            ..Default::default()
        })
    }

    /// Une entrée de sources_catalog, telle que le cœur en émet une par source
    /// déclarée. Une liste de présélections clear est la vérité du cd, qui remainder
    /// au corps par défaut de `list_presets`.
    fn source_catalogue(name: &str, presets: &[(u8, &str)]) -> SourceCatalog {
        SourceCatalog {
            name: name.to_string(),
            presets: presets
                .iter()
                .map(|(index, name)| Preset { index: *index, name: (*name).to_string() })
                .collect(),
        }
    }

    /// L'instantané d'un appareil dont le cœur a publié son sources_catalog, la
    /// source `active` étant celle que la dernière trame d'état désigne.
    ///
    /// **Deux détails de réalisme, parce qu'un instantané qu'aucun producteur
    /// ne peut émettre ne prouve rien** :
    /// - la source active est **ajoutée au sources_catalog** si elle n'y figure pas,
    ///   avec une liste clear : le sources_catalog du cœur nomme *toutes* les sources
    ///   déclarées, et le cd y est présent sans savoir énumérer. Un sources_catalog
    ///   qui ignorerait la source qui plays n'existe pas.
    /// - `preset_count` vaut le **maximum** des indices de la source active, et
    ///   non leur nombre : c'est ce que `Stations::preset_count` renvoie
    ///   vraiment (`radio/src/config.rs`). Trois stations 1, 5 et 99 font donc
    ///   `preset_count: Some(99)` — la forme exacte qui piège une
    ///   implémentation confondant compte et maximum. `None` quand la source
    ///   active n'énumère rien, comme une source qui n'a rien déclaré.
    fn instantane_catalogue(active: &str, sources: &[(&str, &[(u8, &str)])]) -> Snapshot {
        let mut sources_catalog =
            SourcesCatalog { sources: sources.iter().map(|(n, p)| source_catalogue(n, p)).collect() };
        if !sources_catalog.sources.iter().any(|s| s.name == active) {
            sources_catalog.sources.push(source_catalogue(active, &[]));
        }
        let maximum = sources_catalog
            .sources
            .iter()
            .find(|s| s.name == active)
            .and_then(|s| s.presets.iter().map(|p| p.index).max());
        Snapshot {
            sources_catalog,
            ..depuis(PlayerState { source: active.into(), preset_count: maximum, ..Default::default() })
        }
    }

    /// Un sources_catalog de sources nommées sans présélections, la première étant
    /// active.
    ///
    /// C'est la forme que le sources_catalog a **au démarrage** : le cœur connaît ses
    /// sources dès le câblage et remplit leurs présélections au fur et à mesure
    /// que les réponses à `ListPresets` arrivent par le canal de mises à jour.
    fn instantane_avec_catalogue(names: &[&str]) -> Snapshot {
        let sources: Vec<(&str, &[(u8, &str)])> = names.iter().map(|n| (*n, &[][..])).collect();
        instantane_catalogue(names.first().copied().unwrap_or_default(), &sources)
    }

    /// Une source dont les présélections sont nommées, et qui plays.
    ///
    /// Les indices et les names sont **respectés tels quels**, creux compris :
    /// c'est le sources_catalog qui les porte, et `queue` les recopie sans
    /// dériver un rang d'un index.
    fn instantane_avec_presets(source: &str, presets: &[(u8, &str)]) -> Snapshot {
        instantane_catalogue(source, &[(source, presets)])
    }

    /// Une source qui plays pendant qu'une autre est au sources_catalog : le cas qui a
    /// motivé le contournement du garde côté cœur (`handle_source_update` rend
    /// la main sur une trame qui ne vient pas de la source active, or le
    /// sources_catalog décrit toutes les sources).
    fn instantane_actif_sur(active: &str, sources: &[(&str, &[(u8, &str)])]) -> Snapshot {
        instantane_catalogue(active, sources)
    }

    /// Un volume donné, sans rien d'autre autour.
    fn instantane_au_volume(volume: u8) -> Snapshot {
        depuis(PlayerState { volume, ..radio_arretee() })
    }

    /// Une position connue dans ce qui plays, sans rien d'autre autour.
    fn instantane_a_la_position(position_s: u32) -> Snapshot {
        depuis(PlayerState { position_s: Some(position_s), ..radio_arretee() })
    }

    fn traiter_mots(inst: &Snapshot, index: usize, mots: &[&str]) -> Outcome {
        let args: Vec<String> = mots.iter().map(|m| (*m).to_string()).collect();
        handle(inst, index, &args, MAX_CHUNK)
    }

    /// Les lines d'une réponse, ou une panique nommant ce qu'on a eu à la
    /// place — un `Reject` inattendu doit se read dans le message d'échec.
    fn traiter_ok(inst: &Snapshot, mots: &[&str]) -> Vec<String> {
        match traiter_mots(inst, 0, mots) {
            Outcome::Reply { lines, .. } => lines,
            autre => panic!("attendu Reply pour {mots:?}, obtenu {autre:?}"),
        }
    }

    /// Les commands émises par une réponse, ou une panique nommant ce qu'on a
    /// eu à la place — le pendant de `traiter_ok` pour les tests de la Task 7.
    fn cmds(inst: &Snapshot, mots: &[&str]) -> Vec<Command> {
        match traiter_mots(inst, 0, mots) {
            Outcome::Reply { cmds, .. } => cmds,
            autre => panic!("attendu Reply pour {mots:?}, obtenu {autre:?}"),
        }
    }

    // ------------------------------------------------------------------
    // La file d'attente
    // ------------------------------------------------------------------

    #[test]
    fn sans_liste_la_file_se_synthetise_depuis_le_compte() {
        // Le cd : trois pistes, aucun name. La suite est dense, `Pos = Id - 1`,
        // et elle commence à 1 — l'index qu'attend `Command::Select`, pas un
        // rang base 0.
        let inst = instantane_sans_presets("cd", 3);
        assert_eq!(
            queue(&inst),
            vec![
                Entry { index: 1, name: "1".into() },
                Entry { index: 2, name: "2".into() },
                Entry { index: 3, name: "3".into() },
            ]
        );
    }

    #[test]
    fn rien_de_declare_donne_une_file_vide_et_non_la_grille_historique() {
        // `preset_count: None` veut dire « la source n'a rien déclaré », ce que
        // l'IHM traduit par sa grille 1-9. Ici ce serait faux : annoncer neuf
        // entries ferait demander a un client neuf choses dont aucune n'existe.
        let inst = depuis(PlayerState { source: "aux".into(), ..Default::default() });
        assert!(queue(&inst).is_empty());
        assert!(traiter_ok(&inst, &["status"]).contains(&"playlistlength: 0".to_string()));
        assert!(traiter_ok(&inst, &["playlistinfo"]).is_empty());
    }

    #[test]
    fn une_vraie_liste_prend_le_pas_sur_la_synthese() {
        // La branche que la Task 13 met **en tete** : des que le sources_catalog
        // nomme les preselections de la source active, ce sont elles la file —
        // avec leurs indices tels quels, creux compris, et leurs vrais names.
        // Une implementation restee sur la synthese rendrait 1..=99.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(
            queue(&inst),
            vec![
                Entry { index: 1, name: "FIP".into() },
                Entry { index: 5, name: "Nova".into() },
                Entry { index: 99, name: "TSF".into() },
            ]
        );
    }

    #[test]
    fn une_source_active_qui_nenumere_pas_retombe_sur_la_synthese() {
        // Le cd est bien au sources_catalog, avec une liste **clear** : cela veut dire
        // « je n'ai que des numeros », pas « je n'ai rien ». Sans ce repli sur
        // `preset_count`, les douze pistes d'un disque insert disparaitraient
        // le jour ou le sources_catalog arrive — une regression que seule cette
        // combinaison (sources_catalog present, liste clear) peut montrer.
        let inst = Snapshot {
            sources_catalog: SourcesCatalog { sources: vec![source_catalogue("cd", &[])] },
            ..instantane_sans_presets("cd", 12)
        };
        assert_eq!(queue(&inst).len(), 12);
        assert_eq!(queue(&inst)[11], Entry { index: 12, name: "12".into() });
    }

    #[test]
    fn la_file_suit_la_source_active_et_non_la_premiere_du_catalogue() {
        // Le sources_catalog decrit toutes les sources ; la file d'attente n'est
        // faite que de celle qui plays. Prendre la premiere entree du sources_catalog
        // ferait publier les stations de la radio pendant qu'un disque tourne.
        let inst = instantane_actif_sur("cd", &[("radio", &[(1, "FIP"), (5, "Nova")]), ("cd", &[])]);
        assert!(queue(&inst).is_empty(), "le cd n'enumere pas et n'a rien declare");
    }

    #[test]
    fn les_positions_sont_denses_la_ou_les_indices_sont_creux() {
        // LE test du chantier, de bout en bout a travers `handle` : sur des
        // stations 1, 5 et 99, les positions publiees sont 0, 1, 2 — et les
        // `Id` restent 1, 5, 99. Toute derivation d'un rang par soustraction
        // (`Pos = Id - 1`) publierait ici 0, 4, 98.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(
            traiter_ok(&inst, &["playlistinfo"]),
            vec![
                "file: ritornello://radio/1",
                "Title: FIP",
                "Pos: 0",
                "Id: 1",
                "file: ritornello://radio/5",
                "Title: Nova",
                "Pos: 1",
                "Id: 5",
                "file: ritornello://radio/99",
                "Title: TSF",
                "Pos: 2",
                "Id: 99",
            ]
        );
    }

    #[test]
    fn playlistlength_est_la_longueur_de_la_liste_pas_le_maximum_des_indices() {
        // La propriete que rien ne pinçait avant qu'une file creuse existe :
        // trois stations numerotees 1, 5 et 99 font `playlistlength: 3`. Une
        // implementation qui publierait `preset_count` (99, le **maximum**)
        // ferait demander a un client quatre-vingt-seize entries inexistantes.
        // Le fixe le confirme : `preset_count` vaut bien 99 ici, donc les deux
        // valeurs sont franchement distinctes.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(inst.state.preset_count, Some(99), "le fixe doit bien porter le maximum");
        let lines = traiter_ok(&inst, &["status"]);
        assert!(lines.contains(&"playlistlength: 3".to_string()), "{lines:?}");
        assert!(!lines.contains(&"playlistlength: 99".to_string()), "{lines:?}");
    }

    #[test]
    fn stats_compte_les_entrees_et_non_le_maximum_des_indices() {
        // Le jumeau du precedent sur `stats` : meme confusion possible, meme
        // silence des tests avant qu'une file creuse existe. `songs` est un
        // nombre d'entries.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        let lines = traiter_ok(&inst, &["stats"]);
        assert!(lines.contains(&"songs: 3".to_string()), "{lines:?}");
        assert!(!lines.contains(&"songs: 99".to_string()), "{lines:?}");
    }

    #[test]
    fn play_sur_une_liste_creuse_selectionne_lindice_du_rang_demande() {
        // `position_to_index` vu depuis `handle`, avec une file creuse que
        // le producteur peut vraiment emettre : `play 1` doit selectionner 5.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(cmds(&inst, &["play", "0"]), vec![Command::Select(1)]);
        assert_eq!(cmds(&inst, &["play", "1"]), vec![Command::Select(5)]);
        assert_eq!(cmds(&inst, &["play", "2"]), vec![Command::Select(99)]);
        assert!(
            matches!(traiter_mots(&inst, 0, &["play", "3"]), Outcome::Reject(_)),
            "trois entries, donc le rang 3 n'existe pas — meme si l'index 3 est sous le maximum"
        );
    }

    #[test]
    fn playid_sur_un_trou_de_la_liste_creuse_est_refuse() {
        // `index_exists` vu depuis `handle` : 2 est sous le maximum (99) mais
        // n'est pas une station. Une comparaison de bounded le laisserait passer,
        // et le coeur ignorerait le `Select` en silence.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(cmds(&inst, &["playid", "99"]), vec![Command::Select(99)]);
        assert!(matches!(traiter_mots(&inst, 0, &["playid", "2"]), Outcome::Reject(_)));
    }

    #[test]
    fn le_morceau_courant_dune_liste_creuse_publie_le_rang_et_lindice() {
        // `status` et `currentsong` doivent s'accorder sur les deux nombres :
        // `song`/`Pos` est le rang (1 pour la deuxieme entree), `songid`/`Id`
        // l'index (5). Les confondre ferait surligner la mauvaise line.
        // La trame porte `preset: Some(5)` et le name qui va avec, comme le
        // coeur les publie ensemble ; le sources_catalog porte les trois stations.
        let base = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        let inst = Snapshot {
            state: PlayerState {
                playback: Playback::Playing,
                preset: Some(5),
                preset_name: Some("Nova".into()),
                ..base.state
            },
            optimistic_playback: Playback::Playing,
            ..base
        };
        let status = traiter_ok(&inst, &["status"]);
        assert!(status.contains(&"song: 1".to_string()), "{status:?}");
        assert!(status.contains(&"songid: 5".to_string()), "{status:?}");
        let current = traiter_ok(&inst, &["currentsong"]);
        assert!(current.contains(&"Pos: 1".to_string()), "{current:?}");
        assert!(current.contains(&"Id: 5".to_string()), "{current:?}");
        assert!(current.contains(&"Title: Nova".to_string()), "{current:?}");
    }

    // ------------------------------------------------------------------
    // `status`
    // ------------------------------------------------------------------

    #[test]
    fn status_publie_ses_champs_dans_lordre_attendu() {
        // L'order et la présence sont le contrat : un client read ces lines
        // dans l'order où MPD les émet, et un champ manquant en fait renoncer
        // certains. Egalite du vecteur entier, donc, et non des `contains`.
        assert_eq!(
            traiter_ok(&instantane_en_lecture(), &["status"]),
            vec![
                "volume: 40",
                "repeat: 0",
                "random: 0",
                "single: 0",
                "consume: 0",
                "playlist: 7",
                "playlistlength: 3",
                "mixrampdb: 0.000",
                "state: play",
                "song: 1",
                "songid: 2",
                "time: 12:545",
                "elapsed: 12.000",
                "duration: 545.000",
            ]
        );
    }

    #[test]
    fn status_rend_zero_en_volume_quand_le_son_est_coupe() {
        // MPD n'a pas de sourdine : les clients coupent en posant `setvol 0`,
        // donc ils s'attendent a read 0 quand c'est coupe.
        let inst = instantane_muet(65);
        assert!(traiter_ok(&inst, &["status"]).contains(&"volume: 0".to_string()));
    }

    #[test]
    fn status_ne_nomme_aucune_chanson_a_larret() {
        // `songid: 0` designerait une entree reelle : le champ doit etre absent.
        let lines = traiter_ok(&instantane_arrete(), &["status"]);
        assert!(lines.contains(&"state: stop".to_string()));
        assert!(!lines.iter().any(|l| l.starts_with("song")), "{lines:?}");
    }

    #[test]
    fn status_ne_nomme_aucune_chanson_meme_arrete_sur_une_preselection() {
        // La garde sur l'state de playback, distincte de celle sur `preset` :
        // `instantane_arrete` ne prouve que la seconde (il n'a aucune
        // presélection). Une source arretee qui a garde la sienne ne doit
        // designer aucune chanson dans `status`.
        let mut inst = instantane_arrete();
        inst.state.preset = Some(2);
        let lines = traiter_ok(&inst, &["status"]);
        assert!(!lines.iter().any(|l| l.starts_with("song")), "{lines:?}");
        // `currentsong`, lui, garde son track : l'asymetrie est celle de MPD,
        // qui publie un track current meme a l'arret. Les deux gardes sont
        // donc bien distinctes, et ce test dit laquelle est laquelle.
        assert!(traiter_ok(&inst, &["currentsong"])
            .contains(&"file: ritornello://radio/2".to_string()));
    }

    #[test]
    fn status_dit_les_trois_etats() {
        for (inst, attendu) in [
            (instantane_en_lecture(), "state: play"),
            (instantane_en_pause(), "state: pause"),
            (instantane_arrete(), "state: stop"),
        ] {
            let lines = traiter_ok(&inst, &["status"]);
            assert!(lines.contains(&attendu.to_string()), "{attendu} absent de {lines:?}");
            // Un seul `state`, et c'est le bon : une implementation qui
            // emettrait les trois passerait le `contains` ci-dessus.
            assert_eq!(lines.iter().filter(|l| l.starts_with("state: ")).count(), 1);
        }
    }

    #[test]
    fn status_publie_letat_optimiste_et_non_celui_de_la_trame() {
        // La course de `pause` : un client qui envoie `pause` puis `status`
        // dans la meme foulee doit read l'effet de sa propre commande, meme si
        // la trame confirmante n'est pas encore arrivee.
        let mut inst = instantane_en_pause();
        inst.optimistic_playback = Playback::Playing;
        assert!(traiter_ok(&inst, &["status"]).contains(&"state: play".to_string()));
    }

    #[test]
    fn les_options_sont_rapportees_a_zero_mais_pas_omises() {
        let lines = traiter_ok(&instantane_arrete(), &["status"]);
        for key in ["repeat: 0", "random: 0", "single: 0", "consume: 0"] {
            assert!(lines.contains(&key.to_string()), "{key} absent de {lines:?}");
        }
    }

    #[test]
    fn status_designe_la_chanson_par_sa_position_dense_et_son_indice_creux() {
        // La deuxieme preselection : position 1, index 2. Les deux ne sont pas
        // interchangeables, et les confondre fait surligner la mauvaise line.
        let lines = traiter_ok(&instantane_en_lecture(), &["status"]);
        assert!(lines.contains(&"song: 1".to_string()), "{lines:?}");
        assert!(lines.contains(&"songid: 2".to_string()), "{lines:?}");
    }

    #[test]
    fn status_tait_la_chanson_absente_de_la_file() {
        // Une preselection hors de la file (source qui announcement trois entries et
        // plays la septieme) : un `song: 6` designerait une position que le
        // client ne trouvera pas dans le `playlistinfo` qu'il vient de read.
        let mut inst = instantane_en_lecture();
        inst.state.preset = Some(7);
        let lines = traiter_ok(&inst, &["status"]);
        assert!(!lines.iter().any(|l| l.starts_with("song")), "{lines:?}");
    }

    #[test]
    fn status_omet_le_temps_quand_la_position_est_inconnue() {
        // Un stream dont un plugin announcement la duration du track sans en suivre
        // l'avancement : pas d'`elapsed: 0.000` invente, mais la duration remainder.
        let mut inst = instantane_en_lecture();
        inst.state.position_s = None;
        let lines = traiter_ok(&inst, &["status"]);
        assert!(!lines.iter().any(|l| l.starts_with("elapsed")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("time")), "{lines:?}");
        assert!(lines.contains(&"duration: 545.000".to_string()), "{lines:?}");
    }

    #[test]
    fn status_annonce_un_total_nul_sur_un_direct() {
        // `time: 12:0` : la position est connue, la duration non. C'est ce que MPD
        // fait des stream, et un client qui read `time` ne doit pas y trouver
        // autre chose que deux entiers.
        let mut inst = instantane_en_lecture();
        inst.state.track.duration_s = None;
        let lines = traiter_ok(&inst, &["status"]);
        assert!(lines.contains(&"time: 12:0".to_string()), "{lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("duration")), "{lines:?}");
    }

    // ------------------------------------------------------------------
    // `currentsong`
    // ------------------------------------------------------------------

    #[test]
    fn currentsong_ne_dit_rien_quand_rien_ne_joue() {
        assert_eq!(traiter_ok(&instantane_arrete(), &["currentsong"]), Vec::<String>::new());
    }

    #[test]
    fn currentsong_publie_le_morceau_dans_lordre_attendu() {
        assert_eq!(
            traiter_ok(&instantane_en_lecture(), &["currentsong"]),
            vec![
                "file: ritornello://radio/2",
                "Title: So What",
                "Artist: Miles Davis",
                "Album: Kind of Blue",
                // `Date` s'insert entre l'album et la duration : l'order des
                // lines est celui que ce test fige, et un client qui les read
                // par prefixe s'en moque, mais le figer documente le choix.
                "Date: 1959",
                "Time: 545",
                "duration: 545.000",
                "Pos: 1",
                "Id: 2",
            ]
        );
    }

    #[test]
    fn currentsong_omet_les_champs_inconnus_au_lieu_de_les_vider() {
        // Une station sans titre ICY : pas de line `Title:` clear.
        let lines = traiter_ok(&instantane_sans_titre(), &["currentsong"]);
        assert!(!lines.iter().any(|l| l == "Title: " || l == "Artist: "), "{lines:?}");
        // Et pas de champ clear du tout, quel qu'il soit.
        assert!(!lines.iter().any(|l| l.ends_with(": ")), "{lines:?}");
    }

    #[test]
    fn currentsong_retombe_sur_le_nom_de_la_preselection_faute_de_titre() {
        // Le name de la station est la seule chose qu'on sache d'un stream sans
        // etiquette ICY ; sans ce repli, un client n'affiche que l'URI.
        let lines = traiter_ok(&instantane_sans_titre(), &["currentsong"]);
        assert!(lines.contains(&"Title: Chérie FM".to_string()), "{lines:?}");
    }

    #[test]
    fn currentsong_publie_le_morceau_meme_en_pause() {
        // MPD garde un track current en pause : le taire ferait vider l'ecran
        // du client des qu'il appuie sur pause.
        let lines = traiter_ok(&instantane_en_pause(), &["currentsong"]);
        assert!(lines.contains(&"Title: So What".to_string()), "{lines:?}");
    }

    // ------------------------------------------------------------------
    // `playlistinfo` et `plchanges`
    // ------------------------------------------------------------------

    #[test]
    fn playlistinfo_rend_la_file_entiere_avec_ses_positions_et_ses_indices() {
        assert_eq!(
            traiter_ok(&instantane_sans_presets("cd", 2), &["playlistinfo"]),
            vec![
                "file: ritornello://cd/1",
                "Title: 1",
                "Pos: 0",
                "Id: 1",
                "file: ritornello://cd/2",
                "Title: 2",
                "Pos: 1",
                "Id: 2",
            ]
        );
    }

    #[test]
    fn playlistinfo_a_une_position_ne_rend_que_cette_entree() {
        assert_eq!(
            traiter_ok(&instantane_sans_presets("cd", 3), &["playlistinfo", "2"]),
            vec!["file: ritornello://cd/3", "Title: 3", "Pos: 2", "Id: 3"]
        );
    }

    #[test]
    fn playlistinfo_hors_bornes_ou_non_numerique_est_refuse() {
        // Un refus et non une reponse clear : le client a une file perimee, et
        // un `OK` sec le laisserait croire a un trou dans la liste.
        let inst = instantane_sans_presets("cd", 3);
        for mauvais in ["3", "-1", "abc", ""] {
            assert_eq!(
                traiter_mots(&inst, 1, &["playlistinfo", mauvais]),
                Outcome::Reject("ACK [2@1] {playlistinfo} bad song index".to_string()),
                "position {mauvais:?} acceptee a tort"
            );
        }
    }

    #[test]
    fn playlistinfo_accepte_une_plage_dont_la_fin_est_exclue() {
        // `playlistinfo [[SONGPOS] | [START:END]]` : un client qui fenetre sa
        // file demande `0:100`, et un `ACK` sur une requete bien formee lui fait
        // afficher une file clear. `1:3` rend deux entries, pas trois, et leurs
        // `Pos` restent **absolus** — c'est la key avec laquelle le client
        // designera l'entree ensuite.
        assert_eq!(
            traiter_ok(&instantane_sans_presets("cd", 4), &["playlistinfo", "1:3"]),
            vec![
                "file: ritornello://cd/2",
                "Title: 2",
                "Pos: 1",
                "Id: 2",
                "file: ritornello://cd/3",
                "Title: 3",
                "Pos: 2",
                "Id: 3",
            ]
        );
    }

    #[test]
    fn playlistinfo_accepte_une_plage_a_fin_ouverte() {
        // `START:` veut dire « jusqu'au bout », et une fin au-dela de la file se
        // ramene a la file plutot que de deborder.
        let inst = instantane_sans_presets("cd", 4);
        let ouverte = traiter_ok(&inst, &["playlistinfo", "2:"]);
        assert_eq!(ouverte, traiter_ok(&inst, &["playlistinfo", "2:99"]));
        assert_eq!(
            ouverte,
            vec![
                "file: ritornello://cd/3",
                "Title: 3",
                "Pos: 2",
                "Id: 3",
                "file: ritornello://cd/4",
                "Title: 4",
                "Pos: 3",
                "Id: 4",
            ]
        );
    }

    #[test]
    fn une_plage_qui_commence_apres_la_fin_rend_une_tranche_vide() {
        // Bien formee mais sans objet : un client qui fenetre peut demander
        // `9:12` juste apres que la file a retreci. La reponse est « il n'y a
        // rien la-bas », pas une erreur — contrairement a une position seule
        // hors bounds, qui designe une entree precise et remainder un refus.
        let inst = instantane_sans_presets("cd", 3);
        assert_eq!(traiter_ok(&inst, &["playlistinfo", "9:12"]), Vec::<String>::new());
        assert_eq!(traiter_ok(&inst, &["playlistinfo", "3:3"]), Vec::<String>::new());
        assert!(matches!(
            traiter_mots(&inst, 0, &["playlistinfo", "9"]),
            Outcome::Reject(_)
        ));
    }

    #[test]
    fn une_plage_inversee_est_refusee() {
        // Aucun client correct ne produit `3:1` ; l'accepter masquerait le bogue
        // de l'appelant, et MPD le refuse aussi.
        assert_eq!(
            traiter_mots(&instantane_sans_presets("cd", 4), 0, &["playlistinfo", "3:1"]),
            Outcome::Reject("ACK [2@0] {playlistinfo} bad song index".to_string())
        );
    }

    #[test]
    fn plchanges_accepte_la_meme_fenetre_que_playlistinfo() {
        // `plchanges VERSION [START:END]` : meme grammaire, meme reponse.
        let inst = instantane_sans_presets("cd", 4);
        assert_eq!(
            traiter_ok(&inst, &["plchanges", "6", "0:2"]),
            traiter_ok(&inst, &["playlistinfo", "0:2"])
        );
        assert_eq!(
            traiter_mots(&inst, 0, &["plchanges", "6", "3:1"]),
            Outcome::Reject("ACK [2@0] {plchanges} bad song index".to_string())
        );
    }

    #[test]
    fn plchanges_rend_la_file_entiere_quand_la_version_differe() {
        let inst = instantane_sans_presets("cd", 1);
        assert_eq!(
            traiter_ok(&inst, &["plchanges", "6"]),
            vec!["file: ritornello://cd/1", "Title: 1", "Pos: 0", "Id: 1"]
        );
    }

    #[test]
    fn plchanges_ne_rend_rien_quand_la_version_est_a_jour() {
        // Tout l'interet de la commande : un client qui detient la version
        // courante n'a pas a recevoir 51 lines. `queue_version` vaut 7 dans les
        // instantanes de reference.
        let inst = instantane_sans_presets("cd", 3);
        assert_eq!(inst.queue_version, 7, "l'instantane de reference a change de version");
        assert_eq!(traiter_ok(&inst, &["plchanges", "7"]), Vec::<String>::new());
    }

    #[test]
    fn plchanges_sans_nombre_est_refuse() {
        let inst = instantane_arrete();
        for mots in [vec!["plchanges"], vec!["plchanges", "abc"], vec!["plchanges", "-1"]] {
            assert_eq!(
                traiter_mots(&inst, 0, &mots),
                Outcome::Reject("ACK [2@0] {plchanges} integer expected".to_string()),
                "{mots:?} acceptee a tort"
            );
        }
    }

    // ------------------------------------------------------------------
    // Les commands de montage
    // ------------------------------------------------------------------

    #[test]
    fn commands_nannonce_que_ce_qui_existe() {
        let lines = traiter_ok(&instantane_arrete(), &["commands"]);
        assert!(lines.contains(&"command: status".to_string()));
        // La contrepartie, celle qui rend l'announcement honnete. `search` et
        // `lsinfo` en sont sorties : elles sont desormais gerees — clear et bien
        // formee pour la premiere, les sources pour la seconde — parce qu'un
        // onglet clear vaut mieux qu'un onglet qui plante. Ce qui remainder ici est
        // ce qui n'existe vraiment pas : l'writer de la file, l'writer des
        // listes, et l'extinction.
        for absente in ["delete", "move", "swap", "save", "rm", "playlistadd", "update", "kill"] {
            assert!(!lines.contains(&format!("command: {absente}")), "{absente} annoncee a tort");
        }
    }

    #[test]
    fn chaque_commande_annoncee_est_reellement_geree() {
        // Le pendant du test precedent, et le seul qui empeche `COMMANDS` de
        // deriver du `match` : un name announcement mais tombe dans le refus par
        // defaut se voit ici. Un refus pour cause d'argument (`plchanges` sans
        // version) est legitime — c'est le mot `unsupported` qui trahit une
        // commande qui n'existe pas.
        for name in COMMANDS {
            if let Outcome::Reject(refus) = traiter_mots(&instantane_en_lecture(), 0, &[name]) {
                assert!(!refus.contains("unsupported"), "{name} annoncee mais non geree : {refus}");
            }
        }
    }

    #[test]
    fn notcommands_repond_vide() {
        // Elle liste ce que le mot de passe current **interdit**. Il n'y a pas de
        // mot de passe ici, donc rien n'est interdit par permission : la reponse
        // honnete est clear, et non un refus qui ferait renoncer un vieux client
        // qui la demande juste apres `commands`.
        assert_eq!(traiter_mots(&instantane_arrete(), 0, &["notcommands"]), Outcome::ok());
    }

    #[test]
    fn commandes_est_triee_et_sans_doublon() {
        // L'order alphabetique n'apporte rien aux clients, mais il rend visible
        // le doublon et l'insertion en vrac que les Tasks 7 et 13 vont faire.
        let mut triee: Vec<&str> = COMMANDS.to_vec();
        triee.sort_unstable();
        triee.dedup();
        assert_eq!(triee, COMMANDS.to_vec());
    }

    #[test]
    fn tagtypes_ne_nomme_que_les_quatre_etiquettes_portees() {
        // `Date` en fait partie depuis que `currentsong` l'emet : un client qui
        // ne voit pas une etiquette dans `tagtypes` a le droit de ne jamais en
        // read la line, et l'annee restait alors invisible chez lui.
        assert_eq!(
            traiter_ok(&instantane_arrete(), &["tagtypes"]),
            vec!["tagtype: Artist", "tagtype: Album", "tagtype: Title", "tagtype: Date"]
        );
    }

    #[test]
    fn outputs_annonce_une_sortie_unique_et_activee() {
        // Une sortie desactivee, ou aucune sortie du tout, fait afficher
        // « muet » a un client qui n'insistera pas.
        assert_eq!(
            traiter_ok(&instantane_arrete(), &["outputs"]),
            vec!["outputid: 0", "outputname: default", "outputenabled: 1"]
        );
    }

    #[test]
    fn stats_compte_la_file_et_avoue_ne_rien_savoir_du_reste() {
        // `uptime: 0` est delibere : le rendre juste demanderait de memoriser
        // un instant de depart, donc une horloge dans un module qui n'en a pas.
        let lines = traiter_ok(&instantane_sans_presets("cd", 12), &["stats"]);
        assert!(lines.contains(&"songs: 12".to_string()), "{lines:?}");
        assert!(lines.contains(&"uptime: 0".to_string()), "{lines:?}");
        assert!(lines.contains(&"db_update: 0".to_string()), "{lines:?}");
    }

    #[test]
    fn decoders_et_urlhandlers_repondent_ok_sec_mais_repondent() {
        // Presentes et vides : une commande inconnue au montage peut faire
        // renoncer un client avant qu'il n'affiche un ecran.
        for name in ["decoders", "urlhandlers"] {
            assert_eq!(traiter_mots(&instantane_arrete(), 0, &[name]), Outcome::ok(), "{name}");
        }
    }

    #[test]
    fn ping_password_et_close_ne_demandent_rien_a_lappareil() {
        let inst = instantane_arrete();
        assert_eq!(traiter_mots(&inst, 0, &["ping"]), Outcome::ok());
        // Sans verification, et meme sans argument : il n'y a pas de mot de
        // passe, donc rien a controler et rien a refuser.
        assert_eq!(traiter_mots(&inst, 0, &["password", "secret"]), Outcome::ok());
        assert_eq!(traiter_mots(&inst, 0, &["password"]), Outcome::ok());
        assert_eq!(traiter_mots(&inst, 0, &["close"]), Outcome::Close);
    }

    #[test]
    fn aucune_commande_de_lecture_nemet_de_commande_vers_le_coeur() {
        // La playback seule est vraiment seulement de la playback : un `status`
        // qui agirait sur l'appareil serait un effet de bord invisible, et les
        // clients en envoient plusieurs par seconde.
        let inst = instantane_en_lecture();
        let interrogations: [&[&str]; 14] = [
            &["status"],
            &["currentsong"],
            &["playlistinfo"],
            &["playlistinfo", "0"],
            &["playlistinfo", "0:2"],
            &["plchanges", "0"],
            &["commands"],
            &["notcommands"],
            &["tagtypes"],
            &["outputs"],
            &["stats"],
            &["decoders"],
            &["urlhandlers"],
            &["ping"],
        ];
        for mots in interrogations {
            match traiter_mots(&inst, 0, mots) {
                Outcome::Reply { cmds, .. } => {
                    assert!(cmds.is_empty(), "{mots:?} a emis {cmds:?}");
                }
                autre => panic!("attendu Reply pour {mots:?}, obtenu {autre:?}"),
            }
        }
    }

    // ------------------------------------------------------------------
    // `idle` / `noidle`
    // ------------------------------------------------------------------

    #[test]
    fn idle_sans_argument_attend_les_quatre_sujets() {
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["idle"]),
            Outcome::Wait(vec![
                Subsystem::Player,
                Subsystem::Mixer,
                Subsystem::Playlist,
                Subsystem::StoredPlaylist
            ])
        );
    }

    #[test]
    fn idle_ne_retient_que_les_sujets_nommes_dans_lordre_et_sans_doublon() {
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["idle", "mixer", "player", "mixer"]),
            Outcome::Wait(vec![Subsystem::Mixer, Subsystem::Player])
        );
    }

    #[test]
    fn un_mot_hors_du_vocabulaire_mpd_est_refuse() {
        // Un mot que MPD lui-meme ne connait pas : un client qui a mal
        // orthographie son sous-systeme resterait muet pour toujours, ce qui se
        // diagnostique bien plus mal qu'un `ACK`.
        for mot in ["jukebox", "Player", "stored_playlists", ""] {
            assert_eq!(
                traiter_mots(&instantane_arrete(), 2, &["idle", mot]),
                Outcome::Reject("ACK [2@2] {idle} unrecognized idle event".to_string()),
                "{mot:?} aurait du etre refuse"
            );
        }
    }

    #[test]
    fn idle_accepte_les_sous_systemes_de_mpd_que_nous_nemettons_jamais() {
        // Le defaut vu de l'autre cote : tout client bati sur
        // `mpd_send_idle_mask` de libmpdclient envoie une liste explicite, en
        // pratique `database update stored_playlist playlist player mixer output
        // options`. Reject un mot **legal** lui vaudrait un `ACK` sur son
        // premier `idle`, donc une boucle ou un abandon.
        let inst = instantane_arrete();
        let mots = [
            "idle",
            "database",
            "update",
            "stored_playlist",
            "playlist",
            "player",
            "mixer",
            "output",
            "options",
        ];
        assert_eq!(
            traiter_mots(&inst, 0, &mots),
            Outcome::Wait(vec![Subsystem::StoredPlaylist, Subsystem::Playlist, Subsystem::Player, Subsystem::Mixer])
        );
        // Et les quatre autres names du vocabulaire, ceux qu'aucun client
        // current n'envoie mais que MPD connait.
        for mot in ["partition", "sticker", "subscription", "message", "neighbor", "mount"] {
            assert_eq!(
                traiter_mots(&inst, 0, &["idle", mot]),
                Outcome::Wait(Vec::new()),
                "{mot} devrait etre accepte puis ecarte"
            );
        }
    }

    #[test]
    fn une_attente_sur_un_sujet_quon_nemet_jamais_est_vide_et_non_immediate() {
        // `Wait(vec![])` n'est pas `OK` : le client a demande a etre prevenu
        // d'un changement qui n'arrivera jamais, et wait pour toujours est la
        // reponse MPD correcte. Le contrat est note sur la variante, parce que
        // c'est la Task 8 qui pourrait le trahir en traitant le clear comme un
        // `OK` sec.
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["idle", "database"]),
            Outcome::Wait(Vec::new())
        );
    }

    #[tokio::test]
    async fn une_liste_melangee_garde_le_reveil_du_sujet_quon_emet() {
        // `idle database mixer` : `database` est accepte puis ecarte, et cet
        // ecart ne doit pas emporter avec lui le wakeup de `mixer`. Verifie de
        // bout en bout contre l'state partage, et pas seulement sur la charge
        // utile de l'`Outcome`.
        let issue = traiter_mots(&instantane_arrete(), 0, &["idle", "database", "mixer"]);
        let Outcome::Wait(subsystems) = issue else {
            panic!("attendu Wait, obtenu {issue:?}");
        };
        assert_eq!(subsystems, vec![Subsystem::Mixer]);

        let partage = SharedState::default();
        let seen = partage.versions().await;
        partage.apply_state(PlayerState { volume: 55, ..Default::default() }).await;
        // Aucune marge d'horloge : le changement a **deja** eu lieu, donc
        // `wait` rend la main par sa comparaison prealable sans jamais
        // dormir. Si `mixer` avait ete ecarte avec `database`, la liste serait
        // clear et ce test **pendrait** — l'echec est franc, a l'idiome des tests
        // d'`state.rs`.
        assert_eq!(partage.wait(&subsystems, seen).await.moved, vec![Subsystem::Mixer]);
    }

    #[test]
    fn noidle_rend_la_main_sans_attendre() {
        assert_eq!(traiter_mots(&instantane_arrete(), 0, &["noidle"]), Outcome::Cancel);
    }

    // ------------------------------------------------------------------
    // `play` / `playid`
    // ------------------------------------------------------------------

    #[test]
    fn position_vers_index_choisit_le_rang_et_non_lindice_moins_un() {
        // Le décalage qui coûte cher : sur des indices 1, 5, 99, le rang 1
        // (base 0, deuxième entrée) doit rendre 5 — pas 2 (le rang « plus
        // un »), ni aucun autre calcul dérivé de la position. Une file
        // construite à la main : voir la limit documentée sur
        // `instantane_avec_presets`, `queue` ne sait pas encore
        // synthétiser une suite creuse.
        let file = vec![
            Entry { index: 1, name: "FIP".into() },
            Entry { index: 5, name: "France Inter".into() },
            Entry { index: 99, name: "Nova".into() },
        ];
        assert_eq!(position_to_index(&file, 0), Some(1));
        assert_eq!(position_to_index(&file, 1), Some(5));
        assert_eq!(position_to_index(&file, 2), Some(99));
        assert_eq!(position_to_index(&file, 3), None, "hors de la file");
    }

    #[test]
    fn index_existe_verifie_lappartenance_et_non_la_borne() {
        // 2 est bien inférieur au maximum de la file (5), mais absent : un
        // `playid 2` doit refuser, ce qu'une comparaison de bounded laisserait
        // passer à tort une fois la file creuse (Task 13).
        let file = vec![Entry { index: 1, name: "FIP".into() }, Entry { index: 5, name: "France Inter".into() }];
        assert!(index_exists(&file, 5));
        assert!(!index_exists(&file, 2), "2 est sous le maximum (5) mais absent de la file");
    }

    #[test]
    fn play_avec_une_position_selectionne_lentree_de_ce_rang() {
        // Le path de bout en bout, dans les limites de ce que
        // `instantane_avec_presets` peut construire aujourd'hui (voir sa
        // doc) : une file dense où le rang est vérifié en passant par
        // `handle`, pas par un appel direct à `position_to_index`.
        let inst = instantane_avec_presets("radio", &[(1, "un"), (2, "deux"), (3, "trois")]);
        assert_eq!(cmds(&inst, &["play", "0"]), vec![Command::Select(1)]);
        assert_eq!(cmds(&inst, &["play", "2"]), vec![Command::Select(3)]);
    }

    #[test]
    fn playid_verifie_lexistence_via_traiter() {
        let inst = instantane_avec_presets("radio", &[(1, "un"), (2, "deux")]);
        assert_eq!(cmds(&inst, &["playid", "2"]), vec![Command::Select(2)]);
    }

    #[test]
    fn play_hors_bornes_est_refuse_et_nemet_rien() {
        let inst = instantane_avec_presets("radio", &[(1, "FIP")]);
        assert!(matches!(handle(&inst, 0, &["play".into(), "7".into()], MAX_CHUNK), Outcome::Reject(_)));
    }

    #[test]
    fn playid_dun_indice_absent_est_refuse() {
        let inst = instantane_avec_presets("radio", &[(1, "FIP")]);
        assert!(matches!(handle(&inst, 0, &["playid".into(), "9".into()], MAX_CHUNK), Outcome::Reject(_)));
    }

    #[test]
    fn play_et_playid_avec_un_argument_non_numerique_sont_refuses() {
        // `play` sans argument n'est *pas* un refus (c'est la touche Lecture,
        // voir le test suivant) ; c'est seulement un argument non numérique,
        // ou l'absence du seul argument de `playid`, qui doivent l'être.
        let inst = instantane_avec_presets("radio", &[(1, "FIP")]);
        for mots in [vec!["play", "abc"], vec!["playid"], vec!["playid", "abc"]] {
            assert!(matches!(traiter_mots(&inst, 0, &mots), Outcome::Reject(_)), "{mots:?}");
        }
    }

    #[test]
    fn play_sans_argument_relance_ce_qui_etait_charge() {
        // La touche Lecture, pas une sélection.
        let inst = instantane_arrete();
        assert_eq!(cmds(&inst, &["play"]), vec![Command::PlayPause]);
    }

    // ------------------------------------------------------------------
    // `pause`
    // ------------------------------------------------------------------

    #[test]
    fn pause_nemet_rien_quand_letat_est_deja_celui_demande() {
        // C'est ce qui ferme la course : un `pause 1` sur une playback déjà en
        // pause ne doit pas la relancer.
        let inst = instantane_en_pause();
        assert_eq!(cmds(&inst, &["pause", "1"]), Vec::<Command>::new());
        assert_eq!(cmds(&inst, &["pause", "0"]), vec![Command::PlayPause]);
    }

    #[test]
    fn pause_sans_argument_bascule() {
        assert_eq!(cmds(&instantane_en_lecture(), &["pause"]), vec![Command::PlayPause]);
    }

    #[test]
    fn pause_sur_un_lecteur_a_larret_nemet_jamais_rien() {
        // Règle distincte de la comparaison état/cible ci-dessus : `PlayPause`
        // à l'arrêt démarrerait une playback dont ni la source ni ce greffon ne
        // savent rien (voir `SharedState::acknowledge_optimistic`), ce qu'un client
        // n'a pas demandé en appuyant sur « pause ».
        let inst = instantane_arrete();
        assert_eq!(cmds(&inst, &["pause"]), Vec::<Command>::new());
        assert_eq!(cmds(&inst, &["pause", "0"]), Vec::<Command>::new());
        assert_eq!(cmds(&inst, &["pause", "1"]), Vec::<Command>::new());
    }

    #[test]
    fn pause_avec_un_argument_invalide_est_refusee_meme_a_larret() {
        // La validation de l'argument passe avant la garde de l'arrêt : un
        // `pause 2` malformé doit rester un `ACK`, pas être avalé en silence
        // par « rien à faire à l'arrêt ».
        assert!(matches!(
            traiter_mots(&instantane_arrete(), 0, &["pause", "2"]),
            Outcome::Reject(_)
        ));
    }

    // ------------------------------------------------------------------
    // `setvol` / `volume`
    // ------------------------------------------------------------------

    #[test]
    fn setvol_borne_et_refuse_hors_intervalle() {
        let inst = instantane_arrete();
        assert_eq!(cmds(&inst, &["setvol", "40"]), vec![Command::SetVolume(40)]);
        assert!(matches!(handle(&inst, 0, &["setvol".into(), "101".into()], MAX_CHUNK), Outcome::Reject(_)));
        assert!(matches!(handle(&inst, 0, &["setvol".into(), "abc".into()], MAX_CHUNK), Outcome::Reject(_)));
        assert!(matches!(handle(&inst, 0, &["setvol".into()], MAX_CHUNK), Outcome::Reject(_)));
    }

    #[test]
    fn setvol_zero_nest_pas_traduit_en_sourdine() {
        // Ce serait deviner : `Mute` bascule, `SetVolume(0)` pose. Traduire
        // ferait qu'un client remontant le volume tomberait sur un son
        // toujours coupé.
        assert_eq!(cmds(&instantane_au_volume(65), &["setvol", "0"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn setvol_au_dessus_de_zero_leve_la_sourdine() {
        // **Le seul path dont un client MPD dispose pour rallumer le son.**
        // `status` publie `volume: 0` dès que l'appareil est muet, donc le
        // client remonte son curseur, `SetVolume(40)` part, le volume change —
        // et le son restait coupé, sans aucune issue depuis le téléphone.
        // L'order est épinglé ici parce que le test compare un `Vec`, pas parce
        // qu'il changerait le résultat : les deux ordres laissent l'appareil non
        // muet à 40 (le cœur ne repose aucun volume en démutant, voir `setvol`).
        // Ce qu'il préserve est l'intervalle — le son revient déjà à 40 au lieu
        // de repasser par le niveau mémorisé.
        assert_eq!(
            cmds(&instantane_muet(65), &["setvol", "40"]),
            vec![Command::SetVolume(40), Command::Mute]
        );
    }

    #[test]
    fn setvol_nemet_pas_de_sourdine_quand_le_son_nest_pas_coupe() {
        // L'autre sens, et il est essentiel : `Command::Mute` est une
        // **bascule**, donc l'émettre inconditionnellement couperait le son du
        // client qui vient de monter le sien. Même forme conditionnelle que
        // `pause 0`/`pause 1` contre `playback`.
        assert_eq!(cmds(&instantane_au_volume(65), &["setvol", "40"]), vec![Command::SetVolume(40)]);
    }

    #[test]
    fn setvol_zero_sur_un_appareil_muet_ne_leve_rien() {
        // Le cas limit des deux règles réunies : poser zéro n'est pas
        // « demander à entendre », donc rien à lever — et lever ici rallumerait
        // le son d'un client qui demande le silence.
        assert_eq!(cmds(&instantane_muet(65), &["setvol", "0"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn volume_relatif_leve_aussi_la_sourdine() {
        // Même geste, même règle : `volume` est dépréciée mais c'est la vieille
        // moitié du parc de clients, et la laisser sans issue ferait dépendre le
        // retour du son de l'âge du client. Le calcul part du volume
        // **mémorisé** (65) et non du zéro que `status` publie.
        assert_eq!(
            cmds(&instantane_muet(65), &["volume", "+10"]),
            vec![Command::SetVolume(75), Command::Mute]
        );
        // Et un recul qui atteint zéro ne lève rien, comme `setvol 0`.
        assert_eq!(cmds(&instantane_muet(5), &["volume", "-10"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn volume_est_relatif_et_borne_sur_le_volume_courant() {
        // Commande dépréciée mais encore émise. Bornée ici, pas laissée
        // déborder.
        let inst = instantane_au_volume(95);
        assert_eq!(cmds(&inst, &["volume", "+10"]), vec![Command::SetVolume(100)]);
        assert_eq!(cmds(&instantane_au_volume(3), &["volume", "-10"]), vec![Command::SetVolume(0)]);
    }

    #[test]
    fn volume_aux_bornes_de_i16_est_clampee_sans_deborder() {
        // `delta` est parsé tel quel depuis l'argument client, donc n'importe
        // où dans `±32767` : additionner ce maximum à un volume current même
        // faible dépasse `i16` avant que `.clamp` n'ait pu acting. Un panic en
        // debug/test (les vérifications de dépassement sont actives par
        // défaut dans ce profil), une valeur fausse en release — sur un port
        // ouvert au réseau local, sans authentification. Les trois volumes de
        // départ (faible, nul, fort) couvrent les deux sens du débordement.
        assert_eq!(
            cmds(&instantane_au_volume(1), &["volume", "32767"]),
            vec![Command::SetVolume(100)]
        );
        assert_eq!(
            cmds(&instantane_au_volume(0), &["volume", "32767"]),
            vec![Command::SetVolume(100)]
        );
        assert_eq!(
            cmds(&instantane_au_volume(50), &["volume", "-32768"]),
            vec![Command::SetVolume(0)]
        );
    }

    // ------------------------------------------------------------------
    // `seek` / `seekid` / `seekcur`
    // ------------------------------------------------------------------

    #[test]
    fn seekcur_resout_le_relatif_avant_demettre_un_absolu() {
        // `Command` ne porte qu'un positionnement absolu : la résolution est
        // ici.
        let inst = instantane_a_la_position(30);
        assert_eq!(cmds(&inst, &["seekcur", "+10"]), vec![Command::SeekTo(40)]);
        assert_eq!(cmds(&inst, &["seekcur", "-10"]), vec![Command::SeekTo(20)]);
        assert_eq!(cmds(&inst, &["seekcur", "12.5"]), vec![Command::SeekTo(12)]);
        // Un recul plus grand que la position ne produit pas de temps négatif.
        assert_eq!(cmds(&instantane_a_la_position(3), &["seekcur", "-10"]), vec![Command::SeekTo(0)]);
    }

    #[test]
    fn seekcur_relatif_sans_position_connue_est_refuse() {
        // Résoudre un relatif sans point de départ inventerait un temps : ni
        // 0 ni aucune autre valeur silencieuse.
        let inst = instantane_arrete();
        assert_eq!(inst.state.position_s, None, "l'instantane de reference n'a pas de position");
        assert!(matches!(
            traiter_mots(&inst, 0, &["seekcur", "+10"]),
            Outcome::Reject(_)
        ));
    }

    #[test]
    fn seekcur_sans_argument_ou_non_numerique_est_refuse() {
        let inst = instantane_a_la_position(10);
        for mots in [vec!["seekcur"], vec!["seekcur", "abc"], vec!["seekcur", "+abc"]] {
            assert!(matches!(traiter_mots(&inst, 0, &mots), Outcome::Reject(_)), "{mots:?}");
        }
    }

    #[test]
    fn seek_et_seekid_ignorent_leur_premier_argument() {
        // `Command::SeekTo` ne sait pas changer de piste en même temps ; MPD
        // n'envoie de toute façon `seek` que sur ce qui plays.
        let inst = instantane_a_la_position(0);
        assert_eq!(cmds(&inst, &["seek", "0", "42"]), vec![Command::SeekTo(42)]);
        assert_eq!(cmds(&inst, &["seekid", "1", "42"]), vec![Command::SeekTo(42)]);
    }

    #[test]
    fn seek_normalise_un_signe_plus_redondant_en_tete_du_temps() {
        // `seek`/`seekid` restent absolus : un `+` en tête n'y est qu'un signe
        // de nombre comme un autre (`absolute_time` ne distingue pas la forme
        // relative, réservée à `seekcur`), donc `+5` et `5` doivent produire
        // exactement la même commande.
        let inst = instantane_a_la_position(0);
        assert_eq!(cmds(&inst, &["seek", "0", "+5"]), cmds(&inst, &["seek", "0", "5"]));
    }

    #[test]
    fn les_temps_non_finis_sont_refuses_et_non_avales() {
        // `inf` et `nan` se parsent en `f64`, et sans garde `seek 0 inf` rendait
        // `SeekTo(u32::MAX)` tandis que `seek 0 nan` rendait `SeekTo(0)` — tous
        // deux **en silence**, contre la règle que ce module énonce : un
        // argument non numérique est un `ACK 2`, jamais un défaut muet. Même
        // classe que le débordement d'`i16` de `volume`, à deux mètres de là.
        //
        // Les trois formes du protocol sont couvertes, parce que le relatif de
        // `seekcur` a sa propre analyse et donc son propre trou.
        let inst = instantane_a_la_position(30);
        for mots in [
            vec!["seek", "0", "inf"],
            vec!["seek", "0", "-inf"],
            vec!["seek", "0", "nan"],
            vec!["seek", "0", "NaN"],
            vec!["seekid", "1", "inf"],
            vec!["seekcur", "inf"],
            vec!["seekcur", "nan"],
            vec!["seekcur", "+inf"],
            vec!["seekcur", "-inf"],
            vec!["seekcur", "+nan"],
        ] {
            assert!(
                matches!(traiter_mots(&inst, 0, &mots), Outcome::Reject(_)),
                "{mots:?} doit etre refuse, pas avale"
            );
        }
        // Et la forme légitime la plus proche remainder acceptée : `infini` n'est
        // pas un nombre, mais `1e9` en est un.
        assert_eq!(cmds(&inst, &["seek", "0", "1000000"]), vec![Command::SeekTo(1_000_000)]);
    }

    #[test]
    fn seek_et_seekid_sans_temps_sont_refuses() {
        let inst = instantane_a_la_position(0);
        assert!(matches!(traiter_mots(&inst, 0, &["seek", "0"]), Outcome::Reject(_)));
        assert!(matches!(traiter_mots(&inst, 0, &["seekid", "1"]), Outcome::Reject(_)));
    }

    // ------------------------------------------------------------------
    // Les touches simples
    // ------------------------------------------------------------------

    #[test]
    fn les_touches_simples_passent_telles_quelles() {
        let inst = instantane_en_lecture();
        assert_eq!(cmds(&inst, &["next"]), vec![Command::Next]);
        assert_eq!(cmds(&inst, &["previous"]), vec![Command::Prev]);
        assert_eq!(cmds(&inst, &["stop"]), vec![Command::Stop]);
    }

    // ------------------------------------------------------------------
    // Les listes enregistrées : `listplaylists`, `listplaylistinfo`, `load`
    // ------------------------------------------------------------------

    #[test]
    fn listplaylists_nomme_une_liste_par_source() {
        let inst = instantane_avec_catalogue(&["radio", "cd", "fichiers"]);
        let lines = traiter_ok(&inst, &["listplaylists"]);
        assert_eq!(lines.iter().filter(|l| l.starts_with("playlist: ")).count(), 3);
        assert!(lines.contains(&"playlist: radio".to_string()), "{lines:?}");
    }

    #[test]
    fn listplaylists_garde_lordre_du_catalogue() {
        // L'order reçu est celui de la bascule de `SourceCycle`, donc celui que
        // l'utilisateur voit sur sa télécommande : le trier alphabétiquement
        // perdrait une information que le client peut afficher.
        let inst = instantane_avec_catalogue(&["radio", "cd", "fichiers"]);
        let names: Vec<String> = traiter_ok(&inst, &["listplaylists"])
            .into_iter()
            .filter_map(|l| l.strip_prefix("playlist: ").map(str::to_string))
            .collect();
        assert_eq!(names, vec!["radio", "cd", "fichiers"]);
    }

    #[test]
    fn listplaylists_rend_une_date_par_entree() {
        // `Last-Modified` est émis et non omis : des clients le lisent, et son
        // absence les fait trébucher. La valeur est une constante — aucune date
        // n'existe côté appareil, et une horloge ferait croire à un changement
        // à chaque relecture.
        let inst = instantane_avec_catalogue(&["radio", "cd"]);
        assert_eq!(
            traiter_ok(&inst, &["listplaylists"]),
            vec![
                "playlist: radio",
                "Last-Modified: 1970-01-01T00:00:00Z",
                "playlist: cd",
                "Last-Modified: 1970-01-01T00:00:00Z",
            ]
        );
    }

    #[test]
    fn listplaylists_est_vide_avant_le_premier_catalogue() {
        // Un `OK` sec, pas un refus : le greffon ne connaît alors aucune
        // source, et c'est la vérité de cet instant. Le client relira après son
        // réveil sur `stored_playlist`.
        assert_eq!(traiter_mots(&instantane_arrete(), 0, &["listplaylists"]), Outcome::ok());
    }

    #[test]
    fn listplaylistinfo_rend_les_vrais_noms() {
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        let lines = traiter_ok(&inst, &["listplaylistinfo", "radio"]);
        assert!(lines.contains(&"Title: FIP".to_string()), "{lines:?}");
        assert!(lines.contains(&"Title: Nova".to_string()), "{lines:?}");
        // Et l'URI porte l'index **creux**, pas un rang : c'est la clé stable
        // avec laquelle le client retrouvera l'entrée dans la file.
        assert!(lines.contains(&"file: ritornello://radio/5".to_string()), "{lines:?}");
    }

    #[test]
    fn listplaylistinfo_interroge_une_source_qui_ne_joue_pas() {
        // Le cas qui a motivé le contournement du garde côté cœur : le
        // sources_catalog décrit toutes les sources, et un client peut read la liste
        // de la radio pendant qu'un disque tourne.
        let inst = instantane_actif_sur("cd", &[("radio", &[(1, "FIP")])]);
        assert_eq!(inst.state.source, "cd", "le fixe doit bien jouer autre chose");
        assert!(traiter_ok(&inst, &["listplaylistinfo", "radio"])
            .contains(&"Title: FIP".to_string()));
    }

    #[test]
    fn listplaylistinfo_nemet_ni_pos_ni_id() {
        // `Pos` et `Id` désignent une entrée de la **file d'attente**, et une
        // liste enregistrée n'est pas chargée : les émettre donnerait à un
        // client des positions qu'il ne retrouverait pas dans son
        // `playlistinfo`. MPD ne les publie pas non plus ici.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        let lines = traiter_ok(&inst, &["listplaylistinfo", "radio"]);
        assert!(
            !lines.iter().any(|l| l.starts_with("Pos: ") || l.starts_with("Id: ")),
            "{lines:?}"
        );
    }

    #[test]
    fn listplaylistinfo_de_la_source_active_sans_liste_dit_la_meme_chose_que_la_file() {
        // Le cd qui plays : il ne sait pas énumérer, mais `preset_count` décrit
        // bien ses douze pistes, et les deux réponses doivent s'accorder — un
        // client qui compare la liste enregistrée à la file ne doit pas voir
        // deux appareils différents.
        let inst = Snapshot {
            sources_catalog: SourcesCatalog { sources: vec![source_catalogue("cd", &[])] },
            ..instantane_sans_presets("cd", 12)
        };
        let lines = traiter_ok(&inst, &["listplaylistinfo", "cd"]);
        assert_eq!(lines.len(), 24, "deux lines par piste : {lines:?}");
        assert!(lines.contains(&"Title: 12".to_string()), "{lines:?}");
    }

    #[test]
    fn listplaylistinfo_dune_source_inactive_sans_liste_est_vide_et_non_devinee() {
        // `preset_count` ne décrit que la source **active** : deviner le nombre
        // de pistes d'un disque qui ne plays pas serait une invention, et une
        // liste clear bien formée est la réponse honnête.
        let inst = instantane_actif_sur("radio", &[("radio", &[(1, "FIP")]), ("cd", &[])]);
        assert_eq!(traiter_mots(&inst, 0, &["listplaylistinfo", "cd"]), Outcome::ok());
    }

    #[test]
    fn un_nom_de_liste_inconnu_est_un_ack_50() {
        let inst = instantane_avec_catalogue(&["radio"]);
        assert_eq!(
            traiter_mots(&inst, 0, &["listplaylistinfo", "nawak"]),
            Outcome::Reject("ACK [50@0] {listplaylistinfo} no such playlist".to_string())
        );
    }

    #[test]
    fn un_nom_de_liste_absent_est_un_ack_2_et_non_un_50() {
        // Le name manquant n'est pas une liste inexistante mais une syntaxe
        // fautive : `ACK 2`, avec l'index de la commande dans sa liste.
        let inst = instantane_avec_catalogue(&["radio"]);
        for cmd in ["listplaylistinfo", "load"] {
            assert_eq!(
                traiter_mots(&inst, 3, &[cmd]),
                Outcome::Reject(format!("ACK [2@3] {{{cmd}}} wrong number of arguments"))
            );
        }
    }

    #[test]
    fn load_bascule_de_source() {
        let inst = instantane_avec_catalogue(&["radio", "cd"]);
        assert_eq!(cmds(&inst, &["load", "cd"]), vec![Command::SelectSource("cd".into())]);
    }

    #[test]
    fn load_dun_nom_inconnu_est_refuse_et_nemet_rien() {
        // Le greffon ne propose que des names reçus du sources_catalog : c'est lui qui
        // refuse, pas le cœur en silence (`SelectSource` d'un name inconnu y est
        // ignoré, et un `OK` suivi de rien est la pire réponse possible pour un
        // client, qui attendrait un changement de file qui n'arrive jamais).
        let inst = instantane_avec_catalogue(&["radio"]);
        assert_eq!(
            traiter_mots(&inst, 0, &["load", "nawak"]),
            Outcome::Reject("ACK [50@0] {load} no such playlist".to_string())
        );
    }

    #[test]
    fn load_de_la_source_deja_active_bascule_quand_meme() {
        // Aucune ruse ici : c'est le cœur qui sait si `SelectSource` sur la
        // source courante restart ou ne fait rien, et deviner à sa place ferait
        // avaler en silence le `load` d'un client qui vient de perdre son état.
        let inst = instantane_avec_catalogue(&["radio", "cd"]);
        assert_eq!(cmds(&inst, &["load", "radio"]), vec![Command::SelectSource("radio".into())]);
    }

    #[test]
    fn les_trois_commandes_de_liste_sont_desormais_annoncees() {
        // La Task 7 les taisait volontairement : `load` refusait tout name,
        // faute de sources_catalog, et l'annoncer aurait rompu l'honnêteté que
        // `commands` promet. Le sources_catalog est là, elles marchent, elles se
        // déclarent.
        let lines = traiter_ok(&instantane_avec_catalogue(&["radio"]), &["commands"]);
        for name in ["load", "listplaylists", "listplaylistinfo"] {
            assert!(COMMANDS.contains(&name), "{name} absente de COMMANDS");
            assert!(lines.contains(&format!("command: {name}")), "{name} non annoncee");
        }
    }

    // ------------------------------------------------------------------
    // Les refus
    // ------------------------------------------------------------------

    #[test]
    fn une_commande_inconnue_est_refusee_avec_son_indice_de_liste() {
        let inst = instantane_arrete();
        assert_eq!(
            handle(&inst, 3, &["nawak".to_string()], MAX_CHUNK),
            Outcome::Reject("ACK [5@3] {nawak} unsupported".to_string())
        );
    }

    #[test]
    fn les_commandes_decriture_sont_refusees_une_par_une() {
        // Elles doivent l'etre explicitement, pas par defaut : c'est la liste
        // que la doc promet, et un futur `add` accidentellement gere se verrait
        // ici. La liste est celle du § « Ce que le greffon ne fait pas ».
        //
        // **Les six interrogations de bibliotheque en sont sorties** (`lsinfo`,
        // `listall`, `listallinfo`, `search`, `find`, `list`, `count`) : elles
        // repondent desormais, clear et bien forme faute de base de donnees —
        // sauf `lsinfo`, qui rend les sources. Le refus etait un defaut visible
        // chez le client, dont l'onglet affichait une erreur la ou une liste
        // clear n'aurait rien affiche. Ce qui remainder ici est l'writer, qui n'a
        // pas de sens sur cet appareil, et elle seule.
        for cmd in [
            "update",
            "delete",
            "deleteid",
            "move",
            "swap",
            "shuffle",
            "save",
            "rm",
            "rename",
            "playlistadd",
            "playlistdelete",
            "repeat",
            "random",
            "single",
            "consume",
            "crossfade",
            "replay_gain_mode",
            "enableoutput",
            "disableoutput",
            "subscribe",
            "sendmessage",
            "kill",
            // `albumart`, `readpicture` puis `binarylimit` ont figuré ici et
            // n'y sont plus : elles sont désormais gérées, et c'est bien cette
            // liste-là qui devait changer — la retirer d'ici est la moitié
            // « traité ⊆ COMMANDS » du couple d'invariants, l'autre étant
            // `chaque_commande_annoncee_est_reellement_geree`.
        ] {
            assert_eq!(
                traiter_mots(&instantane_arrete(), 0, &[cmd]),
                Outcome::Reject(format!("ACK [5@0] {{{cmd}}} unsupported")),
                "{cmd} devrait etre refusee"
            );
        }
    }

    // ------------------------------------------------------------------
    // La bibliothèque : ce qu'un client peut parcourir
    // ------------------------------------------------------------------

    #[test]
    fn add_dune_uri_de_la_source_active_joue_cette_entree() {
        // **Le geste que le owner a signale casse** : toucher une piste
        // dans une liste enregistree renvoyait `ACK 5`. Sur la source deja
        // active, une seule commande suffit.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        assert_eq!(cmds(&inst, &["add", "ritornello://radio/5"]), vec![Command::Select(5)]);
    }

    #[test]
    fn add_dune_autre_source_la_choisit_avant_de_jouer() {
        // Deux commands, dans cet order : la file *est* la liste de la source,
        // donc jouer une entree d'ailleurs veut dire changer de source d'abord.
        let inst = instantane_actif_sur("radio", &[("radio", &[(1, "FIP")]), ("cd", &[(2, "Piste 2")])]);
        assert_eq!(
            cmds(&inst, &["add", "ritornello://cd/2"]),
            vec![Command::SelectSource("cd".into()), Command::Select(2)]
        );
    }

    #[test]
    fn addid_rend_lidentifiant_comme_le_fait_mpd() {
        // Le seul ecart entre les deux commands. Sa position eventuelle est
        // ignoree : il n'y a pas de file ou inserer.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        assert_eq!(traiter_ok(&inst, &["addid", "ritornello://radio/5", "0"]), vec!["Id: 5"]);
        assert_eq!(
            cmds(&inst, &["addid", "ritornello://radio/5", "0"]),
            vec![Command::Select(5)]
        );
    }

    #[test]
    fn add_dune_uri_qui_ne_designe_rien_est_refuse() {
        // Y compris un index **dans les bounds mais absent** d'une table
        // creuse : meme regle que `playid`, une bounded ne suffit pas.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        for uri in [
            "ritornello://radio/3",     // trou de la suite creuse
            "ritornello://inconnue/1",  // source absente du sources_catalog
            "/musique/piste.flac",      // pas une de nos URI
            "ritornello://radio",       // tronquee
            "ritornello:///1",          // source clear
        ] {
            assert_eq!(
                traiter_mots(&inst, 0, &["add", uri]),
                Outcome::Reject("ACK [50@0] {add} No such song".to_string()),
                "{uri} acceptee a tort"
            );
        }
        assert_eq!(
            traiter_mots(&inst, 0, &["add"]),
            Outcome::Reject("ACK [2@0] {add} wrong number of arguments".to_string())
        );
    }

    #[test]
    fn clear_est_accepte_sans_rien_faire() {
        // Il n'y a pas de file a vider. Un `ACK` interromprait la liste
        // `clear`/`add`/`play` qu'un client envoie pour jouer une piste, donc
        // le refus couterait exactement la fonction qu'on vient d'add.
        let inst = instantane_avec_presets("radio", &[(1, "FIP")]);
        assert_eq!(traiter_ok(&inst, &["clear"]), Vec::<String>::new());
        assert_eq!(cmds(&inst, &["clear"]), Vec::<Command>::new());
    }

    #[test]
    fn lsinfo_a_la_racine_rend_les_sources_comme_listplaylists() {
        // Le navigateur de fichiers d'un client doit montrer ce que l'appareil
        // a : ses sources. Les deux commands doivent répondre exactement la
        // même chose de la racine, sinon un client verrait deux bibliothèques
        // différentes selon l'onglet.
        let inst = instantane_avec_catalogue(&["radio", "cd", "files"]);
        let attendu = traiter_ok(&inst, &["listplaylists"]);
        assert!(attendu.contains(&"playlist: radio".to_string()));
        for racine in [vec!["lsinfo"], vec!["lsinfo", ""], vec!["lsinfo", "/"]] {
            assert_eq!(traiter_ok(&inst, &racine), attendu, "{racine:?}");
        }
    }

    #[test]
    fn lsinfo_dune_source_rend_ses_entrees_comme_listplaylistinfo() {
        // Descendre dans une source doit donner ses présélections, et les mêmes
        // que la commande de liste enregistrée : c'est le même contenu vu par
        // deux chemins, et les laisser diverger ferait jouer autre chose que ce
        // qu'on a touché à l'écran.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        assert_eq!(
            traiter_ok(&inst, &["lsinfo", "radio"]),
            traiter_ok(&inst, &["listplaylistinfo", "radio"])
        );
    }

    #[test]
    fn lsinfo_dun_nom_inconnu_est_refuse() {
        // Un `OK` clear laisserait croire à un dossier réellement clear.
        assert_eq!(
            traiter_mots(&instantane_avec_catalogue(&["radio"]), 0, &["lsinfo", "musique"]),
            Outcome::Reject("ACK [50@0] {lsinfo} No such directory".to_string())
        );
    }

    #[test]
    fn les_interrogations_de_base_repondent_vide_et_bien_forme() {
        // **Vide plutôt que refusé**, et c'est la correction : l'onglet
        // « Albums » d'un client recevait `ACK 5` et affichait une erreur, là
        // où une réponse clear n'affiche rien. Il n'y a pas de base de données
        // ici, et le dire par une liste clear est honnête ; le dire par un refus
        // était juste illisible.
        let inst = instantane_en_lecture();
        for mots in [
            vec!["list", "album"],
            vec!["listall"],
            vec!["listallinfo"],
            vec!["listfiles"],
            vec!["find", "album", "Kind of Blue"],
            vec!["search", "any", "miles"],
        ] {
            assert_eq!(traiter_ok(&inst, &mots), Vec::<String>::new(), "{mots:?}");
        }
        // `count` rend ses deux champs : les clients les lisent sans les tester.
        assert_eq!(
            traiter_ok(&inst, &["count", "album", "Kind of Blue"]),
            vec!["songs: 0".to_string(), "playtime: 0".to_string()]
        );
    }

    #[test]
    fn une_recherche_sans_filtre_reste_refusee() {
        // Une requête tronquée doit s'apprendre, sinon le client croit que sa
        // search n'a rien donné.
        for cmd in ["find", "search"] {
            assert_eq!(
                traiter_mots(&instantane_en_lecture(), 0, &[cmd]),
                Outcome::Reject(format!("ACK [2@0] {{{cmd}}} too few arguments"))
            );
        }
    }

    #[test]
    fn getvol_dit_le_meme_volume_que_status() {
        // Deux volumes qui se contrediraient seraient un défaut invisible
        // jusqu'au jour où un client read les deux — sourdine comprise.
        for inst in [instantane_en_lecture(), instantane_muet(65)] {
            let du_status = traiter_ok(&inst, &["status"])[0].clone();
            assert_eq!(traiter_ok(&inst, &["getvol"]), vec![du_status]);
        }
    }

    #[test]
    fn binarylimit_est_bornee_des_deux_cotes_plutot_que_refusee() {
        // La bounded haute est une décision à nous et non une règle du protocol :
        // refuser un client qui demande plus que ce qu'on veut serve le
        // ferait échouer à la connection, alors qu'une chunk plus petite que
        // demandée est toujours licite.
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["binarylimit", "16384"]),
            Outcome::BinaryLimit(16 * 1024)
        );
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["binarylimit", "1048576"]),
            Outcome::BinaryLimit(MAX_CHUNK_CAP)
        );
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["binarylimit", "1"]),
            Outcome::BinaryLimit(MIN_CHUNK)
        );
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["binarylimit", "beaucoup"]),
            Outcome::Reject("ACK [2@0] {binarylimit} integer expected".to_string())
        );
    }

    #[test]
    fn une_ligne_vide_est_refusee_sans_paniquer() {
        // La session ne devrait pas en soumettre, mais une panique ici
        // couperait la connection d'un client pour une line blanche.
        assert_eq!(
            handle(&instantane_arrete(), 0, &[], MAX_CHUNK),
            Outcome::Reject("ACK [5@0] {} unsupported".to_string())
        );
    }

    // ------------------------------------------------------------------
    // Les pochettes
    // ------------------------------------------------------------------

    /// Le `href` publié par la trame d'état, celui que la trame de cover
    /// doit porter aussi.
    const HREF: &str = "/api/cover/1a2b3c";

    /// L'URI que notre `currentsong` publie pour l'instantané ci-dessous : la
    /// radio plays sa deuxième présélection.
    const URI_COURANTE: &str = "ritornello://radio/2";

    /// Une size qui n'est **pas** un multiple de `MAX_CHUNK` : trois
    /// tranches, dont la dernière est plus courte. Une size ronde laisserait
    /// passer une implémentation qui rend toujours `MAX_CHUNK` bytes.
    const TAILLE: usize = MAX_CHUNK * 2 + 1234;

    /// Un instantané où une cover est arrivée, **cohérente avec l'état**.
    ///
    /// C'est la seule forme que le producteur peut émettre, et c'est le point :
    /// le cœur envoie la trame d'état (qui porte `cover_href`) *puis* les
    /// bytes sous le même `href`. Un instantané dont la cover et l'état ne
    /// s'accorderaient pas existe aussi — c'est la fenêtre entre les deux
    /// trames — mais c'est un autre cas, testé à part.
    fn instantane_avec_pochette(size: usize) -> Snapshot {
        let mut inst = instantane_en_lecture();
        inst.state.track.cover_href = Some(HREF.to_string());
        inst.state.track.cover_origin = Some("files".to_string());
        inst.cover = Some(crate::state::test_cover(HREF, size).into());
        inst
    }

    /// La charge binaire d'une réponse, ou une panique nommant ce qu'on a eu à
    /// la place.
    fn octets_de(inst: &Snapshot, mots: &[&str]) -> Binary {
        match traiter_mots(inst, 0, mots) {
            Outcome::Bytes(b) => b,
            autre => panic!("attendu Bytes pour {mots:?}, obtenu {autre:?}"),
        }
    }

    #[test]
    fn albumart_annonce_la_taille_totale_et_rend_la_premiere_tranche() {
        let inst = instantane_avec_pochette(TAILLE);
        let b = octets_de(&inst, &["albumart", URI_COURANTE, "0"]);
        // `size:` est la size de l'**image entière**, pas de la chunk :
        // c'est elle qui dit au client combien d'allers-retours il lui remainder.
        assert_eq!(b.header, vec![format!("size: {TAILLE}")]);
        assert_eq!(b.chunk, 0..MAX_CHUNK);
        assert_eq!(b.image.len(), TAILLE);
    }

    #[test]
    fn readpicture_ajoute_le_type_mime_et_sert_les_memes_octets() {
        // Les deux names, une seule image : cet appareil n'a qu'une cover par
        // piste, quelle que soit son origine. M.A.L.P. essaie l'un puis
        // l'autre, donc les deux doivent aboutir — et au même endroit.
        let inst = instantane_avec_pochette(TAILLE);
        let art = octets_de(&inst, &["albumart", URI_COURANTE, "0"]);
        let pic = octets_de(&inst, &["readpicture", URI_COURANTE, "0"]);
        assert_eq!(pic.header, vec![format!("size: {TAILLE}"), "type: image/jpeg".to_string()]);
        assert_eq!(pic.chunk, art.chunk);
        assert_eq!(pic.image, art.image);
    }

    #[test]
    fn les_tranches_se_suivent_et_la_derniere_est_plus_courte() {
        // La propriété de découpage vue du module pur : les intervalles
        // couvrent l'image **exactement une fois**, sans trou ni recouvrement.
        // C'est ce qui rend un réassemblage juste possible, et le test de
        // session le vérifie ensuite sur une vraie chaussette.
        let inst = instantane_avec_pochette(TAILLE);
        let mut attendu = 0usize;
        let mut tailles = Vec::new();
        while attendu < TAILLE {
            let b = octets_de(&inst, &["albumart", URI_COURANTE, &attendu.to_string()]);
            assert_eq!(b.chunk.start, attendu, "la chunk doit commencer a l'offset demande");
            tailles.push(b.chunk.len());
            attendu = b.chunk.end;
        }
        assert_eq!(attendu, TAILLE, "les tranches doivent couvrir l'image entiere");
        assert_eq!(tailles, vec![MAX_CHUNK, MAX_CHUNK, 1234]);
    }

    #[test]
    fn un_offset_egal_a_la_taille_rend_une_tranche_vide_et_non_un_refus() {
        // Le comportement de MPD, et la raison est chez le client : une boucle
        // qui ferme par une requête de trop ne doit pas se voir refuser ce
        // qu'elle a déjà. La réponse est bien formée, simplement clear.
        let inst = instantane_avec_pochette(TAILLE);
        let b = octets_de(&inst, &["albumart", URI_COURANTE, &TAILLE.to_string()]);
        assert_eq!(b.header, vec![format!("size: {TAILLE}")]);
        assert!(b.chunk.is_empty(), "{:?}", b.chunk);
    }

    #[test]
    fn un_offset_au_dela_de_la_taille_est_un_defaut_dargument() {
        let inst = instantane_avec_pochette(TAILLE);
        let trop = (TAILLE + 1).to_string();
        for name in ["albumart", "readpicture"] {
            assert_eq!(
                traiter_mots(&inst, 4, &[name, URI_COURANTE, &trop]),
                Outcome::Reject(format!("ACK [2@4] {{{name}}} Offset too large")),
                "{name} devrait refuser un offset hors image"
            );
        }
    }

    #[test]
    fn sans_pochette_les_deux_commandes_refusent_de_la_meme_facon() {
        // Le cas **ordinaire** et non l'exception : la plupart des stream n'ont
        // aucune image. Un `ACK 50` est ce que MPD répond quand il n'y a pas
        // d'art, et c'est ce qui fait basculer un client vers l'autre name
        // plutôt que de l'immobiliser — une réponse clear couronnée de succès
        // ferait conclure « pas d'image » à un client qui n'essaie que
        // `readpicture`.
        let inst = instantane_en_lecture();
        assert!(inst.cover.is_none(), "la fixe de base n'a pas de cover");
        for name in ["albumart", "readpicture"] {
            assert_eq!(
                traiter_mots(&inst, 0, &[name, URI_COURANTE, "0"]),
                Outcome::Reject(format!("ACK [50@0] {{{name}}} No file exists"))
            );
        }
    }

    #[test]
    fn une_uri_qui_nest_pas_ce_qui_joue_est_refusee() {
        // La décision de conception de ce bras. Servir l'image courante sous
        // une URI périmée empoisonnerait durablement le cache d'un client, qui
        // range les pochettes **par URI** : la station 3 montrerait l'image de
        // la 2 jusqu'à son prochain démarrage. Le refus, lui, se répare au
        // réveil suivant.
        let inst = instantane_avec_pochette(TAILLE);
        for demandee in [
            // Une autre présélection de la même source.
            "ritornello://radio/3",
            // La même présélection d'une autre source.
            "ritornello://cd/2",
            // Ce que demanderait un client qui parle à un vrai MPD.
            "Musique/album/piste.flac",
            "",
        ] {
            assert_eq!(
                traiter_mots(&inst, 0, &["albumart", demandee, "0"]),
                Outcome::Reject("ACK [50@0] {albumart} No file exists".to_string()),
                "{demandee} servie a tort"
            );
        }
        // Et l'URI courante, elle, est bien servie : sans cette moitié, le test
        // passerait avec une implémentation qui refuse tout.
        assert!(matches!(
            traiter_mots(&inst, 0, &["albumart", URI_COURANTE, "0"]),
            Outcome::Bytes(_)
        ));
    }

    #[test]
    fn une_pochette_qui_ne_decrit_plus_letat_courant_est_refusee() {
        // **La fenêtre entre les deux trames.** Le cœur envoie l'état d'abord
        // et la cover ensuite : il existe donc un instant où l'état désigne
        // la piste suivante et où la cover tenue est celle de la
        // précédente. Sans ce contrôle, `albumart` servirait l'ancienne image
        // **sous la nouvelle URI** — précisément le cas qui empoisonne le
        // cache du client, atteint sans que personne n'ait mal agi.
        let mut inst = instantane_avec_pochette(TAILLE);
        inst.state.track.cover_href = Some("/api/cover/suivante".to_string());

        assert_eq!(
            traiter_mots(&inst, 0, &["albumart", URI_COURANTE, "0"]),
            Outcome::Reject("ACK [50@0] {albumart} No file exists".to_string())
        );
    }

    #[test]
    fn sans_preselection_courante_aucune_uri_ne_designe_rien() {
        // `currentsong` ne publie pas de `file:` dans cet état, donc aucun
        // client ne peut avoir d'URI légitime à demander.
        let mut inst = instantane_avec_pochette(TAILLE);
        inst.state.preset = None;
        assert_eq!(
            traiter_mots(&inst, 0, &["albumart", URI_COURANTE, "0"]),
            Outcome::Reject("ACK [50@0] {albumart} No file exists".to_string())
        );
    }

    #[test]
    fn les_deux_commandes_exigent_une_uri_et_un_offset() {
        let inst = instantane_avec_pochette(TAILLE);
        for name in ["albumart", "readpicture"] {
            for mots in [vec![name], vec![name, URI_COURANTE], vec![name, URI_COURANTE, "0", "0"]] {
                assert_eq!(
                    traiter_mots(&inst, 1, &mots),
                    Outcome::Reject(format!("ACK [2@1] {{{name}}} wrong number of arguments")),
                    "{mots:?} acceptee a tort"
                );
            }
            // Un offset non numérique est un autre défaut, et il se nomme
            // autrement : le client saura lequel de ses deux arguments revoir.
            for offset in ["abc", "-1", "1.5", ""] {
                assert_eq!(
                    traiter_mots(&inst, 1, &[name, URI_COURANTE, offset]),
                    Outcome::Reject(format!("ACK [2@1] {{{name}}} integer expected")),
                    "offset {offset:?} accepte a tort"
                );
            }
        }
    }

    #[test]
    fn les_deux_noms_sont_annonces_par_commands() {
        // Les deux moitiés de l'honnêteté de `commands`, sur ces deux names
        // précis : ils sont dans la liste, et la liste est ce que la réponse
        // publie. `chaque_commande_annoncee_est_reellement_geree` ferme le
        // couple en vérifiant qu'aucun des deux ne retombe dans le refus par
        // défaut.
        let lines = traiter_ok(&instantane_avec_pochette(TAILLE), &["commands"]);
        for name in ["albumart", "readpicture"] {
            assert!(COMMANDS.contains(&name), "{name} absente de COMMANDS");
            assert!(lines.contains(&format!("command: {name}")), "{name} non annoncee");
        }
    }
}
