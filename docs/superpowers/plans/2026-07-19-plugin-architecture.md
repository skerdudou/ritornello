# Architecture à plugins (Source/Sink/Input) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transformer radio-pi (mono-binaire) en workspace Cargo où Radio, CD et la télécommande MCE sont des processus plugins séparés parlant au cœur par IPC (socket Unix, JSON par ligne), sans perdre aucune fonctionnalité utilisateur existante.

**Architecture:** Trois nouvelles crates partagées (`radio-pi-proto` : types du protocole ; `radio-pi-plugin-sdk` : harnais serveur pour écrire un plugin + client pour le cœur) ; le cœur (`radio-pi-core`) devient un orchestrateur générique (registre de sources, registre de sinks, `Player` mpv toujours partagé et piloté uniquement par le cœur) ; trois binaires plugins (`radio-pi-plugin-radio`, `radio-pi-plugin-cd`, `radio-pi-plugin-mce`) portent la logique existante derrière les traits du SDK.

**Tech Stack:** Rust 2021 (workspace Cargo), tokio, serde/serde_json, axum (statut du cœur + admin du plugin radio), async-trait, evdev, libc, reqwest.

Spec : `docs/superpowers/specs/2026-07-18-plugin-architecture-design.md`.

## Global Constraints

- Développement sous WSL (evdev, sockets Unix, ioctl). Toutes les commandes `cargo` s'exécutent via `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo ..."`.
- Transport IPC : socket Unix, une ligne JSON par message. **Le plugin lie et écoute le socket ; le cœur spawn le plugin puis s'y connecte avec une boucle de retry** (même pattern que `player::mpv::start` existant) — jamais l'inverse.
- Chaque plugin est un processus enfant du cœur (comme mpv aujourd'hui), déclaré dans `/etc/radio-pi/plugins.toml`. La mort d'un plugin Source/Sink/Input est **tolérée** (marqué indisponible, le reste continue) ; seule la mort de mpv fait sortir le processus (systemd relance tout — inchangé).
- Le cœur reste seul maître du `Player` (mpv) partagé. Aucun plugin ne joue de l'audio lui-même.
- Table de routage des commandes (dérivée de la spec + du code actuel) :
  - **Cœur-direct** (jamais relayé) : `VolumeUp`/`VolumeDown`/`Mute` (Player), `PlayPause`/`Stop` (Player, inchangé vs aujourd'hui), `Power` (veille : stop Player + `deactivate` source active ; réveil : `activate` + reprise volume).
  - **Relayé à la source active** : `Select(n)` (ex-`Preset`), `Next`/`Prev` (ex-`StationNext`/`StationPrev`, **ne bascule plus jamais la source active** — changement de comportement assumé, voir spec), `NextTrack`/`PrevTrack` (inchangés dans leur rôle), `Eject` (le plugin CD exécute lui-même l'éjection matérielle et répond `Stop` ; les autres répondent `Noop`).
  - **`SourceCycle`** (ex-`ToggleMode`) : le cœur décide seul de la source suivante (cycle sur les sources enregistrées), envoie `deactivate`/`activate`.
  - **`ReloadStations` disparaît** du protocole partagé : `stations.toml` et son rechargement deviennent internes à `radio-pi-plugin-radio` (sa propre page web appelle directement sa propre fonction de rechargement, dans le même processus — plus besoin de traverser l'IPC).
- Page web : le cœur garde une petite page de statut en lecture seule (liste des plugins, connecté/déconnecté, source active, dernières erreurs) ; chaque plugin qui a besoin d'admin (Radio, pour éditer ses stations) porte sa propre petite page web sur son propre port.
- Un commit par étape « Commit », messages en français, préfixes conventionnels.
- Pas de nouvelle fonctionnalité utilisateur hors ce qui est explicitement listé ci-dessus (le saut direct à la piste CD via `Select(n)` est la seule capacité nouvelle, actée dans la spec).

---

## Organisation des fichiers (workspace cible)

```
radio-pi/
  Cargo.toml                        # workspace root
  crates/
    radio-pi-proto/                 # types du protocole (Command, Source*, Sink*, View)
    radio-pi-plugin-sdk/             # harnais serveur (plugin) + client (cœur)
    radio-pi-core/                  # orchestrateur (ex-binaire actuel, réduit)
    radio-pi-plugin-radio/          # présélections + page web stations
    radio-pi-plugin-cd/             # ioctl CD + MusicBrainz
    radio-pi-plugin-mce/            # evdev + mapping touches
  deploy/
    plugins.example.toml
    radio-pi.service                # inchangé
    deploy.sh                       # étendu au workspace
```

---

### Task 1: Conversion en workspace Cargo (mécanique, comportement inchangé)

**Files:**
- Create: `Cargo.toml` (racine, workspace)
- Create: `crates/radio-pi-core/Cargo.toml`
- Move: `src/**` → `crates/radio-pi-core/src/**` (contenu inchangé, aucune ligne modifiée)
- Move: `tests/fixtures/mb_discid.json` → `crates/radio-pi-core/tests/fixtures/mb_discid.json`
- Modify: `.gitattributes`, `.gitignore` (chemins `/target` restent valables à la racine du workspace, aucun changement de contenu nécessaire)

**Interfaces:**
- Produces: le binaire `radio-pi-core` (renommé depuis `radio-pi`), tous les modules existants inchangés (`config`, `state`, `types`, `keymap`, `input`, `player`, `display`, `web`, `cd`, `musicbrainz`, `core`), 37 tests existants qui passent à l'identique.

- [ ] **Step 1: Déplacer les sources**

Dans WSL :
```bash
cd /mnt/c/projets/perso/radio-pi
mkdir -p crates/radio-pi-core
git mv src crates/radio-pi-core/src
git mv tests crates/radio-pi-core/tests
```

- [ ] **Step 2: Écrire `crates/radio-pi-core/Cargo.toml`** (contenu identique à l'ancien `Cargo.toml` racine, nom de package changé)

```toml
[package]
name = "radio-pi-core"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "radio-pi-core"
path = "src/main.rs"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
axum = "0.7"
evdev = { version = "0.12", features = ["tokio"] }
libc = "0.2"
async-trait = "0.1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }

[dev-dependencies]
tempfile = "3"
tower = { version = "0.4", features = ["util"] }
http-body-util = "0.1"
```

- [ ] **Step 3: Écrire le `Cargo.toml` racine (workspace)**

```toml
[workspace]
resolver = "2"
members = [
    "crates/radio-pi-proto",
    "crates/radio-pi-plugin-sdk",
    "crates/radio-pi-core",
    "crates/radio-pi-plugin-radio",
    "crates/radio-pi-plugin-cd",
    "crates/radio-pi-plugin-mce",
]
```

(Les membres autres que `radio-pi-core` n'existent pas encore : c'est normal, les tâches suivantes les créent. `cargo build`/`cargo test` à cette étape doivent être lancés avec `-p radio-pi-core` tant que les autres crates n'existent pas.)

- [ ] **Step 4: Vérifier que rien n'a régressé**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core"`
Expected: 37 tests passing, 0 failed (même compte qu'avant la conversion).

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo clippy -p radio-pi-core -- -D warnings"`
Expected: aucun warning.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: conversion en workspace Cargo, code existant deplace vers crates/radio-pi-core"
```

---

### Task 2: `radio-pi-proto` — types du protocole partagés

**Files:**
- Create: `crates/radio-pi-proto/Cargo.toml`
- Create: `crates/radio-pi-proto/src/lib.rs`
- Create: `crates/radio-pi-proto/src/command.rs`
- Create: `crates/radio-pi-proto/src/view.rs`
- Create: `crates/radio-pi-proto/src/source.rs`
- Create: `crates/radio-pi-proto/src/sink.rs`
- Modify: root `Cargo.toml` (rien à changer, déjà listé Task 1)

**Interfaces:**
- Produces: `radio_pi_proto::Command` (enum, `Serialize`/`Deserialize`), `radio_pi_proto::View { line1, line2, line3: String }`, `radio_pi_proto::source::{SourceReq, SourceRequest, SourceAction, SourceMessage}`, `radio_pi_proto::sink::{SinkReq, SinkRequest, SinkMessage}`.

- [ ] **Step 1: `Cargo.toml`**

```toml
[package]
name = "radio-pi-proto"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }

[dev-dependencies]
serde_json = "1"
```

- [ ] **Step 2: Écrire les tests (échec attendu) — `crates/radio-pi-proto/src/command.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "arg")]
pub enum Command {
    Select(u8),
    Next,
    Prev,
    NextTrack,
    PrevTrack,
    VolumeUp,
    VolumeDown,
    Mute,
    SourceCycle,
    PlayPause,
    Stop,
    Eject,
    Power,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json_avec_argument() {
        let c = Command::Select(3);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"cmd":"Select","arg":3}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), c);
    }

    #[test]
    fn roundtrip_json_sans_argument() {
        let c = Command::Stop;
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"cmd":"Stop"}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), c);
    }
}
```

- [ ] **Step 3: `crates/radio-pi-proto/src/view.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub line1: String,
    pub line2: String,
    pub line3: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json() {
        let v = View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() };
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<View>(&json).unwrap(), v);
    }
}
```

- [ ] **Step 4: `crates/radio-pi-proto/src/source.rs`**

```rust
use crate::view::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum SourceReq {
    Activate,
    Deactivate,
    Select(u8),
    Next,
    Prev,
    NextTrack,
    PrevTrack,
    Eject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: SourceReq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "data")]
pub enum SourceAction {
    Noop,
    Play { uri: String },
    Stop,
    PlayerNext,
    PlayerPrev,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMessage {
    /// `Some(id)` = réponse corrélée à une requête ; `None` = notification spontanée.
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub action: Option<SourceAction>,
    #[serde(default)]
    pub view: Option<View>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = SourceRequest { id: 7, req: SourceReq::Select(3) };
        let json = serde_json::to_string(&r).unwrap();
        let back: SourceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.req, SourceReq::Select(3));
    }

    #[test]
    fn message_reponse_avec_action_et_vue() {
        let m = SourceMessage {
            id: Some(1),
            action: Some(SourceAction::Play { uri: "http://fip".into() }),
            view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, Some(1));
        assert_eq!(back.action, Some(SourceAction::Play { uri: "http://fip".into() }));
    }

    #[test]
    fn message_notification_sans_id() {
        let m = SourceMessage { id: None, action: None, view: Some(View::default()) };
        let json = serde_json::to_string(&m).unwrap();
        let back: SourceMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, None);
        assert_eq!(back.action, None);
    }
}
```

- [ ] **Step 5: `crates/radio-pi-proto/src/sink.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "req")]
pub enum SinkReq {
    Activate,
    Deactivate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkRequest {
    pub id: u64,
    #[serde(flatten)]
    pub req: SinkReq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkMessage {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub audio_device: Option<String>,
    #[serde(default)]
    pub connected: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = SinkRequest { id: 1, req: SinkReq::Activate };
        let json = serde_json::to_string(&r).unwrap();
        let back: SinkRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.req, SinkReq::Activate);
    }

    #[test]
    fn message_avec_peripherique_audio() {
        let m = SinkMessage {
            id: Some(1),
            audio_device: Some("alsa/bluealsa:DEV=XX".into()),
            connected: None,
            error: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SinkMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.audio_device.as_deref(), Some("alsa/bluealsa:DEV=XX"));
    }
}
```

- [ ] **Step 6: `crates/radio-pi-proto/src/lib.rs`**

```rust
pub mod command;
pub mod sink;
pub mod source;
pub mod view;

pub use command::Command;
pub use sink::{SinkMessage, SinkReq, SinkRequest};
pub use source::{SourceAction, SourceMessage, SourceReq, SourceRequest};
pub use view::View;
```

- [ ] **Step 7: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-proto"`
Expected: 8 tests passing (2 command + 1 view + 3 source + 2 sink).

