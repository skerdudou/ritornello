//! Catalogue i18n partagé de ritornello.
//!
//! Deux couches indépendantes par composant :
//! - `own` : anglais embarqué du composant (`en.toml`), surchargé par le pack
//!   externe `<root>/<composant>/<lang>.toml`.
//! - `common` : anglais embarqué dans ce crate, surchargé par
//!   `<root>/common/<lang>.toml`.
//!
//! Résolution par clé : `own` → `common` → la clé elle-même (filet de secours).
//! Interpolation : le composant fait `catalog.get(key)` puis
//! `str::replace("{n}", &n.to_string())` (aucun moteur de template).

use std::collections::HashMap;
use std::path::Path;

/// Vocabulaire commun anglais embarqué dans le crate.
const COMMON_EN: &str = include_str!("locales/common_en.toml");

/// Parse pur d'un pack TOML plat (`clé = "valeur"`). TOML invalide → map vide.
/// Séparé de l'accès disque pour être testable (comme `audio_output::parse_device_list`).
pub fn parse_pack(s: &str) -> HashMap<String, String> {
    toml::from_str(s).unwrap_or_default()
}

/// Surcharge `base` avec le pack TOML lu sur disque en `path`. Fichier absent :
/// silencieux (cas normal). TOML illisible : `warn` loggé, `base` inchangée.
fn overlay_from_disk(base: &mut HashMap<String, String>, path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    match toml::from_str::<HashMap<String, String>>(&text) {
        Ok(ext) => base.extend(ext),
        Err(e) => tracing::warn!("pack i18n {} ignoré (TOML invalide): {e}", path.display()),
    }
}

pub struct Catalog {
    own: HashMap<String, String>,
    common: HashMap<String, String>,
}

impl Catalog {
    /// Construit le catalogue d'un composant pour une langue donnée.
    /// Part de l'anglais embarqué (`own_en` pour `own`, `COMMON_EN` pour
    /// `common`), puis superpose les packs externes présents et valides.
    /// Jamais de panique : un pack absent ou invalide laisse l'anglais.
    pub fn load(component: &str, locale: &str, root: &Path, own_en: &str) -> Catalog {
        let mut own = parse_pack(own_en);
        let mut common = parse_pack(COMMON_EN);
        overlay_from_disk(&mut common, &root.join("common").join(format!("{locale}.toml")));
        overlay_from_disk(&mut own, &root.join(component).join(format!("{locale}.toml")));
        Catalog { own, common }
    }

    /// Résout une clé : `own` → `common` → la clé elle-même.
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.own
            .get(key)
            .or_else(|| self.common.get(key))
            .map(String::as_str)
            .unwrap_or(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Écrit `<root>/<sous_dossier>/<fichier>` et retourne le TempDir racine.
    fn ecrire(dir: &std::path::Path, sous_dossier: &str, fichier: &str, contenu: &str) {
        let d = dir.join(sous_dossier);
        std::fs::create_dir_all(&d).unwrap();
        let mut f = std::fs::File::create(d.join(fichier)).unwrap();
        f.write_all(contenu.as_bytes()).unwrap();
    }

    #[test]
    fn parse_pack_lit_le_toml_plat_et_ignore_l_invalide() {
        let m = parse_pack("a = \"un\"\nb = \"deux\"\n");
        assert_eq!(m.get("a").map(String::as_str), Some("un"));
        assert_eq!(m.get("b").map(String::as_str), Some("deux"));
        assert!(parse_pack("ceci n'est pas du toml =").is_empty());
    }

    #[test]
    fn own_prime_sur_common() {
        let dir = tempfile::tempdir().unwrap();
        // own_en définit "error", common l'a aussi : own doit gagner.
        let cat = Catalog::load("core", "en", dir.path(), "error = \"own-error\"\n");
        assert_eq!(cat.get("error"), "own-error");
    }

    #[test]
    fn externe_surcharge_l_embarque_own() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "core", "fr.toml", "standby = \"VEILLE\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.get("standby"), "VEILLE");
    }

    #[test]
    fn externe_surcharge_l_embarque_common() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "common", "fr.toml", "error = \"Erreur\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "");
        assert_eq!(cat.get("error"), "Erreur");
    }

    #[test]
    fn cle_manquante_repli_anglais_puis_cle_elle_meme() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        // pas de pack fr : on garde l'anglais embarqué
        assert_eq!(cat.get("standby"), "STANDBY");
        // clé inconnue : on renvoie la clé elle-même
        assert_eq!(cat.get("inconnue"), "inconnue");
    }

    #[test]
    fn toml_invalide_est_ignore_sans_paniquer() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "core", "fr.toml", "ceci = n'est pas valide");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.get("standby"), "STANDBY"); // repli anglais, pas de panique
    }
}
