use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Ce qu'un disque reconnu apprend : l'artiste, l'album, et les titres dans
/// l'ordre des pistes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscInfo {
    pub artist: String,
    pub album: String,
    pub tracks: Vec<String>,
    /// MBID de la release reconnue. C'est la clé de la pochette (voir
    /// [`url_caa`]) : le lookup par TOC la porte déjà, inutile de la
    /// redemander par une recherche texte.
    pub release_id: Option<String>,
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
        .context("non-numeric TOC")?;
    if nums.len() < 3 {
        bail!("TOC too short: {raw:?}");
    }
    let ntracks = nums[0] as usize;
    if nums.len() != ntracks + 2 {
        bail!("inconsistent TOC ({} fields for {} tracks)", nums.len(), ntracks);
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
                release_id: release.get("id").and_then(Value::as_str).map(String::from),
            });
        }
    }
    None
}

/// Requête GET commune aux deux endpoints MusicBrainz utilisés ici (lookup par
/// TOC, recherche par artiste/album). `Ok(None)` = hors ligne ou réponse en
/// échec : les deux appelants traitent ça comme un silence, jamais une erreur
/// à faire remonter.
async fn requete_texte(url: &str) -> Result<Option<String>> {
    // Version tirée du Cargo.toml, comme l'annuaire du plugin radio : un
    // user-agent figé mentirait à la première montée de version.
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "ritornello/",
            env!("CARGO_PKG_VERSION"),
            " (https://github.com/skerdudou/ritornello)"
        ))
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::info!("MusicBrainz unreachable: {e}");
            return Ok(None);
        }
    };
    if !resp.status().is_success() {
        return Ok(None);
    }
    match resp.text().await {
        Ok(b) => Ok(Some(b)),
        Err(e) => {
            tracing::info!("MusicBrainz: response read interrupted: {e}");
            Ok(None)
        }
    }
}

/// Lookup TOC « fuzzy » MusicBrainz. `Ok(None)` = pas trouvé ou hors ligne :
/// le plugin se tait alors, et l'affichage garde ce que la Source montrait.
pub async fn lookup(toc: &str, ntracks: usize) -> Result<Option<DiscInfo>> {
    let url = format!(
        "https://musicbrainz.org/ws/2/discid/-?toc={toc}&fmt=json&inc=recordings+artist-credits"
    );
    let Some(body) = requete_texte(&url).await? else { return Ok(None) };
    Ok(parse_lookup(&body, ntracks))
}

/// URL de la face avant d'une release.
///
/// `front-500` et non `front` : mesure du 2026-08-24, 75 249 octets contre
/// 2 670 705 pour l'original — le cœur plafonne son téléchargement à 2 Mio, un
/// `front` nu serait donc refusé en silence. Un 404 est le cas courant —
/// beaucoup de releases n'ont pas d'image — et le cœur le traite en silence.
pub fn url_caa(mbid: &str) -> String {
    format!("https://coverartarchive.org/release/{mbid}/front-500")
}

/// Échappe une valeur pour la **phrase Lucene** qui l'accueille.
///
/// Dans une phrase entre guillemets, seuls le guillemet et l'antislash sont
/// significatifs pour l'analyseur : un guillemet non échappé referme la phrase
/// et ce qui suit devient de la syntaxe.
fn echappe_lucene(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Pourcent-encode une valeur : tout ce qui n'est pas « non réservé » au sens
/// de la RFC 3986 y passe.
///
/// Octet par octet et non caractère par caractère : c'est la forme d'un
/// pourcent-encodage correct pour de l'UTF-8, et les titres d'album en sont
/// pleins.
fn pourcent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for o in s.as_bytes() {
        if o.is_ascii_alphanumeric() || matches!(*o, b'-' | b'.' | b'_' | b'~') {
            out.push(*o as char);
        } else {
            out.push_str(&format!("%{o:02X}"));
        }
    }
    out
}

/// Requête de recherche d'une release par artiste et album.
///
/// Les deux valeurs viennent d'**étiquettes de fichier arbitraires**, donc
/// d'une entrée qu'on ne choisit pas : elles sont échappées pour les deux
/// langages superposés qu'elles traversent, Lucene à l'intérieur des
/// guillemets puis l'URL par-dessus. Une version antérieure ne traitait que
/// l'espace et le guillemet, au motif que le reste n'apparaît pas dans des
/// métadonnées musicales — c'est faux, et l'échec était silencieux : un album
/// contenant `#` tronquait la requête au fragment, un `&` y injectait un
/// paramètre. L'hôte, lui, ne peut pas changer (il est en dur ci-dessous),
/// donc le pire cas reste une recherche fausse ou vide, jamais une requête
/// ailleurs.
pub fn requete_release(artist: &str, album: &str) -> String {
    let echappe = |s: &str| pourcent_encode(&echappe_lucene(s));
    format!(
        "https://musicbrainz.org/ws/2/release/?query=artist:%22{}%22%20AND%20release:%22{}%22&fmt=json&limit=1",
        echappe(artist),
        echappe(album)
    )
}

/// MBID du premier résultat. `None` = rien trouvé, ou réponse illisible.
pub fn premier_release_id(json: &str) -> Option<String> {
    serde_json::from_str::<Value>(json)
        .ok()?
        .get("releases")?
        .as_array()?
        .first()?
        .get("id")?
        .as_str()
        .map(str::to_string)
}