- [ ] **Step 8: Commit**

```bash
git add crates/radio-pi-proto
git commit -m "feat: crate radio-pi-proto (Command, View, protocole Source/Sink)"
```

---

### Task 3: `radio-pi-plugin-sdk` — transport ligne JSON + traits serveur

**Files:**
- Create: `crates/radio-pi-plugin-sdk/Cargo.toml`
- Create: `crates/radio-pi-plugin-sdk/src/lib.rs`
- Create: `crates/radio-pi-plugin-sdk/src/server.rs`

**Interfaces:**
- Consumes: `radio_pi_proto::{SourceReq, SourceRequest, SourceAction, SourceMessage, SinkReq, SinkRequest, SinkMessage, View, Command}`.
- Produces: traits `SourcePlugin`, `SinkPlugin`, `InputPlugin` ; fonctions `run_source_plugin(plugin: impl SourcePlugin, socket_path: &Path) -> Result<()>`, `run_sink_plugin(plugin: impl SinkPlugin, socket_path: &Path) -> Result<()>`, `run_input_plugin(plugin: impl InputPlugin, socket_path: &Path) -> Result<()>` ; struct `SourceOutcome { action: SourceAction, view: Option<View> }`, `SinkOutcome { audio_device: Option<String>, error: Option<String> }`.

- [ ] **Step 1: `Cargo.toml`**

```toml
[package]
name = "radio-pi-plugin-sdk"
version = "0.1.0"
edition = "2021"

[dependencies]
radio-pi-proto = { path = "../radio-pi-proto" }
anyhow = "1"
async-trait = "0.1"
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Écrire les tests (échec attendu) — `crates/radio-pi-plugin-sdk/src/server.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use radio_pi_proto::{SourceAction, View};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    struct EchoSource;

    #[async_trait::async_trait]
    impl SourcePlugin for EchoSource {
        async fn activate(&mut self) -> SourceOutcome {
            SourceOutcome {
                action: SourceAction::Play { uri: "http://fip".into() },
                view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
            }
        }
        async fn deactivate(&mut self) -> SourceOutcome {
            SourceOutcome { action: SourceAction::Stop, view: None }
        }
        async fn select(&mut self, n: u8) -> SourceOutcome {
            SourceOutcome { action: SourceAction::Play { uri: format!("http://station-{n}") }, view: None }
        }
        async fn next(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
        async fn prev(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
        async fn next_track(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::PlayerNext, view: None } }
        async fn prev_track(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::PlayerPrev, view: None } }
        async fn eject(&mut self) -> SourceOutcome { SourceOutcome { action: SourceAction::Noop, view: None } }
    }

    #[tokio::test]
    async fn dialogue_requete_reponse() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_source_plugin(EchoSource, &socket_for_server).await.unwrap();
        });
        // laisse le temps au serveur de lier le socket
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin");
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: radio_pi_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://fip".into() }));
        assert_eq!(msg.view.unwrap().line2, "FIP");

        write.write_all(b"{\"id\":2,\"req\":\"Select\",\"arg\":3}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: radio_pi_proto::SourceMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(2));
        assert_eq!(msg.action, Some(SourceAction::Play { uri: "http://station-3".into() }));
    }
}
```

- [ ] **Step 3: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk"`
Expected: FAIL (compilation — `SourcePlugin`, `SourceOutcome`, `run_source_plugin` non définis).

- [ ] **Step 4: Implémenter (au-dessus du bloc tests) — trait Source + serveur**

```rust
use anyhow::{Context, Result};
use radio_pi_proto::{SourceAction, SourceReq, SourceRequest, SourceMessage, View};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

pub struct SourceOutcome {
    pub action: SourceAction,
    pub view: Option<View>,
}

#[async_trait::async_trait]
pub trait SourcePlugin: Send + 'static {
    async fn activate(&mut self) -> SourceOutcome;
    async fn deactivate(&mut self) -> SourceOutcome;
    async fn select(&mut self, n: u8) -> SourceOutcome;
    async fn next(&mut self) -> SourceOutcome;
    async fn prev(&mut self) -> SourceOutcome;
    async fn next_track(&mut self) -> SourceOutcome;
    async fn prev_track(&mut self) -> SourceOutcome;
    async fn eject(&mut self) -> SourceOutcome;

    /// Notification spontanée (ex. changement de piste, métadonnées arrivées en
    /// différé). Par défaut ne se termine jamais : un plugin sans notification
    /// spontanée (Radio) n'a rien à écrire de plus.
    async fn poll_notification(&mut self) -> Option<View> {
        std::future::pending().await
    }
}

/// Lie `socket_path`, accepte une connexion (le cœur), puis traite les
/// requêtes et les notifications spontanées jusqu'à fermeture de la
/// connexion.
pub async fn run_source_plugin(mut plugin: impl SourcePlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("liaison de {}", socket_path.display()))?;
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let req: SourceRequest = serde_json::from_str(&line)
                    .with_context(|| format!("requete invalide: {line}"))?;
                let outcome = match req.req {
                    SourceReq::Activate => plugin.activate().await,
                    SourceReq::Deactivate => plugin.deactivate().await,
                    SourceReq::Select(n) => plugin.select(n).await,
                    SourceReq::Next => plugin.next().await,
                    SourceReq::Prev => plugin.prev().await,
                    SourceReq::NextTrack => plugin.next_track().await,
                    SourceReq::PrevTrack => plugin.prev_track().await,
                    SourceReq::Eject => plugin.eject().await,
                };
                let msg = SourceMessage { id: Some(req.id), action: Some(outcome.action), view: outcome.view };
                write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
            }
            view = plugin.poll_notification() => {
                if let Some(view) = view {
                    let msg = SourceMessage { id: None, action: None, view: Some(view) };
                    write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
                }
            }
        }
    }
}
```

- [ ] **Step 5: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk"`
Expected: 1 test passing (`dialogue_requete_reponse`).

- [ ] **Step 6: Ajouter le trait Sink + serveur, avec test (TDD, même méthode qu'aux étapes 2-5)**

Test à ajouter dans le même fichier :

```rust
#[cfg(test)]
mod sink_tests {
    use super::*;

    struct FakeSink;

    #[async_trait::async_trait]
    impl SinkPlugin for FakeSink {
        async fn activate(&mut self) -> SinkOutcome {
            SinkOutcome { audio_device: Some("alsa/bluealsa:DEV=XX".into()), error: None }
        }
        async fn deactivate(&mut self) -> SinkOutcome {
            SinkOutcome { audio_device: None, error: None }
        }
    }

    #[tokio::test]
    async fn dialogue_requete_reponse_sink() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sink.sock");
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            run_sink_plugin(FakeSink, &socket_for_server).await.unwrap();
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin sink");
        let (read, mut write) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(read).lines();
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        write.write_all(b"{\"id\":1,\"req\":\"Activate\"}\n").await.unwrap();
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: radio_pi_proto::SinkMessage = serde_json::from_str(&line).unwrap();
        assert_eq!(msg.id, Some(1));
        assert_eq!(msg.audio_device.as_deref(), Some("alsa/bluealsa:DEV=XX"));
    }
}
```

Implémentation à ajouter (après avoir vérifié l'échec de compilation) :

```rust
use radio_pi_proto::{SinkReq, SinkRequest, SinkMessage};

pub struct SinkOutcome {
    pub audio_device: Option<String>,
    pub error: Option<String>,
}

#[async_trait::async_trait]
pub trait SinkPlugin: Send + 'static {
    async fn activate(&mut self) -> SinkOutcome;
    async fn deactivate(&mut self) -> SinkOutcome;

    async fn poll_notification(&mut self) -> Option<SinkOutcome> {
        std::future::pending().await
    }
}

pub async fn run_sink_plugin(mut plugin: impl SinkPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    let (stream, _) = listener.accept().await?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let req: SinkRequest = serde_json::from_str(&line)?;
                let outcome = match req.req {
                    SinkReq::Activate => plugin.activate().await,
                    SinkReq::Deactivate => plugin.deactivate().await,
                };
                let msg = SinkMessage {
                    id: Some(req.id),
                    audio_device: outcome.audio_device,
                    connected: None,
                    error: outcome.error,
                };
                write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
            }
            outcome = plugin.poll_notification() => {
                if let Some(outcome) = outcome {
                    let msg = SinkMessage {
                        id: None,
                        audio_device: outcome.audio_device,
                        connected: Some(outcome.error.is_none()),
                        error: outcome.error,
                    };
                    write.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await?;
                }
            }
        }
    }
}
```

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk"`
Expected: 2 tests passing.

- [ ] **Step 7: Ajouter le trait Input + serveur, avec test**

Test à ajouter :

```rust
#[cfg(test)]
mod input_tests {
    use super::*;
    use radio_pi_proto::Command;

    struct FixedCommands {
        remaining: Vec<Command>,
    }

    #[async_trait::async_trait]
    impl InputPlugin for FixedCommands {
        async fn next_command(&mut self) -> anyhow::Result<Command> {
            if self.remaining.is_empty() {
                std::future::pending::<()>().await;
            }
            Ok(self.remaining.remove(0))
        }
    }

    #[tokio::test]
    async fn commandes_envoyees_en_ligne() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let socket_for_server = socket.clone();
        let plugin = FixedCommands { remaining: vec![Command::Select(3), Command::Stop] };
        tokio::spawn(async move {
            let _ = run_input_plugin(plugin, &socket_for_server).await;
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let stream = client.expect("connexion au plugin input");
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stream).lines();

        let l1 = lines.next_line().await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Command>(&l1).unwrap(), Command::Select(3));
        let l2 = lines.next_line().await.unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Command>(&l2).unwrap(), Command::Stop);
    }
}
```

Implémentation :

```rust
use radio_pi_proto::Command;

#[async_trait::async_trait]
pub trait InputPlugin: Send + 'static {
    async fn next_command(&mut self) -> Result<Command>;
}

pub async fn run_input_plugin(mut plugin: impl InputPlugin, socket_path: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    let (stream, _) = listener.accept().await?;
    let (_read, mut write) = stream.into_split();
    loop {
        let cmd = plugin.next_command().await?;
        write.write_all(format!("{}\n", serde_json::to_string(&cmd)?).as_bytes()).await?;
    }
}
```

- [ ] **Step 8: Vérifier le succès complet**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk && cargo clippy -p radio-pi-plugin-sdk -- -D warnings"`
Expected: 3 tests passing, 0 warning.

- [ ] **Step 9: `crates/radio-pi-plugin-sdk/src/lib.rs`**

```rust
pub mod server;

pub use server::{
    run_input_plugin, run_sink_plugin, run_source_plugin, InputPlugin, SinkOutcome, SinkPlugin,
    SourceOutcome, SourcePlugin,
};
```

- [ ] **Step 10: Commit**

```bash
git add crates/radio-pi-plugin-sdk
git commit -m "feat: radio-pi-plugin-sdk, harnais serveur Source/Sink/Input"
```

---

### Task 4: `radio-pi-plugin-sdk` — clients pour le cœur (Source/Sink/Input)

**Files:**
- Create: `crates/radio-pi-plugin-sdk/src/client.rs`
- Modify: `crates/radio-pi-plugin-sdk/src/lib.rs`

**Interfaces:**
- Consumes: `radio_pi_proto::{SourceReq, SourceRequest, SourceMessage, SourceAction, SinkReq, SinkRequest, SinkMessage, View, Command}`.
- Produces: `SourceClient::connect(socket_path: &Path, name: String, view_tx: mpsc::Sender<(String, View)>) -> Result<Arc<SourceClient>>`, `SourceClient::request(&self, req: SourceReq) -> Result<SourceAction>` ; `SinkClient::connect(socket_path: &Path, status_tx: mpsc::Sender<(String, bool, Option<String>)>) -> Result<Arc<SinkClient>>`, `SinkClient::request(&self, req: SinkReq) -> Result<Option<String>>` (périphérique audio ou `None`) ; `InputClient::connect(socket_path: &Path, cmd_tx: mpsc::Sender<Command>) -> Result<()>` (tâche de fond, ne retourne qu'en cas d'erreur de connexion).

Ce module reproduit, pour le protocole plugin, exactement le pattern déjà utilisé par `player::mpv::MpvIpc` (corrélation par `id`, tâche de lecture en tâche de fond, `oneshot` par requête).

- [ ] **Step 1: Écrire les tests (échec attendu)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use radio_pi_proto::{SourceAction, View};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn source_client_correle_par_id_et_relaie_la_vue() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let socket_for_server = socket.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: radio_pi_proto::SourceRequest = serde_json::from_str(&line).unwrap();
            let msg = radio_pi_proto::SourceMessage {
                id: Some(req.id),
                action: Some(SourceAction::Play { uri: "http://fip".into() }),
                view: Some(View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() }),
            };
            write.write_all(format!("{}\n", serde_json::to_string(&msg).unwrap()).as_bytes()).await.unwrap();
            let _ = socket_for_server; // garde le chemin vivant pour le débogage
        });

        let (view_tx, mut view_rx) = tokio::sync::mpsc::channel(8);
        let client = SourceClient::connect(&socket, "radio".into(), view_tx).await.unwrap();
        let action = client.request(radio_pi_proto::SourceReq::Activate).await.unwrap();
        assert_eq!(action, SourceAction::Play { uri: "http://fip".into() });
        let (name, view) = view_rx.recv().await.unwrap();
        assert_eq!(name, "radio");
        assert_eq!(view.line2, "FIP");
    }
}
```

- [ ] **Step 2: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk client"`
Expected: FAIL (compilation, `SourceClient` non défini).

