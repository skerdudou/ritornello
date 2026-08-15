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

pub mod m3u;
pub mod mount_options;
pub mod playlist;
pub mod roots;
pub mod scan;

/// Catalogue anglais embarqué, replié sur quand la locale demandée manque.
pub const FILES_EN: &str = include_str!("locales/en.toml");
