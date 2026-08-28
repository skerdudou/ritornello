//! La cover de ce qui plays : la chercher, la retenir, la serve.
//!
//! C'est **l'appareil** qui va chercher l'image, jamais le navigateur. Trois
//! raisons : la page ne doit charger aucune ressource externe — principe déjà
//! posé pour les pages d'admin ; l'image devient disponible à un futur
//! afficheur graphique ; et une cover embarquée dans un fichier, que seul
//! l'appareil peut read, n'aurait aucune URL à donner au navigateur.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ritornello_proto::CoverRef;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::RwLock;
use tokio_util::io::ReaderStream;

/// Plafond d'une image venue du réseau. Écarte le `front` nu du Cover Art
/// Archive, mesuré à 2 670 705 bytes là où `front-500` en rend 75 249.
const NETWORK_CAP: usize = 2 * 1024 * 1024;

/// Préfixe de l'URL locale publiée dans `Track::cover_href`.
///
/// Partagé entre `metadata::Metadata::state`, qui la **fabrique**, et
/// `main::display_relay`, qui la **relit** pour retrouver la clé du cache :
/// deux littéraux auraient pu diverger en silence, et la conséquence aurait été
/// un afficheur qui ne reçoit plus jamais de cover, sans erreur nulle part.
pub const HREF_PREFIX: &str = "/api/cover/";

/// Préfixe des fichiers temporaires d'extraction de cover embarquée,
/// posés dans `std::env::temp_dir()` par `player::mpv::embedded_cover`.
///
/// Partagé entre ce module (purge au démarrage, éviction bornée) et `mpv.rs`
/// (nommage) : les deux doivent reconnaître exactement les mêmes fichiers,
/// sous peine soit de ne jamais les purger, soit — pire — de purger un
/// fichier qui n'est pas de nous.
pub const TEMP_PREFIX: &str = "ritornello-cover-";

/// Vrai si `path` est un fichier temporaire d'extraction créé par ce
/// processus.
///
/// **Jamais** vrai pour un `folder.jpg` déclaré par une Source : celui-là vit
/// sur le partage de l'utilisateur, et le cœur ne doit jamais le supprimer de
/// son propre chef. `CoverPayload::File` porte les deux formes (voir sa doc),
/// c'est ici que la distinction se fait avant d'agir sur le disque.
fn is_cover_temp(path: &std::path::Path) -> bool {
    path.parent() == Some(std::env::temp_dir().as_path())
        && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(TEMP_PREFIX))
}

/// Balaie les fichiers temporaires d'une exécution précédente. Appelée une
/// fois au démarrage, avant que quoi que ce soit ne puisse en créer de
/// nouveaux.
///
/// **Deux raisons, dont une de correction.** Depuis que `embedded_cover`
/// nomme ses fichiers d'après leur contenu et n'écrit que si le name est libre,
/// un fichier laissé par une exécution **tuée en pleine écriture** serait
/// tronqué tout en portant le name d'une image complète : l'écriture
/// conditionnelle l'adopterait, et un afficheur recevrait une image coupée.
/// Ce balayage est ce qui rend ce cas impossible, et c'est pourquoi il doit
/// tourner **avant** que quoi que ce soit ne puisse créer un temporaire.
///
/// La seconde raison est l'accumulation, et elle vaut d'être dite parce
/// qu'on pourrait croire le système s'en charger : rien d'autre n'efface ces
/// fichiers entre deux démarrages, et un `systemctl restart` ne clear **pas**
/// `std::env::temp_dir()` — sur un Pi c'est souvent une `tmpfs`, que seul un
/// vrai redémarrage remet à zéro, et ce qui s'y entasse grignote de la RAM,
/// pas seulement du disque. Compter sur `/tmp` aurait donc laissé fuir
/// exactement le cas le plus fréquent, le redémarrage de service.
///
/// Sans risque de purger quelque chose d'utile : le cache ne survit jamais à
/// un redémarrage (`CoverCache` est reconstruit à chaque lancement), donc
/// rien de ce qui traîne encore ici ne peut être référencé par quoi que ce
/// soit.
pub fn purge_temp_files() {
    purge_temp_files_in(&std::env::temp_dir());
}

/// Cœur testable de `purge_temp_files`, paramétré par le répertoire à
/// balayer.
///
/// `std::env::temp_dir()` est **partagé** par tout le système, et par les
/// autres tests de ce même binaire, qui y écrivent de vrais fichiers
/// `ritornello-cover-*` pour éprouver l'extraction elle-même (voir
/// `player::mpv::tests`) : y lancer un vrai balayage depuis un test le
/// mettrait en concurrence avec eux. Séparée pour qu'un test puisse pointer
/// vers un répertoire à lui, entièrement isolé.
fn purge_temp_files_in(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entree in entries.flatten() {
        let name = entree.file_name();
        if name.to_str().is_some_and(|n| n.starts_with(TEMP_PREFIX)) {
            if let Err(e) = std::fs::remove_file(entree.path()) {
                tracing::debug!("purging leftover cover file {}: {e}", entree.path().display());
            }
        }
    }
}

/// Une trame de cover en cours de construction, partagée entre l'appelant
/// qui la construit et ceux qui l'attendent.
///
/// L'`Option` extérieure est celle de `line` — « rien à pousser », pour les
/// mêmes raisons que partout dans ce module ; l'`Arc<str>` intérieur est la
/// line de texte déjà sérialisée. La cellule est derrière un `Arc` pour que les
/// attendants la tiennent après avoir rendition le verrou de la table.
type FrameInFlight = Arc<tokio::sync::OnceCell<Option<Arc<str>>>>;

