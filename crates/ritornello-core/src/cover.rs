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
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
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

/// Une trame de pochette en cours de construction, partagée entre l'appelant
/// qui la construit et ceux qui l'attendent.
///
/// L'`Option` extérieure est celle de `ligne` — « rien à pousser », pour les
/// mêmes raisons que partout dans ce module ; l'`Arc<str>` intérieur est la
/// ligne de texte déjà sérialisée. La cellule est derrière un `Arc` pour que les
/// attendants la tiennent après avoir rendu le verrou de la table.
type TrameEnVol = Arc<tokio::sync::OnceCell<Option<Arc<str>>>>;

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

/// Ce que le cœur fabrique d'une pochette avant de la pousser sur un socket.
///
/// Absent (`Reglages::rendu` à `None`) quand l'utilisateur a décoché le
/// réencodage : les octets d'origine partent tels quels. Un `Option` plutôt
/// qu'un booléen à l'intérieur, et ce n'est pas cosmétique — les quatre
/// réglages n'existent que là où ils veulent dire quelque chose, si bien qu'un
/// code qui lit `cote_max_px` ne peut pas oublier de vérifier d'abord que le
/// rendu est actif.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rendu {
    /// Côté le plus long de la vignette, en pixels. Le rapport est conservé.
    pub cote_max_px: u32,
    /// Qualité JPEG, 1 à 100. Ignorée pour une image à canal alpha, réencodée
    /// en PNG sans perte.
    pub qualite_jpeg: u8,
    /// Plafond de la vignette produite, en octets. Un filet : au-delà, rien
    /// n'est poussé.
    pub plafond_sortie: usize,
    /// Plafond de pixels à décoder. Comparé aux dimensions lues dans l'en-tête
    /// **avant toute allocation**, et reporté dans `image::Limits` pour le cas
    /// d'un en-tête qui mentirait sur ses propres dimensions.
    pub plafond_pixels: u64,
}

/// Les deux étages du traitement d'une pochette, qu'il ne faut pas confondre.
///
/// `source_max` borne ce que le cœur accepte de **lire**, quoi qu'il arrive
/// ensuite : c'est la seule garde qui subsiste quand le rendu est désactivé, et
/// la plus économique de toutes, puisqu'elle se juge sur la taille du fichier
/// sans lire un octet de son contenu.
///
/// `rendu` ne décrit que ce que le cœur **fabrique**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reglages {
    /// Plafond de la pochette source, en octets.
    pub source_max: usize,
    /// `None` = pousser la source telle quelle.
    pub rendu: Option<Rendu>,
}

impl Default for Reglages {
    /// Les défauts du produit, pas des défauts neutres : un `CoverCache::new()`
    /// se comporte comme un appareil sorti d'usine, y compris dans les tests
    /// qui ne parlent pas de réglages. Dérivés de `state::Settings::default()`
    /// pour qu'il n'existe qu'un seul endroit où ces valeurs sont écrites.
    fn default() -> Self {
        Self::from(&crate::state::Settings::default())
    }
}

impl From<&crate::state::Settings> for Reglages {
    fn from(s: &crate::state::Settings) -> Self {
        Self {
            source_max: (s.cover_source_max_mio as usize) * 1024 * 1024,
            rendu: s.cover_rendition.then(|| Rendu {
                cote_max_px: s.cover_max_edge_px,
                qualite_jpeg: s.cover_jpeg_quality,
                plafond_sortie: (s.cover_max_bytes_ko as usize) * 1024,
                plafond_pixels: (s.cover_max_pixels_mpx as u64) * 1_000_000,
            }),
        }
    }
}

