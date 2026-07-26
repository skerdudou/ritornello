use crate::keymap::map_key;
use anyhow::{Context, Result};
use evdev::{Device, EventType};
use ritornello_proto::Command;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Choisit le chemin du périphérique evdev à ouvrir parmi `candidates`
/// (chemin, nom), par sous-chaîne insensible à la casse. Renvoie `None` si
/// aucun nom ne correspond. Si plusieurs correspondent (récepteurs MCE
/// exposant plusieurs nœuds), loggue un `warn!` listant les candidats puis
/// prend le premier. Fonction pure, testable, séparée de l'ouverture réelle.
pub fn select_device_path(candidates: &[(PathBuf, String)], name_contains: &str) -> Option<PathBuf> {
    let needle = name_contains.to_lowercase();
    let matches: Vec<&(PathBuf, String)> = candidates
        .iter()
        .filter(|(_, name)| name.to_lowercase().contains(&needle))
        .collect();
    if matches.len() > 1 {
        let liste: Vec<String> = matches
            .iter()
            .map(|(p, n)| format!("{} ({})", p.display(), n))
            .collect();
        tracing::warn!(
            "plusieurs périphériques correspondent à « {name_contains} », on prend le premier: {}",
            liste.join(", ")
        );
    }
    matches.first().map(|(p, _)| p.clone())
}

pub fn find_device(name_contains: &str) -> Result<Device> {
    if let Ok(forced) = std::env::var("RITORNELLO_MCE_DEVICE") {
        let dev = Device::open(&forced)
            .with_context(|| format!("ouverture du périphérique forcé {forced}"))?;
        tracing::info!("télécommande (forcée): {} ({forced})", dev.name().unwrap_or("?"));
        return Ok(dev);
    }
    let candidates: Vec<(PathBuf, String)> = evdev::enumerate()
        .map(|(path, dev)| (path, dev.name().unwrap_or("").to_string()))
        .collect();
    match select_device_path(&candidates, name_contains) {
        Some(path) => {
            let dev = Device::open(&path)
                .with_context(|| format!("ouverture de {}", path.display()))?;
            tracing::info!("télécommande: {} ({})", dev.name().unwrap_or("?"), path.display());
            Ok(dev)
        }
        None => anyhow::bail!("aucun périphérique input dont le nom contient « {name_contains} »"),
    }
}

pub async fn run(device: Device, tx: mpsc::Sender<Command>) -> Result<()> {
    let mut stream = device.into_event_stream().context("event stream evdev")?;
    loop {
        let ev = stream.next_event().await?;
        if ev.event_type() == EventType::KEY && ev.value() == 1 {
            if let Some(cmd) = map_key(ev.code()) {
                tracing::debug!("touche {} -> {:?}", ev.code(), cmd);
                let _ = tx.send(cmd).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_device_path_prend_le_seul_candidat_correspondant() {
        let cands = vec![
            (PathBuf::from("/dev/input/event0"), "USB Keyboard".to_string()),
            (PathBuf::from("/dev/input/event1"), "Media Center Ed. eHome".to_string()),
        ];
        assert_eq!(
            select_device_path(&cands, "Media Center"),
            Some(PathBuf::from("/dev/input/event1"))
        );
    }

    #[test]
    fn select_device_path_prend_le_premier_si_plusieurs_candidats() {
        // Récepteur MCE exposant deux nœuds au nom similaire : on prend le
        // premier (et un warn est loggé). Ici on vérifie le choix déterministe.
        let cands = vec![
            (PathBuf::from("/dev/input/event2"), "eHome Infrared Transceiver".to_string()),
            (PathBuf::from("/dev/input/event3"), "eHome Infrared Transceiver Consumer Control".to_string()),
        ];
        assert_eq!(
            select_device_path(&cands, "ehome"),
            Some(PathBuf::from("/dev/input/event2"))
        );
    }

    #[test]
    fn select_device_path_aucun_candidat() {
        let cands = vec![(PathBuf::from("/dev/input/event0"), "USB Keyboard".to_string())];
        assert_eq!(select_device_path(&cands, "Media Center"), None);
    }

    #[test]
    fn find_device_utilise_le_chemin_force_par_env() {
        // Chemin forcé inexistant : find_device DOIT tenter de l'ouvrir (et
        // échouer en le mentionnant), prouvant qu'il n'a pas fait de recherche.
        std::env::set_var("RITORNELLO_MCE_DEVICE", "/dev/input/inexistant-xyz");
        let res = find_device("peu importe");
        std::env::remove_var("RITORNELLO_MCE_DEVICE");
        let err = match res {
            Ok(_) => panic!("l'ouverture du chemin forcé doit échouer"),
            Err(e) => e,
        };
        assert!(format!("{err:#}").contains("inexistant-xyz"));
    }
}
