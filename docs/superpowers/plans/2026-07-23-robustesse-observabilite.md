# Robustesse & observabilité (priorités 1 & 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal :** Rendre l'appareil headless diagnosticable et résilient aux pannes de plugin (P1 : supervision, logs, IPC, i18n) et corriger deux bugs de correction (P2 : affichage CD, réveil de veille décidé par le plugin).

**Architecture :** Ajout d'une variante protocole `SourceReq::Wake` (réveil piloté par le plugin, défaut = `activate`), supervision des processus enfants dans la boucle `select!` du cœur via un `FuturesUnordered` qui met à jour un `StatusState` partagé, politique unique « logguer + ignorer » sur les 4 protocoles IPC avec drainage des requêtes en vol à la déconnexion, et ajout de traces sur les erreurs aujourd'hui avalées. Aucun re-spawn ni reconnexion (détection + marquage + log seulement).

**Tech Stack :** Rust (workspace Cargo, edition 2021), tokio (full), axum 0.7, serde/serde_json, toml 0.8, tracing 0.1, async-trait, evdev 0.12 ; nouvelle dépendance `futures` 0.3 dans `ritornello-core` (Task 6).

## Global Constraints

- 1.1 Supervision : **détecter + marquer + logguer** (page de statut vivante). **Pas** de redémarrage automatique (reporté à une éventuelle spec future).
- 2.2 Réveil : comportement **décidé par le plugin** via `wake()` (défaut = `activate()`) : la radio reprend au réveil/boot, le cd ne se lance pas tout seul. Pas de config admin (YAGNI).
- 1.5 Ligne malformée : **politique unique** sur les 4 protocoles : logguer + ignorer la ligne, garder la connexion. Radio : les deux moitiés en tâches indépendantes.
- Portée : un seul lot « robustesse & observabilité ». Aucune nouvelle fonctionnalité utilisateur au-delà de 2.2.
- Intégration `wake` : `wake()` par défaut délègue à `activate()` ; seul cd surcharge. `resume()` (boot + sortie de veille) utilise `Wake` ; les actions explicites de l'utilisateur (`SourceCycle`/`Select`/`retry_stream`) utilisent `Activate`.
- Supervision = détecter + marquer + logguer UNIQUEMENT — pas de re-spawn/reconnexion, on ne touche pas au registre des sources. La mort de `mpv` reste fatale (relance par systemd).
- Drainage `pending` = drop des `oneshot::Sender` restants à la sortie de la boucle lectrice (le `rx.await` de `request()` échoue immédiatement). Les erreurs d'**écriture** restent propagées (vraie déconnexion).
- La vue renvoyée par `Wake` remonte au cœur par le canal `view_tx`/`source_view_tx` existant — aucune nouvelle plomberie.

### Contraintes d'environnement (à respecter dans CHAQUE tâche)

- Les tests tournent **sous WSL** : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p <crate>"`.
- Clippy sous WSL : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p <crate> -- -D warnings"`.
- `ritornello-core` est **binaire uniquement** (pas de `lib.rs`) : `cargo test -p ritornello-core` compile `main.rs`, donc **toute** tâche touchant le cœur doit laisser `main.rs` compilant.
- Messages de commit **et** commentaires de code en **français** (convention du dépôt).
- Les tests sont dans un `#[cfg(test)] mod ...` **à l'intérieur du fichier testé**.
- Si une tâche ajoute/modifie une dépendance, son commit **inclut le `Cargo.lock` régénéré**.
- Une seule commande de commit par tâche (dernière étape).

---

## File Structure

- `crates/ritornello-proto/src/source.rs` — Task 1 (variante `SourceReq::Wake` + roundtrip).
- `crates/ritornello-plugin-sdk/src/server.rs` — Task 1 (dispatch `Wake` + défaut `wake()`), Task 2 (ligne malformée : warn+continue sur les 3 serveurs).
- `crates/ritornello-plugin-sdk/src/client.rs` — Task 2 (warn sur ligne invalide + drainage `pending` à la déconnexion).
- `crates/ritornello-i18n/src/lib.rs` — Task 3 (`try_parse`, log des packs embarqués).
- `crates/ritornello-core/src/admin.rs` — Task 4 (log avant 502).
- `crates/ritornello-core/src/status.rs` — Task 4 (log `list_devices`), Task 6 (`mark_plugin_disconnected`).
- `crates/ritornello-core/src/core.rs` — Task 4 (log `SetLocale` de `resume`), Task 5 (`resume` envoie `Wake`, accesseur `active_source`).
- `crates/ritornello-core/src/player/mpv.rs` — Task 4 (log `debug!` sur ligne non-JSON).
- `crates/ritornello-core/src/main.rs` — Task 4 (bras `Lagged`), Task 6 (supervision `FuturesUnordered`, `active_source` live).
- `crates/ritornello-core/Cargo.toml` — Task 6 (dépendance `futures`).
- `crates/ritornello-plugin-cd/src/main.rs` — Task 7 (suivi de piste, `wake` surchargé, test EN embarqué).
- `crates/ritornello-plugin-radio/src/main.rs` — Task 8 (découplage source/admin, test EN embarqué).
- `crates/ritornello-plugin-mce/src/input.rs` — Task 9 (désambiguïsation périphérique).

---

### Task 1 : proto + SDK — `SourceReq::Wake`

**Files:**
- Modify: `crates/ritornello-proto/src/source.rs`
- Modify: `crates/ritornello-plugin-sdk/src/server.rs`
- Test: `crates/ritornello-proto/src/source.rs` (`#[cfg(test)] mod tests`)
- Test: `crates/ritornello-plugin-sdk/src/server.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (proto) : `enum SourceReq { … , Wake }` — variante unité, sérialisée `{"req":"Wake"}` (tag `req`, content `arg`).
- Produces (SDK) : `trait SourcePlugin { … async fn wake(&mut self) -> SourceOutcome { self.activate().await } }` (défaut = `activate`, donc plugins existants inchangés) ; `run_source_plugin` dispatche `SourceReq::Wake => plugin.wake().await`.
- Consumes : `SourceOutcome { action: SourceAction, view: Option<View> }` (existant), `SourceRequest { id, req }` (existant).

- [ ] **Step 1 : Test proto qui échoue — roundtrip de `SourceReq::Wake`**

Ajouter dans `crates/ritornello-proto/src/source.rs`, dans `#[cfg(test)] mod tests` :

