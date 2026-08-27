//! Le découpage d'une chaîne ICY : deux fonctions pures, aucun réseau, aucun
//! état. C'est le seul endroit où l'on décide **comment** couper ; la décision
//! de savoir *quel* découpage est le bon appartient à la validation.

/// Séparateurs reconnus, par ordre de priorité — c'est aussi l'ordre dans
/// lequel les candidats sont sondés.
///
/// `" - "` d'abord : c'est la convention de fait du champ `StreamTitle`, le
/// défaut de la plupart des automates de diffusion. Les espaces autour font
/// partie du motif, et ce n'est pas un détail : sans eux, `Jean-Michel Jarre`
/// se ferait couper en deux.
pub const SEPARATEURS: [&str; 5] = [" - ", " – ", " — ", " / ", " : "];

/// Plafond de candidats sondés pour une station.
///
/// Chaque candidat coûte une requête, espacée d'`INTERVALLE_MIN` : quatre font
/// un sondage de quatre secondes, une fois par station, que personne n'attend.
/// Sans plafond, une chaîne portant plusieurs types de séparateurs en
/// produirait dix.
pub const MAX_CANDIDATS: usize = 4;

/// Un découpage possible, et de quoi le rejouer plus tard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidat {
    pub artiste: String,
    pub titre: String,
    pub separateur: &'static str,
    pub artiste_en_premier: bool,
    /// Le titre est le champ du **milieu** — la forme `Artiste - Titre - Album`.
    ///
    /// Ce drapeau existe parce que `Motif` doit pouvoir **rejouer** ce candidat.
    /// Une première version ne le portait pas, et le défaut était une boucle
    /// infinie plutôt qu'un simple titre faux : le candidat du milieu validait,
    /// le motif enregistré ne retenait que le séparateur et l'ordre, donc
    /// `applique` recollait l'album au titre au morceau suivant, la validation
    /// échouait, trois échecs déclenchaient un resondage, le même candidat
    /// validait de nouveau — et ainsi de suite pour toujours.
    pub titre_au_milieu: bool,
}

/// Retire le bruit qu'une station accole à ce qu'elle annonce.
///
/// **Avant** tout découpage, et c'est l'ordre qui compte : une station qui
/// accole sa réclame ferait échouer *tous* les candidats, donc serait classée
/// « ne pas découper » — c'est-à-dire définitivement, puisque rien ne resonde
/// une station ainsi classée.
///
/// Délibérément **conservateur**. Trois formes seulement, celles qui ne peuvent
/// pas appartenir à un titre :
///
/// * tout ce qui suit une barre verticale — elle n'apparaît pas dans un titre ;
/// * un groupe entre crochets en fin de chaîne (durées, marqueurs de régie) ;
/// * les espaces de bord et les espaces répétés.
///
/// Ce qu'on ne fait **pas**, et pourquoi : retirer un suffixe du genre
/// `" - Radio X"` serait indistinguable d'un vrai séparateur, donc casserait
/// autant de stations qu'il en réparerait. Et les parenthèses restent : `(Radio
/// Edit)`, `(Live)`, `(feat. …)` font partie du titre, et les retirer
/// empêcherait la validation au lieu de l'aider. Une station que ce nettoyage
/// ne suffit pas à traiter finira en « ne pas découper », et la page d'admin
/// est le remède prévu pour elle.
pub fn nettoie(brut: &str) -> String {
    let sans_barre = brut.split('|').next().unwrap_or(brut);
    let mut s = sans_barre.trim();
    // Un seul groupe retiré, en fin de chaîne : boucler couperait un titre qui
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
/// Les candidats se dérivent de la chaîne et non d'une liste fixe : on ne
/// construit que pour les séparateurs réellement présents. Une chaîne n'en
/// contient qu'un type en pratique, donc deux candidats — les deux ordres —, et
/// le plafond ne mord que sur les chaînes bavardes.
///
/// Pour un séparateur présent au moins deux fois — la forme
/// `Artiste - Titre - Album`, réelle — un troisième candidat prend le champ du
/// **milieu** comme titre. Sans lui, le titre porterait l'album collé et ne
/// validerait jamais.
///
/// Une moitié vide ne produit pas de candidat : une requête avec un champ vide
/// est une requête pour rien.
pub fn candidats(nettoye: &str) -> Vec<Candidat> {
    let mut out: Vec<Candidat> = Vec::new();
    for separateur in SEPARATEURS {
        let parts: Vec<&str> = nettoye.split(separateur).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }
        let tete = parts[0];
        let reste = parts[1..].join(separateur);
        let mut pousse =
            |artiste: &str, titre: &str, artiste_en_premier: bool, titre_au_milieu: bool| {
                if artiste.is_empty() || titre.is_empty() || out.len() >= MAX_CANDIDATS {
                    return;
                }
                out.push(Candidat {
                    artiste: artiste.to_string(),
                    titre: titre.to_string(),
                    separateur,
                    artiste_en_premier,
                    titre_au_milieu,
                });
            };
        pousse(tete, &reste, true, false);
        pousse(&reste, tete, false, false);
        if parts.len() >= 3 {
            pousse(tete, parts[1], true, true);
        }
    }
    out
}