/// Recherche une release par artiste et album, et rend son identifiant.
///
/// C'est le chemin générique (fichier sans pochette, flux radio dont les
/// métadonnées textuelles suffisent) : contrairement au chemin disque, il ne
/// tient pas de TOC et doit deviner la release à partir d'un texte. `Ok(None)`
/// = rien trouvé ou hors ligne, exactement comme [`lookup`] : le plugin se
/// tait, il ne sait rien de plus que ce qu'on lui a donné.
pub async fn cherche_release(artist: &str, album: &str) -> Result<Option<String>> {
    let url = requete_release(artist, album);
    let Some(body) = requete_texte(&url).await? else { return Ok(None) };
    Ok(premier_release_id(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/mb_discid.json");

    // Le champ "id" de cette fixture a ete ajoute a la main pour cette tache,
    // pas capture avec le reste de la reponse : c'est un MBID valide, mais
    // emprunte a une autre mesure (la release de Kind of Blue mesuree le
    // 2026-08-24 pour l'URL front-500, cf. url_caa ci-dessous), donc ce n'est
    // presque certainement pas la release contre laquelle cette fixture a ete
    // capturee a l'origine. Sans consequence pour le test qui l'utilise
    // (parse_lookup_retient_le_release_id) : il ne verifie que la forme du
    // champ (36 caracteres), jamais sa valeur. Quiconque voudrait tirer une
    // conclusion plus forte de cette fixture (ex. verifier que le MBID
    // correspond bien a cet enregistrement precis) doit d'abord la
    // recapturer.

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

    #[test]
    fn l_url_du_cover_art_archive_demande_une_taille_bornee() {
        // `front` nu rend un PNG de 2 670 705 octets ; `front-500`, 75 249.
        assert_eq!(
            url_caa("e32a3f0b-1c19-3170-bb1c-650893774744"),
            "https://coverartarchive.org/release/e32a3f0b-1c19-3170-bb1c-650893774744/front-500"
        );
    }

    #[test]
    fn parse_lookup_retient_le_release_id() {
        // Le MBID est la clé de l'image, et il était jeté. La fixture réelle
        // du répertoire tests/fixtures est `mb_discid.json` (3 pistes).
        let info = parse_lookup(FIXTURE, 3).unwrap();
        assert!(
            info.release_id.as_deref().is_some_and(|id| id.len() == 36),
            "un MBID fait 36 caracteres, obtenu {:?}",
            info.release_id
        );
    }

    #[test]
    fn la_requete_de_release_echappe_les_guillemets() {
        // Mesure du 2026-08-24 : cette requête rend « Kind of Blue » au score 100.
        let q = requete_release("Miles Davis", "Kind of Blue");
        assert!(q.contains("artist:%22Miles%20Davis%22"), "{q}");
        assert!(q.contains("release:%22Kind%20of%20Blue%22"), "{q}");
        assert!(q.contains("fmt=json"), "{q}");
        assert!(q.contains("limit=1"), "{q}");
    }

    #[test]
    fn la_requete_de_release_survit_a_des_etiquettes_hostiles() {
        // Ces valeurs viennent d'etiquettes de fichier arbitraires. Avec
        // l'echappement minimal d'origine, le `#` tronquait la requete au
        // fragment et le `&` y injectait un parametre — une recherche fausse
        // ou vide, en silence.
        let q = requete_release("AC/DC & Co", "Drum #1 = 100%");
        let params: Vec<&str> = q.split('&').collect();
        assert_eq!(params.len(), 3, "aucun parametre injecte : query, fmt, limit — {q}");
        assert!(q.contains("fmt=json"), "{q}");
        assert!(q.contains("limit=1"), "{q}");
        assert!(!q.contains('#'), "un fragment tronquerait tout ce qui suit — {q}");
        assert!(q.contains("artist:%22AC%2FDC%20%26%20Co%22"), "{q}");
        assert!(q.contains("release:%22Drum%20%231%20%3D%20100%25%22"), "{q}");

        // Etage Lucene : un guillemet non echappe refermerait la phrase, et ce
        // qui suit deviendrait de la syntaxe.
        let q = requete_release("Say \"Yes\"", "a\\b");
        assert!(q.contains("artist:%22Say%20%5C%22Yes%5C%22%22"), "{q}");
        assert!(q.contains("release:%22a%5C%5Cb%22"), "{q}");
    }

    #[test]
    fn le_pourcent_encodage_traite_l_utf8_octet_par_octet() {
        // Un caractere non ASCII fait plusieurs octets, et chacun doit etre
        // encode : « é » vaut %C3%A9, jamais un seul %E9.
        assert_eq!(pourcent_encode("Café"), "Caf%C3%A9");
        assert_eq!(pourcent_encode("a-b_c.d~e"), "a-b_c.d~e", "les non reserves passent tels quels");
    }

    #[test]
    fn premier_release_id_lit_le_premier_resultat() {
        let json = r#"{"count":135,"releases":[{"id":"e32a3f0b-1c19-3170-bb1c-650893774744","score":100},{"id":"autre"}]}"#;
        assert_eq!(
            premier_release_id(json).as_deref(),
            Some("e32a3f0b-1c19-3170-bb1c-650893774744")
        );
        assert_eq!(premier_release_id(r#"{"releases":[]}"#), None);
        assert_eq!(premier_release_id("pas du json"), None);
    }
}
