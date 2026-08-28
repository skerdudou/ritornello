//! État partagé entre la moitié `display` (qui reçoit les trames du cœur) et
//! les sessions clientes MPD (qui répondent aux commands de playback).
//!
//! Le point délicat de tout le greffon vit ici, et il n'est pas dans le
//! protocol : **le réveil manqué**. Un client qui envoie `idle` juste après
//! un changement doit repartir immédiatement, pas wait le changement
//! suivant. Un `Notify` seul perdrait ce réveil — la notification est émise
//! pendant que la session read encore ses versions et compose sa requête, donc
//! avant qu'elle ne s'inscrive, et elle resterait muette jusqu'au changement
//! d'après. D'où la conception retenue : un compteur monotone par
//! sous-système, que la session mémorise **à la connection** et fait vivre de
//! commande en commande, et une comparaison **préalable** dans `wait`.
//! C'est cette comparaison qui interdit le réveil manqué ; le `Notify` ne sert
//! qu'à ne pas sonder.
//!
//! La référence est portée par la connection et non relue à chaque `idle`, et
//! c'est la moitié du dispositif qui manquait : la relire ferait avaler tout ce
//! qui a bougé entre la réponse précédente d'un client et sa line `idle` —
//! c'est-à-dire pendant la seule fenêtre où il n'écoute pas. Voir `versions` et
//! `wait`.

use ritornello_proto::{SourcesCatalog, Command, Cover, Playback, PlayerState, Preset, SourceCatalog};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};

/// Nombre de sous-systèmes, donc size du tableau de compteurs. Une constante
/// et non un `Subsystem::len()` : c'est la bounded du tableau, elle doit être connue
/// à la compilation.
const SUBSYSTEM_COUNT: usize = 4;

/// Les sous-systèmes que `idle` sait nommer, dans l'order où ils indexent le
/// tableau de compteurs.
///
/// Un `enum #[repr(usize)]` servant d'index dans un `[u64; 4]`, et non une
/// table associative : les quatre subsystems sont connus à la compilation, et
/// `versions[sujet as usize]` ne peut pas échouer — pas d'`unwrap` sur un
/// `get`, pas de sujet qu'on aurait oublié d'insérer à la construction.
///
/// Les valeurs explicites ne sont pas décoratives : elles sont l'index, donc
/// **ne pas réordonner** sans réordonner ce que les tests comparent.
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    /// Lecture, pause, arrêt, changement de présélection, position.
    Player = 0,
    /// Volume ou sourdine.
    Mixer = 1,
    /// La file d'attente change. Comme la file d'attente MPD *est* la liste des
    /// présélections de la source active, cela veut dire : changement de
    /// source.
    Playlist = 2,
    /// Le sources_catalog des sources ou de leurs présélections change.
    ///
    /// Son unique déclencheur est `apply_catalog`, et c'est tout le sens
    /// des deux canaux : une trame d'état, même changeant tout le remainder, ne le
    /// bouge jamais — sinon un client abonné aux seules listes enregistrées
    /// serait réveillé à chaque seconde de playback.
    StoredPlaylist = 3,
}

/// La cover courante, telle que le greffon la tient entre deux pistes.
///
/// **Les bytes sont derrière un `Arc`, et c'est structurel** : `Snapshot`
/// est cloné à *chaque* commande de *chaque* session (voir `read`), et une
/// cover pèse jusqu'à `ritornello_proto::COVER_MAX_BYTES` — **20 Mio**. Un
/// `Vec<u8>` nu ferait donc recopier vingt mébioctets pour répondre `ping`.
/// L'`Arc` rend le clone à un incrément de compteur.
///
/// **Ce que l'`Arc` ne garantit pas**, et il faut l'écrire ici parce que ce
/// paragraphe l'a promis à tort : l'image n'existe une seule fois dans le
/// processus que **par génération**. Une session qui répond `albumart` tient
/// son propre clone de l'`Arc` — dans son `Snapshot` *et* dans la réponse
/// binaire — pendant tout son `write_all`, donc un client qui demande une
/// chunk puis cesse de read **épingle cette génération**. Une cover
/// poussée pendant ce temps est une génération de plus, qu'une autre session
/// peut épingler à son tour. Le produit s'écrit donc en clair :
/// `MAX_SESSIONS × COVER_MAX_BYTES` = 16 × 20 Mio = **320 Mio**, auxquels
/// s'add la génération que l'état tient lui-même, soit **340 Mio** sur un
/// appareil d'un gibioctet partagé. Voir `commands::MAX_CHUNK` pour ce qui
/// bounded le remainder, et pour ce qui n'est délibérément **pas** mitigé.
///
/// `Arc<Vec<u8>>` et non `Arc<[u8]>` : la conversion depuis le `Vec<u8>` de la
/// trame est alors un déplacement, là où `Arc<[u8]>::from` réallouerait et
/// recopierait les 20 Mio une fois de plus par piste.
#[derive(Clone, PartialEq)]
pub struct HeldCover {
    /// Exactement le `cover_href` que la trame d'état publie pour la même
    /// image. C'est **la** corrélation entre l'image et ce qui plays : le cœur
    /// envoie l'état d'abord et la cover ensuite, donc il existe une
    /// fenêtre où l'état désigne déjà la piste suivante et où la cover
    /// tenue est encore celle de la précédente. Comparer ce champ à
    /// `state.track.cover_href` est ce qui interdit de serve l'une pour
    /// l'autre (voir le bras `albumart` de `commands.rs`).
    pub href: String,
    /// Type MIME reconnu aux bytes d'en-tête par le cœur, jamais à une
    /// extension. C'est le `type:` que `readpicture` publie.
    pub mime: String,
    pub bytes: Arc<Vec<u8>>,
}

/// `Debug` écrit à la main : le dérivé imprimerait les vingt mébioctets de
/// l'image, et `Snapshot` est `Debug` — donc le moindre `assert_eq!` d'un
/// test raté vomirait l'image entière dans la sortie.
impl std::fmt::Debug for HeldCover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldCover")
            .field("href", &self.href)
            .field("mime", &self.mime)
            .field("bytes", &format_args!("{} o", self.bytes.len()))
            .finish()
    }
}

impl From<Cover> for HeldCover {
    fn from(c: Cover) -> Self {
        Self { href: c.href, mime: c.mime, bytes: Arc::new(c.bytes) }
    }
}

/// Une cover **telle que le cœur en push_cover une**, pour les tests des trois
/// modules qui en ont besoin (celui-ci, `commands`, `session`).
///
/// Le réalisme de cette fixe n'est pas une politesse : une cover bâtie d'un
/// `Default::default()` prouverait une causalité à l'intérieur d'une trame que
/// le producteur ne peut pas émettre. Trois traits sont donc empruntés au vrai
/// producteur :
///
/// * le `href` est de la forme `/api/cover/{clé}` que `cover::HREF_PREFIX`
///   fabrique, et l'appelant le repasse dans `state.track.cover_href` — c'est
///   la seule corrélation qui existe entre l'image et ce qui plays ;
/// * les bytes **commencent par un vrai en-tête JPEG**, parce que le cœur
///   reconnaît le MIME aux bytes d'en-tête et refuse tout ce qu'il ne
///   reconnaît pas : une image dont l'en-tête serait faux ne serait jamais
///   poussée, donc un test qui en emploierait une testerait l'impossible ;
/// * la suite est du **bruit** d'un générateur congruentiel et non un motif
///   régulier : c'est ce qui rend visible une chunk sautée, dupliquée ou
///   décalée d'un octet, qu'un remplissage constant masquerait entièrement.
#[cfg(test)]
pub(crate) fn test_cover(href: &str, size: usize) -> Cover {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    let mut x: u32 = 0x1234_5678;
    while bytes.len() < size {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push((x >> 24) as u8);
    }
    bytes.truncate(size);
    Cover { href: href.to_string(), mime: "image/jpeg".to_string(), bytes }
}

/// Copie cohérente de tout ce qu'une session cliente a besoin de read pour
/// composer une réponse : l'état poussé par le cœur, ce que le greffon croit
/// de la playback, et les compteurs.
///
/// Un seul instantané rendition d'un coup, et non quatre accesseurs : une réponse
/// `status` publie l'état *et* la version de file, et les read par deux prises
/// de verrou successives les laisserait se contredire au milieu d'une réponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// La dernière trame reçue du cœur, **éventuellement recouverte d'un
    /// calque optimiste** : `acknowledge_optimistic` y pose le volume qu'une session
    /// vient de demander, avant que le cœur ne l'ait confirmé (voir là-bas).
    /// Ne pas read ce champ comme le verbatim de ce que le cœur a envoyé — la
    /// trame suivante rétablit la vérité de toute façon, et la comparaison
    /// d'`apply_state` réveille `Mixer` si elle la contredit.
    pub state: PlayerState,
    /// Ce que le greffon **croit** de la playback, y compris une bascule qu'il
    /// vient d'émettre et que la trame n'a pas encore confirmée : c'est la
    /// course de `pause`, où un client qui envoie `pause` puis `status` dans
    /// la même foulée lirait sinon l'état d'avant sa propre commande et
    /// afficherait un bouton qui n'a pas bougé.
    pub optimistic_playback: Playback,
    /// Compteur de version de la file d'attente, celui que `status` publie
    /// sous `playlist`.
    ///
    /// **Monotone**, jamais remis à zéro : un client compare la version qu'il
    /// détient à celle-ci pour savoir s'il a manqué quelque chose, et une
    /// pushback à zéro lui ferait croire qu'il n'a rien manqué alors que tout a
    /// changé.
    pub queue_version: u32,
    /// Un compteur par sujet, du même usage mais pour `idle` : une session
    /// endormie a mémorisé ce tableau, le compare à celui-ci, et repart
    /// aussitôt si quelque chose a bougé pendant qu'elle s'installait.
    pub versions: [u64; SUBSYSTEM_COUNT],
    /// Le dernier sources_catalog reçu du cœur : **toutes** les sources déclarées,
    /// dans l'order de bascule de `SourceCycle`, et les présélections nommées
    /// de chacune quand elle sait les énumérer.
    ///
    /// Il décrit toutes les sources et non la seule active, et c'est
    /// indispensable : `listplaylistinfo "radio"` s'interroge pendant que le cd
    /// plays. Il arrive par un canal distinct des trames d'état (voir
    /// `apply_catalog`), donc il survit à n'importe quel nombre de trames
    /// sans être renvoyé.
    ///
    /// Vide avant la première trame de sources_catalog : un client verra alors une
    /// liste de listes enregistrées clear, et la file d'attente retombe sur la
    /// synthèse depuis `preset_count`. C'est bien la vérité de cet instant —
    /// le greffon ne sait encore rien du sources_catalog.
    pub sources_catalog: SourcesCatalog,
    /// La dernière cover reçue du cœur, ou `None` tant qu'aucune n'est
    /// arrivée — ce qui est le cas ordinaire d'un stream sans image, et non une
    /// anomalie.
    ///
    /// **Une seule**, jamais un cache par piste : l'appareil ne sait publier
    /// que la cover de ce qui plays (voir `DisplayPlugin::cover`), et
    /// mémoriser les précédentes ferait tenir plusieurs mébioctets pour serve
    /// des URI que plus rien ne plays — exactement ce que le bras `albumart`
    /// refuse de faire.
    ///
    /// **Et elle est relâchée, pas seulement remplacée** : `apply_state`
    /// la remet à `None` dès qu'une trame d'état announcement `cover_href: None`.
    /// Sans cela le greffon retenait jusqu'à `COVER_MAX_BYTES` **pour la vie
    /// du processus**, longtemps après l'arrêt de la playback, pour des bytes
    /// que plus aucune commande ne pouvait serve. Voir la garde sur place.
    pub cover: Option<HeldCover>,
}

