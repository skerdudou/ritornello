//! Le dialogue avec un client : la seule partie du greffon qui touche une
//! chaussette.
//!
//! Une tâche par connexion, et c'est l'architecture entière : toute question se
//! répond depuis l'état partagé (une prise de verrou en lecture), toute action
//! est un envoi sur un canal borné. Aucune session n'a donc à attendre le cœur,
//! donc **aucune ne peut retenir une autre** — un client endormi dans un `idle`
//! ne coûte qu'une tâche en attente.
//!
//! Les listes de commandes et `idle` vivent ici et non dans `commandes.rs`,
//! parce que ce sont des faits sur la **connexion** et non sur une commande :
//! `command_list_begin` ne fait rien d'autre que changer ce que les lignes
//! suivantes veulent dire, et `idle` ne fait rien d'autre que suspendre la
//! lecture des lignes. `commandes.rs` reste pur, et se teste sans socket.

use crate::commandes::{traiter, Issue};
use crate::etat::{EtatPartage, Sujet};
use crate::protocole::{ack, decouper, ligne, Ack};
use anyhow::Result;
use ritornello_proto::InputMessage;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// La version annoncée dans la bannière.
///
/// **Elle est mentie, et il faut le dire** : ce greffon n'implémente pas tout
/// MPD 0.23.5, il en implémente ce que `commands` énumère. Le mensonge est
/// délibéré parce que les clients dérivent leurs capacités de ce numéro et non
/// de `commands` seul — libmpdclient et M.A.L.P. comparent la version annoncée
/// avant d'émettre `plchanges`, `seekcur` ou `tagtypes` — donc annoncer une
/// version basse leur ferait renoncer à des commandes qu'on gère réellement.
/// Le risque inverse (annoncer trop haut) est borné par `commands`, qui dit la
/// vérité, et par les `ACK 5` du reste.
const VERSION_ANNONCEE: &str = "0.23.5";

/// Plafond de commandes accumulées dans une liste, avant `command_list_end`.
///
/// Ce n'est pas de la prudence décorative : entre `command_list_begin` et son
/// `end`, la session **mémorise** chaque ligne sans rien exécuter, donc un
/// client (ou un scanner de port bavard) qui n'envoie jamais le `end` fait
/// croître un `Vec` sans borne dans un processus qui tourne sur un Pi. MPD a la
/// même borne, exprimée en octets (`max_command_list_size`, 2 Mio par défaut) ;
/// ici c'est un nombre de commandes, plus simple à justifier et suffisant pour
/// le même effet. 2048 est très au-delà de ce qu'un client réel envoie —
/// M.A.L.P. groupe une dizaine de commandes.
const MAX_COMMANDES_LISTE: usize = 2048;

/// Plafond d'une **ligne** de commande, en octets.
///
/// Sans lui, c'est la dernière surface non bornée d'un port ouvert sur tout le
/// réseau local : un client qui se connecte et envoie des octets **sans jamais
/// envoyer de retour à la ligne** fait allouer le greffon jusqu'à ce que
/// l'allocateur renonce. Sur cet appareil — un Pi 2 B, un gigaoctet partagé
/// entre mpv, le cœur, l'IHM web et huit greffons — cela n'emporte pas
/// seulement le greffon, cela emporte la musique. Et cela ne demande aucune
/// malveillance : un scanner de port ou un client bogué le fait par accident,
/// et le port est atteignable de tout le réseau local sans mot de passe.
///
/// 8 Kio est deux fois le tampon d'entrée de MPD lui-même (4 Kio) et un ordre
/// de grandeur au-dessus de la plus longue ligne légitime du protocole — un nom
/// de liste entre guillemets dans une liste de commandes, quelques centaines
/// d'octets au pire. Très au-dessus du réel, très en dessous de ce qui coûte :
/// même cent connexions simultanées ne réservent ainsi qu'un mégaoctet.
const MAX_LIGNE: usize = 8 * 1024;

/// Le lecteur de lignes de la session : un `BufReader`, plus le plafond.
///
/// Écrit à la main (`fill_buf`/`consume`) plutôt qu'avec `BufReader::lines()`,
/// pour la seule raison qui vaille : `lines()` accumule jusqu'au `\n` **sans
/// borne**. Voir `MAX_LIGNE`.
struct LecteurBorne {
    lecture: BufReader<OwnedReadHalf>,
    /// Les octets de la ligne en cours, entre deux `\n`.
    ///
    /// Il vit dans la structure et non dans la pile de `ligne_suivante`, et ce
    /// n'est pas un détail : c'est ce qui rend cette fonction **sûre à
    /// l'annulation**, exactement comme le tampon de `tokio::io::Lines`.
    /// `attendre_idle` la met dans un `select!` avec le réveil, donc elle est
    /// abandonnée en cours de route chaque fois qu'un dormeur se réveille — si
    /// le tampon était local, la moitié de ligne déjà lue partirait avec lui,
    /// et la commande suivante serait tronquée.
    tampon: Vec<u8>,
}

impl LecteurBorne {
    fn new(lecture: OwnedReadHalf) -> Self {
        Self { lecture: BufReader::new(lecture), tampon: Vec::new() }
    }

