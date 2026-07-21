# Serveur web unique — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Faire du cœur le seul processus à écouter un port TCP, en servant les pages d'admin des plugins via une capacité « admin » transverse acheminée par un second socket Unix, et supprimer le serveur axum du plugin radio.

**Architecture:** Nouveau protocole admin (3 messages requête/réponse corrélés par `id`) dans `ritornello-proto`, servi côté plugin par `run_admin_plugin`/`AdminPlugin` (SDK) sur un socket dédié `--admin-socket`, consommé côté cœur par `AdminClient` (SDK) exposé derrière un trait `AdminBackend` (cœur) que les routes axum `/plugins/{name}/…` interrogent. Le plugin radio troque son serveur axum contre une implémentation d'`AdminPlugin` partageant l'état des stations avec sa partie Source.

**Tech Stack:** Rust, tokio, axum 0.7, sockets Unix, JSON par ligne, serde/serde_json, async-trait.

## Global Constraints

- Le cœur est le **seul** processus à écouter un port TCP (`:8080`). Le plugin radio ne lie plus aucun port.
- La capacité admin est **transverse au genre** : n'importe quel plugin peut la déclarer, indépendamment de Source/Input/Display.
- Transport admin = **second socket dédié** (`--admin-socket`), distinct du socket de genre ; le socket de genre reste intact.
- Le cœur ne connaît **jamais** le schéma des données d'un plugin : il transporte du JSON opaque (`serde_json::Value`), la validation reste dans le plugin.
- Le champ `admin_url: Option<String>` de `plugins.toml` est **remplacé** par `admin: bool` (`#[serde(default)]`, défaut `false`).
- Convention de nommage des sockets : genre = `{runtime_dir}/{name}.sock`, admin = `{runtime_dir}/{name}-admin.sock`.
- Zéro nouvelle fonctionnalité utilisateur : consolidation d'architecture, la page radio garde ses fonctions.
- Ne jamais éditer `src/main/javagen/` (sans objet ici, mais règle projet globale).
- Tests unitaires en `#[cfg(test)] mod` dans le fichier testé (convention Rust déjà en place dans ce dépôt).

---

## File Structure

- `crates/ritornello-proto/src/admin.rs` (créer) — types du protocole admin.
- `crates/ritornello-proto/src/lib.rs` (modifier) — exporter le module admin.
- `crates/ritornello-proto/Cargo.toml` (modifier) — `serde_json` passe en dépendance réelle.
- `crates/ritornello-plugin-sdk/src/server.rs` (modifier) — `AdminPlugin` + `run_admin_plugin`.
- `crates/ritornello-plugin-sdk/src/client.rs` (modifier) — `AdminClient`.
- `crates/ritornello-plugin-sdk/src/lib.rs` (modifier) — ré-exports.
- `crates/ritornello-core/src/plugins.rs` (modifier) — `admin: bool`, `spawn` avec admin-socket.
- `crates/ritornello-core/src/status.rs` (modifier) — champ `admin`, lien interne, `admin_backends` dans `AppState`, routes admin dans `router`.
- `crates/ritornello-core/src/admin.rs` (créer) — trait `AdminBackend`, impl pour `AdminClient`, handlers axum.
- `crates/ritornello-core/src/main.rs` (modifier) — `mod admin;`, connexion des `AdminClient`, câblage `AppState`.
- `crates/ritornello-plugin-radio/src/web.rs` (supprimer).
- `crates/ritornello-plugin-radio/src/admin.rs` (créer) — `RadioAdmin: AdminPlugin`.
- `crates/ritornello-plugin-radio/src/main.rs` (modifier) — double socket, plus de serveur web.
- `crates/ritornello-plugin-radio/src/index.html` (modifier) — `fetch` vers `./api/data`.
- `crates/ritornello-plugin-radio/Cargo.toml` (modifier) — retrait d'`axum`, `tower`, `http-body-util`.
- `deploy/plugins.example.toml`, `README.md`, `deploy/deploy.sh` (modifier) — doc/config.

---

### Task 1: Protocole admin (ritornello-proto)

**Files:**
- Create: `crates/ritornello-proto/src/admin.rs`
- Modify: `crates/ritornello-proto/src/lib.rs`
- Modify: `crates/ritornello-proto/Cargo.toml`

**Interfaces:**
- Produces:
  - `AdminReq` enum : `GetPage`, `GetData`, `SetData(serde_json::Value)` — `#[serde(tag = "req", content = "arg")]`.
  - `AdminRequest { id: u64, #[serde(flatten)] req: AdminReq }`.
  - `AdminResult` enum : `Page(String)`, `Data(serde_json::Value)`, `Set { ok: bool, error: Option<String> }` — `#[serde(tag = "kind", content = "data")]`.
  - `AdminResponse { id: u64, result: AdminResult }`.

- [ ] **Step 1: Promouvoir serde_json en dépendance réelle**

Dans `crates/ritornello-proto/Cargo.toml`, ajouter `serde_json` à `[dependencies]` (il n'était qu'en dev-dependency). Résultat attendu du fichier :

```toml
[package]
name = "ritornello-proto"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
serde_json = "1"
```

- [ ] **Step 2: Écrire le module admin avec ses tests de roundtrip**

