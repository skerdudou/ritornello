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
use ritornello_plugin_sdk::{run_admin_plugin, run_input_plugin, InputPlugin};
use ritornello_proto::Command;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

pub(crate) const GENERIC_INPUT_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn arg_value(flag: &str) -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == flag).map(|i| {
        let value = args
            .get(i + 1)
            .unwrap_or_else(|| panic!("{flag} requiert une valeur (aucun argument apres {flag})"));
        PathBuf::from(value)
    })
}

/// Moitié Input : consomme le mpsc alimenté par toutes les tâches de lecture
/// evdev, quel que soit le périphérique d'origine.
struct EvdevInput {
    rx: mpsc::Receiver<Command>,
}

#[async_trait::async_trait]
impl InputPlugin for EvdevInput {
    async fn next_command(&mut self) -> Result<Command> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("toutes les boucles evdev sont terminees"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = arg_value("--socket").expect("--socket <path> requis");
    // `--admin-socket` n'est fourni par le cœur que si `admin = true` dans
    // plugins.toml. Absent (oubli lors d'une mise à jour de plugins.toml, ou
    // usage volontaire sans page d'admin), on continue en mode dégradé :
    // la moitié Input tourne seule, sans page web.
    let admin_socket = arg_value("--admin-socket");
    if admin_socket.is_none() {
        tracing::warn!(
            "--admin-socket absent : la page d'administration ne sera pas servie, seule la moitie Input tourne"
        );
    }
    let bindings_path =
        PathBuf::from(env_or("RITORNELLO_INPUT_BINDINGS", "/etc/ritornello/input-bindings.toml"));
    let presets_root =
        PathBuf::from(env_or("RITORNELLO_INPUT_PRESETS", "/etc/ritornello/input-presets"));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    // Un plugin Input ne reçoit pas de `SetLocale` (le protocole ne le prévoit
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
    tracing::info!("{ouverts} peripherique(s) d'entree ouvert(s)");

    // La moitié admin n'est construite que si `--admin-socket` a été fourni
    // (mode dégradé sinon, voir plus haut).
    let admin = admin_socket.map(|admin_socket| {
        (
            GenericInputAdmin {
                bindings_path,
                presets_root,
                input_root,
                hub,
                catalog,
            },
            admin_socket,
        )
    });

    // Les deux moitiés sont indépendantes : une panne de la socket admin ne
    // doit pas couper la télécommande, et réciproquement. Chaque moitié tourne
    // dans sa propre tâche tokio::spawn : une panique y est capturée dans le
    // JoinHandle (JoinError) au lieu de dérouler la pile de l'autre moitié.
    // Quand `admin` est `None`, seule la moitié Input est lancée : jamais de
    // `try_join!` ici, les deux tâches (quand les deux existent) restent
    // suivies indépendamment.
    let input_handle = tokio::spawn(async move { run_input_plugin(EvdevInput { rx }, &socket_path).await });

    match admin {
        Some((admin, admin_socket)) => {
            let admin_handle = tokio::spawn(async move { run_admin_plugin(admin, &admin_socket).await });
            let (input_res, admin_res) = tokio::join!(input_handle, admin_handle);
            log_half("moitie input", input_res);
            log_half("moitie admin", admin_res);
        }
        None => {
            log_half("moitie input", input_handle.await);
        }
    }

    Ok(())
}

/// Logue le résultat d'une des deux moitiés (succès / erreur applicative /
/// panique) sans jamais faire remonter l'échec d'une moitié sur l'autre.
fn log_half(label: &str, res: std::result::Result<Result<()>, tokio::task::JoinError>) {
    match res {
        Ok(Ok(())) => tracing::warn!("plugin generic-input ({label}) termine normalement"),
        Ok(Err(e)) => tracing::warn!("plugin generic-input ({label}) erreur: {e}"),
        Err(join_err) => tracing::error!("plugin generic-input ({label}) a panique: {join_err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_embarque_generic_input_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(GENERIC_INPUT_EN).unwrap().is_empty());
    }

    #[test]
    fn chemins_par_defaut() {
        std::env::remove_var("RITORNELLO_INPUT_BINDINGS_TEST");
        assert_eq!(
            env_or("RITORNELLO_INPUT_BINDINGS_TEST", "/etc/ritornello/input-bindings.toml"),
            "/etc/ritornello/input-bindings.toml"
        );
        std::env::set_var("RITORNELLO_INPUT_BINDINGS_TEST", "/tmp/x.toml");
        assert_eq!(
            env_or("RITORNELLO_INPUT_BINDINGS_TEST", "/etc/ritornello/input-bindings.toml"),
            "/tmp/x.toml"
        );
        std::env::remove_var("RITORNELLO_INPUT_BINDINGS_TEST");
    }
}
