//! Moitié Admin : la page de gestion des racines et de la liste de lecture.
//!
//! Elle partage avec la moitié Source la table des racines et la liste en
//! cours, derrière des verrous asynchrones. Les deux moitiés tournent dans des
//! tâches distinctes : une panne ici ne doit jamais couper l'audio.
//!
//! Le protocole admin est **requête/réponse** et ne pousse rien. C'est
//! pourquoi le scan est une tâche asynchrone dont la page interroge
//! l'avancement, et non un flux d'événements.

use crate::state;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_files::m3u::Entry;
use ritornello_plugin_files::playlist::Playlist;
use ritornello_plugin_files::roots::{Root, RootKind, Roots};
use ritornello_plugin_files::store::{self, Location};
use ritornello_plugin_files::{mount, scan};
use ritornello_plugin_sdk::AdminPlugin;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

/// Avancement du scan en cours, tel que la page le lit.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanProgress {
    pub running: bool,
    pub found: usize,
    pub dir: String,
    /// Refus ou incident du **dernier** scan. Conservé après la fin : c'est la
    /// seule façon pour la page d'apprendre qu'un ajout a échoué, l'appel
    /// `add_dir` ayant rendu la main bien avant.
    pub error: Option<String>,
}

pub struct FilesAdmin {
    pub roots_path: PathBuf,
    pub creds_dir: PathBuf,
    pub internal_playlists: PathBuf,
    pub state_path: PathBuf,
    pub roots: Arc<AsyncRwLock<Roots>>,
    pub playlist: Arc<AsyncRwLock<Playlist>>,
    pub catalog: Arc<RwLock<Catalog>>,
    pub scan: Arc<Mutex<ScanProgress>>,
    /// Tâche de scan en cours. En lancer une nouvelle **avorte** la
    /// précédente : deux clics ne doivent pas laisser deux marches concurrentes
    /// saturer un partage lent.
    pub scan_task: Option<tokio::task::JoinHandle<()>>,
    /// Entrées d'un m3u chargé qu'aucune règle n'a su résoudre. Rapportées à la
    /// page, jamais supprimées en silence.
    pub unresolved: Arc<Mutex<Vec<String>>>,
    /// Dernier contenu de dossier ou résultat de recherche demandé par la page.
    ///
    /// `set_data` ne rend qu'un `Ok`/`Err`, sans charge utile : le contenu
    /// voyage donc par `get_data`, exactement comme la recherche d'annuaire du
    /// plugin radio range ses résultats avant que la page ne les relise.
    pub browse: Arc<Mutex<serde_json::Value>>,
    /// Annonce le nombre de présélections à la moitié Source dès qu'il change,
    /// sans attendre qu'une piste soit jouée — sinon la grille de la
    /// télécommande web garderait l'ancien jeu de numéros.
    pub preset_count_tx: tokio::sync::watch::Sender<u8>,
}