/// Ce que le cœur retient d'une cover.
///
/// Deux natures, et c'est délibéré : une cover **locale** n'entre pas en
/// mémoire. Un `folder.jpg` de trois mégaoctets est banal sur un NAS, et le
/// charger en RAM sur un Pi pour une image que le navigateur cachera de son
/// côté serait du gaspillage.
#[derive(Debug, Clone)]
pub enum CoverPayload {
    /// Venue du réseau : les bytes sont en mémoire.
    Bytes(Vec<u8>, &'static str),
    /// Locale : seul le path est retenu, la route relit le fichier.
    File(PathBuf),
}

/// Empreinte de la source, publiée dans l'URL locale.
///
/// `DefaultHasher` et non `sha2` : une collision ferait afficher la mauvaise
/// cover et rien d'autre, ce qui ne justifie pas une dépendance
/// cryptographique. Calculable **avant** le téléchargement, ce qui permet de
/// dédupliquer deux demandes pour la même image.
pub fn key(r: &CoverRef) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match r {
        CoverRef::Url { url } => {
            0u8.hash(&mut h);
            url.hash(&mut h);
        }
        CoverRef::Path { path } => {
            1u8.hash(&mut h);
            path.hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

/// Empreinte du **contenu** d'une image, pour nommer un fichier temporaire.
///
/// Même hacheur que `key`, et le même arbitrage : une collision afficherait la
/// mauvaise cover et rien d'autre. Ce qui change est ce qu'on hache — les
/// bytes de l'image, pas le path d'où ils sortent. Deux pistes d'un même
/// album portant la même cover embarquée retombent donc sur un seul
/// fichier, donc un seul `href`, donc rien à repousser ni à redécoder : le cas
/// embarqué rejoint ainsi le `folder.jpg` local, déjà gratuit. Sans cela, un
/// album de quinze pistes faisait tourner à clear un cache qui n'en tient
/// que le réglage en autorise (`CoverSettings::entries`), extraction, écriture
/// et éviction comprises.
pub fn content_key(bytes: &[u8]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Ce que le cœur fabrique d'une cover avant de la pousser sur un socket.
///
/// Absent (`CoverSettings::rendition` à `None`) quand l'utilisateur a décoché le
/// réencodage : les bytes d'origine partent tels quels. Un `Option` plutôt
/// qu'un booléen à l'intérieur, et ce n'est pas cosmétique — les quatre
/// réglages n'existent que là où ils veulent dire quelque chose, si bien qu'un
/// code qui read `max_edge_px` ne peut pas oublier de vérifier d'abord que le
/// rendition est active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rendition {
    /// Côté le plus long de la thumbnail, en pixels. Le rapport est conservé.
    pub max_edge_px: u32,
    /// Qualité JPEG, 1 à 100. Ignorée pour une image à canal alpha, réencodée
    /// en PNG sans perte.
    pub jpeg_quality: u8,
    /// Plafond de la thumbnail produite, en bytes. Un filet : au-delà, rien
    /// n'est poussé.
    pub output_cap: usize,
    /// Plafond de pixels à décoder. Comparé aux dimensions lues dans l'en-tête
    /// **avant toute allocation**, et reporté dans `image::Limits` pour le cas
    /// d'un en-tête qui mentirait sur ses propres dimensions.
    pub pixel_cap: u64,
}

/// Les deux étages du traitement d'une cover, qu'il ne faut pas confondre.
///
/// `source_max` bounded ce que le cœur accepte de **read**, quoi qu'il arrive
/// ensuite : c'est la seule garde qui subsiste quand le rendition est désactivé, et
/// la plus économique de toutes, puisqu'elle se juge sur la size du fichier
/// sans read un octet de son contenu.
///
/// `rendition` ne décrit que ce que le cœur **fabrique**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverSettings {
    /// Combien de pochettes le cache garde. Voir
    /// `state::Settings::cover_cache_entries`.
    pub entries: usize,
    /// Plafond de la cover source, en bytes.
    pub source_max: usize,
    /// `None` = pousser la source telle quelle.
    pub rendition: Option<Rendition>,
}

impl Default for CoverSettings {
    /// Les défauts du produit, pas des défauts neutres : un `CoverCache::new()`
    /// se comporte comme un appareil sorti d'usine, y compris dans les tests
    /// qui ne parlent pas de réglages. Dérivés de `state::Settings::default()`
    /// pour qu'il n'existe qu'un seul endroit où ces valeurs sont écrites.
    fn default() -> Self {
        Self::from(&crate::state::Settings::default())
    }
}

impl From<&crate::state::Settings> for CoverSettings {
    fn from(s: &crate::state::Settings) -> Self {
        Self {
            entries: s.cover_cache_entries as usize,
            source_max: (s.cover_source_max_mio as usize) * 1024 * 1024,
            rendition: s.cover_rendition.then(|| Rendition {
                max_edge_px: s.cover_max_edge_px,
                jpeg_quality: s.cover_jpeg_quality,
                output_cap: (s.cover_max_bytes_ko as usize) * 1024,
                pixel_cap: (s.cover_max_pixels_mpx as u64) * 1_000_000,
            }),
        }
    }
}

#[derive(Default)]
pub struct CoverCache {
    entries: RwLock<VecDeque<(String, CoverPayload)>>,
    /// Réglages vivants, relus à chaque publication.
    ///
    /// Un verrou `std::sync` et non celui de `tokio`, à la différence de
    /// `entries` juste au-dessus : la section critique est la copie d'une
    /// structure `Copy` de trente bytes, jamais une IO. Cela garde
    /// `Core::set_settings` synchrone — le rendre `async` pour ce champ aurait
    /// contaminé sa signature et tous ses appelants de test. La valeur est
    /// **copiée hors du verrou** avant tout `await` : aucun garde ne traverse
    /// un point de suspension.
    settings: std::sync::RwLock<CoverSettings>,
    /// Les builds de trame en cours, une entrée par clé.
    ///
    /// **Un rendez-vous, pas un cache — la distinction est tout.** Mémoriser une
    /// trame serait faux pour la raison que dit la doc de `rendition` : la clé hache
    /// le *path*, pas le contenu, donc une thumbnail gardée deviendrait fausse
    /// dès que l'utilisateur remplace l'image sous ce path. Une entrée d'ici ne
    /// survit pas à sa construction : le dernier appelant à en sortir la retire,
    /// et l'appelant suivant repart d'une playback neuve du fichier.
    ///
    /// Ce que cela économise : deux afficheurs abonnés qui reçoivent la même
    /// trame d'état demandent la même cover dans le même instant, et
    /// décodaient puis réencodaient deux fois la même image. Sur un Pi 2, c'est
    /// un cœur occupé plusieurs centaines de millisecondes en double.
    ///
    /// `tokio::sync::OnceCell::get_or_init` **est** le rendez-vous : le premier
    /// arrivé exécute, les suivants attendent son résultat. La cellule est
    /// derrière un `Arc` pour que les suiveurs la tiennent après avoir rendition le
    /// verrou de la table — le verrou ne couvre jamais le travail, seulement
    /// l'inscription.
    in_flight: tokio::sync::Mutex<HashMap<String, FrameInFlight>>,
    /// Combien de builds de trame ont **réellement** été exécutées.
    ///
    /// Sous `cfg(test)`, et c'est le bon compromis. Le rendez-vous ne peut se
    /// prouver que par un décompte d'exécutions : `Arc::ptr_eq` sur les trames
    /// rendues montrerait qu'un `Arc` est partagé, ce qui est déjà vrai sans
    /// aucun rendez-vous — chaque appelant reçoit son propre `Arc` sur sa propre
    /// chaîne, et rien dans l'égalité des contenus ne dit combien de fois
    /// l'image a été décodée. Or c'est *cela* qu'on économise.
    ///
    /// Rien en service n'a besoin de ce nombre, donc il n'entre pas dans le
    /// binaire livré : sur un Pi 2, un compteur atomique de plus n'est pas un
    /// coût, mais un champ que personne ne read est une dette.
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
    /// Les thumbnails déjà fabriquées pour la route HTTP, **clé du cache et
    /// ETag réunis**.
    ///
    /// Un cache, cette fois, et non un rendez-vous comme `in_flight` — la
    /// différence tient entièrement à ce qui sert de clé. `line` ne pouvait
    /// rien mémoriser parce que sa clé hache le *path* : l'utilisateur
    /// remplace le `folder.jpg` sous ce path et rien n'invalide l'entrée. Ici
    /// la clé porte en plus l'ETag, c'est-à-dire la date de modification et la
    /// size du fichier (voir `file_etag`) — remplacer le fichier change
    /// donc la clé, et l'ancienne thumbnail n'est plus jamais servie. Elle
    /// s'évince ensuite d'elle-même, comme le reste.
    ///
    /// Sans cela, chaque chargement de la page d'accueil redécoderait et
    /// réencoderait l'image sur un Pi 2, alors que la thumbnail est justement ce
    /// qu'on fabrique pour que le navigateur *n'ait pas* à télécharger trois
    /// mégaoctets. Le navigateur revalide (`no-cache`), donc le cas courant est
    /// un 304 sans rien fabriquer ; ce cache couvre le premier chargement de
    /// chaque nouveau navigateur, et les onglets multiples d'un même appareil.
    thumbnails: RwLock<VecDeque<Thumbnail>>,
    /// Combien de thumbnails ont **réellement** été décodées et réencodées.
    ///
    /// Sous `cfg(test)`, le même arbitrage que `builds` juste au-dessus
    /// et pour la même raison : la seule preuve qu'un cache économise du
    /// travail est un décompte d'exécutions. Comparer deux réponses ne dit
    /// rien — deux fabrications successives rendent les mêmes bytes.
    #[cfg(test)]
    thumbnails_built: std::sync::atomic::AtomicUsize,
}

/// Une thumbnail retenue : son identité (clé du cache **plus** ETag de la
/// source), son type MIME, et ses bytes.
///
/// Un type nommé plutôt qu'un triplet : le premier champ est le seul qui puisse
/// se confondre avec un autre `String`, et il porte justement la propriété qui
/// rend ce cache sûr — voir le champ `thumbnails`.
struct Thumbnail {
    identity: String,
    mime: &'static str,
    bytes: Arc<Vec<u8>>,
}

/// Nombre de thumbnails HTTP retenues. Le même compte que `ENTREES` : au-delà de
/// la cover courante et de quelques précédentes, personne ne redemande.
const THUMBNAILS: usize = 4;

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// La thumbnail déjà fabriquée pour cette identité, s'il y en a une.
    async fn cached_thumbnail(&self, identity: &str) -> Option<(&'static str, Arc<Vec<u8>>)> {
        self.thumbnails
            .read()
            .await
            .iter()
            .find(|v| v.identity == identity)
            .map(|v| (v.mime, v.bytes.clone()))
    }

    /// Retient une thumbnail sous son identité (clé + ETag), en évinçant la plus
    /// ancienne au-delà de `THUMBNAILS`.
    async fn remember_thumbnail(&self, identity: String, mime: &'static str, bytes: Arc<Vec<u8>>) {
        let mut v = self.thumbnails.write().await;
        v.retain(|e| e.identity != identity);
        v.push_back(Thumbnail { identity, mime, bytes });
        while v.len() > THUMBNAILS {
            v.pop_front();
        }
    }

    /// Fabrique — ou retrouve — la thumbnail de `key`, sous l'identité
    /// `identity` (la clé du cache **plus** l'ETag de la source, voir le champ
    /// `thumbnails`).
    ///
    /// `None` veut dire « pas de thumbnail à serve » sans distinguer les cas :
    /// réencodage désactivé par l'utilisateur, image illisible, dimensions
    /// au-delà du cap. L'appelant retombe alors sur l'original, qui est la
    /// réponse qu'il aurait donnée sans cette route.
    async fn thumbnail(&self, key: &str, identity: &str) -> Option<(&'static str, Arc<Vec<u8>>)> {
        if let Some(trouvee) = self.cached_thumbnail(identity).await {
            return Some(trouvee);
        }
        // Une seule playback des réglages pour les deux étages, comme `line` :
        // deux lectures pourraient encadrer un changement et produire une
        // thumbnail selon des règles qui n'ont jamais coexisté.
        let settings = self.settings();
        let rendu_voulu = settings.rendition?;
        let (mime, bytes) = self.bytes(key, settings.source_max).await?;
        #[cfg(test)]
        self.thumbnails_built.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (mime, bytes) = rendition(mime, bytes, rendu_voulu).await?;
        let bytes = Arc::new(bytes);
        self.remember_thumbnail(identity.to_string(), mime, bytes.clone()).await;
        Some((mime, bytes))
    }

    /// Combien de fois une trame a été construite depuis la création du cache.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Combien de thumbnails ont été fabriquées depuis la création du cache.
    #[cfg(test)]
    pub(crate) fn thumbnails_built(&self) -> usize {
        self.thumbnails_built.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Publie de nouveaux réglages. Prise en compte à la publication suivante :
    /// rien n'est mémorisé, donc il n'y a rien à invalider.
    pub fn set_cover_settings(&self, r: CoverSettings) {
        // Un verrou empoisonné voudrait dire qu'un porteur a paniqué en tenant
        // trente bytes `Copy` — impossible sans un défaut ailleurs. Écraser
        // plutôt que propager : des réglages perdus dégraderaient la publication
        // suivante en silence, là où l'empoisonnement, lui, se voit au journal
        // de la panique d'origine.
        match self.settings.write() {
            Ok(mut g) => *g = r,
            Err(e) => *e.into_inner() = r,
        }
    }

    /// Copie des réglages courants, verrou rendition immédiatement.
    fn settings(&self) -> CoverSettings {
        match self.settings.read() {
            Ok(g) => *g,
            Err(e) => *e.into_inner(),
        }
    }

    pub async fn insert(&self, key: String, p: CoverPayload) {
        let mut e = self.entries.write().await;
        e.retain(|(k, _)| k != &key);
        e.push_back((key, p));
        // Relu à chaque insertion : abaisser le réglage doit reprendre la
        // mémoire au prochain track, pas au prochain redémarrage.
        let cap = self.settings().entries.max(1);
        while e.len() > cap {
            let Some((_, evincee)) = e.pop_front() else { break };
            // Borne l'accumulation **pendant** la vie du processus, pas
            // seulement au démarrage (voir `purge_temp_files`) : une session
            // qui tourne des mois et parcourt une grande bibliothèque ne doit
            // pas laisser un fichier par piste distincte jamais rejouée. Ne
            // touche jamais un `folder.jpg` de Source, qui n'est pas à nous.
            if let CoverPayload::File(path) = &evincee {
                if is_cover_temp(path) {
                    if let Err(err) = tokio::fs::remove_file(path).await {
                        tracing::debug!("purging evicted cover file {}: {err}", path.display());
                    }
                }
            }
        }
    }

    pub async fn contains(&self, key: &str) -> bool {
        self.entries.read().await.iter().any(|(k, _)| k == key)
    }

    async fn read(&self, key: &str) -> Option<CoverPayload> {
        self.entries.read().await.iter().find(|(k, _)| k == key).map(|(_, p)| p.clone())
    }

    /// Matérialise les bytes d'une cover : `(mime, bytes)`.
    ///
    /// **Ce que la route HTTP évite justement de faire.** Elle, pour un fichier
    /// local, ouvre, vérifie l'en-tête et *diffuse en stream* sans jamais tenir
    /// l'image entière. Pousser sur un socket n'en laisse pas le choix, d'où
    /// cette méthode — et d'où le cap, qui n'existait pas côté local (voir
    /// `COVER_MAX_BYTES` et la doc de `fetch`).
    ///
    /// `None` couvre indistinctement : clé inconnue, fichier disparu ou
    /// illisible, partage qui ne répond pas, contenu qui n'est plus une image,
    /// et **size au-delà du cap**. L'appelant n'a rien à en distinguer :
    /// dans tous les cas l'afficheur n'a pas d'image, comme il n'en a pas quand
    /// la récupération échoue.
    /// Le cap est **passé par l'appelant** plutôt que relu ici, pour que
    /// `line` ne lise les réglages qu'une seule fois : deux lectures pourraient
    /// encadrer un changement, et produire une thumbnail selon des règles qui
    /// n'ont jamais coexisté.
    async fn bytes(&self, key: &str, cap: usize) -> Option<(&'static str, Vec<u8>)> {
        // Le verrou est rendition **avant** toute IO. Une cover locale vit
        // couramment sur un partage endormi : tenir le verrou de playback
        // pendant `FILE_TIMEOUT` bloquerait les insertions du cache, donc la
        // tâche détachée de `Core::start_cover_fetch`, pour une image.
        //
        // La branche `Bytes` répond sous le verrou plutôt que de passer par
        // `read` : celui-ci clone la `CoverPayload` entière, ce qui ferait deux
        // copies des bytes au lieu d'une.
        let path = {
            let e = self.entries.read().await;
            match e.iter().find(|(k, _)| k == key).map(|(_, p)| p) {
                None => return None,
                // Déjà en mémoire, et déjà borné par construction : ces
                // bytes viennent d'un corps HTTP que `download` a coupé à
                // `NETWORK_CAP`.
                //
                // Le cap réglable est vérifié quand même : il peut être
                // descendu **sous** `NETWORK_CAP`, et alors la bounded de
                // construction ne dit plus rien. Sans ce contrôle, le réglage
                // ne vaudrait que pour les fichiers locaux — vrai aujourd'hui
                // par la seule coïncidence des deux valeurs, et faux dès qu'on
                // y touche.
                Some(CoverPayload::Bytes(v, mime)) => {
                    if v.len() > cap {
                        tracing::warn!(
                            "network cover not pushed: {} bytes over the {cap}-byte limit",
                            v.len()
                        );
                        return None;
                    }
                    return Some((*mime, v.clone()));
                }
                Some(CoverPayload::File(c)) => c.clone(),
            }
        };
        read_file_bounded(&path, cap).await
    }

    /// Construit la line de protocol `DisplayFrame::Cover` pour `key`/`href` :
    /// le JSON complet, base64 compris, terminé par un saut de line, prêt à
    /// être écrit tel quel sur un socket.
    ///
    /// **Construite à chaque appel, jamais mémorisée, et c'est la propriété qui
    /// compte.** Une line encodée retenue d'un appel sur l'autre a été essayée
    /// ici, puis retirée : la clé du cache hache le *path*, pas le contenu, si
    /// bien qu'une line gardée devenait fausse dès que l'utilisateur remplaçait
    /// l'image sous ce path. Et le geste qui y menait tient en trois clics —
    /// désactiver l'afficheur depuis la page d'admin, remplacer le `folder.jpg`,
    /// le réactiver : le relais rebranché repart avec sa garde de déduplication
    /// à zéro (`main::display_relay`, `CoverTracking`), redemande la
    /// cover courante, et recevait la line d'avant. Rien ne l'invalidait
    /// parce que rien ne *pouvait* l'invalider : remplacer un fichier sur un
    /// partage ne passe par aucun code à nous. Une image visiblement fausse est
    /// le pire des défauts de cet appareil, très au-dessus d'un pic mémoire.
    ///
    /// **Le partage reste souhaitable, mais structurel plutôt que mémorisé.**
    /// L'économie visée — payer une fois par *publication* la matérialisation
    /// des bytes et leur base64, jusqu'à `COVER_MAX_BYTES`, plutôt qu'une fois
    /// par relais abonné — s'obtient en construisant la line **au moment de la
    /// publication** et en donnant le même `Arc` à chaque relais. C'est une
    /// refonte à part entière : la construction read un fichier, elle ne peut
    /// donc pas s'installer sur la boucle principale du cœur. Et il n'y avait
    /// rien à gagner à l'anticiper par un memo, parce qu'en service il n'avait
    /// **aucun** appelant second à serve : `wants_covers` est faux par défaut,
    /// un seul greffon le redéfinit, et `display_relay` n'appelle cette
    /// fonction qu'une fois par changement de `cover_href`. Le greffon MPD ne
    /// repasse pas non plus par ici pour serve ses tranches de 8 Kio — il garde
    /// sa propre copie de la trame reçue.
    ///
    /// **Jamais d'`Arc` dans un type sérialisé** : ce qui voyage derrière l'`Arc`
    /// rendition est la line de texte déjà produite par `serde_json`, pas une valeur
    /// `ritornello_proto::Cover` — ce type-là reste un type de fil ordinaire,
    /// sans partage à exprimer. L'`Arc` sert à `DisplayClient::send_cover_line`,
    /// qui écrit ces bytes tels quels plutôt que de recopier et réencoder.
    ///
    /// `None` couvre les mêmes cas que `bytes` : rien à pousser.
    pub async fn line(&self, key: &str, href: &str) -> Option<Arc<str>> {
        // Inscription au rendez-vous. Le verrou de la table ne couvre que
        // l'inscription elle-même — jamais la construction, qui read un fichier
        // et occupe un cœur. Le tenir pendant le travail sérialiserait des clés
        // *différentes*, ce qui est le contraire du but.
        let cellule = {
            let mut in_flight = self.in_flight.lock().await;
            in_flight.entry(key.to_string()).or_insert_with(FrameInFlight::default).clone()
        };

        // `href` n'a pas besoin d'être comparé entre appelants : `key` en est
        // dérivée (`display_relay` la tire de `href` par
        // `strip_prefix(HREF_PREFIX)`), donc deux appelants de même clé
        // portent la même chaîne. Un suiveur reçoit bien la trame du premier
        // arrivé, et elle décrit la même image sous le même name.
        let resultat = cellule
            .get_or_init(|| async {
                #[cfg(test)]
                self.builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Une seule playback des réglages pour les deux étages : voir
                // `octets_bornes`. Deux lectures pourraient encadrer un
                // changement, et produire une thumbnail selon des règles qui
                // n'ont jamais coexisté.
                let settings = self.settings();
                let (mime, bytes) = self.bytes(key, settings.source_max).await?;
                // Le rendition s'applique **ici et pas dans `bytes`**, donc sur le
                // seul path de poussée. La route HTTP `cover_get`, elle,
                // diffuse le fichier local en stream sans jamais le tenir en
                // entier : lui imposer un réencodage lui ferait perdre
                // exactement la propriété qui la rend économique, pour une image
                // que le navigateur redimensionne et met en cache de son côté.
                let (mime, bytes) = match settings.rendition {
                    None => (mime, bytes),
                    Some(r) => rendition(mime, bytes, r).await?,
                };
                let cover = ritornello_proto::Cover {
                    href: href.to_string(),
                    mime: mime.to_string(),
                    bytes,
                };
                let mut line =
                    serde_json::to_string(&ritornello_proto::DisplayFrame::Cover(cover)).ok()?;
                line.push('\n');
                Some(Arc::from(line))
            })
            .await
            .clone();

        // **Le retrait est ce qui empêche le rendez-vous de devenir un cache.**
        // Une `OnceCell` garde sa valeur pour toujours ; laissée dans la table,
        // elle servirait la même thumbnail à un appelant survenu une heure plus
        // tard, alors que le fichier a pu changer sous son path.
        //
        // Tous les appelants tentent le retrait, pas seulement le premier
        // arrivé : si celui-là est abandonné en cours (sa tâche annulée), un
        // suiveur reprend l'initialisation, et personne d'autre ne serait là
        // pour nettoyer.
        //
        // L'identité est vérifiée avant de retirer : entre la fin du travail et
        // ce verrou, un appelant plus récent a pu inscrire une cellule **neuve**
        // sous la même clé. La retirer lui ferait perdre son rendez-vous — pas
        // un défaut de justesse, mais exactement l'économie qu'on installe ici.
        {
            let mut in_flight = self.in_flight.lock().await;
            if in_flight.get(key).is_some_and(|c| Arc::ptr_eq(c, &cellule)) {
                in_flight.remove(key);
            }
        }
        resultat
    }
}

/// Réencode une cover en thumbnail, ou rend les bytes d'origine quand il n'y
/// a rien à gagner.
///
/// Quatre étapes, dans cet order, et l'order **est** la protection :
///
/// 1. **Les dimensions sont lues dans l'en-tête**, sans décoder. Quelques
///    dizaines d'bytes suffisent, et rien n'est alloué à la size de l'image.
/// 2. **La garde anti-bombe** compare le nombre de pixels au cap. C'est la
///    seule bounded qui protège vraiment : la size du fichier ne dit *rien* du
///    coût du décodage — un PNG de 200 Kio peut annoncer 30000 × 30000 pixels,
///    soit 3,6 Gio de buffer, et `source_max` le laisse passer sans broncher.
/// 3. **Le passe-droit** : une image déjà petite en pixels *et* en bytes part
///    telle quelle, sans décodage ni réencodage. Une cover de 300 × 300 tirée
///    d'un fichier n'a rien à gagner d'un aller-retour qui la dégraderait.
/// 4. **Le décodage et l'encodage**, sur un fil bloquant.
///
/// Inverser 2 et 1 serait absurde ; inverser 3 et 2 serait dangereux — une
/// image de 30000 × 30000 pesant 200 Kio passerait le passe-droit sur son poids
/// alors qu'elle est précisément la bombe qu'on cherche à refuser. Le
/// passe-droit teste donc les **deux** critères, et vient après la garde.
///
/// **Rien n'est mémorisé**, et c'est cohérent avec `line` : la clé du cache
/// hache le path, pas le contenu, donc une thumbnail gardée deviendrait fausse
/// dès que l'utilisateur remplace l'image sous ce path. Le prix est un
/// décodage par publication, et `line` n'est appelée qu'une fois par changement
/// de cover et par relais abonné.
///
/// `None` = rien à pousser, comme partout dans ce module : image illisible,
/// dimensions au-delà du cap, ou thumbnail produite au-delà du filet.
async fn rendition(
    mime: &'static str,
    bytes: Vec<u8>,
    r: Rendition,
) -> Option<(&'static str, Vec<u8>)> {
    let (largeur, hauteur) = dimensions(&bytes)?;
    let pixels = u64::from(largeur) * u64::from(hauteur);
    if pixels > r.pixel_cap {
        tracing::warn!(
            "cover not pushed: {largeur}x{hauteur} is {pixels} pixels, over the {} allowed \
             (decoding it would need about {} MiB)",
            r.pixel_cap,
            pixels * 4 / (1024 * 1024)
        );
        return None;
    }
    if largeur.max(hauteur) <= r.max_edge_px && bytes.len() <= r.output_cap {
        tracing::debug!("cover already small ({largeur}x{hauteur}, {} bytes), pushed as it is", bytes.len());
        return Some((mime, bytes));
    }

    // `spawn_blocking` : décoder puis réencoder une image de plusieurs
    // mégapixels occupe un cœur pendant des centaines de millisecondes sur un
    // Pi 2. Le faire sur un fil de l'ordonnanceur figerait la boucle du cœur —
    // donc l'horloge de position, les commands de la télécommande et les
    // requêtes HTTP — le temps d'une cover.
    //
    // Cette tâche n'est **pas annulable** : abandonner la future ici ne
    // l'arrête pas, elle ira jusqu'au bout et son résultat sera jeté. C'est
    // acceptable précisément grâce à la garde de l'étape 2, qui bounded ce
    // qu'elle peut coûter avant de la lancer.
    let plafond_alloc = (r.pixel_cap as usize).saturating_mul(4);
    let travail = tokio::task::spawn_blocking(move || encode(bytes, r, plafond_alloc)).await;
    let (mime, sortie) = match travail {
        Ok(Some(v)) => v,
        Ok(None) => return None,
        Err(e) => {
            // Une panique du décodeur sur une entrée venue du réseau : refusée
            // comme le reste, mais journalisée en `warn` — c'est un défaut de la
            // bibliothèque ou une entrée qui l'a mise en défaut, pas un cas
            // d'usage.
            tracing::warn!("cover rendition panicked: {e}");
            return None;
        }
    };
    if sortie.len() > r.output_cap {
        tracing::warn!(
            "cover not pushed: rendered to {} bytes, over the {}-byte net",
            sortie.len(),
            r.output_cap
        );
        return None;
    }
    tracing::debug!(
        "cover rendered: {} bytes in, {} bytes out ({mime})",
        pixels * 4,
        sortie.len()
    );
    Some((mime, sortie))
}

/// Dimensions annoncées par l'en-tête, sans décoder l'image.
///
/// Séparée pour être testable seule : c'est la valeur dont dépend la garde
/// anti-bombe, et une garde qui read mal ses dimensions ne garde rien.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let player = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    match player.into_dimensions() {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::debug!("cover header unreadable: {e}");
            None
        }
    }
}

