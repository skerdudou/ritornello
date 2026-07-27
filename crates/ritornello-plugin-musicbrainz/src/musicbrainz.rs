use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Ce qu'un disque reconnu apprend : l'artiste, l'album, et les titres dans
/// l'ordre des pistes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscInfo {
    pub artist: String,
    pub album: String,
    pub tracks: Vec<String>,
}

/// Met la TOC brute (`NTRACKS OFF1 … OFFN LEADOUT`, telle que le plugin cd la
/// place dans l'identité) au format attendu par MusicBrainz :
/// `1+NTRACKS+LEADOUT+OFF1+…+OFFN`.
///
/// Cette conversion vit ici, avec le seul code qui connaît MusicBrainz : le
/// plugin cd décrit un disque, il n'a pas à connaître le format de requête d'un
/// fournisseur de métadonnées particulier.
///
/// La validation est refaite intégralement, sans supposer que l'émetteur a bien
/// travaillé : l'identité arrive d'un autre processus, dans un JSON opaque que
/// le cœur ne relit pas.
pub fn mb_toc_param(raw: &str) -> Result<String> {
    let nums: Vec<u64> = raw
        .split_whitespace()
        .map(|s| s.parse::<u64>())
        .collect::<Result<_, _>>()
        .context("TOC non numérique")?;
    if nums.len() < 3 {
        bail!("TOC trop courte: {raw:?}");
    }
    let ntracks = nums[0] as usize;
    if nums.len() != ntracks + 2 {
        bail!("TOC incohérente ({} champs pour {} pistes)", nums.len(), ntracks);
    }
    let leadout = nums[nums.len() - 1];
    let offsets: Vec<String> = nums[1..nums.len() - 1].iter().map(u64::to_string).collect();
    Ok(format!("1+{}+{}+{}", ntracks, leadout, offsets.join("+")))
}

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

/// Lookup TOC « fuzzy » MusicBrainz. `Ok(None)` = pas trouvé ou hors ligne :
/// le plugin se tait alors, et l'affichage garde ce que la Source montrait.
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

    #[test]
    fn toc_musicbrainz_bien_forme() {
        // 3 pistes, offsets 150/22767/41887, leadout 63000
        assert_eq!(mb_toc_param("3 150 22767 41887 63000\n").unwrap(), "1+3+63000+150+22767+41887");
    }

    #[test]
    fn toc_invalide_rejetee_sans_appel_reseau() {
        // L'identité vient d'un autre processus : une TOC douteuse doit être
        // refusée ici, pas envoyée à un service tiers.
        assert!(mb_toc_param("").is_err());
        assert!(mb_toc_param("3 150 22767").is_err());
        assert!(mb_toc_param("abc def").is_err());
    }
}
