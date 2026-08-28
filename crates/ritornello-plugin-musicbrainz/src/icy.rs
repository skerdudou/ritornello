//! Le découpage d'une chaîne ICY : deux fonctions pures, aucun réseau, aucun
//! état. C'est le seul endroit où l'on décide **comment** couper ; la décision
//! de savoir *quel* découpage est le bon appartient à la validation.

/// Séparateurs reconnus, par order de priorité — c'est aussi l'order dans
/// lequel les candidates sont sondés.
///
/// `" - "` d'abord : c'est la convention de fait du champ `StreamTitle`, le
/// défaut de la plupart des automates de diffusion. Les espaces autour font
/// partie du pattern, et ce n'est pas un détail : sans eux, `Jean-Michel Jarre`
/// se ferait couper en deux.
pub const SEPARATORS: [&str; 5] = [" - ", " – ", " — ", " / ", " : "];

/// Plafond de candidates sondés pour une station.
///
/// Chaque candidat coûte une requête, espacée d'`MIN_INTERVAL` : quatre font
/// un sondage de quatre secondes, une fois par station, que personne n'wait.
/// Sans cap, une chaîne portant plusieurs types de séparateurs en
/// produirait dix.
pub const MAX_CANDIDATES: usize = 4;

/// Un découpage possible, et de quoi le rejouer plus tard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub artist: String,
    pub title: String,
    pub separator: &'static str,
    pub artist_first: bool,
    /// Le title est le champ du **milieu** — la forme `Artiste - Titre - Album`.
    ///
    /// Ce drapeau existe parce que `Pattern` doit pouvoir **rejouer** ce candidat.
    /// Une première version ne le portait pas, et le défaut était une boucle
    /// infinie plutôt qu'un simple title faux : le candidat du milieu validait,
    /// le pattern enregistré ne retenait que le séparateur et l'order, donc
    /// `apply` recollait l'album au title au track suivant, la validation
    /// échouait, trois échecs déclenchaient un resondage, le même candidat
    /// validait de nouveau — et ainsi de suite pour toujours.
    pub title_in_middle: bool,
}

