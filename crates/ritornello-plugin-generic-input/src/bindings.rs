use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_proto::Command;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Une touche liée à une commande. Le couple `cmd`/`arg` est exactement la
/// représentation sérialisée de `Command` (`#[serde(tag = "cmd", content =
/// "arg")]`) aplatie dans le binding : aucune liste de commands n'est
/// dupliquée, et le même objet transite tel quel en JSON vers l'IHM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub code: u16,
    #[serde(flatten)]
    pub command: Command,
}

impl Binding {
    /// Constructeur pratique pour les tests (partout ailleurs, un `Binding`
    /// arrive par désérialisation TOML/JSON, jamais construit à la main).
    #[cfg(test)]
    pub fn new(code: u16, command: &Command) -> Self {
        Binding { code, command: command.clone() }
    }

    /// Commande portée par ce binding. `Option` parce que la forme de repli
    /// documentée dans la spec (`cmd: String` + `arg: Option<u8>`) peut porter
    /// une commande inconnue ; sous la forme aplatie nominale, c'est toujours
    /// `Some`. Tout le reste du crate passe par cet accesseur, ce qui confine
    /// le repli éventuel à ce fichier.
    pub fn command(&self) -> Option<Command> {
        Some(self.command.clone())
    }
}

/// Les bindings d'un périphérique, identifié par son **name** (stable au
/// redémarrage) : tous les nœuds evdev portant ce name sont concernés.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    #[serde(default)]
    pub devices: Vec<Device>,
}

/// Erreur de validation typée : le texte utilisateur est produit à la
/// frontière via `message(&Catalog)` (modèle du plugin radio). `Display`
/// fournit une version anglaise pour les logs internes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateCode { device: String, code: u16 },
    SelectOutOfRange { device: String, arg: u8 },
    UnknownCommand { device: String, code: u16 },
}

