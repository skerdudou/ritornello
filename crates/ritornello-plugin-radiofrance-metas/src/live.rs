//! Interrogation du direct d'une station Radio France.
//!
//! L'analyse est une fonction pure, testée sur des réponses réelles ; seule
//! `follows` touche le réseau, et **aucun test ne l'appelle**.
//!
//! Contrairement à OUI FM, qui push_cover ses métadonnées dans un
//! `text/event-stream`, Radio France répond à une interrogation ponctuelle —
//! mais en disant lui-même quand le rappeler (`delayToRefresh`). Le rythme
//! d'interrogation est donc dicté par le serveur, pas par nous : c'est ce qui
//! permet de suivre un track de trois minutes sans marteler un tiers, et de
//! laisser une tranche d'antenne d'une heure tranquille.

use anyhow::{bail, Result};
use ritornello_proto::Link;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

/// Ce qu'une réponse apprend du direct.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    /// Année et liens viennent de la **grille**, comme l'album, et sont donc
    /// remplis au même moment (voir `follows`). Le direct ne les porte pas.
    pub year: Option<u16>,
    pub links: Vec<Link>,
    pub duration_s: Option<u32>,
    /// Début du track, en secondes depuis l'époque Unix, tel que le direct
    /// l'announcement. Brut : c'est l'émission de l'enrichment qui en déduit
    /// l'écoulé, pour que ce module reste sans horloge et testable sur des
    /// captures.
    pub start_time: Option<u64>,
    /// UUID brut de la cover, copié depuis `Direct.cover` dans `follows` — ce
    /// champ est ce qui franchit le canal jusqu'au plugin, qui en fait une URL
    /// (voir `cover_url`). `None` inclut le cas où ce n'est pas un track,
    /// la règle étant déjà tranchée en amont, dans `Direct.cover`.
    pub cover: Option<String>,
}

/// Une réponse lue : ce qui passe, et dans combien de temps rappeler.
#[derive(Debug, Clone, PartialEq)]
pub struct Direct {
    /// `None` quand la réponse ne porte ni titre ni artiste — cas d'un
    /// basculement d'antenne. Le délai, lui, reste exploitable.
    pub meta: Option<Meta>,
    /// Identifiant du track en cours, quand il y en a un. Il n'est jamais
    /// affiché : il sert à retrouver dans la grille ce que le direct ne porte
    /// pas — album, année, lien (voir `supplement_in_schedule`).
    pub song_uuid: Option<String>,
    /// UUID de la cover, **seulement quand un vrai track plays**.
    ///
    /// La station renseigne un `cover` même pour « Le direct » et pour ses
    /// émissions : c'est l'image générique de l'antenne. L'annoncer ferait
    /// taire le relai générique, puisqu'un champ rempli est un champ rempli et
    /// qu'aucun étage supérieur ne peut savoir qu'il l'est mal.
    pub cover: Option<String>,
    pub recontact_at: Duration,
}

/// Attente initiale avant nouvelle tentative après échec, puis doublée.
const BACKOFF_BASE: Duration = Duration::from_secs(2);

/// Plafond du recul. Un appareil qui tourne des mois sans surveillance ne doit
/// pas marteler le serveur d'un tiers ; à l'inverse, plafonner évite qu'une
/// coupure réseau d'une nuit se traduise par des heures d'attente au retour.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Plancher du délai annoncé par le serveur. Mesuré : il descend à 10 s sur
/// les stations qui basculent souvent. Ce plancher n'existe donc pas pour
/// corriger le serveur mais pour borner ce qu'une réponse aberrante — ou un
/// mandataire qui réécrirait le JSON — pourrait nous faire faire.
const RECHECK_MIN: Duration = Duration::from_secs(5);

/// Plafond du délai annoncé. Mesuré : les locales annoncent jusqu'à 51 min,
/// soit la fin de la tranche en cours. Les croire sur parole laisserait
/// l'affichage figé aussi longtemps si la grille change en cours de route ;
/// dix minutes coûtent au pire six requêtes par heure et par station.
const RECHECK_MAX: Duration = Duration::from_secs(600);

