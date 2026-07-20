# Display en plugin + sélecteur de sortie audio Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retirer le genre de plugin `Sink` (jamais implémenté), promouvoir l'affichage au rang de plugin `Display` (sur le modèle d'`Input`, en sens inverse), et ajouter un sélecteur de sortie audio basé sur les périphériques ALSA connus de l'OS.

**Architecture:** `Display` est un protocole à sens unique cœur → plugin, réutilisant directement le type `View` déjà existant (pas de type d'enveloppe, comme `Input` réutilise `Command` directement). La console actuelle déménage à l'identique dans un nouveau binaire `radio-pi-plugin-console`. La sortie audio reste entièrement interne au cœur : `aplay -L` énumère les périphériques, le trait `Player` gagne `set_audio_device`, la page de statut expose un sélecteur.

**Tech Stack:** Rust 2021 (workspace Cargo existant), tokio, serde/serde_json, axum.

Spec : `docs/superpowers/specs/2026-07-21-display-plugin-audio-output-design.md`.

## Global Constraints

- Développement sous WSL. Toutes les commandes `cargo` via `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo ..."`.
- Transport du plugin Display : identique aux autres (socket Unix, JSON par ligne, **le plugin lie et écoute, le cœur se connecte** avec la même boucle de retry que les clients existants).
- Protocole Display : à sens unique **cœur → plugin**, une ligne JSON par mise à jour, **`View` sérialisée directement** (pas de type d'enveloppe — même principe qu'`Input`, qui sérialise `Command` directement).
- La connexion au plugin Display doit être **concurrente** avec les connexions aux plugins Source au démarrage (ne pas réintroduire le défaut déjà corrigé dans l'architecture précédente où une connexion lente bloquait tout le démarrage).
- `Sink` disparaît entièrement : `radio-pi-proto::sink`, `SinkPlugin`/`SinkOutcome`/`run_sink_plugin`/`SinkClient` dans `radio-pi-plugin-sdk`, `PluginKind::Sink` dans le cœur.
- Sortie audio : énumération via `aplay -L` (ALSA, cohérent avec le choix jack/ALSA déjà fait), sélection appliquée à mpv via `set_property audio-device <périphérique>` (propriété mpv modifiable à chaud). Persisté dans `PersistedState`, réappliqué au `resume()`.
- La page de statut du cœur reflète la sélection audio telle que soumise (mise à jour par le PUT lui-même), sans lecture en direct de l'état interne du cœur — même principe déjà accepté pour `active_source` (figé, pas de synchronisation temps réel).
- Un commit par étape « Commit », message en français, préfixe conventionnel. Pas de `unsafe`. Zéro régression sur Radio/CD/MCE.

---

## Organisation des fichiers (changements par rapport à l'existant)

```
crates/
  radio-pi-proto/
    src/sink.rs                  # SUPPRIMÉ
    src/lib.rs                   # modifié (retrait des re-exports sink)
  radio-pi-plugin-sdk/
    src/server.rs                # Sink retiré, DisplayPlugin/run_display_plugin ajoutés
    src/client.rs                # SinkClient retiré, DisplayClient ajouté
    src/lib.rs                   # modifié
  radio-pi-core/
    src/plugins.rs                # PluginKind::Sink -> Display, #[allow(dead_code)] retirés
    src/display.rs                # SUPPRIMÉ (déménagé dans le nouveau plugin)
    src/audio_output.rs            # NOUVEAU
    src/player/mod.rs              # + set_audio_device
    src/player/mpv.rs              # + set_audio_device
    src/state.rs                  # + audio_device: Option<String>
    src/core.rs                   # + audio_device, set_audio_device()
    src/status.rs                  # + endpoints et formulaire sortie audio
    src/main.rs                   # câblage Display + canal audio-output
  radio-pi-plugin-console/         # NOUVELLE CRATE
    Cargo.toml
    src/main.rs
    src/display.rs                # porté à l'identique depuis radio-pi-core
deploy/
  plugins.example.toml            # + entrée console
  deploy.sh                       # + 4e binaire
README.md                         # RADIO_PI_CONSOLE_TTY, sélecteur audio
```

---

### Task 1: `radio-pi-proto` — retrait de `Sink`

**Files:**
- Delete: `crates/radio-pi-proto/src/sink.rs`
- Modify: `crates/radio-pi-proto/src/lib.rs`

**Interfaces:**
- Produces: `radio_pi_proto::lib.rs` sans `mod sink;` ni les re-exports `SinkMessage`/`SinkReq`/`SinkRequest`.

- [ ] **Step 1: Supprimer le fichier**

```bash
git rm crates/radio-pi-proto/src/sink.rs
```

- [ ] **Step 2: Mettre à jour `lib.rs`**

```rust
pub mod command;
pub mod source;
pub mod view;

pub use command::Command;
pub use source::{SourceAction, SourceMessage, SourceReq, SourceRequest};
pub use view::View;
```

- [ ] **Step 3: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-proto && cargo clippy -p radio-pi-proto -- -D warnings"`
Expected: 6 tests passing (8 précédents moins les 2 tests de `sink.rs`), 0 warning.

- [ ] **Step 4: Commit**

```bash
git add crates/radio-pi-proto
git commit -m "refactor(proto): retrait du genre de plugin Sink (jamais implemente)"
```

---

### Task 2: `radio-pi-plugin-sdk` — retrait de Sink, ajout de `DisplayPlugin` (côté serveur)

**Files:**
- Modify: `crates/radio-pi-plugin-sdk/src/server.rs`

**Interfaces:**
- Consumes: `radio_pi_proto::View` (réutilisée directement, aucun nouveau type).
- Produces: `radio_pi_plugin_sdk::{DisplayPlugin, run_display_plugin}` ; `SinkOutcome`/`SinkPlugin`/`run_sink_plugin` supprimés.

- [ ] **Step 1: Retirer la section Sink**

Dans `crates/radio-pi-plugin-sdk/src/server.rs`, supprimer entièrement :
- le bloc `use radio_pi_proto::{SinkReq, SinkRequest, SinkMessage};` et tout ce qui suit jusqu'à la fin de `run_sink_plugin` (struct `SinkOutcome`, trait `SinkPlugin`, fonction `run_sink_plugin`)
- le module `#[cfg(test)] mod sink_tests { ... }` en bas du fichier

- [ ] **Step 2: Écrire le test de `DisplayPlugin` (échec attendu)**

Ajouter, à la place de l'ancien `mod sink_tests`, un nouveau module de test (même style que le test Source existant, avec un vrai socket sur répertoire temporaire) :

```rust
#[cfg(test)]
mod display_tests {
    use super::*;
    use radio_pi_proto::View;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct RecordingDisplay {
        views: Arc<Mutex<Vec<View>>>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for RecordingDisplay {
        async fn show(&mut self, view: View) -> Result<()> {
            self.views.lock().unwrap().push(view);
            Ok(())
        }
    }

    #[tokio::test]
    async fn recoit_les_vues_en_ligne() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let plugin = RecordingDisplay::default();
        let views = plugin.views.clone();
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            let _ = run_display_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin display");
        use tokio::io::AsyncWriteExt;
        let mut write = stream;
        let v = View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() };
        write.write_all(format!("{}\n", serde_json::to_string(&v).unwrap()).as_bytes()).await.unwrap();

        for _ in 0..50 {
            if !views.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(views.lock().unwrap().as_slice(), &[v]);
    }
}
```

- [ ] **Step 3: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk display"`
Expected: FAIL (compilation — `DisplayPlugin`/`run_display_plugin` non définis).

- [ ] **Step 4: Implémenter** (ajouter avant le module de test, à l'endroit où se trouvait la section Sink)

```rust
use radio_pi_proto::View;

#[async_trait::async_trait]
pub trait DisplayPlugin: Send + 'static {
    async fn show(&mut self, view: View) -> Result<()>;
}

/// Lie `socket_path`, accepte une connexion (le cœur), puis affiche chaque
/// vue reçue jusqu'à fermeture de la connexion. Protocole à sens unique :
/// aucune réponse n'est attendue.
pub async fn run_display_plugin(mut plugin: impl DisplayPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("liaison de {}", socket_path.display()))?;
    let (stream, _) = listener.accept().await?;
    let (read, _write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let view: View = serde_json::from_str(&line)
            .with_context(|| format!("vue invalide: {line}"))?;
        plugin.show(view).await?;
    }
    Ok(())
}
```

- [ ] **Step 5: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk && cargo clippy -p radio-pi-plugin-sdk -- -D warnings"`
Expected: 3 tests passing (`dialogue_requete_reponse`, `commandes_envoyees_en_ligne`, `recoit_les_vues_en_ligne` — le test Sink a disparu), 0 warning.

- [ ] **Step 6: Commit**

```bash
git add crates/radio-pi-plugin-sdk/src/server.rs
git commit -m "refactor(sdk): retrait du harnais serveur Sink, ajout de DisplayPlugin/run_display_plugin"
```

---

### Task 3: `radio-pi-plugin-sdk` — retrait de `SinkClient`, ajout de `DisplayClient` (côté cœur)

**Files:**
- Modify: `crates/radio-pi-plugin-sdk/src/client.rs`
- Modify: `crates/radio-pi-plugin-sdk/src/lib.rs`

**Interfaces:**
- Produces: `radio_pi_plugin_sdk::DisplayClient::connect(socket_path: &Path) -> Result<Arc<DisplayClient>>`, `DisplayClient::send(&self, view: &View) -> Result<()>` ; `SinkClient` supprimé.

- [ ] **Step 1: Retirer `SinkClient`**

Dans `crates/radio-pi-plugin-sdk/src/client.rs`, supprimer entièrement la struct `SinkClient` et son `impl` (des deux `use` de types Sink au début du fichier, ne retirer que `SinkMessage, SinkReq, SinkRequest` de la ligne d'import — garder `Command`, `SourceAction`, `SourceMessage`, `SourceReq`, `SourceRequest`, `View`).

- [ ] **Step 2: Écrire le test de `DisplayClient` (échec attendu)**

Ajouter dans le module `#[cfg(test)] mod tests` existant :

```rust
    #[tokio::test]
    async fn display_client_envoie_la_vue_en_ligne() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("display.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let v: View = serde_json::from_str(&line).unwrap();
            assert_eq!(v.line2, "FIP");
        });

        let client = DisplayClient::connect(&socket).await.unwrap();
        client.send(&View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
```

- [ ] **Step 3: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk client"`
Expected: FAIL (compilation, `DisplayClient` non défini).

- [ ] **Step 4: Implémenter** (ajouter après `SourceClient`, à la place de l'ancien `SinkClient`)

```rust
pub struct DisplayClient {
    writer: Mutex<OwnedWriteHalf>,
}

impl DisplayClient {
    pub async fn connect(socket_path: &Path) -> Result<Arc<Self>> {
        let stream = connect_with_retry(socket_path).await?;
        let (_read, write) = stream.into_split();
        Ok(Arc::new(Self { writer: Mutex::new(write) }))
    }

    pub async fn send(&self, view: &View) -> Result<()> {
        let mut w = self.writer.lock().await;
        w.write_all(format!("{}\n", serde_json::to_string(view)?).as_bytes()).await?;
        Ok(())
    }
}
```

- [ ] **Step 5: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk"`
Expected: 5 tests passing total in the crate — 3 in `server.rs` (`dialogue_requete_reponse`, `commandes_envoyees_en_ligne`, `recoit_les_vues_en_ligne`) + 2 in `client.rs` (`source_client_correle_par_id_et_relaie_la_vue`, `display_client_envoie_la_vue_en_ligne`) — 0 warning.

- [ ] **Step 6: Mettre à jour `lib.rs`**

```rust
pub mod client;
pub mod server;

pub use client::{run_input_client, DisplayClient, SourceClient};
pub use server::{
    run_display_plugin, run_input_plugin, run_source_plugin, DisplayPlugin, InputPlugin,
    SourceOutcome, SourcePlugin,
};
```

- [ ] **Step 7: Vérifier le clippy final**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo clippy -p radio-pi-plugin-sdk -- -D warnings"`
Expected: 0 warning.

- [ ] **Step 8: Commit**

```bash
git add crates/radio-pi-plugin-sdk
git commit -m "refactor(sdk): retrait de SinkClient, ajout de DisplayClient (cote coeur)"
```

---

### Task 4: `radio-pi-core` — `plugins.rs` : `Sink` → `Display`, nettoyage des `#[allow(dead_code)]`

**Files:**
- Modify: `crates/radio-pi-core/src/plugins.rs`

**Interfaces:**
- Produces: `plugins::PluginKind { Source, Display, Input }` (remplace `Sink`).

- [ ] **Step 1: Réécrire le fichier**

```rust
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Display,
    Input,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub kind: PluginKind,
    pub exec: String,
    #[serde(default)]
    pub admin_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginConfig>,
}

impl PluginManifest {
    /// Un fichier absent donne un manifeste vide : le cœur démarre sans
    /// plugin plutôt que d'échouer (cohérent avec le traitement déjà
    /// existant pour `stations.toml`).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(_) => Ok(Self::default()),
        }
    }
}

/// Spawn un plugin en lui passant le chemin de la socket qu'il doit lier.
pub fn spawn(exec: &str, socket_path: &Path) -> Result<tokio::process::Child> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(tokio::process::Command::new(exec)
        .arg("--socket")
        .arg(socket_path)
        .kill_on_drop(true)
        .spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_un_manifeste_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
kind = "source"
exec = "/usr/local/lib/radio-pi/plugins/radio-pi-plugin-radio"

[[plugin]]
name = "console"
kind = "display"
exec = "/usr/local/lib/radio-pi/plugins/radio-pi-plugin-console"
admin_url = "http://raspberrypi.local:8081"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 2);
        assert_eq!(m.plugins[0].name, "radio");
        assert_eq!(m.plugins[0].kind, PluginKind::Source);
        assert_eq!(m.plugins[1].kind, PluginKind::Display);
        assert_eq!(m.plugins[1].admin_url.as_deref(), Some("http://raspberrypi.local:8081"));
    }

    #[test]
    fn manifeste_absent_donne_liste_vide() {
        let dir = tempfile::tempdir().unwrap();
        let m = PluginManifest::load(&dir.path().join("absent.toml")).unwrap();
        assert!(m.plugins.is_empty());
    }
}
```

(Les `#[allow(dead_code)]` de la version précédente sont retirés : `PluginKind`, `PluginConfig`, `PluginManifest`, `load` et `spawn` sont déjà tous utilisés par `main.rs` depuis la livraison précédente — c'était une note laissée par la revue finale de l'époque.)

- [ ] **Step 2: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core plugins"`
Expected: FAIL — `main.rs` référence encore `PluginKind::Sink` (retiré à la Task 6). Pour que ce test compile isolément à cette étape, ce n'est pas grave : `cargo test -p radio-pi-core plugins` compile TOUT le crate `radio-pi-core`, y compris `main.rs`. Corriger temporairement `main.rs` en remplaçant la branche `PluginKind::Sink => { ... }` par `PluginKind::Display => { plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "display".into(), connected: false, admin_url: p.admin_url.clone() }); }` (stub minimal, sera remplacé pour de bon à la Task 6).

Run à nouveau : `cargo test -p radio-pi-core plugins` → 2 tests passing, `cargo clippy -p radio-pi-core -- -D warnings` → 0 warning.

- [ ] **Step 3: Commit**

```bash
git add crates/radio-pi-core/src/plugins.rs crates/radio-pi-core/src/main.rs
git commit -m "refactor(core): PluginKind::Sink -> Display, retrait des allow(dead_code) devenus inutiles"
```

---

### Task 5: `radio-pi-plugin-console` — nouveau plugin Display (portage de la console)

**Files:**
- Create: `crates/radio-pi-plugin-console/Cargo.toml`
- Create: `crates/radio-pi-plugin-console/src/display.rs` (porté à l'identique depuis `crates/radio-pi-core/src/display.rs`)
- Create: `crates/radio-pi-plugin-console/src/main.rs`
- Modify: root `Cargo.toml` (ajouter le membre)

**Interfaces:**
- Produces: le binaire `radio-pi-plugin-console`, qui lit `--socket <path>`, implémente `radio_pi_plugin_sdk::DisplayPlugin`.

- [ ] **Step 1: Ajouter le membre au workspace racine**

Dans `Cargo.toml` (racine), ajouter `"crates/radio-pi-plugin-console",` à la liste `members` (ordre alphabétique, après `"crates/radio-pi-plugin-cd"` et avant `"crates/radio-pi-plugin-mce"` — même style que les autres).

- [ ] **Step 2: `Cargo.toml`**

```toml
[package]
name = "radio-pi-plugin-console"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "radio-pi-plugin-console"
path = "src/main.rs"

[dependencies]
anyhow = "1"
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
async-trait = "0.1"
radio-pi-proto = { path = "../radio-pi-proto" }
radio-pi-plugin-sdk = { path = "../radio-pi-plugin-sdk" }
```

- [ ] **Step 3: Porter `display.rs` à l'identique**

Contenu de `crates/radio-pi-plugin-console/src/display.rs` — copie exacte de `crates/radio-pi-core/src/display.rs` (aucune modification : `render_console`, `ConsoleDisplay::open`/`show`, le test `rendu_efface_et_affiche_trois_lignes`), à l'exception d'un seul import : `use radio_pi_proto::View;` reste identique (déjà le bon chemin, aucun changement nécessaire — le fichier se copie mot pour mot).

- [ ] **Step 4: Vérifier que le test porté passe**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-console display"`
Expected: 1 test passing (`rendu_efface_et_affiche_trois_lignes`).

- [ ] **Step 5: `main.rs`**

```rust
mod display;

use anyhow::Result;
use async_trait::async_trait;
use display::ConsoleDisplay;
use radio_pi_plugin_sdk::{run_display_plugin, DisplayPlugin};
use radio_pi_proto::View;
use std::path::PathBuf;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct ConsolePlugin {
    display: ConsoleDisplay,
}

#[async_trait]
impl DisplayPlugin for ConsolePlugin {
    async fn show(&mut self, view: View) -> Result<()> {
        self.display.show(&view)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = socket_path_from_args();
    let tty = PathBuf::from(env_or("RADIO_PI_CONSOLE_TTY", "/dev/tty1"));

    let display = ConsoleDisplay::open(&tty)?;
    run_display_plugin(ConsolePlugin { display }, &socket_path).await
}
```

- [ ] **Step 6: Vérifier le succès complet**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build -p radio-pi-plugin-console && cargo test -p radio-pi-plugin-console && cargo clippy -p radio-pi-plugin-console -- -D warnings"`
Expected: build OK, 1 test passing, 0 warning.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/radio-pi-plugin-console
git commit -m "feat: radio-pi-plugin-console (DisplayPlugin, portage de ConsoleDisplay)"
```

---

### Task 6: `radio-pi-core` — retrait de `display.rs`, câblage du plugin Display dans `main.rs`

**Files:**
- Delete: `crates/radio-pi-core/src/display.rs`
- Modify: `crates/radio-pi-core/src/main.rs`

**Interfaces:**
- Consumes: `radio_pi_plugin_sdk::DisplayClient`.
- Produces : le cœur pousse la `View` courante vers le plugin Display connecté au lieu de la rendre lui-même.

- [ ] **Step 1: Supprimer `display.rs`**

```bash
git rm crates/radio-pi-core/src/display.rs
```

- [ ] **Step 2: Réécrire `main.rs`**

```rust
mod core;
mod player;
mod plugins;
mod state;
mod status;
mod types;

use crate::plugins::{PluginKind, PluginManifest};
use crate::status::{AppState, LogBuffer, LogBufferWriter, PluginStatus, StatusState};
use crate::types::Event;
use anyhow::{Context, Result};
use radio_pi_proto::{Command, View};
use radio_pi_plugin_sdk::{run_input_client, DisplayClient, SourceClient};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[async_trait::async_trait]
impl core::Source for SourceClient {
    async fn request(&self, req: radio_pi_proto::SourceReq) -> Result<radio_pi_proto::SourceAction> {
        SourceClient::request(self, req).await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let log_buffer = Arc::new(LogBuffer::new(50));
    let log_buffer_for_writer = log_buffer.clone();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(move || LogBufferWriter(log_buffer_for_writer.clone()))
                .with_filter(LevelFilter::WARN),
        )
        .init();

    let plugins_path = PathBuf::from(env_or("RADIO_PI_PLUGINS", "/etc/radio-pi/plugins.toml"));
    let state_path = PathBuf::from(env_or("RADIO_PI_STATE", "/var/lib/radio-pi/state.json"));
    let mpv_socket = PathBuf::from(env_or("RADIO_PI_MPV_SOCKET", "/run/radio-pi/mpv.sock"));
    let mpv_bin = env_or("RADIO_PI_MPV_BIN", "mpv");
    let cd_dev = env_or("RADIO_PI_CD_DEV", "/dev/sr0");
    let http_addr = env_or("RADIO_PI_HTTP", "0.0.0.0:8080");
    let runtime_dir = env_or("RADIO_PI_RUNTIME_DIR", "/run/radio-pi");

    let manifest = PluginManifest::load(&plugins_path)
        .with_context(|| format!("chargement de {}", plugins_path.display()))?;
    let persisted = state::load(&state_path);

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
    let (ev_tx, mut ev_rx) = broadcast::channel::<Event>(64);
    let (view_tx, mut view_rx) = watch::channel(View::default());
    let (source_view_tx, mut source_view_rx) = mpsc::channel::<(String, View)>(32);
    let (audio_tx, mut audio_rx) = mpsc::channel::<String>(4);

    // mpv (inchangé).
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, ev_tx.clone())
            .await
            .context("démarrage de mpv")?;

    // Spawn et connexion de chaque plugin déclaré.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    let mut plugin_statuses = Vec::new();
    let mut children = Vec::new();
    let mut source_connects = Vec::new();
    let mut display_connect = None;

    for p in &manifest.plugins {
        let socket_path = PathBuf::from(format!("{runtime_dir}/{}.sock", p.name));
        match plugins::spawn(&p.exec, &socket_path) {
            Ok(child) => {
                children.push(child);
                match p.kind {
                    PluginKind::Source => {
                        let name = p.name.clone();
                        let admin_url = p.admin_url.clone();
                        let view_tx = source_view_tx.clone();
                        source_connects.push(tokio::spawn(async move {
                            let result = SourceClient::connect(&socket_path, name.clone(), view_tx).await;
                            (name, admin_url, result)
                        }));
                    }
                    PluginKind::Display => {
                        let name = p.name.clone();
                        let admin_url = p.admin_url.clone();
                        display_connect = Some(tokio::spawn(async move {
                            let result = DisplayClient::connect(&socket_path).await;
                            (name, admin_url, result)
                        }));
                    }
                    PluginKind::Input => {
                        let tx = cmd_tx.clone();
                        let socket_for_task = socket_path.clone();
                        let name = p.name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_input_client(&socket_for_task, tx).await {
                                tracing::warn!("plugin input {name} deconnecte: {e}");
                            }
                        });
                        plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "input".into(), connected: true, admin_url: p.admin_url.clone() });
                    }
                }
            }
            Err(e) => {
                tracing::warn!("lancement du plugin {} impossible: {e}", p.name);
                plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: format!("{:?}", p.kind).to_lowercase(), connected: false, admin_url: p.admin_url.clone() });
            }
        }
    }

    for handle in source_connects {
        let (name, admin_url, result) = handle.await.context("tache de connexion plugin source interrompue")?;
        match result {
            Ok(client) => {
                sources.insert(name.clone(), client);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: true, admin_url });
            }
            Err(e) => {
                tracing::warn!("plugin {} indisponible: {e}", name);
                plugin_statuses.push(PluginStatus { name, kind: "source".into(), connected: false, admin_url });
            }
        }
    }

    let mut display_client: Option<Arc<DisplayClient>> = None;
    if let Some(handle) = display_connect {
        let (name, admin_url, result) = handle.await.context("tache de connexion plugin display interrompue")?;
        match result {
            Ok(client) => {
                display_client = Some(client);
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: true, admin_url });
            }
            Err(e) => {
                tracing::warn!("plugin display {name} indisponible: {e}");
                plugin_statuses.push(PluginStatus { name, kind: "display".into(), connected: false, admin_url });
            }
        }
    }

    if sources.is_empty() {
        anyhow::bail!("aucune source disponible (plugins.toml vide ou tous les plugins source indisponibles)");
    }

    // Relais des vues vers le plugin d'affichage, s'il est connecté.
    match display_client {
        Some(display_client) => {
            tokio::spawn(async move {
                loop {
                    if view_rx.changed().await.is_err() {
                        break;
                    }
                    let v = view_rx.borrow_and_update().clone();
                    if let Err(e) = display_client.send(&v).await {
                        tracing::warn!("affichage: {e}");
                    }
                }
            });
        }
        None => tracing::warn!("pas de plugin display connecte, on continue sans affichage"),
    }

    // Page de statut du cœur (plugins, source active, dernières erreurs, sortie audio).
    let status_state = Arc::new(RwLock::new(StatusState {
        plugins: plugin_statuses,
        active_source: persisted.active_source.clone(),
    }));
    let audio_current = Arc::new(RwLock::new(persisted.audio_device.clone()));
    {
        let app = status::router(AppState {
            status: status_state.clone(),
            logs: log_buffer.clone(),
            audio_current: audio_current.clone(),
            audio_tx: audio_tx.clone(),
        });
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("page de statut sur http://{http_addr}/status");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("serveur de statut: {e}");
            }
        });
    }

    // Cœur. La page de statut affiche la source active telle que persistée
    // au démarrage (`persisted.active_source`) ; elle n'est pas mise à jour
    // en direct si l'utilisateur change de source ensuite — hors périmètre
    // de cette livraison (aucun test ne l'exige).
    let mut core = core::Core::new(mpv_player, sources, persisted, state_path, view_tx);
    core.resume().await?;

    let mut retry_at: Option<tokio::time::Instant> = None;

    loop {
        let retry_sleep = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                if let Err(e) = core.handle_command(cmd).await {
                    tracing::warn!("commande: {e}");
                }
            }
            Ok(ev) = ev_rx.recv() => {
                if matches!(ev, Event::Title(_) | Event::PlaybackActive) {
                    retry_at = None;
                }
                if let Some(delay) = core.handle_event(ev).await {
                    retry_at = Some(tokio::time::Instant::now() + delay);
                }
            }
            Some((name, view)) = source_view_rx.recv() => {
                core.handle_source_view(&name, view);
            }
            Some(device) = audio_rx.recv() => {
                if let Err(e) = core.set_audio_device(device).await {
                    tracing::warn!("changement de sortie audio: {e}");
                }
            }
            _ = retry_sleep => {
                retry_at = None;
                if let Err(e) = core.retry_stream().await {
                    tracing::warn!("retry flux: {e}");
                }
            }
            status = mpv_child.wait() => {
                anyhow::bail!("mpv termine ({status:?}), arret pour relance par systemd");
            }
        }
    }
}
```

(`core::Core::set_audio_device` et `AppState { audio_current, audio_tx, .. }` sont introduits par les Tasks 8 et 10 — ce fichier ne compilera pleinement qu'une fois ces tâches faites. Continuer malgré tout : les tâches suivantes referment ces trous dans l'ordre.)

- [ ] **Step 3: Vérifier ce qui peut l'être dès maintenant**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build -p radio-pi-core"`
Expected: échoue (méthodes/champs pas encore ajoutés — `core.set_audio_device`, `AppState.audio_current`/`audio_tx`, `persisted.audio_device`). C'est attendu : les Tasks 7-10 ferment ces trous. Ne pas chercher à faire compiler ce fichier isolément à cette étape.

