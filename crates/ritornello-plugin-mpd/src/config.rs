//! L'adresse et le port d'écoute du serveur MPD.
//!
//! Un fichier absent ou illisible retombe sur les défauts en journalisant :
//! c'est la politique de `Stations::load` côté radio, et elle vaut ici pour la
//! même raison — un greffon qui refuse de démarrer pour un fichier mal formé
//! disparaît de la page de statut au lieu d'y expliquer son problème.

use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_listen() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    6600
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Adresse d'écoute. `0.0.0.0` par défaut, comme le serveur web de
    /// l'appareil : la même surface, déjà exposée.
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self { listen: default_listen(), port: default_port() }
    }
}

impl Config {
    /// Charge la config depuis `path`, ou retombe sur les défauts en
    /// journalisant si le fichier est absent, illisible, ou invalide une fois
    /// parsé. Ne renvoie jamais d'erreur : un greffon qui refuse de démarrer
    /// pour un fichier mal formé disparaît de la page de statut au lieu d'y
    /// expliquer son problème.
    pub fn load(path: &Path) -> Self {
        let texte = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::info!("no config at {}: {e}; using defaults", path.display());
                return Self::default();
            }
        };
        match toml::from_str::<Self>(&texte) {
            Ok(c) => match c.validate() {
                Ok(()) => c,
                Err(raison) => {
                    tracing::warn!("invalid config at {}: {raison}; using defaults", path.display());
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!("unreadable config at {}: {e}; using defaults", path.display());
                Self::default()
            }
        }
    }

    /// Rend une **clé** de sources_catalog, pas une phrase : la page d'admin la
    /// traduit (Task 9). Même convention que les refus de la radio.
    pub fn validate(&self) -> Result<(), String> {
        if self.listen.trim().is_empty() {
            return Err("listen_empty".into());
        }
        if self.port == 0 {
            return Err("port_zero".into());
        }
        Ok(())
    }

    /// Enregistre la config sur disque, refusée d'abord si elle est invalide.
    /// L'erreur renvoyée est, comme `validate`, une clé de sources_catalog — jamais
    /// une phrase ni un message d'E/S brut : la page d'admin la traduit.
    ///
    /// Appelée par `admin.rs` (Task 9), qui résout la clé renvoyée en cas
    /// d'échec en phrase de sources_catalog avant de répondre.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let texte = toml::to_string_pretty(self).map_err(|_| "save_failed".to_string())?;
        // Temporaire puis renommage : le renommage est atomique sur le même
        // système de fichiers, donc aucune coupure ne laisse un toml tronqué à
        // la place du bon.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, texte).map_err(|_| "save_failed".to_string())?;
        std::fs::rename(&tmp, path).map_err(|_| "save_failed".to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_config_absente_donne_les_defauts() {
        // Un fichier manquant n'est pas une erreur : le greffon doit demarrer
        // ecoutant 0.0.0.0:6600 sans qu'on ait rien provisionne.
        let c = Config::load(std::path::Path::new("/nexiste/pas.toml"));
        assert_eq!(c.listen, "0.0.0.0");
        assert_eq!(c.port, 6600);
    }

    #[test]
    fn une_config_partielle_complete_par_les_defauts() {
        let c: Config = toml::from_str("port = 6601").unwrap();
        assert_eq!(c.listen, "0.0.0.0");
        assert_eq!(c.port, 6601);
    }

    #[test]
    fn le_port_zero_est_refuse() {
        // 0 demanderait au noyau un port libre : le client ne saurait pas lequel.
        let c = Config { listen: "0.0.0.0".into(), port: 0 };
        assert!(c.validate().is_err());
    }

    #[test]
    fn une_adresse_vide_est_refusee() {
        let c = Config { listen: String::new(), port: 6600 };
        assert!(c.validate().is_err());
    }

    #[test]
    fn valider_rend_des_cles_de_catalogue_et_non_des_phrases() {
        // Convention repo : la page d'admin (Task 9) traduit la clé. Une
        // phrase toute faite ici ne pourrait pas etre relocalisee, et casser
        // discretement en anglais dans une page francaise.
        let clear = Config { listen: String::new(), port: 6600 };
        assert_eq!(clear.validate().unwrap_err(), "listen_empty");
        let zero = Config { listen: "0.0.0.0".into(), port: 0 };
        assert_eq!(zero.validate().unwrap_err(), "port_zero");
    }

    #[test]
    fn lenregistrement_est_atomique_et_relisible() {
        // Ecriture par fichier temporaire puis renommage : une coupure de current
        // ne laisse jamais un toml tronque a la place du bon.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        let c = Config { listen: "127.0.0.1".into(), port: 6601 };
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path), c);
        assert!(!dir.path().join("mpd.toml.tmp").exists(), "le temporaire ne survit pas");
    }

    #[test]
    fn lenregistrement_refuse_une_config_invalide_sans_toucher_au_disque() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        let invalide = Config { listen: "0.0.0.0".into(), port: 0 };
        assert_eq!(invalide.save(&path).unwrap_err(), "port_zero");
        assert!(!path.exists(), "rien ne doit etre ecrit quand la validation refuse");
    }

    #[test]
    fn un_toml_illisible_ne_fait_pas_echouer_le_demarrage() {
        // Meme politique que les stations de la radio : on retombe sur les defauts
        // en journalisant, plutot que de refuser de demarrer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        std::fs::write(&path, "ceci n'est pas du toml =").unwrap();
        assert_eq!(Config::load(&path), Config::default());
    }

    #[test]
    fn une_config_syntaxiquement_valide_mais_refusee_retombe_aussi_sur_les_defauts() {
        // Distinct du test precedent : ici le toml parse, mais `validate()`
        // refuse le contenu (port a 0). `load` doit retomber sur les
        // defauts dans ce cas aussi, pas seulement sur une erreur de parsing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpd.toml");
        std::fs::write(&path, "port = 0\n").unwrap();
        assert_eq!(Config::load(&path), Config::default());
    }
}
