# Rendez-vous des greffons — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Le cœur lie un socket d'enregistrement avant tout lancement ; chaque greffon lie ses sockets de genre puis s'annonce, ce qui supprime les deux attentes devinées, autorise plusieurs genres par greffon, et corrige le singleton des afficheurs.

**Architecture:** Inversion du seul pas de la découverte. Le cœur reste client sur les sockets de genre — les cinq protocoles de fil et les cinq traits du SDK ne changent pas d'une ligne. Tout le neuf est concentré dans un chemin d'enregistrement : `Announcement` dans `ritornello-proto`, un constructeur `Runtime` dans le SDK, une boucle `gather` dans le cœur.

**Tech Stack:** Rust 2021, tokio (sockets Unix, `select!`), serde/serde_json (JSON par lignes), `futures::stream::FuturesUnordered`, `anyhow`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-08-24-rendez-vous-greffons-design.md`

## Global Constraints

- **Tests via WSL uniquement** — cargo n'existe pas dans Git Bash. Toute commande de test :
  `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test <args>"`
- **`-D warnings`** est en vigueur sur ce dépôt : tout code mort, import inutilisé ou variable non lue casse la compilation. Supprimer les imports en même temps que les fonctions.
- **Journaux en anglais**, commentaires et documentation en français. Convention établie du dépôt (commit efeda48).
- **Bascule sèche, aucun repli.** Un greffon qui ne s'annonce pas n'est pas câblé. Ne jamais réintroduire une lecture du `kind` de `plugins.toml`.
- **Les cinq protocoles de fil ne changent pas.** Les tests de protocole existants dans `crates/ritornello-plugin-sdk/src/server.rs` et `client.rs` doivent passer **sans être modifiés** — leur non-modification est la preuve. Si un de ces tests demande à être touché, s'arrêter et le signaler : c'est le signe qu'un protocole a bougé.
- **Nom autoritaire côté fichier.** Le cœur passe `--name`, le greffon le renvoie. Un greffon n'invente jamais son nom.
- Chemins des sockets : `{runtime_dir}/sockets/`, avec `runtime_dir` = `RITORNELLO_RUNTIME_DIR`, défaut `/run/ritornello`.
- Nommage : `register.sock`, `{name}-{genre}.sock`, `{name}-admin.sock`.

---

### Task 1 : `PluginKind` et `Announcement` dans `ritornello-proto`

`PluginKind` doit quitter le cœur : le SDK ne peut pas dépendre de `ritornello-core`, et c'est lui qui sérialise l'annonce.

**Files:**
- Create: `crates/ritornello-proto/src/register.rs`
- Modify: `crates/ritornello-proto/src/lib.rs`
- Modify: `crates/ritornello-core/src/plugins.rs` (retirer `PluginKind`, ré-exporter depuis proto)

**Interfaces:**
- Consomme : rien.
- Produit : `ritornello_proto::{PluginKind, Announcement}`. `PluginKind` a les variantes `Source`, `Display`, `Input`, `Metadata`, sérialisées en minuscules. `Announcement { name: String, kinds: Vec<PluginKind>, admin: bool }`.

- [ ] **Step 1: Écrire les tests qui échouent**

Créer `crates/ritornello-proto/src/register.rs` avec, pour l'instant, seulement le module de tests et les `use` :

```rust
//! Ligne d'annonce d'un greffon, écrite sur le socket d'enregistrement du
//! cœur juste après que le greffon a lié ses propres sockets.
//!
//! L'ordre compte et il est structurel : les sockets sont liés par le
//! constructeur du SDK, l'annonce n'est écrite que par `Runtime::run`. Quand
//! le cœur lit cette ligne, il sait donc à la fois quels genres existent et
//! que les sockets correspondants acceptent déjà une connexion.

use serde::{Deserialize, Serialize};

/// Ce qu'un greffon sait faire. Le genre est une propriété du **binaire**,
/// annoncée par lui, et non une ligne de configuration que l'opérateur
/// devait connaître (voir le même arbitrage rendu pour la page d'admin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Source,
    Display,
    Input,
    /// Enrichit ce que joue la Source active sans que celle-ci le sache.
    ///
    /// **L'ordre compte** entre deux plugins `metadata` qui répondent pour le
    /// même morceau : le premier de `plugins.toml` gagne. Cet ordre vient
    /// désormais du manifeste seul, l'annonce ne le porte pas — voir
    /// `ritornello-core::register`.
    Metadata,
}

/// Une annonce, une ligne de JSON, un greffon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Repris tel quel de `--name`. Sert à corréler N annonces arrivant sur
    /// un socket unique ; l'autorité sur le nom reste au manifeste.
    pub name: String,
    pub kinds: Vec<PluginKind>,
    /// `false` par défaut : un greffon sans page d'admin peut omettre le champ.
    #[serde(default)]
    pub admin: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_genres_se_serialisent_en_minuscules() {
        let a = Announcement {
            name: "mpd".into(),
            kinds: vec![PluginKind::Input, PluginKind::Display],
            admin: true,
        };
        let ligne = serde_json::to_string(&a).unwrap();
        assert_eq!(ligne, r#"{"name":"mpd","kinds":["input","display"],"admin":true}"#);
        assert_eq!(serde_json::from_str::<Announcement>(&ligne).unwrap(), a);
    }

    #[test]
    fn admin_absent_vaut_faux() {
        // Un greffon sans page peut omettre le champ : l'annonce la plus
        // courante doit rester la plus courte à écrire.
        let a: Announcement =
            serde_json::from_str(r#"{"name":"cd","kinds":["source"]}"#).unwrap();
        assert!(!a.admin);
        assert_eq!(a.kinds, vec![PluginKind::Source]);
    }

    #[test]
    fn un_genre_inconnu_est_une_erreur_pas_un_silence() {
        // Une faute de frappe dans un binaire de greffon doit être rapportée,
        // pas absorbée en genre par défaut.
        assert!(serde_json::from_str::<Announcement>(r#"{"name":"x","kinds":["sourec"]}"#).is_err());
    }

    #[test]
    fn plusieurs_genres_survivent_a_l_aller_retour() {
        let a = Announcement {
            name: "double".into(),
            kinds: vec![PluginKind::Source, PluginKind::Metadata],
            admin: false,
        };
        let retour: Announcement =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(retour, a);
    }
}
```

- [ ] **Step 2: Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-proto register"`
Attendu : ÉCHEC de compilation — `register` n'est pas déclaré dans `lib.rs`.

- [ ] **Step 3: Déclarer et exporter le module**

Dans `crates/ritornello-proto/src/lib.rs`, ajouter à côté des modules existants :

```rust
pub mod register;
pub use register::{Announcement, PluginKind};
```

- [ ] **Step 4: Lancer les tests pour vérifier qu'ils passent**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-proto"`
Attendu : les 4 tests de `register` passent, aucun test existant ne casse.

- [ ] **Step 5: Retirer `PluginKind` du cœur au profit d'une ré-exportation**

Dans `crates/ritornello-core/src/plugins.rs`, **supprimer** l'`enum PluginKind` et son `derive`/`serde`, et le remplacer par :

```rust
pub use ritornello_proto::PluginKind;
```

Le reste du fichier n'est pas touché à cette étape (`PluginConfig.kind` part en Task 5).

- [ ] **Step 6: Vérifier que l'espace de travail compile**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-core plugins"`
Attendu : PASS. Les tests de `plugins.rs` qui nomment `PluginKind::Source` etc. compilent contre le type déménagé.

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-proto/src/register.rs crates/ritornello-proto/src/lib.rs crates/ritornello-core/src/plugins.rs
git commit -m "feat(proto): ligne d annonce des greffons, PluginKind demenage depuis le coeur"
```

---

### Task 2 : arguments de ligne de commande du SDK

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/args.rs`
- Modify: `crates/ritornello-plugin-sdk/src/lib.rs`

**Interfaces:**
- Consomme : rien.
- Produit : `ritornello_plugin_sdk::args::{register_socket() -> PathBuf, plugin_name() -> String, socket_prefix() -> PathBuf}`. `arg_value(args, flag) -> Option<PathBuf>` est conservé tel quel. `socket_path()` et `admin_socket_path()` **disparaissent**.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans le `mod tests` de `crates/ritornello-plugin-sdk/src/args.rs`, ajouter :

```rust
    #[test]
    fn extrait_les_trois_options_du_nouveau_montage() {
        let a = args(&[
            "plugin",
            "--register", "/run/ritornello/sockets/register.sock",
            "--name", "radio",
            "--socket-prefix", "/run/ritornello/sockets/radio",
        ]);
        assert_eq!(
            arg_value(&a, "--register"),
            Some(PathBuf::from("/run/ritornello/sockets/register.sock"))
        );
        assert_eq!(arg_value(&a, "--name"), Some(PathBuf::from("radio")));
        assert_eq!(
            arg_value(&a, "--socket-prefix"),
            Some(PathBuf::from("/run/ritornello/sockets/radio"))
        );
    }

    #[test]
    fn suffixe_un_prefixe_par_genre_et_par_admin() {
        let p = PathBuf::from("/run/ritornello/sockets/radio");
        assert_eq!(
            super::genre_socket(&p, ritornello_proto::PluginKind::Source),
            PathBuf::from("/run/ritornello/sockets/radio-source.sock")
        );
        assert_eq!(
            super::admin_socket(&p),
            PathBuf::from("/run/ritornello/sockets/radio-admin.sock")
        );
    }
```

- [ ] **Step 2: Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-plugin-sdk args"`
Attendu : ÉCHEC — `genre_socket` et `admin_socket` n'existent pas.

- [ ] **Step 3: Implémenter**

Dans `crates/ritornello-plugin-sdk/src/args.rs`, **supprimer** `socket_path()` et `admin_socket_path()`, et ajouter :

