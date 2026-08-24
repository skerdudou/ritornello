use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Une entrée de `plugins.toml` : quoi lancer, sous quel nom. Rien d'autre.
///
/// Ni le genre ni la page d'admin n'y sont déclarés : ce sont des propriétés
/// du **binaire**, que celui-ci annonce lui-même sur le socket
/// d'enregistrement du cœur. L'opérateur n'a plus à les connaître, et leur
/// oubli ne peut plus produire de mode dégradé silencieux.
///
/// **L'ordre du fichier reste porteur** : c'est lui qui arbitre entre deux
/// greffons annonçant le genre `metadata` (voir `crate::register`).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub exec: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginConfig>,
}

impl PluginManifest {
    /// Un fichier **absent** donne un manifeste vide : le cœur démarre sans
    /// plugin plutôt que d'échouer (cohérent avec le traitement déjà
    /// existant pour `stations.toml`). Toute autre erreur d'E/S est remontée,
    /// comme un TOML invalide : un `plugins.toml` présent mais illisible
    /// (droits) qui donnerait « aucune source disponible » enverrait le
    /// diagnostic dans la mauvaise direction.
    ///
    /// Un `name` dupliqué n'est ni rejeté ni dédoublonné ici, seulement
    /// signalé (voir `noms_dupliques`) : c'était le contournement employé
    /// avant ce chantier pour faire servir deux genres à un même binaire, et
    /// un appareil en service peut encore le porter.
    pub fn load(path: &Path) -> Result<Self> {
        let manifest: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        for nom in noms_dupliques(&manifest.plugins) {
            tracing::warn!(
                "plugin name '{nom}' appears more than once in plugins.toml: a single \
                 announcement satisfies both entries, and the second connection is wired \
                 twice, left hanging in a backlog nobody accepts"
            );
        }
        Ok(manifest)
    }
}

/// Noms de `plugin.name` apparaissant plus d'une fois dans `plugins`, chacun
/// une seule fois, dans l'ordre de leur première duplication.
///
/// Fonction pure pour être testable : `PluginManifest::load` s'en sert pour
/// nommer chaque doublon dans un `tracing::warn!`, sans rien rejeter ni
/// dédoublonner — un doublon de déclaration reste silencieusement câblé deux
/// fois aujourd'hui, la seconde connexion pendant dans un backlog que
/// personne n'accepte.
fn noms_dupliques(plugins: &[PluginConfig]) -> Vec<String> {
    let mut vus = std::collections::HashSet::new();
    let mut doublons = Vec::new();
    for p in plugins {
        if !vus.insert(p.name.as_str()) && !doublons.iter().any(|d| d == &p.name) {
            doublons.push(p.name.clone());
        }
    }
    doublons
}