/// Délai retenu quand le serveur n'en announcement aucun.
const RECHECK_DEFAULT: Duration = Duration::from_secs(60);

/// Nombre de morceaux consécutifs pour lesquels la grille n'apprend **rien**
/// au-delà duquel on cesse de l'interroger pour cette station.
///
/// La grille publie souvent le track **en retard d'un** : mesuré, elle
/// s'arrête pile au début de ce qui passe. Sur certaines stations elle
/// rattrape en quelques secondes ; sur d'autres — les 45 locales, notamment —
/// elle n'a jamais rien sur toute la durée d'un track. Continuer à demander
/// doublerait le nombre de requêtes chez un tiers pour une réponse qui ne
/// vient pas, ce que le cap évite.
///
/// Le critère porte sur le **supplément entier** (album, année, liens) et non
/// sur le seul album depuis le 2026-08-27 : la grille rend l'année bien plus
/// souvent que l'album — 9 éléments sur 9 mesurés, contre 3 sur 9 pour le lien
/// YouTube — et une requête qui rapporte l'année n'est pas une requête pour
/// rien.
const MAX_MISSES: u32 = 5;

/// Durée maximale plausible pour un élément d'antenne. Au-delà, la durée vient
/// d'une bounded aberrante et vaut mieux ignorée qu'affichée.
const MAX_DURATION_S: u64 = 24 * 3600;

/// URL du direct d'une station, pour un profil de rendition donné.
///
/// Le dernier segment n'identifie pas la station mais le **profil de rendition**
/// que le serveur applique à sa réponse, et il change ce qu'on reçoit — au
/// point qu'un mauvais choix rend le plugin muet. Mesuré au même instant sur
/// Mouv' : `webrf_fip_player` répond « Le direct » / « Mouv' » (le slogan),
/// quand `webrf_mouv_player` répond « La Playlist » / « SOOLKING - Bye Bye
/// (feat. TAYC) », qui est bien ce qui passait à l'antenne. Chaque station
/// porte donc son profil dans la table.
fn live_url(id: u32, profil: &str) -> String {
    format!("https://api.radiofrance.fr/livemeta/live/{id}/{profil}")
}

/// URL de la grille d'une station : la liste des éléments diffusés, où chaque
/// track porte son album. Pas de profil de rendition ici, la forme est unique.
fn schedule_url(id: u32) -> String {
    format!("https://api.radiofrance.fr/livemeta/pull/{id}")
}

/// URL de la cover d'un track.
///
/// `preset` n'est pas optionnel : sans lui, l'API rend un 400. Avec, elle rend
/// un 301 vers le CDN, que le cœur follows. `400x400` est un compromis mesuré —
/// 31 887 bytes, contre un original de size non bornée.
pub fn cover_url(uuid: &str) -> String {
    format!("https://api.radiofrance.fr/v1/services/embed/image/{uuid}?preset=400x400")
}

