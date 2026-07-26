use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Display,
    Input,
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
        std::fs::create_dir_all(parent)?;
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
    Ok(cmd.kill_on_drop(true).spawn()?)
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
    fn manifeste_absent_donne_liste_vide() {
        let dir = tempfile::tempdir().unwrap();
        let m = PluginManifest::load(&dir.path().join("absent.toml")).unwrap();
        assert!(m.plugins.is_empty());
    }
}
