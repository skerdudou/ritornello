//! Interrogation du direct d'une station Radio France.
//!
//! L'analyse est une fonction pure, testée sur des réponses réelles ; seule
//! `suit` touche le réseau, et **aucun test ne l'appelle**.
//!
//! Contrairement à OUI FM, qui pousse ses métadonnées dans un
//! `text/event-stream`, Radio France répond à une interrogation ponctuelle —
//! mais en disant lui-même quand le rappeler (`delayToRefresh`). Le rythme
//! d'interrogation est donc dicté par le serveur, pas par nous : c'est ce qui
//! permet de suivre un morceau de trois minutes sans marteler un tiers, et de
//! laisser une tranche d'antenne d'une heure tranquille.

use anyhow::{bail, Result};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

/// Ce qu'une réponse apprend du direct.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
    /// Début du morceau, en secondes depuis l'époque Unix, tel que le direct
    /// l'annonce. Brut : c'est l'émission de l'enrichissement qui en déduit
    /// l'écoulé, pour que ce module reste sans horloge et testable sur des
    /// captures.
    pub start_time: Option<u64>,
}

/// Une réponse lue : ce qui passe, et dans combien de temps rappeler.
#[derive(Debug, Clone, PartialEq)]
pub struct Direct {
    /// `None` quand la réponse ne porte ni titre ni artiste — cas d'un
    /// basculement d'antenne. Le délai, lui, reste exploitable.
    pub meta: Option<Meta>,
    /// Identifiant du morceau en cours, quand il y en a un. Il n'est jamais
    /// affiché : il sert à retrouver l'album dans la grille (voir
    /// `album_dans_grille`), qui est la seule façon de l'obtenir — le direct
    /// ne porte pas d'album.
    pub song_uuid: Option<String>,
    pub recontacter: Duration,
}

/// Attente initiale avant nouvelle tentative après échec, puis doublée.
const RECUL_BASE: Duration = Duration::from_secs(2);

/// Plafond du recul. Un appareil qui tourne des mois sans surveillance ne doit
/// pas marteler le serveur d'un tiers ; à l'inverse, plafonner évite qu'une
/// coupure réseau d'une nuit se traduise par des heures d'attente au retour.
const RECUL_MAX: Duration = Duration::from_secs(60);

/// Plancher du délai annoncé par le serveur. Mesuré : il descend à 10 s sur
/// les stations qui basculent souvent. Ce plancher n'existe donc pas pour
/// corriger le serveur mais pour borner ce qu'une réponse aberrante — ou un
/// mandataire qui réécrirait le JSON — pourrait nous faire faire.
const RAPPEL_MIN: Duration = Duration::from_secs(5);

/// Plafond du délai annoncé. Mesuré : les locales annoncent jusqu'à 51 min,
/// soit la fin de la tranche en cours. Les croire sur parole laisserait
/// l'affichage figé aussi longtemps si la grille change en cours de route ;
/// dix minutes coûtent au pire six requêtes par heure et par station.
const RAPPEL_MAX: Duration = Duration::from_secs(600);

/// Délai retenu quand le serveur n'en annonce aucun.
const RAPPEL_DEFAUT: Duration = Duration::from_secs(60);

/// Nombre de morceaux consécutifs sans album au-delà duquel on cesse
/// d'interroger la grille de cette station.
///
/// La grille publie souvent le morceau **en retard d'un** : mesuré, elle
/// s'arrête pile au début de ce qui passe. Sur certaines stations elle
/// rattrape en quelques secondes et l'album est là ; sur d'autres — les 45
/// locales, notamment — elle ne l'a jamais eu sur toute la durée d'un morceau.
/// Continuer à demander doublerait le nombre de requêtes chez un tiers pour
/// une réponse qui ne vient pas, ce que le plafond évite.
const MANQUES_MAX: u32 = 5;

/// Durée maximale plausible pour un élément d'antenne. Au-delà, la durée vient
/// d'une borne aberrante et vaut mieux ignorée qu'affichée.
const DUREE_MAX_S: u64 = 24 * 3600;

/// URL du direct d'une station, pour un profil de rendu donné.
///
/// Le dernier segment n'identifie pas la station mais le **profil de rendu**
/// que le serveur applique à sa réponse, et il change ce qu'on reçoit — au
/// point qu'un mauvais choix rend le plugin muet. Mesuré au même instant sur
/// Mouv' : `webrf_fip_player` répond « Le direct » / « Mouv' » (le slogan),
/// quand `webrf_mouv_player` répond « La Playlist » / « SOOLKING - Bye Bye
/// (feat. TAYC) », qui est bien ce qui passait à l'antenne. Chaque station
/// porte donc son profil dans la table.
fn url_direct(id: u32, profil: &str) -> String {
    format!("https://api.radiofrance.fr/livemeta/live/{id}/{profil}")
}