Créer `crates/ritornello-proto/src/admin.rs` :

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    GetPage,
    GetData,
    SetData(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: AdminReq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AdminResult {
    Page(String),
    Data(serde_json::Value),
    Set { ok: bool, error: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminResponse {
    pub id: u64,
    pub result: AdminResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_getpage_roundtrip() {
        let r = AdminRequest { id: 1, req: AdminReq::GetPage };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":1,"req":"GetPage"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.req, AdminReq::GetPage);
    }

    #[test]
    fn request_setdata_porte_le_json_opaque() {
        let r = AdminRequest { id: 2, req: AdminReq::SetData(serde_json::json!({"stations": []})) };
        let json = serde_json::to_string(&r).unwrap();
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 2);
        assert_eq!(back.req, AdminReq::SetData(serde_json::json!({"stations": []})));
    }

    #[test]
    fn response_page_roundtrip() {
        let r = AdminResponse { id: 3, result: AdminResult::Page("<h1>x</h1>".into()) };
        let json = serde_json::to_string(&r).unwrap();
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 3);
        assert_eq!(back.result, AdminResult::Page("<h1>x</h1>".into()));
    }

    #[test]
    fn response_set_roundtrip() {
        let r = AdminResponse { id: 4, result: AdminResult::Set { ok: false, error: Some("nope".into()) } };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":4,"result":{"kind":"Set","data":{"ok":false,"error":"nope"}}}"#);
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, AdminResult::Set { ok: false, error: Some("nope".into()) });
    }
}
```

- [ ] **Step 3: Exporter le module**

Dans `crates/ritornello-proto/src/lib.rs`, ajouter le module et les ré-exports :

```rust
pub mod admin;
pub mod command;
pub mod source;
pub mod view;

pub use admin::{AdminReq, AdminRequest, AdminResponse, AdminResult};
pub use command::Command;
pub use source::{SourceAction, SourceMessage, SourceReq, SourceRequest};
pub use view::View;
```

- [ ] **Step 4: Lancer les tests**

Run (WSL) : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-proto"`
Expected : PASS (les 4 nouveaux tests admin + les tests existants command/source/view verts).

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-proto
git commit -m "feat(proto): protocole admin (GetPage/GetData/SetData, JSON opaque)"
```

---

### Task 2: Serveur admin dans le SDK (AdminPlugin + run_admin_plugin)

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/server.rs`
- Modify: `crates/ritornello-plugin-sdk/src/lib.rs`

**Interfaces:**
- Consumes: `ritornello_proto::{AdminReq, AdminRequest, AdminResponse, AdminResult}` (Task 1).
- Produces:
  - `trait AdminPlugin { fn page(&self) -> &'static str; async fn get_data(&self) -> serde_json::Value; async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>; }`
  - `async fn run_admin_plugin(plugin: impl AdminPlugin, socket_path: &Path) -> Result<()>`

- [ ] **Step 1: Écrire le trait, la fonction et le test serveur**

À la fin de `crates/ritornello-plugin-sdk/src/server.rs` (avant les `#[cfg(test)]` existants, ou après — l'ordre n'importe pas), ajouter les imports en tête du fichier si absents (`AdminReq`, `AdminRequest`, `AdminResponse`, `AdminResult` depuis `ritornello_proto`) puis :

```rust
use ritornello_proto::{AdminReq, AdminRequest, AdminResponse, AdminResult};

#[async_trait::async_trait]
pub trait AdminPlugin: Send + 'static {
    /// HTML statique de la page d'admin (servi tel quel par le cœur).
    fn page(&self) -> &'static str;
    /// État courant, sérialisé en JSON opaque pour le cœur.
    async fn get_data(&self) -> serde_json::Value;
    /// Valide et persiste ; `Err(msg)` = donnée refusée (msg montré à l'utilisateur).
    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>;
}

/// Lie `socket_path`, accepte une connexion (le cœur), puis traite les
/// requêtes admin (requête/réponse corrélée par `id`) jusqu'à fermeture.
pub async fn run_admin_plugin(mut plugin: impl AdminPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("liaison de {}", socket_path.display()))?;
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let req: AdminRequest = serde_json::from_str(&line)
            .with_context(|| format!("requete admin invalide: {line}"))?;
        let result = match req.req {
            AdminReq::GetPage => AdminResult::Page(plugin.page().to_string()),
            AdminReq::GetData => AdminResult::Data(plugin.get_data().await),
            AdminReq::SetData(data) => match plugin.set_data(data).await {
                Ok(()) => AdminResult::Set { ok: true, error: None },
                Err(msg) => AdminResult::Set { ok: false, error: Some(msg) },
            },
        };
        let resp = AdminResponse { id: req.id, result };
        write.write_all(format!("{}\n", serde_json::to_string(&resp)?).as_bytes()).await?;
    }
    Ok(())
}

#[cfg(test)]
mod admin_server_tests {
    use super::*;
    use ritornello_proto::{AdminResponse, AdminResult};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    struct FakeAdmin {
        data: serde_json::Value,
    }

    #[async_trait::async_trait]
    impl AdminPlugin for FakeAdmin {
        fn page(&self) -> &'static str {
            "<h1>hello</h1>"
        }
        async fn get_data(&self) -> serde_json::Value {
            self.data.clone()
        }
        async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
            if data.get("bad").is_some() {
                return Err("refus".into());
            }
            self.data = data;
            Ok(())
        }
    }

    #[tokio::test]
    async fn getpage_getdata_setdata_dialogue() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let socket_srv = socket.clone();
        tokio::spawn(async move {
            run_admin_plugin(FakeAdmin { data: serde_json::json!({"n": 1}) }, &socket_srv)
                .await
                .unwrap();
        });

        let mut stream = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = stream.expect("connexion admin").into_split();
        let mut lines = BufReader::new(read).lines();

        write.write_all(b"{\"id\":1,\"req\":\"GetPage\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert_eq!(r.id, 1);
        assert!(matches!(r.result, AdminResult::Page(ref h) if h.contains("hello")));

        write.write_all(b"{\"id\":2,\"req\":\"GetData\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Data(ref v) if v["n"] == 1));

        write.write_all(b"{\"id\":3,\"req\":\"SetData\",\"arg\":{\"bad\":true}}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Set { ok: false, .. }));
    }
}
```