/// Racine telle que la page l'envoie : comme `Root`, plus le mot de passe.
///
/// Type distinct, et c'est délibéré : le mot de passe ne doit exister que dans
/// ce sens-là. `Root` ne le porte pas, donc `get_data` ne peut pas le rendre
/// par inadvertance, même si quelqu'un ajoute un champ plus tard.
#[derive(Debug, Clone, Deserialize)]
pub struct RootInput {
    pub name: String,
    pub kind: RootKind,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub share: String,
    #[serde(default)]
    pub subpath: Option<String>,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub writable: bool,
    /// **Vide veut dire « garde celui déjà enregistré »**. Sans cette règle,
    /// rouvrir la page et cliquer « Enregistrer » suffirait à casser le
    /// montage, sans que rien ne l'annonce — la page ne peut pas réafficher un
    /// mot de passe qu'elle ne reçoit jamais.
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    SaveRoots { roots: Vec<RootInput> },
    Mount,
    Browse { root: String, #[serde(default)] path: String },
    Search { root: String, query: String },
    AddDir { root: String, #[serde(default)] path: String },
    AddFile { root: String, path: String },
    Remove { index: usize },
    Move { from: usize, to: usize },
    Clear,
    SavePlaylist { name: String, r#where: String },
    LoadPlaylist { name: String, r#where: String },
}

impl FilesAdmin {
    fn mot(&self, cle: &str) -> String {
        self.catalog.read().unwrap().get(cle).to_string()
    }

    /// Résout un chemin **relatif** fourni par la page contre la racine
    /// nommée, en refusant tout ce qui en sortirait.
    ///
    /// C'est la garde d'évasion côté page : `name` est déjà validé par
    /// `Roots`, mais `path` vient du navigateur à chaque requête. Un
    /// `../../etc` y ferait parcourir — et ajouter à une liste de lecture — des
    /// fichiers hors de toute racine déclarée.
    async fn sous_racine(&self, root: &str, path: &str) -> Result<PathBuf, String> {
        let roots = self.roots.read().await;
        let r = roots
            .by_name(root)
            .ok_or_else(|| self.mot("unknown_root").replace("{name}", root))?;
        let base = r.base_dir();
        let cible = if path.is_empty() { base.clone() } else { base.join(path) };
        // Comparaison sur les formes canonisées : c'est la seule qui résiste
        // aux liens symboliques, un `.` ou `..` textuel pouvant être neutralisé
        // par le système de fichiers lui-même.
        let (Ok(base_c), Ok(cible_c)) = (base.canonicalize(), cible.canonicalize()) else {
            return Err(self.mot("scan_io_error").replace("{path}", &cible.display().to_string()));
        };
        if !cible_c.starts_with(&base_c) {
            return Err(self.mot("scan_io_error").replace("{path}", path));
        }
        Ok(cible_c)
    }

    /// Publie la liste à la moitié Source et persiste, après toute
    /// modification. Le compte part **avant** l'écriture disque : la grille web
    /// n'a pas à attendre un `/var/lib` lent.
    async fn liste_modifiee(&self) {
        let liste = self.playlist.read().await;
        let _ = self.preset_count_tx.send(liste.preset_count());
        let stockees: Vec<state::StoredEntry> =
            liste.entries.iter().map(state::StoredEntry::from).collect();
        let index = liste.index;
        drop(liste);
        if let Err(e) = state::update(&self.state_path, |s| {
            s.playlist = stockees;
            s.index = index;
        }) {
            tracing::warn!("persisting the playlist: {e}");
        }
    }

    /// Ajoute des pistes à la liste, en respectant le plafond.
    async fn ajouter(&self, chemins: Vec<PathBuf>) -> Result<(), String> {
        let mut liste = self.playlist.write().await;
        if liste.entries.len() + chemins.len() > scan::MAX_TRACKS {
            return Err(self
                .mot("too_many_tracks")
                .replace("{cap}", &scan::MAX_TRACKS.to_string()));
        }
        liste.entries.extend(
            chemins.into_iter().map(|path| Entry { path, title: None, duration_s: None }),
        );
        Ok(())
    }

    /// Écrit le fichier d'identifiants consommé par `mount.cifs`.
    ///
    /// Les permissions sont posées **à la création**, pas après : créer puis
    /// restreindre laisserait une fenêtre pendant laquelle le mot de passe
    /// serait lisible par tout le monde.
    fn ecrire_identifiants(path: &Path, user: &str, password: &str, domain: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("cred.tmp");
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        use std::io::Write;
        writeln!(f, "username={user}")?;
        writeln!(f, "password={password}")?;
        if !domain.is_empty() {
            writeln!(f, "domain={domain}")?;
        }
        f.sync_all()?;
        drop(f);
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Relit le mot de passe déjà enregistré pour une racine.
    ///
    /// Employé quand la page en envoie un vide : elle ne peut pas renvoyer ce
    /// qu'elle n'a jamais reçu.
    fn mot_de_passe_existant(path: &Path) -> Option<String> {
        let contenu = std::fs::read_to_string(path).ok()?;
        contenu
            .lines()
            .find_map(|l| l.strip_prefix("password="))
            .map(str::to_string)
    }
}

#[async_trait::async_trait]
impl AdminPlugin for FilesAdmin {
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => {
                Some(("text/javascript".to_string(), include_str!("../ui/dist/ui.js").to_string()))
            }
            "ui.css" => {
                Some(("text/css".to_string(), include_str!("../ui/dist/ui.css").to_string()))
            }
            _ => None,
        }
    }

    fn catalog(&self) -> serde_json::Value {
        let cat = self.catalog.read().unwrap();
        serde_json::json!(cat.entries())
    }

    async fn get_data(&self) -> serde_json::Value {
        let roots = self.roots.read().await;
        // Chaque racine repart avec son état de montage, mais **jamais** son
        // mot de passe : `Root` ne le porte pas, il n'y a donc rien à filtrer
        // ici — c'est le type qui garantit l'absence, pas la vigilance.
        let racines: Vec<serde_json::Value> = roots
            .root
            .iter()
            .map(|r| {
                let mut v = serde_json::to_value(r).unwrap_or_default();
                if let Some(o) = v.as_object_mut() {
                    o.insert(
                        "mounted".into(),
                        serde_json::json!(mount::state(r) == mount::MountState::Mounted),
                    );
                }
                v
            })
            .collect();
        let sauvegardees = store::list(&self.internal_playlists, &roots);
        drop(roots);

        let liste = self.playlist.read().await;
        let pistes: Vec<serde_json::Value> = liste
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path.to_string_lossy(),
                    "name": e.display_name(),
                    "duration_s": e.duration_s,
                    // Marquée, jamais masquée : une liste qui rétrécit sans
                    // rien dire est un défaut qu'on met des mois à attribuer.
                    "missing": !e.path.is_file(),
                })
            })
            .collect();
        let index = liste.index;
        drop(liste);

