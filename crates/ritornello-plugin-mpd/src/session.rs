//! Le dialogue avec un client : la seule partie du greffon qui touche une
//! chaussette.
//!
//! Une tâche par connection, et c'est l'architecture entière : toute question se
//! répond depuis l'état partagé (une prise de verrou en playback), toute action
//! est un envoi sur un canal borné. Aucune session n'a donc à wait le cœur,
//! donc **aucune ne peut retenir une autre** — un client endormi dans un `idle`
//! ne coûte qu'une tâche en attente.
//!
//! Les listes de commands et `idle` vivent ici et non dans `commands.rs`,
//! parce que ce sont des faits sur la **connection** et non sur une commande :
//! `command_list_begin` ne fait rien d'autre que changer ce que les lines
//! suivantes veulent dire, et `idle` ne fait rien d'autre que suspendre la
//! playback des lines. `commands.rs` remainder pur, et se teste sans socket.

use crate::commands::{
    cover_announced_but_missing, handle, Binary, Outcome, MAX_CHUNK,
    MAX_CHUNK_CAP,
};
use crate::state::{SharedState, Snapshot, Subsystem};
use crate::protocol::{ack, split, line, Ack};
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
/// version basse leur ferait renoncer à des commands qu'on gère réellement.
/// Le risque inverse (annoncer trop haut) est borné par `commands`, qui dit la
/// vérité, et par les `ACK 5` du remainder.
const ANNOUNCED_VERSION: &str = "0.23.5";

/// Plafond de **sessions simultanées**.
///
/// Le multiplicateur de tout ce qui suit : chaque cap ci-dessous bounded une
/// connection, et rien ne bornait le nombre de connexions. Or le résidu réel
/// d'une session peut atteindre une dizaine de mébioctets (voir `MAX_RESPONSE`
/// pour le calcul), donc cent sessions font le gigaoctet de l'appareil — la
/// panne que tous ces plafonds existent pour éviter, atteinte par le seul
/// path qu'ils laissaient ouvert.
///
/// **16, justifié par la population réelle** : un téléphone, un deuxième
/// téléphone, `mpc` sur l'appareil, à la rigueur une tablette et un client de
/// bureau — cinq au grand maximum, et les clients MPD n'ouvrent qu'une
/// connection chacun (une seconde parfois, pour tenir un `idle` à part). 16
/// laisse donc trois fois la marge de tout usage légitime, tout en bornant le
/// pire cas à un peu moins de 200 Mio là où il était sans bounded.
///
/// **Et ce n'est pas une protection contre la seule malveillance** : un client
/// qui fuit ses connexions — qui en rouvre une à chaque reprise de réseau sans
/// fermer la précédente — y arrive par accident, et c'est même le cas le plus
/// probable des deux. Le refus est alors ce qui garde l'appareil en vie pendant
/// que ce client se comporte mal, et le journal nomme le cap pour que la
/// cause se lise sans deviner.
///
/// Un cap et non une file d'attente : faire patienter une connection
/// derrière un cap atteint garderait un descripteur ouvert et laisserait le
/// client croire qu'il est servi. Reject aussitôt lui dit la vérité, et c'est
/// une réponse qu'un client sait interpréter — un serveur MPD injoignable est
/// un état que tous savent afficher.
///
/// **Les 200 Mio ci-dessus ne comptent que le path texte, et il faut le dire
/// ici** : depuis les pochettes, ce cap multiplie aussi
/// `COVER_MAX_BYTES`. Une session qui répond `albumart` retient la génération
/// d'image qu'elle sert pendant tout son `write_all`, donc seize clients
/// immobiles épinglent seize générations — 16 × 20 Mio = **320 Mio**, plus celle
/// que l'état tient lui-même, soit **340 Mio**. Le calcul complet et ce qui
/// n'est pas mitigé sont sur `commands::MAX_CHUNK` ; ce qu'il faut retenir
/// ici est que ce cap-ci est le seul facteur qui bounded ce produit.
const MAX_SESSIONS: usize = 16;

/// Plafond de commands accumulées dans une liste, avant `command_list_end`.
///
/// Ce n'est pas de la prudence décorative : entre `command_list_begin` et son
/// `end`, la session **mémorise** chaque line sans rien exécuter, donc un
/// client (ou un scanner de port bavard) qui n'envoie jamais le `end` fait
/// croître un `Vec` sans bounded dans un processus qui tourne sur un Pi. MPD a la
/// même bounded, exprimée en bytes (`max_command_list_size`, 2 Mio par défaut) ;
/// ici c'est un nombre de commands, plus simple à justifier et suffisant pour
/// le même effet. 2048 est très au-delà de ce qu'un client réel envoie —
/// M.A.L.P. groupe une dizaine de commands.
const MAX_LIST_COMMANDS: usize = 2048;

/// Plafond des **bytes** accumulés par une liste de commands.
///
/// Le compte de commands ne suffit pas : une line accumulée peut peser
/// jusqu'à `MAX_LINE` en toute légitimité, donc 2048 commands bornent la
/// mémoire à 16 Mio par connection — l'order de grandeur même que `MAX_LINE`
/// existe pour interdire. C'est d'ailleurs en bytes, et non en commands, que
/// MPD exprime la sienne (`max_command_list_size`, 2 Mio par défaut).
///
/// 256 Kio, soit 2048 commands de 128 bytes en moyenne : un `setvol 30` en
/// pèse dix, et la plus longue commande réaliste — un name entre guillemets — en
/// pèse quelques centaines. Très au-dessus de ce qu'un client envoie.
///
/// **Ce cap compte des bytes de texte, pas des bytes de tas**, et l'écart
/// n'est pas cosmétique : ce qui est accumulé est un `Vec<Vec<String>>`, et
/// `split` alloue une chaîne **par jeton**. Une line légale de 8 Kio faite
/// de `"a a a a …"` devient ainsi ~4096 `String` d'un caractère, chacune coûtant
/// ses 24 bytes de structure dans le `Vec` plus une allocation que l'allocateur
/// arrondit — de l'order de 50 bytes pour un caractère utile, soit un facteur
/// proche de trente. 256 Kio comptés peuvent donc peser plusieurs mébioctets
/// réels. Le cap n'en remainder pas moins un cap ; c'est son unité qu'il ne
/// faut pas confondre avec de la mémoire, et `MAX_SESSIONS` est ce qui bounded le
/// produit.
const MAX_LIST_BYTES: usize = 256 * 1024;

/// Plafond des **bytes** d'une réponse, avant l'écriture.
///
/// C'est la même fuite que `MAX_LINE` prise par l'autre bout, et le cap de
/// commands d'une liste ne la bounded pas du tout : il bounded les commands, pas
/// ce qu'elles **produisent**. Une liste de 2048 `playlistinfo` — 26 Kio
/// d'entrée, une boucle, aucune malveillance — rend quatre lines par entrée de
/// file, soit jusqu'à 1020 lines par commande à `preset_count` maximal (255) :
/// deux millions de `String` d'un côté, et surtout **une allocation contiguë de
/// plusieurs dizaines de mébioctets** au moment de mettre tout cela à plat pour
/// le `write_all`. Sur un Pi 2 B, une demande contiguë de cette size échoue
/// contre une mémoire fragmentée bien avant que le total ne soit atteint.
///
/// 1 Mio : la plus longue réponse légitime est un `playlistinfo` complet — 255
/// entrées de quatre lines, une quinzaine de kibioctets en tout, `preset_count`
/// étant un `Option<u8>` — et le cap en laisse donc passer une soixantaine
/// dans une seule liste.
///
/// **Ce que ce cap bounded, et ce qu'il ne bounded pas.** Il est vérifié après
/// chaque commande du lot et non à chaque line, donc le dépassement est
/// constaté à au plus une réponse de commande près (une quinzaine de
/// kibioctets), et le bras `Outcome::Cancel` empile son `list_OK` sans le
/// vérifier du tout — borné par le compte de commands, donc 2048 × 8 bytes,
/// soit 16 Kio. Le résidu au-delà du cap est ainsi d'une trentaine de
/// kibioctets, et non « une réponse de commande ».
///
/// **Deux multiplicateurs à connaître pour recalculer ce que coûte vraiment une
/// session** — les énoncer vaut mieux que d'inscrire un nombre que le prochain
/// changement démentira :
///
/// 1. **La copie simultanée.** `write` met la réponse à plat dans une `String`
///    dont il réserve la capacité exacte *pendant que* `Response.lines` vit
///    encore : le texte existe donc deux fois à cet instant. Le pic **compté**
///    d'une session est ainsi ≈ 2 × 1 Mio (la réponse et sa copie) + 256 Kio
///    (la liste accumulée) ≈ 2,3 Mio, et non 1,3.
/// 2. **Bytes de texte contre bytes de tas.** Comme pour `MAX_LIST_BYTES`,
///    ces plafonds comptent du texte alors que les structures tiennent des
///    `String` : une réponse d'un mébioctet en lines d'une vingtaine d'bytes
///    est ~40 000 `String`, soit le double en tas. Bout à bout, une session
///    poussée à ses deux plafonds tient de l'order de **6 à 12 Mio réels**.
///
/// Le lever qui compte, si ce chiffre devenait gênant, est `MAX_LIST_BYTES`
/// (le terme dominant, à cause du facteur trente des jetons d'un caractère), et
/// non `MAX_SESSIONS` — mais c'est `MAX_SESSIONS` qui bounded le produit.
const MAX_RESPONSE: usize = 1024 * 1024;

/// Plafond d'une **line** de commande, en bytes.
///
/// Sans lui, c'est la dernière surface non bornée d'un port ouvert sur tout le
/// réseau local : un client qui se connecte et envoie des bytes **sans jamais
/// send_frame de retour à la line** fait allouer le greffon jusqu'à ce que
/// l'allocateur renonce. Sur cet appareil — un Pi 2 B, un gigaoctet partagé
/// entre mpv, le cœur, l'IHM web et huit plugins — cela n'emporte pas
/// seulement le greffon, cela emporte la musique. Et cela ne demande aucune
/// malveillance : un scanner de port ou un client bogué le fait par accident,
/// et le port est atteignable de tout le réseau local sans mot de passe.
///
/// 8 Kio est deux fois le buffer d'entrée de MPD lui-même (4 Kio) et un order
/// de grandeur au-dessus de la plus longue line légitime du protocol — un name
/// de liste entre guillemets dans une liste de commands, quelques centaines
/// d'bytes au pire. Très au-dessus du réel, très en dessous de ce qui coûte :
/// un buffer de line par session, donc 128 Kio pour les `MAX_SESSIONS`
/// autorisées.
///
/// (Cette doc a porté un temps la phrase « même cent connexions simultanées ne
/// réservent ainsi qu'un mégaoctet ». Elle était vraie quand ce buffer était
/// toute l'histoire, et elle est devenue fausse d'un facteur mille dès que la
/// liste accumulée et la réponse composée ont eu leurs propres plafonds : le
/// buffer de line n'est plus qu'un terme mineur du résidu d'une session. Voir
/// `MAX_RESPONSE` pour le calcul complet.)
const MAX_LINE: usize = 8 * 1024;

/// Le player de lines de la session : un `BufReader`, plus le cap.
///
/// Écrit à la main (`fill_buf`/`consume`) plutôt qu'avec `BufReader::lines()`,
/// pour la seule raison qui vaille : `lines()` accumule jusqu'au `\n` **sans
/// bounded**. Voir `MAX_LINE`.
struct BoundedReader {
    playback: BufReader<OwnedReadHalf>,
    /// La line lue pendant une attente `idle` et **pushback en file** pour la
    /// boucle de `serve`.
    ///
    /// C'est le mécanisme du « `noidle` implicite » : une commande reçue
    /// pendant un `idle` annule l'attente (`OK` nu) *puis* doit être exécutée
    /// comme n'importe quelle autre line — donc repassée à l'aiguillage
    /// complet de `serve`, listes de commands et lines illisibles comprises,
    /// plutôt que réinterprétée à moitié dans `wait_idle`.
    ///
    /// Une seule line suffit, et un seul emplacement le dit : elle est pushback
    /// juste après avoir été lue, et consommée au tour de boucle suivant, donc
    /// deux ne peuvent pas coexister.
    pushback: Option<String>,
    /// Les bytes de la line en cours, entre deux `\n`.
    ///
    /// Il vit dans la structure et non dans la pile de `next_line`, et ce
    /// n'est pas un détail : c'est ce qui rend cette fonction **sûre à
    /// l'annulation**, exactement comme le buffer de `tokio::io::Lines`.
    /// `wait_idle` la met dans un `select!` avec le réveil, donc elle est
    /// abandonnée en cours de route chaque fois qu'un dormeur se réveille — si
    /// le buffer était local, la moitié de line déjà lue partirait avec lui,
    /// et la commande suivante serait tronquée.
    buffer: Vec<u8>,
}

impl BoundedReader {
    fn new(playback: OwnedReadHalf) -> Self {
        Self { playback: BufReader::new(playback), pushback: None, buffer: Vec::new() }
    }

