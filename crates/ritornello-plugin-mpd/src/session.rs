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

use crate::commandes::{
    pochette_annoncee_mais_absente, traiter, Binaire, Issue, MAX_TRANCHE,
    MAX_TRANCHE_PLAFOND,
};
use crate::etat::{EtatPartage, Instantane, Sujet};
use crate::protocole::{ack, decouper, ligne, Ack};
use anyhow::Result;
use ritornello_proto::InputMessage;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Semaphore};

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

/// Plafond de **sessions simultanées**.
///
/// Le multiplicateur de tout ce qui suit : chaque plafond ci-dessous borne une
/// connexion, et rien ne bornait le nombre de connexions. Or le résidu réel
/// d'une session peut atteindre une dizaine de mébioctets (voir `MAX_REPONSE`
/// pour le calcul), donc cent sessions font le gigaoctet de l'appareil — la
/// panne que tous ces plafonds existent pour éviter, atteinte par le seul
/// chemin qu'ils laissaient ouvert.
///
/// **16, justifié par la population réelle** : un téléphone, un deuxième
/// téléphone, `mpc` sur l'appareil, à la rigueur une tablette et un client de
/// bureau — cinq au grand maximum, et les clients MPD n'ouvrent qu'une
/// connexion chacun (une seconde parfois, pour tenir un `idle` à part). 16
/// laisse donc trois fois la marge de tout usage légitime, tout en bornant le
/// pire cas à un peu moins de 200 Mio là où il était sans borne.
///
/// **Et ce n'est pas une protection contre la seule malveillance** : un client
/// qui fuit ses connexions — qui en rouvre une à chaque reprise de réseau sans
/// fermer la précédente — y arrive par accident, et c'est même le cas le plus
/// probable des deux. Le refus est alors ce qui garde l'appareil en vie pendant
/// que ce client se comporte mal, et le journal nomme le plafond pour que la
/// cause se lise sans deviner.
///
/// Un plafond et non une file d'attente : faire patienter une connexion
/// derrière un plafond atteint garderait un descripteur ouvert et laisserait le
/// client croire qu'il est servi. Refuser aussitôt lui dit la vérité, et c'est
/// une réponse qu'un client sait interpréter — un serveur MPD injoignable est
/// un état que tous savent afficher.
///
/// **Les 200 Mio ci-dessus ne comptent que le chemin texte, et il faut le dire
/// ici** : depuis les pochettes, ce plafond multiplie aussi
/// `COVER_MAX_BYTES`. Une session qui répond `albumart` retient la génération
/// d'image qu'elle sert pendant tout son `write_all`, donc seize clients
/// immobiles épinglent seize générations — 16 × 20 Mio = **320 Mio**, plus celle
/// que l'état tient lui-même, soit **340 Mio**. Le calcul complet et ce qui
/// n'est pas mitigé sont sur `commandes::MAX_TRANCHE` ; ce qu'il faut retenir
/// ici est que ce plafond-ci est le seul facteur qui borne ce produit.
const MAX_SESSIONS: usize = 16;

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

/// Plafond des **octets** accumulés par une liste de commandes.
///
/// Le compte de commandes ne suffit pas : une ligne accumulée peut peser
/// jusqu'à `MAX_LIGNE` en toute légitimité, donc 2048 commandes bornent la
/// mémoire à 16 Mio par connexion — l'ordre de grandeur même que `MAX_LIGNE`
/// existe pour interdire. C'est d'ailleurs en octets, et non en commandes, que
/// MPD exprime la sienne (`max_command_list_size`, 2 Mio par défaut).
///
/// 256 Kio, soit 2048 commandes de 128 octets en moyenne : un `setvol 30` en
/// pèse dix, et la plus longue commande réaliste — un nom entre guillemets — en
/// pèse quelques centaines. Très au-dessus de ce qu'un client envoie.
///
/// **Ce plafond compte des octets de texte, pas des octets de tas**, et l'écart
/// n'est pas cosmétique : ce qui est accumulé est un `Vec<Vec<String>>`, et
/// `decouper` alloue une chaîne **par jeton**. Une ligne légale de 8 Kio faite
/// de `"a a a a …"` devient ainsi ~4096 `String` d'un caractère, chacune coûtant
/// ses 24 octets de structure dans le `Vec` plus une allocation que l'allocateur
/// arrondit — de l'ordre de 50 octets pour un caractère utile, soit un facteur
/// proche de trente. 256 Kio comptés peuvent donc peser plusieurs mébioctets
/// réels. Le plafond n'en reste pas moins un plafond ; c'est son unité qu'il ne
/// faut pas confondre avec de la mémoire, et `MAX_SESSIONS` est ce qui borne le
/// produit.
const MAX_OCTETS_LISTE: usize = 256 * 1024;

/// Plafond des **octets** d'une réponse, avant l'écriture.
///
/// C'est la même fuite que `MAX_LIGNE` prise par l'autre bout, et le plafond de
/// commandes d'une liste ne la borne pas du tout : il borne les commandes, pas
/// ce qu'elles **produisent**. Une liste de 2048 `playlistinfo` — 26 Kio
/// d'entrée, une boucle, aucune malveillance — rend quatre lignes par entrée de
/// file, soit jusqu'à 1020 lignes par commande à `preset_count` maximal (255) :
/// deux millions de `String` d'un côté, et surtout **une allocation contiguë de
/// plusieurs dizaines de mébioctets** au moment de mettre tout cela à plat pour
/// le `write_all`. Sur un Pi 2 B, une demande contiguë de cette taille échoue
/// contre une mémoire fragmentée bien avant que le total ne soit atteint.
///
/// 1 Mio : la plus longue réponse légitime est un `playlistinfo` complet — 255
/// entrées de quatre lignes, une quinzaine de kibioctets en tout, `preset_count`
/// étant un `Option<u8>` — et le plafond en laisse donc passer une soixantaine
/// dans une seule liste.
///
/// **Ce que ce plafond borne, et ce qu'il ne borne pas.** Il est vérifié après
/// chaque commande du lot et non à chaque ligne, donc le dépassement est
/// constaté à au plus une réponse de commande près (une quinzaine de
/// kibioctets), et le bras `Issue::Annuler` empile son `list_OK` sans le
/// vérifier du tout — borné par le compte de commandes, donc 2048 × 8 octets,
/// soit 16 Kio. Le résidu au-delà du plafond est ainsi d'une trentaine de
/// kibioctets, et non « une réponse de commande ».
///
/// **Deux multiplicateurs à connaître pour recalculer ce que coûte vraiment une
/// session** — les énoncer vaut mieux que d'inscrire un nombre que le prochain
/// changement démentira :
///
/// 1. **La copie simultanée.** `ecrire` met la réponse à plat dans une `String`
///    dont il réserve la capacité exacte *pendant que* `Reponse.lignes` vit
///    encore : le texte existe donc deux fois à cet instant. Le pic **compté**
///    d'une session est ainsi ≈ 2 × 1 Mio (la réponse et sa copie) + 256 Kio
///    (la liste accumulée) ≈ 2,3 Mio, et non 1,3.
/// 2. **Octets de texte contre octets de tas.** Comme pour `MAX_OCTETS_LISTE`,
///    ces plafonds comptent du texte alors que les structures tiennent des
///    `String` : une réponse d'un mébioctet en lignes d'une vingtaine d'octets
///    est ~40 000 `String`, soit le double en tas. Bout à bout, une session
///    poussée à ses deux plafonds tient de l'ordre de **6 à 12 Mio réels**.
///
/// Le lever qui compte, si ce chiffre devenait gênant, est `MAX_OCTETS_LISTE`
/// (le terme dominant, à cause du facteur trente des jetons d'un caractère), et
/// non `MAX_SESSIONS` — mais c'est `MAX_SESSIONS` qui borne le produit.
const MAX_REPONSE: usize = 1024 * 1024;

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
/// un tampon de ligne par session, donc 128 Kio pour les `MAX_SESSIONS`
/// autorisées.
///
/// (Cette doc a porté un temps la phrase « même cent connexions simultanées ne
/// réservent ainsi qu'un mégaoctet ». Elle était vraie quand ce tampon était
/// toute l'histoire, et elle est devenue fausse d'un facteur mille dès que la
/// liste accumulée et la réponse composée ont eu leurs propres plafonds : le
/// tampon de ligne n'est plus qu'un terme mineur du résidu d'une session. Voir
/// `MAX_REPONSE` pour le calcul complet.)
const MAX_LIGNE: usize = 8 * 1024;