/// Texte non clear d'un champ, `None` sinon.
fn text(v: &Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Analyse une réponse du direct. `None` pour tout ce qui n'est pas du JSON
/// exploitable — le point d'entrée n'est pas documenté, une refonte doit se
/// traduire par un silence et non par un affichage faux.
///
/// Les names de champs sont ceux mesurés. `now.firstLine` et `now.secondLine`
/// portent la paire à afficher, mais **ce qu'elle contains dépend du profil**
/// (voir `live_url`), et la réponse le dit elle-même :
///
/// - avec `firstLineSongUuid`, `firstLine` **est** le track et `secondLine`
///   son artiste — la paire est déjà séparée, et les bornes délimitent le
///   track, donc leur écart est bien sa durée ;
/// - sans lui, `firstLine` est l'**émission** et `secondLine` porte ce qui s'y
///   plays, sous la forme d'une seule chaîne « ARTISTE - Titre ». Les bornes
///   sont alors celles de l'émission : mesuré sur Mouv', elles couvraient une
///   heure. Les prendre pour la durée d'un track afficherait une progress
///   fausse, donc la durée est écartée dans ce cas.
pub fn parse_direct(charge: &str) -> Option<Direct> {
    let v: Value = serde_json::from_str(charge).ok()?;
    let recontact_at = v
        .get("delayToRefresh")
        .and_then(Value::as_u64)
        .map(|ms| Duration::from_millis(ms).clamp(RECHECK_MIN, RECHECK_MAX))
        .unwrap_or(RECHECK_DEFAULT);
    let Some(now) = v.get("now") else {
        // Réponse bien formée mais sans direct : rien à dire, on repassera.
        return Some(Direct { meta: None, song_uuid: None, cover: None, recontact_at });
    };
    let est_un_morceau = now.get("firstLineSongUuid").is_some_and(|u| !u.is_null());
    let duration = match (now.get("startTime").and_then(Value::as_u64), now.get("endTime").and_then(Value::as_u64)) {
        (Some(debut), Some(fin)) if fin > debut => Some(fin - debut),
        _ => None,
    };
    let title = text(now, "firstLine");
    let artist = text(now, "secondLine");
    // Les deux lines identiques n'apprennent rien deux fois : c'est ce que
    // renvoie une locale hors musique (« Le 18/19, ICI Picardie » des deux
    // côtés), et l'afficher donnerait « X — X ».
    let artist = artist.filter(|a| !title.as_ref().is_some_and(|t| t.trim().eq_ignore_ascii_case(a.trim())));
    // « C'est un track ET la durée est plausible » : une seule expression,
    // employée pour `duration_s` comme pour `start_time`. Écrite deux fois,
    // elle pourrait dériver ; `start_time` sortirait alors sans `duration_s`,
    // et le plafonnement de la position côté cœur — qui a besoin des deux —
    // disparaîtrait en silence, la barre franchissant la fin du track.
    let morceau_plausible = est_un_morceau && duration.is_some_and(|d| d <= MAX_DURATION_S);
    let meta = Meta {
        title,
        artist,
        // Le direct ne porte ni album, ni année, ni lien : tout cela se read
        // dans la grille, à part.
        album: None,
        year: None,
        links: Vec::new(),
        duration_s: duration.filter(|_| morceau_plausible).map(|d| d as u32),
        start_time: now.get("startTime").and_then(Value::as_u64).filter(|_| morceau_plausible),
        // Rempli plus tard, dans `follows`, depuis `Direct.cover` : à ce stade,
        // l'analyse pure ne connaît que le track, pas encore le canal qui le
        // porte jusqu'au plugin.
        cover: None,
    };
    // Une durée seule n'est pas affichable : ce n'est pas une réponse.
    let meta = (meta.artist.is_some() || meta.title.is_some()).then_some(meta);
    let song_uuid = text(now, "songUuid");
    // Le `songUuid` est le seul discriminant fiable entre un track et une
    // émission — mesuré sur quatre stations.
    let cover = song_uuid.as_ref().and_then(|_| text(now, "cover"));
    Some(Direct { meta, song_uuid, cover, recontact_at })
}

/// Ce que l'élément de grille apprend en plus de l'album.
///
/// Regroupés parce qu'ils se lisent dans le **même** élément que
/// `titreAlbum` : les chercher séparément relirait la grille trois fois pour
/// une seule réponse déjà en main.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supplement {
    pub album: Option<String>,
    /// `anneeEditionMusique`, un **nombre** JSON dans les réponses mesurées.
    pub year: Option<u16>,
    /// `lienYoutube`. Validé par `Link::validated` côté cœur, mais déjà filtré
    /// ici sur son hôte : autant ne pas transmettre ce qui sera refusé.
    pub links: Vec<Link>,
}

impl Supplement {
    pub fn is_empty(&self) -> bool {
        self.album.is_none() && self.year.is_none() && self.links.is_empty()
    }
}