```rust
    #[test]
    fn wake_roundtrip() {
        let r = SourceRequest { id: 4, req: SourceReq::Wake };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"req\":\"Wake\""));
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, SourceReq::Wake);
    }
```

- [ ] **Step 2 : Lancer le test, vérifier l'échec de compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-proto wake_roundtrip"`
Attendu : ÉCHEC de compilation (`no variant named Wake found for enum SourceReq`).

- [ ] **Step 3 : Ajouter la variante `Wake`**

Dans `crates/ritornello-proto/src/source.rs`, ajouter `Wake` à l'énumération (après `Activate`) :

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum SourceReq {
    Activate,
    /// Réveil piloté par le plugin (boot / sortie de veille). Défaut côté SDK :
    /// se comporte comme `Activate` ; un plugin peut surcharger `wake()`.
    Wake,
    Deactivate,
    Select(u8),
    Next,
    Prev,
    NextTrack,
    PrevTrack,
    Eject,
    SetLocale(String),
}
```

- [ ] **Step 4 : Lancer le test proto, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-proto wake_roundtrip"`
Attendu : PASS.

- [ ] **Step 5 : Tests SDK qui échouent — défaut délègue à `activate`, surcharge dispatchée**

Ajouter dans `crates/ritornello-plugin-sdk/src/server.rs`, dans le `#[cfg(test)] mod tests` (celui qui contient `EchoSource`) :

```rust
    #[tokio::test]
    async fn wake_par_defaut_delegue_a_activate() {
        // EchoSource ne surcharge PAS wake() : doit se comporter comme activate().
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Wake\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://fip".into() }));
    }

    #[tokio::test]
    async fn wake_surcharge_est_dispatche() {
        struct WakingSource;
        #[async_trait::async_trait]
        impl SourcePlugin for WakingSource {
            async fn activate(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Play { uri: "http://activate".into() }, view: None } }
            async fn deactivate(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
            async fn select(&mut self, _n: u8) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
            async fn next(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
            async fn prev(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
            async fn next_track(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
            async fn prev_track(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
            async fn eject(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
            async fn wake(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Play { uri: "http://wake".into() }, view: None } }
        }
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(WakingSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"id\":1,\"req\":\"Wake\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        // wake() dispatché (http://wake), PAS activate() (http://activate).
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://wake".into() }));
    }
```

- [ ] **Step 6 : Lancer les tests SDK, vérifier l'échec de compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk wake_"`
Attendu : ÉCHEC de compilation (`no method named wake` / `Wake` non couvert dans le `match`).

- [ ] **Step 7 : Ajouter le défaut `wake()` au trait et le dispatch**

Dans `crates/ritornello-plugin-sdk/src/server.rs`, ajouter la méthode par défaut au trait `SourcePlugin` (après `activate`/avant `set_locale`) :

```rust
    /// Réveil (boot / sortie de veille). Par défaut, se comporte comme
    /// `activate()` (jouer) — adapté à la radio et à toute source simple.
    /// Un plugin qui ne doit pas jouer tout seul au réveil (cd) surcharge.
    async fn wake(&mut self) -> SourceOutcome {
        self.activate().await
    }
```

Puis, dans `run_source_plugin`, ajouter le bras de dispatch dans le `match req.req` (après `SourceReq::Activate`) :

```rust
                    SourceReq::Wake => plugin.wake().await,
```

- [ ] **Step 8 : Lancer les tests SDK, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk wake_"`
Attendu : PASS (2 tests).

- [ ] **Step 9 : Clippy sur les deux crates**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-proto -p ritornello-plugin-sdk -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 10 : Commit**

```bash
git add crates/ritornello-proto/src/source.rs crates/ritornello-plugin-sdk/src/server.rs
git commit -m "feat(proto,sdk): variante SourceReq::Wake avec defaut wake()=activate()"
```

---

### Task 2 : SDK — politique « ligne malformée » + drainage de `pending`

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/server.rs` (`run_source_plugin`, `run_admin_plugin`, `run_display_plugin`)
- Modify: `crates/ritornello-plugin-sdk/src/client.rs` (`SourceClient`/`AdminClient` : warn + drainage)
- Test: `crates/ritornello-plugin-sdk/src/server.rs` (`#[cfg(test)] mod tests`)
- Test: `crates/ritornello-plugin-sdk/src/client.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes : `run_source_plugin`, `SourceClient::connect`, `SourceClient::request`, `AdminClient::connect` (existants).
- Produces : mêmes signatures publiques ; comportement — ligne JSON invalide loggée en `warn!` + ignorée (connexion conservée) ; à la sortie de la boucle lectrice de `SourceClient`/`AdminClient`, `pending` est vidé (senders `drop`és) donc une `request()` en vol renvoie `Err` immédiatement (avant le timeout de 5 s).

- [ ] **Step 1 : Test serveur qui échoue — ligne invalide ignorée, requête suivante servie**

Ajouter dans `crates/ritornello-plugin-sdk/src/server.rs`, dans le `#[cfg(test)] mod tests` (avec `EchoSource`) :

```rust
    #[tokio::test]
    async fn source_ignore_ligne_invalide_et_repond_a_la_suivante() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await { client = Some(s); break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (read, mut write) = client.expect("connexion au plugin").into_split();
        let mut lines = BufReader::new(read).lines();
        // Ligne malformée : doit être ignorée (warn + continue), sans fermer la connexion.
        write.write_all(b"ceci n'est pas du json\n").await.unwrap();
        // Requête valide ensuite : réponse normale attendue.
        write.write_all(b"{\"id\":7,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: ritornello_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(7));
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://fip".into() }));
    }
```

