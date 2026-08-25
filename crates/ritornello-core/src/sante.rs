//! Disjoncteur de l'extraction de pochette embarquée : borne l'appel
//! `lofty`, qui peut ne jamais revenir sur un partage réseau muet, et
//! retient les points de montage qui ne répondent plus.
//!
//! # Pourquoi cette borne existe
//!
//! `player::mpv::pochette_embarquee` ouvre et parcourt le fichier **en cours
//! de lecture** avec `lofty`, un appel strictement bloquant. Ce fichier peut
//! venir d'un partage réseau, et ce projet a déjà vécu l'incident que cela
//! cause sans borne : un montage cifs endormi a fait disparaître une page
//! d'admin entière, une IO qui n'aboutit pas retenant la boucle qui aurait dû
//! répondre à tout le reste (voir la mémoire du projet, et
//! `ritornello-plugin-files::sante`, qui a résolu le même problème pour le
//! sondage des durées). Ici, l'appelant est directement la boucle
//! d'événements du cœur : sans cette borne, un partage muet figerait mpv,
//! les commandes et l'HTTP en même temps, pas seulement une page d'admin.
//!
//! # Pourquoi pas `ritornello-plugin-files::sante` directement
//!
//! Ce module reprend la **forme** de ce disjoncteur (délai + `spawn_blocking`
//! + marque par point de montage) sans en dépendre : le cœur ne doit pas se
//! lier au greffon `files` pour un mécanisme qui lui est propre, et il n'a
//! besoin ni de `volumes::parcourable` (la liste noire des pseudo-systèmes de
//! fichiers), ni de `grouper`/`manquants` (pensés pour sonder des milliers de
//! chemins d'un coup) — le cœur ne traite jamais qu'un seul fichier à la
//! fois, celui que mpv vient d'ouvrir.
//!
//! # Pourquoi un fil abandonné, et pourquoi un seul par montage
//!
//! Un appel système en sommeil non interruptible ne se tue pas — même
//! `SIGKILL` ne le réveille pas. Le délai écoulé, le fil de `spawn_blocking`
//! est donc **perdu** jusqu'à ce que le noyau rende la main. C'est pourquoi le
//! point de montage est marqué : les appels suivants rendent la main aussitôt,
//! sans en consommer un second — sans quoi changer de piste plusieurs fois de
//! suite sur un même partage muet perdrait un fil du pool à chaque fois, sans
//! jamais en récupérer un.
//!
//! Ce fil abandonné est aussi le **seul détecteur de reprise** : quand le
//! noyau le libère enfin, il efface la marque.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Délai accordé à l'extraction d'une pochette embarquée.
///
/// Sous les cinq secondes que `MpvIpc::command` tolère déjà pour une réponse
/// de mpv : une extraction bloquée ne doit pas devenir, à elle seule, la plus
/// longue attente que la boucle du cœur puisse subir.
pub const DELAI: Duration = Duration::from_secs(3);

/// Suit la réactivité des points de montage traversés par le fichier en
/// cours de lecture.
pub struct Sante {
    /// Points de montage dont un appel n'est jamais revenu.
    injoignables: Arc<Mutex<HashSet<PathBuf>>>,
    delai: Duration,
    /// Fournisseur de `/proc/mounts`, injectable pour les tests.
    mounts: Box<dyn Fn() -> String + Send + Sync>,
}

impl Default for Sante {
    fn default() -> Self {
        Self::new()
    }
}

impl Sante {
    pub fn new() -> Self {
        Self {
            injoignables: Arc::new(Mutex::new(HashSet::new())),
            delai: DELAI,
            mounts: Box::new(|| std::fs::read_to_string("/proc/mounts").unwrap_or_default()),
        }
    }

    /// Variante de test : délai court et `/proc/mounts` figé.
    #[cfg(test)]
    pub fn pour_test(delai: Duration, mounts: String) -> Self {
        Self { injoignables: Arc::new(Mutex::new(HashSet::new())), delai, mounts: Box::new(move || mounts.clone()) }
    }

    /// Point de montage propriétaire de `chemin` : le plus long préfixe de
    /// `mounts` qui le précède. Retombe sur `chemin` lui-même si aucun ne
    /// correspond (pas de privilège pour lire `/proc/mounts`, environnement
    /// de test) — faute de mieux à quoi rattacher une éventuelle panne.
    ///
    /// Contrairement à `ritornello-plugin-files::volumes::proprietaire`, dont
    /// c'est la version complète, ce module n'a pas besoin d'écarter les
    /// pseudo-systèmes de fichiers (`proc`, `tmpfs`...) : il ne sert qu'à
    /// grouper les échecs par montage, jamais à décider si un chemin est
    /// parcourable.
    fn proprietaire(mounts: &str, chemin: &Path) -> PathBuf {
        mounts
            .lines()
            .filter_map(|l| {
                let mut c = l.split_whitespace();
                let _source = c.next()?;
                let point = c.next()?;
                Some(PathBuf::from(point.replace("\\040", " ").replace("\\011", "\t")))
            })
            .filter(|p| chemin.starts_with(p))
            .max_by_key(|p| p.as_os_str().len())
            .unwrap_or_else(|| chemin.to_path_buf())
    }