Note : `UnixListener`, `BufReader`, `AsyncBufReadExt`, `AsyncWriteExt`, `Context`, `Result`, `Path` sont déjà importés en tête de `server.rs` (utilisés par `run_source_plugin`). N'ajouter que `use ritornello_proto::{AdminReq, ...}`.

- [ ] **Step 2: Ré-exporter dans lib.rs**

Dans `crates/ritornello-plugin-sdk/src/lib.rs` :

```rust
pub mod client;
pub mod server;

pub use client::{run_input_client, DisplayClient, SourceClient};
pub use server::{
    run_admin_plugin, run_display_plugin, run_input_plugin, AdminPlugin, DisplayPlugin,
    InputPlugin, SourceOutcome, SourcePlugin,
};
```

(`AdminClient` sera ajouté à ce `pub use client::{…}` en Task 3.)

- [ ] **Step 3: Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk"`
Expected : PASS (nouveau `admin_server_tests::getpage_getdata_setdata_dialogue` + tests existants source/display/input verts).

- [ ] **Step 4: Commit**

```bash
git add crates/ritornello-plugin-sdk/src/server.rs crates/ritornello-plugin-sdk/src/lib.rs
git commit -m "feat(sdk): AdminPlugin + run_admin_plugin (serveur admin sur socket dedie)"
```

---

### Task 3: Client admin dans le SDK (AdminClient)

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/client.rs`
- Modify: `crates/ritornello-plugin-sdk/src/lib.rs`

**Interfaces:**
- Consumes: `ritornello_proto::{AdminReq, AdminRequest, AdminResponse, AdminResult}` (Task 1) ; `connect_with_retry` (déjà présent dans `client.rs`).
- Produces:
  - `struct AdminClient` avec `async fn connect(socket_path: &Path) -> Result<Arc<Self>>`, `async fn get_page(&self) -> Result<String>`, `async fn get_data(&self) -> Result<serde_json::Value>`, `async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>` (résultat externe = transport/timeout ; interne = verdict de validation du plugin).

- [ ] **Step 1: Écrire AdminClient et son test**

En tête de `crates/ritornello-plugin-sdk/src/client.rs`, ajouter aux imports `ritornello_proto` les types admin :

```rust
use ritornello_proto::{
    AdminReq, AdminRequest, AdminResponse, AdminResult, Command, SourceAction, SourceMessage,
    SourceReq, SourceRequest, View,
};
```

Puis ajouter la structure (après `DisplayClient`, avant `run_input_client` par exemple) :

```rust
pub struct AdminClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AdminResult>>>>,
    next_id: AtomicU64,
}

impl AdminClient {
    pub async fn connect(socket_path: &Path) -> Result<Arc<Self>> {
        let stream = connect_with_retry(socket_path).await?;
        let (read, write) = stream.into_split();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AdminResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self {
            writer: Mutex::new(write),
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(resp) = serde_json::from_str::<AdminResponse>(&line) else { continue };
                if let Some(tx) = pending.lock().await.remove(&resp.id) {
                    let _ = tx.send(resp.result);
                }
            }
            tracing::warn!("connexion au plugin admin fermee");
        });
        Ok(client)
    }

    async fn request(&self, req: AdminReq) -> Result<AdminResult> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = AdminRequest { id, req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => bail!("plugin admin: reponse abandonnee"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("plugin admin: timeout de requete")
            }
        }
    }

    pub async fn get_page(&self) -> Result<String> {
        match self.request(AdminReq::GetPage).await? {
            AdminResult::Page(html) => Ok(html),
            other => bail!("reponse admin inattendue pour GetPage: {other:?}"),
        }
    }

    pub async fn get_data(&self) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetData).await? {
            AdminResult::Data(v) => Ok(v),
            other => bail!("reponse admin inattendue pour GetData: {other:?}"),
        }
    }

    pub async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        match self.request(AdminReq::SetData(data)).await? {
            AdminResult::Set { ok: true, .. } => Ok(Ok(())),
            AdminResult::Set { ok: false, error } => Ok(Err(error.unwrap_or_default())),
            other => bail!("reponse admin inattendue pour SetData: {other:?}"),
        }
    }
}
```

Ajouter le test (dans le `#[cfg(test)] mod tests` existant de `client.rs`, ou un nouveau module ; ici un serveur factice brut qui répond des lignes `AdminResponse` corrélées) :

```rust
#[tokio::test]
async fn admin_client_correle_les_reponses() {
    use ritornello_proto::AdminResponse;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("admin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        // 1re requête (get_page, id=1)
        let _ = lines.next_line().await.unwrap().unwrap();
        write
            .write_all(b"{\"id\":1,\"result\":{\"kind\":\"Page\",\"data\":\"<h1>hi</h1>\"}}\n")
            .await
            .unwrap();
        // 2e requête (set_data, id=2)
        let _ = lines.next_line().await.unwrap().unwrap();
        write
            .write_all(b"{\"id\":2,\"result\":{\"kind\":\"Set\",\"data\":{\"ok\":false,\"error\":\"nope\"}}}\n")
            .await
            .unwrap();
        let _ = &write; // garde l'écriture vivante
        std::future::pending::<()>().await;
    });

    let client = AdminClient::connect(&socket).await.unwrap();
    assert_eq!(client.get_page().await.unwrap(), "<h1>hi</h1>");
    let verdict = client.set_data(serde_json::json!({})).await.unwrap();
    assert_eq!(verdict, Err("nope".to_string()));
}
```

