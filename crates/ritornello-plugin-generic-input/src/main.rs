mod admin;
mod bindings;
mod devices;
mod learn;
// Uniquement compile sous `cargo test` : `ui_placeholder_js` ne sert au run-
// time nulle part dans ce crate (contrairement a `placeholder_html` du coeur,
// utilise en repli par `web.rs`), seulement a `build.rs` (compilation
// separee, via `include!`) et a ses propres tests. Le compiler en continu
// dans le binaire declencherait un `dead_code` que `-D warnings` refuserait.
#[cfg(test)]
mod placeholder;
mod presets;

use crate::admin::GenericInputAdmin;
use crate::bindings::Bindings;
use crate::devices::Hub;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{InputPlugin, Runtime};
use ritornello_proto::InputMessage;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

pub(crate) const GENERIC_INPUT_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Moitié Input : consomme le mpsc alimenté par toutes les tâches de playback
/// evdev, quel que soit le périphérique d'origine.
struct EvdevInput {
    rx: mpsc::Receiver<InputMessage>,
}

#[async_trait::async_trait]
impl InputPlugin for EvdevInput {
    async fn next_command(&mut self) -> Result<InputMessage> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("all evdev loops have ended"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let bindings_path =
        PathBuf::from(env_or("RITORNELLO_INPUT_BINDINGS", "/etc/ritornello/input-bindings.toml"));
    let presets_root =
        PathBuf::from(env_or("RITORNELLO_INPUT_PRESETS", "/etc/ritornello/input-presets"));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    // Un plugin Input ne reçoit pas de `SetLocale` (le protocol ne le prévoit
    // que pour les sources) : la langue de la page vient de l'environnement.
    let locale = env_or("RITORNELLO_LOCALE", "en");
    let catalog = Arc::new(RwLock::new(Catalog::load(
        "generic-input",
        &locale,
        &locales_root,
        GENERIC_INPUT_EN,
    )));

    let (tx, rx) = mpsc::channel(32);
    let hub = Hub::new(Bindings::load(&bindings_path), tx);
    let input_root = PathBuf::from(devices::INPUT_DIR);
    let ouverts = hub.open_new_devices(&input_root);
    tracing::info!("{ouverts} input device(s) opened");

    // Les deux moitiés restent indépendantes : une panne de la page ne doit
    // pas couper la télécommande. C'est `Runtime::run` qui les tient
    // désormais, chacune dans sa tâche — la page n'est plus conditionnelle,
    // puisque le greffon announcement lui-même qu'il en a une.
    let admin = GenericInputAdmin { bindings_path, presets_root, input_root, hub, catalog };
    Runtime::from_args()?.input(EvdevInput { rx })?.admin(admin)?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_embarque_generic_input_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(GENERIC_INPUT_EN).unwrap().is_empty());
    }

    #[tokio::test]
    async fn la_moitie_input_vit_tant_quun_emetteur_du_canal_existe() {
        // Régression (2026-07-27) : déclaré sans `admin = true` et sans aucun
        // périphérique evdev ouvert (WSL, droits manquants sur /dev/input),
        // le plugin sortait aussitôt en exit 0 — la closure du `.map(...)`
        // qui construisait la moitié admin capturait le hub même sans être
        // appelée, et le lâchait ; or le hub tient l'émetteur du canal des
        // commands. Le contrat que `main` doit tenir est celui-ci : tant
        // qu'un émetteur vit, `next_command` attend au lieu de se terminer.
        let (tx, rx) = mpsc::channel::<InputMessage>(4);
        let mut input = EvdevInput { rx };
        tokio::select! {
            _ = input.next_command() => panic!("next_command ne doit pas se terminer tant qu'un emetteur vit"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
        // Émetteur lâché : fin propre, l'erreur nomme la cause.
        drop(tx);
        let e = input.next_command().await.unwrap_err();
        assert!(e.to_string().contains("evdev loops"));
    }

    // Le test `chemins_par_defaut` qui vivait ici ne testait que `env_or`,
    // c'est-à-dire `std::env` : il passait sur n'importe quel programme.
    // Supprimé plutôt que gardé pour le nombre (revue 2026-07-27).
}