/// Le décodage et l'encodage eux-mêmes. **Bloquant** : appelé sous
/// `spawn_blocking`.
fn encode(bytes: Vec<u8>, r: Rendition, plafond_alloc: usize) -> Option<(&'static str, Vec<u8>)> {
    let mut player = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    // La ceinture après les bretelles : la garde de `rendition` a déjà refusé les
    // dimensions trop grandes, mais elle croit l'en-tête. `Limits` bounded
    // l'allocation réelle du décodeur, donc couvre le cas d'un en-tête qui
    // mentirait sur ses propres dimensions — le fichier fabriqué exprès, pas le
    // fichier maladroit.
    let mut limites = image::Limits::default();
    limites.max_alloc = Some(plafond_alloc as u64);
    player.limits(limites);
    let image = match player.decode() {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!("cover undecodable: {e}");
            return None;
        }
    };

    // `thumbnail` et non `resize` : à qualité d'échantillonnage comparable pour
    // une réduction forte (chaque pixel source contribue à un pixel cible), il
    // est nettement moins coûteux — et sur un Pi 2 c'est le facteur qui décide.
    // Le rapport est conservé, l'image tient dans le carré demandé.
    let thumbnail = image.thumbnail(r.max_edge_px, r.max_edge_px);

    let mut sortie = Vec::new();
    // PNG dès qu'il y a un canal alpha, sans perte. Aplatir la transparence
    // demanderait de choisir une couleur de fond — un parti pris visuel que
    // l'appareil n'a pas à prendre sur la cover de quelqu'un d'autre.
    if thumbnail.color().has_alpha() {
        if let Err(e) = thumbnail.write_to(&mut std::io::Cursor::new(&mut sortie), image::ImageFormat::Png) {
            tracing::warn!("cover PNG encoding failed: {e}");
            return None;
        }
        return Some(("image/png", sortie));
    }
    let mut encodeur =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut sortie, r.jpeg_quality);
    // `to_rgb8` : l'encodeur JPEG refuse un buffer à canal alpha, et une image
    // en niveaux de gris ou en palette doit de toute façon être convertie.
    if let Err(e) = encodeur.encode_image(&thumbnail.to_rgb8()) {
        tracing::warn!("cover JPEG encoding failed: {e}");
        return None;
    }
    Some(("image/jpeg", sortie))
}

/// Ce que la playback bornée d'un fichier de cover rend, avant validation
/// du type d'image.
enum BoundedRead {
    Bytes(Vec<u8>),
    /// La size du fichier, **connue par `metadata`, avant toute playback des
    /// bytes eux-mêmes** : voir la doc de `read_file_bounded`.
    TooLarge(u64),
}