/// Album du track `song_uuid` dans une réponse de la grille, s'il y figure.
///
/// La correspondance se fait sur `songId`, **pas** sur `uuid` : `uuid`
/// identifie l'élément de grille, `songId` le track, et c'est ce dernier que
/// le direct renvoie dans `songUuid`. Vérifié sur quatre stations, toutes
/// concordantes sur `songId` et aucune sur `uuid`.
///
/// `None` est le cas courant, pas une anomalie : la grille publie souvent le
/// track en retard d'un, et l'album n'est alors simplement pas encore là.
/// Tout ce que l'élément de grille du track apprend : album, année, liens.
///
/// Un seul parcours pour les trois. `Supplement::default()` quand la grille
/// ignore le track — le cas courant, elle a souvent un track de retard, et
/// ce n'est pas une anomalie.
pub fn supplement_in_schedule(charge: &str, song_uuid: &str) -> Supplement {
    let Ok(v) = serde_json::from_str::<Value>(charge) else { return Supplement::default() };
    let Some(steps) = v.get("steps").and_then(Value::as_object) else {
        return Supplement::default();
    };
    let Some(step) = steps.values().find(|s| s.get("songId").and_then(Value::as_str) == Some(song_uuid))
    else {
        return Supplement::default();
    };
    // `anneeEditionMusique` est un nombre dans les réponses mesurées, mais le
    // text est accepté aussi : le champ vient d'un tiers qui peut changer de
    // forme sans préavis, exactement comme `durationInSeconds` chez OUI FM.
    let year = match step.get("anneeEditionMusique") {
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
    .as_deref()
    .and_then(ritornello_proto::valid_year);
    let links = text(step, "lienYoutube")
        .map(|url| Link::Youtube { url })
        .and_then(Link::validated)
        .into_iter()
        .collect();
    Supplement { album: text(step, "titreAlbum"), year, links }
}

/// Interroge la grille pour ce que le track en cours y gagne : album, année,
/// liens. Toute erreur vaut « rien trouvé » : ce sont des suppléments, ils ne
/// doivent jamais empêcher le titre de partir.
async fn fetch_supplement(client: &reqwest::Client, id: u32, song_uuid: &str) -> Supplement {
    let Ok(resp) = client.get(schedule_url(id)).send().await else { return Supplement::default() };
    if !resp.status().is_success() {
        tracing::debug!("schedule query for station {id}: HTTP {}", resp.status());
        return Supplement::default();
    }
    let Ok(corps) = resp.text().await else { return Supplement::default() };
    supplement_in_schedule(&corps, song_uuid)
}

/// Interroge une fois le direct d'une station.
async fn query(client: &reqwest::Client, id: u32, profil: &str) -> Result<Direct> {
    let resp = client.get(live_url(id, profil)).send().await?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status());
    }
    let corps = resp.text().await?;
    let Some(direct) = parse_direct(&corps) else {
        bail!("reponse illisible ({} bytes)", corps.len());
    };
    Ok(direct)
}

/// Prochain recul après un échec, d'après le recul courant.
pub fn next_backoff(recul: Duration) -> Duration {
    (recul * 2).min(BACKOFF_MAX)
}