    /// Remet une line déjà lue devant le stream. Voir `pushback`.
    fn put_back(&mut self, line: String) {
        debug_assert!(self.pushback.is_none(), "deux lines remises en file a la fois");
        self.pushback = Some(line);
    }

    /// La line suivante sans son `\n`, ou `None` à la fin du stream.
    ///
    /// Une line qui dépasse `MAX_LINE` est une **erreur**, donc la fin de la
    /// session : c'est ce que fait MPD, et c'est le seul choix défendable ici.
    /// Un `ACK` supposerait de nommer la commande fautive — impossible, la
    /// line est tronquée — puis de jeter un nombre inconnu d'bytes jusqu'au
    /// prochain `\n`, c'est-à-dire de garder une connection qui a déjà quitté le
    /// protocol. Close est immédiat, défini, et journalisé par `accepter`.
    async fn next_line(&mut self) -> Result<Option<String>> {
        // La line pushback passe avant la chaussette, et **sans point
        // d'attente** : ce `take` et ce `return` sont dans le même sondage, si
        // bien qu'une annulation ne peut pas se glisser entre les deux et
        // perdre la line.
        if let Some(line) = self.pushback.take() {
            return Ok(Some(line));
        }
        loop {
            let dispo = self.playback.fill_buf().await?;
            if dispo.is_empty() {
                // Eof de stream. Une dernière line sans `\n` est rendue quand
                // même, comme le faisait `Lines` : un client qui ferme sa
                // moitié écriture juste après une commande doit voir cette
                // commande traitée.
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(&mut self.buffer);
                return Ok(Some(Self::finish(line)?));
            }
            match dispo.iter().position(|octet| *octet == b'\n') {
                Some(fin) => {
                    // Le cap est vérifié **avant** de recopier : un
                    // dépassement ne doit pas d'abord allouer ce qu'il refuse.
                    // Contrôlé dans les deux bras, et pas seulement dans celui
                    // sans `\n`, pour que la bounded tienne quelle que soit la
                    // capacité du `BufReader`.
                    if self.buffer.len() + fin > MAX_LINE {
                        anyhow::bail!("command line longer than {MAX_LINE} bytes");
                    }
                    self.buffer.extend_from_slice(&dispo[..fin]);
                    self.playback.consume(fin + 1);
                    let line = std::mem::take(&mut self.buffer);
                    return Ok(Some(Self::finish(line)?));
                }
                None => {
                    let recu = dispo.len();
                    if self.buffer.len() + recu > MAX_LINE {
                        anyhow::bail!(
                            "command line longer than {MAX_LINE} bytes without a newline"
                        );
                    }
                    self.buffer.extend_from_slice(dispo);
                    self.playback.consume(recu);
                }
            }
        }
    }

    /// Les bytes d'une line en `String`.
    ///
    /// Un `\r` terminal est retiré : `\r\n` est ce qu'envoient les clients
    /// écrits sur Windows, et sans cela `ping\r` serait une commande inconnue.
    /// C'est aussi ce que faisait `Lines` — le perdre en changeant de player
    /// aurait été une régression qu'aucun test existant ne voyait.
    ///
    /// Un octet non UTF-8 est une erreur, donc la fin de la session : là aussi
    /// le comportement de `Lines`, conservé tel quel. Le protocol MPD est
    /// textuel, et une commande dont les bytes ne forment pas du texte ne se
    /// découpe pas.
    fn finish(mut line: Vec<u8>) -> Result<String> {
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Ok(String::from_utf8(line)?)
    }
}

/// Les lines d'une réponse en cours de composition, et leur poids en bytes.
///
/// Le compte est tenu au fur et à mesure plutôt que recalculé : la vérification
/// du cap a lieu après chaque commande d'un lot, et resommer la réponse
/// entière à chaque fois rendrait quadratique la composition d'une liste
/// longue. Il compte le `\n` de chaque line, donc c'est exactement le nombre
/// d'bytes que `write` va poser sur la chaussette.
#[derive(Default)]
struct Response {
    lines: Vec<String>,
    bytes: usize,
}

impl Response {
    fn push(&mut self, line: String) {
        self.bytes += line.len() + 1;
        self.lines.push(line);
    }

    fn extend(&mut self, lines: Vec<String>) {
        for line in lines {
            self.push(line);
        }
    }

    /// Vrai quand la réponse dépasse `MAX_RESPONSE`.
    fn too_large(&self) -> bool {
        self.bytes > MAX_RESPONSE
    }
}

/// Ce qui n'appartient qu'à **une** connection, et voyage donc avec elle.
///
/// Regroupés parce qu'ils ont exactement la même nature — deux faits sur le
/// client, que rien ne partage entre sessions — et non pour raccourcir une
/// signature : les séparer laisserait croire qu'ils ont des durées de vie
/// différentes. L'état de liste de commands leur ressemble mais remainder dans
/// `serve` : lui ne traverse jamais un appel à `execute`, puisque c'est
/// `serve` qui décide ce qu'est un lot.
struct Connection {
    /// Les compteurs de subsystems que cette connection a déjà vus : la référence de
    /// tous ses `idle`. Lue par `execute`, avancée seulement par
    /// `wait_idle`, et pour les seuls subsystems qu'un réveil announcement.
    seen: [u64; 4],
    /// La size de chunk que ce client accepte pour les réponses binaires
    /// (voir `commands::binarylimit`). `MAX_CHUNK` tant qu'il n'a rien
    /// demandé — le défaut du protocol.
    binary_limit: usize,
}

/// Ce que la session doit faire après un lot de commands.
enum Next {
    /// Continue à read des lines.
    Continue,
    /// Close la connection : `close`, ou une moitié `input` morte.
    Close,
}