    /// La ligne suivante sans son `\n`, ou `None` à la fin du flux.
    ///
    /// Une ligne qui dépasse `MAX_LIGNE` est une **erreur**, donc la fin de la
    /// session : c'est ce que fait MPD, et c'est le seul choix défendable ici.
    /// Un `ACK` supposerait de nommer la commande fautive — impossible, la
    /// ligne est tronquée — puis de jeter un nombre inconnu d'octets jusqu'au
    /// prochain `\n`, c'est-à-dire de garder une connexion qui a déjà quitté le
    /// protocole. Fermer est immédiat, défini, et journalisé par `accepter`.
    async fn ligne_suivante(&mut self) -> Result<Option<String>> {
        loop {
            let dispo = self.lecture.fill_buf().await?;
            if dispo.is_empty() {
                // Fin de flux. Une dernière ligne sans `\n` est rendue quand
                // même, comme le faisait `Lines` : un client qui ferme sa
                // moitié écriture juste après une commande doit voir cette
                // commande traitée.
                if self.tampon.is_empty() {
                    return Ok(None);
                }
                let ligne = std::mem::take(&mut self.tampon);
                return Ok(Some(Self::finir(ligne)?));
            }
            match dispo.iter().position(|octet| *octet == b'\n') {
                Some(fin) => {
                    // Le plafond est vérifié **avant** de recopier : un
                    // dépassement ne doit pas d'abord allouer ce qu'il refuse.
                    // Contrôlé dans les deux bras, et pas seulement dans celui
                    // sans `\n`, pour que la borne tienne quelle que soit la
                    // capacité du `BufReader`.
                    if self.tampon.len() + fin > MAX_LIGNE {
                        anyhow::bail!("command line longer than {MAX_LIGNE} bytes");
                    }
                    self.tampon.extend_from_slice(&dispo[..fin]);
                    self.lecture.consume(fin + 1);
                    let ligne = std::mem::take(&mut self.tampon);
                    return Ok(Some(Self::finir(ligne)?));
                }
                None => {
                    let recu = dispo.len();
                    if self.tampon.len() + recu > MAX_LIGNE {
                        anyhow::bail!(
                            "command line longer than {MAX_LIGNE} bytes without a newline"
                        );
                    }
                    self.tampon.extend_from_slice(dispo);
                    self.lecture.consume(recu);
                }
            }
        }
    }

    /// Les octets d'une ligne en `String`.
    ///
    /// Un `\r` terminal est retiré : `\r\n` est ce qu'envoient les clients
    /// écrits sur Windows, et sans cela `ping\r` serait une commande inconnue.
    /// C'est aussi ce que faisait `Lines` — le perdre en changeant de lecteur
    /// aurait été une régression qu'aucun test existant ne voyait.
    ///
    /// Un octet non UTF-8 est une erreur, donc la fin de la session : là aussi
    /// le comportement de `Lines`, conservé tel quel. Le protocole MPD est
    /// textuel, et une commande dont les octets ne forment pas du texte ne se
    /// découpe pas.
    fn finir(mut ligne: Vec<u8>) -> Result<String> {
        if ligne.last() == Some(&b'\r') {
            ligne.pop();
        }
        Ok(String::from_utf8(ligne)?)
    }
}

/// Ce que la session doit faire après un lot de commandes.
enum Suite {
    /// Continuer à lire des lignes.
    Continuer,
    /// Fermer la connexion : `close`, ou une moitié `input` morte.
    Fermer,
}

/// Accepte les connexions et donne chacune à sa propre tâche.
///
/// Une erreur d'`accept` est journalisée et la boucle continue : un descripteur
/// épuisé ou une connexion réinitialisée avant l'`accept` ne doit pas emporter
/// le serveur, sinon le port reste ouvert dans un processus qui n'écoute plus.
pub async fn accepter(ecoute: TcpListener, etat: Arc<EtatPartage>, cmd_tx: mpsc::Sender<InputMessage>) {
    loop {
        match ecoute.accept().await {
            Ok((flux, adresse)) => {
                tracing::info!("mpd client connected from {adresse}");
                let etat = etat.clone();
                let cmd_tx = cmd_tx.clone();
                // Une tâche par client, détachée : c'est ce qui rend une
                // session incapable d'en retenir une autre. Le `spawn` ne
                // rend rien à surveiller — une session qui finit n'a rien à
                // dire de plus que ce qu'elle journalise ici.
                tokio::spawn(async move {
                    match servir(flux, etat, cmd_tx).await {
                        Ok(()) => tracing::info!("mpd client {adresse} disconnected"),
                        Err(e) => tracing::info!("mpd session with {adresse} ended: {e}"),
                    }
                });
            }
            Err(e) => {
                tracing::warn!("mpd accept failed: {e}");
            }
        }
    }
}

