# Protocole admin concurrent — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** qu'un `set_data` qui bloque (partage SMB endormi) ne retienne plus ni `ui.js`, ni le catalogue, ni la page d'admin d'un greffon — et que l'IHM sache dire « occupé » plutôt que disparaître.

**Architecture:** le goulot n'est pas le client du cœur (déjà corrélé par `id`, `pending` en `HashMap`), c'est `serve_admin` du SDK, qui lit une ligne, attend le handler, écrit, puis lit la suivante. On le rend concurrent : une tâche par requête, un canal `mpsc` vers l'unique écrivain de la socket, le greffon derrière `Arc<RwLock<P>>` (lectures partagées, `set_data` exclusif). Le cœur décide d'un **budget par nature de requête** et l'envoie dans la trame (`deadline_ms`) ; le serveur l'applique lui-même et répond `Expired` à l'échéance, ce qui inclut l'attente du verrou. Un `Ping` à 500 ms donne au cœur un troisième état de greffon, `busy`, additif comme `stalled`.

**Tech Stack:** Rust (tokio, serde, async_trait, axum), Vue 3 + vitest côté web, TOML i18n en/fr.

**Spec:** cette conversation du 2026-08-26 (aucun fichier de spec séparé) ; contexte : mémoire `incident-partage-muet-boucle-admin` et `docs/plugins.md` §protocole admin.

## Global Constraints

