//! Dialogue avec systemd pour monter et démonter les partages déclarés.
//!
//! Le plugin ne monte rien lui-même : le service tourne en
//! `NoNewPrivileges=true`, `sudo` et tout chemin setuid sont donc
//! structurellement hors d'atteinte. Il demande à systemd de lancer une unité
//! fixe, `ritornello-media-mount.service`, qu'une règle polkit l'autorise à
//! démarrer — et elle seule (`deploy/51-ritornello-media.rules`).
//!
//! Ce module ne sait que deux choses : dire si une racine est montée (lecture
//! de `/proc/mounts`), et demander la réconciliation. Ce qui est réellement
//! monté, et avec quelles options, se décide du côté privilégié.

use crate::roots::{Root, RootKind, MOUNT_ROOT};
use std::path::{Path, PathBuf};

/// L'unité que le plugin démarre. Fixe : c'est aussi le nom que la règle
/// polkit compare, une unité paramétrable serait une autorisation ouverte.
pub const UNIT: &str = "ritornello-media-mount.service";

/// Table des montages du noyau. Champ constant plutôt que littéral disséminé :
/// l'analyse, elle, est une fonction pure et se teste sans ce fichier.
const PROC_MOUNTS: &str = "/proc/mounts";

/// Ce que le plugin sait de la disponibilité d'une racine.
///
/// Volontairement binaire : rien ici ne distingue « pas encore monté » de
/// « montage en échec », parce que le plugin ne peut pas le savoir sans
/// interroger systemd, et que la conduite à tenir est la même — réconcilier,
/// puis rapporter ce que `systemctl` a répondu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountState {
    Mounted,
    NotMounted,
}

/// Point de montage d'une racine, **imposé** : `/mnt/ritornello/<name>`.
///
/// Ce n'est pas `Root::base_dir()`, qui y ajoute le sous-chemin déclaré. Le
/// sous-chemin est parcouru *sous* le point monté et n'apparaît jamais dans
/// `/proc/mounts` : le confondre avec le point de montage ferait passer une
/// racine à sous-chemin pour éternellement non montée.
fn point_de_montage(root: &Root) -> PathBuf {
    PathBuf::from(MOUNT_ROOT).join(&root.name)
}

/// Vrai si `point` figure comme point de montage dans le contenu de
/// `/proc/mounts`.
///
/// Pure — elle prend le texte plutôt que de le lire — pour être testable sans
/// rien monter, ce qu'un test ne peut de toute façon pas faire sans privilège.
///
/// La deuxième colonne échappe l'espace en `\040` (et la tabulation en
/// `\011`) : sans ce traitement, un partage monté sous un nom contenant un
/// espace passerait pour non monté, et le plugin le remonterait en boucle.
pub fn est_monte_dans(proc_mounts: &str, point: &Path) -> bool {
    points_de_montage(proc_mounts).any(|p| p == point)
}

/// Tous les points de montage déclarés, déséchappés.
///
/// Existe pour que la règle de déséchappement n'ait **qu'une seule
/// implémentation** : le binaire racine de montage doit lui aussi énumérer ce
/// qui est monté, pour démonter ce qui n'est plus déclaré. Deux copies de cette
/// règle, c'était une divergence en puissance — l'une traitant `\011` et pas
/// l'autre, par exemple, avec un défaut visible seulement sur un nom rare.
pub fn points_de_montage(proc_mounts: &str) -> impl Iterator<Item = PathBuf> + '_ {
    proc_mounts.lines().filter_map(|ligne| {
        ligne
            .split_whitespace()
            .nth(1)
            .map(|p| PathBuf::from(p.replace("\\040", " ").replace("\\011", "\t")))
    })
}

/// État de montage d'une racine, tel que le noyau le rapporte.
///
/// Une racine locale rend toujours `Mounted` : il n'y a rien à monter, et
/// rendre `NotMounted` lancerait une réconciliation sans fin pour un
/// répertoire que le binaire de montage ignore de toute façon.
///
/// `/proc/mounts` illisible rend `NotMounted` : ne pas savoir, c'est ne pas
/// pouvoir promettre que le partage est là.
pub fn state(root: &Root) -> MountState {
    if root.kind == RootKind::Local {
        return MountState::Mounted;
    }
    let Ok(contenu) = std::fs::read_to_string(PROC_MOUNTS) else {
        return MountState::NotMounted;
    };
    if est_monte_dans(&contenu, &point_de_montage(root)) {
        MountState::Mounted
    } else {
        MountState::NotMounted
    }
}

/// Met en forme l'échec d'un `systemctl`, sortie d'erreur **verbatim**.
///
/// Verbatim parce qu'un refus polkit y est explicite et actionnable
/// (« Interactive authentication required », qui désigne la règle manquante),
/// là où une phrase maison la rendrait opaque. Le repli sur le code de sortie
/// ne sert qu'au cas où `systemctl` échoue sans rien écrire : une erreur vide
/// serait affichée comme un succès silencieux.
fn echec(status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let err = String::from_utf8_lossy(stderr).trim().to_string();
    if err.is_empty() {
        format!("systemctl failed ({status})")
    } else {
        err
    }
}