impl Snapshot {
    /// Les présélections nommées de cette source, telles que le sources_catalog les
    /// donne — ou `None` si le sources_catalog ne connaît pas ce name.
    ///
    /// La distinction compte pour `listplaylistinfo` et `load` : un name absent
    /// du sources_catalog est un `ACK 50` (« cette liste n'existe pas »), alors qu'un
    /// name connu dont la liste est **clear** est une source qui ne sait pas
    /// énumérer — une réponse clear et bien formée, pas une erreur.
    pub fn source_catalog(&self, name: &str) -> Option<&SourceCatalog> {
        self.sources_catalog.sources.iter().find(|s| s.name == name)
    }

    /// Les présélections de la source **active**, ou une chunk clear. C'est
    /// d'elles que la file d'attente MPD est faite.
    pub fn active_presets(&self) -> &[Preset] {
        self.source_catalog(&self.state.source).map_or(&[], |s| s.presets.as_slice())
    }
    /// Ce qu'il faut publier comme état de playback : l'optimiste, pas le brut
    /// de la trame.
    ///
    /// Appelé par le `status` de `commands.rs` depuis la Task 6. L'écrire ici
    /// plutôt qu'à chaque site de réponse évite que l'un d'eux ait à se
    /// souvenir *lequel* des deux champs fait foi — et un test de ce module-là
    /// échoue si `status` read `state.playback`.
    pub fn playback(&self) -> Playback {
        self.optimistic_playback
    }
}

/// Ce que toutes les sessions clientes partagent : l'instantané current et le
/// réveil des `idle` en attente.
///
/// Le verrou est un `tokio::sync::RwLock` et non un `Mutex` : les sessions ne
/// font presque que read, et l'une qui compose un `listplaylistinfo` de 51
/// lines ne doit pas retarder les autres. Les seuls écrivains sont la moitié
/// `display` (une trame) et une session qui vient d'émettre une commande.
#[derive(Default)]
pub struct SharedState {
    inner: RwLock<Snapshot>,
    /// Réveille les `idle` en attente. `notify_waiters` et non `notify_one` :
    /// un changement concerne **tous** les dormeurs, et un permis mémorisé
    /// pour un seul d'entre eux serait pire qu'inutile ici — la comparaison
    /// des compteurs plays déjà le rôle de la mémoire.
    wakeup: Notify,
}

/// Avance normale de l'horloge de position entre deux trames, en seconds,
/// au-delà de laquelle un changement est un **déplacement** et non le temps qui
/// passe.
///
/// Cinq et non une, alors que le cœur émet une trame par seconde : les trames
/// voyagent par un `watch`, qui **coalesce**. Un relais momentanément en retard
/// — un Pi occupé, une cover en cours de playback sur un partage — ne reçoit
/// que la dernière valeur, et voit donc l'horloge sauter de deux, trois ou
/// quatre seconds sans que personne n'ait rien déplacé. La marge couvre ce
/// retard. Le prix est un déplacement de moins de cinq seconds qui ne réveille
/// pas les dormeurs ; ils le liront à leur prochain `status`, où `elapsed` est
/// toujours juste.
const NORMAL_SEEK_S: u32 = 5;

/// Ce changement de position est-il un événement, ou seulement le temps qui
/// passe ?
///
/// **C'est le correctif du défaut le plus coûteux de ce greffon.** Le cœur
/// push_cover une trame d'état **par seconde** en playback, et son seul champ qui
/// bouge est alors `position_s`. Le comparer comme les autres marquait donc
/// `Player` une fois par seconde, et `idle player` — sur lequel tout client MPD
/// se met en attente — se réveillait au même rythme. M.A.L.P. redemandait alors
/// `status`, `currentsong` **et la cover** chaque seconde, ce qui explique
/// l'instabilité observée et l'image qui disparaît : un `albumart` relancé sans
/// cesse au milieu de son propre transfert par tranches. Le vrai MPD n'émet
/// jamais `player` pour l'écoulement du temps ; `elapsed` se read dans `status`,
/// que le client interroge quand il veut.
///
/// Ce qui remainder un événement :
/// - l'apparition ou la disparition de la position (une piste qui commence, un
///   stream sans position) ;
/// - un recul, toujours — c'est un retour en arrière demandé, ou une nouvelle
///   piste qui repart de zéro ;
/// - une avance supérieure à `NORMAL_SEEK_S`, c'est-à-dire un déplacement et
///   non l'horloge.
fn position_jump(avant: Option<u32>, apres: Option<u32>) -> bool {
    match (avant, apres) {
        (Some(a), Some(b)) => b < a || b - a > NORMAL_SEEK_S,
        // L'un des deux est absent : présence et absence sont deux états
        // différents de la playback, et leur bascule est un événement.
        (a, b) => a != b,
    }
}

/// Marque un sujet comme ayant bougé, sans doublon.
///
/// Le dédoublonnage n'est pas cosmétique : une liste de commands MPD peut
/// contenir deux `pause`, et incrémenter deux fois le compteur pour un seul
/// passage sous le verrou ferait publier deux changements là où il n'y en a
/// qu'un.
fn mark(moved: &mut Vec<Subsystem>, sujet: Subsystem) {
    if !moved.contains(&sujet) {
        moved.push(sujet);
    }
}

/// Ce qu'un `idle` a appris : les subsystems à annoncer, et les compteurs de
/// l'instant où ils ont été constatés.
///
/// Une structure et non un `(Vec, [u64; 4])` nu : les deux champs se
/// confondraient à l'usage, et c'est le second qui porte la subtilité — il
/// n'est pas « les compteurs courants » mais « ceux qui ont décidé ce réveil ».
#[derive(Debug, PartialEq)]
pub struct Wakeup {
    /// Les subsystems qui ont bougé, dans l'order où le client les a demandés.
    pub moved: Vec<Subsystem>,
    /// Tous les compteurs, lus dans la même prise de verrou que `moved`.
    ///
    /// L'appelant n'en retient que les entrées des subsystems qu'il **announcement** :
    /// c'est l'équivalent exact du « n'effacer que les drapeaux rapportés » de
    /// MPD.
    pub versions: [u64; SUBSYSTEM_COUNT],
}

impl SharedState {
    /// Copie de l'instantané current. Une copie et non une garde : aucune
    /// session ne doit retenir le verrou au-delà de l'instant de la playback,
    /// même si elle compose ensuite une réponse longue.
    ///
    /// Chaque session cliente l'invoque une fois par commande, pour répondre
    /// depuis la copie plutôt que sous le verrou. Les compteurs qu'un `idle`
    /// mémorise sont dans cette même copie : c'est ce qui les rend cohérents
    /// avec l'état publié dans la même réponse.
    pub async fn read(&self) -> Snapshot {
        self.inner.read().await.clone()
    }

    /// Copie du tableau de compteurs, à mémoriser **une fois par connection** et
    /// à faire vivre de commande en commande jusqu'à `wait`.
    ///
    /// C'est la moitié utile du dispositif anti-réveil-manqué, et son appelant
    /// de production est `session::serve`, **au moment de la bannière** : les
    /// compteurs qu'un `idle` compare sont ceux de la dernière fois que ce
    /// client a été *informé* d'un changement, jamais ceux de l'instant où il
    /// a écrit sa line `idle`.
    ///
    /// **Ce qu'il ne faut surtout pas refaire** — c'était l'état de ce code, et
    /// c'était un défaut : read les compteurs dans l'`Snapshot` de la
    /// commande `idle` elle-même. Cette playback-là est bien cohérente avec
    /// l'état publié dans la même réponse, mais elle **avale** tout ce qui a
    /// bougé entre la réponse précédente et la line `idle`, c'est-à-dire
    /// exactement la fenêtre pendant laquelle un client MPD n'écoute pas. Le
    /// vrai MPD accumule ses drapeaux **par connection** depuis la connection, et
    /// un événement survenu entre deux commands y est rapporté à l'`idle`
    /// suivant. Pour `stored_playlist`, l'avaler n'est pas transitoire : rien
    /// ne le rejouera avant le prochain changement de sources_catalog, donc
    /// `listplaylists` remainder périmé, potentiellement pour toujours.
    ///
    /// Le sens de l'erreur acceptable est l'autre : un réveil superflu coûte au
    /// client une interrogation redondante, un réveil manquant lui coûte la
    /// justesse de son écran (le même arbitrage que `acknowledge_optimistic` et
    /// `apply_cover` énoncent chacun de leur côté).
    pub async fn versions(&self) -> [u64; SUBSYSTEM_COUNT] {
        self.inner.read().await.versions
    }