```rust
use ritornello_proto::PluginKind;

/// Chemin du socket d'enregistrement du cœur (`--register`), obligatoire.
pub fn register_socket() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--register").expect("--register <path> required")
}

/// Nom sous lequel le cœur connaît ce greffon (`--name`), obligatoire.
///
/// Le greffon le **renvoie** dans son annonce sans jamais l'inventer : c'est
/// le manifeste qui a autorité, sinon deux binaires pourraient réclamer le
/// même nom et collisionner sur les chemins de sockets.
pub fn plugin_name() -> String {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--name")
        .expect("--name <name> required")
        .to_string_lossy()
        .into_owned()
}

/// Préfixe des sockets que ce greffon doit lier (`--socket-prefix`).
///
/// Le cœur garde la maîtrise du répertoire et du préfixe ; le greffon n'a
/// autorité que sur les suffixes, qui sont exactement ce qu'il annonce.
pub fn socket_prefix() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    arg_value(&args, "--socket-prefix").expect("--socket-prefix <path> required")
}

/// `{prefixe}-{genre}.sock`.
pub fn genre_socket(prefix: &std::path::Path, kind: PluginKind) -> PathBuf {
    let genre = match kind {
        PluginKind::Source => "source",
        PluginKind::Display => "display",
        PluginKind::Input => "input",
        PluginKind::Metadata => "metadata",
    };
    PathBuf::from(format!("{}-{genre}.sock", prefix.display()))
}

/// `{prefixe}-admin.sock`.
pub fn admin_socket(prefix: &std::path::Path) -> PathBuf {
    PathBuf::from(format!("{}-admin.sock", prefix.display()))
}
```

Dans `crates/ritornello-plugin-sdk/src/lib.rs`, remplacer la ligne
`pub use args::{admin_socket_path, socket_path};` par :

```rust
pub use args::{admin_socket, genre_socket, plugin_name, register_socket, socket_prefix};
```

Ajouter `ritornello-proto` aux dépendances si absent — il est déjà présent dans `Cargo.toml` du SDK.

- [ ] **Step 4: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-plugin-sdk args"`
Attendu : PASS, y compris les deux tests historiques de `arg_value`.

Note : l'espace de travail ne compile plus à ce stade — les huit greffons appellent encore `socket_path()`. C'est attendu, Task 9 les migre. Ne pas tenter de compiler l'espace complet ici.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-plugin-sdk/src/args.rs crates/ritornello-plugin-sdk/src/lib.rs
git commit -m "feat(plugin-sdk): arguments register/name/socket-prefix, suffixes par genre"
```

---

### Task 3 : scinder `run_*_plugin` en `bind_*` + `serve_*`

Le constructeur de Task 4 doit lier tous les sockets **avant** d'annoncer. Il faut donc séparer la liaison du service. Les enveloppes `run_*_plugin` sont conservées pour que les tests de protocole existants passent sans modification.

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/server.rs`

**Interfaces:**
- Consomme : les cinq traits, inchangés.
- Produit : pour chaque genre `X` dans {source, display, input, metadata, admin} :
  - `pub fn bind_X(path: &Path) -> Result<UnixListener>`
  - `pub async fn serve_X(listener: UnixListener, plugin: impl XPlugin) -> Result<()>`
  - `pub async fn run_X_plugin(plugin: impl XPlugin, path: &Path) -> Result<()>` — enveloppe `bind_X` + `serve_X`, signature **identique à l'actuelle**.

- [ ] **Step 1: Écrire le test qui échoue**

Dans le `mod tests` de `crates/ritornello-plugin-sdk/src/server.rs`, ajouter :

```rust
    #[tokio::test]
    async fn bind_puis_serve_equivaut_a_run() {
        // La scission ne doit rien changer au comportement observable : un
        // socket lié par `bind_display` accepte une connexion AVANT que
        // `serve_display` ne tourne (c'est le backlog du noyau, et c'est ce
        // qui rend l'annonce du Runtime fiable).
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let listener = bind_display(&socket).unwrap();

        // Personne ne sert encore : la connexion doit néanmoins aboutir.
        let stream = UnixStream::connect(&socket).await.expect("le backlog accepte avant accept()");

        let recus = Arc::new(Mutex::new(Vec::new()));
        let recus_plugin = recus.clone();
        tokio::spawn(async move {
            serve_display(listener, EnMemoire { recus: recus_plugin }).await.unwrap();
        });

        let (_r, mut w) = stream.into_split();
        let etat = PlayerState::default();
        w.write_all(format!("{}\n", serde_json::to_string(&etat).unwrap()).as_bytes())
            .await
            .unwrap();

        for _ in 0..100 {
            if recus.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("l'etat n'a pas atteint le plugin");
    }
```

Ajouter, si un afficheur de test n'existe pas déjà dans ce module, le bouchon correspondant :

```rust
    struct EnMemoire {
        recus: Arc<Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl DisplayPlugin for EnMemoire {
        async fn show(&mut self, state: PlayerState) -> Result<()> {
            self.recus.lock().unwrap().push(state);
            Ok(())
        }
    }
```

Réutiliser le bouchon déjà présent s'il y en a un — le test `recoit_letat_du_lecteur_en_ligne` en a probablement un ; dans ce cas, ne pas en créer un second.

- [ ] **Step 2: Lancer le test pour vérifier qu'il échoue**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-plugin-sdk bind_puis_serve"`
Attendu : ÉCHEC — `bind_display` et `serve_display` n'existent pas.

- [ ] **Step 3: Scinder les cinq fonctions**

Motif à appliquer, montré ici sur l'afficheur. Le corps de la boucle est **copié tel quel** depuis l'actuel `run_display_plugin` ; seules la liaison et l'`accept` changent de place.

```rust
/// Lie le socket d'un afficheur, sans servir encore.
///
/// Séparé de `serve_display` pour que le `Runtime` puisse lier **tous** ses
/// sockets avant de s'annoncer : c'est cet ordre qui fait de l'annonce une
/// barrière de disponibilité.
pub fn bind_display(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket_path);
    UnixListener::bind(socket_path).with_context(|| format!("binding {}", socket_path.display()))
}

/// Accepte la connexion du cœur, puis affiche chaque état reçu jusqu'à
/// fermeture. Protocole à sens unique : aucune réponse n'est attendue.
///
/// Chaque ligne est un `PlayerState` complet, pas une vue déjà composée : la
/// mise en page appartient au plugin (voir `ritornello-plugin-console::display`).
pub async fn serve_display(listener: UnixListener, mut plugin: impl DisplayPlugin) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let (read, _write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let state: PlayerState = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("invalid player state ignored: {e}");
                continue;
            }
        };
        plugin.show(state).await?;
    }
    Ok(())
}

/// Enveloppe historique : lie puis sert. Conservée pour les appels directs et
/// pour les tests de protocole, qui ne doivent pas bouger.
pub async fn run_display_plugin(plugin: impl DisplayPlugin, socket_path: &Path) -> Result<()> {
    serve_display(bind_display(socket_path)?, plugin).await
}
```

Appliquer le même découpage à :
- `run_source_plugin` → `bind_source` + `serve_source`
- `run_input_plugin` → `bind_input` + `serve_input`
- `run_metadata_plugin` → `bind_metadata` + `serve_metadata`
- `run_admin_plugin` → `bind_admin` + `serve_admin`