Note : le module `tests` de `client.rs` importe déjà `tokio::net::UnixListener`, `BufReader`, `AsyncBufReadExt`, `AsyncWriteExt`. `AdminResult` doit dériver `Debug` (fait en Task 1) pour les messages `bail!`.

- [ ] **Step 2: Ré-exporter AdminClient**

Dans `crates/ritornello-plugin-sdk/src/lib.rs` :

```rust
pub use client::{run_input_client, AdminClient, DisplayClient, SourceClient};
```

- [ ] **Step 3: Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk"`
Expected : PASS (`admin_client_correle_les_reponses` + tous les autres).

- [ ] **Step 4: Commit**

```bash
git add crates/ritornello-plugin-sdk/src/client.rs crates/ritornello-plugin-sdk/src/lib.rs
git commit -m "feat(sdk): AdminClient (connexion + correlation par id du protocole admin)"
```

---

### Task 4: Cœur — champ `admin`, spawn du socket admin (sans encore le servir)

**Files:**
- Modify: `crates/ritornello-core/src/plugins.rs`
- Modify: `crates/ritornello-core/src/status.rs`
- Modify: `crates/ritornello-core/src/main.rs`

**Interfaces:**
- Consumes: rien de nouveau (types déjà en place).
- Produces:
  - `PluginConfig.admin: bool` (remplace `admin_url`).
  - `fn spawn(exec: &str, socket_path: &Path, admin_socket: Option<&Path>) -> Result<tokio::process::Child>`.
  - `PluginStatus.admin: bool` (remplace `admin_url`).

Cette tâche renomme le champ dans les 3 fichiers qui le référencent pour garder le crate `ritornello-core` compilable (il n'a pas de `lib.rs` : `cargo test -p ritornello-core` compile `main.rs`). Elle ne connecte pas encore d'`AdminClient` (Task 5).

- [ ] **Step 1: plugins.rs — champ `admin` et `spawn` avec socket admin**

Dans `crates/ritornello-core/src/plugins.rs`, remplacer le champ et la fonction `spawn` :

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub kind: PluginKind,
    pub exec: String,
    #[serde(default)]
    pub admin: bool,
}
```

```rust
/// Spawn un plugin en lui passant le chemin de la socket de genre qu'il doit
/// lier, et — s'il déclare `admin = true` — un `--admin-socket`.
pub fn spawn(exec: &str, socket_path: &Path, admin_socket: Option<&Path>) -> Result<tokio::process::Child> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let mut cmd = tokio::process::Command::new(exec);
    cmd.arg("--socket").arg(socket_path);
    if let Some(admin) = admin_socket {
        let _ = std::fs::remove_file(admin);
        cmd.arg("--admin-socket").arg(admin);
    }
    Ok(cmd.kill_on_drop(true).spawn()?)
}
```

Mettre à jour le test `charge_un_manifeste_toml` : remplacer `admin_url = "http://raspberrypi.local:8081"` par `admin = true` et l'assertion :

```rust
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
kind = "source"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"

[[plugin]]
name = "console"
kind = "display"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-console"
admin = true
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 2);
        assert_eq!(m.plugins[0].name, "radio");
        assert_eq!(m.plugins[0].kind, PluginKind::Source);
        assert!(!m.plugins[0].admin);
        assert_eq!(m.plugins[1].kind, PluginKind::Display);
        assert!(m.plugins[1].admin);
```

- [ ] **Step 2: status.rs — champ `admin` et lien interne**

Dans `crates/ritornello-core/src/status.rs`, remplacer `admin_url: Option<String>` par `admin: bool` dans `PluginStatus`, son `Deserialize` manuel (`RawPlugin`), et le rendu.

`PluginStatus` :

```rust
#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub name: String,
    pub kind: String,
    pub connected: bool,
    pub admin: bool,
}
```

Dans `impl Deserialize for StatusState`, `RawPlugin` et le mapping :

```rust
        #[derive(serde::Deserialize)]
        struct RawPlugin {
            name: String,
            kind: String,
            connected: bool,
            admin: bool,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(StatusState {
            plugins: raw
                .plugins
                .into_iter()
                .map(|p| PluginStatus { name: p.name, kind: p.kind, connected: p.connected, admin: p.admin })
                .collect(),
            active_source: raw.active_source,
        })
```

Dans `status_page`, remplacer le calcul de `lien` :

```rust
        let etat = if p.connected { "connecté" } else { "indisponible" };
        let lien = if p.admin {
            format!("<a href=\"/plugins/{}/\">admin</a>", escape_html(&p.name))
        } else {
            "-".to_string()
        };
```

Mettre à jour `sample()` dans les tests de `status.rs` :

```rust
    fn sample() -> StatusState {
        StatusState {
            plugins: vec![
                PluginStatus { name: "radio".into(), kind: "source".into(), connected: true, admin: true },
                PluginStatus { name: "cd".into(), kind: "source".into(), connected: false, admin: false },
            ],
            active_source: "radio".into(),
        }
    }
```

Ajouter un test vérifiant le lien interne :

```rust
    #[tokio::test]
    async fn page_statut_lien_admin_interne() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("href=\"/plugins/radio/\""));
        assert!(!html.contains(":8081"));
    }