- [ ] **Step 4: Commit**

```bash
git add crates/radio-pi-core/src/display.rs crates/radio-pi-core/src/main.rs
git commit -m "refactor(core): retrait de l'affichage local, cablage du plugin Display (connexion concurrente)"
```

---

### Task 7: `radio-pi-core` — `Player::set_audio_device`

**Files:**
- Modify: `crates/radio-pi-core/src/player/mod.rs`
- Modify: `crates/radio-pi-core/src/player/mpv.rs`

**Interfaces:**
- Produces: `Player::set_audio_device(&self, device: &str) -> Result<()>`, implémenté pour `MpvPlayer` via `set_property audio-device <device>`.

- [ ] **Step 1: Étendre le trait**

Dans `crates/radio-pi-core/src/player/mod.rs`, ajouter une méthode à la fin du trait `Player` :

```rust
    async fn set_audio_device(&self, device: &str) -> Result<()>;
```

- [ ] **Step 2: Implémenter pour `MpvPlayer`**

Dans `crates/radio-pi-core/src/player/mpv.rs`, ajouter à l'`impl super::Player for MpvPlayer` (après `set_mute`) :

```rust
    async fn set_audio_device(&self, device: &str) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("audio-device"), json!(device)]).await?;
        Ok(())
    }
```

