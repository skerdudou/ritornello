use crate::bindings::Binding;
use ritornello_i18n::Catalog;
use serde::Deserialize;
use std::path::Path;

/// Un preset est une simple liste de bindings, sans nom de périphérique.
#[derive(Debug, Clone, Default, Deserialize)]
struct Preset {
    #[serde(default)]
    bindings: Vec<Binding>,
}

/// Preset introuvable, illisible ou invalide — un seul cas d'erreur côté
/// utilisateur : « ce preset n'existe pas ». Le détail part dans les journaux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPreset(pub String);

impl UnknownPreset {
    pub fn message(&self, catalog: &Catalog) -> String {
        catalog.get("unknown_preset").replace("{preset}", &self.0)
    }
}

impl std::fmt::Display for UnknownPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown preset: {}", self.0)
    }
}

impl std::error::Error for UnknownPreset {}

/// Un nom de preset est un identifiant simple : il vient du navigateur et sert
/// à construire un chemin, donc ni séparateur ni point (pas de `../`).
fn nom_valide(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Parse pur du listing d'un répertoire : ne garde que les `*.toml` au nom
/// valide, sans l'extension, triés et dédoublonnés. Séparé de l'accès disque
/// pour être testable (comme `audio_output::parse_device_list` du cœur).
pub fn parse_preset_names(entries: &[String]) -> Vec<String> {
    let mut noms: Vec<String> = entries
        .iter()
        .filter_map(|e| e.strip_suffix(".toml"))
        .filter(|n| nom_valide(n))
        .map(|n| n.to_string())
        .collect();
    noms.sort();
    noms.dedup();
    noms
}

/// Noms des presets disponibles. Répertoire absent ou illisible → liste vide.
pub fn list(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else {
        tracing::warn!("repertoire de presets {} illisible : aucun preset", root.display());
        return Vec::new();
    };
    let entries: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    parse_preset_names(&entries)
}

/// Parse pur d'un contenu TOML de preset (sans accès disque) : c'est
/// l'unique point de conversion texte → bindings, utilisé aussi bien par
/// `load` (preset livré) que par l'import depuis un fichier téléversé
/// (`admin::Op::ImportPreset`), pour qu'un seul parseur existe.
pub fn parse_preset(content: &str) -> Result<Vec<Binding>, String> {
    let preset: Preset = toml::from_str(content).map_err(|e| e.to_string())?;
    Ok(preset.bindings)
}

/// Charge les bindings d'un preset. Nom invalide, fichier absent ou TOML
/// illisible → `UnknownPreset` (avec un `warn` détaillant la vraie cause).
pub fn load(root: &Path, name: &str) -> Result<Vec<Binding>, UnknownPreset> {
    if !nom_valide(name) {
        tracing::warn!("nom de preset refuse: {name}");
        return Err(UnknownPreset(name.to_string()));
    }
    let path = root.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        tracing::warn!("preset {} illisible: {e}", path.display());
        UnknownPreset(name.to_string())
    })?;
    parse_preset(&text).map_err(|e| {
        tracing::warn!("preset {} invalide: {e}", path.display());
        UnknownPreset(name.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::Command;

    /// Racine des presets livrés dans le dépôt (`deploy/input-presets`).
    fn presets_livres() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/input-presets")
    }

    #[test]
    fn parse_preset_names_ne_garde_que_les_toml_valides() {
        let entries = vec![
            "mce.toml".to_string(),
            "keyboard.toml".to_string(),
            "README.md".to_string(),
            "..toml".to_string(),
            "../evasion.toml".to_string(),
            "mce.toml".to_string(),
        ];
        assert_eq!(parse_preset_names(&entries), vec!["keyboard", "mce"]);
    }

    #[test]
    fn list_decouvre_les_presets_dun_repertoire() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mce.toml"), "").unwrap();
        std::fs::write(dir.path().join("keyboard.toml"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        assert_eq!(list(dir.path()), vec!["keyboard", "mce"]);
    }

    #[test]
    fn list_repertoire_absent_donne_une_liste_vide() {
        assert!(list(Path::new("/nonexistent-presets-xyz")).is_empty());
    }

    #[test]
    fn parse_preset_valide_donne_les_bindings_attendus() {
        let toml =
            "[[bindings]]\ncode = 115\ncmd = \"VolumeUp\"\n\n[[bindings]]\ncode = 2\ncmd = \"Select\"\narg = 1\n";
        let b = parse_preset(toml).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].code, 115);
        assert_eq!(b[0].command(), Some(Command::VolumeUp));
        assert_eq!(b[1].command(), Some(Command::Select(1)));
    }

    #[test]
    fn parse_preset_toml_invalide_donne_une_erreur() {
        assert!(parse_preset("ceci n'est pas = du toml [").is_err());
    }

    #[test]
    fn parse_preset_lit_le_meme_contenu_que_le_fichier_livre() {
        let root = presets_livres();
        let text = std::fs::read_to_string(root.join("mce.toml")).unwrap();
        let via_texte = parse_preset(&text).unwrap();
        let via_fichier = load(&root, "mce").unwrap();
        assert_eq!(via_texte, via_fichier, "le parseur texte diverge du chargement fichier");
        assert!(!via_texte.is_empty());
    }

    #[test]
    fn load_lit_les_bindings_dun_preset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test.toml"),
            "[[bindings]]\ncode = 115\ncmd = \"VolumeUp\"\n\n[[bindings]]\ncode = 2\ncmd = \"Select\"\narg = 1\n",
        )
        .unwrap();
        let b = load(dir.path(), "test").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].code, 115);
        assert_eq!(b[0].command(), Some(Command::VolumeUp));
        assert_eq!(b[1].command(), Some(Command::Select(1)));
    }

    #[test]
    fn load_preset_inconnu_renvoie_une_erreur() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), "absent"), Err(UnknownPreset("absent".into())));
    }

    #[test]
    fn load_refuse_un_nom_detourne() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), "../../etc/passwd").is_err());
        assert!(load(dir.path(), "").is_err());
    }

    #[test]
    fn message_de_preset_inconnu_utilise_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "unknown_preset = \"preset inconnu : {preset}\"\n",
        )
        .unwrap();
        let cat = Catalog::load("generic-input", "fr", dir.path(), crate::GENERIC_INPUT_EN);
        assert_eq!(UnknownPreset("zzz".into()).message(&cat), "preset inconnu : zzz");
    }

    #[test]
    fn les_presets_livres_se_chargent_et_sont_non_vides() {
        let root = presets_livres();
        assert_eq!(list(&root), vec!["keyboard", "mce"]);

        let mce = load(&root, "mce").unwrap();
        assert!(!mce.is_empty());
        assert_eq!(mce.iter().find(|b| b.code == 115).unwrap().command(), Some(Command::VolumeUp));
        assert_eq!(mce.iter().find(|b| b.code == 513).unwrap().command(), Some(Command::Select(1)));
        assert_eq!(mce.iter().find(|b| b.code == 356).unwrap().command(), Some(Command::Power));

        let kbd = load(&root, "keyboard").unwrap();
        assert!(!kbd.is_empty());
        assert_eq!(kbd.iter().find(|b| b.code == 57).unwrap().command(), Some(Command::PlayPause));
        assert_eq!(kbd.iter().find(|b| b.code == 103).unwrap().command(), Some(Command::VolumeUp));
    }
}