/// Lit un fichier de cover pour le pousser, borné et validé.
///
/// **La validation d'en-tête est faite sur les bytes rendus eux-mêmes**, et
/// non sur une première playback séparée. La route HTTP, elle, ne peut pas :
/// elle doit vérifier puis diffuser, donc elle prend soin de garder le *même
/// descripteur* entre les deux lectures — sans quoi un contributeur pourrait
/// remplacer le contenu du partage entre la vérification et le service. Ici le
/// contenu vérifié **est** le contenu rendition, un seul descripteur et une seule
/// playback : la fenêtre n'existe pas du tout, plutôt que d'être fermée. La
/// garantie n'est donc pas affaiblie mais renforcée.
///
/// **La size est vérifiée avant toute playback des bytes**, sur `metadata`,
/// et c'est délibéré : une size de fichier ne demande aucune connaissance du
/// format — pas d'en-tête à interpréter, pas de décodeur, indifférente à un
/// JPEG, un PNG, un WebP ou ce qui viendra ensuite. Un fichier du NAS
/// démesuré (le PNG de 150 Mo que `cover_get` cite comme cas réel) est ainsi
/// refusé sans qu'un seul octet de son contenu ne soit lu, plutôt que d'être
/// découvert après une playback bornée à `COVER_MAX_BYTES + 1` bytes — un
/// coût qui n'a de sens que si le fichier passe la bounded. `take` avant
/// `read_to_end` reste en place ensuite, en filet : si le fichier grossit
/// *entre* le `metadata` et la playback, la fenêtre TOCTOU rouverte ne laisse
/// jamais read plus de `COVER_MAX_BYTES + 1` bytes.
///
/// Deux bornes de temps sous le même délai, et une de size avant tout :
///
/// * `metadata` puis, si la size passe, `COVER_MAX_BYTES + 1` bytes au plus
///   sont lus (le filet TOCTOU ci-dessus).
/// * `FILE_TIMEOUT`, comme partout où ce module touche un fichier : le
///   partage peut être endormi, et l'attente doit être bornée par nous plutôt
///   que par le noyau.
async fn read_file_bounded(
    path: &std::path::Path,
    cap: usize,
) -> Option<(&'static str, Vec<u8>)> {
    let playback = tokio::time::timeout(FILE_TIMEOUT, async {
        let fichier = tokio::fs::File::open(path).await?;
        let size = fichier.metadata().await?.len();
        if size > cap as u64 {
            return Ok::<_, std::io::Error>(BoundedRead::TooLarge(size));
        }
        let mut bytes = Vec::new();
        // `take` **avant** `read_to_end` : `read_to_end` seul lirait le
        // fichier entier, et le contrôle de size arriverait après
        // l'allocation qu'il est censé éviter. N'agit ici que sur la fenêtre
        // TOCTOU (voir la doc au-dessus) : le cas courant a déjà été tranché
        // par `metadata`.
        fichier.take(cap as u64 + 1).read_to_end(&mut bytes).await?;
        Ok(BoundedRead::Bytes(bytes))
    })
    .await;
    let bytes = match playback {
        Ok(Ok(BoundedRead::Bytes(v))) => v,
        Ok(Ok(BoundedRead::TooLarge(size))) => {
            // La size exacte de l'offense, connue sans avoir rien lu de son
            // contenu — c'est ce que la playback bornée à `+ 1` octet ne
            // pourrait jamais journaliser : elle ne verrait jamais que
            // `cap + 1`, quelle que soit la size réelle.
            tracing::warn!(
                "cover file {} not read: {size} bytes over the {cap}-byte limit",
                path.display()
            );
            return None;
        }
        Ok(Err(e)) => {
            tracing::debug!("cover file unreadable: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
            return None;
        }
    };
    if bytes.len() > cap {
        tracing::warn!(
            "cover file {} not pushed: grew past {cap} bytes while being read",
            path.display()
        );
        return None;
    }
    let mime = image_type(&bytes)?;
    Some((mime, bytes))
}

/// Bytes d'en-tête d'une image reconnue. Vérifiés avant de serve un fichier
/// local : sans cela, un contributeur mal écrit ferait serve n'importe quel
/// fichier du système sur une route HTTP publique.
fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Nombre de sauts de redirection tolérés, la valeur par défaut de `reqwest`.
///
/// Reprise explicitement : remplacer la politique par défaut par une
/// politique personnalisée fait aussi perdre son cap, et une chaîne de
/// redirections sans fin est un déni de service à un aller-retour de coût.
const MAX_HOPS: usize = 10;

/// Client HTTP partagé : le construire à chaque appel referait à chaque fois
/// la configuration de `rustls` et le chargement du magasin de racines, ce
/// que la documentation de `reqwest` demande justement d'éviter. La
/// configuration est figée (pas de proxy, pas d'entrée utilisateur) : un
/// échec de construction serait un défaut de l'environnement, pas une
/// panne par requête, d'où l'`expect`.
///
/// **Les redirections sont suivies, mais chaque saut est revalidé.** La
/// conception exige de les suivre (Radio France répond une 301 cross-host,
/// mesurée), et la politique par défaut de `reqwest` les suivait sans rien
/// vérifier : `allowed_target` ne s'appliquait qu'à l'URL de départ, donc
/// l'hôte d'image — un tiers auquel la conception ne fait justement pas
/// confiance, puisque le `coverUrl` d'OUI FM est écrit par autrui — n'avait
/// qu'à répondre `302 http://192.168.1.1/…` pour faire émettre à l'appareil
/// un GET sur son réseau local, changement de schéma compris. Un saut
/// d'indirection annulait tout le garde-fou.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!(
                "ritornello/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/skerdudou/ritornello)"
            ))
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::custom(|saut| {
                if saut.previous().len() >= MAX_HOPS {
                    return saut.stop();
                }
                if allowed_target(saut.url().as_str()) {
                    saut.follow()
                } else {
                    tracing::debug!("cover redirect refused: target not allowed");
                    saut.stop()
                }
            }))
            .build()
            .expect("configuration HTTP figée, ne doit jamais échouer")
    })
}

/// La cible est-elle acceptable pour une requête sortante ?
///
/// Le contrôle vit ici, et non dans `ritornello-proto`, pour deux raisons :
/// c'est ici que la requête part, et rejouer à la main les règles d'analyse
/// d'URL est une course perdue — un point final (`192.168.1.1.`) ou un
/// libellé hexadécimal (`0x7f.0.0.1`) suffisent à faire passer une adresse
/// littérale pour un name d'hôte devant le découpage de chaînes de
/// `ritornello-proto`. `Url::domain()` s'appuie sur l'analyse WHATWG déjà
/// faite par `reqwest` (ré-exportée, donc aucune dépendance de plus) : elle
/// classe l'hôte en IPv4/IPv6 **après** normalisation, quelle qu'ait été sa
/// graphie d'origine, et ne renvoie `Some` que pour un vrai name de domaine.
///
/// `ritornello-proto` garde la forme (https, extension) ; ce module-ci garde
/// la cible : c'est lui qui émet la requête, et c'est le SSE d'une source
/// tierce (OUI FM, par exemple) qui peut fournir l'URL.
///
/// Appliquée à **chaque** cible atteinte, pas seulement à la première :
/// `fetch` filtre l'URL de départ, la politique de redirection de
/// `client()` filtre tous les sauts suivants.
fn allowed_target(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    if u.scheme() != "https" {
        return false;
    }
    match u.domain() {
        // `None` couvre aussi bien l'absence d'hôte qu'une adresse IP
        // littérale (v4 ou v6) : `domain()` ne renvoie `Some` que pour un
        // name de domaine, jamais pour un `HostInternal::Ipv4`/`Ipv6`.
        Some(d) => d.contains('.'),
        None => false,
    }
}

/// Effectue la requête et applique les trois garde-fous réseau : le
/// `Content-Type`, le cap appliqué en lisant par morceaux, et les bytes
/// magiques du corps reçu. Séparée de `fetch` pour rester testable contre
/// un serveur HTTP local (`127.0.0.1`) sans jamais passer par
/// `allowed_target`, qui refuserait justement cette adresse.
async fn download(url: &str) -> Option<CoverPayload> {
    let mut reponse = client().get(url).send().await.ok()?;
    if !reponse.status().is_success() {
        tracing::debug!("cover fetch returned {}", reponse.status());
        return None;
    }
    let mime = reponse
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !mime.starts_with("image/") {
        tracing::debug!("cover fetch refused: content-type {mime:?}");
        return None;
    }
    // Plafond appliqué **en lisant par morceaux** : contrôler le
    // `Content-Length` annoncé ne protège de rien, il est déclaratif.
    let mut bytes = Vec::new();
    while let Some(track) = reponse.chunk().await.ok()? {
        if bytes.len() + track.len() > NETWORK_CAP {
            tracing::debug!("cover fetch refused: over {NETWORK_CAP} bytes");
            return None;
        }
        bytes.extend_from_slice(&track);
    }
    let mime = image_type(&bytes)?;
    Some(CoverPayload::Bytes(bytes, mime))
}

/// Délai accordé à un accès au fichier image lui-même — ouverture,
/// `metadata`, premiers bytes.
///
/// **Le même que celui de l'extraction embarquée** (`health::TIMEOUT`), et pour
/// la même raison : ces deux chemins-ci touchent des fichiers qui vivent
/// couramment sur un partage SMB endormi, et ce projet a déjà vécu la panne
/// qu'une IO qui n'aboutit pas provoque. Aucune boucle d'événements n'est
/// retenue ici — la récupération est détachée, la route HTTP est une tâche par
/// requête — donc l'audio ne risque rien ; ce qui est borné, c'est l'attente
/// elle-même, plutôt que de la laisser durer aussi longtemps que le noyau
/// voudra.
///
/// Une bounded de temps et non `Health` : le disjoncteur prend une **fermeture
/// bloquante** (`spawn_blocking`), là où ces deux chemins sont déjà
/// asynchrones et, dans le cas de `cover_get`, doivent rendre un
/// `tokio::fs::File` à diffuser en stream. L'y faire entrer demanderait de
/// repasser en `std::fs` puis de reconvertir, et de câbler le disjoncteur
/// jusque dans l'`AppState` HTTP — une refonte pour une propriété que la
/// bounded donne déjà. Ce que `Health` apporterait en plus, et qu'on n'a donc
/// pas ici, est la mémoire du montage muet : un fil du pool bloquant reste
/// perdu par tentative, exactement ce que `health.rs` documente comme
/// inévitable une fois le noyau parti.
const FILE_TIMEOUT: std::time::Duration = crate::health::TIMEOUT;

/// Va chercher la cover. `None` = échec, et l'échec est **silencieux** :
/// l'appareil n'affiche simplement pas d'image.
pub async fn fetch(r: &CoverRef) -> Option<CoverPayload> {
    match r {
        CoverRef::Path { path } => {
            let path = PathBuf::from(path);
            let a_lire = path.clone();
            // Ouverture **et** première playback sous la même bounded : c'est
            // l'ouverture qui bloque sur un partage endormi, mais un partage
            // qui répond à l'`open` et plus au `read` est le cas d'une
            // déconnexion en cours — les deux doivent être couverts.
            let reconnu = tokio::time::timeout(FILE_TIMEOUT, async move {
                let mut fichier = tokio::fs::File::open(&a_lire).await.ok()?;
                let mut tete = [0u8; 12];
                let lus = fichier.read(&mut tete).await.ok()?;
                image_type(&tete[..lus])
            })
            .await;
            match reconnu {
                Ok(Some(_)) => {}
                Ok(None) => return None,
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
                    return None;
                }
            }
            // Le cap ne s'applique pas au local : il protège d'un tiers sur
            // le réseau, et un fichier du NAS est de confiance. Ses bytes
            // d'en-tête ont été vérifiés, c'est ce qui compte. La route relira
            // le fichier au moment de serve : entre les deux, le partage n'est
            // plus sous le contrôle de l'appareil (voir `cover_get`).
            Some(CoverPayload::File(path))
        }
        CoverRef::Url { url } => {
            if !allowed_target(url) {
                tracing::debug!("cover fetch refused: target not allowed");
                return None;
            }
            download(url).await
        }
    }
}

/// ETag d'un fichier local : contrairement à la clé du cache — qui hache la
/// **source** (le path), pas le contenu — ce fichier reste modifiable après
/// coup sur son partage. L'ETag doit donc suivre le contenu, pas seulement le
/// path, sans quoi une requête conditionnelle validerait indéfiniment une
/// image que l'utilisateur a pourtant remplacée.
fn file_etag(modifie: Option<std::time::SystemTime>, size: u64) -> String {
    let nanos = modifie
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("\"{nanos:x}-{size:x}\"")
}