- [ ] **Step 3: Mettre à jour la `FakePlayer` de test dans `crates/radio-pi-core/src/core.rs`**

Ajouter à l'`impl crate::player::Player for FakePlayer` (dans le module `#[cfg(test)]` de `core.rs`) :

```rust
        async fn set_audio_device(&self, device: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("audio_device {device}"));
            Ok(())
        }
```

- [ ] **Step 4: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build -p radio-pi-core 2>&1 | tail -30"`
Expected: toujours des erreurs de compilation liées aux autres trous (Tasks 8/10) mais plus aucune liée à `set_audio_device`/`Player`. Si une erreur `set_audio_device` apparaît encore, vérifier que `mpv.rs` et `core.rs` ont bien été mis à jour.

- [ ] **Step 5: Commit**

```bash
git add crates/radio-pi-core/src/player crates/radio-pi-core/src/core.rs
git commit -m "feat(player): set_audio_device sur le trait Player, implemente pour mpv"
```

---

### Task 8: `radio-pi-core` — persistance et application de la sortie audio dans `Core`

**Files:**
- Modify: `crates/radio-pi-core/src/state.rs`
- Modify: `crates/radio-pi-core/src/core.rs`

**Interfaces:**
- Produces: `state::PersistedState { active_source: String, volume: u8, audio_device: Option<String> }` ; `core::Core::set_audio_device(&mut self, device: String) -> Result<()>`.

