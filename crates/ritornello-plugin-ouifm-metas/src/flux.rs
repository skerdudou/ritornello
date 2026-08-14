//! Consommation du `text/event-stream` de métadonnées d'OUI FM.
//!
//! Le découpage et l'analyse sont des fonctions pures, testées sur des trames
//! réelles ; seule `suit` touche le réseau, et **aucun test ne l'appelle**.

use anyhow::{bail, Result};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;

/// Ce qu'une trame apprend du morceau.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub duration_s: Option<u32>,
}

/// Attente initiale avant reconnexion, puis doublée à chaque échec.
const RECUL_BASE: Duration = Duration::from_secs(2);

/// Plafond du recul. Un appareil qui tourne des mois sans surveillance ne doit
/// pas marteler le serveur d'un tiers ; à l'inverse, plafonner évite qu'une
/// coupure réseau d'une nuit se traduise par des heures d'attente au retour.
const RECUL_MAX: Duration = Duration::from_secs(60);

/// Durée au-delà de laquelle une connexion est jugée saine, donc sa coupure
/// accidentelle : le recul repart alors de zéro.
///
/// Le critère est la **durée** et non le nombre de trames reçues. Le serveur en
/// pousse une dès la connexion, donc « au moins une trame » est toujours vrai et
/// ne distingue pas une écoute de quatre heures d'une fermeture immédiate. Avec
/// ce critère-là, le recul était remis à 2 s avant chaque attente, le plafond
/// était inatteignable, et un serveur qui pousse puis ferme aussitôt — cas
/// plausible sur un point d'entrée privé qui se doterait d'une protection
/// anti-abus — aurait fait ouvrir 43 000 requêtes par jour chez un tiers.
const DUREE_SAINE: Duration = Duration::from_secs(60);

/// Durée de silence au bout de laquelle on referme et rouvre.
///
/// `reqwest` détecte un pair disparu en une minute environ (keepalive TCP), mais
/// pas un pair vivant et muet — un mandataire figé tiendrait la connexion
/// indéfiniment sans rien envoyer, et l'affichage resterait figé avec lui. Dix
/// minutes laissent passer le plus long des morceaux sans reconnexion inutile.
const SILENCE_MAX: Duration = Duration::from_secs(600);

/// Extrait les lignes complètes du tampon, en le laissant contenir le reliquat.
///
/// Le tampon est en **octets** et non en texte : un chunk HTTP peut couper au
/// milieu d'un caractère accentué, et décoder chaque chunk séparément
/// remplacerait le « é » d'un nom d'artiste par un caractère de remplacement.
/// Ici, seule une ligne complète est décodée.
pub fn decoupe_lignes(tampon: &mut Vec<u8>) -> Vec<String> {
    let mut lignes = Vec::new();
    while let Some(i) = tampon.iter().position(|&b| b == b'\n') {
        let ligne: Vec<u8> = tampon.drain(..=i).collect();
        // Une ligne mal encodée est remplacée, pas jetée : mieux vaut un
        // caractère douteux qu'un titre absent.
        lignes.push(String::from_utf8_lossy(&ligne[..ligne.len() - 1]).trim_end().to_string());
    }
    lignes
}

/// Analyse une ligne du flux. `None` pour tout ce qui n'est pas une trame de
/// métadonnées exploitable : lignes de commentaire (`:`), champs `event:`/`id:`,
/// JSON illisible, ou trame sans artiste **ni** titre.
///
/// Les noms de champs sont ceux mesurés sur le flux réel : `artist` et `title`
/// déjà séparés (contrairement à l'ICY, qui livre une chaîne unique), plus
/// `durationInSeconds`.
pub fn parse_data_line(ligne: &str) -> Option<Meta> {
    let charge = ligne.strip_prefix("data:")?.trim();
    if charge.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(charge).ok()?;
    fn texte(v: &serde_json::Value, cle: &str) -> Option<String> {
        let s = v.get(cle)?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    }
    // `durationInSeconds` arrive en **chaîne** sur le flux réel (`"216"`), et
    // non en nombre. Mesuré : ne lire que les nombres faisait silencieusement
    // perdre la durée à chaque morceau. Les deux formes sont acceptées, un tiers
    // pouvant changer d'avis sans préavis.
    let duree = v.get("durationInSeconds").and_then(|d| match d {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    });
    let meta = Meta {
        artist: texte(&v, "artist"),
        title: texte(&v, "title"),
        // Une durée absurde vaut mieux ignorée qu'affichée : elle vient d'un tiers.
        duration_s: duree.filter(|d| *d > 0 && *d <= 24 * 3600).map(|d| d as u32),
    };
    // Une durée seule n'est pas affichable : ce n'est pas une réponse.
    (meta.artist.is_some() || meta.title.is_some()).then_some(meta)
}

