//! La pochette de ce qui joue : la chercher, la retenir, la servir.
//!
//! C'est **l'appareil** qui va chercher l'image, jamais le navigateur. Trois
//! raisons : la page ne doit charger aucune ressource externe — principe déjà
//! posé pour les pages d'admin ; l'image devient disponible à un futur
//! afficheur graphique ; et une pochette embarquée dans un fichier, que seul
//! l'appareil peut lire, n'aurait aucune URL à donner au navigateur.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ritornello_proto::CoverRef;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::RwLock;
use tokio_util::io::ReaderStream;

/// Plafond d'une image venue du réseau. Écarte le `front` nu du Cover Art
/// Archive, mesuré à 2 670 705 octets là où `front-500` en rend 75 249.
const PLAFOND_RESEAU: usize = 2 * 1024 * 1024;

/// Nombre d'entrées retenues : la pochette courante et quelques précédentes.
const ENTREES: usize = 4;

/// Préfixe de l'URL locale publiée dans `Morceau::cover_href`.
///
/// Partagé entre `metadata::Metadonnees::etat`, qui la **fabrique**, et
/// `main::relais_afficheur`, qui la **relit** pour retrouver la clé du cache :
/// deux littéraux auraient pu diverger en silence, et la conséquence aurait été
/// un afficheur qui ne reçoit plus jamais de pochette, sans erreur nulle part.
pub const PREFIXE_HREF: &str = "/api/cover/";

/// Préfixe des fichiers temporaires d'extraction de pochette embarquée,
/// posés dans `std::env::temp_dir()` par `player::mpv::pochette_embarquee`.
///
/// Partagé entre ce module (purge au démarrage, éviction bornée) et `mpv.rs`
/// (nommage) : les deux doivent reconnaître exactement les mêmes fichiers,
/// sous peine soit de ne jamais les purger, soit — pire — de purger un
/// fichier qui n'est pas de nous.
pub const PREFIXE_TEMPORAIRE: &str = "ritornello-cover-";

/// Vrai si `chemin` est un fichier temporaire d'extraction créé par ce
/// processus.
///
/// **Jamais** vrai pour un `folder.jpg` déclaré par une Source : celui-là vit
/// sur le partage de l'utilisateur, et le cœur ne doit jamais le supprimer de
/// son propre chef. `Pochette::Fichier` porte les deux formes (voir sa doc),
/// c'est ici que la distinction se fait avant d'agir sur le disque.
fn est_temporaire_de_pochette(chemin: &std::path::Path) -> bool {
    chemin.parent() == Some(std::env::temp_dir().as_path())
        && chemin.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIXE_TEMPORAIRE))
}

/// Balaie les fichiers temporaires d'une exécution précédente. Appelée une
/// fois au démarrage, avant que quoi que ce soit ne puisse en créer de
/// nouveaux.
///
/// **Ce n'est pas une garde de fraîcheur** : `pochette_embarquee` réécrit de
/// toute façon entièrement le fichier avant d'en renvoyer la référence, donc
/// rien n'est jamais servi sans être passé par une extraction fraîche de
/// **cette** exécution — un fichier resté d'une exécution précédente ne
/// pourrait de toute façon jamais être adopté tel quel (voir sa doc). Le
/// seul problème que cette purge résout est l'accumulation : rien d'autre
/// n'efface ces fichiers entre deux démarrages, et sur un Pi `std::env::
/// temp_dir()` est souvent une `tmpfs` — ce qui s'accumule y grignote de la
/// RAM, pas seulement de l'espace disque. Sans risque ici : le cache ne
/// survit jamais à un redémarrage (`CoverCache` est reconstruit à chaque
/// lancement), donc rien de ce qui traîne encore ici ne peut être référencé
/// par quoi que ce soit.
pub fn purge_temporaires() {
    purge_temporaires_dans(&std::env::temp_dir());
}