- [ ] **Step 1: Tests de `state.rs` (échec attendu)**

Remplacer le test `roundtrip_save_load` et `defaut_est_radio_vol60` dans `crates/radio-pi-core/src/state.rs` par :

```rust
    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState { active_source: "cd".into(), volume: 35, audio_device: Some("bluealsa:DEV=XX".into()) };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn defaut_est_radio_vol60_sans_sortie_choisie() {
        let d = PersistedState::default();
        assert_eq!(d.active_source, "radio");
        assert_eq!(d.volume, 60);
        assert_eq!(d.audio_device, None);
    }
```

- [ ] **Step 2: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core state"`
Expected: FAIL (compilation, `PersistedState` n'a pas de champ `audio_device`).

- [ ] **Step 3: Implémenter**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub active_source: String,
    pub volume: u8,
    #[serde(default)]
    pub audio_device: Option<String>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self { active_source: "radio".into(), volume: 60, audio_device: None }
    }
}
```

(`#[serde(default)]` permet de charger un `state.json` écrit par une version antérieure du programme, sans ce champ, sans erreur — il vaudra `None`.)

- [ ] **Step 4: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core state"`
Expected: 3 tests passing.

- [ ] **Step 5: Tests de `core.rs` (échec attendu)**

Ajouter dans le module `#[cfg(test)] mod tests` de `crates/radio-pi-core/src/core.rs` (après `veille_affiche_un_message_dedie_et_ignore_les_vues_pendant_ce_temps`) :

