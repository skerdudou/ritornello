mod display;

use anyhow::Result;
use async_trait::async_trait;
use display::ConsoleDisplay;
use ritornello_plugin_sdk::{DisplayPlugin, Runtime};
use ritornello_proto::PlayerState;
use std::path::PathBuf;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct ConsolePlugin {
    display: ConsoleDisplay,
}

#[async_trait]
impl DisplayPlugin for ConsolePlugin {
    async fn show(&mut self, state: PlayerState) -> Result<()> {
        self.display.show(&state)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let tty = PathBuf::from(env_or("RITORNELLO_CONSOLE_TTY", "/dev/tty1"));
    let display = ConsoleDisplay::open(&tty)?;
    Runtime::from_args()?.display(ConsolePlugin { display })?.run().await
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
        let plugin = ConsolePlugin { display: ConsoleDisplay::open(tty.path()).unwrap() };
        assert!(!plugin.wants_covers(), "la console n'a que faire des octets d'une image");
    }
}
