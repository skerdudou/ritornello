# Annuaire de radios en ligne — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permettre d'ajouter une station de radio depuis l'annuaire communautaire en ligne **Radio Browser** (recherche par nom + filtre pays, depuis la page d'admin du plugin radio) au lieu de saisir une URL de flux à la main, et rendre la numérotation des présélections **automatique par position** (la colonne « présélection » éditable disparaît). La saisie manuelle reste possible : c'est le repli quand l'annuaire est indisponible.

**Architecture:** Nouveau module `directory.rs` dans `ritornello-plugin-radio`, découpé comme `musicbrainz.rs` du plugin cd : une partie **pure** (`parse_search_results`, `search_url`) testée contre une capture réelle rangée dans `tests/fixtures/`, et une partie **réseau** (`search`, `search_with_fallback`, `HttpDirectory`) isolée derrière le trait `Directory` pour que les tests injectent des résultats sans jamais ouvrir de socket. L'appel réseau essaie **plusieurs serveurs de l'annuaire dans l'ordre** jusqu'à ce que l'un réponde (l'annuaire a été observé entièrement en panne pendant la conception), sous un **budget global de 4 s** partagé par tous les essais : le cœur enveloppe chaque aller-retour d'admin dans un `timeout(5 s)` (`AdminClient::request`), une recherche plus longue ne serait jamais vue. C'est **le plugin** qui interroge l'annuaire, jamais le navigateur (CORS, et les pages d'admin du projet ne chargent aucune ressource externe). Le transport passe par le protocole d'admin existant, **sans extension** : `SetData` porte désormais un `op` discriminant (`save` | `search`) comme le plugin `generic-input`, l'opération `search` attend l'appel réseau puis mémorise les résultats dans un `RwLock<Vec<DirectoryStation>>` porté par `RadioAdmin`, et `GetData` renvoie `{stations, search}`. La moitié **Source** du plugin (la lecture audio) ne voit rien de tout ça : elle tourne dans sa propre tâche `tokio::spawn` et ne partage que `Arc<AsyncRwLock<Stations>>` — jamais bloquée par un appel à l'annuaire. La numérotation automatique est côté navigateur (position de la ligne → `preset` 1..N dans la charge utile de `save`), `Stations::validate` restant l'autorité serveur.

**Tech Stack:** Rust 2021, tokio, reqwest 0.12 (`default-features = false`, features `json` + `rustls-tls`, comme le plugin cd), serde / serde_json / toml 0.8, `ritornello-plugin-sdk` (`AdminPlugin`, `run_admin_plugin`, `SourcePlugin`, `run_source_plugin`), `ritornello-i18n` (`Catalog`), tracing, tempfile (dev). IHM : HTML/JS sans dépendance, servi par le cœur sous l'origine unique.

## Global Constraints