```rust
    #[tokio::test]
    async fn resume_applique_la_sortie_audio_persistee() {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: Arc::new(Mutex::new(Vec::new())) }));
        let (tx, _rx) = watch::channel(View::default());
        let persisted = PersistedState { active_source: "radio".into(), volume: 60, audio_device: Some("bluealsa:DEV=XX".into()) };
        let mut core = Core::new(player, sources, persisted, dir.path().join("state.json"), tx);
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device bluealsa:DEV=XX".to_string()));
    }

    #[tokio::test]
    async fn set_audio_device_applique_et_persiste() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device("hw:CARD=Headphones".into()).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device hw:CARD=Headphones".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device.as_deref(), Some("hw:CARD=Headphones"));
    }
```

- [ ] **Step 6: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core core"`
Expected: FAIL (compilation, `Core::set_audio_device` non défini, et `setup()` ne construit pas encore `PersistedState` avec `audio_device` — vérifier que `PersistedState::default()` utilisé par `setup()` compile bien grâce à Step 3 ci-dessus).

- [ ] **Step 7: Implémenter**

Dans `crates/radio-pi-core/src/core.rs` :
1. Ajouter un champ `audio_device: Option<String>,` à la struct `Core` (après `retry_count`).
2. Dans `Core::new`, l'initialiser : ajouter `audio_device: persisted.audio_device.clone(),` dans le constructeur de `Self`.
3. Dans `resume()`, appliquer la sortie persistée juste après le volume :

