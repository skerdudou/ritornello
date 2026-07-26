//! Interrogation de l'annuaire communautaire en ligne Radio Browser.
//!
//! Découpage testable, sur le modèle de `musicbrainz.rs` du plugin cd : la
//! partie *pure* (construction de l'URL de requête, analyse de la réponse) est
//! testée contre une capture réelle rangée dans `tests/fixtures/`, l'appel
//! réseau est isolé à part. Aucun test ne touche le réseau : l'API a été vue
//! en panne pendant la conception, un test réseau serait instable par
//! construction.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Nombre de résultats demandés à l'annuaire (politesse : borne haute).
const LIMIT: u32 = 30;

/// Serveurs de l'annuaire, essayés **dans cet ordre** jusqu'au premier qui
/// répond, et tant qu'il reste du budget (voir `SEARCH_BUDGET`).
/// `all.api.radio-browser.info` est un enregistrement tournant (une adresse
/// différente à chaque résolution) : on vise des serveurs concrets.
///
/// Honnêteté sur cette liste : le parc de miroirs de Radio Browser **bouge
/// avec le temps**, ces cinq noms sont ceux connus au moment de l'écriture et
/// rien ne garantit qu'ils existent tous dans deux ans. Ce n'est pas grave :
/// un hôte inconnu échoue vite (résolution DNS ou connexion refusée, bien
/// avant le plafond d'un essai) et on passe au suivant sans avoir entamé le
/// budget. Ce sont les serveurs **lents** qui coûtent, et c'est précisément
/// eux que le budget global borne. Pendant la conception, `de1` renvoyait
/// `503` et `/json/servers` lui-même répondait « no available server » : d'où
/// ce repli, plutôt qu'une découverte dynamique qui dépendrait du même
/// annuaire en panne.
pub const DEFAULT_BASES: &[&str] = &[
    "https://de1.api.radio-browser.info",
    "https://de2.api.radio-browser.info",
    "https://at1.api.radio-browser.info",
    "https://nl1.api.radio-browser.info",
    "https://fi1.api.radio-browser.info",
];

/// Détail d'erreur quand aucun serveur n'a répondu. Comme `timeout` ou
/// `connect failed`, ce texte court est injecté dans le message **traduit**
/// `search_error` à la frontière admin : la phrase vue par l'utilisateur reste
/// dans sa langue, seul le détail technique est en anglais (même convention
/// que le reste du plugin).
const NO_SERVER: &str = "no directory server answered";

/// Budget **global** de l'opération de recherche, partagé par *tous* les essais
/// — et non un délai par serveur appliqué autant de fois qu'il y a de serveurs.
///
/// La raison est extérieure à ce module, et elle est dure : le cœur invoque
/// `set_data` à travers `AdminClient::request`
/// (`crates/ritornello-plugin-sdk/src/client.rs`), qui enveloppe **tout**
/// aller-retour d'admin dans un `tokio::time::timeout(Duration::from_secs(5),
/// …)`. Passé ce délai, le cœur renvoie une erreur de timeout au navigateur et
/// **jette** notre réponse, même si elle finit par arriver. Une recherche qui
/// dépasse 5 s ne se voit donc jamais : elle continue à travailler pour
/// personne pendant que la page affiche déjà une erreur.
///
/// D'où 4 s et pas davantage : il faut de la marge sous le plafond pour la
/// sérialisation et l'aller-retour sur la socket d'admin. Et d'où, aussi, une
/// liste de serveurs volontairement **courte** : elle n'est pas parcourue
/// « jusqu'au bout coûte que coûte », mais tant qu'il reste du budget —
/// allonger la liste n'achèterait rien, seuls les premiers serveurs seraient
/// réellement essayés en cas de lenteur.
const SEARCH_BUDGET: Duration = Duration::from_secs(4);

/// Plafond d'un essai individuel. Un serveur qui n'a pas répondu en 2 s est
/// considéré comme perdu : le budget restant est mieux employé sur le suivant.
const PER_SERVER: Duration = Duration::from_secs(2);

/// Reste en dessous duquel on n'ouvre plus d'essai : établir une connexion TLS
/// pour l'abandonner aussitôt ne rend service à personne (et ferait porter au
/// serveur suivant l'odieux d'un `timeout` quasi nul, journalisé comme un
/// échec de sa part).
const MIN_ATTEMPT: Duration = Duration::from_millis(300);