/// Le dialogue d'une connexion, du premier octet écrit au dernier lu.
///
/// L'état de liste vit dans cette fonction et nulle part ailleurs : il n'a de
/// sens que pour cette connexion, et deux clients dont l'un est au milieu d'une
/// liste ne se voient pas.
pub async fn servir(flux: TcpStream, etat: Arc<EtatPartage>, cmd_tx: mpsc::Sender<InputMessage>) -> Result<()> {
    let (lecture, mut ecriture) = flux.into_split();
    let mut lignes = LecteurBorne::new(lecture);

    // La bannière part sans qu'on demande rien : c'est le protocole, et un
    // client attend cette ligne avant d'écrire quoi que ce soit.
    ecriture.write_all(format!("OK MPD {VERSION_ANNONCEE}\n").as_bytes()).await?;

    // Les commandes accumulées d'une liste en cours, `None` hors liste.
    // Un `Option<Vec<_>>` plutôt qu'un `Vec` plus un booléen : « pas dans une
    // liste » et « dans une liste encore vide » sont deux états différents, et
    // un `command_list_end` reçu hors liste doit être refusé comme une
    // commande inconnue plutôt que rendre un `OK` de complaisance.
    let mut liste: Option<Vec<Vec<String>>> = None;
    let mut avec_ok = false;

    while let Some(brute) = lignes.ligne_suivante().await? {
        let args = match decouper(&brute) {
            Ok(args) => args,
            Err(code) => {
                // Une ligne illisible est un `ACK`, jamais une rupture : un
                // client qui a mal cité un nom de station doit pouvoir
                // continuer sans se reconnecter.
                //
                // Une liste en cours est en revanche abandonnée : il y manque
                // une commande, donc l'exécuter plus tard exécuterait un lot
                // qui n'est pas celui que le client a écrit.
                let indice = liste.as_ref().map_or(0, Vec::len);
                liste = None;
                let refus = ack(code, indice, premier_mot(&brute), "invalid argument");
                ecrire(&mut ecriture, &[refus]).await?;
                continue;
            }
        };
        // `""` pour une ligne vide : `traiter` la refuse déjà (elle est totale
        // par construction), donc rien ici n'a besoin d'un cas à part.
        let mot = args.first().map_or("", String::as_str);

        if liste.is_some() {
            match mot {
                "command_list_end" => {
                    let lot = liste.take().unwrap_or_default();
                    match executer(&mut lignes, &mut ecriture, &etat, &cmd_tx, &lot, avec_ok).await? {
                        Suite::Continuer => {}
                        Suite::Fermer => break,
                    }
                }
                // `idle` dans une liste : MPD l'interdit, et pour une bonne
                // raison — l'accepter demanderait de suspendre une liste à
                // moitié écrite, dont le `OK` final ne peut pas partir avant
                // le réveil. L'indice porté est le **rang** qu'`idle` occupe
                // dans la liste, sinon le client ne sait pas laquelle de ses
                // commandes a été refusée.
                //
                // Refus **à l'accumulation** et non à l'exécution : la liste
                // ne pourra jamais être exécutée, donc y exécuter d'abord les
                // commandes qui précèdent émettrait de vraies actions (un
                // `next`, un `setvol`) au nom d'un lot que le client ne verra
                // jamais aboutir.
                "idle" => {
                    let indice = liste.as_ref().map_or(0, Vec::len);
                    liste = None;
                    let refus = ack(Ack::Unknown, indice, "idle", "not allowed in command list");
                    ecrire(&mut ecriture, &[refus]).await?;
                }
                _ => {
                    let indice = liste.as_ref().map_or(0, Vec::len);
                    if indice >= MAX_COMMANDES_LISTE {
                        liste = None;
                        let refus = ack(Ack::Unknown, indice, mot, "list too large");
                        ecrire(&mut ecriture, &[refus]).await?;
                    } else if let Some(accumule) = liste.as_mut() {
                        // Accumulé sans être regardé : un `command_list_begin`
                        // imbriqué, un mot inconnu ou une ligne vide seront
                        // refusés par `traiter` à l'exécution, à leur rang, et
                        // interrompront la suite comme n'importe quelle autre
                        // erreur. Aucun cas particulier à écrire ici.
                        accumule.push(args);
                    }
                }
            }
            continue;
        }

        match mot {
            "command_list_begin" => {
                liste = Some(Vec::new());
                avec_ok = false;
            }
            "command_list_ok_begin" => {
                liste = Some(Vec::new());
                avec_ok = true;
            }
            _ => {
                let lot = std::slice::from_ref(&args);
                match executer(&mut lignes, &mut ecriture, &etat, &cmd_tx, lot, false).await? {
                    Suite::Continuer => {}
                    Suite::Fermer => break,
                }
            }
        }
    }
    Ok(())
}

/// Exécute un lot — une commande seule, ou les commandes d'une liste — et
/// **écrit lui-même** la réponse.
///
/// Un seul chemin pour les deux cas : une commande hors liste est un lot d'une
/// commande avec `avec_ok` faux. C'est ce qui garantit qu'une liste répond
/// exactement comme la suite des commandes qu'elle contient, à `list_OK` près.
///
/// `lignes` n'est là que pour `idle` : c'est la seule issue qui a besoin de
/// continuer à lire (le `noidle` qui l'annule) avant d'avoir répondu.
async fn executer(
    lignes: &mut LecteurBorne,
    ecriture: &mut OwnedWriteHalf,
    etat: &EtatPartage,
    cmd_tx: &mpsc::Sender<InputMessage>,
    lot: &[Vec<String>],
    avec_ok: bool,
) -> Result<Suite> {
    let mut sortie: Vec<String> = Vec::new();
    for (indice, args) in lot.iter().enumerate() {
        // **Un seul instantané, lu avant `traiter`.** Les compteurs qu'il
        // porte sont ceux qu'un `idle` mémorise, et les lire dans la même
        // prise de verrou que l'état publié est ce qui rend le réveil manqué
        // impossible : tout ce qui bouge après cette lecture est
        // nécessairement un changement que ce client n'a pas encore vu. Les
        // lire *après* `traiter` (ou dans une seconde prise de verrou)
        // rouvrirait exactement la course que l'état partagé a été bâti pour
        // fermer.
        let instantane = etat.lire().await;
        let vues = instantane.versions;
        match traiter(&instantane, indice, args) {
            Issue::Repondre { lignes: rendues, cmds } => {
                for cmd in &cmds {
                    // **Pousser d'abord, acter ensuite.** Le canal peut
                    // refuser (plein, ou moitié `input` morte) et acter une
                    // bascule qu'on n'a pas émise serait pire que ne pas
                    // l'acter : `status` mentirait jusqu'à la trame suivante,
                    // et un `idle` réveillé annoncerait un changement qui n'a
                    // pas eu lieu.
                    //
                    // `held` faux, jamais autre chose : `held` dit « touche
                    // maintenue enfoncée », une notion de clavier que le
                    // réseau n'a pas.
                    let message = InputMessage { cmd: cmd.clone(), held: false };
                    if cmd_tx.send(message).await.is_err() {
                        // La moitié `input` est morte : plus rien de ce que
                        // dit ce client ne peut aboutir, donc le laisser
                        // parler serait lui mentir.
                        tracing::warn!("mpd input channel closed; closing session");
                        return Ok(Suite::Fermer);
                    }
                }
                etat.acter_optimiste(&cmds).await;
                sortie.extend(rendues);
                if avec_ok {
                    sortie.push("list_OK".to_string());
                }
            }
            // `noidle` reçu hors attente : `OK` sec, et dans une liste un
            // `list_OK` comme n'importe quelle commande sans lignes.
            Issue::Annuler => {
                if avec_ok {
                    sortie.push("list_OK".to_string());
                }
            }
            // La première erreur produit son `ACK` et **rien de ce qui suit
            // n'est exécuté** : le `for` s'arrête là. Les lignes déjà
            // composées partent quand même, comme le fait MPD — un `ACK` ne
            // rétracte pas les réponses des commandes qui, elles, ont abouti.
            Issue::Refuser(refus) => {
                sortie.push(refus);
                ecrire(ecriture, &sortie).await?;
                return Ok(Suite::Continuer);
            }
            Issue::Attendre(sujets) => {
                // `idle` n'atteint jamais ce point dans une liste : la liste
                // l'a refusé à l'accumulation. Hors liste, le lot n'a qu'une
                // commande, donc `sortie` est vide — l'écrire quand même
                // garde cette fonction juste si un jour un lot en contenait
                // plusieurs, plutôt que d'avaler des lignes.
                ecrire(ecriture, &sortie).await?;
                return attendre_idle(lignes, ecriture, etat, &sujets, vues).await;
            }
            Issue::Fermer => {
                // **`OK` puis fermeture, et c'est un choix.** MPD, lui,
                // n'écrit rien avant de fermer sur `close`. Nous répondons,
                // pour que la discipline de cette fonction n'ait aucune
                // exception : toute commande acceptée reçoit exactement un
                // terminateur. Un client qui a déjà cessé de lire fait
                // simplement échouer cette écriture, ce que la session traite
                // comme une fin ordinaire — et un client qui lit encore
                // trouve sa réponse là où il l'attend. La divergence est sans
                // effet observable puisque la connexion se ferme dans les deux
                // cas ; ce qui compte est qu'elle soit délibérée.
                sortie.push("OK".to_string());
                ecrire(ecriture, &sortie).await?;
                return Ok(Suite::Fermer);
            }
        }
    }
    // Un seul `OK` clôt le lot entier : c'est ce qui distingue une liste de
    // commandes de la même suite de commandes envoyées une par une.
    sortie.push("OK".to_string());
    ecrire(ecriture, &sortie).await?;
    Ok(Suite::Continuer)
}

