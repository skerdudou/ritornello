//! Ce qu'une commande MPD devient : un instantané en entrée, des lignes en
//! sortie. **Aucune E/S, aucune horloge.**
//!
//! Cette pureté est le point du module et non une élégance : la table de
//! correspondance entre une commande MPD et la façade de l'appareil est ce
//! qu'un client voit en premier, et c'est aussi ce qui se vérifie le plus mal
//! à l'œil. Une fonction qui ne fait que choisir se teste ligne par ligne ;
//! la session (Task 8) garde pour elle tout ce qui touche la chaussette.
//!
//! **Sans appelant en production avant la Task 8.** Voir l'attribut de module
//! juste en dessous.

// Dans un crate binaire, la compilation non-test exclut le code `#[cfg(test)]`
// : tant que la session (Task 8) n'appelle pas `traiter`, *tout* ce module est
// mort de ce point de vue, y compris les variantes d'`Issue` qu'il construit —
// le lint remonte de proche en proche depuis les racines atteignables par
// `main`. D'où un attribut de module et non une dizaine d'attributs par
// élément : la dette est unique, elle a un seul point de retrait.
//
// **À retirer à la Task 8**, qui câble l'appelant réel. Ce qui resterait mort
// après ce retrait sera du code mort véritable, et c'est exactement ce qu'on
// veut voir apparaître à ce moment-là.
#![allow(dead_code)]