Règles : le corps de boucle de chaque `serve_*` est le corps actuel **sans modification**, de l'`accept()` inclus jusqu'au `Ok(())`. Chaque `bind_*` contient exactement le `create_dir_all` + `remove_file` + `UnixListener::bind` actuel, avec le `with_context` là où il existe déjà (`run_source_plugin` et `run_display_plugin` en ont un ; `run_input_plugin` non — l'ajouter, l'absence de chemin dans l'erreur est un défaut, pas une intention).

- [ ] **Step 4: Exporter les nouvelles fonctions**

Dans `crates/ritornello-plugin-sdk/src/lib.rs`, étendre le `pub use server::{...}` avec :

```rust
    bind_admin, bind_display, bind_input, bind_metadata, bind_source, serve_admin, serve_display,
    serve_input, serve_metadata, serve_source,
```

en conservant tous les noms déjà exportés.

- [ ] **Step 5: Lancer la totalité des tests du SDK**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-plugin-sdk"`
Attendu : PASS. **Aucun test préexistant ne doit avoir été modifié.** Vérifier avec `git diff --stat` que les seules lignes touchées dans `server.rs` sont hors du `mod tests`, à l'exception du test ajouté à l'étape 1.

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-plugin-sdk/src/server.rs crates/ritornello-plugin-sdk/src/lib.rs
git commit -m "refactor(plugin-sdk): scinder liaison et service des cinq genres"
```

---

### Task 4 : le constructeur `Runtime`

**Files:**
- Create: `crates/ritornello-plugin-sdk/src/runtime.rs`
- Modify: `crates/ritornello-plugin-sdk/src/lib.rs`

**Interfaces:**
- Consomme : `bind_*`/`serve_*` de Task 3, `args::*` de Task 2, `ritornello_proto::{Announcement, PluginKind}` de Task 1.
- Produit : `ritornello_plugin_sdk::Runtime` avec `from_args() -> Result<Runtime>`, les méthodes consommantes `source`, `display`, `input`, `metadata`, `admin` (chacune `self -> Result<Self>`), et `run(self) -> Result<()>`.

Les méthodes rendent `Result` parce qu'elles **lient** un socket : un échec de liaison doit remonter à l'appelant, pas paniquer.

- [ ] **Step 1: Écrire les tests qui échouent**

Créer `crates/ritornello-plugin-sdk/src/runtime.rs` avec le module de tests seul :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::{Announcement, PlayerState, PluginKind};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    struct AfficheurBouchon {
        recus: Arc<Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl crate::DisplayPlugin for AfficheurBouchon {
        async fn show(&mut self, state: PlayerState) -> anyhow::Result<()> {
            self.recus.lock().unwrap().push(state);
            Ok(())
        }
    }

    struct EntreeBouchon {
        rx: tokio::sync::mpsc::Receiver<ritornello_proto::InputMessage>,
    }

    #[async_trait::async_trait]
    impl crate::InputPlugin for EntreeBouchon {
        async fn next_command(&mut self) -> anyhow::Result<ritornello_proto::InputMessage> {
            self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("canal ferme"))
        }
    }

    /// Lit l'unique annonce déposée sur un socket d'enregistrement.
    async fn lire_annonce(listener: &UnixListener) -> Announcement {
        let (stream, _) = listener.accept().await.unwrap();
        let mut lignes = BufReader::new(stream).lines();
        let ligne = lignes.next_line().await.unwrap().expect("une annonce");
        serde_json::from_str(&ligne).unwrap()
    }

    #[tokio::test]
    async fn lannonce_decrit_exactement_les_genres_enregistres() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefixe = dir.path().join("mpd");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let recus = Arc::new(Mutex::new(Vec::new()));
        let rt = Runtime::new("mpd".into(), register.clone(), prefixe.clone())
            .display(AfficheurBouchon { recus })
            .unwrap()
            .input(EntreeBouchon { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = lire_annonce(&listener).await;
        assert_eq!(a.name, "mpd");
        assert_eq!(a.kinds, vec![PluginKind::Display, PluginKind::Input]);
        assert!(!a.admin, "aucun .admin() appele");
    }

    #[tokio::test]
    async fn les_sockets_sont_lies_avant_que_lannonce_soit_lisible() {
        // C'est l'invariant central du chantier : quand le coeur lit
        // l'annonce, il peut se connecter sans retenter.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefixe = dir.path().join("mpd");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let recus = Arc::new(Mutex::new(Vec::new()));
        let rt = Runtime::new("mpd".into(), register.clone(), prefixe.clone())
            .display(AfficheurBouchon { recus })
            .unwrap()
            .input(EntreeBouchon { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = lire_annonce(&listener).await;
        // Un connect NU, sans boucle de reprise : il doit aboutir du premier coup.
        for genre in a.kinds {
            let chemin = crate::genre_socket(&prefixe, genre);
            UnixStream::connect(&chemin)
                .await
                .unwrap_or_else(|e| panic!("{} refuse la connexion: {e}", chemin.display()));
        }
    }

    #[tokio::test]
    async fn deux_genres_sont_servis_par_le_meme_processus() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefixe = dir.path().join("mpd");

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let recus = Arc::new(Mutex::new(Vec::new()));
        let recus_test = recus.clone();
        let rt = Runtime::new("mpd".into(), register.clone(), prefixe.clone())
            .display(AfficheurBouchon { recus })
            .unwrap()
            .input(EntreeBouchon { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });
        let _ = lire_annonce(&listener).await;

        // Cote afficheur : le coeur pousse un etat.
        let display = UnixStream::connect(crate::genre_socket(&prefixe, PluginKind::Display))
            .await
            .unwrap();
        let (_r, mut w) = display.into_split();
        w.write_all(format!("{}\n", serde_json::to_string(&PlayerState::default()).unwrap()).as_bytes())
            .await
            .unwrap();

        // Cote entree : le greffon pousse une commande.
        let input = UnixStream::connect(crate::genre_socket(&prefixe, PluginKind::Input))
            .await
            .unwrap();
        tx.send(ritornello_proto::Command::Next.into()).await.unwrap();
        let mut lignes = BufReader::new(input).lines();
        let ligne = lignes.next_line().await.unwrap().expect("une commande");
        assert!(ligne.contains("Next"), "commande inattendue: {ligne}");

        for _ in 0..100 {
            if recus_test.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("l'etat n'a pas atteint l'afficheur alors que l'entree fonctionnait");
    }
}
```

- [ ] **Step 2: Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-plugin-sdk runtime"`
Attendu : ÉCHEC de compilation — `Runtime` n'existe pas.

- [ ] **Step 3: Implémenter le `Runtime`**

En tête de `crates/ritornello-plugin-sdk/src/runtime.rs` :

```rust
//! Constructeur d'un greffon : on enregistre une moitié par genre, chacune
//! liant son socket immédiatement, puis `run()` annonce et sert.
//!
//! L'ordre « lier d'abord, annoncer ensuite » n'est pas une consigne mais une
//! propriété de ce type : les méthodes lient, seul `run()` écrit l'annonce. Un
//! greffon ne peut donc pas annoncer un genre dont le socket n'est pas prêt.

use crate::server::{
    bind_admin, bind_display, bind_input, bind_metadata, bind_source, serve_admin, serve_display,
    serve_input, serve_metadata, serve_source, AdminPlugin, DisplayPlugin, InputPlugin,
    MetadataPlugin, SourcePlugin,
};
use anyhow::{Context, Result};
// `StreamExt` pour le `.next()` du `FuturesUnordered` de `run()`.
use futures::StreamExt;
use ritornello_proto::{Announcement, PluginKind};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};

/// Une moitié prête à servir : son genre, pour l'annonce, et sa boucle.
struct Moitie {
    kind: PluginKind,
    servir: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
}

pub struct Runtime {
    name: String,
    register: PathBuf,
    prefix: PathBuf,
    moities: Vec<Moitie>,
    /// La boucle de la page d'admin, si `.admin()` a été appelé. Hors de
    /// `moities` : `admin` n'est pas un `PluginKind`, c'est un drapeau de
    /// l'annonce.
    admin: Option<Pin<Box<dyn Future<Output = Result<()>> + Send>>>,
}

impl Runtime {
    /// Monte un `Runtime` depuis les arguments passés par le cœur.
    pub fn from_args() -> Result<Self> {
        Ok(Self::new(
            crate::plugin_name(),
            crate::register_socket(),
            crate::socket_prefix(),
        ))
    }

    /// Utile aux tests, qui ne passent pas par `std::env::args`.
    pub fn new(name: String, register: PathBuf, prefix: PathBuf) -> Self {
        Self { name, register, prefix, moities: Vec::new(), admin: None }
    }

    pub fn source(mut self, plugin: impl SourcePlugin) -> Result<Self> {
        let l = bind_source(&crate::genre_socket(&self.prefix, PluginKind::Source))?;
        self.moities.push(Moitie {
            kind: PluginKind::Source,
            servir: Box::pin(serve_source(l, plugin)),
        });
        Ok(self)
    }

    pub fn display(mut self, plugin: impl DisplayPlugin) -> Result<Self> {
        let l = bind_display(&crate::genre_socket(&self.prefix, PluginKind::Display))?;
        self.moities.push(Moitie {
            kind: PluginKind::Display,
            servir: Box::pin(serve_display(l, plugin)),
        });
        Ok(self)
    }

    pub fn input(mut self, plugin: impl InputPlugin) -> Result<Self> {
        let l = bind_input(&crate::genre_socket(&self.prefix, PluginKind::Input))?;
        self.moities.push(Moitie {
            kind: PluginKind::Input,
            servir: Box::pin(serve_input(l, plugin)),
        });
        Ok(self)
    }

    pub fn metadata(mut self, plugin: impl MetadataPlugin) -> Result<Self> {
        let l = bind_metadata(&crate::genre_socket(&self.prefix, PluginKind::Metadata))?;
        self.moities.push(Moitie {
            kind: PluginKind::Metadata,
            servir: Box::pin(serve_metadata(l, plugin)),
        });
        Ok(self)
    }

    pub fn admin(mut self, plugin: impl AdminPlugin) -> Result<Self> {
        let l = bind_admin(&crate::admin_socket(&self.prefix))?;
        self.admin = Some(Box::pin(serve_admin(l, plugin)));
        Ok(self)
    }

    /// Annonce, puis sert toutes les moitiés jusqu'à ce que l'une s'arrête.
    ///
    /// Chaque moitié tourne dans sa propre tâche : la panne de la page
    /// d'admin ne doit pas couper l'audio, et réciproquement — c'est
    /// exactement ce que les greffons `radio`, `files` et `generic-input`
    /// faisaient à la main avant ce constructeur.
    pub async fn run(self) -> Result<()> {
        let annonce = Announcement {
            name: self.name.clone(),
            kinds: self.moities.iter().map(|m| m.kind).collect(),
            admin: self.admin.is_some(),
        };
        let mut flux = UnixStream::connect(&self.register)
            .await
            .with_context(|| format!("connecting to {}", self.register.display()))?;
        flux.write_all(format!("{}\n", serde_json::to_string(&annonce)?).as_bytes()).await?;
        flux.shutdown().await?;
        drop(flux);
        tracing::info!("announced as {} ({:?})", annonce.name, annonce.kinds);

        // Chaque moitié est suivie **indépendamment jusqu'au bout**. Surtout
        // pas de `select_all` ni de `try_join!` : la première moitié qui rend
        // la main — même proprement — terminerait alors tout le greffon, et
        // les autres tâches seraient abandonnées sans que leur échec soit
        // jamais observé. C'est exactement ce que l'ancien montage à la main
        // de `generic-input` interdisait, avec un commentaire qui proscrivait
        // déjà `try_join!` en toutes lettres.
        //
        // `FuturesUnordered` donne le meilleur des deux : chaque moitié est
        // journalisée **dès** qu'elle se termine, en étant nommée, sans que
        // cela cesse d'attendre les autres.
        let mut taches = Vec::new();
        for m in self.moities {
            let nom = format!("{:?}", m.kind).to_lowercase();
            taches.push((nom, tokio::spawn(m.servir)));
        }
        if let Some(admin) = self.admin {
            taches.push(("admin".to_string(), tokio::spawn(admin)));
        }

        let mut en_cours: futures::stream::FuturesUnordered<_> = taches
            .into_iter()
            .map(|(nom, tache)| async move { (nom, tache.await) })
            .collect();

        let mut echecs = 0usize;
        while let Some((nom, resultat)) = en_cours.next().await {
            match resultat {
                Ok(Ok(())) => tracing::info!("{nom} half ended"),
                Ok(Err(e)) => {
                    echecs += 1;
                    tracing::error!("{nom} half failed: {e:#}");
                }
                // Une panique est capturée dans le `JoinHandle` au lieu de
                // dérouler la pile de l'autre moitié.
                Err(e) => {
                    echecs += 1;
                    tracing::error!("{nom} half panicked: {e}");
                }
            }
        }
        if echecs > 0 {
            anyhow::bail!("{echecs} plugin half(s) failed");
        }
        Ok(())
    }
}
```

Ajouter `futures = "0.3"` aux dépendances de `crates/ritornello-plugin-sdk/Cargo.toml` si absent (le cœur l'utilise déjà, vérifier la version employée là-bas et reprendre la même).

Déclarer le module dans `crates/ritornello-plugin-sdk/src/lib.rs` :

```rust
pub mod runtime;
pub use runtime::Runtime;
```

- [ ] **Step 4: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-plugin-sdk"`
Attendu : PASS, les trois tests de `runtime` inclus.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-plugin-sdk/src/runtime.rs crates/ritornello-plugin-sdk/src/lib.rs crates/ritornello-plugin-sdk/Cargo.toml
git commit -m "feat(plugin-sdk): constructeur Runtime multi-genres, annonce apres liaison"
```

---

### Task 5 : cœur — répertoire neuf, nouveaux arguments, `kind` retiré du manifeste

**Files:**
- Modify: `crates/ritornello-core/src/plugins.rs`

**Interfaces:**
- Consomme : `ritornello_proto::PluginKind` (Task 1).
- Produit :
  - `PluginConfig { name: String, exec: String }` — plus de `kind`.
  - `pub fn prepare_sockets_dir(runtime_dir: &Path) -> Result<PathBuf>` — supprime puis recrée `{runtime_dir}/sockets`, rend son chemin.
  - `pub fn spawn(exec: &str, register: &Path, name: &str, prefix: &Path, locale: Option<&str>) -> Result<tokio::process::Child>`
  - `attend_liaison` **supprimée**.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans le `mod tests` de `crates/ritornello-core/src/plugins.rs`, ajouter :

```rust
    #[test]
    fn un_manifeste_sans_kind_se_charge() {
        // Le genre est desormais annonce par le binaire : le fichier ne le
        // porte plus.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "radio"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-radio"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 1);
        assert_eq!(m.plugins[0].name, "radio");
    }

    #[test]
    fn un_kind_residuel_est_ignore_sans_erreur() {
        // Une installation en service porte encore `kind = "source"` : elle
        // doit demarrer, comme l'ancien champ `admin` deja ignore.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(
            &path,
            r#"
[[plugin]]
name = "cd"
kind = "source"
admin = true
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-cd"
"#,
        )
        .unwrap();
        let m = PluginManifest::load(&path).unwrap();
        assert_eq!(m.plugins.len(), 1);
        assert_eq!(m.plugins[0].name, "cd");
    }

    #[test]
    fn le_repertoire_de_sockets_est_neuf_a_chaque_demarrage() {
        // Un fichier rance d'une execution precedente est connectable et
        // ferait dialoguer le coeur avec un zombie : le repertoire est donc
        // rase, pas nettoye au cas par cas.
        let dir = tempfile::tempdir().unwrap();
        let sockets = dir.path().join("sockets");
        std::fs::create_dir_all(&sockets).unwrap();
        let rance = sockets.join("radio-source.sock");
        std::fs::write(&rance, "").unwrap();

        let rendu = prepare_sockets_dir(dir.path()).unwrap();
        assert_eq!(rendu, sockets);
        assert!(rendu.is_dir(), "le repertoire doit exister apres l'appel");
        assert!(!rance.exists(), "le fichier rance doit avoir disparu");
    }

    #[test]
    fn une_erreur_de_lancement_nomme_toujours_lexecutable() {
        let dir = tempfile::tempdir().unwrap();
        let e = spawn(
            "/chemin/qui/nexiste/pas/ritornello-plugin-bidon",
            &dir.path().join("register.sock"),
            "bidon",
            &dir.path().join("bidon"),
            None,
        )
        .expect_err("un executable absent doit echouer");
        let message = format!("{e:#}");
        assert!(
            message.contains("/chemin/qui/nexiste/pas/ritornello-plugin-bidon"),
            "l'erreur doit nommer l'executable cherche: {message}"
        );
    }
```

**Supprimer** de ce module de tests : `attend_liaison_voit_une_socket_liee_en_cours_de_fenetre` et `attend_liaison_abandonne_a_lecheance` (la fonction disparaît). **Adapter** `charge_un_manifeste_toml` et `charge_les_plugins_metadata_dans_lordre_de_declaration` : le premier ne doit plus asserter sur `kind`, le second n'a plus de sens ici et se déplace en Task 7 sous une forme qui teste l'ordre depuis les annonces — le supprimer de ce fichier.

- [ ] **Step 2: Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-core plugins"`
Attendu : ÉCHEC — `prepare_sockets_dir` n'existe pas et `spawn` a l'ancienne signature.

- [ ] **Step 3: Implémenter**

Dans `crates/ritornello-core/src/plugins.rs` :

Retirer `kind` de la structure et réécrire son commentaire :

```rust
/// Une entrée de `plugins.toml` : quoi lancer, sous quel nom. Rien d'autre.
///
/// Ni le genre ni la page d'admin n'y sont déclarés : ce sont des propriétés
/// du **binaire**, que celui-ci annonce lui-même sur le socket
/// d'enregistrement du cœur. L'opérateur n'a plus à les connaître, et leur
/// oubli ne peut plus produire de mode dégradé silencieux. Les champs `kind`
/// et `admin` d'un fichier déjà déployé sont simplement ignorés par serde.
///
/// **L'ordre du fichier reste porteur** : c'est lui qui arbitre entre deux
/// greffons annonçant le genre `metadata` (voir `crate::register`).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub exec: String,
}
```

Remplacer `attend_liaison` (supprimée) par :

```rust
/// Rase et recrée `{runtime_dir}/sockets`, et rend son chemin.
///
/// Un répertoire neuf à chaque démarrage rend les fichiers rances
/// **impossibles** au lieu de reposer sur une pré-suppression au cas par cas :
/// un socket laissé par une exécution précédente est connectable, et le cœur
/// dialoguerait avec un zombie ou attendrait un `ECONNREFUSED` retenté. Une
/// seule instance du cœur par `runtime_dir` — garanti par `RuntimeDirectory=`
/// de systemd en service, par une variable distincte en développement.
pub fn prepare_sockets_dir(runtime_dir: &Path) -> Result<PathBuf> {
    let dir = runtime_dir.join("sockets");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("clearing {}", dir.display()));
        }
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}
```

Réécrire `spawn` :

```rust
/// Lance un greffon en lui disant où s'annoncer, sous quel nom, et avec quel
/// préfixe de sockets.
///
/// Aucune pré-suppression de fichier ici : `prepare_sockets_dir` a rasé le
/// répertoire entier avant le premier lancement.
///
/// `locale` transmet la langue courante via `RITORNELLO_LOCALE`, appliquée
/// **au lancement** seulement (inchangé).
pub fn spawn(
    exec: &str,
    register: &Path,
    name: &str,
    prefix: &Path,
    locale: Option<&str>,
) -> Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(exec);
    cmd.arg("--register").arg(register);
    cmd.arg("--name").arg(name);
    cmd.arg("--socket-prefix").arg(prefix);
    if let Some(locale) = locale {
        cmd.env("RITORNELLO_LOCALE", locale);
    }
    // Le chemin est nommé dans l'erreur : « No such file or directory » seul
    // laisse deviner **lequel** des chemins de `plugins.toml` est en cause, et
    // la confusion la plus courante est justement là — un `exec` de déploiement
    // (`/usr/local/lib/...`) recopié dans une configuration de développement,
    // où les binaires sont sous `target/debug/`.
    cmd.kill_on_drop(true).spawn().with_context(|| format!("executable {exec}"))
}
```

Ajouter `use std::path::PathBuf;` si absent.

- [ ] **Step 4: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-core plugins"`
Attendu : PASS. `main.rs` ne compile plus (il appelle l'ancienne `spawn`) — Task 7 s'en charge. Utiliser `--lib` si nécessaire pour isoler.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/plugins.rs
git commit -m "feat(core): repertoire de sockets neuf, arguments d annonce, kind retire du manifeste"
```

---

### Task 6 : cœur — la boucle de rassemblement

Le morceau neuf, donc celui qui porte le risque. Isolé dans son module pour être testable sans lancer de processus.

**Files:**
- Create: `crates/ritornello-core/src/register.rs`
- Modify: `crates/ritornello-core/src/main.rs` (déclaration `mod register;` seulement)

**Interfaces:**
- Consomme : `ritornello_proto::{Announcement, PluginKind}`.
- Produit :
  - `pub struct Gathered { pub announcements: HashMap<String, Announcement>, pub muets: Vec<String> }`
  - `pub async fn gather<S>(listener: &UnixListener, attendus: &[String], morts: S, echeance: Duration) -> Gathered where S: futures::Stream<Item = String> + Unpin`
  - `pub fn metadata_order(manifeste: &[String], g: &Gathered) -> Vec<String>`

`morts` est un flux de noms de greffons dont le processus s'est arrêté. `attendus` est la liste, **dans l'ordre du manifeste**, des greffons effectivement lancés.

- [ ] **Step 1: Écrire les tests qui échouent**

Créer `crates/ritornello-core/src/register.rs` avec le module de tests seul :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::PluginKind;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    /// Écrit une annonce sur le socket d'enregistrement, comme le ferait un
    /// greffon, puis ferme.
    async fn annonce(register: &std::path::Path, ligne: &str) {
        let mut s = UnixStream::connect(register).await.unwrap();
        s.write_all(format!("{ligne}\n").as_bytes()).await.unwrap();
        s.shutdown().await.unwrap();
    }

    fn aucun_mort() -> impl futures::Stream<Item = String> + Unpin {
        futures::stream::pending()
    }

    #[tokio::test]
    async fn rassemble_toutes_les_annonces_et_rend_la_main_aussitot() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"radio","kinds":["source"],"admin":true}"#).await;
            annonce(&r, r#"{"name":"console","kinds":["display"]}"#).await;
        });

        let debut = std::time::Instant::now();
        let g = gather(
            &listener,
            &["radio".to_string(), "console".to_string()],
            aucun_mort(),
            Duration::from_secs(10),
        )
        .await;

        assert_eq!(g.announcements.len(), 2);
        assert!(g.muets.is_empty());
        assert!(g.announcements["radio"].admin);
        assert_eq!(g.announcements["console"].kinds, vec![PluginKind::Display]);
        assert!(
            debut.elapsed() < Duration::from_secs(2),
            "la boucle doit rendre la main des que tout le monde est la, pas a l'echeance"
        );
    }

    #[tokio::test]
    async fn un_greffon_muet_est_nomme_a_lecheance() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let g = gather(
            &listener,
            &["radio".to_string(), "muet".to_string()],
            aucun_mort(),
            Duration::from_millis(300),
        )
        .await;

        assert_eq!(g.announcements.len(), 1);
        assert_eq!(g.muets, vec!["muet".to_string()]);
    }

    #[tokio::test]
    async fn une_mort_precoce_ecourte_lattente() {
        // Aujourd'hui un greffon qui plante fait tourner 10 s de reprises a
        // vide. Ici, `child.wait()` doit trancher tout de suite.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let debut = std::time::Instant::now();
        let g = gather(
            &listener,
            &["radio".to_string(), "plante".to_string()],
            Box::pin(futures::stream::iter(vec!["plante".to_string()])),
            Duration::from_secs(30),
        )
        .await;

        assert_eq!(g.muets, vec!["plante".to_string()]);
        assert!(
            debut.elapsed() < Duration::from_secs(2),
            "la mort du processus doit ecourter l'attente, pas la subir"
        );
    }

    #[tokio::test]
    async fn un_nom_inconnu_est_ignore_sans_bloquer_les_autres() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"intrus","kinds":["source"]}"#).await;
            annonce(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let g = gather(
            &listener,
            &["radio".to_string()],
            aucun_mort(),
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(g.announcements.len(), 1);
        assert!(g.announcements.contains_key("radio"));
        assert!(!g.announcements.contains_key("intrus"));
    }

    #[tokio::test]
    async fn une_annonce_illisible_ne_compte_pas() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, "ceci n'est pas du json").await;
        });

        let g = gather(
            &listener,
            &["radio".to_string()],
            aucun_mort(),
            Duration::from_millis(300),
        )
        .await;

        assert!(g.announcements.is_empty());
        assert_eq!(g.muets, vec!["radio".to_string()]);
    }

    #[tokio::test]
    async fn une_connexion_muette_ne_retarde_pas_les_autres() {
        // Blocage de tete : si la ligne etait lue dans la branche `accept`, un
        // greffon connecte et silencieux gelerait l'annonce de TOUS les autres
        // jusqu'a l'echeance. C'est le defaut que la tache de lecture par
        // connexion existe pour empecher.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();

        let r = register.clone();
        tokio::spawn(async move {
            // Se connecte, se tait, et garde la connexion ouverte.
            let muet = UnixStream::connect(&r).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(muet);
        });
        let r2 = register.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            annonce(&r2, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let debut = std::time::Instant::now();
        let g = gather(
            &listener,
            &["radio".to_string()],
            aucun_mort(),
            Duration::from_secs(30),
        )
        .await;

        assert_eq!(
            g.announcements.len(),
            1,
            "l'annonce doit passer malgre la connexion muette"
        );
        assert!(
            debut.elapsed() < Duration::from_secs(5),
            "une connexion muette ne doit pas retarder le rassemblement"
        );
    }

    #[tokio::test]
    async fn lordre_des_metadata_suit_le_manifeste_pas_les_arrivees() {
        // La garantie etait acquise par construction (liste batie avant tout
        // lancement) ; elle est desormais maintenue par le code, donc testee.
        let mut announcements = HashMap::new();
        for nom in ["musicbrainz", "ouifm-metas", "radiofrance-metas"] {
            announcements.insert(
                nom.to_string(),
                Announcement {
                    name: nom.to_string(),
                    kinds: vec![PluginKind::Metadata],
                    admin: false,
                },
            );
        }
        announcements.insert(
            "radio".to_string(),
            Announcement { name: "radio".into(), kinds: vec![PluginKind::Source], admin: true },
        );
        let g = Gathered { announcements, muets: Vec::new() };

        // Ordre du manifeste, deliberement different de l'ordre alphabetique
        // et de tout ordre d'arrivee plausible.
        let manifeste = vec![
            "radio".to_string(),
            "ouifm-metas".to_string(),
            "radiofrance-metas".to_string(),
            "musicbrainz".to_string(),
        ];
        assert_eq!(
            metadata_order(&manifeste, &g),
            vec![
                "ouifm-metas".to_string(),
                "radiofrance-metas".to_string(),
                "musicbrainz".to_string()
            ]
        );
    }
}
```

- [ ] **Step 2: Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-core register"`
Attendu : ÉCHEC de compilation — `gather`, `Gathered`, `metadata_order` n'existent pas.

- [ ] **Step 3: Implémenter**

En tête de `crates/ritornello-core/src/register.rs` :

```rust
//! Rassemblement des annonces des greffons.
//!
//! Le cœur lie un socket avant tout lancement, puis attend une annonce par
//! greffon lancé. Comme le greffon lie ses propres sockets **avant** de
//! s'annoncer, la ligne reçue est une barrière de disponibilité : le cœur
//! peut se connecter derrière sans retenter. C'est ce qui remplace les deux
//! attentes devinées d'avant — la fenêtre de 2 s de la page d'admin et les
//! 10 s de reprises de connexion.

use futures::{Stream, StreamExt};
use ritornello_proto::{Announcement, PluginKind};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

/// Ce que le rassemblement a appris.
#[derive(Debug, Default)]
pub struct Gathered {
    /// Annoncés, par nom.
    pub announcements: HashMap<String, Announcement>,
    /// Lancés mais jamais annoncés : morts en route, ou muets à l'échéance.
    /// Nommés, pour que le journal désigne un coupable au lieu de laisser
    /// déduire.
    pub muets: Vec<String>,
}

/// Attend une annonce par greffon lancé.
///
/// Rend la main dès que chaque attendu est soit annoncé, soit mort — donc en
/// pratique bien avant `echeance`. Un délai ne se paie plus qu'à l'échec.
pub async fn gather<S>(
    listener: &UnixListener,
    attendus: &[String],
    morts: S,
    echeance: Duration,
) -> Gathered
where
    S: Stream<Item = String> + Unpin,
{
    let mut restants: Vec<String> = attendus.to_vec();
    let mut announcements = HashMap::new();
    let mut morts = morts.fuse();
    let fin = tokio::time::sleep(echeance);
    tokio::pin!(fin);

    // **Une tâche de lecture par connexion**, et non une lecture en ligne dans
    // la branche `accept` : un greffon qui se connecte puis n'écrit rien ne
    // doit pas retarder l'annonce des autres. Un blocage de tête sur le
    // rendez-vous serait le défaut même que le protocole refuse ailleurs.
    //
    // L'émetteur d'origine reste vivant dans cette portée : `recv()` ne rend
    // donc jamais `None`, et sa branche du `select!` ne se désarme pas.
    let (lignes_tx, mut lignes_rx) = tokio::sync::mpsc::channel::<String>(16);

    while !restants.is_empty() {
        tokio::select! {
            accepte = listener.accept() => {
                match accepte {
                    Ok((stream, _)) => {
                        let tx = lignes_tx.clone();
                        tokio::spawn(async move {
                            let mut lignes = BufReader::new(stream).lines();
                            match lignes.next_line().await {
                                Ok(Some(l)) => {
                                    let _ = tx.send(l).await;
                                }
                                Ok(None) => tracing::warn!(
                                    "a plugin connected to the register socket and said nothing"
                                ),
                                Err(e) => tracing::warn!("reading an announcement failed: {e}"),
                            }
                        });
                    }
                    Err(e) => tracing::warn!("register socket accept failed: {e}"),
                }
            }
            Some(ligne) = lignes_rx.recv() => {
                let annonce: Announcement = match serde_json::from_str(&ligne) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!("unreadable announcement ignored ({e}): {ligne}");
                        continue;
                    }
                };
                // Le nom fait autorité côté manifeste : une annonce qui en
                // porte un autre vient d'un binaire mal lancé, ou d'un
                // greffon qui invente son identité. Elle est nommée puis
                // écartée, jamais câblée.
                if !restants.contains(&annonce.name) {
                    if announcements.contains_key(&annonce.name) {
                        tracing::warn!("duplicate announcement for {}, ignored", annonce.name);
                    } else {
                        tracing::warn!("announcement from unknown plugin {}, ignored", annonce.name);
                    }
                    continue;
                }
                restants.retain(|n| n != &annonce.name);
                tracing::info!("{} announced {:?} (admin: {})", annonce.name, annonce.kinds, annonce.admin);
                announcements.insert(annonce.name.clone(), annonce);
            }
            Some(mort) = morts.next() => {
                // Le processus est parti avant de s'annoncer : cesser de
                // l'attendre. C'est ce qui rend un plantage au démarrage plus
                // rapide à diagnostiquer qu'avant, où il consommait les 10 s
                // de reprises à vide.
                if restants.contains(&mort) {
                    tracing::warn!("plugin {mort} exited before announcing");
                    restants.retain(|n| n != &mort);
                }
            }
            () = &mut fin => {
                tracing::warn!("register deadline reached, still waiting for: {}", restants.join(", "));
                break;
            }
        }
    }

    Gathered { announcements, muets: restants }
}

/// Les greffons `metadata`, **dans l'ordre du manifeste**.
///
/// L'ordre du fichier est la priorité d'arbitrage : entre deux greffons qui
/// répondent pour le même morceau, le premier déclaré gagne. Avant, la liste
/// était bâtie depuis le manifeste avant tout lancement, donc l'ordre était
/// acquis par construction ; il est maintenant reconstruit ici, et un tri par
/// ordre d'arrivée des annonces rendrait l'affichage non reproductible d'un
/// démarrage à l'autre. Ne jamais trier cette liste autrement.
pub fn metadata_order(manifeste: &[String], g: &Gathered) -> Vec<String> {
    manifeste
        .iter()
        .filter(|nom| {
            g.announcements
                .get(*nom)
                .is_some_and(|a| a.kinds.contains(&PluginKind::Metadata))
        })
        .cloned()
        .collect()
}
```

Déclarer le module dans `crates/ritornello-core/src/main.rs`, à côté des autres `mod` :

```rust
mod register;
```

- [ ] **Step 4: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-core register"`
Attendu : PASS, les 7 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/register.rs crates/ritornello-core/src/main.rs
git commit -m "feat(core): boucle de rassemblement des annonces, ordre des metadata depuis le manifeste"
```

---

### Task 7 : cœur — câblage multi-genres depuis les annonces

**Files:**
- Modify: `crates/ritornello-core/src/main.rs:125-265` (bloc de lancement et de connexion)

**Interfaces:**
- Consomme : `plugins::{prepare_sockets_dir, spawn}` (Task 5), `register::{gather, metadata_order, Gathered}` (Task 6), `ritornello_plugin_sdk::{genre_socket, admin_socket}` (Task 2).
- Produit : le câblage complet. `sources: HashMap<String, Arc<dyn core::Source>>`, `display_clients: Vec<Arc<DisplayClient>>` (consommé par Task 8), `admin_backends: HashMap<String, Arc<dyn admin::AdminBackend>>`, `plugin_statuses: Vec<PluginStatus>`.

- [ ] **Step 1: Remplacer la préparation et le lancement**

Avant la boucle de lancement, remplacer le calcul de `metadata_plugins` (actuellement `main.rs:126-132`, bâti depuis `p.kind`) et ajouter la préparation :

```rust
    // Répertoire neuf, puis le socket d'enregistrement lié AVANT tout
    // lancement : un greffon qui démarre vite trouve toujours quelqu'un.
    // `runtime_dir` est une `String` issue de `env_or` : ajouter
    // `use std::path::Path;` en tête de `main.rs` s'il n'y est pas — seul
    // `PathBuf` y est importé aujourd'hui.
    let sockets_dir = plugins::prepare_sockets_dir(Path::new(&runtime_dir))?;
    let register_path = sockets_dir.join("register.sock");
    let register_listener = tokio::net::UnixListener::bind(&register_path)
        .with_context(|| format!("binding {}", register_path.display()))?;
```

Remplacer la boucle de lancement par :

```rust
    let mut plugin_waits = FuturesUnordered::new();
    let mut lances: Vec<String> = Vec::new();
    let mut plugin_statuses = Vec::new();

    for p in &manifest.plugins {
        let prefix = sockets_dir.join(&p.name);
        match plugins::spawn(
            &p.exec,
            &register_path,
            &p.name,
            &prefix,
            persisted.locale.as_deref(),
        ) {
            Ok(child) => {
                let wname = p.name.clone();
                plugin_waits.push(async move {
                    let mut child = child;
                    let status = child.wait().await;
                    (wname, status)
                });
                lances.push(p.name.clone());
            }
            Err(e) => {
                // `{e:#}` et non `{e}` : la chaîne de contexte porte le chemin
                // cherché, que le seul message d'erreur système n'indique pas.
                tracing::warn!("failed to launch plugin {}: {e:#}", p.name);
                plugin_statuses.push(PluginStatus {
                    name: p.name.clone(),
                    kind: "unknown".into(),
                    connected: false,
                    admin: false,
                });
            }
        }
    }
```

Note sur `kind: "unknown"` : un greffon qui n'a pas démarré n'a jamais annoncé de genre, et le manifeste ne le porte plus. La page de statut affiche donc un genre inconnu plutôt que d'en inventer un.

- [ ] **Step 2: Rassembler les annonces**

Juste après la boucle :

```rust
    // Une annonce par greffon lancé. Les morts précoces écourtent l'attente ;
    // `plugin_waits` reste utilisable ensuite, seules les entrées consommées
    // ici en sortent — et ce sont précisément celles dont on a déjà appris la
    // mort.
    let rassemble = register::gather(
        &register_listener,
        &lances,
        (&mut plugin_waits).map(|(nom, _statut)| nom),
        std::time::Duration::from_secs(10),
    )
    .await;

    for muet in &rassemble.muets {
        plugin_statuses.push(PluginStatus {
            name: muet.clone(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
        });
    }

    let ordre_manifeste: Vec<String> = manifest.plugins.iter().map(|p| p.name.clone()).collect();
    let metadata_plugins = register::metadata_order(&ordre_manifeste, &rassemble);
```

Ajouter `use futures::StreamExt;` en tête de `main.rs` s'il n'y est pas déjà (`FuturesUnordered` y est, mais `.map` sur un flux demande `StreamExt`).

- [ ] **Step 3: Câbler chaque genre annoncé**

Remplacer le `match p.kind { ... }` par une boucle sur les annonces, parcourue **dans l'ordre du manifeste** :

```rust
    let mut sources: HashMap<String, Arc<dyn core::Source>> = HashMap::new();
    let mut display_clients: Vec<Arc<DisplayClient>> = Vec::new();
    let mut admin_backends: HashMap<String, Arc<dyn admin::AdminBackend>> = HashMap::new();

    for nom in &ordre_manifeste {
        let Some(annonce) = rassemble.announcements.get(nom) else {
            continue;
        };
        let prefix = sockets_dir.join(nom);

        for kind in &annonce.kinds {
            let socket = ritornello_plugin_sdk::genre_socket(&prefix, *kind);
            // L'annonce prouve que le socket est lié : un `connect` nu suffit,
            // plus de boucle de reprise. Un échec ici est une vraie anomalie,
            // pas une course au démarrage.
            match kind {
                PluginKind::Source => {
                    match SourceClient::connect(&socket, nom.clone(), source_update_tx.clone()).await
                    {
                        Ok(client) => {
                            sources.insert(nom.clone(), client);
                            plugin_statuses.push(PluginStatus {
                                name: nom.clone(),
                                kind: "source".into(),
                                connected: true,
                                admin: annonce.admin,
                            });
                        }
                        Err(e) => {
                            tracing::warn!("plugin {nom} source unavailable: {e}");
                            plugin_statuses.push(PluginStatus {
                                name: nom.clone(),
                                kind: "source".into(),
                                connected: false,
                                admin: annonce.admin,
                            });
                        }
                    }
                }
                PluginKind::Display => match DisplayClient::connect(&socket).await {
                    Ok(client) => {
                        display_clients.push(client);
                        plugin_statuses.push(PluginStatus {
                            name: nom.clone(),
                            kind: "display".into(),
                            connected: true,
                            admin: annonce.admin,
                        });
                    }
                    Err(e) => {
                        tracing::warn!("display plugin {nom} unavailable: {e}");
                        plugin_statuses.push(PluginStatus {
                            name: nom.clone(),
                            kind: "display".into(),
                            connected: false,
                            admin: annonce.admin,
                        });
                    }
                },
                PluginKind::Input => {
                    let tx = cmd_tx.clone();
                    let socket_for_task = socket.clone();
                    let name = nom.clone();
                    tokio::spawn(async move {
                        if let Err(e) = run_input_client(&socket_for_task, tx).await {
                            tracing::warn!("input plugin {name} disconnected: {e}");
                        }
                    });
                    plugin_statuses.push(PluginStatus {
                        name: nom.clone(),
                        kind: "input".into(),
                        connected: true,
                        admin: annonce.admin,
                    });
                }
                PluginKind::Metadata => {
                    // Relais dans les deux sens, dans sa propre tâche : sa
                    // panne ne concerne que les métadonnées. **La lecture
                    // n'est jamais affectée** par un plugin `metadata`.
                    let tx = enrich_tx.clone();
                    let np_rx = now_playing_rx.clone();
                    let socket_for_task = socket.clone();
                    let name = nom.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            run_metadata_client(&socket_for_task, name.clone(), tx, np_rx).await
                        {
                            tracing::warn!("metadata plugin {name} disconnected: {e}");
                        }
                    });
                    plugin_statuses.push(PluginStatus {
                        name: nom.clone(),
                        kind: "metadata".into(),
                        connected: true,
                        admin: annonce.admin,
                    });
                }
            }
        }

        if annonce.admin {
            let chemin = ritornello_plugin_sdk::admin_socket(&prefix);
            match ritornello_plugin_sdk::AdminClient::connect(&chemin).await {
                Ok(client) => {
                    admin_backends.insert(nom.clone(), client);
                }
                Err(e) => tracing::warn!("admin plugin {nom} unreachable: {e}"),
            }
        }
    }
```

**Supprimer** les trois barrières devenues inutiles : la boucle `for handle in source_connects`, le bloc `if let Some(handle) = display_connect`, et la boucle `for handle in admin_connects`. Les connexions sont désormais faites en ligne, l'annonce garantissant qu'elles aboutissent.

**Supprimer** le commentaire « La page d'admin est une capacité **observée** » : elle est désormais annoncée. Le remplacer par :

```rust
    // La page d'admin est **annoncée** par le binaire, plus observée par une
    // fenêtre d'attente : le drapeau des statuts vient de la ligne
    // d'enregistrement.
```

Ajouter les imports nécessaires : `use ritornello_proto::PluginKind;` et `use anyhow::Context;` s'ils manquent.

- [ ] **Step 4: Compiler le cœur**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo build -p ritornello-core"`
Attendu : compile. Si un `admin: false` résiduel subsiste dans un `PluginStatus`, le corriger : le drapeau vient maintenant de `annonce.admin`.

- [ ] **Step 5: Lancer les tests du cœur**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-core"`
Attendu : PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-core/src/main.rs
git commit -m "feat(core): cablage multi-genres depuis les annonces, barrieres de connexion supprimees"
```

---

### Task 8 : cœur — afficheurs multiples

Correction du bug : `display_connect = Some(...)` écrasait silencieusement le premier afficheur déclaré.

**Files:**
- Modify: `crates/ritornello-core/src/main.rs:278-297` (relais d'état vers l'afficheur)

**Interfaces:**
- Consomme : `display_clients: Vec<Arc<DisplayClient>>` (Task 7).
- Produit : une tâche de relais par afficheur.

- [ ] **Step 1: Écrire le test qui échoue**

Ce relais est du câblage dans `main()`, non testable directement. Le comportement testable est côté SDK : deux `DisplayClient` distincts, alimentés depuis un même canal `watch`, reçoivent tous deux l'état, et un afficheur qui ne lit pas n'empêche pas l'autre de recevoir.

Ajouter dans le `mod tests` de `crates/ritornello-plugin-sdk/src/client.rs` :

```rust
    #[tokio::test]
    async fn deux_afficheurs_recoivent_le_meme_etat_et_un_lent_ne_bloque_pas_lautre() {
        // Le singleton d'avant (`display_connect = Some(...)`) faisait
        // disparaitre le premier afficheur declare, sans erreur. Deux clients
        // doivent vivre en parallele, et la contre-pression rester cloisonnee
        // par socket : c'est l'argument meme qui a fait garder des sockets
        // separes plutot que de tout fusionner.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.sock");
        let b = dir.path().join("b.sock");
        let la = UnixListener::bind(&a).unwrap();
        let lb = UnixListener::bind(&b).unwrap();

        let client_a = DisplayClient::connect(&a).await.unwrap();
        let client_b = DisplayClient::connect(&b).await.unwrap();

        // `a` est accepte puis LU ; `b` est accepte et jamais lu.
        let (sa, _) = la.accept().await.unwrap();
        let (_sb, _) = lb.accept().await.unwrap();

        let etat = PlayerState::default();
        client_a.send(&etat).await.unwrap();
        client_b.send(&etat).await.unwrap();
        // Le second envoi vers l'afficheur muet ne doit pas empecher le
        // premier d'aboutir.
        client_a.send(&etat).await.unwrap();

        let mut lignes = BufReader::new(sa).lines();
        assert!(lignes.next_line().await.unwrap().is_some());
        assert!(lignes.next_line().await.unwrap().is_some());
    }
```

- [ ] **Step 2: Lancer le test**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-plugin-sdk deux_afficheurs"`
Attendu : PASS immédiatement — `DisplayClient` supporte déjà plusieurs instances. Ce test verrouille la propriété que Task 8 va exploiter ; s'il échoue, s'arrêter et signaler, l'hypothèse du plan serait fausse.

- [ ] **Step 3: Une tâche de relais par afficheur**

Remplacer le bloc `match display_client { ... }` par :

```rust
    // Relais de l'état vers chaque afficheur connecté : le même canal qui
    // alimente la route SSE de la SPA, chaque plugin composant lui-même sa
    // mise en page depuis la trame reçue.
    //
    // **Une tâche par afficheur**, et non une tâche qui boucle sur N clients :
    // c'est ce qui empêche un afficheur lent — console occupée, écran bloqué
    // en I/O — de retarder les autres. La contre-pression reste cloisonnée par
    // socket, ce qui était l'argument retenu pour ne pas fusionner les sockets
    // des genres.
    //
    // Avant, cette variable était un `Option` : déclarer deux afficheurs ne
    // produisait aucune erreur, mais le cœur ne gardait que le client du
    // dernier déclaré et le premier attendait des lignes qui n'arrivaient
    // jamais.
    if display_clients.is_empty() {
        tracing::warn!("no display plugin connected, continuing without display");
    }
    for display_client in display_clients {
        let mut display_rx = etat_rx.clone();
        tokio::spawn(async move {
            loop {
                if display_rx.changed().await.is_err() {
                    break;
                }
                let etat = display_rx.borrow_and_update().clone();
                if let Err(e) = display_client.send(&etat).await {
                    tracing::warn!("display: {e}");
                }
            }
        });
    }
```

- [ ] **Step 4: Compiler et tester**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test -p ritornello-core -p ritornello-plugin-sdk"`
Attendu : PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/main.rs crates/ritornello-plugin-sdk/src/client.rs
git commit -m "fix(core): plusieurs afficheurs, une tache de relais chacun"
```

---

### Task 9 : migrer les huit binaires de greffons

Bascule sèche. Chaque `main()` passe au `Runtime`. Les branches « `--admin-socket` absent → mode dégradé » **disparaissent** : le greffon décide lui-même s'il sert une page, l'absence n'existe plus.

**Files:**
- Modify: `crates/ritornello-plugin-radio/src/main.rs:224-…`
- Modify: `crates/ritornello-plugin-cd/src/main.rs:334-…`
- Modify: `crates/ritornello-plugin-files/src/main.rs:356-…`
- Modify: `crates/ritornello-plugin-console/src/main.rs` (`main()` en fin de fichier)
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs:48-…`
- Modify: `crates/ritornello-plugin-musicbrainz/src/main.rs:202-206`
- Modify: `crates/ritornello-plugin-ouifm-metas/src/main.rs:152-…`
- Modify: `crates/ritornello-plugin-radiofrance-metas/src/main.rs:166-…`

**Interfaces:**
- Consomme : `ritornello_plugin_sdk::Runtime` (Task 4).
- Produit : huit binaires qui s'annoncent.

- [ ] **Step 1: Les quatre greffons mono-genre sans page**

`console` — remplacer la fin de `main()` :

```rust
    let tty = PathBuf::from(env_or("RITORNELLO_CONSOLE_TTY", "/dev/tty1"));
    let display = ConsoleDisplay::open(&tty)?;
    Runtime::from_args()?.display(ConsolePlugin { display })?.run().await
```

`musicbrainz` :

```rust
    Runtime::from_args()?.metadata(MusicBrainzPlugin::new())?.run().await
```

`ouifm-metas` et `radiofrance-metas` : même forme, en conservant la construction du greffon telle qu'elle est aujourd'hui, et en remplaçant seulement `let socket_path = …; run_metadata_plugin(X, &socket_path).await` par `Runtime::from_args()?.metadata(X)?.run().await`.

`cd` : remplacer `run_source_plugin(source, &socket_path).await` par `Runtime::from_args()?.source(source)?.run().await`.

Dans chaque fichier, ajuster les `use` : retirer `run_display_plugin` / `run_metadata_plugin` / `run_source_plugin` devenus inutilisés (`-D warnings` refuse un import mort), ajouter `ritornello_plugin_sdk::Runtime`.

- [ ] **Step 2: Compiler ces quatre greffons**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo build -p ritornello-plugin-console -p ritornello-plugin-musicbrainz -p ritornello-plugin-ouifm-metas -p ritornello-plugin-radiofrance-metas -p ritornello-plugin-cd"`
Attendu : compile sans avertissement.

- [ ] **Step 3: Commit intermédiaire**

```bash
git add crates/ritornello-plugin-console crates/ritornello-plugin-musicbrainz crates/ritornello-plugin-ouifm-metas crates/ritornello-plugin-radiofrance-metas crates/ritornello-plugin-cd
git commit -m "feat(plugins): console, cd et les trois metadata s annoncent au coeur"
```

- [ ] **Step 4: `generic-input` — input + admin**

Remplacer, dans `main()`, tout le bloc allant de `let socket_path = …` jusqu'à la fin des `tokio::join!` par :

```rust
    let bindings_path =
        PathBuf::from(env_or("RITORNELLO_INPUT_BINDINGS", "/etc/ritornello/input-bindings.toml"));
    let presets_root =
        PathBuf::from(env_or("RITORNELLO_INPUT_PRESETS", "/etc/ritornello/input-presets"));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));
    // Un plugin Input ne reçoit pas de `SetLocale` (le protocole ne le prévoit
    // que pour les sources) : la langue de la page vient de l'environnement.
    let locale = env_or("RITORNELLO_LOCALE", "en");
    let catalog = Arc::new(RwLock::new(Catalog::load(
        "generic-input",
        &locale,
        &locales_root,
        GENERIC_INPUT_EN,
    )));

    let (tx, rx) = mpsc::channel(32);
    let hub = Hub::new(Bindings::load(&bindings_path), tx);
    let input_root = PathBuf::from(devices::INPUT_DIR);
    let ouverts = hub.open_new_devices(&input_root);
    tracing::info!("{ouverts} input device(s) opened");

    // Les deux moitiés restent indépendantes : une panne de la page ne doit
    // pas couper la télécommande. C'est `Runtime::run` qui les tient
    // désormais, chacune dans sa tâche — la page n'est plus conditionnelle,
    // puisque le greffon annonce lui-même qu'il en a une.
    let admin = GenericInputAdmin { bindings_path, presets_root, input_root, hub, catalog };
    Runtime::from_args()?.input(EvdevInput { rx })?.admin(admin)?.run().await
```

Retirer les `use` de `run_admin_plugin` et `run_input_plugin`, ajouter `Runtime`.

- [ ] **Step 5: `radio` — source + admin**

Dans `main()` :
- supprimer `let socket_path = …`, `let admin_socket = …` et l'`if admin_socket.is_none() { tracing::warn!(…) }` avec son commentaire ;
- `preset_count_rx: admin_socket.is_some().then_some(preset_count_rx)` devient `preset_count_rx: Some(preset_count_rx)` — la page existe toujours, le mode dégradé n'a plus de cas ;
- remplacer le montage final des deux moitiés par :

```rust
    let admin = RadioAdmin { /* champs inchangés, tels qu'ils sont construits aujourd'hui */ };
    Runtime::from_args()?.source(source)?.admin(admin)?.run().await
```

Conserver à l'identique la construction de `RadioAdmin` (stations partagées, catalogue, `preset_count_tx`, chemins) : seule la façon de la servir change.

- [ ] **Step 6: `files` — source + admin**

Même traitement que `radio` : retirer la lecture de `--admin-socket` et sa branche dégradée, puis :

```rust
    Runtime::from_args()?.source(source)?.admin(admin)?.run().await
```

en conservant la construction de la source et de la page telles qu'elles sont.

- [ ] **Step 7: Compiler et tester l'espace de travail complet**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test"`
Attendu : PASS partout. C'est le premier point du plan où l'espace de travail est de nouveau cohérent.

- [ ] **Step 8: Commit**

```bash
git add crates/ritornello-plugin-radio crates/ritornello-plugin-files crates/ritornello-plugin-generic-input
git commit -m "feat(plugins): radio, files et generic-input s annoncent, mode degrade admin supprime"
```

---

### Task 10 : page de statut — une ligne par (nom, genre)

Un greffon multi-genres produit plusieurs lignes de même nom. La clé de rendu doit cesser d'être le seul nom.

**Files:**
- Modify: `web/app/src/views/ConfigView.vue:171`
- Modify: `web/app/src/views/ConfigView.test.ts`

**Interfaces:**
- Consomme : `PluginStatus { name, kind, connected, admin }` de `/api/status`, inchangé en forme.
- Produit : un rendu correct pour deux lignes de même `name`.

Le rendu a été localisé lors du contrôle préalable du plan, il n'y a rien à
chercher. `ConfigView.vue:171` porte exactement :

```html
<tr v-for="p in status.plugins" :key="p.name" data-plugin-row class="border-t border-border">
```

L'attribut `data-plugin-row` existe déjà et sert de sélecteur aux tests
voisins : ne pas en introduire un autre.

- [ ] **Step 1: Écrire le test qui échoue**

Dans `web/app/src/views/ConfigView.test.ts`, ajouter ce cas. Il reprend le
helper `monter({...})` du fichier, employé tel quel par les tests voisins
(voir la ligne 172 pour la forme exacte de l'appel) :

```ts
it('rend une ligne par genre pour un greffon multi-genres', async () => {
  // Un greffon peut annoncer plusieurs genres : la cle de rendu ne peut plus
  // etre le seul nom, sinon Vue confond les deux lignes et n'en rend qu'une.
  const { w } = await monter({
    '/api/status': {
      plugins: [
        { name: 'mpd', kind: 'input', connected: true, admin: true },
        { name: 'mpd', kind: 'display', connected: true, admin: true },
      ],
      active_source: '',
    },
  })
  expect(w.findAll('[data-plugin-row]')).toHaveLength(2)
  expect(w.text()).toContain('input')
  expect(w.text()).toContain('display')
})
```

- [ ] **Step 2: Lancer le test**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons/web/app && npx vitest run"`

Si `node_modules` manque dans le worktree, créer les deux jonctions nécessaires (`vue-router` et `@ritornello/ui`) — **jamais** une pour `vite` — puis relancer depuis `web/app`.

Attendu : ÉCHEC, ou avertissement Vue de clé dupliquée.

- [ ] **Step 3: Corriger la clé**

Dans `web/app/src/views/ConfigView.vue:171`, remplacer `:key="p.name"` par :

```html
:key="`${p.name}-${p.kind}`"
```

- [ ] **Step 4: Relancer le test**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons/web/app && npx vitest run"`
Attendu : PASS, sans avertissement de clé dupliquée.

- [ ] **Step 5: Commit**

```bash
git add web/app/src
git commit -m "fix(web): une ligne de statut par nom et genre de greffon"
```

---

### Task 11 : documentation et déploiement

**Files:**
- Modify: `docs/plugins.md` (902 lignes ; sections « Declaring the plugins », « Writing a `metadata` plugin », « A plugin's UI »)
- Modify: `deploy/plugins.example.toml`
- Modify: `docs/development.md` si le protocole de greffons y est décrit (vérifier)

- [ ] **Step 1: `deploy/plugins.example.toml`**

Retirer toutes les lignes `kind = "…"`. Remplacer le paragraphe sur l'ordre des métadonnées par :

```toml
# DECLARATION ORDER STILL MATTERS, for the plugins that announce the
# `metadata` kind: if two of them answer for the same track, the one declared
# first in this file wins, and a plugin declared lower down never overwrites
# it. This is what makes the display reproducible from one boot to the next,
# regardless of network latency.
#
# The kind itself is no longer declared here: each binary announces its own
# kinds — and whether it serves an admin page — on the core's register socket
# when it starts. This file says what to launch and under which name; the
# binary says what it can do.
```

Conserver tous les autres commentaires (préréquis de paquets, frontière de privilèges, tables embarquées) : ils documentent des greffons, pas le protocole.

- [ ] **Step 2: `docs/plugins.md`, section « Declaring the plugins »**

Réécrire la section pour décrire : `plugins.toml` réduit à `name` + `exec` ; le socket d'enregistrement du cœur ; l'ordre « lier ses sockets, puis s'annoncer » ; le fait que l'ordre du fichier arbitre toujours les `metadata`. Documenter la ligne d'annonce avec un exemple réel :

```json
{"name":"mpd","kinds":["input","display"],"admin":true}
```

Documenter les trois arguments reçus par un greffon (`--register`, `--name`, `--socket-prefix`) et les chemins qu'il doit lier (`{prefix}-{genre}.sock`, `{prefix}-admin.sock`).

- [ ] **Step 3: `docs/plugins.md`, sections d'écriture d'un greffon**

Dans « Writing a `metadata` plugin » et « A plugin's UI », remplacer les exemples appelant `socket_path()`, `admin_socket_path()` et `run_*_plugin` par la forme `Runtime` :

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    ritornello_plugin_sdk::Runtime::from_args()?
        .metadata(MonGreffon::new())?
        .run()
        .await
}
```

Ajouter une phrase sur le multi-genres : un même binaire peut chaîner plusieurs méthodes, et l'annonce décrit exactement ce qui a été enregistré.

- [ ] **Step 4: Vérifier `deploy/missing-plugins.awk`**

Aucun changement attendu — il apparie les blocs par `name` et recopie les blocs tels quels. Le confirmer en le relisant, et ne pas le modifier.

- [ ] **Step 5: Commit**

```bash
git add docs/plugins.md deploy/plugins.example.toml
git commit -m "docs(plugins): protocole d annonce, multi-genres, kind retire du manifeste"
```

---

### Task 12 : vérification finale

- [ ] **Step 1: Espace de travail complet**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test"`
Attendu : PASS, zéro échec.

- [ ] **Step 2: Clippy**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo clippy --all-targets -- -D warnings"`
Attendu : aucun avertissement.

- [ ] **Step 3: Tests de l'IHM**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons/web/app && npx vitest run"`
Attendu : PASS.

- [ ] **Step 4: Vérifier qu'aucun test de protocole n'a bougé**

Run :
```bash
git diff main --stat -- crates/ritornello-plugin-sdk/src/server.rs crates/ritornello-plugin-sdk/src/client.rs
```
Contrôler que les seules additions dans les `mod tests` sont les tests **ajoutés** par ce plan (`bind_puis_serve_equivaut_a_run`, `deux_afficheurs_recoivent_le_meme_etat_et_un_lent_ne_bloque_pas_lautre`) et qu'aucun test préexistant n'a été modifié ni supprimé.

- [ ] **Step 5: Vérifier que les attentes devinées ont disparu**

Run :
```bash
grep -rn "attend_liaison\|connect_with_retry\|admin_socket_path\|socket_path()" crates/ --include=*.rs
```
Attendu : aucun résultat. Si `connect_with_retry` subsiste, le supprimer de `client.rs` et remplacer ses cinq appels par un `UnixStream::connect` avec `with_context` nommant le chemin.

- [ ] **Step 6: Commit final**

```bash
git add -A
git commit -m "chore: verification du chantier rendez-vous des greffons"
```

---

## Auto-revue

**Couverture de la spec.** Répertoire par exécution → Task 5. Protocole d'annonce → Tasks 1, 4, 6. Ligne de commande → Task 2. Rassemblement (échéance, mort précoce, nom inconnu, ligne illisible) → Task 6. Multi-genres → Tasks 4, 7. Afficheurs multiples → Task 8. Ordre des métadonnées → Task 6 (`metadata_order` + test), câblé en Task 7. Statuts une ligne par (nom, genre) → Tasks 7, 10. Suppressions (`attend_liaison`, `connect_with_retry`, `--socket`/`--admin-socket`, `PluginConfig.kind`) → Tasks 2, 5, 12. Traits et protocoles inchangés → Task 3 + garde-fou Task 12 Step 4. Documentation → Task 11.

**Point non couvert par la spec, ajouté ici :** la suppression des branches « mode dégradé sans page d'admin » de `radio`, `files` et `generic-input` (Task 9), conséquence directe de l'auto-déclaration — l'absence de `--admin-socket` n'existe plus.

**Cohérence des noms.** `PluginKind` et `Announcement` (Task 1) sont employés tels quels partout. `genre_socket`/`admin_socket` (Task 2) sont les seuls constructeurs de chemins, utilisés par Task 4 et Task 7. `bind_*`/`serve_*` (Task 3) ne sont appelés que par Task 4 et les enveloppes `run_*_plugin`. `gather`/`metadata_order`/`Gathered` (Task 6) sont consommés par Task 7. `display_clients` (Task 7) est consommé par Task 8.

**Ordre des tâches.** L'espace de travail ne compile pas entre Task 2 et Task 9 : les greffons appellent encore `socket_path()`. C'est signalé dans les tâches concernées, avec des commandes de test ciblées par paquet. La cohérence revient au Step 7 de Task 9.