- Source des stations : **annuaire en ligne uniquement** (Radio Browser). Pas de catalogue livré, pas d'import de fichier, pas d'export.
- Qui interroge l'annuaire : **le plugin radio**, pas le navigateur : la page se heurterait au CORS, et les pages d'admin du projet ne chargent **aucune ressource externe**.
- Transport des résultats : aucune extension du protocole d'admin : une opération `search` dans `SetData`, résultats exposés par `GetData` (même mécanique que le mode apprentissage du plugin input).
- Ajout d'une station : côté navigateur, dans la table en cours d'édition ; **rien n'est persisté avant « Enregistrer »** (cohérent avec le reste du projet).
- Numérotation : **automatique, par position** (1, 2, 3…). Le champ preset disparaît de la table. Pas de réordonnancement pour l'instant.
- Limite : **9 présélections** (les chiffres de la télécommande) : au-delà, l'ajout est refusé avec un message clair.
- `Stations::validate` (présélections uniques dans 1..=9, URLs http(s)) reste l'**autorité côté serveur** ; l'IHM refuse d'ajouter une 10ᵉ station plutôt que de laisser la sauvegarde échouer.
- La saisie manuelle d'une URL dans la table doit continuer de fonctionner : c'est le repli quand l'annuaire est en panne.
- La moitié **Source** du plugin n'est jamais bloquée par un appel à l'annuaire : le réseau n'est touché que dans l'opération `search` de la moitié Admin.
- Résilience de l'annuaire : **liste ordonnée de serveurs** essayés l'un après l'autre jusqu'au premier qui répond ; `RITORNELLO_RADIO_DIRECTORY` **épingle** un serveur unique (il devient le seul essayé). Chaque échec individuel est journalisé, un seul message court remonte à la page.
- **Plafond imposé par le cœur** : `AdminClient::request` (`crates/ritornello-plugin-sdk/src/client.rs`) enveloppe **tout** aller-retour d'admin dans un `tokio::time::timeout(Duration::from_secs(5), …)`. L'opération `search` doit donc rendre la main **avant** 5 s, sinon le navigateur reçoit une erreur de timeout et la réponse tardive du plugin est jetée. D'où un **budget global** `SEARCH_BUDGET = 4 s` partagé par **tous** les essais (et non un délai par serveur appliqué N fois), chaque essai recevant `min(budget restant, PER_SERVER = 2 s)` ; dès que le budget est épuisé, on n'ouvre plus d'essai et on renvoie « aucun serveur n'a répondu ».
- Client HTTP **écrit à la main** (reqwest 0.12), et non la crate `radiobrowser` : elle dépend d'`async-std` (deux exécutifs asynchrones dans un binaire armv7, coût réel sur un Pi 2), réclame `reqwest ^0.11` quand le workspace est en 0.12 (deux piles HTTP), et n'est plus publiée depuis octobre 2023. Son seul avantage réel — le repli automatique sur un autre serveur — est repris ici.
- **Aucun test ne touche le réseau.** L'API de l'annuaire a été observée **en panne** pendant la conception (503 sur `de1`, et `/json/servers` lui-même répondant « no available server ») : un test réseau serait instable par construction.
- Le workspace doit compiler après **chaque** task ; **une seule commit par task** (dernière étape).
- Messages de commit et commentaires de code **en français** (les sujets de commit restent sans accents, comme l'historique).
- Tests unitaires en `#[cfg(test)] mod` **dans le fichier testé** (seule exception : le fichier de fixture sous `tests/fixtures/`).
- Toute task qui change une dépendance commite le `Cargo.lock` régénéré.
- Aucune garde `std::sync::RwLock` ne traverse un `.await`.
- Vérification systématique sous WSL : `cargo test -p ritornello-plugin-radio` **et** `cargo clippy -p ritornello-plugin-radio -- -D warnings`.
- Hors périmètre : catalogues locaux livrés, import/export de fichiers, réordonnancement (glisser-déposer), vote/signalement, favoris, historique, recherche par genre ou tag, cache local des résultats entre deux démarrages.

---

## File Structure

- `crates/ritornello-plugin-radio/src/directory.rs` (créer — Tasks 1 et 2) — `DirectoryStation`, `parse_search_results`, `search_url`, `attempt_timeout`, `search`, `search_with_fallback`, trait `Directory`, `HttpDirectory`, `DEFAULT_BASES`, `bases_from_env`, `SEARCH_BUDGET` / `PER_SERVER` / `MIN_ATTEMPT`.
- `crates/ritornello-plugin-radio/tests/fixtures/radio-browser-search.json` (créer — Task 1) — capture réelle de `/json/stations/search`, réduite aux champs analysés.
- `crates/ritornello-plugin-radio/Cargo.toml` (modifier — Task 2) — dépendance `reqwest`.
- `Cargo.lock` (modifier — Task 2) — régénéré.
- `crates/ritornello-plugin-radio/src/main.rs` (modifier — Tasks 1 et 3) — `mod directory;`, construction de `RadioAdmin` (annuaire + état de recherche).
- `crates/ritornello-plugin-radio/src/admin.rs` (modifier — Tasks 3 et 4) — enum `Op` (`save` | `search`), `get_data` → `{stations, search}`, `PAGE_KEYS`.
- `crates/ritornello-plugin-radio/src/index.html` (modifier — Task 4) — recherche annuaire, liste de résultats, numérotation automatique, limite à 9.
- `crates/ritornello-plugin-radio/src/locales/en.toml` (modifier — Tasks 3 et 4) — anglais embarqué.
- `deploy/locales/radio/fr.toml` (modifier — Tasks 3 et 4) — pack français (jeu de clés **identique** à l'anglais).
- `README.md` (modifier — Task 5) — recherche annuaire, `RITORNELLO_RADIO_DIRECTORY`, numérotation automatique.

---

### Task 1: `directory.rs` — partie pure (analyse de la réponse + URL de requête) et fixture

Aucun code réseau dans cette task : uniquement les types, l'analyse d'une réponse JSON et la construction de l'URL de requête, testés contre une capture réelle. Le module est déclaré `#[allow(dead_code)]` dans `main.rs` (il n'est câblé à l'admin qu'en Task 3) pour que `clippy -D warnings` reste vert entre-temps ; l'`allow` est retiré en Task 3.

**Files:**
- Create: `crates/ritornello-plugin-radio/src/directory.rs`
- Create: `crates/ritornello-plugin-radio/tests/fixtures/radio-browser-search.json`
- Modify: `crates/ritornello-plugin-radio/src/main.rs`

**Interfaces:**
- Consumes: `serde::{Deserialize, Serialize}`, `serde_json` (déjà dans les dépendances du crate).
- Produces:
  - `pub struct DirectoryStation { pub name: String, pub url: String, pub codec: String, pub bitrate: u32, pub country: String }` (`Debug, Clone, PartialEq, Eq, Serialize, Deserialize`)
  - `pub fn parse_search_results(json: &str) -> Result<Vec<DirectoryStation>, String>`
  - `pub fn search_url(base: &str, query: &str, country: Option<&str>) -> String`

- [ ] **Step 1: Créer la fixture (capture réelle de l'API)**

Créer `crates/ritornello-plugin-radio/tests/fixtures/radio-browser-search.json` — contenu complet (capture réelle de `/json/stations/search`, réduite aux seuls champs analysés ; l'API en renvoie une trentaine d'autres, que l'analyseur doit ignorer). La dernière entrée, sans URL exploitable, sert de cas de rejet :

```json
[
  {"name":"France Info","url":"http://direct.franceinfo.fr/live/franceinfo-midfi.mp3","url_resolved":"http://direct.franceinfo.fr/live/franceinfo-midfi.mp3","codec":"MP3","bitrate":128,"countrycode":"FR"},
  {"name":"RTL","url":"https://live.m6radio.quortex.io/webM89Hc99XApzgfhXNX8ASN5/grouprtl/nat","url_resolved":"https://live.m6radio.quortex.io/webM89Hc99XApzgfhXNX8ASN5/grouprtl/nat","codec":"AAC","bitrate":64,"countrycode":"FR"},
  {"name":"Europe 1","url":"https://stream.europe1.fr/europe1.aac","url_resolved":"https://stream.europe1.fr/europe1.aac","codec":"AAC","bitrate":128,"countrycode":"FR"},
  {"name":"RMC FR","url":"https://audio.bfmtv.com/rmcradio_128.mp3","url_resolved":"https://audio.bfmtv.com/rmcradio_128.mp3","codec":"MP3","bitrate":128,"countrycode":"FR"},
  {"name":"Station sans flux","url":"","url_resolved":"","codec":"MP3","bitrate":0,"countrycode":"FR"}
]
```

- [ ] **Step 2: Écrire le module avec ses tests (doit échouer : le module n'est pas déclaré)**

Créer `crates/ritornello-plugin-radio/src/directory.rs` — contenu complet :

```rust
//! Interrogation de l'annuaire communautaire en ligne Radio Browser.
//!
//! Découpage testable, sur le modèle de `musicbrainz.rs` du plugin cd : la
//! partie *pure* (construction de l'URL de requête, analyse de la réponse) est
//! testée contre une capture réelle rangée dans `tests/fixtures/`, l'appel
//! réseau est isolé à part. Aucun test ne touche le réseau : l'API a été vue
//! en panne pendant la conception, un test réseau serait instable par
//! construction.

use serde::{Deserialize, Serialize};

/// Nombre de résultats demandés à l'annuaire (politesse : borne haute).
const LIMIT: u32 = 30;

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
}
```

- [ ] **Step 3: Lancer les tests — échec attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **FAIL** — le fichier `src/directory.rs` n'est pas déclaré comme module, il n'est donc pas compilé et aucun de ses tests n'est exécuté (17 tests seulement, ceux qui existaient déjà : 5 dans `main.rs`, 6 dans `config.rs`, 4 dans `admin.rs`, 2 dans `state.rs`).

- [ ] **Step 4: Déclarer le module**

Dans `crates/ritornello-plugin-radio/src/main.rs`, remplacer les trois lignes de tête :

```rust
mod admin;
mod config;
mod state;
```

par :

```rust
mod admin;
mod config;
// Câblé à la moitié Admin en Task 3 (opération `search`) : d'ici là, seuls
// ses tests s'en servent, d'où l'`allow(dead_code)` — retiré au câblage.
#[allow(dead_code)]
mod directory;
mod state;
```

- [ ] **Step 5: Lancer les tests — succès attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **PASS**, 25 tests (17 existants + 8 de `directory.rs`).

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-radio -- -D warnings"`
Expected : aucun avertissement.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(radio): annuaire Radio Browser, analyse des resultats et URL de requete"
```

---

### Task 2: `directory.rs` — partie réseau (`search`, repli multi-serveurs sous budget, trait `Directory`, `HttpDirectory`)

Ajoute `reqwest` et l'appel HTTP réel, qui délègue l'analyse à `parse_search_results`. L'annuaire n'est pas interrogé sur **une** base mais sur une **liste ordonnée** de serveurs, essayés jusqu'au premier qui répond : pendant la conception, `de1` renvoyait `503` et `/json/servers` lui-même répondait « no available server ». `RITORNELLO_RADIO_DIRECTORY` épingle un serveur unique quand l'exploitant veut en imposer un.

Point structurant de cette task : le parcours de la liste est borné par un **budget global** (`SEARCH_BUDGET = 4 s`) partagé par tous les essais, pas par un délai par serveur appliqué autant de fois qu'il y a de serveurs. La raison est extérieure au plugin : le cœur appelle `set_data` à travers `AdminClient::request`, qui enveloppe chaque aller-retour d'admin dans un `timeout(5 s)` ; au-delà, le navigateur reçoit une erreur de timeout et la réponse tardive du plugin est jetée. Un repli qui dépasserait ce plafond ne serait donc pas un repli, juste du travail perdu. Chaque essai reçoit `min(budget restant, PER_SERVER = 2 s)`, et l'arithmétique est extraite dans une fonction **pure** `attempt_timeout`, testable sans réseau ni horloge.

Le trait `Directory` reste la couture qui permettra aux tests de la Task 3 d'injecter des résultats sans réseau. Le module reste `#[allow(dead_code)]` (câblage en Task 3).

**Files:**
- Modify: `crates/ritornello-plugin-radio/Cargo.toml`
- Modify: `crates/ritornello-plugin-radio/src/directory.rs`
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: `reqwest::Client` (`user_agent`, `timeout`, `get`, `send`, `text`), `parse_search_results` et `search_url` (Task 1), `tracing` (déjà dans les dépendances du crate).
- Produces:
  - `pub const DEFAULT_BASES: &[&str]` (5 serveurs, dans l'ordre d'essai : `de1`, `de2`, `at1`, `nl1`, `fi1`)
  - `pub fn bases_from_env() -> Vec<String>` (la variable d'environnement, si définie et non vide, est la **seule** base essayée ; sinon `DEFAULT_BASES`)
  - `const SEARCH_BUDGET: Duration = 4 s` (budget **global** de l'opération), `const PER_SERVER: Duration = 2 s` (plafond d'un essai), `const MIN_ATTEMPT: Duration = 300 ms` (reste en dessous duquel on n'ouvre plus d'essai) — tous privés
  - `fn attempt_timeout(remaining: Duration) -> Option<Duration>` (privée, **pure** : `None` = budget épuisé, sinon `min(remaining, PER_SERVER)`)
  - `pub async fn search(base: &str, query: &str, country: Option<&str>, timeout: Duration) -> Result<Vec<DirectoryStation>, String>` (une seule base ; le délai lui est **donné** par l'appelant, il ne le décide pas)
  - `pub async fn search_with_fallback(bases: &[String], query: &str, country: Option<&str>) -> Result<Vec<DirectoryStation>, String>` (**seul** endroit où le budget est tenu : une `Instant` prise à l'entrée, `attempt_timeout` consulté avant chaque essai)
  - `#[async_trait::async_trait] pub trait Directory: Send + Sync { async fn search(&self, query: &str, country: Option<&str>) -> Result<Vec<DirectoryStation>, String>; }` (inchangé)
  - `pub struct HttpDirectory { pub bases: Vec<String> }` + `pub fn HttpDirectory::from_env() -> Self` + `impl Directory for HttpDirectory`

- [ ] **Step 1: Ajouter la dépendance `reqwest`**

`crates/ritornello-plugin-radio/Cargo.toml` — contenu complet (les *features* sont exactement celles du plugin cd : pas d'OpenSSL système, TLS par rustls) :

```toml
[package]
name = "ritornello-plugin-radio"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "ritornello-plugin-radio"
path = "src/main.rs"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
async-trait = "0.1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
ritornello-proto = { path = "../ritornello-proto" }
ritornello-plugin-sdk = { path = "../ritornello-plugin-sdk" }
ritornello-i18n = { path = "../ritornello-i18n" }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Ajouter les tests de la partie réseau (doivent échouer : `search` n'existe pas)**

Dans `crates/ritornello-plugin-radio/src/directory.rs`, ajouter **à la fin du `mod tests`** (juste avant son `}` final) :

```rust
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

    #[tokio::test]
    async fn http_directory_delegue_au_repli_sur_sa_liste_de_bases() {
        let d = HttpDirectory { bases: vec!["pas-une-url".into()] };
        assert!(Directory::search(&d, "fip", Some("FR")).await.is_err());
        // construit depuis l'environnement : la liste par défaut, non vide
        std::env::remove_var("RITORNELLO_RADIO_DIRECTORY");
        assert_eq!(HttpDirectory::from_env().bases.len(), DEFAULT_BASES.len());
    }
```

- [ ] **Step 3: Lancer les tests — échec attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **FAIL** à la compilation (`cannot find function 'search'`, `cannot find function 'search_with_fallback'`, `cannot find function 'attempt_timeout'`, `cannot find value 'SEARCH_BUDGET'`, `cannot find value 'PER_SERVER'`, `cannot find value 'MIN_ATTEMPT'`, `cannot find value 'DEFAULT_BASES'`, `cannot find function 'bases_from_env'`, `cannot find struct 'HttpDirectory'`, `cannot find trait 'Directory'`).

- [ ] **Step 4: Implémenter la partie réseau**

Dans `crates/ritornello-plugin-radio/src/directory.rs`, remplacer l'en-tête :

```rust
use serde::{Deserialize, Serialize};

/// Nombre de résultats demandés à l'annuaire (politesse : borne haute).
const LIMIT: u32 = 30;
```

par :

```rust
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

/// En-tête réclamé explicitement par l'API Radio Browser.
const USER_AGENT: &str = concat!("ritornello/", env!("CARGO_PKG_VERSION"));
```

puis ajouter, **juste avant** le `#[cfg(test)] mod tests {` :

```rust
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
```

- [ ] **Step 5: Lancer les tests — succès attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **PASS**, 30 tests (25 + 5). Toute la suite reste instantanée : aucun test n'attend un délai (les bases invalides échouent avant la moindre entrée/sortie, et le test du budget est une fonction pure).

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-radio -- -D warnings"`
Expected : aucun avertissement.

- [ ] **Step 6: Commit (avec le `Cargo.lock` régénéré)**

```bash
git add -A
git commit -m "feat(radio): appel reseau a l'annuaire Radio Browser (reqwest, repli multi-serveurs sous budget de 4s)"
```

---

### Task 3: `admin.rs` — opération `search` dans `SetData` et exposition par `GetData`

`SetData` porte désormais un `op` discriminant (`save` | `search`), comme le plugin `generic-input`. `search` attend l'appel à l'annuaire et mémorise les résultats ; `get_data` renvoie `{stations, search}`. La charge utile de `save` change de forme (`{"op":"save","stations":[…]}`), donc `index.html` est ajusté dans la même task pour que la page reste fonctionnelle.

**Files:**
- Modify: `crates/ritornello-plugin-radio/src/admin.rs`
- Modify: `crates/ritornello-plugin-radio/src/main.rs`
- Modify: `crates/ritornello-plugin-radio/src/index.html`
- Modify: `crates/ritornello-plugin-radio/src/locales/en.toml`
- Modify: `deploy/locales/radio/fr.toml`

**Interfaces:**
- Consumes: `crate::directory::{Directory, DirectoryStation}` (dans `admin.rs`) et `crate::directory::HttpDirectory::from_env()` (dans `main.rs`), `crate::config::{Station, Stations}`, `ritornello_i18n::Catalog`, `ritornello_plugin_sdk::AdminPlugin`.
- Produces:
  - `enum Op { Save { stations: Vec<Station> }, Search { query: String, country: String } }` (privé, `#[serde(tag = "op", rename_all = "snake_case")]`)
  - `RadioAdmin` gagne `pub directory: Arc<dyn Directory>` et `pub search: RwLock<Vec<DirectoryStation>>`
  - `get_data()` → `{"stations": [...], "search": [...]}` (`search` = liste vide tant qu'aucune recherche n'a eu lieu)
  - Clés i18n : `bad_request`, `search_error`

- [ ] **Step 1: Ajouter les clés i18n serveur dans les deux catalogues**

`crates/ritornello-plugin-radio/src/locales/en.toml` — remplacer le bloc de tête :

```toml
empty_preset = "empty preset"
preset_out_of_range = "preset {p} out of range 1-9 ({name})"
preset_duplicate = "duplicate preset {p}"
bad_url = "invalid URL for {name}: {url}"
```

par :

```toml
empty_preset = "empty preset"
preset_out_of_range = "preset {p} out of range 1-9 ({name})"
preset_duplicate = "duplicate preset {p}"
bad_url = "invalid URL for {name}: {url}"
bad_request = "invalid request: {detail}"
search_error = "Directory search failed: {detail}"
```

`deploy/locales/radio/fr.toml` — remplacer le bloc de tête :

```toml
empty_preset = "présélection vide"
preset_out_of_range = "présélection {p} hors bornes 1-9 ({name})"
preset_duplicate = "présélection {p} en double"
bad_url = "URL invalide pour {name} : {url}"
```

par :

```toml
empty_preset = "présélection vide"
preset_out_of_range = "présélection {p} hors bornes 1-9 ({name})"
preset_duplicate = "présélection {p} en double"
bad_url = "URL invalide pour {name} : {url}"
bad_request = "requête invalide : {detail}"
search_error = "Recherche dans l'annuaire impossible : {detail}"
```

- [ ] **Step 2: Réécrire les tests d'`admin.rs` (doivent échouer : `Op`, `directory`, `search` n'existent pas)**

Dans `crates/ritornello-plugin-radio/src/admin.rs`, remplacer **tout** le bloc `#[cfg(test)] mod tests { … }` (de la ligne `#[cfg(test)]` jusqu'à la fin du fichier) par :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Station;
    use crate::directory::parse_search_results;

    const FIXTURE: &str = include_str!("../tests/fixtures/radio-browser-search.json");

    /// Annuaire de test : renvoie un résultat figé (ou une erreur) et
    /// enregistre les arguments reçus. Aucune socket, aucun réseau.
    struct StubDirectory {
        resultat: Result<Vec<DirectoryStation>, String>,
        vus: std::sync::Mutex<Vec<(String, Option<String>)>>,
    }

    impl StubDirectory {
        fn ok(stations: Vec<DirectoryStation>) -> Arc<Self> {
            Arc::new(StubDirectory { resultat: Ok(stations), vus: std::sync::Mutex::new(Vec::new()) })
        }
        fn err(msg: &str) -> Arc<Self> {
            Arc::new(StubDirectory {
                resultat: Err(msg.to_string()),
                vus: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl Directory for StubDirectory {
        async fn search(
            &self,
            query: &str,
            country: Option<&str>,
        ) -> Result<Vec<DirectoryStation>, String> {
            self.vus
                .lock()
                .unwrap()
                .push((query.to_string(), country.map(str::to_string)));
            self.resultat.clone()
        }
    }

    fn admin_avec(dir: &std::path::Path, directory: Arc<dyn Directory>) -> RadioAdmin {
        let path = dir.join("stations.toml");
        let stations = Stations {
            stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }],
        };
        stations.save(&path).unwrap();
        RadioAdmin {
            stations_path: path,
            stations: Arc::new(AsyncRwLock::new(stations)),
            catalog: Arc::new(RwLock::new(Catalog::load(
                "radio",
                "en",
                std::path::Path::new("/nonexistent"),
                crate::RADIO_EN,
            ))),
            directory,
            search: RwLock::new(Vec::new()),
        }
    }

    fn admin(dir: &std::path::Path) -> RadioAdmin {
        admin_avec(dir, StubDirectory::ok(Vec::new()))
    }

    #[test]
    fn page_substitue_les_jetons_avec_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("radio")).unwrap();
        std::fs::write(dir.path().join("radio/fr.toml"), "btn_save = \"Enregistrer\"\n").unwrap();
        let mut a = admin(dir.path());
        a.catalog = Arc::new(RwLock::new(Catalog::load("radio", "fr", dir.path(), crate::RADIO_EN)));
        let html = a.page();
        assert!(html.contains("Enregistrer"));
        assert!(!html.contains("{{btn_save}}"));
    }

    #[tokio::test]
    async fn get_data_renvoie_les_stations_et_une_recherche_vide() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let v = a.get_data().await;
        assert_eq!(v["stations"][0]["name"], "FIP");
        assert_eq!(v["search"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn save_valide_persiste_et_met_a_jour() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let nouveau = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "Inter", "url": "http://inter", "preset": 1 }]
        });
        assert!(a.set_data(nouveau).await.is_ok());
        assert_eq!(a.stations.read().await.stations[0].name, "Inter");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "Inter");
    }

    #[tokio::test]
    async fn save_numerote_de_1_a_n_par_position() {
        // Charge utile telle que la produit l'IHM : `preset` = position.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let nouveau = serde_json::json!({
            "op": "save",
            "stations": [
                { "name": "A", "url": "http://a", "preset": 1 },
                { "name": "B", "url": "http://b", "preset": 2 },
                { "name": "C", "url": "http://c", "preset": 3 }
            ]
        });
        assert!(a.set_data(nouveau).await.is_ok());
        let s = Stations::load(&a.stations_path).unwrap();
        assert_eq!(s.by_preset(2).unwrap().name, "B");
        assert_eq!(s.by_preset(3).unwrap().name, "C");
    }

    #[tokio::test]
    async fn save_invalide_renvoie_erreur_et_ne_persiste_pas() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let mauvais = serde_json::json!({
            "op": "save",
            "stations": [{ "name": "X", "url": "http://x", "preset": 12 }]
        });
        assert!(a.set_data(mauvais).await.is_err());
        // l'état partagé et le disque restent inchangés
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn save_dune_dixieme_station_est_refuse_cote_serveur() {
        // Filet serveur : l'IHM refuse déjà d'ajouter au-delà de 9, mais
        // `Stations::validate` reste l'autorité.
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let stations: Vec<serde_json::Value> = (1..=10)
            .map(|i| serde_json::json!({ "name": format!("S{i}"), "url": "http://x", "preset": i }))
            .collect();
        let err = a
            .set_data(serde_json::json!({ "op": "save", "stations": stations }))
            .await
            .unwrap_err();
        assert!(err.contains("10"), "message inattendu: {err}");
        assert!(!Stations::load(&a.stations_path).unwrap().stations.is_empty());
    }

    #[tokio::test]
    async fn search_memorise_les_resultats_et_get_data_les_expose() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(parse_search_results(FIXTURE).unwrap());
        let mut a = admin_avec(dir.path(), stub.clone());
        let op = serde_json::json!({ "op": "search", "query": "france", "country": "FR" });
        assert!(a.set_data(op).await.is_ok());

        let v = a.get_data().await;
        assert_eq!(v["search"].as_array().unwrap().len(), 4);
        assert_eq!(v["search"][0]["name"], "France Info");
        assert_eq!(v["search"][0]["url"], "http://direct.franceinfo.fr/live/franceinfo-midfi.mp3");
        assert_eq!(v["search"][0]["codec"], "MP3");
        assert_eq!(v["search"][0]["bitrate"], 128);
        assert_eq!(v["search"][0]["country"], "FR");
        // les stations configurées ne bougent pas
        assert_eq!(v["stations"][0]["name"], "FIP");
        // rien n'est persisté par une recherche
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
        assert_eq!(stub.vus.lock().unwrap()[0], ("france".to_string(), Some("FR".to_string())));
    }

    #[tokio::test]
    async fn search_sans_pays_ne_transmet_aucun_countrycode() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(Vec::new());
        let mut a = admin_avec(dir.path(), stub.clone());
        let op = serde_json::json!({ "op": "search", "query": "  jazz  ", "country": "" });
        assert!(a.set_data(op).await.is_ok());
        assert_eq!(stub.vus.lock().unwrap()[0], ("jazz".to_string(), None));
        assert_eq!(a.get_data().await["search"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn search_en_erreur_renvoie_un_message_traduit_et_laisse_letat_intact() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubDirectory::ok(parse_search_results(FIXTURE).unwrap());
        let mut a = admin_avec(dir.path(), stub);
        assert!(a
            .set_data(serde_json::json!({ "op": "search", "query": "france", "country": "FR" }))
            .await
            .is_ok());

        // l'annuaire tombe : les résultats précédents restent affichables
        a.directory = StubDirectory::err("timeout");
        let err = a
            .set_data(serde_json::json!({ "op": "search", "query": "france", "country": "FR" }))
            .await
            .unwrap_err();
        assert_eq!(err, "Directory search failed: timeout");
        assert_eq!(a.get_data().await["search"].as_array().unwrap().len(), 4);
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn op_inconnue_ou_absente_renvoie_une_erreur() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let err = a.set_data(serde_json::json!({ "op": "detruire" })).await.unwrap_err();
        assert!(err.starts_with("invalid request:"), "message inattendu: {err}");
        let err2 = a
            .set_data(serde_json::json!({ "stations": [] }))
            .await
            .unwrap_err();
        assert!(err2.starts_with("invalid request:"), "message inattendu: {err2}");
    }
}
```

- [ ] **Step 3: Lancer les tests — échec attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **FAIL** à la compilation (`struct RadioAdmin has no field named 'directory'`, `no field 'search'`, `cannot find trait 'Directory' in this scope`, `cannot find type 'DirectoryStation'`).

- [ ] **Step 4: Implémenter `Op`, l'état de recherche et le nouveau `get_data`/`set_data`**

Dans `crates/ritornello-plugin-radio/src/admin.rs`, remplacer tout ce qui **précède** le `#[cfg(test)]` (c'est-à-dire les `use`, la structure et l'`impl AdminPlugin`) par :