    /// Exécute `f` hors du fil asynchrone, sous délai, au compte du point de
    /// montage propriétaire de `chemin`.
    ///
    /// Rend `None` sans **rien exécuter** si ce point de montage est déjà
    /// connu muet, `None` aussi si le délai s'écoule ou si `f` panique. Un
    /// `None` ne dit donc jamais « pas de pochette » à lui seul : il dit
    /// « on ne sait pas », que l'appelant traite de toute façon comme
    /// « rien à montrer », exactement comme l'absence d'image dans les tags.
    pub async fn borne<T, F>(&self, chemin: &Path, f: F) -> Option<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let cle = Self::proprietaire(&(self.mounts)(), chemin);
        if self.injoignables.lock().unwrap().contains(&cle) {
            return None;
        }
        // `spawn_blocking` et non le fil courant : même borné, l'appel doit
        // sortir du fil asynchrone, sinon il retient tout le reste de la
        // boucle du cœur pendant tout le délai.
        let mut tache = tokio::task::spawn_blocking(f);
        // `&mut tache` : le `JoinHandle` reste à nous après l'expiration, ce
        // qui permet de confier le fil abandonné à la tâche de surveillance
        // ci-dessous.
        match tokio::time::timeout(self.delai, &mut tache).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::warn!("embedded cover extraction on {} failed: {e}", chemin.display());
                None
            }
            Err(_) => {
                tracing::warn!(
                    "{} did not answer within {:?}: treating its mount point {} as unresponsive",
                    chemin.display(),
                    self.delai,
                    cle.display()
                );
                self.injoignables.lock().unwrap().insert(cle.clone());
                let injoignables = Arc::clone(&self.injoignables);
                tokio::spawn(async move {
                    // Attend le fil perdu. Cette tâche peut ne jamais finir ;
                    // elle ne coûte qu'une tâche, là où re-tenter coûterait un
                    // fil du pool à chaque nouvelle piste sur le même partage.
                    let _ = tache.await;
                    tracing::info!("{} answers again", cle.display());
                    injoignables.lock().unwrap().remove(&cle);
                });
                None
            }
        }
    }

    /// Points de montage actuellement muets. Réservé aux tests : rien
    /// n'affiche encore cette information ailleurs (contrairement au
    /// greffon `files`, qui la montre sur sa page).
    #[cfg(test)]
    pub fn muets(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.injoignables.lock().unwrap().iter().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "/dev/root / ext4 rw 0 0\n\
                          //192.168.1.15/musique /mnt/ritornello/nas cifs ro,soft 0 0\n";

    fn sante() -> Sante {
        Sante::pour_test(Duration::from_millis(50), MOUNTS.to_string())
    }

    #[tokio::test]
    async fn un_appel_qui_repond_rend_sa_valeur() {
        let s = sante();
        assert_eq!(s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || 7).await, Some(7));
        assert!(s.muets().is_empty(), "un appel rendu ne doit marquer personne");
    }

    #[tokio::test]
    async fn un_appel_qui_ne_revient_pas_rend_la_main_et_marque_son_montage() {
        let s = sante();
        let debut = std::time::Instant::now();
        let r = s
            .borne(Path::new("/mnt/ritornello/nas/a.mp3"), || {
                std::thread::sleep(Duration::from_millis(400));
                7
            })
            .await;
        assert_eq!(r, None);
        // La borne vaut son prix seulement si elle rend la main *avant* la fin
        // de l'appel : sans la mesure, un `None` pourrait aussi bien venir
        // d'un appel qui a simplement échoué au bout de ses 400 ms.
        assert!(debut.elapsed() < Duration::from_millis(300), "{:?}", debut.elapsed());
        assert_eq!(s.muets(), vec![PathBuf::from("/mnt/ritornello/nas")]);
    }

    #[tokio::test]
    async fn un_montage_marque_ne_consomme_plus_de_fil() {
        let s = sante();
        s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;

        // C'est *l'exécution* qu'on interdit, pas seulement le résultat :
        // chaque appel qui s'exécuterait perdrait un fil du pool de plus, et
        // le pool est fini. Le drapeau prouve que la fermeture n'a pas tourné.
        static TOURNE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        TOURNE.store(false, std::sync::atomic::Ordering::SeqCst);
        let r = s
            .borne(Path::new("/mnt/ritornello/nas/autre/b.mp3"), || {
                TOURNE.store(true, std::sync::atomic::Ordering::SeqCst)
            })
            .await;
        assert_eq!(r, None);
        assert!(!TOURNE.load(std::sync::atomic::Ordering::SeqCst), "le second appel n'aurait pas dû s'exécuter");
    }

    #[tokio::test]
    async fn un_montage_marque_n_ouvre_pas_les_autres() {
        let s = sante();
        s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;
        // `/` est un autre montage : le NAS endormi ne doit pas rendre les
        // pistes locales illisibles, ce qui serait guérir en amputant.
        assert_eq!(s.borne(Path::new("/home/pi/musique/a.mp3"), || 7).await, Some(7));
    }

    #[tokio::test]
    async fn la_marque_s_efface_quand_le_montage_repond_a_nouveau() {
        let s = sante();
        s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(150));
        })
        .await;
        assert!(!s.muets().is_empty(), "le montage doit d'abord être marqué");

        // Le fil abandonné finit par revenir ; c'est lui, et lui seul, qui
        // rouvre le disjoncteur.
        for _ in 0..100 {
            if s.muets().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(s.muets().is_empty(), "la marque devait s'effacer d'elle-même");
        assert_eq!(s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || 7).await, Some(7));
    }

    #[tokio::test]
    async fn sans_montage_connu_le_chemin_lui_meme_fait_cle() {
        let s = Sante::pour_test(Duration::from_millis(50), String::new());
        s.borne(Path::new("/home/pi/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(150));
        })
        .await;
        assert_eq!(s.muets(), vec![PathBuf::from("/home/pi/a.mp3")]);
    }
}