        // Gardes `std::sync` prises après le dernier `.await` : aucune ne
        // traverse un point d'attente.
        let scan = self.scan.lock().unwrap().clone();
        let unresolved = self.unresolved.lock().unwrap().clone();
        let browse = self.browse.lock().unwrap().clone();
        serde_json::json!({
            "roots": racines,
            "playlist": pistes,
            "index": index,
            "scan": scan,
            "browse": browse,
            "saved": sauvegardees.iter().map(|s| serde_json::json!({
                "name": s.name,
                "where": match &s.location {
                    Location::Internal => "internal".to_string(),
                    Location::Root(n) => n.clone(),
                },
            })).collect::<Vec<_>>(),
            "unresolved": unresolved,
        })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let op: Op = serde_json::from_value(data)
            .map_err(|e| self.mot("bad_request").replace("{detail}", &e.to_string()))?;
        match op {
            Op::SaveRoots { roots } => {
                let table = Roots {
                    root: roots
                        .iter()
                        .map(|i| Root {
                            name: i.name.clone(),
                            kind: i.kind,
                            path: i.path.clone(),
                            host: i.host.clone(),
                            share: i.share.clone(),
                            subpath: i.subpath.clone(),
                            user: i.user.clone(),
                            domain: i.domain.clone(),
                            writable: i.writable,
                        })
                        .collect(),
                };
                // Valider **avant** d'écrire quoi que ce soit : un fichier
                // d'identifiants posé pour une racine ensuite refusée resterait
                // orphelin sur le disque.
                table.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                for entree in &roots {
                    if entree.kind != RootKind::Smb {
                        continue;
                    }
                    let Some(r) = table.by_name(&entree.name) else { continue };
                    let chemin = r.credentials_path(&self.creds_dir);
                    let mot_de_passe = if entree.password.is_empty() {
                        Self::mot_de_passe_existant(&chemin).unwrap_or_default()
                    } else {
                        entree.password.clone()
                    };
                    Self::ecrire_identifiants(&chemin, &entree.user, &mot_de_passe, &entree.domain)
                        .map_err(|e| {
                            tracing::warn!("writing credentials for {}: {e}", entree.name);
                            self.mot("store_io_error").replace("{path}", &chemin.display().to_string())
                        })?;
                }
                let texte = toml::to_string_pretty(&table)
                    .map_err(|e| {
                        tracing::warn!("serialising the roots table: {e}");
                        self.mot("store_io_error").replace("{path}", &self.roots_path.display().to_string())
                    })?;
                let tmp = self.roots_path.with_extension("toml.tmp");
                std::fs::write(&tmp, texte)
                    .and_then(|_| std::fs::rename(&tmp, &self.roots_path))
                    .map_err(|e| {
                        tracing::warn!("saving the roots table: {e}");
                        self.mot("store_io_error").replace("{path}", &self.roots_path.display().to_string())
                    })?;
                *self.roots.write().await = table;
                Ok(())
            }

            Op::Mount => mount::reconcile(mount::UNIT).await,

            Op::Browse { root, path } => {
                let dir = self.sous_racine(&root, &path).await?;
                let cat = self.catalog.clone();
                let (dossiers, fichiers) =
                    tokio::task::spawn_blocking(move || scan::list_dir(&dir))
                        .await
                        .map_err(|e| format!("browse task: {e}"))?
                        .map_err(|e| e.message(&cat.read().unwrap()))?;
                *self.browse.lock().unwrap() = serde_json::json!({
                    "root": root,
                    "path": path,
                    "dirs": dossiers,
                    "files": fichiers,
                    "results": [],
                });
                Ok(())
            }

            Op::Search { root, query } => {
                let base = self.sous_racine(&root, "").await?;
                let cat = self.catalog.clone();
                let base_pour_relatif = base.clone();
                let (trouves, tronque) =
                    tokio::task::spawn_blocking(move || scan::search(&base, &query, 200))
                        .await
                        .map_err(|e| format!("search task: {e}"))?
                        .map_err(|e| e.message(&cat.read().unwrap()))?;
                // Chemins **relatifs à la racine** : c'est ce que la page
                // renvoie ensuite dans un `add_file`, et un chemin absolu y
                // serait refusé par la garde d'évasion.
                let relatifs: Vec<String> = trouves
                    .iter()
                    .filter_map(|p| p.strip_prefix(&base_pour_relatif).ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .collect();
                *self.browse.lock().unwrap() = serde_json::json!({
                    "root": root,
                    "path": "",
                    "dirs": [],
                    "files": [],
                    "results": relatifs,
                    // Dit à la page qu'il y en avait davantage, pour qu'elle
                    // invite à affiner plutôt que de présenter une liste
                    // tronquée comme si elle était complète.
                    "truncated": tronque,
                });
                Ok(())
            }

            Op::AddDir { root, path } => {
                let dir = self.sous_racine(&root, &path).await?;
                if let Some(t) = self.scan_task.take() {
                    // Deux clics ne doivent pas laisser deux marches
                    // concurrentes saturer un partage lent.
                    t.abort();
                }
                *self.scan.lock().unwrap() = ScanProgress {
                    running: true,
                    found: 0,
                    dir: path.clone(),
                    error: None,
                };
                let progres = self.scan.clone();
                let playlist = self.playlist.clone();
                let catalog = self.catalog.clone();
                let etat = self.scan.clone();
                let compteur = Arc::new(AtomicUsize::new(0));
                let tx = self.preset_count_tx.clone();
                let state_path = self.state_path.clone();
                self.scan_task = Some(tokio::spawn(async move {
                    let c = compteur.clone();
                    let p = progres.clone();
                    let trouves = tokio::task::spawn_blocking(move || {
                        scan::walk_with(&dir, scan::MAX_TRACKS, &|n, d| {
                            c.store(n, Ordering::Relaxed);
                            if let Ok(mut g) = p.lock() {
                                g.found = n;
                                g.dir = d.display().to_string();
                            }
                        })
                    })
                    .await;
                    let resultat = match trouves {
                        Ok(Ok(chemins)) => {
                            let mut liste = playlist.write().await;
                            if liste.entries.len() + chemins.len() > scan::MAX_TRACKS {
                                Err(catalog
                                    .read()
                                    .unwrap()
                                    .get("too_many_tracks")
                                    .replace("{cap}", &scan::MAX_TRACKS.to_string()))
                            } else {
                                liste.entries.extend(chemins.into_iter().map(|path| Entry {
                                    path,
                                    title: None,
                                    duration_s: None,
                                }));
                                let compte = liste.preset_count();
                                let stockees: Vec<state::StoredEntry> =
                                    liste.entries.iter().map(state::StoredEntry::from).collect();
                                let index = liste.index;
                                drop(liste);
                                let _ = tx.send(compte);
                                if let Err(e) = state::update(&state_path, |s| {
                                    s.playlist = stockees;
                                    s.index = index;
                                }) {
                                    tracing::warn!("persisting the playlist: {e}");
                                }
                                Ok(())
                            }
                        }
                        Ok(Err(e)) => Err(e.message(&catalog.read().unwrap())),
                        Err(e) => Err(format!("scan task: {e}")),
                    };
                    if let Ok(mut g) = etat.lock() {
                        g.running = false;
                        g.error = resultat.err();
                    }
                }));
                Ok(())
            }

            Op::AddFile { root, path } => {
                let fichier = self.sous_racine(&root, &path).await?;
                self.ajouter(vec![fichier]).await?;
                self.liste_modifiee().await;
                Ok(())
            }

            Op::Remove { index } => {
                let mut liste = self.playlist.write().await;
                if index >= liste.entries.len() {
                    return Err(self.mot("bad_request").replace("{detail}", "index"));
                }
                liste.entries.remove(index);
                // L'index de lecture suit : retirer une piste avant celle qui
                // joue décalerait sinon toute la numérotation sous les pieds de
                // l'auditeur.
                if liste.index > index {
                    liste.index -= 1;
                } else if liste.index >= liste.entries.len() {
                    liste.index = 0;
                }
                drop(liste);
                self.liste_modifiee().await;
                Ok(())
            }

            Op::Move { from, to } => {
                let mut liste = self.playlist.write().await;
                if from >= liste.entries.len() || to >= liste.entries.len() {
                    return Err(self.mot("bad_request").replace("{detail}", "index"));
                }
                let e = liste.entries.remove(from);
                liste.entries.insert(to, e);
                drop(liste);
                self.liste_modifiee().await;
                Ok(())
            }

            Op::Clear => {
                let mut liste = self.playlist.write().await;
                liste.entries.clear();
                liste.index = 0;
                drop(liste);
                self.unresolved.lock().unwrap().clear();
                self.liste_modifiee().await;
                Ok(())
            }

            Op::SavePlaylist { name, r#where } => {
                let dest = if r#where == "internal" {
                    Location::Internal
                } else {
                    Location::Root(r#where)
                };
                let roots = self.roots.read().await;
                let liste = self.playlist.read().await;
                store::save(&liste.entries, &name, &dest, &self.internal_playlists, &roots)
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))
            }