```rust
use crate::config::{Station, Stations};
use crate::directory::{Directory, DirectoryStation};
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

/// Opérations portées par `SetData`, discriminées par le champ `op` (modèle du
/// plugin generic-input) : le protocole d'admin n'est **pas** étendu, tout
/// passe par `GetPage` / `GetData` / `SetData`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// Enregistre la table complète. Seule opération qui écrit sur disque.
    /// Les présélections sont attribuées par position côté navigateur, mais
    /// `Stations::validate` reste l'autorité.
    Save {
        #[serde(default)]
        stations: Vec<Station>,
    },
    /// Interroge l'annuaire en ligne et mémorise les résultats. Rien n'est
    /// persisté : l'utilisateur ajoute ensuite les stations qui l'intéressent
    /// puis clique « Enregistrer ».
    Search {
        query: String,
        /// Code pays ISO ; chaîne vide = « tous pays ».
        #[serde(default)]
        country: String,
    },
}

pub struct RadioAdmin {
    pub stations_path: PathBuf,
    pub stations: Arc<AsyncRwLock<Stations>>,
    pub catalog: Arc<RwLock<Catalog>>,
    /// Accès à l'annuaire derrière un trait : les tests injectent des
    /// résultats sans jamais toucher au réseau.
    pub directory: Arc<dyn Directory>,
    /// Derniers résultats de recherche, exposés par `get_data` (champ
    /// `search`) ; liste vide tant qu'aucune recherche n'a été faite. Une
    /// recherche en échec les laisse intacts.
    pub search: RwLock<Vec<DirectoryStation>>,
}

#[async_trait::async_trait]
impl AdminPlugin for RadioAdmin {
    fn page(&self) -> String {
        let cat = self.catalog.read().unwrap();
        let mut html = include_str!("index.html").to_string();
        for key in [
            "admin_title",
            "col_num",
            "col_name",
            "col_url",
            "btn_add",
            "btn_save",
            "load_error_1",
            "load_error_2",
            "saved",
            "save_error",
        ] {
            html = html.replace(&format!("{{{{{key}}}}}"), cat.get(key));
        }
        html
    }

    async fn get_data(&self) -> serde_json::Value {
        let stations = self.stations.read().await.stations.clone();
        // Garde `std::sync` prise après le seul `.await` de la fonction :
        // aucune garde ne traverse un point d'attente.
        let search = self.search.read().unwrap().clone();
        serde_json::json!({ "stations": stations, "search": search })
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let op: Op = serde_json::from_value(data).map_err(|e| {
            self.catalog
                .read()
                .unwrap()
                .get("bad_request")
                .replace("{detail}", &e.to_string())
        })?;
        match op {
            Op::Save { stations } => {
                let stations = Stations { stations };
                stations
                    .validate()
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                stations.save(&self.stations_path).map_err(|e| e.to_string())?;
                *self.stations.write().await = stations;
                Ok(())
            }
            Op::Search { query, country } => {
                let pays = country.trim().to_string();
                let pays = if pays.is_empty() { None } else { Some(pays) };
                // L'appel réseau est attendu ici (pas de sondage, contrairement
                // à l'apprentissage du plugin input) ; il ne concerne que la
                // moitié Admin, la lecture audio n'est jamais bloquée. C'est
                // aussi le point qui doit rendre la main avant les 5 s
                // qu'`AdminClient::request` accorde au cœur : le budget de
                // `search_with_fallback` (4 s) est là pour ça.
                let resultats = self
                    .directory
                    .search(query.trim(), pays.as_deref())
                    .await
                    .map_err(|detail| {
                        self.catalog
                            .read()
                            .unwrap()
                            .get("search_error")
                            .replace("{detail}", &detail)
                    })?;
                *self.search.write().unwrap() = resultats;
                Ok(())
            }
        }
    }
}
```

