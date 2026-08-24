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
    "noidle",
    "outputs",
    "password",
    "ping",
    "playlistinfo",
    "plchanges",
    "stats",
    "status",
    "tagtypes",
    "urlhandlers",
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
        // **Task 7** insère ici les commandes d'action, **Task 13** `load` et
        // les listes enregistrées.
        //
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

fn toute_la_file(inst: &Instantane, file: &[Entree]) -> Vec<String> {
    file.iter()
        .enumerate()
        .flat_map(|(position, entree)| entree_lignes(&inst.etat.source, position, entree))
        .collect()
}

fn playlistinfo(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let file = file_attente(inst);
    let Some(arg) = args.first() else {
        return Issue::lignes(toute_la_file(inst, &file));
    };
    // Une position hors bornes est un refus et non une réponse vide : le client
    // a une file périmée, et lui rendre `OK` le laisserait croire à un trou.
    match arg.parse::<usize>() {
        Ok(position) if position < file.len() => {
            Issue::lignes(entree_lignes(&inst.etat.source, position, &file[position]))
        }
        _ => Issue::Refuser(ack(Ack::Arg, indice, "playlistinfo", "bad song index")),
    }
}

fn plchanges(inst: &Instantane, indice: usize, args: &[String]) -> Issue {
    let Some(version) = args.first().and_then(|a| a.parse::<u32>().ok()) else {
        return Issue::Refuser(ack(Ack::Arg, indice, "plchanges", "integer expected"));
    };
    if version == inst.version_file {
        // Rien à dire, et c'est tout l'intérêt de la commande : un client qui
        // détient la version courante n'a pas à recevoir 51 lignes.
        return Issue::ok();
    }
    // La file entière, faute de savoir ce qui a changé dedans : la file *est*
    // la liste des présélections de la source active, et un changement de
    // source la remplace en totalité.
    Issue::lignes(toute_la_file(inst, &file_attente(inst)))
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

/// Le nom MPD d'un sous-système, tel qu'un client l'écrit dans son `idle`.
fn sujet(nom: &str) -> Option<Sujet> {
    match nom {
        "player" => Some(Sujet::Player),
        "mixer" => Some(Sujet::Mixer),
        "playlist" => Some(Sujet::Playlist),
        "stored_playlist" => Some(Sujet::StoredPlaylist),
        _ => None,
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
        // Un sujet inconnu est refusé et non ignoré : un client qui attend un
        // sous-système que nous n'émettrions jamais resterait muet pour
        // toujours, ce qui se diagnostique bien plus mal qu'un `ACK`.
        let Some(s) = sujet(nom) else {
            return Issue::Refuser(ack(Ack::Arg, indice, "idle", "unrecognized idle event"));
        };
        // Dédoublonné, comme `marquer` côté état : `idle player player` ne
        // décrit qu'une seule attente.
        if !sujets.contains(&s) {
            sujets.push(s);
        }
    }
    Issue::Attendre(sujets)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let interrogations: [&[&str]; 12] = [
            &["status"],
            &["currentsong"],
            &["playlistinfo"],
            &["playlistinfo", "0"],
            &["plchanges", "0"],
            &["commands"],
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
    fn un_sujet_inconnu_dans_idle_est_refuse_et_nattend_rien() {
        // Un client qui attendrait un sous-systeme que nous n'emettons jamais
        // resterait muet pour toujours : bien plus dur a diagnostiquer qu'un
        // `ACK`.
        assert_eq!(
            traiter_mots(&instantane_arrete(), 2, &["idle", "database"]),
            Issue::Refuser("ACK [2@2] {idle} unrecognized idle event".to_string())
        );
    }

    #[test]
    fn noidle_rend_la_main_sans_attendre() {
        assert_eq!(traiter_mots(&instantane_arrete(), 0, &["noidle"]), Issue::Annuler);
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