            Op::LoadPlaylist { name, r#where } => {
                let from = if r#where == "internal" {
                    Location::Internal
                } else {
                    Location::Root(r#where)
                };
                let roots = self.roots.read().await;
                let charge = store::load(&name, &from, &self.internal_playlists, &roots)
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                drop(roots);
                *self.unresolved.lock().unwrap() = charge.unresolved;
                let mut liste = self.playlist.write().await;
                liste.entries = charge.entries;
                liste.index = 0;
                drop(liste);
                self.liste_modifiee().await;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un admin sur des répertoires temporaires, avec une racine locale
    /// déclarée. Le tempdir est volontairement fuité : l'admin vit le temps du
    /// test, et le laisser tomber effacerait les fichiers qu'il écrit.
    fn admin_de_test() -> (FilesAdmin, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let racine = dir.path().to_path_buf();
        std::mem::forget(dir);
        std::fs::create_dir_all(racine.join("media")).unwrap();
        let (tx, _rx) = tokio::sync::watch::channel(0u8);
        let admin = FilesAdmin {
            roots_path: racine.join("media-roots.toml"),
            creds_dir: racine.join("creds"),
            internal_playlists: racine.join("playlists"),
            state_path: racine.join("plugin-files.json"),
            roots: Arc::new(AsyncRwLock::new(Roots::default())),
            playlist: Arc::new(AsyncRwLock::new(Playlist::default())),
            catalog: Arc::new(RwLock::new(Catalog::load(
                "files",
                "en",
                &racine,
                ritornello_plugin_files::FILES_EN,
            ))),
            scan: Arc::new(Mutex::new(ScanProgress::default())),
            scan_task: None,
            unresolved: Arc::new(Mutex::new(Vec::new())),
            browse: Arc::new(Mutex::new(serde_json::json!({}))),
            preset_count_tx: tx,
        };
        (admin, racine)
    }

    fn partage(password: &str) -> serde_json::Value {
        serde_json::json!({
            "op": "save_roots",
            "roots": [{
                "name": "nas", "kind": "smb", "host": "192.168.1.20",
                "share": "musique", "user": "steven", "password": password
            }]
        })
    }

    #[tokio::test]
    async fn get_data_ne_rend_jamais_le_mot_de_passe() {
        // Il n'a aucune raison de traverser vers le navigateur, et la page n'en
        // a pas besoin pour afficher l'état d'un partage. La garantie est
        // portée par le type : `Root` ne contient pas le champ.
        let (mut admin, _) = admin_de_test();
        admin.set_data(partage("secret-du-nas")).await.unwrap();
        let texte = serde_json::to_string(&admin.get_data().await).unwrap();
        assert!(!texte.contains("password"), "{texte}");
        assert!(!texte.contains("secret-du-nas"), "{texte}");
    }

    #[tokio::test]
    async fn un_mot_de_passe_vide_conserve_celui_deja_enregistre() {
        // Sinon rouvrir la page et cliquer « Enregistrer » suffirait à casser
        // le montage, sans que rien ne l'annonce : la page ne peut pas
        // renvoyer un mot de passe qu'elle ne reçoit jamais.
        let (mut admin, _) = admin_de_test();
        admin.set_data(partage("secret-du-nas")).await.unwrap();
        admin.set_data(partage("")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("nas.cred")).unwrap();
        assert!(cred.contains("password=secret-du-nas"), "{cred}");
    }

    #[tokio::test]
    async fn un_mot_de_passe_neuf_remplace_l_ancien() {
        // Garde-fou de la règle ci-dessus : « vide = garde » ne doit pas
        // devenir « on ne peut plus changer de mot de passe ».
        let (mut admin, _) = admin_de_test();
        admin.set_data(partage("ancien")).await.unwrap();
        admin.set_data(partage("nouveau")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("nas.cred")).unwrap();
        assert!(cred.contains("password=nouveau"), "{cred}");
        assert!(!cred.contains("ancien"), "{cred}");
    }

    #[tokio::test]
    async fn une_racine_invalide_est_refusee_par_une_phrase_qui_nomme_le_fautif() {
        let (mut admin, _) = admin_de_test();
        let err = admin
            .set_data(serde_json::json!({
                "op": "save_roots",
                "roots": [{"name":"nas","kind":"smb","host":"nas,uid=0","share":"s","user":"u"}]
            }))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "cle brute renvoyee a l'ecran : {err}");
        assert!(err.contains("nas,uid=0"), "le refus doit nommer ce qui cloche : {err}");
    }