/// Suit une station jusqu'à ce que la tâche soit abandonnée : query le
/// direct, attend le délai annoncé, recommence.
///
/// Ne rend jamais la main. C'est l'appelant qui arrête cette tâche (`abort`)
/// quand ce qui plays change — d'où l'étiquetage de chaque relevé par l'`id` :
/// un relevé déjà en file au moment de l'arrêt doit pouvoir être écarté.
///
/// **Seuls les changements sont émis.** Le serveur redit la même chose à
/// chaque interrogation ; réémettre ferait écrire une line au cœur toutes les
/// dix secondes pour rien. Le premier relevé, lui, part toujours : cette tâche
/// naît avec la station, donc son « dernier vu » est clear, et l'affichage se
/// remplit dès la première réponse plutôt qu'au changement de track suivant.
pub async fn follows(id: u32, profil: String, tx: mpsc::Sender<(u32, Meta)>) {
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
    let mut recul = BACKOFF_BASE / 2;
    // Dernier relevé émis, **sans son album** : c'est sur cette forme que porte
    // la comparaison, pour qu'un album trouvé (ou non) une fois ne change pas
    // le verdict « c'est le même track qu'avant » au tour suivant.
    let mut dernier: Option<Meta> = None;
    let mut manques = 0u32;
    loop {
        match query(&client, id, &profil).await {
            Ok(direct) => {
                recul = BACKOFF_BASE / 2;
                if let Some(mut meta) = direct.meta {
                    // `direct.cover` n'est jamais reconstruit ici : la règle
                    // « pas de cover hors track » est déjà tranchée dans
                    // `parse_direct`, ce champ n'est qu'un passage de témoin
                    // jusqu'au plugin.
                    meta.cover = direct.cover.clone();
                    if dernier.as_ref() != Some(&meta) {
                        dernier = Some(meta.clone());
                        // L'album se cherche **une fois par track**, et
                        // seulement ici : au fil des interrogations d'un même
                        // track, la réponse ne changerait pas.
                        let mut a_envoyer = meta;
                        if let Some(uuid) = direct.song_uuid.as_deref() {
                            if manques < MAX_MISSES {
                                let s = fetch_supplement(&client, id, uuid).await;
                                // Le compteur porte désormais sur le
                                // supplément **entier** et non sur le seul
                                // album, et ce changement de critère est
                                // volontaire : la grille rend l'année bien plus
                                // souvent que l'album (mesuré le 2026-08-27,
                                // 9 éléments sur 9 contre 3 sur 9 pour le lien
                                // YouTube). Continuer à l'interroger quand elle
                                // ne donne pas l'album mais donne l'année n'est
                                // plus une requête pour rien — ce que ce
                                // compteur existe pour éviter.
                                let clear = s.is_empty();
                                a_envoyer.album = s.album;
                                a_envoyer.year = s.year;
                                a_envoyer.links = s.links;
                                if clear {
                                    manques += 1;
                                    if manques == MAX_MISSES {
                                        tracing::debug!(
                                            "station {id}: schedule gave nothing for {MAX_MISSES} tracks, no longer asking"
                                        );
                                    }
                                } else {
                                    manques = 0;
                                }
                            }
                        }
                        if tx.send((id, a_envoyer)).await.is_err() {
                            // Le plugin ne nous écoute plus : la station a changé.
                            return;
                        }
                    }
                }
                tokio::time::sleep(direct.recontact_at).await;
            }
            Err(e) => {
                // Tout échec est journalisé : sans cela, une station qui ne
                // répond plus ne laisserait aucune trace dans `/api/logs` et
                // personne ne verrait jamais rien.
                tracing::info!("live query failed for station {id}: {e}");
                recul = next_backoff(recul);
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
    /// track tout entier, et les bornes couvrent **l'émission** — une heure.
    const REPONSE_MOUV: &str = r#"{"prev":[],"now":{"firstLine":"La Playlist","secondLine":"OZUNA - Mi yo de antes","secondLineSongUuid":"c6ed3f57-10a8-435f-b71e-adca48916dce","thirdLine":null,"producers":null,"songUuid":"c6ed3f57-10a8-435f-b71e-adca48916dce","cover":"2df667ba-2852-495c-89a9-9a998daa7c0d","startTime":1786723200,"endTime":1786726800},"next":[],"delayToRefresh":3090000}"#;

    /// Réponse **capturée telle quelle** sur une locale hors musique : les deux
    /// lines disent la même chose.
    const REPONSE_LOCALE_MUETTE: &str = r#"{"now":{"firstLine":"Le 18/19, ICI Picardie","secondLine":"Le 18/19, ici Picardie","startTime":1786723800,"endTime":1786727400},"delayToRefresh":270000}"#;

    #[test]
    fn analyse_une_reponse_reelle() {
        let d = parse_direct(REPONSE_FIP).unwrap();
        let m = d.meta.unwrap();
        // `firstLine` est le titre, `secondLine` l'artiste : l'inverse de ce
        // que l'order des champs laisse croire au premier regard.
        assert_eq!(m.title.as_deref(), Some("I love marijuana"));
        assert_eq!(m.artist.as_deref(), Some("Linval Thompson"));
        // `firstLineSongUuid` est présent : les bornes sont celles du track.
        assert_eq!(m.duration_s, Some(197));
        assert_eq!(d.recontact_at, Duration::from_secs(70));
    }

    #[test]
    fn une_emission_qui_porte_un_morceau_ne_prend_pas_la_duree_de_lemission() {
        // Le défaut que ce découpage évite : sans `firstLineSongUuid`, les
        // bornes sont celles de l'émission (ici une heure). Les afficher comme
        // durée du track donnerait une progress fausse.
        let d = parse_direct(REPONSE_MOUV).unwrap();
        let m = d.meta.unwrap();
        assert_eq!(m.title.as_deref(), Some("La Playlist"));
        assert_eq!(m.artist.as_deref(), Some("OZUNA - Mi yo de antes"));
        assert_eq!(m.duration_s, None, "3600 s est la tranche, pas le track");
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
        assert_eq!(parse_direct(REPONSE_MOUV).unwrap().recontact_at, RECHECK_MAX);
        let court = r#"{"now":{"firstLine":"t"},"delayToRefresh":10}"#;
        assert_eq!(parse_direct(court).unwrap().recontact_at, RECHECK_MIN);
        let absent = r#"{"now":{"firstLine":"t"}}"#;
        assert_eq!(parse_direct(absent).unwrap().recontact_at, RECHECK_DEFAULT);
    }

    #[test]
    fn une_reponse_sans_direct_donne_un_delai_sans_metadonnees() {
        let d = parse_direct(r#"{"prev":[],"next":[],"delayToRefresh":20000}"#).unwrap();
        assert!(d.meta.is_none(), "rien a afficher");
        assert_eq!(d.recontact_at, Duration::from_secs(20), "mais on sait quand repasser");
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
            supplement_in_schedule(GRILLE, "2edd8576-0344-4cfc-87ea-b7aaca8e3bb2").album.as_deref(),
            Some("African tribute to Art Blakey")
        );
        // L'autre élément de la même grille, pour prouver que la sélection
        // porte bien sur l'identifiant et non sur le premier venu.
        assert_eq!(
            supplement_in_schedule(GRILLE, "9648da4b-ec2c-4c1d-a75c-ba88b6e2a5fb").album.as_deref(),
            Some("Lucky Chops")
        );
    }

    #[test]
    fn la_grille_rend_aussi_lannee_et_le_lien_youtube() {
        // La fixture est une capture reelle : `anneeEditionMusique` y est un
        // **nombre** (2008), et c'est la forme mesuree le 2026-08-27 sur les
        // stations 7 et 65. Ces deux champs etaient lus et jetes.
        let s = supplement_in_schedule(GRILLE, "2edd8576-0344-4cfc-87ea-b7aaca8e3bb2");
        assert_eq!(s.album.as_deref(), Some("African tribute to Art Blakey"));
        assert_eq!(s.year, Some(2008));
        // Cet element-la n'a pas de lien : la grille en donne moins souvent que
        // d'annees (mesure : 3 sur 9 contre 9 sur 9).
        assert!(s.links.is_empty());
        assert!(!s.is_empty(), "album et annee suffisent a ne pas etre clear");
    }

    #[test]
    fn le_lien_youtube_est_retenu_et_valide_sur_son_hote() {
        // Motif mesure le 2026-08-27 sur les stations 7 et 65 :
        // `https://www.youtube.com/watch?v=...`.
        let avec = r#"{"steps":{"a":{"songId":"u","titreAlbum":"X",
            "lienYoutube":"https://www.youtube.com/watch?v=zIqlKJj9IlY"}}}"#;
        assert_eq!(
            supplement_in_schedule(avec, "u").links,
            vec![Link::Youtube { url: "https://www.youtube.com/watch?v=zIqlKJj9IlY".into() }]
        );
        // Un lien vers un autre hote est jete ici deja : inutile de faire
        // traverser au coeur ce qu'il refusera.
        let ailleurs = r#"{"steps":{"a":{"songId":"u","lienYoutube":"https://evil.example/x"}}}"#;
        assert!(supplement_in_schedule(ailleurs, "u").links.is_empty());
    }

    #[test]
    fn une_annee_aberrante_de_la_grille_est_ignoree_sans_perdre_lalbum() {
        let brut = r#"{"steps":{"a":{"songId":"u","titreAlbum":"X","anneeEditionMusique":0}}}"#;
        let s = supplement_in_schedule(brut, "u");
        assert_eq!(s.year, None);
        assert_eq!(s.album.as_deref(), Some("X"), "l'album survit");
        // La forme text est acceptee aussi : le champ vient d'un tiers qui
        // peut changer d'notice, comme `durationInSeconds` chez OUI FM.
        let text = r#"{"steps":{"a":{"songId":"u","anneeEditionMusique":"1952"}}}"#;
        assert_eq!(supplement_in_schedule(text, "u").year, Some(1952));
    }

    #[test]
    fn un_supplement_introuvable_est_vide_et_ne_panique_pas() {
        assert!(supplement_in_schedule(GRILLE, "00000000-0000-0000-0000-000000000000").is_empty());
        assert!(supplement_in_schedule("pas du json", "u").is_empty());
        assert!(supplement_in_schedule("", "u").is_empty());
    }

    #[test]
    fn la_correspondance_porte_sur_songid_et_non_sur_uuid() {
        // `uuid` identifie l'élément de grille, `songId` le track — et c'est
        // `songId` que le direct renvoie. Les confondre ne trouverait jamais
        // rien, silencieusement.
        assert!(supplement_in_schedule(GRILLE, "8c391d63-ff9d-4f2c-9ca9-4290e6ed88e1").album.is_none());
    }

    #[test]
    fn une_grille_qui_ignore_le_morceau_ne_donne_pas_dalbum() {
        // Le cas le plus courant : la grille a un track de retard.
        assert!(supplement_in_schedule(GRILLE, "00000000-0000-0000-0000-000000000000").album.is_none());
        assert!(supplement_in_schedule("", "peu-importe").album.is_none());
        assert!(supplement_in_schedule("pas du json", "peu-importe").album.is_none());
        assert!(supplement_in_schedule(r#"{"stationId":65}"#, "peu-importe").album.is_none());
        // Élément trouvé mais sans album : rien à dire non plus.
        let sans = r#"{"steps":{"x":{"songId":"u","titreAlbum":"  "}}}"#;
        assert!(supplement_in_schedule(sans, "u").album.is_none());
    }

    #[test]
    fn le_direct_expose_lidentifiant_du_morceau_pour_la_recherche_dalbum() {
        let d = parse_direct(REPONSE_FIP).unwrap();
        assert_eq!(d.song_uuid.as_deref(), Some("1691b015-c8b9-48d2-a296-1f846e13af7b"));
        // Hors track, il n'y a rien à chercher.
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
        assert_eq!(schedule_url(65), "https://api.radiofrance.fr/livemeta/pull/65");
    }

    #[test]
    fn lurl_du_direct_porte_lidentifiant_et_le_profil() {
        assert_eq!(
            live_url(7, "webrf_fip_player"),
            "https://api.radiofrance.fr/livemeta/live/7/webrf_fip_player"
        );
        assert_eq!(
            live_url(6, "webrf_mouv_player"),
            "https://api.radiofrance.fr/livemeta/live/6/webrf_mouv_player"
        );
    }

    /// `startTime` est retenu **brut** : c'est au moment d'émettre
    /// l'enrichment qu'on en déduit l'écoulé, pas au moment d'analyser la
    /// réponse — l'analyse reste pure, sans horloge, comme tout ce module.
    #[test]
    fn le_direct_retient_le_debut_du_morceau() {
        let m = parse_direct(REPONSE_FIP).unwrap().meta.unwrap();
        assert_eq!(m.start_time, Some(1786722565));
        assert_eq!(m.duration_s, Some(197));
    }

    /// Même filtre que la durée : sans `firstLineSongUuid`, les bornes sont
    /// celles d'une tranche d'antenne et non d'un track. En déduire une
    /// position afficherait une progress fausse — mesuré à une heure sur
    /// Mouv'.
    #[test]
    fn une_tranche_d_antenne_ne_donne_pas_de_debut_de_morceau() {
        let m = parse_direct(REPONSE_MOUV).unwrap().meta.unwrap();
        assert_eq!(m.start_time, None);
        assert_eq!(m.duration_s, None);
    }

    #[test]
    fn l_url_de_pochette_suit_le_motif_mesure() {
        // Mesure du 2026-08-24 : ce motif rend un 301 vers le CDN, puis un
        // JPEG de 31 887 bytes. `preset` est obligatoire — sans lui, 400.
        assert_eq!(
            cover_url("24abdb92-7220-45c6-8434-a325278efa2b"),
            "https://api.radiofrance.fr/v1/services/embed/image/24abdb92-7220-45c6-8434-a325278efa2b?preset=400x400"
        );
    }

    #[test]
    fn la_pochette_d_un_vrai_morceau_est_retenue() {
        let d = parse_direct(REPONSE_FIP).unwrap();
        assert_eq!(d.cover.as_deref(), Some("5b93ce44-3ed6-4409-a2d7-4bd159c061f8"));
    }

    #[test]
    fn la_pochette_est_tue_quand_ce_n_est_pas_un_morceau() {
        // La station sert une image generique pour « Le direct » et pour ses
        // emissions. L'annoncer ferait taire le relai generique : un champ
        // rempli est un champ rempli, aucun etage superieur ne peut savoir
        // qu'il l'est mal. Le critere est `songUuid`, deja extrait.
        let d = parse_direct(REPONSE_LOCALE_MUETTE).unwrap();
        assert_eq!(d.song_uuid, None, "prealable du test");
        // Precondition, pas une preuve de la regle : REPONSE_LOCALE_MUETTE ne
        // porte aucune key "cover", donc cette assertion passerait meme sans
        // le filtre sur songUuid. C'est l'entree « Le direct » ci-dessous,
        // avec un cover rempli a cote d'un songUuid nul, qui exerce reellement
        // la regle.
        assert_eq!(d.cover, None);

        // Une entree « Le direct » : songUuid nul a cote d'un cover rempli.
        // Valeurs reprises de l'entree `prev` de REPONSE_FIP, capturee plus
        // haut : ce n'est pas invente, c'est la meme forme que le direct sert
        // reellement pour l'antenne generique.
        let direct = r#"{"now":{"firstLine":"Le direct","secondLine":"La radio la plus eclectique du monde","songUuid":null,"cover":"7eee98cb-3f59-4a3b-b921-6a4be85af542"},"delayToRefresh":70000}"#;
        assert_eq!(parse_direct(direct).unwrap().cover, None);
    }

    #[test]
    fn le_recul_croit_jusquau_plafond_et_jamais_au_dela() {
        let mut recul = BACKOFF_BASE;
        let mut vus = vec![recul];
        for _ in 0..10 {
            recul = next_backoff(recul);
            vus.push(recul);
        }
        assert_eq!(vus[1], Duration::from_secs(4));
        assert_eq!(vus[2], Duration::from_secs(8));
        assert_eq!(*vus.last().unwrap(), BACKOFF_MAX, "le cap doit etre atteint");
        assert!(vus.windows(2).all(|p| p[1] >= p[0]), "jamais decroissant");
    }
}