/// Le lecteur de lignes de la session : un `BufReader`, plus le plafond.
///
/// Écrit à la main (`fill_buf`/`consume`) plutôt qu'avec `BufReader::lines()`,
/// pour la seule raison qui vaille : `lines()` accumule jusqu'au `\n` **sans
/// borne**. Voir `MAX_LIGNE`.
struct LecteurBorne {
    lecture: BufReader<OwnedReadHalf>,
    /// La ligne lue pendant une attente `idle` et **remise en file** pour la
    /// boucle de `servir`.
    ///
    /// C'est le mécanisme du « `noidle` implicite » : une commande reçue
    /// pendant un `idle` annule l'attente (`OK` nu) *puis* doit être exécutée
    /// comme n'importe quelle autre ligne — donc repassée à l'aiguillage
    /// complet de `servir`, listes de commandes et lignes illisibles comprises,
    /// plutôt que réinterprétée à moitié dans `attendre_idle`.
    ///
    /// Une seule ligne suffit, et un seul emplacement le dit : elle est remise
    /// juste après avoir été lue, et consommée au tour de boucle suivant, donc
    /// deux ne peuvent pas coexister.
    remise: Option<String>,
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
        Self { lecture: BufReader::new(lecture), remise: None, tampon: Vec::new() }
    }

    /// Remet une ligne déjà lue devant le flux. Voir `remise`.
    fn remettre(&mut self, ligne: String) {
        debug_assert!(self.remise.is_none(), "deux lignes remises en file a la fois");
        self.remise = Some(ligne);
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
        // La ligne remise passe avant la chaussette, et **sans point
        // d'attente** : ce `take` et ce `return` sont dans le même sondage, si
        // bien qu'une annulation ne peut pas se glisser entre les deux et
        // perdre la ligne.
        if let Some(ligne) = self.remise.take() {
            return Ok(Some(ligne));
        }
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

/// Les lignes d'une réponse en cours de composition, et leur poids en octets.
///
/// Le compte est tenu au fur et à mesure plutôt que recalculé : la vérification
/// du plafond a lieu après chaque commande d'un lot, et resommer la réponse
/// entière à chaque fois rendrait quadratique la composition d'une liste
/// longue. Il compte le `\n` de chaque ligne, donc c'est exactement le nombre
/// d'octets que `ecrire` va poser sur la chaussette.
#[derive(Default)]
struct Reponse {
    lignes: Vec<String>,
    octets: usize,
}

impl Reponse {
    fn pousser(&mut self, ligne: String) {
        self.octets += ligne.len() + 1;
        self.lignes.push(ligne);
    }

    fn etendre(&mut self, lignes: Vec<String>) {
        for ligne in lignes {
            self.pousser(ligne);
        }
    }

    /// Vrai quand la réponse dépasse `MAX_REPONSE`.
    fn trop_grande(&self) -> bool {
        self.octets > MAX_REPONSE
    }
}

/// Ce qui n'appartient qu'à **une** connexion, et voyage donc avec elle.
///
/// Regroupés parce qu'ils ont exactement la même nature — deux faits sur le
/// client, que rien ne partage entre sessions — et non pour raccourcir une
/// signature : les séparer laisserait croire qu'ils ont des durées de vie
/// différentes. L'état de liste de commandes leur ressemble mais reste dans
/// `servir` : lui ne traverse jamais un appel à `executer`, puisque c'est
/// `servir` qui décide ce qu'est un lot.
struct Connexion {
    /// Les compteurs de sujets que cette connexion a déjà vus : la référence de
    /// tous ses `idle`. Lue par `executer`, avancée seulement par
    /// `attendre_idle`, et pour les seuls sujets qu'un réveil annonce.
    vues: [u64; 4],
    /// La taille de tranche que ce client accepte pour les réponses binaires
    /// (voir `commandes::binarylimit`). `MAX_TRANCHE` tant qu'il n'a rien
    /// demandé — le défaut du protocole.
    limite_binaire: usize,
}

/// Ce que la session doit faire après un lot de commandes.
enum Suite {
    /// Continuer à lire des lignes.
    Continuer,
    /// Fermer la connexion : `close`, ou une moitié `input` morte.
    Fermer,
}

/// Accepte les connexions, chacune dans sa propre tâche, et **se relie quand
/// les réglages changent**.
///
/// La page d'admin disait « le changement ne prend effet qu'au redémarrage du
/// greffon », et c'était vrai : le socket était lié une fois pour toutes dans
/// `main`. Ce n'est plus le cas — un enregistrement réussi pousse la nouvelle
/// configuration sur `config_rx`, et cette boucle lie le nouveau couple
/// adresse/port.
///
/// **Trois décisions, chacune pour une raison :**
///
/// - **L'ancien écouteur n'est lâché qu'une fois le nouveau lié.** Si le port
///   demandé est déjà pris, ou l'adresse absente de la machine, l'appareil
///   continue de servir là où il servait : un réglage fautif ne doit pas rendre
///   le serveur MPD injoignable, alors même que la page qui l'a provoqué est
///   toujours ouverte. L'échec part au journal, et la page dira l'inverse — le
///   fichier, lui, a bien été enregistré. C'est le compromis assumé : la
///   validation du port ne peut pas anticiper qu'il est occupé.
/// - **Les sessions déjà ouvertes ne sont pas coupées.** Elles tiennent leur
///   propre `TcpStream`, que la fermeture de l'écouteur ne touche pas. Un
///   téléphone en train d'écouter garde donc sa connexion jusqu'à ce qu'il la
///   ferme lui-même, là où un vrai redémarrage de MPD la lui aurait arrachée.
/// - **Le plafond de sessions traverse les reliaisons.** Le sémaphore vit ici,
///   hors de la boucle : le recréer à chaque changement de réglage rendrait
///   `MAX_SESSIONS` contournable par une simple sauvegarde répétée.
///
/// `accept` est annulable sans perte (c'est la garantie de tokio), donc perdre
/// la course du `select!` ne fait jamais tomber une connexion déjà acceptée.
pub async fn ecouter(
    ecoute: TcpListener,
    mut config_rx: tokio::sync::watch::Receiver<crate::config::Config>,
    etat: Arc<EtatPartage>,
    cmd_tx: mpsc::Sender<InputMessage>,
) {
    let places = Arc::new(Semaphore::new(MAX_SESSIONS));
    let mut ecoute = ecoute;
    loop {
        tokio::select! {
            // Ne rend jamais la main : sa seule sortie est d'être annulée par
            // l'autre bras.
            () = boucle_accept(&ecoute, &places, &etat, &cmd_tx) => {}
            change = config_rx.changed() => {
                if change.is_err() {
                    // La moitié admin a disparu (le greffon s'arrête) : plus
                    // aucune reliaison ne viendra, mais il reste à servir.
                    tracing::debug!("mpd settings channel closed; keeping the current socket");
                    boucle_accept(&ecoute, &places, &etat, &cmd_tx).await;
                    return;
                }
                let c = config_rx.borrow_and_update().clone();
                match TcpListener::bind((c.listen.as_str(), c.port)).await {
                    Ok(neuf) => {
                        tracing::info!("mpd server now listening on {}:{}", c.listen, c.port);
                        ecoute = neuf;
                    }
                    Err(e) => tracing::warn!(
                        "mpd could not listen on {}:{} ({e}); keeping the previous socket",
                        c.listen,
                        c.port
                    ),
                }
            }
        }
    }
}

/// La boucle d'acceptation elle-même. Ne rend jamais la main.
///
/// Une erreur d'`accept` est journalisée et la boucle continue : un descripteur
/// épuisé ou une connexion réinitialisée avant l'`accept` ne doit pas emporter
/// le serveur, sinon le port reste ouvert dans un processus qui n'écoute plus.
///
/// Le sémaphore des places est **passé** et non créé ici : il vit dans
/// `ecouter`, pour que le plafond de sessions traverse les reliaisons (voir sa
/// doc). Une place par session, rendue quoi qu'il arrive — le permis vit dans
/// la tâche, donc il repart avec elle, y compris si elle panique, puisque c'est
/// son `Drop` qui le rend. Un `Semaphore` plutôt qu'un compteur atomique pour
/// exactement cette raison : un compteur demanderait de se souvenir de le
/// décrémenter sur chaque chemin de sortie, et le jour où l'un serait oublié
/// l'appareil refuserait tout le monde après seize connexions.
async fn boucle_accept(
    ecoute: &TcpListener,
    places: &Arc<Semaphore>,
    etat: &Arc<EtatPartage>,
    cmd_tx: &mpsc::Sender<InputMessage>,
) {
    loop {
        match ecoute.accept().await {
            Ok((flux, adresse)) => {
                // `try_acquire_owned` et non `acquire` : au-delà du plafond la
                // connexion est **refusée**, pas mise en attente. Voir
                // `MAX_SESSIONS`.
                let Ok(place) = places.clone().try_acquire_owned() else {
                    tracing::warn!("mpd refusing {adresse}: {MAX_SESSIONS} sessions already open");
                    drop(flux);
                    continue;
                };
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
                    // Explicite, alors que la portée s'en chargerait : c'est la
                    // ligne qui dit qu'une place se libère ici, et la chercher
                    // dans une accolade fermante serait une devinette.
                    drop(place);
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

    // **Les compteurs que cette connexion a déjà vus, lus une fois pour
    // toutes.** C'est la référence de tous ses `idle`, et elle vit ici parce
    // que c'est un fait sur la **connexion** — comme l'état de liste juste en
    // dessous, et pour la même raison.
    //
    // Les lire à la bannière et les porter, plutôt que de les relire dans
    // l'`Instantane` de chaque commande `idle`, est la correction d'un défaut
    // réel : la lecture par commande avalait tout ce qui avait bougé entre la
    // réponse précédente et la ligne `idle`, c'est-à-dire pendant la seule
    // fenêtre où un client MPD n'écoute pas. Le vrai MPD accumule ses drapeaux
    // par connexion depuis la connexion ; pour `stored_playlist`, avaler un
    // événement laisse `listplaylists` périmé jusqu'au prochain changement de
    // catalogue — donc potentiellement pour toujours. Voir `etat::versions`.
    //
    // Un `idle` immédiatement réveillé sur un changement que ce client avait
    // peut-être déjà lu par un `status` est le sens d'erreur acceptable : un
    // réveil superflu lui coûte une interrogation redondante, un réveil
    // manquant lui coûte la justesse de son écran.
    let mut connexion = Connexion { vues: etat.versions().await, limite_binaire: MAX_TRANCHE };

    // Les commandes accumulées d'une liste en cours, `None` hors liste.
    // Un `Option<Vec<_>>` plutôt qu'un `Vec` plus un booléen : « pas dans une
    // liste » et « dans une liste encore vide » sont deux états différents, et
    // un `command_list_end` reçu hors liste doit être refusé comme une
    // commande inconnue plutôt que rendre un `OK` de complaisance.
    let mut liste: Option<Vec<Vec<String>>> = None;
    let mut avec_ok = false;
    // Les octets déjà accumulés par la liste en cours. Remis à zéro à chaque
    // ouverture, comme `avec_ok`.
    let mut octets_liste = 0usize;

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
                    match executer(&mut lignes, &mut ecriture, &etat, &cmd_tx, &lot, avec_ok, &mut connexion)
                        .await?
                    {
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
                // **Une réponse binaire dans une liste de commandes : MPD
                // l'autorise, ce greffon la refuse.** Trois raisons, dans
                // l'ordre où elles pèsent :
                //
                // 1. Elle romprait la discipline d'écriture de cette session.
                //    `executer` compose *tout* un lot en texte, vérifie le
                //    plafond, puis écrit **une fois** — ce qui garantit qu'une
                //    réponse à moitié écrite n'est jamais lue comme complète.
                //    Insérer des octets au milieu obligerait soit à vider
                //    l'accumulateur avant chaque image (donc à renoncer à cette
                //    garantie), soit à faire passer les octets par
                //    l'accumulateur de texte — ce qui est impossible, ils ne
                //    sont pas de l'UTF-8.
                // 2. Elle rouvrirait l'amplificateur que la Task 8 a fermé :
                //    2048 `albumart` dans une liste, c'est 26 Kio d'entrée pour
                //    16 Mio écrits, **accumulés avant la première écriture**.
                //    C'est exactement la mesure qui a fait naître
                //    `MAX_REPONSE`, sur ce même port sans authentification.
                // 3. Personne n'en a besoin. Une pochette se récupère par une
                //    suite d'allers-retours dont chaque offset dépend du `size:`
                //    que le précédent a rendu — or une liste de commandes est
                //    envoyée **entière avant** d'être lue. Le client ne peut
                //    donc pas composer le lot qu'il faudrait.
                //
                // Refus à l'accumulation, comme `idle` et pour la même raison :
                // le lot ne pourra jamais aboutir, donc exécuter d'abord les
                // commandes qui le précèdent émettrait de vraies actions au nom
                // d'un lot que le client ne verra jamais.
                "albumart" | "readpicture" => {
                    let indice = liste.as_ref().map_or(0, Vec::len);
                    liste = None;
                    let refus = ack(Ack::Unknown, indice, mot, "not allowed in command list");
                    ecrire(&mut ecriture, &[refus]).await?;
                }
                _ => {
                    let indice = liste.as_ref().map_or(0, Vec::len);
                    // Deux bornes pour un seul refus : le nombre de commandes
                    // (qui borne le travail d'un lot) et leur poids en octets
                    // (qui borne la mémoire, une ligne accumulée pouvant peser
                    // jusqu'à `MAX_LIGNE`). Voir les deux constantes.
                    octets_liste += brute.len() + 1;
                    if indice >= MAX_COMMANDES_LISTE || octets_liste > MAX_OCTETS_LISTE {
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
                octets_liste = 0;
            }
            "command_list_ok_begin" => {
                liste = Some(Vec::new());
                avec_ok = true;
                octets_liste = 0;
            }
            _ => {
                let lot = std::slice::from_ref(&args);
                match executer(&mut lignes, &mut ecriture, &etat, &cmd_tx, lot, false, &mut connexion)
                    .await?
                {
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
/// continuer à lire (le `noidle` qui l'annule, ou la commande qui la remplace)
/// avant d'avoir répondu.
///
/// `connexion` porte ce qui n'appartient qu'à ce client : la référence de
/// compteurs de ses `idle` et la taille de tranche qu'il accepte (voir
/// `Connexion`).
async fn executer(
    lignes: &mut LecteurBorne,
    ecriture: &mut OwnedWriteHalf,
    etat: &EtatPartage,
    cmd_tx: &mpsc::Sender<InputMessage>,
    lot: &[Vec<String>],
    avec_ok: bool,
    connexion: &mut Connexion,
) -> Result<Suite> {
    let Connexion { vues, limite_binaire } = connexion;
    let mut sortie = Reponse::default();
    for (indice, args) in lot.iter().enumerate() {
        // **Un seul instantané, lu avant `traiter`.** Une seule prise de
        // verrou pour tout ce que la réponse publie : la lire en deux fois
        // laisserait `status` se contredire au milieu de lui-même.
        //
        // **Ses compteurs ne servent pas de référence à un `idle`, et c'est le
        // point.** Ils décrivent l'instant de *cette* commande ; la référence
        // d'un `idle` est celle que la connexion porte depuis sa bannière. Les
        // confondre — ce que ce code faisait — avale tout changement survenu
        // entre la réponse précédente et la ligne `idle`, et le commentaire qui
        // vivait ici affirmait le contraire : rien dans cette lecture ne rend
        // « le réveil manqué impossible ». C'est la comparaison d'`attendre`
        // contre la référence de la connexion qui l'interdit.
        let mut instantane = etat.lire().await;
        // **La seule attente que ce module s'autorise avant de traiter**, et
        // elle répare la pochette qui disparaissait à chaque changement de
        // piste : voir `pochette_annoncee_mais_absente` et `attendre_pochette`.
        if pochette_annoncee_mais_absente(&instantane, args) {
            instantane = attendre_pochette(etat, instantane).await;
        }
        match traiter(&instantane, indice, args, *limite_binaire) {
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
                sortie.etendre(rendues);
                if avec_ok {
                    sortie.pousser("list_OK".to_string());
                }
                // Le plafond de réponse, vérifié ici parce que c'est le seul
                // endroit où la réponse grandit. Rien n'a encore été écrit, donc
                // le refus **remplace** tout ce qui était composé : le client
                // reçoit un seul terminateur pour sa requête, sa comptabilité
                // reste juste, et la connexion survit — contrairement à la ligne
                // trop longue, où fermer était le seul choix défendable puisqu'on
                // ne pouvait même pas nommer la commande fautive. Ici on la
                // nomme, et son rang avec elle.
                //
                // **Ce que le refus coûte, et qu'il faut dire** : les commandes
                // `0..=indice` ont déjà poussé leur `InputMessage` et été actées
                // optimistiquement, donc leurs effets sur l'appareil
                // **subsistent** alors que leur sortie est jetée. Un client qui
                // groupe `setvol 40` puis un gros `playlistinfo` verra donc le
                // volume changer sans recevoir la moindre ligne. C'est
                // exactement le compromis que MPD fait sur une erreur en milieu
                // de liste — les commandes déjà exécutées le restent — et il est
                // acceptable pour la même raison : défaire ce qui est parti vers
                // le cœur n'est pas en notre pouvoir, et le client peut toujours
                // relire l'état.
                if sortie.trop_grande() {
                    tracing::warn!("mpd response over {MAX_REPONSE} bytes; refusing");
                    let nom = args.first().map_or("", String::as_str);
                    let refus = ack(Ack::Unknown, indice, nom, "response too large");
                    ecrire(ecriture, &[refus]).await?;
                    return Ok(Suite::Continuer);
                }
            }
            // `noidle` reçu hors attente : `OK` sec, et dans une liste un
            // `list_OK` comme n'importe quelle commande sans lignes.
            //
            // Empilé **sans vérifier le plafond**, à la différence du bras
            // ci-dessus : huit octets par commande, donc 16 Kio au pire pour un
            // lot entier de `noidle`, ce que le compte de commandes borne déjà.
            // C'est ce qui porte le résidu au-delà du plafond à une trentaine de
            // kibioctets, et non à la seule réponse d'une commande.
            Issue::Annuler => {
                if avec_ok {
                    sortie.pousser("list_OK".to_string());
                }
            }
            // `binarylimit` : la valeur est déjà bornée par `commandes`, il n'y
            // a qu'à la retenir. Elle vaut pour la **suite** de cette
            // connexion, y compris pour les commandes qui suivent dans la même
            // liste — c'est ce que fait MPD, et c'est le seul ordre qui rende
            // `binarylimit` puis `albumart` groupés utilisables.
            Issue::LimiteBinaire(n) => {
                *limite_binaire = n;
                if avec_ok {
                    sortie.pousser("list_OK".to_string());
                }
            }
            // La première erreur produit son `ACK` et **rien de ce qui suit
            // n'est exécuté** : le `for` s'arrête là. Les lignes déjà
            // composées partent quand même, comme le fait MPD — un `ACK` ne
            // rétracte pas les réponses des commandes qui, elles, ont abouti.
            Issue::Refuser(refus) => {
                // **Le refus est journalisé avec la commande entière**, et ce
                // n'est pas du confort. Un client qui bute sur une commande non
                // gérée n'affiche qu'un message générique — « unsupported » —
                // et l'opérateur n'a alors aucun moyen de savoir *laquelle* :
                // c'est exactement ce qui a manqué pour diagnostiquer l'échec
                // de M.A.L.P. sur la sélection d'une piste dans une liste
                // enregistrée. Les arguments comptent autant que le nom : la
                // même commande peut être refusée pour sa forme.
                //
                // En `info` et non en `warn` : un refus est une réponse
                // ordinaire du protocole (un client essaie, apprend, passe à
                // autre chose), et le cœur ne retient que les `warn` pour sa
                // carte « dernières erreurs » — y verser chaque commande
                // inconnue d'un client bavard la remplirait de bruit.
                tracing::info!("mpd refused {args:?}: {refus}");
                sortie.pousser(refus);
                ecrire(ecriture, &sortie.lignes).await?;
                return Ok(Suite::Continuer);
            }
            // Une réponse binaire : elle est écrite **seule**, par son propre
            // chemin, et elle clôt la requête — pas de `OK` ajouté par la
            // suite de la boucle, `ecrire_octets` pose le sien.
            //
            // `sortie` est nécessairement vide ici : les deux commandes
            // binaires sont refusées à l'accumulation d'une liste (voir
            // `servir`), donc le lot n'a qu'une commande. L'écrire quand même
            // garde cette fonction juste si un jour ce n'était plus le cas,
            // plutôt que d'avaler des lignes — le même choix que le bras
            // `Attendre` juste en dessous, pour la même raison.
            Issue::Octets(binaire) => {
                ecrire(ecriture, &sortie.lignes).await?;
                ecrire_octets(ecriture, &binaire).await?;
                return Ok(Suite::Continuer);
            }
            Issue::Attendre(sujets) => {
                // `idle` n'atteint jamais ce point dans une liste : la liste
                // l'a refusé à l'accumulation. Hors liste, le lot n'a qu'une
                // commande, donc `sortie` est vide — l'écrire quand même
                // garde cette fonction juste si un jour un lot en contenait
                // plusieurs, plutôt que d'avaler des lignes.
                ecrire(ecriture, &sortie.lignes).await?;
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
                sortie.pousser("OK".to_string());
                ecrire(ecriture, &sortie.lignes).await?;
                return Ok(Suite::Fermer);
            }
        }
    }
    // Un seul `OK` clôt le lot entier : c'est ce qui distingue une liste de
    // commandes de la même suite de commandes envoyées une par une.
    sortie.pousser("OK".to_string());
    ecrire(ecriture, &sortie.lignes).await?;
    Ok(Suite::Continuer)
}

/// Combien de temps une demande de pochette patiente pour une image que
/// l'appareil a déjà annoncée.
///
/// Trois secondes, et le nombre vient des deux échéances qu'il doit couvrir :
/// le cœur borne à `sante::DELAI` la lecture d'un fichier de pochette sur un
/// partage, et un téléchargement réseau est du même ordre. Au-delà, l'image
/// n'arrivera probablement pas pour cette piste, et le refus est la bonne
/// réponse.
///
/// Ce que cette attente **ne** met pas en péril : une session est une tâche à
/// elle seule, donc patienter ici ne retient personne d'autre (voir l'en-tête
/// du module). M.A.L.P. ouvre d'ailleurs une connexion distincte pour les
/// images.
const DELAI_POCHETTE: std::time::Duration = std::time::Duration::from_secs(3);

/// Attend, au plus `DELAI_POCHETTE`, que la pochette annoncée arrive, et rend
/// l'instantané qui décidera.
///
/// Rend la main **dès** que l'attente n'a plus d'objet : soit l'image est là,
/// soit l'état a changé au point que la demande ne se répare plus (piste
/// suivante, arrêt). C'est `pochette_annoncee_mais_absente` qui tranche, la
/// même fonction que celle qui a décidé d'attendre — un seul énoncé de la
/// condition, jamais deux à garder d'accord.
///
/// À l'échéance, rend le dernier instantané lu : `traiter` en tirera le refus
/// ordinaire, comme si l'on n'avait jamais attendu.
async fn attendre_pochette(etat: &EtatPartage, instantane: Instantane) -> Instantane {
    let args = arguments_de_pochette(&instantane);
    let mut courant = instantane;
    let echeance = tokio::time::Instant::now() + DELAI_POCHETTE;
    loop {
        // `Sujet::Player` : c'est le sujet que `appliquer_pochette` déplace —
        // le protocole MPD n'en a aucun pour les images, et le greffon a choisi
        // celui-là (voir sa doc). Attendre sur lui, c'est attendre exactement
        // l'arrivée de l'image, plus les changements de piste, qui doivent eux
        // aussi nous réveiller pour cesser d'attendre.
        let attente = etat.attendre(&[Sujet::Player], courant.versions);
        if tokio::time::timeout_at(echeance, attente).await.is_err() {
            tracing::debug!("mpd cover did not arrive within {DELAI_POCHETTE:?}");
            return courant;
        }
        courant = etat.lire().await;
        if !pochette_annoncee_mais_absente(&courant, &args) {
            return courant;
        }
    }
}

/// Reconstruit les arguments d'une demande de pochette pour ce qui joue.
///
/// **Reconstruits et non transportés**, et la nuance est le sujet : la boucle
/// d'attente doit réévaluer la condition contre l'état *courant*, or l'URI que
/// le client a écrite désigne la piste d'alors. Les rebâtir depuis l'instantané
/// de départ garde exactement la question posée — « l'image de cette piste-là
/// est-elle arrivée ? » — et fait sortir la boucle dès que la piste change,
/// puisque l'URI ne correspondra plus.
fn arguments_de_pochette(inst: &Instantane) -> Vec<String> {
    vec![
        "albumart".to_string(),
        inst.etat
            .preset
            .map(|p| crate::commandes::uri(&inst.etat.source, p))
            .unwrap_or_default(),
        "0".to_string(),
    ]
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
///
/// **`vues` est la référence de la connexion, et cette fonction est la seule à
/// l'avancer** : sur un réveil annoncé, et pour les seuls sujets annoncés. Le
/// vrai MPD n'efface que les drapeaux qu'il vient de rapporter, et tout avancer
/// d'un coup perdrait le changement d'un sujet qu'un `idle` suivant demandera
/// (`idle player` puis `idle mixer` en est le cas le plus court). Un `noidle`,
/// lui, n'annonce rien : il n'avance donc rien, et le changement en attente
/// ressortira à l'`idle` d'après.
async fn attendre_idle(
    lignes: &mut LecteurBorne,
    ecriture: &mut OwnedWriteHalf,
    etat: &EtatPartage,
    sujets: &[Sujet],
    vues: &mut [u64; 4],
) -> Result<Suite> {
    // Deux issues, et il faut écouter les deux : le réveil, et ce que le client
    // dit pendant l'attente — `noidle`, la seule commande que MPD y autorise,
    // ou n'importe quelle autre ligne, qui vaut alors `noidle` implicite.
    // `LecteurBorne::ligne_suivante` est sûre à l'annulation (son tampon vit
    // dans la structure, voir là-bas), donc la branche perdante ne perd aucun
    // octet ; et abandonner `attendre` ne perd aucun réveil, puisque `vues`
    // reste la référence et que les compteurs sont monotones.
    let reveil = tokio::select! {
        reveil = etat.attendre(sujets, *vues) => reveil,
        lue = lignes.ligne_suivante() => {
            let Some(brute) = lue? else {
                // Le client est parti pendant son attente : rien à écrire.
                return Ok(Suite::Fermer);
            };
            // **Une ligne reçue pendant l'attente clôt l'`idle` par un `OK`
            // nu.** C'est la comptabilité du protocole : un client MPD compte
            // un terminateur par requête, et il en a écrit deux — son `idle`,
            // puis cette ligne.
            //
            // Ce code refusait cette ligne par un seul `ACK` et n'écrivait rien
            // pour l'`idle` : deux requêtes se partageaient un terminateur, et
            // le client repartait **décalé de un, définitivement** — chaque
            // réponse suivante lue comme celle de sa requête précédente. Un
            // décalage silencieux et permanent, là où le choix de MPD (fermer)
            // est bruyant et auto-réparateur. Nous gardons le choix de ne pas
            // fermer — « une ligne fautive n'est jamais une rupture », et une
            // reconnexion coûterait au client un défaut qu'aucun journal ne lui
            // montre — mais en réparant ce qu'il avait cassé : l'invariant que
            // cette fonction énonce sur `Issue::Fermer` redevient vrai, toute
            // commande acceptée reçoit exactement un terminateur.
            //
            // **Et l'`OK` nu n'est pas une forme inventée pour l'occasion** :
            // c'est déjà ce que le bras `noidle` écrivait, et c'est donc la
            // même réponse au même endroit. La correction n'étend qu'un
            // comportement existant à un second déclencheur — elle ne peut pas
            // mettre sur le fil une forme qu'un client n'aurait jamais vue.
            ecrire(ecriture, &["OK".to_string()]).await?;
            // `noidle` est la seule ligne qui ne mérite pas de réponse propre :
            // ce n'est pas une requête mais **l'annulation de celle en cours**,
            // et l'`OK` qu'on vient d'écrire est le sien autant que celui de
            // l'`idle` — un seul terminateur pour `idle` + `noidle`, exactement
            // comme MPD. Tout le reste est un `noidle` implicite **suivi de
            // cette commande**, vraisemblablement ce que le client voulait
            // dire : la ligne repart donc dans l'aiguillage complet de
            // `servir` — listes de commandes, lignes illisibles et `close`
            // comprises — sans qu'un seul cas soit réinterprété ici.
            //
            // Une ligne illisible n'est pas `noidle` (elle ne se découpe pas),
            // et c'est la conduite voulue : elle recevra son `ACK` au tour
            // suivant, comme n'importe où ailleurs.
            let est_noidle = decouper(&brute)
                .map(|args| args.first().is_some_and(|mot| mot == "noidle"))
                .unwrap_or(false);
            if !est_noidle {
                lignes.remettre(brute);
            }
            // La référence de la connexion n'avance pas : rien n'a été annoncé,
            // donc un changement survenu pendant cette attente ressortira à
            // l'`idle` suivant.
            return Ok(Suite::Continuer);
        }
    };
    // **Les compteurs rapportés, et eux seuls, sont consommés.** Avancer tout
    // le tableau perdrait le changement d'un sujet que cet `idle` n'a pas
    // demandé.
    for sujet in &reveil.bouges {
        vues[*sujet as usize] = reveil.versions[*sujet as usize];
    }
    let mut reponse: Vec<String> =
        reveil.bouges.iter().map(|sujet| ligne("changed", nom_sujet(*sujet))).collect();
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
    // Capacité exacte dès le départ : sans elle, mettre à plat une réponse
    // proche du mébioctet la réallouerait une vingtaine de fois en doublant, en
    // demandant chaque fois un bloc contigu plus grand que le précédent.
    // `MAX_REPONSE` borne la taille de ce tampon ; cette ligne borne le nombre
    // de fois qu'on la demande.
    let mut tampon = String::with_capacity(lignes.iter().map(|l| l.len() + 1).sum());
    for l in lignes {
        tampon.push_str(l);
        tampon.push('\n');
    }
    ecriture.write_all(tampon.as_bytes()).await?;
    Ok(())
}

/// Écrit une réponse **binaire** d'un seul coup : l'en-tête, les octets bruts,
/// puis le terminateur.
///
/// La forme est celle de MPD, à l'octet près :
///
/// ```text
/// size: <taille de l'image entière>
/// type: <mime>            (readpicture seulement)
/// binary: <taille de cette tranche>
/// <les octets bruts>
/// OK
/// ```
///
/// Le `\n` qui suit les octets bruts n'est pas décoratif : c'est celui que MPD
/// écrit (`Response::WriteBinary`), et libmpdclient le consomme avant de lire
/// le terminateur. L'omettre ferait lire `<dernier octet>OK` comme une ligne
/// inconnue.
///
/// **Un seul `write_all`, comme `ecrire`**, et la même raison : une réponse à
/// moitié écrite serait lue comme une réponse complète par un client qui compte
/// ses terminateurs. La recopie de la tranche dans le tampon coûte au plus
/// `MAX_TRANCHE_PLAFOND` octets — soixante-quatre kibioctets si le client a
/// relevé sa limite par `binarylimit`, huit sinon, à comparer aux dizaines de
/// mébioctets que le chemin texte a dû se voir interdire.
///
/// **Ce que cette fonction ne fait pas : allouer l'image.** `binaire.image` est
/// un `Arc` partagé avec l'état ; seule la tranche est copiée. C'est ce qui rend
/// le pire cas d'une connexion binaire indépendant de la taille de la pochette.
async fn ecrire_octets(ecriture: &mut OwnedWriteHalf, binaire: &Binaire) -> Result<()> {
    // Indexation sans contrôle : c'est `commandes::pochette` qui établit
    // l'intervalle, et son contrat est qu'il tient dans l'image et dans la
    // limite de la connexion, elle-même plafonnée à `MAX_TRANCHE_PLAFOND`.
    // L'assertion de débogage le dit plutôt que de le supposer en silence, sans
    // rien coûter en production.
    let tranche = &binaire.image[binaire.tranche.clone()];
    debug_assert!(
        tranche.len() <= MAX_TRANCHE_PLAFOND,
        "une tranche depasse le plafond du greffon"
    );
    let binary = ligne("binary", tranche.len());
    let entete: usize =
        binaire.entete.iter().chain(std::iter::once(&binary)).map(|l| l.len() + 1).sum();
    // Capacité exacte : en-tête, tranche, puis `\nOK\n`.
    let mut tampon = Vec::with_capacity(entete + tranche.len() + 4);
    for l in binaire.entete.iter().chain(std::iter::once(&binary)) {
        tampon.extend_from_slice(l.as_bytes());
        tampon.push(b'\n');
    }
    tampon.extend_from_slice(tranche);
    tampon.extend_from_slice(b"\nOK\n");
    ecriture.write_all(&tampon).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::etat::Instantane;
    use ritornello_proto::{Command, Morceau, Playback, PlayerState};
    // `AsyncReadExt` pour le `read_exact` des tranches binaires : le client de
    // test est le seul du greffon à lire des octets bruts.
    use tokio::io::AsyncReadExt;
    // Le lecteur borné de la session n'en a plus besoin ; le client de test,
    // lui, lit des lignes sans plafond à défendre.
    use tokio::io::Lines;

    /// Un client de test : les lignes reçues d'un côté, la plume de l'autre.
    struct Client {
        lignes: Lines<BufReader<OwnedReadHalf>>,
        ecriture: OwnedWriteHalf,
    }

    impl Client {
        /// Un client sur un flux déjà ouvert. Séparé de `Serveur::client` :
        /// les tests de reliaison connectent une adresse qui n'est pas celle
        /// que le `Serveur` de test porte.
        fn depuis(flux: TcpStream) -> Client {
            let (lecture, ecriture) = flux.into_split();
            Client { lignes: BufReader::new(lecture).lines(), ecriture }
        }

        async fn connecter(adresse: std::net::SocketAddr) -> Client {
            Client::depuis(TcpStream::connect(adresse).await.unwrap())
        }

        async fn envoyer(&mut self, ligne: &str) {
            self.ecriture.write_all(format!("{ligne}\n").as_bytes()).await.unwrap();
        }

        async fn recevoir(&mut self) -> String {
            self.lignes.next_line().await.unwrap().expect("le serveur a ferme la connexion")
        }

        /// Lit exactement `n` octets **bruts**.
        ///
        /// Derrière le lecteur de lignes (`get_mut`) et non sur la chaussette :
        /// les octets qui suivent un en-tête sont déjà dans le tampon du
        /// `BufReader` au moment où la dernière ligne d'en-tête a été rendue,
        /// et lire la chaussette directement les laisserait là — un test qui
        /// se bloquerait sans que le serveur y soit pour rien.
        async fn octets(&mut self, n: usize) -> Vec<u8> {
            let mut tampon = vec![0u8; n];
            self.lignes.get_mut().read_exact(&mut tampon).await.unwrap();
            tampon
        }

        /// Rejoue la séquence d'un vrai client : une requête par tranche,
        /// l'offset croissant, jusqu'à détenir `size` octets.
        ///
        /// C'est bien la boucle de M.A.L.P. et de libmpdclient : la première
        /// réponse apprend la taille totale, chaque suivante est demandée à
        /// l'offset de ce qu'on a déjà. La sortie de boucle ne dépend d'aucune
        /// horloge ni d'aucun compte d'itérations — seulement de `size`.
        async fn recuperer(&mut self, commande: &str, uri: &str) -> Recuperee {
            let mut recuperee = Recuperee { image: Vec::new(), tailles: Vec::new(), mime: None };
            loop {
                self.envoyer(&format!("{commande} {uri} {}", recuperee.image.len())).await;
                let taille = self.entier("size").await;
                let mut entete = self.recevoir().await;
                // `type:` n'est là que pour `readpicture` : c'est une ligne de
                // plus, avant `binary:`, exactement comme MPD la place.
                if let Some(mime) = entete.strip_prefix("type: ") {
                    recuperee.mime = Some(mime.to_string());
                    entete = self.recevoir().await;
                }
                let n: usize = entete
                    .strip_prefix("binary: ")
                    .unwrap_or_else(|| panic!("attendu binary:, obtenu {entete}"))
                    .parse()
                    .unwrap();
                // Une tranche vide ne fait pas avancer la boucle : la refuser
                // ici transforme un serveur qui piétine en échec franc, plutôt
                // qu'en test qui tourne à vide.
                assert!(n > 0, "une tranche vide ne termine jamais la recuperation");
                recuperee.image.extend_from_slice(&self.octets(n).await);
                recuperee.tailles.push(n);
                // Le `\n` que MPD écrit après les octets bruts : lu comme une
                // ligne vide. Son absence ferait lire `<dernier octet>OK`.
                assert_eq!(self.recevoir().await, "", "un saut de ligne suit les octets bruts");
                assert_eq!(self.recevoir().await, "OK", "chaque tranche est une reponse complete");
                if recuperee.image.len() >= taille {
                    return recuperee;
                }
            }
        }

        /// La valeur entière d'une ligne `clé: nombre` attendue.
        async fn entier(&mut self, cle: &str) -> usize {
            let l = self.recevoir().await;
            l.strip_prefix(&format!("{cle}: "))
                .unwrap_or_else(|| panic!("attendu {cle}:, obtenu {l}"))
                .parse()
                .unwrap()
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

    /// Ce qu'une récupération complète de pochette a produit : l'image
    /// réassemblée, la taille de chaque tranche reçue dans l'ordre, et le MIME
    /// si le serveur l'a annoncé.
    struct Recuperee {
        image: Vec<u8>,
        tailles: Vec<usize>,
        mime: Option<String>,
    }

    struct Serveur {
        adresse: std::net::SocketAddr,
        etat: Arc<EtatPartage>,
        /// Tenu vivant exprès : lâcher l'émetteur ferait sortir `ecouter` de
        /// son `select!` (« la moitié admin a disparu ») et les tests
        /// n'éprouveraient plus le chemin de service ordinaire, seulement celui
        /// de l'extinction.
        _config_tx: tokio::sync::watch::Sender<crate::config::Config>,
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
        let (config_tx, config_rx) =
            tokio::sync::watch::channel(crate::config::Config::default());
        tokio::spawn(ecouter(ecoute, config_rx, etat.clone(), tx));
        (Serveur { adresse, etat, _config_tx: config_tx }, rx)
    }

    impl Serveur {
        async fn client(&self) -> Client {
            Client::connecter(self.adresse).await
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
    /// **Deux trames alternées, et non une répétée** : chaque poussée doit être
    /// un changement réel, sinon la déduplication d'`appliquer_etat` l'avale et
    /// la boucle tourne à vide.
    ///
    /// La boucle, elle, n'arbitre plus aucune course. Elle en arbitrait une :
    /// une trame appliquée avant que la session n'ait lu sa ligne `idle` était
    /// comprise dans les compteurs qu'elle mémorisait, donc invisible pour
    /// elle. **C'était un défaut de la session et non un contrat d'`attendre`**
    /// — la référence d'un `idle` est désormais celle que la connexion porte
    /// depuis sa bannière (voir `servir`), donc une seule trame suffirait ici.
    /// La boucle est gardée parce qu'elle ne dépend d'aucune horloge et qu'elle
    /// reste juste dans les deux cas ; ce qu'il ne faut plus faire, c'est
    /// justifier par elle une course qui a été corrigée.
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
        assert_eq!(*recues.last().unwrap(), "ACK [5@1] {nawak} unsupported", "{recues:?}");
        assert!(!recues.iter().any(|l| l == "OK"), "un ACK remplace le OK: {recues:?}");
        // **Attention à ce qui prouve quoi ici**, la relecture s'est fait
        // prendre et le commentaire précédent disait faux. `reponse()`
        // s'arrête au **premier** terminateur, et un `ACK` en est un : compter
        // les `volume:` de cette seule réponse ne prouve donc rien. Une session
        // qui continuerait la liste après l'erreur écrirait tout d'un bloc, et
        // `reponse()` rendrait exactement les mêmes lignes — jusqu'à l'`ACK`,
        // en laissant le `status` suivant **derrière** dans le flux.
        //
        // Ce qui tue ce mutant, c'est la suite : la commande d'après doit
        // recevoir sa propre réponse et rien d'autre. Un `status` fuité
        // ressort ici, et le compte se fait sur les **deux** réponses. Ne pas
        // « raccourcir » ce test en gardant le compte et en jetant le `ping` :
        // c'est le `ping` qui travaille.
        c.envoyer("ping").await;
        let apres = c.reponse().await;
        assert_eq!(apres, vec!["OK".to_string()], "reponse fuitee: {apres:?}");
        let volumes = recues.iter().chain(apres.iter()).filter(|l| l.starts_with("volume: ")).count();
        assert_eq!(volumes, 1, "le troisieme status ne doit pas avoir tourne: {recues:?} {apres:?}");
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
    async fn une_commande_pendant_une_attente_annule_lattente_puis_est_executee() {
        // **Un test de comptabilité, pas de contenu.** Le client écrit deux
        // lignes (`idle`, `status`) et doit recevoir **deux** terminateurs. Ce
        // code n'en écrivait qu'un — l'`ACK` refusant le `status` — et l'`idle`
        // n'en recevait aucun : le client repartait décalé de un, de façon
        // permanente, chaque réponse suivante lue comme celle de sa requête
        // précédente. Silencieux et définitif, là où le choix de MPD (fermer)
        // est bruyant et auto-réparateur.
        //
        // Le `noidle` implicite est ce qui répare : un `OK` nu clôt l'`idle`,
        // puis la commande est exécutée comme n'importe où ailleurs.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle").await;
        c.envoyer("status").await;

        // Premier terminateur : celui de l'`idle` annulé. `OK` nu, aucun
        // `changed:` — rien n'a bougé, et de toute façon `noidle` n'annonce
        // rien.
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        // Second terminateur : la réponse du `status`, avec ses lignes.
        let deuxieme = c.reponse().await;
        assert!(deuxieme.iter().any(|l| l.starts_with("volume: ")), "{deuxieme:?}");
        assert_eq!(*deuxieme.last().unwrap(), "OK");
        // Et la troisième requête reçoit **sa** réponse : c'est ce qui prouve
        // l'absence de décalage. Un `ping` répond `OK` sec, donc une réponse de
        // `status` qui traînerait dans le flux ressortirait ici.
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);

        // ------------------------------------------------------------------
        // **Et le cas voisin qui doit rester à UN seul terminateur.** Gardé
        // dans le même test exprès : séparés, un remaniement futur croirait
        // l'un redondant. `noidle` n'est pas une requête mais l'annulation de
        // celle en cours, donc `idle` + `noidle` = un `OK`, comme chez MPD.
        // Si la correction ci-dessus le faisait passer à deux, elle aurait
        // cassé le cas correct.
        // ------------------------------------------------------------------
        c.envoyer("idle").await;
        c.envoyer("noidle").await;
        c.envoyer("status").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()], "un seul OK pour idle + noidle");
        let apres = c.reponse().await;
        assert!(
            apres.iter().any(|l| l.starts_with("volume: ")),
            "un terminateur de trop apres noidle: {apres:?}"
        );
    }

    #[tokio::test]
    async fn une_ligne_illisible_pendant_une_attente_compte_aussi_deux_terminateurs() {
        // La même comptabilité sur l'autre entrée de cette branche : une ligne
        // mal citée n'est pas `noidle` (elle ne se découpe pas), donc elle est
        // un `noidle` implicite suivi d'une ligne qui recevra son `ACK` par le
        // chemin ordinaire. Deux lignes écrites, deux terminateurs.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle").await;
        c.envoyer(r#"load "France"#).await;

        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        assert_eq!(c.reponse().await, vec!["ACK [2@0] {load} invalid argument".to_string()]);
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_liste_de_commandes_ouverte_pendant_une_attente_est_traitee_comme_une_liste() {
        // La ligne remise repasse par l'aiguillage **complet** de `servir`, et
        // non par une réinterprétation locale : un `command_list_begin` reçu
        // pendant une attente ouvre donc une vraie liste, dont le `OK` unique
        // arrive après celui de l'`idle` annulé. C'est ce qui garantit qu'aucun
        // cas n'a besoin d'être dupliqué dans `attendre_idle`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle").await;
        c.envoyer("command_list_begin").await;
        c.envoyer("status").await;
        c.envoyer("status").await;
        c.envoyer("command_list_end").await;

        assert_eq!(c.reponse().await, vec!["OK".to_string()], "l'idle annule a son terminateur");
        let liste = c.reponse().await;
        assert_eq!(liste.iter().filter(|l| *l == "OK").count(), 1, "{liste:?}");
        assert_eq!(liste.iter().filter(|l| l.starts_with("volume: ")).count(), 2, "{liste:?}");
    }

    #[tokio::test]
    async fn un_changement_survenu_entre_deux_commandes_est_rapporte_par_lidle_suivant() {
        // **LE test de ce correctif.** La session mémorisait les compteurs dans
        // l'`Instantane` de la commande `idle` elle-même, donc tout ce qui avait
        // bougé entre la réponse précédente du client et sa ligne `idle` était
        // avalé — c'est-à-dire pendant la seule fenêtre où un client MPD
        // n'écoute pas. Pour `stored_playlist`, rien ne rejoue l'événement avant
        // le prochain changement de catalogue : `listplaylists` reste périmé,
        // potentiellement pour toujours. C'est exactement le premier essai
        // prévu sur l'appareil (« désactiver une source, sa liste doit
        // rétrécir »), qui pouvait donc échouer en silence.
        //
        // Sans horloge, et **une seule trame poussée** : c'est ce qui rend la
        // preuve concluante. Aucun changement ne suivra, donc une session qui
        // relit ses compteurs à la ligne `idle` dort pour toujours et ce test
        // **pend** — le mode d'échec voulu. Vérifié contre l'ancien code.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        // Une commande, sa réponse lue jusqu'au terminateur : le client est
        // désormais « entre deux commandes », précisément comme un client qui
        // vient de rafraîchir son écran.
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);

        // Le changement arrive maintenant : personne n'attend.
        s.etat.appliquer_catalogue(ritornello_proto::Catalogue {
            sources: vec![ritornello_proto::SourceCatalogue {
                name: "radio".into(),
                presets: vec![ritornello_proto::Preset { index: 1, name: "FIP".into() }],
            }],
        })
        .await;

        c.envoyer("idle stored_playlist").await;
        assert_eq!(
            c.reponse().await,
            vec!["changed: stored_playlist".to_string(), "OK".to_string()]
        );
    }

    #[tokio::test]
    async fn un_reveil_ne_consomme_que_les_sujets_quil_annonce() {
        // La moitié fine du même dispositif. Le réveil avance la référence de la
        // connexion **sujet par sujet**, comme MPD n'efface que les drapeaux
        // qu'il vient de rapporter : tout avancer d'un coup perdrait le
        // changement d'un sujet que cet `idle`-là n'avait pas demandé, et le
        // défaut réparé au-dessus se rouvrirait d'un cran plus loin.
        //
        // Déterministe et sans horloge : la trame est appliquée **avant** les
        // deux `idle`, donc chacun repart par la comparaison préalable, sans
        // jamais dormir. Une implémentation qui remettrait tout le tableau à
        // niveau au premier réveil ferait *pendre* le second `idle`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        // Une seule trame, qui bouge `player` **et** `mixer`.
        s.etat.appliquer_etat(trame_player_et_mixer(17)).await;

        c.envoyer("idle player").await;
        assert_eq!(c.reponse().await, vec!["changed: player".to_string(), "OK".to_string()]);

        c.envoyer("idle mixer").await;
        assert_eq!(c.reponse().await, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn un_reveil_annonce_consomme_bien_son_compteur() {
        // Le pendant indispensable : la référence doit *avancer*. Sans cela un
        // `idle` rapporterait éternellement le même changement, et un client
        // qui boucle sur `idle` — c'est-à-dire tous — tournerait à pleine
        // vitesse sur la commande faite pour l'en dispenser.
        //
        // Prouvé sans horloge : le second `idle` doit **attendre**, donc la
        // commande d'après est un `noidle` dont le `OK` unique est suivi de la
        // réponse du `status`. Si le second `idle` avait répondu tout seul, il
        // y aurait un terminateur de plus et on lirait ici le `OK` du `noidle`
        // au lieu des lignes du `status` — le même compte que
        // `noidle_rend_la_main_immediatement`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        s.etat.appliquer_etat(trame_mixer(17)).await;

        c.envoyer("idle mixer").await;
        assert_eq!(c.reponse().await, vec!["changed: mixer".to_string(), "OK".to_string()]);

        c.envoyer("idle mixer").await;
        c.envoyer("noidle").await;
        c.envoyer("status").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        let apres = c.reponse().await;
        assert!(
            apres.iter().any(|l| l.starts_with("volume: ")),
            "le second idle a repondu tout seul: {apres:?}"
        );
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
    async fn une_ligne_au_dela_du_plafond_avec_une_fin_de_ligne_ferme_aussi() {
        // Le plafond est contrôlé dans les **deux** bras du lecteur, et le test
        // précédent n'en visite qu'un (le morceau lu ne contient pas de `\n`).
        // Celui-ci visite l'autre : la ligne dépasse le plafond *et* se termine
        // bien. Sans ce cas, retirer le contrôle du bras `Some` laissait passer
        // toute la suite — un plafond que personne n'exerce est un plafond
        // qu'on retire par distraction.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        // Exactement `MAX_LIGNE` octets sans `\n` : légal, le tampon les garde.
        c.ecriture.write_all(&vec![b'a'; MAX_LIGNE]).await.unwrap();
        // Puis un octet de trop, cette fois suivi de sa fin de ligne : c'est le
        // bras `Some` qui doit refuser, en comptant ce qui était déjà accumulé.
        let _ = c.ecriture.write_all(b"b\n").await;
        assert!(
            c.lignes.next_line().await.unwrap().is_none(),
            "une ligne au-dela du plafond ferme la connexion, meme terminee"
        );
    }

    #[tokio::test]
    async fn une_ligne_vide_est_refusee_sans_fermer() {
        // Un `\n` nu. `traiter` sait déjà le refuser (elle est totale par
        // construction), mais aucun test de session ne le montrait bout en
        // bout : la session pourrait l'avaler en silence, et un client qui
        // attend une réponse par ligne resterait pendu.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.ecriture.write_all(b"\n").await.unwrap();
        assert_eq!(c.reponse().await, vec!["ACK [5@0] {} unsupported".to_string()]);
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_reponse_trop_grosse_est_refusee_sans_fermer() {
        // L'amplificateur : `MAX_COMMANDES_LISTE` borne les commandes, pas ce
        // qu'elles **produisent**. Une liste de `playlistinfo` sur une file de
        // 255 entrées rend une quinzaine de kibioctets par commande, et la
        // réponse entière était mise à plat dans une seule `String` avant le
        // `write_all` — donc une allocation contiguë de plusieurs dizaines de
        // mébioctets, demandée à un Pi dont la mémoire est fragmentée. 26 Kio
        // d'entrée suffisaient.
        //
        // Le refus arrive **avant** toute écriture, donc il remplace la réponse
        // au lieu de s'y ajouter : un seul terminateur, et la connexion vit.
        let (s, _rx) = serveur().await;
        s.etat
            .appliquer_etat(PlayerState {
                source: "cd".to_string(),
                preset_count: Some(255),
                ..Default::default()
            })
            .await;
        let mut c = s.client_pret().await;
        let mut lot = String::from("command_list_begin\n");
        for _ in 0..100 {
            lot.push_str("playlistinfo\n");
        }
        lot.push_str("command_list_end\n");
        c.ecriture.write_all(lot.as_bytes()).await.unwrap();
        let recues = c.reponse().await;
        assert_eq!(recues.len(), 1, "le refus remplace la reponse composee: {recues:?}");
        // L'indice exact dépend de l'arithmétique des octets (une quinzaine de
        // kibioctets par commande, un mébioctet de plafond) : ce qui compte est
        // qu'il nomme la commande qui a débordé et son rang dans le lot.
        let refus = &recues[0];
        assert!(refus.starts_with("ACK [5@"), "{refus}");
        assert!(refus.ends_with("] {playlistinfo} response too large"), "{refus}");
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_liste_lourde_en_octets_est_refusee_bien_avant_le_compte() {
        // L'autre moitié du même trou : une ligne accumulée peut légitimement
        // peser `MAX_LIGNE`, donc 2048 commandes bornées **en nombre** pesaient
        // 16 Mio par connexion. Ici trente-deux lignes de 8 Kio tombent
        // *exactement* sur les 256 Kio — le plafond refuse au-delà, pas à
        // égalité — donc c'est la trente-troisième qui franchit, et la boucle en
        // envoie une de plus pour cette raison. Trente-trois, c'est très loin
        // des 2048 commandes : la borne qui refuse ici est bien celle en octets
        // et non celle en nombre.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("command_list_begin").await;
        let mut lot = String::new();
        for _ in 0..MAX_OCTETS_LISTE.div_ceil(MAX_LIGNE) + 1 {
            lot.push_str("ping ");
            lot.push_str(&"a".repeat(MAX_LIGNE - 6));
            lot.push('\n');
        }
        c.ecriture.write_all(lot.as_bytes()).await.unwrap();
        let recues = c.reponse().await;
        assert_eq!(recues.len(), 1, "{recues:?}");
        assert!(recues[0].starts_with("ACK [5@"), "{recues:?}");
        assert!(recues[0].ends_with("] {ping} list too large"), "{recues:?}");
        // L'état de liste est rendu : la commande suivante répond seule.
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn au_dela_du_plafond_de_sessions_une_connexion_est_refusee_aussitot() {
        // Le multiplicateur : chaque autre plafond borne une connexion, et le
        // nombre de connexions ne l'était pas. Un client qui fuit ses
        // connexions — qui en rouvre une à chaque reprise de réseau sans fermer
        // la précédente — arrive ici par accident, sans le moindre script
        // hostile.
        //
        // Sans horloge, et l'ordre est garanti par la bannière : elle est
        // écrite par `servir`, donc *après* la prise de la place. Avoir lu
        // `MAX_SESSIONS` bannières prouve que les `MAX_SESSIONS` places sont
        // prises, et la connexion suivante est donc bien celle qui déborde.
        let (s, _rx) = serveur().await;
        let mut ouverts = Vec::new();
        for _ in 0..MAX_SESSIONS {
            ouverts.push(s.client_pret().await);
        }
        // Celle de trop : acceptée par le noyau (le port écoute toujours), puis
        // fermée aussitôt par `accepter`. Aucune bannière, donc fin de flux.
        let mut refuse = s.client().await;
        assert!(
            refuse.lignes.next_line().await.unwrap().is_none(),
            "au-dela du plafond, la connexion doit etre fermee sans banniere"
        );
        // Et les sessions déjà ouvertes servent encore : le plafond refuse les
        // nouvelles, il ne dégrade pas les anciennes. La première et la
        // dernière, parce qu'un plafond mal câblé casse volontiers l'une des
        // deux extrémités.
        for indice in [0, MAX_SESSIONS - 1] {
            ouverts[indice].envoyer("ping").await;
            assert_eq!(ouverts[indice].reponse().await, vec!["OK".to_string()]);
        }
    }

    #[tokio::test]
    async fn un_changement_de_reglages_relie_le_serveur_sans_redemarrage() {
        // **La demande du propriétaire** : ne plus avoir à relancer le greffon à
        // la main après avoir changé le port sur la page d'admin.
        //
        // Sans horloge, comme `un_client_qui_part_rend_sa_place` : la boucle
        // réessaie jusqu'à ce que le nouveau port réponde, et rien ne l'arrête
        // d'autre que ce succès. Une implémentation qui ne se relierait jamais
        // fait *pendre* le test, ce qui est le mode d'échec voulu — et non un
        // délai deviné qui deviendrait un flake sous charge.
        let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ancienne = ecoute.local_addr().unwrap();
        // Un port libre, choisi par le noyau puis rendu : c'est la seule façon
        // d'en nommer un qui ne soit pas déjà pris sur la machine du test.
        let sonde = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let neuve = sonde.local_addr().unwrap();
        drop(sonde);

        let etat = Arc::new(EtatPartage::default());
        let (cmd_tx, _cmd_rx) = mpsc::channel(64);
        let (config_tx, config_rx) = tokio::sync::watch::channel(crate::config::Config {
            listen: "127.0.0.1".into(),
            port: ancienne.port(),
        });
        tokio::spawn(ecouter(ecoute, config_rx, etat, cmd_tx));

        // L'ancien port sert bien avant tout changement.
        let mut avant = Client::connecter(ancienne).await;
        assert!(avant.recevoir().await.starts_with("OK MPD "));

        config_tx
            .send(crate::config::Config { listen: "127.0.0.1".into(), port: neuve.port() })
            .unwrap();

        let banniere = loop {
            if let Ok(flux) = TcpStream::connect(neuve).await {
                let mut c = Client::depuis(flux);
                break c.recevoir().await;
            }
            tokio::task::yield_now().await;
        };
        assert!(banniere.starts_with("OK MPD "), "banniere inattendue: {banniere}");

        // Et la session déjà ouverte n'a pas été coupée : elle tient son propre
        // flux, que la fermeture de l'écouteur ne touche pas. C'est la
        // différence avec un vrai redémarrage de MPD, et elle est voulue.
        avant.envoyer("ping").await;
        assert_eq!(avant.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn un_port_impossible_laisse_le_serveur_ou_il_etait() {
        // Un réglage fautif — port déjà pris, adresse absente de la machine —
        // ne doit pas rendre le serveur MPD injoignable. L'ancien écouteur
        // n'est lâché qu'une fois le nouveau lié.
        let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ancienne = ecoute.local_addr().unwrap();
        let etat = Arc::new(EtatPartage::default());
        let (cmd_tx, _cmd_rx) = mpsc::channel(64);
        let (config_tx, config_rx) = tokio::sync::watch::channel(crate::config::Config {
            listen: "127.0.0.1".into(),
            port: ancienne.port(),
        });
        tokio::spawn(ecouter(ecoute, config_rx, etat, cmd_tx));

        // Une adresse qu'aucune interface ne porte : le `bind` échoue.
        config_tx
            .send(crate::config::Config { listen: "192.0.2.1".into(), port: 6600 })
            .unwrap();

        // Le serveur répond toujours là où il répondait. Boucle sans horloge,
        // même raison que le test ci-dessus : c'est le succès qui l'arrête.
        let banniere = loop {
            if let Ok(flux) = TcpStream::connect(ancienne).await {
                let mut c = Client::depuis(flux);
                break c.recevoir().await;
            }
            tokio::task::yield_now().await;
        };
        assert!(banniere.starts_with("OK MPD "), "banniere inattendue: {banniere}");
    }

    #[tokio::test]
    async fn un_client_qui_part_rend_sa_place() {
        // Le pendant indispensable : si le permis ne repartait pas avec la
        // tâche, l'appareil refuserait tout le monde après seize connexions
        // dans la vie du processus — une panne qui n'apparaîtrait qu'après des
        // jours, et qui ressemblerait à un défaut de réseau.
        //
        // Sans horloge : la boucle réessaie jusqu'à ce que la place soit rendue,
        // et rien ne l'arrête d'autre que ce succès. Elle est nécessaire parce
        // que rien n'ordonne la fermeture du client avec le moment où la session
        // serveur la constate ; une implémentation qui ne rendrait jamais la
        // place fait *pendre* le test, ce qui est le mode d'échec voulu.
        let (s, _rx) = serveur().await;
        let mut ouverts = Vec::new();
        for _ in 0..MAX_SESSIONS {
            ouverts.push(s.client_pret().await);
        }
        // Le premier s'en va pour de bon : ses deux moitiés sont lâchées.
        ouverts.remove(0);
        let banniere = loop {
            let mut candidat = s.client().await;
            if let Some(ligne) = candidat.lignes.next_line().await.unwrap() {
                break ligne;
            }
        };
        assert!(banniere.starts_with("OK MPD "), "banniere inattendue: {banniere}");
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
                traiter(&Instantane::default(), 0, &args, MAX_TRANCHE),
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
        // Prouvé sans horloge, **en comptant les terminateurs** : `idle` +
        // `noidle` n'en valent qu'un, donc la deuxième réponse lue est celle du
        // `status`. Une session qui aurait rendu `OK` tout de suite en aurait
        // écrit un de plus (le sien, puis celui du `noidle` reçu hors attente),
        // et on lirait ici un `OK` sec au lieu des lignes du `status`.
        //
        // Le discriminant a changé avec le `noidle` implicite : envoyer
        // `status` ne distingue plus rien, puisqu'une attente annulée écrit
        // désormais `OK` puis la réponse du `status` — exactement ce qu'un
        // `idle` répondant tout de suite produirait aussi.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.envoyer("idle database").await;
        // Des trames qui bougent tous les compteurs : aucune ne concerne les
        // sujets demandés (il n'y en a aucun), donc aucune ne doit réveiller.
        s.etat.appliquer_etat(trame_player_et_mixer(17)).await;
        s.etat.appliquer_etat(trame_player_et_mixer(18)).await;
        c.envoyer("noidle").await;
        c.envoyer("status").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        let apres = c.reponse().await;
        assert!(
            apres.iter().any(|l| l.starts_with("volume: ")),
            "idle database a repondu tout seul: {apres:?}"
        );
    }

    // ------------------------------------------------------------------
    // Les pochettes, sur une vraie chaussette
    // ------------------------------------------------------------------

    /// Le `href` que la trame d'état publie, et que la trame de pochette
    /// reprend.
    const HREF: &str = "/api/cover/1a2b3c";

    /// L'URI que notre `currentsong` publie pour l'état ci-dessous.
    const URI_COURANTE: &str = "ritornello://radio/2";

    /// Une taille qui n'est pas un multiple de `MAX_TRANCHE` : trois tranches,
    /// la dernière plus courte que les autres.
    const TAILLE: usize = MAX_TRANCHE * 2 + 1234;

    /// La trame d'état **telle que le cœur l'émet quand une pochette existe** :
    /// elle porte le `cover_href`, et c'est lui que la trame de pochette
    /// reprendra. Une trame sans `cover_href` accompagnée d'une pochette
    /// n'existe pas côté producteur, et un test qui l'emploierait prouverait
    /// une causalité impossible.
    fn trame_avec_pochette() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 40,
            playback: Playback::Playing,
            preset: Some(2),
            preset_count: Some(3),
            preset_name: Some("France Inter".into()),
            morceau: Morceau {
                title: Some("So What".into()),
                cover_href: Some(HREF.to_string()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Pousse l'état **puis** la pochette, dans cet ordre : c'est l'ordre du
    /// cœur (`relais_afficheur` envoie l'état avant les octets), et l'inverse
    /// laisserait le greffon dans un état qu'il ne connaît pas en production.
    async fn avec_pochette(etat: &EtatPartage, taille: usize) -> Vec<u8> {
        let cover = crate::etat::cover_de_test(HREF, taille);
        etat.appliquer_etat(trame_avec_pochette()).await;
        etat.appliquer_pochette(cover.clone()).await;
        cover.bytes
    }

    #[tokio::test]
    async fn albumart_rend_limage_entiere_et_elle_se_reassemble_a_lidentique() {
        // **Le test central de cette tâche.** Il rejoue la séquence d'un vrai
        // client sur une vraie chaussette, et il n'affirme pas « quelque chose
        // est arrivé » : il compare les octets réassemblés à ceux qui ont été
        // poussés. Un découpage qui saute, duplique ou décale un seul octet
        // échoue ici — et l'image est du bruit, donc rien ne peut le masquer.
        let (s, _rx) = serveur().await;
        let attendus = avec_pochette(&s.etat, TAILLE).await;
        let mut c = s.client_pret().await;

        let r = c.recuperer("albumart", URI_COURANTE).await;

        assert_eq!(r.image.len(), TAILLE, "taille reassemblee");
        assert_eq!(r.image, attendus, "les octets doivent arriver intacts");
        // Trois tranches : deux pleines, puis le reste. C'est la preuve que
        // l'offset croissant est honoré (deux requêtes de plus que la première)
        // et que la dernière tranche est plus courte que les autres.
        assert_eq!(r.tailles, vec![MAX_TRANCHE, MAX_TRANCHE, 1234]);
        // `albumart` n'annonce pas de type MIME, contrairement à `readpicture`.
        assert_eq!(r.mime, None);
        // Et la connexion reste utilisable après une réponse binaire : le
        // chemin des octets ne doit pas laisser la session désalignée.
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn readpicture_rend_les_memes_octets_et_annonce_le_type() {
        // M.A.L.P. essaie l'un, puis l'autre : les deux doivent aboutir, et sur
        // la même image. Seul le `type:` les distingue, comme chez MPD.
        let (s, _rx) = serveur().await;
        let attendus = avec_pochette(&s.etat, TAILLE).await;
        let mut c = s.client_pret().await;

        let r = c.recuperer("readpicture", URI_COURANTE).await;

        assert_eq!(r.image, attendus);
        assert_eq!(r.mime.as_deref(), Some("image/jpeg"));
    }

    #[tokio::test]
    async fn une_image_plus_courte_quune_tranche_tient_en_un_seul_aller_retour() {
        // Le cas réel et non le cas limite : la pochette mesurée du Cover Art
        // Archive fait 75 Kio, mais une vignette peut tenir sous les 8 Kio
        // d'une tranche. Une seule requête, une seule tranche, complète.
        let (s, _rx) = serveur().await;
        let attendus = avec_pochette(&s.etat, 1000).await;
        let mut c = s.client_pret().await;

        let r = c.recuperer("albumart", URI_COURANTE).await;

        assert_eq!(r.tailles, vec![1000]);
        assert_eq!(r.image, attendus);
    }

    #[tokio::test]
    async fn un_offset_au_dela_de_la_fin_est_refuse_sans_fermer() {
        let (s, _rx) = serveur().await;
        avec_pochette(&s.etat, TAILLE).await;
        let mut c = s.client_pret().await;

        c.envoyer(&format!("albumart {URI_COURANTE} {}", TAILLE + 1)).await;

        assert_eq!(
            c.reponse().await,
            vec!["ACK [2@0] {albumart} Offset too large".to_string()]
        );
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn sans_pochette_les_deux_commandes_refusent_et_la_connexion_survit() {
        // Le cas ordinaire : un flux sans image. Le client doit recevoir un
        // refus lisible et pouvoir continuer à parler — c'est ce refus qui le
        // fait basculer sur l'autre nom, puis renoncer proprement.
        //
        // **`cover_href: None`, et le détail est tout le test.** L'appareil
        // n'annonce aucune image, donc le refus est définitif et doit tomber
        // **tout de suite** : la nouvelle attente de `attendre_pochette` ne
        // couvre que la fenêtre où une image *a été annoncée* et n'est pas
        // encore arrivée. Une trame porteuse de `cover_href` ici — ce que ce
        // test faisait avant — décrivait au contraire cette fenêtre-là, et le
        // refus immédiat qu'il verrouillait était justement le défaut à
        // corriger.
        let (s, _rx) = serveur().await;
        let mut trame = trame_avec_pochette();
        trame.morceau.cover_href = None;
        s.etat.appliquer_etat(trame).await;
        let mut c = s.client_pret().await;

        for nom in ["albumart", "readpicture"] {
            c.envoyer(&format!("{nom} {URI_COURANTE} 0")).await;
            assert_eq!(
                c.reponse().await,
                vec![format!("ACK [50@0] {{{nom}}} No file exists")]
            );
        }
        c.envoyer("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn binarylimit_change_la_taille_des_tranches_de_cette_connexion() {
        // **Ce que la commande sert vraiment à faire.** Une pochette se
        // récupérait par tranches de 8 Kio, la valeur par défaut de MPD : une
        // image de 500 Kio demandait soixante-deux allers-retours. Un client
        // qui annonce accepter plus doit en recevoir plus — et la valeur ne
        // vaut que pour **sa** connexion.
        let (s, _rx) = serveur().await;
        let attendus = avec_pochette(&s.etat, TAILLE).await;
        let mut c = s.client_pret().await;

        c.envoyer("binarylimit 32768").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);

        let r = c.recuperer("albumart", URI_COURANTE).await;
        assert_eq!(r.image, attendus);
        // TAILLE tient sous 32 Kio : une seule tranche, là où le défaut en
        // demandait trois.
        assert_eq!(r.tailles, vec![TAILLE], "la tranche demandee doit etre honoree");

        // Un second client, qui n'a rien demandé, garde le défaut : la limite
        // est un fait sur la connexion.
        let mut autre = s.client_pret().await;
        let r2 = autre.recuperer("albumart", URI_COURANTE).await;
        assert_eq!(r2.tailles.first(), Some(&MAX_TRANCHE));
    }

    #[tokio::test]
    async fn une_pochette_annoncee_mais_pas_encore_arrivee_est_attendue_et_servie() {
        // **La correction de « la pochette disparaît au changement de piste ».**
        // Le cœur envoie l'état d'abord, les octets ensuite : le client est
        // réveillé par cette trame et demande l'image dans la foulée, pendant
        // que le greffon tient encore celle d'avant — ou rien du tout. Il
        // recevait « No file exists », et M.A.L.P., qui mémorise l'absence par
        // piste, ne redemandait jamais.
        //
        // Ici la demande arrive **avant** les octets, et doit quand même
        // aboutir.
        let (s, _rx) = serveur().await;
        s.etat.appliquer_etat(trame_avec_pochette()).await;
        let mut c = s.client_pret().await;

        let etat = s.etat.clone();
        let attendus = crate::etat::cover_de_test(HREF, TAILLE).bytes;
        // La pochette arrive pendant que la demande patiente. Une tâche à part,
        // parce que c'est exactement la concurrence réelle : deux canaux
        // distincts, l'un derrière l'autre.
        tokio::spawn(async move {
            etat.appliquer_pochette(crate::etat::cover_de_test(HREF, TAILLE)).await;
        });

        let r = c.recuperer("albumart", URI_COURANTE).await;
        assert_eq!(r.image, attendus, "l'image attendue doit finir par etre servie");
    }

    #[tokio::test(start_paused = true)]
    async fn une_pochette_annoncee_qui_narrive_jamais_finit_par_etre_refusee() {
        // Le pendant : l'attente est **bornée**. Sans cette borne, une image
        // qui n'arrive pas — un partage endormi, un 404 du Cover Art Archive —
        // laisserait le client suspendu pour toujours sur une commande dont il
        // attend une réponse.
        //
        // Horloge simulée : tokio avance le temps virtuel dès que tout est en
        // attente, donc ce test ne coûte pas les trois secondes réelles et ne
        // suppose aucune durée d'exécution.
        let (s, _rx) = serveur().await;
        s.etat.appliquer_etat(trame_avec_pochette()).await;
        let mut c = s.client_pret().await;

        c.envoyer(&format!("albumart {URI_COURANTE} 0")).await;

        assert_eq!(
            c.reponse().await,
            vec!["ACK [50@0] {albumart} No file exists".to_string()],
            "l'attente doit finir par rendre le refus ordinaire"
        );
    }

    #[tokio::test]
    async fn une_reponse_binaire_dans_une_liste_est_refusee_a_son_rang() {
        // MPD l'autorise, nous non : voir la justification sur place dans
        // `servir`. Le refus arrive **à l'accumulation**, donc le `status` qui
        // précède n'a pas été exécuté — c'est ce que l'absence de `volume:`
        // prouve.
        let (s, _rx) = serveur().await;
        avec_pochette(&s.etat, TAILLE).await;
        let mut c = s.client_pret().await;

        c.envoyer("command_list_begin").await;
        c.envoyer("status").await;
        c.envoyer(&format!("albumart {URI_COURANTE} 0")).await;
        let recues = c.reponse().await;

        assert_eq!(recues, vec!["ACK [5@1] {albumart} not allowed in command list".to_string()]);
        assert!(!recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");
        // L'état de liste a été rendu, et la commande répond bien hors liste :
        // le refus ne condamne pas la commande, seulement son emballage.
        let r = c.recuperer("albumart", URI_COURANTE).await;
        assert_eq!(r.image.len(), TAILLE);
    }

    #[tokio::test]
    async fn une_pochette_qui_arrive_reveille_un_dormeur_sur_player() {
        // Le bout en bout du réveil, sur une chaussette. Il est **nécessaire**
        // et non cosmétique : le cœur envoie l'état d'abord, donc un client
        // réveillé par la seule trame d'état demande son image trop tôt et
        // reçoit un refus. Sans ce second réveil, il ne saurait jamais que
        // l'image est arrivée.
        //
        // Sans horloge : la boucle pousse des pochettes jusqu'à ce que le
        // dormeur réponde, et une implémentation qui ne réveille pas fait
        // *pendre* le test.
        let (s, _rx) = serveur().await;
        s.etat.appliquer_etat(trame_avec_pochette()).await;
        let mut c = s.client_pret().await;
        c.envoyer("idle player").await;

        let mut i = 0usize;
        let premiere = loop {
            tokio::select! {
                biased;
                lue = c.lignes.next_line() => {
                    break lue.unwrap().expect("le serveur a ferme la connexion");
                }
                // Deux tailles alternées : chaque poussée est donc un
                // changement réel, que la déduplication ne peut pas avaler.
                () = s.etat.appliquer_pochette(
                    crate::etat::cover_de_test(HREF, 1000 + (i % 2) * 500),
                ) => {
                    i += 1;
                    tokio::task::yield_now().await;
                }
            }
        };
        assert_eq!(premiere, "changed: player");
        assert_eq!(c.recevoir().await, "OK");
    }
}
