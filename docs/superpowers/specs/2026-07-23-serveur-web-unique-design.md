# ritornello — Serveur web unique

Faire du cœur le seul processus qui écoute un port TCP : les pages d'admin
des plugins (aujourd'hui la seule page de gestion des stations du plugin
radio) sont servies par le cœur, sous une origine unique, via une capacité
« admin » transverse acheminée par IPC — supprimant le second serveur HTTP
qui tournait dans le plugin radio.

Date : 2026-07-23 — Statut : validé

## Contexte

L'architecture à plugins (specs `2026-07-18-plugin-architecture-design.md` et
`2026-07-21-display-plugin-audio-output-design.md`) définit trois genres de
plugin — Source, Input, Display — chacun communiquant avec le cœur sur **un
socket Unix unique**, avec un protocole minimal propre à son genre :

- **Source** (radio, cd) : requête/réponse corrélée par `id`, bidirectionnel.
- **Display** (console) : sens unique cœur → plugin.
- **Input** (mce) : sens unique plugin → cœur.

Aujourd'hui, le plugin radio fait tourner **son propre serveur axum** (port
8081) pour servir sa page de gestion des stations. Le cœur, lui, sert sa page
de statut sur le port 8080. Deux piles axum/tokio tournent donc en parallèle
dans deux processus. Sur un Raspberry Pi 2 (1 Go de RAM, ARMv7), c'est un coût
mémoire évitable, qui empirerait à chaque futur plugin voulant sa propre page.
De plus, les deux pages sont sur des origines HTTP différentes (`:8080` et
`:8081`), ce qui compliquerait tout partage d'état entre elles (le sélecteur
de langue à venir, notamment).

Le champ `admin_url` de `plugins.toml` (un simple lien externe vers le serveur
du plugin, affiché sur la page de statut) devient obsolète avec ce changement.

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Serveur TCP | Le cœur est le **seul** processus à écouter un port (`:8080`). Le plugin radio perd entièrement son serveur axum. |
| Capacité admin | **Transverse au genre** : n'importe quel plugin (Source, Input, Display, ou futur genre) peut déclarer une page d'admin, indépendamment de son genre. |
| Transport admin | **Second socket dédié** (`--admin-socket`), distinct du socket de genre. Protocole admin uniforme, identique quel que soit le genre. Le socket de genre reste intact. |
| Connaissance du schéma | Le cœur ne connaît **jamais** le schéma des données d'un plugin : il relaie du JSON opaque. La validation reste l'affaire du plugin. |
| Périmètre | Zéro nouvelle fonctionnalité utilisateur : c'est une consolidation d'architecture. La page radio garde exactement ses fonctions actuelles, servie ailleurs. |

## Déclaration de la capacité admin

Dans `plugins.toml`, le champ optionnel `admin_url: Option<String>` est
**remplacé** par `admin: bool` (défaut `false`) :

```toml
[[plugin]]
name = "radio"
kind = "source"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
admin = true
```

Quand `admin = true`, le cœur, au moment de spawn le plugin, lui passe un
argument `--admin-socket <path>` **en plus** de son `--socket <path>` de genre
habituel. Le plugin lie alors ce second socket et y sert le protocole admin.
Un plugin sans `admin = true` ne reçoit pas d'`--admin-socket` et n'a rien à
faire de plus.

## Le protocole admin

Nouveau module `ritornello-proto/src/admin.rs`. Requête/réponse corrélée par
`id`, sur le modèle exact du protocole Source (réutilise le même style de
trames JSON par ligne). Trois requêtes :

- `GetPage` → réponse : le HTML complet de la page d'admin (une `String`). Le
  HTML/JS reste dans les sources du plugin (`include_str!`) ; le cœur ne fait
  que le servir.
- `GetData` → réponse : les données d'admin sous forme de **JSON opaque**
  (`serde_json::Value`) — le cœur transporte la valeur sans l'interpréter (il
  ne connaît pas son schéma).
- `SetData { data: <json opaque> }` → le plugin valide et persiste lui-même,
  puis répond `SetResult { ok: bool, error: Option<String> }`.

Types (indicatifs) :

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    GetPage,
    GetData,
    SetData(serde_json::Value),
}

