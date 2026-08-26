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

/// Intervalle minimal entre deux requêtes vers MusicBrainz.
///
/// Le service demande une requête par seconde et par client, et ne l'applique
/// pas mollement. 1100 ms plutôt que 1000 pour ne pas jouer sur la borne : la
/// marge coûte cent millisecondes sur des tâches détachées que personne
/// n'attend.
pub const INTERVALLE_MIN: std::time::Duration = std::time::Duration::from_millis(1100);

/// Sérialise les requêtes et espace la suivante d'`INTERVALLE_MIN`.
///
/// Le verrou est **tenu pendant l'attente**, et c'est le mécanisme même : deux
/// tâches détachées parties en même temps se retrouvent en file au lieu de
/// mitrailler. Sans lui, le sondage de quatre candidats émettait quatre
/// requêtes dans la même milliseconde, ce que MusicBrainz refuse par des 503 —
/// donc un sondage qui échouait pour une raison qui n'a rien à voir avec le
/// découpage.
///
/// Une structure plutôt qu'un statique nu : c'est ce qui permet à un test
/// d'avoir sa propre instance. Le statique est la couche d'à côté.
pub struct Etrangleur(tokio::sync::Mutex<Option<tokio::time::Instant>>);

impl Etrangleur {
    pub fn new() -> Self {
        Self(tokio::sync::Mutex::new(None))
    }

    pub async fn attend(&self) {
        let mut garde = self.0.lock().await;
        if let Some(precedente) = *garde {
            let ecoule = precedente.elapsed();
            if ecoule < INTERVALLE_MIN {
                tokio::time::sleep(INTERVALLE_MIN - ecoule).await;
            }
        }
        *garde = Some(tokio::time::Instant::now());
    }
}

/// L'étrangleur du processus. Tous les chemins du greffon passent par lui —
/// disque, release, enregistrement — parce que le débit est compté par client
/// et non par fonctionnalité.
fn etrangleur() -> &'static Etrangleur {
    static E: std::sync::OnceLock<Etrangleur> = std::sync::OnceLock::new();
    E.get_or_init(Etrangleur::new)
}