/// Tient l'attente d'un `idle` : rend la main au réveil, ou sur ce que le
/// client dit entre-temps.
///
/// **`sujets` vide veut dire attendre pour toujours**, et non répondre `OK`
/// tout de suite (voir la doc d'`Issue::Attendre`) : un client qui n'a nommé
/// que des sous-systèmes que ce greffon n'émet jamais (`idle database`) a posé
/// une question légitime dont la réponse est le silence. C'est `attendre` qui
/// l'honore sans cas particulier — aucun sujet ne peut différer, donc elle se
/// rendort à chaque notification — et lui passer la liste telle quelle est tout
/// ce qu'il y a à faire. Répondre `OK` ferait boucler le client à pleine
/// vitesse, ce qui est exactement le contraire de ce qu'`idle` sert à éviter.
async fn attendre_idle(
    lignes: &mut LecteurBorne,
    ecriture: &mut OwnedWriteHalf,
    etat: &EtatPartage,
    sujets: &[Sujet],
    vues: [u64; 4],
) -> Result<Suite> {
    // Deux issues, et il faut écouter les deux : le réveil, et la seule
    // commande que MPD autorise pendant une attente.
    // `LecteurBorne::ligne_suivante` est sûre à l'annulation (son tampon vit
    // dans la structure, voir là-bas), donc la branche perdante ne perd aucun
    // octet ; et abandonner `attendre` ne perd aucun réveil, puisque `vues`
    // reste la référence et que les compteurs sont monotones.
    let bouges = tokio::select! {
        bouges = etat.attendre(sujets, vues) => bouges,
        lue = lignes.ligne_suivante() => {
            let Some(brute) = lue? else {
                // Le client est parti pendant son attente : rien à écrire.
                return Ok(Suite::Fermer);
            };
            let mot = match decouper(&brute) {
                Ok(args) => args.first().cloned().unwrap_or_default(),
                Err(code) => {
                    // La ligne illisible tient lieu de réponse à l'`idle` :
                    // une requête, un terminateur, la comptabilité du client
                    // reste juste.
                    let refus = ack(code, 0, premier_mot(&brute), "invalid argument");
                    ecrire(ecriture, &[refus]).await?;
                    return Ok(Suite::Continuer);
                }
            };
            if mot == "noidle" {
                // L'attente est annulée et c'est tout : `OK` sec, aucun
                // `changed:`. Un client qui vient de faire autre chose de son
                // côté relira l'état lui-même.
                ecrire(ecriture, &["OK".to_string()]).await?;
            } else {
                // Toute autre commande pendant une attente : MPD **ferme** la
                // connexion. Nous refusons sans fermer, par cohérence avec le
                // reste de cette session (une ligne fautive n'est jamais une
                // rupture) : l'`ACK` répond à l'`idle` resté en suspens, donc
                // le client garde une requête pour une réponse et peut
                // continuer à parler. Fermer lui coûterait une reconnexion
                // pour un défaut de son côté que le journal ne lui montre pas.
                let refus = ack(Ack::Unknown, 0, &mot, "not allowed during idle");
                ecrire(ecriture, &[refus]).await?;
            }
            return Ok(Suite::Continuer);
        }
    };
    let mut reponse: Vec<String> =
        bouges.iter().map(|sujet| ligne("changed", nom_sujet(*sujet))).collect();
    reponse.push("OK".to_string());
    ecrire(ecriture, &reponse).await?;
    Ok(Suite::Continuer)
}