    /// Applique une trame du cœur : elle fait autorité sur tout.
    ///
    /// (voir aussi `position_jump`, qui décide ce que l'horloge de position
    /// vaut comme événement)
    ///
    /// Les subsystems qui bougent sont décidés **par comparaison champ par champ**
    /// avec l'état précédent, et pas par le seul fait qu'une trame soit
    /// arrivée : le cœur déduplique déjà, mais une reconnexion de la moitié
    /// `display` renvoie l'état current, et cela ne doit pas passer pour un
    /// changement — sinon chaque redémarrage du greffon réveillerait tous les
    /// clients pour rien.
    pub async fn apply_state(&self, state: PlayerState) {
        let mut moved = Vec::new();
        {
            let mut inst = self.inner.write().await;
            let avant = &inst.state;

            if state.volume != avant.volume || state.muted != avant.muted {
                mark(&mut moved, Subsystem::Mixer);
            }
            if state.source != avant.source {
                // Deux subsystems pour un seul champ : la file d'attente *est* la
                // liste des présélections de la source active, donc changer de
                // source change la file (`playlist`) ; et ce qui plays change
                // avec elle (`player`). Un client qui n'écoute que `player`
                // doit apprendre qu'on a changé de source.
                mark(&mut moved, Subsystem::Playlist);
                mark(&mut moved, Subsystem::Player);
            }
            if state.preset_count != avant.preset_count {
                // `preset_count` est ce dont la file d'attente MPD est faite
                // **à défaut de liste nommée** : pour une source qui ne sait
                // pas énumérer (le cd, les fichiers), c'est tout ce que le
                // greffon sait de la file. Un disque inséré passe de
                // `None`/`Some(0)` à
                // `Some(12)` sans changer de name de source, et sans cette
                // comparaison aucun client n'apprendrait qu'il y a douze
                // pistes à jouer — l'action la plus ordinaire qui soit.
                //
                // `Playlist` seul, et **pas** `Player` : c'est la file qui a
                // changé, pas ce qui plays. (`source` bouge les deux parce
                // qu'elle change les deux ; `preset_count` seul ne touche pas
                // au track current.)
                mark(&mut moved, Subsystem::Playlist);
            }
            if state.playback != avant.playback
                || state.preset != avant.preset
                || position_jump(avant.position_s, state.position_s)
                || state.track != avant.track
            {
                mark(&mut moved, Subsystem::Player);
            }

            // La trame écrase l'optimisme, y compris quand elle le contredit :
            // l'optimisme n'est qu'un pont jeté entre la commande émise et sa
            // confirmation, et le laisser survivre à une trame ferait mentir
            // `status` indéfiniment si le cœur avait refusé la bascule.
            inst.optimistic_playback = state.playback;
            inst.state = state;

            // **La cover est relâchée ici, et c'est le seul endroit qui
            // puisse le faire.** `cover_href: None` est le signal du cœur que
            // plus rien de ce qui plays n'a d'illustration ; or `cover`
            // n'était jamais remis à `None`, donc le greffon gardait jusqu'à
            // `COVER_MAX_BYTES` — 20 Mio — pour la vie du processus, y compris
            // longtemps après l'arrêt de la playback, sur un appareil d'un
            // gibioctet partagé avec mpv.
            //
            // Ces bytes-là n'étaient d'ailleurs plus servables : le bras
            // `albumart` exige que le `href` tenu soit celui que la trame
            // announcement (voir `commands::cover`), donc `cover_href: None`
            // les avait déjà rendus inatteignables. Les libérer ne retire
            // aucune réponse à personne.
            //
            // **Pourquoi ce critère et pas « le `href` tenu diffère de celui
            // qu'announcement la trame »**, qui libérerait un peu plus tôt : le cœur
            // envoie l'état *avant* les bytes, donc il existe une fenêtre
            // normale où la trame announcement déjà la clé suivante alors que la
            // cover tenue est encore la précédente. Le critère strict y
            // détruirait une image que la trame d'après aurait légitimée, si
            // l'order des deux canaux s'inversait un jour. `None` est le seul
            // signal qui ne dépende pas de cet order.
            if inst.state.track.cover_href.is_none() {
                // Sans réveil propre : la trame qui fait passer `cover_href` à
                // `None` change `track`, donc elle a déjà marqué `Player`
                // ci-dessus. Et dans le cas dégénéré où `track` serait
                // identique (une trame répétée après que la cover a cessé
                // d'être servable), il n'y a rien à annoncer — `albumart`
                // refusait déjà.
                inst.cover = None;
            }

            for sujet in &moved {
                inst.versions[*sujet as usize] += 1;
            }
            if moved.contains(&Subsystem::Playlist) {
                // Exactement quand `Playlist` bouge : les deux compteurs
                // disent la même chose à deux publics (`idle` et le champ
                // `playlist` de `status`), et les désynchroniser ferait
                // répondre `plchanges` à côté du réveil qui vient de partir.
                inst.queue_version += 1;
            }
        }
        if !moved.is_empty() {
            tracing::trace!("mpd frame moved subsystems {moved:?}");
            self.wakeup.notify_waiters();
        }
    }

    /// Applique un sources_catalog reçu du cœur : la liste des sources et leurs
    /// présélections nommées.
    ///
    /// **Deux subsystems, et pas toujours les deux.**
    /// - `StoredPlaylist` bouge dès que le sources_catalog diffère du précédent :
    ///   c'est le sous-système que MPD réserve aux listes enregistrées, et
    ///   chaque source *est* une liste enregistrée ici.
    /// - `Playlist` (et avec lui `queue_version`) ne bouge que si les
    ///   présélections de la source **active** ont changé — la file d'attente
    ///   vient de là et de nulle part ailleurs. Renommer une station d'une
    ///   source qui ne plays pas change les listes enregistrées sans toucher à
    ///   la file : réveiller `Playlist` ferait retélécharger 51 lines à tous
    ///   les clients pour rien, et un `plchanges` répondrait une file
    ///   identique sous une version neuve.
    ///
    /// Comparaison et non affectation sèche, exactement comme `apply_state`
    /// et pour la même raison : le cœur envoie la valeur courante **à la
    /// connection**, donc une reconnexion de la moitié `display` repasse ici
    /// avec un sources_catalog identique, et cela ne doit pas passer pour un
    /// changement — sinon chaque redémarrage du greffon réveillerait tous les
    /// clients.
    pub async fn apply_catalog(&self, sources_catalog: SourcesCatalog) {
        let mut moved = Vec::new();
        {
            let mut inst = self.inner.write().await;
            if inst.sources_catalog == sources_catalog {
                return;
            }
            // Tout changement réel du sources_catalog bouge `StoredPlaylist` : on est
            // passé la déduplication ci-dessus, donc le sources_catalog diffère
            // vraiment. C'est ce qui réveille un client endormi sur
            // `idle stored_playlist` — le seul sous-système que rien
            // n'incrémentait avant cette tâche.
            mark(&mut moved, Subsystem::StoredPlaylist);
            // Lu avant l'écrasement, sur le name de source de l'instantané
            // current : c'est la source active telle que la dernière trame
            // d'état l'a dite, la seule autorité sur ce qui plays.
            let presets_avant = inst.active_presets().to_vec();
            inst.sources_catalog = sources_catalog;
            if inst.active_presets() != presets_avant.as_slice() {
                mark(&mut moved, Subsystem::Playlist);
            }

            for sujet in &moved {
                inst.versions[*sujet as usize] += 1;
            }
            if moved.contains(&Subsystem::Playlist) {
                // Le même appariement que dans `apply_state` : les deux
                // compteurs disent la même chose à deux publics (`idle` et le
                // champ `playlist` de `status`), et les désynchroniser ferait
                // répondre `plchanges` à côté du réveil qui vient de partir.
                inst.queue_version += 1;
            }
        }
        if !moved.is_empty() {
            tracing::trace!("mpd sources_catalog moved subsystems {moved:?}");
            self.wakeup.notify_waiters();
        }
    }

    /// Applique une cover reçue du cœur : les bytes que `albumart` et
    /// `readpicture` serviront.
    ///
    /// **Le sujet déplacé est `Player`, et c'est le seul choix disponible.**
    /// Le protocol MPD n'a pas de sous-système pour les pochettes : la liste
    /// des names que `idle` accepte est fixée par MPD et un `changed: cover`
    /// ne serait compris par aucun client. Restait à choisir parmi les quatre
    /// que ce greffon émet, et `Player` est celui que les clients relient
    /// réellement à l'illustration : un client MPD redemande `currentsong`
    /// **puis** l'image au réveil de `player`, parce que la cover est un
    /// fait sur le track current. `Mixer` (le volume) et `Playlist` (la
    /// file) ne provoquent aucun rafraîchissement d'image chez les clients
    /// connus, et `StoredPlaylist` est réservé aux listes enregistrées.
    ///
    /// Ce réveil n'est pas décoratif, c'est lui qui rend la fonction utile :
    /// le cœur envoie **l'état d'abord, la cover ensuite** (voir
    /// `display_relay`). Un client réveillé par la seule trame d'état
    /// demande donc son image pendant que le greffon tient encore celle de la
    /// piste précédente — donc reçoit un refus — et sans ce second réveil il
    /// n'apprendrait jamais que l'image est arrivée. Le prix est un
    /// `changed: player` de plus par changement de piste, la même dissymétrie
    /// assumée que `PlayPause` dans `acknowledge_optimistic` : un réveil superflu
    /// coûte au client une interrogation redondante, un réveil manquant lui
    /// coûte une cover clear jusqu'à la piste suivante.
    ///
    /// Comparaison et non affectation sèche, comme les deux fonctions
    /// ci-dessus : le cœur ne push_cover déjà que sur changement, mais il push_cover
    /// aussi la cover courante **au câblage**, donc une reconnexion de la
    /// moitié `display` repasse ici avec la même image et cela ne doit
    /// réveiller personne. La comparaison porte sur les bytes et pas
    /// seulement sur le `href` : l'égalité de deux `Arc` de même contenu est
    /// tranchée sans copie, et se fier au seul `href` ferait taire une image
    /// réellement différente publiée sous la même clé.
    pub async fn apply_cover(&self, cover: Cover) {
        {
            let mut inst = self.inner.write().await;
            let cover = HeldCover::from(cover);
            if inst.cover.as_ref() == Some(&cover) {
                return;
            }
            inst.cover = Some(cover);
            inst.versions[Subsystem::Player as usize] += 1;
        }
        tracing::trace!("mpd cover moved subsystem Player");
        self.wakeup.notify_waiters();
    }