/// Demande à systemd de réconcilier les montages.
///
/// `systemctl` en processus fils, et non une crate D-Bus : c'est ainsi que
/// l'onglet Système parle à systemd et à logind (`crates/ritornello-core/src/
/// system.rs`), et cela évite de tirer une dépendance entière pour un appel.
///
/// **Aucune sonde de capacité préalable.** L'onglet Système peut demander à
/// logind s'il a le droit (`CanPowerOff`), systemd n'offre pas d'équivalent
/// pour `manage-units` — il n'existe pas de « CanStartUnit ». On tente donc, et
/// l'erreur porte la sortie de `systemctl` telle quelle, jusqu'à la page.
pub async fn reconcile(unit: &str) -> Result<(), String> {
    tracing::info!("asking systemd to start {unit}");
    let sortie = tokio::process::Command::new("systemctl")
        .arg("start")
        .arg(unit)
        .output()
        .await
        .map_err(|e| format!("systemctl unavailable: {e}"))?;
    if sortie.status.success() {
        return Ok(());
    }
    Err(echec(sortie.status, &sortie.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deux lignes réelles : un partage cifs monté par le binaire racine, et un
    /// montage étranger qui doit rester sans effet sur la réponse.
    const PROC_MOUNTS_EXEMPLE: &str =
        "//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0\n\
         /dev/sda1 /media/usb ext4 rw 0 0\n";

    fn racine_smb() -> Root {
        Root {
            name: "nas".into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: "musique".into(),
            subpath: None,
            user: "steven".into(),
            domain: String::new(),
            writable: false,
        }
    }

    #[test]
    fn un_point_de_montage_absent_de_proc_mounts_est_non_monte() {
        // L'analyse de /proc/mounts est pure : le test n'a besoin de monter
        // quoi que ce soit, ce qu'il ne pourrait pas faire sans privilège.
        assert!(est_monte_dans(PROC_MOUNTS_EXEMPLE, Path::new("/mnt/ritornello/nas")));
        assert!(!est_monte_dans(PROC_MOUNTS_EXEMPLE, Path::new("/mnt/ritornello/autre")));
    }

    #[test]
    fn un_point_de_montage_avec_espace_echappe_est_reconnu() {
        // /proc/mounts échappe l'espace en \040. Sans ce traitement, un partage
        // « ma musique » passerait pour non monté, et le plugin le remonterait
        // à chaque coup d'œil — une boucle de montage silencieuse.
        let contenu = "//nas/x /mnt/ritornello/ma\\040musique cifs ro 0 0\n";
        assert!(est_monte_dans(contenu, Path::new("/mnt/ritornello/ma musique")));
    }

    #[test]
    fn une_tabulation_echappee_est_reconnue_aussi() {
        // Même mécanisme, autre échappement : \011 est la tabulation. Le
        // traiter à moitié laisserait le même défaut sur un nom plus rare.
        let contenu = "//nas/x /mnt/ritornello/ma\\011musique cifs ro 0 0\n";
        assert!(est_monte_dans(contenu, Path::new("/mnt/ritornello/ma\tmusique")));
    }

    #[test]
    fn le_peripherique_source_ne_se_confond_pas_avec_le_point_de_montage() {
        // La première colonne est la source, la deuxième le point de montage.
        // Chercher n'importe quelle colonne ferait passer pour montée une
        // racine dont seul le nom apparaît ailleurs dans la ligne.
        let contenu = "/mnt/ritornello/nas /mnt/autre none bind 0 0\n";
        assert!(!est_monte_dans(contenu, Path::new("/mnt/ritornello/nas")));
    }

    #[test]
    fn une_racine_locale_na_rien_a_monter() {
        // Sans ce cas, un dossier de l'appareil serait déclaré non monté et le
        // plugin réclamerait une réconciliation que le binaire racine ignore :
        // une demande de privilège perpétuelle pour rien.
        let r = Root {
            name: "usb".into(),
            kind: RootKind::Local,
            path: Some("/media/usb".into()),
            ..racine_smb()
        };
        assert_eq!(state(&r), MountState::Mounted);
    }

    #[test]
    fn le_sous_chemin_nentre_pas_dans_le_point_de_montage() {
        // `base_dir()` ajoute le sous-chemin parcouru ; /proc/mounts ne connaît
        // que le point monté. Les confondre ferait passer toute racine à
        // sous-chemin pour non montée, donc remontée sans fin.
        let r = Root { subpath: Some("Albums".into()), ..racine_smb() };
        assert_eq!(point_de_montage(&r), PathBuf::from("/mnt/ritornello/nas"));
        assert!(est_monte_dans(PROC_MOUNTS_EXEMPLE, &point_de_montage(&r)));
    }

    #[test]
    fn un_echec_de_systemctl_rapporte_sa_sortie_mot_pour_mot() {
        // La raison d'être du choix « pas de message maison » : c'est la phrase
        // de polkit qui dit quoi faire. La reformuler perdrait l'information.
        let refus = b"Failed to start ritornello-media-mount.service: Interactive authentication required.";
        let sortie = std::process::Command::new("false").output().unwrap();
        assert_eq!(
            echec(sortie.status, refus),
            "Failed to start ritornello-media-mount.service: Interactive authentication required."
        );
    }

    #[test]
    fn un_echec_muet_reste_un_message_non_vide() {
        // Un `systemctl` qui échoue sans rien écrire donnerait sinon une erreur
        // vide, que la page afficherait comme un succès silencieux.
        let sortie = std::process::Command::new("false").output().unwrap();
        let m = echec(sortie.status, b"   \n");
        assert!(m.contains("systemctl failed"), "{m:?}");
    }
}