/// URL du flux de métadonnées d'une webradio.
fn url_metas(id: &str) -> String {
    format!("https://www.ouifm.fr/ws/metas?id={id}")
}

/// Ouvre le flux et pousse chaque trame reçue. Renvoie le nombre de trames
/// lues avant la fin (0 = la connexion n'a rien donné).
async fn ecoute(id: &str, tx: &mpsc::Sender<(String, Meta)>) -> Result<usize> {
    let client = reqwest::Client::builder()
        .user_agent("ritornello/0.1 (https://github.com/skerdudou/ritornello)")
        // Délai de **connexion** seulement : le flux, lui, doit rester ouvert
        // indéfiniment, donc aucun timeout global.
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let resp = client.get(url_metas(id)).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let mut octets = resp.bytes_stream();
    let mut tampon: Vec<u8> = Vec::new();
    let mut recues = 0usize;
    loop {
        let Ok(suivant) = tokio::time::timeout(SILENCE_MAX, octets.next()).await else {
            bail!("no data for {} s", SILENCE_MAX.as_secs());
        };
        // Fin de flux propre : le serveur a fermé.
        let Some(chunk) = suivant else { break };
        tampon.extend_from_slice(&chunk?);
        // Garde-fou : un serveur qui n'enverrait jamais de fin de ligne ne doit
        // pas faire grossir ce tampon sans limite sur un appareil à 1 Go.
        if tampon.len() > 64 * 1024 {
            bail!("stream with no line ending, buffer dropped");
        }
        for ligne in decoupe_lignes(&mut tampon) {
            if let Some(meta) = parse_data_line(&ligne) {
                recues += 1;
                if tx.send((id.to_string(), meta)).await.is_err() {
                    // Le plugin ne nous écoute plus : la station a changé.
                    return Ok(recues);
                }
            }
        }
    }
    Ok(recues)
}

/// Prochain délai d'attente avant reconnexion, d'après le délai courant et la
/// durée qu'a tenu la connexion qui vient de se rompre.
///
/// Une connexion qui a tenu est jugée saine : sa coupure est accidentelle, on
/// repart vite (sans quoi une écoute de plusieurs heures finirait par attendre
/// une minute après chaque hoquet). Une connexion qui casse aussitôt fait
/// croître le recul jusqu'au plafond.
pub fn prochain_recul(recul: Duration, duree: Duration) -> Duration {
    if duree >= DUREE_SAINE {
        RECUL_BASE
    } else {
        (recul * 2).min(RECUL_MAX)
    }
}