- [ ] **Step 2 : Lancer le test, vérifier l'échec**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk source_ignore_ligne_invalide"`
Attendu : ÉCHEC (aujourd'hui la ligne invalide fait `?` → la tâche serveur panique via `.unwrap()`, la connexion se ferme, `next_line()` renvoie `None` → `.unwrap()` panique dans le test).

- [ ] **Step 3 : Serveurs — warn + continue sur ligne invalide**

Dans `crates/ritornello-plugin-sdk/src/server.rs`, `run_source_plugin` — remplacer le parse `?` :

```rust
                let req: SourceRequest = match serde_json::from_str(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("ligne source invalide ignoree: {e}");
                        continue;
                    }
                };
```

`run_display_plugin` — remplacer le corps de la boucle `while let Some(line) = lines.next_line().await? {` :

```rust
    while let Some(line) = lines.next_line().await? {
        let view: View = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("vue invalide ignoree: {e}");
                continue;
            }
        };
        plugin.show(view).await?;
    }
```

`run_admin_plugin` — remplacer le parse `?` dans la boucle `while let Some(line) = lines.next_line().await? {` :

```rust
        let req: AdminRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("requete admin invalide ignoree: {e}");
                continue;
            }
        };
```

- [ ] **Step 4 : Lancer le test serveur, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk source_ignore_ligne_invalide"`
Attendu : PASS.

- [ ] **Step 5 : Test client qui échoue — requête en vol échoue vite à la déconnexion**

Ajouter dans `crates/ritornello-plugin-sdk/src/client.rs`, dans le `#[cfg(test)] mod tests` :

```rust
    #[tokio::test]
    async fn requete_en_vol_echoue_vite_a_la_deconnexion() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Lit la requête puis ferme la connexion sans répondre.
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let _ = lines.next_line().await;
            // Fin du bloc : read et _write droppés -> EOF côté client.
        });
        let (view_tx, _view_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), view_tx).await.unwrap();
        let start = std::time::Instant::now();
        let res = client.request(SourceReq::Activate).await;
        assert!(res.is_err());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "la requête doit échouer AVANT le timeout de 5 s (pending drainé)"
        );
    }
```

- [ ] **Step 6 : Lancer le test client, vérifier l'échec**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk requete_en_vol_echoue_vite"`
Attendu : ÉCHEC (aujourd'hui la requête attend le timeout de 5 s → `elapsed < 2 s` faux).

- [ ] **Step 7 : Clients — warn sur ligne invalide + drainage de `pending`**

Dans `crates/ritornello-plugin-sdk/src/client.rs`, `SourceClient::connect` — dans la tâche lectrice, remplacer le corps de la boucle et ajouter le drainage :

```rust
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let msg = match serde_json::from_str::<SourceMessage>(&line) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("message source invalide ignore: {e}");
                        continue;
                    }
                };
                if let (Some(id), Some(action)) = (msg.id, msg.action.clone()) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(action);
                    }
                }
                if let Some(view) = msg.view {
                    if view_tx.try_send((name.clone(), view)).is_err() {
                        tracing::warn!("vue de {name} perdue (canal plein)");
                    }
                }
            }
            // Déconnexion : drainer les requêtes en vol. Dropper chaque Sender
            // fait résoudre le rx.await de request() en Err immédiatement.
            pending.lock().await.clear();
            tracing::warn!("connexion au plugin source fermee");
        });
```

`AdminClient::connect` — même traitement dans sa tâche lectrice :

```rust
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let resp = match serde_json::from_str::<AdminResponse>(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("reponse admin invalide ignoree: {e}");
                        continue;
                    }
                };
                if let Some(tx) = pending.lock().await.remove(&resp.id) {
                    let _ = tx.send(resp.result);
                }
            }
            // Déconnexion : drainer les requêtes en vol (voir SourceClient).
            pending.lock().await.clear();
            tracing::warn!("connexion au plugin admin fermee");
        });
```

- [ ] **Step 8 : Lancer le test client, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk requete_en_vol_echoue_vite"`
Attendu : PASS.

- [ ] **Step 9 : Suite complète + clippy du crate SDK**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-sdk && cargo clippy -p ritornello-plugin-sdk -- -D warnings"`
Attendu : tous les tests PASS, aucun warning.

- [ ] **Step 10 : Commit**

```bash
git add crates/ritornello-plugin-sdk/src/server.rs crates/ritornello-plugin-sdk/src/client.rs
git commit -m "feat(sdk): ligne malformee = log+ignore, drainage de pending a la deconnexion"
```

---

### Task 3 : i18n — `try_parse` + log des packs embarqués

**Files:**
- Modify: `crates/ritornello-i18n/src/lib.rs`
- Test: `crates/ritornello-i18n/src/lib.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces : `pub fn try_parse(s: &str) -> Result<HashMap<String, String>, toml::de::Error>` ; `parse_pack` reste `pub fn parse_pack(s: &str) -> HashMap<String, String>` (silencieux, `try_parse(s).unwrap_or_default()`).
- Consumes : `COMMON_EN` (const embarquée), `Catalog::load(component, locale, root, own_en)` (existant).
- Note : les tests « EN embarqué non vide » **par plugin/cœur** vivent dans leurs tâches respectives (Task 5 pour `core::EN`, Task 7 pour `CD_EN`, Task 8 pour `RADIO_EN`), là où la constante est en portée ; ils appellent `ritornello_i18n::try_parse`.

- [ ] **Step 1 : Tests qui échouent — `try_parse` non vide sur `COMMON_EN`, `Err` sur TOML invalide**

Ajouter dans `crates/ritornello-i18n/src/lib.rs`, dans `#[cfg(test)] mod tests` :

```rust
    #[test]
    fn try_parse_du_common_en_embarque_est_non_vide() {
        assert!(!try_parse(COMMON_EN).unwrap().is_empty());
    }

    #[test]
    fn try_parse_renvoie_err_sur_toml_invalide() {
        assert!(try_parse("ceci n'est pas du toml =").is_err());
    }
```

- [ ] **Step 2 : Lancer les tests, vérifier l'échec de compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-i18n try_parse"`
Attendu : ÉCHEC de compilation (`cannot find function try_parse`).

- [ ] **Step 3 : Factoriser `try_parse` et logguer les packs embarqués**

Dans `crates/ritornello-i18n/src/lib.rs`, remplacer `parse_pack` par la paire `try_parse`/`parse_pack` :

```rust
/// Parse pur d'un pack TOML plat (`clé = "valeur"`). Renvoie l'erreur de parse
/// pour l'appelant qui souhaite la logguer (chargement des couches de base).
pub fn try_parse(s: &str) -> Result<HashMap<String, String>, toml::de::Error> {
    toml::from_str(s)
}

/// Parse pur silencieux : TOML invalide → map vide (l'appelant gère l'absence).
/// Séparé de l'accès disque pour être testable (comme `audio_output::parse_device_list`).
pub fn parse_pack(s: &str) -> HashMap<String, String> {
    try_parse(s).unwrap_or_default()
}
```

Puis, dans `Catalog::load`, remplacer les deux `parse_pack` des couches de base par un chargement qui logue :

```rust
    pub fn load(component: &str, locale: &str, root: &Path, own_en: &str) -> Catalog {
        let mut own = match try_parse(own_en) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("pack embarque {component} invalide: {e}");
                HashMap::new()
            }
        };
        let mut common = match try_parse(COMMON_EN) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("pack embarque common invalide: {e}");
                HashMap::new()
            }
        };
        overlay_from_disk(&mut common, &root.join("common").join(format!("{locale}.toml")));
        overlay_from_disk(&mut own, &root.join(component).join(format!("{locale}.toml")));
        Catalog { own, common }
    }
```

- [ ] **Step 4 : Lancer les tests, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-i18n"`
Attendu : tous les tests PASS (dont les 2 nouveaux ; `parse_pack_lit_le_toml_plat_et_ignore_l_invalide` toujours vert).

- [ ] **Step 5 : Clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-i18n -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-i18n/src/lib.rs
git commit -m "feat(i18n): try_parse partage et warn sur pack embarque invalide"
```

---

### Task 4 : cœur — observabilité (logs sur erreurs avalées)

**Files:**
- Modify: `crates/ritornello-core/src/admin.rs` (3 handlers)
- Modify: `crates/ritornello-core/src/status.rs` (2 sites `list_devices`)
- Modify: `crates/ritornello-core/src/core.rs` (boucle `SetLocale` de `resume`)
- Modify: `crates/ritornello-core/src/player/mpv.rs` (ligne non-JSON)
- Modify: `crates/ritornello-core/src/main.rs` (bras `Lagged` de `ev_rx`)

**Interfaces:**
- Consumes : `tracing::warn!`/`debug!` (crate `tracing` déjà dépendance), `broadcast::error::RecvError` (`broadcast` déjà importé dans `main.rs`).
- Produces : aucun changement de signature ; ajouts de traces + un garde `events_open` sur le bras `ev_rx` (comportement conservé, non fatal).
- Note TDD : tâche essentiellement d'ajout de logs — **pas de nouveau test** (aucun comportement observable ne change, hormis des traces). Validation par la suite existante + clippy. Le seul changement de flot (garde `events_open`) préserve le comportement actuel sur `Closed` (bras désactivé) et ajoute la trace sur `Lagged`.

- [ ] **Step 1 : admin.rs — logguer avant 502 (3 handlers)**

Dans `crates/ritornello-core/src/admin.rs`, remplacer les trois bras `Err(_) => (StatusCode::BAD_GATEWAY, ...)` :

`admin_page` :

```rust
        Some(backend) => match backend.page().await {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (page): {e}");
                (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response()
            }
        },
```

`admin_get_data` :

```rust
        Some(backend) => match backend.get_data().await {
            Ok(value) => Json(value).into_response(),
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (get_data): {e}");
                (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response()
            }
        },
```

`admin_put_data` — remplacer uniquement le bras `Err(_)` :

```rust
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (set_data): {e}");
                (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response()
            }
```

- [ ] **Step 2 : status.rs — logguer l'échec de `list_devices` (2 sites)**

Dans `crates/ritornello-core/src/status.rs`, dans `audio_output_json`, remplacer :

```rust
    let devices = match crate::audio_output::list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("liste des sorties audio indisponible: {e}");
            Vec::new()
        }
    };
