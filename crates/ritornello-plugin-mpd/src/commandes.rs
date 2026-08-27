//! Ce qu'une commande MPD devient : un instantané en entrée, des lignes en
//! sortie. **Aucune E/S, aucune horloge.**
//!
//! Cette pureté est le point du module et non une élégance : la table de
//! correspondance entre une commande MPD et la façade de l'appareil est ce
//! qu'un client voit en premier, et c'est aussi ce qui se vérifie le plus mal
//! à l'œil. Une fonction qui ne fait que choisir se teste ligne par ligne ;
//! la session (Task 8) garde pour elle tout ce qui touche la chaussette.
//!
//! Son appelant est `session.rs`, qui lit les lignes et écrit les réponses :
//! c'est lui, et lui seul, qui appelle `traiter`.

use crate::etat::{Instantane, Sujet};
use crate::protocole::{ack, ligne, Ack};
use ritornello_proto::{Command, Playback, Preset, SourceCatalogue};
use std::ops::Range;
use std::sync::Arc;

/// Ce que le traitement d'une commande demande à la session de faire.
///
/// La décision est **pure** et l'application impure : ce module choisit, la
/// session écrit sur la chaussette et pousse sur le canal. C'est ce qui rend la
/// table de correspondance vérifiable au test unitaire.
#[derive(Debug, PartialEq)]
pub enum Issue {
    /// Ces lignes, puis `OK` — que la session pose, pas nous : dans une liste
    /// de commandes, un seul `OK` clôt l'ensemble.
    Repondre { lignes: Vec<String>, cmds: Vec<Command> },
    /// `ACK` déjà mis en forme. Dans une liste, elle interrompt la suite.
    Refuser(String),
    /// `idle` : attendre l'un de ces sujets.
    ///
    /// **La liste peut etre vide**, et cela ne veut pas dire « repondre tout de
    /// suite » : un client qui n'a nomme que des sous-systemes que ce greffon
    /// n'emet jamais (`idle database`) doit attendre pour toujours. C'est le
    /// comportement MPD correct — il a demande a etre prevenu d'un changement
    /// qui n'arrive jamais. La Task 8 ne doit donc pas traiter le vide comme un
    /// `OK`.
    Attendre(Vec<Sujet>),
    /// `noidle` reçu hors attente : `OK` sec.
    Annuler,
    /// Une réponse **binaire** : `albumart` et `readpicture`.
    ///
    /// Une issue à part, et non des `lignes` : les octets d'une image ne sont
    /// pas de l'UTF-8, donc ils ne peuvent pas voyager dans le `Vec<String>`
    /// de `Repondre` — et surtout ils ne doivent pas traverser l'accumulateur
    /// de texte de la session, qui est ce qui a été trouvé amplificateur d'un
    /// facteur 2048 sur ce même port. Voir `Binaire`.
    Octets(Binaire),
    /// `close` : `OK` puis fermeture.
    Fermer,
}

/// Une réponse binaire toute décidée : l'en-tête textuel, l'image, et la
/// fenêtre de cette réponse dans l'image.
///
/// **L'image est partagée, la tranche est un intervalle** : ce module reste
/// pur (aucune E/S, aucune allocation d'image), la session n'a plus qu'à
/// écrire. Le clone de l'`Arc` est un incrément de compteur, donc composer
/// cette issue ne recopie **jamais** les octets, même pour une image de
/// 20 Mio (`COVER_MAX_BYTES`) ; ce que la session écrira est borné par
/// `MAX_TRANCHE` et par lui seul. En revanche ce clone **retient** cette
/// génération d'image jusqu'à la fin de l'écriture : voir le produit calculé
/// sur `MAX_TRANCHE`.
#[derive(Clone, PartialEq)]
pub struct Binaire {
    /// `size: <total>`, et pour `readpicture` `type: <mime>` — dans cet ordre,
    /// celui de MPD.
    pub entete: Vec<String>,
    /// L'image **entière**, partagée avec l'état (jamais copiée).
    pub image: Arc<Vec<u8>>,
    /// La fenêtre à écrire. Toujours dans les bornes de `image` et d'au plus
    /// `MAX_TRANCHE` octets : c'est `albumart` qui l'établit, et la session
    /// s'y fie pour indexer sans vérifier.
    pub tranche: Range<usize>,
}

/// `Debug` écrit à la main, pour la même raison que celui de `Pochette` : le
/// dérivé imprimerait vingt mébioctets d'image dans le message d'un test raté.
impl std::fmt::Debug for Binaire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binaire")
            .field("entete", &self.entete)
            .field("image", &format_args!("{} o", self.image.len()))
            .field("tranche", &self.tranche)
            .finish()
    }
}

impl Issue {
    /// `OK` sec.
    pub fn ok() -> Self {
        Issue::Repondre { lignes: Vec::new(), cmds: Vec::new() }
    }

    pub fn lignes(lignes: Vec<String>) -> Self {
        Issue::Repondre { lignes, cmds: Vec::new() }
    }

    /// `OK` sec, plus une commande à émettre vers le cœur.
    ///
    /// Sans appelant avant la **Task 7** : aucune commande de lecture seule
    /// n'agit sur l'appareil, et c'est une propriété qu'un test de ce module
    /// vérifie explicitement.
    pub fn agir(cmd: Command) -> Self {
        Issue::Repondre { lignes: Vec::new(), cmds: vec![cmd] }
    }
}

/// Les commandes que ce serveur gère réellement, et rien d'autre.
///
/// **C'est la commande `commands` qui rend le greffon honnête** : un client
/// correct y lit ce qui existe et grise le reste de lui-même. La différence
/// entre « des onglets vides » et « des onglets qui plantent » tient à cette
/// liste, donc elle ne doit jamais promettre plus que le `match` de `traiter`.
/// Un test parcourt la liste et vérifie que chaque nom y est réellement traité.
///
/// Ordre alphabétique : les clients n'en tirent rien, mais un trou se voit.
pub const COMMANDES: &[&str] = &[
    "albumart",
    "close",
    "commands",
    "currentsong",
    "decoders",
    "idle",
    "listplaylistinfo",
    "listplaylists",
    "load",
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

/// Une entrée de la file d'attente : son indice de présélection (**creux**,
/// base 1, celui que `Command::Select` attend) et son nom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entree {
    pub index: u8,
    pub nom: String,
}

/// Les entrées d'une liste de présélections nommées, telle que le catalogue la
/// donne. Les indices sont recopiés **tels quels**, y compris creux : rien ici
/// ne dérive un rang d'un indice.
fn entrees_nommees(presets: &[Preset]) -> Vec<Entree> {
    presets.iter().map(|p| Entree { index: p.index, nom: p.name.clone() }).collect()
}

/// La file d'attente MPD : les présélections de la source active.
///
/// **Deux branches, et l'ordre entre elles est le sujet.**
/// 1. La **vraie liste**, quand le catalogue en donne une non vide pour la
///    source active. Ses indices sont ceux de la source, éventuellement
///    **creux** : `preset_count` est le *maximum* des numéros et non leur
///    nombre, donc des stations 1, 5 et 99 sont légales, là où les positions
///    MPD restent denses. La correspondance passe donc par le **rang** dans
///    cette liste (`position_vers_index`), jamais par une soustraction de 1.
/// 2. La **synthèse**, à défaut : le greffon fabrique `1..=preset_count`, et la
///    suite est alors dense par construction (`Pos = Id - 1`). C'est le cas du
///    cd et des fichiers, qui ne savent pas énumérer — leur entrée de catalogue
///    porte une liste vide, ce qui veut dire « je n'ai que des numéros » et non
///    « je n'ai rien ». Retomber sur `preset_count` est alors la seule façon de
///    voir les douze pistes d'un disque.
///
/// `None` devient **zéro entrée** et non les neuf de la grille historique de
/// l'IHM : cette grille est un pavé numérique, pas une liste. Annoncer neuf
/// entrées ferait demander à un client neuf choses dont aucune n'existe.
pub fn file_attente(inst: &Instantane) -> Vec<Entree> {
    let presets = inst.presets_actifs();
    if !presets.is_empty() {
        return entrees_nommees(presets);
    }
    let n = inst.etat.preset_count.unwrap_or(0);
    (1..=n).map(|i| Entree { index: i, nom: i.to_string() }).collect()
}

/// La date que MPD attend sur chaque entrée de `listplaylists`, faute d'en
/// avoir une.
///
/// Aucune date n'existe côté appareil : une source n'est pas un fichier, elle
/// n'a ni date de modification ni rien qui y ressemble, et en fabriquer une
/// depuis l'horloge courante ferait croire à un client qu'une liste vient de
/// changer chaque fois qu'il la relit. Une constante, donc — et l'époque plutôt
/// qu'une date arbitraire, parce qu'elle se lit comme « inconnue ».
///
/// **Émise et non omise** : le champ est facultatif dans la documentation du
/// protocole, mais des clients le lisent sans le garder (libmpdclient trie ses
/// listes dessus), et son absence les fait trébucher. Le rendre coûte une ligne
/// et ne mentira jamais, puisqu'il ne bougera jamais.
const DATE_INCONNUE: &str = "1970-01-01T00:00:00Z";

/// Traite une commande déjà découpée. `indice` est son rang dans une liste de
/// commandes (0 hors liste) : il doit traverser jusqu'à l'`ACK`, sinon un
/// client ne sait pas laquelle de ses commandes a échoué.
pub fn traiter(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    // Ligne vide : la session ne devrait pas en soumettre, mais cette fonction
    // est totale par construction plutôt que par convention — un `args[0]` sur
    // une tranche vide serait une panique, donc une connexion coupée.
    let Some(cmd) = args.first() else {
        return Issue::Refuser(ack(Ack::Unknown, indice, "", "unsupported"));
    };
    let reste = &args[1..];
    match cmd.as_str() {
        "status" => Issue::lignes(status(inst)),
        "currentsong" => Issue::lignes(currentsong(inst)),
        "playlistinfo" => playlistinfo(inst, indice, reste),
        "plchanges" => plchanges(inst, indice, reste),
        // Chaque source du catalogue **est** une liste enregistrée : c'est la
        // correspondance qui rend l'appareil lisible depuis un client MPD, où
        // « charger la liste radio » veut dire « écouter la radio ».
        "listplaylists" => Issue::lignes(listplaylists(inst)),
        // Interrogeable sur n'importe quelle source, y compris une qui ne joue
        // pas : c'est un fait sur une source, pas sur ce qui joue.
        "listplaylistinfo" => listplaylistinfo(inst, indice, reste),
        "commands" => Issue::lignes(COMMANDES.iter().map(|c| ligne("command", c)).collect()),
        // Son pendant, que de vieux clients demandent juste apres `commands`.
        // Vide, et c'est la reponse honnete : `notcommands` liste ce que le mot
        // de passe courant *interdit*, or il n'y a pas de mot de passe ici (voir
        // la spec, § Reseau), donc rien n'est interdit par permission. Ce qui
        // n'existe pas est simplement absent de `commands`.
        "notcommands" => Issue::ok(),
        // Les trois seules étiquettes que `Morceau` porte. En annoncer d'autres
        // ferait chercher au client des tris que rien n'alimente.
        "tagtypes" => {
            Issue::lignes(["Artist", "Album", "Title"].iter().map(|t| ligne("tagtype", t)).collect())
        }
        // Une sortie unique et toujours active : l'appareil a une sortie audio,
        // que la page d'admin choisit. `enableoutput`/`disableoutput` sont
        // refusées, donc rien ici n'est pilotable — mais un client qui ne voit
        // aucune sortie affiche « muet » et n'insiste pas.
        "outputs" => Issue::lignes(vec![
            ligne("outputid", 0),
            ligne("outputname", "default"),
            ligne("outputenabled", 1),
        ]),
        "stats" => Issue::lignes(stats(inst)),
        // `OK` sec, mais **présentes** : une commande inconnue au montage peut
        // faire renoncer un client avant qu'il n'affiche un écran. Aucune
        // valeur à donner (il n'y a ni greffon de décodage ni schéma d'URL à
        // exposer), et une liste vide est une réponse bien formée.
        "decoders" | "urlhandlers" => Issue::ok(),
        "ping" => Issue::ok(),
        // Acceptée sans rien vérifier : le serveur n'a pas de mot de passe (voir
        // la spec, § Réseau), et un client configuré avec un mot de passe ne
        // doit pas être rejeté pour autant. Sans argument non plus : il n'y a
        // rien à contrôler, donc rien à refuser.
        "password" => Issue::ok(),
        "close" => Issue::Fermer,
        // `idle` ne *répond* pas ici : ce module choisit les sujets, la session
        // (Task 8) tient l'attente et décide qu'un `idle` dans une liste de
        // commandes est illégal. Le découpage des noms de sujets, lui, est pur.
        "idle" => idle(indice, reste),
        "noidle" => Issue::Annuler,
        // `play [POS]` : POS est le **rang** dans la file (base 0, celui que
        // `Pos` publie), jamais l'indice de présélection moins un — les deux
        // ne coïncident plus dès qu'une source énumère une liste creuse.
        // Sans argument, ce n'est pas une sélection mais la touche
        // Lecture : on relance ce qui était chargé.
        "play" => play(inst, indice, reste),
        // `playid <ID>` : l'indice tel quel, mais vérifié dans la file — un
        // `ID` à l'intérieur du maximum (`preset_count`) sans être une entrée
        // réelle de la file creuse doit refuser ; une borne ne suffit pas.
        "playid" => playid(inst, indice, reste),
        // Bascule sans argument ; sinon n'agit que si l'état optimiste diffère
        // de la cible — c'est ce qui ferme la course d'un client qui
        // renverrait la même commande deux fois. Voir `pause`.
        "pause" => pause(inst, indice, reste),
        "stop" => Issue::agir(Command::Stop),
        // La distinction présélection/piste n'est pas d'ici : c'est la source
        // active qui l'interprète (voir la doc de `Command::Next`).
        "next" => Issue::agir(Command::Next),
        "previous" => Issue::agir(Command::Prev),
        "setvol" => setvol(inst, indice, reste),
        // Dépréciée par MPD mais encore émise par de vieux clients : relative
        // au volume courant, et bornée ici (voir `volume`) plutôt que de
        // laisser déborder `Command::SetVolume`, qui lui est absolu.
        "volume" => volume(inst, indice, reste),
        // `seek`/`seekid` ignorent leur premier argument (position ou id) :
        // `SeekTo` ne sait pas changer de piste en même temps, et MPD n'envoie
        // ce genre de commande que sur ce qui joue déjà.
        "seek" => seek(indice, "seek", reste),
        "seekid" => seek(indice, "seekid", reste),
        // Seule forme qui accepte un relatif (`+n`/`-n`), résolu ici depuis
        // `position_s` puisque `Command::SeekTo` ne porte qu'un absolu.
        "seekcur" => seekcur(inst, indice, reste),
        // **Les deux noms répondent exactement la même chose, et ce n'est pas
        // un raccourci.** Pour MPD ce sont deux origines différentes :
        // `albumart` cherche un fichier *à côté* de la piste (un `cover.jpg`
        // dans son dossier), `readpicture` une image *embarquée* dans ses
        // étiquettes. Cet appareil, lui, n'a qu'**une** pochette par piste,
        // quelle que soit son origine : le cœur l'a déjà arbitrée entre le
        // fichier voisin, l'étiquette embarquée et le réseau, et n'en publie
        // qu'une. Distinguer ici demanderait au greffon une information que le
        // protocole d'affichage ne porte pas — et surtout, M.A.L.P. essaie l'un
        // puis l'autre : répondre à un seul des deux ferait dépendre l'affichage
        // de la pochette de l'ordre dans lequel le client s'y prend.
        // Seule différence, celle de MPD : `readpicture` publie un `type:`.
        "albumart" => pochette(inst, indice, "albumart", reste),
        "readpicture" => pochette(inst, indice, "readpicture", reste),
        // `load <nom>` bascule de source. Elle n'ajoute pas à la file (MPD y
        // *concatène* une liste enregistrée) : ici la file d'attente **est**
        // la liste de la source active, donc la charger, c'est la choisir.
        // Le refus n'est plus fixe : le catalogue dit quels noms existent.
        "load" => load(inst, indice, reste),
        // Tout le reste est refusé du même refus, sans distinguer l'inconnu du
        // volontairement non géré — MPD ne les distingue pas non plus, et
        // `commands` dit déjà ce qui existe. Deux de ces refus méritent leur
        // raison écrite : `update` n'a aucun sens (il n'y a pas de base de
        // données à indexer), et `kill` est refusée et non ignorée, parce
        // qu'éteindre l'appareil depuis le réseau sans authentification serait
        // une capacité qu'aucune télécommande de la pièce n'a.
        _ => Issue::Refuser(ack(Ack::Unknown, indice, cmd, "unsupported")),
    }
}