```

- [ ] **Step 3: main.rs — passer le socket admin à spawn, `admin` dans PluginStatus**

Dans `crates/ritornello-core/src/main.rs`, dans la boucle `for p in &manifest.plugins`, calculer le socket admin et l'utiliser à l'appel de `spawn`, et remplacer partout `admin_url` par `admin` dans la construction des `PluginStatus`.

Remplacer le début de la boucle et l'appel `spawn` :

```rust
    for p in &manifest.plugins {
        let socket_path = PathBuf::from(format!("{runtime_dir}/{}.sock", p.name));
        let admin_socket = p
            .admin
            .then(|| PathBuf::from(format!("{runtime_dir}/{}-admin.sock", p.name)));
        match plugins::spawn(&p.exec, &socket_path, admin_socket.as_deref()) {
```

Dans les bras `PluginKind::Source` et `PluginKind::Display`, la variable capturée `admin_url` (et le tuple renvoyé) devient `admin` (un `bool`). Remplacer :
- `let admin_url = p.admin_url.clone();` → `let admin = p.admin;`
- dans le tuple renvoyé par la tâche : `(name, admin_url, result)` → `(name, admin, result)`
- à la réception : `let (name, admin_url, result) = handle.await…` → `let (name, admin, result) = handle.await…`
- `PluginStatus { …, admin_url }` → `PluginStatus { …, admin }`

Dans le bras `PluginKind::Input` et le bras d'erreur `Err(e)`, remplacer `admin_url: p.admin_url.clone()` par `admin: p.admin`.

Après ces remplacements, les 6 constructions de `PluginStatus` (input, bras d'erreur de spawn, source connectée, source indisponible, display connecté, display indisponible) utilisent `admin: …` / le `admin` du tuple. Aucune connexion `AdminClient` n'est ajoutée dans cette tâche.

- [ ] **Step 4: Compiler et tester le cœur**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core"`
Expected : PASS (le crate compile ; `charge_un_manifeste_toml`, `page_statut_lien_admin_interne` et les tests status/audio existants verts).

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/plugins.rs crates/ritornello-core/src/status.rs crates/ritornello-core/src/main.rs
git commit -m "refactor(core): champ admin bool (remplace admin_url), spawn du socket admin, lien de statut interne"
```

---

### Task 5: Cœur — servir les pages d'admin (trait AdminBackend + routes + câblage)

**Files:**
- Create: `crates/ritornello-core/src/admin.rs`
- Modify: `crates/ritornello-core/src/status.rs`
- Modify: `crates/ritornello-core/src/main.rs`

**Interfaces:**
- Consumes: `ritornello_plugin_sdk::AdminClient` (Task 3) ; `crate::status::AppState` (étendu ici).
- Produces:
  - `trait AdminBackend { async fn page(&self) -> Result<String>; async fn get_data(&self) -> Result<serde_json::Value>; async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>; }` + `impl AdminBackend for AdminClient`.
  - Handlers `admin_page`, `admin_get_data`, `admin_put_data`.
  - `AppState.admin_backends: Arc<HashMap<String, Arc<dyn AdminBackend>>>`.
  - Routes `/plugins/:name/` et `/plugins/:name/api/data` sur le routeur du cœur.

- [ ] **Step 1: Créer le module admin (trait, impl, handlers) avec ses tests**

Créer `crates/ritornello-core/src/admin.rs` :

```rust
use crate::status::AppState;
use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::Json;

/// Abstraction des opérations d'admin dont les routes du cœur ont besoin.
/// Implémentée par `AdminClient` (IPC réel) ; un faux l'implémente en test.
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn page(&self) -> Result<String>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>;
}

#[async_trait::async_trait]
impl AdminBackend for ritornello_plugin_sdk::AdminClient {
    async fn page(&self) -> Result<String> {
        self.get_page().await
    }
    async fn get_data(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_data(self).await
    }
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>> {
        ritornello_plugin_sdk::AdminClient::set_data(self, data).await
    }
}

pub async fn admin_page(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.page().await {
            Ok(html) => Html(html).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response(),
        },
    }
}

pub async fn admin_get_data(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.get_data().await {
            Ok(value) => Json(value).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response(),
        },
    }
}