#[derive(Serialize, Deserialize)]
pub struct AdminRequest { pub id: u64, #[serde(flatten)] pub req: AdminReq }

#[derive(Serialize, Deserialize)]
pub struct AdminResponse {
    pub id: u64,
    pub result: AdminResult,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AdminResult {
    Page(String),
    Data(serde_json::Value),
    Set { ok: bool, error: Option<String> },
}
```

## Côté SDK

- `ritornello-plugin-sdk/src/server.rs` : nouveau trait `AdminPlugin` et
  fonction `run_admin_plugin(plugin, admin_socket_path)`, sur le modèle de
  `run_source_plugin` (lie le socket, accepte une connexion, boucle
  requête/réponse) :

  ```rust
  #[async_trait]
  pub trait AdminPlugin: Send + 'static {
      fn page(&self) -> &'static str;                       // HTML statique
      async fn get_data(&self) -> serde_json::Value;        // état courant
      async fn set_data(&mut self, data: serde_json::Value) // validation + persistance
          -> Result<(), String>;                            // Err(msg) = invalide
  }
  ```

  `run_admin_plugin` traduit : `GetPage` → `page()`, `GetData` → `get_data()`,
  `SetData(v)` → `set_data(v)` puis mappe `Ok`→`Set{ok:true,error:None}`,
  `Err(msg)`→`Set{ok:false,error:Some(msg)}`.

- `ritornello-plugin-sdk/src/client.rs` : nouveau `AdminClient` sur le modèle
  de `SourceClient` (connexion avec retry, corrélation par `id`, timeout).
  Méthodes : `get_page() -> Result<String>`, `get_data() -> Result<Value>`,
  `set_data(Value) -> Result<Result<(), String>>` (l'externe = erreur de
  transport/timeout, l'interne = verdict de validation du plugin).

Un plugin qui gère à la fois son genre et l'admin lance **deux tâches** : une
par socket (`run_source_plugin` sur `--socket`, `run_admin_plugin` sur
`--admin-socket`), concurremment. Elles partagent l'état applicatif du plugin
(ex. la liste des stations) via `Arc<RwLock<…>>`, comme le fait déjà
aujourd'hui le plugin radio entre son cœur Source et son serveur web.

## Côté cœur

- `plugins.rs` : `PluginConfig.admin_url` → `PluginConfig.admin: bool`
  (`#[serde(default)]`). `spawn` gagne un chemin d'admin-socket optionnel et,
  s'il est présent, ajoute `--admin-socket <path>` à la commande.
- Au démarrage, pour chaque plugin `admin = true`, le cœur établit un
  `AdminClient` sur le socket admin (connexion concurrente aux autres, comme
  déjà fait pour les sockets de genre — pas de stalle séquentielle).
- Nouvelles routes axum sur le serveur existant du cœur :
  - `GET /plugins/{name}/` → `AdminClient::get_page` → `Html(...)`.
  - `GET /plugins/{name}/api/data` → `AdminClient::get_data` → `Json(...)`.
  - `PUT /plugins/{name}/api/data` → `AdminClient::set_data` :
    `Ok(())` → `204 No Content` ; `Err(msg)` → `422` + corps `{ error: msg }` ;
    erreur de transport (plugin injoignable/timeout) → `502`.
  - Un `{name}` inconnu ou sans capacité admin → `404`.
- Page de statut (`status.rs`) : pour chaque plugin `admin = true`, un lien
  **interne** vers `/plugins/{name}/` (remplace l'ancien lien externe
  `admin_url`). Un plugin admin dont le client est injoignable : le lien reste
  affiché mais marqué indisponible, cohérent avec la tolérance existante.

## Impact sur le plugin radio

- `web.rs` (serveur axum + `router` + `WebState`) est **supprimé**. La logique
  de validation (`Stations::validate`) et de persistance (`Stations::save`)
  est conservée et rebranchée derrière une implémentation d'`AdminPlugin` :
  - `page()` → `include_str!("index.html")` (inchangé).
  - `get_data()` → les stations courantes sérialisées en JSON.
  - `set_data(v)` → désérialise `v` en `Stations`, valide, persiste, met à
    jour l'état partagé ; `Err("preset en double ou hors bornes")` etc. si
    invalide.
- `index.html` : les appels `fetch` passent de `/api/stations` à
  `./api/data` (relatif à `/plugins/radio/`), et le corps échangé reste le
  même objet stations en JSON.
- Le plugin ne lie plus de port TCP du tout ; les variables `RITORNELLO_RADIO_HTTP`
  disparaissent. `RITORNELLO_RADIO_STATIONS` / `RITORNELLO_RADIO_STATE`
  demeurent.
- `main.rs` du plugin radio lance désormais `run_source_plugin` et
  `run_admin_plugin` en parallèle (deux sockets passés en arguments).

## Sécurité / réseau

Effet de bord bienvenu : le plugin radio n'expose plus rien sur le LAN
(fini le `:8081` accessible de l'extérieur). Toute l'IHM passe par le seul
port du cœur ; le pare-feutrage se réduit à un port. Les sockets de plugin
restent locaux (répertoire runtime), inchangés.

## Tests

- `admin.rs` (proto) : roundtrip JSON des trames `AdminRequest`/`AdminResponse`
  pour chaque variante (comme `source.rs`, `command.rs`).
- SDK : `run_admin_plugin` + `AdminClient` testés bout à bout sur un socket
  réel en tempdir — `GetPage`, `GetData`, `SetData` valide (→ `ok:true`),
  `SetData` invalide (→ `ok:false, error:Some`). Modèle identique aux tests
  `dialogue_requete_reponse` / `display_client_*` existants.
- Cœur : les routes `/plugins/{name}/…` testées via `oneshot` axum (comme le
  fait déjà l'ancien `web.rs`), avec un `AdminClient` branché sur un plugin
  factice — `GET data`, `PUT` valide → 204, `PUT` invalide → 422, nom inconnu
  → 404.
- Radio : les trois tests de validation/persistance existants
  (`get_stations_*`, `put_stations_*`) sont conservés, rebranchés sur
  l'implémentation `AdminPlugin` au lieu du routeur axum supprimé.

## Hors périmètre

- L'internationalisation (packs de langue, sélecteur de langue, `SetLocale`) :
  spec suivante, qui s'appuiera sur cette origine HTTP unique.
- Toute généralisation du protocole admin au-delà des 3 messages (ex. tunnel
  HTTP arbitraire, upload de fichiers) : non nécessaire aujourd'hui, à
  réintroduire dans sa propre spec si un plugin futur le justifie.
- Authentification / contrôle d'accès sur les pages d'admin : inchangé par
  rapport à aujourd'hui (aucune ; réseau de confiance supposé, comme la v1).
