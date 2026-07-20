use crate::disc::DiscInfo;
use anyhow::Result;
use serde_json::Value;

/// Cherche dans les releases un media dont le nombre de pistes correspond au
/// disque inséré, et en extrait artiste / album / titres.
pub fn parse_lookup(json: &str, ntracks: usize) -> Option<DiscInfo> {
    let v: Value = serde_json::from_str(json).ok()?;
    let releases = v.get("releases")?.as_array()?;
    for release in releases {
        let Some(media) = release.get("media").and_then(Value::as_array) else { continue };
        for m in media {
            let tracks = m.get("tracks").and_then(Value::as_array);
            let Some(tracks) = tracks else { continue };
            if tracks.len() != ntracks {
                continue;
            }
            let titles: Vec<String> = tracks
                .iter()
                .filter_map(|t| t.get("title").and_then(Value::as_str).map(String::from))
                .collect();
            if titles.len() != ntracks {
                continue;
            }
            return Some(DiscInfo {
                artist: release
                    .pointer("/artist-credit/0/name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
                album: release.get("title").and_then(Value::as_str).unwrap_or("?").to_string(),
                tracks: titles,
            });
        }
    }
    None
}

/// Lookup TOC « fuzzy » MusicBrainz. Ok(None) = pas trouvé / hors ligne
/// (l'appelant garde l'affichage « Piste N »).
pub async fn lookup(toc: &str, ntracks: usize) -> Result<Option<DiscInfo>> {
    let url = format!(
        "https://musicbrainz.org/ws/2/discid/-?toc={toc}&fmt=json&inc=recordings+artist-credits"
    );
    let client = reqwest::Client::builder()
        .user_agent("ritornello/0.1 (https://github.com/skerdudou/ritornello)")
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::info!("MusicBrainz injoignable: {e}");
            return Ok(None);
        }
    };
    if !resp.status().is_success() {
        return Ok(None);
    }
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            tracing::info!("MusicBrainz: lecture de la réponse interrompue: {e}");
            return Ok(None);
        }
    };
    Ok(parse_lookup(&body, ntracks))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/mb_discid.json");

    #[test]
    fn parse_extrait_artiste_album_pistes() {
        let info = parse_lookup(FIXTURE, 3).unwrap();
        assert_eq!(info.artist, "Miles Davis");
        assert_eq!(info.album, "Kind of Blue");
        assert_eq!(info.tracks, vec!["So What", "Freddie Freeloader", "Blue in Green"]);
    }

    #[test]
    fn parse_rejette_si_nb_pistes_incoherent() {
        assert!(parse_lookup(FIXTURE, 12).is_none());
    }

    #[test]
    fn parse_rejette_json_vide_ou_invalide() {
        assert!(parse_lookup("{}", 3).is_none());
        assert!(parse_lookup("pas du json", 3).is_none());
        assert!(parse_lookup("{\"releases\":[]}", 3).is_none());
    }
}