/// Le mot que MPD attend pour `state`.
fn etat_mpd(playback: Playback) -> &'static str {
    match playback {
        Playback::Playing => "play",
        Playback::Paused => "pause",
        Playback::Stopped => "stop",
    }
}

/// Des secondes au format décimal de MPD (`12.000`).
fn secondes(s: u32) -> String {
    format!("{:.3}", f64::from(s))
}

/// Où en est la lecture dans la file d'attente : la **position dense** et
/// l'**indice creux**, ou rien.
///
/// Rien du tout si la présélection courante n'est pas dans la file : mieux
/// vaut un `status` muet sur ce point qu'un `song` désignant une position que
/// le client ne trouvera pas dans le `playlistinfo` qu'il vient de lire.
/// Un seul endroit pour les deux réponses qui en ont besoin (`status` et
/// `currentsong`), sinon elles finiraient par se contredire.
fn courant(inst: &Instantane, file: &[Entree]) -> Option<(usize, u8)> {
    let preset = inst.etat.preset?;
    let position = file.iter().position(|e| e.index == preset)?;
    Some((position, preset))
}

/// L'URI d'une entrée. Un schéma à nous : le greffon ne sert aucun octet, et
/// un client n'a besoin que d'une clé stable pour distinguer deux entrées.
fn uri(source: &str, index: u8) -> String {
    format!("ritornello://{source}/{index}")
}

/// Taille d'une tranche de réponse binaire, en octets.
///
/// **8 Kio, la valeur par défaut de MPD lui-même** (`binarylimit`), et le
/// chiffre n'est pas repris par imitation : c'est le plafond qu'un client qui
/// n'envoie pas de `binarylimit` — donc tous ceux que ce greffon peut servir,
/// puisqu'il ne gère pas cette commande — s'attend à ne jamais voir dépassé.
/// Servir 64 Kio à un client dimensionné pour 8 serait un dépassement de
/// tampon chez lui, provoqué par nous.
///
/// **Confronté à `MAX_REPONSE` (1 Mio), le plafond du chemin texte.** Les deux
/// bornent la même chose — les octets qu'une requête fait écrire — mais elles
/// n'ont ni la même valeur ni le même rôle, et l'écart de 128 est délibéré :
///
/// * `MAX_REPONSE` doit être large parce qu'il borne une réponse **composée**,
///   dont la taille est décidée par ce que le client a demandé (une liste de
///   soixante `playlistinfo`) et non par nous. C'est un plafond de dernier
///   recours, atteint par accumulation.
/// * `MAX_TRANCHE` borne une réponse dont **nous** choisissons la taille : le
///   client ne demande pas « toute l'image », il demande « à partir d'ici », et
///   c'est le serveur qui décide combien il en donne. Rien n'oblige donc à
///   laisser une seule requête écrire un mébioctet, et une image de 2 Mio — un
///   dixième du plafond, voir juste en dessous — se sert en 256 allers-retours
///   dont chacun coûte 8 Kio de tampon transitoire au lieu d'un seul
///   aller-retour qui en coûterait 2048.
///
/// **Le compte d'allers-retours, et pourquoi 8 Kio reste le bon choix malgré
/// lui.** `COVER_MAX_BYTES` vaut 20 Mio, donc le plafond de pochette se sert en
/// ~2560 allers-retours, chacun payant un aller-retour réseau complet (le
/// client ne peut pas les grouper : l'offset de chaque requête dépend du `size:`
/// que la précédente a rendu, et une liste de commandes est envoyée entière
/// avant d'être lue). Sur un Wi-Fi domestique à 20 ms d'aller-retour, cela fait
/// une minute pour une image. Le chiffre est vrai et il est mauvais ; il ne
/// justifie pourtant pas de lever ce plafond :
///
/// 1. **Il décrit le plafond, pas le trafic.** Une pochette réelle pèse 75 Kio
///    (mesure du Cover Art Archive) à quelques centaines de kibioctets pour une
///    étiquette embarquée : 10 à 50 allers-retours, une fraction de seconde.
///    Les 20 Mio sont la borne de refus du protocole d'affichage, pas une
///    taille que le cœur produit.
/// 2. **8 Kio n'est pas un choix, c'est le contrat.** C'est la valeur par
///    défaut de `binarylimit` chez MPD, donc ce qu'un client qui ne l'a pas
///    relevée s'attend à ne jamais voir dépassé — et ce greffon ne gère pas
///    `binarylimit`, donc **aucun** de ses clients ne peut l'avoir relevée.
///    Servir 64 Kio à un client dimensionné pour 8 est un dépassement de
///    tampon chez lui, provoqué par nous, en échange de quelques dizaines de
///    millisecondes.
/// 3. **Le levier existe et il est du bon côté** : implémenter `binarylimit`
///    laisserait le client demander des tranches plus grandes, ce qui est
///    exactement la façon dont MPD résout ce compromis. C'est un ajout de
///    fonction, pas une correction ; ce qu'il ne faut pas faire, c'est relever
///    `MAX_TRANCHE` unilatéralement.
///
/// Conséquence, et c'est le point : le chemin binaire **ne passe pas** par
/// l'accumulateur de texte et n'a donc aucun facteur d'amplification à lui.
/// Le pire cas *transitoire* d'une connexion qui ne fait que des `albumart` est
/// `MAX_TRANCHE` + l'en-tête ≈ 8,3 Kio de tampon, contre les ≈ 2,3 Mio que le
/// chemin texte autorise (voir `MAX_REPONSE`) — soit trois millièmes.
///
/// **Ce qui n'est pas borné par connexion, et il faut l'écrire en produit** —
/// c'est la troisième fois qu'une borne de ce fichier est documentée trop
/// favorablement, donc voici le chiffre et non une nuance. L'image vit une
/// seule fois dans le processus **par génération**, pas une seule fois tout
/// court : `executer` tient son clone d'`Instantane` et la réponse binaire tient
/// son propre clone de l'`Arc`, tous deux pendant le `write_all`. Un client qui
/// demande une tranche puis cesse de lire épingle donc sa génération pour aussi
/// longtemps qu'il le veut, et une pochette poussée entre-temps en crée une
/// autre qu'une deuxième session peut épingler à son tour. Le pire cas est
/// `MAX_SESSIONS × COVER_MAX_BYTES` = 16 × 20 Mio = **320 Mio**, plus la
/// génération que l'état tient lui-même, soit **340 Mio** sur un appareil d'un
/// gibioctet partagé avec mpv.
///
/// Il demande une immobilisation délibérée *et* des pochettes proches du
/// plafond : ce n'est pas un accident, c'est un client hostile — mais le modèle
/// de menace de ce port (ouvert à tout le réseau local, sans mot de passe)
/// accepte déjà cette figure, et c'est pour elle que `MAX_SESSIONS` et
/// `MAX_REPONSE` existent.
///
/// **Aucune mitigation n'est ajoutée ici, et c'est un choix argumenté.** Les
/// deux leviers réels sont hors de portée ou pires que le mal : abaisser
/// `COVER_MAX_BYTES` vit dans `ritornello-proto` et concerne tout l'appareil ;
/// mettre une échéance sur le `write_all` binaire introduirait la première
/// horloge du chemin de session, pour ne protéger que d'un client qui a déjà
/// choisi de nuire. Sérialiser les réponses binaires derrière un sémaphore
/// serait franchement nuisible : un seul client immobile priverait alors tous
/// les autres de pochette. La borne est donc **connue et écrite**, ce qui est ce
/// qui manquait.
pub const MAX_TRANCHE: usize = 8 * 1024;

/// `albumart <uri> <offset>` et `readpicture <uri> <offset>` : une tranche de
/// la pochette de ce qui joue.
///
/// **L'URI est vérifiée strictement contre ce qui joue à cet instant**, et
/// c'est la décision de conception de ce bras. Notre `currentsong` publie
/// `file: ritornello://<source>/<indice>`, donc `albumart ritornello://radio/17`
/// veut dire « la pochette de ce que la présélection 17 joue *maintenant* » —
/// une URI dont le contenu change sous elle, ce qui n'arrive jamais dans un MPD
/// ordinaire où une URI est un fichier. Deux réponses étaient défendables :
///
/// * **Servir quand même** (ignorer l'URI). Le client obtient toujours une
///   image, mais **la mauvaise** dès que sa demande est en retard d'une piste,
///   et le dégât est durable : les clients mettent la pochette en cache **sous
///   l'URI demandée** (M.A.L.P. le fait), donc répondre l'image de la station
///   17 à une demande pour la station 3 empoisonne ce cache — la station 3
///   montrera une image fausse tant que le client n'est pas relancé, et rien
///   ne viendra jamais l'invalider.
/// * **Refuser** (retenu). Le refus est transitoire et se répare tout seul :
///   le client redemande au réveil de `player` suivant, qu'un changement de
///   pochette provoque justement (voir `appliquer_pochette`). Et la rigueur ne
///   coûte rien de légitime — un client demande l'image de ce qu'il vient de
///   lire dans `currentsong`, c'est-à-dire l'URI courante.
///
/// La même exigence porte sur le `href` : la pochette tenue doit être celle que
/// la trame d'état courante annonce. Sans ce second contrôle, la fenêtre entre
/// l'état (envoyé d'abord) et la pochette (envoyée ensuite) ferait servir
/// l'image de la piste précédente **sous l'URI de la nouvelle** — le cas
/// empoisonnant décrit ci-dessus, atteint sans qu'aucun client n'ait rien fait
/// de travers.
fn pochette(inst: &Instantane, indice: usize, nom: &str, reste: &[String]) -> Issue {
    let [demandee, offset] = reste else {
        return Issue::Refuser(ack(Ack::Arg, indice, nom, "wrong number of arguments"));
    };
    let Ok(offset) = offset.parse::<usize>() else {
        return Issue::Refuser(ack(Ack::Arg, indice, nom, "integer expected"));
    };
    // Le refus « il n'y a pas d'image ici », commun aux quatre gardes qui
    // suivent : le client n'a pas à savoir *laquelle* a échoué, et le
    // distinguer lui apprendrait l'état interne du greffon sans lui donner
    // aucune conduite différente à tenir — dans les quatre cas il n'y a pas
    // d'image à cette URI, et dans les quatre cas il redemandera au réveil
    // suivant.
    let absente = || Issue::Refuser(ack(Ack::NoExist, indice, nom, "No file exists"));
    let Some(pochette) = inst.pochette.as_ref() else {
        return absente();
    };
    // Rien ne joue de numéroté : aucune URI ne peut désigner quoi que ce soit,
    // et `currentsong` n'en publie d'ailleurs aucune.
    let Some(preset) = inst.etat.preset else {
        return absente();
    };
    if *demandee != uri(&inst.etat.source, preset) {
        return absente();
    }
    if Some(pochette.href.as_str()) != inst.etat.morceau.cover_href.as_deref() {
        return absente();
    }
    let taille = pochette.octets.len();
    // `>` et non `>=`, exactement comme MPD : à `offset == taille` le client a
    // déjà tout, et la réponse bien formée est une tranche vide — la refuser
    // ferait échouer un client qui ferme sa boucle par une requête de trop.
    // Au-delà, l'offset est faux et c'est un défaut d'argument.
    if offset > taille {
        return Issue::Refuser(ack(Ack::Arg, indice, nom, "Offset too large"));
    }
    let fin = taille.min(offset + MAX_TRANCHE);
    // `size:` est la taille de **l'image entière** et non de la tranche : c'est
    // elle qui dit au client combien d'allers-retours il lui reste. Les
    // confondre ferait s'arrêter le client à la première tranche.
    let mut entete = vec![ligne("size", taille)];
    if nom == "readpicture" {
        // Le seul écart entre les deux commandes, et c'est celui de MPD :
        // `readpicture` annonce le type MIME, `albumart` non.
        entete.push(ligne("type", &pochette.mime));
    }
    Issue::Octets(Binaire { entete, image: pochette.octets.clone(), tranche: offset..fin })
}