- [ ] **Step 3: Implémenter `crates/radio-pi-plugin-sdk/src/client.rs`**

```rust
use anyhow::{bail, Context, Result};
use radio_pi_proto::{Command, SinkMessage, SinkReq, SinkRequest, SourceAction, SourceMessage, SourceReq, SourceRequest, View};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

async fn connect_with_retry(socket_path: &Path) -> Result<UnixStream> {
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket_path).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    stream.with_context(|| format!("connexion a {} (10 s)", socket_path.display()))
}

pub struct SourceClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<SourceAction>>>>,
    next_id: AtomicU64,
}

impl SourceClient {
    pub async fn connect(
        socket_path: &Path,
        name: String,
        view_tx: mpsc::Sender<(String, View)>,
    ) -> Result<Arc<Self>> {
        let stream = connect_with_retry(socket_path).await?;
        let (read, write) = stream.into_split();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self { writer: Mutex::new(write), pending: pending.clone(), next_id: AtomicU64::new(1) });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<SourceMessage>(&line) else { continue };
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
            tracing::warn!("connexion au plugin source fermee");
        });
        Ok(client)
    }

    pub async fn request(&self, req: SourceReq) -> Result<SourceAction> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = SourceRequest { id, req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(action)) => Ok(action),
            Ok(Err(_)) => bail!("plugin source: reponse abandonnee"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("plugin source: timeout de requete")
            }
        }
    }
}

pub struct SinkClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>>,
    next_id: AtomicU64,
}

impl SinkClient {
    pub async fn connect(
        socket_path: &Path,
        name: String,
        status_tx: mpsc::Sender<(String, bool, Option<String>)>,
    ) -> Result<Arc<Self>> {
        let stream = connect_with_retry(socket_path).await?;
        let (read, write) = stream.into_split();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let client = Arc::new(Self { writer: Mutex::new(write), pending: pending.clone(), next_id: AtomicU64::new(1) });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<SinkMessage>(&line) else { continue };
                if let Some(id) = msg.id {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(msg.audio_device.clone());
                    }
                }
                if let Some(connected) = msg.connected {
                    let _ = status_tx.try_send((name.clone(), connected, msg.error.clone()));
                }
            }
        });
        Ok(client)
    }

    pub async fn request(&self, req: SinkReq) -> Result<Option<String>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = SinkRequest { id, req };
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", serde_json::to_string(&msg)?).as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(dev)) => Ok(dev),
            Ok(Err(_)) => bail!("plugin sink: reponse abandonnee"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("plugin sink: timeout de requete")
            }
        }
    }
}

/// Se connecte au plugin input et relaie chaque `Command` reçue sur `cmd_tx`,
/// jusqu'à fermeture de la connexion (ne revient qu'en cas d'erreur ; à
/// spawn dans une tâche dédiée par l'appelant).
pub async fn run_input_client(socket_path: &Path, cmd_tx: mpsc::Sender<Command>) -> Result<()> {
    let stream = connect_with_retry(socket_path).await?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        match serde_json::from_str::<Command>(&line) {
            Ok(cmd) => {
                let _ = cmd_tx.send(cmd).await;
            }
            Err(e) => tracing::warn!("commande invalide recue du plugin input: {e}"),
        }
    }
    bail!("connexion au plugin input fermee")
}
```

- [ ] **Step 4: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-sdk"`
Expected: 4 tests passing (3 précédents + `source_client_correle_par_id_et_relaie_la_vue`).

- [ ] **Step 5: Mettre à jour `lib.rs`**

```rust
pub mod client;
pub mod server;

pub use client::{run_input_client, SinkClient, SourceClient};
pub use server::{
    run_input_plugin, run_sink_plugin, run_source_plugin, InputPlugin, SinkOutcome, SinkPlugin,
    SourceOutcome, SourcePlugin,
};
```

- [ ] **Step 6: `cargo clippy -p radio-pi-plugin-sdk -- -D warnings`** — corriger tout warning avant de continuer.

- [ ] **Step 7: Commit**

```bash
git add crates/radio-pi-plugin-sdk
git commit -m "feat: clients SourceClient/SinkClient/run_input_client (cote coeur)"
```

---

### Task 5: `radio-pi-core` — chargement de `plugins.toml` et supervision des processus plugins

**Files:**
- Create: `crates/radio-pi-core/src/plugins.rs`
- Modify: `crates/radio-pi-core/Cargo.toml` (ajouter les dépendances `radio-pi-proto`, `radio-pi-plugin-sdk`)
- Modify: `crates/radio-pi-core/src/main.rs` (ajouter `mod plugins;`)

**Interfaces:**
- Consumes: `radio_pi_plugin_sdk::{SourceClient, SinkClient, run_input_client}`.
- Produces: `plugins::PluginKind { Source, Sink, Input }`, `plugins::PluginConfig { name: String, kind: PluginKind, exec: String, admin_url: Option<String> }`, `plugins::PluginManifest { plugins: Vec<PluginConfig> }` avec `load(&Path) -> Result<PluginManifest>` ; `plugins::spawn(exec: &str, socket_path: &Path) -> Result<tokio::process::Child>` (spawn avec `--socket <path>` en argument).

- [ ] **Step 1: Ajouter les dépendances**

Dans `crates/radio-pi-core/Cargo.toml`, sous `[dependencies]` :

```toml
radio-pi-proto = { path = "../radio-pi-proto" }
radio-pi-plugin-sdk = { path = "../radio-pi-plugin-sdk" }
```

- [ ] **Step 2: Écrire les tests (échec attendu) — `crates/radio-pi-core/src/plugins.rs`**

```rust
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
name = "mce"
kind = "input"
exec = "/usr/local/lib/radio-pi/plugins/radio-pi-plugin-mce"
admin_url = "http://raspberrypi.local:8081"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 2);
        assert_eq!(m.plugins[0].name, "radio");
        assert_eq!(m.plugins[0].kind, PluginKind::Source);
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

- [ ] **Step 3: Vérifier l'échec**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core plugins"`
Expected: FAIL (compilation).

- [ ] **Step 4: Implémenter**

```rust
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Sink,
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
        .spawn()?)
}
```

- [ ] **Step 5: Vérifier le succès**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core plugins"`
Expected: 2 tests passing.

- [ ] **Step 6: Ajouter `mod plugins;` dans `crates/radio-pi-core/src/main.rs`** (à côté des autres `mod`, ordre alphabétique : après `mod player;` avant `mod state;`).

- [ ] **Step 7: Commit**

```bash
git add crates/radio-pi-core/Cargo.toml crates/radio-pi-core/src/plugins.rs crates/radio-pi-core/src/main.rs
git commit -m "feat: chargement de plugins.toml et spawn generique des processus plugin"
```

---

### Task 6: `radio-pi-core` — généralisation de `Core` : registre de sources, registre de sinks

**Files:**
- Modify: `crates/radio-pi-core/src/types.rs` (supprimer `Mode`, `Command`, `Event` garde `PlaybackIdle`/`PlaybackActive`/`Title`/`TrackChanged` inchangés — `Command` et `View` viennent désormais de `radio_pi_proto`)
- Modify: `crates/radio-pi-core/src/state.rs` (persistance : `active_source: String` au lieu de `mode`/`preset`)
- Rewrite: `crates/radio-pi-core/src/core.rs`
- Modify: `crates/radio-pi-core/src/player/mod.rs` (aucun changement de code, le trait `Player` reste identique)

**Interfaces:**
- Consumes: `player::Player` (inchangé), `radio_pi_proto::{Command, View, SourceAction, SourceReq}`, `radio_pi_plugin_sdk::SourceClient`.
- Produces: `core::Source` (trait interne, généralisation de l'ancien accès direct à `Mode`) — `#[async_trait] trait Source { async fn request(&self, req: SourceReq) -> anyhow::Result<SourceAction>; }`, implémenté pour `Arc<SourceClient>` (production) et pour un mock en test ; `core::Core<P: Player>` avec `new(player: P, sources: HashMap<String, Arc<dyn Source>>, persisted: PersistedState, state_path: PathBuf, view_tx: watch::Sender<View>) -> Self`, `resume`, `handle_command(Command)`, `handle_event(Event) -> Option<Duration>`, `handle_source_view(name: &str, view: View)`, `retry_stream`, `standby`.

- [ ] **Step 1: `types.rs` réduit**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Title(String),
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
    CdInserted,
    CdRemoved,
}
```

(Supprimer `Mode`, `Command`, `View`, `DiscInfo` de ce fichier — `Command`/`View` viennent maintenant de `radio_pi_proto` ; `DiscInfo` migre au plugin CD dans une tâche suivante et n'a plus sa place ici. `CdInserted`/`CdRemoved` restent car mpv/cœur n'en ont plus besoin ; supprimer ces deux variantes aussi — c'est le plugin CD, pas le cœur, qui détecte le CD dans la nouvelle architecture : `Event` se réduit à `Title`/`PlaybackActive`/`PlaybackIdle`/`TrackChanged`, tous liés au `Player` mpv partagé.)

Version finale de `types.rs` :

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Title(String),
    PlaybackActive,
    PlaybackIdle,
    TrackChanged(i64),
}
```

- [ ] **Step 2: `state.rs` — tests d'abord (échec attendu)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaut_si_fichier_absent_ou_corrompu() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.json");
        assert_eq!(load(&missing), PersistedState::default());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{pas du json").unwrap();
        assert_eq!(load(&bad), PersistedState::default());
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState { active_source: "cd".into(), volume: 35 };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn defaut_est_radio_vol60() {
        let d = PersistedState::default();
        assert_eq!(d.active_source, "radio");
        assert_eq!(d.volume, 60);
    }
}
```

- [ ] **Step 3: Vérifier l'échec** — Run: `cargo test -p radio-pi-core state` → FAIL.

- [ ] **Step 4: Implémenter `state.rs`**

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    pub active_source: String,
    pub volume: u8,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self { active_source: "radio".into(), volume: 60 }
    }
}

pub fn load(path: &Path) -> PersistedState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, state: &PersistedState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
```

- [ ] **Step 5: Vérifier le succès** — Run: `cargo test -p radio-pi-core state` → 3 tests PASS.