/// Ce que la route sait serve.
///
/// **Deux tailles et non une, parce que la page en a deux usages.** Le carré de
/// la carte fait 224 px sur téléphone : y charger le `folder.jpg` de trois
/// mégaoctets d'un NAS est du gaspillage pur, surtout en Wi-Fi. Mais la même
/// image agrandie au clic (voir `PlayerCard.vue`) mérite, elle, tous ses
/// pixels. La size est donc **demandée par l'appelant** plutôt que devinée
/// ici.
///
/// Pleine size par défaut, et c'est délibéré : le `cover_href` publié dans
/// l'état désigne l'image telle qu'elle est, sans transformation, pour tout
/// consommateur présent ou futur du protocol. La thumbnail est un service rendition
/// à qui la demande, pas un changement de ce que l'URL nue veut dire.
/// La chaîne de requête de `cover_get`.
///
/// **Une chaîne libre et non une énumération sérialisée**, et c'est une
/// correction : un `enum` faisait refuser la requête entière par l'extracteur
/// `Query` — un `?size=nawak` rendait un 400, donc le repli ♫ sur la page,
/// pour une simple faute de frappe dans une URL. Une valeur inconnue doit
/// valoir le défaut, comme une valeur absente : la size est un service rendition
/// à qui la demande, jamais une condition de service.
#[derive(Debug, Default, serde::Deserialize)]
pub struct CoverParams {
    #[serde(default)]
    size: Option<String>,
}

/// Le mot qui demande la réduction, tel que la page l'écrit dans son URL.
const THUMBNAIL_SIZE: &str = "thumbnail";

/// `GET /api/cover/{clé}[?size=thumbnail]`. La clé est une empreinte de la
/// **source**, donc son immuabilité ne dit rien du contenu : une cover
/// réseau est bien figée sous sa clé (elle vient d'un corps déjà entièrement
/// vérifié), mais un fichier local reste modifiable sur son partage après coup.
pub async fn cover_get(
    State(state): State<crate::status::AppState>,
    Path(key): Path<String>,
    axum::extract::Query(params): axum::extract::Query<CoverParams>,
    headers: HeaderMap,
) -> Response {
    let vignette_demandee = params.size.as_deref() == Some(THUMBNAIL_SIZE);
    let Some(p) = state.covers.read(&key).await else {
        // **Un `warn`, et il manquait.** Cette clé a été publiée dans
        // `cover_href` par le cœur lui-même : ne plus savoir la serve est une
        // promesse rompue, pas un cas ordinaire. Le cache ne garde que
        // `ENTREES` entrées, donc le suspect est l'éviction — c'est cette line
        // qui le dira, là où l'écran ne montrait qu'un ♫ sans explication et où
        // le propriétaire a rapporté « aucun warn ».
        tracing::warn!("cover {key} requested but no longer in the cache (evicted?)");
        return (StatusCode::NOT_FOUND, "inconnue").into_response();
    };
    match p {
        CoverPayload::Bytes(bytes, mime) => {
            // Une cover réseau est figée sous sa clé : son ETag n'a rien à
            // porter de plus que la clé et la size demandée, et sa thumbnail
            // est aussi immuable qu'elle.
            if vignette_demandee {
                let etag = format!("\"{key}-v\"");
                if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok())
                    == Some(etag.as_str())
                {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                if let Some((mime, petite)) =
                    state.covers.thumbnail(&key, &format!("{key}:v")).await
                {
                    return (
                        [
                            (header::CONTENT_TYPE, mime.to_string()),
                            (
                                header::CACHE_CONTROL,
                                "public, max-age=31536000, immutable".to_string(),
                            ),
                            (header::ETAG, etag),
                        ],
                        petite.as_slice().to_vec(),
                    )
                        .into_response();
                }
                // Pas de thumbnail (réencodage désactivé, image illisible,
                // dimensions au-delà du cap) : l'original, qui est la
                // réponse qu'on aurait donnée sans ce paramètre. Mieux vaut une
                // image trop grande que pas d'image.
            }
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
                    (header::ETAG, format!("\"{key}\"")),
                ],
                bytes,
            )
                .into_response()
        }
        CoverPayload::File(path) => {
            // Ouverture et `metadata` sous une bounded de temps : ce fichier
            // vit couramment sur un partage réseau, et cette route est
            // joignable par n'importe quel navigateur du LAN. Sans bounded, un
            // partage endormi retenait la requête aussi longtemps que le
            // noyau le voulait — l'incident même que `health.rs` existe pour
            // borner. L'expiration est traitée comme l'illisibilité qui
            // existait déjà : un 404, que l'IHM rend par son repli ♫.
            //
            // Bornée **en deux temps**, l'en-tête juste en dessous : garder la
            // réponse 304 avant toute playback du corps est ce qui rend une
            // requête conditionnelle réellement bon marché.
            let ouverture = tokio::time::timeout(FILE_TIMEOUT, async {
                let fichier = tokio::fs::File::open(&path).await?;
                let meta = fichier.metadata().await?;
                Ok::<_, std::io::Error>((fichier, meta))
            })
            .await;
            let (mut fichier, meta) = match ouverture {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    // `warn` et non `debug` : le cœur a publié ce `cover_href`,
                    // donc échouer à le serve est un défaut visible à l'écran
                    // et doit l'être au journal. En `debug` il ne l'était nulle
                    // part.
                    tracing::warn!("cover {key} unreadable: {e}");
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
            };
            // L'ETag de la thumbnail n'est pas celui de l'original : c'est le
            // même contenu source mais pas les mêmes bytes servis, et deux
            // réponses différentes sous un même validateur feraient serve au
            // navigateur l'une pour l'autre.
            let etag_source = file_etag(meta.modified().ok(), meta.len());
            let etag = if vignette_demandee {
                format!("\"v-{}\"", etag_source.trim_matches('"'))
            } else {
                etag_source.clone()
            };
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            // **Après le 304 et pas avant** : une requête conditionnelle ne doit
            // rien fabriquer, et c'est ce qui rend la thumbnail bon marché en
            // régime établi. L'identité mémorisée porte l'ETag de la source,
            // donc un `folder.jpg` remplacé sur le partage change de clé et son
            // ancienne thumbnail n'est plus jamais servie.
            if vignette_demandee {
                let identity = format!("{key}:{etag_source}");
                if let Some((mime, petite)) = state.covers.thumbnail(&key, &identity).await {
                    return (
                        [
                            (header::CONTENT_TYPE, mime.to_string()),
                            (header::CACHE_CONTROL, "no-cache".to_string()),
                            (header::ETAG, etag),
                        ],
                        petite.as_slice().to_vec(),
                    )
                        .into_response();
                }
                // Rien à réduire : on retombe sur la diffusion en stream de
                // l'original, ci-dessous, avec l'ETag de la thumbnail — le
                // contenu servi sous cette URL reste cohérent avec son
                // validateur, ce qui est tout ce que le cache exige.
            }
            // Revalidation des bytes d'en-tête au moment de serve, et non
            // seulement au moment de la découverte (`fetch`) : entre les
            // deux, le partage n'est pas sous le contrôle de l'appareil, et un
            // contributeur qui remplacerait le contenu ne doit pas voir serve
            // n'importe quoi sous cette route publique. Même descripteur de
            // fichier pour la vérification et pour le stream servi ensuite : le
            // contenu ne peut pas changer entre les deux lectures.
            //
            // Seconde bounded, sur la playback cette fois : un partage qui
            // répond à l'`open` et plus au premier `read` est le cas d'une
            // déconnexion en cours, et rien ne l'écarterait sans cela.
            let entete = tokio::time::timeout(FILE_TIMEOUT, async {
                let mut tete = [0u8; 12];
                let lus = fichier.read(&mut tete).await?;
                fichier.seek(std::io::SeekFrom::Start(0)).await?;
                Ok::<_, std::io::Error>((tete, lus))
            })
            .await;
            let (tete, lus) = match entete {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    // `warn` et non `debug` : le cœur a publié ce `cover_href`,
                    // donc échouer à le serve est un défaut visible à l'écran
                    // et doit l'être au journal. En `debug` il ne l'était nulle
                    // part.
                    tracing::warn!("cover {key} unreadable: {e}");
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {FILE_TIMEOUT:?}", path.display());
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
            };
            let Some(mime) = image_type(&tete[..lus]) else {
                tracing::warn!(
                    "cover {key} is no longer an image: {}",
                    path.display()
                );
                return (StatusCode::NOT_FOUND, "illisible").into_response();
            };
            // En stream, pas en un `Vec` unique : cette route est joignable sans
            // authentification depuis le LAN, et un fichier local n'a par
            // conception aucun cap de size. Un PNG de 150 Mo sur le
            // partage, ou quelques requêtes concurrentes sur un fichier de
            // quelques mégaoctets, ne doivent pas épuiser la mémoire d'un Pi.
            let corps = Body::from_stream(ReaderStream::new(fichier));
            (
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                    (header::ETAG, etag),
                ],
                corps,
            )
                .into_response()
        }
    }
}