fn status(inst: &Instantane) -> Vec<String> {
    let file = file_attente(inst);
    // `muted` écrase le volume mémorisé. MPD n'a pas de sourdine : les clients
    // coupent le son en posant `setvol 0` et s'attendent donc à relire 0 quand
    // c'est coupé. Rapporter 65 sur un appareil muet ferait afficher un curseur
    // à 65 sur un silence.
    let volume = if inst.etat.muted { 0 } else { inst.etat.volume };
    let mut lignes = vec![ligne("volume", volume)];
    // Rapportées à zéro et **pas omises** : les clients les lisent toujours, et
    // leur absence les fait mal se comporter. Les *écrire* est refusé (Task 7),
    // donc c'est le seul endroit où le greffon publie une valeur qu'il ne sait
    // pas changer — voir la spec, § Ce que le greffon ne fait pas.
    for cle in ["repeat", "random", "single", "consume"] {
        lignes.push(ligne(cle, 0));
    }
    lignes.push(ligne("playlist", inst.version_file));
    // La **longueur de la file**, pas le maximum des indices : c'est le nombre
    // d'entrées qu'un client va demander. Les deux coïncident sur une file
    // synthétisée, et **divergent** dès qu'une source énumère une liste creuse
    // — trois stations numérotées 1, 5 et 99 font `playlistlength: 3`, jamais
    // 99. Publier le maximum ferait demander à un client quatre-vingt-seize
    // entrées qui n'existent pas.
    lignes.push(ligne("playlistlength", file.len()));
    // Aucun fondu enchaîné ici, mais le champ est lu par des clients qui
    // affichent un réglage. Trois décimales comme `elapsed` et `duration`.
    lignes.push(ligne("mixrampdb", "0.000"));
    // L'état **optimiste**, jamais le brut de la trame : un client qui envoie
    // `pause` puis `status` dans la même foulée lirait sinon l'état d'avant sa
    // propre commande, et son bouton n'aurait pas bougé.
    lignes.push(ligne("state", etat_mpd(inst.playback())));
    if inst.playback() != Playback::Stopped {
        // `song`/`songid` **absents** et non à zéro : `songid: 0` désignerait
        // une entrée réelle, donc un client afficherait la mauvaise ligne en
        // surbrillance.
        if let Some((position, index)) = courant(inst, &file) {
            lignes.push(ligne("song", position));
            lignes.push(ligne("songid", index));
        }
    }
    if let Some(position_s) = inst.etat.position_s {
        // `time` est déprécié mais encore lu ; il n'apparaît que si la position
        // est connue, et un total inconnu (un direct) s'y écrit 0 — c'est ce
        // que MPD fait des flux.
        let total = inst.etat.morceau.duration_s.unwrap_or(0);
        lignes.push(ligne("time", format!("{position_s}:{total}")));
        lignes.push(ligne("elapsed", secondes(position_s)));
    }
    // Indépendante de la position : Radio France annonce la durée d'un morceau
    // sur un direct dont personne ne connaît l'avancement.
    if let Some(duree) = inst.etat.morceau.duration_s {
        lignes.push(ligne("duration", secondes(duree)));
    }
    lignes
}

fn currentsong(inst: &Instantane) -> Vec<String> {
    // Rien du tout — donc un `OK` sec — quand aucune présélection n'est
    // désignée. Gardé sur `preset` et non sur l'état de lecture : une lecture
    // en pause a toujours un morceau courant, et MPD le publie.
    let Some(preset) = inst.etat.preset else {
        return Vec::new();
    };
    let file = file_attente(inst);
    let mut lignes = vec![ligne("file", uri(&inst.etat.source, preset))];
    let morceau = &inst.etat.morceau;
    // Un champ absent de `Morceau` ne produit **pas** de ligne : `Artist: `
    // vaut pire qu'aucune ligne, un client l'affiche comme un artiste vide.
    // Le titre seul a un repli, le nom de la présélection — c'est le nom de la
    // station, la seule chose qu'on sache d'un flux sans étiquette ICY.
    if let Some(titre) = morceau.title.as_deref().or(inst.etat.preset_name.as_deref()) {
        lignes.push(ligne("Title", titre));
    }
    if let Some(artiste) = &morceau.artist {
        lignes.push(ligne("Artist", artiste));
    }
    if let Some(album) = &morceau.album {
        lignes.push(ligne("Album", album));
    }
    // `Date` est le nom du tag dans le protocole MPD, et il y est libre :
    // beaucoup de bibliothèques y mettent une année seule. On y met donc
    // l'année telle quelle, sans la maquiller en date complète qu'on n'a pas.
    if let Some(annee) = morceau.year {
        lignes.push(ligne("Date", annee));
    }
    if let Some(duree) = morceau.duration_s {
        // `Time` en entier (déprécié), `duration` en décimal : les deux, parce
        // que les clients se partagent entre les deux selon leur âge.
        lignes.push(ligne("Time", duree));
        lignes.push(ligne("duration", secondes(duree)));
    }
    if let Some((position, index)) = courant(inst, &file) {
        lignes.push(ligne("Pos", position));
        lignes.push(ligne("Id", index));
    }
    lignes
}

/// Les lignes d'une entrée de la file : sa position dense, son indice creux.
fn entree_lignes(source: &str, position: usize, entree: &Entree) -> Vec<String> {
    vec![
        ligne("file", uri(source, entree.index)),
        ligne("Title", &entree.nom),
        ligne("Pos", position),
        ligne("Id", entree.index),
    ]
}

/// Les lignes d'une tranche de la file. `Pos` reste la position **absolue**
/// dans la file et non le rang dans la tranche : c'est la clé avec laquelle le
/// client désignera l'entrée ensuite, et la décaler ferait jouer autre chose que
/// ce qu'il a touché à l'écran.
fn lignes_de_file(inst: &Instantane, file: &[Entree], plage: Range<usize>) -> Vec<String> {
    let debut = plage.start;
    file[plage]
        .iter()
        .enumerate()
        .flat_map(|(decalage, entree)| entree_lignes(&inst.etat.source, debut + decalage, entree))
        .collect()
}

/// Analyse un argument de position MPD : soit une position seule (`3`), soit une
/// plage `START:END` dont la **fin est exclue**, `START:` valant « jusqu'au
/// bout ». Rend les bornes déjà ramenées à la file, ou `None` si l'argument est
/// malformé.
///
/// La grammaire de MPD est `playlistinfo [[SONGPOS] | [START:END]]`, et un
/// client qui fenêtre sa file (M.A.L.P. le fait) demande `0:100`. Refuser une
/// requête bien formée lui ferait afficher une file vide sur les 51 stations de
/// la radio : la plage s'implémente, elle ne se déclare pas non gérée.
///
/// **Trois hors-bornes qui ne se répondent pas pareil**, et l'asymétrie est
/// celle de MPD :
/// - une **plage** qui commence après la fin rend une tranche **vide**. Un
///   client qui fenêtre peut demander `50:100` juste après que la file a
///   rétréci ; sa requête est bien formée, la réponse est « il n'y a rien
///   là-bas », pas une erreur.
/// - une **position seule** hors bornes reste un refus : elle désigne une entrée
///   précise qui n'existe pas, et un `OK` sec laisserait croire à un trou dans
///   la file.
/// - `START > END` est **malformé** : aucun client correct ne le produit, MPD le
///   refuse aussi, et l'accepter masquerait le bogue de l'appelant.
fn bornes(arg: &str, longueur: usize) -> Option<Range<usize>> {
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

fn playlistinfo(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let file = file_attente(inst);
    let Some(arg) = args.first() else {
        return Issue::lignes(lignes_de_file(inst, &file, 0..file.len()));
    };
    match bornes(arg, file.len()) {
        Some(plage) => Issue::lignes(lignes_de_file(inst, &file, plage)),
        None => Issue::Refuser(ack(Ack::Arg, indice, "playlistinfo", "bad song index")),
    }
}

fn plchanges(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let Some(version) = args.first().and_then(|a| a.parse::<u32>().ok()) else {
        return Issue::Refuser(ack(Ack::Arg, indice, "plchanges", "integer expected"));
    };
    if version == inst.version_file {
        // Rien à dire, et c'est tout l'intérêt de la commande : un client qui
        // détient la version courante n'a pas à recevoir 51 lignes. Avant
        // d'analyser la plage, donc : il n'y a rien à fenêtrer dans une réponse
        // vide.
        return Issue::ok();
    }
    // La file entière, faute de savoir ce qui a changé dedans : la file *est* la
    // liste des présélections de la source active, et un changement de source la
    // remplace en totalité. La grammaire est `plchanges VERSION [START:END]` :
    // la même fenêtre que `playlistinfo`.
    let file = file_attente(inst);
    let plage = match args.get(1) {
        None => 0..file.len(),
        Some(arg) => match bornes(arg, file.len()) {
            Some(plage) => plage,
            None => return Issue::Refuser(ack(Ack::Arg, indice, "plchanges", "bad song index")),
        },
    };
    Issue::lignes(lignes_de_file(inst, &file, plage))
}

/// `listplaylists` : une entrée par source du catalogue, **dans l'ordre reçu**
/// — celui de la bascule de `SourceCycle`, donc celui que l'utilisateur voit
/// sur sa télécommande. Ne pas trier : l'ordre porte une information.
///
/// Rien du tout avant la première trame de catalogue, et c'est la vérité de cet
/// instant : le greffon ne connaît alors aucune source. Un client relira après
/// son réveil sur `stored_playlist`.
fn listplaylists(inst: &Instantane) -> Vec<String> {
    inst.catalogue
        .sources
        .iter()
        .flat_map(|s| [ligne("playlist", &s.name), ligne("Last-Modified", DATE_INCONNUE)])
        .collect()
}

/// Les lignes d'une entrée de liste **enregistrée** : son URI et son nom, et
/// rien de plus.
///
/// **Pas de `Pos` ni d'`Id` ici**, contrairement à `entree_lignes` : ces deux
/// étiquettes désignent une entrée de la *file d'attente*, et une liste
/// enregistrée n'est pas chargée. Les émettre pour une source qui ne joue pas
/// donnerait à un client des positions qu'il ne retrouverait pas dans son
/// `playlistinfo` — c'est aussi ce que fait MPD, qui ne les publie que pour la
/// file.
fn lignes_de_liste(source: &str, entree: &Entree) -> Vec<String> {
    vec![ligne("file", uri(source, entree.index)), ligne("Title", &entree.nom)]
}

/// Le nom d'une liste enregistrée tel qu'un client l'a écrit, résolu en source
/// du catalogue. `Err` est l'`ACK 50` déjà mis en forme.
///
/// Un seul endroit pour `listplaylistinfo` et `load` : les deux doivent
/// répondre au *même* jeu de noms que `listplaylists` annonce, et les laisser
/// chercher chacune de son côté les ferait diverger un jour.
fn liste_nommee<'a>(
    inst: &'a Instantane,
    indice: usize,
    cmd: &str,
    args: &[String],
) -> Result<&'a SourceCatalogue, String> {
    let Some(nom) = args.first() else {
        return Err(ack(Ack::Arg, indice, cmd, "wrong number of arguments"));
    };
    inst.catalogue_source(nom).ok_or_else(|| {
        // `ACK 50` et non `ACK 2` : le nom est bien formé, c'est la liste qui
        // n'existe pas — la distinction est celle que MPD fait, et un client
        // qui la lit sait qu'il doit relire `listplaylists` plutôt que de
        // corriger sa syntaxe.
        ack(Ack::NoExist, indice, cmd, "no such playlist")
    })
}

fn listplaylistinfo(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let source = match liste_nommee(inst, indice, "listplaylistinfo", args) {
        Ok(source) => source,
        Err(refus) => return Issue::Refuser(refus),
    };
    // La même règle que `file_attente` là où elle peut s'appliquer, et il faut
    // qu'elle soit la même : une source qui ne sait pas énumérer (le cd) porte
    // une liste vide, et ses entrées se synthétisent depuis le compte. Mais
    // `preset_count` ne décrit que la source **active** — pour une autre, le
    // greffon ne sait rien du nombre, et une liste vide est alors la réponse
    // honnête. C'est le seul endroit du module où les deux se distinguent, et
    // il n'y a pas de meilleure réponse : le catalogue ne porte pas de compte.
    let entrees = if source.presets.is_empty() && source.name == inst.etat.source {
        file_attente(inst)
    } else {
        entrees_nommees(&source.presets)
    };
    Issue::lignes(entrees.iter().flat_map(|e| lignes_de_liste(&source.name, e)).collect())
}

/// `load <nom>` : choisir la source de ce nom.
///
/// Le greffon refuse lui-même un nom absent du catalogue plutôt que d'émettre
/// un `SelectSource` que le cœur ignorerait en silence (voir la doc de
/// `Command::SelectSource`) : il ne propose que des noms qu'il a reçus, donc
/// c'est à lui de savoir lesquels existent. Un `OK` suivi de rien serait la
/// pire réponse possible pour un client, qui attendrait un changement de file
/// qui n'arrive jamais.
fn load(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    match liste_nommee(inst, indice, "load", args) {
        // Le nom du **catalogue** et non l'argument brut : les deux sont égaux
        // par construction (`catalogue_source` compare exactement), mais
        // émettre celui que le cœur nous a donné garde le greffon incapable
        // d'inventer un nom de source.
        Ok(source) => Issue::agir(Command::SelectSource(source.name.clone())),
        Err(refus) => Issue::Refuser(refus),
    }
}

fn stats(inst: &Instantane) -> Vec<String> {
    // `uptime` à 0 **délibérément** : le rendre juste demanderait de mémoriser
    // un instant de départ, donc une horloge de plus dans un module qui n'en a
    // aucune, pour une valeur qu'aucun client d'ici n'utilise. Même raison pour
    // les durées de lecture cumulées.
    vec![
        ligne("artists", 0),
        ligne("albums", 0),
        ligne("songs", file_attente(inst).len()),
        ligne("uptime", 0),
        ligne("db_playtime", 0),
        ligne("db_update", 0),
        ligne("playtime", 0),
    ]
}

/// Ce que vaut un nom de sous-système écrit dans un `idle`.
enum NomIdle {
    /// Un des quatre que ce greffon sait faire bouger.
    Notre(Sujet),
    /// Un sous-système du vocabulaire MPD que nous n'émettrons **jamais**.
    JamaisEmis,
    /// Un mot que MPD lui-même ne connaît pas.
    Inconnu,
}