```

Et dans `status_page`, remplacer la ligne `let devices = crate::audio_output::list_devices().unwrap_or_default();` par le même bloc `match` (identique à ci-dessus).

- [ ] **Step 3 : core.rs — aligner le log `SetLocale` de `resume()`**

Dans `crates/ritornello-core/src/core.rs`, dans `resume()`, remplacer la boucle silencieuse :

```rust
        if let Some(locale) = self.locale.clone() {
            for name in self.source_order.clone() {
                if let Some(src) = self.sources.get(&name) {
                    if let Err(e) = src.request(SourceReq::SetLocale(locale.clone())).await {
                        tracing::warn!("SetLocale vers {name}: {e}");
                    }
                }
            }
        }
```

- [ ] **Step 4 : mpv.rs — logguer en `debug!` la ligne non-JSON**

Dans `crates/ritornello-core/src/player/mpv.rs`, dans la tâche lectrice de `from_stream`, remplacer :

```rust
            while let Ok(Some(line)) = lines.next_line().await {
                let v = match serde_json::from_str::<Value>(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("ligne mpv non-JSON ignoree: {e}");
                        continue;
                    }
                };
```

- [ ] **Step 5 : main.rs — bras `ev_rx` avec traitement du `Lagged`**

Dans `crates/ritornello-core/src/main.rs`, juste avant `loop {`, déclarer le garde :

```rust
    let mut events_open = true;
```

Remplacer le bras `Ok(ev) = ev_rx.recv() => { ... }` du `tokio::select!` par :

```rust
            ev = ev_rx.recv(), if events_open => {
                match ev {
                    Ok(ev) => {
                        if matches!(ev, Event::Title(_) | Event::PlaybackActive) {
                            retry_at = None;
                        }
                        if let Some(delay) = core.handle_event(ev).await {
                            retry_at = Some(tokio::time::Instant::now() + delay);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("events en retard, {n} perdus");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Canal fermé : on désactive le bras (comme aujourd'hui,
                        // le bras cessait de matcher) pour éviter de tourner à vide.
                        events_open = false;
                    }
                }
            }
```

- [ ] **Step 6 : Compiler + suite complète du cœur (aucune régression)**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core"`
Attendu : compilation OK, tous les tests existants PASS.

- [ ] **Step 7 : Clippy du cœur**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-core -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 8 : Commit**

```bash
git add crates/ritornello-core/src/admin.rs crates/ritornello-core/src/status.rs crates/ritornello-core/src/core.rs crates/ritornello-core/src/player/mpv.rs crates/ritornello-core/src/main.rs
git commit -m "feat(core): logguer les erreurs avalees (admin 502, aplay, SetLocale, mpv, events Lagged)"
```

---

### Task 5 : cœur — `resume()` envoie `Wake` + accesseur `active_source()`

**Files:**
- Modify: `crates/ritornello-core/src/core.rs`
- Test: `crates/ritornello-core/src/core.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes : `SourceReq::Wake` (Task 1), `ritornello_i18n::try_parse` (Task 3), `Source::request` (existant).
- Produces : `Core::resume()` envoie `SourceReq::Wake` (au lieu de `Activate`) à la source active ; `pub fn active_source(&self) -> &str` (renvoie `&self.active_source`).
- Dépend de : Task 1 (`SourceReq::Wake`) et Task 3 (`try_parse`, pour le test EN embarqué du cœur).

- [ ] **Step 1 : Adapter le faux `Source` de test à `Wake`**

Dans `crates/ritornello-core/src/core.rs`, `#[cfg(test)] mod tests`, dans `impl Source for FakeSource`, ajouter le mapping `Wake` (radio joue au réveil, comme `activate`) — ajouter ces deux bras dans le `match (self.name, req)` avant le `_ =>` final :

```rust
                ("radio", SourceReq::Wake) => SourceAction::Play { uri: "http://fip".into() },
                ("cd", SourceReq::Wake) => SourceAction::Noop,
```

- [ ] **Step 2 : Mettre à jour l'assertion de `resume_active_la_source_persistee`**

Dans le même module de tests, remplacer l'assertion sur `radio:Activate` par `radio:Wake` :

```rust
    #[tokio::test]
    async fn resume_active_la_source_persistee() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Wake"));
    }
```

- [ ] **Step 3 : Nouveaux tests qui échouent — `resume` envoie `Wake`, `active_source()`, EN embarqué du cœur**

Ajouter dans le module de tests :

```rust
    #[tokio::test]
    async fn resume_envoie_wake_pas_activate() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        let calls = source_calls.lock().unwrap();
        assert!(calls.iter().any(|c| c == "radio:Wake"));
        assert!(!calls.iter().any(|c| c == "radio:Activate"));
    }

    #[test]
    fn active_source_retourne_la_source_courante() {
        let (core, _pc, _sc, _rx, _d) = setup();
        // PersistedState::default().active_source == "radio".
        assert_eq!(core.active_source(), "radio");
    }

    #[test]
    fn en_embarque_du_coeur_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(super::EN).unwrap().is_empty());
    }
```

- [ ] **Step 4 : Lancer les tests, vérifier l'échec de compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core active_source_retourne"`
Attendu : ÉCHEC de compilation (`no method named active_source`) ; `resume_envoie_wake_pas_activate` échouerait aussi (resume envoie encore `Activate`).

- [ ] **Step 5 : `resume()` envoie `Wake` + ajout de `active_source()`**

Dans `crates/ritornello-core/src/core.rs`, dans `resume()`, remplacer la ligne d'activation :

```rust
        let action = self.active().request(SourceReq::Wake).await?;
        self.apply(action).await
```

Ajouter l'accesseur dans `impl<P: Player> Core<P>` (par exemple juste après `active()`) :

```rust
    /// Nom de la source actuellement active (pour la page de statut vivante).
    pub fn active_source(&self) -> &str {
        &self.active_source
    }
```

Ne PAS toucher `retry_stream` (garde `SourceReq::Activate`), ni `SourceCycle`/`Select` dans `handle_command` (gardent `Activate`).

- [ ] **Step 6 : Lancer la suite du cœur, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core"`
Attendu : tous les tests PASS (dont `resume_active_la_source_persistee` mis à jour, `standby_bloque_tout_sauf_power`, `veille_affiche...` qui reposent sur radio jouant au réveil via `Wake`).

- [ ] **Step 7 : Clippy du cœur**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-core -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 8 : Commit**

```bash
git add crates/ritornello-core/src/core.rs
git commit -m "feat(core): resume() envoie Wake et accesseur active_source()"
```

---

### Task 6 : cœur — supervision des plugins + page de statut vivante

**Files:**
- Modify: `crates/ritornello-core/Cargo.toml` (dépendance `futures`)
- Modify: `crates/ritornello-core/src/status.rs` (`mark_plugin_disconnected`)
- Modify: `crates/ritornello-core/src/main.rs` (`FuturesUnordered`, `active_source` live)
- Test: `crates/ritornello-core/src/status.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes : `Core::active_source()` (Task 5), `StatusState` / `PluginStatus` (existants), `Arc<RwLock<StatusState>>` (`status_state`, déjà créé dans `main`).
- Produces : `pub fn mark_plugin_disconnected(state: &mut StatusState, name: &str)` dans `status.rs` (passe `connected=false` pour le plugin nommé ; no-op si inconnu). `main` : chaque `Child` de plugin est surveillé via `FuturesUnordered` ; à sa mort → `warn!` + `mark_plugin_disconnected` ; `status.active_source` mis à jour depuis `core.active_source()` après chaque commande.
- Dépend de : Task 5 (`active_source()`).

- [ ] **Step 1 : Test qui échoue — `mark_plugin_disconnected` bascule `connected`**

Ajouter dans `crates/ritornello-core/src/status.rs`, dans `#[cfg(test)] mod tests` :

```rust
    #[test]
    fn mark_plugin_disconnected_bascule_connected() {
        let mut st = StatusState {
            plugins: vec![
                PluginStatus { name: "radio".into(), kind: "source".into(), connected: true, admin: true },
                PluginStatus { name: "cd".into(), kind: "source".into(), connected: true, admin: false },
            ],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "cd");
        assert!(!st.plugins.iter().find(|p| p.name == "cd").unwrap().connected);
        assert!(st.plugins.iter().find(|p| p.name == "radio").unwrap().connected);
        // Nom inconnu : no-op, ne panique pas.
        mark_plugin_disconnected(&mut st, "inconnu");
    }
```

- [ ] **Step 2 : Lancer le test, vérifier l'échec de compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core mark_plugin_disconnected"`
Attendu : ÉCHEC de compilation (`cannot find function mark_plugin_disconnected`).

- [ ] **Step 3 : Implémenter `mark_plugin_disconnected`**

Dans `crates/ritornello-core/src/status.rs`, ajouter (au niveau module, par ex. après `parse_available_locales`) :

```rust
/// Marque le plugin `name` comme déconnecté dans l'état de statut : un plugin
/// dont le processus s'est terminé n'est plus joignable (supervision, page de
/// statut vivante). No-op si le nom est inconnu.
pub fn mark_plugin_disconnected(state: &mut StatusState, name: &str) {
    for p in &mut state.plugins {
        if p.name == name {
            p.connected = false;
        }
    }
}
```

- [ ] **Step 4 : Lancer le test, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core mark_plugin_disconnected"`
Attendu : PASS.

- [ ] **Step 5 : Ajouter la dépendance `futures`**

Dans `crates/ritornello-core/Cargo.toml`, sous `[dependencies]`, ajouter :

```toml
futures = "0.3"
```

- [ ] **Step 6 : `main.rs` — surveiller les enfants via `FuturesUnordered` et rendre `active_source` vivant**

Dans `crates/ritornello-core/src/main.rs` :

(a) Ajouter l'import en tête de fichier (avec les autres `use`) :

```rust
use futures::stream::{FuturesUnordered, StreamExt};
```

(b) Remplacer la déclaration `let mut children = Vec::new();` par (`.push()` prend `&self`, `.next()` dans la boucle exige `mut`) :

```rust
    let mut plugin_waits = FuturesUnordered::new();
```

(c) Dans la boucle `for p in &manifest.plugins`, bras `Ok(child) =>`, remplacer `children.push(child);` par la mise sous surveillance nommée :

```rust
            Ok(child) => {
                let wname = p.name.clone();
                plugin_waits.push(async move {
                    let mut child = child;
                    let status = child.wait().await;
                    (wname, status)
                });
```

(le reste du bras `Ok(child)` — connexions admin/source/display/input — est inchangé).

(d) Supprimer le commentaire obsolète au-dessus de `let mut core = core::Core::new(` (les lignes « La page de statut affiche la source active telle que persistée … aucun test ne l'exige. ») et le remplacer par :

```rust
    // Cœur. La source active affichée est tenue à jour en direct par la boucle
    // ci-dessous (mise à jour de status_state.active_source après chaque commande).
```

(e) Dans le `tokio::select!`, compléter le bras `cmd_rx` pour rafraîchir la source active, et ajouter le bras de supervision. Remplacer le bras `Some(cmd) = cmd_rx.recv() => { ... }` par :

```rust
            Some(cmd) = cmd_rx.recv() => {
                if let Err(e) = core.handle_command(cmd).await {
                    tracing::warn!("commande: {e}");
                }
                status_state.write().await.active_source = core.active_source().to_string();
            }
```

Ajouter, dans le même `select!` (par ex. avant le bras `status = mpv_child.wait()`), le bras de supervision des plugins :

```rust
            (name, status) = plugin_waits.select_next_some() => {
                tracing::warn!("plugin {name} termine: {status:?}");
                crate::status::mark_plugin_disconnected(&mut status_state.write().await, &name);
            }
```

Note : utiliser `select_next_some()` (et **non** `plugin_waits.next()` avec un motif `Some(..)`) est important — un `FuturesUnordered` **vide** (tous les plugins morts, ou aucun enfant surveillé) renvoie `Ready(None)` à chaque poll, ce qui ferait **tourner la boucle `select!` à vide** (busy-spin) avec le motif `Some`. `select_next_some()` reste `pending` quand le set est vide/terminé — comportement voulu. (`select_next_some` vient de `futures::StreamExt`, déjà importé ; `FuturesUnordered` est `FusedStream`.)

Note : le `mpv_child.wait()` reste un bras dédié **fatal** (inchangé). `plugin_waits` possède les `Child` : à l'arrêt de `main`, ils sont droppés → `kill_on_drop` préservé.

- [ ] **Step 7 : Compiler + suite complète du cœur (avec `Cargo.lock` régénéré)**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-core"`
Attendu : compilation OK (télécharge/verrouille `futures`), tous les tests PASS.

- [ ] **Step 8 : Clippy du cœur**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-core -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 9 : Commit (inclure `Cargo.lock`)**

```bash
git add crates/ritornello-core/Cargo.toml crates/ritornello-core/src/status.rs crates/ritornello-core/src/main.rs Cargo.lock
git commit -m "feat(core): supervision des plugins (FuturesUnordered) et page de statut vivante"
```

---

### Task 7 : plugin cd — suivi de piste + `wake` surchargé + test EN embarqué

**Files:**
- Modify: `crates/ritornello-plugin-cd/src/main.rs`
- Test: `crates/ritornello-plugin-cd/src/main.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes : `SourcePlugin` (avec `wake()` par défaut, Task 1), `SourceOutcome`, `SourceAction`, `ritornello_i18n::try_parse` (Task 3), `CdSource::view()` (existant).
- Produces : `CdSource::next_track`/`prev_track` mettent à jour `self.track` (borné `0..total_tracks`, sans rebouclage) et renvoient `Some(self.view())` ; `CdSource::wake()` renvoie `SourceOutcome { action: SourceAction::Noop, view: Some(self.view()) }`.
- Dépend de : Task 1 (`wake` par défaut) et Task 3 (`try_parse`).

- [ ] **Step 1 : Tests qui échouent — suivi de piste, `wake` sans Play, EN embarqué**

Ajouter dans `crates/ritornello-plugin-cd/src/main.rs`, dans `#[cfg(test)] mod tests` :

```rust
    #[tokio::test]
    async fn next_track_incremente_borne_et_renvoie_une_vue() {
        let (mut source, _p, _m) = source_with_channels();
        source.total_tracks = 3;
        source.track = 0;
        let out = source.next_track().await;
        assert_eq!(out.action, SourceAction::PlayerNext);
        assert!(out.view.is_some(), "la vue doit suivre la piste");
        assert_eq!(source.track, 1);
        // Bornage haut : sur la dernière piste, next_track ne reboucle pas.
        source.track = 2;
        let _ = source.next_track().await;
        assert_eq!(source.track, 2);
    }

    #[tokio::test]
    async fn prev_track_decremente_borne_a_zero() {
        let (mut source, _p, _m) = source_with_channels();
        source.total_tracks = 3;
        source.track = 1;
        let out = source.prev_track().await;
        assert_eq!(out.action, SourceAction::PlayerPrev);
        assert!(out.view.is_some());
        assert_eq!(source.track, 0);
        // Bornage bas : sur la première piste, prev_track reste à 0.
        let _ = source.prev_track().await;
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn wake_rafraichit_sans_jouer() {
        let (mut source, _p, _m) = source_with_channels();
        source.present = false;
        let out = source.wake().await;
        assert_eq!(out.action, SourceAction::Noop, "cd ne doit pas jouer au réveil");
        assert!(out.view.is_some());
    }

    #[test]
    fn en_embarque_cd_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(CD_EN).unwrap().is_empty());
    }
```

- [ ] **Step 2 : Lancer les tests, vérifier l'échec**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-cd"`
Attendu : ÉCHEC — `next_track`/`prev_track` ne modifient pas `self.track` et renvoient `view: None` ; `wake()` (défaut = `activate`) renverrait `Noop` mais `en_embarque_cd` compile après Task 3 (les tests de piste échouent sur les assertions).

- [ ] **Step 3 : Implémenter le suivi de piste et surcharger `wake()`**

Dans `crates/ritornello-plugin-cd/src/main.rs`, dans `impl SourcePlugin for CdSource`, remplacer `next_track` et `prev_track` :

```rust
    async fn next_track(&mut self) -> SourceOutcome {
        // Le lecteur ne remonte pas l'index réel : on suit l'index demandé,
        // borné à la dernière piste connue (pas de rebouclage).
        if self.total_tracks > 0 {
            self.track = (self.track + 1).min(self.total_tracks as i64 - 1);
        }
        SourceOutcome { action: SourceAction::PlayerNext, view: Some(self.view()) }
    }
    async fn prev_track(&mut self) -> SourceOutcome {
        self.track = (self.track - 1).max(0);
        SourceOutcome { action: SourceAction::PlayerPrev, view: Some(self.view()) }
    }
```

Ajouter la surcharge `wake()` dans le même `impl` (par ex. après `activate`) :

```rust
    async fn wake(&mut self) -> SourceOutcome {
        // Réveil : rafraîchir l'affichage (« pas de disque » / infos disque)
        // sans émettre de Play — le cd ne se lance pas tout seul.
        SourceOutcome { action: SourceAction::Noop, view: Some(self.view()) }
    }
```

- [ ] **Step 4 : Lancer les tests, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-cd"`
Attendu : tous les tests PASS (dont les tests existants `resultat_perime_ignore_resultat_frais_applique` et `view_utilise_le_catalogue_apres_set_locale`).

- [ ] **Step 5 : Clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-cd -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-plugin-cd/src/main.rs
git commit -m "feat(cd): suivi de piste (next/prev_track) et wake() sans lecture automatique"
```

---

### Task 8 : plugin radio — découpler les deux moitiés + test EN embarqué

**Files:**
- Modify: `crates/ritornello-plugin-radio/src/main.rs`
- Test: `crates/ritornello-plugin-radio/src/main.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes : `run_source_plugin`, `run_admin_plugin` (avec politique « ligne malformée = log+ignore » de Task 2), `ritornello_i18n::try_parse` (Task 3), `RADIO_EN` (const de crate).
- Produces : `main()` lance les deux moitiés en tâches **indépendantes** via `tokio::join!` (sans court-circuit) — la terminaison/erreur de l'une n'interrompt pas l'autre, chacune loggée.
- Dépend de : Task 2 (politique serveur), Task 3 (`try_parse`).

- [ ] **Step 1 : Test qui échoue — EN embarqué radio non vide**

Ajouter dans `crates/ritornello-plugin-radio/src/main.rs`, dans `#[cfg(test)] mod tests` :

```rust
    #[test]
    fn en_embarque_radio_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(RADIO_EN).unwrap().is_empty());
    }
```

- [ ] **Step 2 : Lancer le test, vérifier l'état**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio en_embarque_radio"`
Attendu : PASS si `RADIO_EN` est valide (le test verrouille le contrat : une faute future dans `en.toml` le fait échouer). Si ÉCHEC, corriger `crates/ritornello-plugin-radio/src/locales/en.toml`.

- [ ] **Step 3 : Découpler les deux moitiés (tâches indépendantes)**

Dans `crates/ritornello-plugin-radio/src/main.rs`, dans `main()`, remplacer le bloc `tokio::try_join!(...)?; Ok(())` :

```rust
    // Les deux moitiés sont indépendantes : une panne (déconnexion, erreur
    // d'écriture) sur la socket admin ne doit pas tuer la lecture audio, et
    // réciproquement. tokio::join! attend les deux sans court-circuit.
    let source_fut = async {
        if let Err(e) = run_source_plugin(source, &socket_path).await {
            tracing::warn!("plugin radio (moitie source) termine: {e}");
        }
    };
    let admin_fut = async {
        if let Err(e) = run_admin_plugin(admin, &admin_socket).await {
            tracing::warn!("plugin radio (moitie admin) termine: {e}");
        }
    };
    tokio::join!(source_fut, admin_fut);
    Ok(())
```

Note : la garantie « une moitié en panne n'arrête pas l'autre » repose sur (a) la politique « log+ignore » côté serveur (Task 2, testée dans le SDK : `source_ignore_ligne_invalide_et_repond_a_la_suivante`) et (b) `tokio::join!` qui n'annule aucune branche. Un test d'intégration bout-en-bout des deux sockets simultanées n'est pas ajouté (logique en `main()`, non unitaire) — vérification manuelle notée.

- [ ] **Step 4 : Suite complète + compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-radio"`
Attendu : compilation OK, tous les tests PASS (dont `empty_preset_utilise_le_catalogue_apres_set_locale`).

- [ ] **Step 5 : Clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-radio -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-plugin-radio/src/main.rs
git commit -m "feat(radio): decoupler source et admin en taches independantes (join sans court-circuit)"
```

---

### Task 9 : plugin mce — désambiguïsation du périphérique

**Files:**
- Modify: `crates/ritornello-plugin-mce/src/input.rs`
- Test: `crates/ritornello-plugin-mce/src/input.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes : `evdev::{Device, enumerate}` (existant), variable d'env `RITORNELLO_MCE_DEVICE`.
- Produces : `pub fn select_device_path(candidates: &[(PathBuf, String)], name_contains: &str) -> Option<PathBuf>` (logique pure : filtre par sous-chaîne du nom, `warn!` si >1 candidat, renvoie le premier) ; `find_device` ouvre exactement `RITORNELLO_MCE_DEVICE` si défini, sinon énumère → `select_device_path` → `Device::open`.

- [ ] **Step 1 : Tests qui échouent — sélection pure + chemin forcé par env**

Ajouter à la fin de `crates/ritornello-plugin-mce/src/input.rs` :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_device_path_prend_le_seul_candidat_correspondant() {
        let cands = vec![
            (PathBuf::from("/dev/input/event0"), "USB Keyboard".to_string()),
            (PathBuf::from("/dev/input/event1"), "Media Center Ed. eHome".to_string()),
        ];
        assert_eq!(
            select_device_path(&cands, "Media Center"),
            Some(PathBuf::from("/dev/input/event1"))
        );
    }

    #[test]
    fn select_device_path_prend_le_premier_si_plusieurs_candidats() {
        // Récepteur MCE exposant deux nœuds au nom similaire : on prend le
        // premier (et un warn est loggé). Ici on vérifie le choix déterministe.
        let cands = vec![
            (PathBuf::from("/dev/input/event2"), "eHome Infrared Transceiver".to_string()),
            (PathBuf::from("/dev/input/event3"), "eHome Infrared Transceiver Consumer Control".to_string()),
        ];
        assert_eq!(
            select_device_path(&cands, "ehome"),
            Some(PathBuf::from("/dev/input/event2"))
        );
    }

    #[test]
    fn select_device_path_aucun_candidat() {
        let cands = vec![(PathBuf::from("/dev/input/event0"), "USB Keyboard".to_string())];
        assert_eq!(select_device_path(&cands, "Media Center"), None);
    }

    #[test]
    fn find_device_utilise_le_chemin_force_par_env() {
        // Chemin forcé inexistant : find_device DOIT tenter de l'ouvrir (et
        // échouer en le mentionnant), prouvant qu'il n'a pas fait de recherche.
        std::env::set_var("RITORNELLO_MCE_DEVICE", "/dev/input/inexistant-xyz");
        let res = find_device("peu importe");
        std::env::remove_var("RITORNELLO_MCE_DEVICE");
        let err = res.expect_err("l'ouverture du chemin forcé doit échouer");
        assert!(format!("{err:#}").contains("inexistant-xyz"));
    }
}
```

- [ ] **Step 2 : Lancer les tests, vérifier l'échec de compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-mce"`
Attendu : ÉCHEC de compilation (`cannot find function select_device_path`).

- [ ] **Step 3 : Séparer la sélection pure et gérer le chemin forcé**

Dans `crates/ritornello-plugin-mce/src/input.rs`, ajouter l'import et remplacer `find_device` :

```rust
use std::path::PathBuf;

/// Choisit le chemin du périphérique evdev à ouvrir parmi `candidates`
/// (chemin, nom), par sous-chaîne insensible à la casse. Renvoie `None` si
/// aucun nom ne correspond. Si plusieurs correspondent (récepteurs MCE
/// exposant plusieurs nœuds), loggue un `warn!` listant les candidats puis
/// prend le premier. Fonction pure, testable, séparée de l'ouverture réelle.
pub fn select_device_path(candidates: &[(PathBuf, String)], name_contains: &str) -> Option<PathBuf> {
    let needle = name_contains.to_lowercase();
    let matches: Vec<&(PathBuf, String)> = candidates
        .iter()
        .filter(|(_, name)| name.to_lowercase().contains(&needle))
        .collect();
    if matches.len() > 1 {
        let liste: Vec<String> = matches
            .iter()
            .map(|(p, n)| format!("{} ({})", p.display(), n))
            .collect();
        tracing::warn!(
            "plusieurs périphériques correspondent à « {name_contains} », on prend le premier: {}",
            liste.join(", ")
        );
    }
    matches.first().map(|(p, _)| p.clone())
}

pub fn find_device(name_contains: &str) -> Result<Device> {
    if let Ok(forced) = std::env::var("RITORNELLO_MCE_DEVICE") {
        let dev = Device::open(&forced)
            .with_context(|| format!("ouverture du périphérique forcé {forced}"))?;
        tracing::info!("télécommande (forcée): {} ({forced})", dev.name().unwrap_or("?"));
        return Ok(dev);
    }
    let candidates: Vec<(PathBuf, String)> = evdev::enumerate()
        .map(|(path, dev)| (path, dev.name().unwrap_or("").to_string()))
        .collect();
    match select_device_path(&candidates, name_contains) {
        Some(path) => {
            let dev = Device::open(&path)
                .with_context(|| format!("ouverture de {}", path.display()))?;
            tracing::info!("télécommande: {} ({})", dev.name().unwrap_or("?"), path.display());
            Ok(dev)
        }
        None => anyhow::bail!("aucun périphérique input dont le nom contient « {name_contains} »"),
    }
}
```

- [ ] **Step 4 : Lancer les tests, vérifier le succès**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-mce"`
Attendu : les 4 tests PASS.

- [ ] **Step 5 : Clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-mce -- -D warnings"`
Attendu : aucun warning.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-plugin-mce/src/input.rs
git commit -m "feat(mce): desambiguisation du peripherique (env RITORNELLO_MCE_DEVICE + warn multi-candidats)"
```

---

## Vérification finale

- [ ] **Suite complète du workspace + clippy global**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test --workspace && cargo clippy --workspace -- -D warnings"`
Attendu : tous les tests PASS, aucun warning.