pub async fn admin_put_data(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(data): Json<serde_json::Value>,
) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.set_data(data).await {
            Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
            Ok(Err(msg)) => (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{router, AppState, LogBuffer, StatusState};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    struct Fake {
        reject: bool,
        down: bool,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Fake {
        async fn page(&self) -> Result<String> {
            if self.down { anyhow::bail!("down") }
            Ok("<h1>radio</h1>".into())
        }
        async fn get_data(&self) -> Result<serde_json::Value> {
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "stations": [] }))
        }
        async fn set_data(&self, _data: serde_json::Value) -> Result<Result<(), String>> {
            if self.down { anyhow::bail!("down") }
            Ok(if self.reject { Err("présélection en double".into()) } else { Ok(()) })
        }
    }

    fn state_with(fake: Fake) -> AppState {
        let (audio_tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut backends: HashMap<String, Arc<dyn AdminBackend>> = HashMap::new();
        backends.insert("radio".into(), Arc::new(fake));
        AppState {
            status: Arc::new(tokio::sync::RwLock::new(StatusState { plugins: vec![], active_source: "radio".into() })),
            logs: Arc::new(LogBuffer::new(10)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
            admin_backends: Arc::new(backends),
        }
    }

    #[tokio::test]
    async fn get_page_sert_le_html() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app.oneshot(Request::get("/plugins/radio/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("radio"));
    }

    #[tokio::test]
    async fn get_data_relaie_le_json() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["stations"].is_array());
    }

    #[tokio::test]
    async fn put_data_valide_renvoie_204() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"stations":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn put_data_invalide_renvoie_422_avec_message() {
        let app = router(state_with(Fake { reject: true, down: false }));
        let resp = app
            .oneshot(
                Request::put("/plugins/radio/api/data")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"stations":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "présélection en double");
    }

    #[tokio::test]
    async fn plugin_inconnu_renvoie_404() {
        let app = router(state_with(Fake { reject: false, down: false }));
        let resp = app.oneshot(Request::get("/plugins/inconnu/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn plugin_injoignable_renvoie_502() {
        let app = router(state_with(Fake { reject: false, down: true }));
        let resp = app.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
```

- [ ] **Step 2: status.rs — champ AppState + routes + helpers de test**

Dans `crates/ritornello-core/src/status.rs`, ajouter le champ à `AppState` (noter l'objet-trait `Arc<dyn …>`) :

```rust
#[derive(Clone)]
pub struct AppState {
    pub status: Arc<RwLock<StatusState>>,
    pub logs: Arc<LogBuffer>,
    pub audio_current: Arc<RwLock<Option<String>>>,
    pub audio_tx: mpsc::Sender<String>,
    pub admin_backends: Arc<std::collections::HashMap<String, Arc<dyn crate::admin::AdminBackend>>>,
}
```

Ajouter les routes dans `router` :

```rust
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status_page))
        .route("/api/status", get(status_json))
        .route("/api/audio-output", get(audio_output_json).put(audio_output_put))
        .route("/plugins/:name/", get(crate::admin::admin_page))
        .route(
            "/plugins/:name/api/data",
            get(crate::admin::admin_get_data).put(crate::admin::admin_put_data),
        )
        .with_state(state)
}
```

Mettre à jour les deux constructeurs d'`AppState` des tests (`app_state` et `app_state_with_audio`) pour inclure `admin_backends: Arc::new(std::collections::HashMap::new())` :

```rust
    fn app_state() -> AppState {
        let (audio_tx, _audio_rx) = tokio::sync::mpsc::channel(4);
        AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(None)),
            audio_tx,
            admin_backends: Arc::new(std::collections::HashMap::new()),
        }
    }

    fn app_state_with_audio() -> (AppState, tokio::sync::mpsc::Receiver<String>) {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
        let state = AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(Some("default".to_string()))),
            audio_tx,
            admin_backends: Arc::new(std::collections::HashMap::new()),
        };
        (state, audio_rx)
    }
```

- [ ] **Step 3: main.rs — déclarer le module, connecter les AdminClient, câbler AppState**

Dans `crates/ritornello-core/src/main.rs` :

1. Ajouter `mod admin;` en tête (après `mod audio_output;`).
2. Ajouter `use std::collections::HashMap;` est déjà présent. Ajouter au besoin `use crate::admin::AdminBackend;`.
3. Avant la boucle de spawn, déclarer : `let mut admin_connects = Vec::new();`
4. Dans la boucle, après le `match plugins::spawn(...) { Ok(child) => { children.push(child); … } }`, à l'intérieur du bras `Ok(child)`, **avant** le `match p.kind`, ajouter la connexion admin si déclarée :

```rust
                if p.admin {
                    let name = p.name.clone();
                    let asock = PathBuf::from(format!("{runtime_dir}/{}-admin.sock", p.name));
                    admin_connects.push(tokio::spawn(async move {
                        let result = ritornello_plugin_sdk::AdminClient::connect(&asock).await;
                        (name, result)
                    }));
                }
```

5. Après les blocs qui résolvent `source_connects` et `display_connect` (juste avant `if sources.is_empty()`), résoudre les connexions admin :

```rust
    let mut admin_backends: HashMap<String, Arc<dyn admin::AdminBackend>> = HashMap::new();
    for handle in admin_connects {
        let (name, result) = handle.await.context("tache de connexion admin interrompue")?;
        match result {
            Ok(client) => {
                admin_backends.insert(name, client);
            }
            Err(e) => tracing::warn!("plugin admin {name} injoignable: {e}"),
        }
    }
```

(`client` est `Arc<AdminClient>` ; l'insertion dans `HashMap<String, Arc<dyn AdminBackend>>` fait la coercition automatiquement.)

6. À la construction de `AppState` (bloc de la page de statut), ajouter le champ :

```rust
        let app = status::router(AppState {
            status: status_state.clone(),
            logs: log_buffer.clone(),
            audio_current: audio_current.clone(),
            audio_tx: audio_tx.clone(),
            admin_backends: Arc::new(admin_backends),
        });
```

- [ ] **Step 4: Compiler et tester le cœur**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core"`
Expected : PASS (les 6 tests de `admin::tests` + tous les tests status/plugins existants verts).

- [ ] **Step 5: Vérifier clippy sur le cœur**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-core -- -D warnings"`
Expected : aucun warning.

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-core/src/admin.rs crates/ritornello-core/src/status.rs crates/ritornello-core/src/main.rs
git commit -m "feat(core): sert les pages d'admin des plugins via AdminBackend + routes /plugins/{name}"
```

---

### Task 6: Plugin radio — remplacer le serveur axum par AdminPlugin

**Files:**
- Delete: `crates/ritornello-plugin-radio/src/web.rs`
- Create: `crates/ritornello-plugin-radio/src/admin.rs`
- Modify: `crates/ritornello-plugin-radio/src/main.rs`
- Modify: `crates/ritornello-plugin-radio/src/index.html`
- Modify: `crates/ritornello-plugin-radio/Cargo.toml`

**Interfaces:**
- Consumes: `ritornello_plugin_sdk::{run_admin_plugin, run_source_plugin, AdminPlugin, SourcePlugin, SourceOutcome}` ; `crate::config::Stations`.
- Produces: `struct RadioAdmin` implémentant `AdminPlugin` ; un `main` lançant les deux serveurs de plugin en parallèle.

- [ ] **Step 1: Supprimer le serveur web**

```bash
git rm crates/ritornello-plugin-radio/src/web.rs
```

- [ ] **Step 2: Créer RadioAdmin avec ses tests**

Créer `crates/ritornello-plugin-radio/src/admin.rs` :

```rust
use crate::config::Stations;
use ritornello_plugin_sdk::AdminPlugin;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct RadioAdmin {
    pub stations_path: PathBuf,
    pub stations: Arc<RwLock<Stations>>,
}

