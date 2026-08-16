//! L'état des deux assistants de déclaration de source.
//!
//! Extrait de `admin.rs`, qui atteignait 800 lignes : les opérations
//! d'assistant y auraient formé un deuxième sujet sans rapport avec la gestion
//! de la liste de lecture.
//!
//! Le protocole admin étant requête/réponse et ne poussant rien, une connexion
//! réseau ne peut pas être attendue dans la requête : un NAS éteint dépasserait
//! le plafond de 5 s du cœur et la requête serait tuée avant d'avoir rien
//! rapporté. `connecter` et `parcourir` lancent donc une tâche et rendent la
//! main aussitôt ; la page suit l'avancement par sondage, exactement comme pour
//! le balayage.

use crate::smb::{self, Credentials};
use crate::{scan, volumes};
use ritornello_i18n::Catalog;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Plafond d'un appel `smbclient`. Large — un NAS qui se réveille prend son
/// temps — mais fini : la page doit toujours finir par apprendre quelque chose.
const DELAI_SMB: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Local,
    Smb,
}

/// Ce que la page lit de l'assistant en cours.
///
/// **Ne contient aucun identifiant.** La garantie est portée par le type, comme
/// pour `Root` : la structure sérialisée n'a pas de champ mot de passe, il n'y
/// a donc rien à filtrer et rien à oublier de filtrer.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Vue {
    pub open: bool,
    pub kind: Option<String>,
    pub host: String,
    pub share: String,
    pub path: String,
    pub shares: Vec<String>,
    pub dirs: Vec<String>,
    pub audio_count: usize,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct Explorateur {
    creds_dir: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    smb_ok: Arc<AtomicBool>,
    vue: Arc<Mutex<Vue>>,
    /// Identifiants de la popin en cours, indexés par hôte.
    ///
    /// En mémoire et **jamais sérialisés** : le mot de passe traverse le fil
    /// une fois, à la connexion, et non à chaque clic dans l'arborescence.
    sessions: Arc<Mutex<HashMap<String, Credentials>>>,
    tache: Option<tokio::task::JoinHandle<()>>,
}

