//! Ce que le plugin et le binaire racine de montage ont en commun.
//!
//! Deux `[[bin]]` d'un même crate ne partagent pas leurs modules : cette
//! bibliothèque est ce qui garantit que le côté privilégié et le côté qui écrit
//! la configuration lisent exactement la même grammaire.
//!
//! Les modules propres au plugin (`admin`, `mount`, `state`, `store`) restent
//! déclarés dans `main.rs` : le binaire de montage n'en a que faire, et les y
//! placer lui imposerait des dépendances qu'un `oneshot` lancé par systemd n'a
//! aucune raison de tirer.

pub mod duree;
pub mod explore;
pub mod m3u;
pub mod mount;
pub mod mount_options;
pub mod playlist;
pub mod roots;
pub mod sante;
pub mod scan;
pub mod smb;
pub mod store;
pub mod volumes;

// Uniquement compilé sous `cargo test` : rien de ce module ne sert au runtime
// dans ce crate. Il est employé par `build.rs` (compilation séparée, via
// `include!`) et par ses propres tests. Le compiler en continu déclencherait un
// `dead_code` que `-D warnings` refuserait.
#[cfg(test)]
mod placeholder;

/// Catalogue anglais embarqué, replié sur quand la locale demandée manque.
pub const FILES_EN: &str = include_str!("locales/en.toml");

#[cfg(test)]
mod tests {
    /// Le pack français **livré**, lu depuis `deploy/`, et non une copie de
    /// test : c'est bien ce fichier-là qui partira sur l'appareil.
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/files/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        // Une clé présente d'un seul côté afficherait l'anglais au milieu du
        // français, sans prévenir : `Catalog::load` se replie silencieusement
        // sur l'embarqué clé par clé.
        let en = ritornello_i18n::try_parse(crate::FILES_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }
}