- [ ] **Step 5: Câbler l'annuaire dans `main.rs`**

Dans `crates/ritornello-plugin-radio/src/main.rs`, retirer l'`allow(dead_code)` posé en Task 1 — remplacer :

```rust
// Câblé à la moitié Admin en Task 3 (opération `search`) : d'ici là, seuls
// ses tests s'en servent, d'où l'`allow(dead_code)` — retiré au câblage.
#[allow(dead_code)]
mod directory;
```

par :

```rust
mod directory;
```

puis remplacer le bloc de construction de la moitié admin :

```rust
    // La moitié admin n'est construite que si `--admin-socket` a été fourni
    // (mode dégradé sinon, voir plus haut).
    let admin = admin_socket
        .map(|admin_socket| (RadioAdmin { stations_path, stations: stations_shared, catalog }, admin_socket));
```

par :

```rust
    // Annuaire en ligne : la liste intégrée de serveurs, essayés dans l'ordre
    // jusqu'au premier qui répond, ou l'unique serveur épinglé par
    // `RITORNELLO_RADIO_DIRECTORY`. Journalisé au démarrage : sur un Pi sans
    // écran, savoir quels serveurs seront interrogés évite de deviner.
    let directory = directory::HttpDirectory::from_env();
    tracing::info!("annuaire radio, serveurs candidats: {}", directory.bases.join(", "));
    // La moitié admin n'est construite que si `--admin-socket` a été fourni
    // (mode dégradé sinon, voir plus haut).
    let admin = admin_socket.map(|admin_socket| {
        (
            RadioAdmin {
                stations_path,
                stations: stations_shared,
                catalog,
                directory: Arc::new(directory),
                search: RwLock::new(Vec::new()),
            },
            admin_socket,
        )
    });
```

