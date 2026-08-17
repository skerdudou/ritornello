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
use ritornello_plugin_files::sante::Sante;
use ritornello_plugin_files::store::{self, Location};
use ritornello_plugin_files::volumes;
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

/// Avancement du sondage des durées, tel que la page le lit.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DureesProgress {
    pub running: bool,
    pub done: usize,
    pub total: usize,
}

/// Combien de pistes sont sondées avant de reprendre le verrou.
///
/// Ni une par une — le verrou serait pris des milliers de fois, en concurrence
/// avec la lecture — ni toutes d'un coup, qui ne montrerait aucun avancement et
/// perdrait tout si le sondage était abandonné en route.
const LOT_DE_SONDAGE: usize = 25;

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
    /// L'assistant en cours. Vit ici plutôt que dans son propre verrou : une
    /// seule popin est ouverte à la fois, et le protocole admin est
    /// séquentiel.
    pub explore: ritornello_plugin_files::explore::Explorateur,
    /// Résultat de la dernière réconciliation de montage.
    ///
    /// Le montage suit désormais la déclaration : l'utilisateur ne clique plus
    /// « Monter ». Un échec ne doit donc pas se perdre — sans ce champ, une
    /// source déclarée resterait « non montée » sans jamais dire pourquoi.
    pub mount_error: Arc<Mutex<Option<String>>>,
    /// `smbclient` est-il utilisable. Sondé au démarrage, resondé à chaque
    /// tentative de connexion.
    pub smb_ok: Arc<std::sync::atomic::AtomicBool>,
    /// La liste a changé depuis que la moitié Source l'a confiée à mpv.
    ///
    /// Partagé avec elle : c'est le seul canal disponible, les notifications du
    /// SDK ne pouvant pas porter d'action.
    pub liste_changee: Arc<std::sync::atomic::AtomicBool>,
    /// La moitié Source joue-t-elle en ce moment.
    ///
    /// Sert à la page pour décider si vider la liste doit aussi demander l'arrêt
    /// au cœur : le faire alors qu'une autre source joue couperait celle-là.
    pub joue: Arc<std::sync::atomic::AtomicBool>,
    /// Disjoncteur des chemins média.
    ///
    /// Toute lecture du système de fichiers déclenchée par une requête admin
    /// **doit** passer par lui. Le protocole admin est sériel et le cœur
    /// abandonne au bout de cinq secondes : un seul `is_file` qui n'aboutit pas
    /// coince le plugin entier, page comprise. Voir `sante` pour la mesure.
    pub sante: Arc<Sante>,
    /// Avancement du sondage des durées.
    pub durees: Arc<Mutex<DureesProgress>>,
    /// Sondage en cours. En lancer un nouveau **abandonne** le précédent : après
    /// un chargement de liste, sonder l'ancienne ne sert plus à rien.
    pub durees_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Déclaration d'une source, d'un seul geste : l'assistant a déjà tout
    /// recueilli, il n'y a plus de table à réécrire ni de nom à saisir.
    ///
    /// Le mot de passe ne voyage que dans ce sens-là : `Root` ne le porte pas,
    /// donc `get_data` ne peut pas le rendre par inadvertance, même si
    /// quelqu'un ajoute un champ plus tard.
    AddSource {
        kind: RootKind,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        host: String,
        #[serde(default)]
        share: String,
        #[serde(default)]
        subpath: Option<String>,
        #[serde(default)]
        user: String,
        #[serde(default)]
        domain: String,
        /// **Vide veut dire « prends celui de la session, à défaut celui déjà
        /// enregistré »**. La page ne peut pas renvoyer un secret qu'elle ne
        /// reçoit jamais, et l'assistant ne doit pas le faire retaper à la
        /// confirmation alors qu'il vient de servir à se connecter.
        #[serde(default)]
        password: String,
        #[serde(default)]
        writable: bool,
    },
    RemoveSource {
        name: String,
    },
    SetWritable {
        name: String,
        writable: bool,
    },
    ExploreOpen {
        kind: ritornello_plugin_files::explore::Kind,
    },
    ExploreClose,
    ExploreLocal {
        path: String,
    },
    SmbConnect {
        host: String,
        #[serde(default)]
        user: String,
        #[serde(default)]
        password: String,
        #[serde(default)]
        domain: String,
    },
    SmbBrowse {
        share: String,
        #[serde(default)]
        path: String,
    },
    /// Retour à la liste des partages déjà obtenue, sans nouvel appel réseau.
    SmbShares,
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
    /// Charge un `.m3u` **trouvé en parcourant une source**, désigné par son
    /// chemin — par opposition à `LoadPlaylist`, qui va chercher une liste
    /// *enregistrée* par son nom dans un magasin.
    LoadM3u { root: String, path: String },
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
        drop(roots);
        // Comparaison sur les formes canonisées : c'est la seule qui résiste
        // aux liens symboliques, un `.` ou `..` textuel pouvant être neutralisé
        // par le système de fichiers lui-même.
        //
        // Sous disjoncteur, parce que `canonicalize` touche le disque : sur un
        // partage en reconnexion il ne rend pas la main, et il est ici sur le
        // chemin de **tout** `set_data` visant une racine. Un refus net vaut
        // mieux qu'une boucle admin coincée, qui emporterait la page avec elle.
        let (b, c) = (base.clone(), cible.clone());
        let Some(canon) = self.sante.borne(&cible, move || Ok((b.canonicalize()?, c.canonicalize()?))).await
        else {
            return Err(self
                .mot("root_unresponsive")
                .replace("{path}", &cible.display().to_string()));
        };
        let Ok::<(PathBuf, PathBuf), std::io::Error>((base_c, cible_c)) = canon else {
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
        // mpv joue une **copie** de la liste, écrite au dernier `Play`. Toute
        // modification l'en écarte, et la moitié Admin ne peut rien lui dire :
        // le SDK interdit aux notifications de porter une action. Ce drapeau est
        // donc le seul moyen de prévenir la moitié Source, qui rendra la liste à
        // jour au prochain ordre qu'elle recevra.
        self.liste_changee.store(true, Ordering::Relaxed);
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

    /// Réconcilie les montages, **et seulement s'il y a quelque chose à
    /// monter ou à démonter**.
    ///
    /// Sans cette garde, déclarer un simple dossier de l'appareil demandait à
    /// systemd de lancer l'unité de montage, laquelle exige une autorisation
    /// polkit : la page affichait alors « la dernière tentative de montage a
    /// échoué — authentification interactive requise » à quelqu'un qui venait
    /// d'ajouter une clé USB et n'avait rien demandé de tel. Un message
    /// alarmant pour un travail qui n'avait pas lieu d'être.
    ///
    /// `aussi` couvre le retrait : la source qui part peut être le dernier
    /// partage de la table, et il faut encore la démonter.
    async fn reconcilier(&self, table: &Roots, aussi: bool) {
        if !aussi && !table.root.iter().any(|r| r.kind == RootKind::Smb) {
            *self.mount_error.lock().unwrap() = None;
            return;
        }
        *self.mount_error.lock().unwrap() = mount::reconcile(mount::UNIT).await.err();
    }

    /// Lance le sondage des durées manquantes, en tâche de fond.
    ///
    /// En tâche de fond parce qu'il n'y a pas le choix : le protocole admin a un
    /// plafond de 5 s, et une liste de deux mille pistes venue d'un partage
    /// demande davantage. La page suit l'avancement par sondage, exactement comme
    /// pour le balayage.
    ///
    /// Ne sonde que ce qui manque : une durée venue d'un `#EXTINF` ou d'un
    /// sondage antérieur est conservée, et `StoredEntry` la persiste — un
    /// redémarrage ne resonde donc rien.
    ///
    /// Les résultats sont appliqués **par chemin** et non par index : la page
    /// peut réordonner ou retirer des pistes pendant le sondage, et appliquer par
    /// position écrirait la durée d'un fichier sur un autre.
    fn lancer_sondage(
        playlist: Arc<AsyncRwLock<Playlist>>,
        durees: Arc<Mutex<DureesProgress>>,
        state_path: PathBuf,
        sante: Arc<Sante>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let a_sonder: Vec<PathBuf> = {
                let liste = playlist.read().await;
                let mut v: Vec<PathBuf> = liste
                    .entries
                    .iter()
                    .filter(|e| e.duration_s.is_none())
                    .map(|e| e.path.clone())
                    .collect();
                // Un même fichier peut figurer deux fois : le sonder une seule
                // fois suffit, la durée sera posée sur toutes ses occurrences.
                v.sort();
                v.dedup();
                v
            };
            if a_sonder.is_empty() {
                *durees.lock().unwrap() = DureesProgress::default();
                return;
            }
            *durees.lock().unwrap() =
                DureesProgress { running: true, done: 0, total: a_sonder.len() };

            let mut faits = 0usize;
            // Découpé en lots **par point de montage** : un lot ne mélange ainsi
            // jamais deux partages, et le disjoncteur d'un montage muet écarte
            // aussitôt tous ses lots suivants sans rien exécuter — sans quoi
            // chaque lot repartirait attendre le même partage.
            let lots: Vec<Vec<PathBuf>> = sante
                .grouper(&a_sonder)
                .into_iter()
                .flat_map(|(_, indices)| {
                    let chemins: Vec<PathBuf> =
                        indices.iter().map(|&i| a_sonder[i].clone()).collect();
                    chemins.chunks(LOT_DE_SONDAGE).map(<[PathBuf]>::to_vec).collect::<Vec<_>>()
                })
                .collect();
            for lot in lots {
                let repere = lot[0].clone();
                // Sous disjoncteur, et pas seulement `spawn_blocking` : sortir
                // du fil asynchrone protège la boucle admin, mais pas le pool.
                // Sur un partage bloqué, chaque relance de `resonder` y perdait
                // un fil de plus, sans jamais en récupérer un — le disjoncteur
                // est ce qui borne cette fuite à un fil par point de montage.
                let mesures = sante
                    .borne(&repere, move || {
                        lot.into_iter()
                            .map(|p| {
                                let d = ritornello_plugin_files::duree::sonder(&p);
                                (p, d)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                // Ce montage ne répond pas : passer au lot suivant, sans
                // abandonner le sondage — les pistes locales de la même liste
                // doivent aboutir. Les lots restants du même partage seront
                // écartés sans frais par le disjoncteur.
                //
                // `faits` n'avance pas pour un lot sauté : la page affichera
                // moins de durées relevées que de pistes, ce qui est la vérité.
                let Some(mesures) = mesures else { continue };

                {
                    let mut liste = playlist.write().await;
                    for (chemin, duree) in &mesures {
                        let Some(d) = duree else { continue };
                        for e in liste.entries.iter_mut() {
                            // `is_none` à nouveau : entre le relevé et
                            // maintenant, un chargement a pu poser une durée.
                            if e.path == *chemin && e.duration_s.is_none() {
                                e.duration_s = Some(*d);
                            }
                        }
                    }
                    let stockees: Vec<state::StoredEntry> =
                        liste.entries.iter().map(state::StoredEntry::from).collect();
                    let index = liste.index;
                    drop(liste);
                    // Persister à chaque lot : un sondage interrompu à mi-course
                    // garde ce qu'il a déjà trouvé, au lieu de tout refaire.
                    if let Err(e) = state::update(&state_path, |s| {
                        s.playlist = stockees;
                        s.index = index;
                    }) {
                        tracing::warn!("persisting track lengths: {e}");
                    }
                }

                faits += mesures.len();
                let mut p = durees.lock().unwrap();
                p.done = faits;
            }
            let mut p = durees.lock().unwrap();
            p.running = false;
        })
    }

    /// Relance le sondage, en abandonnant celui qui tournait.
    fn resonder(&mut self) {
        if let Some(t) = self.durees_task.take() {
            t.abort();
        }
        self.durees_task = Some(Self::lancer_sondage(
            self.playlist.clone(),
            self.durees.clone(),
            self.state_path.clone(),
            self.sante.clone(),
        ));
    }

    /// Écrit la table des racines, atomiquement.
    ///
    /// Le fichier temporaire puis le renommage : une coupure de courant au
    /// milieu d'une écriture directe laisserait une table tronquée, que le
    /// démarrage suivant refuserait — donc plus aucune source.
    fn ecrire_table(&self, table: &Roots) -> Result<(), String> {
        let texte = toml::to_string_pretty(table).map_err(|e| {
            tracing::warn!("serialising the roots table: {e}");
            self.mot("store_io_error").replace("{path}", &self.roots_path.display().to_string())
        })?;
        let tmp = self.roots_path.with_extension("toml.tmp");
        std::fs::write(&tmp, texte).and_then(|_| std::fs::rename(&tmp, &self.roots_path)).map_err(
            |e| {
                tracing::warn!("saving the roots table: {e}");
                self.mot("store_io_error").replace("{path}", &self.roots_path.display().to_string())
            },
        )
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
        // Le stockage interne est local par construction (`/var/lib`) : il se
        // lit directement. Les racines, elles, peuvent être des partages, donc
        // chacune sous son propre disjoncteur — un `read_dir` par racine et par
        // sondage de la page était l'un des deux appels qui ont coincé le
        // plugin le 2026-08-17.
        let mut sauvegardees = store::dans(&self.internal_playlists, Location::Internal);
        let a_ramasser: Vec<(PathBuf, String)> =
            roots.root.iter().map(|r| (r.base_dir(), r.name.clone())).collect();
        drop(roots);
        for (dir, nom) in a_ramasser {
            let d = dir.clone();
            if let Some(v) =
                self.sante.borne(&dir, move || store::dans(&d, Location::Root(nom))).await
            {
                sauvegardees.extend(v);
            }
        }

        let liste = self.playlist.read().await;
        let chemins: Vec<PathBuf> = liste.entries.iter().map(|e| e.path.clone()).collect();
        let decrites: Vec<(String, String, Option<u32>)> = liste
            .entries
            .iter()
            .map(|e| (e.path.to_string_lossy().into_owned(), e.display_name(), e.duration_s))
            .collect();
        let index = liste.index;
        drop(liste);
        // Groupé par point de montage et borné : un seul délai couvre toutes les
        // pistes d'un partage, au lieu d'un appel bloquant par piste.
        let manquants = self.sante.manquants(&chemins).await;
        let pistes: Vec<serde_json::Value> = decrites
            .into_iter()
            .zip(manquants)
            .map(|((path, name, duration_s), manque)| {
                serde_json::json!({
                    "path": path,
                    "name": name,
                    "duration_s": duration_s,
                    // Marquée, jamais masquée : une liste qui rétrécit sans
                    // rien dire est un défaut qu'on met des mois à attribuer.
                    //
                    // `null` quand le montage ne répond pas : dire
                    // « introuvable » accuserait les fichiers d'une panne qui
                    // est celle du partage, et enverrait chercher le défaut au
                    // mauvais endroit. La page l'affiche comme indéterminé.
                    "missing": manque,
                })
            })
            .collect();

        // Gardes `std::sync` prises après le dernier `.await` : aucune ne
        // traverse un point d'attente.
        let scan = self.scan.lock().unwrap().clone();
        let unresolved = self.unresolved.lock().unwrap().clone();
        let browse = self.browse.lock().unwrap().clone();
        let volumes = volumes::volumes(&volumes::lire_proc_mounts());
        let mount_error = self.mount_error.lock().unwrap().clone();
        let can_browse_smb = self.smb_ok.load(std::sync::atomic::Ordering::Relaxed);
        let explore = self.explore.vue();
        serde_json::json!({
            "roots": racines,
            "volumes": volumes,
            "can_browse_smb": can_browse_smb,
            // Ce que la page en fait : décider si vider la liste doit aussi
            // demander l'arrêt. Sans cette information elle couperait la radio
            // en vidant une liste de fichiers qui ne jouait pas.
            "playing": self.joue.load(std::sync::atomic::Ordering::Relaxed),
            // Avancement du sondage des durées : c'est ce qui fait sonder la page
            // le temps qu'elles arrivent, puis cesser.
            "durations": self.durees.lock().unwrap().clone(),
            "explore": explore,
            "mount_error": mount_error,
            // Points de montage dont une sonde n'est jamais revenue. Dits à la
            // page pour qu'elle explique le silence : sans eux, l'utilisateur
            // voit des durées qui n'arrivent pas et des états indéterminés sans
            // aucune indication de cause.
            "unresponsive": self.sante.muets().iter()
                .map(|p| p.display().to_string()).collect::<Vec<_>>(),
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
            Op::AddSource {
                kind,
                path,
                host,
                share,
                subpath,
                user,
                domain,
                password,
                writable,
            } => {
                let mut table = self.roots.read().await.clone();
                // Le doublon exact seul est refusé : deux dossiers différents
                // du même partage sont deux sources légitimes, qui montent le
                // partage deux fois — ce qui est légal, peu coûteux, et surtout
                // sans surprise. Fusionner en élargissant le sous-chemin commun
                // modifierait en silence la portée d'une source déjà déclarée.
                let deja = table.root.iter().any(|r| {
                    r.kind == kind
                        && r.host == host
                        && r.share == share
                        && r.subpath == subpath
                        && r.path == path
                });
                if deja {
                    return Err(self.mot("duplicate_source"));
                }
                let pris: Vec<&str> = table.root.iter().map(|r| r.name.as_str()).collect();
                let indice = match kind {
                    RootKind::Smb => share.clone(),
                    RootKind::Local => path
                        .clone()
                        .unwrap_or_default()
                        .rsplit('/')
                        .find(|s| !s.is_empty())
                        .unwrap_or("disque")
                        .to_string(),
                };
                let name = ritornello_plugin_files::roots::derive_name(&indice, &pris);
                let racine = Root {
                    name: name.clone(),
                    kind,
                    path,
                    host: host.clone(),
                    share,
                    subpath,
                    user: user.clone(),
                    domain: domain.clone(),
                    writable,
                };
                table.root.push(racine);
                // Valider **avant** d'écrire quoi que ce soit : un fichier
                // d'identifiants posé pour une source ensuite refusée resterait
                // orphelin sur le disque, avec un mot de passe dedans.
                table.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;

                if kind == RootKind::Smb {
                    let r = table.by_name(&name).expect("tout juste inseree");
                    let chemin = r.credentials_path(&self.creds_dir);
                    let secret = if !password.is_empty() {
                        password
                    } else if let Some(c) = self.explore.credentials(&host) {
                        c.password
                    } else {
                        Self::mot_de_passe_existant(&chemin).unwrap_or_default()
                    };
                    Self::ecrire_identifiants(&chemin, &user, &secret, &domain).map_err(|e| {
                        tracing::warn!("writing credentials for {name}: {e}");
                        self.mot("store_io_error")
                            .replace("{path}", &chemin.display().to_string())
                    })?;
                }
                self.ecrire_table(&table)?;
                // Le montage suit la déclaration : plus de bouton à trouver.
                // Un échec ne défait PAS la déclaration — l'utilisateur perdrait
                // sa saisie à cause d'un NAS endormi — il est rapporté à part.
                self.reconcilier(&table, false).await;
                *self.roots.write().await = table;
                Ok(())
            }

            Op::RemoveSource { name } => {
                let mut table = self.roots.read().await.clone();
                let Some(i) = table.root.iter().position(|r| r.name == name) else {
                    return Err(self.mot("unknown_source").replace("{name}", &name));
                };
                let partie = table.root.remove(i);
                self.ecrire_table(&table)?;
                // Le fichier d'identifiants part avec la source : le laisser
                // ferait survivre un mot de passe à ce qui le justifiait.
                let _ = std::fs::remove_file(partie.credentials_path(&self.creds_dir));
                // `aussi` : la source qui part peut être le dernier partage de
                // la table, et il reste à la démonter.
                self.reconcilier(&table, partie.kind == RootKind::Smb).await;
                *self.roots.write().await = table;
                Ok(())
            }

            Op::SetWritable { name, writable } => {
                let mut table = self.roots.read().await.clone();
                let Some(r) = table.root.iter_mut().find(|r| r.name == name) else {
                    return Err(self.mot("unknown_source").replace("{name}", &name));
                };
                r.writable = writable;
                self.ecrire_table(&table)?;
                // Remonter est indispensable : `ro` est une option de montage,
                // pas un drapeau relu à chaque écriture. Sans réconciliation,
                // autoriser l'écriture ne changerait rien jusqu'au prochain
                // redémarrage.
                self.reconcilier(&table, false).await;
                *self.roots.write().await = table;
                Ok(())
            }

            Op::ExploreOpen { kind } => {
                self.explore.ouvrir(kind);
                Ok(())
            }
            Op::ExploreClose => {
                self.explore.fermer();
                Ok(())
            }
            Op::ExploreLocal { path } => self.explore.local(&path).await,
            Op::SmbConnect { host, user, password, domain } => {
                // Resonder ici : installer le paquet sans redémarrer le service
                // doit donner un résultat juste plutôt qu'un refus périmé.
                self.smb_ok.store(
                    ritornello_plugin_files::smb::available().await,
                    std::sync::atomic::Ordering::Relaxed,
                );
                self.explore.connecter(host, user, password, domain);
                Ok(())
            }
            Op::SmbBrowse { share, path } => {
                self.explore.parcourir(share, path);
                Ok(())
            }
            Op::SmbShares => {
                self.explore.aux_partages();
                Ok(())
            }

            Op::Mount => mount::reconcile(mount::UNIT).await,

            Op::Browse { root, path } => {
                let dir = self.sous_racine(&root, &path).await?;
                let cat = self.catalog.clone();
                let contenu = tokio::task::spawn_blocking(move || scan::list_dir(&dir))
                    .await
                    .map_err(|e| format!("browse task: {e}"))?
                    .map_err(|e| e.message(&cat.read().unwrap()))?;
                *self.browse.lock().unwrap() = serde_json::json!({
                    "root": root,
                    "path": path,
                    "dirs": contenu.dossiers,
                    "files": contenu.audio,
                    // Les listes de lecture voyagent à part : elles ne
                    // s'ajoutent pas à la liste en cours, elles la remplacent.
                    "playlists": contenu.listes,
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
                let changee = self.liste_changee.clone();
                let playlist_pour_durees = self.playlist.clone();
                let durees = self.durees.clone();
                let state_path_pour_durees = self.state_path.clone();
                let sante_pour_durees = self.sante.clone();
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
                                // Même raison que dans `liste_modifiee` : mpv
                                // joue une copie, et ce drapeau est le seul
                                // canal vers la moitié Source.
                                changee.store(true, Ordering::Relaxed);
                                let _ = tx.send(compte);
                                // Le sondage part d'ici et non du gestionnaire :
                                // celui-ci a rendu la main bien avant que la
                                // marche récursive n'ait ajouté quoi que ce soit.
                                // Sa poignée n'est pas conservée — un sondage
                                // concurrent ne fait que du travail en double, il
                                // ne pose jamais de durée fausse.
                                Self::lancer_sondage(
                                    playlist_pour_durees,
                                    durees,
                                    state_path_pour_durees,
                                    sante_pour_durees,
                                );
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
                self.resonder();
                Ok(())
            }

            Op::Remove { index } => {
                let mut liste = self.playlist.write().await;
                if index >= liste.entries.len() {
                    return Err(self.mot("bad_request").replace("{detail}", "index"));
                }
                let ecoutee = liste.index == index;
                liste.entries.remove(index);
                // L'index de lecture suit : retirer une piste avant celle qui
                // joue décalerait sinon toute la numérotation sous les pieds de
                // l'auditeur.
                //
                // Retirer **celle qu'on écoute** est le cas à part : la lecture
                // s'arrête (la page le demande au cœur), et on repart du début.
                // Laisser l'index sur la position libérée gardait la surbrillance
                // sur une piste qu'on n'avait pas choisie — celle qui a glissé à
                // la place de la disparue.
                if ecoutee {
                    liste.index = 0;
                } else if liste.index > index {
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
                // **L'index suit la piste écoutée.** Il ne le faisait pas, et le
                // défaut était visible : réordonner la liste laissait la
                // surbrillance sur une position qui contenait désormais une autre
                // piste — et la moitié Source aurait relancé la mauvaise.
                //
                // Trois cas, et seulement trois : la piste écoutée est celle
                // qu'on déplace, ou bien le déplacement l'enjambe dans un sens,
                // ou dans l'autre.
                liste.index = if liste.index == from {
                    to
                } else if from < liste.index && to >= liste.index {
                    liste.index - 1
                } else if from > liste.index && to <= liste.index {
                    liste.index + 1
                } else {
                    liste.index
                };
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
                // Abandonne un sondage en cours : il portait sur des pistes qui
                // ne sont plus là, et son avancement mentirait à l'écran.
                self.resonder();
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
                self.resonder();
                Ok(())
            }

            Op::LoadM3u { root, path } => {
                // Un m3u trouvé en parcourant une source, par opposition aux
                // listes **enregistrées** que `LoadPlaylist` va chercher par nom
                // dans un magasin. Ici c'est un fichier comme un autre, désigné
                // par son chemin, et la garde d'évasion s'applique donc.
                let fichier = self.sous_racine(&root, &path).await?;
                if !scan::is_playlist(&fichier) {
                    return Err(self.mot("not_a_playlist").replace("{path}", &path));
                }
                let texte = std::fs::read_to_string(&fichier).map_err(|e| {
                    tracing::warn!("reading {}: {e}", fichier.display());
                    self.mot("store_io_error").replace("{path}", &path)
                })?;
                // Les chemins relatifs se résolvent d'abord contre le répertoire
                // **du m3u**, comme le veut le format ; la racine ne sert qu'aux
                // replis (chemin absolu venu d'une autre machine, lettre de
                // lecteur Windows).
                let dossier = fichier.parent().unwrap_or(&fichier).to_path_buf();
                let base = {
                    let roots = self.roots.read().await;
                    roots.by_name(&root).map(|r| r.base_dir()).unwrap_or_else(|| dossier.clone())
                };
                let charge = ritornello_plugin_files::m3u::parse(&texte, &dossier, &base);
                if charge.entries.len() > scan::MAX_TRACKS {
                    return Err(self
                        .mot("too_many_tracks")
                        .replace("{cap}", &scan::MAX_TRACKS.to_string()));
                }
                // Rapportées, jamais supprimées en silence : une liste plus
                // courte que son fichier est un défaut qu'on met des mois à
                // attribuer.
                *self.unresolved.lock().unwrap() = charge.unresolved;
                let mut liste = self.playlist.write().await;
                liste.entries = charge.entries;
                liste.index = 0;
                drop(liste);
                self.liste_modifiee().await;
                // Un m3u peut porter des `#EXTINF`, mais rarement tous : le
                // sondage ne comble que ce qui manque.
                self.resonder();
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
        let catalogue = Arc::new(RwLock::new(Catalog::load(
            "files",
            "en",
            &racine,
            ritornello_plugin_files::FILES_EN,
        )));
        let smb_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sante = Arc::new(ritornello_plugin_files::sante::Sante::new());
        let admin = FilesAdmin {
            roots_path: racine.join("media-roots.toml"),
            creds_dir: racine.join("creds"),
            internal_playlists: racine.join("playlists"),
            state_path: racine.join("plugin-files.json"),
            roots: Arc::new(AsyncRwLock::new(Roots::default())),
            playlist: Arc::new(AsyncRwLock::new(Playlist::default())),
            catalog: catalogue.clone(),
            scan: Arc::new(Mutex::new(ScanProgress::default())),
            scan_task: None,
            unresolved: Arc::new(Mutex::new(Vec::new())),
            browse: Arc::new(Mutex::new(serde_json::json!({}))),
            preset_count_tx: tx,
            liste_changee: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            joue: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            durees: Arc::new(Mutex::new(DureesProgress::default())),
            durees_task: None,
            explore: ritornello_plugin_files::explore::Explorateur::new(
                racine.join("creds"),
                catalogue.clone(),
                smb_ok.clone(),
                sante.clone(),
            ),
            mount_error: Arc::new(Mutex::new(None)),
            smb_ok,
            sante,
        };
        (admin, racine)
    }

    fn ajout_partage(password: &str) -> serde_json::Value {
        serde_json::json!({
            "op": "add_source", "kind": "smb", "host": "192.168.1.20",
            "share": "musique", "subpath": "Ma Musique", "user": "steven",
            "domain": "", "writable": false, "password": password
        })
    }

    #[tokio::test]
    async fn une_source_ajoutee_recoit_un_nom_derive() {
        // L'utilisateur ne saisit plus de nom : il doit être dérivé, valide, et
        // dérivé du partage pour rester lisible dans /mnt/ritornello.
        let (mut admin, _) = admin_de_test();
        admin.set_data(ajout_partage("p")).await.unwrap();
        let roots = admin.roots.read().await;
        assert_eq!(roots.root.len(), 1);
        assert_eq!(roots.root[0].name, "musique");
        assert_eq!(roots.root[0].subpath.as_deref(), Some("Ma Musique"));
    }

    #[tokio::test]
    async fn ajouter_un_dossier_local_ne_demande_aucun_montage() {
        // Défaut trouvé par le parcours de bout en bout, et invisible ici sans
        // ce test : la réconciliation partait à chaque déclaration, y compris
        // pour un dossier de l'appareil. Elle exige polkit, donc elle échouait,
        // et la page annonçait « la dernière tentative de montage a échoué —
        // authentification interactive requise » à quelqu'un qui venait
        // simplement de brancher une clé USB.
        let dir = tempfile::tempdir().unwrap();
        let (mut admin, _) = admin_de_test();
        admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "local",
                "path": dir.path().display().to_string(),
                "host": "", "share": "", "user": "", "domain": "",
                "password": "", "writable": false
            }))
            .await
            .unwrap();
        assert_eq!(admin.roots.read().await.root.len(), 1);
        assert!(
            admin.mount_error.lock().unwrap().is_none(),
            "aucun montage n'a lieu d'etre tente sans le moindre partage declare"
        );
    }

    #[tokio::test]
    async fn deux_sources_du_meme_partage_ne_se_disputent_pas_leur_nom() {
        // Sans dédoublonnage, la deuxième écraserait le fichier d'identifiants de
        // la première et se disputerait son point de montage.
        let (mut admin, _) = admin_de_test();
        admin.set_data(ajout_partage("p")).await.unwrap();
        let mut second = ajout_partage("p");
        second["subpath"] = serde_json::json!("Rock");
        admin.set_data(second).await.unwrap();
        let roots = admin.roots.read().await;
        assert_eq!(roots.root.len(), 2);
        assert_ne!(roots.root[0].name, roots.root[1].name);
    }

    #[tokio::test]
    async fn le_doublon_exact_est_refuse() {
        // Deux sources identiques monteraient deux fois le même partage au même
        // endroit logique, sans qu'aucune ne serve à rien de plus.
        let (mut admin, _) = admin_de_test();
        admin.set_data(ajout_partage("p")).await.unwrap();
        let err = admin.set_data(ajout_partage("p")).await.unwrap_err();
        assert!(err.contains(' '), "cle brute : {err}");
    }

    #[tokio::test]
    async fn retirer_une_source_efface_son_fichier_d_identifiants() {
        // Sinon un .cred contenant un mot de passe survivrait sur le disque à la
        // source qui l'a justifié.
        let (mut admin, _) = admin_de_test();
        admin.set_data(ajout_partage("secret")).await.unwrap();
        let cred = admin.creds_dir.join("musique.cred");
        assert!(cred.exists());
        admin
            .set_data(serde_json::json!({"op": "remove_source", "name": "musique"}))
            .await
            .unwrap();
        assert!(!cred.exists(), "le fichier d'identifiants a survecu a la source");
        assert!(admin.roots.read().await.root.is_empty());
    }

    #[tokio::test]
    async fn get_data_annonce_les_volumes_et_la_capacite_smb() {
        let (admin, racine) = admin_de_test();
        let faux = racine.join("mounts");
        std::fs::write(&faux, "/dev/sda1 /media/usb vfat rw 0 0\nproc /proc proc rw 0 0\n").unwrap();
        std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &faux);
        let d = admin.get_data().await;
        assert_eq!(d["volumes"][0]["path"], "/media/usb");
        assert_eq!(d["volumes"].as_array().unwrap().len(), 1, "proc ne doit pas etre propose");
        assert!(d["can_browse_smb"].is_boolean());
        assert!(d["explore"].is_object());
        std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
    }

    /// `/proc/mounts` de test : une racine locale et un partage à part, pour que
    /// le silence de l'un n'emporte pas l'autre.
    const MOUNTS_MUETS: &str = "/dev/root / ext4 rw 0 0\n\
                                //192.168.1.15/musique /mnt/ritornello/nas cifs ro,soft 0 0\n";

    #[tokio::test]
    async fn get_data_rend_la_main_et_ne_mente_pas_quand_un_montage_est_muet() {
        // Le test de non-régression de l'incident du 2026-08-17 : `get_data`
        // faisait un `is_file` par piste et un `read_dir` par racine, sur le fil
        // asynchrone. Le protocole admin étant sériel, un montage cifs bloqué
        // dans le noyau y a coincé le plugin entier — jusqu'à faire expirer
        // `ui.js`, qui n'est qu'un `include_str!`.
        let (mut admin, _r) = admin_de_test();
        admin.sante = Arc::new(ritornello_plugin_files::sante::Sante::pour_test(
            std::time::Duration::from_millis(50),
            MOUNTS_MUETS.to_string(),
            vec![PathBuf::from("/mnt/ritornello/nas")],
        ));
        admin.playlist.write().await.entries = vec![
            Entry { path: PathBuf::from("/mnt/ritornello/nas/a.mp3"), title: None, duration_s: None },
            Entry { path: PathBuf::from("/home/pi/absent.mp3"), title: None, duration_s: None },
        ];

        let debut = std::time::Instant::now();
        let d = admin.get_data().await;
        assert!(debut.elapsed() < std::time::Duration::from_secs(1), "{:?}", debut.elapsed());

        // `null` et non `true` : c'est tout l'objet du correctif. Dire
        // « introuvable » pour un partage endormi accuserait les fichiers d'une
        // panne qui est celle du montage. Un `is_file` direct rendrait `true`
        // ici, et ce test tomberait — c'est ce qui le rend utile.
        assert!(d["playlist"][0]["missing"].is_null(), "{}", d["playlist"][0]);
        // La piste locale, elle, reste jugée : le disjoncteur d'un montage ne
        // doit pas rendre les autres indéterminés.
        assert_eq!(d["playlist"][1]["missing"], serde_json::json!(true));
        assert_eq!(d["unresponsive"], serde_json::json!(["/mnt/ritornello/nas"]));
    }

    #[tokio::test]
    async fn basculer_l_inscriptibilite_ne_perd_pas_le_mot_de_passe() {
        // Sans cette opération, changer d'avis imposerait de retirer puis
        // redéclarer, donc de resaisir le mot de passe.
        let (mut admin, _) = admin_de_test();
        admin.set_data(ajout_partage("secret-du-nas")).await.unwrap();
        admin
            .set_data(serde_json::json!({"op": "set_writable", "name": "musique", "writable": true}))
            .await
            .unwrap();
        assert!(admin.roots.read().await.by_name("musique").unwrap().writable);
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=secret-du-nas"), "{cred}");
    }

    #[tokio::test]
    async fn get_data_ne_rend_jamais_le_mot_de_passe() {
        // Il n'a aucune raison de traverser vers le navigateur, et la page n'en
        // a pas besoin pour afficher l'état d'un partage. La garantie est
        // portée par le type : ni `Root` ni la vue de l'assistant ne contiennent
        // le champ.
        let (mut admin, _) = admin_de_test();
        admin.set_data(ajout_partage("secret-du-nas")).await.unwrap();
        let texte = serde_json::to_string(&admin.get_data().await).unwrap();
        assert!(!texte.contains("password"), "{texte}");
        assert!(!texte.contains("secret-du-nas"), "{texte}");
    }

    #[tokio::test]
    async fn un_mot_de_passe_vide_reprend_celui_de_la_session() {
        // L'assistant vient de s'en servir pour se connecter : le faire retaper
        // à la confirmation serait une saisie de plus pour rien, et la page ne
        // peut pas renvoyer un secret qu'elle ne reçoit jamais.
        let (mut admin, _) = admin_de_test();
        admin.explore.ouvrir(ritornello_plugin_files::explore::Kind::Smb);
        admin.explore.connecter(
            "192.168.1.20".into(),
            "steven".into(),
            "secret-du-nas".into(),
            String::new(),
        );
        admin.set_data(ajout_partage("")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=secret-du-nas"), "{cred}");
    }

    #[tokio::test]
    async fn un_mot_de_passe_vide_conserve_celui_deja_enregistre() {
        // Dernier repli, quand la popin a été fermée entre-temps : redéclarer
        // une source du même nom ne doit pas casser en silence un montage qui
        // marchait, faute de mot de passe.
        let (mut admin, _) = admin_de_test();
        std::fs::create_dir_all(&admin.creds_dir).unwrap();
        std::fs::write(
            admin.creds_dir.join("musique.cred"),
            "username=steven\npassword=secret-du-nas\n",
        )
        .unwrap();
        admin.set_data(ajout_partage("")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=secret-du-nas"), "{cred}");
    }

    #[tokio::test]
    async fn un_mot_de_passe_neuf_remplace_l_ancien() {
        // Garde-fou de la règle ci-dessus : « vide = garde » ne doit pas
        // devenir « on ne peut plus changer de mot de passe ».
        let (mut admin, _) = admin_de_test();
        std::fs::create_dir_all(&admin.creds_dir).unwrap();
        std::fs::write(admin.creds_dir.join("musique.cred"), "username=steven\npassword=ancien\n")
            .unwrap();
        admin.set_data(ajout_partage("nouveau")).await.unwrap();
        let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
        assert!(cred.contains("password=nouveau"), "{cred}");
        assert!(!cred.contains("ancien"), "{cred}");
    }

    #[tokio::test]
    async fn une_source_invalide_est_refusee_par_une_phrase_qui_nomme_le_fautif() {
        let (mut admin, _) = admin_de_test();
        let err = admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "smb", "host": "nas,uid=0",
                "share": "musique", "user": "u"
            }))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "cle brute renvoyee a l'ecran : {err}");
        assert!(err.contains("nas,uid=0"), "le refus doit nommer ce qui cloche : {err}");
    }

    #[tokio::test]
    async fn une_source_refusee_ne_laisse_aucun_fichier_d_identifiants() {
        // La validation passe **avant** toute écriture : un fichier posé pour
        // une source ensuite refusée resterait orphelin sur le disque, avec un
        // mot de passe dedans.
        let (mut admin, _) = admin_de_test();
        let _ = admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "smb", "host": "nas,uid=0",
                "share": "musique", "user": "u", "password": "p"
            }))
            .await
            .unwrap_err();
        assert!(!admin.creds_dir.join("musique.cred").exists());
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
        admin.set_data(ajout_partage("secret")).await.unwrap();
        let meta = std::fs::metadata(admin.creds_dir.join("musique.cred")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn la_table_enregistree_se_relit_telle_quelle() {
        let (mut admin, _) = admin_de_test();
        admin.set_data(ajout_partage("p")).await.unwrap();
        let relue = Roots::load(&admin.roots_path).unwrap();
        assert_eq!(relue.root.len(), 1);
        assert_eq!(relue.root[0].host, "192.168.1.20");
        // Et le mot de passe n'y figure pas : il vit dans le fichier
        // d'identifiants, que `mount.cifs` lira seul.
        let toml = std::fs::read_to_string(&admin.roots_path).unwrap();
        assert!(!toml.contains("password"), "{toml}");
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

    /// Attend la fin du sondage des durées, ou abandonne au bout d'un délai.
    ///
    /// Le sondage est **asynchrone** à dessein : le protocole admin a un plafond
    /// de 5 s, et une liste venue d'un partage demande davantage. Un test doit
    /// donc l'attendre, et non supposer qu'il a fini au retour de l'opération.
    async fn attendre_les_durees(admin: &FilesAdmin) {
        for _ in 0..200 {
            let p = admin.durees.lock().unwrap().clone();
            if p.total > 0 && !p.running {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("le sondage des durees n'a jamais abouti");
    }

    /// Fabrique un mp3 réel, ou rend `None` si ffmpeg manque.
    fn mp3_de(secondes: u32, chemin: &Path) -> Option<()> {
        std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={secondes}"))
            .arg(chemin)
            .status()
            .ok()
            .filter(|s| s.success())
            .map(|_| ())
    }

    #[tokio::test]
    async fn ajouter_un_fichier_sonde_sa_duree_en_tache_de_fond() {
        // La demande : les durées manquantes se remplissent d'elles-mêmes, sans
        // bloquer l'ajout — un dossier de mille pistes dépasserait le plafond de
        // 5 s du cœur.
        let (mut admin, racine) = admin_avec_racine_locale().await;
        let media = racine.join("media");
        std::fs::create_dir_all(&media).unwrap();
        if mp3_de(3, &media.join("piste.mp3")).is_none() {
            eprintln!("ffmpeg absent : test saute");
            return;
        }
        admin
            .set_data(serde_json::json!({
                "op": "add_file", "root": "local", "path": "piste.mp3"
            }))
            .await
            .unwrap();
        attendre_les_durees(&admin).await;
        let liste = admin.playlist.read().await;
        let d = liste.entries[0].duration_s.expect("une duree attendue");
        assert!((2..=4).contains(&d), "duree lue {d}");
    }

    #[tokio::test]
    async fn une_duree_deja_connue_nest_pas_ecrasee() {
        // Celles d'un `#EXTINF` sont l'autorité : le fichier peut être un extrait,
        // et resonder par-dessus effacerait ce que la liste affirmait.
        let (admin, _) = admin_avec_racine_locale().await;
        {
            let mut liste = admin.playlist.write().await;
            liste.entries = vec![Entry {
                path: PathBuf::from("/m/inexistant.mp3"),
                title: None,
                duration_s: Some(245),
            }];
        }
        let mut admin = admin;
        admin.resonder();
        // Rien à sonder : le sondage se termine sans rien toucher.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(admin.playlist.read().await.entries[0].duration_s, Some(245));
        assert_eq!(admin.durees.lock().unwrap().total, 0, "rien n'avait a etre sonde");
    }

    #[tokio::test]
    async fn les_durees_sondees_sont_persistees() {
        // Sans persistance, chaque redémarrage resonderait toute la liste — des
        // milliers de lectures d'en-tête sur un partage, pour rien.
        let (mut admin, racine) = admin_avec_racine_locale().await;
        let media = racine.join("media");
        std::fs::create_dir_all(&media).unwrap();
        if mp3_de(2, &media.join("p.mp3")).is_none() {
            eprintln!("ffmpeg absent : test saute");
            return;
        }
        admin
            .set_data(serde_json::json!({"op": "add_file", "root": "local", "path": "p.mp3"}))
            .await
            .unwrap();
        attendre_les_durees(&admin).await;
        let etat = state::load(&admin.state_path);
        assert!(etat.playlist[0].duration_s.is_some(), "la duree doit survivre au redemarrage");
    }

    #[tokio::test]
    async fn retirer_la_piste_ecoutee_repart_du_debut() {
        // Défaut signalé : l'index restait sur la position libérée, donc la
        // surbrillance se posait sur la piste qui avait glissé à la place de la
        // disparue — une piste que l'utilisateur n'avait pas choisie. On repart
        // du début, ce qui va de pair avec l'arrêt que la page demande.
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
        admin.set_data(serde_json::json!({"op": "remove", "index": 2})).await.unwrap();
        let liste = admin.playlist.read().await;
        assert_eq!(liste.index, 0, "on repart du debut");
        assert_eq!(liste.entries.len(), 3);
    }

    #[tokio::test]
    async fn reordonner_la_liste_garde_la_surbrillance_sur_la_piste_ecoutee() {
        // Défaut signalé à l'usage : `move` échangeait les pistes sans toucher à
        // l'index. La surbrillance restait sur une position qui contenait
        // désormais autre chose, et la moitié Source aurait relancé la mauvaise
        // piste.
        //
        // Les trois cas qui déplacent l'index, et un qui ne doit pas y toucher.
        let cas = [
            // (index avant, from, to, index attendu, ce qu'on éprouve)
            (2usize, 2usize, 0usize, 0usize, "on deplace la piste ecoutee"),
            (2, 0, 3, 1, "un deplacement l'enjambe vers l'aval"),
            (1, 3, 0, 2, "un deplacement l'enjambe vers l'amont"),
            (0, 2, 3, 0, "un deplacement qui ne la concerne pas"),
        ];
        for (avant, from, to, attendu, quoi) in cas {
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
                liste.index = avant;
            }
            let mut admin = admin;
            admin
                .set_data(serde_json::json!({"op": "move", "from": from, "to": to}))
                .await
                .unwrap();
            assert_eq!(admin.playlist.read().await.index, attendu, "{quoi}");
        }
    }

    #[tokio::test]
    async fn reordonner_ne_perd_jamais_la_piste_ecoutee() {
        // Garde-fou du test précédent, exprimé sur ce qui compte vraiment : quel
        // que soit le déplacement, l'index doit désigner **le même fichier**.
        for from in 0..4usize {
            for to in 0..4usize {
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
                admin
                    .set_data(serde_json::json!({"op": "move", "from": from, "to": to}))
                    .await
                    .unwrap();
                let liste = admin.playlist.read().await;
                assert_eq!(
                    liste.entries[liste.index].path,
                    PathBuf::from("/m/3.mp3"),
                    "deplacement {from} -> {to} a perdu la piste ecoutee"
                );
            }
        }
    }

    /// Un admin avec une racine locale déclarée sur `media`, et son chemin.
    async fn admin_avec_racine_locale() -> (FilesAdmin, PathBuf) {
        let (admin, racine) = admin_de_test();
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
        (admin, racine)
    }

    #[tokio::test]
    async fn un_m3u_parcouru_se_charge_et_remplace_la_liste() {
        // La demande : pouvoir charger un m3u **trouvé sur la source**, par son
        // chemin, et non une liste enregistrée cherchée par nom dans un magasin.
        let (mut admin, racine) = admin_avec_racine_locale().await;
        let media = racine.join("media");
        std::fs::create_dir_all(media.join("Album")).unwrap();
        std::fs::write(media.join("Album/01.mp3"), b"").unwrap();
        std::fs::write(media.join("Album/02.mp3"), b"").unwrap();
        // Chemins **relatifs au m3u**, comme le veut le format.
        std::fs::write(media.join("Album/tout.m3u"), "01.mp3\n02.mp3\n").unwrap();

        admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "Album/tout.m3u"
            }))
            .await
            .unwrap();
        let liste = admin.playlist.read().await;
        assert_eq!(liste.entries.len(), 2);
        assert_eq!(liste.entries[0].path, media.join("Album/01.mp3"));
        assert_eq!(liste.index, 0, "on repart du debut de la liste chargee");
    }

    #[tokio::test]
    async fn un_m3u_signale_ce_qu_il_n_a_pas_su_retrouver() {
        // Rapportées, jamais supprimées en silence : une liste plus courte que
        // son fichier est un défaut qu'on met des mois à attribuer.
        let (mut admin, racine) = admin_avec_racine_locale().await;
        let media = racine.join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("present.mp3"), b"").unwrap();
        std::fs::write(media.join("liste.m3u"), "present.mp3\nZ:\\ailleurs\\absent.mp3\n").unwrap();

        admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "liste.m3u"
            }))
            .await
            .unwrap();
        assert_eq!(admin.playlist.read().await.entries.len(), 1);
        assert_eq!(admin.unresolved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn charger_autre_chose_qu_un_m3u_est_refuse() {
        // Sans cette garde, on remplacerait la liste par le contenu interprété
        // d'un fichier quelconque — un binaire audio lu comme du texte.
        let (mut admin, racine) = admin_avec_racine_locale().await;
        let media = racine.join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("piste.mp3"), b"").unwrap();
        let err = admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "piste.mp3"
            }))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "cle brute renvoyee a l'ecran : {err}");
        assert!(err.contains("piste.mp3"), "le refus doit nommer le fautif : {err}");
    }

    #[tokio::test]
    async fn charger_un_m3u_hors_de_la_racine_est_refuse() {
        // La garde d'évasion s'applique comme pour tout chemin venu du
        // navigateur : `load_m3u` ne doit pas devenir une lecture de fichier
        // arbitraire.
        let (mut admin, racine) = admin_avec_racine_locale().await;
        std::fs::create_dir_all(racine.join("media")).unwrap();
        std::fs::write(racine.join("dehors.m3u"), "x\n").unwrap();
        let err = admin
            .set_data(serde_json::json!({
                "op": "load_m3u", "root": "local", "path": "../dehors.m3u"
            }))
            .await
            .unwrap_err();
        assert!(err.contains(' '), "cle brute : {err}");
        assert!(admin.playlist.read().await.entries.is_empty());
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
