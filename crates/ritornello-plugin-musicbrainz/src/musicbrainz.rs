use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Ce qu'un disque reconnu apprend : l'artiste, l'album, et les titres dans
/// l'ordre des pistes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscInfo {
    pub artist: String,
    pub album: String,
    pub tracks: Vec<String>,
    /// URL de la pochette, **déjà résolue** : celle du pressage reconnu quand
    /// il annonce une face avant, celle de l'album (`release-group`) sinon.
    ///
    /// Une URL et non un MBID, parce que la réponse porte de quoi choisir
    /// entre deux niveaux et que ce choix se fait ici, une fois, à l'endroit
    /// qui voit le `cover-art-archive`. Un MBID obligerait l'appelant à
    /// redécider — et il n'aurait plus l'information pour le faire.
    ///
    /// `None` veut dire **« ne demande pas d'image »**, pas « disque
    /// inconnu » : le cas ne reste que si la réponse nie la face avant *et*
    /// ne porte aucun release-group.
    pub cover_url: Option<String>,
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

/// Ce que le bloc `cover-art-archive` d'une release dit de sa face avant.
///
/// Trois états et non un booléen : l'absence du bloc (« cette réponse ne dit
/// rien ») ne doit pas être confondue avec `Absente` (« l'archive affirme
/// qu'il n'y en a pas »). Confondre les deux ferait taire la pochette sur
/// toute réponse qui ne porterait pas ce bloc, ce qui serait une régression
/// silencieuse pour un gain nul.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaceAvant {
    /// L'archive annonce une face avant typée : `/front-500` la sert.
    Presente,
    /// Aucune face avant typée pour ce pressage. Mesuré le 2026-08-26 sur
    /// `82ebb36b-0a0f-3608-9c7d-743d9003fbf8` : quatre images (un dos, un
    /// livret, une rondelle, et une sans aucun type) pour un `front: false`,
    /// et `/front-500` y rend bien 404 — l'endpoint suit le **typage**, pas la
    /// présence d'images. Le repli est la pochette de l'album, jamais une
    /// image devinée : voir [`url_caa_groupe`].
    Absente,
    /// Le bloc n'est pas dans la réponse : on ne sait pas.
    Inconnue,
}

/// Lit le bloc `cover-art-archive` d'une release.
///
/// `darkened` vaut `Absente` : l'archive masque alors les images pour raison
/// légale, et les demander ne rend rien.
fn face_avant(release: &Value) -> FaceAvant {
    let Some(caa) = release.get("cover-art-archive").and_then(Value::as_object) else {
        return FaceAvant::Inconnue;
    };
    let Some(front) = caa.get("front").and_then(Value::as_bool) else {
        return FaceAvant::Inconnue;
    };
    let assombrie = caa.get("darkened").and_then(Value::as_bool).unwrap_or(false);
    if front && !assombrie {
        FaceAvant::Presente
    } else {
        FaceAvant::Absente
    }
}

/// Préférence entre deux candidates, du meilleur au pire. Plus petit = mieux.
fn rang(face: FaceAvant) -> u8 {
    match face {
        FaceAvant::Presente => 0,
        // Avant `Absente` : sans le bloc, on ne sait pas, et l'optimisme est
        // le comportement historique — au pire un 404 que le cœur avale.
        FaceAvant::Inconnue => 1,
        // Départagée en dernier, mais pas perdue pour autant : elle se rabat
        // sur la pochette de l'album (voir `depouille`).
        FaceAvant::Absente => 2,
    }
}

