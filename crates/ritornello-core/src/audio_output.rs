use anyhow::{bail, Result};

/// Extrait les noms de périphériques de la sortie de `aplay -L` : chaque
/// ligne non indentée est le nom d'un périphérique sélectionnable ; les
/// lignes indentées qui suivent sont une description, ignorée ici.
pub fn parse_device_list(raw: &str) -> Vec<String> {
    raw.lines()
        .filter(|l| !l.is_empty() && !l.starts_with(' ') && !l.starts_with('\t'))
        .map(|l| l.trim().to_string())
        .collect()
}

pub fn list_devices() -> Result<Vec<String>> {
    let out = std::process::Command::new("aplay").arg("-L").output()?;
    if !out.status.success() {
        bail!("aplay -L a echoue: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(parse_device_list(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrait_les_noms_de_peripheriques_non_indentes() {
        let raw = "null\n    Discard all samples (playback) or generate zero samples (capture)\n\
default\n    Playback/recording through the PulseAudio sound server\n\
sysdefault:CARD=Headphones\n    bcm2835 Headphones, bcm2835 Headphones\n    Default Audio Device\n";
        let devices = parse_device_list(raw);
        assert_eq!(devices, vec!["null", "default", "sysdefault:CARD=Headphones"]);
    }

    #[test]
    fn entree_vide_donne_liste_vide() {
        assert_eq!(parse_device_list(""), Vec::<String>::new());
    }
}