/// Cœur testable de `purge_temporaires`, paramétré par le répertoire à
/// balayer.
///
/// `std::env::temp_dir()` est **partagé** par tout le système, et par les
/// autres tests de ce même binaire, qui y écrivent de vrais fichiers
/// `ritornello-cover-*` pour éprouver l'extraction elle-même (voir
/// `player::mpv::tests`) : y lancer un vrai balayage depuis un test le
/// mettrait en concurrence avec eux. Séparée pour qu'un test puisse pointer
/// vers un répertoire à lui, entièrement isolé.
fn purge_temporaires_dans(dir: &std::path::Path) {
    let Ok(entrees) = std::fs::read_dir(dir) else { return };
    for entree in entrees.flatten() {
        let nom = entree.file_name();
        if nom.to_str().is_some_and(|n| n.starts_with(PREFIXE_TEMPORAIRE)) {
            if let Err(e) = std::fs::remove_file(entree.path()) {
                tracing::debug!("purging leftover cover file {}: {e}", entree.path().display());
            }
        }
    }
}

/// Ce que le cœur retient d'une pochette.
///
/// Deux natures, et c'est délibéré : une pochette **locale** n'entre pas en
/// mémoire. Un `folder.jpg` de trois mégaoctets est banal sur un NAS, et le
/// charger en RAM sur un Pi pour une image que le navigateur cachera de son
/// côté serait du gaspillage.
#[derive(Debug, Clone)]
pub enum Pochette {
    /// Venue du réseau : les octets sont en mémoire.
    Octets(Vec<u8>, &'static str),
    /// Locale : seul le chemin est retenu, la route relit le fichier.
    Fichier(PathBuf),
}

/// Empreinte de la source, publiée dans l'URL locale.
///
/// `DefaultHasher` et non `sha2` : une collision ferait afficher la mauvaise
/// pochette et rien d'autre, ce qui ne justifie pas une dépendance
/// cryptographique. Calculable **avant** le téléchargement, ce qui permet de
/// dédupliquer deux demandes pour la même image.
pub fn cle(r: &CoverRef) -> String {
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

#[derive(Default)]
pub struct CoverCache {
    entrees: RwLock<VecDeque<(String, Pochette)>>,
}

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insere(&self, cle: String, p: Pochette) {
        let mut e = self.entrees.write().await;
        e.retain(|(k, _)| k != &cle);
        e.push_back((cle, p));
        while e.len() > ENTREES {
            let Some((_, evincee)) = e.pop_front() else { break };
            // Borne l'accumulation **pendant** la vie du processus, pas
            // seulement au démarrage (voir `purge_temporaires`) : une session
            // qui tourne des mois et parcourt une grande bibliothèque ne doit
            // pas laisser un fichier par piste distincte jamais rejouée. Ne
            // touche jamais un `folder.jpg` de Source, qui n'est pas à nous.
            if let Pochette::Fichier(chemin) = &evincee {
                if est_temporaire_de_pochette(chemin) {
                    if let Err(err) = tokio::fs::remove_file(chemin).await {
                        tracing::debug!("purging evicted cover file {}: {err}", chemin.display());
                    }
                }
            }
        }
    }

    pub async fn contient(&self, cle: &str) -> bool {
        self.entrees.read().await.iter().any(|(k, _)| k == cle)
    }

    async fn lit(&self, cle: &str) -> Option<Pochette> {
        self.entrees.read().await.iter().find(|(k, _)| k == cle).map(|(_, p)| p.clone())
    }

    /// Matérialise les octets d'une pochette : `(mime, octets)`.
    ///
    /// **Ce que la route HTTP évite justement de faire.** Elle, pour un fichier
    /// local, ouvre, vérifie l'en-tête et *diffuse en flux* sans jamais tenir
    /// l'image entière. Pousser sur un socket n'en laisse pas le choix, d'où
    /// cette méthode — et d'où le plafond, qui n'existait pas côté local (voir
    /// `COVER_MAX_BYTES` et la doc de `recupere`).
    ///
    /// `None` couvre indistinctement : clé inconnue, fichier disparu ou
    /// illisible, partage qui ne répond pas, contenu qui n'est plus une image,
    /// et **taille au-delà du plafond**. L'appelant n'a rien à en distinguer :
    /// dans tous les cas l'afficheur n'a pas d'image, comme il n'en a pas quand
    /// la récupération échoue.
    pub async fn octets(&self, cle: &str) -> Option<(&'static str, Vec<u8>)> {
        // Le verrou est rendu **avant** toute IO. Une pochette locale vit
        // couramment sur un partage endormi : tenir le verrou de lecture
        // pendant `DELAI_FICHIER` bloquerait les insertions du cache, donc la
        // tâche détachée de `Core::lance_pochette`, pour une image.
        //
        // La branche `Octets` répond sous le verrou plutôt que de passer par
        // `lit` : celui-ci clone la `Pochette` entière, ce qui ferait deux
        // copies des octets au lieu d'une.
        let chemin = {
            let e = self.entrees.read().await;
            match e.iter().find(|(k, _)| k == cle).map(|(_, p)| p) {
                None => return None,
                // Déjà en mémoire, et déjà borné par construction : ces
                // octets viennent d'un corps HTTP que `telecharge` a coupé à
                // `PLAFOND_RESEAU`.
                Some(Pochette::Octets(v, mime)) => return Some((*mime, v.clone())),
                Some(Pochette::Fichier(c)) => c.clone(),
            }
        };
        lit_fichier_borne(&chemin).await
    }
}

/// Lit un fichier de pochette pour le pousser, borné et validé.
///
/// **La validation d'en-tête est faite sur les octets rendus eux-mêmes**, et
/// non sur une première lecture séparée. La route HTTP, elle, ne peut pas :
/// elle doit vérifier puis diffuser, donc elle prend soin de garder le *même
/// descripteur* entre les deux lectures — sans quoi un contributeur pourrait
/// remplacer le contenu du partage entre la vérification et le service. Ici le
/// contenu vérifié **est** le contenu rendu, un seul descripteur et une seule
/// lecture : la fenêtre n'existe pas du tout, plutôt que d'être fermée. La
/// garantie n'est donc pas affaiblie mais renforcée.
///
/// Deux bornes, et les deux comptent :
///
/// * `COVER_MAX_BYTES + 1` octets au plus sont lus. Le `+ 1` est ce qui permet
///   de *savoir* qu'on a dépassé sans avoir tout lu : au-delà, refus. Un PNG
///   de 150 Mo sur le partage — cas que `cover_get` cite comme réel — coûte
///   donc 2 Mio, pas 150.
/// * `DELAI_FICHIER`, comme partout où ce module touche un fichier : le
///   partage peut être endormi, et l'attente doit être bornée par nous plutôt
///   que par le noyau.
async fn lit_fichier_borne(chemin: &std::path::Path) -> Option<(&'static str, Vec<u8>)> {
    let lecture = tokio::time::timeout(DELAI_FICHIER, async {
        let fichier = tokio::fs::File::open(chemin).await?;
        let mut octets = Vec::new();
        // `take` **avant** `read_to_end` : `read_to_end` seul lirait le
        // fichier entier, et le contrôle de taille arriverait après
        // l'allocation qu'il est censé éviter.
        fichier
            .take(ritornello_proto::COVER_MAX_BYTES as u64 + 1)
            .read_to_end(&mut octets)
            .await?;
        Ok::<_, std::io::Error>(octets)
    })
    .await;
    let octets = match lecture {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::debug!("cover file unreadable: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("cover file {} did not answer in {DELAI_FICHIER:?}", chemin.display());
            return None;
        }
    };
    if octets.len() > ritornello_proto::COVER_MAX_BYTES {
        tracing::warn!(
            "cover file {} not pushed: over {} bytes",
            chemin.display(),
            ritornello_proto::COVER_MAX_BYTES
        );
        return None;
    }
    let mime = type_image(&octets)?;
    Some((mime, octets))
}

/// Octets d'en-tête d'une image reconnue. Vérifiés avant de servir un fichier
/// local : sans cela, un contributeur mal écrit ferait servir n'importe quel
/// fichier du système sur une route HTTP publique.
fn type_image(octets: &[u8]) -> Option<&'static str> {
    if octets.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if octets.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if octets.len() >= 12 && &octets[0..4] == b"RIFF" && &octets[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Nombre de sauts de redirection tolérés, la valeur par défaut de `reqwest`.
///
/// Reprise explicitement : remplacer la politique par défaut par une
/// politique personnalisée fait aussi perdre son plafond, et une chaîne de
/// redirections sans fin est un déni de service à un aller-retour de coût.
const SAUTS_MAX: usize = 10;

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
/// vérifier : `cible_autorisee` ne s'appliquait qu'à l'URL de départ, donc
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
                if saut.previous().len() >= SAUTS_MAX {
                    return saut.stop();
                }
                if cible_autorisee(saut.url().as_str()) {
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
/// littérale pour un nom d'hôte devant le découpage de chaînes de
/// `ritornello-proto`. `Url::domain()` s'appuie sur l'analyse WHATWG déjà
/// faite par `reqwest` (ré-exportée, donc aucune dépendance de plus) : elle
/// classe l'hôte en IPv4/IPv6 **après** normalisation, quelle qu'ait été sa
/// graphie d'origine, et ne renvoie `Some` que pour un vrai nom de domaine.
///
/// `ritornello-proto` garde la forme (https, extension) ; ce module-ci garde
/// la cible : c'est lui qui émet la requête, et c'est le SSE d'une source
/// tierce (OUI FM, par exemple) qui peut fournir l'URL.
///
/// Appliquée à **chaque** cible atteinte, pas seulement à la première :
/// `recupere` filtre l'URL de départ, la politique de redirection de
/// `client()` filtre tous les sauts suivants.
fn cible_autorisee(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    if u.scheme() != "https" {
        return false;
    }
    match u.domain() {
        // `None` couvre aussi bien l'absence d'hôte qu'une adresse IP
        // littérale (v4 ou v6) : `domain()` ne renvoie `Some` que pour un
        // nom de domaine, jamais pour un `HostInternal::Ipv4`/`Ipv6`.
        Some(d) => d.contains('.'),
        None => false,
    }
}

/// Effectue la requête et applique les trois garde-fous réseau : le
/// `Content-Type`, le plafond appliqué en lisant par morceaux, et les octets
/// magiques du corps reçu. Séparée de `recupere` pour rester testable contre
/// un serveur HTTP local (`127.0.0.1`) sans jamais passer par
/// `cible_autorisee`, qui refuserait justement cette adresse.
async fn telecharge(url: &str) -> Option<Pochette> {
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
    let mut octets = Vec::new();
    while let Some(morceau) = reponse.chunk().await.ok()? {
        if octets.len() + morceau.len() > PLAFOND_RESEAU {
            tracing::debug!("cover fetch refused: over {PLAFOND_RESEAU} bytes");
            return None;
        }
        octets.extend_from_slice(&morceau);
    }
    let mime = type_image(&octets)?;
    Some(Pochette::Octets(octets, mime))
}

/// Délai accordé à un accès au fichier image lui-même — ouverture,
/// `metadata`, premiers octets.
///
/// **Le même que celui de l'extraction embarquée** (`sante::DELAI`), et pour
/// la même raison : ces deux chemins-ci touchent des fichiers qui vivent
/// couramment sur un partage SMB endormi, et ce projet a déjà vécu la panne
/// qu'une IO qui n'aboutit pas provoque. Aucune boucle d'événements n'est
/// retenue ici — la récupération est détachée, la route HTTP est une tâche par
/// requête — donc l'audio ne risque rien ; ce qui est borné, c'est l'attente
/// elle-même, plutôt que de la laisser durer aussi longtemps que le noyau
/// voudra.
///
/// Une borne de temps et non `Sante` : le disjoncteur prend une **fermeture
/// bloquante** (`spawn_blocking`), là où ces deux chemins sont déjà
/// asynchrones et, dans le cas de `cover_get`, doivent rendre un
/// `tokio::fs::File` à diffuser en flux. L'y faire entrer demanderait de
/// repasser en `std::fs` puis de reconvertir, et de câbler le disjoncteur
/// jusque dans l'`AppState` HTTP — une refonte pour une propriété que la
/// borne donne déjà. Ce que `Sante` apporterait en plus, et qu'on n'a donc
/// pas ici, est la mémoire du montage muet : un fil du pool bloquant reste
/// perdu par tentative, exactement ce que `sante.rs` documente comme
/// inévitable une fois le noyau parti.
const DELAI_FICHIER: std::time::Duration = crate::sante::DELAI;

/// Va chercher la pochette. `None` = échec, et l'échec est **silencieux** :
/// l'appareil n'affiche simplement pas d'image.
pub async fn recupere(r: &CoverRef) -> Option<Pochette> {
    match r {
        CoverRef::Path { path } => {
            let chemin = PathBuf::from(path);
            let a_lire = chemin.clone();
            // Ouverture **et** première lecture sous la même borne : c'est
            // l'ouverture qui bloque sur un partage endormi, mais un partage
            // qui répond à l'`open` et plus au `read` est le cas d'une
            // déconnexion en cours — les deux doivent être couverts.
            let reconnu = tokio::time::timeout(DELAI_FICHIER, async move {
                let mut fichier = tokio::fs::File::open(&a_lire).await.ok()?;
                let mut tete = [0u8; 12];
                let lus = fichier.read(&mut tete).await.ok()?;
                type_image(&tete[..lus])
            })
            .await;
            match reconnu {
                Ok(Some(_)) => {}
                Ok(None) => return None,
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {DELAI_FICHIER:?}", chemin.display());
                    return None;
                }
            }
            // Le plafond ne s'applique pas au local : il protège d'un tiers sur
            // le réseau, et un fichier du NAS est de confiance. Ses octets
            // d'en-tête ont été vérifiés, c'est ce qui compte. La route relira
            // le fichier au moment de servir : entre les deux, le partage n'est
            // plus sous le contrôle de l'appareil (voir `cover_get`).
            Some(Pochette::Fichier(chemin))
        }
        CoverRef::Url { url } => {
            if !cible_autorisee(url) {
                tracing::debug!("cover fetch refused: target not allowed");
                return None;
            }
            telecharge(url).await
        }
    }
}

/// ETag d'un fichier local : contrairement à la clé du cache — qui hache la
/// **source** (le chemin), pas le contenu — ce fichier reste modifiable après
/// coup sur son partage. L'ETag doit donc suivre le contenu, pas seulement le
/// chemin, sans quoi une requête conditionnelle validerait indéfiniment une
/// image que l'utilisateur a pourtant remplacée.
fn etag_fichier(modifie: Option<std::time::SystemTime>, taille: u64) -> String {
    let nanos = modifie
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("\"{nanos:x}-{taille:x}\"")
}

/// `GET /api/cover/{clé}`. La clé est une empreinte de la **source**, donc son
/// immuabilité ne dit rien du contenu : une pochette réseau est bien figée
/// sous sa clé (elle vient d'un corps déjà entièrement vérifié), mais un
/// fichier local reste modifiable sur son partage après coup.
pub async fn cover_get(
    State(state): State<crate::status::AppState>,
    Path(cle): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(p) = state.covers.lit(&cle).await else {
        return (StatusCode::NOT_FOUND, "inconnue").into_response();
    };
    match p {
        Pochette::Octets(octets, mime) => (
            [
                (header::CONTENT_TYPE, mime.to_string()),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
                (header::ETAG, format!("\"{cle}\"")),
            ],
            octets,
        )
            .into_response(),
        Pochette::Fichier(chemin) => {
            // Ouverture et `metadata` sous une borne de temps : ce fichier
            // vit couramment sur un partage réseau, et cette route est
            // joignable par n'importe quel navigateur du LAN. Sans borne, un
            // partage endormi retenait la requête aussi longtemps que le
            // noyau le voulait — l'incident même que `sante.rs` existe pour
            // borner. L'expiration est traitée comme l'illisibilité qui
            // existait déjà : un 404, que l'IHM rend par son repli ♫.
            //
            // Bornée **en deux temps**, l'en-tête juste en dessous : garder la
            // réponse 304 avant toute lecture du corps est ce qui rend une
            // requête conditionnelle réellement bon marché.
            let ouverture = tokio::time::timeout(DELAI_FICHIER, async {
                let fichier = tokio::fs::File::open(&chemin).await?;
                let meta = fichier.metadata().await?;
                Ok::<_, std::io::Error>((fichier, meta))
            })
            .await;
            let (mut fichier, meta) = match ouverture {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    tracing::debug!("cover file unreadable: {e}");
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {DELAI_FICHIER:?}", chemin.display());
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
            };
            let etag = etag_fichier(meta.modified().ok(), meta.len());
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str())
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            // Revalidation des octets d'en-tête au moment de servir, et non
            // seulement au moment de la découverte (`recupere`) : entre les
            // deux, le partage n'est pas sous le contrôle de l'appareil, et un
            // contributeur qui remplacerait le contenu ne doit pas voir servir
            // n'importe quoi sous cette route publique. Même descripteur de
            // fichier pour la vérification et pour le flux servi ensuite : le
            // contenu ne peut pas changer entre les deux lectures.
            //
            // Seconde borne, sur la lecture cette fois : un partage qui
            // répond à l'`open` et plus au premier `read` est le cas d'une
            // déconnexion en cours, et rien ne l'écarterait sans cela.
            let entete = tokio::time::timeout(DELAI_FICHIER, async {
                let mut tete = [0u8; 12];
                let lus = fichier.read(&mut tete).await?;
                fichier.seek(std::io::SeekFrom::Start(0)).await?;
                Ok::<_, std::io::Error>((tete, lus))
            })
            .await;
            let (tete, lus) = match entete {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    tracing::debug!("cover file unreadable: {e}");
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
                Err(_) => {
                    tracing::warn!("cover file {} did not answer in {DELAI_FICHIER:?}", chemin.display());
                    return (StatusCode::NOT_FOUND, "illisible").into_response();
                }
            };
            let Some(mime) = type_image(&tete[..lus]) else {
                tracing::debug!("cover file is no longer an image: {}", chemin.display());
                return (StatusCode::NOT_FOUND, "illisible").into_response();
            };
            // En flux, pas en un `Vec` unique : cette route est joignable sans
            // authentification depuis le LAN, et un fichier local n'a par
            // conception aucun plafond de taille. Un PNG de 150 Mo sur le
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

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::CoverRef;

    #[test]
    fn la_cle_est_stable_et_distingue_deux_sources() {
        let a = CoverRef::Url { url: "https://x.org/a.jpg".into() };
        let b = CoverRef::Url { url: "https://x.org/b.jpg".into() };
        assert_eq!(cle(&a), cle(&a), "la cle doit etre stable : elle est publiee dans une URL");
        assert_ne!(cle(&a), cle(&b));
        // Une forme differente pour la meme chaine ne doit pas collisionner.
        assert_ne!(cle(&a), cle(&CoverRef::Path { path: "/https://x.org/a.jpg".into() }));
        // Hexadecimal, donc sans surprise dans un chemin d'URL.
        assert!(cle(&a).chars().all(|c| c.is_ascii_hexdigit()), "{}", cle(&a));
    }

    #[tokio::test]
    async fn le_cache_est_borne_et_oublie_la_plus_ancienne() {
        let cache = CoverCache::new();
        for i in 0..6 {
            cache.insere(format!("k{i}"), Pochette::Octets(vec![i as u8], "image/jpeg")).await;
        }
        // Quatre entrees : la pochette courante et quelques precedentes. Un Pi
        // n'a pas a garder plus, et rien ne survit au redemarrage.
        assert!(!cache.contient("k0").await);
        assert!(!cache.contient("k1").await);
        assert!(cache.contient("k5").await);
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
        // `est_temporaire_de_pochette` reconnaît un fichier comme étant à
        // nous. Un nom aléatoire évite toute collision avec les fichiers que
        // d'autres tests de ce même binaire y écrivent en parallèle (voir
        // `player::mpv::tests`).
        let notre_fichier = tempfile::Builder::new()
            .prefix(PREFIXE_TEMPORAIRE)
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
        cache.insere("a-garder".into(), Pochette::Fichier(folder_jpg.clone())).await;
        cache.insere("notre".into(), Pochette::Fichier(notre_fichier.clone())).await;
        // Assez d'insertions pour dépasser `ENTREES` et évincer les deux
        // premières.
        for i in 0..4u8 {
            cache.insere(format!("k{i}"), Pochette::Octets(vec![i], "image/jpeg")).await;
        }

        assert!(!notre_fichier.exists(), "un fichier temporaire a nous, evince, doit etre supprime du disque");
        assert!(folder_jpg.exists(), "un folder.jpg de Source ne doit jamais etre supprime de son propre chef");
    }

    #[test]
    fn purge_temporaires_efface_les_fichiers_a_nous_mais_rien_dautre() {
        let dir = tempfile::tempdir().unwrap();
        let a_nous = dir.path().join(format!("{PREFIXE_TEMPORAIRE}abcd1234.jpg"));
        let pas_a_nous = dir.path().join("folder.jpg");
        std::fs::write(&a_nous, b"x").unwrap();
        std::fs::write(&pas_a_nous, b"y").unwrap();

        purge_temporaires_dans(dir.path());

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
            recupere(&r).await.is_none(),
            "les octets d'en-tete doivent etre verifies : sans cela, un contributeur mal ecrit \
             ferait servir n'importe quel fichier du systeme sur une route HTTP publique"
        );

        let vrai = dir.path().join("cover.jpg");
        // En-tete JPEG minimal : SOI + marqueur APP0.
        std::fs::write(&vrai, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap();
        let r = CoverRef::Path { path: vrai.to_string_lossy().into_owned() };
        match recupere(&r).await {
            Some(Pochette::Fichier(p)) => assert_eq!(p, vrai),
            autre => panic!("une image locale doit rester un chemin, pas des octets : {autre:?}"),
        }
    }

    // -- `octets` : la matérialisation pour le protocole d'affichage ---------

    /// En-tête JPEG minimal, suivi de `remplissage` octets quelconques.
    fn jpeg(remplissage: usize) -> Vec<u8> {
        let mut v = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        v.resize(6 + remplissage, 0x42);
        v
    }

    #[tokio::test]
    async fn octets_rend_les_octets_dune_pochette_reseau_avec_son_mime() {
        let cache = CoverCache::new();
        let image = jpeg(10);
        cache.insere("k".into(), Pochette::Octets(image.clone(), "image/png")).await;
        assert_eq!(cache.octets("k").await, Some(("image/png", image)));
        assert_eq!(cache.octets("inconnue").await, None);
    }

    #[tokio::test]
    async fn octets_lit_un_fichier_local_que_la_route_aurait_diffuse_en_flux() {
        // La différence avec `cover_get` : ici les octets sont matérialisés,
        // parce que pousser sur un socket n'offre pas d'autre choix.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        let image = jpeg(1000);
        std::fs::write(&chemin, &image).unwrap();
        let cache = CoverCache::new();
        cache.insere("k".into(), Pochette::Fichier(chemin)).await;
        assert_eq!(cache.octets("k").await, Some(("image/jpeg", image)));
    }

    #[tokio::test]
    async fn octets_revalide_len_tete_sur_les_octets_quil_rend() {
        // `recupere` a validé l'en-tête à la découverte, mais entre les deux le
        // partage n'est pas sous le contrôle de l'appareil. Comme la route HTTP,
        // cette lecture-ci ne fait donc pas confiance à la découverte — et elle
        // va plus loin : le contenu vérifié **est** le contenu rendu, une seule
        // lecture sur un seul descripteur, donc il n'y a aucune fenêtre entre
        // la vérification et l'usage.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        std::fs::write(&chemin, jpeg(10)).unwrap();
        let r = CoverRef::Path { path: chemin.to_string_lossy().into_owned() };
        let Some(p) = recupere(&r).await else { panic!("une image locale doit etre acceptee") };
        let cache = CoverCache::new();
        cache.insere("k".into(), p).await;

        // Quelqu'un remplace le contenu du partage après la découverte.
        std::fs::write(&chemin, b"ceci n'est plus une image").unwrap();
        assert_eq!(
            cache.octets("k").await,
            None,
            "les octets rendus doivent etre ceux qui ont ete valides, jamais un contenu suppose"
        );
    }

    #[tokio::test]
    async fn octets_refuse_un_fichier_local_au_dela_du_plafond_et_accepte_le_plafond_pile() {
        // Le plafond du transport, éprouvé sur sa borne exacte. Le local n'a par
        // conception **aucune** limite de taille (voir `recupere`) : c'est donc
        // ici, et nulle part ailleurs, que la borne existe. Un refus, pas une
        // allocation de la taille du fichier — la lecture s'arrête à
        // `COVER_MAX_BYTES + 1` octets, quelle que soit la taille réelle.
        let plafond = ritornello_proto::COVER_MAX_BYTES;
        let dir = tempfile::tempdir().unwrap();

        let pile = dir.path().join("pile.jpg");
        std::fs::write(&pile, jpeg(plafond - 6)).unwrap();
        let cache = CoverCache::new();
        cache.insere("pile".into(), Pochette::Fichier(pile)).await;
        match cache.octets("pile").await {
            Some((mime, o)) => {
                assert_eq!(mime, "image/jpeg");
                assert_eq!(o.len(), plafond, "le plafond pile doit passer, pas etre refuse");
            }
            None => panic!("une image de exactement COVER_MAX_BYTES doit passer"),
        }

        let trop = dir.path().join("trop.jpg");
        std::fs::write(&trop, jpeg(plafond - 5)).unwrap();
        cache.insere("trop".into(), Pochette::Fichier(trop)).await;
        assert_eq!(
            cache.octets("trop").await,
            None,
            "un seul octet au-dela du plafond doit suffire a refuser"
        );
    }

    #[tokio::test]
    async fn octets_rend_none_sur_un_fichier_disparu() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CoverCache::new();
        cache.insere("k".into(), Pochette::Fichier(dir.path().join("absent.jpg"))).await;
        assert_eq!(cache.octets("k").await, None);
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

    // -- cible_autorisee : le garde-fou SSRF, sans aucun réseau -------------

    #[test]
    fn cible_autorisee_rejette_une_ip_litterale_a_point_final() {
        // `!l.is_empty()` du parseur de `ritornello-proto` échoue sur le
        // libellé vide final, ce qui fait *passer* cette forme là-bas.
        // `Url::domain()` s'appuie sur l'hôte réellement résolu par le
        // navigateur/reqwest, qui la normalise en IPv4.
        assert!(!cible_autorisee("https://192.168.1.1./a.jpg"));
    }

    #[test]
    fn cible_autorisee_rejette_une_ip_litterale_en_hexadecimal() {
        assert!(!cible_autorisee("https://0x7f.0.0.1/a.jpg"));
    }

    #[test]
    fn cible_autorisee_rejette_localhost_faute_de_point() {
        assert!(!cible_autorisee("https://localhost/a.jpg"));
    }

    #[test]
    fn cible_autorisee_rejette_une_adresse_ipv6_litterale() {
        assert!(!cible_autorisee("https://[::1]/a.jpg"));
    }

    #[test]
    fn cible_autorisee_accepte_un_vrai_nom_dhote_https() {
        assert!(cible_autorisee("https://coverartarchive.org/x/front-500"));
    }

    // -- telecharge : les trois garde-fous réseau, contre un vrai serveur ---
    //
    // `telecharge` et non `recupere` : `cible_autorisee` refuse justement
    // `127.0.0.1`, donc passer par `recupere` empêcherait ces tests
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
    /// envoyer — ce qui permet à un test de prouver qu'un appelant n'a pas
    /// tenté de lire au-delà de ce qui a été servi.
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
                    // Ne ferme jamais : si l'appelant lit malgré tout le
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
        // sinon la taille effectivement reçue, morceau après morceau.
        let mut premier = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        premier.resize(900_000, 0);
        let second = vec![0u8; 900_000];
        let troisieme = vec![0u8; 900_000]; // total ~2,7 Mo, au-dela du plafond de 2 Mo
        let corps = corps_chunked(&[premier, second, troisieme]);
        let reponse =
            reponse_http("Content-Type: image/jpeg\r\nTransfer-Encoding: chunked\r\n", corps);
        let url = sert(reponse, true).await;
        assert!(
            telecharge(&url).await.is_none(),
            "le plafond doit couper le flux morceau par morceau, sans attendre la fin"
        );
    }

    #[tokio::test]
    async fn un_content_type_refuse_ne_lit_jamais_le_corps() {
        // Le serveur n'envoie jamais le corps qu'il annonce : si `telecharge`
        // le lisait malgré le content-type refusé, cette attente resterait
        // bloquée jusqu'au timeout ci-dessous.
        let reponse = reponse_http("Content-Type: text/html\r\nContent-Length: 1000000\r\n", Vec::new());
        let url = sert(reponse, false).await;
        match tokio::time::timeout(std::time::Duration::from_secs(2), telecharge(&url)).await {
            Ok(None) => {}
            Ok(Some(p)) => panic!("content-type refuse mais une pochette a ete produite : {p:?}"),
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
            // Le delai n'est pas une hypothese de rythme mais un detecteur de
            // panne, comme dans le test du content-type ci-dessus : si le saut
            // etait suivi, l'attente d'un hote injoignable durerait jusqu'au
            // timeout de dix secondes du client et rendrait `None` — donc un
            // test vert pour la mauvaise raison.
            match tokio::time::timeout(std::time::Duration::from_secs(2), telecharge(&url)).await {
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
            telecharge(&url).await.is_none(),
            "le contenu declare `image/png` mais les octets recus ne le sont pas : doit etre refuse"
        );
    }
}
