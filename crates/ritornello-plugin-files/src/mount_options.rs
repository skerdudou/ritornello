//! Construction de la ligne de montage.
//!
//! Isolée dans son propre module pour être testable **sans privilège** : c'est
//! le code qui décide de ce que root exécutera, et il mérite d'être éprouvé
//! sans qu'aucun montage n'ait lieu.

use crate::roots::Root;
use std::path::Path;

/// Construit la commande de montage d'un partage.
///
/// Les options sont une **liste fermée**. Aucun passe-plat vers `mount -o` : une
/// option venue de la configuration serait une option choisie par quiconque
/// atteint l'IHM web, et exécutée par root.
///
/// `soft` parce qu'un NAS endormi doit rendre une erreur d'entrée-sortie plutôt
/// que bloquer indéfiniment le processus qui lit. Le risque de corruption qui
/// déconseille `soft` en écriture ne s'applique pas à un montage `ro` ; il est
/// assumé sur une racine déclarée inscriptible, qui ne sert qu'à déposer un m3u.
///
/// Aucun `vers=` : la négociation du noyau vaut mieux qu'une version figée qui
/// vieillirait mal face à un NAS mis à jour.
pub fn mount_command(root: &Root, creds_dir: &Path, uid: u32, gid: u32) -> Vec<String> {
    let mut options = Vec::new();
    if !root.writable {
        options.push("ro".to_string());
    }
    options.push("soft".to_string());
    options.push("iocharset=utf8".to_string());
    options.push(format!("uid={uid}"));
    options.push(format!("gid={gid}"));
    options.push(format!("credentials={}", root.credentials_path(creds_dir).display()));
    vec![
        "mount".to_string(),
        "-t".to_string(),
        "cifs".to_string(),
        format!("//{}/{}", root.host, root.share),
        // Le point de montage vient de `mount_point()`, donc d'une constante et
        // du nom validé — jamais de la configuration.
        root.mount_point().display().to_string(),
        "-o".to_string(),
        options.join(","),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roots::RootKind;

    /// Fabrique **locale** à ce module : les utilitaires d'un module
    /// `#[cfg(test)]` ne traversent pas les modules.
    fn racine_smb() -> Root {
        Root {
            name: "nas".into(),
            kind: RootKind::Smb,
            path: None,
            host: "192.168.1.20".into(),
            share: "musique".into(),
            subpath: Some("Albums".into()),
            user: "steven".into(),
            domain: String::new(),
            writable: false,
        }
    }

    #[test]
    fn la_ligne_de_montage_impose_le_point_et_les_options() {
        let cmd =
            mount_command(&racine_smb(), Path::new("/etc/ritornello/media-credentials"), 998, 998);
        assert_eq!(cmd[0], "mount");
        assert_eq!(cmd[1], "-t");
        assert_eq!(cmd[2], "cifs");
        assert_eq!(cmd[3], "//192.168.1.20/musique");
        // Le sous-chemin n'entre PAS dans le point de montage : c'est le
        // partage entier qui est monté, le sous-chemin n'étant qu'un endroit où
        // regarder dedans.
        assert_eq!(cmd[4], "/mnt/ritornello/nas");
        assert_eq!(cmd[5], "-o");
        let options: Vec<&str> = cmd[6].split(',').collect();
        assert!(options.contains(&"ro"), "{options:?}");
        assert!(options.contains(&"soft"), "{options:?}");
        assert!(options.contains(&"iocharset=utf8"), "{options:?}");
        assert!(options.contains(&"uid=998"), "{options:?}");
        assert!(options.contains(&"gid=998"), "{options:?}");
        assert!(
            options.contains(&"credentials=/etc/ritornello/media-credentials/nas.cred"),
            "{options:?}"
        );
        // Aucune version figée : la négociation du noyau vaut mieux.
        assert!(!options.iter().any(|o| o.starts_with("vers=")), "{options:?}");
    }

    #[test]
    fn une_racine_inscriptible_perd_le_ro_et_rien_d_autre() {
        let cmd = mount_command(&Root { writable: true, ..racine_smb() }, Path::new("/c"), 1, 1);
        let options: Vec<&str> = cmd[6].split(',').collect();
        assert!(!options.contains(&"ro"), "{options:?}");
        assert!(
            options.contains(&"soft"),
            "soft doit rester : un NAS endormi ne doit pas bloquer la lecture"
        );
    }

    #[test]
    fn la_ligne_ne_contient_aucun_argument_vide() {
        // Un argument vide décalerait tout le reste de la ligne exécutée par
        // root, avec un effet difficile à prévoir.
        let cmd = mount_command(&racine_smb(), Path::new("/c"), 1, 1);
        assert!(cmd.iter().all(|a| !a.is_empty()), "{cmd:?}");
    }
}