- [ ] **Step 6: Ajuster la charge utile de `save` dans la page**

Dans `crates/ritornello-plugin-radio/src/index.html`, remplacer :

```js
  const r = await fetch('./api/data', {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ stations }),
  });
```

par :

```js
  const r = await fetch('./api/data', {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ op: 'save', stations: stations }),
  });
```

(la page est réécrite en Task 4 ; ce seul ajustement suffit à la garder fonctionnelle dès maintenant.)

- [ ] **Step 7: Lancer les tests — succès attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **PASS**, 36 tests (30 + 10 dans `admin.rs` − 4 anciens remplacés).

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-radio -- -D warnings"`
Expected : aucun avertissement.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(radio): operation search dans l'admin, resultats exposes par get_data"
```

---

### Task 4: `index.html` + i18n — recherche annuaire, bouton « Ajouter », numérotation automatique

La page gagne un bloc de recherche (champ + sélecteur de pays + bouton), une liste de résultats avec un bouton **Ajouter** par ligne, et perd la colonne « présélection » éditable : le numéro affiché est la position de la ligne, recalculée à chaque ajout/suppression, et c'est elle qui part dans la charge utile de `save`. Au-delà de **9** stations, l'ajout est refusé avec un message traduit. Les clés de page passent dans un `PAGE_KEYS` avec les deux tests de garde du plugin `generic-input` (toutes présentes en anglais, parité en/fr).

**Files:**
- Modify: `crates/ritornello-plugin-radio/src/index.html`
- Modify: `crates/ritornello-plugin-radio/src/locales/en.toml`
- Modify: `deploy/locales/radio/fr.toml`
- Modify: `crates/ritornello-plugin-radio/src/admin.rs`

**Interfaces:**
- Consumes: `GET ./api/data` → `{stations: [{name, url, preset}], search: [{name, url, codec, bitrate, country}]}` ; `PUT ./api/data` avec `{op:"save", stations:[…]}` ou `{op:"search", query, country}` (204 = accepté, 422 + `{"error": "…"}` = refusé).
- Produces:
  - `pub const PAGE_KEYS: &[&str]` dans `admin.rs` (21 clés)
  - Nouvelles clés i18n (en **et** fr) : `limit_reached`, `search_title`, `search_placeholder`, `country_label`, `country_fr`, `country_us`, `country_all`, `btn_search`, `btn_add_result`, `searching`, `no_results` ; `save_error` devient un **préfixe** (le détail vient du message traduit renvoyé par le plugin).

- [ ] **Step 1: Ajouter les tests de garde (doivent échouer : `PAGE_KEYS` n'existe pas)**

Dans `crates/ritornello-plugin-radio/src/admin.rs`, ajouter **à la fin du `mod tests`** (juste avant son `}` final) :

```rust
    #[test]
    fn page_ne_laisse_aucun_jeton_non_substitue() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let html = a.page();
        assert!(!html.contains("{{"), "jeton non substitue dans la page");
    }

    #[test]
    fn toutes_les_cles_de_page_existent_dans_len_embarque() {
        let en = ritornello_i18n::try_parse(crate::RADIO_EN).unwrap();
        for key in PAGE_KEYS {
            assert!(en.contains_key(*key), "cle absente de en.toml: {key}");
        }
    }

    /// Pack français livré dans le dépôt.
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/radio/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::RADIO_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }

    #[test]
    fn la_page_porte_la_recherche_annuaire_et_la_numerotation_automatique() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let html = a.page();
        // recherche annuaire : opération, champ, sélecteur de pays
        assert!(html.contains("op: 'search'"), "operation search absente de la page");
        assert!(html.contains("id=\"country\""), "selecteur de pays absent");
        assert!(html.contains("value=\"FR\"") && html.contains("value=\"US\""));
        // numérotation automatique : plus de champ preset éditable
        assert!(!html.contains("type=\"number\""), "colonne preset editable encore presente");
        assert!(html.contains("preset: i + 1"), "numerotation par position absente");
        // limite de 9 présélections
        assert!(html.contains("const MAX = 9"), "limite de 9 presets absente");
    }