impl Explorateur {
    pub fn new(
        creds_dir: PathBuf,
        catalog: Arc<RwLock<Catalog>>,
        smb_ok: Arc<AtomicBool>,
    ) -> Self {
        Self {
            creds_dir,
            catalog,
            smb_ok,
            vue: Arc::new(Mutex::new(Vue::default())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            tache: None,
        }
    }

    fn mot(&self, cle: &str) -> String {
        self.catalog.read().unwrap().get(cle).to_string()
    }

    pub fn ouvrir(&mut self, kind: Kind) {
        self.annuler();
        *self.vue.lock().unwrap() = Vue {
            open: true,
            kind: Some(match kind {
                Kind::Local => "local".to_string(),
                Kind::Smb => "smb".to_string(),
            }),
            ..Vue::default()
        };
    }

    pub fn fermer(&mut self) {
        self.annuler();
        // Les identifiants meurent avec la popin : les laisser en mémoire
        // ferait survivre un mot de passe à ce qui l'a recueilli, sans que rien
        // ne le reprenne jamais.
        self.sessions.lock().unwrap().clear();
        *self.vue.lock().unwrap() = Vue::default();
    }

    fn annuler(&mut self) {
        if let Some(t) = self.tache.take() {
            t.abort();
        }
    }

    pub fn credentials(&self, host: &str) -> Option<Credentials> {
        self.sessions.lock().unwrap().get(host).map(|c| Credentials {
            user: c.user.clone(),
            password: c.password.clone(),
            domain: c.domain.clone(),
        })
    }

    /// Contenu d'un dossier de l'appareil.
    ///
    /// Synchrone : un système de fichiers local répond bien en deçà du plafond
    /// du cœur, et rendre cela asynchrone n'ajouterait qu'un aller-retour de
    /// sondage entre chaque niveau ouvert.
    pub fn local(&mut self, path: &str) -> Result<(), String> {
        let chemin = std::path::Path::new(path);
        let mounts = volumes::lire_proc_mounts();
        let canon = chemin
            .canonicalize()
            .map_err(|_| self.mot("bad_local_path").replace("{path}", path))?;
        if !volumes::parcourable(&mounts, &canon) {
            return Err(self.mot("bad_local_path").replace("{path}", path));
        }
        let (dossiers, fichiers) =
            scan::list_dir(&canon).map_err(|e| e.message(&self.catalog.read().unwrap()))?;
        let mut v = self.vue.lock().unwrap();
        v.path = canon.display().to_string();
        v.dirs = dossiers;
        v.audio_count = fichiers.len();
        v.error = None;
        v.busy = false;
        Ok(())
    }

    /// Se connecte à un hôte et énumère ses partages.
    pub fn connecter(&mut self, host: String, user: String, password: String, domain: String) {
        self.annuler();
        if !user.is_empty() {
            self.sessions
                .lock()
                .unwrap()
                .insert(host.clone(), Credentials { user, password, domain });
        }
        if !self.smb_ok.load(Ordering::Relaxed) {
            self.echec(smb::SmbError::NotInstalled, &host);
            return;
        }
        {
            let mut v = self.vue.lock().unwrap();
            v.host = host.clone();
            v.share = String::new();
            v.path = String::new();
            v.shares.clear();
            v.dirs.clear();
            v.busy = true;
            v.error = None;
        }
        let creds = self.credentials(&host);
        let dir = self.creds_dir.clone();
        let vue = self.vue.clone();
        let catalog = self.catalog.clone();
        self.tache = Some(tokio::spawn(async move {
            let r = smb::list_shares(&host, creds.as_ref(), &dir, DELAI_SMB).await;
            let mut v = vue.lock().unwrap();
            v.busy = false;
            match r {
                Ok(partages) => {
                    v.shares = partages;
                    v.error = None;
                }
                Err(e) => {
                    tracing::warn!("listing shares of {host}: {e}");
                    v.error = Some(e.message(&catalog.read().unwrap(), &host));
                }
            }
        }));
    }

    /// Liste un dossier d'un partage.
    pub fn parcourir(&mut self, share: String, path: String) {
        self.annuler();
        let host = self.vue.lock().unwrap().host.clone();
        if !self.smb_ok.load(Ordering::Relaxed) {
            self.echec(smb::SmbError::NotInstalled, &host);
            return;
        }
        {
            let mut v = self.vue.lock().unwrap();
            v.share = share.clone();
            v.path = path.clone();
            v.dirs.clear();
            v.audio_count = 0;
            v.busy = true;
            v.error = None;
        }
        let creds = self.credentials(&host);
        let dir = self.creds_dir.clone();
        let vue = self.vue.clone();
        let catalog = self.catalog.clone();
        self.tache = Some(tokio::spawn(async move {
            let r = smb::list_dir(&host, &share, &path, creds.as_ref(), &dir, DELAI_SMB).await;
            let mut v = vue.lock().unwrap();
            v.busy = false;
            match r {
                Ok(entrees) => {
                    v.dirs = entrees.iter().filter(|e| e.dir).map(|e| e.name.clone()).collect();
                    v.audio_count = entrees
                        .iter()
                        .filter(|e| !e.dir && scan::is_audio(std::path::Path::new(&e.name)))
                        .count();
                    v.error = None;
                }
                Err(e) => {
                    tracing::warn!("listing //{host}/{share}/{path}: {e}");
                    v.error = Some(e.message(&catalog.read().unwrap(), &host));
                }
            }
        }));
    }

    fn echec(&self, e: smb::SmbError, host: &str) {
        let mut v = self.vue.lock().unwrap();
        v.busy = false;
        v.error = Some(e.message(&self.catalog.read().unwrap(), host));
    }

    pub fn vue(&self) -> serde_json::Value {
        serde_json::to_value(&*self.vue.lock().unwrap()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn explorateur(dir: &std::path::Path) -> Explorateur {
        Explorateur::new(
            dir.join("creds"),
            Arc::new(std::sync::RwLock::new(Catalog::load(
                "files",
                "en",
                std::path::Path::new("/inexistant"),
                crate::FILES_EN,
            ))),
            Arc::new(AtomicBool::new(true)),
        )
    }

    #[tokio::test]
    async fn le_mot_de_passe_n_apparait_dans_aucune_vue() {
        // Il n'a aucune raison de retraverser vers le navigateur : la page l'a
        // envoyé une fois, elle n'a pas besoin de le relire pour afficher un
        // arbre de dossiers.
        let dir = tempfile::tempdir().unwrap();
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Smb);
        e.connecter("nas".into(), "steven".into(), "secret-du-nas".into(), String::new());
        let texte = serde_json::to_string(&e.vue()).unwrap();
        assert!(!texte.contains("secret-du-nas"), "{texte}");
        assert!(!texte.contains("password"), "{texte}");
    }

    #[tokio::test]
    async fn fermer_efface_la_session() {
        // Sinon un mot de passe survivrait en mémoire à la popin qui l'a
        // recueilli, sans que rien ne le reprenne jamais.
        let dir = tempfile::tempdir().unwrap();
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Smb);
        e.connecter("nas".into(), "steven".into(), "secret".into(), String::new());
        assert!(e.credentials("nas").is_some());
        e.fermer();
        assert!(e.credentials("nas").is_none());
    }

    #[tokio::test]
    async fn un_chemin_local_hors_volume_est_refuse() {
        // La garde de parcours. Sans elle, la page adresserait /proc/self et
        // l'arbre partirait dans les liens récursifs.
        let dir = tempfile::tempdir().unwrap();
        let faux = dir.path().join("mounts");
        std::fs::write(&faux, "proc /proc proc rw 0 0\n/dev/sda1 / ext4 rw 0 0\n").unwrap();
        std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &faux);
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Local);
        let err = e.local("/proc/self").unwrap_err();
        assert!(err.contains(' '), "cle brute : {err}");
        std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
    }

    #[tokio::test]
    async fn un_dossier_local_rend_ses_sous_dossiers_et_son_compte_audio() {
        // Le compte de fichiers audio est ce qui dit qu'on est au bon endroit :
        // sans lui on choisit un dossier en espérant.
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir_all(media.join("Album")).unwrap();
        std::fs::write(media.join("a.mp3"), b"").unwrap();
        std::fs::write(media.join("b.flac"), b"").unwrap();
        std::fs::write(media.join("notes.txt"), b"").unwrap();
        let faux = dir.path().join("mounts");
        std::fs::write(&faux, format!("/dev/sda1 {} ext4 rw 0 0\n", dir.path().display())).unwrap();
        std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &faux);
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Local);
        e.local(&media.display().to_string()).unwrap();
        let v = e.vue();
        assert_eq!(v["dirs"], serde_json::json!(["Album"]));
        assert_eq!(v["audio_count"], 2, "notes.txt n'est pas un fichier audio");
        std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
    }
}