/// En-tête réclamé explicitement par l'API Radio Browser : un agent
/// identifiable, sur le même format que le plugin cd (`musicbrainz.rs`), avec
/// la version du crate plutôt qu'un numéro figé.
const USER_AGENT: &str = concat!(
    "ritornello/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/skerdudou/ritornello)"
);

/// Une station telle que renvoyée par l'annuaire, réduite aux champs utiles à
/// l'IHM. C'est cette forme qui est exposée par `GetData` (champ `search`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryStation {
    pub name: String,
    pub url: String,
    pub codec: String,
    pub bitrate: u32,
    pub country: String,
}

/// Forme brute d'une entrée de `/json/stations/search`. L'API renvoie une
/// trentaine de champs : tous ceux qui ne sont pas déclarés ici sont ignorés
/// par serde, ce qui rend l'analyse insensible aux évolutions de l'annuaire.
/// Chaque champ est `Option` + `#[serde(default)]` : une entrée incomplète ou
/// un `null` explicite ne doit pas faire échouer la réponse entière.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawStation {
    name: Option<String>,
    url: Option<String>,
    url_resolved: Option<String>,
    codec: Option<String>,
    bitrate: Option<u32>,
    countrycode: Option<String>,
}

/// URL exploitable d'une entrée : `url_resolved` (déjà dé-redirigée par
/// l'annuaire) en priorité, `url` à défaut. `None` si aucune des deux n'est un
/// http(s) — la station est alors ignorée, plutôt que d'être proposée pour
/// finir refusée par `Stations::validate` au moment d'enregistrer.
fn usable_url(raw: &RawStation) -> Option<String> {
    for candidat in [raw.url_resolved.as_deref(), raw.url.as_deref()] {
        let u = candidat.unwrap_or("").trim();
        if u.starts_with("http://") || u.starts_with("https://") {
            return Some(u.to_string());
        }
    }
    None
}

/// Analyse une réponse `/json/stations/search`. Fonction *pure* : c'est elle
/// que testent les tests, jamais le réseau. Les entrées inexploitables sont
/// ignorées silencieusement plutôt que de faire échouer la réponse entière.
pub fn parse_search_results(json: &str) -> Result<Vec<DirectoryStation>, String> {
    let brutes: Vec<RawStation> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(brutes
        .iter()
        .filter_map(|r| {
            let url = usable_url(r)?;
            Some(DirectoryStation {
                name: r.name.clone().unwrap_or_default(),
                url,
                codec: r.codec.clone().unwrap_or_default(),
                bitrate: r.bitrate.unwrap_or(0),
                country: r.countrycode.clone().unwrap_or_default(),
            })
        })
        .collect())
}

/// Encodage pour-cent d'un paramètre de requête (caractères non réservés
/// laissés tels quels). Écrit à la main : la partie pure du module ne dépend
/// d'aucune bibliothèque HTTP, elle reste compilable et testable seule.
fn encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// URL de recherche : `countrycode` est omis quand aucun pays n'est demandé
/// (« tous pays »). `hidebroken` laisse l'annuaire filtrer lui-même les flux
/// morts, `order=clickcount` + `reverse=true` remontent les plus écoutées.
pub fn search_url(base: &str, query: &str, country: Option<&str>) -> String {
    let mut url = format!(
        "{}/json/stations/search?name={}",
        base.trim_end_matches('/'),
        encode(query)
    );
    if let Some(c) = country {
        url.push_str("&countrycode=");
        url.push_str(&encode(&c.to_ascii_uppercase()));
    }
    url.push_str("&hidebroken=true&order=clickcount&reverse=true&limit=");
    url.push_str(&LIMIT.to_string());
    url
}

/// Bases à essayer, dans l'ordre. `RITORNELLO_RADIO_DIRECTORY`, si elle est
/// définie et non vide, **épingle** un serveur : il devient le seul essayé (un
/// exploitant qui impose son miroir ne veut pas nous voir partir ailleurs en
/// douce). Sinon, la liste intégrée.
pub fn bases_from_env() -> Vec<String> {
    match std::env::var("RITORNELLO_RADIO_DIRECTORY") {
        Ok(v) if !v.trim().is_empty() => vec![v.trim().to_string()],
        _ => DEFAULT_BASES.iter().map(|b| b.to_string()).collect(),
    }
}

