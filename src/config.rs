use anyhow::{bail, Context, Result};
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
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.stations {
            if !(1..=9).contains(&s.preset) {
                bail!("présélection {} hors bornes 1-9 ({})", s.preset, s.name);
            }
            if !seen.insert(s.preset) {
                bail!("présélection {} en double", s.preset);
            }
            if !s.url.starts_with("http://") && !s.url.starts_with("https://") {
                bail!("URL invalide pour {} : {}", s.name, s.url);
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
}