```

- [ ] **Step 2: Lancer les tests — échec attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **FAIL** à la compilation (`cannot find value 'PAGE_KEYS' in this scope`).

- [ ] **Step 3: Anglais embarqué**

`crates/ritornello-plugin-radio/src/locales/en.toml` — contenu complet :

```toml
empty_preset = "empty preset"
preset_out_of_range = "preset {p} out of range 1-9 ({name})"
preset_duplicate = "duplicate preset {p}"
bad_url = "invalid URL for {name}: {url}"
bad_request = "invalid request: {detail}"
search_error = "Directory search failed: {detail}"

# page d'admin
admin_title = "stations"
col_num = "N°"
col_name = "Name"
col_url = "Stream URL"
btn_add = "+ Add"
btn_save = "Save"
load_error_1 = "Error loading stations ("
load_error_2 = ") — fix stations.toml, then reload"
saved = "Saved ✓"
save_error = "Error: "
limit_reached = "9 presets maximum — remove one first"

# annuaire en ligne
search_title = "Add from the online directory"
search_placeholder = "Station name"
country_label = "Country"
country_fr = "France"
country_us = "United States"
country_all = "All countries"
btn_search = "Search"
btn_add_result = "Add"
searching = "Searching…"
no_results = "No station found"
```

- [ ] **Step 4: Pack français**

`deploy/locales/radio/fr.toml` — contenu complet (jeu de clés **identique** à l'anglais, c'est l'invariant testé) :

```toml
empty_preset = "présélection vide"
preset_out_of_range = "présélection {p} hors bornes 1-9 ({name})"
preset_duplicate = "présélection {p} en double"
bad_url = "URL invalide pour {name} : {url}"
bad_request = "requête invalide : {detail}"
search_error = "Recherche dans l'annuaire impossible : {detail}"
admin_title = "stations"
col_num = "N°"
col_name = "Nom"
col_url = "URL du flux"
btn_add = "+ Ajouter"
btn_save = "Enregistrer"
load_error_1 = "Erreur de chargement des stations ("
load_error_2 = ") — corriger stations.toml, puis recharger"
saved = "Enregistré ✓"
save_error = "Erreur : "
limit_reached = "9 présélections maximum — en supprimer une d'abord"
search_title = "Ajouter depuis l'annuaire en ligne"
search_placeholder = "Nom de la station"
country_label = "Pays"
country_fr = "France"
country_us = "États-Unis"
country_all = "Tous les pays"
btn_search = "Rechercher"
btn_add_result = "Ajouter"
searching = "Recherche en cours…"
no_results = "Aucune station trouvée"
```

- [ ] **Step 5: Réécrire la page**

`crates/ritornello-plugin-radio/src/index.html` — contenu complet :

```html
<!doctype html>
<html lang="fr">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ritornello — {{admin_title}}</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 44rem; margin: 2rem auto; padding: 0 1rem; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: .25rem; text-align: left; }
  input { width: 100%; box-sizing: border-box; padding: .4rem; }
  td.num { width: 2.5rem; text-align: right; padding-right: .75rem; }
  td.del { width: 2.5rem; }
  button { padding: .4rem .8rem; }
  .bar { margin: 1rem 0; display: flex; gap: .5rem; align-items: center; }
  .bar input { flex: 1; }
  #msg { margin-left: .5rem; }
  #results { list-style: none; padding: 0; }
  #results li { display: flex; gap: .5rem; align-items: center; padding: .25rem 0; }
  #results .label { flex: 1; }