/// Message d'erreur court : l'affichage complet d'une erreur reqwest embarque
/// l'URL et toute la chaîne de causes, illisible dans la zone de message de la
/// page d'admin. Le texte est ensuite injecté dans un message traduit
/// (`search_error`) à la frontière admin.
fn short_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "timeout".to_string()
    } else if e.is_connect() {
        "connect failed".to_string()
    } else {
        e.without_url().to_string()
    }
}

/// Délai à accorder au prochain essai, à partir du budget **restant**.
/// Fonction *pure* : toute l'arithmétique du budget tient ici, ce qui la rend
/// testable sans réseau ni horloge (le seul appelant, lui, lit une `Instant`).
///
/// `None` signifie « budget épuisé » : ne pas démarrer d'essai de plus.
fn attempt_timeout(remaining: Duration) -> Option<Duration> {
    if remaining < MIN_ATTEMPT {
        None
    } else {
        Some(remaining.min(PER_SERVER))
    }
}

/// Interroge **un** serveur de l'annuaire, avec le délai que lui accorde
/// l'appelant : `search` ne décide pas de son temps, c'est
/// `search_with_fallback` qui répartit le budget. Seul point du plugin qui
/// touche au réseau, et uniquement depuis l'opération `search` de la moitié
/// Admin : la moitié Source (lecture audio) n'en dépend jamais. L'analyse est
/// déléguée à `parse_search_results`, ce qui garde tout le décodage testable
/// hors ligne.
pub async fn search(
    base: &str,
    query: &str,
    country: Option<&str>,
    timeout: Duration,
) -> Result<Vec<DirectoryStation>, String> {
    let url = search_url(base, query, country);
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(short_error)?;
    let resp = client.get(&url).send().await.map_err(short_error)?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(short_error)?;
    parse_search_results(&body)
}

/// Essaie les serveurs **dans l'ordre** et renvoie la première réponse
/// exploitable. C'est la résilience minimale attendue d'un annuaire
/// communautaire : pendant la conception, le serveur par défaut renvoyait
/// `503` et la liste officielle des serveurs était elle-même indisponible.
///
/// Le repli se déclenche sur **n'importe quelle** erreur, y compris une
/// réponse illisible : un miroir qui répond du JSON cassé est aussi inutile
/// qu'un miroir muet. Contrepartie assumée : un vrai défaut d'analyse (dont
/// les tests de la Task 1 sont le garde-fou) se présenterait ici comme « aucun
/// serveur n'a répondu » — d'où le journal détaillé ci-dessous, qui donne le
/// vrai motif serveur par serveur.
///
/// **Budget** : c'est ici, et nulle part ailleurs, qu'il est tenu. Une
/// `Instant` est prise à l'entrée ; avant chaque essai, `attempt_timeout` dit
/// combien de temps lui accorder à partir de ce qui reste de `SEARCH_BUDGET`,
/// ou qu'il ne faut plus en ouvrir. On s'arrête alors immédiatement, sans
/// parcourir la fin de la liste : le cœur ne nous écoutera plus passé 5 s (voir
/// `SEARCH_BUDGET`), continuer serait travailler pour personne.
///
/// Journalisation : chaque échec en `warn` avec le serveur concerné, le succès
/// en `debug`, et l'épuisement du budget explicitement distingué de
/// l'épuisement de la liste. Sur un Pi sans écran, c'est la seule façon de
/// distinguer « tout l'annuaire est tombé » de « ce miroir-là est mort » ou de
/// « la liaison est si lente qu'on n'a pu essayer que deux serveurs » — la
/// page, elle, ne reçoit qu'un message court.
pub async fn search_with_fallback(
    bases: &[String],
    query: &str,
    country: Option<&str>,
) -> Result<Vec<DirectoryStation>, String> {
    let debut = Instant::now();
    let mut essais = 0usize;
    for base in bases {
        let restant = SEARCH_BUDGET.saturating_sub(debut.elapsed());
        let Some(delai) = attempt_timeout(restant) else {
            tracing::warn!(
                "budget de recherche epuise apres {essais} essai(s), \
                 {} serveur(s) non essaye(s)",
                bases.len() - essais
            );
            break;
        };
        essais += 1;
        match search(base, query, country, delai).await {
            Ok(stations) => {
                tracing::debug!("annuaire {base}: {} resultat(s)", stations.len());
                return Ok(stations);
            }
            Err(e) => tracing::warn!("annuaire {base} en echec: {e}"),
        }
    }
    // Un seul message court, jamais la concaténation des erreurs : le détail
    // est dans le journal, la page d'admin n'a pas la place pour cinq causes.
    tracing::warn!(
        "aucun serveur d'annuaire n'a repondu ({essais} essaye(s) en {:?})",
        debut.elapsed()
    );
    Err(format!("{NO_SERVER} ({essais} tried)"))
}

