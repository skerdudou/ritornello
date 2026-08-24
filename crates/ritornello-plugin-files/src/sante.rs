//! Disjoncteur des chemins média : borne tout appel système qui peut ne pas
//! revenir, et retient les points de montage qui ne répondent plus.
//!
//! # Pourquoi cette borne existe
//!
//! La moitié Admin sert ses requêtes **en série** et le cœur les abandonne au
//! bout de cinq secondes. Un seul appel système qui n'aboutit pas y coince donc
//! le plugin entier, page comprise. Mesuré le 2026-08-17 sur l'appareil : un
//! montage cifs bloqué a fait expirer jusqu'à `ui.js`, qui n'est pourtant qu'un
//! `include_str!` sans verrou ni entrée-sortie — la boucle était déjà retenue
//! par la requête d'avant.
//!
//! # Pourquoi ce n'est pas réglable au montage
//!
//! `mount.cifs` reçoit déjà `soft` (voir `mount_options`, où un test l'épingle).
//! `soft` borne les tentatives d'une opération sur une session **établie**, pas
//! la reconnexion, qui peut durer des minutes. Aucun réglage cifs ne ramène le
//! pire cas sous les cinq secondes du cœur : la borne doit vivre côté appelant.
//!
//! # Pourquoi un fil abandonné, et pourquoi un seul
//!
//! Un appel système en sommeil non interruptible ne se tue pas — même
//! `SIGKILL` ne le réveille pas. Le délai écoulé, le fil de `spawn_blocking`
//! est donc **perdu** jusqu'à ce que le noyau rende la main. C'est pourquoi le
//! point de montage est marqué : les appels suivants rendent la main aussitôt,
//! sans en consommer un second. Au plus un fil abandonné par point de montage.
//!
//! Ce fil abandonné est aussi le **seul détecteur de reprise** : quand le noyau
//! le libère enfin, il efface la marque. Sonder à nouveau pour savoir si le
//! montage répond coûterait un fil de plus à chaque essai.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::volumes;

/// Délai accordé à un appel système sur un chemin média.
///
/// Bien en dessous des cinq secondes du cœur : il faut qu'un `get_data` qui
/// tombe sur un montage muet reste rendu dans le délai, avec de la marge pour
/// le reste de la réponse.
pub const DELAI: Duration = Duration::from_millis(1500);