</style>
<h1>ritornello</h1>

<table id="t">
  <thead><tr><th>{{col_num}}</th><th>{{col_name}}</th><th>{{col_url}}</th><th></th></tr></thead>
  <tbody></tbody>
</table>

<div class="bar">
  <button id="add">{{btn_add}}</button>
  <button id="save">{{btn_save}}</button>
  <span id="msg"></span>
</div>

<h2>{{search_title}}</h2>
<div class="bar">
  <input id="q" placeholder="{{search_placeholder}}">
  <label for="country">{{country_label}}</label>
  <select id="country">
    <option value="FR">{{country_fr}}</option>
    <option value="US">{{country_us}}</option>
    <option value="">{{country_all}}</option>
  </select>
  <button id="searchBtn">{{btn_search}}</button>
</div>
<ul id="results"></ul>

<script>
// Numérotation automatique : la présélection d'une station est sa position
// dans la table (1 pour la première). Aucune colonne éditable — le numéro
// affiché est recalculé à chaque ajout/suppression, et c'est lui qui part dans
// la charge utile de `save`. Conséquence assumée : supprimer une ligne
// renumérote les suivantes.
const MAX = 9; // les chiffres de la télécommande
const T = {
  loadError1: '{{load_error_1}}',
  loadError2: '{{load_error_2}}',
  saved: '{{saved}}',
  saveError: '{{save_error}}',
  limitReached: '{{limit_reached}}',
  btnAddResult: '{{btn_add_result}}',
  searching: '{{searching}}',
  noResults: '{{no_results}}',
};

const $ = (id) => document.getElementById(id);
const tbody = $('t').querySelector('tbody');
const msg = (t) => { $('msg').textContent = t; };
const rows = () => [...tbody.querySelectorAll('tr')];

function renumber() {
  rows().forEach((tr, i) => { tr.querySelector('td.num').textContent = String(i + 1); });
}

// Ajoute une ligne côté navigateur uniquement : rien n'est persisté avant
// « Enregistrer ». Renvoie false si la limite de 9 présélections est atteinte.
function addRow(s) {
  if (rows().length >= MAX) { msg(T.limitReached); return false; }
  const tr = document.createElement('tr');
  const num = document.createElement('td');
  num.className = 'num';
  const tdName = document.createElement('td');
  const name = document.createElement('input');
  name.value = s && s.name ? s.name : '';
  tdName.appendChild(name);
  const tdUrl = document.createElement('td');
  const url = document.createElement('input');
  url.value = s && s.url ? s.url : '';
  tdUrl.appendChild(url);
  const tdDel = document.createElement('td');
  tdDel.className = 'del';
  const del = document.createElement('button');
  del.textContent = '✕';
  del.onclick = () => { tr.remove(); renumber(); };
  tdDel.appendChild(del);
  tr.append(num, tdName, tdUrl, tdDel);
  tbody.appendChild(tr);
  renumber();
  return true;
}

async function fetchData() {
  const r = await fetch('./api/data');
  if (!r.ok) throw new Error('HTTP ' + r.status);
  return await r.json();
}