    #[tokio::test]
    async fn une_racine_refusee_ne_laisse_aucun_fichier_d_identifiants() {
        // La validation passe **avant** toute écriture : un fichier posé pour
        // une racine ensuite refusée resterait orphelin sur le disque, avec un
        // mot de passe dedans.
        let (mut admin, _) = admin_de_test();
        let _ = admin
            .set_data(serde_json::json!({
                "op": "save_roots",
                "roots": [{"name":"BAD NAME","kind":"smb","host":"h","share":"s",
                           "user":"u","password":"p"}]
            }))
            .await
            .unwrap_err();
        assert!(!admin.creds_dir.join("BAD NAME.cred").exists());
        assert!(!admin.roots_path.exists(), "la table ne doit pas non plus avoir ete ecrite");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn le_fichier_d_identifiants_est_ecrit_en_0600() {
        // Permissions posées à la création, pas après : créer puis restreindre
        // laisserait une fenêtre pendant laquelle le mot de passe serait
        // lisible par tout le monde.
        use std::os::unix::fs::PermissionsExt;
        let (mut admin, _) = admin_de_test();
        admin.set_data(partage("secret")).await.unwrap();
        let meta = std::fs::metadata(admin.creds_dir.join("nas.cred")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn la_table_enregistree_se_relit_telle_quelle() {
        let (mut admin, _) = admin_de_test();
        admin.set_data(partage("p")).await.unwrap();
        let relue = Roots::load(&admin.roots_path).unwrap();
        assert_eq!(relue.root.len(), 1);
        assert_eq!(relue.root[0].host, "192.168.1.20");
        // Et le mot de passe n'y figure pas : il vit dans le fichier
        // d'identifiants, que `mount.cifs` lira seul.
        let toml = std::fs::read_to_string(&admin.roots_path).unwrap();
        assert!(!toml.contains('p') || !toml.contains("password"), "{toml}");
    }

    #[tokio::test]
    async fn retirer_une_piste_avant_celle_qui_joue_decale_l_index() {
        // Sans ce décalage, toute la numérotation glisserait sous les pieds de
        // l'auditeur : la piste 4 deviendrait la 3 alors qu'on écoute toujours
        // la même.
        let (admin, _) = admin_de_test();
        {
            let mut liste = admin.playlist.write().await;
            liste.entries = (1..=4)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/m/{i}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect();
            liste.index = 2;
        }
        let mut admin = admin;
        admin.set_data(serde_json::json!({"op": "remove", "index": 0})).await.unwrap();
        let liste = admin.playlist.read().await;
        assert_eq!(liste.entries.len(), 3);
        assert_eq!(liste.index, 1, "la piste ecoutee doit rester la meme");
    }

    #[tokio::test]
    async fn vider_la_liste_efface_aussi_les_entrees_irresolues() {
        // Elles décrivaient la liste précédente : les laisser afficherait un
        // avertissement sans objet, que rien ne viendrait effacer.
        let (mut admin, _) = admin_de_test();
        admin.unresolved.lock().unwrap().push("Z:\\absent.mp3".into());
        admin.set_data(serde_json::json!({"op": "clear"})).await.unwrap();
        assert!(admin.unresolved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn un_chemin_qui_sort_de_la_racine_est_refuse() {
        // La garde d'évasion : `path` vient du navigateur à chaque requête, et
        // un `../..` y ferait parcourir puis ajouter des fichiers hors de toute
        // racine déclarée.
        let (mut admin, racine) = admin_de_test();
        *admin.roots.write().await = Roots {
            root: vec![Root {
                name: "local".into(),
                kind: RootKind::Local,
                path: Some(racine.join("media").display().to_string()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable: false,
            }],
        };
        let err = admin
            .set_data(serde_json::json!({"op": "browse", "root": "local", "path": "../.."}))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "cle brute : {err}");
    }

    #[tokio::test]
    async fn parcourir_range_le_contenu_pour_get_data() {
        // `set_data` ne rend qu'un Ok/Err : le contenu doit voyager par
        // `get_data`, sans quoi la page n'aurait aucun moyen de l'obtenir.
        let (mut admin, racine) = admin_de_test();
        std::fs::create_dir_all(racine.join("media/Album")).unwrap();
        std::fs::write(racine.join("media/Album/01.mp3"), b"").unwrap();
        std::fs::write(racine.join("media/notes.txt"), b"").unwrap();
        *admin.roots.write().await = Roots {
            root: vec![Root {
                name: "local".into(),
                kind: RootKind::Local,
                path: Some(racine.join("media").display().to_string()),
                host: String::new(),
                share: String::new(),
                subpath: None,
                user: String::new(),
                domain: String::new(),
                writable: false,
            }],
        };
        admin
            .set_data(serde_json::json!({"op": "browse", "root": "local", "path": ""}))
            .await
            .unwrap();
        let data = admin.get_data().await;
        assert_eq!(data["browse"]["dirs"], serde_json::json!(["Album"]));
        // `notes.txt` n'est pas un fichier audio : il n'a rien à faire dans un
        // arbre de navigation musicale.
        assert_eq!(data["browse"]["files"], serde_json::json!([]));
    }
}