/// URL de la grille d'une station : la liste des éléments diffusés, où chaque
/// morceau porte son album. Pas de profil de rendu ici, la forme est unique.
fn url_grille(id: u32) -> String {
    format!("https://api.radiofrance.fr/livemeta/pull/{id}")
}

/// Texte non vide d'un champ, `None` sinon.
fn texte(v: &Value, cle: &str) -> Option<String> {
    let s = v.get(cle)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Analyse une réponse du direct. `None` pour tout ce qui n'est pas du JSON
/// exploitable — le point d'entrée n'est pas documenté, une refonte doit se
/// traduire par un silence et non par un affichage faux.
///
/// Les noms de champs sont ceux mesurés. `now.firstLine` et `now.secondLine`
/// portent la paire à afficher, mais **ce qu'elle contient dépend du profil**
/// (voir `url_direct`), et la réponse le dit elle-même :
///
/// - avec `firstLineSongUuid`, `firstLine` **est** le morceau et `secondLine`
///   son artiste — la paire est déjà séparée, et les bornes délimitent le
///   morceau, donc leur écart est bien sa durée ;
/// - sans lui, `firstLine` est l'**émission** et `secondLine` porte ce qui s'y
///   joue, sous la forme d'une seule chaîne « ARTISTE - Titre ». Les bornes
///   sont alors celles de l'émission : mesuré sur Mouv', elles couvraient une
///   heure. Les prendre pour la durée d'un morceau afficherait une progression
///   fausse, donc la durée est écartée dans ce cas.
pub fn parse_direct(charge: &str) -> Option<Direct> {
    let v: Value = serde_json::from_str(charge).ok()?;
    let recontacter = v
        .get("delayToRefresh")
        .and_then(Value::as_u64)
        .map(|ms| Duration::from_millis(ms).clamp(RAPPEL_MIN, RAPPEL_MAX))
        .unwrap_or(RAPPEL_DEFAUT);
    let Some(now) = v.get("now") else {
        // Réponse bien formée mais sans direct : rien à dire, on repassera.
        return Some(Direct { meta: None, song_uuid: None, recontacter });
    };
    let est_un_morceau = now.get("firstLineSongUuid").is_some_and(|u| !u.is_null());
    let duree = match (now.get("startTime").and_then(Value::as_u64), now.get("endTime").and_then(Value::as_u64)) {
        (Some(debut), Some(fin)) if fin > debut => Some(fin - debut),
        _ => None,
    };
    let title = texte(now, "firstLine");
    let artist = texte(now, "secondLine");
    // Les deux lignes identiques n'apprennent rien deux fois : c'est ce que
    // renvoie une locale hors musique (« Le 18/19, ICI Picardie » des deux
    // côtés), et l'afficher donnerait « X — X ».
    let artist = artist.filter(|a| !title.as_ref().is_some_and(|t| t.trim().eq_ignore_ascii_case(a.trim())));
    let meta = Meta {
        title,
        artist,
        // Le direct ne porte pas d'album : il se lit dans la grille, à part.
        album: None,
        duration_s: duree.filter(|_| est_un_morceau).filter(|d| *d <= DUREE_MAX_S).map(|d| d as u32),
        // Même filtre que la durée : sans `firstLineSongUuid`, les bornes sont
        // celles d'une tranche d'antenne, pas d'un morceau.
        start_time: now
            .get("startTime")
            .and_then(Value::as_u64)
            .filter(|_| est_un_morceau)
            .filter(|_| duree.is_some_and(|d| d <= DUREE_MAX_S)),
    };
    // Une durée seule n'est pas affichable : ce n'est pas une réponse.
    let meta = (meta.artist.is_some() || meta.title.is_some()).then_some(meta);
    Some(Direct { meta, song_uuid: texte(now, "songUuid"), recontacter })
}

/// Album du morceau `song_uuid` dans une réponse de la grille, s'il y figure.
///
/// La correspondance se fait sur `songId`, **pas** sur `uuid` : `uuid`
/// identifie l'élément de grille, `songId` le morceau, et c'est ce dernier que
/// le direct renvoie dans `songUuid`. Vérifié sur quatre stations, toutes
/// concordantes sur `songId` et aucune sur `uuid`.
///
/// `None` est le cas courant, pas une anomalie : la grille publie souvent le
/// morceau en retard d'un, et l'album n'est alors simplement pas encore là.
pub fn album_dans_grille(charge: &str, song_uuid: &str) -> Option<String> {
    let v: Value = serde_json::from_str(charge).ok()?;
    let steps = v.get("steps")?.as_object()?;
    let step = steps.values().find(|s| s.get("songId").and_then(Value::as_str) == Some(song_uuid))?;
    texte(step, "titreAlbum")
}

/// Interroge la grille pour l'album du morceau en cours. Toute erreur vaut
/// « pas d'album » : c'est un supplément, il ne doit jamais empêcher le titre
/// de partir.
async fn cherche_album(client: &reqwest::Client, id: u32, song_uuid: &str) -> Option<String> {
    let resp = client.get(url_grille(id)).send().await.ok()?;
    if !resp.status().is_success() {
        tracing::debug!("schedule query for station {id}: HTTP {}", resp.status());
        return None;
    }
    album_dans_grille(&resp.text().await.ok()?, song_uuid)
}

/// Interroge une fois le direct d'une station.
async fn interroge(client: &reqwest::Client, id: u32, profil: &str) -> Result<Direct> {
    let resp = client.get(url_direct(id, profil)).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let corps = resp.text().await?;
    let Some(direct) = parse_direct(&corps) else {
        bail!("reponse illisible ({} octets)", corps.len());
    };
    Ok(direct)
}

/// Prochain recul après un échec, d'après le recul courant.
pub fn prochain_recul(recul: Duration) -> Duration {
    (recul * 2).min(RECUL_MAX)
}

/// Suit une station jusqu'à ce que la tâche soit abandonnée : interroge le
/// direct, attend le délai annoncé, recommence.
///
/// Ne rend jamais la main. C'est l'appelant qui arrête cette tâche (`abort`)
/// quand ce qui joue change — d'où l'étiquetage de chaque relevé par l'`id` :
/// un relevé déjà en file au moment de l'arrêt doit pouvoir être écarté.
///
/// **Seuls les changements sont émis.** Le serveur redit la même chose à
/// chaque interrogation ; réémettre ferait écrire une ligne au cœur toutes les
/// dix secondes pour rien. Le premier relevé, lui, part toujours : cette tâche
/// naît avec la station, donc son « dernier vu » est vide, et l'affichage se
/// remplit dès la première réponse plutôt qu'au changement de morceau suivant.
pub async fn suit(id: u32, profil: String, tx: mpsc::Sender<(u32, Meta)>) {
    let client = match reqwest::Client::builder()
        .user_agent("ritornello/0.1 (https://github.com/skerdudou/ritornello)")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        // Défaut de construction du client (pile TLS absente) : irrécupérable,
        // et se taire vaut mieux que boucler dessus.
        Err(e) => {
            tracing::warn!("HTTP client unavailable, station {id} will stay silent: {e}");
            return;
        }
    };
    let mut recul = RECUL_BASE / 2;
    // Dernier relevé émis, **sans son album** : c'est sur cette forme que porte
    // la comparaison, pour qu'un album trouvé (ou non) une fois ne change pas
    // le verdict « c'est le même morceau qu'avant » au tour suivant.
    let mut dernier: Option<Meta> = None;
    let mut manques = 0u32;
    loop {
        match interroge(&client, id, &profil).await {
            Ok(direct) => {
                recul = RECUL_BASE / 2;
                if let Some(meta) = direct.meta {
                    if dernier.as_ref() != Some(&meta) {
                        dernier = Some(meta.clone());
                        // L'album se cherche **une fois par morceau**, et
                        // seulement ici : au fil des interrogations d'un même
                        // morceau, la réponse ne changerait pas.
                        let mut a_envoyer = meta;
                        if let Some(uuid) = direct.song_uuid.as_deref() {
                            if manques < MANQUES_MAX {
                                a_envoyer.album = cherche_album(&client, id, uuid).await;
                                if a_envoyer.album.is_some() {
                                    manques = 0;
                                } else {
                                    manques += 1;
                                    if manques == MANQUES_MAX {
                                        tracing::debug!(
                                            "station {id}: no album in the schedule after {MANQUES_MAX} tracks, no longer asking"
                                        );
                                    }
                                }
                            }
                        }
                        if tx.send((id, a_envoyer)).await.is_err() {
                            // Le plugin ne nous écoute plus : la station a changé.
                            return;
                        }
                    }
                }
                tokio::time::sleep(direct.recontacter).await;
            }
            Err(e) => {
                // Tout échec est journalisé : sans cela, une station qui ne
                // répond plus ne laisserait aucune trace dans `/api/logs` et
                // personne ne verrait jamais rien.
                tracing::info!("live query failed for station {id}: {e}");
                recul = prochain_recul(recul);
                tokio::time::sleep(recul).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Réponse **capturée telle quelle** sur le direct de FIP (station 7).
    const REPONSE_FIP: &str = r#"{"prev":[{"firstLine":"Le direct","secondLine":"La radio la plus éclectique du monde","songUuid":null,"cover":"7eee98cb-3f59-4a3b-b921-6a4be85af542","startTime":null,"endTime":null}],"now":{"firstLine":"I love marijuana","firstLineSongUuid":"1691b015-c8b9-48d2-a296-1f846e13af7b","secondLine":"Linval Thompson","secondLineSongUuid":"1691b015-c8b9-48d2-a296-1f846e13af7b","songUuid":"1691b015-c8b9-48d2-a296-1f846e13af7b","cover":"5b93ce44-3ed6-4409-a2d7-4bd159c061f8","startTime":1786722565,"endTime":1786722762},"next":[],"delayToRefresh":70000}"#;

    /// Réponse **capturée telle quelle** sur Mouv' (station 6, profil
    /// `webrf_mouv_player`) : `firstLine` est l'émission, `secondLine` le
    /// morceau tout entier, et les bornes couvrent **l'émission** — une heure.
    const REPONSE_MOUV: &str = r#"{"prev":[],"now":{"firstLine":"La Playlist","secondLine":"OZUNA - Mi yo de antes","secondLineSongUuid":"c6ed3f57-10a8-435f-b71e-adca48916dce","thirdLine":null,"producers":null,"songUuid":"c6ed3f57-10a8-435f-b71e-adca48916dce","cover":"2df667ba-2852-495c-89a9-9a998daa7c0d","startTime":1786723200,"endTime":1786726800},"next":[],"delayToRefresh":3090000}"#;

    /// Réponse **capturée telle quelle** sur une locale hors musique : les deux
    /// lignes disent la même chose.
    const REPONSE_LOCALE_MUETTE: &str = r#"{"now":{"firstLine":"Le 18/19, ICI Picardie","secondLine":"Le 18/19, ici Picardie","startTime":1786723800,"endTime":1786727400},"delayToRefresh":270000}"#;

    #[test]
    fn analyse_une_reponse_reelle() {
        let d = parse_direct(REPONSE_FIP).unwrap();
        let m = d.meta.unwrap();
        // `firstLine` est le titre, `secondLine` l'artiste : l'inverse de ce
        // que l'ordre des champs laisse croire au premier regard.
        assert_eq!(m.title.as_deref(), Some("I love marijuana"));
        assert_eq!(m.artist.as_deref(), Some("Linval Thompson"));
        // `firstLineSongUuid` est présent : les bornes sont celles du morceau.
        assert_eq!(m.duration_s, Some(197));
        assert_eq!(d.recontacter, Duration::from_secs(70));
    }

    #[test]
    fn une_emission_qui_porte_un_morceau_ne_prend_pas_la_duree_de_lemission() {
        // Le défaut que ce découpage évite : sans `firstLineSongUuid`, les
        // bornes sont celles de l'émission (ici une heure). Les afficher comme
        // durée du morceau donnerait une progression fausse.
        let d = parse_direct(REPONSE_MOUV).unwrap();
        let m = d.meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("La Playlist"));
        assert_eq!(m.artist.as_deref(), Some("OZUNA - Mi yo de antes"));
        assert_eq!(m.duration_s, None, "3600 s est la tranche, pas le morceau");
    }

    #[test]
    fn deux_lignes_identiques_ne_sont_pas_repetees() {
        // Sans cela, l'affichage donnerait « Le 18/19, ici Picardie — Le 18/19,
        // ICI Picardie ». La comparaison ignore la casse : la source elle-même
        // n'est pas constante là-dessus (« ICI » contre « ici »).
        let m = parse_direct(REPONSE_LOCALE_MUETTE).unwrap().meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Le 18/19, ICI Picardie"));
        assert_eq!(m.artist, None);
    }

    #[test]
    fn le_delai_annonce_est_borne_des_deux_cotes() {
        // 3 090 000 ms = 51 min : la fin de la tranche. On repasse avant.
        assert_eq!(parse_direct(REPONSE_MOUV).unwrap().recontacter, RAPPEL_MAX);
        let court = r#"{"now":{"firstLine":"t"},"delayToRefresh":10}"#;
        assert_eq!(parse_direct(court).unwrap().recontacter, RAPPEL_MIN);
        let absent = r#"{"now":{"firstLine":"t"}}"#;
        assert_eq!(parse_direct(absent).unwrap().recontacter, RAPPEL_DEFAUT);
    }

    #[test]
    fn une_reponse_sans_direct_donne_un_delai_sans_metadonnees() {
        let d = parse_direct(r#"{"prev":[],"next":[],"delayToRefresh":20000}"#).unwrap();
        assert!(d.meta.is_none(), "rien a afficher");
        assert_eq!(d.recontacter, Duration::from_secs(20), "mais on sait quand repasser");
    }

    #[test]
    fn accepte_une_reponse_partielle() {
        // Toute information disponible est affichée : partiel vaut mieux que rien.
        let m = parse_direct(r#"{"now":{"firstLine":"Téléphone"}}"#).unwrap().meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("Téléphone"));
        assert_eq!(m.artist, None);
        assert_eq!(m.duration_s, None);
    }

    #[test]
    fn ignore_ce_qui_nest_pas_exploitable() {
        assert!(parse_direct("").is_none());
        assert!(parse_direct("pas du json").is_none());
        assert!(parse_direct(r#"{"errCode":"e400","errMessage":"Bad Request"}"#).unwrap().meta.is_none());
        // Ni titre ni artiste : rien à afficher, donc pas une réponse.
        assert!(parse_direct(r#"{"now":{"startTime":1,"endTime":2}}"#).unwrap().meta.is_none());
        assert!(parse_direct(r#"{"now":{"firstLine":"","secondLine":"  "}}"#).unwrap().meta.is_none());
    }

    #[test]
    fn une_duree_absurde_est_ignoree_sans_perdre_le_titre() {
        for (debut, fin) in [(10u64, 10u64), (10, 5), (0, 90_000)] {
            let brut = format!(
                r#"{{"now":{{"firstLine":"t","firstLineSongUuid":"u","startTime":{debut},"endTime":{fin}}}}}"#
            );
            let m = parse_direct(&brut).unwrap().meta.unwrap();
            assert_eq!(m.duration_s, None, "{debut}->{fin}");
            assert_eq!(m.title.as_deref(), Some("t"));
        }
    }

    /// Réponse **capturée telle quelle** sur la grille de FIP Jazz (station
    /// 65), réduite à deux éléments : celui qui passait et le précédent. Le
    /// direct annonçait au même instant `songUuid`
    /// `2edd8576-0344-4cfc-87ea-b7aaca8e3bb2`.
    const GRILLE: &str = r#"{"steps":{"a_65":{"uuid":"11111111-1111-1111-1111-111111111111","stepId":"a_65","title":"Halfway to the Hudson","start":1786823637,"end":1786823881,"stationId":65,"embedType":"song","authors":"Lucky Chops","songId":"9648da4b-ec2c-4c1d-a75c-ba88b6e2a5fb","titreAlbum":"Lucky Chops","label":"MELTED"},"b_65":{"uuid":"8c391d63-ff9d-4f2c-9ca9-4290e6ed88e1","stepId":"8917b609-dfeb-48d8-9e26-8fea1c26a5ff_65","title":"Blakey's mood","start":1786825073,"end":1786825386,"stationId":65,"embedType":"song","authors":"Stephane Huchard","anneeEditionMusique":2008,"songId":"2edd8576-0344-4cfc-87ea-b7aaca8e3bb2","titreAlbum":"African tribute to Art Blakey","label":"HARMONIA","releaseId":"1a098645-6c16-4efd-93d3-473a8708379d"}},"levels":[],"stationId":65}"#;

    #[test]
    fn lalbum_se_lit_dans_la_grille_par_lidentifiant_de_morceau() {
        assert_eq!(
            album_dans_grille(GRILLE, "2edd8576-0344-4cfc-87ea-b7aaca8e3bb2").as_deref(),
            Some("African tribute to Art Blakey")
        );
        // L'autre élément de la même grille, pour prouver que la sélection
        // porte bien sur l'identifiant et non sur le premier venu.
        assert_eq!(
            album_dans_grille(GRILLE, "9648da4b-ec2c-4c1d-a75c-ba88b6e2a5fb").as_deref(),
            Some("Lucky Chops")
        );
    }

    #[test]
    fn la_correspondance_porte_sur_songid_et_non_sur_uuid() {
        // `uuid` identifie l'élément de grille, `songId` le morceau — et c'est
        // `songId` que le direct renvoie. Les confondre ne trouverait jamais
        // rien, silencieusement.
        assert!(album_dans_grille(GRILLE, "8c391d63-ff9d-4f2c-9ca9-4290e6ed88e1").is_none());
    }

    #[test]
    fn une_grille_qui_ignore_le_morceau_ne_donne_pas_dalbum() {
        // Le cas le plus courant : la grille a un morceau de retard.
        assert!(album_dans_grille(GRILLE, "00000000-0000-0000-0000-000000000000").is_none());
        assert!(album_dans_grille("", "peu-importe").is_none());
        assert!(album_dans_grille("pas du json", "peu-importe").is_none());
        assert!(album_dans_grille(r#"{"stationId":65}"#, "peu-importe").is_none());
        // Élément trouvé mais sans album : rien à dire non plus.
        let sans = r#"{"steps":{"x":{"songId":"u","titreAlbum":"  "}}}"#;
        assert!(album_dans_grille(sans, "u").is_none());
    }

    #[test]
    fn le_direct_expose_lidentifiant_du_morceau_pour_la_recherche_dalbum() {
        let d = parse_direct(REPONSE_FIP).unwrap();
        assert_eq!(d.song_uuid.as_deref(), Some("1691b015-c8b9-48d2-a296-1f846e13af7b"));
        // Hors morceau, il n'y a rien à chercher.
        assert!(parse_direct(REPONSE_LOCALE_MUETTE).unwrap().song_uuid.is_none());
    }

    #[test]
    fn le_direct_ne_porte_jamais_dalbum_lui_meme() {
        // Garde-fou : si le point d'entrée se mettait à en donner un, la
        // grille cesserait d'être la seule source et ce test le signalerait.
        assert_eq!(parse_direct(REPONSE_FIP).unwrap().meta.unwrap().album, None);
    }

    #[test]
    fn lurl_de_la_grille_porte_lidentifiant() {
        assert_eq!(url_grille(65), "https://api.radiofrance.fr/livemeta/pull/65");
    }

    #[test]
    fn lurl_du_direct_porte_lidentifiant_et_le_profil() {
        assert_eq!(
            url_direct(7, "webrf_fip_player"),
            "https://api.radiofrance.fr/livemeta/live/7/webrf_fip_player"
        );
        assert_eq!(
            url_direct(6, "webrf_mouv_player"),
            "https://api.radiofrance.fr/livemeta/live/6/webrf_mouv_player"
        );
    }

    /// `startTime` est retenu **brut** : c'est au moment d'émettre
    /// l'enrichissement qu'on en déduit l'écoulé, pas au moment d'analyser la
    /// réponse — l'analyse reste pure, sans horloge, comme tout ce module.
    #[test]
    fn le_direct_retient_le_debut_du_morceau() {
        let m = parse_direct(REPONSE_FIP).unwrap().meta.unwrap();
        assert_eq!(m.start_time, Some(1786722565));
        assert_eq!(m.duration_s, Some(197));
    }

    /// Même filtre que la durée : sans `firstLineSongUuid`, les bornes sont
    /// celles d'une tranche d'antenne et non d'un morceau. En déduire une
    /// position afficherait une progression fausse — mesuré à une heure sur
    /// Mouv'.
    #[test]
    fn une_tranche_d_antenne_ne_donne_pas_de_debut_de_morceau() {
        let m = parse_direct(REPONSE_MOUV).unwrap().meta.unwrap();
        assert_eq!(m.start_time, None);
        assert_eq!(m.duration_s, None);
    }

    #[test]
    fn le_recul_croit_jusquau_plafond_et_jamais_au_dela() {
        let mut recul = RECUL_BASE;
        let mut vus = vec![recul];
        for _ in 0..10 {
            recul = prochain_recul(recul);
            vus.push(recul);
        }
        assert_eq!(vus[1], Duration::from_secs(4));
        assert_eq!(vus[2], Duration::from_secs(8));
        assert_eq!(*vus.last().unwrap(), RECUL_MAX, "le plafond doit etre atteint");
        assert!(vus.windows(2).all(|p| p[1] >= p[0]), "jamais decroissant");
    }
}