- [ ] **Step 6: Réécrire `core.rs` — tests d'abord (échec attendu)**

```rust
use crate::player::Player;
use crate::state::{self, PersistedState};
use crate::types::Event;
use anyhow::Result;
use radio_pi_proto::{Command, SourceAction, SourceReq, View};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait Source: Send + Sync + 'static {
    async fn request(&self, req: SourceReq) -> Result<SourceAction>;
}

pub struct Core<P: Player> {
    player: P,
    sources: HashMap<String, Arc<dyn Source>>,
    source_order: Vec<String>,
    active_source: String,
    volume: u8,
    muted: bool,
    standby: bool,
    stopped: bool,
    retry_count: u32,
    view: View,
    state_path: PathBuf,
    view_tx: watch::Sender<View>,
}

impl<P: Player> Core<P> {
    pub fn new(
        player: P,
        sources: HashMap<String, Arc<dyn Source>>,
        persisted: PersistedState,
        state_path: PathBuf,
        view_tx: watch::Sender<View>,
    ) -> Self {
        let mut source_order: Vec<String> = sources.keys().cloned().collect();
        source_order.sort();
        let active_source = if sources.contains_key(&persisted.active_source) {
            persisted.active_source.clone()
        } else {
            source_order.first().cloned().unwrap_or_default()
        };
        Self {
            player,
            sources,
            source_order,
            active_source,
            volume: persisted.volume,
            muted: false,
            standby: false,
            stopped: false,
            retry_count: 0,
            view: View::default(),
            state_path,
            view_tx,
        }
    }

    pub async fn resume(&mut self) -> Result<()> {
        self.player.set_volume(self.volume).await?;
        let action = self.active().request(SourceReq::Activate).await?;
        self.apply(action).await
    }

    /// Rejoue le contenu courant de la source active (`Activate` demande à la
    /// source de redonner l'URI en cours, pas de passer au contenu suivant).
    pub async fn retry_stream(&mut self) -> Result<()> {
        if !self.standby && !self.stopped {
            let action = self.active().request(SourceReq::Activate).await?;
            self.apply(action).await?;
        }
        Ok(())
    }

    pub fn handle_source_view(&mut self, name: &str, view: View) {
        if name == self.active_source {
            self.view = view;
            self.push_view();
        }
    }

    pub async fn handle_command(&mut self, cmd: Command) -> Result<()> {
        if self.standby && cmd != Command::Power {
            return Ok(());
        }
        match cmd {
            Command::Select(n) => {
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Select(n)).await?;
                self.apply(action).await?;
            }
            Command::Next => {
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Next).await?;
                self.apply(action).await?;
            }
            Command::Prev => {
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Prev).await?;
                self.apply(action).await?;
            }
            Command::NextTrack => {
                let action = self.active().request(SourceReq::NextTrack).await?;
                self.apply(action).await?;
            }
            Command::PrevTrack => {
                let action = self.active().request(SourceReq::PrevTrack).await?;
                self.apply(action).await?;
            }
            Command::Eject => {
                let action = self.active().request(SourceReq::Eject).await?;
                self.apply(action).await?;
            }
            Command::VolumeUp | Command::VolumeDown => {
                let v = self.volume as i16 + if cmd == Command::VolumeUp { 5 } else { -5 };
                self.volume = v.clamp(0, 100) as u8;
                self.player.set_volume(self.volume).await?;
                self.persist();
                self.push_view();
            }
            Command::Mute => {
                self.muted = !self.muted;
                self.player.set_mute(self.muted).await?;
            }
            Command::PlayPause => self.player.toggle_pause().await?,
            Command::Stop => {
                self.stopped = true;
                self.player.stop().await?;
            }
            Command::Power => {
                self.standby = !self.standby;
                if self.standby {
                    let _ = self.active().request(SourceReq::Deactivate).await;
                    self.player.stop().await?;
                } else {
                    self.resume().await?;
                }
            }
            Command::SourceCycle => {
                let _ = self.active().request(SourceReq::Deactivate).await;
                let idx = self.source_order.iter().position(|n| n == &self.active_source).unwrap_or(0);
                let next_idx = (idx + 1) % self.source_order.len().max(1);
                if let Some(next_name) = self.source_order.get(next_idx).cloned() {
                    self.active_source = next_name;
                }
                self.retry_count = 0;
                self.stopped = false;
                let action = self.active().request(SourceReq::Activate).await?;
                self.apply(action).await?;
                self.persist();
            }
        }
        Ok(())
    }

    pub async fn handle_event(&mut self, ev: Event) -> Option<Duration> {
        match ev {
            Event::Title(_) | Event::PlaybackActive => {
                self.retry_count = 0;
            }
            Event::TrackChanged(_) => {}
            Event::PlaybackIdle => {
                if !self.standby && !self.stopped {
                    let delay = (RETRY_BASE * 2u32.pow(self.retry_count)).min(RETRY_MAX);
                    self.retry_count = (self.retry_count + 1).min(4);
                    return Some(delay);
                }
            }
        }
        None
    }

    fn active(&self) -> Arc<dyn Source> {
        self.sources
            .get(&self.active_source)
            .cloned()
            .unwrap_or_else(|| panic!("source active inconnue: {}", self.active_source))
    }

    async fn apply(&mut self, action: SourceAction) -> Result<()> {
        match action {
            SourceAction::Noop => {}
            SourceAction::Play { uri } => {
                self.stopped = false;
                self.player.play(&uri).await?;
            }
            SourceAction::Stop => {
                self.stopped = true;
                self.player.stop().await?;
            }
            SourceAction::PlayerNext => self.player.next().await?,
            SourceAction::PlayerPrev => self.player.prev().await?,
        }
        Ok(())
    }

    fn persist(&self) {
        let st = PersistedState { active_source: self.active_source.clone(), volume: self.volume };
        if let Err(e) = state::save(&self.state_path, &st) {
            tracing::warn!("persistance impossible: {e}");
        }
    }

    fn push_view(&self) {
        let _ = self.view_tx.send(self.view.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct FakePlayer {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::player::Player for FakePlayer {
        async fn play(&self, uri: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("play {uri}"));
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("stop".into());
            Ok(())
        }
        async fn toggle_pause(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("pause".into());
            Ok(())
        }
        async fn next(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("next".into());
            Ok(())
        }
        async fn prev(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("prev".into());
            Ok(())
        }
        async fn set_volume(&self, v: u8) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("vol {v}"));
            Ok(())
        }
        async fn set_mute(&self, m: bool) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("mute {m}"));
            Ok(())
        }
    }

    struct FakeSource {
        name: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Source for FakeSource {
        async fn request(&self, req: SourceReq) -> Result<SourceAction> {
            self.calls.lock().unwrap().push(format!("{}:{:?}", self.name, req));
            Ok(match (self.name, req) {
                ("radio", SourceReq::Activate) => SourceAction::Play { uri: "http://fip".into() },
                ("radio", SourceReq::Select(3)) => SourceAction::Play { uri: "http://inter".into() },
                ("radio", SourceReq::Select(_)) => SourceAction::Noop,
                ("cd", SourceReq::Activate) => SourceAction::Play { uri: "cdda://".into() },
                (_, SourceReq::Eject) if self.name == "cd" => SourceAction::Stop,
                _ => SourceAction::Noop,
            })
        }
    }

    fn setup() -> (Core<FakePlayer>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>, watch::Receiver<View>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let player = FakePlayer::default();
        let player_calls = player.calls.clone();
        let source_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sources: HashMap<String, Arc<dyn Source>> = HashMap::new();
        sources.insert("radio".into(), Arc::new(FakeSource { name: "radio", calls: source_calls.clone() }));
        sources.insert("cd".into(), Arc::new(FakeSource { name: "cd", calls: source_calls.clone() }));
        let (tx, rx) = watch::channel(View::default());
        let core = Core::new(player, sources, PersistedState::default(), dir.path().join("state.json"), tx);
        (core, player_calls, source_calls, rx, dir)
    }

    #[tokio::test]
    async fn resume_active_la_source_persistee() {
        let (mut core, player_calls, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://fip".to_string()));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Activate"));
    }

    #[tokio::test]
    async fn select_relaye_a_la_source_active_sans_changer_active_source() {
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.handle_command(Command::Select(3)).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"play http://inter".to_string()));
        // Select agit sur la source deja active ; seul SourceCycle change active_source.
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "radio");
    }

    #[tokio::test]
    async fn source_cycle_bascule_et_persiste() {
        let (mut core, player_calls, source_calls, _rx, dir) = setup();
        core.handle_command(Command::SourceCycle).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "radio:Deactivate"));
        assert!(source_calls.lock().unwrap().iter().any(|c| c == "cd:Activate"));
        assert!(player_calls.lock().unwrap().contains(&"play cdda://".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.active_source, "cd");
    }

    #[tokio::test]
    async fn standby_bloque_tout_sauf_power() {
        let (mut core, player_calls, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Power).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"stop".to_string()));
        core.handle_command(Command::Select(3)).await.unwrap();
        // aucun nouvel appel "play" apres la veille tant qu'on n'a pas fait Power a nouveau
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 1);
        core.handle_command(Command::Power).await.unwrap();
        assert_eq!(player_calls.lock().unwrap().iter().filter(|c| c.starts_with("play")).count(), 2);
    }

    #[tokio::test]
    async fn stop_intentionnel_ne_declenche_pas_de_retry() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_command(Command::Stop).await.unwrap();
        assert_eq!(core.handle_event(Event::PlaybackIdle).await, None);
    }

    #[tokio::test]
    async fn backoff_croissant_puis_reinitialise_par_un_titre() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.resume().await.unwrap();
        let d1 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        let d2 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert!(d2 > d1);
        core.handle_event(Event::Title("ok".into())).await;
        let d3 = core.handle_event(Event::PlaybackIdle).await.unwrap();
        assert_eq!(d3, d1);
    }

    #[tokio::test]
    async fn vue_dune_source_inactive_est_ignoree() {
        let (mut core, _pc, _sc, mut rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_source_view("cd", View { line1: "CD".into(), line2: "".into(), line3: "".into() });
        assert!(rx.borrow().line1.is_empty()); // la vue de "cd" (inactive) n'a pas ete appliquee
        core.handle_source_view("radio", View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() });
        assert!(rx.borrow_and_update().line1.contains("RADIO"));
    }
}
```

- [ ] **Step 7: Vérifier l'échec**

