use anyhow::{Context, Result};
use ritornello_i18n::Catalog;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Station {
    pub name: String,
    pub url: String,
    pub preset: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stations {
    #[serde(default)]
    pub stations: Vec<Station>,
}

/// Erreur de validation typée : le texte utilisateur est produit à la
/// frontière via `message(&Catalog)`. `Display` fournit une version anglaise
/// pour les journaux internes (dev), hors périmètre i18n.
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    PresetOutOfRange { preset: u8, name: String },
    DuplicatePreset { preset: u8 },
    BadUrl { name: String, url: String },
}

impl ValidationError {
    /// Message localisé remonté à l'utilisateur (corps du 422 côté admin).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ValidationError::PresetOutOfRange { preset, name } => catalog
                .get("preset_out_of_range")
                .replace("{p}", &preset.to_string())
                .replace("{name}", name),
            ValidationError::DuplicatePreset { preset } => {
                catalog.get("preset_duplicate").replace("{p}", &preset.to_string())
            }
            ValidationError::BadUrl { name, url } => catalog
                .get("bad_url")
                .replace("{name}", name)
                .replace("{url}", url),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::PresetOutOfRange { preset, name } => {
                write!(f, "preset {preset} out of range 1-9 ({name})")
            }
            ValidationError::DuplicatePreset { preset } => write!(f, "duplicate preset {preset}"),
            ValidationError::BadUrl { name, url } => write!(f, "invalid URL for {name}: {url}"),
        }
    }
}

impl std::error::Error for ValidationError {}

impl Stations {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("lecture de {}", path.display()))?;
        let s: Stations = toml::from_str(&text)?;
        s.validate()?;
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        // Comme `state::save` et `Bindings::save` : sur une machine vierge
        // sans /etc/ritornello, le premier « Enregistrer » de la page d'admin
        // échouait sur une erreur d'E/S brute.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creation de {}", parent.display()))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.stations {
            if !(1..=9).contains(&s.preset) {
                return Err(ValidationError::PresetOutOfRange { preset: s.preset, name: s.name.clone() });
            }
            if !seen.insert(s.preset) {
                return Err(ValidationError::DuplicatePreset { preset: s.preset });
            }
            if !s.url.starts_with("http://") && !s.url.starts_with("https://") {
                return Err(ValidationError::BadUrl { name: s.name.clone(), url: s.url.clone() });
            }
        }
        Ok(())
    }

    pub fn by_preset(&self, preset: u8) -> Option<&Station> {
        self.stations.iter().find(|s| s.preset == preset)
    }

    pub fn next_preset(&self, from: u8) -> Option<u8> {
        let mut p: Vec<u8> = self.stations.iter().map(|s| s.preset).collect();
        p.sort_unstable();
        p.iter().copied().find(|x| *x > from).or_else(|| p.first().copied())
    }

    pub fn prev_preset(&self, from: u8) -> Option<u8> {
        let mut p: Vec<u8> = self.stations.iter().map(|s| s.preset).collect();
        p.sort_unstable();
        p.iter().rev().copied().find(|x| *x < from).or_else(|| p.last().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Stations {
        Stations {
            stations: vec![
                Station { name: "FIP".into(), url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(), preset: 1 },
                Station { name: "France Inter".into(), url: "http://icecast.radiofrance.fr/franceinter-midfi.mp3".into(), preset: 3 },
            ],
        }
    }

    #[test]
    fn roundtrip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stations.toml");
        sample().save(&path).unwrap();
        assert_eq!(Stations::load(&path).unwrap(), sample());
    }

    #[test]
    fn by_preset_trouve_la_station() {
        assert_eq!(sample().by_preset(3).unwrap().name, "France Inter");
        assert!(sample().by_preset(2).is_none());
    }

    #[test]
    fn next_prev_preset_rebouclent() {
        let s = sample();
        assert_eq!(s.next_preset(1), Some(3));
        assert_eq!(s.next_preset(3), Some(1)); // reboucle
        assert_eq!(s.prev_preset(3), Some(1));
        assert_eq!(s.prev_preset(1), Some(3)); // reboucle
    }

    #[test]
    fn validate_refuse_doublons_et_hors_bornes() {
        let mut s = sample();
        s.stations[1].preset = 1;
        assert!(s.validate().is_err());
        let mut s2 = sample();
        s2.stations[0].preset = 10;
        assert!(s2.validate().is_err());
        let mut s3 = sample();
        s3.stations[0].url = "ftp://nope".into();
        assert!(s3.validate().is_err());
    }

    #[test]
    fn validation_produit_une_erreur_typee() {
        let mut s = sample();
        s.stations[0].preset = 10;
        assert!(matches!(
            s.validate(),
            Err(ValidationError::PresetOutOfRange { preset: 10, .. })
        ));
        let mut d = sample();
        d.stations[1].preset = 1;
        assert!(matches!(d.validate(), Err(ValidationError::DuplicatePreset { preset: 1 })));
    }

    #[test]
    fn message_de_validation_utilise_le_catalogue() {
        use ritornello_i18n::Catalog;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(
            dir.path().join("radio/fr.toml"),
            "preset_out_of_range = \"préréglage {p} hors bornes ({name})\"\n",
        )
        .unwrap();
        let cat = Catalog::load("radio", "fr", dir.path(), crate::RADIO_EN);
        let err = ValidationError::PresetOutOfRange { preset: 10, name: "X".into() };
        assert_eq!(err.message(&cat), "préréglage 10 hors bornes (X)");
    }
}
