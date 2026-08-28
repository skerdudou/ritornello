mod display;

use anyhow::Result;
use async_trait::async_trait;
use display::ConsoleDisplay;
use ritornello_plugin_sdk::{DisplayPlugin, Runtime};
use ritornello_proto::PlayerState;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Période du battement qui fait avancer l'horloge de veille.
///
/// **Dix secondes pour une horloge à la minute**, et ce n'est pas du gâchis :
/// `ConsoleDisplay::show` compare son rendu au précédent et n'écrit rien quand
/// les trois lignes sont identiques, donc un tour ne coûte qu'une lecture de
/// l'heure et trois comparaisons de chaînes tant que la minute n'a pas tourné.
/// Une période d'une minute, elle, ne serait pas alignée sur les minutes
/// rondes : l'affichage aurait pu retarder de presque une minute entière.
const BATTEMENT_HORLOGE: std::time::Duration = std::time::Duration::from_secs(10);

struct ConsolePlugin {
    /// Partagé avec le battement : les deux écrivent sur le même tty, et
    /// jamais en même temps.
    display: Arc<Mutex<ConsoleDisplay>>,
    /// La dernière trame reçue, que le battement réutilise pour redessiner.
    ///
    /// **Le battement ne fabrique pas d'état**, il rejoue le dernier : sans
    /// cela il devrait deviner ce que le cœur a annoncé, et un écran en veille
    /// perdrait le mot de veille au premier tour d'horloge.
    dernier: Arc<Mutex<Option<PlayerState>>>,
}

#[async_trait]
impl DisplayPlugin for ConsolePlugin {
    async fn show(&mut self, state: PlayerState) -> Result<()> {
        *self.dernier.lock().await = Some(state.clone());
        self.display.lock().await.show(&state)
    }
}

/// Redessine périodiquement tant que l'appareil est en veille, pour que
/// l'horloge avance.
///
/// Ne fait rien hors veille : c'est le cœur qui pousse alors, et à une cadence
/// bien supérieure. Ne fait rien non plus avant la première trame — il n'y a
/// alors rien à redessiner, et inventer un écran vide effacerait ce que le tty
/// montrait avant le lancement du greffon.
async fn bat_l_horloge(display: Arc<Mutex<ConsoleDisplay>>, dernier: Arc<Mutex<Option<PlayerState>>>) {
    loop {
        tokio::time::sleep(BATTEMENT_HORLOGE).await;
        let Some(etat) = dernier.lock().await.clone() else { continue };
        if !etat.standby {
            continue;
        }
        if let Err(e) = display.lock().await.show(&etat) {
            // Journalisé et non fatal : un tty momentanément indisponible ne
            // doit pas emporter le greffon, qui reste utile dès qu'il revient.
            tracing::warn!("could not refresh the standby clock: {e}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let tty = PathBuf::from(env_or("RITORNELLO_CONSOLE_TTY", "/dev/tty1"));
    let display = Arc::new(Mutex::new(ConsoleDisplay::open(&tty)?));
    let dernier = Arc::new(Mutex::new(None));
    tokio::spawn(bat_l_horloge(display.clone(), dernier.clone()));
    Runtime::from_args()?.display(ConsolePlugin { display, dernier })?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_console_ne_demande_pas_les_pochettes() {
        // Le pendant du `wants_covers` du serveur MPD, et la raison d'être du
        // corps par défaut : cet afficheur écrit trois lignes sur un tty de
        // vingt colonnes. Lui pousser jusqu'à deux mébioctets par piste, sur un
        // appareil qui en a mille, serait payé pour être jeté — et l'annonce
        // étant dérivée de cette méthode (voir `Runtime::display`), c'est
        // exactement ce que redéfinir ici provoquerait.
        //
        // Écrit du côté qui doit rester silencieux, et non du côté qui demande :
        // c'est ici que la régression se produirait, en ajoutant une surcharge
        // par mimétisme.
        let tty = tempfile::NamedTempFile::new().unwrap();
        let plugin = ConsolePlugin {
            display: Arc::new(Mutex::new(ConsoleDisplay::open(tty.path()).unwrap())),
            dernier: Arc::new(Mutex::new(None)),
        };
        assert!(!plugin.wants_covers(), "la console n'a que faire des octets d'une image");
    }

    #[tokio::test]
    async fn le_battement_redessine_en_veille_et_se_tait_le_reste_du_temps() {
        // Le battement rejoue la **dernière trame reçue** : sans elle, un tour
        // d'horloge effacerait le mot de veille que le cœur avait annoncé. Et
        // il ne touche à rien hors veille, où le cœur pousse déjà à la seconde.
        let tty = tempfile::NamedTempFile::new().unwrap();
        let display = Arc::new(Mutex::new(ConsoleDisplay::open(tty.path()).unwrap()));
        let dernier = Arc::new(Mutex::new(None));
        let mut plugin =
            ConsolePlugin { display: display.clone(), dernier: dernier.clone() };

        // Rien reçu : rien à redessiner.
        assert!(dernier.lock().await.is_none());

        plugin
            .show(PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() })
            .await
            .unwrap();
        let garde = dernier.lock().await;
        let retenu = garde.clone().expect("la trame doit etre retenue pour le battement");
        drop(garde);
        assert!(retenu.standby);
        assert_eq!(retenu.status.as_deref(), Some("VEILLE"));
    }
}