- **Resynchroniser d'abord** : `client.rs` et `main.rs` ont bougé pendant la lecture (une autre session est active, et `main` a avancé de c337774 à f869df8). Toute ligne citée ici est indicative ; relire avant d'éditer.
- Définition de fini = **workspace** : `cargo test --workspace` et `cargo clippy --workspace --all-targets -- -D warnings`, pas `cargo test -p`. Quatre greffons implémentent `AdminPlugin` (`radio`, `files`, `generic-input`, `mpd`) et doivent compiler sans changement.
- Journaux en anglais, écran par catalogue (`crates/ritornello-core/src/locales/en.toml` + `deploy/locales/core/fr.toml`) ; un test Rust impose la parité en/fr — toute clé ajoutée l'est dans les deux fichiers.
- Les formulations des `Display` de `AdminIpcError` sont **inchangées** (elles sont dans les journaux d'appareils en service).
- Aucune compatibilité ascendante à garantir avec des greffons anciens : `deploy.sh` livre cœur et greffons ensemble. `deadline_ms` reste néanmoins `#[serde(default)]` — c'est gratuit et ça garde les tests de protocole existants valides.
- Pas de mutation de `set_data(&mut self)` dans le trait : les quatre impls restent telles quelles.

---

### Task 1 : le protocole porte un budget, un Ping, et une réponse d'échéance

**Files:**
- Modify: `crates/ritornello-proto/src/admin.rs`

**Interfaces:**
- Produces: `AdminRequest { id: u64, deadline_ms: Option<u64>, req: AdminReq }` ; `AdminReq::Ping` ; `AdminResult::Pong` ; `AdminResult::Expired`.

- [ ] **Step 1 : tests qui échouent**

Ajouter dans `mod tests` :

```rust
    #[test]
    fn une_requete_sans_deadline_se_lit_encore() {
        // Les trames écrites avant ce champ : aucun `deadline_ms`.
        let back: AdminRequest = serde_json::from_str(r#"{"id":1,"req":"GetCatalog"}"#).unwrap();
        assert_eq!(back.deadline_ms, None);
        assert_eq!(back.req, AdminReq::GetCatalog);
    }

    #[test]
    fn la_deadline_circule_quand_elle_est_la() {
        let r = AdminRequest { id: 7, deadline_ms: Some(1000), req: AdminReq::GetAsset("ui.js".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":7,"deadline_ms":1000,"req":"GetAsset","arg":"ui.js"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.deadline_ms, Some(1000));
    }

    #[test]
    fn ping_pong_et_expired_font_l_aller_retour() {
        let r = AdminRequest { id: 9, deadline_ms: Some(500), req: AdminReq::Ping };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":9,"deadline_ms":500,"req":"Ping"}"#);
        for res in [AdminResult::Pong, AdminResult::Expired] {
            let json = serde_json::to_string(&AdminResponse { id: 9, result: res.clone() }).unwrap();
            let back: AdminResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(back.result, res);
        }
    }
```

- [ ] **Step 2 : vérifier l'échec**

Run: `cargo test -p ritornello-proto admin::tests`
Expected: erreurs de compilation (`deadline_ms`, `Ping`, `Pong`, `Expired` inconnus).

- [ ] **Step 3 : implémentation**

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    GetAsset(String),
    GetCatalog,
    GetData,
    SetData(serde_json::Value),
    /// Sonde de vivacité : le greffon répond `Pong` sans toucher à son état.
    /// Sert au cœur à distinguer « occupé » (le verrou est pris par un
    /// `set_data` long) de « mort » (socket fermée).
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminRequest {
    pub id: u64,
    /// Budget accordé par le cœur, en millisecondes, **décidé par la nature de
    /// la requête** (un actif en mémoire n'a pas le budget d'un montage
    /// réseau). Le serveur l'applique lui-même et répond `Expired` à
    /// l'échéance ; absent = pas de plafond côté serveur, le client garde le
    /// sien.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(flatten)]
    pub req: AdminReq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AdminResult {
    Asset { mime: String, body: Option<String> },
    Catalog(serde_json::Value),
    Data(serde_json::Value),
    Set { ok: bool, error: Option<String> },
    Pong,
    /// Le greffon **vit** mais n'a pas tenu le budget (traitement ou attente du
    /// verrou). Distinct d'une absence de réponse : ici c'est lui qui le dit.
    Expired,
}
```

Mettre à jour les littéraux `AdminRequest { id, req }` existants dans les tests de ce fichier en `AdminRequest { id, deadline_ms: None, req }`.

- [ ] **Step 4 : vérifier**

Run: `cargo test -p ritornello-proto` puis `cargo check --workspace`
Expected: proto vert ; `cargo check` **échoue** dans `plugin-sdk` (`client.rs` construit `AdminRequest { id, req }` et le `match` de `serve_admin` n'est plus exhaustif) — c'est la Task 2 et la Task 3 qui les réparent. Ne pas commit ici un workspace rouge : enchaîner Task 2 avant le premier commit, **ou** ajouter provisoirement `deadline_ms: None` dans `client.rs:262` et une branche `AdminReq::Ping => AdminResult::Pong` dans `serve_admin` pour rendre le workspace vert, puis commit.

- [ ] **Step 5 : commit**

```bash
git add crates/ritornello-proto/src/admin.rs crates/ritornello-plugin-sdk/src/client.rs crates/ritornello-plugin-sdk/src/server.rs
git commit -m "feat(proto): budget par requete, Ping et Expired sur le protocole admin"
```

---

### Task 2 : `serve_admin` répond hors d'ordre et tient le budget

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/server.rs` (trait `AdminPlugin`, fn `serve_admin`, tests `admin_server_tests`)

**Interfaces:**
- Consumes: Task 1.
- Produces: `pub async fn serve_admin(listener: UnixListener, plugin: impl AdminPlugin) -> Result<()>` (signature inchangée, plus de `mut`) ; `pub trait AdminPlugin: Send + Sync + 'static` (ajout de `Sync`).

- [ ] **Step 1 : tests qui échouent**

Dans `admin_server_tests`, ajouter à `FakeAdmin` un champ `lenteur_set: std::time::Duration` (durée d'un `set_data`), utilisé ainsi : `tokio::time::sleep(self.lenteur_set).await;` en tête de `set_data`. Ajouter un constructeur de connexion pour ne pas dupliquer la boucle de 50 essais :

```rust
    async fn client_connecte(plugin: FakeAdmin) -> (BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        std::mem::forget(dir); // le socket doit survivre au test
        let listener = bind_admin(&socket).unwrap();
        tokio::spawn(async move { serve_admin(listener, plugin).await.unwrap() });
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (r, w) = stream.into_split();
        (BufReader::new(r), w)
    }

    async fn ligne(r: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> AdminResponse {
        let mut s = String::new();
        r.read_line(&mut s).await.unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[tokio::test]
    async fn un_set_data_lent_ne_retient_pas_ui_js() {
        // L'incident du partage muet : la boucle admin était sérielle, donc un
        // seul appel système qui n'aboutit pas retenait `ui.js`, un simple
        // `include_str!`. Ici `set_data` dort 3 s ; l'actif doit revenir bien
        // avant, et **avant** la réponse du set.
        let fake = FakeAdmin { data: serde_json::json!({}), lenteur_set: std::time::Duration::from_secs(3) };
        let (mut r, mut w) = client_connecte(fake).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"req\":\"GetAsset\",\"arg\":\"ui.js\"}\n").await.unwrap();
        let debut = std::time::Instant::now();
        let premiere = ligne(&mut r).await;
        assert_eq!(premiere.id, 2, "l'actif doit repondre avant le set lent");
        assert!(debut.elapsed() < std::time::Duration::from_secs(1), "{:?}", debut.elapsed());
        let seconde = ligne(&mut r).await;
        assert_eq!(seconde.id, 1);
        assert_eq!(seconde.result, AdminResult::Set { ok: true, error: None });
    }

    #[tokio::test]
    async fn le_budget_est_tenu_par_le_serveur() {
        // Le cœur accorde 200 ms ; le set en prend 3 s : le greffon le dit
        // lui-même (`Expired`) au lieu de laisser le client deviner.
        let fake = FakeAdmin { data: serde_json::json!({}), lenteur_set: std::time::Duration::from_secs(3) };
        let (mut r, mut w) = client_connecte(fake).await;
        w.write_all(b"{\"id\":1,\"deadline_ms\":200,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        let debut = std::time::Instant::now();
        let rep = ligne(&mut r).await;
        assert_eq!(rep.result, AdminResult::Expired);
        assert!(debut.elapsed() < std::time::Duration::from_secs(2), "{:?}", debut.elapsed());
    }

    #[tokio::test]
    async fn ping_repond_pong_meme_pendant_un_set_data() {
        let fake = FakeAdmin { data: serde_json::json!({}), lenteur_set: std::time::Duration::from_secs(3) };
        let (mut r, mut w) = client_connecte(fake).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"deadline_ms\":500,\"req\":\"Ping\"}\n").await.unwrap();
        let rep = ligne(&mut r).await;
        assert_eq!((rep.id, rep.result), (2, AdminResult::Pong));
    }

    #[tokio::test]
    async fn get_catalog_attend_le_verrou_dans_son_budget_puis_expire() {
        // Le catalogue lit l'état du greffon, donc attend la fin d'un
        // `set_data` en cours ; si le budget est plus court que ce set, c'est
        // `Expired`, pas un silence. La borne dans l'assertion est large :
        // sous charge, un test qui suppose une exécution rapide est un flake.
        let fake = FakeAdmin { data: serde_json::json!({}), lenteur_set: std::time::Duration::from_secs(3) };
        let (mut r, mut w) = client_connecte(fake).await;
        w.write_all(b"{\"id\":1,\"req\":\"SetData\",\"arg\":{}}\n").await.unwrap();
        w.write_all(b"{\"id\":2,\"deadline_ms\":300,\"req\":\"GetCatalog\"}\n").await.unwrap();
        let rep = ligne(&mut r).await;
        assert_eq!((rep.id, rep.result), (2, AdminResult::Expired));
    }
```

Le test existant `getasset_getdata_setdata_getcatalog_dialogue` reste tel quel (il envoie une requête à la fois : l'ordre est conservé de fait). Le `FakeAdmin` existant reçoit `lenteur_set: Duration::ZERO` dans ses littéraux.

Nota sur `Ping` pendant un `set_data` : `Ping` ne prend **aucun** verrou — c'est ce qui le rend utile.

- [ ] **Step 2 : vérifier l'échec**

Run: `cargo test -p ritornello-plugin-sdk admin_server_tests`
Expected: `un_set_data_lent_ne_retient_pas_ui_js` échoue (id 1 revient d'abord, après 3 s) ; les autres échouent sur `Expired`/`Pong` jamais émis.

- [ ] **Step 3 : implémentation**

```rust
#[async_trait::async_trait]
pub trait AdminPlugin: Send + Sync + 'static {
    fn asset(&self, path: &str) -> Option<(String, String)>;
    fn catalog(&self) -> serde_json::Value;
    async fn get_data(&self) -> serde_json::Value;
    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>;
}

/// Accepte la connexion du cœur, puis traite les requêtes admin **en
/// parallèle** : une tâche par requête, un seul écrivain sur la socket.
///
/// Historiquement sériel (lire, attendre, écrire, relire), ce qui faisait
/// qu'un `set_data` qui montait un partage réseau endormi retenait `ui.js`,
/// un simple `include_str!`, jusqu'au plafond du cœur — la page d'admin
/// « disparaissait ». Les réponses partent maintenant dans l'ordre où elles
/// aboutissent ; c'est l'`id` qui les corrèle, pas l'ordre.
///
/// Le greffon est derrière un `RwLock` : `asset`, `catalog`, `get_data`
/// lisent en parallèle, `set_data` est exclusif — il l'est légitimement, c'est
/// une écriture. Le budget (`deadline_ms`) couvre l'**attente du verrou** aussi
/// bien que le traitement : un `GetCatalog` coincé derrière un `set_data` de
/// 60 s répond `Expired` à son échéance au lieu de se taire.
///
/// `Ping` ne prend aucun verrou : c'est ce qui permet au cœur de distinguer
/// « occupé » de « mort ».
pub async fn serve_admin(listener: UnixListener, plugin: impl AdminPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let plugin = std::sync::Arc::new(tokio::sync::RwLock::new(plugin));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AdminResponse>(64);

    // L'unique écrivain : sérialise les trames sortantes sans sérialiser les
    // traitements.
    let ecrivain = tokio::spawn(async move {
        while let Some(resp) = rx.recv().await {
            let ligne = match serde_json::to_string(&resp) {
                Ok(l) => l,
                Err(e) => { tracing::warn!("admin response not serializable: {e}"); continue; }
            };
            if write.write_all(format!("{ligne}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let req: AdminRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("invalid admin request ignored: {e}");
                continue;
            }
        };
        let plugin = plugin.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let id = req.id;
            let budget = req.deadline_ms.map(std::time::Duration::from_millis);
            let travail = traite_admin(plugin, req.req);
            let result = match budget {
                Some(d) => match tokio::time::timeout(d, travail).await {
                    Ok(r) => r,
                    Err(_) => {
                        tracing::warn!("admin request {id} exceeded its {} ms budget", d.as_millis());
                        AdminResult::Expired
                    }
                },
                None => travail.await,
            };
            // Le destinataire a pu partir (cœur déconnecté) : rien à faire.
            let _ = tx.send(AdminResponse { id, result }).await;
        });
    }
    drop(tx);
    let _ = ecrivain.await;
    Ok(())
}

async fn traite_admin<P: AdminPlugin>(
    plugin: std::sync::Arc<tokio::sync::RwLock<P>>,
    req: AdminReq,
) -> AdminResult {
    match req {
        AdminReq::Ping => AdminResult::Pong,
        AdminReq::GetAsset(path) => match plugin.read().await.asset(&path) {
            Some((mime, body)) => AdminResult::Asset { mime, body: Some(body) },
            None => AdminResult::Asset { mime: "text/plain".to_string(), body: None },
        },
        AdminReq::GetCatalog => AdminResult::Catalog(plugin.read().await.catalog()),
        AdminReq::GetData => AdminResult::Data(plugin.read().await.get_data().await),
        AdminReq::SetData(data) => match plugin.write().await.set_data(data).await {
            Ok(()) => AdminResult::Set { ok: true, error: None },
            Err(msg) => AdminResult::Set { ok: false, error: Some(msg) },
        },
    }
}
```

Point d'attention : `tokio::time::timeout` sur un `write().await.set_data()` **abandonne le futur** à l'échéance — le `set_data` est interrompu au prochain point d'`await` et le verrou relâché. Une IO bloquante dans un `spawn_blocking` n'est en revanche **pas** interrompue : c'est pour cela que la discipline de `sante.rs` (sondes hors fil, disjoncteur par montage) reste nécessaire dans `plugin-files`. À écrire dans `docs/plugins.md` (Task 6).

Si `cargo check --workspace` refuse `Sync` pour l'une des quatre impls (un champ `Rc`, un `RefCell`…), le remplacer localement par un `Mutex`/`Arc` **dans le greffon fautif** — ne pas retirer la borne.

- [ ] **Step 4 : vérifier**

Run: `cargo test -p ritornello-plugin-sdk` puis `cargo check --workspace`
Expected: tout vert (client.rs compile encore grâce au `deadline_ms: None` provisoire de la Task 1).

- [ ] **Step 5 : commit**

```bash
git add crates/ritornello-plugin-sdk/src/server.rs
git commit -m "feat(sdk): serve_admin repond hors d'ordre, tient le budget, et Ping ne prend aucun verrou"
```

---

### Task 3 : le client du cœur décide du budget par nature de requête

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/client.rs` (`AdminClient::request`, les quatre méthodes publiques, `AdminIpcError`, tests)

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: `pub async fn ping(&self) -> Result<()>` sur `AdminClient` ; `pub fn budget(req: &AdminReq) -> Duration` (pub pour les tests et la doc) ; `AdminIpcError::Timeout` couvre maintenant aussi la réponse `Expired`.

- [ ] **Step 1 : tests qui échouent**

Dans les tests de `client.rs`, à côté de `admin_client_dialogue…` (~l. 1220) :

```rust
    #[test]
    fn le_budget_depend_de_la_nature_de_la_requete() {
        use std::time::Duration;
        assert_eq!(budget(&AdminReq::Ping), Duration::from_millis(500));
        assert_eq!(budget(&AdminReq::GetAsset("ui.js".into())), Duration::from_secs(1));
        assert_eq!(budget(&AdminReq::GetCatalog), Duration::from_secs(1));
        assert_eq!(budget(&AdminReq::GetData), Duration::from_secs(5));
        assert_eq!(budget(&AdminReq::SetData(serde_json::json!({}))), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn la_deadline_part_dans_la_trame_et_expired_devient_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let l = lines.next_line().await.unwrap().unwrap();
            let req: ritornello_proto::AdminRequest = serde_json::from_str(&l).unwrap();
            assert_eq!(req.deadline_ms, Some(30_000), "SetData porte son budget");
            write.write_all(b"{\"id\":1,\"result\":{\"kind\":\"Expired\"}}\n").await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = AdminClient::connect(&socket).await.unwrap();
        let err = client.set_data(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err.downcast_ref::<AdminIpcError>(), Some(&AdminIpcError::Timeout));
    }

    #[tokio::test]
    async fn ping_sans_reponse_echoue_en_moins_d_une_seconde() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _garde = stream; // connecté, muet
            std::future::pending::<()>().await;
        });
        let client = AdminClient::connect(&socket).await.unwrap();
        let debut = std::time::Instant::now();
        let err = client.ping().await.unwrap_err();
        assert_eq!(err.downcast_ref::<AdminIpcError>(), Some(&AdminIpcError::Timeout));
        // 500 ms de budget + 500 ms de grâce transport : bien sous les 5 s d'avant.
        assert!(debut.elapsed() < std::time::Duration::from_secs(2), "{:?}", debut.elapsed());
    }
```

- [ ] **Step 2 : vérifier l'échec**

Run: `cargo test -p ritornello-plugin-sdk budget la_deadline ping_sans`
Expected: `budget` et `ping` inconnus.

- [ ] **Step 3 : implémentation**

```rust
/// Budget accordé à une requête admin, **par nature** : c'est le cœur qui
/// sait qu'un actif est un `include_str!` et qu'un `SetData` peut monter un
/// partage réseau. Un plafond unique de 5 s donnait le même délai aux deux.
pub fn budget(req: &AdminReq) -> std::time::Duration {
    use std::time::Duration;
    match req {
        AdminReq::Ping => Duration::from_millis(500),
        AdminReq::GetAsset(_) | AdminReq::GetCatalog => Duration::from_secs(1),
        AdminReq::GetData => Duration::from_secs(5),
        AdminReq::SetData(_) => Duration::from_secs(30),
    }
}

/// Marge accordée au transport au-delà du budget : le serveur répond
/// `Expired` à l'échéance, il faut lui laisser le temps de le dire.
const GRACE: std::time::Duration = std::time::Duration::from_millis(500);
```

Dans `AdminClient::request` :

```rust
    async fn request(&self, req: AdminReq) -> Result<AdminResult> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let budget = budget(&req);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = AdminRequest { id, deadline_ms: Some(budget.as_millis() as u64), req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(budget + GRACE, rx).await {
            // Le greffon vit et le dit lui-même : même verdict que le silence.
            Ok(Ok(AdminResult::Expired)) => Err(AdminIpcError::Timeout.into()),
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(AdminIpcError::Closed.into()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(AdminIpcError::Timeout.into())
            }
        }
    }

    pub async fn ping(&self) -> Result<()> {
        match self.request(AdminReq::Ping).await? {
            AdminResult::Pong => Ok(()),
            other => bail!("unexpected admin response for Ping: {other:?}"),
        }
    }
```

Mettre à jour le doc-commentaire de `AdminIpcError::Timeout` : « Le budget de la requête est dépassé — par silence, ou parce que le greffon a répondu `Expired`. Le plafond n'est plus 5 s pour tous : voir `budget`. » Retirer le `deadline_ms: None` provisoire de la Task 1.

- [ ] **Step 4 : vérifier**

Run: `cargo test -p ritornello-plugin-sdk` puis `cargo test --workspace`
Expected: vert. Si un test du cœur mentionne « 5 s » dans un nom ou une assertion de message, il est repris en Task 4.

- [ ] **Step 5 : commit**

```bash
git add crates/ritornello-plugin-sdk/src/client.rs
git commit -m "feat(sdk): un budget par nature de requete admin, envoye dans la trame, et un ping"
```

---

### Task 4 : le cœur rend 504 quand c'est le temps, et sait qu'un greffon est occupé

**Files:**
- Modify: `crates/ritornello-core/src/admin.rs` (`AdminBackend`, `refus_plugin`, `Fake` de test)
- Modify: `crates/ritornello-core/src/status.rs` (`PluginStatus.busy`, handler de `/api/status`)
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`

**Interfaces:**
- Consumes: `AdminClient::ping` (Task 3).
- Produces: `AdminBackend::ping(&self) -> Result<()>` ; `PluginStatus { …, busy: bool }` sérialisé seulement si vrai ; `/api/status` renvoie `busy: true` pour un greffon dont le ping échoue en `Timeout`.

- [ ] **Step 1 : tests qui échouent**

Dans `admin.rs` `mod tests` :

```rust
    #[tokio::test]
    async fn un_plugin_trop_lent_rend_504_et_un_plugin_mort_502() {
        let lent = router(state_with(Fake { lent: true, ..Default::default() }));
        let r1 = lent.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r1.status(), StatusCode::GATEWAY_TIMEOUT);
        let mort = router(state_with(Fake { down: true, ..Default::default() }));
        let r2 = mort.oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r2.status(), StatusCode::BAD_GATEWAY);
    }
```

Adapter `un_plugin_trop_lent_ne_se_dit_pas_injoignable` : il assertait `BAD_GATEWAY` pour le lent → `GATEWAY_TIMEOUT`.

Dans `status.rs` `mod tests` (utiliser `tests_support` pour construire l'état ; le `Fake` de `admin.rs` n'est pas visible d'ici, donc un petit `FakeOccupe` local) :

```rust
    struct FakeOccupe;
    #[async_trait::async_trait]
    impl crate::admin::AdminBackend for FakeOccupe {
        async fn asset(&self, _: &str) -> anyhow::Result<Option<(String, String)>> { Ok(None) }
        async fn catalog(&self) -> anyhow::Result<serde_json::Value> { Ok(serde_json::json!({})) }
        async fn get_data(&self) -> anyhow::Result<serde_json::Value> { Ok(serde_json::json!({})) }
        async fn set_data(&self, _: serde_json::Value) -> anyhow::Result<Result<(), String>> { Ok(Ok(())) }
        async fn ping(&self) -> anyhow::Result<()> { Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
    }

    #[tokio::test]
    async fn un_greffon_qui_ne_repond_pas_au_ping_est_occupe_dans_le_statut() {
        let st = tests_support::etat_minimal(); // constructeur existant de tests_support, à adapter au nom réel
        st.status.write().await.plugins = vec![PluginStatus::genre("files", "source", true, true)];
        st.admin_backends.write().await.insert("files".into(), Arc::new(FakeOccupe));
        let app = router(st);
        let resp = app.oneshot(Request::get("/api/status").body(Body::empty()).unwrap()).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["plugins"][0]["busy"], serde_json::json!(true));
    }

    #[test]
    fn busy_est_additif_absent_quand_faux() {
        let l = PluginStatus::genre("radio", "source", true, true);
        let json = serde_json::to_string(&l).unwrap();
        assert!(!json.contains("busy"), "{json}");
    }
```

Et le test de parité en/fr existant échouera de lui-même si une clé manque d'un côté.

- [ ] **Step 2 : vérifier l'échec**

Run: `cargo test -p ritornello-core admin:: status::tests::un_greffon_qui busy_est`
Expected: `ping` absent du trait, `busy` inconnu, 502 au lieu de 504.

- [ ] **Step 3 : implémentation**

`admin.rs` :

```rust
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>>;
    async fn catalog(&self) -> Result<serde_json::Value>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>;
    /// Sonde à 500 ms, sans verrou côté greffon : `Err(Timeout)` = occupé,
    /// `Err(Closed)` = mort.
    async fn ping(&self) -> Result<()>;
}
// impl pour AdminClient :
    async fn ping(&self) -> Result<()> { ritornello_plugin_sdk::AdminClient::ping(self).await }
```

`refus_plugin` : le code HTTP suit la cause —

```rust
    let (code, cle) = match e.downcast_ref::<ritornello_plugin_sdk::AdminIpcError>() {
        Some(ritornello_plugin_sdk::AdminIpcError::Timeout) => (StatusCode::GATEWAY_TIMEOUT, "plugin_timeout"),
        _ => (StatusCode::BAD_GATEWAY, "plugin_unreachable"),
    };
    let msg = st.catalog.read().await.get(cle).to_string();
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
```

Le `Fake` de test reçoit `async fn ping(&self) -> Result<()> { if self.lent { return Err(AdminIpcError::Timeout.into()) } if self.down { bail!("down") } Ok(()) }`.

`status.rs` — `PluginStatus` :

```rust
    /// Greffon joint dont la page d'admin ne répond pas au `Ping` : un
    /// `set_data` long tient son verrou (le plus souvent un partage réseau).
    /// Calculé au moment de `/api/status`, jamais stocké : c'est un état qui
    /// change à la seconde. Additif comme `stalled` et `disabled`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
```

Ajouter `busy: false` dans les trois constructeurs (`genre`, `genre_inconnu`, `desactive`) et dans le littéral de `main.rs:~1832`. Dans le handler GET de `/api/status` (chercher la fonction qui sérialise `StatusState` — `status_get` ou équivalent) :

```rust
    let mut etat = st.status.read().await.clone();
    // Toutes les sondes en parallèle : un greffon occupé ne retarde pas la
    // réponse au-delà de son propre budget (500 ms + grâce).
    let dorsaux = st.admin_backends.read().await.clone();
    let sondes = etat.plugins.iter().filter(|p| p.admin).map(|p| {
        let dorsal = dorsaux.get(&p.name).cloned();
        let nom = p.name.clone();
        async move {
            let occupe = match dorsal {
                Some(d) => matches!(
                    d.ping().await.map_err(|e| e.downcast::<ritornello_plugin_sdk::AdminIpcError>()),
                    Err(Ok(ritornello_plugin_sdk::AdminIpcError::Timeout))
                ),
                None => false,
            };
            (nom, occupe)
        }
    });
    let verdicts: std::collections::HashMap<String, bool> = futures::future::join_all(sondes).await.into_iter().collect();
    for p in etat.plugins.iter_mut() {
        p.busy = verdicts.get(&p.name).copied().unwrap_or(false);
    }
    Json(etat)
```

Vérifier que `futures` est déjà une dépendance de `ritornello-core` (`grep futures crates/ritornello-core/Cargo.toml`) ; sinon utiliser `tokio::task::JoinSet`.

Catalogue — les deux fichiers :

```toml
# en.toml
busy = "busy"
plugin_timeout = "the plugin took too long to answer: it is running but held up, most often by a network share that does not respond."
# fr.toml
busy = "occupé"
plugin_timeout = "le plugin a mis trop longtemps à répondre : il tourne, mais quelque chose le retient — le plus souvent un partage réseau qui ne répond pas."
```

(Le « 5 s » du texte disparaît : le budget dépend désormais de la requête.)

- [ ] **Step 4 : vérifier**

Run: `cargo test --workspace` et `cargo clippy --workspace --all-targets -- -D warnings`
Expected: vert, parité en/fr incluse.

- [ ] **Step 5 : commit**

```bash
git add crates/ritornello-core/src/admin.rs crates/ritornello-core/src/status.rs crates/ritornello-core/src/main.rs crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(core,i18n): 504 quand c'est le temps, et l'etat occupe d'un greffon par ping"
```

---

### Task 5 : l'IHM montre « occupé »

**Files:**
- Modify: `web/app/src/types.ts` (interface `PluginStatus`)
- Modify: `web/app/src/views/ConfigView.vue` (agrégat par greffon ~l. 87-141, badge ~l. 288-290)
- Test: `web/app/src/views/ConfigView.test.ts` (existant — sinon le créer à côté, sur le modèle de `useGreffons.test.ts`)

**Interfaces:**
- Consumes: `busy?: boolean` dans le JSON de `/api/status` (Task 4) ; clé i18n `busy` du catalogue du cœur (Task 4).

- [ ] **Step 1 : test qui échoue**

```ts
it('un greffon occupé porte le badge occupé, pas indisponible', async () => {
  // même montage que les tests voisins de ConfigView : état /api/status simulé
  const wrapper = await monteAvecStatut([
    { name: 'files', kind: 'source', connected: true, admin: true, busy: true },
  ])
  const badge = wrapper.get('[data-test="etat-greffon-files"]')
  expect(badge.text()).toBe(t('busy'))
  expect(badge.attributes('data-variant')).toBe('outline')
})
```

Si aucun `data-test` n'existe sur le badge, l'ajouter (`:data-test="`etat-greffon-${p.name}`"`) — c'est le seul changement de gabarit hors libellé.

- [ ] **Step 2 : vérifier l'échec**

Run: `cd web/app && npx vitest run src/views/ConfigView.test.ts`
Expected: FAIL (badge « indisponible » ou « joint »).

- [ ] **Step 3 : implémentation**

`types.ts` :

```ts
export interface PluginStatus { name: string; kind: string; connected: boolean; admin: boolean; stalled?: boolean; disabled?: boolean; busy?: boolean }
```

`ConfigView.vue` : ajouter `busy: boolean` aux deux types locaux (~l. 87 et 99), `busy: !!p.busy` dans la construction (~l. 121), `acc.busy = acc.busy || !!p.busy` dans l'agrégat (~l. 129), `busy: acc.busy` dans le retour (~l. 141). Badge :

```vue
:variant="p.disabled ? 'outline' : p.busy ? 'outline' : p.connected ? 'secondary' : p.stalled ? 'outline' : 'destructive'"
…
{{ p.disabled ? t('disabled') : p.busy ? t('busy') : p.connected ? t('connected') : p.stalled ? t('stalled') : t('unavailable') }}
```

`busy` passe **avant** `connected` : un greffon occupé est joint, et c'est justement pour ça que « joint » ne dit rien d'utile.

- [ ] **Step 4 : vérifier**

Run: `cd web/app && npx vitest run && npm run typecheck`
Expected: vert. (Mémoire : sous charge, quatre tests préexistants frôlent le plafond de 5 s — relancer une fois avant de conclure à une régression.)

- [ ] **Step 5 : commit**

```bash
git add web/app/src/types.ts web/app/src/views/ConfigView.vue web/app/src/views/ConfigView.test.ts
git commit -m "feat(web): le badge occupe d'un greffon dont la page ne repond pas"
```

---

### Task 6 : la doc du protocole dit ce qui a changé

**Files:**
- Modify: `docs/plugins.md` (section protocole admin — chercher « admin » et « 5 s »)
- Modify: `crates/ritornello-core/src/main.rs:553-557` (commentaire qui dit « sériel, donc en retenant la page »)

- [ ] **Step 1 : rédiger**

Dans `docs/plugins.md`, remplacer la description sérielle par :

- une requête = une tâche ; les réponses arrivent dans l'ordre où elles aboutissent, corrélées par `id` ;
- le tableau des budgets (`Ping` 500 ms, actifs et catalogue 1 s, `GetData` 5 s, `SetData` 30 s), le champ `deadline_ms`, la réponse `Expired` ;
- `asset`/`catalog`/`get_data` lisent en parallèle, `set_data` est exclusif ; le budget inclut l'attente du verrou ;
- **ce que le protocole n'absorbe pas** : une IO bloquante hors `await` n'est pas interrompue par le budget — les greffons qui touchent un chemin réseau gardent l'obligation d'exécuter hors fil et sous disjoncteur (`plugin-files/src/sante.rs`) ;
- l'état `busy` du tableau des greffons et sa règle : ping en `Timeout` = occupé, `Closed` = mort.

Le commentaire de `main.rs` sur le 5 s devient : « rendrait une erreur au bout du budget de la requête, là où un 404 franc dit tout de suite… ».

- [ ] **Step 2 : vérifier**

Run: `cargo test --workspace` (un test relit peut-être la doc pour la parité des clés ; sinon rien à faire)
Expected: vert.

- [ ] **Step 3 : commit**

```bash
git add docs/plugins.md crates/ritornello-core/src/main.rs
git commit -m "docs(plugins): le protocole admin concurrent, ses budgets, et ce qu'il n'absorbe pas"
```

---

## Auto-revue

- **Couverture** : concurrence (T2), budget par nature porté par la trame (T1+T3), Ping/occupé (T2+T3+T4+T5), dégradation à l'écran — 504 vs 502, texte sans « 5 s », badge — (T4+T5), doc (T6). ✔
- **Cohérence des noms** : `deadline_ms`, `AdminReq::Ping`, `AdminResult::Pong`/`Expired`, `budget(&AdminReq)`, `AdminClient::ping`, `AdminBackend::ping`, `PluginStatus.busy`, clé i18n `busy` — identiques d'une tâche à l'autre. ✔
- **Risque connu** : la borne `Sync` sur `AdminPlugin` (T2) est le seul point où un greffon pourrait ne plus compiler ; le remède est local au greffon.
- **Flakes** : les tests de T2 utilisent 3 s de lenteur contre des bornes de 1 à 2 s — une marge ≥ 1 s, conforme à la leçon « hypothèse d'exécution rapide ».
