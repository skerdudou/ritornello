use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Display,
    Input,
    /// Enrichit ce que joue la Source active sans que celle-ci le sache.
    ///
    /// **L'ordre de déclaration compte** : entre deux plugins `metadata` qui
    /// répondent pour le même morceau, le premier déclaré gagne. C'est le seul
    /// genre pour lequel l'ordre du fichier est porteur de sens, d'où la
    /// mention ici et dans le README.
    Metadata,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub kind: PluginKind,
    pub exec: String,
    #[serde(default)]
    pub admin: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginConfig>,
}

impl PluginManifest {
    /// Un fichier absent donne un manifeste vide : le cœur démarre sans
    /// plugin plutôt que d'échouer (cohérent avec le traitement déjà
    /// existant pour `stations.toml`).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(_) => Ok(Self::default()),
        }
    }
}

/// Spawn un plugin en lui passant le chemin de la socket de genre qu'il doit
/// lier, et — s'il déclare `admin = true` — un `--admin-socket`.
///
/// `locale` transmet la langue courante du cœur via `RITORNELLO_LOCALE` : elle
/// n'est appliquée qu'**au lancement** du processus enfant, pas en continu — un
/// changement de langue depuis la page de statut ne retraduit la page d'admin
/// d'un plugin qu'après redémarrage du service (le protocole `SetLocale` ne
/// couvre que les sources, pas les pages admin servies par les plugins).
pub fn spawn(
    exec: &str,
    socket_path: &Path,
    admin_socket: Option<&Path>,
    locale: Option<&str>,
) -> Result<tokio::process::Child> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creation du repertoire de sockets {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(socket_path);
    let mut cmd = tokio::process::Command::new(exec);
    cmd.arg("--socket").arg(socket_path);
    if let Some(admin) = admin_socket {
        let _ = std::fs::remove_file(admin);
        cmd.arg("--admin-socket").arg(admin);
    }
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
kind = "source"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"

[[plugin]]
name = "console"
kind = "display"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-console"
admin = true
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 2);
        assert_eq!(m.plugins[0].name, "radio");
        assert_eq!(m.plugins[0].kind, PluginKind::Source);
        assert!(!m.plugins[0].admin);
        assert_eq!(m.plugins[1].kind, PluginKind::Display);
        assert!(m.plugins[1].admin);
    }

    #[test]
    fn charge_les_plugins_metadata_dans_lordre_de_declaration() {
        // L'ordre du fichier est la priorité d'arbitrage : il doit survivre au
        // chargement, et rien ne doit trier cette liste.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "ouifm-metas"
kind = "metadata"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-ouifm-metas"

[[plugin]]
name = "musicbrainz"
kind = "metadata"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-musicbrainz"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        let metadata: Vec<&str> = m
            .plugins
            .iter()
            .filter(|p| p.kind == PluginKind::Metadata)
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(metadata, vec!["ouifm-metas", "musicbrainz"]);
    }

    #[test]
    fn une_erreur_de_lancement_nomme_lexecutable_cherche() {
        // Sans le chemin dans l'erreur, un `plugins.toml` a plusieurs entrees ne
        // laisse aucun moyen de savoir laquelle est fautive : « No such file or
        // directory (os error 2) » ne designe rien.
        let dir = tempfile::tempdir().unwrap();
        let e = spawn("/chemin/qui/nexiste/pas/ritornello-plugin-bidon", &dir.path().join("p.sock"), None, None)
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
}