/// Retire le bruit qu'une station accole à ce qu'elle announcement.
///
/// **Avant** tout découpage, et c'est l'order qui compte : une station qui
/// accole sa réclame ferait échouer *tous* les candidates, donc serait classée
/// « ne pas découper » — c'est-à-dire définitivement, puisque rien ne reprobe
/// une station ainsi classée.
///
/// Délibérément **conservateur**. Trois formes seulement, celles qui ne peuvent
/// pas appartenir à un title :
///
/// * tout ce qui suit une barre verticale — elle n'apparaît pas dans un title ;
/// * un groupe entre crochets en fin de chaîne (durées, marqueurs de régie) ;
/// * les espaces de bord et les espaces répétés.
///
/// Ce qu'on ne fait **pas**, et pourquoi : retirer un suffixe du kind
/// `" - Radio X"` serait indistinguable d'un vrai séparateur, donc casserait
/// autant de stations qu'il en réparerait. Et les parenthèses restent : `(Radio
/// Edit)`, `(Live)`, `(feat. …)` font partie du title, et les retirer
/// empêcherait la validation au lieu de l'aider. Une station que ce nettoyage
/// ne suffit pas à traiter finira en « ne pas découper », et la page d'admin
/// est le remède prévu pour elle.
pub fn clean(raw: &str) -> String {
    let sans_barre = raw.split('|').next().unwrap_or(raw);
    let mut s = sans_barre.trim();
    // Un seul groupe retiré, en fin de chaîne : boucler couperait un title qui
    // finirait légitimement par des crochets.
    if let Some(ouvrant) = s.rfind('[') {
        if s.trim_end().ends_with(']') {
            s = s[..ouvrant].trim_end();
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Dérive les découpages plausibles de la chaîne **nettoyée**.
///
/// Les candidates se dérivent de la chaîne et non d'une liste fixe : on ne
/// construit que pour les séparateurs réellement présents. Une chaîne n'en
/// contains qu'un type en pratique, donc deux candidates — les deux ordres —, et
/// le cap ne mord que sur les chaînes bavardes.
///
/// Pour un séparateur présent au moins deux fois — la forme
/// `Artiste - Titre - Album`, réelle — un troisième candidat prend le champ du
/// **milieu** comme title. Sans lui, le title porterait l'album collé et ne
/// validerait jamais.
///
/// Une moitié clear ne produit pas de candidat : une requête avec un champ clear
/// est une requête pour rien.
pub fn candidates(nettoye: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for separator in SEPARATORS {
        let parts: Vec<&str> = nettoye.split(separator).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }
        let tete = parts[0];
        let reste = parts[1..].join(separator);
        let mut push_cover =
            |artist: &str, title: &str, artist_first: bool, title_in_middle: bool| {
                if artist.is_empty() || title.is_empty() || out.len() >= MAX_CANDIDATES {
                    return;
                }
                out.push(Candidate {
                    artist: artist.to_string(),
                    title: title.to_string(),
                    separator,
                    artist_first,
                    title_in_middle,
                });
            };
        push_cover(tete, &reste, true, false);
        push_cover(&reste, tete, false, false);
        if parts.len() >= 3 {
            push_cover(tete, parts[1], true, true);
        }
    }
    out
}

/// Rejoue un pattern appris sur une chaîne nettoyée.
///
/// **Aucun réseau** : c'est là tout l'intérêt du souvenir. Une fois le pattern
/// d'une station known, séparer artist et title est une opération locale, et
/// seule la cover demande encore une requête.
///
/// `None` quand le pattern ne s'apply pas : la chaîne ne porte pas ce
/// séparateur, une moitié est clear, ou le pattern est `DoNotSplit`. Ce `None`
/// **est** l'échec de validation dont parle la règle des trois échecs
/// consécutifs — pas une erreur, un track qui ne rentre pas dans la forme.
pub fn apply(pattern: &crate::patterns::Pattern, nettoye: &str) -> Option<(String, String)> {
    let crate::patterns::Pattern::Split { separator, artist_first, title_in_middle } = pattern
    else {
        return None;
    };
    // `title_in_middle` : la forme `Artiste - Titre - Album`, où le title est le
    // **deuxième** champ et le reste est ignoré. Sans cette branche, le pattern
    // appris d'un candidat du milieu recollait l'album au title — et comme la
    // validation échouait alors à chaque track, la station se faisait
    // resonder sans fin. Voir `Candidate::title_in_middle`.
    if *title_in_middle {
        let parts: Vec<&str> = nettoye.split(separator.as_str()).map(str::trim).collect();
        let (artist, title) = (parts.first()?, parts.get(1)?);
        if artist.is_empty() || title.is_empty() {
            return None;
        }
        return Some((artist.to_string(), title.to_string()));
    }
    let (tete, reste) = nettoye.split_once(separator.as_str())?;
    let (tete, reste) = (tete.trim(), reste.trim());
    if tete.is_empty() || reste.is_empty() {
        return None;
    }
    Some(if *artist_first {
        (tete.to_string(), reste.to_string())
    } else {
        (reste.to_string(), tete.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::Pattern;

    #[test]
    fn le_nettoyage_retire_la_reclame_apres_une_barre() {
        assert_eq!(clean("Miles Davis - So What | Radio X"), "Miles Davis - So What");
        assert_eq!(clean("  Miles Davis - So What  "), "Miles Davis - So What");
    }

    #[test]
    fn le_nettoyage_retire_une_duree_entre_crochets_en_fin() {
        assert_eq!(clean("Miles Davis - So What [00:09:22]"), "Miles Davis - So What");
    }

    #[test]
    fn le_nettoyage_garde_une_parenthese_qui_fait_partie_du_titre() {
        // `(Radio Edit)`, `(Live)`, `(feat. X)` appartiennent au title. Les
        // retirer casserait la validation au lieu de l'aider.
        let s = "Daft Punk - Around the World (Radio Edit)";
        assert_eq!(clean(s), s);
    }

    #[test]
    fn le_nettoyage_precede_le_decoupage_donc_la_reclame_ne_casse_pas_les_candidats() {
        // La régression que cette étape existe pour empêcher : sans nettoyage,
        // le title du dernier candidat porte « | Radio X », aucun candidat ne
        // validated, et la station est classée « ne pas découper » — c'est-à-dire
        // définitivement, puisque rien ne reprobe une station ainsi classée.
        let c = candidates(&clean("Miles Davis - So What | Radio X"));
        assert!(
            c.iter().any(|c| c.artist == "Miles Davis" && c.title == "So What"),
            "candidates obtenus : {c:?}"
        );
    }

    #[test]
    fn deux_candidats_pour_un_seul_separateur_les_deux_ordres() {
        let c = candidates("Miles Davis - So What");
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].artist.as_str(), c[0].title.as_str()), ("Miles Davis", "So What"));
        assert!(c[0].artist_first, "le standard passe en premier");
        assert_eq!((c[1].artist.as_str(), c[1].title.as_str()), ("So What", "Miles Davis"));
        assert!(!c[1].artist_first);
    }

    #[test]
    fn le_demi_cadratin_est_reconnu_comme_separateur() {
        let c = candidates("Miles Davis – So What");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].separator, " – ");
    }

    #[test]
    fn trois_champs_donnent_aussi_le_candidat_du_milieu() {
        // La forme `Artiste - Titre - Album`, réelle. Sans ce candidat, le
        // title porterait « So What - Kind of Blue » et ne validerait jamais.
        let c = candidates("Miles Davis - So What - Kind of Blue");
        assert!(
            c.iter().any(|c| c.artist == "Miles Davis" && c.title == "So What"),
            "candidates obtenus : {c:?}"
        );
    }

    #[test]
    fn le_plafond_de_candidats_est_tenu() {
        // Plusieurs types de séparateurs dans la même chaîne : le cap doit
        // mordre, sinon un sondage part en dix requêtes.
        let c = candidates("A - B / C : D – E");
        assert!(c.len() <= MAX_CANDIDATES, "{} candidates", c.len());
    }

    #[test]
    fn sans_separateur_il_ny_a_aucun_candidat() {
        // Un slogan, un name d'émission : rien à contraindre côté artist, donc
        // rien de validable. L'appelant en conclut « ne pas découper ».
        assert!(candidates("Vous ecoutez Radio X").is_empty());
        assert!(candidates("").is_empty());
    }

    #[test]
    fn une_moitie_vide_ne_produit_pas_de_candidat() {
        // Une requête avec un champ clear est une requête pour rien.
        //
        // **Les espaces de bord sont l'essentiel de ces deux fixtures**, et une
        // première version les avait oubliés (`"- So What"`, `"Miles Davis -"`) :
        // le séparateur étant `" - "`, ces chaînes n'en contenaient aucun, la
        // garde n'était jamais atteinte, et le test passait pour une mauvaise
        // reason — retirer la garde ne le faisait pas tomber. Trouvé par la
        // preuve par mutation, qui est faite pour ça.
        //
        // Ces deux formes ne sortent **pas** de `clean`, qui coupe les bords :
        // ce test éprouve donc le contrat de `candidates`, qui est une fonction
        // publique et ne doit pas dépendre de qui l'appelle, et non un path de
        // production. La distinction vaut d'être sue avant d'y toucher.
        assert!(candidates(" - So What").is_empty(), "artist clear");
        assert!(candidates("Miles Davis - ").is_empty(), "title clear");
    }

    #[test]
    fn appliquer_un_motif_redonne_le_couple() {
        let m =
            Pattern::Split { separator: " - ".into(), artist_first: false, title_in_middle: false };
        assert_eq!(
            apply(&m, "So What - Miles Davis"),
            Some(("Miles Davis".to_string(), "So What".to_string())),
            "order inverse : l'artist est en second"
        );
    }

    /// **La propriété qui relie les deux moitiés du chantier**, et que rien ne
    /// prouvait : rejouer le pattern d'un candidat sur la chaîne dont il est issu
    /// doit redonner ce candidat, à l'identique.
    ///
    /// Son intérêt n'est pas théorique. Le sondage retient le candidat que
    /// MusicBrainz a validé, puis tous les morceaux suivants sont découpés par
    /// `apply` sans plus aucune requête. Si les deux fonctions divergeaient
    /// sur une forme quelconque, l'appareil afficherait un artist et un title
    /// **faux après un sondage réussi** — la pire des combinaisons, puisque la
    /// validation a bien eu lieu et que rien au journal ne le signalerait.
    ///
    /// Éprouvée sur toutes les formes que les autres tests traitent une à une,
    /// **plus** celles qui les combinent : plusieurs séparateurs dans la même
    /// chaîne, trois champs, séparateurs rares, et un name composé.
    #[test]
    fn appliquer_le_motif_dun_candidat_redonne_ce_candidat() {
        let formes = [
            "Miles Davis - So What",
            "So What - Miles Davis",
            "Miles Davis – So What",
            "Miles Davis — So What",
            "Miles Davis / So What",
            "Miles Davis : So What",
            "Miles Davis - So What - Kind of Blue",
            "A - B / C : D – E",
            "Daft Punk - Around the World (Radio Edit)",
            "Jean-Michel Jarre - Oxygene Pt. 4",
        ];
        for forme in formes {
            let nettoye = clean(forme);
            let cands = candidates(&nettoye);
            assert!(!cands.is_empty(), "« {forme} » doit produire au moins un candidat");
            for c in cands {
                let pattern = crate::patterns::Pattern::from_candidate(&c);
                assert_eq!(
                    apply(&pattern, &nettoye),
                    Some((c.artist.clone(), c.title.clone())),
                    "pattern {pattern:?} rejoue sur « {nettoye} » doit redonner {c:?}"
                );
            }
        }
    }

    #[test]
    fn appliquer_un_motif_absent_de_la_chaine_rend_none() {
        // Le track où la station change de forme : pas un pair bancal,
        // rien du tout. C'est ce `None` qui compte comme échec de validation.
        let m =
            Pattern::Split { separator: " - ".into(), artist_first: true, title_in_middle: false };
        assert_eq!(apply(&m, "Vous ecoutez Radio X"), None);
    }

    #[test]
    fn ne_pas_decouper_ne_produit_jamais_de_couple() {
        assert_eq!(apply(&Pattern::DoNotSplit, "Miles Davis - So What"), None);
    }
}