impl ValidationError {
    /// Message localisé remonté à l'utilisateur (corps du 422 côté admin).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ValidationError::DuplicateCode { device, code } => catalog
                .get("duplicate_code")
                .replace("{code}", &code.to_string())
                .replace("{device}", device),
            ValidationError::SelectOutOfRange { device, arg } => catalog
                .get("select_out_of_range")
                .replace("{n}", &arg.to_string())
                .replace("{device}", device),
            ValidationError::UnknownCommand { device, code } => catalog
                .get("unknown_command")
                .replace("{code}", &code.to_string())
                .replace("{device}", device),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::DuplicateCode { device, code } => {
                write!(f, "code {code} bound twice on {device}")
            }
            ValidationError::SelectOutOfRange { device, arg } => {
                write!(f, "preset {arg} out of range 0-9 on {device}")
            }
            ValidationError::UnknownCommand { device, code } => {
                write!(f, "unknown command bound to code {code} on {device}")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

impl Bindings {
    /// Charge la table. Best-effort : fichier absent ou TOML invalide donnent
    /// une table clear avec un `warn` — jamais de panique, le plugin démarre et
    /// l'utilisateur corrige depuis la page d'admin.
    pub fn load(path: &Path) -> Bindings {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "bindings {} unreadable ({e}): starting with no binding, use the admin page",
                    path.display()
                );
                return Bindings::default();
            }
        };
        match toml::from_str::<Bindings>(&text) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "bindings {} invalid ({e}): starting with no binding, use the admin page",
                    path.display()
                );
                Bindings::default()
            }
        }
    }

    /// Écriture atomique : fichier temporaire puis `rename`, jamais de fichier
    /// tronqué si l'alimentation saute au mauvais moment.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        for dev in &self.devices {
            let mut vus = HashSet::new();
            for b in &dev.bindings {
                if !vus.insert(b.code) {
                    return Err(ValidationError::DuplicateCode {
                        device: dev.name.clone(),
                        code: b.code,
                    });
                }
                match b.command() {
                    None => {
                        return Err(ValidationError::UnknownCommand {
                            device: dev.name.clone(),
                            code: b.code,
                        })
                    }
                    Some(Command::Select(n)) if !(0..=9).contains(&n) => {
                        return Err(ValidationError::SelectOutOfRange {
                            device: dev.name.clone(),
                            arg: n,
                        })
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// Résolution au moment de l'événement : (name du périphérique, code) →
    /// commande. `None` = touche non liée, ignorée silencieusement.
    pub fn resolve(&self, device_name: &str, code: u16) -> Option<Command> {
        self.devices
            .iter()
            .find(|d| d.name == device_name)
            .and_then(|d| d.bindings.iter().find(|b| b.code == code))
            .and_then(|b| b.command())
    }

    /// Remplace l'intégralité des bindings d'un périphérique (création de
    /// l'entrée si elle n'existe pas). Utilisé par `load_preset`.
    pub fn replace_device(&mut self, device: &str, bindings: Vec<Binding>) {
        match self.devices.iter_mut().find(|d| d.name == device) {
            Some(d) => d.bindings = bindings,
            None => self.devices.push(Device { name: device.to_string(), bindings }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exemple() -> Bindings {
        Bindings {
            devices: vec![
                Device {
                    name: "eHome Infrared Transceiver".into(),
                    bindings: vec![
                        Binding::new(115, &Command::VolumeUp),
                        Binding::new(2, &Command::Select(1)),
                    ],
                },
                Device {
                    name: "USB Keyboard".into(),
                    bindings: vec![Binding::new(57, &Command::PlayPause)],
                },
            ],
        }
    }

    /// PREMIER test du chantier : `#[serde(flatten)]` sur un enum à tag
    /// adjacent est éprouvé en JSON dans ce projet, pas en TOML. S'il échoue,
    /// appliquer sans discussion le repli documenté dans la spec (champs
    /// `cmd: String` + `arg: Option<u8>` et conversions vers `Command`), qui
    /// garde exactement la même forme de fichier et de JSON.
    #[test]
    fn binding_roundtrip_toml() {
        let avec_arg = Binding::new(2, &Command::Select(1));
        let t = toml::to_string_pretty(&avec_arg).unwrap();
        assert!(t.contains("code = 2"), "TOML produit: {t}");
        assert!(t.contains("cmd = \"Select\""), "TOML produit: {t}");
        assert!(t.contains("arg = 1"), "TOML produit: {t}");
        assert_eq!(toml::from_str::<Binding>(&t).unwrap(), avec_arg);

        let sans_arg = Binding::new(115, &Command::VolumeUp);
        let t2 = toml::to_string_pretty(&sans_arg).unwrap();
        assert!(!t2.contains("arg"), "TOML produit: {t2}");
        assert_eq!(toml::from_str::<Binding>(&t2).unwrap(), sans_arg);
    }

    #[test]
    fn binding_json_porte_cmd_et_arg_a_plat() {
        assert_eq!(
            serde_json::to_value(Binding::new(2, &Command::Select(1))).unwrap(),
            serde_json::json!({ "code": 2, "cmd": "Select", "arg": 1 })
        );
        assert_eq!(
            serde_json::to_value(Binding::new(166, &Command::Stop)).unwrap(),
            serde_json::json!({ "code": 166, "cmd": "Stop" })
        );
        let b: Binding =
            serde_json::from_value(serde_json::json!({ "code": 9, "cmd": "Select", "arg": 8 }))
                .unwrap();
        assert_eq!(b.command(), Some(Command::Select(8)));
    }

    #[test]
    fn roundtrip_fichier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        exemple().save(&path).unwrap();
        assert_eq!(Bindings::load(&path), exemple());
    }

    #[test]
    fn fichier_absent_donne_une_table_vide() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Bindings::load(&dir.path().join("absent.toml")), Bindings::default());
    }

    #[test]
    fn toml_invalide_donne_une_table_vide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("casse.toml");
        std::fs::write(&path, "ceci n'est pas = du toml [").unwrap();
        assert_eq!(Bindings::load(&path), Bindings::default());
    }

    #[test]
    fn save_ne_laisse_pas_de_fichier_temporaire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        exemple().save(&path).unwrap();
        assert!(path.exists());
        assert!(!dir.path().join("input-bindings.toml.tmp").exists());
    }

    #[test]
    fn validate_refuse_un_code_lie_deux_fois_sur_un_meme_peripherique() {
        let mut b = exemple();
        b.devices[0].bindings.push(Binding::new(115, &Command::Mute));
        assert_eq!(
            b.validate(),
            Err(ValidationError::DuplicateCode {
                device: "eHome Infrared Transceiver".into(),
                code: 115
            })
        );
    }

    #[test]
    fn validate_accepte_le_meme_code_sur_deux_peripheriques_differents() {
        let mut b = exemple();
        b.devices[1].bindings.push(Binding::new(115, &Command::VolumeUp));
        assert!(b.validate().is_ok());
    }

    #[test]
    fn validate_accepte_select_0_la_touche_0_de_la_telecommande() {
        let mut b = exemple();
        b.devices[1].bindings.push(Binding::new(11, &Command::Select(0)));
        assert!(b.validate().is_ok());
    }

    #[test]
    fn validate_refuse_un_select_hors_bornes() {
        let mut b2 = exemple();
        b2.devices[1].bindings.push(Binding::new(11, &Command::Select(10)));
        assert_eq!(
            b2.validate(),
            Err(ValidationError::SelectOutOfRange { device: "USB Keyboard".into(), arg: 10 })
        );
    }

    #[test]
    fn plus10_se_lie_et_fait_le_tour_en_toml() {
        let b = Binding::new(11, &Command::Plus10);
        let t = toml::to_string_pretty(&b).unwrap();
        assert!(t.contains("cmd = \"Plus10\""), "TOML produit: {t}");
        assert!(!t.contains("arg"), "TOML produit: {t}");
        assert_eq!(toml::from_str::<Binding>(&t).unwrap(), b);
        let mut table = Bindings::default();
        table.devices.push(Device { name: "X".into(), bindings: vec![b] });
        assert!(table.validate().is_ok());
    }

    #[test]
    fn save_refuse_une_table_invalide_et_necrit_rien() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        let mut b = exemple();
        b.devices[0].bindings.push(Binding::new(115, &Command::Mute));
        assert!(b.save(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn message_de_validation_utilise_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "duplicate_code = \"code {code} lie deux fois sur {device}\"\n",
        )
        .unwrap();
        let cat =
            Catalog::load("generic-input", "fr", dir.path(), crate::GENERIC_INPUT_EN);
        let err = ValidationError::DuplicateCode { device: "X".into(), code: 42 };
        assert_eq!(err.message(&cat), "code 42 lie deux fois sur X");
    }

    #[test]
    fn resolve_trouve_la_commande_du_bon_peripherique() {
        let b = exemple();
        assert_eq!(b.resolve("eHome Infrared Transceiver", 115), Some(Command::VolumeUp));
        assert_eq!(b.resolve("eHome Infrared Transceiver", 2), Some(Command::Select(1)));
        assert_eq!(b.resolve("USB Keyboard", 57), Some(Command::PlayPause));
        // code non lié sur ce périphérique
        assert_eq!(b.resolve("USB Keyboard", 115), None);
        // périphérique inconnu
        assert_eq!(b.resolve("Souris", 115), None);
    }

    #[test]
    fn replace_device_remplace_ou_cree_lentree() {
        let mut b = exemple();
        b.replace_device("USB Keyboard", vec![Binding::new(50, &Command::Mute)]);
        assert_eq!(b.devices[1].bindings, vec![Binding::new(50, &Command::Mute)]);
        b.replace_device("Nouveau", vec![Binding::new(1, &Command::Power)]);
        assert_eq!(b.devices.len(), 3);
        assert_eq!(b.devices[2].name, "Nouveau");
    }
}