    /// Acte ce que le greffon vient d'émettre, avant que le cœur ne le
    /// confirme.
    ///
    /// **Trois commands seulement**, et c'est délibéré : `PlayPause` (bascule
    /// `Playing`↔`Paused`), `SetVolume` (pose le volume) et `Mute` (bascule la
    /// sourdine). Tout le remainder est ignoré, parce que deviner l'effet d'un
    /// `Select` sur la position, le track ou la présélection serait faux plus
    /// souvent que juste — c'est la source active qui décide, et elle seule. Un
    /// `status` légèrement en retard est bénin ; un `status` qui invente un
    /// track ne l'est pas.
    ///
    /// **`Mute` a rejoint la liste avec le `setvol` qui démute** (voir
    /// `commands::setvol`), et sans elle ce démutage aurait été invisible :
    /// `status` publie `volume: 0` dès que `state.muted` est vrai, donc acter le
    /// seul `SetVolume(40)` aurait laissé un client read `volume: 0` juste
    /// après avoir remonté son curseur — son curseur retombant à zéro,
    /// c'est-à-dire le défaut exact que le calque optimiste existe pour
    /// éviter. Le cœur, lui, honore `Mute` sans condition (`self.muted =
    /// !self.muted`), donc la bascule actée ici n'invente rien.
    ///
    /// Le volume, lui, est posé dans `state` faute d'un champ optimiste à part.
    /// C'est voulu et sans risque : la trame suivante l'écrase de toute façon,
    /// et si le cœur avait borné ou refusé la valeur, la comparaison
    /// d'`apply_state` verra la différence et réveillera `Mixer`. Le seul
    /// effet de bord est que la trame *confirmante* ne rebouge rien — d'où
    /// l'incrément fait ici même.
    ///
    /// **L'asymétrie avec `PlayPause` est voulue**, et il faut l'écrire parce
    /// qu'elle se read comme un oubli : la bascule ne touche pas
    /// `state.playback`, donc la trame confirmante rebouge `Player` une seconde
    /// fois — un `changed: player` redondant. C'est le choix conservateur.
    /// `SetVolume` porte une valeur absolue que le cœur honore presque
    /// toujours au bit près : sans l'incrément fait ici, la trame confirmante
    /// serait identique et *personne* ne serait réveillé — il n'y avait donc
    /// pas le choix. `PlayPause` ne porte aucune valeur : c'est le greffon qui
    /// calcule la bascule, et la source active peut très bien finish ailleurs
    /// (un direct qu'on ne met pas en pause). Laisser `state.playback` intact
    /// garde la trame comme seule autorité sur ce champ, et le prix est un
    /// réveil de trop. Ce prix est le bon sens de la dissymétrie : un réveil
    /// superflu coûte au client une interrogation `status` redondante, un
    /// réveil manquant lui coûte la justesse de son écran.
    ///
    /// Appelée par la session **après** avoir poussé les commands sur le
    /// canal, jamais avant : acter une bascule qu'on n'a pas émise ferait
    /// mentir `status` jusqu'à la trame suivante.
    pub async fn acknowledge_optimistic(&self, commands: &[Command]) {
        let mut moved = Vec::new();
        {
            let mut inst = self.inner.write().await;
            for commande in commands {
                match commande {
                    Command::PlayPause => match inst.optimistic_playback {
                        // Sans effet à l'arrêt : `PlayPause` y démarre une
                        // playback dont le greffon ne sait ni quoi ni où, donc
                        // il attend la trame plutôt que d'annoncer `Playing`
                        // sur un track clear.
                        Playback::Stopped => {}
                        Playback::Playing => {
                            inst.optimistic_playback = Playback::Paused;
                            mark(&mut moved, Subsystem::Player);
                        }
                        Playback::Paused => {
                            inst.optimistic_playback = Playback::Playing;
                            mark(&mut moved, Subsystem::Player);
                        }
                    },
                    Command::SetVolume(niveau) => {
                        let niveau = *niveau;
                        // Comparaison et non affectation seche : un `setvol`
                        // qui repose le volume current (M.A.L.P. en envoie a
                        // chaque relachement de curseur) ne doit pas reveiller
                        // tous les autres clients pour rien.
                        if inst.state.volume != niveau {
                            inst.state.volume = niveau;
                            mark(&mut moved, Subsystem::Mixer);
                        }
                    }
                    // Bascule et non affectation, parce que la commande est une
                    // bascule : le cœur fait `muted = !muted` sans condition
                    // (voir `Command::Mute` dans le cœur), donc l'actée ici est
                    // exacte et non devinée. Pas de comparaison à faire — une
                    // bascule change toujours quelque chose.
                    Command::Mute => {
                        inst.state.muted = !inst.state.muted;
                        mark(&mut moved, Subsystem::Mixer);
                    }
                    _ => {}
                }
            }
            for sujet in &moved {
                inst.versions[*sujet as usize] += 1;
            }
            // Pas de `queue_version` ici : aucune des deux commands actées ne
            // touche la file d'attente.
        }
        if !moved.is_empty() {
            tracing::trace!("mpd optimistic update moved subsystems {moved:?}");
            self.wakeup.notify_waiters();
        }
    }