/// Le nom MPD d'un sous-système, tel qu'un client l'écrit dans son `idle`.
///
/// **Le vocabulaire entier de MPD, pas seulement le nôtre.** C'est la
/// distinction qui décide si un client démarre : tout ce qui est bâti sur
/// `mpd_send_idle_mask` de libmpdclient envoie une liste explicite — en
/// pratique `database update stored_playlist playlist player mixer output
/// options` — et un `ACK` sur son premier `idle` le fait boucler ou renoncer.
/// Refuser un mot que MPD ignore est juste ; refuser un mot **légal** est le
/// même défaut vu de l'autre côté.
///
/// Un sous-système légal que nous n'émettons jamais est donc accepté puis
/// écarté en silence, et l'attente qui en résulte peut ne jamais se terminer.
/// C'est le comportement MPD correct et non un oubli : le client a demandé
/// qu'on le prévienne si ça changeait, et ça ne change jamais.
fn nom_idle(nom: &str) -> NomIdle {
    match nom {
        "player" => NomIdle::Notre(Sujet::Player),
        "mixer" => NomIdle::Notre(Sujet::Mixer),
        "playlist" => NomIdle::Notre(Sujet::Playlist),
        "stored_playlist" => NomIdle::Notre(Sujet::StoredPlaylist),
        // Le reste du vocabulaire de MPD. Aucun n'a de déclencheur ici : il n'y
        // a pas de base de données à indexer (`database`, `update`), une seule
        // sortie qu'on ne pilote pas (`output`), aucune option modifiable
        // (`options`), ni partition, ni étiquette collée, ni abonnement, ni
        // message, ni voisinage, ni montage annoncé sur ce protocole.
        "database" | "update" | "output" | "options" | "partition" | "sticker"
        | "subscription" | "message" | "neighbor" | "mount" => NomIdle::JamaisEmis,
        _ => NomIdle::Inconnu,
    }
}

fn idle(indice: usize, args: &[String]) -> Issue {
    if args.is_empty() {
        // Sans argument, tous les sujets comptent.
        return Issue::Attendre(vec![
            Sujet::Player,
            Sujet::Mixer,
            Sujet::Playlist,
            Sujet::StoredPlaylist,
        ]);
    }
    let mut sujets = Vec::new();
    for nom in args {
        match nom_idle(nom) {
            // Dédoublonné, comme `marquer` côté état : `idle player player` ne
            // décrit qu'une seule attente.
            NomIdle::Notre(s) => {
                if !sujets.contains(&s) {
                    sujets.push(s);
                }
            }
            // Accepté puis écarté : voir `nom_idle`. La liste peut finir vide,
            // et c'est une attente qui ne se terminera jamais — la bonne
            // réponse, pas un oubli.
            NomIdle::JamaisEmis => {}
            // Un mot que MPD ne connaît pas : refusé et non ignoré, sinon un
            // client qui a mal orthographié son sous-système resterait muet
            // pour toujours, ce qui se diagnostique bien plus mal qu'un `ACK`.
            NomIdle::Inconnu => {
                return Issue::Refuser(ack(Ack::Arg, indice, "idle", "unrecognized idle event"))
            }
        }
    }
    Issue::Attendre(sujets)
}

// ----------------------------------------------------------------------
// Les commandes d'action : ce qui demande quelque chose à l'appareil.
// ----------------------------------------------------------------------

/// Traduit une position MPD (le **rang**, base 0, celui que `Pos` publie) en
/// l'indice de présélection qui s'y trouve. `None` si la position dépasse la
/// file.
///
/// Extraite en fonction pure, séparée de `play`, pour se tester aussi sur une
/// file construite à la main. Elle est le seul chemin autorisé de la position
/// vers l'indice : dès qu'une source énumère une liste creuse, « l'indice moins
/// un » n'est plus le rang, et le décalage qu'une soustraction introduirait
/// ferait jouer une station voisine de celle qu'on a touchée à l'écran.
fn position_vers_index(file: &[Entree], position: usize) -> Option<u8> {
    file.get(position).map(|e| e.index)
}

/// Vrai si cet indice de présélection existe réellement dans la file — pas
/// seulement dans les bornes de son maximum.
///
/// La distinction est sans effet sur une file synthétisée (« exister » et
/// « être ≤ au maximum » y sont la même chose) et décisive sur une file creuse,
/// où `preset_count` reste un maximum et non un compte : un `playid` sur un
/// trou de la suite doit refuser, là qu'une comparaison de borne le laisserait
/// passer à tort.
fn index_existe(file: &[Entree], index: u8) -> bool {
    file.iter().any(|e| e.index == index)
}

/// Un temps MPD absolu, en secondes tronquées. `None` si non numérique, non
/// fini ou négatif — jamais un temps négatif ramené à zéro en silence pour
/// cette forme (contrairement à la résolution du relatif de `seekcur`, où zéro
/// est la bonne réponse à un recul trop grand).
///
/// **`inf` et `nan` sont non numériques pour ce protocole**, même si
/// `f64::from_str` les accepte : `seek 0 inf` rendait `SeekTo(u32::MAX)` et
/// `seek 0 nan` rendait `SeekTo(0)`, tous deux **en silence**, contre la règle
/// que ce module énonce douze lignes plus haut — un argument absent ou non
/// numérique est un `Ack::Arg`, jamais un défaut muet. C'est la même classe que
/// le débordement d'`i16` de `volume`, sur le même port sans authentification,
/// à deux mètres de là.
fn temps_absolu(s: &str) -> Option<u32> {
    let v: f64 = s.parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        None
    } else {
        Some(v as u32)
    }
}

fn play(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let Some(arg) = args.first() else {
        // La touche Lecture, pas une sélection : relance ce qui était chargé
        // (ou démarre, pour une source qui sait quoi faire même à l'arrêt —
        // c'est à elle de décider, pas à ce greffon).
        return Issue::agir(Command::PlayPause);
    };
    let Ok(position) = arg.parse::<usize>() else {
        return Issue::Refuser(ack(Ack::Arg, indice, "play", "need a positive integer"));
    };
    match position_vers_index(&file_attente(inst), position) {
        Some(index) => Issue::agir(Command::Select(index)),
        None => Issue::Refuser(ack(Ack::Arg, indice, "play", "bad song index")),
    }
}

fn playid(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let Some(id) = args.first().and_then(|a| a.parse::<u8>().ok()) else {
        return Issue::Refuser(ack(Ack::Arg, indice, "playid", "need a positive integer"));
    };
    if index_existe(&file_attente(inst), id) {
        Issue::agir(Command::Select(id))
    } else {
        Issue::Refuser(ack(Ack::Arg, indice, "playid", "no such song"))
    }
}

/// `pause [0|1]`. Sans argument, bascule ; avec, n'émet que si l'état diffère
/// de la cible — c'est ce qui ferme la course décrite dans la spec (§ `pause`
/// dans `PlayerState.playback`) : un `pause 1` renvoyé deux fois par un client
/// qui n'a pas vu la confirmation ne doit pas relancer la lecture.
///
/// **À l'arrêt, n'émet jamais rien**, quel que soit l'argument : `PlayPause`
/// y démarrerait une lecture dont ni la source ni ce greffon ne savent ni quoi
/// ni où (voir `EtatPartage::acter_optimiste`), ce qu'un client n'a pas
/// demandé en appuyant sur « pause ». La validation de l'argument passe
/// **avant** cette garde : un `pause 2` malformé doit rester un `ACK` même à
/// l'arrêt, pas être avalé en silence.
fn pause(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let cible = match args.first().map(String::as_str) {
        None => None,
        Some("0") => Some(Playback::Playing),
        Some("1") => Some(Playback::Paused),
        Some(_) => return Issue::Refuser(ack(Ack::Arg, indice, "pause", "boolean expected")),
    };
    if inst.playback() == Playback::Stopped {
        return Issue::ok();
    }
    match cible {
        None => Issue::agir(Command::PlayPause),
        Some(cible) if inst.playback() != cible => Issue::agir(Command::PlayPause),
        Some(_) => Issue::ok(),
    }
}

/// `setvol <0-100>` : pose le volume, et **lève la sourdine s'il est au-dessus
/// de zéro**.
///
/// Le premier point est le protocole ; le second est la seule issue qu'un client
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
/// entendre quelque chose ; c'est la lecture qu'on retient.
///
/// **Émise conditionnellement, parce que `Command::Mute` est une bascule** et
/// non une pose : l'émettre alors que rien n'est coupé *couperait* le son. La
/// garde sur `etat.muted` est donc la même forme conditionnelle que `pause
/// 0`/`pause 1` emploie contre `playback`, et pour la même raison.
///
/// **L'ordre des deux commandes ne change pas le résultat**, et il faut l'écrire
/// parce que ce paragraphe a d'abord prétendu le contraire : il affirmait que le
/// cœur, en levant la sourdine, reposait le volume mémorisé, donc qu'il fallait
/// poser le volume après. C'est faux, et une raison fausse est pire qu'une
/// raison absente — elle fait croire à un mécanisme de restauration dont un
/// lecteur déduira des choses. Le bras `Command::Mute` du cœur fait
/// `muted = !muted` puis `set_mute(muted)`, et rien d'autre : le niveau et la
/// sourdine sont deux propriétés indépendantes, deux appels distincts à mpv.
/// `SetVolume(40)` puis `Mute`, ou `Mute` puis `SetVolume(40)`, laissent donc
/// tous deux un appareil non muet à 40.
///
/// L'ordre retenu — `SetVolume` d'abord — ne se joue que sur l'**intervalle**
/// entre les deux, qui existe bel et bien : elles traversent le canal d'entrée
/// l'une après l'autre, et chacune attend mpv.
///
/// * **Ce qu'on entend, et c'est la raison qui pèse.** Poser le niveau pendant
///   que la sortie est encore muette est inaudible, donc le son revient *déjà*
///   à 40. L'ordre inverse le ferait revenir au niveau mémorisé — jusqu'à 100 —
///   le temps d'un aller-retour avant de retomber. Sur un appareil dont le
///   volume mémorisé peut être bien au-dessus de ce que le client demande, c'est
///   la seule des deux différences qui se remarque.
/// * **Ce qu'on voit.** Les deux commandes appellent `show_overlay`, qui lit le
///   `muted` et le `volume` de l'instant. L'incrustation *finale* dit « 40 % »
///   dans les deux ordres ; seule l'intermédiaire diffère, et l'ordre retenu y
///   affiche « muet » — un mot encore juste à cet instant — au lieu de
///   l'ancien niveau, un nombre qui ne l'est plus.
///
/// **`setvol 0` ne coupe pas pour autant**, et c'est la règle inverse
/// inchangée : voir la spec, § « La sourdine, un cas à ne pas rater ».
/// `SetVolume(0)` pose zéro, `Mute` bascule ; les confondre ferait qu'un client
/// remontant le volume après un `setvol 0` trouverait le son toujours coupé —
/// exactement le défaut qu'on répare ici, réintroduit par l'autre bout.
fn setvol(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    match args.first().and_then(|a| a.parse::<u8>().ok()) {
        Some(v) if v <= 100 => Issue::Repondre { lignes: Vec::new(), cmds: demuter(inst, v) },
        _ => Issue::Refuser(ack(Ack::Arg, indice, "setvol", "invalid volume")),
    }
}

/// Les commandes d'une pose de volume : `SetVolume`, plus `Mute` si l'appareil
/// est muet et que le volume demandé n'est pas zéro.
///
/// Un seul endroit pour `setvol` et `volume` : les deux sont le même geste
/// (« monte le son »), et laisser l'une démuter sans l'autre ferait dépendre le
/// retour du son de l'âge du client — `volume` est dépréciée par MPD, donc
/// c'est la vieille moitié du parc qui resterait coincée.
fn demuter(inst: &Instantane, niveau: u8) -> Vec<Command> {
    let mut cmds = vec![Command::SetVolume(niveau)];
    if niveau > 0 && inst.etat.muted {
        cmds.push(Command::Mute);
    }
    cmds
}

/// `volume <±n>` : dépréciée par MPD mais encore émise par de vieux clients.
/// Relative au volume courant et **bornée ici** — `Command::SetVolume` est
/// absolu, donc c'est ce module qui doit calculer et clamper, pas le cœur.
fn volume(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    match args.first().and_then(|a| a.parse::<i16>().ok()) {
        Some(delta) => {
            // Élargi en `i32` avant l'addition : `delta` couvre tout `i16`
            // (±32767), et un volume courant même faible (1) additionné à
            // `i16::MAX` déborde `i16` avant que `.clamp` n'ait pu agir — un
            // panic en debug/test (overflow checks actifs par défaut), une
            // valeur fausse en release. `i32` contient les deux opérandes
            // (volume ≤ 100, delta ≤ 32767) sans aucun risque de dépassement,
            // donc le clamp reste le seul endroit qui borne.
            let nouveau = (i32::from(inst.etat.volume) + i32::from(delta)).clamp(0, 100) as u8;
            // Même levée de sourdine que `setvol`, et par le même chemin : voir
            // `demuter`. Le calcul part du volume **mémorisé** et non du zéro
            // que `status` publie quand c'est coupé — c'est le seul point de
            // départ qui ait un sens, et il rend `volume +5` sur un appareil
            // muet équivalent à ce que la télécommande ferait.
            Issue::Repondre { lignes: Vec::new(), cmds: demuter(inst, nouveau) }
        }
        None => Issue::Refuser(ack(Ack::Arg, indice, "volume", "invalid volume")),
    }
}

/// `seek <POS> <T>` / `seekid <ID> <T>` : le premier argument (position ou id)
/// est ignoré — `Command::SeekTo` ne sait pas changer de piste en même temps,
/// et MPD n'envoie ce genre de commande que sur ce qui joue déjà. `T` est
/// toujours absolu ici ; seul `seekcur` accepte le relatif (voir `seekcur`).
fn seek(indice: usize, cmd: &str, args: &[String]) -> Issue {
    match args.get(1).and_then(|a| temps_absolu(a)) {
        Some(t) => Issue::agir(Command::SeekTo(t)),
        None => Issue::Refuser(ack(Ack::Arg, indice, cmd, "float expected")),
    }
}