#[async_trait::async_trait]
impl AdminPlugin for RadioAdmin {
    fn page(&self) -> &'static str {
        include_str!("index.html")
    }

    async fn get_data(&self) -> serde_json::Value {
        serde_json::to_value(&*self.stations.read().await).unwrap_or(serde_json::Value::Null)
    }

    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String> {
        let stations: Stations =
            serde_json::from_value(data).map_err(|e| format!("JSON invalide : {e}"))?;
        stations.validate().map_err(|e| e.to_string())?;
        stations.save(&self.stations_path).map_err(|e| e.to_string())?;
        *self.stations.write().await = stations;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Station;

    fn admin(dir: &std::path::Path) -> RadioAdmin {
        let path = dir.join("stations.toml");
        let stations = Stations {
            stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }],
        };
        stations.save(&path).unwrap();
        RadioAdmin { stations_path: path, stations: Arc::new(RwLock::new(stations)) }
    }

    #[tokio::test]
    async fn get_data_renvoie_les_stations() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let v = a.get_data().await;
        assert_eq!(v["stations"][0]["name"], "FIP");
    }

    #[tokio::test]
    async fn set_data_valide_persiste_et_met_a_jour() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let nouveau = serde_json::json!({ "stations": [{ "name": "Inter", "url": "http://inter", "preset": 2 }] });
        assert!(a.set_data(nouveau).await.is_ok());
        assert_eq!(a.stations.read().await.stations[0].name, "Inter");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "Inter");
    }

    #[tokio::test]
    async fn set_data_invalide_renvoie_erreur_et_ne_persiste_pas() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = admin(dir.path());
        let mauvais = serde_json::json!({ "stations": [{ "name": "X", "url": "http://x", "preset": 12 }] });
        assert!(a.set_data(mauvais).await.is_err());
        // l'état partagé et le disque restent inchangés
        assert_eq!(a.stations.read().await.stations[0].name, "FIP");
        assert_eq!(Stations::load(&a.stations_path).unwrap().stations[0].name, "FIP");
    }
}
```

- [ ] **Step 3: Réécrire main.rs (double socket, plus de serveur web)**

Remplacer intégralement `crates/ritornello-plugin-radio/src/main.rs` par :

```rust
mod admin;
mod config;
mod state;

use crate::admin::RadioAdmin;
use anyhow::Result;
use config::Stations;
use ritornello_plugin_sdk::{run_admin_plugin, run_source_plugin, SourceOutcome, SourcePlugin};
use ritornello_proto::{SourceAction, View};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn arg_value(flag: &str) -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == flag).map(|i| PathBuf::from(&args[i + 1]))
}

struct RadioSource {
    state_path: PathBuf,
    stations: Arc<RwLock<Stations>>,
    preset: u8,
}

impl RadioSource {
    fn view_for(&self, preset: u8, status: &str) -> View {
        View { line1: format!("RADIO  P{preset}"), line2: status.to_string(), line3: String::new() }
    }

    async fn play_preset(&mut self, n: u8) -> SourceOutcome {
        let stations = self.stations.read().await;
        if let Some(st) = stations.by_preset(n) {
            self.preset = n;
            let _ = state::save(&self.state_path, &state::PluginState { preset: n });
            SourceOutcome {
                action: SourceAction::Play { uri: st.url.clone() },
                view: Some(View { line1: format!("RADIO  P{n}"), line2: st.name.clone(), line3: String::new() }),
            }
        } else {
            SourceOutcome { action: SourceAction::Noop, view: Some(self.view_for(self.preset, "présélection vide")) }
        }
    }
}