/// Le nom MPD d'un sous-système, tel qu'un `changed:` le publie.
///
/// C'est l'inverse exact de la table que `commandes.rs` emploie pour lire un
/// `idle` : un nom qui divergerait ferait annoncer un sous-système qu'aucun
/// client ne saurait redemander. Un test le vérifie en passant chacun de ces
/// noms à `idle` et en exigeant qu'il en ressorte le même sujet.
fn nom_sujet(sujet: Sujet) -> &'static str {
    match sujet {
        Sujet::Player => "player",
        Sujet::Mixer => "mixer",
        Sujet::Playlist => "playlist",
        Sujet::StoredPlaylist => "stored_playlist",
    }
}

/// Le premier mot d'une ligne que `decouper` a refusée, pour nommer la commande
/// dans l'`ACK`. Découpé à l'espace et sans guillemets : c'est tout ce qu'on
/// peut dire d'une ligne mal citée, et un `{}` vide (ce que MPD écrit) laisse
/// le client sans indice sur laquelle de ses lignes était fautive.
fn premier_mot(brute: &str) -> &str {
    brute.split_whitespace().next().unwrap_or("")
}

/// Écrit une réponse d'un seul coup.
///
/// Un `write_all` par réponse et non un par ligne : une réponse de 51 lignes
/// coûte alors un appel système au lieu de 51, et rien ne peut s'intercaler au
/// milieu — deux réponses de la même session sont écrites l'une après l'autre
/// par construction, mais une réponse à moitié écrite serait lue comme une
/// réponse complète par un client qui compte ses terminateurs.
async fn ecrire(ecriture: &mut OwnedWriteHalf, lignes: &[String]) -> Result<()> {
    let mut tampon = String::new();
    for l in lignes {
        tampon.push_str(l);
        tampon.push('\n');
    }
    ecriture.write_all(tampon.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::etat::Instantane;
    use ritornello_proto::{Command, PlayerState};
    // Le lecteur borné de la session n'en a plus besoin ; le client de test,
    // lui, lit des lignes sans plafond à défendre.
    use tokio::io::Lines;

    /// Un client de test : les lignes reçues d'un côté, la plume de l'autre.
    struct Client {
        lignes: Lines<BufReader<OwnedReadHalf>>,
        ecriture: OwnedWriteHalf,
    }

    impl Client {
        async fn envoyer(&mut self, ligne: &str) {
            self.ecriture.write_all(format!("{ligne}\n").as_bytes()).await.unwrap();
        }

        async fn recevoir(&mut self) -> String {
            self.lignes.next_line().await.unwrap().expect("le serveur a ferme la connexion")
        }

        /// Lit jusqu'au terminateur inclus : `OK` ou un `ACK`. `list_OK` n'en
        /// est pas un — c'est ce qui permet de compter les deux.
        async fn reponse(&mut self) -> Vec<String> {
            let mut recues = Vec::new();
            loop {
                let l = self.recevoir().await;
                let fin = l == "OK" || l.starts_with("ACK ");
                recues.push(l);
                if fin {
                    return recues;
                }
            }
        }
    }

    struct Serveur {
        adresse: std::net::SocketAddr,
        etat: Arc<EtatPartage>,
    }

    /// Lie l'écouteur **dans le test** et le donne au serveur, comme
    /// `register.rs` le fait pour ses sockets Unix : l'écouteur existe donc
    /// avant que le client ne se connecte, et aucune boucle de reprise ni
    /// aucun délai n'est nécessaire pour que le `connect` aboutisse.
    async fn serveur() -> (Serveur, mpsc::Receiver<InputMessage>) {
        let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adresse = ecoute.local_addr().unwrap();
        let etat = Arc::new(EtatPartage::default());
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(accepter(ecoute, etat.clone(), tx));
        (Serveur { adresse, etat }, rx)
    }

    impl Serveur {
        async fn client(&self) -> Client {
            let flux = TcpStream::connect(self.adresse).await.unwrap();
            let (lecture, ecriture) = flux.into_split();
            Client { lignes: BufReader::new(lecture).lines(), ecriture }
        }

        /// Un client dont la bannière est déjà avalée.
        async fn client_pret(&self) -> Client {
            let mut c = self.client().await;
            let banniere = c.recevoir().await;
            assert!(banniere.starts_with("OK MPD "), "banniere inattendue: {banniere}");
            c
        }
    }

    /// Une trame qui ne bouge que `mixer`.
    fn trame_mixer(volume: u8) -> PlayerState {
        PlayerState { volume, ..Default::default() }
    }

    /// Une trame qui bouge `player` **et** `mixer` : la position déplace l'un,
    /// le volume l'autre.
    fn trame_player_et_mixer(v: u8) -> PlayerState {
        PlayerState { volume: v, position_s: Some(u32::from(v)), ..Default::default() }
    }

    /// Lit la réponse d'un dormeur en poussant des trames jusqu'à ce qu'elle
    /// arrive.
    ///
    /// **Sans horloge et sans compte d'itérations**, les deux formes de marge
    /// que ce dépôt a apprises à ne plus écrire : la boucle s'arrête quand le
    /// dormeur répond, et une implémentation qui ne réveille jamais fait
    /// *pendre* le test — un blocage franc, pas un passage douteux.
    ///
    /// La répétition n'est pas de la superstition : rien n'ordonne
    /// l'inscription du dormeur avec la première trame. Une trame appliquée
    /// avant que la session n'ait lu sa ligne `idle` est comprise dans les
    /// compteurs qu'elle mémorise, donc invisible pour elle — c'est le contrat
    /// d'`attendre`, et un test qui n'en pousserait qu'une se bloquerait sur
    /// cette course au lieu de la mesurer.
    async fn reponse_sous_trames(
        client: &mut Client,
        etat: &EtatPartage,
        trames: [PlayerState; 2],
    ) -> Vec<String> {
        let mut i = 0usize;
        let premiere = loop {
            tokio::select! {
                // `biased` : dès qu'une ligne est là, on la prend plutôt que
                // de pousser une trame de plus.
                biased;
                lue = client.lignes.next_line() => {
                    break lue.unwrap().expect("le serveur a ferme la connexion");
                }
                () = etat.appliquer_etat(trames[i % 2].clone()) => {
                    i += 1;
                    tokio::task::yield_now().await;
                }
            }
        };
        let mut recues = vec![premiere];
        while recues.last().map(String::as_str) != Some("OK") {
            recues.push(client.recevoir().await);
        }
        recues
    }

    #[tokio::test]
    async fn la_banniere_arrive_sans_quon_demande_rien() {
        let (s, _rx) = serveur().await;
        let mut c = s.client().await;
        let banniere = c.recevoir().await;
        // Comparée à la chaîne littérale et non à `VERSION_ANNONCEE` : contre
        // la constante, ce test ne vérifierait que la mise en forme, alors que
        // c'est le **numéro** qui décide des capacités qu'un client s'autorise.
        // Le changer doit être un geste conscient, pas un effet de bord.
        assert_eq!(banniere, "OK MPD 0.23.5");
    }

    #[tokio::test]
    async fn une_commande_rend_ses_lignes_puis_ok() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("status").await;
        let recues = c.reponse().await;
        assert_eq!(*recues.last().unwrap(), "OK");
        assert!(recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");
        assert!(recues.iter().any(|l| l.starts_with("state: ")), "{recues:?}");
    }

    #[tokio::test]
    async fn une_liste_de_commandes_ne_rend_quun_seul_ok() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("command_list_begin").await;
        c.envoyer("status").await;
        c.envoyer("status").await;
        c.envoyer("command_list_end").await;
        let recues = c.reponse().await;
        let ok = recues.iter().filter(|l| *l == "OK").count();
        assert_eq!(ok, 1, "un seul OK clot la liste: {recues:?}");
        // Et les deux commandes ont bien été exécutées : sans ça, « un seul
        // OK » serait aussi vrai d'une liste qui n'exécute rien.
        assert_eq!(recues.iter().filter(|l| l.starts_with("volume: ")).count(), 2, "{recues:?}");
    }

    #[tokio::test]
    async fn command_list_ok_begin_insere_un_list_ok_par_commande() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("command_list_ok_begin").await;
        c.envoyer("status").await;
        c.envoyer("ping").await;
        c.envoyer("command_list_end").await;
        let recues = c.reponse().await;
        assert_eq!(recues.iter().filter(|l| *l == "list_OK").count(), 2, "{recues:?}");
        assert_eq!(*recues.last().unwrap(), "OK");
        // Le `list_OK` d'une commande sans lignes (`ping`) est le dernier
        // avant le `OK` : c'est ce qui permet à un client de recoller chaque
        // réponse à sa commande, y compris les vides.
        assert_eq!(recues[recues.len() - 2], "list_OK", "{recues:?}");
    }

    #[tokio::test]
    async fn une_erreur_dans_une_liste_interrompt_la_suite() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("command_list_begin").await;
        c.envoyer("status").await;
        c.envoyer("nawak").await;
        c.envoyer("status").await;
        c.envoyer("command_list_end").await;
        let recues = c.reponse().await;
        // Le compte de lignes, et non une attente : le troisième `status`
        // n'ayant pas été exécuté, il n'y a qu'un seul `volume:`. Deux
        // signifieraient que la liste a continué après l'erreur.
        assert_eq!(recues.iter().filter(|l| l.starts_with("volume: ")).count(), 1, "{recues:?}");
        assert_eq!(*recues.last().unwrap(), "ACK [5@1] {nawak} unsupported", "{recues:?}");
        assert!(!recues.iter().any(|l| l == "OK"), "un ACK remplace le OK: {recues:?}");
        // La connexion survit à l'erreur : le client suivant n'a pas à se
        // reconnecter, et l'état de liste a bien été rendu (sinon `ping`
        // serait accumulé sans réponse et cette lecture pendrait).
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn idle_ne_repond_quau_changement() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle").await;
        let recues = reponse_sous_trames(&mut c, &s.etat, [trame_mixer(17), trame_mixer(18)]).await;
        // Le réveil nomme le sous-système et lui seul, puis clôt par `OK`.
        assert_eq!(recues, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn idle_filtre_les_sujets_demandes() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle player").await;
        // Chaque trame bouge `player` **et** `mixer` : le réveil est donc
        // certain (aucune course à arbitrer), et le filtre se mesure à ce que
        // la réponse *ne* nomme *pas*. Une session qui aurait ignoré la liste
        // demandée écrirait ici deux `changed:`.
        let recues = reponse_sous_trames(
            &mut c,
            &s.etat,
            [trame_player_et_mixer(17), trame_player_et_mixer(18)],
        )
        .await;
        assert_eq!(recues, vec!["changed: player".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn noidle_rend_la_main_immediatement() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle").await;
        c.envoyer("noidle").await;
        c.envoyer("status").await;
        // Un `OK` sec, et surtout **rien avant lui** : c'est la preuve sans
        // horloge qu'`idle` ne répond pas de lui-même. S'il avait répondu sans
        // qu'aucune trame ne bouge, la première ligne lue serait un
        // `changed:`.
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        // Et ce `status` est là pour compter les réponses sans horloge : la
        // deuxième doit être **la sienne**. Une session qui aurait rendu un
        // `OK` de complaisance à l'`idle` (au lieu d'attendre) aurait glissé
        // une réponse de plus dans le flux, et on lirait ici le `OK` du
        // `noidle` au lieu des lignes du `status`. Sans cette moitié, le test
        // passait aussi bien avec un `idle` qui répond tout de suite —
        // vérifié, et c'est ce qui l'a fait réécrire.
        let apres = c.reponse().await;
        assert!(apres.iter().any(|l| l.starts_with("volume: ")), "{apres:?}");
        // Et rien n'a bougé dans l'état : `noidle` annule une attente, il ne
        // publie pas de changement.
        assert_eq!(s.etat.lire().await.versions, [0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn une_commande_pendant_une_attente_est_refusee_sans_fermer() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle").await;
        c.envoyer("status").await;
        // MPD fermerait ; ici l'`ACK` répond à l'`idle` en suspens et la
        // connexion continue. Le choix est écrit sur place dans
        // `attendre_idle`.
        assert_eq!(c.reponse().await, vec!["ACK [5@0] {status} not allowed during idle".to_string()]);
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn deux_clients_ne_se_genent_pas() {
        // LE test de l'architecture. Si `accepter` servait les connexions l'une
        // après l'autre au lieu d'une tâche par client, B n'obtiendrait même
        // pas sa bannière tant que A dort, et ce test se **bloquerait** — le
        // mode d'échec franc que ce chantier préfère à une marge d'horloge.
        let (s, _rx) = serveur().await;
        let mut a = s.client_pret().await;
        a.envoyer("idle").await;

        let mut b = s.client_pret().await;
        b.envoyer("status").await;
        let recues = b.reponse().await;
        assert_eq!(*recues.last().unwrap(), "OK", "{recues:?}");
        assert!(recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");

        // Et A dormait vraiment : sans cette moitié, le test passerait aussi
        // avec un A dont la session est morte — le réveil prouve qu'elle était
        // vivante et en attente pendant que B se faisait servir.
        let reveil = reponse_sous_trames(&mut a, &s.etat, [trame_mixer(17), trame_mixer(18)]).await;
        assert_eq!(reveil, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn une_commande_daction_arrive_sur_le_canal_dentree() {
        let (s, mut rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("next").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.cmd, Command::Next);
        assert!(!msg.held, "une commande reseau n'est jamais maintenue");
        // Exactement une : une commande dupliquée ferait sauter deux stations.
        assert!(rx.try_recv().is_err(), "une seule commande pour un seul next");
    }

    #[tokio::test]
    async fn une_lecture_seule_nemet_rien_sur_le_canal() {
        let (s, mut rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("status").await;
        c.reponse().await;
        // La réponse est arrivée, donc la commande est entièrement traitée :
        // si `status` avait émis quoi que ce soit, ce serait déjà dans le
        // canal. Aucune horloge n'est nécessaire pour l'affirmer.
        assert!(rx.try_recv().is_err(), "status ne demande rien a l'appareil");
    }

    #[tokio::test]
    async fn un_canal_ferme_ferme_la_session_sans_acter_la_bascule() {
        // L'ordre « pousser puis acter » se mesure ici et nulle part ailleurs :
        // le canal refuse (récepteur lâché), donc rien n'a été émis, donc rien
        // ne doit avoir été acté. Une session qui appellerait
        // `acter_optimiste` d'abord poserait le volume 30 dans l'état partagé
        // et le ferait publier par `status` à tous les autres clients — une
        // bascule que le cœur n'a jamais reçue.
        let (s, rx) = serveur().await;
        drop(rx);
        let mut c = s.client_pret().await;
        c.envoyer("setvol 30").await;
        assert!(
            c.lignes.next_line().await.unwrap().is_none(),
            "une moitie input morte ferme la session"
        );
        assert_eq!(s.etat.lire().await.etat.volume, 0, "rien ne s'acte si le canal a refuse");
        assert_eq!(s.etat.lire().await.versions, [0, 0, 0, 0], "et personne n'est reveille");
    }

    #[tokio::test]
    async fn idle_dans_une_liste_est_refuse_a_son_rang() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("command_list_begin").await;
        c.envoyer("status").await;
        c.envoyer("idle").await;
        let recues = c.reponse().await;
        // L'indice est le rang d'`idle` dans la liste (1), pas 0 : un client
        // qui groupe dix commandes doit savoir laquelle a été refusée.
        assert_eq!(recues, vec!["ACK [5@1] {idle} not allowed in command list".to_string()]);
        // Refusé **à l'accumulation** : le `status` qui précède n'a pas été
        // exécuté, donc aucune ligne `volume:` n'accompagne l'ACK.
        assert!(!recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");
        // Et l'état de liste a été rendu : la commande suivante répond seule.
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_liste_sans_fin_est_bornee() {
        // Une liste s'accumule en mémoire sans rien exécuter : sans plafond, un
        // client qui n'envoie jamais son `command_list_end` fait croître un
        // `Vec` jusqu'à l'épuisement de la mémoire d'un Pi. Le refus arrive au
        // rang du plafond et rend l'état de liste.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        let mut lot = String::from("command_list_begin\n");
        for _ in 0..=MAX_COMMANDES_LISTE {
            lot.push_str("ping\n");
        }
        c.ecriture.write_all(lot.as_bytes()).await.unwrap();
        let recues = c.reponse().await;
        assert_eq!(
            recues,
            vec![format!("ACK [5@{MAX_COMMANDES_LISTE}] {{ping}} list too large")],
            "{recues:?}"
        );
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_ligne_plus_longue_que_le_plafond_ferme_la_connexion() {
        // La dernière surface non bornée du greffon, et elle est atteignable
        // sans mot de passe depuis tout le réseau local : un client qui envoie
        // des octets sans jamais envoyer de `\n`. Sans plafond, la session
        // accumule jusqu'à ce que l'allocateur renonce — sur un Pi d'un
        // gigaoctet partagé avec mpv, cela emporte la musique et pas seulement
        // le greffon.
        //
        // Sans horloge : la borne se mesure au fait que la connexion **finit**.
        // Sans plafond, ce `next_line` attendrait le `\n` pour toujours et le
        // test pendrait — vérifié, et c'est le mode d'échec voulu.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        let bourrage = vec![b'a'; MAX_LIGNE + 1];
        // L'écriture peut échouer si le serveur a déjà fermé : c'est une fin
        // acceptable et non un échec du test, d'où le résultat ignoré.
        let _ = c.ecriture.write_all(&bourrage).await;
        assert!(
            c.lignes.next_line().await.unwrap().is_none(),
            "une ligne au-dela du plafond ferme la connexion, sans ACK"
        );
    }

    #[tokio::test]
    async fn une_ligne_longue_mais_sous_le_plafond_est_traitee() {
        // Le pendant du test précédent : un plafond qui coupe une ligne
        // légitime serait pire que pas de plafond. La plus longue ligne
        // plausible du protocole est un nom entre guillemets, et elle doit
        // arriver entière — ici elle mesure exactement le plafond.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        let nom = "a".repeat(MAX_LIGNE - "load \"\"".len());
        c.envoyer(&format!("load \"{nom}\"")).await;
        // `load` refuse tout nom faute de catalogue (Task 13), et c'est
        // justement une réponse qui prouve que la ligne a été **découpée** :
        // un `ACK 2` ou une fermeture diraient qu'elle a été tronquée.
        assert_eq!(c.reponse().await, vec!["ACK [50@0] {load} no such playlist".to_string()]);
    }

    #[tokio::test]
    async fn une_ligne_terminee_par_crlf_est_lue_sans_le_retour_chariot() {
        // Les clients écrits sur Windows terminent par `\r\n`. Le lecteur
        // écrit à la main devait reprendre ce que `Lines` faisait pour nous, et
        // rien ne le disait : sans le `\r` retiré, la commande serait `ping\r`,
        // donc un `ACK 5` — une régression qu'aucun test existant ne voyait.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.ecriture.write_all(b"ping\r\n").await.unwrap();
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_derniere_ligne_sans_fin_de_ligne_est_traitee_avant_la_fermeture() {
        // Un client qui envoie sa commande puis ferme sa moitié écriture doit
        // la voir traitée : la fin de flux termine la ligne. C'est ce que
        // faisait `Lines`, et le chemin « tampon non vide à l'EOF » du nouveau
        // lecteur n'a pas d'autre témoin que ce test.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.ecriture.write_all(b"ping").await.unwrap();
        // `shutdown` et non un `drop` : la moitié lecture du client doit rester
        // ouverte pour lire la réponse.
        c.ecriture.shutdown().await.unwrap();
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_ligne_illisible_ne_ferme_pas_la_connexion() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer(r#"load "France"#).await;
        let recues = c.reponse().await;
        assert_eq!(recues, vec!["ACK [2@0] {load} invalid argument".to_string()]);
        // Le client suivant n'a pas à se reconnecter.
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_ligne_illisible_dans_une_liste_labandonne() {
        // La liste ne peut plus être exécutée telle que le client l'a écrite :
        // l'exécuter au `command_list_end` exécuterait un lot amputé de la
        // commande refusée. Elle est donc abandonnée, et le
        // `command_list_end` qui suit est refusé comme une commande hors
        // liste — un client sait alors que son lot n'a pas eu lieu.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("command_list_begin").await;
        c.envoyer("status").await;
        c.envoyer(r#"load "France"#).await;
        assert_eq!(c.reponse().await, vec!["ACK [2@1] {load} invalid argument".to_string()]);
        c.envoyer("command_list_end").await;
        assert_eq!(
            c.reponse().await,
            vec!["ACK [5@0] {command_list_end} unsupported".to_string()]
        );
    }

    #[tokio::test]
    async fn close_repond_ok_puis_ferme() {
        // Décision assumée : MPD n'écrit rien avant de fermer, nous répondons.
        // Voir le commentaire d'`Issue::Fermer` dans `executer`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("close").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        assert!(c.lignes.next_line().await.unwrap().is_none(), "close doit fermer");
    }

    #[tokio::test]
    async fn les_noms_de_sujets_sont_ceux_quidle_accepte() {
        // `nom_sujet` est l'inverse de la table de `commandes.rs`, et rien ne
        // relie les deux au compilateur : un `stored-playlist` au tiret ici
        // ferait annoncer un sous-système qu'aucun client ne saurait
        // redemander. Le vérifier en passant chaque nom à `idle`.
        for sujet in [Sujet::Player, Sujet::Mixer, Sujet::Playlist, Sujet::StoredPlaylist] {
            let args = vec!["idle".to_string(), nom_sujet(sujet).to_string()];
            assert_eq!(
                traiter(&Instantane::default(), 0, &args),
                Issue::Attendre(vec![sujet]),
                "nom_sujet({sujet:?}) n'est pas un nom qu'idle accepte"
            );
        }
    }

    #[tokio::test]
    async fn un_idle_sans_sujet_connu_nest_pas_un_ok_immediat() {
        // `idle database` ne nomme que des sous-systèmes que ce greffon
        // n'émet jamais : la liste de sujets est vide, et le contrat
        // d'`Issue::Attendre` dit que c'est une attente **sans fin**, pas un
        // `OK` immédiat. Un `OK` ferait boucler le client à pleine vitesse
        // sur la seule commande faite pour l'en dispenser.
        //
        // Prouvé sans horloge, par ce que la ligne suivante devient : tant que
        // l'attente tient, une commande autre que `noidle` est refusée. Une
        // session qui aurait rendu `OK` tout de suite ne serait plus en
        // attente, et ce `status` répondrait `volume: …` puis `OK`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle database").await;
        // Des trames qui bougent tous les compteurs : aucune ne concerne les
        // sujets demandés (il n'y en a aucun), donc aucune ne doit réveiller.
        s.etat.appliquer_etat(trame_player_et_mixer(17)).await;
        s.etat.appliquer_etat(trame_player_et_mixer(18)).await;
        c.envoyer("status").await;
        assert_eq!(
            c.reponse().await,
            vec!["ACK [5@0] {status} not allowed during idle".to_string()]
        );
        // L'attente est retombée avec ce refus : la connexion reste utilisable.
        c.envoyer("noidle").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }
}