Ce changement de `types.rs` casse la compilation de `config.rs`/`cd.rs`/`musicbrainz.rs`/`input.rs`/`keymap.rs`/`web.rs` (ils référencent l'ancien `Mode`/`DiscInfo`/`Command`/`View` de `types.rs`) — c'est attendu : ces fichiers sont supprimés du cœur à la Task 7 (portés dans les plugins Tasks 9-11). Pour que `cargo test -p radio-pi-core core` compile dès cette tâche, commenter provisoirement dans `crates/radio-pi-core/src/main.rs` les déclarations `mod cd; mod config; mod input; mod keymap; mod musicbrainz; mod web;` (garder `mod core; mod display; mod player; mod plugins; mod state; mod types;`) ; elles seront retirées pour de bon à la Task 7.

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core core"`
Expected: FAIL (compilation — `Source`, `Core`, `SourceAction` non définis dans `core.rs`).

- [ ] **Step 8: Implémenter** (le bloc `core.rs` ci-dessus, hors bloc de tests, va au-dessus du `#[cfg(test)]`).

- [ ] **Step 9: Vérifier le succès** — Run: `cargo test -p radio-pi-core core` → 7 tests PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/radio-pi-core/src/types.rs crates/radio-pi-core/src/state.rs crates/radio-pi-core/src/core.rs crates/radio-pi-core/Cargo.toml crates/radio-pi-core/src/main.rs
git commit -m "refactor(core): generalisation Mode -> registre de sources (Source trait, SourceAction)"
```

---

### Task 7: `radio-pi-core` — suppression des modules devenus obsolètes, page de statut

**Files:**
- Delete: `crates/radio-pi-core/src/config.rs`, `crates/radio-pi-core/src/cd.rs`, `crates/radio-pi-core/src/musicbrainz.rs`, `crates/radio-pi-core/src/input.rs`, `crates/radio-pi-core/src/keymap.rs`, `crates/radio-pi-core/src/web.rs`, `crates/radio-pi-core/src/index.html`
- Delete: `crates/radio-pi-core/tests/fixtures/mb_discid.json`
- Create: `crates/radio-pi-core/src/status.rs`
- Modify: `crates/radio-pi-core/src/main.rs` (retirer les `mod` supprimés, ajouter `mod status;`)
- Modify: `crates/radio-pi-core/Cargo.toml` (retirer `reqwest`, `evdev`, `libc` — plus utilisés dans le cœur ; garder `axum`, `tower`/`http-body-util` en dev-dependencies pour tester `status.rs`)

**Interfaces:**
- Consumes: `plugins::PluginConfig`.
- Produces: `status::StatusState { plugins: Vec<PluginStatus>, active_source: String }` où `PluginStatus { name: String, kind: String, connected: bool, admin_url: Option<String> }` ; `status::AppState { status: Arc<RwLock<StatusState>>, logs: Arc<LogBuffer> }` ; `status::router(state: AppState) -> axum::Router` avec `GET /status` (HTML, plugins + source active + dernières erreurs) et `GET /api/status` (JSON) ; `status::LogBuffer` (tampon circulaire des dernières lignes) et `status::LogBufferWriter` (adaptateur `io::Write` pour l'y brancher depuis une couche `tracing_subscriber::fmt::layer()`, filtrée `WARN` et au-dessus, câblée Task 8).

Cette tâche retire tout ce qui déménage dans les plugins (Radio/CD/MCE) des tâches suivantes, et ajoute la page de statut du cœur validée dans la spec (liste des plugins, connecté/déconnecté, source active, dernières erreurs).

- [ ] **Step 1: Supprimer les fichiers obsolètes**

```bash
git rm crates/radio-pi-core/src/config.rs crates/radio-pi-core/src/cd.rs crates/radio-pi-core/src/musicbrainz.rs crates/radio-pi-core/src/input.rs crates/radio-pi-core/src/keymap.rs crates/radio-pi-core/src/web.rs crates/radio-pi-core/src/index.html
git rm -r crates/radio-pi-core/tests
```

- [ ] **Step 2: Retirer les dépendances inutiles de `crates/radio-pi-core/Cargo.toml`**

Nouveau contenu complet :

```toml
[package]
name = "radio-pi-core"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "radio-pi-core"
path = "src/main.rs"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
axum = "0.7"
async-trait = "0.1"
radio-pi-proto = { path = "../radio-pi-proto" }
radio-pi-plugin-sdk = { path = "../radio-pi-plugin-sdk" }

[dev-dependencies]
tempfile = "3"
tower = { version = "0.4", features = ["util"] }
http-body-util = "0.1"
```

- [ ] **Step 3: Écrire les tests de `status.rs` (échec attendu)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn sample() -> StatusState {
        StatusState {
            plugins: vec![
                PluginStatus { name: "radio".into(), kind: "source".into(), connected: true, admin_url: Some("http://raspberrypi.local:8081".into()) },
                PluginStatus { name: "cd".into(), kind: "source".into(), connected: false, admin_url: None },
            ],
            active_source: "radio".into(),
        }
    }

    fn app_state() -> AppState {
        AppState { status: Arc::new(tokio::sync::RwLock::new(sample())), logs: Arc::new(LogBuffer::new(50)) }
    }

    #[tokio::test]
    async fn api_status_liste_les_plugins() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let s: StatusState = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.plugins.len(), 2);
        assert_eq!(s.active_source, "radio");
    }

    #[tokio::test]
    async fn page_statut_affiche_les_dernieres_erreurs() {
        let state = app_state();
        state.logs.push("WARN plugin cd indisponible".into());
        let app = router(state);
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("plugin cd indisponible"));
    }

    #[test]
    fn log_buffer_plafonne_a_50_lignes() {
        let buf = LogBuffer::new(50);
        for i in 0..60 {
            buf.push(format!("ligne {i}"));
        }
        let lines = buf.snapshot();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "ligne 10"); // les 10 plus anciennes ont ete evincees
        assert_eq!(lines[49], "ligne 59");
    }

    #[test]
    fn log_buffer_writer_pousse_les_lignes_completes() {
        use std::io::Write;
        let buf = Arc::new(LogBuffer::new(10));
        let mut w = LogBufferWriter(buf.clone());
        write!(w, "WARN plugin radio indisponible\n").unwrap();
        assert_eq!(buf.snapshot(), vec!["WARN plugin radio indisponible".to_string()]);
    }
}
```

- [ ] **Step 4: Vérifier l'échec** — Run: `cargo test -p radio-pi-core status` → FAIL.

- [ ] **Step 5: Implémenter `crates/radio-pi-core/src/status.rs`**

```rust
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub name: String,
    pub kind: String,
    pub connected: bool,
    pub admin_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusState {
    pub plugins: Vec<PluginStatus>,
    pub active_source: String,
}

impl<'de> serde::Deserialize<'de> for StatusState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            plugins: Vec<RawPlugin>,
            active_source: String,
        }
        #[derive(serde::Deserialize)]
        struct RawPlugin {
            name: String,
            kind: String,
            connected: bool,
            admin_url: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(StatusState {
            plugins: raw
                .plugins
                .into_iter()
                .map(|p| PluginStatus { name: p.name, kind: p.kind, connected: p.connected, admin_url: p.admin_url })
                .collect(),
            active_source: raw.active_source,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub status: Arc<RwLock<StatusState>>,
    pub logs: Arc<LogBuffer>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status_page))
        .route("/api/status", get(status_json))
        .with_state(state)
}

async fn status_json(State(state): State<AppState>) -> Json<StatusState> {
    Json(state.status.read().await.clone())
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn status_page(State(state): State<AppState>) -> Html<String> {
    let s = state.status.read().await;
    let mut rows = String::new();
    for p in &s.plugins {
        let etat = if p.connected { "connecté" } else { "indisponible" };
        let lien = p.admin_url.as_deref().unwrap_or("-");
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{etat}</td><td>{lien}</td></tr>",
            escape_html(&p.name),
            escape_html(&p.kind)
        ));
    }
    let logs: String = state
        .logs
        .snapshot()
        .iter()
        .rev()
        .map(|l| format!("<li>{}</li>", escape_html(l)))
        .collect();
    Html(format!(
        "<!doctype html><html lang=\"fr\"><meta charset=\"utf-8\"><title>radio-pi — statut</title>\
         <h1>radio-pi</h1><p>Source active : {}</p>\
         <table border=\"1\"><tr><th>Plugin</th><th>Genre</th><th>État</th><th>Admin</th></tr>{}</table>\
         <h2>Dernières erreurs</h2><ul>{}</ul></html>",
        escape_html(&s.active_source), rows, logs
    ))
}

/// Tampon circulaire des dernières lignes de log (WARN/ERROR), affiché sur
/// la page de statut. `LogBufferWriter` (ci-dessous) y pousse les lignes
/// depuis une couche `tracing` installée dans `main`.
#[derive(Debug)]
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { lines: Mutex::new(VecDeque::with_capacity(capacity)), capacity }
    }

    pub fn push(&self, line: String) {
        let mut lines = self.lines.lock().unwrap();
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }
}

/// Adaptateur `io::Write` pour brancher `LogBuffer` comme sortie d'une
/// couche `tracing_subscriber::fmt::layer()` (voir Task 8).
pub struct LogBufferWriter(pub Arc<LogBuffer>);

impl std::io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(s) = std::str::from_utf8(buf) {
            let line = s.trim_end();
            if !line.is_empty() {
                self.0.push(line.to_string());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
```

- [ ] **Step 6: Vérifier le succès** — Run: `cargo test -p radio-pi-core status` → 4 tests PASS.

- [ ] **Step 7: Mettre à jour `main.rs`** — retirer les déclarations de modules supprimés, ajouter `mod status;` :

```rust
mod core;
mod display;
mod player;
mod plugins;
mod state;
mod status;
mod types;
```

(Le contenu de `fn main()` est entièrement réécrit à la Task 8 — à cette étape, remplacer temporairement le corps de `main()` par un `todo!("cablage a la Task 8")` n'est pas nécessaire : laisser le fichier ne compiler qu'à `cargo test`, pas à `cargo build`, est acceptable puisque `cargo test` ne construit que ce que les tests référencent. Si `cargo test -p radio-pi-core` échoue à cause de `fn main()` qui référence encore les anciens modules, réduire `fn main()` à :

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    tracing::info!("radio-pi-core (cablage complet a la tache suivante)");
    Ok(())
}
```
)

- [ ] **Step 8: Vérifier l'ensemble**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core && cargo clippy -p radio-pi-core -- -D warnings"`
Expected: tous les tests (config/cd/musicbrainz/input/keymap/web supprimés, plugins + core + state + status restants) passent, 0 warning.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(core): retrait des modules migres vers les plugins, ajout de la page de statut"
```

---

### Task 8: `radio-pi-core` — câblage final de `main.rs`

**Files:**
- Rewrite: `crates/radio-pi-core/src/main.rs`

**Interfaces:**
- Consumes: tout ce qui précède (`plugins::{PluginManifest, PluginKind, spawn}`, `core::{Core, Source}`, `player::mpv::start`, `status::{AppState, StatusState, PluginStatus, router, LogBuffer, LogBufferWriter}`, `radio_pi_plugin_sdk::{SourceClient, run_input_client}`).
- Variables d'environnement : `RADIO_PI_PLUGINS` (`/etc/radio-pi/plugins.toml`), `RADIO_PI_STATE` (`/var/lib/radio-pi/state.json`), `RADIO_PI_MPV_SOCKET` (`/run/radio-pi/mpv.sock`), `RADIO_PI_MPV_BIN` (`mpv`), `RADIO_PI_TTY` (`/dev/tty1`), `RADIO_PI_CD_DEV` (`/dev/sr0` — transmis tel quel à mpv comme aujourd'hui), `RADIO_PI_HTTP` (`0.0.0.0:8080`, page de statut).

Note : l'affichage (`display::ConsoleDisplay`) et le composant mpv (`player::mpv`) restent inchangés dans leur code — ils ne sont PAS supprimés Task 7 puisqu'ils restent des responsabilités du cœur. Si la Task 7 les a par erreur retirés de `main.rs`, les rajouter ici (`mod display;` doit être présent — vérifier qu'il n'a pas été supprimé par erreur ; sinon l'ajouter).

- [ ] **Step 1: Vérifier que `mod display;` est bien présent** dans la liste de modules de `main.rs` (sinon l'ajouter — `display.rs` n'a pas bougé depuis le projet initial).

- [ ] **Step 2: Écrire `crates/radio-pi-core/src/main.rs`**

```rust
mod core;
mod display;
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
use radio_pi_plugin_sdk::{run_input_client, SourceClient};
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
    let tty = PathBuf::from(env_or("RADIO_PI_TTY", "/dev/tty1"));
    let cd_dev = env_or("RADIO_PI_CD_DEV", "/dev/sr0");
    let http_addr = env_or("RADIO_PI_HTTP", "0.0.0.0:8080");

    let manifest = PluginManifest::load(&plugins_path)
        .with_context(|| format!("chargement de {}", plugins_path.display()))?;
    let persisted = state::load(&state_path);

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
    let (ev_tx, mut ev_rx) = broadcast::channel::<Event>(64);
    let (view_tx, mut view_rx) = watch::channel(View::default());
    let (source_view_tx, mut source_view_rx) = mpsc::channel::<(String, View)>(32);

    // mpv (inchangé).
    let (mpv_player, mut mpv_child) =
        player::mpv::start(&mpv_bin, &mpv_socket, &cd_dev, ev_tx.clone())
            .await
            .context("démarrage de mpv")?;

    // Affichage console (inchangé).
    match display::ConsoleDisplay::open(&tty) {
        Ok(mut disp) => {
            tokio::spawn(async move {
                loop {
                    if view_rx.changed().await.is_err() {
                        break;
                    }
                    let v = view_rx.borrow_and_update().clone();
                    if let Err(e) = disp.show(&v) {
                        tracing::warn!("affichage: {e}");
                    }
                }
            });
        }
        Err(e) => tracing::warn!("pas d'affichage ({e}), on continue sans"),
    }

    // Spawn et connexion de chaque plugin déclaré.
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    let mut plugin_statuses = Vec::new();
    let mut children = Vec::new();

    for p in &manifest.plugins {
        let socket_path = PathBuf::from(format!("/run/radio-pi/{}.sock", p.name));
        match plugins::spawn(&p.exec, &socket_path) {
            Ok(child) => {
                children.push(child);
                match p.kind {
                    PluginKind::Source => {
                        match SourceClient::connect(&socket_path, p.name.clone(), source_view_tx.clone()).await {
                            Ok(client) => {
                                sources.insert(p.name.clone(), client);
                                plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "source".into(), connected: true, admin_url: p.admin_url.clone() });
                            }
                            Err(e) => {
                                tracing::warn!("plugin {} indisponible: {e}", p.name);
                                plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "source".into(), connected: false, admin_url: p.admin_url.clone() });
                            }
                        }
                    }
                    PluginKind::Sink => {
                        // Aucun plugin sink dans cette livraison : connexion tentée mais
                        // le registre de sinks lui-même arrive dans une spec ultérieure.
                        plugin_statuses.push(PluginStatus { name: p.name.clone(), kind: "sink".into(), connected: false, admin_url: p.admin_url.clone() });
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

    if sources.is_empty() {
        anyhow::bail!("aucune source disponible (plugins.toml vide ou tous les plugins source indisponibles)");
    }

    // Page de statut du cœur (plugins, source active, dernières erreurs).
    let status_state = Arc::new(RwLock::new(StatusState {
        plugins: plugin_statuses,
        active_source: persisted.active_source.clone(),
    }));
    {
        let app = status::router(AppState { status: status_state.clone(), logs: log_buffer.clone() });
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

- [ ] **Step 3: Vérifier la compilation**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build -p radio-pi-core"`
Expected: échoue tant que `radio-pi-plugin-radio`/`-cd`/`-mce` n'existent pas encore en tant que binaires réels (le workspace les référence dans `members` mais ils n'existent pas encore avant les tâches 9-11) — c'est attendu à ce stade : `cargo build -p radio-pi-core` seul (avec `-p`) ne nécessite PAS que les autres membres du workspace compilent, seulement `radio-pi-core` et ses dépendances de chemin (`radio-pi-proto`, `radio-pi-plugin-sdk`). Si `cargo build -p radio-pi-core` échoue pour une autre raison que l'absence des 3 futurs crates, corriger le code de `main.rs` en conséquence.

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-core && cargo clippy -p radio-pi-core -- -D warnings"`
Expected: tous les tests passent, 0 warning.

- [ ] **Step 4: Commit**

```bash
git add crates/radio-pi-core/src/main.rs
git commit -m "feat(core): cablage final (spawn/connexion des plugins, boucle principale, page de statut)"
```

---

### Task 9: `radio-pi-plugin-radio` — plugin Source Radio + page web des stations

**Files:**
- Create: `crates/radio-pi-plugin-radio/Cargo.toml`
- Create: `crates/radio-pi-plugin-radio/src/main.rs`
- Create: `crates/radio-pi-plugin-radio/src/config.rs` (contenu identique à l'ancien `crates/radio-pi-core/src/config.rs`, supprimé Task 7 — le récupérer depuis `git log` : `git show <sha-avant-suppression>:src/config.rs` ou `crates/radio-pi-core/src/config.rs`)
- Create: `crates/radio-pi-plugin-radio/src/web.rs` (contenu identique à l'ancien `web.rs`, adapté : plus de `cmd_tx`/`Command::ReloadStations`, le handler PUT recharge directement l'état partagé du plugin)
- Create: `crates/radio-pi-plugin-radio/src/index.html` (identique à l'ancien, inchangé)
- Create: `crates/radio-pi-plugin-radio/src/state.rs` (persistance du dernier preset, nouveau — mini-fichier local au plugin)

**Interfaces:**
- Produces: le binaire `radio-pi-plugin-radio`, qui lit `--socket <path>` en argument, implémente `radio_pi_plugin_sdk::SourcePlugin`, sert sa page d'admin sur son propre port (`RADIO_PI_RADIO_HTTP`, défaut `0.0.0.0:8081`).

- [ ] **Step 1: `Cargo.toml`**

```toml
[package]
name = "radio-pi-plugin-radio"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "radio-pi-plugin-radio"
path = "src/main.rs"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
axum = "0.7"
async-trait = "0.1"
radio-pi-proto = { path = "../radio-pi-proto" }
radio-pi-plugin-sdk = { path = "../radio-pi-plugin-sdk" }

[dev-dependencies]
tempfile = "3"
tower = { version = "0.4", features = ["util"] }
http-body-util = "0.1"
```

- [ ] **Step 2: Récupérer `config.rs` tel quel**

```bash
git show HEAD~0:crates/radio-pi-core/src/config.rs 2>/dev/null || true
```

(Si la commande ci-dessus échoue parce que le fichier a été supprimé dans un commit antérieur, utiliser `git log --diff-filter=D --summary | grep config.rs` pour retrouver le SHA juste avant suppression, puis `git show <SHA>^:crates/radio-pi-core/src/config.rs > crates/radio-pi-plugin-radio/src/config.rs`.) Le contenu attendu est exactement celui déjà connu (types `Station`/`Stations`, `load`/`save`/`validate`/`by_preset`/`next_preset`/`prev_preset`, 4 tests) — copier ce contenu tel quel dans `crates/radio-pi-plugin-radio/src/config.rs`, sans aucune modification.

- [ ] **Step 3: Écrire `crates/radio-pi-plugin-radio/src/state.rs` — tests d'abord (échec attendu)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaut_preset_1() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("absent.json")).preset, 1);
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &PluginState { preset: 5 }).unwrap();
        assert_eq!(load(&path).preset, 5);
    }
}
```

- [ ] **Step 4: Vérifier l'échec, puis implémenter**

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginState {
    pub preset: u8,
}

impl Default for PluginState {
    fn default() -> Self {
        Self { preset: 1 }
    }
}

pub fn load(path: &Path) -> PluginState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, state: &PluginState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
```

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-radio state"` → 2 tests PASS.

- [ ] **Step 5: `crates/radio-pi-plugin-radio/src/web.rs`**

```rust
use crate::config::Stations;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct WebState {
    pub stations_path: PathBuf,
    pub stations: Arc<RwLock<Stations>>,
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route("/api/stations", get(get_stations).put(put_stations))
        .with_state(state)
}

async fn get_stations(State(st): State<WebState>) -> Json<Stations> {
    Json(st.stations.read().await.clone())
}

async fn put_stations(State(st): State<WebState>, Json(stations): Json<Stations>) -> StatusCode {
    if stations.validate().is_err() {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }
    if stations.save(&st.stations_path).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *st.stations.write().await = stations;
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Station;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn setup() -> (Router, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stations.toml");
        let stations = Stations { stations: vec![Station { name: "FIP".into(), url: "http://fip".into(), preset: 1 }] };
        stations.save(&path).unwrap();
        let app = router(WebState { stations_path: path.clone(), stations: Arc::new(RwLock::new(stations)) });
        (app, path, dir)
    }

    #[tokio::test]
    async fn get_stations_renvoie_le_toml_en_json() {
        let (app, _p, _d) = setup();
        let resp = app.oneshot(Request::get("/api/stations").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let s: Stations = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.stations[0].name, "FIP");
    }

    #[tokio::test]
    async fn put_stations_sauvegarde_et_met_a_jour_letat_partage() {
        let (app, path, _d) = setup();
        let new = Stations { stations: vec![Station { name: "Inter".into(), url: "http://inter".into(), preset: 2 }] };
        let resp = app
            .oneshot(
                Request::put("/api/stations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&new).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);
        assert_eq!(Stations::load(&path).unwrap(), new);
    }

    #[tokio::test]
    async fn put_stations_invalide_renvoie_422() {
        let (app, path, _d) = setup();
        let bad = Stations { stations: vec![Station { name: "X".into(), url: "http://x".into(), preset: 12 }] };
        let resp = app
            .oneshot(
                Request::put("/api/stations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&bad).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(Stations::load(&path).unwrap().stations[0].name, "FIP");
    }
}
```

- [ ] **Step 6: `crates/radio-pi-plugin-radio/src/index.html`** — copier tel quel l'ancien fichier (récupéré comme `config.rs` à l'étape 2 : `git show <SHA>^:crates/radio-pi-core/src/index.html`), aucune modification.