/// Accepte les connexions, chacune dans sa propre tâche, et **se relie quand
/// les réglages changent**.
///
/// La page d'admin disait « le changement ne prend effet qu'au redémarrage du
/// greffon », et c'était vrai : le socket était lié une fois pour toutes dans
/// `main`. Ce n'est plus le cas — un enregistrement réussi push_cover la nouvelle
/// configuration sur `config_rx`, et cette boucle lie le nouveau couple
/// adresse/port.
///
/// **Trois décisions, chacune pour une raison :**
///
/// - **L'ancien écouteur n'est lâché qu'une fois le nouveau lié.** Si le port
///   demandé est déjà pris, ou l'adresse absente de la machine, l'appareil
///   continue de serve là où il servait : un réglage fautif ne doit pas rendre
///   le serveur MPD injoignable, alors même que la page qui l'a provoqué est
///   toujours ouverte. L'échec part au journal, et la page dira l'inverse — le
///   fichier, lui, a bien été enregistré. C'est le compromis assumé : la
///   validation du port ne peut pas anticiper qu'il est occupé.
/// - **Les sessions déjà ouvertes ne sont pas coupées.** Elles tiennent leur
///   propre `TcpStream`, que la fermeture de l'écouteur ne touche pas. Un
///   téléphone en train d'écouter garde donc sa connection jusqu'à ce qu'il la
///   ferme lui-même, là où un vrai redémarrage de MPD la lui aurait arrachée.
/// - **Le cap de sessions traverse les reliaisons.** Le sémaphore vit ici,
///   hors de la boucle : le recréer à chaque changement de réglage rendrait
///   `MAX_SESSIONS` contournable par une simple sauvegarde répétée.
///
/// `accept` est annulable sans perte (c'est la garantie de tokio), donc perdre
/// la course du `select!` ne fait jamais tomber une connection déjà acceptée.
pub async fn listen(
    listener: TcpListener,
    mut config_rx: tokio::sync::watch::Receiver<crate::config::Config>,
    state: Arc<SharedState>,
    cmd_tx: mpsc::Sender<InputMessage>,
) {
    let slots = Arc::new(Semaphore::new(MAX_SESSIONS));
    let mut listener = listener;
    loop {
        tokio::select! {
            // Ne rend jamais la main : sa seule sortie est d'être annulée par
            // l'autre bras.
            () = accept_loop(&listener, &slots, &state, &cmd_tx) => {}
            change = config_rx.changed() => {
                if change.is_err() {
                    // La moitié admin a disparu (le greffon s'arrête) : plus
                    // aucune reliaison ne viendra, mais il remainder à serve.
                    tracing::debug!("mpd settings channel closed; keeping the current socket");
                    accept_loop(&listener, &slots, &state, &cmd_tx).await;
                    return;
                }
                let c = config_rx.borrow_and_update().clone();
                match TcpListener::bind((c.listen.as_str(), c.port)).await {
                    Ok(neuf) => {
                        tracing::info!("mpd server now listening on {}:{}", c.listen, c.port);
                        listener = neuf;
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
/// épuisé ou une connection réinitialisée avant l'`accept` ne doit pas emporter
/// le serveur, sinon le port remainder ouvert dans un processus qui n'écoute plus.
///
/// Le sémaphore des slots est **passé** et non créé ici : il vit dans
/// `listen`, pour que le cap de sessions traverse les reliaisons (voir sa
/// doc). Une place par session, rendue quoi qu'il arrive — le permis vit dans
/// la tâche, donc il repart avec elle, y compris si elle panique, puisque c'est
/// son `Drop` qui le rend. Un `Semaphore` plutôt qu'un compteur atomique pour
/// exactement cette raison : un compteur demanderait de se souvenir de le
/// décrémenter sur chaque path de sortie, et le jour où l'un serait oublié
/// l'appareil refuserait tout le monde après seize connexions.
async fn accept_loop(
    listener: &TcpListener,
    slots: &Arc<Semaphore>,
    state: &Arc<SharedState>,
    cmd_tx: &mpsc::Sender<InputMessage>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, adresse)) => {
                // `try_acquire_owned` et non `acquire` : au-delà du cap la
                // connection est **refusée**, pas mise en attente. Voir
                // `MAX_SESSIONS`.
                let Ok(place) = slots.clone().try_acquire_owned() else {
                    tracing::warn!("mpd refusing {adresse}: {MAX_SESSIONS} sessions already open");
                    drop(stream);
                    continue;
                };
                tracing::info!("mpd client connected from {adresse}");
                let state = state.clone();
                let cmd_tx = cmd_tx.clone();
                // Une tâche par client, détachée : c'est ce qui rend une
                // session incapable d'en retenir une autre. Le `spawn` ne
                // rend rien à surveiller — une session qui finit n'a rien à
                // dire de plus que ce qu'elle journalise ici.
                tokio::spawn(async move {
                    match serve(stream, state, cmd_tx).await {
                        Ok(()) => tracing::info!("mpd client {adresse} disconnected"),
                        Err(e) => tracing::info!("mpd session with {adresse} ended: {e}"),
                    }
                    // Explicite, alors que la portée s'en chargerait : c'est la
                    // line qui dit qu'une place se libère ici, et la chercher
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

/// Le dialogue d'une connection, du premier octet écrit au dernier lu.
///
/// L'état de liste vit dans cette fonction et nulle part ailleurs : il n'a de
/// sens que pour cette connection, et deux clients dont l'un est au milieu d'une
/// liste ne se voient pas.
pub async fn serve(stream: TcpStream, state: Arc<SharedState>, cmd_tx: mpsc::Sender<InputMessage>) -> Result<()> {
    let (playback, mut writer) = stream.into_split();
    let mut lines = BoundedReader::new(playback);

    // La bannière part sans qu'on demande rien : c'est le protocol, et un
    // client attend cette line avant d'écrire quoi que ce soit.
    writer.write_all(format!("OK MPD {ANNOUNCED_VERSION}\n").as_bytes()).await?;

    // **Les compteurs que cette connection a déjà vus, lus une fois pour
    // toutes.** C'est la référence de tous ses `idle`, et elle vit ici parce
    // que c'est un fait sur la **connection** — comme l'état de liste juste en
    // dessous, et pour la même raison.
    //
    // Les read à la bannière et les porter, plutôt que de les relire dans
    // l'`Snapshot` de chaque commande `idle`, est la correction d'un défaut
    // réel : la playback par commande avalait tout ce qui avait bougé entre la
    // réponse précédente et la line `idle`, c'est-à-dire pendant la seule
    // fenêtre où un client MPD n'écoute pas. Le vrai MPD accumule ses drapeaux
    // par connection depuis la connection ; pour `stored_playlist`, avaler un
    // événement laisse `listplaylists` périmé jusqu'au prochain changement de
    // sources_catalog — donc potentiellement pour toujours. Voir `state::versions`.
    //
    // Un `idle` immédiatement réveillé sur un changement que ce client avait
    // peut-être déjà lu par un `status` est le sens d'erreur acceptable : un
    // réveil superflu lui coûte une interrogation redondante, un réveil
    // manquant lui coûte la justesse de son écran.
    let mut connection = Connection { seen: state.versions().await, binary_limit: MAX_CHUNK };

    // Les commands accumulées d'une liste en cours, `None` hors liste.
    // Un `Option<Vec<_>>` plutôt qu'un `Vec` plus un booléen : « pas dans une
    // liste » et « dans une liste encore clear » sont deux états différents, et
    // un `command_list_end` reçu hors liste doit être refusé comme une
    // commande inconnue plutôt que rendre un `OK` de complaisance.
    let mut liste: Option<Vec<Vec<String>>> = None;
    let mut avec_ok = false;
    // Les bytes déjà accumulés par la liste en cours. Remis à zéro à chaque
    // ouverture, comme `avec_ok`.
    let mut octets_liste = 0usize;

    while let Some(brute) = lines.next_line().await? {
        let args = match split(&brute) {
            Ok(args) => args,
            Err(code) => {
                // Une line illisible est un `ACK`, jamais une rupture : un
                // client qui a mal cité un name de station doit pouvoir
                // continuer sans se reconnecter.
                //
                // Une liste en cours est en revanche abandonnée : il y manque
                // une commande, donc l'exécuter plus tard exécuterait un lot
                // qui n'est pas celui que le client a écrit.
                let index = liste.as_ref().map_or(0, Vec::len);
                liste = None;
                let refus = ack(code, index, first_word(&brute), "invalid argument");
                write(&mut writer, &[refus]).await?;
                continue;
            }
        };
        // `""` pour une line clear : `handle` la refuse déjà (elle est totale
        // par construction), donc rien ici n'a besoin d'un cas à part.
        let mot = args.first().map_or("", String::as_str);

        if liste.is_some() {
            match mot {
                "command_list_end" => {
                    let lot = liste.take().unwrap_or_default();
                    match execute(&mut lines, &mut writer, &state, &cmd_tx, &lot, avec_ok, &mut connection)
                        .await?
                    {
                        Next::Continue => {}
                        Next::Close => break,
                    }
                }
                // `idle` dans une liste : MPD l'interdit, et pour une bonne
                // raison — l'accepter demanderait de suspendre une liste à
                // moitié écrite, dont le `OK` final ne peut pas partir avant
                // le réveil. L'index porté est le **rang** qu'`idle` occupe
                // dans la liste, sinon le client ne sait pas laquelle de ses
                // commands a été refusée.
                //
                // Refus **à l'accumulation** et non à l'exécution : la liste
                // ne pourra jamais être exécutée, donc y exécuter d'abord les
                // commands qui précèdent émettrait de vraies actions (un
                // `next`, un `setvol`) au name d'un lot que le client ne verra
                // jamais aboutir.
                "idle" => {
                    let index = liste.as_ref().map_or(0, Vec::len);
                    liste = None;
                    let refus = ack(Ack::Unknown, index, "idle", "not allowed in command list");
                    write(&mut writer, &[refus]).await?;
                }
                // **Une réponse binaire dans une liste de commands : MPD
                // l'autorise, ce greffon la refuse.** Trois raisons, dans
                // l'order où elles pèsent :
                //
                // 1. Elle romprait la discipline d'écriture de cette session.
                //    `execute` compose *tout* un lot en texte, vérifie le
                //    cap, puis écrit **une fois** — ce qui garantit qu'une
                //    réponse à moitié écrite n'est jamais lue comme complète.
                //    Insérer des bytes au milieu obligerait soit à vider
                //    l'accumulateur avant chaque image (donc à renoncer à cette
                //    garantie), soit à faire passer les bytes par
                //    l'accumulateur de texte — ce qui est impossible, ils ne
                //    sont pas de l'UTF-8.
                // 2. Elle rouvrirait l'amplificateur que la Task 8 a fermé :
                //    2048 `albumart` dans une liste, c'est 26 Kio d'entrée pour
                //    16 Mio écrits, **accumulés avant la première écriture**.
                //    C'est exactement la mesure qui a fait naître
                //    `MAX_RESPONSE`, sur ce même port sans authentification.
                // 3. Personne n'en a besoin. Une cover se récupère par une
                //    suite d'allers-retours dont chaque offset dépend du `size:`
                //    que le précédent a rendition — or une liste de commands est
                //    envoyée **entière avant** d'être lue. Le client ne peut
                //    donc pas composer le lot qu'il faudrait.
                //
                // Refus à l'accumulation, comme `idle` et pour la même raison :
                // le lot ne pourra jamais aboutir, donc exécuter d'abord les
                // commands qui le précèdent émettrait de vraies actions au name
                // d'un lot que le client ne verra jamais.
                "albumart" | "readpicture" => {
                    let index = liste.as_ref().map_or(0, Vec::len);
                    liste = None;
                    let refus = ack(Ack::Unknown, index, mot, "not allowed in command list");
                    write(&mut writer, &[refus]).await?;
                }
                _ => {
                    let index = liste.as_ref().map_or(0, Vec::len);
                    // Deux bounds pour un seul refus : le nombre de commands
                    // (qui bounded le travail d'un lot) et leur poids en bytes
                    // (qui bounded la mémoire, une line accumulée pouvant peser
                    // jusqu'à `MAX_LINE`). Voir les deux constantes.
                    octets_liste += brute.len() + 1;
                    if index >= MAX_LIST_COMMANDS || octets_liste > MAX_LIST_BYTES {
                        liste = None;
                        let refus = ack(Ack::Unknown, index, mot, "list too large");
                        write(&mut writer, &[refus]).await?;
                    } else if let Some(accumule) = liste.as_mut() {
                        // Accumulé sans être regardé : un `command_list_begin`
                        // imbriqué, un mot inconnu ou une line clear seront
                        // refusés par `handle` à l'exécution, à leur rang, et
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
                match execute(&mut lines, &mut writer, &state, &cmd_tx, lot, false, &mut connection)
                    .await?
                {
                    Next::Continue => {}
                    Next::Close => break,
                }
            }
        }
    }
    Ok(())
}

/// Exécute un lot — une commande seule, ou les commands d'une liste — et
/// **écrit lui-même** la réponse.
///
/// Un seul path pour les deux cas : une commande hors liste est un lot d'une
/// commande avec `avec_ok` faux. C'est ce qui garantit qu'une liste répond
/// exactement comme la suite des commands qu'elle contains, à `list_OK` près.
///
/// `lines` n'est là que pour `idle` : c'est la seule issue qui a besoin de
/// continuer à read (le `noidle` qui l'annule, ou la commande qui la remplace)
/// avant d'avoir répondu.
///
/// `connection` porte ce qui n'appartient qu'à ce client : la référence de
/// compteurs de ses `idle` et la size de chunk qu'il accepte (voir
/// `Connection`).
async fn execute(
    lines: &mut BoundedReader,
    writer: &mut OwnedWriteHalf,
    state: &SharedState,
    cmd_tx: &mpsc::Sender<InputMessage>,
    lot: &[Vec<String>],
    avec_ok: bool,
    connection: &mut Connection,
) -> Result<Next> {
    let Connection { seen, binary_limit } = connection;
    let mut sortie = Response::default();
    for (index, args) in lot.iter().enumerate() {
        // **Un seul instantané, lu avant `handle`.** Une seule prise de
        // verrou pour tout ce que la réponse publie : la read en deux fois
        // laisserait `status` se contredire au milieu de lui-même.
        //
        // **Ses compteurs ne servent pas de référence à un `idle`, et c'est le
        // point.** Ils décrivent l'instant de *cette* commande ; la référence
        // d'un `idle` est celle que la connection porte depuis sa bannière. Les
        // confondre — ce que ce code faisait — avale tout changement survenu
        // entre la réponse précédente et la line `idle`, et le commentaire qui
        // vivait ici affirmait le contraire : rien dans cette playback ne rend
        // « le réveil manqué impossible ». C'est la comparaison d'`wait`
        // contre la référence de la connection qui l'interdit.
        let mut instantane = state.read().await;
        // **La seule attente que ce module s'autorise avant de handle**, et
        // elle répare la cover qui disparaissait à chaque changement de
        // piste : voir `cover_announced_but_missing` et `wait_cover`.
        if cover_announced_but_missing(&instantane, args) {
            instantane = wait_cover(state, instantane).await;
        }
        match handle(&instantane, index, args, *binary_limit) {
            Outcome::Reply { lines: rendues, cmds } => {
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
                        return Ok(Next::Close);
                    }
                }
                state.acknowledge_optimistic(&cmds).await;
                sortie.extend(rendues);
                if avec_ok {
                    sortie.push("list_OK".to_string());
                }
                // Le cap de réponse, vérifié ici parce que c'est le seul
                // endroit où la réponse grandit. Rien n'a encore été écrit, donc
                // le refus **remplace** tout ce qui était composé : le client
                // reçoit un seul terminateur pour sa requête, sa comptabilité
                // remainder juste, et la connection survit — contrairement à la line
                // trop longue, où fermer était le seul choix défendable puisqu'on
                // ne pouvait même pas nommer la commande fautive. Ici on la
                // nomme, et son rang avec elle.
                //
                // **Ce que le refus coûte, et qu'il faut dire** : les commands
                // `0..=index` ont déjà poussé leur `InputMessage` et été actées
                // optimistiquement, donc leurs effets sur l'appareil
                // **subsistent** alors que leur sortie est jetée. Un client qui
                // groupe `setvol 40` puis un gros `playlistinfo` verra donc le
                // volume changer sans recevoir la moindre line. C'est
                // exactement le compromis que MPD fait sur une erreur en milieu
                // de liste — les commands déjà exécutées le restent — et il est
                // acceptable pour la même raison : défaire ce qui est parti vers
                // le cœur n'est pas en notre pouvoir, et le client peut toujours
                // relire l'état.
                if sortie.too_large() {
                    tracing::warn!("mpd response over {MAX_RESPONSE} bytes; refusing");
                    let name = args.first().map_or("", String::as_str);
                    let refus = ack(Ack::Unknown, index, name, "response too large");
                    write(writer, &[refus]).await?;
                    return Ok(Next::Continue);
                }
            }
            // `noidle` reçu hors attente : `OK` sec, et dans une liste un
            // `list_OK` comme n'importe quelle commande sans lines.
            //
            // Empilé **sans vérifier le cap**, à la différence du bras
            // ci-dessus : huit bytes par commande, donc 16 Kio au pire pour un
            // lot entier de `noidle`, ce que le compte de commands bounded déjà.
            // C'est ce qui porte le résidu au-delà du cap à une trentaine de
            // kibioctets, et non à la seule réponse d'une commande.
            Outcome::Cancel => {
                if avec_ok {
                    sortie.push("list_OK".to_string());
                }
            }
            // `binarylimit` : la valeur est déjà bornée par `commands`, il n'y
            // a qu'à la retenir. Elle vaut pour la **suite** de cette
            // connection, y compris pour les commands qui suivent dans la même
            // liste — c'est ce que fait MPD, et c'est le seul order qui rende
            // `binarylimit` puis `albumart` groupés utilisables.
            Outcome::BinaryLimit(n) => {
                *binary_limit = n;
                if avec_ok {
                    sortie.push("list_OK".to_string());
                }
            }
            // La première erreur produit son `ACK` et **rien de ce qui suit
            // n'est exécuté** : le `for` s'arrête là. Les lines déjà
            // composées partent quand même, comme le fait MPD — un `ACK` ne
            // rétracte pas les réponses des commands qui, elles, ont abouti.
            Outcome::Reject(refus) => {
                // **Le refus est journalisé avec la commande entière**, et ce
                // n'est pas du confort. Un client qui bute sur une commande non
                // gérée n'affiche qu'un message générique — « unsupported » —
                // et l'opérateur n'a alors aucun moyen de savoir *laquelle* :
                // c'est exactement ce qui a manqué pour diagnostiquer l'échec
                // de M.A.L.P. sur la sélection d'une piste dans une liste
                // enregistrée. Les arguments comptent autant que le name : la
                // même commande peut être refusée pour sa forme.
                //
                // En `info` et non en `warn` : un refus est une réponse
                // ordinaire du protocol (un client essaie, apprend, passe à
                // autre chose), et le cœur ne retient que les `warn` pour sa
                // carte « dernières erreurs » — y verser chaque commande
                // inconnue d'un client bavard la remplirait de bruit.
                tracing::info!("mpd refused {args:?}: {refus}");
                sortie.push(refus);
                write(writer, &sortie.lines).await?;
                return Ok(Next::Continue);
            }
            // Une réponse binaire : elle est écrite **seule**, par son propre
            // path, et elle clôt la requête — pas de `OK` ajouté par la
            // suite de la boucle, `write_bytes` pose le sien.
            //
            // `sortie` est nécessairement clear ici : les deux commands
            // binaires sont refusées à l'accumulation d'une liste (voir
            // `serve`), donc le lot n'a qu'une commande. L'écrire quand même
            // garde cette fonction juste si un jour ce n'était plus le cas,
            // plutôt que d'avaler des lines — le même choix que le bras
            // `Wait` juste en dessous, pour la même raison.
            Outcome::Bytes(binaire) => {
                write(writer, &sortie.lines).await?;
                write_bytes(writer, &binaire).await?;
                return Ok(Next::Continue);
            }
            Outcome::Wait(subsystems) => {
                // `idle` n'atteint jamais ce point dans une liste : la liste
                // l'a refusé à l'accumulation. Hors liste, le lot n'a qu'une
                // commande, donc `sortie` est clear — l'écrire quand même
                // garde cette fonction juste si un jour un lot en contenait
                // plusieurs, plutôt que d'avaler des lines.
                write(writer, &sortie.lines).await?;
                return wait_idle(lines, writer, state, &subsystems, seen).await;
            }
            Outcome::Close => {
                // **`OK` puis fermeture, et c'est un choix.** MPD, lui,
                // n'écrit rien avant de fermer sur `close`. Nous répondons,
                // pour que la discipline de cette fonction n'ait aucune
                // exception : toute commande acceptée reçoit exactement un
                // terminateur. Un client qui a déjà cessé de read fait
                // simplement échouer cette écriture, ce que la session traite
                // comme une fin ordinaire — et un client qui read encore
                // trouve sa réponse là où il l'attend. La divergence est sans
                // effet observable puisque la connection se ferme dans les deux
                // cas ; ce qui compte est qu'elle soit délibérée.
                sortie.push("OK".to_string());
                write(writer, &sortie.lines).await?;
                return Ok(Next::Close);
            }
        }
    }
    // Un seul `OK` clôt le lot entier : c'est ce qui distingue une liste de
    // commands de la même suite de commands envoyées une par une.
    sortie.push("OK".to_string());
    write(writer, &sortie.lines).await?;
    Ok(Next::Continue)
}

/// Combien de temps une demande de cover patiente pour une image que
/// l'appareil a déjà annoncée.
///
/// Trois seconds, et le nombre vient des deux échéances qu'il doit couvrir :
/// le cœur bounded à `health::TIMEOUT` la playback d'un fichier de cover sur un
/// partage, et un téléchargement réseau est du même order. Au-delà, l'image
/// n'arrivera probablement pas pour cette piste, et le refus est la bonne
/// réponse.
///
/// Ce que cette attente **ne** met pas en péril : une session est une tâche à
/// elle seule, donc patienter ici ne retient personne d'autre (voir l'en-tête
/// du module). M.A.L.P. ouvre d'ailleurs une connection distincte pour les
/// images.
const COVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Attend, au plus `COVER_TIMEOUT`, que la cover annoncée arrive, et rend
/// l'instantané qui décidera.
///
/// Rend la main **dès** que l'attente n'a plus d'objet : soit l'image est là,
/// soit l'état a changé au point que la demande ne se répare plus (piste
/// suivante, arrêt). C'est `cover_announced_but_missing` qui chunk, la
/// même fonction que celle qui a décidé d'wait — un seul énoncé de la
/// condition, jamais deux à garder d'accord.
///
/// À l'échéance, rend le dernier instantané lu : `handle` en tirera le refus
/// ordinaire, comme si l'on n'avait jamais attendu.
async fn wait_cover(state: &SharedState, instantane: Snapshot) -> Snapshot {
    let args = cover_arguments(&instantane);
    let mut current = instantane;
    let deadline = tokio::time::Instant::now() + COVER_TIMEOUT;
    loop {
        // `Subsystem::Player` : c'est le sujet que `apply_cover` déplace —
        // le protocol MPD n'en a aucun pour les images, et le greffon a choisi
        // celui-là (voir sa doc). Wait sur lui, c'est wait exactement
        // l'arrivée de l'image, plus les changements de piste, qui doivent eux
        // aussi nous réveiller pour cesser d'wait.
        let attente = state.wait(&[Subsystem::Player], current.versions);
        if tokio::time::timeout_at(deadline, attente).await.is_err() {
            tracing::debug!("mpd cover did not arrive within {COVER_TIMEOUT:?}");
            return current;
        }
        current = state.read().await;
        if !cover_announced_but_missing(&current, &args) {
            return current;
        }
    }
}

/// Reconstruit les arguments d'une demande de cover pour ce qui plays.
///
/// **Reconstruits et non transportés**, et la nuance est le sujet : la boucle
/// d'attente doit réévaluer la condition contre l'état *current*, or l'URI que
/// le client a écrite désigne la piste d'alors. Les rebâtir depuis l'instantané
/// de départ garde exactement la question posée — « l'image de cette piste-là
/// est-elle arrivée ? » — et fait sortir la boucle dès que la piste change,
/// puisque l'URI ne correspondra plus.
fn cover_arguments(inst: &Snapshot) -> Vec<String> {
    vec![
        "albumart".to_string(),
        inst.state
            .preset
            .map(|p| crate::commands::uri(&inst.state.source, p))
            .unwrap_or_default(),
        "0".to_string(),
    ]
}

/// Tient l'attente d'un `idle` : rend la main au réveil, ou sur ce que le
/// client dit entre-temps.
///
/// **`subsystems` clear veut dire wait pour toujours**, et non répondre `OK`
/// tout de suite (voir la doc d'`Outcome::Wait`) : un client qui n'a nommé
/// que des sous-systèmes que ce greffon n'émet jamais (`idle database`) a posé
/// une question légitime dont la réponse est le silence. C'est `wait` qui
/// l'honore sans cas particulier — aucun sujet ne peut différer, donc elle se
/// rendort à chaque notification — et lui passer la liste telle quelle est tout
/// ce qu'il y a à faire. Répondre `OK` ferait boucler le client à pleine
/// vitesse, ce qui est exactement le contraire de ce qu'`idle` sert à éviter.
///
/// **`seen` est la référence de la connection, et cette fonction est la seule à
/// l'avancer** : sur un réveil annoncé, et pour les seuls subsystems annoncés. Le
/// vrai MPD n'efface que les drapeaux qu'il vient de rapporter, et tout avancer
/// d'un coup perdrait le changement d'un sujet qu'un `idle` suivant demandera
/// (`idle player` puis `idle mixer` en est le cas le plus court). Un `noidle`,
/// lui, n'announcement rien : il n'avance donc rien, et le changement en attente
/// ressortira à l'`idle` d'après.
async fn wait_idle(
    lines: &mut BoundedReader,
    writer: &mut OwnedWriteHalf,
    state: &SharedState,
    subsystems: &[Subsystem],
    seen: &mut [u64; 4],
) -> Result<Next> {
    // Deux issues, et il faut écouter les deux : le réveil, et ce que le client
    // dit pendant l'attente — `noidle`, la seule commande que MPD y autorise,
    // ou n'importe quelle autre line, qui vaut alors `noidle` implicite.
    // `BoundedReader::next_line` est sûre à l'annulation (son buffer vit
    // dans la structure, voir là-bas), donc la branche perdante ne perd aucun
    // octet ; et abandonner `wait` ne perd aucun réveil, puisque `seen`
    // remainder la référence et que les compteurs sont monotones.
    let wakeup = tokio::select! {
        wakeup = state.wait(subsystems, *seen) => wakeup,
        lue = lines.next_line() => {
            let Some(brute) = lue? else {
                // Le client est parti pendant son attente : rien à écrire.
                return Ok(Next::Close);
            };
            // **Une line reçue pendant l'attente clôt l'`idle` par un `OK`
            // nu.** C'est la comptabilité du protocol : un client MPD compte
            // un terminateur par requête, et il en a écrit deux — son `idle`,
            // puis cette line.
            //
            // Ce code refusait cette line par un seul `ACK` et n'écrivait rien
            // pour l'`idle` : deux requêtes se partageaient un terminateur, et
            // le client repartait **décalé de un, définitivement** — chaque
            // réponse suivante lue comme celle de sa requête précédente. Un
            // décalage silencieux et permanent, là où le choix de MPD (fermer)
            // est bruyant et auto-réparateur. Nous gardons le choix de ne pas
            // fermer — « une line fautive n'est jamais une rupture », et une
            // reconnexion coûterait au client un défaut qu'aucun journal ne lui
            // montre — mais en réparant ce qu'il avait cassé : l'invariant que
            // cette fonction énonce sur `Outcome::Close` redevient vrai, toute
            // commande acceptée reçoit exactement un terminateur.
            //
            // **Et l'`OK` nu n'est pas une forme inventée pour l'occasion** :
            // c'est déjà ce que le bras `noidle` écrivait, et c'est donc la
            // même réponse au même endroit. La correction n'étend qu'un
            // comportement existant à un second déclencheur — elle ne peut pas
            // mettre sur le fil une forme qu'un client n'aurait jamais vue.
            write(writer, &["OK".to_string()]).await?;
            // `noidle` est la seule line qui ne mérite pas de réponse propre :
            // ce n'est pas une requête mais **l'annulation de celle en cours**,
            // et l'`OK` qu'on vient d'écrire est le sien autant que celui de
            // l'`idle` — un seul terminateur pour `idle` + `noidle`, exactement
            // comme MPD. Tout le remainder est un `noidle` implicite **suivi de
            // cette commande**, vraisemblablement ce que le client voulait
            // dire : la line repart donc dans l'aiguillage complet de
            // `serve` — listes de commands, lines illisibles et `close`
            // comprises — sans qu'un seul cas soit réinterprété ici.
            //
            // Une line illisible n'est pas `noidle` (elle ne se découpe pas),
            // et c'est la conduite voulue : elle recevra son `ACK` au tour
            // suivant, comme n'importe où ailleurs.
            let est_noidle = split(&brute)
                .map(|args| args.first().is_some_and(|mot| mot == "noidle"))
                .unwrap_or(false);
            if !est_noidle {
                lines.put_back(brute);
            }
            // La référence de la connection n'avance pas : rien n'a été annoncé,
            // donc un changement survenu pendant cette attente ressortira à
            // l'`idle` suivant.
            return Ok(Next::Continue);
        }
    };
    // **Les compteurs rapportés, et eux seuls, sont consommés.** Avancer tout
    // le tableau perdrait le changement d'un sujet que cet `idle` n'a pas
    // demandé.
    for sujet in &wakeup.moved {
        seen[*sujet as usize] = wakeup.versions[*sujet as usize];
    }
    let mut reponse: Vec<String> =
        wakeup.moved.iter().map(|sujet| line("changed", subsystem_name(*sujet))).collect();
    reponse.push("OK".to_string());
    write(writer, &reponse).await?;
    Ok(Next::Continue)
}

/// Le name MPD d'un sous-système, tel qu'un `changed:` le publie.
///
/// C'est l'inverse exact de la table que `commands.rs` emploie pour read un
/// `idle` : un name qui divergerait ferait annoncer un sous-système qu'aucun
/// client ne saurait redemander. Un test le vérifie en passant chacun de ces
/// names à `idle` et en exigeant qu'il en ressorte le même sujet.
fn subsystem_name(sujet: Subsystem) -> &'static str {
    match sujet {
        Subsystem::Player => "player",
        Subsystem::Mixer => "mixer",
        Subsystem::Playlist => "playlist",
        Subsystem::StoredPlaylist => "stored_playlist",
    }
}

/// Le premier mot d'une line que `split` a refusée, pour nommer la commande
/// dans l'`ACK`. Découpé à l'espace et sans guillemets : c'est tout ce qu'on
/// peut dire d'une line mal citée, et un `{}` clear (ce que MPD écrit) laisse
/// le client sans index sur laquelle de ses lines était fautive.
fn first_word(brute: &str) -> &str {
    brute.split_whitespace().next().unwrap_or("")
}

/// Écrit une réponse d'un seul coup.
///
/// Un `write_all` par réponse et non un par line : une réponse de 51 lines
/// coûte alors un appel système au lieu de 51, et rien ne peut s'intercaler au
/// milieu — deux réponses de la même session sont écrites l'une après l'autre
/// par construction, mais une réponse à moitié écrite serait lue comme une
/// réponse complète par un client qui compte ses terminateurs.
async fn write(writer: &mut OwnedWriteHalf, lines: &[String]) -> Result<()> {
    // Capacité exacte dès le départ : sans elle, mettre à plat une réponse
    // proche du mébioctet la réallouerait une vingtaine de fois en doublant, en
    // demandant chaque fois un bloc contigu plus grand que le précédent.
    // `MAX_RESPONSE` bounded la size de ce buffer ; cette line bounded le nombre
    // de fois qu'on la demande.
    let mut buffer = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for l in lines {
        buffer.push_str(l);
        buffer.push('\n');
    }
    writer.write_all(buffer.as_bytes()).await?;
    Ok(())
}

/// Écrit une réponse **binaire** d'un seul coup : l'en-tête, les bytes bruts,
/// puis le terminateur.
///
/// La forme est celle de MPD, à l'octet près :
///
/// ```text
/// size: <size de l'image entière>
/// type: <mime>            (readpicture seulement)
/// binary: <size de cette chunk>
/// <les bytes bruts>
/// OK
/// ```
///
/// Le `\n` qui suit les bytes bruts n'est pas décoratif : c'est celui que MPD
/// écrit (`Response::WriteBinary`), et libmpdclient le consomme avant de read
/// le terminateur. L'omettre ferait read `<dernier octet>OK` comme une line
/// inconnue.
///
/// **Un seul `write_all`, comme `write`**, et la même raison : une réponse à
/// moitié écrite serait lue comme une réponse complète par un client qui compte
/// ses terminateurs. La recopie de la chunk dans le buffer coûte au plus
/// `MAX_CHUNK_CAP` bytes — soixante-quatre kibioctets si le client a
/// relevé sa limit par `binarylimit`, huit sinon, à comparer aux dizaines de
/// mébioctets que le path texte a dû se voir interdire.
///
/// **Ce que cette fonction ne fait pas : allouer l'image.** `binaire.image` est
/// un `Arc` partagé avec l'état ; seule la chunk est copiée. C'est ce qui rend
/// le pire cas d'une connection binaire indépendant de la size de la cover.
async fn write_bytes(writer: &mut OwnedWriteHalf, binaire: &Binary) -> Result<()> {
    // Indexation sans contrôle : c'est `commands::cover` qui établit
    // l'intervalle, et son contrat est qu'il tient dans l'image et dans la
    // limit de la connection, elle-même plafonnée à `MAX_CHUNK_CAP`.
    // L'assertion de débogage le dit plutôt que de le supposer en silence, sans
    // rien coûter en production.
    let chunk = &binaire.image[binaire.chunk.clone()];
    debug_assert!(
        chunk.len() <= MAX_CHUNK_CAP,
        "une chunk depasse le cap du greffon"
    );
    let binary = line("binary", chunk.len());
    let header: usize =
        binaire.header.iter().chain(std::iter::once(&binary)).map(|l| l.len() + 1).sum();
    // Capacité exacte : en-tête, chunk, puis `\nOK\n`.
    let mut buffer = Vec::with_capacity(header + chunk.len() + 4);
    for l in binaire.header.iter().chain(std::iter::once(&binary)) {
        buffer.extend_from_slice(l.as_bytes());
        buffer.push(b'\n');
    }
    buffer.extend_from_slice(chunk);
    buffer.extend_from_slice(b"\nOK\n");
    writer.write_all(&buffer).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Snapshot;
    use ritornello_proto::{Command, Track, Playback, PlayerState};
    // `AsyncReadExt` pour le `read_exact` des tranches binaires : le client de
    // test est le seul du greffon à read des bytes bruts.
    use tokio::io::AsyncReadExt;
    // Le player borné de la session n'en a plus besoin ; le client de test,
    // lui, read des lines sans cap à défendre.
    use tokio::io::Lines;

    /// Un client de test : les lines reçues d'un côté, la plume de l'autre.
    struct Client {
        lines: Lines<BufReader<OwnedReadHalf>>,
        writer: OwnedWriteHalf,
    }

    impl Client {
        /// Un client sur un stream déjà ouvert. Séparé de `Serveur::client` :
        /// les tests de reliaison connectent une adresse qui n'est pas celle
        /// que le `Serveur` de test porte.
        fn depuis(stream: TcpStream) -> Client {
            let (playback, writer) = stream.into_split();
            Client { lines: BufReader::new(playback).lines(), writer }
        }

        async fn connecter(adresse: std::net::SocketAddr) -> Client {
            Client::depuis(TcpStream::connect(adresse).await.unwrap())
        }

        async fn send_frame(&mut self, line: &str) {
            self.writer.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        }

        async fn recevoir(&mut self) -> String {
            self.lines.next_line().await.unwrap().expect("le serveur a ferme la connection")
        }

        /// Lit exactement `n` bytes **bruts**.
        ///
        /// Derrière le player de lines (`get_mut`) et non sur la chaussette :
        /// les bytes qui suivent un en-tête sont déjà dans le buffer du
        /// `BufReader` au moment où la dernière line d'en-tête a été rendue,
        /// et read la chaussette directement les laisserait là — un test qui
        /// se bloquerait sans que le serveur y soit pour rien.
        async fn bytes(&mut self, n: usize) -> Vec<u8> {
            let mut buffer = vec![0u8; n];
            self.lines.get_mut().read_exact(&mut buffer).await.unwrap();
            buffer
        }

        /// Rejoue la séquence d'un vrai client : une requête par chunk,
        /// l'offset croissant, jusqu'à détenir `size` bytes.
        ///
        /// C'est bien la boucle de M.A.L.P. et de libmpdclient : la première
        /// réponse apprend la size totale, chaque suivante est demandée à
        /// l'offset de ce qu'on a déjà. La sortie de boucle ne dépend d'aucune
        /// horloge ni d'aucun compte d'itérations — seulement de `size`.
        async fn recuperer(&mut self, commande: &str, uri: &str) -> Recuperee {
            let mut recuperee = Recuperee { image: Vec::new(), tailles: Vec::new(), mime: None };
            loop {
                self.send_frame(&format!("{commande} {uri} {}", recuperee.image.len())).await;
                let size = self.entier("size").await;
                let mut header = self.recevoir().await;
                // `type:` n'est là que pour `readpicture` : c'est une line de
                // plus, avant `binary:`, exactement comme MPD la place.
                if let Some(mime) = header.strip_prefix("type: ") {
                    recuperee.mime = Some(mime.to_string());
                    header = self.recevoir().await;
                }
                let n: usize = header
                    .strip_prefix("binary: ")
                    .unwrap_or_else(|| panic!("attendu binary:, obtenu {header}"))
                    .parse()
                    .unwrap();
                // Une chunk clear ne fait pas avancer la boucle : la refuser
                // ici transforme un serveur qui piétine en échec franc, plutôt
                // qu'en test qui tourne à clear.
                assert!(n > 0, "une chunk clear ne terminate jamais la recuperation");
                recuperee.image.extend_from_slice(&self.bytes(n).await);
                recuperee.tailles.push(n);
                // Le `\n` que MPD écrit après les bytes bruts : lu comme une
                // line clear. Son absence ferait read `<dernier octet>OK`.
                assert_eq!(self.recevoir().await, "", "un saut de line suit les bytes bruts");
                assert_eq!(self.recevoir().await, "OK", "chaque chunk est une reponse complete");
                if recuperee.image.len() >= size {
                    return recuperee;
                }
            }
        }

        /// La valeur entière d'une line `clé: nombre` attendue.
        async fn entier(&mut self, key: &str) -> usize {
            let l = self.recevoir().await;
            l.strip_prefix(&format!("{key}: "))
                .unwrap_or_else(|| panic!("attendu {key}:, obtenu {l}"))
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

    /// Ce qu'une récupération complète de cover a produit : l'image
    /// réassemblée, la size de chaque chunk reçue dans l'order, et le MIME
    /// si le serveur l'a annoncé.
    struct Recuperee {
        image: Vec<u8>,
        tailles: Vec<usize>,
        mime: Option<String>,
    }

    struct Serveur {
        adresse: std::net::SocketAddr,
        state: Arc<SharedState>,
        /// Tenu vivant exprès : lâcher l'émetteur ferait sortir `listen` de
        /// son `select!` (« la moitié admin a disparu ») et les tests
        /// n'éprouveraient plus le path de service ordinaire, seulement celui
        /// de l'extinction.
        _config_tx: tokio::sync::watch::Sender<crate::config::Config>,
    }

    /// Lie l'écouteur **dans le test** et le donne au serveur, comme
    /// `register.rs` le fait pour ses sockets Unix : l'écouteur existe donc
    /// avant que le client ne se connecte, et aucune boucle de reprise ni
    /// aucun délai n'est nécessaire pour que le `connect` aboutisse.
    async fn serveur() -> (Serveur, mpsc::Receiver<InputMessage>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adresse = listener.local_addr().unwrap();
        let state = Arc::new(SharedState::default());
        let (tx, rx) = mpsc::channel(64);
        let (config_tx, config_rx) =
            tokio::sync::watch::channel(crate::config::Config::default());
        tokio::spawn(listen(listener, config_rx, state.clone(), tx));
        (Serveur { adresse, state, _config_tx: config_tx }, rx)
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
    /// un changement réel, sinon la déduplication d'`apply_state` l'avale et
    /// la boucle tourne à clear.
    ///
    /// La boucle, elle, n'arbitre plus aucune course. Elle en arbitrait une :
    /// une trame appliquée avant que la session n'ait lu sa line `idle` était
    /// comprise dans les compteurs qu'elle mémorisait, donc invisible pour
    /// elle. **C'était un défaut de la session et non un contrat d'`wait`**
    /// — la référence d'un `idle` est désormais celle que la connection porte
    /// depuis sa bannière (voir `serve`), donc une seule trame suffirait ici.
    /// La boucle est gardée parce qu'elle ne dépend d'aucune horloge et qu'elle
    /// remainder juste dans les deux cas ; ce qu'il ne faut plus faire, c'est
    /// justifier par elle une course qui a été corrigée.
    async fn reponse_sous_trames(
        client: &mut Client,
        state: &SharedState,
        trames: [PlayerState; 2],
    ) -> Vec<String> {
        let mut i = 0usize;
        let premiere = loop {
            tokio::select! {
                // `biased` : dès qu'une line est là, on la prend plutôt que
                // de push une trame de plus.
                biased;
                lue = client.lines.next_line() => {
                    break lue.unwrap().expect("le serveur a ferme la connection");
                }
                () = state.apply_state(trames[i % 2].clone()) => {
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
        // Comparée à la chaîne littérale et non à `ANNOUNCED_VERSION` : contre
        // la constante, ce test ne vérifierait que la mise en forme, alors que
        // c'est le **numéro** qui décide des capacités qu'un client s'autorise.
        // Le changer doit être un geste conscient, pas un effet de bord.
        assert_eq!(banniere, "OK MPD 0.23.5");
    }

    #[tokio::test]
    async fn une_commande_rend_ses_lignes_puis_ok() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("status").await;
        let recues = c.reponse().await;
        assert_eq!(*recues.last().unwrap(), "OK");
        assert!(recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");
        assert!(recues.iter().any(|l| l.starts_with("state: ")), "{recues:?}");
    }

    #[tokio::test]
    async fn une_liste_de_commandes_ne_rend_quun_seul_ok() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("status").await;
        c.send_frame("command_list_end").await;
        let recues = c.reponse().await;
        let ok = recues.iter().filter(|l| *l == "OK").count();
        assert_eq!(ok, 1, "un seul OK clot la liste: {recues:?}");
        // Et les deux commands ont bien été exécutées : sans ça, « un seul
        // OK » serait aussi vrai d'une liste qui n'exécute rien.
        assert_eq!(recues.iter().filter(|l| l.starts_with("volume: ")).count(), 2, "{recues:?}");
    }

    #[tokio::test]
    async fn command_list_ok_begin_insere_un_list_ok_par_commande() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("command_list_ok_begin").await;
        c.send_frame("status").await;
        c.send_frame("ping").await;
        c.send_frame("command_list_end").await;
        let recues = c.reponse().await;
        assert_eq!(recues.iter().filter(|l| *l == "list_OK").count(), 2, "{recues:?}");
        assert_eq!(*recues.last().unwrap(), "OK");
        // Le `list_OK` d'une commande sans lines (`ping`) est le dernier
        // avant le `OK` : c'est ce qui permet à un client de recoller chaque
        // réponse à sa commande, y compris les vides.
        assert_eq!(recues[recues.len() - 2], "list_OK", "{recues:?}");
    }

    #[tokio::test]
    async fn une_erreur_dans_une_liste_interrompt_la_suite() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("nawak").await;
        c.send_frame("status").await;
        c.send_frame("command_list_end").await;
        let recues = c.reponse().await;
        assert_eq!(*recues.last().unwrap(), "ACK [5@1] {nawak} unsupported", "{recues:?}");
        assert!(!recues.iter().any(|l| l == "OK"), "un ACK remplace le OK: {recues:?}");
        // **Attention à ce qui prouve quoi ici**, la relecture s'est fait
        // prendre et le commentaire précédent disait faux. `reponse()`
        // s'arrête au **premier** terminateur, et un `ACK` en est un : compter
        // les `volume:` de cette seule réponse ne prouve donc rien. Une session
        // qui continuerait la liste après l'erreur écrirait tout d'un bloc, et
        // `reponse()` rendrait exactement les mêmes lines — jusqu'à l'`ACK`,
        // en laissant le `status` suivant **derrière** dans le stream.
        //
        // Ce qui tue ce mutant, c'est la suite : la commande d'après doit
        // recevoir sa propre réponse et rien d'autre. Un `status` fuité
        // ressort ici, et le compte se fait sur les **deux** réponses. Ne pas
        // « raccourcir » ce test en gardant le compte et en jetant le `ping` :
        // c'est le `ping` qui travaille.
        c.send_frame("ping").await;
        let apres = c.reponse().await;
        assert_eq!(apres, vec!["OK".to_string()], "reponse fuitee: {apres:?}");
        let volumes = recues.iter().chain(apres.iter()).filter(|l| l.starts_with("volume: ")).count();
        assert_eq!(volumes, 1, "le troisieme status ne doit pas avoir tourne: {recues:?} {apres:?}");
    }

    #[tokio::test]
    async fn idle_ne_repond_quau_changement() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("idle").await;
        let recues = reponse_sous_trames(&mut c, &s.state, [trame_mixer(17), trame_mixer(18)]).await;
        // Le réveil nomme le sous-système et lui seul, puis clôt par `OK`.
        assert_eq!(recues, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn idle_filtre_les_sujets_demandes() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("idle player").await;
        // Chaque trame bouge `player` **et** `mixer` : le réveil est donc
        // certain (aucune course à arbitrer), et le filtre se mesure à ce que
        // la réponse *ne* nomme *pas*. Une session qui aurait ignoré la liste
        // demandée écrirait ici deux `changed:`.
        let recues = reponse_sous_trames(
            &mut c,
            &s.state,
            [trame_player_et_mixer(17), trame_player_et_mixer(18)],
        )
        .await;
        assert_eq!(recues, vec!["changed: player".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn noidle_rend_la_main_immediatement() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("idle").await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
        // Un `OK` sec, et surtout **rien avant lui** : c'est la preuve sans
        // horloge qu'`idle` ne répond pas de lui-même. S'il avait répondu sans
        // qu'aucune trame ne bouge, la première line lue serait un
        // `changed:`.
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        // Et ce `status` est là pour compter les réponses sans horloge : la
        // deuxième doit être **la sienne**. Une session qui aurait rendition un
        // `OK` de complaisance à l'`idle` (au lieu d'wait) aurait glissé
        // une réponse de plus dans le stream, et on lirait ici le `OK` du
        // `noidle` au lieu des lines du `status`. Sans cette moitié, le test
        // passait aussi bien avec un `idle` qui répond tout de suite —
        // vérifié, et c'est ce qui l'a fait réécrire.
        let apres = c.reponse().await;
        assert!(apres.iter().any(|l| l.starts_with("volume: ")), "{apres:?}");
        // Et rien n'a bougé dans l'état : `noidle` annule une attente, il ne
        // publie pas de changement.
        assert_eq!(s.state.read().await.versions, [0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn une_commande_pendant_une_attente_annule_lattente_puis_est_executee() {
        // **Un test de comptabilité, pas de contenu.** Le client écrit deux
        // lines (`idle`, `status`) et doit recevoir **deux** terminateurs. Ce
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
        c.send_frame("idle").await;
        c.send_frame("status").await;

        // Premier terminateur : celui de l'`idle` annulé. `OK` nu, aucun
        // `changed:` — rien n'a bougé, et de toute façon `noidle` n'announcement
        // rien.
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        // Second terminateur : la réponse du `status`, avec ses lines.
        let deuxieme = c.reponse().await;
        assert!(deuxieme.iter().any(|l| l.starts_with("volume: ")), "{deuxieme:?}");
        assert_eq!(*deuxieme.last().unwrap(), "OK");
        // Et la troisième requête reçoit **sa** réponse : c'est ce qui prouve
        // l'absence de décalage. Un `ping` répond `OK` sec, donc une réponse de
        // `status` qui traînerait dans le stream ressortirait ici.
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);

        // ------------------------------------------------------------------
        // **Et le cas voisin qui doit rester à UN seul terminateur.** Gardé
        // dans le même test exprès : séparés, un remaniement futur croirait
        // l'un redondant. `noidle` n'est pas une requête mais l'annulation de
        // celle en cours, donc `idle` + `noidle` = un `OK`, comme chez MPD.
        // Si la correction ci-dessus le faisait passer à deux, elle aurait
        // cassé le cas correct.
        // ------------------------------------------------------------------
        c.send_frame("idle").await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()], "un seul OK pour idle + noidle");
        let apres = c.reponse().await;
        assert!(
            apres.iter().any(|l| l.starts_with("volume: ")),
            "un terminateur de trop apres noidle: {apres:?}"
        );
    }

    #[tokio::test]
    async fn une_ligne_illisible_pendant_une_attente_compte_aussi_deux_terminateurs() {
        // La même comptabilité sur l'autre entrée de cette branche : une line
        // mal citée n'est pas `noidle` (elle ne se découpe pas), donc elle est
        // un `noidle` implicite suivi d'une line qui recevra son `ACK` par le
        // path ordinaire. Deux lines écrites, deux terminateurs.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("idle").await;
        c.send_frame(r#"load "France"#).await;

        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        assert_eq!(c.reponse().await, vec!["ACK [2@0] {load} invalid argument".to_string()]);
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_liste_de_commandes_ouverte_pendant_une_attente_est_traitee_comme_une_liste() {
        // La line pushback repasse par l'aiguillage **complet** de `serve`, et
        // non par une réinterprétation locale : un `command_list_begin` reçu
        // pendant une attente ouvre donc une vraie liste, dont le `OK` unique
        // arrive après celui de l'`idle` annulé. C'est ce qui garantit qu'aucun
        // cas n'a besoin d'être dupliqué dans `wait_idle`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("idle").await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("status").await;
        c.send_frame("command_list_end").await;

        assert_eq!(c.reponse().await, vec!["OK".to_string()], "l'idle annule a son terminateur");
        let liste = c.reponse().await;
        assert_eq!(liste.iter().filter(|l| *l == "OK").count(), 1, "{liste:?}");
        assert_eq!(liste.iter().filter(|l| l.starts_with("volume: ")).count(), 2, "{liste:?}");
    }

    #[tokio::test]
    async fn un_changement_survenu_entre_deux_commandes_est_rapporte_par_lidle_suivant() {
        // **LE test de ce correctif.** La session mémorisait les compteurs dans
        // l'`Snapshot` de la commande `idle` elle-même, donc tout ce qui avait
        // bougé entre la réponse précédente du client et sa line `idle` était
        // avalé — c'est-à-dire pendant la seule fenêtre où un client MPD
        // n'écoute pas. Pour `stored_playlist`, rien ne rejoue l'événement avant
        // le prochain changement de sources_catalog : `listplaylists` remainder périmé,
        // potentiellement pour toujours. C'est exactement le premier essai
        // prévu sur l'appareil (« désactiver une source, sa liste doit
        // rétrécir »), qui pouvait donc échouer en silence.
        //
        // Sans horloge, et **une seule trame poussée** : c'est ce qui rend la
        // preuve concluante. Aucun changement ne suivra, donc une session qui
        // relit ses compteurs à la line `idle` dort pour toujours et ce test
        // **pend** — le mode d'échec voulu. Vérifié contre l'ancien code.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        // Une commande, sa réponse lue jusqu'au terminateur : le client est
        // désormais « entre deux commands », précisément comme un client qui
        // vient de rafraîchir son écran.
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);

        // Le changement arrive maintenant : personne n'attend.
        s.state.apply_catalog(ritornello_proto::SourcesCatalog {
            sources: vec![ritornello_proto::SourceCatalog {
                name: "radio".into(),
                presets: vec![ritornello_proto::Preset { index: 1, name: "FIP".into() }],
            }],
        })
        .await;

        c.send_frame("idle stored_playlist").await;
        assert_eq!(
            c.reponse().await,
            vec!["changed: stored_playlist".to_string(), "OK".to_string()]
        );
    }

    #[tokio::test]
    async fn un_reveil_ne_consomme_que_les_sujets_quil_annonce() {
        // La moitié fine du même dispositif. Le réveil avance la référence de la
        // connection **sujet par sujet**, comme MPD n'efface que les drapeaux
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
        s.state.apply_state(trame_player_et_mixer(17)).await;

        c.send_frame("idle player").await;
        assert_eq!(c.reponse().await, vec!["changed: player".to_string(), "OK".to_string()]);

        c.send_frame("idle mixer").await;
        assert_eq!(c.reponse().await, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn un_reveil_annonce_consomme_bien_son_compteur() {
        // Le pendant indispensable : la référence doit *avancer*. Sans cela un
        // `idle` rapporterait éternellement le même changement, et un client
        // qui boucle sur `idle` — c'est-à-dire tous — tournerait à pleine
        // vitesse sur la commande faite pour l'en dispenser.
        //
        // Prouvé sans horloge : le second `idle` doit **wait**, donc la
        // commande d'après est un `noidle` dont le `OK` unique est suivi de la
        // réponse du `status`. Si le second `idle` avait répondu tout seul, il
        // y aurait un terminateur de plus et on lirait ici le `OK` du `noidle`
        // au lieu des lines du `status` — le même compte que
        // `noidle_rend_la_main_immediatement`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        s.state.apply_state(trame_mixer(17)).await;

        c.send_frame("idle mixer").await;
        assert_eq!(c.reponse().await, vec!["changed: mixer".to_string(), "OK".to_string()]);

        c.send_frame("idle mixer").await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
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
        a.send_frame("idle").await;

        let mut b = s.client_pret().await;
        b.send_frame("status").await;
        let recues = b.reponse().await;
        assert_eq!(*recues.last().unwrap(), "OK", "{recues:?}");
        assert!(recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");

        // Et A dormait vraiment : sans cette moitié, le test passerait aussi
        // avec un A dont la session est morte — le réveil prouve qu'elle était
        // vivante et en attente pendant que B se faisait serve.
        let wakeup = reponse_sous_trames(&mut a, &s.state, [trame_mixer(17), trame_mixer(18)]).await;
        assert_eq!(wakeup, vec!["changed: mixer".to_string(), "OK".to_string()]);
    }

    #[tokio::test]
    async fn une_commande_daction_arrive_sur_le_canal_dentree() {
        let (s, mut rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("next").await;
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
        c.send_frame("status").await;
        c.reponse().await;
        // La réponse est arrivée, donc la commande est entièrement traitée :
        // si `status` avait émis quoi que ce soit, ce serait déjà dans le
        // canal. Aucune horloge n'est nécessaire pour l'affirmer.
        assert!(rx.try_recv().is_err(), "status ne demande rien a l'appareil");
    }

    #[tokio::test]
    async fn un_canal_ferme_ferme_la_session_sans_acter_la_bascule() {
        // L'order « push puis acter » se mesure ici et nulle part ailleurs :
        // le canal refuse (récepteur lâché), donc rien n'a été émis, donc rien
        // ne doit avoir été acté. Une session qui appellerait
        // `acknowledge_optimistic` d'abord poserait le volume 30 dans l'état partagé
        // et le ferait publier par `status` à tous les autres clients — une
        // bascule que le cœur n'a jamais reçue.
        let (s, rx) = serveur().await;
        drop(rx);
        let mut c = s.client_pret().await;
        c.send_frame("setvol 30").await;
        assert!(
            c.lines.next_line().await.unwrap().is_none(),
            "une moitie input morte ferme la session"
        );
        assert_eq!(s.state.read().await.state.volume, 0, "rien ne s'acte si le canal a refuse");
        assert_eq!(s.state.read().await.versions, [0, 0, 0, 0], "et personne n'est reveille");
    }

    #[tokio::test]
    async fn idle_dans_une_liste_est_refuse_a_son_rang() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame("idle").await;
        let recues = c.reponse().await;
        // L'index est le rang d'`idle` dans la liste (1), pas 0 : un client
        // qui groupe dix commands doit savoir laquelle a été refusée.
        assert_eq!(recues, vec!["ACK [5@1] {idle} not allowed in command list".to_string()]);
        // Refusé **à l'accumulation** : le `status` qui précède n'a pas été
        // exécuté, donc aucune line `volume:` n'accompagne l'ACK.
        assert!(!recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");
        // Et l'état de liste a été rendition : la commande suivante répond seule.
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_liste_sans_fin_est_bornee() {
        // Une liste s'accumule en mémoire sans rien exécuter : sans cap, un
        // client qui n'envoie jamais son `command_list_end` fait croître un
        // `Vec` jusqu'à l'épuisement de la mémoire d'un Pi. Le refus arrive au
        // rang du cap et rend l'état de liste.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        let mut lot = String::from("command_list_begin\n");
        for _ in 0..=MAX_LIST_COMMANDS {
            lot.push_str("ping\n");
        }
        c.writer.write_all(lot.as_bytes()).await.unwrap();
        let recues = c.reponse().await;
        assert_eq!(
            recues,
            vec![format!("ACK [5@{MAX_LIST_COMMANDS}] {{ping}} list too large")],
            "{recues:?}"
        );
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_ligne_plus_longue_que_le_plafond_ferme_la_connexion() {
        // La dernière surface non bornée du greffon, et elle est atteignable
        // sans mot de passe depuis tout le réseau local : un client qui envoie
        // des bytes sans jamais send_frame de `\n`. Sans cap, la session
        // accumule jusqu'à ce que l'allocateur renonce — sur un Pi d'un
        // gigaoctet partagé avec mpv, cela emporte la musique et pas seulement
        // le greffon.
        //
        // Sans horloge : la bounded se mesure au fait que la connection **finit**.
        // Sans cap, ce `next_line` attendrait le `\n` pour toujours et le
        // test pendrait — vérifié, et c'est le mode d'échec voulu.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        let bourrage = vec![b'a'; MAX_LINE + 1];
        // L'écriture peut échouer si le serveur a déjà fermé : c'est une fin
        // acceptable et non un échec du test, d'où le résultat ignoré.
        let _ = c.writer.write_all(&bourrage).await;
        assert!(
            c.lines.next_line().await.unwrap().is_none(),
            "une line au-dela du cap ferme la connection, sans ACK"
        );
    }

    #[tokio::test]
    async fn une_ligne_longue_mais_sous_le_plafond_est_traitee() {
        // Le pendant du test précédent : un cap qui coupe une line
        // légitime serait pire que pas de cap. La plus longue line
        // plausible du protocol est un name entre guillemets, et elle doit
        // arriver entière — ici elle mesure exactement le cap.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        let name = "a".repeat(MAX_LINE - "load \"\"".len());
        c.send_frame(&format!("load \"{name}\"")).await;
        // `load` refuse tout name faute de sources_catalog (Task 13), et c'est
        // justement une réponse qui prouve que la line a été **découpée** :
        // un `ACK 2` ou une fermeture diraient qu'elle a été tronquée.
        assert_eq!(c.reponse().await, vec!["ACK [50@0] {load} no such playlist".to_string()]);
    }

    #[tokio::test]
    async fn une_ligne_terminee_par_crlf_est_lue_sans_le_retour_chariot() {
        // Les clients écrits sur Windows terminent par `\r\n`. Le player
        // écrit à la main devait reprendre ce que `Lines` faisait pour nous, et
        // rien ne le disait : sans le `\r` retiré, la commande serait `ping\r`,
        // donc un `ACK 5` — une régression qu'aucun test existant ne voyait.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.writer.write_all(b"ping\r\n").await.unwrap();
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_derniere_ligne_sans_fin_de_ligne_est_traitee_avant_la_fermeture() {
        // Un client qui envoie sa commande puis ferme sa moitié écriture doit
        // la voir traitée : la fin de stream terminate la line. C'est ce que
        // faisait `Lines`, et le path « buffer non clear à l'EOF » du nouveau
        // player n'a pas d'autre témoin que ce test.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.writer.write_all(b"ping").await.unwrap();
        // `shutdown` et non un `drop` : la moitié playback du client doit rester
        // ouverte pour read la réponse.
        c.writer.shutdown().await.unwrap();
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_ligne_au_dela_du_plafond_avec_une_fin_de_ligne_ferme_aussi() {
        // Le cap est contrôlé dans les **deux** bras du player, et le test
        // précédent n'en visite qu'un (le track lu ne contains pas de `\n`).
        // Celui-ci visite l'autre : la line dépasse le cap *et* se terminate
        // bien. Sans ce cas, retirer le contrôle du bras `Some` laissait passer
        // toute la suite — un cap que personne n'exerce est un cap
        // qu'on retire par distraction.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        // Exactement `MAX_LINE` bytes sans `\n` : légal, le buffer les garde.
        c.writer.write_all(&vec![b'a'; MAX_LINE]).await.unwrap();
        // Puis un octet de trop, cette fois suivi de sa fin de line : c'est le
        // bras `Some` qui doit refuser, en comptant ce qui était déjà accumulé.
        let _ = c.writer.write_all(b"b\n").await;
        assert!(
            c.lines.next_line().await.unwrap().is_none(),
            "une line au-dela du cap ferme la connection, meme terminee"
        );
    }

    #[tokio::test]
    async fn une_ligne_vide_est_refusee_sans_fermer() {
        // Un `\n` nu. `handle` sait déjà le refuser (elle est totale par
        // construction), mais aucun test de session ne le montrait bout en
        // bout : la session pourrait l'avaler en silence, et un client qui
        // attend une réponse par line resterait pendu.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.writer.write_all(b"\n").await.unwrap();
        assert_eq!(c.reponse().await, vec!["ACK [5@0] {} unsupported".to_string()]);
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_reponse_trop_grosse_est_refusee_sans_fermer() {
        // L'amplificateur : `MAX_LIST_COMMANDS` bounded les commands, pas ce
        // qu'elles **produisent**. Une liste de `playlistinfo` sur une file de
        // 255 entrées rend une quinzaine de kibioctets par commande, et la
        // réponse entière était mise à plat dans une seule `String` avant le
        // `write_all` — donc une allocation contiguë de plusieurs dizaines de
        // mébioctets, demandée à un Pi dont la mémoire est fragmentée. 26 Kio
        // d'entrée suffisaient.
        //
        // Le refus arrive **avant** toute écriture, donc il remplace la réponse
        // au lieu de s'y add : un seul terminateur, et la connection vit.
        let (s, _rx) = serveur().await;
        s.state
            .apply_state(PlayerState {
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
        c.writer.write_all(lot.as_bytes()).await.unwrap();
        let recues = c.reponse().await;
        assert_eq!(recues.len(), 1, "le refus remplace la reponse composee: {recues:?}");
        // L'index exact dépend de l'arithmétique des bytes (une quinzaine de
        // kibioctets par commande, un mébioctet de cap) : ce qui compte est
        // qu'il nomme la commande qui a débordé et son rang dans le lot.
        let refus = &recues[0];
        assert!(refus.starts_with("ACK [5@"), "{refus}");
        assert!(refus.ends_with("] {playlistinfo} response too large"), "{refus}");
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn une_liste_lourde_en_octets_est_refusee_bien_avant_le_compte() {
        // L'autre moitié du même trou : une line accumulée peut légitimement
        // peser `MAX_LINE`, donc 2048 commands bornées **en nombre** pesaient
        // 16 Mio par connection. Ici trente-deux lines de 8 Kio tombent
        // *exactement* sur les 256 Kio — le cap refuse au-delà, pas à
        // égalité — donc c'est la trente-troisième qui franchit, et la boucle en
        // envoie une de plus pour cette raison. Trente-trois, c'est très loin
        // des 2048 commands : la bounded qui refuse ici est bien celle en bytes
        // et non celle en nombre.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("command_list_begin").await;
        let mut lot = String::new();
        for _ in 0..MAX_LIST_BYTES.div_ceil(MAX_LINE) + 1 {
            lot.push_str("ping ");
            lot.push_str(&"a".repeat(MAX_LINE - 6));
            lot.push('\n');
        }
        c.writer.write_all(lot.as_bytes()).await.unwrap();
        let recues = c.reponse().await;
        assert_eq!(recues.len(), 1, "{recues:?}");
        assert!(recues[0].starts_with("ACK [5@"), "{recues:?}");
        assert!(recues[0].ends_with("] {ping} list too large"), "{recues:?}");
        // L'état de liste est rendition : la commande suivante répond seule.
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn au_dela_du_plafond_de_sessions_une_connexion_est_refusee_aussitot() {
        // Le multiplicateur : chaque autre cap bounded une connection, et le
        // nombre de connexions ne l'était pas. Un client qui fuit ses
        // connexions — qui en rouvre une à chaque reprise de réseau sans fermer
        // la précédente — arrive ici par accident, sans le moindre script
        // hostile.
        //
        // Sans horloge, et l'order est garanti par la bannière : elle est
        // écrite par `serve`, donc *après* la prise de la place. Avoir lu
        // `MAX_SESSIONS` bannières prouve que les `MAX_SESSIONS` slots sont
        // prises, et la connection suivante est donc bien celle qui déborde.
        let (s, _rx) = serveur().await;
        let mut ouverts = Vec::new();
        for _ in 0..MAX_SESSIONS {
            ouverts.push(s.client_pret().await);
        }
        // Celle de trop : acceptée par le noyau (le port écoute toujours), puis
        // fermée aussitôt par `accepter`. Aucune bannière, donc fin de stream.
        let mut refuse = s.client().await;
        assert!(
            refuse.lines.next_line().await.unwrap().is_none(),
            "au-dela du cap, la connection doit etre fermee sans banniere"
        );
        // Et les sessions déjà ouvertes servent encore : le cap refuse les
        // nouvelles, il ne dégrade pas les anciennes. La première et la
        // dernière, parce qu'un cap mal câblé casse volontiers l'une des
        // deux extrémités.
        for index in [0, MAX_SESSIONS - 1] {
            ouverts[index].send_frame("ping").await;
            assert_eq!(ouverts[index].reponse().await, vec!["OK".to_string()]);
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ancienne = listener.local_addr().unwrap();
        // Un port libre, choisi par le noyau puis rendition : c'est la seule façon
        // d'en nommer un qui ne soit pas déjà pris sur la machine du test.
        let sonde = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let neuve = sonde.local_addr().unwrap();
        drop(sonde);

        let state = Arc::new(SharedState::default());
        let (cmd_tx, _cmd_rx) = mpsc::channel(64);
        let (config_tx, config_rx) = tokio::sync::watch::channel(crate::config::Config {
            listen: "127.0.0.1".into(),
            port: ancienne.port(),
        });
        tokio::spawn(listen(listener, config_rx, state, cmd_tx));

        // L'ancien port sert bien avant tout changement.
        let mut avant = Client::connecter(ancienne).await;
        assert!(avant.recevoir().await.starts_with("OK MPD "));

        config_tx
            .send(crate::config::Config { listen: "127.0.0.1".into(), port: neuve.port() })
            .unwrap();

        let banniere = loop {
            if let Ok(stream) = TcpStream::connect(neuve).await {
                let mut c = Client::depuis(stream);
                break c.recevoir().await;
            }
            tokio::task::yield_now().await;
        };
        assert!(banniere.starts_with("OK MPD "), "banniere inattendue: {banniere}");

        // Et la session déjà ouverte n'a pas été coupée : elle tient son propre
        // stream, que la fermeture de l'écouteur ne touche pas. C'est la
        // différence avec un vrai redémarrage de MPD, et elle est voulue.
        avant.send_frame("ping").await;
        assert_eq!(avant.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn un_port_impossible_laisse_le_serveur_ou_il_etait() {
        // Un réglage fautif — port déjà pris, adresse absente de la machine —
        // ne doit pas rendre le serveur MPD injoignable. L'ancien écouteur
        // n'est lâché qu'une fois le nouveau lié.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ancienne = listener.local_addr().unwrap();
        let state = Arc::new(SharedState::default());
        let (cmd_tx, _cmd_rx) = mpsc::channel(64);
        let (config_tx, config_rx) = tokio::sync::watch::channel(crate::config::Config {
            listen: "127.0.0.1".into(),
            port: ancienne.port(),
        });
        tokio::spawn(listen(listener, config_rx, state, cmd_tx));

        // Une adresse qu'aucune interface ne porte : le `bind` échoue.
        config_tx
            .send(crate::config::Config { listen: "192.0.2.1".into(), port: 6600 })
            .unwrap();

        // Le serveur répond toujours là où il répondait. Boucle sans horloge,
        // même raison que le test ci-dessus : c'est le succès qui l'arrête.
        let banniere = loop {
            if let Ok(stream) = TcpStream::connect(ancienne).await {
                let mut c = Client::depuis(stream);
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
            if let Some(line) = candidat.lines.next_line().await.unwrap() {
                break line;
            }
        };
        assert!(banniere.starts_with("OK MPD "), "banniere inattendue: {banniere}");
    }

    #[tokio::test]
    async fn une_ligne_illisible_ne_ferme_pas_la_connexion() {
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame(r#"load "France"#).await;
        let recues = c.reponse().await;
        assert_eq!(recues, vec!["ACK [2@0] {load} invalid argument".to_string()]);
        // Le client suivant n'a pas à se reconnecter.
        c.send_frame("ping").await;
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
        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame(r#"load "France"#).await;
        assert_eq!(c.reponse().await, vec!["ACK [2@1] {load} invalid argument".to_string()]);
        c.send_frame("command_list_end").await;
        assert_eq!(
            c.reponse().await,
            vec!["ACK [5@0] {command_list_end} unsupported".to_string()]
        );
    }

    #[tokio::test]
    async fn close_repond_ok_puis_ferme() {
        // Décision assumée : MPD n'écrit rien avant de fermer, nous répondons.
        // Voir le commentaire d'`Outcome::Close` dans `execute`.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("close").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
        assert!(c.lines.next_line().await.unwrap().is_none(), "close doit fermer");
    }

    #[tokio::test]
    async fn les_noms_de_sujets_sont_ceux_quidle_accepte() {
        // `subsystem_name` est l'inverse de la table de `commands.rs`, et rien ne
        // relie les deux au compilateur : un `stored-playlist` au tiret ici
        // ferait annoncer un sous-système qu'aucun client ne saurait
        // redemander. Le vérifier en passant chaque name à `idle`.
        for sujet in [Subsystem::Player, Subsystem::Mixer, Subsystem::Playlist, Subsystem::StoredPlaylist] {
            let args = vec!["idle".to_string(), subsystem_name(sujet).to_string()];
            assert_eq!(
                handle(&Snapshot::default(), 0, &args, MAX_CHUNK),
                Outcome::Wait(vec![sujet]),
                "subsystem_name({sujet:?}) n'est pas un name qu'idle accepte"
            );
        }
    }

    #[tokio::test]
    async fn un_idle_sans_sujet_connu_nest_pas_un_ok_immediat() {
        // `idle database` ne nomme que des sous-systèmes que ce greffon
        // n'émet jamais : la liste de subsystems est clear, et le contrat
        // d'`Outcome::Wait` dit que c'est une attente **sans fin**, pas un
        // `OK` immédiat. Un `OK` ferait boucler le client à pleine vitesse
        // sur la seule commande faite pour l'en dispenser.
        //
        // Prouvé sans horloge, **en comptant les terminateurs** : `idle` +
        // `noidle` n'en valent qu'un, donc la deuxième réponse lue est celle du
        // `status`. Une session qui aurait rendition `OK` tout de suite en aurait
        // écrit un de plus (le sien, puis celui du `noidle` reçu hors attente),
        // et on lirait ici un `OK` sec au lieu des lines du `status`.
        //
        // Le discriminant a changé avec le `noidle` implicite : send_frame
        // `status` ne distingue plus rien, puisqu'une attente annulée écrit
        // désormais `OK` puis la réponse du `status` — exactement ce qu'un
        // `idle` répondant tout de suite produirait aussi.
        let (s, _rx) = serveur().await;
        let mut c = s.client_pret().await;
        c.send_frame("idle database").await;
        // Des trames qui bougent tous les compteurs : aucune ne concerne les
        // subsystems demandés (il n'y en a aucun), donc aucune ne doit réveiller.
        s.state.apply_state(trame_player_et_mixer(17)).await;
        s.state.apply_state(trame_player_et_mixer(18)).await;
        c.send_frame("noidle").await;
        c.send_frame("status").await;
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

    /// Le `href` que la trame d'état publie, et que la trame de cover
    /// reprend.
    const HREF: &str = "/api/cover/1a2b3c";

    /// L'URI que notre `currentsong` publie pour l'état ci-dessous.
    const URI_COURANTE: &str = "ritornello://radio/2";

    /// Une size qui n'est pas un multiple de `MAX_CHUNK` : trois tranches,
    /// la dernière plus courte que les autres.
    const TAILLE: usize = MAX_CHUNK * 2 + 1234;

    /// La trame d'état **telle que le cœur l'émet quand une cover existe** :
    /// elle porte le `cover_href`, et c'est lui que la trame de cover
    /// reprendra. Une trame sans `cover_href` accompagnée d'une cover
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
            track: Track {
                title: Some("So What".into()),
                cover_href: Some(HREF.to_string()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Pousse l'état **puis** la cover, dans cet order : c'est l'order du
    /// cœur (`display_relay` envoie l'état avant les bytes), et l'inverse
    /// laisserait le greffon dans un état qu'il ne connaît pas en production.
    async fn avec_pochette(state: &SharedState, size: usize) -> Vec<u8> {
        let cover = crate::state::test_cover(HREF, size);
        state.apply_state(trame_avec_pochette()).await;
        state.apply_cover(cover.clone()).await;
        cover.bytes
    }

    #[tokio::test]
    async fn albumart_rend_limage_entiere_et_elle_se_reassemble_a_lidentique() {
        // **Le test central de cette tâche.** Il rejoue la séquence d'un vrai
        // client sur une vraie chaussette, et il n'affirme pas « quelque chose
        // est arrivé » : il compare les bytes réassemblés à ceux qui ont été
        // poussés. Un découpage qui saute, duplique ou décale un seul octet
        // échoue ici — et l'image est du bruit, donc rien ne peut le masquer.
        let (s, _rx) = serveur().await;
        let expected = avec_pochette(&s.state, TAILLE).await;
        let mut c = s.client_pret().await;

        let r = c.recuperer("albumart", URI_COURANTE).await;

        assert_eq!(r.image.len(), TAILLE, "size reassemblee");
        assert_eq!(r.image, expected, "les bytes doivent arriver intacts");
        // Trois tranches : deux pleines, puis le remainder. C'est la preuve que
        // l'offset croissant est honoré (deux requêtes de plus que la première)
        // et que la dernière chunk est plus courte que les autres.
        assert_eq!(r.tailles, vec![MAX_CHUNK, MAX_CHUNK, 1234]);
        // `albumart` n'announcement pas de type MIME, contrairement à `readpicture`.
        assert_eq!(r.mime, None);
        // Et la connection remainder utilisable après une réponse binaire : le
        // path des bytes ne doit pas laisser la session désalignée.
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn readpicture_rend_les_memes_octets_et_annonce_le_type() {
        // M.A.L.P. essaie l'un, puis l'autre : les deux doivent aboutir, et sur
        // la même image. Seul le `type:` les distingue, comme chez MPD.
        let (s, _rx) = serveur().await;
        let expected = avec_pochette(&s.state, TAILLE).await;
        let mut c = s.client_pret().await;

        let r = c.recuperer("readpicture", URI_COURANTE).await;

        assert_eq!(r.image, expected);
        assert_eq!(r.mime.as_deref(), Some("image/jpeg"));
    }

    #[tokio::test]
    async fn une_image_plus_courte_quune_tranche_tient_en_un_seul_aller_retour() {
        // Le cas réel et non le cas limit : la cover mesurée du Cover Art
        // Archive fait 75 Kio, mais une thumbnail peut tenir sous les 8 Kio
        // d'une chunk. Une seule requête, une seule chunk, complète.
        let (s, _rx) = serveur().await;
        let expected = avec_pochette(&s.state, 1000).await;
        let mut c = s.client_pret().await;

        let r = c.recuperer("albumart", URI_COURANTE).await;

        assert_eq!(r.tailles, vec![1000]);
        assert_eq!(r.image, expected);
    }

    #[tokio::test]
    async fn un_offset_au_dela_de_la_fin_est_refuse_sans_fermer() {
        let (s, _rx) = serveur().await;
        avec_pochette(&s.state, TAILLE).await;
        let mut c = s.client_pret().await;

        c.send_frame(&format!("albumart {URI_COURANTE} {}", TAILLE + 1)).await;

        assert_eq!(
            c.reponse().await,
            vec!["ACK [2@0] {albumart} Offset too large".to_string()]
        );
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn sans_pochette_les_deux_commandes_refusent_et_la_connexion_survit() {
        // Le cas ordinaire : un stream sans image. Le client doit recevoir un
        // refus lisible et pouvoir continuer à parler — c'est ce refus qui le
        // fait basculer sur l'autre name, puis renoncer proprement.
        //
        // **`cover_href: None`, et le détail est tout le test.** L'appareil
        // n'announcement aucune image, donc le refus est définitif et doit tomber
        // **tout de suite** : la nouvelle attente de `wait_cover` ne
        // couvre que la fenêtre où une image *a été annoncée* et n'est pas
        // encore arrivée. Une trame porteuse de `cover_href` ici — ce que ce
        // test faisait avant — décrivait au contraire cette fenêtre-là, et le
        // refus immédiat qu'il verrouillait était justement le défaut à
        // corriger.
        let (s, _rx) = serveur().await;
        let mut trame = trame_avec_pochette();
        trame.track.cover_href = None;
        s.state.apply_state(trame).await;
        let mut c = s.client_pret().await;

        for name in ["albumart", "readpicture"] {
            c.send_frame(&format!("{name} {URI_COURANTE} 0")).await;
            assert_eq!(
                c.reponse().await,
                vec![format!("ACK [50@0] {{{name}}} No file exists")]
            );
        }
        c.send_frame("ping").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);
    }

    #[tokio::test]
    async fn binarylimit_change_la_taille_des_tranches_de_cette_connexion() {
        // **Ce que la commande sert vraiment à faire.** Une cover se
        // récupérait par tranches de 8 Kio, la valeur par défaut de MPD : une
        // image de 500 Kio demandait soixante-deux allers-retours. Un client
        // qui announcement accepter plus doit en recevoir plus — et la valeur ne
        // vaut que pour **sa** connection.
        let (s, _rx) = serveur().await;
        let expected = avec_pochette(&s.state, TAILLE).await;
        let mut c = s.client_pret().await;

        c.send_frame("binarylimit 32768").await;
        assert_eq!(c.reponse().await, vec!["OK".to_string()]);

        let r = c.recuperer("albumart", URI_COURANTE).await;
        assert_eq!(r.image, expected);
        // TAILLE tient sous 32 Kio : une seule chunk, là où le défaut en
        // demandait trois.
        assert_eq!(r.tailles, vec![TAILLE], "la chunk demandee doit etre honoree");

        // Un second client, qui n'a rien demandé, garde le défaut : la limit
        // est un fait sur la connection.
        let mut autre = s.client_pret().await;
        let r2 = autre.recuperer("albumart", URI_COURANTE).await;
        assert_eq!(r2.tailles.first(), Some(&MAX_CHUNK));
    }

    #[tokio::test]
    async fn une_pochette_annoncee_mais_pas_encore_arrivee_est_attendue_et_servie() {
        // **La correction de « la cover disparaît au changement de piste ».**
        // Le cœur envoie l'état d'abord, les bytes ensuite : le client est
        // réveillé par cette trame et demande l'image dans la foulée, pendant
        // que le greffon tient encore celle d'avant — ou rien du tout. Il
        // recevait « No file exists », et M.A.L.P., qui mémorise l'absence par
        // piste, ne redemandait jamais.
        //
        // Ici la demande arrive **avant** les bytes, et doit quand même
        // aboutir.
        let (s, _rx) = serveur().await;
        s.state.apply_state(trame_avec_pochette()).await;
        let mut c = s.client_pret().await;

        let state = s.state.clone();
        let expected = crate::state::test_cover(HREF, TAILLE).bytes;
        // La cover arrive pendant que la demande patiente. Une tâche à part,
        // parce que c'est exactement la concurrence réelle : deux canaux
        // distincts, l'un derrière l'autre.
        tokio::spawn(async move {
            state.apply_cover(crate::state::test_cover(HREF, TAILLE)).await;
        });

        let r = c.recuperer("albumart", URI_COURANTE).await;
        assert_eq!(r.image, expected, "l'image attendue doit finish par etre servie");
    }

    #[tokio::test(start_paused = true)]
    async fn une_pochette_annoncee_qui_narrive_jamais_finit_par_etre_refusee() {
        // Le pendant : l'attente est **bornée**. Sans cette bounded, une image
        // qui n'arrive pas — un partage endormi, un 404 du Cover Art Archive —
        // laisserait le client suspendu pour toujours sur une commande dont il
        // attend une réponse.
        //
        // Clock simulée : tokio avance le temps virtuel dès que tout est en
        // attente, donc ce test ne coûte pas les trois seconds réelles et ne
        // suppose aucune durée d'exécution.
        let (s, _rx) = serveur().await;
        s.state.apply_state(trame_avec_pochette()).await;
        let mut c = s.client_pret().await;

        c.send_frame(&format!("albumart {URI_COURANTE} 0")).await;

        assert_eq!(
            c.reponse().await,
            vec!["ACK [50@0] {albumart} No file exists".to_string()],
            "l'attente doit finish par rendre le refus ordinaire"
        );
    }

    #[tokio::test]
    async fn une_reponse_binaire_dans_une_liste_est_refusee_a_son_rang() {
        // MPD l'autorise, nous non : voir la justification sur place dans
        // `serve`. Le refus arrive **à l'accumulation**, donc le `status` qui
        // précède n'a pas été exécuté — c'est ce que l'absence de `volume:`
        // prouve.
        let (s, _rx) = serveur().await;
        avec_pochette(&s.state, TAILLE).await;
        let mut c = s.client_pret().await;

        c.send_frame("command_list_begin").await;
        c.send_frame("status").await;
        c.send_frame(&format!("albumart {URI_COURANTE} 0")).await;
        let recues = c.reponse().await;

        assert_eq!(recues, vec!["ACK [5@1] {albumart} not allowed in command list".to_string()]);
        assert!(!recues.iter().any(|l| l.starts_with("volume: ")), "{recues:?}");
        // L'état de liste a été rendition, et la commande répond bien hors liste :
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
        // Sans horloge : la boucle push_cover des pochettes jusqu'à ce que le
        // dormeur réponde, et une implémentation qui ne réveille pas fait
        // *pendre* le test.
        let (s, _rx) = serveur().await;
        s.state.apply_state(trame_avec_pochette()).await;
        let mut c = s.client_pret().await;
        c.send_frame("idle player").await;

        let mut i = 0usize;
        let premiere = loop {
            tokio::select! {
                biased;
                lue = c.lines.next_line() => {
                    break lue.unwrap().expect("le serveur a ferme la connection");
                }
                // Deux tailles alternées : chaque poussée est donc un
                // changement réel, que la déduplication ne peut pas avaler.
                () = s.state.apply_cover(
                    crate::state::test_cover(HREF, 1000 + (i % 2) * 500),
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