// Renvoie null si l'opération est acceptée (204), sinon le message d'erreur
// traduit renvoyé par le plugin (corps JSON {"error": …} du 422).
async function put(body) {
  const r = await fetch('./api/data', {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (r.status === 204) return null;
  let detail = 'HTTP ' + r.status;
  try { const j = await r.json(); if (j && j.error) detail = j.error; } catch (e) { /* corps non JSON */ }
  return detail;
}

function renderResults(list) {
  const ul = $('results');
  ul.innerHTML = '';
  if (!list.length) {
    const li = document.createElement('li');
    li.textContent = T.noResults;
    ul.appendChild(li);
    return;
  }
  list.forEach((s) => {
    const li = document.createElement('li');
    const label = document.createElement('span');
    label.className = 'label';
    // textContent, jamais innerHTML : le nom vient d'un annuaire public.
    label.textContent = s.name + ' — ' + s.codec + ' ' + s.bitrate + ' kbps'
      + (s.country ? ' (' + s.country + ')' : '');
    const b = document.createElement('button');
    b.textContent = T.btnAddResult;
    b.onclick = () => { if (addRow({ name: s.name, url: s.url })) msg(''); };
    li.append(label, b);
    ul.appendChild(li);
  });
}

async function doSave() {
  const stations = rows().map((tr, i) => {
    const [n, u] = tr.querySelectorAll('input');
    return { preset: i + 1, name: n.value, url: u.value };
  });
  const err = await put({ op: 'save', stations: stations });
  msg(err ? T.saveError + err : T.saved);
}

// Le plugin interroge l'annuaire (le navigateur n'appelle aucune ressource
// externe), mémorise les résultats, puis on les relit par GetData.
async function doSearch() {
  const query = $('q').value.trim();
  if (!query) return;
  msg(T.searching);
  const err = await put({ op: 'search', query: query, country: $('country').value });
  if (err) { msg(err); return; }
  try {
    renderResults((await fetchData()).search || []);
    msg('');
  } catch (e) {
    msg(T.loadError1 + e.message + T.loadError2);
  }
}

async function load() {
  try {
    const data = await fetchData();
    data.stations.sort((a, b) => a.preset - b.preset).forEach((s) => addRow(s));
    if (data.search && data.search.length) renderResults(data.search);
  } catch (e) {
    msg(T.loadError1 + e.message + T.loadError2);
    document.querySelectorAll('button').forEach((b) => { b.disabled = true; });
  }
}

$('add').onclick = () => { if (addRow()) msg(''); };
$('save').onclick = doSave;
$('searchBtn').onclick = doSearch;
$('q').addEventListener('keydown', (e) => { if (e.key === 'Enter') doSearch(); });
load();
</script>
</html>
```

- [ ] **Step 6: Déclarer `PAGE_KEYS` et l'utiliser dans `page()`**

Dans `crates/ritornello-plugin-radio/src/admin.rs`, ajouter **juste avant** la déclaration de `enum Op` :

```rust
/// Clés i18n substituées dans `index.html`. Trois tests les gardent alignées :
/// toutes présentes dans l'anglais embarqué, parité en/fr, et aucun jeton
/// `{{…}}` survivant au rendu.
pub const PAGE_KEYS: &[&str] = &[
    "admin_title",
    "col_num",
    "col_name",
    "col_url",
    "btn_add",
    "btn_save",
    "load_error_1",
    "load_error_2",
    "saved",
    "save_error",
    "limit_reached",
    "search_title",
    "search_placeholder",
    "country_label",
    "country_fr",
    "country_us",
    "country_all",
    "btn_search",
    "btn_add_result",
    "searching",
    "no_results",
];
```

puis remplacer le corps de `page()` :

```rust
    fn page(&self) -> String {
        let cat = self.catalog.read().unwrap();
        let mut html = include_str!("index.html").to_string();
        for key in [
            "admin_title",
            "col_num",
            "col_name",
            "col_url",
            "btn_add",
            "btn_save",
            "load_error_1",
            "load_error_2",
            "saved",
            "save_error",
        ] {
            html = html.replace(&format!("{{{{{key}}}}}"), cat.get(key));
        }
        html
    }
```

par :

```rust
    fn page(&self) -> String {
        let cat = self.catalog.read().unwrap();
        let mut html = include_str!("index.html").to_string();
        for key in PAGE_KEYS {
            html = html.replace(&format!("{{{{{key}}}}}"), cat.get(key));
        }
        html
    }
```

- [ ] **Step 7: Lancer les tests — succès attendu**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : **PASS**, 40 tests (36 + 4).

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-radio -- -D warnings"`
Expected : aucun avertissement.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(radio): recherche dans l'annuaire depuis la page d'admin, numerotation automatique"
```

---

### Task 5: README et vérification finale (workspace + cross-compilation ARM)

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: rien (documentation).
- Produces: section « Plugins » à jour (recherche annuaire, repli multi-serveurs, `RITORNELLO_RADIO_DIRECTORY`, numérotation automatique).

- [ ] **Step 1: Documenter la recherche annuaire dans la section « Plugins »**

Dans `README.md`, remplacer la puce du plugin radio :

```markdown
- `ritornello-plugin-radio` déclare `admin = true` : sa page de gestion des
  stations est servie par le cœur, sous l'origine unique, à
  `http://<hôte>:8080/plugins/radio/` (le plugin ne lie plus aucun port).
```

par :

```markdown
- `ritornello-plugin-radio` déclare `admin = true` : sa page de gestion des
  stations est servie par le cœur, sous l'origine unique, à
  `http://<hôte>:8080/plugins/radio/` (le plugin ne lie plus aucun port). Elle
  permet de saisir une station à la main (nom + URL du flux) **et** d'en
  ajouter une depuis l'annuaire communautaire en ligne
  [Radio Browser](https://api.radio-browser.info) : taper un nom, choisir un
  pays (France, États-Unis, tous), « Rechercher », puis « Ajouter » sur un
  résultat. C'est **le plugin** qui interroge l'annuaire — la page ne charge
  aucune ressource externe — et rien n'est écrit tant qu'« Enregistrer » n'a
  pas été cliqué. Les présélections sont numérotées **automatiquement par
  position** (1 à 9, les chiffres de la télécommande) : ajouter met en fin de
  liste, supprimer renumérote les suivantes ; au-delà de 9, l'ajout est refusé.
  Annuaire injoignable ⇒ message d'erreur sur la page, la lecture en cours et
  les stations déjà configurées ne bougent pas, et la saisie manuelle reste le
  repli. L'annuaire est interrogé sur **plusieurs serveurs essayés dans
  l'ordre** (`de1`, `de2`, `at1`, `nl1`, `fi1` de `api.radio-browser.info`)
  jusqu'à ce que l'un réponde : `all.api.radio-browser.info` est un
  enregistrement tournant, et le parc de miroirs bouge avec le temps — un hôte
  disparu échoue vite, le suivant est essayé, et chaque échec est journalisé.
  L'ensemble tient dans un **budget de 4 s** (2 s au plus par serveur) : la
  page d'admin passe par le protocole d'admin du cœur, qui abandonne toute
  requête au bout de 5 s, donc une recherche qui traîne est arrêtée d'elle-même
  avec un message d'erreur plutôt que de finir en timeout.
  Variables : `RITORNELLO_RADIO_STATIONS`, `RITORNELLO_RADIO_STATE`,
  `RITORNELLO_RADIO_DIRECTORY` (**épingle** un serveur d'annuaire : il devient
  le seul essayé, pour imposer son propre miroir sans recompiler ; non
  définie, la liste intégrée s'applique).
```

- [ ] **Step 2: Vérification finale du workspace**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test --workspace"`
Expected : **PASS**, tout le workspace vert (40 tests pour `ritornello-plugin-radio`).

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy --workspace -- -D warnings"`
Expected : aucun avertissement.

- [ ] **Step 3: Vérifier qu'aucun test ne touche le réseau**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && grep -rn 'radio-browser.info' crates/ritornello-plugin-radio/src"`
Expected : les seules occurrences sont les cinq entrées de `DEFAULT_BASES`, les commentaires, les URL de test **construites** par `search_url` (jamais envoyées) et le test pur de `bases_from_env` (qui compare des chaînes). Aucun test n'envoie de requête vers une vraie base : les tests asynchrones passent tous par `"pas-une-url"` / `"non-plus"`, et le seul qui construit la vraie liste (`HttpDirectory::from_env`) se contente d'en compter les entrées.

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && grep -rn 'DEFAULT_BASES\|bases_from_env\|from_env()' crates/ritornello-plugin-radio/src"`
Expected : définition + `bases_from_env` + `HttpDirectory::from_env` + le câblage de `main.rs` + les deux tests purs — jamais dans un `#[tokio::test]` qui déclenche un appel.

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio 2>&1 | tail -20"`
Expected : suite complète en bien moins d'une seconde (preuve indirecte qu'aucun test n'attend un serveur : un essai réel qui traîne durerait à lui seul jusqu'à `PER_SERVER` = 2 s, et une recherche entière jusqu'à `SEARCH_BUDGET` = 4 s).

- [ ] **Step 3 bis: Vérifier que le budget tient sous le plafond du cœur**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && grep -rn 'from_secs(5)' crates/ritornello-plugin-sdk/src/client.rs"`
Expected : deux occurrences, dont celle d'`AdminClient::request` — le plafond de 5 s appliqué à **tout** aller-retour d'admin, celui que `SEARCH_BUDGET` doit respecter.

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && grep -rn 'SEARCH_BUDGET\|PER_SERVER\|MIN_ATTEMPT\|attempt_timeout' crates/ritornello-plugin-radio/src"`
Expected : les trois constantes définies une seule fois, `attempt_timeout` appelée **uniquement** depuis `search_with_fallback` (le budget n'est tenu qu'à un seul endroit) et depuis son test pur. Aucun `Duration::from_secs(8)` résiduel nulle part.

- [ ] **Step 4: Cross-compilation ARM**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cross build --release --workspace --target armv7-unknown-linux-gnueabihf"`
Expected : **succès**. Point de vigilance : `reqwest` est déclaré `default-features = false` avec `rustls-tls`, donc aucune dépendance à OpenSSL système — c'est exactement la configuration déjà cross-compilée par `ritornello-plugin-cd`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs(radio): documente la recherche dans l'annuaire et RITORNELLO_RADIO_DIRECTORY"
```