/// `seekcur <T>` : `T` est `+n`, `-n`, ou un absolu décimal. `Command` ne
/// porte qu'un positionnement absolu, donc le relatif est résolu ici, depuis
/// `position_s`, tronqué en secondes et **jamais négatif** — un recul plus
/// grand que la position rend `0`, pas un temps négatif.
fn seekcur(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let refuser = |message: &str| Issue::Refuser(ack(Ack::Arg, indice, "seekcur", message));
    let Some(arg) = args.first() else {
        return refuser("float expected");
    };
    let secondes = if arg.starts_with('+') || arg.starts_with('-') {
        let Ok(delta) = arg.parse::<f64>() else {
            return refuser("float expected");
        };
        // La même règle que `temps_absolu`, sur l'autre forme : `+inf` et `-nan`
        // se parsent, et sans cette garde `seekcur +inf` rendait
        // `SeekTo(u32::MAX)` en silence. Le relatif tolère le négatif (un recul
        // trop grand vaut zéro), jamais le non fini — il n'y a pas de position
        // à laquelle « l'infini » se ramène.
        if !delta.is_finite() {
            return refuser("float expected");
        }
        let Some(base) = inst.etat.position_s else {
            // Rien à résoudre depuis : un relatif sans point de départ connu
            // inventerait un temps, ce qu'aucun défaut silencieux ne doit
            // faire (voir la règle du brief sur les arguments hors bornes).
            return refuser("no current position");
        };
        // `.max(0.0)` est explicite plutôt qu'implicite : la conversion
        // `f64 -> u32` sature déjà à 0 sur un flottant négatif depuis Rust
        // 1.45, donc son retrait ne changerait rien à ce résultat-ci — mais
        // rien ne doit dépendre à l'œil de le savoir pour lire cette ligne.
        (f64::from(base) + delta).max(0.0) as u32
    } else {
        match temps_absolu(arg) {
            Some(t) => t,
            None => return refuser("float expected"),
        }
    };
    Issue::agir(Command::SeekTo(secondes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::etat::EtatPartage;
    use ritornello_proto::{Catalogue, Morceau, PlayerState};

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
    /// `playback_optimiste` recopie `etat.playback` : c'est l'état au repos, une
    /// fois la trame confirmante arrivée. Un test veut au contraire les voir
    /// diverger, et il le pose lui-même — c'est justement la propriété qu'il
    /// vérifie.
    ///
    /// `version_file` vaut 7 et non 0, pour que `playlist: 7` ne puisse pas
    /// passer par accident derrière une implémentation qui publierait une
    /// constante.
    fn depuis(etat: PlayerState) -> Instantane {
        Instantane { playback_optimiste: etat.playback, etat, version_file: 7, ..Default::default() }
    }

    /// La radio à l'arrêt : trois présélections, rien ne joue.
    fn radio_arretee() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 40,
            preset_count: Some(3),
            ..Default::default()
        }
    }

    /// La radio sur sa deuxième présélection, avec un morceau complet.
    fn radio_qui_joue(playback: Playback) -> PlayerState {
        PlayerState {
            playback,
            preset: Some(2),
            preset_name: Some("France Inter".into()),
            position_s: Some(12),
            morceau: Morceau {
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                album: Some("Kind of Blue".into()),
                duration_s: Some(545),
                year: Some(1959),
                // Le protocole MPD n'a pas de champ de lien : le greffon n'en
                // lit aucun, meme raison que `cover_href` plus bas.
                links: Vec::new(),
                origin: Some("musicbrainz".into()),
                // Le protocole MPD n'a pas de champ de pochette : le greffon
                // n'en lit aucun, mais le litteral doit rester complet — c'est
                // ce qui force a revoir ce test quand un champ apparait.
                cover_href: None,
                cover_origin: None,
            },
            ..radio_arretee()
        }
    }

    fn instantane_arrete() -> Instantane {
        depuis(radio_arretee())
    }

    /// Le son coupé, mais un volume mémorisé bien réel.
    fn instantane_muet(volume: u8) -> Instantane {
        depuis(PlayerState { volume, muted: true, ..radio_arretee() })
    }

    fn instantane_en_lecture() -> Instantane {
        depuis(radio_qui_joue(Playback::Playing))
    }

    fn instantane_en_pause() -> Instantane {
        depuis(radio_qui_joue(Playback::Paused))
    }

    /// Une station qui joue sans la moindre étiquette ICY : elle a un nom de
    /// présélection, et rien d'autre.
    fn instantane_sans_titre() -> Instantane {
        depuis(PlayerState {
            playback: Playback::Playing,
            preset: Some(1),
            preset_name: Some("Chérie FM".into()),
            ..radio_arretee()
        })
    }

    /// Une source qui déclare un nombre de présélections sans les nommer.
    fn instantane_sans_presets(source: &str, combien: u8) -> Instantane {
        depuis(PlayerState {
            source: source.into(),
            preset_count: Some(combien),
            ..Default::default()
        })
    }

    /// Une entrée de catalogue, telle que le cœur en émet une par source
    /// déclarée. Une liste de présélections vide est la vérité du cd et des
    /// fichiers, qui restent au corps par défaut de `list_presets`.
    fn source_catalogue(nom: &str, presets: &[(u8, &str)]) -> SourceCatalogue {
        SourceCatalogue {
            name: nom.to_string(),
            presets: presets
                .iter()
                .map(|(index, nom)| Preset { index: *index, name: (*nom).to_string() })
                .collect(),
        }
    }

    /// L'instantané d'un appareil dont le cœur a publié son catalogue, la
    /// source `active` étant celle que la dernière trame d'état désigne.
    ///
    /// **Deux détails de réalisme, parce qu'un instantané qu'aucun producteur
    /// ne peut émettre ne prouve rien** :
    /// - la source active est **ajoutée au catalogue** si elle n'y figure pas,
    ///   avec une liste vide : le catalogue du cœur nomme *toutes* les sources
    ///   déclarées, et le cd y est présent sans savoir énumérer. Un catalogue
    ///   qui ignorerait la source qui joue n'existe pas.
    /// - `preset_count` vaut le **maximum** des indices de la source active, et
    ///   non leur nombre : c'est ce que `Stations::preset_count` renvoie
    ///   vraiment (`radio/src/config.rs`). Trois stations 1, 5 et 99 font donc
    ///   `preset_count: Some(99)` — la forme exacte qui piège une
    ///   implémentation confondant compte et maximum. `None` quand la source
    ///   active n'énumère rien, comme une source qui n'a rien déclaré.
    fn instantane_catalogue(active: &str, sources: &[(&str, &[(u8, &str)])]) -> Instantane {
        let mut catalogue =
            Catalogue { sources: sources.iter().map(|(n, p)| source_catalogue(n, p)).collect() };
        if !catalogue.sources.iter().any(|s| s.name == active) {
            catalogue.sources.push(source_catalogue(active, &[]));
        }
        let maximum = catalogue
            .sources
            .iter()
            .find(|s| s.name == active)
            .and_then(|s| s.presets.iter().map(|p| p.index).max());
        Instantane {
            catalogue,
            ..depuis(PlayerState { source: active.into(), preset_count: maximum, ..Default::default() })
        }
    }

    /// Un catalogue de sources nommées sans présélections, la première étant
    /// active.
    ///
    /// C'est la forme que le catalogue a **au démarrage** : le cœur connaît ses
    /// sources dès le câblage et remplit leurs présélections au fur et à mesure
    /// que les réponses à `ListPresets` arrivent par le canal de mises à jour.
    fn instantane_avec_catalogue(noms: &[&str]) -> Instantane {
        let sources: Vec<(&str, &[(u8, &str)])> = noms.iter().map(|n| (*n, &[][..])).collect();
        instantane_catalogue(noms.first().copied().unwrap_or_default(), &sources)
    }

    /// Une source dont les présélections sont nommées, et qui joue.
    ///
    /// Les indices et les noms sont **respectés tels quels**, creux compris :
    /// c'est le catalogue qui les porte, et `file_attente` les recopie sans
    /// dériver un rang d'un indice.
    fn instantane_avec_presets(source: &str, presets: &[(u8, &str)]) -> Instantane {
        instantane_catalogue(source, &[(source, presets)])
    }

    /// Une source qui joue pendant qu'une autre est au catalogue : le cas qui a
    /// motivé le contournement du garde côté cœur (`handle_source_update` rend
    /// la main sur une trame qui ne vient pas de la source active, or le
    /// catalogue décrit toutes les sources).
    fn instantane_actif_sur(active: &str, sources: &[(&str, &[(u8, &str)])]) -> Instantane {
        instantane_catalogue(active, sources)
    }

    /// Un volume donné, sans rien d'autre autour.
    fn instantane_au_volume(volume: u8) -> Instantane {
        depuis(PlayerState { volume, ..radio_arretee() })
    }

    /// Une position connue dans ce qui joue, sans rien d'autre autour.
    fn instantane_a_la_position(position_s: u32) -> Instantane {
        depuis(PlayerState { position_s: Some(position_s), ..radio_arretee() })
    }

    fn traiter_mots(inst: &Instantane, indice: usize, mots: &[&str]) -> Issue {
        let args: Vec<String> = mots.iter().map(|m| (*m).to_string()).collect();
        traiter(inst, indice, &args)
    }

    /// Les lignes d'une réponse, ou une panique nommant ce qu'on a eu à la
    /// place — un `Refuser` inattendu doit se lire dans le message d'échec.
    fn traiter_ok(inst: &Instantane, mots: &[&str]) -> Vec<String> {
        match traiter_mots(inst, 0, mots) {
            Issue::Repondre { lignes, .. } => lignes,
            autre => panic!("attendu Repondre pour {mots:?}, obtenu {autre:?}"),
        }
    }

    /// Les commandes émises par une réponse, ou une panique nommant ce qu'on a
    /// eu à la place — le pendant de `traiter_ok` pour les tests de la Task 7.
    fn cmds(inst: &Instantane, mots: &[&str]) -> Vec<Command> {
        match traiter_mots(inst, 0, mots) {
            Issue::Repondre { cmds, .. } => cmds,
            autre => panic!("attendu Repondre pour {mots:?}, obtenu {autre:?}"),
        }
    }

    // ------------------------------------------------------------------
    // La file d'attente
    // ------------------------------------------------------------------

    #[test]
    fn sans_liste_la_file_se_synthetise_depuis_le_compte() {
        // Le cd : trois pistes, aucun nom. La suite est dense, `Pos = Id - 1`,
        // et elle commence à 1 — l'indice qu'attend `Command::Select`, pas un
        // rang base 0.
        let inst = instantane_sans_presets("cd", 3);
        assert_eq!(
            file_attente(&inst),
            vec![
                Entree { index: 1, nom: "1".into() },
                Entree { index: 2, nom: "2".into() },
                Entree { index: 3, nom: "3".into() },
            ]
        );
    }

    #[test]
    fn rien_de_declare_donne_une_file_vide_et_non_la_grille_historique() {
        // `preset_count: None` veut dire « la source n'a rien déclaré », ce que
        // l'IHM traduit par sa grille 1-9. Ici ce serait faux : annoncer neuf
        // entrees ferait demander a un client neuf choses dont aucune n'existe.
        let inst = depuis(PlayerState { source: "aux".into(), ..Default::default() });
        assert!(file_attente(&inst).is_empty());
        assert!(traiter_ok(&inst, &["status"]).contains(&"playlistlength: 0".to_string()));
        assert!(traiter_ok(&inst, &["playlistinfo"]).is_empty());
    }

    #[test]
    fn une_vraie_liste_prend_le_pas_sur_la_synthese() {
        // La branche que la Task 13 met **en tete** : des que le catalogue
        // nomme les preselections de la source active, ce sont elles la file —
        // avec leurs indices tels quels, creux compris, et leurs vrais noms.
        // Une implementation restee sur la synthese rendrait 1..=99.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(
            file_attente(&inst),
            vec![
                Entree { index: 1, nom: "FIP".into() },
                Entree { index: 5, nom: "Nova".into() },
                Entree { index: 99, nom: "TSF".into() },
            ]
        );
    }

    #[test]
    fn une_source_active_qui_nenumere_pas_retombe_sur_la_synthese() {
        // Le cd est bien au catalogue, avec une liste **vide** : cela veut dire
        // « je n'ai que des numeros », pas « je n'ai rien ». Sans ce repli sur
        // `preset_count`, les douze pistes d'un disque insere disparaitraient
        // le jour ou le catalogue arrive — une regression que seule cette
        // combinaison (catalogue present, liste vide) peut montrer.
        let inst = Instantane {
            catalogue: Catalogue { sources: vec![source_catalogue("cd", &[])] },
            ..instantane_sans_presets("cd", 12)
        };
        assert_eq!(file_attente(&inst).len(), 12);
        assert_eq!(file_attente(&inst)[11], Entree { index: 12, nom: "12".into() });
    }

    #[test]
    fn la_file_suit_la_source_active_et_non_la_premiere_du_catalogue() {
        // Le catalogue decrit toutes les sources ; la file d'attente n'est
        // faite que de celle qui joue. Prendre la premiere entree du catalogue
        // ferait publier les stations de la radio pendant qu'un disque tourne.
        let inst = instantane_actif_sur("cd", &[("radio", &[(1, "FIP"), (5, "Nova")]), ("cd", &[])]);
        assert!(file_attente(&inst).is_empty(), "le cd n'enumere pas et n'a rien declare");
    }

    #[test]
    fn les_positions_sont_denses_la_ou_les_indices_sont_creux() {
        // LE test du chantier, de bout en bout a travers `traiter` : sur des
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
        // ferait demander a un client quatre-vingt-seize entrees inexistantes.
        // Le fixe le confirme : `preset_count` vaut bien 99 ici, donc les deux
        // valeurs sont franchement distinctes.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(inst.etat.preset_count, Some(99), "le fixe doit bien porter le maximum");
        let lignes = traiter_ok(&inst, &["status"]);
        assert!(lignes.contains(&"playlistlength: 3".to_string()), "{lignes:?}");
        assert!(!lignes.contains(&"playlistlength: 99".to_string()), "{lignes:?}");
    }

    #[test]
    fn stats_compte_les_entrees_et_non_le_maximum_des_indices() {
        // Le jumeau du precedent sur `stats` : meme confusion possible, meme
        // silence des tests avant qu'une file creuse existe. `songs` est un
        // nombre d'entrees.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        let lignes = traiter_ok(&inst, &["stats"]);
        assert!(lignes.contains(&"songs: 3".to_string()), "{lignes:?}");
        assert!(!lignes.contains(&"songs: 99".to_string()), "{lignes:?}");
    }

    #[test]
    fn play_sur_une_liste_creuse_selectionne_lindice_du_rang_demande() {
        // `position_vers_index` vu depuis `traiter`, avec une file creuse que
        // le producteur peut vraiment emettre : `play 1` doit selectionner 5.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(cmds(&inst, &["play", "0"]), vec![Command::Select(1)]);
        assert_eq!(cmds(&inst, &["play", "1"]), vec![Command::Select(5)]);
        assert_eq!(cmds(&inst, &["play", "2"]), vec![Command::Select(99)]);
        assert!(
            matches!(traiter_mots(&inst, 0, &["play", "3"]), Issue::Refuser(_)),
            "trois entrees, donc le rang 3 n'existe pas — meme si l'indice 3 est sous le maximum"
        );
    }

    #[test]
    fn playid_sur_un_trou_de_la_liste_creuse_est_refuse() {
        // `index_existe` vu depuis `traiter` : 2 est sous le maximum (99) mais
        // n'est pas une station. Une comparaison de borne le laisserait passer,
        // et le coeur ignorerait le `Select` en silence.
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        assert_eq!(cmds(&inst, &["playid", "99"]), vec![Command::Select(99)]);
        assert!(matches!(traiter_mots(&inst, 0, &["playid", "2"]), Issue::Refuser(_)));
    }

    #[test]
    fn le_morceau_courant_dune_liste_creuse_publie_le_rang_et_lindice() {
        // `status` et `currentsong` doivent s'accorder sur les deux nombres :
        // `song`/`Pos` est le rang (1 pour la deuxieme entree), `songid`/`Id`
        // l'indice (5). Les confondre ferait surligner la mauvaise ligne.
        // La trame porte `preset: Some(5)` et le nom qui va avec, comme le
        // coeur les publie ensemble ; le catalogue porte les trois stations.
        let base = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova"), (99, "TSF")]);
        let inst = Instantane {
            etat: PlayerState {
                playback: Playback::Playing,
                preset: Some(5),
                preset_name: Some("Nova".into()),
                ..base.etat
            },
            playback_optimiste: Playback::Playing,
            ..base
        };
        let status = traiter_ok(&inst, &["status"]);
        assert!(status.contains(&"song: 1".to_string()), "{status:?}");
        assert!(status.contains(&"songid: 5".to_string()), "{status:?}");
        let courant = traiter_ok(&inst, &["currentsong"]);
        assert!(courant.contains(&"Pos: 1".to_string()), "{courant:?}");
        assert!(courant.contains(&"Id: 5".to_string()), "{courant:?}");
        assert!(courant.contains(&"Title: Nova".to_string()), "{courant:?}");
    }

    // ------------------------------------------------------------------
    // `status`
    // ------------------------------------------------------------------

    #[test]
    fn status_publie_ses_champs_dans_lordre_attendu() {
        // L'ordre et la présence sont le contrat : un client lit ces lignes
        // dans l'ordre où MPD les émet, et un champ manquant en fait renoncer
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
        // donc ils s'attendent a lire 0 quand c'est coupe.
        let inst = instantane_muet(65);
        assert!(traiter_ok(&inst, &["status"]).contains(&"volume: 0".to_string()));
    }

    #[test]
    fn status_ne_nomme_aucune_chanson_a_larret() {
        // `songid: 0` designerait une entree reelle : le champ doit etre absent.
        let lignes = traiter_ok(&instantane_arrete(), &["status"]);
        assert!(lignes.contains(&"state: stop".to_string()));
        assert!(!lignes.iter().any(|l| l.starts_with("song")), "{lignes:?}");
    }

    #[test]
    fn status_ne_nomme_aucune_chanson_meme_arrete_sur_une_preselection() {
        // La garde sur l'etat de lecture, distincte de celle sur `preset` :
        // `instantane_arrete` ne prouve que la seconde (il n'a aucune
        // presélection). Une source arretee qui a garde la sienne ne doit
        // designer aucune chanson dans `status`.
        let mut inst = instantane_arrete();
        inst.etat.preset = Some(2);
        let lignes = traiter_ok(&inst, &["status"]);
        assert!(!lignes.iter().any(|l| l.starts_with("song")), "{lignes:?}");
        // `currentsong`, lui, garde son morceau : l'asymetrie est celle de MPD,
        // qui publie un morceau courant meme a l'arret. Les deux gardes sont
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
            let lignes = traiter_ok(&inst, &["status"]);
            assert!(lignes.contains(&attendu.to_string()), "{attendu} absent de {lignes:?}");
            // Un seul `state`, et c'est le bon : une implementation qui
            // emettrait les trois passerait le `contains` ci-dessus.
            assert_eq!(lignes.iter().filter(|l| l.starts_with("state: ")).count(), 1);
        }
    }

    #[test]
    fn status_publie_letat_optimiste_et_non_celui_de_la_trame() {
        // La course de `pause` : un client qui envoie `pause` puis `status`
        // dans la meme foulee doit lire l'effet de sa propre commande, meme si
        // la trame confirmante n'est pas encore arrivee.
        let mut inst = instantane_en_pause();
        inst.playback_optimiste = Playback::Playing;
        assert!(traiter_ok(&inst, &["status"]).contains(&"state: play".to_string()));
    }

    #[test]
    fn les_options_sont_rapportees_a_zero_mais_pas_omises() {
        let lignes = traiter_ok(&instantane_arrete(), &["status"]);
        for cle in ["repeat: 0", "random: 0", "single: 0", "consume: 0"] {
            assert!(lignes.contains(&cle.to_string()), "{cle} absent de {lignes:?}");
        }
    }

    #[test]
    fn status_designe_la_chanson_par_sa_position_dense_et_son_indice_creux() {
        // La deuxieme preselection : position 1, indice 2. Les deux ne sont pas
        // interchangeables, et les confondre fait surligner la mauvaise ligne.
        let lignes = traiter_ok(&instantane_en_lecture(), &["status"]);
        assert!(lignes.contains(&"song: 1".to_string()), "{lignes:?}");
        assert!(lignes.contains(&"songid: 2".to_string()), "{lignes:?}");
    }

    #[test]
    fn status_tait_la_chanson_absente_de_la_file() {
        // Une preselection hors de la file (source qui annonce trois entrees et
        // joue la septieme) : un `song: 6` designerait une position que le
        // client ne trouvera pas dans le `playlistinfo` qu'il vient de lire.
        let mut inst = instantane_en_lecture();
        inst.etat.preset = Some(7);
        let lignes = traiter_ok(&inst, &["status"]);
        assert!(!lignes.iter().any(|l| l.starts_with("song")), "{lignes:?}");
    }

    #[test]
    fn status_omet_le_temps_quand_la_position_est_inconnue() {
        // Un flux dont un plugin annonce la duree du morceau sans en suivre
        // l'avancement : pas d'`elapsed: 0.000` invente, mais la duree reste.
        let mut inst = instantane_en_lecture();
        inst.etat.position_s = None;
        let lignes = traiter_ok(&inst, &["status"]);
        assert!(!lignes.iter().any(|l| l.starts_with("elapsed")), "{lignes:?}");
        assert!(!lignes.iter().any(|l| l.starts_with("time")), "{lignes:?}");
        assert!(lignes.contains(&"duration: 545.000".to_string()), "{lignes:?}");
    }

    #[test]
    fn status_annonce_un_total_nul_sur_un_direct() {
        // `time: 12:0` : la position est connue, la duree non. C'est ce que MPD
        // fait des flux, et un client qui lit `time` ne doit pas y trouver
        // autre chose que deux entiers.
        let mut inst = instantane_en_lecture();
        inst.etat.morceau.duration_s = None;
        let lignes = traiter_ok(&inst, &["status"]);
        assert!(lignes.contains(&"time: 12:0".to_string()), "{lignes:?}");
        assert!(!lignes.iter().any(|l| l.starts_with("duration")), "{lignes:?}");
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
                // `Date` s'insere entre l'album et la duree : l'ordre des
                // lignes est celui que ce test fige, et un client qui les lit
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
        // Une station sans titre ICY : pas de ligne `Title:` vide.
        let lignes = traiter_ok(&instantane_sans_titre(), &["currentsong"]);
        assert!(!lignes.iter().any(|l| l == "Title: " || l == "Artist: "), "{lignes:?}");
        // Et pas de champ vide du tout, quel qu'il soit.
        assert!(!lignes.iter().any(|l| l.ends_with(": ")), "{lignes:?}");
    }

    #[test]
    fn currentsong_retombe_sur_le_nom_de_la_preselection_faute_de_titre() {
        // Le nom de la station est la seule chose qu'on sache d'un flux sans
        // etiquette ICY ; sans ce repli, un client n'affiche que l'URI.
        let lignes = traiter_ok(&instantane_sans_titre(), &["currentsong"]);
        assert!(lignes.contains(&"Title: Chérie FM".to_string()), "{lignes:?}");
    }

    #[test]
    fn currentsong_publie_le_morceau_meme_en_pause() {
        // MPD garde un morceau courant en pause : le taire ferait vider l'ecran
        // du client des qu'il appuie sur pause.
        let lignes = traiter_ok(&instantane_en_pause(), &["currentsong"]);
        assert!(lignes.contains(&"Title: So What".to_string()), "{lignes:?}");
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
        // Un refus et non une reponse vide : le client a une file perimee, et
        // un `OK` sec le laisserait croire a un trou dans la liste.
        let inst = instantane_sans_presets("cd", 3);
        for mauvais in ["3", "-1", "abc", ""] {
            assert_eq!(
                traiter_mots(&inst, 1, &["playlistinfo", mauvais]),
                Issue::Refuser("ACK [2@1] {playlistinfo} bad song index".to_string()),
                "position {mauvais:?} acceptee a tort"
            );
        }
    }

    #[test]
    fn playlistinfo_accepte_une_plage_dont_la_fin_est_exclue() {
        // `playlistinfo [[SONGPOS] | [START:END]]` : un client qui fenetre sa
        // file demande `0:100`, et un `ACK` sur une requete bien formee lui fait
        // afficher une file vide. `1:3` rend deux entrees, pas trois, et leurs
        // `Pos` restent **absolus** — c'est la cle avec laquelle le client
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
        // hors bornes, qui designe une entree precise et reste un refus.
        let inst = instantane_sans_presets("cd", 3);
        assert_eq!(traiter_ok(&inst, &["playlistinfo", "9:12"]), Vec::<String>::new());
        assert_eq!(traiter_ok(&inst, &["playlistinfo", "3:3"]), Vec::<String>::new());
        assert!(matches!(
            traiter_mots(&inst, 0, &["playlistinfo", "9"]),
            Issue::Refuser(_)
        ));
    }

    #[test]
    fn une_plage_inversee_est_refusee() {
        // Aucun client correct ne produit `3:1` ; l'accepter masquerait le bogue
        // de l'appelant, et MPD le refuse aussi.
        assert_eq!(
            traiter_mots(&instantane_sans_presets("cd", 4), 0, &["playlistinfo", "3:1"]),
            Issue::Refuser("ACK [2@0] {playlistinfo} bad song index".to_string())
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
            Issue::Refuser("ACK [2@0] {plchanges} bad song index".to_string())
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
        // courante n'a pas a recevoir 51 lignes. `version_file` vaut 7 dans les
        // instantanes de reference.
        let inst = instantane_sans_presets("cd", 3);
        assert_eq!(inst.version_file, 7, "l'instantane de reference a change de version");
        assert_eq!(traiter_ok(&inst, &["plchanges", "7"]), Vec::<String>::new());
    }

    #[test]
    fn plchanges_sans_nombre_est_refuse() {
        let inst = instantane_arrete();
        for mots in [vec!["plchanges"], vec!["plchanges", "abc"], vec!["plchanges", "-1"]] {
            assert_eq!(
                traiter_mots(&inst, 0, &mots),
                Issue::Refuser("ACK [2@0] {plchanges} integer expected".to_string()),
                "{mots:?} acceptee a tort"
            );
        }
    }

    // ------------------------------------------------------------------
    // Les commandes de montage
    // ------------------------------------------------------------------

    #[test]
    fn commands_nannonce_que_ce_qui_existe() {
        let lignes = traiter_ok(&instantane_arrete(), &["commands"]);
        assert!(lignes.contains(&"command: status".to_string()));
        // La contrepartie, celle qui rend l'annonce honnete :
        for absente in ["add", "search", "lsinfo", "save", "kill"] {
            assert!(!lignes.contains(&format!("command: {absente}")), "{absente} annoncee a tort");
        }
    }

    #[test]
    fn chaque_commande_annoncee_est_reellement_geree() {
        // Le pendant du test precedent, et le seul qui empeche `COMMANDES` de
        // deriver du `match` : un nom annonce mais tombe dans le refus par
        // defaut se voit ici. Un refus pour cause d'argument (`plchanges` sans
        // version) est legitime — c'est le mot `unsupported` qui trahit une
        // commande qui n'existe pas.
        for nom in COMMANDES {
            if let Issue::Refuser(refus) = traiter_mots(&instantane_en_lecture(), 0, &[nom]) {
                assert!(!refus.contains("unsupported"), "{nom} annoncee mais non geree : {refus}");
            }
        }
    }

    #[test]
    fn notcommands_repond_vide() {
        // Elle liste ce que le mot de passe courant **interdit**. Il n'y a pas de
        // mot de passe ici, donc rien n'est interdit par permission : la reponse
        // honnete est vide, et non un refus qui ferait renoncer un vieux client
        // qui la demande juste apres `commands`.
        assert_eq!(traiter_mots(&instantane_arrete(), 0, &["notcommands"]), Issue::ok());
    }

    #[test]
    fn commandes_est_triee_et_sans_doublon() {
        // L'ordre alphabetique n'apporte rien aux clients, mais il rend visible
        // le doublon et l'insertion en vrac que les Tasks 7 et 13 vont faire.
        let mut triee: Vec<&str> = COMMANDES.to_vec();
        triee.sort_unstable();
        triee.dedup();
        assert_eq!(triee, COMMANDES.to_vec());
    }

    #[test]
    fn tagtypes_ne_nomme_que_les_trois_etiquettes_portees() {
        assert_eq!(
            traiter_ok(&instantane_arrete(), &["tagtypes"]),
            vec!["tagtype: Artist", "tagtype: Album", "tagtype: Title"]
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
        let lignes = traiter_ok(&instantane_sans_presets("cd", 12), &["stats"]);
        assert!(lignes.contains(&"songs: 12".to_string()), "{lignes:?}");
        assert!(lignes.contains(&"uptime: 0".to_string()), "{lignes:?}");
        assert!(lignes.contains(&"db_update: 0".to_string()), "{lignes:?}");
    }

    #[test]
    fn decoders_et_urlhandlers_repondent_ok_sec_mais_repondent() {
        // Presentes et vides : une commande inconnue au montage peut faire
        // renoncer un client avant qu'il n'affiche un ecran.
        for nom in ["decoders", "urlhandlers"] {
            assert_eq!(traiter_mots(&instantane_arrete(), 0, &[nom]), Issue::ok(), "{nom}");
        }
    }

    #[test]
    fn ping_password_et_close_ne_demandent_rien_a_lappareil() {
        let inst = instantane_arrete();
        assert_eq!(traiter_mots(&inst, 0, &["ping"]), Issue::ok());
        // Sans verification, et meme sans argument : il n'y a pas de mot de
        // passe, donc rien a controler et rien a refuser.
        assert_eq!(traiter_mots(&inst, 0, &["password", "secret"]), Issue::ok());
        assert_eq!(traiter_mots(&inst, 0, &["password"]), Issue::ok());
        assert_eq!(traiter_mots(&inst, 0, &["close"]), Issue::Fermer);
    }

    #[test]
    fn aucune_commande_de_lecture_nemet_de_commande_vers_le_coeur() {
        // La lecture seule est vraiment seulement de la lecture : un `status`
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
                Issue::Repondre { cmds, .. } => {
                    assert!(cmds.is_empty(), "{mots:?} a emis {cmds:?}");
                }
                autre => panic!("attendu Repondre pour {mots:?}, obtenu {autre:?}"),
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
            Issue::Attendre(vec![
                Sujet::Player,
                Sujet::Mixer,
                Sujet::Playlist,
                Sujet::StoredPlaylist
            ])
        );
    }

    #[test]
    fn idle_ne_retient_que_les_sujets_nommes_dans_lordre_et_sans_doublon() {
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["idle", "mixer", "player", "mixer"]),
            Issue::Attendre(vec![Sujet::Mixer, Sujet::Player])
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
                Issue::Refuser("ACK [2@2] {idle} unrecognized idle event".to_string()),
                "{mot:?} aurait du etre refuse"
            );
        }
    }

    #[test]
    fn idle_accepte_les_sous_systemes_de_mpd_que_nous_nemettons_jamais() {
        // Le defaut vu de l'autre cote : tout client bati sur
        // `mpd_send_idle_mask` de libmpdclient envoie une liste explicite, en
        // pratique `database update stored_playlist playlist player mixer output
        // options`. Refuser un mot **legal** lui vaudrait un `ACK` sur son
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
            Issue::Attendre(vec![Sujet::StoredPlaylist, Sujet::Playlist, Sujet::Player, Sujet::Mixer])
        );
        // Et les quatre autres noms du vocabulaire, ceux qu'aucun client
        // courant n'envoie mais que MPD connait.
        for mot in ["partition", "sticker", "subscription", "message", "neighbor", "mount"] {
            assert_eq!(
                traiter_mots(&inst, 0, &["idle", mot]),
                Issue::Attendre(Vec::new()),
                "{mot} devrait etre accepte puis ecarte"
            );
        }
    }

    #[test]
    fn une_attente_sur_un_sujet_quon_nemet_jamais_est_vide_et_non_immediate() {
        // `Attendre(vec![])` n'est pas `OK` : le client a demande a etre prevenu
        // d'un changement qui n'arrivera jamais, et attendre pour toujours est la
        // reponse MPD correcte. Le contrat est note sur la variante, parce que
        // c'est la Task 8 qui pourrait le trahir en traitant le vide comme un
        // `OK` sec.
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["idle", "database"]),
            Issue::Attendre(Vec::new())
        );
    }

    #[tokio::test]
    async fn une_liste_melangee_garde_le_reveil_du_sujet_quon_emet() {
        // `idle database mixer` : `database` est accepte puis ecarte, et cet
        // ecart ne doit pas emporter avec lui le reveil de `mixer`. Verifie de
        // bout en bout contre l'etat partage, et pas seulement sur la charge
        // utile de l'`Issue`.
        let issue = traiter_mots(&instantane_arrete(), 0, &["idle", "database", "mixer"]);
        let Issue::Attendre(sujets) = issue else {
            panic!("attendu Attendre, obtenu {issue:?}");
        };
        assert_eq!(sujets, vec![Sujet::Mixer]);

        let partage = EtatPartage::default();
        let vues = partage.versions().await;
        partage.appliquer_etat(PlayerState { volume: 55, ..Default::default() }).await;
        // Aucune marge d'horloge : le changement a **deja** eu lieu, donc
        // `attendre` rend la main par sa comparaison prealable sans jamais
        // dormir. Si `mixer` avait ete ecarte avec `database`, la liste serait
        // vide et ce test **pendrait** — l'echec est franc, a l'idiome des tests
        // d'`etat.rs`.
        assert_eq!(partage.attendre(&sujets, vues).await.bouges, vec![Sujet::Mixer]);
    }

    #[test]
    fn noidle_rend_la_main_sans_attendre() {
        assert_eq!(traiter_mots(&instantane_arrete(), 0, &["noidle"]), Issue::Annuler);
    }

    // ------------------------------------------------------------------
    // `play` / `playid`
    // ------------------------------------------------------------------

    #[test]
    fn position_vers_index_choisit_le_rang_et_non_lindice_moins_un() {
        // Le décalage qui coûte cher : sur des indices 1, 5, 99, le rang 1
        // (base 0, deuxième entrée) doit rendre 5 — pas 2 (le rang « plus
        // un »), ni aucun autre calcul dérivé de la position. Une file
        // construite à la main : voir la limite documentée sur
        // `instantane_avec_presets`, `file_attente` ne sait pas encore
        // synthétiser une suite creuse.
        let file = vec![
            Entree { index: 1, nom: "FIP".into() },
            Entree { index: 5, nom: "France Inter".into() },
            Entree { index: 99, nom: "Nova".into() },
        ];
        assert_eq!(position_vers_index(&file, 0), Some(1));
        assert_eq!(position_vers_index(&file, 1), Some(5));
        assert_eq!(position_vers_index(&file, 2), Some(99));
        assert_eq!(position_vers_index(&file, 3), None, "hors de la file");
    }

    #[test]
    fn index_existe_verifie_lappartenance_et_non_la_borne() {
        // 2 est bien inférieur au maximum de la file (5), mais absent : un
        // `playid 2` doit refuser, ce qu'une comparaison de borne laisserait
        // passer à tort une fois la file creuse (Task 13).
        let file = vec![Entree { index: 1, nom: "FIP".into() }, Entree { index: 5, nom: "France Inter".into() }];
        assert!(index_existe(&file, 5));
        assert!(!index_existe(&file, 2), "2 est sous le maximum (5) mais absent de la file");
    }

    #[test]
    fn play_avec_une_position_selectionne_lentree_de_ce_rang() {
        // Le chemin de bout en bout, dans les limites de ce que
        // `instantane_avec_presets` peut construire aujourd'hui (voir sa
        // doc) : une file dense où le rang est vérifié en passant par
        // `traiter`, pas par un appel direct à `position_vers_index`.
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
        assert!(matches!(traiter(&inst, 0, &["play".into(), "7".into()]), Issue::Refuser(_)));
    }

    #[test]
    fn playid_dun_indice_absent_est_refuse() {
        let inst = instantane_avec_presets("radio", &[(1, "FIP")]);
        assert!(matches!(traiter(&inst, 0, &["playid".into(), "9".into()]), Issue::Refuser(_)));
    }

    #[test]
    fn play_et_playid_avec_un_argument_non_numerique_sont_refuses() {
        // `play` sans argument n'est *pas* un refus (c'est la touche Lecture,
        // voir le test suivant) ; c'est seulement un argument non numérique,
        // ou l'absence du seul argument de `playid`, qui doivent l'être.
        let inst = instantane_avec_presets("radio", &[(1, "FIP")]);
        for mots in [vec!["play", "abc"], vec!["playid"], vec!["playid", "abc"]] {
            assert!(matches!(traiter_mots(&inst, 0, &mots), Issue::Refuser(_)), "{mots:?}");
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
        // C'est ce qui ferme la course : un `pause 1` sur une lecture déjà en
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
        // à l'arrêt démarrerait une lecture dont ni la source ni ce greffon ne
        // savent rien (voir `EtatPartage::acter_optimiste`), ce qu'un client
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
            Issue::Refuser(_)
        ));
    }

    // ------------------------------------------------------------------
    // `setvol` / `volume`
    // ------------------------------------------------------------------

    #[test]
    fn setvol_borne_et_refuse_hors_intervalle() {
        let inst = instantane_arrete();
        assert_eq!(cmds(&inst, &["setvol", "40"]), vec![Command::SetVolume(40)]);
        assert!(matches!(traiter(&inst, 0, &["setvol".into(), "101".into()]), Issue::Refuser(_)));
        assert!(matches!(traiter(&inst, 0, &["setvol".into(), "abc".into()]), Issue::Refuser(_)));
        assert!(matches!(traiter(&inst, 0, &["setvol".into()]), Issue::Refuser(_)));
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
        // **Le seul chemin dont un client MPD dispose pour rallumer le son.**
        // `status` publie `volume: 0` dès que l'appareil est muet, donc le
        // client remonte son curseur, `SetVolume(40)` part, le volume change —
        // et le son restait coupé, sans aucune issue depuis le téléphone.
        // L'ordre est épinglé ici parce que le test compare un `Vec`, pas parce
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
        // Le cas limite des deux règles réunies : poser zéro n'est pas
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
        // où dans `±32767` : additionner ce maximum à un volume courant même
        // faible dépasse `i16` avant que `.clamp` n'ait pu agir. Un panic en
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
        assert_eq!(inst.etat.position_s, None, "l'instantane de reference n'a pas de position");
        assert!(matches!(
            traiter_mots(&inst, 0, &["seekcur", "+10"]),
            Issue::Refuser(_)
        ));
    }

    #[test]
    fn seekcur_sans_argument_ou_non_numerique_est_refuse() {
        let inst = instantane_a_la_position(10);
        for mots in [vec!["seekcur"], vec!["seekcur", "abc"], vec!["seekcur", "+abc"]] {
            assert!(matches!(traiter_mots(&inst, 0, &mots), Issue::Refuser(_)), "{mots:?}");
        }
    }

    #[test]
    fn seek_et_seekid_ignorent_leur_premier_argument() {
        // `Command::SeekTo` ne sait pas changer de piste en même temps ; MPD
        // n'envoie de toute façon `seek` que sur ce qui joue.
        let inst = instantane_a_la_position(0);
        assert_eq!(cmds(&inst, &["seek", "0", "42"]), vec![Command::SeekTo(42)]);
        assert_eq!(cmds(&inst, &["seekid", "1", "42"]), vec![Command::SeekTo(42)]);
    }

    #[test]
    fn seek_normalise_un_signe_plus_redondant_en_tete_du_temps() {
        // `seek`/`seekid` restent absolus : un `+` en tête n'y est qu'un signe
        // de nombre comme un autre (`temps_absolu` ne distingue pas la forme
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
        // Les trois formes du protocole sont couvertes, parce que le relatif de
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
                matches!(traiter_mots(&inst, 0, &mots), Issue::Refuser(_)),
                "{mots:?} doit etre refuse, pas avale"
            );
        }
        // Et la forme légitime la plus proche reste acceptée : `infini` n'est
        // pas un nombre, mais `1e9` en est un.
        assert_eq!(cmds(&inst, &["seek", "0", "1000000"]), vec![Command::SeekTo(1_000_000)]);
    }

    #[test]
    fn seek_et_seekid_sans_temps_sont_refuses() {
        let inst = instantane_a_la_position(0);
        assert!(matches!(traiter_mots(&inst, 0, &["seek", "0"]), Issue::Refuser(_)));
        assert!(matches!(traiter_mots(&inst, 0, &["seekid", "1"]), Issue::Refuser(_)));
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
        let lignes = traiter_ok(&inst, &["listplaylists"]);
        assert_eq!(lignes.iter().filter(|l| l.starts_with("playlist: ")).count(), 3);
        assert!(lignes.contains(&"playlist: radio".to_string()), "{lignes:?}");
    }

    #[test]
    fn listplaylists_garde_lordre_du_catalogue() {
        // L'ordre reçu est celui de la bascule de `SourceCycle`, donc celui que
        // l'utilisateur voit sur sa télécommande : le trier alphabétiquement
        // perdrait une information que le client peut afficher.
        let inst = instantane_avec_catalogue(&["radio", "cd", "fichiers"]);
        let noms: Vec<String> = traiter_ok(&inst, &["listplaylists"])
            .into_iter()
            .filter_map(|l| l.strip_prefix("playlist: ").map(str::to_string))
            .collect();
        assert_eq!(noms, vec!["radio", "cd", "fichiers"]);
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
        assert_eq!(traiter_mots(&instantane_arrete(), 0, &["listplaylists"]), Issue::ok());
    }

    #[test]
    fn listplaylistinfo_rend_les_vrais_noms() {
        let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
        let lignes = traiter_ok(&inst, &["listplaylistinfo", "radio"]);
        assert!(lignes.contains(&"Title: FIP".to_string()), "{lignes:?}");
        assert!(lignes.contains(&"Title: Nova".to_string()), "{lignes:?}");
        // Et l'URI porte l'indice **creux**, pas un rang : c'est la clé stable
        // avec laquelle le client retrouvera l'entrée dans la file.
        assert!(lignes.contains(&"file: ritornello://radio/5".to_string()), "{lignes:?}");
    }

    #[test]
    fn listplaylistinfo_interroge_une_source_qui_ne_joue_pas() {
        // Le cas qui a motivé le contournement du garde côté cœur : le
        // catalogue décrit toutes les sources, et un client peut lire la liste
        // de la radio pendant qu'un disque tourne.
        let inst = instantane_actif_sur("cd", &[("radio", &[(1, "FIP")])]);
        assert_eq!(inst.etat.source, "cd", "le fixe doit bien jouer autre chose");
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
        let lignes = traiter_ok(&inst, &["listplaylistinfo", "radio"]);
        assert!(
            !lignes.iter().any(|l| l.starts_with("Pos: ") || l.starts_with("Id: ")),
            "{lignes:?}"
        );
    }

    #[test]
    fn listplaylistinfo_de_la_source_active_sans_liste_dit_la_meme_chose_que_la_file() {
        // Le cd qui joue : il ne sait pas énumérer, mais `preset_count` décrit
        // bien ses douze pistes, et les deux réponses doivent s'accorder — un
        // client qui compare la liste enregistrée à la file ne doit pas voir
        // deux appareils différents.
        let inst = Instantane {
            catalogue: Catalogue { sources: vec![source_catalogue("cd", &[])] },
            ..instantane_sans_presets("cd", 12)
        };
        let lignes = traiter_ok(&inst, &["listplaylistinfo", "cd"]);
        assert_eq!(lignes.len(), 24, "deux lignes par piste : {lignes:?}");
        assert!(lignes.contains(&"Title: 12".to_string()), "{lignes:?}");
    }

    #[test]
    fn listplaylistinfo_dune_source_inactive_sans_liste_est_vide_et_non_devinee() {
        // `preset_count` ne décrit que la source **active** : deviner le nombre
        // de pistes d'un disque qui ne joue pas serait une invention, et une
        // liste vide bien formée est la réponse honnête.
        let inst = instantane_actif_sur("radio", &[("radio", &[(1, "FIP")]), ("cd", &[])]);
        assert_eq!(traiter_mots(&inst, 0, &["listplaylistinfo", "cd"]), Issue::ok());
    }

    #[test]
    fn un_nom_de_liste_inconnu_est_un_ack_50() {
        let inst = instantane_avec_catalogue(&["radio"]);
        assert_eq!(
            traiter_mots(&inst, 0, &["listplaylistinfo", "nawak"]),
            Issue::Refuser("ACK [50@0] {listplaylistinfo} no such playlist".to_string())
        );
    }

    #[test]
    fn un_nom_de_liste_absent_est_un_ack_2_et_non_un_50() {
        // Le nom manquant n'est pas une liste inexistante mais une syntaxe
        // fautive : `ACK 2`, avec l'indice de la commande dans sa liste.
        let inst = instantane_avec_catalogue(&["radio"]);
        for cmd in ["listplaylistinfo", "load"] {
            assert_eq!(
                traiter_mots(&inst, 3, &[cmd]),
                Issue::Refuser(format!("ACK [2@3] {{{cmd}}} wrong number of arguments"))
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
        // Le greffon ne propose que des noms reçus du catalogue : c'est lui qui
        // refuse, pas le cœur en silence (`SelectSource` d'un nom inconnu y est
        // ignoré, et un `OK` suivi de rien est la pire réponse possible pour un
        // client, qui attendrait un changement de file qui n'arrive jamais).
        let inst = instantane_avec_catalogue(&["radio"]);
        assert_eq!(
            traiter_mots(&inst, 0, &["load", "nawak"]),
            Issue::Refuser("ACK [50@0] {load} no such playlist".to_string())
        );
    }

    #[test]
    fn load_de_la_source_deja_active_bascule_quand_meme() {
        // Aucune ruse ici : c'est le cœur qui sait si `SelectSource` sur la
        // source courante relance ou ne fait rien, et deviner à sa place ferait
        // avaler en silence le `load` d'un client qui vient de perdre son état.
        let inst = instantane_avec_catalogue(&["radio", "cd"]);
        assert_eq!(cmds(&inst, &["load", "radio"]), vec![Command::SelectSource("radio".into())]);
    }

    #[test]
    fn les_trois_commandes_de_liste_sont_desormais_annoncees() {
        // La Task 7 les taisait volontairement : `load` refusait tout nom,
        // faute de catalogue, et l'annoncer aurait rompu l'honnêteté que
        // `commands` promet. Le catalogue est là, elles marchent, elles se
        // déclarent.
        let lignes = traiter_ok(&instantane_avec_catalogue(&["radio"]), &["commands"]);
        for nom in ["load", "listplaylists", "listplaylistinfo"] {
            assert!(COMMANDES.contains(&nom), "{nom} absente de COMMANDES");
            assert!(lignes.contains(&format!("command: {nom}")), "{nom} non annoncee");
        }
    }

    // ------------------------------------------------------------------
    // Les refus
    // ------------------------------------------------------------------

    #[test]
    fn une_commande_inconnue_est_refusee_avec_son_indice_de_liste() {
        let inst = instantane_arrete();
        assert_eq!(
            traiter(&inst, 3, &["nawak".to_string()]),
            Issue::Refuser("ACK [5@3] {nawak} unsupported".to_string())
        );
    }

    #[test]
    fn les_commandes_decriture_sont_refusees_une_par_une() {
        // Elles doivent l'etre explicitement, pas par defaut : c'est la liste
        // que la doc promet, et un futur `add` accidentellement gere se verrait
        // ici. La liste est celle du § « Ce que le greffon ne fait pas ».
        for cmd in [
            "lsinfo",
            "listall",
            "listallinfo",
            "search",
            "find",
            "list",
            "count",
            "update",
            "add",
            "addid",
            "delete",
            "deleteid",
            "move",
            "swap",
            "shuffle",
            "clear",
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
            // `albumart` et `readpicture` figuraient ici, et n'y sont plus :
            // elles sont désormais gérées, et c'est bien cette liste-là qui
            // devait changer — la retirer d'ici est la moitié « traité ⊆
            // COMMANDES » du couple d'invariants, l'autre étant
            // `chaque_commande_annoncee_est_reellement_geree`.
            // `binarylimit` prend leur place : c'est la commande que MPD
            // associe aux réponses binaires (elle change la taille de tranche),
            // et ce greffon ne la gère pas — sa tranche est fixée à
            // `MAX_TRANCHE`. Un client qui l'envoie reçoit un refus lisible et
            // garde la valeur par défaut, qui est justement la nôtre.
            "binarylimit",
        ] {
            assert_eq!(
                traiter_mots(&instantane_arrete(), 0, &[cmd]),
                Issue::Refuser(format!("ACK [5@0] {{{cmd}}} unsupported")),
                "{cmd} devrait etre refusee"
            );
        }
    }

    #[test]
    fn une_ligne_vide_est_refusee_sans_paniquer() {
        // La session ne devrait pas en soumettre, mais une panique ici
        // couperait la connexion d'un client pour une ligne blanche.
        assert_eq!(
            traiter(&instantane_arrete(), 0, &[]),
            Issue::Refuser("ACK [5@0] {} unsupported".to_string())
        );
    }

    // ------------------------------------------------------------------
    // Les pochettes
    // ------------------------------------------------------------------

    /// Le `href` publié par la trame d'état, celui que la trame de pochette
    /// doit porter aussi.
    const HREF: &str = "/api/cover/1a2b3c";

    /// L'URI que notre `currentsong` publie pour l'instantané ci-dessous : la
    /// radio joue sa deuxième présélection.
    const URI_COURANTE: &str = "ritornello://radio/2";

    /// Une taille qui n'est **pas** un multiple de `MAX_TRANCHE` : trois
    /// tranches, dont la dernière est plus courte. Une taille ronde laisserait
    /// passer une implémentation qui rend toujours `MAX_TRANCHE` octets.
    const TAILLE: usize = MAX_TRANCHE * 2 + 1234;

    /// Un instantané où une pochette est arrivée, **cohérente avec l'état**.
    ///
    /// C'est la seule forme que le producteur peut émettre, et c'est le point :
    /// le cœur envoie la trame d'état (qui porte `cover_href`) *puis* les
    /// octets sous le même `href`. Un instantané dont la pochette et l'état ne
    /// s'accorderaient pas existe aussi — c'est la fenêtre entre les deux
    /// trames — mais c'est un autre cas, testé à part.
    fn instantane_avec_pochette(taille: usize) -> Instantane {
        let mut inst = instantane_en_lecture();
        inst.etat.morceau.cover_href = Some(HREF.to_string());
        inst.etat.morceau.cover_origin = Some("files".to_string());
        inst.pochette = Some(crate::etat::cover_de_test(HREF, taille).into());
        inst
    }

    /// La charge binaire d'une réponse, ou une panique nommant ce qu'on a eu à
    /// la place.
    fn octets_de(inst: &Instantane, mots: &[&str]) -> Binaire {
        match traiter_mots(inst, 0, mots) {
            Issue::Octets(b) => b,
            autre => panic!("attendu Octets pour {mots:?}, obtenu {autre:?}"),
        }
    }

    #[test]
    fn albumart_annonce_la_taille_totale_et_rend_la_premiere_tranche() {
        let inst = instantane_avec_pochette(TAILLE);
        let b = octets_de(&inst, &["albumart", URI_COURANTE, "0"]);
        // `size:` est la taille de l'**image entière**, pas de la tranche :
        // c'est elle qui dit au client combien d'allers-retours il lui reste.
        assert_eq!(b.entete, vec![format!("size: {TAILLE}")]);
        assert_eq!(b.tranche, 0..MAX_TRANCHE);
        assert_eq!(b.image.len(), TAILLE);
    }

    #[test]
    fn readpicture_ajoute_le_type_mime_et_sert_les_memes_octets() {
        // Les deux noms, une seule image : cet appareil n'a qu'une pochette par
        // piste, quelle que soit son origine. M.A.L.P. essaie l'un puis
        // l'autre, donc les deux doivent aboutir — et au même endroit.
        let inst = instantane_avec_pochette(TAILLE);
        let art = octets_de(&inst, &["albumart", URI_COURANTE, "0"]);
        let pic = octets_de(&inst, &["readpicture", URI_COURANTE, "0"]);
        assert_eq!(pic.entete, vec![format!("size: {TAILLE}"), "type: image/jpeg".to_string()]);
        assert_eq!(pic.tranche, art.tranche);
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
            assert_eq!(b.tranche.start, attendu, "la tranche doit commencer a l'offset demande");
            tailles.push(b.tranche.len());
            attendu = b.tranche.end;
        }
        assert_eq!(attendu, TAILLE, "les tranches doivent couvrir l'image entiere");
        assert_eq!(tailles, vec![MAX_TRANCHE, MAX_TRANCHE, 1234]);
    }

    #[test]
    fn un_offset_egal_a_la_taille_rend_une_tranche_vide_et_non_un_refus() {
        // Le comportement de MPD, et la raison est chez le client : une boucle
        // qui ferme par une requête de trop ne doit pas se voir refuser ce
        // qu'elle a déjà. La réponse est bien formée, simplement vide.
        let inst = instantane_avec_pochette(TAILLE);
        let b = octets_de(&inst, &["albumart", URI_COURANTE, &TAILLE.to_string()]);
        assert_eq!(b.entete, vec![format!("size: {TAILLE}")]);
        assert!(b.tranche.is_empty(), "{:?}", b.tranche);
    }

    #[test]
    fn un_offset_au_dela_de_la_taille_est_un_defaut_dargument() {
        let inst = instantane_avec_pochette(TAILLE);
        let trop = (TAILLE + 1).to_string();
        for nom in ["albumart", "readpicture"] {
            assert_eq!(
                traiter_mots(&inst, 4, &[nom, URI_COURANTE, &trop]),
                Issue::Refuser(format!("ACK [2@4] {{{nom}}} Offset too large")),
                "{nom} devrait refuser un offset hors image"
            );
        }
    }

    #[test]
    fn sans_pochette_les_deux_commandes_refusent_de_la_meme_facon() {
        // Le cas **ordinaire** et non l'exception : la plupart des flux n'ont
        // aucune image. Un `ACK 50` est ce que MPD répond quand il n'y a pas
        // d'art, et c'est ce qui fait basculer un client vers l'autre nom
        // plutôt que de l'immobiliser — une réponse vide couronnée de succès
        // ferait conclure « pas d'image » à un client qui n'essaie que
        // `readpicture`.
        let inst = instantane_en_lecture();
        assert!(inst.pochette.is_none(), "la fixe de base n'a pas de pochette");
        for nom in ["albumart", "readpicture"] {
            assert_eq!(
                traiter_mots(&inst, 0, &[nom, URI_COURANTE, "0"]),
                Issue::Refuser(format!("ACK [50@0] {{{nom}}} No file exists"))
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
                Issue::Refuser("ACK [50@0] {albumart} No file exists".to_string()),
                "{demandee} servie a tort"
            );
        }
        // Et l'URI courante, elle, est bien servie : sans cette moitié, le test
        // passerait avec une implémentation qui refuse tout.
        assert!(matches!(
            traiter_mots(&inst, 0, &["albumart", URI_COURANTE, "0"]),
            Issue::Octets(_)
        ));
    }

    #[test]
    fn une_pochette_qui_ne_decrit_plus_letat_courant_est_refusee() {
        // **La fenêtre entre les deux trames.** Le cœur envoie l'état d'abord
        // et la pochette ensuite : il existe donc un instant où l'état désigne
        // la piste suivante et où la pochette tenue est celle de la
        // précédente. Sans ce contrôle, `albumart` servirait l'ancienne image
        // **sous la nouvelle URI** — précisément le cas qui empoisonne le
        // cache du client, atteint sans que personne n'ait mal agi.
        let mut inst = instantane_avec_pochette(TAILLE);
        inst.etat.morceau.cover_href = Some("/api/cover/suivante".to_string());

        assert_eq!(
            traiter_mots(&inst, 0, &["albumart", URI_COURANTE, "0"]),
            Issue::Refuser("ACK [50@0] {albumart} No file exists".to_string())
        );
    }

    #[test]
    fn sans_preselection_courante_aucune_uri_ne_designe_rien() {
        // `currentsong` ne publie pas de `file:` dans cet état, donc aucun
        // client ne peut avoir d'URI légitime à demander.
        let mut inst = instantane_avec_pochette(TAILLE);
        inst.etat.preset = None;
        assert_eq!(
            traiter_mots(&inst, 0, &["albumart", URI_COURANTE, "0"]),
            Issue::Refuser("ACK [50@0] {albumart} No file exists".to_string())
        );
    }

    #[test]
    fn les_deux_commandes_exigent_une_uri_et_un_offset() {
        let inst = instantane_avec_pochette(TAILLE);
        for nom in ["albumart", "readpicture"] {
            for mots in [vec![nom], vec![nom, URI_COURANTE], vec![nom, URI_COURANTE, "0", "0"]] {
                assert_eq!(
                    traiter_mots(&inst, 1, &mots),
                    Issue::Refuser(format!("ACK [2@1] {{{nom}}} wrong number of arguments")),
                    "{mots:?} acceptee a tort"
                );
            }
            // Un offset non numérique est un autre défaut, et il se nomme
            // autrement : le client saura lequel de ses deux arguments revoir.
            for offset in ["abc", "-1", "1.5", ""] {
                assert_eq!(
                    traiter_mots(&inst, 1, &[nom, URI_COURANTE, offset]),
                    Issue::Refuser(format!("ACK [2@1] {{{nom}}} integer expected")),
                    "offset {offset:?} accepte a tort"
                );
            }
        }
    }

    #[test]
    fn les_deux_noms_sont_annonces_par_commands() {
        // Les deux moitiés de l'honnêteté de `commands`, sur ces deux noms
        // précis : ils sont dans la liste, et la liste est ce que la réponse
        // publie. `chaque_commande_annoncee_est_reellement_geree` ferme le
        // couple en vérifiant qu'aucun des deux ne retombe dans le refus par
        // défaut.
        let lignes = traiter_ok(&instantane_avec_pochette(TAILLE), &["commands"]);
        for nom in ["albumart", "readpicture"] {
            assert!(COMMANDES.contains(&nom), "{nom} absente de COMMANDES");
            assert!(lignes.contains(&format!("command: {nom}")), "{nom} non annoncee");
        }
    }
}