```rust
    pub async fn resume(&mut self) -> Result<()> {
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        let action = self.active().request(SourceReq::Activate).await?;
        self.apply(action).await
    }
```

4. Ajouter une méthode publique :

```rust
    pub async fn set_audio_device(&mut self, device: String) -> Result<()> {
        self.player.set_audio_device(&device).await?;
        self.audio_device = Some(device);
        self.persist();
        Ok(())
    }
```

5. Mettre à jour `persist()` pour inclure le nouveau champ :

```rust
    fn persist(&self) {
        let st = PersistedState {
            active_source: self.active_source.clone(),
            volume: self.volume,
            audio_device: self.audio_device.clone(),
        };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistance impossible: {e}");
        }
    }
```

- [ ] **Step 8: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core core"`
Expected: 9 tests passing (7 précédents + 2 nouveaux).

- [ ] **Step 9: Commit**

```bash
git add crates/radio-pi-core/src/state.rs crates/radio-pi-core/src/core.rs
git commit -m "feat(core): persistance et application de la sortie audio choisie (PersistedState, Core::set_audio_device)"
```

---

### Task 9: `radio-pi-core` — énumération des sorties audio (`aplay -L`)

**Files:**
- Create: `crates/radio-pi-core/src/audio_output.rs`
- Modify: `crates/radio-pi-core/src/main.rs` (ajouter `mod audio_output;`)

**Interfaces:**
- Produces: `audio_output::parse_device_list(raw: &str) -> Vec<String>` (pure, testée), `audio_output::list_devices() -> anyhow::Result<Vec<String>>` (appelle `aplay -L`).

- [ ] **Step 1: Écrire le test (échec attendu)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrait_les_noms_de_peripheriques_non_indentes() {
        let raw = "null\n    Discard all samples (playback) or generate zero samples (capture)\n\
default\n    Playback/recording through the PulseAudio sound server\n\
sysdefault:CARD=Headphones\n    bcm2835 Headphones, bcm2835 Headphones\n    Default Audio Device\n";
        let devices = parse_device_list(raw);
        assert_eq!(devices, vec!["null", "default", "sysdefault:CARD=Headphones"]);
    }

    #[test]
    fn entree_vide_donne_liste_vide() {
        assert_eq!(parse_device_list(""), Vec::<String>::new());
    }
}
```

- [ ] **Step 2: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core audio_output"`
Expected: FAIL (compilation, `parse_device_list` non défini).

- [ ] **Step 3: Implémenter**

```rust
use anyhow::{bail, Result};

/// Extrait les noms de périphériques de la sortie de `aplay -L` : chaque
/// ligne non indentée est le nom d'un périphérique sélectionnable ; les
/// lignes indentées qui suivent sont une description, ignorée ici.
pub fn parse_device_list(raw: &str) -> Vec<String> {
    raw.lines()
        .filter(|l| !l.is_empty() && !l.starts_with(' ') && !l.starts_with('\t'))
        .map(|l| l.trim().to_string())
        .collect()
}

pub fn list_devices() -> Result<Vec<String>> {
    let out = std::process::Command::new("aplay").arg("-L").output()?;
    if !out.status.success() {
        bail!("aplay -L a echoue: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(parse_device_list(&String::from_utf8_lossy(&out.stdout)))
}
```

- [ ] **Step 4: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core audio_output"`
Expected: 2 tests passing.

- [ ] **Step 5: Ajouter `mod audio_output;` dans `main.rs`** (ordre alphabétique, avant `mod core;`).

- [ ] **Step 6: Commit**

```bash
git add crates/radio-pi-core/src/audio_output.rs crates/radio-pi-core/src/main.rs
git commit -m "feat(core): enumeration des sorties audio via aplay -L"
```

---

### Task 10: `radio-pi-core` — sélecteur de sortie audio sur la page de statut

**Files:**
- Modify: `crates/radio-pi-core/src/status.rs`

**Interfaces:**
- Consumes: `audio_output::list_devices`.
- Produces: `status::AppState { status, logs, audio_current: Arc<RwLock<Option<String>>>, audio_tx: mpsc::Sender<String> }` ; routes `GET`/`PUT /api/audio-output` ; formulaire ajouté à `/status`.

- [ ] **Step 1: Écrire les tests (échec attendu)**

Ajouter dans le module `#[cfg(test)] mod tests` de `crates/radio-pi-core/src/status.rs` :

```rust
    fn app_state_with_audio() -> (AppState, tokio::sync::mpsc::Receiver<String>) {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
        let state = AppState {
            status: Arc::new(tokio::sync::RwLock::new(sample())),
            logs: Arc::new(LogBuffer::new(50)),
            audio_current: Arc::new(tokio::sync::RwLock::new(Some("default".to_string()))),
            audio_tx,
        };
        (state, audio_rx)
    }

    #[tokio::test]
    async fn put_audio_output_notifie_et_met_a_jour_la_selection_affichee() {
        let (state, mut audio_rx) = app_state_with_audio();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/audio-output")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"device":"hw:CARD=Headphones"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(audio_rx.recv().await.unwrap(), "hw:CARD=Headphones");
    }

    #[tokio::test]
    async fn get_audio_output_liste_les_peripheriques_et_la_selection() {
        let (state, _audio_rx) = app_state_with_audio();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/audio-output").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], "default");
        assert!(v["devices"].is_array());
    }
```