- [ ] **Step 7: Vérifier** — Run: `cargo test -p radio-pi-plugin-radio web` → 3 tests PASS.

- [ ] **Step 8: `crates/radio-pi-plugin-radio/src/main.rs`** — implémente `SourcePlugin` + lance le serveur web + parse `--socket`.

```rust
mod config;
mod state;
mod web;

use anyhow::{Context, Result};
use config::Stations;
use radio_pi_plugin_sdk::{run_source_plugin, SourceOutcome, SourcePlugin};
use radio_pi_proto::{SourceAction, View};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct RadioSource {
    stations_path: PathBuf,
    state_path: PathBuf,
    stations: Arc<RwLock<Stations>>,
    preset: u8,
}

impl RadioSource {
    fn view_for(&self, preset: u8, status: &str) -> View {
        View {
            line1: format!("RADIO  P{preset}"),
            line2: status.to_string(),
            line3: String::new(),
        }
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

    let socket_path = socket_path_from_args();
    let stations_path = PathBuf::from(env_or("RADIO_PI_RADIO_STATIONS", "/etc/radio-pi/stations.toml"));
    let state_path = PathBuf::from(env_or("RADIO_PI_RADIO_STATE", "/var/lib/radio-pi/plugin-radio.json"));
    let http_addr = env_or("RADIO_PI_RADIO_HTTP", "0.0.0.0:8081");

    let stations = Stations::load(&stations_path).unwrap_or_else(|e| {
        tracing::warn!("stations.toml invalide ou absent ({e}) : demarrage sans stations");
        Stations::default()
    });
    let preset = state::load(&state_path).preset;
    let stations_shared = Arc::new(RwLock::new(stations));

    {
        let app = web::router(web::WebState { stations_path: stations_path.clone(), stations: stations_shared.clone() });
        let listener = tokio::net::TcpListener::bind(&http_addr).await.with_context(|| format!("bind {http_addr}"))?;
        tracing::info!("admin radio sur http://{http_addr}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("serveur web radio: {e}");
            }
        });
    }

    let source = RadioSource { stations_path, state_path, stations: stations_shared, preset };
    run_source_plugin(source, &socket_path).await
}
```

- [ ] **Step 9: Vérifier le succès complet**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-radio && cargo clippy -p radio-pi-plugin-radio -- -D warnings"`
Expected: 4 tests config + 3 tests web + 2 tests state = 9 tests passing, 0 warning.

- [ ] **Step 10: Commit**

```bash
git add crates/radio-pi-plugin-radio
git commit -m "feat: radio-pi-plugin-radio (SourcePlugin, page admin des stations)"
```

---

### Task 10: `radio-pi-plugin-cd` — plugin Source CD