/// Rejoue un motif appris sur une chaîne nettoyée.
///
/// **Aucun réseau** : c'est là tout l'intérêt du souvenir. Une fois le motif
/// d'une station connu, séparer artiste et titre est une opération locale, et
/// seule la pochette demande encore une requête.
///
/// `None` quand le motif ne s'applique pas : la chaîne ne porte pas ce
/// séparateur, une moitié est vide, ou le motif est `NePasDecouper`. Ce `None`
/// **est** l'échec de validation dont parle la règle des trois échecs
/// consécutifs — pas une erreur, un morceau qui ne rentre pas dans la forme.
pub fn applique(motif: &crate::motifs::Motif, nettoye: &str) -> Option<(String, String)> {
    let crate::motifs::Motif::Separe { separateur, artiste_en_premier, titre_au_milieu } = motif
    else {
        return None;
    };
    // `titre_au_milieu` : la forme `Artiste - Titre - Album`, où le titre est le
    // **deuxième** champ et le reste est ignoré. Sans cette branche, le motif
    // appris d'un candidat du milieu recollait l'album au titre — et comme la
    // validation échouait alors à chaque morceau, la station se faisait
    // resonder sans fin. Voir `Candidat::titre_au_milieu`.
    if *titre_au_milieu {
        let parts: Vec<&str> = nettoye.split(separateur.as_str()).map(str::trim).collect();
        let (artiste, titre) = (parts.first()?, parts.get(1)?);
        if artiste.is_empty() || titre.is_empty() {
            return None;
        }
        return Some((artiste.to_string(), titre.to_string()));
    }
    let (tete, reste) = nettoye.split_once(separateur.as_str())?;
    let (tete, reste) = (tete.trim(), reste.trim());
    if tete.is_empty() || reste.is_empty() {
        return None;
    }
    Some(if *artiste_en_premier {
        (tete.to_string(), reste.to_string())
    } else {
        (reste.to_string(), tete.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motifs::Motif;

    #[test]
    fn le_nettoyage_retire_la_reclame_apres_une_barre() {
        assert_eq!(nettoie("Miles Davis - So What | Radio X"), "Miles Davis - So What");
        assert_eq!(nettoie("  Miles Davis - So What  "), "Miles Davis - So What");
    }

    #[test]
    fn le_nettoyage_retire_une_duree_entre_crochets_en_fin() {
        assert_eq!(nettoie("Miles Davis - So What [00:09:22]"), "Miles Davis - So What");
    }

    #[test]
    fn le_nettoyage_garde_une_parenthese_qui_fait_partie_du_titre() {
        // `(Radio Edit)`, `(Live)`, `(feat. X)` appartiennent au titre. Les
        // retirer casserait la validation au lieu de l'aider.
        let s = "Daft Punk - Around the World (Radio Edit)";
        assert_eq!(nettoie(s), s);
    }

    #[test]
    fn le_nettoyage_precede_le_decoupage_donc_la_reclame_ne_casse_pas_les_candidats() {
        // La régression que cette étape existe pour empêcher : sans nettoyage,
        // le titre du dernier candidat porte « | Radio X », aucun candidat ne
        // valide, et la station est classée « ne pas découper » — c'est-à-dire
        // définitivement, puisque rien ne resonde une station ainsi classée.
        let c = candidats(&nettoie("Miles Davis - So What | Radio X"));
        assert!(
            c.iter().any(|c| c.artiste == "Miles Davis" && c.titre == "So What"),
            "candidats obtenus : {c:?}"
        );
    }

    #[test]
    fn deux_candidats_pour_un_seul_separateur_les_deux_ordres() {
        let c = candidats("Miles Davis - So What");
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].artiste.as_str(), c[0].titre.as_str()), ("Miles Davis", "So What"));
        assert!(c[0].artiste_en_premier, "le standard passe en premier");
        assert_eq!((c[1].artiste.as_str(), c[1].titre.as_str()), ("So What", "Miles Davis"));
        assert!(!c[1].artiste_en_premier);
    }

    #[test]
    fn le_demi_cadratin_est_reconnu_comme_separateur() {
        let c = candidats("Miles Davis – So What");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].separateur, " – ");
    }

    #[test]
    fn trois_champs_donnent_aussi_le_candidat_du_milieu() {
        // La forme `Artiste - Titre - Album`, réelle. Sans ce candidat, le
        // titre porterait « So What - Kind of Blue » et ne validerait jamais.
        let c = candidats("Miles Davis - So What - Kind of Blue");
        assert!(
            c.iter().any(|c| c.artiste == "Miles Davis" && c.titre == "So What"),
            "candidats obtenus : {c:?}"
        );
    }

    #[test]
    fn le_plafond_de_candidats_est_tenu() {
        // Plusieurs types de séparateurs dans la même chaîne : le plafond doit
        // mordre, sinon un sondage part en dix requêtes.
        let c = candidats("A - B / C : D – E");
        assert!(c.len() <= MAX_CANDIDATS, "{} candidats", c.len());
    }

    #[test]
    fn sans_separateur_il_ny_a_aucun_candidat() {
        // Un slogan, un nom d'émission : rien à contraindre côté artiste, donc
        // rien de validable. L'appelant en conclut « ne pas découper ».
        assert!(candidats("Vous ecoutez Radio X").is_empty());
        assert!(candidats("").is_empty());
    }

    #[test]
    fn une_moitie_vide_ne_produit_pas_de_candidat() {
        // Une requête avec un champ vide est une requête pour rien.
        //
        // **Les espaces de bord sont l'essentiel de ces deux fixtures**, et une
        // première version les avait oubliés (`"- So What"`, `"Miles Davis -"`) :
        // le séparateur étant `" - "`, ces chaînes n'en contenaient aucun, la
        // garde n'était jamais atteinte, et le test passait pour une mauvaise
        // raison — retirer la garde ne le faisait pas tomber. Trouvé par la
        // preuve par mutation, qui est faite pour ça.
        //
        // Ces deux formes ne sortent **pas** de `nettoie`, qui coupe les bords :
        // ce test éprouve donc le contrat de `candidats`, qui est une fonction
        // publique et ne doit pas dépendre de qui l'appelle, et non un chemin de
        // production. La distinction vaut d'être sue avant d'y toucher.
        assert!(candidats(" - So What").is_empty(), "artiste vide");
        assert!(candidats("Miles Davis - ").is_empty(), "titre vide");
    }

    #[test]
    fn appliquer_un_motif_redonne_le_couple() {
        let m =
            Motif::Separe { separateur: " - ".into(), artiste_en_premier: false, titre_au_milieu: false };
        assert_eq!(
            applique(&m, "So What - Miles Davis"),
            Some(("Miles Davis".to_string(), "So What".to_string())),
            "ordre inverse : l'artiste est en second"
        );
    }

    /// **La propriété qui relie les deux moitiés du chantier**, et que rien ne
    /// prouvait : rejouer le motif d'un candidat sur la chaîne dont il est issu
    /// doit redonner ce candidat, à l'identique.
    ///
    /// Son intérêt n'est pas théorique. Le sondage retient le candidat que
    /// MusicBrainz a validé, puis tous les morceaux suivants sont découpés par
    /// `applique` sans plus aucune requête. Si les deux fonctions divergeaient
    /// sur une forme quelconque, l'appareil afficherait un artiste et un titre
    /// **faux après un sondage réussi** — la pire des combinaisons, puisque la
    /// validation a bien eu lieu et que rien au journal ne le signalerait.
    ///
    /// Éprouvée sur toutes les formes que les autres tests traitent une à une,
    /// **plus** celles qui les combinent : plusieurs séparateurs dans la même
    /// chaîne, trois champs, séparateurs rares, et un nom composé.
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
            let nettoye = nettoie(forme);
            let cands = candidats(&nettoye);
            assert!(!cands.is_empty(), "« {forme} » doit produire au moins un candidat");
            for c in cands {
                let motif = crate::motifs::Motif::depuis_candidat(&c);
                assert_eq!(
                    applique(&motif, &nettoye),
                    Some((c.artiste.clone(), c.titre.clone())),
                    "motif {motif:?} rejoue sur « {nettoye} » doit redonner {c:?}"
                );
            }
        }
    }

    #[test]
    fn appliquer_un_motif_absent_de_la_chaine_rend_none() {
        // Le morceau où la station change de forme : pas un couple bancal,
        // rien du tout. C'est ce `None` qui compte comme échec de validation.
        let m =
            Motif::Separe { separateur: " - ".into(), artiste_en_premier: true, titre_au_milieu: false };
        assert_eq!(applique(&m, "Vous ecoutez Radio X"), None);
    }

    #[test]
    fn ne_pas_decouper_ne_produit_jamais_de_couple() {
        assert_eq!(applique(&Motif::NePasDecouper, "Miles Davis - So What"), None);
    }
}