/// Couture d'injection : la moitié Admin ne connaît que ce trait, ce qui
/// permet aux tests de fournir des résultats (ou une erreur) sans ouvrir la
/// moindre socket.
#[async_trait::async_trait]
pub trait Directory: Send + Sync {
    async fn search(
        &self,
        query: &str,
        country: Option<&str>,
    ) -> Result<Vec<DirectoryStation>, String>;
}

/// Implémentation réelle : un appel HTTP sur les bases configurées, essayées
/// dans l'ordre. La liste est figée à la construction (pas de relecture de
/// l'environnement à chaque recherche) : le comportement d'un processus en
/// cours de vie ne change pas sous les pieds de l'utilisateur.
pub struct HttpDirectory {
    pub bases: Vec<String>,
}

impl HttpDirectory {
    /// Construction usuelle : la liste intégrée, ou le serveur épinglé par
    /// `RITORNELLO_RADIO_DIRECTORY`.
    pub fn from_env() -> Self {
        HttpDirectory { bases: bases_from_env() }
    }
}

#[async_trait::async_trait]
impl Directory for HttpDirectory {
    async fn search(
        &self,
        query: &str,
        country: Option<&str>,
    ) -> Result<Vec<DirectoryStation>, String> {
        search_with_fallback(&self.bases, query, country).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/radio-browser-search.json");

    #[test]
    fn parse_extrait_les_stations_de_la_fixture() {
        let stations = parse_search_results(FIXTURE).unwrap();
        // 5 entrées dans la capture, la dernière est sans URL exploitable
        assert_eq!(stations.len(), 4);
        assert_eq!(
            stations[0],
            DirectoryStation {
                name: "France Info".into(),
                url: "http://direct.franceinfo.fr/live/franceinfo-midfi.mp3".into(),
                codec: "MP3".into(),
                bitrate: 128,
                country: "FR".into(),
            }
        );
        assert_eq!(stations[1].name, "RTL");
        assert_eq!(stations[1].bitrate, 64);
        assert_eq!(stations[3].name, "RMC FR");
    }

    #[test]
    fn parse_ignore_une_entree_sans_url_exploitable() {
        let stations = parse_search_results(FIXTURE).unwrap();
        assert!(
            !stations.iter().any(|s| s.name == "Station sans flux"),
            "une entree sans URL ne doit pas etre proposee"
        );
        // une URL non http(s) est traitée comme absente
        let json = r#"[{"name":"X","url":"ftp://nope","url_resolved":"","codec":"MP3","bitrate":64,"countrycode":"FR"}]"#;
        assert!(parse_search_results(json).unwrap().is_empty());
    }

    #[test]
    fn parse_prefere_url_resolved_a_url() {
        let json = r#"[{"name":"X","url":"http://redirige","url_resolved":"http://final","codec":"MP3","bitrate":128,"countrycode":"FR"}]"#;
        assert_eq!(parse_search_results(json).unwrap()[0].url, "http://final");
        // repli sur `url` quand `url_resolved` est vide ou absent
        let json = r#"[{"name":"X","url":"http://direct","url_resolved":"","codec":"MP3","bitrate":128,"countrycode":"FR"}]"#;
        assert_eq!(parse_search_results(json).unwrap()[0].url, "http://direct");
        let json = r#"[{"name":"X","url":"http://direct"}]"#;
        assert_eq!(parse_search_results(json).unwrap()[0].url, "http://direct");
    }

    #[test]
    fn parse_ignore_les_champs_inconnus() {
        let json = r#"[{"stationuuid":"abc","votes":42,"lastcheckok":1,"name":"X",
            "url":"http://x","url_resolved":"http://x","codec":"MP3","bitrate":128,
            "countrycode":"FR","geo_lat":null}]"#;
        let stations = parse_search_results(json).unwrap();
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].name, "X");
    }

    #[test]
    fn parse_rejette_un_json_invalide() {
        assert!(parse_search_results("pas du json").is_err());
        assert!(parse_search_results("{}").is_err());
        // liste vide = zéro résultat, pas une erreur
        assert_eq!(parse_search_results("[]").unwrap(), vec![]);
    }

    #[test]
    fn url_de_requete_avec_pays() {
        assert_eq!(
            search_url("https://de1.api.radio-browser.info", "france info", Some("fr")),
            "https://de1.api.radio-browser.info/json/stations/search?name=france%20info\
             &countrycode=FR&hidebroken=true&order=clickcount&reverse=true&limit=30"
        );
    }

    #[test]
    fn url_de_requete_sans_pays_omet_countrycode() {
        let url = search_url("https://de1.api.radio-browser.info", "jazz", None);
        assert_eq!(
            url,
            "https://de1.api.radio-browser.info/json/stations/search?name=jazz\
             &hidebroken=true&order=clickcount&reverse=true&limit=30"
        );
        assert!(!url.contains("countrycode"));
    }

    #[test]
    fn url_de_requete_normalise_la_base_et_encode_la_recherche() {
        assert!(search_url("https://de1.api.radio-browser.info/", "x", None)
            .starts_with("https://de1.api.radio-browser.info/json/stations/search?"));
        assert!(search_url("https://x", "rock & roll", None).contains("name=rock%20%26%20roll"));
    }

    /// Test *pur* sur la construction de la liste : l'ordre des serveurs et
    /// l'épinglage par l'environnement. Le repli réel (un serveur muet, le
    /// suivant qui répond) n'est **pas** testé : il demanderait le réseau.
    /// Les trois cas sont dans un seul test parce qu'ils manipulent la même
    /// variable d'environnement, globale au processus : les répartir sur
    /// plusieurs tests les rendrait dépendants de l'ordonnancement de cargo.
    #[test]
    fn bases_par_defaut_dans_l_ordre_et_epinglage_par_l_environnement() {
        std::env::remove_var("RITORNELLO_RADIO_DIRECTORY");
        let attendu: Vec<String> = DEFAULT_BASES.iter().map(|b| b.to_string()).collect();
        assert_eq!(bases_from_env(), attendu);
        // l'ordre est celui de la constante, pas un ordre arbitraire
        assert_eq!(bases_from_env()[0], "https://de1.api.radio-browser.info");
        assert_eq!(bases_from_env()[1], "https://de2.api.radio-browser.info");
        assert_eq!(bases_from_env().len(), 5);

        // épinglée : la variable devient la seule base essayée
        std::env::set_var("RITORNELLO_RADIO_DIRECTORY", "https://fr1.api.radio-browser.info");
        assert_eq!(bases_from_env(), vec!["https://fr1.api.radio-browser.info".to_string()]);

        // valeur vide ou blanche = variable ignorée (repli sur la liste)
        std::env::set_var("RITORNELLO_RADIO_DIRECTORY", "   ");
        assert_eq!(bases_from_env(), attendu);
        std::env::remove_var("RITORNELLO_RADIO_DIRECTORY");

        // `HttpDirectory::from_env()` délègue à `bases_from_env()` : même
        // assertion ici, dans le seul test qui possède la variable
        // d'environnement, plutôt que dans un test séparé qui la lirait sans
        // la posséder (et pourrait alors observer l'épinglage ci-dessus posé
        // par un autre thread de test).
        assert_eq!(HttpDirectory::from_env().bases.len(), DEFAULT_BASES.len());
    }

    /// Arithmétique du budget, testée **sans réseau ni horloge** : c'est elle
    /// qui garantit que `search_with_fallback` rend la main avant le plafond de
    /// 5 s imposé par `AdminClient::request` côté cœur. Test *pur* : il appelle
    /// une fonction totale sur des `Duration` données, rien qui puisse
    /// dépendre de la charge de la machine.
    #[test]
    fn le_budget_borne_chaque_essai_puis_refuse_d_en_ouvrir_un_autre() {
        // budget intact : l'essai est plafonné par PER_SERVER, pas par le reste
        assert_eq!(attempt_timeout(SEARCH_BUDGET), Some(PER_SERVER));
        assert_eq!(attempt_timeout(Duration::from_secs(60)), Some(PER_SERVER));
        // budget entamé : l'essai n'obtient que ce qui reste, jamais plus
        let reste = Duration::from_millis(1_500);
        assert_eq!(attempt_timeout(reste), Some(reste));
        // reste tout juste utilisable
        assert_eq!(attempt_timeout(MIN_ATTEMPT), Some(MIN_ATTEMPT));
        // budget épuisé (ou résidu inutilisable) : aucun essai supplémentaire
        assert_eq!(attempt_timeout(Duration::ZERO), None);
        assert_eq!(attempt_timeout(MIN_ATTEMPT - Duration::from_millis(1)), None);
        // les constantes elles-mêmes tiennent sous le plafond du cœur : c'est
        // l'invariant qui rend l'opération visible par le navigateur.
        assert!(SEARCH_BUDGET < Duration::from_secs(5), "plafond AdminClient depasse");
        assert!(PER_SERVER <= SEARCH_BUDGET, "un seul essai ne doit pas epuiser le budget");
        assert!(MIN_ATTEMPT < PER_SERVER);
    }

    /// Aucun test ne touche le réseau : une base qui n'est pas une URL absolue
    /// fait échouer reqwest **avant** toute entrée/sortie, ce qui vérifie le
    /// chemin d'erreur (message court, pas de panique) sans dépendre de
    /// l'annuaire — observé en panne pendant la conception. Le délai passé
    /// n'est jamais atteint : l'échec est immédiat, le test ne dure pas 2 s.
    #[tokio::test]
    async fn search_sur_une_base_invalide_renvoie_une_erreur_courte() {
        let err = search("pas-une-url", "fip", None, PER_SERVER).await.unwrap_err();
        assert!(!err.is_empty(), "un message d'erreur est attendu");
        assert!(!err.contains('\n'), "message d'une seule ligne attendu: {err}");
    }

    /// Épuisement de la liste : toutes les bases sont invalides, donc toutes
    /// échouent avant la moindre entrée/sortie (même mécanique que le test
    /// précédent). Ce qui est vérifié ici, c'est la **boucle** : un seul
    /// message court en sortie, pas la concaténation des cinq erreurs. Les
    /// échecs étant immédiats, le budget n'est pas entamé : la liste est
    /// parcourue en entier et le test reste instantané (aucune attente réelle,
    /// donc rien de dépendant de l'ordonnanceur).
    #[tokio::test]
    async fn search_avec_repli_essaie_toutes_les_bases_puis_abandonne() {
        let bases = vec!["pas-une-url".to_string(), "non-plus".to_string()];
        let err = search_with_fallback(&bases, "fip", None).await.unwrap_err();
        assert!(err.contains("no directory server answered"), "message inattendu: {err}");
        assert!(!err.contains('\n'), "message d'une seule ligne attendu: {err}");
    }

    /// Ne touche pas `RITORNELLO_RADIO_DIRECTORY` : la liste de bases est
    /// donnée explicitement, pas lue depuis l'environnement. La variable est
    /// globale au processus et déjà manipulée par
    /// `bases_par_defaut_dans_l_ordre_et_epinglage_par_l_environnement`
    /// (qui vérifie aussi `HttpDirectory::from_env()`) ; la lire ici aussi
    /// exposerait ce test à une interleaving entre threads de test.
    #[tokio::test]
    async fn http_directory_delegue_au_repli_sur_sa_liste_de_bases() {
        let d = HttpDirectory { bases: vec!["pas-une-url".into()] };
        assert!(Directory::search(&d, "fip", Some("FR")).await.is_err());
    }
}