/// Suit une webradio jusqu'à ce que la tâche soit abandonnée : ouvre le flux,
/// le relit après coupure avec un recul progressif.
///
/// Ne rend jamais la main. C'est l'appelant qui arrête cette tâche (`abort`)
/// quand ce qui joue change — d'où l'étiquetage de chaque trame par l'`id` : une
/// trame déjà en file au moment de l'arrêt doit pouvoir être écartée.
pub async fn suit(id: String, tx: mpsc::Sender<(String, Meta)>) {
    // Moitié de la base, parce que le recul est recalculé avant chaque
    // attente (voir plus bas) : le premier échec immédiat double cette valeur
    // et attend donc exactement `RECUL_BASE`, comme avant.
    let mut recul = RECUL_BASE / 2;
    loop {
        let debut = tokio::time::Instant::now();
        let resultat = ecoute(&id, &tx).await;
        let duree = debut.elapsed();
        // Toute fermeture est journalisée, y compris celle qui a servi des
        // trames : sans cela, une reconnexion en boucle ne laisserait aucune
        // trace dans `/api/logs` et personne ne verrait jamais rien.
        match resultat {
            Ok(recues) => {
                tracing::info!("metadata stream closed after {recues} frame(s) and {} s", duree.as_secs())
            }
            Err(e) => {
                tracing::info!("metadata stream interrupted after {} s: {e}", duree.as_secs())
            }
        }
        // Le recul est recalculé **avant** de dormir : la coupure qui vient
        // d'arriver dit tout ce qu'il faut savoir (une connexion qui a tenu
        // ramène à la base). L'ordre inverse appliquait l'ancien recul une
        // fois de trop — après une rafale d'échecs puis quatre heures
        // d'écoute saine, la première coupure attendait encore les 60 s
        // périmés — soit précisément le cas que la doc de `prochain_recul`
        // promet d'éviter.
        recul = prochain_recul(recul, duree);
        tokio::time::sleep(recul).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trame **capturée telle quelle** sur le flux d'OUI FM Classic Rock, jeton
    /// de pochette compris. À noter : `durationInSeconds` y est une **chaîne**.
    const TRAME: &str = r#"data: {"coverId":"3134161803443976427/t/th/therollingstones/shesarainbow/214198016_1702973462000","durationInSeconds":"245","artist":"THE ROLLING STONES","deezerId":"9956167","origin":"mds","appleMusicId":"1443171670","custom":"true","mdsId":"3134161803443976427","title":"SHE'S A RAINBOW","type":"song"}"#;

    #[test]
    fn analyse_une_trame_reelle() {
        let m = parse_data_line(TRAME).unwrap();
        assert_eq!(m.artist.as_deref(), Some("THE ROLLING STONES"));
        assert_eq!(m.title.as_deref(), Some("SHE'S A RAINBOW"));
        // La durée était perdue avant correction : le flux la donne en chaîne.
        assert_eq!(m.duration_s, Some(245));
    }

    #[test]
    fn la_duree_est_lue_en_chaine_comme_en_nombre() {
        let en_chaine = parse_data_line(r#"data: {"title":"t","durationInSeconds":"216"}"#).unwrap();
        assert_eq!(en_chaine.duration_s, Some(216));
        let en_nombre = parse_data_line(r#"data: {"title":"t","durationInSeconds":216}"#).unwrap();
        assert_eq!(en_nombre.duration_s, Some(216));
    }

    #[test]
    fn une_duree_absurde_est_ignoree_sans_perdre_le_titre() {
        for brut in ["0", "-5", "abc", "999999999", "\"\"", "null", "[]"] {
            let ligne = format!(r#"data: {{"title":"t","durationInSeconds":{brut}}}"#);
            // Guillemets deja presents pour les cas textuels.
            let ligne = if brut.starts_with('"') || brut == "null" || brut == "[]" {
                ligne
            } else {
                format!(r#"data: {{"title":"t","durationInSeconds":"{brut}"}}"#)
            };
            let m = parse_data_line(&ligne).unwrap_or_else(|| panic!("titre attendu pour {brut}"));
            assert_eq!(m.duration_s, None, "brut={brut}");
            assert_eq!(m.title.as_deref(), Some("t"));
        }
    }

    #[test]
    fn ignore_ce_qui_nest_pas_une_trame_exploitable() {
        assert!(parse_data_line(":ping").is_none(), "commentaire de maintien en vie");
        assert!(parse_data_line("event: message").is_none());
        assert!(parse_data_line("").is_none());
        assert!(parse_data_line("data:").is_none());
        assert!(parse_data_line("data: pas du json").is_none());
        // Ni artiste ni titre : rien à afficher, donc pas une réponse.
        assert!(parse_data_line(r#"data: {"durationInSeconds":10}"#).is_none());
        assert!(parse_data_line(r#"data: {"artist":"","title":"  "}"#).is_none());
    }

    #[test]
    fn accepte_une_trame_partielle() {
        // Décision du propriétaire : on affiche toute information disponible.
        let m = parse_data_line(r#"data: {"artist":"Téléphone"}"#).unwrap();
        assert_eq!(m.artist.as_deref(), Some("Téléphone"));
        assert_eq!(m.title, None);
    }

    #[test]
    fn decoupe_rend_les_lignes_completes_et_garde_le_reliquat() {
        let mut tampon = b"data: {\"a\":1}\ndata: {\"b\"".to_vec();
        let lignes = decoupe_lignes(&mut tampon);
        assert_eq!(lignes, vec!["data: {\"a\":1}".to_string()]);
        assert_eq!(tampon, b"data: {\"b\"".to_vec(), "le reliquat attend la suite");
    }

    #[test]
    fn un_caractere_accentue_coupe_entre_deux_chunks_reste_intact() {
        // « é » est sur deux octets en UTF-8. Décoder chaque chunk séparément
        // donnerait « T?l?phone » ; on ne décode donc qu'une ligne complète.
        let texte = "data: {\"artist\":\"Téléphone\"}\n";
        let octets = texte.as_bytes();
        let coupe = texte.find('é').unwrap() + 1; // au milieu du « é »
        let mut tampon = octets[..coupe].to_vec();
        assert!(decoupe_lignes(&mut tampon).is_empty(), "aucune ligne complete encore");
        tampon.extend_from_slice(&octets[coupe..]);
        let lignes = decoupe_lignes(&mut tampon);
        let m = parse_data_line(&lignes[0]).unwrap();
        assert_eq!(m.artist.as_deref(), Some("Téléphone"));
    }

    #[test]
    fn plusieurs_lignes_dun_seul_chunk_sont_toutes_rendues() {
        let mut tampon = b"data: {\"title\":\"un\"}\n\ndata: {\"title\":\"deux\"}\n".to_vec();
        let lignes = decoupe_lignes(&mut tampon);
        assert_eq!(lignes.len(), 3, "deux trames et la ligne vide de separation");
        let titres: Vec<String> =
            lignes.iter().filter_map(|l| parse_data_line(l)).filter_map(|m| m.title).collect();
        assert_eq!(titres, vec!["un".to_string(), "deux".to_string()]);
    }

    #[test]
    fn lurl_de_metas_porte_lidentifiant() {
        assert_eq!(url_metas("42"), "https://www.ouifm.fr/ws/metas?id=42");
    }

    /// Durée typique d'une connexion qui casse aussitôt.
    const IMMEDIATE: Duration = Duration::from_millis(80);

    #[test]
    fn une_connexion_qui_casse_aussitot_fait_croitre_le_recul_jusquau_plafond() {
        let mut recul = RECUL_BASE;
        let mut vus = vec![recul];
        for _ in 0..10 {
            recul = prochain_recul(recul, IMMEDIATE);
            vus.push(recul);
        }
        assert_eq!(vus[0], Duration::from_secs(2));
        assert_eq!(vus[1], Duration::from_secs(4));
        assert_eq!(vus[2], Duration::from_secs(8));
        assert_eq!(*vus.last().unwrap(), RECUL_MAX, "le plafond doit etre atteint");
        assert!(vus.windows(2).all(|p| p[1] >= p[0]), "jamais decroissant");
    }

    #[test]
    fn une_connexion_qui_a_tenu_remet_le_recul_a_zero() {
        assert_eq!(prochain_recul(RECUL_MAX, DUREE_SAINE), RECUL_BASE);
        assert_eq!(prochain_recul(RECUL_MAX, Duration::from_secs(4 * 3600)), RECUL_BASE);
    }

    #[test]
    fn une_trame_recue_ne_suffit_pas_a_remettre_le_recul_a_zero() {
        // C'est le défaut que ce découpage corrige : le serveur pousse une trame
        // **dès la connexion**, donc « au moins une trame reçue » est toujours
        // vrai. En s'y fiant, le recul repartait de 2 s à chaque tour, le plafond
        // était inatteignable, et un serveur qui pousse puis ferme aussitôt
        // faisait ouvrir une requête toutes les 2 s indéfiniment chez un tiers.
        // Ici, une connexion d'une demi-seconde — le temps d'une trame — laisse
        // le recul croître.
        let apres_une_trame = prochain_recul(Duration::from_secs(8), Duration::from_millis(500));
        assert_eq!(apres_une_trame, Duration::from_secs(16));
    }

    #[test]
    fn le_seuil_de_sante_est_franc() {
        // Juste en dessous du seuil : le recul croît encore.
        assert_eq!(
            prochain_recul(Duration::from_secs(2), DUREE_SAINE - Duration::from_millis(1)),
            Duration::from_secs(4)
        );
    }
}