    /// Attend qu'un des `subsystems` demandés bouge par rapport aux compteurs
    /// `seen`, et rend ceux qui ont bougé — dans l'order où ils ont été
    /// demandés — **avec les compteurs de l'instant qui a décidé**.
    ///
    /// **Compare d'abord, attend ensuite.** Si quelque chose a bougé depuis
    /// que l'appelant a lu `seen`, la fonction rend la main sans jamais
    /// toucher au `Notify` : c'est là et nulle part ailleurs que le réveil
    /// manqué est interdit. Et `seen` n'est **pas** un instantané pris au
    /// moment de la commande `idle` : c'est la référence que la connection
    /// porte depuis sa bannière (voir `versions`), donc un changement survenu
    /// entre deux commands de ce client est encore devant elle et sort ici.
    ///
    /// `Wakeup::versions` est ce qui permet à l'appelant d'avancer sa référence
    /// **sujet par sujet** : le vrai MPD n'efface que les drapeaux qu'il vient
    /// de rapporter, et tout avancer d'un coup perdrait le changement d'un
    /// sujet non demandé — la même erreur que celle-ci répare, d'un cran plus
    /// loin.
    ///
    /// L'inscription au réveil est faite *sous le verrou de playback*, avant la
    /// comparaison. Sans cela le trou se rouvrirait d'un cran plus loin : un
    /// `notify_waiters` émis entre la comparaison et le premier sondage du
    /// `Notified` ne trouverait aucun inscrit, et le dormeur attendrait le
    /// changement d'après. Un écrivain a besoin du verrou en écriture, donc
    /// tant que la garde en playback est tenue, aucun changement ne peut se
    /// glisser entre l'inscription et la comparaison.
    ///
    /// La boucle n'est pas de la prudence en trop : `notify_waiters` réveille
    /// tous les dormeurs, y compris ceux dont aucun sujet demandé n'a bougé,
    /// et ceux-là doivent se rendormir.
    ///
    /// Appelée par la session pour tenir un `idle`. Une liste de subsystems clear
    /// n'en sort jamais, et c'est le contrat : voir `Outcome::Wait`.
    pub async fn wait(&self, subsystems: &[Subsystem], seen: [u64; SUBSYSTEM_COUNT]) -> Wakeup {
        loop {
            let notifie = self.wakeup.notified();
            tokio::pin!(notifie);
            let (moved, versions) = {
                let inst = self.inner.read().await;
                // `enable` inscrit le futur maintenant plutôt qu'au premier
                // sondage : voir le raisonnement sur le verrou ci-dessus.
                let _ = notifie.as_mut().enable();
                let moved = subsystems
                    .iter()
                    .copied()
                    .filter(|sujet| inst.versions[*sujet as usize] != seen[*sujet as usize])
                    .collect::<Vec<_>>();
                // Les compteurs de **cette** playback, et non d'une seconde
                // prise de verrou après coup : entre les deux, un sujet
                // rapporté pourrait rebouger, et l'appelant avancerait sa
                // référence au-delà d'un changement jamais annoncé.
                (moved, inst.versions)
            };
            if !moved.is_empty() {
                return Wakeup { moved, versions };
            }
            notifie.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un sources_catalog tel que le cœur en émet : chaque source déclarée est
    /// nommée, et ses présélections sont celles qu'elle sait énumérer (vides
    /// pour le cd, qui remainder au corps par défaut de `list_presets`).
    fn catalogue_de(sources: &[(&str, &[(u8, &str)])]) -> SourcesCatalog {
        SourcesCatalog {
            sources: sources
                .iter()
                .map(|(name, presets)| SourceCatalog {
                    name: (*name).to_string(),
                    presets: presets
                        .iter()
                        .map(|(index, name)| Preset { index: *index, name: (*name).to_string() })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Le plus petit sources_catalog que le cœur puisse émettre : une source nommée,
    /// avec une présélection.
    fn catalogue_a_une_source() -> SourcesCatalog {
        catalogue_de(&[("radio", &[(1, "FIP")])])
    }

    #[tokio::test]
    async fn lire_rend_letat_par_defaut_avant_toute_application() {
        let partage = SharedState::default();
        assert_eq!(partage.read().await, Snapshot::default());
    }

    #[tokio::test]
    async fn appliquer_etat_remplace_ce_que_lire_rend_ensuite() {
        let partage = SharedState::default();
        let nouvel_etat = PlayerState { volume: 42, source: "radio".into(), ..Default::default() };

        partage.apply_state(nouvel_etat.clone()).await;

        assert_eq!(partage.read().await.state, nouvel_etat);
    }

    #[tokio::test]
    async fn une_trame_qui_change_le_volume_reveille_mixer_et_pas_playlist() {
        let e = SharedState::default();
        let avant = e.versions().await;
        e.apply_state(PlayerState { volume: 40, ..Default::default() }).await;
        let apres = e.versions().await;
        assert_ne!(avant[Subsystem::Mixer as usize], apres[Subsystem::Mixer as usize]);
        assert_eq!(avant[Subsystem::Playlist as usize], apres[Subsystem::Playlist as usize]);
        assert_eq!(avant[Subsystem::Player as usize], apres[Subsystem::Player as usize], "le volume n'est pas du player");
    }

    #[tokio::test]
    async fn une_trame_qui_change_la_sourdine_reveille_mixer() {
        // `muted` compte autant que `volume` : les clients MPD coupent le son
        // en posant `setvol 0`, mais la sourdine peut aussi venir de la
        // telecommande, et le client doit l'apprendre.
        let e = SharedState::default();
        let avant = e.versions().await;
        e.apply_state(PlayerState { muted: true, ..Default::default() }).await;
        let apres = e.versions().await;
        assert_ne!(avant[Subsystem::Mixer as usize], apres[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn une_trame_identique_ne_reveille_personne() {
        // Le coeur deduplique deja, mais une reconnexion renvoie l'state
        // current : il ne doit pas passer pour un changement.
        let e = SharedState::default();
        let trame = PlayerState {
            volume: 40,
            source: "radio".into(),
            playback: Playback::Playing,
            preset: Some(3),
            preset_count: Some(51),
            position_s: Some(12),
            ..Default::default()
        };
        e.apply_state(trame.clone()).await;
        let avant = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_state(trame).await;

        assert_eq!(avant, e.versions().await);
        assert_eq!(queue_version, e.read().await.queue_version, "la file n'a pas bouge non plus");
    }

    #[tokio::test]
    async fn un_changement_de_source_reveille_playlist_et_player() {
        // La file d'attente EST la liste des preselections de la source
        // active : changer de source change la file, et change aussi ce qui
        // plays.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let avant = e.versions().await;

        e.apply_state(PlayerState { source: "cd".into(), ..Default::default() }).await;

        let apres = e.versions().await;
        assert_ne!(avant[Subsystem::Playlist as usize], apres[Subsystem::Playlist as usize]);
        assert_ne!(avant[Subsystem::Player as usize], apres[Subsystem::Player as usize]);
        assert_eq!(avant[Subsystem::Mixer as usize], apres[Subsystem::Mixer as usize], "le volume n'a pas bouge");
    }

    #[tokio::test]
    async fn un_disque_insere_change_la_file_dattente() {
        // `preset_count` est la longueur de la file MPD (`playlistlength`) :
        // un disque insert fait passer le player CD de « rien a numeroter » a
        // douze pistes, sans changer de name de source. Sans wakeup de
        // `Playlist` ni avance de `queue_version`, un client remainder sur une file
        // clear et l'action la plus ordinaire du monde ne se voit pas depuis le
        // telephone. Et `Player` ne doit pas bouger : la file a change, pas ce
        // qui plays.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "cd".into(), preset_count: Some(0), ..Default::default() })
            .await;
        let avant = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_state(PlayerState { source: "cd".into(), preset_count: Some(12), ..Default::default() })
            .await;

        let apres = e.versions().await;
        assert_ne!(avant[Subsystem::Playlist as usize], apres[Subsystem::Playlist as usize], "la file a change");
        assert!(
            e.read().await.queue_version > queue_version,
            "queue_version doit avancer avec la file, sinon plchanges mentira"
        );
        assert_eq!(avant[Subsystem::Player as usize], apres[Subsystem::Player as usize], "ce qui plays n'a pas change");
        assert_eq!(avant[Subsystem::Mixer as usize], apres[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn chaque_transition_de_preset_count_bouge_la_file() {
        // Les trois transitions que le type `Option<u8>` rend distinctes, a
        // source constante. Celle qui porte tout le poids de ce test est
        // **`None` -> `Some(0)`** : c'est la seule que perdrait une
        // comparaison ecrite sur `unwrap_or(0)`, et les deux valeurs ne
        // decrivent pas la meme file. `None` veut dire « la source n'a rien
        // declare » — le consommateur retombe sur la grille historique 1-9,
        // donc neuf entries ; `Some(0)` veut dire « rien a numeroter » — un
        // player CD sans disque, donc zero entree. Les confondre ferait
        // rater l'insertion d'un disque dans une source qui ne declarait rien
        // auparavant.
        //
        // Les deux autres lines couvrent les deux sens du mouvement, mais il
        // faut savoir ce qu'elles valent comme preuve : elles passeraient
        // aussi sous `unwrap_or(0)` (0 != 12, puis 12 != 0). Seule la premiere
        // separe les deux implementations.
        let transitions: [(&str, Option<u8>, Option<u8>); 3] = [
            ("rien declare -> zero piste", None, Some(0)),
            ("zero piste -> douze pistes", Some(0), Some(12)),
            ("douze pistes -> rien declare", Some(12), None),
        ];
        for (name, depart, arrivee) in transitions {
            let e = SharedState::default();
            e.apply_state(PlayerState { source: "cd".into(), preset_count: depart, ..Default::default() })
                .await;
            let avant = e.versions().await;
            let queue_version = e.read().await.queue_version;

            e.apply_state(PlayerState { source: "cd".into(), preset_count: arrivee, ..Default::default() })
                .await;

            let apres = e.versions().await;
            assert_ne!(
                avant[Subsystem::Playlist as usize],
                apres[Subsystem::Playlist as usize],
                "{name} : la file doit bouger"
            );
            assert!(
                e.read().await.queue_version > queue_version,
                "{name} : queue_version doit avancer avec la file"
            );
            assert_eq!(
                avant[Subsystem::Player as usize],
                apres[Subsystem::Player as usize],
                "{name} : ce qui plays n'a pas change"
            );
        }
    }

    #[tokio::test]
    async fn le_morceau_la_position_et_la_preselection_reveillent_player_seul() {
        // Les trois champs que le brief nomme sous `player`, chacun teste
        // separement : oublier l'un des trois laisserait un client muet
        // pendant tout un track.
        let base = PlayerState { source: "radio".into(), ..Default::default() };
        let variantes: [(&str, PlayerState); 3] = [
            ("playback", PlayerState { playback: Playback::Playing, ..base.clone() }),
            ("position", PlayerState { position_s: Some(7), ..base.clone() }),
            ("preselection", PlayerState { preset: Some(4), ..base.clone() }),
        ];
        for (name, trame) in variantes {
            let e = SharedState::default();
            e.apply_state(base.clone()).await;
            let avant = e.versions().await;

            e.apply_state(trame).await;

            let apres = e.versions().await;
            assert_ne!(avant[Subsystem::Player as usize], apres[Subsystem::Player as usize], "{name} devrait bouger player");
            assert_eq!(avant[Subsystem::Playlist as usize], apres[Subsystem::Playlist as usize], "{name} ne touche pas la file");
            assert_eq!(avant[Subsystem::Mixer as usize], apres[Subsystem::Mixer as usize], "{name} ne touche pas le mixer");
        }
    }

    #[tokio::test]
    async fn lhorloge_de_position_ne_reveille_personne() {
        // **La régression la plus coûteuse de ce greffon.** Le cœur push_cover une
        // trame par seconde en playback, et son seul champ qui bouge est alors
        // `position_s` : mark `Player` pour cela réveillait tous les
        // clients endormis sur `idle player` une fois par seconde. M.A.L.P.
        // redemandait `status`, `currentsong` et la **cover** au même
        // rythme, ce qui hachait le transfert par tranches de l'image — d'où
        // l'instabilité et la cover qui disparaît.
        let base = PlayerState {
            source: "files".into(),
            playback: Playback::Playing,
            position_s: Some(30),
            ..Default::default()
        };
        let e = SharedState::default();
        e.apply_state(base.clone()).await;
        let avant = e.versions().await;

        // Quatre seconds d'horloge, une par trame : rien ne doit bouger.
        for s in 31..=34 {
            e.apply_state(PlayerState { position_s: Some(s), ..base.clone() }).await;
        }

        assert_eq!(
            avant[Subsystem::Player as usize],
            e.versions().await[Subsystem::Player as usize],
            "le temps qui passe n'est pas un evenement MPD"
        );
    }

    #[tokio::test]
    async fn un_deplacement_reveille_player() {
        // Le pendant du test ci-dessus : la tolérance ne doit pas avaler un
        // vrai déplacement, sinon la barre de progress du client resterait
        // à l'ancienne position jusqu'à ce qu'il redemande `status` de
        // lui-même. Les deux sens comptent — un recul est toujours un
        // événement, une avance seulement au-delà de la tolérance.
        let base = PlayerState {
            source: "files".into(),
            playback: Playback::Playing,
            position_s: Some(30),
            ..Default::default()
        };
        for (name, position) in [("avance", 90u32), ("recul", 5)] {
            let e = SharedState::default();
            e.apply_state(base.clone()).await;
            let avant = e.versions().await;

            e.apply_state(PlayerState { position_s: Some(position), ..base.clone() }).await;

            assert_ne!(
                avant[Subsystem::Player as usize],
                e.versions().await[Subsystem::Player as usize],
                "{name} : un deplacement doit reveiller player"
            );
        }
    }

    #[tokio::test]
    async fn lapparition_et_la_disparition_de_la_position_reveillent_player() {
        // Une piste qui commence, un stream qui n'a plus de position : deux états
        // différents de la playback, pas l'horloge qui avance.
        let sans = PlayerState { source: "radio".into(), ..Default::default() };
        let avec = PlayerState { position_s: Some(1), ..sans.clone() };
        for (name, depart, arrivee) in
            [("apparition", sans.clone(), avec.clone()), ("disparition", avec, sans)]
        {
            let e = SharedState::default();
            e.apply_state(depart).await;
            let avant = e.versions().await;

            e.apply_state(arrivee).await;

            assert_ne!(
                avant[Subsystem::Player as usize],
                e.versions().await[Subsystem::Player as usize],
                "{name} : doit reveiller player"
            );
        }
    }

    #[tokio::test]
    async fn le_titre_du_morceau_reveille_player() {
        // Un stream radio ne change ni de source ni de preselection quand le
        // track change : c'est le seul signal que le client recevra.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let avant = e.versions().await;

        let mut trame = PlayerState { source: "radio".into(), ..Default::default() };
        trame.track.title = Some("Sonate".into());
        e.apply_state(trame).await;

        assert_ne!(avant[Subsystem::Player as usize], e.versions().await[Subsystem::Player as usize]);
    }

    #[tokio::test]
    async fn aucune_trame_ne_bouge_stored_playlist() {
        // Le seul declencheur de ce sujet est `apply_catalog`. Une trame
        // qui change tout le remainder ne doit pas l'incrementer au passage.
        let e = SharedState::default();
        let avant = e.versions().await;

        e.apply_state(PlayerState {
            volume: 30,
            muted: true,
            source: "cd".into(),
            playback: Playback::Playing,
            preset: Some(2),
            position_s: Some(3),
            ..Default::default()
        })
        .await;

        assert_eq!(
            avant[Subsystem::StoredPlaylist as usize],
            e.versions().await[Subsystem::StoredPlaylist as usize]
        );
    }

    #[tokio::test]
    async fn un_catalogue_neuf_reveille_stored_playlist() {
        let e = SharedState::default();
        let avant = e.versions().await;

        e.apply_catalog(catalogue_a_une_source()).await;

        let apres = e.versions().await;
        assert_ne!(avant[Subsystem::StoredPlaylist as usize], apres[Subsystem::StoredPlaylist as usize]);
        assert_eq!(
            e.read().await.sources_catalog,
            catalogue_a_une_source(),
            "le sources_catalog doit aussi etre memorise, pas seulement compte"
        );
    }

    #[tokio::test]
    async fn un_catalogue_identique_ne_reveille_personne() {
        // Le coeur envoie la valeur courante **a la connection** : une
        // reconnexion de la moitie `display` repasse ici avec le meme
        // sources_catalog, et ne doit pas passer pour un changement — sinon chaque
        // redemarrage du greffon reveille tous les clients.
        let e = SharedState::default();
        e.apply_catalog(catalogue_a_une_source()).await;
        let avant = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_catalog(catalogue_a_une_source()).await;

        assert_eq!(avant, e.versions().await);
        assert_eq!(queue_version, e.read().await.queue_version);
    }

    #[tokio::test]
    async fn un_catalogue_qui_touche_la_source_active_bouge_aussi_la_file() {
        // La file d'attente MPD *est* la liste des preselections de la source
        // active : renommer une station de la radio pendant qu'elle plays
        // change la file, donc `Playlist` et `queue_version` avec elle.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        e.apply_catalog(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let avant = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_catalog(catalogue_de(&[("radio", &[(1, "FIP Rock")]), ("cd", &[])])).await;

        let apres = e.versions().await;
        assert_ne!(avant[Subsystem::StoredPlaylist as usize], apres[Subsystem::StoredPlaylist as usize]);
        assert_ne!(avant[Subsystem::Playlist as usize], apres[Subsystem::Playlist as usize]);
        assert!(
            e.read().await.queue_version > queue_version,
            "queue_version doit avancer avec la file, sinon plchanges mentira"
        );
    }

    #[tokio::test]
    async fn un_catalogue_qui_ne_touche_quune_source_inactive_laisse_la_file_tranquille() {
        // Le pendant, et celui qui a une valeur : la radio se renomme une
        // station pendant que le cd plays. Les listes enregistrees ont change,
        // la file d'attente non — reveiller `Playlist` ferait retelecharger la
        // file a tous les clients, et `plchanges` rendrait une file identique
        // sous une version neuve.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "cd".into(), ..Default::default() }).await;
        e.apply_catalog(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let avant = e.versions().await;
        let queue_version = e.read().await.queue_version;

        e.apply_catalog(catalogue_de(&[("radio", &[(1, "FIP"), (5, "Nova")]), ("cd", &[])]))
            .await;

        let apres = e.versions().await;
        assert_ne!(
            avant[Subsystem::StoredPlaylist as usize],
            apres[Subsystem::StoredPlaylist as usize],
            "les listes enregistrees ont bel et bien change"
        );
        assert_eq!(
            avant[Subsystem::Playlist as usize],
            apres[Subsystem::Playlist as usize],
            "la file d'attente vient de la source active, et elle n'a pas bouge"
        );
        assert_eq!(queue_version, e.read().await.queue_version);
        assert_eq!(
            avant[Subsystem::Player as usize],
            apres[Subsystem::Player as usize],
            "un sources_catalog ne dit rien de ce qui plays"
        );
    }

    #[tokio::test]
    async fn le_catalogue_ne_voyage_pas_avec_chaque_trame_detat() {
        // Non-regression du choix des deux canaux : dix trames d'state, un seul
        // sources_catalog. Les trames sont toutes differentes (le volume monte),
        // donc chacune reveille bel et bien quelque chose — ce test ne peut
        // pas passer parce qu'il n'y aurait rien eu a appliquer.
        let e = SharedState::default();
        e.apply_catalog(catalogue_a_une_source()).await;
        let apres_catalogue = e.versions().await;
        for v in 1..=10u8 {
            e.apply_state(PlayerState { volume: v, ..Default::default() }).await;
        }
        let apres = e.versions().await;
        assert_eq!(apres[Subsystem::StoredPlaylist as usize], apres_catalogue[Subsystem::StoredPlaylist as usize]);
        assert_eq!(
            apres[Subsystem::Mixer as usize],
            apres_catalogue[Subsystem::Mixer as usize] + 10,
            "les dix trames doivent avoir compte pour dix, sinon ce test ne prouve rien"
        );
        assert_eq!(e.read().await.sources_catalog, catalogue_a_une_source(), "et le sources_catalog survit aux trames");
    }

    #[tokio::test]
    async fn catalogue_source_distingue_le_nom_inconnu_de_la_liste_vide() {
        // La distinction sur laquelle reposent `listplaylistinfo` et `load` :
        // un name absent du sources_catalog est un `ACK 50`, un name connu sans
        // preselection est une reponse clear et bien formee.
        let e = SharedState::default();
        e.apply_catalog(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;
        let inst = e.read().await;

        assert!(inst.source_catalog("nawak").is_none());
        assert_eq!(inst.source_catalog("cd").map(|s| s.presets.len()), Some(0));
        assert_eq!(inst.source_catalog("radio").map(|s| s.presets.len()), Some(1));
    }

    #[tokio::test]
    async fn les_presets_actifs_suivent_la_source_que_la_trame_designe() {
        // `active_presets` read le name de source de la derniere trame : c'est
        // elle et non le sources_catalog qui dit ce qui plays.
        let e = SharedState::default();
        e.apply_catalog(catalogue_de(&[("radio", &[(1, "FIP")]), ("cd", &[])])).await;

        e.apply_state(PlayerState { source: "cd".into(), ..Default::default() }).await;
        assert!(e.read().await.active_presets().is_empty());

        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        assert_eq!(e.read().await.active_presets().len(), 1);
    }

    #[tokio::test]
    async fn un_catalogue_reveille_un_dormeur_inscrit_sur_les_listes_enregistrees() {
        // La contrepartie utile : un client qui dort sur `stored_playlist`
        // doit repartir quand le sources_catalog arrive, et c'est le seul evenement
        // qui le reveillera jamais.
        let e = std::sync::Arc::new(SharedState::default());
        let seen = e.versions().await;
        let dormeur = {
            let e = e.clone();
            tokio::spawn(async move { e.wait(&[Subsystem::StoredPlaylist], seen).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        e.apply_catalog(catalogue_a_une_source()).await;

        assert_eq!(dormeur.await.unwrap().moved, vec![Subsystem::StoredPlaylist]);
    }

    #[tokio::test]
    async fn la_version_de_file_est_monotone() {
        // Jamais pushback a zero : un client qui compare croirait n'avoir rien
        // manque. Le troisieme tour revient a "radio", la valeur initiale, et
        // c'est justement le cas qu'une implementation derivee de l'state (et
        // non d'un compteur) raterait.
        let e = SharedState::default();
        let mut precedente = e.read().await.queue_version;
        for source in ["radio", "cd", "radio"] {
            e.apply_state(PlayerState { source: source.into(), ..Default::default() }).await;
            let v = e.read().await.queue_version;
            assert!(v > precedente, "{v} devrait depasser {precedente}");
            precedente = v;
        }
    }

    #[tokio::test]
    async fn la_version_de_file_ne_bouge_que_quand_la_file_bouge() {
        // Le pendant du test precedent : monotone ne veut pas dire "qui monte
        // a chaque trame". Un `plchanges` rendrait sinon toute la file a
        // chaque seconde de playback.
        let e = SharedState::default();
        e.apply_state(PlayerState { source: "radio".into(), ..Default::default() }).await;
        let avant = e.read().await.queue_version;

        e.apply_state(PlayerState { source: "radio".into(), volume: 50, position_s: Some(9), ..Default::default() })
            .await;

        assert_eq!(avant, e.read().await.queue_version);
    }

    #[tokio::test]
    async fn un_changement_survenu_avant_lattente_ne_se_perd_pas() {
        // LE test qui compte : la session read les versions, un changement
        // arrive, *ensuite* elle s'endort. Elle doit repartir aussitot. Avec
        // un `Notify` seul, ce wakeup serait perdu et le client resterait muet
        // jusqu'au changement suivant.
        let e = SharedState::default();
        let seen = e.versions().await;
        e.apply_state(PlayerState { volume: 40, ..Default::default() }).await;
        // Pas de `timeout` ici : si l'attente bloque, le test pend et l'echec
        // est franc. Une marge d'horloge serait un flake en puissance.
        let changes = e.wait(&[Subsystem::Mixer], seen).await;
        assert_eq!(changes.moved, vec![Subsystem::Mixer]);
        // Et le réveil rend les compteurs qui l'ont décidé : c'est ce que la
        // session retiendra comme nouvelle référence de sa connection.
        assert_eq!(changes.versions, e.versions().await);
    }

    #[tokio::test]
    async fn lattente_ne_rend_que_les_sujets_demandes() {
        let e = SharedState::default();
        let seen = e.versions().await;
        e.apply_state(PlayerState { volume: 40, source: "cd".into(), ..Default::default() }).await;
        let changes = e.wait(&[Subsystem::Mixer], seen).await;
        assert_eq!(changes.moved, vec![Subsystem::Mixer], "playlist a change mais n'etait pas demande");
    }

    #[tokio::test]
    async fn lattente_rend_les_sujets_dans_lordre_demande() {
        // L'order est celui de la demande et non celui de l'enum : c'est ce
        // que la session ecrira en lines `changed:`, et un order stable est
        // ce qui rend cette sortie testable a la Task 8.
        let e = SharedState::default();
        let seen = e.versions().await;
        e.apply_state(PlayerState { volume: 40, source: "cd".into(), ..Default::default() }).await;

        let changes = e.wait(&[Subsystem::Playlist, Subsystem::Mixer, Subsystem::Player], seen).await;

        assert_eq!(changes.moved, vec![Subsystem::Playlist, Subsystem::Mixer, Subsystem::Player]);
    }

    #[tokio::test]
    async fn une_trame_arrivee_pendant_lattente_reveille_le_dormeur() {
        // L'autre moitie du dispositif : quand la comparaison prealable ne
        // trouve rien, c'est le `Notify` qui doit rendre la main. Le dormeur
        // est lance dans une tache et les `yield_now` lui laissent atteindre
        // son point d'attente (ordonnanceur mono-tache de `#[tokio::test]`,
        // donc la tache en file passe avant celle qui cede).
        //
        // Aucune horloge : si la notification n'arrive pas, le `await` du
        // handle pend et l'echec est franc. Un `timeout` "assez long" serait
        // un flake en puissance — c'est exactement la famille de tests que le
        // chantier precedent a du supprimer.
        let e = std::sync::Arc::new(SharedState::default());
        let seen = e.versions().await;
        let dormeur = {
            let e = e.clone();
            tokio::spawn(async move { e.wait(&[Subsystem::Player], seen).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;

        assert_eq!(dormeur.await.unwrap().moved, vec![Subsystem::Player]);
    }

    #[tokio::test]
    async fn un_dormeur_ne_repart_pas_sur_un_sujet_qui_nest_pas_le_sien() {
        // `notify_waiters` reveille tout le monde, donc un dormeur inscrit sur
        // `Mixer` seul est bel et bien reveille par une trame `player` — et
        // doit se rendormir. Sans la boucle de `wait`, il rendrait une
        // liste clear et la session ecrirait un `OK` sans `changed:`, ce
        // qu'aucun client MPD ne sait interpreter.
        let e = std::sync::Arc::new(SharedState::default());
        let seen = e.versions().await;
        let dormeur = {
            let e = e.clone();
            tokio::spawn(async move { e.wait(&[Subsystem::Mixer], seen).await })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        // Ne bouge que `player` : le dormeur est reveille pour rien.
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(!dormeur.is_finished(), "un wakeup sur un autre sujet ne doit pas terminer l'attente");

        // Puis ce qu'il attendait vraiment.
        e.apply_state(PlayerState { playback: Playback::Playing, volume: 22, ..Default::default() }).await;
        assert_eq!(dormeur.await.unwrap().moved, vec![Subsystem::Mixer]);
    }

    #[tokio::test]
    async fn letat_optimiste_devance_la_trame_puis_lui_cede() {
        // La course de `pause` : le greffon acte la bascule des qu'il l'emet,
        // et la trame suivante fait autorite.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        e.acknowledge_optimistic(&[Command::PlayPause]).await;
        assert_eq!(e.read().await.playback(), Playback::Paused, "acte avant la trame");
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        assert_eq!(e.read().await.playback(), Playback::Playing, "la trame fait autorite");
    }

    #[tokio::test]
    async fn la_bascule_optimiste_repart_de_la_valeur_optimiste() {
        // Deux `pause` d'affilee reviennent a l'state de depart : la bascule
        // read `optimistic_playback` et non la trame, sinon la seconde
        // rebasculerait depuis `Playing` et rendrait encore `Paused`.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;

        e.acknowledge_optimistic(&[Command::PlayPause]).await;
        e.acknowledge_optimistic(&[Command::PlayPause]).await;

        assert_eq!(e.read().await.playback(), Playback::Playing);
    }

    #[tokio::test]
    async fn la_bascule_optimiste_est_sans_effet_a_larret() {
        // `PlayPause` a l'arret demarre une playback dont le greffon ne sait
        // ni quoi ni ou : il attend la trame plutot que d'annoncer `Playing`
        // sur un track clear.
        let e = SharedState::default();
        let avant = e.versions().await;

        e.acknowledge_optimistic(&[Command::PlayPause]).await;

        assert_eq!(e.read().await.playback(), Playback::Stopped);
        assert_eq!(avant, e.versions().await, "rien a annoncer, donc aucun wakeup");
    }

    #[tokio::test]
    async fn acter_un_volume_le_publie_aussitot_et_reveille_mixer() {
        // Un client qui envoie `setvol 70` puis `status` dans la meme foulee
        // doit read 70, et les autres clients doivent etre reveilles : la
        // trame confirmante, elle, sera identique et ne bougera rien.
        let e = SharedState::default();
        let avant = e.versions().await;

        e.acknowledge_optimistic(&[Command::SetVolume(70)]).await;

        assert_eq!(e.read().await.state.volume, 70);
        assert_ne!(avant[Subsystem::Mixer as usize], e.versions().await[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn acter_le_volume_deja_en_place_ne_reveille_personne() {
        let e = SharedState::default();
        e.apply_state(PlayerState { volume: 70, ..Default::default() }).await;
        let avant = e.versions().await;

        e.acknowledge_optimistic(&[Command::SetVolume(70)]).await;

        assert_eq!(avant, e.versions().await);
    }

    #[tokio::test]
    async fn acter_ignore_les_commandes_dont_leffet_ne_se_devine_pas() {
        // Deviner ce qu'un `Select` fait a la position, au track ou a la
        // preselection serait faux plus souvent que juste : c'est la source
        // active qui decide.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, volume: 30, ..Default::default() }).await;
        let avant_instantane = e.read().await;

        // `Mute` n'est **plus** de la liste : elle s'acte depuis que `setvol`
        // démute (voir `commands::setvol`), et son test propre est juste en
        // dessous. `VolumeUp`/`VolumeDown` y restent : elles ne portent pas de
        // valeur et c'est le cœur qui décide du pas.
        e.acknowledge_optimistic(&[
            Command::Select(4),
            Command::Next,
            Command::Prev,
            Command::Stop,
            Command::SeekTo(30),
            Command::VolumeUp,
            Command::SourceCycle,
        ])
        .await;

        assert_eq!(avant_instantane, e.read().await, "aucune de ces commands ne s'acte");
    }

    #[tokio::test]
    async fn une_liste_de_deux_bascules_ne_compte_quun_changement() {
        // Le dedoublonnage de `mark` : deux `pause` dans une meme liste de
        // commands MPD passent sous le verrou une seule fois, et un seul
        // changement est publie — l'state final, lui, est bien celui des deux
        // bascules.
        let e = SharedState::default();
        e.apply_state(PlayerState { playback: Playback::Playing, ..Default::default() }).await;
        let avant = e.versions().await;

        e.acknowledge_optimistic(&[Command::PlayPause, Command::PlayPause]).await;

        assert_eq!(
            avant[Subsystem::Player as usize] + 1,
            e.versions().await[Subsystem::Player as usize],
            "un seul incrément pour une seule prise de verrou"
        );
        assert_eq!(e.read().await.playback(), Playback::Playing);
    }

    #[tokio::test]
    async fn acter_mute_bascule_la_sourdine_et_reveille_mixer() {
        // Sans cet actage, le `setvol` qui démute serait invisible : `status`
        // publie `volume: 0` tant que `state.muted` est vrai, donc un client qui
        // remonte son curseur le verrait retomber à zéro jusqu'à la trame
        // suivante — le défaut exact que le calque optimiste existe pour
        // éviter.
        let e = SharedState::default();
        e.apply_state(PlayerState { volume: 40, muted: true, ..Default::default() }).await;
        let avant = e.versions().await;

        e.acknowledge_optimistic(&[Command::SetVolume(40), Command::Mute]).await;

        let inst = e.read().await;
        assert!(!inst.state.muted, "la sourdine doit etre levee");
        assert_eq!(inst.state.volume, 40);
        assert_ne!(avant[Subsystem::Mixer as usize], e.versions().await[Subsystem::Mixer as usize]);
    }

    #[tokio::test]
    async fn acter_mute_est_bien_une_bascule_dans_les_deux_sens() {
        // Une bascule et non une pose : l'acter comme « muet = vrai » ferait
        // qu'un `Mute` émis depuis un appareil déjà muet publierait une
        // sourdine que le cœur vient au contraire de lever.
        let e = SharedState::default();
        e.acknowledge_optimistic(&[Command::Mute]).await;
        assert!(e.read().await.state.muted, "faux -> vrai");
        e.acknowledge_optimistic(&[Command::Mute]).await;
        assert!(!e.read().await.state.muted, "vrai -> faux");
    }

    #[test]
    fn les_sujets_indexent_le_tableau_sans_trou() {
        // La conception repose sur `sujet as usize` : si un jour une variante
        // recevait une valeur hors bounds ou en double, l'indexation
        // paniquerait ou deux subsystems partageraient un compteur.
        let indices = [
            Subsystem::Player as usize,
            Subsystem::Mixer as usize,
            Subsystem::Playlist as usize,
            Subsystem::StoredPlaylist as usize,
        ];
        let mut vus = [false; SUBSYSTEM_COUNT];
        for i in indices {
            assert!(i < SUBSYSTEM_COUNT, "{i} sort du tableau de compteurs");
            assert!(!vus[i], "deux subsystems partagent l'index {i}");
            vus[i] = true;
        }
        assert!(vus.iter().all(|v| *v), "un index du tableau n'a pas de sujet");
    }

    // ------------------------------------------------------------------
    // Les pochettes
    // ------------------------------------------------------------------

    /// Le `href` que le cœur publie, dans les deux endroits qui doivent
    /// coïncider : la trame d'état et la trame de cover.
    const HREF: &str = "/api/cover/1a2b3c";

    #[tokio::test]
    async fn une_pochette_recue_est_tenue_et_reveille_player() {
        let e = SharedState::default();
        let avant = e.versions().await;

        e.apply_cover(test_cover(HREF, 4096)).await;

        let inst = e.read().await;
        let tenue = inst.cover.expect("la cover doit etre tenue");
        assert_eq!(tenue.href, HREF);
        assert_eq!(tenue.mime, "image/jpeg");
        // Les bytes au bit près : c'est ce que `albumart` servira.
        assert_eq!(*tenue.bytes, test_cover(HREF, 4096).bytes);
        assert_ne!(
            avant[Subsystem::Player as usize],
            e.versions().await[Subsystem::Player as usize],
            "une cover est un fait sur le track current"
        );
    }

    #[tokio::test]
    async fn une_pochette_ne_reveille_que_player() {
        // Le pendant du test précédent : `Mixer` n'a rien à voir avec une
        // image, et réveiller `Playlist` ferait retélécharger la file entière
        // à tous les clients à chaque changement de piste. `StoredPlaylist`
        // est réservé aux listes enregistrées.
        let e = SharedState::default();
        let avant = e.versions().await;
        let file_avant = e.read().await.queue_version;

        e.apply_cover(test_cover(HREF, 4096)).await;

        let apres = e.versions().await;
        for sujet in [Subsystem::Mixer, Subsystem::Playlist, Subsystem::StoredPlaylist] {
            assert_eq!(
                avant[sujet as usize], apres[sujet as usize],
                "{sujet:?} n'a rien a apprendre d'une cover"
            );
        }
        // Et la version de file d'attente non plus : la file n'a pas changé,
        // et l'incrémenter ferait répondre `plchanges` pour rien.
        assert_eq!(file_avant, e.read().await.queue_version);
    }

    #[tokio::test]
    async fn la_meme_pochette_deux_fois_ne_reveille_personne() {
        // Le cœur push_cover la cover courante **au câblage**, donc une
        // reconnexion de la moitié `display` repasse ici avec la même image.
        // Sans la comparaison, chaque redémarrage du greffon réveillerait tous
        // les clients — et leur ferait retélécharger jusqu'à vingt mébioctets.
        let e = SharedState::default();
        e.apply_cover(test_cover(HREF, 4096)).await;
        let avant = e.versions().await;

        e.apply_cover(test_cover(HREF, 4096)).await;

        assert_eq!(avant, e.versions().await);
    }

    #[tokio::test]
    async fn des_octets_differents_sous_le_meme_href_sont_un_changement() {
        // La comparaison porte sur les bytes et pas seulement sur le `href` :
        // se fier à la seule clé ferait taire une image réellement nouvelle
        // publiée sous une clé recyclée, et le client garderait l'ancienne
        // pour toujours.
        let e = SharedState::default();
        e.apply_cover(test_cover(HREF, 4096)).await;
        let avant = e.versions().await;

        e.apply_cover(test_cover(HREF, 8192)).await;

        assert_ne!(avant[Subsystem::Player as usize], e.versions().await[Subsystem::Player as usize]);
        assert_eq!(e.read().await.cover.unwrap().bytes.len(), 8192);
    }

    /// Une trame d'état **telle que le cœur l'émet quand une cover existe** :
    /// elle announcement le `href` de l'image tenue.
    ///
    /// Le réalisme n'est pas une politesse. Ce test employait une trame
    /// `Default` — donc sans `cover_href` — pour prouver qu'une trame d'état ne
    /// jette pas la cover : une trame que le producteur n'émet **jamais** en
    /// même temps qu'une cover, et qui prouvait donc une causalité
    /// impossible. Elle masquait au passage que rien ne relâchait jamais
    /// l'image.
    fn trame_qui_annonce(href: &str) -> PlayerState {
        PlayerState {
            source: "radio".into(),
            preset: Some(2),
            track: ritornello_proto::Track {
                title: Some("So What".into()),
                cover_href: Some(href.to_string()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn une_trame_detat_ne_jette_pas_la_pochette_quelle_annonce() {
        // Les deux canaux écrivent dans le même instantané et chacun ne doit
        // toucher que le sien — la même propriété que pour le sources_catalog. Une
        // trame d'état arrive **chaque seconde** de playback : si elle remettait
        // la cover à `None`, `albumart` ne répondrait qu'entre deux trames,
        // c'est-à-dire jamais.
        let e = SharedState::default();
        e.apply_cover(test_cover(HREF, 4096)).await;

        e.apply_state(PlayerState { volume: 17, ..trame_qui_annonce(HREF) }).await;

        let inst = e.read().await;
        assert_eq!(inst.cover.map(|p| p.bytes.len()), Some(4096));
        assert_eq!(inst.state.volume, 17);
    }

    #[tokio::test]
    async fn une_trame_sans_pochette_relache_les_octets_tenus() {
        // Le pendant, et il manquait entièrement : `cover` n'était jamais
        // remis à `None`, donc le greffon retenait jusqu'à 20 Mio pour la vie du
        // processus — y compris longtemps après l'arrêt de la playback. Le
        // signal est le `cover_href` de la trame d'état : `None` veut dire que
        // plus rien de ce qui plays n'a d'image, et c'est exactement la condition
        // sous laquelle `albumart` refusait déjà de serve ces bytes.
        let e = SharedState::default();
        e.apply_state(trame_qui_annonce(HREF)).await;
        e.apply_cover(test_cover(HREF, 4096)).await;
        assert!(e.read().await.cover.is_some(), "la cover doit d'abord etre tenue");

        // La piste suivante n'a pas d'illustration : le cœur l'announcement ainsi, et
        // n'enverra aucune trame de cover pour elle.
        e.apply_state(PlayerState {
            track: ritornello_proto::Track { title: Some("Blue in Green".into()), ..Default::default() },
            ..trame_qui_annonce(HREF)
        })
        .await;

        assert!(e.read().await.cover.is_none(), "les bytes doivent etre relaches");
    }

    #[tokio::test]
    async fn une_trame_qui_annonce_une_autre_cle_garde_la_pochette_tenue() {
        // La fenêtre normale du cœur : il envoie l'état **avant** les bytes,
        // donc la trame announcement déjà la clé suivante quand la cover tenue est
        // encore la précédente. Le relâchement ne doit pas s'y déclencher —
        // sinon une inversion d'order des deux canaux détruirait une image que
        // la trame d'après aurait légitimée. `albumart` refuse pendant cette
        // fenêtre (le `href` ne correspond pas), et c'est tout ce qu'il faut.
        let e = SharedState::default();
        e.apply_state(trame_qui_annonce(HREF)).await;
        e.apply_cover(test_cover(HREF, 4096)).await;

        e.apply_state(trame_qui_annonce("/api/cover/999999")).await;

        assert!(e.read().await.cover.is_some(), "la fenetre state/cover n'est pas un relachement");
    }

    #[tokio::test]
    async fn un_dormeur_sur_player_est_reveille_par_une_pochette() {
        // Le bout en bout du réveil, dans ce module : `wait` ne sonde pas,
        // donc c'est bien le `notify_waiters` d'`apply_cover` qui rend
        // la main. Sans horloge : si l'implémentation ne réveillait pas, ce
        // test **pendrait** — le mode d'échec voulu.
        let e = Arc::new(SharedState::default());
        let seen = e.versions().await;
        let dormeur = e.clone();
        let attente = tokio::spawn(async move { dormeur.wait(&[Subsystem::Player], seen).await });
        // La comparaison préalable d'`wait` interdit le réveil manqué : que
        // la cover arrive avant ou après l'inscription du dormeur, il
        // repart. Aucune synchronisation n'est donc nécessaire ici.
        e.apply_cover(test_cover(HREF, 4096)).await;
        assert_eq!(attente.await.unwrap().moved, vec![Subsystem::Player]);
    }
}