#[derive(Default)]
pub struct CoverCache {
    entrees: RwLock<VecDeque<(String, Pochette)>>,
    /// Réglages vivants, relus à chaque publication.
    ///
    /// Un verrou `std::sync` et non celui de `tokio`, à la différence de
    /// `entrees` juste au-dessus : la section critique est la copie d'une
    /// structure `Copy` de trente octets, jamais une IO. Cela garde
    /// `Core::set_settings` synchrone — le rendre `async` pour ce champ aurait
    /// contaminé sa signature et tous ses appelants de test. La valeur est
    /// **copiée hors du verrou** avant tout `await` : aucun garde ne traverse
    /// un point de suspension.
    reglages: std::sync::RwLock<Reglages>,
    /// Les constructions de trame en cours, une entrée par clé.
    ///
    /// **Un rendez-vous, pas un cache — la distinction est tout.** Mémoriser une
    /// trame serait faux pour la raison que dit la doc de `rendu` : la clé hache
    /// le *chemin*, pas le contenu, donc une vignette gardée deviendrait fausse
    /// dès que l'utilisateur remplace l'image sous ce chemin. Une entrée d'ici ne
    /// survit pas à sa construction : le dernier appelant à en sortir la retire,
    /// et l'appelant suivant repart d'une lecture neuve du fichier.
    ///
    /// Ce que cela économise : deux afficheurs abonnés qui reçoivent la même
    /// trame d'état demandent la même pochette dans le même instant, et
    /// décodaient puis réencodaient deux fois la même image. Sur un Pi 2, c'est
    /// un cœur occupé plusieurs centaines de millisecondes en double.
    ///
    /// `tokio::sync::OnceCell::get_or_init` **est** le rendez-vous : le premier
    /// arrivé exécute, les suivants attendent son résultat. La cellule est
    /// derrière un `Arc` pour que les suiveurs la tiennent après avoir rendu le
    /// verrou de la table — le verrou ne couvre jamais le travail, seulement
    /// l'inscription.
    en_vol: tokio::sync::Mutex<HashMap<String, TrameEnVol>>,
    /// Combien de constructions de trame ont **réellement** été exécutées.
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
    /// coût, mais un champ que personne ne lit est une dette.
    #[cfg(test)]
    constructions: std::sync::atomic::AtomicUsize,
}

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Combien de fois une trame a été construite depuis la création du cache.
    #[cfg(test)]
    pub(crate) fn constructions(&self) -> usize {
        self.constructions.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Publie de nouveaux réglages. Prise en compte à la publication suivante :
    /// rien n'est mémorisé, donc il n'y a rien à invalider.
    pub fn set_reglages(&self, r: Reglages) {
        // Un verrou empoisonné voudrait dire qu'un porteur a paniqué en tenant
        // trente octets `Copy` — impossible sans un défaut ailleurs. Écraser
        // plutôt que propager : des réglages perdus dégraderaient la publication
        // suivante en silence, là où l'empoisonnement, lui, se voit au journal
        // de la panique d'origine.
        match self.reglages.write() {
            Ok(mut g) => *g = r,
            Err(e) => *e.into_inner() = r,
        }
    }

    /// Copie des réglages courants, verrou rendu immédiatement.
    fn reglages(&self) -> Reglages {
        match self.reglages.read() {
            Ok(g) => *g,
            Err(e) => *e.into_inner(),
        }
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
    /// Le plafond est **passé par l'appelant** plutôt que relu ici, pour que
    /// `ligne` ne lise les réglages qu'une seule fois : deux lectures pourraient
    /// encadrer un changement, et produire une vignette selon des règles qui
    /// n'ont jamais coexisté.
    async fn octets(&self, cle: &str, plafond: usize) -> Option<(&'static str, Vec<u8>)> {
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
                //
                // Le plafond réglable est vérifié quand même : il peut être
                // descendu **sous** `PLAFOND_RESEAU`, et alors la borne de
                // construction ne dit plus rien. Sans ce contrôle, le réglage
                // ne vaudrait que pour les fichiers locaux — vrai aujourd'hui
                // par la seule coïncidence des deux valeurs, et faux dès qu'on
                // y touche.
                Some(Pochette::Octets(v, mime)) => {
                    if v.len() > plafond {
                        tracing::warn!(
                            "network cover not pushed: {} bytes over the {plafond}-byte limit",
                            v.len()
                        );
                        return None;
                    }
                    return Some((*mime, v.clone()));
                }
                Some(Pochette::Fichier(c)) => c.clone(),
            }
        };
        lit_fichier_borne(&chemin, plafond).await
    }

    /// Construit la ligne de protocole `DisplayFrame::Cover` pour `cle`/`href` :
    /// le JSON complet, base64 compris, terminé par un saut de ligne, prêt à
    /// être écrit tel quel sur un socket.
    ///
    /// **Construite à chaque appel, jamais mémorisée, et c'est la propriété qui
    /// compte.** Une ligne encodée retenue d'un appel sur l'autre a été essayée
    /// ici, puis retirée : la clé du cache hache le *chemin*, pas le contenu, si
    /// bien qu'une ligne gardée devenait fausse dès que l'utilisateur remplaçait
    /// l'image sous ce chemin. Et le geste qui y menait tient en trois clics —
    /// désactiver l'afficheur depuis la page d'admin, remplacer le `folder.jpg`,
    /// le réactiver : le relais rebranché repart avec sa garde de déduplication
    /// à zéro (`main::relais_afficheur`, `SuiviPochette`), redemande la
    /// pochette courante, et recevait la ligne d'avant. Rien ne l'invalidait
    /// parce que rien ne *pouvait* l'invalider : remplacer un fichier sur un
    /// partage ne passe par aucun code à nous. Une image visiblement fausse est
    /// le pire des défauts de cet appareil, très au-dessus d'un pic mémoire.
    ///
    /// **Le partage reste souhaitable, mais structurel plutôt que mémorisé.**
    /// L'économie visée — payer une fois par *publication* la matérialisation
    /// des octets et leur base64, jusqu'à `COVER_MAX_BYTES`, plutôt qu'une fois
    /// par relais abonné — s'obtient en construisant la ligne **au moment de la
    /// publication** et en donnant le même `Arc` à chaque relais. C'est une
    /// refonte à part entière : la construction lit un fichier, elle ne peut
    /// donc pas s'installer sur la boucle principale du cœur. Et il n'y avait
    /// rien à gagner à l'anticiper par un memo, parce qu'en service il n'avait
    /// **aucun** appelant second à servir : `wants_covers` est faux par défaut,
    /// un seul greffon le redéfinit, et `relais_afficheur` n'appelle cette
    /// fonction qu'une fois par changement de `cover_href`. Le greffon MPD ne
    /// repasse pas non plus par ici pour servir ses tranches de 8 Kio — il garde
    /// sa propre copie de la trame reçue.
    ///
    /// **Jamais d'`Arc` dans un type sérialisé** : ce qui voyage derrière l'`Arc`
    /// rendu est la ligne de texte déjà produite par `serde_json`, pas une valeur
    /// `ritornello_proto::Cover` — ce type-là reste un type de fil ordinaire,
    /// sans partage à exprimer. L'`Arc` sert à `DisplayClient::send_cover_line`,
    /// qui écrit ces octets tels quels plutôt que de recopier et réencoder.
    ///
    /// `None` couvre les mêmes cas que `octets` : rien à pousser.
    pub async fn ligne(&self, cle: &str, href: &str) -> Option<Arc<str>> {
        // Inscription au rendez-vous. Le verrou de la table ne couvre que
        // l'inscription elle-même — jamais la construction, qui lit un fichier
        // et occupe un cœur. Le tenir pendant le travail sérialiserait des clés
        // *différentes*, ce qui est le contraire du but.
        let cellule = {
            let mut en_vol = self.en_vol.lock().await;
            en_vol.entry(cle.to_string()).or_insert_with(TrameEnVol::default).clone()
        };

        // `href` n'a pas besoin d'être comparé entre appelants : `cle` en est
        // dérivée (`relais_afficheur` la tire de `href` par
        // `strip_prefix(PREFIXE_HREF)`), donc deux appelants de même clé
        // portent la même chaîne. Un suiveur reçoit bien la trame du premier
        // arrivé, et elle décrit la même image sous le même nom.
        let resultat = cellule
            .get_or_init(|| async {
                #[cfg(test)]
                self.constructions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Une seule lecture des réglages pour les deux étages : voir
                // `octets_bornes`. Deux lectures pourraient encadrer un
                // changement, et produire une vignette selon des règles qui
                // n'ont jamais coexisté.
                let reglages = self.reglages();
                let (mime, octets) = self.octets(cle, reglages.source_max).await?;
                // Le rendu s'applique **ici et pas dans `octets`**, donc sur le
                // seul chemin de poussée. La route HTTP `cover_get`, elle,
                // diffuse le fichier local en flux sans jamais le tenir en
                // entier : lui imposer un réencodage lui ferait perdre
                // exactement la propriété qui la rend économique, pour une image
                // que le navigateur redimensionne et met en cache de son côté.
                let (mime, octets) = match reglages.rendu {
                    None => (mime, octets),
                    Some(r) => rendu(mime, octets, r).await?,
                };
                let cover = ritornello_proto::Cover {
                    href: href.to_string(),
                    mime: mime.to_string(),
                    bytes: octets,
                };
                let mut ligne =
                    serde_json::to_string(&ritornello_proto::DisplayFrame::Cover(cover)).ok()?;
                ligne.push('\n');
                Some(Arc::from(ligne))
            })
            .await
            .clone();

        // **Le retrait est ce qui empêche le rendez-vous de devenir un cache.**
        // Une `OnceCell` garde sa valeur pour toujours ; laissée dans la table,
        // elle servirait la même vignette à un appelant survenu une heure plus
        // tard, alors que le fichier a pu changer sous son chemin.
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
            let mut en_vol = self.en_vol.lock().await;
            if en_vol.get(cle).is_some_and(|c| Arc::ptr_eq(c, &cellule)) {
                en_vol.remove(cle);
            }
        }
        resultat
    }
}