/// Extrait artiste / album / titres d'une release **dont le nombre de pistes
/// correspond**. `None` si elle ne correspond pas.
fn depouille(release: &Value, ntracks: usize) -> Option<DiscInfo> {
    let media = release.get("media").and_then(Value::as_array)?;
    for m in media {
        let Some(tracks) = m.get("tracks").and_then(Value::as_array) else { continue };
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
        // Le choix du niveau se fait ici, une seule fois, à l'endroit qui voit
        // le `cover-art-archive` : ce pressage-ci s'il a une face avant,
        // l'album sinon.
        let cover_url = match face_avant(release) {
            FaceAvant::Presente | FaceAvant::Inconnue => {
                release.get("id").and_then(Value::as_str).map(url_caa)
            }
            FaceAvant::Absente => {
                release.pointer("/release-group/id").and_then(Value::as_str).map(url_caa_groupe)
            }
        };
        return Some(DiscInfo {
            artist: release
                .pointer("/artist-credit/0/name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            album: release.get("title").and_then(Value::as_str).unwrap_or("?").to_string(),
            tracks: titles,
            cover_url,
        });
    }
    None
}

/// Cherche dans les releases un media dont le nombre de pistes correspond au
/// disque inséré, et en extrait artiste / album / titres.
///
/// **Le nombre de pistes reste le seul filtre** ; entre les candidates qui le
/// passent, la présence d'une face avant départage. Mesuré le 2026-08-26 sur
/// le lookup que ce module construit : 25 releases rendues, 10 avec une face
/// avant, et la **première** — celle que retenait la version précédente — n'en
/// avait aucune. Le disque partait donc sans image alors qu'une candidate
/// recevable en portait une.
///
/// Le texte vient toujours de la release retenue. L'image aussi, **sauf** si
/// cette release n'a pas de face avant : elle vient alors de l'album, donc
/// possiblement d'un autre pressage (voir [`url_caa_groupe`]). Le compromis est
/// assumé dans ce sens-là seulement — la bonne pochette d'une autre édition
/// vaut mieux que pas de pochette, alors que l'inverse (des titres empruntés à
/// un autre pressage) afficherait des faux.
pub fn parse_lookup(json: &str, ntracks: usize) -> Option<DiscInfo> {
    let v: Value = serde_json::from_str(json).ok()?;
    let releases = v.get("releases")?.as_array()?;
    let mut meilleure: Option<(u8, DiscInfo)> = None;
    for release in releases {
        let Some(info) = depouille(release, ntracks) else { continue };
        let r = rang(face_avant(release));
        if r == 0 {
            return Some(info);
        }
        if meilleure.as_ref().is_none_or(|(vu, _)| r < *vu) {
            meilleure = Some((r, info));
        }
    }
    meilleure.map(|(_, info)| info)
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
    let Some(body) = requete_texte(&url_lookup(toc)).await? else { return Ok(None) };
    Ok(parse_lookup(&body, ntracks))
}

/// URL du lookup par TOC. Fonction à part, et testée : les `inc` décident de
/// ce que la réponse portera, donc de ce que l'analyse pourra en tirer, et un
/// `inc` perdu se traduirait par une perte de fonction silencieuse.
///
/// `release-groups` sert au repli de pochette quand la release n'a pas de face
/// avant typée (voir [`parse_lookup`]). Mesuré le 2026-08-26 : il est rendu sur
/// les 25 releases de la réponse sans aller-retour de plus — c'est ce qui rend
/// ce repli gratuit côté MusicBrainz.
///
/// La TOC n'est pas échappée : elle est validée chiffre par chiffre en amont
/// (`mb_toc_param`), donc ne contient que des nombres et des `+`.
fn url_lookup(toc: &str) -> String {
    format!(
        "https://musicbrainz.org/ws/2/discid/-?toc={toc}&fmt=json&inc=recordings+artist-credits+release-groups"
    )
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

/// URL de la face avant d'un **release-group** : la pochette de l'album, prise
/// sur l'un de ses pressages.
///
/// C'est le repli quand le pressage reconnu n'a pas de face avant typée. Deux
/// projets de référence font exactement cela, et ce n'est pas une coïncidence :
/// Picard — le tagueur de l'équipe MusicBrainz — en fait une option depuis sa
/// 1.3, et `beets` interroge les deux niveaux en marquant le second comme
/// repli. Aucun des deux ne devine sur une image non typée.
///
/// Mesuré le 2026-08-26 : sur le pressage 1997 de *Kind of Blue*, dont la
/// réponse annonce `front: false`, cette URL rend 200 et un JPEG de 50 220
/// octets — une vraie face avant, là où l'URL de la release rend 404.
///
/// L'image peut venir d'un **autre pressage** que celui reconnu. C'est assumé :
/// pour un appareil d'écoute, c'est la pochette de l'album, et mieux vaut la
/// bonne pochette d'une autre édition que pas de pochette du tout.
pub fn url_caa_groupe(rgid: &str) -> String {
    format!("https://coverartarchive.org/release-group/{rgid}/front-500")
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

    /// Reduction d'une capture reelle du lookup que ce module construit
    /// (2026-08-26, 25 releases rendues). Trois candidates conservees, dans un
    /// ordre qui reproduit exactement le piege : d'abord une a 3 pistes **sans**
    /// face avant — celle que retenait la version precedente — puis un leurre
    /// qui a une face avant mais 11 pistes, puis la bonne. Chaque release est
    /// reduite aux champs que l'analyse lit, plus son bloc `cover-art-archive`
    /// recopie tel quel.
    const FIXTURE_POCHETTES: &str = include_str!("../tests/fixtures/mb_discid_pochettes.json");

    #[test]
    fn la_face_avant_departage_les_candidates_recevables() {
        let info = parse_lookup(FIXTURE_POCHETTES, 3).unwrap();
        // Pas la premiere qui colle (Hellfire, `front: false`) : celle qui a
        // une image. Sans ce tri, le disque partait sans pochette alors qu'une
        // candidate recevable en portait une.
        assert_eq!(info.album, "Kiss You Off");
        assert_eq!(info.artist, "Scissor Sisters");
        // Face avant annoncee : l'URL vise le pressage, pas l'album.
        assert_eq!(
            info.cover_url.as_deref(),
            Some("https://coverartarchive.org/release/2de62a1b-0401-4569-bfe4-7bac2a61dea2/front-500")
        );
        // Le texte suit l'image : les deux viennent de la meme release, sans
        // quoi la pochette affichee ne correspondrait pas aux titres.
        assert_eq!(info.tracks[0], "Kiss You Off");
    }

    #[test]
    fn le_nombre_de_pistes_reste_le_filtre_et_une_image_ne_le_contourne_pas() {
        // Le leurre de la fixture (« Connectivity! ») a bien une face avant,
        // mais 11 pistes. La preferer serait annoncer un autre disque.
        let info = parse_lookup(FIXTURE_POCHETTES, 11).unwrap();
        assert_eq!(info.album, "Connectivity!");
        // Et pour 3 pistes, il ne doit jamais sortir.
        assert_eq!(parse_lookup(FIXTURE_POCHETTES, 3).unwrap().album, "Kiss You Off");
        // Un nombre de pistes qu'aucune candidate ne porte : rien.
        assert!(parse_lookup(FIXTURE_POCHETTES, 7).is_none());
    }

    #[test]
    fn sans_face_avant_la_pochette_de_lalbum_prend_le_relais() {
        // Mesure du 2026-08-26 : `/front-500` sur une release dont la reponse
        // dit `front: false` rend 404, meme avec quatre images — l'endpoint
        // suit le typage. Le repli est celui de Picard et de beets : la
        // pochette du release-group, qui est une vraie face avant typee.
        let sans = r#"{"releases":[
            {"id":"11111111-1111-1111-1111-111111111111","title":"Sans image","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":false,"count":4,"darkened":false},
             "release-group":{"id":"33333333-3333-3333-3333-333333333333"},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        let info = parse_lookup(sans, 1).unwrap();
        assert_eq!(info.album, "Sans image", "le texte vient toujours du pressage");
        assert_eq!(
            info.cover_url.as_deref(),
            Some("https://coverartarchive.org/release-group/33333333-3333-3333-3333-333333333333/front-500"),
            "l'image, elle, vient de l'album"
        );
    }

    #[test]
    fn sans_face_avant_ni_release_group_rien_nest_promis() {
        // Le seul cas qui reste muet. Annoncer l'URL du pressage ferait faire
        // au coeur une requete dont on sait deja qu'elle rendra 404.
        let rien = r#"{"releases":[
            {"id":"11111111-1111-1111-1111-111111111111","title":"Sans image","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":false,"count":0,"darkened":false},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        let info = parse_lookup(rien, 1).unwrap();
        assert_eq!(info.album, "Sans image", "le texte reste utile");
        assert_eq!(info.cover_url, None, "rien a demander a l'archive");
    }

    #[test]
    fn une_release_assombrie_se_rabat_comme_une_release_sans_face_avant() {
        // `darkened` : l'archive masque les images pour raison legale. Le
        // `front: true` qui l'accompagne ne veut alors plus rien dire, et
        // demander le pressage ne rendrait rien.
        let sombre = r#"{"releases":[
            {"id":"22222222-2222-2222-2222-222222222222","title":"Masquee","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":true,"count":4,"darkened":true},
             "release-group":{"id":"44444444-4444-4444-4444-444444444444"},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        assert_eq!(
            parse_lookup(sombre, 1).unwrap().cover_url.as_deref(),
            Some("https://coverartarchive.org/release-group/44444444-4444-4444-4444-444444444444/front-500")
        );
    }

    #[test]
    fn un_bloc_absent_ne_vaut_pas_absence_de_pochette() {
        // Garde-fou contre une regression silencieuse : traiter « la reponse
        // ne dit rien » comme « pas d'image » ferait taire la pochette sur
        // toute reponse ne portant pas ce bloc, pour un gain nul. La fixture
        // historique n'en a pas, et son URL doit continuer de viser le
        // pressage — le comportement d'avant ce chantier.
        assert!(!FIXTURE.contains("cover-art-archive"), "prealable du test");
        let url = parse_lookup(FIXTURE, 3).unwrap().cover_url.unwrap();
        assert!(url.starts_with("https://coverartarchive.org/release/"), "{url}");
    }

    #[test]
    fn une_face_avant_du_pressage_lemporte_sur_la_pochette_de_lalbum() {
        // Les deux candidates collent au nombre de pistes. La premiere n'a pas
        // de face avant mais a un album ; la seconde en a une. C'est la
        // seconde qu'il faut, parce que son image est celle de ce pressage-ci.
        let deux = r#"{"releases":[
            {"id":"11111111-1111-1111-1111-111111111111","title":"Sans","artist-credit":[{"name":"A"}],
             "cover-art-archive":{"front":false,"count":0,"darkened":false},
             "release-group":{"id":"33333333-3333-3333-3333-333333333333"},
             "media":[{"tracks":[{"title":"un"}]}]},
            {"id":"22222222-2222-2222-2222-222222222222","title":"Avec","artist-credit":[{"name":"B"}],
             "cover-art-archive":{"front":true,"count":1,"darkened":false},
             "release-group":{"id":"44444444-4444-4444-4444-444444444444"},
             "media":[{"tracks":[{"title":"un"}]}]}]}"#;
        let info = parse_lookup(deux, 1).unwrap();
        assert_eq!(info.album, "Avec");
        assert_eq!(
            info.cover_url.as_deref(),
            Some("https://coverartarchive.org/release/22222222-2222-2222-2222-222222222222/front-500")
        );
    }

    #[test]
    fn lurl_de_lalbum_suit_le_motif_mesure() {
        // Mesure du 2026-08-26 : 200 et un JPEG de 50 220 octets sur le
        // release-group de « Kind of Blue », la ou l'URL du pressage 1997 rend
        // 404.
        assert_eq!(
            url_caa_groupe("8e8a594f-2175-38c7-a871-abb68ec363e7"),
            "https://coverartarchive.org/release-group/8e8a594f-2175-38c7-a871-abb68ec363e7/front-500"
        );
    }

    #[test]
    fn parse_lookup_retient_le_mbid_pour_la_pochette() {
        // Le MBID est la clé de l'image, et il était jeté. La fixture réelle
        // du répertoire tests/fixtures est `mb_discid.json` (3 pistes).
        let url = parse_lookup(FIXTURE, 3).unwrap().cover_url.expect("une URL de pochette");
        let mbid = url
            .strip_prefix("https://coverartarchive.org/release/")
            .and_then(|r| r.strip_suffix("/front-500"))
            .unwrap_or("");
        assert_eq!(mbid.len(), 36, "un MBID fait 36 caracteres, obtenu {url:?}");
    }

    #[test]
    fn le_lookup_demande_le_release_group() {
        // Sans `release-groups` dans le `inc`, le repli de pochette n'a aucun
        // identifiant a viser et disparait en silence. Mesure du 2026-08-26 :
        // ce parametre est rendu sur les 25 releases sans aller-retour de plus.
        let url = url_lookup("1+3+63000+150+22767+41887");
        assert!(url.contains("inc=recordings+artist-credits+release-groups"), "{url}");
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