#[async_trait::async_trait]
impl SourcePlugin for RadioSource {
    async fn activate(&mut self) -> SourceOutcome {
        let preset = self.preset;
        self.play_preset(preset).await
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Stop, view: None }
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        self.play_preset(n).await
    }
    async fn next(&mut self) -> SourceOutcome {
        let next = self.stations.read().await.next_preset(self.preset);
        match next {
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome { action: SourceAction::Noop, view: None },
        }
    }
    async fn prev(&mut self) -> SourceOutcome {
        let prev = self.stations.read().await.prev_preset(self.preset);
        match prev {
            Some(n) => self.play_preset(n).await,
            None => SourceOutcome { action: SourceAction::Noop, view: None },
        }
    }
    async fn next_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn prev_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn eject(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = arg_value("--socket").expect("--socket <path> requis");
    let admin_socket = arg_value("--admin-socket").expect("--admin-socket <path> requis");
    let stations_path = PathBuf::from(env_or("RITORNELLO_RADIO_STATIONS", "/etc/ritornello/stations.toml"));
    let state_path = PathBuf::from(env_or("RITORNELLO_RADIO_STATE", "/var/lib/ritornello/plugin-radio.json"));

    let stations = Stations::load(&stations_path).unwrap_or_else(|e| {
        tracing::warn!("stations.toml invalide ou absent ({e}) : demarrage sans stations");
        Stations::default()
    });
    let preset = state::load(&state_path).preset;
    let stations_shared = Arc::new(RwLock::new(stations));

    let source = RadioSource { state_path, stations: stations_shared.clone(), preset };
    let admin = RadioAdmin { stations_path, stations: stations_shared };

    tokio::try_join!(
        run_source_plugin(source, &socket_path),
        run_admin_plugin(admin, &admin_socket),
    )?;
    Ok(())
}
```

- [ ] **Step 4: Mettre à jour index.html (fetch relatif ./api/data)**

Dans `crates/ritornello-plugin-radio/src/index.html`, remplacer les URLs d'appel `fetch`. Les appels visaient `/api/stations` ; la page étant désormais servie sous `/plugins/radio/`, utiliser un chemin **relatif** `./api/data` pour le GET et le PUT.

- Remplacer tout `fetch('/api/stations'` par `fetch('./api/data'`.
- Remplacer tout `fetch("/api/stations"` par `fetch("./api/data"`.

Le corps JSON échangé (objet `{ stations: [...] }`) est inchangé — c'est exactement ce que sérialise/désérialise `Stations`.

- [ ] **Step 5: Nettoyer Cargo.toml du plugin radio**

Dans `crates/ritornello-plugin-radio/Cargo.toml`, retirer les dépendances devenues inutiles (`axum` en dépendance ; `tower` et `http-body-util` en dev-dependencies, qui servaient aux tests de `web.rs`). Résultat attendu :

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
ritornello-proto = { path = "../ritornello-proto" }
ritornello-plugin-sdk = { path = "../ritornello-plugin-sdk" }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 6: Compiler et tester le plugin radio**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Expected : PASS (les 3 tests de `admin::tests` + les tests `config`/`state` existants ; plus aucun test `web`).

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-plugin-radio
git commit -m "refactor(radio): remplace le serveur axum par AdminPlugin (double socket, page servie par le coeur)"
```

---

### Task 7: Déploiement et documentation

**Files:**
- Modify: `deploy/plugins.example.toml`
- Modify: `README.md`
- Modify: `deploy/deploy.sh` (si une variable radio HTTP y figure)

**Interfaces:** aucune (config + docs).

- [ ] **Step 1: plugins.example.toml — `admin = true` sur la radio**

Remplacer le contenu de `deploy/plugins.example.toml` par (radio déclare `admin = true` ; l'ancien `admin_url` disparaît partout) :

```toml
[[plugin]]
name = "radio"
kind = "source"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
admin = true

[[plugin]]
name = "cd"
kind = "source"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-cd"

[[plugin]]
name = "mce"
kind = "input"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mce"

[[plugin]]
name = "console"
kind = "display"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-console"
```

- [ ] **Step 2: README — origine unique, page radio servie par le cœur**

Dans `README.md`, section `## Plugins` :
- Remplacer la puce décrivant la page radio sur `:8081` par une puce indiquant que la page de gestion des stations est servie par le cœur, sous la même origine, à `http://<hôte>:8080/plugins/radio/` (le plugin radio ne lie plus de port).
- Dans la description générale, mentionner qu'un plugin peut déclarer `admin = true` (au lieu de l'ancien `admin_url`) pour exposer une page d'admin servie par le cœur.

Puce de remplacement (l'ancienne : « `ritornello-plugin-radio` sert sa propre page … sur `http://<pi>:8081` ») :

```markdown
- `ritornello-plugin-radio` déclare `admin = true` : sa page de gestion des
  stations est servie par le cœur, sous l'origine unique, à
  `http://<hôte>:8080/plugins/radio/` (le plugin ne lie plus aucun port).
```

Dans la recette `## Développement`, retirer la ligne d'environnement
`RITORNELLO_RADIO_HTTP=127.0.0.1:8081 \` (le plugin radio ne lit plus cette
variable).

- [ ] **Step 3: deploy.sh — retirer toute variable RITORNELLO_RADIO_HTTP éventuelle**

Vérifier `deploy/deploy.sh` : s'il définit `RITORNELLO_RADIO_HTTP`, la retirer. (Sinon, aucune modification.)

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && grep -rn RITORNELLO_RADIO_HTTP deploy README.md || echo CLEAN"`
Expected : `CLEAN` (plus aucune référence).

- [ ] **Step 4: Vérification finale — build, clippy, tests, cross-compilation**

Run (build + tests + clippy natif) :
`wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test --workspace && cargo clippy --workspace -- -D warnings"`
Expected : tous les tests verts, aucun warning clippy.

Run (cross-compilation ARM, non-régression du build de déploiement) :
`wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cross build --release --workspace --target armv7-unknown-linux-gnueabihf"`
Expected : `Finished release`.

- [ ] **Step 5: Commit**

```bash
git add deploy/plugins.example.toml README.md deploy/deploy.sh
git commit -m "docs(deploy): admin=true pour la radio, page servie par le coeur, suppression de RITORNELLO_RADIO_HTTP"
```

---

## Notes de validation manuelle (hors CI, à faire une fois sur machine de dev)

Après Task 6, lancer une instance locale (recette du README, sans `RITORNELLO_RADIO_HTTP`) et vérifier :
- `http://127.0.0.1:8080/status` liste la radio avec un lien `admin` interne.
- `http://127.0.0.1:8080/plugins/radio/` affiche la page des stations.
- Modifier une station dans la page → `PUT` → `204` → rechargement → valeur persistée.
- Saisir une présélection invalide (ex. 12) → `422` + message affiché.
- Plus rien n'écoute sur `:8081` (`ss -ltnp | grep 8081` vide).