**Files:**
- Create: `crates/radio-pi-plugin-cd/Cargo.toml`
- Create: `crates/radio-pi-plugin-cd/src/main.rs`
- Create: `crates/radio-pi-plugin-cd/src/cd.rs` (contenu identique à l'ancien `crates/radio-pi-core/src/cd.rs`, récupéré comme Task 9 Step 2)
- Create: `crates/radio-pi-plugin-cd/src/musicbrainz.rs` (contenu identique à l'ancien, idem)
- Create: `crates/radio-pi-plugin-cd/tests/fixtures/mb_discid.json` (contenu identique à l'ancien)
- Create: `crates/radio-pi-plugin-cd/src/disc.rs` (nouveau : `DiscInfo`, déplacé depuis l'ancien `types.rs` de `radio-pi-core`)

**Interfaces:**
- Produces: le binaire `radio-pi-plugin-cd`, implémentant `SourcePlugin`. Détection d'insertion/retrait migrée depuis `main.rs` du cœur vers ce plugin (boucle `cd::watch` propre au plugin, poussant des notifications spontanées via `poll_notification`).

- [ ] **Step 1: `Cargo.toml`**

```toml
[package]
name = "radio-pi-plugin-cd"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "radio-pi-plugin-cd"
path = "src/main.rs"

[dependencies]
anyhow = "1"
libc = "0.2"
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
async-trait = "0.1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
radio-pi-proto = { path = "../radio-pi-proto" }
radio-pi-plugin-sdk = { path = "../radio-pi-plugin-sdk" }
```

- [ ] **Step 2: Récupérer `cd.rs`, `musicbrainz.rs`, la fixture** tels quels (même méthode que Task 9 Step 2) — contenu inchangé (ioctl `CDROM_DRIVE_STATUS`, `read_toc`, `mb_toc_param`, `eject`, `parse_lookup`, `lookup`, tests inclus).

Dans `cd.rs`, un seul changement est nécessaire : `use crate::types::Event;` (import de l'ancien `radio-pi-core::types::Event`, qui n'existe plus dans ce crate) doit être retiré — la fonction `watch` ne pousse plus un `Event` du cœur mais alimente directement l'état interne du plugin (voir `main.rs` ci-dessous). Remplacer la fonction `watch` par :

```rust
/// Poll toutes les 2 s ; retourne `true`/`false` sur changement de présence du disque.
pub async fn watch(dev: PathBuf, tx: tokio::sync::mpsc::Sender<bool>) {
    let mut present = false;
    loop {
        let now = matches!(drive_status(&dev), Ok(DriveStatus::DiscOk));
        if now != present {
            present = now;
            let _ = tx.send(now).await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
```

(signature changée : `tx: mpsc::Sender<bool>` au lieu de `mpsc::Sender<crate::types::Event>` ; le reste du fichier — `drive_status`, `read_toc`, `mb_toc_param`, `eject`, les tests `toc_musicbrainz_bien_forme`/`toc_invalide_rejete` — reste identique à l'ancien `cd.rs`.)

- [ ] **Step 3: `crates/radio-pi-plugin-cd/src/disc.rs`**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscInfo {
    pub artist: String,
    pub album: String,
    pub tracks: Vec<String>,
}
```

Dans `musicbrainz.rs`, remplacer `use crate::types::DiscInfo;` par `use crate::disc::DiscInfo;` (seul changement, le reste du fichier — `parse_lookup`, `lookup`, tests — est identique).

- [ ] **Step 4: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-cd cd:: && cargo test -p radio-pi-plugin-cd musicbrainz"`
Expected: 2 tests (cd) + 3 tests (musicbrainz) = 5 tests passing (nécessite `mod cd; mod disc; mod musicbrainz;` déclarés dans un `main.rs` provisoire ou un `lib.rs` de test — ajouter dès maintenant le squelette de `main.rs` avec juste les déclarations de modules et un `fn main() {}` vide pour que `cargo test` compile, le vrai contenu de `main.rs` arrive Step 6).

- [ ] **Step 5: Commit intermédiaire**

```bash
git add crates/radio-pi-plugin-cd/Cargo.toml crates/radio-pi-plugin-cd/src/cd.rs crates/radio-pi-plugin-cd/src/musicbrainz.rs crates/radio-pi-plugin-cd/src/disc.rs crates/radio-pi-plugin-cd/tests
git commit -m "feat: portage cd.rs/musicbrainz.rs dans radio-pi-plugin-cd"
```

- [ ] **Step 6: `crates/radio-pi-plugin-cd/src/main.rs`**

```rust
mod cd;
mod disc;
mod musicbrainz;

use anyhow::Result;
use disc::DiscInfo;
use radio_pi_plugin_sdk::{run_source_plugin, SourceOutcome, SourcePlugin};
use radio_pi_proto::{SourceAction, View};
use std::path::PathBuf;
use tokio::sync::mpsc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct CdSource {
    cd_dev: String,
    present: bool,
    track: i64,
    info: Option<DiscInfo>,
    presence_rx: mpsc::Receiver<bool>,
}

impl CdSource {
    fn view(&self) -> View {
        if !self.present {
            return View { line1: "CD".into(), line2: "pas de disque".into(), line3: String::new() };
        }
        let n = self.track.max(0) as usize;
        match &self.info {
            Some(info) => View {
                line1: format!("CD  {}/{}", n + 1, info.tracks.len()),
                line2: format!("{} — {}", info.artist, info.album),
                line3: info.tracks.get(n).cloned().unwrap_or_default(),
            },
            None => View {
                line1: format!("CD  piste {}", n + 1),
                line2: "CD audio".into(),
                line3: String::new(),
            },
        }
    }

    async fn lookup_metadata(&mut self) {
        self.info = None;
        match cd::read_toc(&self.cd_dev).and_then(|raw| cd::mb_toc_param(&raw)) {
            Ok((toc, n)) => match musicbrainz::lookup(&toc, n).await {
                Ok(info) => self.info = info,
                Err(e) => tracing::info!("lookup MusicBrainz: {e}"),
            },
            Err(e) => tracing::info!("TOC illisible: {e}"),
        }
    }
}

#[async_trait::async_trait]
impl SourcePlugin for CdSource {
    async fn activate(&mut self) -> SourceOutcome {
        if self.present {
            SourceOutcome { action: SourceAction::Play { uri: "cdda://".into() }, view: Some(self.view()) }
        } else {
            SourceOutcome { action: SourceAction::Noop, view: Some(self.view()) }
        }
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Stop, view: None }
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        if !self.present || n == 0 {
            return SourceOutcome { action: SourceAction::Noop, view: None };
        }
        self.track = (n - 1) as i64;
        SourceOutcome { action: SourceAction::Play { uri: format!("cdda://{n}") }, view: Some(self.view()) }
    }
    async fn next(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn prev(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::Noop, view: None }
    }
    async fn next_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::PlayerNext, view: None }
    }
    async fn prev_track(&mut self) -> SourceOutcome {
        SourceOutcome { action: SourceAction::PlayerPrev, view: None }
    }
    async fn eject(&mut self) -> SourceOutcome {
        cd::eject(&self.cd_dev);
        SourceOutcome { action: SourceAction::Stop, view: Some(View { line1: "CD".into(), line2: "pas de disque".into(), line3: String::new() }) }
    }

    async fn poll_notification(&mut self) -> Option<View> {
        let present = self.presence_rx.recv().await?;
        self.present = present;
        self.track = 0;
        if present {
            self.lookup_metadata().await;
        } else {
            self.info = None;
        }
        Some(self.view())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = socket_path_from_args();
    let cd_dev = env_or("RADIO_PI_CD_DEV", "/dev/sr0");

    let (presence_tx, presence_rx) = mpsc::channel(8);
    tokio::spawn(cd::watch(PathBuf::from(cd_dev.clone()), presence_tx));

    let source = CdSource { cd_dev, present: false, track: 0, info: None, presence_rx };
    run_source_plugin(source, &socket_path).await
}
```

- [ ] **Step 7: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build -p radio-pi-plugin-cd && cargo test -p radio-pi-plugin-cd && cargo clippy -p radio-pi-plugin-cd -- -D warnings"`
Expected: build OK, 5 tests passing (2 cd + 3 musicbrainz), 0 warning.

- [ ] **Step 8: Commit**

```bash
git add crates/radio-pi-plugin-cd/src/main.rs
git commit -m "feat: radio-pi-plugin-cd (SourcePlugin, detection CD et MusicBrainz internes au plugin)"
```

---

### Task 11: `radio-pi-plugin-mce` — plugin Input télécommande MCE

**Files:**
- Create: `crates/radio-pi-plugin-mce/Cargo.toml`
- Create: `crates/radio-pi-plugin-mce/src/main.rs`
- Create: `crates/radio-pi-plugin-mce/src/keymap.rs` (adapté : `Command` vient de `radio_pi_proto`, variantes renommées)
- Create: `crates/radio-pi-plugin-mce/src/input.rs` (contenu identique à l'ancien `input.rs`, adapté à `radio_pi_proto::Command`)

**Interfaces:**
- Produces: le binaire `radio-pi-plugin-mce`, implémentant `radio_pi_plugin_sdk::InputPlugin`.

- [ ] **Step 1: `Cargo.toml`**

```toml
[package]
name = "radio-pi-plugin-mce"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "radio-pi-plugin-mce"
path = "src/main.rs"

[dependencies]
anyhow = "1"
evdev = { version = "0.12", features = ["tokio"] }
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
async-trait = "0.1"
radio-pi-proto = { path = "../radio-pi-proto" }
radio-pi-plugin-sdk = { path = "../radio-pi-plugin-sdk" }
```

- [ ] **Step 2: Écrire les tests de `keymap.rs` (échec attendu)** — mêmes assertions que l'ancien `keymap.rs`, adaptées aux nouveaux noms de variantes :

```rust
use radio_pi_proto::Command;

pub fn map_key(code: u16) -> Option<Command> {
    Some(match code {
        2..=10 => Command::Select((code - 1) as u8),
        513..=521 => Command::Select((code - 512) as u8),
        115 => Command::VolumeUp,
        114 => Command::VolumeDown,
        113 => Command::Mute,
        402 => Command::Next,
        403 => Command::Prev,
        164 => Command::PlayPause,
        163 => Command::NextTrack,
        165 => Command::PrevTrack,
        166 => Command::Stop,
        161 => Command::Eject,
        226 => Command::SourceCycle,
        116 | 356 => Command::Power,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chiffres_vers_select() {
        assert_eq!(map_key(2), Some(Command::Select(1)));
        assert_eq!(map_key(10), Some(Command::Select(9)));
        assert_eq!(map_key(513), Some(Command::Select(1)));
        assert_eq!(map_key(521), Some(Command::Select(9)));
    }

    #[test]
    fn touches_media_et_volume() {
        assert_eq!(map_key(115), Some(Command::VolumeUp));
        assert_eq!(map_key(114), Some(Command::VolumeDown));
        assert_eq!(map_key(113), Some(Command::Mute));
        assert_eq!(map_key(402), Some(Command::Next));
        assert_eq!(map_key(403), Some(Command::Prev));
        assert_eq!(map_key(164), Some(Command::PlayPause));
        assert_eq!(map_key(163), Some(Command::NextTrack));
        assert_eq!(map_key(165), Some(Command::PrevTrack));
        assert_eq!(map_key(166), Some(Command::Stop));
        assert_eq!(map_key(161), Some(Command::Eject));
        assert_eq!(map_key(226), Some(Command::SourceCycle));
        assert_eq!(map_key(116), Some(Command::Power));
        assert_eq!(map_key(356), Some(Command::Power));
    }

    #[test]
    fn touche_inconnue_ignoree() {
        assert_eq!(map_key(30), None);
    }
}
```

(Ce fichier contient déjà l'implémentation — il n'y a pas de RED distinct ici puisque le mapping est trivial et que la seule chose testée est la table de correspondance ; écrire le fichier complet directement, lancer les tests, ils doivent passer du premier coup. C'est acceptable : la logique est un simple `match` sans branche complexe, contrairement à la plupart des autres tâches de ce plan.)

- [ ] **Step 3: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test -p radio-pi-plugin-mce keymap"`
Expected: 3 tests passing.

- [ ] **Step 4: `crates/radio-pi-plugin-mce/src/input.rs`** (identique à l'ancien `input.rs`, seul le type `Command` change de provenance) :

```rust
use crate::keymap::map_key;
use anyhow::{Context, Result};
use evdev::{Device, EventType};
use radio_pi_proto::Command;
use tokio::sync::mpsc;

pub fn find_device(name_contains: &str) -> Result<Device> {
    let needle = name_contains.to_lowercase();
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("").to_lowercase();
        if name.contains(&needle) {
            tracing::info!("télécommande: {} ({})", dev.name().unwrap_or("?"), path.display());
            return Ok(dev);
        }
    }
    anyhow::bail!("aucun périphérique input dont le nom contient « {name_contains} »")
}

pub async fn run(device: Device, tx: mpsc::Sender<Command>) -> Result<()> {
    let mut stream = device.into_event_stream().context("event stream evdev")?;
    loop {
        let ev = stream.next_event().await?;
        if ev.event_type() == EventType::KEY && ev.value() == 1 {
            if let Some(cmd) = map_key(ev.code()) {
                tracing::debug!("touche {} -> {:?}", ev.code(), cmd);
                let _ = tx.send(cmd).await;
            }
        }
    }
}
```

- [ ] **Step 5: `crates/radio-pi-plugin-mce/src/main.rs`**

```rust
mod input;
mod keymap;

use anyhow::Result;
use radio_pi_plugin_sdk::{run_input_plugin, InputPlugin};
use radio_pi_proto::Command;
use std::path::PathBuf;
use tokio::sync::mpsc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn socket_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--socket").expect("--socket <path> requis");
    PathBuf::from(&args[idx + 1])
}

struct MceInput {
    rx: mpsc::Receiver<Command>,
}

#[async_trait::async_trait]
impl InputPlugin for MceInput {
    async fn next_command(&mut self) -> Result<Command> {
        self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("boucle evdev terminee"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = socket_path_from_args();
    let input_name = env_or("RADIO_PI_MCE_INPUT_NAME", "Media Center");

    let device = input::find_device(&input_name)?;
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        if let Err(e) = input::run(device, tx).await {
            tracing::error!("boucle evdev terminee: {e}");
        }
    });

    run_input_plugin(MceInput { rx }, &socket_path).await
}
```

- [ ] **Step 6: Vérifier**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build -p radio-pi-plugin-mce && cargo test -p radio-pi-plugin-mce && cargo clippy -p radio-pi-plugin-mce -- -D warnings"`
Expected: build OK, 3 tests passing, 0 warning.

- [ ] **Step 7: Commit**

```bash
git add crates/radio-pi-plugin-mce
git commit -m "feat: radio-pi-plugin-mce (InputPlugin, portage evdev/keymap)"
```

---

### Task 12: Build du workspace complet + `plugins.toml` d'exemple

**Files:**
- Modify: `deploy/stations.example.toml` (inchangé, déplacer sa référence dans le README si besoin)
- Create: `deploy/plugins.example.toml`
- Modify: `deploy/deploy.sh`
- Modify: `deploy/radio-pi.service` (chemin de l'exécutable renommé)
- Modify: `README.md`

**Interfaces:**
- Consumes: tous les binaires du workspace (`radio-pi-core`, `radio-pi-plugin-radio`, `radio-pi-plugin-cd`, `radio-pi-plugin-mce`).

- [ ] **Step 1: Vérifier que le workspace complet compile et que tous les tests passent**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo test --workspace"`
Expected: tous les tests de toutes les crates passent : 8 (proto) + 4 (SDK) + 16 (core : 2 plugins + 7 core + 3 state + 4 status) + 9 (radio : 4 config + 3 web + 2 state) + 5 (cd : 2 cd + 3 musicbrainz) + 3 (mce : keymap) = 45 tests, 0 régression.

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo clippy --workspace -- -D warnings"`
Expected: 0 warning.

- [ ] **Step 2: `deploy/plugins.example.toml`**

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
```

- [ ] **Step 3: `deploy/radio-pi.service`** (seul le binaire change de nom, tout le reste est inchangé) :

```ini
[Unit]
Description=radio-pi (radio internet + CD)
After=network-online.target sound.target getty@tty1.service
Wants=network-online.target
Conflicts=getty@tty1.service

[Service]
ExecStart=/usr/local/bin/radio-pi-core
Restart=always
RestartSec=2
# Accès /dev/tty1 (affichage HDMI), /dev/sr0, sockets des plugins : root pour la v1.
User=root
RuntimeDirectory=radio-pi
StateDirectory=radio-pi
Environment=RADIO_PI_STATE=/var/lib/radio-pi/state.json

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 4: `deploy/deploy.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail
PI="${PI:-pi@raspberrypi.local}"
TARGET=armv7-unknown-linux-gnueabihf
OUT="target/$TARGET/release"

cargo install cross --locked 2>/dev/null || true
cross build --release --workspace --target "$TARGET"

ssh "$PI" 'sudo mkdir -p /usr/local/lib/radio-pi/plugins /etc/radio-pi'

scp "$OUT/radio-pi-core" "$PI:/tmp/radio-pi-core"
scp "$OUT/radio-pi-plugin-radio" "$OUT/radio-pi-plugin-cd" "$OUT/radio-pi-plugin-mce" "$PI:/tmp/"
scp deploy/radio-pi.service "$PI:/tmp/"

ssh "$PI" 'sudo mv /tmp/radio-pi-core /usr/local/bin/radio-pi-core \
  && sudo mv /tmp/radio-pi-plugin-radio /tmp/radio-pi-plugin-cd /tmp/radio-pi-plugin-mce /usr/local/lib/radio-pi/plugins/ \
  && sudo chmod +x /usr/local/lib/radio-pi/plugins/* \
  && sudo mv /tmp/radio-pi.service /etc/systemd/system/ \
  && sudo systemctl daemon-reload \
  && sudo systemctl enable radio-pi \
  && sudo systemctl restart radio-pi \
  && systemctl status radio-pi --no-pager'
echo "OK — logs : ssh $PI journalctl -u radio-pi -f"
```

- [ ] **Step 5: Mettre à jour `README.md`**

Ajouter, après la section « Préparation du Pi », un paragraphe :

```markdown
## Plugins

`radio-pi-core` charge `/etc/radio-pi/plugins.toml` au démarrage (voir
`deploy/plugins.example.toml`) : chaque entrée déclare un plugin (`source`,
`sink` ou `input`), le chemin de son exécutable, et un `admin_url` optionnel
affiché sur la page de statut du cœur (`http://<pi>:8080/status`).

- `radio-pi-plugin-radio` sert sa propre page de gestion des stations sur
  `http://<pi>:8081` (`stations.toml`, comme avant).
- La mort d'un plugin est tolérée : il est marqué indisponible sur la page de
  statut, les autres continuent de fonctionner.
```

- [ ] **Step 6: Vérifier la cross-compilation du workspace complet**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cross build --release --workspace --target armv7-unknown-linux-gnueabihf"`
Expected: 4 binaires produits sous `target/armv7-unknown-linux-gnueabihf/release/` (`radio-pi-core`, `radio-pi-plugin-radio`, `radio-pi-plugin-cd`, `radio-pi-plugin-mce`).

- [ ] **Step 7: Commit**

```bash
chmod +x deploy/deploy.sh
git add deploy README.md
git commit -m "feat(deploy): workspace multi-binaires, plugins.toml d'exemple, deploy.sh etendu"
```

---

### Task 13: Validation manuelle en développement (WSL)

**Files:** aucun fichier modifié — vérification de bout en bout avant de considérer l'architecture prête.

- [ ] **Step 1: Préparer un environnement de test local**

```bash
mkdir -p /tmp/rp/plugins
cat > /tmp/rp/stations.toml <<'EOF'
[[stations]]
name = "FIP"
url = "http://icecast.radiofrance.fr/fip-midfi.mp3"
preset = 1
EOF
cat > /tmp/rp/plugins.toml <<'EOF'
[[plugin]]
name = "radio"
kind = "source"
exec = "/mnt/c/projets/perso/radio-pi/target/debug/radio-pi-plugin-radio"
EOF
```

- [ ] **Step 2: Compiler en debug**

Run: `wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo build --workspace"`
Expected: build OK.

- [ ] **Step 3: Lancer le cœur avec un seul plugin (radio) branché**

```bash
timeout 20 env \
  RADIO_PI_PLUGINS=/tmp/rp/plugins.toml \
  RADIO_PI_STATE=/tmp/rp/state.json \
  RADIO_PI_MPV_SOCKET=/tmp/rp/mpv.sock \
  RADIO_PI_TTY=/dev/stdout \
  RADIO_PI_HTTP=127.0.0.1:8080 \
  RADIO_PI_RADIO_STATIONS=/tmp/rp/stations.toml \
  RADIO_PI_RADIO_STATE=/tmp/rp/plugin-radio.json \
  RADIO_PI_RADIO_HTTP=127.0.0.1:8081 \
  wsl -e bash -lc "source ~/.cargo/env && cd /mnt/c/projets/perso/radio-pi && cargo run -p radio-pi-core"
```

Expected (observer les logs et la console) :
- mpv démarre, se connecte ;
- le plugin radio est spawné et connecté (`sources: HashMap` en contient un) ;
- la console affiche `RADIO  P1` / `FIP` (ou « connexion… » si le flux n'est pas joignable depuis WSL) ;
- `curl http://127.0.0.1:8080/api/status` renvoie un JSON avec `"name":"radio"`, `"connected":true` ;
- `curl http://127.0.0.1:8081/api/stations` renvoie la station FIP.

- [ ] **Step 4: Vérifier la tolérance à la panne d'un plugin**

Modifier `/tmp/rp/plugins.toml` pour pointer `exec` vers un chemin inexistant, relancer la même commande avec un timeout court, et vérifier dans les logs que le cœur log un avertissement (« lancement du plugin radio impossible ») **sans** planter — `curl http://127.0.0.1:8080/api/status` doit répondre avec `"connected":false`.

- [ ] **Step 5: Documenter les limites connues restant à valider sur le vrai Pi (ne pas coder ici)**

Ajouter au ledger de suivi (`.superpowers/sdd/progress.md` ou équivalent pour ce plan) : la checklist matérielle existante (télécommande MCE, `cd-discid`, son) doit être redéroulée après cette refonte puisque les processus ont changé (spawn de 3 enfants au lieu d'un seul mpv), même si le comportement observable ne change pas.

---

## Auto-relecture (à faire par le contrôleur avant dispatch, pas une tâche à exécuter)

- **Couverture de la spec** : mécanisme IPC (Tasks 2-4), tolérance aux pannes de plugin (Task 8/13), Radio/CD/MCE portés (Tasks 9-11), page de statut du cœur (Task 7-8), page admin déplacée dans le plugin radio (Task 9), saut direct à la piste CD via `Select` (Task 10), déploiement multi-binaires (Task 12) — tout couvert.
- **Hors périmètre confirmé** : aucun plugin Sink réel n'est livré (le registre existe côté protocole/SDK mais `plugins.toml` d'exemple n'en déclare aucun) — conforme à la spec.
- **Cohérence des types** : `Command`/`View`/`SourceReq`/`SourceAction`/`SourceMessage` définis Task 2, consommés à l'identique Tasks 6, 8, 9, 10, 11 — noms vérifiés cohérents partout (`SourceAction::Play{uri}`, `SourceReq::Select(u8)`, etc.).