use crate::etat::{Instantane, Sujet};
use crate::protocole::{ack, ligne, Ack};
use ritornello_proto::{Command, Playback};
use std::ops::Range;

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
    /// `close` : `OK` puis fermeture.
    Fermer,
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
///
/// **Task 7** y ajoute les commandes d'action (`play`, `pause`, `setvol`,
/// `seek`, `next`, …), **Task 13** `load`, `listplaylists` et
/// `listplaylistinfo` quand le catalogue arrive. Tant qu'elles n'y sont pas,
/// elles n'existent pas — et un client les grise plutôt que de les tenter.
pub const COMMANDES: &[&str] = &[
    "close",
    "commands",
    "currentsong",
    "decoders",
    "idle",
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

/// La file d'attente MPD : les présélections de la source active.
///
/// Un seul cas aujourd'hui, la **synthèse** : à défaut de savoir énumérer, le
/// greffon fabrique `1..=preset_count`, et la suite est alors dense par
/// construction (`Pos = Id - 1`).
///
/// **Task 13 ajoute la branche « vraie liste » en tête**, dès que le catalogue
/// entre dans le greffon : une source qui sait énumérer rend des indices
/// éventuellement **creux** — `preset_count` est le *maximum* des numéros et
/// non leur nombre, donc des stations 1, 5 et 99 sont légales — là où les
/// positions MPD restent denses. C'est le piège du chantier, et la forme
/// « retour anticipé, synthèse en dernier » est faite pour qu'il n'y ait rien
/// à restructurer à ce moment-là.
///
/// `None` devient **zéro entrée** et non les neuf de la grille historique de
/// l'IHM : cette grille est un pavé numérique, pas une liste. Annoncer neuf
/// entrées ferait demander à un client neuf choses dont aucune n'existe.
pub fn file_attente(inst: &Instantane) -> Vec<Entree> {
    let n = inst.etat.preset_count.unwrap_or(0);
    (1..=n).map(|i| Entree { index: i, nom: i.to_string() }).collect()
}

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
        // ne coïncident plus dès qu'une source sait énumérer une liste creuse
        // (Task 13). Sans argument, ce n'est pas une sélection mais la touche
        // Lecture : on relance ce qui était chargé.
        "play" => play(inst, indice, reste),
        // `playid <ID>` : l'indice tel quel, mais vérifié dans la file — un
        // `ID` à l'intérieur du maximum (`preset_count`) sans y être une fois
        // la file creuse (Task 13) doit refuser, pas seulement une borne.
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
        "setvol" => setvol(indice, reste),
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
        // PROVISOIRE (Task 13) : `load <nom>` devrait choisir une source par
        // son nom (`Command::SelectSource`), mais aucun catalogue de sources
        // n'existe encore dans ce greffon pour vérifier qu'un tel nom existe
        // — le catalogue n'arrive qu'à la Task 13. En attendant, tout nom est
        // donc « inexistant », d'où le même refus qu'un vrai nom absent du
        // futur catalogue rendrait — ce n'est pas un hasard. Volontairement
        // **absent de `COMMANDES`** (voir la spec, Ruling 3) : y annoncer une
        // commande qui refuse toujours romprait l'honnêteté que `commands`
        // promet à un client correct.
        "load" => Issue::Refuser(ack(Ack::NoExist, indice, "load", "no such playlist")),
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
    // d'entrées qu'un client va demander. (Les deux coïncident tant que la file
    // est synthétisée ; Task 13 les sépare.)
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
/// Extraite en fonction pure, séparée de `play`, pour se tester sur une file
/// construite à la main : `file_attente` ne sait aujourd'hui que synthétiser
/// une suite dense (`1..=preset_count`), où le rang et « l'indice moins un »
/// coïncident toujours et ne peuvent donc jamais démasquer un décalage
/// silencieux à travers `Instantane`. La Task 13 apporte la vraie liste,
/// éventuellement creuse ; cette fonction est déjà correcte pour ce jour-là.
fn position_vers_index(file: &[Entree], position: usize) -> Option<u8> {
    file.get(position).map(|e| e.index)
}

/// Vrai si cet indice de présélection existe réellement dans la file — pas
/// seulement dans les bornes de son maximum.
///
/// Distinction sans effet aujourd'hui (`file_attente` ne rend qu'une suite
/// dense `1..=preset_count`, donc « exister » et « être ≤ au maximum » sont
/// encore la même chose), mais qui cessera de l'être dès qu'une file peut être
/// creuse (Task 13, où `preset_count` reste un maximum et non un compte) : un
/// `playid` sur un trou de cette suite doit refuser, une comparaison de borne
/// le laisserait passer à tort.
fn index_existe(file: &[Entree], index: u8) -> bool {
    file.iter().any(|e| e.index == index)
}

/// Un temps MPD absolu, en secondes tronquées. `None` si non numérique ou
/// négatif — jamais un temps négatif ramené à zéro en silence pour cette forme
/// (contrairement à la résolution du relatif de `seekcur`, où zéro est la
/// bonne réponse à un recul trop grand).
fn temps_absolu(s: &str) -> Option<u32> {
    let v: f64 = s.parse().ok()?;
    if v < 0.0 {
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

fn setvol(indice: usize, args: &[String]) -> Issue {
    match args.first().and_then(|a| a.parse::<u8>().ok()) {
        // `0` n'est **pas** traduit en `Mute` : voir la spec, § « La sourdine,
        // un cas à ne pas rater ». `Mute` bascule, `SetVolume` pose ; les
        // confondre ferait qu'un client remontant le volume après ce `setvol
        // 0` trouverait le son toujours coupé.
        Some(v) if v <= 100 => Issue::agir(Command::SetVolume(v)),
        _ => Issue::Refuser(ack(Ack::Arg, indice, "setvol", "invalid volume")),
    }
}

/// `volume <±n>` : dépréciée par MPD mais encore émise par de vieux clients.
/// Relative au volume courant et **bornée ici** — `Command::SetVolume` est
/// absolu, donc c'est ce module qui doit calculer et clamper, pas le cœur.
fn volume(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    match args.first().and_then(|a| a.parse::<i16>().ok()) {
        Some(delta) => {
            let nouveau = (i16::from(inst.etat.volume) + delta).clamp(0, 100) as u8;
            Issue::agir(Command::SetVolume(nouveau))
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
    use ritornello_proto::{Morceau, PlayerState};

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
                origin: Some("musicbrainz".into()),
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

    /// Une source dont les présélections sont nommées.
    ///
    /// **Limite actuelle, à lire avant de s'en servir** : `preset_count` est
    /// le seul levier que porte `Instantane` avant la Task 13, et
    /// `file_attente` n'en tire qu'une suite dense `1..=preset_count` — les
    /// noms et indices donnés ici ne sont donc respectés que pour un jeu déjà
    /// dense partant de 1 (`[(1, _), (2, _), (3, _), …]`), et seul le
    /// *nombre* d'entrées compte alors, pas les indices ni les noms demandés.
    /// Un jeu creux (`[(1, _), (5, _), (99, _)]`) ne peut **pas** être
    /// construit par cette voie tant que la vraie liste n'existe pas : cette
    /// distinction est couverte séparément, sur la fonction pure que
    /// `play`/`playid` délèguent (`position_vers_index`, `index_existe`),
    /// avec une file construite à la main plutôt qu'avec cet instantané.
    fn instantane_avec_presets(source: &str, presets: &[(u8, &str)]) -> Instantane {
        depuis(PlayerState {
            source: source.into(),
            preset_count: Some(presets.len() as u8),
            ..Default::default()
        })
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
        assert_eq!(partage.attendre(&sujets, vues).await, vec![Sujet::Mixer]);
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
    fn volume_est_relatif_et_borne_sur_le_volume_courant() {
        // Commande dépréciée mais encore émise. Bornée ici, pas laissée
        // déborder.
        let inst = instantane_au_volume(95);
        assert_eq!(cmds(&inst, &["volume", "+10"]), vec![Command::SetVolume(100)]);
        assert_eq!(cmds(&instantane_au_volume(3), &["volume", "-10"]), vec![Command::SetVolume(0)]);
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
    // `load`, provisoire (Task 13)
    // ------------------------------------------------------------------

    #[test]
    fn load_est_provisoirement_refuse_faute_de_catalogue() {
        // Voir le commentaire du bras `load` dans `traiter` : aucun catalogue
        // de sources n'existe encore pour vérifier qu'un nom donné existe, la
        // Task 13 remplace ce refus fixe par une vraie recherche.
        assert_eq!(
            traiter_mots(&instantane_arrete(), 0, &["load", "radio"]),
            Issue::Refuser("ACK [50@0] {load} no such playlist".to_string())
        );
    }

    #[test]
    fn load_nest_pas_annoncee_dans_commands() {
        // Ruling 3 : annoncer une commande qui refuse toujours romprait
        // l'honnêteté que `commands` promet à un client correct.
        assert!(!COMMANDES.contains(&"load"));
        let lignes = traiter_ok(&instantane_arrete(), &["commands"]);
        assert!(!lignes.contains(&"command: load".to_string()));
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
            "albumart",
            "readpicture",
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
}