/// Suit la réactivité des points de montage traversés par les chemins média.
pub struct Sante {
    /// Points de montage dont une sonde n'est jamais revenue.
    injoignables: Arc<Mutex<HashSet<PathBuf>>>,
    delai: Duration,
    /// Fournisseur de `/proc/mounts`, injectable pour les tests — même procédé
    /// que `volumes::lire_proc_mounts`, qu'il appelle par défaut.
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
            mounts: Box::new(volumes::lire_proc_mounts),
        }
    }

    /// Variante de test : délai court, `/proc/mounts` figé, et une liste de
    /// points de montage déjà tenus pour muets.
    ///
    /// Publique, et non derrière `#[cfg(test)]` : les tests de la moitié Admin
    /// vivent dans le binaire, qui consomme cette bibliothèque compilée **sans**
    /// `cfg(test)`. Un raccourci caché là y serait invisible, et la moitié Admin
    /// est justement celle qu'il faut pouvoir mettre face à un montage muet sans
    /// en avoir un sous la main.
    pub fn pour_test(delai: Duration, mounts: String, muets: Vec<PathBuf>) -> Self {
        Self {
            injoignables: Arc::new(Mutex::new(muets.into_iter().collect())),
            delai,
            mounts: Box::new(move || mounts.clone()),
        }
    }

    /// Point de montage propriétaire de `chemin`, la clé du disjoncteur.
    ///
    /// Le blocage est une propriété du **montage**, pas de la racine déclarée :
    /// deux racines sur le même partage tombent ensemble, et un chemin choisi
    /// dans l'assistant est couvert sans être déclaré nulle part.
    fn cle(mounts: &str, chemin: &Path) -> PathBuf {
        volumes::proprietaire(mounts, chemin)
            .map(|v| v.path)
            // Aucun montage propriétaire : le chemin lui-même fait une clé
            // honnête, faute de mieux à quoi rattacher la panne.
            .unwrap_or_else(|| chemin.to_path_buf())
    }

    /// Vrai si une sonde sur ce point de montage n'est jamais revenue.
    pub fn injoignable(&self, chemin: &Path) -> bool {
        let cle = Self::cle(&(self.mounts)(), chemin);
        self.injoignables.lock().unwrap().contains(&cle)
    }

    /// Points de montage actuellement muets, pour que la page le dise.
    pub fn muets(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.injoignables.lock().unwrap().iter().cloned().collect();
        v.sort();
        v
    }

    /// Exécute `f` hors du fil asynchrone, sous délai, au compte du point de
    /// montage propriétaire de `chemin`.
    ///
    /// Rend `None` sans **rien exécuter** si ce point de montage est déjà connu
    /// muet, `None` aussi si le délai s'écoule ou si `f` panique. Un `None` ne
    /// dit donc jamais « absent » : il dit « on ne sait pas », ce que
    /// l'appelant doit reporter tel quel plutôt que de le traduire en fait.
    pub async fn borne<T, F>(&self, chemin: &Path, f: F) -> Option<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let cle = Self::cle(&(self.mounts)(), chemin);
        if self.injoignables.lock().unwrap().contains(&cle) {
            return None;
        }
        // `spawn_blocking` et non le fil courant : même borné, l'appel doit
        // sortir du fil asynchrone, sinon il retient les autres tâches du
        // runtime pendant tout le délai.
        let mut tache = tokio::task::spawn_blocking(f);
        // `&mut tache` : le `JoinHandle` reste à nous après l'expiration, ce qui
        // permet de le confier au surveillant ci-dessous.
        match tokio::time::timeout(self.delai, &mut tache).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::warn!("probe of {} failed: {e}", chemin.display());
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
                    // elle ne coûte qu'une tâche, là où re-sonder coûterait un
                    // fil du pool à chaque tentative.
                    let _ = tache.await;
                    tracing::info!("{} answers again", cle.display());
                    injoignables.lock().unwrap().remove(&cle);
                });
                None
            }
        }
    }

    /// Groupe les indices de `chemins` par point de montage propriétaire.
    ///
    /// Grouper avant d'agir est ce qui rend la borne tenable : un seul délai
    /// couvre toutes les pistes d'un même partage. Sans ça, une liste de deux
    /// mille pistes sur un partage muet coûterait deux mille délais.
    ///
    /// Le résultat est trié par point de montage : à charge utile égale, la page
    /// doit recevoir la même chose d'un sondage à l'autre.
    pub fn grouper(&self, chemins: &[PathBuf]) -> Vec<(PathBuf, Vec<usize>)> {
        let mounts = (self.mounts)();
        let mut groupes: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for (i, c) in chemins.iter().enumerate() {
            groupes.entry(Self::cle(&mounts, c)).or_default().push(i);
        }
        let mut v: Vec<(PathBuf, Vec<usize>)> = groupes.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Dit, pour chaque chemin, s'il manque — `None` quand son point de montage
    /// ne répond pas.
    ///
    /// `None` et non `true` : afficher « introuvable » pour un partage endormi
    /// accuserait les fichiers d'une panne qui est celle du montage, et
    /// enverrait chercher le défaut au mauvais endroit.
    ///
    /// Les chemins sont groupés par point de montage : un seul délai couvre
    /// toutes les pistes d'un même partage, au lieu d'un par piste.
    pub async fn manquants(&self, chemins: &[PathBuf]) -> Vec<Option<bool>> {
        let mut out = vec![None; chemins.len()];
        for (_, indices) in self.grouper(chemins) {
            let lot: Vec<PathBuf> = indices.iter().map(|&i| chemins[i].clone()).collect();
            let repere = lot[0].clone();
            let mesure =
                self.borne(&repere, move || lot.iter().map(|p| !p.is_file()).collect::<Vec<_>>());
            if let Some(v) = mesure.await {
                for (n, &i) in indices.iter().enumerate() {
                    out[i] = v.get(n).copied();
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Deux montages distincts, pour que le disjoncteur de l'un n'ouvre pas
    /// l'autre.
    const MOUNTS: &str = "/dev/root / ext4 rw 0 0\n\
                          //192.168.1.15/musique /mnt/ritornello/nas cifs ro,soft 0 0\n";

    fn sante() -> Sante {
        Sante::pour_test(Duration::from_millis(50), MOUNTS.to_string(), Vec::new())
    }

    #[tokio::test]
    async fn un_appel_qui_repond_rend_sa_valeur() {
        let s = sante();
        assert_eq!(s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || 7).await, Some(7));
        assert!(s.muets().is_empty(), "un appel rendu ne doit marquer personne");
    }

    /// L'appel ne revient **jamais** de lui-même : il bloque sur un canal que
    /// le test ne libère qu'à la fin.
    ///
    /// La propriété gardée est la même qu'avant — la borne vaut son prix
    /// seulement si elle rend la main *avant* la fin de l'appel — mais elle
    /// était prouvée par une marge d'horloge murale : 300 ms mesurées contre un
    /// délai de 50 ms et un appel de 400 ms. Une hypothèse d'exécution rapide,
    /// donc un flake dès que les autres binaires de test chargent la machine.
    ///
    /// Un appel qui ne finit pas tant qu'on ne l'y autorise pas rend le `None`
    /// vrai **par construction** : aucune charge ne peut faire gagner la course
    /// à l'appel, là où un `sleep` de 400 ms pouvait la gagner. Le `timeout` du
    /// test ne garde plus que la régression franche — une borne qui attendrait
    /// l'appel au lieu de le borner ferait pendre ce test, et cette ligne le
    /// sanctionne avec un message plutôt qu'en expirant sans rien dire.
    ///
    /// À ne pas remplacer par `tokio::time::pause()` : mesuré, l'horloge
    /// virtuelle n'avance pas tant qu'une tâche de `spawn_blocking` est en vol,
    /// donc l'appel gagnait et l'assertion s'inversait en `Some(7)`.
    #[tokio::test]
    async fn un_appel_qui_ne_revient_pas_rend_la_main_et_marque_son_montage() {
        let s = sante();
        let (liberation, attente) = std::sync::mpsc::channel::<()>();
        let r = tokio::time::timeout(
            Duration::from_secs(10),
            s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), move || {
                let _ = attente.recv();
                7
            }),
        )
        .await
        .expect("la borne doit rendre la main a son delai, pas attendre l'appel");
        assert_eq!(r, None);
        assert_eq!(s.muets(), vec![PathBuf::from("/mnt/ritornello/nas")]);
        // Libère le fil bloquant, sinon l'arrêt du runtime l'attendrait.
        let _ = liberation.send(());
    }

    #[tokio::test]
    async fn un_montage_marque_ne_consomme_plus_de_fil() {
        let s = sante();
        s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;

        // C'est *l'exécution* qu'on interdit, pas seulement le résultat : chaque
        // appel qui s'exécuterait perdrait un fil du pool de plus, et le pool
        // est fini. Le drapeau prouve que la fermeture n'a pas tourné.
        static TOURNE: AtomicBool = AtomicBool::new(false);
        TOURNE.store(false, Ordering::SeqCst);
        let r = s
            .borne(Path::new("/mnt/ritornello/nas/autre/b.mp3"), || {
                TOURNE.store(true, Ordering::SeqCst)
            })
            .await;
        assert_eq!(r, None);
        assert!(!TOURNE.load(Ordering::SeqCst), "le second appel n'aurait pas dû s'exécuter");
    }

    #[tokio::test]
    async fn un_montage_marque_n_ouvre_pas_les_autres() {
        let s = sante();
        s.borne(Path::new("/mnt/ritornello/nas/a.mp3"), || {
            std::thread::sleep(Duration::from_millis(400));
        })
        .await;
        // `/` est un autre montage : le NAS endormi ne doit pas rendre les
        // sources locales inutilisables, ce qui serait guérir en amputant.
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
    async fn manquants_distingue_absent_de_indetermine() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present.mp3");
        std::fs::write(&present, b"x").unwrap();
        let absent = dir.path().join("absent.mp3");

        // Le montage du dossier temporaire est décrit comme `/`, celui du NAS
        // reste à part : la réponse doit être connue pour les deux premiers et
        // indéterminée pour le troisième.
        let s = Sante::pour_test(
            Duration::from_millis(50),
            MOUNTS.to_string(),
            vec![PathBuf::from("/mnt/ritornello/nas")],
        );

        let r = s
            .manquants(&[
                present.clone(),
                absent.clone(),
                PathBuf::from("/mnt/ritornello/nas/c.mp3"),
            ])
            .await;
        assert_eq!(r, vec![Some(false), Some(true), None]);
    }
}
