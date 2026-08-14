use anyhow::{bail, Result};

/// One selectable ALSA PCM, as listed by `aplay -L`: the technical name and
/// the human-readable description the SPA shows first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub description: String,
}

/// Parses `aplay -L`: each non-indented line names a selectable PCM; the
/// indented lines under it are its description (kept, trimmed, joined with
/// " — "). The `null` PCM is filtered out: it discards audio — useless in an
/// audio chain, and it used to sit first in the list where the SPA's old
/// preselection fallback could send it on a distracted "Change" click.
pub fn parse_device_list(raw: &str) -> Vec<AudioDevice> {
    let mut devices: Vec<AudioDevice> = Vec::new();
    // While skipping `null`, its own indented lines must not leak into the
    // previous device's description.
    let mut skipping = false;
    for line in raw.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            if line.trim().is_empty() {
                continue;
            }
            let name = line.trim().to_string();
            skipping = name == "null";
            if !skipping {
                devices.push(AudioDevice { name, description: String::new() });
            }
        } else if !skipping {
            if let Some(d) = devices.last_mut() {
                let part = line.trim();
                if !part.is_empty() {
                    if !d.description.is_empty() {
                        d.description.push_str(" — ");
                    }
                    d.description.push_str(part);
                }
            }
        }
    }
    devices
}

pub fn list_devices() -> Result<Vec<AudioDevice>> {
    let out = std::process::Command::new("aplay").arg("-L").output()?;
    if !out.status.success() {
        bail!("aplay -L failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(parse_device_list(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garde_les_descriptions_et_filtre_null() {
        let raw = "null\n    Discard all samples (playback) or generate zero samples (capture)\n\
default\n    Playback/recording through the PulseAudio sound server\n\
sysdefault:CARD=Headphones\n    bcm2835 Headphones, bcm2835 Headphones\n    Default Audio Device\n";
        let devices = parse_device_list(raw);
        assert_eq!(
            devices,
            vec![
                AudioDevice {
                    name: "default".into(),
                    description: "Playback/recording through the PulseAudio sound server".into(),
                },
                AudioDevice {
                    name: "sysdefault:CARD=Headphones".into(),
                    description: "bcm2835 Headphones, bcm2835 Headphones — Default Audio Device".into(),
                },
            ]
        );
    }

    #[test]
    fn peripherique_sans_description_et_entree_vide() {
        assert_eq!(
            parse_device_list("hw:CARD=Loopback\n"),
            vec![AudioDevice { name: "hw:CARD=Loopback".into(), description: String::new() }]
        );
        assert_eq!(parse_device_list(""), Vec::<AudioDevice>::new());
    }
}