/// Réencode une pochette en vignette, ou rend les octets d'origine quand il n'y
/// a rien à gagner.
///
/// Quatre étapes, dans cet ordre, et l'ordre **est** la protection :
///
/// 1. **Les dimensions sont lues dans l'en-tête**, sans décoder. Quelques
///    dizaines d'octets suffisent, et rien n'est alloué à la taille de l'image.
/// 2. **La garde anti-bombe** compare le nombre de pixels au plafond. C'est la
///    seule borne qui protège vraiment : la taille du fichier ne dit *rien* du
///    coût du décodage — un PNG de 200 Kio peut annoncer 30000 × 30000 pixels,
///    soit 3,6 Gio de tampon, et `source_max` le laisse passer sans broncher.
/// 3. **Le passe-droit** : une image déjà petite en pixels *et* en octets part
///    telle quelle, sans décodage ni réencodage. Une pochette de 300 × 300 tirée
///    d'un fichier n'a rien à gagner d'un aller-retour qui la dégraderait.
/// 4. **Le décodage et l'encodage**, sur un fil bloquant.
///
/// Inverser 2 et 1 serait absurde ; inverser 3 et 2 serait dangereux — une
/// image de 30000 × 30000 pesant 200 Kio passerait le passe-droit sur son poids
/// alors qu'elle est précisément la bombe qu'on cherche à refuser. Le
/// passe-droit teste donc les **deux** critères, et vient après la garde.
///
/// **Rien n'est mémorisé**, et c'est cohérent avec `ligne` : la clé du cache
/// hache le chemin, pas le contenu, donc une vignette gardée deviendrait fausse
/// dès que l'utilisateur remplace l'image sous ce chemin. Le prix est un
/// décodage par publication, et `ligne` n'est appelée qu'une fois par changement
/// de pochette et par relais abonné.
///
/// `None` = rien à pousser, comme partout dans ce module : image illisible,
/// dimensions au-delà du plafond, ou vignette produite au-delà du filet.
async fn rendu(
    mime: &'static str,
    octets: Vec<u8>,
    r: Rendu,
) -> Option<(&'static str, Vec<u8>)> {
    let (largeur, hauteur) = dimensions(&octets)?;
    let pixels = u64::from(largeur) * u64::from(hauteur);
    if pixels > r.plafond_pixels {
        tracing::warn!(
            "cover not pushed: {largeur}x{hauteur} is {pixels} pixels, over the {} allowed \
             (decoding it would need about {} MiB)",
            r.plafond_pixels,
            pixels * 4 / (1024 * 1024)
        );
        return None;
    }
    if largeur.max(hauteur) <= r.cote_max_px && octets.len() <= r.plafond_sortie {
        tracing::debug!("cover already small ({largeur}x{hauteur}, {} bytes), pushed as it is", octets.len());
        return Some((mime, octets));
    }

    // `spawn_blocking` : décoder puis réencoder une image de plusieurs
    // mégapixels occupe un cœur pendant des centaines de millisecondes sur un
    // Pi 2. Le faire sur un fil de l'ordonnanceur figerait la boucle du cœur —
    // donc l'horloge de position, les commandes de la télécommande et les
    // requêtes HTTP — le temps d'une pochette.
    //
    // Cette tâche n'est **pas annulable** : abandonner la future ici ne
    // l'arrête pas, elle ira jusqu'au bout et son résultat sera jeté. C'est
    // acceptable précisément grâce à la garde de l'étape 2, qui borne ce
    // qu'elle peut coûter avant de la lancer.
    let plafond_alloc = (r.plafond_pixels as usize).saturating_mul(4);
    let travail = tokio::task::spawn_blocking(move || encode(octets, r, plafond_alloc)).await;
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
    if sortie.len() > r.plafond_sortie {
        tracing::warn!(
            "cover not pushed: rendered to {} bytes, over the {}-byte net",
            sortie.len(),
            r.plafond_sortie
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
/// anti-bombe, et une garde qui lit mal ses dimensions ne garde rien.
fn dimensions(octets: &[u8]) -> Option<(u32, u32)> {
    let lecteur = image::ImageReader::new(std::io::Cursor::new(octets))
        .with_guessed_format()
        .ok()?;
    match lecteur.into_dimensions() {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::debug!("cover header unreadable: {e}");
            None
        }
    }
}

/// Le décodage et l'encodage eux-mêmes. **Bloquant** : appelé sous
/// `spawn_blocking`.
fn encode(octets: Vec<u8>, r: Rendu, plafond_alloc: usize) -> Option<(&'static str, Vec<u8>)> {
    let mut lecteur = image::ImageReader::new(std::io::Cursor::new(&octets))
        .with_guessed_format()
        .ok()?;
    // La ceinture après les bretelles : la garde de `rendu` a déjà refusé les
    // dimensions trop grandes, mais elle croit l'en-tête. `Limits` borne
    // l'allocation réelle du décodeur, donc couvre le cas d'un en-tête qui
    // mentirait sur ses propres dimensions — le fichier fabriqué exprès, pas le
    // fichier maladroit.
    let mut limites = image::Limits::default();
    limites.max_alloc = Some(plafond_alloc as u64);
    lecteur.limits(limites);
    let image = match lecteur.decode() {
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
    let vignette = image.thumbnail(r.cote_max_px, r.cote_max_px);

    let mut sortie = Vec::new();
    // PNG dès qu'il y a un canal alpha, sans perte. Aplatir la transparence
    // demanderait de choisir une couleur de fond — un parti pris visuel que
    // l'appareil n'a pas à prendre sur la pochette de quelqu'un d'autre.
    if vignette.color().has_alpha() {
        if let Err(e) = vignette.write_to(&mut std::io::Cursor::new(&mut sortie), image::ImageFormat::Png) {
            tracing::warn!("cover PNG encoding failed: {e}");
            return None;
        }
        return Some(("image/png", sortie));
    }
    let mut encodeur =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut sortie, r.qualite_jpeg);
    // `to_rgb8` : l'encodeur JPEG refuse un tampon à canal alpha, et une image
    // en niveaux de gris ou en palette doit de toute façon être convertie.
    if let Err(e) = encodeur.encode_image(&vignette.to_rgb8()) {
        tracing::warn!("cover JPEG encoding failed: {e}");
        return None;
    }
    Some(("image/jpeg", sortie))
}

/// Ce que la lecture bornée d'un fichier de pochette rend, avant validation
/// du type d'image.
enum LectureBornee {
    Octets(Vec<u8>),
    /// La taille du fichier, **connue par `metadata`, avant toute lecture des
    /// octets eux-mêmes** : voir la doc de `lit_fichier_borne`.
    TropGros(u64),
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
/// **La taille est vérifiée avant toute lecture des octets**, sur `metadata`,
/// et c'est délibéré : une taille de fichier ne demande aucune connaissance du
/// format — pas d'en-tête à interpréter, pas de décodeur, indifférente à un
/// JPEG, un PNG, un WebP ou ce qui viendra ensuite. Un fichier du NAS
/// démesuré (le PNG de 150 Mo que `cover_get` cite comme cas réel) est ainsi
/// refusé sans qu'un seul octet de son contenu ne soit lu, plutôt que d'être
/// découvert après une lecture bornée à `COVER_MAX_BYTES + 1` octets — un
/// coût qui n'a de sens que si le fichier passe la borne. `take` avant
/// `read_to_end` reste en place ensuite, en filet : si le fichier grossit
/// *entre* le `metadata` et la lecture, la fenêtre TOCTOU rouverte ne laisse
/// jamais lire plus de `COVER_MAX_BYTES + 1` octets.
///
/// Deux bornes de temps sous le même délai, et une de taille avant tout :
///
/// * `metadata` puis, si la taille passe, `COVER_MAX_BYTES + 1` octets au plus
///   sont lus (le filet TOCTOU ci-dessus).
/// * `DELAI_FICHIER`, comme partout où ce module touche un fichier : le
///   partage peut être endormi, et l'attente doit être bornée par nous plutôt
///   que par le noyau.
async fn lit_fichier_borne(
    chemin: &std::path::Path,
    plafond: usize,
) -> Option<(&'static str, Vec<u8>)> {
    let lecture = tokio::time::timeout(DELAI_FICHIER, async {
        let fichier = tokio::fs::File::open(chemin).await?;
        let taille = fichier.metadata().await?.len();
        if taille > plafond as u64 {
            return Ok::<_, std::io::Error>(LectureBornee::TropGros(taille));
        }
        let mut octets = Vec::new();
        // `take` **avant** `read_to_end` : `read_to_end` seul lirait le
        // fichier entier, et le contrôle de taille arriverait après
        // l'allocation qu'il est censé éviter. N'agit ici que sur la fenêtre
        // TOCTOU (voir la doc au-dessus) : le cas courant a déjà été tranché
        // par `metadata`.
        fichier.take(plafond as u64 + 1).read_to_end(&mut octets).await?;
        Ok(LectureBornee::Octets(octets))
    })
    .await;
    let octets = match lecture {
        Ok(Ok(LectureBornee::Octets(v))) => v,
        Ok(Ok(LectureBornee::TropGros(taille))) => {
            // La taille exacte de l'offense, connue sans avoir rien lu de son
            // contenu — c'est ce que la lecture bornée à `+ 1` octet ne
            // pourrait jamais journaliser : elle ne verrait jamais que
            // `plafond + 1`, quelle que soit la taille réelle.
            tracing::warn!(
                "cover file {} not read: {taille} bytes over the {plafond}-byte limit",
                chemin.display()
            );
            return None;
        }
        Ok(Err(e)) => {
            tracing::debug!("cover file unreadable: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("cover file {} did not answer in {DELAI_FICHIER:?}", chemin.display());
            return None;
        }
    };
    if octets.len() > plafond {
        tracing::warn!(
            "cover file {} not pushed: grew past {plafond} bytes while being read",
            chemin.display()
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

/// Fixtures d'image partagées par les tests de ce module et ceux de `main`.
///
/// Ici et non dans chaque `mod tests` : deux copies d'un générateur d'image
/// dériveraient, et un test qui croit produire une image décodable alors qu'il
/// n'en produit plus est un faux positif silencieux.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Un JPEG **réellement décodable** de `largeur × hauteur`.
    ///
    /// Nécessaire dès qu'un test traverse `CoverCache::ligne` : le rendu, actif
    /// par défaut, décode l'image, et un en-tête suivi de remplissage est refusé
    /// — à juste titre, c'est un fichier tronqué.
    ///
    /// Un dégradé et non un aplat : un aplat se comprime à quelques centaines
    /// d'octets quelle que soit sa taille, ce qui rendrait indistinguables « la
    /// vignette a été produite » et « le plafond de sortie n'a jamais été
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

    /// Un PNG décodable **à canal alpha**, pour le chemin sans perte.
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

    /// Le plafond de source des réglages par défaut.
    ///
    /// Les tests d'`octets` ci-dessous portent sur le plafond, pas sur le rendu :
    /// le passer explicitement rend visible dans le test la borne qu'il éprouve,
    /// là où elle était cachée dans une constante de module. Le prendre des
    /// réglages **par défaut** plutôt que de `COVER_MAX_BYTES` en direct est
    /// délibéré : c'est la valeur qu'un appareil sorti d'usine applique
    /// réellement.
    fn plafond() -> usize {
        Reglages::default().source_max
    }

    /// En-tête JPEG minimal, suivi de `remplissage` octets quelconques.
    ///
    /// **Indécodable exprès** : ces octets valident l'en-tête que `type_image`
    /// inspecte, et rien de plus. Cela convient à tout ce qui porte sur les
    /// tailles et les plafonds, et cela ne convient **pas** à ce qui porte sur
    /// le rendu — voir `image_reelle`, plus bas.
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
        assert_eq!(cache.octets("k", plafond()).await, Some(("image/png", image)));
        assert_eq!(cache.octets("inconnue", plafond()).await, None);
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
        assert_eq!(cache.octets("k", plafond()).await, Some(("image/jpeg", image)));
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
            cache.octets("k", plafond()).await,
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
        let plafond = plafond();
        let dir = tempfile::tempdir().unwrap();

        let pile = dir.path().join("pile.jpg");
        std::fs::write(&pile, jpeg(plafond - 6)).unwrap();
        let cache = CoverCache::new();
        cache.insere("pile".into(), Pochette::Fichier(pile)).await;
        match cache.octets("pile", plafond).await {
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
            cache.octets("trop", plafond).await,
            None,
            "un seul octet au-dela du plafond doit suffire a refuser"
        );
    }

    #[tokio::test]
    async fn octets_rend_none_sur_un_fichier_disparu() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CoverCache::new();
        cache.insere("k".into(), Pochette::Fichier(dir.path().join("absent.jpg"))).await;
        assert_eq!(cache.octets("k", plafond()).await, None);
    }

    /// Prouve que le refus vient de `metadata`, appelé **avant** toute lecture
    /// des octets — pas de la lecture bornée à `COVER_MAX_BYTES + 1` octets
    /// qui reste en filet plus loin dans `lit_fichier_borne`. Un test qui se
    /// contenterait de vérifier le `None` ne distinguerait pas les deux : la
    /// lecture bornée refuse tout aussi bien. La preuve tient dans le
    /// journal : il doit nommer la taille **réelle** du fichier, très au-delà
    /// de `COVER_MAX_BYTES + 1` — un nombre que la lecture bornée ne pourrait
    /// jamais rendre, puisqu'elle ne lit jamais plus que cette borne.
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
        let chemin = dir.path().join("trop-gros.png");
        // Bien au-dela de COVER_MAX_BYTES + 1 : une lecture bornee a cette
        // limite ne pourrait jamais journaliser un nombre pareil. Fichier
        // creux (`set_len`) : aucune ecriture reelle des octets, seul le
        // metadata doit y suffire.
        let taille_reelle = ritornello_proto::COVER_MAX_BYTES as u64 + 50_000_000;
        let fichier = std::fs::File::create(&chemin).unwrap();
        fichier.set_len(taille_reelle).unwrap();
        drop(fichier);

        let tampon = Tampon::default();
        // `#[tokio::test]` est mono-thread par defaut : le repartiteur pose
        // par thread reste donc valide a travers le `.await` qui suit.
        let subscriber = tracing_subscriber::fmt().with_writer(tampon.clone()).with_ansi(false).finish();
        let garde = tracing::subscriber::set_default(subscriber);
        let resultat = lit_fichier_borne(&chemin, plafond()).await;
        drop(garde);

        assert!(resultat.is_none(), "un fichier bien au-dela du plafond doit etre refuse");
        let journal = String::from_utf8(tampon.0.lock().unwrap().clone()).unwrap();
        assert!(
            journal.contains(&taille_reelle.to_string()),
            "le journal doit nommer la taille reelle du fichier, connue par metadata avant toute lecture : {journal}"
        );
    }

    // -- `ligne` : la trame de pochette, relue a chaque appel ---------------

    #[tokio::test]
    async fn ligne_relit_le_fichier_donc_une_image_remplacee_sous_le_meme_chemin_est_servie_neuve() {
        // **Le defaut le plus grave de la passe, a la maille du cache.** La cle
        // hache le chemin, pas le contenu : rien dans le cache ne peut voir
        // qu'un `folder.jpg` a ete remplace sur le partage. Tant que `ligne`
        // relit le fichier a chaque appel, ce n'est pas un probleme ; une ligne
        // encodee mise en memoire, elle, servait pour toujours l'image d'avant.
        //
        // Le scenario reel est en trois clics : desactiver l'afficheur depuis la
        // page d'admin, remplacer l'image, le reactiver. Le relais rebranche
        // redemande la pochette courante — donc `ligne` avec la **meme cle** —
        // et personne n'a insere quoi que ce soit entre-temps. C'est pourquoi ce
        // test n'appelle pas `insere` une seconde fois : une invalidation posee
        // dans `insere` ne couvrirait pas ce chemin-la.
        //
        // Deux images **decodables**, et petites : sous 640 px et sous le
        // plafond de sortie, le rendu par defaut les laisse passer telles
        // quelles (voir `rendu`, etape 3). Les octets servis sont donc ceux du
        // fichier, ce qui garde a ce test l'assertion la plus tranchante
        // possible sur la fraicheur — et documente le passe-droit au passage.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        std::fs::write(&chemin, fixtures::jpeg_decodable(48, 48)).unwrap();
        let cache = CoverCache::new();
        cache.insere("k".into(), Pochette::Fichier(chemin.clone())).await;

        let avant = cache.ligne("k", "/api/cover/k").await.expect("une image locale doit produire une ligne");
        // L'utilisateur remplace la pochette sur le partage. Dimensions
        // differentes, pour que l'inegalite ne tienne pas au seul remplissage.
        std::fs::write(&chemin, fixtures::jpeg_decodable(64, 64)).unwrap();
        let apres = cache.ligne("k", "/api/cover/k").await.expect("le second appel doit reussir aussi");

        assert_ne!(
            &*avant, &*apres,
            "apres remplacement du fichier sous la meme cle, la ligne servie doit etre la nouvelle"
        );
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&apres).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(
                    c.bytes,
                    fixtures::jpeg_decodable(64, 64),
                    "les octets servis doivent etre ceux du fichier actuel"
                );
            }
            autre => panic!("une trame de pochette etait attendue : {autre:?}"),
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
        cache.insere("a".into(), Pochette::Fichier(a)).await;
        cache.insere("b".into(), Pochette::Fichier(b)).await;

        let ligne_a = cache.ligne("a", "/api/cover/a").await.unwrap();
        let ligne_b = cache.ligne("b", "/api/cover/b").await.unwrap();
        assert_ne!(&*ligne_a, &*ligne_b, "deux cles differentes doivent produire des lignes differentes");

        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&ligne_a).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(c.href, "/api/cover/a");
                assert_eq!(c.mime, "image/jpeg");
            }
            autre => panic!("une trame de pochette etait attendue : {autre:?}"),
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
    // -- Le rendu : ce que le cœur fabrique avant de pousser ----------------

    /// Un `Rendu` dont chaque champ est nommé par le test qui s'en sert : les
    /// défauts du produit (640 px, 512 Kio, 16 Mpx) rendraient la plupart des
    /// cas inatteignables sans fabriquer des images énormes.
    fn rendu_de_test(cote_max_px: u32, plafond_sortie: usize, plafond_pixels: u64) -> Rendu {
        Rendu { cote_max_px, qualite_jpeg: 85, plafond_sortie, plafond_pixels }
    }

    #[tokio::test]
    async fn huit_afficheurs_qui_demandent_la_meme_pochette_ne_la_construisent_quune_fois() {
        // Le rendez-vous. Deux afficheurs abonnés reçoivent la **même** trame
        // d'état, donc demandent la même pochette dans le même instant, et
        // décodaient puis réencodaient deux fois la même image — plusieurs
        // centaines de millisecondes de cœur en double sur un Pi 2.
        //
        // La preuve est un **décompte d'exécutions**, et il n'y en a pas
        // d'autre : comparer les trames rendues ne dirait rien, deux
        // constructions successives de la même image produisant des octets
        // identiques.
        let cache = Arc::new(CoverCache::new());
        // Un rendu qui a vraiment du travail : 600 × 400 dépasse le côté
        // maximal, donc l'image est décodée et réencodée pour de bon. Sans
        // cela, le passe-droit rendrait la source telle quelle et le premier
        // arrivé finirait sans jamais suspendre — aucun suiveur n'aurait le
        // temps d'arriver, et le test passerait sans rien prouver.
        cache.set_reglages(Reglages {
            source_max: 8 * 1024 * 1024,
            rendu: Some(rendu_de_test(64, 512 * 1024, 16_000_000)),
        });
        cache
            .insere("k".into(), Pochette::Octets(fixtures::jpeg_decodable(600, 400), "image/jpeg"))
            .await;

        let taches: Vec<_> = (0..8)
            .map(|_| {
                let c = cache.clone();
                tokio::spawn(async move { c.ligne("k", "/api/cover/k").await })
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
            cache.constructions(),
            1,
            "une seule construction pour huit demandes concurrentes de la meme cle"
        );
    }

    #[tokio::test]
    async fn le_rendez_vous_ne_retient_rien_une_fois_la_trame_construite() {
        // Le rendez-vous n'est **pas** un cache, et c'est ce qui le rend
        // acceptable : la clé hache le *chemin*, pas le contenu, donc une trame
        // retenue deviendrait fausse dès que l'utilisateur remplace l'image sous
        // ce chemin. Une `OnceCell` gardant sa valeur pour toujours, tout tient
        // au retrait de l'entrée.
        let cache = Arc::new(CoverCache::new());
        cache.set_reglages(Reglages {
            source_max: 8 * 1024 * 1024,
            rendu: Some(rendu_de_test(64, 512 * 1024, 16_000_000)),
        });
        cache
            .insere("k".into(), Pochette::Octets(fixtures::jpeg_decodable(600, 400), "image/jpeg"))
            .await;

        assert!(cache.ligne("k", "/api/cover/k").await.is_some());
        assert!(
            cache.en_vol.lock().await.is_empty(),
            "la table des constructions en cours doit etre vide apres coup"
        );

        // Et la seconde demande **reconstruit**, au lieu d'être servie par une
        // cellule restée en place.
        assert!(cache.ligne("k", "/api/cover/k").await.is_some());
        assert_eq!(
            cache.constructions(),
            2,
            "deux demandes separees dans le temps doivent produire deux constructions"
        );
    }

    #[tokio::test]
    async fn une_image_deja_petite_part_telle_quelle_sans_etre_reencodee() {
        // Le passe-droit. L'identité **binaire** est l'assertion qui compte :
        // un aller-retour décodage/encodage produirait des octets différents
        // même à dimensions égales, donc l'égalité prouve qu'aucun n'a eu lieu.
        let source = fixtures::jpeg_decodable(64, 64);
        let sortie = rendu("image/jpeg", source.clone(), rendu_de_test(640, 512 * 1024, 16_000_000))
            .await
            .expect("une petite image doit passer");
        assert_eq!(sortie, ("image/jpeg", source));
    }

    #[tokio::test]
    async fn une_image_trop_grande_est_reduite_en_gardant_son_rapport() {
        // 300 × 150, réduite à un côté de 100 : le rapport 2:1 doit survivre.
        // Vérifié en **décodant la sortie**, pas en croyant le code sur parole.
        let source = fixtures::jpeg_decodable(300, 150);
        let (mime, sortie) = rendu("image/jpeg", source.clone(), rendu_de_test(100, 512 * 1024, 16_000_000))
            .await
            .expect("une grande image doit etre reduite, pas refusee");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(dimensions(&sortie), Some((100, 50)), "le rapport 2:1 doit etre conserve");
        assert!(
            sortie.len() < source.len(),
            "une vignette de 100x50 doit peser moins que son original de 300x150 : {} contre {}",
            sortie.len(),
            source.len()
        );
    }

    #[tokio::test]
    async fn une_image_a_canal_alpha_est_reencodee_en_png_sans_perte() {
        // Aplatir la transparence demanderait de choisir une couleur de fond,
        // parti pris visuel que l'appareil n'a pas à prendre. Le mime change,
        // donc la trame poussée le déclare — un afficheur qui reçoit
        // `image/jpeg` avec des octets PNG afficherait un carré cassé.
        let source = fixtures::png_alpha(300, 300);
        let (mime, sortie) = rendu("image/png", source, rendu_de_test(100, 512 * 1024, 16_000_000))
            .await
            .expect("un png alpha doit etre rendu");
        assert_eq!(mime, "image/png", "le mime doit suivre le format reellement produit");
        assert_eq!(dimensions(&sortie), Some((100, 100)));
    }

    /// **La garde anti-bombe, et son ordre.**
    ///
    /// L'image de ce test franchirait le passe-droit sans difficulté : 100 px de
    /// côté sous les 640 autorisés, deux kilooctets sous le plafond de sortie.
    /// Seule la garde de pixels la refuse. Le test échoue donc si la garde
    /// disparaît **et** si elle est déplacée après le passe-droit — c'est ce
    /// second cas qui compte, parce qu'une bombe est précisément une image
    /// minuscule en octets et démesurée en pixels.
    #[tokio::test]
    async fn le_plafond_de_pixels_refuse_avant_tout_decodage_et_avant_le_passe_droit() {
        let source = fixtures::jpeg_decodable(100, 100);
        assert!(
            source.len() < 512 * 1024,
            "la fixture doit tenir sous le plafond de sortie, sinon le test ne prouve pas l'ordre"
        );
        assert_eq!(
            rendu("image/jpeg", source, rendu_de_test(640, 512 * 1024, 1_000)).await,
            None,
            "10000 pixels au-dela d'un plafond de 1000 doivent etre refuses"
        );
    }

    #[tokio::test]
    async fn une_vignette_au_dela_du_filet_de_sortie_nest_pas_poussee() {
        // Le filet, éprouvé sur un plafond volontairement minuscule : une
        // vignette 200 × 200 d'un dégradé ne tient pas dans 200 octets.
        let source = fixtures::jpeg_decodable(400, 400);
        assert_eq!(
            rendu("image/jpeg", source, rendu_de_test(200, 200, 16_000_000)).await,
            None,
            "une vignette au-dela du filet ne doit pas etre poussee"
        );
    }

    #[tokio::test]
    async fn linterrupteur_decoche_pousse_la_source_sans_la_decoder() {
        // Deux propriétés en une, et la fixture est l'astuce : ces octets ont un
        // en-tête JPEG valide mais un contenu **indécodable**. S'ils arrivent
        // intacts au bout de `ligne`, c'est que le décodeur n'a pas été appelé
        // du tout — pas seulement que son résultat a été ignoré.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        let source = jpeg(1000);
        std::fs::write(&chemin, &source).unwrap();
        let cache = CoverCache::new();
        cache.set_reglages(Reglages { source_max: plafond(), rendu: None });
        cache.insere("k".into(), Pochette::Fichier(chemin)).await;

        let ligne = cache.ligne("k", "/api/cover/k").await.expect("la source doit partir telle quelle");
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&ligne).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(c.bytes, source, "les octets pousses doivent etre ceux de la source");
                assert_eq!(c.mime, "image/jpeg");
            }
            autre => panic!("une trame de pochette etait attendue : {autre:?}"),
        }
    }

    #[tokio::test]
    async fn linterrupteur_coche_ecarte_une_image_dont_les_octets_ne_se_decodent_pas() {
        // Le pendant du test ci-dessus, et un **changement de comportement**
        // assumé : `type_image` ne lit que les octets magiques, donc un fichier
        // tronqué passait cette validation et partait vers les afficheurs, qui
        // montraient chacun un carré cassé à leur façon. Le rendu le tranche une
        // fois pour tous, au centre.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        std::fs::write(&chemin, jpeg(1000)).unwrap();
        let cache = CoverCache::new();
        cache.insere("k".into(), Pochette::Fichier(chemin)).await;
        assert!(
            cache.ligne("k", "/api/cover/k").await.is_none(),
            "un fichier dont l'en-tete ment sur son contenu ne doit pas etre pousse"
        );
    }

    #[tokio::test]
    async fn les_reglages_du_produit_reencodent_une_grande_pochette() {
        // Le chemin de production complet, avec les défauts et sans les
        // paramétrer : une pochette 1000 × 1000 doit arriver en 640 px.
        // Sans ce test, tous les autres pourraient passer avec des réglages
        // qu'aucun appareil n'applique.
        let dir = tempfile::tempdir().unwrap();
        let chemin = dir.path().join("folder.jpg");
        let source = fixtures::jpeg_decodable(1000, 1000);
        std::fs::write(&chemin, &source).unwrap();
        let cache = CoverCache::new();
        cache.insere("k".into(), Pochette::Fichier(chemin)).await;

        let ligne = cache.ligne("k", "/api/cover/k").await.expect("une pochette doit etre poussee");
        match serde_json::from_str::<ritornello_proto::DisplayFrame>(&ligne).unwrap() {
            ritornello_proto::DisplayFrame::Cover(c) => {
                assert_eq!(
                    dimensions(&c.bytes),
                    Some((640, 640)),
                    "les reglages par defaut doivent ramener le cote long a 640 px"
                );
                assert!(
                    c.bytes.len() < source.len(),
                    "la vignette doit peser moins que la source : {} contre {}",
                    c.bytes.len(),
                    source.len()
                );
            }
            autre => panic!("une trame de pochette etait attendue : {autre:?}"),
        }
    }

    #[test]
    fn les_reglages_traduisent_linterrupteur_en_absence_de_rendu() {
        // La conversion `Settings -> Reglages`, qui est le seul endroit où
        // l'interrupteur devient une structure. `None` plutôt qu'un booléen
        // porté à côté : c'est ce qui rend impossible de lire `cote_max_px`
        // sans avoir vérifié d'abord que le rendu est actif.
        let mut s = crate::state::Settings::default();
        assert!(Reglages::from(&s).rendu.is_some(), "le defaut du produit reencode");

        s.cover_rendition = false;
        assert!(Reglages::from(&s).rendu.is_none());
        assert_eq!(
            Reglages::from(&s).source_max,
            20 * 1024 * 1024,
            "le plafond de source survit a l'interrupteur : c'est sa raison d'etre"
        );
    }
}