/// Rase et recrée `{runtime_dir}/sockets`, et rend son chemin.
///
/// Un répertoire neuf à chaque démarrage rend les fichiers rances
/// **impossibles** au lieu de reposer sur une pré-suppression au cas par cas :
/// un socket laissé par une exécution précédente est connectable, et le cœur
/// dialoguerait avec un zombie ou attendrait un `ECONNREFUSED` retenté. Une
/// seule instance du cœur par `runtime_dir` — garanti par `RuntimeDirectory=`
/// de systemd en service, par une variable distincte en développement.
pub fn prepare_sockets_dir(runtime_dir: &Path) -> Result<PathBuf> {
    let dir = runtime_dir.join("sockets");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("clearing {}", dir.display()));
        }
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Lance un greffon en lui disant où s'annoncer, sous quel nom, et avec quel
/// préfixe de sockets.
///
/// Aucune pré-suppression de fichier ici : `prepare_sockets_dir` a rasé le
/// répertoire entier avant le premier lancement.
///
/// `locale` transmet la langue courante via `RITORNELLO_LOCALE`, appliquée
/// **au lancement** seulement (inchangé).
pub fn spawn(
    exec: &str,
    register: &Path,
    name: &str,
    prefix: &Path,
    locale: Option<&str>,
) -> Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(exec);
    cmd.arg("--register").arg(register);
    cmd.arg("--name").arg(name);
    cmd.arg("--socket-prefix").arg(prefix);
    if let Some(locale) = locale {
        cmd.env("RITORNELLO_LOCALE", locale);
    }
    // Le chemin est nommé dans l'erreur : « No such file or directory » seul
    // laisse deviner **lequel** des chemins de `plugins.toml` est en cause, et
    // la confusion la plus courante est justement là — un `exec` de déploiement
    // (`/usr/local/lib/...`) recopié dans une configuration de développement,
    // où les binaires sont sous `target/debug/`.
    cmd.kill_on_drop(true).spawn().with_context(|| format!("executable {exec}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_un_manifeste_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"

[[plugin]]
name = "console"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-console"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 2);
        assert_eq!(m.plugins[0].name, "radio");
        assert_eq!(m.plugins[1].name, "console");
    }

    #[test]
    fn un_manifeste_sans_kind_se_charge() {
        // Le genre est desormais annonce par le binaire : le fichier ne le
        // porte plus.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 1);
        assert_eq!(m.plugins[0].name, "radio");
    }

    #[test]
    fn le_repertoire_de_sockets_est_neuf_a_chaque_demarrage() {
        // Un fichier rance d'une execution precedente est connectable et
        // ferait dialoguer le coeur avec un zombie : le repertoire est donc
        // rase, pas nettoye au cas par cas.
        let dir = tempfile::tempdir().unwrap();
        let sockets = dir.path().join("sockets");
        std::fs::create_dir_all(&sockets).unwrap();
        let rance = sockets.join("radio-source.sock");
        std::fs::write(&rance, "").unwrap();

        let rendu = prepare_sockets_dir(dir.path()).unwrap();
        assert_eq!(rendu, sockets);
        assert!(rendu.is_dir(), "le repertoire doit exister apres l'appel");
        assert!(!rance.exists(), "le fichier rance doit avoir disparu");
    }

    #[test]
    fn un_manifeste_absent_est_vide_mais_illisible_est_une_erreur() {
        // Absent = installation sans plugin, cas normal. Illisible (ici : le
        // « répertoire » parent est en réalité un fichier) = un problème à
        // nommer — « aucune source disponible » enverrait le diagnostic dans
        // la mauvaise direction.
        let dir = tempfile::tempdir().unwrap();
        let absent = PluginManifest::load(&dir.path().join("plugins.toml")).unwrap();
        assert!(absent.plugins.is_empty());
        let bouchon = dir.path().join("pas-un-repertoire");
        std::fs::write(&bouchon, "").unwrap();
        assert!(PluginManifest::load(&bouchon.join("plugins.toml")).is_err());
    }

    #[test]
    fn une_erreur_de_lancement_nomme_toujours_lexecutable() {
        let dir = tempfile::tempdir().unwrap();
        let e = spawn(
            "/chemin/qui/nexiste/pas/ritornello-plugin-bidon",
            &dir.path().join("register.sock"),
            "bidon",
            &dir.path().join("bidon"),
            None,
        )
        .expect_err("un executable absent doit echouer");
        let message = format!("{e:#}");
        assert!(
            message.contains("/chemin/qui/nexiste/pas/ritornello-plugin-bidon"),
            "l'erreur doit nommer l'executable cherche: {message}"
        );
    }

    #[test]
    fn manifeste_absent_donne_liste_vide() {
        let dir = tempfile::tempdir().unwrap();
        let m = PluginManifest::load(&dir.path().join("absent.toml")).unwrap();
        assert!(m.plugins.is_empty());
    }

    #[test]
    fn detecte_un_nom_duplique_sans_le_rejeter_ni_le_dedoublonner() {
        // Le doublon de nom était le contournement d'avant ce chantier pour
        // faire servir deux genres à un même binaire : un manifeste qui le
        // porte doit charger tel quel (les deux entrées), pas échouer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "mpd"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mpd"

[[plugin]]
name = "mpd"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mpd"

[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 3, "le doublon n'est pas dedoublonne au chargement");
        assert_eq!(noms_dupliques(&m.plugins), vec!["mpd".to_string()]);
    }

    #[test]
    fn aucun_nom_duplique_ne_signale_rien() {
        let plugins = vec![
            PluginConfig { name: "radio".into(), exec: "radio".into() },
            PluginConfig { name: "files".into(), exec: "files".into() },
        ];
        assert!(noms_dupliques(&plugins).is_empty());
    }
}