- [ ] **Step 2: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core status"`
Expected: FAIL (compilation, `AppState` n'a pas `audio_current`/`audio_tx`, routes absentes).

- [ ] **Step 3: Implémenter**

Modifier `AppState` :

```rust
#[derive(Clone)]
pub struct AppState {
    pub status: Arc<RwLock<StatusState>>,
    pub logs: Arc<LogBuffer>,
    pub audio_current: Arc<RwLock<Option<String>>>,
    pub audio_tx: mpsc::Sender<String>,
}
```

Ajouter en tête de fichier : `use tokio::sync::mpsc;` et `use serde::Deserialize;`.

Étendre `router` :

```rust
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status_page))
        .route("/api/status", get(status_json))
        .route("/api/audio-output", get(audio_output_json).put(audio_output_put))
        .with_state(state)
}
```

Ajouter les handlers (après `status_json`) :

```rust
#[derive(Serialize)]
struct AudioOutputResponse {
    devices: Vec<String>,
    current: Option<String>,
}

async fn audio_output_json(State(state): State<AppState>) -> Json<AudioOutputResponse> {
    let devices = crate::audio_output::list_devices().unwrap_or_default();
    let current = state.audio_current.read().await.clone();
    Json(AudioOutputResponse { devices, current })
}

#[derive(Deserialize)]
struct AudioOutputRequest {
    device: String,
}

async fn audio_output_put(State(state): State<AppState>, Json(req): Json<AudioOutputRequest>) -> StatusCode {
    *state.audio_current.write().await = Some(req.device.clone());
    if state.audio_tx.send(req.device).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::NO_CONTENT
}
```

Ajouter `use axum::http::StatusCode;` en tête si absent (déjà présent dans les tests, l'ajouter aussi hors `#[cfg(test)]`).

Étendre `status_page` pour inclure le formulaire de sortie audio. Remplacer le `format!` final par :

```rust
    let devices = crate::audio_output::list_devices().unwrap_or_default();
    let current = state.audio_current.read().await.clone();
    let options: String = devices
        .iter()
        .map(|d| {
            let sel = if Some(d) == current.as_ref() { " selected" } else { "" };
            format!("<option value=\"{}\"{sel}>{}</option>", escape_html(d), escape_html(d))
        })
        .collect();
    Html(format!(
        "<!doctype html><html lang=\"fr\"><meta charset=\"utf-8\"><title>radio-pi — statut</title>\
         <h1>radio-pi</h1><p>Source active : {}</p>\
         <table border=\"1\"><tr><th>Plugin</th><th>Genre</th><th>État</th><th>Admin</th></tr>{}</table>\
         <h2>Sortie audio</h2>\
         <select id=\"audio-device\">{options}</select>\
         <button onclick=\"setAudioOutput()\">Changer</button> <span id=\"audio-msg\"></span>\
         <script>\
         async function setAudioOutput() {{\
           const device = document.getElementById('audio-device').value;\
           const r = await fetch('/api/audio-output', {{method:'PUT', headers:{{'content-type':'application/json'}}, body: JSON.stringify({{device}})}});\
           document.getElementById('audio-msg').textContent = r.ok ? 'OK' : 'Erreur';\
         }}\
         </script>\
         <h2>Dernières erreurs</h2><ul>{}</ul></html>",
        escape_html(&s.active_source), rows, logs
    ))
```

(Notez que `s` (le `StatusState` lu) doit rester accessible pour `rows`/`s.active_source` : garder le `let s = state.status.read().await;` déjà présent en haut de la fonction, et ajouter les nouvelles lignes `devices`/`current`/`options` juste avant le `Html(format!(...))` final.)

- [ ] **Step 4: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core status && cargo clippy -p radio-pi-core -- -D warnings"`
Expected: 6 tests passing (4 précédents + 2 nouveaux), 0 warning.

- [ ] **Step 5: Vérifier la compilation complète du crate**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build -p radio-pi-core && cargo test -p radio-pi-core && cargo clippy -p radio-pi-core -- -D warnings"`
Expected: build OK (tous les trous laissés par la Task 6 sont maintenant refermés), tous les tests passent (attendu : 2 plugins + 9 core + 3 state + 6 status = 20 tests), 0 warning.

- [ ] **Step 6: Commit**

```bash
git add crates/radio-pi-core/src/status.rs
git commit -m "feat(core): selecteur de sortie audio sur la page de statut (GET/PUT /api/audio-output)"
```

---

### Task 11: Build du workspace complet, déploiement, README

**Files:**
- Modify: `deploy/plugins.example.toml`
- Modify: `deploy/deploy.sh`
- Modify: `README.md`

**Interfaces:**
- Consumes: les 5 binaires du workspace (`radio-pi-core`, `radio-pi-plugin-radio`, `radio-pi-plugin-cd`, `radio-pi-plugin-mce`, `radio-pi-plugin-console`).

- [ ] **Step 1: Vérifier le workspace complet**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test --workspace"`
Expected: tous les tests passent (compter précisément à l'exécution ; aucune régression sur les crates non touchées par ce plan — `radio-pi-plugin-radio`, `radio-pi-plugin-cd`, `radio-pi-plugin-mce` restent à leurs comptes déjà connus : 9, 5, 3).

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo clippy --workspace -- -D warnings"`
Expected: 0 warning.

- [ ] **Step 2: `deploy/plugins.example.toml`** — ajouter l'entrée console :

```toml
[[plugin]]
name = "radio"
kind = "source"
exec = "/usr/local/lib/radio-pi/plugins/radio-pi-plugin-radio"
admin_url = "http://raspberrypi.local:8081"

[[plugin]]
name = "cd"
kind = "source"
exec = "/usr/local/lib/radio-pi/plugins/radio-pi-plugin-cd"

[[plugin]]
name = "mce"
kind = "input"
exec = "/usr/local/lib/radio-pi/plugins/radio-pi-plugin-mce"

[[plugin]]
name = "console"
kind = "display"
exec = "/usr/local/lib/radio-pi/plugins/radio-pi-plugin-console"
```

- [ ] **Step 3: `deploy/deploy.sh`** — ajouter le 4e binaire à la liste scp des plugins :

Remplacer la ligne :
```bash
scp "$OUT/radio-pi-plugin-radio" "$OUT/radio-pi-plugin-cd" "$OUT/radio-pi-plugin-mce" "$PI:/tmp/"
```
par :
```bash
scp "$OUT/radio-pi-plugin-radio" "$OUT/radio-pi-plugin-cd" "$OUT/radio-pi-plugin-mce" "$OUT/radio-pi-plugin-console" "$PI:/tmp/"
```

Et la ligne de déplacement distant :
```bash
&& sudo mv /tmp/radio-pi-plugin-radio /tmp/radio-pi-plugin-cd /tmp/radio-pi-plugin-mce /usr/local/lib/radio-pi/plugins/ \
```
par :
```bash
&& sudo mv /tmp/radio-pi-plugin-radio /tmp/radio-pi-plugin-cd /tmp/radio-pi-plugin-mce /tmp/radio-pi-plugin-console /usr/local/lib/radio-pi/plugins/ \
```

- [ ] **Step 4: `README.md`**

Dans la section « Plugins », ajouter une ligne mentionnant le plugin d'affichage et le sélecteur de sortie audio :

```markdown
- `radio-pi-plugin-console` est le plugin d'affichage (console HDMI, variable
  `RADIO_PI_CONSOLE_TTY`, défaut `/dev/tty1`). La page de statut du cœur
  (`http://<pi>:8080/status`) propose aussi un sélecteur de sortie audio,
  basé sur les périphériques ALSA connus du système (`aplay -L`) — une
  enceinte Bluetooth déjà appairée via `bluetoothctl` y apparaîtra
  automatiquement une fois exposée par `bluez-alsa`.
```

Dans la recette « Développement (WSL) », retirer `RADIO_PI_TTY=/dev/stdout` de la commande `cargo run -p radio-pi-core` (elle n'existe plus sur le cœur) et ajouter le plugin console au `plugins.toml` d'exemple + son propre `RADIO_PI_CONSOLE_TTY=/dev/stdout` :

```
    cat > /tmp/rp/plugins.toml <<'PLUGINS'
    [[plugin]]
    name = "radio"
    kind = "source"
    exec = "target/debug/radio-pi-plugin-radio"

    [[plugin]]
    name = "console"
    kind = "display"
    exec = "target/debug/radio-pi-plugin-console"
    PLUGINS
    cat > /tmp/rp/stations.toml <<'STATIONS'
    [[stations]]
    name = "FIP"
    url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
    preset = 1
    STATIONS
    RADIO_PI_PLUGINS=/tmp/rp/plugins.toml RADIO_PI_STATE=/tmp/rp/state.json \
    RADIO_PI_MPV_SOCKET=/tmp/rp/mpv.sock RADIO_PI_RUNTIME_DIR=/tmp/rp \
    RADIO_PI_HTTP=127.0.0.1:8080 \
    RADIO_PI_CONSOLE_TTY=/dev/stdout \
    RADIO_PI_RADIO_STATIONS=/tmp/rp/stations.toml RADIO_PI_RADIO_STATE=/tmp/rp/plugin-radio.json \
    RADIO_PI_RADIO_HTTP=127.0.0.1:8081 \
    cargo run -p radio-pi-core
```

- [ ] **Step 5: Vérifier la cross-compilation du workspace complet**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cross build --release --workspace --target armv7-unknown-linux-gnueabihf"`
Expected: 5 binaires produits sous `target/armv7-unknown-linux-gnueabihf/release/` (`radio-pi-core`, `radio-pi-plugin-radio`, `radio-pi-plugin-cd`, `radio-pi-plugin-mce`, `radio-pi-plugin-console`).

- [ ] **Step 6: Commit**

```bash
git add deploy README.md
git commit -m "feat(deploy): plugin console dans plugins.toml/deploy.sh, README a jour (Display, selecteur audio)"
```

---

### Task 12: Validation manuelle en développement (WSL)

**Files:** aucun fichier modifié.

- [ ] **Step 1: Préparer l'environnement**

Suivre exactement la recette du README mise à jour (Task 11, Step 4) : `plugins.toml` avec `radio` (source) + `console` (display), `stations.toml` avec FIP, variables d'environnement incluant `RADIO_PI_CONSOLE_TTY=/dev/stdout` et `RADIO_PI_RUNTIME_DIR=/tmp/rp`.

- [ ] **Step 2: Build debug**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build --workspace"`
Expected: 5 binaires produits sous `target/debug/`.

- [ ] **Step 3: Lancer avec un timeout et observer**

```bash
timeout 20 env \
  RADIO_PI_PLUGINS=/tmp/rp/plugins.toml \
  RADIO_PI_STATE=/tmp/rp/state.json \
  RADIO_PI_MPV_SOCKET=/tmp/rp/mpv.sock \
  RADIO_PI_RUNTIME_DIR=/tmp/rp \
  RADIO_PI_HTTP=127.0.0.1:8080 \
  RADIO_PI_CONSOLE_TTY=/dev/stdout \
  RADIO_PI_RADIO_STATIONS=/tmp/rp/stations.toml \
  RADIO_PI_RADIO_STATE=/tmp/rp/plugin-radio.json \
  RADIO_PI_RADIO_HTTP=127.0.0.1:8081 \
  target/debug/radio-pi-core
```

Attendu :
- la console (stdout) affiche `RADIO  P1` / `FIP` — **via le plugin console**, pas via un rendu direct du cœur (confirmer en observant que le plugin console apparaît bien dans `/api/status` avec `"connected":true`, `"kind":"display"`) ;
- `curl http://127.0.0.1:8080/api/status` montre les deux plugins (`radio`, `console`) connectés ;
- `curl http://127.0.0.1:8080/api/audio-output` renvoie une liste de périphériques (si `aplay` est installé sous WSL — sinon `devices` peut être vide, ce qui est toléré, pas une erreur) et `"current": null` (aucune sortie choisie encore) ;
- `curl -X PUT http://127.0.0.1:8080/api/audio-output -H 'content-type: application/json' -d '{"device":"default"}'` renvoie 204, et un `curl` suivant sur `/api/audio-output` montre `"current":"default"`.

- [ ] **Step 4: Documenter le résultat**

Noter dans le ledger de suivi (`.superpowers/sdd/progress.md`) : confirmation que le mécanisme Display fonctionne de bout en bout à travers un vrai processus séparé, et que le sélecteur de sortie audio répond. Si `aplay` est absent de l'environnement WSL de test, le noter comme limitation d'environnement (pas un défaut de code) — à revérifier sur le vrai Pi où ALSA est déjà en place.

---

## Auto-relecture (à faire par le contrôleur avant dispatch, pas une tâche à exécuter)

- **Couverture de la spec** : retrait de Sink (Tasks 1-4), Display en plugin sur le modèle d'Input inversé (Tasks 2-3, 5-6), sélecteur de sortie audio basé sur `aplay -L` avec persistance (Tasks 7-10), déploiement (Task 11), validation (Task 12) — tout couvert.
- **Cohérence des types** : `View` réutilisée directement pour Display (pas de nouveau type, cohérent avec la décision de simplification actée pendant l'écriture du plan) ; `PluginKind::Display` cohérent entre `plugins.rs`, `main.rs`, `plugins.example.toml`.
- **Point d'attention pour l'implémenteur de la Task 6** : le fichier ne compile pas seul avant les Tasks 7-10 — c'est volontaire (même pattern que la tâche de câblage de l'architecture précédente), la Task 4 gère déjà un cas similaire avec un stub temporaire pour `PluginKind::Display`.
