use crate::bindings::Bindings;
use crate::learn::LearnState;
use evdev::{Device, EventType};
use ritornello_proto::{Command, InputMessage};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

/// Racine des nœuds evdev sur un Linux standard.
pub const INPUT_DIR: &str = "/dev/input";

/// Filtre pur d'un listing de répertoire : ne garde que les nœuds `eventN`,
/// triés. Séparé de l'accès disque pour être testable sans matériel (comme
/// `audio_output::parse_device_list` du cœur).
pub fn event_nodes(root: &Path, entries: &[String]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = entries
        .iter()
        .filter(|n| {
            n.strip_prefix("event")
                .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|n| root.join(n))
        .collect();
    v.sort();
    v
}

/// Listing disque des nœuds evdev. Répertoire absent ou illisible → liste
/// vide et `warn` : jamais fatal.
pub fn scan_event_nodes(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else {
        tracing::warn!("repertoire {} illisible : aucun peripherique d'entree", root.display());
        return Vec::new();
    };
    let entries: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    event_nodes(root, &entries)
}

/// Ce que produit un appui de touche : la commande liée, ou rien. Le
/// périphérique en cours d'apprentissage n'émet plus rien (sinon apprendre
/// « Volume + » déclencherait un volume +) ; les autres continuent
/// normalement. Fonction pure, testable sans matériel.
pub fn key_outcome(
    bindings: &Bindings,
    learning_device: Option<&str>,
    device_name: &str,
    code: u16,
) -> Option<Command> {
    if learning_device == Some(device_name) {
        return None;
    }
    bindings.resolve(device_name, code)
}

/// État partagé entre la moitié Input (les tâches de lecture) et la moitié
/// Admin. `std::sync::RwLock` : les gardes sont toujours relâchées avant le
/// moindre `.await`, et `page()` (synchrone) peut lire sans runtime.
#[derive(Clone)]
pub struct Hub {
    pub bindings: Arc<RwLock<Bindings>>,
    pub learn: Arc<RwLock<LearnState>>,
    /// Nœuds actuellement ouverts : chemin → nom du périphérique.
    pub open: Arc<RwLock<BTreeMap<PathBuf, String>>>,
    pub tx: mpsc::Sender<InputMessage>,
}

impl Hub {
    pub fn new(bindings: Bindings, tx: mpsc::Sender<InputMessage>) -> Hub {
        Hub {
            bindings: Arc::new(RwLock::new(bindings)),
            learn: Arc::new(RwLock::new(LearnState::default())),
            open: Arc::new(RwLock::new(BTreeMap::new())),
            tx,
        }
    }

    /// Noms des périphériques actuellement ouverts, triés et dédoublonnés
    /// (plusieurs nœuds peuvent porter le même nom). Les entrées vides sont
    /// écartées : le nom vide est un placeholder de réservation posé dans
    /// `open` pendant que `Device::open` est en cours (voir
    /// `open_new_devices`), et la page d'admin sonde `device_names()` toutes
    /// les 300 ms pendant l'apprentissage — sans ce filtre elle afficherait
    /// transitoirement une entrée fantôme.
    pub fn device_names(&self) -> Vec<String> {
        let mut noms: Vec<String> = self
            .open
            .read()
            .unwrap()
            .values()
            .filter(|n| !n.is_empty())
            .cloned()
            .collect();
        noms.sort();
        noms.dedup();
        noms
    }

    /// Ouvre tous les nœuds evdev lisibles pas encore ouverts et lance une
    /// tâche de lecture par nœud. Renvoie le nombre de nouveaux nœuds. Un
    /// périphérique illisible (droits, disparu entre l'énumération et
    /// l'ouverture) est logué en `warn` et ignoré — jamais fatal.
    pub fn open_new_devices(&self, root: &Path) -> usize {
        let mut nouveaux = 0;
        for path in scan_event_nodes(root) {
            // Réservation atomique : le test d'appartenance et l'insertion se
            // font sous le même verrou en écriture, pour qu'un second rescan
            // concurrent (double-clic sur « Rafraîchir ») ne puisse pas
            // ouvrir le même nœud deux fois et lancer deux lecteurs dessus.
            {
                let mut open = self.open.write().unwrap();
                if open.contains_key(&path) {
                    continue;
                }
                open.insert(path.clone(), String::new());
            }
            let dev = match Device::open(&path) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("peripherique {} illisible, ignore: {e}", path.display());
                    self.open.write().unwrap().remove(&path);
                    continue;
                }
            };
            let name = dev.name().unwrap_or("?").to_string();
            self.open.write().unwrap().insert(path.clone(), name.clone());
            self.spawn_reader(path, dev, name);
            nouveaux += 1;
        }
        nouveaux
    }

    /// Une tâche de lecture par nœud, toutes alimentant le même mpsc.
    fn spawn_reader(&self, path: PathBuf, dev: Device, name: String) {
        let hub = self.clone();
        tokio::spawn(async move {
            let mut stream = match dev.into_event_stream() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("flux evdev {} indisponible: {e}", path.display());
                    hub.forget(&path);
                    return;
                }
            };
            tracing::info!("peripherique ecoute: {name} ({})", path.display());
            loop {
                let ev = match stream.next_event().await {
                    Ok(ev) => ev,
                    Err(e) => {
                        // Débranchement : cette tâche se termine, les autres
                        // continuent.
                        tracing::info!("lecture de {} terminee: {e}", path.display());
                        break;
                    }
                };
                if ev.event_type() != EventType::KEY || ev.value() != 1 {
                    continue;
                }
                // L'apprentissage consomme le premier appui et n'émet rien.
                let capture = { hub.learn.write().unwrap().capture(&name, ev.code()) };
                if capture {
                    continue;
                }
                // Aucune garde de verrou ne traverse le `.await` d'envoi.
                let cmd = {
                    let learn = hub.learn.read().unwrap();
                    let b = hub.bindings.read().unwrap();
                    key_outcome(&b, learn.device(), &name, ev.code())
                };
                if let Some(cmd) = cmd {
                    tracing::debug!("{name}: touche {} -> {cmd:?}", ev.code());
                    let _ = hub.tx.send(InputMessage::from(cmd)).await;
                }
            }
            hub.forget(&path);
        });
    }

    /// Oublie un nœud dont la lecture s'est terminée. Si plus aucun nœud ne
    /// porte ce nom, l'apprentissage éventuellement en cours dessus est
    /// abandonné (le périphérique a disparu).
    fn forget(&self, path: &Path) {
        let nom = self.open.write().unwrap().remove(path);
        if let Some(nom) = nom {
            if !self.device_names().contains(&nom) {
                self.learn.write().unwrap().cancel_if(&nom);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, Device as BindDevice};

    fn table() -> Bindings {
        Bindings {
            devices: vec![BindDevice {
                name: "eHome".into(),
                bindings: vec![Binding::new(115, &Command::VolumeUp)],
            }],
        }
    }

    fn hub_de_test() -> (Hub, mpsc::Receiver<InputMessage>) {
        let (tx, rx) = mpsc::channel(8);
        (Hub::new(table(), tx), rx)
    }

    #[test]
    fn event_nodes_ne_garde_que_les_noeuds_event() {
        let entries = vec![
            "event10".to_string(),
            "event2".to_string(),
            "mice".to_string(),
            "by-id".to_string(),
            "eventX".to_string(),
            "event".to_string(),
        ];
        assert_eq!(
            event_nodes(Path::new("/dev/input"), &entries),
            vec![PathBuf::from("/dev/input/event10"), PathBuf::from("/dev/input/event2")]
        );
    }

    #[test]
    fn scan_event_nodes_repertoire_absent_donne_vide() {
        assert!(scan_event_nodes(Path::new("/nonexistent-input-xyz")).is_empty());
    }

    #[test]
    fn key_outcome_resout_le_binding_du_bon_peripherique() {
        let t = table();
        assert_eq!(key_outcome(&t, None, "eHome", 115), Some(Command::VolumeUp));
        assert_eq!(key_outcome(&t, None, "eHome", 42), None);
        assert_eq!(key_outcome(&t, None, "Autre", 115), None);
    }

    #[test]
    fn key_outcome_supprime_lemission_du_seul_peripherique_en_apprentissage() {
        let mut t = table();
        t.devices.push(BindDevice {
            name: "USB Keyboard".into(),
            bindings: vec![Binding::new(115, &Command::VolumeUp)],
        });
        // apprentissage sur eHome : eHome muet, le clavier continue
        assert_eq!(key_outcome(&t, Some("eHome"), "eHome", 115), None);
        assert_eq!(
            key_outcome(&t, Some("eHome"), "USB Keyboard", 115),
            Some(Command::VolumeUp)
        );
    }

    #[test]
    fn device_names_dedoublonne_et_trie() {
        let (hub, _rx) = hub_de_test();
        {
            let mut open = hub.open.write().unwrap();
            open.insert(PathBuf::from("/dev/input/event3"), "eHome".into());
            open.insert(PathBuf::from("/dev/input/event1"), "USB Keyboard".into());
            open.insert(PathBuf::from("/dev/input/event2"), "eHome".into());
        }
        assert_eq!(hub.device_names(), vec!["USB Keyboard", "eHome"]);
    }

    #[tokio::test]
    async fn open_new_devices_sur_un_repertoire_sans_noeud_nouvre_rien() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mice"), "").unwrap();
        let (hub, _rx) = hub_de_test();
        assert_eq!(hub.open_new_devices(dir.path()), 0);
        assert!(hub.device_names().is_empty());
    }

    #[test]
    fn forget_retire_le_noeud_de_la_carte() {
        let (hub, _rx) = hub_de_test();
        let p = PathBuf::from("/dev/input/event7");
        hub.open.write().unwrap().insert(p.clone(), "eHome".into());
        hub.forget(&p);
        assert!(hub.device_names().is_empty());
    }

    #[test]
    fn le_hub_supprime_lemission_du_peripherique_en_apprentissage() {
        let (hub, _rx) = hub_de_test();
        hub.bindings.write().unwrap().devices.push(BindDevice {
            name: "USB Keyboard".into(),
            bindings: vec![Binding::new(115, &Command::VolumeUp)],
        });
        hub.learn.write().unwrap().learn("eHome");

        let sortie = |nom: &str, code: u16| {
            let learn = hub.learn.read().unwrap();
            let b = hub.bindings.read().unwrap();
            key_outcome(&b, learn.device(), nom, code)
        };
        assert_eq!(sortie("eHome", 115), None);
        assert_eq!(sortie("USB Keyboard", 115), Some(Command::VolumeUp));

        // une fois le code capturé, eHome réémet
        hub.learn.write().unwrap().capture("eHome", 115);
        assert_eq!(sortie("eHome", 115), Some(Command::VolumeUp));
    }

    #[test]
    fn forget_abandonne_lapprentissage_quand_le_dernier_noeud_disparait() {
        let (hub, _rx) = hub_de_test();
        let p1 = PathBuf::from("/dev/input/event1");
        let p2 = PathBuf::from("/dev/input/event2");
        {
            let mut open = hub.open.write().unwrap();
            open.insert(p1.clone(), "eHome".into());
            open.insert(p2.clone(), "eHome".into());
        }
        hub.learn.write().unwrap().learn("eHome");
        // un seul des deux nœuds disparaît : l'apprentissage continue
        hub.forget(&p1);
        assert_eq!(hub.learn.read().unwrap().device(), Some("eHome"));
        // le dernier disparaît : l'apprentissage est abandonné
        hub.forget(&p2);
        assert_eq!(hub.learn.read().unwrap().snapshot(), None);
    }
}