/// Requête GET commune aux deux endpoints MusicBrainz utilisés ici (lookup par
/// TOC, recherche par artiste/album). `Ok(None)` = hors ligne ou réponse en
/// échec : les deux appelants traitent ça comme un silence, jamais une erreur
/// à faire remonter.
async fn requete_texte(url: &str) -> Result<Option<String>> {
    etrangleur().attend().await;
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

/// Score minimal d'une recherche de release pour être crue.
///
/// La recherche MusicBrainz rend presque toujours **quelque chose** de
/// plausible : sans seuil, `premier_release_id` croyait le premier résultat
/// quel qu'il soit, et un album mal orthographié dans les étiquettes d'un
/// fichier recevait une pochette fausse avec aplomb. 85 plutôt que 90 pour la
/// release, parce que la requête contraint deux champs (artiste et album) dont
/// l'un vient d'étiquettes arbitraires : un peu plus de tolérance qu'un titre
/// d'enregistrement, que la station écrit d'une seule main.
pub const SEUIL_RELEASE: u64 = 85;

/// MBID du premier résultat, **s'il est assez sûr**. `None` = rien trouvé,
/// réponse illisible, ou meilleur résultat trop incertain.
pub fn premier_release_id(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    let premiere = v.get("releases")?.as_array()?.first()?;
    // Score absent = refus, et un `warn` plutôt qu'un `debug` : c'est un champ
    // que l'API rend toujours, donc son absence est un changement de schéma.
    // Refuser garde la correction (pas de pochette fausse) et le niveau de
    // journal rend la panne diagnosticable, là où supposer « assez sûr »
    // restaurerait le défaut sans une ligne.
    let Some(score) = premiere.get("score").and_then(Value::as_u64) else {
        tracing::warn!("release search: no score field, refusing rather than guessing");
        return None;
    };
    if score < SEUIL_RELEASE {
        tracing::debug!("release search: best match scored {score}, under the {SEUIL_RELEASE} needed");
        return None;
    }
    premiere.get("id")?.as_str().map(str::to_string)
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

/// Score minimal d'une recherche d'enregistrement pour être crue.
///
/// Plus haut que `SEUIL_RELEASE` : ici les deux champs contraints viennent de
/// la **même** chaîne écrite d'une seule main par la station, donc un vrai
/// couple obtient un score franc. Et la validation sert à *choisir* entre deux
/// découpages : plus le seuil est haut, moins l'ordre inverse a de chances de
/// se glisser au-dessus.
pub const SEUIL_RECORDING: u64 = 90;

/// Ce qu'un enregistrement rendu par la recherche apprend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enregistrement {
    pub score: u64,
    /// Le titre **tel que MusicBrainz l'écrit**. C'est lui qu'on compare au
    /// candidat après normalisation, et cette comparaison porte la validation :
    /// le score seul est trop généreux.
    pub titre: String,
    /// Première release, s'il en a une. La pochette en vient.
    ///
    /// Pas de choix « intelligent » entre original, compilation et remaster :
    /// MusicBrainz ne les classe pas par pertinence, et ce serait une
    /// heuristique de plus pour un carré de 500 pixels.
    pub release_id: Option<String>,
}

/// Requête de recherche d'un enregistrement par artiste et titre.
///
/// Les deux valeurs viennent d'une **station**, donc d'une entrée qu'on ne
/// choisit pas : échappées pour les deux langages superposés qu'elles
/// traversent, Lucene puis l'URL. Voir la doc de `requete_release`, qui écrit
/// ce qu'une version antérieure y avait manqué.
pub fn requete_recording(artist: &str, title: &str) -> String {
    let echappe = |s: &str| pourcent_encode(&echappe_lucene(s));
    format!(
        "https://musicbrainz.org/ws/2/recording/?query=artist:%22{}%22%20AND%20recording:%22{}%22&fmt=json&limit=1",
        echappe(artist),
        echappe(title)
    )
}

/// Premier enregistrement de la réponse. `None` = rien, illisible, ou sans
/// score — voir `premier_release_id` pour le raisonnement sur le score absent.
pub fn premier_enregistrement(json: &str) -> Option<Enregistrement> {
    let v: Value = serde_json::from_str(json).ok()?;
    let premier = v.get("recordings")?.as_array()?.first()?;
    let Some(score) = premier.get("score").and_then(Value::as_u64) else {
        tracing::warn!("recording search: no score field, refusing rather than guessing");
        return None;
    };
    Some(Enregistrement {
        score,
        titre: premier.get("title")?.as_str()?.to_string(),
        release_id: premier
            .get("releases")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .and_then(|r| r.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Forme comparable d'un titre : minuscules, diacritiques retirés, et tout ce
/// qui n'est ni lettre ni chiffre ramené à un espace unique.
///
/// **Pas** une normalisation Unicode complète, et c'est assumé : une crate de
/// décomposition pour une soixantaine de caractères latins ne se justifie pas
/// dans ce dépôt, et un titre en écriture non latine n'a pas de diacritique à
/// retirer — il traverse cette fonction inchangé, ce qui est exactement le
/// comportement voulu.
pub fn normalise(s: &str) -> String {
    let mut mots: Vec<String> = Vec::new();
    let mut courant = String::new();
    for c in s.chars() {
        let c = sans_diacritique(c).to_lowercase().next().unwrap_or(c);
        if c.is_alphanumeric() {
            courant.push(c);
        } else if !courant.is_empty() {
            mots.push(std::mem::take(&mut courant));
        }
    }
    if !courant.is_empty() {
        mots.push(courant);
    }
    mots.join(" ")
}

/// Le caractère latin de base d'un caractère accentué, sinon lui-même.
///
/// Table plutôt qu'algorithme : elle couvre le français, l'espagnol,
/// l'allemand et le portugais, ce qui est le parc réel d'un appareil de salon
/// européen. Ce qui n'y figure pas passe inchangé.
fn sans_diacritique(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' | 'í' | 'ì' => 'i',
        'ô' | 'ö' | 'ó' | 'õ' | 'ò' => 'o',
        'ù' | 'û' | 'ü' | 'ú' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ÿ' | 'ý' => 'y',
        'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => 'A',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Î' | 'Ï' | 'Í' | 'Ì' => 'I',
        'Ô' | 'Ö' | 'Ó' | 'Õ' | 'Ò' => 'O',
        'Ù' | 'Û' | 'Ü' | 'Ú' => 'U',
        'Ç' => 'C',
        'Ñ' => 'N',
        autre => autre,
    }
}

/// Cherche un enregistrement, et rend ce qu'on en sait. `Ok(None)` = rien
/// trouvé ou hors ligne, comme partout dans ce module.
pub async fn cherche_enregistrement(artist: &str, title: &str) -> Result<Option<Enregistrement>> {
    let url = requete_recording(artist, title);
    let Some(body) = requete_texte(&url).await? else { return Ok(None) };
    Ok(premier_enregistrement(&body))
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

    /// Réponse de recherche de release **telle que MusicBrainz l'émet** : le
    /// champ `score` est toujours présent, et c'est lui qu'on ignorait.
    fn reponse_release(score: u64) -> String {
        format!(
            r#"{{"created":"2026-08-26T12:00:00.000Z","count":1,"offset":0,
            "releases":[{{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","score":{score},
            "title":"Kind of Blue","status":"Official"}}]}}"#
        )
    }

    #[test]
    fn une_release_assez_sure_est_retenue() {
        assert_eq!(
            premier_release_id(&reponse_release(SEUIL_RELEASE)).as_deref(),
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            "le seuil pile doit passer"
        );
    }

    #[test]
    fn une_release_trop_incertaine_est_refusee() {
        // Le defaut latent : aujourd'hui un album mal orthographie recoit une
        // pochette fausse avec aplomb, parce que la recherche rend toujours
        // quelque chose de plausible.
        assert_eq!(premier_release_id(&reponse_release(SEUIL_RELEASE - 1)), None);
    }

    #[test]
    fn un_score_absent_est_refuse_et_non_suppose_bon() {
        // Un score manquant veut dire « je ne sais pas ». Le supposer bon
        // reviendrait au defaut d'avant, en silence ; le supposer mauvais coupe
        // la fonctionnalite, mais visiblement (voir le `warn`).
        let sans = r#"{"releases":[{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","title":"X"}]}"#;
        assert_eq!(premier_release_id(sans), None);
    }

    #[test]
    fn une_reponse_sans_release_reste_none() {
        assert_eq!(premier_release_id(r#"{"releases":[]}"#), None);
        assert_eq!(premier_release_id("pas du json"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn letrangleur_espace_deux_requetes_consecutives() {
        // Horloge virtuelle : `sleep` avance le temps sans attendre, donc ce test
        // dure une microseconde tout en éprouvant un intervalle de 1,1 s.
        // L'étrangleur est **construit ici** et non pris d'un statique : deux
        // tests qui partageraient l'instance se pollueraient l'un l'autre.
        let e = Etrangleur::new();
        let depart = tokio::time::Instant::now();
        e.attend().await;
        assert_eq!(depart.elapsed(), std::time::Duration::ZERO, "la premiere ne doit pas attendre");
        e.attend().await;
        assert!(
            depart.elapsed() >= INTERVALLE_MIN,
            "la seconde doit etre espacee de {INTERVALLE_MIN:?}, mesure {:?}",
            depart.elapsed()
        );
    }

    /// Réponse de recherche d'enregistrement **telle que MusicBrainz l'émet** :
    /// `score`, `title`, et les releases dont sortira la pochette.
    fn reponse_recording(score: u64, titre: &str, avec_release: bool) -> String {
        let releases = if avec_release {
            r#","releases":[{"id":"11111111-2222-3333-4444-555555555555","title":"Kind of Blue"}]"#
        } else {
            ""
        };
        format!(
            r#"{{"created":"2026-08-26T12:00:00.000Z","count":1,"offset":0,
            "recordings":[{{"id":"99999999-8888-7777-6666-555555555555","score":{score},
            "title":"{titre}","length":545000{releases}}}]}}"#
        )
    }

    #[test]
    fn la_requete_dun_enregistrement_echappe_les_deux_langages() {
        // Lucene à l'intérieur des guillemets, puis l'URL par-dessus : la même
        // exigence que `requete_release`, pour la même raison — ces valeurs
        // viennent d'une station, donc d'une entrée qu'on ne choisit pas.
        let url = requete_recording(r#"AC"DC"#, "Back in Black & Co");
        assert!(url.starts_with("https://musicbrainz.org/ws/2/recording/?query="), "{url}");
        // Deux esperluettes structurelles seulement (avant fmt, avant limit) :
        // le brief attendait `== 1`, mais l'URL porte toujours `&fmt=json&limit=1`
        // en plus de `?query=`, donc deux '&' littéraux au minimum, jamais un
        // seul — voir le rapport de tâche pour le détail. Celle du titre est
        // percent-encodée (%26) et ne s'ajoute donc pas au compte.
        assert_eq!(
            url.matches('&').count(),
            2,
            "seuls fmt et limit doivent introduire un & ; rien depuis le titre : {url}"
        );
        assert!(url.contains("%5C%22"), "le guillemet doit etre echappe deux fois : {url}");
    }

    #[test]
    fn un_enregistrement_est_lu_avec_son_score_et_sa_release() {
        let e = premier_enregistrement(&reponse_recording(100, "So What", true)).unwrap();
        assert_eq!(e.score, 100);
        assert_eq!(e.titre, "So What");
        assert_eq!(e.release_id.as_deref(), Some("11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn un_enregistrement_sans_release_reste_exploitable() {
        // Le découpage est acquis même sans image : le couple artiste/titre vaut
        // par lui-même, et le cœur traite déjà une pochette absente en silence.
        let e = premier_enregistrement(&reponse_recording(100, "So What", false)).unwrap();
        assert_eq!(e.release_id, None);
        assert_eq!(e.titre, "So What");
    }

    #[test]
    fn une_reponse_illisible_ou_vide_rend_none() {
        assert!(premier_enregistrement(r#"{"recordings":[]}"#).is_none());
        assert!(premier_enregistrement("pas du json").is_none());
        // Score absent : refus, comme pour la release.
        assert!(premier_enregistrement(r#"{"recordings":[{"id":"x","title":"y"}]}"#).is_none());
    }

    #[test]
    fn la_normalisation_rend_comparables_deux_ecritures_du_meme_titre() {
        assert_eq!(normalise("So What"), normalise("so  what"));
        assert_eq!(normalise("Où es-tu ?"), normalise("ou es tu"));
        assert_eq!(normalise("Café/Crème"), normalise("cafe creme"));
    }

    #[test]
    fn la_normalisation_ne_confond_pas_deux_titres_differents() {
        // Le contrôle : une normalisation trop agressive accepterait n'importe
        // quoi, et la validation ne validerait plus rien.
        assert_ne!(normalise("So What"), normalise("So What Else"));
        assert_ne!(normalise("Naima"), normalise("Nauma"));
    }
}