/// Fixtures d'image partagées par les tests de ce module et ceux de `main`.
///
/// Ici et non dans chaque `mod tests` : deux copies d'un générateur d'image
/// dériveraient, et un test qui croit produire une image décodable alors qu'il
/// n'en produit plus est un faux positif silencieux.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Un JPEG **réellement décodable** de `largeur × hauteur`.
    ///
    /// Nécessaire dès qu'un test traverse `CoverCache::line` : le rendition, active
    /// par défaut, décode l'image, et un en-tête suivi de remplissage est refusé
    /// — à juste titre, c'est un fichier tronqué.
    ///
    /// Un dégradé et non un aplat : un aplat se comprime à quelques centaines
    /// d'bytes quelle que soit sa size, ce qui rendrait indistinguables « la
    /// thumbnail a été produite » et « le cap de sortie n'a jamais été
    /// approché ».
    pub fn jpeg_decodable(largeur: u32, hauteur: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(largeur, hauteur);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        }
        let mut sortie = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut sortie, 90)
            .encode_image(&img)
            .expect("encodage de la fixture");
        sortie
    }

    /// Un PNG décodable **à canal alpha**, pour le path sans perte.
    pub fn png_alpha(largeur: u32, hauteur: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(largeur, hauteur);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x % 256) as u8, (y % 256) as u8, 0, ((x + y) % 256) as u8]);
        }
        let mut sortie = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut sortie), image::ImageFormat::Png)
            .expect("encodage de la fixture");
        sortie
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::CoverRef;

    #[test]
    fn la_cle_est_stable_et_distingue_deux_sources() {
        let a = CoverRef::Url { url: "https://x.org/a.jpg".into() };
        let b = CoverRef::Url { url: "https://x.org/b.jpg".into() };
        assert_eq!(key(&a), key(&a), "la key doit etre stable : elle est publiee dans une URL");
        assert_ne!(key(&a), key(&b));
        // Une forme differente pour la meme chaine ne doit pas collisionner.
        assert_ne!(key(&a), key(&CoverRef::Path { path: "/https://x.org/a.jpg".into() }));
        // Hexadecimal, donc sans surprise dans un path d'URL.
        assert!(key(&a).chars().all(|c| c.is_ascii_hexdigit()), "{}", key(&a));
    }

    /// Le corps servi par la vraie route HTTP pour cette clé et cette size.
    ///
    /// Par `status::router` et une vraie requête, comme
    /// `la_route_http_sert_ce_que_le_coeur_vient_de_deposer` : la chaîne
    /// d'extracteurs (dont le `Query` qui read `size`) fait partie de ce qu'on
    /// éprouve, et l'appeler en fonction la court-circuiterait.
    async fn corps_servi(cache: &Arc<CoverCache>, key: &str, requete: &str) -> (u16, Vec<u8>) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let app = crate::status::router(crate::status::AppState {
            covers: cache.clone(),
            ..crate::status::tests_support::app_state()
        });
        let resp = app
            .oneshot(Request::get(format!("/api/cover/{key}{requete}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let statut = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (statut, bytes.to_vec())
    }

    #[tokio::test]
    async fn la_route_sert_une_vignette_a_qui_la_demande_et_loriginal_sinon() {
        // **La demande du propriétaire** : le carré de 224 px de la page
        // d'accueil ne doit plus télécharger le `folder.jpg` entier d'un NAS.
        // L'URL nue, elle, ne change pas de sens — c'est elle que la vue
        // agrandie charge, et elle doit rendre tous les pixels.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let grande = fixtures::jpeg_decodable(1500, 1500);
        std::fs::write(&path, &grande).unwrap();
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".to_string(), CoverPayload::File(path)).await;

        let (statut, pleine) = corps_servi(&cache, "k", "").await;
        assert_eq!(statut, 200);
        assert_eq!(pleine.len(), grande.len(), "l'URL nue sert le fichier tel quel");

        let (statut, thumbnail) = corps_servi(&cache, "k", "?size=thumbnail").await;
        assert_eq!(statut, 200);
        assert!(
            thumbnail.len() < pleine.len(),
            "la thumbnail doit peser moins ({} contre {})",
            thumbnail.len(),
            pleine.len()
        );
        let (l, h) = dimensions(&thumbnail).expect("la thumbnail doit rester une image lisible");
        let cote = crate::state::Settings::default().cover_max_edge_px;
        assert!(l <= cote && h <= cote, "thumbnail {l}x{h}, cap {cote}");
    }

    #[tokio::test]
    async fn une_taille_inconnue_retombe_sur_loriginal_plutot_que_sur_une_erreur() {
        // Une URL mal formée ne doit pas rendre la cover introuvable : le
        // carré de la page afficherait le repli ♫ pour une faute de frappe.
        let cache = Arc::new(CoverCache::new());
        let bytes = fixtures::jpeg_decodable(40, 40);
        cache.insert("k".to_string(), CoverPayload::Bytes(bytes.clone(), "image/jpeg")).await;
        let (statut, corps) = corps_servi(&cache, "k", "?size=nawak").await;
        assert_eq!(statut, 200);
        assert_eq!(corps, bytes);
    }

    #[tokio::test]
    async fn la_vignette_dun_fichier_nest_fabriquee_quune_fois() {
        // Décoder puis réencoder coûte des centaines de millisecondes sur un
        // Pi 2 : deux navigateurs qui ouvrent la page ne doivent pas le payer
        // deux fois. L'identité mémorisée porte l'ETag de la source, donc rien
        // de périmé ne peut être servi (voir le champ `thumbnails`).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(1200, 1200)).unwrap();
        let cache = Arc::new(CoverCache::new());
        cache.insert("k".to_string(), CoverPayload::File(path.clone())).await;

        let (_, une) = corps_servi(&cache, "k", "?size=thumbnail").await;
        let (statut, deux) = corps_servi(&cache, "k", "?size=thumbnail").await;
        assert_eq!(statut, 200);
        assert_eq!(une, deux);
        // **Le décompte est la seule preuve** : comparer les deux réponses ne
        // dit rien, deux fabrications successives rendent les mêmes bytes.
        // Même raison que le compteur `builds` du rendez-vous.
        assert_eq!(cache.thumbnails_built(), 1, "la seconde demande doit etre gratuite");
    }

    #[tokio::test]
    async fn le_cache_est_borne_par_le_reglage_et_oublie_la_plus_ancienne() {
        // **La bounded est desormais un reglage** (`cover_cache_entries`, 20 par
        // defaut) et non une constante : le test la pose donc lui-meme, ce qui
        // prouve du meme coup qu'elle est bien lue a chaque insertion.
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { entries: 4, ..CoverSettings::default() });
        for i in 0..6 {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![i as u8], "image/jpeg")).await;
        }
        assert!(!cache.contains("k0").await);
        assert!(!cache.contains("k1").await);
        assert!(cache.contains("k5").await);
    }

    #[tokio::test]
    async fn abaisser_le_reglage_reprend_la_memoire_des_la_prochaine_insertion() {
        // Le reglage est relu **a chaque insertion** : l'abaisser ne doit pas
        // attendre un redemarrage pour rendre la memoire, sinon le regler ne
        // sert a rien tant que l'appareil plays.
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { entries: 10, ..CoverSettings::default() });
        for i in 0..10 {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![i as u8], "image/jpeg")).await;
        }
        assert!(cache.contains("k0").await, "prealable : les dix tiennent");

        cache.set_cover_settings(CoverSettings { entries: 3, ..CoverSettings::default() });
        cache.insert("neuve".into(), CoverPayload::Bytes(vec![99], "image/jpeg")).await;

        assert!(cache.contains("neuve").await);
        assert!(cache.contains("k9").await, "les plus recentes restent");
        assert!(!cache.contains("k0").await, "les plus anciennes partent tout de suite");
        assert!(!cache.contains("k7").await);
    }

    /// L'éviction hors bornes doit reprendre l'espace des fichiers
    /// temporaires d'extraction qu'elle fait sortir du cache — sinon rien
    /// d'autre ne les efface jamais pendant la vie du processus — mais ne
    /// doit **jamais** toucher un `folder.jpg` déclaré par une Source, qui
    /// vit sur son propre partage.
    #[tokio::test]
    async fn l_eviction_supprime_un_fichier_temporaire_a_nous_mais_jamais_un_folder_jpg_de_source() {
        // Nom unique garanti par `tempfile`, dans le vrai répertoire
        // temporaire système : c'est là, et seulement là, qu'
        // `is_cover_temp` reconnaît un fichier comme étant à
        // nous. Un name aléatoire évite toute collision avec les fichiers que
        // d'autres tests de ce même binaire y écrivent en parallèle (voir
        // `player::mpv::tests`).
        let notre_fichier = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .suffix(".jpg")
            .tempfile_in(std::env::temp_dir())
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        // Un `folder.jpg` de Source vit ailleurs, jamais dans le répertoire
        // temporaire système : simulé ici dans un répertoire à lui.
        let dir_source = tempfile::tempdir().unwrap();
        let folder_jpg = dir_source.path().join("folder.jpg");
        std::fs::write(&folder_jpg, b"x").unwrap();

        let cache = CoverCache::new();
        // Borne posee explicitement : la valeur par defaut est de vingt
        // entries, ce que ce test ne veut pas avoir a remplir.
        cache.set_cover_settings(CoverSettings { entries: 4, ..CoverSettings::default() });
        cache.insert("a-garder".into(), CoverPayload::File(folder_jpg.clone())).await;
        cache.insert("notre".into(), CoverPayload::File(notre_fichier.clone())).await;
        // Assez d'insertions pour dépasser la bounded et évincer les deux
        // premières.
        for i in 0..4u8 {
            cache.insert(format!("k{i}"), CoverPayload::Bytes(vec![i], "image/jpeg")).await;
        }

        assert!(!notre_fichier.exists(), "un fichier temporaire a nous, evince, doit etre supprime du disque");
        assert!(folder_jpg.exists(), "un folder.jpg de Source ne doit jamais etre supprime de son propre chef");
    }

    #[test]
    fn purge_temporaires_efface_les_fichiers_a_nous_mais_rien_dautre() {
        let dir = tempfile::tempdir().unwrap();
        let a_nous = dir.path().join(format!("{TEMP_PREFIX}abcd1234.jpg"));
        let pas_a_nous = dir.path().join("folder.jpg");
        std::fs::write(&a_nous, b"x").unwrap();
        std::fs::write(&pas_a_nous, b"y").unwrap();

        purge_temp_files_in(dir.path());

        assert!(!a_nous.exists(), "un fichier a nous, reste d'une execution precedente, doit disparaitre");
        assert!(pas_a_nous.exists(), "un fichier qui n'est pas a nous ne doit jamais etre touche");
    }

    #[tokio::test]
    async fn un_fichier_local_qui_n_est_pas_une_image_est_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let faux = dir.path().join("folder.jpg");
        std::fs::write(&faux, b"ceci n'est pas une image").unwrap();
        let r = CoverRef::Path { path: faux.to_string_lossy().into_owned() };
        assert!(
            fetch(&r).await.is_none(),
            "les bytes d'en-tete doivent etre verifies : sans cela, un contributeur mal ecrit \
             ferait serve n'importe quel fichier du systeme sur une route HTTP publique"
        );

        let vrai = dir.path().join("cover.jpg");
        // En-tete JPEG minimal : SOI + marqueur APP0.
        std::fs::write(&vrai, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = CoverRef::Path { path: vrai.to_string_lossy().into_owned() };
        match fetch(&r).await {
            Some(CoverPayload::File(p)) => assert_eq!(p, vrai),
            autre => panic!("une image locale doit rester un path, pas des bytes : {autre:?}"),
        }
    }

    // -- `bytes` : la matérialisation pour le protocol d'affichage ---------

    /// Le cap de source des réglages par défaut.
    ///
    /// Les tests d'`bytes` ci-dessous portent sur le cap, pas sur le rendition :
    /// le passer explicitement rend visible dans le test la bounded qu'il éprouve,
    /// là où elle était cachée dans une constante de module. Le prendre des
    /// réglages **par défaut** plutôt que de `COVER_MAX_BYTES` en direct est
    /// délibéré : c'est la valeur qu'un appareil sorti d'usine applique
    /// réellement.
    fn cap() -> usize {
        CoverSettings::default().source_max
    }

    /// En-tête JPEG minimal, suivi de `remplissage` bytes quelconques.
    ///
    /// **Indécodable exprès** : ces bytes valident l'en-tête que `image_type`
    /// inspecte, et rien de plus. Cela convient à tout ce qui porte sur les
    /// tailles et les plafonds, et cela ne convient **pas** à ce qui porte sur
    /// le rendition — voir `image_reelle`, plus bas.
    fn jpeg(remplissage: usize) -> Vec<u8> {
        let mut v = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        v.resize(6 + remplissage, 0x42);
        v
    }

    #[tokio::test]
    async fn octets_rend_les_octets_dune_pochette_reseau_avec_son_mime() {
        let cache = CoverCache::new();
        let image = jpeg(10);
        cache.insert("k".into(), CoverPayload::Bytes(image.clone(), "image/png")).await;
        assert_eq!(cache.bytes("k", cap()).await, Some(("image/png", image)));
        assert_eq!(cache.bytes("inconnue", cap()).await, None);
    }

    #[tokio::test]
    async fn octets_lit_un_fichier_local_que_la_route_aurait_diffuse_en_flux() {
        // La différence avec `cover_get` : ici les bytes sont matérialisés,
        // parce que pousser sur un socket n'offre pas d'autre choix.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let image = jpeg(1000);
        std::fs::write(&path, &image).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path)).await;
        assert_eq!(cache.bytes("k", cap()).await, Some(("image/jpeg", image)));
    }

    #[tokio::test]
    async fn octets_revalide_len_tete_sur_les_octets_quil_rend() {
        // `fetch` a validé l'en-tête à la découverte, mais entre les deux le
        // partage n'est pas sous le contrôle de l'appareil. Comme la route HTTP,
        // cette playback-ci ne fait donc pas confiance à la découverte — et elle
        // va plus loin : le contenu vérifié **est** le contenu rendition, une seule
        // playback sur un seul descripteur, donc il n'y a aucune fenêtre entre
        // la vérification et l'usage.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, jpeg(10)).unwrap();
        let r = CoverRef::Path { path: path.to_string_lossy().into_owned() };
        let Some(p) = fetch(&r).await else { panic!("une image locale doit etre acceptee") };
        let cache = CoverCache::new();
        cache.insert("k".into(), p).await;

        // Quelqu'un remplace le contenu du partage après la découverte.
        std::fs::write(&path, b"ceci n'est plus une image").unwrap();
        assert_eq!(
            cache.bytes("k", cap()).await,
            None,
            "les bytes rendus doivent etre ceux qui ont ete valides, jamais un contenu suppose"
        );
    }

    #[tokio::test]
    async fn octets_refuse_un_fichier_local_au_dela_du_plafond_et_accepte_le_plafond_pile() {
        // Le cap du transport, éprouvé sur sa bounded exacte. Le local n'a par
        // conception **aucune** limite de size (voir `fetch`) : c'est donc
        // ici, et nulle part ailleurs, que la bounded existe. Un refus, pas une
        // allocation de la size du fichier — la playback s'arrête à
        // `COVER_MAX_BYTES + 1` bytes, quelle que soit la size réelle.
        let cap = cap();
        let dir = tempfile::tempdir().unwrap();

        let pile = dir.path().join("pile.jpg");
        std::fs::write(&pile, jpeg(cap - 6)).unwrap();
        let cache = CoverCache::new();
        cache.insert("pile".into(), CoverPayload::File(pile)).await;
        match cache.bytes("pile", cap).await {
            Some((mime, o)) => {
                assert_eq!(mime, "image/jpeg");
                assert_eq!(o.len(), cap, "le cap pile doit passer, pas etre refuse");
            }
            None => panic!("une image de exactement COVER_MAX_BYTES doit passer"),
        }

        let trop = dir.path().join("trop.jpg");
        std::fs::write(&trop, jpeg(cap - 5)).unwrap();
        cache.insert("trop".into(), CoverPayload::File(trop)).await;
        assert_eq!(
            cache.bytes("trop", cap).await,
            None,
            "un seul octet au-dela du cap doit suffire a refuser"
        );
    }

    #[tokio::test]
    async fn octets_rend_none_sur_un_fichier_disparu() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(dir.path().join("absent.jpg"))).await;
        assert_eq!(cache.bytes("k", cap()).await, None);
    }

    /// Prouve que le refus vient de `metadata`, appelé **avant** toute playback
    /// des bytes — pas de la playback bornée à `COVER_MAX_BYTES + 1` bytes
    /// qui reste en filet plus loin dans `read_file_bounded`. Un test qui se
    /// contenterait de vérifier le `None` ne distinguerait pas les deux : la
    /// playback bornée refuse tout aussi bien. La preuve tient dans le
    /// journal : il doit nommer la size **réelle** du fichier, très au-delà
    /// de `COVER_MAX_BYTES + 1` — un nombre que la playback bornée ne pourrait
    /// jamais rendre, puisqu'elle ne read jamais plus que cette bounded.
    #[tokio::test]
    async fn le_plafond_est_verifie_sur_la_taille_du_fichier_avant_toute_lecture() {
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Tampon(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Tampon {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Tampon {
            type Writer = Tampon;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trop-gros.png");
        // Bien au-dela de COVER_MAX_BYTES + 1 : une playback bornee a cette
        // limite ne pourrait jamais journaliser un nombre pareil. File
        // creux (`set_len`) : aucune ecriture reelle des bytes, seul le
        // metadata doit y suffire.
        let taille_reelle = ritornello_proto::COVER_MAX_BYTES as u64 + 50_000_000;
        let fichier = std::fs::File::create(&path).unwrap();
        fichier.set_len(taille_reelle).unwrap();
        drop(fichier);

        let buffer = Tampon::default();
        // `#[tokio::test]` est mono-thread par defaut : le repartiteur pose
        // par thread reste donc valide a travers le `.await` qui suit.
        let subscriber = tracing_subscriber::fmt().with_writer(buffer.clone()).with_ansi(false).finish();
        let garde = tracing::subscriber::set_default(subscriber);
        let resultat = read_file_bounded(&path, cap()).await;
        drop(garde);

        assert!(resultat.is_none(), "un fichier bien au-dela du cap doit etre refuse");
        let journal = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(
            journal.contains(&taille_reelle.to_string()),
            "le journal doit nommer la size reelle du fichier, connue par metadata avant toute playback : {journal}"
        );
    }

    // -- `line` : la trame de cover, relue a chaque appel ---------------

    #[tokio::test]
    async fn ligne_relit_le_fichier_donc_une_image_remplacee_sous_le_meme_chemin_est_servie_neuve() {
        // **Le defaut le plus grave de la passe, a la maille du cache.** La key
        // hache le path, pas le contenu : rien dans le cache ne peut voir
        // qu'un `folder.jpg` a ete remplace sur le partage. Tant que `line`
        // relit le fichier a chaque appel, ce n'est pas un probleme ; une line
        // encodee mise en memoire, elle, servait pour toujours l'image d'avant.
        //
        // Le scenario reel est en trois clics : desactiver l'afficheur depuis la
        // page d'admin, remplacer l'image, le reactiver. Le relais rebranche
        // redemande la cover courante — donc `line` avec la **meme key** —
        // et personne n'a insert quoi que ce soit entre-temps. C'est pourquoi ce
        // test n'appelle pas `insert` une seconde fois : une invalidation posee
        // dans `insert` ne couvrirait pas ce path-la.
        //
        // Deux images **decodables**, et petites : sous 640 px et sous le
        // cap de sortie, le rendition par defaut les laisse passer telles
        // quelles (voir `rendition`, etape 3). Les bytes servis sont donc ceux du
        // fichier, ce qui garde a ce test l'assertion la plus tranchante
        // possible sur la fraicheur — et documente le passe-droit au passage.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, fixtures::jpeg_decodable(48, 48)).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path.clone())).await;

        let avant = cache.line("k", "/api/cover/k").await.expect("une image locale doit produire une line");
        // L'utilisateur remplace la cover sur le partage. Dimensions
        // differentes, pour que l'inegalite ne tienne pas au seul remplissage.
        std::fs::write(&path, fixtures::jpeg_decodable(64, 64)).unwrap();
        let apres = cache.line("k", "/api/cover/k").await.expect("le second appel doit reussir aussi");

        assert_ne!(
            &*avant, &*apres,
            "apres remplacement du fichier sous la meme key, la line servie doit etre la nouvelle"
        );
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&apres).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(
                    c.bytes,
                    fixtures::jpeg_decodable(64, 64),
                    "les bytes servis doivent etre ceux du fichier actuel"
                );
            }
            autre => panic!("une trame de cover etait attendue : {autre:?}"),
        }
    }

    #[tokio::test]
    async fn ligne_change_quand_la_cle_change_et_reste_une_trame_de_pochette_valide() {
        // Le pendant du test ci-dessus : deux cles distinctes designent deux
        // images distinctes, et chacune doit rendre la sienne.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        std::fs::write(&a, fixtures::jpeg_decodable(48, 48)).unwrap();
        std::fs::write(&b, fixtures::jpeg_decodable(64, 64)).unwrap();
        let cache = CoverCache::new();
        cache.insert("a".into(), CoverPayload::File(a)).await;
        cache.insert("b".into(), CoverPayload::File(b)).await;

        let ligne_a = cache.line("a", "/api/cover/a").await.unwrap();
        let ligne_b = cache.line("b", "/api/cover/b").await.unwrap();
        assert_ne!(&*ligne_a, &*ligne_b, "deux cles differentes doivent produire des lines differentes");

        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&ligne_a).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(c.href, "/api/cover/a");
                assert_eq!(c.mime, "image/jpeg");
            }
            autre => panic!("une trame de cover etait attendue : {autre:?}"),
        }
    }

    #[tokio::test]
    async fn la_route_rend_404_sur_une_cle_inconnue() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app
            .oneshot(Request::get("/api/cover/inexistante").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -- allowed_target : le garde-fou SSRF, sans aucun réseau -------------

    #[test]
    fn cible_autorisee_rejette_une_ip_litterale_a_point_final() {
        // `!l.is_empty()` du parseur de `ritornello-proto` échoue sur le
        // libellé clear final, ce qui fait *passer* cette forme là-bas.
        // `Url::domain()` s'appuie sur l'hôte réellement résolu par le
        // navigateur/reqwest, qui la normalise en IPv4.
        assert!(!allowed_target("https://192.168.1.1./a.jpg"));
    }

    #[test]
    fn cible_autorisee_rejette_une_ip_litterale_en_hexadecimal() {
        assert!(!allowed_target("https://0x7f.0.0.1/a.jpg"));
    }

    #[test]
    fn cible_autorisee_rejette_localhost_faute_de_point() {
        assert!(!allowed_target("https://localhost/a.jpg"));
    }

    #[test]
    fn cible_autorisee_rejette_une_adresse_ipv6_litterale() {
        assert!(!allowed_target("https://[::1]/a.jpg"));
    }

    #[test]
    fn cible_autorisee_accepte_un_vrai_nom_dhote_https() {
        assert!(allowed_target("https://coverartarchive.org/x/front-500"));
    }

    // -- download : les trois garde-fous réseau, contre un vrai serveur ---
    //
    // `download` et non `fetch` : `allowed_target` refuse justement
    // `127.0.0.1`, donc passer par `fetch` empêcherait ces tests
    // d'atteindre le code qu'ils veulent exercer.

    /// Sérialise un corps en `Transfer-Encoding: chunked`.
    fn corps_chunked(morceaux: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for m in morceaux {
            out.extend_from_slice(format!("{:x}\r\n", m.len()).as_bytes());
            out.extend_from_slice(m);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    fn reponse_http(entetes: &str, corps: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("HTTP/1.1 200 OK\r\n{entetes}\r\n").as_bytes());
        out.extend_from_slice(&corps);
        out
    }

    /// Sert `reponse` à la première connexion reçue sur `127.0.0.1`, sur un
    /// port choisi par l'OS. `fermer` ferme la connexion juste après l'avoir
    /// écrite ; sinon la connexion reste ouverte, sans plus jamais rien
    /// send_frame — ce qui permet à un test de prouver qu'un appelant n'a pas
    /// tenté de read au-delà de ce qui a été servi.
    async fn sert(reponse: Vec<u8>, fermer: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut ignore = [0u8; 4096];
                let _ = socket.read(&mut ignore).await;
                let _ = socket.write_all(&reponse).await;
                if fermer {
                    let _ = socket.shutdown().await;
                } else {
                    // Ne ferme jamais : si l'appelant read malgré tout le
                    // corps, il restera bloqué jusqu'au timeout du test.
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            }
        });
        format!("http://127.0.0.1:{port}/cover.jpg")
    }

    #[tokio::test]
    async fn le_plafond_reseau_coupe_un_flux_par_morceaux_avant_la_fin() {
        // Aucun `Content-Length` (réponse `chunked`) : rien à quoi se fier
        // sinon la size effectivement reçue, track après track.
        let mut premier = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        premier.resize(900_000, 0);
        let second = vec![0u8; 900_000];
        let troisieme = vec![0u8; 900_000]; // total ~2,7 Mo, au-dela du cap de 2 Mo
        let corps = corps_chunked(&[premier, second, troisieme]);
        let reponse =
            reponse_http("Content-Type: image/jpeg\r\nTransfer-Encoding: chunked\r\n", corps);
        let url = sert(reponse, true).await;
        assert!(
            download(&url).await.is_none(),
            "le cap doit couper le stream track par track, sans attendre la fin"
        );
    }

    #[tokio::test]
    async fn un_content_type_refuse_ne_lit_jamais_le_corps() {
        // Le serveur n'envoie jamais le corps qu'il announcement : si `download`
        // le lisait malgré le content-type refusé, cette attente resterait
        // bloquée jusqu'au timeout ci-dessous.
        let reponse = reponse_http("Content-Type: text/html\r\nContent-Length: 1000000\r\n", Vec::new());
        let url = sert(reponse, false).await;
        match tokio::time::timeout(std::time::Duration::from_secs(2), download(&url)).await {
            Ok(None) => {}
            Ok(Some(p)) => panic!("content-type refuse mais une cover a ete produite : {p:?}"),
            Err(_) => panic!(
                "timeout : le corps a ete lu (ou son attente entamee) malgre le content-type refuse"
            ),
        }
    }

    /// Sert une redirection vers `cible`, puis ne répond plus rien : si le
    /// client suivait le saut, il tenterait de joindre `cible` — ce que
    /// l'assertion du test constate par l'échec de la requête entière.
    fn reponse_redirection(cible: &str) -> Vec<u8> {
        format!("HTTP/1.1 302 Found\r\nLocation: {cible}\r\nContent-Length: 0\r\n\r\n").into_bytes()
    }

    #[tokio::test]
    async fn une_redirection_vers_une_ip_litterale_est_refusee() {
        // Le garde-fou SSRF ne valait que pour l'URL de depart : l'hote
        // d'image — un tiers, puisque le `coverUrl` d'OUI FM est ecrit par
        // autrui — n'avait qu'a repondre `302` vers une adresse du LAN pour
        // faire emettre a l'appareil un GET dessus, changement de schema
        // compris. Un saut d'indirection annulait tout le controle.
        //
        // `192.0.2.1` (bloc de documentation RFC 5737) plutot que la passerelle
        // du reseau de developpement : si la politique laissait passer, ce test
        // doit echouer sans avoir joint quoi que ce soit de reel.
        for cible in [
            "http://192.0.2.1/a.jpg",
            "https://192.0.2.1/a.jpg",
            // Meme forme d'hote qu'une cible legitime, mais le schema retombe
            // en clair : refuse aussi. Domaine en `.invalid`, qui ne resout
            // jamais — aucun test d'ici ne touche le reseau.
            "http://coverartarchive.invalid/a.jpg",
        ] {
            let url = sert(reponse_redirection(cible), true).await;
            // Le timeout n'est pas une hypothese de rythme mais un detecteur de
            // panne, comme dans le test du content-type ci-dessus : si le saut
            // etait suivi, l'attente d'un hote injoignable durerait jusqu'au
            // timeout de dix secondes du client et rendrait `None` — donc un
            // test vert pour la mauvaise raison.
            match tokio::time::timeout(std::time::Duration::from_secs(2), download(&url)).await {
                Ok(None) => {}
                Ok(Some(p)) => panic!("redirection suivie vers {cible:?} : {p:?}"),
                Err(_) => panic!("le saut vers {cible:?} a ete tente : la politique doit le refuser"),
            }
        }
    }

    #[tokio::test]
    async fn un_corps_qui_nest_pas_une_image_est_refuse_malgre_le_content_type() {
        let corps = b"ceci n'est pas une image".to_vec();
        let reponse = reponse_http(
            &format!("Content-Type: image/png\r\nContent-Length: {}\r\n", corps.len()),
            corps,
        );
        let url = sert(reponse, true).await;
        assert!(
            download(&url).await.is_none(),
            "le contenu declare `image/png` mais les bytes recus ne le sont pas : doit etre refuse"
        );
    }
    // -- Le rendition : ce que le cœur fabrique avant de pousser ----------------

    /// Un `Rendition` dont chaque champ est nommé par le test qui s'en sert : les
    /// défauts du produit (640 px, 512 Kio, 16 Mpx) rendraient la plupart des
    /// cas inatteignables sans fabriquer des images énormes.
    fn rendu_de_test(max_edge_px: u32, output_cap: usize, pixel_cap: u64) -> Rendition {
        Rendition { max_edge_px, jpeg_quality: 85, output_cap, pixel_cap }
    }

    #[tokio::test]
    async fn huit_afficheurs_qui_demandent_la_meme_pochette_ne_la_construisent_quune_fois() {
        // Le rendez-vous. Deux afficheurs abonnés reçoivent la **même** trame
        // d'état, donc demandent la même cover dans le même instant, et
        // décodaient puis réencodaient deux fois la même image — plusieurs
        // centaines de millisecondes de cœur en double sur un Pi 2.
        //
        // La preuve est un **décompte d'exécutions**, et il n'y en a pas
        // d'autre : comparer les trames rendues ne dirait rien, deux
        // builds successives de la même image produisant des bytes
        // identiques.
        let cache = Arc::new(CoverCache::new());
        // Un rendition qui a vraiment du travail : 600 × 400 dépasse le côté
        // maximal, donc l'image est décodée et réencodée pour de bon. Sans
        // cela, le passe-droit rendrait la source telle quelle et le premier
        // arrivé finirait sans jamais suspendre — aucun suiveur n'aurait le
        // temps d'arriver, et le test passerait sans rien prouver.
        cache.set_cover_settings(CoverSettings {
            entries: 20,
            source_max: 8 * 1024 * 1024,
            rendition: Some(rendu_de_test(64, 512 * 1024, 16_000_000)),
        });
        cache
            .insert("k".into(), CoverPayload::Bytes(fixtures::jpeg_decodable(600, 400), "image/jpeg"))
            .await;

        let taches: Vec<_> = (0..8)
            .map(|_| {
                let c = cache.clone();
                tokio::spawn(async move { c.line("k", "/api/cover/k").await })
            })
            .collect();
        let mut trames = Vec::new();
        for t in taches {
            trames.push(t.await.expect("aucune tache ne doit paniquer"));
        }

        assert!(trames.iter().all(|t| t.is_some()), "les huit doivent recevoir une trame");
        let premiere = trames[0].as_deref().unwrap();
        assert!(
            trames.iter().all(|t| t.as_deref() == Some(premiere)),
            "les huit doivent recevoir la meme trame"
        );
        assert_eq!(
            cache.builds(),
            1,
            "une seule construction pour huit demandes concurrentes de la meme key"
        );
    }

    #[tokio::test]
    async fn le_rendez_vous_ne_retient_rien_une_fois_la_trame_construite() {
        // Le rendez-vous n'est **pas** un cache, et c'est ce qui le rend
        // acceptable : la clé hache le *path*, pas le contenu, donc une trame
        // retenue deviendrait fausse dès que l'utilisateur remplace l'image sous
        // ce path. Une `OnceCell` gardant sa valeur pour toujours, tout tient
        // au retrait de l'entrée.
        let cache = Arc::new(CoverCache::new());
        cache.set_cover_settings(CoverSettings {
            entries: 20,
            source_max: 8 * 1024 * 1024,
            rendition: Some(rendu_de_test(64, 512 * 1024, 16_000_000)),
        });
        cache
            .insert("k".into(), CoverPayload::Bytes(fixtures::jpeg_decodable(600, 400), "image/jpeg"))
            .await;

        assert!(cache.line("k", "/api/cover/k").await.is_some());
        assert!(
            cache.in_flight.lock().await.is_empty(),
            "la table des builds en cours doit etre clear apres coup"
        );

        // Et la seconde demande **reconstruit**, au lieu d'être servie par une
        // cellule restée en place.
        assert!(cache.line("k", "/api/cover/k").await.is_some());
        assert_eq!(
            cache.builds(),
            2,
            "deux demandes separees dans le temps doivent produire deux builds"
        );
    }

    #[tokio::test]
    async fn une_image_deja_petite_part_telle_quelle_sans_etre_reencodee() {
        // Le passe-droit. L'identité **binaire** est l'assertion qui compte :
        // un aller-retour décodage/encodage produirait des bytes différents
        // même à dimensions égales, donc l'égalité prouve qu'aucun n'a eu lieu.
        let source = fixtures::jpeg_decodable(64, 64);
        let sortie = rendition("image/jpeg", source.clone(), rendu_de_test(640, 512 * 1024, 16_000_000))
            .await
            .expect("une petite image doit passer");
        assert_eq!(sortie, ("image/jpeg", source));
    }

    #[tokio::test]
    async fn une_image_trop_grande_est_reduite_en_gardant_son_rapport() {
        // 300 × 150, réduite à un côté de 100 : le rapport 2:1 doit survivre.
        // Vérifié en **décodant la sortie**, pas en croyant le code sur parole.
        let source = fixtures::jpeg_decodable(300, 150);
        let (mime, sortie) = rendition("image/jpeg", source.clone(), rendu_de_test(100, 512 * 1024, 16_000_000))
            .await
            .expect("une grande image doit etre reduite, pas refusee");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(dimensions(&sortie), Some((100, 50)), "le rapport 2:1 doit etre conserve");
        assert!(
            sortie.len() < source.len(),
            "une thumbnail de 100x50 doit peser moins que son original de 300x150 : {} contre {}",
            sortie.len(),
            source.len()
        );
    }

    #[tokio::test]
    async fn une_image_a_canal_alpha_est_reencodee_en_png_sans_perte() {
        // Aplatir la transparence demanderait de choisir une couleur de fond,
        // parti pris visuel que l'appareil n'a pas à prendre. Le mime change,
        // donc la trame poussée le déclare — un afficheur qui reçoit
        // `image/jpeg` avec des bytes PNG afficherait un carré cassé.
        let source = fixtures::png_alpha(300, 300);
        let (mime, sortie) = rendition("image/png", source, rendu_de_test(100, 512 * 1024, 16_000_000))
            .await
            .expect("un png alpha doit etre rendition");
        assert_eq!(mime, "image/png", "le mime doit suivre le format reellement produit");
        assert_eq!(dimensions(&sortie), Some((100, 100)));
    }

    /// **La garde anti-bombe, et son order.**
    ///
    /// L'image de ce test franchirait le passe-droit sans difficulté : 100 px de
    /// côté sous les 640 autorisés, deux kilooctets sous le cap de sortie.
    /// Seule la garde de pixels la refuse. Le test échoue donc si la garde
    /// disparaît **et** si elle est déplacée après le passe-droit — c'est ce
    /// second cas qui compte, parce qu'une bombe est précisément une image
    /// minuscule en bytes et démesurée en pixels.
    #[tokio::test]
    async fn le_plafond_de_pixels_refuse_avant_tout_decodage_et_avant_le_passe_droit() {
        let source = fixtures::jpeg_decodable(100, 100);
        assert!(
            source.len() < 512 * 1024,
            "la fixture doit tenir sous le cap de sortie, sinon le test ne prouve pas l'order"
        );
        assert_eq!(
            rendition("image/jpeg", source, rendu_de_test(640, 512 * 1024, 1_000)).await,
            None,
            "10000 pixels au-dela d'un cap de 1000 doivent etre refuses"
        );
    }

    #[tokio::test]
    async fn une_vignette_au_dela_du_filet_de_sortie_nest_pas_poussee() {
        // Le filet, éprouvé sur un cap volontairement minuscule : une
        // thumbnail 200 × 200 d'un dégradé ne tient pas dans 200 bytes.
        let source = fixtures::jpeg_decodable(400, 400);
        assert_eq!(
            rendition("image/jpeg", source, rendu_de_test(200, 200, 16_000_000)).await,
            None,
            "une thumbnail au-dela du filet ne doit pas etre pushed"
        );
    }

    #[tokio::test]
    async fn linterrupteur_decoche_pousse_la_source_sans_la_decoder() {
        // Deux propriétés en une, et la fixture est l'astuce : ces bytes ont un
        // en-tête JPEG valide mais un contenu **indécodable**. S'ils arrivent
        // intacts au bout de `line`, c'est que le décodeur n'a pas été appelé
        // du tout — pas seulement que son résultat a été ignoré.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let source = jpeg(1000);
        std::fs::write(&path, &source).unwrap();
        let cache = CoverCache::new();
        cache.set_cover_settings(CoverSettings { entries: 20, source_max: cap(), rendition: None });
        cache.insert("k".into(), CoverPayload::File(path)).await;

        let line = cache.line("k", "/api/cover/k").await.expect("la source doit partir telle quelle");
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&line).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(c.bytes, source, "les bytes pousses doivent etre ceux de la source");
                assert_eq!(c.mime, "image/jpeg");
            }
            autre => panic!("une trame de cover etait attendue : {autre:?}"),
        }
    }

    #[tokio::test]
    async fn linterrupteur_coche_ecarte_une_image_dont_les_octets_ne_se_decodent_pas() {
        // Le pendant du test ci-dessus, et un **changement de comportement**
        // assumé : `image_type` ne read que les bytes magiques, donc un fichier
        // tronqué passait cette validation et partait vers les afficheurs, qui
        // montraient chacun un carré cassé à leur façon. Le rendition le tranche une
        // fois pour tous, au centre.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        std::fs::write(&path, jpeg(1000)).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path)).await;
        assert!(
            cache.line("k", "/api/cover/k").await.is_none(),
            "un fichier dont l'en-tete ment sur son contenu ne doit pas etre push_cover"
        );
    }

    #[tokio::test]
    async fn les_reglages_du_produit_reencodent_une_grande_pochette() {
        // Le path de production complet, avec les défauts et sans les
        // paramétrer : une cover 1000 × 1000 doit arriver en 640 px.
        // Sans ce test, tous les autres pourraient passer avec des réglages
        // qu'aucun appareil n'applique.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folder.jpg");
        let source = fixtures::jpeg_decodable(1000, 1000);
        std::fs::write(&path, &source).unwrap();
        let cache = CoverCache::new();
        cache.insert("k".into(), CoverPayload::File(path)).await;

        let line = cache.line("k", "/api/cover/k").await.expect("une cover doit etre pushed");
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&line).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(
                    dimensions(&c.bytes),
                    Some((640, 640)),
                    "les settings par defaut doivent ramener le cote long a 640 px"
                );
                assert!(
                    c.bytes.len() < source.len(),
                    "la thumbnail doit peser moins que la source : {} contre {}",
                    c.bytes.len(),
                    source.len()
                );
            }
            autre => panic!("une trame de cover etait attendue : {autre:?}"),
        }
    }

    #[test]
    fn les_reglages_traduisent_linterrupteur_en_absence_de_rendu() {
        // La conversion `Settings -> CoverSettings`, qui est le seul endroit où
        // l'interrupteur devient une structure. `None` plutôt qu'un booléen
        // porté à côté : c'est ce qui rend impossible de read `max_edge_px`
        // sans avoir vérifié d'abord que le rendition est active.
        let mut s = crate::state::Settings::default();
        assert!(CoverSettings::from(&s).rendition.is_some(), "le defaut du produit reencode");

        s.cover_rendition = false;
        assert!(CoverSettings::from(&s).rendition.is_none());
        assert_eq!(
            CoverSettings::from(&s).source_max,
            20 * 1024 * 1024,
            "le cap de source survit a l'interrupteur : c'est sa raison d'etre"
        );
    }
}
